use std::path::Path;

use super::super::{parse, LocalBindingKind};
use super::infer_in;

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
    let names: Vec<(&str, LocalBindingKind)> = index
        .local_bindings
        .iter()
        .map(|binding| (binding.name.as_str(), binding.kind))
        .collect();
    assert!(names.contains(&("count", LocalBindingKind::Parameter)));
    assert!(names.contains(&("foo", LocalBindingKind::Parameter)));
    assert!(names.contains(&("cursor_limit", LocalBindingKind::LocalVariable)));
    assert!(names.contains(&("name", LocalBindingKind::LocalVariable)));
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
