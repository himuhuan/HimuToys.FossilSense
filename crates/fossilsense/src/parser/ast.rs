use std::path::Path;

use super::lexical::compact_whitespace;
use super::{
    AliasTarget, AliasTargetFidelity, DeclaratorShape, FieldDef, LocalBinding, LocalBindingKind,
    LocalDeclaration, MemberConfidence, MemberDef, MemberKind, Occurrence, ParseFacts,
    RecordConfidence, RecordDef, RecordKind, RecordRangeFidelity, SymbolKind, SymbolRole,
    SyntacticRole, TypeAlias,
};
use crate::call_model::{SourcePosition, SourceRange};
use crate::config::SourceLanguage;
mod aliases;
mod bindings;
mod members;
mod objects;

use aliases::*;
pub use bindings::infer_receiver_record;
use bindings::{collect_function_local_bindings, occurrence_at, symbol_from_name_node};
use members::*;
use objects::*;

use crate::semantic_model::{
    DeclarationBacking, DeclarationFact, DeclarationIdentity, DeclarationLocator, LanguageFidelity,
    LogicalEntityKey, SemanticDeclarationKind, SemanticDeclarationRole, SemanticFactFidelity,
    SemanticFactProvenance, SemanticLanguage,
};

pub(super) struct AstIndex {
    pub(super) parse_error_count: usize,
    pub(super) declarations: Vec<DeclarationFact>,
    pub(super) type_symbols: Vec<super::RawDeclaration>,
    pub(super) occurrences: Vec<Occurrence>,
    pub(super) fields: Vec<FieldDef>,
    pub(super) members: Vec<MemberDef>,
    pub(super) enum_constants: Vec<super::RawDeclaration>,
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
    language: SourceLanguage,
) -> AstIndex {
    let mut out = AstIndex {
        parse_error_count: 0,
        declarations: Vec::new(),
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
        .intersects(ParseFacts::DECLARATIONS | ParseFacts::CALL_RELATIONS)
        .then(|| {
            super::callables::CallFactCollector::new(
                path,
                source,
                line_starts,
                language,
                facts.contains(ParseFacts::CALL_RELATIONS),
            )
        });
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
        if facts.contains(ParseFacts::DECLARATIONS)
            && node.kind() == "declaration"
            && is_namespace_or_file_scope_declaration(node)
        {
            collect_object_declarations(
                node,
                path,
                source,
                line_starts,
                language,
                &mut out.declarations,
            );
        }
        if facts.contains(ParseFacts::DECLARATIONS)
            && matches!(node.kind(), "preproc_def" | "preproc_function_def")
        {
            collect_macro_declaration(
                node,
                path,
                source,
                line_starts,
                language,
                &mut out.declarations,
            );
        }

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

        if facts.contains(ParseFacts::DECLARATIONS)
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
        if facts.intersects(ParseFacts::DECLARATIONS | ParseFacts::RECORDS | ParseFacts::FIELDS)
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

                let typedef = parent_typedef_name_node(node, source);
                let typedef_name = typedef.as_ref().map(|(_, name)| name.clone());

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
                    let name_range = name_node
                        .or_else(|| typedef.as_ref().map(|(node, _)| *node))
                        .map(|node| source_range(node, source, line_starts))
                        .unwrap_or_else(|| source_range(node, source, line_starts));
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
                        name_range,
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
            if let Some(symbol) = symbol_from_name_node(
                id,
                SymbolKind::EnumConstant,
                SymbolRole::Definition,
                node,
                source,
                line_starts,
            ) {
                out.enum_constants.push(symbol);
            }
        } else if facts.intersects(ParseFacts::DECLARATIONS | ParseFacts::ALIASES)
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
                            if facts.contains(ParseFacts::DECLARATIONS) {
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
                            if facts.intersects(ParseFacts::DECLARATIONS | ParseFacts::ALIASES) {
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
        } else if facts.intersects(ParseFacts::DECLARATIONS | ParseFacts::ALIASES)
            && node.kind() == "alias_declaration"
        {
            collect_cpp_alias_declaration(
                node,
                path,
                source,
                line_starts,
                facts,
                &mut out.type_symbols,
                &mut out.aliases,
            );
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
        let collected = collector.finish();
        out.callable_anchors = collected.anchors;
        if facts.contains(ParseFacts::CALL_RELATIONS) {
            out.call_sites = collected.call_sites;
        }
    }
    out
}
