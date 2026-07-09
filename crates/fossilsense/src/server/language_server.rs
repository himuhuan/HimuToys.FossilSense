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
                        PROJECT_CONTEXTS_LSP_COMMAND.to_string(),
                        SET_PROJECT_CONTEXT_LSP_COMMAND.to_string(),
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
        self.preload_completion_history().await;
        self.spawn_index_roots(None).await;
    }

    async fn shutdown(&self) -> LspResult<()> {
        self.client
            .log_message(MessageType::INFO, "FossilSense shutting down")
            .await;
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        self.handle_did_open(params).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        self.handle_did_change(params).await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.handle_did_close(params).await;
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> LspResult<Option<GotoDefinitionResponse>> {
        self.handle_goto_definition(params).await
    }

    async fn references(&self, params: ReferenceParams) -> LspResult<Option<Vec<Location>>> {
        self.handle_references(params).await
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> LspResult<Option<Vec<SymbolInformation>>> {
        self.handle_workspace_symbol(params).await
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> LspResult<Option<DocumentSymbolResponse>> {
        self.handle_document_symbol(params).await
    }

    async fn completion(&self, params: CompletionParams) -> LspResult<Option<CompletionResponse>> {
        self.handle_completion(params).await
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
        self.handle_did_change_watched_files(params).await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        self.handle_did_save(params).await;
    }

    async fn execute_command(&self, params: ExecuteCommandParams) -> LspResult<Option<Value>> {
        self.handle_execute_command(params).await
    }
}
