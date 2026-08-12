use std::collections::HashSet;
use std::path::Path;

use crate::call_model::{
    AnchorRole, CallForm, CallSiteFact, CallableAnchor, CallableKind, FactProvenance,
    LinkageDomain, OwnerKindHint, SignatureFidelity, SignatureShape, SourcePosition, SourceRange,
};
use crate::config::SourceLanguage;

mod signature;

use signature::*;

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
    record_names: HashSet<String>,
    collect_call_sites: bool,
}

impl<'a> CallFactCollector<'a> {
    pub(super) fn new(
        path: &Path,
        source: &'a str,
        line_starts: &'a [usize],
        language: SourceLanguage,
        collect_call_sites: bool,
    ) -> Self {
        let path_text = path.to_string_lossy().replace('\\', "/");
        let is_cpp = language == SourceLanguage::Cpp;
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
            record_names: HashSet::new(),
            collect_call_sites,
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
            "struct_specifier" | "union_specifier" | "class_specifier" => {
                let name = node
                    .child_by_field_name("name")
                    .and_then(|name| text(name, self.source))
                    .map(str::to_string);
                if let Some(record_name) = name.as_deref() {
                    self.record_names
                        .insert(self.qualify_record_name(record_name));
                }
                if node.child_by_field_name("body").is_some() {
                    self.scopes.push(ScopeFrame::Record {
                        node_id: node.id(),
                        name,
                    });
                }
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
            "field_declaration" if self.current_callable().is_none() => {
                if let Some(anchor) = self.callable_anchor(node, AnchorRole::Declaration) {
                    self.anchors.push(anchor);
                }
            }
            "call_expression" if self.collect_call_sites => self.collect_call_site(node),
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
        if role == AnchorRole::Declaration
            && declaration.child_by_field_name("type").is_none()
            && (self.error_depth > 0 || contains_error_or_missing(declaration))
        {
            return None;
        }
        let declarator = function_declarator
            .child_by_field_name("declarator")
            .unwrap_or(function_declarator);
        let (name_node, explicit_owner, name) = callable_name(declarator, self.source)?;
        if crate::language_builtins::is_language_keyword(&name) {
            return None;
        }
        if role == AnchorRole::Declaration
            && macro_like_identifier(&name)
            && declaration.child_by_field_name("type").is_some_and(|kind| {
                kind.kind() == "macro_type_specifier"
                    && kind.end_position().row < name_node.start_position().row
            })
        {
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
            } else if self.owner_matches_known_record(&owner, &namespaces) {
                OwnerKindHint::Record
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
        let body = declaration.child_by_field_name("body");
        let declaration_end = body
            .map(|body| {
                trim_ascii_whitespace_end(self.source, declaration.start_byte(), body.start_byte())
            })
            .unwrap_or_else(|| declaration.end_byte());
        let declaration_range = self.source_range_bytes(declaration.start_byte(), declaration_end);
        let presentation_signature = self
            .source
            .get(declaration_range.start_byte..declaration_range.end_byte)
            .unwrap_or(&name)
            .trim()
            .to_string();
        let canonical_signature = canonical_callable_signature(
            declaration,
            function_declarator,
            name_node,
            &name,
            self.source,
            self.is_cpp,
            &presentation_signature,
        );
        let syntax_error_overlap = self.error_depth > 0 || contains_error_or_missing(declaration);
        let signature_fidelity = if syntax_error_overlap {
            SignatureFidelity::Malformed
        } else {
            SignatureFidelity::AstExact
        };
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
            canonical_signature,
            self.path_if_internal(internal),
            owner_kind
        );
        let entity_key = digest(&family_input);
        let anchor_fingerprint = digest(&format!(
            "{}|{:?}|{}|{}|{}|{}",
            entity_key,
            role,
            self.path,
            declaration_range.start_byte,
            declaration_range.end_byte,
            presentation_signature
        ));
        let body_range = body.map(|body| self.source_range(body));

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
            canonical_signature,
            presentation_signature,
            signature_fidelity,
            name_range: self.source_range(name_node),
            declaration_range,
            body_range,
            guard: preprocessor_guard(declaration, self.source),
            provenance: FactProvenance::Ast,
            syntax_error_overlap,
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
            guard: preprocessor_guard(call, self.source),
            provenance: FactProvenance::Ast,
            // `enter(call_expression)` runs before the walker reaches the
            // argument subtree, so `error_depth` alone only sees malformed
            // ancestors.  A trailing comma or a missing closing parenthesis
            // is represented by an ERROR/missing descendant of this call and
            // must make its arity evidence unreliable as well.
            syntax_error_overlap: self.error_depth > 0 || contains_error_or_missing(call),
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
            canonical_signature: String::new(),
            presentation_signature: String::new(),
            signature_fidelity: SignatureFidelity::AstExact,
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

    fn qualify_record_name(&self, record_name: &str) -> String {
        let mut names = self.namespace_names();
        names.push(record_name.to_string());
        names.join("::")
    }

    fn owner_matches_known_record(&self, owner: &str, namespaces: &[String]) -> bool {
        if self.record_names.contains(owner) {
            return true;
        }
        if owner.contains("::") || namespaces.is_empty() {
            return false;
        }
        let mut qualified = namespaces.to_vec();
        qualified.push(owner.to_string());
        self.record_names.contains(&qualified.join("::"))
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

    fn source_range_bytes(&self, start_byte: usize, end_byte: usize) -> SourceRange {
        let start_row = self
            .line_starts
            .partition_point(|line_start| *line_start <= start_byte)
            .saturating_sub(1);
        let end_row = self
            .line_starts
            .partition_point(|line_start| *line_start <= end_byte)
            .saturating_sub(1);
        SourceRange {
            start: SourcePosition {
                line: start_row as u32,
                character: utf16_col(self.source, self.line_starts, start_row, start_byte),
            },
            end: SourcePosition {
                line: end_row as u32,
                character: utf16_col(self.source, self.line_starts, end_row, end_byte),
            },
            start_byte,
            end_byte,
        }
    }
}

fn macro_like_identifier(name: &str) -> bool {
    name.bytes()
        .all(|byte| byte == b'_' || byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

#[cfg(test)]
mod canonical_signature_tests {
    use super::canonical_full_signature;

    #[test]
    fn whitespace_normalization_never_joins_distinct_operator_tokens() {
        assert_ne!(
            canonical_full_signature("bool inspect(bool a = left & &right);"),
            canonical_full_signature("bool inspect(bool a = left&&right);")
        );
        assert_eq!(
            canonical_full_signature("extern int lookup ( int key , const char * value );"),
            canonical_full_signature("extern int lookup(int key,const char*value)")
        );
    }
}
