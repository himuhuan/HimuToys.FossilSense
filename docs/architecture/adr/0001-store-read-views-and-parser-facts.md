# ADR 0001: Store Read Views and Parser Fact Projections

Status: accepted

## Context

`IndexStore` historically exposed a broad query surface, and several read-model
builders consumed tuple-shaped data whose meaning depended on SQL column order.
`FileSemanticIndex` historically mixed persistent facts, request-time facts,
skipped groups, and lexical fallback in one compatibility structure.

The goal of the v1.2.2 store/parser contract work is to make those boundaries
explicit while preserving user-visible behavior.

## Decision

Use small store read views under `store::views` instead of introducing one broad
store trait or changing SQLite connection ownership. Read views return typed
rows or existing FossilSense domain candidates.

Keep `FileSemanticIndex` as the compatibility shape and add borrowed
projections:

- `persistent_facts()` for index-time facts.
- `request_facts()` for live request facts.
- `fact_availability()` for requested, skipped, and fallback-unavailable groups.

## Consequences

- Feature code can migrate away from SQL-shaped records without schema churn.
- `model` and `resolver` remain the canonical candidate/ranking language.
- Existing parser fields remain available while production consumers move to
  projections with focused tests.
- Empty vectors no longer have to mean three different things; availability
  metadata distinguishes available-empty, not-requested, and fallback.
- Compatibility wrappers and legacy field access may remain when they are
  intentionally tested or used as parity oracles.

## Non-Decisions

- No SQLite schema migration.
- No parser algorithm change.
- No completion ranking, definition ranking, include policy, reference role, or
  member completion behavior change.
- No new runtime dependency.
