use std::collections::HashSet;
use std::path::Path;

use crate::call_model::{
    AnchorRole, CallForm, CallSiteFact, CallableAnchor, CallableKind, FactProvenance,
    LinkageDomain, OwnerKindHint, SignatureFidelity, SignatureShape, SourcePosition, SourceRange,
};
use crate::semantic_model::{
    AliasTarget, AliasTargetFidelity, CompletionKindHint, DeclarationBacking, DeclarationFact,
    DeclarationIdentity, DeclarationLocator, DeclaratorShape, FallbackCompletionFact, FieldDef,
    ImportFact, LanguageFidelity, LogicalEntityKey, MemberConfidence, MemberDef, MemberKind,
    Occurrence, PackageFact, RecordConfidence, RecordDef, RecordKind, RecordRangeFidelity,
    SemanticDeclarationKind, SemanticDeclarationRole, SemanticFactFidelity, SemanticFactProvenance,
    SemanticLanguage, SyntacticRole, TypeAlias,
};

use super::ast::AstIndex;
use super::{LocalBinding, LocalBindingKind, LocalDeclaration, ParseFacts};

pub(super) struct GoAstProduct {
    pub(super) ast: AstIndex,
    pub(super) package: Option<PackageFact>,
    pub(super) imports: Vec<ImportFact>,
    pub(super) build_guard: Option<String>,
}

#[derive(Clone)]
struct CallableScope {
    entity_key: String,
    body_range: SourceRange,
    anchor_available: bool,
}

enum Visit<'tree> {
    Enter(tree_sitter::Node<'tree>),
    Exit(tree_sitter::Node<'tree>),
}

pub(super) fn collect_go_ast_index(
    root: tree_sitter::Node<'_>,
    path: &Path,
    source: &str,
    line_starts: &[usize],
    facts: ParseFacts,
) -> GoAstProduct {
    let path_text = normalized_path(path);
    let build_guard = combine_build_guards(extract_build_guard(source), filename_build_guard(path));
    let package = collect_package(root, source, line_starts);
    let package_name = package
        .as_ref()
        .map(|package| package.name.as_str())
        .unwrap_or("<unknown>");
    let package_key = physical_package_key(&path_text, package_name);
    let imports = collect_imports(root, source, line_starts);
    let mut ast = empty_ast();
    let mut callable_stack = Vec::new();
    let mut global_initializer = None;
    let mut stack = vec![Visit::Enter(root)];

    while let Some(visit) = stack.pop() {
        let node = match visit {
            Visit::Enter(node) => node,
            Visit::Exit(node) => {
                if matches!(
                    node.kind(),
                    "function_declaration" | "method_declaration" | "func_literal"
                ) {
                    callable_stack.pop();
                }
                continue;
            }
        };

        ast.parse_error_count += usize::from(node.is_error() || node.is_missing());

        match node.kind() {
            "function_declaration" | "method_declaration" => {
                if let Some(anchor) = callable_anchor(
                    node,
                    &path_text,
                    package_name,
                    &package_key,
                    source,
                    line_starts,
                    build_guard.as_deref(),
                ) {
                    if let Some(owner) = anchor.owner.as_deref() {
                        if facts.intersects(ParseFacts::FIELDS | ParseFacts::RECORDS) {
                            ast.members
                                .push(method_member(owner, &package_key, &anchor));
                        }
                    }
                    if let Some(body_range) = anchor.body_range {
                        callable_stack.push(CallableScope {
                            entity_key: anchor.entity_key.clone(),
                            body_range,
                            anchor_available: true,
                        });
                    } else {
                        callable_stack.push(CallableScope {
                            entity_key: anchor.entity_key.clone(),
                            body_range: anchor.declaration_range,
                            anchor_available: true,
                        });
                    }
                    if facts.intersects(ParseFacts::DECLARATIONS | ParseFacts::CALL_RELATIONS) {
                        ast.callable_anchors.push(anchor);
                    }
                } else {
                    callable_stack.push(CallableScope {
                        entity_key: digest(&format!(
                            "go:malformed:{}:{}",
                            path_text,
                            node.start_byte()
                        )),
                        body_range: range(node, source, line_starts),
                        anchor_available: false,
                    });
                }
            }
            "func_literal" => {
                let anchor = function_literal_anchor(
                    node,
                    &path_text,
                    package_name,
                    &package_key,
                    source,
                    line_starts,
                    build_guard.as_deref(),
                );
                callable_stack.push(CallableScope {
                    entity_key: anchor.entity_key.clone(),
                    body_range: anchor.body_range.unwrap_or(anchor.declaration_range),
                    anchor_available: true,
                });
                if facts.contains(ParseFacts::CALL_RELATIONS) {
                    ast.callable_anchors.push(anchor);
                }
            }
            "type_spec" | "type_alias"
                if callable_stack.is_empty()
                    && facts.intersects(ParseFacts::DECLARATIONS | ParseFacts::RECORDS) =>
            {
                collect_type_spec(
                    node,
                    &path_text,
                    package_name,
                    &package_key,
                    source,
                    line_starts,
                    build_guard.as_deref(),
                    facts,
                    &mut ast,
                );
            }
            "var_spec" | "const_spec"
                if callable_stack.is_empty() && facts.contains(ParseFacts::DECLARATIONS) =>
            {
                collect_object_spec(
                    node,
                    &path_text,
                    package_name,
                    &package_key,
                    source,
                    line_starts,
                    build_guard.as_deref(),
                    &mut ast.declarations,
                );
            }
            "call_expression" if facts.contains(ParseFacts::CALL_RELATIONS) => {
                let caller_entity_key = if let Some(scope) =
                    callable_stack.last().filter(|scope| scope.anchor_available)
                {
                    scope.entity_key.clone()
                } else if callable_stack.is_empty() {
                    let scope = global_initializer.get_or_insert_with(|| {
                        let anchor = global_initializer_anchor(
                            node,
                            &path_text,
                            package_name,
                            &package_key,
                            source,
                            line_starts,
                            build_guard.as_deref(),
                        );
                        let scope = CallableScope {
                            entity_key: anchor.entity_key.clone(),
                            body_range: anchor.declaration_range,
                            anchor_available: true,
                        };
                        ast.callable_anchors.push(anchor);
                        scope
                    });
                    scope.entity_key.clone()
                } else {
                    String::new()
                };
                if caller_entity_key.is_empty() {
                    continue;
                }
                if let Some(call) = call_site(
                    node,
                    &path_text,
                    &caller_entity_key,
                    source,
                    line_starts,
                    build_guard.as_deref(),
                ) {
                    ast.call_sites.push(call);
                }
            }
            "parameter_declaration" | "variadic_parameter_declaration"
                if facts.contains(ParseFacts::LOCAL_DECLS) && !callable_stack.is_empty() =>
            {
                collect_local_binding(
                    node,
                    source,
                    callable_stack.last().expect("checked non-empty"),
                    LocalBindingKind::Parameter,
                    &mut ast.local_bindings,
                    &mut ast.local_declarations,
                );
            }
            "short_var_declaration" | "var_spec"
                if facts.contains(ParseFacts::LOCAL_DECLS) && !callable_stack.is_empty() =>
            {
                collect_local_binding(
                    node,
                    source,
                    callable_stack.last().expect("checked non-empty"),
                    LocalBindingKind::LocalVariable,
                    &mut ast.local_bindings,
                    &mut ast.local_declarations,
                );
            }
            "const_spec"
                if facts.contains(ParseFacts::LOCAL_DECLS) && !callable_stack.is_empty() =>
            {
                collect_local_binding(
                    node,
                    source,
                    callable_stack.last().expect("checked non-empty"),
                    LocalBindingKind::LocalConstant,
                    &mut ast.local_bindings,
                    &mut ast.local_declarations,
                );
            }
            "type_spec" | "type_alias"
                if facts.contains(ParseFacts::LOCAL_DECLS) && !callable_stack.is_empty() =>
            {
                collect_local_binding(
                    node,
                    source,
                    callable_stack.last().expect("checked non-empty"),
                    LocalBindingKind::LocalType,
                    &mut ast.local_bindings,
                    &mut ast.local_declarations,
                );
            }
            "identifier" | "field_identifier" | "type_identifier" | "package_identifier"
                if facts.contains(ParseFacts::OCCURRENCES) =>
            {
                if let Some(occurrence) = occurrence(node, source, line_starts) {
                    ast.occurrences.push(occurrence);
                }
            }
            _ => {}
        }

        stack.push(Visit::Exit(node));
        let mut cursor = node.walk();
        let children: Vec<_> = node.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push(Visit::Enter(child));
        }
    }

    GoAstProduct {
        ast,
        package,
        imports,
        build_guard,
    }
}

fn empty_ast() -> AstIndex {
    AstIndex {
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
    }
}

fn collect_package(
    root: tree_sitter::Node<'_>,
    source: &str,
    line_starts: &[usize],
) -> Option<PackageFact> {
    let package_clause = find_first(root, |node| node.kind() == "package_clause")?;
    let name = find_first(package_clause, |node| node.kind() == "package_identifier")?;
    Some(PackageFact {
        name: text(name, source)?.to_string(),
        name_range: range(name, source, line_starts),
    })
}

fn collect_imports(
    root: tree_sitter::Node<'_>,
    source: &str,
    line_starts: &[usize],
) -> Vec<ImportFact> {
    let mut imports = Vec::new();
    walk_named(root, |node| {
        if node.kind() != "import_spec" {
            return;
        }
        let path_node = node.child_by_field_name("path").or_else(|| {
            find_first(node, |child| {
                matches!(
                    child.kind(),
                    "interpreted_string_literal" | "raw_string_literal"
                )
            })
        });
        let Some(path_node) = path_node else {
            return;
        };
        let Some(raw_path) = text(path_node, source) else {
            return;
        };
        let path = unquote_import_path(raw_path);
        if path.is_empty() {
            return;
        }
        let alias = import_alias(node, path_node, source);
        imports.push(ImportFact {
            path,
            alias,
            path_range: range(path_node, source, line_starts),
            declaration_range: range(node, source, line_starts),
        });
    });
    imports.sort_by_key(|import| import.declaration_range.start_byte);
    imports
}

fn import_alias(
    node: tree_sitter::Node<'_>,
    path_node: tree_sitter::Node<'_>,
    source: &str,
) -> Option<String> {
    if let Some(name) = node.child_by_field_name("name") {
        return text(name, source).map(str::to_string);
    }
    let prefix = source
        .get(node.start_byte()..path_node.start_byte())
        .unwrap_or_default()
        .trim();
    (!prefix.is_empty()).then(|| {
        prefix
            .split_whitespace()
            .last()
            .unwrap_or(prefix)
            .to_string()
    })
}

#[allow(clippy::too_many_arguments)]
fn collect_type_spec(
    node: tree_sitter::Node<'_>,
    path: &str,
    package_name: &str,
    package_key: &str,
    source: &str,
    line_starts: &[usize],
    guard: Option<&str>,
    facts: ParseFacts,
    ast: &mut AstIndex,
) {
    let Some(name_node) = node
        .child_by_field_name("name")
        .or_else(|| find_first(node, |child| child.kind() == "type_identifier"))
    else {
        return;
    };
    let Some(name) = text(name_node, source) else {
        return;
    };
    let type_node = node.child_by_field_name("type").or_else(|| {
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .filter(|child| child.id() != name_node.id())
            .last()
    });
    let declaration_range = range(node, source, line_starts);
    let name_range = range(name_node, source, line_starts);
    let signature = compact(
        source
            .get(node.start_byte()..node.end_byte())
            .unwrap_or_default(),
    );
    let is_alias = node.kind() == "type_alias"
        || type_node.is_some_and(|type_node| {
            source
                .get(name_node.end_byte()..type_node.start_byte())
                .is_some_and(|between| between.contains('='))
        });
    let kind = if is_alias {
        SemanticDeclarationKind::Alias
    } else {
        SemanticDeclarationKind::Type
    };
    let record_key = format!("go:{package_key}:{name}");
    let record_kind = type_node.and_then(|node| match node.kind() {
        "struct_type" => Some(RecordKind::Struct),
        "interface_type" => Some(RecordKind::Interface),
        _ => None,
    });
    let backing = if let Some(record_kind) = record_kind {
        if facts.intersects(ParseFacts::RECORDS | ParseFacts::FIELDS) {
            let type_node = type_node.expect("checked");
            let record = record_fact(
                node,
                type_node,
                name_node,
                name,
                &record_key,
                record_kind,
                source,
                line_starts,
            );
            if facts.contains(ParseFacts::FIELDS) {
                match record_kind {
                    RecordKind::Interface => collect_interface_members(
                        type_node,
                        &record_key,
                        source,
                        line_starts,
                        &mut ast.members,
                    ),
                    _ => collect_struct_members(
                        type_node,
                        &record_key,
                        source,
                        line_starts,
                        &mut ast.fields,
                        &mut ast.members,
                    ),
                }
            }
            ast.records.push(record);
        }
        DeclarationBacking::Record {
            record_key: record_key.clone(),
        }
    } else if is_alias {
        let target_text = type_node
            .and_then(|type_node| text(type_node, source))
            .map(compact)
            .unwrap_or_default();
        let fingerprint = digest(&format!(
            "go-alias|{package_key}|{name}|{}",
            declaration_range.start_byte
        ));
        ast.aliases.push(TypeAlias {
            alias: name.to_string(),
            target: AliasTarget::UnresolvedTypeName(target_text.clone()),
            start_byte: name_range.start_byte,
            end_byte: name_range.end_byte,
            start_line: name_range.start.line as usize,
            start_col: name_range.start.character as usize,
            end_line: name_range.end.line as usize,
            end_col: name_range.end.character as usize,
            declaration_range,
            declaration_hash: range_hash(source, declaration_range),
            underlying_spelling: target_text,
            declarator_shape: DeclaratorShape::Identity,
            target_fidelity: if contains_error(node) {
                AliasTargetFidelity::Malformed
            } else {
                AliasTargetFidelity::AstExact
            },
            fingerprint: fingerprint.clone(),
        });
        DeclarationBacking::TypeAlias { fingerprint }
    } else {
        DeclarationBacking::SourceRange {
            range: declaration_range,
        }
    };

    if facts.contains(ParseFacts::DECLARATIONS) {
        let fingerprint = match &backing {
            DeclarationBacking::Record { record_key } => {
                digest(&format!("record|{path}|{record_key}"))
            }
            DeclarationBacking::TypeAlias { fingerprint } => fingerprint.clone(),
            _ => digest(&format!(
                "go-type|{package_key}|{name}|{}",
                declaration_range.start_byte
            )),
        };
        ast.declarations.push(declaration(
            path,
            package_name,
            package_key,
            name,
            kind,
            name_range,
            declaration_range,
            Some(signature),
            None,
            guard,
            fingerprint,
            backing,
            contains_error(node),
            None,
        ));
    }
}

fn record_fact(
    declaration: tree_sitter::Node<'_>,
    type_node: tree_sitter::Node<'_>,
    name_node: tree_sitter::Node<'_>,
    name: &str,
    record_key: &str,
    kind: RecordKind,
    source: &str,
    line_starts: &[usize],
) -> RecordDef {
    let declaration_range = range(declaration, source, line_starts);
    let body_range = range(type_node, source, line_starts);
    let start = type_node.start_position();
    let end = type_node.end_position();
    RecordDef {
        record_key: record_key.to_string(),
        display_name: name.to_string(),
        tag_name: Some(name.to_string()),
        typedef_name: None,
        kind,
        start_byte: type_node.start_byte(),
        end_byte: type_node.end_byte(),
        start_line: start.row,
        start_col: range(type_node, source, line_starts).start.character as usize,
        end_line: end.row,
        end_col: range(type_node, source, line_starts).end.character as usize,
        name_range: range(name_node, source, line_starts),
        body_range,
        declaration_range,
        declaration_hash: range_hash(source, declaration_range),
        range_fidelity: if contains_error(declaration) {
            RecordRangeFidelity::Malformed
        } else {
            RecordRangeFidelity::AstExact
        },
        confidence: RecordConfidence::NamedTag,
        signature: compact(
            source
                .get(declaration.start_byte()..declaration.end_byte())
                .unwrap_or_default(),
        ),
    }
}

fn collect_interface_members(
    node: tree_sitter::Node<'_>,
    record_key: &str,
    source: &str,
    line_starts: &[usize],
    members: &mut Vec<MemberDef>,
) {
    let mut cursor = node.walk();
    for child in node
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "method_elem")
    {
        let Some(name_node) = child.child_by_field_name("name") else {
            continue;
        };
        let Some(name) = text(name_node, source) else {
            continue;
        };
        let name_range = range(name_node, source, line_starts);
        members.push(MemberDef {
            record_key: record_key.to_string(),
            name: name.to_string(),
            kind: MemberKind::Method,
            confidence: MemberConfidence::InBody,
            type_name: None,
            start_byte: name_range.start_byte,
            end_byte: name_range.end_byte,
            start_line: name_range.start.line as usize,
            start_col: name_range.start.character as usize,
            end_line: name_range.end.line as usize,
            end_col: name_range.end.character as usize,
            signature: compact(
                source
                    .get(child.start_byte()..child.end_byte())
                    .unwrap_or_default(),
            ),
        });
    }
}

fn collect_struct_members(
    node: tree_sitter::Node<'_>,
    record_key: &str,
    source: &str,
    line_starts: &[usize],
    fields: &mut Vec<FieldDef>,
    members: &mut Vec<MemberDef>,
) {
    let Some(list) = direct_named_child(node, "field_declaration_list") else {
        return;
    };
    let mut cursor = list.walk();
    for child in list
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "field_declaration")
    {
        let type_text = child
            .child_by_field_name("type")
            .and_then(|type_node| text(type_node, source))
            .map(compact);
        let mut names = field_nodes(child, "name", &["field_identifier", "identifier"]);
        if names.is_empty() {
            if let Some(embedded_name) = child
                .child_by_field_name("type")
                .and_then(embedded_field_name_node)
            {
                names.push(embedded_name);
            }
        }
        for name_node in names {
            let Some(name) = text(name_node, source) else {
                continue;
            };
            let name_range = range(name_node, source, line_starts);
            let signature = compact(
                source
                    .get(child.start_byte()..child.end_byte())
                    .unwrap_or_default(),
            );
            fields.push(FieldDef {
                record_key: record_key.to_string(),
                name: name.to_string(),
                start_byte: name_range.start_byte,
                end_byte: name_range.end_byte,
                start_line: name_range.start.line as usize,
                start_col: name_range.start.character as usize,
                end_line: name_range.end.line as usize,
                end_col: name_range.end.character as usize,
                signature: signature.clone(),
            });
            members.push(MemberDef {
                record_key: record_key.to_string(),
                name: name.to_string(),
                kind: MemberKind::Field,
                confidence: MemberConfidence::InBody,
                type_name: type_text.clone(),
                start_byte: name_range.start_byte,
                end_byte: name_range.end_byte,
                start_line: name_range.start.line as usize,
                start_col: name_range.start.character as usize,
                end_line: name_range.end.line as usize,
                end_col: name_range.end.character as usize,
                signature,
            });
        }
    }
}

fn embedded_field_name_node(type_node: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    match type_node.kind() {
        "type_identifier" | "field_identifier" => Some(type_node),
        "pointer_type" => type_node
            .child_by_field_name("type")
            .or_else(|| first_named_child(type_node))
            .and_then(embedded_field_name_node),
        "generic_type" => type_node
            .child_by_field_name("type")
            .or_else(|| {
                let mut cursor = type_node.walk();
                let child = type_node
                    .named_children(&mut cursor)
                    .find(|child| child.kind() != "type_arguments");
                child
            })
            .and_then(embedded_field_name_node),
        "qualified_type" => type_node.child_by_field_name("name").or_else(|| {
            let mut cursor = type_node.walk();
            let child = type_node.named_children(&mut cursor).find(|child| {
                matches!(child.kind(), "type_identifier" | "field_identifier")
                    && child.id()
                        != type_node
                            .child_by_field_name("package")
                            .map(|package| package.id())
                            .unwrap_or(usize::MAX)
            });
            child
        }),
        _ => type_node
            .child_by_field_name("name")
            .or_else(|| type_node.child_by_field_name("type"))
            .and_then(embedded_field_name_node),
    }
}

fn first_named_child(node: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    let mut cursor = node.walk();
    let child = node.named_children(&mut cursor).next();
    child
}

fn direct_named_child<'tree>(
    node: tree_sitter::Node<'tree>,
    kind: &str,
) -> Option<tree_sitter::Node<'tree>> {
    let mut cursor = node.walk();
    let child = node
        .named_children(&mut cursor)
        .find(|child| child.kind() == kind);
    child
}

fn method_member(owner: &str, package_key: &str, anchor: &CallableAnchor) -> MemberDef {
    MemberDef {
        record_key: format!("go:{package_key}:{owner}"),
        name: anchor.name.clone(),
        kind: MemberKind::Method,
        confidence: MemberConfidence::OutOfClassOwner,
        type_name: None,
        start_byte: anchor.name_range.start_byte,
        end_byte: anchor.name_range.end_byte,
        start_line: anchor.name_range.start.line as usize,
        start_col: anchor.name_range.start.character as usize,
        end_line: anchor.name_range.end.line as usize,
        end_col: anchor.name_range.end.character as usize,
        signature: anchor.presentation_signature.clone(),
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_object_spec(
    node: tree_sitter::Node<'_>,
    path: &str,
    package_name: &str,
    package_key: &str,
    source: &str,
    line_starts: &[usize],
    guard: Option<&str>,
    declarations: &mut Vec<DeclarationFact>,
) {
    let declaration_range = range(node, source, line_starts);
    let signature = compact(
        source
            .get(node.start_byte()..node.end_byte())
            .unwrap_or_default(),
    );
    let names = field_nodes(node, "name", &["identifier"]);
    for name_node in names {
        let Some(name) = text(name_node, source) else {
            continue;
        };
        let name_range = range(name_node, source, line_starts);
        declarations.push(declaration(
            path,
            package_name,
            package_key,
            name,
            SemanticDeclarationKind::Object,
            name_range,
            declaration_range,
            Some(signature.clone()),
            None,
            guard,
            digest(&format!(
                "go-object|{package_key}|{name}|{}",
                name_range.start_byte
            )),
            DeclarationBacking::SourceRange { range: name_range },
            contains_error(node),
            Some(has_initializer(node, source)),
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn callable_anchor(
    node: tree_sitter::Node<'_>,
    path: &str,
    package_name: &str,
    package_key: &str,
    source: &str,
    line_starts: &[usize],
    guard: Option<&str>,
) -> Option<CallableAnchor> {
    let name_node = node.child_by_field_name("name")?;
    let name = text(name_node, source)?;
    let owner = (node.kind() == "method_declaration")
        .then(|| node.child_by_field_name("receiver"))
        .flatten()
        .and_then(receiver_type)
        .and_then(|node| text(node, source))
        .map(strip_generic_arguments);
    let qualified_name = match owner.as_deref() {
        Some(owner) => format!("{package_name}::{owner}::{name}"),
        None => format!("{package_name}::{name}"),
    };
    let parameters = node.child_by_field_name("parameters");
    let (min_arity, variadic) = parameters.map(parameter_shape).unwrap_or((0, false));
    let body = node.child_by_field_name("body");
    let header_end = body.map_or(node.end_byte(), |body| body.start_byte());
    let canonical_signature = compact(
        source
            .get(node.start_byte()..header_end)
            .unwrap_or_default(),
    );
    let declaration_range = range(node, source, line_starts);
    let signature_fidelity = if contains_error(node) {
        SignatureFidelity::Malformed
    } else {
        SignatureFidelity::AstExact
    };
    let entity_key = if name == "init" && owner.is_none() {
        digest(&format!(
            "go:{package_key}:init:{path}:{}",
            declaration_range.start_byte
        ))
    } else {
        digest(&format!(
            "go:{package_key}:{}:{name}",
            owner.as_deref().unwrap_or("")
        ))
    };
    let anchor_fingerprint = digest(&format!(
        "go-anchor|{path}|{entity_key}|{}",
        declaration_range.start_byte
    ));
    Some(CallableAnchor {
        path: path.to_string(),
        name: name.to_string(),
        qualified_name,
        owner,
        owner_kind: (node.kind() == "method_declaration").then_some(OwnerKindHint::Record),
        kind: CallableKind::Function,
        role: if body.is_some() {
            AnchorRole::Definition
        } else {
            AnchorRole::Declaration
        },
        linkage: LinkageDomain::Package(package_key.to_string()),
        signature: SignatureShape {
            normalized: canonical_signature.clone(),
            min_arity: Some(min_arity),
            max_arity: (!variadic).then_some(min_arity),
            variadic,
        },
        canonical_signature: canonical_signature.clone(),
        presentation_signature: canonical_signature,
        signature_fidelity,
        name_range: range(name_node, source, line_starts),
        declaration_range,
        body_range: body.map(|body| range(body, source, line_starts)),
        guard: guard.map(str::to_string),
        provenance: FactProvenance::Ast,
        syntax_error_overlap: contains_error(node),
        entity_key,
        anchor_fingerprint,
    })
}

#[allow(clippy::too_many_arguments)]
fn function_literal_anchor(
    node: tree_sitter::Node<'_>,
    path: &str,
    package_name: &str,
    package_key: &str,
    source: &str,
    line_starts: &[usize],
    guard: Option<&str>,
) -> CallableAnchor {
    let declaration_range = range(node, source, line_starts);
    let body = node.child_by_field_name("body");
    let body_range = body.map(|body| range(body, source, line_starts));
    let header_end = body.map_or(node.end_byte(), |body| body.start_byte());
    let signature = compact(
        source
            .get(node.start_byte()..header_end)
            .unwrap_or_default(),
    );
    let entity_key = digest(&format!(
        "go-lambda|{path}|{}|{}",
        declaration_range.start_byte, declaration_range.end_byte
    ));
    CallableAnchor {
        path: path.to_string(),
        name: "<function literal>".to_string(),
        qualified_name: format!(
            "{package_name}::<function literal@{}>",
            declaration_range.start_byte
        ),
        owner: None,
        owner_kind: None,
        kind: CallableKind::SyntheticLambda,
        role: AnchorRole::Synthetic,
        linkage: LinkageDomain::Package(package_key.to_string()),
        signature: SignatureShape {
            normalized: signature.clone(),
            min_arity: None,
            max_arity: None,
            variadic: false,
        },
        canonical_signature: signature.clone(),
        presentation_signature: signature,
        signature_fidelity: if contains_error(node) {
            SignatureFidelity::Malformed
        } else {
            SignatureFidelity::AstExact
        },
        name_range: declaration_range,
        declaration_range,
        body_range,
        guard: guard.map(str::to_string),
        provenance: FactProvenance::Synthetic,
        syntax_error_overlap: contains_error(node),
        anchor_fingerprint: digest(&format!(
            "go-lambda-anchor|{path}|{entity_key}|{}",
            declaration_range.start_byte
        )),
        entity_key,
    }
}

#[allow(clippy::too_many_arguments)]
fn global_initializer_anchor(
    call: tree_sitter::Node<'_>,
    path: &str,
    package_name: &str,
    package_key: &str,
    source: &str,
    line_starts: &[usize],
    guard: Option<&str>,
) -> CallableAnchor {
    let declaration_range = range(call, source, line_starts);
    let entity_key = digest(&format!("go-initializer|{path}|{package_key}"));
    CallableAnchor {
        path: path.to_string(),
        name: "<package initialization>".to_string(),
        qualified_name: format!("{package_name}::<package initialization:{path}>"),
        owner: None,
        owner_kind: None,
        kind: CallableKind::SyntheticGlobalInitializer,
        role: AnchorRole::Synthetic,
        linkage: LinkageDomain::Package(package_key.to_string()),
        signature: SignatureShape {
            normalized: String::new(),
            min_arity: Some(0),
            max_arity: Some(0),
            variadic: false,
        },
        canonical_signature: String::new(),
        presentation_signature: String::new(),
        signature_fidelity: SignatureFidelity::AstExact,
        name_range: declaration_range,
        declaration_range,
        body_range: None,
        guard: guard.map(str::to_string),
        provenance: FactProvenance::Synthetic,
        syntax_error_overlap: contains_error(call),
        anchor_fingerprint: digest(&format!("go-initializer-anchor|{path}|{entity_key}")),
        entity_key,
    }
}

fn parameter_shape(parameters: tree_sitter::Node<'_>) -> (u32, bool) {
    let mut arity = 0u32;
    let mut variadic = false;
    let mut cursor = parameters.walk();
    for child in parameters.named_children(&mut cursor) {
        if !matches!(
            child.kind(),
            "parameter_declaration" | "variadic_parameter_declaration"
        ) {
            continue;
        }
        variadic |= child.kind() == "variadic_parameter_declaration";
        let names = field_nodes(child, "name", &["identifier"]);
        arity = arity.saturating_add(names.len().max(1) as u32);
    }
    (arity, variadic)
}

fn receiver_type(receiver: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    find_first(receiver, |node| node.kind() == "type_identifier")
}

fn strip_generic_arguments(value: &str) -> String {
    value
        .split_once('[')
        .map_or(value, |(base, _)| base)
        .trim_start_matches('*')
        .to_string()
}

fn call_site(
    node: tree_sitter::Node<'_>,
    path: &str,
    caller_entity_key: &str,
    source: &str,
    line_starts: &[usize],
    guard: Option<&str>,
) -> Option<CallSiteFact> {
    let function = node.child_by_field_name("function")?;
    let expression_range = range(node, source, line_starts);
    let (callee, form) = match function.kind() {
        "identifier" => (function, CallForm::DirectName),
        "selector_expression" => (
            function
                .child_by_field_name("field")
                .or_else(|| find_first(function, |child| child.kind() == "field_identifier"))?,
            CallForm::QualifiedName,
        ),
        "parenthesized_expression" => {
            let identifier = find_first(function, |child| child.kind() == "identifier")?;
            (identifier, CallForm::ParenthesizedName)
        }
        _ => (function, CallForm::Unsupported),
    };
    let callee_name = text(callee, source).map(str::to_string);
    let arguments = node.child_by_field_name("arguments");
    let argument_count = arguments.map(|arguments| {
        let mut cursor = arguments.walk();
        arguments.named_children(&mut cursor).count() as u32
    });
    // Go selector receivers need package/type binding before they can be
    // compared with a declaration's canonical qualified name. Retaining the
    // source spelling here (including plain direct names) would make the
    // generic call resolver reject every package-qualified anchor as unequal.
    // Keep the call form as evidence, but leave qualification unresolved so
    // call hierarchy conservatively returns same-name candidates.
    let qualified_name = None;
    Some(CallSiteFact {
        path: path.to_string(),
        caller_entity_key: caller_entity_key.to_string(),
        expression_range,
        callee_range: range(callee, source, line_starts),
        callee_name,
        qualified_name,
        form,
        argument_count,
        guard: guard.map(str::to_string),
        provenance: FactProvenance::Ast,
        syntax_error_overlap: contains_error(node),
        site_fingerprint: digest(&format!(
            "go-call|{path}|{caller_entity_key}|{}",
            expression_range.start_byte
        )),
    })
}

fn collect_local_binding(
    node: tree_sitter::Node<'_>,
    source: &str,
    scope: &CallableScope,
    kind: LocalBindingKind,
    bindings: &mut Vec<LocalBinding>,
    declarations: &mut Vec<LocalDeclaration>,
) {
    let type_text = node
        .child_by_field_name("type")
        .and_then(|type_node| text(type_node, source))
        .map(compact);
    let names = binding_name_nodes(node);
    let (scope_start_byte, scope_end_byte) = nearest_lexical_scope(node)
        .map(|scope| (scope.start_byte(), scope.end_byte()))
        .unwrap_or((scope.body_range.start_byte, scope.body_range.end_byte));
    for name_node in names {
        let Some(name) = text(name_node, source) else {
            continue;
        };
        bindings.push(LocalBinding {
            name: name.to_string(),
            kind,
            type_text: type_text.clone(),
            decl_start_byte: name_node.start_byte(),
            function_start_byte: scope.body_range.start_byte,
            function_end_byte: scope.body_range.end_byte,
            scope_start_byte,
            scope_end_byte,
        });
        if let Some(record_type) = type_text.as_deref().and_then(simple_type_name) {
            declarations.push(LocalDeclaration {
                name: name.to_string(),
                record_type,
                decl_start_byte: name_node.start_byte(),
            });
        }
    }
}

fn binding_name_nodes(node: tree_sitter::Node<'_>) -> Vec<tree_sitter::Node<'_>> {
    if node.kind() != "short_var_declaration" {
        return field_nodes(node, "name", &["identifier"]);
    }
    let Some(left) = node.child_by_field_name("left") else {
        return Vec::new();
    };
    let mut output = Vec::new();
    let mut stack = vec![left];
    while let Some(current) = stack.pop() {
        if current.kind() == "identifier" {
            output.push(current);
            continue;
        }
        let mut cursor = current.walk();
        let children: Vec<_> = current.named_children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    output
}

fn nearest_lexical_scope(node: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if matches!(
            parent.kind(),
            "block"
                | "if_statement"
                | "for_statement"
                | "expression_switch_statement"
                | "type_switch_statement"
                | "select_statement"
                | "expression_case"
                | "type_case"
                | "communication_case"
        ) {
            return Some(parent);
        }
        current = parent.parent();
    }
    None
}

fn simple_type_name(value: &str) -> Option<String> {
    let value = value.trim().trim_start_matches('*');
    let value = value.split_once('[').map_or(value, |(base, _)| base);
    (!value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_'))
    .then(|| value.to_string())
}

fn occurrence(
    node: tree_sitter::Node<'_>,
    source: &str,
    line_starts: &[usize],
) -> Option<Occurrence> {
    let name = text(node, source)?;
    if name.is_empty() {
        return None;
    }
    let node_range = range(node, source, line_starts);
    let role = syntactic_role(node);
    Some(Occurrence {
        name: name.to_string(),
        start_byte: node.start_byte(),
        line: node_range.start.line,
        start_col: node_range.start.character,
        length: name.encode_utf16().count() as u32,
        role,
    })
}

fn syntactic_role(node: tree_sitter::Node<'_>) -> SyntacticRole {
    let Some(parent) = node.parent() else {
        return SyntacticRole::Read;
    };
    if matches!(
        parent.kind(),
        "function_declaration"
            | "method_declaration"
            | "type_spec"
            | "var_spec"
            | "const_spec"
            | "parameter_declaration"
            | "variadic_parameter_declaration"
            | "field_declaration"
            | "short_var_declaration"
    ) && parent
        .child_by_field_name("name")
        .is_some_and(|name| name.id() == node.id())
    {
        return SyntacticRole::Definition;
    }
    if parent.kind() == "call_expression"
        && parent
            .child_by_field_name("function")
            .is_some_and(|function| {
                function.id() == node.id()
                    || function
                        .child_by_field_name("field")
                        .is_some_and(|field| field.id() == node.id())
            })
    {
        return SyntacticRole::Call;
    }
    if node.kind() == "type_identifier" {
        return SyntacticRole::TypeUse;
    }
    if matches!(
        parent.kind(),
        "assignment_statement" | "short_var_declaration" | "inc_statement" | "dec_statement"
    ) && parent.child_by_field_name("left").is_some_and(|left| {
        left.start_byte() <= node.start_byte() && node.end_byte() <= left.end_byte()
    }) {
        return SyntacticRole::Write;
    }
    SyntacticRole::Read
}

#[allow(clippy::too_many_arguments)]
fn declaration(
    path: &str,
    package_name: &str,
    package_key: &str,
    name: &str,
    kind: SemanticDeclarationKind,
    name_range: SourceRange,
    declaration_range: SourceRange,
    signature: Option<String>,
    owner: Option<String>,
    guard: Option<&str>,
    fingerprint: String,
    backing: DeclarationBacking,
    incomplete: bool,
    has_initializer: Option<bool>,
) -> DeclarationFact {
    let qualified_name = match owner.as_deref() {
        Some(owner) => format!("{package_name}::{owner}::{name}"),
        None => format!("{package_name}::{name}"),
    };
    let linkage = LinkageDomain::Package(package_key.to_string());
    let locator_fingerprint = digest(&format!(
        "go-locator|{path}|{}|{}|{fingerprint}",
        declaration_range.start_byte, declaration_range.end_byte
    ));
    DeclarationFact {
        identity: DeclarationIdentity {
            locator: DeclarationLocator {
                workspace_id: String::new(),
                path: path.to_string(),
                range: declaration_range,
                fingerprint: locator_fingerprint,
            },
            logical_key: LogicalEntityKey {
                qualified_name: qualified_name.clone(),
                declaration_kind: kind,
                owner: owner.clone(),
                // Go has no declaration overloading. Parameter names,
                // initializer text, and build constraints remain evidence on
                // the fact but must not split one package entity.
                canonical_signature: None,
                linkage_domain: format!("package:{package_key}"),
                guard_fingerprint: None,
            },
            language: SemanticLanguage::Go,
            language_fidelity: LanguageFidelity::Explicit,
            provenance: SemanticFactProvenance::Ast,
            fact_fidelity: if incomplete {
                SemanticFactFidelity::Incomplete
            } else {
                SemanticFactFidelity::Authoritative
            },
            role: SemanticDeclarationRole::Definition,
        },
        name: name.to_string(),
        qualified_name,
        declaration_kind: kind,
        role: SemanticDeclarationRole::Definition,
        path: path.to_string(),
        name_range,
        declaration_range,
        canonical_signature: signature,
        declarator_shape: None,
        has_initializer,
        owner,
        linkage,
        guard: guard.map(str::to_string),
        backing,
    }
}

fn has_initializer(node: tree_sitter::Node<'_>, source: &str) -> bool {
    source
        .get(node.start_byte()..node.end_byte())
        .is_some_and(|text| text.contains('='))
        || node.child_by_field_name("value").is_some()
}

pub(super) fn extract_build_guard(source: &str) -> Option<String> {
    let mut go_build = Vec::new();
    let mut legacy = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(expression) = trimmed.strip_prefix("//go:build") {
            let expression = expression.trim();
            if !expression.is_empty() {
                go_build.push(expression.to_string());
            }
            continue;
        }
        if let Some(expression) = trimmed.strip_prefix("// +build") {
            let expression = expression.trim();
            if !expression.is_empty() {
                legacy.push(expression.to_string());
            }
            continue;
        }
        if trimmed.starts_with("package ") {
            break;
        }
        if !trimmed.is_empty() && !trimmed.starts_with("//") {
            break;
        }
    }
    match go_build.as_slice() {
        [] => (!legacy.is_empty()).then(|| legacy.join(" && ")),
        [expression] => Some(expression.clone()),
        expressions => Some(format!(
            "conflicting //go:build directives: {}",
            expressions.join(" | ")
        )),
    }
}

pub(super) fn combine_build_guards(
    source_guard: Option<String>,
    filename_guard: Option<String>,
) -> Option<String> {
    match (source_guard, filename_guard) {
        (Some(source), Some(filename)) => Some(format!("({source}) && ({filename})")),
        (Some(source), None) => Some(source),
        (None, Some(filename)) => Some(filename),
        (None, None) => None,
    }
}

pub(super) fn filename_build_guard(path: &Path) -> Option<String> {
    const KNOWN_OS: &[&str] = &[
        "aix",
        "android",
        "darwin",
        "dragonfly",
        "freebsd",
        "hurd",
        "illumos",
        "ios",
        "js",
        "linux",
        "nacl",
        "netbsd",
        "openbsd",
        "plan9",
        "solaris",
        "wasip1",
        "windows",
        "zos",
    ];
    const KNOWN_ARCH: &[&str] = &[
        "386",
        "amd64",
        "amd64p32",
        "arm",
        "armbe",
        "arm64",
        "arm64be",
        "loong64",
        "mips",
        "mipsle",
        "mips64",
        "mips64le",
        "mips64p32",
        "mips64p32le",
        "ppc",
        "ppc64",
        "ppc64le",
        "riscv",
        "riscv64",
        "s390",
        "s390x",
        "sparc",
        "sparc64",
        "wasm",
    ];

    let stem = path.file_stem()?.to_str()?;
    let stem = stem.strip_suffix("_test").unwrap_or(stem);
    let (base, last) = stem.rsplit_once('_')?;
    if base.is_empty() {
        return None;
    }
    let is_os = |value: &str| KNOWN_OS.contains(&value);
    let is_arch = |value: &str| KNOWN_ARCH.contains(&value);
    let expression = if is_arch(last) {
        base.rsplit_once('_')
            .filter(|(basename, os)| !basename.is_empty() && is_os(os))
            .map(|(_, os)| os)
            .map_or_else(|| last.to_string(), |os| format!("{os} && {last}"))
    } else if is_os(last) {
        last.to_string()
    } else {
        return None;
    };
    Some(format!("filename: {expression}"))
}

pub(super) fn fallback_completions(source: &str) -> Vec<FallbackCompletionFact> {
    const LIMIT: usize = 512;
    const KEYWORDS: &[&str] = &[
        "break",
        "default",
        "func",
        "interface",
        "select",
        "case",
        "defer",
        "go",
        "map",
        "struct",
        "chan",
        "else",
        "goto",
        "package",
        "switch",
        "const",
        "fallthrough",
        "if",
        "range",
        "type",
        "continue",
        "for",
        "import",
        "return",
        "var",
    ];
    let line_starts = super::line_starts(source);
    let mut output = Vec::new();
    let mut seen = HashSet::new();
    let bytes = source.as_bytes();
    let mut index = 0;
    while index < bytes.len() && output.len() < LIMIT {
        if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'/') {
            index = bytes[index..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |offset| index + offset + 1);
            continue;
        }
        if !is_identifier_start(bytes[index]) {
            index += 1;
            continue;
        }
        let start = index;
        index += 1;
        while index < bytes.len() && is_identifier_continue(bytes[index]) {
            index += 1;
        }
        let name = &source[start..index];
        if KEYWORDS.contains(&name) || !seen.insert(name.to_string()) {
            continue;
        }
        let line = line_starts.partition_point(|line_start| *line_start <= start) - 1;
        let line_start = line_starts.get(line).copied().unwrap_or(0);
        output.push(FallbackCompletionFact {
            name: name.to_string(),
            kind_hint: CompletionKindHint::Object,
            range: SourceRange {
                start: SourcePosition {
                    line: line as u32,
                    character: utf16_col(source, line_start, start) as u32,
                },
                end: SourcePosition {
                    line: line as u32,
                    character: utf16_col(source, line_start, index) as u32,
                },
                start_byte: start,
                end_byte: index,
            },
            detail: Some("Go lexical fallback".to_string()),
        });
    }
    output
}

fn is_identifier_start(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphabetic()
}

fn is_identifier_continue(byte: u8) -> bool {
    is_identifier_start(byte) || byte.is_ascii_digit()
}

fn field_nodes<'tree>(
    node: tree_sitter::Node<'tree>,
    field: &str,
    fallback_kinds: &[&str],
) -> Vec<tree_sitter::Node<'tree>> {
    let mut cursor = node.walk();
    let fields: Vec<_> = node.children_by_field_name(field, &mut cursor).collect();
    if !fields.is_empty() {
        return fields;
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .take_while(|child| fallback_kinds.contains(&child.kind()))
        .collect()
}

fn find_first<'tree>(
    root: tree_sitter::Node<'tree>,
    predicate: impl Fn(tree_sitter::Node<'tree>) -> bool,
) -> Option<tree_sitter::Node<'tree>> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.id() != root.id() && predicate(node) {
            return Some(node);
        }
        let mut cursor = node.walk();
        let children: Vec<_> = node.named_children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    None
}

fn walk_named(root: tree_sitter::Node<'_>, mut visitor: impl FnMut(tree_sitter::Node<'_>)) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        visitor(node);
        let mut cursor = node.walk();
        let children: Vec<_> = node.named_children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
}

fn contains_error(root: tree_sitter::Node<'_>) -> bool {
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

fn text<'a>(node: tree_sitter::Node<'_>, source: &'a str) -> Option<&'a str> {
    source.get(node.start_byte()..node.end_byte())
}

fn range(node: tree_sitter::Node<'_>, source: &str, line_starts: &[usize]) -> SourceRange {
    let start = node.start_position();
    let end = node.end_position();
    let start_line = line_starts.get(start.row).copied().unwrap_or(0);
    let end_line = line_starts.get(end.row).copied().unwrap_or(0);
    SourceRange {
        start: SourcePosition {
            line: start.row as u32,
            character: utf16_col(source, start_line, node.start_byte()) as u32,
        },
        end: SourcePosition {
            line: end.row as u32,
            character: utf16_col(source, end_line, node.end_byte()) as u32,
        },
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
    }
}

fn utf16_col(source: &str, line_start: usize, byte: usize) -> usize {
    source
        .get(line_start..byte)
        .unwrap_or_default()
        .encode_utf16()
        .count()
}

fn range_hash(source: &str, range: SourceRange) -> [u8; 32] {
    *blake3::hash(
        source
            .get(range.start_byte..range.end_byte)
            .unwrap_or_default()
            .as_bytes(),
    )
    .as_bytes()
}

fn normalized_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn physical_package_key(path: &str, package_name: &str) -> String {
    let directory = path
        .rsplit_once('/')
        .map(|(directory, _)| directory)
        .filter(|directory| !directory.is_empty())
        .unwrap_or(".");
    format!("{directory}#{package_name}")
}

fn compact(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn digest(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..24].to_string()
}

fn unquote_import_path(value: &str) -> String {
    let value = value.trim();
    if value.starts_with('`') && value.ends_with('`') && value.len() >= 2 {
        return value[1..value.len() - 1].to_string();
    }
    serde_json::from_str::<String>(value).unwrap_or_else(|_| value.trim_matches('"').to_string())
}
