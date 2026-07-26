use std::path::Path;

use super::{
    infer_receiver_record, parse, parse_with_handle, FactAvailability, FactGroup,
    FactUnavailableReason, FileSemanticIndex, MemberConfidence, MemberKind, Occurrence, ParseFacts,
    ParserHandle, SymbolKind, SyntacticRole,
};
use crate::semantic_model::{
    ParseOutcome, SemanticDeclarationKind, SemanticDeclarationRole, SemanticLanguage,
};

#[test]
fn go_ast_projects_package_imports_build_guard_and_semantic_facts() {
    let source = r#"//go:build tinygo && arm

package sensor

import (
    spi "device/spi"
    _ "unsafe"
)

type Reader interface {
    Read([]byte) (int, error)
}

type Sample struct {
    Value int
}

type Alias = Sample

const DefaultRate = 100
var Global Sample

func NewSample(value int) Sample {
    return Sample{Value: value}
}

func (s *Sample) Read(out []byte) (int, error) {
    _ = spi.Mode0
    return copy(out, nil), nil
}

func Use() {
    _ = NewSample(DefaultRate)
}
"#;

    let index = parse(Path::new("sensor.go"), source);

    assert_eq!(
        index.package.as_ref().map(|package| package.name.as_str()),
        Some("sensor")
    );
    assert_eq!(index.build_guard.as_deref(), Some("tinygo && arm"));
    assert!(index
        .imports
        .iter()
        .any(|import| import.path == "device/spi" && import.alias.as_deref() == Some("spi")));
    assert!(index
        .imports
        .iter()
        .any(|import| import.path == "unsafe" && import.alias.as_deref() == Some("_")));

    for name in [
        "Reader",
        "Sample",
        "Alias",
        "DefaultRate",
        "Global",
        "NewSample",
        "Read",
        "Use",
    ] {
        let declaration = index
            .declarations
            .iter()
            .find(|declaration| declaration.name == name)
            .unwrap_or_else(|| panic!("missing Go declaration {name}"));
        assert_eq!(declaration.identity.language, SemanticLanguage::Go);
        assert_eq!(declaration.guard.as_deref(), Some("tinygo && arm"));
    }

    assert!(index.declarations.iter().any(|declaration| {
        declaration.name == "Read"
            && declaration.declaration_kind == SemanticDeclarationKind::Method
            && declaration.owner.as_deref() == Some("Sample")
    }));
    assert!(index.declarations.iter().any(|declaration| {
        declaration.name == "Alias"
            && declaration.declaration_kind == SemanticDeclarationKind::Alias
    }));
    assert!(index
        .callable_anchors
        .iter()
        .any(|anchor| { anchor.name == "NewSample" && anchor.signature.min_arity == Some(1) }));
    assert!(index.call_sites.iter().any(|call| {
        call.callee_name.as_deref() == Some("NewSample") && call.argument_count == Some(1)
    }));
    assert!(index
        .members
        .iter()
        .any(|member| member.name == "Value" && member.type_name.as_deref() == Some("int")));
    assert!(index.records.iter().any(|record| {
        record.display_name == "Reader"
            && record.kind == crate::semantic_model::RecordKind::Interface
    }));
    assert!(index.members.iter().any(|member| {
        member.name == "Read"
            && member.kind == MemberKind::Method
            && member.record_key == "go:.#sensor:Reader"
    }));
    assert!(!index.diagnostics.fallback_used);
}

#[test]
fn malformed_go_remains_a_bounded_partial_or_fallback_product() {
    let index = parse(
        Path::new("broken.go"),
        "package broken\nfunc Open(value int {\n\treturn value\n",
    );

    assert!(matches!(
        index.parse_outcome,
        ParseOutcome::PartialAst | ParseOutcome::LexicalFallback
    ));
    assert!(index
        .declarations
        .iter()
        .all(|declaration| declaration.identity.language == SemanticLanguage::Go));
    assert!(index.call_sites.len() <= 16);
}

#[test]
fn go_package_identity_is_physical_stable_and_round_trips_package_linkage() {
    let first = parse(
        Path::new("src/sensor/windows.go"),
        "//go:build windows\n\npackage sensor\n\
         type Device struct { Windows int }\n\
         func Open(path string) {}\n\
         func (device Device) Read(path string) {}\n\
         var Same = 1\n",
    );
    let second = parse(
        Path::new("src/sensor/linux.go"),
        "//go:build linux\n\npackage sensor\n\
         type Device struct { Linux int }\n\
         func Open(name string) {}\n\
         func (other Device) Read(name string) {}\n\
         var Same = 2\n",
    );
    let other = parse(
        Path::new("cmd/tool/main.go"),
        "package sensor\nfunc Open(name string) {}\nvar Same = 2\n",
    );
    let first_open = first
        .callable_anchors
        .iter()
        .find(|anchor| anchor.name == "Open")
        .expect("first Open");
    let second_open = second
        .callable_anchors
        .iter()
        .find(|anchor| anchor.name == "Open")
        .expect("second Open");
    let other_open = other
        .callable_anchors
        .iter()
        .find(|anchor| anchor.name == "Open")
        .expect("other Open");

    assert_eq!(first_open.entity_key, second_open.entity_key);
    assert_ne!(first_open.entity_key, other_open.entity_key);
    assert!(matches!(
        first_open.linkage,
        crate::call_model::LinkageDomain::Package(_)
    ));
    let first_open_declaration = first
        .declarations
        .iter()
        .find(|declaration| declaration.name == "Open")
        .expect("first Open declaration");
    let second_open_declaration = second
        .declarations
        .iter()
        .find(|declaration| declaration.name == "Open")
        .expect("second Open declaration");
    assert_eq!(
        first_open_declaration.identity.logical_key, second_open_declaration.identity.logical_key,
        "parameter names and build guards are evidence, not Go declaration identity"
    );
    assert_ne!(
        first_open_declaration.canonical_signature, second_open_declaration.canonical_signature,
        "the original signature remains presentation evidence"
    );

    let first_same = first
        .declarations
        .iter()
        .find(|declaration| declaration.name == "Same")
        .expect("first Same");
    let second_same = second
        .declarations
        .iter()
        .find(|declaration| declaration.name == "Same")
        .expect("second Same");
    assert_ne!(
        first_same.identity.locator.fingerprint,
        second_same.identity.locator.fingerprint
    );
    assert_eq!(
        first_same.identity.logical_key, second_same.identity.logical_key,
        "initializer text must not split a package object identity"
    );
    for name in ["Device", "Read"] {
        let first_fact = first
            .declarations
            .iter()
            .find(|declaration| declaration.name == name)
            .unwrap_or_else(|| panic!("first {name}"));
        let second_fact = second
            .declarations
            .iter()
            .find(|declaration| declaration.name == name)
            .unwrap_or_else(|| panic!("second {name}"));
        assert_eq!(
            first_fact.identity.logical_key, second_fact.identity.logical_key,
            "{name} build variants must share one logical identity"
        );
    }

    let first_init = parse(
        Path::new("src/sensor/init_a.go"),
        "package sensor\nfunc init() {}\n",
    );
    let second_init = parse(
        Path::new("src/sensor/init_b.go"),
        "package sensor\nfunc init() {}\n",
    );
    assert_ne!(
        first_init.callable_anchors[0].entity_key,
        second_init.callable_anchors[0].entity_key
    );
    assert_ne!(
        first_init.declarations[0].identity.logical_key,
        second_init.declarations[0].identity.logical_key,
        "each init declaration remains a distinct physical callable"
    );
}

#[test]
fn go_local_declarations_do_not_pollute_package_facts_and_short_vars_are_bound() {
    let index = parse(
        Path::new("scope.go"),
        r#"package scope

func Use(input int) {
    var first, second int
    const localConstant = 1
    type localType int
    if input > 0 {
        shortA, shortB := first, second
        _ = shortA
        _ = shortB
    }
}
"#,
    );

    for local in [
        "first",
        "second",
        "localConstant",
        "localType",
        "shortA",
        "shortB",
    ] {
        assert!(
            index
                .declarations
                .iter()
                .all(|declaration| declaration.name != local),
            "{local} must not become a package declaration"
        );
    }
    for binding in [
        "input",
        "first",
        "second",
        "localConstant",
        "localType",
        "shortA",
        "shortB",
    ] {
        assert!(
            index
                .local_bindings
                .iter()
                .any(|local| local.name == binding),
            "missing local binding {binding}"
        );
    }
    let short = index
        .local_bindings
        .iter()
        .find(|binding| binding.name == "shortA")
        .expect("shortA");
    assert!(
        short.scope_start_byte > index.callable_anchors[0].body_range.unwrap().start_byte,
        "nested-block binding must keep its narrower lexical scope"
    );
}

#[test]
fn go_statement_initializer_bindings_stop_at_the_statement_boundary() {
    let source = r#"package scope

func Use() {
    if ifOnly := ready(); ifOnly {
        _ = ifOnly
    }
    _ = ifOnly

    for forOnly := 0; forOnly < 1; forOnly++ {
        _ = forOnly
    }
    _ = forOnly

    switch switchOnly := value(); switchOnly {
    case 1:
        _ = switchOnly
    }
    _ = switchOnly
}
"#;
    let index = parse(Path::new("statement_scope.go"), source);
    for name in ["ifOnly", "forOnly", "switchOnly"] {
        let binding = index
            .local_bindings
            .iter()
            .find(|binding| binding.name == name)
            .unwrap_or_else(|| panic!("missing {name} binding"));
        let outside_use = source
            .rfind(&format!("_ = {name}"))
            .unwrap_or_else(|| panic!("missing outside use for {name}"));
        assert!(
            binding.scope_end_byte < outside_use,
            "{name} must not remain visible after its owning statement"
        );
    }
}

#[test]
fn go_package_initializers_and_function_literals_keep_distinct_callers() {
    let index = parse(
        Path::new("init.go"),
        r#"package initpkg

var Default = load()

func Use() {
    callback := func() { nested() }
    callback()
}
"#,
    );
    let initializer = index
        .callable_anchors
        .iter()
        .find(|anchor| anchor.kind == crate::call_model::CallableKind::SyntheticGlobalInitializer)
        .expect("package initializer");
    let lambda = index
        .callable_anchors
        .iter()
        .find(|anchor| anchor.kind == crate::call_model::CallableKind::SyntheticLambda)
        .expect("function literal");
    assert!(index.call_sites.iter().any(|call| {
        call.callee_name.as_deref() == Some("load")
            && call.caller_entity_key == initializer.entity_key
    }));
    assert!(index.call_sites.iter().any(|call| {
        call.callee_name.as_deref() == Some("nested") && call.caller_entity_key == lambda.entity_key
    }));
}

#[test]
fn go_struct_members_keep_direct_fields_and_embedded_names_without_promotion() {
    let index = parse(
        Path::new("fields.go"),
        r#"package fields

type Inner struct{}
type Outer struct {
    Inner
    *pkg.Other
    Box[int]
    pkg.Pair[string, int]
    Nested struct { PromotedWrongly int }
    First, Second int
}
"#,
    );
    let outer_key = index
        .records
        .iter()
        .find(|record| record.display_name == "Outer")
        .expect("Outer")
        .record_key
        .clone();
    let names: std::collections::HashSet<_> = index
        .members
        .iter()
        .filter(|member| member.record_key == outer_key)
        .map(|member| member.name.as_str())
        .collect();
    for expected in ["Inner", "Other", "Box", "Pair", "Nested", "First", "Second"] {
        assert!(names.contains(expected), "missing direct field {expected}");
    }
    assert!(!names.contains("PromotedWrongly"));
    assert!(!names.contains("int"));
    assert!(!names.contains("string"));
}

#[test]
fn go_build_guard_scans_the_complete_comment_header_and_survives_fallback() {
    let mut source = String::new();
    for line in 0..96 {
        source.push_str(&format!("// license line {line}\n"));
    }
    source.push_str("//go:build tinygo && arm\n\npackage guarded\n");
    assert_eq!(
        parse(Path::new("guarded.go"), &source)
            .build_guard
            .as_deref(),
        Some("tinygo && arm")
    );

    let broken = parse(
        Path::new("broken_guard.go"),
        "//go:build windows\n\nfunc Broken( {\n",
    );
    assert_eq!(broken.build_guard.as_deref(), Some("windows"));
}

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
    // The declaration keeps this a usable partial AST while the following
    // invalid top-level expression lands in an error region. `stray` must
    // still be emitted as an occurrence, with role Read.
    let src = "int stable;\n1 + stray;\n";
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

    assert!(index.declarations.iter().any(|symbol| {
        symbol.name == "Boom"
            && matches!(
                symbol.declaration_kind,
                SemanticDeclarationKind::Type | SemanticDeclarationKind::Alias
            )
            && symbol.role == SemanticDeclarationRole::Definition
    }));
    assert_eq!(field_containers(&index, "x"), vec!["Boom".to_string()]);
}

#[test]
fn extracts_multiline_typedef_struct_when_member_comments_contain_braces() {
    let index = parse(
        Path::new("b.c"),
        "typedef struct {\n    int x; // comment with }\n    const char *text; /* comment with { */\n} Boom;\n",
    );

    assert!(index.declarations.iter().any(|symbol| {
        symbol.name == "Boom"
            && matches!(
                symbol.declaration_kind,
                SemanticDeclarationKind::Type | SemanticDeclarationKind::Alias
            )
            && symbol.role == SemanticDeclarationRole::Definition
    }));
    assert_eq!(field_containers(&index, "x"), vec!["Boom".to_string()]);
    assert_eq!(field_containers(&index, "text"), vec!["Boom".to_string()]);
}

#[test]
fn trailing_comments_cannot_create_type_symbols() {
    let source = r#"typedef struct AVTextWriter {
    const AVClass *priv_class;      ///< private class of the writer, if any
    int priv_size;                  ///< private size for the writer private class
    const char *name;
} AVTextWriter;
"#;
    let index = parse(Path::new("checkpoint.h"), source);
    let mut types: Vec<_> = index
        .declarations
        .iter()
        .filter(|symbol| {
            matches!(
                symbol.declaration_kind,
                SemanticDeclarationKind::Type | SemanticDeclarationKind::Alias
            )
        })
        .collect();
    types.sort_by_key(|symbol| symbol.name_range.start_byte);

    assert_eq!(
        types
            .iter()
            .map(|symbol| symbol.name.as_str())
            .collect::<Vec<_>>(),
        vec!["AVTextWriter", "AVTextWriter"]
    );
    assert!(types.iter().all(|symbol| {
        source.get(symbol.name_range.start_byte..symbol.name_range.end_byte)
            == Some(symbol.name.as_str())
    }));
    assert_eq!(
        (
            types[0].name_range.start.line,
            types[0].name_range.start.character
        ),
        (0, 15)
    );
    assert_eq!(
        (
            types[1].name_range.start.line,
            types[1].name_range.start.character
        ),
        (4, 2)
    );
    assert!(!index
        .declarations
        .iter()
        .any(|symbol| symbol.name == "const"));
    assert!(!index.declarations.iter().any(|symbol| symbol.name == "of"));
}

#[test]
fn builtin_like_typedef_remains_a_navigable_ast_symbol() {
    let source = "typedef unsigned long size_t;\n";
    let index = parse(Path::new("stddef.h"), source);
    let symbol = index
        .declarations
        .iter()
        .find(|symbol| symbol.name == "size_t")
        .expect("size_t typedef symbol");

    assert!(matches!(
        symbol.declaration_kind,
        SemanticDeclarationKind::Type | SemanticDeclarationKind::Alias
    ));
    assert_eq!(
        source.get(symbol.name_range.start_byte..symbol.name_range.end_byte),
        Some("size_t")
    );
}

#[test]
fn multiline_macro_with_braces_does_not_swallow_following_typedef_struct() {
    let source = r#"#define FREE(ptr)                                                              \
    do                                                                         \
    {                                                                          \
        if ((ptr) != NULL)                                                     \
        {                                                                      \
            free(ptr);                                                         \
            (ptr) = NULL;                                                      \
        }                                                                      \
    } while (0)

typedef struct xxx {
    int value;
} xxx_t;

typedef struct xxxa {
    int other;
} xxxa_t;
"#;
    let index = parse(Path::new("macro_typedef.c"), source);

    let xxx_t = index
        .declarations
        .iter()
        .find(|symbol| {
            symbol.name == "xxx_t"
                && matches!(
                    symbol.declaration_kind,
                    SemanticDeclarationKind::Type | SemanticDeclarationKind::Alias
                )
                && symbol.role == SemanticDeclarationRole::Definition
        })
        .expect("first typedef after multiline macro should be a type definition");
    assert!(
        xxx_t
            .canonical_signature
            .as_deref()
            .is_some_and(|signature| signature.starts_with("typedef struct xxx")),
        "typedef signature should not include the macro body: {:?}",
        xxx_t.canonical_signature
    );
    assert!(!xxx_t
        .canonical_signature
        .as_deref()
        .unwrap_or_default()
        .contains("while (0)"));

    assert!(index.declarations.iter().any(|symbol| {
        symbol.name == "xxxa_t"
            && matches!(
                symbol.declaration_kind,
                SemanticDeclarationKind::Type | SemanticDeclarationKind::Alias
            )
            && symbol.role == SemanticDeclarationRole::Definition
    }));
    assert_eq!(field_containers(&index, "value"), vec!["xxx_t".to_string()]);
    assert_eq!(
        field_containers(&index, "other"),
        vec!["xxxa_t".to_string()]
    );
}

#[test]
fn multiline_macro_with_trailing_space_after_backslash_does_not_swallow_typedef() {
    let source = "#define WRAP(value) \\   \n    do { (value); } while (0)\n\ntypedef struct after_macro {\n    int field;\n} after_macro_t;\n";
    let index = parse(Path::new("macro_spacing.h"), source);

    assert!(index.declarations.iter().any(|symbol| {
        symbol.name == "after_macro_t"
            && matches!(
                symbol.declaration_kind,
                SemanticDeclarationKind::Type | SemanticDeclarationKind::Alias
            )
            && symbol.role == SemanticDeclarationRole::Definition
    }));
    assert_eq!(
        field_containers(&index, "field"),
        vec!["after_macro_t".to_string()]
    );
}

#[test]
fn preprocessor_directives_inside_typedef_struct_body_keep_typedef_statement() {
    let source = r#"typedef struct guarded {
#if defined(CONFIG_X)
    int enabled;
#else
    int disabled;
#endif
} guarded_t;
"#;
    let index = parse(Path::new("guarded_typedef.h"), source);

    assert!(index.declarations.iter().any(|symbol| {
        symbol.name == "guarded_t"
            && matches!(
                symbol.declaration_kind,
                SemanticDeclarationKind::Type | SemanticDeclarationKind::Alias
            )
            && symbol.role == SemanticDeclarationRole::Definition
    }));
    assert_eq!(
        field_containers(&index, "enabled"),
        vec!["guarded_t".to_string()]
    );
    assert_eq!(
        field_containers(&index, "disabled"),
        vec!["guarded_t".to_string()]
    );
}

#[test]
fn multiline_macro_inside_typedef_struct_body_does_not_reset_pending_typedef() {
    let source = r#"typedef struct context {
#define DECL_FIELD(name)                                                       \
    int name
    DECL_FIELD(generated);
    int explicit_field;
} context_t;
"#;
    let index = parse(Path::new("macro_in_record.h"), source);

    assert!(index.declarations.iter().any(|symbol| {
        symbol.name == "context_t"
            && matches!(
                symbol.declaration_kind,
                SemanticDeclarationKind::Type | SemanticDeclarationKind::Alias
            )
            && symbol.role == SemanticDeclarationRole::Definition
    }));
    assert_eq!(
        field_containers(&index, "explicit_field"),
        vec!["context_t".to_string()]
    );
}

#[test]
fn field_members_capture_record_type_name() {
    let index = parse(
        Path::new("nested.c"),
        "struct Inner { int value; };\ntypedef struct Inner Inner;\nstruct Outer { struct Inner mem1; Inner *mem2; const struct Inner *mem3; int count; };\n",
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

    let mem3 = index
        .members
        .iter()
        .find(|member| member.name == "mem3")
        .expect("mem3");
    assert_eq!(mem3.type_name.as_deref(), Some("Inner"));

    let count = index
        .members
        .iter()
        .find(|member| member.name == "count")
        .expect("count");
    assert_eq!(count.type_name, None);
}

#[test]
fn nested_anonymous_record_members_get_synthetic_type_names() {
    let source = "typedef struct { struct { int xxx; } mem1[4]; union { int tag; } u; } A;\n";
    let index = parse(Path::new("nested.c"), source);

    let mem1 = index
        .members
        .iter()
        .find(|member| member.name == "mem1")
        .expect("mem1");
    assert_eq!(mem1.type_name.as_deref(), Some("A.mem1"));
    assert!(index
        .records
        .iter()
        .any(|record| record.display_name == "A.mem1"
            && record.confidence == super::RecordConfidence::Heuristic));
    let nested = index
        .records
        .iter()
        .find(|record| record.display_name == "A.mem1")
        .expect("synthetic nested record");
    assert_eq!(
        &source[nested.declaration_range.start_byte..nested.declaration_range.end_byte],
        "struct { int xxx; } mem1[4];"
    );
    assert_eq!(nested.range_fidelity, super::RecordRangeFidelity::AstExact);
    assert_eq!(field_containers(&index, "xxx"), vec!["A.mem1".to_string()]);

    let u = index
        .members
        .iter()
        .find(|member| member.name == "u")
        .expect("u");
    assert_eq!(u.type_name.as_deref(), Some("A.u"));
    assert_eq!(field_containers(&index, "tag"), vec!["A.u".to_string()]);
}

#[test]
fn function_pointer_fields_are_fields_not_methods() {
    let index = parse(
        Path::new("callbacks.c"),
        "struct Callbacks { int (*on_value)(int value); void run(void); };\n",
    );

    let cb = index
        .members
        .iter()
        .find(|member| member.name == "on_value")
        .expect("on_value");
    assert_eq!(cb.kind, MemberKind::Field);

    let run = index
        .members
        .iter()
        .find(|member| member.name == "run")
        .expect("run");
    assert_eq!(run.kind, MemberKind::Method);
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
    assert!(index.declarations.iter().any(|symbol| {
        symbol.name == "ANSWER" && symbol.declaration_kind == SemanticDeclarationKind::Macro
    }));
    assert!(index.declarations.iter().any(|symbol| {
        symbol.name == "hello_value" && symbol.role == SemanticDeclarationRole::Declaration
    }));
    assert!(index.declarations.iter().any(|symbol| {
        symbol.name == "main" && symbol.role == SemanticDeclarationRole::Definition
    }));
}

#[test]
fn c_file_scope_objects_distinguish_declaration_tentative_and_full_definition() {
    let source = "extern int declared;\n\
                  int tentative;\n\
                  static int internal_tentative;\n\
                  int full = 1;\n\
                  extern int extern_full = 2;\n\
                  int uncertain = };\n";
    let index = parse(Path::new("objects.c"), source);

    for (name, expected_role) in [
        ("declared", SemanticDeclarationRole::Declaration),
        ("tentative", SemanticDeclarationRole::TentativeDefinition),
        (
            "internal_tentative",
            SemanticDeclarationRole::TentativeDefinition,
        ),
        ("full", SemanticDeclarationRole::Definition),
        ("extern_full", SemanticDeclarationRole::Definition),
    ] {
        let symbol = index
            .declarations
            .iter()
            .find(|symbol| {
                symbol.name == name && symbol.declaration_kind == SemanticDeclarationKind::Object
            })
            .unwrap_or_else(|| panic!("missing object symbol {name}"));
        assert_eq!(symbol.role, expected_role, "unexpected role for {name}");
    }
    assert!(!index
        .declarations
        .iter()
        .any(|fact| fact.name == "uncertain"));
}

#[test]
fn cpp_file_scope_object_without_initializer_is_a_full_definition() {
    let source = "extern int declared;\nint full;\nstatic int internal_full;\n";
    let index = parse(Path::new("objects.cpp"), source);

    let role = |name: &str| {
        index
            .declarations
            .iter()
            .find(|symbol| {
                symbol.name == name && symbol.declaration_kind == SemanticDeclarationKind::Object
            })
            .map(|symbol| symbol.role)
            .unwrap_or_else(|| panic!("missing object symbol {name}"))
    };

    assert_eq!(role("declared"), SemanticDeclarationRole::Declaration);
    assert_eq!(role("full"), SemanticDeclarationRole::Definition);
    assert_eq!(role("internal_full"), SemanticDeclarationRole::Definition);
}

#[test]
fn usable_ast_keeps_initializer_calls_out_of_function_declarations() {
    let source = "int value = make(1);\n";
    let index = parse(Path::new("objects.c"), source);

    assert!(!index.diagnostics.fallback_used);
    assert_eq!(index.diagnostics.ast_source, super::FactSource::Ast);
    assert!(index
        .occurrences
        .iter()
        .any(|occ| occ.name == "value" && occ.role == SyntacticRole::Declaration));
    assert!(index
        .call_sites
        .iter()
        .any(|call| call.callee_name.as_deref() == Some("make")));
    assert!(
        index
            .callable_anchors
            .iter()
            .all(|anchor| anchor.name != "make" && anchor.name != "value"),
        "initializer expressions and objects must not become callable anchors"
    );
}

#[test]
fn usable_cpp_ast_keeps_constructor_style_object_out_of_function_declarations() {
    let source = "struct Widget { Widget(int); };\nWidget widget(42);\n";
    let index = parse(Path::new("objects.cpp"), source);

    assert!(!index.diagnostics.fallback_used);
    assert_eq!(index.diagnostics.ast_source, super::FactSource::Ast);
    assert!(index
        .occurrences
        .iter()
        .any(|occ| occ.name == "widget" && occ.role == SyntacticRole::Declaration));
    assert!(
        index
            .callable_anchors
            .iter()
            .all(|anchor| anchor.name != "widget"),
        "constructor-style object initialization must not become a callable anchor"
    );
}

#[test]
fn usable_ast_keeps_function_pointer_declarator_out_of_function_declarations() {
    let source = "int (*handler)(int);\n";
    let index = parse(Path::new("callbacks.c"), source);

    assert!(!index.diagnostics.fallback_used);
    assert_eq!(index.diagnostics.ast_source, super::FactSource::Ast);
    assert!(
        index
            .callable_anchors
            .iter()
            .all(|anchor| anchor.name != "handler" && anchor.name != "int"),
        "function pointer declarators must not become callable anchors"
    );
}

#[test]
fn usable_ast_keeps_top_level_macro_invocation_out_of_function_declarations() {
    let source = "REGISTER(foo);\nint real(void);\n";
    let index = parse(Path::new("macros.c"), source);

    assert!(!index.diagnostics.fallback_used);
    assert_eq!(index.diagnostics.ast_source, super::FactSource::Ast);
    assert!(index
        .callable_anchors
        .iter()
        .any(|anchor| anchor.name == "real"));
    assert!(
        index
            .callable_anchors
            .iter()
            .all(|anchor| anchor.name != "REGISTER"),
        "top-level macro invocations must not become callable anchors"
    );
}

#[test]
fn parser_semantic_candidate_fixtures_cover_required_declaration_shapes() {
    let c = include_str!("fixtures/semantic_candidates.c");
    let cpp = include_str!("fixtures/semantic_candidates.cpp");
    let header = include_str!("fixtures/semantic_candidates.h");
    let inl = include_str!("fixtures/semantic_candidates.inl");

    for (path, source) in [
        ("semantic_candidates.c", c),
        ("semantic_candidates.cpp", cpp),
        ("semantic_candidates.h", header),
        ("semantic_candidates.inl", inl),
    ] {
        let index = parse(Path::new(path), source);
        assert!(
            !index.diagnostics.fallback_used,
            "{path} should produce a usable syntax tree"
        );
        assert_eq!(index.diagnostics.ast_source, super::FactSource::Ast);
    }

    let c_index = parse(Path::new("semantic_candidates.c"), c);
    assert!(c_index.diagnostics.parse_error_count > 0);
    assert!(has_symbol(
        &c_index,
        "c_defined_function",
        SymbolKind::Function
    ));
    assert!(has_symbol(
        &c_index,
        "c_internal_object",
        SymbolKind::GlobalVariable
    ));
    assert!(c.contains("int c_first_object, c_second_object = 2;"));
    assert!(c.contains("int (*c_handler)(int);"));
    assert!(c.contains("REGISTER_CANDIDATE(alpha);"));
    assert!(c.contains("int c_malformed_object = };"));

    let cpp_index = parse(Path::new("semantic_candidates.cpp"), cpp);
    assert!(cpp_index
        .members
        .iter()
        .any(|member| member.name == "method"));
    assert!(cpp.contains("template <typename T>"));
    assert!(cpp.contains("demo::Widget cpp_global_widget(42);"));
    assert!(cpp.contains("Widget Widget::operator+"));

    let header_index = parse(Path::new("semantic_candidates.h"), header);
    assert!(has_symbol(
        &header_index,
        "guarded_api",
        SymbolKind::Function
    ));
    assert!(header_index
        .aliases
        .iter()
        .any(|alias| alias.alias == "CandidateRecordPtr"));
    assert!(header.contains("#ifndef SEMANTIC_CANDIDATES_H"));
    assert!(header.contains("#ifdef ENABLE_OPTIONAL_CANDIDATE"));

    let inl_index = parse(Path::new("semantic_candidates.inl"), inl);
    assert!(has_symbol(
        &inl_index,
        "qualified_candidate",
        SymbolKind::Function
    ));
    assert!(inl.contains("template <typename T>"));
    assert!(inl.contains("inline int Widget::method"));
    assert!(inl.contains("demo::clamp_candidate(3, 1, 5)"));
}

#[test]
fn leading_comments_do_not_pollute_symbol_signature_or_start_line() {
    let source = "#define VALUE 1\n/// @brief Helps the smoke test.\nvoid helper(void);\n";
    let index = parse(Path::new("defs.h"), source);
    let symbol = index
        .declarations
        .iter()
        .find(|symbol| symbol.name == "helper")
        .expect("helper symbol");

    assert_eq!(symbol.name_range.start.line, 2);
    assert_eq!(
        symbol.canonical_signature.as_deref(),
        Some("void helper(void)")
    );
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
        .declarations
        .iter()
        .find(|symbol| symbol.name == "guarded")
        .expect("guarded symbol");

    assert_eq!(symbol.guard.as_deref(), Some("#ifdef CONFIG_X"));
}

#[test]
fn parse_reports_ast_provenance_on_clean_file() {
    // A syntactically valid file has canonical AST declarations and no lexical
    // completion fallback.
    let index = parse(
        Path::new("a.c"),
        "#define M 1\nstruct S { int x; };\ntypedef struct S St;\nvoid f(void) { struct S s; }\n",
    );
    let d = index.diagnostics;
    assert!(!d.fallback_used);
    assert_eq!(d.lexical_source, super::FactSource::Lexical);
    assert_eq!(d.ast_source, super::FactSource::Ast);
    assert_eq!(
        index.parse_outcome,
        crate::semantic_model::ParseOutcome::Ast
    );
    assert!(index.fallback_completions.is_empty());
    assert!(index.declarations.iter().all(|fact| {
        fact.identity.provenance == crate::semantic_model::SemanticFactProvenance::Ast
    }));
    // Macro, record, alias, and local facts all come from the one AST product.
    assert!(index.declarations.iter().any(|s| s.name == "M"));
    assert!(index.records.iter().any(|r| r.display_name == "S"));
    assert!(!index.occurrences.is_empty());
    assert!(index.aliases.iter().any(|a| a.alias == "St"));
    assert!(index
        .local_declarations
        .iter()
        .any(|l| l.name == "s" && l.record_type == "S"));
}

#[test]
fn header_and_inl_declaration_metadata_use_the_resolved_cpp_language() {
    for path in ["api.h", "api.inl"] {
        let index = parse(Path::new(path), "class Widget {};\nint api(int value);\n");
        assert!(!index.declarations.is_empty(), "{path}");
        assert!(index.declarations.iter().all(|declaration| {
            declaration.identity.language == crate::semantic_model::SemanticLanguage::Cpp
        }));
    }

    let overridden = super::parse_with_language(
        Path::new("legacy.h"),
        "int legacy_api(int value);\n",
        crate::config::SourceLanguage::C,
        super::ParseFacts::ALL,
    );
    assert!(overridden.declarations.iter().all(|declaration| {
        declaration.identity.language == crate::semantic_model::SemanticLanguage::C
    }));
}

#[test]
fn cpp_using_aliases_are_canonical_ast_declarations() {
    let source = "struct Widget {};\nusing WidgetAlias = Widget;\n";
    let index = parse(Path::new("aliases.h"), source);
    let declaration = index
        .declarations
        .iter()
        .find(|declaration| declaration.name == "WidgetAlias")
        .expect("using alias declaration");
    assert_eq!(
        declaration.declaration_kind,
        crate::semantic_model::SemanticDeclarationKind::Alias
    );
    assert_eq!(
        declaration.identity.provenance,
        crate::semantic_model::SemanticFactProvenance::Ast
    );
    assert_eq!(
        &source[declaration.name_range.start_byte..declaration.name_range.end_byte],
        "WidgetAlias"
    );
    assert!(index.aliases.iter().any(|alias| {
        alias.alias == "WidgetAlias"
            && alias.target
                == crate::semantic_model::AliasTarget::UnresolvedTypeName("Widget".to_string())
    }));
}

#[test]
fn partial_ast_keeps_ast_declarations_without_fallback_completions() {
    // A stray token yields an error-laden but still usable tree. That is NOT
    // the lexical-fallback path: declarations still come exclusively from the
    // usable AST, and the error count is non-zero.
    let index = parse(Path::new("b.c"), "#define OK 1\n@\n");
    assert!(!index.diagnostics.fallback_used);
    assert_eq!(index.diagnostics.ast_source, super::FactSource::Ast);
    assert!(index.diagnostics.parse_error_count > 0);
    assert_eq!(
        index.parse_outcome,
        crate::semantic_model::ParseOutcome::PartialAst
    );
    assert!(index.fallback_completions.is_empty());
    assert!(index.declarations.iter().all(|fact| {
        fact.identity.provenance == crate::semantic_model::SemanticFactProvenance::Ast
    }));
    assert!(index.declarations.iter().any(|s| s.name == "OK"));
}

#[test]
fn partial_ast_marks_only_the_overlapping_declaration_incomplete() {
    let index = parse(Path::new("partial.c"), "int broken = ;\nint healthy;\n");
    assert_eq!(
        index.parse_outcome,
        crate::semantic_model::ParseOutcome::PartialAst
    );
    assert!(index.fallback_completions.is_empty());
    let broken = index
        .declarations
        .iter()
        .find(|declaration| declaration.name == "broken")
        .expect("broken declaration");
    let healthy = index
        .declarations
        .iter()
        .find(|declaration| declaration.name == "healthy")
        .expect("healthy declaration");
    assert_eq!(
        broken.identity.fact_fidelity,
        crate::semantic_model::SemanticFactFidelity::Incomplete
    );
    assert_eq!(
        healthy.identity.fact_fidelity,
        crate::semantic_model::SemanticFactFidelity::Authoritative
    );
}

#[test]
fn comments_do_not_mask_an_otherwise_hard_ast_failure() {
    let index = parse(Path::new("broken.c"), "// still a valid comment\n@\n");
    assert_eq!(
        index.parse_outcome,
        crate::semantic_model::ParseOutcome::LexicalFallback
    );
    assert!(index.declarations.is_empty());
}

#[test]
fn comments_only_remain_a_clean_ast_product() {
    let index = parse(Path::new("comments.c"), "// no declarations here\n");
    assert_eq!(
        index.parse_outcome,
        crate::semantic_model::ParseOutcome::Ast
    );
    assert!(index.declarations.is_empty());
    assert!(index.fallback_completions.is_empty());
}

#[test]
fn wholly_invalid_translation_unit_uses_completion_only_fallback() {
    let source = "((( guessed(value);\n";
    let index = parse(Path::new("broken.c"), source);
    assert_eq!(
        index.parse_outcome,
        crate::semantic_model::ParseOutcome::LexicalFallback
    );
    assert!(index.declarations.is_empty());
    assert!(index
        .fallback_completions
        .iter()
        .any(|completion| completion.name == "guessed"));
}

#[test]
fn lexical_fallback_product_has_completion_hints_and_no_ast() {
    // The fallback product (returned when tree-sitter yields no usable tree)
    // keeps include facts and isolated completion hints, empties all AST facts,
    // and is distinguishable from a clean parse by its outcome/provenance.
    let source = "#include \"x.h\"\n#define Z 9\n";
    let includes = super::scan_includes(source);
    let index = super::lexical_fallback(
        source,
        includes,
        ParseFacts::ALL,
        crate::config::SourceLanguage::C,
    );
    assert!(index.diagnostics.fallback_used);
    assert_eq!(
        index.parse_outcome,
        crate::semantic_model::ParseOutcome::LexicalFallback
    );
    assert_eq!(
        index.diagnostics.ast_source,
        super::FactSource::LexicalFallback
    );
    assert_eq!(index.diagnostics.lexical_source, super::FactSource::Lexical);
    assert_eq!(index.diagnostics.parse_error_count, 0);
    assert!(index.fallback_completions.iter().any(|s| s.name == "Z"));
    assert!(index.declarations.is_empty());
    assert_eq!(index.includes.len(), 1);
    assert!(index.occurrences.is_empty());
    assert!(index.records.is_empty());
    assert!(index.local_declarations.is_empty());
}

#[test]
fn cancelled_parse_returns_none_without_synthesizing_fallback() {
    let cancel = std::sync::atomic::AtomicBool::new(true);
    let result = super::parse_with_handle_control(
        Path::new("cancelled.h"),
        "int should_not_be_cached(void);\n",
        crate::config::SourceLanguage::Cpp,
        None,
        ParseFacts::ALL,
        Some(&cancel),
    );
    assert!(result.is_none());
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

fn fact_mask_source() -> &'static str {
    r#"#include "api.h"
#define FLAG 1
enum Color { RED };
struct Widget { int width; void resize(); static int count(); };
typedef Widget WidgetAlias;
int use(Widget *w, int count) {
    int local_value = count;
    w->width = local_value;
    return FLAG + RED;
}
"#
}

fn has_symbol(index: &FileSemanticIndex, name: &str, kind: SymbolKind) -> bool {
    let expected = match kind {
        SymbolKind::Function => SemanticDeclarationKind::Function,
        SymbolKind::Macro => SemanticDeclarationKind::Macro,
        SymbolKind::Type => SemanticDeclarationKind::Type,
        SymbolKind::EnumConstant => SemanticDeclarationKind::EnumConstant,
        SymbolKind::GlobalVariable => SemanticDeclarationKind::Object,
        SymbolKind::Field => return false,
    };
    index.declarations.iter().any(|symbol| {
        symbol.name == name
            && (symbol.declaration_kind == expected
                || (kind == SymbolKind::Type
                    && symbol.declaration_kind == SemanticDeclarationKind::Alias))
    })
}

#[test]
fn parse_fact_masks_document_current_field_contents() {
    let path = Path::new("facts.cpp");

    let index = parse_with_handle(path, fact_mask_source(), None, ParseFacts::INDEX);
    let persistent = index.persistent_facts();
    assert_eq!(persistent.includes.len(), index.includes.len());
    assert_eq!(persistent.declarations.len(), index.declarations.len());
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
    assert!(index
        .declarations
        .iter()
        .any(|declaration| declaration.name == "use"));
    assert!(index.occurrences.is_empty());
    assert!(index.local_declarations.is_empty());
    assert!(index.local_bindings.is_empty());
    assert_eq!(index.diagnostics.ast_source, super::FactSource::Ast);
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
    assert!(color_ref.callable_anchors.is_empty());
    assert!(color_ref.local_declarations.is_empty());
    assert!(color_ref.local_bindings.is_empty());
    assert_eq!(
        color_ref.fact_availability(FactGroup::Occurrences),
        FactAvailability::Available
    );
    assert_eq!(
        color_ref.fact_availability(FactGroup::Aliases),
        FactAvailability::NotRequested
    );
    assert_eq!(
        color_ref.fact_availability(FactGroup::CallableAnchors),
        FactAvailability::NotRequested
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
fn call_facts_capture_free_function_anchors_and_direct_calls() {
    let source = r#"
static int helper(int value) { return value; }
int caller(void) { return helper(7); }
"#;
    let index = parse(std::path::Path::new("src/main.c"), source);
    let helper = index
        .callable_anchors
        .iter()
        .find(|anchor| anchor.name == "helper")
        .expect("helper anchor");
    assert_eq!(helper.role, crate::call_model::AnchorRole::Definition);
    assert!(matches!(
        helper.linkage,
        crate::call_model::LinkageDomain::Internal(_)
    ));
    assert_eq!(helper.signature.min_arity, Some(1));
    assert_eq!(helper.signature.max_arity, Some(1));

    let call = index
        .call_sites
        .iter()
        .find(|call| call.callee_name.as_deref() == Some("helper"))
        .expect("helper call");
    assert_eq!(call.form, crate::call_model::CallForm::DirectName);
    assert_eq!(call.argument_count, Some(1));
    assert_eq!(call.caller_entity_key, index.callable_anchors[1].entity_key);
}

#[test]
fn callable_anchors_project_canonical_function_declaration_facts() {
    let source = r#"
static int helper(int value) { return value; }
int caller(void) { return helper(7); }
"#;
    let index = parse(std::path::Path::new("src/main.c"), source);
    let helper_anchor = index
        .callable_anchors
        .iter()
        .find(|anchor| anchor.name == "helper")
        .expect("helper anchor");
    let helper_fact = index
        .declarations
        .iter()
        .find(|declaration| declaration.name == "helper")
        .expect("helper declaration fact");

    assert_eq!(
        helper_fact.declaration_kind,
        crate::semantic_model::SemanticDeclarationKind::Function
    );
    assert_eq!(
        helper_fact.role,
        crate::semantic_model::SemanticDeclarationRole::Definition
    );
    assert_eq!(helper_fact.owner, None);
    assert_eq!(helper_fact.path, "src/main.c");
    assert_eq!(helper_fact.name_range, helper_anchor.name_range);
    assert_eq!(
        helper_fact.declaration_range,
        helper_anchor.declaration_range
    );
    assert_eq!(
        helper_fact.canonical_signature.as_deref(),
        Some(helper_anchor.canonical_signature.as_str())
    );
    assert!(matches!(
        helper_fact.linkage,
        crate::call_model::LinkageDomain::Internal(_)
    ));
    assert_eq!(
        helper_fact.identity.locator.fingerprint,
        helper_anchor.anchor_fingerprint
    );
    assert_eq!(
        helper_fact.identity.logical_key.qualified_name,
        helper_anchor.qualified_name
    );
    assert_eq!(
        helper_fact.identity.language,
        crate::semantic_model::SemanticLanguage::C
    );
    assert_eq!(
        helper_fact.identity.language_fidelity,
        crate::semantic_model::LanguageFidelity::Explicit
    );
    assert_eq!(
        helper_fact.identity.provenance,
        crate::semantic_model::SemanticFactProvenance::Ast
    );
    assert_eq!(
        helper_fact.identity.fact_fidelity,
        crate::semantic_model::SemanticFactFidelity::Authoritative
    );
}

#[test]
fn callable_anchors_project_canonical_method_declaration_facts() {
    let source = r#"
struct Worker {
  int run(void) { return 1; }
};
"#;
    let index = parse(std::path::Path::new("src/main.cpp"), source);
    let method_fact = index
        .declarations
        .iter()
        .find(|declaration| declaration.name == "run")
        .expect("method declaration fact");

    assert_eq!(method_fact.qualified_name, "Worker::run");
    assert_eq!(method_fact.owner.as_deref(), Some("Worker"));
    assert_eq!(
        method_fact.declaration_kind,
        crate::semantic_model::SemanticDeclarationKind::Method
    );
    assert_eq!(
        method_fact.role,
        crate::semantic_model::SemanticDeclarationRole::Definition
    );
    assert_eq!(
        method_fact.identity.language,
        crate::semantic_model::SemanticLanguage::Cpp
    );
    assert_eq!(
        method_fact.identity.logical_key.owner.as_deref(),
        Some("Worker")
    );
    assert_eq!(
        method_fact.identity.logical_key.declaration_kind,
        crate::semantic_model::SemanticDeclarationKind::Method
    );
}

#[test]
fn ast_projects_file_scope_object_declaration_facts() {
    let source = r#"
extern int declared_object;
int first_object, second_object = 2;
static int internal_object;
int (*handler)(int);
"#;
    let index = parse(std::path::Path::new("src/objects.c"), source);
    let object = |name: &str| {
        index
            .declarations
            .iter()
            .find(|declaration| declaration.name == name)
            .unwrap_or_else(|| panic!("missing object declaration fact {name}"))
    };

    let declared = object("declared_object");
    assert_eq!(
        declared.declaration_kind,
        crate::semantic_model::SemanticDeclarationKind::Object
    );
    assert_eq!(
        declared.role,
        crate::semantic_model::SemanticDeclarationRole::Declaration
    );
    assert_eq!(declared.has_initializer, Some(false));

    let first = object("first_object");
    let second = object("second_object");
    assert_eq!(
        first.role,
        crate::semantic_model::SemanticDeclarationRole::TentativeDefinition
    );
    assert_eq!(
        second.role,
        crate::semantic_model::SemanticDeclarationRole::Definition
    );
    assert_eq!(first.has_initializer, Some(false));
    assert_eq!(second.has_initializer, Some(true));
    assert_ne!(
        first.identity.locator.fingerprint,
        second.identity.locator.fingerprint
    );

    let internal = object("internal_object");
    assert!(matches!(
        internal.linkage,
        crate::call_model::LinkageDomain::Internal(_)
    ));

    let handler = object("handler");
    assert!(matches!(
        handler.declarator_shape,
        Some(super::DeclaratorShape::FunctionPointer { .. })
    ));
    assert_eq!(
        handler.identity.language,
        crate::semantic_model::SemanticLanguage::C
    );
    assert_eq!(
        handler.identity.provenance,
        crate::semantic_model::SemanticFactProvenance::Ast
    );
}

#[test]
fn ast_projects_namespace_scope_cpp_object_declaration_facts() {
    let source = r#"
namespace demo {
struct Widget { explicit Widget(int); };
Widget widget(42);
int first, second = 2;
}
"#;
    let index = parse(std::path::Path::new("src/objects.cpp"), source);
    let widget = index
        .declarations
        .iter()
        .find(|declaration| declaration.name == "widget")
        .expect("widget object fact");
    assert_eq!(widget.qualified_name, "demo::widget");
    assert_eq!(widget.owner.as_deref(), Some("demo"));
    assert_eq!(
        widget.role,
        crate::semantic_model::SemanticDeclarationRole::Definition
    );
    assert_eq!(widget.has_initializer, Some(true));
    assert_eq!(
        widget.identity.language,
        crate::semantic_model::SemanticLanguage::Cpp
    );

    let second = index
        .declarations
        .iter()
        .find(|declaration| declaration.name == "second")
        .expect("second object fact");
    assert_eq!(second.qualified_name, "demo::second");
    assert_eq!(second.has_initializer, Some(true));
}

#[test]
fn call_facts_capture_namespace_qualified_free_calls() {
    let source = r#"
namespace net { int open(int port) { return port; } }
int start(void) { return net :: open(80); }
"#;
    let index = parse(std::path::Path::new("src/main.cpp"), source);
    let open = index
        .callable_anchors
        .iter()
        .find(|anchor| anchor.name == "open")
        .expect("namespace function");
    assert_eq!(open.qualified_name, "net::open");
    assert_eq!(
        open.owner_kind,
        Some(crate::call_model::OwnerKindHint::Namespace)
    );
    let call = index
        .call_sites
        .iter()
        .find(|call| call.callee_name.as_deref() == Some("open"))
        .expect("qualified call");
    assert_eq!(call.form, crate::call_model::CallForm::QualifiedName);
    assert_eq!(call.qualified_name.as_deref(), Some("net::open"));
}

#[test]
fn out_of_namespace_qualified_definition_keeps_unknown_owner_evidence() {
    let source = r#"
namespace net { int open(int port); }
int net::open(int port) { return port; }
"#;
    let index = parse(std::path::Path::new("src/main.cpp"), source);
    let outside_definition = index
        .callable_anchors
        .iter()
        .find(|anchor| {
            anchor.name == "open" && anchor.role == crate::call_model::AnchorRole::Definition
        })
        .expect("out-of-namespace definition");

    assert_eq!(outside_definition.qualified_name, "net::open");
    assert_eq!(outside_definition.owner.as_deref(), Some("net"));
    assert_eq!(
        outside_definition.owner_kind,
        Some(crate::call_model::OwnerKindHint::Unknown),
        "without compiler binding, an explicit owner outside its namespace is conservative"
    );
}

#[test]
fn malformed_call_descendants_disable_arity_evidence() {
    let source = r#"
int pick(int left, int right);
int trailing(void) { return pick(1,); }
int missing(void) { return pick(1; }
"#;
    let index = parse(std::path::Path::new("src/main.c"), source);
    let calls: Vec<_> = index
        .call_sites
        .iter()
        .filter(|call| call.callee_name.as_deref() == Some("pick"))
        .collect();

    assert_eq!(
        calls.len(),
        2,
        "both malformed calls remain best-effort facts"
    );
    assert!(calls.iter().all(|call| call.syntax_error_overlap));
}

#[test]
fn call_facts_label_record_methods_and_member_calls_without_binding_them() {
    let source = r#"
struct Worker {
  int run(void) { return helper(); }
};
int invoke(Worker *worker) { return worker->run(); }
"#;
    let index = parse(std::path::Path::new("src/main.cpp"), source);
    let method = index
        .callable_anchors
        .iter()
        .find(|anchor| anchor.name == "run")
        .expect("method anchor");
    assert_eq!(
        method.owner_kind,
        Some(crate::call_model::OwnerKindHint::Record)
    );
    let member_call = index
        .call_sites
        .iter()
        .find(|call| call.callee_name.as_deref() == Some("run"))
        .expect("member call fact");
    assert_eq!(member_call.form, crate::call_model::CallForm::MemberArrow);
}

#[test]
fn call_facts_keep_indirect_and_global_initialization_explicit() {
    let source = r#"
int target(void);
int (*fp)(void) = target;
int initialized = target();
int caller(void) { return (*fp)(); }
"#;
    let index = parse(std::path::Path::new("src/main.c"), source);
    assert!(index.callable_anchors.iter().any(|anchor| {
        anchor.kind == crate::call_model::CallableKind::SyntheticGlobalInitializer
    }));
    assert!(
        !index
            .declarations
            .iter()
            .any(|declaration| declaration.name == "<global initialization>"),
        "synthetic call-relation scopes are not canonical declarations"
    );
    assert!(index.declarations.iter().all(|declaration| {
        declaration.identity.provenance == crate::semantic_model::SemanticFactProvenance::Ast
    }));
    assert!(index
        .call_sites
        .iter()
        .any(|call| call.form == crate::call_model::CallForm::FunctionPointer));
}

#[test]
fn external_call_declaration_retention_updates_canonical_declaration_facts() {
    let mut index = parse(
        std::path::Path::new("C:/sdk/api.h"),
        "int sdk_open(int port) { return port; }\n",
    );
    index.retain_external_call_declarations();

    let anchor = index
        .callable_anchors
        .iter()
        .find(|anchor| anchor.name == "sdk_open")
        .expect("callable anchor");
    let fact = index
        .declarations
        .iter()
        .find(|declaration| declaration.name == "sdk_open")
        .expect("declaration fact");

    assert_eq!(anchor.role, crate::call_model::AnchorRole::Declaration);
    assert_eq!(
        fact.role,
        crate::semantic_model::SemanticDeclarationRole::Declaration
    );
    assert_eq!(
        fact.identity.role,
        crate::semantic_model::SemanticDeclarationRole::Declaration
    );
    assert!(index.call_sites.is_empty());
}

#[test]
fn out_of_class_method_definition_projects_as_same_method_identity() {
    let source = r#"
struct Worker {
  int run(int value);
};
int Worker::run(int value) { return value; }
"#;
    let index = parse(std::path::Path::new("src/worker.cpp"), source);
    let method_anchors: Vec<_> = index
        .callable_anchors
        .iter()
        .filter(|anchor| anchor.name == "run")
        .collect();
    assert_eq!(method_anchors.len(), 2);
    assert!(method_anchors
        .iter()
        .all(|anchor| { anchor.owner_kind == Some(crate::call_model::OwnerKindHint::Record) }));
    assert_eq!(method_anchors[0].entity_key, method_anchors[1].entity_key);

    let method_facts: Vec<_> = index
        .declarations
        .iter()
        .filter(|declaration| declaration.name == "run")
        .collect();
    assert_eq!(method_facts.len(), 2);
    assert!(method_facts.iter().all(|fact| {
        fact.declaration_kind == crate::semantic_model::SemanticDeclarationKind::Method
            && fact.identity.logical_key.declaration_kind
                == crate::semantic_model::SemanticDeclarationKind::Method
    }));
    assert_eq!(
        method_facts[0].identity.logical_key,
        method_facts[1].identity.logical_key
    );
}

#[test]
fn object_logical_identity_ignores_extern_initializer_and_neighbor_declarators() {
    let declaration = parse(
        std::path::Path::new("include/api.h"),
        "extern int shared_value;\n",
    );
    let definition = parse(std::path::Path::new("src/api.c"), "int shared_value = 1;\n");
    let declaration = declaration
        .declarations
        .iter()
        .find(|declaration| declaration.name == "shared_value")
        .expect("object declaration");
    let definition = definition
        .declarations
        .iter()
        .find(|declaration| declaration.name == "shared_value")
        .expect("object definition");
    assert_eq!(
        declaration.identity.logical_key.canonical_signature,
        Some("int shared_value".to_string())
    );
    assert_eq!(
        declaration.identity.logical_key.canonical_signature,
        definition.identity.logical_key.canonical_signature
    );

    let multiple = parse(
        std::path::Path::new("src/multiple.c"),
        "int first, second = shared_value;\n",
    );
    let first = multiple
        .declarations
        .iter()
        .find(|declaration| declaration.name == "first")
        .expect("first object");
    let second = multiple
        .declarations
        .iter()
        .find(|declaration| declaration.name == "second")
        .expect("second object");
    assert_eq!(
        first.identity.logical_key.canonical_signature,
        Some("int first".to_string())
    );
    assert_eq!(
        second.identity.logical_key.canonical_signature,
        Some("int second".to_string())
    );
}

#[test]
fn cpp_namespace_scope_const_object_has_internal_linkage_by_default() {
    let index = parse(
        std::path::Path::new("src/constants.cpp"),
        "const int file_local = 1;\nextern const int shared_constant;\n",
    );
    let file_local = index
        .declarations
        .iter()
        .find(|declaration| declaration.name == "file_local")
        .expect("file-local const");
    let shared = index
        .declarations
        .iter()
        .find(|declaration| declaration.name == "shared_constant")
        .expect("extern const");

    assert!(matches!(
        file_local.linkage,
        crate::call_model::LinkageDomain::Internal(_)
    ));
    assert!(matches!(
        shared.linkage,
        crate::call_model::LinkageDomain::External
    ));
}

#[test]
fn inl_uses_cpp_language_for_parser_and_canonical_facts() {
    let source = r#"
namespace demo {
struct Widget { int method(int value); };
}
inline int demo::Widget::method(int value) { return value; }
"#;
    let index = parse(std::path::Path::new("src/widget.inl"), source);
    assert!(!index.diagnostics.fallback_used);
    assert_eq!(index.diagnostics.parse_error_count, 0);
    assert!(!index.declarations.is_empty());
    assert!(index.declarations.iter().all(|declaration| {
        declaration.identity.language == crate::semantic_model::SemanticLanguage::Cpp
    }));
    assert!(index.callable_anchors.iter().any(|anchor| {
        anchor.name == "method"
            && anchor.owner_kind == Some(crate::call_model::OwnerKindHint::Record)
    }));
}

#[test]
fn canonical_declarations_cover_all_public_semantic_kinds_with_backing() {
    use crate::semantic_model::{DeclarationBacking, SemanticDeclarationKind};

    let source = "#define LIMIT 4\n\
                  struct Widget { void run(); int value; };\n\
                  typedef Widget WidgetAlias;\n\
                  enum Mode { Fast };\n\
                  int global_value;\n\
                  void free_fn(void) {}\n";
    let index = parse(std::path::Path::new("model.cpp"), source);
    for (name, kind) in [
        ("LIMIT", SemanticDeclarationKind::Macro),
        ("Widget", SemanticDeclarationKind::Type),
        ("WidgetAlias", SemanticDeclarationKind::Alias),
        ("Fast", SemanticDeclarationKind::EnumConstant),
        ("global_value", SemanticDeclarationKind::Object),
        ("free_fn", SemanticDeclarationKind::Function),
        ("run", SemanticDeclarationKind::Method),
    ] {
        let declaration = index
            .declarations
            .iter()
            .find(|declaration| declaration.name == name && declaration.declaration_kind == kind)
            .unwrap_or_else(|| panic!("missing canonical {kind:?} declaration for {name}"));
        assert!(!declaration.identity.locator.fingerprint.is_empty());
        assert!(!matches!(declaration.backing, DeclarationBacking::None));
    }
}

#[test]
fn canonical_declarations_preserve_c_tag_and_object_namespaces() {
    use crate::semantic_model::SemanticDeclarationKind;

    let index = parse(
        std::path::Path::new("namespaces.c"),
        "struct Foo { int field; };\nint Foo;\n",
    );
    assert!(index.declarations.iter().any(|declaration| {
        declaration.name == "Foo" && declaration.declaration_kind == SemanticDeclarationKind::Type
    }));
    assert!(index.declarations.iter().any(|declaration| {
        declaration.name == "Foo" && declaration.declaration_kind == SemanticDeclarationKind::Object
    }));
}

#[test]
fn record_declaration_uses_exact_tag_name_range() {
    use crate::semantic_model::SemanticDeclarationKind;

    let source = "struct Widget {\n    int field;\n};\n";
    let index = parse(std::path::Path::new("record.c"), source);
    let declaration = index
        .declarations
        .iter()
        .find(|declaration| {
            declaration.name == "Widget"
                && declaration.declaration_kind == SemanticDeclarationKind::Type
        })
        .expect("record declaration");

    assert_eq!(
        &source[declaration.name_range.start_byte..declaration.name_range.end_byte],
        "Widget"
    );
    assert!(declaration.declaration_range.end_byte > declaration.name_range.end_byte);
}

#[test]
fn lexical_fallback_is_completion_only_and_has_no_candidate_identity() {
    let source = "#define FALLBACK_VALUE 1\nint fallback_object;\n";
    let index = super::lexical_fallback(
        source,
        super::scan_includes(source),
        ParseFacts::ALL,
        crate::config::SourceLanguage::C,
    );
    assert!(index.declarations.is_empty());
    assert!(!index.fallback_completions.is_empty());
}

#[test]
fn call_relation_fact_mask_is_explicit() {
    let source = "int caller(void) { return callee(); }\n";
    let skipped = parse_with_handle(
        std::path::Path::new("main.c"),
        source,
        None,
        ParseFacts::COLOR_REF,
    );
    assert!(skipped.callable_anchors.is_empty());
    assert!(skipped.call_sites.is_empty());
    assert_eq!(
        skipped.fact_availability(FactGroup::CallableAnchors),
        FactAvailability::NotRequested
    );
    assert_eq!(
        skipped.fact_availability(FactGroup::CallSites),
        FactAvailability::NotRequested
    );

    let indexed = parse_with_handle(
        std::path::Path::new("main.c"),
        source,
        None,
        ParseFacts::INDEX,
    );
    assert_eq!(
        indexed.fact_availability(FactGroup::CallSites),
        FactAvailability::Available
    );
    assert_eq!(indexed.call_sites.len(), 1);
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
    assert_eq!(skipped.diagnostics.ast_source, super::FactSource::Ast);
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
    assert_eq!(clean_empty.diagnostics.ast_source, super::FactSource::Ast);
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
    let fallback = super::lexical_fallback(
        fallback_source,
        super::scan_includes(fallback_source),
        ParseFacts::ALL,
        crate::config::SourceLanguage::C,
    );
    assert!(fallback.records.is_empty());
    assert!(fallback.members.is_empty());
    assert!(fallback.aliases.is_empty());
    assert!(fallback.local_declarations.is_empty());
    assert!(fallback.local_bindings.is_empty());
    assert!(fallback.diagnostics.fallback_used);
    assert_eq!(
        fallback.diagnostics.ast_source,
        super::FactSource::LexicalFallback
    );
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

    let fallback_index = super::lexical_fallback(
        fallback_source,
        super::scan_includes(fallback_source),
        ParseFacts::INDEX,
        crate::config::SourceLanguage::C,
    );
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

#[test]
fn c_callable_identity_ignores_parameter_names_extern_body_and_whitespace() {
    let declaration = parse(
        Path::new("api_decl.c"),
        "extern int lookup ( int key , const char * value );\n",
    );
    let definition = parse(
        Path::new("api.c"),
        "extern int lookup(int key,const char*value) { return key; }\n",
    );
    let renamed = parse(
        Path::new("renamed.c"),
        "extern int lookup(int table, const char *value) { return table; }\n",
    );
    let declaration = declaration
        .callable_anchors
        .iter()
        .find(|anchor| anchor.name == "lookup")
        .expect("declaration anchor");
    let definition = definition
        .callable_anchors
        .iter()
        .find(|anchor| anchor.name == "lookup")
        .expect("definition anchor");
    let renamed = renamed
        .callable_anchors
        .iter()
        .find(|anchor| anchor.name == "lookup")
        .expect("renamed anchor");

    assert_eq!(
        declaration.canonical_signature,
        definition.canonical_signature
    );
    assert_eq!(definition.canonical_signature, renamed.canonical_signature);
    assert!(!definition.canonical_signature.contains("extern"));
    assert!(!definition.canonical_signature.contains("key"));
    assert!(!definition.canonical_signature.contains("table"));
    assert!(declaration.presentation_signature.ends_with(';'));
    assert!(!definition.presentation_signature.contains("return key"));
    assert!(!definition.presentation_signature.contains('{'));
    assert_eq!(
        definition.signature_fidelity,
        crate::call_model::SignatureFidelity::AstExact
    );
    assert_eq!(
        &"extern int lookup(int key,const char*value) { return key; }"
            [definition.declaration_range.start_byte..definition.declaration_range.end_byte],
        definition.presentation_signature
    );
}

#[test]
fn c_callable_identity_still_rejects_incompatible_parameter_types() {
    let int_anchor = parse(Path::new("api_decl.c"), "extern int lookup(int value);\n")
        .callable_anchors
        .into_iter()
        .find(|anchor| anchor.name == "lookup")
        .expect("int anchor");
    let long_anchor = parse(Path::new("api.c"), "int lookup(long value) { return 0; }\n")
        .callable_anchors
        .into_iter()
        .find(|anchor| anchor.name == "lookup")
        .expect("long anchor");

    assert_ne!(
        int_anchor.canonical_signature,
        long_anchor.canonical_signature
    );
}

#[test]
fn c_callable_identity_ignores_nested_function_pointer_parameter_names() {
    let declaration = parse(
        Path::new("api_decl.c"),
        "int visit(int (*callback)(int));\n",
    )
    .callable_anchors
    .into_iter()
    .find(|anchor| anchor.name == "visit")
    .expect("declaration anchor");
    let definition = parse(
        Path::new("api.c"),
        "int visit(int (*fn)(int value)) { return fn(value); }\n",
    )
    .callable_anchors
    .into_iter()
    .find(|anchor| anchor.name == "visit")
    .expect("definition anchor");

    assert_eq!(
        declaration.canonical_signature,
        definition.canonical_signature
    );
    assert!(!definition.canonical_signature.contains("fn"));
    assert!(!definition.canonical_signature.contains("value"));
}

#[test]
fn callable_anchor_fingerprints_distinguish_identical_cross_file_declarations() {
    let first = parse(Path::new("first.h"), "int lookup(int key);\n");
    let second = parse(Path::new("second.h"), "int lookup(int key);\n");
    let first = first
        .callable_anchors
        .iter()
        .find(|anchor| anchor.name == "lookup")
        .expect("first declaration");
    let second = second
        .callable_anchors
        .iter()
        .find(|anchor| anchor.name == "lookup")
        .expect("second declaration");
    assert_eq!(first.entity_key, second.entity_key);
    assert_ne!(first.anchor_fingerprint, second.anchor_fingerprint);
}

#[test]
fn callable_signature_shapes_cover_default_variadic_void_and_unspecified_arity() {
    let cpp = parse(
        Path::new("arity.cpp"),
        "int defaults(int first, int second = 2);\n\
         int variadic(int first, ...);\n\
         int required(int value[sizeof(1 == 1)]);\n\
         int optional(int value = sizeof(1 == 1));\n",
    );
    let defaults = cpp
        .callable_anchors
        .iter()
        .find(|anchor| anchor.name == "defaults")
        .expect("defaults");
    assert_eq!(
        (defaults.signature.min_arity, defaults.signature.max_arity),
        (Some(1), Some(2))
    );
    let variadic = cpp
        .callable_anchors
        .iter()
        .find(|anchor| anchor.name == "variadic")
        .expect("variadic");
    let required = cpp
        .callable_anchors
        .iter()
        .find(|anchor| anchor.name == "required")
        .expect("required comparison expression");
    assert_eq!(
        (required.signature.min_arity, required.signature.max_arity),
        (Some(1), Some(1))
    );
    let optional = cpp
        .callable_anchors
        .iter()
        .find(|anchor| anchor.name == "optional")
        .expect("optional default expression");
    assert_eq!(
        (optional.signature.min_arity, optional.signature.max_arity),
        (Some(0), Some(1))
    );
    assert_eq!(variadic.signature.min_arity, Some(1));
    assert_eq!(variadic.signature.max_arity, None);
    assert!(variadic.signature.variadic);

    let c = parse(Path::new("arity.c"), "int old();\nint zero(void);\n");
    let old = c
        .callable_anchors
        .iter()
        .find(|anchor| anchor.name == "old")
        .expect("old style");
    let zero = c
        .callable_anchors
        .iter()
        .find(|anchor| anchor.name == "zero")
        .expect("void");
    assert_eq!(
        (old.signature.min_arity, old.signature.max_arity),
        (None, None)
    );
    assert_eq!(
        (zero.signature.min_arity, zero.signature.max_arity),
        (Some(0), Some(0))
    );
}

#[test]
fn record_ranges_preserve_multiline_body_comments_preprocessor_crlf_and_utf16() {
    let source = "/*😀*/ struct Packet {\r\n    int type; /* field docs */\r\n#ifdef WITH_SIZE\r\n    int size;\r\n#endif\r\n};\r\n";
    let index = parse(Path::new("packet.h"), source);
    let record = index
        .records
        .iter()
        .find(|record| record.display_name == "Packet")
        .expect("Packet record");

    assert_eq!(
        &source[record.start_byte..record.end_byte],
        "struct Packet {\r\n    int type; /* field docs */\r\n#ifdef WITH_SIZE\r\n    int size;\r\n#endif\r\n}"
    );
    let body = &source[record.body_range.start_byte..record.body_range.end_byte];
    assert!(body.starts_with('{') && body.ends_with('}'));
    assert!(body.contains("/* field docs */"));
    assert!(body.contains("#ifdef WITH_SIZE"));
    assert_eq!(
        &source[record.declaration_range.start_byte..record.declaration_range.end_byte],
        "struct Packet {\r\n    int type; /* field docs */\r\n#ifdef WITH_SIZE\r\n    int size;\r\n#endif\r\n};"
    );
    assert_eq!(
        record.start_col,
        "/*😀*/ ".encode_utf16().count(),
        "record columns are UTF-16, not UTF-8 bytes"
    );
    assert_eq!(record.range_fidelity, super::RecordRangeFidelity::AstExact);
}

#[test]
fn record_ranges_cover_anonymous_typedef_union_and_cpp_class() {
    for (path, source, name, kind) in [
        (
            "anon.h",
            "typedef struct { int value; } Buffer;",
            "Buffer",
            super::RecordKind::Struct,
        ),
        (
            "union.h",
            "union Value { int integer; };",
            "Value",
            super::RecordKind::Union,
        ),
        (
            "widget.hpp",
            "class Widget { int width; };",
            "Widget",
            super::RecordKind::Class,
        ),
    ] {
        let index = parse(Path::new(path), source);
        let record = index
            .records
            .iter()
            .find(|record| record.display_name == name)
            .expect("record");
        assert_eq!(record.kind, kind);
        assert!(source[record.body_range.start_byte..record.body_range.end_byte].starts_with('{'));
        assert!(
            source[record.declaration_range.start_byte..record.declaration_range.end_byte]
                .ends_with(';')
        );
    }
}

#[test]
fn typedef_facts_keep_each_declarator_shape_ranges_and_same_named_tag_alias() {
    let source = "struct Foo { int value; };\n\
typedef struct Foo Foo, *FooPtr, FooArray[4];\n\
typedef const struct Foo FooConst;\n\
typedef int (*Callback)(int);\n\
typedef struct Foo Foo;\n";
    let index = parse(Path::new("aliases.h"), source);
    let aliases = &index.aliases;

    let first_foo = aliases
        .iter()
        .find(|alias| alias.alias == "Foo")
        .expect("same-named tag alias must be retained");
    assert_eq!(first_foo.declarator_shape, super::DeclaratorShape::Identity);
    assert_eq!(first_foo.underlying_spelling, "struct Foo");
    assert_eq!(&source[first_foo.start_byte..first_foo.end_byte], "Foo");
    assert!(
        source[first_foo.declaration_range.start_byte..first_foo.declaration_range.end_byte]
            .starts_with("typedef struct Foo")
    );

    let pointer = aliases
        .iter()
        .find(|alias| alias.alias == "FooPtr")
        .expect("pointer alias");
    assert_eq!(
        pointer.declarator_shape,
        super::DeclaratorShape::Pointer {
            qualifiers: Vec::new()
        }
    );
    let array = aliases
        .iter()
        .find(|alias| alias.alias == "FooArray")
        .expect("array alias");
    assert_eq!(
        array.declarator_shape,
        super::DeclaratorShape::Array {
            extent_text: "4".to_string()
        }
    );
    let qualified = aliases
        .iter()
        .find(|alias| alias.alias == "FooConst")
        .expect("qualified alias");
    assert_eq!(qualified.underlying_spelling, "const struct Foo");
    assert_eq!(
        qualified.declarator_shape,
        super::DeclaratorShape::Qualified {
            qualifiers: vec!["const".to_string()]
        }
    );
    let callback = aliases
        .iter()
        .find(|alias| alias.alias == "Callback")
        .expect("function pointer alias");
    assert_eq!(
        callback.declarator_shape,
        super::DeclaratorShape::Unsupported
    );
    assert!(aliases.iter().all(|alias| {
        alias.target_fidelity == super::AliasTargetFidelity::AstExact
            && alias.fingerprint.len() == 24
    }));
    let foo_fingerprints: std::collections::HashSet<_> = aliases
        .iter()
        .filter(|alias| alias.alias == "Foo")
        .map(|alias| alias.fingerprint.as_str())
        .collect();
    assert_eq!(foo_fingerprints.len(), 2);
}

#[test]
fn malformed_typedef_never_claims_an_exact_declarator_shape() {
    let index = parse(
        Path::new("malformed_alias.h"),
        "struct Foo { int value; };\ntypedef struct Foo Broken",
    );
    let alias = index
        .aliases
        .iter()
        .find(|alias| alias.alias == "Broken")
        .expect("best-effort malformed alias fact");
    assert_eq!(alias.target_fidelity, super::AliasTargetFidelity::Malformed);
    assert_eq!(alias.declarator_shape, super::DeclaratorShape::Unsupported);
}

#[test]
fn hover_semantics_mask_collects_type_and_callable_facts_without_occurrences() {
    let index = parse_with_handle(
        Path::new("hover.h"),
        "struct Foo { int value; }; typedef struct Foo FooT; int use(FooT value);",
        None,
        ParseFacts::HOVER_SEMANTICS,
    );
    assert!(!index.records.is_empty());
    assert!(!index.aliases.is_empty());
    assert!(!index.callable_anchors.is_empty());
    assert!(index.occurrences.is_empty());
    assert!(index.local_declarations.is_empty());
    assert_eq!(
        index.fact_availability(FactGroup::Records),
        FactAvailability::Available
    );
    assert_eq!(
        index.fact_availability(FactGroup::Aliases),
        FactAvailability::Available
    );
    assert_eq!(
        index.fact_availability(FactGroup::CallableAnchors),
        FactAvailability::Available
    );
    assert_eq!(
        index.fact_availability(FactGroup::Occurrences),
        FactAvailability::NotRequested
    );
}

#[test]
fn declaration_and_call_relation_masks_are_strictly_decoupled() {
    let source = "struct Widget { int value; };\n\
                  typedef struct Widget WidgetAlias;\n\
                  int helper(void);\n\
                  int value = helper();\n";

    let declarations_only =
        parse_with_handle(Path::new("masks.c"), source, None, ParseFacts::DECLARATIONS);
    assert!(declarations_only
        .declarations
        .iter()
        .any(|declaration| declaration.name == "helper"));
    assert!(declarations_only.declarations.iter().any(|declaration| {
        declaration.name == "Widget"
            && matches!(
                declaration.backing,
                crate::semantic_model::DeclarationBacking::Record { .. }
            )
    }));
    assert!(declarations_only.declarations.iter().any(|declaration| {
        declaration.name == "WidgetAlias"
            && declaration.declaration_kind == crate::semantic_model::SemanticDeclarationKind::Alias
            && matches!(
                declaration.backing,
                crate::semantic_model::DeclarationBacking::TypeAlias { .. }
            )
    }));
    assert!(declarations_only.call_sites.is_empty());
    assert!(declarations_only.callable_anchors.is_empty());
    assert_eq!(
        declarations_only.fact_availability(FactGroup::CallableAnchors),
        FactAvailability::NotRequested
    );
    assert_eq!(
        declarations_only.fact_availability(FactGroup::CallSites),
        FactAvailability::NotRequested
    );

    let relations_only = parse_with_handle(
        Path::new("masks.c"),
        source,
        None,
        ParseFacts::CALL_RELATIONS,
    );
    assert!(relations_only.declarations.is_empty());
    assert!(!relations_only.callable_anchors.is_empty());
    assert_eq!(relations_only.call_sites.len(), 1);

    let records_only = parse_with_handle(
        Path::new("masks.h"),
        "struct Widget { int value; };\n",
        None,
        ParseFacts::RECORDS,
    );
    assert!(!records_only.records.is_empty());
    assert!(records_only.declarations.is_empty());
}
