use super::*;

#[tokio::test]
async fn relation_foundation_unsaved_standard_call_hierarchy_retains_member_facts() {
    let root = tempdir().unwrap();
    let source="struct A{int run(){return 1;}};\nstruct B{int run(){return 2;}};\nint caller(A a){return a.run();}\n";
    write_workspace_file(root.path(), "main.cpp", source);
    crate::indexer::index_workspace(
        root.path(),
        crate::indexer::IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .unwrap();
    let service = test_backend_service();
    *service.inner().workspace_roots.lock().await = vec![root.path().to_path_buf()];
    service
        .inner()
        .session
        .cache
        .publish_full_index(&service.inner().client, root.path().to_path_buf())
        .await
        .unwrap();
    let uri = Url::from_file_path(root.path().join("main.cpp")).unwrap();
    let dirty = source.replace(
        "caller(A a){return a.run();}",
        "caller(B b){return b.run();}",
    );
    open_test_document(&service, uri.clone(), 2, dirty).await;
    let items = service
        .inner()
        .prepare_call_items(
            &uri,
            Position {
                line: 2,
                character: 5,
            },
        )
        .await
        .unwrap();
    let outgoing = service.inner().standard_outgoing(&items[0]).await.unwrap();
    assert_eq!(outgoing.len(), 1);
    assert_eq!(
        outgoing[0].to.selection_range.start.line, 1,
        "dirty receiver must select B.run"
    );
}
