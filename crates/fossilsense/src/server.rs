use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};

use anyhow::Result;
use serde_json::Value;
use tokio::sync::{Mutex, RwLock};
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::{
    CompletionItem, CompletionItemKind, CompletionList, CompletionOptions, CompletionParams,
    CompletionResponse, DidChangeTextDocumentParams, DidChangeWatchedFilesParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
    DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse, Documentation,
    ExecuteCommandParams, GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverParams,
    HoverProviderCapability, InitializeParams, InitializeResult, InitializedParams, Location,
    MessageType, OneOf, ReferenceParams, SaveOptions, SemanticTokenType, SemanticTokens,
    SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensOptions, SemanticTokensParams,
    SemanticTokensRangeParams, SemanticTokensRangeResult, SemanticTokensResult,
    SemanticTokensServerCapabilities, ServerCapabilities, ServerInfo, SignatureHelp,
    SignatureHelpParams, SymbolInformation, TextDocumentSyncCapability, TextDocumentSyncKind,
    TextDocumentSyncOptions, TextDocumentSyncSaveOptions, Url, WorkspaceFoldersServerCapabilities,
    WorkspaceServerCapabilities, WorkspaceSymbolParams,
};
use tower_lsp::{async_trait, Client, LanguageServer, LspService, Server};

use crate::completion_words;
use crate::config::WorkspaceConfig;
use crate::includes::{self, IncludeForm};
use crate::model;
use crate::parser::{self, FileSemanticIndex};
use crate::pathing;
use crate::query::{self, NameTable};
use crate::reachability::{self, ReachGraph};
use crate::references;
use crate::resolver;
use crate::store::IndexStore;

mod hover;
mod include_completion;
mod indexing;
mod language_server;
mod lsp_adapters;
mod member_completion;
mod options;
mod semantic_tokens;
mod signature_help;
mod state;

use include_completion::{
    collect_include_candidates_with_table, configured_include_paths, location_at_file_start,
    resolve_include_paths, ExternalIncludeDirCache, IncludeCompletionTable,
};
#[cfg(test)]
use indexing::{
    ready_cache_message, rebuild_include_table, rebuild_indexed_file_list, RootDirtyChange,
};
use indexing::{watched_change_in_scope, IndexScheduleState, WatchDecision};
use lsp_adapters::{
    candidate_to_location, grouped_reference_items, hit_to_location, parsed_to_document_symbol,
    record_to_symbol_information, GroupedReferenceItem,
};
use options::{
    candidate_reason_log_lines, completion_trigger_characters, empty_completion_list,
    member_completion_is_incomplete, parse_completion_mode, parse_debug_candidate_reasons,
    parse_debug_perf_logs, parse_include_paths, parse_include_scoping_enabled,
    parse_semantic_coloring_mode, signature_help_options,
};

type NameTables = Arc<Mutex<HashMap<PathBuf, Arc<NameTable>>>>;
type ReachGraphs = Arc<Mutex<HashMap<PathBuf, Arc<StdRwLock<ReachGraph>>>>>;
type IncludeTables = Arc<Mutex<HashMap<PathBuf, Arc<IncludeCompletionTable>>>>;
type IndexedFileLists = Arc<Mutex<HashMap<PathBuf, Arc<Vec<(String, PathBuf)>>>>>;
type IndexGenerations = Arc<Mutex<HashMap<PathBuf, state::WorkspaceGeneration>>>;
type LocalWordEntry = (i32, Arc<HashSet<String>>);
type LocalWordCache = Arc<Mutex<HashMap<Url, LocalWordEntry>>>;
type IndexSchedule = Arc<Mutex<IndexScheduleState>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CompletionCandidateSource {
    Indexed,
    #[allow(dead_code)]
    LocalBinding,
    LocalWord,
}

#[derive(Debug)]
struct CompletionCandidate {
    name: String,
    tier: model::ScopeTier,
    confidence: model::ResolutionConfidence,
    score: i32,
    item: CompletionItem,
    source: CompletionCandidateSource,
}

fn dedup_completion_candidates(candidates: Vec<CompletionCandidate>) -> Vec<CompletionCandidate> {
    let mut best_by_name: HashMap<String, usize> = HashMap::new();
    let mut survivors: Vec<Option<CompletionCandidate>> =
        candidates.into_iter().map(Some).collect();
    for i in 0..survivors.len() {
        let Some((name, tier, confidence)) = survivors[i]
            .as_ref()
            .map(|candidate| (candidate.name.clone(), candidate.tier, candidate.confidence))
        else {
            continue;
        };
        let key = (tier, confidence);
        match best_by_name.get(&name) {
            None => {
                best_by_name.insert(name, i);
            }
            Some(&prev_i) => {
                let (prev_tier, prev_conf, prev_score, prev_source) = {
                    let prev = survivors[prev_i].as_ref().expect("survivor present");
                    (prev.tier, prev.confidence, prev.score, prev.source)
                };
                let current = survivors[i].as_ref().expect("survivor present");
                if completion_candidate_beats(
                    current.source,
                    key,
                    current.score,
                    prev_source,
                    (prev_tier, prev_conf),
                    prev_score,
                ) {
                    survivors[prev_i] = None;
                    best_by_name.insert(name, i);
                } else {
                    survivors[i] = None;
                }
            }
        }
    }
    survivors.into_iter().flatten().collect()
}

fn completion_candidate_beats(
    source: CompletionCandidateSource,
    key: (model::ScopeTier, model::ResolutionConfidence),
    score: i32,
    prev_source: CompletionCandidateSource,
    prev_key: (model::ScopeTier, model::ResolutionConfidence),
    prev_score: i32,
) -> bool {
    let rank = completion_source_rank(source);
    let prev_rank = completion_source_rank(prev_source);
    rank > prev_rank
        || (rank == prev_rank && (key > prev_key || (key == prev_key && score > prev_score)))
}

fn completion_source_rank(source: CompletionCandidateSource) -> u8 {
    match source {
        CompletionCandidateSource::LocalBinding => 3,
        CompletionCandidateSource::Indexed => 2,
        CompletionCandidateSource::LocalWord => 1,
    }
}

#[allow(dead_code)]
fn completion_items_for_local_bindings(
    hits: Vec<query::LocalCompletionCandidate>,
) -> Vec<CompletionCandidate> {
    hits.into_iter()
        .map(|hit| CompletionCandidate {
            name: hit.name.clone(),
            tier: model::ScopeTier::Current,
            confidence: model::ResolutionConfidence::Heuristic,
            score: hit.score,
            item: CompletionItem {
                label: hit.name,
                kind: Some(CompletionItemKind::VARIABLE),
                detail: Some(hit.detail),
                sort_text: Some(format!("{:08}", 100_000_000 - hit.score)),
                ..Default::default()
            },
            source: CompletionCandidateSource::LocalBinding,
        })
        .collect()
}

fn exact_indexed_completion_candidates_for_local_word(
    table: &NameTable,
    word: &str,
    local_score: i32,
    scope: Option<&query::CompletionScope>,
    open_reason: Option<reachability::OpenReason>,
    limit: usize,
) -> Vec<CompletionCandidate> {
    table
        .exact_name_hits_scoped(word, limit, scope)
        .into_iter()
        .map(|hit| {
            let (confidence, reason) =
                resolver::confidence_reason_for(hit.tier, false, open_reason);
            let label = model::completion_scope_label(hit.tier, confidence, reason);
            CompletionCandidate {
                name: hit.name.clone(),
                tier: hit.tier,
                confidence,
                score: local_score,
                item: CompletionItem {
                    label: hit.name,
                    kind: Some(query::lsp_completion_kind_from_parser(hit.kind)),
                    sort_text: Some(format!("{:08}", 100_000_000 - local_score)),
                    detail: label.as_ref().map(|l| l.detail.to_string()),
                    documentation: label.map(|l| Documentation::String(l.documentation)),
                    ..Default::default()
                },
                source: CompletionCandidateSource::Indexed,
            }
        })
        .collect()
}

const REFRESH_INDEX_COMMAND: &str = "fossilsense.refreshIndex";
const REFRESH_INDEX_LSP_COMMAND: &str = "fossilsense.lsp.refreshIndex";
const REBUILD_INDEX_COMMAND: &str = "fossilsense.rebuildIndex";
const REBUILD_INDEX_LSP_COMMAND: &str = "fossilsense.lsp.rebuildIndex";
/// Client command for role-grouped find-references: takes one argument object
/// `{ uri, line, character }` and returns the role-labeled hits the standard
/// `textDocument/references` cannot carry over the wire.
const GROUPED_REFERENCES_LSP_COMMAND: &str = "fossilsense.lsp.groupedReferences";

pub async fn run_stdio() -> Result<()> {
    eprintln!("FossilSense LSP starting on stdio");

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(|client| Backend {
        client,
        workspace_roots: Arc::new(Mutex::new(Vec::new())),
        index_schedule: Arc::new(Mutex::new(IndexScheduleState::default())),
        open_docs: Arc::new(Mutex::new(HashMap::new())),
        live_parse_cache: Arc::new(RwLock::new(HashMap::new())),
        name_tables: Arc::new(Mutex::new(HashMap::new())),
        reach_graphs: Arc::new(Mutex::new(HashMap::new())),
        include_tables: Arc::new(Mutex::new(HashMap::new())),
        indexed_file_lists: Arc::new(Mutex::new(HashMap::new())),
        index_generations: Arc::new(Mutex::new(HashMap::new())),
        external_include_dir_cache: Arc::new(StdMutex::new(HashMap::new())),
        local_word_cache: Arc::new(Mutex::new(HashMap::new())),
        include_paths: Arc::new(Mutex::new(Vec::new())),
        completion_enabled: AtomicBool::new(true),
        semantic_coloring_enabled: AtomicBool::new(true),
        scoping_enabled: AtomicBool::new(true),
        debug_candidate_reasons: AtomicBool::new(false),
        perf_logging_enabled: AtomicBool::new(false),
        reference_role_cache: Arc::new(references::ReferenceRoleCache::new()),
        reference_search_cache: Arc::new(references::ReferenceSearchCache::new()),
        completion_memo: Arc::new(Mutex::new(HashMap::new())),
        config_cache: Arc::new(Mutex::new(HashMap::new())),
    });

    Server::new(stdin, stdout, socket).serve(service).await;
    Ok(())
}

struct Backend {
    client: Client,
    workspace_roots: Arc<Mutex<Vec<PathBuf>>>,
    index_schedule: IndexSchedule,
    /// Full text of currently-open buffers (FULL text sync), with LSP version
    /// for cache-invalidation bookkeeping.
    open_docs: Arc<Mutex<HashMap<Url, (i32, String)>>>,
    /// Live-document parse cache: keyed by `(Url, version)`, stores the full
    /// `FileSemanticIndex` for each open document version so that semantic
    /// tokens, member completion, and document symbols avoid repeated parses.
    live_parse_cache: Arc<RwLock<HashMap<Url, (i32, Arc<FileSemanticIndex>)>>>,
    /// In-memory fuzzy symbol name table per workspace root.
    name_tables: NameTables,
    /// In-memory `#include` reachability graph per workspace root, rebuilt after
    /// each index pass alongside the name table.
    reach_graphs: ReachGraphs,
    /// In-memory include completion table per workspace root. This mirrors the
    /// indexed workspace file list so include completion does not scan SQLite on
    /// each keystroke.
    include_tables: IncludeTables,
    /// Indexed workspace files per root, reused by references discovery.
    indexed_file_lists: IndexedFileLists,
    /// Unified generation token per root for derived index state and dependent
    /// request caches.
    index_generations: IndexGenerations,
    /// Directory-listing cache for configured external include roots.
    external_include_dir_cache: ExternalIncludeDirCache,
    /// Local identifier words extracted from open documents, keyed by URI and
    /// LSP version.
    local_word_cache: LocalWordCache,
    /// External include reference directories (normalized) forwarded from the
    /// client; used for indexing, include-path completion, and jump-to-header.
    include_paths: Arc<Mutex<Vec<String>>>,
    /// Whether completion is enabled (based on initializationOptions).
    completion_enabled: AtomicBool,
    /// Whether semantic coloring is enabled (based on initializationOptions).
    semantic_coloring_enabled: AtomicBool,
    /// Whether limited include-reachability scoping is enabled. When off, both
    /// coloring and completion fall back to whole-index (unscoped) behavior.
    scoping_enabled: AtomicBool,
    /// Whether goto-definition logs each candidate's scope reasoning
    /// (tier/confidence/reason) to the output panel. Off by default; gated by
    /// `fossilsense.debug.candidateReasons`. A debug aid, not a user contract.
    debug_candidate_reasons: AtomicBool,
    /// Whether `[perf]` request/index timings are sent to the output panel.
    /// Off by default; enabled by `RUST_LOG` debug/trace or client init options.
    perf_logging_enabled: AtomicBool,
    /// Fingerprint-keyed cache of per-file reference-role classifications, so
    /// repeated find-references queries do not re-parse unchanged files.
    reference_role_cache: Arc<references::ReferenceRoleCache>,
    /// Complete reference-search result cache shared by standard references and
    /// the grouped-references command. Cleared when open or watched files change.
    reference_search_cache: Arc<references::ReferenceSearchCache>,
    /// Per-document completion narrowing memo: the last prefix and the candidate
    /// pool each workspace table produced for it, reused when the next prefix
    /// extends it and the tables are unchanged.
    completion_memo: Arc<Mutex<HashMap<Url, state::CompletionMemo>>>,
    /// Cache for `WorkspaceConfig` per workspace root. Avoids re-reading and
    /// re-parsing `fossilsense.json` on every `did_change_watched_files` event.
    /// Invalidated when `fossilsense.json` itself changes (which triggers
    /// `WatchDecision::Full` and reloads the config in the index path).
    config_cache: Arc<Mutex<HashMap<PathBuf, WorkspaceConfig>>>,
}

impl Backend {
    /// Resolve an `#include` directive to the header file(s) it names and return
    /// a location at the top of each. One match jumps; several return a ranked
    /// candidate list; none returns nothing (we never fabricate a target).
    async fn goto_include(
        &self,
        uri: &Url,
        form: IncludeForm,
        rel: String,
    ) -> LspResult<Option<GotoDefinitionResponse>> {
        let current_dir = uri_to_path(uri).and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let client_include_roots = self.include_paths.lock().await.clone();
        let workspace_root = self.root_for_uri(uri).await;
        let include_roots =
            configured_include_paths(workspace_root.as_deref(), &client_include_roots);
        let db_path = workspace_root
            .as_ref()
            .and_then(|root| pathing::default_index_path(root).ok());

        let result = tokio::task::spawn_blocking(move || -> Result<Vec<Location>> {
            let resolved = resolve_include_paths(
                form,
                &rel,
                current_dir.as_deref(),
                workspace_root.as_deref(),
                &include_roots,
                db_path.as_deref(),
            )?;
            Ok(resolved
                .iter()
                .filter_map(|path| location_at_file_start(path))
                .collect())
        })
        .await;

        match self.unwrap_query("include definition", result).await {
            Some(locations) if !locations.is_empty() => {
                Ok(Some(GotoDefinitionResponse::Array(locations)))
            }
            _ => Ok(None),
        }
    }

    /// Header-path completion inside an `#include` directive: list matching files
    /// and sub-directories from the current file's directory, indexed workspace
    /// headers' roots, and the configured include paths. Headers only — never
    /// symbols. The delimiter form influences ranking, not which candidates show.
    async fn complete_include(
        &self,
        uri: &Url,
        form: IncludeForm,
        partial: String,
    ) -> LspResult<Option<CompletionResponse>> {
        let (dir_part, seg) = includes::split_partial(&partial);
        let current_dir = uri_to_path(uri).and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let client_include_roots = self.include_paths.lock().await.clone();
        let workspace_root = self.root_for_uri(uri).await;
        let include_table = match &workspace_root {
            Some(root) => self.include_tables.lock().await.get(root).cloned(),
            None => None,
        };
        let include_roots =
            configured_include_paths(workspace_root.as_deref(), &client_include_roots);
        let db_path = workspace_root
            .as_ref()
            .and_then(|root| pathing::default_index_path(root).ok());
        let external_cache = self.external_include_dir_cache.clone();
        let limit = query::COMPLETION_LIMIT;

        let started = tokio::time::Instant::now();
        let hit_memory = include_table.is_some();
        let hit_db = db_path.as_ref().is_some_and(|path| path.exists());
        let result = tokio::task::spawn_blocking(move || -> Result<Vec<CompletionItem>> {
            Ok(collect_include_candidates_with_table(
                form,
                &dir_part,
                &seg,
                current_dir.as_deref(),
                workspace_root.as_deref(),
                &include_roots,
                db_path.as_deref(),
                include_table.as_deref(),
                Some(&external_cache),
                limit,
            ))
        })
        .await;
        let total_ms = started.elapsed().as_millis();
        self.perf_log(|| {
            format!(
                "[perf] include_completion total={}ms workspace_table={} workspace_index={}",
                total_ms,
                if hit_memory { "memory" } else { "unavailable" },
                if hit_db { "available" } else { "unavailable" }
            )
        })
        .await;

        match self.unwrap_query("include completion", result).await {
            Some(items) if !items.is_empty() => Ok(Some(CompletionResponse::Array(items))),
            // Stay incomplete so the editor re-queries as the path is typed.
            _ => Ok(Some(empty_completion_list(true))),
        }
    }

    /// Text of an open buffer, falling back to the file on disk.
    async fn document_text(&self, uri: &Url) -> Option<String> {
        self.document_snapshot(uri).await.map(|(_, text)| text)
    }

    /// Version + text of an open buffer, read under one lock so live-parse cache
    /// entries cannot pair an old text snapshot with a newer LSP version.
    async fn document_snapshot(&self, uri: &Url) -> Option<(i32, String)> {
        if let Some((version, text)) = self.open_docs.lock().await.get(uri) {
            return Some((*version, text.clone()));
        }
        uri_to_path(uri)
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| (0, text))
    }

    async fn local_words_for(&self, uri: &Url, version: i32, text: &str) -> Arc<HashSet<String>> {
        local_words_for_cache(&self.local_word_cache, uri, version, text).await
    }

    /// Return a parsed `FileSemanticIndex` for an open document, using an
    /// in-memory cache keyed by `(uri, version)`. The cache stores the full
    /// (all-facts) parse result so that multiple request types (semantic
    /// tokens, member completion, document symbols) for the same version
    /// share a single parse. Parsing is spawned on the blocking thread-pool.
    async fn get_or_parse_document(
        &self,
        uri: &Url,
        path: &Path,
        version: i32,
        text: &str,
    ) -> Option<Arc<FileSemanticIndex>> {
        if version == 0 {
            let path_owned = path.to_path_buf();
            let text_owned = text.to_string();
            return tokio::task::spawn_blocking(move || {
                Arc::new(parser::parse(&path_owned, &text_owned))
            })
            .await
            .ok();
        }

        // Fast path: cached entry with matching version.
        {
            let cache = self.live_parse_cache.read().await;
            if let Some((v, cached)) = cache.get(uri) {
                if *v == version {
                    self.perf_log(|| format!("[perf] live_parse_cache hit {uri} (v{version})"))
                        .await;
                    return Some(cached.clone());
                }
            }
        }

        // Cache miss: parse on the blocking thread-pool and store.
        self.perf_log(|| format!("[perf] live_parse_cache miss {uri} (v{version})"))
            .await;
        let path_owned = path.to_path_buf();
        let text_owned = text.to_string();
        let index =
            tokio::task::spawn_blocking(move || Arc::new(parser::parse(&path_owned, &text_owned)))
                .await
                .ok()?;

        self.live_parse_cache
            .write()
            .await
            .insert(uri.clone(), (version, index.clone()));
        Some(index)
    }

    /// Compute the limited include-reachability scope for `uri`: the current
    /// file's workspace-relative path plus its bounded reachable set. Returns
    /// `None` when scoping is disabled, no graph exists yet, or the path cannot
    /// be resolved — callers then fall back to whole-index behavior.
    async fn reach_scope_for(&self, uri: &Url) -> Option<(String, Arc<reachability::ReachScope>)> {
        if !self.scoping_enabled.load(Ordering::Relaxed) {
            return None;
        }
        let root = self.root_for_uri(uri).await?;
        let path = uri_to_path(uri)?;
        let rel = pathing::relative_slash_path(&root, &path).ok()?;
        let graph = self.reach_graphs.lock().await.get(&root).cloned()?;
        let scope = graph.read().ok()?.reachable(&rel);
        Some((rel, scope))
    }

    /// Workspace root containing `uri`, falling back to the first root.
    async fn root_for_uri(&self, uri: &Url) -> Option<PathBuf> {
        let roots = self.workspace_roots.lock().await;
        let path = uri_to_path(uri);
        path.as_ref()
            .and_then(|path| roots.iter().find(|root| path.starts_with(root)).cloned())
            .or_else(|| roots.first().cloned())
    }

    /// Flatten a `spawn_blocking` query result, logging any failure.
    async fn unwrap_query<T>(
        &self,
        what: &str,
        result: std::result::Result<Result<T>, tokio::task::JoinError>,
    ) -> Option<T> {
        match result {
            Ok(Ok(value)) => Some(value),
            Ok(Err(err)) => {
                self.client
                    .log_message(
                        MessageType::ERROR,
                        query_error_log_line(what, "query", &format!("{err:#}")),
                    )
                    .await;
                None
            }
            Err(err) => {
                self.client
                    .log_message(
                        MessageType::ERROR,
                        query_error_log_line(what, "task", &err.to_string()),
                    )
                    .await;
                None
            }
        }
    }

    async fn perf_log(&self, build_message: impl FnOnce() -> String) {
        emit_perf_log(
            &self.client,
            self.perf_logging_enabled.load(Ordering::Relaxed),
            build_message,
        )
        .await;
    }
}

fn query_error_log_line(what: &str, kind: &str, detail: &str) -> String {
    format!(
        "FS_QUERY_ERROR kind={} what={} detail={}",
        stable_log_value(kind),
        stable_log_value(what),
        detail.replace(['\r', '\n'], " ")
    )
}

fn stable_log_value(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '/' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

async fn emit_perf_log(client: &Client, enabled: bool, build_message: impl FnOnce() -> String) {
    if enabled {
        client.log_message(MessageType::LOG, build_message()).await;
    }
}

async fn local_words_for_cache(
    cache: &LocalWordCache,
    uri: &Url,
    version: i32,
    text: &str,
) -> Arc<HashSet<String>> {
    {
        let cache_guard = cache.lock().await;
        if let Some((cached_version, words)) = cache_guard.get(uri) {
            if *cached_version == version {
                return words.clone();
            }
        }
    }

    let words = Arc::new(completion_words::extract_words(text));
    cache
        .lock()
        .await
        .insert(uri.clone(), (version, words.clone()));
    words
}

fn workspace_roots_from_initialize(params: &InitializeParams) -> Vec<PathBuf> {
    if let Some(folders) = &params.workspace_folders {
        let roots: Vec<PathBuf> = folders
            .iter()
            .filter_map(|folder| uri_to_path(&folder.uri))
            .collect();

        if !roots.is_empty() {
            return roots;
        }
    }

    params
        .root_uri
        .as_ref()
        .and_then(uri_to_path)
        .into_iter()
        .collect()
}

fn uri_to_path(uri: &Url) -> Option<PathBuf> {
    uri.to_file_path().ok()
}

#[cfg(test)]
mod tests;
