use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, RwLock as StdRwLock};

use anyhow::Result;
use tokio::time::{sleep, Duration};
use tower_lsp::lsp_types::notification::Notification;
use tower_lsp::lsp_types::{FileChangeType, FileEvent, MessageType};
use tower_lsp::Client;

use super::{
    emit_perf_log, state, uri_to_path, Backend, IncludeCompletionTable, IncludeTables,
    IndexGenerations, IndexSchedule, IndexedFileLists, NameTables, ReachGraphs,
};
use crate::indexer::{self, IndexOptions};
use crate::pathing;
use crate::progress::{DegradedCapabilities, IndexState, IndexStatus};
use crate::query::NameTable;
use crate::reachability::ReachGraph;
use crate::store::IndexStore;

mod cache;
mod watch;

pub(super) use cache::{ready_cache_message, rebuild_include_table, rebuild_indexed_file_list};
use cache::{
    rebuild_name_table, rebuild_reach_graph, refresh_reach_graph_incremental,
    refresh_workspace_generation, update_name_table_paths,
};
pub(super) use watch::watched_change_in_scope;

const INDEX_DEBOUNCE: Duration = Duration::from_millis(350);

#[derive(Debug, Default)]
pub(super) struct IndexScheduleState {
    pub(super) running: bool,
    pub(super) scheduled: bool,
    pub(super) pending_requested: bool,
    pub(super) pending_full: bool,
    pub(super) pending_force: bool,
    pub(super) pending_changes: Vec<RootDirtyChange>,
}

#[derive(Debug, Clone)]
pub(super) struct RootDirtyChange {
    pub(super) root: PathBuf,
    pub(super) rel_path: String,
    pub(super) change: indexer::DirtyFileChange,
}

pub(super) enum WatchDecision {
    Full,
    Dirty(RootDirtyChange),
}

enum ScheduledIndex {
    Full { force: bool },
    Dirty(Vec<RootDirtyChange>),
}

enum IndexStatusNotification {}

impl Notification for IndexStatusNotification {
    type Params = IndexStatus;
    const METHOD: &'static str = "fossilsense/indexStatus";
}

impl Backend {
    pub(super) async fn spawn_dirty_files(&self, changes: Vec<RootDirtyChange>) {
        self.reference_search_cache.clear();
        let roots = self.workspace_roots.lock().await.clone();
        let include_paths = self.include_paths.lock().await.clone();
        let client = self.client.clone();
        let index_schedule = self.index_schedule.clone();
        let name_tables = self.name_tables.clone();
        let reach_graphs = self.reach_graphs.clone();
        let include_tables = self.include_tables.clone();
        let indexed_file_lists = self.indexed_file_lists.clone();
        let index_generations = self.index_generations.clone();
        let perf_logging_enabled = self
            .perf_logging_enabled
            .load(std::sync::atomic::Ordering::Relaxed);

        let mut state = index_schedule.lock().await;
        state.pending_requested = true;
        state.pending_changes.extend(changes);
        if state.running || state.scheduled {
            return;
        }
        state.scheduled = true;
        drop(state);

        tokio::spawn(async move {
            run_scheduled_indexes(
                client,
                roots,
                include_paths,
                name_tables,
                reach_graphs,
                include_tables,
                indexed_file_lists,
                index_generations,
                index_schedule,
                perf_logging_enabled,
            )
            .await;
        });
    }

    pub(super) async fn spawn_index_roots(&self, force: Option<bool>) {
        self.reference_search_cache.clear();
        let roots = self.workspace_roots.lock().await.clone();
        let include_paths = self.include_paths.lock().await.clone();
        let client = self.client.clone();
        let index_schedule = self.index_schedule.clone();
        let name_tables = self.name_tables.clone();
        let reach_graphs = self.reach_graphs.clone();
        let include_tables = self.include_tables.clone();
        let indexed_file_lists = self.indexed_file_lists.clone();
        let index_generations = self.index_generations.clone();
        let force = force.unwrap_or(false);
        let perf_logging_enabled = self
            .perf_logging_enabled
            .load(std::sync::atomic::Ordering::Relaxed);

        let mut state = index_schedule.lock().await;
        state.pending_requested = true;
        state.pending_full = true;
        state.pending_force |= force;
        state.pending_changes.clear();
        if state.running || state.scheduled {
            return;
        }
        state.scheduled = true;
        drop(state);

        tokio::spawn(async move {
            run_scheduled_indexes(
                client,
                roots,
                include_paths,
                name_tables,
                reach_graphs,
                include_tables,
                indexed_file_lists,
                index_generations,
                index_schedule,
                perf_logging_enabled,
            )
            .await;
        });
    }
}

async fn run_scheduled_indexes(
    client: Client,
    roots: Vec<PathBuf>,
    include_paths: Vec<String>,
    name_tables: NameTables,
    reach_graphs: ReachGraphs,
    include_tables: IncludeTables,
    indexed_file_lists: IndexedFileLists,
    index_generations: IndexGenerations,
    index_schedule: IndexSchedule,
    perf_logging_enabled: bool,
) {
    loop {
        sleep(INDEX_DEBOUNCE).await;

        let scheduled = {
            let mut state = index_schedule.lock().await;
            state.scheduled = false;
            state.running = true;
            state.pending_requested = false;
            if state.pending_full {
                state.pending_full = false;
                state.pending_changes.clear();
                let force = state.pending_force;
                state.pending_force = false;
                ScheduledIndex::Full { force }
            } else {
                let changes = std::mem::take(&mut state.pending_changes);
                ScheduledIndex::Dirty(changes)
            }
        };

        match scheduled {
            ScheduledIndex::Full { force } => {
                index_roots(
                    client.clone(),
                    roots.clone(),
                    include_paths.clone(),
                    name_tables.clone(),
                    reach_graphs.clone(),
                    include_tables.clone(),
                    indexed_file_lists.clone(),
                    index_generations.clone(),
                    force,
                    perf_logging_enabled,
                )
                .await;
            }
            ScheduledIndex::Dirty(changes) if !changes.is_empty() => {
                index_dirty_roots(
                    client.clone(),
                    include_paths.clone(),
                    name_tables.clone(),
                    reach_graphs.clone(),
                    include_tables.clone(),
                    indexed_file_lists.clone(),
                    index_generations.clone(),
                    changes,
                    perf_logging_enabled,
                )
                .await;
            }
            ScheduledIndex::Dirty(_) => {}
        }

        let should_continue = {
            let mut state = index_schedule.lock().await;
            state.running = false;
            if state.pending_requested {
                state.scheduled = true;
                true
            } else {
                false
            }
        };

        if !should_continue {
            break;
        }
    }
}

async fn index_roots(
    client: Client,
    roots: Vec<PathBuf>,
    include_paths: Vec<String>,
    name_tables: NameTables,
    reach_graphs: ReachGraphs,
    include_tables: IncludeTables,
    indexed_file_lists: IndexedFileLists,
    index_generations: IndexGenerations,
    force: bool,
    perf_logging_enabled: bool,
) {
    if roots.is_empty() {
        client
            .log_message(
                MessageType::WARNING,
                "FossilSense has no workspace root to index",
            )
            .await;
        return;
    }

    for root in roots {
        let display_root = root.display().to_string();
        client
            .log_message(MessageType::INFO, format!("scanning {}", display_root))
            .await;

        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let index_root = root.clone();
        let include_paths_for_index = include_paths.clone();
        let result = tokio::task::spawn_blocking(move || {
            indexer::index_workspace(
                index_root,
                IndexOptions {
                    db_path: None,
                    force,
                    include_paths: include_paths_for_index,
                    ..Default::default()
                },
                |status| {
                    let _ = sender.send(status);
                },
            )
        });

        while let Some(status) = receiver.recv().await {
            // During indexing a populated message denotes a scope-config warning
            // (see WorkspaceConfig::load); surface it without pattern-matching the
            // message text. Ready/Failed statuses carry their own messaging.
            if matches!(status.state, IndexState::Indexing) {
                if let Some(message) = &status.message {
                    client
                        .log_message(MessageType::WARNING, format!("config issue: {message}"))
                        .await;
                }
            }
            if matches!(status.state, IndexState::Ready) {
                continue;
            }
            client
                .send_notification::<IndexStatusNotification>(status)
                .await;
        }

        match result.await {
            Ok(Ok(mut stats)) => {
                client
                    .log_message(
                        MessageType::INFO,
                        format!(
                            "index complete for {}: {} files, {} symbols, elapsed={}ms (discover={}ms, parse={}ms, write={}ms, check={}ms, include_edge={}ms)",
                            display_root,
                            stats.total_files,
                            stats.symbols,
                            stats.elapsed_ms,
                            stats.discover_ms,
                            stats.parse_ms,
                            stats.write_ms,
                            stats.check_ms,
                            stats.include_edge_ms,
                        ),
                    )
                    .await;
                let nt_started = tokio::time::Instant::now();
                match rebuild_name_table(&name_tables, root.clone()).await {
                    Ok(count) => {
                        stats.name_table_ms = nt_started.elapsed().as_millis();
                        let rg_started = tokio::time::Instant::now();
                        let mut degraded = DegradedCapabilities::default();
                        degraded.reach_graph =
                            !rebuild_reach_graph(&client, &reach_graphs, root.clone()).await;
                        let include_count =
                            match rebuild_include_table(&include_tables, root.clone()).await {
                                Ok(count) => count,
                                Err(err) => {
                                    degraded.include_table = true;
                                    client
                                        .log_message(
                                            MessageType::WARNING,
                                            format!(
                                            "include completion table build failed for {}: {err:#}",
                                            display_root
                                        ),
                                        )
                                        .await;
                                    0
                                }
                            };
                        let ref_file_count = match rebuild_indexed_file_list(
                            &indexed_file_lists,
                            root.clone(),
                        )
                        .await
                        {
                            Ok(count) => count,
                            Err(err) => {
                                degraded.reference_file_list = true;
                                client
                                    .log_message(
                                        MessageType::WARNING,
                                        format!(
                                            "reference file-list build failed for {}: {err:#}",
                                            display_root
                                        ),
                                    )
                                    .await;
                                0
                            }
                        };
                        stats.reach_graph_ms = rg_started.elapsed().as_millis();
                        refresh_workspace_generation(
                            &index_generations,
                            &name_tables,
                            &reach_graphs,
                            &include_tables,
                            &indexed_file_lists,
                            root.clone(),
                        )
                        .await;
                        client
                            .log_message(
                                if degraded.any() {
                                    MessageType::WARNING
                                } else {
                                    MessageType::INFO
                                },
                                ready_cache_message(
                                    "name table ready",
                                    count,
                                    include_count,
                                    ref_file_count,
                                    stats.name_table_ms,
                                    stats.reach_graph_ms,
                                    &degraded,
                                ),
                            )
                            .await;
                        emit_perf_log(&client, perf_logging_enabled, || {
                            format!(
                                "[perf] index_full total={}ms discover={}ms check={}ms parse={}ms write={}ms include_edge={}ms name_table={}ms reach_graph={}ms force={}",
                                stats
                                    .elapsed_ms
                                    .saturating_add(stats.name_table_ms)
                                    .saturating_add(stats.reach_graph_ms),
                                stats.discover_ms,
                                stats.check_ms,
                                stats.parse_ms,
                                stats.write_ms,
                                stats.include_edge_ms,
                                stats.name_table_ms,
                                stats.reach_graph_ms,
                                force,
                            )
                        })
                        .await;
                        client
                            .send_notification::<IndexStatusNotification>(
                                IndexStatus::ready_with_degraded(display_root, &stats, degraded),
                            )
                            .await;
                    }
                    Err(err) => {
                        client
                            .send_notification::<IndexStatusNotification>(IndexStatus::failed(
                                display_root.clone(),
                                format!("name table build failed: {err:#}"),
                            ))
                            .await;
                        client
                            .log_message(
                                MessageType::ERROR,
                                format!("name table build failed for {}: {err:#}", display_root),
                            )
                            .await;
                    }
                }
            }
            Ok(Err(err)) => {
                client
                    .send_notification::<IndexStatusNotification>(IndexStatus::failed(
                        display_root.clone(),
                        format!("{err:#}"),
                    ))
                    .await;
                client
                    .log_message(
                        MessageType::ERROR,
                        format!("index failed for {}: {err:#}", display_root),
                    )
                    .await;
            }
            Err(err) => {
                client
                    .send_notification::<IndexStatusNotification>(IndexStatus::failed(
                        display_root.clone(),
                        err.to_string(),
                    ))
                    .await;
                client
                    .log_message(
                        MessageType::ERROR,
                        format!("index task failed for {}: {err}", display_root),
                    )
                    .await;
            }
        }
    }
}

async fn index_dirty_roots(
    client: Client,
    include_paths: Vec<String>,
    name_tables: NameTables,
    reach_graphs: ReachGraphs,
    include_tables: IncludeTables,
    indexed_file_lists: IndexedFileLists,
    index_generations: IndexGenerations,
    changes: Vec<RootDirtyChange>,
    perf_logging_enabled: bool,
) {
    let mut latest_by_file: HashMap<(PathBuf, String), RootDirtyChange> = HashMap::new();
    for change in changes {
        latest_by_file.insert((change.root.clone(), change.rel_path.clone()), change);
    }

    let mut by_root: HashMap<PathBuf, Vec<RootDirtyChange>> = HashMap::new();
    for (_, change) in latest_by_file {
        by_root.entry(change.root.clone()).or_default().push(change);
    }

    for (root, changes) in by_root {
        let display_root = root.display().to_string();
        let rel_paths: Vec<String> = changes
            .iter()
            .map(|change| change.rel_path.clone())
            .collect();
        client
            .log_message(
                MessageType::INFO,
                format!(
                    "updating {} dirty files for {}",
                    rel_paths.len(),
                    display_root
                ),
            )
            .await;

        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let index_root = root.clone();
        let include_paths_for_index = include_paths.clone();
        let dirty_changes: Vec<indexer::DirtyFileChange> =
            changes.into_iter().map(|change| change.change).collect();
        let result = tokio::task::spawn_blocking(move || {
            indexer::index_dirty_files(
                index_root,
                dirty_changes,
                IndexOptions {
                    db_path: None,
                    force: false,
                    include_paths: include_paths_for_index,
                    ..Default::default()
                },
                |status| {
                    let _ = sender.send(status);
                },
            )
        });

        while let Some(status) = receiver.recv().await {
            if matches!(status.state, IndexState::Indexing) {
                if let Some(message) = &status.message {
                    client
                        .log_message(MessageType::WARNING, format!("config issue: {message}"))
                        .await;
                }
            }
            if matches!(status.state, IndexState::Ready) {
                continue;
            }
            client
                .send_notification::<IndexStatusNotification>(status)
                .await;
        }

        match result.await {
            Ok(Ok(mut stats)) => {
                client
                    .log_message(
                        MessageType::INFO,
                        format!(
                            "dirty update complete for {}: {} files, indexed={}, deleted={}, symbols={}, elapsed={}ms (parse={}ms, write={}ms, check={}ms, include_edge={}ms)",
                            display_root,
                            stats.total_files,
                            stats.indexed_files,
                            stats.deleted_files,
                            stats.symbols,
                            stats.elapsed_ms,
                            stats.parse_ms,
                            stats.write_ms,
                            stats.check_ms,
                            stats.include_edge_ms,
                        ),
                    )
                    .await;
                let nt_started = tokio::time::Instant::now();
                match update_name_table_paths(&name_tables, root.clone(), &rel_paths).await {
                    Ok(count) => {
                        stats.name_table_ms = nt_started.elapsed().as_millis();
                        let rg_started = tokio::time::Instant::now();
                        let mut degraded = DegradedCapabilities::default();
                        degraded.reach_graph = !refresh_reach_graph_incremental(
                            &client,
                            &reach_graphs,
                            root.clone(),
                            &stats.include_edge_sources_rebuilt,
                        )
                        .await;
                        let include_count = match rebuild_include_table(
                            &include_tables,
                            root.clone(),
                        )
                        .await
                        {
                            Ok(count) => count,
                            Err(err) => {
                                degraded.include_table = true;
                                client
                                        .log_message(
                                            MessageType::WARNING,
                                            format!(
                                                "include completion table update failed for {}: {err:#}",
                                                display_root
                                            ),
                                        )
                                        .await;
                                0
                            }
                        };
                        let ref_file_count = match rebuild_indexed_file_list(
                            &indexed_file_lists,
                            root.clone(),
                        )
                        .await
                        {
                            Ok(count) => count,
                            Err(err) => {
                                degraded.reference_file_list = true;
                                client
                                    .log_message(
                                        MessageType::WARNING,
                                        format!(
                                            "reference file-list update failed for {}: {err:#}",
                                            display_root
                                        ),
                                    )
                                    .await;
                                0
                            }
                        };
                        stats.reach_graph_ms = rg_started.elapsed().as_millis();
                        refresh_workspace_generation(
                            &index_generations,
                            &name_tables,
                            &reach_graphs,
                            &include_tables,
                            &indexed_file_lists,
                            root.clone(),
                        )
                        .await;
                        client
                            .log_message(
                                if degraded.any() {
                                    MessageType::WARNING
                                } else {
                                    MessageType::INFO
                                },
                                ready_cache_message(
                                    "name table updated",
                                    count,
                                    include_count,
                                    ref_file_count,
                                    stats.name_table_ms,
                                    stats.reach_graph_ms,
                                    &degraded,
                                ),
                            )
                            .await;
                        emit_perf_log(&client, perf_logging_enabled, || {
                            format!(
                                "[perf] index_dirty_update total={}ms check={}ms parse={}ms write={}ms include_edge={}ms name_table={}ms reach_graph={}ms indexed={} deleted={}",
                                stats
                                    .elapsed_ms
                                    .saturating_add(stats.name_table_ms)
                                    .saturating_add(stats.reach_graph_ms),
                                stats.check_ms,
                                stats.parse_ms,
                                stats.write_ms,
                                stats.include_edge_ms,
                                stats.name_table_ms,
                                stats.reach_graph_ms,
                                stats.indexed_files,
                                stats.deleted_files,
                            )
                        })
                        .await;
                        client
                            .send_notification::<IndexStatusNotification>(
                                IndexStatus::ready_with_degraded(display_root, &stats, degraded),
                            )
                            .await;
                    }
                    Err(err) => {
                        client
                            .send_notification::<IndexStatusNotification>(IndexStatus::failed(
                                display_root.clone(),
                                format!("name table update failed: {err:#}"),
                            ))
                            .await;
                        client
                            .log_message(
                                MessageType::ERROR,
                                format!("name table update failed for {}: {err:#}", display_root),
                            )
                            .await;
                    }
                }
            }
            Ok(Err(err)) => {
                client
                    .send_notification::<IndexStatusNotification>(IndexStatus::failed(
                        display_root.clone(),
                        format!("{err:#}"),
                    ))
                    .await;
                client
                    .log_message(
                        MessageType::ERROR,
                        format!("dirty update failed for {}: {err:#}", display_root),
                    )
                    .await;
            }
            Err(err) => {
                client
                    .send_notification::<IndexStatusNotification>(IndexStatus::failed(
                        display_root.clone(),
                        err.to_string(),
                    ))
                    .await;
                client
                    .log_message(
                        MessageType::ERROR,
                        format!("dirty update task failed for {}: {err}", display_root),
                    )
                    .await;
            }
        }
    }
}
