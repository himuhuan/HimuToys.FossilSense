//! Protocol-agnostic query logic: in-memory fuzzy name table, definition
//! ranking, and cursor-word extraction. Kept free of
//! `tower-lsp` request types so the scoring/ranking can be unit-tested.

use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::sync::Arc;

use crate::model::ScopeTier;
use crate::parser::{SymbolKind as ParserKind, SymbolRole};
use crate::project_context::{ProjectContextIndex, ProjectKey};
use crate::reachability::ReachScope;
use crate::resolver::{self, ResolveContext};
use crate::store::views::{DeclarationNameRow, DeclarationStoreView};

pub mod callables;
mod comments;
#[allow(dead_code)]
mod current_file_overlay;
mod documentation;
mod hover;
mod local_completion;
mod name_index_builder;
mod name_search;
mod name_updates;
mod signatures;
mod source_excerpt;
mod text;
pub mod type_resolution;

pub(crate) use callables::is_source_path;
#[cfg(test)]
pub use callables::CounterpartEvidence;
pub use callables::{
    call_declaration_presentations_at, call_definition_presentations, hover_presentations,
    resolve_callable_candidates, resolve_counterparts, signature_active_index,
    signature_presentations, ArgumentState, CallSiteContext, CallableCandidateMetrics,
    CallableCandidateSet, CallableQueryInput, CandidateCoverage, CandidateIncompleteReason,
    CandidateOrigin, ContextReliability, ResolvedCallableAnchor,
    CALLABLE_CANDIDATE_RESOLVER_VERSION,
};
pub use comments::RenderedSymbolComment;

#[allow(unused_imports)]
pub use current_file_overlay::{current_file_overlay_candidates, CurrentFileOverlayCandidate};
pub use documentation::{rank_documentation_candidates, DocumentationCandidate};
#[allow(unused_imports)]
pub use hover::{
    comment_documentation_for_candidate_symbol, hover_markdown_for_candidate, RankedHoverCandidate,
    HOVER_CANDIDATE_LIMIT,
};
pub(crate) use local_completion::local_binding_visible_for_completion;
pub use local_completion::{
    local_completion_candidates, visible_local_binding, LocalCompletionCandidate,
};
pub use signatures::{
    call_context_at, signature_parts, signature_parts_for_name, CallContext, ParameterSpan,
    RankedSignatureCandidate, SignatureParts, SIGNATURE_HELP_LIMIT,
};
pub use source_excerpt::{
    SourceByteRange as SourceExcerptRange, SourceExcerpt as SourceExcerptOutcome,
    SourceExcerptReader, SourceRevision as SourceExcerptRevision,
};
pub(crate) use text::completion_word_score_lowered;
use text::is_boundary;
pub use text::{
    byte_offset_at, completion_prefix_at, completion_word_score, is_member_completion_context,
    member_access_chain_at, word_at,
};
pub use type_resolution::*;

/// Maximum number of compact recall entries processed between cooperative
/// cancellation checks. The check is deliberately block-based so the hot loop
/// does not perform an atomic load for every declaration.
pub(crate) const COMPLETION_CANCELLATION_CHECK_INTERVAL: usize = 256;
/// Maximum compact NameTable entries/posting rows touched by one ordinary
/// completion request across all workspace roots. Result quotas cap output;
/// this independent budget caps candidate generation CPU.
pub(crate) const COMPLETION_RECALL_CANDIDATE_BUDGET: usize = 16_384;
/// Maximum compact source/name metadata probes used by a priority completion
/// channel. Production replay asserts this separately from declaration rows so
/// hidden setup work cannot pass the latency gate with a misleading row count.
pub(crate) const COMPLETION_PRIORITY_METADATA_PROBE_LIMIT: usize = 4_096;

/// Request-owned cancellation observed by long-running completion recall.
/// Implementations must be cheap and thread-safe because checks run inside the
/// foreground blocking worker.
pub(crate) trait CompletionQueryCancellation: Send + Sync {
    fn is_cancelled(&self) -> bool;
}

pub(crate) struct CompletionRecallQuery<'a> {
    pub query: &'a str,
    pub quotas: CompletionRecallQuotas,
    pub scope: Option<&'a CompletionScope>,
    pub active_project: Option<&'a ProjectKey>,
    pub prior_pool: Option<&'a [usize]>,
    pub semantic_family: Option<crate::semantic_model::SemanticFamily>,
    pub cancellation: Option<&'a dyn CompletionQueryCancellation>,
    pub candidate_budget: usize,
}

#[cfg(test)]
use name_search::{sort_scored, top_scored};
#[cfg(test)]
use name_updates::declaration_name_entries;
use name_updates::{
    hash_table_bytes, name_entry, parser_kind_from_declaration_kind,
    symbol_role_from_declaration_role,
};

/// Default cap on workspace-symbol results handed back to the editor.
pub const WORKSPACE_SYMBOL_LIMIT: usize = 200;

/// A ranked name hit from the in-memory [`NameTable`]. The `score` is the
/// resolver's packed sort key (`tier.rank() * TIER_STRIDE + base_match +
/// locality`), encoding strict-tier lexicographic order so the editor's
/// `sort_text` and the cross-root merge can sort by a single integer. `tier`
/// and `base_match` are exposed separately so callers can derive
/// `(ResolutionConfidence, ResolutionReason)` via
/// [`resolver::confidence_reason_for`] and dedup by `(tier, confidence)` without
/// re-deriving the tier from the packed score.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankedNameHit {
    pub id: i64,
    /// Packed sort key: `tier.rank() * TIER_STRIDE + base_match + locality`.
    /// Higher is better. Sort by this descending.
    pub score: i32,
    /// Scope tier assigned by [`resolver::scope_tier`]. Drives confidence/reason
    /// projection and same-name dedup.
    pub tier: ScopeTier,
    /// Raw match-quality score from `score_match` (exact/prefix/substr/subseq),
    /// kept separate from tier/locality policy.
    pub base_match: i32,
    pub name_len: usize,
    pub name: String,
    pub kind: ParserKind,
    pub role: SymbolRole,
    pub semantic_family: crate::semantic_model::SemanticFamily,
    /// Best-effort build-marker ownership for ordinary completion only.
    pub project_key: Option<ProjectKey>,
}

// ===========================================================================
// In-memory fuzzy name table
// ===========================================================================

#[derive(Clone)]
struct NameEntry {
    id: i64,
    name: Arc<str>,
    lower: Arc<str>,
    external: bool,
    /// First-layer external header (`#include`d directly by a workspace file).
    /// Carried so in-memory coloring can reproduce the SQL unscoped fallback's
    /// `workspace OR directly_included` filter; always `false` for workspace.
    directly_included: bool,
    path: Arc<str>,
    kind: ParserKind,
    role: SymbolRole,
    semantic_family: crate::semantic_model::SemanticFamily,
    project_key: Option<ProjectKey>,
}

const NO_PROJECT_ID: u32 = u32::MAX;

/// Packed per-declaration evidence for the resident name-recall index.
///
/// `directly_included` is meaningful only for external entries, so the
/// constructor normalizes that bit away for workspace declarations.
#[derive(Clone, Copy)]
#[repr(transparent)]
struct CompactNameFlags(u8);

impl CompactNameFlags {
    const EXTERNAL: u8 = 1 << 0;
    const DIRECTLY_INCLUDED: u8 = 1 << 1;
    const GO_FAMILY: u8 = 1 << 2;

    fn new(
        semantic_family: crate::semantic_model::SemanticFamily,
        external: bool,
        directly_included: bool,
    ) -> Self {
        let mut bits = match semantic_family {
            crate::semantic_model::SemanticFamily::CFamily => 0,
            crate::semantic_model::SemanticFamily::Go => Self::GO_FAMILY,
        };
        if external {
            bits |= Self::EXTERNAL;
        }
        if external && directly_included {
            bits |= Self::DIRECTLY_INCLUDED;
        }
        Self(bits)
    }

    fn semantic_family(self) -> crate::semantic_model::SemanticFamily {
        if self.0 & Self::GO_FAMILY == 0 {
            crate::semantic_model::SemanticFamily::CFamily
        } else {
            crate::semantic_model::SemanticFamily::Go
        }
    }

    fn external(self) -> bool {
        self.0 & Self::EXTERNAL != 0
    }

    fn directly_included(self) -> bool {
        self.0 & Self::DIRECTLY_INCLUDED != 0
    }
}

#[derive(Clone, Copy)]
struct CompactNameEntry {
    id: i64,
    name_id: u32,
    path_id: u32,
    project_id: u32,
    kind: ParserKind,
    role: SymbolRole,
    flags: CompactNameFlags,
}

#[derive(Clone)]
struct NameString {
    original: Arc<str>,
    lower: Arc<str>,
}

#[derive(Clone, Copy)]
struct NameEntryRef<'a> {
    id: i64,
    name: &'a str,
    lower: &'a str,
    external: bool,
    directly_included: bool,
    path: &'a str,
    kind: ParserKind,
    role: SymbolRole,
    semantic_family: crate::semantic_model::SemanticFamily,
    project_key: Option<&'a ProjectKey>,
}

#[derive(Debug, Clone, Copy)]
struct ScoredCandidate {
    score: i32,
    name_len: usize,
    index: usize,
    tier: ScopeTier,
    base_match: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompletionRecallQuotas {
    pub total_indexed: usize,
    pub reachable: usize,
    pub external: usize,
    pub unknown: usize,
    pub global: usize,
    pub same_project: usize,
}

impl CompletionRecallQuotas {
    pub fn default_for_completion_limit(limit: usize) -> Self {
        Self {
            total_indexed: limit.saturating_mul(3),
            reachable: limit,
            external: limit / 2,
            unknown: limit / 2,
            global: limit,
            same_project: 0,
        }
    }

    pub fn with_project_context(limit: usize) -> Self {
        let mut quotas = Self::default_for_completion_limit(limit);
        quotas.same_project = limit / 2;
        quotas.total_indexed = quotas.total_indexed.saturating_add(quotas.same_project);
        quotas
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CompletionRecallMetrics {
    pub reachable: usize,
    pub external: usize,
    pub unknown: usize,
    pub global: usize,
    pub same_project: usize,
    pub pool_total: usize,
    pub indexed_returned: usize,
    pub entries_inspected: usize,
    pub prefix_entries_inspected: usize,
    pub fuzzy_entries_inspected: usize,
    pub fuzzy_posting_entries_inspected: usize,
    pub fuzzy_sample_entries_inspected: usize,
    pub priority_source_probes: usize,
    pub priority_source_attempts: usize,
    pub priority_sources_initialized: usize,
    pub priority_fuzzy_name_probes: usize,
    pub priority_fuzzy_declaration_probes: usize,
    pub selection_entries_inspected: usize,
    pub active_entries_total: usize,
    pub candidate_budget: usize,
    pub cancellation_checks: usize,
    pub cancelled: bool,
    pub truncated: bool,
}

impl CompletionRecallMetrics {
    pub fn merge_from(&mut self, other: CompletionRecallMetrics) {
        self.reachable += other.reachable;
        self.external += other.external;
        self.unknown += other.unknown;
        self.global += other.global;
        self.same_project += other.same_project;
        self.pool_total += other.pool_total;
        self.indexed_returned += other.indexed_returned;
        self.entries_inspected += other.entries_inspected;
        self.prefix_entries_inspected += other.prefix_entries_inspected;
        self.fuzzy_entries_inspected += other.fuzzy_entries_inspected;
        self.fuzzy_posting_entries_inspected += other.fuzzy_posting_entries_inspected;
        self.fuzzy_sample_entries_inspected += other.fuzzy_sample_entries_inspected;
        self.priority_source_probes += other.priority_source_probes;
        self.priority_source_attempts += other.priority_source_attempts;
        self.priority_sources_initialized += other.priority_sources_initialized;
        self.priority_fuzzy_name_probes += other.priority_fuzzy_name_probes;
        self.priority_fuzzy_declaration_probes += other.priority_fuzzy_declaration_probes;
        self.selection_entries_inspected += other.selection_entries_inspected;
        self.active_entries_total += other.active_entries_total;
        self.candidate_budget += other.candidate_budget;
        self.cancellation_checks += other.cancellation_checks;
        self.cancelled |= other.cancelled;
        self.truncated |= other.truncated;
    }
}

/// Reachability scope for completion ranking: the current file's path plus the
/// bounded `#include`-reachable set (with `open` flag). Built by the LSP
/// completion path from `reach_scope_for`; `None`-equivalent (no scope) is
/// represented by passing `None` to `search_ranked_scoped_*`. Tier resolution
/// is delegated to [`resolver::scope_tier`]; this struct is the owned
/// counterpart to [`resolver::ResolveContext`] so it can be moved into a
/// `spawn_blocking` task.
#[derive(Debug, Clone)]
pub struct CompletionScope {
    pub current_path: Option<String>,
    pub reach: ReachScope,
    pub direct_external_files: HashSet<String>,
}

impl CompletionScope {
    /// Build a [`ResolveContext`] borrowing from this scope, for passage to
    /// [`resolver::scope_tier`].
    pub fn resolve_context(&self) -> ResolveContext<'_> {
        ResolveContext {
            current_path: self.current_path.as_deref(),
            reach: Some(&self.reach),
            direct_external_files: Some(&self.direct_external_files),
        }
    }
}

struct NameSegment {
    entries: Vec<CompactNameEntry>,
    names: Vec<NameString>,
    paths: Vec<Arc<str>>,
    path_ids: HashMap<Arc<str>, u32>,
    path_counts: Vec<usize>,
    path_is_external: Vec<bool>,
    projects: Vec<ProjectKey>,
    /// Entry indices sorted by `(lowercased name, original name)`, partitioned
    /// by semantic family. Every entry occurs in exactly one partition, so
    /// family isolation does not duplicate the resident per-declaration index.
    sorted_by_family: [Vec<usize>; 2],
    /// One-byte normalized head postings for single-character completion.
    /// Each declaration occurs in at most one bucket as a compact `u32` local
    /// index; buckets are ordered by the static portion of completion ranking.
    short_prefix_heads_by_family: [HashMap<u8, Vec<u32>>; 2],
    /// Compact continuous-name and boundary-initial trigram postings. The high
    /// token bit distinguishes the two match classes; every posting stores
    /// segment-local `u32` indices ordered by static completion quality.
    fuzzy_postings_by_family: [CompactFuzzyPostings; 2],
    /// Each name's sole declaring path, or `MULTI_PATH_ID` when declarations
    /// span paths. Fuzzy recovery can reject a tombstoned name from compact
    /// metadata without expanding declaration rows outside the request budget.
    sole_path_by_name: Vec<u32>,
    /// Unique `(three-byte name head, path)` pairs. A query uses the first one,
    /// two, or three bytes as a range key, locating relevant active path
    /// postings without probing unrelated paths in lexical path order.
    prefix_paths_by_family: [CompactPrefixPathPostings; 2],
    /// Per-path declaration postings in lexical name order. The CSR form adds
    /// one compact `u32` per declaration and lets bounded completion reserve a
    /// reachability channel without scanning unrelated workspace entries.
    path_postings_by_family: [CompactPathPostings; 2],
    /// Project postings are language-partitioned and lexical. Request-time
    /// project checks therefore neither allocate a workspace-sized index list
    /// nor inspect declarations from another semantic family.
    by_project: HashMap<ProjectKey, CompactProjectPostings>,
}

/// Mutually exclusive structural estimates for one immutable name segment.
/// These values explain the compact completion index; they are not an
/// allocator-level accounting of the whole process.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct NameSegmentMemoryBreakdown {
    pub(crate) declaration_entry_bytes: usize,
    pub(crate) name_record_bytes: usize,
    pub(crate) original_name_bytes: usize,
    pub(crate) lowercase_name_bytes: usize,
    pub(crate) shared_name_bytes: usize,
    pub(crate) path_metadata_bytes: usize,
    pub(crate) project_metadata_bytes: usize,
    pub(crate) sorting_index_bytes: usize,
    pub(crate) short_prefix_posting_bytes: usize,
    pub(crate) fuzzy_posting_bytes: usize,
    pub(crate) prefix_path_posting_bytes: usize,
    pub(crate) path_posting_bytes: usize,
    pub(crate) project_posting_bytes: usize,
    pub(crate) fixed_overhead_bytes: usize,
}

impl NameSegmentMemoryBreakdown {
    pub(crate) fn bytes(&self) -> usize {
        self.declaration_entry_bytes
            .saturating_add(self.name_record_bytes)
            .saturating_add(self.original_name_bytes)
            .saturating_add(self.lowercase_name_bytes)
            .saturating_add(self.shared_name_bytes)
            .saturating_add(self.path_metadata_bytes)
            .saturating_add(self.project_metadata_bytes)
            .saturating_add(self.sorting_index_bytes)
            .saturating_add(self.short_prefix_posting_bytes)
            .saturating_add(self.fuzzy_posting_bytes)
            .saturating_add(self.prefix_path_posting_bytes)
            .saturating_add(self.path_posting_bytes)
            .saturating_add(self.project_posting_bytes)
            .saturating_add(self.fixed_overhead_bytes)
    }

    pub(crate) fn add_assign(&mut self, other: Self) {
        self.declaration_entry_bytes = self
            .declaration_entry_bytes
            .saturating_add(other.declaration_entry_bytes);
        self.name_record_bytes = self
            .name_record_bytes
            .saturating_add(other.name_record_bytes);
        self.original_name_bytes = self
            .original_name_bytes
            .saturating_add(other.original_name_bytes);
        self.lowercase_name_bytes = self
            .lowercase_name_bytes
            .saturating_add(other.lowercase_name_bytes);
        self.shared_name_bytes = self
            .shared_name_bytes
            .saturating_add(other.shared_name_bytes);
        self.path_metadata_bytes = self
            .path_metadata_bytes
            .saturating_add(other.path_metadata_bytes);
        self.project_metadata_bytes = self
            .project_metadata_bytes
            .saturating_add(other.project_metadata_bytes);
        self.sorting_index_bytes = self
            .sorting_index_bytes
            .saturating_add(other.sorting_index_bytes);
        self.short_prefix_posting_bytes = self
            .short_prefix_posting_bytes
            .saturating_add(other.short_prefix_posting_bytes);
        self.fuzzy_posting_bytes = self
            .fuzzy_posting_bytes
            .saturating_add(other.fuzzy_posting_bytes);
        self.prefix_path_posting_bytes = self
            .prefix_path_posting_bytes
            .saturating_add(other.prefix_path_posting_bytes);
        self.path_posting_bytes = self
            .path_posting_bytes
            .saturating_add(other.path_posting_bytes);
        self.project_posting_bytes = self
            .project_posting_bytes
            .saturating_add(other.project_posting_bytes);
        self.fixed_overhead_bytes = self
            .fixed_overhead_bytes
            .saturating_add(other.fixed_overhead_bytes);
    }
}

struct CompactProjectPostings {
    project_id: u32,
    by_family: [Vec<u32>; 2],
}

#[derive(Clone, Copy)]
struct CompactPostingRange {
    start: u32,
    len: u32,
}

/// CSR-style fuzzy posting storage. The dictionary maps a trigram token to a
/// range in one exact-capacity name-ID array. Name IDs, rather than declaration
/// IDs, avoid repeating the same trigram for every redeclaration; request-time
/// expansion back to compact declarations remains under the candidate budget.
struct CompactFuzzyPostings {
    ranges: HashMap<u32, CompactPostingRange>,
    name_ids: Vec<u32>,
}

const UNSEEN_PATH_ID: u32 = u32::MAX - 1;
const MULTI_PATH_ID: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CompactPrefixPathPair {
    token: u32,
    path_id: u32,
}

struct CompactPrefixPathPostings {
    pairs: Vec<CompactPrefixPathPair>,
    /// Positions into `pairs`, grouped by the segment-local project ID and
    /// still ordered by prefix token. Selected-project recovery can therefore
    /// skip unrelated workspace paths without duplicating each pair payload.
    project_positions: HashMap<u32, Vec<u32>>,
}

struct CompactPathPostings {
    offsets: Vec<u32>,
    indices: Vec<u32>,
}

impl CompactPathPostings {
    fn build(entries: &[CompactNameEntry], sorted_indices: &[usize], path_count: usize) -> Self {
        let mut counts = vec![0u32; path_count];
        for &index in sorted_indices {
            let path = entries[index].path_id as usize;
            counts[path] = counts[path]
                .checked_add(1)
                .expect("path posting length exceeds u32");
        }

        let mut offsets = Vec::with_capacity(path_count + 1);
        offsets.push(0u32);
        for count in counts {
            let next = offsets
                .last()
                .copied()
                .unwrap_or_default()
                .checked_add(count)
                .expect("path posting offsets exceed u32");
            offsets.push(next);
        }
        let mut positions = offsets[..path_count].to_vec();
        let mut indices = vec![0u32; sorted_indices.len()];
        for &index in sorted_indices {
            let path = entries[index].path_id as usize;
            let position = positions[path] as usize;
            indices[position] = u32::try_from(index).expect("name segment exceeds u32 indices");
            positions[path] += 1;
        }
        Self { offsets, indices }
    }

    fn posting(&self, path_id: u32) -> &[u32] {
        let path = path_id as usize;
        let start = self.offsets[path] as usize;
        let end = self.offsets[path + 1] as usize;
        &self.indices[start..end]
    }

    fn accounted_bytes(&self) -> usize {
        size_of::<Self>()
            .saturating_add(self.offsets.capacity().saturating_mul(size_of::<u32>()))
            .saturating_add(self.indices.capacity().saturating_mul(size_of::<u32>()))
    }
}

impl CompactFuzzyPostings {
    fn build(ordered_name_ids: &[u32], names: &[NameString]) -> Self {
        let mut slot_by_token = HashMap::<u32, u32>::new();
        let mut counts = Vec::<u32>::new();
        for &name_id in ordered_name_ids {
            for token in fuzzy_tokens_for_name(&names[name_id as usize]) {
                let slot = *slot_by_token.entry(token).or_insert_with(|| {
                    let slot = u32::try_from(counts.len()).expect("fuzzy token slots exceed u32");
                    counts.push(0);
                    slot
                });
                counts[slot as usize] = counts[slot as usize]
                    .checked_add(1)
                    .expect("fuzzy posting length exceeds u32");
            }
        }

        let mut starts = Vec::with_capacity(counts.len());
        let mut total = 0u32;
        for &count in &counts {
            starts.push(total);
            total = total
                .checked_add(count)
                .expect("fuzzy posting rows exceed u32");
        }
        let mut cursors = starts.clone();
        let mut name_ids = vec![0u32; total as usize];
        for &name_id in ordered_name_ids {
            for token in fuzzy_tokens_for_name(&names[name_id as usize]) {
                let slot = slot_by_token[&token] as usize;
                let cursor = &mut cursors[slot];
                name_ids[*cursor as usize] = name_id;
                *cursor += 1;
            }
        }
        let mut ranges = HashMap::with_capacity(slot_by_token.len());
        for (token, slot) in slot_by_token {
            let slot = slot as usize;
            ranges.insert(
                token,
                CompactPostingRange {
                    start: starts[slot],
                    len: counts[slot],
                },
            );
        }
        Self { ranges, name_ids }
    }

    fn posting(&self, token: u32) -> &[u32] {
        let Some(range) = self.ranges.get(&token) else {
            return &[];
        };
        let start = range.start as usize;
        &self.name_ids[start..start + range.len as usize]
    }

    fn accounted_bytes(&self) -> usize {
        hash_table_bytes::<u32, CompactPostingRange>(self.ranges.capacity())
            .saturating_add(self.name_ids.capacity().saturating_mul(size_of::<u32>()))
    }
}

fn three_byte_prefix_token(value: &str) -> Option<u32> {
    let bytes = value.as_bytes();
    let first = *bytes.first()?;
    Some(
        (u32::from(first) << 16)
            | (u32::from(bytes.get(1).copied().unwrap_or_default()) << 8)
            | u32::from(bytes.get(2).copied().unwrap_or_default()),
    )
}

fn three_byte_prefix_bounds(value: &str) -> Option<(u32, u32)> {
    let bytes = value.as_bytes();
    let first = *bytes.first()?;
    let lower = match bytes.len() {
        1 => u32::from(first) << 16,
        2 => (u32::from(first) << 16) | (u32::from(bytes[1]) << 8),
        _ => (u32::from(first) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2]),
    };
    let width = match bytes.len() {
        1 => 1 << 16,
        2 => 1 << 8,
        _ => 1,
    };
    Some((lower, lower + width))
}

impl CompactPrefixPathPostings {
    fn build(entries: &[CompactNameEntry], names: &[NameString], family_slot: usize) -> Self {
        let mut raw_pairs = entries
            .iter()
            .filter(|entry| semantic_family_slot(entry.flags.semantic_family()) == family_slot)
            .filter_map(|entry| {
                three_byte_prefix_token(&names[entry.name_id as usize].lower).map(|token| {
                    (
                        CompactPrefixPathPair {
                            token,
                            path_id: entry.path_id,
                        },
                        entry.project_id,
                    )
                })
            })
            .collect::<Vec<_>>();
        raw_pairs
            .sort_unstable_by_key(|(pair, project_id)| (pair.token, pair.path_id, *project_id));
        raw_pairs.dedup();

        let mut pairs = Vec::with_capacity(raw_pairs.len());
        let mut project_positions = HashMap::<u32, Vec<u32>>::new();
        for (pair, project_id) in raw_pairs {
            let position = if pairs.last() == Some(&pair) {
                u32::try_from(pairs.len() - 1).expect("prefix path positions exceed u32")
            } else {
                let position =
                    u32::try_from(pairs.len()).expect("prefix path positions exceed u32");
                pairs.push(pair);
                position
            };
            if project_id != NO_PROJECT_ID {
                let positions = project_positions.entry(project_id).or_default();
                if positions.last().copied() != Some(position) {
                    positions.push(position);
                }
            }
        }
        Self {
            pairs,
            project_positions,
        }
    }

    fn paths_for_prefix(&self, prefix: &str) -> &[CompactPrefixPathPair] {
        let Some((lower, upper)) = three_byte_prefix_bounds(prefix) else {
            return &[];
        };
        let start = self.pairs.partition_point(|pair| pair.token < lower);
        let end = self.pairs.partition_point(|pair| pair.token < upper);
        &self.pairs[start..end]
    }

    fn project_positions_for_prefix(&self, project_id: u32, prefix: &str) -> &[u32] {
        let Some((lower, upper)) = three_byte_prefix_bounds(prefix) else {
            return &[];
        };
        let Some(positions) = self.project_positions.get(&project_id) else {
            return &[];
        };
        let start =
            positions.partition_point(|position| self.pairs[*position as usize].token < lower);
        let end =
            positions.partition_point(|position| self.pairs[*position as usize].token < upper);
        &positions[start..end]
    }

    fn accounted_bytes(&self) -> usize {
        size_of::<Self>()
            .saturating_add(
                self.pairs
                    .capacity()
                    .saturating_mul(size_of::<CompactPrefixPathPair>()),
            )
            .saturating_add(hash_table_bytes::<u32, Vec<u32>>(
                self.project_positions.capacity(),
            ))
            .saturating_add(
                self.project_positions
                    .values()
                    .fold(0usize, |bytes, positions| {
                        bytes.saturating_add(positions.capacity().saturating_mul(size_of::<u32>()))
                    }),
            )
    }
}

/// Segmented workspace name index. Full publication installs one immutable base;
/// dirty publication appends small changed-file segments and updates only the
/// per-path active override map. Virtual entry indices remain stable for the
/// lifetime of one engine snapshot and are invalidated with its completion memo.
/// This is a prefix-recall accelerator only: consumers must hydrate every
/// semantic presentation through `CandidateQueryService::semantic_candidates`.
pub struct NameTable {
    base: Arc<NameSegment>,
    deltas: Arc<Vec<Arc<NameSegment>>>,
    /// Path -> active delta segment. `None` is a deletion tombstone.
    path_overrides: Arc<HashMap<Arc<str>, Option<usize>>>,
    /// Base paths that remain active in this immutable table view. Publication
    /// updates this list; request-time recovery can therefore open a bounded
    /// path posting without walking a stale base cursor first.
    active_base_paths: Arc<Vec<Arc<str>>>,
    /// Active paths retained by each delta segment. A later partial replacement
    /// removes only the affected path from its original delta, allowing bounded
    /// recall to traverse the remaining path posting directly instead of
    /// walking an arbitrarily long run of tombstoned sibling declarations.
    active_delta_paths: Arc<Vec<Arc<Vec<Arc<str>>>>>,
    /// Active declaration counts for every selected-project/language pair.
    /// Completion consults this O(1) summary before enabling project quotas;
    /// it must never scan project postings outside the candidate budget.
    active_project_family_counts: Arc<HashMap<ProjectKey, [usize; 2]>>,
    delta_offsets: Arc<Vec<usize>>,
    active_len: usize,
    slot_len: usize,
    /// Sparse request-local replacement for durable first-layer external
    /// flags. Dirty include edits can change this workspace-wide property
    /// without rebuilding the immutable segmented name index.
    direct_include_overrides: Arc<HashMap<String, bool>>,
    /// Cached unscoped coloring fallback: all workspace files in a closed
    /// reachability set. Reused by `colorable_kind_counts(None)` instead of
    /// rebuilding the same path set on every semantic-token request.
    all_workspace_reach: Arc<ReachScope>,
}

const ALL_SEMANTIC_FAMILY_SLOTS: [usize; 2] = [0, 1];

fn semantic_family_slot(family: crate::semantic_model::SemanticFamily) -> usize {
    match family {
        crate::semantic_model::SemanticFamily::CFamily => 0,
        crate::semantic_model::SemanticFamily::Go => 1,
    }
}

fn semantic_family_slots(
    family: Option<crate::semantic_model::SemanticFamily>,
) -> &'static [usize] {
    match family {
        None => &ALL_SEMANTIC_FAMILY_SLOTS,
        Some(crate::semantic_model::SemanticFamily::CFamily) => &ALL_SEMANTIC_FAMILY_SLOTS[..1],
        Some(crate::semantic_model::SemanticFamily::Go) => &ALL_SEMANTIC_FAMILY_SLOTS[1..],
    }
}

fn completion_role_recall_priority(role: SymbolRole) -> u8 {
    match role {
        SymbolRole::Declaration => 4,
        SymbolRole::TentativeDefinition => 3,
        SymbolRole::Definition => 2,
        SymbolRole::UnknownDeclarationOrDefinition => 1,
    }
}

const FUZZY_BOUNDARY_TRIGRAM_TAG: u32 = 1 << 24;
const MAX_CONTIGUOUS_TRIGRAMS_PER_NAME: usize = 64;
const MAX_BOUNDARY_INITIALS_FOR_TRIGRAMS: usize = 8;

fn fuzzy_trigram_token(bytes: &[u8], boundary: bool) -> u32 {
    debug_assert_eq!(bytes.len(), 3);
    u32::from(bytes[0])
        | (u32::from(bytes[1]) << 8)
        | (u32::from(bytes[2]) << 16)
        | if boundary {
            FUZZY_BOUNDARY_TRIGRAM_TAG
        } else {
            0
        }
}

fn sampled_trigram_tokens(bytes: &[u8], boundary: bool, cap: usize, output: &mut Vec<u32>) {
    let window_count = bytes.len().saturating_sub(2);
    if window_count <= cap {
        output.extend(
            bytes
                .windows(3)
                .map(|window| fuzzy_trigram_token(window, boundary)),
        );
        return;
    }
    for ordinal in 0..cap {
        let position = if cap <= 1 {
            0
        } else {
            ordinal.saturating_mul(window_count - 1) / (cap - 1)
        };
        output.push(fuzzy_trigram_token(
            &bytes[position..position + 3],
            boundary,
        ));
    }
}

fn fuzzy_tokens_for_name(name: &NameString) -> Vec<u32> {
    let mut tokens = Vec::new();
    sampled_trigram_tokens(
        name.lower.as_bytes(),
        false,
        MAX_CONTIGUOUS_TRIGRAMS_PER_NAME,
        &mut tokens,
    );
    let original = name.original.as_bytes();
    let lower = name.lower.as_bytes();
    let boundary_initials: Vec<u8> = (0..lower.len())
        .filter(|&index| is_boundary(original, index))
        .map(|index| lower[index])
        .take(MAX_BOUNDARY_INITIALS_FOR_TRIGRAMS)
        .collect();
    for first in 0..boundary_initials.len() {
        for second in first + 1..boundary_initials.len() {
            for third in second + 1..boundary_initials.len() {
                tokens.push(fuzzy_trigram_token(
                    &[
                        boundary_initials[first],
                        boundary_initials[second],
                        boundary_initials[third],
                    ],
                    true,
                ));
            }
        }
    }
    tokens.sort_unstable();
    tokens.dedup();
    tokens
}

fn fuzzy_query_tokens(query: &str) -> (Vec<u32>, Vec<u32>) {
    let mut continuous: Vec<_> = query
        .as_bytes()
        .windows(3)
        .map(|window| fuzzy_trigram_token(window, false))
        .collect();
    continuous.sort_unstable();
    continuous.dedup();
    let boundary_query: Vec<_> = query
        .as_bytes()
        .iter()
        .copied()
        .take(MAX_BOUNDARY_INITIALS_FOR_TRIGRAMS)
        .collect();
    let mut boundary = Vec::new();
    for first in 0..boundary_query.len() {
        for second in first + 1..boundary_query.len() {
            for third in second + 1..boundary_query.len() {
                boundary.push(fuzzy_trigram_token(
                    &[
                        boundary_query[first],
                        boundary_query[second],
                        boundary_query[third],
                    ],
                    true,
                ));
            }
        }
    }
    boundary.sort_unstable();
    boundary.dedup();
    (continuous, boundary)
}

fn static_segment_entry_order(
    entries: &[CompactNameEntry],
    names: &[NameString],
    paths: &[Arc<str>],
    left: u32,
    right: u32,
) -> std::cmp::Ordering {
    let left = entries[left as usize];
    let right = entries[right as usize];
    let left_name = &names[left.name_id as usize].original;
    let right_name = &names[right.name_id as usize].original;
    left_name
        .len()
        .cmp(&right_name.len())
        .then_with(|| left_name.cmp(right_name))
        .then_with(|| {
            completion_role_recall_priority(right.role)
                .cmp(&completion_role_recall_priority(left.role))
        })
        .then_with(|| paths[left.path_id as usize].cmp(&paths[right.path_id as usize]))
        .then_with(|| left.id.cmp(&right.id))
}

/// Entry indices sorted by `(lowercased name, original name)` for prefix search,
/// partitioned by semantic family so unrelated languages cannot consume a
/// bounded request's candidate budget.
fn sorted_indices_by_family(entries: &[CompactNameEntry], names: &[NameString]) -> [Vec<usize>; 2] {
    let mut name_order: Vec<u32> = (0..names.len())
        .map(|index| u32::try_from(index).expect("name arena exceeds u32 IDs"))
        .collect();
    name_order.sort_unstable_by(|&a, &b| {
        names[a as usize]
            .lower
            .cmp(&names[b as usize].lower)
            .then_with(|| names[a as usize].original.cmp(&names[b as usize].original))
    });

    let mut counts = vec![0_u32; names.len()];
    for entry in entries {
        counts[entry.name_id as usize] += 1;
    }
    let mut cursors = vec![0_u32; names.len()];
    let mut next = 0_u32;
    for name_id in name_order {
        cursors[name_id as usize] = next;
        next = next
            .checked_add(counts[name_id as usize])
            .expect("name index exceeds u32 entry positions");
    }
    let mut sorted = vec![0_usize; entries.len()];
    for (index, entry) in entries.iter().enumerate() {
        let cursor = &mut cursors[entry.name_id as usize];
        sorted[*cursor as usize] = index;
        *cursor += 1;
    }
    let mut by_family = [Vec::new(), Vec::new()];
    for index in sorted {
        by_family[semantic_family_slot(entries[index].flags.semantic_family())].push(index);
    }
    by_family
}

fn candidate_postings_by_family(
    entries: &[CompactNameEntry],
    names: &[NameString],
    paths: &[Arc<str>],
) -> ([HashMap<u8, Vec<u32>>; 2], [CompactFuzzyPostings; 2]) {
    let mut heads = [HashMap::new(), HashMap::new()];
    let mut static_order = [Vec::new(), Vec::new()];
    for (index, entry) in entries.iter().enumerate() {
        static_order[semantic_family_slot(entry.flags.semantic_family())]
            .push(u32::try_from(index).expect("name segment exceeds u32 entry indices"));
    }
    for indices in &mut static_order {
        indices.sort_unstable_by(|&left, &right| {
            static_segment_entry_order(entries, names, paths, left, right)
        });
    }
    for (family_slot, indices) in static_order.iter().enumerate() {
        for &local in indices {
            let entry = entries[local as usize];
            let name = &names[entry.name_id as usize];
            if let Some(&head) = name.lower.as_bytes().first() {
                heads[family_slot]
                    .entry(head)
                    .or_insert_with(Vec::new)
                    .push(local);
            }
        }
    }
    let mut name_families = vec![0u8; names.len()];
    for entry in entries {
        name_families[entry.name_id as usize] |=
            1u8 << semantic_family_slot(entry.flags.semantic_family());
    }
    let mut ordered_names = [Vec::new(), Vec::new()];
    for name_id in 0..names.len() {
        let name_id = u32::try_from(name_id).expect("name arena exceeds u32 IDs");
        for (family_slot, family_names) in ordered_names.iter_mut().enumerate() {
            if name_families[name_id as usize] & (1u8 << family_slot) != 0 {
                family_names.push(name_id);
            }
        }
    }
    for family_names in &mut ordered_names {
        family_names.sort_unstable_by(|&left, &right| {
            let left_name = &names[left as usize].original;
            let right_name = &names[right as usize].original;
            left_name
                .len()
                .cmp(&right_name.len())
                .then_with(|| left_name.cmp(right_name))
                .then_with(|| left.cmp(&right))
        });
    }
    let [c_names, go_names] = ordered_names;
    let fuzzy = [
        CompactFuzzyPostings::build(&c_names, names),
        CompactFuzzyPostings::build(&go_names, names),
    ];
    (heads, fuzzy)
}

fn all_workspace_reach(segment: &NameSegment) -> ReachScope {
    ReachScope {
        files: segment
            .paths
            .iter()
            .zip(&segment.path_is_external)
            .filter(|(_, external)| !**external)
            .map(|(path, _)| path.to_string())
            .collect(),
        heuristic_files: HashSet::new(),
        open: false,
        reason: None,
    }
}

impl NameTable {
    #[allow(dead_code)]
    pub fn build(names: Vec<(i64, String, bool)>) -> Self {
        Self::build_with_paths(
            names
                .into_iter()
                .map(|(id, name, external)| {
                    (id, name, external, String::new(), String::new(), false)
                })
                .collect(),
        )
    }

    #[allow(dead_code)]
    pub fn build_with_paths(names: Vec<(i64, String, bool, String, String, bool)>) -> Self {
        let entries: Vec<NameEntry> = names.into_iter().map(name_entry).collect();
        Self::from_entries(entries)
    }

    pub(crate) fn build_from_declaration_view(
        view: &DeclarationStoreView<'_>,
        project_context: Option<&ProjectContextIndex>,
    ) -> anyhow::Result<Self> {
        let mut builder = name_index_builder::NameIndexBuilder::new(project_context);
        view.visit_name_rows(|row| {
            builder.push_declaration(row);
            Ok(())
        })?;
        Ok(builder.finish())
    }

    #[cfg(test)]
    pub(crate) fn build_from_declaration_name_rows_with_project_context(
        rows: Vec<DeclarationNameRow>,
        project_context: Option<&ProjectContextIndex>,
    ) -> Self {
        Self::from_entries(declaration_name_entries(rows, project_context))
    }

    #[cfg(test)]
    pub fn build_with_paths_and_project_context(
        names: Vec<(i64, String, bool, String, String, bool)>,
        project_context: &ProjectContextIndex,
    ) -> Self {
        let entries = names
            .into_iter()
            .map(name_entry)
            .map(|mut entry| {
                if !entry.external {
                    entry.project_key = project_context.nearest_for_file(&entry.path);
                }
                entry
            })
            .collect();
        Self::from_entries(entries)
    }

    fn from_entries(entries: Vec<NameEntry>) -> Self {
        let mut builder = name_index_builder::NameIndexBuilder::new(None);
        for entry in entries {
            builder.push_entry(entry);
        }
        builder.finish()
    }

    fn from_base_segment(base: NameSegment) -> Self {
        let all_workspace_reach = Arc::new(all_workspace_reach(&base));
        let active_len = base.entries.len();
        let mut active_base_paths = base.paths.clone();
        active_base_paths.sort_unstable();
        let active_project_family_counts = base
            .by_project
            .iter()
            .map(|(key, postings)| {
                (
                    key.clone(),
                    [postings.by_family[0].len(), postings.by_family[1].len()],
                )
            })
            .collect();
        Self {
            base: Arc::new(base),
            deltas: Arc::new(Vec::new()),
            path_overrides: Arc::new(HashMap::new()),
            active_base_paths: Arc::new(active_base_paths),
            active_delta_paths: Arc::new(Vec::new()),
            active_project_family_counts: Arc::new(active_project_family_counts),
            delta_offsets: Arc::new(Vec::new()),
            active_len,
            slot_len: active_len,
            direct_include_overrides: Arc::new(HashMap::new()),
            all_workspace_reach,
        }
    }

    fn entry(&self, index: usize) -> NameEntryRef<'_> {
        if index < self.base.entries.len() {
            return self.base.entry(index);
        }
        let delta_index = self
            .delta_offsets
            .partition_point(|offset| *offset <= index)
            .saturating_sub(1);
        let offset = self.delta_offsets[delta_index];
        self.deltas[delta_index].entry(index - offset)
    }

    fn segment_for_index(&self, index: usize) -> Option<usize> {
        (index >= self.base.entries.len()).then(|| {
            self.delta_offsets
                .partition_point(|offset| *offset <= index)
                .saturating_sub(1)
        })
    }

    fn is_active_index(&self, index: usize) -> bool {
        let entry = self.entry(index);
        match self.path_overrides.get(entry.path) {
            None => self.segment_for_index(index).is_none(),
            Some(Some(active_delta)) => self.segment_for_index(index) == Some(*active_delta),
            Some(None) => false,
        }
    }

    fn active_indices(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.slot_len).filter(|index| self.is_active_index(*index))
    }

    fn extend_prefix_matches(
        &self,
        segment: &NameSegment,
        offset: usize,
        needle_lower: &str,
        semantic_family: Option<crate::semantic_model::SemanticFamily>,
        output: &mut Vec<usize>,
    ) {
        for &family_slot in semantic_family_slots(semantic_family) {
            let sorted = &segment.sorted_by_family[family_slot];
            let start = sorted.partition_point(|&index| segment.entry(index).lower < needle_lower);
            for &local in &sorted[start..] {
                if !segment.entry(local).lower.starts_with(needle_lower) {
                    break;
                }
                let index = offset + local;
                if self.is_active_index(index) {
                    output.push(index);
                }
            }
        }
    }

    #[cfg(test)]
    pub fn delta_segment_count(&self) -> usize {
        self.deltas.len()
    }

    pub(crate) fn needs_compaction(&self) -> bool {
        self.deltas.len() >= 64
            || self.slot_len.saturating_sub(self.base.entries.len())
                > self.base.entries.len().saturating_div(4)
    }

    pub(crate) fn compacted(&self) -> Self {
        let mut builder = name_index_builder::NameIndexBuilder::new(None);
        for index in self.active_indices() {
            builder.push_ref(self.entry(index));
        }
        let mut compacted = builder.finish();
        compacted.direct_include_overrides = self.direct_include_overrides.clone();
        compacted
    }

    #[cfg(test)]
    fn active_entry(&self, index: usize) -> NameEntryRef<'_> {
        self.entry(index)
    }
}

impl NameSegment {
    fn from_entries(entries: Vec<NameEntry>) -> Self {
        let mut builder = name_index_builder::NameIndexBuilder::new(None);
        for entry in entries {
            builder.push_entry(entry);
        }
        builder.finish_segment()
    }

    fn from_compact_parts(
        entries: Vec<CompactNameEntry>,
        names: Vec<NameString>,
        paths: Vec<Arc<str>>,
        path_ids: HashMap<Arc<str>, u32>,
        path_counts: Vec<usize>,
        path_is_external: Vec<bool>,
        projects: Vec<ProjectKey>,
    ) -> Self {
        let sorted_by_family = sorted_indices_by_family(&entries, &names);
        let (short_prefix_heads_by_family, fuzzy_postings_by_family) =
            candidate_postings_by_family(&entries, &names, &paths);
        let mut sole_path_by_name = vec![UNSEEN_PATH_ID; names.len()];
        for entry in &entries {
            let path = &mut sole_path_by_name[entry.name_id as usize];
            *path = match *path {
                UNSEEN_PATH_ID => entry.path_id,
                existing if existing == entry.path_id => existing,
                _ => MULTI_PATH_ID,
            };
        }
        debug_assert!(sole_path_by_name
            .iter()
            .all(|path_id| *path_id != UNSEEN_PATH_ID));
        let prefix_paths_by_family = std::array::from_fn(|family_slot| {
            CompactPrefixPathPostings::build(&entries, &names, family_slot)
        });
        let path_postings_by_family = std::array::from_fn(|family_slot| {
            CompactPathPostings::build(&entries, &sorted_by_family[family_slot], paths.len())
        });
        let mut by_project: HashMap<ProjectKey, CompactProjectPostings> = HashMap::new();
        for (family_slot, sorted) in sorted_by_family.iter().enumerate() {
            for &index in sorted {
                let entry = entries[index];
                if entry.project_id != NO_PROJECT_ID {
                    let postings = by_project
                        .entry(projects[entry.project_id as usize].clone())
                        .or_insert_with(|| CompactProjectPostings {
                            project_id: entry.project_id,
                            by_family: std::array::from_fn(|_| Vec::new()),
                        });
                    debug_assert_eq!(postings.project_id, entry.project_id);
                    postings.by_family[family_slot]
                        .push(u32::try_from(index).expect("name segment exceeds u32 indices"));
                }
            }
        }
        Self {
            entries,
            names,
            paths,
            path_ids,
            path_counts,
            path_is_external,
            projects,
            sorted_by_family,
            short_prefix_heads_by_family,
            fuzzy_postings_by_family,
            sole_path_by_name,
            prefix_paths_by_family,
            path_postings_by_family,
            by_project,
        }
    }

    fn entry(&self, index: usize) -> NameEntryRef<'_> {
        let entry = self.entries[index];
        let name = &self.names[entry.name_id as usize];
        NameEntryRef {
            id: entry.id,
            name: &name.original,
            lower: &name.lower,
            external: entry.flags.external(),
            directly_included: entry.flags.directly_included(),
            path: &self.paths[entry.path_id as usize],
            kind: entry.kind,
            role: entry.role,
            semantic_family: entry.flags.semantic_family(),
            project_key: (entry.project_id != NO_PROJECT_ID)
                .then(|| &self.projects[entry.project_id as usize]),
        }
    }

    fn path_count(&self, path: &str) -> usize {
        self.path_ids
            .get(path)
            .map_or(0, |id| self.path_counts[*id as usize])
    }

    fn interned_path(&self, path: &str) -> Option<Arc<str>> {
        self.path_ids
            .get(path)
            .map(|id| self.paths[*id as usize].clone())
    }

    #[allow(dead_code)]
    fn accounted_bytes(&self) -> usize {
        self.memory_breakdown().bytes()
    }

    fn memory_breakdown(&self) -> NameSegmentMemoryBreakdown {
        let arc_header = size_of::<usize>().saturating_mul(2);
        let mut breakdown = NameSegmentMemoryBreakdown {
            declaration_entry_bytes: self
                .entries
                .capacity()
                .saturating_mul(size_of::<CompactNameEntry>()),
            name_record_bytes: self
                .names
                .capacity()
                .saturating_mul(size_of::<NameString>()),
            path_metadata_bytes: self
                .paths
                .capacity()
                .saturating_mul(size_of::<Arc<str>>())
                .saturating_add(hash_table_bytes::<Arc<str>, u32>(self.path_ids.capacity()))
                .saturating_add(
                    self.path_counts
                        .capacity()
                        .saturating_mul(size_of::<usize>()),
                )
                .saturating_add(
                    self.path_is_external
                        .capacity()
                        .saturating_mul(size_of::<bool>()),
                )
                .saturating_add(
                    self.sole_path_by_name
                        .capacity()
                        .saturating_mul(size_of::<u32>()),
                ),
            project_metadata_bytes: self
                .projects
                .capacity()
                .saturating_mul(size_of::<ProjectKey>())
                .saturating_add(hash_table_bytes::<ProjectKey, CompactProjectPostings>(
                    self.by_project.capacity(),
                )),
            sorting_index_bytes: self
                .sorted_by_family
                .iter()
                .map(|indices| indices.capacity().saturating_mul(size_of::<usize>()))
                .sum::<usize>(),
            fixed_overhead_bytes: size_of::<Self>(),
            ..NameSegmentMemoryBreakdown::default()
        };

        // Normal production names own one Arc each. Avoid allocating a
        // workspace-sized temporary map while building a report just to prove
        // that common case. Only candidate shared Arcs need pointer tracking.
        let mut shared_name_allocations = HashMap::<usize, (usize, usize, bool, bool)>::new();
        for name in &self.names {
            for (value, is_original) in [(&name.original, true), (&name.lower, false)] {
                if Arc::strong_count(value) == 1 {
                    if is_original {
                        breakdown.original_name_bytes = breakdown
                            .original_name_bytes
                            .saturating_add(value.len().saturating_add(arc_header));
                    } else {
                        breakdown.lowercase_name_bytes = breakdown
                            .lowercase_name_bytes
                            .saturating_add(value.len().saturating_add(arc_header));
                    }
                } else {
                    let pointer = Arc::as_ptr(value) as *const () as usize;
                    let allocation = shared_name_allocations.entry(pointer).or_insert_with(|| {
                        (value.len().saturating_add(arc_header), 0, false, false)
                    });
                    allocation.1 = allocation.1.saturating_add(1);
                    if is_original {
                        allocation.2 = true;
                    } else {
                        allocation.3 = true;
                    }
                }
            }
        }
        for (_, (bytes, references, used_as_original, used_as_lowercase)) in shared_name_allocations
        {
            if references > 1 {
                breakdown.shared_name_bytes = breakdown.shared_name_bytes.saturating_add(bytes);
            } else if used_as_original {
                breakdown.original_name_bytes = breakdown.original_name_bytes.saturating_add(bytes);
            } else if used_as_lowercase {
                breakdown.lowercase_name_bytes =
                    breakdown.lowercase_name_bytes.saturating_add(bytes);
            }
        }

        breakdown.path_metadata_bytes = breakdown.path_metadata_bytes.saturating_add(
            self.paths.iter().fold(0usize, |bytes, path| {
                bytes.saturating_add(path.len()).saturating_add(arc_header)
            }),
        );
        breakdown.project_metadata_bytes =
            breakdown
                .project_metadata_bytes
                .saturating_add(self.projects.iter().fold(0usize, |bytes, project| {
                    bytes
                        .saturating_add(project.workspace_root_id.len())
                        .saturating_add(project.project_path.len())
                }));
        for (key, postings) in &self.by_project {
            breakdown.project_metadata_bytes = breakdown
                .project_metadata_bytes
                .saturating_add(key.workspace_root_id.len())
                .saturating_add(key.project_path.len());
            breakdown.project_posting_bytes = breakdown.project_posting_bytes.saturating_add(
                postings
                    .by_family
                    .iter()
                    .map(|family| family.capacity().saturating_mul(size_of::<u32>()))
                    .sum::<usize>(),
            );
        }
        breakdown.short_prefix_posting_bytes =
            self.short_prefix_heads_by_family
                .iter()
                .fold(0usize, |bytes, family_heads| {
                    bytes
                        .saturating_add(hash_table_bytes::<u8, Vec<u32>>(family_heads.capacity()))
                        .saturating_add(family_heads.values().fold(0usize, |bytes, indices| {
                            bytes
                                .saturating_add(indices.capacity().saturating_mul(size_of::<u32>()))
                        }))
                });
        breakdown.fuzzy_posting_bytes = self
            .fuzzy_postings_by_family
            .iter()
            .fold(0usize, |bytes, postings| {
                bytes.saturating_add(postings.accounted_bytes())
            });
        breakdown.prefix_path_posting_bytes = self
            .prefix_paths_by_family
            .iter()
            .fold(0usize, |bytes, postings| {
                bytes.saturating_add(postings.accounted_bytes())
            });
        breakdown.path_posting_bytes = self
            .path_postings_by_family
            .iter()
            .fold(0usize, |bytes, postings| {
                bytes.saturating_add(postings.accounted_bytes())
            });
        breakdown
    }
}

pub const COMPLETION_LIMIT: usize = 100;
pub const COMPLETION_LOCALITY_BONUS: i32 = 50;
pub const MIN_PREFIX_LEN: usize = 1;
pub const MEMBER_COMPLETION_MIN_PREFIX_LEN: usize = 2;

#[allow(dead_code)]
pub fn normalized_receiver_record_hint(receiver_name: &str) -> String {
    receiver_name
        .trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_digit())
        .to_ascii_lowercase()
}

/// Prefix lengths below this value use a tightened recall threshold
/// (`SHORT_PREFIX_MIN_SCORE`); at this length and above the full fuzzy tier
/// set (including subsequence / camelCase-initials matches) is restored.
pub const SHORT_PREFIX_MIN_LEN: usize = 3;

/// Minimum raw `score_match` accepted for short prefixes (len < 3): keeps the
/// exact (1000), prefix (800), and word-boundary-substring (650) tiers, drops
/// plain substrings (500) and all subsequence tiers (400/200).
pub const SHORT_PREFIX_MIN_SCORE: i32 = 650;

#[cfg(test)]
mod tests;
