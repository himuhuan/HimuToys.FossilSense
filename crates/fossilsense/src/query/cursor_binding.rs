use crate::parser::{
    CursorSyntax, FileSemanticIndex, LocalBindingKind, LocalBindingNamespace, LookupDomain,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalBindingRef {
    pub document_version: i32,
    pub binding_index: usize,
    pub declaration_byte: usize,
    pub scope_start_byte: usize,
    pub scope_end_byte: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingReason {
    NoLocalBinding,
    LocalKindMismatch,
    DedicatedDomain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingResolution<T> {
    Resolved(T),
    Ambiguous(Vec<T>),
    UnresolvedWithinDomain {
        domain: LookupDomain,
        reason: BindingReason,
    },
    Unsupported {
        domain_hint: LookupDomain,
    },
}

pub fn resolve_local_cursor(
    index: &FileSemanticIndex,
    syntax: &CursorSyntax,
    word: &str,
    byte: usize,
    version: i32,
) -> BindingResolution<LocalBindingRef> {
    let domain = syntax.domain;
    match domain {
        LookupDomain::NonCode | LookupDomain::Unsupported => {
            return BindingResolution::Unsupported {
                domain_hint: domain,
            }
        }
        LookupDomain::Member | LookupDomain::Label => {
            return BindingResolution::UnresolvedWithinDomain {
                domain,
                reason: BindingReason::DedicatedDomain,
            }
        }
        LookupDomain::QualifiedValue | LookupDomain::QualifiedType => {
            return BindingResolution::UnresolvedWithinDomain {
                domain,
                reason: BindingReason::NoLocalBinding,
            }
        }
        _ => {}
    }
    let namespace = if domain == LookupDomain::Tag {
        LocalBindingNamespace::Tag
    } else {
        LocalBindingNamespace::Ordinary
    };
    let visible = index
        .local_bindings
        .iter()
        .enumerate()
        .filter(|(_, binding)| {
            let at_declaration = binding.decl_start_byte <= byte
                && byte <= binding.decl_start_byte.saturating_add(binding.name.len());
            binding.name == word
                && binding.namespace == namespace
                && (at_declaration
                    || super::local_completion::local_binding_visible_for_completion(binding, byte))
        });
    // Branch conditions are not evaluated by this best-effort parser. A
    // declaration selected merely because it is lexically last is not proof
    // of an active local binding, including tags and typedefs.
    if visible.clone().any(|(_, binding)| {
        index
            .cursor
            .at(binding.decl_start_byte)
            .is_some_and(|fact| fact.conditional)
    }) {
        return BindingResolution::Unsupported {
            domain_hint: domain,
        };
    }
    let Some(last) = visible
        .clone()
        .map(|(_, binding)| binding.decl_start_byte)
        .max()
    else {
        return BindingResolution::UnresolvedWithinDomain {
            domain,
            reason: BindingReason::NoLocalBinding,
        };
    };
    let mut matching = Vec::new();
    for (binding_index, binding) in visible.filter(|(_, binding)| binding.decl_start_byte == last) {
        let type_use = matches!(domain, LookupDomain::Type | LookupDomain::Tag);
        if type_use != (binding.kind == LocalBindingKind::LocalType) {
            return BindingResolution::UnresolvedWithinDomain {
                domain,
                reason: BindingReason::LocalKindMismatch,
            };
        }
        matching.push(LocalBindingRef {
            document_version: version,
            binding_index,
            declaration_byte: binding.decl_start_byte,
            scope_start_byte: binding.scope_start_byte,
            scope_end_byte: binding.scope_end_byte,
        });
    }
    if matching.len() == 1 {
        BindingResolution::Resolved(matching.remove(0))
    } else {
        BindingResolution::Ambiguous(matching)
    }
}
