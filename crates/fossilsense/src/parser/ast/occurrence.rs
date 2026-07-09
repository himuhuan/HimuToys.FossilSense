use crate::parser::{Occurrence, SyntacticRole};

use super::{byte_to_utf16_col, utf16_units};

/// Build an `Occurrence` from an `identifier` / `type_identifier` node. Tree-sitter
/// keeps these out of comments and string literals, so no manual skipping is
/// needed. `None` for an empty/unreadable node.
pub(super) fn occurrence_at(
    node: tree_sitter::Node<'_>,
    source: &str,
    line_starts: &[usize],
) -> Option<Occurrence> {
    let text = node.utf8_text(source.as_bytes()).ok()?;
    if text.is_empty() {
        return None;
    }
    let start = node.start_position();
    let start_byte = node.start_byte();
    let line_start = line_starts.get(start.row).copied().unwrap_or_else(|| {
        source[..start_byte]
            .rfind('\n')
            .map(|index| index + 1)
            .unwrap_or(0)
    });
    Some(Occurrence {
        name: text.to_string(),
        start_byte,
        line: start.row as u32,
        start_col: byte_to_utf16_col(source, line_start, start_byte) as u32,
        length: utf16_units(text),
        role: classify_occurrence_role(node),
    })
}

/// Classify the syntactic role of an `identifier` / `type_identifier` node from
/// its position in the tree. This is purely lexical/structural -- no semantic
/// binding -- and any shape we do not recognize falls back to `Read`, so an
/// unfamiliar construct or a parse-error region never yields a wrong-but-
/// confident role.
fn classify_occurrence_role(node: tree_sitter::Node<'_>) -> SyntacticRole {
    // Type position is encoded by tree-sitter as a distinct node kind.
    if node.kind() == "type_identifier" {
        return SyntacticRole::TypeUse;
    }
    let Some(parent) = node.parent() else {
        return SyntacticRole::Read;
    };
    match parent.kind() {
        "call_expression" => field_is(parent, "function", node, SyntacticRole::Call),
        "assignment_expression" => field_is(parent, "left", node, SyntacticRole::Write),
        "update_expression" => field_is(parent, "argument", node, SyntacticRole::Write),
        // Defining sites: enum constant name and macro name.
        "enumerator" => field_is(parent, "name", node, SyntacticRole::Definition),
        "preproc_def" | "preproc_function_def" => {
            field_is(parent, "name", node, SyntacticRole::Definition)
        }
        // Binding declarations are reached through one or more declarator wrappers.
        _ => binding_role(node).unwrap_or(SyntacticRole::Read),
    }
}

/// `role` when `node` is exactly the `field` child of `parent`, else `Read`.
fn field_is(
    parent: tree_sitter::Node<'_>,
    field: &str,
    node: tree_sitter::Node<'_>,
    role: SyntacticRole,
) -> SyntacticRole {
    if parent.child_by_field_name(field) == Some(node) {
        role
    } else {
        SyntacticRole::Read
    }
}

/// Walk up the declarator chain: a node reached only through `declarator` fields
/// up to a declaration/definition is a binding occurrence. Ascending through any
/// non-declarator field (e.g. an initializer `value`) returns `None`, so the
/// declared name is classified but the initializer expression is not.
fn binding_role(node: tree_sitter::Node<'_>) -> Option<SyntacticRole> {
    let mut cur = node;
    loop {
        let parent = cur.parent()?;
        match parent.kind() {
            "pointer_declarator"
            | "array_declarator"
            | "init_declarator"
            | "function_declarator"
            | "parenthesized_declarator"
            | "reference_declarator" => {
                if parent.child_by_field_name("declarator") == Some(cur) {
                    cur = parent;
                } else {
                    return None;
                }
            }
            "declaration" | "field_declaration" | "parameter_declaration" => {
                return (parent.child_by_field_name("declarator") == Some(cur))
                    .then_some(SyntacticRole::Declaration);
            }
            "function_definition" => {
                return (parent.child_by_field_name("declarator") == Some(cur))
                    .then_some(SyntacticRole::Definition);
            }
            _ => return None,
        }
    }
}
