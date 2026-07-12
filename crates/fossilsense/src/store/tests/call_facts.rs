use super::*;

#[test]
fn callable_and_call_site_facts_round_trip_through_active_views() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).unwrap();
    upsert_source(
        &mut store,
        "src/main.c",
        "static int helper(int v) { return v; }\nint caller(void) { return helper(3); }\n",
    );

    let helper = store.call_fact_view().anchors_by_name("helper").unwrap();
    assert_eq!(helper.len(), 1);
    assert_eq!(helper[0].path, "src/main.c");
    assert_eq!(helper[0].linkage_kind, "internal");
    assert_eq!(helper[0].min_arity, Some(1));
    assert_eq!(helper[0].role, "definition");
    assert_eq!(helper[0].declaration_range.start.line, 0);
    assert_eq!(helper[0].declaration_range.end.line, 0);
    assert!(helper[0].body_range.is_some());

    let calls = store
        .call_fact_view()
        .call_sites_by_callee("helper")
        .unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].call_form, "direct_name");
    assert_eq!(calls[0].argument_count, Some(1));
    let by_caller = store
        .call_fact_view()
        .call_sites_by_caller(&calls[0].caller_entity_key)
        .unwrap();
    assert_eq!(by_caller, calls);

    let coverage = store.call_fact_view().coverage().unwrap();
    assert_eq!(coverage.eligible_files, 1);
    assert_eq!(coverage.analyzed_files, 1);
    assert_eq!(coverage.fallback_files, 0);
    assert_eq!(coverage.callable_anchors, 2);
    assert_eq!(coverage.call_sites, 1);
}

#[test]
fn schema_15_persists_compact_typed_call_facts() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).unwrap();
    upsert_source(
        &mut store,
        "src/main.c",
        "static int helper(int v) { return v; }\nint caller(void) { return helper(3); }\n",
    );

    let version: String = store
        .conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version, "15");

    let anchor_columns: Vec<(String, String)> = store
        .conn
        .prepare("PRAGMA table_info(callable_anchor_facts)")
        .unwrap()
        .query_map([], |row| Ok((row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(anchor_columns.contains(&("entity_digest".into(), "BLOB".into())));
    assert!(anchor_columns.contains(&("name_id".into(), "INTEGER".into())));
    assert!(anchor_columns.contains(&("flags".into(), "INTEGER".into())));
    assert!(!anchor_columns.iter().any(|(name, _)| name == "entity_key"));

    let site_columns: Vec<(String, String)> = store
        .conn
        .prepare("PRAGMA table_info(call_site_facts)")
        .unwrap()
        .query_map([], |row| Ok((row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(site_columns.contains(&("caller_anchor_id".into(), "INTEGER".into())));
    assert!(site_columns.contains(&("callee_name_id".into(), "INTEGER".into())));
    for removed in [
        "caller_entity_key",
        "site_fingerprint",
        "expression_start_line",
        "expression_start_col",
        "expression_end_line",
        "expression_end_col",
    ] {
        assert!(!site_columns.iter().any(|(name, _)| name == removed));
    }

    let (entity_type, entity_bytes, caller_type, form_type, joined): (
        String,
        i64,
        String,
        String,
        i64,
    ) = store
        .conn
        .query_row(
            "SELECT typeof(a.entity_digest), length(a.entity_digest),
                    typeof(c.caller_anchor_id), typeof(c.call_form),
                    (a.id = c.caller_anchor_id)
             FROM call_site_facts c
             JOIN callable_anchor_facts a ON a.id = c.caller_anchor_id
             LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(entity_type, "blob");
    assert_eq!(entity_bytes, 12);
    assert_eq!(caller_type, "integer");
    assert_eq!(form_type, "integer");
    assert_eq!(joined, 1);
}

#[test]
fn dirty_revision_replaces_old_call_facts_without_leaking_stale_rows() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).unwrap();
    upsert_source(
        &mut store,
        "main.c",
        "int first(void); int caller(void) { return first(); }\n",
    );
    assert_eq!(
        store
            .call_fact_view()
            .call_sites_by_callee("first")
            .unwrap()
            .len(),
        1
    );

    upsert_source(
        &mut store,
        "main.c",
        "int second(void); int caller(void) { return second(); }\n",
    );
    assert!(store
        .call_fact_view()
        .call_sites_by_callee("first")
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .call_fact_view()
            .call_sites_by_callee("second")
            .unwrap()
            .len(),
        1
    );
    let duplicate_strings: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM (
                SELECT text FROM call_strings GROUP BY text HAVING COUNT(*) > 1
             )",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(duplicate_strings, 0);
}

#[test]
fn external_headers_contribute_callable_declarations_without_body_calls() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).unwrap();
    upsert_with_source(
        &mut store,
        "C:/sdk/api.h",
        "int sdk_open(int port);\n",
        FileSource::External,
    );
    let anchors = store.call_fact_view().anchors_by_name("sdk_open").unwrap();
    assert_eq!(anchors.len(), 1);
    assert_eq!(anchors[0].source, "external");
    assert_eq!(anchors[0].role, "declaration");
    assert!(store
        .call_fact_view()
        .call_sites_by_caller(&anchors[0].entity_key)
        .unwrap()
        .is_empty());
}
