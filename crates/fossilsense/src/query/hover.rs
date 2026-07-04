use crate::model::DefinitionCandidate;
use crate::reachability::ReachScope;
use crate::store::SymbolRecord;

use super::definitions::rank_definition_records_with_scope;

pub const HOVER_CANDIDATE_LIMIT: usize = 4;
const COMMENT_LINE_LIMIT: usize = 48;
const COMMENT_CHAR_LIMIT: usize = 2_000;

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
    if cursor == 0 {
        return None;
    }

    let previous = lines[cursor - 1].trim_start();
    let raw_lines = if is_line_comment(previous) {
        collect_line_comment_group(&lines, cursor - 1)
    } else if is_block_comment_boundary(previous) {
        collect_block_comment_group(&lines, cursor - 1)
    } else {
        Vec::new()
    };
    if raw_lines.is_empty() {
        return None;
    }

    let cleaned = clean_comment_lines(&raw_lines);
    render_comment_markdown(&cleaned)
}

pub fn hover_markdown_for_candidate(
    ranked: &RankedHoverCandidate,
    comment_markdown: Option<&str>,
) -> String {
    let mut out = String::new();
    if !ranked.signature.trim().is_empty() {
        out.push_str("```c\n");
        out.push_str(&sanitize_markdown(&ranked.signature));
        out.push_str("\n```\n\n");
    } else {
        out.push_str("```c\n");
        out.push_str(&ranked.candidate.name);
        out.push_str("\n```\n\n");
    }

    if let Some(comment) = comment_markdown.filter(|s| !s.trim().is_empty()) {
        out.push_str(comment.trim());
        out.push_str("\n\n");
    }

    out.push_str(&format!(
        "**FossilSense candidate:** {} {} in `{}`  \n",
        ranked.candidate.kind, ranked.candidate.role, ranked.candidate.path
    ));
    out.push_str(&format!(
        "tier: `{}` | confidence: `{}` | reason: `{}`",
        ranked.candidate.tier.as_str(),
        ranked.candidate.confidence.as_str(),
        ranked.candidate.reason.as_str()
    ));
    if let Some(guard) = &ranked.guard {
        out.push_str(&format!("  \nguard: `{}`", sanitize_markdown(guard)));
    }
    out
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
        out.push(sanitize_markdown(&line));
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
    let mut prose = Vec::new();
    let mut params = Vec::new();
    let mut returns = Vec::new();
    let mut retvals = Vec::new();
    let mut notes = Vec::new();
    let mut warnings = Vec::new();

    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = command_rest(trimmed, "brief") {
            push_limited(&mut prose, rest);
        } else if let Some(rest) = command_rest(trimmed, "param") {
            let (name, body) = split_name_body(rest);
            params.push(format!("- `{}`{}", name, body_suffix(body)));
        } else if let Some(rest) =
            command_rest(trimmed, "return").or_else(|| command_rest(trimmed, "returns"))
        {
            push_limited(&mut returns, rest);
        } else if let Some(rest) = command_rest(trimmed, "retval") {
            let (name, body) = split_name_body(rest);
            retvals.push(format!("- `{}`{}", name, body_suffix(body)));
        } else if let Some(rest) = command_rest(trimmed, "note") {
            push_limited(&mut notes, rest);
        } else if let Some(rest) = command_rest(trimmed, "warning") {
            push_limited(&mut warnings, rest);
        } else if let Some(stripped) = strip_unknown_command(trimmed) {
            push_limited(&mut prose, stripped);
        } else {
            push_limited(&mut prose, trimmed);
        }
    }

    let mut sections = Vec::new();
    if !prose.is_empty() {
        sections.push(prose.join("\n"));
    }
    if !params.is_empty() {
        sections.push(format!("**Parameters**\n{}", params.join("\n")));
    }
    if !returns.is_empty() {
        sections.push(format!("**Returns:** {}", returns.join(" ")));
    }
    if !retvals.is_empty() {
        sections.push(format!("**Return values**\n{}", retvals.join("\n")));
    }
    for note in notes {
        sections.push(format!("> **Note:** {}", note));
    }
    for warning in warnings {
        sections.push(format!("> **Warning:** {}", warning));
    }

    let rendered = sections.join("\n\n");
    (!rendered.trim().is_empty()).then(|| rendered.chars().take(COMMENT_CHAR_LIMIT).collect())
}

fn command_rest<'a>(line: &'a str, command: &str) -> Option<&'a str> {
    for prefix in ['@', '\\'] {
        let marker = format!("{prefix}{command}");
        if let Some(rest) = line.strip_prefix(&marker) {
            if rest.is_empty() || rest.starts_with(char::is_whitespace) {
                return Some(rest.trim());
            }
            if command == "param" && rest.starts_with('[') {
                if let Some(close) = rest.find(']') {
                    let after = &rest[close + 1..];
                    if after.is_empty() || after.starts_with(char::is_whitespace) {
                        return Some(after.trim());
                    }
                }
            }
        }
    }
    None
}

fn strip_unknown_command(line: &str) -> Option<&str> {
    let first = line.as_bytes().first().copied()?;
    if first != b'@' && first != b'\\' {
        return None;
    }
    Some(line[1..].trim())
}

fn split_name_body(rest: &str) -> (&str, &str) {
    let trimmed = rest.trim();
    match trimmed.split_once(char::is_whitespace) {
        Some((name, body)) => (name, body.trim()),
        None => (trimmed, ""),
    }
}

fn body_suffix(body: &str) -> String {
    if body.is_empty() {
        String::new()
    } else {
        format!(" - {body}")
    }
}

fn push_limited(out: &mut Vec<String>, value: &str) {
    let value = value.trim();
    if !value.is_empty() {
        out.push(value.to_string());
    }
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
    fn doxygen_comment_renders_markdown_sections() {
        let source = "/**\n * @brief Adds two values.\n * @param lhs left side\n * @param rhs right side\n * @return the sum\n */\nint add(int lhs, int rhs);\n";
        let markdown = leading_comment_markdown(source, 6).expect("comment");
        assert!(markdown.contains("Adds two values."));
        assert!(markdown.contains("**Parameters**"));
        assert!(markdown.contains("- `lhs` - left side"));
        assert!(markdown.contains("**Returns:** the sum"));
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
        assert!(markdown.contains("weird custom tag is kept readable"));
        assert!(markdown.contains("warning without marker"));
        assert!(!markdown.contains("/*"));
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
    fn doxygen_param_direction_renders_as_parameter() {
        let source = "/**\n * @brief Copies bytes.\n * @param[in] src source bytes\n * @param[out] dst destination bytes\n */\nvoid copy(void *dst, const void *src);\n";
        let markdown = leading_comment_markdown(source, 5).expect("comment");
        assert!(markdown.contains("**Parameters**"));
        assert!(markdown.contains("- `src` - source bytes"));
        assert!(markdown.contains("- `dst` - destination bytes"));
        assert!(!markdown.contains("param[in]"));
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
        assert!(markdown.contains("```c\nint foo(int x)\n```"));
        assert!(markdown.contains("Does work."));
        assert!(markdown.contains("tier: `global`"));
        assert!(!markdown.contains(":43"));
        assert!(!markdown.contains("start_line"));
    }
}
