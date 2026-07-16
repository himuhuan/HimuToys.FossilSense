use std::collections::{HashMap, HashSet};

use super::*;
use crate::call_model::{
    AnchorRole, CallForm, CallSiteFact, CallableKind, FactProvenance, LinkageDomain,
    SignatureFidelity, SignatureShape, SourcePosition, SourceRange,
};
use crate::model::{
    CandidateRange, DefinitionCandidate, ResolutionConfidence, ResolutionReason, ScopeTier,
};

fn source_range(start: usize) -> SourceRange {
    SourceRange {
        start: SourcePosition {
            line: 0,
            character: start as u32,
        },
        end: SourcePosition {
            line: 0,
            character: start as u32 + 3,
        },
        start_byte: start,
        end_byte: start + 3,
    }
}

fn resolved(
    path: &str,
    role: AnchorRole,
    tier: ScopeTier,
    min_arity: Option<u32>,
    max_arity: Option<u32>,
    variadic: bool,
) -> ResolvedCallableAnchor {
    let name = "pick";
    let canonical_signature = "int pick(int lhs, int rhs)";
    let range = source_range(path.len());
    ResolvedCallableAnchor::new(
        crate::call_model::CallableAnchor {
            path: path.to_string(),
            name: name.to_string(),
            qualified_name: name.to_string(),
            owner: None,
            owner_kind: None,
            kind: CallableKind::Function,
            role,
            linkage: LinkageDomain::External,
            signature: SignatureShape {
                normalized: canonical_signature.to_string(),
                min_arity,
                max_arity,
                variadic,
            },
            canonical_signature: canonical_signature.to_string(),
            presentation_signature: canonical_signature.to_string(),
            signature_fidelity: SignatureFidelity::AstExact,
            name_range: range,
            declaration_range: range,
            body_range: (role == AnchorRole::Definition).then_some(range),
            guard: None,
            provenance: FactProvenance::Ast,
            syntax_error_overlap: false,
            entity_key: format!("{name}:{canonical_signature}"),
            anchor_fingerprint: format!("{path}@{}", range.start_byte),
        },
        DefinitionCandidate {
            name: name.to_string(),
            kind: "function".to_string(),
            role: role.as_str().to_string(),
            path: path.to_string(),
            range: CandidateRange {
                start_line: range.start.line,
                start_col: range.start.character,
                end_line: range.end.line,
                end_col: range.end.character,
            },
            source: "workspace".to_string(),
            tier,
            base_match: 1_000,
            confidence: ResolutionConfidence::Fallback,
            reason: ResolutionReason::GlobalFallback,
        },
        CandidateOrigin::Base,
    )
}

fn complete_call(arity: Option<u32>) -> CallSiteContext {
    let range = source_range(2);
    let fact = CallSiteFact {
        path: "src/main.c".to_string(),
        caller_entity_key: "caller".to_string(),
        expression_range: range,
        callee_range: range,
        callee_name: Some("pick".to_string()),
        qualified_name: None,
        form: CallForm::DirectName,
        argument_count: arity,
        guard: None,
        provenance: FactProvenance::Ast,
        syntax_error_overlap: false,
        site_fingerprint: "site".to_string(),
    };
    CallSiteContext::from_complete_call(
        &fact,
        SourcePosition {
            line: 0,
            character: 3,
        },
    )
    .expect("callee context")
}

#[test]
fn complete_call_filters_incompatible_keeps_unknown_and_preserves_scope_order() {
    let mut anchors = vec![
        resolved(
            "current/unspecified.h",
            AnchorRole::Declaration,
            ScopeTier::Current,
            Some(0),
            None,
            false,
        ),
        resolved(
            "global/one.h",
            AnchorRole::Declaration,
            ScopeTier::Global,
            Some(1),
            Some(1),
            false,
        ),
        resolved(
            "global/two.h",
            AnchorRole::Declaration,
            ScopeTier::Global,
            Some(2),
            Some(2),
            false,
        ),
    ];

    let outcome = apply_arity_policy(&mut anchors, Some(&complete_call(Some(2))));

    assert_eq!(outcome, ArityFilterOutcome::Filtered);
    assert_eq!(anchors.len(), 2);
    assert_eq!(anchors[0].candidate.tier, ScopeTier::Current);
    assert_eq!(anchors[0].arity_compatibility, ArityCompatibility::Unknown);
    assert_eq!(
        anchors[1].arity_compatibility,
        ArityCompatibility::Compatible
    );
    assert!(anchors
        .iter()
        .all(|anchor| anchor.anchor.path != "global/one.h"));
}

#[test]
fn arity_supports_default_variadic_partial_and_all_mismatch_fallback() {
    let defaulted = resolved(
        "default.h",
        AnchorRole::Declaration,
        ScopeTier::Reachable,
        Some(1),
        Some(2),
        false,
    );
    let variadic = resolved(
        "var.h",
        AnchorRole::Declaration,
        ScopeTier::Reachable,
        Some(1),
        None,
        true,
    );
    assert_eq!(
        compatibility_for_signature(&defaulted.anchor.signature, Some(&complete_call(Some(2)))),
        ArityCompatibility::Compatible
    );
    assert_eq!(
        compatibility_for_signature(&variadic.anchor.signature, Some(&complete_call(Some(7)))),
        ArityCompatibility::Compatible
    );

    let partial = CallSiteContext::partial(
        "pick".to_string(),
        CallForm::DirectName,
        source_range(0),
        2,
        1,
        ContextReliability::Reliable,
    );
    let mut fixed_one = resolved(
        "one.h",
        AnchorRole::Declaration,
        ScopeTier::Reachable,
        Some(1),
        Some(1),
        false,
    );
    fixed_one.candidate.confidence = ResolutionConfidence::Reachable;
    assert_eq!(
        compatibility_for_signature(&fixed_one.anchor.signature, Some(&partial)),
        ArityCompatibility::Incompatible
    );

    let mut all_bad = vec![fixed_one];
    assert_eq!(
        apply_arity_policy(&mut all_bad, Some(&complete_call(Some(3)))),
        ArityFilterOutcome::MismatchFallback
    );
    assert_eq!(all_bad.len(), 1, "fallback restores navigation candidates");
    assert_eq!(
        all_bad[0].candidate.confidence,
        ResolutionConfidence::Fallback,
        "a retained arity mismatch must not present stronger match confidence"
    );
}

#[test]
fn arity_distinguishes_exact_zero_from_c_unspecified_and_skips_unreliable_contexts() {
    let exact_zero = SignatureShape {
        normalized: "int pick(void)".to_string(),
        min_arity: Some(0),
        max_arity: Some(0),
        variadic: false,
    };
    let c_unspecified = SignatureShape {
        normalized: "int pick()".to_string(),
        min_arity: Some(0),
        max_arity: None,
        variadic: false,
    };
    let one_argument = complete_call(Some(1));
    assert_eq!(
        compatibility_for_signature(&exact_zero, Some(&one_argument)),
        ArityCompatibility::Incompatible
    );
    assert_eq!(
        compatibility_for_signature(&c_unspecified, Some(&one_argument)),
        ArityCompatibility::Unknown
    );

    let mut unreliable = one_argument;
    unreliable.reliability = ContextReliability::SyntaxErrorOverlap;
    let mut anchors = vec![resolved(
        "zero.h",
        AnchorRole::Declaration,
        ScopeTier::Reachable,
        Some(0),
        Some(0),
        false,
    )];
    assert_eq!(
        apply_arity_policy(&mut anchors, Some(&unreliable)),
        ArityFilterOutcome::NotApplied
    );
    assert_eq!(anchors.len(), 1);
    assert_eq!(anchors[0].arity_compatibility, ArityCompatibility::Unknown);
}

#[test]
fn complete_call_projection_is_callee_bounded_and_degrades_unreliable_forms() {
    let range = source_range(4);
    let mut fact = CallSiteFact {
        path: "main.c".to_string(),
        caller_entity_key: "caller".to_string(),
        expression_range: range,
        callee_range: range,
        callee_name: Some("pick".to_string()),
        qualified_name: None,
        form: CallForm::DirectName,
        argument_count: Some(2),
        guard: None,
        provenance: FactProvenance::Ast,
        syntax_error_overlap: true,
        site_fingerprint: "site".to_string(),
    };
    assert!(CallSiteContext::from_complete_call(
        &fact,
        SourcePosition {
            line: 1,
            character: 0,
        }
    )
    .is_none());
    let context = CallSiteContext::from_complete_call(&fact, range.start).expect("context");
    assert_eq!(context.reliability, ContextReliability::SyntaxErrorOverlap);

    fact.form = CallForm::MemberDot;
    fact.syntax_error_overlap = false;
    assert_eq!(
        CallSiteContext::from_complete_call(&fact, range.start)
            .expect("unsupported context")
            .reliability,
        ContextReliability::UnsupportedCallForm
    );
}

#[test]
fn strict_one_to_one_counterpart_drives_consistent_presentations() {
    let source = resolved(
        "src/api.c",
        AnchorRole::Definition,
        ScopeTier::Current,
        Some(2),
        Some(2),
        false,
    );
    let header = resolved(
        "include/api.h",
        AnchorRole::Declaration,
        ScopeTier::Reachable,
        Some(2),
        Some(2),
        false,
    );
    let reach = HashMap::from([(
        "src/api.c".to_string(),
        ReachScope {
            files: ["include/api.h".to_string()].into_iter().collect(),
            heuristic_files: Default::default(),
            open: false,
            reason: None,
        },
    )]);

    let groups = resolve_counterparts(
        &[source.clone(), header],
        &reach,
        &CandidateCoverage::complete(2),
    );

    assert_eq!(groups.len(), 1);
    assert_eq!(
        groups[0].counterpart_evidence,
        CounterpartEvidence::StrictOneToOne
    );
    assert_eq!(hover_presentations(&groups)[0].anchor.path, "include/api.h");
    assert_eq!(
        signature_presentations(&groups)[0].anchor.path,
        "include/api.h"
    );
    assert_eq!(
        call_definition_presentations(&groups)[0].anchor.path,
        "src/api.c"
    );
    assert_eq!(
        call_declaration_presentations(&groups)[0].anchor.path,
        "include/api.h"
    );
    assert_eq!(
        anchor_opposite_definition(&groups, &source.anchor.anchor_fingerprint)
            .expect("opposite")
            .anchor
            .path,
        "include/api.h"
    );
}

#[test]
fn presentation_order_uses_the_strongest_tier_of_a_counterpart_group() {
    let paired_header = resolved(
        "include/paired.h",
        AnchorRole::Declaration,
        ScopeTier::Global,
        Some(2),
        Some(2),
        false,
    );
    let paired_source = resolved(
        "src/paired.c",
        AnchorRole::Definition,
        ScopeTier::Current,
        Some(2),
        Some(2),
        false,
    );
    let reachable_header = resolved(
        "include/reachable.h",
        AnchorRole::Declaration,
        ScopeTier::Reachable,
        Some(2),
        Some(2),
        false,
    );
    let groups = vec![
        CallableVariantGroup {
            header_declaration: Some(paired_header),
            source_definition: Some(paired_source),
            other_variants: Vec::new(),
            group_tier: ScopeTier::Current,
            counterpart_evidence: CounterpartEvidence::StrictOneToOne,
        },
        CallableVariantGroup {
            header_declaration: Some(reachable_header),
            source_definition: None,
            other_variants: Vec::new(),
            group_tier: ScopeTier::Reachable,
            counterpart_evidence: CounterpartEvidence::Unpaired,
        },
    ];

    assert_eq!(
        hover_presentations(&groups)[0].anchor.path,
        "include/paired.h"
    );
    assert_eq!(
        call_definition_presentations(&groups)[0].anchor.path,
        "src/paired.c"
    );
}

#[test]
fn definition_presentations_return_only_definitions_and_keep_competitors() {
    let declaration = resolved(
        "include/api.h",
        AnchorRole::Declaration,
        ScopeTier::Current,
        Some(2),
        Some(2),
        false,
    );
    let first = resolved(
        "src/first.c",
        AnchorRole::Definition,
        ScopeTier::Reachable,
        Some(2),
        Some(2),
        false,
    );
    let second = resolved(
        "src/second.c",
        AnchorRole::Definition,
        ScopeTier::Reachable,
        Some(2),
        Some(2),
        false,
    );
    let groups = vec![
        CallableVariantGroup {
            header_declaration: Some(declaration),
            source_definition: None,
            other_variants: Vec::new(),
            group_tier: ScopeTier::Current,
            counterpart_evidence: CounterpartEvidence::Unpaired,
        },
        CallableVariantGroup {
            header_declaration: None,
            source_definition: Some(first),
            other_variants: Vec::new(),
            group_tier: ScopeTier::Reachable,
            counterpart_evidence: CounterpartEvidence::Ambiguous { candidate_edges: 2 },
        },
        CallableVariantGroup {
            header_declaration: None,
            source_definition: Some(second),
            other_variants: Vec::new(),
            group_tier: ScopeTier::Reachable,
            counterpart_evidence: CounterpartEvidence::Ambiguous { candidate_edges: 2 },
        },
    ];

    let paths: Vec<_> = call_definition_presentations(&groups)
        .into_iter()
        .map(|anchor| anchor.anchor.path.as_str())
        .collect();
    assert_eq!(paths, vec!["src/first.c", "src/second.c"]);
}

#[test]
fn strict_pair_dominance_applies_only_at_the_strongest_relevant_tier() {
    let paired_header = resolved(
        "include/api.h",
        AnchorRole::Declaration,
        ScopeTier::Reachable,
        Some(2),
        Some(2),
        false,
    );
    let paired_source = resolved(
        "src/api.c",
        AnchorRole::Definition,
        ScopeTier::Current,
        Some(2),
        Some(2),
        false,
    );
    let fallback_definition = resolved(
        "other/api.c",
        AnchorRole::Definition,
        ScopeTier::Global,
        Some(2),
        Some(2),
        false,
    );
    let fallback_declaration = resolved(
        "src/local.h",
        AnchorRole::Declaration,
        ScopeTier::Current,
        Some(2),
        Some(2),
        false,
    );
    let groups = vec![
        CallableVariantGroup {
            header_declaration: Some(paired_header),
            source_definition: Some(paired_source),
            other_variants: Vec::new(),
            group_tier: ScopeTier::Current,
            counterpart_evidence: CounterpartEvidence::StrictOneToOne,
        },
        CallableVariantGroup {
            header_declaration: None,
            source_definition: Some(fallback_definition),
            other_variants: Vec::new(),
            group_tier: ScopeTier::Global,
            counterpart_evidence: CounterpartEvidence::Unpaired,
        },
        CallableVariantGroup {
            header_declaration: Some(fallback_declaration),
            source_definition: None,
            other_variants: Vec::new(),
            group_tier: ScopeTier::Current,
            counterpart_evidence: CounterpartEvidence::Unpaired,
        },
    ];

    let definitions = call_definition_presentations(&groups);
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0].anchor.path, "src/api.c");
    let declarations = call_declaration_presentations(&groups);
    assert_eq!(declarations.len(), 1);
    assert_eq!(declarations[0].anchor.path, "src/local.h");
}

#[test]
fn declaration_presentations_choose_highest_tier_nearest_declaration_then_definition_fallback() {
    let mut older = resolved(
        "src/main.c",
        AnchorRole::Declaration,
        ScopeTier::Current,
        Some(2),
        Some(2),
        false,
    );
    older.anchor.name_range = source_range(10);
    let mut nearer = older.clone();
    nearer.anchor.name_range = source_range(40);
    nearer.anchor.anchor_fingerprint = "nearer".into();
    let reachable = resolved(
        "include/api.h",
        AnchorRole::Declaration,
        ScopeTier::Reachable,
        Some(2),
        Some(2),
        false,
    );
    let declaration_groups = vec![
        CallableVariantGroup {
            header_declaration: None,
            source_definition: None,
            other_variants: vec![older],
            group_tier: ScopeTier::Current,
            counterpart_evidence: CounterpartEvidence::Unpaired,
        },
        CallableVariantGroup {
            header_declaration: None,
            source_definition: None,
            other_variants: vec![nearer],
            group_tier: ScopeTier::Current,
            counterpart_evidence: CounterpartEvidence::Unpaired,
        },
        CallableVariantGroup {
            header_declaration: Some(reachable),
            source_definition: None,
            other_variants: Vec::new(),
            group_tier: ScopeTier::Reachable,
            counterpart_evidence: CounterpartEvidence::Unpaired,
        },
    ];
    let declarations = call_declaration_presentations(&declaration_groups);
    assert_eq!(declarations.len(), 1);
    assert_eq!(declarations[0].anchor.anchor_fingerprint, "nearer");

    let mut older_definition = resolved(
        "src/main.c",
        AnchorRole::Definition,
        ScopeTier::Current,
        Some(2),
        Some(2),
        false,
    );
    older_definition.anchor.name_range = source_range(20);
    let mut nearer_definition = older_definition.clone();
    nearer_definition.anchor.name_range = source_range(60);
    nearer_definition.anchor.anchor_fingerprint = "nearer-definition".into();
    let definition_groups = vec![
        CallableVariantGroup {
            header_declaration: None,
            source_definition: Some(older_definition),
            other_variants: Vec::new(),
            group_tier: ScopeTier::Current,
            counterpart_evidence: CounterpartEvidence::Unpaired,
        },
        CallableVariantGroup {
            header_declaration: None,
            source_definition: Some(nearer_definition),
            other_variants: Vec::new(),
            group_tier: ScopeTier::Current,
            counterpart_evidence: CounterpartEvidence::Unpaired,
        },
    ];
    let fallback = call_declaration_presentations(&definition_groups);
    assert_eq!(fallback.len(), 1);
    assert_eq!(fallback[0].anchor.anchor_fingerprint, "nearer-definition");
}

#[test]
fn declaration_presentations_ignore_same_file_redeclarations_after_the_use() {
    let mut before = resolved(
        "src/main.c",
        AnchorRole::Declaration,
        ScopeTier::Current,
        Some(2),
        Some(2),
        false,
    );
    before.anchor.name_range = source_range(10);
    before.anchor.anchor_fingerprint = "before".into();
    let mut after = before.clone();
    after.anchor.name_range = source_range(40);
    after.anchor.anchor_fingerprint = "after".into();
    let groups = vec![
        CallableVariantGroup {
            header_declaration: None,
            source_definition: None,
            other_variants: vec![before],
            group_tier: ScopeTier::Current,
            counterpart_evidence: CounterpartEvidence::Unpaired,
        },
        CallableVariantGroup {
            header_declaration: None,
            source_definition: None,
            other_variants: vec![after],
            group_tier: ScopeTier::Current,
            counterpart_evidence: CounterpartEvidence::Unpaired,
        },
    ];

    let declarations = call_declaration_presentations_at(&groups, "src/main.c", 25);

    assert_eq!(declarations.len(), 1);
    assert_eq!(declarations[0].anchor.anchor_fingerprint, "before");
}

#[test]
fn active_signature_prefers_a_proven_compatible_group_without_reordering() {
    let mut current_unknown = resolved(
        "include/current.h",
        AnchorRole::Declaration,
        ScopeTier::Current,
        Some(0),
        None,
        false,
    );
    current_unknown.arity_compatibility = ArityCompatibility::Unknown;
    let mut reachable_compatible = resolved(
        "include/reachable.h",
        AnchorRole::Declaration,
        ScopeTier::Reachable,
        Some(2),
        Some(2),
        false,
    );
    reachable_compatible.arity_compatibility = ArityCompatibility::Compatible;
    let groups = vec![
        CallableVariantGroup {
            header_declaration: Some(current_unknown),
            source_definition: None,
            other_variants: Vec::new(),
            group_tier: ScopeTier::Current,
            counterpart_evidence: CounterpartEvidence::Unpaired,
        },
        CallableVariantGroup {
            header_declaration: Some(reachable_compatible),
            source_definition: None,
            other_variants: Vec::new(),
            group_tier: ScopeTier::Reachable,
            counterpart_evidence: CounterpartEvidence::Unpaired,
        },
    ];

    let presentations = signature_presentations(&groups);
    assert_eq!(presentations[0].anchor.path, "include/current.h");
    assert_eq!(signature_active_index(&presentations), 1);
}

#[test]
fn ambiguity_and_incomplete_coverage_never_claim_unique_counterpart() {
    let header = resolved(
        "include/api.h",
        AnchorRole::Declaration,
        ScopeTier::Reachable,
        Some(2),
        Some(2),
        false,
    );
    let source_a = resolved(
        "src/a.c",
        AnchorRole::Definition,
        ScopeTier::Reachable,
        Some(2),
        Some(2),
        false,
    );
    let source_b = resolved(
        "src/b.c",
        AnchorRole::Definition,
        ScopeTier::Reachable,
        Some(2),
        Some(2),
        false,
    );
    let closed = ReachScope {
        files: ["include/api.h".to_string()].into_iter().collect(),
        heuristic_files: Default::default(),
        open: false,
        reason: None,
    };
    let reach = HashMap::from([
        ("src/a.c".to_string(), closed.clone()),
        ("src/b.c".to_string(), closed),
    ]);
    let anchors = vec![header, source_a, source_b];

    let set = resolve_callable_candidates(CallableQueryInput {
        base_anchors: anchors.clone(),
        overlay_anchors: Vec::new(),
        shadowed_paths: HashSet::new(),
        call_context: None,
        source_reach: reach.clone(),
        visible_internal_paths: HashSet::new(),
        coverage: CandidateCoverage::complete(3),
    });
    assert_eq!(set.metrics().counterpart_strict, 0);
    assert_eq!(set.metrics().counterpart_ambiguous, 3);

    let groups = resolve_counterparts(&anchors, &reach, &CandidateCoverage::complete(3));
    assert_eq!(groups.len(), 3);
    assert!(groups
        .iter()
        .all(|group| { group.counterpart_evidence != CounterpartEvidence::StrictOneToOne }));

    let truncated = CandidateCoverage {
        scanned: 3,
        truncated: true,
        scope_open: false,
        incomplete_reason: Some(CandidateIncompleteReason::ScanLimit),
    };
    let groups = resolve_counterparts(&anchors, &reach, &truncated);
    assert!(groups
        .iter()
        .all(|group| { group.counterpart_evidence == CounterpartEvidence::IncompleteCoverage }));

    let incomplete = CandidateCoverage {
        scanned: 3,
        truncated: false,
        scope_open: true,
        incomplete_reason: Some(CandidateIncompleteReason::FactsUnavailable),
    };
    let groups = resolve_counterparts(&anchors, &reach, &incomplete);
    assert!(groups
        .iter()
        .all(|group| { group.counterpart_evidence == CounterpartEvidence::IncompleteCoverage }));
}

#[test]
fn unrelated_open_source_does_not_block_a_known_counterpart() {
    let header = resolved(
        "include/api.h",
        AnchorRole::Declaration,
        ScopeTier::Reachable,
        Some(2),
        Some(2),
        false,
    );
    let closed_source = resolved(
        "src/closed.c",
        AnchorRole::Definition,
        ScopeTier::Reachable,
        Some(2),
        Some(2),
        false,
    );
    let open_source = resolved(
        "src/open.c",
        AnchorRole::Definition,
        ScopeTier::Reachable,
        Some(2),
        Some(2),
        false,
    );
    let reach = HashMap::from([
        (
            "src/closed.c".to_string(),
            ReachScope {
                files: ["include/api.h".to_string()].into_iter().collect(),
                heuristic_files: Default::default(),
                open: false,
                reason: None,
            },
        ),
        (
            "src/open.c".to_string(),
            ReachScope {
                files: HashSet::new(),
                heuristic_files: Default::default(),
                open: true,
                reason: Some(crate::reachability::OpenReason::UnresolvedInclude),
            },
        ),
    ]);

    let coverage = CandidateCoverage {
        scanned: 3,
        truncated: false,
        scope_open: true,
        incomplete_reason: None,
    };
    let groups = resolve_counterparts(&[header, closed_source, open_source], &reach, &coverage);
    assert_eq!(groups.len(), 2);
    assert_eq!(
        groups
            .iter()
            .filter(|group| { group.counterpart_evidence == CounterpartEvidence::StrictOneToOne })
            .count(),
        1
    );
    assert_eq!(
        groups
            .iter()
            .filter(|group| group.counterpart_evidence == CounterpartEvidence::Unpaired)
            .count(),
        1
    );
}

#[test]
fn counterpart_requires_external_exact_signature_and_known_reach() {
    let source = resolved(
        "src/api.C",
        AnchorRole::Definition,
        ScopeTier::Reachable,
        Some(2),
        Some(2),
        false,
    );
    let header = resolved(
        "include/api.H",
        AnchorRole::Declaration,
        ScopeTier::Reachable,
        Some(2),
        Some(2),
        false,
    );
    let reach = |open, includes_header| {
        HashMap::from([(
            "src/api.C".to_string(),
            ReachScope {
                files: if includes_header {
                    ["include/api.H".to_string()].into_iter().collect()
                } else {
                    HashSet::new()
                },
                heuristic_files: Default::default(),
                open,
                reason: None,
            },
        )])
    };
    let assert_unpaired = |source: ResolvedCallableAnchor,
                           header: ResolvedCallableAnchor,
                           reach: HashMap<String, ReachScope>| {
        let groups =
            resolve_counterparts(&[source, header], &reach, &CandidateCoverage::complete(2));
        assert!(groups
            .iter()
            .all(|group| group.counterpart_evidence != CounterpartEvidence::StrictOneToOne));
    };

    let open_coverage = CandidateCoverage {
        scanned: 2,
        truncated: false,
        scope_open: true,
        incomplete_reason: None,
    };
    let groups = resolve_counterparts(
        &[source.clone(), header.clone()],
        &reach(true, true),
        &open_coverage,
    );
    assert_eq!(groups.len(), 1);
    assert_eq!(
        groups[0].counterpart_evidence,
        CounterpartEvidence::StrictOneToOne
    );
    assert_unpaired(source.clone(), header.clone(), reach(false, false));

    let mut internal = source.clone();
    internal.anchor.linkage = LinkageDomain::Internal("src/api.C".to_string());
    assert_unpaired(internal, header.clone(), reach(false, true));

    let mut lexical = header.clone();
    lexical.anchor.signature_fidelity = SignatureFidelity::LexicalFallback;
    assert_unpaired(source.clone(), lexical, reach(false, true));

    let mut signature_mismatch = header;
    signature_mismatch.anchor.canonical_signature = "int pick(long lhs, long rhs)".to_string();
    assert_unpaired(source, signature_mismatch, reach(false, true));
}

#[test]
fn counterpart_does_not_cross_namespaces_or_pair_record_members() {
    let source = resolved(
        "src/api.cpp",
        AnchorRole::Definition,
        ScopeTier::Reachable,
        Some(2),
        Some(2),
        false,
    );
    let header = resolved(
        "include/api.hpp",
        AnchorRole::Declaration,
        ScopeTier::Reachable,
        Some(2),
        Some(2),
        false,
    );
    let reach = HashMap::from([(
        "src/api.cpp".to_string(),
        ReachScope {
            files: ["include/api.hpp".to_string()].into_iter().collect(),
            heuristic_files: Default::default(),
            open: false,
            reason: None,
        },
    )]);
    let assert_not_unique = |source: ResolvedCallableAnchor, header: ResolvedCallableAnchor| {
        assert!(
            resolve_counterparts(&[source, header], &reach, &CandidateCoverage::complete(2),)
                .iter()
                .all(|group| group.counterpart_evidence != CounterpartEvidence::StrictOneToOne)
        );
    };

    let mut other_namespace = header.clone();
    other_namespace.anchor.owner = Some("other".to_string());
    other_namespace.anchor.owner_kind = Some(crate::call_model::OwnerKindHint::Namespace);
    other_namespace.anchor.qualified_name = "other::pick".to_string();
    assert_not_unique(source.clone(), other_namespace);

    let mut member_source = source;
    member_source.anchor.owner = Some("Widget".to_string());
    member_source.anchor.owner_kind = Some(crate::call_model::OwnerKindHint::Record);
    member_source.anchor.qualified_name = "Widget::pick".to_string();
    let mut member_header = header;
    member_header.anchor.owner = Some("Widget".to_string());
    member_header.anchor.owner_kind = Some(crate::call_model::OwnerKindHint::Record);
    member_header.anchor.qualified_name = "Widget::pick".to_string();
    assert_not_unique(member_source, member_header);
}

#[test]
fn unknown_explicit_owner_is_conservative_but_never_a_strict_counterpart() {
    let mut source = resolved(
        "src/api.cpp",
        AnchorRole::Definition,
        ScopeTier::Current,
        Some(2),
        Some(2),
        false,
    );
    source.anchor.owner = Some("net".to_string());
    source.anchor.owner_kind = Some(OwnerKindHint::Unknown);
    source.anchor.qualified_name = "net::pick".to_string();
    let mut header = resolved(
        "include/api.hpp",
        AnchorRole::Declaration,
        ScopeTier::Reachable,
        Some(2),
        Some(2),
        false,
    );
    header.anchor.owner = Some("net".to_string());
    header.anchor.owner_kind = Some(OwnerKindHint::Namespace);
    header.anchor.qualified_name = "net::pick".to_string();
    let mut context = complete_call(Some(2));
    context.form = CallForm::QualifiedName;
    context.qualified_name = Some("net::pick".to_string());
    let reach = ReachScope {
        files: ["include/api.hpp".to_string()].into_iter().collect(),
        heuristic_files: Default::default(),
        open: false,
        reason: None,
    };

    let set = resolve_callable_candidates(CallableQueryInput {
        base_anchors: vec![source, header],
        overlay_anchors: Vec::new(),
        shadowed_paths: HashSet::new(),
        call_context: Some(context),
        source_reach: HashMap::from([("src/api.cpp".to_string(), reach)]),
        visible_internal_paths: HashSet::new(),
        coverage: CandidateCoverage::complete(2),
    });

    assert_eq!(set.anchors.len(), 2, "Unknown owner remains navigable");
    assert!(set
        .groups
        .iter()
        .all(|group| group.counterpart_evidence != CounterpartEvidence::StrictOneToOne));
}

#[test]
fn unsupported_member_context_cannot_launder_name_candidates() {
    let mut unknown_owner = resolved(
        "src/widget.cpp",
        AnchorRole::Definition,
        ScopeTier::Current,
        Some(0),
        Some(0),
        false,
    );
    unknown_owner.anchor.owner = Some("Widget".to_string());
    unknown_owner.anchor.owner_kind = Some(OwnerKindHint::Unknown);
    unknown_owner.anchor.qualified_name = "Widget::pick".to_string();
    let free_function = resolved(
        "src/free.cpp",
        AnchorRole::Definition,
        ScopeTier::Reachable,
        Some(0),
        Some(0),
        false,
    );
    let mut context = complete_call(Some(0));
    context.form = CallForm::MemberDot;
    context.reliability = ContextReliability::UnsupportedCallForm;

    let set = resolve_callable_candidates(CallableQueryInput {
        base_anchors: vec![unknown_owner, free_function],
        overlay_anchors: Vec::new(),
        shadowed_paths: HashSet::new(),
        call_context: Some(context),
        source_reach: HashMap::new(),
        visible_internal_paths: HashSet::new(),
        coverage: CandidateCoverage::complete(2),
    });

    assert!(set.anchors.is_empty());
    assert!(set.groups.is_empty());
}

#[test]
fn candidate_metrics_are_aggregate_and_cover_arity_and_counterparts() {
    let source = resolved(
        "src/api.c",
        AnchorRole::Definition,
        ScopeTier::Current,
        Some(2),
        Some(2),
        false,
    );
    let header = resolved(
        "include/api.h",
        AnchorRole::Declaration,
        ScopeTier::Reachable,
        Some(2),
        Some(2),
        false,
    );
    let reach = ReachScope {
        files: ["include/api.h".to_string()].into_iter().collect(),
        heuristic_files: Default::default(),
        open: false,
        reason: None,
    };
    let set = resolve_callable_candidates(CallableQueryInput {
        base_anchors: vec![source, header],
        overlay_anchors: Vec::new(),
        shadowed_paths: HashSet::new(),
        call_context: Some(complete_call(Some(2))),
        source_reach: HashMap::from([("src/api.c".to_string(), reach)]),
        visible_internal_paths: HashSet::new(),
        coverage: CandidateCoverage::complete(4),
    });

    assert_eq!(
        set.metrics(),
        CallableCandidateMetrics {
            raw_candidates: 4,
            filtered_candidates: 2,
            grouped_candidates: 1,
            arity_compatible: 2,
            arity_unknown: 0,
            arity_incompatible: 0,
            counterpart_strict: 1,
            counterpart_ambiguous: 0,
        }
    );
}

#[test]
fn internal_linkage_candidates_are_confined_to_the_current_translation_unit() {
    let mut visible = resolved(
        "src/current.c",
        AnchorRole::Definition,
        ScopeTier::Current,
        Some(0),
        Some(0),
        false,
    );
    visible.anchor.linkage = LinkageDomain::Internal("src/current.c".to_string());
    let mut unrelated = resolved(
        "src/other.c",
        AnchorRole::Definition,
        ScopeTier::Global,
        Some(0),
        Some(0),
        false,
    );
    unrelated.anchor.linkage = LinkageDomain::Internal("src/other.c".to_string());

    let set = resolve_callable_candidates(CallableQueryInput {
        base_anchors: vec![unrelated, visible],
        overlay_anchors: Vec::new(),
        shadowed_paths: HashSet::new(),
        call_context: None,
        source_reach: HashMap::new(),
        visible_internal_paths: HashSet::from(["src/current.c".to_string()]),
        coverage: CandidateCoverage::complete(2),
    });

    assert_eq!(set.anchors.len(), 1);
    assert_eq!(set.anchors[0].anchor.path, "src/current.c");
}

#[test]
fn pure_resolver_shadows_base_paths_and_deduplicates_overlay_fingerprints() {
    let stale = resolved(
        "include/api.h",
        AnchorRole::Declaration,
        ScopeTier::Reachable,
        Some(1),
        Some(1),
        false,
    );
    let mut overlay = resolved(
        "include/api.h",
        AnchorRole::Declaration,
        ScopeTier::Reachable,
        Some(2),
        Some(2),
        false,
    );
    overlay.origin = CandidateOrigin::Overlay;
    let duplicate = overlay.clone();

    let set = resolve_callable_candidates(CallableQueryInput {
        base_anchors: vec![stale],
        overlay_anchors: vec![overlay, duplicate],
        shadowed_paths: HashSet::from(["include/api.h".to_string()]),
        call_context: Some(complete_call(Some(2))),
        source_reach: HashMap::new(),
        visible_internal_paths: HashSet::new(),
        coverage: CandidateCoverage::complete(2),
    });

    assert_eq!(set.anchors.len(), 1);
    assert_eq!(set.anchors[0].origin, CandidateOrigin::Overlay);
    assert_eq!(
        set.anchors[0].arity_compatibility,
        ArityCompatibility::Compatible
    );
}

#[test]
fn cross_file_fingerprint_collision_cannot_create_a_false_one_to_one_pair() {
    let source = resolved(
        "src/pick.c",
        AnchorRole::Definition,
        ScopeTier::Current,
        Some(2),
        Some(2),
        false,
    );
    let mut first_header = resolved(
        "include/first.h",
        AnchorRole::Declaration,
        ScopeTier::Reachable,
        Some(2),
        Some(2),
        false,
    );
    let mut second_header = resolved(
        "include/second.h",
        AnchorRole::Declaration,
        ScopeTier::Reachable,
        Some(2),
        Some(2),
        false,
    );
    // Defensive proof: even legacy/imported facts that reused a fingerprint
    // remain distinct anchors because path/range are part of request identity.
    first_header.anchor.anchor_fingerprint = "legacy-collision".into();
    second_header.anchor.anchor_fingerprint = "legacy-collision".into();
    let reach = ReachScope {
        files: [
            "src/pick.c".to_string(),
            "include/first.h".to_string(),
            "include/second.h".to_string(),
        ]
        .into_iter()
        .collect(),
        heuristic_files: Default::default(),
        open: false,
        reason: None,
    };
    let set = resolve_callable_candidates(CallableQueryInput {
        base_anchors: vec![source, first_header, second_header],
        overlay_anchors: Vec::new(),
        shadowed_paths: HashSet::new(),
        call_context: None,
        source_reach: HashMap::from([("src/pick.c".to_string(), reach)]),
        visible_internal_paths: HashSet::new(),
        coverage: CandidateCoverage::complete(3),
    });
    assert_eq!(set.anchors.len(), 3);
    assert!(set
        .groups
        .iter()
        .all(|group| { group.counterpart_evidence != CounterpartEvidence::StrictOneToOne }));
}
