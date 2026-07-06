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
    member_operator_before_prefix(&chars, start).is_some()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberAccessChain {
    pub receiver: String,
    pub completed_members: Vec<String>,
}

/// The single receiver name before the `.`/`->` at the cursor when the
/// expression can be reduced to one record-typed identifier. Calls and casts
/// still return `None` and degrade to the global member fallback.
#[allow(dead_code)]
pub fn member_receiver_name(line_text: &str, character: u32) -> Option<String> {
    member_access_chain_at(line_text, character)
        .and_then(|chain| chain.completed_members.is_empty().then_some(chain.receiver))
}

/// Extract a bounded C member chain before the current member prefix. Handles
/// simple identifiers plus common lvalue adornments such as array subscripts,
/// parentheses, and unary `*`/`&`: `a.b[i].`, `p->items[n]->`, `(*p).x.`.
/// Calls, casts, arithmetic, and macro-shaped expressions stay unsupported so
/// member completion remains a best-effort candidate lookup, not expression
/// type inference.
pub fn member_access_chain_at(line_text: &str, character: u32) -> Option<MemberAccessChain> {
    let chars: Vec<char> = line_text.chars().collect();
    let target = char_index_at_utf16(&chars, character);
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';

    // Walk back over the in-progress field prefix to the access operator.
    let mut start = target;
    while start > 0 && is_ident(chars[start - 1]) {
        start -= 1;
    }
    let (op_pos, _) = member_operator_before_prefix(&chars, start)?;

    for chain_start in 0..op_pos {
        if chain_start > 0
            && (is_ident(chars[chain_start - 1])
                || matches!(chars[chain_start - 1], '.' | '>' | ']' | ')'))
        {
            continue;
        }
        if let Some(chain) = parse_member_chain_expr(&chars[chain_start..op_pos]) {
            return Some(chain);
        }
    }
    None
}

fn member_operator_before_prefix(chars: &[char], prefix_start: usize) -> Option<(usize, usize)> {
    let mut op_end = prefix_start;
    while op_end > 0 && chars[op_end - 1].is_whitespace() {
        op_end -= 1;
    }
    if op_end >= 1 && chars[op_end - 1] == '.' {
        Some((op_end - 1, op_end))
    } else if op_end >= 2 && chars[op_end - 2] == '-' && chars[op_end - 1] == '>' {
        Some((op_end - 2, op_end))
    } else {
        None
    }
}

fn parse_member_chain_expr(chars: &[char]) -> Option<MemberAccessChain> {
    let mut cursor = 0usize;
    skip_ws(chars, &mut cursor);
    let chain = parse_chain_expr(chars, &mut cursor)?;
    skip_ws(chars, &mut cursor);
    (cursor == chars.len()).then_some(chain)
}

fn parse_chain_expr(chars: &[char], cursor: &mut usize) -> Option<MemberAccessChain> {
    skip_ws(chars, cursor);
    while *cursor < chars.len() && matches!(chars[*cursor], '*' | '&') {
        *cursor += 1;
        skip_ws(chars, cursor);
    }

    let mut chain = if *cursor < chars.len() && chars[*cursor] == '(' {
        *cursor += 1;
        let inner = parse_chain_expr(chars, cursor)?;
        skip_ws(chars, cursor);
        if chars.get(*cursor) != Some(&')') {
            return None;
        }
        *cursor += 1;
        inner
    } else {
        let receiver = parse_identifier(chars, cursor)?;
        MemberAccessChain {
            receiver,
            completed_members: Vec::new(),
        }
    };

    loop {
        skip_ws(chars, cursor);
        if chars.get(*cursor) == Some(&'[') {
            *cursor = skip_balanced(chars, *cursor, '[', ']')?;
            continue;
        }

        let member_op = if chars.get(*cursor) == Some(&'.') {
            *cursor += 1;
            true
        } else if chars.get(*cursor) == Some(&'-') && chars.get(*cursor + 1) == Some(&'>') {
            *cursor += 2;
            true
        } else {
            false
        };
        if !member_op {
            break;
        }

        skip_ws(chars, cursor);
        let member = parse_identifier(chars, cursor)?;
        chain.completed_members.push(member);
    }

    Some(chain)
}

fn parse_identifier(chars: &[char], cursor: &mut usize) -> Option<String> {
    let is_ident_start = |c: char| c.is_ascii_alphabetic() || c == '_';
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    if *cursor >= chars.len() || !is_ident_start(chars[*cursor]) {
        return None;
    }
    let start = *cursor;
    *cursor += 1;
    while *cursor < chars.len() && is_ident(chars[*cursor]) {
        *cursor += 1;
    }
    Some(chars[start..*cursor].iter().collect())
}

fn skip_ws(chars: &[char], cursor: &mut usize) {
    while *cursor < chars.len() && chars[*cursor].is_whitespace() {
        *cursor += 1;
    }
}

fn skip_balanced(chars: &[char], open_pos: usize, open: char, close: char) -> Option<usize> {
    if chars.get(open_pos) != Some(&open) {
        return None;
    }
    let mut depth = 1usize;
    let mut cursor = open_pos + 1;
    while cursor < chars.len() {
        match chars[cursor] {
            '"' | '\'' => cursor = skip_quoted(chars, cursor),
            '/' if chars.get(cursor + 1) == Some(&'/') => {
                cursor += 2;
                while cursor < chars.len() && chars[cursor] != '\n' {
                    cursor += 1;
                }
            }
            '/' if chars.get(cursor + 1) == Some(&'*') => {
                cursor += 2;
                while cursor + 1 < chars.len()
                    && !(chars[cursor] == '*' && chars[cursor + 1] == '/')
                {
                    cursor += 1;
                }
                cursor = (cursor + 2).min(chars.len());
            }
            ch if ch == open => {
                depth += 1;
                cursor += 1;
            }
            ch if ch == close => {
                depth = depth.saturating_sub(1);
                cursor += 1;
                if depth == 0 {
                    return Some(cursor);
                }
            }
            '(' => cursor = skip_balanced(chars, cursor, '(', ')')?,
            _ => cursor += 1,
        }
    }
    None
}

fn skip_quoted(chars: &[char], quote_start: usize) -> usize {
    let quote = chars[quote_start];
    let mut cursor = quote_start + 1;
    while cursor < chars.len() {
        if chars[cursor] == '\\' {
            cursor = (cursor + 2).min(chars.len());
            continue;
        }
        if chars[cursor] == quote {
            return cursor + 1;
        }
        cursor += 1;
    }
    chars.len()
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
        // Multi-segment chain has a chain parser, but the compatibility API
        // still exposes only a bare receiver.
        assert_eq!(member_receiver_name("a.b.c", 5), None);
        // Indexed receiver: `arr[i].x` resolves to the base receiver.
        assert_eq!(member_receiver_name("arr[i].x", 8).as_deref(), Some("arr"));
        // Not a member context at all.
        assert_eq!(member_receiver_name("plain", 5), None);
    }

    #[test]
    fn member_access_chain_extracts_supported_c_lvalue_segments() {
        assert_eq!(
            member_access_chain_at("void f(void) { a.mem1.xxx", 26),
            Some(MemberAccessChain {
                receiver: "a".to_string(),
                completed_members: vec!["mem1".to_string()],
            })
        );
        assert_eq!(
            member_access_chain_at("ptr->inner.value", 16),
            Some(MemberAccessChain {
                receiver: "ptr".to_string(),
                completed_members: vec!["inner".to_string()],
            })
        );
        assert_eq!(
            member_access_chain_at("a.mem1[n].xxx", 14),
            Some(MemberAccessChain {
                receiver: "a".to_string(),
                completed_members: vec!["mem1".to_string()],
            })
        );
        assert_eq!(
            member_access_chain_at("(*ptr).inner.value", 18),
            Some(MemberAccessChain {
                receiver: "ptr".to_string(),
                completed_members: vec!["inner".to_string()],
            })
        );
        assert_eq!(
            member_access_chain_at("arr[i].value", 12),
            Some(MemberAccessChain {
                receiver: "arr".to_string(),
                completed_members: Vec::new(),
            })
        );
        assert_eq!(member_access_chain_at("get()->value", 12), None);
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

        assert!(
            completion_word_score("l", "lerp3u16", bonus).is_some(),
            "single-char prefix should keep the initial completion session alive"
        );
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
