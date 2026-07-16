//! Shared scope/ranking resolver: the single protocol-agnostic primitive that
//! assigns a [`ScopeTier`] to a name candidate and ranks candidates. Goto,
//! workspace-symbol, completion, and coloring kind resolution all derive a
//! candidate's scope tier through this module; none re-implements its own
//! "current vs reachable vs external vs global" scope test.
//!
//! Free of `tower-lsp` request types so tier resolution and ordering unit-test
//! in isolation. Depends only on the candidate `model` and reachability inputs.

use std::cmp::Ordering;
use std::collections::HashSet;

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
    /// Origin-specific direct `ExternalExact` targets. `Some` overrides the
    /// legacy workspace-wide `directly_included` bit; `None` keeps that bit for
    /// callers that have already projected request-local path evidence.
    pub direct_external_files: Option<&'a HashSet<String>>,
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
            direct_external_files: None,
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
/// 4. **`Unknown`** — the candidate is reachable only through a suffix-match
///    edge, or the scope is open and the candidate is not proven in the exact
///    reachable set (cannot prove unreachable either; must not be buried below
///    a `Global`).
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
    let directly_included = ctx
        .and_then(|context| context.direct_external_files)
        .map_or(directly_included, |paths| paths.contains(path));
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
    if reach.heuristic_files.contains(path) {
        // A suffix match is useful bounded recall evidence, but it is not
        // compiler-level proof and must never be laundered into Reachable.
        return ScopeTier::Unknown;
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
mod tests {
    use super::*;
    use crate::model::CandidateRange;
    use crate::reachability::OpenReason;
    fn reach(files: &[&str], open: bool) -> ReachScope {
        ReachScope {
            files: files.iter().map(|s| s.to_string()).collect(),
            heuristic_files: Default::default(),
            open,
            reason: if open {
                Some(OpenReason::UnresolvedInclude)
            } else {
                None
            },
        }
    }

    fn ctx<'a>(current: Option<&'a str>, reach: Option<&'a ReachScope>) -> ResolveContext<'a> {
        ResolveContext {
            current_path: current,
            reach,
            direct_external_files: None,
        }
    }

    // --- 2.3: scope_tier from representative inputs ------------------------

    #[test]
    fn current_file_candidate_is_current() {
        let r = reach(&["src/main.c"], false);
        let c = ctx(Some("src/main.c"), Some(&r));
        assert_eq!(
            scope_tier("src/main.c", false, false, Some(&c)),
            ScopeTier::Current
        );
    }

    #[test]
    fn reachable_workspace_candidate_is_reachable() {
        let r = reach(&["src/main.c", "inc/b.h"], false);
        let c = ctx(Some("src/main.c"), Some(&r));
        assert_eq!(
            scope_tier("inc/b.h", false, false, Some(&c)),
            ScopeTier::Reachable
        );
    }

    #[test]
    fn suffix_reachable_workspace_candidate_is_unknown() {
        let mut r = reach(&["src/main.c"], false);
        r.heuristic_files.insert("inc/b.h".to_string());
        let c = ctx(Some("src/main.c"), Some(&r));
        assert_eq!(
            scope_tier("inc/b.h", false, false, Some(&c)),
            ScopeTier::Unknown
        );
    }

    #[test]
    fn unreachable_closed_scope_workspace_is_global() {
        let r = reach(&["src/main.c"], false);
        let c = ctx(Some("src/main.c"), Some(&r));
        assert_eq!(
            scope_tier("other/c.h", false, false, Some(&c)),
            ScopeTier::Global
        );
    }

    #[test]
    fn unreachable_open_scope_workspace_is_unknown() {
        let r = reach(&["src/main.c"], true);
        let c = ctx(Some("src/main.c"), Some(&r));
        assert_eq!(
            scope_tier("other/c.h", false, false, Some(&c)),
            ScopeTier::Unknown
        );
    }

    #[test]
    fn first_layer_external_is_external_even_without_graph() {
        // No reach graph: first-layer external is still External (direct
        // include is reachability evidence).
        let c = ctx(Some("src/main.c"), None);
        assert_eq!(
            scope_tier("C:/mingw/include/stddef.h", true, true, Some(&c)),
            ScopeTier::External
        );
    }

    #[test]
    fn non_first_layer_external_without_graph_is_global() {
        let c = ctx(Some("src/main.c"), None);
        // External but not directly included: no reachability evidence, falls
        // to Global (it is an external file, but no include edge connects it).
        assert_eq!(
            scope_tier("C:/mingw/include/deep.h", true, false, Some(&c)),
            ScopeTier::Global
        );
    }

    #[test]
    fn first_layer_external_outranks_current_file_check_is_independent() {
        // First-layer external is checked before current-file because the
        // external flag excludes workspace. Verify a workspace candidate that
        // happens to set directly_included does not accidentally become
        // External: directly_included is meaningful only for external files.
        let r = reach(&["src/main.c"], false);
        let c = ctx(Some("src/main.c"), Some(&r));
        assert_eq!(
            scope_tier("src/main.c", false, true, Some(&c)),
            ScopeTier::Current,
            "workspace directly_included is irrelevant; current file wins"
        );
    }

    #[test]
    fn no_context_yields_external_or_global() {
        assert_eq!(
            scope_tier("C:/mingw/include/stddef.h", true, true, None),
            ScopeTier::External
        );
        assert_eq!(
            scope_tier("src/foo.c", false, false, None),
            ScopeTier::Global
        );
        assert_eq!(
            scope_tier("C:/mingw/include/deep.h", true, false, None),
            ScopeTier::Global
        );
    }

    #[test]
    fn reachable_in_open_set_still_reachable() {
        // Open scope: a file in the set is still Reachable (its reachability
        // is proven by traversal); open only softens the not-in-set case.
        let r = reach(&["src/main.c", "inc/b.h"], true);
        let c = ctx(Some("src/main.c"), Some(&r));
        assert_eq!(
            scope_tier("inc/b.h", false, false, Some(&c)),
            ScopeTier::Reachable
        );
    }

    // --- 2.4: packing invariant + overflow --------------------------------

    #[test]
    fn tier_packing_invariant_holds() {
        // TIER_STRIDE must strictly exceed the entire base_match + locality
        // range, so tier can never be overtaken by match quality + locality.
        const { assert!(TIER_STRIDE > MAX_BASE_MATCH + MAX_LOCALITY) };
        // The locality cap is enforced by the constants, not by callers
        // counting segments.
        assert_eq!(MAX_LOCALITY, LOCALITY_PER_SEGMENT * MAX_LOCALITY_SEGMENTS);
    }

    #[test]
    fn tier_packing_does_not_overflow() {
        // Highest tier + max base_match + max locality must not overflow i32.
        let max_packed = ScopeTier::Current.rank() * TIER_STRIDE + MAX_BASE_MATCH + MAX_LOCALITY;
        assert!(max_packed < i32::MAX);
        // Lowest packed value is 0 (Global + 0 + 0).
        assert_eq!(pack_score(ScopeTier::Global, 0, 0), 0);
        // A higher tier strictly dominates any lower tier regardless of
        // base_match/locality.
        let high = pack_score(ScopeTier::Reachable, 0, 0);
        let low = pack_score(ScopeTier::Global, MAX_BASE_MATCH, MAX_LOCALITY);
        assert!(high > low, "Reachable tier (score {high}) must outrank Global tier even at max base_match+locality (score {low})");
        // External > Unknown > Global edges, pinned.
        assert!(
            pack_score(ScopeTier::External, 0, 0)
                > pack_score(ScopeTier::Unknown, MAX_BASE_MATCH, MAX_LOCALITY)
        );
        assert!(
            pack_score(ScopeTier::Unknown, 0, 0)
                > pack_score(ScopeTier::Global, MAX_BASE_MATCH, MAX_LOCALITY)
        );
    }

    #[test]
    fn locality_is_capped_at_max() {
        // A path that shares many segments with the current file does not let
        // locality exceed MAX_LOCALITY.
        let deep_a = "root/a/b/c/d/e/f/g/h/i/j/file.c";
        let deep_b = "root/a/b/c/d/e/f/g/h/i/j/other.c";
        assert_eq!(locality(deep_a, Some(deep_b)), MAX_LOCALITY);
        // No current path → 0.
        assert_eq!(locality(deep_a, None), 0);
        // No shared prefix → 0.
        assert_eq!(locality("alpha/x.c", Some("beta/y.c")), 0);
        // Shared prefix of 2 segments → 20.
        assert_eq!(locality("src/sub/x.c", Some("src/sub/y.c")), 20);
    }

    // --- 2.5: confidence_reason_for projection per tier -------------------

    #[test]
    fn confidence_reason_for_current_exact_and_non_exact() {
        assert_eq!(
            confidence_reason_for(ScopeTier::Current, true, None),
            (ResolutionConfidence::Exact, ResolutionReason::CurrentFile)
        );
        assert_eq!(
            confidence_reason_for(ScopeTier::Current, false, None),
            (
                ResolutionConfidence::Reachable,
                ResolutionReason::CurrentFile
            )
        );
        // Current ignores the open reason entirely.
        assert_eq!(
            confidence_reason_for(ScopeTier::Current, true, Some(OpenReason::AmbiguousInclude)),
            (ResolutionConfidence::Exact, ResolutionReason::CurrentFile)
        );
    }

    #[test]
    fn confidence_reason_for_reachable_external_unknown_global() {
        assert_eq!(
            confidence_reason_for(ScopeTier::Reachable, true, None),
            (
                ResolutionConfidence::Reachable,
                ResolutionReason::ReachableInclude
            )
        );
        assert_eq!(
            confidence_reason_for(ScopeTier::External, true, None),
            (
                ResolutionConfidence::Heuristic,
                ResolutionReason::ExternalFirstLayer
            )
        );
        assert_eq!(
            confidence_reason_for(ScopeTier::Unknown, true, None),
            (
                ResolutionConfidence::Fallback,
                ResolutionReason::GlobalFallback
            )
        );
        assert_eq!(
            confidence_reason_for(ScopeTier::Global, true, None),
            (
                ResolutionConfidence::Fallback,
                ResolutionReason::GlobalFallback
            )
        );
    }

    // --- R6: per-candidate Ambiguous from open-scope AmbiguousInclude ------

    #[test]
    fn unknown_tier_under_ambiguous_include_is_ambiguous() {
        // An `Unknown`-tier candidate whose open scope was opened by an
        // ambiguous include surfaces `Ambiguous` confidence; the reason stays
        // `GlobalFallback` (the candidate is still scope-unproven).
        assert_eq!(
            confidence_reason_for(ScopeTier::Unknown, true, Some(OpenReason::AmbiguousInclude)),
            (
                ResolutionConfidence::Ambiguous,
                ResolutionReason::GlobalFallback
            )
        );
    }

    #[test]
    fn unknown_tier_under_other_open_reasons_stays_fallback() {
        // Any open cause other than AmbiguousInclude keeps a plain Fallback.
        for reason in [
            OpenReason::UnresolvedInclude,
            OpenReason::DepthLimit,
            OpenReason::NodeLimit,
        ] {
            assert_eq!(
                confidence_reason_for(ScopeTier::Unknown, true, Some(reason)),
                (
                    ResolutionConfidence::Fallback,
                    ResolutionReason::GlobalFallback
                ),
                "open reason {reason:?} must not produce Ambiguous"
            );
        }
        // No open reason at all is also Fallback.
        assert_eq!(
            confidence_reason_for(ScopeTier::Unknown, true, None),
            (
                ResolutionConfidence::Fallback,
                ResolutionReason::GlobalFallback
            )
        );
    }

    #[test]
    fn ambiguous_include_only_affects_unknown_tier() {
        // Passing AmbiguousInclude to any non-Unknown tier yields the same
        // result as passing None — parity for every other tier.
        for tier in [
            ScopeTier::Current,
            ScopeTier::Reachable,
            ScopeTier::External,
            ScopeTier::Global,
        ] {
            assert_eq!(
                confidence_reason_for(tier, true, Some(OpenReason::AmbiguousInclude)),
                confidence_reason_for(tier, true, None),
                "{tier:?} must ignore the open reason"
            );
        }
    }

    #[test]
    fn exact_never_labels_a_non_current_candidate() {
        for tier in [
            ScopeTier::Reachable,
            ScopeTier::External,
            ScopeTier::Unknown,
            ScopeTier::Global,
        ] {
            let (confidence, _) = confidence_reason_for(tier, true, None);
            assert_ne!(
                confidence,
                ResolutionConfidence::Exact,
                "Exact must not be emitted for {:?} tier",
                tier
            );
        }
    }

    // --- 2.6: dedup_keep_higher ------------------------------------------

    fn cand(
        name: &str,
        tier: ScopeTier,
        confidence: ResolutionConfidence,
        path: &str,
    ) -> DefinitionCandidate {
        DefinitionCandidate {
            name: name.to_string(),
            kind: "function".to_string(),
            role: "definition".to_string(),
            path: path.to_string(),
            range: CandidateRange {
                start_line: 0,
                start_col: 0,
                end_line: 0,
                end_col: 0,
            },
            source: "workspace".to_string(),
            tier,
            base_match: 0,
            confidence,
            reason: ResolutionReason::GlobalFallback,
        }
    }

    #[test]
    fn dedup_keeps_higher_tier_regardless_of_iteration_order() {
        // Higher-tier first then lower-tier: higher survives.
        let higher_first = vec![
            cand(
                "foo",
                ScopeTier::Reachable,
                ResolutionConfidence::Reachable,
                "inc/b.h",
            ),
            cand(
                "foo",
                ScopeTier::Global,
                ResolutionConfidence::Fallback,
                "other/c.h",
            ),
        ];
        let out = dedup_keep_higher(higher_first);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].tier, ScopeTier::Reachable);

        // Lower-tier first then higher-tier: higher still survives (this is
        // the regression case for first-seen-wins dedup).
        let lower_first = vec![
            cand(
                "foo",
                ScopeTier::Global,
                ResolutionConfidence::Fallback,
                "other/c.h",
            ),
            cand(
                "foo",
                ScopeTier::Reachable,
                ResolutionConfidence::Reachable,
                "inc/b.h",
            ),
        ];
        let out = dedup_keep_higher(lower_first);
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].tier,
            ScopeTier::Reachable,
            "higher tier wins regardless of order"
        );
    }

    #[test]
    fn dedup_keeps_higher_confidence_within_same_tier() {
        // Same tier, different confidence: higher confidence wins. (In R2 the
        // confidence is derived from tier + exact_name, so within a tier the
        // confidence distinction is the Current exact/non-exact one — but the
        // dedup rule is pinned on the (tier, confidence) key regardless.)
        let a = cand(
            "foo",
            ScopeTier::Current,
            ResolutionConfidence::Exact,
            "src/main.c",
        );
        let b = cand(
            "foo",
            ScopeTier::Current,
            ResolutionConfidence::Reachable,
            "src/main.c",
        );
        let out = dedup_keep_higher(vec![b, a]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].confidence, ResolutionConfidence::Exact);
    }

    #[test]
    fn dedup_preserves_distinct_names_and_input_order() {
        // Distinct names are all kept; surviving candidates keep their input
        // order so a pre-sorted input stays sorted after dedup.
        let input = vec![
            cand(
                "alpha",
                ScopeTier::Current,
                ResolutionConfidence::Exact,
                "src/a.c",
            ),
            cand(
                "beta",
                ScopeTier::Reachable,
                ResolutionConfidence::Reachable,
                "inc/b.h",
            ),
            cand(
                "alpha",
                ScopeTier::Global,
                ResolutionConfidence::Fallback,
                "other/c.h",
            ),
            cand(
                "gamma",
                ScopeTier::External,
                ResolutionConfidence::Heuristic,
                "ext/d.h",
            ),
        ];
        let out = dedup_keep_higher(input);
        let names: Vec<&str> = out.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta", "gamma"]);
        assert_eq!(out[0].tier, ScopeTier::Current);
        assert_eq!(out[2].tier, ScopeTier::External);
    }

    // --- 2.7: comparator + sort_candidates -------------------------------

    #[test]
    fn compare_candidates_orders_by_tier_first() {
        // A Reachable declaration outranks a Global definition even though
        // the definition has higher base_match (definition = 1000 > 0).
        let reachable_decl = DefinitionCandidate {
            name: "foo".to_string(),
            kind: "function".to_string(),
            role: "declaration".to_string(),
            path: "inc/b.h".to_string(),
            range: CandidateRange {
                start_line: 0,
                start_col: 0,
                end_line: 0,
                end_col: 0,
            },
            source: "workspace".to_string(),
            tier: ScopeTier::Reachable,
            base_match: 0,
            confidence: ResolutionConfidence::Reachable,
            reason: ResolutionReason::ReachableInclude,
        };
        let global_def = DefinitionCandidate {
            name: "foo".to_string(),
            kind: "function".to_string(),
            role: "definition".to_string(),
            path: "other/c.h".to_string(),
            range: CandidateRange {
                start_line: 0,
                start_col: 0,
                end_line: 0,
                end_col: 0,
            },
            source: "workspace".to_string(),
            tier: ScopeTier::Global,
            base_match: 1000,
            confidence: ResolutionConfidence::Fallback,
            reason: ResolutionReason::GlobalFallback,
        };
        // Sort places reachable_decl first.
        let mut v = vec![global_def.clone(), reachable_decl.clone()];
        sort_candidates(&mut v, Some("src/main.c"));
        assert_eq!(v[0].tier, ScopeTier::Reachable, "tier dominates base_match");
        assert_eq!(v[1].tier, ScopeTier::Global);
        // Comparator agrees.
        assert_eq!(
            compare_candidates(&reachable_decl, &global_def, Some("src/main.c")),
            Ordering::Less
        );
    }

    #[test]
    fn compare_candidates_ties_break_by_base_match_then_locality_then_path_then_line() {
        // Same tier; higher base_match wins.
        let mut v = vec![
            cand_tier(ScopeTier::Reachable, 100, "inc/a.h", 1),
            cand_tier(ScopeTier::Reachable, 500, "inc/b.h", 1),
        ];
        sort_candidates(&mut v, Some("src/main.c"));
        assert_eq!(v[0].base_match, 500);
        // Same tier + base_match: locality (shared prefix) breaks the tie.
        let mut v = vec![
            cand_tier(ScopeTier::Reachable, 100, "src/sub/x.h", 1),
            cand_tier(ScopeTier::Reachable, 100, "other/y.h", 1),
        ];
        sort_candidates(&mut v, Some("src/sub/main.c"));
        assert_eq!(v[0].path, "src/sub/x.h", "closer path wins via locality");
        // Same tier + base_match + locality: path asc, then line asc.
        let mut v = vec![
            cand_tier(ScopeTier::Reachable, 100, "zzz/a.h", 5),
            cand_tier(ScopeTier::Reachable, 100, "aaa/a.h", 5),
        ];
        sort_candidates(&mut v, Some("src/main.c"));
        assert_eq!(v[0].path, "aaa/a.h", "path asc tie-break");
    }

    fn cand_tier(tier: ScopeTier, base_match: i32, path: &str, line: u32) -> DefinitionCandidate {
        DefinitionCandidate {
            name: "foo".to_string(),
            kind: "function".to_string(),
            role: "definition".to_string(),
            path: path.to_string(),
            range: CandidateRange {
                start_line: line,
                start_col: 0,
                end_line: line,
                end_col: 0,
            },
            source: "workspace".to_string(),
            tier,
            base_match,
            confidence: confidence_reason_for(tier, true, None).0,
            reason: confidence_reason_for(tier, true, None).1,
        }
    }
}
