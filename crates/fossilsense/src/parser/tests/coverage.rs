use super::*;
use crate::semantic_model::{CoverageEvidence, CoverageReason, FactCoverage};

#[test]
fn declaration_coverage_available_partial_and_checked_empty_are_distinct() {
    let parsed = parse(
        Path::new("partial.c"),
        "DECLARE_HANDLER(net);\nint healthy;\n",
    );
    assert_eq!(
        parsed.fact_availability(FactGroup::Declarations),
        FactAvailability::Available
    );
    assert_eq!(
        parsed.fact_coverage(FactGroup::Declarations),
        FactCoverage::Partial
    );
    let names: Vec<_> = parsed
        .declarations
        .iter()
        .map(|fact| fact.name.as_str())
        .collect();
    assert_eq!(names, ["healthy"]);
    assert_eq!(
        parsed.declarations[0].identity.fact_fidelity,
        SemanticFactFidelity::Authoritative
    );
    assert!(parsed
        .diagnostics
        .coverage
        .gaps
        .iter()
        .any(|gap| gap.reason == CoverageReason::UnknownMacro));
    for source in ["", "/* no declarations */", "int healthy;\n"] {
        let parsed = parse(Path::new("empty.c"), source);
        assert_eq!(
            parsed.fact_coverage(FactGroup::Declarations),
            FactCoverage::Complete
        );
        assert_eq!(
            parsed.fact_coverage(FactGroup::Aliases),
            FactCoverage::Complete
        );
    }
    let skipped = parse_with_handle(Path::new("empty.c"), "", None, ParseFacts::INCLUDES);
    assert_eq!(
        skipped.fact_availability(FactGroup::Declarations),
        FactAvailability::NotRequested
    );
    assert_eq!(
        skipped.fact_coverage(FactGroup::Declarations),
        FactCoverage::Unknown
    );
}

#[test]
fn declaration_coverage_unknown_macro_has_original_utf16_range() {
    let source = "/* 🙂 */\r\nDECLARE_HANDLER(net);\r\nint healthy;\r\n";
    let parsed = parse(Path::new("range.c"), source);
    let gap = parsed
        .diagnostics
        .coverage
        .gaps
        .iter()
        .find(|gap| gap.reason == CoverageReason::UnknownMacro)
        .unwrap();
    let start = source.find("DECLARE_HANDLER").unwrap();
    assert_eq!(gap.range.start_byte, start);
    assert_eq!(gap.range.end_byte, start + "DECLARE_HANDLER(net);".len());
    assert_eq!(gap.range.start.line, 1);
    assert_eq!(gap.range.start.character, 0);
    assert_eq!(gap.range.end.line, 1);
    assert_eq!(gap.range.end.character, 21);
}

#[test]
fn declaration_coverage_does_not_treat_expressions_as_declaration_macros() {
    let source = "/* DECLARE_HANDLER(net); */\nconst char *text = \"DECLARE_HANDLER(net);\";\nint value = INIT_VALUE(3);\nvoid work(int parameter) { CALL(parameter); }\n";
    let parsed = parse(Path::new("expressions.c"), source);
    assert_eq!(
        parsed.fact_coverage(FactGroup::Declarations),
        FactCoverage::Complete
    );
    assert!(
        parsed.diagnostics.coverage.gaps.is_empty(),
        "{:?}",
        parsed.diagnostics.coverage.gaps
    );
}

#[test]
fn declaration_coverage_known_typedef_parenthesized_object_is_not_a_macro() {
    let parsed = parse(
        Path::new("alias.c"),
        "typedef int Number;\nNumber (value);\n",
    );
    // tree-sitter C represents this legal spelling as a call expression.
    // Do not invent a declaration outside the shared decoder, or claim complete.
    assert!(parsed.declarations.iter().any(|fact| fact.name == "Number"));
    assert_eq!(
        parsed.fact_coverage(FactGroup::Declarations),
        FactCoverage::Partial
    );
    assert!(parsed
        .diagnostics
        .coverage
        .gaps
        .iter()
        .any(|gap| gap.reason == CoverageReason::UnsupportedDeclarator));
    assert!(!parsed
        .diagnostics
        .coverage
        .gaps
        .iter()
        .any(|gap| gap.reason == CoverageReason::UnknownMacro));
}

#[test]
fn declaration_coverage_shared_header_and_local_type_limits_are_visible() {
    let shared = parse(Path::new("shared.h"), "int healthy;\n");
    assert_eq!(
        shared.fact_coverage(FactGroup::Declarations),
        FactCoverage::Partial
    );
    assert!(shared
        .diagnostics
        .coverage
        .gaps
        .iter()
        .any(|gap| gap.reason == CoverageReason::LanguageAmbiguity));
    let source =
        "int healthy;\nvoid work(void) { struct Local { int field; }; typedef int LocalAlias; }\n";
    let parsed = parse(Path::new("local.c"), source);
    assert!(parsed
        .declarations
        .iter()
        .all(|fact| !["Local", "LocalAlias"].contains(&fact.name.as_str())));
    assert!(parsed
        .diagnostics
        .coverage
        .gaps
        .iter()
        .any(|gap| gap.evidence == CoverageEvidence::ScopeNotSupported));
    assert_eq!(
        parsed.fact_coverage(FactGroup::Records),
        FactCoverage::Partial
    );
    assert_eq!(
        parsed.fact_coverage(FactGroup::Aliases),
        FactCoverage::Partial
    );
}

#[test]
fn declaration_coverage_budgets_keep_a_tail_gap_and_never_claim_complete() {
    use crate::parser::coverage::{with_limits, CoverageLimits};
    for limits in [
        CoverageLimits {
            regions: 1,
            ..CoverageLimits::default()
        },
        CoverageLimits {
            details: 1,
            ..CoverageLimits::default()
        },
        CoverageLimits {
            nodes: 2,
            ..CoverageLimits::default()
        },
    ] {
        let parsed = with_limits(limits, || {
            parse(
                Path::new("limited.c"),
                "REGISTER(first);\nREGISTER(second);\nint healthy;\n",
            )
        });
        assert!(parsed.diagnostics.coverage.summary.truncated);
        assert!(parsed.diagnostics.coverage.gaps.len() <= limits.details);
        let tail = parsed
            .diagnostics
            .coverage
            .gaps
            .iter()
            .find(|gap| gap.reason == CoverageReason::BudgetExhausted)
            .unwrap();
        assert_eq!(
            tail.range.end_byte,
            "REGISTER(first);\nREGISTER(second);\nint healthy;\n".len()
        );
        assert_ne!(
            parsed.fact_coverage(FactGroup::Declarations),
            FactCoverage::Complete
        );
        assert_eq!(parsed.diagnostics.coverage.summary.uncovered_ratio(), None);
    }
}

#[test]
fn declaration_coverage_detail_truncation_does_not_restore_rejected_macro_facts() {
    use crate::parser::coverage::{with_limits, CoverageLimits};
    let parsed = with_limits(
        CoverageLimits {
            details: 1,
            ..CoverageLimits::default()
        },
        || {
            parse(
                Path::new("macro.c"),
                "DECLARE(first);\nDECLARE(second);\nint healthy;\n",
            )
        },
    );
    assert!(!parsed
        .declarations
        .iter()
        .any(|fact| ["first", "second", "DECLARE"].contains(&fact.name.as_str())));
    assert!(parsed
        .declarations
        .iter()
        .any(|fact| fact.name == "healthy"));
    assert!(parsed.diagnostics.coverage.summary.truncated);
}

#[test]
fn declaration_coverage_cpp_external_type_ambiguity_does_not_invent_a_fact() {
    use crate::parser::coverage::{with_limits, CoverageLimits};
    let source = "#include \"types.h\"\nNumber(value);\n";
    // No audit runs in this control. The original grammar produces an
    // expression and the existing canonical extractor has no object fact.
    let baseline = with_limits(
        CoverageLimits {
            nodes: 0,
            ..Default::default()
        },
        || parse(Path::new("case.cpp"), source),
    );
    let parsed = parse(Path::new("case.cpp"), source);
    assert!(baseline.declarations.is_empty());
    assert_eq!(parsed.declarations, baseline.declarations);
    assert_eq!(
        parsed.fact_coverage(FactGroup::Declarations),
        FactCoverage::Partial
    );
    assert!(parsed
        .diagnostics
        .coverage
        .gaps
        .iter()
        .any(|gap| gap.reason == CoverageReason::UnknownMacro));
}

#[test]
fn declaration_coverage_three_unknown_c_macros_after_detail_limit_have_no_entities() {
    use crate::parser::coverage::{with_limits, CoverageLimits};
    let parsed = with_limits(
        CoverageLimits {
            details: 1,
            ..CoverageLimits::default()
        },
        || {
            parse(
                Path::new("three.c"),
                "DECLARE(first);\nDECLARE(second);\nDECLARE(third);\nint healthy;\n",
            )
        },
    );
    let names: Vec<_> = parsed
        .declarations
        .iter()
        .map(|fact| fact.name.as_str())
        .collect();
    assert_eq!(names, ["healthy"]);
    assert!(parsed.diagnostics.coverage.summary.truncated);
    assert_ne!(
        parsed.fact_coverage(FactGroup::Declarations),
        FactCoverage::Complete
    );
}
