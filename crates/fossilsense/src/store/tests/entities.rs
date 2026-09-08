use super::*;
use crate::semantic_model::{EntityIdentity, SemanticDeclarationRole};

#[test]
fn entity_location_index_retains_guarded_occurrences_and_uses_identity_index() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).unwrap();
    upsert_source(
        &mut store,
        "api.h",
        "#ifndef API_H\n#define API_H\nint run_task(int input);\n#endif\n",
    );
    upsert_source(
        &mut store,
        "impl.c",
        "int run_task(int value) { return value; }\n",
    );
    let facts = store.declaration_view().by_name("run_task").unwrap();
    assert_eq!(facts.len(), 2);
    assert_ne!(facts[0].fact.guard, facts[1].fact.guard);
    let identity = EntityIdentity::from_declaration(&facts[0].fact);
    for row in &facts {
        assert_eq!(
            identity.digest(),
            EntityIdentity::digest_for_declaration(&row.fact)
        );
        assert_eq!(
            store
                .entity_view()
                .identity_for_declaration(row.id)
                .unwrap(),
            Some(identity.digest())
        );
    }
    let page = store
        .entity_view()
        .occurrences(&identity, SemanticDeclarationRole::Definition, 0, 256)
        .unwrap();
    assert_eq!(page.rows.len(), 1);
    assert!(!page.truncated);
    let loaded = store
        .declaration_view()
        .by_ids(&[page.rows[0].declaration_id])
        .unwrap();
    assert_eq!(loaded[0].fact.path, "impl.c");
    let mut statement = store.conn.prepare("EXPLAIN QUERY PLAN SELECT e.declaration_id FROM entity_occurrences e JOIN active_file_revisions a ON a.revision_id=e.revision_id WHERE e.family=?1 AND e.domain=?2 AND e.identity_digest=?3 AND e.role=1 AND e.declaration_id>0 ORDER BY e.declaration_id LIMIT 256").unwrap();
    let plan = statement
        .query_map(
            rusqlite::params![
                identity.family as u8,
                identity.domain as u8,
                identity.digest().as_slice()
            ],
            |row| row.get::<_, String>(3),
        )
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
        .join("\n");
    assert!(plan.contains("idx_entity_occurrence_identity"), "{plan}");
    assert!(!plan.contains("TEMP B-TREE"), "{plan}");
    let locator = &facts[0].fact.identity.locator.fingerprint;
    assert_eq!(
        store
            .declaration_view()
            .by_locator_fingerprint_family(locator, identity.family)
            .unwrap()
            .unwrap()
            .id,
        facts[0].id
    );
    let locator_plan: String = store.conn.query_row(
        "EXPLAIN QUERY PLAN SELECT d.id FROM declarations d WHERE d.locator_fingerprint=unhex(?1) LIMIT 2",
        rusqlite::params![locator], |row| row.get(3)).unwrap();
    assert!(
        locator_plan.contains("idx_declaration_facts_locator"),
        "{locator_plan}"
    );
    drop(statement);
    upsert_source(&mut store, "impl.c", "/* implementation removed */\n");
    assert!(store
        .entity_view()
        .occurrences(&identity, SemanticDeclarationRole::Definition, 0, 256)
        .unwrap()
        .rows
        .is_empty());
    let violations: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0);
}

#[test]
fn entity_location_pages_have_bounded_stable_continuations() {
    let dir = tempdir().unwrap();
    let mut store = IndexStore::open(&dir.path().join("index.sqlite"), dir.path()).unwrap();
    upsert_source(&mut store, "api.h", &"int run_task(void);\n".repeat(300));
    let seed = store
        .declaration_view()
        .by_name_limited("run_task", 1)
        .unwrap()
        .0
        .remove(0);
    let identity = EntityIdentity::from_declaration(&seed.fact);
    let first = store
        .entity_view()
        .occurrences(&identity, SemanticDeclarationRole::Declaration, 0, 9999)
        .unwrap();
    assert_eq!(first.rows.len(), 256);
    assert!(first.truncated);
    let second = store
        .entity_view()
        .occurrences(
            &identity,
            SemanticDeclarationRole::Declaration,
            first.after_id.unwrap(),
            256,
        )
        .unwrap();
    assert_eq!(second.rows.len(), 44);
    assert!(!second.truncated);
    assert!(first.rows.iter().all(|a| second
        .rows
        .iter()
        .all(|b| a.declaration_id != b.declaration_id)));
}

#[test]
fn entity_location_publication_rejects_missing_or_mismatched_occurrences() {
    for corruption in [
        "DELETE FROM entity_occurrences",
        "UPDATE entity_occurrences SET role=3",
    ] {
        let dir = tempdir().unwrap();
        let mut store = IndexStore::open(&dir.path().join("index.sqlite"), dir.path()).unwrap();
        upsert_source(&mut store, "impl.c", "int run_task(void) { return 1; }\n");
        store.conn.execute(corruption, []).unwrap();
        let result = store.prepare_full_build_publication();
        assert!(
            result.is_err(),
            "corrupt entity lookup was publishable: {corruption}"
        );
    }
}
