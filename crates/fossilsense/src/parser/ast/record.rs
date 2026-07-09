use crate::parser::lexical::compact_whitespace;
use crate::parser::{
    FieldDef, MemberConfidence, MemberDef, MemberKind, RecordConfidence, RecordDef, RecordKind,
};

use super::{byte_to_utf16_col, declarator_identifier, node_text, record_type_name};

#[allow(clippy::too_many_arguments)]
pub(super) fn collect_record_and_members(
    node: tree_sitter::Node<'_>,
    source: &str,
    line_starts: &[usize],
    collect_fields: bool,
    records: &mut Vec<RecordDef>,
    fields: &mut Vec<FieldDef>,
    members: &mut Vec<MemberDef>,
) {
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let tag_name = node
        .child_by_field_name("name")
        .and_then(|name| node_text(name, source))
        .map(|name| name.to_string());

    let typedef_name = parent_typedef_name(node, source);

    if tag_name.is_none() && typedef_name.is_none() {
        return;
    }

    let kind = match node.kind() {
        "union_specifier" => RecordKind::Union,
        "class_specifier" => RecordKind::Class,
        _ => RecordKind::Struct,
    };

    let confidence = if tag_name.is_some() {
        RecordConfidence::NamedTag
    } else if typedef_name.is_some() {
        RecordConfidence::AnonymousTypedef
    } else {
        RecordConfidence::Heuristic
    };

    let display_name = typedef_name
        .clone()
        .or_else(|| tag_name.clone())
        .unwrap_or_default();

    let start_pos = node.start_position();
    let end_pos = node.end_position();
    let start_byte = node.start_byte();
    let end_byte = node.end_byte();
    let start_line = start_pos.row;
    let end_line = end_pos.row;

    let start_line_byte = line_starts.get(start_line).copied().unwrap_or(0);
    let start_col = byte_to_utf16_col(source, start_line_byte, start_byte);

    let end_line_byte = line_starts.get(end_line).copied().unwrap_or(0);
    let end_col = byte_to_utf16_col(source, end_line_byte, end_byte);

    let record_key = format!("rec_{}", start_byte);

    let sig_end = body.start_byte();
    let raw_sig = source.get(start_byte..sig_end).unwrap_or("");
    let signature = compact_whitespace(raw_sig);

    records.push(RecordDef {
        record_key: record_key.clone(),
        display_name: display_name.clone(),
        tag_name,
        typedef_name,
        kind,
        start_byte,
        end_byte,
        start_line,
        start_col,
        end_line,
        end_col,
        confidence,
        signature,
    });

    if collect_fields {
        collect_body_members(
            body,
            &record_key,
            &display_name,
            source,
            line_starts,
            records,
            fields,
            members,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_body_members(
    body: tree_sitter::Node<'_>,
    record_key: &str,
    record_display_name: &str,
    source: &str,
    line_starts: &[usize],
    records: &mut Vec<RecordDef>,
    fields: &mut Vec<FieldDef>,
    members: &mut Vec<MemberDef>,
) {
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if child.kind().starts_with("preproc_") {
            collect_body_members(
                child,
                record_key,
                record_display_name,
                source,
                line_starts,
                records,
                fields,
                members,
            );
            continue;
        }
        if child.kind() != "field_declaration" {
            continue;
        }
        let mut decl_cursor = child.walk();
        let declarators: Vec<tree_sitter::Node<'_>> = child
            .children_by_field_name("declarator", &mut decl_cursor)
            .collect();

        if declarators.is_empty() {
            // Anonymous nested struct/union member: flatten its fields up.
            if let Some(type_node) = child.child_by_field_name("type") {
                if matches!(type_node.kind(), "struct_specifier" | "union_specifier") {
                    if let Some(inner) = type_node.child_by_field_name("body") {
                        collect_body_members(
                            inner,
                            record_key,
                            record_display_name,
                            source,
                            line_starts,
                            records,
                            fields,
                            members,
                        );
                    }
                }
            }
            continue;
        }

        let signature = compact_whitespace(child.utf8_text(source.as_bytes()).unwrap_or_default());
        let member_type_name = child
            .child_by_field_name("type")
            .and_then(|type_node| record_type_name(type_node, source));
        let anonymous_record_type = child
            .child_by_field_name("type")
            .filter(|type_node| anonymous_record_type_node(*type_node));
        for decl in declarators {
            if let Some((id_node, name)) = declarator_identifier(decl, source) {
                let kind = method_member_kind(child, decl, source);
                let mut type_name = (kind == MemberKind::Field)
                    .then(|| member_type_name.clone())
                    .flatten();
                if kind == MemberKind::Field && type_name.is_none() {
                    if let Some(type_node) = anonymous_record_type {
                        let nested_display_name = format!("{record_display_name}.{name}");
                        let nested_record_key =
                            format!("rec_{}_{}", type_node.start_byte(), id_node.start_byte());
                        push_synthetic_nested_record(
                            records,
                            type_node,
                            &nested_record_key,
                            &nested_display_name,
                            source,
                            line_starts,
                        );
                        if let Some(inner) = type_node.child_by_field_name("body") {
                            collect_body_members(
                                inner,
                                &nested_record_key,
                                &nested_display_name,
                                source,
                                line_starts,
                                records,
                                fields,
                                members,
                            );
                        }
                        type_name = Some(nested_display_name);
                    }
                }
                if kind == MemberKind::Field {
                    let start_pos = id_node.start_position();
                    let end_pos = id_node.end_position();
                    let start_byte = id_node.start_byte();
                    let end_byte = id_node.end_byte();
                    let start_line = start_pos.row;
                    let end_line = end_pos.row;

                    let start_line_byte = line_starts.get(start_line).copied().unwrap_or(0);
                    let start_col = byte_to_utf16_col(source, start_line_byte, start_byte);

                    let end_line_byte = line_starts.get(end_line).copied().unwrap_or(0);
                    let end_col = byte_to_utf16_col(source, end_line_byte, end_byte);

                    fields.push(FieldDef {
                        record_key: record_key.to_string(),
                        name: name.to_string(),
                        start_byte,
                        end_byte,
                        start_line,
                        start_col,
                        end_line,
                        end_col,
                        signature: signature.clone(),
                    });
                }
                push_member(
                    members,
                    record_key.to_string(),
                    name.to_string(),
                    id_node,
                    kind,
                    MemberConfidence::InBody,
                    type_name,
                    signature.clone(),
                    source,
                    line_starts,
                );
            }
        }
    }
}

fn method_member_kind(
    declaration: tree_sitter::Node<'_>,
    declarator: tree_sitter::Node<'_>,
    source: &str,
) -> MemberKind {
    if !declarator_contains_kind(declarator, "function_declarator") {
        return MemberKind::Field;
    }
    if function_declarator_is_pointer_like(declarator) {
        return MemberKind::Field;
    }
    let signature = declaration
        .utf8_text(source.as_bytes())
        .map(compact_whitespace)
        .unwrap_or_default();
    let has_static_child = direct_child_text(declaration, "storage_class_specifier", source)
        .as_deref()
        == Some("static");
    if signature.starts_with("static ") || has_static_child {
        MemberKind::StaticMethod
    } else {
        MemberKind::Method
    }
}

fn direct_child_text(node: tree_sitter::Node<'_>, kind: &str, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    let text = node
        .children(&mut cursor)
        .find(|child| child.kind() == kind)
        .and_then(|child| child.utf8_text(source.as_bytes()).ok())
        .map(compact_whitespace);
    text
}

fn declarator_contains_kind(node: tree_sitter::Node<'_>, kind: &str) -> bool {
    if node.kind() == kind {
        return true;
    }
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .any(|child| declarator_contains_kind(child, kind));
    found
}

fn function_declarator_is_pointer_like(node: tree_sitter::Node<'_>) -> bool {
    if node.kind() == "function_declarator" {
        if let Some(inner) = node.child_by_field_name("declarator") {
            return declarator_contains_kind(inner, "pointer_declarator")
                || declarator_contains_kind(inner, "reference_declarator");
        }
        return false;
    }
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .any(function_declarator_is_pointer_like);
    found
}

pub(super) fn collect_out_of_class_method_member(
    node: tree_sitter::Node<'_>,
    source: &str,
    line_starts: &[usize],
    members: &mut Vec<MemberDef>,
) {
    let Some(declarator) = node.child_by_field_name("declarator") else {
        return;
    };
    if !declarator_contains_kind(declarator, "function_declarator") {
        return;
    }
    let Some((owner, method, method_byte)) =
        simple_owner_method_from_declarator(declarator, source)
    else {
        return;
    };
    let signature = function_like_signature(node, source);
    push_member_at_byte(
        members,
        format!("owner:{owner}"),
        method,
        method_byte,
        MemberKind::Method,
        MemberConfidence::OutOfClassOwner,
        None,
        signature,
        source,
        line_starts,
    );
}

fn simple_owner_method_from_declarator(
    declarator: tree_sitter::Node<'_>,
    source: &str,
) -> Option<(String, String, usize)> {
    let text = declarator.utf8_text(source.as_bytes()).ok()?;
    let before_params = text.split_once('(')?.0.trim();
    if before_params.contains('<') || before_params.contains('>') {
        return None;
    }
    let parts: Vec<&str> = before_params.split("::").collect();
    if parts.len() != 2 || !is_identifier(parts[0]) || !is_identifier(parts[1]) {
        return None;
    }
    let owner = parts[0].to_string();
    let method = parts[1].to_string();
    let method_relative = text.find(&format!("::{method}"))? + 2;
    Some((owner, method, declarator.start_byte() + method_relative))
}

fn function_like_signature(node: tree_sitter::Node<'_>, source: &str) -> String {
    let end = node
        .child_by_field_name("body")
        .map(|body| body.start_byte())
        .unwrap_or_else(|| node.end_byte());
    compact_whitespace(source.get(node.start_byte()..end).unwrap_or_default())
}

fn anonymous_record_type_node(type_node: tree_sitter::Node<'_>) -> bool {
    matches!(
        type_node.kind(),
        "struct_specifier" | "union_specifier" | "class_specifier"
    ) && type_node.child_by_field_name("body").is_some()
        && type_node.child_by_field_name("name").is_none()
}

fn push_synthetic_nested_record(
    records: &mut Vec<RecordDef>,
    type_node: tree_sitter::Node<'_>,
    record_key: &str,
    display_name: &str,
    source: &str,
    line_starts: &[usize],
) {
    if records.iter().any(|record| record.record_key == record_key) {
        return;
    }

    let kind = match type_node.kind() {
        "union_specifier" => RecordKind::Union,
        "class_specifier" => RecordKind::Class,
        _ => RecordKind::Struct,
    };
    let start_pos = type_node.start_position();
    let end_pos = type_node.end_position();
    let start_byte = type_node.start_byte();
    let end_byte = type_node.end_byte();
    let start_line = start_pos.row;
    let end_line = end_pos.row;
    let start_line_byte = line_starts.get(start_line).copied().unwrap_or(0);
    let end_line_byte = line_starts.get(end_line).copied().unwrap_or(0);
    let sig_end = type_node
        .child_by_field_name("body")
        .map(|body| body.start_byte())
        .unwrap_or(end_byte);
    let signature = compact_whitespace(source.get(start_byte..sig_end).unwrap_or(""));

    records.push(RecordDef {
        record_key: record_key.to_string(),
        display_name: display_name.to_string(),
        tag_name: None,
        typedef_name: None,
        kind,
        start_byte,
        end_byte,
        start_line,
        start_col: byte_to_utf16_col(source, start_line_byte, start_byte),
        end_line,
        end_col: byte_to_utf16_col(source, end_line_byte, end_byte),
        confidence: RecordConfidence::Heuristic,
        signature,
    });
}

fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

#[allow(clippy::too_many_arguments)]
fn push_member(
    members: &mut Vec<MemberDef>,
    record_key: String,
    name: String,
    id_node: tree_sitter::Node<'_>,
    kind: MemberKind,
    confidence: MemberConfidence,
    type_name: Option<String>,
    signature: String,
    source: &str,
    line_starts: &[usize],
) {
    push_member_at_byte(
        members,
        record_key,
        name,
        id_node.start_byte(),
        kind,
        confidence,
        type_name,
        signature,
        source,
        line_starts,
    );
}

#[allow(clippy::too_many_arguments)]
fn push_member_at_byte(
    members: &mut Vec<MemberDef>,
    record_key: String,
    name: String,
    start_byte: usize,
    kind: MemberKind,
    confidence: MemberConfidence,
    type_name: Option<String>,
    signature: String,
    source: &str,
    line_starts: &[usize],
) {
    let end_byte = start_byte + name.len();
    let start_line = line_starts.partition_point(|line_start| *line_start <= start_byte) - 1;
    let end_line = line_starts.partition_point(|line_start| *line_start <= end_byte) - 1;
    let start_line_byte = line_starts.get(start_line).copied().unwrap_or(0);
    let end_line_byte = line_starts.get(end_line).copied().unwrap_or(0);
    members.push(MemberDef {
        record_key,
        name,
        kind,
        confidence,
        type_name,
        start_byte,
        end_byte,
        start_line,
        start_col: byte_to_utf16_col(source, start_line_byte, start_byte),
        end_line,
        end_col: byte_to_utf16_col(source, end_line_byte, end_byte),
        signature,
    });
}

fn parent_typedef_name(node: tree_sitter::Node<'_>, source: &str) -> Option<String> {
    let parent = node.parent()?;
    if parent.kind() == "type_definition" {
        let mut cursor = parent.walk();
        for decl in parent.children_by_field_name("declarator", &mut cursor) {
            if let Some((_, alias)) = declarator_identifier(decl, source) {
                return Some(alias.to_string());
            }
        }
    }
    None
}
