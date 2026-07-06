use std::sync::OnceLock;

use regex::Regex;

use super::{Include, Symbol, SymbolKind, SymbolRole};

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
    let mut statement = PendingStatement::default();
    let mut in_leading_block_comment = false;

    for (line_index, line) in source.lines().enumerate() {
        let line = strip_leading_comments(line, &mut in_leading_block_comment);
        let trimmed = line.trim();
        let top_level = brace_depth == 0;

        if let Some(include) = capture_include(trimmed, line_index) {
            includes.push(include);
        }

        if let Some(symbol) = capture_macro(
            &line,
            line_index,
            line_starts,
            source,
            current_guard(&guard_stack),
        ) {
            symbols.push(symbol);
        }

        if (statement.active || top_level) && !trimmed.starts_with('#') && !trimmed.is_empty() {
            statement.push(&line, line_index);
            if statement.is_complete() {
                symbols.extend(capture_statement_symbols(
                    &statement,
                    line_starts,
                    source,
                    current_guard(&guard_stack),
                ));
                statement.clear();
            }
        } else if !top_level {
            statement.clear();
        }

        update_guard_stack(trimmed, &mut guard_stack);
        brace_depth += brace_delta(&line);
        if brace_depth < 0 {
            brace_depth = 0;
        }
    }

    (symbols, includes)
}

fn strip_leading_comments(line: &str, in_block_comment: &mut bool) -> String {
    let mut rest = line;
    loop {
        let trimmed = rest.trim_start();
        if *in_block_comment {
            if let Some(end) = trimmed.find("*/") {
                *in_block_comment = false;
                rest = &trimmed[end + 2..];
                continue;
            }
            return String::new();
        }
        if trimmed.starts_with("//") {
            return String::new();
        }
        if trimmed.starts_with("/*") {
            if let Some(end) = trimmed.find("*/") {
                rest = &trimmed[end + 2..];
                continue;
            }
            *in_block_comment = true;
            return String::new();
        }
        return rest.to_string();
    }
}

fn capture_include(trimmed: &str, line: usize) -> Option<Include> {
    include_regex().captures(trimmed).map(|captures| Include {
        line,
        target_text: captures
            .get(1)
            .expect("include target")
            .as_str()
            .trim()
            .to_string(),
    })
}

fn capture_macro(
    line: &str,
    line_index: usize,
    line_starts: &[usize],
    source: &str,
    guard: Option<String>,
) -> Option<Symbol> {
    let captures = macro_regex().captures(line.trim())?;
    let name = captures.get(1)?.as_str();
    Some(make_symbol(
        name,
        SymbolKind::Macro,
        SymbolRole::Definition,
        line_index,
        line_index,
        line_starts,
        source,
        line.trim().to_string(),
        guard,
    ))
}

fn capture_statement_symbols(
    statement: &PendingStatement,
    line_starts: &[usize],
    source: &str,
    guard: Option<String>,
) -> Vec<Symbol> {
    let compact = compact_whitespace(&statement.text);
    let mut symbols = Vec::new();

    if let Some(symbol) = capture_function(statement, &compact, line_starts, source, guard.clone())
    {
        symbols.push(symbol);
        return symbols;
    }

    if let Some(symbol) = capture_typedef(statement, &compact, line_starts, source, guard.clone()) {
        symbols.push(symbol);
    }

    symbols.extend(capture_tag_types(
        statement,
        &compact,
        line_starts,
        source,
        guard.clone(),
    ));

    // Enum constants are extracted from the AST (`collect_enum_constants`), which
    // handles multi-line enums the line-based pass cannot.

    if let Some(symbol) = capture_global_variable(statement, &compact, line_starts, source, guard) {
        symbols.push(symbol);
    }

    symbols
}

fn capture_function(
    statement: &PendingStatement,
    compact: &str,
    line_starts: &[usize],
    source: &str,
    guard: Option<String>,
) -> Option<Symbol> {
    let captures = function_regex().captures(compact)?;
    let name = captures.get(1)?.as_str();

    if matches!(
        name,
        "if" | "for" | "while" | "switch" | "return" | "sizeof" | "defined"
    ) {
        return None;
    }

    let role = if compact.contains('{') && !compact.ends_with(';') {
        SymbolRole::Definition
    } else {
        SymbolRole::Declaration
    };

    Some(make_symbol(
        name,
        SymbolKind::Function,
        role,
        statement.start_line,
        statement.end_line,
        line_starts,
        source,
        trim_open_brace(compact).to_string(),
        guard,
    ))
}

fn capture_typedef(
    statement: &PendingStatement,
    compact: &str,
    line_starts: &[usize],
    source: &str,
    guard: Option<String>,
) -> Option<Symbol> {
    let captures = typedef_regex().captures(compact)?;
    let name = captures.get(1)?.as_str();
    Some(make_symbol(
        name,
        SymbolKind::Type,
        SymbolRole::Definition,
        statement.start_line,
        statement.end_line,
        line_starts,
        source,
        compact.to_string(),
        guard,
    ))
}

fn capture_tag_types(
    statement: &PendingStatement,
    compact: &str,
    line_starts: &[usize],
    source: &str,
    guard: Option<String>,
) -> Vec<Symbol> {
    tag_type_regex()
        .captures_iter(compact)
        .filter_map(|captures| captures.get(2).map(|name| name.as_str()))
        .map(|name| {
            make_symbol(
                name,
                SymbolKind::Type,
                SymbolRole::Definition,
                statement.start_line,
                statement.end_line,
                line_starts,
                source,
                compact.to_string(),
                guard.clone(),
            )
        })
        .collect()
}

fn capture_global_variable(
    statement: &PendingStatement,
    compact: &str,
    line_starts: &[usize],
    source: &str,
    guard: Option<String>,
) -> Option<Symbol> {
    if compact.contains('(')
        || compact.starts_with("typedef ")
        || compact.starts_with("struct ")
        || compact.starts_with("union ")
        || compact.starts_with("enum ")
        || !compact.ends_with(';')
    {
        return None;
    }

    let captures = global_var_regex().captures(compact)?;
    let name = captures.get(1)?.as_str();
    Some(make_symbol(
        name,
        SymbolKind::GlobalVariable,
        SymbolRole::Definition,
        statement.start_line,
        statement.end_line,
        line_starts,
        source,
        compact.to_string(),
        guard,
    ))
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

fn update_guard_stack(trimmed: &str, guard_stack: &mut Vec<String>) {
    if trimmed.starts_with("#if ")
        || trimmed.starts_with("#ifdef ")
        || trimmed.starts_with("#ifndef ")
    {
        guard_stack.push(trimmed.to_string());
    } else if trimmed.starts_with("#elif ") || trimmed.starts_with("#else") {
        if let Some(last) = guard_stack.last_mut() {
            *last = trimmed.to_string();
        }
    } else if trimmed.starts_with("#endif") {
        guard_stack.pop();
    }
}

fn current_guard(guard_stack: &[String]) -> Option<String> {
    if guard_stack.is_empty() {
        None
    } else {
        Some(guard_stack.join(" && "))
    }
}

fn brace_delta(line: &str) -> isize {
    let mut delta = 0;
    for byte in line.bytes() {
        match byte {
            b'{' => delta += 1,
            b'}' => delta -= 1,
            _ => {}
        }
    }
    delta
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

fn trim_open_brace(text: &str) -> &str {
    text.trim_end_matches('{').trim_end()
}

#[derive(Debug, Default)]
struct PendingStatement {
    text: String,
    start_line: usize,
    end_line: usize,
    active: bool,
    brace_balance: isize,
}

impl PendingStatement {
    fn push(&mut self, line: &str, line_index: usize) {
        if !self.active {
            self.start_line = line_index;
            self.active = true;
        }
        self.end_line = line_index;
        self.text.push_str(line);
        self.text.push('\n');
        self.brace_balance += brace_delta(line);
    }

    fn is_complete(&self) -> bool {
        let trimmed = self.text.trim_end();
        if trimmed.ends_with(';') && self.brace_balance <= 0 {
            return true;
        }
        if trimmed.ends_with('{') {
            return !looks_like_record_body_declaration(trimmed);
        }
        if trimmed.ends_with('}') && self.brace_balance <= 0 {
            return true;
        }
        false
    }

    fn clear(&mut self) {
        self.text.clear();
        self.active = false;
        self.brace_balance = 0;
    }
}

fn looks_like_record_body_declaration(text: &str) -> bool {
    let compact = compact_whitespace(text);
    if !compact.ends_with('{') {
        return false;
    }
    let prefix = compact.trim_end_matches('{').trim_end();
    prefix.starts_with("typedef struct ")
        || prefix == "typedef struct"
        || prefix.starts_with("typedef union ")
        || prefix == "typedef union"
        || prefix.starts_with("typedef enum ")
        || prefix == "typedef enum"
        || prefix.starts_with("typedef class ")
        || prefix == "typedef class"
        || prefix.starts_with("struct ")
        || prefix == "struct"
        || prefix.starts_with("union ")
        || prefix == "union"
        || prefix.starts_with("enum ")
        || prefix == "enum"
        || prefix.starts_with("class ")
        || prefix == "class"
}

fn include_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r#"^#\s*include\s+(.+)$"#).expect("include regex"))
}

fn macro_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r#"^#\s*define\s+([A-Za-z_][A-Za-z0-9_]*)"#).expect("macro regex")
    })
}

fn function_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r#"([A-Za-z_][A-Za-z0-9_]*)\s*\([^;{}]*\)\s*(?:;|\{|\{.*\})$"#)
            .expect("function regex")
    })
}

fn typedef_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r#"\btypedef\b.*\b([A-Za-z_][A-Za-z0-9_]*)\s*;"#).expect("typedef regex")
    })
}

fn tag_type_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r#"\b(struct|union|enum|class)\s+([A-Za-z_][A-Za-z0-9_]*)"#)
            .expect("tag type regex")
    })
}

fn global_var_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r#"([A-Za-z_][A-Za-z0-9_]*)\s*(?:\[[^\]]*\])?\s*(?:=[^;]*)?;"#)
            .expect("global variable regex")
    })
}
