#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompletionIntentKind {
    Neutral,
    TypeName,
    ExpressionValue,
    CallTarget,
    MacroPreprocessor,
    DeclarationName,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum CompletionIntentConfidence {
    Low,
    Medium,
    High,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CompletionIntent {
    pub kind: CompletionIntentKind,
    pub confidence: CompletionIntentConfidence,
}

impl Default for CompletionIntent {
    fn default() -> Self {
        Self {
            kind: CompletionIntentKind::Neutral,
            confidence: CompletionIntentConfidence::Low,
        }
    }
}

impl CompletionIntentKind {
    pub(crate) fn as_summary_str(self) -> &'static str {
        match self {
            CompletionIntentKind::Neutral => "neutral",
            CompletionIntentKind::TypeName => "type_name",
            CompletionIntentKind::ExpressionValue => "expression_value",
            CompletionIntentKind::CallTarget => "call_target",
            CompletionIntentKind::MacroPreprocessor => "macro_preprocessor",
            CompletionIntentKind::DeclarationName => "declaration_name",
        }
    }
}

impl CompletionIntentConfidence {
    pub(crate) fn as_summary_str(self) -> &'static str {
        match self {
            CompletionIntentConfidence::Low => "low",
            CompletionIntentConfidence::Medium => "medium",
            CompletionIntentConfidence::High => "high",
        }
    }
}

pub(crate) fn classify_completion_intent(
    line_text: &str,
    character: u32,
    prefix: &str,
) -> CompletionIntent {
    let cursor = byte_index_for_utf16_position(line_text, character);
    let before_cursor = &line_text[..cursor];
    let after_cursor = &line_text[cursor..];
    let before_prefix = before_cursor
        .strip_suffix(prefix)
        .unwrap_or(before_cursor)
        .trim_end();
    let trimmed_before = before_cursor.trim_start();

    if is_preprocessor_macro_context(trimmed_before) {
        return CompletionIntent {
            kind: CompletionIntentKind::MacroPreprocessor,
            confidence: CompletionIntentConfidence::High,
        };
    }

    if after_cursor.trim_start().starts_with('(') {
        return CompletionIntent {
            kind: CompletionIntentKind::CallTarget,
            confidence: CompletionIntentConfidence::High,
        };
    }

    let previous_token = previous_token(before_prefix);
    if previous_token.as_deref().is_some_and(is_type_intent_cue) {
        return CompletionIntent {
            kind: CompletionIntentKind::TypeName,
            confidence: CompletionIntentConfidence::High,
        };
    }

    if previous_token
        .as_deref()
        .is_some_and(is_pointer_or_reference)
        && typeish_token_before_pointer_or_reference(before_prefix)
    {
        return CompletionIntent {
            kind: CompletionIntentKind::DeclarationName,
            confidence: CompletionIntentConfidence::Medium,
        };
    }

    if previous_token
        .as_deref()
        .is_some_and(is_expression_intent_cue)
    {
        return CompletionIntent {
            kind: CompletionIntentKind::ExpressionValue,
            confidence: CompletionIntentConfidence::Medium,
        };
    }

    if previous_token
        .as_deref()
        .is_some_and(is_typeish_declaration_token)
    {
        return CompletionIntent {
            kind: CompletionIntentKind::DeclarationName,
            confidence: CompletionIntentConfidence::Medium,
        };
    }

    CompletionIntent::default()
}

fn byte_index_for_utf16_position(text: &str, character: u32) -> usize {
    let mut utf16_units = 0;
    for (byte_idx, ch) in text.char_indices() {
        if utf16_units >= character {
            return byte_idx;
        }
        utf16_units += ch.len_utf16() as u32;
    }
    text.len()
}

fn is_preprocessor_macro_context(trimmed_before: &str) -> bool {
    let Some(rest) = trimmed_before.strip_prefix('#') else {
        return false;
    };
    let directive = rest.split_whitespace().next().unwrap_or_default();
    matches!(directive, "if" | "ifdef" | "ifndef" | "elif" | "define")
}

fn previous_token(before_prefix: &str) -> Option<String> {
    let trimmed = before_prefix.trim_end();
    if trimmed.is_empty() {
        return None;
    }
    let end = trimmed.len();
    if end > 0 {
        let ch = trimmed[..end].chars().next_back()?;
        if ch.is_alphanumeric() || ch == '_' || ch == '*' || ch == '&' {
        } else {
            return Some(ch.to_string());
        }
    }
    let mut start = end;
    while start > 0 {
        let ch = trimmed[..start].chars().next_back()?;
        if ch.is_alphanumeric() || ch == '_' || ch == '*' || ch == '&' {
            start -= ch.len_utf8();
        } else {
            break;
        }
    }
    Some(trimmed[start..end].to_string())
}

fn is_type_intent_cue(token: &str) -> bool {
    matches!(
        token,
        "struct" | "union" | "enum" | "class" | "typedef" | "using" | "sizeof" | "new"
    )
}

fn is_expression_intent_cue(token: &str) -> bool {
    matches!(
        token,
        "=" | "return"
            | "("
            | "["
            | ","
            | "?"
            | ":"
            | "+"
            | "-"
            | "*"
            | "/"
            | "%"
            | "&"
            | "|"
            | "!"
            | "<"
            | ">"
    )
}

fn is_typeish_declaration_token(token: &str) -> bool {
    let trimmed = token.trim_matches(|ch| ch == '*' || ch == '&');
    trimmed
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_uppercase())
        || matches!(
            trimmed,
            "int"
                | "char"
                | "short"
                | "long"
                | "float"
                | "double"
                | "bool"
                | "void"
                | "size_t"
                | "uint8_t"
                | "uint16_t"
                | "uint32_t"
                | "uint64_t"
                | "int8_t"
                | "int16_t"
                | "int32_t"
                | "int64_t"
        )
}

fn is_pointer_or_reference(token: &str) -> bool {
    token.chars().all(|ch| ch == '*' || ch == '&')
}

fn typeish_token_before_pointer_or_reference(before_prefix: &str) -> bool {
    let mut trimmed = before_prefix.trim_end();
    while let Some(ch) = trimmed.chars().next_back() {
        if ch == '*' || ch == '&' || ch.is_whitespace() {
            trimmed = &trimmed[..trimmed.len() - ch.len_utf8()];
        } else {
            break;
        }
    }
    previous_token(trimmed)
        .as_deref()
        .is_some_and(is_typeish_declaration_token)
}
