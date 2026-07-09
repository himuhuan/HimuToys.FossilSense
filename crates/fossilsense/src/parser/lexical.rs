use super::{Include, Symbol, SymbolKind, SymbolRole};

mod capture;
mod scanner;

use capture::{capture_include, capture_macro, capture_statement_symbols};
use scanner::{
    code_brace_delta, current_guard, line_continues_preprocessor, strip_leading_comments,
    update_guard_stack, BraceScanState, PendingStatement,
};

/// Line-based lexical extraction of top-level symbols and `#include` lines. Pure
/// string scanning — no tree-sitter — so it cannot fail and is the basis of the
/// lexical-fallback path when tree-sitter yields no usable tree.
pub(super) fn extract_symbols_and_includes(
    source: &str,
    line_starts: &[usize],
) -> (Vec<Symbol>, Vec<Include>) {
    let mut symbols = Vec::new();
    let mut includes = Vec::new();
    let mut guard_stack = Vec::new();
    let mut brace_depth = 0isize;
    let mut brace_state = BraceScanState::default();
    let mut statement = PendingStatement::default();
    let mut in_leading_block_comment = false;
    let mut preprocessor_continuation = false;

    for (line_index, line) in source.lines().enumerate() {
        let line = strip_leading_comments(line, &mut in_leading_block_comment);
        let trimmed = line.trim();
        let starts_preprocessor = trimmed.starts_with('#');
        let preprocessor_line = preprocessor_continuation || starts_preprocessor;
        let directive_start = starts_preprocessor && !preprocessor_continuation;
        let top_level = brace_depth == 0;
        let line_brace_delta = if preprocessor_line {
            0
        } else {
            code_brace_delta(&line, &mut brace_state)
        };

        if directive_start {
            if let Some(include) = capture_include(trimmed, line_index) {
                includes.push(include);
            }
        }

        if directive_start {
            if let Some(symbol) = capture_macro(
                &line,
                line_index,
                line_starts,
                source,
                current_guard(&guard_stack),
            ) {
                symbols.push(symbol);
            }
        }

        if (statement.active || top_level) && !preprocessor_line && !trimmed.is_empty() {
            statement.push(&line, line_index, line_brace_delta);
            if statement.is_complete() {
                symbols.extend(capture_statement_symbols(
                    &statement,
                    line_starts,
                    source,
                    current_guard(&guard_stack),
                ));
                statement.clear();
            }
        } else if !top_level && !statement.active {
            statement.clear();
        }

        if directive_start {
            update_guard_stack(trimmed, &mut guard_stack);
        }
        brace_depth += line_brace_delta;
        if brace_depth < 0 {
            brace_depth = 0;
        }
        preprocessor_continuation = preprocessor_line && line_continues_preprocessor(&line);
    }

    (symbols, includes)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn make_symbol(
    name: &str,
    kind: SymbolKind,
    role: SymbolRole,
    start_line: usize,
    end_line: usize,
    line_starts: &[usize],
    source: &str,
    signature: String,
    guard: Option<String>,
) -> Symbol {
    let start_byte = line_starts.get(start_line).copied().unwrap_or(0);
    let end_byte = line_end_byte(source, line_starts, end_line);
    Symbol {
        name: name.to_string(),
        kind,
        role,
        start_byte,
        end_byte,
        start_line,
        start_col: 0,
        end_line,
        end_col: end_byte.saturating_sub(line_starts.get(end_line).copied().unwrap_or(end_byte)),
        signature,
        guard,
        container: None,
    }
}

fn line_end_byte(source: &str, line_starts: &[usize], line: usize) -> usize {
    line_starts
        .get(line + 1)
        .copied()
        .map(|next| next.saturating_sub(1))
        .unwrap_or(source.len())
}

pub(super) fn compact_whitespace(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut in_space = false;
    let mut started = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if started && !in_space {
                result.push(' ');
                in_space = true;
            }
        } else {
            result.push(ch);
            in_space = false;
            started = true;
        }
    }
    // Trim trailing space: if the last push was a space, remove it.
    if result.ends_with(' ') {
        result.truncate(result.len() - 1);
    }
    result
}
