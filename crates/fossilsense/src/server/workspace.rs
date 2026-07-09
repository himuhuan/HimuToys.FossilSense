use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, RwLock as StdRwLock};

use tokio::sync::{Mutex, RwLock};
use tower_lsp::lsp_types::Url;

use super::include_completion::IncludeCompletionTable;
use super::state;
use super::{IndexGenerations, IndexedFileLists, LocalWordCache, NameTables, ReachGraphs};
use crate::completion_words;
use crate::parser::FileSemanticIndex;
use crate::project_context::ProjectContextIndex;
use crate::query::NameTable;
use crate::reachability::ReachGraph;
use crate::references;

type LiveParseCache = Arc<RwLock<HashMap<Url, (i32, Arc<FileSemanticIndex>)>>>;

#[derive(Clone, Default)]
pub(super) struct DocumentStore {
    pub(in crate::server) open_docs: Arc<Mutex<HashMap<Url, (i32, String)>>>,
    pub(in crate::server) live_parse_cache: LiveParseCache,
    pub(in crate::server) local_word_cache: LocalWordCache,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DocumentSnapshot {
    pub(super) version: i32,
    pub(super) text: String,
}

impl DocumentStore {
    pub(super) async fn open_document(&self, uri: Url, version: i32, text: String) {
        self.open_docs.lock().await.insert(uri, (version, text));
    }

    pub(super) async fn change_document(&self, uri: Url, version: i32, text: String) {
        self.open_docs
            .lock()
            .await
            .insert(uri.clone(), (version, text));
        self.clear_live_state(&uri).await;
    }

    pub(super) async fn close_document(&self, uri: &Url) {
        self.open_docs.lock().await.remove(uri);
        self.clear_live_state(uri).await;
    }

    pub(super) async fn clear_live_state(&self, uri: &Url) {
        self.live_parse_cache.write().await.remove(uri);
        self.local_word_cache.lock().await.remove(uri);
    }

    pub(super) async fn snapshot(&self, uri: &Url) -> Option<DocumentSnapshot> {
        self.open_docs
            .lock()
            .await
            .get(uri)
            .map(|(version, text)| DocumentSnapshot {
                version: *version,
                text: text.clone(),
            })
    }

    pub(super) async fn local_words_for(
        &self,
        uri: &Url,
        version: i32,
        text: &str,
    ) -> Arc<HashSet<String>> {
        {
            let cache_guard = self.local_word_cache.lock().await;
            if let Some((cached_version, words)) = cache_guard.get(uri) {
                if *cached_version == version {
                    return words.clone();
                }
            }
        }

        let words = Arc::new(completion_words::extract_words(text));
        self.local_word_cache
            .lock()
            .await
            .insert(uri.clone(), (version, words.clone()));
        words
    }

    pub(super) async fn cached_live_parse(
        &self,
        uri: &Url,
        version: i32,
    ) -> Option<Arc<FileSemanticIndex>> {
        let cache = self.live_parse_cache.read().await;
        cache.get(uri).and_then(|(cached_version, parsed)| {
            (*cached_version == version).then(|| parsed.clone())
        })
    }

    pub(super) async fn store_live_parse(
        &self,
        uri: Url,
        version: i32,
        parsed: Arc<FileSemanticIndex>,
    ) {
        self.live_parse_cache
            .write()
            .await
            .insert(uri, (version, parsed));
    }

    #[cfg(test)]
    pub(super) async fn store_live_parse_for_test(
        &self,
        uri: Url,
        version: i32,
        parsed: Arc<FileSemanticIndex>,
    ) {
        self.store_live_parse(uri, version, parsed).await;
    }

    #[cfg(test)]
    pub(super) async fn live_parse_for_test(&self, uri: &Url) -> Option<Arc<FileSemanticIndex>> {
        self.live_parse_cache
            .read()
            .await
            .get(uri)
            .map(|(_, parsed)| parsed.clone())
    }

    #[cfg(test)]
    pub(super) async fn local_word_cache_entry_for_test(
        &self,
        uri: &Url,
    ) -> Option<(i32, Arc<HashSet<String>>)> {
        self.local_word_cache.lock().await.get(uri).cloned()
    }
}

#[derive(Clone)]
pub(super) struct CacheLedger {
    pub(in crate::server) name_tables: NameTables,
    pub(in crate::server) reach_graphs: ReachGraphs,
    pub(in crate::server) include_tables: super::IncludeTables,
    pub(in crate::server) project_context_indexes: super::ProjectContextIndexes,
    pub(in crate::server) indexed_file_lists: IndexedFileLists,
    pub(in crate::server) index_generations: IndexGenerations,
    pub(in crate::server) read_model_snapshots: ReadModelSnapshots,
    pub(in crate::server) reference_role_cache: Arc<references::ReferenceRoleCache>,
    pub(in crate::server) reference_search_cache: Arc<references::ReferenceSearchCache>,
    pub(in crate::server) completion_memo: Arc<Mutex<HashMap<Url, state::CompletionMemo>>>,
}

pub(in crate::server) type ReadModelSnapshots = Arc<Mutex<HashMap<PathBuf, WorkspaceReadModels>>>;

#[derive(Clone)]
pub(in crate::server) struct WorkspaceReadModels {
    pub(in crate::server) generation: state::WorkspaceGeneration,
    pub(in crate::server) name_table: Option<Arc<NameTable>>,
    pub(in crate::server) reach_graph: Option<Arc<StdRwLock<ReachGraph>>>,
    pub(in crate::server) include_table: Option<Arc<IncludeCompletionTable>>,
    pub(in crate::server) project_context: Option<Arc<ProjectContextIndex>>,
    pub(in crate::server) indexed_files: Option<Arc<Vec<(String, PathBuf)>>>,
}

impl Default for CacheLedger {
    fn default() -> Self {
        Self {
            name_tables: Arc::new(Mutex::new(HashMap::new())),
            reach_graphs: Arc::new(Mutex::new(HashMap::new())),
            include_tables: Arc::new(Mutex::new(HashMap::new())),
            project_context_indexes: Arc::new(Mutex::new(HashMap::new())),
            indexed_file_lists: Arc::new(Mutex::new(HashMap::new())),
            index_generations: Arc::new(Mutex::new(HashMap::new())),
            read_model_snapshots: Arc::new(Mutex::new(HashMap::new())),
            reference_role_cache: Arc::new(references::ReferenceRoleCache::new()),
            reference_search_cache: Arc::new(references::ReferenceSearchCache::new()),
            completion_memo: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

pub(super) struct CompletionMemoLookup {
    pub(super) prior_pools: Vec<Option<Vec<usize>>>,
    pub(super) hit_kind: &'static str,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct WorkspaceSnapshotSettings {
    pub(super) completion_enabled: bool,
    pub(super) semantic_coloring_enabled: bool,
    pub(super) scoping_enabled: bool,
    pub(super) perf_logging_enabled: bool,
}

#[derive(Clone)]
pub(super) struct WorkspaceSnapshot {
    pub(super) root: PathBuf,
    pub(super) generation: state::WorkspaceGeneration,
    pub(super) settings: WorkspaceSnapshotSettings,
    pub(super) name_table: Option<Arc<NameTable>>,
    pub(super) reach_graph: Option<Arc<StdRwLock<ReachGraph>>>,
    pub(super) include_table: Option<Arc<IncludeCompletionTable>>,
    pub(super) project_context: Option<Arc<ProjectContextIndex>>,
    pub(super) indexed_files: Option<Arc<Vec<(String, PathBuf)>>>,
}

#[derive(Clone)]
pub(super) struct CachePublishReport {
    pub(super) symbol_count: usize,
    pub(super) include_count: usize,
    pub(super) reference_file_count: usize,
    pub(super) name_table_ms: u128,
    pub(super) reach_graph_ms: u128,
    pub(super) degraded: crate::progress::DegradedCapabilities,
    pub(super) generation: state::WorkspaceGeneration,
    pub(super) include_table_error: Option<String>,
    pub(super) reference_file_list_error: Option<String>,
}

impl CacheLedger {
    pub(super) async fn name_table(&self, root: &PathBuf) -> Option<Arc<NameTable>> {
        self.name_tables.lock().await.get(root).cloned()
    }

    pub(super) async fn reach_graph(&self, root: &PathBuf) -> Option<Arc<StdRwLock<ReachGraph>>> {
        self.reach_graphs.lock().await.get(root).cloned()
    }

    pub(super) async fn include_table(
        &self,
        root: &PathBuf,
    ) -> Option<Arc<IncludeCompletionTable>> {
        self.include_tables.lock().await.get(root).cloned()
    }

    pub(super) async fn indexed_file_list(
        &self,
        root: &PathBuf,
    ) -> Option<Arc<Vec<(String, PathBuf)>>> {
        self.indexed_file_lists.lock().await.get(root).cloned()
    }

    pub(super) async fn project_context(&self, root: &PathBuf) -> Option<Arc<ProjectContextIndex>> {
        self.project_context_indexes.lock().await.get(root).cloned()
    }

    pub(super) async fn generation(&self, root: &PathBuf) -> state::WorkspaceGeneration {
        self.index_generations
            .lock()
            .await
            .get(root)
            .copied()
            .unwrap_or_else(state::WorkspaceGeneration::missing)
    }

    pub(super) async fn snapshot(
        &self,
        root: PathBuf,
        settings: WorkspaceSnapshotSettings,
    ) -> WorkspaceSnapshot {
        if let Some(models) = self.read_model_snapshots.lock().await.get(&root).cloned() {
            return WorkspaceSnapshot {
                generation: models.generation,
                name_table: models.name_table,
                reach_graph: models.reach_graph,
                include_table: models.include_table,
                project_context: models.project_context,
                indexed_files: models.indexed_files,
                root,
                settings,
            };
        }

        WorkspaceSnapshot {
            generation: self.generation(&root).await,
            name_table: self.name_table(&root).await,
            reach_graph: self.reach_graph(&root).await,
            include_table: self.include_table(&root).await,
            project_context: self.project_context(&root).await,
            indexed_files: self.indexed_file_list(&root).await,
            root,
            settings,
        }
    }

    pub(super) fn invalidate_references(&self) {
        self.reference_search_cache.clear();
    }

    pub(super) fn invalidate_after_index_change(&self) {
        self.invalidate_references();
    }

    pub(super) async fn clear_completion_memo(&self, uri: &Url) {
        self.completion_memo.lock().await.remove(uri);
    }

    pub(super) async fn clear_all_completion_memos(&self) {
        self.completion_memo.lock().await.clear();
    }

    pub(super) async fn record_completion_memo(
        &self,
        uri: Url,
        prefix: String,
        generation: u64,
        pools: Vec<Vec<usize>>,
    ) {
        self.completion_memo.lock().await.insert(
            uri,
            state::CompletionMemo {
                prefix,
                generation,
                pools,
            },
        );
    }

    pub(super) async fn completion_memo_pools(
        &self,
        uri: &Url,
        generation: u64,
        prefix: &str,
        table_count: usize,
    ) -> CompletionMemoLookup {
        let memo = self.completion_memo.lock().await;
        match memo.get(uri) {
            Some(m)
                if state::completion_memo_is_valid(m.generation, generation, &m.prefix, prefix)
                    && prefix == m.prefix =>
            {
                CompletionMemoLookup {
                    prior_pools: m.pools.iter().cloned().map(Some).collect(),
                    hit_kind: "hot",
                }
            }
            Some(m)
                if state::completion_memo_is_valid(m.generation, generation, &m.prefix, prefix) =>
            {
                CompletionMemoLookup {
                    prior_pools: m.pools.iter().cloned().map(Some).collect(),
                    hit_kind: "pool",
                }
            }
            Some(_) | None => CompletionMemoLookup {
                prior_pools: vec![None; table_count],
                hit_kind: "cold",
            },
        }
    }

    #[cfg(test)]
    pub(super) async fn completion_memo_for_test(
        &self,
        uri: &Url,
    ) -> Option<state::CompletionMemo> {
        self.completion_memo.lock().await.get(uri).cloned()
    }

    #[cfg(test)]
    pub(super) async fn set_name_table_for_test(&self, root: PathBuf, table: Arc<NameTable>) {
        self.name_tables.lock().await.insert(root, table);
    }

    #[cfg(test)]
    pub(super) async fn set_indexed_file_list_for_test(
        &self,
        root: PathBuf,
        files: Arc<Vec<(String, PathBuf)>>,
    ) {
        self.indexed_file_lists.lock().await.insert(root, files);
    }

    #[cfg(test)]
    pub(super) fn mark_reference_search_cache_for_test(
        &self,
        root: &str,
        identifier: &str,
        generation: u64,
    ) {
        self.reference_search_cache
            .put_empty_for_test(root, identifier, generation);
    }

    #[cfg(test)]
    pub(super) fn reference_search_cache_len_for_test(&self) -> usize {
        self.reference_search_cache.len_for_test()
    }
}

#[derive(Clone)]
pub(super) struct WorkspaceSession {
    pub(super) documents: DocumentStore,
    pub(super) cache: CacheLedger,
}

impl WorkspaceSession {
    pub(super) fn new(documents: DocumentStore, cache: CacheLedger) -> Self {
        Self { documents, cache }
    }

    pub(super) async fn open_document(&self, uri: Url, version: i32, text: String) {
        self.documents.open_document(uri, version, text).await;
    }

    pub(super) async fn change_document(&self, uri: Url, version: i32, text: String) {
        self.documents
            .change_document(uri.clone(), version, text)
            .await;
        self.cache.clear_completion_memo(&uri).await;
        self.cache.invalidate_references();
    }

    pub(super) async fn close_document(&self, uri: &Url) {
        self.documents.close_document(uri).await;
        self.cache.clear_completion_memo(uri).await;
    }

    #[cfg(test)]
    pub(super) async fn snapshot_for_root(&self, root: PathBuf) -> WorkspaceSnapshot {
        self.cache
            .snapshot(root, WorkspaceSnapshotSettings::default())
            .await
    }

    pub(super) async fn snapshot_for_root_with_settings(
        &self,
        root: PathBuf,
        settings: WorkspaceSnapshotSettings,
    ) -> WorkspaceSnapshot {
        self.cache.snapshot(root, settings).await
    }
}
