use crate::model::DefinitionCandidate;
use crate::reachability::ReachScope;
use crate::store::SymbolRecord;

use super::definitions::rank_definition_records_with_scope;

pub const HOVER_CANDIDATE_LIMIT: usize = 4;
const COMMENT_LINE_LIMIT: usize = 48;
const COMMENT_CHAR_LIMIT: usize = 2_000;
const ORDINARY_COMMENT_LINE_LIMIT: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankedHoverCandidate {
    pub candidate: DefinitionCandidate,
    pub signature: String,
    pub guard: Option<String>,
}

pub fn rank_hover_candidates(
    records: Vec<SymbolRecord>,
    current_rel_path: &str,
    scope: Option<&ReachScope>,
    limit: usize,
) -> Vec<RankedHoverCandidate> {
    rank_definition_records_with_scope(records, current_rel_path, scope)
        .into_iter()
        .take(limit)
        .map(|ranked| RankedHoverCandidate {
            signature: ranked.record.signature,
            guard: ranked.record.guard,
            candidate: ranked.candidate,
        })
        .collect()
}

pub fn leading_comment_markdown(source: &str, symbol_start_line: u32) -> Option<String> {
    let lines: Vec<&str> = source.lines().collect();
    let cursor = (symbol_start_line as usize).min(lines.len());

    let inline_block = collect_inline_block_comment_on_symbol_line(&lines, cursor);
    let raw_lines = if !inline_block.is_empty() {
        inline_block
    } else if cursor == 0 {
        Vec::new()
    } else {
        let previous = lines[cursor - 1].trim_start();
        if is_line_comment(previous) {
            collect_line_comment_group(&lines, cursor - 1)
        } else if is_block_comment_boundary(previous) {
            collect_block_comment_group(&lines, cursor - 1)
        } else {
            Vec::new()
        }
    };
    if raw_lines.is_empty() {
        return None;
    }

    let cleaned = clean_comment_lines(&raw_lines);
    if !should_attach_comment_block(&raw_lines, &cleaned) {
        return None;
    }
    render_comment_markdown(&cleaned)
}

pub fn hover_markdown_for_candidate(
    ranked: &RankedHoverCandidate,
    comment_markdown: Option<&str>,
) -> String {
    let mut out = String::new();
    let signature_comment = leading_signature_comment_markdown(&ranked.signature);
    let comment_markdown = comment_markdown.or(signature_comment.as_deref());

    if let Some(comment) = comment_markdown.filter(|s| !s.trim().is_empty()) {
        out.push_str(comment.trim());
        out.push_str("\n\n");
    }

    out.push_str("```c\n");
    let guard = ranked
        .guard
        .as_deref()
        .filter(|guard| !guard.trim().is_empty())
        .filter(|guard| should_render_guard_wrapper(guard));
    if let Some(guard) = guard {
        out.push_str(&sanitize_markdown(guard.trim()));
        out.push('\n');
    }
    out.push_str(&format!("// In {}\n", ranked.candidate.path));
    out.push_str(&sanitize_markdown(&display_signature(
        &ranked.signature,
        &ranked.candidate.name,
    )));
    out.push('\n');
    if let Some(guard) = guard {
        out.push_str("#endif // ^^ ");
        out.push_str(&sanitize_markdown(&guard_closing_label(guard)));
        out.push_str(" ^^\n");
    }
    out.push_str("```\n\n");

    out.push_str(&format!(
        "<small><span style=\"color: var(--vscode-descriptionForeground);\"><em>tier: {} | confidence: {} | reason: {}</em></span></small>",
        ranked.candidate.tier.as_str(),
        ranked.candidate.confidence.as_str(),
        ranked.candidate.reason.as_str()
    ));
    out
}

fn display_signature(signature: &str, fallback_name: &str) -> String {
    let stripped = strip_leading_signature_comments(signature).trim();
    if stripped.is_empty() {
        fallback_name.to_string()
    } else {
        stripped.to_string()
    }
}

fn strip_leading_signature_comments(signature: &str) -> &str {
    let mut rest = signature.trim_start();
    loop {
        if rest.starts_with("/*") {
            let Some(end) = rest.find("*/") else {
                return "";
            };
            rest = rest[end + 2..].trim_start();
            continue;
        }
        if rest.starts_with("//") {
            let Some(end) = rest.find('\n') else {
                return "";
            };
            rest = rest[end + 1..].trim_start();
            continue;
        }
        return rest;
    }
}

fn leading_signature_comment_markdown(signature: &str) -> Option<String> {
    let comment = leading_signature_comment(signature)?;
    let raw_lines: Vec<String> = comment.lines().map(|line| line.to_string()).collect();
    let cleaned = clean_comment_lines(&raw_lines);
    if !should_attach_comment_block(&raw_lines, &cleaned) {
        return None;
    }
    render_comment_markdown(&cleaned)
}

fn leading_signature_comment(signature: &str) -> Option<String> {
    let trimmed = signature.trim_start();
    if trimmed.starts_with("/*") {
        let end = trimmed.find("*/")? + 2;
        return Some(trimmed[..end].to_string());
    }
    if trimmed.starts_with("//") {
        let lines: Vec<&str> = trimmed
            .lines()
            .take_while(|line| is_line_comment(line.trim_start()))
            .collect();
        if !lines.is_empty() {
            return Some(lines.join("\n"));
        }
    }
    None
}

fn should_render_guard_wrapper(guard: &str) -> bool {
    !is_header_guard_label(&guard_closing_label(guard))
}

fn is_header_guard_label(label: &str) -> bool {
    let label = label.trim();
    label.ends_with("_H")
}

fn guard_closing_label(guard: &str) -> String {
    let trimmed = guard.trim();
    for prefix in ["#ifndef", "#ifdef", "#define"] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            if let Some(token) = rest.split_whitespace().next() {
                return clean_guard_token(token);
            }
        }
    }
    if let Some(token) = defined_guard_token(trimmed) {
        return token;
    }
    trimmed.to_string()
}

fn defined_guard_token(value: &str) -> Option<String> {
    let index = value.find("defined")?;
    let after = value[index + "defined".len()..].trim_start();
    if let Some(rest) = after.strip_prefix('(') {
        let end = rest.find(')')?;
        return Some(clean_guard_token(&rest[..end]));
    }
    after.split_whitespace().next().map(clean_guard_token)
}

fn clean_guard_token(token: &str) -> String {
    token
        .trim()
        .trim_matches(|c: char| c == '(' || c == ')' || c == '!')
        .to_string()
}

fn collect_line_comment_group(lines: &[&str], start: usize) -> Vec<String> {
    let mut first = start;
    while first > 0 && is_line_comment(lines[first - 1].trim_start()) {
        first -= 1;
    }
    lines[first..=start]
        .iter()
        .take(COMMENT_LINE_LIMIT)
        .map(|line| (*line).to_string())
        .collect()
}

fn collect_block_comment_group(lines: &[&str], end: usize) -> Vec<String> {
    if !is_block_comment_boundary(lines[end]) {
        return Vec::new();
    }
    let mut first = end;
    while first > 0 && !lines[first].trim_start().starts_with("/*") {
        if !is_block_comment_scan_line(lines[first])
            || has_trailing_code_after_block_close(lines[first])
        {
            return Vec::new();
        }
        first -= 1;
    }
    if !lines[first].trim_start().starts_with("/*") {
        return Vec::new();
    }
    if has_trailing_code_after_block_close(lines[first]) {
        return Vec::new();
    }
    lines[first..=end]
        .iter()
        .take(COMMENT_LINE_LIMIT)
        .map(|line| (*line).to_string())
        .collect()
}

fn collect_inline_block_comment_on_symbol_line(lines: &[&str], symbol_line: usize) -> Vec<String> {
    let Some(line) = lines.get(symbol_line) else {
        return Vec::new();
    };
    let trimmed = line.trim_start();
    if !is_block_comment_boundary(trimmed) || !trimmed.contains("*/") {
        return Vec::new();
    }

    let mut first = symbol_line;
    while first > 0 && !lines[first].trim_start().starts_with("/*") {
        if !is_block_comment_scan_line(lines[first]) {
            return Vec::new();
        }
        first -= 1;
    }
    if !lines[first].trim_start().starts_with("/*") {
        return Vec::new();
    }

    let mut out: Vec<String> = lines[first..=symbol_line]
        .iter()
        .map(|line| (*line).to_string())
        .collect();
    if let Some(last) = out.last_mut() {
        if let Some(close) = last.find("*/") {
            last.truncate(close + 2);
        }
    }
    out
}

fn clean_comment_lines(raw_lines: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for raw in raw_lines.iter().take(COMMENT_LINE_LIMIT) {
        let mut line = raw.trim().to_string();
        if line.starts_with("///") || line.starts_with("//!") {
            line = line[3..].to_string();
        } else if line.starts_with("//") {
            line = line[2..].to_string();
        } else {
            line = strip_block_markers(&line);
        }
        let line = line.trim().to_string();
        out.push(line);
    }
    trim_empty_edges(out)
}

fn strip_block_markers(line: &str) -> String {
    let mut s = line.trim().to_string();
    if s.starts_with("/**") || s.starts_with("/*!") {
        s = s[3..].to_string();
    } else if s.starts_with("/*") {
        s = s[2..].to_string();
    }
    if s.ends_with("*/") {
        s.truncate(s.len().saturating_sub(2));
    }
    let s = s.trim_start();
    let s = s.strip_prefix('*').unwrap_or(s).trim_start();
    s.to_string()
}

fn render_comment_markdown(lines: &[String]) -> Option<String> {
    let rendered = lines
        .iter()
        .map(|line| highlight_doc_tags(&sanitize_markdown(line)))
        .collect::<Vec<_>>()
        .join("\n");
    (!rendered.trim().is_empty()).then(|| rendered.chars().take(COMMENT_CHAR_LIMIT).collect())
}

fn trim_empty_edges(mut lines: Vec<String>) -> Vec<String> {
    while lines.first().is_some_and(|line| line.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    lines
}

fn sanitize_markdown(value: &str) -> String {
    value.replace("```", "'''")
}

fn should_attach_comment_block(raw_lines: &[String], cleaned: &[String]) -> bool {
    if cleaned.is_empty() || is_file_header_comment(cleaned) {
        return false;
    }

    if is_doc_comment_block(raw_lines) {
        return true;
    }

    cleaned
        .iter()
        .filter(|line| !line.trim().is_empty())
        .count()
        <= ORDINARY_COMMENT_LINE_LIMIT
}

fn is_doc_comment_block(raw_lines: &[String]) -> bool {
    raw_lines
        .iter()
        .find(|line| !line.trim().is_empty())
        .is_some_and(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with("///")
                || trimmed.starts_with("//!")
                || trimmed.starts_with("/**")
                || trimmed.starts_with("/*!")
        })
}

fn is_file_header_comment(lines: &[String]) -> bool {
    lines.iter().any(|line| {
        let lower = line.trim().to_ascii_lowercase();
        lower.starts_with("@file")
            || lower.starts_with("\\file")
            || lower.contains("spdx-license-identifier")
            || lower.contains("copyright")
            || lower.starts_with("license:")
            || lower.starts_with("project:")
            || lower.starts_with("module:")
            || lower.starts_with("author:")
    })
}

fn highlight_doc_tags(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some((start, end)) = next_doc_tag(rest) {
        out.push_str(&rest[..start]);
        out.push('`');
        out.push_str(&rest[start..end].replace('`', "'"));
        out.push('`');
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

fn next_doc_tag(value: &str) -> Option<(usize, usize)> {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'@' | b'\\' if is_doxygen_tag_start(value, index) => {
                return Some((index, doxygen_tag_end(value, index + 1)));
            }
            b'<' => {
                if let Some(end) = xml_tag_end(value, index) {
                    return Some((index, end));
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn is_doxygen_tag_start(value: &str, index: usize) -> bool {
    let Some(next) = value.as_bytes().get(index + 1).copied() else {
        return false;
    };
    if !is_ident_start(next) {
        return false;
    }
    if index == 0 {
        return true;
    }
    let previous = value.as_bytes()[index - 1];
    !is_ident_continue(previous) && previous != b'.'
}

fn doxygen_tag_end(value: &str, mut index: usize) -> usize {
    let bytes = value.as_bytes();
    while index < bytes.len() && is_ident_continue(bytes[index]) {
        index += 1;
    }
    if bytes.get(index) == Some(&b'[') {
        let mut close = index + 1;
        while close < bytes.len() && bytes[close] != b']' && !bytes[close].is_ascii_whitespace() {
            close += 1;
        }
        if bytes.get(close) == Some(&b']') {
            index = close + 1;
        }
    }
    index
}

fn xml_tag_end(value: &str, index: usize) -> Option<usize> {
    let bytes = value.as_bytes();
    let mut cursor = index + 1;
    if bytes.get(cursor) == Some(&b'/') {
        cursor += 1;
    }
    if !bytes.get(cursor).copied().is_some_and(is_ident_start) {
        return None;
    }
    while cursor < bytes.len() && bytes[cursor] != b'>' {
        if bytes[cursor] == b'<' {
            return None;
        }
        cursor += 1;
    }
    (cursor < bytes.len()).then_some(cursor + 1)
}

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_ident_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn is_line_comment(line: &str) -> bool {
    line.starts_with("//")
}

fn is_block_comment_boundary(line: &str) -> bool {
    let trimmed = line.trim_start();
    !trimmed.is_empty() && (trimmed.starts_with("/*") || trimmed.starts_with('*'))
}

fn is_block_comment_scan_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.is_empty() || trimmed.starts_with("/*") || trimmed.starts_with('*')
}

fn has_trailing_code_after_block_close(line: &str) -> bool {
    line.find("*/")
        .is_some_and(|index| !line[index + 2..].trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbol_record(
        name: &str,
        kind: &str,
        role: &str,
        path: &str,
        line: u32,
        signature: &str,
    ) -> SymbolRecord {
        SymbolRecord {
            id: 0,
            name: name.to_string(),
            kind: kind.to_string(),
            role: role.to_string(),
            path: path.to_string(),
            start_line: line,
            start_col: 0,
            end_line: line,
            end_col: 0,
            signature: signature.to_string(),
            guard: None,
            source: "workspace".to_string(),
            directly_included: false,
        }
    }

    #[test]
    fn hover_candidates_preserve_signatures_and_scope_order() {
        let records = vec![
            symbol_record(
                "foo",
                "function",
                "definition",
                "other/foo.c",
                20,
                "int foo(float x)",
            ),
            symbol_record(
                "foo",
                "macro",
                "definition",
                "src/main.c",
                2,
                "#define foo(x) (x)",
            ),
            symbol_record(
                "foo",
                "function",
                "declaration",
                "inc/foo.h",
                7,
                "int foo(int x);",
            ),
        ];
        let reach = ReachScope {
            files: ["src/main.c".to_string(), "inc/foo.h".to_string()]
                .into_iter()
                .collect(),
            open: false,
            reason: None,
        };
        let ranked = rank_hover_candidates(records, "src/main.c", Some(&reach), 4);
        assert_eq!(ranked[0].candidate.path, "src/main.c");
        assert_eq!(ranked[0].signature, "#define foo(x) (x)");
        assert_eq!(ranked[1].candidate.path, "inc/foo.h");
        assert_eq!(ranked[1].signature, "int foo(int x);");
    }

    #[test]
    fn hover_candidates_cap_after_ranking() {
        let records = vec![
            symbol_record("foo", "function", "definition", "b.c", 0, "int foo(int b)"),
            symbol_record("foo", "function", "definition", "a.c", 0, "int foo(int a)"),
        ];
        let ranked = rank_hover_candidates(records, "main.c", None, 1);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].candidate.path, "a.c");
    }

    #[test]
    fn doxygen_comment_preserves_lines_and_highlights_tags() {
        let source = "/**\n * @brief Adds two values.\n * @param lhs left side\n * @param rhs right side\n * @return the sum\n */\nint add(int lhs, int rhs);\n";
        let markdown = leading_comment_markdown(source, 6).expect("comment");
        assert_eq!(
            markdown,
            "`@brief` Adds two values.\n`@param` lhs left side\n`@param` rhs right side\n`@return` the sum"
        );
        assert!(!markdown.contains("*/"));
    }

    #[test]
    fn ordinary_line_comments_render_as_prose() {
        let source = "// Initializes the driver.\n// Safe to call twice.\nvoid init(void);\n";
        let markdown = leading_comment_markdown(source, 2).expect("comment");
        assert_eq!(markdown, "Initializes the driver.\nSafe to call twice.");
    }

    #[test]
    fn messy_comment_degrades_to_readable_text() {
        let source = "/* ***\n * @weird custom tag is kept readable\n * warning without marker\n */\nint odd(void);\n";
        let markdown = leading_comment_markdown(source, 4).expect("comment");
        assert!(markdown.contains("`@weird` custom tag is kept readable"));
        assert!(markdown.contains("warning without marker"));
        assert!(!markdown.contains("/*"));
    }

    #[test]
    fn file_header_comment_does_not_attach_to_first_symbol() {
        let source = "/*\n * Copyright 2026 Example Corp.\n * Project: boot firmware\n * License: internal use only\n */\nint first_symbol(void);\n";
        assert!(leading_comment_markdown(source, 5).is_none());
    }

    #[test]
    fn doxygen_file_comment_does_not_attach_to_first_symbol() {
        let source = "/**\n * @file driver.h\n * @brief Shared driver declarations.\n */\nint first_symbol(void);\n";
        assert!(leading_comment_markdown(source, 4).is_none());
    }

    #[test]
    fn blank_line_between_comment_and_symbol_blocks_attachment() {
        let source = "// Docs for previous thing\n\nint current;\n";
        assert!(leading_comment_markdown(source, 2).is_none());
    }

    #[test]
    fn trailing_inline_block_comment_does_not_attach_to_next_symbol() {
        let source = "int old; /* note for old */\nint current;\n";
        assert!(leading_comment_markdown(source, 1).is_none());
    }

    #[test]
    fn block_comment_with_internal_blank_line_still_attaches() {
        let source = "/**\n * First paragraph.\n\n * Second paragraph.\n */\nint current;\n";
        let markdown = leading_comment_markdown(source, 5).expect("comment");
        assert!(markdown.contains("First paragraph."));
        assert!(markdown.contains("Second paragraph."));
    }

    #[test]
    fn inline_leading_block_comment_on_symbol_line_still_attaches() {
        let source = "/** Test to see if a format is supported. */ bool test_fmt(void);\n";
        let markdown = leading_comment_markdown(source, 0).expect("comment");
        assert_eq!(markdown, "Test to see if a format is supported.");
    }

    #[test]
    fn closing_block_comment_on_symbol_line_still_attaches() {
        let source = "/**\n * Test to see if a format is supported.\n */ bool test_fmt(void);\n";
        let markdown = leading_comment_markdown(source, 2).expect("comment");
        assert_eq!(markdown, "Test to see if a format is supported.");
    }

    #[test]
    fn doxygen_param_direction_renders_as_parameter() {
        let source = "/**\n * @brief Copies bytes.\n * @param[in] src source bytes\n * @param[out] dst destination bytes\n */\nvoid copy(void *dst, const void *src);\n";
        let markdown = leading_comment_markdown(source, 5).expect("comment");
        assert!(markdown.contains("`@brief` Copies bytes."));
        assert!(markdown.contains("`@param[in]` src source bytes"));
        assert!(markdown.contains("`@param[out]` dst destination bytes"));
        assert!(!markdown.contains("**Parameters**"));
    }

    #[test]
    fn multiline_comment_keeps_line_breaks_and_highlights_common_tags() {
        let source = "/**\n * @brief First line.\n *\n * Second line with <summary>tag</summary> and \\return marker.\n */\nint current;\n";
        let markdown = leading_comment_markdown(source, 5).expect("comment");
        assert_eq!(
            markdown,
            "`@brief` First line.\n\nSecond line with `<summary>`tag`</summary>` and `\\return` marker."
        );
    }

    #[test]
    fn code_line_between_comment_and_symbol_blocks_attachment() {
        let source = "// Docs for old thing\nint old;\nint current;\n";
        assert!(leading_comment_markdown(source, 2).is_none());
    }

    #[test]
    fn hover_markdown_uses_signature_and_hides_source_ranges() {
        let ranked = rank_hover_candidates(
            vec![symbol_record(
                "foo",
                "function",
                "definition",
                "src/foo.c",
                42,
                "int foo(int x)",
            )],
            "src/main.c",
            None,
            1,
        )
        .remove(0);
        let markdown = hover_markdown_for_candidate(&ranked, Some("Does work."));
        assert!(markdown.contains("```c\n// In src/foo.c\nint foo(int x)\n```"));
        assert!(markdown.contains("Does work."));
        assert!(markdown.contains("tier: global"));
        assert!(!markdown.contains(":43"));
        assert!(!markdown.contains("start_line"));
    }

    #[test]
    fn hover_markdown_renders_quiet_source_header_and_metadata() {
        let ranked = rank_hover_candidates(
            vec![symbol_record(
                "ff_sws_lut3d_test_fmt",
                "function",
                "definition",
                "libswscale/lut3d.c",
                12,
                "bool ff_sws_lut3d_test_fmt(enum AVPixelFormat fmt, int output)",
            )],
            "libswscale/lut3d.c",
            None,
            1,
        )
        .remove(0);

        let markdown = hover_markdown_for_candidate(&ranked, None);

        assert!(markdown.contains(
            "```c\n// In libswscale/lut3d.c\nbool ff_sws_lut3d_test_fmt(enum AVPixelFormat fmt, int output)\n```"
        ));
        assert!(markdown.contains(
            "<small><span style=\"color: var(--vscode-descriptionForeground);\"><em>tier: current | confidence: exact | reason: current_file</em></span></small>"
        ));
        assert!(!markdown.contains("FossilSense candidate"));
        assert!(!markdown.contains("function definition in"));
    }

    #[test]
    fn hover_markdown_splits_signature_comment_and_omits_header_guard_wrapper() {
        let mut ranked = rank_hover_candidates(
            vec![symbol_record(
                "ff_sws_lut3d_test_fmt",
                "function",
                "declaration",
                "libswscale/lut3d.h",
                5,
                "/** * Test to see if a given format is supported by the 3DLUT input/output code. */ bool ff_sws_lut3d_test_fmt(enum AVPixelFormat fmt, int output);",
            )],
            "libswscale/lut3d.c",
            None,
            1,
        )
        .remove(0);
        ranked.guard = Some("#ifndef SWSCALE_LUT3D_H".to_string());

        let markdown = hover_markdown_for_candidate(&ranked, None);

        assert!(markdown.starts_with(
            "Test to see if a given format is supported by the 3DLUT input/output code.\n\n"
        ));
        assert!(markdown.contains(
            "```c\n// In libswscale/lut3d.h\nbool ff_sws_lut3d_test_fmt(enum AVPixelFormat fmt, int output);\n```"
        ));
        assert!(!markdown.contains("#ifndef SWSCALE_LUT3D_H"));
        assert!(!markdown.contains("#endif // ^^ SWSCALE_LUT3D_H ^^"));
        assert!(!markdown.contains("/**"));
        assert!(!markdown.contains("*/ bool"));
        assert!(!markdown.contains("guard:"));
    }

    #[test]
    fn hover_markdown_keeps_non_header_guard_wrapper() {
        let mut ranked = rank_hover_candidates(
            vec![symbol_record(
                "platform_init",
                "function",
                "declaration",
                "include/platform.h",
                8,
                "void platform_init(void);",
            )],
            "src/main.c",
            None,
            1,
        )
        .remove(0);
        ranked.guard = Some("#ifdef CONFIG_PLATFORM_INIT".to_string());

        let markdown = hover_markdown_for_candidate(&ranked, None);

        assert!(markdown.contains(
            "```c\n#ifdef CONFIG_PLATFORM_INIT\n// In include/platform.h\nvoid platform_init(void);\n#endif // ^^ CONFIG_PLATFORM_INIT ^^\n```"
        ));
    }
}
