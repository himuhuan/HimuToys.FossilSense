use super::*;

#[async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> LspResult<InitializeResult> {
        let roots = workspace_roots_from_initialize(&params);
        {
            let mut stored = self.workspace_roots.lock().await;
            *stored = roots;
        }

        {
            let mut stored = self.include_paths.lock().await;
            *stored = parse_include_paths(&params);
        }

        let completion_mode = parse_completion_mode(&params);
        self.completion_enabled
            .store(completion_mode.is_enabled(), Ordering::Relaxed);
        self.strict_prefix_ranking.store(
            parse_completion_prefix_ranking(&params) == completion::CompletionPrefixRanking::Strict,
            Ordering::Relaxed,
        );
        *self.completion_history_mode.lock().await = parse_completion_history_mode(&params);
        *self.project_context_selection.lock().await =
            parse_initial_project_context_selection(&params);

        let completion_provider = if self.completion_enabled.load(Ordering::Relaxed) {
            Some(CompletionOptions {
                trigger_characters: Some(completion_trigger_characters()),
                resolve_provider: Some(true),
                ..Default::default()
            })
        } else {
            None
        };

        let semantic_mode = parse_semantic_coloring_mode(&params);
        self.semantic_coloring_enabled
            .store(semantic_mode.is_enabled(), Ordering::Relaxed);

        self.scoping_enabled
            .store(parse_include_scoping_enabled(&params), Ordering::Relaxed);

        self.debug_candidate_reasons
            .store(parse_debug_candidate_reasons(&params), Ordering::Relaxed);
        self.perf_logging_enabled
            .store(parse_debug_perf_logs(&params), Ordering::Relaxed);

        let semantic_tokens_provider = if self.semantic_coloring_enabled.load(Ordering::Relaxed) {
            Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
                SemanticTokensOptions {
                    // Legend order defines the token-type indices used when
                    // encoding (macro = 0, type = 1, enumMember = 2,
                    // parameter = 3, variable = 4); no modifiers are declared.
                    legend: SemanticTokensLegend {
                        token_types: vec![
                            SemanticTokenType::MACRO,
                            SemanticTokenType::TYPE,
                            SemanticTokenType::ENUM_MEMBER,
                            SemanticTokenType::PARAMETER,
                            SemanticTokenType::VARIABLE,
                        ],
                        token_modifiers: vec![],
                    },
                    range: Some(true),
                    full: Some(SemanticTokensFullOptions::Bool(true)),
                    ..Default::default()
                },
            ))
        } else {
            None
        };

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(TextDocumentSyncKind::INCREMENTAL),
                        save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                            include_text: Some(false),
                        })),
                        ..TextDocumentSyncOptions::default()
                    },
                )),
                workspace: Some(WorkspaceServerCapabilities {
                    workspace_folders: Some(WorkspaceFoldersServerCapabilities {
                        supported: Some(true),
                        change_notifications: Some(OneOf::Left(true)),
                    }),
                    file_operations: None,
                }),
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                completion_provider,
                signature_help_provider: Some(signature_help_options()),
                semantic_tokens_provider,
                call_hierarchy_provider: Some(CallHierarchyServerCapability::Simple(true)),
                execute_command_provider: Some(tower_lsp::lsp_types::ExecuteCommandOptions {
                    commands: vec![
                        REFRESH_INDEX_LSP_COMMAND.to_string(),
                        REBUILD_INDEX_LSP_COMMAND.to_string(),
                        GROUPED_REFERENCES_LSP_COMMAND.to_string(),
                        COMPLETION_ACCEPTED_LSP_COMMAND.to_string(),
                        CLEAR_COMPLETION_HISTORY_LSP_COMMAND.to_string(),
                        PROJECT_CONTEXTS_LSP_COMMAND.to_string(),
                        SET_PROJECT_CONTEXT_LSP_COMMAND.to_string(),
                        CALL_RELATIONS_LSP_COMMAND.to_string(),
                    ],
                    ..Default::default()
                }),
                ..ServerCapabilities::default()
            },
            server_info: Some(ServerInfo {
                name: "FossilSense".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "FossilSense initialized")
            .await;
        self.preload_completion_history().await;
        self.spawn_index_roots(None).await;
    }

    async fn shutdown(&self) -> LspResult<()> {
        self.client
            .log_message(MessageType::INFO, "FossilSense shutting down")
            .await;
        Ok(())
    }

    async fn prepare_call_hierarchy(
        &self,
        params: CallHierarchyPrepareParams,
    ) -> LspResult<Option<Vec<CallHierarchyItem>>> {
        let position = params.text_document_position_params;
        Ok(self
            .prepare_call_items(&position.text_document.uri, position.position)
            .await)
    }

    async fn incoming_calls(
        &self,
        params: CallHierarchyIncomingCallsParams,
    ) -> LspResult<Option<Vec<CallHierarchyIncomingCall>>> {
        Ok(self.standard_incoming(&params.item).await)
    }

    async fn outgoing_calls(
        &self,
        params: CallHierarchyOutgoingCallsParams,
    ) -> LspResult<Option<Vec<CallHierarchyOutgoingCall>>> {
        Ok(self.standard_outgoing(&params.item).await)
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        self.session
            .open_document(
                uri.clone(),
                params.text_document.version,
                params.text_document.text,
            )
            .await;
        if let Some(root) = self.root_for_uri(&uri).await {
            let generation = self
                .request_context_for_root(root.clone())
                .await
                .engine
                .semantic_generation;
            let rel_paths = uri
                .to_file_path()
                .ok()
                .and_then(|path| pathing::relative_slash_path(&root, &path).ok())
                .map(|path| vec![path]);
            if let Some(rel_paths) = rel_paths {
                self.session
                    .documents
                    .reconcile_published_files(root, Some(rel_paths), generation)
                    .await;
            }
        }
        self.client
            .log_message(MessageType::LOG, "document opened")
            .await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        if !self
            .session
            .apply_document_changes(&uri, params.text_document.version, params.content_changes)
            .await
        {
            self.client
                .log_message(
                    MessageType::WARNING,
                    "ignored invalid incremental document change",
                )
                .await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.session.close_document(&params.text_document.uri).await;
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> LspResult<Option<GotoDefinitionResponse>> {
        let position = params.text_document_position_params;
        let uri = position.text_document.uri;

        let documents = self
            .session
            .documents
            .capture_request_snapshot(Some(&uri))
            .await;
        let Some((_version, text)) = self.document_snapshot_from_request(&uri, &documents).await
        else {
            return Ok(None);
        };
        let line_text = text
            .lines()
            .nth(position.position.line as usize)
            .unwrap_or_default();

        // An `#include` line resolves to the included header rather than a symbol.
        if let Some((form, rel)) = includes::parse_include_line(line_text) {
            return self.goto_include(&uri, form, rel).await;
        }

        let Some(word) = query::word_at(line_text, position.position.character) else {
            return Ok(None);
        };
        if crate::language_builtins::is_language_keyword(&word) {
            return Ok(None);
        }

        let Some(root) = self.root_for_uri(&uri).await else {
            return Ok(None);
        };
        let current_rel = uri_to_path(&uri)
            .and_then(|path| pathing::relative_slash_path(&root, &path).ok())
            .unwrap_or_default();
        // Reachability scope for candidate tier resolution (Current / Reachable
        // / External / Unknown / Global). A file in the set is proved reachable
        // regardless of whether the set is open; an open scope routes
        // not-proven-reachable workspace candidates to `Unknown` (preserving
        // the R1 "open scope does not bury unreachable" softening as a tier).
        // `None` when scoping is disabled or no graph exists yet — non-current
        // workspace files then fall back to `Global`.
        let total_started = std::time::Instant::now();
        let context = self.request_context_for_root(root.clone()).await;
        let reach_started = std::time::Instant::now();
        let reach_scope: Option<Arc<reachability::ReachScope>> = self
            .reach_scope_from_context(&uri, &context)
            .map(|(_, reach)| reach);
        let mut reach_us = reach_started.elapsed().as_micros();
        let project_context = context.engine.project_context.clone();
        let semantic_generation = context.engine.semantic_generation;
        let call_read_handle = context.engine.call_read_handle.clone();
        let reach_graph = context.engine.reach_graph.clone();
        let overlay_started = std::time::Instant::now();
        let overlay = self
            .candidate_overlay_snapshot_from_documents(
                &root,
                semantic_generation,
                reach_graph.as_deref(),
                context.engine.indexed_files.as_deref().map(Vec::as_slice),
                documents,
            )
            .await;
        reach_us = reach_us.saturating_add(overlay_started.elapsed().as_micros());
        let source_position = crate::call_model::SourcePosition {
            line: position.position.line,
            character: position.position.character,
        };

        // Debug-gated candidate-reason logging (default off): when on, each
        // returned candidate's tier/confidence/reason is logged to the output
        // panel. The flag only adds log lines; it never changes which locations
        // are returned or their order.
        let debug_reasons = self.debug_candidate_reasons.load(Ordering::Relaxed);
        let client = self.client.clone();
        let word_for_log = word.clone();

        let result = tokio::task::spawn_blocking(
            move || -> Result<(Vec<Location>, Vec<String>, SemanticRequestPerf)> {
                let query_started = std::time::Instant::now();
                let service = crate::candidate_service::CandidateQueryService::new(
                    call_read_handle.as_deref(),
                    &overlay,
                    &current_rel,
                    reach_scope.as_deref(),
                    reach_graph.as_deref(),
                );
                let call_context = service.complete_call_context_at(source_position)?;
                let callable_set = service.callable_candidates(&word, call_context.clone())?;
                let mut perf = SemanticRequestPerf::from_callable_set(&callable_set);
                perf.reach_us = reach_us;
                if !callable_set.anchors.is_empty() {
                    // A callee token inside a function body is an ordinary call
                    // even when its name matches the enclosing/recursive
                    // function. Anchor opposite-only navigation applies solely
                    // to the declaration/definition identifier itself.
                    let origin = if call_context.is_some() {
                        None
                    } else {
                        service.anchor_at(source_position)?
                    };
                    let selected: Vec<&query::ResolvedCallableAnchor> = origin
                        .as_ref()
                        .and_then(|origin| {
                            query::anchor_opposite_definition(
                                &callable_set.groups,
                                &origin.anchor_fingerprint,
                            )
                        })
                        .map(|opposite| vec![opposite])
                        .unwrap_or_else(|| {
                            // Without a proven strict opposite, Definition
                            // uses the ordinary ranked candidate set. Keeping
                            // the origin is intentional: removing it can turn
                            // an unpaired declaration into no result, or leave
                            // only a semantically weaker same-name function.
                            query::call_definition_presentations(&callable_set.groups)
                        });
                    let candidates: Vec<_> = selected
                        .iter()
                        .map(|candidate| candidate.candidate.clone())
                        .collect();
                    perf.query_us = query_started.elapsed().as_micros();
                    let mut debug_lines = candidate_reason_log_lines(&candidates, debug_reasons);
                    if debug_reasons && callable_set.arity_mismatch_fallback {
                        debug_lines.insert(
                            0,
                            "arity_mismatch_fallback: no candidate matched the available argument-count evidence; retained candidates use fallback confidence"
                                .to_string(),
                        );
                    }
                    let locations: Vec<Location> = candidates
                        .iter()
                        .filter_map(|candidate| candidate_to_location(&root, candidate))
                        .collect();
                    perf.returned = locations.len();
                    return Ok((locations, debug_lines, perf));
                }

                // Non-callable symbols retain the existing navigation policy,
                // but their durable/live recall still comes through the same
                // generation-pinned overlay boundary.
                let records = service.non_callable_symbols(&word)?;
                let non_callable_count = records.len();
                let origin_record = records
                    .iter()
                    .find(|record| {
                        record.path == current_rel
                            && (record.start_line, record.start_col)
                                <= (source_position.line, source_position.character)
                            && (source_position.line, source_position.character)
                                <= (record.end_line, record.end_col)
                    })
                    .cloned();
                let candidates = query::rank_navigation_candidates_with_scope(
                    records,
                    &current_rel,
                    service.effective_current_reach(),
                    origin_record.as_ref(),
                    project_context.as_deref(),
                );
                perf.include_non_callable_candidates(non_callable_count);
                perf.query_us = query_started.elapsed().as_micros();
                let debug_lines = candidate_reason_log_lines(&candidates, debug_reasons);
                let locations: Vec<Location> = candidates
                    .iter()
                    .filter_map(|candidate| candidate_to_location(&root, candidate))
                    .collect();
                perf.returned = locations.len();
                Ok((locations, debug_lines, perf))
            },
        )
        .await;

        let metrics = result
            .as_ref()
            .ok()
            .and_then(|result| result.as_ref().ok().map(|(_, _, metrics)| *metrics))
            .unwrap_or_default();
        self.perf_log(|| metrics.log_line("definition", total_started.elapsed().as_micros()))
            .await;

        match self.unwrap_query("definition", result).await {
            Some((locations, debug_lines, _)) if !locations.is_empty() => {
                if !debug_lines.is_empty() {
                    client
                        .log_message(
                            MessageType::INFO,
                            format!(
                                "FossilSense goto '{}': {} candidate(s) (tier/confidence/reason):",
                                word_for_log,
                                debug_lines.len()
                            ),
                        )
                        .await;
                    for line in debug_lines {
                        client.log_message(MessageType::INFO, line).await;
                    }
                }
                Ok(Some(GotoDefinitionResponse::Array(locations)))
            }
            _ => Ok(None),
        }
    }

    async fn references(&self, params: ReferenceParams) -> LspResult<Option<Vec<Location>>> {
        let position = params.text_document_position;
        let uri = position.text_document.uri;

        let Some(text) = self.document_text(&uri).await else {
            return Ok(None);
        };
        let line_text = text
            .lines()
            .nth(position.position.line as usize)
            .unwrap_or_default();
        let Some(word) = query::word_at(line_text, position.position.character) else {
            return Ok(None);
        };

        let Some(root) = self.root_for_uri(&uri).await else {
            return Ok(None);
        };

        let client = self.client.clone();
        let search_word = word.clone();
        let role_cache = self.session.cache.reference_role_cache.clone();
        let search_cache = self.session.cache.reference_search_cache.clone();
        let context = self.request_context_for_root(root.clone()).await;
        let indexed_generation = context.engine.epoch.as_u64();
        let indexed_files = context.engine.indexed_files.clone();
        let result = tokio::task::spawn_blocking(
            move || -> Result<(Vec<Location>, bool, references::ReferencesTiming)> {
                let (mut hits, truncated, timing) =
                    references::search_references_with_shared_files(
                        &root,
                        &search_word,
                        &role_cache,
                        &search_cache,
                        indexed_generation,
                        indexed_files,
                    )?;
                // Group by role for the editor: definition/declaration first, then
                // call, write, type-use, and plain reads last; ties keep path/line
                // order so each file's hits stay contiguous. This reuses the
                // candidate-model vocabulary (role grouping is the reference-side
                // counterpart to `ResolutionConfidence`/`ResolutionReason`); a text
                // hit does not carry a `ScopeTier` and is not re-ranked by the
                // shared resolver. The grouped-references command uses the same sort.
                references::sort_hits_by_role(&mut hits);
                let locations: Vec<Location> = hits
                    .iter()
                    .filter_map(|hit| hit_to_location(&root, hit))
                    .collect();
                Ok((locations, truncated, timing))
            },
        )
        .await;

        match self.unwrap_query("references", result).await {
            Some((locations, truncated, timing)) => {
                self.perf_log(|| format!(
                    "[perf] references total={}ms discover={}ms search={}ms classify={}ms occs={} cached={} truncated={}",
                    timing.total_ms,
                    timing.discover_ms,
                    timing.search_ms,
                    timing.classify_ms,
                    timing.total_occurrences,
                    timing.cached,
                    truncated,
                ))
                .await;
                if truncated {
                    client
                        .log_message(
                            MessageType::INFO,
                            format!(
                                "FossilSense references for '{word}' returned more than {} results; output truncated",
                                references::REFERENCES_LIMIT
                            ),
                        )
                        .await;
                }
                Ok(Some(locations))
            }
            _ => Ok(None),
        }
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> LspResult<Option<Vec<SymbolInformation>>> {
        let tables: Vec<(PathBuf, u64, Arc<NameTable>)> = {
            let roots = self.workspace_roots.lock().await.clone();
            let mut tables = Vec::new();
            for root in roots {
                let context = self.request_context_for_root(root).await;
                if let Some(table) = context.engine.name_table.clone() {
                    tables.push((
                        context.engine.root.clone(),
                        context.engine.semantic_generation.0,
                        table,
                    ));
                }
            }
            tables
        };
        if tables.is_empty() {
            return Ok(None);
        }

        let query_text = params.query;
        let result = tokio::task::spawn_blocking(move || -> Result<Vec<SymbolInformation>> {
            let mut hits = Vec::new();
            for (root_index, (_, _, table)) in tables.iter().enumerate() {
                for hit in table.search_ranked(&query_text, query::WORKSPACE_SYMBOL_LIMIT) {
                    hits.push((root_index, hit));
                }
            }
            hits.sort_by(|a, b| {
                b.1.score
                    .cmp(&a.1.score)
                    .then(a.1.name_len.cmp(&b.1.name_len))
                    .then_with(|| a.1.name.cmp(&b.1.name))
                    .then(a.0.cmp(&b.0))
            });
            hits.truncate(query::WORKSPACE_SYMBOL_LIMIT);

            if hits.is_empty() {
                return Ok(Vec::new());
            }

            let mut records_by_root_and_id = HashMap::new();
            let mut ids_by_root: HashMap<usize, Vec<i64>> = HashMap::new();
            for (root_index, hit) in &hits {
                ids_by_root.entry(*root_index).or_default().push(hit.id);
            }

            for (root_index, ids) in ids_by_root {
                let (root, generation, _) = &tables[root_index];
                let db_path = pathing::default_index_path(root)?;
                if !db_path.exists() {
                    continue;
                }
                let records = IndexStore::read_at_generation(&db_path, *generation, |store| {
                    store.symbol_read_view().symbols_by_ids(&ids)
                })?;
                for record in records {
                    records_by_root_and_id.insert((root_index, record.id), record);
                }
            }

            Ok(hits
                .into_iter()
                .filter_map(|(root_index, hit)| {
                    let root = &tables[root_index].0;
                    records_by_root_and_id
                        .get(&(root_index, hit.id))
                        .and_then(|record| record_to_symbol_information(root, record))
                })
                .collect())
        })
        .await;

        Ok(self.unwrap_query("workspace/symbol", result).await)
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> LspResult<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri;
        let Some((version, text)) = self.document_snapshot(&uri).await else {
            return Ok(None);
        };
        let Some(path) = uri_to_path(&uri) else {
            return Ok(None);
        };

        let started = tokio::time::Instant::now();
        // Live parse served from the in-memory cache (one parse per document
        // version, shared across semantic tokens, completion, and symbols).
        let index = self
            .get_or_parse_document(
                &uri,
                &path,
                version,
                &text,
                parser::ParseFacts::SYMBOLS | parser::ParseFacts::INCLUDES,
            )
            .await;
        let Some(index) = index else {
            return Ok(None);
        };
        // Extract persistent symbols synchronously from the cached index.
        let document_symbols: Vec<DocumentSymbol> = index
            .persistent_facts()
            .symbols
            .iter()
            .map(parsed_to_document_symbol)
            .collect();
        self.perf_log(|| {
            format!(
                "[perf] document_symbol total={}ms count={}",
                started.elapsed().as_millis(),
                document_symbols.len(),
            )
        })
        .await;
        Ok(Some(DocumentSymbolResponse::Nested(document_symbols)))
    }

    async fn completion(&self, params: CompletionParams) -> LspResult<Option<CompletionResponse>> {
        let request_settings = self.request_settings();
        if !request_settings.completion_enabled {
            return Ok(None);
        }

        let ordinary_started = tokio::time::Instant::now();
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;

        // Current-buffer text and every divergent open-document overlay must
        // come from one lock-consistent capture. Member completion consumes
        // the whole capture below; ordinary completion only needs `current`.
        let document_request = self
            .session
            .documents
            .capture_request_snapshot(Some(&uri))
            .await;
        let Some((version, text)) = self
            .document_snapshot_from_request(&uri, &document_request)
            .await
        else {
            return Ok(Some(empty_completion_list(true)));
        };
        let completion_overlay_epoch = document_request.overlay_epoch;

        let line_text = text.lines().nth(position.line as usize).unwrap_or_default();

        // Inside an `#include "..."` / `<...>`: offer header paths, not symbols.
        if let Some((form, partial)) =
            includes::include_completion_context(line_text, position.character)
        {
            return self.complete_include(&uri, form, partial, &text).await;
        }

        if query::is_member_completion_context(line_text, position.character) {
            return self
                .complete_members(&uri, version, &text, line_text, position, document_request)
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
                self.get_or_parse_document(
                    &uri,
                    &path,
                    version,
                    &text,
                    parser::ParseFacts::COMPLETION,
                )
                .await
            }
            None => None,
        };
        let local_words = self.local_words_for(&uri, version, &text).await;

        let contexts = {
            let roots = self.workspace_roots.lock().await.clone();
            let mut contexts = Vec::with_capacity(roots.len());
            for root in roots {
                contexts.push(self.request_context_for_root(root).await);
            }
            contexts
        };
        let mut tables = Vec::new();
        let mut table_roots = Vec::new();
        let mut table_semantic_generations = Vec::new();
        let mut table_generations = Vec::new();
        let mut overlay_names_by_table = Vec::new();
        let current_root = self.root_for_uri(&uri).await;
        let mut effective_completion_scope = None;
        for context in &contexts {
            if let Some(table) = context.engine.name_table.clone() {
                let overlay = self
                    .candidate_overlay_snapshot_from_documents(
                        &context.engine.root,
                        context.engine.semantic_generation,
                        context.engine.reach_graph.as_deref(),
                        context.engine.indexed_files.as_deref().map(Vec::as_slice),
                        document_request.clone(),
                    )
                    .await;
                if current_root.as_deref() == Some(context.engine.root.as_path())
                    && context.settings.scoping_enabled
                {
                    effective_completion_scope = uri_to_path(&uri)
                        .and_then(|path| {
                            pathing::relative_slash_path(&context.engine.root, &path).ok()
                        })
                        .and_then(|rel| {
                            overlay
                                .effective_reach_graph(context.engine.reach_graph.as_deref())
                                .map(|graph| query::CompletionScope {
                                    reach: graph.reachable(&rel).as_ref().clone(),
                                    current_path: Some(rel),
                                })
                        });
                }
                let overlay_names = overlay.completion_names();
                let rows = overlay_names
                    .iter()
                    .map(|entry| {
                        (
                            entry.id,
                            entry.name.clone(),
                            entry.external,
                            entry.path.clone(),
                            entry.kind.clone(),
                            entry.directly_included,
                        )
                    })
                    .collect();
                let effective_table = table
                    .with_updated_paths(overlay.shadowed_paths(), rows)
                    .with_direct_include_overrides(overlay.direct_include_overrides());
                table_generations.push((context.engine.root.clone(), context.engine.epoch));
                table_roots.push(context.engine.root.clone());
                table_semantic_generations.push(context.engine.semantic_generation);
                overlay_names_by_table.push(
                    overlay_names
                        .into_iter()
                        .map(|entry| (entry.id, entry))
                        .collect(),
                );
                tables.push(OrdinaryCompletionNameTable {
                    table: Arc::new(effective_table),
                });
            }
        }
        let (active_project_context, project_selection_epoch) =
            self.effective_project_for_uri(&uri, &contexts).await;

        // Limited include-reachability scope: re-ranks candidates by their
        // `ScopeTier` (current / reachable / first-layer external / unknown /
        // global) via the shared resolver. None => whole-index ranking (scoping
        // off, no graph yet, or unresolvable path).
        let scope = effective_completion_scope;

        let limit = query::COMPLETION_LIMIT;
        let locality_bonus = query::COMPLETION_LOCALITY_BONUS;

        // Per-document narrowing: reuse the previous prefix's candidate pool when
        // the new prefix extends it and the same name-table generation is in
        // play. A shortened/changed prefix or a rebuilt table generation resets
        // to a full scan.
        let completion_generation = state::combine_completion_generation(
            &table_generations,
            project_selection_epoch,
            active_project_context.as_ref(),
            completion_overlay_epoch,
        );
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
            prefix_ranking: request_settings.prefix_ranking,
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
                        let mut item = ordinary_completion_item_to_lsp(
                            ordinary_item,
                            &uri,
                            &table_roots,
                            &table_semantic_generations,
                            &overlay_names_by_table,
                            completion_overlay_epoch,
                            version,
                        );
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
                        version,
                        completion_generation,
                        &timings,
                        &metrics,
                    )
                })
                .await;
                // Record this prefix's pools for the next (extending) keystroke.
                self.session
                    .cache
                    .record_completion_memo(
                        uri,
                        memo_prefix,
                        completion_generation,
                        output.new_pools,
                    )
                    .await;
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

    async fn signature_help(
        &self,
        params: SignatureHelpParams,
    ) -> LspResult<Option<SignatureHelp>> {
        self.provide_signature_help(params).await
    }

    async fn completion_resolve(&self, item: CompletionItem) -> LspResult<CompletionItem> {
        self.resolve_completion_documentation(item).await
    }

    async fn hover(&self, params: HoverParams) -> LspResult<Option<Hover>> {
        self.provide_hover(params).await
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> LspResult<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri;
        match self.compute_semantic_tokens(&uri, None).await {
            Some(data) => Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
                result_id: None,
                data,
            }))),
            None => Ok(None),
        }
    }

    async fn semantic_tokens_range(
        &self,
        params: SemanticTokensRangeParams,
    ) -> LspResult<Option<SemanticTokensRangeResult>> {
        let uri = params.text_document.uri;
        match self.compute_semantic_tokens(&uri, Some(params.range)).await {
            Some(data) => Ok(Some(SemanticTokensRangeResult::Tokens(SemanticTokens {
                result_id: None,
                data,
            }))),
            None => Ok(None),
        }
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        let roots = self.workspace_roots.lock().await.clone();
        let mut dirty_changes = Vec::new();
        let mut project_context_roots = Vec::new();
        let mut needs_full = false;

        // Ensure the config cache is populated for each root (on first event)
        // and reused across events in this batch. The cache avoids re-reading
        // `fossilsense.json` on every file save. `fossilsense.json` changes
        // themselves trigger `WatchDecision::Full` which bypasses the cache.
        {
            let mut cache = self.config_cache.lock().await;
            for root in &roots {
                if !cache.contains_key(root) {
                    let (config, _) = WorkspaceConfig::load(root);
                    cache.insert(root.clone(), config);
                }
            }
        }

        for change in &params.changes {
            match watched_change_in_scope(&roots, change, &self.config_cache).await {
                Some(WatchDecision::Full) => needs_full = true,
                Some(WatchDecision::ProjectContext(root)) => project_context_roots.push(root),
                Some(WatchDecision::Dirty(dirty)) => dirty_changes.push(dirty),
                None => {}
            }
        }

        let relevant_changes =
            dirty_changes.len() + project_context_roots.len() + usize::from(needs_full);
        let dirty_count = dirty_changes.len();
        if relevant_changes > 0 {
            self.session.cache.invalidate_references();
        }
        self.client
            .log_message(
                MessageType::LOG,
                format!(
                    "received {} watched file changes ({} in FossilSense scope, {} dirty files)",
                    params.changes.len(),
                    relevant_changes,
                    dirty_count
                ),
            )
            .await;

        if needs_full {
            self.spawn_index_roots(None).await;
        } else {
            if !dirty_changes.is_empty() {
                self.spawn_dirty_files(dirty_changes).await;
            }
            if !project_context_roots.is_empty() {
                self.refresh_project_context_roots(project_context_roots)
                    .await;
            }
        }
    }

    async fn did_change_workspace_folders(&self, params: DidChangeWorkspaceFoldersParams) {
        let removed: Vec<PathBuf> = params
            .event
            .removed
            .iter()
            .filter_map(|folder| uri_to_path(&folder.uri))
            .collect();
        let added: Vec<PathBuf> = params
            .event
            .added
            .iter()
            .filter_map(|folder| uri_to_path(&folder.uri))
            .collect();

        {
            let mut roots = self.workspace_roots.lock().await;
            roots.retain(|root| !removed.contains(root));
            roots.extend(added.iter().cloned());
            roots.sort();
            roots.dedup();
        }
        if !removed.is_empty() {
            self.session.cache.remove_workspace_roots(&removed).await;
            self.config_cache
                .lock()
                .await
                .retain(|root, _| !removed.contains(root));
            let removed_history_paths: Vec<PathBuf> = removed
                .iter()
                .filter_map(|root| pathing::default_completion_history_path(root).ok())
                .collect();
            self.completion_history
                .lock()
                .await
                .retain(|path, _| !removed_history_paths.contains(path));
        }
        if !added.is_empty() {
            self.spawn_index_roots(None).await;
        }

        self.client
            .log_message(
                MessageType::INFO,
                format!(
                    "workspace folders updated: added={}, removed={}, active={}",
                    added.len(),
                    removed.len(),
                    self.workspace_roots.lock().await.len()
                ),
            )
            .await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;
        if let Some(root) = self.root_for_uri(&uri).await {
            let context = self.request_context_for_root(root).await;
            self.session
                .save_document(&uri, context.engine.semantic_generation)
                .await;
        } else {
            self.session.cache.invalidate_references();
        }
        self.client
            .log_message(
                MessageType::LOG,
                "document saved; waiting for file watcher before reindex",
            )
            .await;
    }

    async fn execute_command(&self, params: ExecuteCommandParams) -> LspResult<Option<Value>> {
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
        } else if params.command == GROUPED_REFERENCES_LSP_COMMAND {
            // Role-grouped find-references: same cached search as the standard
            // `references` request, but the result carries each hit's role so
            // the client can group/label it (the LSP `Location` cannot).
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
                    // Reuses the full search-result cache shared with standard
                    // references; on a cache hit this does not redo discovery or
                    // the text-search pass.
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
