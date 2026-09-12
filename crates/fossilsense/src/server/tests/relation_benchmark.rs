use super::*;
use crate::candidate_service::{relation_facts::*, relation_types::*};
use crate::semantic_model::{EntityIdentity, SemanticFamily};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "release-only relation fact pressure and real completion replay"]
async fn benchmark_relation_foundation() {
    require_release_completion_benchmark();
    let output = PathBuf::from(
        std::env::var_os("FOSSILSENSE_RELATION_BENCH_ROOT").expect("benchmark evidence directory"),
    );
    let root = output.join("workspace");
    fs::create_dir(&root).unwrap();
    let done = Arc::new(AtomicBool::new(false));
    let peak = Arc::new(AtomicU64::new(0));
    let sample_done = done.clone();
    let sample_peak = peak.clone();
    let sampler = std::thread::spawn(move || {
        while !sample_done.load(std::sync::atomic::Ordering::Relaxed) {
            sample_peak.fetch_max(
                crate::resource::current_process_memory_bytes(),
                std::sync::atomic::Ordering::Relaxed,
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    });
    let mut input_bytes = 0u64;
    for file in 0..500 {
        let mut source = String::new();
        for n in 0..1000 {
            source.push_str(&format!("int bench_value_{file:03}_{n:04};\n"));
        }
        source.push_str(&format!("void touch_{file}(void) {{\n"));
        for n in 0..1000 {
            source.push_str(&format!("bench_value_{file:03}_{n:04}++;\n"));
        }
        source.push_str("}\n");
        input_bytes += source.len() as u64;
        fs::write(root.join(format!("part_{file:03}.c")), source).unwrap();
    }
    let witness="struct A {int run(){return 1;}}; struct B {int run(){return 2;}};\nint tx(){return 3;}\nstruct Ops{int (*send)();}; static Ops ops={.send=tx};\nint caller(A a){int (*fp)()=&tx; fp(); ops.send(); return a.run();}\n";
    fs::write(root.join("relations.cpp"), witness).unwrap();
    input_bytes += witness.len() as u64;
    let db = output.join("index.sqlite");
    let stats = crate::indexer::index_workspace(
        &root,
        crate::indexer::IndexOptions {
            db_path: Some(db.clone()),
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .unwrap();
    assert!(stats.declarations >= 500_000);
    let service = test_backend_service();
    *service.inner().workspace_roots.lock().await = vec![root.clone()];
    service
        .inner()
        .set_completion_history_mode_for_test(crate::completion_history::CompletionHistoryMode::Off)
        .await;
    let engine = service
        .inner()
        .session
        .cache
        .publish_full_index_from_db_for_test(root.clone(), db.clone())
        .await
        .unwrap();
    let read = Arc::new(
        crate::declaration_read::DeclarationReadContext::from_handle(
            engine.call_read_handle.as_ref().unwrap().clone(),
        ),
    );
    let relations = RelationQueryContext {
        read: Some(read.clone()),
        overlays: Arc::new(crate::candidate_service::CandidateOverlaySnapshot::default()),
        reach: None,
        family: SemanticFamily::CFamily,
    };
    let (_, _, calls) = crate::call_service::CallRelationService::new(&read)
        .query_at(
            "relations.cpp",
            crate::call_model::SourcePosition {
                line: 3,
                character: 5,
            },
            crate::call_model::RelationDirection::Outgoing,
            0,
            100,
            100,
        )
        .unwrap();
    assert!(calls.relations.iter().any(|r| r
        .callee
        .as_ref()
        .is_some_and(|c| c.qualified_name == "A::run")));
    assert!(calls
        .relations
        .iter()
        .any(|r| r.callee.as_ref().is_some_and(|c| c.name == "tx")));
    assert!(!calls.relations.iter().any(|r| r
        .callee
        .as_ref()
        .is_some_and(|c| c.qualified_name == "B::run")));
    let set = relations
        .service("part_000.c")
        .semantic_candidates(
            "bench_value_000_0000",
            crate::candidate_service::SemanticIntent::Value,
        )
        .unwrap();
    let selected = crate::candidate_service::focused_candidates(&set)[0];
    let target = relations.target(RelationTarget::Entity(EntityIdentity::from_declaration(
        &selected.fact,
    )));
    let references = relations
        .relation_references(
            &target,
            true,
            &RelationCursor::default(),
            &RelationControl::default(),
        )
        .unwrap();
    assert_eq!(references.references.len(), 1);
    let counts = read
        .read(|s| s.relation_fact_view().benchmark_counts())
        .unwrap();
    assert!(counts.0 >= 500_000);
    let declaration_index = engine.declaration_index.as_ref().unwrap();
    let before_sql = declaration_index.payload_cache_stats().sql_reads;
    let uri = Url::from_file_path(root.join("completion.c")).unwrap();
    let (text, line, character) = text_and_position("void completion(){bench_v/*cursor*/;}\n");
    open_test_document(&service, uri.clone(), 1, text).await;
    let mut latencies = Vec::new();
    for _ in 0..64 {
        let start = std::time::Instant::now();
        let response = service
            .inner()
            .completion(completion_params(uri.clone(), line, character))
            .await
            .unwrap()
            .unwrap();
        latencies.push(start.elapsed().as_micros() as u64);
        assert!(completion_response_is_incomplete(&response));
        assert!(!completion_items(response).is_empty());
    }
    let observations = service.inner().take_completion_perf_for_test();
    assert_eq!(observations.len(), 64);
    let recall: Vec<_> = observations
        .iter()
        .map(|(_, m)| m.recall_channels)
        .collect();
    validate_completion_replay_recall(&recall, 500_000).unwrap();
    assert_eq!(
        declaration_index.payload_cache_stats().sql_reads,
        before_sql
    );
    latencies.sort_unstable();
    let p95 = latencies[64 * 95 / 100];
    let mut stable = Vec::new();
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let bytes = crate::resource::current_process_memory_bytes();
        assert!(bytes > 0);
        stable.push(bytes);
    }
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    sampler.join().unwrap();
    let report = serde_json::json!({"scenario":"relation-semantic-foundation","synthetic":true,"files":501,"declarations":stats.declarations,"input_bytes":input_bytes,
        "index_elapsed_ms":stats.elapsed_ms,"write_ms":stats.write_ms,"database_bytes":fs::metadata(&db).unwrap().len(),"relation_fact_rows":counts.0,"relation_payload_bytes":counts.1,
        "stable_samples":stable.len(),"stable_window_ms":10000,"stable_max_bytes":stable.iter().max(),"peak_process_bytes":peak.load(std::sync::atomic::Ordering::Relaxed),
        "completion_requests":64,"completion_p95_us":p95,"completion_detail_sql":declaration_index.payload_cache_stats().sql_reads-before_sql,
        "completion_observations":recall.iter().map(|m|serde_json::json!({"active_entries":m.active_entries_total,"inspected":m.entries_inspected,"budget":m.candidate_budget,"indexed_returned":m.indexed_returned,"truncated":m.truncated})).collect::<Vec<_>>(),
        "stable_samples_bytes":stable,"verified_call_relations":calls.relations.len(),"verified_references":references.references.len()});
    fs::write(
        output.join("metrics.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    println!("relation_benchmark: {report}");
    assert!(p95 <= 50_000, "completion P95 regression: {p95} us");
}
