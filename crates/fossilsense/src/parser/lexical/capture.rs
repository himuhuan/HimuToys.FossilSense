use std::sync::OnceLock;

use regex::Regex;

use crate::parser::{Include, Symbol, SymbolKind, SymbolRole};

use super::scanner::PendingStatement;
use super::{compact_whitespace, make_symbol};

pub(super) fn capture_include(trimmed: &str, line: usize) -> Option<Include> {
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

pub(super) fn capture_macro(
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

pub(super) fn capture_statement_symbols(
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

    symbols.extend(capture_typedefs(
        statement,
        &compact,
        line_starts,
        source,
        guard.clone(),
    ));

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

fn capture_typedefs(
    statement: &PendingStatement,
    compact: &str,
    line_starts: &[usize],
    source: &str,
    guard: Option<String>,
) -> Vec<Symbol> {
    if !compact.starts_with("typedef ") && compact != "typedef" {
        return Vec::new();
    }

    let mut names = record_typedef_aliases(&statement.text);
    if names.is_empty() {
        if let Some(captures) = typedef_regex().captures(compact) {
            if let Some(name) = captures.get(1) {
                names.push(name.as_str().to_string());
            }
        }
    }

    names.sort();
    names.dedup();
    names
        .into_iter()
        .map(|name| {
            make_symbol(
                &name,
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

fn trim_open_brace(text: &str) -> &str {
    text.trim_end_matches('{').trim_end()
}

fn record_typedef_aliases(text: &str) -> Vec<String> {
    if !starts_with_typedef_keyword(text) {
        return Vec::new();
    }
    let chars: Vec<char> = text.chars().collect();
    let Some(open) = first_code_char(&chars, '{') else {
        return Vec::new();
    };
    let Some(close) = matching_code_delimiter(&chars, open, '{', '}') else {
        return Vec::new();
    };
    let Some(semi) = first_code_char_from(&chars, ';', close + 1) else {
        return Vec::new();
    };
    if !looks_like_record_typedef_prefix(&chars[..open]) {
        return Vec::new();
    }

    split_top_level_declarators(&chars[close + 1..semi])
        .into_iter()
        .filter_map(|segment| declarator_alias_name(&segment))
        .collect()
}

fn starts_with_typedef_keyword(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed == "typedef"
        || trimmed
            .strip_prefix("typedef")
            .is_some_and(|rest| rest.starts_with(char::is_whitespace))
}

fn looks_like_record_typedef_prefix(chars: &[char]) -> bool {
    let text: String = chars.iter().collect();
    record_keyword_regex().is_match(&text)
}

fn first_code_char(chars: &[char], needle: char) -> Option<usize> {
    first_code_char_from(chars, needle, 0)
}

fn first_code_char_from(chars: &[char], needle: char, start: usize) -> Option<usize> {
    let mut i = start;
    let mut in_block_comment = false;
    while i < chars.len() {
        if in_block_comment {
            if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                in_block_comment = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if chars[i] == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
            in_block_comment = true;
            i += 2;
            continue;
        }
        if chars[i] == '"' || chars[i] == '\'' {
            i = skip_quoted_chars(chars, i);
            continue;
        }
        if chars[i] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn matching_code_delimiter(
    chars: &[char],
    open_pos: usize,
    open: char,
    close: char,
) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open_pos;
    let mut in_block_comment = false;
    while i < chars.len() {
        if in_block_comment {
            if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                in_block_comment = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if chars[i] == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
            in_block_comment = true;
            i += 2;
            continue;
        }
        if chars[i] == '"' || chars[i] == '\'' {
            i = skip_quoted_chars(chars, i);
            continue;
        }
        if chars[i] == open {
            depth += 1;
        } else if chars[i] == close {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

fn split_top_level_declarators(chars: &[char]) -> Vec<Vec<char>> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    let mut paren = 0usize;
    let mut bracket = 0usize;
    while i < chars.len() {
        match chars[i] {
            '"' | '\'' => i = skip_quoted_chars(chars, i),
            '(' => {
                paren += 1;
                i += 1;
            }
            ')' => {
                paren = paren.saturating_sub(1);
                i += 1;
            }
            '[' => {
                bracket += 1;
                i += 1;
            }
            ']' => {
                bracket = bracket.saturating_sub(1);
                i += 1;
            }
            ',' if paren == 0 && bracket == 0 => {
                parts.push(chars[start..i].to_vec());
                start = i + 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    if start < chars.len() {
        parts.push(chars[start..].to_vec());
    }
    parts
}

fn declarator_alias_name(segment: &[char]) -> Option<String> {
    let mut chars = strip_known_attributes(segment);
    trim_char_vec(&mut chars);

    loop {
        trim_char_vec(&mut chars);
        let last = chars.last().copied()?;
        if last == ']' {
            let open = matching_open_delimiter(&chars, chars.len() - 1, '[', ']')?;
            chars.truncate(open);
            continue;
        }
        if last == ')' {
            let open = matching_open_delimiter(&chars, chars.len() - 1, '(', ')')?;
            if open == 0 {
                break;
            }
            chars.truncate(open);
            continue;
        }
        break;
    }

    let text: String = chars.iter().collect();
    identifier_regex()
        .find_iter(&text)
        .map(|m| m.as_str())
        .filter(|ident| !TYPEDEF_DECLARATOR_SKIP_WORDS.contains(ident))
        .last()
        .map(str::to_string)
}

fn strip_known_attributes(chars: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(chars.len());
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '[' && chars.get(i + 1) == Some(&'[') {
            if let Some(close) = find_double_bracket_close(chars, i + 2) {
                i = close + 2;
                continue;
            }
        }

        if is_ident_start(chars[i]) {
            let start = i;
            i += 1;
            while i < chars.len() && is_ident_continue(chars[i]) {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            if matches!(
                word.as_str(),
                "__attribute__" | "__declspec" | "_Alignas" | "alignas"
            ) {
                let mut j = i;
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                if chars.get(j) == Some(&'(') {
                    if let Some(close) = matching_code_delimiter(chars, j, '(', ')') {
                        i = close + 1;
                        continue;
                    }
                }
            }
            out.extend(chars[start..i].iter());
            continue;
        }

        out.push(chars[i]);
        i += 1;
    }
    out
}

fn find_double_bracket_close(chars: &[char], start: usize) -> Option<usize> {
    let mut i = start;
    while i + 1 < chars.len() {
        if chars[i] == ']' && chars[i + 1] == ']' {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn matching_open_delimiter(
    chars: &[char],
    close_pos: usize,
    open: char,
    close: char,
) -> Option<usize> {
    let mut depth = 0usize;
    for i in (0..=close_pos).rev() {
        if chars[i] == close {
            depth += 1;
        } else if chars[i] == open {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

fn trim_char_vec(chars: &mut Vec<char>) {
    while chars.first().is_some_and(|ch| ch.is_whitespace()) {
        chars.remove(0);
    }
    while chars.last().is_some_and(|ch| ch.is_whitespace()) {
        chars.pop();
    }
}

fn skip_quoted_chars(chars: &[char], quote_start: usize) -> usize {
    let quote = chars[quote_start];
    let mut i = quote_start + 1;
    while i < chars.len() {
        if chars[i] == '\\' {
            i = (i + 2).min(chars.len());
            continue;
        }
        if chars[i] == quote {
            return i + 1;
        }
        i += 1;
    }
    chars.len()
}

fn is_ident_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

fn is_ident_continue(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}

const TYPEDEF_DECLARATOR_SKIP_WORDS: &[&str] = &[
    "const",
    "volatile",
    "restrict",
    "_Atomic",
    "__attribute__",
    "__declspec",
    "_Alignas",
    "alignas",
];

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

fn record_keyword_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r#"\btypedef\b[\s\S]*\b(struct|union|enum|class)\b"#)
            .expect("record typedef regex")
    })
}

fn identifier_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r#"[A-Za-z_][A-Za-z0-9_]*"#).expect("identifier regex"))
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
