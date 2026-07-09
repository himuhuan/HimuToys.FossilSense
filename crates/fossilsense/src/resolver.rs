//! Shared scope/ranking resolver: the single protocol-agnostic primitive that
//! assigns a [`ScopeTier`] to a name candidate and ranks candidates. Goto,
//! workspace-symbol, completion, and coloring kind resolution all derive a
//! candidate's scope tier through this module; none re-implements its own
//! "current vs reachable vs external vs global" scope test.
//!
//! Free of `tower-lsp` request types so tier resolution and ordering unit-test
//! in isolation. Depends only on the candidate `model` and reachability inputs.

use std::cmp::Ordering;

use crate::model::{DefinitionCandidate, ResolutionConfidence, ResolutionReason, ScopeTier};
use crate::reachability::{OpenReason, ReachScope};

/// Stride used to pack [`ScopeTier::rank`] into a single sortable integer. Tier
/// strictly dominates `base_match + locality`: this constant is *defined* to be
/// larger than the entire [`MAX_BASE_MATCH`] + [`MAX_LOCALITY`] range, so tier
/// can never be overtaken by match quality or locality — the packed integer is
/// a faithful encoding of the lexicographic order, not an independent knob.
///
/// The invariant `TIER_STRIDE > MAX_BASE_MATCH + MAX_LOCALITY` is asserted in
/// `tier_packing_invariant_holds`; `tier_packing_does_not_overflow` asserts
/// that `tier.rank() * TIER_STRIDE + base_match + locality` cannot overflow
/// `i32` for any valid tier/base_match/locality combination.
pub const TIER_STRIDE: i32 = 2048;

/// Upper bound on the `base_match` value any caller supplies. Callers MUST keep
/// their `base_match` at or below this; the packing invariant relies on it.
/// Covers both completion's `score_match` (max 1000) and goto's
/// definition-preference quality (max 1100 = 1000 definition + 100 function in
/// `.c`) with headroom for the member-completion base (max 900) and the
/// current-file local-word score (max 750).
#[allow(dead_code)]
pub const MAX_BASE_MATCH: i32 = 1500;

/// Upper bound on the locality tiebreak. Locality is `LOCALITY_PER_SEGMENT *
/// shared_path_prefix_segments`, capped at [`MAX_LOCALITY_SEGMENTS`] so it
/// stays well under one `base_match` step (it only breaks ties within an equal
/// `(tier, base_match)`).
pub const MAX_LOCALITY: i32 = 100;

/// Per-shared-path-segment locality bonus.
pub const LOCALITY_PER_SEGMENT: i32 = 10;

/// Maximum number of shared path prefix segments that contribute to locality.
/// Beyond this, deeper nesting does not improve the locality tiebreak.
pub const MAX_LOCALITY_SEGMENTS: i32 = MAX_LOCALITY / LOCALITY_PER_SEGMENT;

/// The inputs tier resolution needs: the current file's repository-relative
/// path plus the optional [`ReachScope`] (reachable set + `open`). `reach =
/// None` means no reach graph is available (scoping disabled, no index yet, or
/// the path cannot be resolved) — every workspace candidate then resolves to
/// [`ScopeTier::Global`] and first-layer external to [`ScopeTier::External`].
#[derive(Debug, Clone, Copy)]
pub struct ResolveContext<'a> {
    /// Repository-relative path (with `/` separators) of the file the query
    /// originated from, or `None` when no current file is known.
    pub current_path: Option<&'a str>,
    /// Bounded `#include`-reachable set for the current file, or `None` when
    /// no reach graph exists.
    pub reach: Option<&'a ReachScope>,
}

impl<'a> ResolveContext<'a> {
    /// Build a context with no reach graph: every workspace candidate
    /// resolves to `Global`, first-layer external to `External`. Used by
    /// callers that have a current file but no reachability analysis.
    #[allow(dead_code)]
    pub fn unscoped(current_path: Option<&'a str>) -> Self {
        Self {
            current_path,
            reach: None,
        }
    }
}

/// Resolve the [`ScopeTier`] for a candidate from its file and the current
/// reach context. The single source of "which scope tier" — goto, completion,
/// workspace-symbol, and coloring all call this; none re-implements its own
/// scope-membership test.
///
/// Decision order (strongest evidence first):
/// 1. **`External`** — an external header that is first-layer directly
///    `#include`d (`external && directly_included`). Direct include evidence
///    outranks every other signal except being in the current file: a direct
///    include is reachability proof even without a graph.
/// 2. **`Current`** — the candidate's path equals `ctx.current_path`.
/// 3. **`Reachable`** — the candidate's path is in `ctx.reach.files`
///    (proven reachable by traversal; the set may still be open, but this
///    file's reachability is proven).
/// 4. **`Unknown`** — the scope is open and the candidate is not proven in
///    the reachable set (cannot prove unreachable either; must not be buried
///    below a `Global`).
/// 5. **`Global`** — closed scope and the candidate is not in the reachable
///    set (proven unreachable), or no reach context at all.
///
/// With no context (`ctx = None`) the result is `External` for a first-layer
/// external candidate and `Global` otherwise — reproducing the unscoped
/// ranking except for the intended `External > Global` reversal.
pub fn scope_tier(
    path: &str,
    external: bool,
    directly_included: bool,
    ctx: Option<&ResolveContext<'_>>,
) -> ScopeTier {
    // First-layer external header: direct include is reachability evidence
    // regardless of whether a reach graph exists.
    if external && directly_included {
        return ScopeTier::External;
    }
    let Some(ctx) = ctx else {
        // No context at all (no current file, no reach graph).
        return ScopeTier::Global;
    };
    if ctx.current_path == Some(path) {
        return ScopeTier::Current;
    }
    let Some(reach) = ctx.reach else {
        // Context present but no reach graph: workspace candidate with no
        // scope evidence.
        return ScopeTier::Global;
    };
    if reach.files.contains(path) {
        // Proven reachable by traversal (the set may still be open, but this
        // file's reachability is proven by its membership in the set).
        return ScopeTier::Reachable;
    }
    if reach.open {
        // Open scope: cannot prove this candidate unreachable.
        return ScopeTier::Unknown;
    }
    // Closed scope and not in the reachable set: proven unreachable.
    ScopeTier::Global
}

/// Locality tiebreak: shared path prefix segments between `path` and
/// `current_path`, scaled by [`LOCALITY_PER_SEGMENT`] and capped at
/// [`MAX_LOCALITY`]. `None` for `current_path` yields 0. Locality only breaks
/// ties within an equal `(tier, base_match)` — it is bounded well under one
/// `base_match` step and never changes which tier a candidate occupies.
pub fn locality(path: &str, current_path: Option<&str>) -> i32 {
    let Some(current) = current_path else {
        return 0;
    };
    let segments = common_path_prefix_segments(path, current);
    let capped = segments.min(MAX_LOCALITY_SEGMENTS.max(0) as usize) as i32;
    LOCALITY_PER_SEGMENT * capped
}

/// Pack `(tier, base_match, locality)` into a single sortable integer.
/// Strict-tier lexicographic order: tier dominates `base_match`, which
/// dominates locality. The packing is the opposite of an additive score blend:
/// [`TIER_STRIDE`] is defined larger than the entire `base_match + locality`
/// range, so tier can never be overtaken. Callers MUST keep `base_match ≤
/// MAX_BASE_MATCH` and `locality ≤ MAX_LOCALITY`; the packing invariant test
/// pins both.
pub fn pack_score(tier: ScopeTier, base_match: i32, locality: i32) -> i32 {
    tier.rank() * TIER_STRIDE + base_match + locality
}

/// Compare two [`DefinitionCandidate`]s by strict-tier lexicographic order:
/// `(tier rank desc, base_match desc, locality desc, path asc, line asc)`.
/// Locality is computed from each candidate's path and the supplied
/// `current_path`. Use this (or precomputed scores via [`pack_score`]) for
/// every name→candidate read path so ranking stays unified.
#[allow(dead_code)]
pub fn compare_candidates(
    a: &DefinitionCandidate,
    b: &DefinitionCandidate,
    current_path: Option<&str>,
) -> Ordering {
    let locality_a = locality(&a.path, current_path);
    let locality_b = locality(&b.path, current_path);
    let score_a = pack_score(a.tier, a.base_match, locality_a);
    let score_b = pack_score(b.tier, b.base_match, locality_b);
    score_b
        .cmp(&score_a)
        .then_with(|| a.path.cmp(&b.path))
        .then(a.range.start_line.cmp(&b.range.start_line))
}

/// Sort a slice of [`DefinitionCandidate`]s in place via
/// [`compare_candidates`] with `current_path` for locality. The canonical
/// ranking entry point for goto-definition.
#[allow(dead_code)]
pub fn sort_candidates(candidates: &mut [DefinitionCandidate], current_path: Option<&str>) {
    candidates.sort_by(|a, b| compare_candidates(a, b, current_path));
}

/// Derive `(ResolutionConfidence, ResolutionReason)` from a candidate's
/// [`ScopeTier`], whether the queried name matched exactly, and the current
/// reach scope's open reason. The tier is the single source, so confidence and
/// reason cannot drift from the tier that ranked the candidate.
///
/// Mapping:
/// - `Current` + exact  → `(Exact, CurrentFile)`
/// - `Current` + !exact → `(Reachable, CurrentFile)`
/// - `Reachable`        → `(Reachable, ReachableInclude)`
/// - `External`         → `(Heuristic, ExternalFirstLayer)`
/// - `Unknown` + open reason [`OpenReason::AmbiguousInclude`] → `(Ambiguous, GlobalFallback)`
/// - `Unknown` (other open reasons) / `Global` → `(Fallback, GlobalFallback)`
///
/// `exact_name` only affects `Current`; `open_reason` only affects `Unknown`;
/// every other tier ignores both. `Exact` confidence is never emitted for a
/// non-current candidate, so it can never label a `GlobalFallback`. The
/// per-candidate `Ambiguous` confidence (R6 wiring of the variant the candidate
/// model reserved) is produced only when an `Unknown`-tier candidate sits under
/// an open scope whose first cause is `AmbiguousInclude` — a scope-layer
/// ambiguity signal, not a per-candidate binding claim. The reason stays
/// `GlobalFallback` because the candidate is still scope-unproven; only the
/// confidence distinguishes "ambiguous include" from a plain fallback.
pub fn confidence_reason_for(
    tier: ScopeTier,
    exact_name: bool,
    open_reason: Option<OpenReason>,
) -> (ResolutionConfidence, ResolutionReason) {
    match tier {
        ScopeTier::Current => {
            let confidence = if exact_name {
                ResolutionConfidence::Exact
            } else {
                ResolutionConfidence::Reachable
            };
            (confidence, ResolutionReason::CurrentFile)
        }
        ScopeTier::Reachable => (
            ResolutionConfidence::Reachable,
            ResolutionReason::ReachableInclude,
        ),
        ScopeTier::External => (
            ResolutionConfidence::Heuristic,
            ResolutionReason::ExternalFirstLayer,
        ),
        ScopeTier::Unknown => {
            // Open scope. When the scope opened *because of* an ambiguous
            // include, surface the reserved per-candidate `Ambiguous`
            // confidence; any other open cause stays an undifferentiated
            // `Fallback`.
            let confidence = if open_reason == Some(OpenReason::AmbiguousInclude) {
                ResolutionConfidence::Ambiguous
            } else {
                ResolutionConfidence::Fallback
            };
            (confidence, ResolutionReason::GlobalFallback)
        }
        ScopeTier::Global => (
            ResolutionConfidence::Fallback,
            ResolutionReason::GlobalFallback,
        ),
    }
}

/// Deduplicate same-name candidates, keeping the one with the higher
/// `(`[`ScopeTier`]`,`[`ResolutionConfidence`]`)` — not the first one
/// encountered. A lower-tier candidate SHALL NOT displace a higher-tier
/// candidate of the same name merely because it was iterated first. The output
/// order preserves the input order of the surviving candidates (callers sort
/// after dedup via [`sort_candidates`] or the packed score).
#[allow(dead_code)]
pub fn dedup_keep_higher(candidates: Vec<DefinitionCandidate>) -> Vec<DefinitionCandidate> {
    // Keep the surviving candidate's original position so a pre-sorted input
    // stays ordered after dedup. Track for each name the current best index
    // and replace it when a strictly higher (tier, confidence) candidate is
    // seen.
    let mut best_by_name: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut survivors: Vec<Option<DefinitionCandidate>> =
        candidates.into_iter().map(Some).collect();
    for i in 0..survivors.len() {
        let (name, key) = {
            let Some(cand) = survivors[i].as_ref() else {
                continue;
            };
            (cand.name.clone(), (cand.tier, cand.confidence))
        };
        match best_by_name.get(&name) {
            None => {
                best_by_name.insert(name, i);
            }
            Some(&prev_i) => {
                let prev_key = {
                    let prev = survivors[prev_i].as_ref().expect("survivor present");
                    (prev.tier, prev.confidence)
                };
                if key > prev_key {
                    // Displace the previous holder: drop it from the output and
                    // record the new winner's index.
                    survivors[prev_i] = None;
                    best_by_name.insert(name, i);
                } else {
                    // The existing holder wins; drop this candidate.
                    survivors[i] = None;
                }
            }
        }
    }
    survivors.into_iter().flatten().collect()
}

/// Number of leading path segments shared by `a` and `b` (split on `/`).
pub(crate) fn common_path_prefix_segments(a: &str, b: &str) -> usize {
    a.split('/')
        .zip(b.split('/'))
        .take_while(|(x, y)| x == y)
        .count()
}

#[cfg(test)]
mod tests;
