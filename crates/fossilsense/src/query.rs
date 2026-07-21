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
    project_key: Option<ProjectKey>,
}

const NO_PROJECT_ID: u32 = u32::MAX;

#[derive(Clone, Copy)]
struct CompactNameEntry {
    id: i64,
    name_id: u32,
    path_id: u32,
    project_id: u32,
    kind: ParserKind,
    role: SymbolRole,
    external: bool,
    directly_included: bool,
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
    sorted: Vec<usize>,
    by_project: HashMap<ProjectKey, Vec<usize>>,
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

/// Entry indices sorted by `(lowercased name, original name)` for prefix search.
fn sorted_indices(entries: &[CompactNameEntry], names: &[NameString]) -> Vec<usize> {
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
    sorted
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
        Self {
            base: Arc::new(base),
            deltas: Arc::new(Vec::new()),
            path_overrides: Arc::new(HashMap::new()),
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
        output: &mut Vec<usize>,
    ) {
        let start = segment
            .sorted
            .partition_point(|&index| segment.entry(index).lower < needle_lower);
        for &local in &segment.sorted[start..] {
            if !segment.entry(local).lower.starts_with(needle_lower) {
                break;
            }
            let index = offset + local;
            if self.is_active_index(index) {
                output.push(index);
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
        let sorted = sorted_indices(&entries, &names);
        let mut by_project: HashMap<ProjectKey, Vec<usize>> = HashMap::new();
        for (index, entry) in entries.iter().enumerate() {
            if entry.project_id != NO_PROJECT_ID {
                by_project
                    .entry(projects[entry.project_id as usize].clone())
                    .or_default()
                    .push(index);
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
            sorted,
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
            external: entry.external,
            directly_included: entry.directly_included,
            path: &self.paths[entry.path_id as usize],
            kind: entry.kind,
            role: entry.role,
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

    fn accounted_bytes(&self) -> usize {
        let arc_header = size_of::<usize>().saturating_mul(2);
        let names = self.names.iter().fold(0usize, |bytes, name| {
            bytes
                .saturating_add(name.original.len())
                .saturating_add(arc_header)
                .saturating_add(name.lower.len())
                .saturating_add(arc_header)
        });
        let paths = self.paths.iter().fold(0usize, |bytes, path| {
            bytes.saturating_add(path.len()).saturating_add(arc_header)
        });
        let projects = self.projects.iter().fold(0usize, |bytes, project| {
            bytes
                .saturating_add(project.workspace_root_id.len())
                .saturating_add(project.project_path.len())
        });
        let by_project = self
            .by_project
            .iter()
            .fold(0usize, |bytes, (key, indices)| {
                bytes
                    .saturating_add(key.workspace_root_id.len())
                    .saturating_add(key.project_path.len())
                    .saturating_add(indices.capacity().saturating_mul(size_of::<usize>()))
            });

        size_of::<Self>()
            .saturating_add(
                self.entries
                    .capacity()
                    .saturating_mul(size_of::<CompactNameEntry>()),
            )
            .saturating_add(
                self.names
                    .capacity()
                    .saturating_mul(size_of::<NameString>()),
            )
            .saturating_add(names)
            .saturating_add(self.paths.capacity().saturating_mul(size_of::<Arc<str>>()))
            .saturating_add(paths)
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
                self.projects
                    .capacity()
                    .saturating_mul(size_of::<ProjectKey>()),
            )
            .saturating_add(projects)
            .saturating_add(self.sorted.capacity().saturating_mul(size_of::<usize>()))
            .saturating_add(hash_table_bytes::<ProjectKey, Vec<usize>>(
                self.by_project.capacity(),
            ))
            .saturating_add(by_project)
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
