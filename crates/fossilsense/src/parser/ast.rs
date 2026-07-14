use std::path::Path;

use super::lexical::{compact_whitespace, make_symbol};
use super::{
    AliasTarget, AliasTargetFidelity, DeclaratorShape, FieldDef, LocalBinding, LocalBindingKind,
    LocalDeclaration, MemberConfidence, MemberDef, MemberKind, Occurrence, ParseFacts,
    RecordConfidence, RecordDef, RecordKind, RecordRangeFidelity, Symbol, SymbolKind, SymbolRole,
    SyntacticRole, TypeAlias,
};
use crate::call_model::{SourcePosition, SourceRange};

pub(super) struct AstIndex {
    pub(super) parse_error_count: usize,
    pub(super) type_symbols: Vec<Symbol>,
    pub(super) occurrences: Vec<Occurrence>,
    pub(super) fields: Vec<FieldDef>,
    pub(super) members: Vec<MemberDef>,
    pub(super) enum_constants: Vec<Symbol>,
    pub(super) aliases: Vec<TypeAlias>,
    pub(super) records: Vec<RecordDef>,
    pub(super) local_declarations: Vec<LocalDeclaration>,
    pub(super) local_bindings: Vec<LocalBinding>,
    pub(super) callable_anchors: Vec<crate::call_model::CallableAnchor>,
    pub(super) call_sites: Vec<crate::call_model::CallSiteFact>,
}

/// Collect AST-only index data in one iterative pass. This keeps indexing fast
/// on large workspaces and avoids recursive Rust stack use on deep syntax trees.
pub(super) fn collect_ast_index(
    root: tree_sitter::Node<'_>,
    path: &Path,
    source: &str,
    line_starts: &[usize],
    facts: ParseFacts,
) -> AstIndex {
    let mut out = AstIndex {
        parse_error_count: 0,
        type_symbols: Vec::new(),
        occurrences: Vec::new(),
        fields: Vec::new(),
        members: Vec::new(),
        enum_constants: Vec::new(),
        aliases: Vec::new(),
        records: Vec::new(),
        local_declarations: Vec::new(),
        local_bindings: Vec::new(),
        callable_anchors: Vec::new(),
        call_sites: Vec::new(),
    };
    enum Visit<'tree> {
        Enter(tree_sitter::Node<'tree>),
        Exit(tree_sitter::Node<'tree>),
    }
    let mut call_collector = facts
        .contains(ParseFacts::CALL_RELATIONS)
        .then(|| super::callables::CallFactCollector::new(path, source, line_starts));
    let mut stack = vec![Visit::Enter(root)];
    while let Some(visit) = stack.pop() {
        let node = match visit {
            Visit::Enter(node) => node,
            Visit::Exit(node) => {
                if let Some(collector) = call_collector.as_mut() {
                    collector.exit(node);
                }
                continue;
            }
        };
        if let Some(collector) = call_collector.as_mut() {
            collector.enter(node);
        }
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

        if facts.contains(ParseFacts::SYMBOLS)
            && matches!(
                node.kind(),
                "struct_specifier" | "union_specifier" | "enum_specifier" | "class_specifier"
            )
            && node.child_by_field_name("body").is_some()
        {
            if let Some(name) = node.child_by_field_name("name") {
                if let Some(symbol) = symbol_from_name_node(
                    name,
                    SymbolKind::Type,
                    SymbolRole::Definition,
                    node,
                    source,
                    line_starts,
                ) {
                    out.type_symbols.push(symbol);
                }
            }
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
                let name_node = node.child_by_field_name("name");
                let tag_name = name_node
                    .and_then(|name| node_text(name, source))
                    .map(str::to_string);

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
                    let declaration = enclosing_record_declaration(node).unwrap_or(node);
                    let declaration_range = record_declaration_range(node, source, line_starts);
                    let declaration_hash = source_range_hash(source, declaration_range);
                    let range_fidelity = if contains_error_or_missing(declaration) {
                        RecordRangeFidelity::Malformed
                    } else {
                        RecordRangeFidelity::AstExact
                    };

                    out.records.push(RecordDef {
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
                        body_range: source_range(body, source, line_starts),
                        declaration_range,
                        declaration_hash,
                        range_fidelity,
                        confidence,
                        signature,
                    });

                    if facts.contains(ParseFacts::FIELDS) {
                        collect_body_members(
                            body,
                            &record_key,
                            &display_name,
                            source,
                            line_starts,
                            &mut out.records,
                            &mut out.fields,
                            &mut out.members,
                        );
                    }
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
        } else if facts.intersects(ParseFacts::SYMBOLS | ParseFacts::ALIASES)
            && node.kind() == "type_definition"
        {
            if let Some(type_node) = node.child_by_field_name("type") {
                if let Some(target) = get_alias_target(type_node, source) {
                    let mut cursor = node.walk();
                    let declarators: Vec<_> = node
                        .children_by_field_name("declarator", &mut cursor)
                        .collect();
                    let underlying_spelling = alias_underlying_spelling(
                        node,
                        type_node,
                        declarators.first().copied(),
                        source,
                    );
                    let base_qualifiers = typedef_base_qualifiers(node, source);
                    let declaration_range = source_range(node, source, line_starts);
                    let declaration_hash = source_range_hash(source, declaration_range);
                    let target_fidelity = if contains_error_or_missing(node) {
                        AliasTargetFidelity::Malformed
                    } else {
                        AliasTargetFidelity::AstExact
                    };
                    let path_text = path.to_string_lossy().replace('\\', "/");
                    for decl in declarators {
                        if let Some((alias_node, alias)) =
                            typedef_declarator_identifier(decl, source)
                        {
                            if facts.contains(ParseFacts::SYMBOLS) {
                                if let Some(symbol) = symbol_from_name_node(
                                    alias_node,
                                    SymbolKind::Type,
                                    SymbolRole::Definition,
                                    node,
                                    source,
                                    line_starts,
                                ) {
                                    out.type_symbols.push(symbol);
                                }
                            }
                            if facts.contains(ParseFacts::ALIASES) {
                                let alias_start = alias_node.start_position();
                                let alias_end = alias_node.end_position();
                                let declarator_shape = if target_fidelity
                                    == AliasTargetFidelity::Malformed
                                    || contains_error_or_missing(decl)
                                {
                                    DeclaratorShape::Unsupported
                                } else {
                                    typedef_declarator_shape(decl, source, &base_qualifiers)
                                };
                                let fingerprint = digest(&format!(
                                    "{}|{}|{}|{}|{:?}|{:?}",
                                    path_text,
                                    node.start_byte(),
                                    alias_node.start_byte(),
                                    alias,
                                    target,
                                    declarator_shape
                                ));
                                out.aliases.push(TypeAlias {
                                    alias: alias.to_string(),
                                    target: target.clone(),
                                    start_byte: alias_node.start_byte(),
                                    end_byte: alias_node.end_byte(),
                                    start_line: alias_start.row,
                                    start_col: byte_to_utf16_col(
                                        source,
                                        line_starts.get(alias_start.row).copied().unwrap_or(0),
                                        alias_node.start_byte(),
                                    ),
                                    end_line: alias_end.row,
                                    end_col: byte_to_utf16_col(
                                        source,
                                        line_starts.get(alias_end.row).copied().unwrap_or(0),
                                        alias_node.end_byte(),
                                    ),
                                    declaration_range,
                                    declaration_hash,
                                    underlying_spelling: underlying_spelling.clone(),
                                    declarator_shape,
                                    target_fidelity,
                                    fingerprint,
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
        if call_collector.is_some() {
            stack.push(Visit::Exit(node));
        }
        let mut cursor = node.walk();
        let children: Vec<_> = node.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push(Visit::Enter(child));
        }
    }
    if let Some(collector) = call_collector {
        let facts = collector.finish();
        out.callable_anchors = facts.anchors;
        out.call_sites = facts.call_sites;
    }
    out
}

fn symbol_from_name_node(
    name_node: tree_sitter::Node<'_>,
    kind: SymbolKind,
    role: SymbolRole,
    declaration_node: tree_sitter::Node<'_>,
    source: &str,
    line_starts: &[usize],
) -> Option<Symbol> {
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

    Some(Symbol {
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
        guard: None,
        container: None,
    })
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
    let enclosing_declaration = enclosing_record_declaration(type_node);
    let range_is_malformed = contains_error_or_missing(type_node)
        || enclosing_declaration.is_some_and(contains_error_or_missing);

    let declaration_range = record_declaration_range(type_node, source, line_starts);
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
        body_range: type_node
            .child_by_field_name("body")
            .map(|body| source_range(body, source, line_starts))
            .unwrap_or_else(|| source_range(type_node, source, line_starts)),
        declaration_range,
        declaration_hash: source_range_hash(source, declaration_range),
        range_fidelity: if range_is_malformed {
            RecordRangeFidelity::Malformed
        } else {
            RecordRangeFidelity::AstExact
        },
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

fn enclosing_record_declaration(record: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    let parent = record.parent()?;
    matches!(
        parent.kind(),
        "type_definition" | "declaration" | "field_declaration"
    )
    .then_some(parent)
}

fn record_declaration_range(
    record: tree_sitter::Node<'_>,
    source: &str,
    line_starts: &[usize],
) -> SourceRange {
    if let Some(declaration) = enclosing_record_declaration(record) {
        return source_range(declaration, source, line_starts);
    }

    // At translation-unit scope the C/C++ grammars expose a standalone record
    // specifier directly and keep its terminating semicolon as an unnamed
    // sibling. Join those two exact AST tokens rather than scanning arbitrary
    // following source text.
    if let Some(semicolon) = record.next_sibling().filter(|node| node.kind() == ";") {
        return source_range_bytes(
            record.start_byte(),
            semicolon.end_byte(),
            source,
            line_starts,
        );
    }

    source_range(record, source, line_starts)
}

fn source_range(node: tree_sitter::Node<'_>, source: &str, line_starts: &[usize]) -> SourceRange {
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

fn source_range_bytes(
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

fn alias_underlying_spelling(
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

fn strip_typedef_keyword(value: &str) -> &str {
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

fn typedef_base_qualifiers(node: tree_sitter::Node<'_>, source: &str) -> Vec<String> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|child| child.kind() == "type_qualifier")
        .filter_map(|child| node_text(child, source).map(str::to_string))
        .collect()
}

fn typedef_declarator_shape(
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

fn simple_alias_identifier(node: tree_sitter::Node<'_>) -> bool {
    matches!(
        node.kind(),
        "identifier" | "field_identifier" | "type_identifier" | "primitive_type"
    )
}

fn contains_error_or_missing(root: tree_sitter::Node<'_>) -> bool {
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

fn digest(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..24].to_string()
}

fn source_range_hash(source: &str, range: SourceRange) -> [u8; 32] {
    let bytes = source
        .as_bytes()
        .get(range.start_byte..range.end_byte)
        .unwrap_or_default();
    *blake3::hash(bytes).as_bytes()
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

/// Tree-sitter C classifies names from its built-in typedef set (for example
/// `size_t`) as `primitive_type` even when that token is the declarator being
/// defined. Accept that grammar-specific shape only at a typedef declarator
/// boundary; the shared declaration walker must not treat arbitrary primitive
/// type nodes as bindings.
fn typedef_declarator_identifier<'a>(
    node: tree_sitter::Node<'a>,
    source: &'a str,
) -> Option<(tree_sitter::Node<'a>, &'a str)> {
    if node.kind() == "primitive_type" {
        return node_text(node, source).map(|text| (node, text));
    }
    declarator_identifier(node, source)
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
