use std::sync::OnceLock;

use regex::Regex;

use super::{Include, RawDeclaration, SymbolKind, SymbolRole};
use crate::call_model::{SourcePosition, SourceRange};
use crate::config::SourceLanguage;
use crate::semantic_model::{CompletionKindHint, FallbackCompletionFact};

mod declarators;

use declarators::*;

/// The only lexical pass that runs on every parse.
pub(super) fn scan_includes(source: &str) -> Vec<Include> {
    let mut in_leading_block_comment = false;
    source
        .lines()
        .enumerate()
        .filter_map(|(line, text)| {
            let code = strip_leading_comments(text, &mut in_leading_block_comment);
            capture_include(code.trim(), line)
        })
        .collect()
}

/// Last-resort name hints. These facts deliberately carry no semantic identity
/// or navigation locator and are only exposed through the fallback completion
/// channel.
pub(super) fn extract_fallback_completions(
    source: &str,
    language: SourceLanguage,
) -> Vec<FallbackCompletionFact> {
    let starts = super::line_starts(source);
    let symbols =
        extract_fallback_declarations_raw(source, &starts, language == SourceLanguage::Cpp);
    symbols
        .into_iter()
        .filter_map(|symbol| {
            let kind_hint = match symbol.kind {
                SymbolKind::Function => CompletionKindHint::Function,
                SymbolKind::Macro => CompletionKindHint::Macro,
                SymbolKind::Type => CompletionKindHint::Type,
                SymbolKind::GlobalVariable | SymbolKind::EnumConstant => CompletionKindHint::Object,
                SymbolKind::Field => return None,
            };
            Some(FallbackCompletionFact {
                name: symbol.name,
                kind_hint,
                range: SourceRange {
                    start: SourcePosition {
                        line: symbol.start_line as u32,
                        character: symbol.start_col as u32,
                    },
                    end: SourcePosition {
                        line: symbol.end_line as u32,
                        character: symbol.end_col as u32,
                    },
                    start_byte: symbol.start_byte,
                    end_byte: symbol.end_byte,
                },
                detail: (!symbol.signature.is_empty()).then_some(symbol.signature),
            })
        })
        .collect()
}

/// Private staging for completion-only name hints. Its declaration-shaped
/// records never leave this module and are converted to `FallbackCompletionFact`.
fn extract_fallback_declarations_raw(
    source: &str,
    line_starts: &[usize],
    is_cpp: bool,
) -> Vec<RawDeclaration> {
    let mut symbols = Vec::new();
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
                    is_cpp,
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

    symbols
}

#[cfg(test)]
pub(super) fn extract_symbols_and_includes(
    source: &str,
    line_starts: &[usize],
    is_cpp: bool,
) -> (Vec<RawDeclaration>, Vec<Include>) {
    (
        extract_fallback_declarations_raw(source, line_starts, is_cpp),
        scan_includes(source),
    )
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
) -> Option<RawDeclaration> {
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
    is_cpp: bool,
) -> Vec<RawDeclaration> {
    // All regex classification must see code only. In particular, flattening a
    // multi-line record before removing `//` comments can turn
    // `// ... class\nconst char *name;` into the false tag declaration
    // `class const`. Keep the original statement for signatures and ranges,
    // but mask comments and literals before any name-producing regex runs.
    let code_only = mask_comments_and_literals(&statement.text);
    let compact = compact_whitespace(&code_only);
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

    if let Some(symbol) =
        capture_global_variable(statement, &compact, line_starts, source, guard, is_cpp)
    {
        symbols.push(symbol);
    }

    symbols
}

/// Replace comment and literal bytes with ASCII spaces while preserving newlines
/// and byte offsets. This is a lexical safety boundary for the regex fallback,
/// not a C/C++ parser: normal parsing derives type symbols from tree-sitter.
fn mask_comments_and_literals(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = bytes.to_vec();
    let mut i = 0usize;

    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            let start = i;
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            mask_non_newlines(&mut out, start, i);
            continue;
        }
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            let start = i;
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            mask_non_newlines(&mut out, start, i);
            continue;
        }

        // C++ raw string literal: recognize the `R"delimiter(` core even when
        // preceded by an encoding prefix (`u8`, `u`, `U`, or `L`).
        if bytes[i] == b'R' && bytes.get(i + 1) == Some(&b'"') {
            if let Some(end) = raw_string_end(bytes, i) {
                mask_non_newlines(&mut out, i, end);
                i = end;
                continue;
            }
        }

        if bytes[i] == b'"' || bytes[i] == b'\'' {
            let start = i;
            i = skip_quoted_bytes(bytes, i);
            mask_non_newlines(&mut out, start, i);
            continue;
        }
        i += 1;
    }

    // Code bytes are copied unchanged and masked bytes are ASCII, so valid
    // UTF-8 input remains valid UTF-8.
    String::from_utf8(out).expect("masking preserves UTF-8")
}

fn mask_non_newlines(out: &mut [u8], start: usize, end: usize) {
    for byte in &mut out[start..end] {
        if !matches!(*byte, b'\n' | b'\r') {
            *byte = b' ';
        }
    }
}

fn raw_string_end(bytes: &[u8], start: usize) -> Option<usize> {
    let delimiter_start = start + 2;
    let open_rel = bytes
        .get(delimiter_start..)?
        .iter()
        .position(|byte| *byte == b'(')?;
    if open_rel > 16 {
        return None;
    }
    let open = delimiter_start + open_rel;
    let delimiter = &bytes[delimiter_start..open];
    if delimiter
        .iter()
        .any(|byte| byte.is_ascii_whitespace() || matches!(*byte, b'(' | b')' | b'\\'))
    {
        return None;
    }

    let mut i = open + 1;
    while i < bytes.len() {
        if bytes[i] == b')'
            && bytes.get(i + 1..i + 1 + delimiter.len()) == Some(delimiter)
            && bytes.get(i + 1 + delimiter.len()) == Some(&b'"')
        {
            return Some(i + delimiter.len() + 2);
        }
        i += 1;
    }
    Some(bytes.len())
}

fn capture_function(
    statement: &PendingStatement,
    compact: &str,
    line_starts: &[usize],
    source: &str,
    guard: Option<String>,
) -> Option<RawDeclaration> {
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
) -> Vec<RawDeclaration> {
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
) -> Vec<RawDeclaration> {
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
    is_cpp: bool,
) -> Option<RawDeclaration> {
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
    let name_match = captures.get(1)?;
    let name = name_match.as_str();
    let role = classify_global_object_role(compact, name_match, is_cpp);
    Some(make_symbol(
        name,
        SymbolKind::GlobalVariable,
        role,
        statement.start_line,
        statement.end_line,
        line_starts,
        source,
        compact.to_string(),
        guard,
    ))
}

fn classify_global_object_role(compact: &str, name: regex::Match<'_>, is_cpp: bool) -> SymbolRole {
    match declarator_has_initializer(&compact[name.end()..]) {
        Some(true) => SymbolRole::Definition,
        Some(false) if contains_identifier(&compact[..name.start()], "extern") => {
            SymbolRole::Declaration
        }
        // C++ has no tentative-definition category: a namespace-scope object
        // declaration without `extern` is a full definition.
        Some(false) if is_cpp => SymbolRole::Definition,
        Some(false) => SymbolRole::TentativeDefinition,
        None => SymbolRole::UnknownDeclarationOrDefinition,
    }
}

/// Inspect only the selected declarator suffix. `Some(true)` means a top-level
/// initializer was found, `Some(false)` means the declarator ended cleanly at a
/// semicolon, and `None` preserves uncertainty for malformed/unbalanced input.
fn declarator_has_initializer(suffix: &str) -> Option<bool> {
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut brace_depth = 0usize;
    let mut has_initializer = false;

    for ch in suffix.chars() {
        match ch {
            '(' => paren_depth += 1,
            '[' => bracket_depth += 1,
            '{' => brace_depth += 1,
            ')' => paren_depth = paren_depth.checked_sub(1)?,
            ']' => bracket_depth = bracket_depth.checked_sub(1)?,
            '}' => brace_depth = brace_depth.checked_sub(1)?,
            '=' if paren_depth == 0 && bracket_depth == 0 && brace_depth == 0 => {
                has_initializer = true;
            }
            ';' if paren_depth == 0 && bracket_depth == 0 && brace_depth == 0 => {
                return Some(has_initializer);
            }
            _ => {}
        }
    }

    None
}

fn contains_identifier(text: &str, expected: &str) -> bool {
    identifier_regex()
        .find_iter(text)
        .any(|identifier| identifier.as_str() == expected)
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
) -> RawDeclaration {
    let start_byte = line_starts.get(start_line).copied().unwrap_or(0);
    let end_byte = line_end_byte(source, line_starts, end_line);
    RawDeclaration {
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
        incomplete: false,
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

fn line_end_byte(source: &str, line_starts: &[usize], line: usize) -> usize {
    line_starts
        .get(line + 1)
        .copied()
        .map(|next| next.saturating_sub(1))
        .unwrap_or(source.len())
}

fn line_continues_preprocessor(line: &str) -> bool {
    line.trim_end_matches([' ', '\t', '\r']).ends_with('\\')
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
    fn push(&mut self, line: &str, line_index: usize, brace_delta: isize) {
        if !self.active {
            self.start_line = line_index;
            self.active = true;
        }
        self.end_line = line_index;
        self.text.push_str(line);
        self.text.push('\n');
        self.brace_balance += brace_delta;
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

#[derive(Debug, Default)]
struct BraceScanState {
    in_block_comment: bool,
}

fn code_brace_delta(line: &str, state: &mut BraceScanState) -> isize {
    let mut delta = 0isize;
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if state.in_block_comment {
            if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                state.in_block_comment = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }

        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            break;
        }
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            state.in_block_comment = true;
            i += 2;
            continue;
        }
        if bytes[i] == b'"' || bytes[i] == b'\'' {
            i = skip_quoted_bytes(bytes, i);
            continue;
        }

        match bytes[i] {
            b'{' => delta += 1,
            b'}' => delta -= 1,
            _ => {}
        }
        i += 1;
    }
    delta
}

fn skip_quoted_bytes(bytes: &[u8], quote_start: usize) -> usize {
    let quote = bytes[quote_start];
    let mut i = quote_start + 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i = (i + 2).min(bytes.len());
            continue;
        }
        if bytes[i] == quote {
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}

#[cfg(test)]
mod tests {
    use super::{extract_symbols_and_includes, mask_comments_and_literals, scan_includes};

    #[test]
    fn include_scan_ignores_directives_inside_multiline_block_comments() {
        let source = "/* disabled includes\n\
                      #include \"phantom.h\"\n\
                      */\n\
                      #include \"real.h\"\n";

        let includes = scan_includes(source);

        assert_eq!(includes.len(), 1);
        assert_eq!(includes[0].line, 3);
        assert_eq!(includes[0].target_text, "\"real.h\"");
    }

    #[test]
    fn masking_keeps_comment_and_literal_words_out_of_regex_input() {
        let source = "struct Real { int x; // class const\n".to_string()
            + "const char *s = R\"tag(union Phantom)tag\"; /* enum Ghost */\n};";
        let masked = mask_comments_and_literals(&source);

        assert!(masked.contains("struct Real"));
        assert!(masked.contains("const char *s"));
        assert!(!masked.contains("class const"));
        assert!(!masked.contains("union Phantom"));
        assert!(!masked.contains("enum Ghost"));
        assert_eq!(masked.len(), source.len());
        assert_eq!(masked.matches('\n').count(), source.matches('\n').count());
    }

    #[test]
    fn lexical_fallback_does_not_extract_types_from_trailing_comments() {
        let source = "typedef struct AVTextWriter {\n\
            const AVClass *priv_class; ///< private class of the writer, if any\n\
            int priv_size; ///< writer private class\n\
            const char *name;\n\
            } AVTextWriter;\n";
        let mut line_starts = vec![0];
        line_starts.extend(
            source
                .match_indices('\n')
                .map(|(index, _)| index + 1)
                .filter(|index| *index < source.len()),
        );
        let (symbols, _) = extract_symbols_and_includes(source, &line_starts, false);
        let names: Vec<_> = symbols.iter().map(|symbol| symbol.name.as_str()).collect();

        assert_eq!(names, vec!["AVTextWriter", "AVTextWriter"]);
        assert!(!names.contains(&"const"));
        assert!(!names.contains(&"of"));
    }
}
