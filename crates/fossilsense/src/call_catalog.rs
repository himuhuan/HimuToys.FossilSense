//! Immutable, protocol-neutral read model for one-hop call relations.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

use anyhow::Result;

use crate::call_model::{
    AnchorRole, CallForm, CallRelation, CallSiteFact, CallableAnchor, CallableEntity, CallableKind,
    CallableLocator, CoverageSummary, EvidenceCode, FactProvenance, LinkageDomain, OwnerKindHint,
    RelationConfidence, RelationDirection, SignatureShape, SourcePosition, SourceRange,
};
use crate::store::views::CallFactStoreView;
#[cfg(test)]
use crate::store::views::{CallCoverageRow, CallSiteRow, CallableAnchorRow};

mod compact;
mod rows;
use compact::{
    CompactRelation, CompactRelationKey, EvidenceBits, RelationBuilder, StoredCallSite, StringId,
    StringPool,
};
use rows::{anchor_from_row, call_from_row};

type EntityId = u32;
type CallSiteId = u32;
type RelationId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelationCatalogStats {
    pub entities: usize,
    pub call_sites: usize,
    pub relations: usize,
    pub relation_call_site_refs: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RelationCatalogBuildMetrics {
    pub load_anchors_ms: u128,
    pub load_call_sites_ms: u128,
    pub group_entities_ms: u128,
    pub resolve_relations_ms: u128,
    pub finalize_ms: u128,
}

#[derive(Debug)]
pub struct RelationPage {
    pub relations: Vec<CallRelation>,
    pub total: usize,
    pub site_limited: bool,
}

#[derive(Debug, Default)]
pub struct RelationCatalog {
    entities: Vec<CallableEntity>,
    entity_by_key: HashMap<String, EntityId>,
    by_name: HashMap<String, Vec<EntityId>>,
    by_path: HashMap<String, Vec<EntityId>>,
    strings: StringPool,
    call_sites: Vec<StoredCallSite>,
    calls_by_path: HashMap<StringId, Vec<CallSiteId>>,
    relations: Vec<CompactRelation>,
    relation_call_sites: Vec<CallSiteId>,
    outgoing: HashMap<EntityId, Vec<RelationId>>,
    incoming: HashMap<EntityId, Vec<RelationId>>,
    coverage: CoverageSummary,
    build_metrics: RelationCatalogBuildMetrics,
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

    #[cfg(test)]
    pub fn build_with_coverage(
        anchors: Vec<CallableAnchorRow>,
        calls: Vec<CallSiteRow>,
        coverage: CallCoverageRow,
    ) -> Self {
        Self::from_facts(
            anchors.into_iter().map(anchor_from_row).collect(),
            calls.into_iter().map(call_from_row),
            CoverageSummary {
                eligible_files: coverage.eligible_files,
                analyzed_files: coverage.analyzed_files,
                fallback_files: coverage.fallback_files,
                external_bodies_limited: true,
            },
        )
    }

    pub fn build_from_view(view: &CallFactStoreView<'_>) -> Result<Self> {
        let coverage = view.coverage()?;
        let anchor_started = Instant::now();
        let mut anchors = Vec::with_capacity(coverage.callable_anchors as usize);
        view.visit_all_anchors(|row| {
            anchors.push(anchor_from_row(row));
            Ok(())
        })?;
        let load_anchors_ms = anchor_started.elapsed().as_millis();

        let call_site_started = Instant::now();
        let mut strings = StringPool::default();
        let mut call_sites = Vec::with_capacity(coverage.call_sites as usize);
        view.visit_all_call_sites(|row| {
            call_sites.push(StoredCallSite::from_fact(call_from_row(row), &mut strings));
            Ok(())
        })?;
        let load_call_sites_ms = call_site_started.elapsed().as_millis();

        Ok(Self::from_stored_facts(
            anchors,
            strings,
            call_sites,
            CoverageSummary {
                eligible_files: coverage.eligible_files,
                analyzed_files: coverage.analyzed_files,
                fallback_files: coverage.fallback_files,
                external_bodies_limited: true,
            },
            RelationCatalogBuildMetrics {
                load_anchors_ms,
                load_call_sites_ms,
                ..Default::default()
            },
        ))
    }

    pub fn with_overlays(
        &self,
        mut overlays: Vec<(String, Vec<CallableAnchor>, Vec<CallSiteFact>)>,
    ) -> Self {
        let shadowed: HashSet<String> = overlays.iter().map(|(path, _, _)| path.clone()).collect();
        let mut all_anchors: Vec<_> = self
            .entities
            .iter()
            .flat_map(|entity| entity.variants.iter().cloned())
            .filter(|anchor| !shadowed.contains(&anchor.path))
            .collect();
        let mut all_calls: Vec<_> = self
            .call_sites
            .iter()
            .filter(|call| !shadowed.contains(self.strings.get(call.path)))
            .map(|call| call.materialize(&self.strings))
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

    fn from_facts<I>(anchors: Vec<CallableAnchor>, call_sites: I, coverage: CoverageSummary) -> Self
    where
        I: IntoIterator<Item = CallSiteFact>,
    {
        let mut strings = StringPool::default();
        let call_sites: Vec<_> = call_sites
            .into_iter()
            .map(|call| StoredCallSite::from_fact(call, &mut strings))
            .collect();
        Self::from_stored_facts(
            anchors,
            strings,
            call_sites,
            coverage,
            RelationCatalogBuildMetrics::default(),
        )
    }

    fn from_stored_facts(
        anchors: Vec<CallableAnchor>,
        strings: StringPool,
        call_sites: Vec<StoredCallSite>,
        coverage: CoverageSummary,
        mut build_metrics: RelationCatalogBuildMetrics,
    ) -> Self {
        let group_started = Instant::now();
        let mut grouped: HashMap<String, Vec<CallableAnchor>> = HashMap::new();
        for anchor in anchors {
            grouped
                .entry(anchor.entity_key.clone())
                .or_default()
                .push(anchor);
        }

        let mut entities = Vec::with_capacity(grouped.len());
        let mut entity_by_key = HashMap::with_capacity(grouped.len());
        let mut by_name: HashMap<String, Vec<EntityId>> = HashMap::new();
        let mut by_path: HashMap<String, Vec<EntityId>> = HashMap::new();
        for (key, mut variants) in grouped {
            variants.sort_by(primary_anchor_cmp);
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
            let entity_id = compact_id(entities.len(), "callable entities");
            if eligible_free_function(&entity) {
                by_name
                    .entry(entity.name.clone())
                    .or_default()
                    .push(entity_id);
            }
            for path in entity.variants.iter().map(|anchor| anchor.path.clone()) {
                by_path.entry(path).or_default().push(entity_id);
            }
            entity_by_key.insert(key, entity_id);
            entities.push(entity);
        }
        for ids in by_name.values_mut().chain(by_path.values_mut()) {
            ids.sort_by(|left, right| {
                entity(&entities, *left)
                    .entity_key
                    .cmp(&entity(&entities, *right).entity_key)
            });
            ids.dedup();
        }

        let mut calls_by_path: HashMap<StringId, Vec<CallSiteId>> = HashMap::new();
        for (index, call) in call_sites.iter().enumerate() {
            let call_id = compact_id(index, "call sites");
            calls_by_path.entry(call.path).or_default().push(call_id);
        }

        let mut catalog = Self {
            entities,
            entity_by_key,
            by_name,
            by_path,
            strings,
            call_sites,
            calls_by_path,
            relations: Vec::new(),
            relation_call_sites: Vec::new(),
            outgoing: HashMap::new(),
            incoming: HashMap::new(),
            coverage,
            build_metrics: RelationCatalogBuildMetrics::default(),
        };
        build_metrics.group_entities_ms = group_started.elapsed().as_millis();

        let resolve_started = Instant::now();
        let mut relation_by_key = HashMap::new();
        let mut builders = Vec::new();
        let mut candidates = Vec::new();
        for index in 0..catalog.call_sites.len() {
            let call_id = compact_id(index, "call sites");
            let Some(caller) = catalog
                .entity_by_key
                .get(
                    catalog
                        .strings
                        .get(catalog.call_site(call_id).caller_entity_key),
                )
                .copied()
            else {
                continue;
            };
            catalog.resolve_call_into(call_id, &mut candidates);
            if candidates.is_empty() {
                record_relation(
                    &mut relation_by_key,
                    &mut builders,
                    CompactRelationKey {
                        caller,
                        callee: None,
                        confidence: RelationConfidence::Unresolved,
                        ambiguity_site: None,
                    },
                    call_id,
                    unresolved_evidence(catalog.call_site(call_id)),
                );
                continue;
            }
            let ambiguous = candidates.len() > 1;
            for (callee, evidence) in candidates.iter().copied() {
                record_relation(
                    &mut relation_by_key,
                    &mut builders,
                    CompactRelationKey {
                        caller,
                        callee: Some(callee),
                        confidence: if ambiguous {
                            RelationConfidence::Ambiguous
                        } else {
                            confidence(evidence)
                        },
                        ambiguity_site: ambiguous.then_some(call_id),
                    },
                    call_id,
                    evidence,
                );
            }
        }
        build_metrics.resolve_relations_ms = resolve_started.elapsed().as_millis();

        let finalize_started = Instant::now();
        let relation_ref_count = builders
            .iter()
            .map(|builder| 1 + builder.additional_call_sites.len())
            .sum();
        catalog.relations.reserve(builders.len());
        catalog.relation_call_sites.reserve(relation_ref_count);
        for builder in builders {
            let start = compact_id(catalog.relation_call_sites.len(), "relation call-site refs");
            let len = compact_id(
                1 + builder.additional_call_sites.len(),
                "call sites per relation",
            );
            catalog.relation_call_sites.push(builder.first_call_site);
            catalog
                .relation_call_sites
                .extend(builder.additional_call_sites);
            catalog.relations.push(CompactRelation {
                key: builder.key,
                call_site_start: start,
                call_site_len: len,
                evidence: builder.evidence,
            });
        }

        for (index, relation) in catalog.relations.iter().enumerate() {
            let relation_id = compact_id(index, "relations");
            catalog
                .outgoing
                .entry(relation.key.caller)
                .or_default()
                .push(relation_id);
            if let Some(callee) = relation.key.callee {
                catalog
                    .incoming
                    .entry(callee)
                    .or_default()
                    .push(relation_id);
            }
        }
        let entities = &catalog.entities;
        let relations = &catalog.relations;
        for relation_ids in catalog
            .outgoing
            .values_mut()
            .chain(catalog.incoming.values_mut())
        {
            relation_ids.sort_by(|left, right| relation_order(entities, relations, *left, *right));
        }
        build_metrics.finalize_ms = finalize_started.elapsed().as_millis();
        catalog.build_metrics = build_metrics;
        catalog
    }

    #[cfg(test)]
    pub fn entity(&self, key: &str) -> Option<&CallableEntity> {
        self.entity_by_key
            .get(key)
            .map(|entity_id| self.entity_by_id(*entity_id))
    }

    pub fn entity_at(&self, path: &str, position: SourcePosition) -> Option<&CallableEntity> {
        self.entities_at(path, position).into_iter().next()
    }

    pub fn entities_at(&self, path: &str, position: SourcePosition) -> Vec<&CallableEntity> {
        let mut call_candidates = Vec::new();
        let mut resolved = Vec::new();
        let call_ids = self
            .strings
            .id(path)
            .and_then(|path_id| self.calls_by_path.get(&path_id));
        for call_id in call_ids.into_iter().flatten() {
            let call = self.call_site(*call_id);
            if position_in_range(position, call.callee_range) {
                self.resolve_call_into(*call_id, &mut resolved);
                call_candidates.extend(
                    resolved
                        .iter()
                        .map(|(entity_id, _)| self.entity_by_id(*entity_id)),
                );
            }
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
            .map(|entity_id| self.entity_by_id(*entity_id))
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
        self.relation_page(
            RelationDirection::Outgoing,
            caller_key,
            0,
            usize::MAX,
            usize::MAX,
        )
        .relations
    }

    pub fn incoming(&self, callee_key: &str) -> Vec<CallRelation> {
        self.relation_page(
            RelationDirection::Incoming,
            callee_key,
            0,
            usize::MAX,
            usize::MAX,
        )
        .relations
    }

    pub fn relation_page(
        &self,
        direction: RelationDirection,
        entity_key: &str,
        cursor: usize,
        relation_limit: usize,
        call_site_limit: usize,
    ) -> RelationPage {
        let Some(entity_id) = self.entity_by_key.get(entity_key).copied() else {
            return RelationPage {
                relations: Vec::new(),
                total: 0,
                site_limited: false,
            };
        };
        let relation_ids = match direction {
            RelationDirection::Outgoing => self.outgoing.get(&entity_id),
            RelationDirection::Incoming => self.incoming.get(&entity_id),
        }
        .map(Vec::as_slice)
        .unwrap_or_default();
        let total = relation_ids.len();
        let mut site_limited = false;
        let relations = relation_ids
            .iter()
            .skip(cursor)
            .take(relation_limit)
            .map(|relation_id| {
                let (relation, limited) =
                    self.materialize_relation(*relation_id, direction, call_site_limit);
                site_limited |= limited;
                relation
            })
            .collect();
        RelationPage {
            relations,
            total,
            site_limited,
        }
    }

    pub fn coverage(&self) -> &CoverageSummary {
        &self.coverage
    }

    pub fn resolve_locator(&self, locator: &CallableLocator) -> Option<&CallableEntity> {
        if let Some(entity_id) = self.entity_by_key.get(&locator.entity_key) {
            return Some(self.entity_by_id(*entity_id));
        }
        self.by_path
            .get(&locator.path)?
            .iter()
            .map(|entity_id| self.entity_by_id(*entity_id))
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

    pub fn stats(&self) -> RelationCatalogStats {
        RelationCatalogStats {
            entities: self.entities.len(),
            call_sites: self.call_sites.len(),
            relations: self.relations.len(),
            relation_call_site_refs: self.relation_call_sites.len(),
        }
    }

    pub fn build_metrics(&self) -> RelationCatalogBuildMetrics {
        self.build_metrics
    }

    fn entity_by_id(&self, entity_id: EntityId) -> &CallableEntity {
        entity(&self.entities, entity_id)
    }

    fn call_site(&self, call_site_id: CallSiteId) -> &StoredCallSite {
        &self.call_sites[call_site_id as usize]
    }

    fn materialize_relation(
        &self,
        relation_id: RelationId,
        direction: RelationDirection,
        call_site_limit: usize,
    ) -> (CallRelation, bool) {
        let relation = &self.relations[relation_id as usize];
        let start = relation.call_site_start as usize;
        let len = relation.call_site_len as usize;
        let end = start + len;
        let take = len.min(call_site_limit);
        let call_sites = self.relation_call_sites[start..end]
            .iter()
            .take(take)
            .map(|call_site_id| self.call_site(*call_site_id).materialize(&self.strings))
            .collect();
        let ambiguity_set_id = relation
            .key
            .ambiguity_site
            .map(|call_site_id| self.call_site(call_site_id).site_fingerprint.to_string());
        (
            CallRelation {
                caller: self.entity_by_id(relation.key.caller).clone(),
                callee: relation
                    .key
                    .callee
                    .map(|callee| self.entity_by_id(callee).clone()),
                direction,
                call_sites,
                confidence: relation.key.confidence,
                evidence: relation.evidence.into_ledger(),
                ambiguity_set_id,
            },
            len > take,
        )
    }

    fn resolve_call_into(
        &self,
        call_site_id: CallSiteId,
        output: &mut Vec<(EntityId, EvidenceBits)>,
    ) {
        output.clear();
        let call = self.call_site(call_site_id);
        if !matches!(
            call.form,
            CallForm::DirectName | CallForm::QualifiedName | CallForm::ParenthesizedName
        ) {
            return;
        }
        let Some(name) = call
            .callee_name
            .map(|callee_name| self.strings.get(callee_name))
        else {
            return;
        };
        let Some(keys) = self.by_name.get(name) else {
            return;
        };
        output.reserve(keys.len());
        let call_path = self.strings.get(call.path);
        for candidate_id in keys {
            let candidate = self.entity_by_id(*candidate_id);
            let mut evidence = EvidenceBits::default();
            if let Some(qualified) = call
                .qualified_name
                .map(|qualified_name| self.strings.get(qualified_name))
            {
                if qualified != candidate.qualified_name.as_str() {
                    continue;
                }
                evidence = evidence.support(EvidenceCode::ExplicitQualifier);
            }
            if candidate.primary_anchor.path.as_str() == call_path {
                evidence = evidence.support(EvidenceCode::SameFile);
            }
            match &candidate.linkage {
                LinkageDomain::Internal(path) if path.as_str() != call_path => continue,
                LinkageDomain::Internal(_) => {
                    evidence = evidence.support(EvidenceCode::InternalLinkage)
                }
                _ => {}
            }
            if let Some(arity) = call.argument_count {
                match candidate.signature.accepts_arity(arity) {
                    Some(true) => evidence = evidence.support(EvidenceCode::CompatibleArity),
                    Some(false) => continue,
                    None => evidence = evidence.unknown(EvidenceCode::CompatibleArity),
                }
            }
            if call.syntax_error_overlap {
                evidence = evidence.unknown(EvidenceCode::SyntaxErrorOverlap);
            }
            if std::path::Path::new(&candidate.primary_anchor.path).is_absolute()
                && candidate
                    .variants
                    .iter()
                    .all(|anchor| anchor.body_range.is_none())
            {
                evidence = evidence.unknown(EvidenceCode::ExternalBodyUnavailable);
            }
            if evidence.supports == 0 {
                evidence = evidence.support(EvidenceCode::NameOnly);
            }
            output.push((*candidate_id, evidence));
        }
    }
}

fn compact_id(index: usize, what: &str) -> u32 {
    u32::try_from(index).unwrap_or_else(|_| panic!("{what} exceed the compact catalog limit"))
}

fn entity(entities: &[CallableEntity], entity_id: EntityId) -> &CallableEntity {
    &entities[entity_id as usize]
}

fn record_relation(
    relation_by_key: &mut HashMap<CompactRelationKey, RelationId>,
    builders: &mut Vec<RelationBuilder>,
    key: CompactRelationKey,
    call_site: CallSiteId,
    evidence: EvidenceBits,
) {
    if let Some(relation_id) = relation_by_key.get(&key) {
        let current = &mut builders[*relation_id as usize];
        current.additional_call_sites.push(call_site);
        current.evidence.merge(evidence);
        return;
    }
    let relation_id = compact_id(builders.len(), "relations");
    relation_by_key.insert(key, relation_id);
    builders.push(RelationBuilder {
        key,
        first_call_site: call_site,
        additional_call_sites: Vec::new(),
        evidence,
    });
}

fn relation_order(
    entities: &[CallableEntity],
    relations: &[CompactRelation],
    left: RelationId,
    right: RelationId,
) -> Ordering {
    let left = &relations[left as usize];
    let right = &relations[right as usize];
    left.key
        .callee
        .map(|callee| &entity(entities, callee).qualified_name)
        .cmp(
            &right
                .key
                .callee
                .map(|callee| &entity(entities, callee).qualified_name),
        )
        .then_with(|| {
            entity(entities, left.key.caller)
                .qualified_name
                .cmp(&entity(entities, right.key.caller).qualified_name)
        })
        .then_with(|| left.key.ambiguity_site.cmp(&right.key.ambiguity_site))
}

fn eligible_free_function(entity: &CallableEntity) -> bool {
    entity.kind == CallableKind::Function
        && entity.owner_kind != Some(OwnerKindHint::Record)
        && !(entity.owner.is_some() && entity.owner_kind == Some(OwnerKindHint::Unknown))
}

fn primary_anchor_cmp(left: &CallableAnchor, right: &CallableAnchor) -> Ordering {
    u8::from(left.role != AnchorRole::Definition)
        .cmp(&u8::from(right.role != AnchorRole::Definition))
        .then_with(|| {
            u8::from(left.provenance != FactProvenance::Ast)
                .cmp(&u8::from(right.provenance != FactProvenance::Ast))
        })
        .then_with(|| left.path.cmp(&right.path))
        .then_with(|| left.name_range.start_byte.cmp(&right.name_range.start_byte))
}

fn position_in_range(position: SourcePosition, range: SourceRange) -> bool {
    (position.line, position.character) >= (range.start.line, range.start.character)
        && (position.line, position.character) <= (range.end.line, range.end.character)
}

fn confidence(evidence: EvidenceBits) -> RelationConfidence {
    if evidence.contains_support(EvidenceCode::ExplicitQualifier)
        || evidence.contains_support(EvidenceCode::InternalLinkage)
    {
        RelationConfidence::High
    } else if evidence.contains_support(EvidenceCode::SameFile)
        || evidence.contains_support(EvidenceCode::CompatibleArity)
    {
        RelationConfidence::Medium
    } else {
        RelationConfidence::Low
    }
}

fn unresolved_evidence(call: &StoredCallSite) -> EvidenceBits {
    if matches!(
        call.form,
        CallForm::DirectName | CallForm::QualifiedName | CallForm::ParenthesizedName
    ) {
        EvidenceBits::default().unknown(EvidenceCode::NameOnly)
    } else {
        EvidenceBits::default().unknown(EvidenceCode::UnsupportedCallForm)
    }
}

pub fn signature_digest(signature: &SignatureShape) -> String {
    blake3::hash(signature.normalized.as_bytes())
        .to_hex()
        .to_string()
}

#[cfg(test)]
mod tests;
