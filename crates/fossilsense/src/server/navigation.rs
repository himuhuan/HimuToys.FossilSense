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
        let source_cursor_byte =
            query::byte_offset_at(&text, position.position.line, position.position.character);

        // C and C++ labels inhabit a function-local namespace. Only parse the
        // request document when the bytes around the selected identifier could
        // be `goto name` or `name:`; tree-sitter then proves the exact syntax
        // context and the enclosing function before a local label can dominate
        // workspace candidates with the same spelling.
        if label_navigation_syntax_hint(&text, &word, source_cursor_byte) {
            let label_uri = uri.clone();
            let label_path = current_rel.clone();
            let label_text = text.clone();
            let label_word = word.clone();
            match tokio::task::spawn_blocking(move || {
                label_navigation_location(
                    &label_uri,
                    &label_path,
                    &label_text,
                    &label_word,
                    source_cursor_byte,
                )
            })
            .await
            {
                Ok(LabelNavigation::Found(location)) => {
                    return Ok(Some(GotoDefinitionResponse::Array(vec![location])));
                }
                // A proven `goto name` belongs exclusively to the enclosing
                // function's label namespace.  A missing label must not fall
                // through to an unrelated workspace function/object named
                // `name`.
                Ok(LabelNavigation::MissingDefinition) => return Ok(None),
                Ok(LabelNavigation::NotLabelSyntax) | Err(_) => {}
            }
        }

        // Lexical bindings are proven by C scope rules and dominate every
        // workspace same-name candidate, regardless of recall completeness.
        if ordinary_identifier_navigation_context(line_text, position.position.character) {
            let local_uri = uri.clone();
            let local_path = current_rel.clone();
            let local_text = text.clone();
            let local_word = word.clone();
            let local_position = position.position;
            if let Ok(Some(location)) = tokio::task::spawn_blocking(move || {
                local_binding_location(
                    &local_uri,
                    &local_path,
                    &local_text,
                    &local_word,
                    local_position,
                )
            })
            .await
            {
                return Ok(Some(GotoDefinitionResponse::Array(vec![location])));
            }
        }
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
                    let selected = match operation {
                        NavigationOperation::Definition => {
                            query::call_definition_presentations(&callable_set.groups)
                        }
                        NavigationOperation::Declaration => {
                            query::call_declaration_presentations_at(
                                &callable_set.groups,
                                &current_rel,
                                source_cursor_byte,
                            )
                        }
                    };
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
                let candidates = match operation {
                    NavigationOperation::Definition => {
                        query::rank_navigation_candidates_with_scope(
                            records,
                            &current_rel,
                            service.effective_current_reach(),
                            origin_record.as_ref(),
                            project_context.as_deref(),
                        )
                    }
                    NavigationOperation::Declaration => {
                        query::rank_declaration_candidates_with_scope(
                            records,
                            &current_rel,
                            service.effective_current_reach(),
                        )
                    }
                };
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

pub(super) fn local_binding_location(
    uri: &Url,
    current_path: &str,
    text: &str,
    word: &str,
    position: tower_lsp::lsp_types::Position,
) -> Option<Location> {
    let cursor_byte = query::byte_offset_at(text, position.line, position.character);
    let parsed = parser::parse_with_handle(
        Path::new(current_path),
        text,
        None,
        parser::ParseFacts::LOCAL_DECLS,
    );
    let binding = query::visible_local_binding(&parsed.local_bindings, word, cursor_byte)?;
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
}

pub(super) fn label_navigation_location(
    uri: &Url,
    current_path: &str,
    text: &str,
    word: &str,
    cursor_byte: usize,
) -> LabelNavigation<Location> {
    let (start_byte, end_byte) =
        match label_navigation_byte_range(current_path, text, word, cursor_byte) {
            LabelNavigation::NotLabelSyntax => return LabelNavigation::NotLabelSyntax,
            LabelNavigation::MissingDefinition => return LabelNavigation::MissingDefinition,
            LabelNavigation::Found(range) => range,
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
        LabelNavigation::NotLabelSyntax | LabelNavigation::MissingDefinition => None,
    }
}

fn label_navigation_byte_range(
    current_path: &str,
    text: &str,
    word: &str,
    cursor_byte: usize,
) -> LabelNavigation<(usize, usize)> {
    let Some((query_start, query_end)) = identifier_byte_range_at(text, cursor_byte) else {
        return LabelNavigation::NotLabelSyntax;
    };
    if text.get(query_start..query_end) != Some(word)
        || !label_navigation_syntax_hint_for_range(text, query_start, query_end)
    {
        return LabelNavigation::NotLabelSyntax;
    }

    let language = if is_cpp_navigation_path(current_path) {
        tree_sitter_cpp::LANGUAGE.into()
    } else {
        tree_sitter_c::LANGUAGE.into()
    };
    let parser = parser::ParserHandle::new();
    let Some(tree) = parser
        .parse_with_language(language, text, None)
        .ok()
        .flatten()
    else {
        return LabelNavigation::NotLabelSyntax;
    };
    let Some(context) = label_context_node_at(tree.root_node(), query_start, query_end) else {
        return LabelNavigation::NotLabelSyntax;
    };
    let Some(scope) = enclosing_label_scope(context) else {
        return LabelNavigation::NotLabelSyntax;
    };

    let target = if context.kind() == "labeled_statement" {
        context.child_by_field_name("label")
    } else {
        label_definition_in_scope(scope, text, word)
    };
    target.map_or(LabelNavigation::MissingDefinition, |target| {
        LabelNavigation::Found((target.start_byte(), target.end_byte()))
    })
}

pub(super) fn label_navigation_syntax_hint(text: &str, word: &str, cursor_byte: usize) -> bool {
    let Some((start, end)) = identifier_byte_range_at(text, cursor_byte) else {
        return false;
    };
    text.get(start..end) == Some(word) && label_navigation_syntax_hint_for_range(text, start, end)
}

fn label_navigation_syntax_hint_for_range(text: &str, start: usize, end: usize) -> bool {
    let bytes = text.as_bytes();

    let mut after = end;
    while bytes
        .get(after)
        .is_some_and(|byte| byte.is_ascii_whitespace())
    {
        after += 1;
    }
    if bytes.get(after) == Some(&b':') {
        return true;
    }

    let mut before = start;
    while before > 0 && bytes[before - 1].is_ascii_whitespace() {
        before -= 1;
    }
    let previous_end = before;
    while before > 0 && is_ascii_identifier_byte(bytes[before - 1]) {
        before -= 1;
    }
    text.get(before..previous_end) == Some("goto")
        && (before == 0 || !is_ascii_identifier_byte(bytes[before - 1]))
}

fn identifier_byte_range_at(text: &str, cursor_byte: usize) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut anchor = cursor_byte.min(bytes.len());
    if anchor == bytes.len()
        || !bytes
            .get(anchor)
            .is_some_and(|byte| is_ascii_identifier_byte(*byte))
    {
        if anchor == 0 || !is_ascii_identifier_byte(bytes[anchor - 1]) {
            return None;
        }
        anchor -= 1;
    }

    let mut start = anchor;
    while start > 0 && is_ascii_identifier_byte(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = anchor + 1;
    while end < bytes.len() && is_ascii_identifier_byte(bytes[end]) {
        end += 1;
    }
    (bytes[start].is_ascii_alphabetic() || bytes[start] == b'_').then_some((start, end))
}

fn is_ascii_identifier_byte(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphanumeric()
}

fn is_cpp_navigation_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "cpp" | "hpp" | "cc" | "hh" | "cxx" | "hxx"
            )
        })
}

fn label_context_node_at<'tree>(
    root: tree_sitter::Node<'tree>,
    query_start: usize,
    query_end: usize,
) -> Option<tree_sitter::Node<'tree>> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.start_byte() > query_start || node.end_byte() < query_end {
            continue;
        }
        if matches!(node.kind(), "goto_statement" | "labeled_statement") {
            if let Some(label) = node.child_by_field_name("label") {
                if label.kind() == "statement_identifier"
                    && label.start_byte() == query_start
                    && label.end_byte() == query_end
                {
                    return Some(node);
                }
            }
        }
        for index in (0..node.named_child_count()).rev() {
            if let Some(child) = node.named_child(index) {
                stack.push(child);
            }
        }
    }
    None
}

fn enclosing_label_scope(context: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    let mut current = context.parent();
    while let Some(node) = current {
        if is_label_scope_node(node) {
            return Some(node);
        }
        current = node.parent();
    }
    None
}

fn label_definition_in_scope<'tree>(
    scope: tree_sitter::Node<'tree>,
    text: &str,
    word: &str,
) -> Option<tree_sitter::Node<'tree>> {
    let mut stack = vec![scope];
    while let Some(node) = stack.pop() {
        if node.id() != scope.id() && is_label_scope_node(node) {
            continue;
        }
        if node.kind() == "labeled_statement" {
            if let Some(label) = node.child_by_field_name("label") {
                if label.kind() == "statement_identifier"
                    && text.get(label.start_byte()..label.end_byte()) == Some(word)
                {
                    return Some(label);
                }
            }
        }
        for index in (0..node.named_child_count()).rev() {
            if let Some(child) = node.named_child(index) {
                stack.push(child);
            }
        }
    }
    None
}

fn is_label_scope_node(node: tree_sitter::Node<'_>) -> bool {
    matches!(node.kind(), "function_definition" | "lambda_expression")
}

fn source_position_for_byte(text: &str, byte: usize) -> tower_lsp::lsp_types::Position {
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
