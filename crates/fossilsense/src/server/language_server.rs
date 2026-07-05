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
        *self.completion_history_mode.lock().await = parse_completion_history_mode(&params);

        let completion_provider = if self.completion_enabled.load(Ordering::Relaxed) {
            Some(CompletionOptions {
                trigger_characters: Some(completion_trigger_characters()),
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
                        change: Some(TextDocumentSyncKind::FULL),
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
                execute_command_provider: Some(tower_lsp::lsp_types::ExecuteCommandOptions {
                    commands: vec![
                        REFRESH_INDEX_LSP_COMMAND.to_string(),
                        REBUILD_INDEX_LSP_COMMAND.to_string(),
                        GROUPED_REFERENCES_LSP_COMMAND.to_string(),
                        COMPLETION_ACCEPTED_LSP_COMMAND.to_string(),
                        CLEAR_COMPLETION_HISTORY_LSP_COMMAND.to_string(),
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
        self.spawn_index_roots(None).await;
    }

    async fn shutdown(&self) -> LspResult<()> {
        self.client
            .log_message(MessageType::INFO, "FossilSense shutting down")
            .await;
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        self.open_docs.lock().await.insert(
            uri.clone(),
            (params.text_document.version, params.text_document.text),
        );
        self.client
            .log_message(MessageType::LOG, format!("opened {uri}"))
            .await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        // FULL text sync: the final change carries the entire document.
        if let Some(change) = params.content_changes.into_iter().next_back() {
            self.open_docs.lock().await.insert(
                params.text_document.uri.clone(),
                (params.text_document.version, change.text),
            );
            self.live_parse_cache
                .write()
                .await
                .remove(&params.text_document.uri);
            self.local_word_cache
                .lock()
                .await
                .remove(&params.text_document.uri);
            self.completion_memo
                .lock()
                .await
                .remove(&params.text_document.uri);
            self.reference_search_cache.clear();
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.open_docs
            .lock()
            .await
            .remove(&params.text_document.uri);
        self.live_parse_cache
            .write()
            .await
            .remove(&params.text_document.uri);
        self.local_word_cache
            .lock()
            .await
            .remove(&params.text_document.uri);
        self.completion_memo
            .lock()
            .await
            .remove(&params.text_document.uri);
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> LspResult<Option<GotoDefinitionResponse>> {
        let position = params.text_document_position_params;
        let uri = position.text_document.uri;

        let Some(text) = self.document_text(&uri).await else {
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
        let reach_scope: Option<Arc<reachability::ReachScope>> =
            self.reach_scope_for(&uri).await.map(|(_, reach)| reach);

        // Debug-gated candidate-reason logging (default off): when on, each
        // returned candidate's tier/confidence/reason is logged to the output
        // panel. The flag only adds log lines; it never changes which locations
        // are returned or their order.
        let debug_reasons = self.debug_candidate_reasons.load(Ordering::Relaxed);
        let client = self.client.clone();
        let word_for_log = word.clone();

        let result =
            tokio::task::spawn_blocking(move || -> Result<(Vec<Location>, Vec<String>)> {
                let db_path = pathing::default_index_path(&root)?;
                if !db_path.exists() {
                    return Ok((Vec::new(), Vec::new()));
                }
                let store = IndexStore::open_readonly(&db_path)?;
                let candidates = query::rank_definitions_into_candidates_with_scope(
                    store.symbols_by_name(&word)?,
                    &current_rel,
                    reach_scope.as_deref(),
                );
                let debug_lines = candidate_reason_log_lines(&candidates, debug_reasons);
                let locations = candidates
                    .iter()
                    .filter_map(|candidate| candidate_to_location(&root, candidate))
                    .collect();
                Ok((locations, debug_lines))
            })
            .await;

        match self.unwrap_query("definition", result).await {
            Some((locations, debug_lines)) if !locations.is_empty() => {
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
        let role_cache = self.reference_role_cache.clone();
        let search_cache = self.reference_search_cache.clone();
        let (indexed_generation, indexed_files) = {
            let generation = self
                .index_generations
                .lock()
                .await
                .get(&root)
                .copied()
                .unwrap_or_else(state::WorkspaceGeneration::missing)
                .as_u64();
            let indexed_file_lists = self.indexed_file_lists.lock().await;
            (
                generation,
                indexed_file_lists.get(&root).map(|files| (**files).clone()),
            )
        };
        let result = tokio::task::spawn_blocking(
            move || -> Result<(Vec<Location>, bool, references::ReferencesTiming)> {
                let (mut hits, truncated, timing) =
                    references::search_references_with_result_cache_and_files(
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
        let tables: Vec<(PathBuf, Arc<NameTable>)> = {
            let roots = self.workspace_roots.lock().await.clone();
            let name_tables = self.name_tables.lock().await;
            roots
                .into_iter()
                .filter_map(|root| name_tables.get(&root).cloned().map(|table| (root, table)))
                .collect()
        };
        if tables.is_empty() {
            return Ok(None);
        }

        let query_text = params.query;
        let result = tokio::task::spawn_blocking(move || -> Result<Vec<SymbolInformation>> {
            let mut hits = Vec::new();
            for (root_index, (root, table)) in tables.iter().enumerate() {
                for hit in table.search_ranked(&query_text, query::WORKSPACE_SYMBOL_LIMIT) {
                    hits.push((root_index, root.clone(), hit));
                }
            }
            hits.sort_by(|a, b| {
                b.2.score
                    .cmp(&a.2.score)
                    .then(a.2.name_len.cmp(&b.2.name_len))
                    .then_with(|| a.2.name.cmp(&b.2.name))
                    .then(a.0.cmp(&b.0))
            });
            hits.truncate(query::WORKSPACE_SYMBOL_LIMIT);

            if hits.is_empty() {
                return Ok(Vec::new());
            }

            let mut records_by_root_and_id = HashMap::new();
            let mut ids_by_root: HashMap<PathBuf, Vec<i64>> = HashMap::new();
            for (_, root, hit) in &hits {
                ids_by_root.entry(root.clone()).or_default().push(hit.id);
            }

            for (root, ids) in ids_by_root {
                let db_path = pathing::default_index_path(&root)?;
                if !db_path.exists() {
                    continue;
                }
                let store = IndexStore::open_readonly(&db_path)?;
                for record in store.symbols_by_ids(&ids)? {
                    records_by_root_and_id.insert((root.clone(), record.id), record);
                }
            }

            Ok(hits
                .into_iter()
                .filter_map(|(_, root, hit)| {
                    records_by_root_and_id
                        .get(&(root.clone(), hit.id))
                        .and_then(|record| record_to_symbol_information(&root, record))
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
            .get_or_parse_document(&uri, &path, version, &text)
            .await;
        let Some(index) = index else {
            return Ok(None);
        };
        // Extract symbols synchronously from the cached index.
        let symbols = index.symbols.clone();
        let document_symbols: Vec<DocumentSymbol> =
            symbols.iter().map(parsed_to_document_symbol).collect();
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
        if !self.completion_enabled.load(Ordering::Relaxed) {
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
        let local_binding_hits = parsed_document
            .as_ref()
            .map(|index| {
                query::local_completion_candidates(
                    &index.local_bindings,
                    &text,
                    position.line,
                    position.character,
                    &prefix,
                    query::COMPLETION_LIMIT,
                )
            })
            .unwrap_or_default();
        let current_file_overlay_hits = parsed_document
            .as_ref()
            .map(|index| {
                query::current_file_overlay_candidates(
                    index,
                    &text,
                    position.line,
                    position.character,
                    &prefix,
                    query::COMPLETION_LIMIT,
                )
            })
            .unwrap_or_default();

        let local_words = self.local_words_for(&uri, version, &text).await;

        let (tables, table_generations): (
            Vec<(PathBuf, Arc<NameTable>)>,
            Vec<(PathBuf, state::WorkspaceGeneration)>,
        ) = {
            let roots = self.workspace_roots.lock().await.clone();
            let name_tables = self.name_tables.lock().await;
            let index_generations = self.index_generations.lock().await;
            let mut tables = Vec::new();
            let mut generations = Vec::new();
            for root in roots {
                if let Some(table) = name_tables.get(&root).cloned() {
                    let generation = index_generations
                        .get(&root)
                        .copied()
                        .unwrap_or_else(state::WorkspaceGeneration::missing);
                    generations.push((root.clone(), generation));
                    tables.push((root, table));
                }
            }
            (tables, generations)
        };

        // Limited include-reachability scope: re-ranks candidates by their
        // `ScopeTier` (current / reachable / first-layer external / unknown /
        // global) via the shared resolver. None => whole-index ranking (scoping
        // off, no graph yet, or unresolvable path).
        let scope = self
            .reach_scope_for(&uri)
            .await
            .map(|(rel, reach)| query::CompletionScope {
                current_path: Some(rel),
                reach: (*reach).clone(),
            });

        let limit = query::COMPLETION_LIMIT;
        let locality_bonus = query::COMPLETION_LOCALITY_BONUS;

        // Per-document narrowing: reuse the previous prefix's candidate pool when
        // the new prefix extends it and the same name-table generation is in
        // play. A shortened/changed prefix or a rebuilt table generation resets
        // to a full scan.
        let completion_generation = state::combine_workspace_generations(&table_generations);
        let completion_started = tokio::time::Instant::now();
        let (prior_pools, hit_kind): (Vec<Option<Vec<usize>>>, &str) = {
            let memo = self.completion_memo.lock().await;
            match memo.get(&uri) {
                Some(m)
                    if state::completion_memo_is_valid(
                        m.generation,
                        completion_generation,
                        &m.prefix,
                        &prefix,
                    ) && prefix == m.prefix =>
                {
                    (m.pools.iter().cloned().map(Some).collect(), "hot")
                }
                Some(m)
                    if state::completion_memo_is_valid(
                        m.generation,
                        completion_generation,
                        &m.prefix,
                        &prefix,
                    ) =>
                {
                    (m.pools.iter().cloned().map(Some).collect(), "pool")
                }
                Some(_) => (vec![None; tables.len()], "cold"),
                _ => (vec![None; tables.len()], "cold"),
            }
        };
        let memo_prefix = prefix.clone();
        let command_workspace_hash = history_workspace_hash.clone();
        let command_prefix_bucket = history_prefix_bucket.clone();
        let context_ms = ordinary_started.elapsed().as_millis();

        let result = tokio::task::spawn_blocking(
            move || -> Result<(
                Vec<CompletionItem>,
                Vec<Vec<usize>>,
                crate::completion::CompletionPipelineMetrics,
                u128,
                u128,
                u128,
            )> {
                use crate::model::ScopeTier;
                let recall_started = std::time::Instant::now();
                // First cause of the open scope (if any). An `Unknown`-tier
                // candidate under an ambiguous include surfaces `Ambiguous`
                // (and the `ambiguous` label) instead of a plain `Fallback`.
                let open_reason = scope.as_ref().and_then(|s| s.reach.reason);
                // Each candidate carries its name (for dedup/sort), the
                // `(tier, confidence)` dedup key (kept separate so the dedup
                // can compare without re-deriving the tier), the packed sort
                // key, and the LSP item.
                let mut candidates: Vec<CompletionCandidate> = Vec::new();
                let mut new_pools: Vec<Vec<usize>> = Vec::with_capacity(tables.len());
                let mut recall_channels = query::CompletionRecallMetrics::default();

                // Collect indexed symbol candidates from all workspace roots. The
                // symbol `kind` is cached in the in-memory NameTable, so the hot
                // path no longer opens a read-only SQLite store per keystroke.
                let quotas = query::CompletionRecallQuotas::default_for_completion_limit(limit);
                for (idx, (_root, table)) in tables.iter().enumerate() {
                    let prior = prior_pools.get(idx).and_then(|p| p.as_deref());
                    let (hits, pool, metrics) = table.search_completion_recall_pooled(
                        &prefix,
                        quotas,
                        scope.as_ref(),
                        prior,
                    );
                    recall_channels.merge_from(metrics);
                    new_pools.push(pool);
                    candidates.extend(completion_items_for_indexed_hits(hits, open_reason));
                }

                candidates.extend(completion_items_for_local_bindings(local_binding_hits));
                let current_file_text_overlay_names: HashSet<String> = current_file_overlay_hits
                    .iter()
                    .filter(|hit| !hit.semantic || hit.detail.as_deref() == Some("text"))
                    .map(|hit| hit.name.clone())
                    .collect();
                candidates.extend(completion_items_for_current_file_overlay(
                    current_file_overlay_hits,
                ));

                // Add current-file word candidates as fallback-only items.
                // They remain useful for not-yet-indexed names, but same-name
                // indexed symbols keep their semantic kind/detail.
                for word in local_words.iter() {
                    // The token under the cursor is already the user's input,
                    // not a reusable completion candidate. Returning it as an
                    // exact local word makes the editor prefer `prin` over an
                    // indexed semantic candidate such as `printf`.
                    if word == &prefix {
                        continue;
                    }
                    let Some(word_score) =
                        query::completion_word_score(&prefix, word, locality_bonus)
                    else {
                        continue;
                    };
                    let tier = ScopeTier::Global;
                    // Raw local words are fallback text suggestions, not
                    // current-file definitions. Keep their un-packed text score
                    // so indexed symbols retain resolver-tier priority.
                    let (confidence, _reason) =
                        crate::resolver::confidence_reason_for(tier, false, None);
                    let sort_text = format!("{:08}", 100_000_000 - word_score);
                    let mut exact_indexed = Vec::new();
                    for (_, table) in &tables {
                        exact_indexed.extend(exact_indexed_completion_candidates_for_local_word(
                            table.as_ref(),
                            word,
                            word_score,
                            scope.as_ref(),
                            open_reason,
                            limit,
                        ));
                    }
                    if !exact_indexed.is_empty() {
                        candidates.extend(exact_indexed);
                        continue;
                    }
                    if current_file_text_overlay_names.contains(word.as_str()) {
                        continue;
                    }
                    let mut evidence = crate::completion::CandidateEvidence::new(
                        crate::completion::CandidateSource::LocalWord,
                        tier,
                        confidence,
                        word_score,
                    );
                    evidence.kind = crate::completion::CompletionCandidateKind::Text;
                    set_completion_history_key(&mut evidence, word);
                    candidates.push(CompletionCandidate::new(
                        word.clone(),
                        evidence,
                        CompletionItem {
                            label: word.clone(),
                            kind: Some(CompletionItemKind::TEXT),
                            sort_text: Some(sort_text),
                            ..Default::default()
                        },
                    ));
                }

                let recall_ms = recall_started.elapsed().as_millis();
                let merge_rank_started = std::time::Instant::now();
                let mut output = crate::completion::run_evidence_aware_pipeline_with_context(
                    candidates,
                    limit,
                    crate::completion::CompletionRankContext {
                        intent,
                        history_enabled,
                        history: history_snapshot,
                        prefix_bucket: history_prefix_bucket,
                    },
                );
                output.metrics.recall_channels = recall_channels;
                let merge_rank_ms = merge_rank_started.elapsed().as_millis();

                let render_started = std::time::Instant::now();
                let mut items: Vec<CompletionItem> = output
                    .items
                    .into_iter()
                    .map(|candidate| {
                        let mut item = candidate.payload;
                        if history_enabled {
                            if let Some(workspace_hash) = command_workspace_hash.as_deref() {
                                attach_completion_history_accept_command(
                                    &mut item,
                                    candidate.evidence,
                                    workspace_hash,
                                    intent.kind,
                                    &command_prefix_bucket,
                                );
                            }
                        }
                        item
                    })
                    .collect();
                apply_final_completion_sort_text(&mut items);
                let render_ms = render_started.elapsed().as_millis();

                Ok((items, new_pools, output.metrics, recall_ms, merge_rank_ms, render_ms))
            },
        )
        .await;

        // The list is always incomplete: results are truncated to
        // `COMPLETION_LIMIT` and the recall threshold widens with prefix
        // length, so the editor must re-query with the full current prefix on
        // every keystroke. This lets longer-named symbols re-enter the
        // truncation window as the user keeps typing, and prevents an empty
        // first batch from sticking as a "complete" no-match list.
        match self.unwrap_query("completion", result).await {
            Some((items, new_pools, metrics, recall_ms, merge_rank_ms, render_ms)) => {
                let timings = crate::completion::CompletionStageTimings {
                    total_ms: completion_started.elapsed().as_millis(),
                    context_ms,
                    recall_ms,
                    merge_rank_ms,
                    render_ms,
                };
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
                self.completion_memo.lock().await.insert(
                    uri,
                    state::CompletionMemo {
                        prefix: memo_prefix,
                        generation: completion_generation,
                        pools: new_pools,
                    },
                );
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
                Some(WatchDecision::Dirty(dirty)) => dirty_changes.push(dirty),
                None => {}
            }
        }

        let relevant_changes = dirty_changes.len() + usize::from(needs_full);
        let dirty_count = dirty_changes.len();
        if relevant_changes > 0 {
            self.reference_search_cache.clear();
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
        } else if !dirty_changes.is_empty() {
            self.spawn_dirty_files(dirty_changes).await;
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        self.reference_search_cache.clear();
        self.client
            .log_message(
                MessageType::LOG,
                format!(
                    "saved {} (waiting for file watcher before reindex)",
                    params.text_document.uri
                ),
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
            let role_cache = self.reference_role_cache.clone();
            let search_cache = self.reference_search_cache.clone();
            let (indexed_generation, indexed_files) = {
                let generation = self
                    .index_generations
                    .lock()
                    .await
                    .get(&root)
                    .copied()
                    .unwrap_or_else(state::WorkspaceGeneration::missing)
                    .as_u64();
                let indexed_file_lists = self.indexed_file_lists.lock().await;
                (
                    generation,
                    indexed_file_lists.get(&root).map(|files| (**files).clone()),
                )
            };
            let result = tokio::task::spawn_blocking(
                move || -> Result<(Vec<GroupedReferenceItem>, bool, references::ReferencesTiming)> {
                    // Reuses the full search-result cache shared with standard
                    // references; on a cache hit this does not redo discovery or
                    // the text-search pass.
                    let (mut hits, truncated, timing) =
                        references::search_references_with_result_cache_and_files(
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
