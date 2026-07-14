use super::byte_offset_at;
use super::callables::ArgumentState;
use crate::call_model::CallForm;
use crate::model::DefinitionCandidate;

pub const SIGNATURE_HELP_LIMIT: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallContext {
    pub name: String,
    pub qualified_name: Option<String>,
    pub form: CallForm,
    pub active_argument: u32,
    pub argument_state: ArgumentState,
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
            LexState::Code => {
                match byte {
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
                    b'"' => {
                        mark_argument_token(&mut stack);
                        state = LexState::String { escaped: false };
                    }
                    b'\'' => {
                        mark_argument_token(&mut stack);
                        state = LexState::Char { escaped: false };
                    }
                    b'(' => {
                        mark_argument_token(&mut stack);
                        let target = partial_call_target_before(bytes, i)
                            .filter(|target| !is_control_keyword(&target.name));
                        stack.push(DelimFrame::Paren {
                            name: target.as_ref().map(|target| target.name.clone()),
                            qualified_name: target
                                .as_ref()
                                .and_then(|target| target.qualified_name.clone()),
                            form: target.map_or(CallForm::Unsupported, |target| target.form),
                            active_argument: 0,
                            current_argument_has_token: false,
                            unsupported_template_argument: false,
                        });
                    }
                    b')' => pop_matching(&mut stack, FrameKind::Paren),
                    b'[' => {
                        mark_argument_token(&mut stack);
                        stack.push(DelimFrame::Bracket);
                    }
                    b']' => pop_matching(&mut stack, FrameKind::Bracket),
                    b'{' => {
                        mark_argument_token(&mut stack);
                        stack.push(DelimFrame::Brace);
                    }
                    b'}' => pop_matching(&mut stack, FrameKind::Brace),
                    b'<' if looks_like_template_start(bytes, i, offset) => {
                        mark_argument_token(&mut stack);
                        stack.push(DelimFrame::Angle);
                    }
                    b'>' => pop_matching(&mut stack, FrameKind::Angle),
                    b',' => {
                        if stack.iter().any(|frame| matches!(frame, DelimFrame::Angle)) {
                            if let Some(DelimFrame::Paren {
                                name: Some(_),
                                unsupported_template_argument,
                                ..
                            }) = stack.iter_mut().rev().find(|frame| {
                                matches!(frame, DelimFrame::Paren { name: Some(_), .. })
                            }) {
                                *unsupported_template_argument = true;
                            }
                            i += 1;
                            continue;
                        }
                        if let Some(DelimFrame::Paren {
                            name: Some(_),
                            active_argument,
                            current_argument_has_token,
                            ..
                        }) = stack.last_mut()
                        {
                            *active_argument += 1;
                            *current_argument_has_token = false;
                        }
                    }
                    byte if !byte.is_ascii_whitespace() => mark_argument_token(&mut stack),
                    _ => {}
                }
            }
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
            qualified_name,
            form,
            active_argument,
            current_argument_has_token,
            unsupported_template_argument,
        } => Some(CallContext {
            name: name.clone(),
            qualified_name: qualified_name.clone(),
            form: *form,
            active_argument: *active_argument,
            argument_state: if *unsupported_template_argument {
                ArgumentState::Unknown
            } else {
                ArgumentState::Partial {
                    minimum_arity: active_argument + u32::from(*current_argument_has_token),
                    active_argument: *active_argument,
                }
            },
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct PartialCallTarget {
    name: String,
    qualified_name: Option<String>,
    form: CallForm,
}

fn partial_call_target_before(bytes: &[u8], open_paren: usize) -> Option<PartialCallTarget> {
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
    let name = std::str::from_utf8(&bytes[start..end]).ok()?.to_string();
    let previous = previous_non_ws_index(bytes, start);
    if previous.is_some_and(|index| bytes[index] == b'.') {
        return Some(PartialCallTarget {
            name,
            qualified_name: None,
            form: CallForm::MemberDot,
        });
    }
    if let Some(arrow_end) = previous.filter(|index| bytes[*index] == b'>') {
        if previous_non_ws_index(bytes, arrow_end).is_some_and(|index| bytes[index] == b'-') {
            return Some(PartialCallTarget {
                name,
                qualified_name: None,
                form: CallForm::MemberArrow,
            });
        }
    }

    let mut qualified_start = start;
    let mut cursor = start;
    let mut has_qualifier = false;
    loop {
        let Some(second_colon) =
            previous_non_ws_index(bytes, cursor).filter(|index| bytes[*index] == b':')
        else {
            break;
        };
        let Some(first_colon) =
            previous_non_ws_index(bytes, second_colon).filter(|index| bytes[*index] == b':')
        else {
            break;
        };
        let mut owner_end = first_colon;
        while owner_end > 0 && bytes[owner_end - 1].is_ascii_whitespace() {
            owner_end -= 1;
        }
        let mut owner_start = owner_end;
        while owner_start > 0 && is_ident_continue(bytes[owner_start - 1]) {
            owner_start -= 1;
        }
        if owner_start == owner_end || !is_ident_start(bytes[owner_start]) {
            break;
        }
        qualified_start = owner_start;
        cursor = owner_start;
        has_qualifier = true;
    }
    if has_qualifier {
        let qualified_name = std::str::from_utf8(&bytes[qualified_start..end])
            .ok()?
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();
        Some(PartialCallTarget {
            name,
            qualified_name: Some(qualified_name),
            form: CallForm::QualifiedName,
        })
    } else {
        Some(PartialCallTarget {
            name,
            qualified_name: None,
            form: CallForm::DirectName,
        })
    }
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
        qualified_name: Option<String>,
        form: CallForm,
        active_argument: u32,
        current_argument_has_token: bool,
        unsupported_template_argument: bool,
    },
    Bracket,
    Brace,
    Angle,
}

fn mark_argument_token(stack: &mut [DelimFrame]) {
    if let Some(DelimFrame::Paren {
        name: Some(_),
        current_argument_has_token,
        ..
    }) = stack
        .iter_mut()
        .rev()
        .find(|frame| matches!(frame, DelimFrame::Paren { name: Some(_), .. }))
    {
        *current_argument_has_token = true;
    }
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

    #[test]
    fn call_context_after_open_paren_is_first_argument() {
        let text = "int main(void) {\n  foo(\n}\n";
        let ctx = call_context_at(text, 1, 6).expect("call context");
        assert_eq!(ctx.name, "foo");
        assert_eq!(ctx.active_argument, 0);
        assert_eq!(
            ctx.argument_state,
            ArgumentState::Partial {
                minimum_arity: 0,
                active_argument: 0,
            }
        );
    }

    #[test]
    fn call_context_distinguishes_empty_and_started_arguments() {
        let empty = call_context_at("foo(", 0, 4).expect("empty call");
        let first = call_context_at("foo(value", 0, 9).expect("first argument");
        let second_empty = call_context_at("foo(value, ", 0, 11).expect("second argument");
        let second = call_context_at("foo(value, next", 0, 15).expect("second value");
        assert_eq!(
            empty.argument_state,
            ArgumentState::Partial {
                minimum_arity: 0,
                active_argument: 0,
            }
        );
        assert_eq!(
            first.argument_state,
            ArgumentState::Partial {
                minimum_arity: 1,
                active_argument: 0,
            }
        );
        assert_eq!(
            second_empty.argument_state,
            ArgumentState::Partial {
                minimum_arity: 1,
                active_argument: 1,
            }
        );
        assert_eq!(
            second.argument_state,
            ArgumentState::Partial {
                minimum_arity: 2,
                active_argument: 1,
            }
        );
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
    fn call_context_keeps_template_like_argument_commas_as_unknown() {
        let text = "void f(void) {\n  foo(std::pair<int, int>{}, \n}\n";
        let line = 1;
        let character = text.lines().nth(1).expect("line").chars().count() as u32;
        let context = call_context_at(text, line, character).expect("best-effort call context");
        assert_eq!(context.name, "foo");
        assert_eq!(context.active_argument, 1);
        assert_eq!(context.argument_state, ArgumentState::Unknown);
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
    fn call_context_keeps_member_forms_unsupported_and_normalizes_qualified_names() {
        let dot = call_context_at("obj.foo(", 0, 8).expect("dot member call");
        assert_eq!(dot.name, "foo");
        assert_eq!(dot.form, CallForm::MemberDot);
        assert_eq!(dot.qualified_name, None);

        let arrow = call_context_at("obj -> foo(", 0, 11).expect("arrow member call");
        assert_eq!(arrow.form, CallForm::MemberArrow);

        let qualified = call_context_at("net :: io :: open(", 0, 18).expect("qualified call");
        assert_eq!(qualified.form, CallForm::QualifiedName);
        assert_eq!(qualified.qualified_name.as_deref(), Some("net::io::open"));
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
}
