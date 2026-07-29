use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::{Mutex, RwLock};
use tower_lsp::lsp_types::{Position, TextDocumentContentChangeEvent, Url};

use super::state;
use super::LocalWordCache;
use crate::call_model::SemanticGeneration;
use crate::candidate_service::CandidateOverlaySnapshot;
use crate::completion_words;
use crate::config::SourceLanguage;
use crate::parser::{FileSemanticIndex, ParseFacts};
use crate::pathing;
#[cfg(test)]
use crate::query::NameTable;
#[cfg(test)]
use crate::reachability::ReachGraph;
use crate::store::IndexStore;

mod models;
mod session;
use models::{invalidate_candidate_overlay_root, CandidateOverlayCacheKey};
pub(super) use models::{
    CacheLedger, CachePublishReport, CompletionMemoLookup, EngineSnapshot, RequestContext,
    RequestSettings,
};
pub(super) use session::WorkspaceSession;

type LiveParseCache =
    Arc<RwLock<HashMap<Url, (i32, SourceLanguage, ParseFacts, Arc<FileSemanticIndex>)>>>;
type ExternalOverlayParseCache =
    Arc<Mutex<HashMap<ExternalOverlayParseKey, Arc<FileSemanticIndex>>>>;
type LiveParseGates = Arc<Mutex<HashMap<Url, Arc<Mutex<()>>>>>;
type LiveParseCancellations = Arc<Mutex<HashMap<Url, (i32, Arc<AtomicBool>)>>>;
const MAX_EXTERNAL_OVERLAY_PARSE_CACHE_ENTRIES: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ExternalOverlayParseKey {
    uri: Url,
    version: i32,
    language: SourceLanguage,
    identity_path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RelationOverlayState {
    Clean,
    Unsaved,
    SavedAwaitingContentHash(String),
}

impl RelationOverlayState {
    fn is_active(&self) -> bool {
        !matches!(self, Self::Clean)
    }
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
    overlay_epoch: Arc<AtomicU64>,
    pub(in crate::server) live_parse_cache: LiveParseCache,
    external_overlay_parse_cache: ExternalOverlayParseCache,
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

/// One lock-consistent view of the live-document state used by a semantic
/// request. `current` and `all` are captured under the same `open_docs` guard,
/// and `overlay_epoch` is read before that guard is released. Consumers can
/// therefore never pair current-buffer text with a different all-open overlay.
#[derive(Clone, Debug)]
pub(super) struct DocumentRequestSnapshot {
    pub(super) overlay_epoch: u64,
    pub(super) current: Option<DocumentSnapshot>,
    pub(super) all: Vec<(Url, DocumentSnapshot)>,
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
        // `didOpen` proves only which bytes the editor is showing. Even when
        // those bytes match disk, the active semantic generation may predate
        // the file or contain an older external edit. Keep the document in the
        // overlay until `reconcile_published_files` proves that this exact hash
        // exists in the captured generation.
        let relation_overlay = RelationOverlayState::SavedAwaitingContentHash(
            blake3::hash(text.as_bytes()).to_hex().to_string(),
        );
        let mut documents = self.open_docs.lock().await;
        let replaced_active = documents
            .get(&uri)
            .is_some_and(|document| document.relation_overlay.is_active());
        let inserted_active = relation_overlay.is_active();
        documents.insert(
            uri,
            OpenDocument {
                version,
                text: Arc::from(text),
                relation_overlay,
            },
        );
        if replaced_active || inserted_active {
            self.bump_overlay_epoch();
        }
    }

    #[cfg(test)]
    pub(super) async fn change_document(&self, uri: Url, version: i32, text: String) {
        let mut documents = self.open_docs.lock().await;
        documents.insert(
            uri.clone(),
            OpenDocument {
                version,
                text: Arc::from(text),
                relation_overlay: RelationOverlayState::Unsaved,
            },
        );
        self.bump_overlay_epoch();
        drop(documents);
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
            self.bump_overlay_epoch();
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
            self.bump_overlay_epoch();
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
        let mut changed = false;
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
                        changed = true;
                    }
                }
            }
        }
        if changed {
            self.bump_overlay_epoch();
        }
    }

    pub(super) async fn close_document(&self, uri: &Url) {
        let mut documents = self.open_docs.lock().await;
        let removed_active = documents
            .remove(uri)
            .is_some_and(|document| document.relation_overlay.is_active());
        if removed_active {
            // Keep the mutation and epoch transition under the same guard used
            // by `capture_request_snapshot`.
            self.bump_overlay_epoch();
        }
        drop(documents);
        self.clear_live_state(uri).await;
        self.live_parse_gates.lock().await.remove(uri);
    }

    pub(super) async fn clear_live_state(&self, uri: &Url) {
        self.live_parse_cache.write().await.remove(uri);
        self.external_overlay_parse_cache
            .lock()
            .await
            .retain(|key, _| &key.uri != uri);
        self.local_word_cache.lock().await.remove(uri);
        if let Some((_, cancellation)) = self.live_parse_cancellations.lock().await.remove(uri) {
            cancellation.store(true, Ordering::Relaxed);
        }
    }

    pub(super) async fn invalidate_language_config_roots(&self, roots: &[PathBuf]) {
        let affected = {
            let mut documents = self.open_docs.lock().await;
            let mut affected = Vec::new();
            for (uri, document) in documents.iter_mut() {
                let Some(path) = uri.to_file_path().ok() else {
                    continue;
                };
                if !roots
                    .iter()
                    .any(|root| pathing::path_is_within(root, &path))
                {
                    continue;
                }
                if matches!(document.relation_overlay, RelationOverlayState::Clean) {
                    document.relation_overlay = RelationOverlayState::SavedAwaitingContentHash(
                        blake3::hash(document.text.as_bytes()).to_hex().to_string(),
                    );
                }
                affected.push(uri.clone());
            }
            if !affected.is_empty() {
                self.bump_overlay_epoch();
            }
            affected
        };
        for uri in affected {
            self.clear_live_state(&uri).await;
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
        language: SourceLanguage,
        facts: ParseFacts,
    ) -> Option<Arc<FileSemanticIndex>> {
        let cache = self.live_parse_cache.read().await;
        cache
            .get(uri)
            .and_then(|(cached_version, cached_language, cached_facts, parsed)| {
                (*cached_version == version
                    && *cached_language == language
                    && cached_facts.contains(facts))
                .then(|| parsed.clone())
            })
    }

    pub(super) async fn cached_external_overlay_parse(
        &self,
        uri: &Url,
        version: i32,
        language: SourceLanguage,
        identity_path: &str,
    ) -> Option<Arc<FileSemanticIndex>> {
        self.external_overlay_parse_cache
            .lock()
            .await
            .get(&ExternalOverlayParseKey {
                uri: uri.clone(),
                version,
                language,
                identity_path: identity_path.to_string(),
            })
            .cloned()
    }

    pub(super) async fn cached_live_parse_facts(
        &self,
        uri: &Url,
        version: i32,
        language: SourceLanguage,
    ) -> ParseFacts {
        self.live_parse_cache
            .read()
            .await
            .get(uri)
            .filter(|(cached_version, cached_language, _, _)| {
                *cached_version == version && *cached_language == language
            })
            .map_or(ParseFacts::empty(), |(_, _, facts, _)| *facts)
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
        language: SourceLanguage,
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
                .insert(uri, (version, language, facts, parsed));
        }
    }

    pub(super) async fn store_external_overlay_parse(
        &self,
        uri: Url,
        version: i32,
        language: SourceLanguage,
        identity_path: String,
        parsed: Arc<FileSemanticIndex>,
    ) {
        let open_docs = self.open_docs.lock().await;
        if open_docs.get(&uri).map(|document| document.version) != Some(version) {
            return;
        }
        let key = ExternalOverlayParseKey {
            uri,
            version,
            language,
            identity_path,
        };
        let mut cache = self.external_overlay_parse_cache.lock().await;
        if cache.len() >= MAX_EXTERNAL_OVERLAY_PARSE_CACHE_ENTRIES && !cache.contains_key(&key) {
            if let Some(evicted) = cache.keys().next().cloned() {
                cache.remove(&evicted);
            }
        }
        cache.insert(key, parsed);
    }

    #[cfg(test)]
    pub(super) async fn external_overlay_parse_cache_len_for_test(&self) -> usize {
        self.external_overlay_parse_cache.lock().await.len()
    }

    pub(super) async fn capture_request_snapshot(
        &self,
        current_uri: Option<&Url>,
    ) -> DocumentRequestSnapshot {
        let documents = self.open_docs.lock().await;
        let epoch = self.overlay_epoch.load(Ordering::Acquire);
        let current = current_uri.and_then(|uri| {
            documents.get(uri).map(|document| DocumentSnapshot {
                version: document.version,
                text: document.text.clone(),
                relation_overlay: document.relation_overlay.clone(),
            })
        });
        let all = documents
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
            .collect();
        DocumentRequestSnapshot {
            overlay_epoch: epoch,
            current,
            all,
        }
    }

    #[cfg(test)]
    pub(super) async fn all_snapshots_with_overlay_epoch(
        &self,
    ) -> (u64, Vec<(Url, DocumentSnapshot)>) {
        let snapshot = self.capture_request_snapshot(None).await;
        (snapshot.overlay_epoch, snapshot.all)
    }

    fn bump_overlay_epoch(&self) {
        self.overlay_epoch.fetch_add(1, Ordering::AcqRel);
    }

    #[cfg(test)]
    pub(super) async fn store_live_parse_for_test(
        &self,
        uri: Url,
        version: i32,
        parsed: Arc<FileSemanticIndex>,
    ) {
        let language = uri
            .to_file_path()
            .ok()
            .map(|path| SourceLanguage::default_for_path(&path))
            .unwrap_or(SourceLanguage::C);
        self.store_live_parse(uri, version, language, ParseFacts::ALL, parsed)
            .await;
    }

    #[cfg(test)]
    pub(super) async fn live_parse_for_test(&self, uri: &Url) -> Option<Arc<FileSemanticIndex>> {
        self.live_parse_cache
            .read()
            .await
            .get(uri)
            .map(|(_, _, _, parsed)| parsed.clone())
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

impl CacheLedger {
    pub(in crate::server) async fn current_engine_snapshot(
        &self,
        root: &PathBuf,
    ) -> Option<Arc<EngineSnapshot>> {
        self.engine_snapshots.lock().await.get(root).cloned()
    }

    pub(in crate::server) async fn publish_workspace_semantics_if_absent(
        &self,
        root: PathBuf,
        workspace_semantics: Arc<super::workspace_config::PublishedWorkspaceSemantics>,
    ) -> Arc<EngineSnapshot> {
        let _publish_guard = self.publish_gate.lock().await;
        if let Some(current) = self.current_engine_snapshot(&root).await {
            return current;
        }

        let mut snapshot = EngineSnapshot::empty(root);
        snapshot.epoch = self.allocate_engine_epoch();
        snapshot.workspace_semantics = workspace_semantics;
        self.publish_engine_snapshot(snapshot).await
    }

    pub(super) async fn remove_workspace_roots(&self, roots: &[PathBuf]) {
        if roots.is_empty() {
            return;
        }
        self.engine_snapshots
            .lock()
            .await
            .retain(|root, _| !roots.contains(root));
        let mut overlays = self.candidate_overlays.lock().await;
        for root in roots {
            invalidate_candidate_overlay_root(&mut overlays, root);
        }
        drop(overlays);
        self.clear_all_completion_memos().await;
        self.invalidate_references();
    }

    pub(super) async fn invalidate_candidate_overlay_roots(&self, roots: &[PathBuf]) {
        let mut overlays = self.candidate_overlays.lock().await;
        for root in roots {
            invalidate_candidate_overlay_root(&mut overlays, root);
        }
        drop(overlays);
        self.clear_all_completion_memos().await;
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
        // Invalidate on both sides of the engine-map swap. A builder captured
        // in either half of this publication window receives a stale cache
        // revision and cannot republish after the second invalidation.
        {
            let mut overlays = self.candidate_overlays.lock().await;
            invalidate_candidate_overlay_root(&mut overlays, &snapshot.root);
        }
        self.engine_snapshots
            .lock()
            .await
            .insert(snapshot.root.clone(), snapshot.clone());
        {
            let mut overlays = self.candidate_overlays.lock().await;
            invalidate_candidate_overlay_root(&mut overlays, &snapshot.root);
        }
        snapshot
    }

    pub(super) async fn candidate_overlay(
        &self,
        root: &Path,
        semantic_generation: SemanticGeneration,
        overlay_epoch: u64,
    ) -> (Option<Arc<CandidateOverlaySnapshot>>, u64) {
        let overlays = self.candidate_overlays.lock().await;
        let revision = overlays.root_revisions.get(root).copied().unwrap_or(0);
        let cached = overlays
            .entries
            .get(&CandidateOverlayCacheKey {
                root: root.to_path_buf(),
                semantic_generation,
                overlay_epoch,
            })
            .cloned();
        (cached, revision)
    }

    /// Publish one fully-built immutable overlay. A concurrent request may
    /// have won the same key; in that case both callers share the first Arc.
    /// Older epochs are dropped once a newer overlay for the same base lands.
    pub(super) async fn publish_candidate_overlay(
        &self,
        root: PathBuf,
        semantic_generation: SemanticGeneration,
        overlay_epoch: u64,
        expected_cache_revision: u64,
        overlay: Arc<CandidateOverlaySnapshot>,
    ) -> Arc<CandidateOverlaySnapshot> {
        let key = CandidateOverlayCacheKey {
            root: root.clone(),
            semantic_generation,
            overlay_epoch,
        };
        let mut cache = self.candidate_overlays.lock().await;
        if cache.root_revisions.get(&root).copied().unwrap_or(0) != expected_cache_revision {
            return overlay;
        }
        if let Some(existing) = cache.entries.get(&key) {
            return existing.clone();
        }
        let newest_epoch = cache
            .entries
            .keys()
            .filter(|candidate| {
                candidate.root == root && candidate.semantic_generation == semantic_generation
            })
            .map(|candidate| candidate.overlay_epoch)
            .max();
        if newest_epoch.is_none_or(|epoch| overlay_epoch >= epoch) {
            cache.entries.retain(|candidate, _| {
                candidate.root != root
                    || candidate.semantic_generation != semantic_generation
                    || candidate.overlay_epoch > overlay_epoch
            });
            cache.entries.insert(key, overlay.clone());
        }
        overlay
    }

    #[cfg(test)]
    pub(super) async fn candidate_overlay_cache_len_for_test(&self) -> usize {
        self.candidate_overlays.lock().await.entries.len()
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
            declaration_index: current.declaration_index.clone(),
            name_table: Some(table),
            fallback_completion_table: current.fallback_completion_table.clone(),
            reach_graph: current.reach_graph.clone(),
            include_table: current.include_table.clone(),
            go_import_table: current.go_import_table.clone(),
            indexed_files: current.indexed_files.clone(),
            project_context: current.project_context.clone(),
            call_read_handle: current.call_read_handle.clone(),
            workspace_semantics: current.workspace_semantics.clone(),
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
            declaration_index: current.declaration_index.clone(),
            name_table: current.name_table.clone(),
            fallback_completion_table: current.fallback_completion_table.clone(),
            reach_graph: current.reach_graph.clone(),
            include_table: current.include_table.clone(),
            go_import_table: current.go_import_table.clone(),
            indexed_files: Some(files),
            project_context: current.project_context.clone(),
            call_read_handle: current.call_read_handle.clone(),
            workspace_semantics: current.workspace_semantics.clone(),
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
            declaration_index: current.declaration_index.clone(),
            name_table: current.name_table.clone(),
            fallback_completion_table: current.fallback_completion_table.clone(),
            reach_graph: Some(graph),
            include_table: current.include_table.clone(),
            go_import_table: current.go_import_table.clone(),
            indexed_files: current.indexed_files.clone(),
            project_context: current.project_context.clone(),
            call_read_handle: current.call_read_handle.clone(),
            workspace_semantics: current.workspace_semantics.clone(),
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
