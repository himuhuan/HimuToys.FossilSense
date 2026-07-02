use super::*;

#[test]
fn kind_counts_scoped_filters_to_reachable_files() {
    use std::collections::HashSet;

    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    // Same-named type defined in two unrelated workspace headers.
    upsert_source(&mut store, "inc/b.h", "typedef int widget_t;\n");
    upsert_source(&mut store, "other/c.h", "typedef float widget_t;\n");
    let reader = IndexStore::open_readonly(&db).expect("readonly");

    // Unscoped: both definitions count.
    let unscoped = reader
        .kind_counts_by_names(&["widget_t"])
        .expect("unscoped");
    assert_eq!(unscoped["widget_t"].get("type").copied(), Some(2));

    // Scoped to only the reachable header: a single definition counts.
    let reachable: HashSet<String> = ["inc/b.h".to_string()].into_iter().collect();
    let scoped = reader
        .kind_counts_by_names_scoped(&["widget_t"], Some(&reachable))
        .expect("scoped");
    assert_eq!(scoped["widget_t"].get("type").copied(), Some(1));

    // Scope that reaches neither header: the name is absent (would not color).
    let elsewhere: HashSet<String> = ["src/a.c".to_string()].into_iter().collect();
    let none = reader
        .kind_counts_by_names_scoped(&["widget_t"], Some(&elsewhere))
        .expect("none");
    assert!(!none.contains_key("widget_t"));

    // None scope behaves exactly like the unscoped query (open/fallback).
    let passthrough = reader
        .kind_counts_by_names_scoped(&["widget_t"], None)
        .expect("passthrough");
    assert_eq!(passthrough["widget_t"].get("type").copied(), Some(2));
}

#[test]
fn fields_by_record_scoped_prefers_reachable_record() {
    use std::collections::HashSet;

    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    // Same struct tag, different members, in two files.
    upsert_source(&mut store, "inc/w.h", "struct W { int width; };\n");
    upsert_source(&mut store, "other/w.h", "struct W { int height; };\n");
    let reader = IndexStore::open_readonly(&db).expect("readonly");

    // Unscoped: members from both definitions.
    let all = fields_by_record_names(&reader, &["W"]);
    assert_eq!(all, vec!["height".to_string(), "width".to_string()]);

    // Scoped to the reachable file: only that record's fields.
    let reachable: HashSet<String> = ["inc/w.h".to_string()].into_iter().collect();
    let scoped = fields_by_record_names_scoped(&reader, &["W"], &reachable);
    assert_eq!(scoped, vec!["width".to_string()]);
}

#[test]
fn include_edges_round_trip_and_open_files() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    upsert_source(&mut store, "a.c", "int a;\n");
    upsert_source(&mut store, "b.h", "int b;\n");

    let files = store.files_with_ids().expect("files");
    let id = |p: &str| files.iter().find(|(_, path, _)| path == p).unwrap().0;

    // a.c -> b.h resolved edge with its resolution kind; a.c also has one
    // unresolved include and one ambiguous include (multi-hit no exact).
    store
        .replace_include_edges(
            &[id("a.c")],
            &[(id("a.c"), id("b.h"), "suffix_match".to_string())],
            &[(id("a.c"), 1)],
            &[(id("a.c"), 1)],
            true,
        )
        .expect("replace");

    let edges = store.load_include_edge_paths().expect("edges");
    assert_eq!(edges, vec![("a.c".to_string(), "b.h".to_string())]);
    let open = store.open_include_file_paths().expect("open");
    assert_eq!(open, vec!["a.c".to_string()]);
    let ambiguous = store.ambiguous_include_file_paths().expect("ambiguous");
    assert_eq!(ambiguous, vec!["a.c".to_string()]);
    // The edge round-trips its `resolution` (one of the four kind strings),
    // so a future derivation/read can rely on the recorded kind instead of
    // recomputing it.
    let with_kind = store
        .load_include_edge_paths_with_resolution()
        .expect("edges with kind");
    assert_eq!(
        with_kind,
        vec![(
            "a.c".to_string(),
            "b.h".to_string(),
            "suffix_match".to_string()
        )]
    );
}
