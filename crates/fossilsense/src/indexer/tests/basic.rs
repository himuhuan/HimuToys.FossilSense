use super::*;

#[test]
fn indexes_mini_workspace_and_skips_unchanged_files() {
    let dir = tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("src")).expect("src");
    fs::create_dir_all(dir.path().join("include")).expect("include");
    fs::create_dir_all(dir.path().join("target")).expect("target");
    fs::write(
        dir.path().join("src/main.c"),
        "int main(void) { return hello_value(); }\n",
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
    assert_eq!(first.callable_anchors, 2);
    assert_eq!(first.call_sites, 1);

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
    assert_eq!(second.callable_anchors, 2);
    assert_eq!(second.call_sites, 1);
}

#[test]
fn default_full_rebuild_publishes_side_by_side_and_preserves_old_reader() {
    let workspace = tempdir().expect("workspace");
    let source = workspace.path().join("main.c");
    fs::write(&source, "int first_generation(void) { return 1; }\n").expect("first source");
    let cache_dir = crate::pathing::default_index_directory(workspace.path()).expect("cache dir");
    if cache_dir.exists() {
        fs::remove_dir_all(&cache_dir).expect("clear unique test cache");
    }

    let first = index_workspace(
        workspace.path(),
        IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("first side-by-side build");
    assert_eq!(first.semantic_generation, 1);
    let first_path = crate::pathing::default_index_path(workspace.path()).expect("first active");
    assert!(first_path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("index-g1-"));
    let old_reader = IndexStore::open_readonly(&first_path).expect("old reader");
    assert_eq!(
        old_reader
            .symbols_by_name("first_generation")
            .expect("first symbol")
            .len(),
        1
    );

    fs::write(&source, "int second_generation(void) { return 2; }\n").expect("second source");
    let second = index_workspace(
        workspace.path(),
        IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("second side-by-side build");
    assert_eq!(second.semantic_generation, 2);
    let second_path = crate::pathing::default_index_path(workspace.path()).expect("second active");
    assert_ne!(first_path, second_path);
    assert!(
        first_path.is_file(),
        "old generation must remain leased by path"
    );

    assert_eq!(
        old_reader
            .symbols_by_name("first_generation")
            .expect("old snapshot remains readable")
            .len(),
        1
    );
    let new_reader = IndexStore::open_readonly(&second_path).expect("new reader");
    assert!(new_reader
        .symbols_by_name("first_generation")
        .expect("old symbol removed")
        .is_empty());
    assert_eq!(
        new_reader
            .symbols_by_name("second_generation")
            .expect("new symbol")
            .len(),
        1
    );

    fs::write(cache_dir.join("active-index"), "../broken.sqlite\n").expect("corrupt manifest");
    fs::write(&source, "int recovered_generation(void) { return 3; }\n").expect("recovery source");
    let recovered = index_workspace(
        workspace.path(),
        IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("force rebuild recovers manifest");
    assert_eq!(recovered.semantic_generation, 3);
    let recovered_path =
        crate::pathing::default_index_path(workspace.path()).expect("recovered active");
    assert_ne!(recovered_path, second_path);
    let recovered_reader = IndexStore::open_readonly(&recovered_path).expect("recovered reader");
    assert_eq!(
        recovered_reader
            .symbols_by_name("recovered_generation")
            .expect("recovered symbol")
            .len(),
        1
    );

    drop(recovered_reader);
    drop(new_reader);
    drop(old_reader);
    fs::remove_dir_all(cache_dir).expect("clean unique test cache");
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
    assert_eq!(stats.callable_anchors, 1);
    assert_eq!(stats.call_sites, 0);

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
        "#include <deep.h>\ntypedef unsigned long size_t;\nstruct ExtType { int a; };\nint external_inline(void) { return deep_value(); }\n",
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
    assert_eq!(stats.call_sites, 0, "external bodies are navigation leaves");

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

#[test]
fn bounded_parse_write_pipeline_crosses_multiple_batches() {
    let ws = tempdir().expect("ws");
    for index in 0..300 {
        fs::write(
            ws.path().join(format!("file_{index:03}.c")),
            format!("int function_{index:03}(void) {{ return {index}; }}\n"),
        )
        .expect("source");
    }
    let db = ws.path().join("index.sqlite");
    let stats = index_workspace(
        ws.path(),
        IndexOptions {
            db_path: Some(db),
            force: true,
            parse_threads: Some(2),
            ..Default::default()
        },
        |_| {},
    )
    .expect("bounded pipeline index");

    assert_eq!(stats.indexed_files, 300);
    assert_eq!(stats.total_files, 300);
    assert_eq!(stats.symbols, 300);
}
