//! Construction-only accounting of retained Rust container capacities.
//! File facts currently own their allocations (no shared Arc payloads).
use super::FileSemanticIndex;
use std::mem::size_of;
#[cfg(test)]
thread_local! {
    static ACCOUNTED_ENTRIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
#[cfg(test)]
pub(super) fn reset_accounted_entries() {
    ACCOUNTED_ENTRIES.set(0);
}
#[cfg(test)]
pub(super) fn accounted_entries() -> usize {
    ACCOUNTED_ENTRIES.get()
}
#[cfg(test)]
pub(super) fn record_accounted_entries(count: usize) {
    if super::budget::active() {
        ACCOUNTED_ENTRIES.set(ACCOUNTED_ENTRIES.get().saturating_add(count));
    }
}

pub(super) trait HeapBytes {
    fn heap_bytes(&self) -> usize;
}
macro_rules! scalars { ($($ty:ty),* $(,)?) => { $(impl HeapBytes for $ty { fn heap_bytes(&self) -> usize { 0 } })* }; }
scalars!(
    bool,
    usize,
    u8,
    u16,
    u32,
    u64,
    super::ParseFacts,
    super::LookupDomain
);
impl<T: HeapBytes, const N: usize> HeapBytes for [T; N] {
    fn heap_bytes(&self) -> usize {
        self.iter()
            .fold(0usize, |n, v| n.saturating_add(v.heap_bytes()))
    }
}
impl HeapBytes for String {
    fn heap_bytes(&self) -> usize {
        self.capacity()
    }
}
impl<T: HeapBytes> HeapBytes for Option<T> {
    fn heap_bytes(&self) -> usize {
        self.as_ref().map_or(0, HeapBytes::heap_bytes)
    }
}
impl<T: HeapBytes> HeapBytes for Vec<T> {
    fn heap_bytes(&self) -> usize {
        #[cfg(test)]
        record_accounted_entries(self.len());
        self.iter()
            .fold(self.capacity().saturating_mul(size_of::<T>()), |n, v| {
                n.saturating_add(v.heap_bytes())
            })
    }
}
/// Construction-only accounting for facts that are immutable after append.
/// Final parser output still receives a complete retained_bytes check.
#[derive(Default)]
pub(super) struct AppendOnlyVecBytes {
    counted_len: usize,
    payload_heap: usize,
}
impl AppendOnlyVecBytes {
    pub(super) fn observe<T: HeapBytes>(&mut self, values: &Vec<T>) -> usize {
        // A shrink starts a new accounting sequence. This is defensive; AST
        // collection does not retain, clear, or mutate prior payloads.
        if values.len() < self.counted_len {
            self.counted_len = 0;
            self.payload_heap = 0;
        }
        #[cfg(test)]
        record_accounted_entries(values.len() - self.counted_len);
        for value in &values[self.counted_len..] {
            self.payload_heap = self.payload_heap.saturating_add(value.heap_bytes());
        }
        self.counted_len = values.len();
        self.payload_heap
            .saturating_add(values.capacity().saturating_mul(size_of::<T>()))
    }
}

macro_rules! ast_fact_bytes {
    ($($field:ident),* $(,)?) => {
        #[derive(Default)]
        pub(super) struct AstFactBytes { $($field: AppendOnlyVecBytes),* }
        impl AstFactBytes {
            pub(super) fn observe(&mut self, ast: &super::ast::AstIndex) -> usize {
                // Exhaustive pattern: adding a fact vector requires accounting.
                let super::ast::AstIndex { parse_error_count: _, $($field),* } = ast;
                0usize$(.saturating_add(self.$field.observe($field)))*
            }
        }
    };
}
ast_fact_bytes!(
    declarations,
    type_symbols,
    occurrences,
    fields,
    members,
    enum_constants,
    aliases,
    records,
    local_declarations,
    local_bindings,
    callable_anchors,
    call_sites
);

// Exhaustive destructuring makes newly added fact fields a compile error here.
macro_rules! struct_heap { ($ty:path, { $($field:ident),* }) => { impl HeapBytes for $ty { fn heap_bytes(&self) -> usize { let Self { $($field),* } = self; 0usize$(.saturating_add($field.heap_bytes()))* } } }; }
impl FileSemanticIndex {
    pub(crate) fn retained_bytes(&self) -> usize {
        size_of::<Self>().saturating_add(self.heap_bytes())
    }
}
impl HeapBytes for crate::call_model::LinkageDomain {
    fn heap_bytes(&self) -> usize {
        match self {
            Self::Internal(s) | Self::Package(s) => s.capacity(),
            Self::External | Self::Unknown => 0,
        }
    }
}
impl HeapBytes for crate::semantic_model::DeclarationBacking {
    fn heap_bytes(&self) -> usize {
        match self {
            Self::CallableAnchor { fingerprint } | Self::TypeAlias { fingerprint } => {
                fingerprint.capacity()
            }
            Self::Record { record_key } => record_key.capacity(),
            Self::SourceRange { .. } | Self::None => 0,
        }
    }
}
impl HeapBytes for crate::semantic_model::AliasTarget {
    fn heap_bytes(&self) -> usize {
        match self {
            Self::RecordKey(s) | Self::UnresolvedTypeName(s) => s.capacity(),
            Self::NamedRecord { tag, kind: _ } => tag.capacity(),
        }
    }
}
impl HeapBytes for crate::semantic_model::DeclaratorShape {
    fn heap_bytes(&self) -> usize {
        match self {
            Self::Pointer { qualifiers } | Self::Qualified { qualifiers } => {
                qualifiers.heap_bytes()
            }
            Self::Array { extent_text } => extent_text.capacity(),
            Self::Function { signature } | Self::FunctionPointer { signature } => {
                signature.capacity()
            }
            Self::Identity | Self::Unsupported => 0,
        }
    }
}
impl HeapBytes for crate::semantic_model::TypeNameDomain {
    fn heap_bytes(&self) -> usize {
        0
    }
}
struct_heap!(super::FileSemanticIndex, { source_fingerprint, cursor, language, language_evidence, includes, package, imports, build_guard, declarations, fallback_completions, parse_outcome, occurrences, records, fields, members, aliases, callable_anchors, call_sites, local_declarations, local_bindings, diagnostics });
struct_heap!(super::CursorFacts, { spans, truncated });
struct_heap!(super::CursorSyntax, { start_byte, end_byte, domain, qualifier, conditional, owner_type });
struct_heap!(super::ParseDiagnostics, { parse_error_count, fallback_used, lexical_source, ast_source, requested_facts, recovery, recovery_budget_exhausted, coverage });
struct_heap!(crate::semantic_model::DeclarationCoverage, { summary, gaps });
struct_heap!(crate::semantic_model::CoverageGap, { range, groups, reason, evidence, related_declarations, recovery_rules });
impl HeapBytes for crate::semantic_model::CoverageEvidence {
    fn heap_bytes(&self) -> usize {
        0
    }
}
impl HeapBytes for crate::semantic_model::CoverageReason {
    fn heap_bytes(&self) -> usize {
        0
    }
}
struct_heap!(crate::call_model::SourceRange, { start, end, start_byte, end_byte });
struct_heap!(crate::call_model::SourcePosition, { line, character });
struct_heap!(crate::semantic_model::DeclarationCoverageSummary, { requested_groups, checked_groups, partial_groups, reason_groups, candidate_regions, explained_regions, non_declaration_regions, uncovered_regions, candidate_bytes, uncovered_bytes, node_visits, recovered_regions, truncated });
struct_heap!(super::recovery::RecoveryDiagnostic, { start_byte, end_byte, affected_start_byte, affected_end_byte, rule, mapping, outcome });
impl HeapBytes for super::recovery::RecoveryOutcome {
    fn heap_bytes(&self) -> usize {
        0
    }
}
impl HeapBytes for super::recovery::RecoveryMapping {
    fn heap_bytes(&self) -> usize {
        0
    }
}
impl HeapBytes for super::recovery::RecoveryRule {
    fn heap_bytes(&self) -> usize {
        0
    }
}
impl HeapBytes for super::FactSource {
    fn heap_bytes(&self) -> usize {
        0
    }
}
struct_heap!(super::LocalBinding, { name, kind, namespace, type_text, decl_start_byte, function_start_byte, function_end_byte, scope_start_byte, scope_end_byte });
impl HeapBytes for super::LocalBindingNamespace {
    fn heap_bytes(&self) -> usize {
        0
    }
}
impl HeapBytes for super::LocalBindingKind {
    fn heap_bytes(&self) -> usize {
        0
    }
}
struct_heap!(super::LocalDeclaration, { name, record_type, decl_start_byte });
struct_heap!(crate::call_model::CallSiteFact, { path, caller_entity_key, expression_range, callee_range, callee_name, qualified_name, form, argument_count, guard, provenance, syntax_error_overlap, site_fingerprint });
impl HeapBytes for crate::call_model::FactProvenance {
    fn heap_bytes(&self) -> usize {
        0
    }
}
impl HeapBytes for crate::call_model::CallForm {
    fn heap_bytes(&self) -> usize {
        0
    }
}
struct_heap!(crate::call_model::CallableAnchor, { path, name, qualified_name, owner, owner_kind, kind, role, linkage, signature, canonical_signature, presentation_signature, signature_fidelity, name_range, declaration_range, body_range, guard, provenance, syntax_error_overlap, entity_key, anchor_fingerprint });
impl HeapBytes for crate::call_model::SignatureFidelity {
    fn heap_bytes(&self) -> usize {
        0
    }
}
struct_heap!(crate::call_model::SignatureShape, { normalized, min_arity, max_arity, variadic });
impl HeapBytes for crate::call_model::AnchorRole {
    fn heap_bytes(&self) -> usize {
        0
    }
}
impl HeapBytes for crate::call_model::CallableKind {
    fn heap_bytes(&self) -> usize {
        0
    }
}
impl HeapBytes for crate::call_model::OwnerKindHint {
    fn heap_bytes(&self) -> usize {
        0
    }
}
struct_heap!(crate::semantic_model::TypeAlias, { alias, target, start_byte, end_byte, start_line, start_col, end_line, end_col, declaration_range, declaration_hash, underlying_spelling, declarator_shape, target_fidelity, fingerprint, owner, guard });
impl HeapBytes for crate::semantic_model::AliasTargetFidelity {
    fn heap_bytes(&self) -> usize {
        0
    }
}
struct_heap!(crate::semantic_model::MemberDef, { record_key, name, kind, confidence, type_name, type_domain, start_byte, end_byte, start_line, start_col, end_line, end_col, signature, guard });
impl HeapBytes for crate::semantic_model::MemberConfidence {
    fn heap_bytes(&self) -> usize {
        0
    }
}
impl HeapBytes for crate::semantic_model::MemberKind {
    fn heap_bytes(&self) -> usize {
        0
    }
}
struct_heap!(crate::semantic_model::FieldDef, { record_key, name, start_byte, end_byte, start_line, start_col, end_line, end_col, signature });
struct_heap!(crate::semantic_model::RecordDef, { record_key, display_name, tag_name, typedef_name, kind, start_byte, end_byte, start_line, start_col, end_line, end_col, name_range, body_range, declaration_range, declaration_hash, range_fidelity, confidence, signature, owner, guard });
impl HeapBytes for crate::semantic_model::RecordConfidence {
    fn heap_bytes(&self) -> usize {
        0
    }
}
impl HeapBytes for crate::semantic_model::RecordRangeFidelity {
    fn heap_bytes(&self) -> usize {
        0
    }
}
impl HeapBytes for crate::semantic_model::RecordKind {
    fn heap_bytes(&self) -> usize {
        0
    }
}
struct_heap!(crate::semantic_model::Occurrence, { name, start_byte, line, start_col, length, role });
impl HeapBytes for crate::semantic_model::SyntacticRole {
    fn heap_bytes(&self) -> usize {
        0
    }
}
impl HeapBytes for crate::semantic_model::ParseOutcome {
    fn heap_bytes(&self) -> usize {
        0
    }
}
struct_heap!(crate::semantic_model::FallbackCompletionFact, { name, kind_hint, range, detail });
impl HeapBytes for crate::semantic_model::CompletionKindHint {
    fn heap_bytes(&self) -> usize {
        0
    }
}
scalars!(crate::semantic_model::DeclarationTagKind);
struct_heap!(crate::semantic_model::DeclarationFact, { tag_kind, identity, name, qualified_name, declaration_kind, role, path, name_range, declaration_range, canonical_signature, declarator_shape, has_initializer, owner, linkage, guard, backing });
impl HeapBytes for crate::semantic_model::SemanticDeclarationRole {
    fn heap_bytes(&self) -> usize {
        0
    }
}
impl HeapBytes for crate::semantic_model::SemanticDeclarationKind {
    fn heap_bytes(&self) -> usize {
        0
    }
}
struct_heap!(crate::semantic_model::DeclarationIdentity, { locator, logical_key, language, language_fidelity, provenance, fact_fidelity, role });
impl HeapBytes for crate::semantic_model::SemanticFactFidelity {
    fn heap_bytes(&self) -> usize {
        0
    }
}
impl HeapBytes for crate::semantic_model::SemanticFactProvenance {
    fn heap_bytes(&self) -> usize {
        0
    }
}
impl HeapBytes for crate::semantic_model::LanguageFidelity {
    fn heap_bytes(&self) -> usize {
        0
    }
}
impl HeapBytes for crate::semantic_model::SemanticLanguage {
    fn heap_bytes(&self) -> usize {
        0
    }
}
struct_heap!(crate::semantic_model::LogicalEntityKey, { qualified_name, declaration_kind, owner, canonical_signature, linkage_domain, guard_fingerprint });
struct_heap!(crate::semantic_model::DeclarationLocator, { workspace_id, path, range, fingerprint });
struct_heap!(crate::semantic_model::ImportFact, { path, alias, path_range, declaration_range });
struct_heap!(crate::semantic_model::PackageFact, { name, name_range });
struct_heap!(crate::semantic_model::Include, { line, target_text });
struct_heap!(crate::semantic_model::LanguageEvidence, { source_kind, fidelity, ambiguous, rule_version, configuration_key, probe_limit_reached });
impl HeapBytes for crate::semantic_model::LanguageSourceKind {
    fn heap_bytes(&self) -> usize {
        0
    }
}

struct_heap!(super::RawDeclaration, { name, kind, role, start_byte, end_byte, start_line, start_col, end_line, end_col, signature, tag_kind, guard, container, incomplete });
scalars!(super::SymbolKind, super::SymbolRole, &'static str);
struct_heap!(super::ast::AstIndex, { parse_error_count, declarations, type_symbols, occurrences, fields, members, enum_constants, aliases, records, local_declarations, local_bindings, callable_anchors, call_sites });

#[cfg(test)]
mod tests {
    #[test]
    fn append_only_budget_counts_spare_capacity_and_new_payloads() {
        use super::{AppendOnlyVecBytes, HeapBytes};
        let mut ledger = AppendOnlyVecBytes::default();
        let mut values = Vec::with_capacity(8);
        let mut first = String::with_capacity(2048);
        first.push_str("field");
        values.push(first);
        assert_eq!(ledger.observe(&values), values.heap_bytes());
        values.reserve_exact(128);
        assert_eq!(ledger.observe(&values), values.heap_bytes());
        values.push("another field".into());
        assert_eq!(ledger.observe(&values), values.heap_bytes());
        assert_eq!(ledger.observe(&values), values.heap_bytes());
        values.clear();
        assert_eq!(ledger.observe(&values), values.heap_bytes());
    }
    #[test]
    fn retained_capacity_saturates_in_nested_containers() {
        use super::HeapBytes;
        struct Huge;
        impl HeapBytes for Huge {
            fn heap_bytes(&self) -> usize {
                usize::MAX
            }
        }
        assert_eq!(Some(vec![Huge, Huge]).heap_bytes(), usize::MAX);
        assert_eq!([Huge, Huge].heap_bytes(), usize::MAX);
    }

    #[test]
    fn retained_bytes_counts_spare_nested_capacity() {
        let mut index = super::super::parse_thread_local_with_selection(
            std::path::Path::new("sample.c"),
            "int sample;",
            crate::config::LanguageSelection::explicit(crate::config::SourceLanguage::C),
            super::super::ParseFacts::INDEX,
        );
        let before = index.retained_bytes();
        let name = &mut index.declarations[0].identity.locator.path;
        let old_capacity = name.capacity();
        name.reserve(8192);
        let added = name.capacity() - old_capacity;
        assert_eq!(index.retained_bytes(), before + added);
    }
}
