//! Protocol-neutral vocabulary for best-effort call relations.
//!
//! These values describe syntax facts, logical callable candidates, and the
//! quality of a relation result. They deliberately do not claim compiler-level
//! binding and do not depend on parser, persistence, or LSP types.

// The contract lands before its parser/store consumers so later stages compile
// against stable names instead of growing temporary parallel DTOs.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

pub const RELATION_PROTOCOL_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SemanticGeneration(pub u64);

impl SemanticGeneration {
    pub const MISSING: Self = Self(0);

    pub fn is_published(self) -> bool {
        self.0 != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelationRevision {
    pub engine_epoch: u64,
    pub semantic_generation: SemanticGeneration,
    pub overlay_epoch: u64,
    pub resolver_version: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcePosition {
    pub line: u32,
    pub character: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRange {
    pub start: SourcePosition,
    pub end: SourcePosition,
    pub start_byte: usize,
    pub end_byte: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallableKind {
    Function,
    SyntheticGlobalInitializer,
    SyntheticLambda,
    FunctionLikeMacro,
}

impl CallableKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::SyntheticGlobalInitializer => "synthetic_global_initializer",
            Self::SyntheticLambda => "synthetic_lambda",
            Self::FunctionLikeMacro => "function_like_macro",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorRole {
    Declaration,
    Definition,
    Synthetic,
}

impl AnchorRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Declaration => "declaration",
            Self::Definition => "definition",
            Self::Synthetic => "synthetic",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnerKindHint {
    Namespace,
    Record,
    Unknown,
}

impl OwnerKindHint {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Namespace => "namespace",
            Self::Record => "record",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "file")]
pub enum LinkageDomain {
    External,
    Internal(String),
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactProvenance {
    Ast,
    LexicalFallback,
    Synthetic,
}

impl FactProvenance {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ast => "ast",
            Self::LexicalFallback => "lexical_fallback",
            Self::Synthetic => "synthetic",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallFactAvailability {
    Available,
    NotRequested,
    LexicalFallback,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignatureShape {
    pub normalized: String,
    pub min_arity: Option<u32>,
    pub max_arity: Option<u32>,
    pub variadic: bool,
}

impl SignatureShape {
    pub fn accepts_arity(&self, arity: u32) -> Option<bool> {
        let min = self.min_arity?;
        if arity < min {
            return Some(false);
        }
        match self.max_arity {
            Some(max) => Some(arity <= max),
            None if self.variadic => Some(true),
            None => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallableLocator {
    pub workspace_id: String,
    pub path: String,
    pub entity_key: String,
    pub anchor_fingerprint: String,
    pub old_start_byte: usize,
    pub signature_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallableAnchor {
    pub path: String,
    pub name: String,
    pub qualified_name: String,
    pub owner: Option<String>,
    pub owner_kind: Option<OwnerKindHint>,
    pub kind: CallableKind,
    pub role: AnchorRole,
    pub linkage: LinkageDomain,
    pub signature: SignatureShape,
    pub name_range: SourceRange,
    pub declaration_range: SourceRange,
    pub body_range: Option<SourceRange>,
    pub guard: Option<String>,
    pub provenance: FactProvenance,
    pub syntax_error_overlap: bool,
    pub entity_key: String,
    pub anchor_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallableEntity {
    pub entity_key: String,
    pub name: String,
    pub qualified_name: String,
    pub owner: Option<String>,
    pub owner_kind: Option<OwnerKindHint>,
    pub kind: CallableKind,
    pub linkage: LinkageDomain,
    pub signature: SignatureShape,
    pub primary_anchor: CallableAnchor,
    pub variants: Vec<CallableAnchor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallForm {
    DirectName,
    QualifiedName,
    ParenthesizedName,
    MemberDot,
    MemberArrow,
    StaticMember,
    FunctionPointer,
    CallableObject,
    ExplicitConstruction,
    Unsupported,
}

impl CallForm {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DirectName => "direct_name",
            Self::QualifiedName => "qualified_name",
            Self::ParenthesizedName => "parenthesized_name",
            Self::MemberDot => "member_dot",
            Self::MemberArrow => "member_arrow",
            Self::StaticMember => "static_member",
            Self::FunctionPointer => "function_pointer",
            Self::CallableObject => "callable_object",
            Self::ExplicitConstruction => "explicit_construction",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallSiteFact {
    pub path: String,
    pub caller_entity_key: String,
    pub expression_range: SourceRange,
    pub callee_range: SourceRange,
    pub callee_name: Option<String>,
    pub qualified_name: Option<String>,
    pub form: CallForm,
    pub argument_count: Option<u32>,
    pub guard: Option<String>,
    pub provenance: FactProvenance,
    pub syntax_error_overlap: bool,
    pub site_fingerprint: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationConfidence {
    High,
    Medium,
    Low,
    Ambiguous,
    Unresolved,
    Unavailable,
}

impl RelationConfidence {
    pub fn rank(self) -> u8 {
        match self {
            Self::High => 5,
            Self::Medium => 4,
            Self::Low => 3,
            Self::Ambiguous => 2,
            Self::Unresolved => 1,
            Self::Unavailable => 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceCode {
    SameFile,
    InternalLinkage,
    ExplicitQualifier,
    ReachableDeclaration,
    CompatibleArity,
    CompatibleSignature,
    NameOnly,
    OpenIncludeScope,
    MacroExpansionUnknown,
    PreprocessorBranchUnknown,
    SyntaxErrorOverlap,
    UnsupportedCallForm,
    ExternalBodyUnavailable,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceLedger {
    pub supports: Vec<EvidenceCode>,
    pub contradictions: Vec<EvidenceCode>,
    pub unknowns: Vec<EvidenceCode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallTargetCandidate {
    pub entity: CallableEntity,
    pub confidence: RelationConfidence,
    pub evidence: EvidenceLedger,
    pub ambiguity_set_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationDirection {
    Incoming,
    Outgoing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallRelation {
    pub caller: CallableEntity,
    pub callee: Option<CallableEntity>,
    pub direction: RelationDirection,
    pub call_sites: Vec<CallSiteFact>,
    pub confidence: RelationConfidence,
    pub evidence: EvidenceLedger,
    pub ambiguity_set_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetState {
    Complete,
    PageLimited,
    ScanLimited,
    CandidateLimited,
    TimeLimited,
    Cancelled,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageSummary {
    pub eligible_files: u64,
    pub analyzed_files: u64,
    pub fallback_files: u64,
    pub external_bodies_limited: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_shape_distinguishes_incompatible_unknown_and_variadic_arity() {
        let fixed = SignatureShape {
            normalized: "(int,int)".into(),
            min_arity: Some(2),
            max_arity: Some(2),
            variadic: false,
        };
        assert_eq!(fixed.accepts_arity(1), Some(false));
        assert_eq!(fixed.accepts_arity(2), Some(true));

        let variadic = SignatureShape {
            normalized: "(const char*,...)".into(),
            min_arity: Some(1),
            max_arity: None,
            variadic: true,
        };
        assert_eq!(variadic.accepts_arity(5), Some(true));

        let unknown = SignatureShape {
            normalized: "()".into(),
            min_arity: None,
            max_arity: None,
            variadic: false,
        };
        assert_eq!(unknown.accepts_arity(0), None);
    }

    #[test]
    fn relation_revision_has_stable_camel_case_wire_shape() {
        let value = serde_json::to_value(RelationRevision {
            engine_epoch: 7,
            semantic_generation: SemanticGeneration(4),
            overlay_epoch: 3,
            resolver_version: 1,
        })
        .unwrap();
        assert_eq!(value["engineEpoch"], 7);
        assert_eq!(value["semanticGeneration"], 4);
        assert_eq!(value["overlayEpoch"], 3);
        assert_eq!(value["resolverVersion"], 1);
    }

    #[test]
    fn relation_confidence_order_is_explicit() {
        assert!(RelationConfidence::High.rank() > RelationConfidence::Medium.rank());
        assert!(RelationConfidence::Medium.rank() > RelationConfidence::Low.rank());
        assert!(RelationConfidence::Ambiguous.rank() > RelationConfidence::Unresolved.rank());
        assert!(RelationConfidence::Unresolved.rank() > RelationConfidence::Unavailable.rank());
    }
}
