//! Protocol-agnostic candidate vocabulary: the canonical types that model
//! FossilSense's best-effort name candidates. This module is the single source
//! of truth shared by the code and the concept-model doc — new features reuse
//! these names instead of introducing parallel `smart`/`semantic`/`scope`
//! types.
//!
//! The genuinely-new types ([`DefinitionCandidate`], [`ResolutionConfidence`],
//! [`ResolutionReason`]) live here. The concept anchors ([`Occurrence`],
//! [`ReferenceHit`], [`ReachScope`], [`OpenReason`]) stay in their producing
//! modules and are re-exported so callers and docs reference one name per
//! concept.

/// Match-quality confidence for a [`DefinitionCandidate`]. Higher variants
/// outrank lower ones (`Exact` > `Reachable` > `Heuristic` > `Ambiguous` >
/// `Fallback`). This is *match-quality confidence*, not semantic binding:
/// `Exact` means an exact name match on a reachable/current-file definition,
/// never a compiler-level binding. Derived from the existing scope/source
/// signals; R2 will split base-match vs policy score on top of this axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResolutionConfidence {
    Exact,
    Reachable,
    Heuristic,
    /// Reserved for per-candidate include-ambiguity labeling. Part of the
    /// spec-mandated exhaustive taxonomy; no producer yet. R3 surfaces include
    /// ambiguity at the *scope* layer instead — `OpenReason::AmbiguousInclude`
    /// opens the reach scope, and ambiguous includes yield no proven edges,
    /// so wrong twins fall to `Unknown` tier (soft path) without being
    /// mis-colored. Projecting the ambiguity onto this per-candidate label
    /// (goto/completion `detail`) is deferred to R6; the variant stays
    /// reserved meanwhile.
    #[allow(dead_code)]
    Ambiguous,
    Fallback,
}

impl ResolutionConfidence {
    /// Higher rank = higher confidence. `Exact` outranks `Fallback`.
    fn rank(self) -> u8 {
        match self {
            ResolutionConfidence::Exact => 4,
            ResolutionConfidence::Reachable => 3,
            ResolutionConfidence::Heuristic => 2,
            ResolutionConfidence::Ambiguous => 1,
            ResolutionConfidence::Fallback => 0,
        }
    }

    /// Stable lowercase string used in tests and diagnostics. Never localized.
    pub fn as_str(self) -> &'static str {
        match self {
            ResolutionConfidence::Exact => "exact",
            ResolutionConfidence::Reachable => "reachable",
            ResolutionConfidence::Heuristic => "heuristic",
            ResolutionConfidence::Ambiguous => "ambiguous",
            ResolutionConfidence::Fallback => "fallback",
        }
    }
}

impl PartialOrd for ResolutionConfidence {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ResolutionConfidence {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.rank().cmp(&other.rank())
    }
}

/// Why a [`DefinitionCandidate`] appears and ranks where it does. Describes the
/// scope/source evidence (current file, include-reachable, first-layer external,
/// global fallback), *not* a semantic binding claim. Stable, human- and
/// test-readable via [`ResolutionReason::as_str`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResolutionReason {
    CurrentFile,
    ReachableInclude,
    ExternalFirstLayer,
    GlobalFallback,
}

impl ResolutionReason {
    /// Stable lowercase string used in tests and diagnostics. Never localized.
    pub fn as_str(self) -> &'static str {
        match self {
            ResolutionReason::CurrentFile => "current_file",
            ResolutionReason::ReachableInclude => "reachable_include",
            ResolutionReason::ExternalFirstLayer => "external_first_layer",
            ResolutionReason::GlobalFallback => "global_fallback",
        }
    }
}

/// Scope-tier ranking axis for a [`DefinitionCandidate`]: the canonical total
/// order every name→candidate read path (goto, completion, workspace-symbol,
/// coloring) ranks by, kept structurally separate from match quality
/// (`base_match`). This is the **policy** axis — where the candidate lives
/// relative to the current reach context — not a semantic binding: `Current`
/// means "in the current file", not "is the bound definition".
///
/// Total order, strongest evidence first: `Current > Reachable > External >
/// Unknown > Global`. `External > Global` is intentional: a direct include is
/// reachability evidence, while a global workspace symbol has no path from the
/// current file. See `AGENTS.md` resolver rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScopeTier {
    /// Definition in the current file.
    Current,
    /// Workspace file proven in the `#include`-reachable set (the file is in
    /// `ReachScope::files`; the set may still be open, but this file's
    /// reachability is proven by traversal).
    Reachable,
    /// External (toolchain) header that is first-layer directly `#include`d by
    /// the current request origin (`directly_included == true`).
    External,
    /// Reachability is heuristic (`ReachScope::heuristic_files`) or the scope
    /// is open: the candidate is not proven in the exact reachable set, but it
    /// cannot be proven unreachable either. Must not be buried below Global.
    Unknown,
    /// Workspace file proven not reachable (closed scope, not in the reachable
    /// set), or no scope evidence at all (no reach context).
    Global,
}

impl ScopeTier {
    /// Higher rank = higher tier. `Current` (4) outranks `Global` (0). The
    /// ranking axis is total; [`Ord`](trait.Ord.html) is derived from this.
    pub fn rank(self) -> i32 {
        match self {
            ScopeTier::Current => 4,
            ScopeTier::Reachable => 3,
            ScopeTier::External => 2,
            ScopeTier::Unknown => 1,
            ScopeTier::Global => 0,
        }
    }

    /// Stable lowercase string used in tests and diagnostics. Never localized.
    #[allow(dead_code)]
    pub fn as_str(self) -> &'static str {
        match self {
            ScopeTier::Current => "current",
            ScopeTier::Reachable => "reachable",
            ScopeTier::External => "external",
            ScopeTier::Unknown => "unknown",
            ScopeTier::Global => "global",
        }
    }
}

impl PartialOrd for ScopeTier {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ScopeTier {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.rank().cmp(&other.rank())
    }
}

/// A half-open `[start, end)` UTF-16 position range within a source file, in the
/// same units LSP uses. Carried by [`DefinitionCandidate`] so the LSP boundary
/// can construct a `Location` without re-reading the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CandidateRange {
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

/// A best-effort, indexed, jumpable definition/declaration **candidate** —
/// never a compiler-bound semantic symbol. Carries the indexed facts (name,
/// kind, definition-role, repository-relative path, range, workspace/external
/// source) plus the R2 resolver currency: a [`ScopeTier`] (scope policy) and a
/// `base_match` (match quality), plus a [`ResolutionConfidence`] and a single
/// [`ResolutionReason`] derived from the tier via
/// [`crate::resolver::confidence_reason_for`].
///
/// `tier` (policy) and `base_match` (match quality) are kept structurally
/// separate and **never summed into one field by callers** — the resolver packs
/// them into a single sort key via a `TIER_STRIDE` chosen so tier strictly
/// dominates base_match + locality (see `resolver::pack_score`). Locality is a
/// sub-`base_match` tiebreak computed at scoring time, not stored here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionCandidate {
    pub name: String,
    /// Indexed kind string ("function"/"macro"/"type"/...).
    pub kind: String,
    /// Definition role string ("definition" or "declaration").
    pub role: String,
    /// Repository-relative path with `/` separators (absolute for external).
    pub path: String,
    pub range: CandidateRange,
    /// `"workspace"` or `"external"`.
    pub source: String,
    /// Scope policy: where this candidate lives relative to the current reach
    /// context. The single ranking truth; `confidence`/`reason` project from
    /// it. Assigned via [`crate::resolver::scope_tier`].
    pub tier: ScopeTier,
    /// Match-quality score (textual or definitional), kept separate from
    /// `tier`/locality policy. Callers supply it: completion/workspace-symbol
    /// pass the fuzzy `score_match` quality (≤ 1000); goto passes a
    /// definition-preference quality (definition > declaration, function in
    /// `.c` > `.h`). Callers MUST NOT fold tier or locality into this field.
    pub base_match: i32,
    /// Match-quality confidence projected from `tier` + exact-name. Higher
    /// variants outrank lower ones. Derived, not independently assigned.
    pub confidence: ResolutionConfidence,
    /// Why this candidate appears and ranks where it does, projected from
    /// `tier` + exact-name. Describes scope/source evidence, not a semantic
    /// binding. Derived, not independently assigned.
    pub reason: ResolutionReason,
}

/// How confidently a shared candidate set proved its focused answer: `Exact`
/// (unique authoritative target under complete coverage), `Preferred` (unique
/// but coverage was open/truncated/incomplete, so uniqueness is a preference,
/// not a proof), `Ambiguous` (several strongest targets survive), `Fallback`
/// (no authoritative evidence). Surfaced as uncertainty evidence (hover
/// footer, possible-targets coverage, debug logs); never used as a filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CandidateDisposition {
    Exact,
    Preferred,
    Ambiguous,
    Fallback,
}

impl CandidateDisposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Preferred => "preferred",
            Self::Ambiguous => "ambiguous",
            Self::Fallback => "fallback",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SharedCandidateCoverage {
    pub scanned: usize,
    pub truncated: bool,
    pub scope_open: bool,
    pub facts_incomplete: bool,
    pub generation_mismatch: bool,
}

impl SharedCandidateCoverage {
    #[allow(dead_code)]
    pub fn complete(scanned: usize) -> Self {
        Self {
            scanned,
            ..Self::default()
        }
    }

    fn permits_exact(&self) -> bool {
        !self.truncated && !self.scope_open && !self.facts_incomplete && !self.generation_mismatch
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CandidateRef {
    pub group_index: usize,
    pub candidate_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateGroup<T> {
    pub logical_key: Option<crate::semantic_model::LogicalEntityKey>,
    pub declaration_kind: crate::semantic_model::SemanticDeclarationKind,
    pub tier: ScopeTier,
    pub authoritative: bool,
    pub low_fidelity: bool,
    pub candidates: Vec<T>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateSet<T> {
    pub all: Vec<CandidateGroup<T>>,
    pub focused: Vec<CandidateRef>,
    pub coverage: SharedCandidateCoverage,
    pub disposition: CandidateDisposition,
    /// Recalled candidates living in groups outside `focused`, i.e. omitted
    /// from the default presentation by intent and/or tier focus. Alternatives
    /// inside a focused group (e.g. a declaration behind its focused
    /// definition) are presentation views of the same entity and are not
    /// counted.
    pub alternative_count: usize,
}

impl<T> CandidateSet<T> {
    pub fn new(
        all: Vec<CandidateGroup<T>>,
        focused: Vec<CandidateRef>,
        coverage: SharedCandidateCoverage,
    ) -> Self {
        let disposition = classify_candidate_disposition(&all, &focused, &coverage);
        let focused_groups: std::collections::HashSet<usize> = focused
            .iter()
            .map(|candidate_ref| candidate_ref.group_index)
            .collect();
        let alternative_count = all
            .iter()
            .enumerate()
            .filter(|(index, _)| !focused_groups.contains(index))
            .map(|(_, group)| group.candidates.len())
            .sum();
        Self {
            all,
            focused,
            coverage,
            disposition,
            alternative_count,
        }
    }
}

fn classify_candidate_disposition<T>(
    all: &[CandidateGroup<T>],
    focused: &[CandidateRef],
    coverage: &SharedCandidateCoverage,
) -> CandidateDisposition {
    let has_authoritative = all.iter().any(|group| group.authoritative);
    if !has_authoritative {
        return CandidateDisposition::Fallback;
    }

    let mut focused_groups = Vec::new();
    for candidate_ref in focused {
        if !focused_groups.contains(&candidate_ref.group_index) {
            focused_groups.push(candidate_ref.group_index);
        }
    }

    if focused_groups.len() > 1 || focused.len() > 1 {
        return CandidateDisposition::Ambiguous;
    }

    let Some(candidate_ref) = focused.first() else {
        return CandidateDisposition::Fallback;
    };
    let Some(group) = all.get(candidate_ref.group_index) else {
        return CandidateDisposition::Fallback;
    };
    if !group.authoritative || group.low_fidelity {
        return CandidateDisposition::Fallback;
    }

    if coverage.permits_exact() {
        CandidateDisposition::Exact
    } else {
        CandidateDisposition::Preferred
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordCandidate {
    pub id: i64,
    pub display_name: String,
    pub tag_name: Option<String>,
    pub typedef_name: Option<String>,
    pub kind: RecordKind,
    pub path: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
    pub confidence: RecordConfidence,
    pub signature: String,
    pub tier: ScopeTier,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberCandidate {
    pub name: String,
    pub kind: crate::parser::MemberKind,
    pub signature: String,
    pub type_name: Option<String>,
    pub tier: ScopeTier,
    pub confidence: crate::parser::MemberConfidence,
    pub owner_path: String,
    /// Blake3 hash of the exact owner source revision that produced this
    /// member. Completion resolve validates it before reading lazy docs.
    pub owner_revision_hash: Option<String>,
    pub handle: MemberCandidateHandle,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberCandidateHandle {
    pub persistent_id: Option<i64>,
    pub fingerprint: String,
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

impl MemberCandidateHandle {
    pub fn new(
        persistent_id: Option<i64>,
        owner_path: &str,
        record_key: &str,
        member: &crate::semantic_model::MemberDef,
    ) -> Self {
        Self::from_parts(
            persistent_id,
            owner_path,
            record_key,
            &member.name,
            member.kind,
            member.start_byte,
            member.end_byte,
            member.start_line as u32,
            member.start_col as u32,
            member.end_line as u32,
            member.end_col as u32,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn from_parts(
        persistent_id: Option<i64>,
        owner_path: &str,
        _record_key: &str,
        name: &str,
        kind: crate::semantic_model::MemberKind,
        start_byte: usize,
        end_byte: usize,
        start_line: u32,
        start_col: u32,
        end_line: u32,
        end_col: u32,
    ) -> Self {
        let identity = format!(
            "{owner_path}|{name}|{}|{start_byte}|{end_byte}|{start_line}|{start_col}",
            kind.as_str(),
        );
        Self {
            persistent_id,
            fingerprint: blake3::hash(identity.as_bytes()).to_hex().to_string(),
            start_line,
            start_col,
            end_line,
            end_col,
        }
    }
}

/// User-visible best-effort label for a completion candidate (R6). `detail` is a
/// short inline tag shown next to the item; `documentation` is the full
/// `tier` + `confidence` + `reason` shown only when the item is expanded. Both
/// are presentation strings derived from the same `(tier, confidence, reason)`
/// that ranked and deduped the candidate — they cannot disagree with the
/// ranking. This is a best-effort scope label, not a semantic-binding claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionScopeLabel {
    /// Short inline tag: `reachable` / `external` / `global` / `ambiguous`.
    pub detail: &'static str,
    /// Full explanation, e.g. `FossilSense: external candidate (heuristic, external_first_layer)`.
    pub documentation: String,
}

/// Build the [`CompletionScopeLabel`] for a candidate, or `None` for a
/// `Current`-tier candidate. Current-file candidates are the common, obvious
/// case and are intentionally left unlabeled to avoid cluttering the list; every
/// other tier is tagged so a reachable candidate is distinguishable from a
/// global-fallback or ambiguous one. The `detail` tag is derived from the
/// candidate's `confidence` (which is itself a projection of the tier), so it
/// stays consistent with ranking.
pub fn completion_scope_label(
    tier: ScopeTier,
    confidence: ResolutionConfidence,
    reason: ResolutionReason,
) -> Option<CompletionScopeLabel> {
    if tier == ScopeTier::Current {
        return None;
    }
    let detail = match confidence {
        ResolutionConfidence::Reachable => "reachable",
        ResolutionConfidence::Heuristic => "external",
        ResolutionConfidence::Ambiguous => "ambiguous",
        ResolutionConfidence::Fallback => "global",
        // `Exact` is only produced for the `Current` tier (handled above); a
        // non-current `Exact` is not expected, so leave it unlabeled rather
        // than inventing a tag.
        ResolutionConfidence::Exact => return None,
    };
    Some(CompletionScopeLabel {
        detail,
        documentation: format!(
            "FossilSense: {} candidate ({}, {})",
            tier.as_str(),
            confidence.as_str(),
            reason.as_str()
        ),
    })
}

// Re-export the concept anchors as the canonical names. The types stay defined
// in their producing modules (parser/references/reachability); re-exporting
// gives a single canonical name per concept without relocating production
// logic. `Occurrence` is consumed via `model::Occurrence` by coloring; the
// others are the spec-mandated canonical names that R2+ will consume as the
// codebase adopts the vocabulary.
#[allow(unused_imports)]
pub use crate::parser::{Occurrence, RecordConfidence, RecordKind};
#[allow(unused_imports)]
pub use crate::reachability::{OpenReason, ReachScope};
#[allow(unused_imports)]
pub use crate::references::ReferenceHit;

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate_group(
        declaration_kind: crate::semantic_model::SemanticDeclarationKind,
        tier: ScopeTier,
        authoritative: bool,
        low_fidelity: bool,
        candidates: Vec<&'static str>,
    ) -> CandidateGroup<&'static str> {
        CandidateGroup {
            logical_key: None,
            declaration_kind,
            tier,
            authoritative,
            low_fidelity,
            candidates,
        }
    }

    #[test]
    fn confidence_full_ordering_exact_outranks_fallback() {
        // Derived Ord orders variants top-to-bottom: Exact > Reachable >
        // Heuristic > Ambiguous > Fallback.
        assert!(ResolutionConfidence::Exact > ResolutionConfidence::Fallback);
        assert!(ResolutionConfidence::Exact > ResolutionConfidence::Reachable);
        assert!(ResolutionConfidence::Reachable > ResolutionConfidence::Heuristic);
        assert!(ResolutionConfidence::Heuristic > ResolutionConfidence::Ambiguous);
        assert!(ResolutionConfidence::Ambiguous > ResolutionConfidence::Fallback);
    }

    #[test]
    fn confidence_representation_is_stable_and_exhaustive() {
        // Every variant maps to a distinct, stable, lowercase string. The
        // `match` in `as_str` is exhaustive by construction (the compiler
        // rejects a missing variant).
        let confidences = [
            ResolutionConfidence::Exact,
            ResolutionConfidence::Reachable,
            ResolutionConfidence::Heuristic,
            ResolutionConfidence::Ambiguous,
            ResolutionConfidence::Fallback,
        ];
        let strings: Vec<&str> = confidences.iter().map(|c| c.as_str()).collect();
        assert_eq!(
            strings,
            vec!["exact", "reachable", "heuristic", "ambiguous", "fallback"]
        );
        let mut sorted = strings.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            confidences.len(),
            "representations are distinct"
        );
    }

    #[test]
    fn reason_representation_is_stable_and_exhaustive() {
        // Every variant maps to a distinct, stable, lowercase string. The
        // `match` in `as_str` is exhaustive by construction.
        let reasons = [
            ResolutionReason::CurrentFile,
            ResolutionReason::ReachableInclude,
            ResolutionReason::ExternalFirstLayer,
            ResolutionReason::GlobalFallback,
        ];
        let strings: Vec<&str> = reasons.iter().map(|r| r.as_str()).collect();
        assert_eq!(
            strings,
            vec![
                "current_file",
                "reachable_include",
                "external_first_layer",
                "global_fallback"
            ]
        );
        let mut sorted = strings.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), reasons.len(), "representations are distinct");
    }

    #[test]
    fn scope_tier_full_ordering_current_outranks_global() {
        // Derived Ord orders variants top-to-bottom:
        // Current > Reachable > External > Unknown > Global.
        assert!(ScopeTier::Current > ScopeTier::Global);
        assert!(ScopeTier::Current > ScopeTier::Reachable);
        assert!(ScopeTier::Reachable > ScopeTier::External);
        // The contentious edge — direct include > no path. Pinned here so a
        // future re-order has to update the spec/test together.
        assert!(ScopeTier::External > ScopeTier::Unknown);
        assert!(ScopeTier::Unknown > ScopeTier::Global);
    }

    #[test]
    fn scope_tier_rank_is_total_and_consistent_with_ord() {
        // Every pair of distinct tiers has a deterministic Ord matching rank().
        let tiers = [
            ScopeTier::Current,
            ScopeTier::Reachable,
            ScopeTier::External,
            ScopeTier::Unknown,
            ScopeTier::Global,
        ];
        for a in tiers {
            for b in tiers {
                let ord = a.cmp(&b);
                assert_eq!(ord, a.rank().cmp(&b.rank()));
                assert_eq!(a == b, a.rank() == b.rank());
            }
        }
        // Rank is the documented 4..=0 range.
        assert_eq!(ScopeTier::Current.rank(), 4);
        assert_eq!(ScopeTier::Global.rank(), 0);
    }

    #[test]
    fn scope_tier_representation_is_stable_distinct_and_exhaustive() {
        let tiers = [
            ScopeTier::Current,
            ScopeTier::Reachable,
            ScopeTier::External,
            ScopeTier::Unknown,
            ScopeTier::Global,
        ];
        let strings: Vec<&str> = tiers.iter().map(|t| t.as_str()).collect();
        assert_eq!(
            strings,
            vec!["current", "reachable", "external", "unknown", "global"]
        );
        let mut sorted = strings.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), tiers.len(), "representations are distinct");
    }

    // --- R6: completion_scope_label --------------------------------------

    #[test]
    fn completion_label_tags_each_non_current_tier() {
        // The label is built from (tier, confidence, reason); use the resolver
        // projection so the test exercises the same mapping production uses.
        use crate::reachability::OpenReason;
        use crate::resolver::confidence_reason_for;

        // Reachable → "reachable".
        let (c, r) = confidence_reason_for(ScopeTier::Reachable, false, None);
        let label = completion_scope_label(ScopeTier::Reachable, c, r).expect("labeled");
        assert_eq!(label.detail, "reachable");
        assert!(label.documentation.contains("reachable"));

        // External → "external".
        let (c, r) = confidence_reason_for(ScopeTier::External, false, None);
        assert_eq!(
            completion_scope_label(ScopeTier::External, c, r)
                .unwrap()
                .detail,
            "external"
        );

        // Global → "global".
        let (c, r) = confidence_reason_for(ScopeTier::Global, false, None);
        assert_eq!(
            completion_scope_label(ScopeTier::Global, c, r)
                .unwrap()
                .detail,
            "global"
        );

        // Unknown under ambiguous include → "ambiguous".
        let (c, r) = confidence_reason_for(
            ScopeTier::Unknown,
            false,
            Some(OpenReason::AmbiguousInclude),
        );
        assert_eq!(
            completion_scope_label(ScopeTier::Unknown, c, r)
                .unwrap()
                .detail,
            "ambiguous"
        );

        // Unknown under any other open cause → "global" (plain fallback).
        let (c, r) = confidence_reason_for(
            ScopeTier::Unknown,
            false,
            Some(OpenReason::UnresolvedInclude),
        );
        assert_eq!(
            completion_scope_label(ScopeTier::Unknown, c, r)
                .unwrap()
                .detail,
            "global"
        );
    }

    #[test]
    fn completion_label_is_none_for_current_tier() {
        use crate::resolver::confidence_reason_for;
        // Current + exact and Current + non-exact are both unlabeled.
        let (c, r) = confidence_reason_for(ScopeTier::Current, true, None);
        assert!(completion_scope_label(ScopeTier::Current, c, r).is_none());
        let (c, r) = confidence_reason_for(ScopeTier::Current, false, None);
        assert!(completion_scope_label(ScopeTier::Current, c, r).is_none());
    }

    #[test]
    fn completion_label_documentation_names_tier_confidence_reason() {
        use crate::resolver::confidence_reason_for;
        let (c, r) = confidence_reason_for(ScopeTier::External, false, None);
        let doc = completion_scope_label(ScopeTier::External, c, r)
            .unwrap()
            .documentation;
        // Documentation carries the full triple so an expanded item explains
        // exactly why the candidate appeared and ranked where it did.
        assert!(doc.contains("external"));
        assert!(doc.contains("heuristic"));
        assert!(doc.contains("external_first_layer"));
    }

    #[test]
    fn candidate_set_classifies_complete_unique_authoritative_target_as_exact() {
        let set = CandidateSet::new(
            vec![candidate_group(
                crate::semantic_model::SemanticDeclarationKind::Function,
                ScopeTier::Reachable,
                true,
                false,
                vec!["decl"],
            )],
            vec![CandidateRef {
                group_index: 0,
                candidate_index: 0,
            }],
            SharedCandidateCoverage::complete(1),
        );

        assert_eq!(set.disposition, CandidateDisposition::Exact);
        assert_eq!(set.alternative_count, 0);
        assert_eq!(set.disposition.as_str(), "exact");
    }

    #[test]
    fn candidate_set_classifies_open_or_incomplete_unique_target_as_preferred() {
        let set = CandidateSet::new(
            vec![candidate_group(
                crate::semantic_model::SemanticDeclarationKind::Object,
                ScopeTier::Reachable,
                true,
                false,
                vec!["object"],
            )],
            vec![CandidateRef {
                group_index: 0,
                candidate_index: 0,
            }],
            SharedCandidateCoverage {
                scanned: 1,
                scope_open: true,
                ..SharedCandidateCoverage::default()
            },
        );

        assert_eq!(set.disposition, CandidateDisposition::Preferred);
        assert_eq!(set.disposition.as_str(), "preferred");
    }

    #[test]
    fn candidate_set_classifies_multiple_strongest_targets_as_ambiguous() {
        let set = CandidateSet::new(
            vec![
                candidate_group(
                    crate::semantic_model::SemanticDeclarationKind::Method,
                    ScopeTier::Reachable,
                    true,
                    false,
                    vec!["method"],
                ),
                candidate_group(
                    crate::semantic_model::SemanticDeclarationKind::Function,
                    ScopeTier::Reachable,
                    true,
                    false,
                    vec!["free"],
                ),
            ],
            vec![
                CandidateRef {
                    group_index: 0,
                    candidate_index: 0,
                },
                CandidateRef {
                    group_index: 1,
                    candidate_index: 0,
                },
            ],
            SharedCandidateCoverage::complete(2),
        );

        assert_eq!(set.disposition, CandidateDisposition::Ambiguous);
        assert_eq!(set.disposition.as_str(), "ambiguous");
    }

    #[test]
    fn candidate_set_classifies_only_low_fidelity_evidence_as_fallback() {
        let set = CandidateSet::new(
            vec![candidate_group(
                crate::semantic_model::SemanticDeclarationKind::Macro,
                ScopeTier::Global,
                false,
                true,
                vec!["fallback"],
            )],
            vec![CandidateRef {
                group_index: 0,
                candidate_index: 0,
            }],
            SharedCandidateCoverage {
                scanned: 1,
                facts_incomplete: true,
                ..SharedCandidateCoverage::default()
            },
        );

        assert_eq!(set.disposition, CandidateDisposition::Fallback);
        assert_eq!(set.disposition.as_str(), "fallback");
    }

    #[test]
    fn candidate_group_envelope_is_kind_neutral() {
        let kinds = [
            crate::semantic_model::SemanticDeclarationKind::Function,
            crate::semantic_model::SemanticDeclarationKind::Method,
            crate::semantic_model::SemanticDeclarationKind::Object,
            crate::semantic_model::SemanticDeclarationKind::Type,
            crate::semantic_model::SemanticDeclarationKind::Alias,
            crate::semantic_model::SemanticDeclarationKind::EnumConstant,
            crate::semantic_model::SemanticDeclarationKind::Macro,
        ];
        let groups: Vec<_> = kinds
            .iter()
            .copied()
            .map(|kind| candidate_group(kind, ScopeTier::Global, true, false, vec!["candidate"]))
            .collect();
        let set = CandidateSet::new(groups, Vec::new(), SharedCandidateCoverage::complete(7));

        let actual: Vec<_> = set.all.iter().map(|group| group.declaration_kind).collect();
        assert_eq!(actual, kinds);
        assert_eq!(set.alternative_count, kinds.len());
    }

    #[test]
    fn alternative_count_counts_suppressed_groups_not_same_entity_views() {
        // Group 0 (focused): one entity presented through two physical rows
        // (definition + declaration). Group 1 (suppressed lower tier): three
        // same-name rows. Only the suppressed group's rows are alternatives.
        let set = CandidateSet::new(
            vec![
                candidate_group(
                    crate::semantic_model::SemanticDeclarationKind::Object,
                    ScopeTier::Reachable,
                    true,
                    false,
                    vec!["definition", "declaration"],
                ),
                candidate_group(
                    crate::semantic_model::SemanticDeclarationKind::Object,
                    ScopeTier::Global,
                    true,
                    false,
                    vec!["distant_a", "distant_b", "distant_c"],
                ),
            ],
            vec![CandidateRef {
                group_index: 0,
                candidate_index: 0,
            }],
            SharedCandidateCoverage::complete(5),
        );

        assert_eq!(set.alternative_count, 3);
        assert_eq!(set.disposition, CandidateDisposition::Exact);
    }
}
