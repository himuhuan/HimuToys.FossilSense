use super::*;

pub(super) fn collect_cpp_alias_declaration(
    node: tree_sitter::Node<'_>,
    path: &Path,
    source: &str,
    line_starts: &[usize],
    facts: ParseFacts,
    type_symbols: &mut Vec<super::super::RawDeclaration>,
    aliases: &mut Vec<TypeAlias>,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let Some(name) = node_text(name_node, source) else {
        return;
    };
    let Some(type_descriptor) = node.child_by_field_name("type") else {
        return;
    };

    if facts.contains(ParseFacts::DECLARATIONS) {
        if let Some(symbol) = symbol_from_name_node(
            name_node,
            SymbolKind::Type,
            SymbolRole::Definition,
            node,
            source,
            line_starts,
        ) {
            type_symbols.push(symbol);
        }
    }
    if !facts.intersects(ParseFacts::DECLARATIONS | ParseFacts::ALIASES) {
        return;
    }

    let target_node = type_descriptor
        .child_by_field_name("type")
        .unwrap_or(type_descriptor);
    let Some(target) = get_alias_target(target_node, source) else {
        return;
    };
    let malformed = contains_error_or_missing(node);
    let target_fidelity = if malformed {
        AliasTargetFidelity::Malformed
    } else {
        AliasTargetFidelity::AstExact
    };
    let declarator_shape = if malformed {
        DeclaratorShape::Unsupported
    } else {
        cpp_alias_declarator_shape(type_descriptor, source)
    };
    let declaration_range = source_range(node, source, line_starts);
    let declaration_hash = source_range_hash(source, declaration_range);
    let start = name_node.start_position();
    let end = name_node.end_position();
    let underlying_spelling = node_text(type_descriptor, source)
        .map(compact_whitespace)
        .unwrap_or_default();
    let path_text = path.to_string_lossy().replace('\\', "/");
    let fingerprint = digest(&format!(
        "{}|{}|{}|{}|{:?}|{:?}",
        path_text,
        node.start_byte(),
        name_node.start_byte(),
        name,
        target,
        declarator_shape
    ));
    aliases.push(TypeAlias {
        alias: name.to_string(),
        target,
        start_byte: name_node.start_byte(),
        end_byte: name_node.end_byte(),
        start_line: start.row,
        start_col: byte_to_utf16_col(
            source,
            line_starts.get(start.row).copied().unwrap_or(0),
            name_node.start_byte(),
        ),
        end_line: end.row,
        end_col: byte_to_utf16_col(
            source,
            line_starts.get(end.row).copied().unwrap_or(0),
            name_node.end_byte(),
        ),
        declaration_range,
        declaration_hash,
        underlying_spelling,
        declarator_shape,
        target_fidelity,
        fingerprint,
    });
}

pub(super) fn cpp_alias_declarator_shape(
    type_descriptor: tree_sitter::Node<'_>,
    source: &str,
) -> DeclaratorShape {
    let qualifiers = direct_type_qualifiers(type_descriptor, source);
    let Some(declarator) = type_descriptor.child_by_field_name("declarator") else {
        return if qualifiers.is_empty() {
            DeclaratorShape::Identity
        } else {
            DeclaratorShape::Qualified { qualifiers }
        };
    };

    let has_pointer = node_kind_contains(declarator, "pointer_declarator");
    let has_function = node_kind_contains(declarator, "function_declarator");
    if has_pointer && has_function {
        return DeclaratorShape::FunctionPointer {
            signature: node_text(type_descriptor, source)
                .map(compact_whitespace)
                .unwrap_or_default(),
        };
    }
    if has_pointer {
        let mut pointer_qualifiers = qualifiers;
        collect_type_qualifiers(declarator, source, &mut pointer_qualifiers);
        pointer_qualifiers.sort();
        pointer_qualifiers.dedup();
        return DeclaratorShape::Pointer {
            qualifiers: pointer_qualifiers,
        };
    }
    if node_kind_contains(declarator, "array_declarator") {
        let extent_text = declarator
            .child_by_field_name("size")
            .and_then(|size| node_text(size, source))
            .unwrap_or_default()
            .trim()
            .to_string();
        return DeclaratorShape::Array { extent_text };
    }
    DeclaratorShape::Unsupported
}

pub(super) fn direct_type_qualifiers(node: tree_sitter::Node<'_>, source: &str) -> Vec<String> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|child| child.kind() == "type_qualifier")
        .filter_map(|child| node_text(child, source).map(str::to_string))
        .collect()
}

pub(super) fn collect_type_qualifiers(
    node: tree_sitter::Node<'_>,
    source: &str,
    qualifiers: &mut Vec<String>,
) {
    if node.kind() == "type_qualifier" {
        if let Some(qualifier) = node_text(node, source) {
            qualifiers.push(qualifier.to_string());
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_type_qualifiers(child, source, qualifiers);
    }
}

pub(super) fn node_kind_contains(node: tree_sitter::Node<'_>, suffix: &str) -> bool {
    if node.kind().ends_with(suffix) {
        return true;
    }
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .any(|child| node_kind_contains(child, suffix));
    found
}

pub(super) fn source_range(
    node: tree_sitter::Node<'_>,
    source: &str,
    line_starts: &[usize],
) -> SourceRange {
    let start = node.start_position();
    let end = node.end_position();
    SourceRange {
        start: SourcePosition {
            line: start.row as u32,
            character: byte_to_utf16_col(
                source,
                line_starts.get(start.row).copied().unwrap_or(0),
                node.start_byte(),
            ) as u32,
        },
        end: SourcePosition {
            line: end.row as u32,
            character: byte_to_utf16_col(
                source,
                line_starts.get(end.row).copied().unwrap_or(0),
                node.end_byte(),
            ) as u32,
        },
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
    }
}

pub(super) fn source_range_bytes(
    start_byte: usize,
    end_byte: usize,
    source: &str,
    line_starts: &[usize],
) -> SourceRange {
    let start_line = line_starts
        .partition_point(|line_start| *line_start <= start_byte)
        .saturating_sub(1);
    let end_line = line_starts
        .partition_point(|line_start| *line_start <= end_byte)
        .saturating_sub(1);
    SourceRange {
        start: SourcePosition {
            line: start_line as u32,
            character: byte_to_utf16_col(
                source,
                line_starts.get(start_line).copied().unwrap_or(0),
                start_byte,
            ) as u32,
        },
        end: SourcePosition {
            line: end_line as u32,
            character: byte_to_utf16_col(
                source,
                line_starts.get(end_line).copied().unwrap_or(0),
                end_byte,
            ) as u32,
        },
        start_byte,
        end_byte,
    }
}

pub(super) fn alias_underlying_spelling(
    declaration: tree_sitter::Node<'_>,
    type_node: tree_sitter::Node<'_>,
    first_declarator: Option<tree_sitter::Node<'_>>,
    source: &str,
) -> String {
    let prefix_end = first_declarator
        .map(|declarator| declarator.start_byte())
        .unwrap_or_else(|| type_node.end_byte());
    let before_type = source
        .get(declaration.start_byte()..type_node.start_byte())
        .unwrap_or_default();
    let after_type = source
        .get(type_node.end_byte()..prefix_end)
        .unwrap_or_default();
    let type_spelling = if let Some(body) = type_node.child_by_field_name("body") {
        source
            .get(type_node.start_byte()..body.start_byte())
            .unwrap_or_default()
    } else {
        source
            .get(type_node.start_byte()..type_node.end_byte())
            .unwrap_or_default()
    };
    let before_type = strip_typedef_keyword(before_type);
    compact_whitespace(&format!("{before_type}{type_spelling}{after_type}"))
}

pub(super) fn strip_typedef_keyword(value: &str) -> &str {
    let trimmed = value.trim_start();
    let Some(rest) = trimmed.strip_prefix("typedef") else {
        return value;
    };
    if rest
        .chars()
        .next()
        .is_some_and(|ch| ch == '_' || ch.is_ascii_alphanumeric())
    {
        value
    } else {
        rest
    }
}

pub(super) fn typedef_base_qualifiers(node: tree_sitter::Node<'_>, source: &str) -> Vec<String> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|child| child.kind() == "type_qualifier")
        .filter_map(|child| node_text(child, source).map(str::to_string))
        .collect()
}

pub(super) fn typedef_declarator_shape(
    declarator: tree_sitter::Node<'_>,
    source: &str,
    base_qualifiers: &[String],
) -> DeclaratorShape {
    if simple_alias_identifier(declarator) {
        return if base_qualifiers.is_empty() {
            DeclaratorShape::Identity
        } else {
            DeclaratorShape::Qualified {
                qualifiers: base_qualifiers.to_vec(),
            }
        };
    }

    match declarator.kind() {
        "pointer_declarator" => {
            let Some(inner) = declarator.child_by_field_name("declarator") else {
                return DeclaratorShape::Unsupported;
            };
            if !simple_alias_identifier(inner) {
                return DeclaratorShape::Unsupported;
            }
            let mut cursor = declarator.walk();
            let mut qualifiers = Vec::new();
            for child in declarator.named_children(&mut cursor) {
                if child.id() == inner.id() {
                    continue;
                }
                if child.kind() != "type_qualifier" {
                    return DeclaratorShape::Unsupported;
                }
                if let Some(qualifier) = node_text(child, source) {
                    qualifiers.push(qualifier.to_string());
                }
            }
            DeclaratorShape::Pointer { qualifiers }
        }
        "array_declarator" => {
            let Some(inner) = declarator.child_by_field_name("declarator") else {
                return DeclaratorShape::Unsupported;
            };
            if !simple_alias_identifier(inner) {
                return DeclaratorShape::Unsupported;
            }
            let extent_text = declarator
                .child_by_field_name("size")
                .and_then(|size| node_text(size, source))
                .unwrap_or_default()
                .trim()
                .to_string();
            DeclaratorShape::Array { extent_text }
        }
        _ => DeclaratorShape::Unsupported,
    }
}

pub(super) fn simple_alias_identifier(node: tree_sitter::Node<'_>) -> bool {
    matches!(
        node.kind(),
        "identifier" | "field_identifier" | "type_identifier" | "primitive_type"
    )
}

pub(super) fn contains_error_or_missing(root: tree_sitter::Node<'_>) -> bool {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.is_error() || node.is_missing() {
            return true;
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    false
}

pub(super) fn digest(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..24].to_string()
}

pub(super) fn source_range_hash(source: &str, range: SourceRange) -> [u8; 32] {
    let bytes = source
        .as_bytes()
        .get(range.start_byte..range.end_byte)
        .unwrap_or_default();
    *blake3::hash(bytes).as_bytes()
}

pub(super) fn parent_typedef_name_node<'tree>(
    node: tree_sitter::Node<'tree>,
    source: &'tree str,
) -> Option<(tree_sitter::Node<'tree>, String)> {
    let parent = node.parent()?;
    if parent.kind() == "type_definition" {
        let mut cursor = parent.walk();
        for decl in parent.children_by_field_name("declarator", &mut cursor) {
            if let Some((alias_node, alias)) = declarator_identifier(decl, source) {
                return Some((alias_node, alias.to_string()));
            }
        }
    }
    None
}

pub(super) fn get_alias_target(
    type_node: tree_sitter::Node<'_>,
    source: &str,
) -> Option<AliasTarget> {
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

/// Name of a record type node: the tag of a struct/union/enum specifier, or the
/// text of a plain `type_identifier` (typedef). `None` for primitive and other
/// non-record types.
pub(super) fn record_type_name(type_node: tree_sitter::Node<'_>, source: &str) -> Option<String> {
    match type_node.kind() {
        "struct_specifier" | "union_specifier" | "enum_specifier" | "class_specifier" => type_node
            .child_by_field_name("name")
            .and_then(|n| node_text(n, source))
            .map(str::to_string),
        "type_identifier" => node_text(type_node, source).map(str::to_string),
        _ => {
            let mut cursor = type_node.walk();
            let found = type_node
                .children(&mut cursor)
                .find_map(|child| record_type_name(child, source));
            found
        }
    }
}

/// Unwrap pointer/array/init/function declarators to the base identifier node
/// and its text.
pub(super) fn declarator_identifier<'a>(
    node: tree_sitter::Node<'a>,
    source: &'a str,
) -> Option<(tree_sitter::Node<'a>, &'a str)> {
    match node.kind() {
        "identifier" | "field_identifier" | "type_identifier" => {
            node_text(node, source).map(|text| (node, text))
        }
        _ => node
            .child_by_field_name("declarator")
            .and_then(|inner| declarator_identifier(inner, source))
            .or_else(|| declarator_identifier_deep(node, source)),
    }
}

/// Tree-sitter C classifies names from its built-in typedef set (for example
/// `size_t`) as `primitive_type` even when that token is the declarator being
/// defined. Accept that grammar-specific shape only at a typedef declarator
/// boundary; the shared declaration walker must not treat arbitrary primitive
/// type nodes as bindings.
pub(super) fn typedef_declarator_identifier<'a>(
    node: tree_sitter::Node<'a>,
    source: &'a str,
) -> Option<(tree_sitter::Node<'a>, &'a str)> {
    if node.kind() == "primitive_type" {
        return node_text(node, source).map(|text| (node, text));
    }
    declarator_identifier(node, source)
}

pub(super) fn declarator_identifier_deep<'a>(
    node: tree_sitter::Node<'a>,
    source: &'a str,
) -> Option<(tree_sitter::Node<'a>, &'a str)> {
    if matches!(node.kind(), "parameter_list" | "parameter_declaration") {
        return None;
    }
    if matches!(
        node.kind(),
        "identifier" | "field_identifier" | "type_identifier"
    ) {
        return node_text(node, source).map(|text| (node, text));
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(found) = declarator_identifier_deep(child, source) {
            return Some(found);
        }
    }
    None
}

pub(super) fn node_text<'a>(node: tree_sitter::Node<'_>, source: &'a str) -> Option<&'a str> {
    node.utf8_text(source.as_bytes())
        .ok()
        .filter(|t| !t.is_empty())
}

pub(super) fn byte_to_utf16_col(source: &str, line_start_byte: usize, target_byte: usize) -> usize {
    if target_byte <= line_start_byte {
        return 0;
    }
    let s = &source[line_start_byte..std::cmp::min(target_byte, source.len())];
    utf16_units(s) as usize
}

pub(super) fn utf16_units(text: &str) -> u32 {
    // Fast path: ASCII text uses the same number of UTF-16 code units as bytes.
    if text.is_ascii() {
        return text.len() as u32;
    }
    text.chars().map(|ch| ch.len_utf16() as u32).sum()
}
