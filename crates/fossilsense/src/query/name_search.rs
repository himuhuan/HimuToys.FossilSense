use std::collections::BinaryHeap;

use super::*;

impl NameTable {
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

    #[cfg(test)]
    pub fn exact_name_hits_scoped(
        &self,
        name: &str,
        limit: usize,
        scope: Option<&CompletionScope>,
    ) -> Vec<RankedNameHit> {
        self.exact_name_hits_scoped_filtered(name, limit, scope, None)
    }

    pub fn exact_name_hits_scoped_for_family(
        &self,
        name: &str,
        limit: usize,
        scope: Option<&CompletionScope>,
        semantic_family: crate::semantic_model::SemanticFamily,
    ) -> Vec<RankedNameHit> {
        self.exact_name_hits_scoped_filtered(name, limit, scope, Some(semantic_family))
    }

    fn exact_name_hits_scoped_filtered(
        &self,
        name: &str,
        limit: usize,
        scope: Option<&CompletionScope>,
        semantic_family: Option<crate::semantic_model::SemanticFamily>,
    ) -> Vec<RankedNameHit> {
        if name.is_empty() || limit == 0 {
            return Vec::new();
        }
        let needle = name.to_ascii_lowercase();
        let indices: Vec<usize> = self
            .prefix_candidates(&needle)
            .into_iter()
            .filter(|index| {
                let entry = self.entry(*index);
                entry.lower == needle
                    && semantic_family.is_none_or(|family| entry.semantic_family == family)
            })
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
    /// policy through [`resolver::coloring_scope_allows`]: determinate
    /// in-scope tiers (`Current`, `Reachable`, or first-layer `External`) count,
    /// as do bounded paths in `ReachScope::heuristic_files`. A candidate that
    /// is `Unknown` only because the scope is open does **not** count, so
    /// unresolved includes cannot admit unrelated whole-index definitions.
    /// `scope = None` (scoping disabled, no graph, or no current file)
    /// preserves the prior unscoped fallback
    /// `workspace OR directly_included` by synthesizing a context whose
    /// reachable set contains every workspace file: workspace → `Reachable`
    /// (colors), first-layer external → `External` (colors), non-first-layer
    /// external → `Global` (does not color). Names with no colorable in-scope
    /// definition are absent from the result (they resolve to no color),
    /// matching the SQL behavior.
    #[cfg(test)]
    pub fn colorable_kind_counts(
        &self,
        names: &HashSet<&str>,
        scope: Option<&CompletionScope>,
    ) -> HashMap<String, HashMap<String, usize>> {
        self.colorable_kind_counts_filtered(names, scope, None)
    }

    pub fn colorable_kind_counts_for_family(
        &self,
        names: &HashSet<&str>,
        scope: Option<&CompletionScope>,
        semantic_family: crate::semantic_model::SemanticFamily,
    ) -> HashMap<String, HashMap<String, usize>> {
        self.colorable_kind_counts_filtered(names, scope, Some(semantic_family))
    }

    fn colorable_kind_counts_filtered(
        &self,
        names: &HashSet<&str>,
        scope: Option<&CompletionScope>,
        semantic_family: Option<crate::semantic_model::SemanticFamily>,
    ) -> HashMap<String, HashMap<String, usize>> {
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
                direct_external_files: None,
            }),
        };
        let ctx_ref = ctx_owned.as_ref();
        for index in self.active_indices() {
            let entry = self.entry(index);
            if semantic_family.is_some_and(|family| entry.semantic_family != family) {
                continue;
            }
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
            let in_scope = resolver::coloring_scope_allows(
                entry.path,
                entry.external,
                self.directly_included_for(entry),
                ctx_ref,
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
    #[cfg(test)]
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
        self.search_completion_recall_pooled_with_project_filtered(super::CompletionRecallQuery {
            query,
            quotas,
            scope,
            active_project,
            prior_pool,
            semantic_family: None,
            cancellation: None,
        })
    }

    pub fn search_completion_recall_pooled_with_project_for_family(
        &self,
        query: &str,
        quotas: CompletionRecallQuotas,
        scope: Option<&CompletionScope>,
        active_project: Option<&ProjectKey>,
        prior_pool: Option<&[usize]>,
        semantic_family: crate::semantic_model::SemanticFamily,
    ) -> (Vec<RankedNameHit>, Vec<usize>, CompletionRecallMetrics) {
        self.search_completion_recall_pooled_with_project_filtered(super::CompletionRecallQuery {
            query,
            quotas,
            scope,
            active_project,
            prior_pool,
            semantic_family: Some(semantic_family),
            cancellation: None,
        })
    }

    pub(crate) fn search_completion_recall_pooled_controlled(
        &self,
        query: super::CompletionRecallQuery<'_>,
    ) -> (Vec<RankedNameHit>, Vec<usize>, CompletionRecallMetrics) {
        debug_assert!(query.cancellation.is_some());
        self.search_completion_recall_pooled_with_project_filtered(query)
    }

    fn search_completion_recall_pooled_with_project_filtered(
        &self,
        query: super::CompletionRecallQuery<'_>,
    ) -> (Vec<RankedNameHit>, Vec<usize>, CompletionRecallMetrics) {
        let total_limit = query.quotas.total_indexed;
        let (mut scored, mut pool, mut scan_metrics) = self.scored_pool_for_query(
            query.query,
            query.scope,
            query.prior_pool,
            query.cancellation,
        );
        if scan_metrics.cancelled {
            return cancelled_recall(scan_metrics);
        }
        if scan_metrics.check(query.cancellation) {
            return cancelled_recall(scan_metrics);
        }
        if let Some(semantic_family) = query.semantic_family {
            scored
                .retain(|candidate| self.entry(candidate.index).semantic_family == semantic_family);
            pool.retain(|index| self.entry(*index).semantic_family == semantic_family);
        }
        if scan_metrics.check(query.cancellation) {
            return cancelled_recall(scan_metrics);
        }
        let reserved = query
            .quotas
            .reachable
            .saturating_add(query.quotas.external)
            .saturating_add(query.quotas.unknown)
            .saturating_add(query.quotas.global)
            .saturating_add(query.quotas.same_project);
        let Some(global_top) = top_scored_controlled(
            scored.iter().copied(),
            |_| true,
            total_limit.saturating_add(reserved),
            self,
            query.cancellation,
            &mut scan_metrics,
        ) else {
            return cancelled_recall(scan_metrics);
        };

        let mut selected_indices = HashSet::new();
        let mut selected = Vec::new();
        let Some(reachable) = top_scored_controlled(
            scored.iter().copied(),
            |candidate| channel_for_tier(candidate.tier) == ScopeChannel::Reachable,
            query.quotas.reachable,
            self,
            query.cancellation,
            &mut scan_metrics,
        ) else {
            return cancelled_recall(scan_metrics);
        };
        take_channel(
            &reachable,
            ScopeChannel::Reachable,
            query.quotas.reachable,
            &mut selected_indices,
            &mut selected,
        );
        let same_project = if let Some(indices) = query
            .active_project
            .and_then(|key| self.project_indices(key))
        {
            let Some(top) = top_scored_controlled(
                scored.iter().copied(),
                |candidate| indices.binary_search(&candidate.index).is_ok(),
                query.quotas.same_project,
                self,
                query.cancellation,
                &mut scan_metrics,
            ) else {
                return cancelled_recall(scan_metrics);
            };
            top
        } else {
            Vec::new()
        };
        take_same_project(
            self,
            &same_project,
            query.active_project,
            query.quotas.same_project,
            &mut selected_indices,
            &mut selected,
        );
        for (channel, quota) in [
            (ScopeChannel::External, query.quotas.external),
            (ScopeChannel::Unknown, query.quotas.unknown),
            (ScopeChannel::Global, query.quotas.global),
        ] {
            let Some(channel_top) = top_scored_controlled(
                scored.iter().copied(),
                |candidate| channel_for_tier(candidate.tier) == channel,
                quota,
                self,
                query.cancellation,
                &mut scan_metrics,
            ) else {
                return cancelled_recall(scan_metrics);
            };
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

        if scan_metrics.check(query.cancellation) {
            return cancelled_recall(scan_metrics);
        }
        sort_scored(&mut selected, self);
        selected.truncate(total_limit);
        let hits = self.scored_to_hits(selected);
        let mut metrics = recall_metrics(&hits, pool.len(), query.active_project);
        metrics.entries_inspected = scan_metrics.entries_inspected;
        metrics.selection_entries_inspected = scan_metrics.selection_entries_inspected;
        metrics.cancellation_checks = scan_metrics.cancellation_checks;
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
        cancellation: Option<&dyn super::CompletionQueryCancellation>,
    ) -> (Vec<ScoredCandidate>, Vec<usize>, RecallScanMetrics) {
        let ctx_owned: Option<ResolveContext<'_>> = scope.map(|s| s.resolve_context());
        let ctx_ref = ctx_owned.as_ref();
        let query = query.trim();
        if query.is_empty() {
            let mut scored = Vec::new();
            let mut scan_metrics = RecallScanMetrics::default();
            for index in self.active_indices() {
                if scan_metrics.should_cancel(cancellation) {
                    return (Vec::new(), Vec::new(), scan_metrics);
                }
                let entry = self.entry(index);
                let tier = resolver::scope_tier(
                    entry.path,
                    entry.external,
                    self.directly_included_for(entry),
                    ctx_ref,
                );
                let loc = resolver::locality(entry.path, ctx_ref.and_then(|c| c.current_path));
                scored.push(ScoredCandidate {
                    score: resolver::pack_score(tier, 0, loc),
                    name_len: entry.name.len(),
                    index,
                    tier,
                    base_match: 0,
                });
                scan_metrics.entries_inspected += 1;
            }
            if scan_metrics.cancel_after_scan(cancellation) {
                return (Vec::new(), Vec::new(), scan_metrics);
            }
            sort_scored(&mut scored, self);
            return (scored, Vec::new(), scan_metrics);
        }

        let needle = query.to_ascii_lowercase();
        let min_score = if needle.len() < SHORT_PREFIX_MIN_LEN {
            SHORT_PREFIX_MIN_SCORE
        } else {
            0
        };
        let mut scored = Vec::new();
        let mut pool = Vec::new();
        let mut scan_metrics = RecallScanMetrics::default();
        match prior_pool {
            Some(indices) => {
                for &i in indices {
                    if scan_metrics.should_cancel(cancellation) {
                        return (Vec::new(), Vec::new(), scan_metrics);
                    }
                    self.consider(i, &needle, min_score, ctx_ref, &mut scored, &mut pool);
                    scan_metrics.entries_inspected += 1;
                }
            }
            None => {
                for i in self.active_indices() {
                    if scan_metrics.should_cancel(cancellation) {
                        return (Vec::new(), Vec::new(), scan_metrics);
                    }
                    self.consider(i, &needle, min_score, ctx_ref, &mut scored, &mut pool);
                    scan_metrics.entries_inspected += 1;
                }
            }
        }
        if scan_metrics.cancel_after_scan(cancellation) {
            return (Vec::new(), Vec::new(), scan_metrics);
        }
        (scored, pool, scan_metrics)
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
                    role: entry.role,
                    semantic_family: entry.semantic_family,
                    project_key: entry.project_key.cloned(),
                }
            })
            .collect()
    }
}

fn cancelled_recall(
    scan_metrics: RecallScanMetrics,
) -> (Vec<RankedNameHit>, Vec<usize>, CompletionRecallMetrics) {
    (
        Vec::new(),
        Vec::new(),
        CompletionRecallMetrics {
            entries_inspected: scan_metrics.entries_inspected,
            selection_entries_inspected: scan_metrics.selection_entries_inspected,
            cancellation_checks: scan_metrics.cancellation_checks,
            cancelled: true,
            ..CompletionRecallMetrics::default()
        },
    )
}

#[derive(Default)]
struct RecallScanMetrics {
    entries_inspected: usize,
    selection_entries_inspected: usize,
    cancellation_checks: usize,
    cancelled: bool,
}

impl RecallScanMetrics {
    fn should_cancel(
        &mut self,
        cancellation: Option<&dyn super::CompletionQueryCancellation>,
    ) -> bool {
        if !self
            .entries_inspected
            .is_multiple_of(super::COMPLETION_CANCELLATION_CHECK_INTERVAL)
        {
            return false;
        }
        self.check(cancellation)
    }

    fn cancel_after_scan(
        &mut self,
        cancellation: Option<&dyn super::CompletionQueryCancellation>,
    ) -> bool {
        self.check(cancellation)
    }

    fn check(&mut self, cancellation: Option<&dyn super::CompletionQueryCancellation>) -> bool {
        let Some(cancellation) = cancellation else {
            return false;
        };
        self.cancellation_checks += 1;
        self.cancelled = cancellation.is_cancelled();
        self.cancelled
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

pub(super) fn sort_scored(scored: &mut [ScoredCandidate], table: &NameTable) {
    scored.sort_by(|a, b| scored_order(a, b, table));
}

fn scored_order(a: &ScoredCandidate, b: &ScoredCandidate, table: &NameTable) -> std::cmp::Ordering {
    let a_entry = table.entry(a.index);
    let b_entry = table.entry(b.index);
    b.score
        .cmp(&a.score)
        .then(a.name_len.cmp(&b.name_len))
        .then_with(|| a_entry.name.cmp(b_entry.name))
        // Role is a same-logical-name recall tie-break only; it must not
        // reorder different labels before fuzzy/name quality has spoken.
        .then_with(|| {
            completion_role_recall_priority(b_entry.role)
                .cmp(&completion_role_recall_priority(a_entry.role))
        })
        .then_with(|| a_entry.path.cmp(b_entry.path))
        .then_with(|| a_entry.id.cmp(&b_entry.id))
}

fn completion_role_recall_priority(role: SymbolRole) -> u8 {
    match role {
        SymbolRole::Declaration => 4,
        SymbolRole::TentativeDefinition => 3,
        SymbolRole::Definition => 2,
        SymbolRole::UnknownDeclarationOrDefinition => 1,
    }
}

fn top_scored_controlled<I, F>(
    scored: I,
    mut include: F,
    limit: usize,
    table: &NameTable,
    cancellation: Option<&dyn super::CompletionQueryCancellation>,
    scan_metrics: &mut RecallScanMetrics,
) -> Option<Vec<ScoredCandidate>>
where
    I: IntoIterator<Item = ScoredCandidate>,
    F: FnMut(&ScoredCandidate) -> bool,
{
    if limit == 0 {
        return Some(Vec::new());
    }
    if scan_metrics.check(cancellation) {
        return None;
    }

    let mut retained = BinaryHeap::with_capacity(limit);
    let mut block_len = 0usize;
    for candidate in scored {
        if include(&candidate) {
            let candidate = WorstScoredCandidate { candidate, table };
            if retained.len() < limit {
                retained.push(candidate);
            } else if retained
                .peek()
                .is_some_and(|worst| candidate.cmp(worst).is_lt())
            {
                retained.pop();
                retained.push(candidate);
            }
        }
        block_len += 1;
        scan_metrics.selection_entries_inspected =
            scan_metrics.selection_entries_inspected.saturating_add(1);
        if block_len == super::COMPLETION_CANCELLATION_CHECK_INTERVAL {
            block_len = 0;
            if scan_metrics.check(cancellation) {
                return None;
            }
        }
    }
    if block_len != 0 && scan_metrics.check(cancellation) {
        return None;
    }
    let mut retained: Vec<_> = retained
        .into_iter()
        .map(|candidate| candidate.candidate)
        .collect();
    sort_scored(&mut retained, table);
    Some(retained)
}

struct WorstScoredCandidate<'a> {
    candidate: ScoredCandidate,
    table: &'a NameTable,
}

impl PartialEq for WorstScoredCandidate<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for WorstScoredCandidate<'_> {}

impl PartialOrd for WorstScoredCandidate<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for WorstScoredCandidate<'_> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        scored_order(&self.candidate, &other.candidate, self.table)
    }
}

pub(super) fn top_scored(
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
