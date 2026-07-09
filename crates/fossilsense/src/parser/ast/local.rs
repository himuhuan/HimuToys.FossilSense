use crate::parser::lexical::compact_whitespace;
use crate::parser::{LocalBinding, LocalBindingKind, LocalDeclaration};

use super::{declarator_identifier, record_type_name};

pub(super) fn collect_record_local_declarations(
    node: tree_sitter::Node<'_>,
    source: &str,
    out: &mut Vec<LocalDeclaration>,
) {
    // Record-typed local/parameter bindings, captured for positional receiver
    // inference. The byte offset of each declared identifier lets
    // `infer_receiver_record` pick the nearest declaration before a cursor.
    if let Some(type_name) = node
        .child_by_field_name("type")
        .and_then(|type_node| record_type_name(type_node, source))
    {
        let mut cursor = node.walk();
        for decl in node.children_by_field_name("declarator", &mut cursor) {
            if let Some((id_node, name)) = declarator_identifier(decl, source) {
                out.push(LocalDeclaration {
                    name: name.to_string(),
                    record_type: type_name.clone(),
                    decl_start_byte: id_node.start_byte(),
                });
            }
        }
    }
}

/// Infer the record type of `receiver_name` from the nearest record-typed local
/// or parameter declaration whose declared identifier begins before `byte_offset`.
/// `decls` comes from `FileSemanticIndex::local_declarations`, so this is a pure
/// positional query with no parse of its own. Returns the record name (tag or
/// typedef) so the caller can resolve its fields; `None` when no such declaration
/// exists (the caller then falls back to the global field list).
pub fn infer_receiver_record(
    decls: &[LocalDeclaration],
    receiver_name: &str,
    byte_offset: usize,
) -> Option<String> {
    decls
        .iter()
        .filter(|decl| decl.name == receiver_name && decl.decl_start_byte < byte_offset)
        .max_by_key(|decl| decl.decl_start_byte)
        .map(|decl| decl.record_type.clone())
}

pub(super) fn collect_function_local_bindings(
    function: tree_sitter::Node<'_>,
    source: &str,
    out: &mut Vec<LocalBinding>,
) {
    let Some(body) = function.child_by_field_name("body") else {
        return;
    };
    let function_start_byte = body.start_byte();
    let function_end_byte = body.end_byte();

    if let Some(declarator) = function.child_by_field_name("declarator") {
        collect_parameter_bindings(
            declarator,
            source,
            function_start_byte,
            function_end_byte,
            out,
        );
    }

    collect_local_variable_bindings(body, source, function_start_byte, function_end_byte, out);
}

fn collect_parameter_bindings(
    root: tree_sitter::Node<'_>,
    source: &str,
    function_start_byte: usize,
    function_end_byte: usize,
    out: &mut Vec<LocalBinding>,
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "parameter_declaration" {
            push_binding_declarators(
                node,
                source,
                LocalBindingKind::Parameter,
                function_start_byte,
                function_end_byte,
                out,
            );
            continue;
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
}

fn collect_local_variable_bindings(
    body: tree_sitter::Node<'_>,
    source: &str,
    function_start_byte: usize,
    function_end_byte: usize,
    out: &mut Vec<LocalBinding>,
) {
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        if node.kind() == "declaration" {
            push_binding_declarators(
                node,
                source,
                LocalBindingKind::LocalVariable,
                function_start_byte,
                function_end_byte,
                out,
            );
            continue;
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
}

fn push_binding_declarators(
    declaration: tree_sitter::Node<'_>,
    source: &str,
    kind: LocalBindingKind,
    function_start_byte: usize,
    function_end_byte: usize,
    out: &mut Vec<LocalBinding>,
) {
    let type_text = binding_type_text(declaration, source);
    let mut cursor = declaration.walk();
    for declarator in declaration.children_by_field_name("declarator", &mut cursor) {
        if let Some((id_node, name)) = declarator_identifier(declarator, source) {
            out.push(LocalBinding {
                name: name.to_string(),
                kind,
                type_text: type_text.clone(),
                decl_start_byte: id_node.start_byte(),
                function_start_byte,
                function_end_byte,
            });
        }
    }
}

fn binding_type_text(node: tree_sitter::Node<'_>, source: &str) -> Option<String> {
    node.child_by_field_name("type")
        .and_then(|type_node| type_node.utf8_text(source.as_bytes()).ok())
        .map(compact_whitespace)
        .filter(|text| !text.is_empty())
}
