#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProtobufCDeclarationKind {
    Message,
    Enum,
}

impl ProtobufCDeclarationKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Enum => "enum",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProtobufCDeclaration {
    pub(crate) proto_name: String,
    pub(crate) c_name: String,
    pub(crate) kind: ProtobufCDeclarationKind,
    pub(crate) start_byte: usize,
    pub(crate) end_byte: usize,
    pub(crate) start_line: u32,
    pub(crate) start_col: u32,
    pub(crate) end_line: u32,
    pub(crate) end_col: u32,
}

pub(crate) const MAX_PROTOBUF_C_TOKENS_PER_FILE: usize = 262_144;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProtobufCExtraction {
    pub(crate) declarations: Vec<ProtobufCDeclaration>,
    pub(crate) truncated: bool,
}

pub(crate) fn extract_protobuf_c_declarations(source: &str) -> ProtobufCExtraction {
    extract_protobuf_c_declarations_with_token_limit(source, MAX_PROTOBUF_C_TOKENS_PER_FILE)
}

fn extract_protobuf_c_declarations_with_token_limit(
    source: &str,
    max_tokens: usize,
) -> ProtobufCExtraction {
    let (tokens, truncated) = lex(source, max_tokens);
    let mut package = Vec::<String>::new();
    let mut frames = Vec::<Frame>::new();
    let mut pending_frame = None;
    let mut declarations = Vec::new();
    let mut index = 0usize;

    while index < tokens.len() {
        match &tokens[index].kind {
            TokenKind::Symbol('{') => {
                frames.push(pending_frame.take().unwrap_or(Frame::Generic));
                index += 1;
            }
            TokenKind::Symbol('}') => {
                frames.pop();
                pending_frame = None;
                index += 1;
            }
            TokenKind::Symbol(';') => {
                pending_frame = None;
                index += 1;
            }
            TokenKind::Ident(keyword)
                if keyword == "package" && frames.is_empty() && pending_frame.is_none() =>
            {
                let mut parsed = Vec::new();
                let mut cursor = index + 1;
                let mut expect_ident = true;
                while cursor < tokens.len() {
                    match &tokens[cursor].kind {
                        TokenKind::Ident(value) if expect_ident => {
                            parsed.push(value.clone());
                            expect_ident = false;
                        }
                        TokenKind::Symbol('.') if !expect_ident => expect_ident = true,
                        TokenKind::Symbol(';') => break,
                        _ => {
                            parsed.clear();
                            break;
                        }
                    }
                    cursor += 1;
                }
                if !parsed.is_empty() && !expect_ident {
                    package = parsed;
                }
                index = cursor.saturating_add(1);
            }
            TokenKind::Ident(keyword)
                if matches!(keyword.as_str(), "message" | "enum" | "service") =>
            {
                if frames.iter().any(|frame| matches!(frame, Frame::Ignore)) {
                    index += 1;
                    continue;
                }
                let Some(name_token) = tokens.get(index + 1) else {
                    break;
                };
                let TokenKind::Ident(name) = &name_token.kind else {
                    index += 1;
                    continue;
                };
                if keyword == "service" {
                    pending_frame = Some(Frame::Ignore);
                    index += 2;
                    continue;
                }
                if frames.iter().any(|frame| matches!(frame, Frame::Enum)) {
                    index += 2;
                    continue;
                }

                let kind = if keyword == "message" {
                    ProtobufCDeclarationKind::Message
                } else {
                    ProtobufCDeclarationKind::Enum
                };
                let mut proto_components = package.clone();
                proto_components.extend(frames.iter().filter_map(|frame| match frame {
                    Frame::Message(name) => Some(name.clone()),
                    _ => None,
                }));
                proto_components.push(name.clone());
                let c_name = proto_components
                    .iter()
                    .map(|component| protobuf_c_type_component(component))
                    .collect::<Vec<_>>()
                    .join("__");
                declarations.push(ProtobufCDeclaration {
                    proto_name: proto_components.join("."),
                    c_name,
                    kind,
                    start_byte: name_token.start_byte,
                    end_byte: name_token.end_byte,
                    start_line: name_token.line,
                    start_col: name_token.col,
                    end_line: name_token.line,
                    end_col: name_token.col.saturating_add(name.len() as u32),
                });
                pending_frame = Some(match kind {
                    ProtobufCDeclarationKind::Message => Frame::Message(name.clone()),
                    ProtobufCDeclarationKind::Enum => Frame::Enum,
                });
                index += 2;
            }
            _ => {
                index += 1;
            }
        }
    }

    ProtobufCExtraction {
        declarations,
        truncated,
    }
}

#[derive(Debug)]
enum Frame {
    Message(String),
    Enum,
    Ignore,
    Generic,
}

#[derive(Debug)]
struct Token {
    kind: TokenKind,
    start_byte: usize,
    end_byte: usize,
    line: u32,
    col: u32,
}

#[derive(Debug)]
enum TokenKind {
    Ident(String),
    Symbol(char),
}

fn lex(source: &str, max_tokens: usize) -> (Vec<Token>, bool) {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0usize;
    let mut line = 0u32;
    let mut col = 0u32;
    while index < bytes.len() {
        if tokens.len() >= max_tokens {
            return (tokens, true);
        }
        match bytes[index] {
            b'\n' => {
                index += 1;
                line = line.saturating_add(1);
                col = 0;
            }
            byte if byte.is_ascii_whitespace() => {
                index += 1;
                col = col.saturating_add(1);
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index += 2;
                col = col.saturating_add(2);
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                    col = col.saturating_add(1);
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                col = col.saturating_add(2);
                while index < bytes.len() {
                    if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
                        index += 2;
                        col = col.saturating_add(2);
                        break;
                    }
                    if bytes[index] == b'\n' {
                        index += 1;
                        line = line.saturating_add(1);
                        col = 0;
                    } else {
                        index += 1;
                        col = col.saturating_add(1);
                    }
                }
            }
            quote @ (b'"' | b'\'') => {
                index += 1;
                col = col.saturating_add(1);
                while index < bytes.len() {
                    match bytes[index] {
                        b'\\' => {
                            let advance = usize::from(index + 1 < bytes.len()) + 1;
                            index += advance;
                            col = col.saturating_add(advance as u32);
                        }
                        byte if byte == quote => {
                            index += 1;
                            col = col.saturating_add(1);
                            break;
                        }
                        b'\n' => {
                            index += 1;
                            line = line.saturating_add(1);
                            col = 0;
                        }
                        _ => {
                            index += 1;
                            col = col.saturating_add(1);
                        }
                    }
                }
            }
            byte if byte.is_ascii_alphabetic() || byte == b'_' => {
                let start = index;
                let start_col = col;
                while index < bytes.len()
                    && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
                {
                    index += 1;
                    col = col.saturating_add(1);
                }
                tokens.push(Token {
                    kind: TokenKind::Ident(source[start..index].to_string()),
                    start_byte: start,
                    end_byte: index,
                    line,
                    col: start_col,
                });
            }
            symbol @ (b'{' | b'}' | b';' | b'.') => {
                tokens.push(Token {
                    kind: TokenKind::Symbol(symbol as char),
                    start_byte: index,
                    end_byte: index + 1,
                    line,
                    col,
                });
                index += 1;
                col = col.saturating_add(1);
            }
            _ => {
                index += 1;
                col = col.saturating_add(1);
            }
        }
    }
    (tokens, false)
}

fn protobuf_c_type_component(component: &str) -> String {
    component
        .split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            let Some(first) = chars.next() else {
                return String::new();
            };
            first.to_ascii_uppercase().to_string() + chars.as_str()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_package_top_level_and_nested_message_and_enum_names() {
        let source = r#"syntax = "proto3";
package foo.bar;

message outer_box {
  enum state_code {
    STATE_UNKNOWN = 0;
  }
  message inner_item {}
}

enum result_code { RESULT_OK = 0; }
"#;

        let extraction = extract_protobuf_c_declarations(source);
        let declarations = extraction.declarations;
        let summary: Vec<_> = declarations
            .iter()
            .map(|declaration| {
                (
                    declaration.proto_name.as_str(),
                    declaration.c_name.as_str(),
                    declaration.kind.as_str(),
                    declaration.start_line,
                )
            })
            .collect();

        assert_eq!(
            summary,
            vec![
                ("foo.bar.outer_box", "Foo__Bar__OuterBox", "message", 3),
                (
                    "foo.bar.outer_box.state_code",
                    "Foo__Bar__OuterBox__StateCode",
                    "enum",
                    4,
                ),
                (
                    "foo.bar.outer_box.inner_item",
                    "Foo__Bar__OuterBox__InnerItem",
                    "message",
                    7,
                ),
                ("foo.bar.result_code", "Foo__Bar__ResultCode", "enum", 10),
            ]
        );
    }

    #[test]
    fn ignores_comments_strings_and_service_bodies() {
        let source = r#"
// message Commented {}
/* enum Blocked { X = 0; } */
option note = "message StringValue { enum Hidden {} }";
service ignored_service {
  rpc Call(Request) returns (Reply);
  option note = "message AlsoHidden {}";
}
message Visible {}
"#;

        let extraction = extract_protobuf_c_declarations(source);
        let declarations = extraction.declarations;

        assert_eq!(declarations.len(), 1);
        assert_eq!(declarations[0].proto_name, "Visible");
        assert_eq!(declarations[0].c_name, "Visible");
        assert_eq!(declarations[0].start_line, 8);
    }

    #[test]
    fn incomplete_input_keeps_safe_declarations_and_never_requires_a_full_ast() {
        let extraction = extract_protobuf_c_declarations(
            "package demo;\nmessage Ready {}\nmessage Partial {\n enum Nested\n",
        );
        let declarations = extraction.declarations;

        assert_eq!(
            declarations
                .iter()
                .map(|declaration| declaration.c_name.as_str())
                .collect::<Vec<_>>(),
            vec!["Demo__Ready", "Demo__Partial", "Demo__Partial__Nested"]
        );
    }

    #[test]
    fn extraction_stops_at_a_fixed_token_budget_and_marks_truncation() {
        let source = "package demo;\n".to_string() + &"message Item {}\n".repeat(100);
        let extraction = extract_protobuf_c_declarations_with_token_limit(&source, 12);

        assert!(extraction.truncated);
        assert!(extraction.declarations.len() < 100);
        assert!(extraction
            .declarations
            .iter()
            .all(|declaration| declaration.c_name == "Demo__Item"));
    }
}
