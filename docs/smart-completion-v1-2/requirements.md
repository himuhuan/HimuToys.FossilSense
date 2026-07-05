# Smart Completion v1.2 Requirements

Status: implemented-and-verified
Date: 2026-07-05
Feature brief name: smart-completion-v1-2

## 1. 需求来源和背景

- User request, 2026-07-05: advance the long-term next-version plan, bump the project to `v1.2.0`, use `docs/research/smart-completion-dev-eval.md`, and complete Phase 0-1.
- User decision, 2026-07-05: selected `smart-completion-v1-2` as the feature brief name.
- `AGENTS.md` delegates project instructions to `CLAUDE.md`.
- `CLAUDE.md` positions FossilSense as a best-effort C/C++ navigation and analysis tool for large Windows workspaces without reliable compile environments. Smart completion must not require clangd, compile commands, ctags, a compiler, or a build-system model.
- `CLAUDE.md` requires completion to keep `CompletionList.isIncomplete = true`, avoid disk IO on the per-keystroke hot path, expose confidence/fallback/ambiguity honestly, and avoid parallel `smart` or `semantic` models that bypass `ScopeTier`, `ResolutionConfidence`, `ResolutionReason`, `NameTable`, or shared resolver concepts.
- `docs/research/smart-completion-dev-eval.md` recommends Phase 0 as completion observability and evaluation baseline, and Phase 1 as a protocol-agnostic completion pipeline refactor that first preserves old ranking behavior.
- Current code already includes function-local completion candidates, raw local words, indexed `NameTable` hits, completion memo narrowing, and include/member short-circuit paths. Phase 0-1 must build on that implementation rather than re-implement the previous local-completion work.
- Current code logs coarse completion perf only as total time, prefix, and memo hit type. It does not expose per-source counts, dedup/rank counts, or a shadow-rank comparison hook.

## 2. 用户需求

- UR1: The next version facts move consistently to `v1.2.0`.
- UR2: Maintainers can see completion pipeline source counts and stage timing summaries under existing debug/perf logging.
- UR3: Ordinary identifier completion logic is moved toward a protocol-agnostic pipeline boundary without changing user-visible ranking in Phase 1.
- UR4: The first pipeline supports evidence-style candidate metadata and same-name deduplication while preserving the current source priority: local binding, indexed symbol, then raw local word.
- UR5: A shadow-ranking comparison hook exists so later Phase 2 rankers can be evaluated without changing displayed results.
- UR6: Include path completion, member completion, short-prefix behavior, local binding behavior, raw word fallback, memo narrowing, and `isIncomplete = true` remain compatible.

## 3. 范围与非范围

In scope:

- Bump Rust crate and VS Code extension package versions to `1.2.0`, and synchronize README version facts.
- Add a focused `completion` core module for ordinary identifier completion candidate metadata, compatible dedup/ranking, metrics, timing summary formatting, and shadow rank comparison.
- Keep the current strict resolver-packed score behavior for displayed ordinary completion ranking.
- Log structured completion summaries through the existing perf logging gate without candidate names or source text.
- Update documentation to state that v1.2.0 starts the smart-completion groundwork with observable, compatible pipeline refactoring.
- Add focused Rust tests for the pipeline, metrics, shadow comparison, and existing server completion behavior.

Out of scope:

- Phase 2 soft scope prior, evidence-aware weighted ranker, guard-band policy, or changed candidate ordering.
- Phase 3 current-file overlay expansion beyond the existing function-local candidates and raw words.
- Phase 4 intent classifier.
- Multi-channel recall quotas, include ranking enhancements, member method indexing, local history, anonymous telemetry, ML ranking, auto include insertion, or full C++ semantic inference.
- SQLite schema migration or per-keystroke workspace scans/disk queries.
- New user settings.

## 4. 场景与预期

| ID | Role | Trigger | Preconditions | Action | Expected result |
|---|---|---|---|---|---|
| SC1 | Maintainer | Preparing v1.2.0 | Repository is on the release branch | Inspect version facts | Rust crate, extension package, and README facts say `1.2.0`. |
| SC2 | Maintainer | Debugging completion quality | Perf logging is enabled | Request ordinary identifier completion | Logs include total/context/recall/merge-rank/render timing and source counts, without candidate names. |
| SC3 | Maintainer | Refactoring completion | Existing completion candidates include indexed, local binding, and local word sources | Run pipeline unit tests | Compatible ranker preserves current score sorting and same-name source priority. |
| SC4 | Maintainer | Preparing Phase 2 | A future ranker is developed | Compare display and shadow order | Shadow comparison reports moved candidates and max rank delta without affecting returned items. |
| SC5 | C/C++ developer | Requests include or member completion | Cursor is in `#include`, `.`, or `->` context | Request completion | Existing include/member paths still short-circuit before ordinary pipeline. |
| SC6 | C/C++ developer | Requests ordinary completion | Current function has local bindings and current-file words | Request completion | Existing local binding, indexed, and raw word behavior remains compatible and list stays incomplete. |

## 5. 功能性需求

- FR1 Version bump: Rust crate, VS Code extension package, and README version facts must move to `1.2.0`.
- FR2 Completion core module: ordinary completion candidate source, evidence, compatible ranking, metrics, timing summary, and shadow comparison must live in a focused module outside `server/language_server.rs`.
- FR3 Compatible ranking: Phase 1 displayed ranking must preserve current behavior: dedup by source priority and sort by descending score then name.
- FR4 Candidate source metrics: the pipeline must count indexed, local binding, and local word candidates before and after dedup/ranking.
- FR5 Shadow comparison: the codebase must expose a deterministic helper that compares displayed order against a shadow order and reports moved count and max rank delta.
- FR6 Structured perf summary: ordinary completion perf logs must include stage timing fields, memo hit kind, source counts, candidate counts, and shadow summary, while avoiding candidate labels/source code.
- FR7 Hot-path compatibility: the refactor must not add per-keystroke SQLite queries, workspace scans, or new external dependencies.
- FR8 Surface gating: include-path and member completion must continue to return before ordinary identifier completion.
- FR9 Incomplete list behavior: ordinary completion responses must remain `isIncomplete = true`, including empty and truncated responses.
- FR10 Documentation sync: repository documentation must describe v1.2.0 as smart-completion groundwork and explicitly state Phase 0-1 does not change ranking semantics yet.

## 6. 非功能性需求

- NFR1 Best-effort honesty: docs and logs must describe evidence, fallback, and compatibility without claiming compiler-grade semantic binding.
- NFR2 Privacy and source safety: debug/perf output must not dump raw candidate names or source snippets by default.
- NFR3 Maintainability: completion candidate merge/rank helpers must be unit-testable without constructing a tower-lsp server.
- NFR4 Compatibility: existing Rust tests for completion, parser, query, include, member, reference, and indexing behavior must keep passing.
- NFR5 Large-workspace safety: the compatible pipeline must operate on already-recalled in-memory candidates and current open-document facts.
- NFR6 Release readiness: the version bump must be verifiable by Rust tests and extension compile; VSIX packaging remains the release gate for an external publish.

## 7. 技术方案

### Recommended: compatible completion core extraction

Create a small `crates/fossilsense/src/completion.rs` module that owns candidate metadata, source priority, compatible dedup/ranking, source-count metrics, timing summary formatting, and shadow rank comparison. The LSP server continues to render `CompletionItem`s and collect indexed/local/local-word candidates, then hands them to the module for merge/rank/truncate. The first rank policy is compatibility-only, so user-visible order should remain stable.

Trade-off: this does not deliver Phase 2 ranking improvements yet, but it creates the tested insertion point needed to evaluate them safely.

### Alternative A: keep metrics inside `server/language_server.rs`

This is quicker but keeps ranking, logging, and LSP request plumbing tangled in the large handler. It also makes Phase 2 harder to unit-test and contradicts the research recommendation.

### Alternative B: implement soft evidence ranker now

This has higher user-visible upside, but it changes core ordering before Phase 0 observability and Phase 1 compatibility are proven. It is explicitly deferred.

### Design decisions

- D1: Use compatible completion core extraction.
- D2: Keep displayed ordinary completion ranking unchanged in this phase.
- D3: Use existing perf logging gate for completion summaries.
- D4: Keep logs source-safe by default: counts and timings only, no candidate names.
- D5: Treat shadow comparison as infrastructure for later rankers; this phase compares compatible orders unless a later ranker is supplied.

## 8. 需求跟踪矩阵

| Requirement | Sources | Scenarios | Design decisions | Plan tasks | Test/validation | Status |
|---|---|---|---|---|---|---|
| FR1 | UR1, user request | SC1 | D1 | Task 1 | `rg -n "1\\.2\\.0" README.md crates/fossilsense/Cargo.toml extensions/vscode/package.json` | 已计划 |
| FR2 | UR3, eval Phase 1 | SC3 | D1 | Task 2 | `cargo test -p fossilsense completion::tests` | 已计划 |
| FR3 | UR3, UR6, `CLAUDE.md` | SC3, SC6 | D2 | Task 2, Task 3 | `cargo test -p fossilsense completion::tests::compatible_pipeline_preserves_score_order_and_source_priority` | 已计划 |
| FR4 | UR2 | SC2 | D3, D4 | Task 2, Task 3 | `cargo test -p fossilsense completion::tests::pipeline_metrics_count_sources_before_and_after_dedup` | 已计划 |
| FR5 | UR5, eval Phase 0 | SC4 | D5 | Task 2 | `cargo test -p fossilsense completion::tests::shadow_comparison_reports_rank_movement` | 已计划 |
| FR6 | UR2 | SC2 | D3, D4 | Task 3 | `cargo test -p fossilsense completion::tests::completion_perf_summary_omits_candidate_names` | 已计划 |
| FR7 | `CLAUDE.md` hot path | SC2, SC6 | D1, D2 | Task 3 | Code review plus `cargo test -p fossilsense server::tests::local_binding_pipeline_uses_open_document_bindings_before_local_words` | 已计划 |
| FR8 | UR6 | SC5 | D2 | Task 3 | Existing include/member completion tests and `cargo test -p fossilsense server::tests` | 已计划 |
| FR9 | `CLAUDE.md` completion rules | SC6 | D2 | Task 3 | Existing server completion tests; `cargo test -p fossilsense` | 已计划 |
| FR10 | UR6, `CLAUDE.md` docs | SC1-SC6 | D1-D5 | Task 4 | `rg -n "Phase 0-1|v1.2.0|smart completion" README.md extensions/vscode/README.md CLAUDE.md` | 已计划 |
| NFR1 | `CLAUDE.md` candidate-not-binding rule | SC2-SC6 | D2, D4 | Task 4 | Documentation wording review | 已计划 |
| NFR2 | eval debug risk | SC2 | D4 | Task 2, Task 3 | `completion_perf_summary_omits_candidate_names` | 已计划 |
| NFR3 | `CLAUDE.md` maintainability | SC3 | D1 | Task 2 | `cargo test -p fossilsense completion::tests` | 已计划 |
| NFR4 | Existing behavior | SC5, SC6 | D2 | Task 5 | `cargo test -p fossilsense` | 已计划 |
| NFR5 | `CLAUDE.md` large workspace | SC2, SC6 | D1, D2 | Task 3 | Code review for no new workspace IO | 已计划 |
| NFR6 | User request, release rules | SC1 | D1 | Task 5 | `cargo test -p fossilsense`; `cd extensions/vscode && pnpm run compile` | 已计划 |

## 9. 风险与缓解

| Risk | Impact | Likelihood | Mitigation | Trigger |
|---|---|---|---|---|
| R1 Refactor accidentally changes ranking | Completion regression | Medium | Compatibility ranker tests assert old score/name ordering and source priority | Same-name or equal-score candidate order changes |
| R2 Logs leak source identifiers | Privacy and trust issue | Medium | Summary logs include counts and timings only; tests assert candidate labels are absent | Perf logging enabled in proprietary workspace |
| R3 Phase 0-1 is mistaken for full smart completion | Over-promising | Medium | README/CLAUDE wording states this is groundwork and Phase 2+ is deferred | Release notes or docs imply soft ranking |
| R4 More instrumentation adds latency | Completion feels slower | Low | Reuse existing timers and in-memory counts; no candidate serialization | p95 completion latency regresses |
| R5 Module extraction creates duplicate concepts | Concept drift | Medium | Keep names generic and tied to existing `ScopeTier` / `ResolutionConfidence` / source priority | New module invents independent semantic tiers |

## 10. 用户确认记录

- 2026-07-05: User requested long-term next-version advancement, `v1.2.0` version bump, and completion of Phase 0-1 from `docs/research/smart-completion-dev-eval.md`.
- 2026-07-05: User explicitly required the `himupowers:brainstorming` route to determine requirements.
- 2026-07-05: User selected `smart-completion-v1-2` as the feature brief name.
- 2026-07-05: Phase 0-1 was implemented and verified with Rust tests, mini-c index smoke, VS Code compile, and VSIX packaging.
