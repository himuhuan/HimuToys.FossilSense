use super::*;

#[test]
fn sql_affected_include_sources_finds_by_basename() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");

    // a.c includes "util.h"; b.c includes <stdio.h>.
    upsert_source(
        &mut store,
        "src/a.c",
        "#include \"util.h\"\nint a(void){return 0;}\n",
    );
    upsert_source(
        &mut store,
        "src/b.c",
        "#include <stdio.h>\nint b(void){return 0;}\n",
    );

    let affected = store
        .affected_include_sources(
            &["inc/util.h".to_string()], // changed path
            &Default::default(),
            &[],
        )
        .expect("affected");

    // a.c should be in the list because its include basename "util.h" matches.
    assert!(
        affected.contains(&"src/a.c".to_string()),
        "a.c should be found by basename match: {affected:?}"
    );
    // b.c should NOT be affected (different basename).
    assert!(
        !affected.contains(&"src/b.c".to_string()),
        "b.c should not be affected"
    );
}

#[test]
fn sql_affected_include_sources_finds_by_normalized_target() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");

    upsert_source(
        &mut store,
        "src/a.c",
        "#include \"inc/util.h\"\nint a(void){return 0;}\n",
    );

    let affected = store
        .affected_include_sources(
            &["inc/util.h".to_string()], // exact normalized match
            &Default::default(),
            &[],
        )
        .expect("affected");

    assert!(
        affected.contains(&"src/a.c".to_string()),
        "a.c should be found by normalized target: {affected:?}"
    );
}

#[test]
fn batch_delete_missing_files_anti_join() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");

    upsert_source(&mut store, "keep.c", "int keep(void){return 0;}\n");
    upsert_source(&mut store, "remove.c", "int remove(void){return 0;}\n");

    let mut seen = HashSet::new();
    seen.insert("keep.c".to_string());
    let deleted = store.delete_missing_files(&seen).expect("delete");
    assert_eq!(deleted, 1, "one file should be deleted");

    let names = store.declaration_name_rows().expect("names");
    assert!(names.iter().any(|row| row.name == "keep"));
    assert!(!names.iter().any(|row| row.name == "remove"));
}

#[test]
fn batch_declarations_by_ids_preserves_order_and_omits_missing() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");

    upsert_source(&mut store, "a.c", "int first(void){return 1;}\n");
    upsert_source(&mut store, "b.c", "int second(void){return 2;}\n");
    upsert_source(&mut store, "c.c", "int third(void){return 3;}\n");

    let all = store.declaration_name_rows().expect("names");
    let ids: Vec<i64> = all.iter().map(|row| row.id).collect();
    assert!(ids.len() >= 3, "expected at least 3 declarations");

    // Query in reverse order with a non-existent id and a duplicate mixed in.
    let query_ids = vec![ids[2], 99999, ids[0], ids[2], ids[1]];
    let records = store.declarations_by_ids(&query_ids).expect("by ids");
    assert_eq!(records.len(), 4, "missing id 99999 should be omitted");
    assert_eq!(records[0].id, ids[2], "order preserved: third first");
    assert_eq!(records[1].id, ids[0], "order preserved: first second");
    assert_eq!(records[2].id, ids[2], "duplicate id preserved");
    assert_eq!(records[3].id, ids[1], "order preserved: second last");
}

#[test]
fn wal_checkpoint_after_full_rebuild() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");

    store.begin_full_rebuild_load().expect("begin");
    upsert_source(&mut store, "a.c", "int x(void){return 0;}\n");
    store.finish_full_rebuild_load().expect("finish");

    // No error = WAL checkpoint succeeded. Verify store is still readable.
    let reader = IndexStore::open_readonly(&db).expect("readonly");
    assert!(!reader.declaration_name_rows().expect("names").is_empty());
}

#[test]
fn full_build_defers_secondary_indexes_until_facts_are_complete() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open_for_full_rebuild(&db, dir.path()).expect("bulk store");
    let wal_autocheckpoint: i64 = store
        .conn
        .query_row("PRAGMA wal_autocheckpoint", [], |row| row.get(0))
        .expect("bulk WAL auto-checkpoint mode");
    assert_eq!(wal_autocheckpoint, 0);
    let journal_mode: String = store
        .conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("bulk journal mode");
    assert_eq!(journal_mode.to_ascii_lowercase(), "memory");
    let synchronous: i64 = store
        .conn
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .expect("bulk synchronous mode");
    assert_eq!(synchronous, 0);
    let call_index_count = |store: &IndexStore| -> i64 {
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name IN (
                    'idx_call_strings_text',
                    'idx_callable_anchor_name', 'idx_callable_anchor_qualified_name',
                    'idx_callable_anchor_entity_key', 'idx_callable_anchor_revision',
                    'idx_call_site_caller', 'idx_call_site_callee_arity',
                    'idx_call_site_revision'
                 )",
                [],
                |row| row.get(0),
            )
            .expect("call index count")
    };
    let cleanup_index_count = |store: &IndexStore| -> i64 {
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name IN (
                    'idx_fallback_completion_revision',
                    'idx_declaration_facts_revision',
                    'idx_import_facts_revision',
                    'idx_include_facts_revision',
                    'idx_record_facts_revision',
                    'idx_member_facts_revision',
                    'idx_type_alias_facts_revision',
                    'idx_type_alias_facts_target_record',
                    'idx_call_site_file_id',
                    'idx_include_edges_dst',
                    'idx_pending_file_revisions_file_id',
                    'idx_pending_file_revisions_revision_id'
                 )",
                [],
                |row| row.get(0),
            )
            .expect("cleanup index count")
    };
    assert_eq!(call_index_count(&store), 0);
    assert_eq!(cleanup_index_count(&store), 0);

    store.begin_full_rebuild_load().expect("begin");
    upsert_source(
        &mut store,
        "main.c",
        "int helper(int v) { return v; }\nint caller(void) { return helper(3); }\n",
    );
    store.finish_full_rebuild_load().expect("finish facts");
    assert_eq!(call_index_count(&store), 0);
    assert_eq!(cleanup_index_count(&store), 0);
    assert_eq!(test_call_sites_by_callee(&store, "helper").len(), 1);

    store
        .finalize_full_build_indexes()
        .expect("build call indexes");
    assert_eq!(call_index_count(&store), 8);
    assert_eq!(cleanup_index_count(&store), 12);
    let (strings, distinct_strings): (i64, i64) = store
        .conn
        .query_row(
            "SELECT COUNT(*), COUNT(DISTINCT text) FROM call_strings",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("unique call strings");
    assert_eq!(strings, distinct_strings);
    let plan: Vec<String> = store
        .conn
        .prepare(
            "EXPLAIN QUERY PLAN
             SELECT id FROM call_site_facts
             WHERE callee_name_id = (SELECT id FROM call_strings WHERE text = 'helper')
               AND argument_count = 1",
        )
        .unwrap()
        .query_map([], |row| row.get(3))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(
        plan.iter()
            .any(|detail| detail.contains("idx_call_site_callee_arity")),
        "unexpected call lookup plan: {plan:?}"
    );
}

#[test]
fn existing_explicit_full_build_keeps_online_call_string_uniqueness() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    {
        let mut store = IndexStore::open(&db, dir.path()).expect("initial store");
        upsert_source(
            &mut store,
            "main.c",
            "int first(void); int caller(void) { return first(); }\n",
        );
    }

    let mut store =
        IndexStore::open_for_full_rebuild(&db, dir.path()).expect("existing bulk store");
    let string_index: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'index' AND name = 'idx_call_strings_text'",
            [],
            |row| row.get(0),
        )
        .expect("string index");
    assert_eq!(string_index, 1);
    store.begin_full_rebuild_load().expect("begin replacement");
    upsert_source(
        &mut store,
        "main.c",
        "int second(void); int caller(void) { return second(); }\n",
    );
    store
        .finish_full_rebuild_load()
        .expect("finish replacement");
    store
        .finalize_full_build_indexes()
        .expect("finalize replacement indexes");

    assert!(test_call_sites_by_callee(&store, "first").is_empty());
    assert_eq!(test_call_sites_by_callee(&store, "second").len(), 1);
    let duplicates: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM (
                SELECT text FROM call_strings GROUP BY text HAVING COUNT(*) > 1
             )",
            [],
            |row| row.get(0),
        )
        .expect("duplicate strings");
    assert_eq!(duplicates, 0);
}

#[test]
fn explicit_index_lock_serializes_writers_and_rejects_target_advancement() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    {
        let mut store = IndexStore::open(&db, dir.path()).expect("initial store");
        upsert_source(&mut store, "main.c", "int first(void) { return 1; }\n");
    }

    let lock = ExplicitIndexLock::acquire(&db).expect("first writer lock");
    let snapshot = lock
        .capture_replacement_snapshot()
        .expect("replacement snapshot");
    assert_eq!(snapshot.generation(), 1);
    let competing_error = ExplicitIndexLock::acquire(&db)
        .err()
        .expect("a second writer must not share the lock");
    assert!(
        competing_error.to_string().contains("locked"),
        "unexpected competing writer error: {competing_error:#}"
    );

    // Simulate a non-cooperating external writer. FossilSense writers cannot
    // reach this state because they all hold the sibling lock, but publication
    // still revalidates the target and fails closed.
    let connection = rusqlite::Connection::open(&db).expect("external connection");
    connection
        .execute(
            "UPDATE meta SET value = '2' WHERE key = 'semantic_generation'",
            [],
        )
        .expect("advance target generation");
    drop(connection);
    let changed_error = lock
        .prepare_for_atomic_replacement(&snapshot)
        .expect_err("changed target must not be replaced");
    assert!(
        changed_error.to_string().contains("changed"),
        "unexpected target-change error: {changed_error:#}"
    );
    drop(lock);

    ExplicitIndexLock::acquire(&db).expect("process exit releases writer lock");
}

#[test]
fn explicit_replacement_snapshot_rejects_an_invalid_generation() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    {
        let store = IndexStore::open(&db, dir.path()).expect("store");
        store
            .conn
            .execute(
                "UPDATE meta SET value = 'not-a-generation'
                 WHERE key = 'semantic_generation'",
                [],
            )
            .expect("corrupt generation");
    }

    let lock = ExplicitIndexLock::acquire(&db).expect("writer lock");
    let error = lock
        .capture_replacement_snapshot()
        .expect_err("invalid generation must not silently become zero");
    assert!(
        error.to_string().contains("invalid semantic generation"),
        "unexpected invalid-generation error: {error:#}"
    );
}

#[test]
fn explicit_replacement_drains_a_real_persistent_wal() {
    use std::ffi::{c_void, CString};

    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    {
        let mut store = IndexStore::open(&db, dir.path()).expect("store");
        upsert_source(&mut store, "main.c", "int first(void) { return 1; }\n");
    }
    let connection = rusqlite::Connection::open(&db).expect("WAL writer");
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .expect("WAL mode");
    connection
        .pragma_update(None, "wal_autocheckpoint", 0)
        .expect("disable auto-checkpoint");
    let database_name = CString::new("main").expect("database name");
    let mut persist = 1_i32;
    let result = unsafe {
        rusqlite::ffi::sqlite3_file_control(
            connection.handle(),
            database_name.as_ptr(),
            rusqlite::ffi::SQLITE_FCNTL_PERSIST_WAL,
            (&mut persist as *mut i32).cast::<c_void>(),
        )
    };
    assert_eq!(result, rusqlite::ffi::SQLITE_OK);
    connection
        .execute_batch(
            "CREATE TABLE wal_probe (value INTEGER NOT NULL);
             INSERT INTO wal_probe VALUES (1);",
        )
        .expect("write persistent WAL");
    drop(connection);

    let wal = db.with_file_name("index.sqlite-wal");
    assert!(wal.try_exists().expect("inspect persistent WAL"));
    drain_sqlite_wal(&db).expect("drain WAL");
    assert!(!wal.try_exists().expect("inspect drained WAL"));
    assert!(!db
        .with_file_name("index.sqlite-shm")
        .try_exists()
        .expect("inspect drained SHM"));
    let connection = rusqlite::Connection::open(&db).expect("drained database");
    let probe: i64 = connection
        .query_row("SELECT value FROM wal_probe", [], |row| row.get(0))
        .expect("checkpointed WAL row");
    assert_eq!(probe, 1);
}
