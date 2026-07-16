//! Protocol-neutral request-time callable candidate resolution.

mod arity;
mod counterpart;
mod presentation;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};

use crate::call_model::CallableAnchor;
use crate::call_model::OwnerKindHint;
use crate::model::DefinitionCandidate;
use crate::reachability::ReachScope;

#[cfg(test)]
pub use arity::compatibility_for_signature;
pub use arity::{
    apply_arity_policy, ArgumentState, ArityCompatibility, ArityFilterOutcome, CallSiteContext,
    ContextReliability,
};
pub(crate) use counterpart::is_source_path;
pub use counterpart::{resolve_counterparts, CounterpartEvidence};
#[cfg(test)]
pub use presentation::anchor_opposite_definition;
#[cfg(test)]
pub use presentation::call_declaration_presentations;
pub use presentation::{
    call_declaration_presentations_at, call_definition_presentations, hover_presentations,
    signature_active_index, signature_presentations,
};

/// Changes whenever callable identity/filter/grouping semantics change.
///
/// This is deliberately independent from the Call Relations wire protocol.
#[allow(dead_code)] // Read by the release hardening gate and future relation cursors.
pub const CALLABLE_CANDIDATE_RESOLVER_VERSION: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CandidateOrigin {
    Base,
    Overlay,
}

#[allow(dead_code)] // The full bounded-query vocabulary is part of the coverage contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CandidateIncompleteReason {
    ScanLimit,
    CandidateBudget,
    TimeBudget,
    Cancelled,
    FactsUnavailable,
    GenerationMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CandidateCoverage {
    pub scanned: usize,
    pub truncated: bool,
    pub scope_open: bool,
    pub incomplete_reason: Option<CandidateIncompleteReason>,
}

impl CandidateCoverage {
    #[cfg(test)]
    pub fn complete(scanned: usize) -> Self {
        Self {
            scanned,
            ..Self::default()
        }
    }

    pub fn permits_uniqueness(&self) -> bool {
        !self.truncated && !self.scope_open && self.incomplete_reason.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCallableAnchor {
    pub anchor: CallableAnchor,
    pub candidate: DefinitionCandidate,
    pub arity_compatibility: ArityCompatibility,
    pub origin: CandidateOrigin,
}

impl ResolvedCallableAnchor {
    pub fn new(
        anchor: CallableAnchor,
        candidate: DefinitionCandidate,
        origin: CandidateOrigin,
    ) -> Self {
        Self {
            anchor,
            candidate,
            arity_compatibility: ArityCompatibility::Unknown,
            origin,
        }
    }

    pub fn canonical_signature(&self) -> &str {
        &self.anchor.canonical_signature
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallableVariantGroup {
    pub header_declaration: Option<ResolvedCallableAnchor>,
    pub source_definition: Option<ResolvedCallableAnchor>,
    pub other_variants: Vec<ResolvedCallableAnchor>,
    pub group_tier: crate::model::ScopeTier,
    pub counterpart_evidence: CounterpartEvidence,
}

impl CallableVariantGroup {
    pub fn variants(&self) -> impl Iterator<Item = &ResolvedCallableAnchor> {
        self.header_declaration
            .iter()
            .chain(self.source_definition.iter())
            .chain(self.other_variants.iter())
    }

    pub fn strongest_arity_compatibility(&self) -> ArityCompatibility {
        self.variants()
            .map(|variant| variant.arity_compatibility)
            .max_by_key(|compatibility| compatibility.rank())
            .unwrap_or(ArityCompatibility::Unknown)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallableCandidateSet {
    pub anchors: Vec<ResolvedCallableAnchor>,
    pub groups: Vec<CallableVariantGroup>,
    pub coverage: CandidateCoverage,
    pub arity_mismatch_fallback: bool,
}

/// Privacy-safe aggregate counters for one pure callable resolution.
///
/// No candidate names, paths, signatures or source text are retained.  The
/// raw count comes from the bounded recall coverage; all remaining counters
/// are derived from the already-filtered candidate set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CallableCandidateMetrics {
    pub raw_candidates: usize,
    pub filtered_candidates: usize,
    pub grouped_candidates: usize,
    pub arity_compatible: usize,
    pub arity_unknown: usize,
    pub arity_incompatible: usize,
    pub counterpart_strict: usize,
    pub counterpart_ambiguous: usize,
}

impl CallableCandidateSet {
    pub fn metrics(&self) -> CallableCandidateMetrics {
        let mut metrics = CallableCandidateMetrics {
            raw_candidates: self.coverage.scanned,
            filtered_candidates: self.anchors.len(),
            grouped_candidates: self.groups.len(),
            ..CallableCandidateMetrics::default()
        };
        for anchor in &self.anchors {
            match anchor.arity_compatibility {
                ArityCompatibility::Compatible => metrics.arity_compatible += 1,
                ArityCompatibility::Unknown => metrics.arity_unknown += 1,
                ArityCompatibility::Incompatible => metrics.arity_incompatible += 1,
            }
        }
        for group in &self.groups {
            match group.counterpart_evidence {
                CounterpartEvidence::StrictOneToOne => metrics.counterpart_strict += 1,
                CounterpartEvidence::Ambiguous { .. } => metrics.counterpart_ambiguous += 1,
                CounterpartEvidence::Unpaired | CounterpartEvidence::IncompleteCoverage => {}
            }
        }
        metrics
    }
}

pub struct CallableQueryInput {
    pub base_anchors: Vec<ResolvedCallableAnchor>,
    pub overlay_anchors: Vec<ResolvedCallableAnchor>,
    pub shadowed_paths: HashSet<String>,
    pub call_context: Option<CallSiteContext>,
    pub source_reach: HashMap<String, ReachScope>,
    /// Physical files proven to participate in the current translation unit.
    /// Internal-linkage anchors outside this set are impossible bindings.
    pub visible_internal_paths: HashSet<String>,
    pub coverage: CandidateCoverage,
}

/// Pure orchestration over already-recalled, generation-pinned facts.
///
/// The store-backed `CandidateQueryService` owns generation guards and bounded
/// reads; this function deliberately owns no persistence resources.
pub fn resolve_callable_candidates(input: CallableQueryInput) -> CallableCandidateSet {
    let mut anchors: Vec<_> = input
        .base_anchors
        .into_iter()
        .filter(|anchor| !input.shadowed_paths.contains(&anchor.anchor.path))
        .collect();
    anchors.extend(input.overlay_anchors);

    // This resolver intentionally models free-function requests only. Keep a
    // parser-Unknown explicit owner as a conservative ordinary candidate: an
    // out-of-namespace `net::open` definition has that shape without compiler
    // binding evidence. It remains ineligible for strict counterpart pairing.
    // Proven record members are still excluded.
    anchors.retain(|anchor| {
        matches!(
            anchor.anchor.owner_kind,
            None | Some(OwnerKindHint::Namespace | OwnerKindHint::Unknown)
        )
    });
    anchors.retain(|anchor| match &anchor.anchor.linkage {
        crate::call_model::LinkageDomain::Internal(_) => {
            input.visible_internal_paths.contains(&anchor.anchor.path)
        }
        crate::call_model::LinkageDomain::External | crate::call_model::LinkageDomain::Unknown => {
            true
        }
    });
    // Defend the pure boundary too, even though the request facade normally
    // rejects these forms first. Otherwise a `widget.open()` name-only recall
    // could launder ordinary free functions or Unknown explicit owners into a
    // member-call result.
    if input.call_context.as_ref().is_some_and(|context| {
        context.reliability == ContextReliability::UnsupportedCallForm
            || !matches!(
                context.form,
                crate::call_model::CallForm::DirectName
                    | crate::call_model::CallForm::QualifiedName
                    | crate::call_model::CallForm::ParenthesizedName
            )
    }) {
        anchors.clear();
    }
    if let Some(qualified_name) = input
        .call_context
        .as_ref()
        .filter(|context| context.reliability.is_reliable())
        .and_then(|context| context.qualified_name.as_deref())
    {
        anchors.retain(|anchor| anchor.anchor.qualified_name == qualified_name);
    }

    let mut fingerprints = HashSet::new();
    anchors.retain(|anchor| {
        fingerprints.insert((
            anchor.anchor.path.clone(),
            anchor.anchor.anchor_fingerprint.clone(),
            anchor.anchor.name_range.start_byte,
            anchor.anchor.name_range.end_byte,
        ))
    });

    let arity_outcome = apply_arity_policy(&mut anchors, input.call_context.as_ref());
    let groups = resolve_counterparts(&anchors, &input.source_reach, &input.coverage);

    CallableCandidateSet {
        anchors,
        groups,
        coverage: input.coverage,
        arity_mismatch_fallback: arity_outcome == ArityFilterOutcome::MismatchFallback,
    }
}
