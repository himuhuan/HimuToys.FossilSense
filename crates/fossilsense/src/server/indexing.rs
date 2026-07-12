use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::time::{sleep, Duration};
use tower_lsp::lsp_types::notification::Notification;
use tower_lsp::lsp_types::{FileChangeType, FileEvent, MessageType};
use tower_lsp::Client;

use super::{
    emit_perf_log, uri_to_path, Backend, CacheLedger, CachePublishReport, DocumentStore,
    IndexSchedule,
};
use crate::indexer::{self, IndexOptions};
use crate::pathing;
use crate::progress::{IndexState, IndexStatus};

mod cache;
mod watch;

pub(super) use cache::ready_cache_message;
#[cfg(test)]
pub(super) use cache::{rebuild_include_table, rebuild_indexed_file_list};
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
    ProjectContext(PathBuf),
    Dirty(RootDirtyChange),
}

enum ScheduledIndex {
    Full { force: bool },
    Dirty(Vec<RootDirtyChange>),
}

#[derive(Clone)]
struct IndexWorkspaceState {
    documents: DocumentStore,
    roots: Arc<tokio::sync::Mutex<Vec<PathBuf>>>,
}

enum IndexStatusNotification {}

impl Notification for IndexStatusNotification {
    type Params = IndexStatus;
    const METHOD: &'static str = "fossilsense/indexStatus";
}

enum ProjectContextChangedNotification {}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectContextChanged {
    workspace_root_id: String,
    project_count: usize,
}

impl Notification for ProjectContextChangedNotification {
    type Params = ProjectContextChanged;
    const METHOD: &'static str = "fossilsense/projectContextChanged";
}

impl Backend {
    pub(super) async fn refresh_project_context_roots(&self, mut roots: Vec<PathBuf>) {
        roots.sort();
        roots.dedup();
        for root in roots {
            match self
                .session
                .cache
                .refresh_project_context(&self.client, root.clone())
                .await
            {
                Ok(count) => {
                    self.client
                        .log_message(
                            MessageType::INFO,
                            format!(
                                "project context refreshed for {}: {} projects",
                                root.display(),
                                count
                            ),
                        )
                        .await;
                    self.client
                        .send_notification::<ProjectContextChangedNotification>(
                            ProjectContextChanged {
                                workspace_root_id: pathing::workspace_hash(&root),
                                project_count: count,
                            },
                        )
                        .await;
                }
                Err(err) => {
                    self.client
                        .log_message(
                            MessageType::WARNING,
                            format!(
                                "project context refresh failed for {}: {err:#}",
                                root.display()
                            ),
                        )
                        .await;
                }
            }
        }
    }

    pub(super) async fn spawn_dirty_files(&self, changes: Vec<RootDirtyChange>) {
        self.session.cache.invalidate_after_index_change().await;
        let workspace_state = IndexWorkspaceState {
            documents: self.session.documents.clone(),
            roots: self.workspace_roots.clone(),
        };
        let include_paths = self.include_paths.lock().await.clone();
        let client = self.client.clone();
        let index_schedule = self.index_schedule.clone();
        let cache = self.session.cache.clone();
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
                workspace_state,
                include_paths,
                cache,
                index_schedule,
                perf_logging_enabled,
            )
            .await;
        });
    }

    pub(super) async fn spawn_index_roots(&self, force: Option<bool>) {
        self.session.cache.invalidate_after_index_change().await;
        let workspace_state = IndexWorkspaceState {
            documents: self.session.documents.clone(),
            roots: self.workspace_roots.clone(),
        };
        let include_paths = self.include_paths.lock().await.clone();
        let client = self.client.clone();
        let index_schedule = self.index_schedule.clone();
        let cache = self.session.cache.clone();
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
                workspace_state,
                include_paths,
                cache,
                index_schedule,
                perf_logging_enabled,
            )
            .await;
        });
    }
}

async fn run_scheduled_indexes(
    client: Client,
    workspace_state: IndexWorkspaceState,
    include_paths: Vec<String>,
    cache: CacheLedger,
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
                let roots = workspace_state.roots.lock().await.clone();
                index_roots(
                    client.clone(),
                    roots.clone(),
                    include_paths.clone(),
                    cache.clone(),
                    workspace_state.clone(),
                    force,
                    perf_logging_enabled,
                )
                .await;
            }
            ScheduledIndex::Dirty(changes) if !changes.is_empty() => {
                index_dirty_roots(
                    client.clone(),
                    include_paths.clone(),
                    cache.clone(),
                    workspace_state.clone(),
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
    cache: CacheLedger,
    workspace_state: IndexWorkspaceState,
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
                if let Some(warning) = &stats.maintenance_warning {
                    client
                        .log_message(MessageType::WARNING, warning.clone())
                        .await;
                }
                client
                    .log_message(
                        MessageType::INFO,
                        format!(
                            "index complete for {}: {} files, {} symbols, elapsed={}ms (discover={}ms, parse={}ms, write={}ms, secondary_index={}ms, publication={}ms, check={}ms, include_edge={}ms)",
                            display_root,
                            stats.total_files,
                            stats.symbols,
                            stats.elapsed_ms,
                            stats.discover_ms,
                            stats.parse_ms,
                            stats.write_ms,
                            stats.secondary_index_ms,
                            stats.publication_ms,
                            stats.check_ms,
                            stats.include_edge_ms,
                        ),
                    )
                    .await;
                match cache.publish_full_index(&client, root.clone()).await {
                    Ok(report) => {
                        if !workspace_state.roots.lock().await.contains(&root) {
                            cache
                                .remove_workspace_roots(std::slice::from_ref(&root))
                                .await;
                            continue;
                        }
                        workspace_state
                            .documents
                            .reconcile_published_files(
                                root.clone(),
                                None,
                                report.semantic_generation,
                            )
                            .await;
                        stats.name_table_ms = report.name_table_ms;
                        stats.reach_graph_ms = report.reach_graph_ms;
                        let _published_epoch = report.epoch;
                        log_cache_degradation(&client, &display_root, "build", &report).await;
                        client
                            .log_message(
                                if report.degraded.any() {
                                    MessageType::WARNING
                                } else {
                                    MessageType::INFO
                                },
                                ready_cache_message(
                                    "name table ready",
                                    report.symbol_count,
                                    report.include_count,
                                    report.reference_file_count,
                                    stats.name_table_ms,
                                    stats.reach_graph_ms,
                                    &report.degraded,
                                ),
                            )
                            .await;
                        emit_perf_log(&client, perf_logging_enabled, || {
                            format!(
                                "[perf] index_full total={}ms discover={}ms check={}ms parse={}ms write={}ms secondary_index={}ms publication={}ms include_edge={}ms name_table={}ms reach_graph={}ms force={}",
                                stats
                                    .elapsed_ms
                                    .saturating_add(stats.name_table_ms)
                                    .saturating_add(stats.reach_graph_ms),
                                stats.discover_ms,
                                stats.check_ms,
                                stats.parse_ms,
                                stats.write_ms,
                                stats.secondary_index_ms,
                                stats.publication_ms,
                                stats.include_edge_ms,
                                stats.name_table_ms,
                                stats.reach_graph_ms,
                                force,
                            )
                        })
                        .await;
                        client
                            .send_notification::<IndexStatusNotification>(
                                IndexStatus::ready_with_degraded(
                                    display_root,
                                    &stats,
                                    report.degraded,
                                ),
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
    cache: CacheLedger,
    workspace_state: IndexWorkspaceState,
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
        if !workspace_state.roots.lock().await.contains(&root) {
            continue;
        }
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
                if let Some(warning) = &stats.maintenance_warning {
                    client
                        .log_message(MessageType::WARNING, warning.clone())
                        .await;
                }
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
                match cache
                    .publish_dirty_index(
                        &client,
                        root.clone(),
                        &rel_paths,
                        &stats.include_edge_sources_rebuilt,
                    )
                    .await
                {
                    Ok(report) => {
                        if !workspace_state.roots.lock().await.contains(&root) {
                            cache
                                .remove_workspace_roots(std::slice::from_ref(&root))
                                .await;
                            continue;
                        }
                        workspace_state
                            .documents
                            .reconcile_published_files(
                                root.clone(),
                                Some(rel_paths.clone()),
                                report.semantic_generation,
                            )
                            .await;
                        stats.name_table_ms = report.name_table_ms;
                        stats.reach_graph_ms = report.reach_graph_ms;
                        let _published_epoch = report.epoch;
                        log_cache_degradation(&client, &display_root, "update", &report).await;
                        client
                            .log_message(
                                if report.degraded.any() {
                                    MessageType::WARNING
                                } else {
                                    MessageType::INFO
                                },
                                ready_cache_message(
                                    "name table updated",
                                    report.symbol_count,
                                    report.include_count,
                                    report.reference_file_count,
                                    stats.name_table_ms,
                                    stats.reach_graph_ms,
                                    &report.degraded,
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
                                IndexStatus::ready_with_degraded(
                                    display_root,
                                    &stats,
                                    report.degraded,
                                ),
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

async fn log_cache_degradation(
    client: &Client,
    display_root: &str,
    operation: &str,
    report: &CachePublishReport,
) {
    if report.degraded.include_table {
        let detail = report
            .include_table_error
            .as_deref()
            .unwrap_or("unavailable");
        client
            .log_message(
                MessageType::WARNING,
                format!("include completion table {operation} failed for {display_root}: {detail}"),
            )
            .await;
    }
    if report.degraded.reference_file_list {
        let detail = report
            .reference_file_list_error
            .as_deref()
            .unwrap_or("unavailable");
        client
            .log_message(
                MessageType::WARNING,
                format!("reference file-list {operation} failed for {display_root}: {detail}"),
            )
            .await;
    }
}
