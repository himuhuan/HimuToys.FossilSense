use std::collections::HashMap;
use std::sync::Arc;

use crate::call_model::{
    CallForm, CallSiteFact, EvidenceCode, EvidenceLedger, FactProvenance, RelationConfidence,
    SourceRange,
};

use super::{CallSiteId, EntityId};

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct EvidenceBits {
    pub(super) supports: u16,
    contradictions: u16,
    unknowns: u16,
}

impl EvidenceBits {
    pub(super) fn support(mut self, code: EvidenceCode) -> Self {
        self.supports |= evidence_bit(code);
        self
    }

    pub(super) fn unknown(mut self, code: EvidenceCode) -> Self {
        self.unknowns |= evidence_bit(code);
        self
    }

    pub(super) fn merge(&mut self, other: Self) {
        self.supports |= other.supports;
        self.contradictions |= other.contradictions;
        self.unknowns |= other.unknowns;
    }

    pub(super) fn contains_support(self, code: EvidenceCode) -> bool {
        self.supports & evidence_bit(code) != 0
    }

    pub(super) fn into_ledger(self) -> EvidenceLedger {
        fn collect(bits: u16) -> Vec<EvidenceCode> {
            EVIDENCE_CODES
                .iter()
                .copied()
                .filter(|code| bits & evidence_bit(*code) != 0)
                .collect()
        }

        EvidenceLedger {
            supports: collect(self.supports),
            contradictions: collect(self.contradictions),
            unknowns: collect(self.unknowns),
        }
    }
}

const EVIDENCE_CODES: [EvidenceCode; 13] = [
    EvidenceCode::SameFile,
    EvidenceCode::InternalLinkage,
    EvidenceCode::ExplicitQualifier,
    EvidenceCode::ReachableDeclaration,
    EvidenceCode::CompatibleArity,
    EvidenceCode::CompatibleSignature,
    EvidenceCode::NameOnly,
    EvidenceCode::OpenIncludeScope,
    EvidenceCode::MacroExpansionUnknown,
    EvidenceCode::PreprocessorBranchUnknown,
    EvidenceCode::SyntaxErrorOverlap,
    EvidenceCode::UnsupportedCallForm,
    EvidenceCode::ExternalBodyUnavailable,
];

fn evidence_bit(code: EvidenceCode) -> u16 {
    1u16 << match code {
        EvidenceCode::SameFile => 0,
        EvidenceCode::InternalLinkage => 1,
        EvidenceCode::ExplicitQualifier => 2,
        EvidenceCode::ReachableDeclaration => 3,
        EvidenceCode::CompatibleArity => 4,
        EvidenceCode::CompatibleSignature => 5,
        EvidenceCode::NameOnly => 6,
        EvidenceCode::OpenIncludeScope => 7,
        EvidenceCode::MacroExpansionUnknown => 8,
        EvidenceCode::PreprocessorBranchUnknown => 9,
        EvidenceCode::SyntaxErrorOverlap => 10,
        EvidenceCode::UnsupportedCallForm => 11,
        EvidenceCode::ExternalBodyUnavailable => 12,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct CompactRelationKey {
    pub(super) caller: EntityId,
    pub(super) callee: Option<EntityId>,
    pub(super) confidence: RelationConfidence,
    pub(super) ambiguity_site: Option<CallSiteId>,
}

#[derive(Debug)]
pub(super) struct RelationBuilder {
    pub(super) key: CompactRelationKey,
    pub(super) first_call_site: CallSiteId,
    pub(super) additional_call_sites: Vec<CallSiteId>,
    pub(super) evidence: EvidenceBits,
}

#[derive(Debug)]
pub(super) struct CompactRelation {
    pub(super) key: CompactRelationKey,
    pub(super) call_site_start: u32,
    pub(super) call_site_len: u32,
    pub(super) evidence: EvidenceBits,
}

pub(super) type StringId = u32;

#[derive(Debug, Default)]
pub(super) struct StringPool {
    values: Vec<Arc<str>>,
    ids: HashMap<Arc<str>, StringId>,
}

impl StringPool {
    pub(super) fn intern(&mut self, value: String) -> StringId {
        if let Some(id) = self.ids.get(value.as_str()) {
            return *id;
        }
        let id = u32::try_from(self.values.len()).expect("interned strings exceed compact limit");
        let value: Arc<str> = Arc::from(value);
        self.values.push(value.clone());
        self.ids.insert(value, id);
        id
    }

    pub(super) fn intern_optional(&mut self, value: Option<String>) -> Option<StringId> {
        value.map(|value| self.intern(value))
    }

    pub(super) fn get(&self, id: StringId) -> &str {
        &self.values[id as usize]
    }

    pub(super) fn id(&self, value: &str) -> Option<StringId> {
        self.ids.get(value).copied()
    }
}

#[derive(Debug)]
pub(super) struct StoredCallSite {
    pub(super) path: StringId,
    pub(super) caller_entity_key: StringId,
    pub(super) expression_range: SourceRange,
    pub(super) callee_range: SourceRange,
    pub(super) callee_name: Option<StringId>,
    pub(super) qualified_name: Option<StringId>,
    pub(super) form: CallForm,
    pub(super) argument_count: Option<u32>,
    pub(super) guard: Option<StringId>,
    pub(super) provenance: FactProvenance,
    pub(super) syntax_error_overlap: bool,
    pub(super) site_fingerprint: Box<str>,
}

impl StoredCallSite {
    pub(super) fn from_fact(fact: CallSiteFact, strings: &mut StringPool) -> Self {
        Self {
            path: strings.intern(fact.path),
            caller_entity_key: strings.intern(fact.caller_entity_key),
            expression_range: fact.expression_range,
            callee_range: fact.callee_range,
            callee_name: strings.intern_optional(fact.callee_name),
            qualified_name: strings.intern_optional(fact.qualified_name),
            form: fact.form,
            argument_count: fact.argument_count,
            guard: strings.intern_optional(fact.guard),
            provenance: fact.provenance,
            syntax_error_overlap: fact.syntax_error_overlap,
            site_fingerprint: fact.site_fingerprint.into_boxed_str(),
        }
    }

    pub(super) fn materialize(&self, strings: &StringPool) -> CallSiteFact {
        CallSiteFact {
            path: strings.get(self.path).to_string(),
            caller_entity_key: strings.get(self.caller_entity_key).to_string(),
            expression_range: self.expression_range,
            callee_range: self.callee_range,
            callee_name: self.callee_name.map(|value| strings.get(value).to_string()),
            qualified_name: self
                .qualified_name
                .map(|value| strings.get(value).to_string()),
            form: self.form,
            argument_count: self.argument_count,
            guard: self.guard.map(|value| strings.get(value).to_string()),
            provenance: self.provenance,
            syntax_error_overlap: self.syntax_error_overlap,
            site_fingerprint: self.site_fingerprint.to_string(),
        }
    }
}
