//! Marker cleaning and tolerant Doxygen/XML comment parsing.
//!
//! Parsing produces a protocol-neutral [`CommentDocument`]. It never emits
//! Markdown directly.

use super::model::{
    CommentBlock, CommentDiagnostics, CommentDocument, CommentRenderOptions, CommentStyle,
    RawComment, TagAttribute, TagBlock, TagSyntax, TextBlock,
};

pub fn parse_raw_comment(raw: &RawComment, options: &CommentRenderOptions) -> CommentDocument {
    let cleaned = clean_comment_text(&raw.text, raw.style);
    let initial_diagnostics = CommentDiagnostics {
        truncated: raw.truncated,
        ..CommentDiagnostics::default()
    };
    if cleaned.iter().all(|line| line.trim().is_empty()) {
        return CommentDocument {
            blocks: Vec::new(),
            diagnostics: initial_diagnostics,
        };
    }
    if is_file_header_comment(&cleaned) {
        return CommentDocument {
            blocks: Vec::new(),
            diagnostics: initial_diagnostics,
        };
    }
    parse_cleaned_lines(&cleaned, options, initial_diagnostics)
}

pub fn is_attachable_document(doc: &CommentDocument) -> bool {
    !doc.blocks.is_empty()
}

fn parse_cleaned_lines(
    lines: &[String],
    options: &CommentRenderOptions,
    mut diagnostics: CommentDiagnostics,
) -> CommentDocument {
    let mut blocks = Vec::new();
    let mut index = 0usize;
    let limited: Vec<String> = lines
        .iter()
        .take(options.max_comment_lines)
        .cloned()
        .collect();
    if lines.len() > options.max_comment_lines {
        diagnostics.truncated = true;
    }

    while index < limited.len() {
        let line = &limited[index];
        if let Some(tag) = match_line_start_doxygen(line) {
            let (block, consumed, malformed) = consume_doxygen_tag(&limited, index, tag);
            if malformed {
                diagnostics.malformed_fallback = true;
            }
            blocks.push(CommentBlock::Tag(block));
            index += consumed;
            continue;
        }
        if let Some(tag) = match_line_start_xml_open(line) {
            let (block, consumed, unclosed) = consume_xml_tag(&limited, index, tag);
            if unclosed {
                diagnostics.unclosed_xml = true;
                diagnostics.malformed_fallback = true;
            }
            blocks.push(CommentBlock::Tag(block));
            index += consumed;
            continue;
        }

        let mut text_lines = Vec::new();
        while index < limited.len() {
            let current = &limited[index];
            if match_line_start_doxygen(current).is_some()
                || match_line_start_xml_open(current).is_some()
            {
                break;
            }
            text_lines.push(current.clone());
            index += 1;
        }
        // Drop purely leading empty edges only at document start; keep internal blanks.
        if blocks.is_empty() {
            while text_lines
                .first()
                .is_some_and(|line| line.trim().is_empty())
            {
                text_lines.remove(0);
            }
        }
        if !text_lines.is_empty() {
            blocks.push(CommentBlock::Text(TextBlock { lines: text_lines }));
        }
    }

    trim_trailing_empty_text(&mut blocks);
    CommentDocument {
        blocks,
        diagnostics,
    }
}

#[derive(Debug, Clone)]
struct DoxygenMatch {
    syntax: TagSyntax,
    raw_name: String,
    canonical_name: String,
    direction: Option<String>,
    rest: String,
    raw_line: String,
}

#[derive(Debug, Clone)]
struct XmlOpenMatch {
    raw_name: String,
    canonical_name: String,
    attributes: Vec<TagAttribute>,
    inline_body: String,
    self_closed: bool,
    #[allow(dead_code)]
    raw_line: String,
}

fn match_line_start_doxygen(line: &str) -> Option<DoxygenMatch> {
    let trimmed = line.trim_start();
    let bytes = trimmed.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let (syntax, name_start) = match bytes[0] {
        b'@' => (TagSyntax::DoxygenAt, 1usize),
        b'\\' => (TagSyntax::DoxygenBackslash, 1usize),
        b'/' if bytes.len() > 1 && is_ident_start(bytes[1]) => (TagSyntax::DoxygenSlash, 1usize),
        _ => return None,
    };
    if !bytes.get(name_start).copied().is_some_and(is_ident_start) {
        return None;
    }
    let mut name_end = name_start + 1;
    while name_end < bytes.len() && is_ident_continue(bytes[name_end]) {
        name_end += 1;
    }
    let raw_name = trimmed[name_start..name_end].to_string();
    let canonical_name = raw_name.to_ascii_lowercase();

    let mut cursor = name_end;
    let mut direction = None;
    if bytes.get(cursor) == Some(&b'[') {
        let mut close = cursor + 1;
        while close < bytes.len() && bytes[close] != b']' {
            close += 1;
        }
        if bytes.get(close) == Some(&b']') {
            direction = Some(trimmed[cursor + 1..close].trim().to_ascii_lowercase());
            cursor = close + 1;
        }
    }
    let rest = trimmed[cursor..].trim_start().to_string();
    Some(DoxygenMatch {
        syntax,
        raw_name,
        canonical_name,
        direction,
        rest,
        raw_line: line.to_string(),
    })
}

fn match_line_start_xml_open(line: &str) -> Option<XmlOpenMatch> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('<') || trimmed.starts_with("</") {
        return None;
    }
    let bytes = trimmed.as_bytes();
    let mut cursor = 1usize;
    if !bytes.get(cursor).copied().is_some_and(is_ident_start) {
        return None;
    }
    let name_start = cursor;
    cursor += 1;
    while cursor < bytes.len() && is_ident_continue(bytes[cursor]) {
        cursor += 1;
    }
    let raw_name = trimmed[name_start..cursor].to_string();
    let canonical_name = raw_name.to_ascii_lowercase();

    let mut attributes = Vec::new();
    loop {
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() {
            return None;
        }
        if bytes[cursor] == b'>' {
            cursor += 1;
            break;
        }
        if bytes[cursor] == b'/' && bytes.get(cursor + 1) == Some(&b'>') {
            return Some(XmlOpenMatch {
                raw_name,
                canonical_name,
                attributes,
                inline_body: String::new(),
                self_closed: true,
                raw_line: line.to_string(),
            });
        }
        if !is_ident_start(bytes[cursor]) {
            // Not a recoverable open tag start at line head.
            return None;
        }
        let attr_start = cursor;
        cursor += 1;
        while cursor < bytes.len() && (is_ident_continue(bytes[cursor]) || bytes[cursor] == b'-') {
            cursor += 1;
        }
        let attr_name = trimmed[attr_start..cursor].to_string();
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'=') {
            return None;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let quote = *bytes.get(cursor)?;
        if quote != b'"' && quote != b'\'' {
            return None;
        }
        cursor += 1;
        let value_start = cursor;
        while cursor < bytes.len() && bytes[cursor] != quote {
            cursor += 1;
        }
        if cursor >= bytes.len() {
            return None;
        }
        let attr_value = trimmed[value_start..cursor].to_string();
        cursor += 1;
        attributes.push(TagAttribute {
            name: attr_name,
            value: attr_value,
        });
    }

    let inline_body = trimmed[cursor..].to_string();
    // If the same line already contains the closing tag, peel it off.
    let close_needle = format!("</{raw_name}>");
    let (inline_body, self_closed) = if let Some(close_at) = find_ci(&inline_body, &close_needle) {
        (inline_body[..close_at].to_string(), true)
    } else {
        (inline_body, false)
    };

    Some(XmlOpenMatch {
        raw_name,
        canonical_name,
        attributes,
        inline_body,
        self_closed,
        raw_line: line.to_string(),
    })
}

fn consume_doxygen_tag(
    lines: &[String],
    start: usize,
    tag: DoxygenMatch,
) -> (TagBlock, usize, bool) {
    let mut body_lines = Vec::new();
    if !tag.rest.is_empty() {
        body_lines.push(tag.rest.clone());
    }
    let mut consumed = 1usize;
    let mut index = start + 1;
    while index < lines.len() {
        let current = &lines[index];
        if match_line_start_doxygen(current).is_some()
            || match_line_start_xml_open(current).is_some()
        {
            break;
        }
        body_lines.push(current.clone());
        consumed += 1;
        index += 1;
    }

    let mut attributes = Vec::new();
    let mut lines_out = body_lines;
    if is_param_name(&tag.canonical_name) {
        if let Some(direction) = tag.direction.clone() {
            attributes.push(TagAttribute {
                name: "direction".to_string(),
                value: direction,
            });
        }
        if let Some((name, rest)) =
            split_first_token(lines_out.first().map(String::as_str).unwrap_or(""))
        {
            attributes.push(TagAttribute {
                name: "name".to_string(),
                value: name,
            });
            if rest.is_empty() {
                if !lines_out.is_empty() {
                    lines_out.remove(0);
                }
            } else if let Some(first) = lines_out.first_mut() {
                *first = rest;
            }
        }
        for line in lines_out.iter_mut().skip(1) {
            *line = line.trim_start().to_string();
        }
    }

    (
        TagBlock {
            canonical_name: normalize_canonical(&tag.canonical_name),
            raw_name: tag.raw_name,
            syntax: tag.syntax,
            attributes,
            raw: std::iter::once(tag.raw_line)
                .chain(lines[start + 1..start + consumed].iter().cloned())
                .collect::<Vec<_>>()
                .join("\n"),
            lines: lines_out,
        },
        consumed,
        false,
    )
}

fn consume_xml_tag(lines: &[String], start: usize, tag: XmlOpenMatch) -> (TagBlock, usize, bool) {
    let mut body_lines = Vec::new();
    if !tag.inline_body.trim().is_empty() || (!tag.inline_body.is_empty() && tag.self_closed) {
        if !tag.inline_body.is_empty() {
            body_lines.push(tag.inline_body.clone());
        }
    } else if !tag.inline_body.is_empty() {
        body_lines.push(tag.inline_body.clone());
    }

    let mut consumed = 1usize;
    let mut unclosed = false;
    if !tag.self_closed {
        let close_needle = format!("</{}>", tag.raw_name);
        let mut index = start + 1;
        let mut closed = false;
        while index < lines.len() {
            let current = &lines[index];
            consumed += 1;
            if let Some(close_at) = find_ci(current.trim_start(), &close_needle) {
                let before = current.trim_start()[..close_at].to_string();
                if !before.is_empty() {
                    body_lines.push(before);
                }
                closed = true;
                break;
            }
            // A new top-level tag before close is treated as unclosed recovery stop.
            if match_line_start_doxygen(current).is_some()
                || match_line_start_xml_open(current).is_some()
            {
                unclosed = true;
                consumed -= 1;
                break;
            }
            body_lines.push(current.clone());
            index += 1;
        }
        if !closed && !unclosed {
            unclosed = true;
        }
    }

    (
        TagBlock {
            canonical_name: normalize_canonical(&tag.canonical_name),
            raw_name: tag.raw_name,
            syntax: TagSyntax::Xml,
            attributes: tag.attributes,
            raw: lines[start..start + consumed].join("\n"),
            lines: trim_empty_edges(body_lines),
        },
        consumed,
        unclosed,
    )
}

fn clean_comment_text(text: &str, style: CommentStyle) -> Vec<String> {
    let raw_lines: Vec<&str> = text.lines().collect();
    if raw_lines.is_empty() {
        return Vec::new();
    }
    match style {
        CommentStyle::Line | CommentStyle::DocLine => clean_line_comment_lines(&raw_lines),
        CommentStyle::Block | CommentStyle::DocBlock => clean_block_comment_lines(&raw_lines),
    }
}

fn clean_line_comment_lines(raw_lines: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for raw in raw_lines {
        let trimmed = raw.trim_start();
        let body = if let Some(rest) = trimmed.strip_prefix("///") {
            rest
        } else if let Some(rest) = trimmed.strip_prefix("//!") {
            rest
        } else if let Some(rest) = trimmed.strip_prefix("//") {
            rest
        } else {
            trimmed
        };
        // Preserve a single leading space convention without forcing trim of
        // meaningful indentation beyond the comment marker.
        let body = strip_one_leading_space(body).trim_end();
        out.push(body.to_string());
    }
    trim_empty_edges(out)
}

fn clean_block_comment_lines(raw_lines: &[&str]) -> Vec<String> {
    let mut lines: Vec<String> = raw_lines.iter().map(|line| (*line).to_string()).collect();
    if let Some(first) = lines.first_mut() {
        let trimmed_start = first.trim_start();
        let leading_ws_len = first.len() - trimmed_start.len();
        let stripped = if let Some(rest) = trimmed_start.strip_prefix("/**") {
            rest
        } else if let Some(rest) = trimmed_start.strip_prefix("/*!") {
            rest
        } else if let Some(rest) = trimmed_start.strip_prefix("/*") {
            rest
        } else {
            trimmed_start
        };
        *first = format!("{}{}", &first[..leading_ws_len], stripped);
    }
    if let Some(last) = lines.last_mut() {
        if let Some(index) = last.rfind("*/") {
            last.truncate(index);
        }
    }

    let mut out = Vec::new();
    for line in lines {
        out.push(strip_block_margin_star(&line));
    }
    trim_empty_edges(out)
}

fn strip_block_margin_star(line: &str) -> String {
    let trimmed = line.trim_start();
    // Only strip one decorative margin star. A second star belongs to content
    // (for example Markdown list markers).
    let body = if let Some(rest) = trimmed.strip_prefix('*') {
        strip_one_leading_space(rest)
    } else {
        // Keep non-margin content; trim only the indentation that usually
        // accompanies block decoration.
        trimmed
    };
    body.trim_end().to_string()
}

fn strip_one_leading_space(value: &str) -> &str {
    value.strip_prefix(' ').unwrap_or(value)
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

fn is_param_name(canonical: &str) -> bool {
    canonical == "param" || canonical == "parameter"
}

fn normalize_canonical(name: &str) -> String {
    match name {
        "returns" | "retval" | "result" => "return".to_string(),
        "parameter" => "param".to_string(),
        other => other.to_string(),
    }
}

fn split_first_token(value: &str) -> Option<(String, String)> {
    let trimmed = value.trim_start();
    if trimmed.is_empty() {
        return None;
    }
    let bytes = trimmed.as_bytes();
    if !is_ident_start(bytes[0]) {
        return None;
    }
    let mut end = 1usize;
    while end < bytes.len() && is_ident_continue(bytes[end]) {
        end += 1;
    }
    let name = trimmed[..end].to_string();
    let rest = trimmed[end..].trim_start().to_string();
    Some((name, rest))
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

fn trim_trailing_empty_text(blocks: &mut Vec<CommentBlock>) {
    while let Some(CommentBlock::Text(text)) = blocks.last() {
        if text.lines.iter().all(|line| line.trim().is_empty()) {
            blocks.pop();
        } else {
            break;
        }
    }
    if let Some(CommentBlock::Text(text)) = blocks.last_mut() {
        while text.lines.last().is_some_and(|line| line.trim().is_empty()) {
            text.lines.pop();
        }
    }
}

fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .to_ascii_lowercase()
        .find(&needle.to_ascii_lowercase())
}

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_ident_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}
