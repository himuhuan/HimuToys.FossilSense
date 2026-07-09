use std::path::Path;

use super::super::{
    parse, parse_with_handle, AliasTarget, MemberConfidence, MemberKind, ParseFacts, ParserHandle,
    RecordConfidence, RecordKind, SymbolKind, SymbolRole,
};
use super::field_containers;

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
fn extracts_multiline_typedef_struct_when_member_comments_contain_braces() {
    let index = parse(
        Path::new("b.c"),
        "typedef struct {\n    int x; // comment with }\n    const char *text; /* comment with { */\n} Boom;\n",
    );

    assert!(index.symbols.iter().any(|symbol| {
        symbol.name == "Boom"
            && symbol.kind == SymbolKind::Type
            && symbol.role == SymbolRole::Definition
    }));
    assert_eq!(field_containers(&index, "x"), vec!["Boom".to_string()]);
    assert_eq!(field_containers(&index, "text"), vec!["Boom".to_string()]);
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
        .symbols
        .iter()
        .find(|symbol| {
            symbol.name == "xxx_t"
                && symbol.kind == SymbolKind::Type
                && symbol.role == SymbolRole::Definition
        })
        .expect("first typedef after multiline macro should be a type definition");
    assert!(
        xxx_t.signature.starts_with("typedef struct xxx"),
        "typedef signature should not include the macro body: {:?}",
        xxx_t.signature
    );
    assert!(!xxx_t.signature.contains("while (0)"));

    assert!(index.symbols.iter().any(|symbol| {
        symbol.name == "xxxa_t"
            && symbol.kind == SymbolKind::Type
            && symbol.role == SymbolRole::Definition
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

    assert!(index.symbols.iter().any(|symbol| {
        symbol.name == "after_macro_t"
            && symbol.kind == SymbolKind::Type
            && symbol.role == SymbolRole::Definition
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

    assert!(index.symbols.iter().any(|symbol| {
        symbol.name == "guarded_t"
            && symbol.kind == SymbolKind::Type
            && symbol.role == SymbolRole::Definition
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

    assert!(index.symbols.iter().any(|symbol| {
        symbol.name == "context_t"
            && symbol.kind == SymbolKind::Type
            && symbol.role == SymbolRole::Definition
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
    let index = parse(
        Path::new("nested.c"),
        "typedef struct { struct { int xxx; } mem1[4]; union { int tag; } u; } A;\n",
    );

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
            && record.confidence == RecordConfidence::Heuristic));
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
        && matches!(&alias.target, AliasTarget::NamedRecord { tag, .. } if tag == "Foo")));
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
    assert_eq!(w_rec.kind, RecordKind::Struct);
    assert_eq!(w_rec.confidence, RecordConfidence::NamedTag);

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
    assert_eq!(widget_rec.confidence, RecordConfidence::AnonymousTypedef);

    let widget_fields: Vec<&str> = index
        .fields
        .iter()
        .filter(|f| f.record_key == widget_rec.record_key)
        .map(|f| f.name.as_str())
        .collect();
    assert_eq!(widget_fields, vec!["field_widget"]);

    // 3. Check typedef FooT alias
    let foot_alias = index.aliases.iter().find(|a| a.alias == "FooT").unwrap();
    assert!(matches!(&foot_alias.target, AliasTarget::NamedRecord { tag, .. } if tag == "Foo"));

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
