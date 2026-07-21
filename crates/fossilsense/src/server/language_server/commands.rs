use super::*;

impl Backend {
    pub(super) async fn execute_server_command(
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
        } else if params.command == CALL_RELATIONS_LSP_COMMAND {
            let Some(arg) = params.arguments.first() else {
                return Ok(None);
            };
            Ok(self.rich_relations_command(arg).await)
        } else if params.command == POSSIBLE_TARGETS_LSP_COMMAND {
            let Some(arg) = params.arguments.first() else {
                return Ok(None);
            };
            Ok(self.possible_targets_command(arg).await)
        } else if params.command == GROUPED_REFERENCES_LSP_COMMAND {
            let Some(arg) = params.arguments.first() else {
                return Ok(None);
            };
            let Some(uri) = arg
                .get("uri")
                .and_then(|v| v.as_str())
                .and_then(|s| Url::parse(s).ok())
            else {
                return Ok(None);
            };
            let line = arg.get("line").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let character = arg.get("character").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

            let Some(text) = self.document_text(&uri).await else {
                return Ok(None);
            };
            let line_text = text.lines().nth(line as usize).unwrap_or_default();
            let Some(word) = query::word_at(line_text, character) else {
                return Ok(None);
            };
            let Some(root) = self.root_for_uri(&uri).await else {
                return Ok(None);
            };
            let role_cache = self.session.cache.reference_role_cache.clone();
            let search_cache = self.session.cache.reference_search_cache.clone();
            let context = self.request_context_for_root(root.clone()).await;
            let indexed_generation = context.engine.epoch.as_u64();
            let indexed_files = context.engine.indexed_files.clone();
            let result = tokio::task::spawn_blocking(
                move || -> Result<(Vec<GroupedReferenceItem>, bool, references::ReferencesTiming)> {
                    let (mut hits, truncated, timing) =
                        references::search_references_with_shared_files(
                            &root,
                            &word,
                            &role_cache,
                            &search_cache,
                            indexed_generation,
                            indexed_files,
                        )?;
                    references::sort_hits_by_role(&mut hits);
                    Ok((grouped_reference_items(&root, &hits), truncated, timing))
                },
            )
            .await;
            match self.unwrap_query("grouped references", result).await {
                Some((items, truncated, timing)) => {
                    self.perf_log(|| format!(
                        "[perf] grouped_references total={}ms discover={}ms search={}ms classify={}ms occs={} cached={} truncated={}",
                        timing.total_ms,
                        timing.discover_ms,
                        timing.search_ms,
                        timing.classify_ms,
                        timing.total_occurrences,
                        timing.cached,
                        truncated,
                    ))
                    .await;
                    Ok(Some(serde_json::to_value(items).unwrap_or(Value::Null)))
                }
                None => Ok(None),
            }
        } else if params.command == PROJECT_CONTEXTS_LSP_COMMAND {
            let uri =
                project_context_commands::project_context_command_uri(params.arguments.first());
            let status = self.project_context_status(uri.as_ref()).await;
            Ok(serde_json::to_value(status).ok())
        } else if params.command == SET_PROJECT_CONTEXT_LSP_COMMAND {
            let uri =
                project_context_commands::project_context_command_uri(params.arguments.first());
            let selection =
                project_context_commands::project_context_selection_arg(params.arguments.first())
                    .unwrap_or(ProjectContextSelection::Auto);
            let status = self
                .set_project_context_selection(selection, uri.as_ref())
                .await;
            Ok(serde_json::to_value(status).ok())
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
}
