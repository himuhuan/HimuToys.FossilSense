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

use crate::completion::{self, CandidateEvidence, CandidateSource, CompletionCandidateKind};
#[cfg(test)]
use crate::completion_history::CompletionHistorySnapshot;
use crate::completion_history::{
    CompletionAcceptEvent, CompletionHistoryMode, CompletionHistoryStore,
};
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
    collect_include_candidates_with_table_and_evidence, configured_include_paths,
    location_at_file_start, resolve_include_paths, CurrentIncludeEvidence, ExternalIncludeDirCache,
    IncludeCompletionTable,
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
    member_completion_is_incomplete, parse_completion_history_mode, parse_completion_mode,
    parse_debug_candidate_reasons, parse_debug_perf_logs, parse_include_paths,
    parse_include_scoping_enabled, parse_semantic_coloring_mode, signature_help_options,
};

type NameTables = Arc<Mutex<HashMap<PathBuf, Arc<NameTable>>>>;
type ReachGraphs = Arc<Mutex<HashMap<PathBuf, Arc<StdRwLock<ReachGraph>>>>>;
type IncludeTables = Arc<Mutex<HashMap<PathBuf, Arc<IncludeCompletionTable>>>>;
type IndexedFileLists = Arc<Mutex<HashMap<PathBuf, Arc<Vec<(String, PathBuf)>>>>>;
type IndexGenerations = Arc<Mutex<HashMap<PathBuf, state::WorkspaceGeneration>>>;
type LocalWordEntry = (i32, Arc<HashSet<String>>);
type LocalWordCache = Arc<Mutex<HashMap<Url, LocalWordEntry>>>;
type IndexSchedule = Arc<Mutex<IndexScheduleState>>;

type CompletionCandidate = completion::PipelineCandidate<CompletionItem>;

fn completion_candidate_kind_from_parser(kind: parser::SymbolKind) -> CompletionCandidateKind {
    match kind {
        parser::SymbolKind::Function => CompletionCandidateKind::Function,
        parser::SymbolKind::Macro => CompletionCandidateKind::Macro,
        parser::SymbolKind::Type => CompletionCandidateKind::Type,
        parser::SymbolKind::EnumConstant => CompletionCandidateKind::EnumConstant,
        parser::SymbolKind::GlobalVariable | parser::SymbolKind::Field => {
            CompletionCandidateKind::Variable
        }
    }
}

fn completion_items_for_local_bindings(
    hits: Vec<query::LocalCompletionCandidate>,
) -> Vec<CompletionCandidate> {
    hits.into_iter()
        .map(|hit| {
            let mut evidence = CandidateEvidence::new(
                CandidateSource::LocalBinding,
                model::ScopeTier::Current,
                model::ResolutionConfidence::Heuristic,
                hit.score,
            );
            evidence.match_score = hit.match_score;
            evidence.kind = CompletionCandidateKind::Variable;
            CompletionCandidate::new(
                hit.name.clone(),
                evidence,
                CompletionItem {
                    label: hit.name,
                    kind: Some(CompletionItemKind::VARIABLE),
                    detail: Some(hit.detail),
                    sort_text: Some(format!("{:08}", 100_000_000 - hit.score)),
                    ..Default::default()
                },
            )
        })
        .collect()
}

fn completion_items_for_current_file_overlay(
    hits: Vec<query::CurrentFileOverlayCandidate>,
) -> Vec<CompletionCandidate> {
    hits.into_iter()
        .map(|hit| {
            let is_text = !hit.semantic || hit.detail.as_deref() == Some("text");
            let source = if is_text {
                CandidateSource::LocalWord
            } else {
                CandidateSource::CurrentFileOverlay
            };
            let tier = if is_text {
                model::ScopeTier::Global
            } else {
                model::ScopeTier::Current
            };
            let confidence = if is_text {
                model::ResolutionConfidence::Fallback
            } else {
                model::ResolutionConfidence::Heuristic
            };
            let kind = if is_text {
                CompletionItemKind::TEXT
            } else {
                query::lsp_completion_kind_from_parser(hit.kind)
            };
            let mut evidence = CandidateEvidence::new(source, tier, confidence, hit.match_score);
            evidence.match_score = hit.match_score;
            evidence.proximity_score = hit.proximity_score;
            evidence.kind = if is_text {
                CompletionCandidateKind::Text
            } else {
                completion_candidate_kind_from_parser(hit.kind)
            };

            CompletionCandidate::new(
                hit.name.clone(),
                evidence,
                CompletionItem {
                    label: hit.name,
                    kind: Some(kind),
                    detail: hit.detail,
                    ..Default::default()
                },
            )
        })
        .collect()
}

fn completion_items_for_indexed_hits(
    hits: Vec<query::RankedNameHit>,
    open_reason: Option<reachability::OpenReason>,
) -> Vec<CompletionCandidate> {
    hits.into_iter()
        .map(|hit| {
            let (confidence, reason) =
                resolver::confidence_reason_for(hit.tier, false, open_reason);
            let label = model::completion_scope_label(hit.tier, confidence, reason);
            let mut evidence =
                CandidateEvidence::new(CandidateSource::Indexed, hit.tier, confidence, hit.score);
            evidence.match_score = hit.base_match;
            evidence.kind = completion_candidate_kind_from_parser(hit.kind);
            CompletionCandidate::new(
                hit.name.clone(),
                evidence,
                CompletionItem {
                    label: hit.name,
                    kind: Some(query::lsp_completion_kind_from_parser(hit.kind)),
                    sort_text: Some(format!("{:08}", 100_000_000 - hit.score)),
                    detail: label.as_ref().map(|l| l.detail.to_string()),
                    documentation: label.map(|l| Documentation::String(l.documentation)),
                    ..Default::default()
                },
            )
        })
        .collect()
}

fn apply_final_completion_sort_text(items: &mut [CompletionItem]) {
    for (index, item) in items.iter_mut().enumerate() {
        item.sort_text = Some(format!("{index:08}"));
    }
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
            let mut evidence =
                CandidateEvidence::new(CandidateSource::Indexed, hit.tier, confidence, local_score);
            evidence.match_score = hit.base_match;
            evidence.kind = completion_candidate_kind_from_parser(hit.kind);
            CompletionCandidate::new(
                hit.name.clone(),
                evidence,
                CompletionItem {
                    label: hit.name,
                    kind: Some(query::lsp_completion_kind_from_parser(hit.kind)),
                    sort_text: Some(format!("{:08}", 100_000_000 - local_score)),
                    detail: label.as_ref().map(|l| l.detail.to_string()),
                    documentation: label.map(|l| Documentation::String(l.documentation)),
                    ..Default::default()
                },
            )
        })
        .collect()
}

const REFRESH_INDEX_COMMAND: &str = "fossilsense.refreshIndex";
const REFRESH_INDEX_LSP_COMMAND: &str = "fossilsense.lsp.refreshIndex";
const REBUILD_INDEX_COMMAND: &str = "fossilsense.rebuildIndex";
const REBUILD_INDEX_LSP_COMMAND: &str = "fossilsense.lsp.rebuildIndex";
pub(super) const COMPLETION_ACCEPTED_LSP_COMMAND: &str = "fossilsense.lsp.completionAccepted";
pub(super) const CLEAR_COMPLETION_HISTORY_LSP_COMMAND: &str =
    "fossilsense.lsp.clearCompletionHistory";
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
        completion_history_mode: Arc::new(Mutex::new(CompletionHistoryMode::Auto)),
        completion_history: Arc::new(Mutex::new(HashMap::new())),
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
    /// Local-only accepted-completion history mode. `auto` and `on` both record
    /// anonymous positive feedback; `off` keeps deterministic v1.2.0 ranking.
    completion_history_mode: Arc<Mutex<CompletionHistoryMode>>,
    /// Workspace-local completion history stores, keyed by their cache file
    /// path so multi-root workspaces remain separate on disk.
    completion_history: Arc<Mutex<HashMap<PathBuf, CompletionHistoryStore>>>,
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
        text: &str,
    ) -> LspResult<Option<CompletionResponse>> {
        let (dir_part, seg) = includes::split_partial(&partial);
        let current_dir = uri_to_path(uri).and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let client_include_roots = self.include_paths.lock().await.clone();
        let workspace_root = self.root_for_uri(uri).await;
        let current_rel_path = workspace_root.as_ref().and_then(|root| {
            uri_to_path(uri).and_then(|path| pathing::relative_slash_path(root, &path).ok())
        });
        let current_rel_dir = current_rel_path
            .as_deref()
            .and_then(|path| path.rsplit_once('/').map(|(dir, _)| dir.to_string()));
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
        let text = text.to_string();

        let started = tokio::time::Instant::now();
        let hit_memory = include_table.is_some();
        let hit_db = db_path.as_ref().is_some_and(|path| path.exists());
        let result = tokio::task::spawn_blocking(
            move || -> Result<(Vec<CompletionItem>, include_completion::IncludeCompletionMetrics)> {
                let evidence =
                    CurrentIncludeEvidence::from_text(&text, current_rel_path.as_deref());
                Ok(collect_include_candidates_with_table_and_evidence(
                    form,
                    &dir_part,
                    &seg,
                    current_dir.as_deref(),
                    workspace_root.as_deref(),
                    &include_roots,
                    db_path.as_deref(),
                    include_table.as_deref(),
                    Some(&external_cache),
                    current_rel_dir.as_deref(),
                    Some(&evidence),
                    limit,
                ))
            },
        )
        .await;
        let total_ms = started.elapsed().as_millis();
        let metrics = result
            .as_ref()
            .ok()
            .and_then(|inner| inner.as_ref().ok().map(|(_, metrics)| *metrics))
            .unwrap_or_default();
        self.perf_log(|| {
            format!(
                "[perf] include_completion total={}ms workspace_table={} workspace_index={} same_directory={} recent={} sibling={} basename={} depth_penalty={}",
                total_ms,
                if hit_memory { "memory" } else { "unavailable" },
                if hit_db { "available" } else { "unavailable" },
                metrics.same_directory,
                metrics.recent,
                metrics.sibling,
                metrics.basename,
                metrics.depth_penalty,
            )
        })
        .await;

        match self.unwrap_query("include completion", result).await {
            Some((items, _metrics)) if !items.is_empty() => {
                Ok(Some(CompletionResponse::Array(items)))
            }
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

    async fn record_completion_accept(&self, event: CompletionAcceptEvent) -> Result<()> {
        if !self.completion_history_mode.lock().await.is_enabled() {
            return Ok(());
        }

        let Some(root) = self.workspace_roots.lock().await.first().cloned() else {
            return Ok(());
        };
        let history_path = pathing::default_completion_history_path(&root)?;
        let mut stores = self.completion_history.lock().await;
        if !stores.contains_key(&history_path) {
            stores.insert(
                history_path.clone(),
                CompletionHistoryStore::open(&history_path)?,
            );
        }
        if let Some(store) = stores.get_mut(&history_path) {
            store.record_accept(event)?;
        }
        Ok(())
    }

    async fn clear_completion_history(&self) -> Result<usize> {
        let roots = self.workspace_roots.lock().await.clone();
        let mut stores = self.completion_history.lock().await;
        let mut removed = 0usize;

        if roots.is_empty() {
            for store in stores.values_mut() {
                removed += store.clear_all()?;
            }
            return Ok(removed);
        }

        for root in roots {
            let history_path = pathing::default_completion_history_path(&root)?;
            if !stores.contains_key(&history_path) {
                stores.insert(
                    history_path.clone(),
                    CompletionHistoryStore::open(&history_path)?,
                );
            }
            if let Some(store) = stores.get_mut(&history_path) {
                removed += store.clear_all()?;
            }
        }
        Ok(removed)
    }

    #[cfg(test)]
    async fn set_completion_history_mode_for_test(&self, mode: CompletionHistoryMode) {
        *self.completion_history_mode.lock().await = mode;
    }

    #[cfg(test)]
    async fn history_snapshot_for_test(&self, workspace_hash: &str) -> CompletionHistorySnapshot {
        let stores = self.completion_history.lock().await;
        let mut snapshot = CompletionHistorySnapshot::default();
        for store in stores.values() {
            snapshot.append_from(store.snapshot(workspace_hash));
        }
        snapshot
    }
}

fn completion_accept_event_from_arg(arg: Option<&Value>) -> Option<CompletionAcceptEvent> {
    let arg = arg?;
    let workspace_hash = non_empty_string_field(arg, "workspaceHash")?;
    let candidate_hash = non_empty_string_field(arg, "candidateHash")?;
    let kind = non_empty_string_field(arg, "kind")?;
    let intent = non_empty_string_field(arg, "intent")?;
    let prefix_bucket = arg
        .get("prefixBucket")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();

    Some(CompletionAcceptEvent {
        workspace_hash,
        candidate_hash,
        kind,
        intent,
        prefix_bucket,
        accepted_at: crate::completion_history::now_unix_secs(),
    })
}

fn non_empty_string_field(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
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
