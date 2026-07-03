# Signature Help Reachable Definitions Requirements

Status: current
Date: 2026-07-02
Feature brief name: signature-help-reachable-definitions

## 1. 需求来源和背景

- User request, 2026-07-02: "用户编码时的函数参数提示，更加准确的函数可达性确定保证查找定义的准确". The requested outcome combines coding-time function parameter hints with more accurate function reachability for definition lookup.
- `AGENTS.md` delegates project instructions to `CLAUDE.md`.
- `CLAUDE.md` positions FossilSense as a best-effort C/C++ navigation and analysis tool for large Windows workspaces without reliable compile environments. This requirement must not depend on clangd, compile commands, external ctags, or compile-grade semantic binding.
- `CLAUDE.md` requires new navigation and completion behavior to reuse existing concepts such as `DefinitionCandidate`, `ScopeTier`, `ResolutionConfidence`, `ResolutionReason`, `ReachScope`, and the shared `resolver`; candidates must expose confidence, fallback, and ambiguity instead of pretending to be exact bindings.
- `README.md` and `extensions/vscode/README.md` list current lightweight completion and go-to-definition behavior, and explicitly state that FossilSense has no signature help, parameter hints, or overload resolution at this snapshot.
- `extensions/vscode/package.json` exposes settings for `fossilsense.mode`, `fossilsense.completion.mode`, `fossilsense.includeScoping.mode`, and `fossilsense.debug.candidateReasons`, but no dedicated signature-help setting. Phase one is governed by the server being active.
- Code inspection shows existing function symbols carry `signature` text through `parser::Symbol`, SQLite stores the signature, `IndexStore::symbols_by_name` can fetch exact-name candidates, and `query::rank_definitions_into_candidates_with_scope` already ranks candidates with open-aware reachability. `server::language_server` registers definition, references, symbols, completion, and semantic tokens, but does not register a `signature_help_provider`.
- The repository does not contain `templates/requirements-template.md`; this document uses the required requirements sections as the effective template.

## 2. 用户需求

- UR1: While editing C/C++ code, the user can see function parameter hints at a call site.
- UR2: Function candidates used by parameter hints and go-to-definition are ranked with more accurate include reachability, so reachable definitions are preferred over unrelated same-name functions.
- UR3: The feature remains best-effort and works without a build system, compiler arguments, full IntelliSense, or `compile_commands.json`.
- UR4: The first implementation phase covers both parameter hints and function definition candidate ordering, while staying within FossilSense's existing resolver and reachability model.
- UR5: Ambiguity, fallback, and open include scope are visible in user-facing or debug-facing evidence instead of being hidden.

## 3. 范围与非范围

In scope:

- Register LSP signature help for C and C++ documents when the FossilSense server is active.
- Trigger signature help on `(` and `,`, and answer explicit editor signature-help requests inside an existing call.
- Identify the current call expression name and active argument index from the open document using a tolerant request-time parser.
- Fetch exact-name function candidates from the index, filter to function symbols, and rank them through existing reachability-aware resolver primitives.
- Convert stored function `signature` strings into LSP `SignatureInformation` and `ParameterInformation` when parameters can be parsed conservatively.
- Show the whole signature even when parameter splitting is uncertain.
- Harden function go-to-definition ordering with tests for current, reachable, external, ambiguous, unknown, and global candidates.
- Update user-facing documentation that currently says signature help is absent.

Out of scope:

- Compile-grade overload resolution.
- Argument type matching.
- Template, namespace, inheritance, access-control, and C++ overload semantics.
- Function-like macro expansion or macro-selected signatures.
- Preprocessor branch evaluation.
- Inferring function pointer target types or member function receiver types.
- Auto-import, snippets, diagnostics, or semantic references.
- A new external parser or toolchain dependency.

## 4. 场景与预期

| ID | Role | Trigger | Preconditions | Action | Expected result |
|---|---|---|---|---|---|
| SC1 | C/C++ developer | Types `foo(` | `foo` exists in the index as a function | Request signature help at the cursor after `(` | FossilSense returns signature candidates for `foo`, active parameter `0`, and the best reachable candidate first. |
| SC2 | C/C++ developer | Types `,` inside `foo(a, ` | A signature for `foo` can be split into parameters | Request signature help after the comma | The same ranked signatures are returned with active parameter `1` when the index is in range. |
| SC3 | C/C++ developer | Calls a duplicate function name | One `foo` is reachable through includes and another is unrelated | Request signature help or go-to-definition on `foo` | The reachable candidate ranks ahead of global fallback candidates. |
| SC4 | C/C++ developer | Calls a function under unresolved or ambiguous include scope | The reach graph is open | Request signature help or go-to-definition | FossilSense keeps useful candidates, labels ambiguity or fallback through confidence/reason, and does not hard-filter unrelated candidates out of existence. |
| SC5 | C/C++ developer | Calls a function whose signature cannot be split safely | The stored signature is available but syntactically complex | Request signature help | FossilSense shows the full signature label without fabricated per-parameter labels. |
| SC6 | C/C++ developer | Works before indexing is ready or outside a workspace root | No usable index or root exists | Request signature help | FossilSense returns an empty response and logs no misleading error. |

## 5. 功能性需求

- FR1 Signature-help capability: the server must advertise `signatureHelpProvider` for active FossilSense C/C++ sessions, with trigger characters `(` and `,`.
- FR2 Call-site detection: the request path must identify a nearest enclosing function call name and active argument index from the open document at the LSP position. It must handle nested parentheses, brackets, braces, and commas inside nested argument expressions conservatively.
- FR3 Function candidate retrieval: signature help must query exact-name symbols and keep only `function` candidates. It must use the same workspace root, current relative path, and `ReachScope` inputs used by go-to-definition when available.
- FR4 Candidate ranking: function signature candidates must be ranked through the shared resolver model. Tier must dominate definition/declaration preference and locality, matching the existing definition-candidate policy.
- FR5 Signature conversion: the feature must convert existing stored function signature text into LSP signature labels and parameter labels when the parameter list can be split without ambiguity. If splitting is unsafe, it must return the whole signature label with no fabricated parameter list.
- FR6 Active parameter: the returned `activeParameter` must match the best-effort argument index at the cursor when that index is within the parsed parameter count. If the index exceeds the count, FossilSense must keep the signature visible without selecting a nonexistent parameter.
- FR7 User-visible uncertainty: non-current or uncertain signatures must expose tier, confidence, and reason in signature documentation or an equivalent visible/debuggable channel consistent with existing completion and go-to-definition labeling.
- FR8 Go-to-definition hardening: tests must prove that function candidates respect current, reachable, first-layer external, unknown, and global ordering under closed and open reach scopes.
- FR9 Degraded behavior: missing indexes, missing roots, parse fallback, malformed signatures, and unsupported call shapes must return empty or reduced signature help rather than failing the request.
- FR10 Documentation update: implementation must update README and extension README capability statements so they describe supported signature help and unsupported overload/semantic cases accurately.

## 6. 非功能性需求

- NFR1 No external toolchain: the feature must not require clangd, a compiler, compile commands, ctags, or a build-system model.
- NFR2 Honest best-effort semantics: returned signatures and definitions are candidates, not semantic bindings. Confidence, fallback, and ambiguity must stay visible where user impact exists.
- NFR3 Performance: a signature-help request may perform an exact-name SQLite lookup, but must not scan the workspace or run broad fuzzy search. Candidate counts must be capped before LSP response assembly.
- NFR4 Large workspace safety: request-time work must be proportional to the current open document context and exact-name candidate set, not total indexed symbol count.
- NFR5 Maintainability: call-context parsing, signature splitting, and candidate ranking must be covered by focused unit tests and kept in pure helper code where possible.
- NFR6 Compatibility: the feature must not change existing completion behavior, include completion behavior, semantic coloring, references, or conflict-extension detection.
- NFR7 Configuration simplicity: phase one must not add a new user setting. Stopping the FossilSense server or setting `fossilsense.mode = off` disables the provider along with the rest of the server.
- NFR8 Documentation accuracy: user-facing docs must list what the feature can do, what it cannot do, and how fallback or ambiguity is surfaced.

## 7. 技术方案

### Recommended: request-time signature help over existing signatures

Add a focused signature-help path that reuses existing indexed function signatures and resolver ranking:

- Add pure helpers for call-context detection and signature splitting.
- Add `server/signature_help.rs` for LSP orchestration, following the existing `server/member_completion.rs` pattern.
- Reuse `IndexStore::symbols_by_name` for exact-name candidate retrieval.
- Reuse `query::rank_definitions_into_candidates_with_scope` or a small function-specific wrapper that preserves the same resolver semantics.
- Build `SignatureHelp` from ranked function candidates, including active signature, active parameter, labels, and confidence/reason documentation.
- Add regression tests around duplicate function names and include reachability.

Trade-off: this gives immediate value without schema migration and keeps the feature aligned with FossilSense's best-effort model. Parameter labels come from conservative parsing of stored signatures, so complex declarations can degrade to whole-signature display.

### Alternative A: persist structured function parameters

Extend parsing and SQLite schema to store function parameter names, types, and ranges during indexing.

Trade-off: parameter labels would be cleaner and faster to assemble, but this adds schema migration, parser complexity, and more index-time surface area before proving the feature's user value.

### Alternative B: text-only signature help without resolver ranking

Detect the call name and show signatures by exact text match without reachability-aware ranking.

Trade-off: this is simpler, but it does not satisfy the requirement for more accurate function reachability and would create a parallel ranking path outside the shared resolver model.

### Design decisions

- D1: Use the recommended request-time signature-help path for phase one.
- D2: Do not change the SQLite schema in phase one.
- D3: Do not introduce a new setting; signature help is active when the FossilSense server is active.
- D4: Use function-only candidates for signature help; macros are excluded from phase one.
- D5: Prefer reduced results over fabricated precision when call-context or signature parsing is uncertain.

## 8. 需求跟踪矩阵

| Requirement | Sources | Scenarios | Design decisions | Plan tasks | Test/validation | Status |
|---|---|---|---|---|---|---|
| FR1 | UR1, README current gap | SC1, SC2 | D1, D3 | Task 5, Task 6, Task 8 | `cargo test -p fossilsense server::options::tests::signature_help_options_trigger_on_paren_and_comma -- --nocapture`; manual VS Code smoke | 已计划 |
| FR2 | UR1, code current open-doc model | SC1, SC2, SC5 | D1, D5 | Task 1, Task 6 | `cargo test -p fossilsense query::signatures -- --nocapture` | 已计划 |
| FR3 | UR2, `IndexStore::symbols_by_name` | SC3, SC4 | D1, D4 | Task 3, Task 6 | `cargo test -p fossilsense query::signatures -- --nocapture`; `cargo test -p fossilsense server::signature_help -- --nocapture` | 已计划 |
| FR4 | UR2, `CLAUDE.md` resolver rules | SC3, SC4 | D1 | Task 3, Task 4, Task 6 | `cargo test -p fossilsense query::definitions -- --nocapture`; `cargo test -p fossilsense query::signatures -- --nocapture` | 已计划 |
| FR5 | UR1, stored `signature` field | SC1, SC5 | D1, D2, D5 | Task 2, Task 6 | `cargo test -p fossilsense query::signatures -- --nocapture`; `cargo test -p fossilsense server::signature_help -- --nocapture` | 已计划 |
| FR6 | UR1 | SC2, SC5 | D5 | Task 1, Task 2, Task 6 | `cargo test -p fossilsense query::signatures -- --nocapture`; `cargo test -p fossilsense server::signature_help -- --nocapture` | 已计划 |
| FR7 | UR5, `CLAUDE.md` user-visible labels | SC3, SC4 | D1, D5 | Task 3, Task 4, Task 6, Task 7 | `cargo test -p fossilsense query::definitions -- --nocapture`; `cargo test -p fossilsense server::signature_help -- --nocapture`; README review | 已计划 |
| FR8 | UR2, resolver hardening requirement | SC3, SC4 | D1 | Task 4, Task 8 | `cargo test -p fossilsense query::definitions -- --nocapture`; manual Go to Definition smoke | 已计划 |
| FR9 | UR3, FossilSense degradation policy | SC6 | D5 | Task 1, Task 2, Task 6, Task 8 | `cargo test -p fossilsense query::signatures -- --nocapture`; `cargo test -p fossilsense server::signature_help -- --nocapture`; missing index smoke | 已计划 |
| FR10 | README and extension README current statements | SC1-SC6 | D1-D5 | Task 7, Task 8 | `rg -n "no signature help|There is still \\*\\*no\\*\\* signature help" README.md extensions/vscode/README.md`; `pnpm run compile` | 已计划 |
| NFR1 | `CLAUDE.md` project positioning | SC1-SC6 | D1, D2 | Task 7, Task 8 | `cargo test -p fossilsense`; dependency review; docs review | 已计划 |
| NFR2 | `CLAUDE.md` candidate-not-binding rule | SC3, SC4 | D5 | Task 3, Task 4, Task 6, Task 7 | confidence/reason assertions in `query::definitions` and `server::signature_help`; README wording review | 已计划 |
| NFR3 | Large workspace requirement | SC1-SC6 | D1, D2 | Task 3, Task 6, Task 8 | exact-name lookup review; candidate cap test; `cargo run -p fossilsense -- index samples/mini-c --db target/signature-help-mini.sqlite --force` | 已计划 |
| NFR4 | Large workspace requirement | SC1-SC6 | D1 | Task 1, Task 6 | request-local parsing unit tests in `query::signatures`; no workspace scan code review | 已计划 |
| NFR5 | Maintainability rule | SC1-SC6 | D1 | Task 1, Task 2, Task 3, Task 6 | focused tests in `query::signatures` and `server::signature_help` | 已计划 |
| NFR6 | Existing feature compatibility | SC1-SC6 | D3 | Task 4, Task 5, Task 6, Task 8 | `cargo test -p fossilsense`; `pnpm run compile` | 已计划 |
| NFR7 | Configuration simplicity | SC1-SC6 | D3 | Task 5, Task 6 | server capability helper test; package settings review showing no new setting | 已计划 |
| NFR8 | Documentation accuracy | SC1-SC6 | D5 | Task 7, Task 8 | README and extension README review; stale negative-claim `rg` check | 已计划 |

## 9. 风险与缓解

| Risk | Impact | Likelihood | Mitigation | Trigger |
|---|---|---|---|---|
| R1 Complex C/C++ declarations are split into incorrect parameters | Misleading parameter highlight | Medium | Use conservative splitting; fall back to whole signature without per-parameter labels | Signature splitter sees unmatched delimiters or unsupported structure |
| R2 Function-like macros look like calls but are not indexed functions | Missing hints for macro calls | Medium | Exclude macros in phase one and document that macro signature help is unsupported | Call name resolves only to macro candidates |
| R3 Users interpret ranked signatures as exact overload resolution | Trust mismatch | Medium | Include confidence/reason documentation and docs language that signatures are candidates | Multiple same-name candidates appear |
| R4 Large projects produce many same-name function candidates | Slow response or noisy UI | Medium | Cap ranked candidates and keep exact-name lookup only | Candidate count exceeds response cap |
| R5 Open include scope makes unrelated candidates appear | Noisy fallback results | Medium | Preserve open-scope semantics but label `Ambiguous` or `Fallback`; do not bury all non-reachable candidates | `ReachScope.open` is true |
| R6 Go-to-definition and signature help drift in ranking behavior | Inconsistent user experience | Low | Reuse shared resolver ranking and cover both paths with duplicate-function tests | Tests show different order for the same candidate set |
| R7 Documentation becomes too optimistic | User confusion | Medium | Update docs with can/cannot/fallback and avoid compile-grade claims | README wording omits best-effort limits |

## 10. 用户确认记录

- 2026-07-02: User selected `signature-help-reachable-definitions` as the feature brief name.
- 2026-07-02: User selected phase-one scope "参数提示+定义排序", covering both coding-time parameter hints and definition candidate ordering.
- 2026-07-02: User approved the presented requirements design for writing to this document.
