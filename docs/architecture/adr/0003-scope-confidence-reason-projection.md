# ADR 0003: Scope, Confidence, And Reason Projection

Status: Accepted

## Context

FossilSense ranking depends on scope evidence, include reachability, locality, match quality, ambiguity, and fallback state. Older or ad hoc scoring risks making heuristic results look more precise than they are.

The canonical projection lives in the shared model/resolver concepts rather than in feature-specific magic scoring.

## Decision

Scope, confidence, and reason SHALL be projected through the canonical model/resolver layer. `ScopeTier` describes the source tier, `ResolutionConfidence` describes certainty, and `ResolutionReason` describes why the candidate is ranked that way.

Heuristic candidates are not semantic binding results. A candidate produced by name match, text fallback, weak receiver inference, current-file overlay, or open-scope recall must retain labels that reveal its confidence and fallback state.

Ambiguity is a first-class signal. Ambiguous includes and unresolved includes can create open scope; open scope changes ranking/fallback behavior but must not be described as proof that all global candidates are reachable.

## Consequences

Navigation, completion, references, coloring, hover, and signature help can share explanations. Future refactors should move logic toward shared projection, not duplicate one-off reason labels.

Some current modules still mix presentation with query logic. That is acceptable for the v1.2.2 baseline, but Phase B fitness checks and later refactoring should make these boundaries visible.
