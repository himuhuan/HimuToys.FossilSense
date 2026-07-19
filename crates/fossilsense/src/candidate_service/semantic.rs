use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::model::{
    CandidateGroup, CandidateRange, CandidateRef, CandidateSet, DefinitionCandidate,
    ResolutionConfidence, ResolutionReason, ScopeTier, SharedCandidateCoverage,
};
use crate::query::CandidateRevision;
use crate::resolver::{self, ResolveContext};
use crate::semantic_model::{
    DeclarationBacking, DeclarationFact, LogicalEntityKey, SemanticDeclarationKind,
    SemanticDeclarationRole, SemanticFactFidelity,
};

use super::{
    candidate_origin_priority, CandidateOrigin, CandidateQueryService, TypeCandidateBundle,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum SemanticIntent {
    Neutral,
    Call,
    Type,
    Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDeclarationCandidate {
    pub persistent_id: Option<i64>,
    pub fact: DeclarationFact,
    pub backing_kind: String,
    pub backing_id: Option<i64>,
    pub tier: ScopeTier,
    pub confidence: ResolutionConfidence,
    pub reason: ResolutionReason,
    pub origin: CandidateOrigin,
    pub external: bool,
    pub directly_included: bool,
    pub revision: Option<CandidateRevision>,
}

impl ResolvedDeclarationCandidate {
    pub fn as_definition_candidate(&self) -> DefinitionCandidate {
        DefinitionCandidate {
            name: self.fact.name.clone(),
            kind: declaration_kind_name(self.fact.declaration_kind).into(),
            role: declaration_role_name(self.fact.role).into(),
            path: self.fact.path.clone(),
            range: CandidateRange {
                start_line: self.fact.name_range.start.line,
                start_col: self.fact.name_range.start.character,
                end_line: self.fact.name_range.end.line,
                end_col: self.fact.name_range.end.character,
            },
            source: if self.external {
                "external".into()
            } else {
                "workspace".into()
            },
            tier: self.tier,
            base_match: 1_000,
            confidence: self.confidence,
            reason: self.reason,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateHandle {
    pub locator: CandidateHandleLocator,
    pub logical_key: LogicalEntityKey,
    pub locator_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "origin")]
pub enum CandidateHandleLocator {
    Persistent { declaration_id: i64 },
    Overlay { fingerprint: String },
}

pub fn focused_candidates(
    set: &CandidateSet<ResolvedDeclarationCandidate>,
) -> Vec<&ResolvedDeclarationCandidate> {
    set.focused
        .iter()
        .filter_map(|candidate_ref| {
            set.all
                .get(candidate_ref.group_index)?
                .candidates
                .get(candidate_ref.candidate_index)
        })
        .collect()
}

pub fn focused_callable_fingerprints(
    set: &CandidateSet<ResolvedDeclarationCandidate>,
) -> HashSet<&str> {
    let focused_groups: HashSet<_> = set.focused.iter().map(|item| item.group_index).collect();
    set.all
        .iter()
        .enumerate()
        .filter(|(index, _)| focused_groups.contains(index))
        .flat_map(|(_, group)| group.candidates.iter())
        .filter(|candidate| {
            matches!(
                candidate.fact.declaration_kind,
                SemanticDeclarationKind::Function | SemanticDeclarationKind::Method
            )
        })
        .map(|candidate| candidate.fact.identity.locator.fingerprint.as_str())
        .collect()
}

pub fn focused_has_kind(
    set: &CandidateSet<ResolvedDeclarationCandidate>,
    accepts: impl Fn(SemanticDeclarationKind) -> bool,
) -> bool {
    let focused_groups: HashSet<_> = set.focused.iter().map(|item| item.group_index).collect();
    set.all
        .iter()
        .enumerate()
        .any(|(index, group)| focused_groups.contains(&index) && accepts(group.declaration_kind))
}

pub fn navigation_presentations(
    set: &CandidateSet<ResolvedDeclarationCandidate>,
    declaration: bool,
) -> Vec<DefinitionCandidate> {
    let focused_groups: HashSet<_> = set.focused.iter().map(|item| item.group_index).collect();
    let mut selected = Vec::new();
    for (index, group) in set.all.iter().enumerate() {
        if !focused_groups.contains(&index) {
            continue;
        }
        let preferred_role = if declaration {
            group
                .candidates
                .iter()
                .any(|candidate| candidate.fact.role == SemanticDeclarationRole::Declaration)
                .then_some(SemanticDeclarationRole::Declaration)
        } else {
            [
                SemanticDeclarationRole::Definition,
                SemanticDeclarationRole::TentativeDefinition,
                SemanticDeclarationRole::Declaration,
                SemanticDeclarationRole::Unknown,
            ]
            .into_iter()
            .find(|role| {
                group
                    .candidates
                    .iter()
                    .any(|candidate| candidate.fact.role == *role)
            })
        };
        if let Some(role) = preferred_role {
            selected.extend(
                group
                    .candidates
                    .iter()
                    .filter(|candidate| candidate.fact.role == role)
                    .map(ResolvedDeclarationCandidate::as_definition_candidate),
            );
        } else if let Some(candidate) = group.candidates.first() {
            selected.push(candidate.as_definition_candidate());
        }
    }
    selected.sort_by(|left, right| {
        right
            .tier
            .cmp(&left.tier)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.range.start_line.cmp(&right.range.start_line))
            .then_with(|| left.range.start_col.cmp(&right.range.start_col))
    });
    selected.dedup_by(|left, right| left.path == right.path && left.range == right.range);
    selected
}

impl CandidateQueryService<'_> {
    pub fn semantic_candidates(
        &self,
        name: &str,
        intent: SemanticIntent,
    ) -> Result<CandidateSet<ResolvedDeclarationCandidate>> {
        let mut scanned = 0usize;
        let mut truncated = false;
        let mut candidates = Vec::new();
        let resolve_context = ResolveContext {
            current_path: Some(self.current_path),
            reach: self.current_reach.as_deref(),
            direct_external_files: None,
        };

        if let Some(handle) = self.handle {
            let (rows, limited) = if let Some(index) = self.declaration_index {
                let scope =
                    self.current_reach
                        .as_deref()
                        .map(|reach| crate::query::CompletionScope {
                            reach: reach.clone(),
                            current_path: Some(self.current_path.to_string()),
                            direct_external_files: self
                                .reach_graph
                                .map(|graph| {
                                    graph.directly_included_external_paths_from(self.current_path)
                                })
                                .unwrap_or_default(),
                        });
                let hits = index.exact_name_hits_scoped(
                    name,
                    self.exact_name_limit.saturating_add(1),
                    scope.as_ref(),
                );
                let limited = hits.len() > self.exact_name_limit;
                let ids: Vec<_> = hits
                    .into_iter()
                    .take(self.exact_name_limit)
                    .map(|hit| hit.id)
                    .collect();
                (index.payloads_by_ids(handle, &ids)?, limited)
            } else {
                let (current_paths, reachable_paths) = self.durable_priority_path_groups();
                let (rows, limited) = handle.read(|store| {
                    let view = store.declaration_view();
                    let (global, mut limited) =
                        view.by_name_limited(name, self.exact_name_limit)?;
                    if !limited {
                        return Ok((global, false));
                    }

                    let mut rows = Vec::new();
                    for paths in [&current_paths, &reachable_paths] {
                        let remaining = self.exact_name_limit.saturating_sub(rows.len());
                        let (priority, priority_limited) =
                            view.by_name_in_paths_limited(name, paths, remaining)?;
                        rows.extend(priority);
                        limited |= priority_limited;
                    }
                    rows.extend(global);
                    let mut seen = HashSet::new();
                    rows.retain(|row| seen.insert(row.id));
                    Ok((rows, limited))
                })?;
                (rows.into_iter().map(Arc::new).collect(), limited)
            };
            scanned += rows.len();
            truncated |= limited;
            candidates.extend(rows.into_iter().filter_map(|row| {
                let mut row = (*row).clone();
                if self.overlays.shadows(&row.fact.path) {
                    return None;
                }
                let (external, directly_included) =
                    self.path_evidence(&row.fact.path, row.external, row.directly_included);
                let tier = resolver::scope_tier(
                    &row.fact.path,
                    external,
                    directly_included,
                    Some(&resolve_context),
                );
                let (confidence, reason) = resolver::confidence_reason_for(
                    tier,
                    true,
                    self.current_reach.as_ref().and_then(|reach| reach.reason),
                );
                row.fact.identity.locator.workspace_id = handle.generation.0.to_string();
                Some(ResolvedDeclarationCandidate {
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
                })
            }));
        }

        let overlay = self.overlays.declarations(name);
        scanned += overlay.len();
        candidates.extend(overlay.iter().cloned().map(|entry| {
            let external = std::path::Path::new(&entry.path).is_absolute();
            let (external, directly_included) = self.path_evidence(&entry.path, external, false);
            let tier = resolver::scope_tier(
                &entry.path,
                external,
                directly_included,
                Some(&resolve_context),
            );
            let (confidence, reason) = resolver::confidence_reason_for(
                tier,
                true,
                self.current_reach.as_ref().and_then(|reach| reach.reason),
            );
            ResolvedDeclarationCandidate {
                persistent_id: None,
                fact: entry.fact,
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
        }));

        candidates.sort_by(candidate_order);
        if candidates.len() > self.exact_name_limit {
            candidates.truncate(self.exact_name_limit);
            truncated = true;
        }
        Ok(build_set(
            candidates,
            intent,
            SharedCandidateCoverage {
                scanned,
                truncated,
                scope_open: self.current_reach.as_ref().is_some_and(|reach| reach.open),
                facts_incomplete: self.overlays.has_incomplete_facts(),
                generation_mismatch: false,
            },
        ))
    }

    pub fn resolve_candidate_handle(
        &self,
        candidate: &CandidateHandle,
    ) -> Result<Option<ResolvedDeclarationCandidate>> {
        match &candidate.locator {
            CandidateHandleLocator::Persistent { declaration_id } => {
                let Some(handle) = self.handle else {
                    return Ok(None);
                };
                let mut rows =
                    handle.read(|store| store.declaration_view().by_ids(&[*declaration_id]))?;
                let Some(row) = rows.pop() else {
                    return Ok(None);
                };
                if row.fact.identity.locator.fingerprint != candidate.locator_fingerprint
                    || row.fact.identity.logical_key != candidate.logical_key
                    || self.overlays.shadows(&row.fact.path)
                {
                    return Ok(None);
                }
                Ok(self
                    .semantic_candidates(&row.fact.name, SemanticIntent::Neutral)?
                    .all
                    .into_iter()
                    .flat_map(|group| group.candidates)
                    .find(|resolved| resolved.persistent_id == Some(*declaration_id)))
            }
            CandidateHandleLocator::Overlay { fingerprint } => {
                let Some(entry) = self.overlays.declaration_by_fingerprint(fingerprint) else {
                    let name = candidate
                        .logical_key
                        .qualified_name
                        .rsplit("::")
                        .next()
                        .unwrap_or(candidate.logical_key.qualified_name.as_str());
                    if let Some(resolved) = self
                        .semantic_candidates(name, SemanticIntent::Neutral)?
                        .all
                        .into_iter()
                        .flat_map(|group| group.candidates)
                        .find(|resolved| {
                            resolved.fact.identity.logical_key == candidate.logical_key
                        })
                    {
                        return Ok(Some(resolved));
                    }
                    let Some(read_handle) = self.handle else {
                        return Ok(None);
                    };
                    let (rows, _) = read_handle.read(|store| {
                        store
                            .declaration_view()
                            .by_logical_key_limited(&candidate.logical_key, self.exact_name_limit)
                    })?;
                    let Some(name) = rows
                        .into_iter()
                        .find(|row| !self.overlays.shadows(&row.fact.path))
                        .map(|row| row.fact.name)
                    else {
                        return Ok(None);
                    };
                    return Ok(self
                        .semantic_candidates(&name, SemanticIntent::Neutral)?
                        .all
                        .into_iter()
                        .flat_map(|group| group.candidates)
                        .find(|resolved| {
                            resolved.fact.identity.logical_key == candidate.logical_key
                        }));
                };
                if entry.fact.identity.logical_key != candidate.logical_key
                    || entry.fact.identity.locator.fingerprint != candidate.locator_fingerprint
                {
                    return Ok(None);
                }
                Ok(self
                    .semantic_candidates(&entry.fact.name, SemanticIntent::Neutral)?
                    .all
                    .into_iter()
                    .flat_map(|group| group.candidates)
                    .find(|resolved| resolved.fact.identity.locator.fingerprint == *fingerprint))
            }
        }
    }

    pub fn type_candidates_for_set(
        &self,
        name: &str,
        semantic: &CandidateSet<ResolvedDeclarationCandidate>,
    ) -> Result<TypeCandidateBundle> {
        let focused_groups: HashSet<_> = semantic
            .focused
            .iter()
            .map(|item| item.group_index)
            .collect();
        let mut record_ids = HashSet::new();
        let mut record_keys = HashSet::new();
        let mut alias_ids = HashSet::new();
        let mut alias_fingerprints = HashSet::new();
        for candidate in semantic
            .all
            .iter()
            .enumerate()
            .filter(|(index, _)| focused_groups.contains(index))
            .flat_map(|(_, group)| group.candidates.iter())
        {
            match (&candidate.fact.backing, candidate.backing_id) {
                (DeclarationBacking::Record { record_key }, _) => {
                    record_keys.insert(record_key.as_str());
                    if let Some(id) = candidate.backing_id {
                        record_ids.insert(id);
                    }
                }
                (DeclarationBacking::TypeAlias { fingerprint }, _) => {
                    alias_fingerprints.insert(fingerprint.as_str());
                    if let Some(id) = candidate.backing_id {
                        alias_ids.insert(id);
                    }
                }
                _ => {}
            }
        }
        let mut bundle = self.type_candidates(name)?;
        bundle
            .records
            .candidates
            .retain(|record| match &record.identity {
                crate::query::RecordCandidateIdentity::Persistent(id) => record_ids.contains(id),
                crate::query::RecordCandidateIdentity::ParserKey { record_key, .. } => {
                    record_keys.contains(record_key.as_str())
                }
            });
        bundle
            .aliases
            .candidates
            .retain(|alias| match &alias.identity {
                crate::query::TypeAliasCandidateIdentity::Persistent { id, .. } => {
                    alias_ids.contains(id)
                }
                crate::query::TypeAliasCandidateIdentity::ParserFingerprint {
                    fingerprint, ..
                } => alias_fingerprints.contains(fingerprint.as_str()),
            });
        let retained_aliases: HashSet<_> = bundle
            .aliases
            .candidates
            .iter()
            .map(|alias| alias.identity.clone())
            .collect();
        bundle
            .alias_resolutions
            .retain(|resolution| retained_aliases.contains(&resolution.alias.identity));
        Ok(bundle)
    }
}

fn build_set(
    candidates: Vec<ResolvedDeclarationCandidate>,
    intent: SemanticIntent,
    coverage: SharedCandidateCoverage,
) -> CandidateSet<ResolvedDeclarationCandidate> {
    let mut groups = Vec::<CandidateGroup<ResolvedDeclarationCandidate>>::new();
    let mut keyed = HashMap::<LogicalEntityKey, usize>::new();
    for candidate in candidates {
        let reliable = candidate.fact.identity.fact_fidelity != SemanticFactFidelity::LowFidelity;
        let group_index = if reliable {
            let key = candidate.fact.identity.logical_key.clone();
            *keyed.entry(key.clone()).or_insert_with(|| {
                let index = groups.len();
                groups.push(empty_group(Some(key), &candidate));
                index
            })
        } else {
            let index = groups.len();
            groups.push(empty_group(None, &candidate));
            index
        };
        let group = &mut groups[group_index];
        group.tier = group.tier.max(candidate.tier);
        group.authoritative |=
            candidate.fact.identity.fact_fidelity == SemanticFactFidelity::Authoritative;
        group.low_fidelity &=
            candidate.fact.identity.fact_fidelity != SemanticFactFidelity::Authoritative;
        group.candidates.push(candidate);
    }
    for group in &mut groups {
        group.candidates.sort_by(candidate_order);
    }
    groups.sort_by(|left, right| {
        right
            .tier
            .cmp(&left.tier)
            .then_with(|| kind_rank(left.declaration_kind).cmp(&kind_rank(right.declaration_kind)))
    });

    let matching: Vec<_> = groups
        .iter()
        .enumerate()
        .filter(|(_, group)| intent_accepts(intent, group.declaration_kind))
        .collect();
    let source = if matching.is_empty() {
        groups.iter().enumerate().collect()
    } else {
        matching
    };
    let highest = source.iter().map(|(_, group)| group.tier).max();
    let focused = source
        .into_iter()
        .filter(|(_, group)| Some(group.tier) == highest)
        .map(|(group_index, _)| CandidateRef {
            group_index,
            candidate_index: 0,
        })
        .collect();
    CandidateSet::new(groups, focused, coverage)
}

fn empty_group(
    logical_key: Option<LogicalEntityKey>,
    candidate: &ResolvedDeclarationCandidate,
) -> CandidateGroup<ResolvedDeclarationCandidate> {
    CandidateGroup {
        logical_key,
        declaration_kind: candidate.fact.declaration_kind,
        tier: candidate.tier,
        authoritative: false,
        low_fidelity: true,
        candidates: Vec::new(),
    }
}

fn candidate_order(
    left: &ResolvedDeclarationCandidate,
    right: &ResolvedDeclarationCandidate,
) -> std::cmp::Ordering {
    right
        .tier
        .cmp(&left.tier)
        .then_with(|| fidelity_rank(right).cmp(&fidelity_rank(left)))
        .then_with(|| role_rank(right.fact.role).cmp(&role_rank(left.fact.role)))
        .then_with(|| {
            candidate_origin_priority(right.origin).cmp(&candidate_origin_priority(left.origin))
        })
        .then_with(|| left.fact.path.cmp(&right.fact.path))
        .then_with(|| {
            left.fact
                .name_range
                .start_byte
                .cmp(&right.fact.name_range.start_byte)
        })
}

fn intent_accepts(intent: SemanticIntent, kind: SemanticDeclarationKind) -> bool {
    match intent {
        SemanticIntent::Neutral => true,
        SemanticIntent::Call => matches!(
            kind,
            SemanticDeclarationKind::Function | SemanticDeclarationKind::Method
        ),
        SemanticIntent::Type => matches!(
            kind,
            SemanticDeclarationKind::Type | SemanticDeclarationKind::Alias
        ),
        SemanticIntent::Value => matches!(
            kind,
            SemanticDeclarationKind::Object
                | SemanticDeclarationKind::EnumConstant
                | SemanticDeclarationKind::Macro
                | SemanticDeclarationKind::Function
                | SemanticDeclarationKind::Method
        ),
    }
}

fn fidelity_rank(candidate: &ResolvedDeclarationCandidate) -> u8 {
    match candidate.fact.identity.fact_fidelity {
        SemanticFactFidelity::Authoritative => 2,
        SemanticFactFidelity::Incomplete => 1,
        SemanticFactFidelity::LowFidelity => 0,
    }
}

fn role_rank(role: SemanticDeclarationRole) -> u8 {
    match role {
        SemanticDeclarationRole::Definition => 3,
        SemanticDeclarationRole::TentativeDefinition => 2,
        SemanticDeclarationRole::Declaration => 1,
        SemanticDeclarationRole::Unknown => 0,
    }
}

fn kind_rank(kind: SemanticDeclarationKind) -> u8 {
    match kind {
        SemanticDeclarationKind::Function => 0,
        SemanticDeclarationKind::Method => 1,
        SemanticDeclarationKind::Object => 2,
        SemanticDeclarationKind::Type => 3,
        SemanticDeclarationKind::Alias => 4,
        SemanticDeclarationKind::EnumConstant => 5,
        SemanticDeclarationKind::Macro => 6,
    }
}

fn declaration_kind_name(kind: SemanticDeclarationKind) -> &'static str {
    match kind {
        SemanticDeclarationKind::Function | SemanticDeclarationKind::Method => "function",
        SemanticDeclarationKind::Object => "global_variable",
        SemanticDeclarationKind::Type | SemanticDeclarationKind::Alias => "type",
        SemanticDeclarationKind::EnumConstant => "enum_constant",
        SemanticDeclarationKind::Macro => "macro",
    }
}

fn declaration_role_name(role: SemanticDeclarationRole) -> &'static str {
    match role {
        SemanticDeclarationRole::Definition => "definition",
        SemanticDeclarationRole::Declaration => "declaration",
        SemanticDeclarationRole::TentativeDefinition => "tentative_definition",
        SemanticDeclarationRole::Unknown => "unknown",
    }
}
