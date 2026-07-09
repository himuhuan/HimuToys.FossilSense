use std::collections::HashSet;

use crate::model::ScopeTier;
use crate::project_context::ProjectContextKey;
use crate::resolver::{self, ResolveContext};

use super::{
    score_match, sort_scored, CompletionScope, NameTable, RankedNameHit, ScoredCandidate,
    INDEXED_RECALL_FULL_SCAN_MAX,
};
use crate::query::{SHORT_PREFIX_MIN_LEN, SHORT_PREFIX_MIN_SCORE};

const COMPLETION_RECALL_CANDIDATE_CAP_MULTIPLIER: usize = 16;
const COMPLETION_RECALL_CANDIDATE_MIN_CAP: usize = 512;
const COMPLETION_RECALL_SCOPED_EXTRA_CAP_MULTIPLIER: usize = 32;

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
            total_indexed: limit.saturating_mul(4),
            reachable: limit,
            external: limit / 2,
            unknown: limit / 2,
            global: limit,
            same_project: limit / 2,
        }
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

impl NameTable {
    pub fn search_completion_recall_pooled(
        &self,
        query: &str,
        quotas: CompletionRecallQuotas,
        scope: Option<&CompletionScope>,
        active_project_context: Option<&ProjectContextKey>,
        prior_pool: Option<&[usize]>,
    ) -> (Vec<RankedNameHit>, Vec<usize>, CompletionRecallMetrics) {
        let total_limit = quotas.total_indexed;
        let (mut scored, pool) =
            self.scored_pool_for_query(query, quotas, scope, active_project_context, prior_pool);
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
        take_same_project(
            &scored,
            &self.entries,
            active_project_context,
            quotas.same_project,
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
        let metrics = recall_metrics(&hits, pool.len(), active_project_context);
        (hits, pool, metrics)
    }

    fn scored_pool_for_query(
        &self,
        query: &str,
        quotas: CompletionRecallQuotas,
        scope: Option<&CompletionScope>,
        active_project_context: Option<&ProjectContextKey>,
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
            Some(indices) if !indices.is_empty() => {
                for &i in indices {
                    self.consider(i, &needle, min_score, ctx_ref, &mut scored, &mut pool);
                }
            }
            _ if self.entries.len() <= INDEXED_RECALL_FULL_SCAN_MAX => {
                for i in 0..self.entries.len() {
                    self.consider(i, &needle, min_score, ctx_ref, &mut scored, &mut pool);
                }
            }
            _ => {
                let cap = completion_recall_candidate_cap(quotas);
                let mut indices = self.indexed_candidate_indices(&needle, Some(cap));
                let mut seen: HashSet<usize> = indices.iter().copied().collect();
                add_scope_seed_indices(
                    self,
                    &needle,
                    scope,
                    quotas
                        .reachable
                        .saturating_mul(COMPLETION_RECALL_SCOPED_EXTRA_CAP_MULTIPLIER)
                        .max(COMPLETION_RECALL_CANDIDATE_MIN_CAP),
                    &mut seen,
                    &mut indices,
                );
                add_project_seed_indices(
                    self,
                    &needle,
                    active_project_context,
                    quotas
                        .same_project
                        .saturating_mul(COMPLETION_RECALL_SCOPED_EXTRA_CAP_MULTIPLIER)
                        .max(COMPLETION_RECALL_CANDIDATE_MIN_CAP),
                    &mut seen,
                    &mut indices,
                );
                for i in indices {
                    self.consider(i, &needle, min_score, ctx_ref, &mut scored, &mut pool);
                }
                // This large-table cold path is intentionally bounded by recall
                // indexes, so `pool` is not an exhaustive narrowing base for
                // future keystrokes. Return no reusable pool; the next prefix
                // should cold-recall through the indexes again.
                pool.clear();
            }
        }
        (scored, pool)
    }
}

fn completion_recall_candidate_cap(quotas: CompletionRecallQuotas) -> usize {
    quotas
        .total_indexed
        .saturating_mul(COMPLETION_RECALL_CANDIDATE_CAP_MULTIPLIER)
        .max(COMPLETION_RECALL_CANDIDATE_MIN_CAP)
}

fn add_scope_seed_indices(
    table: &NameTable,
    needle: &str,
    scope: Option<&CompletionScope>,
    cap: usize,
    seen: &mut HashSet<usize>,
    out: &mut Vec<usize>,
) {
    let Some(scope) = scope else {
        return;
    };
    let mut taken = 0usize;
    if let Some(current_path) = scope.current_path.as_deref() {
        add_matching_indices(table, needle, current_path, cap, seen, out, &mut taken);
    }
    for path in &scope.reach.files {
        if taken >= cap {
            break;
        }
        add_matching_indices(table, needle, path, cap, seen, out, &mut taken);
    }
}

fn add_project_seed_indices(
    table: &NameTable,
    needle: &str,
    active_project_context: Option<&ProjectContextKey>,
    cap: usize,
    seen: &mut HashSet<usize>,
    out: &mut Vec<usize>,
) {
    let Some(active_project_context) = active_project_context else {
        return;
    };
    let Some(indices) = table.project_indices(active_project_context) else {
        return;
    };
    let mut taken = 0usize;
    for &index in indices {
        if taken >= cap {
            break;
        }
        if seen.contains(&index) || score_match(needle, &table.entries[index]).is_none() {
            continue;
        }
        seen.insert(index);
        out.push(index);
        taken += 1;
    }
}

fn add_matching_indices(
    table: &NameTable,
    needle: &str,
    path: &str,
    cap: usize,
    seen: &mut HashSet<usize>,
    out: &mut Vec<usize>,
    taken: &mut usize,
) {
    let Some(indices) = table.path_indices(path) else {
        return;
    };
    for &index in indices {
        if *taken >= cap {
            break;
        }
        if seen.contains(&index) || score_match(needle, &table.entries[index]).is_none() {
            continue;
        }
        seen.insert(index);
        out.push(index);
        *taken += 1;
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
    scored: &[ScoredCandidate],
    entries: &[super::NameEntry],
    active_project_context: Option<&ProjectContextKey>,
    quota: usize,
    selected_indices: &mut HashSet<usize>,
    selected: &mut Vec<ScoredCandidate>,
) {
    let Some(active_project_context) = active_project_context else {
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
        if entries[candidate.index].project_context.as_ref() != Some(active_project_context) {
            continue;
        }
        if selected_indices.insert(candidate.index) {
            selected.push(*candidate);
            taken += 1;
        }
    }
}

fn recall_metrics(
    hits: &[RankedNameHit],
    pool_total: usize,
    active_project_context: Option<&ProjectContextKey>,
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
        if active_project_context.is_some()
            && hit.project_context.as_ref() == active_project_context
        {
            metrics.same_project += 1;
        }
    }
    metrics
}
