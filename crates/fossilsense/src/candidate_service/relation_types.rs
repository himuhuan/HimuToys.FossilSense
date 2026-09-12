//! Protocol-neutral relation context; all semantic reads share captured identities.
#![allow(dead_code)] // Public semantic API consumed by the follow-up relation backend.
use super::member_resolution::*;
use super::relation_facts::*;
use super::*;
use crate::query;
use crate::semantic_model::{relations::*, EntityIdentity};
use crate::store::views::relation_facts::{RelationFact, RelationFactLookup};
use std::collections::VecDeque;
use std::path::PathBuf;

#[derive(Clone)]
pub enum RelationTarget {
    Entity(EntityIdentity),
    Member(OwnerMemberRef),
    Local {
        path: String,
        binding: query::LocalBindingRef,
        source_fingerprint: [u8; 32],
    },
    File(String),
}

#[derive(Clone)]
pub struct RelationTargetHandle {
    pub target: RelationTarget,
    identity: Option<crate::declaration_read_handle::DeclarationReadIdentity>,
    overlay_epoch: u64,
}

pub struct RelationQueryContext<'a> {
    pub read: Option<Arc<DeclarationReadContext>>,
    pub overlays: Arc<CandidateOverlaySnapshot>,
    pub reach: Option<&'a ReachGraph>,
    pub family: SemanticFamily,
}
impl RelationQueryContext<'_> {
    pub fn service<'a>(&'a self, path: &'a str) -> CandidateQueryService<'a> {
        CandidateQueryService::new_for_family(
            self.read.as_deref(),
            &self.overlays,
            path,
            None,
            self.reach,
            self.family,
        )
    }
    pub fn target(&self, target: RelationTarget) -> RelationTargetHandle {
        RelationTargetHandle {
            target,
            identity: self.read.as_ref().map(|r| r.handle().identity().clone()),
            overlay_epoch: self.overlays.epoch,
        }
    }
    pub fn valid(&self, target: &RelationTargetHandle) -> bool {
        target.overlay_epoch == self.overlays.epoch
            && target.identity.as_ref() == self.read.as_ref().map(|r| r.handle().identity())
            && match &target.target {
                RelationTarget::Entity(id) => id.family == self.family,
                RelationTarget::Member(member) => {
                    member.family == self.family
                        && member.generation
                            == self
                                .read
                                .as_ref()
                                .map(|r| r.handle().generation)
                                .unwrap_or(crate::call_model::SemanticGeneration(0))
                }
                _ => true,
            }
    }
    pub fn members(
        &self,
        path: &str,
        receiver: &ReceiverFact,
        selector: &str,
        control: &RelationControl<'_>,
    ) -> Result<(Vec<OwnerMemberRef>, RelationCoverage)> {
        let mut coverage = RelationCoverage::default();
        if control.cancelled() {
            coverage.cancelled = true;
            return Ok((Vec::new(), coverage));
        }
        let Some(ty) = receiver.type_name.as_deref() else {
            coverage.partial = true;
            return Ok((Vec::new(), coverage));
        };
        if self.family != SemanticFamily::CFamily {
            coverage.unavailable = true;
            return Ok((Vec::new(), coverage));
        }
        let root = PathBuf::new();
        let roots = vec![root.clone()];
        let contexts = HashMap::from([(
            root.clone(),
            MemberRootQueryContext {
                declaration_read: self.read.clone(),
                overlay: self.overlays.clone(),
                current_path: path.to_owned(),
                reach_graph: None,
                current_reach: self.reach.map(|g| g.reachable(path)),
                semantic_generation: self
                    .read
                    .as_ref()
                    .map(|r| r.handle().generation)
                    .unwrap_or(crate::call_model::SemanticGeneration(0)),
                semantic_family: self.family,
            },
        )]);
        let mut resolver =
            MemberResolutionService::new(&roots, &contexts, MemberPolicy::BoundNavigation);
        let ty = clean_type(ty);
        let owners = resolver.resolve_owner_spelling(&ty, receiver.tag_domain)?;
        coverage.partial |= owners.incomplete || owners.budget_exhausted || owners.ambiguous;
        let mut queue: VecDeque<_> = owners
            .candidates
            .into_iter()
            .flat_map(|(_, rs)| rs)
            .collect();
        let mut visited = HashSet::new();
        let mut result = Vec::new();
        let mut base_remaining = control.scan_limit.min(256);
        while let Some(record) = queue.pop_front() {
            if control.cancelled() {
                coverage.cancelled = true;
                result.clear();
                break;
            }
            if !visited.insert(record.identity.clone()) {
                coverage.cycle = true;
                coverage.partial = true;
                continue;
            }
            if visited.len() > OWNER_VISIT_LIMIT {
                coverage.partial = true;
                break;
            }
            match resolver.resolve_selected(
                vec![(root.clone(), vec![record.clone()])],
                &receiver.chain,
                selector,
            )? {
                query::BindingResolution::Resolved(member) => {
                    result.push(member);
                    continue;
                }
                query::BindingResolution::Ambiguous(members) => {
                    result.extend(members);
                    coverage.partial = true;
                    continue;
                }
                query::BindingResolution::Unsupported { .. } => {
                    coverage.partial = true;
                    break;
                }
                _ => {}
            }
            if base_remaining == 0 {
                coverage.partial = true;
                break;
            }
            let (facts, cov) = self.service(&record.path).relation_fact_page(
                1,
                RelationFactLookup::Name(&record.display_name),
                &RelationCursor::default(),
                &RelationControl {
                    scan_limit: base_remaining,
                    cancellation: control.cancellation,
                },
            )?;
            base_remaining = base_remaining.saturating_sub(cov.scanned);
            coverage.scanned += cov.scanned;
            coverage.partial |= cov.next.is_some() || cov.partial;
            coverage.unavailable |= cov.unavailable;
            for row in facts {
                let RelationFact::Base(base) = row.fact else {
                    continue;
                };
                if row.path != record.path
                    || base.source.range.start_byte < record.declaration_range.start_byte
                    || base.source.range.end_byte > record.declaration_range.end_byte
                {
                    continue;
                }
                if base.dependent {
                    coverage.partial = true;
                    continue;
                }
                let next = resolver.resolve_owner_spelling(&base.base_name, false)?;
                coverage.partial |=
                    next.budget_exhausted || next.ambiguous || base.source.guard.is_some();
                queue.extend(next.candidates.into_iter().flat_map(|(_, rs)| rs));
            }
        }
        coverage.scanned += resolver.consumed_work();
        Ok((result, coverage))
    }
}

pub(super) fn clean_type(spelling: &str) -> String {
    spelling
        .split_whitespace()
        .filter(|s| !matches!(*s, "const" | "volatile" | "static" | "extern" | "register"))
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches(['*', '&', ' '])
        .to_owned()
}

pub struct InheritanceEdge {
    pub derived: query::RecordCandidate,
    pub base: Option<query::RecordCandidate>,
    pub fact: ExplicitBaseFact,
}
pub struct InheritancePage {
    pub edges: Vec<InheritanceEdge>,
    pub coverage: RelationCoverage,
}
impl RelationQueryContext<'_> {
    pub fn reverse_alias_names(
        &self,
        record: &query::RecordCandidate,
        control: &RelationControl<'_>,
    ) -> Result<(Vec<String>, bool)> {
        let mut names = vec![record.display_name.clone()];
        let mut index = 0;
        let mut partial = false;
        while index < names.len() && index < 64 {
            if control.cancelled() {
                return Ok((names, true));
            }
            let name = names[index].clone();
            index += 1;
            let record_id = if index == 1 {
                if let query::RecordCandidateIdentity::Persistent(id) = record.identity {
                    Some(id)
                } else {
                    None
                }
            } else {
                None
            };
            let rows = self
                .read
                .as_ref()
                .map(|r| {
                    r.read(|store| {
                        store.member_view().alias_names_for_target(
                            &name,
                            record_id,
                            self.family,
                            64 - names.len(),
                        )
                    })
                })
                .transpose()?
                .unwrap_or_default();
            partial |= rows.1;
            let mut aliases: Vec<_> = rows
                .0
                .into_iter()
                .filter(|(_, path)| !self.overlays.shadows(path))
                .map(|(name, _)| name)
                .collect();
            aliases.extend(
                self.overlays
                    .alias_by_target
                    .get(&name)
                    .into_iter()
                    .flatten()
                    .filter(|f| {
                        self.overlays.semantic_family_for_path(&f.path) == Some(self.family)
                    })
                    .take(65)
                    .map(|f| f.alias.alias.clone()),
            );
            for alias in aliases {
                if names.contains(&alias) {
                    continue;
                }
                if names.len() == 64 {
                    partial = true;
                    break;
                }
                names.push(alias);
            }
        }
        Ok((names, partial))
    }
    pub fn inheritance(
        &self,
        record: &query::RecordCandidate,
        incoming: bool,
        cursor: &RelationCursor,
        control: &RelationControl<'_>,
    ) -> Result<InheritancePage> {
        let current = match &record.identity {
            query::RecordCandidateIdentity::Persistent(id) => self
                .read
                .as_ref()
                .map(|read| read.read(|store| store.member_view().record_row_by_id(*id)))
                .transpose()?
                .flatten()
                .is_some_and(|row| {
                    row.path == record.path
                        && row.declaration_hash == record.declaration_hash
                        && !self.overlays.shadows(&record.path)
                        && Some(row.revision_hash.as_str())
                            == record.revision.as_ref().map(|r| r.hash.as_str())
                }),
            query::RecordCandidateIdentity::ParserKey { path, record_key } => self
                .overlays
                .record_by_key
                .get(&(path.clone(), record_key.clone()))
                .is_some_and(|r| r.record.declaration_hash == record.declaration_hash),
        };
        if !current {
            return Ok(InheritancePage {
                edges: Vec::new(),
                coverage: RelationCoverage {
                    stale: true,
                    ..Default::default()
                },
            });
        }
        if self.family != SemanticFamily::CFamily {
            return Ok(InheritancePage {
                edges: Vec::new(),
                coverage: RelationCoverage {
                    unavailable: true,
                    ..Default::default()
                },
            });
        }
        let service = self.service(&record.path);
        let (names, alias_partial) = if incoming {
            self.reverse_alias_names(record, control)?
        } else {
            (vec![record.display_name.clone()], false)
        };
        let mut facts = Vec::new();
        let mut coverage = RelationCoverage {
            partial: alias_partial,
            ..Default::default()
        };
        for (index, name) in names.iter().enumerate().skip(cursor.lookup_offset) {
            let lookup = if incoming {
                RelationFactLookup::Target(name)
            } else {
                RelationFactLookup::Name(name)
            };
            let scan_cursor = if index == cursor.lookup_offset {
                cursor.clone()
            } else {
                RelationCursor::default()
            };
            let (rows, cov) = service.relation_fact_page(
                1,
                lookup,
                &scan_cursor,
                &RelationControl {
                    scan_limit: control.scan_limit.saturating_sub(coverage.scanned),
                    cancellation: control.cancellation,
                },
            )?;
            coverage.scanned += cov.scanned;
            coverage.partial |= cov.partial;
            coverage.unavailable |= cov.unavailable;
            coverage.cancelled |= cov.cancelled;
            facts.extend(rows);
            if let Some(mut next) = cov.next {
                next.lookup_offset = index;
                coverage.next = Some(next);
                break;
            }
            if coverage.scanned >= control.scan_limit || coverage.cancelled {
                if index + 1 < names.len() {
                    coverage.next = Some(RelationCursor {
                        lookup_offset: index + 1,
                        ..Default::default()
                    });
                }
                break;
            }
        }
        let mut edges = Vec::new();
        let mut budget = 64;
        for row in facts {
            if control.cancelled() {
                coverage.cancelled = true;
                edges.clear();
                break;
            }
            let RelationFact::Base(fact) = row.fact else {
                continue;
            };
            let service = self.service(&row.path);
            let derived = service.type_candidates_with_budget(&fact.derived_name, &mut budget)?;
            let derived = derived.records.candidates.into_iter().find(|r| {
                r.path == row.path
                    && r.declaration_range.start_byte <= fact.source.range.start_byte
                    && fact.source.range.end_byte <= r.declaration_range.end_byte
            });
            let Some(derived) = derived else {
                coverage.partial = true;
                continue;
            };
            if !incoming && derived.identity != record.identity {
                continue;
            }
            let mut bases = Vec::new();
            if !fact.dependent {
                let root = PathBuf::new();
                let roots = vec![root.clone()];
                let contexts = HashMap::from([(
                    root,
                    MemberRootQueryContext {
                        declaration_read: self.read.clone(),
                        overlay: self.overlays.clone(),
                        current_path: row.path.clone(),
                        current_reach: self.reach.map(|g| g.reachable(&row.path)),
                        reach_graph: None,
                        semantic_generation: self
                            .read
                            .as_ref()
                            .map(|r| r.handle().generation)
                            .unwrap_or(crate::call_model::SemanticGeneration(0)),
                        semantic_family: self.family,
                    },
                )]);
                let mut resolver =
                    MemberResolutionService::new(&roots, &contexts, MemberPolicy::BoundNavigation);
                let resolved = resolver.resolve_owner_spelling(&fact.base_name, false)?;
                coverage.partial |= resolved.budget_exhausted || resolved.ambiguous;
                bases.extend(resolved.candidates.into_iter().flat_map(|(_, r)| r));
            }
            if bases.is_empty() {
                coverage.partial = true;
                if !incoming {
                    edges.push(InheritanceEdge {
                        derived,
                        base: None,
                        fact,
                    });
                }
                continue;
            }
            for base in bases {
                if incoming && base.identity != record.identity {
                    continue;
                }
                coverage.cycle |= base.identity == derived.identity;
                coverage.partial |= fact.source.guard.is_some() || coverage.cycle;
                edges.push(InheritanceEdge {
                    derived: derived.clone(),
                    base: Some(base),
                    fact: fact.clone(),
                });
            }
        }
        Ok(InheritancePage { edges, coverage })
    }
}
