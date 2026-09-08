//! Bounded occurrence expansion after the cursor has selected its subjects.
//! Identity keys retrieve candidates; only direct evidence relates occurrences.
use super::semantic::{focused_candidates, ResolvedDeclarationCandidate};
use super::{
    CandidateOrigin, CandidateQueryService, CandidateRevision, LookupPolicy, SemanticIntent,
};
use crate::model::{CandidateSet, DefinitionCandidate};
use crate::semantic_model::{
    declaration_relation, EntityIdentity, RelationEvidence, RelationStrength,
    SemanticDeclarationRole as Role,
};
use crate::store::views::DeclarationReadRow;
use anyhow::Result;
use std::collections::HashSet;

pub const ENTITY_LIMIT: usize = 64;
pub const EDGE_LIMIT: usize = 1024;
pub const LOCATION_LIMIT: usize = 256;

#[derive(Debug, Clone)]
pub struct EntitySubject {
    pub candidate: ResolvedDeclarationCandidate,
    identity: EntityIdentity,
    incarnation: Option<String>,
    database_path: Option<std::path::PathBuf>,
    generation: Option<u64>,
    overlay_epoch: u64,
    subjects_truncated: bool,
}
#[derive(Debug, Default)]
pub struct EntityCoverage {
    pub entities: usize,
    pub edges: usize,
    pub truncated: bool,
    pub incomplete: bool,
    pub stale: bool,
    pub condition_ambiguous: bool,
}
#[derive(Debug)]
pub struct EntityLocation {
    pub candidate: ResolvedDeclarationCandidate,
    pub evidence: RelationEvidence,
}
#[derive(Debug, Default)]
pub struct EntityLocations {
    pub locations: Vec<EntityLocation>,
    pub coverage: EntityCoverage,
}
impl EntityLocations {
    pub fn diagnostic(&self) -> &'static str {
        if self.coverage.stale {
            "stale"
        } else if self.coverage.truncated {
            "candidate_truncated"
        } else if self.coverage.incomplete {
            "relations_incomplete"
        } else if self.coverage.condition_ambiguous {
            "conditional_candidates"
        } else if self
            .locations
            .iter()
            .any(|v| v.candidate.fact.role == Role::Definition)
        {
            if self
                .locations
                .iter()
                .any(|v| v.evidence.strength != RelationStrength::Proven)
            {
                "definition_candidates"
            } else {
                "definition_found"
            }
        } else if self.locations.is_empty() {
            "unsupported_or_no_subject"
        } else {
            "indexed_declarations_only"
        }
    }
    pub fn candidates(&self) -> Vec<DefinitionCandidate> {
        self.locations
            .iter()
            .map(|item| item.candidate.as_definition_candidate())
            .collect()
    }
}
impl CandidateQueryService<'_> {
    pub fn entity_callable_presentations(
        &self,
        related: &EntityLocations,
    ) -> Result<Vec<crate::query::ResolvedCallableAnchor>> {
        let ids: Vec<_> = related
            .locations
            .iter()
            .filter_map(|item| item.candidate.backing_id)
            .take(LOCATION_LIMIT)
            .collect();
        let rows = self
            .handle
            .map(|handle| {
                handle.read(|store| {
                    store
                        .call_fact_view()
                        .anchors_by_ids_family(&ids, self.semantic_family)
                })
            })
            .transpose()?
            .unwrap_or_default();
        let mut by_id: std::collections::HashMap<_, _> =
            rows.into_iter().map(|row| (row.id, row)).collect();
        let mut output = Vec::new();
        let mut overlay_remaining = EDGE_LIMIT;
        for item in &related.locations {
            let candidate = &item.candidate;
            let anchor = if let Some(id) = candidate.backing_id {
                by_id
                    .remove(&id)
                    .map(crate::call_catalog::rows::anchor_from_row)
            } else {
                let anchors = self.overlays.callable_by_name.get(&candidate.fact.name);
                let mut found = None;
                for anchor in anchors.into_iter().flatten().take(overlay_remaining) {
                    overlay_remaining -= 1;
                    if anchor.path == candidate.fact.path
                        && anchor.anchor_fingerprint == candidate.fact.identity.locator.fingerprint
                    {
                        found = Some(anchor.clone());
                        break;
                    }
                }
                found
            };
            if let Some(anchor) = anchor.filter(|anchor| {
                anchor.path == candidate.fact.path
                    && anchor.anchor_fingerprint == candidate.fact.identity.locator.fingerprint
            }) {
                output.push(crate::query::ResolvedCallableAnchor::new(
                    anchor,
                    candidate.as_definition_candidate(),
                    candidate.origin,
                ));
            }
        }
        Ok(output)
    }

    pub fn member_entity_subjects(
        &self,
        members: &[super::member_resolution::OwnerMemberRef],
    ) -> Result<Vec<EntitySubject>> {
        let mut candidates = Vec::new();
        for selected in members.iter().take(ENTITY_LIMIT) {
            let member = &selected.member;
            if member.kind != crate::parser::MemberKind::Method
                || selected.family != self.semantic_family
                || self
                    .handle
                    .is_some_and(|handle| handle.generation != selected.generation)
            {
                continue;
            }
            let start = crate::call_model::SourcePosition {
                line: member.handle.start_line,
                character: member.handle.start_col,
            };
            let end = crate::call_model::SourcePosition {
                line: member.handle.end_line,
                character: member.handle.end_col,
            };
            if self.overlays.shadows(&member.owner_path) {
                for entry in self
                    .overlays
                    .declarations(&member.name)
                    .iter()
                    .take(EDGE_LIMIT)
                {
                    if entry.path == member.owner_path
                        && entry.fact.name_range.start == start
                        && entry.fact.name_range.end == end
                    {
                        candidates.push(self.candidate_from_overlay(entry));
                    }
                }
            } else if let Some(handle) = self.handle {
                let rows = handle.read(|store| {
                    store
                        .declaration_view()
                        .at_name_range(&member.owner_path, start, end)
                })?;
                candidates.extend(
                    rows.into_iter()
                        .filter(|row| {
                            Some(row.revision_hash.as_str())
                                == member.owner_revision_hash.as_deref()
                        })
                        .map(|row| self.candidate_from_row(row)),
                );
            }
        }
        self.entity_subjects(candidates)
    }
    pub(super) fn candidate_from_overlay(
        &self,
        entry: &super::OverlayDeclarationFact,
    ) -> ResolvedDeclarationCandidate {
        let (external, directly_included) = self.path_evidence(
            &entry.path,
            std::path::Path::new(&entry.path).is_absolute(),
            false,
        );
        let context = crate::resolver::ResolveContext {
            current_path: Some(self.current_path),
            reach: self.current_reach.as_deref(),
            direct_external_files: None,
        };
        let tier =
            crate::resolver::scope_tier(&entry.path, external, directly_included, Some(&context));
        let (confidence, reason) = crate::resolver::confidence_reason_for(
            tier,
            true,
            self.current_reach.as_ref().and_then(|reach| reach.reason),
        );
        ResolvedDeclarationCandidate {
            persistent_id: None,
            fact: entry.fact.clone(),
            backing_kind: "overlay".into(),
            backing_id: None,
            tier,
            confidence,
            reason,
            origin: CandidateOrigin::Overlay,
            external,
            directly_included,
            revision: None,
        }
    }

    pub fn resolve_subject(
        &self,
        name: &str,
        intent: SemanticIntent,
        policy: LookupPolicy<'_>,
        call_context: Option<crate::query::CallSiteContext>,
    ) -> Result<(
        CandidateSet<ResolvedDeclarationCandidate>,
        Vec<EntitySubject>,
    )> {
        let set = self.semantic_candidates_with_policy(name, intent, policy)?;
        let mut candidates = focused_candidates(&set);
        if call_context.is_some()
            && candidates.iter().any(|c| {
                matches!(
                    c.fact.declaration_kind,
                    crate::semantic_model::SemanticDeclarationKind::Function
                        | crate::semantic_model::SemanticDeclarationKind::Method
                )
            })
        {
            let callables = self.callable_subject_candidates(name, call_context)?;
            if !callables.anchors.is_empty() {
                let fingerprints: HashSet<_> = callables
                    .anchors
                    .iter()
                    .map(|c| c.anchor.anchor_fingerprint.as_str())
                    .collect();
                candidates.retain(|c| {
                    fingerprints.contains(c.fact.identity.locator.fingerprint.as_str())
                });
            }
        }
        let subjects = self.entity_subjects(candidates.into_iter().cloned().collect())?;
        Ok((set, subjects))
    }
    pub fn entity_subjects(
        &self,
        candidates: Vec<ResolvedDeclarationCandidate>,
    ) -> Result<Vec<EntitySubject>> {
        let incarnation = self
            .handle
            .map(|handle| {
                handle.read(|store| {
                    let view = store.entity_view();
                    for candidate in candidates.iter().take(ENTITY_LIMIT) {
                        if let Some(id) = candidate.persistent_id {
                            anyhow::ensure!(
                                view.identity_for_declaration(id)?
                                    == Some(EntityIdentity::digest_for_declaration(
                                        &candidate.fact
                                    )),
                                "stale entity occurrence identity"
                            );
                        }
                    }
                    view.incarnation()
                })
            })
            .transpose()?;
        let subjects_truncated = candidates.len() > ENTITY_LIMIT;
        Ok(candidates
            .into_iter()
            .take(ENTITY_LIMIT)
            .map(|candidate| EntitySubject {
                identity: EntityIdentity::from_declaration(&candidate.fact),
                candidate,
                incarnation: incarnation.clone(),
                database_path: self
                    .handle
                    .map(|handle| handle.database_path().to_path_buf()),
                generation: self.handle.map(|handle| handle.generation.0),
                overlay_epoch: self.overlays.epoch,
                subjects_truncated,
            })
            .collect())
    }
    pub(super) fn candidate_from_row(
        &self,
        row: DeclarationReadRow,
    ) -> ResolvedDeclarationCandidate {
        let (external, directly_included) =
            self.path_evidence(&row.fact.path, row.external, row.directly_included);
        let context = crate::resolver::ResolveContext {
            current_path: Some(self.current_path),
            reach: self.current_reach.as_deref(),
            direct_external_files: None,
        };
        let tier = crate::resolver::scope_tier(
            &row.fact.path,
            external,
            directly_included,
            Some(&context),
        );
        let (confidence, reason) = crate::resolver::confidence_reason_for(
            tier,
            true,
            self.current_reach.as_ref().and_then(|reach| reach.reason),
        );
        ResolvedDeclarationCandidate {
            persistent_id: Some(row.id),
            fact: row.fact,
            backing_kind: row.backing_kind,
            backing_id: row.backing_id,
            tier,
            confidence,
            reason,
            origin: CandidateOrigin::Base,
            external,
            directly_included,
            revision: Some(CandidateRevision {
                id: row.revision_id,
                size: row.revision_size,
                mtime_ns: row.revision_mtime_ns,
                hash: row.revision_hash,
            }),
        }
    }
    #[cfg(test)]
    pub fn entity_locations(
        &self,
        subjects: &[EntitySubject],
        declaration: bool,
    ) -> Result<EntityLocations> {
        self.entity_locations_at(subjects, declaration, None)
    }
    pub fn entity_documentation_locations(
        &self,
        subjects: &[EntitySubject],
    ) -> Result<EntityLocations> {
        self.entity_locations_with_roles(subjects, true, None, true)
    }
    pub fn entity_locations_at(
        &self,
        subjects: &[EntitySubject],
        declaration: bool,
        position: Option<crate::call_model::SourcePosition>,
    ) -> Result<EntityLocations> {
        self.entity_locations_with_roles(subjects, declaration, position, false)
    }
    fn entity_locations_with_roles(
        &self,
        subjects: &[EntitySubject],
        declaration: bool,
        position: Option<crate::call_model::SourcePosition>,
        all_roles: bool,
    ) -> Result<EntityLocations> {
        let mut result = EntityLocations::default();
        result.coverage.incomplete = self.overlays.incomplete_facts_reason().is_some();
        result.coverage.truncated =
            subjects.len() > ENTITY_LIMIT || subjects.iter().any(|s| s.subjects_truncated);
        let incarnation = self
            .handle
            .map(|handle| handle.read(|store| store.entity_view().incarnation()))
            .transpose()?;
        let mut identities = HashSet::new();
        let mut seen = HashSet::new();
        for subject in subjects.iter().take(ENTITY_LIMIT) {
            if subject.database_path.as_deref() != self.handle.map(|handle| handle.database_path())
                || subject.incarnation != incarnation
                || subject.generation != self.handle.map(|h| h.generation.0)
                || subject.overlay_epoch != self.overlays.epoch
            {
                result.coverage.stale = true;
                continue;
            }
            // Multiple condition variants remain separate subjects. No edges
            // discovered below become new roots of the relation traversal.
            let own_occurrence = matches!(
                subject.identity.domain,
                crate::semantic_model::EntityDomain::Alias
                    | crate::semantic_model::EntityDomain::Macro
            );
            let subject_key = (
                &subject.identity,
                &subject.candidate.fact.guard,
                own_occurrence
                    .then_some(subject.candidate.fact.identity.locator.fingerprint.as_str()),
            );
            if !identities.insert(subject_key) {
                continue;
            }
            result.coverage.entities += 1;
            if matches!(
                subject.identity.domain,
                crate::semantic_model::EntityDomain::Alias
                    | crate::semantic_model::EntityDomain::Macro
            ) {
                let fact = &subject.candidate.fact;
                if result.locations.len() < LOCATION_LIMIT {
                    result.locations.push(EntityLocation {
                        candidate: subject.candidate.clone(),
                        evidence: declaration_relation(fact, fact, true),
                    });
                } else {
                    result.coverage.truncated = true;
                }
                continue;
            }
            let roles: &[Role] = if declaration {
                &[
                    Role::Declaration,
                    Role::Definition,
                    Role::TentativeDefinition,
                ]
            } else {
                &[
                    Role::Definition,
                    Role::TentativeDefinition,
                    Role::Declaration,
                ]
            };
            let mut selected = Vec::new();
            for &role in roles {
                let mut candidates = Vec::new();
                let remaining = EDGE_LIMIT.saturating_sub(result.coverage.edges);
                if remaining == 0 {
                    result.coverage.truncated = true;
                    break;
                }
                // Dirty facts are direct identity-index hits, never a global
                // name scan. Fingerprints point back to the captured payload.
                if let Some(fingerprints) = self
                    .overlays
                    .declaration_entity_fingerprints
                    .get(&subject.identity.digest())
                {
                    for fingerprint in fingerprints.iter().take(remaining) {
                        result.coverage.edges += 1;
                        if let Some(entry) = self.overlays.declaration_by_fingerprint(fingerprint) {
                            if entry.fact.role == role {
                                candidates.push(self.candidate_from_overlay(entry));
                            }
                        }
                    }
                    result.coverage.truncated |= fingerprints.len() > remaining;
                }
                if let Some(handle) = self.handle {
                    let mut after = 0;
                    loop {
                        let limit = (EDGE_LIMIT.saturating_sub(result.coverage.edges)).min(256);
                        if limit == 0 {
                            result.coverage.truncated = true;
                            break;
                        }
                        let (page, rows) = handle.read(|store| {
                            if store.entity_view().incarnation()?
                                != incarnation.as_deref().unwrap_or("")
                            {
                                anyhow::bail!("entity database incarnation changed");
                            }
                            let page = store.entity_view().occurrences(
                                &subject.identity,
                                role,
                                after,
                                limit,
                            )?;
                            let ids: Vec<_> =
                                page.rows.iter().map(|row| row.declaration_id).collect();
                            let rows = store.declaration_view().by_ids(&ids)?;
                            Ok((page, rows))
                        })?;
                        result.coverage.edges += page.rows.len();
                        candidates.extend(
                            rows.into_iter()
                                .filter(|row| !self.overlays.shadows(&row.fact.path))
                                .map(|row| self.candidate_from_row(row)),
                        );
                        if !page.truncated {
                            break;
                        }
                        let Some(next) = page.after_id else {
                            break;
                        };
                        after = next;
                        if candidates.len() >= LOCATION_LIMIT {
                            result.coverage.truncated = true;
                            break;
                        }
                    }
                }
                for candidate in candidates {
                    let left = &subject.candidate.fact;
                    let right = &candidate.fact;
                    if declaration
                        && right.path == self.current_path
                        && position.is_some_and(|p| {
                            (
                                right.name_range.start.line,
                                right.name_range.start.character,
                            ) > (p.line, p.character)
                        })
                    {
                        continue;
                    }
                    let reachable = left.path == right.path
                        || self.reach_graph.is_some_and(|graph| {
                            graph.reachable(&right.path).files.contains(&left.path)
                                || graph.reachable(&left.path).files.contains(&right.path)
                        });
                    let evidence = declaration_relation(left, right, reachable);
                    if evidence.strength == RelationStrength::Incompatible {
                        continue;
                    }
                    result.coverage.incomplete |= !evidence.complete;
                    result.coverage.condition_ambiguous |= matches!(
                        evidence.condition,
                        crate::semantic_model::ConditionRelation::Unknown
                            | crate::semantic_model::ConditionRelation::Unconditional
                    );
                    selected.push(EntityLocation {
                        candidate,
                        evidence,
                    });
                }
                if !all_roles && !selected.is_empty() {
                    break;
                }
            }
            if selected.is_empty()
                && !result.coverage.stale
                && !(declaration
                    && subject.candidate.fact.path == self.current_path
                    && position.is_some_and(|p| {
                        (
                            subject.candidate.fact.name_range.start.line,
                            subject.candidate.fact.name_range.start.character,
                        ) > (p.line, p.character)
                    }))
            {
                let fact = &subject.candidate.fact;
                selected.push(EntityLocation {
                    candidate: subject.candidate.clone(),
                    evidence: declaration_relation(fact, fact, true),
                });
            }
            for location in selected {
                let key = (
                    location.candidate.fact.path.clone(),
                    location.candidate.fact.name_range,
                );
                if seen.insert(key) {
                    if result.locations.len() == LOCATION_LIMIT {
                        result.coverage.truncated = true;
                        break;
                    }
                    result.locations.push(location);
                }
            }
        }
        result.locations.sort_by(|a, b| {
            crate::resolver::compare_candidates(
                &a.candidate.as_definition_candidate(),
                &b.candidate.as_definition_candidate(),
                Some(self.current_path),
            )
        });
        Ok(result)
    }
}
