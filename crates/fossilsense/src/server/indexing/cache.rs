use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use tower_lsp::lsp_types::MessageType;
use tower_lsp::Client;

use crate::call_model::SemanticGeneration;
use crate::call_service::CallReadHandle;
use crate::pathing;
use crate::progress::DegradedCapabilities;
use crate::project_context::{self, ProjectContextIndex};
use crate::query::NameTable;
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

/// Build the in-memory fuzzy name table for `root` from one committed SQLite
/// view. The result remains private until the complete engine snapshot is
/// atomically published.
async fn rebuild_name_table(
    root: PathBuf,
    project_context: Option<Arc<ProjectContextIndex>>,
) -> Result<Arc<NameTable>> {
    let built = tokio::task::spawn_blocking(move || -> Result<NameTable> {
        let db_path = pathing::default_index_path(&root)?;
        let store = IndexStore::open_readonly(&db_path)?;
        Ok(NameTable::build_from_rows_with_project_context(
            store.name_table_view().symbol_rows()?,
            project_context.as_deref(),
        ))
    })
    .await;

    match built {
        Ok(Ok(table)) => Ok(Arc::new(table)),
        Ok(Err(err)) => Err(err),
        Err(err) => Err(err.into()),
    }
}

fn capture_call_read_handle(
    root: &PathBuf,
    generation: SemanticGeneration,
) -> Result<Arc<CallReadHandle>> {
    Ok(Arc::new(CallReadHandle::at_generation(
        pathing::default_index_path(root)?,
        generation,
    )))
}

async fn update_name_table_paths(
    previous: Option<&NameTable>,
    root: PathBuf,
    paths: &[String],
    project_context: Option<Arc<ProjectContextIndex>>,
) -> Result<Arc<NameTable>> {
    let Some(previous) = previous else {
        return rebuild_name_table(root, project_context).await;
    };

    let paths_vec = paths.to_vec();
    let built = tokio::task::spawn_blocking(
        move || -> Result<Vec<crate::store::views::NameTableSymbolRow>> {
            let db_path = pathing::default_index_path(&root)?;
            let store = IndexStore::open_readonly(&db_path)?;
            store.name_table_view().symbol_rows_for_paths(&paths_vec)
        },
    )
    .await;

    let fresh_names = match built {
        Ok(Ok(names)) => names,
        Ok(Err(err)) => return Err(err),
        Err(err) => return Err(err.into()),
    };
    let path_set: HashSet<String> = paths.iter().cloned().collect();
    Ok(Arc::new(
        previous.with_updated_path_rows_with_project_context(
            &path_set,
            fresh_names,
            project_context.as_deref(),
        ),
    ))
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
        let name_table = rebuild_name_table(root.clone(), project_context.clone()).await?;
        let symbol_count = name_table.len();
        let name_table_ms = nt_started.elapsed().as_millis();
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
            name_table: Some(name_table),
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
        let name_table = update_name_table_paths(
            previous
                .as_ref()
                .and_then(|snapshot| snapshot.name_table.as_deref()),
            root.clone(),
            rel_paths,
            project_context.clone(),
        )
        .await?;
        let symbol_count = name_table.len();
        let name_table_ms = nt_started.elapsed().as_millis();
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
            root,
            epoch,
            semantic_generation,
            name_table: Some(name_table),
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
        let previous_name_table = previous
            .name_table
            .as_ref()
            .context("project context refresh requires a published name table")?;
        let name_table =
            Arc::new(previous_name_table.with_project_context(project_context.as_deref()));
        let mut degraded = previous.degraded.clone();
        degraded.project_context = project_context.is_none();

        self.publish_engine_snapshot(EngineSnapshot {
            root,
            epoch: self.allocate_engine_epoch(),
            semantic_generation: previous.semantic_generation,
            name_table: Some(name_table),
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
