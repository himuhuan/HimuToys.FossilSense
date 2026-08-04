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
pub(crate) use cache::{hydrate_memory_report, snapshot_memory_report_from_parts};
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
    pub(super) pending_all_roots: bool,
    pub(super) pending_full_roots: Vec<PathBuf>,
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
    Full(PathBuf),
    ProjectContext(PathBuf),
    Dirty(RootDirtyChange),
}

pub(super) enum ScheduledIndex {
    Full {
        roots: Option<Vec<PathBuf>>,
        force: bool,
        changes: Vec<RootDirtyChange>,
    },
    Dirty(Vec<RootDirtyChange>),
}

impl IndexScheduleState {
    pub(super) fn request_dirty_changes(&mut self, changes: Vec<RootDirtyChange>) {
        self.pending_requested = true;
        self.pending_changes.extend(changes);
    }

    pub(super) fn request_all_roots(&mut self, force: bool) {
        self.pending_requested = true;
        self.pending_full = true;
        self.pending_all_roots = true;
        self.pending_full_roots.clear();
        self.pending_force |= force;
        self.pending_changes.clear();
    }

    pub(super) fn request_full_roots(&mut self, roots: Vec<PathBuf>) {
        if roots.is_empty() {
            return;
        }
        self.pending_requested = true;
        self.pending_full = true;
        if self.pending_all_roots {
            return;
        }
        self.pending_full_roots.extend(roots);
        self.pending_full_roots.sort();
        self.pending_full_roots.dedup();
        self.pending_changes
            .retain(|change| !self.pending_full_roots.contains(&change.root));
    }

    pub(super) fn take_scheduled_index(&mut self) -> ScheduledIndex {
        self.pending_requested = false;
        if !self.pending_full {
            return ScheduledIndex::Dirty(std::mem::take(&mut self.pending_changes));
        }

        self.pending_full = false;
        let force = std::mem::take(&mut self.pending_force);
        let roots = if std::mem::take(&mut self.pending_all_roots) {
            self.pending_full_roots.clear();
            self.pending_changes.clear();
            None
        } else {
            let roots = std::mem::take(&mut self.pending_full_roots);
            self.pending_changes
                .retain(|change| !roots.contains(&change.root));
            Some(roots)
        };
        let changes = std::mem::take(&mut self.pending_changes);
        ScheduledIndex::Full {
            roots,
            force,
            changes,
        }
    }
}

#[derive(Clone)]
struct IndexWorkspaceState {
    documents: DocumentStore,
    roots: Arc<tokio::sync::Mutex<Vec<PathBuf>>>,
}

#[derive(Clone)]
struct IndexClientConfiguration {
    include_paths: Vec<String>,
    go_module_paths: Vec<String>,
    protobuf_c_enabled: Option<bool>,
    protobuf_c_proto_paths: Vec<String>,
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
        let configuration = IndexClientConfiguration {
            include_paths: self.include_paths.lock().await.clone(),
            go_module_paths: self.go_module_paths.lock().await.clone(),
            protobuf_c_enabled: *self.protobuf_c_enabled.lock().await,
            protobuf_c_proto_paths: self.protobuf_c_proto_paths.lock().await.clone(),
        };
        let client = self.client.clone();
        let index_schedule = self.index_schedule.clone();
        let cache = self.session.cache.clone();
        let perf_logging_enabled = self
            .perf_logging_enabled
            .load(std::sync::atomic::Ordering::Relaxed);

        let mut state = index_schedule.lock().await;
        state.request_dirty_changes(changes);
        if state.running || state.scheduled {
            return;
        }
        state.scheduled = true;
        drop(state);

        tokio::spawn(async move {
            run_scheduled_indexes(
                client,
                workspace_state,
                configuration,
                cache,
                index_schedule,
                perf_logging_enabled,
            )
            .await;
        });
    }

    pub(super) async fn spawn_index_roots(&self, force: Option<bool>) {
        self.spawn_index_roots_with_scope(None, force.unwrap_or(false))
            .await;
    }

    pub(super) async fn spawn_index_root_changes(&self, roots: Vec<PathBuf>) {
        self.spawn_index_roots_with_scope(Some(roots), false).await;
    }

    async fn spawn_index_roots_with_scope(&self, roots: Option<Vec<PathBuf>>, force: bool) {
        self.session.cache.invalidate_after_index_change().await;
        let root_scope = match roots.as_ref() {
            Some(roots) => roots.clone(),
            None => self.workspace_roots.lock().await.clone(),
        };
        // A user-triggered refresh/rebuild must observe fossilsense.json even
        // when no file-watcher event arrived.
        self.config_cache
            .lock()
            .await
            .retain(|root, _| !root_scope.contains(root));
        #[cfg(test)]
        self.invalidate_external_source_root_cache(&root_scope)
            .await;
        let workspace_state = IndexWorkspaceState {
            documents: self.session.documents.clone(),
            roots: self.workspace_roots.clone(),
        };
        let configuration = IndexClientConfiguration {
            include_paths: self.include_paths.lock().await.clone(),
            go_module_paths: self.go_module_paths.lock().await.clone(),
            protobuf_c_enabled: *self.protobuf_c_enabled.lock().await,
            protobuf_c_proto_paths: self.protobuf_c_proto_paths.lock().await.clone(),
        };
        let client = self.client.clone();
        let index_schedule = self.index_schedule.clone();
        let cache = self.session.cache.clone();
        let perf_logging_enabled = self
            .perf_logging_enabled
            .load(std::sync::atomic::Ordering::Relaxed);

        let mut state = index_schedule.lock().await;
        if let Some(roots) = roots {
            state.request_full_roots(roots);
        } else {
            state.request_all_roots(force);
        }
        if state.running || state.scheduled {
            return;
        }
        if !state.pending_requested {
            return;
        }
        state.scheduled = true;
        drop(state);

        tokio::spawn(async move {
            run_scheduled_indexes(
                client,
                workspace_state,
                configuration,
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
    configuration: IndexClientConfiguration,
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
            state.take_scheduled_index()
        };

        match scheduled {
            ScheduledIndex::Full {
                roots,
                force,
                changes,
            } => {
                let current_roots = workspace_state.roots.lock().await.clone();
                let roots = roots.map_or_else(
                    || current_roots.clone(),
                    |mut scoped| {
                        scoped.retain(|root| current_roots.contains(root));
                        scoped
                    },
                );
                index_roots(
                    client.clone(),
                    roots,
                    configuration.clone(),
                    cache.clone(),
                    workspace_state.clone(),
                    force,
                    perf_logging_enabled,
                )
                .await;
                if !changes.is_empty() {
                    index_dirty_roots(
                        client.clone(),
                        configuration.clone(),
                        cache.clone(),
                        workspace_state.clone(),
                        changes,
                        perf_logging_enabled,
                    )
                    .await;
                }
            }
            ScheduledIndex::Dirty(changes) if !changes.is_empty() => {
                index_dirty_roots(
                    client.clone(),
                    configuration.clone(),
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
    configuration: IndexClientConfiguration,
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
        let IndexClientConfiguration {
            include_paths: include_paths_for_index,
            go_module_paths: go_module_paths_for_index,
            protobuf_c_enabled: protobuf_c_enabled_for_index,
            protobuf_c_proto_paths: protobuf_c_proto_paths_for_index,
        } = configuration.clone();
        let result = tokio::task::spawn_blocking(move || {
            let prepared = indexer::prepare_index_configuration(
                &index_root,
                &include_paths_for_index,
                &go_module_paths_for_index,
                protobuf_c_enabled_for_index,
                &protobuf_c_proto_paths_for_index,
            )?;
            let stats = indexer::index_workspace(
                &index_root,
                IndexOptions {
                    db_path: None,
                    force,
                    include_paths: include_paths_for_index,
                    go_module_paths: go_module_paths_for_index,
                    protobuf_c_enabled: protobuf_c_enabled_for_index,
                    protobuf_c_proto_paths: protobuf_c_proto_paths_for_index,
                    prepared_configuration: Some(prepared.clone()),
                    ..Default::default()
                },
                |status| {
                    let _ = sender.send(status);
                },
            )?;
            let workspace_semantics = Arc::new(
                super::workspace_config::PublishedWorkspaceSemantics::from_index_configuration(
                    &index_root,
                    &prepared,
                ),
            );
            Ok::<_, anyhow::Error>((stats, workspace_semantics))
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
            Ok(Ok((mut stats, workspace_semantics))) => {
                if let Some(warning) = &stats.maintenance_warning {
                    client
                        .log_message(MessageType::WARNING, warning.clone())
                        .await;
                }
                client
                    .log_message(
                        MessageType::INFO,
                        format!(
                            "index complete for {}: {} files, {} declarations, elapsed={}ms (discover={}ms, parse={}ms, write={}ms, secondary_index={}ms, publication={}ms, check={}ms, include_edge={}ms)",
                            display_root,
                            stats.total_files,
                            stats.declarations,
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
                match cache
                    .publish_full_index_with_semantics(&client, root.clone(), workspace_semantics)
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
                                    report.declaration_count,
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
    configuration: IndexClientConfiguration,
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
        let published = cache.current_engine_snapshot(&root).await;
        let store_generation = cache::load_store_semantic_generation(root.clone()).await;
        let may_increment = published.as_ref().is_some_and(|snapshot| {
            snapshot.semantic_generation != crate::call_model::SemanticGeneration::MISSING
                && store_generation
                    .as_ref()
                    .is_ok_and(|generation| *generation == snapshot.semantic_generation)
        });
        if !may_increment {
            client
                .log_message(
                    MessageType::WARNING,
                    format!(
                        "dirty update for {} has no direct published base; rebuilding the full workspace",
                        root.display()
                    ),
                )
                .await;
            index_roots(
                client.clone(),
                vec![root],
                configuration.clone(),
                cache.clone(),
                workspace_state.clone(),
                false,
                perf_logging_enabled,
            )
            .await;
            continue;
        }
        let workspace_semantics = published
            .expect("incremental eligibility requires a published snapshot")
            .workspace_semantics
            .clone();
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
        let IndexClientConfiguration {
            include_paths: include_paths_for_index,
            go_module_paths: go_module_paths_for_index,
            protobuf_c_enabled: protobuf_c_enabled_for_index,
            protobuf_c_proto_paths: protobuf_c_proto_paths_for_index,
        } = configuration.clone();
        let workspace_semantics_for_index = workspace_semantics.clone();
        let dirty_changes: Vec<indexer::DirtyFileChange> =
            changes.into_iter().map(|change| change.change).collect();
        let result = tokio::task::spawn_blocking(move || {
            let prepared = workspace_semantics_for_index.index_configuration_snapshot();
            let stats = indexer::index_dirty_files(
                &index_root,
                dirty_changes,
                IndexOptions {
                    db_path: None,
                    force: false,
                    include_paths: include_paths_for_index,
                    go_module_paths: go_module_paths_for_index,
                    protobuf_c_enabled: protobuf_c_enabled_for_index,
                    protobuf_c_proto_paths: protobuf_c_proto_paths_for_index,
                    prepared_configuration: Some(prepared.clone()),
                    ..Default::default()
                },
                |status| {
                    let _ = sender.send(status);
                },
            )?;
            Ok::<_, anyhow::Error>((stats, workspace_semantics_for_index))
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
            Ok(Ok((mut stats, workspace_semantics))) => {
                if let Some(warning) = &stats.maintenance_warning {
                    client
                        .log_message(MessageType::WARNING, warning.clone())
                        .await;
                }
                client
                    .log_message(
                        MessageType::INFO,
                        format!(
                            "dirty update complete for {}: {} files, indexed={}, deleted={}, declarations={}, elapsed={}ms (parse={}ms, write={}ms, check={}ms, include_edge={}ms)",
                            display_root,
                            stats.total_files,
                            stats.indexed_files,
                            stats.deleted_files,
                            stats.declarations,
                            stats.elapsed_ms,
                            stats.parse_ms,
                            stats.write_ms,
                            stats.check_ms,
                            stats.include_edge_ms,
                        ),
                    )
                    .await;
                match cache
                    .publish_dirty_index_with_semantics(
                        &client,
                        root.clone(),
                        &rel_paths,
                        &stats.include_edge_sources_rebuilt,
                        workspace_semantics,
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
                                    report.declaration_count,
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

#[cfg(test)]
impl Backend {
    pub(super) async fn run_dirty_index_for_test(&self, changes: Vec<RootDirtyChange>) {
        index_dirty_roots(
            self.client.clone(),
            IndexClientConfiguration {
                include_paths: self.include_paths.lock().await.clone(),
                go_module_paths: self.go_module_paths.lock().await.clone(),
                protobuf_c_enabled: *self.protobuf_c_enabled.lock().await,
                protobuf_c_proto_paths: self.protobuf_c_proto_paths.lock().await.clone(),
            },
            self.session.cache.clone(),
            IndexWorkspaceState {
                documents: self.session.documents.clone(),
                roots: self.workspace_roots.clone(),
            },
            changes,
            false,
        )
        .await;
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
    if report.degraded.go_import_table {
        let detail = report
            .go_import_table_error
            .as_deref()
            .unwrap_or("unavailable");
        client
            .log_message(
                MessageType::WARNING,
                format!(
                    "Go import completion table {operation} failed for {display_root}: {detail}"
                ),
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
