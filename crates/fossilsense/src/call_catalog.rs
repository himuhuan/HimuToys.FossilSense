//! Immutable, protocol-neutral read model for one-hop call relations.

use std::collections::{HashMap, HashSet};

use crate::call_model::{
    AnchorRole, CallForm, CallRelation, CallSiteFact, CallableAnchor, CallableEntity, CallableKind,
    CallableLocator, CoverageSummary, EvidenceCode, EvidenceLedger, FactProvenance, LinkageDomain,
    OwnerKindHint, RelationConfidence, RelationDirection, SignatureShape, SourcePosition,
    SourceRange,
};
use crate::store::views::{CallCoverageRow, CallSiteRow, CallableAnchorRow};

#[derive(Debug, Clone, Default)]
pub struct RelationCatalog {
    entities: HashMap<String, CallableEntity>,
    by_name: HashMap<String, Vec<String>>,
    by_path: HashMap<String, Vec<String>>,
    call_sites: Vec<CallSiteFact>,
    by_caller: HashMap<String, Vec<usize>>,
    outgoing_cache: HashMap<String, Vec<CallRelation>>,
    incoming_cache: HashMap<String, Vec<CallRelation>>,
    coverage: CoverageSummary,
}

impl RelationCatalog {
    #[cfg(test)]
    pub fn build(anchors: Vec<CallableAnchorRow>, calls: Vec<CallSiteRow>) -> Self {
        Self::build_with_coverage(
            anchors,
            calls,
            CallCoverageRow {
                eligible_files: 0,
                analyzed_files: 0,
                fallback_files: 0,
                callable_anchors: 0,
                call_sites: 0,
            },
        )
    }

    pub fn build_with_coverage(
        anchors: Vec<CallableAnchorRow>,
        calls: Vec<CallSiteRow>,
        coverage: CallCoverageRow,
    ) -> Self {
        Self::from_facts(
            anchors.into_iter().map(anchor_from_row).collect(),
            calls.into_iter().map(call_from_row).collect(),
            CoverageSummary {
                eligible_files: coverage.eligible_files,
                analyzed_files: coverage.analyzed_files,
                fallback_files: coverage.fallback_files,
                external_bodies_limited: true,
            },
        )
    }

    pub fn with_overlays(
        &self,
        mut overlays: Vec<(String, Vec<CallableAnchor>, Vec<CallSiteFact>)>,
    ) -> Self {
        let shadowed: HashSet<String> = overlays.iter().map(|(path, _, _)| path.clone()).collect();
        let mut all_anchors: Vec<_> = self
            .entities
            .values()
            .flat_map(|entity| entity.variants.iter().cloned())
            .filter(|anchor| !shadowed.contains(&anchor.path))
            .collect();
        let mut all_calls: Vec<_> = self
            .call_sites
            .iter()
            .filter(|call| !shadowed.contains(&call.path))
            .cloned()
            .collect();
        for (path, anchors, calls) in &mut overlays {
            for anchor in anchors.iter_mut() {
                anchor.path = path.clone();
            }
            for call in calls.iter_mut() {
                call.path = path.clone();
            }
            all_anchors.append(anchors);
            all_calls.append(calls);
        }
        Self::from_facts(all_anchors, all_calls, self.coverage.clone())
    }

    fn from_facts(
        anchors: Vec<CallableAnchor>,
        call_sites: Vec<CallSiteFact>,
        coverage: CoverageSummary,
    ) -> Self {
        let mut grouped: HashMap<String, Vec<CallableAnchor>> = HashMap::new();
        for anchor in anchors {
            grouped
                .entry(anchor.entity_key.clone())
                .or_default()
                .push(anchor);
        }

        let mut entities = HashMap::new();
        let mut by_name: HashMap<String, Vec<String>> = HashMap::new();
        let mut by_path: HashMap<String, Vec<String>> = HashMap::new();
        for (key, mut variants) in grouped {
            variants.sort_by_key(primary_anchor_order);
            variants.dedup_by(|a, b| a.anchor_fingerprint == b.anchor_fingerprint);
            let primary_anchor = variants[0].clone();
            let entity = CallableEntity {
                entity_key: key.clone(),
                name: primary_anchor.name.clone(),
                qualified_name: primary_anchor.qualified_name.clone(),
                owner: primary_anchor.owner.clone(),
                owner_kind: primary_anchor.owner_kind,
                kind: primary_anchor.kind,
                linkage: primary_anchor.linkage.clone(),
                signature: primary_anchor.signature.clone(),
                primary_anchor,
                variants,
            };
            if eligible_free_function(&entity) {
                by_name
                    .entry(entity.name.clone())
                    .or_default()
                    .push(key.clone());
            }
            for path in entity.variants.iter().map(|anchor| anchor.path.clone()) {
                by_path.entry(path).or_default().push(key.clone());
            }
            entities.insert(key, entity);
        }
        for keys in by_name.values_mut().chain(by_path.values_mut()) {
            keys.sort();
            keys.dedup();
        }

        let mut by_caller: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, call) in call_sites.iter().enumerate() {
            by_caller
                .entry(call.caller_entity_key.clone())
                .or_default()
                .push(index);
        }
        let mut catalog = Self {
            entities,
            by_name,
            by_path,
            call_sites,
            by_caller,
            outgoing_cache: HashMap::new(),
            incoming_cache: HashMap::new(),
            coverage,
        };
        let caller_keys: Vec<_> = catalog.by_caller.keys().cloned().collect();
        for caller_key in caller_keys {
            let outgoing = catalog.compute_outgoing(&caller_key);
            for relation in &outgoing {
                if let Some(callee) = &relation.callee {
                    let mut incoming = relation.clone();
                    incoming.direction = RelationDirection::Incoming;
                    catalog
                        .incoming_cache
                        .entry(callee.entity_key.clone())
                        .or_default()
                        .push(incoming);
                }
            }
            catalog.outgoing_cache.insert(caller_key, outgoing);
        }
        for relations in catalog.incoming_cache.values_mut() {
            *relations = aggregate_relations(std::mem::take(relations));
        }
        catalog
    }

    #[cfg(test)]
    pub fn entity(&self, key: &str) -> Option<&CallableEntity> {
        self.entities.get(key)
    }

    pub fn entity_at(&self, path: &str, position: SourcePosition) -> Option<&CallableEntity> {
        self.entities_at(path, position).into_iter().next()
    }

    pub fn entities_at(&self, path: &str, position: SourcePosition) -> Vec<&CallableEntity> {
        let mut call_candidates = Vec::new();
        for call in self
            .call_sites
            .iter()
            .filter(|call| call.path == path && position_in_range(position, call.callee_range))
        {
            call_candidates.extend(
                self.resolve_call(call)
                    .into_iter()
                    .map(|(entity, _)| entity),
            );
        }
        call_candidates.sort_by(|a, b| a.entity_key.cmp(&b.entity_key));
        call_candidates.dedup_by(|a, b| a.entity_key == b.entity_key);
        if !call_candidates.is_empty() {
            return call_candidates;
        }

        let matches: Vec<_> = self
            .by_path
            .get(path)
            .into_iter()
            .flatten()
            .filter_map(|key| self.entities.get(key))
            .filter(|entity| eligible_free_function(entity))
            .filter(|entity| {
                entity.variants.iter().any(|anchor| {
                    anchor.path == path
                        && (position_in_range(position, anchor.name_range)
                            || anchor
                                .body_range
                                .is_some_and(|range| position_in_range(position, range)))
                })
            })
            .collect();
        matches
            .into_iter()
            .min_by_key(|entity| {
                let range = entity.primary_anchor.declaration_range;
                range.end_byte.saturating_sub(range.start_byte)
            })
            .into_iter()
            .collect()
    }

    pub fn outgoing(&self, caller_key: &str) -> Vec<CallRelation> {
        self.outgoing_cache
            .get(caller_key)
            .cloned()
            .unwrap_or_default()
    }

    fn compute_outgoing(&self, caller_key: &str) -> Vec<CallRelation> {
        let Some(caller) = self.entities.get(caller_key) else {
            return Vec::new();
        };
        let mut relations = Vec::new();
        for index in self.by_caller.get(caller_key).into_iter().flatten() {
            let call = &self.call_sites[*index];
            let candidates = self.resolve_call(call);
            if candidates.is_empty() {
                relations.push(unresolved_relation(
                    caller,
                    call,
                    RelationDirection::Outgoing,
                ));
                continue;
            }
            let ambiguous = candidates.len() > 1;
            for (candidate, evidence) in candidates {
                relations.push(CallRelation {
                    caller: caller.clone(),
                    callee: Some(candidate.clone()),
                    direction: RelationDirection::Outgoing,
                    call_sites: vec![call.clone()],
                    confidence: if ambiguous {
                        RelationConfidence::Ambiguous
                    } else {
                        confidence(&evidence)
                    },
                    evidence,
                    ambiguity_set_id: ambiguous.then(|| call.site_fingerprint.clone()),
                });
            }
        }
        aggregate_relations(relations)
    }

    pub fn incoming(&self, callee_key: &str) -> Vec<CallRelation> {
        self.incoming_cache
            .get(callee_key)
            .cloned()
            .unwrap_or_default()
    }

    pub fn coverage(&self) -> &CoverageSummary {
        &self.coverage
    }

    pub fn resolve_locator(&self, locator: &CallableLocator) -> Option<&CallableEntity> {
        if let Some(entity) = self.entities.get(&locator.entity_key) {
            return Some(entity);
        }
        self.by_path
            .get(&locator.path)?
            .iter()
            .filter_map(|key| self.entities.get(key))
            .filter(|entity| eligible_free_function(entity))
            .filter(|entity| signature_digest(&entity.signature) == locator.signature_digest)
            .min_by_key(|entity| {
                entity
                    .primary_anchor
                    .name_range
                    .start_byte
                    .abs_diff(locator.old_start_byte)
            })
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entities.len()
    }

    fn resolve_call(&self, call: &CallSiteFact) -> Vec<(&CallableEntity, EvidenceLedger)> {
        if !matches!(
            call.form,
            CallForm::DirectName | CallForm::QualifiedName | CallForm::ParenthesizedName
        ) {
            return Vec::new();
        }
        let Some(name) = call.callee_name.as_deref() else {
            return Vec::new();
        };
        let Some(keys) = self.by_name.get(name) else {
            return Vec::new();
        };
        keys.iter()
            .filter_map(|key| self.entities.get(key))
            .filter_map(|candidate| {
                let mut evidence = EvidenceLedger::default();
                if let Some(qualified) = call.qualified_name.as_deref() {
                    if qualified != candidate.qualified_name {
                        return None;
                    }
                    evidence.supports.push(EvidenceCode::ExplicitQualifier);
                }
                if candidate.primary_anchor.path == call.path {
                    evidence.supports.push(EvidenceCode::SameFile);
                }
                match &candidate.linkage {
                    LinkageDomain::Internal(path) if path != &call.path => return None,
                    LinkageDomain::Internal(_) => {
                        evidence.supports.push(EvidenceCode::InternalLinkage)
                    }
                    _ => {}
                }
                if let Some(arity) = call.argument_count {
                    match candidate.signature.accepts_arity(arity) {
                        Some(true) => evidence.supports.push(EvidenceCode::CompatibleArity),
                        Some(false) => return None,
                        None => evidence.unknowns.push(EvidenceCode::CompatibleArity),
                    }
                }
                if call.syntax_error_overlap {
                    evidence.unknowns.push(EvidenceCode::SyntaxErrorOverlap);
                }
                if std::path::Path::new(&candidate.primary_anchor.path).is_absolute()
                    && candidate
                        .variants
                        .iter()
                        .all(|anchor| anchor.body_range.is_none())
                {
                    evidence
                        .unknowns
                        .push(EvidenceCode::ExternalBodyUnavailable);
                }
                if evidence.supports.is_empty() {
                    evidence.supports.push(EvidenceCode::NameOnly);
                }
                Some((candidate, evidence))
            })
            .collect()
    }
}

fn eligible_free_function(entity: &CallableEntity) -> bool {
    entity.kind == CallableKind::Function
        && entity.owner_kind != Some(OwnerKindHint::Record)
        && !(entity.owner.is_some() && entity.owner_kind == Some(OwnerKindHint::Unknown))
}

fn primary_anchor_order(anchor: &CallableAnchor) -> (u8, u8, String, usize) {
    (
        u8::from(anchor.role != AnchorRole::Definition),
        u8::from(anchor.provenance != FactProvenance::Ast),
        anchor.path.clone(),
        anchor.name_range.start_byte,
    )
}

fn position_in_range(position: SourcePosition, range: SourceRange) -> bool {
    (position.line, position.character) >= (range.start.line, range.start.character)
        && (position.line, position.character) <= (range.end.line, range.end.character)
}

fn confidence(evidence: &EvidenceLedger) -> RelationConfidence {
    if evidence.supports.contains(&EvidenceCode::ExplicitQualifier)
        || evidence.supports.contains(&EvidenceCode::InternalLinkage)
    {
        RelationConfidence::High
    } else if evidence.supports.contains(&EvidenceCode::SameFile)
        || evidence.supports.contains(&EvidenceCode::CompatibleArity)
    {
        RelationConfidence::Medium
    } else {
        RelationConfidence::Low
    }
}

fn unresolved_relation(
    caller: &CallableEntity,
    call: &CallSiteFact,
    direction: RelationDirection,
) -> CallRelation {
    let mut evidence = EvidenceLedger::default();
    if matches!(
        call.form,
        CallForm::DirectName | CallForm::QualifiedName | CallForm::ParenthesizedName
    ) {
        evidence.unknowns.push(EvidenceCode::NameOnly);
    } else {
        evidence.unknowns.push(EvidenceCode::UnsupportedCallForm);
    }
    CallRelation {
        caller: caller.clone(),
        callee: None,
        direction,
        call_sites: vec![call.clone()],
        confidence: RelationConfidence::Unresolved,
        evidence,
        ambiguity_set_id: None,
    }
}

fn aggregate_relations(relations: Vec<CallRelation>) -> Vec<CallRelation> {
    let mut grouped: HashMap<
        (String, Option<String>, RelationConfidence, Option<String>),
        CallRelation,
    > = HashMap::new();
    for relation in relations {
        let key = (
            relation.caller.entity_key.clone(),
            relation.callee.as_ref().map(|e| e.entity_key.clone()),
            relation.confidence,
            relation.ambiguity_set_id.clone(),
        );
        grouped
            .entry(key)
            .and_modify(|current| {
                current.call_sites.extend(relation.call_sites.clone());
                merge_evidence(&mut current.evidence, &relation.evidence);
            })
            .or_insert(relation);
    }
    let mut result: Vec<_> = grouped.into_values().collect();
    result.sort_by(|a, b| {
        a.callee
            .as_ref()
            .map(|e| &e.qualified_name)
            .cmp(&b.callee.as_ref().map(|e| &e.qualified_name))
            .then_with(|| a.caller.qualified_name.cmp(&b.caller.qualified_name))
    });
    result
}

fn merge_evidence(target: &mut EvidenceLedger, source: &EvidenceLedger) {
    fn merge(values: &mut Vec<EvidenceCode>, additions: &[EvidenceCode]) {
        let mut seen: HashSet<_> = values.iter().copied().collect();
        values.extend(
            additions
                .iter()
                .copied()
                .filter(|value| seen.insert(*value)),
        );
    }
    merge(&mut target.supports, &source.supports);
    merge(&mut target.contradictions, &source.contradictions);
    merge(&mut target.unknowns, &source.unknowns);
}

fn anchor_from_row(row: CallableAnchorRow) -> CallableAnchor {
    let linkage = match row.linkage_kind.as_str() {
        "external" => LinkageDomain::External,
        // The parser sees an absolute path while active store rows expose the
        // canonical workspace-relative path. Normalize the domain here so a
        // same-file static call compares like with like.
        "internal" => LinkageDomain::Internal(row.path.clone()),
        _ => LinkageDomain::Unknown,
    };
    let declaration_range = row.declaration_range;
    let body_range = row.body_range;
    CallableAnchor {
        path: row.path,
        name: row.name,
        qualified_name: row.qualified_name,
        owner: row.owner,
        owner_kind: parse_owner_kind(row.owner_kind.as_deref()),
        kind: parse_callable_kind(&row.kind),
        role: parse_anchor_role(&row.role),
        linkage,
        signature: SignatureShape {
            normalized: row.signature,
            min_arity: row.min_arity,
            max_arity: row.max_arity,
            variadic: row.variadic,
        },
        name_range: row.name_range,
        declaration_range,
        body_range,
        guard: row.guard,
        provenance: parse_provenance(&row.provenance),
        syntax_error_overlap: row.syntax_error_overlap,
        entity_key: row.entity_key,
        anchor_fingerprint: row.anchor_fingerprint,
    }
}

fn call_from_row(row: CallSiteRow) -> CallSiteFact {
    CallSiteFact {
        path: row.path,
        caller_entity_key: row.caller_entity_key,
        expression_range: row.expression_range,
        callee_range: row.callee_range,
        callee_name: row.callee_name,
        qualified_name: row.qualified_name,
        form: parse_call_form(&row.call_form),
        argument_count: row.argument_count,
        guard: row.guard,
        provenance: parse_provenance(&row.provenance),
        syntax_error_overlap: row.syntax_error_overlap,
        site_fingerprint: row.site_fingerprint,
    }
}

fn parse_callable_kind(value: &str) -> CallableKind {
    match value {
        "synthetic_global_initializer" => CallableKind::SyntheticGlobalInitializer,
        "synthetic_lambda" => CallableKind::SyntheticLambda,
        "function_like_macro" => CallableKind::FunctionLikeMacro,
        _ => CallableKind::Function,
    }
}
fn parse_anchor_role(value: &str) -> AnchorRole {
    match value {
        "definition" => AnchorRole::Definition,
        "synthetic" => AnchorRole::Synthetic,
        _ => AnchorRole::Declaration,
    }
}
fn parse_owner_kind(value: Option<&str>) -> Option<OwnerKindHint> {
    value.map(|v| match v {
        "namespace" => OwnerKindHint::Namespace,
        "record" => OwnerKindHint::Record,
        _ => OwnerKindHint::Unknown,
    })
}
fn parse_provenance(value: &str) -> FactProvenance {
    match value {
        "synthetic" => FactProvenance::Synthetic,
        "lexical_fallback" => FactProvenance::LexicalFallback,
        _ => FactProvenance::Ast,
    }
}
fn parse_call_form(value: &str) -> CallForm {
    match value {
        "direct_name" => CallForm::DirectName,
        "qualified_name" => CallForm::QualifiedName,
        "parenthesized_name" => CallForm::ParenthesizedName,
        "member_dot" => CallForm::MemberDot,
        "member_arrow" => CallForm::MemberArrow,
        "static_member" => CallForm::StaticMember,
        "function_pointer" => CallForm::FunctionPointer,
        "callable_object" => CallForm::CallableObject,
        "explicit_construction" => CallForm::ExplicitConstruction,
        _ => CallForm::Unsupported,
    }
}

pub fn signature_digest(signature: &SignatureShape) -> String {
    blake3::hash(signature.normalized.as_bytes())
        .to_hex()
        .to_string()
}

#[cfg(test)]
mod tests;
