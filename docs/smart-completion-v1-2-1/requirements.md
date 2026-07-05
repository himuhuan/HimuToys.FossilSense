# Smart Completion v1.2.1 Phase 7-8 Requirements

Status: implemented-and-verified
Date: 2026-07-05
Feature brief name: smart-completion-v1-2-1

## 1. 需求来源和背景

- User request, 2026-07-05: advance the next long-term version plan, bump the project to `v1.2.1`, use `docs/research/smart-completion-dev-eval.md`, and complete Phase 7-8 because prior phases were completed in v1.2.0 and smoke-tested.
- User instruction, 2026-07-05: use `himupowers:brainstorming` before determining requirements or implementation.
- User decision, 2026-07-05: selected `smart-completion-v1-2-1` as the feature brief name.
- User decision, 2026-07-05: approved 方案 A: Phase 7 member evidence plus Phase 8 local completion history, with local history defaulting to `auto` and providing disable/clear controls.
- User clarification, 2026-07-05: this can be a large refactor, and startup full index rebuild is acceptable. v1.2.1 does not need complex compatibility migration for old index data.
- `AGENTS.md` delegates repository instructions to `CLAUDE.md`.
- `CLAUDE.md` positions FossilSense as a best-effort C/C++ navigation and analysis tool for large Windows workspaces without reliable compile environments. v1.2.1 must not require clangd, compile commands, ctags, compiler invocation, macro expansion, or a build-system model.
- `CLAUDE.md` records v1.2.0 Phase 0-6 completion rules: ordinary identifier completion uses the completion core, deterministic evidence-aware ranking, current-file overlay, intent evidence, multi-channel recall, include ranking, and source-safe perf summaries. It explicitly keeps member method schema, weak receiver inference, local history, auto include insertion, ML, and telemetry outside Phase 4-6.
- `README.md` and `extensions/vscode/README.md` currently describe v1.2.0 and state that method-member completion and local history are not enabled. v1.2.1 must update these facts without contradicting archived Phase 0-6 requirements.
- `docs/research/smart-completion-dev-eval.md` defines Phase 7 as extending member completion to member evidence, starting with schema/parser/store support for methods, then weak receiver inference. It defines Phase 8 as local-only completion history using accepted-count/intent/prefix statistics, clearable and disableable, with no upload.
- Pre-implementation code facts captured before v1.2.1 work:
  - `crates/fossilsense/src/server/member_completion.rs` is field-only. It resolves a simple receiver record from current-file local declarations, queries `record_defs` plus `fields`, and falls back to prefix-matched global field names.
  - `crates/fossilsense/src/store/schema.rs` uses `SCHEMA_VERSION = 8`, `record_defs`, `fields`, and `type_aliases`; there is no `members` table.
  - `crates/fossilsense/src/parser.rs` and `crates/fossilsense/src/parser/ast.rs` expose `RecordDef`, `FieldDef`, aliases, record-typed local declarations, and local bindings. They do not expose method members.
  - `crates/fossilsense/src/store.rs` migrates schema mismatch by dropping data tables and recreating current schema, which matches the accepted full-rebuild approach.
  - `crates/fossilsense/src/completion.rs` already centralizes ordinary completion evidence, intent, ranker, guard bands, metrics, and source-safe summary.
  - `crates/fossilsense/src/server/language_server.rs` already registers LSP `executeCommand` and routes include/member completion before ordinary identifier completion.
  - `extensions/vscode/src/extension.ts` already sends LSP execute-command requests for FossilSense commands but does not currently record completion accept events.
- VS Code Extension API documentation states that a `CompletionItem.command` can execute after the completion is inserted. There is no repository-local evidence of a general accepted-completion event. Phase 8 therefore uses completion-item commands as best-effort positive feedback rather than pretending to observe every user intention.
- Recent commits show Phase 0-6 landed in order: `b776d0a`, `d6c8a59`, `fc31eb3`, `4cbdf00`, `b6e801b`, `1d94106`, `2d67f06`, `78e814d`, and `54ac864`.

## 2. 用户需求

- UR1: Project version facts must move consistently from `1.2.0` to `1.2.1`.
- UR2: Member completion after `.` and `->` should no longer be limited to fields when indexed C++ class/struct methods are available.
- UR3: Resolved receiver member completion should present fields and methods together, with appropriate LSP kinds and best-effort scope/confidence labels.
- UR4: FossilSense must remain honest that member candidates are owner-scoped best-effort evidence, not compiler-grade method binding.
- UR5: Phase 7 should support an initial method/member schema and parser path without implementing inheritance, overload resolution, templates, namespaces, access control, or full expression typing.
- UR6: Weak receiver inference should improve common cases only when confidence can be represented; it must not turn arbitrary expressions into false exact owners.
- UR7: Member fallback should stay bounded, prefix-only, and incomplete; adding methods must not dump all indexed methods for empty or one-character prefixes.
- UR8: Ordinary identifier completion should use local-only accepted-completion history as a bounded ranking signal.
- UR9: Local history must be clearable, disableable, workspace-local, and private by default; it must not upload telemetry or print raw accepted candidate labels in normal logs.
- UR10: Disabling local history must return completion ordering to the deterministic v1.2.0 evidence-aware ranker, aside from unrelated Phase 7 member changes.
- UR11: Full index rebuild at startup or first v1.2.1 launch is acceptable for the member schema change.
- UR12: Documentation must explain the exact v1.2.1 capability boundary: member evidence and local history exist, but complete C++ semantics, telemetry, ML ranker, and auto include insertion do not.

## 3. 范围与非范围

In scope:

- Bump Rust crate version, VS Code extension version, README version facts, and release wording to `1.2.1`.
- Add a canonical member model that reuses existing record/scope vocabulary and can represent at least `Field`, `Method`, `StaticMethod`, and `NestedType` as best-effort member evidence.
- Add a schema version bump and a clean member storage model. Because full rebuild is accepted, implementation may drop old indexed data and rebuild instead of migrating old `fields` rows in place.
- Preserve or intentionally replace the existing `fields` path with a unified member query, as long as current field behavior remains covered by tests.
- Extend parser AST collection for C++ class/struct body method declarations and definitions in the first self-consistent subset.
- Support low-confidence owner association for simple out-of-class `Owner::method` definitions only when the owner name can be extracted without type inference.
- Extend member completion rendering so resolved receivers can return fields and methods with stable ordering, LSP kind, concise detail, and full documentation carrying tier/confidence/reason.
- Keep fallback member completion prefix-only, capped, and incomplete. The fallback may include method names only after a two-character prefix and must label fallback uncertainty.
- Add narrow weak receiver inference:
  - highest confidence: existing explicit local/parameter record declarations.
  - medium confidence: pointer/reference variants of explicit declarations already represented by the local declaration type text.
  - low confidence: simple receiver name and unique indexed record/typedef correlation where ambiguity can be labeled.
- Add local completion history as local-only positive feedback using completion-item accept commands.
- Store local history in a workspace-specific local cache, not in project source and not in the main symbol index schema unless the design explicitly separates it from source-derived facts.
- Add configuration for local history mode with `auto`, `on`, and `off`, defaulting to `auto`.
- Add a command to clear local completion history.
- Integrate local history into the ordinary completion ranker as bounded evidence keyed by workspace, candidate kind/key, prefix bucket, and intent. The boost must be smaller than high-confidence current/local semantic evidence.
- Add source-safe observability: counts and bucket summaries are allowed; raw accepted names, source snippets, and include/member paths are not logged by default.
- Add focused Rust and TypeScript tests for member parsing/storage/query/rendering, weak receiver inference, local history command plumbing, ranking boost bounds, disable/clear behavior, and docs consistency.
- Produce a v1.2.1 VSIX package during release verification.

Out of scope:

- Full C++ semantic type inference, macro expansion, overload resolution, template instantiation, namespaces, using-directive lookup, inherited members, virtual dispatch, access control, operator overloads, and data-flow receiver typing.
- Auto include insertion or any source edit triggered by accepting a completion item.
- Anonymous telemetry, remote analytics, cloud sync, ML ranker, LLM completion, or model distribution.
- Negative feedback from undo/delete/edit churn. Phase 8 uses positive accept evidence only.
- User-visible rank-weight tuning knobs.
- Per-keystroke SQLite queries, workspace scans, compile steps, or external include tree scans outside existing cached index/include behavior.
- Preserving old v1.2.0 index database contents across the schema version bump. v1.2.1 can rebuild.

## 4. 场景与预期

| ID | Role | Trigger | Preconditions | Action | Expected result |
|---|---|---|---|---|---|
| SC1 | Maintainer | Preparing v1.2.1 | Repository currently reports `1.2.0` in crate, extension, and docs | Inspect version facts | Rust crate, VS Code extension, README, extension README, and release wording report `1.2.1`. |
| SC2 | C++ developer | Completing a resolved receiver | Indexed header contains `struct Widget { int width; void resize(); };`; current file has `Widget *w` before cursor | Request completion after `w->r` | `resize` appears as a method candidate and `width` remains available as a field candidate; both carry owner/scope evidence. |
| SC3 | C developer | Completing existing field scenario | Indexed C struct contains fields and no methods | Request completion after `point.` | Existing field completion behavior remains available and tested under the unified member path. |
| SC4 | C++ developer | Completing a class method | Indexed class body declares `void start(); static int count();` | Request completion after a resolved object receiver | Instance method and static-like method evidence can render with method-like kinds; labels avoid implying overload or access-control resolution. |
| SC5 | C++ developer | Out-of-class method definition exists | `class Widget { void resize(); }; void Widget::resize() {}` is indexed | Request completion after a resolved `Widget` receiver | `resize` can appear once with merged or deduplicated member evidence; low-confidence owner association is labeled if only the out-of-class form was observed. |
| SC6 | C/C++ developer | Receiver cannot be inferred | Cursor is after `make_widget()->` or another unsupported expression | Type a two-character member prefix | Fallback returns capped prefix matches, marks the list incomplete, and labels uncertainty; empty or one-character prefixes return empty incomplete. |
| SC7 | C/C++ developer | Weak receiver name correlation is unique | A receiver named `widget` appears, and the index has a unique `Widget` record with matching normalized name | Request member completion | Weak receiver fallback may suggest that record's members with low confidence; ambiguous correlations do not become exact owners. |
| SC8 | C/C++ developer | Accepting an ordinary completion | Local history mode is `auto` or `on`; a FossilSense completion item carries an accept command | Accept the item | A local positive feedback record is stored for the workspace without uploading data or logging the raw candidate label. |
| SC9 | C/C++ developer | Repeating a similar completion | Local history has prior accepted evidence for the same candidate/prefix/intent bucket | Request ordinary completion again | The accepted candidate receives a bounded boost and may rise among comparable candidates; high-confidence current/local semantic candidates remain protected. |
| SC10 | Privacy-conscious user | Disabling or clearing history | Local history has stored accept evidence | Set mode `off` or run clear command | Ranking returns to deterministic non-history behavior, and clear removes local history for the workspace. |
| SC11 | Maintainer | First v1.2.1 launch with old index | Existing DB has schema version 8 | Start server or rebuild index | Store detects schema mismatch, drops old indexed data, recreates current schema, and performs a full rebuild instead of trying to preserve old member rows. |
| SC12 | Maintainer | Debugging completion quality | Perf logging is enabled | Request ordinary or member completion | Logs show counts, buckets, and timing summaries without candidate labels, accepted names, source snippets, or paths by default. |

## 5. 功能性需求

- FR1 Version bump: Rust crate, VS Code extension package, root README, extension README, and release/package wording must move to `1.2.1`.
- FR2 Member model: the codebase must expose a canonical member evidence model with member kind, owner record id/key, name, signature, source range, confidence, and scope tier.
- FR3 Schema rebuild: the index schema must move beyond version 8 and support full rebuild on mismatch; old indexed data may be dropped.
- FR4 Field compatibility: existing field extraction, storage, record/alias resolution, and member completion tests must keep passing through the new or unified member path.
- FR5 Method parsing: parser AST collection must extract first-version C++ class/struct body method declarations and definitions into member evidence.
- FR6 Out-of-class method subset: parser/indexer may associate simple `Owner::method` definitions with an owner record when the owner text can be extracted; this association must carry lower confidence than in-body members.
- FR7 Store queries: store must provide owner-scoped member queries and fallback member-name queries that return member kind and best available owner scope evidence.
- FR8 Member rendering: `.` / `->` completion must render fields and methods with appropriate LSP kinds, detail/documentation, deterministic sort, and `CompletionList.isIncomplete` semantics.
- FR9 Member fallback guard: fallback member completion must require at least a two-character prefix, use prefix matching only, cap results by `COMPLETION_LIMIT`, and stay incomplete when owner is not resolved.
- FR10 Weak receiver inference: implementation must add only narrow weak receiver inference with explicit confidence labels and ambiguity handling.
- FR11 Ordinary completion isolation: member fields and methods must not leak into ordinary identifier completion unless they are already valid top-level indexed symbols by existing rules.
- FR12 Local history accept signal: ordinary completion items must be able to record a local positive feedback event after insertion using a VS Code/LSP command path.
- FR13 History storage: local history must be workspace-specific, local-only, bounded in size, decayable or aging-aware, and separate from source-derived index facts.
- FR14 History ranking: ordinary completion ranker must consume history evidence as a bounded boost keyed by candidate, prefix bucket, intent, and kind.
- FR15 History controls: VS Code extension must expose local history configuration with `auto`, `on`, and `off`, plus a user command to clear history.
- FR16 Disable behavior: with local history disabled or cleared, ordinary completion ranking must match the deterministic v1.2.0 ranker for non-member paths under equal inputs.
- FR17 Source-safe metrics: completion/member/history perf summaries must use counts and bucket names, not raw candidate labels, accepted names, source snippets, or paths by default.
- FR18 Documentation sync: `CLAUDE.md`, `README.md`, `extensions/vscode/README.md`, and `extensions/vscode/package.json` descriptions must describe v1.2.1 capabilities, limits, fallbacks, privacy behavior, and non-goals.
- FR19 Package verification: release verification must produce an installable `dist/fossilsense-vscode-1.2.1_BUILD*.vsix` that bundles the native binary.

## 6. 非功能性需求

- NFR1 Best-effort honesty: method/member candidates, weak receiver inference, and history ranking must be described as evidence, not semantic binding.
- NFR2 Large-workspace hot path: completion requests must use in-memory tables, live parse cache, and bounded local history lookups; no per-keystroke workspace scan or symbol-index SQLite query is allowed.
- NFR3 Determinism: equal source-derived inputs and equal local-history state must produce stable ordering through deterministic tie-breakers.
- NFR4 Privacy: local history must remain local, user-clearable, and off-switchable. Default logs must not expose accepted candidate names.
- NFR5 Maintainability: member parsing, store queries, member rendering, history storage, history ranking, and command plumbing must be testable in focused modules without full end-to-end manual VS Code testing.
- NFR6 Compatibility by rebuild: schema incompatibility can force a full index rebuild; implementation must fail visibly if rebuild or derived table construction fails rather than pretending the index is ready.
- NFR7 Concept stability: new member and history evidence must reuse existing `ScopeTier`, `ResolutionConfidence`, `ResolutionReason`, parser facts, and completion evidence vocabulary; no parallel semantic tier system is allowed.
- NFR8 Bounded influence: local history boost must not override high-confidence current/local bindings or strong member owner evidence by itself.
- NFR9 User control: disabling completion still disables member/history participation; disabling history must not disable the rest of completion.
- NFR10 Documentation consistency: current docs must not continue to say member methods or local history are not enabled after v1.2.1 is implemented.

## 7. 技术方案

### Recommended: clean member model plus local-only accept history

Use the accepted full-rebuild allowance to introduce a clean member evidence model. Add a schema version bump and either add a `members` table beside `fields` or replace `fields` with a unified `members` table while keeping field compatibility tests. The recommended implementation is a unified member query surface backed by a clean table that stores owner record id, member name, kind, signature, range, and confidence. Existing record/alias resolution continues to produce owner record candidates; member completion then queries members by owner ids and ranks by owner tier plus member kind/match evidence.

Parser work stays deliberately narrow. In-body class/struct/union fields remain supported, and C++ in-body method declarations/definitions are collected as method members. Simple out-of-class `Owner::method` definitions may be associated with a record by textual owner when unambiguous, but they are not treated as proof of overload, namespace, access, template, or inheritance semantics.

Local history uses `CompletionItem.command` on ordinary identifier completions to report accept events to the extension/server command path after insertion. The history store is local, workspace-specific, bounded, clearable, and excluded from normal source-safe logs. The completion ranker receives history as another evidence feature and applies a small capped boost after source, scope, intent, match, and guard-band logic.

Trade-off: this is a larger schema/model refactor than patching method names into the old `fields` path, but it gives Phase 7 a coherent foundation and avoids accumulating false C++ semantics. It also lets Phase 8 integrate into the existing completion evidence model instead of creating a separate ranking system.

### Alternative A: keep `fields` and bolt methods onto it

This minimizes schema churn but makes method kind, owner confidence, fallback behavior, and future member kinds awkward. It also risks preserving old field-only assumptions in function names and tests. Because startup full rebuild is acceptable, this is not preferred.

### Alternative B: implement Phase 7 only and leave history disabled

This reduces privacy and command-plumbing risk, but it does not complete Phase 8 as requested. It also leaves the completion ranker without a controlled history signal even though v1.2.0 already has the evidence pipeline needed to consume one.

### Alternative C: use anonymous telemetry or ML-style history

This is rejected for v1.2.1. It conflicts with the local-only privacy requirement, requires product and infrastructure decisions outside this repository, and is unnecessary for a first local feedback loop.

### Design decisions

- D1: v1.2.1 uses a schema version bump and permits full rebuild rather than complex compatibility migration.
- D2: Member evidence is a first-class model, not a string-only extension of field fallback.
- D3: Field behavior remains compatible through tests even if storage is unified behind `members`.
- D4: Method support is limited to extractable class/struct body methods plus a simple, lower-confidence out-of-class subset.
- D5: Weak receiver inference is confidence-labeled and declines ambiguous cases.
- D6: Member fallback remains prefix-only, capped, incomplete, and gated by minimum prefix length.
- D7: Local history records only positive accept evidence from completion-item commands.
- D8: Local history is bounded, local-only, clearable, disableable, and source-safe in logs.
- D9: History is rank evidence with capped influence, not a separate personalization ranker.
- D10: Documentation and package metadata must move coherently to `1.2.1`.

## 8. 需求跟踪矩阵

| Requirement | Sources | Scenarios | Design decisions | Plan tasks | Test/validation | Status |
|---|---|---|---|---|---|---|
| FR1 | UR1, user request | SC1 | D10 | Task 1, Task 8, Task 9 | `rg -n -F "1.2.1" crates/fossilsense/Cargo.toml extensions/vscode/package.json README.md extensions/vscode/README.md CLAUDE.md`; `pnpm run package` | 已验证 |
| FR2 | UR2-UR5, eval Phase 7 | SC2-SC5 | D2 | Task 2, Task 3, Task 4 | `cargo test -p fossilsense store::tests::members -- --nocapture`; `cargo test -p fossilsense parser::tests -- --nocapture` | 已验证 |
| FR3 | UR11, user full-rebuild clarification | SC11 | D1 | Task 2, Task 9 | `cargo test -p fossilsense store::tests::resilience_schema -- --nocapture`; mini-c index smoke | 已验证 |
| FR4 | UR2, existing field behavior | SC3 | D3 | Task 2, Task 3, Task 4, Task 5 | `cargo test -p fossilsense store::tests::members -- --nocapture`; `cargo test -p fossilsense server::tests -- --nocapture` | 已验证 |
| FR5 | UR2, UR5, eval Phase 7 | SC2, SC4 | D4 | Task 3 | `cargo test -p fossilsense parser::tests -- --nocapture` | 已验证 |
| FR6 | UR5, UR6, eval Phase 7 | SC5 | D4, D5 | Task 3, Task 4 | parser/store tests for simple `Owner::method` lower-confidence association | 已验证 |
| FR7 | UR2-UR7 | SC2-SC7 | D2, D5, D6 | Task 4 | store owner-scoped and fallback member query tests | 已验证 |
| FR8 | UR2-UR4 | SC2-SC6 | D2, D6 | Task 5 | server member completion tests for FIELD/METHOD kinds and labels | 已验证 |
| FR9 | UR7 | SC6 | D6 | Task 5 | fallback prefix/cap/incomplete tests | 已验证 |
| FR10 | UR6 | SC7 | D5 | Task 5 | weak receiver inference tests for unique/ambiguous cases and short-prefix gating | 已验证 |
| FR11 | UR7, `CLAUDE.md` ordinary completion rules | SC2-SC6 | D2, D6 | Task 4, Task 5 | ordinary completion tests proving members do not leak into identifier completion | 已验证 |
| FR12 | UR8, VS Code API docs | SC8 | D7 | Task 6, Task 7 | TypeScript command plumbing tests and LSP execute-command tests | 已验证 |
| FR13 | UR9 | SC8-SC10 | D8 | Task 6 | history tests for multi-root workspace keying, invalid hash rejection, bounds, and clear | 已验证 |
| FR14 | UR8, UR10 | SC9, SC10 | D8, D9 | Task 7 | ranker tests for bounded history boost and disabled parity | 已验证 |
| FR15 | UR9 | SC10 | D8 | Task 1, Task 6 | extension config/command tests | 已验证 |
| FR16 | UR10 | SC10 | D8, D9 | Task 7 | deterministic no-history parity tests | 已验证 |
| FR17 | UR9, privacy rules | SC8-SC12 | D8 | Task 5, Task 6, Task 7, Task 8 | source-safe summary tests and docs grep | 已验证 |
| FR18 | UR12, docs consistency rules | SC1-SC12 | D10 | Task 8 | docs grep for v1.2.1, member evidence, local history, non-goals | 已验证 |
| FR19 | release hard rule in `CLAUDE.md` | SC1, SC11 | D10 | Task 9 | `pnpm run package` creates v1.2.1 VSIX with bundled binary | 已验证 |
| NFR1 | `CLAUDE.md` candidate-not-binding rule | SC2-SC7 | D2, D4, D5 | Task 5, Task 8 | wording review and tests for confidence labels | 已验证 |
| NFR2 | hot path rules | SC2-SC10 | D6, D8, D9 | Task 5, Task 6, Task 7 | code review plus ordinary-completion history no-open test | 已验证 |
| NFR3 | UX stability | SC2-SC10 | D6, D9 | Task 5, Task 7 | deterministic sorting tests | 已验证 |
| NFR4 | UR9 | SC8-SC10, SC12 | D8 | Task 6, Task 7, Task 8 | clear/disable tests and source-safe log tests | 已验证 |
| NFR5 | maintainability rules | SC2-SC12 | D2-D9 | Task 2, Task 3, Task 4, Task 5, Task 6, Task 7 | focused parser/store/server/extension tests | 已验证 |
| NFR6 | user full-rebuild clarification, `CLAUDE.md` ready-state rule | SC11 | D1 | Task 2, Task 9 | schema rebuild and mini-c index smoke | 已验证 |
| NFR7 | `CLAUDE.md` model stability rule | SC2-SC10 | D2, D9 | Task 2, Task 3, Task 4, Task 7, Task 8 | code review for reused model vocabulary | 已验证 |
| NFR8 | UR10 | SC9, SC10 | D9 | Task 7 | history boost cannot beat protected current/local evidence tests | 已验证 |
| NFR9 | settings compatibility | SC10 | D8 | Task 1, Task 6 | config tests for completion mode vs history mode | 已验证 |
| NFR10 | docs consistency | SC1-SC12 | D10 | Task 8 | docs grep and review | 已验证 |

## 9. 风险与缓解

| Risk | Impact | Likelihood | Mitigation | Trigger |
|---|---|---|---|---|
| R1 Member methods are mistaken for full C++ semantics | Users overtrust candidates | High | Labels, docs, confidence, and tests state owner evidence only; no inherited/overload/template/access claims | UI or docs imply exact C++ binding |
| R2 Schema refactor breaks existing field completion | Existing C users lose a working feature | Medium | Keep field compatibility tests and migrate field behavior through unified member queries before adding methods | Store/member tests fail for C struct fields |
| R3 Out-of-class owner association creates false owners | Wrong methods appear for a receiver | Medium | Only support simple textual owner extraction and label as lower confidence; decline ambiguous owners | Multiple owners match one method owner text |
| R4 Weak receiver inference becomes too magical | Unsupported expressions produce noisy member lists | Medium | Restrict to explicit declarations and unique low-confidence name correlation; keep fallback incomplete and prefix-gated | Receiver expression is not a simple identifier/pointer identifier |
| R5 Member fallback gets polluted by all methods | Member list becomes noisy in large workspaces | Medium | Prefix-only fallback, minimum two-character gate, per-kind caps if needed, and incomplete list semantics | One-character member fallback returns non-empty list |
| R6 Local history overpowers deterministic ranking | Old habits hide better current/local candidates | Medium | Small capped boost, decay/bounds, guard tests, and disabled parity tests | History candidate outranks high-confidence current/local binding by history alone |
| R7 Completion accept command records too much or wrong data | Privacy or quality regression | Medium | Store candidate keys and buckets, not source snippets; record positive feedback only after insertion command; support clear/off | Default log or store includes raw source snippets |
| R8 VS Code command path misses some accepts | History benefit is partial | Medium | Treat Phase 8 as best-effort positive evidence; absence of history never hurts base ranking | Accepted item lacks command execution in a supported path |
| R9 Full rebuild takes time on large workspaces | First v1.2.1 startup feels expensive | Medium | Surface indexing progress and avoid pretending ready; document schema rebuild behavior | Schema mismatch triggers rebuild and status does not show progress |
| R10 Docs conflict with earlier Phase 0-6 docs | Maintainers misread current behavior | Medium | Keep historical phase docs as historical, update current docs and v1.2.1 requirements clearly | README/CLAUDE still say local history or member methods are not enabled |

## 10. 用户确认记录

- 2026-07-05: User requested v1.2.1 advancement based on `docs/research/smart-completion-dev-eval.md`, specifically Phase 7-8 after v1.2.0 Phase 0-6 smoke success.
- 2026-07-05: User required `himupowers:brainstorming` for requirement determination.
- 2026-07-05: User selected `smart-completion-v1-2-1` as the feature brief name.
- 2026-07-05: User approved 方案 A: Phase 7 member evidence plus Phase 8 local history, with local history default `auto` and clear/disable controls.
- 2026-07-05: User clarified that a large refactor is acceptable and startup full rebuild is acceptable.
- 2026-07-05: User approved this requirements document and requested implementation planning.
- 2026-07-05: Implementation and verification completed. Passed targeted Rust tests (`parser::tests`, `store::tests::members`, `store::tests::resilience_schema`, `completion_history`, `completion::tests`, `server::tests`), full `cargo test -p fossilsense`, mini-c forced index smoke, extension `pnpm run compile` / `pnpm run test`, `git diff --check`, placeholder scan, and `pnpm run package`. Generated VSIX: `dist/fossilsense-vscode-1.2.1_BUILD20260705_180741.vsix`.
