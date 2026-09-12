use super::*;
use crate::semantic_model::relations::*;
use crate::semantic_model::EntityDomain;
use std::collections::{HashMap, HashSet};

pub(super) struct RelationVisit<'tree, 'a> {
    pub node: tree_sitter::Node<'tree>,
    pub source: &'a str,
    pub lines: &'a [usize],
    pub language: SourceLanguage,
    pub context: &'a DeclarationContext,
    pub caller: Option<String>,
    pub facts: ParseFacts,
}

pub(super) struct RelationCollector {
    locals: Vec<LocalBinding>,
    globals: HashMap<(Option<String>, String), ReceiverFact>,
    current_owner: Option<String>,
    function: Option<usize>,
    function_range: Option<(usize, usize)>,
    local_bytes: super::super::retained::AppendOnlyVecBytes,
    global_bytes: usize,
}

impl RelationCollector {
    pub(super) fn new() -> Self {
        Self {
            locals: Vec::new(),
            globals: HashMap::new(),
            current_owner: None,
            function: None,
            function_range: None,
            local_bytes: Default::default(),
            global_bytes: 0,
        }
    }

    pub(super) fn enter(&mut self, visit: RelationVisit<'_, '_>, out: &mut RelationFacts) {
        let RelationVisit {
            node,
            source,
            lines,
            language,
            context,
            caller,
            facts,
        } = visit;
        self.current_owner = context.owner();
        if node.kind() == "function_definition" {
            self.locals.clear();
            self.function_range = node
                .child_by_field_name("body")
                .map(|n| (n.start_byte(), n.end_byte()));
            self.function = Some(node.id());
        }
        if let Some((start, end)) = self.function_range {
            if let Some((name, ty)) = function_pointer_initialization(node, source) {
                let (scope_start_byte, scope_end_byte) =
                    bindings::nearest_compound_scope(node, start, end);
                self.locals.push(LocalBinding {
                    name: node_text(name, source).unwrap_or_default().to_owned(),
                    kind: LocalBindingKind::LocalVariable,
                    namespace: super::super::LocalBindingNamespace::Ordinary,
                    type_text: Some(ty.to_owned()),
                    decl_start_byte: name.start_byte(),
                    function_start_byte: start,
                    function_end_byte: end,
                    scope_start_byte,
                    scope_end_byte,
                });
            }
            if matches!(node.kind(), "declaration" | "parameter_declaration") {
                let (scope_start, scope_end) = bindings::nearest_compound_scope(node, start, end);
                let kind = if node.kind() == "parameter_declaration" {
                    LocalBindingKind::Parameter
                } else {
                    LocalBindingKind::LocalVariable
                };
                bindings::push_binding_declarators(
                    node,
                    source,
                    kind,
                    start,
                    end,
                    scope_start,
                    scope_end,
                    &mut self.locals,
                );
            }
        }
        let evidence = || RelationSource {
            range: source_range(node, source, lines),
            fingerprint: blake3::hash(&source.as_bytes()[node.byte_range()]).to_hex()[..24]
                .to_owned(),
            enclosing_callable: caller.clone(),
            owner: context.owner(),
            guard: context.guard(),
            provenance: crate::semantic_model::SemanticFactProvenance::Ast,
            fidelity: if node.has_error() || bindings::error_or_missing_ancestor(node) {
                SemanticFactFidelity::Incomplete
            } else {
                SemanticFactFidelity::Authoritative
            },
        };
        if node.kind() == "declaration" && caller.is_none() {
            let decoded = decode_direct_declarators(node, source, false);
            for entity in decoded
                .entities
                .iter()
                .filter(|e| e.kind == DeclaredEntityKind::Object)
            {
                let ty = node
                    .child_by_field_name("type")
                    .and_then(|n| node_text(n, source));
                self.global_bytes = self
                    .global_bytes
                    .saturating_add(256 + entity.name.len() * 2 + ty.map_or(0, str::len));
                self.globals.insert(
                    (self.current_owner.clone(), entity.name.clone()),
                    ReceiverFact {
                        spelling: entity.name.clone(),
                        type_name: ty.map(str::to_owned),
                        tag_domain: ty
                            .is_some_and(|s| s.starts_with("struct ") || s.starts_with("union ")),
                        chain: Vec::new(),
                        object_anchor: Some(entity.name_node.start_byte()),
                        object_scope: None,
                    },
                );
            }
        }
        if facts.contains(ParseFacts::BINDING_SITES)
            && matches!(
                node.kind(),
                "identifier" | "field_identifier" | "type_identifier"
            )
        {
            if let Some(role) = reference_role(node, source) {
                let spelling = node_text(node, source).unwrap_or_default().to_owned();
                let receiver = node
                    .parent()
                    .filter(|p| p.kind() == "field_expression")
                    .filter(|p| p.child_by_field_name("field") == Some(node))
                    .and_then(|p| p.child_by_field_name("argument"))
                    .and_then(|n| self.receiver(n, source, lines));
                let local_anchor = self
                    .local(&spelling, node.start_byte())
                    .map(|b| b.decl_start_byte);
                let domain = if role == ReferenceRole::TypeUse {
                    if node.parent().is_some_and(|p| {
                        matches!(
                            p.kind(),
                            "struct_specifier"
                                | "class_specifier"
                                | "union_specifier"
                                | "enum_specifier"
                        )
                    }) {
                        EntityDomain::Tag
                    } else {
                        EntityDomain::Alias
                    }
                } else if role == ReferenceRole::Call {
                    EntityDomain::Callable
                } else {
                    EntityDomain::Value
                };
                out.binding_sites.push(BindingSiteFact {
                    source: evidence(),
                    spelling,
                    domain,
                    role,
                    member_access: node.parent().is_some_and(|p| {
                        p.kind() == "field_expression"
                            && p.child_by_field_name("field") == Some(node)
                    }),
                    qualifier: node
                        .parent()
                        .filter(|p| p.kind() == "qualified_identifier")
                        .and_then(|p| p.child_by_field_name("scope"))
                        .and_then(|n| node_text(n, source))
                        .map(str::to_owned),
                    receiver,
                    local_anchor,
                });
            }
        }
        if facts.contains(ParseFacts::EXPLICIT_BASES)
            && language == SourceLanguage::Cpp
            && node.kind() == "base_class_clause"
        {
            if let Some(record) = node.parent() {
                if let Some(name) = record
                    .child_by_field_name("name")
                    .and_then(|n| node_text(n, source))
                {
                    let mut access = if record.kind() == "class_specifier" {
                        BaseAccess::Private
                    } else {
                        BaseAccess::Public
                    };
                    let default_access = access;
                    let mut virtual_base = false;
                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        let text = node_text(child, source).unwrap_or_default();
                        match child.kind() {
                            "," => {
                                access = default_access;
                                virtual_base = false;
                            }
                            "access_specifier" => {
                                access = match text {
                                    "private" => BaseAccess::Private,
                                    "protected" => BaseAccess::Protected,
                                    _ => BaseAccess::Public,
                                }
                            }
                            "virtual" => virtual_base = true,
                            "type_identifier"
                            | "qualified_identifier"
                            | "template_type"
                            | "dependent_type" => {
                                let mut source_fact = evidence();
                                source_fact.range = source_range(child, source, lines);
                                source_fact.owner = context.owner();
                                out.explicit_bases.push(ExplicitBaseFact {
                                    source: source_fact,
                                    derived_record_key: format!("rec_{}", record.start_byte()),
                                    derived_name: name.to_owned(),
                                    base_name: text.to_owned(),
                                    access,
                                    is_virtual: virtual_base,
                                    dependent: text.contains('<')
                                        || child.kind() == "dependent_type",
                                });
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        if facts.contains(ParseFacts::INDIRECT_ASSIGNMENTS) {
            let pair = if node.kind() == "init_declarator" {
                node.child_by_field_name("declarator")
                    .zip(node.child_by_field_name("value"))
            } else if node.kind() == "assignment_expression" {
                node.child_by_field_name("left")
                    .zip(node.child_by_field_name("right"))
            } else {
                None
            };
            if let Some((left, right)) = pair {
                let id = declarator_identifier(left, source)
                    .map(|(n, _)| n)
                    .unwrap_or(left);
                let (slot_node, member) = if left.kind() == "field_expression" {
                    (
                        left.child_by_field_name("argument").unwrap_or(left),
                        left.child_by_field_name("field")
                            .and_then(|n| node_text(n, source))
                            .map(str::to_owned),
                    )
                } else {
                    (id, None)
                };
                if let Some(slot) = self.receiver(slot_node, source, lines) {
                    if right.kind() != "initializer_list" || member.is_some() {
                        let target = direct_target(right, source)
                            .filter(|name| self.local(name, right.start_byte()).is_none());
                        out.indirect_assignments.push(IndirectAssignmentFact {
                            source: evidence(),
                            slot,
                            member,
                            unknown_write: target.is_none(),
                            target_name: target,
                            branch: branch_range(node, source, lines),
                        });
                    }
                }
            }
            // Each designated entry is collected when the shared DFS visits
            // it, so large lists cannot bypass the fact-byte checkpoints.
            if node.kind() == "initializer_pair" {
                let initializer = node.parent().filter(|n| n.kind() == "initializer_list");
                let declaration = initializer
                    .and_then(|n| n.parent())
                    .filter(|n| n.kind() == "init_declarator");
                let slot_node = declaration
                    .and_then(|n| n.child_by_field_name("declarator"))
                    .and_then(|n| declarator_identifier(n, source).map(|(id, _)| id));
                let designator = node
                    .child_by_field_name("designator")
                    .or_else(|| node.named_child(0));
                let value = node.child_by_field_name("value");
                if let (Some(slot), Some(d), Some(v)) = (
                    slot_node.and_then(|n| self.receiver(n, source, lines)),
                    designator,
                    value,
                ) {
                    let raw = node_text(d, source).unwrap_or_default().trim();
                    if let Some(field) = raw.strip_prefix('.').filter(|s| identifier(s)) {
                        let target = direct_target(v, source)
                            .filter(|name| self.local(name, v.start_byte()).is_none());
                        out.indirect_assignments.push(IndirectAssignmentFact {
                            source: evidence(),
                            slot,
                            member: Some(field.to_owned()),
                            unknown_write: target.is_none(),
                            target_name: target,
                            branch: branch_range(node, source, lines),
                        });
                    }
                }
            }
            // Taking a slot's address escapes it. Keep the known assignments,
            // but do not claim that the candidate set describes all writes.
            if node.kind() == "pointer_expression"
                && node_text(node, source).is_some_and(|s| s.trim_start().starts_with('&'))
            {
                if let Some(slot) = node
                    .child_by_field_name("argument")
                    .and_then(|n| self.receiver(n, source, lines))
                {
                    if slot.object_anchor.is_some() {
                        out.indirect_assignments.push(IndirectAssignmentFact {
                            source: evidence(),
                            slot,
                            member: None,
                            target_name: None,
                            branch: branch_range(node, source, lines),
                            unknown_write: true,
                        });
                    }
                }
            }
        }
        if facts.contains(ParseFacts::MACRO_FACTS)
            && matches!(
                node.kind(),
                "preproc_function_def" | "preproc_def" | "preproc_call"
            )
        {
            let undef = node.kind() == "preproc_call"
                && node
                    .child_by_field_name("directive")
                    .and_then(|n| node_text(n, source))
                    == Some("#undef");
            let name_node = if undef {
                node.child_by_field_name("argument")
            } else {
                node.child_by_field_name("name")
            };
            if let Some(name) = name_node
                .and_then(|n| node_text(n, source))
                .map(str::trim)
                .filter(|n| identifier(n))
            {
                let function_like = node.kind() == "preproc_function_def";
                let parameters = node
                    .child_by_field_name("parameters")
                    .and_then(|n| node_text(n, source))
                    .unwrap_or_default();
                let body = node
                    .child_by_field_name("value")
                    .and_then(|n| node_text(n, source))
                    .unwrap_or_default();
                let mut src = evidence();
                if let Some(n) = name_node {
                    src.range = source_range(n, source, lines);
                }
                out.macros.push(MacroFact {
                    source: src,
                    name: name.to_owned(),
                    function_like,
                    undef,
                    direct_calls: if function_like {
                        macro_calls(body, parameters)
                    } else {
                        Vec::new()
                    },
                    replacement_range: node
                        .child_by_field_name("value")
                        .map(|n| source_range(n, source, lines))
                        .unwrap_or_else(|| source_range(node, source, lines)),
                    expansion_not_evaluated: true,
                });
            }
        }
    }

    pub(super) fn retained_bytes(&mut self) -> usize {
        self.local_bytes
            .observe(&self.locals)
            .saturating_add(self.global_bytes)
    }
    pub(super) fn exit(&mut self, node: tree_sitter::Node<'_>) {
        if self.function == Some(node.id()) {
            self.locals.clear();
            self.function = None;
            self.function_range = None;
        }
    }

    fn local(&self, name: &str, byte: usize) -> Option<&LocalBinding> {
        self.locals
            .iter()
            .filter(|b| {
                b.name == name
                    && b.decl_start_byte <= byte
                    && (b.scope_start_byte <= byte && byte < b.scope_end_byte
                        || b.decl_start_byte == byte)
            })
            .max_by_key(|b| b.decl_start_byte)
    }

    fn receiver(
        &self,
        node: tree_sitter::Node<'_>,
        source: &str,
        lines: &[usize],
    ) -> Option<ReceiverFact> {
        if node.kind() == "field_expression" {
            let mut receiver =
                self.receiver(node.child_by_field_name("argument")?, source, lines)?;
            receiver
                .chain
                .push(node_text(node.child_by_field_name("field")?, source)?.to_owned());
            if node
                .child_by_field_name("operator")
                .and_then(|n| node_text(n, source))
                == Some("->")
            {
                receiver.object_anchor = None;
            }
            return (receiver.chain.len() <= 8).then_some(receiver);
        }
        if !matches!(node.kind(), "identifier" | "this") {
            return None;
        }
        let name = node_text(node, source)?;
        if let Some(binding) = self.local(name, node.start_byte()) {
            let ty = binding.type_text.clone();
            let scope = super::super::coverage::source_range(
                source,
                lines,
                binding.scope_start_byte..binding.scope_end_byte,
            );
            return Some(ReceiverFact {
                spelling: name.to_owned(),
                tag_domain: ty
                    .as_ref()
                    .is_some_and(|s| s.starts_with("struct ") || s.starts_with("union ")),
                type_name: ty,
                chain: Vec::new(),
                object_anchor: Some(binding.decl_start_byte),
                object_scope: Some(scope),
            });
        }
        let mut owner = self.current_owner.as_deref();
        for _ in 0..64 {
            if let Some(found) = self
                .globals
                .get(&(owner.map(str::to_owned), name.to_owned()))
            {
                return Some(found.clone());
            }
            let Some(current) = owner else {
                break;
            };
            owner = current.rsplit_once("::").map(|(prefix, _)| prefix);
        }
        Some(ReceiverFact {
            spelling: name.to_owned(),
            type_name: None,
            tag_domain: false,
            chain: Vec::new(),
            object_anchor: None,
            object_scope: None,
        })
    }
}

/// C++'s grammar can parse `int (*fp)() = target` as nested cast/call
/// expressions. Recognize only this complete, literal pointer-declarator
/// shape; it supplies a candidate slot, never a runtime points-to proof.
pub(in crate::parser) fn function_pointer_initialization<'a>(
    node: tree_sitter::Node<'a>,
    source: &'a str,
) -> Option<(tree_sitter::Node<'a>, &'a str)> {
    if node.kind() != "assignment_expression"
        || node
            .child_by_field_name("operator")
            .and_then(|n| node_text(n, source))
            != Some("=")
    {
        return None;
    }
    let outer = node.child_by_field_name("left")?;
    if outer.kind() != "call_expression"
        || outer.child_by_field_name("arguments")?.named_child_count() != 0
    {
        return None;
    }
    let inner = outer.child_by_field_name("function")?;
    if inner.kind() != "call_expression" {
        return None;
    }
    let ty = inner.child_by_field_name("function")?;
    if ty.kind() != "primitive_type" {
        return None;
    }
    let args = inner.child_by_field_name("arguments")?;
    if args.named_child_count() != 1 {
        return None;
    }
    let pointer = args.named_child(0)?;
    if pointer.kind() != "pointer_expression"
        || !node_text(pointer, source)?.trim_start().starts_with('*')
    {
        return None;
    }
    let name = pointer.child_by_field_name("argument")?;
    if name.kind() != "identifier" {
        return None;
    }
    Some((name, node_text(ty, source)?))
}

pub(in crate::parser) fn pointer_declarator_pseudo_call(
    mut call: tree_sitter::Node<'_>,
    source: &str,
) -> bool {
    for _ in 0..3 {
        let Some(parent) = call.parent() else {
            return false;
        };
        if parent.kind() == "assignment_expression"
            && parent.child_by_field_name("left") == Some(call)
        {
            return function_pointer_initialization(parent, source).is_some();
        }
        if parent.kind() != "call_expression" {
            return false;
        }
        call = parent;
    }
    false
}

fn branch_range(
    mut node: tree_sitter::Node<'_>,
    source: &str,
    lines: &[usize],
) -> Option<SourceRange> {
    while let Some(parent) = node.parent() {
        if matches!(
            parent.kind(),
            "if_statement"
                | "conditional_expression"
                | "switch_statement"
                | "for_statement"
                | "while_statement"
        ) {
            return Some(source_range(node, source, lines));
        }
        if parent.kind() == "function_definition" {
            break;
        }
        node = parent;
    }
    None
}

fn identifier(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .enumerate()
            .all(|(i, b)| b == b'_' || b.is_ascii_alphabetic() || i > 0 && b.is_ascii_digit())
}

fn direct_target(node: tree_sitter::Node<'_>, source: &str) -> Option<String> {
    match node.kind() {
        "identifier" | "qualified_identifier" => node_text(node, source).map(str::to_owned),
        "pointer_expression" if node_text(node, source)?.trim_start().starts_with('&') => {
            direct_target(node.child_by_field_name("argument")?, source)
        }
        "parenthesized_expression" => direct_target(node.named_child(0)?, source),
        _ => None,
    }
}

fn macro_calls(body: &str, parameters: &str) -> Vec<String> {
    // Never guess a target assembled by preprocessing or parameter substitution.
    if body.len() > 64 * 1024 || body.contains('#') {
        return Vec::new();
    }
    let params: HashSet<_> = parameters
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .filter(|s| !s.is_empty())
        .collect();
    let map = crate::c_lexical::LexicalMap::new(body, true);
    let bytes = body.as_bytes();
    let mut i = 0;
    let mut result = Vec::new();
    while i < bytes.len() && result.len() < 64 {
        if map.code[i] && (bytes[i].is_ascii_alphabetic() || bytes[i] == b'_') {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let name = &body[start..i];
            let next = map.next_code_token(bytes, i);
            if next.is_some_and(|n| bytes[n] == b'(')
                && !params.contains(name)
                && !crate::language_builtins::is_language_keyword(name)
                && !(0..start)
                    .rev()
                    .find(|i| map.code[*i] && !bytes[*i].is_ascii_whitespace())
                    .is_some_and(|i| matches!(bytes[i], b'.' | b'>' | b':'))
            {
                result.push(name.to_owned());
            }
        } else {
            i += 1;
        }
    }
    result.sort();
    result.dedup();
    result
}

fn reference_role(node: tree_sitter::Node<'_>, source: &str) -> Option<ReferenceRole> {
    if is_declaration_name(node) {
        return None;
    }
    let parent = node.parent()?;
    if parent.kind() == "qualified_identifier" && parent.child_by_field_name("scope") == Some(node)
    {
        return None;
    }
    if matches!(
        bindings::classify_occurrence_role(node),
        SyntacticRole::Definition | SyntacticRole::Declaration
    ) || matches!(
        parent.kind(),
        "field_designator" | "namespace_definition" | "preproc_params" | "preproc_call"
    ) || matches!(
        parent.kind(),
        "struct_specifier"
            | "class_specifier"
            | "union_specifier"
            | "enum_specifier"
            | "type_definition"
            | "alias_declaration"
    ) && parent.child_by_field_name("name") == Some(node)
    {
        return None;
    }
    if node.has_error() || bindings::error_or_missing_ancestor(node) {
        return Some(ReferenceRole::Unknown);
    }
    if node.kind() == "type_identifier" {
        return Some(ReferenceRole::TypeUse);
    }
    let mut expression = node;
    for _ in 0..16 {
        let Some(p) = expression.parent() else {
            break;
        };
        if (p.kind() == "field_expression" && p.child_by_field_name("field") == Some(expression))
            || matches!(
                p.kind(),
                "parenthesized_expression" | "qualified_identifier"
            )
        {
            expression = p;
            continue;
        }
        if p.kind() == "call_expression" && p.child_by_field_name("function") == Some(expression) {
            return Some(ReferenceRole::Call);
        }
        if p.kind() == "assignment_expression" && p.child_by_field_name("left") == Some(expression)
        {
            let op = p
                .child_by_field_name("operator")
                .and_then(|n| node_text(n, source))
                .unwrap_or("=");
            return Some(if op == "=" {
                ReferenceRole::Write
            } else {
                ReferenceRole::ReadWrite
            });
        }
        if p.kind() == "update_expression" {
            return Some(ReferenceRole::ReadWrite);
        }
        if p.kind() == "pointer_expression"
            && node_text(p, source).is_some_and(|s| s.trim_start().starts_with('&'))
        {
            return Some(ReferenceRole::Address);
        }
        return Some(match p.kind() {
            "return_statement"
            | "argument_list"
            | "binary_expression"
            | "unary_expression"
            | "conditional_expression"
            | "init_declarator"
            | "initializer_pair"
            | "field_expression"
            | "expression_statement"
            | "assignment_expression"
            | "pointer_expression"
            | "subscript_expression"
            | "condition_clause" => ReferenceRole::Read,
            _ => ReferenceRole::Unknown,
        });
    }
    Some(ReferenceRole::Unknown)
}

fn is_declaration_name(mut node: tree_sitter::Node<'_>) -> bool {
    for _ in 0..128 {
        let Some(parent) = node.parent() else {
            return false;
        };
        if matches!(
            parent.kind(),
            "declaration"
                | "field_declaration"
                | "parameter_declaration"
                | "type_definition"
                | "function_definition"
        ) {
            let mut cursor = parent.walk();
            return parent
                .children_by_field_name("declarator", &mut cursor)
                .any(|child| child == node);
        }
        if matches!(
            parent.kind(),
            "pointer_declarator"
                | "array_declarator"
                | "init_declarator"
                | "function_declarator"
                | "parenthesized_declarator"
                | "reference_declarator"
        ) && (parent.child_by_field_name("declarator") == Some(node)
            || parent.kind() == "parenthesized_declarator" && parent.named_child(0) == Some(node))
        {
            node = parent;
        } else {
            return false;
        }
    }
    false
}
