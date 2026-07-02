use super::*;

#[test]
fn builds_include_edges_and_reachability_scopes_coloring() {
    use crate::reachability::ReachGraph;
    use crate::store::IndexStore;

    let dir = tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("src")).expect("src");
    fs::create_dir_all(dir.path().join("inc")).expect("inc");
    fs::create_dir_all(dir.path().join("other")).expect("other");
    // a.c includes inc/b.h (reachable); other/c.h defines the same type name
    // but is not included anywhere.
    fs::write(
        dir.path().join("src/a.c"),
        "#include \"b.h\"\nwidget_t use(void) { return (widget_t)0; }\n",
    )
    .expect("a.c");
    fs::write(dir.path().join("inc/b.h"), "typedef int widget_t;\n").expect("b.h");
    fs::write(dir.path().join("other/c.h"), "typedef float widget_t;\n").expect("c.h");
    // d.c has an include that resolves to nothing => its reachable set opens.
    fs::write(
        dir.path().join("src/d.c"),
        "#include \"nonexistent.h\"\nint d(void){return 0;}\n",
    )
    .expect("d.c");
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

    // The a.c -> inc/b.h edge resolved (the include matched by suffix).
    let edges = store.load_include_edge_paths().expect("edges");
    assert!(
        edges.contains(&("src/a.c".to_string(), "inc/b.h".to_string())),
        "a.c should resolve an edge to inc/b.h, got {edges:?}"
    );

    // Build the reachability graph the way the server does.
    let graph = ReachGraph::new(
        edges,
        store.open_include_file_paths().expect("open"),
        store.ambiguous_include_file_paths().expect("ambiguous"),
    );

    // a.c reaches itself and inc/b.h, determinately; never other/c.h.
    let scope_a = graph.reachable("src/a.c");
    assert!(scope_a.files.contains("src/a.c"));
    assert!(scope_a.files.contains("inc/b.h"));
    assert!(!scope_a.files.contains("other/c.h"));
    assert!(!scope_a.open, "a.c include picture is fully resolved");

    // d.c has an unresolved include => its set is open (soften the gate).
    let scope_d = graph.reachable("src/d.c");
    assert!(scope_d.open, "an unresolvable include opens the set");

    // Coloring is scoped to a.c's reachable set: widget_t counts once (b.h),
    // not twice — the unreachable other/c.h definition is excluded.
    let scoped = store
        .kind_counts_by_names_scoped(&["widget_t"], Some(&scope_a.files))
        .expect("scoped counts");
    assert_eq!(scoped["widget_t"].get("type").copied(), Some(1));

    // Unscoped (the open/fallback path) still sees both workspace defs.
    let unscoped = store
        .kind_counts_by_names(&["widget_t"])
        .expect("unscoped counts");
    assert_eq!(unscoped["widget_t"].get("type").copied(), Some(2));
}

#[test]
fn dirty_add_header_rebuilds_sources_that_previously_had_unresolved_include() {
    use crate::reachability::ReachGraph;

    let dir = tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("src")).expect("src");
    fs::create_dir_all(dir.path().join("inc")).expect("inc");
    let source_path = dir.path().join("src/a.c");
    let header_path = dir.path().join("inc/b.h");
    fs::write(
        &source_path,
        "#include \"b.h\"\nwidget_t use(void) { return (widget_t)0; }\n",
    )
    .expect("a.c");
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
    .expect("initial index");

    {
        let store = IndexStore::open_readonly(&db).expect("readonly");
        let graph = ReachGraph::new(
            store.load_include_edge_paths().expect("edges"),
            store.open_include_file_paths().expect("open"),
            store.ambiguous_include_file_paths().expect("ambiguous"),
        );
        assert!(
            graph.reachable("src/a.c").open,
            "missing header should leave the source open"
        );
    }

    fs::write(&header_path, "typedef int widget_t;\n").expect("b.h");
    let stats = index_dirty_files(
        dir.path(),
        vec![DirtyFileChange {
            absolute_path: header_path,
            kind: DirtyFileKind::Upsert,
        }],
        IndexOptions {
            db_path: Some(db.clone()),
            force: false,
            ..Default::default()
        },
        |_| {},
    )
    .expect("dirty index");
    assert!(
        stats
            .include_edge_sources_rebuilt
            .contains(&"src/a.c".to_string()),
        "dirty header add should report the source whose include edges were rebuilt"
    );

    let store = IndexStore::open_readonly(&db).expect("readonly");
    let edges = store.load_include_edge_paths().expect("edges");
    assert!(
        edges.contains(&("src/a.c".to_string(), "inc/b.h".to_string())),
        "adding b.h should rebuild a.c's include edge, got {edges:?}"
    );
    let graph = ReachGraph::new(
        edges,
        store.open_include_file_paths().expect("open"),
        store.ambiguous_include_file_paths().expect("ambiguous"),
    );
    let scope = graph.reachable("src/a.c");
    assert!(scope.files.contains("inc/b.h"));
    assert!(
        !scope.open,
        "resolved include should clear the source's open marker"
    );
}

#[test]
fn dirty_delete_header_rebuilds_sources_that_included_it_as_open() {
    use crate::reachability::ReachGraph;

    let dir = tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("src")).expect("src");
    fs::create_dir_all(dir.path().join("inc")).expect("inc");
    let source_path = dir.path().join("src/a.c");
    let header_path = dir.path().join("inc/b.h");
    fs::write(
        &source_path,
        "#include \"b.h\"\nwidget_t use(void) { return (widget_t)0; }\n",
    )
    .expect("a.c");
    fs::write(&header_path, "typedef int widget_t;\n").expect("b.h");
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
    .expect("initial index");

    fs::remove_file(&header_path).expect("delete b.h");
    let stats = index_dirty_files(
        dir.path(),
        vec![DirtyFileChange {
            absolute_path: header_path,
            kind: DirtyFileKind::Delete,
        }],
        IndexOptions {
            db_path: Some(db.clone()),
            force: false,
            ..Default::default()
        },
        |_| {},
    )
    .expect("dirty index");
    assert!(
        stats
            .include_edge_sources_rebuilt
            .contains(&"src/a.c".to_string()),
        "dirty header delete should report the source whose include edges were rebuilt"
    );

    let store = IndexStore::open_readonly(&db).expect("readonly");
    let edges = store.load_include_edge_paths().expect("edges");
    assert!(
        !edges.contains(&("src/a.c".to_string(), "inc/b.h".to_string())),
        "deleted b.h edge should be gone, got {edges:?}"
    );
    let graph = ReachGraph::new(
        edges,
        store.open_include_file_paths().expect("open"),
        store.ambiguous_include_file_paths().expect("ambiguous"),
    );
    assert!(
        graph.reachable("src/a.c").open,
        "deleting b.h should mark a.c's include set open"
    );
}

#[test]
fn respects_fossilsense_json_exclude() {
    let dir = tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("src")).expect("src");
    fs::create_dir_all(dir.path().join("src/generated")).expect("src/generated");
    fs::write(
        dir.path().join("fossilsense.json"),
        r#"{"include": ["src/"], "exclude": ["src/generated/"]}"#,
    )
    .expect("config");
    fs::write(
        dir.path().join("src/main.c"),
        "int main(void) { return 0; }\n",
    )
    .expect("main");
    fs::write(
        dir.path().join("src/generated/auto.c"),
        "int auto(void) { return 0; }\n",
    )
    .expect("auto");
    let db = dir.path().join("index.sqlite");

    let stats = index_workspace(
        dir.path(),
        IndexOptions {
            db_path: Some(db),
            force: false,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    assert_eq!(stats.total_files, 1, "src/generated/ should be excluded");
}

// --- Single resolution pass: form/priority, ambiguity-opens-scope,
//     consistent `directly_included` derivation -------------------------

#[test]
fn same_basename_quote_resolves_local_header_determinate_scope() {
    // The acceptance-criterion case: `src/a/foo.c` includes "util.h" with a
    // same-basename `vendor/util.h` also indexed. The quote form's
    // RelativeExact tier wins (the including file's own dir), producing one
    // edge to src/a/util.h; vendor/util.h never becomes a twin edge and the
    // scope stays determinate (no ambiguity ⇒ closed).
    use crate::reachability::ReachGraph;

    let dir = tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("src/a")).expect("src/a");
    fs::create_dir_all(dir.path().join("vendor")).expect("vendor");
    fs::write(
        dir.path().join("src/a/foo.c"),
        "#include \"util.h\"\nint use(void){ return util_value(); }\n",
    )
    .expect("foo.c");
    fs::write(dir.path().join("src/a/util.h"), "int util_value(void);\n").expect("src/a/util.h");
    fs::write(dir.path().join("vendor/util.h"), "int util_value(void);\n").expect("vendor/util.h");
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
    let edges = store.load_include_edge_paths().expect("edges");
    // One and only one edge from foo.c — to the local header, never the
    // vendor twin.
    let foo_edges: Vec<_> = edges
        .iter()
        .filter(|(src, _)| src == "src/a/foo.c")
        .collect();
    assert_eq!(
        foo_edges,
        vec![&("src/a/foo.c".to_string(), "src/a/util.h".to_string())],
        "same-basename quote resolves the local header, no twin edge; got {edges:?}"
    );

    let graph = ReachGraph::new(
        store.load_include_edge_paths().expect("edges"),
        store.open_include_file_paths().expect("open"),
        store.ambiguous_include_file_paths().expect("ambiguous"),
    );
    let scope = graph.reachable("src/a/foo.c");
    assert!(scope.files.contains("src/a/util.h"));
    assert!(
        !scope.files.contains("vendor/util.h"),
        "vendor twin must not be proven reachable"
    );
    assert!(!scope.open, "no unresolved/ambiguous include ⇒ determinate");
    assert!(scope.reason.is_none());
}

#[test]
fn multi_hit_include_opens_scope_ambiguous_no_twin_edges() {
    // `src/x.c` #include "util.h" with NO exact-tier hit and BOTH lib/util.h
    // and vendor/util.h carrying the basename. Resolution is Ambiguous: the
    // source's `ambiguous_includes` count goes up, no include_edges are
    // produced for either twin, and the scope opens with AmbiguousInclude.
    use crate::reachability::{OpenReason, ReachGraph};

    let dir = tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("src")).expect("src");
    fs::create_dir_all(dir.path().join("lib")).expect("lib");
    fs::create_dir_all(dir.path().join("vendor")).expect("vendor");
    fs::write(
        dir.path().join("src/x.c"),
        "#include \"util.h\"\nint x(void){ return 0; }\n",
    )
    .expect("x.c");
    fs::write(dir.path().join("lib/util.h"), "int lib_util(void);\n").expect("lib/util.h");
    fs::write(dir.path().join("vendor/util.h"), "int vendor_util(void);\n").expect("vendor/util.h");
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
    let edges = store.load_include_edge_paths().expect("edges");
    // No edges from x.c to either twin — ambiguity adds nothing to the
    // proven reachable set.
    assert!(
        !edges.iter().any(|(src, _)| src == "src/x.c"),
        "ambiguous include MUST NOT produce proven edges; got {edges:?}"
    );

    let graph = ReachGraph::new(
        store.load_include_edge_paths().expect("edges"),
        store.open_include_file_paths().expect("open"),
        store.ambiguous_include_file_paths().expect("ambiguous"),
    );
    let scope = graph.reachable("src/x.c");
    assert!(scope.open);
    assert_eq!(
        scope.reason,
        Some(OpenReason::AmbiguousInclude),
        "an ambiguous include opens the scope with AmbiguousInclude"
    );
}

#[test]
fn quote_resolving_local_does_not_flag_external_twin_directly_included() {
    // `src/a/foo.c` #include "util.h" resolves RelativeExact to src/a/util.h,
    // while a configured include path also contains a util.h. The external
    // util.h MUST NOT be flagged `directly_included` (the second loose
    // matcher that used to flag it is deleted; the flag is now derived only
    // from `external_exact` edges, and this edge is `relative_exact`).
    let ws = tempdir().expect("ws");
    fs::create_dir_all(ws.path().join("src/a")).expect("src/a");
    fs::write(
        ws.path().join("src/a/foo.c"),
        "#include \"util.h\"\nint use(void){ return util_value(); }\n",
    )
    .expect("foo.c");
    // Local util.h: provides the definition; the external twin carries the
    // same name so any first-layer flagging would double-count it.
    fs::write(
        ws.path().join("src/a/util.h"),
        "int util_value(void){ return 7; }\n",
    )
    .expect("src/a/util.h");

    let ext = tempdir().expect("ext");
    // External util.h: same name, would be the wrong twin if it were ever
    // flagged first-layer.
    fs::write(
        ext.path().join("util.h"),
        "int util_value(void){ return 99; }\n",
    )
    .expect("ext util.h");
    let ext_root = ext.path().to_string_lossy().replace('\\', "/");
    let db = ws.path().join("index.sqlite");

    index_workspace(
        ws.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            include_paths: vec![ext_root],
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let store = IndexStore::open_readonly(&db).expect("readonly");
    // The edge from foo.c to the LOCAL util.h is `relative_exact`; the
    // external util.h is NOT a proven edge target.
    let paths = store
        .load_include_edge_paths_with_resolution()
        .expect("edges with kind");
    let foo_edges: Vec<_> = paths
        .iter()
        .filter(|(src, _, _)| src == "src/a/foo.c")
        .collect();
    assert_eq!(
        foo_edges,
        vec![&(
            "src/a/foo.c".to_string(),
            "src/a/util.h".to_string(),
            "relative_exact".to_string()
        )],
        "quote include resolves the local util.h as RelativeExact; got {paths:?}"
    );

    let ext_path = format!("{}/util.h", ext.path().to_string_lossy().replace('\\', "/"));
    // External `util.h` symbol is present but NOT first-layer (its
    // directly_included flag is false because no workspace file has an
    // ExternalExact edge to it).
    let defs = store.symbols_by_name("util_value").expect("util_value");
    let ext_def = defs
        .iter()
        .find(|r| r.path == ext_path)
        .expect("external util.h indexed");
    assert!(
        !ext_def.directly_included,
        "quote-resolving-local must not flag the external twin as directly_included"
    );
    // Coloring: the external twin (not first-layer) is excluded from kind
    // counts; the local workspace definition alone counts.
    let counts = store.kind_counts_by_names(&["util_value"]).expect("counts");
    assert_eq!(counts["util_value"].get("function").copied(), Some(1));
}

#[test]
fn angle_external_include_flags_target_first_layer() {
    // A workspace file's `#include <stddef.h>` that resolves ExternalExact
    // to a configured-include-path stddef.h flags that external stddef.h as
    // `directly_included` (the same pass derives it from the edge kind).
    let ws = tempdir().expect("ws");
    fs::create_dir_all(ws.path().join("src")).expect("src");
    fs::write(
        ws.path().join("src/main.c"),
        "#include <stddef.h>\nint main(void){ size_t n = 0; return (int)n; }\n",
    )
    .expect("main");

    let ext = tempdir().expect("ext");
    fs::write(
        ext.path().join("stddef.h"),
        "typedef unsigned long size_t;\n",
    )
    .expect("ext stddef.h");
    let ext_root = ext.path().to_string_lossy().replace('\\', "/");
    let db = ws.path().join("index.sqlite");

    index_workspace(
        ws.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            include_paths: vec![ext_root],
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let store = IndexStore::open_readonly(&db).expect("readonly");
    let ext_path = format!(
        "{}/stddef.h",
        ext.path().to_string_lossy().replace('\\', "/")
    );
    let edges = store
        .load_include_edge_paths_with_resolution()
        .expect("edges");
    let kind = edges
        .iter()
        .find(|(_, dst, _)| dst == &ext_path)
        .map(|(_, _, kind)| kind.clone())
        .expect("edge to external stddef.h");
    assert_eq!(
        kind, "external_exact",
        "angle external include resolves ExternalExact; got {kind:?}"
    );
    // First-layer flag derives from the ExternalExact edge → size_t colors.
    let counts = store.kind_counts_by_names(&["size_t"]).expect("counts");
    assert_eq!(counts["size_t"].get("type").copied(), Some(1));
}
