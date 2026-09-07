use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real hover/F12 replay; requires release and U-Boot benchmark database"]
async fn benchmark_uboot_binding_replay() {
    require_release_completion_benchmark();
    replay_binding_requests().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "functional diagnostic only; requires U-Boot database; not performance evidence"]
async fn diagnose_binding_source_coverage() {
    replay_binding_requests().await;
}

async fn replay_binding_requests() {
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
    let index = engine.declaration_index.as_ref().unwrap();
    assert!(index.len() >= 500_000);
    let files = engine.indexed_files.as_ref().unwrap().len();
    assert!(files >= 10_000);
    let store = crate::store::IndexStore::open_readonly(&db).unwrap();
    let mut ids = Vec::new();
    let mut selected_files = HashSet::new();
    store
        .declaration_view()
        .visit_name_rows(|row| {
            if ids.len() < 64
                && !row.external
                && row.path.ends_with(".c")
                && row.declaration_kind == crate::semantic_model::SemanticDeclarationKind::Function
                && row.role == crate::semantic_model::SemanticDeclarationRole::Definition
                && fs::metadata(root.join(row.path))
                    .is_ok_and(|metadata| metadata.len() <= 256 * 1024)
                && selected_files.insert(row.path.to_owned())
            {
                ids.push(row.id);
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(
        ids.len(),
        64,
        "64 bounded real-source witnesses are required"
    );
    let witnesses = index
        .payloads_by_ids(engine.call_read_handle.as_ref().unwrap(), &ids)
        .unwrap();
    assert_eq!(witnesses.len(), 64);
    let mut inputs = Vec::new();
    for row in witnesses {
        let uri = Url::from_file_path(root.join(&row.fact.path)).unwrap();
        let source = fs::read_to_string(root.join(&row.fact.path)).unwrap();
        open_test_document(&service, uri.clone(), 1, source.clone()).await;
        let family = crate::semantic_model::SemanticFamily::CFamily;
        let (at_cursor, truncated) = store
            .call_fact_view()
            .anchors_at_family_limited(
                &row.fact.path,
                row.fact.name_range.start.line,
                row.fact.name_range.start.character,
                family,
                16,
            )
            .unwrap();
        assert!(
            !truncated,
            "fixture source occurrence must be fully checked"
        );
        let origin = at_cursor
            .iter()
            .find(|anchor| anchor.anchor_fingerprint == row.fact.identity.locator.fingerprint)
            .expect("typed callable witness");
        let (related, _) = store
            .call_fact_view()
            .anchors_by_entity_key_family_limited(&origin.entity_key, family, 128)
            .unwrap();
        // This is a set of positively verified locations, not a completeness or
        // uniqueness oracle. Always include the exact source occurrence.
        let mut paths: Vec<_> = related.iter().map(|anchor| anchor.path.clone()).collect();
        paths.push(row.fact.path.clone());
        inputs.push((uri, source, row, paths));
    }
    println!("binding_replay_declarations: {}", index.len());
    println!("binding_replay_files: {files}");
    for phase in ["cold", "warm", "edited"] {
        for feature in ["hover", "definition"] {
            let mut observations = Vec::with_capacity(64);
            for (uri, source, row, related_paths) in &inputs {
                if phase == "cold" {
                    service
                        .inner()
                        .session
                        .documents
                        .clear_live_state(uri)
                        .await;
                    drop(index.suspend_payload_cache_for_full_publication());
                }
                let position = row.fact.name_range.start;
                let request = async {
                    if feature == "hover" {
                        let value = service
                            .inner()
                            .hover(hover_params(uri.clone(), position.line, position.character))
                            .await
                            .unwrap()
                            .unwrap_or_else(|| panic!("missing {phase}/{feature} result for {}:{}:{} {} (id {}), observation {:?}",
                                row.fact.path, position.line, position.character, row.fact.name, row.id,
                                service.inner().session.binding_observations.lock().unwrap().back()));
                        let text = hover_text(value.contents);
                        assert!(
                            text.contains(&row.fact.name)
                                && related_paths.iter().any(|path| text.contains(path)),
                            "wrong hover witness: {}: {text}; allowed {related_paths:?}",
                            row.fact.path
                        );
                    } else {
                        let value = service
                            .inner()
                            .goto_definition(goto_definition_params(
                                uri.clone(),
                                position.line,
                                position.character,
                            ))
                            .await
                            .unwrap()
                            .unwrap_or_else(|| {
                                panic!(
                                    "missing {phase}/{feature} result for {}:{}:{} {} (id {})",
                                    row.fact.path,
                                    position.line,
                                    position.character,
                                    row.fact.name,
                                    row.id
                                )
                            });
                        let targets = definition_locations(value);
                        assert!(
                            targets.iter().any(|target| target.uri == *uri
                                && target.range.start.line == position.line
                                && target.range.start.character == position.character),
                            "wrong target for {}: {targets:?}",
                            row.fact.name
                        );
                    }
                };
                if phase == "edited" {
                    let edit = service.inner().session.change_document(
                        uri.clone(),
                        if feature == "hover" { 2 } else { 3 },
                        format!("{source}\n/* binding replay edit */\n"),
                    );
                    tokio::join!(request, edit);
                } else {
                    request.await;
                }
                let observation = service
                    .inner()
                    .session
                    .binding_observations
                    .lock()
                    .unwrap()
                    .back()
                    .unwrap()
                    .clone();
                assert!(observation.completed && observation.returned);
                observations.push(observation);
            }
            let mut elapsed: Vec<_> = observations.iter().map(|item| item.total_us).collect();
            elapsed.sort_unstable();
            let prefix = format!("binding_{phase}_{feature}");
            println!("{prefix}_requests: {}", observations.len());
            println!("{prefix}_correct_targets: {}", observations.len());
            for percentile in [50, 95, 99] {
                println!(
                    "{prefix}_p{percentile}_us: {}",
                    elapsed[elapsed.len() * percentile / 100]
                );
            }
            println!(
                "{prefix}_sqlite_read_sessions: {}",
                observations
                    .iter()
                    .map(|item| item.sqlite_read_sessions)
                    .sum::<usize>()
            );
            println!(
                "{prefix}_parse_cache_hits: {}",
                observations.iter().filter(|item| item.cache_hit).count()
            );
            for (name, values) in [
                (
                    "capture",
                    observations
                        .iter()
                        .map(|item| item.capture_us)
                        .collect::<Vec<_>>(),
                ),
                (
                    "parse",
                    observations.iter().map(|item| item.parse_us).collect(),
                ),
                (
                    "binding",
                    observations.iter().map(|item| item.binding_us).collect(),
                ),
                (
                    "overlay",
                    observations.iter().map(|item| item.overlay_us).collect(),
                ),
                (
                    "query",
                    observations.iter().map(|item| item.query_us).collect(),
                ),
                (
                    "hydration",
                    observations.iter().map(|item| item.hydration_us).collect(),
                ),
                (
                    "render",
                    observations.iter().map(|item| item.render_us).collect(),
                ),
            ] {
                let mut values = values;
                values.sort_unstable();
                println!(
                    "{prefix}_{name}_p95_us: {}",
                    values[values.len() * 95 / 100]
                );
            }
        }
    }
}
