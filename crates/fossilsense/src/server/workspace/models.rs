use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::Mutex;
use tower_lsp::lsp_types::Url;

use super::super::go_import_completion::GoImportCompletionTable;
use super::super::include_completion::IncludeCompletionTable;
use super::super::state;
use crate::call_model::SemanticGeneration;
use crate::call_service::CallReadHandle;
use crate::candidate_service::{CandidateOverlaySnapshot, RecallUniverseId};
use crate::declaration_index::SemanticDeclarationIndex;
use crate::project_context::ProjectContextIndex;
use crate::query::NameTable;
use crate::reachability::ReachGraph;
use crate::references;

#[derive(Clone)]
pub(in crate::server) struct CacheLedger {
    pub(in crate::server) engine_snapshots: EngineSnapshots,
    pub(in crate::server) publish_gate: Arc<Mutex<()>>,
    pub(super) next_engine_epoch: Arc<AtomicU64>,
    pub(in crate::server) reference_role_cache: Arc<references::ReferenceRoleCache>,
    pub(in crate::server) reference_search_cache: Arc<references::ReferenceSearchCache>,
    pub(in crate::server) completion_memo: Arc<Mutex<HashMap<Url, state::CompletionMemo>>>,
    pub(super) candidate_overlays: Arc<Mutex<CandidateOverlayCache>>,
    pub(super) semantic_index_memory_budget_bytes: Arc<AtomicU64>,
}

#[derive(Default)]
pub(super) struct CandidateOverlayCache {
    pub(super) entries: HashMap<CandidateOverlayCacheKey, Arc<CandidateOverlaySnapshot>>,
    pub(super) completion_entries: HashMap<CompletionOverlayCacheKey, CompletionOverlayCacheEntry>,
    pub(super) root_revisions: HashMap<PathBuf, u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct CandidateOverlayCacheKey {
    pub(super) root: PathBuf,
    pub(super) semantic_generation: SemanticGeneration,
    pub(super) overlay_epoch: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct CompletionOverlayCacheKey {
    pub(super) root: PathBuf,
    pub(super) engine_epoch: state::EngineEpoch,
    pub(super) semantic_generation: SemanticGeneration,
}

pub(super) struct CompletionOverlayCacheEntry {
    pub(super) universe: RecallUniverseId,
    pub(super) newest_overlay_epoch: u64,
    pub(super) snapshot: Arc<CandidateOverlaySnapshot>,
}

pub(super) fn invalidate_candidate_overlay_root(cache: &mut CandidateOverlayCache, root: &Path) {
    cache.entries.retain(|key, _| key.root != root);
    cache.completion_entries.retain(|key, _| key.root != root);
    let revision = cache.root_revisions.entry(root.to_path_buf()).or_default();
    *revision = revision.wrapping_add(1).max(1);
}

pub(in crate::server) type EngineSnapshots = Arc<Mutex<HashMap<PathBuf, Arc<EngineSnapshot>>>>;

#[derive(Clone)]
pub(in crate::server) struct EngineSnapshot {
    pub(in crate::server) root: PathBuf,
    pub(in crate::server) epoch: state::EngineEpoch,
    pub(in crate::server) semantic_generation: SemanticGeneration,
    pub(in crate::server) declaration_index: Option<Arc<SemanticDeclarationIndex>>,
    pub(in crate::server) name_table: Option<Arc<NameTable>>,
    pub(in crate::server) fallback_completion_table:
        Arc<crate::completion::ordinary_service::FallbackCompletionNameTable>,
    pub(in crate::server) reach_graph: Option<Arc<ReachGraph>>,
    pub(in crate::server) include_table: Option<Arc<IncludeCompletionTable>>,
    pub(in crate::server) go_import_table: Option<Arc<GoImportCompletionTable>>,
    pub(in crate::server) indexed_files: Option<Arc<Vec<(String, PathBuf)>>>,
    pub(in crate::server) project_context: Option<Arc<ProjectContextIndex>>,
    pub(in crate::server) call_read_handle: Option<Arc<CallReadHandle>>,
    pub(in crate::server) workspace_semantics:
        Arc<super::super::workspace_config::PublishedWorkspaceSemantics>,
    pub(in crate::server) degraded: crate::progress::DegradedCapabilities,
}

impl EngineSnapshot {
    pub(super) fn empty(root: PathBuf) -> Self {
        let workspace_semantics =
            Arc::new(super::super::workspace_config::PublishedWorkspaceSemantics::empty(&root));
        Self {
            root,
            epoch: state::EngineEpoch::missing(),
            semantic_generation: SemanticGeneration::MISSING,
            declaration_index: None,
            name_table: None,
            fallback_completion_table: Arc::new(
                crate::completion::ordinary_service::FallbackCompletionNameTable::default(),
            ),
            reach_graph: None,
            include_table: None,
            go_import_table: None,
            indexed_files: None,
            project_context: None,
            call_read_handle: None,
            workspace_semantics,
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
            candidate_overlays: Arc::new(Mutex::new(CandidateOverlayCache::default())),
            semantic_index_memory_budget_bytes: Arc::new(AtomicU64::new(256 * 1024 * 1024)),
        }
    }
}

impl CacheLedger {
    pub(in crate::server) fn set_semantic_index_memory_budget_mb(&self, budget_mb: u64) {
        self.semantic_index_memory_budget_bytes
            .store(budget_mb.saturating_mul(1024 * 1024), Ordering::Release);
    }

    pub(in crate::server) fn semantic_index_memory_budget_bytes(&self) -> usize {
        usize::try_from(
            self.semantic_index_memory_budget_bytes
                .load(Ordering::Acquire),
        )
        .unwrap_or(usize::MAX)
    }
}

pub(in crate::server) struct CompletionMemoLookup {
    pub(in crate::server) prior_pools: Vec<Option<Vec<usize>>>,
    pub(in crate::server) hit_kind: &'static str,
}

#[derive(Clone, Copy, Debug, Default)]
pub(in crate::server) struct RequestSettings {
    pub(in crate::server) completion_enabled: bool,
    pub(in crate::server) prefix_ranking: crate::completion::CompletionPrefixRanking,
    pub(in crate::server) semantic_coloring_enabled: bool,
    pub(in crate::server) scoping_enabled: bool,
    pub(in crate::server) perf_logging_enabled: bool,
}

#[derive(Clone)]
pub(in crate::server) struct RequestContext {
    pub(in crate::server) engine: Arc<EngineSnapshot>,
    pub(in crate::server) settings: RequestSettings,
}

#[derive(Clone)]
pub(in crate::server) struct CachePublishReport {
    pub(in crate::server) semantic_generation: SemanticGeneration,
    pub(in crate::server) declaration_count: usize,
    pub(in crate::server) include_count: usize,
    pub(in crate::server) reference_file_count: usize,
    pub(in crate::server) name_table_ms: u128,
    pub(in crate::server) reach_graph_ms: u128,
    pub(in crate::server) degraded: crate::progress::DegradedCapabilities,
    pub(in crate::server) epoch: state::EngineEpoch,
    pub(in crate::server) include_table_error: Option<String>,
    pub(in crate::server) go_import_table_error: Option<String>,
    pub(in crate::server) reference_file_list_error: Option<String>,
}
