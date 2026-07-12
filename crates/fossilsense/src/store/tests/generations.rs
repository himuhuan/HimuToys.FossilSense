use super::*;
use crate::store::{FileIndexPayload, FileIndexUpdate, IncludeGraphUpdate};

fn fingerprint(path: &str, source: &str, revision: i64) -> FileFingerprint {
    FileFingerprint {
        path: path.to_string(),
        extension: path.rsplit('.').next().unwrap_or("c").to_string(),
        size: source.len() as u64,
        mtime_ns: revision,
        hash: format!("hash-{revision}"),
    }
}

#[test]
fn staged_file_revision_is_invisible_until_manifest_flip() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).unwrap();
    upsert_source(&mut store, "main.c", "int old_name(void);\n");
    assert_eq!(store.semantic_generation().unwrap(), 1);

    let source = "int new_name(void);\n";
    let parsed = parse(std::path::Path::new("main.c"), source);
    let fp = fingerprint("main.c", source, 2);
    let build = store.begin_index_build(false).unwrap();
    store
        .stage_file_updates(
            build,
            &[FileIndexUpdate {
                fingerprint: &fp,
                source: FileSource::Workspace,
                payload: FileIndexPayload::Ok(&parsed),
            }],
        )
        .unwrap();

    assert!(store.symbols_by_name("old_name").unwrap().len() == 1);
    assert!(store.symbols_by_name("new_name").unwrap().is_empty());
    assert_eq!(store.semantic_generation().unwrap(), 1);

    let published = store
        .commit_index_build(build, &IncludeGraphUpdate::default())
        .unwrap();
    assert_eq!(published.generation, 2);
    assert!(published.cleanup_warning.is_none());
    assert!(store.symbols_by_name("old_name").unwrap().is_empty());
    assert!(store.symbols_by_name("new_name").unwrap().len() == 1);
}

#[test]
fn sqlite_reader_keeps_one_active_generation_across_publish() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.sqlite");
    let mut writer = IndexStore::open(&db, dir.path()).unwrap();
    upsert_source(&mut writer, "main.c", "int before(void);\n");

    let mut reader = IndexStore::open_readonly(&db).unwrap();
    let transaction = reader.conn.transaction().unwrap();
    let before_count: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM symbols WHERE name = 'before'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(before_count, 1);

    let source = "int after(void);\n";
    let parsed = parse(std::path::Path::new("main.c"), source);
    let fp = fingerprint("main.c", source, 2);
    let build = writer.begin_index_build(false).unwrap();
    writer
        .stage_file_updates(
            build,
            &[FileIndexUpdate {
                fingerprint: &fp,
                source: FileSource::Workspace,
                payload: FileIndexPayload::Ok(&parsed),
            }],
        )
        .unwrap();
    writer
        .commit_index_build(build, &IncludeGraphUpdate::default())
        .unwrap();

    let old_after_publish: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM symbols WHERE name = 'before'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let new_after_publish: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM symbols WHERE name = 'after'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!((old_after_publish, new_after_publish), (1, 0));
    transaction.commit().unwrap();
    assert_eq!(writer.symbols_by_name("after").unwrap().len(), 1);
}

#[test]
fn request_generation_guard_rejects_a_newer_active_manifest() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).unwrap();
    upsert_source(&mut store, "main.c", "int before(void);\n");
    let captured_generation = store.semantic_generation().unwrap();

    let source = "int after(void);\n";
    let parsed = parse(std::path::Path::new("main.c"), source);
    let fp = fingerprint("main.c", source, 2);
    let build = store.begin_index_build(false).unwrap();
    store
        .stage_file_updates(
            build,
            &[FileIndexUpdate {
                fingerprint: &fp,
                source: FileSource::Workspace,
                payload: FileIndexPayload::Ok(&parsed),
            }],
        )
        .unwrap();
    store
        .commit_index_build(build, &IncludeGraphUpdate::default())
        .unwrap();

    let error = IndexStore::read_at_generation(&db, captured_generation, |reader| {
        reader.symbols_by_name("after")
    })
    .expect_err("a request snapshot must not mix with a newer database generation");
    assert!(error.to_string().contains("generation"));
}

#[test]
fn abandoned_build_cannot_replace_active_facts() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).unwrap();
    upsert_source(&mut store, "main.c", "int stable(void);\n");

    let source = "int abandoned(void);\n";
    let parsed = parse(std::path::Path::new("main.c"), source);
    let fp = fingerprint("main.c", source, 2);
    let abandoned = store.begin_index_build(false).unwrap();
    store
        .stage_file_updates(
            abandoned,
            &[FileIndexUpdate {
                fingerprint: &fp,
                source: FileSource::Workspace,
                payload: FileIndexPayload::Ok(&parsed),
            }],
        )
        .unwrap();

    let replacement = store.begin_index_build(false).unwrap();
    assert!(store.symbols_by_name("stable").unwrap().len() == 1);
    assert!(store.symbols_by_name("abandoned").unwrap().is_empty());
    store
        .commit_index_build(replacement, &IncludeGraphUpdate::default())
        .unwrap();
    assert!(store.symbols_by_name("stable").unwrap().len() == 1);
}

#[test]
fn full_rebuild_switches_the_complete_file_set_once() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).unwrap();
    upsert_source(&mut store, "old.c", "int old_only(void);\n");

    let source = "int new_only(void);\n";
    let parsed = parse(std::path::Path::new("new.c"), source);
    let fp = fingerprint("new.c", source, 1);
    let build = store.begin_index_build(true).unwrap();
    store
        .stage_file_updates(
            build,
            &[FileIndexUpdate {
                fingerprint: &fp,
                source: FileSource::Workspace,
                payload: FileIndexPayload::Ok(&parsed),
            }],
        )
        .unwrap();
    assert!(store.symbols_by_name("old_only").unwrap().len() == 1);
    assert!(store.symbols_by_name("new_only").unwrap().is_empty());

    store
        .commit_index_build(
            build,
            &IncludeGraphUpdate {
                clear_all: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(store.symbols_by_name("old_only").unwrap().is_empty());
    assert!(store.symbols_by_name("new_only").unwrap().len() == 1);
}

#[test]
fn semantic_read_guard_rejects_a_mismatched_snapshot_generation() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.sqlite");
    let mut writer = IndexStore::open(&db, dir.path()).unwrap();
    upsert_source(&mut writer, "main.c", "int current(void);\n");
    let reader = IndexStore::open_readonly(&db).unwrap();

    let guard = reader.begin_semantic_read(Some(1)).unwrap();
    assert_eq!(guard.generation(), 1);
    assert_eq!(guard.store().symbols_by_name("current").unwrap().len(), 1);
    guard.finish().unwrap();

    let error = reader.begin_semantic_read(Some(9)).err().unwrap();
    assert!(error.to_string().contains("semantic generation mismatch"));
}

#[test]
fn cleanup_failure_does_not_turn_a_committed_generation_into_failure() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).unwrap();
    upsert_source(&mut store, "main.c", "int before_cleanup(void);\n");

    // Fail only the post-commit inactive-revision deletion. The generation
    // transaction itself does not delete from file_revisions.
    store
        .conn
        .execute_batch(
            "CREATE TRIGGER fail_inactive_revision_cleanup
             BEFORE DELETE ON file_revisions
             BEGIN
                 SELECT RAISE(ABORT, 'injected cleanup failure');
             END;",
        )
        .unwrap();

    let source = "int after_cleanup(void);\n";
    let parsed = parse(std::path::Path::new("main.c"), source);
    let fp = fingerprint("main.c", source, 2);
    let build = store.begin_index_build(false).unwrap();
    store
        .stage_file_updates(
            build,
            &[FileIndexUpdate {
                fingerprint: &fp,
                source: FileSource::Workspace,
                payload: FileIndexPayload::Ok(&parsed),
            }],
        )
        .unwrap();

    let outcome = store
        .commit_index_build(build, &IncludeGraphUpdate::default())
        .unwrap();
    assert_eq!(outcome.generation, 2);
    assert!(outcome
        .cleanup_warning
        .as_deref()
        .is_some_and(|warning| warning.contains("injected cleanup failure")));
    assert_eq!(store.semantic_generation().unwrap(), 2);
    assert!(store.symbols_by_name("before_cleanup").unwrap().is_empty());
    assert_eq!(store.symbols_by_name("after_cleanup").unwrap().len(), 1);
}
