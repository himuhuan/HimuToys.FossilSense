# ADR 0005: v1.2.2 Behavior-Preserving Architecture Health Release

Status: Accepted

## Context

The v1.2.2 work is motivated by maintainability: server/cache/completion boundaries are too implicit, and future changes need guardrails. The product behavior is already defined by v1.2.1 documentation, tests, and `CLAUDE.md`.

## Decision

v1.2.2 is a behavior-preserving architecture health release. It MUST NOT intentionally change navigation, completion, coloring, references, configuration, privacy, or VSIX packaging behavior.

Allowed work includes architecture documentation, ADRs, risk tracking, regression checklists, import inventory, fitness functions, internal state boundaries, ordinary completion service extraction, compatibility tests, and release hardening.

Not included: complete C++ semantics, ML ranking, telemetry, cloud sync, auto include insertion, new runtime dependencies, broad directory reshuffles for churn, or user-visible ranking policy changes.

## Consequences

Every implementation phase must prove behavior preservation with targeted tests or checklists. If a later phase discovers unavoidable behavior drift, it must stop and update the change artifacts instead of silently widening v1.2.2.

The release still must produce a self-contained VSIX through the normal packaging flow.
