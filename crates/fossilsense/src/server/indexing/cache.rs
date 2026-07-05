use super::*;

/// Rebuild the in-memory fuzzy name table for `root` from its SQLite index.
pub(super) async fn rebuild_name_table(name_tables: &NameTables, root: PathBuf) -> Result<usize> {
    let build_root = root.clone();
    let built = tokio::task::spawn_blocking(move || -> Result<NameTable> {
        let db_path = pathing::default_index_path(&build_root)?;
        let store = IndexStore::open_readonly(&db_path)?;
        Ok(NameTable::build_with_paths(
            store.load_symbol_names_with_paths()?,
        ))
    })
    .await;

    match built {
        Ok(Ok(table)) => {
            let count = table.len();
            name_tables.lock().await.insert(root, Arc::new(table));
            Ok(count)
        }
        Ok(Err(err)) => Err(err),
        Err(err) => Err(err.into()),
    }
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

/// Attempt incremental reach graph refresh after a dirty update. If the existing
/// graph is absent, the store load fails, or the refresh cannot be applied, falls
/// back to a full rebuild.
pub(super) async fn refresh_reach_graph_incremental(
    client: &Client,
    reach_graphs: &ReachGraphs,
    root: PathBuf,
    source_paths: &[String],
) -> bool {
    if source_paths.is_empty() {
        if reach_graphs.lock().await.contains_key(&root) {
            return true;
        }
        return rebuild_reach_graph(client, reach_graphs, root).await;
    }

    let source_vec = source_paths.to_vec();
    let source_vec_clone = source_vec.clone();
    let root_clone = root.clone();

    let loaded = {
        let db_root = root_clone.clone();
        tokio::task::spawn_blocking(move || -> Result<_> {
            let db_path = crate::pathing::default_index_path(&db_root)?;
            let store = IndexStore::open_readonly(&db_path)?;
            store.load_include_data_for_sources(&source_vec_clone)
        })
        .await
    };

    match loaded {
        Ok(Ok((edges, open))) => {
            let existing = reach_graphs.lock().await.get(&root).cloned();
            let refreshed = existing.is_some_and(|graph| match graph.write() {
                Ok(mut graph) => {
                    graph.refresh_sources(&source_vec, edges, open);
                    true
                }
                Err(_) => false,
            });
            if !refreshed {
                return rebuild_reach_graph(client, reach_graphs, root).await;
            }
            client
                .log_message(
                    MessageType::INFO,
                    format!(
                        "reach graph incrementally refreshed for {} sources",
                        source_vec.len()
                    ),
                )
                .await;
            true
        }
        _ => {
            // Fall back to full rebuild on any error.
            client
                .log_message(
                    MessageType::INFO,
                    "reach graph refresh unavailable, falling back to full rebuild".to_string(),
                )
                .await;
            rebuild_reach_graph(client, reach_graphs, root).await
        }
    }
}

/// Rebuild the in-memory include reachability graph for `root` from its SQLite
/// index. A fresh graph instance is the new "generation", so previously memoized
/// reachable sets are discarded by replacing the stored `Arc`. A failure here is
/// non-fatal: scoping simply stays absent (whole-index fallback) and is logged.
pub(super) async fn rebuild_reach_graph(
    client: &Client,
    reach_graphs: &ReachGraphs,
    root: PathBuf,
) -> bool {
    let build_root = root.clone();
    let built = tokio::task::spawn_blocking(move || -> Result<ReachGraph> {
        let db_path = pathing::default_index_path(&build_root)?;
        let store = IndexStore::open_readonly(&db_path)?;
        Ok(ReachGraph::new(
            store.load_include_edge_paths()?,
            store.open_include_file_paths()?,
            store.ambiguous_include_file_paths()?,
        ))
    })
    .await;

    match built {
        Ok(Ok(graph)) => {
            reach_graphs
                .lock()
                .await
                .insert(root, Arc::new(StdRwLock::new(graph)));
            true
        }
        Ok(Err(err)) => {
            reach_graphs.lock().await.remove(&root);
            client
                .log_message(
                    MessageType::WARNING,
                    format!("reachability graph build failed: {err:#}"),
                )
                .await;
            false
        }
        Err(err) => {
            reach_graphs.lock().await.remove(&root);
            client
                .log_message(
                    MessageType::WARNING,
                    format!("reachability graph task failed: {err}"),
                )
                .await;
            false
        }
    }
}

pub(super) async fn update_name_table_paths(
    name_tables: &NameTables,
    root: PathBuf,
    paths: &[String],
) -> Result<usize> {
    let build_root = root.clone();
    let paths_vec = paths.to_vec();
    #[allow(clippy::type_complexity)]
    let loaded = tokio::task::spawn_blocking(
        move || -> Result<Vec<(i64, String, bool, String, String, bool)>> {
            let db_path = pathing::default_index_path(&build_root)?;
            let store = IndexStore::open_readonly(&db_path)?;
            store.load_symbol_names_for_paths(&paths_vec)
        },
    )
    .await;

    let fresh_names = match loaded {
        Ok(Ok(names)) => names,
        Ok(Err(err)) => return Err(err),
        Err(err) => return Err(err.into()),
    };

    let path_set: HashSet<String> = paths.iter().cloned().collect();
    let mut tables = name_tables.lock().await;
    let updated = match tables.get(&root) {
        Some(existing) => existing.with_updated_paths(&path_set, fresh_names),
        None => {
            drop(tables);
            return rebuild_name_table(name_tables, root).await;
        }
    };
    let count = updated.len();
    tables.insert(root, Arc::new(updated));
    Ok(count)
}

pub(in crate::server) async fn rebuild_include_table(
    include_tables: &IncludeTables,
    root: PathBuf,
) -> Result<usize> {
    let build_root = root.clone();
    let built = tokio::task::spawn_blocking(move || -> Result<IncludeCompletionTable> {
        let db_path = pathing::default_index_path(&build_root)?;
        let store = IndexStore::open_readonly(&db_path)?;
        Ok(IncludeCompletionTable::build_with_edges(
            store.workspace_file_paths()?,
            store.load_include_edge_paths()?,
        ))
    })
    .await;

    match built {
        Ok(Ok(table)) => {
            let count = table.len();
            include_tables.lock().await.insert(root, Arc::new(table));
            Ok(count)
        }
        Ok(Err(err)) => {
            include_tables.lock().await.remove(&root);
            Err(err)
        }
        Err(err) => {
            include_tables.lock().await.remove(&root);
            Err(err.into())
        }
    }
}

pub(in crate::server) async fn rebuild_indexed_file_list(
    indexed_file_lists: &IndexedFileLists,
    root: PathBuf,
) -> Result<usize> {
    let build_root = root.clone();
    let built = tokio::task::spawn_blocking(move || -> Result<Vec<(String, PathBuf)>> {
        let db_path = pathing::default_index_path(&build_root)?;
        let store = IndexStore::open_readonly(&db_path)?;
        Ok(store
            .indexed_workspace_files()?
            .into_iter()
            .map(|rel| {
                let abs = build_root.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
                (rel, abs)
            })
            .collect())
    })
    .await;

    match built {
        Ok(Ok(files)) => {
            let count = files.len();
            indexed_file_lists
                .lock()
                .await
                .insert(root, Arc::new(files));
            Ok(count)
        }
        Ok(Err(err)) => {
            indexed_file_lists.lock().await.remove(&root);
            Err(err)
        }
        Err(err) => {
            indexed_file_lists.lock().await.remove(&root);
            Err(err.into())
        }
    }
}

pub(super) async fn refresh_workspace_generation(
    index_generations: &IndexGenerations,
    name_tables: &NameTables,
    reach_graphs: &ReachGraphs,
    include_tables: &IncludeTables,
    indexed_file_lists: &IndexedFileLists,
    root: PathBuf,
) -> state::WorkspaceGeneration {
    let name_table = name_tables.lock().await.get(&root).cloned();
    let reach_graph = reach_graphs.lock().await.get(&root).cloned();
    let include_table = include_tables.lock().await.get(&root).cloned();
    let indexed_file_list = indexed_file_lists.lock().await.get(&root).cloned();

    let generation = state::workspace_generation_for_parts(
        &root,
        state::WorkspaceGenerationParts {
            name_table: name_table.map(|table| Arc::as_ptr(&table) as usize),
            reach_graph: reach_graph.map(|graph| Arc::as_ptr(&graph) as usize),
            include_table: include_table.map(|table| Arc::as_ptr(&table) as usize),
            indexed_file_list: indexed_file_list.map(|files| Arc::as_ptr(&files) as usize),
        },
    );
    index_generations.lock().await.insert(root, generation);
    generation
}
