use super::*;
use crate::store::test_support::{
    hold_external_wal_writer, inspect_explicit_replacement, install_old_revision_cleanup_guard,
};

#[test]
fn protobuf_c_configuration_merges_project_and_editor_paths() {
    let ws = tempdir().expect("workspace");
    let project_proto = ws.path().join("proto");
    fs::create_dir_all(&project_proto).expect("project proto");
    let external = tempdir().expect("external proto");
    fs::write(
        ws.path().join("fossilsense.json"),
        r#"{"protobufC":{"enabled":true,"protoPaths":["proto"]}}"#,
    )
    .expect("config");

    let external_path = crate::pathing::normalize_abs_path(external.path());
    let prepared = crate::indexer::prepare_index_configuration(
        ws.path(),
        &[],
        &[],
        None,
        std::slice::from_ref(&external_path),
    )
    .expect("prepare");

    assert!(prepared.protobuf_c_enabled);
    assert_eq!(prepared.proto_roots.len(), 2);
    let canonical_external = external.path().canonicalize().expect("canonical external");
    let canonical_project = project_proto
        .canonicalize()
        .expect("canonical project proto");
    assert!(prepared
        .proto_roots
        .iter()
        .any(|root| root == &canonical_external));
    assert!(prepared
        .proto_roots
        .iter()
        .any(|root| root == &canonical_project));
}

#[test]
fn explicit_editor_disable_avoids_proto_path_validation() {
    let ws = tempdir().expect("workspace");
    fs::write(
        ws.path().join("fossilsense.json"),
        r#"{"protobufC":{"enabled":true,"protoPaths":["missing-proto"]}}"#,
    )
    .expect("config");

    let prepared =
        crate::indexer::prepare_index_configuration(ws.path(), &[], &[], Some(false), &[])
            .expect("prepare disabled");

    assert!(!prepared.protobuf_c_enabled);
    assert!(prepared.proto_roots.is_empty());
    assert!(!prepared
        .issues
        .iter()
        .any(|issue| issue.message.contains("protobufC")));
}

#[test]
fn enabled_protobuf_c_without_paths_reports_inactive_reason() {
    let ws = tempdir().expect("workspace");
    let prepared =
        crate::indexer::prepare_index_configuration(ws.path(), &[], &[], Some(true), &[])
            .expect("prepare enabled");

    assert!(prepared.protobuf_c_enabled);
    assert!(prepared.proto_roots.is_empty());
    assert!(prepared
        .issues
        .iter()
        .any(|issue| issue.message.contains("no valid proto directories")));
}

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
    assert!(first.declarations >= 2);
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
fn indexer_uses_language_overrides_for_header_declaration_metadata() {
    let dir = tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("legacy")).expect("legacy");
    fs::write(
        dir.path().join("fossilsense.json"),
        r#"{
          "languageOverrides": [
            { "glob": "**/*.h", "language": "cpp" },
            { "glob": "legacy/**/*.h", "language": "c" }
          ]
        }"#,
    )
    .expect("config");
    fs::write(dir.path().join("legacy/api.h"), "int legacy_object;\n").expect("legacy header");
    fs::write(dir.path().join("modern.h"), "int modern_object;\n").expect("modern header");
    let db = dir.path().join("index.sqlite");

    index_workspace(
        dir.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let store = IndexStore::open_readonly(&db).expect("store");
    let legacy = store
        .declarations_by_name("legacy_object")
        .expect("legacy declaration");
    let modern = store
        .declarations_by_name("modern_object")
        .expect("modern declaration");
    assert_eq!(legacy.len(), 1);
    assert_eq!(modern.len(), 1);
    assert_eq!(
        legacy[0].fact.identity.language,
        crate::semantic_model::SemanticLanguage::C
    );
    assert_eq!(
        legacy[0].fact.role,
        crate::semantic_model::SemanticDeclarationRole::TentativeDefinition
    );
    assert_eq!(
        modern[0].fact.identity.language,
        crate::semantic_model::SemanticLanguage::Cpp
    );
    assert_eq!(
        modern[0].fact.role,
        crate::semantic_model::SemanticDeclarationRole::Definition
    );
}

#[test]
fn incremental_index_reparses_unchanged_source_when_language_override_changes() {
    let dir = tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("legacy")).expect("legacy");
    let config_path = dir.path().join("fossilsense.json");
    let source_path = dir.path().join("legacy/api.h");
    fs::write(
        &config_path,
        r#"{"languageOverrides":[{"glob":"legacy/**/*.h","language":"cpp"}]}"#,
    )
    .expect("initial config");
    fs::write(&source_path, "int language_sensitive_object;\n").expect("header");
    let original_metadata = fs::metadata(&source_path).expect("source metadata");
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

    fs::write(
        &config_path,
        r#"{"languageOverrides":[{"glob":"legacy/**/*.h","language":"c"}]}"#,
    )
    .expect("updated config");
    let unchanged_metadata = fs::metadata(&source_path).expect("unchanged source metadata");
    assert_eq!(unchanged_metadata.len(), original_metadata.len());
    assert_eq!(
        unchanged_metadata.modified().expect("unchanged mtime"),
        original_metadata.modified().expect("original mtime")
    );

    let updated = index_workspace(
        dir.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            force: false,
            ..Default::default()
        },
        |_| {},
    )
    .expect("incremental index");
    assert_eq!(updated.indexed_files, 1);
    assert_eq!(updated.skipped_files, 0);

    let store = IndexStore::open_readonly(&db).expect("store");
    let declaration = store
        .declarations_by_name("language_sensitive_object")
        .expect("declaration");
    assert_eq!(declaration.len(), 1);
    assert_eq!(
        declaration[0].fact.identity.language,
        crate::semantic_model::SemanticLanguage::C
    );
    assert_eq!(
        declaration[0].fact.role,
        crate::semantic_model::SemanticDeclarationRole::TentativeDefinition
    );
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
            .declarations_by_name("first_generation")
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
            .declarations_by_name("first_generation")
            .expect("old snapshot remains readable")
            .len(),
        1
    );
    let new_reader = IndexStore::open_readonly(&second_path).expect("new reader");
    assert!(new_reader
        .declarations_by_name("first_generation")
        .expect("old symbol removed")
        .is_empty());
    assert_eq!(
        new_reader
            .declarations_by_name("second_generation")
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
            .declarations_by_name("recovered_generation")
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
fn explicit_force_rebuild_publishes_a_fresh_database_without_old_cleanup_debt() {
    let workspace = tempdir().expect("workspace");
    let source = workspace.path().join("main.c");
    let db = workspace.path().join("explicit.sqlite");
    fs::write(&source, "int first_generation(void) { return 1; }\n").expect("first source");

    let first = index_workspace(
        workspace.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("first explicit build");
    assert_eq!(first.semantic_generation, 1);

    install_old_revision_cleanup_guard(&db).expect("install old-database cleanup guard");

    fs::write(&source, "int second_generation(void) { return 2; }\n").expect("second source");
    let second = index_workspace(
        workspace.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("second explicit build");

    assert_eq!(second.semantic_generation, 2);
    assert_eq!(
        second.maintenance_warning, None,
        "a fresh explicit build must not inherit old cleanup failures"
    );
    let store = IndexStore::open_readonly(&db).expect("new explicit database");
    assert!(store
        .declarations_by_name("first_generation")
        .expect("old declaration")
        .is_empty());
    assert_eq!(
        store
            .declarations_by_name("second_generation")
            .expect("new declaration")
            .len(),
        1
    );
    drop(store);

    let replacement = inspect_explicit_replacement(&db).expect("inspect replaced database");
    assert_eq!(
        replacement.trigger_count, 0,
        "the old schema must not be copied"
    );
    assert_eq!(
        replacement.revision_count, 1,
        "the replacement must contain only the published generation"
    );
}

#[test]
fn explicit_force_rebuild_preserves_old_database_when_wal_cannot_be_drained() {
    let workspace = tempdir().expect("workspace");
    let source = workspace.path().join("main.c");
    let db = workspace.path().join("explicit.sqlite");
    fs::write(&source, "int first_generation(void) { return 1; }\n").expect("first source");
    index_workspace(
        workspace.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("first explicit build");

    let blocker = hold_external_wal_writer(&db).expect("hold external WAL writer");
    fs::write(&source, "int second_generation(void) { return 2; }\n").expect("second source");

    let error = index_workspace(
        workspace.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect_err("a live external WAL writer must block replacement");
    assert!(
        error.to_string().contains("locked") || error.to_string().contains("journal"),
        "unexpected WAL drain error: {error:#}"
    );
    blocker.release().expect("release external writer");

    let store = IndexStore::open_readonly(&db).expect("preserved old database");
    assert_eq!(store.semantic_generation().expect("old generation"), 1);
    assert_eq!(
        store
            .declarations_by_name("first_generation")
            .expect("old declaration")
            .len(),
        1
    );
    assert!(store
        .declarations_by_name("second_generation")
        .expect("unpublished declaration")
        .is_empty());
    drop(store);
    let staging_count = fs::read_dir(workspace.path())
        .expect("workspace entries")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".fossilsense-index-build-")
        })
        .count();
    assert_eq!(staging_count, 0, "failed staging must be reclaimed");
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
        .declarations_by_name("old_name")
        .expect("old symbols")
        .is_empty());
    assert_eq!(
        store
            .declarations_by_name("new_name")
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
    assert!(store.external_declaration_count().expect("ext count") > 0);

    // ext.h is first-layer (directly included) → its defs color.
    let first = store
        .declaration_kind_counts_by_names(&["size_t", "ExtType"])
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
    let deep = store
        .declaration_kind_counts_by_names(&["DeepType"])
        .expect("deep");
    assert!(
        !deep.contains_key("DeepType"),
        "transitive header must not color"
    );

    // size_t resolves to an external definition with an absolute path.
    let defs = store.declarations_by_name("size_t").expect("size_t defs");
    assert!(defs.iter().any(|r| r.external));
    assert!(defs.iter().all(|r| r.fact.path.contains('/')));
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
    assert_eq!(store.external_declaration_count().expect("ext count"), 0);
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
fn protobuf_c_sources_require_an_include_edge_and_store_nested_declaration_lines() {
    let workspace = tempdir().expect("workspace");
    fs::create_dir_all(workspace.path().join("src")).expect("src");
    fs::create_dir_all(workspace.path().join("proto/messages")).expect("proto");
    let include = tempdir().expect("generated include");
    fs::create_dir_all(include.path().join("messages")).expect("messages");

    fs::write(
        workspace.path().join("src/main.c"),
        "#include <wrapper.h>\nDemo__Outer__Inner *value;\n",
    )
    .expect("main");
    fs::write(
        include.path().join("wrapper.h"),
        "#include <messages/system_cfg.pb-c.h>\n",
    )
    .expect("wrapper");
    fs::write(
        include.path().join("messages/system_cfg.pb-c.h"),
        "typedef struct Demo__Outer Demo__Outer;\n\
         typedef struct Demo__Outer__Inner Demo__Outer__Inner;\n\
         typedef enum _Demo__Mode { DEMO__MODE__UNKNOWN = 0 } Demo__Mode;\n",
    )
    .expect("generated header");
    fs::write(
        include.path().join("messages/unused.pb-c.h"),
        "typedef struct Demo__Unused Demo__Unused;\n",
    )
    .expect("unused generated header");
    fs::write(
        workspace.path().join("proto/messages/system_cfg.proto"),
        "package demo;\nmessage Outer {\n  message Inner {}\n}\nenum Mode { UNKNOWN = 0; }\n",
    )
    .expect("proto");
    fs::write(
        workspace.path().join("proto/messages/unused.proto"),
        "package demo;\nmessage Unused {}\n",
    )
    .expect("unused proto");
    fs::write(
        workspace.path().join("fossilsense.json"),
        serde_json::json!({
            "includePaths": [crate::pathing::normalize_abs_path(include.path())],
            "protobufC": { "enabled": true, "protoPaths": ["proto"] }
        })
        .to_string(),
    )
    .expect("config");

    let db = workspace.path().join("index.sqlite");
    index_workspace(
        workspace.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let store = IndexStore::open_readonly(&db).expect("store");
    let nested_ids: Vec<_> = store
        .declarations_by_name("Demo__Outer__Inner")
        .expect("nested declarations")
        .into_iter()
        .map(|row| row.id)
        .collect();
    assert!(!nested_ids.is_empty(), "generated nested type is indexed");
    let (sources, truncated) = store
        .protobuf_c_source_view()
        .sources_for_declaration_ids(&nested_ids, 64)
        .expect("proto sources");
    assert!(!truncated);
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].proto_name, "demo.Outer.Inner");
    assert_eq!(sources[0].c_name, "Demo__Outer__Inner");
    assert_eq!(sources[0].kind, "message");
    assert_eq!(sources[0].start_line, 2);
    assert_eq!(sources[0].match_kind, "relative_path");

    let unused_ids: Vec<_> = store
        .declarations_by_name("Demo__Unused")
        .expect("unused declarations")
        .into_iter()
        .map(|row| row.id)
        .collect();
    assert!(
        !unused_ids.is_empty(),
        "unreferenced generated header is still indexed"
    );
    let (unused_sources, _) = store
        .protobuf_c_source_view()
        .sources_for_declaration_ids(&unused_ids, 64)
        .expect("unused proto sources");
    assert!(
        unused_sources.is_empty(),
        "a generated header without an incoming include edge must not be associated"
    );
}

#[test]
fn protobuf_c_relative_match_uses_the_parsed_include_target_for_workspace_headers() {
    let workspace = tempdir().expect("workspace");
    fs::create_dir_all(workspace.path().join("src")).expect("src");
    fs::create_dir_all(workspace.path().join("include/messages")).expect("generated include");
    fs::create_dir_all(workspace.path().join("common/messages")).expect("proto root");
    fs::write(
        workspace.path().join("src/main.c"),
        "#include \"../include/wrapper.h\"\nDemo__Device *device;\n",
    )
    .expect("main");
    fs::write(
        workspace.path().join("include/wrapper.h"),
        "#include \"messages/device.pb-c.h\"\n",
    )
    .expect("wrapper");
    fs::write(
        workspace.path().join("include/messages/device.pb-c.h"),
        "typedef struct Demo__Device Demo__Device;\n",
    )
    .expect("generated header");
    fs::write(
        workspace.path().join("common/messages/device.proto"),
        "package demo;\nmessage Device {}\n",
    )
    .expect("proto");
    fs::write(
        workspace.path().join("fossilsense.json"),
        r#"{"protobufC":{"enabled":true,"protoPaths":["common"]}}"#,
    )
    .expect("config");

    let db = workspace.path().join("index.sqlite");
    index_workspace(
        workspace.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let store = IndexStore::open_readonly(&db).expect("store");
    let ids: Vec<_> = store
        .declarations_by_name("Demo__Device")
        .expect("generated type")
        .into_iter()
        .map(|row| row.id)
        .collect();
    let (sources, truncated) = store
        .protobuf_c_source_view()
        .sources_for_declaration_ids(&ids, 64)
        .expect("sources");
    assert!(!truncated);
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].match_kind, "relative_path");
}

#[test]
fn protobuf_c_same_basename_include_edges_keep_strong_paths_on_their_own_headers() {
    let workspace = tempdir().expect("workspace");
    for directory in ["include/a", "include/b", "proto/a", "proto/b"] {
        fs::create_dir_all(workspace.path().join(directory)).expect("directory");
    }
    fs::write(
        workspace.path().join("main.c"),
        "#include \"include/wrapper.h\"\nDemo__Device *device;\n",
    )
    .expect("main");
    fs::write(
        workspace.path().join("include/wrapper.h"),
        "#include \"a/device.pb-c.h\"\n#include \"b/device.pb-c.h\"\n",
    )
    .expect("wrapper");
    for directory in ["a", "b"] {
        fs::write(
            workspace
                .path()
                .join(format!("include/{directory}/device.pb-c.h")),
            "typedef struct Demo__Device Demo__Device;\n",
        )
        .expect("generated header");
        fs::write(
            workspace
                .path()
                .join(format!("proto/{directory}/device.proto")),
            "package demo;\nmessage Device {}\n",
        )
        .expect("proto");
    }
    fs::write(
        workspace.path().join("fossilsense.json"),
        r#"{"protobufC":{"enabled":true,"protoPaths":["proto"]}}"#,
    )
    .expect("config");

    let db = workspace.path().join("index.sqlite");
    index_workspace(
        workspace.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let store = IndexStore::open_readonly(&db).expect("store");
    let declarations = store
        .declarations_by_name("Demo__Device")
        .expect("generated types");
    for directory in ["a", "b"] {
        let ids: Vec<_> = declarations
            .iter()
            .filter(|row| row.fact.path == format!("include/{directory}/device.pb-c.h"))
            .map(|row| row.id)
            .collect();
        assert!(!ids.is_empty(), "generated declaration for {directory}");
        let (sources, truncated) = store
            .protobuf_c_source_view()
            .sources_for_declaration_ids(&ids, 64)
            .expect("sources");
        assert!(!truncated);
        assert_eq!(sources.len(), 1, "sources for header {directory}");
        assert!(
            sources[0]
                .proto_path
                .replace('\\', "/")
                .ends_with(&format!("/proto/{directory}/device.proto")),
            "header {directory} received the wrong strong source: {:?}",
            sources
        );
        assert_eq!(sources[0].match_kind, "relative_path");
    }
}

#[test]
fn protobuf_c_same_basename_candidates_are_stable_and_explicitly_truncated() {
    let workspace = tempdir().expect("workspace");
    fs::create_dir_all(workspace.path().join("generated")).expect("generated");
    fs::create_dir_all(workspace.path().join("proto")).expect("proto root");
    fs::write(
        workspace.path().join("main.c"),
        "#include \"generated/system_cfg.pb-c.h\"\nDemo__Item *item;\n",
    )
    .expect("main");
    fs::write(
        workspace.path().join("generated/system_cfg.pb-c.h"),
        "typedef struct Demo__Item Demo__Item;\n",
    )
    .expect("generated header");
    for index in 0..65 {
        let directory = workspace.path().join(format!("proto/candidate_{index:02}"));
        fs::create_dir_all(&directory).expect("candidate directory");
        fs::write(
            directory.join("system_cfg.proto"),
            "package demo;\nmessage Item {}\n",
        )
        .expect("proto candidate");
    }
    fs::write(
        workspace.path().join("fossilsense.json"),
        r#"{"protobufC":{"enabled":true,"protoPaths":["proto"]}}"#,
    )
    .expect("config");

    let db = workspace.path().join("index.sqlite");
    index_workspace(
        workspace.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");
    let store = IndexStore::open_readonly(&db).expect("store");
    let ids: Vec<_> = store
        .declarations_by_name("Demo__Item")
        .expect("generated type")
        .into_iter()
        .map(|row| row.id)
        .collect();
    let (sources, truncated) = store
        .protobuf_c_source_view()
        .sources_for_declaration_ids(&ids, 64)
        .expect("sources");

    assert!(truncated);
    assert_eq!(sources.len(), 64);
    assert!(sources
        .iter()
        .all(|source| source.match_kind == "same_basename"));
    assert!(sources.windows(2).all(|pair| {
        pair[0].proto_path.to_ascii_lowercase() <= pair[1].proto_path.to_ascii_lowercase()
    }));
}

#[test]
fn protobuf_c_repeated_declarations_are_bounded_before_association_publication() {
    let workspace = tempdir().expect("workspace");
    fs::create_dir_all(workspace.path().join("generated")).expect("generated");
    fs::create_dir_all(workspace.path().join("proto")).expect("proto");
    fs::write(
        workspace.path().join("main.c"),
        "#include \"generated/device.pb-c.h\"\nDemo__Device *device;\n",
    )
    .expect("main");
    fs::write(
        workspace.path().join("generated/device.pb-c.h"),
        "typedef struct Demo__Device Demo__Device;\n",
    )
    .expect("generated header");
    let proto = "package demo;\n".to_string() + &"message Device {}\n".repeat(100);
    fs::write(workspace.path().join("proto/device.proto"), proto).expect("proto");
    fs::write(
        workspace.path().join("fossilsense.json"),
        r#"{"protobufC":{"enabled":true,"protoPaths":["proto"]}}"#,
    )
    .expect("config");

    let db = workspace.path().join("index.sqlite");
    index_workspace(
        workspace.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");
    let store = IndexStore::open_readonly(&db).expect("store");
    let ids: Vec<_> = store
        .declarations_by_name("Demo__Device")
        .expect("generated type")
        .into_iter()
        .map(|row| row.id)
        .collect();
    let (sources, truncated) = store
        .protobuf_c_source_view()
        .sources_for_declaration_ids(&ids, 64)
        .expect("sources");

    assert!(truncated);
    assert_eq!(sources.len(), 64);
}

#[test]
fn protobuf_c_source_changes_become_visible_on_the_next_reindex() {
    let workspace = tempdir().expect("workspace");
    fs::create_dir_all(workspace.path().join("generated")).expect("generated");
    fs::create_dir_all(workspace.path().join("proto")).expect("proto");
    fs::write(
        workspace.path().join("main.c"),
        "#include \"generated/device.pb-c.h\"\nDemo__Device *device;\n",
    )
    .expect("main");
    fs::write(
        workspace.path().join("generated/device.pb-c.h"),
        "typedef struct Demo__Device Demo__Device;\n",
    )
    .expect("generated header");
    let proto = workspace.path().join("proto/device.proto");
    fs::write(&proto, "package demo;\nmessage Device {}\n").expect("proto");
    fs::write(
        workspace.path().join("fossilsense.json"),
        r#"{"protobufC":{"enabled":true,"protoPaths":["proto"]}}"#,
    )
    .expect("config");

    let db = workspace.path().join("index.sqlite");
    let first = index_workspace(
        workspace.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("first index");
    let declaration_ids = {
        let store = IndexStore::open_readonly(&db).expect("store");
        let ids: Vec<_> = store
            .declarations_by_name("Demo__Device")
            .expect("generated type")
            .into_iter()
            .map(|row| row.id)
            .collect();
        let (sources, _) = store
            .protobuf_c_source_view()
            .sources_for_declaration_ids(&ids, 64)
            .expect("first sources");
        assert_eq!(sources[0].start_line, 1);
        ids
    };

    fs::write(
        &proto,
        "// generated source note\npackage demo;\n\nmessage Device {}\n",
    )
    .expect("updated proto");
    let second = index_workspace(
        workspace.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            ..Default::default()
        },
        |_| {},
    )
    .expect("second index");
    assert!(second.semantic_generation > first.semantic_generation);

    let store = IndexStore::open_readonly(&db).expect("updated store");
    let updated_ids: Vec<_> = store
        .declarations_by_name("Demo__Device")
        .expect("stable generated type")
        .into_iter()
        .map(|row| row.id)
        .collect();
    assert_eq!(updated_ids, declaration_ids);
    let (sources, truncated) = store
        .protobuf_c_source_view()
        .sources_for_declaration_ids(&updated_ids, 64)
        .expect("updated sources");
    assert!(!truncated);
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].start_line, 3);
}

#[test]
fn protobuf_c_over_cap_root_is_reported_and_other_roots_continue() {
    let workspace = tempdir().expect("workspace");
    fs::create_dir_all(workspace.path().join("generated")).expect("generated");
    fs::create_dir_all(workspace.path().join("proto-over-cap")).expect("over cap root");
    fs::create_dir_all(workspace.path().join("proto-valid")).expect("valid root");
    fs::write(
        workspace.path().join("main.c"),
        "#include \"generated/device.pb-c.h\"\nDemo__Device *device;\n",
    )
    .expect("main");
    fs::write(
        workspace.path().join("generated/device.pb-c.h"),
        "typedef struct Demo__Device Demo__Device;\n",
    )
    .expect("generated header");
    fs::write(
        workspace.path().join("proto-over-cap/device.proto"),
        "package demo; message Device {}\n",
    )
    .expect("over-cap proto");
    fs::write(
        workspace.path().join("proto-over-cap/extra.txt"),
        "counts toward the per-root scan cap\n",
    )
    .expect("extra file");
    fs::write(
        workspace.path().join("proto-valid/device.proto"),
        "package demo;\nmessage Device {}\n",
    )
    .expect("valid proto");
    fs::write(
        workspace.path().join("fossilsense.json"),
        r#"{"protobufC":{"enabled":true,"protoPaths":["proto-over-cap","proto-valid"]}}"#,
    )
    .expect("config");

    let db = workspace.path().join("index.sqlite");
    let mut messages = Vec::new();
    index_workspace(
        workspace.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            force: true,
            external_max_files: Some(1),
            ..Default::default()
        },
        |status| messages.extend(status.message),
    )
    .expect("index");

    assert!(messages
        .iter()
        .any(|message| { message.contains("exceeds cap") && message.contains("proto-over-cap") }));
    let store = IndexStore::open_readonly(&db).expect("store");
    let ids: Vec<_> = store
        .declarations_by_name("Demo__Device")
        .expect("generated type")
        .into_iter()
        .map(|row| row.id)
        .collect();
    let (sources, truncated) = store
        .protobuf_c_source_view()
        .sources_for_declaration_ids(&ids, 64)
        .expect("sources");
    assert!(!truncated);
    assert_eq!(sources.len(), 1);
    assert!(sources[0].proto_path.contains("proto-valid"));
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
    assert_eq!(stats.declarations, 300);
}
