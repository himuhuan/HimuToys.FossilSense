//! Handler-chain Markdown rendering for structured comment documents.

use super::model::{
    CommentBlock, CommentDocument, CommentRenderOptions, RenderedComment, TagBlock, TextBlock,
};

pub fn render_document(
    document: &CommentDocument,
    options: &CommentRenderOptions,
) -> Option<RenderedComment> {
    if document.blocks.is_empty() {
        return None;
    }

    let handlers: [&dyn TagRenderer; 3] = [
        &ParameterTagRenderer,
        &ReturnTagRenderer,
        &FallbackTagRenderer,
    ];

    let mut writer = MarkdownWriter::new(options.max_chars);
    let mut diagnostics = document.diagnostics.clone();
    let mut index = 0usize;

    while index < document.blocks.len() {
        match &document.blocks[index] {
            CommentBlock::Text(text) => {
                if !writer.write_text_block(text) {
                    diagnostics.truncated = true;
                    break;
                }
                index += 1;
            }
            CommentBlock::Tag(tag) => {
                let handler = handlers
                    .iter()
                    .find(|handler| handler.can_handle(tag))
                    .expect("fallback tag renderer must always match");
                let consumed = handler.consume_run(&document.blocks, index);
                let run = &document.blocks[index..index + consumed];
                if !handler.render_run(run, &mut writer) {
                    diagnostics.truncated = true;
                    break;
                }
                index += consumed;
            }
        }
    }

    if diagnostics.truncated {
        writer.mark_truncated();
    }
    let writer_truncated = writer.truncated;
    let markdown = writer.finish();
    if markdown.trim().is_empty() {
        return None;
    }
    if writer_truncated {
        diagnostics.truncated = true;
    }
    Some(RenderedComment {
        markdown,
        diagnostics,
    })
}

trait TagRenderer {
    fn can_handle(&self, tag: &TagBlock) -> bool;
    fn consume_run(&self, blocks: &[CommentBlock], start: usize) -> usize {
        let _ = (blocks, start);
        1
    }
    fn render_run(&self, blocks: &[CommentBlock], writer: &mut MarkdownWriter) -> bool;
}

struct ParameterTagRenderer;
struct ReturnTagRenderer;
struct FallbackTagRenderer;

impl TagRenderer for ParameterTagRenderer {
    fn can_handle(&self, tag: &TagBlock) -> bool {
        tag.canonical_name == "param"
    }

    fn consume_run(&self, blocks: &[CommentBlock], start: usize) -> usize {
        let mut count = 0usize;
        for block in &blocks[start..] {
            match block {
                CommentBlock::Tag(tag) if tag.canonical_name == "param" => count += 1,
                _ => break,
            }
        }
        count.max(1)
    }

    fn render_run(&self, blocks: &[CommentBlock], writer: &mut MarkdownWriter) -> bool {
        if !writer.write_heading("Parameters") {
            return false;
        }
        for block in blocks {
            let CommentBlock::Tag(tag) = block else {
                continue;
            };
            let name = tag
                .attributes
                .iter()
                .find(|attr| attr.name == "name")
                .map(|attr| attr.value.as_str())
                .unwrap_or("?");
            let direction = tag
                .attributes
                .iter()
                .find(|attr| attr.name == "direction")
                .map(|attr| attr.value.as_str());
            if !writer.write_parameter_item(name, direction, &tag.lines) {
                return false;
            }
        }
        true
    }
}

impl TagRenderer for ReturnTagRenderer {
    fn can_handle(&self, tag: &TagBlock) -> bool {
        tag.canonical_name == "return"
    }

    fn render_run(&self, blocks: &[CommentBlock], writer: &mut MarkdownWriter) -> bool {
        let Some(CommentBlock::Tag(tag)) = blocks.first() else {
            return true;
        };
        if !writer.write_heading("Returns") {
            return false;
        }
        writer.write_prose_lines(&tag.lines)
    }
}

impl TagRenderer for FallbackTagRenderer {
    fn can_handle(&self, _tag: &TagBlock) -> bool {
        true
    }

    fn render_run(&self, blocks: &[CommentBlock], writer: &mut MarkdownWriter) -> bool {
        let Some(CommentBlock::Tag(tag)) = blocks.first() else {
            return true;
        };
        let heading = titleize_tag(&tag.canonical_name);
        if !writer.write_heading(&heading) {
            return false;
        }
        writer.write_prose_lines(&tag.lines)
    }
}

pub struct MarkdownWriter {
    out: String,
    max_chars: usize,
    truncated: bool,
    needs_block_gap: bool,
}

impl MarkdownWriter {
    fn new(max_chars: usize) -> Self {
        Self {
            out: String::new(),
            max_chars,
            truncated: false,
            needs_block_gap: false,
        }
    }

    fn finish(mut self) -> String {
        if self.truncated {
            self.append_ellipsis();
        }
        self.out
    }

    fn mark_truncated(&mut self) {
        self.truncated = true;
    }

    fn write_heading(&mut self, title: &str) -> bool {
        if !self.ensure_block_gap() {
            return false;
        }
        let line = format!("### {title}");
        if !self.push_raw(&line) {
            return false;
        }
        self.push_raw("\n\n");
        self.needs_block_gap = false;
        true
    }

    fn write_text_block(&mut self, text: &TextBlock) -> bool {
        if !self.ensure_block_gap() {
            return false;
        }
        let ok = self.write_prose_lines(&text.lines);
        self.needs_block_gap = true;
        ok
    }

    fn write_parameter_item(
        &mut self,
        name: &str,
        direction: Option<&str>,
        lines: &[String],
    ) -> bool {
        let mut head = format!("- `{}`", escape_code_span(name));
        if let Some(direction) = direction.filter(|value| !value.is_empty()) {
            head.push_str(" *(");
            head.push_str(&escape_markdown_text(direction));
            head.push_str(")*");
        }
        let first = lines.first().map(String::as_str).unwrap_or("").trim();
        if !first.is_empty() {
            head.push_str(" — ");
            head.push_str(&escape_markdown_text(first));
        }
        if !self.push_line_with_optional_hard_break(&head, lines.len() > 1) {
            return false;
        }
        for (index, line) in lines.iter().enumerate().skip(1) {
            let is_last = index + 1 == lines.len();
            if line.is_empty() {
                // Preserve paragraph-like gaps inside a list item as hard breaks.
                if !self.push_line_with_optional_hard_break("  ", !is_last) {
                    return false;
                }
                continue;
            }
            let continued = format!("  {}", escape_markdown_text(line));
            if !self.push_line_with_optional_hard_break(&continued, !is_last) {
                return false;
            }
        }
        if !lines.is_empty() {
            // End the item with a normal newline (already emitted). Ensure next item starts cleanly.
        }
        self.needs_block_gap = true;
        true
    }

    fn write_prose_lines(&mut self, lines: &[String]) -> bool {
        if lines.is_empty() {
            self.needs_block_gap = true;
            return true;
        }
        let mut index = 0usize;
        while index < lines.len() {
            if lines[index].is_empty() {
                if !self.push_raw("\n") {
                    return false;
                }
                index += 1;
                continue;
            }
            // Gather a visual paragraph of consecutive non-empty lines.
            let mut paragraph = Vec::new();
            while index < lines.len() && !lines[index].is_empty() {
                paragraph.push(lines[index].as_str());
                index += 1;
            }
            for (offset, line) in paragraph.iter().enumerate() {
                let is_last = offset + 1 == paragraph.len();
                if !self.push_prose_line(line, !is_last) {
                    return false;
                }
            }
            if index < lines.len() && lines[index].is_empty() {
                // Paragraph boundary: blank line in source.
                if !self.push_raw("\n") {
                    return false;
                }
            }
        }
        self.needs_block_gap = true;
        true
    }

    fn push_line_with_optional_hard_break(&mut self, line: &str, hard_break: bool) -> bool {
        if hard_break {
            let mut with_break = line.to_string();
            with_break.push_str("  ");
            if !self.push_raw(&with_break) {
                return false;
            }
            self.push_raw("\n")
        } else {
            if !self.push_raw(line) {
                return false;
            }
            self.push_raw("\n")
        }
    }

    fn push_prose_line(&mut self, line: &str, hard_break: bool) -> bool {
        let escaped = escape_markdown_text(line);
        let suffix = if hard_break { "  \n" } else { "\n" };
        let complete = format!("{escaped}{suffix}");
        if self.push_raw(&complete) {
            return true;
        }

        // Preserve as much of the current prose line as possible. The prefix
        // helper avoids leaving a dangling Markdown escape or partial HTML
        // entity; finish() adds the visible omission marker.
        let remaining = self.max_chars.saturating_sub(self.out.chars().count());
        let prefix = safe_markdown_prefix(&escaped, remaining.saturating_sub(1));
        self.out.push_str(&prefix);
        false
    }

    fn ensure_block_gap(&mut self) -> bool {
        if !self.needs_block_gap || self.out.is_empty() {
            return true;
        }
        if self.out.ends_with("\n\n") {
            return true;
        }
        if self.out.ends_with('\n') {
            self.push_raw("\n")
        } else {
            self.push_raw("\n\n")
        }
    }

    fn append_ellipsis(&mut self) {
        if self.max_chars == 0 {
            return;
        }
        while self.out.ends_with(char::is_whitespace) {
            self.out.pop();
        }

        let preferred_gap = if self.out.is_empty() { "" } else { "\n\n" };
        let preferred_len = preferred_gap.chars().count() + 1;
        if self.out.chars().count() + preferred_len <= self.max_chars {
            self.out.push_str(preferred_gap);
            self.out.push('…');
            return;
        }

        while self.out.chars().count() + 1 > self.max_chars {
            self.out.pop();
        }
        while self.out.ends_with(char::is_whitespace) {
            self.out.pop();
        }
        self.out.push('…');
    }

    fn push_raw(&mut self, value: &str) -> bool {
        if self.truncated {
            return false;
        }
        let remaining = self.max_chars.saturating_sub(self.out.chars().count());
        if value.chars().count() <= remaining {
            self.out.push_str(value);
            return true;
        }
        // Truncate on a block boundary: do not slice mid-token into the buffer.
        self.truncated = true;
        false
    }
}

fn titleize_tag(canonical: &str) -> String {
    let mut chars = canonical.chars();
    let Some(first) = chars.next() else {
        return "Tag".to_string();
    };
    let mut out = first.to_ascii_uppercase().to_string();
    out.extend(chars);
    out
}

fn escape_markdown_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let list_marker = markdown_list_marker_byte(value);
    for (index, ch) in value.char_indices() {
        if list_marker == Some(index) {
            out.push('\\');
        }
        match ch {
            '`' => out.push('\''),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '\\' | '#' | '*' | '_' | '[' | ']' | '!' | '|' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    // Neutralize accidental fence openers after backtick rewriting.
    out.replace("'''", "''′")
}

fn markdown_list_marker_byte(value: &str) -> Option<usize> {
    let bytes = value.as_bytes();
    let indent = bytes.iter().take_while(|byte| **byte == b' ').count();
    if indent > 3 || indent >= bytes.len() {
        return None;
    }
    let marker = bytes[indent];
    if matches!(marker, b'-' | b'+')
        && bytes
            .get(indent + 1)
            .is_some_and(|byte| byte.is_ascii_whitespace())
    {
        return Some(indent);
    }
    if !marker.is_ascii_digit() {
        return None;
    }
    let mut cursor = indent + 1;
    while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
        cursor += 1;
    }
    if matches!(bytes.get(cursor), Some(b'.' | b')'))
        && bytes
            .get(cursor + 1)
            .is_some_and(|byte| byte.is_ascii_whitespace())
    {
        Some(cursor)
    } else {
        None
    }
}

fn safe_markdown_prefix(value: &str, max_chars: usize) -> String {
    let mut prefix: String = value.chars().take(max_chars).collect();
    if prefix.ends_with('\\') {
        prefix.pop();
    }
    if let Some(amp) = prefix.rfind('&') {
        if !prefix[amp..].contains(';') {
            prefix.truncate(amp);
        }
    }
    prefix
}

fn escape_code_span(value: &str) -> String {
    value.replace('`', "'")
}
