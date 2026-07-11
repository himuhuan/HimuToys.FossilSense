use std::path::Path;

use crate::call_model::{
    AnchorRole, CallForm, CallSiteFact, CallableAnchor, CallableKind, FactProvenance,
    LinkageDomain, OwnerKindHint, SignatureShape, SourcePosition, SourceRange,
};

pub(super) struct CollectedCallFacts {
    pub(super) anchors: Vec<CallableAnchor>,
    pub(super) call_sites: Vec<CallSiteFact>,
}

enum ScopeFrame {
    Namespace {
        node_id: usize,
        name: String,
    },
    Record {
        node_id: usize,
        name: Option<String>,
    },
    Callable {
        node_id: usize,
        entity_key: Option<String>,
    },
    Lambda {
        node_id: usize,
    },
}

impl ScopeFrame {
    fn node_id(&self) -> usize {
        match self {
            Self::Namespace { node_id, .. }
            | Self::Record { node_id, .. }
            | Self::Callable { node_id, .. }
            | Self::Lambda { node_id } => *node_id,
        }
    }
}

pub(super) struct CallFactCollector<'a> {
    path: String,
    is_cpp: bool,
    source: &'a str,
    line_starts: &'a [usize],
    scopes: Vec<ScopeFrame>,
    error_depth: usize,
    anchors: Vec<CallableAnchor>,
    call_sites: Vec<CallSiteFact>,
    global_entity_key: Option<String>,
}

impl<'a> CallFactCollector<'a> {
    pub(super) fn new(path: &Path, source: &'a str, line_starts: &'a [usize]) -> Self {
        let path_text = path.to_string_lossy().replace('\\', "/");
        let is_cpp = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                matches!(
                    extension.to_ascii_lowercase().as_str(),
                    "cpp" | "hpp" | "cc" | "hh" | "cxx" | "hxx" | "inl"
                )
            });
        Self {
            path: path_text,
            is_cpp,
            source,
            line_starts,
            scopes: Vec::new(),
            error_depth: 0,
            anchors: Vec::new(),
            call_sites: Vec::new(),
            global_entity_key: None,
        }
    }

    pub(super) fn enter(&mut self, node: tree_sitter::Node<'_>) {
        if node.is_error() || node.is_missing() {
            self.error_depth += 1;
        }

        match node.kind() {
            "namespace_definition" => {
                let name = node
                    .child_by_field_name("name")
                    .and_then(|name| text(name, self.source))
                    .unwrap_or("<anonymous>")
                    .to_string();
                self.scopes.push(ScopeFrame::Namespace {
                    node_id: node.id(),
                    name,
                });
            }
            "struct_specifier" | "union_specifier" | "class_specifier"
                if node.child_by_field_name("body").is_some() =>
            {
                let name = node
                    .child_by_field_name("name")
                    .and_then(|name| text(name, self.source))
                    .map(str::to_string);
                self.scopes.push(ScopeFrame::Record {
                    node_id: node.id(),
                    name,
                });
            }
            "lambda_expression" => self.scopes.push(ScopeFrame::Lambda { node_id: node.id() }),
            "function_definition" => {
                let anchor = self.callable_anchor(node, AnchorRole::Definition);
                let entity_key = anchor.as_ref().map(|anchor| anchor.entity_key.clone());
                if let Some(anchor) = anchor {
                    self.anchors.push(anchor);
                }
                self.scopes.push(ScopeFrame::Callable {
                    node_id: node.id(),
                    entity_key,
                });
            }
            "declaration" if self.current_callable().is_none() => {
                if let Some(anchor) = self.callable_anchor(node, AnchorRole::Declaration) {
                    self.anchors.push(anchor);
                }
            }
            "call_expression" => self.collect_call_site(node),
            _ => {}
        }
    }

    pub(super) fn exit(&mut self, node: tree_sitter::Node<'_>) {
        if self
            .scopes
            .last()
            .is_some_and(|scope| scope.node_id() == node.id())
        {
            self.scopes.pop();
        }
        if node.is_error() || node.is_missing() {
            self.error_depth = self.error_depth.saturating_sub(1);
        }
    }

    pub(super) fn finish(self) -> CollectedCallFacts {
        CollectedCallFacts {
            anchors: self.anchors,
            call_sites: self.call_sites,
        }
    }

    fn callable_anchor(
        &self,
        declaration: tree_sitter::Node<'_>,
        role: AnchorRole,
    ) -> Option<CallableAnchor> {
        let function_declarator = find_descendant(declaration, "function_declarator")?;
        if declarator_is_pointer_like(function_declarator) {
            return None;
        }
        let declarator = function_declarator
            .child_by_field_name("declarator")
            .unwrap_or(function_declarator);
        let (name_node, explicit_owner, name) = callable_name(declarator, self.source)?;
        if crate::language_builtins::is_language_keyword(&name) {
            return None;
        }

        let namespaces = self.namespace_names();
        let record_owner = self.record_owner();
        let (owner, owner_kind) = if let Some(record) = record_owner {
            (record, Some(OwnerKindHint::Record))
        } else if let Some(owner) = explicit_owner {
            let namespace = namespaces.join("::");
            let kind = if !namespace.is_empty() && owner == namespace {
                OwnerKindHint::Namespace
            } else {
                OwnerKindHint::Unknown
            };
            (Some(owner), Some(kind))
        } else if namespaces.is_empty() {
            (None, None)
        } else {
            (Some(namespaces.join("::")), Some(OwnerKindHint::Namespace))
        };
        let qualified_name = owner
            .as_ref()
            .map_or_else(|| name.clone(), |owner| format!("{owner}::{name}"));
        let signature = signature_shape(function_declarator, self.source, self.is_cpp);
        let declaration_text = text(declaration, self.source).unwrap_or(&name);
        let normalized_declaration = compact_whitespace(declaration_text);
        let internal = has_storage_class(declaration, self.source, "static")
            || namespaces.iter().any(|name| name == "<anonymous>");
        let linkage = if internal {
            LinkageDomain::Internal(self.path.clone())
        } else {
            LinkageDomain::External
        };
        let family_input = format!(
            "{}|{}|{}|{:?}",
            qualified_name,
            signature.normalized,
            self.path_if_internal(internal),
            owner_kind
        );
        let entity_key = digest(&family_input);
        let anchor_fingerprint = digest(&format!(
            "{}|{:?}|{}",
            entity_key, role, normalized_declaration
        ));
        let body_range = declaration
            .child_by_field_name("body")
            .map(|body| self.source_range(body));

        Some(CallableAnchor {
            path: self.path.clone(),
            name,
            qualified_name,
            owner,
            owner_kind,
            kind: CallableKind::Function,
            role,
            linkage,
            signature,
            name_range: self.source_range(name_node),
            declaration_range: self.source_range(declaration),
            body_range,
            guard: None,
            provenance: FactProvenance::Ast,
            syntax_error_overlap: self.error_depth > 0,
            entity_key,
            anchor_fingerprint,
        })
    }

    fn collect_call_site(&mut self, call: tree_sitter::Node<'_>) {
        let caller_entity_key = match self.current_callable() {
            Some(Some(entity_key)) => entity_key,
            Some(None) => return,
            None => self.global_initializer_key(call),
        };
        let Some(function) = call.child_by_field_name("function") else {
            return;
        };
        let normalized = normalize_call_target(function, self.source);
        let callee_range = normalized
            .name_node
            .map(|node| self.source_range(node))
            .unwrap_or_else(|| self.source_range(function));
        let expression_range = self.source_range(call);
        let argument_count = call
            .child_by_field_name("arguments")
            .map(named_argument_count);
        let site_fingerprint = digest(&format!(
            "{}|{}|{}|{:?}|{:?}",
            self.path,
            caller_entity_key,
            expression_range.start_byte,
            normalized.form,
            normalized.qualified_name
        ));
        self.call_sites.push(CallSiteFact {
            path: self.path.clone(),
            caller_entity_key,
            expression_range,
            callee_range,
            callee_name: normalized.name,
            qualified_name: normalized.qualified_name,
            form: normalized.form,
            argument_count,
            guard: None,
            provenance: FactProvenance::Ast,
            syntax_error_overlap: self.error_depth > 0,
            site_fingerprint,
        });
    }

    fn current_callable(&self) -> Option<Option<String>> {
        for scope in self.scopes.iter().rev() {
            match scope {
                ScopeFrame::Callable { entity_key, .. } => return Some(entity_key.clone()),
                ScopeFrame::Lambda { .. } => return Some(None),
                _ => {}
            }
        }
        None
    }

    fn global_initializer_key(&mut self, call: tree_sitter::Node<'_>) -> String {
        if let Some(key) = &self.global_entity_key {
            return key.clone();
        }
        let qualified_name = "file::<global initialization>".to_string();
        let entity_key = digest(&format!("{}|{qualified_name}", self.path));
        let range = self.source_range(call);
        self.anchors.push(CallableAnchor {
            path: self.path.clone(),
            name: "<global initialization>".to_string(),
            qualified_name,
            owner: None,
            owner_kind: None,
            kind: CallableKind::SyntheticGlobalInitializer,
            role: AnchorRole::Synthetic,
            linkage: LinkageDomain::Internal(self.path.clone()),
            signature: SignatureShape {
                normalized: String::new(),
                min_arity: Some(0),
                max_arity: Some(0),
                variadic: false,
            },
            name_range: range,
            declaration_range: range,
            body_range: None,
            guard: None,
            provenance: FactProvenance::Synthetic,
            syntax_error_overlap: self.error_depth > 0,
            entity_key: entity_key.clone(),
            anchor_fingerprint: digest(&format!("{}|global", entity_key)),
        });
        self.global_entity_key = Some(entity_key.clone());
        entity_key
    }

    fn namespace_names(&self) -> Vec<String> {
        self.scopes
            .iter()
            .filter_map(|scope| match scope {
                ScopeFrame::Namespace { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect()
    }

    fn record_owner(&self) -> Option<Option<String>> {
        self.scopes.iter().rev().find_map(|scope| match scope {
            ScopeFrame::Record { name, .. } => Some(name.clone()),
            _ => None,
        })
    }

    fn path_if_internal(&self, internal: bool) -> &str {
        if internal {
            &self.path
        } else {
            ""
        }
    }

    fn source_range(&self, node: tree_sitter::Node<'_>) -> SourceRange {
        let start = node.start_position();
        let end = node.end_position();
        SourceRange {
            start: SourcePosition {
                line: start.row as u32,
                character: utf16_col(self.source, self.line_starts, start.row, node.start_byte()),
            },
            end: SourcePosition {
                line: end.row as u32,
                character: utf16_col(self.source, self.line_starts, end.row, node.end_byte()),
            },
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
        }
    }
}

struct NormalizedCallTarget<'tree> {
    name_node: Option<tree_sitter::Node<'tree>>,
    name: Option<String>,
    qualified_name: Option<String>,
    form: CallForm,
}

fn normalize_call_target<'tree>(
    node: tree_sitter::Node<'tree>,
    source: &str,
) -> NormalizedCallTarget<'tree> {
    match node.kind() {
        "identifier" => NormalizedCallTarget {
            name_node: Some(node),
            name: text(node, source).map(str::to_string),
            qualified_name: None,
            form: CallForm::DirectName,
        },
        "qualified_identifier" => {
            let qualified = text(node, source).map(str::to_string);
            let name_node = node
                .child_by_field_name("name")
                .or_else(|| last_identifier(node));
            NormalizedCallTarget {
                name: name_node
                    .and_then(|name| text(name, source))
                    .map(str::to_string),
                name_node,
                qualified_name: qualified,
                form: CallForm::QualifiedName,
            }
        }
        "parenthesized_expression" => {
            let inner = named_children(node).into_iter().next();
            let Some(inner) = inner else {
                return unsupported_target(CallForm::Unsupported);
            };
            let mut target = normalize_call_target(inner, source);
            if matches!(target.form, CallForm::DirectName | CallForm::QualifiedName) {
                target.form = CallForm::ParenthesizedName;
            }
            target
        }
        "field_expression" => {
            let name_node = node
                .child_by_field_name("field")
                .or_else(|| last_identifier(node));
            let raw = text(node, source).unwrap_or_default();
            NormalizedCallTarget {
                name: name_node
                    .and_then(|name| text(name, source))
                    .map(str::to_string),
                name_node,
                qualified_name: None,
                form: if raw.contains("->") {
                    CallForm::MemberArrow
                } else {
                    CallForm::MemberDot
                },
            }
        }
        "pointer_expression" => unsupported_target(CallForm::FunctionPointer),
        _ => unsupported_target(CallForm::Unsupported),
    }
}

fn unsupported_target(form: CallForm) -> NormalizedCallTarget<'static> {
    NormalizedCallTarget {
        name_node: None,
        name: None,
        qualified_name: None,
        form,
    }
}

fn callable_name<'tree>(
    declarator: tree_sitter::Node<'tree>,
    source: &str,
) -> Option<(tree_sitter::Node<'tree>, Option<String>, String)> {
    if matches!(declarator.kind(), "identifier" | "field_identifier") {
        return Some((declarator, None, text(declarator, source)?.to_string()));
    }
    if declarator.kind() == "qualified_identifier" {
        let full = text(declarator, source)?;
        let name_node = declarator
            .child_by_field_name("name")
            .or_else(|| last_identifier(declarator))?;
        let name = text(name_node, source)?.to_string();
        let owner = full.rsplit_once("::").map(|(owner, _)| owner.to_string());
        return Some((name_node, owner, name));
    }
    if let Some(child) = declarator.child_by_field_name("declarator") {
        return callable_name(child, source);
    }
    let identifier = last_identifier(declarator)?;
    Some((identifier, None, text(identifier, source)?.to_string()))
}

fn signature_shape(
    declarator: tree_sitter::Node<'_>,
    source: &str,
    is_cpp: bool,
) -> SignatureShape {
    let parameters = declarator
        .child_by_field_name("parameters")
        .or_else(|| find_descendant(declarator, "parameter_list"));
    let Some(parameters) = parameters else {
        return SignatureShape {
            normalized: String::new(),
            min_arity: None,
            max_arity: None,
            variadic: false,
        };
    };
    let normalized = compact_whitespace(text(parameters, source).unwrap_or_default());
    let children = named_children(parameters);
    if children.len() == 1 && text(children[0], source).is_some_and(|value| value.trim() == "void")
    {
        return SignatureShape {
            normalized,
            min_arity: Some(0),
            max_arity: Some(0),
            variadic: false,
        };
    }
    let mut min = 0u32;
    let mut max = 0u32;
    let mut variadic = false;
    for child in children {
        if child.kind().contains("variadic") {
            variadic = true;
            continue;
        }
        if child.kind().contains("parameter") {
            max += 1;
            let raw = text(child, source).unwrap_or_default();
            if !raw.contains('=') {
                min += 1;
            }
        }
    }
    let empty_c_parameters = !is_cpp && min == 0 && max == 0 && !variadic;
    SignatureShape {
        normalized,
        min_arity: (!empty_c_parameters).then_some(min),
        max_arity: if variadic || empty_c_parameters {
            None
        } else {
            Some(max)
        },
        variadic,
    }
}

fn has_storage_class(node: tree_sitter::Node<'_>, source: &str, expected: &str) -> bool {
    named_children(node).into_iter().any(|child| {
        child.kind() == "storage_class_specifier" && text(child, source) == Some(expected)
    })
}

fn declarator_is_pointer_like(node: tree_sitter::Node<'_>) -> bool {
    let declarator = node.child_by_field_name("declarator");
    declarator.is_some_and(|declarator| {
        matches!(
            declarator.kind(),
            "pointer_declarator" | "parenthesized_declarator"
        ) && find_descendant(declarator, "pointer_declarator").is_some()
    })
}

fn find_descendant<'tree>(
    root: tree_sitter::Node<'tree>,
    kind: &str,
) -> Option<tree_sitter::Node<'tree>> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == kind {
            return Some(node);
        }
        stack.extend(named_children(node).into_iter().rev());
    }
    None
}

fn last_identifier(root: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    let mut found = None;
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "identifier" | "field_identifier") {
            found = Some(node);
        }
        stack.extend(named_children(node));
    }
    found
}

fn named_argument_count(arguments: tree_sitter::Node<'_>) -> u32 {
    named_children(arguments).len() as u32
}

fn named_children(node: tree_sitter::Node<'_>) -> Vec<tree_sitter::Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn text<'a>(node: tree_sitter::Node<'_>, source: &'a str) -> Option<&'a str> {
    source.get(node.start_byte()..node.end_byte())
}

fn compact_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn digest(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..24].to_string()
}

fn utf16_col(source: &str, line_starts: &[usize], row: usize, byte: usize) -> u32 {
    let line_start = line_starts.get(row).copied().unwrap_or(0).min(byte);
    source
        .get(line_start..byte)
        .unwrap_or_default()
        .encode_utf16()
        .count() as u32
}
