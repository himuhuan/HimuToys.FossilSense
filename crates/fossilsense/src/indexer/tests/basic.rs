use super::*;

#[test]
fn indexes_mini_workspace_and_skips_unchanged_files() {
    let dir = tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("src")).expect("src");
    fs::create_dir_all(dir.path().join("include")).expect("include");
    fs::create_dir_all(dir.path().join("target")).expect("target");
    fs::write(
        dir.path().join("src/main.c"),
        "int main(void) { return 0; }\n",
    )
    .expect("main");
    fs::write(
        dir.path().join("include/hello.h"),
        "int hello_value(void);\n",
    )
    .expect("header");
    fs::write(
        dir.path().join("target/generated.c"),
        "int ignored(void);\n",
    )
    .expect("generated");
    let db = dir.path().join("index.sqlite");

    let first = index_workspace(
        dir.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            force: false,
            ..Default::default()
        },
        |_| {},
    )
    .expect("first index");

    assert_eq!(first.total_files, 2);
    assert_eq!(first.indexed_files, 2);
    assert!(first.symbols >= 2);

    let second = index_workspace(
        dir.path(),
        IndexOptions {
            db_path: Some(db),
            force: false,
            ..Default::default()
        },
        |_| {},
    )
    .expect("second index");

    assert_eq!(second.total_files, 2);
    assert_eq!(second.indexed_files, 0);
    assert_eq!(second.skipped_files, 2);
}

#[test]
fn dirty_file_update_reindexes_only_changed_file() {
    let dir = tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("src")).expect("src");
    let source_path = dir.path().join("src/main.c");
    fs::write(&source_path, "int old_name(void) { return 1; }\n").expect("write old");
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

    fs::write(&source_path, "int new_name(void) { return 2; }\n").expect("write new");
    let stats = index_dirty_files(
        dir.path(),
        vec![DirtyFileChange {
            absolute_path: source_path,
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

    assert_eq!(stats.total_files, 1);
    assert_eq!(stats.indexed_files, 1);
    assert_eq!(stats.skipped_files, 0);
    assert_eq!(stats.deleted_files, 0);
    assert_eq!(stats.discover_ms, 0);

    let store = IndexStore::open_readonly(&db).expect("store");
    assert!(store
        .symbols_by_name("old_name")
        .expect("old symbols")
        .is_empty());
    assert_eq!(
        store
            .symbols_by_name("new_name")
            .expect("new symbols")
            .len(),
        1
    );
}

#[test]
fn respects_fossilsense_json_include() {
    let dir = tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("src")).expect("src");
    fs::create_dir_all(dir.path().join("third_party")).expect("third_party");
    fs::write(
        dir.path().join("fossilsense.json"),
        r#"{"include": ["src/"]}"#,
    )
    .expect("config");
    fs::write(
        dir.path().join("src/main.c"),
        "int main(void) { return 0; }\n",
    )
    .expect("main");
    fs::write(
        dir.path().join("third_party/foo.c"),
        "int foo(void) { return 0; }\n",
    )
    .expect("foo");
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

    assert_eq!(stats.total_files, 1, "only src/ should be included");
}

#[test]
fn indexes_external_headers_and_marks_first_layer() {
    use crate::pathing;
    use crate::store::IndexStore;

    // Workspace directly includes ext.h; ext.h transitively includes deep.h.
    let ws = tempdir().expect("ws");
    fs::create_dir_all(ws.path().join("src")).expect("src");
    fs::write(
        ws.path().join("src/main.c"),
        "#include <ext.h>\nint main(void){ size_t n = 0; struct ExtType e; return (int)n; }\n",
    )
    .expect("main");

    let ext = tempdir().expect("ext");
    fs::write(
        ext.path().join("ext.h"),
        "#include <deep.h>\ntypedef unsigned long size_t;\nstruct ExtType { int a; };\n",
    )
    .expect("ext.h");
    fs::write(ext.path().join("deep.h"), "typedef int DeepType;\n").expect("deep.h");

    let ext_root = ext.path().to_string_lossy().replace('\\', "/");
    let db = ws.path().join("index.sqlite");

    let stats = index_workspace(
        ws.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            include_paths: vec![ext_root],
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    // Workspace file + both external headers are indexed.
    assert_eq!(stats.total_files, 3);

    let store = IndexStore::open_readonly(&db).expect("readonly");
    assert!(store.external_symbol_count().expect("ext count") > 0);

    // ext.h is first-layer (directly included) → its defs color.
    let first = store
        .kind_counts_by_names(&["size_t", "ExtType"])
        .expect("first");
    assert!(
        first.contains_key("size_t"),
        "size_t should color (first layer)"
    );
    assert!(
        first.contains_key("ExtType"),
        "ExtType should color (first layer)"
    );

    // deep.h is transitively included only → excluded from coloring.
    let deep = store.kind_counts_by_names(&["DeepType"]).expect("deep");
    assert!(
        !deep.contains_key("DeepType"),
        "transitive header must not color"
    );

    // size_t resolves to an external definition with an absolute path.
    let defs = store.symbols_by_name("size_t").expect("size_t defs");
    assert!(defs.iter().any(|r| r.source == "external"));
    assert!(defs.iter().all(|r| r.path.contains('/')));
    let _ = pathing::normalize_abs_path(ext.path());
}

#[test]
fn external_root_over_cap_indexes_no_symbols() {
    use crate::store::IndexStore;

    let ws = tempdir().expect("ws");
    fs::write(ws.path().join("main.c"), "int main(void){return 0;}\n").expect("main");

    // Three external headers, but a one-file cap forces path-only mode.
    let ext = tempdir().expect("ext");
    for name in ["a.h", "b.h", "c.h"] {
        fs::write(ext.path().join(name), "typedef int t;\n").expect("hdr");
    }
    let ext_root = ext.path().to_string_lossy().replace('\\', "/");
    let db = ws.path().join("index.sqlite");

    index_workspace(
        ws.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            include_paths: vec![ext_root],
            external_max_files: Some(1),
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let store = IndexStore::open_readonly(&db).expect("readonly");
    // Over-cap root contributes no symbols; path resolution still works on disk.
    assert_eq!(store.external_symbol_count().expect("ext count"), 0);
}

#[test]
fn missing_include_path_is_not_fatal() {
    let ws = tempdir().expect("ws");
    fs::write(ws.path().join("main.c"), "int main(void){return 0;}\n").expect("main");
    let db = ws.path().join("index.sqlite");

    // A non-existent include path must be skipped, not fail the index.
    let stats = index_workspace(
        ws.path(),
        IndexOptions {
            db_path: Some(db),
            include_paths: vec!["Z:/definitely/missing/include".to_string()],
            ..Default::default()
        },
        |_| {},
    )
    .expect("index should still succeed");
    assert_eq!(stats.total_files, 1);
}
