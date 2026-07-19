use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use tower_lsp::lsp_types::MessageType;
use tower_lsp::Client;

use crate::call_model::SemanticGeneration;
use crate::call_service::CallReadHandle;
use crate::declaration_index::SemanticDeclarationIndex;
use crate::pathing;
use crate::progress::DegradedCapabilities;
use crate::project_context::{self, ProjectContextIndex};
use crate::reachability::ReachGraph;
use crate::server::workspace::EngineSnapshot;
use crate::server::{CacheLedger, CachePublishReport, IncludeCompletionTable};
use crate::store::IndexStore;

async fn load_semantic_generation(root: PathBuf) -> Result<SemanticGeneration> {
    tokio::task::spawn_blocking(move || -> Result<SemanticGeneration> {
        let db_path = pathing::default_index_path(&root)?;
        let store = IndexStore::open_readonly(&db_path)?;
        let guard = store.begin_semantic_read(None)?;
        let generation = SemanticGeneration(guard.generation());
        guard.finish()?;
        Ok(generation)
    })
    .await?
}

/// Build the generation-scoped declaration read model from one committed
/// SQLite view. The result remains private until the complete engine snapshot
/// is atomically published.
async fn rebuild_declaration_index(
    root: PathBuf,
    project_context: Option<Arc<ProjectContextIndex>>,
    payload_budget_bytes: usize,
) -> Result<Arc<SemanticDeclarationIndex>> {
    let built = tokio::task::spawn_blocking(move || -> Result<SemanticDeclarationIndex> {
        let db_path = pathing::default_index_path(&root)?;
        let store = IndexStore::open_readonly(&db_path)?;
        let mut rows = Vec::new();
        store.declaration_view().visit_core_rows(|row| {
            rows.push(row);
            Ok(())
        })?;
        let index =
            SemanticDeclarationIndex::build(rows, project_context.as_deref(), payload_budget_bytes);
        // A deliberately large budget opts into eager payload residency. The
        // factor is conservative: core rows contain the hot identity/range
        // strings, while complete typed payloads additionally carry signature,
        // declarator shape, linkage, guard, and backing data.
        if index.should_preload_all_payloads() {
            index.preload_payloads(store.declaration_view().all()?);
        }
        Ok(index)
    })
    .await;

    match built {
        Ok(Ok(table)) => Ok(Arc::new(table)),
        Ok(Err(err)) => Err(err),
        Err(err) => Err(err.into()),
    }
}

fn capture_call_read_handle(
    root: &Path,
    generation: SemanticGeneration,
) -> Result<Arc<CallReadHandle>> {
    Ok(Arc::new(CallReadHandle::at_generation(
        pathing::default_index_path(root)?,
        generation,
    )))
}

async fn update_declaration_index_paths(
    previous: Option<&SemanticDeclarationIndex>,
    root: PathBuf,
    paths: &[String],
    project_context: Option<Arc<ProjectContextIndex>>,
    payload_budget_bytes: usize,
) -> Result<Arc<SemanticDeclarationIndex>> {
    let Some(previous) = previous else {
        return rebuild_declaration_index(root, project_context, payload_budget_bytes).await;
    };

    let paths_vec = paths.to_vec();
    let read_payloads = SemanticDeclarationIndex::budget_prefers_eager_payloads(
        payload_budget_bytes,
        previous.accounted_core_bytes(),
    );
    let query_root = root.clone();
    let built = tokio::task::spawn_blocking(
        move ||
              -> Result<(
            Vec<crate::store::views::DeclarationCoreRow>,
            Option<Vec<crate::store::views::DeclarationReadRow>>,
        )> {
            let db_path = pathing::default_index_path(&query_root)?;
            let store = IndexStore::open_readonly(&db_path)?;
            let core_rows = store.declaration_view().core_rows_for_paths(&paths_vec)?;
            let payload_rows = read_payloads
                .then(|| store.declaration_view().all())
                .transpose()?;
            Ok((core_rows, payload_rows))
        },
    )
    .await;

    let (fresh_names, payload_rows) = match built {
        Ok(Ok(rows)) => rows,
        Ok(Err(err)) => return Err(err),
        Err(err) => return Err(err.into()),
    };
    let path_set: HashSet<String> = paths.iter().cloned().collect();
    let index = previous.with_updated_paths(
        &path_set,
        fresh_names,
        project_context.as_deref(),
        payload_budget_bytes,
    );
    if index.should_preload_all_payloads() {
        index.preload_payloads(payload_rows.unwrap_or_default());
    }
    Ok(Arc::new(index))
}

async fn rebuild_project_context(
    client: &Client,
    root: PathBuf,
) -> Option<Arc<ProjectContextIndex>> {
    let build_root = root.clone();
    let built = tokio::task::spawn_blocking(move || -> Result<ProjectContextIndex> {
        let (config, _) = crate::config::WorkspaceConfig::load(&build_root);
        project_context::discover_project_contexts(&build_root, &config)
    })
    .await;

    match built {
        Ok(Ok(index)) => Some(Arc::new(index)),
        Ok(Err(err)) => {
            client
                .log_message(
                    MessageType::WARNING,
                    format!("project context discovery failed: {err:#}"),
                )
                .await;
            None
        }
        Err(err) => {
            client
                .log_message(
                    MessageType::WARNING,
                    format!("project context task failed: {err}"),
                )
                .await;
            None
        }
    }
}

async fn load_reach_graph(root: PathBuf) -> Result<Arc<ReachGraph>> {
    let built = tokio::task::spawn_blocking(move || -> Result<ReachGraph> {
        let db_path = pathing::default_index_path(&root)?;
        let store = IndexStore::open_readonly(&db_path)?;
        let reach_view = store.reach_graph_view();
        Ok(ReachGraph::from_rows(
            reach_view.include_edges()?,
            reach_view.unresolved_includes()?,
            reach_view.ambiguous_includes()?,
        ))
    })
    .await;

    match built {
        Ok(Ok(graph)) => Ok(Arc::new(graph)),
        Ok(Err(err)) => Err(err),
        Err(err) => Err(err.into()),
    }
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
        let store = IndexStore::open_readonly(&db_path)?;
        Ok(IncludeCompletionTable::build_from_rows(
            store.include_table_view().workspace_paths()?,
            store.reach_graph_view().include_edges()?,
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
        let store = IndexStore::open_readonly(&db_path)?;
        Ok(store
            .reference_file_view()
            .indexed_workspace_files()?
            .into_iter()
            .map(|row| {
                let abs = build_root.join(row.path.replace('/', std::path::MAIN_SEPARATOR_STR));
                (row.path, abs)
            })
            .collect())
    })
    .await;

    match built {
        Ok(Ok(files)) => Ok(Arc::new(files)),
        Ok(Err(err)) => Err(err),
        Err(err) => Err(err.into()),
    }
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

pub(in crate::server) fn ready_cache_message(
    prefix: &str,
    symbol_count: usize,
    include_count: usize,
    ref_file_count: usize,
    name_table_ms: u128,
    reach_graph_ms: u128,
    degraded: &DegradedCapabilities,
) -> String {
    let mut message = format!(
        "{prefix}: {symbol_count} symbols, include table={include_count} paths, reference files={ref_file_count} (name_table={name_table_ms}ms, reach_graph={reach_graph_ms}ms)"
    );
    if degraded.any() {
        message.push_str("; degraded=");
        message.push_str(&degraded.labels().join(","));
    }
    message
}

impl CacheLedger {
    pub(in crate::server) async fn publish_full_index(
        &self,
        client: &Client,
        root: PathBuf,
    ) -> Result<CachePublishReport> {
        // SQLite has one writer and the runtime has one snapshot publisher. The
        // previous engine snapshot stays visible while every next component is
        // built off to the side.
        let _publish_guard = self.publish_gate.lock().await;
        let semantic_generation = load_semantic_generation(root.clone()).await?;

        let nt_started = tokio::time::Instant::now();
        let project_context = rebuild_project_context(client, root.clone()).await;
        let declaration_index = rebuild_declaration_index(
            root.clone(),
            project_context.clone(),
            self.semantic_index_memory_budget_bytes(),
        )
        .await?;
        let symbol_count = declaration_index.len();
        let name_table_ms = nt_started.elapsed().as_millis();
        client
            .log_message(
                MessageType::LOG,
                format!(
                    "semantic declaration index: declarations={}, core_bytes={}, payload_budget_bytes={}",
                    symbol_count,
                    declaration_index.accounted_core_bytes(),
                    self.semantic_index_memory_budget_bytes(),
                ),
            )
            .await;
        let call_read_handle = capture_call_read_handle(&root, semantic_generation)?;

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

        let mut reference_file_list_error = None;
        let indexed_files = match rebuild_indexed_file_list(root.clone()).await {
            Ok(files) => Some(files),
            Err(err) => {
                degraded.reference_file_list = true;
                reference_file_list_error = Some(format!("{err:#}"));
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
            reach_graph,
            include_table,
            indexed_files,
            project_context,
            call_read_handle: Some(call_read_handle),
            degraded: degraded.clone(),
        })
        .await;
        self.invalidate_after_index_change().await;

        Ok(CachePublishReport {
            semantic_generation,
            symbol_count,
            include_count,
            reference_file_count,
            name_table_ms,
            reach_graph_ms,
            degraded,
            epoch,
            include_table_error,
            reference_file_list_error,
        })
    }

    pub(in crate::server) async fn publish_dirty_index(
        &self,
        client: &Client,
        root: PathBuf,
        rel_paths: &[String],
        include_edge_sources_rebuilt: &[String],
    ) -> Result<CachePublishReport> {
        let _publish_guard = self.publish_gate.lock().await;
        let semantic_generation = load_semantic_generation(root.clone()).await?;
        let previous = self.current_engine_snapshot(&root).await;
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
        let symbol_count = declaration_index.len();
        let name_table_ms = nt_started.elapsed().as_millis();
        let should_compact_name_index = declaration_index.needs_compaction();
        let call_read_handle = capture_call_read_handle(&root, semantic_generation)?;

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
            reach_graph,
            include_table,
            indexed_files,
            project_context,
            call_read_handle: Some(call_read_handle),
            degraded: degraded.clone(),
        })
        .await;
        self.invalidate_after_index_change().await;

        let report = CachePublishReport {
            semantic_generation,
            symbol_count,
            include_count,
            reference_file_count,
            name_table_ms,
            reach_graph_ms,
            degraded,
            epoch,
            include_table_error,
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
            reach_graph: current.reach_graph.clone(),
            include_table: current.include_table.clone(),
            indexed_files: current.indexed_files.clone(),
            project_context: current.project_context.clone(),
            call_read_handle: current.call_read_handle.clone(),
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
        let project_context = rebuild_project_context(client, root.clone()).await;
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
            reach_graph: previous.reach_graph.clone(),
            include_table: previous.include_table.clone(),
            indexed_files: previous.indexed_files.clone(),
            project_context,
            call_read_handle: previous.call_read_handle.clone(),
            degraded,
        })
        .await;
        self.invalidate_after_index_change().await;
        self.completion_memo.lock().await.clear();
        Ok(project_count)
    }
}
