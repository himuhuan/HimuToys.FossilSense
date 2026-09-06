use super::*;

#[test]
fn health_check_rejects_a_clean_reparse_that_loses_an_unaffected_declaration() {
    let source = "int keep;\nint target __aligned(8);\n";
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_c::LANGUAGE.into())
        .expect("C grammar");
    let before = parser.parse(source, None).expect("original tree");
    let mut rewritten = source.as_bytes().to_vec();
    mask_non_newlines(&mut rewritten, 0.."int keep;".len());
    let attribute_start = source.find("__aligned").expect("attribute");
    mask_non_newlines(
        &mut rewritten,
        attribute_start..attribute_start + "__aligned(8)".len(),
    );
    let rewritten = String::from_utf8(rewritten).expect("masked UTF-8");
    let after = parser
        .parse(&rewritten, None)
        .expect("clean rewritten tree");
    let target_start = source.find("int target").expect("target");
    let target_range = target_start..source.len();

    assert!(!healthy_observations_unchanged(
        before.root_node(),
        after.root_node(),
        source,
        std::slice::from_ref(&target_range),
    ));
}

#[test]
fn health_check_rejects_an_unaffected_declaration_kind_change() {
    let source = "int keep  ;\nint target __aligned(8);\n";
    let rewritten = "int keep();\nint target             ;\n";
    assert_eq!(source.len(), rewritten.len());
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_c::LANGUAGE.into())
        .expect("C grammar");
    let before = parser.parse(source, None).expect("original tree");
    let after = parser.parse(rewritten, None).expect("rewritten tree");
    let target_start = source.find("int target").expect("target");
    let target_range = target_start..source.len();

    assert!(!healthy_observations_unchanged(
        before.root_node(),
        after.root_node(),
        source,
        std::slice::from_ref(&target_range),
    ));
}
