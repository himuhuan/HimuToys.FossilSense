use super::compact_whitespace;

pub(super) fn strip_leading_comments(line: &str, in_block_comment: &mut bool) -> String {
    let mut rest = line;
    loop {
        let trimmed = rest.trim_start();
        if *in_block_comment {
            if let Some(end) = trimmed.find("*/") {
                *in_block_comment = false;
                rest = &trimmed[end + 2..];
                continue;
            }
            return String::new();
        }
        if trimmed.starts_with("//") {
            return String::new();
        }
        if trimmed.starts_with("/*") {
            if let Some(end) = trimmed.find("*/") {
                rest = &trimmed[end + 2..];
                continue;
            }
            *in_block_comment = true;
            return String::new();
        }
        return rest.to_string();
    }
}

pub(super) fn update_guard_stack(trimmed: &str, guard_stack: &mut Vec<String>) {
    if trimmed.starts_with("#if ")
        || trimmed.starts_with("#ifdef ")
        || trimmed.starts_with("#ifndef ")
    {
        guard_stack.push(trimmed.to_string());
    } else if trimmed.starts_with("#elif ") || trimmed.starts_with("#else") {
        if let Some(last) = guard_stack.last_mut() {
            *last = trimmed.to_string();
        }
    } else if trimmed.starts_with("#endif") {
        guard_stack.pop();
    }
}

pub(super) fn current_guard(guard_stack: &[String]) -> Option<String> {
    if guard_stack.is_empty() {
        None
    } else {
        Some(guard_stack.join(" && "))
    }
}

pub(super) fn line_continues_preprocessor(line: &str) -> bool {
    line.trim_end_matches([' ', '\t', '\r']).ends_with('\\')
}

#[derive(Debug, Default)]
pub(super) struct PendingStatement {
    pub(super) text: String,
    pub(super) start_line: usize,
    pub(super) end_line: usize,
    pub(super) active: bool,
    brace_balance: isize,
}

impl PendingStatement {
    pub(super) fn push(&mut self, line: &str, line_index: usize, brace_delta: isize) {
        if !self.active {
            self.start_line = line_index;
            self.active = true;
        }
        self.end_line = line_index;
        self.text.push_str(line);
        self.text.push('\n');
        self.brace_balance += brace_delta;
    }

    pub(super) fn is_complete(&self) -> bool {
        let trimmed = self.text.trim_end();
        if trimmed.ends_with(';') && self.brace_balance <= 0 {
            return true;
        }
        if trimmed.ends_with('{') {
            return !looks_like_record_body_declaration(trimmed);
        }
        if trimmed.ends_with('}') && self.brace_balance <= 0 {
            return true;
        }
        false
    }

    pub(super) fn clear(&mut self) {
        self.text.clear();
        self.active = false;
        self.brace_balance = 0;
    }
}

fn looks_like_record_body_declaration(text: &str) -> bool {
    let compact = compact_whitespace(text);
    if !compact.ends_with('{') {
        return false;
    }
    let prefix = compact.trim_end_matches('{').trim_end();
    prefix.starts_with("typedef struct ")
        || prefix == "typedef struct"
        || prefix.starts_with("typedef union ")
        || prefix == "typedef union"
        || prefix.starts_with("typedef enum ")
        || prefix == "typedef enum"
        || prefix.starts_with("typedef class ")
        || prefix == "typedef class"
        || prefix.starts_with("struct ")
        || prefix == "struct"
        || prefix.starts_with("union ")
        || prefix == "union"
        || prefix.starts_with("enum ")
        || prefix == "enum"
        || prefix.starts_with("class ")
        || prefix == "class"
}

#[derive(Debug, Default)]
pub(super) struct BraceScanState {
    in_block_comment: bool,
}

pub(super) fn code_brace_delta(line: &str, state: &mut BraceScanState) -> isize {
    let mut delta = 0isize;
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if state.in_block_comment {
            if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                state.in_block_comment = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }

        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            break;
        }
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            state.in_block_comment = true;
            i += 2;
            continue;
        }
        if bytes[i] == b'"' || bytes[i] == b'\'' {
            i = skip_quoted_bytes(bytes, i);
            continue;
        }

        match bytes[i] {
            b'{' => delta += 1,
            b'}' => delta -= 1,
            _ => {}
        }
        i += 1;
    }
    delta
}

fn skip_quoted_bytes(bytes: &[u8], quote_start: usize) -> usize {
    let quote = bytes[quote_start];
    let mut i = quote_start + 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i = (i + 2).min(bytes.len());
            continue;
        }
        if bytes[i] == quote {
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}
