use std::path::Path;

use super::{
    infer_receiver_record, parse, parse_with_handle, FileSemanticIndex, MemberConfidence,
    MemberKind, Occurrence, ParseFacts, ParserHandle, SymbolKind, SymbolRole, SyntacticRole,
};

/// Role of the (single) occurrence of `name` in a parsed buffer.
fn role_of(path: &str, source: &str, name: &str) -> Option<SyntacticRole> {
    let index = parse(Path::new(path), source);
    index
        .occurrences
        .iter()
        .find(|occ| occ.name == name)
        .map(|occ| occ.role)
}

/// Role of `name`'s occurrence on a specific (0-based) line. The occurrence
/// vec is not in source order, so position-keyed lookup is deterministic.
fn role_at_line(path: &str, source: &str, name: &str, line: u32) -> Option<SyntacticRole> {
    let index = parse(Path::new(path), source);
    index
        .occurrences
        .iter()
        .find(|occ| occ.name == name && occ.line == line)
        .map(|occ| occ.role)
}

#[test]
fn role_classifies_call_assignment_type_and_read() {
    // line: 0 prototype, 1 def header, 2 decl, 3 assign+call, 4 incr, 5 return
    let src = "int g(void);\nint f(widget_t *w) {\n    int x;\n    x = g();\n    x++;\n    return x;\n}\n";
    // `g` is a prototype declaration on line 0, a call on line 3.
    assert_eq!(
        role_at_line("a.c", src, "g", 0),
        Some(SyntacticRole::Declaration)
    );
    assert_eq!(role_at_line("a.c", src, "g", 3), Some(SyntacticRole::Call));
    // `widget_t` (line 1) is a type use.
    assert_eq!(
        role_at_line("a.c", src, "widget_t", 1),
        Some(SyntacticRole::TypeUse)
    );
    // `w` (line 1) is a parameter binding.
    assert_eq!(
        role_at_line("a.c", src, "w", 1),
        Some(SyntacticRole::Declaration)
    );
    // `x`: declared (line 2), written (lines 3, 4), read (line 5).
    assert_eq!(
        role_at_line("a.c", src, "x", 2),
        Some(SyntacticRole::Declaration)
    );
    assert_eq!(role_at_line("a.c", src, "x", 5), Some(SyntacticRole::Read));
}

#[test]
fn role_marks_assignment_target_as_write() {
    // `y` declared on line 1, assigned on line 2.
    let src = "void f(void) {\n    int y;\n    y = 1;\n}\n";
    assert_eq!(
        role_at_line("a.c", src, "y", 1),
        Some(SyntacticRole::Declaration)
    );
    assert_eq!(role_at_line("a.c", src, "y", 2), Some(SyntacticRole::Write));
}

#[test]
fn role_marks_increment_target_as_write() {
    let src = "void f(void) {\n    int c;\n    c++;\n}\n";
    assert_eq!(role_at_line("a.c", src, "c", 2), Some(SyntacticRole::Write));
}

#[test]
fn role_in_error_region_falls_back_to_read() {
    // A top-level expression is invalid C, so this lands in an error region;
    // `stray` must still be emitted as an occurrence, with role Read.
    let src = "1 + stray;\n";
    let index = parse(Path::new("a.c"), src);
    let occ = index
        .occurrences
        .iter()
        .find(|occ| occ.name == "stray")
        .expect("stray occurrence still emitted in an error region");
    assert_eq!(occ.role, SyntacticRole::Read);
}

#[test]
fn role_marks_definitions() {
    // Macro and enum definition sites are Definition; function body is a
    // Definition; a prototype name is a Declaration.
    assert_eq!(
        role_of("a.c", "#define FOO 1\n", "FOO"),
        Some(SyntacticRole::Definition)
    );
    assert_eq!(
        role_of("a.c", "enum E { RED };\n", "RED"),
        Some(SyntacticRole::Definition)
    );
    assert_eq!(
        role_of("a.c", "int main(void) { return 0; }\n", "main"),
        Some(SyntacticRole::Definition)
    );
    assert_eq!(
        role_of("a.c", "int proto(void);\n", "proto"),
        Some(SyntacticRole::Declaration)
    );
}

#[test]
fn role_cpp_field_declaration_and_type_use() {
    // Limited C++: a class with a typed data member. The member type is a
    // TypeUse; an instance declaration of the class is a TypeUse for the
    // class name and a Declaration for the variable.
    let src = "class Widget { int count; };\nWidget makeWidget(void);\nWidget w;\n";
    assert_eq!(
        role_of("a.cpp", src, "Widget"),
        Some(SyntacticRole::TypeUse)
    );
    // `w` is the declared variable.
    assert_eq!(role_of("a.cpp", src, "w"), Some(SyntacticRole::Declaration));
}

fn field_containers(index: &FileSemanticIndex, name: &str) -> Vec<String> {
    index
        .fields
        .iter()
        .filter(|f| f.name == name)
        .map(|f| {
            index
                .records
                .iter()
                .find(|r| r.record_key == f.record_key)
                .map(|r| r.display_name.clone())
                .unwrap_or_default()
        })
        .collect()
}

#[test]
fn extracts_named_struct_fields() {
    let index = parse(Path::new("p.c"), "struct Point { int x; int y; };\n");
    assert_eq!(field_containers(&index, "x"), vec!["Point".to_string()]);
    assert_eq!(field_containers(&index, "y"), vec!["Point".to_string()]);
}

#[test]
fn parses_class_body_methods_as_members() {
    let source = r#"
        class Widget {
        public:
            int width;
            void resize(int w);
            static int count();
        };
    "#;
    let index = parse(Path::new("widget.cpp"), source);

    assert!(index
        .members
        .iter()
        .any(|member| member.name == "width" && member.kind == MemberKind::Field));
    assert!(index
        .members
        .iter()
        .any(|member| member.name == "resize" && member.kind == MemberKind::Method));
    assert!(index
        .members
        .iter()
        .any(|member| member.name == "count" && member.kind == MemberKind::StaticMethod));
}

#[test]
fn method_member_signature_uses_declaration_text() {
    let source = "struct Widget { void resize(int width); };";
    let index = parse(Path::new("widget.hpp"), source);
    let method = index
        .members
        .iter()
        .find(|member| member.name == "resize")
        .expect("method");

    assert_eq!(method.kind, MemberKind::Method);
    assert!(method.signature.contains("void resize(int width)"));
    assert_eq!(method.confidence, MemberConfidence::InBody);
}

#[test]
fn parses_simple_out_of_class_method_owner_as_lower_confidence() {
    let source = r#"
        class Widget { void resize(); };
        void Widget::resize() {}
    "#;
    let index = parse(Path::new("widget.cpp"), source);
    let matches: Vec<_> = index
        .members
        .iter()
        .filter(|member| member.name == "resize")
        .collect();

    assert!(matches
        .iter()
        .any(|member| member.confidence == MemberConfidence::InBody));
    assert!(matches
        .iter()
        .any(|member| member.confidence == MemberConfidence::OutOfClassOwner));
}

#[test]
fn parser_handle_reuses_across_c_and_cpp_language_switches() {
    let handle = ParserHandle::new();
    let c_index = parse_with_handle(
        Path::new("point.c"),
        "struct Point { int x; int y; };\n",
        Some(&handle),
        ParseFacts::ALL,
    );
    let cpp_index = parse_with_handle(
        Path::new("box.cpp"),
        "class Box { int value; };\n",
        Some(&handle),
        ParseFacts::ALL,
    );

    assert_eq!(field_containers(&c_index, "x"), vec!["Point".to_string()]);
    assert_eq!(
        field_containers(&cpp_index, "value"),
        vec!["Box".to_string()]
    );
}

#[test]
fn extracts_anonymous_typedef_struct_fields() {
    let index = parse(
        Path::new("b.c"),
        "typedef struct { int len; char *data; } Buffer;\n",
    );
    assert_eq!(field_containers(&index, "len"), vec!["Buffer".to_string()]);
    assert_eq!(field_containers(&index, "data"), vec!["Buffer".to_string()]);
}

#[test]
fn extracts_multiline_typedef_struct_type_symbol() {
    let index = parse(
        Path::new("b.c"),
        "typedef struct {\n    int x;\n    int y;\n} Boom;\n",
    );

    assert!(index.symbols.iter().any(|symbol| {
        symbol.name == "Boom"
            && symbol.kind == SymbolKind::Type
            && symbol.role == SymbolRole::Definition
    }));
    assert_eq!(field_containers(&index, "x"), vec!["Boom".to_string()]);
}

#[test]
fn field_members_capture_record_type_name() {
    let index = parse(
        Path::new("nested.c"),
        "struct Inner { int value; };\ntypedef struct Inner Inner;\nstruct Outer { struct Inner mem1; Inner *mem2; int count; };\n",
    );

    let mem1 = index
        .members
        .iter()
        .find(|member| member.name == "mem1")
        .expect("mem1");
    assert_eq!(mem1.type_name.as_deref(), Some("Inner"));

    let mem2 = index
        .members
        .iter()
        .find(|member| member.name == "mem2")
        .expect("mem2");
    assert_eq!(mem2.type_name.as_deref(), Some("Inner"));

    let count = index
        .members
        .iter()
        .find(|member| member.name == "count")
        .expect("count");
    assert_eq!(count.type_name, None);
}

#[test]
fn flattens_nested_anonymous_union_fields() {
    let index = parse(
        Path::new("v.c"),
        "struct Var { int tag; union { int i; float f; }; };\n",
    );
    assert_eq!(field_containers(&index, "tag"), vec!["Var".to_string()]);
    assert_eq!(field_containers(&index, "i"), vec!["Var".to_string()]);
    assert_eq!(field_containers(&index, "f"), vec!["Var".to_string()]);
}

#[test]
fn records_typedef_alias_to_tag() {
    let index = parse(
        Path::new("a.c"),
        "struct Foo { int a; };\ntypedef struct Foo FooT;\n",
    );
    assert!(index.aliases.iter().any(|alias| alias.alias == "FooT"
        && matches!(&alias.target, super::AliasTarget::NamedRecord { tag, .. } if tag == "Foo")));
    // Fields stay attributed to the tag, reachable from the alias via the store.
    assert_eq!(field_containers(&index, "a"), vec!["Foo".to_string()]);
}

#[test]
fn test_record_field_alias_identity_extended() {
    let src = r#"
            // 1. Named struct W
            struct W {
                int field_w1;
            };

            // 2. Anonymous typedef struct
            typedef struct {
                int field_widget;
            } Widget;

            // 3. Typedef struct Foo FooT (where Foo has a body)
            struct Foo {
                int field_foo;
            };
            typedef struct Foo FooT;

            // 4. Nested anonymous field flattening
            struct Nested {
                int tag;
                union {
                    int i;
                    float f;
                };
            };

            // 5. Same file record-key disambiguation (second struct W)
            struct W_second {
                int field_w2;
            };
        "#;
    let index = parse(Path::new("test.c"), src);

    // 1. Check named struct W
    let w_rec = index
        .records
        .iter()
        .find(|r| r.display_name == "W")
        .unwrap();
    assert_eq!(w_rec.tag_name.as_deref(), Some("W"));
    assert_eq!(w_rec.typedef_name, None);
    assert_eq!(w_rec.kind, super::RecordKind::Struct);
    assert_eq!(w_rec.confidence, super::RecordConfidence::NamedTag);

    let w_fields: Vec<&str> = index
        .fields
        .iter()
        .filter(|f| f.record_key == w_rec.record_key)
        .map(|f| f.name.as_str())
        .collect();
    assert_eq!(w_fields, vec!["field_w1"]);

    // 2. Check anonymous typedef struct Widget
    let widget_rec = index
        .records
        .iter()
        .find(|r| r.display_name == "Widget")
        .unwrap();
    assert_eq!(widget_rec.tag_name, None);
    assert_eq!(widget_rec.typedef_name.as_deref(), Some("Widget"));
    assert_eq!(
        widget_rec.confidence,
        super::RecordConfidence::AnonymousTypedef
    );

    let widget_fields: Vec<&str> = index
        .fields
        .iter()
        .filter(|f| f.record_key == widget_rec.record_key)
        .map(|f| f.name.as_str())
        .collect();
    assert_eq!(widget_fields, vec!["field_widget"]);

    // 3. Check typedef FooT alias
    let foot_alias = index.aliases.iter().find(|a| a.alias == "FooT").unwrap();
    assert!(
        matches!(&foot_alias.target, super::AliasTarget::NamedRecord { tag, .. } if tag == "Foo")
    );

    // 4. Check nested anonymous field flattening
    let nested_rec = index
        .records
        .iter()
        .find(|r| r.display_name == "Nested")
        .unwrap();
    let nested_fields: Vec<&str> = index
        .fields
        .iter()
        .filter(|f| f.record_key == nested_rec.record_key)
        .map(|f| f.name.as_str())
        .collect();
    assert_eq!(nested_fields, vec!["tag", "i", "f"]);

    // 5. Check same file record-key disambiguation
    let w_second_rec = index
        .records
        .iter()
        .find(|r| r.display_name == "W_second")
        .unwrap();
    assert_ne!(w_rec.record_key, w_second_rec.record_key);

    let w_second_fields: Vec<&str> = index
        .fields
        .iter()
        .filter(|f| f.record_key == w_second_rec.record_key)
        .map(|f| f.name.as_str())
        .collect();
    assert_eq!(w_second_fields, vec!["field_w2"]);
}

#[test]
fn coloring_collects_enum_definitions() {
    let defs = parse(Path::new("e.c"), "enum Color { RED, GREEN, BLUE };\n").coloring_defs();
    assert!(defs.enum_defs.contains("RED"));
    assert!(defs.enum_defs.contains("GREEN"));
    assert!(defs.enum_defs.contains("BLUE"));
}

/// Receiver inference over the parsed product's local declarations (the same
/// data the server feeds `infer_receiver_record`).
fn infer_in(path: &str, source: &str, name: &str, byte_offset: usize) -> Option<String> {
    let index = parse(Path::new(path), source);
    infer_receiver_record(&index.local_declarations, name, byte_offset)
}

#[test]
fn infers_receiver_record_for_local_param_and_file_scope() {
    // Local pointer via `->`.
    let local = "void f(void) {\n    struct Foo *p;\n    p->x;\n}\n";
    let off = local.find("p->").expect("usage") + 1;
    assert_eq!(infer_in("a.c", local, "p", off).as_deref(), Some("Foo"));

    // Function parameter via `->`.
    let param = "int g(struct Bar *b) {\n    return b->v;\n}\n";
    let off = param.find("b->").expect("usage") + 1;
    assert_eq!(infer_in("a.c", param, "b", off).as_deref(), Some("Bar"));

    // File-scope variable via `.`.
    let file_scope = "struct Baz top;\nvoid h(void) {\n    top.x = 1;\n}\n";
    let off = file_scope.find("top.").expect("usage") + 1;
    assert_eq!(
        infer_in("a.c", file_scope, "top", off).as_deref(),
        Some("Baz")
    );

    // Unknown receiver yields nothing (caller then falls back).
    assert_eq!(infer_in("a.c", local, "missing", off), None);
}

#[test]
fn local_bindings_collect_parameters_and_locals_in_function() {
    let src = "int f(int count, struct Foo *foo) {\n    int cursor_limit = count;\n    char *name;\n    return cursor_limit;\n}\n";
    let index = parse(Path::new("a.c"), src);
    let names: Vec<(&str, super::LocalBindingKind)> = index
        .local_bindings
        .iter()
        .map(|binding| (binding.name.as_str(), binding.kind))
        .collect();
    assert!(names.contains(&("count", super::LocalBindingKind::Parameter)));
    assert!(names.contains(&("foo", super::LocalBindingKind::Parameter)));
    assert!(names.contains(&("cursor_limit", super::LocalBindingKind::LocalVariable)));
    assert!(names.contains(&("name", super::LocalBindingKind::LocalVariable)));
    assert!(index
        .local_bindings
        .iter()
        .all(|binding| binding.function_start_byte < binding.function_end_byte));
}

#[test]
fn local_bindings_ignore_file_scope_declarations() {
    let src = "int global_value;\nvoid f(void) {\n    int local_value;\n}\n";
    let index = parse(Path::new("a.c"), src);
    assert!(index.local_bindings.iter().any(|b| b.name == "local_value"));
    assert!(index
        .local_bindings
        .iter()
        .all(|b| b.name != "global_value"));
}

#[test]
fn local_bindings_are_empty_without_function_definition() {
    let src = "#define Z 1\n";
    let index = parse(Path::new("a.c"), src);
    assert!(index.local_bindings.is_empty());
}

fn occurrence_lines(occurrences: &[Occurrence], name: &str) -> Vec<u32> {
    occurrences
        .iter()
        .filter(|occ| occ.name == name)
        .map(|occ| occ.line)
        .collect()
}

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
fn coloring_collects_macro_definition_and_usages() {
    let source = r#"#define FOO 1
int main(void) {
    return FOO + FOO;
}
"#;
    let index = parse(Path::new("main.c"), source);
    let defs = index.coloring_defs();
    assert!(defs.macro_defs.contains("FOO"));
    assert!(defs.type_defs.is_empty());
    // The define site (line 0) plus two usages on line 2.
    let foo_lines = occurrence_lines(&index.occurrences, "FOO");
    assert!(foo_lines.contains(&0));
    assert_eq!(foo_lines.iter().filter(|&&l| l == 2).count(), 2);
}

#[test]
fn coloring_collects_type_definitions() {
    let source = r#"typedef struct { int x; } widget_t;
struct Node { int v; };
enum Color { RED, GREEN };
widget_t make(void);
struct Node *head;
enum Color current;
"#;
    let index = parse(Path::new("types.c"), source);
    let defs = index.coloring_defs();
    assert!(defs.type_defs.contains("widget_t"));
    assert!(defs.type_defs.contains("Node"));
    assert!(defs.type_defs.contains("Color"));
    // Usages are recorded as occurrences.
    assert!(!occurrence_lines(&index.occurrences, "widget_t").is_empty());
    assert!(!occurrence_lines(&index.occurrences, "Node").is_empty());
    assert!(!occurrence_lines(&index.occurrences, "Color").is_empty());
}

#[test]
fn coloring_skips_identifiers_in_comments_and_strings() {
    let source = r#"#define FOO 1
// FOO mentioned in a comment
const char *s = "FOO in a string";
"#;
    let index = parse(Path::new("main.c"), source);
    // Only the define-site FOO (line 0) is an occurrence; comment/string text
    // never reaches the syntax tree as identifiers.
    let foo_lines = occurrence_lines(&index.occurrences, "FOO");
    assert_eq!(foo_lines, vec![0]);
}

#[test]
fn coloring_positions_use_utf16_columns() {
    let prefix = r#"int main(void) { const char *s = "中文"; return "#;
    let source = format!("#define FOO 1\n{prefix}FOO;\n");
    let index = parse(Path::new("main.c"), &source);
    let usage = index
        .occurrences
        .iter()
        .find(|occ| occ.name == "FOO" && occ.line == 1)
        .expect("FOO usage");

    assert_eq!(usage.start_col, prefix.encode_utf16().count() as u32);
    assert_eq!(usage.length, 3);
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
    assert_eq!(d.symbols_source, super::FactSource::Lexical);
    assert_eq!(d.ast_source, super::FactSource::Ast);
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
    assert_eq!(index.diagnostics.ast_source, super::FactSource::Ast);
    assert!(index.diagnostics.parse_error_count > 0);
    assert!(index.symbols.iter().any(|s| s.name == "OK"));
}

#[test]
fn lexical_fallback_product_has_lexical_facts_and_no_ast() {
    // The fallback product (returned when tree-sitter yields no usable tree)
    // keeps lexical symbols/includes, empties AST facts, and is distinguishable
    // from a clean parse by `fallback_used` / `ast_source`.
    let source = "#include \"x.h\"\n#define Z 9\n";
    let ls = super::line_starts(source);
    let (symbols, includes) = super::extract_symbols_and_includes(source, &ls);
    let index = super::lexical_fallback(symbols, includes);
    assert!(index.diagnostics.fallback_used);
    assert_eq!(
        index.diagnostics.ast_source,
        super::FactSource::LexicalFallback
    );
    assert_eq!(index.diagnostics.symbols_source, super::FactSource::Lexical);
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
        let got = super::compact_whitespace(case);
        let expected = old_impl(case);
        assert_eq!(
            got, expected,
            "Mismatch for input {:?}: got {:?}, expected {:?}",
            case, got, expected
        );
    }
}
