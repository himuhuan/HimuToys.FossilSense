//! Protocol-agnostic query logic: in-memory fuzzy name table, definition
//! ranking, and cursor-word extraction. Kept free of
//! `tower-lsp` request types so the scoring/ranking can be unit-tested.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::model::ScopeTier;
use crate::parser::SymbolKind as ParserKind;
use crate::project_context::{ProjectContextIndex, ProjectKey};
use crate::reachability::ReachScope;
use crate::resolver::{self, ResolveContext};
use crate::store::views::{NameTableStoreView, NameTableSymbolRow};

pub mod callables;
mod comments;
#[allow(dead_code)]
mod current_file_overlay;
mod definitions;
mod documentation;
mod hover;
mod local_completion;
mod name_index_builder;
mod signatures;
mod source_excerpt;
mod text;
pub mod type_resolution;

pub(crate) use callables::is_source_path;
#[cfg(test)]
pub use callables::CounterpartEvidence;
pub use callables::{
    anchor_opposite_definition, call_definition_presentations, hover_presentations,
    resolve_callable_candidates, resolve_counterparts, signature_active_index,
    signature_presentations, ArgumentState, CallSiteContext, CallableCandidateMetrics,
    CallableCandidateSet, CallableQueryInput, CandidateCoverage, CandidateIncompleteReason,
    CandidateOrigin, ContextReliability, ResolvedCallableAnchor,
    CALLABLE_CANDIDATE_RESOLVER_VERSION,
};
pub use comments::RenderedSymbolComment;

#[allow(unused_imports)]
pub use current_file_overlay::{current_file_overlay_candidates, CurrentFileOverlayCandidate};
pub use definitions::{
    rank_definitions_into_candidates_with_scope, rank_navigation_candidates_with_scope,
};
pub use documentation::{rank_documentation_candidates, DocumentationCandidate};
pub use hover::{
    comment_documentation_for_candidate_symbol, hover_markdown_for_candidate,
    rank_hover_candidates, RankedHoverCandidate, HOVER_CANDIDATE_LIMIT,
};
pub use local_completion::{local_completion_candidates, LocalCompletionCandidate};
pub use signatures::{
    call_context_at, signature_parts, signature_parts_for_name, CallContext, ParameterSpan,
    RankedSignatureCandidate, SignatureParts, SIGNATURE_HELP_LIMIT,
};
pub use source_excerpt::{
    SourceByteRange as SourceExcerptRange, SourceExcerpt as SourceExcerptOutcome,
    SourceExcerptReader, SourceRevision as SourceExcerptRevision,
};
use text::is_boundary;
pub use text::{
    byte_offset_at, completion_prefix_at, completion_word_score, is_member_completion_context,
    member_access_chain_at, word_at,
};
pub use type_resolution::*;

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
}

impl CompletionScope {
    /// Build a [`ResolveContext`] borrowing from this scope, for passage to
    /// [`resolver::scope_tier`].
    pub fn resolve_context(&self) -> ResolveContext<'_> {
        ResolveContext {
            current_path: self.current_path.as_deref(),
            reach: Some(&self.reach),
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

    #[cfg(test)]
    pub fn build_from_rows_with_project_context(
        rows: Vec<NameTableSymbolRow>,
        project_context: Option<&ProjectContextIndex>,
    ) -> Self {
        let entries = name_entries_from_rows_with_project_context(rows, project_context);
        Self::from_entries(entries)
    }

    pub(crate) fn build_from_store_view(
        view: &NameTableStoreView<'_>,
        project_context: Option<&ProjectContextIndex>,
    ) -> anyhow::Result<Self> {
        let mut builder = name_index_builder::NameIndexBuilder::new(project_context);
        view.visit_symbol_rows(|row| {
            builder.push(row);
            Ok(())
        })?;
        Ok(builder.finish())
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
}

impl NameTable {
    #[allow(dead_code)]
    pub fn with_updated_paths(
        &self,
        paths: &HashSet<String>,
        names: Vec<(i64, String, bool, String, String, bool)>,
    ) -> Self {
        let fresh_entries = names.into_iter().map(name_entry);
        self.with_updated_entries(paths, fresh_entries)
    }

    #[allow(dead_code)]
    pub fn with_updated_path_rows(
        &self,
        paths: &HashSet<String>,
        rows: Vec<NameTableSymbolRow>,
    ) -> Self {
        let fresh_entries = rows.into_iter().map(name_entry_from_row);
        self.with_updated_entries(paths, fresh_entries)
    }

    pub fn with_updated_path_rows_with_project_context(
        &self,
        paths: &HashSet<String>,
        rows: Vec<NameTableSymbolRow>,
        project_context: Option<&ProjectContextIndex>,
    ) -> Self {
        let fresh_entries = name_entries_from_rows_with_project_context(rows, project_context);
        self.with_updated_entries(paths, fresh_entries)
    }

    pub fn project_indices(&self, key: &ProjectKey) -> Option<Vec<usize>> {
        let mut indices = Vec::new();
        if let Some(base) = self.base.by_project.get(key) {
            indices.extend(
                base.iter()
                    .copied()
                    .filter(|index| self.is_active_index(*index)),
            );
        }
        for (delta_index, delta) in self.deltas.iter().enumerate() {
            let Some(project) = delta.by_project.get(key) else {
                continue;
            };
            let offset = self.delta_offsets[delta_index];
            indices.extend(
                project
                    .iter()
                    .map(|index| offset + index)
                    .filter(|index| self.is_active_index(*index)),
            );
        }
        (!indices.is_empty()).then_some(indices)
    }

    /// Re-derive build-marker ownership over this already-published name
    /// generation. Marker-only refreshes use this instead of reopening SQLite,
    /// so an overlapping index writer cannot leak partially committed rows into
    /// the runtime snapshot.
    pub fn with_project_context(&self, project_context: Option<&ProjectContextIndex>) -> Self {
        let mut builder = name_index_builder::NameIndexBuilder::new(project_context);
        for index in self.active_indices() {
            builder.push_ref_with_project_context(self.entry(index));
        }
        let mut rebuilt = builder.finish();
        rebuilt.direct_include_overrides = self.direct_include_overrides.clone();
        rebuilt
    }

    /// Apply sparse first-layer external evidence from a request-local dirty
    /// include graph. This is an O(changed external paths) clone and leaves the
    /// compact base/delta segments shared.
    pub fn with_direct_include_overrides(&self, overrides: &HashMap<String, bool>) -> Self {
        if overrides.is_empty() {
            return Self {
                base: self.base.clone(),
                deltas: self.deltas.clone(),
                path_overrides: self.path_overrides.clone(),
                delta_offsets: self.delta_offsets.clone(),
                active_len: self.active_len,
                slot_len: self.slot_len,
                direct_include_overrides: self.direct_include_overrides.clone(),
                all_workspace_reach: self.all_workspace_reach.clone(),
            };
        }
        let mut merged = self.direct_include_overrides.as_ref().clone();
        merged.extend(
            overrides
                .iter()
                .map(|(path, included)| (path.clone(), *included)),
        );
        Self {
            base: self.base.clone(),
            deltas: self.deltas.clone(),
            path_overrides: self.path_overrides.clone(),
            delta_offsets: self.delta_offsets.clone(),
            active_len: self.active_len,
            slot_len: self.slot_len,
            direct_include_overrides: Arc::new(merged),
            all_workspace_reach: self.all_workspace_reach.clone(),
        }
    }

    fn with_updated_entries(
        &self,
        paths: &HashSet<String>,
        fresh_entries: impl IntoIterator<Item = NameEntry>,
    ) -> Self {
        let fresh_entries: Vec<NameEntry> = fresh_entries.into_iter().collect();
        let fresh_segment = Arc::new(NameSegment::from_entries(fresh_entries));
        let mut deltas = self.deltas.as_ref().clone();
        let delta_index = deltas.len();
        let mut offsets = self.delta_offsets.as_ref().clone();
        offsets.push(self.slot_len);
        let fresh_slots = fresh_segment.entries.len();

        let mut overrides = self.path_overrides.as_ref().clone();
        let mut active_len = self.active_len;
        for path in paths {
            let old_count = match overrides.get(path.as_str()) {
                Some(Some(previous_delta)) => self.deltas[*previous_delta].path_count(path),
                Some(None) => 0,
                None => self.base.path_count(path),
            };
            let fresh_count = fresh_segment.path_count(path);
            active_len = active_len.saturating_sub(old_count) + fresh_count;
            let interned_path = fresh_segment
                .interned_path(path)
                .unwrap_or_else(|| Arc::<str>::from(path.as_str()));
            overrides.insert(interned_path, (fresh_count > 0).then_some(delta_index));
        }

        let mut all_workspace_reach = self.all_workspace_reach.as_ref().clone();
        for path in paths {
            all_workspace_reach.files.remove(path);
        }
        for (path, external) in fresh_segment
            .paths
            .iter()
            .zip(&fresh_segment.path_is_external)
        {
            if !external {
                all_workspace_reach.files.insert(path.to_string());
            }
        }
        deltas.push(fresh_segment);
        Self {
            base: self.base.clone(),
            deltas: Arc::new(deltas),
            path_overrides: Arc::new(overrides),
            delta_offsets: Arc::new(offsets),
            active_len,
            slot_len: self.slot_len + fresh_slots,
            direct_include_overrides: self.direct_include_overrides.clone(),
            all_workspace_reach: Arc::new(all_workspace_reach),
        }
    }

    fn directly_included_for(&self, entry: NameEntryRef<'_>) -> bool {
        if !entry.external {
            return false;
        }
        self.direct_include_overrides
            .get(entry.path)
            .copied()
            .unwrap_or(entry.directly_included)
    }

    /// Entry indices whose lowercased name starts with `needle_lower` (the exact
    /// and prefix tiers), found by binary search over the sorted index. Returns
    /// the same set a full scan would classify as exact/prefix, in sorted order.
    pub fn prefix_candidates(&self, needle_lower: &str) -> Vec<usize> {
        if needle_lower.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        self.extend_prefix_matches(&self.base, 0, needle_lower, &mut out);
        for (delta_index, delta) in self.deltas.iter().enumerate() {
            self.extend_prefix_matches(
                delta,
                self.delta_offsets[delta_index],
                needle_lower,
                &mut out,
            );
        }
        out.sort_by(|left, right| {
            self.entry(*left)
                .lower
                .cmp(self.entry(*right).lower)
                .then_with(|| self.entry(*left).name.cmp(self.entry(*right).name))
        });
        out
    }

    pub fn exact_name_hits_scoped(
        &self,
        name: &str,
        limit: usize,
        scope: Option<&CompletionScope>,
    ) -> Vec<RankedNameHit> {
        if name.is_empty() || limit == 0 {
            return Vec::new();
        }
        let needle = name.to_ascii_lowercase();
        let indices: Vec<usize> = self
            .prefix_candidates(&needle)
            .into_iter()
            .filter(|index| self.entry(*index).lower == needle)
            .collect();
        let ctx_owned: Option<ResolveContext<'_>> = scope.map(|s| s.resolve_context());
        self.rank_indices(&needle, limit, ctx_owned.as_ref(), &indices)
    }

    pub fn len(&self) -> usize {
        self.active_len
    }

    /// In-memory equivalent of `store::kind_counts_by_names_scoped`, restricted
    /// to the colorable kinds (macro / type / enum_constant). Those kinds are
    /// always `role = 'definition'` in the index, so counting entries of those
    /// kinds reproduces the SQL definition-count exactly — without opening the
    /// database on the coloring hot path.
    ///
    /// The in-scope gate is delegated to the shared [`resolver::scope_tier`]
    /// primitive: a colorable definition counts only when its tier is one of
    /// the determinate in-scope tiers (`Current`, `Reachable`, or first-layer
    /// `External`). An open/indeterminate scope routes not-proven-reachable
    /// workspace candidates to `Unknown`, which does **not** count — the
    /// hard-gate suppress-only behavior. `scope = None` (scoping disabled, no
    /// graph, or no current file) preserves the prior unscoped fallback
    /// `workspace OR directly_included` by synthesizing a context whose
    /// reachable set contains every workspace file: workspace → `Reachable`
    /// (colors), first-layer external → `External` (colors), non-first-layer
    /// external → `Global` (does not color). Names with no colorable in-scope
    /// definition are absent from the result (they resolve to no color),
    /// matching the SQL behavior.
    pub fn colorable_kind_counts(
        &self,
        names: &HashSet<&str>,
        scope: Option<&CompletionScope>,
    ) -> HashMap<String, HashMap<String, usize>> {
        use crate::model::ScopeTier;
        let mut counts: HashMap<String, HashMap<String, usize>> = HashMap::new();
        if names.is_empty() {
            return counts;
        }
        // Synthesize a context for the unscoped fallback (scope = None): a
        // closed scope whose reachable set contains every workspace file. The
        // resolver then maps workspace → Reachable, first-layer external →
        // External, non-first-layer external → Global — reproducing the prior
        // `workspace OR directly_included` gate via the shared primitive
        // rather than a per-entry ad-hoc test.
        let ctx_owned: Option<ResolveContext<'_>> = match scope {
            Some(s) => Some(s.resolve_context()),
            None => Some(ResolveContext {
                current_path: None,
                reach: Some(self.all_workspace_reach.as_ref()),
            }),
        };
        let ctx_ref = ctx_owned.as_ref();
        for index in self.active_indices() {
            let entry = self.entry(index);
            let kind = match entry.kind {
                ParserKind::Macro => "macro",
                ParserKind::Type => "type",
                ParserKind::EnumConstant => "enum_constant",
                // Non-colorable kinds never affect `resolve_kind`; skip them.
                _ => continue,
            };
            if !names.contains(&entry.name) {
                continue;
            }
            let tier = resolver::scope_tier(
                entry.path,
                entry.external,
                self.directly_included_for(entry),
                ctx_ref,
            );
            // Hard gate: only determinate in-scope tiers color. Open/indeterminate
            // (`Unknown`) and out-of-scope (`Global`) do not color.
            let in_scope = matches!(
                tier,
                ScopeTier::Current | ScopeTier::Reachable | ScopeTier::External
            );
            if !in_scope {
                continue;
            }
            *counts
                .entry(entry.name.to_string())
                .or_default()
                .entry(kind.to_string())
                .or_insert(0) += 1;
        }
        counts
    }

    /// Return up to `limit` matching symbol ids, best match first.
    pub fn search(&self, query: &str, limit: usize) -> Vec<i64> {
        self.search_ranked(query, limit)
            .into_iter()
            .map(|hit| hit.id)
            .collect()
    }

    /// Return up to `limit` matching symbol names with their ranking metadata.
    ///
    /// Unscoped fast path: when the exact/prefix candidates already fill the
    /// limit, no lower-scored fuzzy match (boundary-substring 650 at best) can
    /// enter the unscoped top-N (the minimum exact/prefix score is 750), so the
    /// full scan is skipped via the prefix index. Otherwise falls back to the
    /// full scan, which is identical to scoped search with `scope = None`.
    pub fn search_ranked(&self, query: &str, limit: usize) -> Vec<RankedNameHit> {
        let trimmed = query.trim();
        if !trimmed.is_empty() && limit > 0 {
            let needle = trimmed.to_ascii_lowercase();
            let candidates = self.prefix_candidates(&needle);
            if candidates.len() >= limit {
                return self.rank_indices(&needle, limit, None, &candidates);
            }
        }
        self.search_ranked_scoped(query, limit, None)
    }

    /// Reachability-scoped variant of [`search_ranked`]. When `scope` is set,
    /// candidates are re-ranked by whether their defining file is the current
    /// file, reachable via `#include`, or neither — without filtering any out.
    pub fn search_ranked_scoped(
        &self,
        query: &str,
        limit: usize,
        scope: Option<&CompletionScope>,
    ) -> Vec<RankedNameHit> {
        self.search_ranked_scoped_pooled(query, limit, scope, None)
            .0
    }

    /// Pooled/narrowable scoped search. Returns the ranked hits plus a
    /// *tier-agnostic* candidate pool: every entry whose `score_match` is `Some`
    /// for `query`, regardless of the short-prefix recall gate. Because a prefix
    /// of a subsequence is itself a subsequence, the matches of any extending
    /// prefix are a subset of this pool — so a follow-up keystroke can re-score
    /// `prior_pool` instead of the whole table and still produce identical hits.
    ///
    /// `prior_pool = Some(pool)` restricts the scan to those indices (narrowing);
    /// `None` scans the whole table (a cold query). Callers must only narrow when
    /// the new prefix extends the prefix that produced `prior_pool`.
    ///
    /// Ranking is strict-tier lexicographic via [`resolver::pack_score`]: tier
    /// dominates `base_match` (fuzzy match quality), which dominates locality.
    /// The narrowing pool / prefix-index fast paths are unchanged — they gate on
    /// `base_match`, which is unchanged per entry, so pooling stays valid.
    pub fn search_ranked_scoped_pooled(
        &self,
        query: &str,
        limit: usize,
        scope: Option<&CompletionScope>,
        prior_pool: Option<&[usize]>,
    ) -> (Vec<RankedNameHit>, Vec<usize>) {
        let ctx_owned: Option<ResolveContext<'_>> = scope.map(|s| s.resolve_context());
        let ctx_ref = ctx_owned.as_ref();
        let query = query.trim();
        if query.is_empty() {
            // Empty query: rank by tier first, then name. The packed score
            // encodes (tier, 0, locality) so sorting by score desc reproduces
            // the strict-tier order; ties on tier break by name asc.
            let scored: Vec<ScoredCandidate> = self
                .active_indices()
                .map(|index| {
                    let entry = self.entry(index);
                    let tier = resolver::scope_tier(
                        entry.path,
                        entry.external,
                        self.directly_included_for(entry),
                        ctx_ref,
                    );
                    let loc = resolver::locality(entry.path, ctx_ref.and_then(|c| c.current_path));
                    let score = resolver::pack_score(tier, 0, loc);
                    ScoredCandidate {
                        score,
                        name_len: entry.name.len(),
                        index,
                        tier,
                        base_match: 0,
                    }
                })
                .collect();
            let hits = self.scored_to_hits(top_scored(scored, limit, self));
            // An empty query establishes no usable narrowing base.
            return (hits, Vec::new());
        }

        let needle = query.to_ascii_lowercase();
        // Short-prefix recall tightening (D3): for needles shorter than 3
        // characters, require a minimum raw score of 650 so only exact, prefix,
        // and word-boundary-substring hits qualify. Plain substrings (500) and
        // all subsequence tiers (400/200) are dropped, eliminating the
        // random-looking long tail at 2 chars. At len >= 3 the full tier set
        // (including camelCase-initials subsequences) is restored. The
        // threshold is applied to the raw `score_match` output (the per-entry
        // `base_match`), before tier/locality packing, so an external
        // boundary-substr hit still passes.
        let min_score = if needle.len() < SHORT_PREFIX_MIN_LEN {
            SHORT_PREFIX_MIN_SCORE
        } else {
            0
        };
        let mut scored: Vec<ScoredCandidate> = Vec::new();
        let mut pool: Vec<usize> = Vec::new();
        match prior_pool {
            Some(indices) => {
                for &i in indices {
                    self.consider(i, &needle, min_score, ctx_ref, &mut scored, &mut pool);
                }
            }
            None => {
                for i in self.active_indices() {
                    self.consider(i, &needle, min_score, ctx_ref, &mut scored, &mut pool);
                }
            }
        }

        let hits = self.rank_scored(scored, limit, ctx_ref);
        (hits, pool)
    }

    #[allow(dead_code)]
    pub fn search_completion_recall_pooled(
        &self,
        query: &str,
        quotas: CompletionRecallQuotas,
        scope: Option<&CompletionScope>,
        prior_pool: Option<&[usize]>,
    ) -> (Vec<RankedNameHit>, Vec<usize>, CompletionRecallMetrics) {
        self.search_completion_recall_pooled_with_project(query, quotas, scope, None, prior_pool)
    }

    pub fn search_completion_recall_pooled_with_project(
        &self,
        query: &str,
        quotas: CompletionRecallQuotas,
        scope: Option<&CompletionScope>,
        active_project: Option<&ProjectKey>,
        prior_pool: Option<&[usize]>,
    ) -> (Vec<RankedNameHit>, Vec<usize>, CompletionRecallMetrics) {
        let total_limit = quotas.total_indexed;
        let (scored, pool) = self.scored_pool_for_query(query, scope, prior_pool);
        let reserved = quotas
            .reachable
            .saturating_add(quotas.external)
            .saturating_add(quotas.unknown)
            .saturating_add(quotas.global)
            .saturating_add(quotas.same_project);
        let global_top = top_scored(scored.clone(), total_limit.saturating_add(reserved), self);

        let mut selected_indices = HashSet::new();
        let mut selected = Vec::new();
        let reachable = top_scored(
            scored
                .iter()
                .copied()
                .filter(|candidate| channel_for_tier(candidate.tier) == ScopeChannel::Reachable)
                .collect(),
            quotas.reachable,
            self,
        );
        take_channel(
            &reachable,
            ScopeChannel::Reachable,
            quotas.reachable,
            &mut selected_indices,
            &mut selected,
        );
        let same_project = active_project
            .and_then(|key| self.project_indices(key))
            .map(|indices| {
                top_scored(
                    scored
                        .iter()
                        .copied()
                        .filter(|candidate| indices.binary_search(&candidate.index).is_ok())
                        .collect(),
                    quotas.same_project,
                    self,
                )
            })
            .unwrap_or_default();
        take_same_project(
            self,
            &same_project,
            active_project,
            quotas.same_project,
            &mut selected_indices,
            &mut selected,
        );
        for (channel, quota) in [
            (ScopeChannel::External, quotas.external),
            (ScopeChannel::Unknown, quotas.unknown),
            (ScopeChannel::Global, quotas.global),
        ] {
            let channel_top = top_scored(
                scored
                    .iter()
                    .copied()
                    .filter(|candidate| channel_for_tier(candidate.tier) == channel)
                    .collect(),
                quota,
                self,
            );
            take_channel(
                &channel_top,
                channel,
                quota,
                &mut selected_indices,
                &mut selected,
            );
        }

        for candidate in &global_top {
            if selected.len() >= total_limit {
                break;
            }
            if selected_indices.insert(candidate.index) {
                selected.push(*candidate);
            }
        }

        sort_scored(&mut selected, self);
        selected.truncate(total_limit);
        let hits = self.scored_to_hits(selected);
        let metrics = recall_metrics(&hits, pool.len(), active_project);
        (hits, pool, metrics)
    }

    /// Score entry `i` against `needle`: push it into the tier-agnostic `pool`
    /// when it matches at all, and into `scored` (with the resolver's packed
    /// sort key) when it also clears the short-prefix gate. The packed score
    /// encodes `(tier, base_match, locality)` so tier strictly dominates
    /// `base_match`; the pool gates only on `base_match` (unchanged per entry),
    /// so narrowing stays valid across keystrokes.
    fn consider(
        &self,
        i: usize,
        needle: &str,
        min_score: i32,
        ctx: Option<&ResolveContext<'_>>,
        scored: &mut Vec<ScoredCandidate>,
        pool: &mut Vec<usize>,
    ) {
        if !self.is_active_index(i) {
            return;
        }
        let entry = self.entry(i);
        if let Some(base_match) = score_match(needle, entry) {
            pool.push(i);
            if base_match < min_score {
                return;
            }
            let tier = resolver::scope_tier(
                entry.path,
                entry.external,
                self.directly_included_for(entry),
                ctx,
            );
            let loc = resolver::locality(entry.path, ctx.and_then(|c| c.current_path));
            let score = resolver::pack_score(tier, base_match, loc);
            scored.push(ScoredCandidate {
                score,
                name_len: entry.name.len(),
                index: i,
                tier,
                base_match,
            });
        }
    }

    /// Rank a set of candidate indices for the unscoped fast path: score, sort,
    /// and truncate exactly as the full scan would.
    fn rank_indices(
        &self,
        needle: &str,
        limit: usize,
        ctx: Option<&ResolveContext<'_>>,
        candidates: &[usize],
    ) -> Vec<RankedNameHit> {
        let mut scored: Vec<ScoredCandidate> = Vec::new();
        let mut pool: Vec<usize> = Vec::new();
        for &i in candidates {
            self.consider(i, needle, 0, ctx, &mut scored, &mut pool);
        }
        self.rank_scored(scored, limit, ctx)
    }

    /// Sort `(score, name_len, index)` tuples best-first and resolve them into
    /// `RankedNameHit`s, truncated to `limit`. The `score` is the resolver's
    /// packed key; the hit also carries the per-entry `tier` and `base_match`
    /// so callers can dedup by `(tier, confidence)` and derive labels without
    /// re-deriving the tier.
    fn rank_scored(
        &self,
        scored: Vec<ScoredCandidate>,
        limit: usize,
        _ctx: Option<&ResolveContext<'_>>,
    ) -> Vec<RankedNameHit> {
        self.scored_to_hits(top_scored(scored, limit, self))
    }

    fn scored_pool_for_query(
        &self,
        query: &str,
        scope: Option<&CompletionScope>,
        prior_pool: Option<&[usize]>,
    ) -> (Vec<ScoredCandidate>, Vec<usize>) {
        let ctx_owned: Option<ResolveContext<'_>> = scope.map(|s| s.resolve_context());
        let ctx_ref = ctx_owned.as_ref();
        let query = query.trim();
        if query.is_empty() {
            let mut scored: Vec<ScoredCandidate> = self
                .active_indices()
                .map(|index| {
                    let entry = self.entry(index);
                    let tier = resolver::scope_tier(
                        entry.path,
                        entry.external,
                        self.directly_included_for(entry),
                        ctx_ref,
                    );
                    let loc = resolver::locality(entry.path, ctx_ref.and_then(|c| c.current_path));
                    ScoredCandidate {
                        score: resolver::pack_score(tier, 0, loc),
                        name_len: entry.name.len(),
                        index,
                        tier,
                        base_match: 0,
                    }
                })
                .collect();
            sort_scored(&mut scored, self);
            return (scored, Vec::new());
        }

        let needle = query.to_ascii_lowercase();
        let min_score = if needle.len() < SHORT_PREFIX_MIN_LEN {
            SHORT_PREFIX_MIN_SCORE
        } else {
            0
        };
        let mut scored = Vec::new();
        let mut pool = Vec::new();
        match prior_pool {
            Some(indices) => {
                for &i in indices {
                    self.consider(i, &needle, min_score, ctx_ref, &mut scored, &mut pool);
                }
            }
            None => {
                for i in self.active_indices() {
                    self.consider(i, &needle, min_score, ctx_ref, &mut scored, &mut pool);
                }
            }
        }
        (scored, pool)
    }

    fn scored_to_hits(&self, scored: Vec<ScoredCandidate>) -> Vec<RankedNameHit> {
        scored
            .into_iter()
            .map(|candidate| {
                let entry = self.entry(candidate.index);
                RankedNameHit {
                    id: entry.id,
                    score: candidate.score,
                    tier: candidate.tier,
                    base_match: candidate.base_match,
                    name_len: candidate.name_len,
                    name: entry.name.to_string(),
                    kind: entry.kind,
                    project_key: entry.project_key.cloned(),
                }
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScopeChannel {
    Reachable,
    External,
    Unknown,
    Global,
}

fn channel_for_tier(tier: ScopeTier) -> ScopeChannel {
    match tier {
        ScopeTier::Current | ScopeTier::Reachable => ScopeChannel::Reachable,
        ScopeTier::External => ScopeChannel::External,
        ScopeTier::Unknown => ScopeChannel::Unknown,
        ScopeTier::Global => ScopeChannel::Global,
    }
}

fn take_channel(
    scored: &[ScoredCandidate],
    channel: ScopeChannel,
    quota: usize,
    selected_indices: &mut HashSet<usize>,
    selected: &mut Vec<ScoredCandidate>,
) {
    if quota == 0 {
        return;
    }
    let mut taken = 0;
    for candidate in scored {
        if taken >= quota {
            break;
        }
        if channel_for_tier(candidate.tier) != channel {
            continue;
        }
        if selected_indices.insert(candidate.index) {
            selected.push(*candidate);
            taken += 1;
        }
    }
}

fn take_same_project(
    table: &NameTable,
    scored: &[ScoredCandidate],
    active_project: Option<&ProjectKey>,
    quota: usize,
    selected_indices: &mut HashSet<usize>,
    selected: &mut Vec<ScoredCandidate>,
) {
    let Some(key) = active_project else {
        return;
    };
    let Some(project_indices) = table.project_indices(key) else {
        return;
    };
    if quota == 0 {
        return;
    }
    let mut taken = 0;
    for candidate in scored {
        if taken >= quota {
            break;
        }
        if project_indices.binary_search(&candidate.index).is_err() {
            continue;
        }
        if selected_indices.insert(candidate.index) {
            selected.push(*candidate);
            taken += 1;
        }
    }
}

fn sort_scored(scored: &mut [ScoredCandidate], table: &NameTable) {
    scored.sort_by(|a, b| scored_order(a, b, table));
}

fn scored_order(a: &ScoredCandidate, b: &ScoredCandidate, table: &NameTable) -> std::cmp::Ordering {
    b.score
        .cmp(&a.score)
        .then(a.name_len.cmp(&b.name_len))
        .then_with(|| table.entry(a.index).name.cmp(table.entry(b.index).name))
}

fn top_scored(
    mut scored: Vec<ScoredCandidate>,
    limit: usize,
    table: &NameTable,
) -> Vec<ScoredCandidate> {
    if limit == 0 {
        return Vec::new();
    }
    if scored.len() > limit {
        scored.select_nth_unstable_by(limit, |a, b| scored_order(a, b, table));
        scored.truncate(limit);
    }
    sort_scored(&mut scored, table);
    scored
}

fn recall_metrics(
    hits: &[RankedNameHit],
    pool_total: usize,
    active_project: Option<&ProjectKey>,
) -> CompletionRecallMetrics {
    let mut metrics = CompletionRecallMetrics {
        pool_total,
        indexed_returned: hits.len(),
        ..CompletionRecallMetrics::default()
    };
    for hit in hits {
        match channel_for_tier(hit.tier) {
            ScopeChannel::Reachable => metrics.reachable += 1,
            ScopeChannel::External => metrics.external += 1,
            ScopeChannel::Unknown => metrics.unknown += 1,
            ScopeChannel::Global => metrics.global += 1,
        }
        if active_project.is_some() && hit.project_key.as_ref() == active_project {
            metrics.same_project += 1;
        }
    }
    metrics
}

/// Build a `NameEntry` from a loader tuple
/// `(id, name, external, path, kind, directly_included)`.
#[allow(dead_code)]
fn name_entry(
    (id, name, external, path, kind, directly_included): (i64, String, bool, String, String, bool),
) -> NameEntry {
    name_entry_parts(id, name, external, path, kind, directly_included, None)
}

fn name_entry_from_row(row: NameTableSymbolRow) -> NameEntry {
    name_entry_from_row_with_project_context(row, None)
}

fn name_entry_from_row_with_project_context(
    row: NameTableSymbolRow,
    project_context: Option<&ProjectContextIndex>,
) -> NameEntry {
    let project_key = if row.external {
        None
    } else {
        project_context.and_then(|index| index.nearest_for_file(&row.path))
    };
    name_entry_parts(
        row.symbol_id,
        row.label,
        row.external,
        row.path,
        row.kind,
        row.directly_included,
        project_key,
    )
}

fn name_entries_from_rows_with_project_context(
    rows: Vec<NameTableSymbolRow>,
    project_context: Option<&ProjectContextIndex>,
) -> Vec<NameEntry> {
    let mut project_by_path = HashMap::<String, Option<ProjectKey>>::new();
    rows.into_iter()
        .map(|row| {
            let project_key = if row.external {
                None
            } else if let Some(project) = project_by_path.get(&row.path) {
                project.clone()
            } else {
                let project = project_context.and_then(|index| index.nearest_for_file(&row.path));
                project_by_path.insert(row.path.clone(), project.clone());
                project
            };
            name_entry_parts(
                row.symbol_id,
                row.label,
                row.external,
                row.path,
                row.kind,
                row.directly_included,
                project_key,
            )
        })
        .collect()
}

fn name_entry_parts(
    id: i64,
    name: String,
    external: bool,
    path: String,
    kind: String,
    directly_included: bool,
    project_key: Option<ProjectKey>,
) -> NameEntry {
    let lower = name.to_ascii_lowercase();
    NameEntry {
        id,
        name: Arc::from(name),
        lower: Arc::from(lower),
        external,
        directly_included,
        path: Arc::from(path),
        kind: crate::parser::kind_from_str(&kind),
        project_key,
    }
}

/// Score a single name against an already-lowercased query. `None` means no
/// match (not even a subsequence). Higher is better.
fn score_match(needle: &str, entry: NameEntryRef<'_>) -> Option<i32> {
    let hay = entry.lower;

    if hay == needle {
        return Some(1000);
    }
    if hay.starts_with(needle) {
        return Some(800);
    }
    if let Some(at) = hay.find(needle) {
        let boundary = is_boundary(entry.name.as_bytes(), at);
        return Some(if boundary { 650 } else { 500 });
    }
    subsequence_match(needle.as_bytes(), entry.name.as_bytes(), hay.as_bytes())
        .map(|all_boundary| if all_boundary { 400 } else { 200 })
}

/// Greedy left-to-right subsequence test. Returns `Some(all_on_boundary)` when
/// `needle` is a subsequence of the name, where `all_on_boundary` is true if
/// every matched character landed on a word boundary (initials-style match).
fn subsequence_match(needle: &[u8], orig: &[u8], lower: &[u8]) -> Option<bool> {
    let mut qi = 0;
    let mut all_boundary = true;
    let mut i = 0;
    while i < lower.len() && qi < needle.len() {
        if lower[i] == needle[qi] {
            if !is_boundary(orig, i) {
                all_boundary = false;
            }
            qi += 1;
        }
        i += 1;
    }
    if qi == needle.len() {
        Some(all_boundary)
    } else {
        None
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
