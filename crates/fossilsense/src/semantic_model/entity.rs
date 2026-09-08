//! Entity discovery keys are separate from conditional source occurrences.
use super::{DeclarationFact, SemanticDeclarationKind, SemanticFamily, SemanticLanguage};
use serde::{Deserialize, Serialize};

pub const ENTITY_RELATION_FORMAT_VERSION: i64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum EntityDomain {
    Callable,
    Tag,
    Alias,
    Value,
    Macro,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EntityIdentity {
    pub family: SemanticFamily,
    pub domain: EntityDomain,
    pub qualified_name: String,
    pub owner: Option<String>,
    pub structural_signature: Option<String>,
    pub linkage_domain: String,
}

impl EntityIdentity {
    pub fn domain_for_declaration(fact: &DeclarationFact) -> EntityDomain {
        declaration_domain(fact)
    }
    pub fn from_declaration(fact: &DeclarationFact) -> Self {
        let domain = declaration_domain(fact);
        let key = &fact.identity.logical_key;
        Self {
            family: fact.identity.language.semantic_family(),
            domain,
            qualified_name: fact.qualified_name.clone(),
            owner: fact.owner.clone(),
            structural_signature: declaration_signature(fact, domain).map(str::to_owned),
            linkage_domain: key.linkage_domain.clone(),
        }
    }
    pub fn digest_for_declaration(fact: &DeclarationFact) -> [u8; 12] {
        let domain = declaration_domain(fact);
        digest_parts(
            fact.identity.language.semantic_family(),
            domain,
            &fact.qualified_name,
            fact.owner.as_deref(),
            declaration_signature(fact, domain),
            &fact.identity.logical_key.linkage_domain,
        )
    }
    pub fn digest(&self) -> [u8; 12] {
        digest_parts(
            self.family,
            self.domain,
            &self.qualified_name,
            self.owner.as_deref(),
            self.structural_signature.as_deref(),
            &self.linkage_domain,
        )
    }
}
fn declaration_domain(fact: &DeclarationFact) -> EntityDomain {
    match fact.declaration_kind {
        SemanticDeclarationKind::Function | SemanticDeclarationKind::Method => {
            EntityDomain::Callable
        }
        SemanticDeclarationKind::Type => EntityDomain::Tag,
        SemanticDeclarationKind::Alias => EntityDomain::Alias,
        SemanticDeclarationKind::Macro => EntityDomain::Macro,
        SemanticDeclarationKind::Object | SemanticDeclarationKind::EnumConstant => {
            EntityDomain::Value
        }
    }
}
fn declaration_signature(fact: &DeclarationFact, domain: EntityDomain) -> Option<&str> {
    if domain == EntityDomain::Tag {
        return fact.tag_kind.map(super::DeclarationTagKind::entity_kind);
    }
    if domain != EntityDomain::Callable {
        return None;
    }
    if fact.identity.language == SemanticLanguage::Go {
        fact.identity.logical_key.canonical_signature.as_deref()
    } else {
        fact.canonical_signature.as_deref()
    }
}
fn digest_parts(
    family: SemanticFamily,
    domain: EntityDomain,
    name: &str,
    owner: Option<&str>,
    signature: Option<&str>,
    linkage: &str,
) -> [u8; 12] {
    // Stable versioned framing; writer borrows canonical facts rather than
    // cloning their strings to construct an index key.
    let encoded = serde_json::to_vec(&(
        ENTITY_RELATION_FORMAT_VERSION,
        family,
        domain,
        name,
        owner,
        signature,
        linkage,
    ))
    .expect("entity identity serialization");
    blake3::hash(&encoded).as_bytes()[..12]
        .try_into()
        .expect("12 byte digest")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationStrength {
    Proven,
    CompatibleCandidate,
    Incompatible,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConditionRelation {
    Same,
    Unconditional,
    Unknown,
    Incompatible,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelationEvidence {
    pub strength: RelationStrength,
    pub condition: ConditionRelation,
    pub reachable: bool,
    pub owner_proven: bool,
    pub complete: bool,
}

/// Deliberately small condition algebra. Unknown expressions never connect
/// otherwise incompatible variants by transitive equivalence.
pub fn condition_relation(left: Option<&str>, right: Option<&str>) -> ConditionRelation {
    if left == right {
        return ConditionRelation::Same;
    }
    if left.is_none_or(str::is_empty) || right.is_none_or(str::is_empty) {
        return ConditionRelation::Unconditional;
    }
    let left_terms = condition_literals(left.unwrap_or_default());
    let right_terms = condition_literals(right.unwrap_or_default());
    if left_terms.iter().any(|(name, value)| {
        right_terms
            .iter()
            .any(|(other, other_value)| name == other && value != other_value)
    }) {
        return ConditionRelation::Incompatible;
    }
    ConditionRelation::Unknown
}

/// Interpret only bounded conjunctions of simple parser-produced directives.
/// Unknown/disjunctive expressions never become a proof of incompatibility.
fn condition_literals(guard: &str) -> Vec<(String, bool)> {
    if guard.len() > 4096 || guard.contains("||") {
        return Vec::new();
    }
    let compact = guard;
    let mut literals = Vec::new();
    let mut terms = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    let bytes = compact.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth < 0 {
                    return Vec::new();
                }
            }
            b'&' if depth == 0 && bytes.get(index + 1) == Some(&b'&') => {
                if terms.len() == 32 {
                    return Vec::new();
                }
                terms.push(&compact[start..index]);
                index += 1;
                start = index + 1;
            }
            _ => {}
        }
        index += 1;
    }
    if depth != 0 {
        return Vec::new();
    }
    terms.push(&compact[start..]);
    for term in terms.into_iter().take(32) {
        let mut term = term.trim();
        let mut positive = true;
        for _ in 0..16 {
            if let Some(inner) = term.strip_prefix('!') {
                positive = !positive;
                term = inner.trim();
                continue;
            }
            if term.starts_with('(') && term.ends_with(')') {
                let mut depth = 0i32;
                let enclosing = term.bytes().enumerate().all(|(i, b)| {
                    if b == b'(' {
                        depth += 1;
                    } else if b == b')' {
                        depth -= 1;
                    }
                    depth > 0 || i + 1 == term.len()
                });
                if enclosing {
                    term = term[1..term.len() - 1].trim();
                    continue;
                }
            }
            break;
        }
        let (mut atom, defined) = if let Some(atom) = strip_condition_keyword(term, "#ifndef") {
            positive = !positive;
            (atom, true)
        } else if let Some(atom) = strip_condition_keyword(term, "#ifdef") {
            (atom, true)
        } else if let Some(atom) = strip_condition_keyword(term, "#elif") {
            (atom, false)
        } else if let Some(atom) = strip_condition_keyword(term, "#if") {
            (atom, false)
        } else {
            (term, false)
        };
        if let Some(inner) = atom.strip_prefix('!') {
            positive = !positive;
            atom = inner.trim();
        }
        let (atom, defined) = if let Some(inner) = strip_condition_keyword(atom, "defined") {
            (
                inner
                    .strip_prefix('(')
                    .and_then(|s| s.strip_suffix(')'))
                    .unwrap_or(inner)
                    .trim(),
                true,
            )
        } else {
            (atom.trim(), defined)
        };
        if atom.is_empty()
            || !atom.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || atom.as_bytes()[0].is_ascii_digit()
        {
            continue;
        }
        literals.push((
            format!("{}:{atom}", if defined { "defined" } else { "value" }),
            positive,
        ));
    }
    literals
}

fn strip_condition_keyword<'a>(text: &'a str, keyword: &str) -> Option<&'a str> {
    let rest = text.strip_prefix(keyword)?;
    if rest.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some(rest.trim())
}

pub fn declaration_relation(
    left: &DeclarationFact,
    right: &DeclarationFact,
    reachable: bool,
) -> RelationEvidence {
    let condition = condition_relation(left.guard.as_deref(), right.guard.as_deref());
    let same_identity =
        EntityIdentity::from_declaration(left) == EntityIdentity::from_declaration(right);
    let complete = left.identity.fact_fidelity == super::SemanticFactFidelity::Authoritative
        && right.identity.fact_fidelity == super::SemanticFactFidelity::Authoritative;
    let owner_proven = left.owner == right.owner;
    relation_evidence(same_identity, condition, reachable, owner_proven, complete)
}
fn relation_evidence(
    same_identity: bool,
    condition: ConditionRelation,
    reachable: bool,
    owner_proven: bool,
    complete: bool,
) -> RelationEvidence {
    RelationEvidence {
        strength: if !same_identity || condition == ConditionRelation::Incompatible {
            RelationStrength::Incompatible
        } else if complete && owner_proven && reachable && condition == ConditionRelation::Same {
            RelationStrength::Proven
        } else {
            RelationStrength::CompatibleCandidate
        },
        condition,
        reachable,
        owner_proven,
        complete,
    }
}
/// Adapter from call-specific facts to the same evidence decision. It does not
/// infer record ownership: that proof belongs to MemberResolutionService.
pub fn callable_relation(
    left: &crate::call_model::CallableAnchor,
    right: &crate::call_model::CallableAnchor,
    reachable: bool,
) -> RelationEvidence {
    use crate::call_model::{CallableKind, LinkageDomain, OwnerKindHint, SignatureFidelity};
    let same_identity = left.kind == CallableKind::Function
        && right.kind == CallableKind::Function
        && left.qualified_name == right.qualified_name
        && left.owner == right.owner
        && !left.canonical_signature.is_empty()
        && left.canonical_signature == right.canonical_signature
        && left.linkage == right.linkage
        && matches!(left.linkage, LinkageDomain::External);
    let owner_proven = [left, right]
        .iter()
        .all(|a| matches!(a.owner_kind, None | Some(OwnerKindHint::Namespace)));
    let complete = [left, right]
        .iter()
        .all(|a| a.signature_fidelity == SignatureFidelity::AstExact && !a.syntax_error_overlap);
    relation_evidence(
        same_identity,
        condition_relation(left.guard.as_deref(), right.guard.as_deref()),
        reachable,
        owner_proven,
        complete,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn entity_location_unknown_condition_does_not_bridge_incompatible_variants() {
        assert_eq!(
            condition_relation(Some("ENABLED"), Some("!ENABLED")),
            ConditionRelation::Incompatible
        );
        assert_eq!(
            condition_relation(Some("ENABLED"), Some("OTHER")),
            ConditionRelation::Unknown
        );
        assert_eq!(
            condition_relation(Some("OTHER"), Some("!ENABLED")),
            ConditionRelation::Unknown
        );
        assert_eq!(
            relation_evidence(true, ConditionRelation::Unknown, true, true, true).strength,
            RelationStrength::CompatibleCandidate
        );
        assert_eq!(
            relation_evidence(true, ConditionRelation::Incompatible, true, true, true).strength,
            RelationStrength::Incompatible
        );
    }
}
