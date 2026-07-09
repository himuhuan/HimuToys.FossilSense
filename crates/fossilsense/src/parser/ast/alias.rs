use crate::parser::{AliasTarget, RecordKind, TypeAlias};

use super::{byte_to_utf16_col, declarator_identifier, node_text};

pub(super) fn collect_type_aliases(
    node: tree_sitter::Node<'_>,
    source: &str,
    line_starts: &[usize],
    aliases: &mut Vec<TypeAlias>,
) {
    let Some(type_node) = node.child_by_field_name("type") else {
        return;
    };
    let Some(target) = get_alias_target(type_node, source) else {
        return;
    };

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

    let mut cursor = node.walk();
    for decl in node.children_by_field_name("declarator", &mut cursor) {
        if let Some((_, alias)) = declarator_identifier(decl, source) {
            let is_same = match &target {
                AliasTarget::NamedRecord { tag, .. } => alias == tag,
                AliasTarget::UnresolvedTypeName(name) => alias == name,
                _ => false,
            };
            if !is_same {
                aliases.push(TypeAlias {
                    alias: alias.to_string(),
                    target: target.clone(),
                    start_byte,
                    end_byte,
                    start_line,
                    start_col,
                    end_line,
                    end_col,
                });
            }
        }
    }
}

fn get_alias_target(type_node: tree_sitter::Node<'_>, source: &str) -> Option<AliasTarget> {
    match type_node.kind() {
        "struct_specifier" | "union_specifier" | "class_specifier" => {
            let kind = match type_node.kind() {
                "union_specifier" => RecordKind::Union,
                "class_specifier" => RecordKind::Class,
                _ => RecordKind::Struct,
            };
            if type_node.child_by_field_name("body").is_some() {
                Some(AliasTarget::RecordKey(format!(
                    "rec_{}",
                    type_node.start_byte()
                )))
            } else if let Some(name_node) = type_node.child_by_field_name("name") {
                let tag = node_text(name_node, source)?.to_string();
                Some(AliasTarget::NamedRecord { tag, kind })
            } else {
                None
            }
        }
        "type_identifier" => {
            let name = node_text(type_node, source)?.to_string();
            Some(AliasTarget::UnresolvedTypeName(name))
        }
        _ => {
            let name = node_text(type_node, source)?.to_string();
            Some(AliasTarget::UnresolvedTypeName(name))
        }
    }
}
