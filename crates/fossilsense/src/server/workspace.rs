use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::{Mutex, RwLock};
use tower_lsp::lsp_types::Url;

use super::include_completion::IncludeCompletionTable;
use super::state;
use super::LocalWordCache;
use crate::completion_words;
use crate::parser::FileSemanticIndex;
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
        // Hold the document version stable through publication. If didChange
        // already advanced the buffer, this older request may still use its
        // local result but must not overwrite the cache for the latest text.
        let open_docs = self.open_docs.lock().await;
        if open_docs
            .get(uri)
            .is_some_and(|(current_version, _)| *current_version == version)
        {
            self.local_word_cache
                .lock()
                .await
                .insert(uri.clone(), (version, words.clone()));
        }
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
        // Latest document revision wins. Holding `open_docs` until the cache
        // write completes makes this atomic with didChange's version update:
        // either the old result lands first and didChange clears it, or the
        // version has advanced and the old result is discarded.
        let open_docs = self.open_docs.lock().await;
        if open_docs
            .get(&uri)
            .is_some_and(|(current_version, _)| *current_version == version)
        {
            self.live_parse_cache
                .write()
                .await
                .insert(uri, (version, parsed));
        }
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
    pub(in crate::server) engine_snapshots: EngineSnapshots,
    pub(in crate::server) publish_gate: Arc<Mutex<()>>,
    next_engine_epoch: Arc<AtomicU64>,
    pub(in crate::server) reference_role_cache: Arc<references::ReferenceRoleCache>,
    pub(in crate::server) reference_search_cache: Arc<references::ReferenceSearchCache>,
    pub(in crate::server) completion_memo: Arc<Mutex<HashMap<Url, state::CompletionMemo>>>,
}

pub(in crate::server) type EngineSnapshots = Arc<Mutex<HashMap<PathBuf, Arc<EngineSnapshot>>>>;

/// Complete immutable read-model state published for one workspace. Every
/// request holds one `Arc<EngineSnapshot>` for its entire indexed read, so a
/// later index publication cannot change any component under that request.
#[derive(Clone)]
pub(in crate::server) struct EngineSnapshot {
    pub(in crate::server) root: PathBuf,
    pub(in crate::server) epoch: state::EngineEpoch,
    pub(in crate::server) name_table: Option<Arc<NameTable>>,
    pub(in crate::server) reach_graph: Option<Arc<ReachGraph>>,
    pub(in crate::server) include_table: Option<Arc<IncludeCompletionTable>>,
    pub(in crate::server) indexed_files: Option<Arc<Vec<(String, PathBuf)>>>,
    #[allow(dead_code)] // Captured now; request capability-health routing is the next phase.
    pub(in crate::server) degraded: crate::progress::DegradedCapabilities,
}

impl EngineSnapshot {
    fn empty(root: PathBuf) -> Self {
        Self {
            root,
            epoch: state::EngineEpoch::missing(),
            name_table: None,
            reach_graph: None,
            include_table: None,
            indexed_files: None,
            degraded: crate::progress::DegradedCapabilities::default(),
        }
    }
}

impl Default for CacheLedger {
    fn default() -> Self {
        Self {
            engine_snapshots: Arc::new(Mutex::new(HashMap::new())),
            publish_gate: Arc::new(Mutex::new(())),
            next_engine_epoch: Arc::new(AtomicU64::new(1)),
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
pub(super) struct RequestSettings {
    pub(super) completion_enabled: bool,
    pub(super) semantic_coloring_enabled: bool,
    pub(super) scoping_enabled: bool,
    pub(super) perf_logging_enabled: bool,
}

/// Indexed inputs and request-scoped settings captured before a feature begins.
/// Document text/revision remains owned by `DocumentStore` and will be folded
/// into this context in the next request-boundary phase.
#[derive(Clone)]
pub(super) struct RequestContext {
    pub(super) engine: Arc<EngineSnapshot>,
    pub(super) settings: RequestSettings,
}

#[derive(Clone)]
pub(super) struct CachePublishReport {
    pub(super) symbol_count: usize,
    pub(super) include_count: usize,
    pub(super) reference_file_count: usize,
    pub(super) name_table_ms: u128,
    pub(super) reach_graph_ms: u128,
    pub(super) degraded: crate::progress::DegradedCapabilities,
    pub(super) epoch: state::EngineEpoch,
    pub(super) include_table_error: Option<String>,
    pub(super) reference_file_list_error: Option<String>,
}

impl CacheLedger {
    pub(in crate::server) async fn current_engine_snapshot(
        &self,
        root: &PathBuf,
    ) -> Option<Arc<EngineSnapshot>> {
        self.engine_snapshots.lock().await.get(root).cloned()
    }

    pub(in crate::server) fn allocate_engine_epoch(&self) -> state::EngineEpoch {
        let value = self.next_engine_epoch.fetch_add(1, Ordering::Relaxed);
        state::EngineEpoch::published(value)
    }

    pub(in crate::server) async fn publish_engine_snapshot(
        &self,
        snapshot: EngineSnapshot,
    ) -> Arc<EngineSnapshot> {
        let snapshot = Arc::new(snapshot);
        self.engine_snapshots
            .lock()
            .await
            .insert(snapshot.root.clone(), snapshot.clone());
        snapshot
    }

    pub(super) async fn request_context(
        &self,
        root: PathBuf,
        settings: RequestSettings,
    ) -> RequestContext {
        let engine = self
            .current_engine_snapshot(&root)
            .await
            .unwrap_or_else(|| Arc::new(EngineSnapshot::empty(root)));
        RequestContext { engine, settings }
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
        let current = self
            .current_engine_snapshot(&root)
            .await
            .unwrap_or_else(|| Arc::new(EngineSnapshot::empty(root.clone())));
        self.publish_engine_snapshot(EngineSnapshot {
            root,
            epoch: self.allocate_engine_epoch(),
            name_table: Some(table),
            reach_graph: current.reach_graph.clone(),
            include_table: current.include_table.clone(),
            indexed_files: current.indexed_files.clone(),
            degraded: current.degraded.clone(),
        })
        .await;
    }

    #[cfg(test)]
    pub(super) async fn set_indexed_file_list_for_test(
        &self,
        root: PathBuf,
        files: Arc<Vec<(String, PathBuf)>>,
    ) {
        let current = self
            .current_engine_snapshot(&root)
            .await
            .unwrap_or_else(|| Arc::new(EngineSnapshot::empty(root.clone())));
        self.publish_engine_snapshot(EngineSnapshot {
            root,
            epoch: self.allocate_engine_epoch(),
            name_table: current.name_table.clone(),
            reach_graph: current.reach_graph.clone(),
            include_table: current.include_table.clone(),
            indexed_files: Some(files),
            degraded: current.degraded.clone(),
        })
        .await;
    }

    #[cfg(test)]
    pub(super) async fn set_reach_graph_for_test(&self, root: PathBuf, graph: Arc<ReachGraph>) {
        let current = self
            .current_engine_snapshot(&root)
            .await
            .unwrap_or_else(|| Arc::new(EngineSnapshot::empty(root.clone())));
        self.publish_engine_snapshot(EngineSnapshot {
            root,
            epoch: self.allocate_engine_epoch(),
            name_table: current.name_table.clone(),
            reach_graph: Some(graph),
            include_table: current.include_table.clone(),
            indexed_files: current.indexed_files.clone(),
            degraded: current.degraded.clone(),
        })
        .await;
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
    pub(super) async fn request_context_for_root(&self, root: PathBuf) -> RequestContext {
        self.cache
            .request_context(root, RequestSettings::default())
            .await
    }

    pub(super) async fn request_context_for_root_with_settings(
        &self,
        root: PathBuf,
        settings: RequestSettings,
    ) -> RequestContext {
        self.cache.request_context(root, settings).await
    }
}
