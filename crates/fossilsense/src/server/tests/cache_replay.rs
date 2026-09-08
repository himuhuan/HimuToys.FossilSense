use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "production declaration cache replay; release and U-Boot database required"]
async fn benchmark_uboot_declaration_cache_replay() {
    require_release_completion_benchmark();
    let db = PathBuf::from(std::env::var_os("FOSSILSENSE_BENCH_DB").expect("benchmark DB"));
    let root = PathBuf::from(std::env::var_os("FOSSILSENSE_BENCH_ROOT").expect("benchmark root"));
    let service = test_backend_service();
    *service.inner().workspace_roots.lock().await = vec![root.clone()];
    let engine = service
        .inner()
        .session
        .cache
        .publish_full_index_from_db_for_test(root.clone(), db.clone())
        .await
        .unwrap();
    let original = engine.declaration_index.as_ref().unwrap();
    let handle = engine.call_read_handle.as_ref().unwrap();
    assert!(original.len() >= 500_000);
    let files = engine.indexed_files.as_ref().unwrap().len();
    assert!(files >= 10_000);
    let store = crate::store::IndexStore::open_readonly(&db).unwrap();
    let mut candidate_ids = Vec::new();
    store
        .declaration_view()
        .visit_name_rows(|row| {
            if candidate_ids.len() < 8192
                && !row.external
                && row.path.ends_with(".c")
                && row.declaration_kind == crate::semantic_model::SemanticDeclarationKind::Function
                && row.role == crate::semantic_model::SemanticDeclarationRole::Definition
            {
                candidate_ids.push(row.id);
            }
            Ok(())
        })
        .unwrap();
    let mut witnesses = Vec::new();
    for batch in candidate_ids.chunks(64) {
        for row in original.payloads_by_ids(handle, batch).unwrap() {
            if witnesses.len() == 64 {
                break;
            }
            if row.fact.linkage != crate::call_model::LinkageDomain::External {
                continue;
            }
            let matches = store
                .declaration_view()
                .by_name_limited(&row.fact.name, 2)
                .unwrap();
            if matches.0.len() != 1 || matches.1 {
                continue;
            }
            if fs::read_to_string(root.join(&row.fact.path)).is_err() {
                continue;
            }
            witnesses.push(row);
        }
        if witnesses.len() == 64 {
            break;
        }
    }
    assert_eq!(
        witnesses.len(),
        64,
        "64 unique external definition witnesses required"
    );
    drop(store);
    let ids: Vec<_> = witnesses.iter().map(|row| row.id).collect();
    let budget_probe = original.with_payload_budget_for_test(8 * 1024 * 1024);
    budget_probe.payloads_by_ids(handle, &ids).unwrap();
    let boundary_budget = budget_probe.payload_cache_stats().bytes;
    drop(budget_probe);
    let caller_uri = Url::from_file_path(root.join("fossilsense_cache_replay.c")).unwrap();
    open_test_document(
        &service,
        caller_uri.clone(),
        1,
        "void replay(void) {}\n".to_string(),
    )
    .await;
    let mut version = 1;
    println!("cache_replay_declarations: {}", original.len());
    println!("cache_replay_files: {files}");
    for (phase, budget) in [
        ("cold", 8 * 1024 * 1024),
        ("warm", 8 * 1024 * 1024),
        ("boundary", boundary_budget),
        ("eviction", 16 * 1024),
    ] {
        for feature in ["hover", "definition"] {
            let index = Arc::new(original.with_payload_budget_for_test(budget));
            if phase != "cold" {
                index.payloads_by_ids(handle, &ids).unwrap();
            }
            let mut snapshot = engine.as_ref().clone();
            snapshot.epoch = service.inner().session.cache.allocate_engine_epoch();
            snapshot.declaration_index = Some(index.clone());
            service
                .inner()
                .session
                .cache
                .publish_engine_snapshot(snapshot)
                .await;
            let before = index.payload_cache_stats();
            let mut elapsed = Vec::with_capacity(64);
            for row in &witnesses {
                if phase == "cold" {
                    drop(index.suspend_payload_cache_for_full_publication());
                }
                let source = format!("void replay(void) {{ {}(); }}\n", row.fact.name);
                let character = source.find(&row.fact.name).unwrap() as u32;
                version += 1;
                service
                    .inner()
                    .session
                    .change_document(caller_uri.clone(), version, source)
                    .await;
                let started = std::time::Instant::now();
                if feature == "hover" {
                    let result = service
                        .inner()
                        .hover(hover_params(caller_uri.clone(), 0, character))
                        .await
                        .unwrap()
                        .expect("typed hover");
                    let text = hover_text(result.contents);
                    assert!(
                        text.contains(&row.fact.name) && text.contains(&row.fact.path),
                        "wrong hover for {}",
                        row.fact.name
                    );
                } else {
                    let result = service
                        .inner()
                        .goto_definition(goto_definition_params(caller_uri.clone(), 0, character))
                        .await
                        .unwrap()
                        .expect("typed definition");
                    let expected = Url::from_file_path(root.join(&row.fact.path)).unwrap();
                    let targets = definition_locations(result);
                    assert_eq!(targets.len(), 1, "unexpected targets for {}", row.fact.name);
                    let target = &targets[0];
                    assert!(
                        target.uri == expected
                            && target.range.start.line == row.fact.name_range.start.line
                            && target.range.start.character == row.fact.name_range.start.character
                            && target.range.end.line == row.fact.name_range.end.line
                            && target.range.end.character == row.fact.name_range.end.character,
                        "wrong definition range for {}",
                        row.fact.name
                    );
                }
                elapsed.push(started.elapsed().as_micros() as u64);
            }
            elapsed.sort_unstable();
            let after = index.payload_cache_stats();
            let prefix = format!("cache_{phase}_{feature}");
            println!("{prefix}_requests: 64");
            println!("{prefix}_correct_targets: 64");
            for percentile in [50, 95, 99] {
                println!(
                    "{prefix}_p{percentile}_us: {}",
                    elapsed[64 * percentile / 100]
                );
            }
            for (name, value) in [
                ("hits", after.hits - before.hits),
                ("misses", after.misses - before.misses),
                ("payload_sql_reads", after.sql_reads - before.sql_reads),
                ("evictions", after.evictions - before.evictions),
                (
                    "admission_skips",
                    after.admission_skips - before.admission_skips,
                ),
                (
                    "eviction_limit_skips",
                    after.eviction_limit_skips - before.eviction_limit_skips,
                ),
                ("victim_steps", after.victim_steps - before.victim_steps),
                ("lock_wait_ns", after.lock_wait_ns - before.lock_wait_ns),
            ] {
                println!("{prefix}_{name}: {value}");
            }
            println!("{prefix}_bytes: {}", after.bytes);
            println!("{prefix}_budget_bytes: {budget}");
            assert!(after.bytes <= budget);
            if phase == "cold" {
                assert!(
                    after.sql_reads > before.sql_reads,
                    "cold replay must hydrate persistent rows"
                );
            }
            if phase == "warm" {
                assert!(
                    after.hits > before.hits,
                    "warm replay must use the payload cache"
                );
            }
            if phase == "eviction" {
                assert!(
                    after.evictions > before.evictions,
                    "eviction replay must exercise real cache turnover"
                );
            }
        }
    }
    service.inner().session.close_document(&caller_uri).await;
}
