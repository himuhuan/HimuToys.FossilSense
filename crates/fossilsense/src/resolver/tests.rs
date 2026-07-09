use super::*;
use crate::model::CandidateRange;
use crate::reachability::OpenReason;
fn reach(files: &[&str], open: bool) -> ReachScope {
    ReachScope {
        files: files.iter().map(|s| s.to_string()).collect(),
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
