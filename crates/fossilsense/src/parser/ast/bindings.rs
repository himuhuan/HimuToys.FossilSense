use super::*;

pub(super) fn symbol_from_name_node(
    name_node: tree_sitter::Node<'_>,
    kind: SymbolKind,
    role: SymbolRole,
    declaration_node: tree_sitter::Node<'_>,
    source: &str,
    line_starts: &[usize],
) -> Option<super::super::RawDeclaration> {
    let name = node_text(name_node, source)?;
    if name.is_empty() || crate::language_builtins::is_language_keyword(name) {
        return None;
    }
    let start = name_node.start_position();
    let end = name_node.end_position();
    let start_byte = name_node.start_byte();
    let end_byte = name_node.end_byte();

    // Navigation symbols carry the exact identifier token range. This makes
    // name provenance mechanically checkable and prevents a guessed name from
    // pointing at the beginning of an enclosing multi-line declaration.
    if source.get(start_byte..end_byte) != Some(name) {
        return None;
    }

    Some(super::super::RawDeclaration {
        name: name.to_string(),
        kind,
        role,
        start_byte,
        end_byte,
        start_line: start.row,
        start_col: byte_to_utf16_col(
            source,
            line_starts.get(start.row).copied().unwrap_or(0),
            start_byte,
        ),
        end_line: end.row,
        end_col: byte_to_utf16_col(
            source,
            line_starts.get(end.row).copied().unwrap_or(0),
            end_byte,
        ),
        signature: compact_whitespace(
            declaration_node
                .utf8_text(source.as_bytes())
                .unwrap_or(name),
        ),
        tag_kind: None,
        guard: None,
        container: None,
        incomplete: contains_error_or_missing(declaration_node)
            || error_or_missing_ancestor(declaration_node),
    })
}

pub(super) fn error_or_missing_ancestor(mut node: tree_sitter::Node<'_>) -> bool {
    while let Some(parent) = node.parent() {
        if parent.is_error() || parent.is_missing() {
            return true;
        }
        node = parent;
    }
    false
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

pub(super) fn collect_parameter_bindings(
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

pub(super) fn collect_local_variable_bindings(
    body: tree_sitter::Node<'_>,
    source: &str,
    function_start_byte: usize,
    function_end_byte: usize,
    out: &mut Vec<LocalBinding>,
) {
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        if node.kind() == "declaration" {
            let (scope_start_byte, scope_end_byte) =
                nearest_compound_scope(node, function_start_byte, function_end_byte);
            push_binding_declarators(
                node,
                source,
                LocalBindingKind::LocalVariable,
                function_start_byte,
                function_end_byte,
                scope_start_byte,
                scope_end_byte,
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

#[allow(clippy::too_many_arguments)]
pub(super) fn push_binding_declarators(
    declaration: tree_sitter::Node<'_>,
    source: &str,
    kind: LocalBindingKind,
    function_start_byte: usize,
    function_end_byte: usize,
    scope_start_byte: usize,
    scope_end_byte: usize,
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
                scope_start_byte,
                scope_end_byte,
            });
        }
    }
}

pub(super) fn nearest_compound_scope(
    declaration: tree_sitter::Node<'_>,
    function_start_byte: usize,
    function_end_byte: usize,
) -> (usize, usize) {
    let mut parent = declaration.parent();
    while let Some(node) = parent {
        if node.kind() == "compound_statement" {
            return (node.start_byte(), node.end_byte());
        }
        // Initializer/condition declarations belong to the control statement,
        // not to the surrounding compound block. Declarations inside a braced
        // body encounter that body's compound statement first.
        if matches!(
            node.kind(),
            "for_statement" | "if_statement" | "switch_statement" | "while_statement"
        ) {
            return (node.start_byte(), node.end_byte());
        }
        if node.kind() == "function_definition" {
            break;
        }
        parent = node.parent();
    }
    (function_start_byte, function_end_byte)
}

pub(super) fn binding_type_text(node: tree_sitter::Node<'_>, source: &str) -> Option<String> {
    node.child_by_field_name("type")
        .and_then(|type_node| type_node.utf8_text(source.as_bytes()).ok())
        .map(compact_whitespace)
        .filter(|text| !text.is_empty())
}

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
/// its position in the tree. This is purely lexical/structural — no semantic
/// binding — and any shape we do not recognize falls back to `Read`, so an
/// unfamiliar construct or a parse-error region never yields a wrong-but-
/// confident role.
pub(super) fn classify_occurrence_role(node: tree_sitter::Node<'_>) -> SyntacticRole {
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
pub(super) fn field_is(
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
pub(super) fn binding_role(node: tree_sitter::Node<'_>) -> Option<SyntacticRole> {
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
