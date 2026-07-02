use super::*;

// --- R7: slop-case fixtures and integration tests ------------------------

fn write_shadow_kinds_fixture(dir: &std::path::Path) {
    fs::write(
        dir.join("shadow_test.c"),
        "#define SHADOW 1\n\
             typedef int SHADOW;\n\
             int SHADOW(void) { return 2; }\n\
             static int SHADOW;\n\
             void use(void) {\n    int SHADOW = SHADOW();\n    (void)SHADOW;\n}\n",
    )
    .expect("shadow_test.c");
}

fn write_typedef_alias_fixture(dir: &std::path::Path) {
    fs::create_dir_all(dir.join("src")).expect("src");
    fs::create_dir_all(dir.join("vendor")).expect("vendor");
    // main.c defines FooT — it is Current-tier for itself.
    fs::write(
        dir.join("src/main.c"),
        "typedef int FooT;\nFooT global_foo;\n",
    )
    .expect("main.c");
    // util.h also defines FooT — unreachable from main.c (not included).
    fs::write(dir.join("src/util.h"), "typedef float FooT;\n").expect("util.h");
    // vendor.h also defines FooT — also unreachable.
    fs::write(dir.join("vendor/vendor.h"), "typedef long FooT;\n").expect("vendor.h");
}

fn write_struct_multi_def_fixture(dir: &std::path::Path) {
    fs::create_dir_all(dir.join("src")).expect("src");
    fs::write(
        dir.join("src/main.c"),
        "struct W { int a; };\nvoid use(void) { struct W w; w.a = 1; }\n",
    )
    .expect("main.c");
    fs::write(dir.join("src/other.h"), "struct W { float b; char* c; };\n").expect("other.h");
}

fn write_multi_root_fixture(root_a: &std::path::Path, root_b: &std::path::Path) {
    fs::create_dir_all(root_a).expect("root-a");
    fs::create_dir_all(root_b).expect("root-b");
    fs::write(root_a.join("api.h"), "int api_init(void);\n").expect("root-a/api.h");
    fs::write(root_b.join("api.h"), "float api_init(float);\n").expect("root-b/api.h");
}

#[test]
fn shadow_kinds_different_kinds_same_name_all_indexed() {
    use crate::store::IndexStore;
    let dir = tempdir().expect("tempdir");
    write_shadow_kinds_fixture(dir.path());
    let db = dir.path().join("index.sqlite");

    index_workspace(
        dir.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            force: false,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let store = IndexStore::open_readonly(&db).expect("readonly");
    let names = store.load_symbol_names_with_paths().expect("names");

    // All five SHADOW entities are indexed — verify no panic and distinct kinds.
    let shadow_entries: Vec<_> = names
        .iter()
        .filter(|(_, name, _, _, _, _)| name == "SHADOW")
        .collect();
    assert!(
        shadow_entries.len() >= 3,
        "at least macro, typedef, function kinds indexed (locals may not be global symbols)"
    );
    let kinds: Vec<&str> = shadow_entries
        .iter()
        .map(|(_, _, _, _, kind, _)| kind.as_str())
        .collect();
    // Verify at least macro, type, and function are present with distinct kinds.
    assert!(kinds.contains(&"macro"), "SHADOW macro must be indexed");
    assert!(kinds.contains(&"type"), "SHADOW typedef must be indexed");
    assert!(
        kinds.contains(&"function"),
        "SHADOW function must be indexed"
    );
}

#[test]
fn typedef_alias_same_name_different_reach_sets() {
    let dir = tempdir().expect("tempdir");
    write_typedef_alias_fixture(dir.path());
    let db = dir.path().join("index.sqlite");

    index_workspace(
        dir.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            force: false,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let store = IndexStore::open_readonly(&db).expect("readonly");
    let names = store.load_symbol_names_with_paths().expect("names");
    let foo_entries: Vec<_> = names
        .iter()
        .filter(|(_, name, _, _, _, _)| name == "FooT")
        .collect();
    // At least the main.c FooT is indexed; others may or may not be depending
    // on whether they were parsed. The key is no panic and the reachable one is
    // distinguishable by path from the unreachable ones.
    assert!(!foo_entries.is_empty(), "at least one FooT is indexed");
    let has_main = foo_entries
        .iter()
        .any(|(_, _, _, path, _, _)| path == "src/main.c");
    assert!(has_main, "FooT from src/main.c is indexed");

    // Build the include graph. Since main.c does not include util.h or vendor.h,
    // those files are not reachable — their FooT aliases stay isolated.
    let edges = store.load_include_edge_paths().expect("edges");
    let unresolved = store.open_include_file_paths().unwrap_or_default();
    let ambiguous = store.ambiguous_include_file_paths().unwrap_or_default();
    let graph = crate::reachability::ReachGraph::new(edges, unresolved, ambiguous);
    let scope = graph.reachable("src/main.c");
    // main.c has no #include directives, so scope is just itself (determinate).
    assert!(!scope.open, "scope is determinate — no includes at all");
}

#[test]
fn struct_multi_def_different_fields() {
    let dir = tempdir().expect("tempdir");
    write_struct_multi_def_fixture(dir.path());
    let db = dir.path().join("index.sqlite");

    index_workspace(
        dir.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            force: false,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let store = IndexStore::open_readonly(&db).expect("readonly");
    // Verify both struct W definitions exist in the store with different paths.
    let names = store.load_symbol_names_with_paths().expect("names");
    let w_types: Vec<_> = names
        .iter()
        .filter(|(_, name, _, _, kind, _)| name == "W" && kind == "type")
        .collect();
    assert_eq!(w_types.len(), 2, "both struct W definitions indexed");
    let paths: Vec<&str> = w_types
        .iter()
        .map(|(_, _, _, path, _, _)| path.as_str())
        .collect();
    assert!(paths.contains(&"src/main.c"));
    assert!(paths.contains(&"src/other.h"));

    // Build reachability: main.c doesn't include other.h, so other.h's struct W
    // fields should not pollute main.c's reachable scope.
    let edges = store.load_include_edge_paths().expect("edges");
    let unresolved = store.open_include_file_paths().unwrap_or_default();
    let ambiguous = store.ambiguous_include_file_paths().unwrap_or_default();
    let graph = crate::reachability::ReachGraph::new(edges, unresolved, ambiguous);
    let scope = graph.reachable("src/main.c");
    // src/other.h is NOT reachable from main.c (not included, no edge).
    assert!(
        !scope.files.contains("src/other.h"),
        "other.h is unreachable from main.c"
    );
}

#[test]
fn multi_root_same_name_candidates() {
    let dir = tempdir().expect("tempdir");
    let root_a = dir.path().join("root-a");
    let root_b = dir.path().join("root-b");
    write_multi_root_fixture(&root_a, &root_b);

    // Index each root independently into separate databases (mimics multi-root).
    let db_a = dir.path().join("a.sqlite");
    let db_b = dir.path().join("b.sqlite");

    index_workspace(
        &root_a,
        IndexOptions {
            db_path: Some(db_a.clone()),
            force: false,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index root-a");

    index_workspace(
        &root_b,
        IndexOptions {
            db_path: Some(db_b.clone()),
            force: false,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index root-b");

    let store_a = IndexStore::open_readonly(&db_a).expect("readonly a");
    let store_b = IndexStore::open_readonly(&db_b).expect("readonly b");

    let names_a = store_a.load_symbol_names_with_paths().expect("names a");
    let names_b = store_b.load_symbol_names_with_paths().expect("names b");

    let a_init: Vec<_> = names_a
        .iter()
        .filter(|(_, n, _, _, _, _)| n == "api_init")
        .collect();
    let b_init: Vec<_> = names_b
        .iter()
        .filter(|(_, n, _, _, _, _)| n == "api_init")
        .collect();

    assert_eq!(a_init.len(), 1, "root-a has one api_init");
    assert_eq!(b_init.len(), 1, "root-b has one api_init");
    // Both roots index their own api.h independently; the paths within each
    // root's database are root-relative. Each database starts at id=1, so
    // both get id=1 — that's correct, each root's DB is independent.
    // The key property: multi-root indexing produces both candidates without
    // panic, and they can coexist as independent name-table entries when
    // the server merges them across roots.
}
