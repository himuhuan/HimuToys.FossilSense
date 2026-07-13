use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::{Mutex, RwLock};
use tower_lsp::lsp_types::{Position, TextDocumentContentChangeEvent, Url};

use super::include_completion::IncludeCompletionTable;
use super::state;
use super::LocalWordCache;
use crate::call_model::SemanticGeneration;
use crate::call_service::CallReadHandle;
use crate::completion_words;
use crate::parser::{FileSemanticIndex, ParseFacts};
use crate::pathing;
use crate::project_context::ProjectContextIndex;
use crate::query::NameTable;
use crate::reachability::ReachGraph;
use crate::references;
use crate::store::IndexStore;

type LiveParseCache = Arc<RwLock<HashMap<Url, (i32, ParseFacts, Arc<FileSemanticIndex>)>>>;
type LiveParseGates = Arc<Mutex<HashMap<Url, Arc<Mutex<()>>>>>;
type LiveParseCancellations = Arc<Mutex<HashMap<Url, (i32, Arc<AtomicBool>)>>>;

#[derive(Clone, Debug, PartialEq, Eq)]
enum RelationOverlayState {
    Clean,
    Unsaved,
    SavedAwaitingContentHash(String),
}

#[derive(Clone, Debug)]
struct OpenDocument {
    version: i32,
    text: Arc<str>,
    relation_overlay: RelationOverlayState,
}

#[derive(Clone, Default)]
pub(super) struct DocumentStore {
    open_docs: Arc<Mutex<HashMap<Url, OpenDocument>>>,
    pub(in crate::server) live_parse_cache: LiveParseCache,
    live_parse_gates: LiveParseGates,
    live_parse_cancellations: LiveParseCancellations,
    pub(in crate::server) local_word_cache: LocalWordCache,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DocumentSnapshot {
    pub(super) version: i32,
    pub(super) text: Arc<str>,
    relation_overlay: RelationOverlayState,
}

impl DocumentSnapshot {
    pub(super) fn needs_relation_overlay(&self, _generation: SemanticGeneration) -> bool {
        match &self.relation_overlay {
            RelationOverlayState::Clean => false,
            RelationOverlayState::Unsaved => true,
            RelationOverlayState::SavedAwaitingContentHash(_) => true,
        }
    }
}

impl DocumentStore {
    pub(super) async fn open_document(&self, uri: Url, version: i32, text: String) {
        let relation_overlay = relation_overlay_state_on_open(&uri, &text).await;
        self.open_docs.lock().await.insert(
            uri,
            OpenDocument {
                version,
                text: Arc::from(text),
                relation_overlay,
            },
        );
    }

    #[cfg(test)]
    pub(super) async fn change_document(&self, uri: Url, version: i32, text: String) {
        self.open_docs.lock().await.insert(
            uri.clone(),
            OpenDocument {
                version,
                text: Arc::from(text),
                relation_overlay: RelationOverlayState::Unsaved,
            },
        );
        self.clear_live_state(&uri).await;
    }

    pub(super) async fn apply_document_changes(
        &self,
        uri: &Url,
        version: i32,
        changes: Vec<TextDocumentContentChangeEvent>,
    ) -> bool {
        let applied = {
            let mut documents = self.open_docs.lock().await;
            let Some(document) = documents.get_mut(uri) else {
                return false;
            };
            let mut text = document.text.to_string();
            for change in changes {
                match change.range {
                    None => text = change.text,
                    Some(range) => {
                        let Some(start) = utf16_position_to_byte(&text, range.start) else {
                            return false;
                        };
                        let Some(end) = utf16_position_to_byte(&text, range.end) else {
                            return false;
                        };
                        if start > end {
                            return false;
                        }
                        text.replace_range(start..end, &change.text);
                    }
                }
            }
            document.text = Arc::from(text);
            document.version = version;
            document.relation_overlay = RelationOverlayState::Unsaved;
            true
        };
        if applied {
            self.clear_live_state(uri).await;
        }
        applied
    }

    pub(super) async fn save_document(&self, uri: &Url, _generation: SemanticGeneration) {
        if let Some(document) = self.open_docs.lock().await.get_mut(uri) {
            document.relation_overlay = RelationOverlayState::SavedAwaitingContentHash(
                blake3::hash(document.text.as_bytes()).to_hex().to_string(),
            );
        }
    }

    /// Clear saved overlays only when the published revision contains the same
    /// bytes as the still-open document. A generation advance alone is not
    /// evidence that this particular file was part of that publication.
    pub(super) async fn reconcile_published_files(
        &self,
        root: PathBuf,
        rel_paths: Option<Vec<String>>,
        generation: SemanticGeneration,
    ) {
        let candidates: Vec<(Url, String, String)> = {
            let docs = self.open_docs.lock().await;
            docs.iter()
                .filter_map(|(uri, document)| {
                    let RelationOverlayState::SavedAwaitingContentHash(hash) =
                        &document.relation_overlay
                    else {
                        return None;
                    };
                    let path = uri.to_file_path().ok()?;
                    let rel = pathing::relative_slash_path(&root, &path).ok()?;
                    if rel_paths
                        .as_ref()
                        .is_some_and(|paths| !paths.iter().any(|path| path == &rel))
                    {
                        return None;
                    }
                    Some((uri.clone(), rel, hash.clone()))
                })
                .collect()
        };
        if candidates.is_empty() {
            return;
        }

        let db_path = match pathing::default_index_path(&root) {
            Ok(path) => path,
            Err(_) => return,
        };
        let paths: Vec<String> = candidates.iter().map(|(_, rel, _)| rel.clone()).collect();
        let stored = match tokio::task::spawn_blocking(move || {
            IndexStore::read_at_generation(&db_path, generation.0, |store| {
                store.stored_files(&paths)
            })
        })
        .await
        {
            Ok(Ok(files)) => files,
            _ => return,
        };

        let mut docs = self.open_docs.lock().await;
        for (uri, rel, expected_hash) in candidates {
            if stored
                .get(&rel)
                .is_some_and(|file| file.hash == expected_hash)
            {
                if let Some(document) = docs.get_mut(&uri) {
                    if matches!(
                        &document.relation_overlay,
                        RelationOverlayState::SavedAwaitingContentHash(hash) if hash == &expected_hash
                    ) {
                        document.relation_overlay = RelationOverlayState::Clean;
                    }
                }
            }
        }
    }

    pub(super) async fn close_document(&self, uri: &Url) {
        self.open_docs.lock().await.remove(uri);
        self.clear_live_state(uri).await;
        self.live_parse_gates.lock().await.remove(uri);
    }

    pub(super) async fn clear_live_state(&self, uri: &Url) {
        self.live_parse_cache.write().await.remove(uri);
        self.local_word_cache.lock().await.remove(uri);
        if let Some((_, cancellation)) = self.live_parse_cancellations.lock().await.remove(uri) {
            cancellation.store(true, Ordering::Relaxed);
        }
    }

    pub(super) async fn snapshot(&self, uri: &Url) -> Option<DocumentSnapshot> {
        self.open_docs
            .lock()
            .await
            .get(uri)
            .map(|document| DocumentSnapshot {
                version: document.version,
                text: document.text.clone(),
                relation_overlay: document.relation_overlay.clone(),
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
            .is_some_and(|document| document.version == version)
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
        facts: ParseFacts,
    ) -> Option<Arc<FileSemanticIndex>> {
        let cache = self.live_parse_cache.read().await;
        cache
            .get(uri)
            .and_then(|(cached_version, cached_facts, parsed)| {
                (*cached_version == version && cached_facts.contains(facts)).then(|| parsed.clone())
            })
    }

    pub(super) async fn cached_live_parse_facts(&self, uri: &Url, version: i32) -> ParseFacts {
        self.live_parse_cache
            .read()
            .await
            .get(uri)
            .filter(|(cached_version, _, _)| *cached_version == version)
            .map_or(ParseFacts::empty(), |(_, facts, _)| *facts)
    }

    pub(super) async fn live_parse_gate(&self, uri: &Url) -> Arc<Mutex<()>> {
        self.live_parse_gates
            .lock()
            .await
            .entry(uri.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    pub(super) async fn live_parse_cancellation(&self, uri: &Url, version: i32) -> Arc<AtomicBool> {
        let mut cancellations = self.live_parse_cancellations.lock().await;
        if let Some((cached_version, cancellation)) = cancellations.get(uri) {
            if *cached_version == version {
                return cancellation.clone();
            }
            cancellation.store(true, Ordering::Relaxed);
        }
        let cancellation = Arc::new(AtomicBool::new(false));
        cancellations.insert(uri.clone(), (version, cancellation.clone()));
        cancellation
    }

    pub(super) async fn store_live_parse(
        &self,
        uri: Url,
        version: i32,
        facts: ParseFacts,
        parsed: Arc<FileSemanticIndex>,
    ) {
        // Latest document revision wins. Holding `open_docs` until the cache
        // write completes makes this atomic with didChange's version update:
        // either the old result lands first and didChange clears it, or the
        // version has advanced and the old result is discarded.
        let open_docs = self.open_docs.lock().await;
        if open_docs
            .get(&uri)
            .is_some_and(|document| document.version == version)
        {
            self.live_parse_cache
                .write()
                .await
                .insert(uri, (version, facts, parsed));
        }
    }

    pub(super) async fn all_snapshots(&self) -> Vec<(Url, DocumentSnapshot)> {
        self.open_docs
            .lock()
            .await
            .iter()
            .map(|(uri, document)| {
                (
                    uri.clone(),
                    DocumentSnapshot {
                        version: document.version,
                        text: document.text.clone(),
                        relation_overlay: document.relation_overlay.clone(),
                    },
                )
            })
            .collect()
    }

    #[cfg(test)]
    pub(super) async fn store_live_parse_for_test(
        &self,
        uri: Url,
        version: i32,
        parsed: Arc<FileSemanticIndex>,
    ) {
        self.store_live_parse(uri, version, ParseFacts::ALL, parsed)
            .await;
    }

    #[cfg(test)]
    pub(super) async fn live_parse_for_test(&self, uri: &Url) -> Option<Arc<FileSemanticIndex>> {
        self.live_parse_cache
            .read()
            .await
            .get(uri)
            .map(|(_, _, parsed)| parsed.clone())
    }

    #[cfg(test)]
    pub(super) async fn local_word_cache_entry_for_test(
        &self,
        uri: &Url,
    ) -> Option<(i32, Arc<HashSet<String>>)> {
        self.local_word_cache.lock().await.get(uri).cloned()
    }
}

fn utf16_position_to_byte(text: &str, position: Position) -> Option<usize> {
    let mut line_start = 0usize;
    for _ in 0..position.line {
        let newline = text[line_start..].find('\n')?;
        line_start += newline + 1;
    }
    let line_end = text[line_start..]
        .find('\n')
        .map_or(text.len(), |offset| line_start + offset);
    let line = &text[line_start..line_end];
    let target = position.character as usize;
    let mut utf16_units = 0usize;
    for (byte, ch) in line.char_indices() {
        if utf16_units == target {
            return Some(line_start + byte);
        }
        utf16_units += ch.len_utf16();
        if utf16_units > target {
            return None;
        }
    }
    (utf16_units == target).then_some(line_end)
}

async fn relation_overlay_state_on_open(uri: &Url, text: &str) -> RelationOverlayState {
    let Ok(path) = uri.to_file_path() else {
        return RelationOverlayState::Unsaved;
    };
    match tokio::task::spawn_blocking(move || std::fs::read(path)).await {
        Ok(Ok(bytes)) if bytes == text.as_bytes() => RelationOverlayState::Clean,
        Ok(Ok(_)) | Ok(Err(_)) | Err(_) => RelationOverlayState::Unsaved,
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
    pub(in crate::server) semantic_generation: SemanticGeneration,
    pub(in crate::server) name_table: Option<Arc<NameTable>>,
    pub(in crate::server) reach_graph: Option<Arc<ReachGraph>>,
    pub(in crate::server) include_table: Option<Arc<IncludeCompletionTable>>,
    pub(in crate::server) indexed_files: Option<Arc<Vec<(String, PathBuf)>>>,
    pub(in crate::server) project_context: Option<Arc<ProjectContextIndex>>,
    pub(in crate::server) call_read_handle: Option<Arc<CallReadHandle>>,
    #[allow(dead_code)] // Captured now; request capability-health routing is the next phase.
    pub(in crate::server) degraded: crate::progress::DegradedCapabilities,
}

impl EngineSnapshot {
    fn empty(root: PathBuf) -> Self {
        Self {
            root,
            epoch: state::EngineEpoch::missing(),
            semantic_generation: SemanticGeneration::MISSING,
            name_table: None,
            reach_graph: None,
            include_table: None,
            indexed_files: None,
            project_context: None,
            call_read_handle: None,
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
    pub(super) prefix_ranking: crate::completion::CompletionPrefixRanking,
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
    pub(super) semantic_generation: SemanticGeneration,
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

    pub(super) async fn remove_workspace_roots(&self, roots: &[PathBuf]) {
        if roots.is_empty() {
            return;
        }
        self.engine_snapshots
            .lock()
            .await
            .retain(|root, _| !roots.contains(root));
        self.clear_all_completion_memos().await;
        self.invalidate_references();
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

    pub(super) async fn invalidate_after_index_change(&self) {
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
        let current = self
            .current_engine_snapshot(&root)
            .await
            .unwrap_or_else(|| Arc::new(EngineSnapshot::empty(root.clone())));
        self.publish_engine_snapshot(EngineSnapshot {
            root,
            epoch: self.allocate_engine_epoch(),
            semantic_generation: current.semantic_generation,
            name_table: Some(table),
            reach_graph: current.reach_graph.clone(),
            include_table: current.include_table.clone(),
            indexed_files: current.indexed_files.clone(),
            project_context: current.project_context.clone(),
            call_read_handle: current.call_read_handle.clone(),
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
            semantic_generation: current.semantic_generation,
            name_table: current.name_table.clone(),
            reach_graph: current.reach_graph.clone(),
            include_table: current.include_table.clone(),
            indexed_files: Some(files),
            project_context: current.project_context.clone(),
            call_read_handle: current.call_read_handle.clone(),
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
            semantic_generation: current.semantic_generation,
            name_table: current.name_table.clone(),
            reach_graph: Some(graph),
            include_table: current.include_table.clone(),
            indexed_files: current.indexed_files.clone(),
            project_context: current.project_context.clone(),
            call_read_handle: current.call_read_handle.clone(),
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

    #[cfg(test)]
    pub(super) async fn change_document(&self, uri: Url, version: i32, text: String) {
        self.documents
            .change_document(uri.clone(), version, text)
            .await;
        self.cache.invalidate_references();
    }

    pub(super) async fn apply_document_changes(
        &self,
        uri: &Url,
        version: i32,
        changes: Vec<TextDocumentContentChangeEvent>,
    ) -> bool {
        let applied = self
            .documents
            .apply_document_changes(uri, version, changes)
            .await;
        if applied {
            self.cache.invalidate_references();
        }
        applied
    }

    pub(super) async fn close_document(&self, uri: &Url) {
        self.documents.close_document(uri).await;
        self.cache.clear_completion_memo(uri).await;
    }

    pub(super) async fn save_document(&self, uri: &Url, generation: SemanticGeneration) {
        self.documents.save_document(uri, generation).await;
        self.cache.invalidate_references();
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
