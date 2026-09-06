//! Shared C-family lexical boundaries for recovery and bounded language probes.
pub(crate) struct LexicalMap {
    pub(crate) code: Vec<bool>,
    pub(crate) preprocessor: Vec<bool>,
}

impl LexicalMap {
    pub(crate) fn new(source: &str, is_cpp: bool) -> Self {
        let bytes = source.as_bytes();
        let mut code = vec![true; bytes.len()];
        let mut preprocessor = vec![false; bytes.len()];
        mark_preprocessor_logical_lines(bytes, &mut preprocessor);
        let mut index = 0usize;
        while index < bytes.len() {
            if bytes.get(index..index + 2) == Some(b"//") {
                let start = index;
                index += 2;
                loop {
                    while index < bytes.len() && bytes[index] != b'\n' {
                        index += 1;
                    }
                    let continued = index > start
                        && bytes[..index].iter().rev().find(|byte| **byte != b'\r') == Some(&b'\\');
                    if index < bytes.len() {
                        index += 1;
                    }
                    if !continued || index >= bytes.len() {
                        break;
                    }
                }
                code[start..index].fill(false);
                continue;
            }
            if bytes.get(index..index + 2) == Some(b"/*") {
                let start = index;
                index += 2;
                while index + 1 < bytes.len() && bytes.get(index..index + 2) != Some(b"*/") {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
                code[start..index].fill(false);
                continue;
            }
            if is_cpp && bytes[index] == b'R' && bytes.get(index + 1) == Some(&b'"') {
                let start = index;
                if let Some(end) = raw_string_end(bytes, start) {
                    code[start..end].fill(false);
                    index = end;
                    continue;
                }
                code[start..].fill(false);
                break;
            }
            if matches!(bytes[index], b'"' | b'\'') {
                let start = index;
                index = skip_quoted_bytes(bytes, index);
                code[start..index].fill(false);
                continue;
            }
            index += 1;
        }
        for (index, is_preprocessor) in preprocessor.iter().copied().enumerate() {
            if is_preprocessor {
                code[index] = false;
            }
        }
        Self { code, preprocessor }
    }

    pub(crate) fn next_code_token(&self, bytes: &[u8], mut index: usize) -> Option<usize> {
        while index < bytes.len() {
            if self.preprocessor[index] {
                return None;
            }
            if self.code[index] && !bytes[index].is_ascii_whitespace() {
                return Some(index);
            }
            index += 1;
        }
        None
    }

    pub(crate) fn contains_identifier(&self, source: &str, identifier: &str) -> bool {
        let bytes = source.as_bytes();
        source[..self.code.len()]
            .match_indices(identifier)
            .any(|(start, _)| {
                let end = start + identifier.len();
                self.code[start..end].iter().all(|code| *code)
                    && (start == 0 || !identifier_byte(bytes[start - 1]))
                    && (end == bytes.len() || !identifier_byte(bytes[end]))
            })
    }
}

fn mark_preprocessor_logical_lines(bytes: &[u8], output: &mut [bool]) {
    let mut start = 0usize;
    let mut continuation = false;
    while start < bytes.len() {
        let newline = bytes[start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |offset| start + offset + 1);
        let content_end = if newline > start && bytes[newline - 1] == b'\n' {
            newline - 1
        } else {
            newline
        };
        let first = bytes[start..content_end]
            .iter()
            .position(|byte| !byte.is_ascii_whitespace())
            .map(|offset| start + offset);
        let is_preprocessor = continuation || first.is_some_and(|index| bytes[index] == b'#');
        if is_preprocessor {
            output[start..content_end].fill(true);
        }
        let last = bytes[start..content_end]
            .iter()
            .rposition(|byte| !byte.is_ascii_whitespace())
            .map(|offset| start + offset);
        continuation = is_preprocessor && last.is_some_and(|index| bytes[index] == b'\\');
        start = newline;
    }
}

fn raw_string_end(bytes: &[u8], start: usize) -> Option<usize> {
    let delimiter_start = start + 2;
    let open_rel = bytes
        .get(delimiter_start..)?
        .iter()
        .position(|byte| *byte == b'(')?;
    if open_rel > 16 {
        return None;
    }
    let open = delimiter_start + open_rel;
    let delimiter = &bytes[delimiter_start..open];
    if delimiter
        .iter()
        .any(|byte| byte.is_ascii_whitespace() || matches!(*byte, b'(' | b')' | b'\\'))
    {
        return None;
    }
    let mut index = open + 1;
    while index < bytes.len() {
        if bytes[index] == b')'
            && bytes.get(index + 1..index + 1 + delimiter.len()) == Some(delimiter)
            && bytes.get(index + 1 + delimiter.len()) == Some(&b'"')
        {
            return Some(index + delimiter.len() + 2);
        }
        index += 1;
    }
    None
}

fn skip_quoted_bytes(bytes: &[u8], start: usize) -> usize {
    let quote = bytes[start];
    let mut index = start + 1;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            index = (index + 2).min(bytes.len());
        } else if bytes[index] == quote {
            return index + 1;
        } else {
            index += 1;
        }
    }
    bytes.len()
}

fn identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 128
}
