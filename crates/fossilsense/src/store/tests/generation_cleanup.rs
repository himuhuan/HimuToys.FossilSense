use super::*;
use crate::store::IncludeGraphUpdate;

#[test]
fn cleanup_parent_checks_have_bounded_child_indexes() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.sqlite");
    let store = IndexStore::open(&db, dir.path()).unwrap();
    let expected = [
        "idx_fallback_completion_revision",
        "idx_declaration_facts_revision",
        "idx_import_facts_revision",
        "idx_include_facts_revision",
        "idx_record_facts_revision",
        "idx_member_facts_revision",
        "idx_type_alias_facts_revision",
        "idx_call_site_file_id",
        "idx_include_edges_dst",
        "idx_type_alias_facts_target_record",
        "idx_pending_file_revisions_file_id",
        "idx_pending_file_revisions_revision_id",
    ];
    for index in expected {
        let exists: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = ?1",
                [index],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(exists, 1, "cleanup lookup index {index} is missing");
    }
}

#[test]
fn orphan_file_cleanup_replays_fact_and_relation_parent_actions() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).unwrap();
    upsert_source(
        &mut store,
        "live.c",
        "struct Item { int value; };\n\
         typedef struct Item ItemAlias;\n\
         static int helper(int value) { return value; }\n\
         int caller(void) { return helper(1); }\n",
    );

    store
        .conn
        .execute(
            "INSERT INTO file_entries (
                 path, extension, size, mtime_ns, hash, indexed_at, status,
                 error, source, directly_included, unresolved_includes,
                 ambiguous_includes
             )
             SELECT
                 'orphan.c', extension, size, mtime_ns, hash, indexed_at,
                 status, error, source, directly_included,
                 unresolved_includes, ambiguous_includes
             FROM file_entries WHERE path = 'live.c'",
            [],
        )
        .unwrap();
    let orphan_file_id: i64 = store
        .conn
        .query_row(
            "SELECT id FROM file_entries WHERE path = 'orphan.c'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let record_id: i64 = store
        .conn
        .query_row("SELECT id FROM record_facts LIMIT 1", [], |row| row.get(0))
        .unwrap();
    let caller_anchor_id: i64 = store
        .conn
        .query_row(
            "SELECT anchor.id FROM callable_anchor_facts anchor
             JOIN call_strings name ON name.id = anchor.name_id
             WHERE name.text = 'caller'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let declaration_id: i64 = store
        .conn
        .query_row(
            "SELECT id FROM declaration_facts WHERE name = 'Item' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    store
        .conn
        .execute(
            "UPDATE record_facts SET file_id = ?1 WHERE id = ?2",
            rusqlite::params![orphan_file_id, record_id],
        )
        .unwrap();
    store
        .conn
        .execute(
            "UPDATE callable_anchor_facts SET file_id = ?1 WHERE id = ?2",
            rusqlite::params![orphan_file_id, caller_anchor_id],
        )
        .unwrap();
    store
        .conn
        .execute(
            "UPDATE declaration_facts SET file_id = ?1 WHERE id = ?2",
            rusqlite::params![orphan_file_id, declaration_id],
        )
        .unwrap();
    let mut valid_before = store.conn.prepare("PRAGMA foreign_key_check").unwrap();
    assert!(!valid_before.exists([]).unwrap());
    drop(valid_before);

    let build = store.begin_index_build(false).unwrap();
    assert_eq!(store.stage_delete_file(build, "orphan.c").unwrap(), 1);
    let outcome = store
        .commit_index_build(build, &IncludeGraphUpdate::default())
        .unwrap();
    assert!(
        outcome.cleanup_warning.is_none(),
        "orphan-only cleanup must reproduce parent actions: {:?}",
        outcome.cleanup_warning
    );

    for (table, id) in [
        ("file_entries", orphan_file_id),
        ("declaration_facts", declaration_id),
        ("record_facts", record_id),
        ("callable_anchor_facts", caller_anchor_id),
    ] {
        let remaining: i64 = store
            .conn
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE id = ?1"),
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0, "{table} retained its deleted parent");
    }
    let dependent_counts: (i64, i64, i64) = store
        .conn
        .query_row(
            "SELECT
                 (SELECT COUNT(*) FROM call_site_facts
                  WHERE caller_anchor_id = ?1),
                 (SELECT COUNT(*) FROM member_facts
                  WHERE record_id = ?2),
                 (SELECT COUNT(*) FROM type_alias_facts
                  WHERE target_record_id = ?2)",
            rusqlite::params![caller_anchor_id, record_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(dependent_counts, (0, 0, 0));
    let revision_counts: (i64, i64) = store
        .conn
        .query_row(
            "SELECT
                 (SELECT COUNT(*) FROM file_revisions),
                 (SELECT COUNT(*) FROM active_file_revisions)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(revision_counts, (1, 1));
    let mut valid_after = store.conn.prepare("PRAGMA foreign_key_check").unwrap();
    assert!(!valid_after.exists([]).unwrap());
}
