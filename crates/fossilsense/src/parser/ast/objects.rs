use super::*;

pub(super) fn collect_macro_declaration(
    node: tree_sitter::Node<'_>,
    path: &Path,
    source: &str,
    line_starts: &[usize],
    language: SourceLanguage,
    declarations: &mut Vec<DeclarationFact>,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let Some(name) = node_text(name_node, source) else {
        return;
    };
    let path_text = path.to_string_lossy().replace('\\', "/");
    let name_range = source_range(name_node, source, line_starts);
    let declaration_range = source_range(node, source, line_starts);
    let signature = node_text(node, source).map(compact_whitespace);
    let fingerprint = digest(&format!(
        "macro|{}|{}|{}|{}",
        path_text,
        name,
        node.start_byte(),
        node.end_byte()
    ));
    declarations.push(DeclarationFact {
        identity: DeclarationIdentity {
            locator: DeclarationLocator {
                workspace_id: String::new(),
                path: path_text.clone(),
                range: declaration_range,
                fingerprint,
            },
            logical_key: LogicalEntityKey {
                qualified_name: name.to_string(),
                declaration_kind: SemanticDeclarationKind::Macro,
                owner: None,
                canonical_signature: signature.clone(),
                linkage_domain: "external".to_string(),
                guard_fingerprint: None,
            },
            language: match language {
                SourceLanguage::C => SemanticLanguage::C,
                SourceLanguage::Cpp => SemanticLanguage::Cpp,
            },
            language_fidelity: LanguageFidelity::Explicit,
            provenance: SemanticFactProvenance::Ast,
            fact_fidelity: if contains_error_or_missing(node) {
                SemanticFactFidelity::Incomplete
            } else {
                SemanticFactFidelity::Authoritative
            },
            role: SemanticDeclarationRole::Definition,
        },
        name: name.to_string(),
        qualified_name: name.to_string(),
        declaration_kind: SemanticDeclarationKind::Macro,
        role: SemanticDeclarationRole::Definition,
        path: path_text,
        name_range,
        declaration_range,
        canonical_signature: signature,
        declarator_shape: None,
        has_initializer: None,
        owner: None,
        linkage: crate::call_model::LinkageDomain::External,
        guard: None,
        backing: DeclarationBacking::SourceRange { range: name_range },
    });
}

pub(super) fn collect_object_declarations(
    declaration: tree_sitter::Node<'_>,
    path: &Path,
    source: &str,
    line_starts: &[usize],
    language: SourceLanguage,
    declarations: &mut Vec<DeclarationFact>,
) {
    if node_has_storage_class(declaration, source, "typedef") {
        return;
    }

    let mut cursor = declaration.walk();
    let declarators: Vec<_> = declaration
        .children_by_field_name("declarator", &mut cursor)
        .collect();
    let first_declarator = declarators.first().copied();
    for declarator in declarators {
        let contains_function_declarator =
            declarator_contains_kind(declarator, "function_declarator");
        if contains_function_declarator && !function_declarator_is_pointer_like(declarator) {
            continue;
        }

        let Some((name_node, name)) = declarator_identifier(declarator, source) else {
            continue;
        };
        if crate::language_builtins::is_language_keyword(name) {
            continue;
        }

        let owner = namespace_owner(declaration, source);
        let qualified_name = owner
            .as_ref()
            .map_or_else(|| name.to_string(), |owner| format!("{owner}::{name}"));
        let has_initializer = object_declarator_has_initializer(declarator);
        let is_cpp = language == SourceLanguage::Cpp;
        let role = if has_initializer {
            SemanticDeclarationRole::Definition
        } else if node_has_storage_class(declaration, source, "extern") {
            SemanticDeclarationRole::Declaration
        } else if is_cpp {
            SemanticDeclarationRole::Definition
        } else {
            SemanticDeclarationRole::TentativeDefinition
        };
        let internal = node_has_storage_class(declaration, source, "static")
            || (is_cpp
                && node_has_type_qualifier(declaration, source, "const")
                && !node_has_storage_class(declaration, source, "extern"))
            || owner
                .as_deref()
                .is_some_and(|owner| owner.contains("<anonymous>"));
        let path_text = path.to_string_lossy().replace('\\', "/");
        let linkage = if internal {
            crate::call_model::LinkageDomain::Internal(path_text.clone())
        } else {
            crate::call_model::LinkageDomain::External
        };
        let declaration_range = source_range(declaration, source, line_starts);
        let name_range = source_range(name_node, source, line_starts);
        let signature = canonical_object_signature(
            declaration,
            first_declarator.unwrap_or(declarator),
            declarator,
            source,
        );
        let shape = object_declarator_shape(declarator, source);
        let fact_fidelity = if contains_error_or_missing(declaration) {
            SemanticFactFidelity::Incomplete
        } else {
            SemanticFactFidelity::Authoritative
        };
        let linkage_domain = match &linkage {
            crate::call_model::LinkageDomain::External => "external".to_string(),
            crate::call_model::LinkageDomain::Internal(path) => format!("internal:{path}"),
            crate::call_model::LinkageDomain::Unknown => "unknown".to_string(),
        };
        let logical_key = LogicalEntityKey {
            qualified_name: qualified_name.clone(),
            declaration_kind: SemanticDeclarationKind::Object,
            owner: owner.clone(),
            canonical_signature: Some(signature.clone()),
            linkage_domain,
            guard_fingerprint: None,
        };
        let fingerprint = digest(&format!(
            "object|{}|{}|{}|{}|{}",
            path_text,
            qualified_name,
            declaration.start_byte(),
            name_node.start_byte(),
            signature
        ));
        let locator = DeclarationLocator {
            workspace_id: String::new(),
            path: path_text.clone(),
            range: declaration_range,
            fingerprint,
        };
        declarations.push(DeclarationFact {
            identity: DeclarationIdentity {
                locator,
                logical_key,
                language: if is_cpp {
                    SemanticLanguage::Cpp
                } else {
                    SemanticLanguage::C
                },
                language_fidelity: LanguageFidelity::Explicit,
                provenance: SemanticFactProvenance::Ast,
                fact_fidelity,
                role,
            },
            name: name.to_string(),
            qualified_name,
            declaration_kind: SemanticDeclarationKind::Object,
            role,
            path: path_text,
            name_range,
            declaration_range,
            canonical_signature: Some(signature),
            declarator_shape: Some(shape),
            has_initializer: Some(has_initializer),
            owner,
            linkage,
            guard: None,
            backing: DeclarationBacking::SourceRange { range: name_range },
        });
    }
}

pub(super) fn is_namespace_or_file_scope_declaration(node: tree_sitter::Node<'_>) -> bool {
    let mut parent = node.parent();
    while let Some(ancestor) = parent {
        match ancestor.kind() {
            "translation_unit" | "namespace_definition" => return true,
            "function_definition"
            | "compound_statement"
            | "field_declaration"
            | "parameter_declaration"
            | "for_statement"
            | "while_statement"
            | "if_statement" => return false,
            _ => parent = ancestor.parent(),
        }
    }
    false
}

pub(super) fn object_declarator_has_initializer(declarator: tree_sitter::Node<'_>) -> bool {
    if declarator.kind() == "init_declarator" && declarator.child_by_field_name("value").is_some() {
        return true;
    }
    let mut cursor = declarator.walk();
    let has_initializer = declarator
        .children(&mut cursor)
        .any(object_declarator_has_initializer);
    has_initializer
}

pub(super) fn object_declarator_shape(
    declarator: tree_sitter::Node<'_>,
    source: &str,
) -> DeclaratorShape {
    if function_declarator_is_pointer_like(declarator) {
        return DeclaratorShape::FunctionPointer {
            signature: declarator
                .utf8_text(source.as_bytes())
                .map(compact_whitespace)
                .unwrap_or_default(),
        };
    }
    let unwrapped = unwrap_init_declarator(declarator);
    typedef_declarator_shape(unwrapped, source, &[])
}

pub(super) fn unwrap_init_declarator(node: tree_sitter::Node<'_>) -> tree_sitter::Node<'_> {
    if node.kind() == "init_declarator" {
        node.child_by_field_name("declarator").unwrap_or(node)
    } else {
        node
    }
}

pub(super) fn node_has_storage_class(
    node: tree_sitter::Node<'_>,
    source: &str,
    expected: &str,
) -> bool {
    let mut cursor = node.walk();
    let has_storage_class = node.children(&mut cursor).any(|child| {
        child.kind() == "storage_class_specifier" && node_text(child, source) == Some(expected)
    });
    has_storage_class
}

pub(super) fn node_has_type_qualifier(
    node: tree_sitter::Node<'_>,
    source: &str,
    expected: &str,
) -> bool {
    let mut cursor = node.walk();
    let has_qualifier = node.children(&mut cursor).any(|child| {
        child.kind() == "type_qualifier" && node_text(child, source) == Some(expected)
    });
    has_qualifier
}

pub(super) fn canonical_object_signature(
    declaration: tree_sitter::Node<'_>,
    first_declarator: tree_sitter::Node<'_>,
    declarator: tree_sitter::Node<'_>,
    source: &str,
) -> String {
    let prefix = source
        .get(declaration.start_byte()..first_declarator.start_byte())
        .map(strip_object_storage_specifiers)
        .unwrap_or_default();
    let declarator = unwrap_init_declarator(declarator);
    let declarator_text = declarator.utf8_text(source.as_bytes()).unwrap_or_default();
    compact_whitespace(&format!("{prefix} {declarator_text}"))
}

pub(super) fn strip_object_storage_specifiers(prefix: &str) -> String {
    prefix
        .split_whitespace()
        .filter(|token| {
            !matches!(
                *token,
                "extern" | "static" | "register" | "auto" | "_Thread_local" | "thread_local"
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn namespace_owner(node: tree_sitter::Node<'_>, source: &str) -> Option<String> {
    let mut names = Vec::new();
    let mut parent = node.parent();
    while let Some(ancestor) = parent {
        if ancestor.kind() == "namespace_definition" {
            let name = ancestor
                .child_by_field_name("name")
                .and_then(|name| node_text(name, source))
                .unwrap_or("<anonymous>");
            names.push(name.to_string());
        }
        parent = ancestor.parent();
    }
    if names.is_empty() {
        None
    } else {
        names.reverse();
        Some(names.join("::"))
    }
}
