use std::path::Path;

use super::super::{parse, SyntacticRole};
use super::{occurrence_lines, role_at_line, role_of};

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

#[test]
fn coloring_collects_enum_definitions() {
    let defs = parse(Path::new("e.c"), "enum Color { RED, GREEN, BLUE };\n").coloring_defs();
    assert!(defs.enum_defs.contains("RED"));
    assert!(defs.enum_defs.contains("GREEN"));
    assert!(defs.enum_defs.contains("BLUE"));
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
