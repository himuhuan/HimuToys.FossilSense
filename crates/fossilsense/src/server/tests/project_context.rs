use super::*;
use tower_lsp::LanguageServer as _;

async fn refresh_project_contexts(service: &LspService<super::super::Backend>, roots: &[PathBuf]) {
    for root in roots {
        service
            .inner()
            .session
            .cache
            .refresh_project_context_index(&service.inner().client, root.clone())
            .await
            .expect("refresh project contexts");
    }
}

fn project_context_command(uri: Option<&Url>) -> ExecuteCommandParams {
    ExecuteCommandParams {
        command: super::PROJECT_CONTEXTS_LSP_COMMAND.to_string(),
        arguments: uri
            .map(|uri| vec![serde_json::json!({ "uri": uri.as_str() })])
            .unwrap_or_default(),
        work_done_progress_params: Default::default(),
    }
}

#[tokio::test]
async fn project_context_command_exposes_projects_and_active_file_context() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    write_workspace_file(dir.path(), "app/Makefile", "all:\n");
    write_workspace_file(dir.path(), "app/src/main.c", "int main(void);\n");
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(dir.path().to_path_buf());
    refresh_project_contexts(&service, &[dir.path().to_path_buf()]).await;

    let uri = Url::from_file_path(dir.path().join("app/src/main.c")).expect("uri");
    let value = service
        .inner()
        .execute_command(project_context_command(Some(&uri)))
        .await
        .expect("command")
        .expect("value");
    let status: crate::project_context::ProjectContextStatus =
        serde_json::from_value(value).expect("status");

    assert_eq!(status.projects.len(), 1);
    assert_eq!(status.projects[0].key.project_path, "app");
    assert_eq!(
        status.active_project.expect("active project").project_path,
        "app"
    );
}

#[tokio::test]
async fn stale_manual_project_selection_falls_back_to_auto() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    write_workspace_file(dir.path(), "app/Makefile", "all:\n");
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(dir.path().to_path_buf());
    refresh_project_contexts(&service, &[dir.path().to_path_buf()]).await;

    let stale_key = crate::project_context::ProjectContextKey {
        workspace_root_id: "missing-root".to_string(),
        project_path: "deleted".to_string(),
    };
    let value = service
        .inner()
        .execute_command(ExecuteCommandParams {
            command: super::SET_PROJECT_CONTEXT_LSP_COMMAND.to_string(),
            arguments: vec![serde_json::json!({
                "selection": { "kind": "manual", "key": stale_key }
            })],
            work_done_progress_params: Default::default(),
        })
        .await
        .expect("command")
        .expect("value");
    let status: crate::project_context::ProjectContextStatus =
        serde_json::from_value(value).expect("status");

    assert_eq!(
        status.selection,
        crate::project_context::ProjectContextSelection::Auto
    );
}

#[tokio::test]
async fn multi_root_project_keys_disambiguate_same_relative_path() {
    let service = test_backend_service();
    let first = tempdir().expect("first");
    let second = tempdir().expect("second");
    write_workspace_file(first.path(), "app/Makefile", "all:\n");
    write_workspace_file(second.path(), "app/Makefile", "all:\n");
    {
        let mut roots = service.inner().workspace_roots.lock().await;
        roots.push(first.path().to_path_buf());
        roots.push(second.path().to_path_buf());
    }
    refresh_project_contexts(
        &service,
        &[first.path().to_path_buf(), second.path().to_path_buf()],
    )
    .await;

    let value = service
        .inner()
        .execute_command(project_context_command(None))
        .await
        .expect("command")
        .expect("value");
    let status: crate::project_context::ProjectContextStatus =
        serde_json::from_value(value).expect("status");

    assert_eq!(status.projects.len(), 2);
    assert_eq!(status.projects[0].key.project_path, "app");
    assert_eq!(status.projects[1].key.project_path, "app");
    assert_ne!(
        status.projects[0].key.workspace_root_id,
        status.projects[1].key.workspace_root_id
    );
}

#[tokio::test]
async fn marker_refresh_updates_generation_and_clears_completion_memo() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root.clone());
    refresh_project_contexts(&service, std::slice::from_ref(&root)).await;
    let before = service
        .inner()
        .session
        .snapshot_for_root(root.clone())
        .await;
    let uri = Url::from_file_path(root.join("app/src/main.c")).expect("uri");
    service
        .inner()
        .session
        .cache
        .record_completion_memo(uri.clone(), "fo".to_string(), 7, vec![vec![0]])
        .await;

    write_workspace_file(dir.path(), "app/Makefile", "all:\n");
    service
        .inner()
        .refresh_project_context_roots(vec![root.clone()])
        .await;
    let after = service.inner().session.snapshot_for_root(root).await;

    assert_ne!(before.generation, after.generation);
    assert_eq!(
        after
            .project_context
            .as_ref()
            .expect("project contexts")
            .projects()
            .len(),
        1
    );
    assert!(service
        .inner()
        .session
        .cache
        .completion_memo_for_test(&uri)
        .await
        .is_none());
}
