use super::*;
use tower_lsp::LanguageServer as _;

#[tokio::test]
async fn ordinary_completion_does_not_open_history_store_on_completion_hot_path() {
    let (src, line, character) = text_and_position(
        "#define FS_MAGIC 1\n\
         void f(void) { FS/*cursor*/(); }\n",
    );
    let dir = tempdir().expect("tempdir");
    let history_path =
        crate::pathing::default_completion_history_path(dir.path()).expect("history path");
    std::fs::create_dir_all(history_path.parent().expect("history parent")).expect("mkdir");
    std::fs::write(&history_path, "{\"version\":1,\"entries\":[]}").expect("write history");
    let uri = Url::from_file_path(dir.path().join("a.c")).expect("file uri");
    let service = test_backend_service();
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(dir.path().to_path_buf());
    open_test_document(&service, uri.clone(), 1, src).await;
    service
        .inner()
        .set_completion_history_mode_for_test(crate::completion_history::CompletionHistoryMode::On)
        .await;

    service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion")
        .expect("response");

    assert!(
        service.inner().completion_history.lock().await.is_empty(),
        "ordinary completion should use only already-loaded in-memory history"
    );
}

#[tokio::test]
async fn ordinary_completion_presents_static_keyword_with_lsp_kind_and_detail() {
    let (src, line, character) = text_and_position("str/*cursor*/");
    let dir = tempdir().expect("tempdir");
    let uri = Url::from_file_path(dir.path().join("a.c")).expect("file uri");
    let service = test_backend_service();
    open_test_document(&service, uri.clone(), 1, src).await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion")
        .expect("response");
    assert!(completion_response_is_incomplete(&response));
    let item = completion_items(response)
        .into_iter()
        .find(|item| item.label == "struct")
        .expect("struct keyword completion");

    assert_eq!(item.kind, Some(CompletionItemKind::KEYWORD));
    assert_eq!(item.detail.as_deref(), Some("keyword"));
}

#[tokio::test]
async fn ordinary_completion_builtin_only_result_stays_incomplete() {
    let (src, line, character) = text_and_position("si/*cursor*/");
    let dir = tempdir().expect("tempdir");
    let uri = Url::from_file_path(dir.path().join("a.c")).expect("file uri");
    let service = test_backend_service();
    open_test_document(&service, uri.clone(), 1, src).await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion")
        .expect("response");

    assert!(completion_response_is_incomplete(&response));
    assert!(completion_items(response)
        .into_iter()
        .any(|item| item.label == "size_t"
            && item.kind == Some(CompletionItemKind::STRUCT)
            && item.detail.as_deref() == Some("builtin type")));
}

#[derive(Debug, PartialEq, Eq)]
struct PresentedCompletion {
    label: String,
    kind: Option<CompletionItemKind>,
    detail: Option<String>,
    documentation: Option<String>,
    sort_text: Option<String>,
    has_history_command: bool,
}

fn presented_completion(item: &CompletionItem) -> PresentedCompletion {
    PresentedCompletion {
        label: item.label.clone(),
        kind: item.kind,
        detail: item.detail.clone(),
        documentation: item.documentation.as_ref().map(|doc| match doc {
            Documentation::String(text) => text.clone(),
            Documentation::MarkupContent(markup) => markup.value.clone(),
        }),
        sort_text: item.sort_text.clone(),
        has_history_command: item.command.is_some(),
    }
}

#[tokio::test]
async fn ordinary_completion_compat_fixture_captures_presented_boundary_output() {
    let (src, line, character) = text_and_position(
        "#include \"reachable.h\"\n\
         #define fs_overlay_macro 1\n\
         typedef int fs_overlay_type;\n\
         int fixture(int fs_param) {\n\
             int fs_local_value;\n\
             fs_text_word();\n\
             fs/*cursor*/\n\
         }\n",
    );
    let dir = tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    write_workspace_file(dir.path(), "src/main.c", &src);
    write_workspace_file(dir.path(), "reachable.h", "int fs_reachable_index(void);\n");

    let uri = Url::from_file_path(root.join("src/main.c")).expect("file uri");
    let service = test_backend_service();
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
        .name_tables
        .lock()
        .await
        .insert(
            root.clone(),
            Arc::new(crate::query::NameTable::build_with_paths(vec![
                (
                    1,
                    "fs_reachable_index".to_string(),
                    false,
                    "reachable.h".to_string(),
                    "function".to_string(),
                    false,
                ),
                (
                    2,
                    "fs_external_index".to_string(),
                    true,
                    "sdk/external.h".to_string(),
                    "type".to_string(),
                    true,
                ),
                (
                    3,
                    "fs_unknown_index".to_string(),
                    false,
                    "ambiguous/unknown.h".to_string(),
                    "enum_constant".to_string(),
                    false,
                ),
                (
                    4,
                    "fs_global_index".to_string(),
                    false,
                    "global.c".to_string(),
                    "macro".to_string(),
                    false,
                ),
            ])),
        );
    service
        .inner()
        .session
        .cache
        .reach_graphs
        .lock()
        .await
        .insert(
            root.clone(),
            Arc::new(std::sync::RwLock::new(
                crate::reachability::ReachGraph::new(
                    vec![("src/main.c".to_string(), "reachable.h".to_string())],
                    vec![],
                    vec!["src/main.c".to_string()],
                ),
            )),
        );
    open_test_document(&service, uri.clone(), 1, src).await;
    service
        .inner()
        .set_completion_history_mode_for_test(crate::completion_history::CompletionHistoryMode::On)
        .await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    assert!(completion_response_is_incomplete(&response));
    let items = completion_items(response);
    let presented: Vec<_> = items.iter().take(9).map(presented_completion).collect();

    assert_eq!(
        presented,
        vec![
            PresentedCompletion {
                label: "fs_param".to_string(),
                kind: Some(CompletionItemKind::VARIABLE),
                detail: Some("parameter: int".to_string()),
                documentation: None,
                sort_text: Some("00000000".to_string()),
                has_history_command: true,
            },
            PresentedCompletion {
                label: "fs_local_value".to_string(),
                kind: Some(CompletionItemKind::VARIABLE),
                detail: Some("local: int".to_string()),
                documentation: None,
                sort_text: Some("00000001".to_string()),
                has_history_command: true,
            },
            PresentedCompletion {
                label: "fs_overlay_type".to_string(),
                kind: Some(CompletionItemKind::STRUCT),
                detail: Some("current".to_string()),
                documentation: None,
                sort_text: Some("00000002".to_string()),
                has_history_command: true,
            },
            PresentedCompletion {
                label: "fs_overlay_macro".to_string(),
                kind: Some(CompletionItemKind::CONSTANT),
                detail: Some("current".to_string()),
                documentation: None,
                sort_text: Some("00000003".to_string()),
                has_history_command: true,
            },
            PresentedCompletion {
                label: "fs_reachable_index".to_string(),
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some("reachable".to_string()),
                documentation: Some(
                    "FossilSense: reachable candidate (reachable, reachable_include)".to_string(),
                ),
                sort_text: Some("00000004".to_string()),
                has_history_command: true,
            },
            PresentedCompletion {
                label: "fs_external_index".to_string(),
                kind: Some(CompletionItemKind::STRUCT),
                detail: Some("external".to_string()),
                documentation: Some(
                    "FossilSense: external candidate (heuristic, external_first_layer)".to_string(),
                ),
                sort_text: Some("00000005".to_string()),
                has_history_command: true,
            },
            PresentedCompletion {
                label: "fs_global_index".to_string(),
                kind: Some(CompletionItemKind::CONSTANT),
                detail: Some("ambiguous".to_string()),
                documentation: Some(
                    "FossilSense: unknown candidate (ambiguous, global_fallback)".to_string(),
                ),
                sort_text: Some("00000006".to_string()),
                has_history_command: true,
            },
            PresentedCompletion {
                label: "fs_unknown_index".to_string(),
                kind: Some(CompletionItemKind::ENUM_MEMBER),
                detail: Some("ambiguous".to_string()),
                documentation: Some(
                    "FossilSense: unknown candidate (ambiguous, global_fallback)".to_string(),
                ),
                sort_text: Some("00000007".to_string()),
                has_history_command: true,
            },
            PresentedCompletion {
                label: "fs_text_word".to_string(),
                kind: Some(CompletionItemKind::TEXT),
                detail: Some("text".to_string()),
                documentation: None,
                sort_text: Some("00000008".to_string()),
                has_history_command: true,
            },
        ]
    );
}

// --- R7: local word vs indexed candidate tier ordering --------------------

#[test]
fn local_word_does_not_outrank_reachable_indexed_candidate() {
    // A local word's best possible score (exact match + locality bonus)
    // must not exceed a Reachable-tier indexed candidate's pack_score,
    // which uses strict-tier ordering (TIER_STRIDE) to dominate.
    // This verifies the design invariant: the resolver's pack_score
    // guarantees tier strictly dominates match quality.
    use crate::model::ScopeTier;
    use crate::query::completion_word_score;
    use crate::resolver;

    let local_best = completion_word_score("foo", "foo", crate::query::COMPLETION_LOCALITY_BONUS);
    assert!(local_best.is_some(), "exact match must score");

    // A Reachable-tier indexed candidate with a moderate base_match.
    let indexed_score = resolver::pack_score(
        ScopeTier::Reachable,
        800, // base_match (prefix quality)
        0,   // no locality bonus
    );
    assert!(
        indexed_score > local_best.unwrap(),
        "Reachable-tier indexed candidate (score {}) must outrank best local word (score {})",
        indexed_score,
        local_best.unwrap()
    );

    // Even an External-tier indexed candidate outranks best local words.
    let external_score = resolver::pack_score(
        ScopeTier::External,
        1000, // exact match
        0,
    );
    assert!(
        external_score > local_best.unwrap(),
        "External-tier indexed exact match (score {}) outranks best local word (score {})",
        external_score,
        local_best.unwrap()
    );
}

#[test]
fn completion_dedup_keeps_indexed_kind_over_same_name_local_word() {
    use crate::model::{ResolutionConfidence, ScopeTier};

    let indexed = crate::completion::PipelineCandidate::new(
        "hello_value",
        crate::completion::CandidateEvidence::new(
            crate::completion::CandidateSource::Indexed,
            ScopeTier::Reachable,
            ResolutionConfidence::Reachable,
            30_000,
        ),
        CompletionItem {
            label: "hello_value".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            ..Default::default()
        },
    );
    let local = crate::completion::PipelineCandidate::new(
        "hello_value",
        crate::completion::CandidateEvidence::new(
            crate::completion::CandidateSource::LocalWord,
            ScopeTier::Current,
            ResolutionConfidence::Heuristic,
            40_000,
        ),
        CompletionItem {
            label: "hello_value".to_string(),
            kind: Some(CompletionItemKind::TEXT),
            ..Default::default()
        },
    );

    let deduped = crate::completion::run_compatible_pipeline(vec![indexed, local], 10).items;
    assert_eq!(deduped.len(), 1);
    assert_eq!(deduped[0].payload.kind, Some(CompletionItemKind::FUNCTION));
}

#[test]
fn completion_dedup_keeps_local_binding_over_same_name_indexed_and_local_word() {
    use crate::model::{ResolutionConfidence, ScopeTier};

    let indexed = crate::completion::PipelineCandidate::new(
        "count",
        crate::completion::CandidateEvidence::new(
            crate::completion::CandidateSource::Indexed,
            ScopeTier::Reachable,
            ResolutionConfidence::Reachable,
            30_000,
        ),
        CompletionItem {
            label: "count".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            ..Default::default()
        },
    );
    let local_binding = crate::completion::PipelineCandidate::new(
        "count",
        crate::completion::CandidateEvidence::new(
            crate::completion::CandidateSource::LocalBinding,
            ScopeTier::Current,
            ResolutionConfidence::Heuristic,
            40_000,
        ),
        CompletionItem {
            label: "count".to_string(),
            kind: Some(CompletionItemKind::VARIABLE),
            detail: Some("parameter: int".to_string()),
            ..Default::default()
        },
    );
    let local_word = crate::completion::PipelineCandidate::new(
        "count",
        crate::completion::CandidateEvidence::new(
            crate::completion::CandidateSource::LocalWord,
            ScopeTier::Global,
            ResolutionConfidence::Fallback,
            1_000,
        ),
        CompletionItem {
            label: "count".to_string(),
            kind: Some(CompletionItemKind::TEXT),
            ..Default::default()
        },
    );

    let deduped =
        crate::completion::run_compatible_pipeline(vec![indexed, local_word, local_binding], 10)
            .items;
    assert_eq!(deduped.len(), 1);
    assert_eq!(
        deduped[0].evidence.source,
        crate::completion::CandidateSource::LocalBinding
    );
    assert_eq!(deduped[0].payload.kind, Some(CompletionItemKind::VARIABLE));
}

#[tokio::test]
async fn ordinary_completion_uses_unsaved_current_file_overlay() {
    let (src, line, character) = text_and_position(
        "#define FS_MAGIC 1\n\
         typedef int FsAlias;\n\
         void f(void) { FS/*cursor*/ }\n",
    );
    let dir = tempdir().expect("tempdir");
    let uri = Url::from_file_path(dir.path().join("a.c")).expect("file uri");
    let service = test_backend_service();
    open_test_document(&service, uri.clone(), 1, src).await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    if let CompletionResponse::List(list) = &response {
        assert!(list.is_incomplete);
    }
    let items = completion_items(response);

    assert!(items
        .iter()
        .any(|item| item.label == "FS_MAGIC" && item.detail.as_deref() == Some("current")));
    assert!(items
        .iter()
        .any(|item| item.label == "FsAlias" && item.detail.as_deref() == Some("current")));
}

#[tokio::test]
async fn current_file_text_overlay_renders_text_kind() {
    let (src, line, character) = text_and_position(
        "void f(void) {\n\
             localThing();\n\
             localT/*cursor*/\n\
         }\n",
    );
    let dir = tempdir().expect("tempdir");
    let uri = Url::from_file_path(dir.path().join("a.c")).expect("file uri");
    let service = test_backend_service();
    open_test_document(&service, uri.clone(), 1, src).await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let items = completion_items(response);
    let local = items
        .iter()
        .find(|item| item.label == "localThing")
        .expect("localThing text overlay completion");

    assert_eq!(local.kind, Some(CompletionItemKind::TEXT));
    assert_eq!(local.detail.as_deref(), Some("text"));
}

#[tokio::test]
async fn text_overlay_still_allows_exact_indexed_semantic_recovery() {
    let (src, line, character) = text_and_position(
        "void f(void) {\n\
             localThing();\n\
             loc/*cursor*/\n\
         }\n",
    );
    let dir = tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    let uri = Url::from_file_path(root.join("a.c")).expect("file uri");
    let service = test_backend_service();
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root.clone());
    let mut names: Vec<_> = (0..150)
        .map(|idx| {
            (
                idx,
                format!("localT{idx:03}"),
                false,
                "dense.c".to_string(),
                "global_variable".to_string(),
                false,
            )
        })
        .collect();
    names.push((
        999,
        "localThing".to_string(),
        false,
        "a.c".to_string(),
        "function".to_string(),
        false,
    ));
    service
        .inner()
        .session
        .cache
        .name_tables
        .lock()
        .await
        .insert(
            root,
            Arc::new(crate::query::NameTable::build_with_paths(names)),
        );
    open_test_document(&service, uri.clone(), 1, src).await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let items = completion_items(response);
    let local = items
        .iter()
        .find(|item| item.label == "localThing")
        .expect("localThing completion");

    assert_eq!(local.kind, Some(CompletionItemKind::FUNCTION));
    assert_ne!(local.detail.as_deref(), Some("text"));
}

#[test]
fn final_rank_sort_text_matches_pipeline_order() {
    let mut items = vec![
        CompletionItem {
            label: "b".into(),
            ..Default::default()
        },
        CompletionItem {
            label: "a".into(),
            ..Default::default()
        },
    ];

    super::apply_final_completion_sort_text(&mut items);

    assert_eq!(items[0].sort_text.as_deref(), Some("00000000"));
    assert_eq!(items[1].sort_text.as_deref(), Some("00000001"));
}

#[tokio::test]
async fn local_binding_pipeline_uses_open_document_bindings_before_local_words() {
    let src = "int f(int count) {\n    int cursor_limit;\n    cur\n}\n";
    let dir = tempdir().expect("tempdir");
    let uri = Url::from_file_path(dir.path().join("a.c")).expect("file uri");
    let service = test_backend_service();
    open_test_document(&service, uri.clone(), 1, src.to_string()).await;

    let response = service
        .inner()
        .completion(completion_params(uri, 2, 7))
        .await
        .expect("completion request")
        .expect("completion response");
    if let CompletionResponse::List(list) = &response {
        assert!(list.is_incomplete);
    }
    let items = completion_items(response);
    let cursor = items
        .iter()
        .find(|item| item.label == "cursor_limit")
        .expect("cursor_limit completion");

    assert_eq!(cursor.kind, Some(CompletionItemKind::VARIABLE));
    assert_eq!(cursor.detail.as_deref(), Some("local: int"));
}
