use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::request::{GotoDeclarationParams, GotoDeclarationResponse};
use tower_lsp::lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyIncomingCallsParams, CallHierarchyItem,
    CallHierarchyOutgoingCall, CallHierarchyOutgoingCallsParams, CallHierarchyPrepareParams,
    CallHierarchyServerCapability, Command, CompletionItem, CompletionItemKind, CompletionList,
    CompletionOptions, CompletionParams, CompletionResponse, DeclarationCapability,
    DidChangeTextDocumentParams, DidChangeWatchedFilesParams, DidChangeWorkspaceFoldersParams,
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

use crate::call_model::SemanticGeneration;
use crate::completion::ordinary_service::{
    OrdinaryCompletionDocumentationTarget, OrdinaryCompletionInput, OrdinaryCompletionItem,
    OrdinaryCompletionKind, OrdinaryCompletionNameTable,
};
use crate::completion::{self, CandidateEvidence};
use crate::completion_history::{
    candidate_hash, candidate_hash_key_from_hex, CompletionAcceptEvent, CompletionHistoryMode,
    CompletionHistorySnapshot, CompletionHistoryStore,
};
use crate::config::WorkspaceConfig;
use crate::includes::{self, IncludeForm};
use crate::parser::{self, FileSemanticIndex};
use crate::pathing;
use crate::project_context::ProjectContextSelection;
use crate::query::{self, NameTable};
use crate::reachability;
use crate::references;
use crate::store::IndexStore;

mod call_hierarchy;
mod candidate_context;
mod completion_documentation;
mod hover;
mod include_completion;
mod indexing;
mod language_server;
mod lsp_adapters;
mod member_completion;
mod navigation;
mod options;
mod possible_targets;
mod project_context_commands;
mod semantic_tokens;
mod signature_help;
mod state;
mod workspace;

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
use navigation::NavigationOperation;
use options::{
    candidate_reason_log_lines, completion_trigger_characters, empty_completion_list,
    member_completion_is_incomplete, parse_completion_history_mode, parse_completion_mode,
    parse_completion_prefix_ranking, parse_debug_candidate_reasons, parse_debug_perf_logs,
    parse_include_paths, parse_include_scoping_enabled, parse_initial_project_context_selection,
    parse_semantic_coloring_mode, signature_help_options,
};
use workspace::{
    CacheLedger, CachePublishReport, DocumentRequestSnapshot, DocumentStore, RequestContext,
    RequestSettings, WorkspaceSession,
};

type LocalWordEntry = (i32, Arc<HashSet<String>>);
type LocalWordCache = Arc<Mutex<HashMap<Url, LocalWordEntry>>>;
type IndexSchedule = Arc<Mutex<IndexScheduleState>>;

/// Privacy-safe aggregate diagnostics for semantic candidate requests. This
/// deliberately carries no symbol, signature, path, URI, or source text.
#[derive(Clone, Copy, Debug, Default)]
struct SemanticRequestPerf {
    candidates: query::CallableCandidateMetrics,
    returned: usize,
    hydration_count: usize,
    hydration_bytes: usize,
    query_us: u128,
    hydration_us: u128,
    reach_us: u128,
    coverage_open: bool,
    coverage_truncated: bool,
    coverage_incomplete: bool,
    coverage_reason: u8,
    arity_fallback: bool,
}

impl SemanticRequestPerf {
    fn from_callable_set(set: &query::CallableCandidateSet) -> Self {
        Self {
            candidates: set.metrics(),
            coverage_open: set.coverage.scope_open,
            coverage_truncated: set.coverage.truncated,
            coverage_incomplete: set.coverage.incomplete_reason.is_some(),
            coverage_reason: coverage_reason_code(set.coverage.incomplete_reason),
            arity_fallback: set.arity_mismatch_fallback,
            ..Self::default()
        }
    }

    fn log_line(self, feature: &'static str, total_us: u128) -> String {
        format!(
            "[perf] semantic_candidates feature={feature} total_us={total_us} query_us={} reach_us={} hydration_us={} raw={} filtered={} grouped={} returned={} arity_compatible={} arity_unknown={} arity_incompatible={} counterpart_strict={} counterpart_ambiguous={} hydration_count={} hydration_bytes={} coverage_open={} coverage_truncated={} coverage_incomplete={} coverage_reason={} arity_fallback={}",
            self.query_us,
            self.reach_us,
            self.hydration_us,
            self.candidates.raw_candidates,
            self.candidates.filtered_candidates,
            self.candidates.grouped_candidates,
            self.returned,
            self.candidates.arity_compatible,
            self.candidates.arity_unknown,
            self.candidates.arity_incompatible,
            self.candidates.counterpart_strict,
            self.candidates.counterpart_ambiguous,
            self.hydration_count,
            self.hydration_bytes,
            self.coverage_open as u8,
            self.coverage_truncated as u8,
            self.coverage_incomplete as u8,
            self.coverage_reason,
            self.arity_fallback as u8,
        )
    }

    fn include_type_candidates(&mut self, bundle: &crate::candidate_service::TypeCandidateBundle) {
        let coverage = if bundle.records.coverage.scanned >= bundle.aliases.coverage.scanned {
            &bundle.records.coverage
        } else {
            &bundle.aliases.coverage
        };
        self.candidates.raw_candidates = self
            .candidates
            .raw_candidates
            .saturating_add(coverage.scanned);
        self.candidates.filtered_candidates = self
            .candidates
            .filtered_candidates
            .saturating_add(bundle.records.candidates.len())
            .saturating_add(bundle.aliases.candidates.len());
        self.candidates.grouped_candidates = self
            .candidates
            .grouped_candidates
            .saturating_add(bundle.records.candidates.len())
            .saturating_add(bundle.alias_resolutions.len());
        self.coverage_open |= coverage.scope_open;
        self.coverage_truncated |= coverage.truncated;
        self.coverage_incomplete |= coverage.incomplete_reason.is_some();
        if self.coverage_reason == 0 {
            self.coverage_reason = coverage_reason_code(coverage.incomplete_reason);
        }
    }

    fn include_non_callable_candidates(&mut self, count: usize) {
        self.candidates.raw_candidates = self.candidates.raw_candidates.saturating_add(count);
        self.candidates.filtered_candidates =
            self.candidates.filtered_candidates.saturating_add(count);
        self.candidates.grouped_candidates =
            self.candidates.grouped_candidates.saturating_add(count);
    }
}

fn coverage_reason_code(reason: Option<query::CandidateIncompleteReason>) -> u8 {
    match reason {
        None => 0,
        Some(query::CandidateIncompleteReason::ScanLimit) => 1,
        Some(query::CandidateIncompleteReason::CandidateBudget) => 2,
        Some(query::CandidateIncompleteReason::TimeBudget) => 3,
        Some(query::CandidateIncompleteReason::Cancelled) => 4,
        Some(query::CandidateIncompleteReason::FactsUnavailable) => 5,
        Some(query::CandidateIncompleteReason::GenerationMismatch) => 6,
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct HydrationStats {
    count: usize,
    bytes: usize,
}

#[derive(Clone, Copy)]
enum LiveParseCacheEvent {
    Hit,
    Coalesced,
    Miss,
}

fn live_parse_cache_log(event: LiveParseCacheEvent) -> &'static str {
    match event {
        LiveParseCacheEvent::Hit => "[perf] live_parse_cache state=hit",
        LiveParseCacheEvent::Coalesced => "[perf] live_parse_cache state=coalesced",
        LiveParseCacheEvent::Miss => "[perf] live_parse_cache state=miss",
    }
}

impl HydrationStats {
    fn record(&mut self, source: Option<&str>) {
        if let Some(source) = source {
            self.count += 1;
            self.bytes = self.bytes.saturating_add(source.len());
        }
    }
}

fn apply_final_completion_sort_text(items: &mut [CompletionItem]) {
    for (index, item) in items.iter_mut().enumerate() {
        item.sort_text = Some(format!("{index:08}"));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum CompletionDocumentationData {
    Indexed {
        version: u8,
        root: String,
        uri: String,
        label: String,
        symbol_id: i64,
        semantic_generation: u64,
        overlay_epoch: u64,
        document_version: i32,
    },
    CurrentDocument {
        version: u8,
        uri: String,
        start_line: u32,
        overlay_epoch: u64,
        document_version: i32,
    },
    Overlay {
        version: u8,
        root: String,
        uri: String,
        label: String,
        path: String,
        start_line: u32,
        start_col: u32,
        end_line: u32,
        end_col: u32,
        semantic_generation: u64,
        overlay_epoch: u64,
        document_version: i32,
    },
    Member {
        version: u8,
        uri: String,
        owner_path: String,
        signature: String,
        owner_revision_hash: String,
        overlay_epoch: u64,
        document_version: i32,
    },
}

fn ordinary_completion_item_to_lsp(
    item: OrdinaryCompletionItem,
    uri: &Url,
    table_roots: &[PathBuf],
    table_semantic_generations: &[SemanticGeneration],
    overlay_names: &[HashMap<i64, crate::candidate_service::OverlayCompletionName>],
    overlay_epoch: u64,
    document_version: i32,
) -> CompletionItem {
    let documentation_label = item.label.clone();
    let data = item.documentation_target.and_then(|target| {
        let target = match target {
            OrdinaryCompletionDocumentationTarget::Indexed {
                table_index,
                symbol_id,
            } => match overlay_names
                .get(table_index)
                .and_then(|locators| locators.get(&symbol_id))
            {
                Some(locator) => CompletionDocumentationData::Overlay {
                    version: 3,
                    root: table_roots.get(table_index)?.to_string_lossy().into_owned(),
                    uri: uri.to_string(),
                    label: documentation_label.clone(),
                    path: locator.path.clone(),
                    start_line: locator.start_line,
                    start_col: locator.start_col,
                    end_line: locator.end_line,
                    end_col: locator.end_col,
                    semantic_generation: table_semantic_generations.get(table_index)?.0,
                    overlay_epoch,
                    document_version,
                },
                None => CompletionDocumentationData::Indexed {
                    version: 3,
                    root: table_roots.get(table_index)?.to_string_lossy().into_owned(),
                    uri: uri.to_string(),
                    label: documentation_label.clone(),
                    symbol_id,
                    semantic_generation: table_semantic_generations.get(table_index)?.0,
                    overlay_epoch,
                    document_version,
                },
            },
            OrdinaryCompletionDocumentationTarget::CurrentDocument { start_line } => {
                CompletionDocumentationData::CurrentDocument {
                    version: 3,
                    uri: uri.to_string(),
                    start_line,
                    overlay_epoch,
                    document_version,
                }
            }
        };
        serde_json::to_value(target).ok()
    });
    CompletionItem {
        label: item.label,
        kind: Some(ordinary_completion_kind_to_lsp(item.kind)),
        detail: item.detail,
        documentation: item.documentation.map(Documentation::String),
        sort_text: item.initial_sort_text,
        data,
        ..Default::default()
    }
}

fn ordinary_completion_kind_to_lsp(kind: OrdinaryCompletionKind) -> CompletionItemKind {
    match kind {
        OrdinaryCompletionKind::Text => CompletionItemKind::TEXT,
        OrdinaryCompletionKind::Keyword => CompletionItemKind::KEYWORD,
        OrdinaryCompletionKind::Function => CompletionItemKind::FUNCTION,
        OrdinaryCompletionKind::Macro => CompletionItemKind::CONSTANT,
        OrdinaryCompletionKind::Type => CompletionItemKind::STRUCT,
        OrdinaryCompletionKind::Variable => CompletionItemKind::VARIABLE,
        OrdinaryCompletionKind::EnumConstant => CompletionItemKind::ENUM_MEMBER,
    }
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
const POSSIBLE_TARGETS_LSP_COMMAND: &str = "fossilsense.lsp.possibleTargets";
const PROJECT_CONTEXTS_LSP_COMMAND: &str = "fossilsense.lsp.projectContexts";
const SET_PROJECT_CONTEXT_LSP_COMMAND: &str = "fossilsense.lsp.setProjectContext";
const CALL_RELATIONS_LSP_COMMAND: &str = "fossilsense.lsp.callRelations";

pub async fn run_stdio() -> Result<()> {
    eprintln!("FossilSense LSP starting on stdio");

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(|client| Backend {
        client,
        workspace_roots: Arc::new(Mutex::new(Vec::new())),
        index_schedule: Arc::new(Mutex::new(IndexScheduleState::default())),
        session: WorkspaceSession::new(DocumentStore::default(), CacheLedger::default()),
        external_include_dir_cache: Arc::new(StdMutex::new(HashMap::new())),
        include_paths: Arc::new(Mutex::new(Vec::new())),
        completion_enabled: AtomicBool::new(true),
        strict_prefix_ranking: AtomicBool::new(true),
        semantic_coloring_enabled: AtomicBool::new(true),
        scoping_enabled: AtomicBool::new(true),
        completion_history_mode: Arc::new(Mutex::new(CompletionHistoryMode::Auto)),
        completion_history: Arc::new(Mutex::new(HashMap::new())),
        completion_history_write_gate: Arc::new(Mutex::new(())),
        project_context_selection: Arc::new(Mutex::new(ProjectContextSelection::Auto)),
        project_context_selection_epoch: AtomicU64::new(1),
        debug_candidate_reasons: AtomicBool::new(false),
        perf_logging_enabled: AtomicBool::new(false),
        config_cache: Arc::new(Mutex::new(HashMap::new())),
    });

    Server::new(stdin, stdout, socket).serve(service).await;
    Ok(())
}

struct Backend {
    client: Client,
    workspace_roots: Arc<Mutex<Vec<PathBuf>>>,
    index_schedule: IndexSchedule,
    /// Facade for live documents, read models, cache invalidation, and request snapshots.
    session: WorkspaceSession,
    /// Directory-listing cache for configured external include roots.
    external_include_dir_cache: ExternalIncludeDirCache,
    /// External include reference directories (normalized) forwarded from the
    /// client; used for indexing, include-path completion, and jump-to-header.
    include_paths: Arc<Mutex<Vec<String>>>,
    /// Whether completion is enabled (based on initializationOptions).
    completion_enabled: AtomicBool,
    /// Whether ordinary identifier completion guards exact/literal-prefix
    /// matches above all fuzzy matches. False preserves legacy scope-first
    /// ranking.
    strict_prefix_ranking: AtomicBool,
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
    /// Serializes atomic history-file replacements without holding the store map.
    completion_history_write_gate: Arc<Mutex<()>>,
    /// User-selected completion project policy. The extension persists the
    /// choice; the server validates it against current immutable snapshots.
    project_context_selection: Arc<Mutex<ProjectContextSelection>>,
    /// Changes whenever selection changes so completion memo pools cannot be
    /// reused under a different effective project.
    project_context_selection_epoch: AtomicU64,
    /// Whether goto-definition logs each candidate's scope reasoning
    /// (tier/confidence/reason) to the output panel. Off by default; gated by
    /// `fossilsense.debug.candidateReasons`. A debug aid, not a user contract.
    debug_candidate_reasons: AtomicBool,
    /// Whether `[perf]` request/index timings are sent to the output panel.
    /// Off by default; enabled by `RUST_LOG` debug/trace or client init options.
    perf_logging_enabled: AtomicBool,
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
        let semantic_generation = match &workspace_root {
            Some(root) => Some(
                self.request_context_for_root(root.clone())
                    .await
                    .engine
                    .semantic_generation
                    .0,
            ),
            None => None,
        };
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
                semantic_generation,
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
        let request_context = match &workspace_root {
            Some(root) => Some(self.request_context_for_root(root.clone()).await),
            None => None,
        };
        let include_table = request_context
            .as_ref()
            .and_then(|context| context.engine.include_table.clone());
        let semantic_generation = request_context
            .as_ref()
            .map(|context| context.engine.semantic_generation.0);
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
                    semantic_generation,
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
    async fn document_text(&self, uri: &Url) -> Option<Arc<str>> {
        self.document_snapshot(uri).await.map(|(_, text)| text)
    }

    /// Version + text of an open buffer, read under one lock so live-parse cache
    /// entries cannot pair an old text snapshot with a newer LSP version.
    async fn document_snapshot(&self, uri: &Url) -> Option<(i32, Arc<str>)> {
        if let Some(snapshot) = self.session.documents.snapshot(uri).await {
            return Some((snapshot.version, snapshot.text));
        }
        self.disk_document_snapshot(uri).await
    }

    /// Resolve current text from a caller-owned atomic all-document capture.
    /// Closed-document disk fallback is isolated on the blocking pool so no
    /// semantic handler performs filesystem I/O on Tokio's async workers.
    async fn document_snapshot_from_request(
        &self,
        uri: &Url,
        request: &DocumentRequestSnapshot,
    ) -> Option<(i32, Arc<str>)> {
        if let Some(snapshot) = &request.current {
            return Some((snapshot.version, snapshot.text.clone()));
        }
        self.disk_document_snapshot(uri).await
    }

    async fn disk_document_snapshot(&self, uri: &Url) -> Option<(i32, Arc<str>)> {
        let path = uri_to_path(uri)?;
        tokio::task::spawn_blocking(move || std::fs::read_to_string(path))
            .await
            .ok()?
            .ok()
            .map(|text| (0, Arc::from(text)))
    }

    async fn local_words_for(&self, uri: &Url, version: i32, text: &str) -> Arc<HashSet<String>> {
        self.session
            .documents
            .local_words_for(uri, version, text)
            .await
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
        requested_facts: parser::ParseFacts,
    ) -> Option<Arc<FileSemanticIndex>> {
        if version == 0 {
            let path_owned = path.to_path_buf();
            let text_owned = text.to_string();
            return tokio::task::spawn_blocking(move || {
                Arc::new(parser::parse_thread_local_with_facts(
                    &path_owned,
                    &text_owned,
                    requested_facts,
                ))
            })
            .await
            .ok();
        }

        // Fast path: cached entry with matching version.
        if let Some(cached) = self
            .session
            .documents
            .cached_live_parse(uri, version, requested_facts)
            .await
        {
            self.perf_log(|| live_parse_cache_log(LiveParseCacheEvent::Hit).to_string())
                .await;
            return Some(cached);
        }

        // Coalesce concurrent semantic-token/completion/symbol requests for
        // the same open document. The second waiter rechecks after acquiring
        // the gate and reuses the first parse instead of duplicating all-facts
        // parsing on the blocking pool.
        let parse_gate = self.session.documents.live_parse_gate(uri).await;
        let _parse_guard = parse_gate.lock().await;
        if let Some(cached) = self
            .session
            .documents
            .cached_live_parse(uri, version, requested_facts)
            .await
        {
            self.perf_log(|| live_parse_cache_log(LiveParseCacheEvent::Coalesced).to_string())
                .await;
            return Some(cached);
        }

        // Cache miss: parse on the blocking thread-pool and store.
        self.perf_log(|| live_parse_cache_log(LiveParseCacheEvent::Miss).to_string())
            .await;
        let path_owned = path.to_path_buf();
        let text_owned = text.to_string();
        let facts = self
            .session
            .documents
            .cached_live_parse_facts(uri, version)
            .await
            | requested_facts;
        let cancellation = self
            .session
            .documents
            .live_parse_cancellation(uri, version)
            .await;
        let parse_cancellation = cancellation.clone();
        let index = tokio::task::spawn_blocking(move || {
            Arc::new(parser::parse_thread_local_with_facts_cancel(
                &path_owned,
                &text_owned,
                facts,
                &parse_cancellation,
            ))
        })
        .await
        .ok()?;
        if cancellation.load(Ordering::Relaxed) {
            return None;
        }

        self.session
            .documents
            .store_live_parse(uri.clone(), version, facts, index.clone())
            .await;
        Some(index)
    }

    fn reach_scope_from_context(
        &self,
        uri: &Url,
        context: &RequestContext,
    ) -> Option<(String, Arc<reachability::ReachScope>)> {
        let path = uri_to_path(uri)?;
        if !context.settings.scoping_enabled {
            return None;
        }
        let rel = pathing::relative_slash_path(&context.engine.root, &path).ok()?;
        let graph = context.engine.reach_graph.clone()?;
        let scope = graph.reachable(&rel);
        Some((rel, scope))
    }

    fn request_settings(&self) -> RequestSettings {
        RequestSettings {
            completion_enabled: self.completion_enabled.load(Ordering::Relaxed),
            prefix_ranking: if self.strict_prefix_ranking.load(Ordering::Relaxed) {
                completion::CompletionPrefixRanking::Strict
            } else {
                completion::CompletionPrefixRanking::ScopeFirst
            },
            semantic_coloring_enabled: self.semantic_coloring_enabled.load(Ordering::Relaxed),
            scoping_enabled: self.scoping_enabled.load(Ordering::Relaxed),
            perf_logging_enabled: self.perf_logging_enabled.load(Ordering::Relaxed),
        }
    }

    async fn request_context_for_root(&self, root: PathBuf) -> RequestContext {
        self.session
            .request_context_for_root_with_settings(root, self.request_settings())
            .await
    }

    async fn request_context_for_uri(&self, uri: &Url) -> Option<RequestContext> {
        let root = self.root_for_uri(uri).await?;
        Some(self.request_context_for_root(root).await)
    }

    /// Most-specific workspace root containing `uri`, falling back to the first
    /// root for the legacy outside-workspace behavior.
    async fn root_for_uri(&self, uri: &Url) -> Option<PathBuf> {
        let roots = self.workspace_roots.lock().await;
        let path = uri_to_path(uri);
        path.as_ref()
            .and_then(|path| {
                roots
                    .iter()
                    .filter(|root| pathing::path_is_within(root, path))
                    .max_by_key(|root| root.components().count())
                    .cloned()
            })
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
            self.request_settings().perf_logging_enabled,
            build_message,
        )
        .await;
    }

    async fn preload_completion_history(&self) {
        if !self.completion_history_mode.lock().await.is_enabled() {
            return;
        }
        let roots = self.workspace_roots.lock().await.clone();
        let history_paths: Vec<PathBuf> = roots
            .iter()
            .filter_map(|root| pathing::default_completion_history_path(root).ok())
            .collect();
        if history_paths.is_empty() {
            return;
        }

        let loaded =
            tokio::task::spawn_blocking(move || -> Vec<(PathBuf, CompletionHistoryStore)> {
                history_paths
                    .into_iter()
                    .filter_map(|path| {
                        CompletionHistoryStore::open(&path)
                            .ok()
                            .map(|store| (path, store))
                    })
                    .collect()
            })
            .await
            .unwrap_or_default();
        if loaded.is_empty() {
            return;
        }

        let mut stores = self.completion_history.lock().await;
        for (path, store) in loaded {
            stores.entry(path).or_insert(store);
        }
    }

    async fn record_completion_accept(&self, event: CompletionAcceptEvent) -> Result<()> {
        if !self.completion_history_mode.lock().await.is_enabled() {
            return Ok(());
        }

        let roots = self.workspace_roots.lock().await.clone();
        let Some(root) = roots
            .into_iter()
            .find(|root| completion_history_workspace_hash(root) == event.workspace_hash)
        else {
            return Ok(());
        };
        let history_path = pathing::default_completion_history_path(&root)?;
        if !self
            .completion_history
            .lock()
            .await
            .contains_key(&history_path)
        {
            let open_path = history_path.clone();
            let opened =
                tokio::task::spawn_blocking(move || CompletionHistoryStore::open(&open_path))
                    .await??;
            self.completion_history
                .lock()
                .await
                .entry(history_path.clone())
                .or_insert(opened);
        }
        let write = self
            .completion_history
            .lock()
            .await
            .get_mut(&history_path)
            .expect("history store inserted")
            .record_accept_deferred(event)?;
        let _write_guard = self.completion_history_write_gate.lock().await;
        tokio::task::spawn_blocking(move || write.persist()).await??;
        Ok(())
    }

    async fn clear_completion_history(&self) -> Result<usize> {
        let roots = self.workspace_roots.lock().await.clone();
        let mut removed = 0usize;
        let mut writes = Vec::new();

        if roots.is_empty() {
            let mut stores = self.completion_history.lock().await;
            for store in stores.values_mut() {
                let (count, write) = store.clear_all_deferred()?;
                removed += count;
                writes.push(write);
            }
        } else {
            for root in roots {
                let history_path = pathing::default_completion_history_path(&root)?;
                if !self
                    .completion_history
                    .lock()
                    .await
                    .contains_key(&history_path)
                {
                    let open_path = history_path.clone();
                    let store = tokio::task::spawn_blocking(move || {
                        CompletionHistoryStore::open(&open_path)
                            .unwrap_or_else(|_| CompletionHistoryStore::empty(&open_path))
                    })
                    .await?;
                    self.completion_history
                        .lock()
                        .await
                        .entry(history_path.clone())
                        .or_insert(store);
                }
                if let Some(store) = self.completion_history.lock().await.get_mut(&history_path) {
                    let (count, write) = store.clear_all_deferred()?;
                    removed += count;
                    writes.push(write);
                }
            }
        }
        let _write_guard = self.completion_history_write_gate.lock().await;
        tokio::task::spawn_blocking(move || -> Result<()> {
            for write in writes {
                write.persist()?;
            }
            Ok(())
        })
        .await??;
        Ok(removed)
    }

    async fn completion_history_snapshot_for_root(
        &self,
        root: &Path,
        workspace_hash: &str,
    ) -> Result<CompletionHistorySnapshot> {
        let history_path = pathing::default_completion_history_path(root)?;
        let stores = self.completion_history.lock().await;
        Ok(stores
            .get(&history_path)
            .map(|store| store.snapshot(workspace_hash))
            .unwrap_or_default())
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
    let candidate_hash = candidate_hash_field(arg, "candidateHash")?;
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

fn candidate_hash_field(value: &Value, field: &str) -> Option<String> {
    let hash = non_empty_string_field(value, field)?;
    candidate_hash_key_from_hex(&hash)?;
    Some(hash.to_ascii_lowercase())
}

fn completion_history_workspace_hash(root: &Path) -> String {
    pathing::canonical_workspace(root)
        .map(|workspace| pathing::workspace_hash(&workspace))
        .unwrap_or_else(|_| pathing::workspace_hash(root))
}

fn attach_completion_history_accept_command(
    item: &mut CompletionItem,
    evidence: CandidateEvidence,
    workspace_hash: &str,
    intent: completion::CompletionIntentKind,
    prefix_bucket: &str,
) {
    if evidence.history_key.is_none() {
        return;
    }
    let kind = evidence.kind.as_history_kind_str();
    item.command = Some(Command {
        title: "FossilSense completion accepted".to_string(),
        command: COMPLETION_ACCEPTED_LSP_COMMAND.to_string(),
        arguments: Some(vec![serde_json::json!({
            "workspaceHash": workspace_hash,
            "candidateHash": candidate_hash(&item.label, kind),
            "kind": kind,
            "intent": intent.as_summary_str(),
            "prefixBucket": prefix_bucket,
        })]),
    });
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

#[cfg(test)]
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

    let words = Arc::new(crate::completion_words::extract_words(text));
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
