use super::*;

impl Backend {
    pub(super) async fn handle_completion(
        &self,
        params: CompletionParams,
    ) -> LspResult<Option<CompletionResponse>> {
        let request_settings = self.snapshot_settings();
        if !request_settings.completion_enabled {
            return Ok(None);
        }

        let ordinary_started = tokio::time::Instant::now();
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;

        let Some((version, text)) = self.document_snapshot(&uri).await else {
            return Ok(Some(empty_completion_list(true)));
        };

        let line_text = text.lines().nth(position.line as usize).unwrap_or_default();

        // Inside an `#include "..."` / `<...>`: offer header paths, not symbols.
        if let Some((form, partial)) =
            includes::include_completion_context(line_text, position.character)
        {
            return self.complete_include(&uri, form, partial, &text).await;
        }

        if query::is_member_completion_context(line_text, position.character) {
            return self
                .complete_members(&uri, version, &text, line_text, position)
                .await;
        }

        let prefix = query::completion_prefix_at(line_text, position.character).unwrap_or_default();
        if prefix.len() < query::MIN_PREFIX_LEN {
            return Ok(Some(empty_completion_list(true)));
        }
        let intent =
            crate::completion::classify_completion_intent(line_text, position.character, &prefix);
        let history_enabled = self.completion_history_mode.lock().await.is_enabled();
        let history_root = if history_enabled {
            self.root_for_uri(&uri).await
        } else {
            None
        };
        let history_workspace_hash = history_root
            .as_ref()
            .map(|root| completion_history_workspace_hash(root));
        let history_prefix_bucket = crate::completion_history::prefix_bucket(&prefix);
        let history_snapshot = match (
            history_enabled,
            history_root.as_deref(),
            history_workspace_hash.as_deref(),
        ) {
            (true, Some(root), Some(workspace_hash)) => self
                .completion_history_snapshot_for_root(root, workspace_hash)
                .await
                .unwrap_or_default(),
            _ => crate::completion_history::CompletionHistorySnapshot::default(),
        };

        let parsed_document = match uri_to_path(&uri) {
            Some(path) => {
                self.get_or_parse_document(&uri, &path, version, &text)
                    .await
            }
            None => None,
        };
        let local_words = self.local_words_for(&uri, version, &text).await;

        let snapshots = {
            let roots = self.workspace_roots.lock().await.clone();
            let mut snapshots = Vec::with_capacity(roots.len());
            for root in roots {
                snapshots.push(self.workspace_snapshot_for_root(root).await);
            }
            snapshots
        };
        let mut tables = Vec::new();
        let mut table_generations = Vec::new();
        for snapshot in &snapshots {
            if let Some(table) = snapshot.name_table.clone() {
                table_generations.push((snapshot.root.clone(), snapshot.generation));
                tables.push(OrdinaryCompletionNameTable { table });
            }
        }

        // Limited include-reachability scope: re-ranks candidates by their
        // `ScopeTier` (current / reachable / first-layer external / unknown /
        // global) via the shared resolver. None => whole-index ranking (scoping
        // off, no graph yet, or unresolvable path).
        let current_snapshot = self
            .root_for_uri(&uri)
            .await
            .and_then(|root| snapshots.iter().find(|snapshot| snapshot.root == root));
        let scope = current_snapshot
            .and_then(|snapshot| self.reach_scope_from_snapshot(&uri, snapshot))
            .map(|(rel, reach)| query::CompletionScope {
                current_path: Some(rel),
                reach: (*reach).clone(),
            });
        let active_project_context = match self.project_context_selection.lock().await.clone() {
            ProjectContextSelection::Auto => current_snapshot.and_then(|snapshot| {
                let path = uri_to_path(&uri)?;
                let rel = pathing::relative_slash_path(&snapshot.root, &path).ok()?;
                snapshot
                    .project_context
                    .as_ref()
                    .and_then(|index| index.nearest_for_file(&rel))
            }),
            ProjectContextSelection::Manual { key } => Some(key),
            ProjectContextSelection::Unspecified => None,
        };

        let limit = query::COMPLETION_LIMIT;
        let locality_bonus = query::COMPLETION_LOCALITY_BONUS;

        // Per-document narrowing: reuse the previous prefix's candidate pool when
        // the new prefix extends it and the same name-table generation is in
        // play. A shortened/changed prefix or a rebuilt table generation resets
        // to a full scan.
        let completion_generation = state::combine_workspace_generations(&table_generations);
        let completion_started = tokio::time::Instant::now();
        let memo_lookup = self
            .session
            .cache
            .completion_memo_pools(&uri, completion_generation, &prefix, tables.len())
            .await;
        let prior_pools = memo_lookup.prior_pools;
        let hit_kind = memo_lookup.hit_kind;
        let memo_prefix = prefix.clone();
        let context_ms = ordinary_started.elapsed().as_millis();
        let table_count = tables.len();

        let service_input = OrdinaryCompletionInput {
            prefix: prefix.clone(),
            text,
            line: position.line,
            character: position.character,
            parsed_document,
            local_words,
            tables,
            scope,
            active_project_context,
            prior_pools,
            intent,
            history_enabled,
            history: history_snapshot,
            prefix_bucket: history_prefix_bucket.clone(),
            limit,
            locality_bonus,
        };

        let result = tokio::task::spawn_blocking(move || -> Result<_> {
            Ok(crate::completion::ordinary_service::complete_ordinary_identifier(service_input))
        })
        .await;

        // The list is always incomplete: results are truncated to
        // `COMPLETION_LIMIT` and the recall threshold widens with prefix
        // length, so the editor must re-query with the full current prefix on
        // every keystroke. This lets longer-named symbols re-enter the
        // truncation window as the user keeps typing, and prevents an empty
        // first batch from sticking as a "complete" no-match list.
        match self.unwrap_query("completion", result).await {
            Some(output) => {
                let render_started = std::time::Instant::now();
                let mut items: Vec<CompletionItem> = output
                    .items
                    .into_iter()
                    .map(|ordinary_item| {
                        let evidence = ordinary_item.evidence;
                        let mut item = ordinary_completion_item_to_lsp(ordinary_item);
                        if history_enabled {
                            if let Some(workspace_hash) = history_workspace_hash.as_deref() {
                                attach_completion_history_accept_command(
                                    &mut item,
                                    evidence,
                                    workspace_hash,
                                    intent.kind,
                                    &history_prefix_bucket,
                                );
                            }
                        }
                        item
                    })
                    .collect();
                apply_final_completion_sort_text(&mut items);
                let render_ms = render_started.elapsed().as_millis();
                let timings = crate::completion::CompletionStageTimings {
                    total_ms: completion_started.elapsed().as_millis(),
                    context_ms,
                    recall_ms: output.recall_ms,
                    merge_rank_ms: output.merge_rank_ms,
                    render_ms,
                };
                let metrics = output.metrics;
                self.perf_log(|| {
                    crate::completion::completion_perf_summary(
                        &memo_prefix,
                        hit_kind,
                        &timings,
                        &metrics,
                    )
                })
                .await;
                // Record this prefix's pools for the next (extending) keystroke.
                if output.new_pools.len() == table_count
                    && output.new_pools.iter().any(|pool| !pool.is_empty())
                {
                    self.session
                        .cache
                        .record_completion_memo(
                            uri,
                            memo_prefix,
                            completion_generation,
                            output.new_pools,
                        )
                        .await;
                } else {
                    self.session.cache.clear_completion_memo(&uri).await;
                }
                if items.is_empty() {
                    Ok(Some(empty_completion_list(true)))
                } else {
                    Ok(Some(CompletionResponse::List(CompletionList {
                        is_incomplete: true,
                        items,
                    })))
                }
            }
            _ => Ok(Some(empty_completion_list(true))),
        }
    }
}
