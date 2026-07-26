use std::collections::BTreeMap;
use std::ops::Range as IndexRange;

use tower_lsp::lsp_types::{
    CompletionItem, CompletionItemKind, CompletionList, CompletionResponse, CompletionTextEdit,
    Position, Range, TextEdit,
};

use crate::store::views::GoImportablePackageRow;

const GO_IMPORT_COMPLETION_LIMIT: usize = 100;
const GO_IMPORT_CONTEXT_MAX_LINES: usize = 4_096;
const GO_IMPORT_CONTEXT_MAX_BYTES: usize = 256 * 1_024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GoImportCompletionContext {
    prefix: String,
    line: u32,
    path_start_character: u32,
    path_end_character: u32,
    cursor_character: u32,
}

#[derive(Debug)]
struct GoImportCompletionEntry {
    import_path: String,
    package_keys: Vec<String>,
}

impl GoImportCompletionEntry {
    fn contains_package_key(&self, package_key: &str) -> bool {
        self.package_keys
            .binary_search_by(|candidate| candidate.as_str().cmp(package_key))
            .is_ok()
    }
}

#[derive(Debug, Default)]
pub(super) struct GoImportCompletionTable {
    entries: Vec<GoImportCompletionEntry>,
}

impl GoImportCompletionTable {
    pub(super) fn build(rows: Vec<GoImportablePackageRow>) -> Self {
        let mut by_path = BTreeMap::<String, Vec<String>>::new();
        for row in rows
            .into_iter()
            .filter(|row| !row.import_path.is_empty() && row.import_path != "C")
        {
            by_path
                .entry(row.import_path)
                .or_default()
                .push(package_key_identity(&row.package_key));
        }
        let entries = by_path
            .into_iter()
            .map(|(import_path, mut package_keys)| {
                package_keys.sort();
                package_keys.dedup();
                GoImportCompletionEntry {
                    import_path,
                    package_keys,
                }
            })
            .collect();
        Self { entries }
    }

    fn matching_range(&self, prefix: &str) -> IndexRange<usize> {
        let start = self
            .entries
            .partition_point(|entry| entry.import_path.as_str() < prefix);
        let end = start
            + self.entries[start..].partition_point(|entry| entry.import_path.starts_with(prefix));
        start..end
    }

    pub(super) fn complete(
        &self,
        context: &GoImportCompletionContext,
        current_package_key: Option<&str>,
    ) -> CompletionResponse {
        let mut items = Vec::new();
        let current_package_key = current_package_key.map(package_key_identity);
        for entry in &self.entries[self.matching_range(&context.prefix)] {
            if current_package_key
                .as_deref()
                .is_some_and(|current| entry.contains_package_key(current))
            {
                continue;
            }
            if items.len() == GO_IMPORT_COMPLETION_LIMIT {
                break;
            }
            items.push(CompletionItem {
                label: entry.import_path.clone(),
                kind: Some(CompletionItemKind::MODULE),
                detail: Some("Go package (indexed)".into()),
                sort_text: Some(entry.import_path.clone()),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                    range: Range::new(
                        Position::new(context.line, context.path_start_character),
                        Position::new(context.line, context.path_end_character),
                    ),
                    new_text: entry.import_path.clone(),
                })),
                ..Default::default()
            });
        }
        CompletionResponse::List(CompletionList {
            // Re-query as the import prefix changes. The table is bounded and
            // prefix-sensitive, like ordinary completion recall.
            is_incomplete: true,
            items,
        })
    }
}

fn package_key_identity(package_key: &str) -> String {
    #[cfg(windows)]
    {
        if let Some((path, package_name)) = package_key.rsplit_once('#') {
            return format!("{}#{package_name}", path.to_ascii_lowercase());
        }
    }
    package_key.to_string()
}

pub(super) fn go_import_completion_context(
    source: &str,
    line: u32,
    character: u32,
) -> Option<GoImportCompletionContext> {
    let (line_text, mut in_block_comment, mut in_import_block) =
        bounded_line_with_lexical_state(source, line)?;
    let cursor_byte = utf16_byte_offset(line_text, character);
    let (quote_byte, quote, path_end_byte) =
        import_string_at_cursor(line_text, cursor_byte, in_block_comment)?;
    let syntax_before_quote =
        code_without_comments(&line_text[..quote_byte], &mut in_block_comment);
    let direct_import = is_direct_import_prefix(&syntax_before_quote);
    if !in_import_block {
        in_import_block = starts_import_block(&syntax_before_quote);
    }
    if !direct_import && !in_import_block {
        return None;
    }

    let path_start_byte = quote_byte + quote.len_utf8();
    let prefix = line_text.get(path_start_byte..cursor_byte)?;
    if prefix
        .chars()
        .any(|ch| ch == '"' || ch == '`' || ch == '\\' || ch.is_whitespace())
    {
        return None;
    }
    let path_start_character = line_text[..path_start_byte]
        .chars()
        .map(|ch| ch.len_utf16() as u32)
        .sum();
    let path_end_character = line_text[..path_end_byte]
        .chars()
        .map(|ch| ch.len_utf16() as u32)
        .sum();
    Some(GoImportCompletionContext {
        prefix: prefix.to_string(),
        line,
        path_start_character,
        path_end_character,
        cursor_character: character,
    })
}

pub(super) fn current_go_package_key(relative_path: &str, source: &str) -> Option<String> {
    let mut in_block_comment = false;
    let mut scanned_bytes = 0_usize;
    let mut package_name = None;
    for line in source.lines().take(256) {
        scanned_bytes = scanned_bytes.saturating_add(line.len().saturating_add(1));
        if scanned_bytes > GO_IMPORT_CONTEXT_MAX_BYTES {
            break;
        }
        let code = code_without_comments(line, &mut in_block_comment);
        let Some(rest) = code.trim().strip_prefix("package") else {
            continue;
        };
        if !rest.starts_with(char::is_whitespace) {
            continue;
        }
        package_name = rest
            .trim_start()
            .split(|ch: char| !ch.is_alphanumeric() && ch != '_')
            .next()
            .filter(|name| !name.is_empty())
            .map(str::to_string);
        if package_name.is_some() {
            break;
        }
    }
    let package_name = package_name?;
    let directory = relative_path
        .rsplit_once('/')
        .map(|(directory, _)| directory)
        .filter(|directory| !directory.is_empty())
        .unwrap_or(".");
    Some(format!("{directory}#{package_name}"))
}

fn is_direct_import_prefix(value: &str) -> bool {
    let value = value.trim_start();
    let Some(rest) = value.strip_prefix("import") else {
        return false;
    };
    rest.starts_with(char::is_whitespace) && !rest.trim_start().starts_with('(')
}

fn bounded_line_with_lexical_state(source: &str, line: u32) -> Option<(&str, bool, bool)> {
    if line as usize >= GO_IMPORT_CONTEXT_MAX_LINES {
        return None;
    }
    let mut in_block_comment = false;
    let mut in_import_block = false;
    let mut scanned_bytes = 0_usize;
    for (index, source_line) in source.lines().enumerate() {
        scanned_bytes = scanned_bytes.saturating_add(source_line.len().saturating_add(1));
        if scanned_bytes > GO_IMPORT_CONTEXT_MAX_BYTES {
            return None;
        }
        if index == line as usize {
            return Some((source_line, in_block_comment, in_import_block));
        }
        let code = code_without_comments(source_line, &mut in_block_comment);
        let code = code.trim();
        if !in_import_block && starts_import_block(code) {
            in_import_block = !code.contains(')');
        } else if in_import_block && code.contains(')') {
            in_import_block = false;
        }
    }
    None
}

fn starts_import_block(code: &str) -> bool {
    code.trim_start()
        .strip_prefix("import")
        .is_some_and(|rest| {
            rest.starts_with(char::is_whitespace) && rest.trim_start().starts_with('(')
        })
}

fn import_string_at_cursor(
    line: &str,
    cursor_byte: usize,
    mut in_block_comment: bool,
) -> Option<(usize, char, usize)> {
    #[derive(Clone, Copy)]
    enum StringState {
        None,
        Double { start: usize, escaped: bool },
        Raw { start: usize },
        Rune { escaped: bool },
    }

    let bytes = line.as_bytes();
    let mut state = StringState::None;
    let mut index = 0;
    while index < cursor_byte {
        if in_block_comment {
            if bytes.get(index..index + 2) == Some(b"*/") {
                in_block_comment = false;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        state = match state {
            StringState::None => {
                if bytes.get(index..index + 2) == Some(b"//") {
                    return None;
                }
                if bytes.get(index..index + 2) == Some(b"/*") {
                    in_block_comment = true;
                    index += 2;
                    continue;
                }
                match bytes[index] {
                    b'"' => StringState::Double {
                        start: index,
                        escaped: false,
                    },
                    b'`' => StringState::Raw { start: index },
                    b'\'' => StringState::Rune { escaped: false },
                    _ => StringState::None,
                }
            }
            StringState::Double { start, escaped } => {
                if escaped {
                    StringState::Double {
                        start,
                        escaped: false,
                    }
                } else {
                    match bytes[index] {
                        b'\\' => StringState::Double {
                            start,
                            escaped: true,
                        },
                        b'"' => StringState::None,
                        _ => StringState::Double {
                            start,
                            escaped: false,
                        },
                    }
                }
            }
            StringState::Raw { start } => {
                if bytes[index] == b'`' {
                    StringState::None
                } else {
                    StringState::Raw { start }
                }
            }
            StringState::Rune { escaped } => {
                if escaped {
                    StringState::Rune { escaped: false }
                } else {
                    match bytes[index] {
                        b'\\' => StringState::Rune { escaped: true },
                        b'\'' => StringState::None,
                        _ => StringState::Rune { escaped: false },
                    }
                }
            }
        };
        index += 1;
    }
    let (start, quote) = match state {
        StringState::Double { start, .. } => (start, '"'),
        StringState::Raw { start } => (start, '`'),
        StringState::None | StringState::Rune { .. } => return None,
    };
    let end = string_content_end(line, cursor_byte, quote).unwrap_or(cursor_byte);
    Some((start, quote, end))
}

fn string_content_end(line: &str, start: usize, quote: char) -> Option<usize> {
    let bytes = line.as_bytes();
    let quote = quote as u8;
    let mut escaped = false;
    for (index, byte) in bytes.iter().copied().enumerate().skip(start) {
        if quote == b'"' && escaped {
            escaped = false;
            continue;
        }
        if quote == b'"' && byte == b'\\' {
            escaped = true;
            continue;
        }
        if byte == quote {
            return Some(index);
        }
    }
    None
}

fn code_without_comments(line: &str, in_block_comment: &mut bool) -> String {
    let bytes = line.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    let mut quote = None;
    let mut escaped = false;
    while index < bytes.len() {
        if *in_block_comment {
            if bytes.get(index..index + 2) == Some(b"*/") {
                *in_block_comment = false;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if let Some(delimiter) = quote {
            output.push(bytes[index]);
            if delimiter == b'"' && escaped {
                escaped = false;
            } else if delimiter == b'"' && bytes[index] == b'\\' {
                escaped = true;
            } else if bytes[index] == delimiter {
                quote = None;
            }
            index += 1;
            continue;
        }
        if bytes.get(index..index + 2) == Some(b"//") {
            break;
        }
        if bytes.get(index..index + 2) == Some(b"/*") {
            if output.last().copied() != Some(b' ') {
                output.push(b' ');
            }
            *in_block_comment = true;
            index += 2;
            continue;
        }
        if matches!(bytes[index], b'"' | b'`' | b'\'') {
            quote = Some(bytes[index]);
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn utf16_byte_offset(value: &str, character: u32) -> usize {
    let mut utf16 = 0_u32;
    for (byte, ch) in value.char_indices() {
        let next = utf16.saturating_add(ch.len_utf16() as u32);
        if next > character {
            return byte;
        }
        utf16 = next;
    }
    value.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context_option_at_marker(source: &str) -> Option<GoImportCompletionContext> {
        let marker = "/*cursor*/";
        let byte = source.find(marker).expect("cursor marker");
        let clean = source.replacen(marker, "", 1);
        let before = &clean[..byte];
        let line = before.bytes().filter(|byte| *byte == b'\n').count() as u32;
        let line_start = before.rfind('\n').map_or(0, |index| index + 1);
        let character = before[line_start..]
            .chars()
            .map(|ch| ch.len_utf16() as u32)
            .sum();
        go_import_completion_context(&clean, line, character)
    }

    fn context_at_marker(source: &str) -> GoImportCompletionContext {
        context_option_at_marker(source).expect("import context")
    }

    fn import_row(package_key: &str, import_path: &str) -> GoImportablePackageRow {
        GoImportablePackageRow {
            package_key: package_key.into(),
            import_path: import_path.into(),
        }
    }

    #[test]
    fn detects_direct_and_grouped_go_import_strings() {
        let direct =
            go_import_completion_context("package main\nimport \"example.com/de\"\n", 1, 22)
                .expect("direct import");
        assert_eq!(direct.prefix, "example.com/de");

        let grouped = go_import_completion_context(
            "package main\nimport (\n  alias \"example.com/bo\"\n)\n",
            2,
            23,
        )
        .expect("grouped import");
        assert_eq!(grouped.prefix, "example.com/bo");
    }

    #[test]
    fn rejects_ordinary_go_strings() {
        assert!(go_import_completion_context(
            "package main\nvar endpoint = \"example.com/de\"\n",
            1,
            32,
        )
        .is_none());
    }

    #[test]
    fn rejects_line_and_block_comments_inside_grouped_imports() {
        for source in [
            "package main\nimport (\n  // \"example.com/de/*cursor*/\"\n)\n",
            "package main\nimport (\n  /* \"example.com/de/*cursor*/\" */\n)\n",
            "package main\n/* comment starts\nimport (\n  \"example.com/de/*cursor*/\"\n*/\n",
        ] {
            let marker = "/*cursor*/";
            let byte = source.find(marker).expect("cursor");
            let clean = source.replacen(marker, "", 1);
            let before = &clean[..byte];
            let line = before.bytes().filter(|byte| *byte == b'\n').count() as u32;
            let start = before.rfind('\n').map_or(0, |index| index + 1);
            let character = before[start..]
                .chars()
                .map(|ch| ch.len_utf16() as u32)
                .sum();
            assert!(
                go_import_completion_context(&clean, line, character).is_none(),
                "{source:?}"
            );
        }
    }

    #[test]
    fn inline_block_comments_separate_go_tokens_without_disabling_imports() {
        let direct =
            context_at_marker("package main\nimport/* note */\"example.com/de/*cursor*/\"\n");
        assert_eq!(direct.prefix, "example.com/de");

        let grouped =
            context_at_marker("package main\nimport/* note */(\n\"example.com/bo/*cursor*/\"\n)\n");
        assert_eq!(grouped.prefix, "example.com/bo");
        assert_eq!(
            current_go_package_key("device/use.go", "package/* note */device\n"),
            Some("device#device".into())
        );
        assert!(context_option_at_marker(
            "package main\nim/* comments cannot join tokens */port \"example.com/de/*cursor*/\"\n"
        )
        .is_none());
    }

    #[test]
    fn import_context_scan_stops_at_the_open_document_budget() {
        let mut source = String::from("package main\n");
        for _ in 0..4_097 {
            source.push_str("// padding\n");
        }
        source.push_str("import \"example.com/de/*cursor*/\"\n");
        assert!(context_option_at_marker(&source).is_none());
    }

    #[test]
    fn completion_replaces_the_whole_import_path_when_cursor_is_in_the_middle() {
        let context = context_at_marker("package main\nimport \"example.com/de/*cursor*/vice\"\n");
        let table =
            GoImportCompletionTable::build(vec![import_row("device#device", "example.com/device")]);
        let CompletionResponse::List(list) = table.complete(&context, None) else {
            panic!("completion list");
        };
        let CompletionTextEdit::Edit(edit) = list.items[0].text_edit.clone().expect("text edit")
        else {
            panic!("plain edit");
        };
        assert_eq!(edit.new_text, "example.com/device");
        assert!(
            edit.range.end.character > context.cursor_character,
            "the stale suffix must be replaced, not appended"
        );
    }

    #[test]
    fn prefix_range_uses_the_sorted_slice_and_self_package_is_filtered() {
        let mut rows = (0..10_000)
            .map(|index| {
                import_row(
                    &format!("pkg/{index}#pkg"),
                    &format!("example.com/{index:05}"),
                )
            })
            .collect::<Vec<_>>();
        rows.push(import_row("zeta#zeta", "zzzz.example/zeta"));
        rows.push(import_row("self#self", "zzzz.example/self"));
        rows.push(import_row("conflict#other", "zzzz.example/self"));
        let table = GoImportCompletionTable::build(rows);
        let range = table.matching_range("zzzz.example/");
        assert!(
            range.start >= 10_000,
            "late prefixes must use a binary lower bound"
        );
        assert_eq!(range.len(), 2);
        let self_entry = table
            .entries
            .iter()
            .find(|entry| entry.import_path == "zzzz.example/self")
            .expect("self entry");
        assert!(self_entry.contains_package_key("self#self"));

        let context = context_at_marker("package self\nimport \"zzzz.example/*cursor*/\"\n");
        let CompletionResponse::List(list) = table.complete(&context, Some("self#self")) else {
            panic!("completion list");
        };
        assert_eq!(
            list.items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            vec!["zzzz.example/zeta"]
        );
        assert!(table.matching_range("zzzz.missing/").is_empty());
    }
}
