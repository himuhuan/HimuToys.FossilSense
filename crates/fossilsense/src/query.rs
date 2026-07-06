//! Protocol-agnostic query logic: in-memory fuzzy name table, definition
//! ranking, cursor-word extraction and symbol-kind mapping. Kept free of
//! `tower-lsp` request types so the scoring/ranking can be unit-tested.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::model::ScopeTier;
use crate::parser::SymbolKind as ParserKind;
use crate::reachability::ReachScope;
use crate::resolver::{self, ResolveContext};
use crate::store::views::NameTableSymbolRow;

#[allow(dead_code)]
mod current_file_overlay;
mod definitions;
mod hover;
mod local_completion;
mod lsp_kinds;
mod signatures;
mod text;

#[allow(unused_imports)]
pub use current_file_overlay::{current_file_overlay_candidates, CurrentFileOverlayCandidate};
pub use definitions::rank_definitions_into_candidates_with_scope;
pub use hover::{
    hover_markdown_for_candidate, leading_comment_markdown, rank_hover_candidates,
    RankedHoverCandidate, HOVER_CANDIDATE_LIMIT,
};
pub use local_completion::{local_completion_candidates, LocalCompletionCandidate};
pub use lsp_kinds::{lsp_kind_from_parser, lsp_symbol_kind};
pub use signatures::{
    call_context_at, rank_function_signature_candidates, signature_parts, signature_parts_for_name,
    CallContext, ParameterSpan, RankedSignatureCandidate, SignatureParts, SIGNATURE_HELP_LIMIT,
};
use text::is_boundary;
pub use text::{
    byte_offset_at, completion_prefix_at, completion_word_score, is_member_completion_context,
    member_access_chain_at, word_at,
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
}

// ===========================================================================
// In-memory fuzzy name table
// ===========================================================================

#[derive(Clone)]
struct NameEntry {
    id: i64,
    name: String,
    lower: String,
    external: bool,
    /// First-layer external header (`#include`d directly by a workspace file).
    /// Carried so in-memory coloring can reproduce the SQL unscoped fallback's
    /// `workspace OR directly_included` filter; always `false` for workspace.
    directly_included: bool,
    path: String,
    kind: ParserKind,
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
}

impl CompletionRecallQuotas {
    pub fn default_for_completion_limit(limit: usize) -> Self {
        Self {
            total_indexed: limit.saturating_mul(3),
            reachable: limit,
            external: limit / 2,
            unknown: limit / 2,
            global: limit,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CompletionRecallMetrics {
    pub reachable: usize,
    pub external: usize,
    pub unknown: usize,
    pub global: usize,
    pub pool_total: usize,
    pub indexed_returned: usize,
}

impl CompletionRecallMetrics {
    pub fn merge_from(&mut self, other: CompletionRecallMetrics) {
        self.reachable += other.reachable;
        self.external += other.external;
        self.unknown += other.unknown;
        self.global += other.global;
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

/// All symbol names loaded into memory for sub-second fuzzy search. Built once
/// per workspace after an index pass; `search` returns symbol ids that callers
/// resolve back to full records via the store.
pub struct NameTable {
    entries: Vec<NameEntry>,
    /// Entry indices sorted by lowercased name, enabling binary-search retrieval
    /// of the exact/prefix tiers without a full scan.
    sorted: Vec<usize>,
    /// Cached unscoped coloring fallback: all workspace files in a closed
    /// reachability set. Reused by `colorable_kind_counts(None)` instead of
    /// rebuilding the same path set on every semantic-token request.
    all_workspace_reach: Arc<ReachScope>,
}

/// Entry indices sorted by `(lowercased name, original name)` for prefix search.
fn sorted_indices(entries: &[NameEntry]) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..entries.len()).collect();
    idx.sort_by(|&a, &b| {
        entries[a]
            .lower
            .cmp(&entries[b].lower)
            .then_with(|| entries[a].name.cmp(&entries[b].name))
    });
    idx
}

fn all_workspace_reach(entries: &[NameEntry]) -> ReachScope {
    ReachScope {
        files: entries
            .iter()
            .filter(|entry| !entry.external)
            .map(|entry| entry.path.clone())
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

    pub fn build_from_rows(rows: Vec<NameTableSymbolRow>) -> Self {
        let entries: Vec<NameEntry> = rows.into_iter().map(name_entry_from_row).collect();
        Self::from_entries(entries)
    }

    fn from_entries(entries: Vec<NameEntry>) -> Self {
        let sorted = sorted_indices(&entries);
        let all_workspace_reach = Arc::new(all_workspace_reach(&entries));
        Self {
            entries,
            sorted,
            all_workspace_reach,
        }
    }

    #[allow(dead_code)]
    pub fn with_updated_paths(
        &self,
        paths: &HashSet<String>,
        names: Vec<(i64, String, bool, String, String, bool)>,
    ) -> Self {
        let fresh_entries = names.into_iter().map(name_entry);
        self.with_updated_entries(paths, fresh_entries)
    }

    pub fn with_updated_path_rows(
        &self,
        paths: &HashSet<String>,
        rows: Vec<NameTableSymbolRow>,
    ) -> Self {
        let fresh_entries = rows.into_iter().map(name_entry_from_row);
        self.with_updated_entries(paths, fresh_entries)
    }

    fn with_updated_entries(
        &self,
        paths: &HashSet<String>,
        fresh_entries: impl IntoIterator<Item = NameEntry>,
    ) -> Self {
        let mut entries: Vec<NameEntry> = self
            .entries
            .iter()
            .filter(|entry| !paths.contains(&entry.path))
            .cloned()
            .collect();
        entries.extend(fresh_entries);
        Self::from_entries(entries)
    }

    /// Entry indices whose lowercased name starts with `needle_lower` (the exact
    /// and prefix tiers), found by binary search over the sorted index. Returns
    /// the same set a full scan would classify as exact/prefix, in sorted order.
    pub fn prefix_candidates(&self, needle_lower: &str) -> Vec<usize> {
        if needle_lower.is_empty() {
            return Vec::new();
        }
        let start = self
            .sorted
            .partition_point(|&i| self.entries[i].lower.as_str() < needle_lower);
        let mut out = Vec::new();
        for &i in &self.sorted[start..] {
            if self.entries[i].lower.starts_with(needle_lower) {
                out.push(i);
            } else {
                break;
            }
        }
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
        let start = self
            .sorted
            .partition_point(|&i| self.entries[i].lower.as_str() < needle.as_str());
        let mut indices = Vec::new();
        for &i in &self.sorted[start..] {
            match self.entries[i].lower.as_str().cmp(needle.as_str()) {
                std::cmp::Ordering::Equal => indices.push(i),
                std::cmp::Ordering::Greater => break,
                std::cmp::Ordering::Less => {}
            }
        }
        let ctx_owned: Option<ResolveContext<'_>> = scope.map(|s| s.resolve_context());
        self.rank_indices(&needle, limit, ctx_owned.as_ref(), &indices)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
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
        for entry in &self.entries {
            let kind = match entry.kind {
                ParserKind::Macro => "macro",
                ParserKind::Type => "type",
                ParserKind::EnumConstant => "enum_constant",
                // Non-colorable kinds never affect `resolve_kind`; skip them.
                _ => continue,
            };
            if !names.contains(entry.name.as_str()) {
                continue;
            }
            let tier = resolver::scope_tier(
                &entry.path,
                entry.external,
                entry.directly_included,
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
                .entry(entry.name.clone())
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
            let mut entries: Vec<(ScoredCandidate, &NameEntry)> = self
                .entries
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    let tier = resolver::scope_tier(
                        &entry.path,
                        entry.external,
                        entry.directly_included,
                        ctx_ref,
                    );
                    let loc = resolver::locality(&entry.path, ctx_ref.and_then(|c| c.current_path));
                    let score = resolver::pack_score(tier, 0, loc);
                    (
                        ScoredCandidate {
                            score,
                            name_len: entry.name.len(),
                            index,
                            tier,
                            base_match: 0,
                        },
                        entry,
                    )
                })
                .collect();
            entries.sort_by(|a, b| {
                b.0.score
                    .cmp(&a.0.score)
                    .then(a.0.name_len.cmp(&b.0.name_len))
                    .then_with(|| a.1.name.cmp(&b.1.name))
            });
            let hits = entries
                .into_iter()
                .take(limit)
                .map(|(candidate, entry)| RankedNameHit {
                    id: entry.id,
                    score: candidate.score,
                    tier: candidate.tier,
                    base_match: candidate.base_match,
                    name_len: candidate.name_len,
                    name: entry.name.clone(),
                    kind: entry.kind,
                })
                .collect();
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
                for i in 0..self.entries.len() {
                    self.consider(i, &needle, min_score, ctx_ref, &mut scored, &mut pool);
                }
            }
        }

        let hits = self.rank_scored(scored, limit, ctx_ref);
        (hits, pool)
    }

    pub fn search_completion_recall_pooled(
        &self,
        query: &str,
        quotas: CompletionRecallQuotas,
        scope: Option<&CompletionScope>,
        prior_pool: Option<&[usize]>,
    ) -> (Vec<RankedNameHit>, Vec<usize>, CompletionRecallMetrics) {
        let total_limit = quotas.total_indexed;
        let (mut scored, pool) = self.scored_pool_for_query(query, scope, prior_pool);
        sort_scored(&mut scored, &self.entries);

        let mut selected_indices = HashSet::new();
        let mut selected = Vec::new();
        take_channel(
            &scored,
            ScopeChannel::Reachable,
            quotas.reachable,
            &mut selected_indices,
            &mut selected,
        );
        take_channel(
            &scored,
            ScopeChannel::External,
            quotas.external,
            &mut selected_indices,
            &mut selected,
        );
        take_channel(
            &scored,
            ScopeChannel::Unknown,
            quotas.unknown,
            &mut selected_indices,
            &mut selected,
        );
        take_channel(
            &scored,
            ScopeChannel::Global,
            quotas.global,
            &mut selected_indices,
            &mut selected,
        );

        for candidate in &scored {
            if selected.len() >= total_limit {
                break;
            }
            if selected_indices.insert(candidate.index) {
                selected.push(*candidate);
            }
        }

        sort_scored(&mut selected, &self.entries);
        selected.truncate(total_limit);
        let hits = self.scored_to_hits(selected);
        let metrics = recall_metrics(&hits, pool.len());
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
        let entry = &self.entries[i];
        if let Some(base_match) = score_match(needle, entry) {
            pool.push(i);
            if base_match < min_score {
                return;
            }
            let tier =
                resolver::scope_tier(&entry.path, entry.external, entry.directly_included, ctx);
            let loc = resolver::locality(&entry.path, ctx.and_then(|c| c.current_path));
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
        mut scored: Vec<ScoredCandidate>,
        limit: usize,
        _ctx: Option<&ResolveContext<'_>>,
    ) -> Vec<RankedNameHit> {
        sort_scored(&mut scored, &self.entries);
        scored.truncate(limit);
        self.scored_to_hits(scored)
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
                .entries
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    let tier = resolver::scope_tier(
                        &entry.path,
                        entry.external,
                        entry.directly_included,
                        ctx_ref,
                    );
                    let loc = resolver::locality(&entry.path, ctx_ref.and_then(|c| c.current_path));
                    ScoredCandidate {
                        score: resolver::pack_score(tier, 0, loc),
                        name_len: entry.name.len(),
                        index,
                        tier,
                        base_match: 0,
                    }
                })
                .collect();
            sort_scored(&mut scored, &self.entries);
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
                for i in 0..self.entries.len() {
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
                let entry = &self.entries[candidate.index];
                RankedNameHit {
                    id: entry.id,
                    score: candidate.score,
                    tier: candidate.tier,
                    base_match: candidate.base_match,
                    name_len: candidate.name_len,
                    name: entry.name.clone(),
                    kind: entry.kind,
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

fn sort_scored(scored: &mut [ScoredCandidate], entries: &[NameEntry]) {
    scored.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(a.name_len.cmp(&b.name_len))
            .then_with(|| entries[a.index].name.cmp(&entries[b.index].name))
    });
}

fn recall_metrics(hits: &[RankedNameHit], pool_total: usize) -> CompletionRecallMetrics {
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
    }
    metrics
}

/// Build a `NameEntry` from a loader tuple
/// `(id, name, external, path, kind, directly_included)`.
#[allow(dead_code)]
fn name_entry(
    (id, name, external, path, kind, directly_included): (i64, String, bool, String, String, bool),
) -> NameEntry {
    name_entry_parts(id, name, external, path, kind, directly_included)
}

fn name_entry_from_row(row: NameTableSymbolRow) -> NameEntry {
    name_entry_parts(
        row.symbol_id,
        row.label,
        row.external,
        row.path,
        row.kind,
        row.directly_included,
    )
}

fn name_entry_parts(
    id: i64,
    name: String,
    external: bool,
    path: String,
    kind: String,
    directly_included: bool,
) -> NameEntry {
    let lower = name.to_ascii_lowercase();
    NameEntry {
        id,
        name,
        lower,
        external,
        directly_included,
        path,
        kind: crate::parser::kind_from_str(&kind),
    }
}

/// Score a single name against an already-lowercased query. `None` means no
/// match (not even a subsequence). Higher is better.
fn score_match(needle: &str, entry: &NameEntry) -> Option<i32> {
    let hay = entry.lower.as_str();

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
