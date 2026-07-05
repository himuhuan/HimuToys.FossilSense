use super::*;

#[test]
fn test_store_schema_v6() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");

    // 1. Open the store to create the schema
    {
        let _store = IndexStore::open(&db, dir.path()).expect("store");
    }

    // 2. Open read-only and query sqlite_master to verify tables exist
    let reader = IndexStore::open_readonly(&db).expect("readonly");
    let tables: Vec<String> = reader
        .conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert!(tables.contains(&"record_defs".to_string()));
    assert!(tables.contains(&"members".to_string()));
    assert!(tables.contains(&"type_aliases".to_string()));

    // 3. Deleting a file cascades its records, members, and aliases
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    let source = "struct Foo { int a; };\ntypedef struct Foo FooT;\n";
    let index = parse(std::path::Path::new("foo.h"), source);
    let fingerprint = FileFingerprint {
        path: "foo.h".to_string(),
        extension: "h".to_string(),
        size: source.len() as u64,
        mtime_ns: 1,
        hash: "abc".to_string(),
    };
    store
        .upsert_file_index(&fingerprint, &index)
        .expect("upsert");

    // Verify they are inserted
    let records = store
        .resolve_record_candidates(&["Foo"], None)
        .expect("records");
    assert_eq!(records.len(), 1);
    let members = store
        .members_for_records(&[records[0].id], None, None)
        .expect("members");
    assert_eq!(members.len(), 1);

    // Delete the file
    store
        .delete_missing_files(&Default::default())
        .expect("delete missing");

    // Verify they are gone (cascaded)
    let records_after = store
        .resolve_record_candidates(&["Foo"], None)
        .expect("records");
    assert!(records_after.is_empty());
}

#[test]
fn test_store_write_extended() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");

    // same-named records in two files get distinct ids
    let src1 = "struct W { int field1; };\n";
    let index1 = parse(std::path::Path::new("file1.h"), src1);
    let fp1 = FileFingerprint {
        path: "file1.h".to_string(),
        extension: "h".to_string(),
        size: src1.len() as u64,
        mtime_ns: 1,
        hash: "abc".to_string(),
    };
    store.upsert_file_index(&fp1, &index1).expect("upsert");

    let src2 = "struct W { int field2; };\n";
    let index2 = parse(std::path::Path::new("file2.h"), src2);
    let fp2 = FileFingerprint {
        path: "file2.h".to_string(),
        extension: "h".to_string(),
        size: src2.len() as u64,
        mtime_ns: 2,
        hash: "def".to_string(),
    };
    store.upsert_file_index(&fp2, &index2).expect("upsert");

    let records = store
        .resolve_record_candidates(&["W"], None)
        .expect("records");
    assert_eq!(records.len(), 2);
    assert_ne!(records[0].id, records[1].id);

    // fields point at the correct id
    let fields0 = store.fields_for_records(&[records[0].id]).expect("fields0");
    let fields1 = store.fields_for_records(&[records[1].id]).expect("fields1");
    if records[0].path == "file1.h" {
        assert_eq!(fields0, vec!["field1".to_string()]);
        assert_eq!(fields1, vec!["field2".to_string()]);
    } else {
        assert_eq!(fields0, vec!["field2".to_string()]);
        assert_eq!(fields1, vec!["field1".to_string()]);
    }

    // anonymous typedef fields point at the typedef display record
    let src3 = "typedef struct { int len; } Buffer;\n";
    let index3 = parse(std::path::Path::new("file3.h"), src3);
    let fp3 = FileFingerprint {
        path: "file3.h".to_string(),
        extension: "h".to_string(),
        size: src3.len() as u64,
        mtime_ns: 3,
        hash: "ghi".to_string(),
    };
    store.upsert_file_index(&fp3, &index3).expect("upsert");

    let records3 = store
        .resolve_record_candidates(&["Buffer"], None)
        .expect("records3");
    assert_eq!(records3.len(), 1);
    assert_eq!(records3[0].display_name, "Buffer");
    let fields3 = store
        .fields_for_records(&[records3[0].id])
        .expect("fields3");
    assert_eq!(fields3, vec!["len".to_string()]);
}

#[test]
fn alias_target_kind_filters_same_tag_records() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");

    upsert_source(
        &mut store,
        "struct_foo.h",
        "struct Foo { int struct_field; };\n",
    );
    upsert_source(
        &mut store,
        "union_foo.h",
        "union Foo { int union_field; };\n",
    );
    upsert_source(&mut store, "alias.h", "typedef union Foo FooU;\n");

    let candidates = store
        .resolve_record_candidates(&["FooU"], None)
        .expect("candidates");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].kind, crate::parser::RecordKind::Union);

    let fields = store
        .fields_for_records(&[candidates[0].id])
        .expect("fields");
    assert_eq!(fields, vec!["union_field".to_string()]);
}
