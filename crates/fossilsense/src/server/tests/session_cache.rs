use super::*;

#[tokio::test]
async fn local_word_cache_is_keyed_by_document_version() {
    let cache = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let uri = Url::parse("file:///tmp/cache-test.c").expect("uri");

    let first = local_words_for_cache(&cache, &uri, 1, "int cached_word;").await;
    let second = local_words_for_cache(&cache, &uri, 1, "int changed_word;").await;
    assert!(Arc::ptr_eq(&first, &second));
    assert!(second.iter().any(|word| word == "cached_word"));
    assert!(!second.iter().any(|word| word == "changed_word"));

    let third = local_words_for_cache(&cache, &uri, 2, "int changed_word;").await;
    assert!(!Arc::ptr_eq(&second, &third));
    assert!(third.iter().any(|word| word == "changed_word"));
}

#[tokio::test]
async fn workspace_session_change_invalidates_live_document_caches() {
    let documents = super::DocumentStore::default();
    let cache = super::CacheLedger::default();
    let session = super::WorkspaceSession::new(documents.clone(), cache.clone());
    let root = tempdir().expect("root");
    let path = root.path().join("src/main.c");
    let uri = Url::from_file_path(&path).expect("uri");

    documents
        .open_document(uri.clone(), 1, "int cached_word;\n".to_string())
        .await;
    let words = documents
        .local_words_for(&uri, 1, "int cached_word;\n")
        .await;
    assert!(words.contains("cached_word"));
    let parsed = Arc::new(crate::parser::parse(&path, "int cached_word;\n"));
    documents
        .store_live_parse_for_test(uri.clone(), 1, parsed)
        .await;
    cache
        .record_completion_memo(uri.clone(), "ca".to_string(), 7, vec![vec![0usize, 1usize]])
        .await;
    cache.mark_reference_search_cache_for_test("root", "cached_word", 7);

    session
        .change_document(uri.clone(), 2, "int changed_word;\n".to_string())
        .await;

    let snapshot = documents.snapshot(&uri).await.expect("open document");
    assert_eq!(snapshot.version, 2);
    assert!(snapshot.text.contains("changed_word"));
    assert!(
        documents.live_parse_for_test(&uri).await.is_none(),
        "did_change must clear the live parse cache for the edited document"
    );
    assert!(
        documents
            .local_word_cache_entry_for_test(&uri)
            .await
            .is_none(),
        "did_change must invalidate local words so completion sees the new text"
    );
    assert!(
        cache.completion_memo_for_test(&uri).await.is_none(),
        "did_change must clear per-document completion narrowing state"
    );
    assert_eq!(
        cache.reference_search_cache_len_for_test(),
        0,
        "document changes must clear complete reference search results"
    );
}

#[tokio::test]
async fn workspace_session_close_clears_live_only_state_not_indexed_workspace_data() {
    let documents = super::DocumentStore::default();
    let cache = super::CacheLedger::default();
    let session = super::WorkspaceSession::new(documents.clone(), cache.clone());
    let root = tempdir().expect("root");
    let root_path = root.path().to_path_buf();
    let file_path = root.path().join("src/main.c");
    let uri = Url::from_file_path(&file_path).expect("uri");

    documents
        .open_document(uri.clone(), 1, "int indexed_symbol;\n".to_string())
        .await;
    documents
        .store_live_parse_for_test(
            uri.clone(),
            1,
            Arc::new(crate::parser::parse(&file_path, "int indexed_symbol;\n")),
        )
        .await;
    let _ = documents
        .local_words_for(&uri, 1, "int indexed_symbol;\n")
        .await;
    cache
        .set_name_table_for_test(
            root_path.clone(),
            Arc::new(crate::query::NameTable::build(vec![(
                1,
                "indexed_symbol".to_string(),
                false,
            )])),
        )
        .await;
    cache
        .set_indexed_file_list_for_test(
            root_path.clone(),
            Arc::new(vec![("src/main.c".to_string(), file_path.clone())]),
        )
        .await;

    session.close_document(&uri).await;

    assert!(documents.snapshot(&uri).await.is_none());
    assert!(documents.live_parse_for_test(&uri).await.is_none());
    assert!(documents
        .local_word_cache_entry_for_test(&uri)
        .await
        .is_none());
    assert!(
        cache.name_table(&root_path).await.is_some(),
        "closing an editor buffer must not delete indexed symbol data"
    );
    assert!(
        cache.indexed_file_list(&root_path).await.is_some(),
        "closing an editor buffer must not delete indexed reference scope"
    );
}

#[tokio::test]
async fn cache_ledger_publishes_full_and_dirty_read_models_with_generations() {
    let root = tempdir().expect("root");
    let root_path = root.path().to_path_buf();
    write_workspace_file(root.path(), "Makefile", "all:\n");
    write_workspace_file(root.path(), "src/main.c", "int alpha_symbol;\n");
    crate::indexer::index_workspace(
        root.path(),
        crate::indexer::IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("initial index");

    let service = test_backend_service();
    let full = service
        .inner()
        .session
        .cache
        .publish_full_index(&service.inner().client, root_path.clone())
        .await
        .expect("publish full");
    assert_eq!(full.symbol_count, 1);
    assert_eq!(full.reference_file_count, 1);
    let full_snapshot = service
        .inner()
        .session
        .snapshot_for_root(root_path.clone())
        .await;
    assert!(full_snapshot.name_table.is_some());
    assert!(full_snapshot.reach_graph.is_some());
    assert!(full_snapshot.include_table.is_some());
    assert_eq!(
        full_snapshot
            .project_context
            .as_ref()
            .expect("project contexts")
            .projects()
            .len(),
        1
    );
    assert!(full_snapshot.indexed_files.is_some());
    assert_ne!(full_snapshot.generation.as_u64(), 0);

    write_workspace_file(
        root.path(),
        "src/main.c",
        "int beta_symbol;\nint gamma_symbol;\n",
    );
    crate::indexer::index_dirty_files(
        root.path(),
        vec![crate::indexer::DirtyFileChange {
            absolute_path: root.path().join("src/main.c"),
            kind: crate::indexer::DirtyFileKind::Upsert,
        }],
        crate::indexer::IndexOptions {
            force: false,
            ..Default::default()
        },
        |_| {},
    )
    .expect("dirty index");
    let dirty = service
        .inner()
        .session
        .cache
        .publish_dirty_index(
            &service.inner().client,
            root_path.clone(),
            &["src/main.c".to_string()],
            &[],
        )
        .await
        .expect("publish dirty");
    assert_eq!(dirty.symbol_count, 2);
    let dirty_snapshot = service.inner().session.snapshot_for_root(root_path).await;
    assert_ne!(full_snapshot.generation, dirty_snapshot.generation);
    assert_eq!(dirty_snapshot.name_table.as_ref().expect("table").len(), 2);
    assert!(dirty_snapshot.project_context.is_some());
}

#[tokio::test]
async fn cache_ledger_full_publish_rebuilds_read_model_contents() {
    let root = tempdir().expect("root");
    let root_path = root.path().to_path_buf();
    write_workspace_file(
        root.path(),
        "src/main.c",
        "#include \"api.h\"\n#include \"missing.h\"\nint alpha_symbol;\n",
    );
    write_workspace_file(root.path(), "src/api.h", "int api_symbol(void);\n");
    crate::indexer::index_workspace(
        root.path(),
        crate::indexer::IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let service = test_backend_service();
    let report = service
        .inner()
        .session
        .cache
        .publish_full_index(&service.inner().client, root_path.clone())
        .await
        .expect("publish");
    assert_eq!(report.symbol_count, 2);
    assert_eq!(report.reference_file_count, 2);
    assert!(!report.degraded.any());

    let snapshot = service
        .inner()
        .session
        .snapshot_for_root(root_path.clone())
        .await;
    let name_table = snapshot.name_table.as_ref().expect("name table");
    assert!(name_table
        .search_ranked("api_symbol", 10)
        .iter()
        .any(|hit| hit.name == "api_symbol"));

    let reach_graph = snapshot.reach_graph.as_ref().expect("reach graph");
    let reachable = reach_graph
        .read()
        .expect("reach graph read")
        .reachable("src/main.c");
    assert!(reachable.files.contains("src/api.h"));
    assert!(reachable.open);
    assert_eq!(
        reachable.reason,
        Some(crate::reachability::OpenReason::UnresolvedInclude)
    );

    let include_table = snapshot.include_table.as_ref().expect("include table");
    assert_eq!(include_table.len(), 2);
    assert_eq!(include_table.edge_count(), 1);

    let indexed_files = snapshot.indexed_files.as_ref().expect("indexed files");
    let rels: Vec<&str> = indexed_files
        .iter()
        .map(|(rel, _abs)| rel.as_str())
        .collect();
    assert_eq!(rels, vec!["src/api.h", "src/main.c"]);
    assert!(indexed_files
        .iter()
        .all(|(_rel, abs)| abs.starts_with(&root_path)));
}

#[tokio::test]
async fn dirty_reach_graph_refresh_publishes_new_arc_generation() {
    let root = tempdir().expect("root");
    let root_path = root.path().to_path_buf();
    write_workspace_file(root.path(), "src/main.c", "#include \"old.h\"\n");
    write_workspace_file(root.path(), "src/old.h", "int old_symbol;\n");
    write_workspace_file(root.path(), "src/new.h", "int new_symbol;\n");
    crate::indexer::index_workspace(
        root.path(),
        crate::indexer::IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("initial index");

    let service = test_backend_service();
    service
        .inner()
        .session
        .cache
        .publish_full_index(&service.inner().client, root_path.clone())
        .await
        .expect("publish full");
    let before = service
        .inner()
        .session
        .snapshot_for_root(root_path.clone())
        .await;
    let before_graph = before.reach_graph.as_ref().expect("before graph").clone();

    write_workspace_file(root.path(), "src/main.c", "#include \"new.h\"\n");
    crate::indexer::index_dirty_files(
        root.path(),
        vec![crate::indexer::DirtyFileChange {
            absolute_path: root.path().join("src/main.c"),
            kind: crate::indexer::DirtyFileKind::Upsert,
        }],
        crate::indexer::IndexOptions {
            force: false,
            ..Default::default()
        },
        |_| {},
    )
    .expect("dirty index");
    service
        .inner()
        .session
        .cache
        .publish_dirty_index(
            &service.inner().client,
            root_path.clone(),
            &["src/main.c".to_string()],
            &["src/main.c".to_string()],
        )
        .await
        .expect("publish dirty");

    let after = service.inner().session.snapshot_for_root(root_path).await;
    let after_graph = after.reach_graph.as_ref().expect("after graph");

    assert!(
        !Arc::ptr_eq(&before_graph, after_graph),
        "dirty reach refresh must publish a fresh Arc so captured snapshots stay stable"
    );
    assert!(before_graph
        .read()
        .expect("before read")
        .reachable("src/main.c")
        .files
        .contains("src/old.h"));
    assert!(after_graph
        .read()
        .expect("after read")
        .reachable("src/main.c")
        .files
        .contains("src/new.h"));
}

#[tokio::test]
async fn cache_ledger_completion_memo_reuses_prefix_only_with_same_generation() {
    let cache = super::CacheLedger::default();
    let uri = Url::parse("file:///tmp/memo.c").expect("uri");

    cache
        .record_completion_memo(uri.clone(), "fo".to_string(), 42, vec![vec![1, 2, 3]])
        .await;

    let reused = cache.completion_memo_pools(&uri, 42, "foo", 1).await;
    assert_eq!(reused.hit_kind, "pool");
    assert_eq!(reused.prior_pools, vec![Some(vec![1, 2, 3])]);

    let hot = cache.completion_memo_pools(&uri, 42, "fo", 1).await;
    assert_eq!(hot.hit_kind, "hot");

    let stale = cache.completion_memo_pools(&uri, 43, "foo", 1).await;
    assert_eq!(stale.hit_kind, "cold");
    assert_eq!(stale.prior_pools, vec![None]);
}

#[tokio::test]
async fn cache_ledger_clears_reference_search_cache_after_document_and_index_changes() {
    let documents = super::DocumentStore::default();
    let cache = super::CacheLedger::default();
    let session = super::WorkspaceSession::new(documents, cache.clone());
    let uri = Url::parse("file:///tmp/references.c").expect("uri");

    cache.mark_reference_search_cache_for_test("root", "needle", 1);
    assert_eq!(cache.reference_search_cache_len_for_test(), 1);
    session
        .change_document(uri, 2, "int needle;\n".to_string())
        .await;
    assert_eq!(cache.reference_search_cache_len_for_test(), 0);

    cache.mark_reference_search_cache_for_test("root", "needle", 2);
    assert_eq!(cache.reference_search_cache_len_for_test(), 1);
    cache.invalidate_after_index_change();
    assert_eq!(cache.reference_search_cache_len_for_test(), 0);
}

#[tokio::test]
async fn reach_scope_uses_captured_workspace_snapshot_graph() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    let uri = Url::from_file_path(root.join("main.c")).expect("file uri");
    let captured_graph = Arc::new(StdRwLock::new(crate::reachability::ReachGraph::new(
        vec![("main.c".to_string(), "captured.h".to_string())],
        vec![],
        vec![],
    )));
    let snapshot = super::WorkspaceSnapshot {
        root: root.clone(),
        generation: super::state::WorkspaceGeneration::missing(),
        settings: super::WorkspaceSnapshotSettings {
            scoping_enabled: true,
            ..Default::default()
        },
        name_table: None,
        reach_graph: Some(captured_graph),
        include_table: None,
        project_context: None,
        indexed_files: None,
    };

    service
        .inner()
        .session
        .cache
        .reach_graphs
        .lock()
        .await
        .insert(
            root,
            Arc::new(StdRwLock::new(crate::reachability::ReachGraph::new(
                vec![("main.c".to_string(), "ledger.h".to_string())],
                vec![],
                vec![],
            ))),
        );

    let (_rel, scope) = service
        .inner()
        .reach_scope_from_snapshot(&uri, &snapshot)
        .expect("scope from captured snapshot");

    assert!(scope.files.contains("captured.h"));
    assert!(
        !scope.files.contains("ledger.h"),
        "request scope must come from the already captured snapshot"
    );
}

#[tokio::test]
async fn failed_include_table_rebuild_clears_stale_cache() {
    let root = tempdir().expect("root");
    let root_path = root.path().to_path_buf();
    let include_tables: super::IncludeTables = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    include_tables.lock().await.insert(
        root_path.clone(),
        Arc::new(IncludeCompletionTable::build(vec!["stale.h".to_string()])),
    );

    let result = rebuild_include_table(&include_tables, root_path.clone()).await;

    assert!(result.is_err(), "missing index should fail the rebuild");
    assert!(
        !include_tables.lock().await.contains_key(&root_path),
        "degraded include table must not keep stale candidates"
    );
}

#[tokio::test]
async fn include_table_rebuild_carries_include_edges_for_ranking() {
    let root = tempdir().expect("root");
    let root_path = root.path().to_path_buf();
    std::fs::write(root.path().join("a.c"), "#include \"b.h\"\n").expect("a");
    std::fs::write(root.path().join("b.h"), "int b;\n").expect("b");
    crate::indexer::index_workspace(
        root.path(),
        crate::indexer::IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let include_tables: super::IncludeTables = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let count = rebuild_include_table(&include_tables, root_path.clone())
        .await
        .expect("rebuild include table");
    let table = include_tables
        .lock()
        .await
        .get(&root_path)
        .cloned()
        .expect("table");

    assert_eq!(count, 2);
    assert_eq!(table.edge_count(), 1);
}

#[tokio::test]
async fn failed_reference_file_list_rebuild_clears_stale_cache() {
    let root = tempdir().expect("root");
    let root_path = root.path().to_path_buf();
    let indexed_file_lists: super::IndexedFileLists =
        Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    indexed_file_lists.lock().await.insert(
        root_path.clone(),
        Arc::new(vec![("stale.c".to_string(), root_path.join("stale.c"))]),
    );

    let result = rebuild_indexed_file_list(&indexed_file_lists, root_path.clone()).await;

    assert!(result.is_err(), "missing index should fail the rebuild");
    assert!(
        !indexed_file_lists.lock().await.contains_key(&root_path),
        "degraded reference file-list must not keep stale discovery scope"
    );
}
