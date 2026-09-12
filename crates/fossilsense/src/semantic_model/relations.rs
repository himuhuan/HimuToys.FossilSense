//! Source facts for bounded relation queries. These are not resolved graph edges.
use super::{EntityDomain, SemanticFactFidelity};
use crate::call_model::SourceRange;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceRole {
    Read,
    Write,
    ReadWrite,
    Call,
    TypeUse,
    Address,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationSource {
    pub range: SourceRange,
    pub fingerprint: String,
    pub enclosing_callable: Option<String>,
    pub owner: Option<String>,
    pub guard: Option<String>,
    pub fidelity: SemanticFactFidelity,
    pub provenance: super::SemanticFactProvenance,
}

/// A syntactic use. Path and source revision belong to the containing file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingSiteFact {
    pub source: RelationSource,
    pub spelling: String,
    pub domain: EntityDomain,
    pub role: ReferenceRole,
    pub receiver: Option<ReceiverFact>,
    pub member_access: bool,
    pub qualifier: Option<String>,
    /// Lexically bound identifier anchor, used only within a captured document.
    pub local_anchor: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiverFact {
    pub spelling: String,
    pub type_name: Option<String>,
    pub tag_domain: bool,
    pub chain: Vec<String>,
    pub object_anchor: Option<usize>,
    pub object_scope: Option<SourceRange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BaseAccess {
    Public,
    Protected,
    Private,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplicitBaseFact {
    pub source: RelationSource,
    pub derived_record_key: String,
    pub derived_name: String,
    pub base_name: String,
    pub access: BaseAccess,
    pub is_virtual: bool,
    pub dependent: bool,
}

/// Object identity is part of a slot: two objects with the same field are distinct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndirectAssignmentFact {
    pub source: RelationSource,
    pub slot: ReceiverFact,
    pub member: Option<String>,
    pub target_name: Option<String>,
    pub branch: Option<SourceRange>,
    pub unknown_write: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MacroFact {
    pub source: RelationSource,
    pub name: String,
    pub function_like: bool,
    pub undef: bool,
    /// Only literal, non-parameter call names proven in the replacement tokens.
    pub direct_calls: Vec<String>,
    pub replacement_range: SourceRange,
    pub expansion_not_evaluated: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RelationFacts {
    pub binding_sites: Vec<BindingSiteFact>,
    pub explicit_bases: Vec<ExplicitBaseFact>,
    pub indirect_assignments: Vec<IndirectAssignmentFact>,
    pub macros: Vec<MacroFact>,
}
