# v1.2.2 Architecture Follow-ups

Status: current follow-up record for the v1.2.2 architecture health release.

This page is the stable link for Final Review scope notes and deferred architecture candidates from OpenSpec change `plan-healthy-v122-architecture-refactor`.

## Final Review Record

Date: 2026-07-06

Reviewed artifacts:

- `openspec/changes/plan-healthy-v122-architecture-refactor/proposal.md`
- `openspec/changes/plan-healthy-v122-architecture-refactor/design.md`
- `openspec/changes/plan-healthy-v122-architecture-refactor/specs/**/*.md`
- `openspec/changes/plan-healthy-v122-architecture-refactor/tasks.md`
- `docs/research/healthy-fossilsense-dev-eval.md`
- `docs/architecture/README.md`
- `dist/DELIVERY-NOTE-1.2.2.md`

OpenSpec status was re-run with:

```powershell
openspec status --change plan-healthy-v122-architecture-refactor --json
```

Result: schema `spec-driven`; required artifacts `proposal`, `design`, `specs`, and `tasks` are valid with status `done`.

Scope review conclusion: the required v1.2.2 work is limited to Phase A, Phase B, Phase C, Phase D, and Phase H from `docs/research/healthy-fossilsense-dev-eval.md`.

The release remains behavior-preserving. No Final Review finding expands v1.2.2 to include optional Phase E, Phase F, or Phase G work.

## Follow-up Candidates

These items are recorded as follow-up candidate work only. They are not required v1.2.2 release blockers and should be proposed as separate OpenSpec changes before implementation.

| Candidate | Source phase | Reason deferred |
| --- | --- | --- |
| `IndexStore` small facade and read-model builder contracts | Phase E | Useful store/persistence cleanup, but not required for the A/B/C/D/H behavior-preserving architecture health release. |
| Parser facts semantic transition for persistent/request facts and fact availability | Phase F | Higher blast radius across parser, indexer, query, coloring, references, and member completion; better suited after v1.2.2 guardrails are in place. |
| Include/reachability policy consolidation | Phase G | Valuable policy cleanup, but scope-sensitive because open scope and ambiguity must remain soft-ranking/fallback signals. |

Before any follow-up starts, keep the same guardrails:

- do not describe best-effort candidates as compile-accurate bindings;
- keep ordinary completion ranking and presentation compatibility unless a new change explicitly accepts a behavior change;
- avoid per-keystroke SQLite, full workspace scans, or unbounded parsing on hot request paths;
- update release notes and architecture docs if a future version intentionally changes user-visible behavior.
