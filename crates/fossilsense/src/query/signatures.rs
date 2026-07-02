use std::collections::HashMap;

use super::byte_offset_at;
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
    let mut i = offset;
    let mut paren_depth = 0i32;
    let mut bracket_depth = 0i32;
    let mut brace_depth = 0i32;
    let mut angle_depth = 0i32;
    let mut active_argument = 0u32;

    while i > 0 {
        i -= 1;
        match bytes[i] {
            b')' => paren_depth += 1,
            b'(' if paren_depth > 0 => paren_depth -= 1,
            b']' => bracket_depth += 1,
            b'[' if bracket_depth > 0 => bracket_depth -= 1,
            b'}' => brace_depth += 1,
            b'{' if brace_depth > 0 => brace_depth -= 1,
            b'>' => angle_depth += 1,
            b'<' if angle_depth > 0 => angle_depth -= 1,
            b',' if paren_depth == 0 && bracket_depth == 0 && brace_depth == 0 => {
                if angle_depth > 0 {
                    return None;
                }
                active_argument += 1;
            }
            b'(' if bracket_depth == 0 && brace_depth == 0 => {
                let name = identifier_before(bytes, i)?;
                if is_control_keyword(&name) {
                    return None;
                }
                return Some(CallContext {
                    name,
                    active_argument,
                });
            }
            _ => {}
        }
    }
    None
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
    let functions: Vec<SymbolRecord> = records
        .into_iter()
        .filter(|record| record.kind == "function")
        .collect();
    let signatures: HashMap<(String, u32, u32, String), String> = functions
        .iter()
        .map(|record| {
            (
                (
                    record.path.clone(),
                    record.start_line,
                    record.start_col,
                    record.role.clone(),
                ),
                record.signature.clone(),
            )
        })
        .collect();

    crate::query::rank_definitions_into_candidates_with_scope(functions, current_rel_path, scope)
        .into_iter()
        .filter_map(|candidate| {
            let key = (
                candidate.path.clone(),
                candidate.range.start_line,
                candidate.range.start_col,
                candidate.role.clone(),
            );
            signatures
                .get(&key)
                .cloned()
                .map(|signature| RankedSignatureCandidate {
                    candidate,
                    signature,
                })
        })
        .take(limit)
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
    std::str::from_utf8(&bytes[start..end]).ok().map(str::to_string)
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

fn parameter_list_bounds(label: &str) -> Option<(usize, usize)> {
    let bytes = label.as_bytes();
    let open = bytes.iter().position(|byte| *byte == b'(')?;
    let mut depth = 0i32;
    for (idx, byte) in bytes.iter().enumerate().skip(open) {
        match *byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((open, idx));
                }
            }
            _ => {}
        }
    }
    None
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
        crate::store::SymbolRecord {
            id: 0,
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
    fn malformed_signature_returns_whole_label_without_parameters() {
        let parts = signature_parts("int broken(int a, ");
        assert_eq!(parts.label, "int broken(int a, ");
        assert!(parts.parameters.is_empty());
    }

    #[test]
    fn signature_candidates_keep_only_functions_and_preserve_signature() {
        let records = vec![
            symbol_record("foo", "macro", "definition", "inc/foo.h", "#define foo(x) (x)"),
            symbol_record("foo", "function", "declaration", "inc/foo.h", "int foo(int x);"),
        ];
        let ranked = rank_function_signature_candidates(records, "src/main.c", None, 10);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].signature, "int foo(int x);");
        assert_eq!(ranked[0].candidate.kind, "function");
    }

    #[test]
    fn signature_candidates_use_reachability_tier_order() {
        let records = vec![
            symbol_record("foo", "function", "definition", "other/foo.c", "int foo(float x)"),
            symbol_record("foo", "function", "declaration", "inc/foo.h", "int foo(int x);"),
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
}
