use super::lexical::make_symbol;
use super::{
    FieldDef, LocalBinding, LocalDeclaration, MemberDef, Occurrence, ParseFacts, RecordDef, Symbol,
    SymbolKind, SymbolRole, TypeAlias,
};

mod alias;
mod local;
mod occurrence;
mod record;

use alias::collect_type_aliases;
use local::{collect_function_local_bindings, collect_record_local_declarations};
use occurrence::occurrence_at;
use record::{collect_out_of_class_method_member, collect_record_and_members};

pub use local::infer_receiver_record;

pub(super) struct AstIndex {
    pub(super) parse_error_count: usize,
    pub(super) occurrences: Vec<Occurrence>,
    pub(super) fields: Vec<FieldDef>,
    pub(super) members: Vec<MemberDef>,
    pub(super) enum_constants: Vec<Symbol>,
    pub(super) aliases: Vec<TypeAlias>,
    pub(super) records: Vec<RecordDef>,
    pub(super) local_declarations: Vec<LocalDeclaration>,
    pub(super) local_bindings: Vec<LocalBinding>,
}

/// Collect AST-only index data in one iterative pass. This keeps indexing fast
/// on large workspaces and avoids recursive Rust stack use on deep syntax trees.
pub(super) fn collect_ast_index(
    root: tree_sitter::Node<'_>,
    source: &str,
    line_starts: &[usize],
    facts: ParseFacts,
) -> AstIndex {
    let mut out = AstIndex {
        parse_error_count: 0,
        occurrences: Vec::new(),
        fields: Vec::new(),
        members: Vec::new(),
        enum_constants: Vec::new(),
        aliases: Vec::new(),
        records: Vec::new(),
        local_declarations: Vec::new(),
        local_bindings: Vec::new(),
    };
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        out.parse_error_count += usize::from(node.is_error() || node.is_missing());

        // Identifier occurrences (coloring + reference roles). Skipped when
        // the caller does not need them (e.g. index-time bulk parsing).
        if facts.contains(ParseFacts::OCCURRENCES)
            && matches!(node.kind(), "identifier" | "type_identifier")
        {
            if let Some(occ) = occurrence_at(node, source, line_starts) {
                out.occurrences.push(occ);
            }
        }

        if facts.contains(ParseFacts::LOCAL_DECLS) && node.kind() == "function_definition" {
            collect_function_local_bindings(node, source, &mut out.local_bindings);
        }

        if facts.contains(ParseFacts::FIELDS)
            && matches!(node.kind(), "function_definition" | "declaration")
        {
            collect_out_of_class_method_member(node, source, line_starts, &mut out.members);
        }

        // Record + member collection. Gated by either bit since both are
        // extracted from the same struct/union/class body.
        if facts.intersects(ParseFacts::RECORDS | ParseFacts::FIELDS)
            && matches!(
                node.kind(),
                "struct_specifier" | "union_specifier" | "class_specifier"
            )
        {
            collect_record_and_members(
                node,
                source,
                line_starts,
                facts.contains(ParseFacts::FIELDS),
                &mut out.records,
                &mut out.fields,
                &mut out.members,
            );
        } else if node.kind() == "enumerator" {
            let id = node.child_by_field_name("name").unwrap_or(node);
            if let Some(name) = node_text(id, source) {
                let row = id.start_position().row;
                out.enum_constants.push(make_symbol(
                    name,
                    SymbolKind::EnumConstant,
                    SymbolRole::Definition,
                    row,
                    row,
                    line_starts,
                    source,
                    name.to_string(),
                    None,
                ));
            }
        } else if facts.contains(ParseFacts::ALIASES) && node.kind() == "type_definition" {
            collect_type_aliases(node, source, line_starts, &mut out.aliases);
        } else if facts.contains(ParseFacts::LOCAL_DECLS)
            && matches!(node.kind(), "declaration" | "parameter_declaration")
        {
            collect_record_local_declarations(node, source, &mut out.local_declarations);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    out
}

/// Name of a record type node: the tag of a struct/union/enum specifier, or the
/// text of a plain `type_identifier` (typedef). `None` for primitive and other
/// non-record types.
fn record_type_name(type_node: tree_sitter::Node<'_>, source: &str) -> Option<String> {
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
fn declarator_identifier<'a>(
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

fn declarator_identifier_deep<'a>(
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

fn node_text<'a>(node: tree_sitter::Node<'_>, source: &'a str) -> Option<&'a str> {
    node.utf8_text(source.as_bytes())
        .ok()
        .filter(|t| !t.is_empty())
}

fn byte_to_utf16_col(source: &str, line_start_byte: usize, target_byte: usize) -> usize {
    if target_byte <= line_start_byte {
        return 0;
    }
    let s = &source[line_start_byte..std::cmp::min(target_byte, source.len())];
    utf16_units(s) as usize
}

fn utf16_units(text: &str) -> u32 {
    // Fast path: ASCII text uses the same number of UTF-16 code units as bytes.
    if text.is_ascii() {
        return text.len() as u32;
    }
    text.chars().map(|ch| ch.len_utf16() as u32).sum()
}
