use super::*;
use crate::store::{
    FileIndexPayload, FileIndexUpdate, IncludeGraphUpdate, ProtobufCSourceAssociation,
};

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

    assert!(store.declarations_by_name("old_name").unwrap().len() == 1);
    assert!(store.declarations_by_name("new_name").unwrap().is_empty());
    assert_eq!(store.semantic_generation().unwrap(), 1);

    let published = store
        .commit_index_build(build, &IncludeGraphUpdate::default())
        .unwrap();
    assert_eq!(published.generation, 2);
    assert!(published.cleanup_warning.is_none());
    assert!(store.declarations_by_name("old_name").unwrap().is_empty());
    assert!(store.declarations_by_name("new_name").unwrap().len() == 1);
}

#[test]
fn failed_protobuf_c_association_publish_preserves_the_active_generation() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).unwrap();
    upsert_source(
        &mut store,
        "device.pb-c.h",
        "typedef struct Demo__Device Demo__Device;\n",
    );
    let declaration_id = store
        .declarations_by_name("Demo__Device")
        .unwrap()
        .into_iter()
        .next()
        .expect("generated declaration")
        .id;

    let build = store.begin_index_build(false).unwrap();
    let graph = IncludeGraphUpdate {
        protobuf_c_sources: Some(vec![protobuf_c_source(declaration_id, "old.proto")]),
        ..Default::default()
    };
    let published = store.commit_index_build(build, &graph).unwrap();
    assert_eq!(published.generation, 2);

    let failed_build = store.begin_index_build(false).unwrap();
    let invalid_graph = IncludeGraphUpdate {
        protobuf_c_sources: Some(vec![protobuf_c_source(i64::MAX, "new.proto")]),
        ..Default::default()
    };
    store
        .commit_index_build(failed_build, &invalid_graph)
        .expect_err("invalid association must reject the publication");

    assert_eq!(store.semantic_generation().unwrap(), 2);
    let (sources, truncated) = store
        .protobuf_c_source_view()
        .sources_for_declaration_ids(&[declaration_id], 64)
        .unwrap();
    assert!(!truncated);
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].proto_path, "old.proto");
}

#[test]
fn protobuf_c_source_query_covers_the_full_semantic_candidate_budget() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).unwrap();
    let source = "typedef struct Demo__Device Demo__Device;\n".repeat(129);
    upsert_source(&mut store, "device.pb-c.h", &source);
    let declaration_ids: Vec<_> = store
        .declarations_by_name("Demo__Device")
        .unwrap()
        .into_iter()
        .map(|row| row.id)
        .take(129)
        .collect();
    assert_eq!(declaration_ids.len(), 129);

    let build = store.begin_index_build(false).unwrap();
    let graph = IncludeGraphUpdate {
        protobuf_c_sources: Some(vec![protobuf_c_source(
            declaration_ids[128],
            "candidate-129.proto",
        )]),
        ..Default::default()
    };
    store.commit_index_build(build, &graph).unwrap();

    let (sources, truncated) = store
        .protobuf_c_source_view()
        .sources_for_declaration_ids(&declaration_ids, 64)
        .unwrap();
    assert!(!truncated);
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].proto_path, "candidate-129.proto");
}

fn protobuf_c_source(declaration_id: i64, proto_path: &str) -> ProtobufCSourceAssociation {
    ProtobufCSourceAssociation {
        declaration_id,
        proto_path: proto_path.to_string(),
        proto_name: "demo.Device".to_string(),
        c_name: "Demo__Device".to_string(),
        kind: "message".to_string(),
        start_byte: 14,
        end_byte: 31,
        start_line: 0,
        start_col: 14,
        end_line: 0,
        end_col: 31,
        match_kind: "relative_path".to_string(),
        source_truncated: false,
    }
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
            "SELECT COUNT(*) FROM declarations WHERE name = 'before'",
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
            "SELECT COUNT(*) FROM declarations WHERE name = 'before'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let new_after_publish: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM declarations WHERE name = 'after'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!((old_after_publish, new_after_publish), (1, 0));
    transaction.commit().unwrap();
    assert_eq!(writer.declarations_by_name("after").unwrap().len(), 1);
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
        reader.declarations_by_name("after")
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
    let raw_revisions: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM file_revisions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        raw_revisions, 1,
        "starting a replacement build must reclaim abandoned staging revisions"
    );
    assert!(store.declarations_by_name("stable").unwrap().len() == 1);
    assert!(store.declarations_by_name("abandoned").unwrap().is_empty());
    store
        .commit_index_build(replacement, &IncludeGraphUpdate::default())
        .unwrap();
    assert!(store.declarations_by_name("stable").unwrap().len() == 1);
}

#[test]
fn inactive_cleanup_deletes_child_facts_before_revision_parents() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).unwrap();
    upsert_source(&mut store, "main.c", "int before_cleanup(void);\n");
    store
        .conn
        .execute_batch(
            "CREATE TRIGGER require_child_first_cleanup
             BEFORE DELETE ON file_revisions
             WHEN EXISTS(
                 SELECT 1 FROM declaration_facts WHERE revision_id = OLD.id
             )
             BEGIN
                 SELECT RAISE(ABORT, 'revision deleted before child facts');
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

    assert!(
        outcome.cleanup_warning.is_none(),
        "bulk cleanup must remove child facts before parent revisions: {:?}",
        outcome.cleanup_warning
    );
    let raw_revisions: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM file_revisions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(raw_revisions, 1);
}

#[test]
fn cleanup_preserves_parent_cascade_for_cross_file_fact_pairs() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).unwrap();
    upsert_source(&mut store, "a.c", "int stale_from_a(void);\n");
    upsert_source(&mut store, "b.c", "int stable_from_b(void);\n");

    let b_file_id: i64 = store
        .conn
        .query_row(
            "SELECT id FROM file_entries WHERE path = 'b.c'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    store
        .conn
        .execute(
            "UPDATE declaration_facts SET file_id = ?1
             WHERE name = 'stale_from_a'",
            [b_file_id],
        )
        .unwrap();
    let mut pre_cleanup_check = store.conn.prepare("PRAGMA foreign_key_check").unwrap();
    assert!(
        !pre_cleanup_check.exists([]).unwrap(),
        "the schema permits independently valid revision/file foreign keys"
    );
    drop(pre_cleanup_check);

    let source = "int fresh_from_a(void);\n";
    let parsed = parse(std::path::Path::new("a.c"), source);
    let fp = fingerprint("a.c", source, 2);
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

    assert!(
        outcome.cleanup_warning.is_none(),
        "bulk cleanup must fall back to revision-only cascade semantics: {:?}",
        outcome.cleanup_warning
    );
    assert!(store
        .declarations_by_name("stale_from_a")
        .unwrap()
        .is_empty());
    assert_eq!(store.declarations_by_name("fresh_from_a").unwrap().len(), 1);
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
    assert_eq!(revision_counts, (2, 2));
}

#[test]
fn failed_cleanup_rolls_back_and_next_build_retries_the_whole_debt() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).unwrap();
    upsert_source(&mut store, "old.c", "int old_only(void) { return 1; }\n");
    store
        .conn
        .execute_batch(
            "CREATE TRIGGER fail_orphan_call_string_cleanup
             BEFORE DELETE ON call_strings
             WHEN OLD.text = 'old_only'
             BEGIN
                 SELECT RAISE(ABORT, 'injected call-string cleanup failure');
             END;",
        )
        .unwrap();

    let source = "int new_only(void) { return 2; }\n";
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
    let outcome = store
        .commit_index_build(
            build,
            &IncludeGraphUpdate {
                clear_all: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(outcome
        .cleanup_warning
        .as_deref()
        .is_some_and(|warning| warning.contains("injected call-string cleanup failure")));
    assert_eq!(store.semantic_generation().unwrap(), 2);
    assert!(store.declarations_by_name("old_only").unwrap().is_empty());
    assert_eq!(store.declarations_by_name("new_only").unwrap().len(), 1);

    let raw_revisions: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM file_revisions", [], |row| row.get(0))
        .unwrap();
    let raw_old_declarations: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM declaration_facts WHERE name = 'old_only'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let cleanup_required: String = store
        .conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'cleanup_required'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        (
            raw_revisions,
            raw_old_declarations,
            cleanup_required.as_str()
        ),
        (2, 1, "1"),
        "a late cleanup failure must roll back every cleanup delete and persist retry debt"
    );
    let foreign_keys_enabled: i64 = store
        .conn
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        foreign_keys_enabled, 1,
        "cleanup failure must restore foreign-key enforcement"
    );

    store
        .conn
        .execute_batch("DROP TRIGGER fail_orphan_call_string_cleanup")
        .unwrap();
    let _next = store.begin_index_build(false).unwrap();
    let remaining_revisions: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM file_revisions", [], |row| row.get(0))
        .unwrap();
    let remaining_old_strings: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM call_strings WHERE text = 'old_only'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let cleanup_required: String = store
        .conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'cleanup_required'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining_revisions, 1);
    assert_eq!(remaining_old_strings, 0);
    assert_eq!(cleanup_required, "0");
    let mut foreign_key_check = store.conn.prepare("PRAGMA foreign_key_check").unwrap();
    assert!(!foreign_key_check.exists([]).unwrap());
}

#[test]
fn current_schema_without_cleanup_marker_is_audited_before_the_next_build() {
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
    store
        .conn
        .execute("DELETE FROM meta WHERE key = 'cleanup_required'", [])
        .unwrap();
    drop(store);

    let mut reopened = IndexStore::open(&db, dir.path()).unwrap();
    let backfilled_marker: String = reopened
        .conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'cleanup_required'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        backfilled_marker, "1",
        "an existing current-schema database without the marker needs one legacy audit"
    );

    let _next = reopened.begin_index_build(false).unwrap();
    let raw_revisions: i64 = reopened
        .conn
        .query_row("SELECT COUNT(*) FROM file_revisions", [], |row| row.get(0))
        .unwrap();
    let cleanup_required: String = reopened
        .conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'cleanup_required'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(raw_revisions, 1);
    assert_eq!(cleanup_required, "0");
}

#[test]
fn bulk_cleanup_removes_every_revision_owned_fact_family() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).unwrap();
    upsert_source(
        &mut store,
        "src/rich.c",
        "#include \"dep.h\"\n\
         struct Item { int value; };\n\
         typedef struct Item ItemAlias;\n\
         static int helper(int value) { return value; }\n\
         int caller(void) { return helper(1); }\n",
    );
    upsert_source(
        &mut store,
        "src/rich.go",
        "package rich\nimport device \"example.com/device\"\n\
         func Read() { device.Open() }\n",
    );
    upsert_source(&mut store, "src/broken.c", "((( guessed(value);\n");

    let revision_fact_tables = [
        "fallback_completion_facts",
        "declaration_facts",
        "package_facts",
        "import_facts",
        "include_facts",
        "record_facts",
        "member_facts",
        "type_alias_facts",
        "callable_anchor_facts",
        "call_site_facts",
    ];
    for table in revision_fact_tables {
        let count: i64 = store
            .conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(count > 0, "fixture must populate {table}");
    }

    let source = "int only_active(void) { return 1; }\n";
    let parsed = parse(std::path::Path::new("src/fresh.c"), source);
    let fp = fingerprint("src/fresh.c", source, 1);
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
    let outcome = store
        .commit_index_build(
            build,
            &IncludeGraphUpdate {
                clear_all: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(outcome.cleanup_warning.is_none());

    for table in revision_fact_tables {
        let stale: i64 = store
            .conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM {table} facts
                     WHERE facts.revision_id NOT IN (
                         SELECT revision_id FROM active_file_revisions
                     )"
                ),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stale, 0, "{table} retained an inactive revision");
    }
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
    assert!(store.declarations_by_name("old_only").unwrap().len() == 1);
    assert!(store.declarations_by_name("new_only").unwrap().is_empty());

    store
        .commit_index_build(
            build,
            &IncludeGraphUpdate {
                clear_all: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(store.declarations_by_name("old_only").unwrap().is_empty());
    assert!(store.declarations_by_name("new_only").unwrap().len() == 1);
}

#[test]
fn full_rebuild_retains_schema16_callable_signature_strings() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.sqlite");
    // Reproduce the production cold/full path: call-fact indexes do not exist
    // while commit-time cleanup validates every call_strings reference.
    let mut store = IndexStore::open_for_full_rebuild(&db, dir.path()).unwrap();
    let source = "int hotfix_target(const unsigned long value) { return (int)value; }\n";
    let parsed = parse(std::path::Path::new("main.c"), source);
    let fp = fingerprint("main.c", source, 1);
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

    let outcome = store
        .commit_index_build(
            build,
            &IncludeGraphUpdate {
                clear_all: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        outcome.cleanup_warning.is_none(),
        "schema-16 signature strings must not be targeted by cleanup: {:?}",
        outcome.cleanup_warning
    );

    let (shape, canonical, presentation): (String, String, String) = store
        .conn
        .query_row(
            "SELECT shape.text, canonical.text, presentation.text
             FROM callable_anchor_facts anchor
             JOIN call_strings shape ON shape.id = anchor.signature_id
             JOIN call_strings canonical ON canonical.id = anchor.canonical_signature_id
             JOIN call_strings presentation ON presentation.id = anchor.presentation_signature_id
             WHERE anchor.kind = 0 AND anchor.name_id = (
                 SELECT id FROM call_strings WHERE text = 'hotfix_target'
             )",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_ne!(shape, canonical);
    assert_ne!(shape, presentation);
    assert!(!canonical.is_empty());
    assert!(!presentation.is_empty());

    let mut foreign_key_check = store.conn.prepare("PRAGMA foreign_key_check").unwrap();
    assert!(!foreign_key_check.exists([]).unwrap());
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
    assert_eq!(
        guard.store().declarations_by_name("current").unwrap().len(),
        1
    );
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
    assert!(store
        .declarations_by_name("before_cleanup")
        .unwrap()
        .is_empty());
    assert_eq!(
        store.declarations_by_name("after_cleanup").unwrap().len(),
        1
    );
}
