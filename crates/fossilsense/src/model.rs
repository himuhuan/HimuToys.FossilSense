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
/// Unknown > Global`. See `openspec/changes/candidate-resolver/design.md` D2
/// for the rationale, including the contentious `External > Global` edge (a
/// direct include is reachability evidence; a global workspace symbol has no
/// path from the current file).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScopeTier {
    /// Definition in the current file.
    Current,
    /// Workspace file proven in the `#include`-reachable set (the file is in
    /// `ReachScope::files`; the set may still be open, but this file's
    /// reachability is proven by traversal).
    Reachable,
    /// External (toolchain) header that is first-layer directly `#include`d by
    /// a workspace file (`directly_included == true`).
    External,
    /// Reachability indeterminate because the scope is open: the candidate is
    /// not proven in the reachable set, but it cannot be proven unreachable
    /// either. Must not be buried below a `Global` candidate.
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
    pub tier: ScopeTier,
    pub confidence: crate::parser::MemberConfidence,
    pub owner_path: String,
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
}
