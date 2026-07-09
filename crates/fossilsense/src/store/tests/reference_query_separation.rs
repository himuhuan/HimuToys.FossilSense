use std::fs;
use std::path::Path;

fn read(path: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(root.join(path)).unwrap_or_else(|err| panic!("failed to read {path}: {err}"))
}

fn strip_line_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| line.split_once("//").map_or(line, |(code, _)| code))
        .collect::<Vec<_>>()
        .join("\n")
}

fn reference_hit_body(source: &str) -> String {
    let start = source
        .find("pub struct ReferenceHit")
        .expect("ReferenceHit definition");
    let after_start = &source[start..];
    let open = after_start.find('{').expect("ReferenceHit body open");
    let mut depth = 0usize;
    let mut end = open;
    for (idx, ch) in after_start[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = open + idx + ch.len_utf8();
                    break;
                }
            }
            _ => {}
        }
    }
    after_start[open..end].to_string()
}

fn reference_hit_scope_tier_violations(source: &str) -> Vec<&'static str> {
    let body = reference_hit_body(source);
    [
        "ScopeTier",
        "ResolutionConfidence",
        "ResolutionReason",
        "tier:",
        "scope:",
        "confidence:",
        "reason:",
        "score:",
        "candidate:",
    ]
    .into_iter()
    .filter(|forbidden| body.contains(forbidden))
    .collect()
}

fn function_body(source: &str, signature: &str) -> String {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("{signature} definition"));
    let after_start = &source[start..];
    let open = after_start.find('{').expect("function body open");
    let mut depth = 0usize;
    let mut end = open;
    for (idx, ch) in after_start[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = open + idx + ch.len_utf8();
                    break;
                }
            }
            _ => {}
        }
    }
    after_start[open..end].to_string()
}

fn assert_reference_hit_has_no_scope_tier_data(source: &str) {
    if let Some(forbidden) = reference_hit_scope_tier_violations(source)
        .into_iter()
        .next()
    {
        panic!("ReferenceHit must stay a text hit with syntactic role only; found `{forbidden}`");
    }
}

#[test]
fn references_module_stays_resolver_free() {
    let source = strip_line_comments(&read("src/references.rs"));

    for forbidden in [
        "crate::resolver",
        "resolver::",
        "pack_score",
        "scope_tier",
        "confidence_reason_for",
    ] {
        assert!(
            !source.contains(forbidden),
            "references.rs must not import resolver ranking; found `{forbidden}`"
        );
    }
}

#[test]
fn reference_hit_stays_text_hit_plus_syntactic_role() {
    let source = strip_line_comments(&read("src/references.rs"));

    for required in [
        "pub rel_path: String",
        "pub line: u32",
        "pub start_col_utf16: u32",
        "pub end_col_utf16: u32",
        "pub role: SyntacticRole",
    ] {
        assert!(
            source.contains(required),
            "ReferenceHit should keep required text-hit field `{required}`"
        );
    }
    assert_reference_hit_has_no_scope_tier_data(&source);
}

#[test]
fn references_route_role_classification_through_role_fact_helper() {
    let source = strip_line_comments(&read("src/references.rs"));

    for required in [
        "fn reference_role_facts(",
        "ParseFacts::COLOR_REF",
        "request_facts()",
        "FactGroup::Occurrences",
        "FactAvailability::Available",
    ] {
        assert!(
            source.contains(required),
            "references.rs should make reference role-fact helper usage explicit, missing `{required}`"
        );
    }

    let position_roles = function_body(&source, "fn position_roles(");
    assert!(
        position_roles.contains("reference_role_facts("),
        "position_roles should delegate occurrence projection to the reference role-fact helper"
    );
    assert!(
        !position_roles.contains("request_facts()"),
        "position_roles should not inline request-facts interpretation"
    );
}

#[test]
fn reference_separation_guard_fixture_catches_scope_tier_on_reference_hit() {
    let fixture = "pub struct ReferenceHit { pub rel_path: String, pub tier: ScopeTier }";

    let violations = reference_hit_scope_tier_violations(fixture);

    assert!(
        violations.contains(&"ScopeTier") && violations.contains(&"tier:"),
        "guard fixture should fail when ReferenceHit carries ScopeTier-style data"
    );
}
