//! Best-effort comment attachment near a symbol declaration.
//!
//! The extractor only returns raw comment text and placement. It does not clean
//! markers or interpret Doxygen/XML tags.

use super::model::{
    CommentAnchor, CommentPlacement, CommentRenderOptions, CommentStyle, RawComment,
};

pub fn extract_comment(
    source: &str,
    anchor: &CommentAnchor,
    options: &CommentRenderOptions,
) -> Option<RawComment> {
    let lines: Vec<&str> = source.lines().collect();
    let cursor = (anchor.start_line as usize).min(lines.len());
    if cursor >= lines.len() && anchor.start_line as usize >= lines.len() {
        return None;
    }

    if let Some(raw) = extract_trailing_same_line(&lines, cursor, &anchor.symbol_name) {
        return Some(clamp_raw(raw, options));
    }
    if let Some(raw) = extract_inline_leading_same_line(&lines, cursor) {
        return Some(clamp_raw(raw, options));
    }
    if cursor > 0 {
        if let Some(raw) = extract_leading_above(&lines, cursor) {
            return Some(clamp_raw(raw, options));
        }
    }
    None
}

/// Compatibility path: recover a leading comment embedded in a stored signature.
pub fn extract_signature_fallback(signature: &str) -> Option<RawComment> {
    let trimmed = signature.trim_start();
    if trimmed.starts_with("/*") {
        let end = trimmed.find("*/")? + 2;
        let text = trimmed[..end].to_string();
        let style = comment_style_from_text(&text);
        return Some(RawComment {
            text,
            placement: CommentPlacement::SignatureFallback,
            style,
            start_line: 0,
            end_line: 0,
            truncated: false,
        });
    }
    if trimmed.starts_with("//") {
        let collected: Vec<&str> = trimmed
            .lines()
            .take_while(|line| line.trim_start().starts_with("//"))
            .collect();
        if collected.is_empty() {
            return None;
        }
        let text = collected.join("\n");
        let style = comment_style_from_text(&text);
        return Some(RawComment {
            text,
            placement: CommentPlacement::SignatureFallback,
            style,
            start_line: 0,
            end_line: collected.len().saturating_sub(1) as u32,
            truncated: false,
        });
    }
    None
}

fn clamp_raw(mut raw: RawComment, options: &CommentRenderOptions) -> RawComment {
    let line_count = raw.text.lines().count();
    if line_count > options.max_comment_lines {
        raw.truncated = true;
        raw.text = raw
            .text
            .lines()
            .take(options.max_comment_lines)
            .collect::<Vec<_>>()
            .join("\n");
        // Keep block comments syntactically closed after line clamping when possible.
        if matches!(raw.style, CommentStyle::Block | CommentStyle::DocBlock)
            && !raw.text.contains("*/")
        {
            raw.text.push_str("\n*/");
        }
    }
    raw
}

fn extract_trailing_same_line(
    lines: &[&str],
    cursor: usize,
    symbol_name: &str,
) -> Option<RawComment> {
    let line = *lines.get(cursor)?;
    let comment_start = find_trailing_comment_start(line)?;
    if !trailing_belongs_to_symbol(&line[..comment_start], symbol_name) {
        return None;
    }
    let text = line[comment_start..].trim_end().to_string();
    if text.is_empty() {
        return None;
    }
    Some(RawComment {
        style: comment_style_from_text(&text),
        placement: CommentPlacement::TrailingSameLine,
        text,
        start_line: cursor as u32,
        end_line: cursor as u32,
        truncated: false,
    })
}

fn extract_inline_leading_same_line(lines: &[&str], cursor: usize) -> Option<RawComment> {
    let line = *lines.get(cursor)?;
    let trimmed = line.trim_start();
    // Accept both `/** docs */ bool x;` and a closing `*/ bool x;` that continues a
    // block opened on earlier lines.
    if !(is_block_comment_boundary(trimmed) && trimmed.contains("*/")) {
        return None;
    }

    let mut first = cursor;
    while first > 0 && !lines[first].trim_start().starts_with("/*") {
        if !is_block_comment_scan_line(lines[first]) {
            return None;
        }
        first -= 1;
    }
    if !lines[first].trim_start().starts_with("/*") {
        return None;
    }
    // Reject cases where the opening line itself is a trailing comment on another
    // declaration (`int old; /* note */`).
    if line_has_code_before_block_comment(lines[first]) {
        return None;
    }

    let mut out: Vec<String> = lines[first..=cursor]
        .iter()
        .map(|line| (*line).to_string())
        .collect();
    if let Some(last) = out.last_mut() {
        if let Some(close) = last.find("*/") {
            // Keep only the leading block comment; code after `*/` belongs to the declaration.
            last.truncate(close + 2);
        }
    }
    let text = out.join("\n");
    Some(RawComment {
        style: comment_style_from_text(&text),
        placement: CommentPlacement::InlineLeadingSameLine,
        text,
        start_line: first as u32,
        end_line: cursor as u32,
        truncated: false,
    })
}

fn extract_leading_above(lines: &[&str], cursor: usize) -> Option<RawComment> {
    let previous = lines[cursor - 1].trim_start();
    if previous.is_empty() {
        return None;
    }
    if is_line_comment(previous) {
        let mut first = cursor - 1;
        while first > 0 && is_line_comment(lines[first - 1].trim_start()) {
            first -= 1;
        }
        let text = lines[first..cursor].join("\n");
        return Some(RawComment {
            style: comment_style_from_text(&text),
            placement: CommentPlacement::LeadingAbove,
            text,
            start_line: first as u32,
            end_line: (cursor - 1) as u32,
            truncated: false,
        });
    }
    // Only a real block-comment close can start an upward block scan. Treating
    // every `*...` line as a boundary confuses pointer dereferences such as
    // `*ptr = 1;` with comment margin lines.
    if is_block_comment_end(previous) {
        return collect_block_comment_group(lines, cursor - 1);
    }
    None
}

fn collect_block_comment_group(lines: &[&str], end: usize) -> Option<RawComment> {
    if !is_block_comment_end(lines[end]) {
        return None;
    }
    let mut first = end;
    while first > 0 && !lines[first].trim_start().starts_with("/*") {
        if !is_block_comment_scan_line(lines[first])
            || has_trailing_code_after_block_close(lines[first])
        {
            return None;
        }
        first -= 1;
    }
    if !lines[first].trim_start().starts_with("/*") {
        return None;
    }
    if has_trailing_code_after_block_close(lines[first]) {
        // The block closed on an earlier line with trailing code: not a leading group.
        return None;
    }
    // A closing `*/` on the line immediately above the symbol with trailing code
    // belongs to a previous declaration, not this symbol.
    if end + 1 < lines.len() && has_trailing_code_after_block_close(lines[end]) {
        return None;
    }
    // If the end line is only a block close with no code, and it closed a block
    // that had trailing code on an earlier line, collect_block already rejected.
    // Reject `int old; /* note */` style when scanning from the next symbol:
    // the previous line has code before `/*`.
    if line_has_code_before_block_comment(lines[end]) {
        return None;
    }
    // Walk upward: if the opening line has code before `/*`, this is not a pure
    // leading comment block.
    if line_has_code_before_block_comment(lines[first]) {
        return None;
    }

    let text = lines[first..=end].join("\n");
    Some(RawComment {
        style: comment_style_from_text(&text),
        placement: CommentPlacement::LeadingAbove,
        text,
        start_line: first as u32,
        end_line: end as u32,
        truncated: false,
    })
}

fn trailing_belongs_to_symbol(before_comment: &str, symbol_name: &str) -> bool {
    let Some(ident_end) = find_last_ident_end(before_comment, symbol_name) else {
        return false;
    };
    let after_ident = &before_comment[ident_end..];
    let Some(semi) = find_top_level_char(after_ident, ';') else {
        return false;
    };
    let between = &after_ident[..semi];
    if has_top_level_comma(between) {
        return false;
    }
    // Comma-separated declarators (`int left, right;`) are ambiguous even for
    // the final name: refuse whenever the whole statement has a top-level comma.
    let statement_start = before_comment[..ident_end]
        .rfind(';')
        .map(|index| index + 1)
        .unwrap_or(0);
    let statement = before_comment[statement_start..ident_end + semi + 1].trim_start();
    if has_top_level_comma(statement) {
        return false;
    }
    let after_semi = after_ident[semi + 1..].trim();
    after_semi.is_empty()
}

fn find_last_ident_end(text: &str, symbol_name: &str) -> Option<usize> {
    if symbol_name.is_empty() {
        return None;
    }
    let bytes = text.as_bytes();
    let name = symbol_name.as_bytes();
    let mut index = 0usize;
    let mut last = None;
    let mut in_str = false;
    let mut in_char = false;
    let mut escaped = false;
    while index < bytes.len() {
        let b = bytes[index];
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            index += 1;
            continue;
        }
        if in_char {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'\'' {
                in_char = false;
            }
            index += 1;
            continue;
        }
        match b {
            b'"' => {
                in_str = true;
                index += 1;
            }
            b'\'' => {
                in_char = true;
                index += 1;
            }
            _ if is_ident_start(b) => {
                let start = index;
                index += 1;
                while index < bytes.len() && is_ident_continue(bytes[index]) {
                    index += 1;
                }
                if &bytes[start..index] == name {
                    last = Some(index);
                }
            }
            _ => index += 1,
        }
    }
    last
}

fn find_trailing_comment_start(line: &str) -> Option<usize> {
    // Prefer a comment that follows a top-level declaration terminator so
    // inline-leading blocks like `/* docs */ int x; // trail` still leave the
    // trailing comment discoverable.
    let bytes = line.as_bytes();
    let mut index = 0usize;
    let mut in_str = false;
    let mut in_char = false;
    let mut escaped = false;
    let mut depth = 0i32;
    let mut last_top_level_semi: Option<usize> = None;
    let mut comment_after_semi: Option<usize> = None;
    let mut first_comment: Option<usize> = None;

    while index < bytes.len() {
        let b = bytes[index];
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            index += 1;
            continue;
        }
        if in_char {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'\'' {
                in_char = false;
            }
            index += 1;
            continue;
        }
        match b {
            b'"' => {
                in_str = true;
                index += 1;
            }
            b'\'' => {
                in_char = true;
                index += 1;
            }
            b'(' | b'[' | b'{' => {
                depth += 1;
                index += 1;
            }
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                index += 1;
            }
            b';' if depth == 0 => {
                last_top_level_semi = Some(index);
                index += 1;
            }
            b'/' if index + 1 < bytes.len()
                && (bytes[index + 1] == b'/' || bytes[index + 1] == b'*') =>
            {
                if first_comment.is_none() {
                    first_comment = Some(index);
                }
                if last_top_level_semi.is_some() {
                    comment_after_semi = Some(index);
                }
                if bytes[index + 1] == b'/' {
                    break;
                }
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                if index + 1 < bytes.len() {
                    index += 2;
                }
            }
            _ => index += 1,
        }
    }
    comment_after_semi.or(first_comment)
}

fn find_top_level_char(text: &str, target: char) -> Option<usize> {
    let mut depth = 0i32;
    for (index, ch) in text.char_indices() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            c if c == target && depth == 0 => return Some(index),
            _ => {}
        }
    }
    None
}

fn has_top_level_comma(text: &str) -> bool {
    find_top_level_char(text, ',').is_some()
}

fn comment_style_from_text(text: &str) -> CommentStyle {
    let trimmed = text.trim_start();
    if trimmed.starts_with("/**") || trimmed.starts_with("/*!") {
        CommentStyle::DocBlock
    } else if trimmed.starts_with("/*") {
        CommentStyle::Block
    } else if trimmed.starts_with("///") || trimmed.starts_with("//!") {
        CommentStyle::DocLine
    } else {
        CommentStyle::Line
    }
}

fn is_line_comment(line: &str) -> bool {
    line.starts_with("//")
}

fn is_block_comment_boundary(line: &str) -> bool {
    let trimmed = line.trim_start();
    !trimmed.is_empty() && (trimmed.starts_with("/*") || trimmed.starts_with('*'))
}

fn is_block_comment_end(line: &str) -> bool {
    let trimmed = line.trim_start();
    (trimmed.starts_with("/*") || trimmed.starts_with('*')) && trimmed.contains("*/")
}

fn is_block_comment_scan_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.is_empty() || trimmed.starts_with("/*") || trimmed.starts_with('*')
}

fn has_trailing_code_after_block_close(line: &str) -> bool {
    line.find("*/")
        .is_some_and(|index| !line[index + 2..].trim().is_empty())
}

fn line_has_code_before_block_comment(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("/*") {
        return false;
    }
    // Pure continuation lines start with `*`.
    if trimmed.starts_with('*') {
        return false;
    }
    // Any non-comment non-empty content before a block opener means trailing
    // code on a previous declaration line.
    find_trailing_comment_start(line).is_some_and(|index| {
        let before = line[..index].trim();
        !before.is_empty() && line[index..].trim_start().starts_with("/*")
    })
}

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_ident_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}
