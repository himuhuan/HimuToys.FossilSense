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

    let names = store.load_symbol_names().expect("names");
    assert!(names.iter().any(|(_, n, _)| n == "keep"));
    assert!(!names.iter().any(|(_, n, _)| n == "remove"));
}

#[test]
fn batch_symbols_by_ids_preserves_order_and_omits_missing() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");

    upsert_source(&mut store, "a.c", "int first(void){return 1;}\n");
    upsert_source(&mut store, "b.c", "int second(void){return 2;}\n");
    upsert_source(&mut store, "c.c", "int third(void){return 3;}\n");

    let all = store.load_symbol_names().expect("names");
    let ids: Vec<i64> = all.iter().map(|(id, _, _)| *id).collect();
    assert!(ids.len() >= 3, "expected at least 3 symbols");

    // Query in reverse order with a non-existent id and a duplicate mixed in.
    let query_ids = vec![ids[2], 99999, ids[0], ids[2], ids[1]];
    let records = store.symbols_by_ids(&query_ids).expect("by ids");
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
    assert!(!reader.load_symbol_names().expect("names").is_empty());
}

#[test]
fn full_build_defers_call_indexes_until_facts_are_complete() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open_for_full_rebuild(&db, dir.path()).expect("bulk store");
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
    assert_eq!(call_index_count(&store), 0);

    store.begin_full_rebuild_load().expect("begin");
    upsert_source(
        &mut store,
        "main.c",
        "int helper(int v) { return v; }\nint caller(void) { return helper(3); }\n",
    );
    store.finish_full_rebuild_load().expect("finish facts");
    assert_eq!(call_index_count(&store), 0);
    assert_eq!(test_call_sites_by_callee(&store, "helper").len(), 1);

    store
        .finalize_full_build_indexes()
        .expect("build call indexes");
    assert_eq!(call_index_count(&store), 8);
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
