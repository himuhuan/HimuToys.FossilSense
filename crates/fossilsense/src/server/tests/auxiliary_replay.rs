use super::*;

/// Run directly from the release test executable. No compiler work is timed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "release-only real include/import completion replay"]
async fn benchmark_auxiliary_completion_replay() {
    require_release_completion_benchmark();
    let root = tempdir().unwrap();
    write_workspace_file(
        root.path(),
        "go.mod",
        "module example.test/replay\n\ngo 1.24\n",
    );
    for n in 0..1000 {
        write_workspace_file(
            root.path(),
            &format!("headers/d{n}/bench_{n:04}.h"),
            "struct Header;\n",
        );
    }
    for n in 0..128 {
        write_workspace_file(
            root.path(),
            &format!("lib{n:03}/lib.go"),
            "package lib\nfunc Exported() {}\n",
        );
    }
    write_workspace_file(root.path(), "caller.c", "int caller;\n");
    write_workspace_file(root.path(), "caller.go", "package main\nfunc main() {}\n");
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
    let include_uri = Url::from_file_path(root.path().join("caller.c")).unwrap();
    let go_uri = Url::from_file_path(root.path().join("caller.go")).unwrap();
    let (include_text, include_line, include_character) =
        text_and_position("#include \"bench_/*cursor*/\"\n");
    let (go_text, go_line, go_character) =
        text_and_position("package main\nimport \"example.test/replay/lib/*cursor*/\"\n");
    open_test_document(&service, include_uri.clone(), 1, include_text).await;
    open_test_document(&service, go_uri.clone(), 1, go_text).await;
    let mut observations = Vec::new();
    for phase in ["first-pass", "warm", "after-rename"] {
        if phase == "after-rename" {
            fs::rename(
                root.path().join("headers/d0/bench_0000.h"),
                root.path().join("headers/d0/bench_moved.h"),
            )
            .unwrap();
            let paths = ["headers/d0/bench_0000.h", "headers/d0/bench_moved.h"];
            crate::indexer::index_dirty_files(
                root.path(),
                paths
                    .iter()
                    .enumerate()
                    .map(|(i, path)| crate::indexer::DirtyFileChange {
                        absolute_path: root.path().join(path),
                        kind: if i == 0 {
                            crate::indexer::DirtyFileKind::Delete
                        } else {
                            crate::indexer::DirtyFileKind::Upsert
                        },
                    })
                    .collect(),
                crate::indexer::IndexOptions::default(),
                |_| {},
            )
            .unwrap();
            let report = service
                .inner()
                .session
                .cache
                .publish_dirty_index(
                    &service.inner().client,
                    root.path().to_path_buf(),
                    &paths.map(str::to_owned),
                    &[],
                )
                .await
                .unwrap();
            assert!(report.auxiliary.full_reason.is_none());
            assert!(report.auxiliary.full_components.is_empty());
            println!(
                "auxiliary_update: {}",
                serde_json::to_string(&report.auxiliary).unwrap()
            );
        }
        for (feature, uri, line, character, prefix) in [
            (
                "include",
                &include_uri,
                include_line,
                include_character,
                "bench_",
            ),
            (
                "go-import",
                &go_uri,
                go_line,
                go_character,
                "example.test/replay/lib",
            ),
        ] {
            let mut micros = Vec::new();
            let mut minimum = usize::MAX;
            for _ in 0..64 {
                let started = std::time::Instant::now();
                let response = service
                    .inner()
                    .completion(completion_params(uri.clone(), line, character))
                    .await
                    .unwrap()
                    .unwrap();
                micros.push(started.elapsed().as_micros() as u64);
                let items = completion_items(response);
                assert!(!items.is_empty());
                assert!(items.iter().all(|item| item.label.starts_with(prefix)));
                assert!(!items
                    .iter()
                    .any(|item| phase == "after-rename" && item.label == "bench_0000.h"));
                minimum = minimum.min(items.len());
            }
            micros.sort_unstable();
            let p95_us = micros[60];
            assert!(
                p95_us <= 50_000,
                "{feature}/{phase} P95 {p95_us}us exceeds 50ms"
            );
            observations.push(serde_json::json!({"phase":phase,"feature":feature,"requests":64,"p95_us":p95_us,"max_us":micros[63],"returned_min":minimum}));
        }
    }
    let engine = service
        .inner()
        .session
        .request_context_for_root(root.path().to_path_buf())
        .await
        .engine;
    println!(
        "auxiliary_replay: {}",
        serde_json::json!({"headers":1000,"go_packages":129,"requests":384,"observations":observations,
        "include_bytes":engine.include_table.as_ref().unwrap().accounted_bytes(),
        "include_path_bytes":engine.include_path_index.as_ref().unwrap().accounted_bytes(),
        "go_import_bytes":engine.go_import_table.as_ref().unwrap().accounted_bytes()})
    );
}
