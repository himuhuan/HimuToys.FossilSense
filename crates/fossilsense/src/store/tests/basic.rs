use super::*;
use crate::semantic_model::{SemanticDeclarationKind, SemanticDeclarationRole};

#[test]
fn writes_declarations_and_cleans_deleted_files() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    let source = "int hello_value(void);\n";
    let index = parse(std::path::Path::new("hello.h"), source);
    let fingerprint = FileFingerprint {
        path: "hello.h".to_string(),
        extension: "h".to_string(),
        size: source.len() as u64,
        mtime_ns: 1,
        hash: "abc".to_string(),
    };

    store
        .upsert_file_index(&fingerprint, &index)
        .expect("upsert");
    assert_eq!(store.declaration_count().expect("count"), 1);

    let deleted = store
        .delete_missing_files(&Default::default())
        .expect("delete missing");
    assert_eq!(deleted, 1);
    assert_eq!(store.declaration_count().expect("count"), 0);
}

#[test]
fn reads_declarations_by_name_and_id() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    let source = "int hello_value(void) { return 42; }\n";
    let index = parse(std::path::Path::new("src/hello.c"), source);
    let fingerprint = FileFingerprint {
        path: "src/hello.c".to_string(),
        extension: "c".to_string(),
        size: source.len() as u64,
        mtime_ns: 1,
        hash: "abc".to_string(),
    };
    store
        .upsert_file_index(&fingerprint, &index)
        .expect("upsert");

    let reader = IndexStore::open_readonly(&db).expect("readonly");

    let names = reader.declaration_name_rows().expect("names");
    assert!(names.iter().any(|row| row.name == "hello_value"));

    let by_name = reader.declarations_by_name("hello_value").expect("by name");
    assert_eq!(by_name.len(), 1);
    let record = &by_name[0];
    assert_eq!(
        record.fact.declaration_kind,
        SemanticDeclarationKind::Function
    );
    assert_eq!(record.fact.role, SemanticDeclarationRole::Definition);
    assert_eq!(record.fact.path, "src/hello.c");

    let by_id = reader.declarations_by_ids(&[record.id]).expect("by id");
    assert_eq!(by_id, by_name);

    assert!(reader
        .declarations_by_name("missing")
        .expect("miss")
        .is_empty());
}

#[test]
fn indexes_first_typedef_after_multiline_macro_for_goto_definition() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    let source = r#"#define FREE(ptr)                                                              \
    do                                                                         \
    {                                                                          \
        if ((ptr) != NULL)                                                     \
        {                                                                      \
            free(ptr);                                                         \
            (ptr) = NULL;                                                      \
        }                                                                      \
    } while (0)

typedef struct xxx {
    int value;
} xxx_t;

typedef struct xxxa {
    int other;
} xxxa_t;
"#;
    upsert_source(&mut store, "macro_typedef.h", source);

    let reader = IndexStore::open_readonly(&db).expect("readonly");
    let xxx_defs = reader.declarations_by_name("xxx_t").expect("xxx_t");
    assert_eq!(xxx_defs.len(), 1);
    assert_eq!(
        xxx_defs[0].fact.declaration_kind,
        SemanticDeclarationKind::Alias
    );
    assert_eq!(xxx_defs[0].fact.role, SemanticDeclarationRole::Definition);
    let signature = xxx_defs[0]
        .fact
        .canonical_signature
        .as_deref()
        .unwrap_or_default();
    assert!(signature.starts_with("typedef struct xxx"));
    assert!(!signature.contains("while (0)"));

    assert_eq!(
        reader.declarations_by_name("xxxa_t").expect("xxxa_t").len(),
        1
    );
    assert_eq!(
        fields_by_record_names(&reader, &["xxx_t"]),
        vec!["value".to_string()]
    );
}

#[test]
fn marking_file_error_clears_old_symbols() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    let source = "int stale_symbol(void) { return 1; }\n";
    let index = parse(std::path::Path::new("stale.c"), source);
    let ok_fingerprint = FileFingerprint {
        path: "stale.c".to_string(),
        extension: "c".to_string(),
        size: source.len() as u64,
        mtime_ns: 1,
        hash: "ok".to_string(),
    };
    store
        .upsert_file_index(&ok_fingerprint, &index)
        .expect("upsert");
    assert_eq!(
        store
            .declarations_by_name("stale_symbol")
            .expect("symbol before error")
            .len(),
        1
    );

    let error_fingerprint = FileFingerprint {
        hash: "error".to_string(),
        ..ok_fingerprint
    };
    store
        .mark_file_error(&error_fingerprint, "failed to read")
        .expect("mark error");

    assert!(store
        .declarations_by_name("stale_symbol")
        .expect("symbol after error")
        .is_empty());
}

#[test]
fn counts_definition_kinds_by_name() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");

    // `WRAP` is both a macro and a function across two files; `widget_t` is a
    // single type definition.
    let macro_src = "#define WRAP(x) (x)\ntypedef int widget_t;\n";
    let macro_index = parse(std::path::Path::new("a.h"), macro_src);
    store
        .upsert_file_index(
            &FileFingerprint {
                path: "a.h".to_string(),
                extension: "h".to_string(),
                size: macro_src.len() as u64,
                mtime_ns: 1,
                hash: "a".to_string(),
            },
            &macro_index,
        )
        .expect("upsert a");

    let fn_src = "int WRAP(int x) { return x; }\n";
    let fn_index = parse(std::path::Path::new("b.c"), fn_src);
    store
        .upsert_file_index(
            &FileFingerprint {
                path: "b.c".to_string(),
                extension: "c".to_string(),
                size: fn_src.len() as u64,
                mtime_ns: 1,
                hash: "b".to_string(),
            },
            &fn_index,
        )
        .expect("upsert b");

    let reader = IndexStore::open_readonly(&db).expect("readonly");
    let counts = reader
        .declaration_kind_counts_by_names(&["WRAP", "widget_t", "absent"])
        .expect("counts");

    let wrap = counts.get("WRAP").expect("wrap counts");
    assert_eq!(wrap.get("macro").copied(), Some(1));
    assert_eq!(wrap.get("function").copied(), Some(1));

    let widget = counts.get("widget_t").expect("widget counts");
    assert_eq!(widget.get("type").copied(), Some(1));

    assert!(!counts.contains_key("absent"));
    assert!(reader
        .declaration_kind_counts_by_names(&[])
        .expect("empty")
        .is_empty());
}
