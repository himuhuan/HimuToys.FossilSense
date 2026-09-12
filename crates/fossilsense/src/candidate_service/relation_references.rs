//! Entity-bound references; the legacy text-reference contract stays separate.
#![allow(dead_code)] // Public semantic API consumed by the follow-up relation backend.
use super::relation_facts::*;
use super::relation_types::*;
use super::*;
use crate::query;
use crate::semantic_model::{
    condition_relation, relations::*, ConditionRelation, EntityDomain, EntityIdentity,
};
use crate::store::views::relation_facts::{RelationFact, RelationFactLookup};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingState {
    Resolved,
    Candidate,
    Unresolved,
}
pub struct BoundReference {
    pub path: String,
    pub site: BindingSiteFact,
    pub targets: Vec<RelationTarget>,
    pub state: BindingState,
}
pub struct ReferencePage {
    pub references: Vec<BoundReference>,
    pub coverage: RelationCoverage,
}

impl RelationQueryContext<'_> {
    pub fn relation_references(
        &self,
        target: &RelationTargetHandle,
        incoming: bool,
        cursor: &RelationCursor,
        control: &RelationControl<'_>,
    ) -> Result<ReferencePage> {
        if !self.valid(target) {
            return Ok(ReferencePage {
                references: Vec::new(),
                coverage: RelationCoverage {
                    stale: true,
                    ..Default::default()
                },
            });
        }
        let (name, path) = match &target.target {
            RelationTarget::Entity(id) => (
                id.qualified_name
                    .rsplit("::")
                    .next()
                    .unwrap_or(&id.qualified_name)
                    .to_owned(),
                String::new(),
            ),
            RelationTarget::Member(member) => {
                (member.member.name.clone(), member.member.owner_path.clone())
            }
            RelationTarget::File(path) => (String::new(), path.clone()),
            RelationTarget::Local { .. } => {
                return Ok(ReferencePage {
                    references: Vec::new(),
                    coverage: RelationCoverage {
                        unavailable: true,
                        ..Default::default()
                    },
                })
            }
        };
        let (rows, mut coverage) = if incoming || matches!(target.target, RelationTarget::File(_)) {
            let lookup = if incoming {
                RelationFactLookup::Name(&name)
            } else {
                RelationFactLookup::Path(&path)
            };
            self.service(&path)
                .relation_fact_page(0, lookup, cursor, control)?
        } else {
            let service = self.service(&path);
            let subjects = match &target.target {
                RelationTarget::Entity(id)
                    if id.family == self.family && id.domain == EntityDomain::Callable =>
                {
                    let candidates = service.semantic_candidates_with_policy(
                        &name,
                        SemanticIntent::Call,
                        LookupPolicy::BoundDomain {
                            domain: crate::parser::LookupDomain::Value,
                            qualifier: id.owner.as_deref(),
                        },
                    )?;
                    let matching = candidates
                        .all
                        .into_iter()
                        .flat_map(|g| g.candidates)
                        .filter(|c| EntityIdentity::from_declaration(&c.fact) == *id)
                        .collect();
                    service.entity_subjects(matching)?
                }
                RelationTarget::Member(member) => {
                    service.member_entity_subjects(std::slice::from_ref(member))?
                }
                _ => Vec::new(),
            };
            let locations = service.entity_documentation_locations(&subjects)?;
            let mut keys: Vec<_> = service
                .entity_callable_presentations(&locations)?
                .into_iter()
                .filter(|a| a.anchor.body_range.is_some())
                .map(|a| a.anchor.entity_key)
                .collect();
            keys.sort();
            keys.dedup();
            let mut rows = Vec::new();
            let mut cov = RelationCoverage {
                partial: locations.coverage.incomplete || locations.coverage.truncated,
                unavailable: keys.is_empty(),
                ..Default::default()
            };
            for (index, key) in keys.iter().enumerate().skip(cursor.lookup_offset) {
                let scan_cursor = if index == cursor.lookup_offset {
                    cursor.clone()
                } else {
                    RelationCursor::default()
                };
                let (next, page) = service.relation_fact_page(
                    0,
                    RelationFactLookup::Caller(key),
                    &scan_cursor,
                    &RelationControl {
                        scan_limit: control.scan_limit.saturating_sub(cov.scanned),
                        cancellation: control.cancellation,
                    },
                )?;
                cov.scanned += page.scanned;
                cov.partial |= page.partial;
                cov.unavailable |= page.unavailable;
                cov.cancelled |= page.cancelled;
                rows.extend(next);
                if let Some(mut next) = page.next {
                    next.lookup_offset = index;
                    cov.next = Some(next);
                    break;
                }
                if cov.scanned >= control.scan_limit || cov.cancelled {
                    if index + 1 < keys.len() {
                        cov.next = Some(RelationCursor {
                            lookup_offset: index + 1,
                            ..Default::default()
                        });
                    }
                    break;
                }
            }
            (rows, cov)
        };
        let mut references = Vec::new();
        for row in rows {
            if control.cancelled() {
                coverage.cancelled = true;
                references.clear();
                break;
            }
            let RelationFact::Binding(site) = row.fact else {
                continue;
            };
            // Global queries never reinterpret a lexically bound local as a
            // same-spelled global. Local queries use the captured parser below.
            if site.local_anchor.is_some() && site.receiver.is_none() {
                continue;
            }
            let (targets, state) = self.bind_reference(&row.path, &site, control)?;
            if incoming
                && !targets.is_empty()
                && !targets
                    .iter()
                    .any(|candidate| same_target(candidate, &target.target))
            {
                continue;
            }
            if targets.is_empty() {
                coverage.partial = true;
            }
            references.push(BoundReference {
                path: row.path,
                site,
                targets,
                state,
            });
        }
        Ok(ReferencePage {
            references,
            coverage,
        })
    }

    fn bind_reference(
        &self,
        path: &str,
        site: &BindingSiteFact,
        control: &RelationControl<'_>,
    ) -> Result<(Vec<RelationTarget>, BindingState)> {
        if site.member_access && site.receiver.is_none() {
            return Ok((Vec::new(), BindingState::Unresolved));
        }
        if let Some(receiver) = &site.receiver {
            let (members, coverage) = self.members(path, receiver, &site.spelling, control)?;
            let state = if members.is_empty() {
                BindingState::Unresolved
            } else if members.len() == 1 && !coverage.partial && !coverage.unavailable {
                BindingState::Resolved
            } else {
                BindingState::Candidate
            };
            return Ok((
                members.into_iter().map(RelationTarget::Member).collect(),
                state,
            ));
        }
        let (domain, intent) = match site.domain {
            EntityDomain::Callable => (crate::parser::LookupDomain::Value, SemanticIntent::Call),
            EntityDomain::Macro => (crate::parser::LookupDomain::Value, SemanticIntent::Neutral),
            EntityDomain::Tag => (crate::parser::LookupDomain::Tag, SemanticIntent::Type),
            EntityDomain::Alias => (crate::parser::LookupDomain::Type, SemanticIntent::Type),
            EntityDomain::Value => (crate::parser::LookupDomain::Value, SemanticIntent::Value),
        };
        let set = self.service(path).semantic_candidates_with_policy(
            &site.spelling,
            intent,
            LookupPolicy::BoundDomain {
                domain: if site.qualifier.is_some() {
                    if site.role == ReferenceRole::TypeUse {
                        crate::parser::LookupDomain::QualifiedType
                    } else {
                        crate::parser::LookupDomain::QualifiedValue
                    }
                } else {
                    domain
                },
                qualifier: site.qualifier.as_deref(),
            },
        )?;
        let mut targets = Vec::new();
        for candidate in focused_candidates(&set) {
            if condition_relation(
                site.source.guard.as_deref(),
                candidate.fact.guard.as_deref(),
            ) == ConditionRelation::Incompatible
            {
                continue;
            }
            let id = EntityIdentity::from_declaration(&candidate.fact);
            if !targets
                .iter()
                .any(|t| matches!(t,RelationTarget::Entity(existing) if *existing==id))
            {
                targets.push(RelationTarget::Entity(id));
            }
        }
        let state = if targets.is_empty() {
            BindingState::Unresolved
        } else if targets.len() == 1
            && set.disposition == crate::model::CandidateDisposition::Exact
            && site.source.guard.is_none()
        {
            BindingState::Resolved
        } else {
            BindingState::Candidate
        };
        Ok((targets, state))
    }

    pub fn local_references(
        &self,
        target: &RelationTargetHandle,
        path: &str,
        version: i32,
        index: &FileSemanticIndex,
        cursor: usize,
        control: &RelationControl<'_>,
    ) -> ReferencePage {
        let stale = || ReferencePage {
            references: Vec::new(),
            coverage: RelationCoverage {
                stale: true,
                ..Default::default()
            },
        };
        let RelationTarget::Local {
            path: target_path,
            binding,
            source_fingerprint,
        } = &target.target
        else {
            return stale();
        };
        if !self.valid(target)
            || target_path != path
            || binding.document_version != version
            || *source_fingerprint != index.source_fingerprint
        {
            return stale();
        }
        let Some(selected) = index.local_bindings.get(binding.binding_index) else {
            return stale();
        };
        if selected.decl_start_byte != binding.declaration_byte {
            return stale();
        }
        let mut references = Vec::new();
        let mut coverage = RelationCoverage::default();
        for site in index
            .relations
            .binding_sites
            .iter()
            .skip(cursor)
            .take(control.scan_limit.clamp(1, 256))
        {
            coverage.scanned += 1;
            if control.cancelled() {
                coverage.cancelled = true;
                references.clear();
                break;
            }
            if site.spelling != selected.name {
                continue;
            }
            let Some(syntax) = index.cursor.at(site.source.range.start_byte) else {
                coverage.partial = true;
                continue;
            };
            match query::resolve_local_cursor(
                index,
                syntax,
                &site.spelling,
                site.source.range.start_byte,
                version,
            ) {
                query::BindingResolution::Resolved(found) if found == *binding => {
                    references.push(BoundReference {
                        path: path.to_owned(),
                        site: site.clone(),
                        targets: vec![target.target.clone()],
                        state: BindingState::Resolved,
                    })
                }
                query::BindingResolution::Unsupported { .. } => coverage.partial = true,
                _ => {}
            }
        }
        if cursor + coverage.scanned < index.relations.binding_sites.len() {
            coverage.next = Some(RelationCursor {
                overlay_offset: cursor + coverage.scanned,
                ..Default::default()
            });
        }
        ReferencePage {
            references,
            coverage,
        }
    }
}

fn same_target(left: &RelationTarget, right: &RelationTarget) -> bool {
    match (left, right) {
        (RelationTarget::Entity(a), RelationTarget::Entity(b)) => a == b,
        (RelationTarget::Member(a), RelationTarget::Member(b)) => {
            a.generation == b.generation
                && a.family == b.family
                && a.owner == b.owner
                && a.member.handle == b.member.handle
                && a.member.owner_path == b.member.owner_path
        }
        (RelationTarget::File(a), RelationTarget::File(b)) => a == b,
        _ => false,
    }
}
