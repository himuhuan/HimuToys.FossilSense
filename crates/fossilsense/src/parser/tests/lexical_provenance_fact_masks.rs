use std::path::Path;

use super::super::{
    compact_whitespace, extract_symbols_and_includes, lexical_fallback,
    lexical_fallback_with_facts, line_starts, parse, parse_with_handle, FactAvailability,
    FactGroup, FactSource, FactUnavailableReason, ParseFacts, SymbolKind, SymbolRole,
};
use super::{fact_mask_source, has_symbol};

#[test]
fn extracts_mini_c_symbols() {
    let source = r#"#include "hello.h"
#define ANSWER 42
int hello_value(void);
int main(void) {
    return hello_value();
}
"#;

    let index = parse(Path::new("main.c"), source);
    assert!(index
        .includes
        .iter()
        .any(|include| include.target_text == "\"hello.h\""));
    assert!(index
        .symbols
        .iter()
        .any(|symbol| { symbol.name == "ANSWER" && symbol.kind == SymbolKind::Macro }));
    assert!(index
        .symbols
        .iter()
        .any(|symbol| { symbol.name == "hello_value" && symbol.role == SymbolRole::Declaration }));
    assert!(index
        .symbols
        .iter()
        .any(|symbol| { symbol.name == "main" && symbol.role == SymbolRole::Definition }));
}

#[test]
fn leading_comments_do_not_pollute_symbol_signature_or_start_line() {
    let source = "#define VALUE 1\n/// @brief Helps the smoke test.\nvoid helper(void);\n";
    let index = parse(Path::new("defs.h"), source);
    let symbol = index
        .symbols
        .iter()
        .find(|symbol| symbol.name == "helper")
        .expect("helper symbol");

    assert_eq!(symbol.start_line, 2);
    assert_eq!(symbol.signature, "void helper(void);");
}

#[test]
fn records_preprocessor_guard() {
    let source = r#"#ifdef CONFIG_X
int guarded(void);
#endif
"#;

    let index = parse(Path::new("guarded.h"), source);
    let symbol = index
        .symbols
        .iter()
        .find(|symbol| symbol.name == "guarded")
        .expect("guarded symbol");

    assert_eq!(symbol.guard.as_deref(), Some("#ifdef CONFIG_X"));
}

#[test]
fn parse_reports_ast_provenance_on_clean_file() {
    // A syntactically valid file: lexical symbols and AST facts coexist in one
    // product, with `fallback_used = false` and AST provenance.
    let index = parse(
        Path::new("a.c"),
        "#define M 1\nstruct S { int x; };\ntypedef struct S St;\nvoid f(void) { struct S s; }\n",
    );
    let d = index.diagnostics;
    assert!(!d.fallback_used);
    assert_eq!(d.symbols_source, FactSource::Lexical);
    assert_eq!(d.ast_source, FactSource::Ast);
    // Lexical fact (macro) and AST facts (record/occurrences/alias/local decl)
    // are all present on the single product.
    assert!(index.symbols.iter().any(|s| s.name == "M"));
    assert!(index.records.iter().any(|r| r.display_name == "S"));
    assert!(!index.occurrences.is_empty());
    assert!(index.aliases.iter().any(|a| a.alias == "St"));
    assert!(index
        .local_declarations
        .iter()
        .any(|l| l.name == "s" && l.record_type == "S"));
}

#[test]
fn parse_keeps_lexical_symbols_through_parse_errors() {
    // A stray token yields an error-laden but still usable tree. That is NOT
    // the lexical-fallback path: lexical symbols are extracted, AST facts come
    // from the error tree, and the error count is non-zero.
    let index = parse(Path::new("b.c"), "#define OK 1\n@\n");
    assert!(!index.diagnostics.fallback_used);
    assert_eq!(index.diagnostics.ast_source, FactSource::Ast);
    assert!(index.diagnostics.parse_error_count > 0);
    assert!(index.symbols.iter().any(|s| s.name == "OK"));
}

#[test]
fn lexical_fallback_product_has_lexical_facts_and_no_ast() {
    // The fallback product (returned when tree-sitter yields no usable tree)
    // keeps lexical symbols/includes, empties AST facts, and is distinguishable
    // from a clean parse by `fallback_used` / `ast_source`.
    let source = "#include \"x.h\"\n#define Z 9\n";
    let ls = line_starts(source);
    let (symbols, includes) = extract_symbols_and_includes(source, &ls);
    let index = lexical_fallback(symbols, includes);
    assert!(index.diagnostics.fallback_used);
    assert_eq!(index.diagnostics.ast_source, FactSource::LexicalFallback);
    assert_eq!(index.diagnostics.symbols_source, FactSource::Lexical);
    assert_eq!(index.diagnostics.parse_error_count, 0);
    assert!(index.symbols.iter().any(|s| s.name == "Z"));
    assert_eq!(index.includes.len(), 1);
    assert!(index.occurrences.is_empty());
    assert!(index.records.is_empty());
    assert!(index.local_declarations.is_empty());
}

#[test]
fn compact_whitespace_equivalence_fuzzy() {
    // Single-pass implementation must match split_whitespace behavior exactly.
    // Test various whitespace patterns: none, leading, trailing, internal,
    // mixed (spaces, tabs, newlines), and typical C code fragments.
    fn old_impl(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    let cases = [
        "",
        "hello",
        "  leading spaces",
        "trailing spaces   ",
        "multiple   internal   spaces",
        "\t\ttabs\t\tand\tspaces\t",
        "line1\nline2\n\nline3",
        "mixed \t whitespace \n newlines \r\n here",
        "   ",
        "\t\n\r",
        "int main(void) { return 0; }",
        "#define FOO(x)  ((x) * (x))",
        "struct  node  {  int  val;  struct  node  *next;  };",
        "a",           // single char
        "  a  b  c  ", // short with padding
    ];

    for case in cases {
        let got = compact_whitespace(case);
        let expected = old_impl(case);
        assert_eq!(
            got, expected,
            "Mismatch for input {:?}: got {:?}, expected {:?}",
            case, got, expected
        );
    }
}

#[test]
fn parse_fact_masks_document_current_field_contents() {
    let path = Path::new("facts.cpp");

    let index = parse_with_handle(path, fact_mask_source(), None, ParseFacts::INDEX);
    let persistent = index.persistent_facts();
    assert_eq!(persistent.symbols.len(), index.symbols.len());
    assert_eq!(persistent.includes.len(), index.includes.len());
    assert_eq!(persistent.records.len(), index.records.len());
    assert_eq!(persistent.fields.len(), index.fields.len());
    assert_eq!(persistent.members.len(), index.members.len());
    assert_eq!(persistent.aliases.len(), index.aliases.len());
    assert!(has_symbol(&index, "FLAG", SymbolKind::Macro));
    assert!(has_symbol(&index, "RED", SymbolKind::EnumConstant));
    assert_eq!(index.includes.len(), 1);
    assert!(index
        .records
        .iter()
        .any(|record| record.display_name == "Widget"));
    assert!(index.fields.iter().any(|field| field.name == "width"));
    assert!(index.members.iter().any(|member| member.name == "resize"));
    assert!(index
        .aliases
        .iter()
        .any(|alias| alias.alias == "WidgetAlias"));
    assert!(index.occurrences.is_empty());
    assert!(index.local_declarations.is_empty());
    assert!(index.local_bindings.is_empty());
    assert_eq!(index.diagnostics.ast_source, FactSource::Ast);
    assert_eq!(index.diagnostics.requested_facts, ParseFacts::INDEX);
    assert_eq!(
        index.fact_availability(FactGroup::Occurrences),
        FactAvailability::NotRequested
    );
    assert_eq!(
        index.fact_availability(FactGroup::LocalDeclarations),
        FactAvailability::NotRequested
    );
    assert_eq!(
        index.fact_availability(FactGroup::LocalBindings),
        FactAvailability::NotRequested
    );
    assert_eq!(
        index.fact_availability(FactGroup::Records),
        FactAvailability::Available
    );
    assert_eq!(
        index.fact_availability(FactGroup::Fields),
        FactAvailability::Available
    );
    assert_eq!(
        index.fact_availability(FactGroup::Members),
        FactAvailability::Available
    );
    assert_eq!(
        index.fact_availability(FactGroup::Aliases),
        FactAvailability::Available
    );

    let color_ref = parse_with_handle(path, fact_mask_source(), None, ParseFacts::COLOR_REF);
    let request = color_ref.request_facts();
    assert_eq!(request.occurrences.len(), color_ref.occurrences.len());
    assert_eq!(
        request.local_declarations.len(),
        color_ref.local_declarations.len()
    );
    assert_eq!(request.local_bindings.len(), color_ref.local_bindings.len());
    assert!(has_symbol(&color_ref, "FLAG", SymbolKind::Macro));
    assert!(has_symbol(&color_ref, "RED", SymbolKind::EnumConstant));
    assert_eq!(color_ref.includes.len(), 1);
    assert!(color_ref.occurrences.iter().any(|occ| occ.name == "w"));
    assert!(color_ref.records.is_empty());
    assert!(color_ref.fields.is_empty());
    assert!(color_ref.members.is_empty());
    assert!(color_ref.aliases.is_empty());
    assert!(color_ref.local_declarations.is_empty());
    assert!(color_ref.local_bindings.is_empty());
    assert_eq!(
        color_ref.fact_availability(FactGroup::Occurrences),
        FactAvailability::Available
    );
    assert_eq!(
        color_ref.fact_availability(FactGroup::Records),
        FactAvailability::NotRequested
    );
    assert_eq!(
        color_ref.fact_availability(FactGroup::LocalDeclarations),
        FactAvailability::NotRequested
    );

    let member = parse_with_handle(path, fact_mask_source(), None, ParseFacts::MEMBER);
    assert!(has_symbol(&member, "FLAG", SymbolKind::Macro));
    assert_eq!(member.includes.len(), 1);
    assert!(member.occurrences.is_empty());
    assert!(member
        .records
        .iter()
        .any(|record| record.display_name == "Widget"));
    assert!(member.members.iter().any(|m| m.name == "width"));
    assert!(member
        .aliases
        .iter()
        .any(|alias| alias.alias == "WidgetAlias"));
    assert!(member
        .local_declarations
        .iter()
        .any(|decl| decl.name == "w" && decl.record_type == "Widget"));
    assert!(member
        .local_bindings
        .iter()
        .any(|binding| binding.name == "local_value"));
    assert_eq!(
        member.fact_availability(FactGroup::Occurrences),
        FactAvailability::NotRequested
    );
    assert_eq!(
        member.fact_availability(FactGroup::LocalDeclarations),
        FactAvailability::Available
    );
    assert_eq!(
        member.fact_availability(FactGroup::LocalBindings),
        FactAvailability::Available
    );

    let all = parse_with_handle(path, fact_mask_source(), None, ParseFacts::ALL);
    assert!(has_symbol(&all, "FLAG", SymbolKind::Macro));
    assert!(has_symbol(&all, "RED", SymbolKind::EnumConstant));
    assert_eq!(all.includes.len(), 1);
    assert!(all.occurrences.iter().any(|occ| occ.name == "FLAG"));
    assert!(all
        .records
        .iter()
        .any(|record| record.display_name == "Widget"));
    assert!(all.members.iter().any(|m| m.name == "resize"));
    assert!(all.aliases.iter().any(|alias| alias.alias == "WidgetAlias"));
    assert!(all.local_declarations.iter().any(|decl| decl.name == "w"));
    assert!(all
        .local_bindings
        .iter()
        .any(|binding| binding.name == "count"));
    assert_eq!(
        all.fact_availability(FactGroup::Occurrences),
        FactAvailability::Available
    );
    assert_eq!(
        all.fact_availability(FactGroup::LocalDeclarations),
        FactAvailability::Available
    );
    assert_eq!(
        parse(Path::new("facts.cpp"), fact_mask_source())
            .diagnostics
            .requested_facts,
        ParseFacts::ALL
    );
}

#[test]
fn records_only_mask_keeps_member_facts_not_requested() {
    let index = parse_with_handle(
        Path::new("records_only.cpp"),
        fact_mask_source(),
        None,
        ParseFacts::RECORDS,
    );

    assert!(index
        .records
        .iter()
        .any(|record| record.display_name == "Widget"));
    assert!(index.fields.is_empty());
    assert!(index.members.is_empty());
    assert_eq!(
        index.fact_availability(FactGroup::Records),
        FactAvailability::Available
    );
    assert_eq!(
        index.fact_availability(FactGroup::Fields),
        FactAvailability::NotRequested
    );
    assert_eq!(
        index.fact_availability(FactGroup::Members),
        FactAvailability::NotRequested
    );
}

#[test]
fn availability_distinguishes_empty_skipped_and_fallback_ast_vectors() {
    let path = Path::new("facts.cpp");
    let all = parse_with_handle(path, fact_mask_source(), None, ParseFacts::ALL);
    assert!(!all.local_declarations.is_empty());

    let skipped = parse_with_handle(path, fact_mask_source(), None, ParseFacts::INDEX);
    assert!(skipped.local_declarations.is_empty());
    assert!(skipped.local_bindings.is_empty());
    assert!(!skipped.diagnostics.fallback_used);
    assert_eq!(skipped.diagnostics.ast_source, FactSource::Ast);
    assert_eq!(
        skipped.fact_availability(FactGroup::LocalDeclarations),
        FactAvailability::NotRequested
    );
    assert_eq!(
        skipped.fact_availability(FactGroup::LocalBindings),
        FactAvailability::NotRequested
    );

    let clean_empty = parse_with_handle(
        Path::new("empty.c"),
        "int only_global;\n",
        None,
        ParseFacts::ALL,
    );
    assert!(clean_empty.records.is_empty());
    assert!(clean_empty.members.is_empty());
    assert!(clean_empty.aliases.is_empty());
    assert!(clean_empty.local_declarations.is_empty());
    assert!(!clean_empty.diagnostics.fallback_used);
    assert_eq!(clean_empty.diagnostics.ast_source, FactSource::Ast);
    assert_eq!(
        clean_empty.fact_availability(FactGroup::Records),
        FactAvailability::Available
    );
    assert_eq!(
        clean_empty.fact_availability(FactGroup::Members),
        FactAvailability::Available
    );
    assert_eq!(
        clean_empty.fact_availability(FactGroup::LocalDeclarations),
        FactAvailability::Available
    );

    let fallback_source = "#include \"x.h\"\n#define ONLY_LEXICAL 1\n";
    let line_starts = line_starts(fallback_source);
    let (symbols, includes) = extract_symbols_and_includes(fallback_source, &line_starts);
    let fallback = lexical_fallback_with_facts(symbols, includes, ParseFacts::ALL);
    assert!(fallback.records.is_empty());
    assert!(fallback.members.is_empty());
    assert!(fallback.aliases.is_empty());
    assert!(fallback.local_declarations.is_empty());
    assert!(fallback.local_bindings.is_empty());
    assert!(fallback.diagnostics.fallback_used);
    assert_eq!(fallback.diagnostics.ast_source, FactSource::LexicalFallback);
    assert_eq!(fallback.diagnostics.requested_facts, ParseFacts::ALL);
    assert_eq!(
        fallback.fact_availability(FactGroup::Records),
        FactAvailability::Unavailable(FactUnavailableReason::LexicalFallback)
    );
    assert_eq!(
        fallback.fact_availability(FactGroup::Members),
        FactAvailability::Unavailable(FactUnavailableReason::LexicalFallback)
    );
    assert_eq!(
        fallback.fact_availability(FactGroup::LocalDeclarations),
        FactAvailability::Unavailable(FactUnavailableReason::LexicalFallback)
    );

    let (symbols, includes) = extract_symbols_and_includes(fallback_source, &line_starts);
    let fallback_index = lexical_fallback_with_facts(symbols, includes, ParseFacts::INDEX);
    assert_eq!(
        fallback_index.fact_availability(FactGroup::Occurrences),
        FactAvailability::NotRequested
    );
    assert_eq!(
        fallback_index.fact_availability(FactGroup::LocalDeclarations),
        FactAvailability::NotRequested
    );
    assert_eq!(
        fallback_index.fact_availability(FactGroup::Records),
        FactAvailability::Unavailable(FactUnavailableReason::LexicalFallback)
    );

    assert_eq!(skipped.local_declarations, clean_empty.local_declarations);
    assert_eq!(clean_empty.local_declarations, fallback.local_declarations);
    assert_eq!(clean_empty.records, fallback.records);
}
