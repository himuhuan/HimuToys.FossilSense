use std::collections::HashSet;

const CPP_KEYWORDS: &[&str] = &[
    "auto",
    "break",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "continue",
    "default",
    "delete",
    "do",
    "double",
    "dynamic_cast",
    "else",
    "enum",
    "explicit",
    "extern",
    "false",
    "float",
    "for",
    "friend",
    "goto",
    "if",
    "inline",
    "int",
    "long",
    "mutable",
    "namespace",
    "new",
    "operator",
    "private",
    "protected",
    "public",
    "register",
    "reinterpret_cast",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "static_cast",
    "struct",
    "switch",
    "template",
    "this",
    "throw",
    "true",
    "try",
    "typedef",
    "typeid",
    "typename",
    "union",
    "unsigned",
    "using",
    "virtual",
    "void",
    "volatile",
    "while",
    "alignas",
    "alignof",
    "and",
    "and_eq",
    "asm",
    "atomic_cancel",
    "atomic_commit",
    "atomic_noexcept",
    "bitand",
    "bitor",
    "bool",
    "char16_t",
    "char32_t",
    "compl",
    "concept",
    "const_cast",
    "constexpr",
    "decltype",
    "not",
    "not_eq",
    "nullptr",
    "or",
    "or_eq",
    "register",
    "restrict",
    "static_assert",
    "thread_local",
    "wchar_t",
    "xor",
    "xor_eq",
    "override",
    "final",
    "NULL",
];

pub fn cpp_keywords() -> HashSet<&'static str> {
    CPP_KEYWORDS.iter().copied().collect()
}

pub fn extract_words(text: &str) -> HashSet<String> {
    let keywords = cpp_keywords();
    let mut words = HashSet::new();
    let mut chars = text.char_indices().peekable();
    let mut in_line_comment = false;
    let mut in_block_comment = false;

    while let Some(&(i, ch)) = chars.peek() {
        if in_line_comment {
            if ch == '\n' {
                in_line_comment = false;
            }
            chars.next();
            continue;
        }
        if in_block_comment {
            if ch == '*' && text[i..].starts_with("*/") {
                in_block_comment = false;
                chars.next();
                chars.next();
            } else {
                chars.next();
            }
            continue;
        }

        if ch == '/' {
            let rest = &text[i..];
            if rest.starts_with("//") {
                in_line_comment = true;
                chars.next();
                chars.next();
                continue;
            }
            if rest.starts_with("/*") {
                in_block_comment = true;
                chars.next();
                chars.next();
                continue;
            }
        }

        if ch == '"' {
            chars.next();
            while let Some(&(_, c)) = chars.peek() {
                if c == '\\' {
                    chars.next();
                    chars.next();
                    continue;
                }
                if c == '"' {
                    chars.next();
                    break;
                }
                chars.next();
            }
            continue;
        }

        if ch == '\'' {
            chars.next();
            while let Some(&(_, c)) = chars.peek() {
                if c == '\\' {
                    chars.next();
                    chars.next();
                    continue;
                }
                if c == '\'' {
                    chars.next();
                    break;
                }
                chars.next();
            }
            continue;
        }

        if ch.is_ascii_alphabetic() || ch == '_' {
            let start = i;
            while let Some(&(_, c)) = chars.peek() {
                if c.is_ascii_alphanumeric() || c == '_' {
                    chars.next();
                } else {
                    break;
                }
            }
            let word = &text[start..chars.peek().map(|(j, _)| *j).unwrap_or(text.len())];
            if !keywords.contains(word) {
                words.insert(word.to_string());
            }
            continue;
        }

        chars.next();
    }

    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_line_comments() {
        let words = extract_words("// some_helper_function\nint x;");
        assert!(!words.contains("some_helper_function"));
        assert!(words.contains("x"));
    }

    #[test]
    fn skips_block_comments() {
        let words = extract_words("/* helper_macro */ int y;");
        assert!(!words.contains("helper_macro"));
        assert!(words.contains("y"));
    }

    #[test]
    fn skips_string_literals() {
        let words = extract_words(r#"const char *s = "some_symbol_name"; int z;"#);
        assert!(!words.contains("some_symbol_name"));
        assert!(words.contains("z"));
    }

    #[test]
    fn skips_char_literals() {
        let words = extract_words("char c = 'x'; int w;");
        assert!(!words.contains("x"));
        assert!(words.contains("w"));
    }

    #[test]
    fn extracts_identifiers() {
        let words = extract_words("hello_value myFunc _private");
        assert!(words.contains("hello_value"));
        assert!(words.contains("myFunc"));
        assert!(words.contains("_private"));
    }

    #[test]
    fn filters_cpp_keywords() {
        let words = extract_words("int return if else while for struct class namespace");
        assert!(words.is_empty());
    }

    #[test]
    fn deduplicates_words() {
        let words = extract_words("helper helper helper");
        assert_eq!(words.len(), 1);
        assert!(words.contains("helper"));
    }

    #[test]
    fn handles_escaped_quotes_in_strings() {
        let words = extract_words(r#"char *s = "hello \"world\""; int a;"#);
        assert!(words.contains("a"));
        assert!(!words.contains("world"));
    }

    #[test]
    fn handles_escaped_chars_in_char_literals() {
        let words = extract_words(r#"char c = '\n'; int b;"#);
        assert!(words.contains("b"));
    }

    #[test]
    fn handles_multiline_block_comment() {
        let words = extract_words("int a; /* comment\nacross lines\nwith words */ int b;");
        assert!(words.contains("a"));
        assert!(words.contains("b"));
        assert!(!words.contains("comment"));
        assert!(!words.contains("across"));
        assert!(!words.contains("words"));
    }

    #[test]
    fn keyword_set_has_common_entries() {
        let kw = cpp_keywords();
        assert!(kw.contains("int"));
        assert!(kw.contains("return"));
        assert!(kw.contains("class"));
        assert!(kw.contains("struct"));
        assert!(kw.contains("namespace"));
        assert!(!kw.contains("main"));
    }

    #[test]
    fn extract_words_case_insensitive_prefix_match() {
        let words = extract_words("MyFunction myVariable MY_CONST");
        assert!(words.contains("MyFunction"));
        assert!(words.contains("myVariable"));
        assert!(words.contains("MY_CONST"));
    }

    #[test]
    fn extract_words_preserves_case() {
        let words = extract_words("getValue GetValue GET_VALUE");
        assert!(words.contains("getValue"));
        assert!(words.contains("GetValue"));
        assert!(words.contains("GET_VALUE"));
    }

    #[test]
    fn extract_words_skips_preprocessor_directives_content() {
        let words = extract_words("#define HELLO 1\nint x;");
        assert!(words.contains("HELLO"));
        assert!(words.contains("x"));
        assert!(!words.contains("1"));
    }
}
