use super::*;
use crate::semantic_model::SemanticFamily;
use crate::store::views::relation_facts::RelationFactLookup;

#[test]
fn relation_foundation_saved_delete_and_rename_remove_reverse_facts() {
    let temp = tempdir().unwrap();
    let db = temp.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, temp.path()).unwrap();
    upsert_source(
        &mut store,
        "old.cpp",
        "int x; void f(){x++;} struct B{};struct D:B{};",
    );
    assert_eq!(
        store
            .relation_fact_view()
            .scan(
                0,
                RelationFactLookup::Name("x"),
                SemanticFamily::CFamily,
                0,
                256
            )
            .unwrap()
            .rows
            .len(),
        1
    );
    upsert_source(
        &mut store,
        "old.cpp",
        "int x; void f(){} struct B{};struct D{};",
    );
    assert!(store
        .relation_fact_view()
        .scan(
            0,
            RelationFactLookup::Name("x"),
            SemanticFamily::CFamily,
            0,
            256
        )
        .unwrap()
        .rows
        .is_empty());
    assert!(store
        .relation_fact_view()
        .scan(
            1,
            RelationFactLookup::Target("B"),
            SemanticFamily::CFamily,
            0,
            256
        )
        .unwrap()
        .rows
        .is_empty());
    upsert_source(&mut store, "new.cpp", "int x; void f(){x++;}");
    store.delete_file("old.cpp").unwrap();
    let rows = store
        .relation_fact_view()
        .scan(
            0,
            RelationFactLookup::Name("x"),
            SemanticFamily::CFamily,
            0,
            256,
        )
        .unwrap()
        .rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].path, "new.cpp");
    store.delete_file("new.cpp").unwrap();
    assert!(store
        .relation_fact_view()
        .scan(
            0,
            RelationFactLookup::Name("x"),
            SemanticFamily::CFamily,
            0,
            256
        )
        .unwrap()
        .rows
        .is_empty());
    let mut integrity = store.conn.prepare("PRAGMA foreign_key_check").unwrap();
    assert!(!integrity.exists([]).unwrap());
    let journal: String = store
        .conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(journal, "wal");
    let synchronous: i64 = store
        .conn
        .query_row("PRAGMA synchronous", [], |r| r.get(0))
        .unwrap();
    assert_eq!(synchronous, 1);
}

#[test]
fn relation_foundation_missing_fact_group_is_unavailable_even_without_rows() {
    let temp = tempdir().unwrap();
    let mut store = IndexStore::open(&temp.path().join("index.sqlite"), temp.path()).unwrap();
    let source = "int x; void f(){x++;}";
    let parsed = crate::parser::parse_with_handle(
        std::path::Path::new("main.c"),
        source,
        None,
        crate::parser::ParseFacts::DECLARATIONS,
    );
    let fingerprint = FileFingerprint {
        path: "main.c".into(),
        extension: "c".into(),
        size: source.len() as u64,
        mtime_ns: 1,
        hash: "revision1".into(),
    };
    store.upsert_file_index(&fingerprint, &parsed).unwrap();
    let page = store
        .relation_fact_view()
        .scan(
            0,
            RelationFactLookup::Path("main.c"),
            SemanticFamily::CFamily,
            0,
            256,
        )
        .unwrap();
    assert!(page.rows.is_empty());
    assert!(page.unavailable);
    assert!(page.partial);
}

#[test]
fn relation_foundation_schema_upgrade_cancellation_keeps_old_capabilities() {
    schema_upgrade_cancellation(false);
    schema_upgrade_cancellation(true);
}
fn schema_upgrade_cancellation(explicit: bool) {
    let workspace = tempdir().unwrap();
    let explicit_path = explicit.then(|| workspace.path().join("explicit.sqlite"));
    std::fs::write(
        workspace.path().join("main.c"),
        "int original; void f(){original++;}",
    )
    .unwrap();
    crate::indexer::index_workspace(
        workspace.path(),
        crate::indexer::IndexOptions {
            force: true,
            db_path: explicit_path.clone(),
            ..Default::default()
        },
        |_| {},
    )
    .unwrap();
    let old_path = explicit_path
        .clone()
        .unwrap_or_else(|| crate::pathing::default_index_path(workspace.path()).unwrap());
    let old = IndexStore::open_readonly(&old_path).unwrap();
    old.conn.execute_batch("DROP TABLE relation_source_facts;DROP TABLE relation_file_coverage;UPDATE meta SET value='34' WHERE key='schema_version';").unwrap();
    assert!(!old.relation_fact_view().available().unwrap());
    let coordinator = crate::build_coordinator::BuildCoordinator::with_policy_and_sampler(
        crate::build_coordinator::BuildPolicy::default(),
        || 0,
    );
    let permit = coordinator
        .try_acquire(
            workspace.path().to_path_buf(),
            crate::build_coordinator::BuildKind::FullIndex,
        )
        .unwrap();
    permit
        .reserve(256 * 1024 * 1024, 200 * 1024 * 1024)
        .unwrap();
    let cancellation = permit.cancellation();
    let result = crate::indexer::index_workspace_with_permit(
        workspace.path(),
        crate::indexer::IndexOptions {
            db_path: explicit_path.clone(),
            ..Default::default()
        },
        &permit,
        |status| {
            if status
                .phase
                .as_deref()
                .is_some_and(|p| p.starts_with("publishing"))
            {
                cancellation.cancel();
            }
        },
    );
    assert!(result.unwrap_err().to_string().contains("cancelled"));
    assert_eq!(
        explicit_path
            .unwrap_or_else(|| crate::pathing::default_index_path(workspace.path()).unwrap()),
        old_path
    );
    assert_eq!(old.declaration_view().by_name("original").unwrap().len(), 1);
    assert!(!old.relation_fact_view().available().unwrap());
}
