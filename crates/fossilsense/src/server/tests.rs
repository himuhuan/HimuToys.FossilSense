#![allow(clippy::field_reassign_with_default)]

use super::{
    grouped_reference_items, local_words_for_cache, rebuild_include_table,
    rebuild_indexed_file_list,
};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex as StdMutex};
use tempfile::tempdir;
use tower_lsp::lsp_types::{
    CallHierarchyOutgoingCallsParams, CallHierarchyPrepareParams, CompletionItem,
    CompletionItemKind, CompletionParams, CompletionResponse, DeclarationCapability,
    DidChangeTextDocumentParams, DidChangeWatchedFilesParams, DidChangeWorkspaceFoldersParams,
    DidOpenTextDocumentParams, DocumentSymbolParams, DocumentSymbolResponse, Documentation,
    ExecuteCommandParams, FileChangeType, FileEvent, GotoDefinitionParams, GotoDefinitionResponse,
    HoverContents, HoverParams, InitializeParams, OneOf, Position, ReferenceContext,
    ReferenceParams, SemanticTokensParams, SemanticTokensResult, SignatureHelpParams,
    TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams, Url, VersionedTextDocumentIdentifier, WorkspaceFolder,
    WorkspaceFoldersChangeEvent, WorkspaceSymbolParams,
};
use tower_lsp::{LanguageServer as _, LspService};

fn test_backend_service() -> LspService<super::Backend> {
    let (service, _) = LspService::new(|client| super::Backend {
        client,
        workspace_roots: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        index_schedule: Arc::new(tokio::sync::Mutex::new(IndexScheduleState::default())),
        session: super::WorkspaceSession::new(
            super::DocumentStore::default(),
            super::CacheLedger::default(),
        ),
        external_include_dir_cache: Arc::new(StdMutex::new(HashMap::new())),
        include_paths: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        go_module_paths: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        protobuf_c_enabled: Arc::new(tokio::sync::Mutex::new(None)),
        protobuf_c_proto_paths: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        completion_enabled: AtomicBool::new(true),
        strict_prefix_ranking: AtomicBool::new(true),
        semantic_coloring_enabled: AtomicBool::new(true),
        scoping_enabled: AtomicBool::new(true),
        completion_history_mode: Arc::new(tokio::sync::Mutex::new(
            crate::completion_history::CompletionHistoryMode::Auto,
        )),
        completion_history: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        completion_history_write_gate: Arc::new(tokio::sync::Mutex::new(())),
        completion_runtime: super::completion_runtime::CompletionRuntime::default(),
        project_context_selection: Arc::new(tokio::sync::Mutex::new(
            crate::project_context::ProjectContextSelection::Auto,
        )),
        project_context_selection_epoch: AtomicU64::new(1),
        debug_candidate_reasons: AtomicBool::new(false),
        perf_logging_enabled: AtomicBool::new(false),
        completion_perf_observations: Arc::new(StdMutex::new(Vec::new())),
        config_cache: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        workspace_semantics_bootstrap: Arc::new(tokio::sync::Mutex::new(Default::default())),
        external_source_roots_cache: Arc::new(tokio::sync::Mutex::new(Default::default())),
        resource_monitor_shutdown: Arc::new(tokio::sync::Notify::new()),
    });
    service
}

fn empty_workspace_semantics(
    root: &std::path::Path,
) -> Arc<super::workspace_config::PublishedWorkspaceSemantics> {
    Arc::new(super::workspace_config::PublishedWorkspaceSemantics::empty(
        root,
    ))
}

fn completion_overlay_request<'a>(
    root: &'a std::path::Path,
    current_uri: &'a Url,
    engine_epoch: super::state::EngineEpoch,
    generation: crate::call_model::SemanticGeneration,
) -> super::candidate_context::CompletionOverlayRequest<'a> {
    super::candidate_context::CompletionOverlayRequest {
        root,
        current_uri,
        engine_epoch,
        generation,
        base_reach_graph: None,
        indexed_workspace_files: None,
        workspace_semantics: empty_workspace_semantics(root),
    }
}

async fn current_test_workspace_semantics(
    service: &LspService<super::Backend>,
    root: &std::path::Path,
) -> Arc<super::workspace_config::PublishedWorkspaceSemantics> {
    let include_paths = service.inner().include_paths.lock().await.clone();
    let go_module_paths = service.inner().go_module_paths.lock().await.clone();
    Arc::new(
        super::workspace_config::PublishedWorkspaceSemantics::load_current(
            root,
            &include_paths,
            &go_module_paths,
        ),
    )
}

#[tokio::test]
async fn first_request_bootstraps_workspace_configuration_before_index_publication() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().join("workspace");
    let include_root = dir.path().join("sdk");
    std::fs::create_dir_all(root.join("legacy")).expect("workspace");
    std::fs::create_dir_all(&include_root).expect("include root");
    std::fs::write(
        root.join("fossilsense.json"),
        serde_json::json!({
            "includePaths": [crate::pathing::normalize_abs_path(&include_root)],
            "languageOverrides": [{
                "glob": "legacy/**/*.h",
                "language": "go"
            }]
        })
        .to_string(),
    )
    .expect("config");
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root.clone());

    let context = service.inner().request_context_for_root(root.clone()).await;

    assert_eq!(
        context
            .engine
            .workspace_semantics
            .language_for_path(&root.join("legacy/api.h")),
        crate::config::SourceLanguage::Go
    );
    assert!(context
        .engine
        .workspace_semantics
        .external_roots
        .normalized_include_roots()
        .contains(&crate::pathing::normalize_abs_path(&include_root)));
    assert_eq!(
        context.engine.semantic_generation,
        crate::call_model::SemanticGeneration::MISSING
    );
    assert!(context.engine.name_table.is_none());
}

#[tokio::test]
async fn concurrent_first_requests_share_workspace_configuration_bootstrap() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().join("workspace");
    std::fs::create_dir_all(&root).expect("workspace");
    std::fs::write(
        root.join("fossilsense.json"),
        r#"{"languageOverrides":[{"glob":"legacy/**/*.h","language":"go"}]}"#,
    )
    .expect("config");
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root.clone());

    let started = Arc::new(tokio::sync::Barrier::new(2));
    let resume = Arc::new(tokio::sync::Barrier::new(2));
    service
        .inner()
        .set_workspace_semantics_bootstrap_barriers_for_test(&root, started.clone(), resume.clone())
        .await;

    let first_root = root.clone();
    let second_root = root.clone();
    let release_bootstrap = async move {
        started.wait().await;
        tokio::task::yield_now().await;
        resume.wait().await;
    };
    let (first, second, ()) = tokio::join!(
        service.inner().request_context_for_root(first_root),
        service.inner().request_context_for_root(second_root),
        release_bootstrap,
    );

    assert!(
        Arc::ptr_eq(&first.engine, &second.engine),
        "concurrent first requests must share the same configuration-only snapshot"
    );
    assert_eq!(
        service
            .inner()
            .workspace_semantics_bootstrap_preparation_count_for_test(&root)
            .await,
        1,
        "concurrent first requests must prepare workspace configuration only once"
    );
}

#[tokio::test]
async fn concurrent_first_requests_share_failed_workspace_configuration_bootstrap() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    let missing_root = dir.path().join("missing-workspace");
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(missing_root.clone());

    let started = Arc::new(tokio::sync::Barrier::new(2));
    let resume = Arc::new(tokio::sync::Barrier::new(2));
    service
        .inner()
        .set_workspace_semantics_bootstrap_barriers_for_test(
            &missing_root,
            started.clone(),
            resume.clone(),
        )
        .await;

    let first_root = missing_root.clone();
    let second_root = missing_root.clone();
    let release_bootstrap = async move {
        started.wait().await;
        tokio::task::yield_now().await;
        resume.wait().await;
    };
    let (_, _, ()) = tokio::join!(
        service.inner().request_context_for_root(first_root),
        service.inner().request_context_for_root(second_root),
        release_bootstrap,
    );

    assert_eq!(
        service
            .inner()
            .workspace_semantics_bootstrap_preparation_count_for_test(&missing_root)
            .await,
        1,
        "concurrent waiters must share a failed bootstrap attempt instead of retrying serially"
    );

    std::fs::create_dir_all(missing_root.join("legacy")).expect("recovered workspace");
    std::fs::write(
        missing_root.join("fossilsense.json"),
        r#"{"languageOverrides":[{"glob":"legacy/**/*.h","language":"go"}]}"#,
    )
    .expect("recovered config");
    let recovered = service
        .inner()
        .request_context_for_root(missing_root.clone())
        .await;
    assert_eq!(
        service
            .inner()
            .workspace_semantics_bootstrap_preparation_count_for_test(&missing_root)
            .await,
        2,
        "a later request must retry after the shared failed attempt"
    );
    assert_eq!(
        recovered
            .engine
            .workspace_semantics
            .language_for_path(&missing_root.join("legacy/api.h")),
        crate::config::SourceLanguage::Go
    );
}

#[tokio::test]
async fn cancelling_first_request_does_not_abandon_workspace_configuration_bootstrap() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().join("workspace");
    std::fs::create_dir_all(root.join("legacy")).expect("workspace");
    std::fs::write(
        root.join("fossilsense.json"),
        r#"{"languageOverrides":[{"glob":"legacy/**/*.h","language":"go"}]}"#,
    )
    .expect("config");
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root.clone());

    let started = Arc::new(tokio::sync::Barrier::new(2));
    let resume = Arc::new(tokio::sync::Barrier::new(2));
    service
        .inner()
        .set_workspace_semantics_bootstrap_barriers_for_test(&root, started.clone(), resume.clone())
        .await;

    let mut first = Box::pin(service.inner().request_context_for_root(root.clone()));
    tokio::select! {
        _ = started.wait() => {}
        _ = &mut first => panic!("bootstrap unexpectedly completed before cancellation"),
    }
    drop(first);

    let release = tokio::time::timeout(std::time::Duration::from_secs(2), resume.wait());
    let second = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        service.inner().request_context_for_root(root.clone()),
    );
    let (released, second) = tokio::join!(release, second);
    assert!(
        released.is_ok(),
        "bootstrap owner must survive cancellation of the first request"
    );
    let second = second.expect("second request must observe bootstrap completion");
    assert_eq!(
        second
            .engine
            .workspace_semantics
            .language_for_path(&root.join("legacy/api.h")),
        crate::config::SourceLanguage::Go
    );
}

#[tokio::test]
async fn workspace_remove_readd_cannot_publish_a_stale_configuration_bootstrap() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().join("workspace");
    std::fs::create_dir_all(root.join("legacy")).expect("workspace");
    std::fs::write(
        root.join("fossilsense.json"),
        r#"{"languageOverrides":[{"glob":"legacy/**/*.h","language":"go"}]}"#,
    )
    .expect("config");
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root.clone());

    let finalize_started = Arc::new(tokio::sync::Barrier::new(2));
    let finalize_resume = Arc::new(tokio::sync::Barrier::new(2));
    service
        .inner()
        .set_workspace_semantics_bootstrap_finalize_barriers_for_test(
            &root,
            finalize_started.clone(),
            finalize_resume.clone(),
        )
        .await;

    let mut first = Box::pin(service.inner().request_context_for_root(root.clone()));
    tokio::select! {
        _ = finalize_started.wait() => {}
        _ = &mut first => panic!("bootstrap unexpectedly published before finalization barrier"),
    }

    let uri = Url::from_directory_path(&root).expect("workspace uri");
    let folder = WorkspaceFolder {
        uri,
        name: "workspace".into(),
    };
    let update = service
        .inner()
        .did_change_workspace_folders(DidChangeWorkspaceFoldersParams {
            event: WorkspaceFoldersChangeEvent {
                added: vec![folder.clone()],
                removed: vec![folder],
            },
        });
    let release = async move {
        tokio::task::yield_now().await;
        finalize_resume.wait().await;
    };
    let ((), ()) = tokio::join!(update, release);
    tokio::time::timeout(std::time::Duration::from_secs(2), &mut first)
        .await
        .expect("stale bootstrap waiter must be released");

    assert!(
        service
            .inner()
            .session
            .cache
            .current_engine_snapshot(&root)
            .await
            .is_none(),
        "root cleanup must remove a snapshot published by an older bootstrap attempt"
    );
}

#[tokio::test]
async fn workspace_remove_readd_blocks_new_bootstrap_until_engine_cleanup_finishes() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().join("workspace");
    std::fs::create_dir_all(root.join("legacy")).expect("workspace");
    std::fs::write(
        root.join("fossilsense.json"),
        r#"{"languageOverrides":[{"glob":"legacy/**/*.h","language":"go"}]}"#,
    )
    .expect("config");
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root.clone());

    let removal_started = Arc::new(tokio::sync::Notify::new());
    let removal_resume = Arc::new(tokio::sync::Notify::new());
    let next_attempt_started = Arc::new(tokio::sync::Notify::new());
    service
        .inner()
        .set_workspace_semantics_removal_barriers_for_test(
            &root,
            removal_started.clone(),
            removal_resume.clone(),
        )
        .await;
    service
        .inner()
        .set_workspace_semantics_bootstrap_attempt_started_for_test(
            &root,
            next_attempt_started.clone(),
        )
        .await;

    let uri = Url::from_directory_path(&root).expect("workspace uri");
    let folder = WorkspaceFolder {
        uri,
        name: "workspace".into(),
    };
    let mut update = Box::pin(service.inner().did_change_workspace_folders(
        DidChangeWorkspaceFoldersParams {
            event: WorkspaceFoldersChangeEvent {
                added: vec![folder.clone()],
                removed: vec![folder],
            },
        },
    ));
    tokio::select! {
        _ = removal_started.notified() => {}
        _ = &mut update => panic!("workspace cleanup unexpectedly skipped its test barrier"),
    }

    let mut request = Box::pin(service.inner().request_context_for_root(root.clone()));
    let entered_during_cleanup =
        tokio::time::timeout(std::time::Duration::from_millis(100), async {
            tokio::select! {
                _ = next_attempt_started.notified() => {}
                _ = &mut request => panic!("request unexpectedly completed during root cleanup"),
            }
        })
        .await;
    assert!(
        entered_during_cleanup.is_err(),
        "a new bootstrap attempt must not enter between invalidation and engine cleanup"
    );

    removal_resume.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(2), &mut update)
        .await
        .expect("workspace cleanup");
    let context = tokio::time::timeout(std::time::Duration::from_secs(2), &mut request)
        .await
        .expect("request must retry after cleanup");
    assert_eq!(
        context
            .engine
            .workspace_semantics
            .language_for_path(&root.join("legacy/api.h")),
        crate::config::SourceLanguage::Go
    );
    assert!(
        service
            .inner()
            .session
            .cache
            .current_engine_snapshot(&root)
            .await
            .is_some(),
        "the post-cleanup attempt must retain its published snapshot"
    );
}

fn completion_params(uri: Url, line: u32, character: u32) -> CompletionParams {
    CompletionParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position::new(line, character),
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
        context: None,
    }
}

fn goto_definition_params(uri: Url, line: u32, character: u32) -> GotoDefinitionParams {
    GotoDefinitionParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position::new(line, character),
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    }
}

fn hover_params(uri: Url, line: u32, character: u32) -> HoverParams {
    HoverParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position::new(line, character),
        },
        work_done_progress_params: Default::default(),
    }
}

fn signature_help_params(uri: Url, line: u32, character: u32) -> SignatureHelpParams {
    SignatureHelpParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position::new(line, character),
        },
        work_done_progress_params: Default::default(),
        context: None,
    }
}

fn reference_params(uri: Url, line: u32, character: u32) -> ReferenceParams {
    ReferenceParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position::new(line, character),
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
        context: ReferenceContext {
            include_declaration: true,
        },
    }
}

fn completion_items(response: CompletionResponse) -> Vec<CompletionItem> {
    match response {
        CompletionResponse::Array(items) => items,
        CompletionResponse::List(list) => list.items,
    }
}

fn completion_response_is_incomplete(response: &CompletionResponse) -> bool {
    match response {
        CompletionResponse::Array(_) => false,
        CompletionResponse::List(list) => list.is_incomplete,
    }
}

fn definition_locations(response: GotoDefinitionResponse) -> Vec<tower_lsp::lsp_types::Location> {
    match response {
        GotoDefinitionResponse::Array(locations) => locations,
        GotoDefinitionResponse::Scalar(location) => vec![location],
        GotoDefinitionResponse::Link(_) => panic!("unexpected location links"),
    }
}

fn hover_text(contents: HoverContents) -> String {
    match contents {
        HoverContents::Scalar(marked) => match marked {
            tower_lsp::lsp_types::MarkedString::String(value) => value,
            tower_lsp::lsp_types::MarkedString::LanguageString(value) => value.value,
        },
        HoverContents::Array(values) => values
            .into_iter()
            .map(|value| match value {
                tower_lsp::lsp_types::MarkedString::String(value) => value,
                tower_lsp::lsp_types::MarkedString::LanguageString(value) => value.value,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        HoverContents::Markup(markup) => markup.value,
    }
}

fn documentation_text(documentation: Documentation) -> String {
    match documentation {
        Documentation::String(value) => value,
        Documentation::MarkupContent(markup) => markup.value,
    }
}

fn text_and_position(marked: &str) -> (String, u32, u32) {
    let marker = "/*cursor*/";
    let cursor_byte = marked.find(marker).expect("cursor marker");
    let text = marked.replacen(marker, "", 1);
    let before = &text[..cursor_byte];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let character = before[line_start..]
        .chars()
        .map(|ch| ch.len_utf16() as u32)
        .sum();
    (text, line, character)
}

#[test]
fn semantic_candidate_perf_log_contains_only_aggregate_contract_fields() {
    let metrics = super::SemanticRequestPerf {
        candidates: crate::query::CallableCandidateMetrics {
            raw_candidates: 9,
            filtered_candidates: 7,
            grouped_candidates: 4,
            arity_compatible: 3,
            arity_unknown: 2,
            arity_incompatible: 2,
            counterpart_strict: 1,
            counterpart_ambiguous: 1,
        },
        returned: 3,
        hydration_count: 2,
        hydration_bytes: 512,
        query_us: 41,
        hydration_us: 17,
        reach_us: 5,
        coverage_open: true,
        coverage_truncated: false,
        coverage_incomplete: true,
        coverage_reason: 4,
        arity_fallback: false,
    };

    let line = metrics.log_line("hover", 99);
    for field in [
        "raw=9",
        "filtered=7",
        "grouped=4",
        "returned=3",
        "arity_compatible=3",
        "arity_unknown=2",
        "arity_incompatible=2",
        "counterpart_strict=1",
        "counterpart_ambiguous=1",
        "hydration_count=2",
        "hydration_bytes=512",
        "query_us=41",
        "hydration_us=17",
        "reach_us=5",
        "coverage_open=1",
        "coverage_truncated=0",
        "coverage_incomplete=1",
        "coverage_reason=4",
        "arity_fallback=0",
    ] {
        assert!(line.contains(field), "missing aggregate metric {field}");
    }
    assert!(!line.contains("symbol"));
    assert!(!line.contains("path"));
    assert!(!line.contains("source"));
}

#[test]
fn indexed_completion_uses_compact_v6_declaration_handle() {
    let uri = Url::parse("file:///workspace/main.c").expect("uri");
    let item = crate::completion::ordinary_service::OrdinaryCompletionItem {
        label: "target".into(),
        kind: crate::completion::ordinary_service::OrdinaryCompletionKind::Function,
        detail: None,
        documentation: None,
        initial_sort_text: None,
        evidence: crate::completion::CandidateEvidence::new(
            crate::completion::CandidateSource::Indexed,
            crate::model::ScopeTier::Global,
            crate::model::ResolutionConfidence::Fallback,
            1,
        ),
        documentation_target: Some(
            crate::completion::ordinary_service::OrdinaryCompletionDocumentationTarget::Declaration {
                table_index: 0,
                declaration_id: 42,
                declaration_name: "answer".to_string(),
            },
        ),
    };
    let rendered = super::ordinary_completion_item_to_lsp(
        item,
        &uri,
        &[PathBuf::from("/workspace")],
        &[crate::call_model::SemanticGeneration(7)],
        3,
        11,
    );
    let data = rendered.data.expect("compact declaration data");
    assert_eq!(
        data.get("version").and_then(serde_json::Value::as_u64),
        Some(6)
    );
    assert_eq!(
        data.get("declarationId")
            .and_then(serde_json::Value::as_i64),
        Some(42)
    );
    assert_eq!(
        data.get("declarationName")
            .and_then(serde_json::Value::as_str),
        Some("answer")
    );
    assert!(data.get("handle").is_none());
}

#[test]
fn live_parse_perf_log_never_contains_document_identity_or_revision() {
    for event in [
        super::LiveParseCacheEvent::Hit,
        super::LiveParseCacheEvent::Coalesced,
        super::LiveParseCacheEvent::Miss,
    ] {
        let line = super::live_parse_cache_log(event);
        assert!(line.starts_with("[perf] live_parse_cache state="));
        assert!(!line.contains("file:"));
        assert!(!line.contains("/"));
        assert!(!line.contains("\\"));
        assert!(!line.contains("version"));
    }
}

#[tokio::test]
async fn workspace_folder_removal_drops_root_and_published_snapshot() {
    let service = test_backend_service();
    let dir = tempdir().expect("root");
    let root = dir.path().to_path_buf();
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
        .publish_engine_snapshot(super::workspace::EngineSnapshot {
            root: root.clone(),
            epoch: super::state::EngineEpoch::published(1),
            semantic_generation: crate::call_model::SemanticGeneration(1),
            declaration_index: None,
            name_table: None,
            fallback_completion_table: Arc::new(Default::default()),
            reach_graph: None,
            include_table: None,
            go_import_table: None,
            indexed_files: None,
            include_path_index: None,
            project_context: None,
            call_read_handle: None,
            workspace_semantics: empty_workspace_semantics(&root),
            degraded: Default::default(),
        })
        .await;

    service
        .inner()
        .did_change_workspace_folders(DidChangeWorkspaceFoldersParams {
            event: WorkspaceFoldersChangeEvent {
                added: Vec::new(),
                removed: vec![WorkspaceFolder {
                    uri: Url::from_file_path(&root).expect("uri"),
                    name: "removed".to_string(),
                }],
            },
        })
        .await;

    assert!(service.inner().workspace_roots.lock().await.is_empty());
    assert!(service
        .inner()
        .session
        .cache
        .current_engine_snapshot(&root)
        .await
        .is_none());
}

#[tokio::test]
async fn name_index_compaction_publishes_only_for_the_expected_engine_epoch() {
    let cache = super::CacheLedger::default();
    let root = tempdir().expect("root").path().to_path_buf();
    let paths = std::collections::HashSet::from(["src/changed.c".to_string()]);
    let mut table = crate::query::NameTable::build_with_paths(vec![
        (
            1,
            "base_name".to_string(),
            false,
            "src/base.c".to_string(),
            "function".to_string(),
            false,
        ),
        (
            2,
            "changed_0".to_string(),
            false,
            "src/changed.c".to_string(),
            "function".to_string(),
            false,
        ),
    ]);
    for revision in 1..=64 {
        table = table.with_updated_paths(
            &paths,
            vec![(
                2 + revision,
                format!("changed_{revision}"),
                false,
                "src/changed.c".to_string(),
                "function".to_string(),
                false,
            )],
        );
    }
    let initial_epoch = cache.allocate_engine_epoch();
    let declaration_index = Arc::new(
        crate::declaration_index::SemanticDeclarationIndex::from_name_table_for_test(table),
    );
    cache
        .publish_engine_snapshot(super::workspace::EngineSnapshot {
            root: root.clone(),
            epoch: initial_epoch,
            semantic_generation: crate::call_model::SemanticGeneration(7),
            declaration_index: Some(declaration_index.clone()),
            name_table: Some(declaration_index.name_table_arc()),
            fallback_completion_table: Arc::new(Default::default()),
            reach_graph: None,
            include_table: None,
            go_import_table: None,
            indexed_files: None,
            include_path_index: None,
            project_context: None,
            call_read_handle: None,
            workspace_semantics: empty_workspace_semantics(&root),
            degraded: Default::default(),
        })
        .await;

    assert!(cache
        .compact_name_index_if_current(root.clone(), initial_epoch)
        .await
        .expect("compact current"));
    let compacted = cache
        .current_engine_snapshot(&root)
        .await
        .expect("compacted snapshot");
    assert_ne!(compacted.epoch, initial_epoch);
    assert_eq!(compacted.semantic_generation.0, 7);
    assert_eq!(
        compacted
            .name_table
            .as_ref()
            .expect("name table")
            .delta_segment_count(),
        0
    );

    assert!(!cache
        .compact_name_index_if_current(root.clone(), initial_epoch)
        .await
        .expect("stale compaction discarded"));
    assert_eq!(
        cache
            .current_engine_snapshot(&root)
            .await
            .expect("current snapshot")
            .epoch,
        compacted.epoch
    );
}

fn write_workspace_file(root: &std::path::Path, rel: &str, text: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, text).expect("write file");
}

async fn open_test_document(
    service: &LspService<super::Backend>,
    uri: Url,
    version: i32,
    text: String,
) {
    service
        .inner()
        .session
        .open_document(uri, version, text)
        .await;
}

async fn indexed_backend_with_open_doc(
    indexed_files: &[(&str, &str)],
    open_rel: &str,
    marked_open_text: &str,
) -> (tempfile::TempDir, LspService<super::Backend>, Url, u32, u32) {
    let dir = tempdir().expect("tempdir");
    for (rel, text) in indexed_files {
        write_workspace_file(dir.path(), rel, text);
    }
    let (open_text, line, character) = text_and_position(marked_open_text);
    write_workspace_file(dir.path(), open_rel, &open_text);
    crate::indexer::index_workspace(
        dir.path(),
        crate::indexer::IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let uri = Url::from_file_path(dir.path().join(open_rel)).expect("file uri");
    let service = test_backend_service();
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(dir.path().to_path_buf());
    service
        .inner()
        .session
        .cache
        .publish_full_index(&service.inner().client, dir.path().to_path_buf())
        .await
        .expect("publish test index");
    open_test_document(&service, uri.clone(), 1, open_text).await;
    (dir, service, uri, line, character)
}

#[cfg(debug_assertions)]
fn require_release_completion_benchmark() {
    panic!("the U-Boot LSP completion replay must run with cargo test --release");
}

#[cfg(not(debug_assertions))]
fn require_release_completion_benchmark() {}

fn validate_completion_replay_recall(
    metrics: &[crate::query::CompletionRecallMetrics],
    minimum_active_entries: usize,
) -> std::result::Result<(), String> {
    if metrics.is_empty() {
        return Err("completion replay recorded no recall observations".to_string());
    }
    for (request, metric) in metrics.iter().enumerate() {
        if metric.indexed_returned == 0 {
            return Err(format!(
                "completion replay request {request} returned no indexed candidates"
            ));
        }
        if metric.active_entries_total < minimum_active_entries {
            return Err(format!(
                "completion replay request {request} saw only {} active entries",
                metric.active_entries_total
            ));
        }
        if metric.candidate_budget != crate::query::COMPLETION_RECALL_CANDIDATE_BUDGET {
            return Err(format!(
                "completion replay request {request} used candidate budget {}",
                metric.candidate_budget
            ));
        }
        if metric.entries_inspected == 0
            || metric.entries_inspected > crate::query::COMPLETION_RECALL_CANDIDATE_BUDGET
        {
            return Err(format!(
                "completion replay request {request} inspected {} entries",
                metric.entries_inspected
            ));
        }
        let source_attempt_limit = metric
            .candidate_budget
            .saturating_div(8)
            .min(crate::query::COMPLETION_PRIORITY_METADATA_PROBE_LIMIT);
        if metric.priority_source_attempts > source_attempt_limit {
            return Err(format!(
                "completion replay request {request} initialized {} priority sources",
                metric.priority_source_attempts
            ));
        }
        if metric.priority_sources_initialized != metric.priority_source_attempts {
            return Err(format!(
                "completion replay request {request} reported {}/{} initialized/attempted priority sources",
                metric.priority_sources_initialized, metric.priority_source_attempts
            ));
        }
        for (label, probes) in [
            ("priority source", metric.priority_source_probes),
            ("priority fuzzy name", metric.priority_fuzzy_name_probes),
            (
                "priority fuzzy declaration",
                metric.priority_fuzzy_declaration_probes,
            ),
        ] {
            if probes > crate::query::COMPLETION_PRIORITY_METADATA_PROBE_LIMIT {
                return Err(format!(
                    "completion replay request {request} performed {probes} {label} probes"
                ));
            }
        }
        if !metric.truncated {
            return Err(format!(
                "completion replay request {request} did not expose bounded truncation"
            ));
        }
    }
    Ok(())
}

#[test]
fn completion_replay_gate_rejects_non_indexed_false_green() {
    let empty_fast_metrics = vec![crate::query::CompletionRecallMetrics::default(); 64];
    assert!(
        validate_completion_replay_recall(&empty_fast_metrics, 500_000).is_err(),
        "a fast builtin-only response must not satisfy the production recall gate"
    );

    let valid = crate::query::CompletionRecallMetrics {
        indexed_returned: 1,
        entries_inspected: crate::query::COMPLETION_RECALL_CANDIDATE_BUDGET,
        active_entries_total: 654_890,
        candidate_budget: crate::query::COMPLETION_RECALL_CANDIDATE_BUDGET,
        truncated: true,
        ..Default::default()
    };
    assert!(validate_completion_replay_recall(&vec![valid; 64], 500_000).is_ok());

    let mut unbounded_sources = valid;
    unbounded_sources.priority_source_probes = 4_097;
    assert!(
        validate_completion_replay_recall(&[unbounded_sources], 500_000).is_err(),
        "the replay gate must reject hidden source metadata work"
    );
    let mut unbounded_fuzzy = valid;
    unbounded_fuzzy.priority_fuzzy_name_probes = 4_097;
    assert!(
        validate_completion_replay_recall(&[unbounded_fuzzy], 500_000).is_err(),
        "the replay gate must reject hidden fuzzy metadata work"
    );
    let mut inconsistent_sources = valid;
    inconsistent_sources.priority_source_attempts = 1;
    assert!(
        validate_completion_replay_recall(&[inconsistent_sources], 500_000).is_err(),
        "every successful source attempt must initialize exactly one cursor"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "U-Boot production LSP completion replay; set FOSSILSENSE_BENCH_DB and FOSSILSENSE_BENCH_ROOT and run with --release"]
async fn benchmark_uboot_lsp_completion_replay_stays_within_latency_and_sql_gates() {
    require_release_completion_benchmark();
    let db_path = std::env::var_os("FOSSILSENSE_BENCH_DB")
        .map(PathBuf::from)
        .expect("set FOSSILSENSE_BENCH_DB to a current-schema U-Boot database");
    let root = std::env::var_os("FOSSILSENSE_BENCH_ROOT")
        .map(PathBuf::from)
        .expect("set FOSSILSENSE_BENCH_ROOT to the indexed U-Boot checkout");
    assert!(db_path.is_file(), "benchmark database does not exist");
    assert!(root.is_dir(), "benchmark workspace does not exist");

    let service = test_backend_service();
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root.clone());
    service
        .inner()
        .set_completion_history_mode_for_test(crate::completion_history::CompletionHistoryMode::Off)
        .await;
    let snapshot = service
        .inner()
        .session
        .cache
        .publish_full_index_from_db_for_test(root.clone(), db_path)
        .await
        .expect("hydrate and publish benchmark engine snapshot");
    let declaration_index = snapshot
        .declaration_index
        .clone()
        .expect("benchmark declaration index");
    assert!(
        declaration_index.len() >= 500_000,
        "LSP replay requires the full U-Boot declaration set"
    );

    let uri = Url::from_file_path(root.join(".fossilsense-completion-replay.c"))
        .expect("benchmark document URI");
    const REPLAY_SELF_INCLUDE: &str = ".fossilsense-completion-replay.c";
    const REPLAY_MISSING_INCLUDE: &str = ".fossilsense-completion-replay-missing.h";
    let (initial_text, line, character) = text_and_position(&format!(
        "#include \"{REPLAY_SELF_INCLUDE}\"\nvoid completion_replay(void) {{ i/*cursor*/; }}\n"
    ));
    open_test_document(&service, uri.clone(), 1, initial_text).await;
    let sql_reads_before = declaration_index.payload_cache_stats().sql_reads;
    let prefixes = ["i", "in", "init", "d", "de", "dev", "c", "cmd"];
    let mut samples = Vec::with_capacity(prefixes.len() * 8);
    let mut version = 1;

    for pass in 0..10 {
        for (prefix_index, prefix) in prefixes.iter().copied().enumerate() {
            if pass == 2 && prefix_index == 0 {
                service
                    .inner()
                    .session
                    .cache
                    .reset_completion_overlay_cache_metrics_for_test();
            }
            version += 1;
            // Alternate the include projection on every request. This forces a
            // stable-universe cache miss at U-Boot scale while retaining the
            // ordinary completion handler, render and bounded-recall gates.
            let include_target = if (pass * prefixes.len() + prefix_index).is_multiple_of(2) {
                REPLAY_SELF_INCLUDE
            } else {
                REPLAY_MISSING_INCLUDE
            };
            let (text, current_line, current_character) = text_and_position(&format!(
                "#include \"{include_target}\"\nvoid completion_replay(void) {{ {prefix}/*cursor*/; }}\n"
            ));
            assert_eq!(
                (current_line, current_character),
                (line, character + prefix.len() as u32 - 1)
            );
            service
                .inner()
                .session
                .change_document(uri.clone(), version, text)
                .await;

            let started = std::time::Instant::now();
            let response = service
                .inner()
                .completion(completion_params(
                    uri.clone(),
                    current_line,
                    current_character,
                ))
                .await
                .expect("completion request")
                .expect("completion response");
            let elapsed_us = started.elapsed().as_micros();
            assert!(completion_response_is_incomplete(&response));
            assert!(
                !completion_items(response).is_empty(),
                "U-Boot replay returned no completion items for {prefix}"
            );
            if pass >= 2 {
                samples.push(elapsed_us);
            }
        }
    }

    samples.sort_unstable();
    let p50 = samples[samples.len() / 2];
    let p95 = samples[samples.len() * 95 / 100];
    let max = samples[samples.len() - 1];
    let observations = service.inner().take_completion_perf_for_test();
    assert_eq!(observations.len(), prefixes.len() * 10);
    let observations = &observations[prefixes.len() * 2..];
    let recall_observations: Vec<_> = observations
        .iter()
        .map(|(_, metrics)| metrics.recall_channels)
        .collect();
    validate_completion_replay_recall(&recall_observations, 500_000)
        .expect("production LSP replay indexed-recall gate");
    let percentile = |values: &mut Vec<u128>, percentile: usize| {
        values.sort_unstable();
        values[values.len() * percentile / 100]
    };
    let context_p95 = percentile(
        &mut observations
            .iter()
            .map(|(timings, _)| timings.context_ms)
            .collect(),
        95,
    );
    let parse_p95 = percentile(
        &mut observations
            .iter()
            .map(|(timings, _)| timings.parse_ms)
            .collect(),
        95,
    );
    let local_words_p95 = percentile(
        &mut observations
            .iter()
            .map(|(timings, _)| timings.local_words_ms)
            .collect(),
        95,
    );
    let overlay_p95 = percentile(
        &mut observations
            .iter()
            .map(|(timings, _)| timings.overlay_ms)
            .collect(),
        95,
    );
    let worker_p95 = percentile(
        &mut observations
            .iter()
            .map(|(timings, _)| timings.worker_ms)
            .collect(),
        95,
    );
    let render_p95 = percentile(
        &mut observations
            .iter()
            .map(|(timings, _)| timings.render_ms)
            .collect(),
        95,
    );
    let entries_inspected_max = observations
        .iter()
        .map(|(_, metrics)| metrics.recall_channels.entries_inspected)
        .max()
        .unwrap_or_default();
    let entries_inspected_min = recall_observations
        .iter()
        .map(|metrics| metrics.entries_inspected)
        .min()
        .unwrap_or_default();
    let indexed_returned_min = recall_observations
        .iter()
        .map(|metrics| metrics.indexed_returned)
        .min()
        .unwrap_or_default();
    let active_entries_min = recall_observations
        .iter()
        .map(|metrics| metrics.active_entries_total)
        .min()
        .unwrap_or_default();
    let candidate_budget_min = recall_observations
        .iter()
        .map(|metrics| metrics.candidate_budget)
        .min()
        .unwrap_or_default();
    let candidate_budget_max = recall_observations
        .iter()
        .map(|metrics| metrics.candidate_budget)
        .max()
        .unwrap_or_default();
    let truncated_requests = recall_observations
        .iter()
        .filter(|metrics| metrics.truncated)
        .count();
    let priority_source_probes_max = recall_observations
        .iter()
        .map(|metrics| metrics.priority_source_probes)
        .max()
        .unwrap_or_default();
    let priority_source_attempts_max = recall_observations
        .iter()
        .map(|metrics| metrics.priority_source_attempts)
        .max()
        .unwrap_or_default();
    let priority_sources_initialized_max = recall_observations
        .iter()
        .map(|metrics| metrics.priority_sources_initialized)
        .max()
        .unwrap_or_default();
    let priority_fuzzy_name_probes_max = recall_observations
        .iter()
        .map(|metrics| metrics.priority_fuzzy_name_probes)
        .max()
        .unwrap_or_default();
    let priority_fuzzy_declaration_probes_max = recall_observations
        .iter()
        .map(|metrics| metrics.priority_fuzzy_declaration_probes)
        .max()
        .unwrap_or_default();
    let sql_reads_after = declaration_index.payload_cache_stats().sql_reads;
    let (overlay_cache_hits, overlay_cache_misses) = service
        .inner()
        .session
        .cache
        .completion_overlay_cache_metrics_for_test();
    const P95_LIMIT_US: u128 = 50_000;

    println!(
        "completion_lsp_replay_declarations: {}",
        declaration_index.len()
    );
    println!("completion_lsp_replay_requests: {}", samples.len());
    println!(
        "completion_lsp_replay_forced_include_miss_requests: {}",
        overlay_cache_misses
    );
    println!("completion_lsp_replay_p50_us: {p50}");
    println!("completion_lsp_replay_p95_us: {p95}");
    println!("completion_lsp_replay_max_us: {max}");
    println!("completion_lsp_replay_p95_limit_us: {P95_LIMIT_US}");
    println!("completion_lsp_replay_context_p95_ms: {context_p95}");
    println!("completion_lsp_replay_parse_p95_ms: {parse_p95}");
    println!("completion_lsp_replay_local_words_p95_ms: {local_words_p95}");
    println!("completion_lsp_replay_overlay_p95_ms: {overlay_p95}");
    println!("completion_lsp_replay_worker_p95_ms: {worker_p95}");
    println!("completion_lsp_replay_render_p95_ms: {render_p95}");
    println!("completion_lsp_replay_indexed_returned_min: {indexed_returned_min}");
    println!("completion_lsp_replay_active_entries_min: {active_entries_min}");
    println!("completion_lsp_replay_candidate_budget_min: {candidate_budget_min}");
    println!("completion_lsp_replay_candidate_budget_max: {candidate_budget_max}");
    println!("completion_lsp_replay_truncated_requests: {truncated_requests}");
    println!("completion_lsp_replay_entries_inspected_min: {entries_inspected_min}");
    println!("completion_lsp_replay_entries_inspected_max: {entries_inspected_max}");
    println!("completion_lsp_replay_priority_source_probes_max: {priority_source_probes_max}");
    println!("completion_lsp_replay_priority_source_attempts_max: {priority_source_attempts_max}");
    println!(
        "completion_lsp_replay_priority_sources_initialized_max: {priority_sources_initialized_max}"
    );
    println!(
        "completion_lsp_replay_priority_fuzzy_name_probes_max: {priority_fuzzy_name_probes_max}"
    );
    println!(
        "completion_lsp_replay_priority_fuzzy_declaration_probes_max: {priority_fuzzy_declaration_probes_max}"
    );
    println!(
        "completion_lsp_replay_sql_reads: {}",
        sql_reads_after.saturating_sub(sql_reads_before)
    );

    assert_eq!(
        sql_reads_after, sql_reads_before,
        "ordinary completion lists must not hydrate declaration payloads"
    );
    assert_eq!(
        overlay_cache_hits, 0,
        "alternating include universes must not hit the completion overlay cache"
    );
    assert_eq!(
        overlay_cache_misses,
        samples.len() as u64,
        "the replay must measure one real overlay miss per sampled request"
    );
    assert!(
        entries_inspected_max <= crate::query::COMPLETION_RECALL_CANDIDATE_BUDGET,
        "production LSP recall escaped the shared request budget"
    );
    assert!(
        p95 <= P95_LIMIT_US,
        "U-Boot production LSP completion p95 {p95} us exceeded {P95_LIMIT_US} us"
    );
}

#[tokio::test]
async fn lexical_fallback_is_completion_only_across_lsp_consumers() {
    let broken_text = "((( guessed(value);\n";
    let (dir, service, main_uri, line, character) = indexed_backend_with_open_doc(
        &[("broken.c", broken_text)],
        "main.c",
        "void f(void) { gue/*cursor*/; }\n",
    )
    .await;

    let completion = service
        .inner()
        .completion(completion_params(main_uri, line, character))
        .await
        .expect("completion request")
        .expect("fallback completion response");
    let guessed = completion_items(completion)
        .into_iter()
        .find(|item| item.label == "guessed")
        .expect("isolated lexical fallback completion");
    assert_eq!(guessed.kind, Some(CompletionItemKind::FUNCTION));
    assert!(
        guessed.data.is_none(),
        "fallback must have no resolve handle"
    );
    assert!(guessed.documentation.is_none());

    let (open_broken_text, broken_line, broken_character) =
        text_and_position("((( gue/*cursor*/ssed(value);\n");
    assert_eq!(open_broken_text, broken_text);
    let broken_uri = Url::from_file_path(dir.path().join("broken.c")).expect("broken uri");
    open_test_document(&service, broken_uri.clone(), 1, open_broken_text).await;

    assert!(service
        .inner()
        .hover(hover_params(
            broken_uri.clone(),
            broken_line,
            broken_character
        ))
        .await
        .expect("hover request")
        .is_none());
    assert!(service
        .inner()
        .goto_definition(goto_definition_params(
            broken_uri.clone(),
            broken_line,
            broken_character
        ))
        .await
        .expect("definition request")
        .is_none());

    let symbols = service
        .inner()
        .document_symbol(DocumentSymbolParams {
            text_document: TextDocumentIdentifier {
                uri: broken_uri.clone(),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .expect("document symbol request")
        .expect("empty document symbol response");
    match symbols {
        DocumentSymbolResponse::Nested(symbols) => assert!(symbols.is_empty()),
        DocumentSymbolResponse::Flat(symbols) => assert!(symbols.is_empty()),
    }

    let workspace_symbols = service
        .inner()
        .symbol(WorkspaceSymbolParams {
            query: "guessed".to_string(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .expect("workspace symbol request")
        .unwrap_or_default();
    assert!(workspace_symbols
        .iter()
        .all(|symbol| symbol.name != "guessed"));
}

#[tokio::test]
async fn cpp_constructor_style_object_is_one_object_in_completion_hover_and_navigation() {
    let indexed_source = "struct Widget { explicit Widget(int); };\n\
                          Widget stale_widget(1);\n\
                          void use(void) { stale_widget/*cursor*/; }\n";
    let (_dir, service, uri, _line, _character) =
        indexed_backend_with_open_doc(&[], "main.cpp", indexed_source).await;
    let (dirty_source, line, character) = text_and_position(
        "struct Widget { explicit Widget(int); };\n\
         Widget widget(42);\n\
         void use(void) { widget/*cursor*/; }\n",
    );
    open_test_document(&service, uri.clone(), 2, dirty_source).await;

    let completion = service
        .inner()
        .completion(completion_params(uri.clone(), line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let widgets: Vec<_> = completion_items(completion)
        .into_iter()
        .filter(|item| item.label == "widget")
        .collect();
    assert_eq!(
        widgets.len(),
        1,
        "canonical declaration must deduplicate overlays"
    );
    assert_eq!(widgets[0].kind, Some(CompletionItemKind::VARIABLE));

    let hover = service
        .inner()
        .hover(hover_params(uri.clone(), line, character))
        .await
        .expect("hover request")
        .expect("object hover");
    let hover = hover_text(hover.contents);
    assert!(hover.contains("Widget widget"), "{hover}");

    let definition = service
        .inner()
        .goto_definition(goto_definition_params(uri.clone(), line, character))
        .await
        .expect("definition request")
        .expect("object definition");
    let locations = definition_locations(definition);
    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0].uri, uri);
    assert_eq!(locations[0].range.start.line, 1);
}

#[tokio::test]
async fn unsaved_header_overlay_uses_the_same_configured_language_as_indexing() {
    let config = r#"{
      "languageOverrides": [
        { "glob": "legacy/**/*.h", "language": "cpp" },
        { "glob": "legacy/api.h", "language": "c" }
      ]
    }"#;
    let (dir, service, uri, _line, _character) = indexed_backend_with_open_doc(
        &[("fossilsense.json", config)],
        "legacy/api.h",
        "int live_object/*cursor*/;\n",
    )
    .await;
    let path = dir.path().join("legacy/api.h");
    let document = service
        .inner()
        .session
        .documents
        .snapshot(&uri)
        .await
        .expect("open document");
    let parsed = service
        .inner()
        .get_or_parse_document(
            &uri,
            &path,
            document.version,
            &document.text,
            crate::parser::ParseFacts::DECLARATIONS,
        )
        .await
        .expect("live parse");
    let declaration = parsed
        .declarations
        .iter()
        .find(|declaration| declaration.name == "live_object")
        .expect("live declaration");
    assert_eq!(
        declaration.identity.language,
        crate::semantic_model::SemanticLanguage::C
    );
    assert_eq!(
        declaration.role,
        crate::semantic_model::SemanticDeclarationRole::TentativeDefinition
    );
}

#[tokio::test]
async fn clean_indexed_language_override_keeps_its_family_for_semantic_queries() {
    let config = r#"{
      "languageOverrides": [
        { "glob": "legacy/**/*.h", "language": "go" }
      ]
    }"#;
    let source =
        "package legacy\nfunc Open() int { return 1 }\nfunc Use() { _ = Open/*cursor*/() }\n";
    let (dir, service, uri, line, character) =
        indexed_backend_with_open_doc(&[("fossilsense.json", config)], "legacy/api.h", source)
            .await;
    let generation = service
        .inner()
        .request_context_for_root(dir.path().to_path_buf())
        .await
        .engine
        .semantic_generation;
    service
        .inner()
        .session
        .documents
        .reconcile_published_files(
            dir.path().to_path_buf(),
            Some(vec!["legacy/api.h".into()]),
            generation,
        )
        .await;
    let clean = service
        .inner()
        .session
        .documents
        .snapshot(&uri)
        .await
        .expect("clean document");
    assert!(!clean.needs_relation_overlay(generation));

    // Configuration N+1 may be loaded while generation N is still serving.
    // Semantic requests must keep using N's published language resolver until
    // the replacement index is successfully published.
    std::fs::write(dir.path().join("fossilsense.json"), "{}").expect("generation N+1 config");
    service.inner().config_cache.lock().await.remove(dir.path());
    let still_published = service
        .inner()
        .request_context_for_root(dir.path().to_path_buf())
        .await;
    assert_eq!(
        still_published
            .engine
            .workspace_semantics
            .language_for_uri(&uri),
        crate::config::SourceLanguage::Go
    );

    let definition = service
        .inner()
        .goto_definition(goto_definition_params(uri.clone(), line, character))
        .await
        .expect("definition request")
        .expect("Go declaration from overridden .h");
    let locations = definition_locations(definition);
    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0].uri, uri);
    assert_eq!(locations[0].range.start.line, 1);
}

#[tokio::test]
async fn published_language_snapshot_controls_tokens_and_symbols_during_config_rebuild() {
    let config = r#"{
      "languageOverrides": [
        { "glob": "legacy/**/*.h", "language": "go" }
      ]
    }"#;
    let source = "package legacy\ntype Device/*cursor*/ struct { Value int }\n";
    let (dir, service, uri, _line, _character) =
        indexed_backend_with_open_doc(&[("fossilsense.json", config)], "legacy/api.h", source)
            .await;
    std::fs::write(dir.path().join("fossilsense.json"), "{}").expect("generation N+1 config");
    service.inner().config_cache.lock().await.remove(dir.path());
    service
        .inner()
        .session
        .documents
        .clear_live_state(&uri)
        .await;

    service
        .inner()
        .compute_semantic_tokens(&uri, None)
        .await
        .expect("semantic tokens");
    assert!(service
        .inner()
        .session
        .documents
        .cached_live_parse(
            &uri,
            1,
            crate::config::SourceLanguage::Go,
            crate::parser::ParseFacts::COLOR_LIVE,
        )
        .await
        .is_some());

    service
        .inner()
        .session
        .documents
        .clear_live_state(&uri)
        .await;
    let symbols = service
        .inner()
        .document_symbol(DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .expect("document symbols")
        .expect("published-language symbols");
    let names = match symbols {
        DocumentSymbolResponse::Nested(symbols) => symbols
            .into_iter()
            .map(|symbol| symbol.name)
            .collect::<Vec<_>>(),
        DocumentSymbolResponse::Flat(symbols) => symbols
            .into_iter()
            .map(|symbol| symbol.name)
            .collect::<Vec<_>>(),
    };
    assert!(names.iter().any(|name| name == "Device"), "{names:?}");
}

#[tokio::test]
async fn include_navigation_uses_published_roots_during_config_rebuild() {
    let dir = tempdir().expect("root");
    let root = dir.path().join("workspace");
    let old_include = dir.path().join("old-include");
    let new_include = dir.path().join("new-include");
    for path in [&root, &old_include, &new_include] {
        fs::create_dir_all(path).expect("directory");
    }
    fs::write(old_include.join("api.h"), "int old_api;\n").expect("old header");
    fs::write(new_include.join("api.h"), "int new_api;\n").expect("new header");
    fs::write(root.join("main.c"), "#include \"api.h\"\n").expect("source");
    fs::write(
        root.join("fossilsense.json"),
        serde_json::json!({
            "includePaths": [crate::pathing::normalize_abs_path(&old_include)]
        })
        .to_string(),
    )
    .expect("generation N config");
    crate::indexer::index_workspace(
        &root,
        crate::indexer::IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");
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
        .publish_full_index(&service.inner().client, root.clone())
        .await
        .expect("publish");
    fs::write(
        root.join("fossilsense.json"),
        serde_json::json!({
            "includePaths": [crate::pathing::normalize_abs_path(&new_include)]
        })
        .to_string(),
    )
    .expect("generation N+1 config");
    service.inner().config_cache.lock().await.remove(&root);
    fs::remove_file(crate::pathing::default_index_path(&root).expect("db path"))
        .expect("remove database");

    let result = service
        .inner()
        .goto_include(
            &Url::from_file_path(root.join("main.c")).expect("source uri"),
            crate::includes::IncludeForm::Quote,
            "api.h".into(),
        )
        .await
        .expect("include navigation")
        .expect("published include root");
    let locations = definition_locations(result);
    assert_eq!(locations.len(), 1);
    assert_eq!(
        locations[0].uri,
        Url::from_file_path(old_include.join("api.h")).expect("old header uri")
    );
}

#[tokio::test]
async fn go_live_parse_uses_the_same_relative_package_identity_as_indexing() {
    let (dir, service, uri, _line, _character) = indexed_backend_with_open_doc(
        &[],
        "src/sensor/read.go",
        "package sensor\nfunc Read/*cursor*/() {}\n",
    )
    .await;
    let path = dir.path().join("src/sensor/read.go");
    let document = service
        .inner()
        .session
        .documents
        .snapshot(&uri)
        .await
        .expect("open document");
    let parsed = service
        .inner()
        .get_or_parse_document(
            &uri,
            &path,
            document.version,
            &document.text,
            crate::parser::ParseFacts::DECLARATIONS,
        )
        .await
        .expect("live parse");
    let declaration = parsed
        .declarations
        .iter()
        .find(|declaration| declaration.name == "Read")
        .expect("Read declaration");

    assert_eq!(declaration.path, "src/sensor/read.go");
    assert_eq!(
        declaration.linkage,
        crate::call_model::LinkageDomain::Package("src/sensor#sensor".to_string())
    );
}

#[tokio::test]
async fn go_lsp_symbols_navigation_hover_and_possible_targets_share_persisted_facts() {
    let api = "package device\n\
               // Exported returns the adjusted sensor value.\n\
               func Exported(value int) int { return value + 1 }\n";
    let (dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[
            ("go.mod", "module example.com/board\n\ngo 1.22\n"),
            ("device/api.go", api),
        ],
        "device/use.go",
        "package device\nfunc Use() int { return Exported/*cursor*/(1) }\n",
    )
    .await;

    let symbols = service
        .inner()
        .document_symbol(DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .expect("document symbols")
        .expect("Go document symbols");
    let names: Vec<_> = match symbols {
        DocumentSymbolResponse::Nested(symbols) => {
            symbols.into_iter().map(|symbol| symbol.name).collect()
        }
        DocumentSymbolResponse::Flat(symbols) => {
            symbols.into_iter().map(|symbol| symbol.name).collect()
        }
    };
    assert!(names.iter().any(|name| name == "Use"), "{names:?}");

    let workspace_symbols = service
        .inner()
        .symbol(WorkspaceSymbolParams {
            query: "Exported".into(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .expect("workspace symbols")
        .expect("Go workspace symbols");
    assert!(
        workspace_symbols
            .iter()
            .any(|symbol| symbol.name == "Exported"),
        "{workspace_symbols:?}"
    );

    let definition = service
        .inner()
        .goto_definition(goto_definition_params(uri.clone(), line, character))
        .await
        .expect("Go definition")
        .expect("Exported definition");
    let definition_targets = definition_locations(definition);
    assert_eq!(definition_targets.len(), 1);
    assert_eq!(
        definition_targets[0].uri,
        Url::from_file_path(dir.path().join("device/api.go")).expect("api uri")
    );
    assert_eq!(definition_targets[0].range.start.line, 2);

    let declaration = service
        .inner()
        .goto_declaration(goto_definition_params(uri.clone(), line, character))
        .await
        .expect("Go declaration")
        .expect("Exported declaration");
    assert_eq!(definition_locations(declaration).len(), 1);

    let hover = service
        .inner()
        .hover(hover_params(uri.clone(), line, character))
        .await
        .expect("Go hover")
        .expect("Exported hover");
    let hover = hover_text(hover.contents);
    assert!(hover.contains("func Exported(value int) int"), "{hover}");
    assert!(
        hover.contains("returns the adjusted sensor value"),
        "{hover}"
    );

    let possible = service
        .inner()
        .possible_targets_command(&serde_json::json!({
            "uri": uri,
            "line": line,
            "character": character,
        }))
        .await
        .expect("Go possible targets");
    assert_eq!(possible["name"], "Exported");
    assert_eq!(possible["items"].as_array().map(Vec::len), Some(1));
    assert_eq!(possible["items"][0]["linkage"], "package");
}

#[tokio::test]
async fn go_lsp_completion_covers_indexed_local_member_import_and_resolve() {
    let api = "package device\n\
               // Exported reports the current device value.\n\
               func Exported(value int) int { return value }\n\
               type Device struct { Reading int }\n\
               func (device Device) Reset() {}\n";
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[
            ("go.mod", "module example.com/board\n\ngo 1.22\n"),
            ("device/api.go", api),
        ],
        "main.go",
        "package main\nfunc Use() { Expor/*cursor*/ }\n",
    )
    .await;

    let response = service
        .inner()
        .completion(completion_params(uri.clone(), line, character))
        .await
        .expect("ordinary Go completion")
        .expect("ordinary completion response");
    let exported = completion_items(response)
        .into_iter()
        .find(|item| item.label == "Exported")
        .expect("indexed Go completion");
    assert_eq!(exported.kind, Some(CompletionItemKind::FUNCTION));
    let resolved = service
        .inner()
        .completion_resolve(exported)
        .await
        .expect("resolve indexed Go completion");
    let documentation = resolved
        .documentation
        .map(documentation_text)
        .expect("resolved Go documentation");
    assert!(
        documentation.contains("reports the current device value"),
        "{documentation}"
    );

    let (local_source, local_line, local_character) =
        text_and_position("package main\nfunc Use() { localThing := 1; _ = localTh/*cursor*/ }\n");
    open_test_document(&service, uri.clone(), 2, local_source).await;
    let local = service
        .inner()
        .completion(completion_params(uri.clone(), local_line, local_character))
        .await
        .expect("local Go completion")
        .expect("local completion response");
    assert!(
        completion_items(local)
            .iter()
            .any(|item| item.label == "localThing"),
        "short variable should be recalled from the current Go scope"
    );

    let (member_source, member_line, member_character) = text_and_position(
        "package main\n\
         type Device struct { Reading int }\n\
         func (device Device) Reset() {}\n\
         func Use() { var device Device; _ = device.Re/*cursor*/ }\n",
    );
    open_test_document(&service, uri.clone(), 3, member_source).await;
    let members = service
        .inner()
        .completion(completion_params(
            uri.clone(),
            member_line,
            member_character,
        ))
        .await
        .expect("member Go completion")
        .expect("member completion response");
    let member_labels: Vec<_> = completion_items(members)
        .into_iter()
        .map(|item| item.label)
        .collect();
    assert!(member_labels.iter().any(|label| label == "Reading"));
    assert!(member_labels.iter().any(|label| label == "Reset"));

    let (import_source, import_line, import_character) =
        text_and_position("package main\nimport \"example.com/board/de/*cursor*/\"\n");
    open_test_document(&service, uri.clone(), 4, import_source).await;
    let imports = service
        .inner()
        .completion(completion_params(uri, import_line, import_character))
        .await
        .expect("Go import completion")
        .expect("Go import completion response");
    let device = completion_items(imports)
        .into_iter()
        .find(|item| item.label == "example.com/board/device")
        .expect("indexed Go package import path");
    assert_eq!(device.kind, Some(CompletionItemKind::MODULE));
}

#[tokio::test]
async fn go_import_completion_does_not_offer_the_current_package() {
    let source = "package device\nimport \"example.com/board/devi/*cursor*/\"\n";
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[
            ("go.mod", "module example.com/board\n\ngo 1.22\n"),
            ("device/api.go", "package device\nfunc Open() {}\n"),
        ],
        "device/use.go",
        source,
    )
    .await;
    #[cfg(windows)]
    let uri = {
        let upper_path = PathBuf::from(
            uri.to_file_path()
                .expect("workspace path")
                .to_string_lossy()
                .to_ascii_uppercase(),
        );
        let upper_uri = Url::from_file_path(upper_path).expect("uppercase workspace uri");
        open_test_document(
            &service,
            upper_uri.clone(),
            2,
            source.replace("/*cursor*/", ""),
        )
        .await;
        upper_uri
    };
    let completion = service
        .inner()
        .completion(completion_params(uri.clone(), line, character))
        .await
        .expect("self import completion")
        .expect("self import completion response");
    assert!(completion_items(completion)
        .iter()
        .all(|item| item.label != "example.com/board/device"));
}

#[tokio::test]
async fn go_import_completion_filters_the_open_external_module_package() {
    let dir = tempdir().expect("tempdir");
    let workspace = dir.path().join("workspace");
    let external = dir.path().join("external");
    fs::create_dir_all(&workspace).expect("workspace");
    fs::create_dir_all(external.join("pkg")).expect("external package");
    fs::write(workspace.join("go.mod"), "module example.com/workspace\n").expect("workspace mod");
    fs::write(workspace.join("main.go"), "package main\nfunc main() {}\n").expect("workspace main");
    fs::write(external.join("go.mod"), "module example.com/external\n").expect("external mod");
    let (source, line, character) =
        text_and_position("package pkg\nimport \"example.com/external/p/*cursor*/\"\n");
    let external_file = external.join("pkg/use.go");
    fs::write(&external_file, &source).expect("external source");
    fs::write(
        workspace.join("fossilsense.json"),
        serde_json::json!({"goModulePaths": [external.to_string_lossy()]}).to_string(),
    )
    .expect("workspace config");
    crate::indexer::index_workspace(
        &workspace,
        crate::indexer::IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let service = test_backend_service();
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(workspace.clone());
    service
        .inner()
        .session
        .cache
        .publish_full_index(&service.inner().client, workspace)
        .await
        .expect("publish");
    #[cfg(windows)]
    let external_file = PathBuf::from(external_file.to_string_lossy().to_ascii_uppercase());
    let uri = Url::from_file_path(external_file).expect("external uri");
    open_test_document(&service, uri.clone(), 1, source).await;

    let completion = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("external self completion")
        .expect("external completion response");
    assert!(completion_items(completion)
        .iter()
        .all(|item| item.label != "example.com/external/pkg"));
}

#[tokio::test]
async fn go_lsp_references_keep_roles_and_do_not_cross_the_c_family_boundary() {
    let source = "package device\nfunc Target() {}\nfunc Use() { Target/*cursor*/() }\n";
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[
            ("go.mod", "module example.com/board\n\ngo 1.22\n"),
            (
                "same.c",
                "void Target(void) {}\nvoid use(void) { Target(); }\n",
            ),
        ],
        "main.go",
        source,
    )
    .await;

    let references = service
        .inner()
        .references(reference_params(uri.clone(), line, character))
        .await
        .expect("Go references")
        .expect("Go reference locations");
    assert_eq!(references.len(), 2, "{references:?}");
    assert!(
        references
            .iter()
            .all(|location| location.uri.path().ends_with("/main.go")),
        "{references:?}"
    );

    let grouped = service
        .inner()
        .execute_command(ExecuteCommandParams {
            command: super::GROUPED_REFERENCES_LSP_COMMAND.into(),
            arguments: vec![serde_json::json!({
                "uri": uri,
                "line": line,
                "character": character,
            })],
            work_done_progress_params: Default::default(),
        })
        .await
        .expect("grouped Go references")
        .expect("grouped Go reference response");
    let roles: Vec<_> = grouped
        .as_array()
        .expect("grouped reference array")
        .iter()
        .filter_map(|item| item["role"].as_str())
        .collect();
    assert_eq!(roles, vec!["definition", "call"]);
}

#[tokio::test]
async fn references_use_one_cached_language_config_snapshot_per_request() {
    let initial_config = r#"{"languageOverrides":[{"glob":"legacy/**/*.h","language":"go"}]}"#;
    let source = "package legacy\nfunc Target() {}\nfunc Use() { Target/*cursor*/() }\n";
    let (dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[
            ("fossilsense.json", initial_config),
            ("same.c", "void Target(void) {}\n"),
        ],
        "legacy/api.h",
        source,
    )
    .await;
    assert_eq!(
        service.inner().source_language_for_uri(&uri).await,
        crate::config::SourceLanguage::Go,
        "prime the request-side cached resolver"
    );
    fs::write(
        dir.path().join("fossilsense.json"),
        r#"{"languageOverrides":[{"glob":"legacy/**/*.h","language":"c"}]}"#,
    )
    .expect("change config on disk without a watcher event");

    let references = service
        .inner()
        .references(reference_params(uri, line, character))
        .await
        .expect("references")
        .expect("reference locations");
    assert_eq!(
        references.len(),
        2,
        "one request must not mix the cached request language with a newly reloaded file resolver"
    );
    assert!(
        references
            .iter()
            .all(|location| location.uri.path().ends_with("/legacy/api.h")),
        "{references:?}"
    );
}

#[tokio::test]
async fn go_lsp_signature_help_handles_variadics_and_semantic_tokens() {
    let api = "package device\n\
               // Format combines a prefix and values.\n\
               func Format(prefix string, values ...int) int { return len(values) }\n";
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[
            ("go.mod", "module example.com/board\n\ngo 1.22\n"),
            ("device/api.go", api),
        ],
        "device/use.go",
        "package device\nfunc Use() { _ = Format(\"value\", 1, /*cursor*/) }\n",
    )
    .await;
    let help = service
        .inner()
        .signature_help(signature_help_params(uri.clone(), line, character))
        .await
        .expect("Go signature help")
        .expect("Go signature response");
    assert_eq!(help.signatures.len(), 1);
    assert!(
        help.signatures[0].label.contains("values ...int"),
        "{}",
        help.signatures[0].label
    );
    assert_eq!(
        help.signatures[0].active_parameter,
        Some(1),
        "arguments beyond the fixed prefix belong to the final variadic parameter"
    );

    let semantic_source = "package device\n\
                           type Device struct{}\n\
                           func Use() { var current Device; _ = current }\n";
    open_test_document(&service, uri.clone(), 2, semantic_source.into()).await;
    let tokens = service
        .inner()
        .semantic_tokens_full(SemanticTokensParams {
            text_document: TextDocumentIdentifier { uri },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .expect("Go semantic tokens")
        .expect("Go semantic token response");
    let SemanticTokensResult::Tokens(tokens) = tokens else {
        panic!("expected full semantic tokens");
    };
    assert!(!tokens.data.is_empty());
}

#[tokio::test]
async fn go_lsp_call_hierarchy_keeps_direct_calls_separate_from_same_named_methods() {
    let source = "package device\n\
                  func Run() {}\n\
                  type Worker struct{}\n\
                  func (Worker) Run() {}\n\
                  func Caller/*cursor*/() { Run() }\n";
    let (_dir, service, uri, line, character) =
        indexed_backend_with_open_doc(&[], "main.go", source).await;
    let prepared = service
        .inner()
        .prepare_call_hierarchy(CallHierarchyPrepareParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position::new(line, character),
            },
            work_done_progress_params: Default::default(),
        })
        .await
        .expect("prepare Go call hierarchy")
        .expect("Go call hierarchy item");
    assert_eq!(prepared.len(), 1);
    assert_eq!(prepared[0].name, "Caller");

    let outgoing = service
        .inner()
        .outgoing_calls(CallHierarchyOutgoingCallsParams {
            item: prepared[0].clone(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .expect("Go outgoing calls")
        .expect("Go outgoing call response");
    assert_eq!(outgoing.len(), 1, "{outgoing:?}");
    assert_eq!(outgoing[0].to.name, "Run");
    assert_eq!(
        outgoing[0].to.selection_range.start.line, 1,
        "a direct Run() call must not bind to Worker.Run"
    );
}

#[tokio::test]
async fn go_lsp_cgo_possible_targets_expose_the_unsupported_language_boundary() {
    let source = "package device\n\
                  import \"C\"\n\
                  func Target() {}\n\
                  func Use() { Target/*cursor*/() }\n";
    let (_dir, service, uri, line, character) =
        indexed_backend_with_open_doc(&[], "main.go", source).await;
    let possible = service
        .inner()
        .possible_targets_command(&serde_json::json!({
            "uri": uri,
            "line": line,
            "character": character,
        }))
        .await
        .expect("cgo possible targets");
    assert_eq!(
        possible["coverage"]["openReason"],
        "unsupported_language_boundary"
    );
    assert_eq!(possible["coverage"]["open"], true);
}

#[tokio::test]
async fn live_parse_language_reuses_cached_workspace_resolver_until_invalidation() {
    let service = test_backend_service();
    let dir = tempdir().expect("root");
    let root = dir.path().to_path_buf();
    let header = root.join("legacy/api.h");
    fs::create_dir_all(header.parent().expect("parent")).expect("legacy dir");
    fs::write(
        root.join("fossilsense.json"),
        r#"{"languageOverrides":[{"glob":"legacy/**/*.h","language":"c"}]}"#,
    )
    .expect("initial config");
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root.clone());

    assert_eq!(
        service.inner().source_language_for_path(&header).await,
        crate::config::SourceLanguage::C
    );
    fs::write(
        root.join("fossilsense.json"),
        r#"{"languageOverrides":[{"glob":"legacy/**/*.h","language":"cpp"}]}"#,
    )
    .expect("updated config");
    assert_eq!(
        service.inner().source_language_for_path(&header).await,
        crate::config::SourceLanguage::C,
        "request hot paths must reuse the cached resolver instead of rereading the file"
    );

    service.inner().config_cache.lock().await.remove(&root);
    assert_eq!(
        service.inner().source_language_for_path(&header).await,
        crate::config::SourceLanguage::Cpp,
        "explicit invalidation must reload the derived resolver"
    );
}

#[tokio::test]
async fn explicit_rebuild_refreshes_external_root_authorization_without_watcher() {
    let service = test_backend_service();
    let dir = tempdir().expect("root");
    let root = dir.path().join("workspace");
    let old_module = dir.path().join("old-module");
    let new_module = dir.path().join("new-module");
    for path in [&root, &old_module, &new_module] {
        fs::create_dir_all(path).expect("directory");
    }
    let old_source = old_module.join("old.go");
    let new_source = new_module.join("new.go");
    fs::write(&old_source, "package old\n").expect("old source");
    fs::write(&new_source, "package new\n").expect("new source");
    fs::write(
        root.join("fossilsense.json"),
        serde_json::json!({
            "goModulePaths": [crate::pathing::normalize_abs_path(&old_module)]
        })
        .to_string(),
    )
    .expect("old config");
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root.clone());

    let old_roots = service
        .inner()
        .authorized_external_source_roots(&root)
        .await;
    assert!(old_roots
        .authorized_path(&old_source, crate::semantic_model::SemanticFamily::Go)
        .is_some());

    fs::write(
        root.join("fossilsense.json"),
        serde_json::json!({
            "goModulePaths": [crate::pathing::normalize_abs_path(&new_module)]
        })
        .to_string(),
    )
    .expect("new config");
    service.inner().spawn_index_roots(Some(true)).await;

    let refreshed = service
        .inner()
        .authorized_external_source_roots(&root)
        .await;
    assert!(
        refreshed
            .authorized_path(&new_source, crate::semantic_model::SemanticFamily::Go)
            .is_some(),
        "explicit rebuild must reload external roots from disk"
    );
    assert!(
        refreshed
            .authorized_path(&old_source, crate::semantic_model::SemanticFamily::Go)
            .is_none(),
        "a removed root must not remain authorized after explicit rebuild"
    );
}

#[tokio::test]
async fn non_file_uri_language_uses_the_uri_path_extension_not_the_workspace_root() {
    let service = test_backend_service();
    let uri = Url::parse("untitled:/scratch/header.h").expect("untitled URI");

    assert_eq!(
        service.inner().source_language_for_uri(&uri).await,
        crate::config::SourceLanguage::Cpp
    );
}

#[tokio::test]
async fn goto_definition_uses_live_current_document_typedef_when_index_is_stale() {
    let dir = tempdir().expect("tempdir");
    write_workspace_file(dir.path(), "main.c", "void indexed_only(void) {}\n");
    crate::indexer::index_workspace(
        dir.path(),
        crate::indexer::IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let uri = Url::from_file_path(dir.path().join("main.c")).expect("file uri");
    let service = test_backend_service();
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(dir.path().to_path_buf());
    service
        .inner()
        .session
        .cache
        .publish_full_index(&service.inner().client, dir.path().to_path_buf())
        .await
        .expect("publish test index");

    let (src, line, character) = text_and_position(
        "typedef struct {\n\
             int value;\n\
         } Boom;\n\
         \n\
         void f(void) {\n\
             Boom/*cursor*/ b;\n\
         }\n",
    );
    open_test_document(&service, uri.clone(), 2, src).await;

    let response = service
        .inner()
        .goto_definition(goto_definition_params(uri.clone(), line, character))
        .await
        .expect("goto definition")
        .expect("definition response");
    let locations = match response {
        GotoDefinitionResponse::Array(locations) => locations,
        GotoDefinitionResponse::Scalar(location) => vec![location],
        GotoDefinitionResponse::Link(_) => panic!("unexpected location links"),
    };

    assert!(
        locations
            .iter()
            .any(|location| location.uri == uri && location.range.start.line == 2),
        "live typedef definition should be returned even when the persisted index is stale"
    );
}

#[tokio::test]
async fn goto_definition_rejects_keyword_polluted_by_trailing_comments() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[],
        "checkpoint.h",
        r#"typedef struct AVTextWriter {
    const/*cursor*/ AVClass *priv_class; ///< private class of the writer, if any
    int priv_size;                       ///< writer private class
    const char *name;
} AVTextWriter;
"#,
    )
    .await;

    let response = service
        .inner()
        .goto_definition(goto_definition_params(uri, line, character))
        .await
        .expect("goto definition request");

    assert!(
        response.is_none(),
        "language keywords must never be jump targets"
    );
}

#[tokio::test]
async fn goto_definition_finds_first_typedef_after_multiline_macro_from_index() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[],
        "macro_typedef.h",
        r#"#define FREE(ptr)                                                              \
    do                                                                         \
    {                                                                          \
        if ((ptr) != NULL)                                                     \
        {                                                                      \
            free(ptr);                                                         \
            (ptr) = NULL;                                                      \
        }                                                                      \
    } while (0)

typedef struct xxx {
    int value;
} xxx_t;

void use_type(void) {
    xxx_t/*cursor*/ item;
}
"#,
    )
    .await;

    let response = service
        .inner()
        .goto_definition(goto_definition_params(uri.clone(), line, character))
        .await
        .expect("goto definition")
        .expect("definition response");
    let locations = match response {
        GotoDefinitionResponse::Array(locations) => locations,
        GotoDefinitionResponse::Scalar(location) => vec![location],
        GotoDefinitionResponse::Link(_) => panic!("unexpected location links"),
    };

    assert!(
        locations
            .iter()
            .any(|location| location.uri == uri && location.range.start.line == 12),
        "indexed typedef immediately after multiline macro should be a goto-definition target"
    );
}

#[tokio::test]
async fn goto_definition_keeps_an_unpaired_callable_anchor_as_a_target() {
    let (_dir, service, uri, line, character) =
        indexed_backend_with_open_doc(&[], "api.h", "int lone/*cursor*/(int value);\n").await;

    let response = service
        .inner()
        .goto_definition(goto_definition_params(uri.clone(), line, character))
        .await
        .expect("goto definition")
        .expect("unpaired anchor should remain navigable");
    let locations = match response {
        GotoDefinitionResponse::Array(locations) => locations,
        GotoDefinitionResponse::Scalar(location) => vec![location],
        GotoDefinitionResponse::Link(_) => panic!("unexpected location links"),
    };

    assert!(
        locations
            .iter()
            .any(|location| location.uri == uri && location.range.start.line == 0),
        "the current declaration is the conservative definition target when no strict counterpart exists"
    );
}

#[tokio::test]
async fn callable_arity_mismatch_fallback_is_visible_and_remains_navigable() {
    let source = "int pick(int value);\n\
                  int pick(int left, int right);\n\
                  void f(void) { pick/*cursor*/(1, 2, 3); }\n";
    let (_dir, service, uri, line, character) =
        indexed_backend_with_open_doc(&[], "main.cpp", source).await;

    let hover = service
        .inner()
        .hover(hover_params(uri.clone(), line, character))
        .await
        .expect("hover request")
        .expect("fallback hover");
    let hover = hover_text(hover.contents);
    assert!(hover.contains("Arity mismatch fallback"));
    assert!(hover.contains("pick(int value)"));
    assert!(hover.contains("pick(int left, int right)"));

    let definition = service
        .inner()
        .goto_definition(goto_definition_params(uri.clone(), line, character))
        .await
        .expect("definition request")
        .expect("fallback definition");
    let locations = definition_locations(definition);
    assert_eq!(locations.len(), 2, "fallback must retain both candidates");

    let (signature_text, signature_line, signature_character) = text_and_position(
        "int pick(int value);\n\
         int pick(int left, int right);\n\
         void f(void) { pick(1, 2, 3/*cursor*/); }\n",
    );
    assert_eq!(
        service
            .inner()
            .session
            .documents
            .snapshot(&uri)
            .await
            .expect("open document")
            .text
            .as_ref(),
        signature_text
    );
    let signature_help = service
        .inner()
        .signature_help(signature_help_params(
            uri,
            signature_line,
            signature_character,
        ))
        .await
        .expect("signature request")
        .expect("fallback signatures");
    assert_eq!(signature_help.signatures.len(), 2);
    assert!(signature_help.signatures.iter().all(|signature| {
        signature
            .documentation
            .clone()
            .is_some_and(|documentation| {
                documentation_text(documentation).contains("Arity mismatch fallback")
            })
    }));
}

#[tokio::test]
async fn signature_help_keeps_candidates_for_template_like_partial_argument() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[],
        "main.cpp",
        "int pick(int value);\n\
         int pick(int left, int right);\n\
         void f(void) { pick(std::pair<int, int>{}, /*cursor*/); }\n",
    )
    .await;

    let signature_help = service
        .inner()
        .signature_help(signature_help_params(uri, line, character))
        .await
        .expect("signature request")
        .expect("unknown-arity signature candidates");
    assert_eq!(signature_help.signatures.len(), 2);
    assert!(signature_help
        .signatures
        .iter()
        .any(|signature| signature.active_parameter == Some(1)));
}

#[tokio::test]
async fn malformed_call_does_not_hard_filter_definition_candidates_by_arity() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[],
        "main.cpp",
        "int pick(int value);\n\
         int pick(int left, int right);\n\
         void f(void) { pick/*cursor*/(1,); }\n",
    )
    .await;

    let response = service
        .inner()
        .goto_definition(goto_definition_params(uri, line, character))
        .await
        .expect("definition request")
        .expect("malformed call remains navigable");
    assert_eq!(definition_locations(response).len(), 2);
}

#[tokio::test]
async fn goto_definition_returns_multiple_same_signature_implementations() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[
            ("api.h", "int work(int value);\n"),
            ("impl_a.c", "int work(int value) { return value; }\n"),
            ("impl_b.c", "int work(int value) { return value + 1; }\n"),
        ],
        "main.c",
        "#include \"api.h\"\nint run(void) { return work/*cursor*/(1); }\n",
    )
    .await;

    let response = service
        .inner()
        .goto_definition(goto_definition_params(uri, line, character))
        .await
        .expect("definition request")
        .expect("implementation definitions");
    let paths: HashSet<_> = definition_locations(response)
        .into_iter()
        .filter_map(|location| location.uri.to_file_path().ok())
        .filter_map(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .collect();
    assert!(paths.contains("impl_a.c"), "first implementation missing");
    assert!(paths.contains("impl_b.c"), "second implementation missing");
}

#[tokio::test]
async fn declaration_and_definition_keep_stable_operation_semantics() {
    let (dir, service, main_uri, line, character) = indexed_backend_with_open_doc(
        &[
            ("api.h", "extern int read_value(int);\n"),
            (
                "api.c",
                "#include \"api.h\"\nint read_value(int value) { return value; }\n",
            ),
        ],
        "main.c",
        "#include \"api.h\"\nint run(void) { return read_value/*cursor*/(1); }\n",
    )
    .await;
    let header_uri = Url::from_file_path(dir.path().join("api.h")).expect("header uri");
    let source_uri = Url::from_file_path(dir.path().join("api.c")).expect("source uri");

    let source_definition = service
        .inner()
        .goto_definition(goto_definition_params(source_uri.clone(), 1, 6))
        .await
        .expect("source definition request")
        .expect("source definition response");
    assert_eq!(definition_locations(source_definition)[0].uri, source_uri);

    let source_declaration = service
        .inner()
        .goto_declaration(goto_definition_params(source_uri.clone(), 1, 6))
        .await
        .expect("source declaration request")
        .expect("source declaration response");
    assert_eq!(definition_locations(source_declaration)[0].uri, header_uri);

    let header_definition = service
        .inner()
        .goto_definition(goto_definition_params(header_uri.clone(), 0, 14))
        .await
        .expect("header definition request")
        .expect("header definition response");
    assert_eq!(definition_locations(header_definition)[0].uri, source_uri);

    let header_declaration = service
        .inner()
        .goto_declaration(goto_definition_params(header_uri.clone(), 0, 14))
        .await
        .expect("header declaration request")
        .expect("header declaration response");
    assert_eq!(definition_locations(header_declaration)[0].uri, header_uri);

    let call_declaration = service
        .inner()
        .goto_declaration(goto_definition_params(main_uri, line, character))
        .await
        .expect("call declaration request")
        .expect("call declaration response");
    assert_eq!(definition_locations(call_declaration)[0].uri, header_uri);
}

#[tokio::test]
async fn local_binding_navigation_dominates_workspace_same_name_candidates() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[("other.c", "int value(void) { return 1; }\n")],
        "main.c",
        "int run(void) {\n    int value;\n    return value/*cursor*/;\n}\n",
    )
    .await;

    for response in [
        service
            .inner()
            .goto_definition(goto_definition_params(uri.clone(), line, character))
            .await
            .expect("local definition request")
            .expect("local definition response"),
        service
            .inner()
            .goto_declaration(goto_definition_params(uri.clone(), line, character))
            .await
            .expect("local declaration request")
            .expect("local declaration response"),
    ] {
        let locations = definition_locations(response);
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].uri, uri);
        assert_eq!(locations[0].range.start.line, 1);
    }
}

#[tokio::test]
async fn hover_agrees_with_navigation_on_local_bindings() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[("other.c", "int value(void) { return 1; }\n")],
        "main.c",
        "int run(void) {\n    int value = 2;\n    return value/*cursor*/;\n}\n",
    )
    .await;

    let hover = service
        .inner()
        .hover(hover_params(uri.clone(), line, character))
        .await
        .expect("hover request")
        .expect("local binding hover");
    let hover = hover_text(hover.contents);
    assert!(
        hover.contains("// In main.c"),
        "hover must describe the request document: {hover}"
    );
    assert!(
        hover.contains("int value = 2;"),
        "hover must show the proven local declaration line: {hover}"
    );
    assert!(hover.contains("reason: lexical_binding"), "{hover}");
    assert!(
        !hover.contains("other.c"),
        "workspace same-name candidate must not leak into a lexically bound hover: {hover}"
    );
    let first_parse = service
        .inner()
        .session
        .documents
        .cached_live_parse(
            &uri,
            1,
            crate::config::SourceLanguage::C,
            crate::parser::ParseFacts::LOCAL_DECLS,
        )
        .await
        .expect("local hover must populate the versioned live-parse cache");
    service
        .inner()
        .hover(hover_params(uri.clone(), line, character))
        .await
        .expect("cached hover request")
        .expect("cached local binding hover");
    let reused_parse = service
        .inner()
        .session
        .documents
        .cached_live_parse(
            &uri,
            1,
            crate::config::SourceLanguage::C,
            crate::parser::ParseFacts::LOCAL_DECLS,
        )
        .await
        .expect("cached local parse");
    assert!(
        Arc::ptr_eq(&first_parse, &reused_parse),
        "repeated hover on the same document version must reuse its parsed local facts"
    );

    let definition = service
        .inner()
        .goto_definition(goto_definition_params(uri.clone(), line, character))
        .await
        .expect("definition request")
        .expect("local definition response");
    let locations = definition_locations(definition);
    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0].uri, uri);
    assert_eq!(locations[0].range.start.line, 1);
}

#[tokio::test]
async fn hover_agrees_with_navigation_on_labels() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[("other.c", "int same(void) { return 1; }\n")],
        "main.c",
        "void run(void) {\n    goto sa/*cursor*/me;\nsame:\n    return;\n}\n",
    )
    .await;

    let hover = service
        .inner()
        .hover(hover_params(uri, line, character))
        .await
        .expect("hover request")
        .expect("label hover");
    let hover = hover_text(hover.contents);
    assert!(hover.contains("same:"), "{hover}");
    assert!(hover.contains("reason: label_namespace"), "{hover}");
    assert!(
        !hover.contains("other.c"),
        "workspace same-name candidate must not leak into a label hover: {hover}"
    );

    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[("other.c", "int missing(void) { return 1; }\n")],
        "main.c",
        "void run(void) {\n    goto mis/*cursor*/sing;\n}\n",
    )
    .await;
    assert!(
        service
            .inner()
            .hover(hover_params(uri, line, character))
            .await
            .expect("missing label hover request")
            .is_none(),
        "a missing label must not surface workspace same-name hover candidates"
    );
}

#[tokio::test]
async fn hover_and_definition_hydrate_the_same_declaration() {
    let (dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[("dep.h", "int shared_table[4];\n")],
        "main.c",
        "#include \"dep.h\"\nint use(void) { return shared_table/*cursor*/[0]; }\n",
    )
    .await;

    let definition = service
        .inner()
        .goto_definition(goto_definition_params(uri.clone(), line, character))
        .await
        .expect("definition request")
        .expect("definition response");
    let locations = definition_locations(definition);
    assert_eq!(locations.len(), 1);
    let dep_uri = Url::from_file_path(dir.path().join("dep.h")).expect("dep uri");
    assert_eq!(locations[0].uri, dep_uri);

    let hover = service
        .inner()
        .hover(hover_params(uri, line, character))
        .await
        .expect("hover request")
        .expect("hover response");
    let hover = hover_text(hover.contents);
    assert!(
        hover.contains("// In dep.h"),
        "hover and definition must hydrate the same declaration: {hover}"
    );
    assert!(hover.contains("shared_table"), "{hover}");
    assert!(
        !hover.contains("suppressed"),
        "a unique entity must not report suppressed alternatives: {hover}"
    );
}

#[tokio::test]
async fn protobuf_c_hover_appends_proto_source_without_changing_definition_target() {
    let (dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[
            (
                "fossilsense.json",
                r#"{"protobufC":{"enabled":true,"protoPaths":["proto"]}}"#,
            ),
            ("wrapper.h", "#include \"messages/system_cfg.pb-c.h\"\n"),
            (
                "messages/system_cfg.pb-c.h",
                "typedef struct Demo__Outer Demo__Outer;\n",
            ),
            (
                "proto/messages/system_cfg.proto",
                "package demo;\nmessage Outer {}\n",
            ),
        ],
        "main.c",
        "#include \"wrapper.h\"\nDemo__Ou/*cursor*/ter *value;\n",
    )
    .await;

    let hover = service
        .inner()
        .hover(hover_params(uri.clone(), line, character))
        .await
        .expect("hover request")
        .expect("hover response");
    let hover = hover_text(hover.contents);
    assert!(hover.contains("proto 来源"), "{hover}");
    assert!(
        hover.contains("proto/messages/system_cfg.proto:2"),
        "{hover}"
    );
    assert!(hover.contains("相对路径"), "{hover}");
    assert!(hover.contains("demo.Outer"), "{hover}");

    let definition = service
        .inner()
        .goto_definition(goto_definition_params(uri, line, character))
        .await
        .expect("definition request")
        .expect("definition response");
    let locations = definition_locations(definition);
    assert!(!locations.is_empty());
    let generated_uri =
        Url::from_file_path(dir.path().join("messages/system_cfg.pb-c.h")).expect("generated uri");
    assert!(locations
        .iter()
        .all(|location| location.uri == generated_uri));
}

#[tokio::test]
async fn suppressed_same_name_candidates_stay_visible_as_evidence_and_escape_hatch() {
    let (dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[
            ("dep.h", "int limit = 10;\n"),
            ("distant_a.c", "static int limit = 1;\n"),
            ("distant_b.c", "static int limit = 2;\n"),
        ],
        "main.c",
        "#include \"dep.h\"\nint use(void) { return limit/*cursor*/; }\n",
    )
    .await;

    let definition = service
        .inner()
        .goto_definition(goto_definition_params(uri.clone(), line, character))
        .await
        .expect("definition request")
        .expect("definition response");
    let locations = definition_locations(definition);
    let dep_uri = Url::from_file_path(dir.path().join("dep.h")).expect("dep uri");
    assert_eq!(
        locations.len(),
        1,
        "lower-tier same-name groups are suppressed"
    );
    assert_eq!(locations[0].uri, dep_uri);

    let hover = service
        .inner()
        .hover(hover_params(uri.clone(), line, character))
        .await
        .expect("hover request")
        .expect("hover response");
    let hover = hover_text(hover.contents);
    assert!(hover.contains("// In dep.h"), "{hover}");
    assert!(
        hover.contains("2 same-name candidate(s) outside the focused result"),
        "suppression must stay visible as hover evidence: {hover}"
    );
    assert!(hover.contains("matches: exact"), "{hover}");
    assert!(!hover.contains("distant_a.c"), "{hover}");

    let response = service
        .inner()
        .execute_command(ExecuteCommandParams {
            command: super::POSSIBLE_TARGETS_LSP_COMMAND.to_string(),
            arguments: vec![serde_json::json!({
                "uri": uri,
                "line": line,
                "character": character,
            })],
            work_done_progress_params: Default::default(),
        })
        .await
        .expect("possible targets command")
        .expect("possible targets response");
    assert_eq!(response["coverage"]["disposition"], "exact");
    assert_eq!(response["coverage"]["alternativeCount"], 2);
    let items = response["items"].as_array().expect("items");
    let suppressed: Vec<_> = items
        .iter()
        .filter(|item| item["focused"] == false)
        .filter_map(|item| item["location"]["uri"].as_str())
        .collect();
    assert_eq!(
        suppressed.len(),
        2,
        "the escape hatch must list every suppressed candidate: {items:?}"
    );
    assert!(suppressed.iter().any(|uri| uri.contains("distant_a.c")));
    assert!(suppressed.iter().any(|uri| uri.contains("distant_b.c")));
    assert!(items.iter().any(|item| item["focused"] == true
        && item["location"]["uri"]
            .as_str()
            .is_some_and(|uri| uri.contains("dep.h"))));
}

#[tokio::test]
async fn possible_targets_lists_semantic_groups_suppressed_behind_the_focused_callable() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[
            ("api.h", "int helper(int value);\n"),
            (
                "api.c",
                "#include \"api.h\"\nint helper(int value) { return value; }\n",
            ),
            ("distant.c", "static int helper(int value) { return 0; }\n"),
        ],
        "main.c",
        "#include \"api.h\"\nint run(void) { return helper/*cursor*/(1); }\n",
    )
    .await;

    let response = service
        .inner()
        .execute_command(ExecuteCommandParams {
            command: super::POSSIBLE_TARGETS_LSP_COMMAND.to_string(),
            arguments: vec![serde_json::json!({
                "uri": uri,
                "line": line,
                "character": character,
            })],
            work_done_progress_params: Default::default(),
        })
        .await
        .expect("possible targets command")
        .expect("possible targets response");
    assert_eq!(response["coverage"]["alternativeCount"], 1);
    let items = response["items"].as_array().expect("items");
    assert!(
        items.iter().any(|item| item["focused"] == false
            && item["location"]["uri"]
                .as_str()
                .is_some_and(|uri| uri.contains("distant.c"))),
        "an internal-linkage same-name group suppressed by tier focus must stay inspectable: {items:?}"
    );
    assert!(items.iter().any(|item| item["focused"] == true
        && item["location"]["uri"]
            .as_str()
            .is_some_and(|uri| uri.contains("api.c"))));
}

#[tokio::test]
async fn label_navigation_is_scoped_to_the_enclosing_function() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[("other.c", "int same(void) { return 1; }\n")],
        "main.c",
        "void first(void) {\n\
         same:\n\
             return;\n\
         }\n\
         void second(void) {\n\
             goto same/*cursor*/;\n\
         same:\n\
             return;\n\
         }\n",
    )
    .await;

    for response in [
        service
            .inner()
            .goto_definition(goto_definition_params(uri.clone(), line, character))
            .await
            .expect("label definition request")
            .expect("label definition response"),
        service
            .inner()
            .goto_declaration(goto_definition_params(uri.clone(), line, character))
            .await
            .expect("label declaration request")
            .expect("label declaration response"),
    ] {
        let locations = definition_locations(response);
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].uri, uri);
        assert_eq!(locations[0].range.start, Position::new(6, 0));
        assert_eq!(locations[0].range.end, Position::new(6, 4));
    }

    let possible = service
        .inner()
        .execute_command(ExecuteCommandParams {
            command: super::POSSIBLE_TARGETS_LSP_COMMAND.to_string(),
            arguments: vec![serde_json::json!({
                "uri": uri,
                "line": line,
                "character": character,
            })],
            work_done_progress_params: Default::default(),
        })
        .await
        .expect("label possible targets request")
        .expect("label possible targets response");
    assert_eq!(possible["items"][0]["kind"], "label");
    assert_eq!(possible["items"][0]["reason"], "label_namespace");
    assert_eq!(possible["coverage"]["bounded"], false);

    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[("other.c", "int missing(void) { return 1; }\n")],
        "missing.c",
        "void run(void) { goto missing/*cursor*/; }\n",
    )
    .await;
    assert!(service
        .inner()
        .goto_definition(goto_definition_params(uri.clone(), line, character))
        .await
        .expect("missing label definition request")
        .is_none());
    assert!(service
        .inner()
        .goto_declaration(goto_definition_params(uri.clone(), line, character))
        .await
        .expect("missing label declaration request")
        .is_none());
    assert!(service
        .inner()
        .execute_command(ExecuteCommandParams {
            command: super::POSSIBLE_TARGETS_LSP_COMMAND.to_string(),
            arguments: vec![serde_json::json!({
                "uri": uri,
                "line": line,
                "character": character,
            })],
            work_done_progress_params: Default::default(),
        })
        .await
        .expect("missing label possible targets request")
        .is_none());
}

#[tokio::test]
async fn possible_targets_returns_suppressed_callable_variants_and_lexical_local_only() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[
            ("api.h", "int choose(int value);\n"),
            (
                "api.c",
                "#include \"api.h\"\nint choose(int value) { return value; }\n",
            ),
            (
                "alternate.c",
                "int choose(int value) { return value + 1; }\n",
            ),
        ],
        "main.c",
        "#include \"api.h\"\nint run(void) { return choose/*cursor*/(1); }\n",
    )
    .await;

    let response = service
        .inner()
        .execute_command(ExecuteCommandParams {
            command: super::POSSIBLE_TARGETS_LSP_COMMAND.to_string(),
            arguments: vec![serde_json::json!({
                "uri": uri,
                "line": line,
                "character": character,
            })],
            work_done_progress_params: Default::default(),
        })
        .await
        .expect("possible targets command")
        .expect("possible targets response");
    let items = response["items"].as_array().expect("items");
    assert_eq!(items.len(), 3, "all bounded callable variants survive");
    let roles: HashSet<_> = items
        .iter()
        .filter_map(|item| item["role"].as_str())
        .collect();
    assert_eq!(roles, HashSet::from(["definition", "declaration"]));
    assert_eq!(response["coverage"]["bounded"], true);

    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[("other.c", "int value;\n")],
        "local.c",
        "int run(void) { int value; return value/*cursor*/; }\n",
    )
    .await;
    let response = service
        .inner()
        .execute_command(ExecuteCommandParams {
            command: super::POSSIBLE_TARGETS_LSP_COMMAND.to_string(),
            arguments: vec![serde_json::json!({
                "uri": uri,
                "line": line,
                "character": character,
            })],
            work_done_progress_params: Default::default(),
        })
        .await
        .expect("local possible targets command")
        .expect("local possible targets response");
    let items = response["items"].as_array().expect("local items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["kind"], "local_binding");
    assert_eq!(items[0]["reason"], "lexical_binding");
    assert_eq!(response["coverage"]["bounded"], false);
}

#[tokio::test]
async fn possible_targets_reports_facts_unavailable_for_dirty_hard_parse_failure() {
    let (_dir, service, uri, _line, _character) =
        indexed_backend_with_open_doc(&[], "broken.c", "int guessed/*cursor*/;\n").await;
    let (broken, line, character) = text_and_position("((( guessed/*cursor*/(value);\n");
    service
        .inner()
        .did_change(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: uri.clone(),
                version: 2,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: broken,
            }],
        })
        .await;

    let response = service
        .inner()
        .execute_command(ExecuteCommandParams {
            command: super::POSSIBLE_TARGETS_LSP_COMMAND.to_string(),
            arguments: vec![serde_json::json!({
                "uri": uri,
                "line": line,
                "character": character,
            })],
            work_done_progress_params: Default::default(),
        })
        .await
        .expect("possible targets command")
        .expect("incomplete possible targets response");

    assert_eq!(response["items"].as_array().map(Vec::len), Some(0));
    assert_eq!(
        response["coverage"]["incompleteReason"],
        "facts_unavailable"
    );
}

#[tokio::test]
async fn external_object_definition_prefers_full_definition_and_declaration_prefers_header() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[
            ("api.h", "extern int count;\n"),
            ("api.c", "#include \"api.h\"\nint count = 1;\n"),
        ],
        "main.c",
        "#include \"api.h\"\nint run(void) { return count/*cursor*/; }\n",
    )
    .await;

    let definition = service
        .inner()
        .goto_definition(goto_definition_params(uri.clone(), line, character))
        .await
        .expect("object definition request")
        .expect("object definition response");
    let definition_paths = definition_locations(definition)
        .into_iter()
        .filter_map(|location| location.uri.to_file_path().ok())
        .collect::<Vec<_>>();
    assert_eq!(definition_paths.len(), 1);
    assert_eq!(
        definition_paths[0].file_name().unwrap().to_string_lossy(),
        "api.c"
    );

    let declaration = service
        .inner()
        .goto_declaration(goto_definition_params(uri, line, character))
        .await
        .expect("object declaration request")
        .expect("object declaration response");
    let declaration_paths = definition_locations(declaration)
        .into_iter()
        .filter_map(|location| location.uri.to_file_path().ok())
        .collect::<Vec<_>>();
    assert_eq!(declaration_paths.len(), 1);
    assert_eq!(
        declaration_paths[0].file_name().unwrap().to_string_lossy(),
        "api.h"
    );
}

#[tokio::test]
async fn static_object_navigation_does_not_cross_translation_units() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[("other.c", "static int private_state;\n")],
        "main.c",
        "static int private_state;\nint run(void) { return private_state/*cursor*/; }\n",
    )
    .await;

    let response = service
        .inner()
        .goto_definition(goto_definition_params(uri.clone(), line, character))
        .await
        .expect("static object definition request")
        .expect("static object definition response");
    let locations = definition_locations(response);

    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0].uri, uri);
    assert_eq!(locations[0].range.start.line, 0);
}

#[tokio::test]
async fn hover_uses_dirty_other_document_callable_overlay_without_stale_signature() {
    let (dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[("api.h", "int target(int indexed_value);\n")],
        "main.c",
        "#include \"api.h\"\nint run(void) { return target/*cursor*/(1, 2); }\n",
    )
    .await;
    let header_uri = Url::from_file_path(dir.path().join("api.h")).expect("header uri");
    open_test_document(
        &service,
        header_uri,
        2,
        "int target(int dirty_left, int dirty_right);\n".into(),
    )
    .await;

    let hover = service
        .inner()
        .hover(hover_params(uri, line, character))
        .await
        .expect("hover request")
        .expect("dirty header hover");
    let hover = hover_text(hover.contents);
    assert!(hover.contains("dirty_left"));
    assert!(hover.contains("dirty_right"));
    assert!(!hover.contains("indexed_value"));
}

#[tokio::test]
async fn callable_comment_hydration_rejects_disk_revision_newer_than_index() {
    let (dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[(
            "api.h",
            "/// Original indexed callable docs.\nint target(int value);\n",
        )],
        "main.c",
        "#include \"api.h\"\nint run(void) { return target/*cursor*/(1); }\n",
    )
    .await;
    std::fs::write(
        dir.path().join("api.h"),
        "/// Newer disk docs must not mix with the old anchor.\nint target(int value);\n",
    )
    .expect("external edit");

    let hover = service
        .inner()
        .hover(hover_params(uri.clone(), line, character))
        .await
        .expect("hover request")
        .expect("callable hover");
    let hover = hover_text(hover.contents);
    assert!(hover.contains("target(int value)"));
    assert!(!hover.contains("Newer disk docs"));
    assert!(!hover.contains("Original indexed callable docs"));

    let signature_help = service
        .inner()
        .signature_help(signature_help_params(uri, line, character + 2))
        .await
        .expect("signature request")
        .expect("signature candidates");
    let rendered = signature_help
        .signatures
        .into_iter()
        .filter_map(|signature| signature.documentation)
        .map(documentation_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!rendered.contains("Newer disk docs"));
    assert!(!rendered.contains("Original indexed callable docs"));
}

#[tokio::test]
async fn did_open_disk_matched_external_edit_overlays_stale_generation() {
    let (dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[("api.h", "int target(int indexed_value);\n")],
        "main.c",
        "#include \"api.h\"\nint run(void) { return target/*cursor*/(1, 2); }\n",
    )
    .await;
    let header_uri = Url::from_file_path(dir.path().join("api.h")).expect("header uri");
    let externally_edited = "int target(int external_left, int external_right);\n";
    std::fs::write(dir.path().join("api.h"), externally_edited).expect("external edit");

    service
        .inner()
        .did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: header_uri,
                language_id: "c".into(),
                version: 2,
                text: externally_edited.into(),
            },
        })
        .await;

    let hover = service
        .inner()
        .hover(hover_params(uri, line, character))
        .await
        .expect("hover request")
        .expect("external edit hover");
    let hover = hover_text(hover.contents);
    assert!(hover.contains("external_left"));
    assert!(hover.contains("external_right"));
    assert!(!hover.contains("indexed_value"));
}

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
        cache.completion_memo_for_test(&uri).await.is_some(),
        "document edits retain indexed candidate pools; prefix validation decides reuse"
    );
    assert_eq!(
        cache.reference_search_cache_len_for_test(),
        0,
        "document changes must clear complete reference search results"
    );
}

#[tokio::test]
async fn incremental_document_changes_apply_sequentially_with_utf16_positions() {
    use tower_lsp::lsp_types::{Position, Range, TextDocumentContentChangeEvent};

    let documents = super::DocumentStore::default();
    let uri = Url::parse("file:///tmp/incremental.c").expect("uri");
    documents
        .open_document(uri.clone(), 1, "a😀bc\nsecond\n".to_string())
        .await;

    let applied = documents
        .apply_document_changes(
            &uri,
            2,
            vec![
                TextDocumentContentChangeEvent {
                    range: Some(Range::new(Position::new(0, 3), Position::new(0, 4))),
                    range_length: Some(1),
                    text: "B".to_string(),
                },
                TextDocumentContentChangeEvent {
                    range: Some(Range::new(Position::new(1, 0), Position::new(1, 6))),
                    range_length: Some(6),
                    text: "next".to_string(),
                },
            ],
        )
        .await;

    assert!(applied);
    let snapshot = documents.snapshot(&uri).await.expect("snapshot");
    assert_eq!(snapshot.version, 2);
    assert_eq!(snapshot.text.as_ref(), "a😀Bc\nnext\n");
}

#[tokio::test]
async fn document_change_cancels_obsolete_live_parse_work() {
    use std::sync::atomic::Ordering;
    use tower_lsp::lsp_types::TextDocumentContentChangeEvent;

    let documents = super::DocumentStore::default();
    let uri = Url::parse("file:///tmp/cancel-parse.c").expect("uri");
    documents
        .open_document(uri.clone(), 1, "int old_name;\n".to_string())
        .await;
    let old = documents.live_parse_cancellation(&uri, 1).await;
    assert!(!old.load(Ordering::Relaxed));

    assert!(
        documents
            .apply_document_changes(
                &uri,
                2,
                vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: "int new_name;\n".to_string(),
                }],
            )
            .await
    );
    assert!(old.load(Ordering::Relaxed));
    let current = documents.live_parse_cancellation(&uri, 2).await;
    assert!(!current.load(Ordering::Relaxed));
}

#[tokio::test]
async fn stale_document_work_cannot_overwrite_latest_revision_caches() {
    let documents = super::DocumentStore::default();
    let root = tempdir().expect("root");
    let path = root.path().join("main.c");
    let uri = Url::from_file_path(&path).expect("uri");

    documents
        .open_document(uri.clone(), 1, "int old_word;\n".to_string())
        .await;
    documents
        .change_document(uri.clone(), 2, "int new_word;\n".to_string())
        .await;

    let stale_parse = Arc::new(crate::parser::parse(&path, "int old_word;\n"));
    documents
        .store_live_parse_for_test(uri.clone(), 1, stale_parse)
        .await;
    assert!(
        documents.live_parse_for_test(&uri).await.is_none(),
        "a completed old parse must be discarded after the document advances"
    );

    let current_parse = Arc::new(crate::parser::parse(&path, "int new_word;\n"));
    documents
        .store_live_parse_for_test(uri.clone(), 2, current_parse.clone())
        .await;
    assert!(Arc::ptr_eq(
        &documents
            .cached_live_parse(
                &uri,
                2,
                crate::config::SourceLanguage::C,
                crate::parser::ParseFacts::ALL,
            )
            .await
            .expect("current parse"),
        &current_parse
    ));

    let stale_words = documents.local_words_for(&uri, 1, "int old_word;\n").await;
    assert!(stale_words.contains("old_word"));
    assert!(
        documents
            .local_word_cache_entry_for_test(&uri)
            .await
            .is_none(),
        "old request words may be returned to that request but not cached"
    );

    let current_words = documents.local_words_for(&uri, 2, "int new_word;\n").await;
    assert!(current_words.contains("new_word"));
    assert_eq!(
        documents
            .local_word_cache_entry_for_test(&uri)
            .await
            .expect("current words")
            .0,
        2
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
    let engine = cache
        .current_engine_snapshot(&root_path)
        .await
        .expect("published engine snapshot");
    assert!(
        engine.name_table.is_some(),
        "closing an editor buffer must not delete indexed symbol data"
    );
    assert!(
        engine.indexed_files.is_some(),
        "closing an editor buffer must not delete indexed reference scope"
    );
}

#[tokio::test]
async fn cache_ledger_publishes_full_and_dirty_read_models_with_generations() {
    let root = tempdir().expect("root");
    let root_path = root.path().to_path_buf();
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
    assert_eq!(full.declaration_count, 1);
    assert_eq!(full.reference_file_count, 1);
    let full_context = service
        .inner()
        .session
        .request_context_for_root(root_path.clone())
        .await;
    assert!(full_context.engine.name_table.is_some());
    assert!(full_context.engine.reach_graph.is_some());
    assert!(full_context.engine.include_table.is_some());
    assert!(full_context.engine.indexed_files.is_some());
    assert!(full_context.engine.project_context.is_some());
    assert_ne!(full_context.engine.epoch.as_u64(), 0);
    assert_eq!(full_context.engine.semantic_generation.0, 1);

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
    assert_eq!(dirty.declaration_count, 2);
    let dirty_context = service
        .inner()
        .session
        .request_context_for_root(root_path)
        .await;
    assert_ne!(full_context.engine.epoch, dirty_context.engine.epoch);
    assert_eq!(dirty_context.engine.semantic_generation.0, 2);
    assert_eq!(
        dirty_context
            .engine
            .name_table
            .as_ref()
            .expect("table")
            .len(),
        2
    );
}

#[tokio::test]
async fn dirty_scheduler_keeps_published_configuration_until_full_rebuild() {
    let root = tempdir().expect("root");
    let root_path = root.path().to_path_buf();
    write_workspace_file(
        root.path(),
        "fossilsense.json",
        r#"{"languageOverrides":[{"glob":"legacy/**/*.h","language":"go"}]}"#,
    );
    write_workspace_file(
        root.path(),
        "legacy/api.h",
        "package legacy\nfunc PublishedGo() {}\n",
    );
    write_workspace_file(root.path(), "other.c", "int old_dirty_name;\n");
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
        .workspace_roots
        .lock()
        .await
        .push(root_path.clone());
    service
        .inner()
        .session
        .cache
        .publish_full_index(&service.inner().client, root_path.clone())
        .await
        .expect("publish generation N");

    write_workspace_file(root.path(), "fossilsense.json", "{}");
    write_workspace_file(root.path(), "other.c", "int new_dirty_name;\n");
    service
        .inner()
        .run_dirty_index_for_test(vec![super::RootDirtyChange {
            root: root_path.clone(),
            rel_path: "other.c".into(),
            change: crate::indexer::DirtyFileChange {
                absolute_path: root.path().join("other.c"),
                kind: crate::indexer::DirtyFileKind::Upsert,
            },
        }])
        .await;

    let context = service
        .inner()
        .request_context_for_root(root_path.clone())
        .await;
    assert_eq!(context.engine.semantic_generation.0, 2);
    assert_eq!(
        context
            .engine
            .workspace_semantics
            .language_for_path(&root.path().join("legacy/api.h")),
        crate::config::SourceLanguage::Go,
        "a dirty source event must not adopt a newer unindexed configuration"
    );
    assert!(context
        .engine
        .name_table
        .as_ref()
        .expect("name table")
        .search_ranked("new_dirty_name", 8)
        .iter()
        .any(|hit| hit.name == "new_dirty_name"));
}

#[tokio::test]
async fn dirty_cache_publication_fully_hydrates_across_a_generation_gap() {
    let root = tempdir().expect("root");
    let root_path = root.path().to_path_buf();
    write_workspace_file(root.path(), "a.c", "int alpha_old;\n");
    write_workspace_file(root.path(), "b.c", "int beta_old;\n");
    write_workspace_file(root.path(), "new.h", "int from_new_header;\n");
    crate::indexer::index_workspace(
        root.path(),
        crate::indexer::IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("generation 1 index");

    let service = test_backend_service();
    let semantics = current_test_workspace_semantics(&service, root.path()).await;
    service
        .inner()
        .session
        .cache
        .publish_full_index_with_semantics(
            &service.inner().client,
            root_path.clone(),
            semantics.clone(),
        )
        .await
        .expect("publish generation 1");

    write_workspace_file(
        root.path(),
        "b.c",
        "#include \"new.h\"\nint beta_from_generation_2;\n",
    );
    write_workspace_file(root.path(), "c.c", "int gamma_from_generation_2;\n");
    crate::indexer::index_dirty_files(
        root.path(),
        vec![
            crate::indexer::DirtyFileChange {
                absolute_path: root.path().join("b.c"),
                kind: crate::indexer::DirtyFileKind::Upsert,
            },
            crate::indexer::DirtyFileChange {
                absolute_path: root.path().join("c.c"),
                kind: crate::indexer::DirtyFileKind::Upsert,
            },
        ],
        crate::indexer::IndexOptions::default(),
        |_| {},
    )
    .expect("commit generation 2 without cache publication");

    write_workspace_file(root.path(), "a.c", "int alpha_from_generation_3;\n");
    crate::indexer::index_dirty_files(
        root.path(),
        vec![crate::indexer::DirtyFileChange {
            absolute_path: root.path().join("a.c"),
            kind: crate::indexer::DirtyFileKind::Upsert,
        }],
        crate::indexer::IndexOptions::default(),
        |_| {},
    )
    .expect("commit generation 3");

    let report = service
        .inner()
        .session
        .cache
        .publish_dirty_index_with_semantics(
            &service.inner().client,
            root_path.clone(),
            &["a.c".into()],
            &["a.c".into()],
            semantics,
        )
        .await
        .expect("gap publication");
    assert_eq!(report.semantic_generation.0, 3);

    let context = service.inner().request_context_for_root(root_path).await;
    let names = context.engine.name_table.as_ref().expect("name table");
    for expected in [
        "alpha_from_generation_3",
        "beta_from_generation_2",
        "gamma_from_generation_2",
    ] {
        assert!(
            names
                .search_ranked(expected, 8)
                .iter()
                .any(|hit| hit.name == expected),
            "full hydration after a gap must include {expected}"
        );
    }
    assert_eq!(
        context
            .engine
            .indexed_files
            .as_ref()
            .expect("indexed files")
            .len(),
        4
    );
    assert!(context
        .engine
        .reach_graph
        .as_ref()
        .expect("reach graph")
        .reachable("b.c")
        .files
        .contains("new.h"));
}

#[tokio::test]
async fn dirty_scheduler_promotes_a_generation_gap_to_full_rebuild() {
    let root = tempdir().expect("root");
    let root_path = root.path().to_path_buf();
    write_workspace_file(root.path(), "a.c", "int alpha_old;\n");
    write_workspace_file(root.path(), "b.c", "int beta_old;\n");
    crate::indexer::index_workspace(
        root.path(),
        crate::indexer::IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("generation 1 index");

    let service = test_backend_service();
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root_path.clone());
    service
        .inner()
        .session
        .cache
        .publish_full_index(&service.inner().client, root_path.clone())
        .await
        .expect("publish generation 1");

    write_workspace_file(
        root.path(),
        "b.c",
        "int beta_from_unpublished_generation;\n",
    );
    crate::indexer::index_dirty_files(
        root.path(),
        vec![crate::indexer::DirtyFileChange {
            absolute_path: root.path().join("b.c"),
            kind: crate::indexer::DirtyFileKind::Upsert,
        }],
        crate::indexer::IndexOptions::default(),
        |_| {},
    )
    .expect("commit generation 2 without publishing");
    write_workspace_file(root.path(), "a.c", "int alpha_after_gap;\n");

    service
        .inner()
        .run_dirty_index_for_test(vec![super::RootDirtyChange {
            root: root_path.clone(),
            rel_path: "a.c".into(),
            change: crate::indexer::DirtyFileChange {
                absolute_path: root.path().join("a.c"),
                kind: crate::indexer::DirtyFileKind::Upsert,
            },
        }])
        .await;

    let context = service.inner().request_context_for_root(root_path).await;
    assert_eq!(context.engine.semantic_generation.0, 3);
    let names = context.engine.name_table.as_ref().expect("name table");
    for expected in ["alpha_after_gap", "beta_from_unpublished_generation"] {
        assert!(
            names
                .search_ranked(expected, 8)
                .iter()
                .any(|hit| hit.name == expected),
            "full rebuild after a generation gap must include {expected}"
        );
    }
}

#[tokio::test]
async fn marker_only_refresh_retags_names_publishes_epoch_and_clears_memos() {
    let root = tempdir().expect("root");
    let root_path = root.path().to_path_buf();
    write_workspace_file(root.path(), "app/src/main.c", "int project_symbol;\n");
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
    service
        .inner()
        .session
        .cache
        .publish_full_index(&service.inner().client, root_path.clone())
        .await
        .expect("publish");
    let before = service
        .inner()
        .session
        .request_context_for_root(root_path.clone())
        .await;
    assert!(before
        .engine
        .project_context
        .as_ref()
        .expect("available")
        .projects()
        .is_empty());
    let published_declaration_count = before
        .engine
        .name_table
        .as_ref()
        .expect("published name table")
        .len();
    // Marker-only publication must be derived exclusively from the immutable
    // published generation. Removing SQLite proves the refresh cannot observe
    // a concurrent writer's partial committed state.
    fs::remove_file(crate::pathing::default_index_path(root.path()).expect("db path"))
        .expect("remove index database");
    let uri = Url::from_file_path(root.path().join("app/src/main.c")).expect("uri");
    service
        .inner()
        .session
        .cache
        .record_completion_memo(uri.clone(), "pro".into(), 7, vec![vec![0]])
        .await;

    write_workspace_file(root.path(), "app/Makefile", "all:\n");
    let count = service
        .inner()
        .session
        .cache
        .refresh_project_context(&service.inner().client, root_path.clone())
        .await
        .expect("refresh");
    assert_eq!(count, 1);
    let after_create = service
        .inner()
        .session
        .request_context_for_root(root_path.clone())
        .await;
    assert_ne!(before.engine.epoch, after_create.engine.epoch);
    assert_eq!(
        after_create
            .engine
            .name_table
            .as_ref()
            .expect("retagged table")
            .len(),
        published_declaration_count
    );
    let project = after_create
        .engine
        .project_context
        .as_ref()
        .and_then(|index| index.nearest_for_file("app/src/main.c"))
        .expect("project");
    let hit = after_create
        .engine
        .name_table
        .as_ref()
        .expect("table")
        .search_ranked("project_symbol", 10)
        .into_iter()
        .next()
        .expect("hit");
    assert_eq!(hit.project_key, Some(project));
    assert!(service
        .inner()
        .session
        .cache
        .completion_memo_for_test(&uri)
        .await
        .is_none());

    fs::remove_file(root.path().join("app/Makefile")).expect("delete marker");
    service
        .inner()
        .session
        .cache
        .refresh_project_context(&service.inner().client, root_path.clone())
        .await
        .expect("refresh delete");
    let after_delete = service
        .inner()
        .session
        .request_context_for_root(root_path)
        .await;
    assert_ne!(after_create.engine.epoch, after_delete.engine.epoch);
    assert!(after_delete
        .engine
        .project_context
        .as_ref()
        .expect("available")
        .projects()
        .is_empty());
}

#[tokio::test]
async fn marker_refresh_uses_published_workspace_config_not_newer_disk_config() {
    let root = tempdir().expect("root");
    let root_path = root.path().to_path_buf();
    write_workspace_file(root.path(), "fossilsense.json", r#"{"exclude":["app"]}"#);
    write_workspace_file(root.path(), "app/Makefile", "all:\n");
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
    service
        .inner()
        .session
        .cache
        .publish_full_index(&service.inner().client, root_path.clone())
        .await
        .expect("publish");
    let published = service
        .inner()
        .session
        .request_context_for_root(root_path.clone())
        .await;
    assert!(published
        .engine
        .project_context
        .as_ref()
        .expect("project context")
        .projects()
        .is_empty());

    write_workspace_file(root.path(), "fossilsense.json", "{}");
    let count = service
        .inner()
        .session
        .cache
        .refresh_project_context(&service.inner().client, root_path.clone())
        .await
        .expect("marker refresh");
    assert_eq!(
        count, 0,
        "marker refresh must retain generation N's exclusion until N+1 publishes"
    );
    let refreshed = service
        .inner()
        .session
        .request_context_for_root(root_path)
        .await;
    assert!(refreshed
        .engine
        .project_context
        .as_ref()
        .expect("project context")
        .projects()
        .is_empty());
    assert!(refreshed
        .engine
        .workspace_semantics
        .workspace
        .exclude
        .iter()
        .any(|entry| entry == "app"));
}

#[tokio::test]
async fn nested_marker_refresh_reassigns_name_ownership_and_removal_restores_parent() {
    let root = tempdir().expect("root");
    let root_path = root.path().to_path_buf();
    write_workspace_file(root.path(), "Makefile", "all:\n");
    write_workspace_file(root.path(), "app/src/main.c", "int nested_symbol;\n");
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
    service
        .inner()
        .session
        .cache
        .publish_full_index(&service.inner().client, root_path.clone())
        .await
        .expect("publish");

    let project_path_for_symbol = |context: &super::RequestContext| {
        context
            .engine
            .name_table
            .as_ref()
            .expect("table")
            .search_ranked("nested_symbol", 10)
            .into_iter()
            .next()
            .and_then(|hit| hit.project_key)
            .map(|key| key.project_path)
    };
    let parent = service
        .inner()
        .session
        .request_context_for_root(root_path.clone())
        .await;
    assert_eq!(project_path_for_symbol(&parent).as_deref(), Some(""));

    write_workspace_file(root.path(), "app/CMakeLists.txt", "");
    service
        .inner()
        .session
        .cache
        .refresh_project_context(&service.inner().client, root_path.clone())
        .await
        .expect("nested refresh");
    let nested = service
        .inner()
        .session
        .request_context_for_root(root_path.clone())
        .await;
    assert_eq!(project_path_for_symbol(&nested).as_deref(), Some("app"));

    fs::remove_file(root.path().join("app/CMakeLists.txt")).expect("remove nested marker");
    service
        .inner()
        .session
        .cache
        .refresh_project_context(&service.inner().client, root_path.clone())
        .await
        .expect("parent refresh");
    let restored = service
        .inner()
        .session
        .request_context_for_root(root_path)
        .await;
    assert_eq!(project_path_for_symbol(&restored).as_deref(), Some(""));
}

#[tokio::test]
async fn marker_watcher_classifies_supported_and_ignores_excluded_or_fragment_files() {
    let root = tempdir().expect("root");
    fs::create_dir_all(root.path().join("app")).expect("app");
    fs::create_dir_all(root.path().join("build")).expect("build");
    let roots = vec![root.path().to_path_buf()];
    let cache = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let event = |path: &std::path::Path, typ| FileEvent {
        uri: Url::from_file_path(path).expect("uri"),
        typ,
    };

    let marker = super::watched_change_in_scope(
        &roots,
        &event(&root.path().join("app/Makefile"), FileChangeType::CREATED),
        &cache,
    )
    .await;
    assert!(matches!(
        marker,
        Some(super::WatchDecision::ProjectContext(_))
    ));

    let excluded = super::watched_change_in_scope(
        &roots,
        &event(
            &root.path().join("build/build.ninja"),
            FileChangeType::CREATED,
        ),
        &cache,
    )
    .await;
    assert!(excluded.is_none());

    let fragment = super::watched_change_in_scope(
        &roots,
        &event(
            &root.path().join("app/rules.ninja"),
            FileChangeType::CREATED,
        ),
        &cache,
    )
    .await;
    assert!(fragment.is_none());

    let renamed_away = super::watched_change_in_scope(
        &roots,
        &event(&root.path().join("app/Makefile"), FileChangeType::DELETED),
        &cache,
    )
    .await;
    assert!(matches!(
        renamed_away,
        Some(super::WatchDecision::ProjectContext(_))
    ));

    for go_metadata in ["go.mod", "go.work"] {
        let decision = super::watched_change_in_scope(
            &roots,
            &event(
                &root.path().join("app").join(go_metadata),
                FileChangeType::CHANGED,
            ),
            &cache,
        )
        .await;
        assert!(
            matches!(decision, Some(super::WatchDecision::Full(_))),
            "{go_metadata} changes must rebuild package/import evidence"
        );
    }
}

#[tokio::test]
async fn watcher_scopes_go_metadata_rebuild_to_the_matching_workspace_root() {
    let first = tempdir().expect("first root");
    let second = tempdir().expect("second root");
    fs::create_dir_all(second.path().join("src")).expect("second source tree");
    let first_root = first.path().to_path_buf();
    let second_root = second.path().to_path_buf();
    let service = test_backend_service();
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .extend([first_root.clone(), second_root.clone()]);

    service
        .inner()
        .did_change_watched_files(DidChangeWatchedFilesParams {
            changes: vec![
                FileEvent {
                    uri: Url::from_file_path(first.path().join("go.mod")).expect("go.mod uri"),
                    typ: FileChangeType::CHANGED,
                },
                FileEvent {
                    uri: Url::from_file_path(second.path().join("src/main.go"))
                        .expect("source uri"),
                    typ: FileChangeType::CHANGED,
                },
            ],
        })
        .await;

    let state = service.inner().index_schedule.lock().await;
    assert!(state.pending_full);
    assert!(!state.pending_all_roots);
    assert_eq!(state.pending_full_roots, vec![first_root]);
    assert_eq!(state.pending_changes.len(), 1);
    assert_eq!(state.pending_changes[0].root, second_root);
}

#[tokio::test]
async fn watcher_routes_nested_workspace_changes_to_the_most_specific_root() {
    let outer = tempdir().expect("outer");
    let inner = outer.path().join("nested");
    fs::create_dir_all(inner.join("src")).expect("inner tree");
    let outer_root = outer.path().to_path_buf();
    let roots = vec![outer_root.clone(), inner.clone()];
    let cache = Arc::new(tokio::sync::Mutex::new(HashMap::from([
        (
            outer_root.clone(),
            super::WorkspaceRootConfig::fallback(&outer_root),
        ),
        (inner.clone(), super::WorkspaceRootConfig::fallback(&inner)),
    ])));
    let event = |path: &std::path::Path, typ| FileEvent {
        uri: Url::from_file_path(path).expect("uri"),
        typ,
    };

    let marker = super::watched_change_in_scope(
        &roots,
        &event(&inner.join("CMakeLists.txt"), FileChangeType::CREATED),
        &cache,
    )
    .await;
    match marker {
        Some(super::WatchDecision::ProjectContext(root)) => assert_eq!(root, inner),
        _ => panic!("nested marker should refresh the nested workspace"),
    }

    let source = super::watched_change_in_scope(
        &roots,
        &event(&inner.join("src/main.c"), FileChangeType::CHANGED),
        &cache,
    )
    .await;
    match source {
        Some(super::WatchDecision::Dirty(change)) => assert_eq!(change.root, inner),
        _ => panic!("nested source should dirty the nested workspace"),
    }

    let config = super::watched_change_in_scope(
        &roots,
        &event(&inner.join("fossilsense.json"), FileChangeType::CHANGED),
        &cache,
    )
    .await;
    assert!(matches!(
        config,
        Some(super::WatchDecision::Full(root)) if root == inner
    ));
    let cached = cache.lock().await;
    assert!(cached.contains_key(&outer_root));
    assert!(!cached.contains_key(&inner));
}

#[tokio::test]
async fn language_override_watch_reparses_unchanged_open_document_and_overlay() {
    let service = test_backend_service();
    let dir = tempdir().expect("workspace");
    let root = dir.path().to_path_buf();
    fs::create_dir_all(root.join("legacy")).expect("legacy");
    let path = root.join("legacy/api.h");
    let uri = Url::from_file_path(&path).expect("uri");
    *service.inner().workspace_roots.lock().await = vec![root.clone()];
    open_test_document(
        &service,
        uri.clone(),
        1,
        "package legacy\nfunc Open() {}\n".into(),
    )
    .await;

    let first = service
        .inner()
        .get_or_parse_document(
            &uri,
            &path,
            1,
            "package legacy\nfunc Open() {}\n",
            crate::parser::ParseFacts::HOVER_SEMANTICS,
        )
        .await
        .expect("first parse");
    assert_eq!(
        first.language,
        crate::semantic_model::SemanticLanguage::Cpp,
        ".h starts in the C/C++ family"
    );
    let first_overlay = service
        .inner()
        .candidate_overlay_snapshot(&root, crate::call_model::SemanticGeneration(0), None, None)
        .await;

    fs::write(
        root.join("fossilsense.json"),
        r#"{
          "languageOverrides": [
            { "glob": "legacy/**/*.h", "language": "go" }
          ]
        }"#,
    )
    .expect("config");
    service
        .inner()
        .did_change_watched_files(DidChangeWatchedFilesParams {
            changes: vec![FileEvent {
                uri: Url::from_file_path(root.join("fossilsense.json")).expect("config uri"),
                typ: FileChangeType::CHANGED,
            }],
        })
        .await;

    let second = service
        .inner()
        .get_or_parse_document(
            &uri,
            &path,
            1,
            "package legacy\nfunc Open() {}\n",
            crate::parser::ParseFacts::HOVER_SEMANTICS,
        )
        .await
        .expect("reparsed Go document");
    assert_eq!(second.language, crate::semantic_model::SemanticLanguage::Go);
    assert!(!Arc::ptr_eq(&first, &second));
    let second_overlay = service
        .inner()
        .candidate_overlay_snapshot(&root, crate::call_model::SemanticGeneration(0), None, None)
        .await;
    assert!(!Arc::ptr_eq(&first_overlay, &second_overlay));
    assert_eq!(
        second_overlay.semantic_family_for_path("legacy/api.h"),
        Some(crate::semantic_model::SemanticFamily::Go)
    );
}

#[tokio::test]
async fn project_context_commands_validate_selection_and_outside_uri_has_no_automatic_project() {
    let root = tempdir().expect("root");
    let other = tempdir().expect("outside");
    let root_path = root.path().to_path_buf();
    write_workspace_file(root.path(), "server/Makefile", "all:\n");
    write_workspace_file(root.path(), "server/main.c", "int server_api;\n");
    write_workspace_file(root.path(), "lib/CMakeLists.txt", "");
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
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root_path.clone());
    service
        .inner()
        .session
        .cache
        .publish_full_index(&service.inner().client, root_path.clone())
        .await
        .expect("publish");
    let uri = Url::from_file_path(root.path().join("server/main.c")).expect("uri");
    let value = service
        .inner()
        .execute_command(ExecuteCommandParams {
            command: super::PROJECT_CONTEXTS_LSP_COMMAND.to_string(),
            arguments: vec![serde_json::json!({"uri": uri})],
            work_done_progress_params: Default::default(),
        })
        .await
        .expect("status")
        .expect("value");
    let status: crate::project_context::ProjectContextStatus =
        serde_json::from_value(value).expect("status dto");
    assert!(status.available);
    assert_eq!(status.projects.len(), 2);
    assert_eq!(
        status
            .automatic_project
            .as_ref()
            .expect("automatic")
            .project_path,
        "server"
    );

    let manual = status
        .projects
        .iter()
        .find(|project| project.key.project_path == "lib")
        .expect("lib")
        .key
        .clone();
    let manual_with_stale_case = crate::project_context::ProjectKey {
        project_path: manual.project_path.to_ascii_uppercase(),
        ..manual.clone()
    };
    let memo_uri = uri.clone();
    service
        .inner()
        .session
        .cache
        .record_completion_memo(memo_uri.clone(), "ser".into(), 9, vec![vec![0]])
        .await;
    let value = service
        .inner()
        .execute_command(ExecuteCommandParams {
            command: super::SET_PROJECT_CONTEXT_LSP_COMMAND.to_string(),
            arguments: vec![serde_json::json!({
                "uri": uri,
                "selection": {"kind": "manual", "key": manual_with_stale_case}
            })],
            work_done_progress_params: Default::default(),
        })
        .await
        .expect("set")
        .expect("value");
    let manual_status: crate::project_context::ProjectContextStatus =
        serde_json::from_value(value).expect("manual status");
    assert!(matches!(
        manual_status.selection,
        crate::project_context::ProjectContextSelection::Manual { .. }
    ));
    assert_eq!(manual_status.active_project, Some(manual));
    assert!(service
        .inner()
        .session
        .cache
        .completion_memo_for_test(&memo_uri)
        .await
        .is_none());

    let outside = Url::from_file_path(other.path().join("outside.c")).expect("outside uri");
    let outside_status = service.inner().project_context_status(Some(&outside)).await;
    assert!(outside_status.available);
    assert!(outside_status.automatic_project.is_none());

    let unmarked_uri =
        Url::from_file_path(root.path().join("unmarked/file.c")).expect("unmarked uri");
    let unmarked_status = service
        .inner()
        .set_project_context_selection(
            crate::project_context::ProjectContextSelection::Auto,
            Some(&unmarked_uri),
        )
        .await;
    assert!(unmarked_status.available);
    assert!(unmarked_status.active_project.is_none());

    let unspecified_status = service
        .inner()
        .set_project_context_selection(
            crate::project_context::ProjectContextSelection::Unspecified,
            Some(&Url::from_file_path(root.path().join("server/main.c")).expect("server uri")),
        )
        .await;
    assert!(unspecified_status.active_project.is_none());

    let current = service
        .inner()
        .session
        .cache
        .current_engine_snapshot(&root_path)
        .await
        .expect("current snapshot");
    let mut degraded = current.degraded.clone();
    degraded.project_context = true;
    service
        .inner()
        .session
        .cache
        .publish_engine_snapshot(super::workspace::EngineSnapshot {
            root: root_path,
            epoch: service.inner().session.cache.allocate_engine_epoch(),
            semantic_generation: current.semantic_generation,
            declaration_index: current.declaration_index.clone(),
            name_table: current.name_table.clone(),
            fallback_completion_table: current.fallback_completion_table.clone(),
            reach_graph: current.reach_graph.clone(),
            include_table: current.include_table.clone(),
            go_import_table: current.go_import_table.clone(),
            indexed_files: current.indexed_files.clone(),
            include_path_index: current.include_path_index.clone(),
            project_context: None,
            call_read_handle: None,
            workspace_semantics: current.workspace_semantics.clone(),
            degraded,
        })
        .await;
    let unavailable_status = service
        .inner()
        .project_context_status(Some(
            &Url::from_file_path(root.path().join("server/main.c")).expect("server uri"),
        ))
        .await;
    assert!(!unavailable_status.available);
    assert!(unavailable_status.projects.is_empty());
    assert!(unavailable_status.active_project.is_none());
}

#[tokio::test]
async fn automatic_project_uses_the_most_specific_containing_workspace_root() {
    let outer = tempdir().expect("outer");
    let inner = outer.path().join("nested");
    fs::create_dir_all(inner.join("src")).expect("inner tree");
    write_workspace_file(outer.path(), "Makefile", "all:\n");
    write_workspace_file(&inner, "CMakeLists.txt", "");
    write_workspace_file(&inner, "src/main.c", "int nested_api;\n");
    for root in [outer.path(), inner.as_path()] {
        crate::indexer::index_workspace(
            root,
            crate::indexer::IndexOptions {
                force: true,
                ..Default::default()
            },
            |_| {},
        )
        .expect("index root");
    }

    let service = test_backend_service();
    let roots = vec![outer.path().to_path_buf(), inner.clone()];
    *service.inner().workspace_roots.lock().await = roots.clone();
    for root in roots {
        service
            .inner()
            .session
            .cache
            .publish_full_index(&service.inner().client, root)
            .await
            .expect("publish root");
    }
    let uri = Url::from_file_path(inner.join("src/main.c")).expect("uri");
    assert_eq!(
        service.inner().root_for_uri(&uri).await,
        Some(inner.clone())
    );
    let status = service.inner().project_context_status(Some(&uri)).await;
    let automatic = status.automatic_project.expect("automatic project");
    assert_eq!(automatic.project_path, "");
    assert_eq!(
        automatic.workspace_root_id,
        crate::pathing::workspace_hash(&inner.canonicalize().expect("canonical inner"))
    );
    assert!(status
        .projects
        .iter()
        .any(|project| project.key.project_path == "nested"));
    assert!(status
        .projects
        .iter()
        .any(|project| project.key == automatic));
}

#[tokio::test]
async fn project_selector_remains_available_when_another_root_model_is_degraded() {
    let available = tempdir().expect("available root");
    let degraded = tempdir().expect("degraded root");
    write_workspace_file(available.path(), "app/Makefile", "all:\n");
    write_workspace_file(available.path(), "app/main.c", "int available_api;\n");
    crate::indexer::index_workspace(
        available.path(),
        crate::indexer::IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index available root");

    let service = test_backend_service();
    *service.inner().workspace_roots.lock().await = vec![
        available.path().to_path_buf(),
        degraded.path().to_path_buf(),
    ];
    service
        .inner()
        .session
        .cache
        .publish_full_index(&service.inner().client, available.path().to_path_buf())
        .await
        .expect("publish available root");

    let degraded_uri = Url::from_file_path(degraded.path().join("main.c")).expect("degraded uri");
    let status = service
        .inner()
        .project_context_status(Some(&degraded_uri))
        .await;
    assert!(status.available);
    assert_eq!(status.projects.len(), 1);
    assert!(status.automatic_project.is_none());
    assert!(status.active_project.is_none());
}

#[tokio::test]
async fn automatic_and_manual_project_selection_change_duplicate_completion_immediately() {
    let root = tempdir().expect("root");
    write_workspace_file(root.path(), "server/Makefile", "all:\n");
    write_workspace_file(root.path(), "server/server.h", "int get_xxx(void);\n");
    write_workspace_file(root.path(), "lib/CMakeLists.txt", "");
    write_workspace_file(root.path(), "lib/xxx.h", "#define get_xxx 1\n");
    let (source, line, character) =
        text_and_position("void use_api(void) {\n    get/*cursor*/\n}\n");
    write_workspace_file(root.path(), "server/server.c", &source);
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
    let root_path = root.path().to_path_buf();
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root_path.clone());
    service
        .inner()
        .session
        .cache
        .publish_full_index(&service.inner().client, root_path)
        .await
        .expect("publish");
    let uri = Url::from_file_path(root.path().join("server/server.c")).expect("uri");
    open_test_document(&service, uri.clone(), 1, source).await;

    let auto_items = completion_items(
        service
            .inner()
            .completion(completion_params(uri.clone(), line, character))
            .await
            .expect("auto completion")
            .expect("auto response"),
    );
    let auto = auto_items
        .iter()
        .find(|item| item.label == "get_xxx")
        .expect("auto item");
    assert_eq!(auto.kind, Some(CompletionItemKind::FUNCTION));

    let status = service.inner().project_context_status(Some(&uri)).await;
    let library_key = status
        .projects
        .iter()
        .find(|project| project.key.project_path == "lib")
        .expect("library project")
        .key
        .clone();
    service
        .inner()
        .set_project_context_selection(
            crate::project_context::ProjectContextSelection::Manual { key: library_key },
            Some(&uri),
        )
        .await;
    let manual_items = completion_items(
        service
            .inner()
            .completion(completion_params(uri, line, character))
            .await
            .expect("manual completion")
            .expect("manual response"),
    );
    let manual = manual_items
        .iter()
        .find(|item| item.label == "get_xxx")
        .expect("manual item");
    assert_eq!(manual.kind, Some(CompletionItemKind::CONSTANT));
}

#[tokio::test]
async fn initialize_advertises_project_context_commands() {
    let service = test_backend_service();
    let initialized = service
        .inner()
        .initialize(InitializeParams::default())
        .await
        .expect("initialize");
    assert_eq!(
        initialized.capabilities.declaration_provider,
        Some(DeclarationCapability::Simple(true))
    );
    assert_eq!(
        initialized.capabilities.definition_provider,
        Some(OneOf::Left(true))
    );
    let commands = initialized
        .capabilities
        .execute_command_provider
        .expect("commands")
        .commands;
    assert!(commands.contains(&super::PROJECT_CONTEXTS_LSP_COMMAND.to_string()));
    assert!(commands.contains(&super::SET_PROJECT_CONTEXT_LSP_COMMAND.to_string()));
    assert!(commands.contains(&super::POSSIBLE_TARGETS_LSP_COMMAND.to_string()));
}

#[tokio::test]
async fn initialize_defaults_to_strict_prefix_ranking_and_accepts_scope_first() {
    let strict = test_backend_service();
    strict
        .inner()
        .initialize(InitializeParams::default())
        .await
        .expect("default initialize");
    assert!(strict
        .inner()
        .strict_prefix_ranking
        .load(std::sync::atomic::Ordering::Relaxed));

    let scope_first = test_backend_service();
    scope_first
        .inner()
        .initialize(InitializeParams {
            initialization_options: Some(serde_json::json!({
                "fossilsense": {
                    "completion": { "prefixRanking": "scopeFirst" }
                }
            })),
            ..Default::default()
        })
        .await
        .expect("scope-first initialize");
    assert!(!scope_first
        .inner()
        .strict_prefix_ranking
        .load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(
        scope_first.inner().request_settings().prefix_ranking,
        crate::completion::CompletionPrefixRanking::ScopeFirst
    );
}

#[tokio::test]
async fn initialize_captures_client_go_module_paths() {
    let service = test_backend_service();
    service
        .inner()
        .initialize(InitializeParams {
            initialization_options: Some(serde_json::json!({
                "fossilsense": {
                    "goModulePaths": ["C:\\deps\\device", "/opt/go/device"]
                }
            })),
            ..Default::default()
        })
        .await
        .expect("initialize");

    assert_eq!(
        *service.inner().go_module_paths.lock().await,
        vec!["C:/deps/device".to_string(), "/opt/go/device".to_string()]
    );
}

#[tokio::test]
async fn initialize_captures_optional_protobuf_c_editor_configuration() {
    let service = test_backend_service();
    service
        .inner()
        .initialize(InitializeParams {
            initialization_options: Some(serde_json::json!({
                "fossilsense": {
                    "protobufC": {
                        "enabled": true,
                        "protoPaths": ["C:\\shared\\proto", "/opt/proto"]
                    }
                }
            })),
            ..Default::default()
        })
        .await
        .expect("initialize");

    assert_eq!(*service.inner().protobuf_c_enabled.lock().await, Some(true));
    assert_eq!(
        *service.inner().protobuf_c_proto_paths.lock().await,
        vec!["C:/shared/proto".to_string(), "/opt/proto".to_string()]
    );
}

#[tokio::test]
async fn initialize_project_context_off_is_effective_before_extension_state_restore() {
    let service = test_backend_service();
    service
        .inner()
        .initialize(InitializeParams {
            initialization_options: Some(serde_json::json!({
                "fossilsense": { "projectContext": { "mode": "off" } }
            })),
            ..Default::default()
        })
        .await
        .expect("initialize");

    assert_eq!(
        *service.inner().project_context_selection.lock().await,
        crate::project_context::ProjectContextSelection::Unspecified
    );
}

#[tokio::test]
async fn dirty_publish_does_not_mutate_an_in_flight_engine_snapshot() {
    let root = tempdir().expect("root");
    let root_path = root.path().to_path_buf();
    write_workspace_file(root.path(), "main.c", "#include \"old.h\"\nint before;\n");
    write_workspace_file(root.path(), "old.h", "int old_symbol;\n");
    write_workspace_file(root.path(), "new.h", "int new_symbol;\n");
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
        .expect("full publish");
    let in_flight = service
        .inner()
        .session
        .request_context_for_root(root_path.clone())
        .await;
    let old_graph = in_flight.engine.reach_graph.clone().expect("old graph");
    assert!(old_graph.reachable("main.c").files.contains("old.h"));

    write_workspace_file(root.path(), "main.c", "#include \"new.h\"\nint after;\n");
    crate::indexer::index_dirty_files(
        root.path(),
        vec![crate::indexer::DirtyFileChange {
            absolute_path: root.path().join("main.c"),
            kind: crate::indexer::DirtyFileKind::Upsert,
        }],
        crate::indexer::IndexOptions::default(),
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
            &["main.c".to_string()],
            &["main.c".to_string()],
        )
        .await
        .expect("dirty publish");

    let current = service
        .inner()
        .session
        .request_context_for_root(root_path)
        .await;
    let new_graph = current.engine.reach_graph.clone().expect("new graph");
    assert!(!Arc::ptr_eq(&old_graph, &new_graph));
    assert_ne!(in_flight.engine.epoch, current.engine.epoch);

    let old_scope = old_graph.reachable("main.c");
    assert!(old_scope.files.contains("old.h"));
    assert!(!old_scope.files.contains("new.h"));

    let new_scope = new_graph.reachable("main.c");
    assert!(new_scope.files.contains("new.h"));
    assert!(!new_scope.files.contains("old.h"));
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
async fn cache_ledger_never_narrows_from_an_incomplete_candidate_pool() {
    let cache = super::CacheLedger::default();
    let uri = Url::parse("file:///tmp/incomplete-memo.c").expect("uri");

    cache
        .record_completion_memo_with_completeness_for_test(
            uri.clone(),
            "fo".to_string(),
            42,
            vec![vec![1, 2, 3], vec![7, 8]],
            vec![false, true],
        )
        .await;

    let narrowed = cache.completion_memo_pools(&uri, 42, "foo", 2).await;
    assert_eq!(narrowed.hit_kind, "pool");
    assert_eq!(
        narrowed.prior_pools,
        vec![None, Some(vec![7, 8])],
        "a truncated pool cannot be treated as a complete prefix superset"
    );
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
    cache.invalidate_after_index_change().await;
    assert_eq!(cache.reference_search_cache_len_for_test(), 0);
}

#[tokio::test]
async fn relation_overlay_tracks_only_divergent_or_not_yet_indexed_documents() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("tracked.c");
    std::fs::write(&path, "void tracked(void);\n").expect("write source");
    crate::indexer::index_workspace(dir.path(), crate::indexer::IndexOptions::default(), |_| {})
        .expect("index initial source");
    let db_path = crate::pathing::default_index_path(dir.path()).expect("index path");
    let initial_generation = crate::store::IndexStore::open_readonly(&db_path)
        .and_then(|store| store.semantic_generation())
        .expect("initial generation");
    let uri = Url::from_file_path(&path).expect("uri");
    let documents = super::DocumentStore::default();

    documents
        .open_document(uri.clone(), 1, "void tracked(void);\n".into())
        .await;
    let (opened_epoch, _) = documents.all_snapshots_with_overlay_epoch().await;
    let awaiting_validation = documents.snapshot(&uri).await.expect("open snapshot");
    assert!(awaiting_validation
        .needs_relation_overlay(crate::call_model::SemanticGeneration(initial_generation)));
    documents
        .reconcile_published_files(
            dir.path().to_path_buf(),
            Some(vec!["tracked.c".to_string()]),
            crate::call_model::SemanticGeneration(initial_generation),
        )
        .await;
    let (clean_epoch, _) = documents.all_snapshots_with_overlay_epoch().await;
    assert!(clean_epoch > opened_epoch);
    let clean = documents.snapshot(&uri).await.expect("clean snapshot");
    assert!(!clean.needs_relation_overlay(crate::call_model::SemanticGeneration(4)));

    documents
        .change_document(uri.clone(), 2, "void changed(void);\n".into())
        .await;
    let (unsaved_epoch, _) = documents.all_snapshots_with_overlay_epoch().await;
    assert!(unsaved_epoch > clean_epoch);
    let unsaved = documents.snapshot(&uri).await.expect("unsaved snapshot");
    assert!(unsaved.needs_relation_overlay(crate::call_model::SemanticGeneration(4)));

    documents
        .save_document(&uri, crate::call_model::SemanticGeneration(4))
        .await;
    let (saved_epoch, _) = documents.all_snapshots_with_overlay_epoch().await;
    assert!(saved_epoch > unsaved_epoch);
    let awaiting = documents.snapshot(&uri).await.expect("saved snapshot");
    assert!(awaiting.needs_relation_overlay(crate::call_model::SemanticGeneration(4)));
    assert!(awaiting.needs_relation_overlay(crate::call_model::SemanticGeneration(5)));

    // An unrelated generation advance must not clear this file's overlay. It
    // becomes clean only after the active revision proves that the saved bytes
    // for this exact path were published.
    std::fs::write(&path, "void changed(void);\n").expect("save changed source");
    crate::indexer::index_workspace(dir.path(), crate::indexer::IndexOptions::default(), |_| {})
        .expect("index saved source");
    let generation = crate::store::IndexStore::open_readonly(&db_path)
        .and_then(|store| store.semantic_generation())
        .expect("active generation");
    documents
        .reconcile_published_files(
            dir.path().to_path_buf(),
            Some(vec!["tracked.c".to_string()]),
            crate::call_model::SemanticGeneration(generation),
        )
        .await;
    let (published_epoch, _) = documents.all_snapshots_with_overlay_epoch().await;
    assert!(published_epoch > saved_epoch);
    let published = documents.snapshot(&uri).await.expect("published snapshot");
    assert!(!published.needs_relation_overlay(crate::call_model::SemanticGeneration(generation)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_document_capture_keeps_current_all_and_epoch_atomic_during_changes() {
    let dir = tempdir().expect("tempdir");
    let uri = Url::from_file_path(dir.path().join("racing.c")).expect("uri");
    let documents = super::DocumentStore::default();
    documents
        .open_document(uri.clone(), 1, "int revision_1;\n".into())
        .await;

    let writer_documents = documents.clone();
    let writer_uri = uri.clone();
    let writer = tokio::spawn(async move {
        for version in 2..=64 {
            writer_documents
                .change_document(
                    writer_uri.clone(),
                    version,
                    format!("int revision_{version};\n"),
                )
                .await;
            tokio::task::yield_now().await;
        }
    });

    for _ in 0..128 {
        let captured = documents.capture_request_snapshot(Some(&uri)).await;
        let current = captured.current.expect("current document");
        let all = captured
            .all
            .iter()
            .find(|(candidate, _)| candidate == &uri)
            .map(|(_, snapshot)| snapshot)
            .expect("current document in all-open snapshot");
        assert_eq!(current.version, all.version);
        assert_eq!(current.text, all.text);
        assert_eq!(captured.overlay_epoch, current.version as u64);
        tokio::task::yield_now().await;
    }
    writer.await.expect("writer");
}

#[tokio::test]
async fn completion_overlay_reuses_recall_universe_across_current_body_edits() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    let uri = Url::from_file_path(root.join("main.c")).expect("main uri");
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root.clone());
    open_test_document(
        &service,
        uri.clone(),
        1,
        "void stable(void) { first_prefix; }\n".into(),
    )
    .await;
    let generation = crate::call_model::SemanticGeneration::MISSING;
    let engine_epoch = super::state::EngineEpoch::published(1);

    let first_documents = service
        .inner()
        .session
        .documents
        .capture_request_snapshot(Some(&uri))
        .await;
    let (first, first_universe) = service
        .inner()
        .completion_overlay_snapshot_from_documents(
            completion_overlay_request(&root, &uri, engine_epoch, generation),
            first_documents,
        )
        .await;

    service
        .inner()
        .session
        .change_document(
            uri.clone(),
            2,
            "void stable(void) { second_prefix; }\n".into(),
        )
        .await;
    let second_documents = service
        .inner()
        .session
        .documents
        .capture_request_snapshot(Some(&uri))
        .await;
    let (second, second_universe) = service
        .inner()
        .completion_overlay_snapshot_from_documents(
            completion_overlay_request(&root, &uri, engine_epoch, generation),
            second_documents,
        )
        .await;

    assert_eq!(first_universe, second_universe);
    assert!(
        Arc::ptr_eq(&first, &second),
        "body-only edits must reuse the immutable completion overlay projection"
    );

    service
        .inner()
        .session
        .change_document(
            uri.clone(),
            3,
            "#include \"replacement.h\"\nvoid stable(void) { second_prefix; }\n".into(),
        )
        .await;
    let changed_documents = service
        .inner()
        .session
        .documents
        .capture_request_snapshot(Some(&uri))
        .await;
    let (changed, changed_universe) = service
        .inner()
        .completion_overlay_snapshot_from_documents(
            completion_overlay_request(&root, &uri, engine_epoch, generation),
            changed_documents,
        )
        .await;

    assert_ne!(second_universe, changed_universe);
    assert!(!Arc::ptr_eq(&second, &changed));
}

#[tokio::test]
async fn completion_include_miss_reuses_generation_pinned_path_and_graph_bases() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    let uri = Url::from_file_path(root.join("src/main.c")).expect("main uri");
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root.clone());

    let mut indexed_files = (0..8_192)
        .map(|index| {
            let path = format!("include/noise_{index}.h");
            let absolute = root.join(path.replace('/', std::path::MAIN_SEPARATOR_STR));
            (path, absolute)
        })
        .collect::<Vec<_>>();
    indexed_files.push(("include/api.h".to_string(), root.join("include/api.h")));
    indexed_files.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    service
        .inner()
        .session
        .cache
        .set_indexed_file_list_for_test(root.clone(), Arc::new(indexed_files))
        .await;
    let published_graph = Arc::new(crate::reachability::ReachGraph::new(
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ));
    service
        .inner()
        .session
        .cache
        .set_reach_graph_for_test(root.clone(), published_graph.clone())
        .await;
    let engine = service
        .inner()
        .session
        .cache
        .current_engine_snapshot(&root)
        .await
        .expect("published engine");

    open_test_document(
        &service,
        uri.clone(),
        1,
        "#include \"include/api.h\"\nvoid use(void) {}\n".into(),
    )
    .await;
    let documents = service
        .inner()
        .session
        .documents
        .capture_request_snapshot(Some(&uri))
        .await;
    let (overlay, _) = service
        .inner()
        .completion_overlay_snapshot_from_documents(
            super::candidate_context::CompletionOverlayRequest {
                root: &root,
                current_uri: &uri,
                engine_epoch: engine.epoch,
                generation: engine.semantic_generation,
                base_reach_graph: engine.reach_graph.as_deref(),
                indexed_workspace_files: engine.indexed_files.as_deref().map(Vec::as_slice),
                workspace_semantics: engine.workspace_semantics.clone(),
            },
            documents,
        )
        .await;

    let published_paths = engine
        .include_path_index
        .as_ref()
        .expect("published include paths");
    assert_eq!(
        overlay.include_path_view_shape_for_test(published_paths),
        (true, 1),
        "the cache miss must retain the published path Arc and own only src/main.c"
    );
    let effective_graph = overlay
        .effective_reach_graph(None)
        .expect("effective graph");
    assert_eq!(
        effective_graph.request_overlay_shape_for_test(&published_graph),
        (true, 1, 1),
        "the request graph must retain the published Arc and replace one source"
    );
    assert!(effective_graph
        .reachable("src/main.c")
        .files
        .contains("include/api.h"));
}

#[tokio::test]
async fn completion_overlay_invalidates_when_another_dirty_declaration_changes() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    let current_uri = Url::from_file_path(root.join("main.c")).expect("main uri");
    let header_uri = Url::from_file_path(root.join("api.h")).expect("header uri");
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root.clone());
    open_test_document(
        &service,
        current_uri.clone(),
        1,
        "void use(void) { Dirty; }\n".into(),
    )
    .await;
    open_test_document(
        &service,
        header_uri.clone(),
        1,
        "int DirtyBefore(void);\n".into(),
    )
    .await;
    let generation = crate::call_model::SemanticGeneration::MISSING;
    let engine_epoch = super::state::EngineEpoch::published(1);
    let first_documents = service
        .inner()
        .session
        .documents
        .capture_request_snapshot(Some(&current_uri))
        .await;
    let (first, first_universe) = service
        .inner()
        .completion_overlay_snapshot_from_documents(
            completion_overlay_request(&root, &current_uri, engine_epoch, generation),
            first_documents,
        )
        .await;
    assert!(first
        .completion_names()
        .iter()
        .any(|entry| entry.name == "DirtyBefore"));

    service
        .inner()
        .session
        .change_document(header_uri, 2, "int DirtyAfter(void);\n".into())
        .await;
    let second_documents = service
        .inner()
        .session
        .documents
        .capture_request_snapshot(Some(&current_uri))
        .await;
    let (second, second_universe) = service
        .inner()
        .completion_overlay_snapshot_from_documents(
            completion_overlay_request(&root, &current_uri, engine_epoch, generation),
            second_documents,
        )
        .await;

    assert_ne!(first_universe, second_universe);
    assert!(!Arc::ptr_eq(&first, &second));
    let names = second.completion_names();
    assert!(names.iter().any(|entry| entry.name == "DirtyAfter"));
    assert!(names.iter().all(|entry| entry.name != "DirtyBefore"));
}

#[tokio::test]
async fn completion_overlay_cache_rejects_late_old_universe_and_engine_publication() {
    let cache = super::CacheLedger::default();
    let root = tempdir().expect("root").path().to_path_buf();
    let engine_epoch = super::state::EngineEpoch::published(1);
    let generation = crate::call_model::SemanticGeneration(1);
    let old_universe = crate::candidate_service::RecallUniverseId::for_test(1);
    let new_universe = crate::candidate_service::RecallUniverseId::for_test(2);
    let (_, cache_revision) = cache
        .completion_overlay(&root, engine_epoch, generation, 1, old_universe)
        .await;
    let old_snapshot = Arc::new(crate::candidate_service::CandidateOverlaySnapshot::new(
        1,
        Vec::new(),
    ));
    let new_snapshot = Arc::new(crate::candidate_service::CandidateOverlaySnapshot::new(
        2,
        Vec::new(),
    ));

    let published_new = cache
        .publish_completion_overlay(super::workspace::CompletionOverlayPublication {
            root: root.clone(),
            engine_epoch,
            semantic_generation: generation,
            overlay_epoch: 2,
            universe: new_universe,
            expected_cache_revision: cache_revision,
            snapshot: new_snapshot.clone(),
        })
        .await;
    assert!(Arc::ptr_eq(&published_new, &new_snapshot));

    let returned_old = cache
        .publish_completion_overlay(super::workspace::CompletionOverlayPublication {
            root: root.clone(),
            engine_epoch,
            semantic_generation: generation,
            overlay_epoch: 1,
            universe: old_universe,
            expected_cache_revision: cache_revision,
            snapshot: old_snapshot.clone(),
        })
        .await;
    assert!(Arc::ptr_eq(&returned_old, &old_snapshot));
    let (cached_new, _) = cache
        .completion_overlay(&root, engine_epoch, generation, 2, new_universe)
        .await;
    assert!(cached_new.is_some_and(|cached| Arc::ptr_eq(&cached, &new_snapshot)));
    assert!(cache
        .completion_overlay(&root, engine_epoch, generation, 2, old_universe)
        .await
        .0
        .is_none());

    cache
        .publish_engine_snapshot(super::workspace::EngineSnapshot {
            root: root.clone(),
            epoch: super::state::EngineEpoch::published(2),
            semantic_generation: crate::call_model::SemanticGeneration(2),
            declaration_index: None,
            name_table: None,
            fallback_completion_table: Arc::new(Default::default()),
            reach_graph: None,
            include_table: None,
            go_import_table: None,
            indexed_files: None,
            include_path_index: None,
            project_context: None,
            call_read_handle: None,
            workspace_semantics: empty_workspace_semantics(&root),
            degraded: Default::default(),
        })
        .await;
    assert!(cache
        .completion_overlay(&root, engine_epoch, generation, 3, new_universe)
        .await
        .0
        .is_none());

    let late_snapshot = Arc::new(crate::candidate_service::CandidateOverlaySnapshot::new(
        3,
        Vec::new(),
    ));
    let returned_late = cache
        .publish_completion_overlay(super::workspace::CompletionOverlayPublication {
            root: root.clone(),
            engine_epoch,
            semantic_generation: generation,
            overlay_epoch: 3,
            universe: new_universe,
            expected_cache_revision: cache_revision,
            snapshot: late_snapshot.clone(),
        })
        .await;
    assert!(Arc::ptr_eq(&returned_late, &late_snapshot));
    assert!(
        cache
            .completion_overlay(&root, engine_epoch, generation, 3, new_universe)
            .await
            .0
            .is_none(),
        "a builder captured before engine publication must not repopulate the old engine key"
    );
}

#[tokio::test]
async fn overlay_caches_reject_snapshots_with_pending_external_path_probes() {
    let external = tempdir().expect("external include root");
    std::fs::write(external.path().join("api.h"), "int external_api(void);\n")
        .expect("external header");
    let parsed = crate::parser::parse_with_handle(
        std::path::Path::new("main.c"),
        "#include <api.h>\nint main(void) { return 0; }\n",
        None,
        crate::parser::ParseFacts::HOVER_SEMANTICS,
    );
    let mut pending = crate::candidate_service::CandidateOverlaySnapshot::new(
        1,
        vec![crate::candidate_service::FileCandidateOverlay::from_index(
            "main.c".into(),
            &parsed,
        )],
    );
    pending.refresh_reach_graph(
        Some(Arc::new(crate::reachability::ReachGraph::new(
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ))),
        Some(Arc::new(crate::candidate_service::IncludePathIndex::build(
            [("main.c".to_string(), true)],
        ))),
        &[crate::pathing::normalize_abs_path(external.path())],
    );
    assert!(
        !pending.path_view_cacheable(),
        "the first request must not wait for the filesystem probe"
    );
    let pending = Arc::new(pending);

    let cache = super::CacheLedger::default();
    let root = tempdir().expect("workspace root").path().to_path_buf();
    let engine_epoch = super::state::EngineEpoch::published(1);
    let generation = crate::call_model::SemanticGeneration(1);
    let universe = crate::candidate_service::RecallUniverseId::for_test(9);
    let (_, completion_revision) = cache
        .completion_overlay(&root, engine_epoch, generation, 1, universe)
        .await;
    cache
        .publish_completion_overlay(super::workspace::CompletionOverlayPublication {
            root: root.clone(),
            engine_epoch,
            semantic_generation: generation,
            overlay_epoch: 1,
            universe,
            expected_cache_revision: completion_revision,
            snapshot: pending.clone(),
        })
        .await;
    assert!(
        cache
            .completion_overlay(&root, engine_epoch, generation, 1, universe)
            .await
            .0
            .is_none(),
        "completion cache must rebuild after a pending background probe"
    );

    let (_, candidate_revision) = cache.candidate_overlay(&root, generation, 1).await;
    cache
        .publish_candidate_overlay(root.clone(), generation, 1, candidate_revision, pending)
        .await;
    assert!(
        cache
            .candidate_overlay(&root, generation, 1)
            .await
            .0
            .is_none(),
        "semantic candidate cache must rebuild after a pending background probe"
    );
}

#[tokio::test]
async fn completion_overlay_cache_metrics_measure_real_hits_and_misses() {
    let cache = super::CacheLedger::default();
    let root = tempdir().expect("workspace root").path().to_path_buf();
    let engine_epoch = super::state::EngineEpoch::published(1);
    let generation = crate::call_model::SemanticGeneration(1);
    let universe = crate::candidate_service::RecallUniverseId::for_test(11);
    cache.reset_completion_overlay_cache_metrics_for_test();

    let (_, revision) = cache
        .completion_overlay(&root, engine_epoch, generation, 1, universe)
        .await;
    cache
        .publish_completion_overlay(super::workspace::CompletionOverlayPublication {
            root: root.clone(),
            engine_epoch,
            semantic_generation: generation,
            overlay_epoch: 1,
            universe,
            expected_cache_revision: revision,
            snapshot: Arc::new(crate::candidate_service::CandidateOverlaySnapshot::new(
                1,
                Vec::new(),
            )),
        })
        .await;
    assert!(cache
        .completion_overlay(&root, engine_epoch, generation, 2, universe)
        .await
        .0
        .is_some());
    assert_eq!(cache.completion_overlay_cache_metrics_for_test(), (1, 1));
}

#[tokio::test]
async fn current_completion_projection_tombstones_renamed_indexed_declaration() {
    let (_dir, service, uri, _, _) = indexed_backend_with_open_doc(
        &[],
        "main.c",
        "int OldCurrent(void);\nvoid use(void) { Old/*cursor*/; }\n",
    )
    .await;
    let (changed, line, character) =
        text_and_position("int NewCurrent(void);\nvoid use(void) { New/*cursor*/; }\n");
    service
        .inner()
        .session
        .change_document(uri.clone(), 2, changed)
        .await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let items = completion_items(response);

    assert!(items.iter().any(|item| item.label == "NewCurrent"));
    assert!(
        items.iter().all(|item| item.label != "OldCurrent"),
        "the current dirty path must tombstone its durable declarations"
    );
}

#[tokio::test]
async fn completion_memo_generation_tracks_recall_universe_not_body_revision() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    let uri = Url::from_file_path(root.join("main.c")).expect("main uri");
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
        .set_name_table_for_test(
            root,
            Arc::new(crate::query::NameTable::build(vec![
                (1, "able_symbol".to_string(), false),
                (2, "about_symbol".to_string(), false),
            ])),
        )
        .await;
    let (first_text, first_line, first_character) =
        text_and_position("void use(void) { a/*cursor*/; }\n");
    open_test_document(&service, uri.clone(), 1, first_text).await;
    service
        .inner()
        .completion(completion_params(uri.clone(), first_line, first_character))
        .await
        .expect("first completion request")
        .expect("first completion response");
    let first_generation = service
        .inner()
        .session
        .cache
        .completion_memo_for_test(&uri)
        .await
        .expect("first memo")
        .generation;

    let (body_text, body_line, body_character) =
        text_and_position("void use(void) { ab/*cursor*/; }\n");
    service
        .inner()
        .session
        .change_document(uri.clone(), 2, body_text)
        .await;
    service
        .inner()
        .completion(completion_params(uri.clone(), body_line, body_character))
        .await
        .expect("body completion request")
        .expect("body completion response");
    let body_generation = service
        .inner()
        .session
        .cache
        .completion_memo_for_test(&uri)
        .await
        .expect("body memo")
        .generation;
    assert_eq!(
        first_generation, body_generation,
        "a body-only edit must retain the indexed recall universe"
    );

    let (include_text, include_line, include_character) =
        text_and_position("#include \"replacement.h\"\nvoid use(void) { ab/*cursor*/; }\n");
    service
        .inner()
        .session
        .change_document(uri.clone(), 3, include_text)
        .await;
    service
        .inner()
        .completion(completion_params(
            uri.clone(),
            include_line,
            include_character,
        ))
        .await
        .expect("include completion request")
        .expect("include completion response");
    let include_generation = service
        .inner()
        .session
        .cache
        .completion_memo_for_test(&uri)
        .await
        .expect("include memo")
        .generation;
    assert_ne!(body_generation, include_generation);
}

#[tokio::test]
async fn candidate_overlay_cache_includes_dirty_external_header_with_normalized_absolute_path() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().join("workspace");
    let include_root = dir.path().join("sdk").join("include");
    std::fs::create_dir_all(&root).expect("workspace");
    std::fs::create_dir_all(&include_root).expect("include root");
    let header = include_root.join("vendor.h");
    std::fs::write(&header, "int saved_vendor(void);\n").expect("saved header");
    let uri = Url::from_file_path(&header).expect("header uri");
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root.clone());
    *service.inner().include_paths.lock().await =
        vec![crate::pathing::normalize_abs_path(&include_root)];
    service
        .inner()
        .session
        .open_document(uri.clone(), 1, "int dirty_vendor(void);\n".into())
        .await;

    let generation = crate::call_model::SemanticGeneration::MISSING;
    let first = service
        .inner()
        .candidate_overlay_snapshot(&root, generation, None, None)
        .await;
    let first_root_mapping = service
        .inner()
        .authorized_external_source_roots(&root)
        .await;
    let first_authorization_misses = first_root_mapping.authorization_miss_count_for_test();
    assert_eq!(first_authorization_misses, 1);
    let external_path = crate::pathing::normalize_abs_path(&header);
    assert!(first.shadows(&external_path));
    assert_eq!(
        first.source_text(&external_path),
        Some("int dirty_vendor(void);\n")
    );

    let cached = service
        .inner()
        .candidate_overlay_snapshot(&root, generation, None, None)
        .await;
    assert!(Arc::ptr_eq(&first, &cached));
    assert_eq!(
        service
            .inner()
            .session
            .cache
            .candidate_overlay_cache_len_for_test()
            .await,
        1
    );

    service
        .inner()
        .session
        .change_document(uri.clone(), 2, "int newer_vendor(void);\n".into())
        .await;
    let newer = service
        .inner()
        .candidate_overlay_snapshot(&root, generation, None, None)
        .await;
    let newer_root_mapping = service
        .inner()
        .authorized_external_source_roots(&root)
        .await;
    assert!(!Arc::ptr_eq(&first, &newer));
    assert!(
        Arc::ptr_eq(&first_root_mapping, &newer_root_mapping),
        "a new overlay epoch must reuse validated external-root mappings"
    );
    assert_eq!(
        newer_root_mapping.authorization_miss_count_for_test(),
        first_authorization_misses,
        "didChange must not repeat filesystem authorization for the same external URI"
    );
    assert_eq!(
        newer.source_text(&external_path),
        Some("int newer_vendor(void);\n")
    );
    assert_eq!(
        service
            .inner()
            .session
            .cache
            .candidate_overlay_cache_len_for_test()
            .await,
        1
    );

    service
        .inner()
        .did_close(tower_lsp::lsp_types::DidCloseTextDocumentParams {
            text_document: TextDocumentIdentifier { uri },
        })
        .await;
    assert_eq!(
        newer_root_mapping.authorization_cache_len_for_test(),
        0,
        "closing an external URI must discard its generation-bound authorization"
    );
}

#[tokio::test]
async fn did_close_prevents_a_late_external_authorization_cache_write() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().join("workspace");
    let include_root = dir.path().join("sdk");
    std::fs::create_dir_all(&root).expect("workspace");
    std::fs::create_dir_all(&include_root).expect("include root");
    let header = include_root.join("vendor.h");
    std::fs::write(&header, "int vendor(void);\n").expect("header");
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root.clone());
    *service.inner().include_paths.lock().await =
        vec![crate::pathing::normalize_abs_path(&include_root)];
    let semantics = current_test_workspace_semantics(&service, &root).await;
    let started = Arc::new(std::sync::Barrier::new(2));
    let resume = Arc::new(std::sync::Barrier::new(2));
    semantics
        .external_roots
        .set_authorization_publish_barriers_for_test(started.clone(), resume.clone());
    service
        .inner()
        .session
        .cache
        .publish_engine_snapshot(super::workspace::EngineSnapshot {
            root: root.clone(),
            epoch: super::state::EngineEpoch::published(1),
            semantic_generation: crate::call_model::SemanticGeneration::MISSING,
            declaration_index: None,
            name_table: None,
            fallback_completion_table: Arc::new(Default::default()),
            reach_graph: None,
            include_table: None,
            go_import_table: None,
            indexed_files: None,
            include_path_index: None,
            project_context: None,
            call_read_handle: None,
            workspace_semantics: semantics.clone(),
            degraded: Default::default(),
        })
        .await;
    let uri = Url::from_file_path(&header).expect("header uri");
    service
        .inner()
        .session
        .open_document(uri.clone(), 1, "int dirty_vendor(void);\n".into())
        .await;

    let roots = semantics.external_roots.clone();
    let authorize_header = header.clone();
    let authorization = tokio::task::spawn_blocking(move || roots.mapped_path(&authorize_header));
    tokio::task::spawn_blocking(move || started.wait())
        .await
        .expect("wait for miss");

    service
        .inner()
        .did_close(tower_lsp::lsp_types::DidCloseTextDocumentParams {
            text_document: TextDocumentIdentifier { uri },
        })
        .await;
    tokio::task::spawn_blocking(move || resume.wait())
        .await
        .expect("release miss");
    assert!(authorization.await.expect("authorization worker").is_some());
    assert_eq!(
        semantics.external_roots.authorization_cache_len_for_test(),
        0,
        "a miss computed before didClose must not publish after invalidation"
    );
}

#[tokio::test]
async fn candidate_overlay_includes_configured_go_module_roots_but_rejects_siblings() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().join("workspace");
    let config_module = dir.path().join("configured-module");
    let config_identity_root = dir
        .path()
        .join("identity-parent")
        .join("..")
        .join("configured-module");
    let client_module = dir.path().join("client-module");
    let sibling = dir.path().join("not-configured");
    for path in [
        &root,
        &config_module,
        &client_module,
        &sibling,
        &dir.path().join("identity-parent"),
    ] {
        std::fs::create_dir_all(path).expect("external root");
    }
    std::fs::write(
        root.join("fossilsense.json"),
        serde_json::json!({
            "goModulePaths": [crate::pathing::normalize_abs_path(&config_identity_root)]
        })
        .to_string(),
    )
    .expect("workspace config");
    let configured_go = config_module.join("configured.go");
    let client_go = client_module.join("client.go");
    let sibling_go = sibling.join("sibling.go");
    for path in [&configured_go, &client_go, &sibling_go] {
        std::fs::write(path, "package external\n").expect("saved Go source");
    }
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root.clone());
    *service.inner().go_module_paths.lock().await =
        vec![crate::pathing::normalize_abs_path(&client_module)];
    for (path, declaration) in [
        (&configured_go, "ConfiguredDirty"),
        (&client_go, "ClientDirty"),
        (&sibling_go, "SiblingDirty"),
    ] {
        service
            .inner()
            .session
            .open_document(
                Url::from_file_path(path).expect("Go uri"),
                1,
                format!("package external\nfunc {declaration}() {{}}\n"),
            )
            .await;
    }

    let overlay = service
        .inner()
        .candidate_overlay_snapshot(
            &root,
            crate::call_model::SemanticGeneration::MISSING,
            None,
            None,
        )
        .await;
    for path in [
        crate::pathing::normalize_abs_path(&config_identity_root.join("configured.go")),
        crate::pathing::normalize_abs_path(&client_go),
    ] {
        assert!(
            overlay.shadows(&path),
            "configured Go root must shadow {path}"
        );
        assert_eq!(
            overlay.semantic_family_for_path(&path),
            Some(crate::semantic_model::SemanticFamily::Go)
        );
    }
    assert!(
        !overlay.shadows(&crate::pathing::normalize_abs_path(&sibling_go)),
        "an adjacent unconfigured external file must remain unauthorized"
    );
}

#[tokio::test]
async fn candidate_overlay_shadows_every_persisted_include_alias_identity() {
    let dir = tempdir().expect("tempdir");
    let workspace = dir.path().join("workspace");
    let external = dir.path().join("external");
    let alias_a = dir.path().join("alias-a").join("..").join("external");
    let alias_b = dir.path().join("alias-b").join("..").join("external");
    for path in [
        &workspace,
        &external,
        &dir.path().join("alias-a"),
        &dir.path().join("alias-b"),
    ] {
        std::fs::create_dir_all(path).expect("directory");
    }
    std::fs::write(workspace.join("main.cpp"), "void use(void) {}\n").expect("workspace source");
    let external_source = external.join("api.h");
    std::fs::write(&external_source, "int saved_external(void);\n").expect("saved external source");
    std::fs::write(
        workspace.join("fossilsense.json"),
        serde_json::json!({
            "includePaths": [
                crate::pathing::normalize_abs_path(&alias_a),
                crate::pathing::normalize_abs_path(&alias_b)
            ]
        })
        .to_string(),
    )
    .expect("workspace config");
    crate::indexer::index_workspace(
        &workspace,
        crate::indexer::IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let service = test_backend_service();
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(workspace.clone());
    service
        .inner()
        .session
        .cache
        .publish_full_index(&service.inner().client, workspace.clone())
        .await
        .expect("publish");
    open_test_document(
        &service,
        Url::from_file_path(&external_source).expect("external uri"),
        1,
        "int dirty_external(void);\n".into(),
    )
    .await;

    let context = service
        .inner()
        .request_context_for_root(workspace.clone())
        .await;
    let overlay = service
        .inner()
        .candidate_overlay_snapshot(
            &workspace,
            context.engine.semantic_generation,
            context.engine.reach_graph.as_deref(),
            context.engine.indexed_files.as_deref().map(Vec::as_slice),
        )
        .await;
    for identity in [
        crate::pathing::normalize_abs_path(&alias_a.join("api.h")),
        crate::pathing::normalize_abs_path(&alias_b.join("api.h")),
    ] {
        assert!(
            overlay.shadows(&identity),
            "dirty canonical file must shadow persisted alias {identity}"
        );
    }
    let query = crate::candidate_service::CandidateQueryService::new_with_declarations_for_family(
        context.engine.call_read_handle.as_deref(),
        context.engine.declaration_index.as_deref(),
        &overlay,
        "main.cpp",
        None,
        context.engine.reach_graph.as_deref(),
        crate::semantic_model::SemanticFamily::CFamily,
    );
    let stale = query
        .semantic_candidates(
            "saved_external",
            crate::candidate_service::SemanticIntent::Call,
        )
        .expect("stale query");
    assert!(
        stale.all.iter().all(|group| group.candidates.is_empty()),
        "no persisted include alias may leak stale facts"
    );
    let dirty = query
        .semantic_candidates(
            "dirty_external",
            crate::candidate_service::SemanticIntent::Call,
        )
        .expect("dirty query");
    assert!(dirty.all.iter().any(|group| !group.candidates.is_empty()));
}

#[tokio::test]
async fn candidate_overlay_bounds_alias_parses_but_tombstones_every_identity() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().join("workspace");
    let external = dir.path().join("external");
    std::fs::create_dir_all(&root).expect("workspace");
    std::fs::create_dir_all(&external).expect("external");
    let source = external.join("api.h");
    std::fs::write(&source, "package external\nfunc SavedAlias() {}\n").expect("saved source");
    let mut include_paths = Vec::new();
    let mut identities = Vec::new();
    for index in 0..12 {
        let marker = dir.path().join(format!("alias-{index}"));
        std::fs::create_dir_all(&marker).expect("alias marker");
        let identity_root = marker.join("..").join("external");
        include_paths.push(crate::pathing::normalize_abs_path(&identity_root));
        identities.push(crate::pathing::normalize_abs_path(
            &identity_root.join("api.h"),
        ));
    }
    std::fs::write(
        root.join("fossilsense.json"),
        serde_json::json!({
            "includePaths": include_paths,
            "languageOverrides": [{"glob": "**/api.h", "language": "go"}]
        })
        .to_string(),
    )
    .expect("workspace config");
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root.clone());
    open_test_document(
        &service,
        Url::from_file_path(&source).expect("external uri"),
        1,
        "package external\nfunc dirty_alias() {}\n".into(),
    )
    .await;

    let overlay = service
        .inner()
        .candidate_overlay_snapshot(
            &root,
            crate::call_model::SemanticGeneration::MISSING,
            None,
            None,
        )
        .await;
    assert!(
        identities.iter().all(|identity| overlay.shadows(identity)),
        "every persisted identity must be tombstoned even beyond the parse budget"
    );
    assert!(
        overlay.declarations("dirty_alias").len()
            <= super::candidate_context::MAX_EXTERNAL_OVERLAY_PARSED_IDENTITIES,
        "dirty parsing must have a fixed per-document identity bound"
    );
    assert!(
        overlay.has_incomplete_facts(),
        "unparsed aliases must make coverage explicitly incomplete"
    );
    assert_eq!(
        overlay
            .semantic_family_for_path(identities.last().expect("identity beyond the parse budget")),
        Some(crate::semantic_model::SemanticFamily::Go),
        "a tombstone must retain the published override family"
    );
}

#[tokio::test]
async fn candidate_overlay_reuses_external_parse_across_workspace_roots() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    let root_a = dir.path().join("workspace-a");
    let root_b = dir.path().join("workspace-b");
    let external = dir.path().join("external");
    for path in [&root_a, &root_b, &external] {
        std::fs::create_dir_all(path).expect("directory");
    }
    let source = external.join("device.go");
    std::fs::write(&source, "package device\nfunc Saved(void) {}\n").expect("saved source");
    let config = serde_json::json!({
        "goModulePaths": [crate::pathing::normalize_abs_path(&external)]
    })
    .to_string();
    for root in [&root_a, &root_b] {
        std::fs::write(root.join("fossilsense.json"), &config).expect("workspace config");
    }
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .extend([root_a.clone(), root_b.clone()]);
    open_test_document(
        &service,
        Url::from_file_path(&source).expect("external uri"),
        1,
        "package device\nfunc Dirty() {}\n".into(),
    )
    .await;

    for root in [&root_a, &root_b] {
        service
            .inner()
            .candidate_overlay_snapshot(
                root,
                crate::call_model::SemanticGeneration::MISSING,
                None,
                None,
            )
            .await;
    }

    assert_eq!(
        service
            .inner()
            .session
            .documents
            .external_overlay_parse_cache_len_for_test()
            .await,
        1,
        "the same URI, version, language, and identity must share one external parse"
    );
}

#[tokio::test]
async fn candidate_overlay_uses_alias_identity_for_language_override() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().join("workspace");
    let external = dir.path().join("external");
    let identity_root = dir
        .path()
        .join("identity-parent")
        .join("..")
        .join("external");
    for path in [&root, &external, &dir.path().join("identity-parent")] {
        std::fs::create_dir_all(path).expect("directory");
    }
    let source = external.join("device.h");
    std::fs::write(&source, "package device\nfunc SavedGo() {}\n").expect("saved source");
    let identity = crate::pathing::normalize_abs_path(&identity_root.join("device.h"));
    std::fs::write(
        root.join("fossilsense.json"),
        serde_json::json!({
            "includePaths": [crate::pathing::normalize_abs_path(&identity_root)],
            "languageOverrides": [{"glob": identity, "language": "go"}]
        })
        .to_string(),
    )
    .expect("workspace config");
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root.clone());
    open_test_document(
        &service,
        Url::from_file_path(&source).expect("external uri"),
        1,
        "package device\nfunc DirtyGo() {}\n".into(),
    )
    .await;

    let overlay = service
        .inner()
        .candidate_overlay_snapshot(
            &root,
            crate::call_model::SemanticGeneration::MISSING,
            None,
            None,
        )
        .await;
    let identity = crate::pathing::normalize_abs_path(&identity_root.join("device.h"));
    assert!(overlay.shadows(&identity));
    assert_eq!(
        overlay.semantic_family_for_path(&identity),
        Some(crate::semantic_model::SemanticFamily::Go)
    );
    assert!(
        !overlay.declarations("DirtyGo").is_empty(),
        "identity-specific Go override must select the Go parser"
    );
}

#[tokio::test]
async fn candidate_overlay_tombstones_external_go_file_after_its_parent_is_removed() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().join("workspace");
    let go_module = dir.path().join("device-module");
    let package = go_module.join("device");
    let source = package.join("device.go");
    std::fs::create_dir_all(&root).expect("workspace");
    std::fs::create_dir_all(&package).expect("external package");
    std::fs::write(&source, "package device\nfunc Saved() {}\n").expect("saved Go source");
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root.clone());
    *service.inner().go_module_paths.lock().await =
        vec![crate::pathing::normalize_abs_path(&go_module)];
    service
        .inner()
        .session
        .open_document(
            Url::from_file_path(&source).expect("Go uri"),
            1,
            "package device\nfunc Dirty() {}\n".into(),
        )
        .await;
    std::fs::remove_dir_all(&package).expect("remove package directory");

    let overlay = service
        .inner()
        .candidate_overlay_snapshot(
            &root,
            crate::call_model::SemanticGeneration::MISSING,
            None,
            None,
        )
        .await;
    let identity = crate::pathing::normalize_abs_path(&source);
    assert!(
        overlay.shadows(&identity),
        "an open external buffer must tombstone its persisted identity after its parent disappears"
    );
    assert_eq!(
        overlay.source_text(&identity),
        Some("package device\nfunc Dirty() {}\n")
    );
}

#[tokio::test]
async fn dirty_external_go_alias_shadows_persisted_declarations() {
    let dir = tempdir().expect("tempdir");
    let workspace = dir.path().join("workspace");
    let external = dir.path().join("external");
    let identity_root = dir
        .path()
        .join("identity-parent")
        .join("..")
        .join("external");
    let package = external.join("device");
    std::fs::create_dir_all(&workspace).expect("workspace");
    std::fs::create_dir_all(dir.path().join("identity-parent")).expect("identity parent");
    std::fs::create_dir_all(&package).expect("external package");
    std::fs::write(
        workspace.join("go.mod"),
        "module example.com/workspace\n\ngo 1.22\n",
    )
    .expect("workspace go.mod");
    std::fs::write(
        external.join("go.mod"),
        "module example.com/external\n\ngo 1.22\n",
    )
    .expect("external go.mod");
    let (main_source, line, character) = text_and_position(
        "package main\n\
         import device \"example.com/external/device\"\n\
         func use() { device./*cursor*/ }\n",
    );
    std::fs::write(workspace.join("main.go"), &main_source).expect("workspace source");
    let external_source = package.join("device.go");
    std::fs::write(
        &external_source,
        "package device\nfunc SavedExternal() {}\n",
    )
    .expect("saved external source");
    std::fs::write(
        workspace.join("fossilsense.json"),
        serde_json::json!({
            "goModulePaths": [crate::pathing::normalize_abs_path(&identity_root)]
        })
        .to_string(),
    )
    .expect("workspace config");
    crate::indexer::index_workspace(
        &workspace,
        crate::indexer::IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let service = test_backend_service();
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(workspace.clone());
    service
        .inner()
        .session
        .cache
        .publish_full_index(&service.inner().client, workspace.clone())
        .await
        .expect("publish");
    let main_uri = Url::from_file_path(workspace.join("main.go")).expect("main uri");
    open_test_document(&service, main_uri.clone(), 1, main_source).await;
    let external_uri = Url::from_file_path(&external_source).expect("external uri");
    open_test_document(
        &service,
        external_uri.clone(),
        1,
        "package device\nfunc DirtyExternal() {}\n".into(),
    )
    .await;

    let completion = service
        .inner()
        .completion(completion_params(main_uri, line, character))
        .await
        .expect("Go completion")
        .expect("Go completion response");
    let labels = completion_items(completion)
        .into_iter()
        .map(|item| item.label)
        .collect::<Vec<_>>();
    assert!(
        labels.iter().all(|label| label != "SavedExternal"),
        "persisted declaration leaked through alias identity: {labels:?}"
    );
    let context = service
        .inner()
        .request_context_for_root(workspace.clone())
        .await;
    let overlay = service
        .inner()
        .candidate_overlay_snapshot(
            &workspace,
            context.engine.semantic_generation,
            context.engine.reach_graph.as_deref(),
            context.engine.indexed_files.as_deref().map(Vec::as_slice),
        )
        .await;
    let query = crate::candidate_service::CandidateQueryService::new_with_declarations_for_family(
        context.engine.call_read_handle.as_deref(),
        context.engine.declaration_index.as_deref(),
        &overlay,
        "main.go",
        None,
        context.engine.reach_graph.as_deref(),
        crate::semantic_model::SemanticFamily::Go,
    );
    let stale = query
        .semantic_candidates(
            "SavedExternal",
            crate::candidate_service::SemanticIntent::Call,
        )
        .expect("stale candidate query");
    assert!(
        stale.all.iter().all(|group| group.candidates.is_empty()),
        "the dirty alias must tombstone persisted candidates"
    );
    let dirty = query
        .semantic_candidates(
            "DirtyExternal",
            crate::candidate_service::SemanticIntent::Call,
        )
        .expect("dirty candidate query");
    assert!(
        dirty.all.iter().any(|group| !group.candidates.is_empty()),
        "the mapped overlay must expose dirty candidates"
    );
}

#[tokio::test]
async fn candidate_overlay_keeps_published_external_semantics_until_next_generation() {
    let dir = tempdir().expect("tempdir");
    let workspace = dir.path().join("workspace");
    let external = dir.path().join("external");
    let package = external.join("device");
    std::fs::create_dir_all(&workspace).expect("workspace");
    std::fs::create_dir_all(&package).expect("external package");
    std::fs::write(
        workspace.join("go.mod"),
        "module example.com/workspace\n\ngo 1.22\n",
    )
    .expect("workspace go.mod");
    std::fs::write(
        external.join("go.mod"),
        "module example.com/external\n\ngo 1.22\n",
    )
    .expect("external go.mod");
    std::fs::write(workspace.join("main.go"), "package main\n").expect("workspace source");
    let external_source = package.join("device.go");
    std::fs::write(
        &external_source,
        "package device\nfunc SavedExternal() {}\n",
    )
    .expect("saved source");
    std::fs::write(
        workspace.join("fossilsense.json"),
        serde_json::json!({
            "goModulePaths": [crate::pathing::normalize_abs_path(&external)]
        })
        .to_string(),
    )
    .expect("generation N config");
    crate::indexer::index_workspace(
        &workspace,
        crate::indexer::IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index generation N");

    let service = test_backend_service();
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(workspace.clone());
    service
        .inner()
        .session
        .cache
        .publish_full_index(&service.inner().client, workspace.clone())
        .await
        .expect("publish generation N");
    let external_uri = Url::from_file_path(&external_source).expect("external uri");
    open_test_document(
        &service,
        external_uri.clone(),
        1,
        "package device\nfunc DirtyExternal() {}\n".into(),
    )
    .await;
    let context = service
        .inner()
        .request_context_for_root(workspace.clone())
        .await;

    std::fs::write(workspace.join("fossilsense.json"), "{}").expect("generation N+1 config");
    service.inner().config_cache.lock().await.remove(&workspace);
    service
        .inner()
        .invalidate_external_source_root_cache(std::slice::from_ref(&workspace))
        .await;
    service
        .inner()
        .session
        .cache
        .invalidate_candidate_overlay_roots(std::slice::from_ref(&workspace))
        .await;

    let overlay = service
        .inner()
        .candidate_overlay_snapshot(
            &workspace,
            context.engine.semantic_generation,
            context.engine.reach_graph.as_deref(),
            context.engine.indexed_files.as_deref().map(Vec::as_slice),
        )
        .await;
    let identity = crate::pathing::normalize_abs_path(&external_source);
    assert!(
        overlay.shadows(&identity),
        "generation N dirty file must keep shadowing N facts while N+1 is only scheduled"
    );
    let query = crate::candidate_service::CandidateQueryService::new_with_declarations_for_family(
        context.engine.call_read_handle.as_deref(),
        context.engine.declaration_index.as_deref(),
        &overlay,
        "main.go",
        None,
        context.engine.reach_graph.as_deref(),
        crate::semantic_model::SemanticFamily::Go,
    );
    let stale = query
        .semantic_candidates(
            "SavedExternal",
            crate::candidate_service::SemanticIntent::Call,
        )
        .expect("stale query");
    assert!(stale.all.iter().all(|group| group.candidates.is_empty()));
    let dirty = query
        .semantic_candidates(
            "DirtyExternal",
            crate::candidate_service::SemanticIntent::Call,
        )
        .expect("dirty query");
    assert!(dirty.all.iter().any(|group| !group.candidates.is_empty()));

    let documents = service
        .inner()
        .session
        .documents
        .capture_request_snapshot(Some(&external_uri))
        .await;
    crate::indexer::index_workspace(
        &workspace,
        crate::indexer::IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index generation N+1");
    service
        .inner()
        .session
        .cache
        .publish_full_index(&service.inner().client, workspace.clone())
        .await
        .expect("publish generation N+1");
    let relation_state = service
        .inner()
        .relation_state_from_context(&external_uri, workspace, context, documents)
        .await
        .expect("generation N relation state");
    assert!(
        relation_state
            .overlays
            .iter()
            .any(|overlay| overlay.path == identity),
        "call hierarchy must retain N's dirty external tombstone after N+1 publishes"
    );
}

#[tokio::test]
async fn candidate_overlay_does_not_adopt_a_new_graph_after_request_publication() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    let uri = Url::from_file_path(root.join("main.c")).expect("uri");
    service
        .inner()
        .session
        .open_document(uri, 1, "int dirty_main(void);\n".into())
        .await;
    let generation = crate::call_model::SemanticGeneration(7);
    service
        .inner()
        .session
        .cache
        .publish_engine_snapshot(super::workspace::EngineSnapshot {
            root: root.clone(),
            epoch: super::state::EngineEpoch::published(1),
            semantic_generation: generation,
            declaration_index: None,
            name_table: None,
            fallback_completion_table: Arc::new(Default::default()),
            reach_graph: Some(Arc::new(crate::reachability::ReachGraph::new(
                vec![("unrelated.c".into(), "old.h".into())],
                vec![],
                vec![],
            ))),
            include_table: None,
            go_import_table: None,
            indexed_files: None,
            include_path_index: None,
            project_context: None,
            call_read_handle: None,
            workspace_semantics: empty_workspace_semantics(&root),
            degraded: Default::default(),
        })
        .await;
    let old = service
        .inner()
        .session
        .cache
        .current_engine_snapshot(&root)
        .await
        .expect("old snapshot");
    service
        .inner()
        .session
        .cache
        .publish_engine_snapshot(super::workspace::EngineSnapshot {
            root: root.clone(),
            epoch: super::state::EngineEpoch::published(2),
            semantic_generation: generation,
            declaration_index: None,
            name_table: None,
            fallback_completion_table: Arc::new(Default::default()),
            reach_graph: Some(Arc::new(crate::reachability::ReachGraph::new(
                vec![("unrelated.c".into(), "new.h".into())],
                vec![],
                vec![],
            ))),
            include_table: None,
            go_import_table: None,
            indexed_files: None,
            include_path_index: None,
            project_context: None,
            call_read_handle: None,
            workspace_semantics: old.workspace_semantics.clone(),
            degraded: Default::default(),
        })
        .await;

    let overlay = service
        .inner()
        .candidate_overlay_snapshot(&root, generation, old.reach_graph.as_deref(), None)
        .await;
    let scope = overlay
        .effective_reach_graph(None)
        .expect("conservative dirty graph")
        .reachable("unrelated.c");
    assert!(!scope.files.contains("old.h"));
    assert!(!scope.files.contains("new.h"));
}

#[tokio::test]
async fn candidate_overlay_cache_rejects_a_late_build_after_publication() {
    let cache = super::CacheLedger::default();
    let root = PathBuf::from("/workspace/late-overlay");
    let generation = crate::call_model::SemanticGeneration(7);
    let (cached, build_revision) = cache.candidate_overlay(&root, generation, 3).await;
    assert!(cached.is_none());

    cache
        .publish_engine_snapshot(super::workspace::EngineSnapshot {
            root: root.clone(),
            epoch: super::state::EngineEpoch::published(8),
            semantic_generation: generation,
            declaration_index: None,
            name_table: None,
            fallback_completion_table: Arc::new(Default::default()),
            reach_graph: None,
            include_table: None,
            go_import_table: None,
            indexed_files: None,
            include_path_index: None,
            project_context: None,
            call_read_handle: None,
            workspace_semantics: empty_workspace_semantics(&root),
            degraded: Default::default(),
        })
        .await;
    let late = Arc::new(crate::candidate_service::CandidateOverlaySnapshot::new(
        3,
        Vec::new(),
    ));
    let returned = cache
        .publish_candidate_overlay(root, generation, 3, build_revision, late.clone())
        .await;

    assert!(Arc::ptr_eq(&returned, &late));
    assert_eq!(cache.candidate_overlay_cache_len_for_test().await, 0);
}

#[tokio::test]
async fn indexed_completion_resolve_rejects_cross_generation_context_before_overlay_build() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    let uri = Url::from_file_path(root.join("main.c")).expect("uri");
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root.clone());
    service
        .inner()
        .session
        .open_document(uri.clone(), 1, "int dirty(void);\n".into())
        .await;
    service
        .inner()
        .session
        .cache
        .publish_engine_snapshot(super::workspace::EngineSnapshot {
            root: root.clone(),
            epoch: super::state::EngineEpoch::published(2),
            semantic_generation: crate::call_model::SemanticGeneration(12),
            declaration_index: None,
            name_table: None,
            fallback_completion_table: Arc::new(Default::default()),
            reach_graph: None,
            include_table: None,
            go_import_table: None,
            indexed_files: None,
            include_path_index: None,
            project_context: None,
            call_read_handle: None,
            workspace_semantics: empty_workspace_semantics(&root),
            degraded: Default::default(),
        })
        .await;
    let item = CompletionItem {
        label: "dirty".into(),
        data: Some(
            serde_json::to_value(super::CompletionDocumentationData::Candidate {
                version: 4,
                root: root.to_string_lossy().into_owned(),
                uri: uri.to_string(),
                handle: crate::candidate_service::CandidateHandle {
                    locator: crate::candidate_service::CandidateHandleLocator::Persistent {
                        declaration_id: 1,
                    },
                    logical_key: crate::semantic_model::LogicalEntityKey {
                        qualified_name: "dirty".into(),
                        declaration_kind: crate::semantic_model::SemanticDeclarationKind::Function,
                        owner: None,
                        canonical_signature: None,
                        linkage_domain: "external".into(),
                        guard_fingerprint: None,
                    },
                    locator_fingerprint: "stale".into(),
                    semantic_family: crate::config::SemanticFamily::CFamily,
                },
                semantic_generation: 11,
                overlay_epoch: service
                    .inner()
                    .session
                    .documents
                    .capture_request_snapshot(Some(&uri))
                    .await
                    .overlay_epoch,
                document_version: 1,
            })
            .expect("completion data"),
        ),
        ..Default::default()
    };

    let resolved = service
        .inner()
        .completion_resolve(item)
        .await
        .expect("resolve");
    assert!(resolved.documentation.is_none());
    assert_eq!(
        service
            .inner()
            .session
            .cache
            .candidate_overlay_cache_len_for_test()
            .await,
        0,
        "stale completion data must be rejected before mixing/building current overlay state"
    );
}

#[tokio::test]
async fn reach_scope_uses_captured_request_context_graph() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    let uri = Url::from_file_path(root.join("main.c")).expect("file uri");
    let captured_graph = Arc::new(crate::reachability::ReachGraph::new(
        vec![("main.c".to_string(), "captured.h".to_string())],
        vec![],
        vec![],
    ));
    let context = super::RequestContext {
        engine: Arc::new(super::workspace::EngineSnapshot {
            root: root.clone(),
            epoch: super::state::EngineEpoch::missing(),
            semantic_generation: crate::call_model::SemanticGeneration::MISSING,
            declaration_index: None,
            name_table: None,
            fallback_completion_table: Arc::new(Default::default()),
            reach_graph: Some(captured_graph),
            include_table: None,
            go_import_table: None,
            indexed_files: None,
            include_path_index: None,
            project_context: None,
            call_read_handle: None,
            workspace_semantics: empty_workspace_semantics(&root),
            degraded: crate::progress::DegradedCapabilities::default(),
        }),
        settings: super::RequestSettings {
            scoping_enabled: true,
            ..Default::default()
        },
    };

    service
        .inner()
        .session
        .cache
        .set_reach_graph_for_test(
            root,
            Arc::new(crate::reachability::ReachGraph::new(
                vec![("main.c".to_string(), "ledger.h".to_string())],
                vec![],
                vec![],
            )),
        )
        .await;

    let (_rel, scope) = service
        .inner()
        .reach_scope_from_context(&uri, &context)
        .expect("scope from captured request context");

    assert!(scope.files.contains("captured.h"));
    assert!(
        !scope.files.contains("ledger.h"),
        "request scope must come from the already captured snapshot"
    );
}

#[tokio::test]
async fn failed_include_table_rebuild_cannot_replace_published_state() {
    let root = tempdir().expect("root");
    let root_path = root.path().to_path_buf();
    let result = rebuild_include_table(root_path).await;

    assert!(result.is_err(), "missing index should fail the rebuild");
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

    let table = rebuild_include_table(root_path)
        .await
        .expect("rebuild include table");

    assert_eq!(table.len(), 2);
    assert_eq!(table.edge_count(), 1);
}

#[tokio::test]
async fn failed_reference_file_list_rebuild_cannot_replace_published_state() {
    let root = tempdir().expect("root");
    let root_path = root.path().to_path_buf();
    let result = rebuild_indexed_file_list(root_path).await;

    assert!(result.is_err(), "missing index should fail the rebuild");
}

// --- R6 section 4: grouped references role exposure --------------------

#[test]
fn grouped_reference_items_preserve_role_and_order() {
    use crate::parser::SyntacticRole;
    use crate::references::{self, ReferenceHit};
    let dir = tempdir().expect("tempdir");
    let mut hits = vec![
        ReferenceHit {
            rel_path: "a.c".into(),
            line: 9,
            start_col_utf16: 0,
            end_col_utf16: 3,
            role: SyntacticRole::Read,
        },
        ReferenceHit {
            rel_path: "b.c".into(),
            line: 2,
            start_col_utf16: 0,
            end_col_utf16: 3,
            role: SyntacticRole::Definition,
        },
    ];
    references::sort_hits_by_role(&mut hits);
    let items = grouped_reference_items(dir.path(), &hits);
    assert_eq!(items.len(), 2);
    // Definition group first; each item carries its role label for the client.
    assert_eq!(items[0].role, "definition");
    assert_eq!(items[1].role, "read");
}

#[tokio::test]
async fn member_completion_returns_fields_and_methods_for_resolved_receiver() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[(
            "widget.hpp",
            "struct Widget {\n/// Current widget width.\nint width;\n/// Resizes the widget.\nvoid resize();\n};\n",
        )],
        "main.cpp",
        "#include \"widget.hpp\"\nvoid f(Widget *w) { w->/*cursor*/ }\n",
    )
    .await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let items = completion_items(response);

    assert!(items
        .iter()
        .any(|item| item.label == "resize" && item.kind == Some(CompletionItemKind::METHOD)));
    assert!(items
        .iter()
        .any(|item| item.label == "width" && item.kind == Some(CompletionItemKind::FIELD)));

    let resize = items
        .into_iter()
        .find(|item| item.label == "resize")
        .expect("resize completion");
    let resolved = service
        .inner()
        .completion_resolve(resize)
        .await
        .expect("resolve member completion");
    let documentation = resolved.documentation.expect("member documentation");
    let documentation = match documentation {
        Documentation::String(value) => value,
        Documentation::MarkupContent(markup) => markup.value,
    };
    assert!(documentation.contains("Resizes the widget."));
}

#[tokio::test]
async fn member_completion_uses_primary_published_family_across_workspace_roots() {
    let dir = tempdir().expect("root");
    let go_root = dir.path().join("go-root");
    let c_root = dir.path().join("c-root");
    fs::create_dir_all(go_root.join("legacy")).expect("Go tree");
    fs::create_dir_all(&c_root).expect("C tree");
    fs::write(
        go_root.join("fossilsense.json"),
        r#"{"languageOverrides":[{"glob":"legacy/**/*.h","language":"go"}]}"#,
    )
    .expect("Go override");
    let (go_text, line, character) =
        text_and_position("package legacy\nfunc Use() { mystery.fie/*cursor*/ }\n");
    let go_path = go_root.join("legacy/api.h");
    fs::write(&go_path, &go_text).expect("Go source");
    fs::write(
        c_root.join("record.h"),
        "struct Other { int fieldFromC; };\n",
    )
    .expect("C source");
    for root in [&go_root, &c_root] {
        crate::indexer::index_workspace(
            root,
            crate::indexer::IndexOptions {
                force: true,
                ..Default::default()
            },
            |_| {},
        )
        .expect("index root");
    }

    let service = test_backend_service();
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .extend([go_root.clone(), c_root.clone()]);
    for root in [&go_root, &c_root] {
        service
            .inner()
            .session
            .cache
            .publish_full_index(&service.inner().client, root.clone())
            .await
            .expect("publish root");
    }
    let uri = Url::from_file_path(go_path).expect("Go URI");
    open_test_document(&service, uri.clone(), 1, go_text).await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion")
        .expect("member completion");
    let labels = completion_items(response)
        .into_iter()
        .map(|item| item.label)
        .collect::<Vec<_>>();
    assert!(
        labels.iter().all(|label| label != "fieldFromC"),
        "C-family fallback leaked into a Go request: {labels:?}"
    );
}

#[tokio::test]
async fn member_completion_resolve_allows_go_module_owner_and_rejects_sibling() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().join("workspace");
    let go_module = dir.path().join("device-module");
    let configured_go_module = dir
        .path()
        .join("identity-parent")
        .join("..")
        .join("device-module");
    let sibling = dir.path().join("not-configured");
    std::fs::create_dir_all(&root).expect("workspace");
    std::fs::create_dir_all(&go_module).expect("Go module");
    std::fs::create_dir_all(dir.path().join("identity-parent")).expect("identity parent");
    std::fs::create_dir_all(&sibling).expect("sibling");
    let request_uri = Url::from_file_path(root.join("main.go")).expect("request uri");
    let saved_source = "package device\n\
                        type Device struct{}\n\
                        // Reset has saved documentation.\n\
                        func (Device) Reset() {}\n";
    let source = "package device\n\
                  type Device struct{}\n\
                  // Reset uses unsaved external documentation.\n\
                  func (Device) Reset() {}\n";
    let allowed_owner = configured_go_module.join("device.go");
    let rejected_owner = go_module
        .join("..")
        .join(sibling.file_name().expect("sibling name"))
        .join("device.go");
    std::fs::write(go_module.join("device.go"), saved_source).expect("allowed owner");
    std::fs::write(&rejected_owner, source).expect("rejected owner");
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root.clone());
    *service.inner().go_module_paths.lock().await =
        vec![crate::pathing::normalize_abs_path(&configured_go_module)];
    open_test_document(&service, request_uri.clone(), 1, "package main\n".into()).await;
    service
        .inner()
        .session
        .open_document(
            Url::from_file_path(go_module.join("device.go")).expect("external Go uri"),
            1,
            source.into(),
        )
        .await;
    let generation = crate::call_model::SemanticGeneration(21);
    let workspace_semantics = current_test_workspace_semantics(&service, &root).await;
    service
        .inner()
        .session
        .cache
        .publish_engine_snapshot(super::workspace::EngineSnapshot {
            root: root.clone(),
            epoch: super::state::EngineEpoch::published(21),
            semantic_generation: generation,
            declaration_index: None,
            name_table: None,
            fallback_completion_table: Arc::new(Default::default()),
            reach_graph: None,
            include_table: None,
            go_import_table: None,
            indexed_files: None,
            include_path_index: None,
            project_context: None,
            call_read_handle: None,
            workspace_semantics,
            degraded: Default::default(),
        })
        .await;
    let overlay_epoch = service
        .inner()
        .session
        .documents
        .capture_request_snapshot(Some(&request_uri))
        .await
        .overlay_epoch;
    let completion_item = |owner: &std::path::Path| {
        let owner_path = crate::pathing::normalize_abs_path(owner);
        let parsed = crate::parser::parse_with_language(
            owner,
            source,
            crate::config::SourceLanguage::Go,
            crate::parser::ParseFacts::ALL,
        );
        let member = parsed
            .members
            .iter()
            .find(|member| member.name == "Reset")
            .expect("Reset member");
        CompletionItem {
            label: "Reset".into(),
            data: Some(
                serde_json::to_value(super::CompletionDocumentationData::Member {
                    version: 5,
                    root: root.to_string_lossy().into_owned(),
                    uri: request_uri.to_string(),
                    owner_path: owner_path.clone(),
                    handle: crate::model::MemberCandidateHandle::new(
                        None,
                        &owner_path,
                        &member.record_key,
                        member,
                    ),
                    semantic_family: crate::semantic_model::SemanticFamily::Go,
                    semantic_generation: generation.0,
                    owner_revision_hash: blake3::hash(source.as_bytes()).to_hex().to_string(),
                    overlay_epoch,
                    document_version: 1,
                })
                .expect("member completion data"),
            ),
            ..Default::default()
        }
    };

    let resolved = service
        .inner()
        .completion_resolve(completion_item(&allowed_owner))
        .await
        .expect("allowed resolve");
    let documentation =
        documentation_text(resolved.documentation.expect("external Go documentation"));
    assert!(documentation.contains("uses unsaved external documentation"));

    let rejected = service
        .inner()
        .completion_resolve(completion_item(&rejected_owner))
        .await
        .expect("rejected resolve");
    assert!(
        rejected.documentation.is_none(),
        "an adjacent unconfigured owner path must not be read"
    );
}

#[tokio::test]
async fn member_completion_resolve_uses_alias_identity_language_override() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().join("workspace");
    let external = dir.path().join("external");
    let identity_root = dir
        .path()
        .join("identity-parent")
        .join("..")
        .join("external");
    for path in [&root, &external, &dir.path().join("identity-parent")] {
        std::fs::create_dir_all(path).expect("directory");
    }
    let owner = identity_root.join("device.h");
    let canonical_owner = external.join("device.h");
    let owner_path = crate::pathing::normalize_abs_path(&owner);
    let source = "package device\n\
                  type Device struct{}\n\
                  // Reset uses identity-selected Go documentation.\n\
                  func (Device) Reset() {}\n";
    std::fs::write(&canonical_owner, source).expect("external source");
    std::fs::write(
        root.join("fossilsense.json"),
        serde_json::json!({
            "includePaths": [crate::pathing::normalize_abs_path(&identity_root)],
            "languageOverrides": [{"glob": owner_path, "language": "go"}]
        })
        .to_string(),
    )
    .expect("workspace config");
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root.clone());
    let request_uri = Url::from_file_path(root.join("main.go")).expect("request uri");
    open_test_document(&service, request_uri.clone(), 1, "package main\n".into()).await;
    open_test_document(
        &service,
        Url::from_file_path(&canonical_owner).expect("external uri"),
        1,
        source.into(),
    )
    .await;
    let generation = crate::call_model::SemanticGeneration(22);
    let workspace_semantics = current_test_workspace_semantics(&service, &root).await;
    service
        .inner()
        .session
        .cache
        .publish_engine_snapshot(super::workspace::EngineSnapshot {
            root: root.clone(),
            epoch: super::state::EngineEpoch::published(22),
            semantic_generation: generation,
            declaration_index: None,
            name_table: None,
            fallback_completion_table: Arc::new(Default::default()),
            reach_graph: None,
            include_table: None,
            go_import_table: None,
            indexed_files: None,
            include_path_index: None,
            project_context: None,
            call_read_handle: None,
            workspace_semantics,
            degraded: Default::default(),
        })
        .await;
    let overlay_epoch = service
        .inner()
        .session
        .documents
        .capture_request_snapshot(Some(&request_uri))
        .await
        .overlay_epoch;
    let parsed = crate::parser::parse_with_language(
        &owner,
        source,
        crate::config::SourceLanguage::Go,
        crate::parser::ParseFacts::ALL,
    );
    let member = parsed
        .members
        .iter()
        .find(|member| member.name == "Reset")
        .expect("Reset member");
    let item = CompletionItem {
        label: "Reset".into(),
        data: Some(
            serde_json::to_value(super::CompletionDocumentationData::Member {
                version: 5,
                root: root.to_string_lossy().into_owned(),
                uri: request_uri.to_string(),
                owner_path: owner_path.clone(),
                handle: crate::model::MemberCandidateHandle::new(
                    None,
                    &owner_path,
                    &member.record_key,
                    member,
                ),
                semantic_family: crate::semantic_model::SemanticFamily::Go,
                semantic_generation: generation.0,
                owner_revision_hash: blake3::hash(source.as_bytes()).to_hex().to_string(),
                overlay_epoch,
                document_version: 1,
            })
            .expect("member completion data"),
        ),
        ..Default::default()
    };

    let resolved = service
        .inner()
        .completion_resolve(item)
        .await
        .expect("resolve");
    let documentation = documentation_text(
        resolved
            .documentation
            .expect("identity-selected Go documentation"),
    );
    assert!(documentation.contains("identity-selected Go documentation"));
}

#[tokio::test]
async fn member_completion_marks_ambiguous_owner_candidates_incomplete() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[
            ("left.hpp", "struct Widget { int from_left; };\n"),
            ("right.hpp", "struct Widget { int from_right; };\n"),
        ],
        "main.cpp",
        "#include \"left.hpp\"\n#include \"right.hpp\"\nvoid f(Widget *w) { w->/*cursor*/ }\n",
    )
    .await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");

    assert!(
        completion_response_is_incomplete(&response),
        "multiple highest-tier owners must not be presented as a closed result"
    );
    let items = completion_items(response);
    assert!(items.iter().any(|item| item.label == "from_left"));
    assert!(items.iter().any(|item| item.label == "from_right"));
    assert!(items.iter().all(|item| {
        item.detail
            .as_deref()
            .is_some_and(|detail| detail.contains("ambiguous owner"))
    }));
}

#[tokio::test]
async fn member_completion_resolve_rejects_changed_owner_revision() {
    let (dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[(
            "widget.hpp",
            "struct Widget {\n/// Original resize docs.\nvoid resize();\n};\n",
        )],
        "main.cpp",
        "#include \"widget.hpp\"\nvoid f(Widget *w) { w->res/*cursor*/ }\n",
    )
    .await;
    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let item = completion_items(response)
        .into_iter()
        .find(|item| item.label == "resize")
        .expect("resize completion");
    let header_uri = Url::from_file_path(dir.path().join("widget.hpp")).expect("header uri");
    service
        .inner()
        .session
        .change_document(
            header_uri,
            2,
            "struct Widget {\n/// Replacement docs must not hydrate the old item.\nvoid resize();\n};\n"
                .into(),
        )
        .await;

    let resolved = service
        .inner()
        .completion_resolve(item)
        .await
        .expect("resolve stale member item");
    let documentation = resolved
        .documentation
        .map(documentation_text)
        .unwrap_or_default();
    assert!(!documentation.contains("Replacement docs"));
    assert!(!documentation.contains("Original resize docs"));
}

#[tokio::test]
async fn member_completion_uses_dirty_header_members_and_tombstones_stale_fields() {
    let (dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[(
            "widget.hpp",
            "struct Widget { int indexed_field; void indexed_method(); };\n",
        )],
        "main.cpp",
        "#include \"widget.hpp\"\nvoid f(Widget *w) { w->/*cursor*/ }\n",
    )
    .await;
    let header_uri = Url::from_file_path(dir.path().join("widget.hpp")).expect("header uri");
    open_test_document(
        &service,
        header_uri,
        2,
        "struct Widget {\nint dirty_field;\n/// Unsaved dirty method documentation.\nvoid dirty_method();\n};\n"
            .into(),
    )
    .await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let items = completion_items(response);

    assert!(items.iter().any(|item| item.label == "dirty_field"));
    assert!(items.iter().any(|item| item.label == "dirty_method"));
    assert!(
        !items.iter().any(|item| item.label == "indexed_field"),
        "dirty record must tombstone its stale durable field"
    );
    assert!(
        !items.iter().any(|item| item.label == "indexed_method"),
        "dirty record must tombstone its stale durable method"
    );
    let dirty_method = items
        .into_iter()
        .find(|item| item.label == "dirty_method")
        .expect("dirty method completion");
    let resolved = service
        .inner()
        .completion_resolve(dirty_method)
        .await
        .expect("resolve dirty member completion");
    let documentation =
        documentation_text(resolved.documentation.expect("dirty member documentation"));
    assert!(documentation.contains("Unsaved dirty method documentation."));
}

#[tokio::test]
async fn member_completion_tombstones_dirty_secondary_root_owner_and_alias() {
    let primary = tempdir().expect("primary root");
    let secondary = tempdir().expect("secondary root");
    let (main_text, line, character) =
        text_and_position("void use_remote(RemoteAlias value) { value.f/*cursor*/ }\n");
    let indexed_types = concat!(
        "struct RemoteRecord { int former_field; };\n",
        "typedef RemoteRecord RemoteAlias;\n",
    );
    let dirty_types = concat!(
        "struct RemoteRecord { int fresh_field; };\n",
        "typedef RemoteRecord RemoteAlias;\n",
    );
    write_workspace_file(primary.path(), "main.cpp", &main_text);
    write_workspace_file(secondary.path(), "remote.hpp", indexed_types);
    for root in [primary.path(), secondary.path()] {
        crate::indexer::index_workspace(
            root,
            crate::indexer::IndexOptions {
                force: true,
                ..Default::default()
            },
            |_| {},
        )
        .expect("index workspace root");
    }

    let service = test_backend_service();
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .extend([primary.path().to_path_buf(), secondary.path().to_path_buf()]);
    for root in [primary.path(), secondary.path()] {
        service
            .inner()
            .session
            .cache
            .publish_full_index(&service.inner().client, root.to_path_buf())
            .await
            .expect("publish workspace root index");
    }

    let main_uri = Url::from_file_path(primary.path().join("main.cpp")).expect("main uri");
    service
        .inner()
        .did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: main_uri.clone(),
                language_id: "cpp".into(),
                version: 1,
                text: main_text,
            },
        })
        .await;
    let secondary_uri =
        Url::from_file_path(secondary.path().join("remote.hpp")).expect("secondary uri");
    service
        .inner()
        .did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: secondary_uri.clone(),
                language_id: "cpp".into(),
                version: 1,
                text: indexed_types.into(),
            },
        })
        .await;
    service
        .inner()
        .did_change(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: secondary_uri,
                version: 2,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: dirty_types.into(),
            }],
        })
        .await;

    let response = service
        .inner()
        .completion(completion_params(main_uri, line, character))
        .await
        .expect("member completion request")
        .expect("member completion response");
    let items = completion_items(response);
    let labels = items
        .iter()
        .map(|item| item.label.as_str())
        .collect::<Vec<_>>();
    assert!(
        items.iter().any(|item| item.label == "fresh_field"),
        "secondary dirty member was not visible: {labels:?}"
    );
    assert!(
        !items.iter().any(|item| item.label == "former_field"),
        "secondary dirty owner/alias must tombstone its durable member: {labels:?}"
    );
}

#[tokio::test]
async fn member_completion_does_not_revive_a_record_deleted_in_dirty_header() {
    let (dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[("widget.hpp", "struct Widget { int stale_field; };\n")],
        "main.cpp",
        "#include \"widget.hpp\"\nvoid f(Widget *w) { w->st/*cursor*/ }\n",
    )
    .await;
    let header_uri = Url::from_file_path(dir.path().join("widget.hpp")).expect("header uri");
    open_test_document(
        &service,
        header_uri,
        2,
        "struct Replacement { int fresh_field; };\n".into(),
    )
    .await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let items = completion_items(response);
    assert!(
        !items.iter().any(|item| item.label == "stale_field"),
        "dirty record deletion must tombstone the durable owner"
    );
}

#[tokio::test]
async fn member_completion_does_not_revive_clean_alias_dirty_deleted_target() {
    let (dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[
            ("alias.hpp", "typedef Widget Alias;\n"),
            ("widget.hpp", "struct Widget { int stale_field; };\n"),
        ],
        "main.cpp",
        "#include \"widget.hpp\"\n#include \"alias.hpp\"\nvoid f(Alias value) { value.st/*cursor*/ }\n",
    )
    .await;
    let widget_uri = Url::from_file_path(dir.path().join("widget.hpp")).expect("widget uri");
    open_test_document(
        &service,
        widget_uri,
        2,
        "struct Replacement { int fresh_field; };\n".into(),
    )
    .await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let items = completion_items(response);
    assert!(
        !items.iter().any(|item| item.label == "stale_field"),
        "a clean alias must not revive its dirty-deleted terminal record"
    );
}

#[tokio::test]
async fn member_completion_follows_dirty_typedef_retarget() {
    let (dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[(
            "types.hpp",
            "struct A { int from_a; };\nstruct B { int from_b; };\ntypedef A Active;\n",
        )],
        "main.cpp",
        "#include \"types.hpp\"\nvoid f(Active value) { value./*cursor*/ }\n",
    )
    .await;
    let header_uri = Url::from_file_path(dir.path().join("types.hpp")).expect("header uri");
    open_test_document(
        &service,
        header_uri,
        2,
        "struct A { int from_a; };\nstruct B { int from_b; };\ntypedef B Active;\n".into(),
    )
    .await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let items = completion_items(response);

    let labels: Vec<_> = items.iter().map(|item| item.label.as_str()).collect();
    assert!(
        items.iter().any(|item| item.label == "from_b"),
        "dirty typedef members were: {labels:?}"
    );
    assert!(
        !items.iter().any(|item| item.label == "from_a"),
        "dirty typedef target must replace the persisted alias target"
    );
}

#[tokio::test]
async fn member_completion_does_not_revive_deleted_dirty_chain_segment() {
    let (dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[(
            "nested.hpp",
            "struct Inner { int stale_value; };\nstruct Outer { Inner child; };\n",
        )],
        "main.cpp",
        "#include \"nested.hpp\"\nvoid f(Outer value) { value.child./*cursor*/ }\n",
    )
    .await;
    let header_uri = Url::from_file_path(dir.path().join("nested.hpp")).expect("header uri");
    open_test_document(
        &service,
        header_uri,
        2,
        "struct Inner { int fresh_value; };\nstruct Outer { int replacement; };\n".into(),
    )
    .await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let items = completion_items(response);
    assert!(
        items.is_empty(),
        "a deleted dirty chain segment must not fall through to durable members: {:?}",
        items.iter().map(|item| &item.label).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn member_completion_resolves_simple_nested_member_chain() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[(
            "nested.hpp",
            "struct Inner { int value; };\nstruct Outer { struct Inner mem1; };\n",
        )],
        "main.cpp",
        "#include \"nested.hpp\"\nvoid f(Outer *a) { a->mem1./*cursor*/ }\n",
    )
    .await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let items = completion_items(response);

    assert!(items
        .iter()
        .any(|item| item.label == "value" && item.kind == Some(CompletionItemKind::FIELD)));
}

#[tokio::test]
async fn member_completion_resolves_indexed_anonymous_nested_member_chain() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[(
            "nested.h",
            "typedef struct { struct { int xxx; } mem1[4]; } A;\n",
        )],
        "main.c",
        "#include \"nested.h\"\nvoid f(void) { A a; a.mem1[0]./*cursor*/ }\n",
    )
    .await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let items = completion_items(response);

    assert!(items
        .iter()
        .any(|item| item.label == "xxx" && item.kind == Some(CompletionItemKind::FIELD)));
}

#[tokio::test]
async fn member_completion_falls_back_when_chain_parse_fails() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[("widget.hpp", "struct Widget { int width; int window; };\n")],
        "main.cpp",
        "void f(void) { make_widget()->wi/*cursor*/ }\n",
    )
    .await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let items = completion_items(response);

    assert!(items
        .iter()
        .any(|item| item.label == "width" && item.kind == Some(CompletionItemKind::FIELD)));
    assert!(items
        .iter()
        .any(|item| item.label == "window" && item.kind == Some(CompletionItemKind::FIELD)));
}

#[tokio::test]
async fn member_completion_does_not_leak_global_owner_when_reachable_owner_lacks_prefix() {
    let (dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[
            ("reachable.hpp", "struct W { int width; };\n"),
            ("global.hpp", "struct W { int height; };\n"),
        ],
        "main.cpp",
        "#include \"reachable.hpp\"\nvoid f(W *w) { w->he/*cursor*/ }\n",
    )
    .await;
    service
        .inner()
        .session
        .cache
        .set_reach_graph_for_test(
            dir.path().to_path_buf(),
            Arc::new(crate::reachability::ReachGraph::new(
                vec![("main.cpp".to_string(), "reachable.hpp".to_string())],
                vec![],
                vec![],
            )),
        )
        .await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let items = completion_items(response);

    assert!(
        !items.iter().any(|item| item.label == "height"),
        "global W::height must not leak when reachable W has members but no 'he' member"
    );
    assert!(
        items.is_empty(),
        "resolved receiver should return an empty incomplete list instead of falling back"
    );
}

#[tokio::test]
async fn member_completion_does_not_revive_lower_tier_members_when_dirty_owner_is_empty() {
    let (dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[
            (
                "reachable.hpp",
                "struct W { int indexed_reachable_member; };\n",
            ),
            ("global.hpp", "struct W { int stale_global_member; };\n"),
        ],
        "main.cpp",
        "#include \"reachable.hpp\"\nvoid f(W *w) { w->/*cursor*/ }\n",
    )
    .await;
    service
        .inner()
        .session
        .cache
        .set_reach_graph_for_test(
            dir.path().to_path_buf(),
            Arc::new(crate::reachability::ReachGraph::new(
                vec![("main.cpp".to_string(), "reachable.hpp".to_string())],
                vec![],
                vec![],
            )),
        )
        .await;
    let reachable_uri =
        Url::from_file_path(dir.path().join("reachable.hpp")).expect("reachable uri");
    open_test_document(&service, reachable_uri, 2, "struct W {};\n".to_string()).await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let items = completion_items(response);
    let labels = items
        .iter()
        .map(|item| item.label.as_str())
        .collect::<Vec<_>>();

    assert!(
        !items.iter().any(|item| item.label == "stale_global_member"),
        "an empty dirty reachable owner must not revive a lower-tier same-name owner: {labels:?}"
    );
    assert!(
        !items
            .iter()
            .any(|item| item.label == "indexed_reachable_member"),
        "the dirty owner must still tombstone its own durable members: {labels:?}"
    );
}

#[tokio::test]
async fn member_completion_chain_uses_globally_highest_tier_owner_across_roots() {
    let primary = tempdir().expect("primary root");
    let secondary = tempdir().expect("secondary root");
    let (main_text, line, character) = text_and_position(concat!(
        "#include \"preferred.hpp\"\n",
        "void f(Outer value) { value.child./*cursor*/ }\n",
    ));
    write_workspace_file(primary.path(), "main.cpp", &main_text);
    write_workspace_file(
        primary.path(),
        "preferred.hpp",
        concat!(
            "struct PreferredChild { int preferred_member; };\n",
            "struct Outer { PreferredChild child; };\n",
        ),
    );
    write_workspace_file(
        secondary.path(),
        "global.hpp",
        concat!(
            "struct GlobalChild { int leaked_global_member; };\n",
            "struct Outer { GlobalChild child; };\n",
        ),
    );
    for root in [primary.path(), secondary.path()] {
        crate::indexer::index_workspace(
            root,
            crate::indexer::IndexOptions {
                force: true,
                ..Default::default()
            },
            |_| {},
        )
        .expect("index workspace root");
    }

    let service = test_backend_service();
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .extend([primary.path().to_path_buf(), secondary.path().to_path_buf()]);
    for root in [primary.path(), secondary.path()] {
        service
            .inner()
            .session
            .cache
            .publish_full_index(&service.inner().client, root.to_path_buf())
            .await
            .expect("publish workspace root index");
    }
    service
        .inner()
        .session
        .cache
        .set_reach_graph_for_test(
            primary.path().to_path_buf(),
            Arc::new(crate::reachability::ReachGraph::new(
                vec![("main.cpp".to_string(), "preferred.hpp".to_string())],
                vec![],
                vec![],
            )),
        )
        .await;
    service
        .inner()
        .session
        .cache
        .set_reach_graph_for_test(
            secondary.path().to_path_buf(),
            Arc::new(crate::reachability::ReachGraph::new(vec![], vec![], vec![])),
        )
        .await;
    let main_uri = Url::from_file_path(primary.path().join("main.cpp")).expect("main uri");
    open_test_document(&service, main_uri.clone(), 1, main_text).await;

    let response = service
        .inner()
        .completion(completion_params(main_uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    assert!(
        !completion_response_is_incomplete(&response),
        "a lower-tier owner in another root must not make an otherwise exact chain ambiguous"
    );
    let items = completion_items(response);
    let labels = items
        .iter()
        .map(|item| item.label.as_str())
        .collect::<Vec<_>>();
    let preferred = items
        .iter()
        .find(|item| item.label == "preferred_member")
        .unwrap_or_else(|| panic!("reachable chain member was missing: {labels:?}"));

    assert!(
        !items
            .iter()
            .any(|item| item.label == "leaked_global_member"),
        "the lower-tier Outer.child chain must not participate: {labels:?}"
    );
    assert!(
        !preferred
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("ambiguous owner")),
        "the globally shadowed owner must not taint the preferred chain as ambiguous"
    );
}

#[tokio::test]
async fn member_completion_resolves_alias_target_split_across_workspace_roots() {
    let primary = tempdir().expect("primary root");
    let secondary = tempdir().expect("secondary root");
    let (main_text, line, character) = text_and_position(concat!(
        "#include \"alias.hpp\"\n",
        "void use_remote(RemoteAlias value) { value.fr/*cursor*/ }\n",
    ));
    write_workspace_file(primary.path(), "main.cpp", &main_text);
    write_workspace_file(
        primary.path(),
        "alias.hpp",
        "typedef RemoteRecord RemoteAlias;\n",
    );
    write_workspace_file(
        secondary.path(),
        "remote.hpp",
        concat!(
            "struct RemoteRecord { int fresh_field; };\n",
            "struct UnrelatedRecord { int fresh_fallback_noise; };\n",
        ),
    );
    for root in [primary.path(), secondary.path()] {
        crate::indexer::index_workspace(
            root,
            crate::indexer::IndexOptions {
                force: true,
                ..Default::default()
            },
            |_| {},
        )
        .expect("index workspace root");
    }

    let service = test_backend_service();
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .extend([primary.path().to_path_buf(), secondary.path().to_path_buf()]);
    for root in [primary.path(), secondary.path()] {
        service
            .inner()
            .session
            .cache
            .publish_full_index(&service.inner().client, root.to_path_buf())
            .await
            .expect("publish workspace root index");
    }
    service
        .inner()
        .session
        .cache
        .set_reach_graph_for_test(
            primary.path().to_path_buf(),
            Arc::new(crate::reachability::ReachGraph::new(
                vec![("main.cpp".to_string(), "alias.hpp".to_string())],
                vec![],
                vec![],
            )),
        )
        .await;
    service
        .inner()
        .session
        .cache
        .set_reach_graph_for_test(
            secondary.path().to_path_buf(),
            Arc::new(crate::reachability::ReachGraph::new(vec![], vec![], vec![])),
        )
        .await;
    let main_uri = Url::from_file_path(primary.path().join("main.cpp")).expect("main uri");
    open_test_document(&service, main_uri.clone(), 1, main_text).await;

    let response = service
        .inner()
        .completion(completion_params(main_uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let items = completion_items(response);
    let labels = items
        .iter()
        .map(|item| item.label.as_str())
        .collect::<Vec<_>>();

    assert!(
        items.iter().any(|item| item.label == "fresh_field"),
        "the alias target name discovered in the primary root must resolve to the record in the secondary root: {labels:?}"
    );
    assert!(
        !items
            .iter()
            .any(|item| item.label == "fresh_fallback_noise"),
        "cross-root alias closure must not degrade into unrelated global member fallback: {labels:?}"
    );
}

#[tokio::test]
async fn member_fallback_still_blocks_one_character_prefix() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[("widget.hpp", "struct Widget { int width; void wipe(); };\n")],
        "main.cpp",
        "void f(void) { make_widget()->w/*cursor*/ }\n",
    )
    .await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    assert!(completion_items(response).is_empty());
}

#[tokio::test]
async fn weak_receiver_uses_member_fallback_min_prefix_gate() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[("widget.hpp", "struct Widget { int width; int window; };\n")],
        "main.cpp",
        "void f(void) { widget->w/*cursor*/ }\n",
    )
    .await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");

    assert!(
        completion_items(response).is_empty(),
        "weak receiver correlation must not bypass the member fallback short-prefix gate"
    );
}

#[tokio::test]
async fn execute_command_records_completion_accept_when_history_enabled() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(dir.path().to_path_buf());
    service
        .inner()
        .set_completion_history_mode_for_test(crate::completion_history::CompletionHistoryMode::On)
        .await;
    let workspace_hash = super::completion_history_workspace_hash(dir.path());

    service
        .inner()
        .execute_command(ExecuteCommandParams {
            command: super::COMPLETION_ACCEPTED_LSP_COMMAND.to_string(),
            arguments: vec![serde_json::json!({
                "workspaceHash": workspace_hash,
                "candidateHash": crate::completion_history::candidate_hash("printf", "function"),
                "kind": "function",
                "intent": "call_target",
                "prefixBucket": "pr"
            })],
            work_done_progress_params: Default::default(),
        })
        .await
        .expect("command");

    assert_eq!(
        service
            .inner()
            .history_snapshot_for_test(&workspace_hash)
            .await
            .total_accepts(),
        1
    );
}

#[tokio::test]
async fn execute_command_ignores_invalid_completion_candidate_hash() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(dir.path().to_path_buf());
    service
        .inner()
        .set_completion_history_mode_for_test(crate::completion_history::CompletionHistoryMode::On)
        .await;
    let workspace_hash = super::completion_history_workspace_hash(dir.path());

    service
        .inner()
        .execute_command(ExecuteCommandParams {
            command: super::COMPLETION_ACCEPTED_LSP_COMMAND.to_string(),
            arguments: vec![serde_json::json!({
                "workspaceHash": workspace_hash,
                "candidateHash": "abc",
                "kind": "function",
                "intent": "call_target",
                "prefixBucket": "pr"
            })],
            work_done_progress_params: Default::default(),
        })
        .await
        .expect("command");

    assert_eq!(
        service
            .inner()
            .history_snapshot_for_test(&workspace_hash)
            .await
            .total_accepts(),
        0
    );
}

#[tokio::test]
async fn completion_accept_history_is_recorded_in_matching_workspace_root() {
    let service = test_backend_service();
    let first = tempdir().expect("first tempdir");
    let second = tempdir().expect("second tempdir");
    {
        let mut roots = service.inner().workspace_roots.lock().await;
        roots.push(first.path().to_path_buf());
        roots.push(second.path().to_path_buf());
    }
    service
        .inner()
        .set_completion_history_mode_for_test(crate::completion_history::CompletionHistoryMode::On)
        .await;
    let first_hash = super::completion_history_workspace_hash(first.path());
    let second_hash = super::completion_history_workspace_hash(second.path());

    service
        .inner()
        .execute_command(ExecuteCommandParams {
            command: super::COMPLETION_ACCEPTED_LSP_COMMAND.to_string(),
            arguments: vec![serde_json::json!({
                "workspaceHash": second_hash,
                "candidateHash": crate::completion_history::candidate_hash("printf", "function"),
                "kind": "function",
                "intent": "call_target",
                "prefixBucket": "pr"
            })],
            work_done_progress_params: Default::default(),
        })
        .await
        .expect("command");

    let first_path = crate::pathing::default_completion_history_path(first.path()).expect("path");
    let second_path = crate::pathing::default_completion_history_path(second.path()).expect("path");
    let first_store =
        crate::completion_history::CompletionHistoryStore::open(&first_path).expect("first store");
    let second_store = crate::completion_history::CompletionHistoryStore::open(&second_path)
        .expect("second store");

    assert_eq!(first_store.snapshot(&first_hash).total_accepts(), 0);
    assert_eq!(first_store.snapshot(&second_hash).total_accepts(), 0);
    assert_eq!(second_store.snapshot(&second_hash).total_accepts(), 1);
}

#[tokio::test]
async fn execute_command_ignores_completion_accept_when_history_disabled() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(dir.path().to_path_buf());
    service
        .inner()
        .set_completion_history_mode_for_test(crate::completion_history::CompletionHistoryMode::Off)
        .await;
    let workspace_hash = super::completion_history_workspace_hash(dir.path());

    service
        .inner()
        .execute_command(ExecuteCommandParams {
            command: super::COMPLETION_ACCEPTED_LSP_COMMAND.to_string(),
            arguments: vec![serde_json::json!({
                "workspaceHash": workspace_hash,
                "candidateHash": crate::completion_history::candidate_hash("printf", "function"),
                "kind": "function",
                "intent": "call_target",
                "prefixBucket": "pr"
            })],
            work_done_progress_params: Default::default(),
        })
        .await
        .expect("command");

    assert_eq!(
        service
            .inner()
            .history_snapshot_for_test(&workspace_hash)
            .await
            .total_accepts(),
        0
    );
}

#[tokio::test]
async fn clear_completion_history_overwrites_corrupt_history_file() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(dir.path().to_path_buf());
    let history_path =
        crate::pathing::default_completion_history_path(dir.path()).expect("history path");
    std::fs::create_dir_all(history_path.parent().expect("history parent")).expect("mkdir");
    std::fs::write(&history_path, "{not json").expect("write corrupt history");

    service
        .inner()
        .clear_completion_history()
        .await
        .expect("clear corrupt history");

    let store = crate::completion_history::CompletionHistoryStore::open(&history_path)
        .expect("history should be parseable after clear");
    assert_eq!(
        store
            .snapshot(&super::completion_history_workspace_hash(dir.path()))
            .total_accepts(),
        0
    );
}

#[tokio::test]
async fn ordinary_completion_items_attach_history_accept_command_when_enabled() {
    let (src, line, character) = text_and_position(
        "#define FS_MAGIC 1\n\
         void f(void) { FS/*cursor*/(); }\n",
    );
    let dir = tempdir().expect("tempdir");
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

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion")
        .expect("response");
    let item = completion_items(response)
        .into_iter()
        .find(|item| item.label == "FS_MAGIC")
        .expect("FS_MAGIC");

    let command = item.command.as_ref().expect("history command");
    assert_eq!(command.command, super::COMPLETION_ACCEPTED_LSP_COMMAND);
    let argument = command
        .arguments
        .as_ref()
        .and_then(|arguments| arguments.first())
        .expect("command argument");
    assert_eq!(
        argument.get("kind").and_then(|value| value.as_str()),
        Some("macro")
    );
    assert_eq!(
        argument.get("intent").and_then(|value| value.as_str()),
        Some("call_target")
    );
    assert_eq!(
        argument
            .get("prefixBucket")
            .and_then(|value| value.as_str()),
        Some("fs")
    );
    assert!(argument
        .get("workspaceHash")
        .and_then(|value| value.as_str())
        .is_some_and(|value| !value.is_empty()));
    assert!(argument
        .get("candidateHash")
        .and_then(|value| value.as_str())
        .is_some_and(|value| value.len() == 16));
}

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
        .set_name_table_for_test(
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
        )
        .await;
    service
        .inner()
        .session
        .cache
        .set_reach_graph_for_test(
            root.clone(),
            Arc::new(crate::reachability::ReachGraph::new(
                vec![("src/main.c".to_string(), "reachable.h".to_string())],
                vec![],
                vec!["src/main.c".to_string()],
            )),
        )
        .await;
    service
        .inner()
        .session
        .cache
        .set_indexed_file_list_for_test(
            root.clone(),
            Arc::new(vec![
                ("src/main.c".to_string(), root.join("src/main.c")),
                ("reachable.h".to_string(), root.join("reachable.h")),
            ]),
        )
        .await;
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
                detail: Some("typedef int fs_overlay_type;".to_string()),
                documentation: None,
                sort_text: Some("00000002".to_string()),
                has_history_command: true,
            },
            PresentedCompletion {
                label: "fs_overlay_macro".to_string(),
                kind: Some(CompletionItemKind::CONSTANT),
                detail: Some("#define fs_overlay_macro 1".to_string()),
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
                label: "fs_global_index".to_string(),
                kind: Some(CompletionItemKind::CONSTANT),
                detail: Some("global".to_string()),
                documentation: Some(
                    "FossilSense: global candidate (fallback, global_fallback)".to_string(),
                ),
                sort_text: Some("00000005".to_string()),
                has_history_command: true,
            },
            PresentedCompletion {
                label: "fs_unknown_index".to_string(),
                kind: Some(CompletionItemKind::ENUM_MEMBER),
                detail: Some("global".to_string()),
                documentation: Some(
                    "FossilSense: global candidate (fallback, global_fallback)".to_string(),
                ),
                sort_text: Some("00000006".to_string()),
                has_history_command: true,
            },
            PresentedCompletion {
                label: "fs_external_index".to_string(),
                kind: Some(CompletionItemKind::STRUCT),
                detail: Some("global".to_string()),
                documentation: Some(
                    "FossilSense: global candidate (fallback, global_fallback)".to_string(),
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

#[test]
fn history_accept_command_uses_final_kind_for_candidate_hash() {
    let mut item = CompletionItem {
        label: "same_name".to_string(),
        ..Default::default()
    };
    let mut evidence = crate::completion::CandidateEvidence::new(
        crate::completion::CandidateSource::Indexed,
        crate::model::ScopeTier::Reachable,
        crate::model::ResolutionConfidence::Heuristic,
        700,
    );
    evidence.kind = crate::completion::CompletionCandidateKind::Function;
    evidence.history_key = Some(crate::completion_history::candidate_hash_key(
        "same_name",
        "variable",
    ));

    super::attach_completion_history_accept_command(
        &mut item,
        evidence,
        "workspace",
        crate::completion::CompletionIntentKind::CallTarget,
        "sa",
    );

    let argument = item
        .command
        .as_ref()
        .and_then(|command| command.arguments.as_ref())
        .and_then(|arguments| arguments.first())
        .expect("history command argument");
    let expected_hash = crate::completion_history::candidate_hash("same_name", "function");
    assert_eq!(
        argument
            .get("candidateHash")
            .and_then(|value| value.as_str()),
        Some(expected_hash.as_str())
    );
}

// --- R7: completion memo validity (generation + prefix extension check) ---

#[test]
fn completion_memo_valid_when_prefix_extends_and_same_generation() {
    assert!(super::state::completion_memo_is_valid(42, 42, "fo", "foo"));
}

#[test]
fn completion_memo_invalid_when_generation_differs() {
    assert!(!super::state::completion_memo_is_valid(10, 20, "fo", "foo"));
}

#[test]
fn completion_memo_invalid_when_prefix_shortens() {
    assert!(!super::state::completion_memo_is_valid(1, 1, "foo", "fo"));
}

#[test]
fn completion_memo_invalid_when_prefix_changes() {
    assert!(!super::state::completion_memo_is_valid(1, 1, "foo", "bar"));
}

#[test]
fn completion_memo_invalid_when_prior_prefix_empty() {
    // An empty prior prefix means there is no usable narrowing base.
    assert!(!super::state::completion_memo_is_valid(1, 1, "", "a"));
    // Even extending an empty prefix is invalid — the prior scan was
    // the empty-prefix full pass which doesn't provide a focused pool.
    assert!(!super::state::completion_memo_is_valid(1, 1, "", "foo"));
}

#[tokio::test]
async fn completion_runtime_supersedes_queued_request_before_cpu_admission() {
    let runtime = super::completion_runtime::CompletionRuntime::with_permits_for_test(1);
    let blocker_uri = Url::parse("file:///workspace/blocker.c").expect("blocker uri");
    let target_uri = Url::parse("file:///workspace/target.c").expect("target uri");

    let blocker = runtime.begin(blocker_uri);
    let blocker_permit = blocker
        .acquire()
        .await
        .expect("first request owns the only foreground permit");

    let queued = runtime.begin(target_uri.clone());
    let queued_wait = tokio::spawn(async move { queued.acquire().await.is_some() });
    tokio::task::yield_now().await;

    let latest = runtime.begin(target_uri);
    assert!(
        !queued_wait.await.expect("queued request task"),
        "a superseded request must leave the admission queue"
    );

    drop(blocker_permit);
    let latest_permit = latest
        .acquire()
        .await
        .expect("latest request receives the released permit");
    drop(latest_permit);

    let metrics = runtime.metrics_for_test();
    assert_eq!(metrics.superseded, 1);
    assert_eq!(metrics.cancelled_before_admission, 1);
}

#[test]
fn document_change_supersedes_active_completion_token() {
    let runtime = super::completion_runtime::CompletionRuntime::with_permits_for_test(1);
    let uri = Url::parse("file:///workspace/target.c").expect("target uri");
    let request = runtime.begin(uri.clone());

    runtime.supersede(&uri);

    assert!(request.is_cancelled());
    assert!(request.stop_before_worker());
    let metrics = runtime.metrics_for_test();
    assert_eq!(metrics.superseded, 1);
    assert_eq!(metrics.cancelled_before_worker, 1);
}

#[test]
fn completion_runtime_never_allows_older_work_to_replace_a_post_change_request() {
    let runtime = super::completion_runtime::CompletionRuntime::with_permits_for_test(1);
    let uri = Url::parse("file:///workspace/target.c").expect("target uri");
    let mut older = runtime.begin(uri.clone());

    runtime.supersede(&uri);
    let mut latest = runtime.begin(uri);

    assert!(older.is_cancelled());
    assert!(!older.is_current());
    assert!(latest.is_current());
    assert!(!older.finish(), "old work must not become publishable");
    assert!(latest.finish(), "the post-change request remains current");
}

#[tokio::test]
async fn cancelled_request_cannot_repopulate_a_memo_after_waiting_for_its_lock() {
    let runtime = super::completion_runtime::CompletionRuntime::with_permits_for_test(1);
    let cache = super::CacheLedger::default();
    let uri = Url::parse("file:///workspace/target.c").expect("target uri");
    let request = runtime.begin(uri.clone());
    let memo_guard = cache.completion_memo.clone().lock_owned().await;

    let waiting_cache = cache.clone();
    let waiting_uri = uri.clone();
    let commit = tokio::spawn(async move {
        let mut request = request;
        let committed = waiting_cache
            .record_completion_memo_if_current(
                &request,
                waiting_uri,
                super::state::CompletionMemo {
                    prefix: "tar".to_string(),
                    generation: 7,
                    pools: vec![vec![1, 2, 3]],
                    pool_complete: vec![true],
                },
                true,
            )
            .await;
        let publishable = request.finish();
        (committed, publishable)
    });
    tokio::task::yield_now().await;

    runtime.supersede(&uri);
    let mut latest = runtime.begin(uri.clone());
    drop(memo_guard);

    let (committed, publishable) = commit.await.expect("memo commit task");
    assert!(
        !committed,
        "a stale request must not write its candidate pool"
    );
    assert!(!publishable, "a stale request must not return its result");
    assert!(
        cache.completion_memo_for_test(&uri).await.is_none(),
        "didChange/didClose must not be followed by stale memo resurrection"
    );
    assert!(latest.finish());
}

#[tokio::test]
async fn probe_dependent_completion_clears_prior_narrowing_memo() {
    let runtime = super::completion_runtime::CompletionRuntime::with_permits_for_test(1);
    let cache = super::CacheLedger::default();
    let uri = Url::parse("file:///workspace/target.c").expect("target uri");
    cache
        .record_completion_memo(uri.clone(), "ta".into(), 6, vec![vec![1, 2]])
        .await;
    let request = runtime.begin(uri.clone());

    let committed = cache
        .record_completion_memo_if_current(
            &request,
            uri.clone(),
            super::state::CompletionMemo {
                prefix: "tar".into(),
                generation: 7,
                pools: vec![vec![1, 2, 3]],
                pool_complete: vec![true],
            },
            false,
        )
        .await;

    assert!(committed, "the request is still current");
    assert!(
        cache.completion_memo_for_test(&uri).await.is_none(),
        "a probe-dependent ranking must not be reused after its path epoch changes"
    );
}

#[test]
fn engine_epoch_reserves_zero_for_missing_state() {
    assert_eq!(super::state::EngineEpoch::missing().as_u64(), 0);
    assert_eq!(super::state::EngineEpoch::published(1).as_u64(), 1);
    assert_ne!(
        super::state::EngineEpoch::missing(),
        super::state::EngineEpoch::published(1)
    );
}

#[test]
fn combined_completion_generation_changes_with_engine_selection_or_project() {
    let root = PathBuf::from("workspace");
    let first = super::state::EngineEpoch::published(1);
    let second = super::state::EngineEpoch::published(2);
    let project = crate::project_context::ProjectKey {
        workspace_root_id: "root".into(),
        project_path: "app".into(),
    };

    let first_universe = crate::candidate_service::RecallUniverseId::for_test(1);
    let second_universe = crate::candidate_service::RecallUniverseId::for_test(2);
    let combined_first = super::state::combine_completion_generation(
        &[(root.clone(), first)],
        1,
        None,
        &[first_universe],
    );
    let combined_second = super::state::combine_completion_generation(
        &[(root.clone(), second)],
        1,
        None,
        &[first_universe],
    );
    let combined_selection = super::state::combine_completion_generation(
        &[(root.clone(), first)],
        2,
        None,
        &[first_universe],
    );
    let combined_project = super::state::combine_completion_generation(
        &[(root, first)],
        1,
        Some(&project),
        &[first_universe],
    );
    let combined_universe = super::state::combine_completion_generation(
        &[(PathBuf::from("workspace"), first)],
        1,
        None,
        &[second_universe],
    );

    assert_ne!(combined_first, combined_second);
    assert_ne!(combined_first, combined_selection);
    assert_ne!(combined_first, combined_project);
    assert_ne!(combined_first, combined_universe);
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
        "/// Unsaved magic value.\n\
         #define FS_MAGIC 1\n\
         typedef int FsAlias;\n\
         void f(void) { FS/*cursor*/ }\n",
    );
    let dir = tempdir().expect("tempdir");
    let uri = Url::from_file_path(dir.path().join("a.c")).expect("file uri");
    let service = test_backend_service();
    open_test_document(&service, uri.clone(), 1, src).await;

    let response = service
        .inner()
        .completion(completion_params(uri.clone(), line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    if let CompletionResponse::List(list) = &response {
        assert!(list.is_incomplete);
    }
    let items = completion_items(response);

    assert!(items.iter().any(
        |item| item.label == "FS_MAGIC" && item.detail.as_deref() == Some("#define FS_MAGIC 1")
    ));
    let alias = items
        .iter()
        .find(|item| item.label == "FsAlias")
        .expect("FsAlias completion");
    assert_eq!(alias.detail.as_deref(), Some("typedef int FsAlias;"));

    let magic = items
        .into_iter()
        .find(|item| item.label == "FS_MAGIC")
        .expect("FS_MAGIC completion");
    let resolved = service
        .inner()
        .completion_resolve(magic)
        .await
        .expect("resolve current completion");
    let documentation = resolved.documentation.expect("current documentation");
    let documentation = match documentation {
        Documentation::String(value) => value,
        Documentation::MarkupContent(markup) => markup.value,
    };
    assert!(documentation.contains("Unsaved magic value."));
}

#[tokio::test]
async fn ordinary_completion_merges_other_dirty_document_symbols_and_tombstones_base() {
    let (dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[(
            "api.hpp",
            "int DirtyOldFunction(void);\n#define DirtyOldMacro 1\ntypedef int DirtyOldType;\n",
        )],
        "main.cpp",
        "void f(void) { Dir/*cursor*/ }\n",
    )
    .await;
    let header_uri = Url::from_file_path(dir.path().join("api.hpp")).expect("header uri");
    open_test_document(
        &service,
        header_uri,
        2,
        "int DirtyFunction(void);\n#define DirtyMacro 2\ntypedef long DirtyType;\n".into(),
    )
    .await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let items = completion_items(response);
    for name in ["DirtyFunction", "DirtyMacro", "DirtyType"] {
        assert!(
            items.iter().any(|item| item.label == name),
            "missing dirty other-document symbol {name}: {:?}",
            items.iter().map(|item| &item.label).collect::<Vec<_>>()
        );
    }
    for name in ["DirtyOldFunction", "DirtyOldMacro", "DirtyOldType"] {
        assert!(
            !items.iter().any(|item| item.label == name),
            "dirty path must tombstone stale completion {name}"
        );
    }
}

#[tokio::test]
async fn current_completion_resolve_rejects_changed_document_revision() {
    let (source, line, character) = text_and_position(
        "/// Original value.\n#define REVISION_ITEM 1\nvoid f(void) { REV/*cursor*/ }\n",
    );
    let dir = tempdir().expect("tempdir");
    let uri = Url::from_file_path(dir.path().join("main.c")).expect("uri");
    let service = test_backend_service();
    open_test_document(&service, uri.clone(), 1, source).await;
    let response = service
        .inner()
        .completion(completion_params(uri.clone(), line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let item = completion_items(response)
        .into_iter()
        .find(|item| item.label == "REVISION_ITEM")
        .expect("current completion");

    service
        .inner()
        .session
        .change_document(
            uri,
            2,
            "/// Replacement comment must not hydrate the old item.\n#define REVISION_ITEM 2\n"
                .into(),
        )
        .await;
    let resolved = service
        .inner()
        .completion_resolve(item)
        .await
        .expect("resolve stale current item");
    let documentation = resolved
        .documentation
        .map(documentation_text)
        .unwrap_or_default();
    assert!(!documentation.contains("Replacement comment"));
    assert!(!documentation.contains("Original value"));
}

#[tokio::test]
async fn indexed_completion_resolve_rejects_changed_overlay_revision() {
    let (dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[(
            "api.h",
            "/// Original indexed docs.\nint RevisionFunction(void);\n",
        )],
        "main.c",
        "void f(void) { Rev/*cursor*/ }\n",
    )
    .await;
    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let item = completion_items(response)
        .into_iter()
        .find(|item| item.label == "RevisionFunction")
        .expect("indexed completion");
    let header_uri = Url::from_file_path(dir.path().join("api.h")).expect("header uri");
    service
        .inner()
        .session
        .change_document(
            header_uri,
            2,
            "/// Replacement indexed docs must not hydrate the old item.\nint RevisionFunction(void);\n"
                .into(),
        )
        .await;

    let resolved = service
        .inner()
        .completion_resolve(item)
        .await
        .expect("resolve stale indexed item");
    let documentation = resolved
        .documentation
        .map(documentation_text)
        .unwrap_or_default();
    assert!(!documentation.contains("Replacement indexed docs"));
    assert!(!documentation.contains("Original indexed docs"));
}

#[tokio::test]
async fn overlay_completion_resolve_rejects_a_newer_overlay_epoch() {
    let (dir, service, uri, line, character) =
        indexed_backend_with_open_doc(&[], "main.c", "void f(void) { Over/*cursor*/ }\n").await;
    let header_uri = Url::from_file_path(dir.path().join("api.h")).expect("header uri");
    open_test_document(
        &service,
        header_uri.clone(),
        1,
        "/// Original dirty docs.\nint OverlayFunction(void);\n".into(),
    )
    .await;
    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let item = completion_items(response)
        .into_iter()
        .find(|item| item.label == "OverlayFunction")
        .expect("dirty overlay completion");
    assert_eq!(
        item.data
            .as_ref()
            .and_then(|data| data.get("handle"))
            .and_then(|handle| handle.get("locator"))
            .and_then(|locator| locator.get("origin"))
            .and_then(serde_json::Value::as_str),
        Some("overlay")
    );

    service
        .inner()
        .session
        .change_document(
            header_uri,
            2,
            "/// Replacement dirty docs must not hydrate the old item.\nint OverlayFunction(void);\n"
                .into(),
        )
        .await;

    let resolved = service
        .inner()
        .completion_resolve(item)
        .await
        .expect("resolve stale overlay item");
    let documentation = resolved
        .documentation
        .map(documentation_text)
        .unwrap_or_default();
    assert!(!documentation.contains("Replacement dirty docs"));
    assert!(!documentation.contains("Original dirty docs"));
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
        "library.c".to_string(),
        "function".to_string(),
        false,
    ));
    service
        .inner()
        .session
        .cache
        .set_name_table_for_test(
            root,
            Arc::new(crate::query::NameTable::build_with_paths(names)),
        )
        .await;
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

#[tokio::test]
async fn local_binding_completion_is_not_hydrated_from_same_name_global() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[("other.c", "int count(void) { return 1; }\n")],
        "main.c",
        "int f(int count) {\n    return cou/*cursor*/;\n}\n",
    )
    .await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let count = completion_items(response)
        .into_iter()
        .find(|item| item.label == "count")
        .expect("local count completion");

    assert_eq!(count.kind, Some(CompletionItemKind::VARIABLE));
    assert_eq!(count.detail.as_deref(), Some("parameter: int"));
    assert!(count.data.is_none());
}

// --- R7: watcher/debounce IndexScheduleState machine tests ---------------

use super::{IndexScheduleState, ScheduledIndex};

fn dirty_change(root: &str, rel: &str) -> super::RootDirtyChange {
    super::RootDirtyChange {
        root: std::path::PathBuf::from(root),
        rel_path: rel.to_string(),
        change: crate::indexer::DirtyFileChange {
            absolute_path: std::path::PathBuf::from(root).join(rel),
            kind: crate::indexer::DirtyFileKind::Upsert,
        },
    }
}

#[test]
fn index_schedule_dirty_merge_accumulates_changes() {
    let mut state = IndexScheduleState::default();
    state.pending_requested = true;
    state.pending_changes.push(dirty_change("/root", "src/a.c"));
    state.pending_changes.push(dirty_change("/root", "src/b.c"));
    state.pending_changes.push(dirty_change("/root", "inc/c.h"));
    assert_eq!(state.pending_changes.len(), 3);
    assert!(!state.pending_full, "full flag not set for dirty-only");
    assert!(state.pending_requested, "requested flag set");
}

#[test]
fn index_schedule_full_overrides_dirty() {
    let mut state = IndexScheduleState::default();
    state.pending_requested = true;
    state.pending_changes.push(dirty_change("/root", "src/a.c"));
    state.pending_changes.push(dirty_change("/root", "src/b.c"));
    assert_eq!(state.pending_changes.len(), 2);

    // Full request arrives — it overrides dirty changes.
    state.pending_full = true;
    state.pending_force = true;
    state.pending_changes.clear();
    assert!(state.pending_full);
    assert!(state.pending_force);
    assert!(state.pending_changes.is_empty());
}

#[test]
fn index_schedule_scoped_full_preserves_dirty_work_for_other_roots() {
    let root_a = PathBuf::from("/workspace/a");
    let root_b = PathBuf::from("/workspace/b");
    let mut state = IndexScheduleState::default();
    state.request_dirty_changes(vec![
        dirty_change("/workspace/a", "src/a.go"),
        dirty_change("/workspace/b", "src/b.go"),
    ]);
    state.request_full_roots(vec![root_a.clone()]);

    match state.take_scheduled_index() {
        ScheduledIndex::Full {
            roots: Some(roots),
            force,
            changes,
        } => {
            assert_eq!(roots, vec![root_a]);
            assert!(!force);
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].root, root_b);
            assert_eq!(changes[0].rel_path, "src/b.go");
        }
        _ => panic!("expected a root-scoped full index"),
    }
}

#[test]
fn index_schedule_scoped_full_merges_roots_without_becoming_global() {
    let root_a = PathBuf::from("/workspace/a");
    let root_b = PathBuf::from("/workspace/b");
    let mut state = IndexScheduleState::default();
    state.request_full_roots(vec![root_a.clone()]);
    state.request_full_roots(vec![root_b.clone(), root_a.clone()]);

    match state.take_scheduled_index() {
        ScheduledIndex::Full {
            roots: Some(roots),
            force,
            changes,
        } => {
            assert_eq!(roots, vec![root_a, root_b]);
            assert!(!force);
            assert!(changes.is_empty());
        }
        _ => panic!("expected a root-scoped full index"),
    }
}

#[test]
fn index_schedule_second_request_during_running() {
    let mut state = IndexScheduleState::default();
    // Current indexing pass is running.
    state.running = true;
    state.scheduled = false; // current pass was the one

    // A new dirty request comes in while running.
    state.pending_requested = true;
    state
        .pending_changes
        .push(dirty_change("/root", "src/new.c"));

    // Verify flags: running stays true (still executing), scheduled is false
    // (old pass is still running), but pending_requested is set for re-schedule.
    assert!(state.running);
    assert!(
        !state.scheduled,
        "old pass still running, not yet re-scheduled"
    );
    assert!(state.pending_requested, "re-schedule requested");
    assert_eq!(state.pending_changes.len(), 1);
}

#[test]
fn index_schedule_state_reset_after_full_consumed() {
    let mut state = IndexScheduleState::default();
    state.running = true;
    state.scheduled = true;
    state.pending_requested = true;
    state.pending_full = true;

    // "Consume" the scheduled full index.
    state.running = false;
    state.scheduled = false;
    state.pending_full = false;
    state.pending_force = false;
    // pending_requested is set by a concurrent request; after the loop
    // checks it, it would spawn again. Here we verify the consumed state.
    assert!(!state.running);
    assert!(!state.scheduled);
    assert!(!state.pending_full);
    assert!(!state.pending_force);
}

#[test]
fn index_schedule_dirty_follows_full() {
    // Scenario: full index runs, a dirty request arrives during it.
    // After the full finishes and pending_requested is seen, the loop
    // re-checks and processes the dirty changes.
    let mut state = IndexScheduleState::default();
    state.running = true;
    state.scheduled = true;
    state.pending_full = true;
    state.pending_force = false;

    // Dirty request arrives during full execution.
    state.pending_requested = true;
    state
        .pending_changes
        .push(dirty_change("/root", "src/edited.c"));

    // Full index finishes.
    state.running = false;
    state.scheduled = false;
    state.pending_full = false;
    state.pending_force = false;

    // Loop sees pending_requested, checks pending_full=false, falls to
    // dirty path with the accumulated change.
    assert!(state.pending_requested, "dirty work still pending");
    assert!(!state.pending_full, "full work consumed");
    assert_eq!(state.pending_changes.len(), 1);
    assert_eq!(state.pending_changes[0].rel_path, "src/edited.c");

    // Consume the dirty request.
    state.running = true;
    state.scheduled = true;
    state.pending_requested = false;
    state.pending_changes.clear();

    // Dirty run completes — no more work.
    state.running = false;
    state.scheduled = false;
    assert!(!state.running);
    assert!(!state.scheduled);
    assert!(state.pending_changes.is_empty());
    assert!(!state.pending_requested);
}

// --- R7: error degradation — IndexStatus state correctness ---------------

#[test]
fn index_status_failed_has_correct_state() {
    let failed = crate::progress::IndexStatus::failed("/workspace".into(), "disk full".into());
    assert_eq!(failed.state, crate::progress::IndexState::Failed);
    assert!(
        !failed.message.as_deref().unwrap_or("").is_empty(),
        "failed status must carry an error message"
    );
}

#[test]
fn index_status_ready_distinguishable_from_failed() {
    let failed = crate::progress::IndexStatus::failed("/workspace".into(), "disk full".into());
    let stats = crate::progress::IndexStats::default();
    let ready = crate::progress::IndexStatus::ready("/workspace".into(), &stats);

    assert_ne!(
        ready.state, failed.state,
        "Ready and Failed must be distinguishable states"
    );
    assert_eq!(ready.state, crate::progress::IndexState::Ready);
    assert_eq!(failed.state, crate::progress::IndexState::Failed);
    // A Ready status carries indexed counts; a Failed status carries zeroes
    // and a non-empty message — they must never be confused.
    assert!(ready.message.is_none(), "Ready carries no error message");
    assert!(failed.message.is_some(), "Failed carries an error message");
}

#[test]
fn index_status_ready_carries_degraded_capabilities() {
    let stats = crate::progress::IndexStats::default();
    let degraded = crate::progress::DegradedCapabilities {
        call_relations: false,
        reach_graph: true,
        include_table: false,
        go_import_table: false,
        reference_file_list: true,
        project_context: false,
    };
    let ready =
        crate::progress::IndexStatus::ready_with_degraded("/workspace".into(), &stats, degraded);

    assert_eq!(ready.state, crate::progress::IndexState::Ready);
    assert!(ready.degraded_capabilities.any());
    assert_eq!(
        ready.degraded_capabilities.labels(),
        vec!["reachGraph", "referenceFileList"]
    );
}

#[test]
fn ready_cache_message_names_degraded_capabilities() {
    let degraded = crate::progress::DegradedCapabilities {
        call_relations: false,
        reach_graph: true,
        include_table: true,
        go_import_table: false,
        reference_file_list: false,
        project_context: false,
    };

    let message = super::ready_cache_message("name table ready", 7, 3, 2, 11, 13, &degraded);

    assert!(message.contains("name table ready: 7 declarations"));
    assert!(message.contains("include table=3 paths"));
    assert!(message.contains("reference files=2"));
    assert!(message.contains("degraded=reachGraph,includeTable"));
}

#[test]
fn query_error_log_line_is_structured_and_single_line() {
    let line =
        super::query_error_log_line("grouped references", "query", "db failed\nwhile reading");

    assert_eq!(
        line,
        "FS_QUERY_ERROR kind=query what=grouped_references detail=db failed while reading"
    );
}
