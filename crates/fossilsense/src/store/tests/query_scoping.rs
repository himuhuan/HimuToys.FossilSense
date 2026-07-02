use super::*;

#[test]
fn test_query_and_server_scoping() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");

    // Header 1: reach1.h defines struct W { int field_a; }
    let src1 = "struct W { int field_a; };\ntypedef struct W WT;\n";
    let index1 = parse(std::path::Path::new("reach1.h"), src1);
    let fp1 = FileFingerprint {
        path: "reach1.h".to_string(),
        extension: "h".to_string(),
        size: src1.len() as u64,
        mtime_ns: 1,
        hash: "hash1".to_string(),
    };
    store.upsert_file_index(&fp1, &index1).expect("upsert");

    // Header 2: reach2.h defines struct W { int field_b; }
    let src2 = "struct W { int field_b; };\ntypedef struct W WT;\n";
    let index2 = parse(std::path::Path::new("reach2.h"), src2);
    let fp2 = FileFingerprint {
        path: "reach2.h".to_string(),
        extension: "h".to_string(),
        size: src2.len() as u64,
        mtime_ns: 2,
        hash: "hash2".to_string(),
    };
    store.upsert_file_index(&fp2, &index2).expect("upsert");

    // 1. Scoped query: unreachable same-named struct W fields do not contaminate.
    // If we query for "W" with a scope of only reach1.h:
    let reach_set1: HashSet<String> = vec!["reach1.h".to_string()].into_iter().collect();
    let reach_scope1 = crate::reachability::ReachScope {
        files: reach_set1,
        open: false,
        reason: None,
    };
    let ctx1 = crate::resolver::ResolveContext {
        current_path: None,
        reach: Some(&reach_scope1),
    };

    let candidates1 = store
        .resolve_record_candidates(&["W"], Some(&ctx1))
        .expect("c1");
    assert_eq!(candidates1.len(), 2); // both are returned, but sorted by tier
    assert_eq!(candidates1[0].tier, crate::model::ScopeTier::Reachable);
    assert_eq!(candidates1[0].path, "reach1.h");
    assert_eq!(candidates1[1].tier, crate::model::ScopeTier::Global);
    assert_eq!(candidates1[1].path, "reach2.h");

    // Fields for highest-tier candidate only (mocking member completion filter)
    let highest_tier = candidates1[0].tier;
    let highest_candidates: Vec<i64> = candidates1
        .iter()
        .filter(|c| c.tier == highest_tier)
        .map(|c| c.id)
        .collect();
    let fields1 = store
        .fields_for_records(&highest_candidates)
        .expect("fields1");
    assert_eq!(fields1, vec!["field_a".to_string()]); // field_b is not returned because it's only in Global tier candidate!

    // 2. Same WT alias resolves differently under different reachable sets
    let reach_set2: HashSet<String> = vec!["reach2.h".to_string()].into_iter().collect();
    let reach_scope2 = crate::reachability::ReachScope {
        files: reach_set2,
        open: false,
        reason: None,
    };
    let ctx2 = crate::resolver::ResolveContext {
        current_path: None,
        reach: Some(&reach_scope2),
    };

    let candidates2 = store
        .resolve_record_candidates(&["WT"], Some(&ctx2))
        .expect("c2");
    assert_eq!(candidates2[0].tier, crate::model::ScopeTier::Reachable);
    assert_eq!(candidates2[0].path, "reach2.h");

    let highest_candidates2: Vec<i64> = candidates2
        .iter()
        .filter(|c| c.tier == candidates2[0].tier)
        .map(|c| c.id)
        .collect();
    let fields2 = store
        .fields_for_records(&highest_candidates2)
        .expect("fields2");
    assert_eq!(fields2, vec!["field_b".to_string()]);

    // 3. Fallback ranking: unresolved receiver fallback ranks reachable before global
    let fallback = store
        .fallback_field_candidates("field", 100, Some(&ctx1))
        .expect("fallback");
    // "field_a" is Reachable (rank 4), "field_b" is Global (rank 1). So "field_a" outranks "field_b"!
    assert_eq!(fallback[0].0, "field_a");
    assert_eq!(fallback[0].1, crate::model::ScopeTier::Reachable);
    assert_eq!(fallback[1].0, "field_b");
    assert_eq!(fallback[1].1, crate::model::ScopeTier::Global);
}

#[test]
fn record_same_display_name_across_workspace_and_external() {
    // R4 identity: a workspace `struct W` and an external `struct W` are two
    // distinct record identities, never one merged container. The first-layer
    // external record ranks above the unreachable workspace one (External >
    // Global), and their fields never cross-contaminate.
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    upsert_with_source(
        &mut store,
        "ws/w.h",
        "struct W { int ws_field; };\n",
        FileSource::Workspace,
    );
    let ext_path = "C:/ext/w.h";
    upsert_with_source(
        &mut store,
        ext_path,
        "struct W { int ext_field; };\n",
        FileSource::External,
    );
    // Promote the external header to first-layer so it resolves to External.
    store
        .mark_directly_included(&[ext_path.to_string()])
        .expect("mark");

    let candidates = store
        .resolve_record_candidates(&["W"], None)
        .expect("candidates");
    assert_eq!(
        candidates.len(),
        2,
        "same display name stays two identities"
    );
    assert_ne!(candidates[0].id, candidates[1].id);
    // First-layer external outranks the unreachable workspace record.
    assert_eq!(candidates[0].tier, crate::model::ScopeTier::External);
    assert_eq!(candidates[0].path, ext_path);
    assert_eq!(candidates[1].tier, crate::model::ScopeTier::Global);
    assert_eq!(candidates[1].path, "ws/w.h");

    // Fields stay attributed to their own record identity.
    let ext_fields = store.fields_for_records(&[candidates[0].id]).expect("ext");
    assert_eq!(ext_fields, vec!["ext_field".to_string()]);
    let ws_fields = store.fields_for_records(&[candidates[1].id]).expect("ws");
    assert_eq!(ws_fields, vec!["ws_field".to_string()]);
}

#[test]
fn open_scope_softens_but_keeps_record_and_field_candidates() {
    use std::collections::HashSet;
    // Asymmetry contract (D5): for member completion, an open reach scope
    // softens out-of-set candidates to `Unknown` but must NOT clear them.
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    upsert_source(&mut store, "inc/w.h", "struct W { int width; };\n");
    upsert_source(&mut store, "other/w.h", "struct W { int height; };\n");

    // Open scope reaching neither header (e.g. opened by an ambiguous include).
    let reach = crate::reachability::ReachScope {
        files: HashSet::new(),
        open: true,
        reason: Some(crate::reachability::OpenReason::AmbiguousInclude),
    };
    let ctx = crate::resolver::ResolveContext {
        current_path: None,
        reach: Some(&reach),
    };

    // Record candidates soften to Unknown, but both stay (not emptied).
    let candidates = store
        .resolve_record_candidates(&["W"], Some(&ctx))
        .expect("candidates");
    assert_eq!(
        candidates.len(),
        2,
        "open scope must not clear record candidates"
    );
    assert!(candidates
        .iter()
        .all(|c| c.tier == crate::model::ScopeTier::Unknown));

    // Field fallback stays broad too: both fields surface, tier-annotated
    // Unknown rather than dropped.
    let fallback = store
        .fallback_field_candidates("", 100, Some(&ctx))
        .expect("fallback");
    let names: Vec<&str> = fallback.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"width"));
    assert!(names.contains(&"height"));
    assert!(fallback
        .iter()
        .all(|(_, t)| *t == crate::model::ScopeTier::Unknown));
}

// --- R7: error degradation — bad DB must not panic -----------------------
