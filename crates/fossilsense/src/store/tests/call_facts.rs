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
