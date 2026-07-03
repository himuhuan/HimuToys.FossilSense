use super::byte_offset_at;
use super::definitions::rank_definition_records_with_scope;
use crate::model::DefinitionCandidate;
use crate::reachability::ReachScope;
use crate::store::SymbolRecord;

pub const SIGNATURE_HELP_LIMIT: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallContext {
    pub name: String,
    pub active_argument: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParameterSpan {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureParts {
    pub label: String,
    pub parameters: Vec<ParameterSpan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankedSignatureCandidate {
    pub candidate: DefinitionCandidate,
    pub signature: String,
}

pub fn call_context_at(text: &str, line: u32, character: u32) -> Option<CallContext> {
    let offset = byte_offset_at(text, line, character).min(text.len());
    let bytes = text.as_bytes();
    let mut stack = Vec::new();
    let mut state = LexState::Code;
    let mut i = 0usize;

    while i < offset {
        let byte = bytes[i];
        match state {
            LexState::Code => match byte {
                b'/' if i + 1 < offset && bytes[i + 1] == b'/' => {
                    state = LexState::LineComment;
                    i += 2;
                    continue;
                }
                b'/' if i + 1 < offset && bytes[i + 1] == b'*' => {
                    state = LexState::BlockComment;
                    i += 2;
                    continue;
                }
                b'"' => state = LexState::String { escaped: false },
                b'\'' => state = LexState::Char { escaped: false },
                b'(' => {
                    let name = identifier_before(bytes, i).filter(|name| !is_control_keyword(name));
                    stack.push(DelimFrame::Paren {
                        name,
                        active_argument: 0,
                        unsupported_template_argument: false,
                    });
                }
                b')' => pop_matching(&mut stack, FrameKind::Paren),
                b'[' => stack.push(DelimFrame::Bracket),
                b']' => pop_matching(&mut stack, FrameKind::Bracket),
                b'{' => stack.push(DelimFrame::Brace),
                b'}' => pop_matching(&mut stack, FrameKind::Brace),
                b'<' if looks_like_template_start(bytes, i, offset) => {
                    stack.push(DelimFrame::Angle);
                }
                b'>' => pop_matching(&mut stack, FrameKind::Angle),
                b',' => {
                    if stack.iter().any(|frame| matches!(frame, DelimFrame::Angle)) {
                        if let Some(DelimFrame::Paren {
                            name: Some(_),
                            unsupported_template_argument,
                            ..
                        }) = stack
                            .iter_mut()
                            .rev()
                            .find(|frame| matches!(frame, DelimFrame::Paren { name: Some(_), .. }))
                        {
                            *unsupported_template_argument = true;
                        }
                        i += 1;
                        continue;
                    }
                    if let Some(DelimFrame::Paren {
                        name: Some(_),
                        active_argument,
                        ..
                    }) = stack.last_mut()
                    {
                        *active_argument += 1;
                    }
                }
                _ => {}
            },
            LexState::String { escaped } => {
                state = match (escaped, byte) {
                    (true, _) => LexState::String { escaped: false },
                    (false, b'\\') => LexState::String { escaped: true },
                    (false, b'"') => LexState::Code,
                    _ => LexState::String { escaped: false },
                };
            }
            LexState::Char { escaped } => {
                state = match (escaped, byte) {
                    (true, _) => LexState::Char { escaped: false },
                    (false, b'\\') => LexState::Char { escaped: true },
                    (false, b'\'') => LexState::Code,
                    _ => LexState::Char { escaped: false },
                };
            }
            LexState::LineComment => {
                if byte == b'\n' {
                    state = LexState::Code;
                }
            }
            LexState::BlockComment => {
                if byte == b'*' && i + 1 < offset && bytes[i + 1] == b'/' {
                    state = LexState::Code;
                    i += 2;
                    continue;
                }
            }
        }
        i += 1;
    }

    stack.iter().rev().find_map(|frame| match frame {
        DelimFrame::Paren {
            name: Some(name),
            active_argument,
            unsupported_template_argument,
        } => Some(CallContext {
            name: name.clone(),
            active_argument: (!unsupported_template_argument).then_some(*active_argument)?,
        }),
        _ => None,
    })
}

pub fn signature_parts(signature: &str) -> SignatureParts {
    let label = signature
        .trim_end()
        .strip_suffix('{')
        .map(|without_brace| without_brace.trim_end().to_string())
        .unwrap_or_else(|| signature.to_string());
    let Some((open, close)) = parameter_list_bounds(&label) else {
        return SignatureParts {
            label,
            parameters: Vec::new(),
        };
    };
    signature_parts_from_bounds(label, open, close)
}

pub fn signature_parts_for_name(signature: &str, name: &str) -> SignatureParts {
    let label = signature
        .trim_end()
        .strip_suffix('{')
        .map(|without_brace| without_brace.trim_end().to_string())
        .unwrap_or_else(|| signature.to_string());
    let Some((open, close)) = parameter_list_bounds_for_name(&label, name) else {
        return SignatureParts {
            label,
            parameters: Vec::new(),
        };
    };
    signature_parts_from_bounds(label, open, close)
}

fn signature_parts_from_bounds(label: String, open: usize, close: usize) -> SignatureParts {
    let inner = &label[open + 1..close];
    if inner.trim().is_empty() || inner.trim() == "void" {
        return SignatureParts {
            label,
            parameters: Vec::new(),
        };
    }
    let Some(parameters) = split_parameter_spans(&label, open + 1, close) else {
        return SignatureParts {
            label,
            parameters: Vec::new(),
        };
    };
    SignatureParts { label, parameters }
}

pub fn rank_function_signature_candidates(
    records: Vec<SymbolRecord>,
    current_rel_path: &str,
    scope: Option<&ReachScope>,
    limit: usize,
) -> Vec<RankedSignatureCandidate> {
    let functions = records
        .into_iter()
        .filter(|record| record.kind == "function")
        .collect();

    rank_definition_records_with_scope(functions, current_rel_path, scope)
        .into_iter()
        .take(limit)
        .map(|ranked| {
            let signature = ranked.record.signature;
            RankedSignatureCandidate {
                candidate: ranked.candidate,
                signature,
            }
        })
        .collect()
}

fn identifier_before(bytes: &[u8], open_paren: usize) -> Option<String> {
    let mut end = open_paren;
    while end > 0 && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && is_ident_continue(bytes[start - 1]) {
        start -= 1;
    }
    if start == end || !is_ident_start(bytes[start]) {
        return None;
    }
    std::str::from_utf8(&bytes[start..end])
        .ok()
        .map(str::to_string)
}

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_ident_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn is_control_keyword(name: &str) -> bool {
    matches!(
        name,
        "if" | "for" | "while" | "switch" | "return" | "sizeof" | "defined"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LexState {
    Code,
    String { escaped: bool },
    Char { escaped: bool },
    LineComment,
    BlockComment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DelimFrame {
    Paren {
        name: Option<String>,
        active_argument: u32,
        unsupported_template_argument: bool,
    },
    Bracket,
    Brace,
    Angle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    Paren,
    Bracket,
    Brace,
    Angle,
}

fn pop_matching(stack: &mut Vec<DelimFrame>, kind: FrameKind) {
    if stack.last().is_some_and(|frame| frame.kind() == kind) {
        stack.pop();
    }
}

impl DelimFrame {
    fn kind(&self) -> FrameKind {
        match self {
            DelimFrame::Paren { .. } => FrameKind::Paren,
            DelimFrame::Bracket => FrameKind::Bracket,
            DelimFrame::Brace => FrameKind::Brace,
            DelimFrame::Angle => FrameKind::Angle,
        }
    }
}

fn looks_like_template_start(bytes: &[u8], open_angle: usize, limit: usize) -> bool {
    if !template_name_before_angle(bytes, open_angle) {
        return false;
    }
    let mut depth = 0i32;
    for (idx, byte) in bytes.iter().enumerate().take(limit).skip(open_angle) {
        match *byte {
            b'<' => depth += 1,
            b'>' => {
                depth -= 1;
                if depth == 0 {
                    return idx > open_angle + 1;
                }
            }
            b';' | b'\n' | b')' | b']' | b'}' if depth == 1 => return false,
            _ => {}
        }
    }
    false
}

fn template_name_before_angle(bytes: &[u8], open_angle: usize) -> bool {
    let Some(name_start) = identifier_start_before(bytes, open_angle) else {
        return false;
    };
    let angle_touches_name = open_angle > 0 && is_ident_continue(bytes[open_angle - 1]);
    let namespace_qualified =
        name_start >= 2 && bytes.get(name_start - 2..name_start) == Some(b"::");
    angle_touches_name || namespace_qualified
}

fn parameter_list_bounds(label: &str) -> Option<(usize, usize)> {
    let bytes = label.as_bytes();
    let open = bytes
        .iter()
        .position(|byte| *byte == b'(')
        .filter(|open| is_plain_function_name_before(bytes, *open))?;
    matching_close_paren(bytes, open).map(|close| (open, close))
}

fn parameter_list_bounds_for_name(label: &str, name: &str) -> Option<(usize, usize)> {
    if name.is_empty() {
        return None;
    }
    let bytes = label.as_bytes();
    for (idx, _) in label.match_indices(name) {
        let end = idx + name.len();
        let before_boundary = idx == 0 || !is_ident_continue(bytes[idx - 1]);
        let after_boundary = end == bytes.len() || !is_ident_continue(bytes[end]);
        if !before_boundary || !after_boundary || is_function_pointer_declarator_name(bytes, idx) {
            continue;
        }
        let open = skip_ascii_whitespace(bytes, end);
        if bytes.get(open) != Some(&b'(') {
            continue;
        }
        if let Some(close) = matching_close_paren(bytes, open) {
            return Some((open, close));
        }
    }
    None
}

fn matching_close_paren(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (idx, byte) in bytes.iter().enumerate().skip(open) {
        match *byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(idx);
                }
            }
            _ => {}
        }
    }
    None
}

fn is_plain_function_name_before(bytes: &[u8], open_paren: usize) -> bool {
    if open_paren == 0 || !is_ident_continue(bytes[open_paren - 1]) {
        return false;
    }
    let Some(name_start) = identifier_start_before(bytes, open_paren) else {
        return false;
    };
    !is_function_pointer_declarator_name(bytes, name_start)
}

fn identifier_start_before(bytes: &[u8], open_paren: usize) -> Option<usize> {
    let mut end = open_paren;
    while end > 0 && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && is_ident_continue(bytes[start - 1]) {
        start -= 1;
    }
    if start == end || !is_ident_start(bytes[start]) {
        return None;
    }
    Some(start)
}

fn is_function_pointer_declarator_name(bytes: &[u8], name_start: usize) -> bool {
    let Some(pointer_idx) = previous_non_ws_index(bytes, name_start) else {
        return false;
    };
    matches!(bytes[pointer_idx], b'*' | b'&') && previous_non_ws(bytes, pointer_idx) == Some(b'(')
}

fn previous_non_ws(bytes: &[u8], before: usize) -> Option<u8> {
    previous_non_ws_index(bytes, before).map(|idx| bytes[idx])
}

fn previous_non_ws_index(bytes: &[u8], before: usize) -> Option<usize> {
    bytes
        .get(..before)?
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
}

fn skip_ascii_whitespace(bytes: &[u8], mut idx: usize) -> usize {
    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
        idx += 1;
    }
    idx
}

fn split_parameter_spans(label: &str, start: usize, end: usize) -> Option<Vec<ParameterSpan>> {
    let bytes = label.as_bytes();
    let mut spans = Vec::new();
    let mut part_start = start;
    let mut paren = 0i32;
    let mut bracket = 0i32;
    let mut brace = 0i32;
    for (idx, byte) in bytes.iter().enumerate().take(end).skip(start) {
        match *byte {
            b'(' => paren += 1,
            b')' => paren -= 1,
            b'[' => bracket += 1,
            b']' => bracket -= 1,
            b'{' => brace += 1,
            b'}' => brace -= 1,
            b',' if paren == 0 && bracket == 0 && brace == 0 => {
                push_trimmed_span(label, part_start, idx, &mut spans);
                part_start = idx + 1;
            }
            _ => {}
        }
        if paren < 0 || bracket < 0 || brace < 0 {
            return None;
        }
    }
    if paren != 0 || bracket != 0 || brace != 0 {
        return None;
    }
    push_trimmed_span(label, part_start, end, &mut spans);
    Some(spans)
}

fn push_trimmed_span(label: &str, start: usize, end: usize, spans: &mut Vec<ParameterSpan>) {
    let mut s = start;
    let mut e = end;
    let bytes = label.as_bytes();
    while s < e && bytes[s].is_ascii_whitespace() {
        s += 1;
    }
    while e > s && bytes[e - 1].is_ascii_whitespace() {
        e -= 1;
    }
    if s < e {
        spans.push(ParameterSpan {
            start: s as u32,
            end: e as u32,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbol_record(
        name: &str,
        kind: &str,
        role: &str,
        path: &str,
        signature: &str,
    ) -> crate::store::SymbolRecord {
        symbol_record_with_id(0, name, kind, role, path, signature)
    }

    fn symbol_record_with_id(
        id: i64,
        name: &str,
        kind: &str,
        role: &str,
        path: &str,
        signature: &str,
    ) -> crate::store::SymbolRecord {
        crate::store::SymbolRecord {
            id,
            name: name.to_string(),
            kind: kind.to_string(),
            role: role.to_string(),
            path: path.to_string(),
            start_line: 1,
            start_col: 0,
            end_line: 1,
            end_col: 0,
            signature: signature.to_string(),
            guard: None,
            source: "workspace".to_string(),
            directly_included: false,
        }
    }

    #[test]
    fn call_context_after_open_paren_is_first_argument() {
        let text = "int main(void) {\n  foo(\n}\n";
        let ctx = call_context_at(text, 1, 6).expect("call context");
        assert_eq!(ctx.name, "foo");
        assert_eq!(ctx.active_argument, 0);
    }

    #[test]
    fn call_context_counts_only_top_level_commas() {
        let text = "void f(void) {\n  foo(a, bar(b, c), arr[1, 2], \n}\n";
        let ctx = call_context_at(text, 1, 34).expect("call context");
        assert_eq!(ctx.name, "foo");
        assert_eq!(ctx.active_argument, 3);
    }

    #[test]
    fn call_context_uses_nearest_nested_call() {
        let text = "void f(void) {\n  outer(alpha, inner(beta, \n}\n";
        let ctx = call_context_at(text, 1, 28).expect("call context");
        assert_eq!(ctx.name, "inner");
        assert_eq!(ctx.active_argument, 1);
    }

    #[test]
    fn call_context_rejects_control_keywords() {
        let text = "void f(void) {\n  if (ready, \n}\n";
        assert!(call_context_at(text, 1, 13).is_none());
    }

    #[test]
    fn call_context_rejects_template_like_argument_commas() {
        let text = "void f(void) {\n  foo(std::pair<int, int>{}, \n}\n";
        let line = 1;
        let character = text.lines().nth(1).expect("line").chars().count() as u32;
        assert!(
            call_context_at(text, line, character).is_none(),
            "unsupported template-like shapes must degrade to None"
        );
    }

    #[test]
    fn call_context_allows_commas_after_less_than_expression() {
        let text = "void f(void) {\n  if (idx < limit) {}\n  foo(first, \n}\n";
        let line = 2;
        let character = text.lines().nth(2).expect("line").chars().count() as u32;
        let ctx = call_context_at(text, line, character).expect("call context");
        assert_eq!(ctx.name, "foo");
        assert_eq!(ctx.active_argument, 1);
    }

    #[test]
    fn call_context_ignores_commas_inside_string_literals() {
        let text = "void f(void) {\n  log_message(\"a,b\", \n}\n";
        let line = 1;
        let character = text.lines().nth(1).expect("line").chars().count() as u32;
        let ctx = call_context_at(text, line, character).expect("call context");
        assert_eq!(ctx.name, "log_message");
        assert_eq!(ctx.active_argument, 1);
    }

    #[test]
    fn signature_parts_extracts_simple_parameters() {
        let parts = signature_parts("int foo(int a, const char *name)");
        assert_eq!(parts.label, "int foo(int a, const char *name)");
        let labels: Vec<&str> = parts
            .parameters
            .iter()
            .map(|span| &parts.label[span.start as usize..span.end as usize])
            .collect();
        assert_eq!(labels, vec!["int a", "const char *name"]);
    }

    #[test]
    fn signature_parts_keeps_void_parameter_list_empty() {
        let parts = signature_parts("void reset(void)");
        assert!(parts.parameters.is_empty());
    }

    #[test]
    fn signature_parts_ignores_nested_commas() {
        let parts = signature_parts("void visit(int (*cb)(int, int), int flags)");
        let labels: Vec<&str> = parts
            .parameters
            .iter()
            .map(|span| &parts.label[span.start as usize..span.end as usize])
            .collect();
        assert_eq!(labels, vec!["int (*cb)(int, int)", "int flags"]);
    }

    #[test]
    fn signature_parts_do_not_fabricate_parameters_for_function_pointer_return() {
        let parts = signature_parts("int (*factory(void))(int);");
        assert_eq!(parts.label, "int (*factory(void))(int);");
        assert!(parts.parameters.is_empty());
    }

    #[test]
    fn signature_parts_extracts_pointer_return_parameters() {
        let parts = signature_parts_for_name("char *dup_string(const char *s)", "dup_string");
        let labels: Vec<&str> = parts
            .parameters
            .iter()
            .map(|span| &parts.label[span.start as usize..span.end as usize])
            .collect();
        assert_eq!(labels, vec!["const char *s"]);
    }

    #[test]
    fn signature_parts_for_name_skips_prefix_attributes() {
        let parts = signature_parts_for_name("__attribute__((nonnull(1))) int foo(int x)", "foo");
        let labels: Vec<&str> = parts
            .parameters
            .iter()
            .map(|span| &parts.label[span.start as usize..span.end as usize])
            .collect();
        assert_eq!(labels, vec!["int x"]);
    }

    #[test]
    fn malformed_signature_returns_whole_label_without_parameters() {
        let parts = signature_parts("int broken(int a, ");
        assert_eq!(parts.label, "int broken(int a, ");
        assert!(parts.parameters.is_empty());
    }

    #[test]
    fn signature_candidates_keep_only_functions_and_preserve_signature() {
        let records = vec![
            symbol_record(
                "foo",
                "macro",
                "definition",
                "inc/foo.h",
                "#define foo(x) (x)",
            ),
            symbol_record(
                "foo",
                "function",
                "declaration",
                "inc/foo.h",
                "int foo(int x);",
            ),
        ];
        let ranked = rank_function_signature_candidates(records, "src/main.c", None, 10);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].signature, "int foo(int x);");
        assert_eq!(ranked[0].candidate.kind, "function");
    }

    #[test]
    fn signature_candidates_use_reachability_tier_order() {
        let records = vec![
            symbol_record(
                "foo",
                "function",
                "definition",
                "other/foo.c",
                "int foo(float x)",
            ),
            symbol_record(
                "foo",
                "function",
                "declaration",
                "inc/foo.h",
                "int foo(int x);",
            ),
        ];
        let reach = crate::reachability::ReachScope {
            files: ["src/main.c".to_string(), "inc/foo.h".to_string()]
                .into_iter()
                .collect(),
            open: false,
            reason: None,
        };
        let ranked = rank_function_signature_candidates(records, "src/main.c", Some(&reach), 10);
        assert_eq!(ranked[0].candidate.path, "inc/foo.h");
        assert_eq!(ranked[0].candidate.tier, crate::model::ScopeTier::Reachable);
        assert_eq!(ranked[0].signature, "int foo(int x);");
    }

    #[test]
    fn signature_candidates_cap_results_after_ranking() {
        let records = vec![
            symbol_record("foo", "function", "definition", "a.c", "int foo(int a)"),
            symbol_record("foo", "function", "definition", "b.c", "int foo(int b)"),
        ];
        let ranked = rank_function_signature_candidates(records, "main.c", None, 1);
        assert_eq!(ranked.len(), 1);
    }

    #[test]
    fn signature_candidates_preserve_distinct_signatures_for_colliding_location_keys() {
        let records = vec![
            symbol_record_with_id(
                1,
                "foo",
                "function",
                "definition",
                "inc/foo.h",
                "int foo(int x)",
            ),
            symbol_record_with_id(
                2,
                "foo",
                "function",
                "definition",
                "inc/foo.h",
                "int foo(float x)",
            ),
        ];
        let ranked = rank_function_signature_candidates(records, "src/main.c", None, 10);
        let signatures: Vec<&str> = ranked
            .iter()
            .map(|candidate| candidate.signature.as_str())
            .collect();
        assert_eq!(ranked.len(), 2);
        assert_eq!(signatures, vec!["int foo(int x)", "int foo(float x)"]);
    }
}
