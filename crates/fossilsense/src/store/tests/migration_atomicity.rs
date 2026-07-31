use super::*;

const MIGRATION_CRASH_DB_ENV: &str = "FOSSILSENSE_TEST_MIGRATION_CRASH_DB";
const MIGRATION_CRASH_ROOT_ENV: &str = "FOSSILSENSE_TEST_MIGRATION_CRASH_ROOT";
const MIGRATION_CRASH_MARKER_ENV: &str = "FOSSILSENSE_TEST_MIGRATION_CRASH_MARKER";
const MIGRATION_CRASH_MARKER_CONTENT: &[u8] = b"destructive-drop-complete\n";
const MIGRATION_CRASH_HELPER_EXIT: i32 = 91;

fn schema_snapshot(conn: &rusqlite::Connection) -> Vec<(String, String, String, String)> {
    conn.prepare(
        "SELECT type, name, tbl_name, COALESCE(sql, '')
         FROM sqlite_master
         ORDER BY type, name",
    )
    .expect("prepare schema snapshot")
    .query_map([], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
    })
    .expect("query schema snapshot")
    .collect::<rusqlite::Result<_>>()
    .expect("collect schema snapshot")
}

#[test]
fn failed_schema_migration_rolls_back_all_ddl_and_can_retry() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let before = {
        let conn = rusqlite::Connection::open(&db).expect("conn");
        conn.execute_batch(
            "CREATE TABLE meta (
                 key TEXT PRIMARY KEY NOT NULL,
                 value TEXT NOT NULL
             );
             INSERT INTO meta (key, value) VALUES
                 ('schema_version', '27'),
                 ('workspace_root', 'legacy-root'),
                 ('semantic_generation', '7');
             CREATE TABLE files (
                 id INTEGER PRIMARY KEY,
                 path TEXT NOT NULL
             );
             INSERT INTO files (id, path) VALUES (1, 'legacy.c');

             -- This is legal in the old database but conflicts with a current
             -- lookup index after the destructive migration steps have run.
             CREATE TABLE idx_declaration_facts_name (
                 sentinel TEXT NOT NULL
             );
             INSERT INTO idx_declaration_facts_name (sentinel) VALUES ('keep');",
        )
        .expect("seed legacy schema and deterministic migration blocker");
        schema_snapshot(&conn)
    };

    let migration = IndexStore::open(&db, dir.path());
    assert!(
        migration.is_err(),
        "the lookup-index name conflict must abort migration"
    );

    {
        let conn = rusqlite::Connection::open(&db).expect("inspect failed migration");
        assert_eq!(
            schema_snapshot(&conn),
            before,
            "failed migration must restore every dropped and created schema object"
        );
        let legacy_path: String = conn
            .query_row("SELECT path FROM files WHERE id = 1", [], |row| row.get(0))
            .expect("legacy row survives");
        let metadata: Vec<(String, String)> = conn
            .prepare("SELECT key, value FROM meta ORDER BY key")
            .expect("prepare metadata")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("query metadata")
            .collect::<rusqlite::Result<_>>()
            .expect("collect metadata");
        assert_eq!(legacy_path, "legacy.c");
        assert_eq!(
            metadata,
            vec![
                ("schema_version".to_string(), "27".to_string()),
                ("semantic_generation".to_string(), "7".to_string()),
                ("workspace_root".to_string(), "legacy-root".to_string()),
            ],
            "failed migration must not publish current metadata"
        );
        conn.execute("DROP TABLE idx_declaration_facts_name", [])
            .expect("remove deterministic blocker");
    }

    let store = IndexStore::open(&db, dir.path()).expect("retry migration");
    let version: String = store
        .conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .expect("current schema version");
    let current_files: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM file_entries", [], |row| row.get(0))
        .expect("current file count");
    assert_eq!(version, crate::store::schema::SCHEMA_VERSION.to_string());
    assert_eq!(
        current_files, 0,
        "successful retry rebuilds an empty schema"
    );
}

#[test]
fn malformed_schema_version_fails_closed_without_reclassifying_the_database_as_new() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    {
        let conn = rusqlite::Connection::open(&db).expect("conn");
        conn.execute_batch(
            "CREATE TABLE meta (
                 key TEXT PRIMARY KEY NOT NULL,
                 value TEXT NOT NULL
             );
             INSERT INTO meta (key, value) VALUES
                 ('schema_version', 'not-a-version'),
                 ('workspace_root', 'legacy-root');
             CREATE TABLE legacy_payload (
                 value TEXT NOT NULL
             );
             INSERT INTO legacy_payload (value) VALUES ('keep');",
        )
        .expect("seed malformed schema metadata");
    }

    let migration = IndexStore::open(&db, dir.path());
    assert!(
        migration.is_err(),
        "malformed schema metadata must not be treated as a new database"
    );

    let conn = rusqlite::Connection::open(&db).expect("inspect failed open");
    let version: String = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .expect("malformed version remains");
    let payload: String = conn
        .query_row("SELECT value FROM legacy_payload", [], |row| row.get(0))
        .expect("legacy payload remains");
    let current_objects: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE name IN ('file_entries', 'file_revisions', 'declaration_facts')",
            [],
            |row| row.get(0),
        )
        .expect("count current objects");
    assert_eq!(version, "not-a-version");
    assert_eq!(payload, "keep");
    assert_eq!(
        current_objects, 0,
        "failed open must not install current schema objects"
    );
}

#[test]
fn failed_full_rebuild_index_deferral_rolls_back_partial_index_drops() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    drop(IndexStore::open(&db, dir.path()).expect("seed current schema"));

    let before = {
        let conn = rusqlite::Connection::open(&db).expect("conn");
        schema_snapshot(&conn)
    };

    IndexStore::set_migration_failpoint_for_test(
        crate::store::MigrationFailpoint::AfterDeferredIndexDrop,
    );
    let migration = IndexStore::open_for_full_rebuild(&db, dir.path());
    assert!(
        migration.is_err(),
        "injected failure must abort full-rebuild open after index deferral"
    );

    let conn = rusqlite::Connection::open(&db).expect("inspect failed index deferral");
    assert_eq!(
        schema_snapshot(&conn),
        before,
        "failed index deferral must restore every index dropped earlier in the batch"
    );
    drop(conn);

    let store =
        IndexStore::open_for_full_rebuild(&db, dir.path()).expect("retry full-rebuild open");
    let deferred_revision_indexes: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'index'
               AND name IN (
                   'idx_fallback_completion_revision',
                   'idx_declaration_facts_revision'
               )",
            [],
            |row| row.get(0),
        )
        .expect("count deferred indexes after successful retry");
    assert_eq!(
        deferred_revision_indexes, 0,
        "successful full-rebuild open defers both revision indexes"
    );
}

#[test]
fn migration_crash_child() {
    let Some(db) = std::env::var_os(MIGRATION_CRASH_DB_ENV) else {
        return;
    };
    let root = std::env::var_os(MIGRATION_CRASH_ROOT_ENV)
        .expect("crash child workspace root must accompany database path");
    IndexStore::set_migration_failpoint_for_test(
        crate::store::MigrationFailpoint::AbortAfterDestructiveDrop,
    );
    let outcome = IndexStore::open(
        &std::path::PathBuf::from(db),
        &std::path::PathBuf::from(root),
    );
    eprintln!(
        "migration abort failpoint was not reached; open succeeded: {}",
        outcome.is_ok()
    );
    std::process::exit(MIGRATION_CRASH_HELPER_EXIT);
}

#[test]
fn process_abort_during_destructive_migration_recovers_the_old_wal_schema() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let marker = dir.path().join("migration-abort.marker");
    let before = {
        let conn = rusqlite::Connection::open(&db).expect("conn");
        conn.pragma_update(None, "journal_mode", "WAL")
            .expect("enable WAL");
        conn.execute_batch(
            "CREATE TABLE meta (
                 key TEXT PRIMARY KEY NOT NULL,
                 value TEXT NOT NULL
             );
             INSERT INTO meta (key, value) VALUES
                 ('schema_version', '27'),
                 ('workspace_root', 'legacy-root'),
                 ('semantic_generation', '11');
             CREATE TABLE files (
                 id INTEGER PRIMARY KEY,
                 path TEXT NOT NULL
             );
             INSERT INTO files (id, path) VALUES (1, 'survives-crash.c');
             PRAGMA wal_checkpoint(TRUNCATE);",
        )
        .expect("seed WAL legacy schema");
        schema_snapshot(&conn)
    };

    let output = std::process::Command::new(std::env::current_exe().expect("current test exe"))
        .arg("--exact")
        .arg("store::tests::migration_atomicity::migration_crash_child")
        .arg("--nocapture")
        .env(MIGRATION_CRASH_DB_ENV, &db)
        .env(MIGRATION_CRASH_ROOT_ENV, dir.path())
        .env(MIGRATION_CRASH_MARKER_ENV, &marker)
        .output()
        .expect("run migration crash child");
    let marker_content = std::fs::read(&marker).unwrap_or_else(|error| {
        panic!(
            "crash child did not prove it reached the destructive-drop failpoint: {error}\n\
             stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(
        marker_content, MIGRATION_CRASH_MARKER_CONTENT,
        "crash child marker must be completely flushed before abort"
    );
    assert!(
        !output.status.success(),
        "crash child unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_ne!(
        output.status.code(),
        Some(MIGRATION_CRASH_HELPER_EXIT),
        "crash child reached the end instead of aborting\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    {
        let conn = rusqlite::Connection::open(&db).expect("recover crashed WAL database");
        assert_eq!(
            schema_snapshot(&conn),
            before,
            "WAL recovery must restore every schema object from before migration"
        );
        let legacy_path: String = conn
            .query_row("SELECT path FROM files WHERE id = 1", [], |row| row.get(0))
            .expect("legacy row survives process abort");
        let metadata: Vec<(String, String)> = conn
            .prepare("SELECT key, value FROM meta ORDER BY key")
            .expect("prepare recovered metadata")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("query recovered metadata")
            .collect::<rusqlite::Result<_>>()
            .expect("collect recovered metadata");
        let quick_check: String = conn
            .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
            .expect("quick check recovered database");
        assert_eq!(legacy_path, "survives-crash.c");
        assert_eq!(
            metadata,
            vec![
                ("schema_version".to_string(), "27".to_string()),
                ("semantic_generation".to_string(), "11".to_string()),
                ("workspace_root".to_string(), "legacy-root".to_string()),
            ]
        );
        assert_eq!(quick_check, "ok");
    }

    let store = IndexStore::open(&db, dir.path()).expect("retry migration after crash recovery");
    let version: String = store
        .conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .expect("current schema version");
    assert_eq!(version, crate::store::schema::SCHEMA_VERSION.to_string());
}
