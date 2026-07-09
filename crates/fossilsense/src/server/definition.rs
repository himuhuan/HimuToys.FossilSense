use super::*;

impl Backend {
    pub(super) async fn handle_goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> LspResult<Option<GotoDefinitionResponse>> {
        let position = params.text_document_position_params;
        let uri = position.text_document.uri;

        let Some((version, text)) = self.document_snapshot(&uri).await else {
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
        let live_records = match uri_to_path(&uri) {
            Some(path) if !current_rel.is_empty() => self
                .get_or_parse_document(&uri, &path, version, &text)
                .await
                .map(|index| live_definition_records_for_word(&index, &word, &current_rel))
                .unwrap_or_default(),
            _ => Vec::new(),
        };

        // Reachability scope for candidate tier resolution (Current / Reachable
        // / External / Unknown / Global). A file in the set is proved reachable
        // regardless of whether the set is open; an open scope routes
        // not-proven-reachable workspace candidates to `Unknown` (preserving
        // the R1 "open scope does not bury unreachable" softening as a tier).
        // `None` when scoping is disabled or no graph exists yet -- non-current
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
                let live_records_replace_current_file = !live_records.is_empty();
                let mut records = live_records;
                let db_path = pathing::default_index_path(&root)?;
                if db_path.exists() {
                    let store = IndexStore::open_readonly(&db_path)?;
                    let mut indexed_records = store.symbol_read_view().symbols_by_name(&word)?;
                    if live_records_replace_current_file {
                        indexed_records.retain(|record| record.path != current_rel);
                    }
                    records.extend(indexed_records);
                }
                dedup_symbol_records(&mut records);
                let candidates = query::rank_definitions_into_candidates_with_scope(
                    records,
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
}

fn live_definition_records_for_word(
    index: &FileSemanticIndex,
    word: &str,
    current_rel: &str,
) -> Vec<crate::store::SymbolRecord> {
    let persistent_facts = index.persistent_facts();
    let mut records: Vec<_> = persistent_facts
        .symbols
        .iter()
        .filter(|symbol| symbol.name == word)
        .map(|symbol| crate::store::SymbolRecord {
            id: 0,
            name: symbol.name.clone(),
            kind: live_symbol_kind(symbol.kind).to_string(),
            role: live_symbol_role(symbol.role).to_string(),
            path: current_rel.to_string(),
            start_line: symbol.start_line as u32,
            start_col: symbol.start_col as u32,
            end_line: symbol.end_line as u32,
            end_col: symbol.end_col as u32,
            signature: symbol.signature.clone(),
            guard: symbol.guard.clone(),
            source: crate::store::FileSource::Workspace.as_str().to_string(),
            directly_included: false,
        })
        .collect();

    // The lexical symbol pass normally covers typedefs, functions, macros,
    // globals, enum constants, and fields. Fall back to AST-only type facts for
    // cases the lexical pass intentionally cannot infer, keeping the common path
    // duplicate-free for typedefs such as `typedef struct { ... } Boom;`.
    if records.is_empty() {
        records.extend(
            persistent_facts
                .aliases
                .iter()
                .filter(|alias| alias.alias == word)
                .map(|alias| crate::store::SymbolRecord {
                    id: 0,
                    name: alias.alias.clone(),
                    kind: "type".to_string(),
                    role: "definition".to_string(),
                    path: current_rel.to_string(),
                    start_line: alias.start_line as u32,
                    start_col: alias.start_col as u32,
                    end_line: alias.end_line as u32,
                    end_col: alias.end_col as u32,
                    signature: alias.alias.clone(),
                    guard: None,
                    source: crate::store::FileSource::Workspace.as_str().to_string(),
                    directly_included: false,
                }),
        );
        records.extend(
            persistent_facts
                .records
                .iter()
                .filter(|record| {
                    record.display_name == word
                        || record.tag_name.as_deref() == Some(word)
                        || record.typedef_name.as_deref() == Some(word)
                })
                .map(|record| crate::store::SymbolRecord {
                    id: 0,
                    name: word.to_string(),
                    kind: "type".to_string(),
                    role: "definition".to_string(),
                    path: current_rel.to_string(),
                    start_line: record.start_line as u32,
                    start_col: record.start_col as u32,
                    end_line: record.end_line as u32,
                    end_col: record.end_col as u32,
                    signature: record.signature.clone(),
                    guard: None,
                    source: crate::store::FileSource::Workspace.as_str().to_string(),
                    directly_included: false,
                }),
        );
    }

    dedup_symbol_records(&mut records);
    records
}

fn live_symbol_kind(kind: parser::SymbolKind) -> &'static str {
    match kind {
        parser::SymbolKind::Function => "function",
        parser::SymbolKind::Macro => "macro",
        parser::SymbolKind::Type => "type",
        parser::SymbolKind::EnumConstant => "enum_constant",
        parser::SymbolKind::GlobalVariable => "global_variable",
        parser::SymbolKind::Field => "field",
    }
}

fn live_symbol_role(role: parser::SymbolRole) -> &'static str {
    match role {
        parser::SymbolRole::Definition => "definition",
        parser::SymbolRole::Declaration => "declaration",
    }
}

fn dedup_symbol_records(records: &mut Vec<crate::store::SymbolRecord>) {
    let mut seen = std::collections::HashSet::new();
    records.retain(|record| {
        seen.insert((
            record.name.clone(),
            record.kind.clone(),
            record.role.clone(),
            record.path.clone(),
            record.start_line,
            record.start_col,
            record.end_line,
            record.end_col,
        ))
    });
}
