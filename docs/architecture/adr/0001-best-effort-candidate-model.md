# ADR 0001: Best-Effort Candidate Model

Status: Accepted

## Context

FossilSense targets C/C++ workspaces that often lack reliable `compile_commands.json`, macro configuration, include roots, or platform-specific build context. In that environment, navigation, completion, references, coloring, hover, and signature help can provide useful ranked candidates but cannot honestly claim compiler-grade semantic binding.

The codebase already has canonical concepts for this: `DefinitionCandidate`, `ScopeTier`, `ResolutionConfidence`, `ResolutionReason`, `ReachScope`, `OpenReason`, `Occurrence`, `ReferenceHit`, `RecordCandidate`, and record/member/alias facts. These concepts describe evidence, ranking, syntax roles, and fallback state.

## Decision

FossilSense returns best-effort candidates. It MUST NOT describe heuristic name matches as compile-accurate semantic bindings.

Candidate-producing features SHALL reuse the canonical model/resolver concepts:

- `DefinitionCandidate` for ranked definition-like targets.
- `ScopeTier` for current, reachable, external, unknown, global, and related scope ordering.
- `ResolutionConfidence` for exact, reachable, heuristic, ambiguous, and fallback confidence.
- `ResolutionReason` for why a candidate ranked as it did.
- `ReachScope` and `OpenReason` for include reachability and open scope.
- `Occurrence` and syntactic roles for parsed identifier appearances.
- `ReferenceHit` for reference results with role and range.
- `RecordCandidate` and record/member/alias facts for degraded member and type evidence.

When evidence is incomplete, FossilSense SHALL expose confidence, fallback, ambiguity, and open scope rather than hide uncertainty. New code must not introduce a parallel "semantic" model that bypasses these concepts.

## Consequences

Maintainers can refactor internals without changing the product promise. Users see ranked, explainable candidates. Tests and documentation must verify labels and fallback semantics, not only that a non-empty result exists.

The trade-off is that some labels are intentionally cautious. That is correct for FossilSense: a candidate can be useful without being a compiler binding.
