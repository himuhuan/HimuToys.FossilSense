use super::request_target;
use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NavigationOperation {
    Declaration,
    Definition,
}

impl NavigationOperation {
    fn label(self) -> &'static str {
        match self {
            Self::Declaration => "declaration",
            Self::Definition => "definition",
        }
    }
}

impl Backend {
    pub(super) async fn navigate_symbol(
        &self,
        params: GotoDefinitionParams,
        operation: NavigationOperation,
    ) -> LspResult<Option<GotoDefinitionResponse>> {
        let mut timer = query_session::BindingTimer::new(self, operation.label());
        let result = self
            .navigate_symbol_timed(params, operation, &mut timer)
            .await;
        timer.observation.completed = true;
        timer.observation.returned = result.as_ref().is_ok_and(Option::is_some);
        timer.log().await;
        result
    }

    async fn navigate_symbol_timed(
        &self,
        params: GotoDefinitionParams,
        operation: NavigationOperation,
        timer: &mut query_session::BindingTimer<'_>,
    ) -> LspResult<Option<GotoDefinitionResponse>> {
        let position = params.text_document_position_params;
        let uri = position.text_document.uri;

        let started = std::time::Instant::now();
        let session = self.capture_query_session(&uri).await;
        timer.observation.capture_us = started.elapsed().as_micros();
        let Some(query_session) = session else {
            return Ok(None);
        };
        let root = query_session.root.clone();
        let context = query_session.context.clone();
        let documents = query_session.documents.clone();
        let Some((version, text)) = self.document_snapshot_from_request(&uri, &documents).await
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

        let current_abs = uri_to_path(&uri);
        let current_rel = current_abs
            .as_deref()
            .and_then(|path| pathing::relative_slash_path(&root, path).ok())
            .unwrap_or_default();
        let source_language = context
            .engine
            .workspace_semantics
            .selection_for_uri(&uri, &text)
            .language;
        let source_cursor_byte =
            query::byte_offset_at(&text, position.position.line, position.position.character);

        let target = self
            .resolve_request_target(
                &query_session,
                request_target::RequestTargetInput {
                    uri: &uri,
                    document: (version, text.clone()),
                    word: &word,
                    cursor_byte: source_cursor_byte,
                    source_language,
                },
                timer,
            )
            .await;
        let syntax = match target {
            request_target::RequestTarget::Label(label) => {
                let location = Location {
                    uri,
                    range: tower_lsp::lsp_types::Range {
                        start: source_position_for_byte(&text, label.start_byte),
                        end: source_position_for_byte(&text, label.end_byte),
                    },
                };
                return Ok(Some(GotoDefinitionResponse::Array(vec![location])));
            }
            request_target::RequestTarget::Local(local) => {
                let render_started = std::time::Instant::now();
                let binding = &local.parsed.local_bindings[local.binding.binding_index];
                let start = source_position_for_byte(&text, binding.decl_start_byte);
                let end = tower_lsp::lsp_types::Position {
                    line: start.line,
                    character: start.character + binding.name.encode_utf16().count() as u32,
                };
                let result = Some(GotoDefinitionResponse::Array(vec![Location {
                    uri,
                    range: tower_lsp::lsp_types::Range { start, end },
                }]));
                timer.observation.render_us = render_started.elapsed().as_micros();
                return Ok(result);
            }
            request_target::RequestTarget::Members(members) => {
                let locations = self
                    .member_entity_locations(
                        &query_session,
                        members,
                        operation == NavigationOperation::Declaration,
                    )
                    .await;
                return Ok(
                    (!locations.is_empty()).then_some(GotoDefinitionResponse::Array(locations))
                );
            }
            request_target::RequestTarget::Workspace(syntax) => syntax,
            request_target::RequestTarget::Unavailable(unavailable) => {
                if unavailable.is_failure() {
                    self.client
                        .log_message(MessageType::ERROR, unavailable.diagnostic())
                        .await;
                }
                return Ok(None);
            }
        };
        // Reachability scope for candidate tier resolution (Current / Reachable
        // / External / Unknown / Global). A file in the set is proved reachable
        // regardless of whether the set is open; an open scope routes
        // not-proven-reachable workspace candidates to `Unknown` (preserving
        // the R1 "open scope does not bury unreachable" softening as a tier).
        // `None` when scoping is disabled or no graph exists yet — non-current
        // workspace files then fall back to `Global`.
        let total_started = std::time::Instant::now();
        let semantic_family = source_language.semantic_family();
        let reach_started = std::time::Instant::now();
        let reach_scope: Option<Arc<reachability::ReachScope>> = self
            .reach_scope_from_context(&uri, &context)
            .map(|(_, reach)| reach);
        let mut reach_us = reach_started.elapsed().as_micros();
        let declaration_read = context.engine.declaration_read_context();
        let reach_graph = context.engine.reach_graph.clone();
        let overlay_started = std::time::Instant::now();
        let overlay = self
            .candidate_overlay_snapshot_from_documents(&root, context.engine.clone(), documents)
            .await;
        timer.observation.overlay_us = overlay_started.elapsed().as_micros();
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

        let reads = timer.reads.clone();
        let result = tokio::task::spawn_blocking(
            move || -> Result<(Vec<Location>, Vec<String>, SemanticRequestPerf)> {
                let _reads = crate::call_service::ReadSessionProbe::enter(reads);
                let query_started = std::time::Instant::now();
                let declaration_read = declaration_read?;
                let service = crate::candidate_service::CandidateQueryService::new_for_family(
                    declaration_read.as_ref(),
                    &overlay,
                    &current_rel,
                    reach_scope.as_deref(),
                    reach_graph.as_deref(),
                    semantic_family,
                );
                let target = request_target::resolve_workspace_target(
                    &service,
                    &word,
                    &syntax,
                    source_position,
                )?;
                let semantic_set = target.semantic_set;
                let semantic_count = semantic_set
                    .all
                    .iter()
                    .map(|group| group.candidates.len())
                    .sum();
                let related = service.entity_locations_at(
                    &target.subjects,
                    operation == NavigationOperation::Declaration,
                    Some(source_position),
                )?;
                let candidates = related.candidates();
                let mut perf = SemanticRequestPerf {
                    reach_us,
                    entity_visits: related.coverage.entities,
                    entity_edges: related.coverage.edges,
                    entity_locations: related.locations.len(),
                    entity_truncated: related.coverage.truncated,
                    ..Default::default()
                };
                perf.include_non_callable_candidates(semantic_count);
                perf.query_us = query_started.elapsed().as_micros();
                let mut debug_lines = candidate_reason_log_lines(&candidates, debug_reasons);
                if debug_reasons {
                    debug_lines.insert(0, candidate_set_debug_line(&semantic_set));
                    debug_lines.push(format!(
                        "entity_locations: {} entities={} edges={} truncated={}",
                        related.diagnostic(),
                        related.coverage.entities,
                        related.coverage.edges,
                        related.coverage.truncated
                    ));
                }
                let render_started = std::time::Instant::now();
                let locations: Vec<Location> = candidates
                    .iter()
                    .filter_map(|candidate| candidate_to_location(&root, candidate))
                    .collect();
                perf.returned = locations.len();
                perf.render_us += render_started.elapsed().as_micros();
                Ok((locations, debug_lines, perf))
            },
        )
        .await;

        let metrics = result
            .as_ref()
            .ok()
            .and_then(|result| result.as_ref().ok().map(|(_, _, metrics)| *metrics))
            .unwrap_or_default();
        timer.observation.query_us = metrics.query_us;
        timer.observation.entity_visits = metrics.entity_visits;
        timer.observation.entity_edges = metrics.entity_edges;
        timer.observation.entity_locations = metrics.entity_locations;
        timer.observation.entity_truncated = metrics.entity_truncated;

        timer.observation.hydration_us = metrics.hydration_us;
        timer.observation.render_us = metrics.render_us;
        self.perf_log(|| metrics.log_line(operation.label(), total_started.elapsed().as_micros()))
            .await;

        match self.unwrap_query(operation.label(), result).await {
            Some((locations, debug_lines, _)) if !locations.is_empty() => {
                if !debug_lines.is_empty() {
                    client
                        .log_message(
                            MessageType::INFO,
                            format!(
                                "FossilSense goto {} '{}': {} candidate(s) (tier/confidence/reason):",
                                operation.label(),
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

/// Set-level uncertainty evidence for the debug-gated candidate-reason log:
/// how confidently the shared candidate set proved its focused answer and how
/// many recalled same-name candidates were suppressed from presentation.
fn candidate_set_debug_line(
    set: &crate::model::CandidateSet<crate::candidate_service::ResolvedDeclarationCandidate>,
) -> String {
    format!(
        "candidate_set: disposition={} suppressed={} truncated={} scope_open={}",
        set.disposition.as_str(),
        set.alternative_count,
        set.coverage.truncated,
        set.coverage.scope_open,
    )
}

/// Prove the nearest visible parameter/local binding for `word` at the cursor,
/// using only the request document. Shared by navigation, hover, and possible
/// targets so every feature answers a lexically bound identifier identically.
pub(super) fn visible_local_binding_at(
    current_path: &str,
    text: &str,
    word: &str,
    position: tower_lsp::lsp_types::Position,
    language: crate::config::SourceLanguage,
) -> Option<crate::parser::LocalBinding> {
    let cursor_byte = query::byte_offset_at(text, position.line, position.character);
    let parsed = parser::parse_with_handle_and_language(
        Path::new(current_path),
        text,
        language,
        None,
        parser::ParseFacts::LOCAL_DECLS,
    );
    query::visible_local_binding(&parsed.local_bindings, word, cursor_byte).cloned()
}

pub(super) fn local_binding_location(
    uri: &Url,
    current_path: &str,
    text: &str,
    word: &str,
    position: tower_lsp::lsp_types::Position,
    language: crate::config::SourceLanguage,
) -> Option<Location> {
    let binding = visible_local_binding_at(current_path, text, word, position, language)?;
    let start = source_position_for_byte(text, binding.decl_start_byte);
    let end = tower_lsp::lsp_types::Position {
        line: start.line,
        character: start.character + binding.name.encode_utf16().count() as u32,
    };
    Some(Location {
        uri: uri.clone(),
        range: tower_lsp::lsp_types::Range { start, end },
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LabelNavigation<T> {
    NotLabelSyntax,
    MissingDefinition,
    Found(T),
    Failed(String),
}

pub(super) fn label_navigation_location(
    uri: &Url,
    current_path: &str,
    text: &str,
    word: &str,
    cursor_byte: usize,
    language: crate::config::SourceLanguage,
) -> LabelNavigation<Location> {
    let (start_byte, end_byte) = match label_navigation_byte_range_with_language(
        current_path,
        text,
        word,
        cursor_byte,
        language,
    ) {
        LabelNavigation::NotLabelSyntax => return LabelNavigation::NotLabelSyntax,
        LabelNavigation::MissingDefinition => return LabelNavigation::MissingDefinition,
        LabelNavigation::Found(range) => range,
        LabelNavigation::Failed(reason) => return LabelNavigation::Failed(reason),
    };
    LabelNavigation::Found(Location {
        uri: uri.clone(),
        range: tower_lsp::lsp_types::Range {
            start: source_position_for_byte(text, start_byte),
            end: source_position_for_byte(text, end_byte),
        },
    })
}

/// Resolve a label query using only the request document's syntax tree.
///
/// The selected identifier must be tree-sitter's `label` field on either a
/// `goto_statement` or `labeled_statement`. Labels are then searched only in
/// the nearest function-like scope; nested functions and C++ lambdas form new
/// scopes and are not traversed from an outer query.
#[cfg(test)]
fn label_definition_byte_range(
    current_path: &str,
    text: &str,
    word: &str,
    cursor_byte: usize,
) -> Option<(usize, usize)> {
    match label_navigation_byte_range(current_path, text, word, cursor_byte) {
        LabelNavigation::Found(range) => Some(range),
        LabelNavigation::NotLabelSyntax
        | LabelNavigation::MissingDefinition
        | LabelNavigation::Failed(_) => None,
    }
}

#[cfg(test)]
fn label_navigation_byte_range(
    current_path: &str,
    text: &str,
    word: &str,
    cursor_byte: usize,
) -> LabelNavigation<(usize, usize)> {
    label_navigation_byte_range_with_language(
        current_path,
        text,
        word,
        cursor_byte,
        crate::config::SourceLanguage::default_for_path(Path::new(current_path)),
    )
}

fn label_navigation_byte_range_with_language(
    _current_path: &str,
    text: &str,
    word: &str,
    cursor_byte: usize,
    language: crate::config::SourceLanguage,
) -> LabelNavigation<(usize, usize)> {
    match super::request_target::label_target_byte_range(text, word, cursor_byte, language) {
        super::request_target::LabelTargetResolution::NotLabelSyntax => {
            LabelNavigation::NotLabelSyntax
        }
        super::request_target::LabelTargetResolution::MissingDefinition => {
            LabelNavigation::MissingDefinition
        }
        super::request_target::LabelTargetResolution::Found(range) => LabelNavigation::Found(range),
        super::request_target::LabelTargetResolution::Failed(reason) => {
            LabelNavigation::Failed(reason)
        }
    }
}

pub(super) fn label_navigation_syntax_hint(text: &str, word: &str, cursor_byte: usize) -> bool {
    super::request_target::label_target_syntax_hint(text, word, cursor_byte)
}

pub(super) fn source_position_for_byte(text: &str, byte: usize) -> tower_lsp::lsp_types::Position {
    let byte = byte.min(text.len());
    let before = &text[..byte];
    let line = before.bytes().filter(|value| *value == b'\n').count() as u32;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let character = before[line_start..].encode_utf16().count() as u32;
    tower_lsp::lsp_types::Position { line, character }
}

/// Local bindings inhabit C's ordinary-identifier namespace.  Do not let one
/// shadow a syntactically distinct member, tag, or label query merely because
/// the spelling is the same.
pub(super) fn ordinary_identifier_navigation_context(line_text: &str, character: u32) -> bool {
    if query::is_member_completion_context(line_text, character) {
        return false;
    }

    let cursor = query::byte_offset_at(line_text, 0, character).min(line_text.len());
    let bytes = line_text.as_bytes();
    let is_ident = |byte: u8| byte == b'_' || byte.is_ascii_alphanumeric();
    let mut anchor = cursor;
    if anchor == bytes.len() || !bytes.get(anchor).is_some_and(|byte| is_ident(*byte)) {
        if anchor == 0 || !is_ident(bytes[anchor - 1]) {
            return true;
        }
        anchor -= 1;
    }
    let mut start = anchor;
    while start > 0 && is_ident(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = anchor + 1;
    while end < bytes.len() && is_ident(bytes[end]) {
        end += 1;
    }

    let before = line_text[..start].trim_end();
    let previous = before
        .rsplit(|ch: char| !(ch == '_' || ch.is_ascii_alphanumeric()))
        .find(|part| !part.is_empty());
    if matches!(previous, Some("goto" | "struct" | "union" | "enum")) {
        return false;
    }

    !line_text[end..].trim_start().starts_with(':')
}

#[cfg(test)]
mod tests {
    use super::{
        label_definition_byte_range, label_navigation_byte_range,
        ordinary_identifier_navigation_context, LabelNavigation,
    };

    fn marked_source(marked: &str) -> (String, usize) {
        let marker = "/*cursor*/";
        let cursor = marked.find(marker).expect("cursor marker");
        (marked.replacen(marker, "", 1), cursor)
    }

    #[test]
    fn local_binding_dominance_is_limited_to_the_ordinary_namespace() {
        assert!(ordinary_identifier_navigation_context("return value;", 9));
        assert!(!ordinary_identifier_navigation_context(
            "return obj.value;",
            13
        ));
        assert!(!ordinary_identifier_navigation_context("goto value;", 7));
        assert!(!ordinary_identifier_navigation_context(
            "struct value item;",
            9
        ));
        assert!(!ordinary_identifier_navigation_context("value: return;", 2));
    }

    #[test]
    fn goto_resolves_the_label_in_its_own_function() {
        let (text, cursor) = marked_source(
            "void first(void) { same: return; }\n\
             void second(void) { goto sa/*cursor*/me; same: return; }\n",
        );
        let expected = text.rfind("same:").expect("second label");

        assert_eq!(
            label_definition_byte_range("main.c", &text, "same", cursor),
            Some((expected, expected + "same".len()))
        );
    }

    #[test]
    fn label_definition_resolves_to_itself() {
        let (text, cursor) = marked_source("void f(void) { tar/*cursor*/get: return; }\n");
        let expected = text.find("target:").expect("label");

        assert_eq!(
            label_definition_byte_range("main.c", &text, "target", cursor),
            Some((expected, expected + "target".len()))
        );
    }

    #[test]
    fn cpp_label_navigation_uses_the_cpp_grammar() {
        let (text, cursor) = marked_source("void f() { goto do/*cursor*/ne; done: return; }\n");
        let expected = text.rfind("done:").expect("label");

        assert_eq!(
            label_definition_byte_range("main.cpp", &text, "done", cursor),
            Some((expected, expected + "done".len()))
        );
    }

    #[test]
    fn computed_goto_and_non_label_identifier_do_not_trigger_label_navigation() {
        let (computed, computed_cursor) =
            marked_source("void f(void) { goto *tar/*cursor*/get; target: return; }\n");
        assert_eq!(
            label_definition_byte_range("main.c", &computed, "target", computed_cursor),
            None
        );

        let (ordinary, ordinary_cursor) =
            marked_source("int target; void f(void) { tar/*cursor*/get++; }\n");
        assert_eq!(
            label_definition_byte_range("main.c", &ordinary, "target", ordinary_cursor),
            None
        );
    }

    #[test]
    fn goto_does_not_cross_function_boundaries_for_a_missing_label() {
        let (text, cursor) = marked_source(
            "void first(void) { missing: return; }\n\
             void second(void) { goto mis/*cursor*/sing; }\n",
        );

        assert_eq!(
            label_definition_byte_range("main.c", &text, "missing", cursor),
            None
        );
        assert_eq!(
            label_navigation_byte_range("main.c", &text, "missing", cursor),
            LabelNavigation::MissingDefinition
        );
    }

    #[test]
    fn colon_in_an_expression_is_not_mistaken_for_label_syntax() {
        let (text, cursor) =
            marked_source("int f(int flag, int value) { return flag ? val/*cursor*/ue : 0; }\n");

        assert_eq!(
            label_navigation_byte_range("main.c", &text, "value", cursor),
            LabelNavigation::NotLabelSyntax
        );
    }
}
