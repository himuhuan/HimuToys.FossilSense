use super::*;
use crate::semantic_model::{AliasTargetFidelity, DeclaratorShape, RecordRangeFidelity};

#[test]
fn test_store_schema_v16() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");

    // 1. Open the store to create the schema
    {
        let _store = IndexStore::open(&db, dir.path()).expect("store");
    }

    // 2. Active semantic names are views over immutable revision fact tables.
    let reader = IndexStore::open_readonly(&db).expect("readonly");
    let tables: Vec<String> = reader
        .conn
        .prepare("SELECT name FROM sqlite_master WHERE type IN ('table', 'view')")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert!(tables.contains(&"record_defs".to_string()));
    assert!(tables.contains(&"members".to_string()));
    assert!(tables.contains(&"type_aliases".to_string()));

    let record_columns: Vec<String> = reader
        .conn
        .prepare("PRAGMA table_info(record_facts)")
        .unwrap()
        .query_map([], |row| row.get(1))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(record_columns.contains(&"body_start_byte".to_string()));
    assert!(record_columns.contains(&"declaration_start_byte".to_string()));
    assert!(record_columns.contains(&"range_fidelity".to_string()));
    assert!(record_columns.contains(&"declaration_hash".to_string()));

    let alias_columns: Vec<String> = reader
        .conn
        .prepare("PRAGMA table_info(type_alias_facts)")
        .unwrap()
        .query_map([], |row| row.get(1))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(alias_columns.contains(&"declaration_start_byte".to_string()));
    assert!(alias_columns.contains(&"underlying_spelling".to_string()));
    assert!(alias_columns.contains(&"declarator_shape".to_string()));
    assert!(alias_columns.contains(&"target_fidelity".to_string()));
    assert!(alias_columns.contains(&"fingerprint".to_string()));
    assert!(alias_columns.contains(&"declaration_hash".to_string()));

    let alias_indexes: Vec<String> = reader
        .conn
        .prepare("PRAGMA index_list(type_alias_facts)")
        .unwrap()
        .query_map([], |row| row.get(1))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(alias_indexes.contains(&"idx_type_alias_facts_alias".to_string()));
    assert!(alias_indexes.contains(&"idx_type_alias_facts_fingerprint".to_string()));

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
fn record_and_alias_rows_roundtrip_semantic_ranges_shapes_and_revision_metadata() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    let source = concat!(
        "struct Foo {\r\n",
        "  int field;\r\n",
        "};\r\n",
        "typedef const struct Foo FooConst;\r\n",
        "typedef struct Foo *FooPtr;\r\n",
        "typedef struct Foo FooArray[4];\r\n",
        "typedef int (*Callback)(int);\r\n",
    );
    upsert_source(&mut store, "a_types.h", source);

    let (records, record_truncated) = store
        .member_view()
        .record_rows_by_name_limited("Foo", 8)
        .expect("record rows");
    assert!(!record_truncated);
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.range_fidelity, RecordRangeFidelity::AstExact);
    assert_eq!(
        &source[record.start_byte..record.end_byte],
        "struct Foo {\r\n  int field;\r\n}"
    );
    assert_eq!(
        &source[record.body_range.start_byte..record.body_range.end_byte],
        "{\r\n  int field;\r\n}"
    );
    assert_eq!(
        &source[record.declaration_range.start_byte..record.declaration_range.end_byte],
        "struct Foo {\r\n  int field;\r\n};"
    );
    assert!(record.revision_id > 0);
    assert_eq!(record.revision_size, source.len() as u64);
    assert_eq!(record.revision_mtime_ns, 1);
    assert_eq!(record.revision_hash, "a_types.h-hash");
    assert_eq!(
        record.declaration_hash,
        *blake3::hash(
            &source.as_bytes()
                [record.declaration_range.start_byte..record.declaration_range.end_byte]
        )
        .as_bytes()
    );

    let aliases = [
        (
            "FooConst",
            "const struct Foo",
            DeclaratorShape::Qualified {
                qualifiers: vec!["const".to_string()],
            },
        ),
        (
            "FooPtr",
            "struct Foo",
            DeclaratorShape::Pointer {
                qualifiers: Vec::new(),
            },
        ),
        (
            "FooArray",
            "struct Foo",
            DeclaratorShape::Array {
                extent_text: "4".to_string(),
            },
        ),
        ("Callback", "int", DeclaratorShape::Unsupported),
    ];
    for (name, underlying, shape) in aliases {
        let (rows, truncated) = store
            .member_view()
            .alias_rows_by_name_limited(name, 8)
            .expect("alias rows");
        assert!(!truncated);
        assert_eq!(rows.len(), 1, "alias {name}");
        let alias = &rows[0];
        assert_eq!(
            &source[alias.name_range.start_byte..alias.name_range.end_byte],
            name
        );
        assert!(
            source[alias.declaration_range.start_byte..alias.declaration_range.end_byte]
                .starts_with("typedef ")
        );
        assert_eq!(alias.underlying_spelling, underlying);
        assert_eq!(alias.declarator_shape, shape);
        assert_eq!(alias.target_fidelity, AliasTargetFidelity::AstExact);
        assert_eq!(alias.fingerprint.len(), 24);
        assert!(alias
            .fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(alias.revision_id, record.revision_id);
        assert_eq!(alias.revision_size, source.len() as u64);
        assert_eq!(alias.revision_mtime_ns, 1);
        assert_eq!(alias.revision_hash, "a_types.h-hash");
        assert_eq!(
            alias.declaration_hash,
            *blake3::hash(
                &source.as_bytes()
                    [alias.declaration_range.start_byte..alias.declaration_range.end_byte]
            )
            .as_bytes()
        );
    }
}

#[test]
fn exact_name_record_and_alias_reads_are_bounded_and_stably_ordered() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    upsert_source(
        &mut store,
        "z_types.h",
        "struct Foo { int z; }; typedef struct Foo FooAlias;\n",
    );
    upsert_source(
        &mut store,
        "a_types.h",
        "struct Foo { int a; }; typedef struct Foo FooAlias;\n",
    );

    let (records, records_truncated) = store
        .member_view()
        .record_rows_by_name_limited("Foo", 1)
        .expect("record rows");
    assert!(records_truncated);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].path, "a_types.h");

    let (aliases, aliases_truncated) = store
        .member_view()
        .alias_rows_by_name_limited("FooAlias", 1)
        .expect("alias rows");
    assert!(aliases_truncated);
    assert_eq!(aliases.len(), 1);
    assert_eq!(aliases[0].path, "a_types.h");

    let (missing, missing_truncated) = store
        .member_view()
        .alias_rows_by_name_limited("fooalias", 1)
        .expect("case-sensitive alias rows");
    assert!(missing.is_empty());
    assert!(!missing_truncated);
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
