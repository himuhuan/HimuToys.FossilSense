use super::*;

#[test]
fn bundled_sqlite_contains_wal_reset_fix() {
    let version = rusqlite::version_number();
    let fixed = version >= 3_051_003
        || (3_050_007..3_051_000).contains(&version)
        || (3_044_006..3_045_000).contains(&version);
    assert!(
        fixed,
        "bundled SQLite {version} predates the WAL-reset corruption fix"
    );
}

#[test]
fn corrupted_db_errors_on_query_not_panic() {
    // SQLite defers validation; open_readonly may succeed even on garbage,
    // but the first SQL query must fail gracefully (no panic).
    let dir = tempdir().expect("tempdir");
    let bad_db = dir.path().join("corrupt.sqlite");
    std::fs::write(&bad_db, b"\x00\xFF\x00\xFF\xDE\xAD\xBE\xEF").expect("write");
    // Open may or may not succeed — if it does, querying must fail.
    if let Ok(store) = IndexStore::open_readonly(&bad_db) {
        let result = store.load_symbol_names();
        assert!(result.is_err(), "query on garbage DB must return Err");
    }
    // If open fails, that's also fine — we just verify no panic.
}

#[test]
fn empty_db_file_errors_on_query_not_panic() {
    let dir = tempdir().expect("tempdir");
    let empty_db = dir.path().join("empty.sqlite");
    std::fs::write(&empty_db, b"").expect("write");
    if let Ok(store) = IndexStore::open_readonly(&empty_db) {
        let result = store.load_symbol_names();
        assert!(result.is_err(), "query on empty file must return Err");
    }
}

#[test]
fn open_readonly_on_missing_file_returns_error_not_panic() {
    let dir = tempdir().expect("tempdir");
    let missing = dir.path().join("nonexistent.sqlite");
    assert!(!missing.exists());
    let result = IndexStore::open_readonly(&missing);
    assert!(result.is_err(), "missing file must return Err, not panic");
}

// --- Phase 5: SQL include invalidation, batch ops ----------------------

#[test]
fn current_schema_includes_have_normalized_metadata() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");

    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    upsert_source(
        &mut store,
        "src/main.c",
        "#include \"util.h\"\n#include <sys/types.h>\n#define MACRO_INC(x) <x>\n",
    );

    // Verify the schema version is current.
    let version: String = store
        .conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .expect("version");
    assert_eq!(
        version,
        crate::store::schema::SCHEMA_VERSION.to_string(),
        "schema version must be current"
    );

    // Verify includes columns exist and are populated.
    let mut stmt = store
        .conn
        .prepare("SELECT target_text, target_form, target_normalized, target_basename FROM includes ORDER BY line")
        .expect("prepare");
    let rows: Vec<(String, String, String, String)> = stmt
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();

    // Quote include: form="quote", normalized="util.h", basename="util.h"
    let quote_row = &rows[0];
    assert_eq!(quote_row.0, "\"util.h\"");
    assert_eq!(quote_row.1, "quote");
    assert_eq!(quote_row.2, "util.h");
    assert_eq!(quote_row.3, "util.h");

    // Angle include: form="angle", normalized="sys/types.h", basename="types.h"
    let angle_row = &rows[1];
    assert_eq!(angle_row.0, "<sys/types.h>");
    assert_eq!(angle_row.1, "angle");
    assert_eq!(angle_row.2, "sys/types.h");
    assert_eq!(angle_row.3, "types.h");

    // The third include is a macro definition, not an #include directive.
    // The parser only produces include rows for `#include` lines, so we get
    // 2 includes (not 3). The macro line is parsed as a symbol, not an include.
    assert_eq!(rows.len(), 2, "only #include lines produce include rows");

    // Existing `includes_with_file_ids` still returns raw target_text for
    // edge rebuild (old API unchanged).
    let raw = store.includes_with_file_ids(None).expect("raw");
    assert_eq!(raw.len(), 2);
    assert!(raw.iter().any(|(_, t)| t == "\"util.h\""));
}

#[test]
fn current_schema_has_members_table_and_version_9_or_newer() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let store = IndexStore::open(&db, dir.path()).expect("store");

    let version: String = store
        .conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .expect("version");
    assert!(version.parse::<i64>().expect("numeric version") >= 9);

    store
        .conn
        .prepare(
            "SELECT record_id, name, kind, confidence, signature, type_name FROM members LIMIT 1",
        )
        .expect("members table exists");
}

#[test]
fn opening_v8_schema_drops_old_field_rows_for_full_rebuild() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    {
        let conn = rusqlite::Connection::open(&db).expect("conn");
        conn.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL);
             INSERT INTO meta (key, value) VALUES ('schema_version', '8');
             CREATE TABLE files (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 path TEXT NOT NULL UNIQUE,
                 extension TEXT NOT NULL,
                 size INTEGER NOT NULL,
                 mtime_ns INTEGER NOT NULL,
                 hash TEXT NOT NULL,
                 indexed_at INTEGER NOT NULL,
                 status TEXT NOT NULL,
                 error TEXT,
                 source TEXT NOT NULL DEFAULT 'workspace',
                 directly_included INTEGER NOT NULL DEFAULT 0,
                 unresolved_includes INTEGER NOT NULL DEFAULT 0,
                 ambiguous_includes INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE record_defs (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 file_id INTEGER NOT NULL,
                 display_name TEXT NOT NULL,
                 tag_name TEXT,
                 typedef_name TEXT,
                 kind TEXT NOT NULL,
                 start_byte INTEGER NOT NULL,
                 end_byte INTEGER NOT NULL,
                 start_line INTEGER NOT NULL,
                 start_col INTEGER NOT NULL,
                 end_line INTEGER NOT NULL,
                 end_col INTEGER NOT NULL,
                 signature TEXT NOT NULL,
                 confidence TEXT NOT NULL
             );
             CREATE TABLE fields (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 record_id INTEGER NOT NULL,
                 name TEXT NOT NULL,
                 start_byte INTEGER NOT NULL,
                 end_byte INTEGER NOT NULL,
                 start_line INTEGER NOT NULL,
                 start_col INTEGER NOT NULL,
                 end_line INTEGER NOT NULL,
                 end_col INTEGER NOT NULL,
                 signature TEXT NOT NULL
             );
             INSERT INTO files (path, extension, size, mtime_ns, hash, indexed_at, status)
             VALUES ('old.h', 'h', 1, 1, 'hash', 1, 'ok');
             INSERT INTO record_defs (
                 file_id, display_name, kind, start_byte, end_byte, start_line, start_col,
                 end_line, end_col, signature, confidence
             )
             VALUES (1, 'Old', 'struct', 0, 1, 0, 0, 0, 1, 'struct Old', 'named_tag');
             INSERT INTO fields (
                 record_id, name, start_byte, end_byte, start_line, start_col, end_line, end_col,
                 signature
             )
             VALUES (1, 'stale', 0, 5, 0, 0, 0, 5, 'int stale');",
        )
        .expect("seed v8");
    }

    let store = IndexStore::open(&db, dir.path()).expect("migrate");
    let count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM members", [], |row| row.get(0))
        .expect("count members");
    assert_eq!(count, 0);
}

#[test]
fn current_schema_migrate_by_drop_clears_old_data() {
    // Simulate an older schema by opening v6, inserting data, then opening
    // with the current schema — the old tables should be dropped and recreated.
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");

    // First, create a v6 store and insert data.
    {
        // Write a v6 schema version into the db by manually setting the meta.
        // We use IndexStore::open which will migrate to current, so instead
        // we open a raw connection to seed v6 state.
        let conn = rusqlite::Connection::open(&db).expect("conn");
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.execute(
            "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('schema_version', '6')",
            [],
        )
        .unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS files (
                id INTEGER PRIMARY KEY AUTOINCREMENT, path TEXT NOT NULL UNIQUE,
                extension TEXT NOT NULL, size INTEGER NOT NULL, mtime_ns INTEGER NOT NULL,
                hash TEXT NOT NULL, indexed_at INTEGER NOT NULL, status TEXT NOT NULL,
                error TEXT, source TEXT NOT NULL DEFAULT 'workspace',
                directly_included INTEGER NOT NULL DEFAULT 0,
                unresolved_includes INTEGER NOT NULL DEFAULT 0,
                ambiguous_includes INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS includes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                line INTEGER NOT NULL, target_text TEXT NOT NULL
            );
            INSERT INTO files (path, extension, size, mtime_ns, hash, indexed_at, status)
            VALUES ('old.c', 'c', 100, 1, 'abc', 1, 'ok');
            INSERT INTO includes (file_id, line, target_text) VALUES (1, 1, '\"old.h\"');",
        )
        .unwrap();
    }

    // Open with current schema: migrate-by-drop clears old tables.
    let store = IndexStore::open(&db, dir.path()).expect("store");

    // Old data is gone; current schema has the three extra columns.
    let count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM includes", [], |row| row.get(0))
        .expect("count");
    assert_eq!(count, 0, "old includes rows dropped by migration");

    let columns: Vec<String> = store
        .conn
        .prepare("PRAGMA table_info(includes)")
        .unwrap()
        .query_map([], |row| row.get(1))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(columns.contains(&"target_form".to_string()));
    assert!(columns.contains(&"target_normalized".to_string()));
    assert!(columns.contains(&"target_basename".to_string()));
}

#[test]
fn opening_schema_15_drops_old_semantic_facts_for_schema_16_rebuild() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    {
        let conn = rusqlite::Connection::open(&db).expect("conn");
        conn.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL);
             INSERT INTO meta (key, value) VALUES ('schema_version', '15');
             CREATE TABLE callable_anchor_facts (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 entity_key TEXT NOT NULL,
                 name TEXT NOT NULL
             );
             CREATE TABLE call_site_facts (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 caller_entity_key TEXT NOT NULL,
                 site_fingerprint TEXT NOT NULL
             );
             INSERT INTO callable_anchor_facts (entity_key, name) VALUES ('old-key', 'old');
             INSERT INTO call_site_facts (caller_entity_key, site_fingerprint)
             VALUES ('old-key', 'old-site');
             CREATE VIEW callable_anchors AS SELECT * FROM callable_anchor_facts;
             CREATE VIEW call_sites AS SELECT * FROM call_site_facts;",
        )
        .expect("seed schema 15 call facts");
    }

    let store = IndexStore::open(&db, dir.path()).expect("open schema 16");
    let version: String = store
        .conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .expect("schema version");
    assert_eq!(version, "16");

    let anchor_count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM callable_anchor_facts", [], |row| {
            row.get(0)
        })
        .expect("anchor count");
    let site_count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM call_site_facts", [], |row| row.get(0))
        .expect("site count");
    assert_eq!((anchor_count, site_count), (0, 0));

    let anchor_columns: Vec<String> = store
        .conn
        .prepare("PRAGMA table_info(callable_anchor_facts)")
        .unwrap()
        .query_map([], |row| row.get(1))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(anchor_columns.contains(&"entity_digest".to_string()));
    assert!(anchor_columns.contains(&"canonical_signature_id".to_string()));
    assert!(anchor_columns.contains(&"presentation_signature_id".to_string()));
    assert!(anchor_columns.contains(&"signature_fidelity".to_string()));
    assert!(!anchor_columns.contains(&"entity_key".to_string()));
}

#[test]
fn parser_fact_version_mismatch_invalidates_and_rebuilds_current_schema() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    {
        let mut store = IndexStore::open(&db, dir.path()).expect("store");
        upsert_source(&mut store, "stale.c", "int stale(void) { return 1; }\n");
        assert!(IndexStore::has_current_schema(&db).expect("current schema"));
        store
            .conn
            .execute(
                "UPDATE file_revisions SET parser_version = ?1",
                [crate::parser::PARSER_FACT_VERSION - 1],
            )
            .expect("mark parser facts stale");
    }

    assert!(
        !IndexStore::has_current_schema(&db).expect("stale parser version"),
        "an active revision from an older parser fact contract must force rebuild"
    );

    let store = IndexStore::open(&db, dir.path()).expect("rebuild stale parser facts");
    let active_revisions: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM active_file_revisions", [], |row| {
            row.get(0)
        })
        .expect("active revisions");
    let symbols: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM symbol_facts", [], |row| row.get(0))
        .expect("symbol facts");
    assert_eq!((active_revisions, symbols), (0, 0));
    drop(store);
    assert!(IndexStore::has_current_schema(&db).expect("rebuilt schema"));
}
