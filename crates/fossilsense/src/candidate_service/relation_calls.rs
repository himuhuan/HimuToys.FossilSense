//! One target policy shared by incoming and outgoing call queries.
use super::relation_facts::*;
use super::relation_types::*;
use super::*;
use crate::call_model::*;
use crate::semantic_model::{condition_relation, relations::*, ConditionRelation};
use crate::store::views::relation_facts::{RelationFact, RelationFactLookup};

pub struct CallTargetResolution {
    pub anchors: Vec<CallableAnchor>,
    pub verified: Option<VerifiedCallTargets>,
    pub scanned: usize,
}
impl RelationQueryContext<'_> {
    pub fn call_targets(
        &self,
        call: &CallSiteFact,
        caller: Option<&CallableAnchor>,
        control: &RelationControl<'_>,
    ) -> Result<CallTargetResolution> {
        anyhow::ensure!(
            !control.cancelled(),
            "request cancelled during relation target query"
        );
        let mut result = CallTargetResolution {
            anchors: Vec::new(),
            verified: None,
            scanned: 0,
        };
        if self.family != SemanticFamily::CFamily {
            return Ok(result);
        }
        let service = self.service(&call.path);
        let (sites, cov) = service.relation_fact_page(
            0,
            RelationFactLookup::Caller(&call.caller_entity_key),
            &RelationCursor::default(),
            control,
        )?;
        result.scanned += cov.scanned;
        let binding = sites
            .into_iter()
            .filter(|r| r.path == call.path)
            .find_map(|r| match r.fact {
                RelationFact::Binding(f)
                    if f.source.range.start_byte == call.callee_range.start_byte =>
                {
                    Some(f)
                }
                _ => None,
            });
        if binding.is_none() && cov.next.is_some() {
            result.verified = Some(VerifiedCallTargets {
                partial: true,
                ..Default::default()
            });
            return Ok(result);
        }
        let name = call.callee_name.as_deref().unwrap_or("");
        let (macros, macro_cov) = service.relation_fact_page(
            3,
            RelationFactLookup::Name(name),
            &RelationCursor::default(),
            control,
        )?;
        result.scanned += macro_cov.scanned;
        let local_event = macros
            .iter()
            .filter(|r| {
                r.path == call.path
                    && r.fact.source().range.start_byte < call.expression_range.start_byte
            })
            .max_by_key(|r| r.fact.source().range.start_byte);
        let local_undef = local_event.is_some_and(
            |r| matches!(&r.fact,RelationFact::Macro(f) if f.undef && f.source.guard.is_none()),
        );
        let mut active = Vec::new();
        for row in &macros {
            anyhow::ensure!(
                !control.cancelled(),
                "request cancelled during macro candidate query"
            );
            let RelationFact::Macro(fact) = &row.fact else {
                continue;
            };
            if fact.undef || !fact.function_like || local_undef {
                continue;
            }
            if macros.iter().any(|later| {
                later.path == row.path
                    && later.fact.source().guard.is_none()
                    && later.fact.source().range.start_byte > fact.source.range.start_byte
                    && (later.path != call.path
                        || later.fact.source().range.start_byte < call.expression_range.start_byte)
            }) {
                continue;
            }
            if row.path == call.path {
                if fact.source.range.start_byte >= call.expression_range.start_byte {
                    continue;
                }
                if local_event.is_some_and(|last| {
                    last.fact.source().guard.is_none()
                        && last.fact.source().range.start_byte > fact.source.range.start_byte
                }) {
                    continue;
                }
            } else if !self
                .reach
                .as_ref()
                .is_some_and(|g| g.reachable(&call.path).files.contains(&row.path))
            {
                continue;
            }
            if condition_relation(call.guard.as_deref(), fact.source.guard.as_deref())
                == ConditionRelation::Incompatible
            {
                continue;
            }
            active.push(row);
        }
        if !active.is_empty() && matches!(call.form, CallForm::DirectName) {
            let mut proof = VerifiedCallTargets {
                partial: macro_cov.next.is_some(),
                ..Default::default()
            };
            let anchors = service.callable_subject_candidates(name, None)?;
            for row in active {
                for candidate in &anchors.anchors {
                    let a = &candidate.anchor;
                    if a.kind != CallableKind::FunctionLikeMacro
                        || a.path != row.path
                        || a.name_range != row.fact.source().range
                    {
                        continue;
                    }
                    proof.targets.push(VerifiedCallTarget {
                        anchor_fingerprint: a.anchor_fingerprint.clone(),
                        candidate_only: true,
                        evidence: EvidenceLedger {
                            supports: vec![EvidenceCode::MacroExpansionUnknown],
                            unknowns: vec![EvidenceCode::ExpansionNotEvaluated],
                            ..Default::default()
                        },
                        sources: vec![CallTargetSource {
                            path: row.path.clone(),
                            range: row.fact.source().range,
                        }],
                    });
                    result.anchors.push(a.clone());
                }
            }
            result.verified = Some(proof);
            return Ok(result);
        }
        let mut receiver = binding.as_ref().and_then(|b| b.receiver.clone());
        if receiver.is_none() {
            if let Some((owner, _)) = call
                .qualified_name
                .as_deref()
                .and_then(|q| q.rsplit_once("::"))
            {
                let bundle = service.type_candidates(owner)?;
                if !bundle.records.candidates.is_empty() || !bundle.aliases.candidates.is_empty() {
                    receiver = Some(ReceiverFact {
                        spelling: owner.to_owned(),
                        type_name: Some(owner.to_owned()),
                        tag_domain: false,
                        chain: Vec::new(),
                        object_anchor: None,
                        object_scope: None,
                    });
                }
            }
        }
        if let Some(receiver) = &receiver {
            let (members, member_cov) = self.members(&call.path, receiver, name, control)?;
            result.scanned += member_cov.scanned;
            let mut proof = VerifiedCallTargets {
                partial: member_cov.partial
                    || member_cov.unavailable
                    || cov.next.is_some()
                    || call.syntax_error_overlap,
                ..Default::default()
            };
            let subjects = service.member_entity_subjects(&members)?;
            let locations = service.entity_documentation_locations(&subjects)?;
            proof.partial |= locations.coverage.truncated || locations.coverage.stale;
            for candidate in service.entity_callable_presentations(&locations)? {
                let a = candidate.anchor;
                if call
                    .argument_count
                    .is_some_and(|count| a.signature.accepts_arity(count) == Some(false))
                    || condition_relation(call.guard.as_deref(), a.guard.as_deref())
                        == ConditionRelation::Incompatible
                {
                    continue;
                }
                let dynamic = a
                    .presentation_signature
                    .split_whitespace()
                    .any(|t| matches!(t, "virtual" | "override"));
                proof.targets.push(VerifiedCallTarget {
                    anchor_fingerprint: a.anchor_fingerprint.clone(),
                    candidate_only: dynamic || a.guard.is_some() || proof.partial,
                    evidence: EvidenceLedger {
                        supports: vec![
                            EvidenceCode::OwnedMember,
                            EvidenceCode::CompatibleSignature,
                        ],
                        unknowns: if dynamic {
                            vec![EvidenceCode::RuntimeDispatchUnknown]
                        } else {
                            Vec::new()
                        },
                        ..Default::default()
                    },
                    sources: Vec::new(),
                });
                result.anchors.push(a);
            }
            // A field may be an operations-table slot rather than a method.
            if !proof.targets.is_empty() {
                result.verified = Some(proof);
                return Ok(result);
            }
            // A variable naming a pointer is not a points-to identity. Only a
            // known field on a concrete object can match initializer evidence.
            if !members
                .iter()
                .any(|m| m.member.kind == crate::parser::MemberKind::Field)
                || call.form == CallForm::MemberArrow
                || receiver
                    .type_name
                    .as_ref()
                    .is_none_or(|ty| ty.contains(['*', '&']))
                || receiver.object_anchor.is_none()
            {
                proof.partial = true;
                result.verified = Some(proof);
                return Ok(result);
            }
        }
        let indirect = receiver.is_some()
            || binding.as_ref().is_some_and(|b| b.local_anchor.is_some())
            || call.form == CallForm::FunctionPointer;
        if indirect {
            let (rows, assignment_cov) = service.relation_fact_page(
                2,
                RelationFactLookup::Name(name),
                &RelationCursor::default(),
                control,
            )?;
            result.scanned += assignment_cov.scanned;
            let mut proof = VerifiedCallTargets {
                partial: assignment_cov.next.is_some()
                    || assignment_cov.unavailable
                    || cov.next.is_some(),
                ..Default::default()
            };
            let mut assignments = Vec::new();
            for row in rows {
                anyhow::ensure!(
                    !control.cancelled(),
                    "request cancelled during assignment candidate query"
                );
                let RelationFact::Assignment(f) = row.fact else {
                    continue;
                };
                if row.path != call.path {
                    continue;
                }
                let same = if let Some(r) = &receiver {
                    f.member.as_deref() == Some(name)
                        && r.object_anchor.is_some()
                        && r.object_anchor == f.slot.object_anchor
                        && r.spelling == f.slot.spelling
                        && r.chain == f.slot.chain
                } else {
                    f.member.is_none()
                        && binding.as_ref().is_some_and(|b| {
                            b.local_anchor.is_some() && b.local_anchor == f.slot.object_anchor
                        })
                };
                if !same {
                    continue;
                }
                if f.source.enclosing_callable.is_some()
                    && f.source.enclosing_callable.as_ref() != Some(&call.caller_entity_key)
                {
                    proof.partial = true;
                    continue;
                }
                if f.source.range.start_byte >= call.expression_range.start_byte {
                    continue;
                }
                if condition_relation(call.guard.as_deref(), f.source.guard.as_deref())
                    == ConditionRelation::Incompatible
                {
                    continue;
                }
                assignments.push(f);
            }
            assignments.sort_by_key(|f| f.source.range.start_byte);
            let last_unconditional = assignments
                .iter()
                .rposition(|f| f.branch.is_none() && f.source.guard.is_none() && !f.unknown_write);
            for (i, f) in assignments.iter().enumerate() {
                proof.partial |= f.unknown_write || f.branch.is_some();
                if last_unconditional.is_some_and(|last| i < last) {
                    continue;
                }
                let Some(target) = f.target_name.as_deref() else {
                    continue;
                };
                let candidates = service.callable_subject_candidates(target, None)?;
                for candidate in candidates.anchors {
                    let a = candidate.anchor;
                    if a.kind != CallableKind::Function
                        || a.owner_kind == Some(OwnerKindHint::Record)
                        || matches!(&a.linkage,LinkageDomain::Internal(path) if path!=&call.path)
                        || condition_relation(f.source.guard.as_deref(), a.guard.as_deref())
                            == ConditionRelation::Incompatible
                    {
                        continue;
                    }
                    proof.targets.push(VerifiedCallTarget {
                        anchor_fingerprint: a.anchor_fingerprint.clone(),
                        candidate_only: true,
                        evidence: EvidenceLedger {
                            supports: vec![EvidenceCode::AssignmentCandidate],
                            unknowns: vec![EvidenceCode::RuntimeDispatchUnknown],
                            ..Default::default()
                        },
                        sources: vec![
                            CallTargetSource {
                                path: call.path.clone(),
                                range: f.source.range,
                            },
                            CallTargetSource {
                                path: call.path.clone(),
                                range: call.expression_range,
                            },
                        ],
                    });
                    result.anchors.push(a);
                }
            }
            result.verified = Some(proof);
            return Ok(result);
        }
        if caller.is_some_and(|c| c.kind == CallableKind::FunctionLikeMacro) {
            let mut proof = VerifiedCallTargets::default();
            for candidate in service.callable_subject_candidates(name, None)?.anchors {
                let a = candidate.anchor;
                if a.kind != CallableKind::Function
                    || matches!(&a.linkage,LinkageDomain::Internal(p) if p!=&call.path)
                {
                    continue;
                }
                proof.targets.push(VerifiedCallTarget {
                    anchor_fingerprint: a.anchor_fingerprint.clone(),
                    candidate_only: true,
                    evidence: EvidenceLedger {
                        supports: vec![EvidenceCode::MacroExpansionUnknown],
                        unknowns: vec![EvidenceCode::ExpansionNotEvaluated],
                        ..Default::default()
                    },
                    sources: vec![CallTargetSource {
                        path: call.path.clone(),
                        range: call.expression_range,
                    }],
                });
                result.anchors.push(a);
            }
            result.verified = Some(proof);
        }
        Ok(result)
    }

    /// Reverse recall uses assignment targets to discover slot spellings, then
    /// the same resolver verifies the object and owner at each recalled site.
    pub fn indirect_incoming_names(
        &self,
        name: &str,
        control: &RelationControl<'_>,
    ) -> Result<(Vec<String>, bool)> {
        let (rows, coverage) = self.service("").relation_fact_page(
            2,
            RelationFactLookup::Target(name),
            &RelationCursor::default(),
            control,
        )?;
        let mut names = Vec::new();
        for row in rows {
            if let RelationFact::Assignment(f) = row.fact {
                names.push(f.member.unwrap_or(f.slot.spelling));
            }
        }
        names.sort();
        names.dedup();
        Ok((names, coverage.next.is_some() || coverage.unavailable))
    }
}
