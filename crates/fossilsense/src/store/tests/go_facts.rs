use super::*;
use crate::call_model::LinkageDomain;
use crate::semantic_model::{DeclarationBacking, SemanticLanguage};

#[test]
fn go_package_import_guard_and_language_round_trip_through_active_views() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    let source = r#"//go:build tinygo && arm

package sensor

import (
    device "example.com/board/device"
    _ "example.com/board/register"
)

func Read() {
    device.Open()
}
"#;
    upsert_source(&mut store, "src/sensor/read.go", source);

    let reader = IndexStore::open_readonly(&db).expect("readonly");
    let package = reader
        .package_import_view()
        .package_for_path("src/sensor/read.go")
        .expect("package read")
        .expect("package row");
    assert_eq!(package.name, "sensor");
    assert_eq!(package.build_guard.as_deref(), Some("tinygo && arm"));
    assert_eq!(package.path, "src/sensor/read.go");
    assert!(package.name_range.start_byte < package.name_range.end_byte);

    let (imports, imports_truncated) = reader
        .package_import_view()
        .imports_for_path("src/sensor/read.go", 8)
        .expect("imports");
    assert!(!imports_truncated);
    assert_eq!(imports.len(), 2);
    assert_eq!(imports[0].alias.as_deref(), Some("device"));
    assert_eq!(imports[0].import_path, "example.com/board/device");
    assert_eq!(imports[1].alias.as_deref(), Some("_"));
    assert_eq!(imports[1].import_path, "example.com/board/register");
    assert!(imports
        .iter()
        .all(|import| import.path_range.start_byte < import.path_range.end_byte));

    let declaration = reader
        .declarations_by_name("Read")
        .expect("declaration")
        .into_iter()
        .next()
        .expect("Read declaration");
    assert_eq!(declaration.fact.identity.language, SemanticLanguage::Go);
    assert_eq!(
        declaration.fact.identity.logical_key.linkage_domain,
        "package:src/sensor#sensor"
    );
    assert_eq!(declaration.fact.guard.as_deref(), Some("tinygo && arm"));
}

#[test]
fn go_defined_type_source_range_backing_round_trips_the_name_range() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    upsert_source(
        &mut store,
        "src/model/types.go",
        "package model\n\ntype UserID string\n",
    );

    let reader = IndexStore::open_readonly(&db).expect("readonly");
    let declaration = reader
        .declarations_by_name("UserID")
        .expect("UserID declarations")
        .into_iter()
        .next()
        .expect("UserID declaration");
    assert_ne!(
        declaration.fact.name_range, declaration.fact.declaration_range,
        "the fixture must distinguish the name from the whole declaration"
    );
    let DeclarationBacking::SourceRange { range } = declaration.fact.backing else {
        panic!("defined Go types use source-range backing");
    };
    assert_eq!(range, declaration.fact.name_range);
}

#[test]
fn go_import_reads_are_bounded_with_limit_plus_one() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    upsert_source(
        &mut store,
        "src/imports.go",
        "package imports\nimport (\n\"one\"\n\"two\"\n\"three\"\n)\n",
    );

    let reader = IndexStore::open_readonly(&db).expect("readonly");
    let (imports, truncated) = reader
        .package_import_view()
        .imports_for_path("src/imports.go", 2)
        .expect("bounded imports");
    assert_eq!(imports.len(), 2);
    assert!(truncated);

    let (none, truncated) = reader
        .package_import_view()
        .imports_for_path("src/imports.go", 0)
        .expect("zero imports");
    assert!(none.is_empty());
    assert!(!truncated);
}

#[test]
fn compact_name_and_fallback_rows_keep_their_semantic_family() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    upsert_source(
        &mut store,
        "src/shared.c",
        "int SharedOpen(void) { return 1; }\n",
    );
    upsert_source(
        &mut store,
        "src/shared.go",
        "package shared\nfunc SharedOpen() int { return 1 }\n",
    );
    upsert_source(
        &mut store,
        "src/broken.go",
        "package broken\nfunc Broken(value int {\n",
    );

    let reader = IndexStore::open_readonly(&db).expect("readonly");
    let rows = reader.declaration_name_rows().expect("name rows");
    assert!(rows.iter().any(|row| {
        row.path == "src/shared.c" && row.semantic_family == crate::config::SemanticFamily::CFamily
    }));
    assert!(rows.iter().any(|row| {
        row.path == "src/shared.go" && row.semantic_family == crate::config::SemanticFamily::Go
    }));
    let (go_rows, go_truncated) = reader
        .declaration_view()
        .by_name_family_limited("SharedOpen", crate::config::SemanticFamily::Go, 1)
        .expect("bounded Go declarations");
    assert_eq!(go_rows.len(), 1);
    assert_eq!(go_rows[0].fact.identity.language, SemanticLanguage::Go);
    assert!(!go_truncated);
    let (c_rows, c_truncated) = reader
        .declaration_view()
        .by_name_family_limited("SharedOpen", crate::config::SemanticFamily::CFamily, 1)
        .expect("bounded C-family declarations");
    assert_eq!(c_rows.len(), 1);
    assert_eq!(c_rows[0].fact.identity.language, SemanticLanguage::C);
    assert!(!c_truncated);
    assert!(reader
        .fallback_completion_view()
        .all()
        .expect("fallback rows")
        .iter()
        .all(|row| row.semantic_family == crate::config::SemanticFamily::Go));
}

#[test]
fn staged_go_package_facts_are_invisible_until_generation_commit() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    upsert_source(&mut store, "src/old.go", "package old\nvar Value = 1\n");

    let source = "package fresh\nvar Value = 2\n";
    let parsed = parse(std::path::Path::new("src/fresh.go"), source);
    let fingerprint = FileFingerprint {
        path: "src/fresh.go".to_string(),
        extension: "go".to_string(),
        size: source.len() as u64,
        mtime_ns: 2,
        hash: "fresh-hash".to_string(),
    };
    let build = store.begin_index_build(false).expect("build");
    store
        .stage_file_updates(
            build,
            &[super::super::FileIndexUpdate {
                fingerprint: &fingerprint,
                source: FileSource::Workspace,
                payload: super::super::FileIndexPayload::Ok(&parsed),
            }],
        )
        .expect("stage");

    assert!(store
        .package_import_view()
        .package_for_path("src/fresh.go")
        .expect("package before commit")
        .is_none());

    store
        .commit_index_build(build, &Default::default())
        .expect("commit");
    assert_eq!(
        store
            .package_import_view()
            .package_for_path("src/fresh.go")
            .expect("package after commit")
            .expect("package row")
            .name,
        "fresh"
    );
}

#[test]
fn go_package_linkage_and_split_file_methods_survive_storage_hydration() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    upsert_source(
        &mut store,
        "src/sensor/type.go",
        "package sensor\ntype Sample struct { Value int }\nfunc Open(path string) {}\n",
    );
    upsert_source(
        &mut store,
        "src/sensor/read.go",
        "package sensor\nfunc (sample *Sample) Read() int { return sample.Value }\nfunc Open(name string) {}\n",
    );

    let reader = IndexStore::open_readonly(&db).expect("readonly");
    let opens = reader.declarations_by_name("Open").expect("Open rows");
    assert_eq!(opens.len(), 2);
    assert_eq!(
        opens[0].fact.identity.logical_key,
        opens[1].fact.identity.logical_key
    );
    assert_eq!(
        opens[0].logical_key_digest, opens[1].logical_key_digest,
        "SQLite hydration must preserve the shared Go package identity"
    );
    let package_key = match &opens[0].fact.linkage {
        LinkageDomain::Package(package_key) => package_key.clone(),
        linkage => panic!("expected package linkage, got {linkage:?}"),
    };
    assert!(opens.iter().all(|row| {
        matches!(
            &row.fact.linkage,
            LinkageDomain::Package(candidate) if candidate == &package_key
        )
    }));
    let anchors = test_anchors_by_name(&reader, "Open");
    assert_eq!(anchors.len(), 2);
    assert!(anchors.iter().all(|anchor| {
        anchor.linkage_kind == "package"
            && anchor.linkage_file.as_deref() == Some(package_key.as_str())
    }));

    let records = reader
        .member_view()
        .resolve_record_candidates(&["Sample"], None)
        .expect("Sample record");
    let record_ids: Vec<_> = records.iter().map(|record| record.id).collect();
    let members = reader
        .member_view()
        .members_for_records(&record_ids, None, None)
        .expect("members");
    let method = members
        .iter()
        .find(|member| member.name == "Read")
        .expect("split-file method");
    assert_eq!(method.owner_path, "src/sensor/read.go");
}

#[test]
fn distinct_go_init_functions_keep_distinct_logical_keys_after_hydration() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    upsert_source(
        &mut store,
        "src/sensor/init_a.go",
        "package sensor\nfunc init() {}\n",
    );
    upsert_source(
        &mut store,
        "src/sensor/init_b.go",
        "package sensor\nfunc init() {}\n",
    );

    let reader = IndexStore::open_readonly(&db).expect("readonly");
    let declarations = reader.declarations_by_name("init").expect("init rows");
    assert_eq!(declarations.len(), 2);
    assert_ne!(
        declarations[0].fact.identity.logical_key,
        declarations[1].fact.identity.logical_key
    );
    assert_ne!(
        declarations[0].logical_key_digest,
        declarations[1].logical_key_digest
    );
}

#[test]
fn go_build_guard_is_revision_scoped_even_without_a_package_fact() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    upsert_source(
        &mut store,
        "src/broken.go",
        "//go:build tinygo\n\nfunc Broken( {\n",
    );

    assert_eq!(
        store
            .package_import_view()
            .build_guard_for_path("src/broken.go")
            .expect("guard"),
        Some("tinygo".to_string())
    );
    assert!(store
        .package_import_view()
        .package_for_path("src/broken.go")
        .expect("package")
        .is_none());
}
