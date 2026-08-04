use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use tokio::sync::Mutex;
use tower_lsp::lsp_types::Url;

use super::super::go_import_completion::GoImportCompletionTable;
use super::super::include_completion::IncludeCompletionTable;
use super::super::state;
use crate::call_model::SemanticGeneration;
use crate::call_service::CallReadHandle;
use crate::candidate_service::{CandidateOverlaySnapshot, IncludePathIndex, RecallUniverseId};
use crate::declaration_index::SemanticDeclarationIndex;
use crate::memory_report::{DeclarationCacheSample, SnapshotMemoryReport};
use crate::project_context::ProjectContextIndex;
use crate::query::NameTable;
use crate::reachability::ReachGraph;
use crate::references;

/// Per-(root, epoch) memoized static memory reports, shared with the
/// resource-monitor blocking thread.
type SnapshotMemoryReports =
    Arc<StdMutex<HashMap<(PathBuf, state::EngineEpoch), Arc<SnapshotMemoryReport>>>>;

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
    /// Per-(root, epoch) static memory reports. Entries are computed lazily on
    /// the resource-monitor thread and pruned as soon as a newer generation is
    /// published for the same root, so the map never outlives its snapshot.
    pub(in crate::server) snapshot_memory_reports: SnapshotMemoryReports,
    #[cfg(test)]
    pub(super) completion_overlay_cache_hits: Arc<AtomicU64>,
    #[cfg(test)]
    pub(super) completion_overlay_cache_misses: Arc<AtomicU64>,
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
    pub(in crate::server) include_path_index: Option<Arc<IncludePathIndex>>,
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
            include_path_index: None,
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
            snapshot_memory_reports: Arc::new(StdMutex::new(HashMap::new())),
            #[cfg(test)]
            completion_overlay_cache_hits: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            completion_overlay_cache_misses: Arc::new(AtomicU64::new(0)),
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

    /// Static per-generation reports plus live payload-cache samples for the
    /// given published snapshots. Missing static reports are computed once and
    /// memoized per (root, epoch); stale generations are pruned. Runs on the
    /// resource-monitor blocking thread, not on request paths.
    pub(in crate::server) fn memory_observation(
        &self,
        snapshots: &[Arc<EngineSnapshot>],
    ) -> (Vec<SnapshotMemoryReport>, Vec<DeclarationCacheSample>) {
        let mut memo = self
            .snapshot_memory_reports
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let current: std::collections::HashSet<(PathBuf, state::EngineEpoch)> = snapshots
            .iter()
            .map(|snapshot| (snapshot.root.clone(), snapshot.epoch))
            .collect();
        memo.retain(|key, _| current.contains(key));

        let mut statics = Vec::with_capacity(snapshots.len());
        let mut caches = Vec::new();
        for snapshot in snapshots {
            let key = (snapshot.root.clone(), snapshot.epoch);
            let report = memo.entry(key).or_insert_with(|| {
                Arc::new(crate::server::indexing::snapshot_memory_report_from_parts(
                    snapshot.declaration_index.as_deref(),
                    &snapshot.fallback_completion_table,
                    snapshot.reach_graph.as_deref(),
                    snapshot.include_table.as_deref(),
                    snapshot.go_import_table.as_deref(),
                    snapshot
                        .indexed_files
                        .as_ref()
                        .map(|files| files.as_slice()),
                    snapshot.project_context.as_deref(),
                ))
            });
            statics.push((**report).clone());
            if let Some(index) = &snapshot.declaration_index {
                caches.push(DeclarationCacheSample {
                    stats: index.payload_cache_stats(),
                    budget_bytes: index.payload_budget_bytes(),
                });
            }
        }
        (statics, caches)
    }

    /// Bytes of unsaved source text held by cached candidate overlay
    /// snapshots, for memory observability.
    pub(in crate::server) async fn overlay_memory_bytes(&self) -> usize {
        let overlays = self.candidate_overlays.lock().await;
        let mut bytes = 0usize;
        for snapshot in overlays.entries.values() {
            bytes = bytes.saturating_add(snapshot.source_text_bytes());
        }
        for entry in overlays.completion_entries.values() {
            bytes = bytes.saturating_add(entry.snapshot.source_text_bytes());
        }
        bytes
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
