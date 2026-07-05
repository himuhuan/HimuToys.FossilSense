# Smart Completion Phase 2-3 Requirements

Status: implemented-and-verified
Date: 2026-07-05
Feature brief name: smart-completion-phase2-3

## 1. 需求来源和背景

- User request, 2026-07-05: advance the long-term next-version plan using `docs/research/smart-completion-dev-eval.md`, and complete Phase 2-3.
- User instruction, 2026-07-05: use `himupowers:brainstorming` to determine requirements before implementation.
- User decision, 2026-07-05: selected `smart-completion-phase2-3` as the feature brief name.
- User decision, 2026-07-05: approved the Phase 2-3 scope as ordinary identifier completion only: evidence-aware deterministic ranker plus current-file overlay expansion; intent classifier, include ranking, member methods, local history, and ML are excluded from this phase.
- `AGENTS.md` delegates project instructions to `CLAUDE.md`.
- `CLAUDE.md` positions FossilSense as a best-effort C/C++ navigation and analysis tool for large Windows workspaces without reliable compile environments. This phase must not require clangd, compile commands, ctags, compiler invocations, or a build-system model.
- Before this Phase 2-3 implementation, `CLAUDE.md` recorded v1.2.0 Phase 0-1 rules: ordinary identifier completion passed through `completion` core, displayed ranking remained strict resolver-packed compatibility ranking, logs were source-safe summaries, and soft scope prior, intent classifier, multi-channel recall, include ranking, member method schema, local history, and ML were outside Phase 0-1.
- `docs/smart-completion-v1-2/requirements.md` and `docs/smart-completion-v1-2/plans/2026-07-05--implementation-plan.md` show Phase 0-1 is implemented and verified: candidate metadata, compatible dedup/rank/truncate, source metrics, timing summary, and shadow rank comparison exist in `crates/fossilsense/src/completion.rs`.
- `docs/research/smart-completion-dev-eval.md` defines Phase 2 as deterministic evidence-aware ranking with soft scope prior, guard band, local binding priority, bounded local-word uplift, and short labels; it defines Phase 3 as current-file ephemeral overlay expansion for macro definitions, typedef/using/type aliases, enum constants, current-file functions, current-file records/types, nearby identifier frequency/distance, and raw word fallback.
- Current code facts: `crates/fossilsense/src/completion.rs` has a compatible pipeline with single-source `CandidateEvidence`; `crates/fossilsense/src/server/language_server.rs` gathers indexed `NameTable` hits, current-function local bindings, and current-file raw words before calling the pipeline; `crates/fossilsense/src/query/local_completion.rs` covers parameters and prior local variables; `crates/fossilsense/src/parser.rs` already exposes symbols, records, type aliases, enum constants, local bindings, and parse fallback diagnostics through `FileSemanticIndex`.

## 2. 用户需求

- UR1: Ordinary identifier completion should move beyond Phase 0-1 compatibility ranking and use a deterministic, explainable evidence-aware ranker.
- UR2: Scope tier remains an input evidence source, but ordinary completion should use a soft scope prior with guard bands instead of strict resolver-packed score as the final ordering rule.
- UR3: Current-function local bindings continue to be highly prioritized and keep their existing behavior.
- UR4: Current open document facts beyond locals, including macros, aliases, enum constants, functions, record/type definitions, and nearby identifier usage, should participate as structured overlay evidence when available.
- UR5: Same-name candidates from indexed symbols, local bindings, current-file overlay, and raw words should merge evidence into one user-visible item rather than discarding useful provenance.
- UR6: Raw local words remain fallback text suggestions and must not be presented as semantic current-file definitions.
- UR7: Completion details and debug/perf summaries should explain why candidates rise in ranking without leaking candidate names or source snippets by default.
- UR8: Include-path completion, member completion, short-prefix noise gates, completion memo narrowing, and `CompletionList.isIncomplete = true` remain compatible.

## 3. 范围与非范围

In scope:

- Add a deterministic completion ranker in the existing `completion` core, with centralized weights, guard-band rules, and unit tests.
- Upgrade candidate evidence from a single winning source toward merged evidence across indexed symbols, local bindings, current-file overlay facts, and raw word fallback.
- Keep `resolver::scope_tier` and `resolver::confidence_reason_for` as the canonical scope/confidence/reason projection, while no longer using `resolver::pack_score` as the ordinary completion final ranking rule.
- Extend current-file overlay using the already parsed open document: macro definitions, typedef/using/type aliases, enum constants, current-file function declarations/definitions, record/type definitions, and nearby identifier usage frequency/distance.
- Render short, restrained detail tags such as `local`, `current`, `reachable`, `external`, `ambiguous`, `global`, and `text`; fuller evidence remains in documentation or debug output.
- Preserve source-safe perf/debug summaries using counts, timings, score classes, and shadow summary fields rather than raw names.
- Update `CLAUDE.md`, `README.md`, and `extensions/vscode/README.md` so documented completion rules match the implementation.
- Add focused Rust tests for ranker ordering, guard bands, evidence merge, overlay extraction, fallback behavior, and server integration.

Out of scope:

- Lightweight intent classifier Phase 4, including TypeName, ExpressionValue, CallTarget, MacroPreprocessor, and DeclarationName classification.
- Multi-channel recall quotas, larger internal candidate pools, and new `NameTable` recall APIs.
- Include completion sibling/recent/frequency ranking.
- Member methods, member schema migration, weak receiver inference, inheritance, overload, template, namespace, and access-control semantics.
- Local history personalization, anonymous telemetry, ML reranker, LLM completion, or auto include insertion.
- SQLite schema migration, new user-facing settings, and per-keystroke workspace scans or SQLite queries.

## 4. 场景与预期

| ID | Role | Trigger | Preconditions | Action | Expected result |
|---|---|---|---|---|---|
| SC1 | C/C++ developer | Completing a current function local | Cursor is inside a function body and a matching parameter or prior local declaration exists | Request ordinary completion | The local binding appears near the top, keeps variable/parameter kind and detail, and is not displaced by weak global/text matches. |
| SC2 | C/C++ developer | Completing an unsaved macro or type in the current file | Open document contains a new `#define`, typedef/using alias, enum constant, function, or record/type definition not yet indexed | Request ordinary completion with a matching prefix | The current-file fact appears as a structured candidate, marked as current/local evidence, without requiring a saved file or reindex. |
| SC3 | C/C++ developer | Same name exists in index and current file | Indexed candidate and overlay candidate share a label | Request ordinary completion | One visible item is returned with merged evidence; semantic kind/detail is preserved when stronger than raw text fallback. |
| SC4 | C/C++ developer | Reachable prefix candidate competes with global exact/fuzzy candidate | Include reachability graph is available | Request ordinary completion | Reachable candidates retain a strong prior; global/text candidates only rise above them when guard-band rules find strong current/local evidence. |
| SC5 | C/C++ developer | Short prefix completion | Prefix length is one or two characters | Request ordinary completion | Existing short-prefix noise gates remain effective; ordinary substring and subsequence long-tail noise does not flood the result list. |
| SC6 | Maintainer | Debugging ranking quality | Perf logging is enabled | Request ordinary completion | Logs show stage timings, source/evidence counts, ranker class summaries, guard-band summary, and shadow movement without raw candidate labels. |
| SC7 | C/C++ developer | Include or member completion | Cursor is in `#include`, `.`, or `->` context | Request completion | Existing include/member completion paths still short-circuit before ordinary identifier ranker. |

## 5. 功能性需求

- FR1 Ranker module: the existing `completion` core must expose a deterministic evidence-aware ranker with centralized weight constants and tests.
- FR2 Soft scope prior: ordinary identifier completion final order must use scope tier as a weighted evidence feature rather than strict `pack_score` ordering.
- FR3 Guard bands: the ranker must prevent low-confidence global/text candidates from outranking higher-trust reachable/current candidates unless they carry strong current-file, local-binding, or exact-match evidence.
- FR4 Local binding priority: current-function parameters and prior local variables must keep a strong priority and existing rendering behavior.
- FR5 Evidence merge: same-label candidates from indexed, local binding, overlay, and raw-word sources must merge evidence into a single candidate before final ranking.
- FR6 Overlay facts: current open document macro definitions, typedef/using/type aliases, enum constants, function declarations/definitions, and record/type definitions must be eligible for ordinary identifier completion when their names match the prefix.
- FR7 Nearby usage evidence: current-file identifier usage frequency and distance may raise raw-word fallback within bounded limits, while keeping raw words classified as text/fallback evidence.
- FR8 Fallback resilience: parse fallback or missing AST facts must still return indexed and raw-word candidates, and ordinary completion must remain incomplete.
- FR9 User-visible labels: non-current or uncertain candidates must keep concise detail/documentation labels for tier/confidence/reason; raw words must be distinguishable as text fallback.
- FR10 Source-safe observability: perf/debug summary must include ranker stage timing, evidence/source counts, guard-band counts, and shadow-rank movement without default raw labels or source snippets.
- FR11 Surface compatibility: include-path and member completion must continue to bypass ordinary ranker logic.
- FR12 Documentation sync: `CLAUDE.md`, root `README.md`, and extension `README.md` must describe Phase 2-3 behavior, limitations, confidence/fallback semantics, and excluded capabilities.

## 6. 非功能性需求

- NFR1 Best-effort honesty: docs, labels, and tests must avoid implying compiler-grade semantic binding.
- NFR2 Large-workspace hot path: the ranker and overlay must use already available in-memory `NameTable`, open-document parse cache, local word cache, and current document text; no new per-keystroke workspace scan or SQLite query is allowed.
- NFR3 Determinism: equal inputs must produce stable output ordering, with deterministic tie-breakers.
- NFR4 Testability: ranker, merge, overlay extraction, and score explanation must be testable without constructing a tower-lsp server.
- NFR5 Compatibility: existing include, member, local binding, completion memo, parser fallback, query scoping, and server completion tests must keep passing after intentional assertion updates.
- NFR6 Privacy: default logs must avoid raw candidate names and source snippets.
- NFR7 Maintainability: new weights and guard thresholds must live in one completion-owned configuration or constants block, not scattered in LSP handlers.
- NFR8 Documentation consistency: no current docs may continue to say ordinary completion displayed ranking is strict compatibility mode after Phase 2 is enabled.

## 7. 技术方案

### Recommended: evolve the existing completion core

Extend `crates/fossilsense/src/completion.rs` from a compatible pipeline into an evidence-aware completion core. Candidate evidence becomes mergeable, the ranker computes a deterministic final score from scope, confidence, source, text match, local/overlay evidence, and fallback penalties, and guard bands run before final ordering. `server/language_server.rs` continues to gather snapshots, prefix, reach scope, local bindings, local words, and indexed hits, then delegates merge/rank/truncate to the completion core. Current-file overlay extraction uses `FileSemanticIndex` from the live parse cache so unsaved facts can participate without index writes.

Trade-off: this reuses Phase 0-1 structure and keeps the server hot path bounded, but requires careful updates to tests that currently assert strict-tier ordering.

### Alternative A: add soft score adjustments in `server/language_server.rs`

This is faster to code, but it scatters ranking rules across LSP plumbing, bypasses the Phase 0-1 module boundary, and makes future Phase 4/5 work harder to test. It also increases the risk of concept drift warned about in `CLAUDE.md`.

### Alternative B: change `resolver::pack_score` globally

This would make all consumers share the soft ranker immediately, but it would unintentionally affect goto definition, semantic coloring, workspace symbols, and member completion. The current project model needs ordinary completion to migrate first while other surfaces keep their established semantics.

### Design decisions

- D1: Use the existing `completion` core as the only place for ordinary completion merge, rank, guard, metrics, and summary formatting.
- D2: Keep `ScopeTier`, `ResolutionConfidence`, and `ResolutionReason` as shared vocabulary; do not create parallel semantic tiers.
- D3: Limit Phase 2-3 to ordinary identifier completion.
- D4: Use open-document parser facts for overlay, with raw words as fallback when parser facts are absent.
- D5: Keep raw-word fallback bounded and visibly textual.
- D6: Keep `resolver::pack_score` available for strict-policy surfaces and compatibility tests that are not ordinary completion final ranking.

## 8. 需求跟踪矩阵

| Requirement | Sources | Scenarios | Design decisions | Plan tasks | Test/validation | Status |
|---|---|---|---|---|---|---|
| FR1 | UR1, eval Phase 2 | SC1-SC6 | D1 | Task 1 | `cargo test -p fossilsense completion::tests::ranker` | 已验证 |
| FR2 | UR2, eval Phase 2, `CLAUDE.md` migration note | SC4 | D1, D2, D6 | Task 1 | Ranker tests showing soft prior can differ from strict packed score | 已验证 |
| FR3 | UR2, UR6, eval risk section | SC4, SC5 | D1, D5 | Task 1 | Guard-band tests for global/text versus reachable/current candidates | 已验证 |
| FR4 | UR3, Phase 0-1 behavior | SC1 | D1, D4 | Task 2 | Existing and updated local binding server tests | 已验证 |
| FR5 | UR5, eval Phase 3 duplicate risk | SC3 | D1, D2 | Task 1, Task 3 | Evidence merge tests for indexed plus overlay plus raw word | 已验证 |
| FR6 | UR4, eval Phase 3 | SC2 | D4 | Task 2 | Overlay extraction tests for macro, alias, enum constant, function, record/type | 已验证 |
| FR7 | UR4, UR6, eval Phase 3 | SC5 | D4, D5 | Task 2 | Nearby usage scoring tests with bounded raw-word uplift | 已验证 |
| FR8 | UR8, parser fallback rules | SC2, SC5 | D4, D5 | Task 3 | Parse fallback server/pipeline tests; `isIncomplete = true` checks | 已验证 |
| FR9 | UR7, `CLAUDE.md` user-visible labels | SC1-SC6 | D2, D5 | Task 3 | Completion item detail/documentation assertions | 已验证 |
| FR10 | UR7, eval observability risks | SC6 | D1, D5 | Task 1, Task 3 | Summary tests proving no raw names/source snippets | 已验证 |
| FR11 | UR8, current server surface gating | SC7 | D3 | Task 3 | Include/member tests keep passing | 已验证 |
| FR12 | UR1-UR8, docs consistency rules | SC1-SC7 | D1-D6 | Task 4 | `rg -n "strict resolver-packed|soft scope prior|Phase 2-3" README.md extensions/vscode/README.md CLAUDE.md` | 已验证 |
| NFR1 | `CLAUDE.md` candidate-not-binding rule | SC1-SC7 | D2, D5 | Task 4 | Documentation wording review | 已验证 |
| NFR2 | `CLAUDE.md` hot path rule | SC1-SC7 | D1, D4 | Task 3, Task 5 | Code review plus `cargo test -p fossilsense`; no new store opens in ordinary completion | 已验证 |
| NFR3 | UX stability risk | SC4-SC6 | D1 | Task 1 | Deterministic tie-breaker tests | 已验证 |
| NFR4 | Maintainability rules | SC6 | D1 | Task 1, Task 2 | Pure unit tests without LSP service | 已验证 |
| NFR5 | Existing behavior | SC5, SC7 | D3, D6 | Task 5 | `cargo test -p fossilsense` | 已验证 |
| NFR6 | Privacy rules | SC6 | D1 | Task 1, Task 3 | Source-safe summary tests | 已验证 |
| NFR7 | `CLAUDE.md` magic-score warning | SC1-SC6 | D1 | Task 1 | Weight constants confined to completion core | 已验证 |
| NFR8 | Docs consistency | SC1-SC7 | D6 | Task 4 | Documentation grep and review | 已验证 |

## 9. 风险与缓解

| Risk | Impact | Likelihood | Mitigation | Trigger |
|---|---|---|---|---|
| R1 Soft ranker makes ordering feel less explainable | Users may distrust why a global/text candidate rises | Medium | Guard bands, concise labels, source-safe debug summaries, and tests for reasoned reversals | A lower-tier candidate outranks a reachable/current candidate |
| R2 Existing strict-tier tests fail broadly | Implementation cost grows | Medium | Update only ordinary-completion final-ranking assertions; preserve resolver strict tests for non-completion policy | Tests mention strict `pack_score` as ordinary completion invariant |
| R3 Overlay duplicates indexed candidates | Confusing duplicate labels or lost semantic kind | Medium | Evidence merge before ranking; tests for same-name indexed plus overlay plus raw word | One label appears twice in completion list |
| R4 Overlay overstates unsaved facts | Candidate looks compiler-precise | Medium | Mark overlay as current/local evidence, keep confidence language best-effort, and avoid claiming binding | Detail/documentation implies exact semantic resolution |
| R5 Nearby raw words become noisy | Short prefixes regress | Medium | Preserve short-prefix gates and cap raw-word uplift below structured evidence unless guard bands allow it | One- or two-character completion shows long-tail text noise |
| R6 Hot path latency regresses | Completion feels slow in large workspaces | Medium | Use live parse cache and current local word cache; no new per-key DB or workspace scan; retain limit caps | Perf summary shows rank/overlay stages dominate |
| R7 Logs leak proprietary names | Privacy and trust issue | Low | Default summaries use counts/classes only; optional named dumps are not part of this phase | Perf/debug line contains raw candidate labels |

## 10. 用户确认记录

- 2026-07-05: User requested Phase 2-3 advancement based on `docs/research/smart-completion-dev-eval.md`.
- 2026-07-05: User required `himupowers:brainstorming` for requirement determination.
- 2026-07-05: User selected `smart-completion-phase2-3` as the feature brief name.
- 2026-07-05: User approved the design scope: ordinary identifier completion evidence-aware deterministic ranker plus current-file overlay expansion; intent classifier, include ranking, member methods, local history, and ML excluded from this phase.
