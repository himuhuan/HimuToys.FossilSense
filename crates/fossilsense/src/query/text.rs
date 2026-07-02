/// Extract the identifier at an LSP position within a single line. `character`
/// is a UTF-16 code-unit offset (per the LSP spec); identifiers are ASCII so
/// once mapped to a char index we expand over `[A-Za-z0-9_]`.
pub fn word_at(line_text: &str, character: u32) -> Option<String> {
    let chars: Vec<char> = line_text.chars().collect();

    // Map the UTF-16 column to a char index.
    let target = char_index_at_utf16(&chars, character);

    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';

    // The cursor may sit on the identifier or just past its end.
    let (mut start, mut end) = if target < chars.len() && is_ident(chars[target]) {
        (target, target)
    } else if target > 0 && is_ident(chars[target - 1]) {
        (target - 1, target - 1)
    } else {
        return None;
    };

    while start > 0 && is_ident(chars[start - 1]) {
        start -= 1;
    }
    while end + 1 < chars.len() && is_ident(chars[end + 1]) {
        end += 1;
    }

    let word: String = chars[start..=end].iter().collect();
    if word
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
    {
        Some(word)
    } else {
        None
    }
}

/// Extract the identifier prefix immediately before an LSP cursor position.
///
/// Unlike `word_at`, this intentionally does not expand through characters
/// after the cursor; completion should search for what the user has typed.
pub fn completion_prefix_at(line_text: &str, character: u32) -> Option<String> {
    let chars: Vec<char> = line_text.chars().collect();
    let target = char_index_at_utf16(&chars, character);
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';

    if target == 0 || !is_ident(chars[target - 1]) {
        return None;
    }

    let mut start = target - 1;
    while start > 0 && is_ident(chars[start - 1]) {
        start -= 1;
    }

    let word: String = chars[start..target].iter().collect();
    if word
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
    {
        Some(word)
    } else {
        None
    }
}

/// Whether the completion prefix is immediately after `.` or `->`.
pub fn is_member_completion_context(line_text: &str, character: u32) -> bool {
    let chars: Vec<char> = line_text.chars().collect();
    let target = char_index_at_utf16(&chars, character);
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';

    let mut start = target;
    while start > 0 && is_ident(chars[start - 1]) {
        start -= 1;
    }

    if start == 0 {
        return false;
    }
    if chars[start - 1] == '.' {
        return true;
    }
    start >= 2 && chars[start - 2] == '-' && chars[start - 1] == '>'
}

/// The single-identifier receiver immediately before the `.`/`->` at the cursor.
///
/// Returns `None` for receivers we will not try to type-infer: a call result
/// (`get()->`), a chained/multi-segment access (`a.b.`), or an indexed
/// expression (`arr[i].`). Those degrade to the global field fallback.
pub fn member_receiver_name(line_text: &str, character: u32) -> Option<String> {
    let chars: Vec<char> = line_text.chars().collect();
    let target = char_index_at_utf16(&chars, character);
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';

    // Walk back over the in-progress field prefix to the access operator.
    let mut start = target;
    while start > 0 && is_ident(chars[start - 1]) {
        start -= 1;
    }
    let op_pos = if start >= 1 && chars[start - 1] == '.' {
        start - 1
    } else if start >= 2 && chars[start - 2] == '-' && chars[start - 1] == '>' {
        start - 2
    } else {
        return None;
    };

    if op_pos == 0 || !is_ident(chars[op_pos - 1]) {
        return None;
    }
    let mut rstart = op_pos;
    while rstart > 0 && is_ident(chars[rstart - 1]) {
        rstart -= 1;
    }
    // Reject chained/call/indexed receivers: only a bare identifier qualifies.
    if rstart > 0 && matches!(chars[rstart - 1], '.' | '>' | ')' | ']') {
        return None;
    }

    let recv: String = chars[rstart..op_pos].iter().collect();
    if recv
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
    {
        Some(recv)
    } else {
        None
    }
}

/// Byte offset of an LSP position (line + UTF-16 column) within `text`.
pub fn byte_offset_at(text: &str, line: u32, character: u32) -> usize {
    let mut offset = 0usize;
    for (index, line_text) in text.split_inclusive('\n').enumerate() {
        if index as u32 == line {
            let chars: Vec<char> = line_text.chars().collect();
            let idx = char_index_at_utf16(&chars, character);
            let byte_in_line: usize = chars[..idx].iter().map(|ch| ch.len_utf8()).sum();
            return offset + byte_in_line;
        }
        offset += line_text.len();
    }
    text.len()
}

fn char_index_at_utf16(chars: &[char], character: u32) -> usize {
    let mut units = 0;
    for (idx, ch) in chars.iter().enumerate() {
        if units >= character {
            return idx;
        }
        units += ch.len_utf16() as u32;
        if units > character {
            return idx;
        }
    }
    chars.len()
}

pub(super) fn is_boundary(bytes: &[u8], i: usize) -> bool {
    if i == 0 {
        return true;
    }
    let prev = bytes[i - 1];
    let cur = bytes[i];
    (prev == b'_' && cur != b'_')
        || (prev.is_ascii_lowercase() && cur.is_ascii_uppercase())
        || (prev.is_ascii_alphabetic() && cur.is_ascii_digit())
}

/// Score a current-file word completion candidate with the same short-prefix
/// recall gate used for indexed symbols. Local words keep their simpler
/// exact/prefix/substring tiers, but 1-2 character prefixes reject plain
/// substrings unless the match starts at an identifier word boundary.
pub fn completion_word_score(prefix: &str, word: &str, locality_bonus: i32) -> Option<i32> {
    let needle = prefix.to_ascii_lowercase();
    let hay = word.to_ascii_lowercase();
    if needle.len() < super::MIN_PREFIX_LEN || hay.len() < super::MIN_PREFIX_LEN {
        return None;
    }

    let exact = hay == needle;
    if exact {
        return Some(700 + locality_bonus);
    }

    let starts = hay.starts_with(&needle);
    if starts {
        return Some(550 + locality_bonus);
    }

    let at = hay.find(&needle)?;
    if needle.len() < super::SHORT_PREFIX_MIN_LEN && !is_boundary(word.as_bytes(), at) {
        return None;
    }
    Some(250 + locality_bonus)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_at_finds_identifier() {
        let line = "    return hello_value();";
        // cursor inside "hello_value"
        let col = line.find("hello_value").unwrap() as u32 + 3;
        assert_eq!(word_at(line, col).as_deref(), Some("hello_value"));
    }

    #[test]
    fn word_at_handles_cursor_after_word() {
        let line = "int main";
        assert_eq!(word_at(line, 8).as_deref(), Some("main"));
    }

    #[test]
    fn word_at_returns_none_on_whitespace() {
        let line = "a   b";
        assert_eq!(word_at(line, 2), None);
    }

    #[test]
    fn word_at_rejects_number_literals() {
        assert_eq!(word_at("return 42;", 8), None);
        assert_eq!(word_at("return foo42;", 11).as_deref(), Some("foo42"));
        assert_eq!(word_at("_value = 1;", 0).as_deref(), Some("_value"));
    }

    #[test]
    fn word_at_extracts_single_char_identifier() {
        assert_eq!(word_at("x = 1;", 0).as_deref(), Some("x"));
    }

    #[test]
    fn completion_prefix_only_uses_text_before_cursor() {
        assert_eq!(
            completion_prefix_at("hello_value", 2).as_deref(),
            Some("he")
        );
        assert_eq!(
            completion_prefix_at("return hello_value();", 12).as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn completion_prefix_handles_cursor_after_word() {
        assert_eq!(completion_prefix_at("int main", 8).as_deref(), Some("main"));
        assert_eq!(completion_prefix_at("a   b", 2), None);
        assert_eq!(completion_prefix_at("return 42", 9), None);
    }

    #[test]
    fn member_completion_context_detects_dot_and_arrow() {
        assert!(is_member_completion_context("object.me", 9));
        assert!(is_member_completion_context("object->me", 10));
        assert!(is_member_completion_context("object.", 7));
        assert!(is_member_completion_context("object->", 8));
        assert!(!is_member_completion_context("global_me", 9));
    }

    #[test]
    fn member_receiver_name_extracts_simple_receiver() {
        // `obj.fie<cursor>` -> receiver "obj".
        assert_eq!(member_receiver_name("obj.fie", 7).as_deref(), Some("obj"));
        // `ptr->v<cursor>` -> receiver "ptr".
        assert_eq!(member_receiver_name("ptr->v", 6).as_deref(), Some("ptr"));
        // Right after the operator, before typing anything.
        assert_eq!(member_receiver_name("ptr->", 5).as_deref(), Some("ptr"));
        assert_eq!(member_receiver_name("obj.", 4).as_deref(), Some("obj"));
    }

    #[test]
    fn member_receiver_name_rejects_call_chain_and_index() {
        // Call result: `get()->v` -> no simple receiver.
        assert_eq!(member_receiver_name("get()->v", 8), None);
        // Multi-segment chain: `a.b.c` -> receiver before the second `.` is `b`,
        // but it is itself preceded by `.`, so we decline to infer.
        assert_eq!(member_receiver_name("a.b.c", 5), None);
        // Indexed: `arr[i].x` -> no simple receiver.
        assert_eq!(member_receiver_name("arr[i].x", 8), None);
        // Not a member context at all.
        assert_eq!(member_receiver_name("plain", 5), None);
    }

    #[test]
    fn byte_offset_at_maps_line_and_column() {
        let text = "ab\ncd->e\n";
        // Line 1, column 2 -> just after "cd".
        assert_eq!(byte_offset_at(text, 1, 2), 5);
        assert_eq!(&text[..byte_offset_at(text, 1, 2)], "ab\ncd");
    }

    #[test]
    fn local_word_short_prefix_rejects_plain_substring() {
        let bonus = super::super::COMPLETION_LOCALITY_BONUS;

        assert!(completion_word_score("fo", "Foobar", bonus).is_some());
        assert!(
            completion_word_score("ba", "FooBar", bonus).is_some(),
            "boundary substring should survive at len 2"
        );
        assert!(
            completion_word_score("ba", "Foobar", bonus).is_none(),
            "plain substring should be dropped at len 2"
        );
        assert!(
            completion_word_score("oba", "Foobar", bonus).is_some(),
            "len >= 3 keeps the existing local substring recall"
        );
    }
}
