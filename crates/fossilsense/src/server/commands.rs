use super::*;

impl Backend {
    pub(super) async fn handle_execute_command(
        &self,
        params: ExecuteCommandParams,
    ) -> LspResult<Option<Value>> {
        if params.command == REFRESH_INDEX_LSP_COMMAND || params.command == REFRESH_INDEX_COMMAND {
            self.client
                .log_message(MessageType::INFO, "refreshing index (incremental)")
                .await;
            self.spawn_index_roots(Some(false)).await;
            Ok(None)
        } else if params.command == REBUILD_INDEX_LSP_COMMAND
            || params.command == REBUILD_INDEX_COMMAND
        {
            self.client
                .log_message(MessageType::INFO, "rebuilding index (force)")
                .await;
            self.spawn_index_roots(Some(true)).await;
            Ok(None)
        } else if params.command == GROUPED_REFERENCES_LSP_COMMAND {
            self.handle_grouped_references_command(params).await
        } else if params.command == PROJECT_CONTEXTS_LSP_COMMAND {
            self.handle_project_contexts_command(params).await
        } else if params.command == SET_PROJECT_CONTEXT_LSP_COMMAND {
            self.handle_set_project_context_command(params).await
        } else if params.command == COMPLETION_ACCEPTED_LSP_COMMAND {
            if let Some(event) = completion_accept_event_from_arg(params.arguments.first()) {
                if self.record_completion_accept(event).await.is_err() {
                    self.client
                        .log_message(
                            MessageType::ERROR,
                            "FossilSense completion history record failed",
                        )
                        .await;
                }
            }
            Ok(None)
        } else if params.command == CLEAR_COMPLETION_HISTORY_LSP_COMMAND {
            match self.clear_completion_history().await {
                Ok(removed) => {
                    self.client
                        .log_message(
                            MessageType::INFO,
                            format!("FossilSense completion history cleared entries={removed}"),
                        )
                        .await;
                }
                Err(_) => {
                    self.client
                        .log_message(
                            MessageType::ERROR,
                            "FossilSense completion history clear failed",
                        )
                        .await;
                }
            }
            Ok(None)
        } else {
            Ok(None)
        }
    }

    async fn handle_project_contexts_command(
        &self,
        params: ExecuteCommandParams,
    ) -> LspResult<Option<Value>> {
        let uri = command_uri(params.arguments.first());
        let status = self.project_context_status(uri.as_ref()).await;
        Ok(serde_json::to_value(status).ok())
    }

    async fn handle_set_project_context_command(
        &self,
        params: ExecuteCommandParams,
    ) -> LspResult<Option<Value>> {
        let selection = project_context_selection_arg(params.arguments.first())
            .unwrap_or(ProjectContextSelection::Auto);
        let selection = self.validated_project_context_selection(selection).await;
        *self.project_context_selection.lock().await = selection;
        self.session.cache.clear_all_completion_memos().await;
        let uri = command_uri(params.arguments.first());
        let status = self.project_context_status(uri.as_ref()).await;
        Ok(serde_json::to_value(status).ok())
    }

    async fn validated_project_context_selection(
        &self,
        selection: ProjectContextSelection,
    ) -> ProjectContextSelection {
        let ProjectContextSelection::Manual { key } = selection else {
            return selection;
        };
        let roots = self.workspace_roots.lock().await.clone();
        for root in roots {
            let snapshot = self.workspace_snapshot_for_root(root).await;
            if snapshot
                .project_context
                .as_ref()
                .is_some_and(|index| index.contains_key(&key))
            {
                return ProjectContextSelection::Manual { key };
            }
        }
        ProjectContextSelection::Auto
    }

    async fn project_context_status(&self, uri: Option<&Url>) -> ProjectContextStatus {
        let roots = self.workspace_roots.lock().await.clone();
        let mut projects = Vec::new();
        let mut automatic_project = None;
        for root in roots {
            let snapshot = self.workspace_snapshot_for_root(root).await;
            if let Some(index) = snapshot.project_context.as_ref() {
                projects.extend(index.projects().iter().cloned());
                if automatic_project.is_none() {
                    if let Some(uri) = uri {
                        if let Some(path) = uri_to_path(uri) {
                            if path.starts_with(&snapshot.root) {
                                if let Ok(rel) = pathing::relative_slash_path(&snapshot.root, &path)
                                {
                                    automatic_project = index.nearest_for_file(&rel);
                                }
                            }
                        }
                    }
                }
            }
        }
        projects.sort_by(|a, b| a.key.cmp(&b.key));
        let selection = self.project_context_selection.lock().await.clone();
        let active_project = match &selection {
            ProjectContextSelection::Auto => automatic_project.clone(),
            ProjectContextSelection::Manual { key } => Some(key.clone()),
            ProjectContextSelection::Unspecified => None,
        };
        ProjectContextStatus {
            projects,
            selection,
            automatic_project,
            active_project,
        }
    }
}

fn command_uri(arg: Option<&Value>) -> Option<Url> {
    arg.and_then(|value| {
        value
            .get("uri")
            .or(Some(value))
            .and_then(|value| value.as_str())
            .and_then(|raw| Url::parse(raw).ok())
    })
}

fn project_context_selection_arg(arg: Option<&Value>) -> Option<ProjectContextSelection> {
    let value = arg?;
    let value = value.get("selection").unwrap_or(value);
    serde_json::from_value(value.clone()).ok()
}
