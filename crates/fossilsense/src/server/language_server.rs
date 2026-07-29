use super::*;

mod commands;
mod initialization;
mod watched_files;

#[async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> LspResult<InitializeResult> {
        self.initialize_server(params).await
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "FossilSense initialized")
            .await;
        self.preload_completion_history().await;
        self.spawn_index_roots(None).await;
        super::resource_monitor::spawn_resource_usage_reporter(
            self.client.clone(),
            self.workspace_roots.clone(),
            self.resource_monitor_shutdown.clone(),
        );
    }

    async fn shutdown(&self) -> LspResult<()> {
        self.resource_monitor_shutdown.notify_one();
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
        if let Some(path) = uri_to_path(&uri) {
            self.invalidate_external_source_path_authorization(&path)
                .await;
        }
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
        let uri = params.text_document.uri;
        let path = uri_to_path(&uri);
        self.session.close_document(&uri).await;
        if let Some(path) = path {
            self.invalidate_external_source_path_authorization(&path)
                .await;
        }
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> LspResult<Option<GotoDefinitionResponse>> {
        self.navigate_symbol(params, NavigationOperation::Definition)
            .await
    }

    async fn goto_declaration(
        &self,
        params: GotoDeclarationParams,
    ) -> LspResult<Option<GotoDeclarationResponse>> {
        self.navigate_symbol(params, NavigationOperation::Declaration)
            .await
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
        let reference_cache_epoch = search_cache.epoch();
        let context = self.request_context_for_root(root.clone()).await;
        let indexed_generation = context.engine.epoch.as_u64();
        let indexed_files = context.engine.indexed_files.clone();
        let semantic_family = context
            .engine
            .workspace_semantics
            .language_for_uri(&uri)
            .semantic_family();
        let workspace_config = context.engine.workspace_semantics.workspace.clone();
        let language_resolver = context.engine.workspace_semantics.language.clone();
        let result = tokio::task::spawn_blocking(
            move || -> Result<(Vec<Location>, bool, references::ReferencesTiming)> {
                let (mut hits, truncated, timing) =
                    references::search_references_with_shared_files_for_family(
                        &root,
                        &search_word,
                        &role_cache,
                        &search_cache,
                        indexed_generation,
                        indexed_files,
                        semantic_family,
                        reference_cache_epoch,
                        workspace_config,
                        language_resolver,
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
        let tables: Vec<(
            PathBuf,
            Arc<crate::declaration_index::SemanticDeclarationIndex>,
            Arc<crate::call_service::CallReadHandle>,
        )> = {
            let roots = self.workspace_roots.lock().await.clone();
            let mut tables = Vec::new();
            for root in roots {
                let context = self.request_context_for_root(root).await;
                if let (Some(index), Some(read_handle)) = (
                    context.engine.declaration_index.clone(),
                    context.engine.call_read_handle.clone(),
                ) {
                    tables.push((context.engine.root.clone(), index, read_handle));
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
            for (root_index, (_, index, _)) in tables.iter().enumerate() {
                for hit in index
                    .name_table()
                    .search_ranked(&query_text, query::WORKSPACE_SYMBOL_LIMIT)
                {
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

            let mut payloads = Vec::with_capacity(tables.len());
            for (root_index, (_, index, read_handle)) in tables.iter().enumerate() {
                let ids: Vec<_> = hits
                    .iter()
                    .filter(|(candidate_root, _)| *candidate_root == root_index)
                    .map(|(_, hit)| hit.id)
                    .collect();
                payloads.push(
                    index
                        .payloads_by_ids(read_handle, &ids)?
                        .into_iter()
                        .map(|row| (row.id, row))
                        .collect::<HashMap<_, _>>(),
                );
            }

            Ok(hits
                .into_iter()
                .filter_map(|(root_index, hit)| {
                    let root = &tables[root_index].0;
                    payloads[root_index]
                        .get(&hit.id)
                        .and_then(|row| declaration_to_symbol_information(root, row))
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
        let source_language = self
            .request_context_for_uri(&uri)
            .await
            .map(|context| context.engine.workspace_semantics.language_for_uri(&uri))
            .unwrap_or_else(|| SourceLanguage::default_for_path(&path));

        let started = tokio::time::Instant::now();
        // Live parse served from the in-memory cache (one parse per document
        // version, shared across semantic tokens, completion, and symbols).
        let index = self
            .get_or_parse_document_with_language(
                &uri,
                &path,
                version,
                &text,
                parser::ParseFacts::DECLARATIONS | parser::ParseFacts::INCLUDES,
                source_language,
            )
            .await;
        let Some(index) = index else {
            return Ok(None);
        };
        // Extract persistent symbols synchronously from the cached index.
        let document_symbols: Vec<DocumentSymbol> = index
            .persistent_facts()
            .declarations
            .iter()
            .map(declaration_to_document_symbol)
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

        let current_root = self.root_for_uri(&uri).await;
        let primary_context = match current_root.as_ref() {
            Some(root) => Some(self.request_context_for_root(root.clone()).await),
            None => None,
        };
        let source_language = primary_context
            .as_ref()
            .map(|context| context.engine.workspace_semantics.language_for_uri(&uri))
            .unwrap_or_else(|| SourceLanguage::default_for_path(Path::new(uri.path())));

        if source_language == SourceLanguage::Go {
            if let Some(import_context) = go_import_completion::go_import_completion_context(
                &text,
                position.line,
                position.character,
            ) {
                let (table, current_package_key) =
                    match (current_root.as_ref(), primary_context.as_ref()) {
                        (Some(root), Some(context)) => {
                            let current_package_key = uri_to_path(&uri).and_then(|path| {
                                let identity_path = pathing::relative_slash_path(root, &path)
                                    .unwrap_or_else(|_| pathing::normalize_abs_path(&path));
                                go_import_completion::current_go_package_key(&identity_path, &text)
                            });
                            (context.engine.go_import_table.clone(), current_package_key)
                        }
                        _ => (None, None),
                    };
                return Ok(Some(match table {
                    Some(table) => table.complete(&import_context, current_package_key.as_deref()),
                    None => empty_completion_list(true),
                }));
            }
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
            current_root.clone()
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
                self.get_or_parse_document_with_language(
                    &uri,
                    &path,
                    version,
                    &text,
                    parser::ParseFacts::COMPLETION,
                    source_language,
                )
                .await
            }
            None => None,
        };
        let local_words = self.local_words_for(&uri, version, &text).await;

        let mut contexts = {
            let roots = self.workspace_roots.lock().await.clone();
            let mut contexts = Vec::with_capacity(roots.len());
            for root in roots {
                if current_root.as_ref() == Some(&root) {
                    if let Some(context) = primary_context.as_ref() {
                        contexts.push(context.clone());
                        continue;
                    }
                }
                contexts.push(self.request_context_for_root(root).await);
            }
            contexts
        };
        contexts
            .sort_by_key(|context| current_root.as_deref() != Some(context.engine.root.as_path()));
        let mut tables = Vec::new();
        let mut table_roots = Vec::new();
        let mut table_semantic_generations = Vec::new();
        let mut table_generations = Vec::new();
        let mut effective_completion_scope = None;
        for context in &contexts {
            if let Some(table) = context.engine.name_table.clone() {
                let overlay = self
                    .candidate_overlay_snapshot_from_documents(
                        &context.engine.root,
                        context.engine.semantic_generation,
                        context.engine.reach_graph.as_deref(),
                        context.engine.indexed_files.as_deref().map(Vec::as_slice),
                        context.engine.workspace_semantics.clone(),
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
                                .map(|graph| {
                                    let direct_external_files =
                                        graph.directly_included_external_paths_from(&rel);
                                    query::CompletionScope {
                                        reach: graph.reachable(&rel).as_ref().clone(),
                                        current_path: Some(rel),
                                        direct_external_files,
                                    }
                                })
                        });
                }
                let overlay_names = overlay.completion_names();
                let overlay_handles = overlay_names
                    .iter()
                    .filter_map(|entry| {
                        entry
                            .candidate_handle
                            .clone()
                            .map(|handle| (entry.id, handle))
                    })
                    .collect();
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
                            entry.semantic_family,
                        )
                    })
                    .collect();
                let effective_table = table
                    .with_updated_family_paths(overlay.shadowed_paths(), rows)
                    .with_direct_include_overrides(overlay.direct_include_overrides());
                let overlay_fallbacks = overlay.fallback_completion_facts().iter().map(|entry| {
                    (
                        crate::completion::ordinary_service::FallbackCompletionName {
                            name: entry.fact.name.clone(),
                            kind_hint: entry.fact.kind_hint,
                            detail: entry.fact.detail.clone(),
                            path: entry.path.clone(),
                        },
                        overlay
                            .semantic_family_for_path(&entry.path)
                            .unwrap_or(crate::semantic_model::SemanticFamily::CFamily),
                    )
                });
                let effective_fallback_table = context
                    .engine
                    .fallback_completion_table
                    .with_updated_family_paths(overlay.shadowed_paths(), overlay_fallbacks);
                table_generations.push((context.engine.root.clone(), context.engine.epoch));
                table_roots.push(context.engine.root.clone());
                table_semantic_generations.push(context.engine.semantic_generation);
                tables.push(OrdinaryCompletionNameTable {
                    table: Arc::new(effective_table),
                    overlay_handles,
                    fallback_table: Arc::new(effective_fallback_table),
                });
            }
        }
        if tables.is_empty() {
            if let (Some(_), Some(path)) = (parsed_document.as_ref(), uri_to_path(&uri)) {
                let standalone_root = path.parent().unwrap_or(path.as_path()).to_path_buf();
                table_generations.push((standalone_root.clone(), state::EngineEpoch::missing()));
                table_roots.push(standalone_root);
                table_semantic_generations.push(SemanticGeneration::MISSING);
                tables.push(OrdinaryCompletionNameTable {
                    table: Arc::new(crate::query::NameTable::build(Vec::new())),
                    overlay_handles: std::collections::HashMap::new(),
                    fallback_table: Arc::new(
                        crate::completion::ordinary_service::FallbackCompletionNameTable::default(),
                    ),
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
        self.handle_watched_file_changes(params).await;
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
            self.remove_workspace_runtime_roots(&removed).await;
            self.config_cache
                .lock()
                .await
                .retain(|root, _| !removed.contains(root));
            #[cfg(test)]
            self.invalidate_external_source_root_cache(&removed).await;
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
        self.execute_server_command(params).await
    }
}
