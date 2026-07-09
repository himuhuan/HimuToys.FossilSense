use super::*;
use tower_lsp::LanguageServer as _;

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
