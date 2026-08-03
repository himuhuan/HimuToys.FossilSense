use super::*;

impl Backend {
    pub(super) async fn initialize_server(
        &self,
        params: InitializeParams,
    ) -> LspResult<InitializeResult> {
        let roots = workspace_roots_from_initialize(&params);
        *self.workspace_roots.lock().await = roots;
        *self.include_paths.lock().await = parse_include_paths(&params);
        *self.go_module_paths.lock().await = parse_go_module_paths(&params);
        *self.protobuf_c_enabled.lock().await = parse_protobuf_c_enabled(&params);
        *self.protobuf_c_proto_paths.lock().await = parse_protobuf_c_proto_paths(&params);
        self.session
            .cache
            .set_semantic_index_memory_budget_mb(parse_semantic_index_memory_budget_mb(&params));

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
                declaration_provider: Some(DeclarationCapability::Simple(true)),
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
                        POSSIBLE_TARGETS_LSP_COMMAND.to_string(),
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
}
