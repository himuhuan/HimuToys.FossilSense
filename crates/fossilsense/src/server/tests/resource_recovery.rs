use super::*;
use crate::build_coordinator::{BuildCoordinator, BuildPolicy};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_changes_are_recovered_when_another_file_is_saved() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    write_workspace_file(&root, "a.c", "int before_a;\n");
    write_workspace_file(&root, "b.c", "int before_b;\n");
    write_workspace_file(&root, "deleted.c", "int deleted_name;\n");
    crate::indexer::index_workspace(&root, Default::default(), |_| {}).unwrap();
    let memory = Arc::new(AtomicU64::new(0));
    let sampled = memory.clone();
    let mut cache = super::super::CacheLedger::default();
    cache.build_coordinator = BuildCoordinator::with_policy_and_sampler(
        BuildPolicy {
            wait_timeout: std::time::Duration::from_millis(1),
            ..Default::default()
        },
        move || sampled.load(std::sync::atomic::Ordering::SeqCst),
    );
    let service = test_backend_service_with_cache(cache);
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root.clone());
    service
        .inner()
        .session
        .cache
        .publish_full_index(&service.inner().client, root.clone())
        .await
        .unwrap();
    let before = service
        .inner()
        .session
        .cache
        .current_engine_snapshot(&root)
        .await
        .unwrap();
    write_workspace_file(&root, "a.c", "int after_a;\n");
    fs::remove_file(root.join("deleted.c")).unwrap();
    let change = |name: &str, kind| super::super::RootDirtyChange {
        root: root.clone(),
        rel_path: name.into(),
        change: crate::indexer::DirtyFileChange {
            absolute_path: root.join(name),
            kind,
        },
    };
    memory.store(2 * 1024 * 1024 * 1024, std::sync::atomic::Ordering::SeqCst);
    service
        .inner()
        .run_dirty_index_for_test(vec![
            change("a.c", crate::indexer::DirtyFileKind::Upsert),
            change("deleted.c", crate::indexer::DirtyFileKind::Delete),
        ])
        .await;
    let rejected = service
        .inner()
        .session
        .cache
        .current_engine_snapshot(&root)
        .await
        .unwrap();
    assert_eq!(
        before.epoch, rejected.epoch,
        "resource failure must preserve the old generation"
    );
    let coordinator = service.inner().session.cache.build_coordinator.snapshot();
    assert_eq!(coordinator.active_builds, 0);
    assert_eq!(coordinator.active_reserved_bytes, 0);
    memory.store(0, std::sync::atomic::Ordering::SeqCst);
    write_workspace_file(&root, "b.c", "int after_b;\n");
    service
        .inner()
        .run_dirty_index_for_test(vec![change("b.c", crate::indexer::DirtyFileKind::Upsert)])
        .await;
    let recovered = service
        .inner()
        .session
        .cache
        .current_engine_snapshot(&root)
        .await
        .unwrap();
    assert!(!service
        .inner()
        .session
        .cache
        .roots_needing_rescan
        .lock()
        .await
        .contains(&root));
    let handle = recovered.call_read_handle.as_ref().unwrap();
    for name in ["before_a", "before_b", "deleted_name"] {
        assert!(
            handle
                .read(|store| store.declaration_view().by_name_limited(name, 4))
                .unwrap()
                .0
                .is_empty(),
            "stale declaration survived recovery: {name}"
        );
    }
    for name in ["after_a", "after_b"] {
        assert_eq!(
            handle
                .read(|store| store.declaration_view().by_name_limited(name, 4))
                .unwrap()
                .0
                .len(),
            1,
            "recovery missed {name}"
        );
    }
}
