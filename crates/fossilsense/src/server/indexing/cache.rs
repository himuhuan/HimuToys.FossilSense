use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use tower_lsp::lsp_types::MessageType;
use tower_lsp::Client;

use crate::candidate_service::IncludePathIndex;
use crate::completion::ordinary_service::FallbackCompletionNameTable;
use crate::config::WorkspaceConfig;
use crate::declaration_index::SemanticDeclarationIndex;
use crate::memory_report::{
    DeclarationCacheSample, HydratedMemoryReport, MemoryReport, OpenDocumentsMemoryReport,
    SnapshotMemoryReport,
};
use crate::pathing;
use crate::progress::DegradedCapabilities;
use crate::project_context::{self, ProjectContextIndex};
use crate::reachability::ReachGraph;
use crate::resource::current_process_memory_bytes;
use crate::server::workspace::EngineSnapshot;
use crate::server::{
    CacheLedger, CachePublishReport, GoImportCompletionTable, IncludeCompletionTable,
};
use crate::store::IndexStore;

mod declaration_models;
use declaration_models::{
    build_declaration_index_from_db, capture_call_read_handle, load_semantic_generation,
    rebuild_declaration_index, rebuild_fallback_completion_table, rebuild_project_context,
    update_declaration_index_paths,
};
mod message;
pub(in crate::server) use message::ready_cache_message;

async fn load_reach_graph(root: PathBuf) -> Result<Arc<ReachGraph>> {
    let built = tokio::task::spawn_blocking(move || -> Result<ReachGraph> {
        let db_path = pathing::default_index_path(&root)?;
        build_reach_graph_from_db(&db_path)
    })
    .await;

    match built {
        Ok(Ok(graph)) => Ok(Arc::new(graph)),
        Ok(Err(err)) => Err(err),
        Err(err) => Err(err.into()),
    }
}

pub(super) async fn load_store_semantic_generation(
    root: PathBuf,
) -> Result<crate::call_model::SemanticGeneration> {
    load_semantic_generation(root).await
}

fn build_reach_graph_from_db(db_path: &Path) -> Result<ReachGraph> {
    let store = IndexStore::open_readonly(db_path)?;
    let reach_view = store.reach_graph_view();
    let package_view = store.go_package_graph_view();
    Ok(ReachGraph::from_rows_with_packages(
        reach_view.include_edges()?,
        reach_view.unresolved_includes()?,
        reach_view.ambiguous_includes()?,
        package_view.package_files()?,
        package_view.package_edges()?,
        package_view.open_packages()?,
    ))
}

async fn rebuild_reach_graph(client: &Client, root: PathBuf) -> Option<Arc<ReachGraph>> {
    match load_reach_graph(root).await {
        Ok(graph) => Some(graph),
        Err(err) => {
            client
                .log_message(
                    MessageType::WARNING,
                    format!("reachability graph build failed: {err:#}"),
                )
                .await;
            None
        }
    }
}

/// Prepare an incremental graph generation without mutating `previous`. If the
/// scoped store load cannot be used, fall back to a full immutable rebuild.
async fn refresh_reach_graph_incremental(
    client: &Client,
    previous: Option<Arc<ReachGraph>>,
    root: PathBuf,
    source_paths: &[String],
) -> Option<Arc<ReachGraph>> {
    if source_paths.is_empty() {
        return match previous {
            Some(graph) => Some(graph),
            None => rebuild_reach_graph(client, root).await,
        };
    }
    if source_paths.iter().any(|path| path.ends_with(".go")) {
        return rebuild_reach_graph(client, root).await;
    }

    let Some(previous) = previous else {
        return rebuild_reach_graph(client, root).await;
    };

    let sources = source_paths.to_vec();
    let load_sources = sources.clone();
    let load_root = root.clone();
    let loaded = tokio::task::spawn_blocking(move || -> Result<_> {
        let db_path = pathing::default_index_path(&load_root)?;
        let store = IndexStore::open_readonly(&db_path)?;
        store
            .reach_graph_view()
            .include_data_for_sources(&load_sources)
    })
    .await;

    match loaded {
        Ok(Ok((edges, open))) => {
            let graph = Arc::new(previous.with_refreshed_sources_from_rows(&sources, edges, open));
            client
                .log_message(
                    MessageType::INFO,
                    format!(
                        "reach graph incrementally refreshed for {} sources",
                        sources.len()
                    ),
                )
                .await;
            Some(graph)
        }
        Ok(Err(_)) | Err(_) => {
            client
                .log_message(
                    MessageType::INFO,
                    "reach graph refresh unavailable, falling back to full rebuild".to_string(),
                )
                .await;
            rebuild_reach_graph(client, root).await
        }
    }
}

pub(in crate::server) async fn rebuild_include_table(
    root: PathBuf,
) -> Result<Arc<IncludeCompletionTable>> {
    let built = tokio::task::spawn_blocking(move || -> Result<IncludeCompletionTable> {
        let db_path = pathing::default_index_path(&root)?;
        build_include_table_from_db(&db_path)
    })
    .await;

    match built {
        Ok(Ok(table)) => Ok(Arc::new(table)),
        Ok(Err(err)) => Err(err),
        Err(err) => Err(err.into()),
    }
}

fn build_include_table_from_db(db_path: &Path) -> Result<IncludeCompletionTable> {
    let store = IndexStore::open_readonly(db_path)?;
    Ok(IncludeCompletionTable::build_from_rows(
        store.include_table_view().workspace_paths()?,
        store.reach_graph_view().include_edges()?,
    ))
}

pub(in crate::server) async fn rebuild_go_import_table(
    root: PathBuf,
) -> Result<Arc<GoImportCompletionTable>> {
    let built = tokio::task::spawn_blocking(move || -> Result<GoImportCompletionTable> {
        let db_path = pathing::default_index_path(&root)?;
        let store = IndexStore::open_readonly(&db_path)?;
        Ok(GoImportCompletionTable::build(
            store.go_package_graph_view().importable_packages()?,
        ))
    })
    .await;

    match built {
        Ok(Ok(table)) => Ok(Arc::new(table)),
        Ok(Err(err)) => Err(err),
        Err(err) => Err(err.into()),
    }
}

pub(in crate::server) async fn rebuild_indexed_file_list(
    root: PathBuf,
) -> Result<Arc<Vec<(String, PathBuf)>>> {
    let build_root = root.clone();
    let built = tokio::task::spawn_blocking(move || -> Result<Vec<(String, PathBuf)>> {
        let db_path = pathing::default_index_path(&build_root)?;
        build_indexed_file_list_from_db(&db_path, &build_root)
    })
    .await;

    match built {
        Ok(Ok(files)) => Ok(Arc::new(files)),
        Ok(Err(err)) => Err(err),
        Err(err) => Err(err.into()),
    }
}

fn build_indexed_file_list_from_db(db_path: &Path, root: &Path) -> Result<Vec<(String, PathBuf)>> {
    let store = IndexStore::open_readonly(db_path)?;
    Ok(store
        .reference_file_view()
        .indexed_workspace_files()?
        .into_iter()
        .map(|row| {
            let abs = root.join(row.path.replace('/', std::path::MAIN_SEPARATOR_STR));
            (row.path, abs)
        })
        .collect())
}

async fn rebuild_include_path_index(root: PathBuf) -> Result<Arc<IncludePathIndex>> {
    let built = tokio::task::spawn_blocking(move || -> Result<IncludePathIndex> {
        let db_path = pathing::default_index_path(&root)?;
        build_include_path_index_from_db(&db_path)
    })
    .await;

    match built {
        Ok(Ok(index)) => Ok(Arc::new(index)),
        Ok(Err(err)) => Err(err),
        Err(err) => Err(err.into()),
    }
}

fn build_include_path_index_from_db(db_path: &Path) -> Result<IncludePathIndex> {
    let store = IndexStore::open_readonly(db_path)?;
    Ok(IncludePathIndex::build(
        store.include_table_view().include_resolution_paths()?,
    ))
}

/// Static per-generation memory report assembled from engine-snapshot parts.
/// Shared by the resource monitor (once per published generation) and the
/// headless `fossilsense memory` CLI. Values are structure-level estimates;
/// the process-level gates stay authoritative.
pub(crate) fn snapshot_memory_report_from_parts(
    declaration_index: Option<&SemanticDeclarationIndex>,
    fallback_table: &FallbackCompletionNameTable,
    reach_graph: Option<&ReachGraph>,
    include_table: Option<&IncludeCompletionTable>,
    go_import_table: Option<&GoImportCompletionTable>,
    indexed_files: Option<&[(String, PathBuf)]>,
    project_context: Option<&ProjectContextIndex>,
) -> SnapshotMemoryReport {
    let mut report = SnapshotMemoryReport {
        fallback_table_bytes: fallback_table.accounted_bytes(),
        ..SnapshotMemoryReport::default()
    };
    if let Some(index) = declaration_index {
        let (base, deltas, delta_count) = index.name_table().accounted_segment_split();
        report.name_table_bytes = index.accounted_core_bytes();
        report.name_entry_count = index.len();
        report.base_segment_bytes = base;
        report.delta_segments_bytes = deltas;
        report.delta_segment_count = delta_count;
    }
    if let Some(graph) = reach_graph {
        report.reach_graph_bytes = graph.accounted_bytes();
        report.include_edge_count = graph.include_edge_count();
    }
    if let Some(table) = include_table {
        report.include_table_bytes = table.accounted_bytes();
    }
    if let Some(table) = go_import_table {
        report.go_import_table_bytes = table.accounted_bytes();
    }
    if let Some(files) = indexed_files {
        report.file_count = files.len();
        let mut bytes = crate::memory_report::vec_bytes::<(String, PathBuf)>(files.len());
        for (relative, absolute) in files {
            bytes = bytes
                .saturating_add(relative.len())
                .saturating_add(absolute.as_os_str().len());
        }
        report.indexed_files_bytes = bytes;
    }
    if let Some(context) = project_context {
        report.project_context_bytes = context.accounted_bytes();
    }
    report
}

/// Hydrate the memory-dominant components of the read model the LSP server
/// publishes from an existing index and measure their memory footprint.
/// Backs the headless `fossilsense memory` CLI: synchronous, and on large
/// workspaces takes seconds plus hundreds of MiB by design. The include path
/// index and call read handles are not hydrated; like on the live server
/// they stay inside `process.other_bytes`, keeping the CLI and server
/// category accounting consistent.
pub(crate) fn hydrate_memory_report(
    root: &Path,
    db_path: Option<PathBuf>,
    total_budget_bytes: usize,
) -> Result<HydratedMemoryReport> {
    let db_path = match db_path {
        Some(path) => path,
        None => pathing::default_index_path(root)?,
    };
    let memory_before = current_process_memory_bytes();
    let started = Instant::now();
    let (config, _) = WorkspaceConfig::load(root);
    let project_context = project_context::discover_project_contexts(root, &config)?;
    let declarations =
        build_declaration_index_from_db(&db_path, Some(&project_context), total_budget_bytes)?;
    let fallback_table = {
        let store = IndexStore::open_readonly(&db_path)?;
        FallbackCompletionNameTable::build(store.fallback_completion_view().all()?)
    };
    let go_import_table = {
        let store = IndexStore::open_readonly(&db_path)?;
        GoImportCompletionTable::build(store.go_package_graph_view().importable_packages()?)
    };
    let reach_graph = build_reach_graph_from_db(&db_path)?;
    let include_table = build_include_table_from_db(&db_path)?;
    let indexed_files = build_indexed_file_list_from_db(&db_path, root)?;
    let hydration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let memory_after = current_process_memory_bytes();

    let snapshot = snapshot_memory_report_from_parts(
        Some(&declarations),
        &fallback_table,
        Some(&reach_graph),
        Some(&include_table),
        Some(&go_import_table),
        Some(&indexed_files),
        Some(&project_context),
    );
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let report = MemoryReport::assemble(
        std::slice::from_ref(&snapshot),
        &[DeclarationCacheSample {
            stats: declarations.payload_cache_stats(),
            budget_bytes: declarations.payload_budget_bytes(),
        }],
        OpenDocumentsMemoryReport::default(),
        memory_after,
        index_db_disk_bytes(&db_path),
        timestamp,
    );
    Ok(HydratedMemoryReport {
        declarations: declarations.len(),
        files: indexed_files.len(),
        hydration_ms,
        memory_before_bytes: memory_before,
        memory_after_bytes: memory_after,
        hydration_delta_bytes: memory_after.saturating_sub(memory_before),
        report,
    })
}

/// SQLite index file size including WAL/SHM sidecars when present.
fn index_db_disk_bytes(db_path: &Path) -> u64 {
    let mut total = 0u64;
    for suffix in ["", "-wal", "-shm"] {
        let mut name = db_path.as_os_str().to_os_string();
        name.push(suffix);
        if let Ok(metadata) = std::fs::metadata(&name) {
            total = total.saturating_add(metadata.len());
        }
    }
    total
}

async fn update_indexed_file_list(
    previous: Option<Arc<Vec<(String, PathBuf)>>>,
    root: PathBuf,
    paths: &[String],
) -> Result<Arc<Vec<(String, PathBuf)>>> {
    let Some(previous) = previous else {
        return rebuild_indexed_file_list(root).await;
    };
    let changed = paths.to_vec();
    let load_root = root.clone();
    let rows = tokio::task::spawn_blocking(move || -> Result<_> {
        let db_path = pathing::default_index_path(&load_root)?;
        let store = IndexStore::open_readonly(&db_path)?;
        store
            .reference_file_view()
            .indexed_workspace_files_for_paths(&changed)
    })
    .await??;
    let changed: HashSet<&str> = paths.iter().map(String::as_str).collect();
    let mut files: Vec<(String, PathBuf)> = previous
        .iter()
        .filter(|(path, _)| !changed.contains(path.as_str()))
        .cloned()
        .collect();
    files.extend(rows.into_iter().map(|row| {
        let absolute = root.join(row.path.replace('/', std::path::MAIN_SEPARATOR_STR));
        (row.path, absolute)
    }));
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(Arc::new(files))
}

impl CacheLedger {
    /// Hydrate the same immutable read-model components as a full publication,
    /// but from an explicit benchmark database. This deliberately exists only
    /// for production-path LSP tests: normal runtime publication must keep using
    /// the generation-leased default index path for the workspace.
    #[cfg(test)]
    pub(in crate::server) async fn publish_full_index_from_db_for_test(
        &self,
        root: PathBuf,
        db_path: PathBuf,
    ) -> Result<Arc<EngineSnapshot>> {
        let total_budget_bytes = self.semantic_index_memory_budget_bytes();
        let epoch = self.allocate_engine_epoch();
        let build_root = root.clone();
        let snapshot = tokio::task::spawn_blocking(move || -> Result<EngineSnapshot> {
            let (config, _) = crate::config::WorkspaceConfig::load(&build_root);
            let project_context = Arc::new(crate::project_context::discover_project_contexts(
                &build_root,
                &config,
            )?);
            let declaration_index = Arc::new(build_declaration_index_from_db(
                &db_path,
                Some(&project_context),
                total_budget_bytes,
            )?);
            let store = IndexStore::open_readonly(&db_path)?;
            let guard = store.begin_semantic_read(None)?;
            let semantic_generation = crate::call_model::SemanticGeneration(guard.generation());
            guard.finish()?;
            let fallback_completion_table = Arc::new(
                crate::completion::ordinary_service::FallbackCompletionNameTable::build(
                    store.fallback_completion_view().all()?,
                ),
            );
            let go_import_table = Arc::new(GoImportCompletionTable::build(
                store.go_package_graph_view().importable_packages()?,
            ));
            let reach_graph = Arc::new(build_reach_graph_from_db(&db_path)?);
            let include_table = Arc::new(build_include_table_from_db(&db_path)?);
            let indexed_files = Arc::new(build_indexed_file_list_from_db(&db_path, &build_root)?);
            let include_path_index = Arc::new(build_include_path_index_from_db(&db_path)?);
            let call_read_handle = Arc::new(crate::call_service::CallReadHandle::at_generation(
                db_path,
                semantic_generation,
            ));
            let workspace_semantics = Arc::new(
                super::super::workspace_config::PublishedWorkspaceSemantics::load_current(
                    &build_root,
                    &[],
                    &[],
                ),
            );

            Ok(EngineSnapshot {
                root: build_root,
                epoch,
                semantic_generation,
                declaration_index: Some(declaration_index.clone()),
                name_table: Some(declaration_index.name_table_arc()),
                fallback_completion_table,
                reach_graph: Some(reach_graph),
                include_table: Some(include_table),
                go_import_table: Some(go_import_table),
                indexed_files: Some(indexed_files),
                include_path_index: Some(include_path_index),
                project_context: Some(project_context),
                call_read_handle: Some(call_read_handle),
                workspace_semantics,
                degraded: DegradedCapabilities::default(),
            })
        })
        .await??;

        Ok(self.publish_engine_snapshot(snapshot).await)
    }

    #[cfg(test)]
    pub(in crate::server) async fn publish_full_index(
        &self,
        client: &Client,
        root: PathBuf,
    ) -> Result<CachePublishReport> {
        let workspace_semantics = Arc::new(
            super::super::workspace_config::PublishedWorkspaceSemantics::load_current(
                &root,
                &[],
                &[],
            ),
        );
        self.publish_full_index_with_semantics(client, root, workspace_semantics)
            .await
    }

    pub(in crate::server) async fn publish_full_index_with_semantics(
        &self,
        client: &Client,
        root: PathBuf,
        workspace_semantics: Arc<super::super::workspace_config::PublishedWorkspaceSemantics>,
    ) -> Result<CachePublishReport> {
        // SQLite has one writer and the runtime has one snapshot publisher. The
        // previous engine snapshot stays visible while every next component is
        // built off to the side.
        let _publish_guard = self.publish_gate.lock().await;
        self.publish_full_index_under_gate(client, root, workspace_semantics)
            .await
    }

    async fn publish_full_index_under_gate(
        &self,
        client: &Client,
        root: PathBuf,
        workspace_semantics: Arc<super::super::workspace_config::PublishedWorkspaceSemantics>,
    ) -> Result<CachePublishReport> {
        let semantic_generation = load_semantic_generation(root.clone()).await?;

        let nt_started = tokio::time::Instant::now();
        let project_context =
            rebuild_project_context(client, root.clone(), workspace_semantics.workspace.clone())
                .await;
        let declaration_index = rebuild_declaration_index(
            root.clone(),
            project_context.clone(),
            self.semantic_index_memory_budget_bytes(),
        )
        .await?;
        let declaration_count = declaration_index.len();
        let name_table_ms = nt_started.elapsed().as_millis();
        client
            .log_message(
                MessageType::LOG,
                format!(
                    "semantic declaration index: declarations={}, core_bytes={}, total_budget_bytes={}, payload_budget_bytes={}",
                    declaration_count,
                    declaration_index.accounted_core_bytes(),
                    declaration_index.total_budget_bytes(),
                    declaration_index.payload_budget_bytes(),
                ),
            )
            .await;
        let call_read_handle = capture_call_read_handle(&root, semantic_generation)?;
        let fallback_completion_table = rebuild_fallback_completion_table(root.clone()).await?;

        let rg_started = tokio::time::Instant::now();
        let reach_graph = rebuild_reach_graph(client, root.clone()).await;
        let mut degraded = DegradedCapabilities {
            reach_graph: reach_graph.is_none(),
            project_context: project_context.is_none(),
            call_relations: false,
            ..Default::default()
        };

        let mut include_table_error = None;
        let include_table = match rebuild_include_table(root.clone()).await {
            Ok(table) => Some(table),
            Err(err) => {
                degraded.include_table = true;
                include_table_error = Some(format!("{err:#}"));
                None
            }
        };
        let include_count = include_table.as_ref().map_or(0, |table| table.len());
        let mut go_import_table_error = None;
        let go_import_table = match rebuild_go_import_table(root.clone()).await {
            Ok(table) => Some(table),
            Err(err) => {
                degraded.go_import_table = true;
                go_import_table_error = Some(format!("{err:#}"));
                None
            }
        };

        let mut reference_file_list_error = None;
        let indexed_files = match rebuild_indexed_file_list(root.clone()).await {
            Ok(files) => Some(files),
            Err(err) => {
                degraded.reference_file_list = true;
                reference_file_list_error = Some(format!("{err:#}"));
                None
            }
        };
        let include_path_index = match rebuild_include_path_index(root.clone()).await {
            Ok(index) => Some(index),
            Err(err) => {
                degraded.reference_file_list = true;
                let error = format!("include path index: {err:#}");
                reference_file_list_error = Some(match reference_file_list_error.take() {
                    Some(previous) => format!("{previous}; {error}"),
                    None => error,
                });
                None
            }
        };
        let reference_file_count = indexed_files.as_ref().map_or(0, |files| files.len());
        let reach_graph_ms = rg_started.elapsed().as_millis();
        let observed_generation = load_semantic_generation(root.clone()).await?;
        anyhow::ensure!(
            observed_generation == semantic_generation,
            "semantic generation changed while building the engine snapshot"
        );

        let epoch = self.allocate_engine_epoch();
        self.publish_engine_snapshot(EngineSnapshot {
            root,
            epoch,
            semantic_generation,
            declaration_index: Some(declaration_index.clone()),
            name_table: Some(declaration_index.name_table_arc()),
            fallback_completion_table,
            reach_graph,
            include_table,
            go_import_table,
            indexed_files,
            include_path_index,
            project_context,
            call_read_handle: Some(call_read_handle),
            workspace_semantics,
            degraded: degraded.clone(),
        })
        .await;
        self.invalidate_after_index_change().await;

        Ok(CachePublishReport {
            semantic_generation,
            declaration_count,
            include_count,
            reference_file_count,
            name_table_ms,
            reach_graph_ms,
            degraded,
            epoch,
            include_table_error,
            go_import_table_error,
            reference_file_list_error,
        })
    }

    #[cfg(test)]
    pub(in crate::server) async fn publish_dirty_index(
        &self,
        client: &Client,
        root: PathBuf,
        rel_paths: &[String],
        include_edge_sources_rebuilt: &[String],
    ) -> Result<CachePublishReport> {
        let workspace_semantics = self
            .current_engine_snapshot(&root)
            .await
            .map(|snapshot| snapshot.workspace_semantics.clone())
            .unwrap_or_else(|| {
                Arc::new(
                    super::super::workspace_config::PublishedWorkspaceSemantics::load_current(
                        &root,
                        &[],
                        &[],
                    ),
                )
            });
        self.publish_dirty_index_with_semantics(
            client,
            root,
            rel_paths,
            include_edge_sources_rebuilt,
            workspace_semantics,
        )
        .await
    }

    pub(in crate::server) async fn publish_dirty_index_with_semantics(
        &self,
        client: &Client,
        root: PathBuf,
        rel_paths: &[String],
        include_edge_sources_rebuilt: &[String],
        workspace_semantics: Arc<super::super::workspace_config::PublishedWorkspaceSemantics>,
    ) -> Result<CachePublishReport> {
        let _publish_guard = self.publish_gate.lock().await;
        let semantic_generation = load_semantic_generation(root.clone()).await?;
        let previous = self.current_engine_snapshot(&root).await;
        let direct_base = previous.as_ref().is_some_and(|snapshot| {
            snapshot.semantic_generation != crate::call_model::SemanticGeneration::MISSING
                && snapshot.semantic_generation.0.checked_add(1) == Some(semantic_generation.0)
        });
        if !direct_base {
            return self
                .publish_full_index_under_gate(client, root, workspace_semantics)
                .await;
        }
        anyhow::ensure!(
            previous.as_ref().is_some_and(|snapshot| Arc::ptr_eq(
                &snapshot.workspace_semantics,
                &workspace_semantics
            )),
            "dirty index configuration differs from its published base; full index required"
        );
        let project_context = previous
            .as_ref()
            .and_then(|snapshot| snapshot.project_context.clone());

        let nt_started = tokio::time::Instant::now();
        let declaration_index = update_declaration_index_paths(
            previous
                .as_ref()
                .and_then(|snapshot| snapshot.declaration_index.as_deref()),
            root.clone(),
            rel_paths,
            project_context.clone(),
            self.semantic_index_memory_budget_bytes(),
        )
        .await?;
        let declaration_count = declaration_index.len();
        let name_table_ms = nt_started.elapsed().as_millis();
        let should_compact_name_index = declaration_index.needs_compaction();
        let call_read_handle = capture_call_read_handle(&root, semantic_generation)?;
        let fallback_completion_table = rebuild_fallback_completion_table(root.clone()).await?;

        let rg_started = tokio::time::Instant::now();
        let reach_graph = refresh_reach_graph_incremental(
            client,
            previous
                .as_ref()
                .and_then(|snapshot| snapshot.reach_graph.clone()),
            root.clone(),
            include_edge_sources_rebuilt,
        )
        .await;
        let mut degraded = DegradedCapabilities {
            reach_graph: reach_graph.is_none(),
            project_context: project_context.is_none(),
            call_relations: false,
            ..Default::default()
        };

        let mut include_table_error = None;
        let include_table = match rebuild_include_table(root.clone()).await {
            Ok(table) => Some(table),
            Err(err) => {
                degraded.include_table = true;
                include_table_error = Some(format!("{err:#}"));
                None
            }
        };
        let include_count = include_table.as_ref().map_or(0, |table| table.len());
        let mut go_import_table_error = None;
        let go_import_table = match rebuild_go_import_table(root.clone()).await {
            Ok(table) => Some(table),
            Err(err) => {
                degraded.go_import_table = true;
                go_import_table_error = Some(format!("{err:#}"));
                None
            }
        };

        let mut reference_file_list_error = None;
        let indexed_files = match update_indexed_file_list(
            previous
                .as_ref()
                .and_then(|snapshot| snapshot.indexed_files.clone()),
            root.clone(),
            rel_paths,
        )
        .await
        {
            Ok(files) => Some(files),
            Err(err) => {
                degraded.reference_file_list = true;
                reference_file_list_error = Some(format!("{err:#}"));
                None
            }
        };
        let include_path_index = match rebuild_include_path_index(root.clone()).await {
            Ok(index) => Some(index),
            Err(err) => {
                degraded.reference_file_list = true;
                let error = format!("include path index: {err:#}");
                reference_file_list_error = Some(match reference_file_list_error.take() {
                    Some(previous) => format!("{previous}; {error}"),
                    None => error,
                });
                None
            }
        };
        let reference_file_count = indexed_files.as_ref().map_or(0, |files| files.len());
        let reach_graph_ms = rg_started.elapsed().as_millis();
        let observed_generation = load_semantic_generation(root.clone()).await?;
        anyhow::ensure!(
            observed_generation == semantic_generation,
            "semantic generation changed while building the engine snapshot"
        );

        let epoch = self.allocate_engine_epoch();
        self.publish_engine_snapshot(EngineSnapshot {
            root: root.clone(),
            epoch,
            semantic_generation,
            declaration_index: Some(declaration_index.clone()),
            name_table: Some(declaration_index.name_table_arc()),
            fallback_completion_table,
            reach_graph,
            include_table,
            go_import_table,
            indexed_files,
            include_path_index,
            project_context,
            call_read_handle: Some(call_read_handle),
            workspace_semantics,
            degraded: degraded.clone(),
        })
        .await;
        self.invalidate_after_index_change().await;

        let report = CachePublishReport {
            semantic_generation,
            declaration_count,
            include_count,
            reference_file_count,
            name_table_ms,
            reach_graph_ms,
            degraded,
            epoch,
            include_table_error,
            go_import_table_error,
            reference_file_list_error,
        };
        drop(_publish_guard);
        if should_compact_name_index {
            self.spawn_name_index_compaction(client.clone(), root, epoch);
        }
        Ok(report)
    }

    fn spawn_name_index_compaction(
        &self,
        client: Client,
        root: PathBuf,
        expected_epoch: crate::server::state::EngineEpoch,
    ) {
        let cache = self.clone();
        tokio::spawn(async move {
            let started = tokio::time::Instant::now();
            match cache
                .compact_name_index_if_current(root.clone(), expected_epoch)
                .await
            {
                Ok(true) => {
                    client
                        .log_message(
                            MessageType::INFO,
                            format!(
                                "name index compacted for {} in {}ms",
                                root.display(),
                                started.elapsed().as_millis()
                            ),
                        )
                        .await;
                }
                Ok(false) => {}
                Err(error) => {
                    client
                        .log_message(
                            MessageType::WARNING,
                            format!("name index compaction failed: {error:#}"),
                        )
                        .await;
                }
            }
        });
    }

    pub(in crate::server) async fn compact_name_index_if_current(
        &self,
        root: PathBuf,
        expected_epoch: crate::server::state::EngineEpoch,
    ) -> Result<bool> {
        let Some(snapshot) = self.current_engine_snapshot(&root).await else {
            return Ok(false);
        };
        if snapshot.epoch != expected_epoch {
            return Ok(false);
        }
        let Some(declaration_index) = snapshot.declaration_index.clone() else {
            return Ok(false);
        };
        if !declaration_index.needs_compaction() {
            return Ok(false);
        }
        let compacted =
            tokio::task::spawn_blocking(move || Arc::new(declaration_index.compacted())).await?;

        let _publish_guard = self.publish_gate.lock().await;
        let Some(current) = self.current_engine_snapshot(&root).await else {
            return Ok(false);
        };
        if current.epoch != expected_epoch {
            return Ok(false);
        }
        self.publish_engine_snapshot(EngineSnapshot {
            root,
            epoch: self.allocate_engine_epoch(),
            semantic_generation: current.semantic_generation,
            declaration_index: Some(compacted.clone()),
            name_table: Some(compacted.name_table_arc()),
            fallback_completion_table: current.fallback_completion_table.clone(),
            reach_graph: current.reach_graph.clone(),
            include_table: current.include_table.clone(),
            go_import_table: current.go_import_table.clone(),
            indexed_files: current.indexed_files.clone(),
            include_path_index: current.include_path_index.clone(),
            project_context: current.project_context.clone(),
            call_read_handle: current.call_read_handle.clone(),
            workspace_semantics: current.workspace_semantics.clone(),
            degraded: current.degraded.clone(),
        })
        .await;
        self.clear_all_completion_memos().await;
        Ok(true)
    }

    /// Refresh build-marker ownership without re-indexing or reparsing source
    /// files. The prior snapshot remains visible until the replacement project
    /// index and tagged NameTable are both ready.
    pub(in crate::server) async fn refresh_project_context(
        &self,
        client: &Client,
        root: PathBuf,
    ) -> Result<usize> {
        let _publish_guard = self.publish_gate.lock().await;
        let previous = self
            .current_engine_snapshot(&root)
            .await
            .context("project context refresh requires a published engine snapshot")?;
        let project_context = rebuild_project_context(
            client,
            root.clone(),
            previous.workspace_semantics.workspace.clone(),
        )
        .await;
        let project_count = project_context
            .as_ref()
            .map_or(0, |index| index.projects().len());
        let previous_declaration_index = previous
            .declaration_index
            .as_ref()
            .context("project context refresh requires a published declaration index")?;
        let declaration_index =
            Arc::new(previous_declaration_index.with_project_context(project_context.as_deref()));
        let mut degraded = previous.degraded.clone();
        degraded.project_context = project_context.is_none();

        self.publish_engine_snapshot(EngineSnapshot {
            root,
            epoch: self.allocate_engine_epoch(),
            semantic_generation: previous.semantic_generation,
            declaration_index: Some(declaration_index.clone()),
            name_table: Some(declaration_index.name_table_arc()),
            fallback_completion_table: previous.fallback_completion_table.clone(),
            reach_graph: previous.reach_graph.clone(),
            include_table: previous.include_table.clone(),
            go_import_table: previous.go_import_table.clone(),
            indexed_files: previous.indexed_files.clone(),
            include_path_index: previous.include_path_index.clone(),
            project_context,
            call_read_handle: previous.call_read_handle.clone(),
            workspace_semantics: previous.workspace_semantics.clone(),
            degraded,
        })
        .await;
        self.invalidate_after_index_change().await;
        self.completion_memo.lock().await.clear();
        Ok(project_count)
    }
}

#[cfg(test)]
mod memory_tests {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::declaration_models::build_declaration_index_from_db;
    use super::{
        build_include_path_index_from_db, build_include_table_from_db,
        build_indexed_file_list_from_db, build_reach_graph_from_db,
    };
    use crate::call_model::SemanticGeneration;
    use crate::call_service::CallReadHandle;
    use crate::config::WorkspaceConfig;
    use crate::declaration_index::SemanticDeclarationIndex;
    use crate::project_context::{self, ProjectContextIndex};
    use crate::reachability::ReachGraph;
    use crate::resource::current_process_memory_bytes;
    use crate::server::IncludeCompletionTable;
    use crate::store::IndexStore;

    const MIB: u64 = 1024 * 1024;
    const SEMANTIC_INDEX_TOTAL_BUDGET_BYTES: usize = 256 * 1024 * 1024;
    const SINGLE_GENERATION_PRIVATE_LIMIT_BYTES: u64 = 384 * MIB;
    const SIDE_BY_SIDE_PEAK_PRIVATE_LIMIT_BYTES: u64 = 512 * MIB;
    const MINIMUM_UBOOT_DECLARATIONS: usize = 500_000;
    const MINIMUM_UBOOT_FILES: usize = 10_000;

    #[cfg(debug_assertions)]
    fn require_release_memory_gate() {
        panic!("the memory gate must run with cargo test --release");
    }

    #[cfg(not(debug_assertions))]
    fn require_release_memory_gate() {}

    struct HydratedEngineReadModel {
        declarations: SemanticDeclarationIndex,
        reach_graph: ReachGraph,
        include_table: IncludeCompletionTable,
        indexed_files: Vec<(String, PathBuf)>,
        include_path_index: crate::candidate_service::IncludePathIndex,
        project_context: ProjectContextIndex,
        call_read_handle: CallReadHandle,
    }

    impl HydratedEngineReadModel {
        fn build(root: &Path, db_path: &Path) -> anyhow::Result<Self> {
            let (config, _) = WorkspaceConfig::load(root);
            let project_context = project_context::discover_project_contexts(root, &config)?;
            let declarations = build_declaration_index_from_db(
                db_path,
                Some(&project_context),
                SEMANTIC_INDEX_TOTAL_BUDGET_BYTES,
            )?;
            let reach_graph = build_reach_graph_from_db(db_path)?;
            let include_table = build_include_table_from_db(db_path)?;
            let indexed_files = build_indexed_file_list_from_db(db_path, root)?;
            let include_path_index = build_include_path_index_from_db(db_path)?;
            let store = IndexStore::open_readonly(db_path)?;
            let guard = store.begin_semantic_read(None)?;
            let semantic_generation = SemanticGeneration(guard.generation());
            guard.finish()?;
            let call_read_handle =
                CallReadHandle::at_generation(db_path.to_path_buf(), semantic_generation);
            Ok(Self {
                declarations,
                reach_graph,
                include_table,
                indexed_files,
                include_path_index,
                project_context,
                call_read_handle,
            })
        }

        fn retain_all_components(&self) {
            std::hint::black_box((
                &self.declarations,
                &self.reach_graph,
                &self.include_table,
                &self.indexed_files,
                &self.include_path_index,
                &self.project_context,
                &self.call_read_handle,
            ));
        }
    }

    struct PeakMemorySampler {
        peak: Arc<AtomicU64>,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl PeakMemorySampler {
        fn start() -> Self {
            let peak = Arc::new(AtomicU64::new(current_process_memory_bytes()));
            let stop = Arc::new(AtomicBool::new(false));
            let thread = {
                let peak = peak.clone();
                let stop = stop.clone();
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        peak.fetch_max(current_process_memory_bytes(), Ordering::Relaxed);
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    peak.fetch_max(current_process_memory_bytes(), Ordering::Relaxed);
                })
            };
            Self {
                peak,
                stop,
                thread: Some(thread),
            }
        }

        fn peak_bytes(&self) -> u64 {
            self.peak.load(Ordering::Relaxed)
        }

        fn finish(mut self) -> u64 {
            self.stop.store(true, Ordering::Relaxed);
            self.thread
                .take()
                .expect("memory sampler thread")
                .join()
                .expect("memory sampler must finish before evaluating the engine hydration gate");
            self.peak_bytes()
        }
    }

    impl Drop for PeakMemorySampler {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    #[test]
    #[ignore = "U-Boot engine hydration memory gate; set FOSSILSENSE_BENCH_DB and FOSSILSENSE_BENCH_ROOT"]
    fn uboot_engine_hydration_stays_below_private_memory_gate() {
        require_release_memory_gate();
        let db_path = std::env::var_os("FOSSILSENSE_BENCH_DB")
            .map(PathBuf::from)
            .expect("set FOSSILSENSE_BENCH_DB to a current-schema U-Boot database");
        let root = std::env::var_os("FOSSILSENSE_BENCH_ROOT")
            .map(PathBuf::from)
            .expect("set FOSSILSENSE_BENCH_ROOT to the indexed U-Boot checkout");
        assert!(db_path.is_file(), "benchmark database does not exist");
        assert!(root.is_dir(), "benchmark workspace does not exist");
        let memory_before = current_process_memory_bytes();
        assert!(
            memory_before > 0,
            "process private/RSS memory collection is unavailable"
        );

        let sampler = PeakMemorySampler::start();
        let first_started = Instant::now();
        let first = HydratedEngineReadModel::build(&root, &db_path)
            .expect("hydrate first U-Boot engine read model");
        first.retain_all_components();
        let first_build_ms = first_started.elapsed().as_millis();
        let single_generation_private_bytes = current_process_memory_bytes();
        let single_generation_peak_private_bytes =
            sampler.peak_bytes().max(single_generation_private_bytes);

        assert!(
            first.declarations.len() >= MINIMUM_UBOOT_DECLARATIONS,
            "memory gate requires a full U-Boot declaration set (observed {})",
            first.declarations.len()
        );
        assert!(
            first.indexed_files.len() >= MINIMUM_UBOOT_FILES,
            "memory gate requires a full U-Boot file set (observed {})",
            first.indexed_files.len()
        );
        assert!(
            single_generation_private_bytes <= SINGLE_GENERATION_PRIVATE_LIMIT_BYTES,
            "one hydrated engine generation uses {} MiB; limit is {} MiB",
            single_generation_private_bytes / MIB,
            SINGLE_GENERATION_PRIVATE_LIMIT_BYTES / MIB
        );
        assert!(
            single_generation_peak_private_bytes <= SINGLE_GENERATION_PRIVATE_LIMIT_BYTES,
            "first-generation engine hydration peaks at {} MiB; limit is {} MiB",
            single_generation_peak_private_bytes / MIB,
            SINGLE_GENERATION_PRIVATE_LIMIT_BYTES / MIB
        );

        // Publication is side-by-side: an in-flight request may retain the
        // first immutable snapshot while the replacement is fully hydrated.
        let second_started = Instant::now();
        let second = HydratedEngineReadModel::build(&root, &db_path)
            .expect("hydrate replacement U-Boot engine read model");
        first.retain_all_components();
        second.retain_all_components();
        let second_build_ms = second_started.elapsed().as_millis();
        let two_generation_private_bytes = current_process_memory_bytes();
        let peak_private_bytes = sampler.finish();

        println!(
            "engine_hydration_declarations: {}",
            first.declarations.len()
        );
        println!("engine_hydration_files: {}", first.indexed_files.len());
        println!(
            "engine_hydration_recall_bytes: {}",
            first.declarations.accounted_core_bytes()
        );
        println!("engine_hydration_memory_before_bytes: {memory_before}");
        println!("engine_hydration_single_private_bytes: {single_generation_private_bytes}");
        println!(
            "engine_hydration_single_peak_private_bytes: {single_generation_peak_private_bytes}"
        );
        println!("engine_hydration_two_generation_private_bytes: {two_generation_private_bytes}");
        println!("engine_hydration_peak_private_bytes: {peak_private_bytes}");
        println!("engine_hydration_first_build_ms: {first_build_ms}");
        println!("engine_hydration_second_build_ms: {second_build_ms}");

        assert!(
            peak_private_bytes <= SIDE_BY_SIDE_PEAK_PRIVATE_LIMIT_BYTES,
            "side-by-side engine hydration peaks at {} MiB; hard limit is {} MiB",
            peak_private_bytes / MIB,
            SIDE_BY_SIDE_PEAK_PRIVATE_LIMIT_BYTES / MIB
        );
    }
}
