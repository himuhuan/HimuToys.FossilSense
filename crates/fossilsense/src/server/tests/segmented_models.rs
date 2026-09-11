use super::*;
use crate::server::{workspace::EngineSnapshot, IncludeCompletionTable, IncludeForm};

#[tokio::test]
async fn segmented_dirty_body_edit_reuses_unchanged_auxiliary_models() {
    let mut work_samples = Vec::new();
    for count in [10, 1000] {
        let root = tempdir().unwrap();
        for index in 0..count {
            write_workspace_file(
                root.path(),
                &format!("include/d{index}/item.h"),
                "struct Item;\n",
            );
        }
        write_workspace_file(root.path(), "src/main.c", "int value = 1;\n");
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
        let ledger = &service.inner().session.cache;
        ledger
            .publish_full_index(&service.inner().client, root.path().to_path_buf())
            .await
            .unwrap();
        let before = service
            .inner()
            .session
            .request_context_for_root(root.path().to_path_buf())
            .await
            .engine;
        write_workspace_file(root.path(), "src/main.c", "int value = 2;\n");
        crate::indexer::index_dirty_files(
            root.path(),
            vec![crate::indexer::DirtyFileChange {
                absolute_path: root.path().join("src/main.c"),
                kind: crate::indexer::DirtyFileKind::Upsert,
            }],
            crate::indexer::IndexOptions::default(),
            |_| {},
        )
        .unwrap();
        let report = ledger
            .publish_dirty_index(
                &service.inner().client,
                root.path().to_path_buf(),
                &["src/main.c".into()],
                &[],
            )
            .await
            .unwrap();
        assert!(report.auxiliary.full_reason.is_none());
        assert!(report.auxiliary.full_components.is_empty());
        assert_eq!(report.auxiliary.shared_components, 5);
        assert_eq!(report.auxiliary.copied_directory_entries, 0);
        work_samples.push(report.auxiliary.scoped_rows_read);
        let after = service
            .inner()
            .session
            .request_context_for_root(root.path().to_path_buf())
            .await
            .engine;
        assert_ne!(before.epoch, after.epoch);
        let shared = [
            (
                "fallback",
                Arc::ptr_eq(
                    &before.fallback_completion_table,
                    &after.fallback_completion_table,
                ),
            ),
            (
                "include",
                Arc::ptr_eq(
                    before.include_table.as_ref().unwrap(),
                    after.include_table.as_ref().unwrap(),
                ),
            ),
            (
                "go import",
                Arc::ptr_eq(
                    before.go_import_table.as_ref().unwrap(),
                    after.go_import_table.as_ref().unwrap(),
                ),
            ),
            (
                "include paths",
                Arc::ptr_eq(
                    before.include_path_index.as_ref().unwrap(),
                    after.include_path_index.as_ref().unwrap(),
                ),
            ),
            (
                "indexed files",
                Arc::ptr_eq(
                    before.indexed_files.as_ref().unwrap(),
                    after.indexed_files.as_ref().unwrap(),
                ),
            ),
        ];
        assert!(
            shared.iter().all(|(_, reused)| *reused),
            "{count} files: unchanged models were copied: {shared:?}"
        );
        assert_eq!(before.indexed_files.as_ref().unwrap().len(), count + 1);
        assert_eq!(after.indexed_files.as_ref().unwrap().len(), count + 1);
    }
    assert_eq!(
        work_samples[0], work_samples[1],
        "same edit must read the same auxiliary rows regardless of workspace size"
    );
}

#[tokio::test]
async fn segmented_header_rename_shares_include_path_base_and_retains_old_view() {
    let root = tempdir().unwrap();
    write_workspace_file(root.path(), "old.h", "struct Before;\n");
    write_workspace_file(root.path(), "keep.h", "struct Kept;\n");
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
    let ledger = &service.inner().session.cache;
    ledger
        .publish_full_index(&service.inner().client, root.path().to_path_buf())
        .await
        .unwrap();
    let before = service
        .inner()
        .session
        .request_context_for_root(root.path().to_path_buf())
        .await
        .engine;
    fs::rename(root.path().join("old.h"), root.path().join("renamed.h")).unwrap();
    crate::indexer::index_dirty_files(
        root.path(),
        vec![
            crate::indexer::DirtyFileChange {
                absolute_path: root.path().join("old.h"),
                kind: crate::indexer::DirtyFileKind::Delete,
            },
            crate::indexer::DirtyFileChange {
                absolute_path: root.path().join("renamed.h"),
                kind: crate::indexer::DirtyFileKind::Upsert,
            },
        ],
        crate::indexer::IndexOptions::default(),
        |_| {},
    )
    .unwrap();
    ledger
        .publish_dirty_index(
            &service.inner().client,
            root.path().to_path_buf(),
            &["old.h".into(), "renamed.h".into()],
            &["old.h".into(), "renamed.h".into()],
        )
        .await
        .unwrap();
    let after = service
        .inner()
        .session
        .request_context_for_root(root.path().to_path_buf())
        .await
        .engine;
    assert!(
        before
            .include_path_index
            .as_ref()
            .unwrap()
            .shares_base_for_test(after.include_path_index.as_ref().unwrap()),
        "renaming one header copied the complete include path index"
    );
    assert!(
        before
            .include_table
            .as_ref()
            .unwrap()
            .shares_base_for_test(after.include_table.as_ref().unwrap()),
        "renaming one header copied the complete include completion table"
    );
    assert!(before
        .indexed_files
        .as_ref()
        .unwrap()
        .contains_path("old.h"));
    assert!(!before
        .indexed_files
        .as_ref()
        .unwrap()
        .contains_path("renamed.h"));
    assert!(!after.indexed_files.as_ref().unwrap().contains_path("old.h"));
    assert!(after
        .indexed_files
        .as_ref()
        .unwrap()
        .contains_path("renamed.h"));
}

#[tokio::test]
async fn segmented_go_body_edit_reuses_nonempty_import_catalogue() {
    let root = tempdir().unwrap();
    write_workspace_file(
        root.path(),
        "go.mod",
        "module example.test/repo\n\ngo 1.24\n",
    );
    write_workspace_file(root.path(), "lib/a.go", "package lib\nfunc A() {}\n");
    write_workspace_file(root.path(), "keep/a.go", "package keep\nfunc Keep() {}\n");
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
    let ledger = &service.inner().session.cache;
    ledger
        .publish_full_index(&service.inner().client, root.path().to_path_buf())
        .await
        .unwrap();
    let before = service
        .inner()
        .session
        .request_context_for_root(root.path().to_path_buf())
        .await
        .engine;
    write_workspace_file(root.path(), "lib/a.go", "package lib\nfunc B() {}\n");
    crate::indexer::index_dirty_files(
        root.path(),
        vec![crate::indexer::DirtyFileChange {
            absolute_path: root.path().join("lib/a.go"),
            kind: crate::indexer::DirtyFileKind::Upsert,
        }],
        crate::indexer::IndexOptions::default(),
        |_| {},
    )
    .unwrap();
    ledger
        .publish_dirty_index(
            &service.inner().client,
            root.path().to_path_buf(),
            &["lib/a.go".into()],
            &["lib/a.go".into()],
        )
        .await
        .unwrap();
    let edited = service
        .inner()
        .session
        .request_context_for_root(root.path().to_path_buf())
        .await
        .engine;
    assert!(
        Arc::ptr_eq(
            before.go_import_table.as_ref().unwrap(),
            edited.go_import_table.as_ref().unwrap()
        ),
        "Go body edit rebuilt an unchanged import catalogue"
    );
}

#[tokio::test]
async fn segmented_go_mixed_case_directory_preserves_catalogue() {
    let root = tempdir().unwrap();
    write_workspace_file(
        root.path(),
        "go.mod",
        "module example.test/repo\n\ngo 1.24\n",
    );
    write_workspace_file(root.path(), "Lib/a.go", "package lib\nfunc A() {}\n");
    write_workspace_file(root.path(), "keep/a.go", "package keep\nfunc Keep() {}\n");
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
    let ledger = &service.inner().session.cache;
    ledger
        .publish_full_index(&service.inner().client, root.path().to_path_buf())
        .await
        .unwrap();
    let before = service
        .inner()
        .session
        .request_context_for_root(root.path().to_path_buf())
        .await
        .engine;
    write_workspace_file(root.path(), "Lib/a.go", "package lib\nfunc B() {}\n");
    crate::indexer::index_dirty_files(
        root.path(),
        vec![crate::indexer::DirtyFileChange {
            absolute_path: root.path().join("Lib/a.go"),
            kind: crate::indexer::DirtyFileKind::Upsert,
        }],
        crate::indexer::IndexOptions::default(),
        |_| {},
    )
    .unwrap();
    ledger
        .publish_dirty_index(
            &service.inner().client,
            root.path().to_path_buf(),
            &["Lib/a.go".into()],
            &["Lib/a.go".into()],
        )
        .await
        .unwrap();
    let edited = service
        .inner()
        .session
        .request_context_for_root(root.path().to_path_buf())
        .await
        .engine;
    assert!(
        Arc::ptr_eq(
            before.go_import_table.as_ref().unwrap(),
            edited.go_import_table.as_ref().unwrap()
        ),
        "Go body edit rebuilt an unchanged import catalogue"
    );
}

#[tokio::test]
async fn segmented_combined_delta_budget_rebuilds_instead_of_partial_publication() {
    let root = tempdir().unwrap();
    write_workspace_file(root.path(), "keep.c", "int keep;\n");
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
    let ledger = &service.inner().session.cache;
    ledger
        .publish_full_index(&service.inner().client, root.path().to_path_buf())
        .await
        .unwrap();
    let mut paths = Vec::new();
    let mut changes = Vec::new();
    for n in 0..64 {
        let path = format!("files/n{n}.c");
        write_workspace_file(root.path(), &path, &format!("int item{n};\n"));
        changes.push(crate::indexer::DirtyFileChange {
            absolute_path: root.path().join(&path),
            kind: crate::indexer::DirtyFileKind::Upsert,
        });
        paths.push(path);
    }
    crate::indexer::index_dirty_files(
        root.path(),
        changes,
        crate::indexer::IndexOptions::default(),
        |_| {},
    )
    .unwrap();
    ledger
        .publish_dirty_index(
            &service.inner().client,
            root.path().to_path_buf(),
            &paths,
            &[],
        )
        .await
        .unwrap();
    let after = service
        .inner()
        .session
        .request_context_for_root(root.path().to_path_buf())
        .await
        .engine;
    assert_eq!(after.indexed_files.as_ref().unwrap().len(), 65);
    assert_eq!(
        after
            .name_table
            .as_ref()
            .unwrap()
            .memory_breakdown()
            .delta_segment_count,
        0,
        "combined auxiliary delta exceeded 256 partitions without a full rebuild"
    );
}

#[tokio::test]
async fn segmented_include_lsp_reports_inspection_truncation() {
    let root = tempdir().unwrap();
    let service = test_backend_service();
    *service.inner().workspace_roots.lock().await = vec![root.path().to_path_buf()];
    let mut paths: Vec<String> = (0..16_384).map(|n| format!("d{n}/shared.h")).collect();
    paths.push("last/shared_extra.h".into());
    let mut engine = EngineSnapshot::empty(root.path().to_path_buf());
    engine.include_table = Some(Arc::new(IncludeCompletionTable::build(paths)));
    service
        .inner()
        .session
        .cache
        .publish_engine_snapshot(engine)
        .await
        .expect("segmented model snapshot identity");
    let uri = Url::from_file_path(root.path().join("main.c")).unwrap();
    let response = service
        .inner()
        .complete_include(&uri, IncludeForm::Quote, "shared".into(), "")
        .await
        .unwrap()
        .unwrap();
    let CompletionResponse::List(list) = response else {
        panic!("inspection limit must produce an incomplete LSP list");
    };
    assert!(list.is_incomplete);
    assert!(!list.items.is_empty());
}

#[tokio::test]
async fn segmented_database_incarnation_change_forces_full_rebuild() {
    let root = tempdir().unwrap();
    write_workspace_file(root.path(), "main.c", "int original;\n");
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
    let ledger = &service.inner().session.cache;
    ledger
        .publish_full_index(&service.inner().client, root.path().to_path_buf())
        .await
        .unwrap();
    let before = service
        .inner()
        .session
        .request_context_for_root(root.path().to_path_buf())
        .await
        .engine;
    write_workspace_file(root.path(), "main.c", "int replacement;\n");
    crate::indexer::index_dirty_files(
        root.path(),
        vec![crate::indexer::DirtyFileChange {
            absolute_path: root.path().join("main.c"),
            kind: crate::indexer::DirtyFileKind::Upsert,
        }],
        crate::indexer::IndexOptions::default(),
        |_| {},
    )
    .unwrap();
    let store = crate::store::IndexStore::open(
        &crate::pathing::default_index_path(root.path()).unwrap(),
        root.path(),
    )
    .unwrap();
    store
        .entity_view()
        .replace_incarnation_for_test("different-database-instance")
        .unwrap();
    drop(store);
    ledger
        .publish_dirty_index(
            &service.inner().client,
            root.path().to_path_buf(),
            &["main.c".into()],
            &[],
        )
        .await
        .unwrap();
    let after = service
        .inner()
        .session
        .request_context_for_root(root.path().to_path_buf())
        .await
        .engine;
    assert!(
        !Arc::ptr_eq(
            before.indexed_files.as_ref().unwrap(),
            after.indexed_files.as_ref().unwrap()
        ),
        "different database instance reused the old auxiliary base"
    );
}

#[tokio::test]
async fn segmented_published_fallback_survives_other_document_completion() {
    let (dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[("broken.c", "((( guess_old(value);\n")],
        "main.c",
        "void f(void) { guess_/*cursor*/; }\n",
    )
    .await;
    write_workspace_file(dir.path(), "broken.c", "((( guess_new(value);\n");
    crate::indexer::index_dirty_files(
        dir.path(),
        vec![crate::indexer::DirtyFileChange {
            absolute_path: dir.path().join("broken.c"),
            kind: crate::indexer::DirtyFileKind::Upsert,
        }],
        crate::indexer::IndexOptions::default(),
        |_| {},
    )
    .unwrap();
    service
        .inner()
        .session
        .cache
        .publish_dirty_index(
            &service.inner().client,
            dir.path().to_path_buf(),
            &["broken.c".into()],
            &[],
        )
        .await
        .unwrap();
    let items = completion_items(
        service
            .inner()
            .completion(completion_params(uri, line, character))
            .await
            .unwrap()
            .unwrap(),
    );
    assert!(
        items.iter().any(|item| item.label == "guess_new"),
        "published fallback disappeared in another document"
    );
    assert!(
        !items.iter().any(|item| item.label == "guess_old"),
        "deleted fallback resurrected"
    );
}

#[tokio::test]
async fn segmented_configuration_module_and_cancelled_publications_keep_boundaries() {
    use crate::build_coordinator::{BuildCancellation, BuildKind};
    let root = tempdir().unwrap();
    write_workspace_file(root.path(), "main.c", "int before;\n");
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
    let ledger = &service.inner().session.cache;
    ledger
        .publish_full_index(&service.inner().client, root.path().to_path_buf())
        .await
        .unwrap();
    for changed_configuration in [true, false] {
        let before = service
            .inner()
            .session
            .request_context_for_root(root.path().to_path_buf())
            .await
            .engine;
        crate::indexer::index_dirty_files(
            root.path(),
            vec![crate::indexer::DirtyFileChange {
                absolute_path: root.path().join("main.c"),
                kind: crate::indexer::DirtyFileKind::Upsert,
            }],
            crate::indexer::IndexOptions::default(),
            |_| {},
        )
        .unwrap();
        let cancellation = BuildCancellation::new();
        let permit = ledger
            .build_coordinator
            .acquire(
                root.path().to_path_buf(),
                BuildKind::ReadModel,
                cancellation.clone(),
            )
            .await
            .unwrap();
        cancellation.cancel();
        assert!(ledger
            .publish_dirty_index_with_semantics_in_lifecycle(
                &service.inner().client,
                root.path().to_path_buf(),
                &["main.c".into()],
                &[],
                before.workspace_semantics.clone(),
                &permit
            )
            .await
            .is_err());
        assert!(Arc::ptr_eq(
            &before,
            &service
                .inner()
                .session
                .request_context_for_root(root.path().to_path_buf())
                .await
                .engine
        ));
        drop(permit);
        let semantics = if changed_configuration {
            Arc::new((*before.workspace_semantics).clone())
        } else {
            before.workspace_semantics.clone()
        };
        let paths = vec![if changed_configuration {
            "main.c".into()
        } else {
            "go.mod".into()
        }];
        let report = ledger
            .publish_dirty_index_with_semantics(
                &service.inner().client,
                root.path().to_path_buf(),
                &paths,
                &[],
                semantics,
            )
            .await
            .unwrap();
        assert_eq!(
            report.auxiliary.full_reason,
            Some(if changed_configuration {
                "configuration"
            } else {
                "module-boundary"
            })
        );
        let after = service
            .inner()
            .session
            .request_context_for_root(root.path().to_path_buf())
            .await
            .engine;
        assert!(!Arc::ptr_eq(
            before.indexed_files.as_ref().unwrap(),
            after.indexed_files.as_ref().unwrap()
        ));
        assert!(before
            .indexed_files
            .as_ref()
            .unwrap()
            .contains_path("main.c"));
    }
}
