use super::lexical::{compact_whitespace, make_symbol};
use super::{
    AliasTarget, FieldDef, LocalBinding, LocalBindingKind, LocalDeclaration, MemberConfidence,
    MemberDef, MemberKind, Occurrence, ParseFacts, RecordConfidence, RecordDef, RecordKind, Symbol,
    SymbolKind, SymbolRole, SyntacticRole, TypeAlias,
};

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
            if let Some(body) = node.child_by_field_name("body") {
                let tag_name = node
                    .child_by_field_name("name")
                    .and_then(|n| node_text(n, source))
                    .map(|s| s.to_string());

                let typedef_name = parent_typedef_name(node, source);

                if tag_name.is_some() || typedef_name.is_some() {
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

                    out.records.push(RecordDef {
                        record_key: record_key.clone(),
                        display_name,
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

                    collect_body_members(
                        body,
                        &record_key,
                        source,
                        line_starts,
                        &mut out.fields,
                        &mut out.members,
                    );
                }
            }
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
            if let Some(type_node) = node.child_by_field_name("type") {
                if let Some(target) = get_alias_target(type_node, source) {
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
                                out.aliases.push(TypeAlias {
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
            }
        } else if facts.contains(ParseFacts::LOCAL_DECLS)
            && matches!(node.kind(), "declaration" | "parameter_declaration")
        {
            // Record-typed local/parameter bindings, captured for positional
            // receiver inference. The byte offset of each declared identifier lets
            // `infer_receiver_record` pick the nearest declaration before a cursor.
            if let Some(type_name) = node
                .child_by_field_name("type")
                .and_then(|t| record_type_name(t, source))
            {
                let mut cursor = node.walk();
                for decl in node.children_by_field_name("declarator", &mut cursor) {
                    if let Some((id_node, name)) = declarator_identifier(decl, source) {
                        out.local_declarations.push(LocalDeclaration {
                            name: name.to_string(),
                            record_type: type_name.clone(),
                            decl_start_byte: id_node.start_byte(),
                        });
                    }
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    out
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

fn collect_function_local_bindings(
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

/// Build an `Occurrence` from an `identifier` / `type_identifier` node. Tree-sitter
/// keeps these out of comments and string literals, so no manual skipping is
/// needed. `None` for an empty/unreadable node.
fn occurrence_at(
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

fn collect_body_members(
    body: tree_sitter::Node<'_>,
    record_key: &str,
    source: &str,
    line_starts: &[usize],
    fields: &mut Vec<FieldDef>,
    members: &mut Vec<MemberDef>,
) {
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
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
                            source,
                            line_starts,
                            fields,
                            members,
                        );
                    }
                }
            }
            continue;
        }

        let signature = compact_whitespace(child.utf8_text(source.as_bytes()).unwrap_or_default());
        for decl in declarators {
            if let Some((id_node, name)) = declarator_identifier(decl, source) {
                let kind = method_member_kind(child, decl, source);
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

fn collect_out_of_class_method_member(
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

fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn push_member(
    members: &mut Vec<MemberDef>,
    record_key: String,
    name: String,
    id_node: tree_sitter::Node<'_>,
    kind: MemberKind,
    confidence: MemberConfidence,
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
        signature,
        source,
        line_starts,
    );
}

fn push_member_at_byte(
    members: &mut Vec<MemberDef>,
    record_key: String,
    name: String,
    start_byte: usize,
    kind: MemberKind,
    confidence: MemberConfidence,
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
        _ => None,
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
            .and_then(|inner| declarator_identifier(inner, source)),
    }
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
