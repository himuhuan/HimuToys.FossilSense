use super::*;
use crate::query;

#[tokio::test]
async fn cursor_session_global_overlay_keeps_captured_source_after_reopen() {
    let (_dir, service, uri, _, _) = indexed_backend_with_open_doc(
        &[],
        "main.c",
        "int original;\nint f(void) { return original/*cursor*/; }\n",
    )
    .await;
    let backend = service.inner();
    let captured = backend.capture_query_session(&uri).await.unwrap();
    let original = backend
        .document_snapshot_from_request(&uri, &captured.documents)
        .await
        .unwrap()
        .1;
    let (replacement, line, col) =
        text_and_position("int replacement;\nint f(void) { return replacement/*cursor*/; }\n");
    backend.session.close_document(&uri).await;
    backend
        .session
        .open_document(uri.clone(), 1, replacement)
        .await;
    backend
        .hover(hover_params(uri.clone(), line, col))
        .await
        .unwrap()
        .unwrap();
    let overlay = backend
        .candidate_overlay_snapshot_from_documents(
            &captured.root,
            captured.context.engine.clone(),
            captured.documents,
        )
        .await;
    assert_eq!(overlay.source_text("main.c").unwrap(), original.as_ref());
    assert_eq!(
        overlay
            .declarations_for_family("original", crate::semantic_model::SemanticFamily::CFamily)
            .len(),
        1
    );
    assert!(overlay
        .declarations_for_family(
            "replacement",
            crate::semantic_model::SemanticFamily::CFamily
        )
        .is_empty());
}

#[tokio::test]
async fn cursor_session_external_overlay_cache_is_bound_to_source_content() {
    let service = test_backend_service();
    let external = tempdir().unwrap();
    let uri = Url::from_file_path(external.path().join("outside.h")).unwrap();
    let backend = service.inner();
    let language = crate::config::LanguageSelection::explicit(crate::config::SourceLanguage::C);
    backend
        .session
        .open_document(uri.clone(), 1, "int original;\n".into())
        .await;
    backend.session.close_document(&uri).await;
    backend
        .session
        .open_document(uri.clone(), 1, "int replacement;\n".into())
        .await;
    backend
        .get_or_parse_external_overlay_document(
            &uri,
            "outside.h",
            1,
            "int replacement;\n",
            language,
        )
        .await
        .unwrap();
    let old = backend
        .get_or_parse_external_overlay_document(&uri, "outside.h", 1, "int original;\n", language)
        .await
        .unwrap();
    assert!(old
        .declarations
        .iter()
        .any(|declaration| declaration.name == "original"));
    assert!(!old
        .declarations
        .iter()
        .any(|declaration| declaration.name == "replacement"));
    let current = backend
        .get_or_parse_external_overlay_document(
            &uri,
            "outside.h",
            1,
            "int replacement;\n",
            language,
        )
        .await
        .unwrap();
    assert!(current
        .declarations
        .iter()
        .any(|declaration| declaration.name == "replacement"));
}

#[tokio::test]
async fn cursor_session_overlay_cache_does_not_retain_each_old_engine() {
    let cache = super::super::CacheLedger::default();
    let root = PathBuf::from("cursor-cache-root");
    let generation = crate::call_model::SemanticGeneration(1);
    for epoch in 1..100 {
        let engine = super::super::state::EngineEpoch::published(epoch);
        let (_, revision) = cache
            .candidate_overlay_for_engine(&root, generation, 1, engine)
            .await;
        cache
            .publish_candidate_overlay_for_engine(
                root.clone(),
                generation,
                1,
                engine,
                revision,
                Arc::new(crate::candidate_service::CandidateOverlaySnapshot::new(
                    1,
                    Vec::new(),
                )),
            )
            .await;
        assert_eq!(cache.candidate_overlay_cache_len_for_test().await, 1);
    }
}

#[tokio::test]
async fn cursor_domain_type_tag_and_qualified_names_preserve_namespace() {
    for (source, expected) in [
        (
            "typedef int value;\nvalue/*cursor*/ f(void);\n",
            Some(Position::new(0, 12)),
        ),
        (
            "struct value { int x; };\nstruct value/*cursor*/ *p;\n",
            Some(Position::new(0, 7)),
        ),
        (
            "typedef int value;\nint f(void) { return missing::value/*cursor*/; }\n",
            None,
        ),
    ] {
        let (_dir, service, uri, line, character) =
            indexed_backend_with_open_doc(&[], "main.cpp", source).await;
        let result = service
            .inner()
            .goto_definition(goto_definition_params(uri.clone(), line, character))
            .await
            .unwrap();
        if let Some(expected) = expected {
            let targets = definition_locations(result.expect("type target"));
            assert!(!targets.is_empty());
            assert!(
                targets
                    .iter()
                    .all(|target| target.uri == uri && target.range.start == expected),
                "{targets:?}"
            );
        } else {
            assert!(result.is_none());
        }
    }
}

#[tokio::test]
async fn cursor_domain_go_never_returns_c_typedef_or_unrelated_qualified_type() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[
            ("other.c", "typedef int value;\n"),
            ("go.mod", "module example.test/demo\n\ngo 1.22\n"),
        ],
        "main.go",
        "package demo\nfunc f() int { return value/*cursor*/ }\n",
    )
    .await;
    assert!(service
        .inner()
        .goto_definition(goto_definition_params(uri, line, character))
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn cursor_session_reopened_version_cannot_reuse_different_source_facts() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[],
        "main.c",
        "int f(int original) { return original/*cursor*/; }\n",
    )
    .await;
    let backend = service.inner();
    let session = backend.capture_query_session(&uri).await.unwrap();
    let original = backend
        .document_snapshot_from_request(&uri, &session.documents)
        .await
        .unwrap();
    backend.session.close_document(&uri).await;
    backend
        .session
        .open_document(
            uri.clone(),
            1,
            "int f(int replacement) { return replacement; }\n".into(),
        )
        .await;
    backend
        .hover(hover_params(uri.clone(), 0, 33))
        .await
        .unwrap();
    let byte = query::byte_offset_at(&original.1, line, character);
    let binding = session
        .bind_cursor(backend, &uri, original, "original", byte)
        .await
        .unwrap();
    assert!(
        matches!(binding.resolution, query::BindingResolution::Resolved(_)),
        "{:?}",
        binding.resolution
    );
    let current = backend
        .hover(hover_params(uri.clone(), 0, 33))
        .await
        .unwrap()
        .unwrap();
    assert!(hover_text(current.contents).contains("replacement"));
}

#[tokio::test]
async fn cursor_domain_jsonrpc_route_resolves_parameter_and_records_full_request() {
    use tower::Service;
    use tower_lsp::jsonrpc::Request;
    let (_dir, mut service, uri, line, character) = indexed_backend_with_open_doc(
        &[],
        "main.c",
        "typedef int value;\nint f(int value) { return 1 ? value/*cursor*/ : 0; }\n",
    )
    .await;
    let initialize = Request::build("initialize").id(1).params(serde_json::json!({"capabilities":{}, "rootUri": Url::from_directory_path(_dir.path()).unwrap()})).finish();
    let response = service.call(initialize).await.unwrap().unwrap();
    assert!(response.error().is_none(), "{response:?}");
    let request = Request::build("textDocument/definition")
        .id(2)
        .params(serde_json::to_value(goto_definition_params(uri.clone(), line, character)).unwrap())
        .finish();
    let response = service.call(request).await.unwrap().unwrap();
    assert!(response.error().is_none(), "{response:?}");
    let response: GotoDefinitionResponse =
        serde_json::from_value(response.result().unwrap().clone()).unwrap();
    let targets = definition_locations(response);
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].uri, uri);
    assert_eq!(targets[0].range.start, Position::new(1, 10));
    let observations = service.inner().session.binding_observations.lock().unwrap();
    let last = observations.back().unwrap();
    assert!(last.completed && last.returned);
    assert!(last.total_us >= last.parse_us + last.binding_us);
    assert_eq!(last.sqlite_read_sessions, 0);
}

#[tokio::test]
async fn cursor_domain_hover_and_navigation_share_parse_and_time_empty_results() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[],
        "main.c",
        "int f(int local) { return local/*cursor*/; }\n",
    )
    .await;
    service
        .inner()
        .hover(hover_params(uri.clone(), line, character))
        .await
        .unwrap();
    service
        .inner()
        .goto_definition(goto_definition_params(uri.clone(), line, character))
        .await
        .unwrap();
    {
        let observations = service.inner().session.binding_observations.lock().unwrap();
        assert_eq!(observations.len(), 2);
        assert!(observations.back().unwrap().cache_hit);
        assert!(observations
            .iter()
            .all(|item| item.completed && item.returned && item.sqlite_read_sessions == 0));
    }
    service
        .inner()
        .hover(hover_params(uri, 200, 0))
        .await
        .unwrap();
    let observations = service.inner().session.binding_observations.lock().unwrap();
    let last = observations.back().unwrap();
    assert!(last.completed && !last.returned);
    assert_eq!(
        last.outcome,
        super::super::query_session::QueryOutcome::NoCandidates
    );
}

#[test]
fn query_outcome_reasons_remain_distinct_before_protocol_mapping() {
    use super::super::query_session::QueryOutcome;
    use super::super::request_target::{target_unavailable_outcome, TargetUnavailable};
    use crate::declaration_read_handle::DeclarationReadFailureReason;
    use crate::parser::LookupDomain;

    let outcomes = [
        target_unavailable_outcome(&TargetUnavailable::MissingLabel),
        target_unavailable_outcome(&TargetUnavailable::Unsupported {
            domain: LookupDomain::Member,
        }),
        QueryOutcome::from_read_failure(DeclarationReadFailureReason::SnapshotUnavailable),
        QueryOutcome::from_read_failure(DeclarationReadFailureReason::ExecutionFailed),
        QueryOutcome::from_read_failure(DeclarationReadFailureReason::Cancelled),
    ];

    assert_eq!(outcomes[0], QueryOutcome::NoCandidates);
    assert_eq!(outcomes[1], QueryOutcome::Unsupported);
    assert_eq!(outcomes[2], QueryOutcome::SnapshotUnavailable);
    assert_eq!(outcomes[3], QueryOutcome::ExecutionFailed);
    assert_eq!(outcomes[4], QueryOutcome::Cancelled);
    assert_eq!(
        outcomes
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        5
    );
}

#[tokio::test]
async fn cursor_domain_cancelled_request_is_recorded_without_unbounded_history() {
    let (_dir, service, uri, _, _) =
        indexed_backend_with_open_doc(&[], "main.c", "int value/*cursor*/;\n").await;
    let roots_guard = service.inner().workspace_roots.lock().await;
    let mut request = Box::pin(service.inner().hover(hover_params(uri, 0, 4)));
    std::future::poll_fn(|cx| {
        assert!(std::future::Future::poll(request.as_mut(), cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    drop(request);
    drop(roots_guard);
    assert!(
        !service
            .inner()
            .session
            .binding_observations
            .lock()
            .unwrap()
            .back()
            .unwrap()
            .completed
    );
    assert_eq!(
        service
            .inner()
            .session
            .binding_observations
            .lock()
            .unwrap()
            .back()
            .unwrap()
            .outcome,
        super::super::query_session::QueryOutcome::Cancelled
    );
    let cancelled = service
        .inner()
        .session
        .binding_observations
        .lock()
        .unwrap()
        .back()
        .unwrap()
        .clone();
    assert!(
        super::super::query_session::dropped_observation_log_line(true, &cancelled)
            .expect("performance logging emits a dropped request")
            .contains("\"outcome\":\"cancelled\"")
    );
    assert!(super::super::query_session::dropped_observation_log_line(false, &cancelled).is_none());
    for _ in 0..140 {
        let timer = super::super::query_session::BindingTimer::new(service.inner(), "hover");
        drop(timer);
    }
    let observations = service.inner().session.binding_observations.lock().unwrap();
    assert_eq!(observations.len(), 128);
    assert!(observations.iter().all(|item| !item.completed));
    assert!(observations
        .iter()
        .all(|item| item.outcome == super::super::query_session::QueryOutcome::Cancelled));
}

#[tokio::test]
async fn cursor_session_retries_same_generation_engine_swap_and_captures_edit() {
    let (_dir, service, uri, _, _) = indexed_backend_with_open_doc(
        &[],
        "main.c",
        "int value;\nint f(void) { return value/*cursor*/; }\n",
    )
    .await;
    let backend = service.inner();
    let root = backend.root_for_uri(&uri).await.unwrap();
    let before = backend.request_context_for_root(root.clone()).await.engine;
    let changed = "int edited;\nint f(void) { return edited; }\n";
    let session = backend
        .capture_query_session_with_hook(&uri, |attempt, after_documents| {
            let before = before.clone();
            let uri = uri.clone();
            async move {
                if attempt == 0 && after_documents {
                    backend
                        .session
                        .change_document(uri, 2, changed.into())
                        .await;
                    let mut next = (*before).clone();
                    next.epoch = backend.session.cache.allocate_engine_epoch();
                    backend
                        .session
                        .cache
                        .publish_engine_snapshot(next)
                        .await
                        .expect("cursor test snapshot identity");
                }
            }
        })
        .await
        .unwrap();
    assert_eq!(
        session.context.engine.semantic_generation,
        before.semantic_generation
    );
    assert!(!Arc::ptr_eq(&session.context.engine, &before));
    let (version, text) = backend
        .document_snapshot_from_request(&uri, &session.documents)
        .await
        .unwrap();
    assert_eq!(version, 2);
    assert_eq!(text.as_ref(), changed);
    backend
        .session
        .change_document(uri.clone(), 3, "int later;".into())
        .await;
    assert_eq!(
        backend
            .document_snapshot_from_request(&uri, &session.documents)
            .await
            .unwrap()
            .0,
        2
    );
}

#[tokio::test]
async fn cursor_session_continuous_engine_race_stops_after_three_attempts() {
    let (_dir, service, uri, _, _) =
        indexed_backend_with_open_doc(&[], "main.c", "int value/*cursor*/;\n").await;
    let backend = service.inner();
    let root = backend.root_for_uri(&uri).await.unwrap();
    let attempts = std::sync::atomic::AtomicUsize::new(0);
    let result = backend
        .capture_query_session_with_hook(&uri, |_, after_documents| {
            let root = root.clone();
            let attempts = &attempts;
            async move {
                if after_documents {
                    attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let current = backend.request_context_for_root(root).await.engine;
                    let mut next = (*current).clone();
                    next.epoch = backend.session.cache.allocate_engine_epoch();
                    backend
                        .session
                        .cache
                        .publish_engine_snapshot(next)
                        .await
                        .expect("cursor test snapshot identity");
                }
            }
        })
        .await;
    assert!(result.is_none());
    assert_eq!(attempts.load(std::sync::atomic::Ordering::Relaxed), 3);
}

#[tokio::test]
async fn cursor_domain_nested_shadowing_and_utf16_keep_exact_local_target() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[], "main.c", "int value;\nint f(int value) {\n { int value = 1; const char *s = \"😀\"; return value/*cursor*/; }\n}\n",
    ).await;
    let result = service
        .inner()
        .goto_definition(goto_definition_params(uri.clone(), line, character))
        .await
        .unwrap()
        .unwrap();
    let locations = definition_locations(result);
    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0].uri, uri);
    assert_eq!(
        locations[0].range,
        tower_lsp::lsp_types::Range::new(Position::new(2, 7), Position::new(2, 12))
    );
}

#[tokio::test]
async fn cursor_domain_ternary_parameter_beats_typedef_in_navigation_and_hover() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[], "main.c",
        "typedef int value;\nint f(int flag, int value) {\n return flag ? value/*cursor*/ : 0;\n}\n",
    ).await;
    for response in [
        service
            .inner()
            .goto_definition(goto_definition_params(uri.clone(), line, character))
            .await
            .unwrap(),
        service
            .inner()
            .goto_declaration(goto_definition_params(uri.clone(), line, character))
            .await
            .unwrap(),
    ] {
        let locations = definition_locations(response.expect("parameter binding"));
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].uri, uri);
        assert_eq!(locations[0].range.start, Position::new(1, 20));
    }
    let text = hover_text(
        service
            .inner()
            .hover(hover_params(uri, line, character))
            .await
            .unwrap()
            .unwrap()
            .contents,
    );
    assert!(text.contains("lexical_binding"), "{text}");
    assert!(!text.contains("typedef"), "{text}");
}

#[tokio::test]
async fn cursor_domain_members_never_bind_same_name_typedef() {
    for source in [
        "struct S { int state; };\nint f(struct S *p) { return p->state/*cursor*/; }\n",
        "int f(void) { return unknown->state/*cursor*/; }\n",
        "struct S { int state; };\nstruct S s = { .state/*cursor*/ = 1 };\n",
    ] {
        let (dir, service, uri, line, character) =
            indexed_backend_with_open_doc(&[("other.c", "typedef int state;\n")], "main.c", source)
                .await;
        let unrelated = Url::from_file_path(dir.path().join("other.c")).unwrap();
        if let Some(response) = service
            .inner()
            .goto_definition(goto_definition_params(uri.clone(), line, character))
            .await
            .unwrap()
        {
            assert!(definition_locations(response)
                .iter()
                .all(|location| location.uri != unrelated));
        }
        if let Some(hover) = service
            .inner()
            .hover(hover_params(uri, line, character))
            .await
            .unwrap()
        {
            assert!(!hover_text(hover.contents).contains("typedef int state"));
        }
    }
}

#[tokio::test]
async fn cursor_domain_non_code_and_value_mismatch_have_no_cross_domain_targets() {
    for source in [
        "// value/*cursor*/\nint f(void) { return 1; }\n",
        "const char *s = \"value/*cursor*/\";\n",
        "int f(void) { return value/*cursor*/; }\n",
    ] {
        let (_dir, service, uri, line, character) =
            indexed_backend_with_open_doc(&[("other.c", "typedef int value;\n")], "main.c", source)
                .await;
        assert!(service
            .inner()
            .goto_definition(goto_definition_params(uri.clone(), line, character))
            .await
            .unwrap()
            .is_none());
        assert!(service
            .inner()
            .hover(hover_params(uri, line, character))
            .await
            .unwrap()
            .is_none());
    }
}

#[tokio::test]
async fn cursor_domain_conditional_local_types_do_not_guess_a_branch() {
    for declaration in [
        "#if FLAG\nstruct S { int a; };\n#else\nstruct S { int b; };\n#endif\nstruct S/*cursor*/ *p;",
        "#if FLAG\ntypedef int S;\n#else\ntypedef char S;\n#endif\nS/*cursor*/ p;",
    ] {
        let source = format!("void f(void) {{\n{declaration}\n}}\n");
        let (_dir, service, uri, line, col) = indexed_backend_with_open_doc(&[], "main.c", &source).await;
        let result = service.inner().goto_definition(goto_definition_params(uri.clone(), line, col)).await.unwrap();
        assert!(result.is_none(), "conditional declarations must not bind arbitrarily: {result:?}");
        let hover = service.inner().hover(hover_params(uri, line, col)).await.unwrap();
        assert!(hover.is_none(), "conditional declarations must not present one branch: {hover:?}");
    }
}

#[tokio::test]
async fn cursor_hover_definition_keeps_proven_header_counterpart() {
    let (_dir, service, uri, line, col) = indexed_backend_with_open_doc(
        &[(
            "api.h",
            "#ifndef API_H\n#define API_H\nint run_task(void);\n#endif\n",
        )],
        "impl.c",
        "#include \"api.h\"\nint run_task/*cursor*/(void) { return 1; }\n",
    )
    .await;
    let hover = service
        .inner()
        .hover(hover_params(uri, line, col))
        .await
        .unwrap()
        .expect("a source definition must retain its proven callable presentation");
    let text = hover_text(hover.contents);
    assert!(
        text.contains("run_task") && text.contains("api.h"),
        "{text}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cursor_captured_request_survives_newer_edit_cancellation() {
    let source = format!(
        "static int original/*cursor*/(void) {{ return 1; }}\n{}",
        (0..20_000)
            .map(|i| format!("int object_{i};\n"))
            .collect::<String>()
    );
    let (_dir, service, uri, line, col) =
        indexed_backend_with_open_doc(&[], "main.c", &source).await;
    let backend = service.inner();
    let captured = backend.capture_query_session(&uri).await.unwrap();
    let doc = backend
        .document_snapshot_from_request(&uri, &captured.documents)
        .await
        .unwrap();
    let token = backend
        .session
        .documents
        .live_parse_cancellation(&uri, 1)
        .await;
    let byte = query::byte_offset_at(&doc.1, line, col);
    let bind = captured.bind_cursor(backend, &uri, doc, "original", byte);
    let edit = async {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while Arc::strong_count(&token) < 4 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the captured parse actually started");
        backend
            .session
            .change_document(
                uri.clone(),
                2,
                "static int replacement(void) { return 2; }\n".into(),
            )
            .await;
    };
    let (binding, ()) = tokio::join!(bind, edit);
    assert!(token.load(std::sync::atomic::Ordering::Relaxed));
    assert!(
        binding.is_some(),
        "owned request source must survive cancellation of obsolete live work"
    );
    let overlay = backend
        .candidate_overlay_snapshot_from_documents(
            &captured.root,
            captured.context.engine.clone(),
            captured.documents,
        )
        .await;
    assert!(!overlay
        .declarations_for_family("original", crate::semantic_model::SemanticFamily::CFamily)
        .is_empty());
    assert!(overlay
        .declarations_for_family(
            "replacement",
            crate::semantic_model::SemanticFamily::CFamily
        )
        .is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cursor_captured_go_overlay_keeps_package_identity_after_cancellation() {
    let source = format!(
        "package demo\nfunc original/*cursor*/() {{}}\n{}",
        (0..20_000)
            .map(|i| format!("var object_{i} int\n"))
            .collect::<String>()
    );
    let (_dir, service, uri, _, _) =
        indexed_backend_with_open_doc(&[], "pkg/main.go", &source).await;
    let backend = service.inner();
    let captured = backend.capture_query_session(&uri).await.unwrap();
    let doc = backend
        .document_snapshot_from_request(&uri, &captured.documents)
        .await
        .unwrap();
    let reference = crate::parser::parse_thread_local_with_selection(
        std::path::Path::new("pkg/main.go"),
        &doc.1,
        captured
            .context
            .engine
            .workspace_semantics
            .selection_for_uri(&uri, &doc.1),
        crate::parser::ParseFacts::HOVER_SEMANTICS,
    );
    let token = backend
        .session
        .documents
        .live_parse_cancellation(&uri, 1)
        .await;
    let capture = backend.candidate_overlay_snapshot_from_documents(
        &captured.root,
        captured.context.engine.clone(),
        captured.documents,
    );
    let edit = async {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while Arc::strong_count(&token) < 4 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("captured Go parse started");
        backend
            .session
            .change_document(
                uri.clone(),
                2,
                "package other\nfunc replacement() {}\n".into(),
            )
            .await;
    };
    let (overlay, ()) = tokio::join!(capture, edit);
    assert!(token.load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(
        overlay.callable_anchors("original"),
        reference.callable_anchors.as_slice()
    );
    let expected = reference
        .declarations
        .iter()
        .find(|fact| fact.name == "original")
        .unwrap();
    let actual =
        overlay.declarations_for_family("original", crate::semantic_model::SemanticFamily::Go);
    assert_eq!(actual[0].fact.identity, expected.identity);
}
