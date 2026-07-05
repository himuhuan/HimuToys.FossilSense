# Smart Completion Phase 4-6 Requirements

Status: implemented-and-verified
Date: 2026-07-05
Feature brief name: smart-completion-phase4-6

## 1. 需求来源和背景

- User request, 2026-07-05: advance the long-term next-version plan using `docs/research/smart-completion-dev-eval.md`; complete Phase 4-6; keep the plan logically self-consistent without chasing unnecessary completeness.
- User instruction, 2026-07-05: use `himupowers:brainstorming` to determine requirements before implementation.
- User decision, 2026-07-05: selected `smart-completion-phase4-6` as the feature brief name.
- User decision, 2026-07-05: approved the Phase 4-6 scope as lightweight intent classifier, multi-channel recall quotas, and include completion ranking enhancement; member methods, local history, telemetry, ML, and auto include insertion remain outside this phase.
- `AGENTS.md` delegates project instructions to `CLAUDE.md`.
- `CLAUDE.md` positions FossilSense as a best-effort C/C++ navigation and analysis tool for large Windows workspaces without reliable compile environments. Phase 4-6 must not require clangd, compile commands, ctags, compiler invocations, or a build-system model.
- `CLAUDE.md` requires ordinary completion to keep `CompletionList.isIncomplete = true`, avoid per-keystroke disk IO, preserve short-prefix noise gates, expose confidence/fallback/ambiguity honestly, and avoid parallel semantic concepts outside `ScopeTier`, `ResolutionConfidence`, `ResolutionReason`, `NameTable`, and shared resolver vocabulary.
- `README.md` now states v1.2.0 smart completion Phase 0-6 is implemented through the completion core: ordinary identifier completion uses deterministic evidence-aware ranking, current-file overlay, evidence merge, lightweight intent ranking, bounded multi-channel recall, source-safe perf summaries, and shadow rank summaries. It states member method schema, local history, telemetry, ML, and auto include insertion remain excluded.
- `docs/smart-completion-v1-2/requirements.md` records Phase 0-1 as completed: observable compatible completion pipeline, candidate metadata, source metrics, timing summary, and shadow comparison.
- `docs/smart-completion-phase2-3/requirements.md` records Phase 2-3 as implemented and verified: evidence-aware deterministic ranker, soft scope prior, guard bands, current-file overlay, same-name evidence merge, and documentation sync.
- `docs/research/smart-completion-dev-eval.md` defines Phase 4 as a lightweight rule-based intent classifier; Phase 5 as multi-channel recall with quotas; Phase 6 as include completion ranking using recent/sibling/directory/frequency/depth signals.
- Current code facts: `crates/fossilsense/src/completion.rs` owns candidate evidence, merge, rank, guard, metrics, and source-safe perf summary; `crates/fossilsense/src/server/language_server.rs` routes include/member completion before ordinary completion, gathers local bindings, current-file overlay, local words, and indexed `NameTable` hits, then calls `run_evidence_aware_pipeline`; `crates/fossilsense/src/query.rs` provides `NameTable`, `RankedNameHit`, strict resolver-packed recall metadata, pooled narrowing, short-prefix gating, and `COMPLETION_LIMIT`; `crates/fossilsense/src/query/current_file_overlay.rs` extracts current-file semantic/text overlay; `crates/fossilsense/src/server/include_completion.rs` provides include path completion and `IncludeCompletionTable`.
- Recent commits show the long-term smart completion work has proceeded in order: `b776d0a feat: add v1.2 smart completion phase 0-1` and `d6c8a59 Implement smart completion phase 2-3`.

## 2. 用户需求

- UR1: Ordinary identifier completion should understand coarse editing intent without pretending to perform compiler-grade semantic analysis.
- UR2: Type-name positions should lift types, typedef/using aliases, records, and enum-like type candidates above weak expression-value candidates.
- UR3: Expression-value positions should lift variables, functions, macros that behave like values, and enum constants above weak type-only candidates.
- UR4: Call-target positions should lift functions and function-like macros when the user is about to type or has just typed a call.
- UR5: Preprocessor conditions and directives should lift macros and avoid type/global symbol noise where possible.
- UR6: Declaration-name contexts should avoid aggressively suggesting unrelated global symbols when the user appears to be naming a new local or declaration.
- UR7: Multi-channel recall should keep current-file overlay, local bindings, reachable symbols, direct external symbols, open-scope unknown symbols, global backoff symbols, and local text words represented in the internal pool before final reranking.
- UR8: Candidate pool growth must remain bounded and memo-friendly so large Windows workspaces stay responsive.
- UR9: Include path completion should better match project habits: quoted includes prefer same-directory or sibling/component headers, while angle includes continue to respect include paths and external roots.
- UR10: Include ranking should use lightweight in-memory evidence such as include recency in the current file, sibling include patterns, basename frequency, same-directory preference, and path depth penalty.
- UR11: Existing include/member short-circuit behavior, short-prefix noise gates, current-file overlay behavior, guard bands, source-safe logs, and `CompletionList.isIncomplete = true` must remain compatible.

## 3. 范围与非范围

In scope:

- Add a lightweight rule-based `CompletionIntent` classifier for ordinary identifier completion.
- Cover these intents in Phase 4: `TypeName`, `ExpressionValue`, `CallTarget`, `MacroPreprocessor`, and `DeclarationName`.
- Treat `IncludePath` and `MemberAccess` as existing surface-routing contexts that continue to short-circuit to include/member completion before ordinary completion.
- Add intent evidence and confidence to the completion pipeline, with low-confidence intent producing small ranking adjustments rather than hard filters.
- Extend the evidence-aware ranker so candidate kind/source can respond to intent while preserving scope/confidence guard bands.
- Add multi-channel recall quotas for ordinary completion, using existing `NameTable`, local bindings, current-file overlay, local words, reachability scope, and completion memo pools.
- Add `NameTable` support for returning larger bounded raw pools by channel or tier so final ranking is not starved by one strict top-N.
- Keep short-prefix gates based on raw match quality; do not reintroduce ordinary substring or subsequence long-tail noise for one- and two-character prefixes.
- Extend `IncludeCompletionTable` or adjacent include completion state with in-memory ranking evidence for sibling/recent/basename/depth signals.
- Keep quote/angle include base priority semantics intact while improving ordering within comparable candidates.
- Add focused Rust tests for intent classification, intent-aware ranking, channel quota behavior, memo narrowing compatibility, include ranking, and source-safe perf summaries.
- Update `CLAUDE.md`, `README.md`, and `extensions/vscode/README.md` to describe Phase 4-6 capabilities, limits, fallbacks, and non-goals.

Out of scope:

- Member method/member evidence schema, parser extraction for methods, weak receiver inference, inheritance, overload, template, namespace, and access-control semantics.
- Local history personalization or accepted-completion feedback.
- Anonymous telemetry, ML reranking, LLM completion, or model distribution.
- Auto include insertion or source editing triggered by completion acceptance.
- Full C/C++ semantic typing, macro expansion, data-flow analysis, or build-system inference.
- New external dependencies, compiler invocations, clangd integration, or `compile_commands.json` requirements.
- Per-keystroke SQLite queries, workspace scans, external include tree scans beyond existing cached directory listing behavior, or unbounded candidate pools.
- User-facing configuration knobs for rank weights in this phase.

## 4. 场景与预期

| ID | Role | Trigger | Preconditions | Action | Expected result |
|---|---|---|---|---|---|
| SC1 | C/C++ developer | Completing a type position | Cursor follows declaration-like syntax, cast-like syntax, `new`, or another lightweight type-name cue | Request ordinary completion | Type aliases, records, and type symbols rise above weak expression/global fallback candidates; no non-type candidate disappears solely because of intent. |
| SC2 | C/C++ developer | Completing an expression position | Cursor is in RHS, return, condition, initializer, or argument-like syntax | Request ordinary completion | Variables, functions, enum constants, and value-like macros rise; type-only candidates are demoted but remain available if otherwise strong. |
| SC3 | C/C++ developer | Completing a call target | Cursor context suggests an identifier before `(` or after a partially typed call target | Request ordinary completion | Functions and function-like macros rise; text/global candidates cannot outrank structured reachable/current call targets without strong evidence. |
| SC4 | C/C++ developer | Completing a preprocessor condition | Cursor is on `#if`, `#ifdef`, `#ifndef`, `#elif`, or macro-oriented directive context | Request ordinary completion | Macros rise and unrelated type/global names are demoted; malformed directive text falls back to low-confidence ordinary ranking. |
| SC5 | C/C++ developer | Naming a new declaration | Cursor appears after a recognized type/declaration prefix where a new variable/function parameter name is likely | Request ordinary completion | Existing unrelated global symbols are less aggressively promoted, reducing accidental reuse suggestions while keeping local/current textual hints available. |
| SC6 | C/C++ developer | Strong candidates exist in different channels | Current-file overlay, reachable indexed symbols, global fallback symbols, and text words all match the prefix | Request ordinary completion | Internal candidate pool includes bounded representation from each relevant channel before reranking; final Top 100 is not filled solely by one channel. |
| SC7 | C/C++ developer | Short prefix in a large workspace | Prefix length is one or two characters | Request ordinary completion | Exact, prefix, and word-boundary substring gates remain; expanded pools do not reintroduce plain substring/subsequence noise. |
| SC8 | C/C++ developer | Quoted include near sibling headers | Current file includes related headers or sits next to matching headers | Request include completion inside quotes | Same-directory, sibling/component, and recently included project headers rise while quote search order remains current directory -> workspace -> include paths. |
| SC9 | C/C++ developer | Angle include from configured external roots | `fossilsense.includePaths` contains external include roots | Request include completion inside angle brackets | External/includePath root candidates keep their base priority; depth and basename evidence refine ordering without hiding workspace fallback candidates. |
| SC10 | Maintainer | Debugging completion quality | Perf logging is enabled | Request ordinary or include completion | Logs expose counts, timings, intent category/confidence bucket, channel counts, guard summaries, and include ranking summary without raw candidate names or source snippets. |

## 5. 功能性需求

- FR1 Intent context model: ordinary completion must derive a `CompletionIntent` and confidence from the current line, cursor position, and, when available, parsed open-document facts.
- FR2 Type intent ranking: `TypeName` intent must positively weight type, alias, record, and type-like candidates while applying only bounded demotion to value candidates.
- FR3 Expression intent ranking: `ExpressionValue` intent must positively weight variable, function, enum constant, and value-like macro candidates while applying only bounded demotion to type-only candidates.
- FR4 Call intent ranking: `CallTarget` intent must positively weight function candidates and function-like macros.
- FR5 Macro intent ranking: `MacroPreprocessor` intent must positively weight macro candidates in preprocessor contexts.
- FR6 Declaration-name intent: `DeclarationName` intent must reduce unrelated global/indexed reuse pressure and keep current/local/text naming evidence available without claiming a semantic binding.
- FR7 Intent fallback: malformed code or uncertain context must produce low-confidence or neutral intent and must not remove candidates.
- FR8 Ranker integration: intent scoring must be centralized in the completion core and combined with existing source, scope, confidence, match, proximity, and guard-band evidence.
- FR9 Channel recall quotas: ordinary completion recall must gather bounded candidates from local bindings, current-file overlay, reachable indexed symbols, direct external symbols, open-scope unknown symbols, global backoff symbols, and local text words where those channels exist.
- FR10 NameTable raw pool support: `NameTable` must expose a bounded recall path that can produce channel/tier-aware pools without requiring final strict `pack_score` top-N to be the only input to reranking.
- FR11 Memo compatibility: completion memo narrowing must remain correct when channel pools grow; generation invalidation must include all state that affects recalled pools.
- FR12 Short-prefix gate preservation: one- and two-character ordinary completion must continue to reject plain substring and subsequence long-tail matches.
- FR13 Candidate caps: expanded recall must use per-channel and total caps so returned items remain limited by `COMPLETION_LIMIT` and internal work remains bounded.
- FR14 Include ranking evidence: include completion must score same-directory, sibling/component, recent include, basename frequency, and path depth signals in addition to existing quote/angle source priority and prefix matching.
- FR15 Include table invalidation: include ranking evidence derived from indexed workspace files must rebuild or invalidate with the same generation rules as the existing include completion table.
- FR16 Source-safe observability: perf/debug summaries must include intent bucket, channel input counts, post-rank counts, guarded counts, include ranking summary counts, and timings without default raw labels or source snippets.
- FR17 Surface compatibility: include path and member completion must continue to short-circuit before ordinary identifier completion; member completion remains field-focused.
- FR18 Documentation sync: `CLAUDE.md`, root `README.md`, and extension `README.md` must describe Phase 4-6 behavior, confidence/fallback semantics, and excluded capabilities.

## 6. 非功能性需求

- NFR1 Best-effort honesty: implementation, labels, docs, and tests must not imply compiler-grade type inference or semantic binding.
- NFR2 Large-workspace hot path: ordinary completion must use in-memory name tables, existing reachability scope, open-document parse cache, current local word cache, and include table state; no per-keystroke workspace scan or SQLite query is allowed.
- NFR3 Determinism: equal inputs must produce stable ordering through deterministic tie-breakers.
- NFR4 Bounded latency: expanded candidate pools must have per-channel and total caps, and tests or perf summaries must make recall/rank cost visible.
- NFR5 Privacy: default logs must avoid raw candidate names, include paths, and source snippets; path-sensitive debugging is not part of this phase.
- NFR6 Maintainability: intent classification, recall quotas, rank weights, and include ranking signals must be testable as pure logic without constructing a full tower-lsp server.
- NFR7 Compatibility: existing include, member, local binding, overlay, ranker, memo, parser fallback, query scoping, and server completion tests must keep passing after intentional assertion updates.
- NFR8 Concept stability: new models must reuse existing `ScopeTier`, `ResolutionConfidence`, `ResolutionReason`, parser symbol kinds, and completion evidence vocabulary; no parallel semantic tier system is allowed.
- NFR9 Documentation consistency: no current documentation may continue to say intent classifier, multi-channel recall, or include recent/sibling ranking are disabled after this phase is implemented.

## 7. 技术方案

### Recommended: extend the existing completion core and recall/include tables

Add a small, protocol-agnostic intent classifier beside the existing completion core or within a focused completion submodule. The LSP handler continues to detect include/member surfaces first. For ordinary completion, it computes `CompletionIntent`, passes intent evidence into the existing pipeline, and lets the completion core apply centralized intent-aware score adjustments and guard bands.

For Phase 5, extend `NameTable` with bounded channel-aware recall helpers. The existing strict ranked search and `pack_score` metadata remain available for non-completion callers and compatibility, but ordinary completion can request larger capped pools by scope tier/source channel before final evidence-aware reranking. Completion memo stores per-table pool indices in a way that preserves prefix narrowing and generation invalidation.

For Phase 6, extend `IncludeCompletionTable` or an adjacent include ranking structure to precompute lightweight workspace include evidence from indexed file paths and include edges already available after indexing. `collect_include_candidates_with_table` keeps quote/angle source ordering as base priority, then applies same-directory/sibling/recent/frequency/depth adjustments within bounded candidate lists.

Trade-off: this approach is incremental and aligns with the Phase 0-3 architecture, but it requires careful tests to prove expanded pools do not regress latency or short-prefix noise.

### Alternative A: add ad hoc intent and include weights in `server/language_server.rs`

This is faster to code, but it spreads ranking rules through LSP request plumbing and makes unit tests harder. It also conflicts with the project rule that completion merge/rank behavior should live in the completion core rather than accumulating magic scores in handlers.

### Alternative B: perform a broad `completion/` directory migration first

This would create cleaner long-term files such as `context.rs`, `recall.rs`, `ranker.rs`, and `include.rs`, but it front-loads structural churn before the actual Phase 4-6 behavior. It is acceptable only if `completion.rs` becomes too large during implementation; it is not required for the first self-consistent version.

### Design decisions

- D1: Implement Phase 4-6 as an incremental extension of the existing Phase 0-3 pipeline.
- D2: Keep include/member surface routing outside ordinary identifier completion.
- D3: Use rule-based intent with confidence; never hard-filter candidates by intent.
- D4: Centralize intent weights and guard interaction in the completion core.
- D5: Expand recall through bounded channel quotas rather than unbounded larger top-N.
- D6: Preserve `NameTable` strict ranked APIs for existing callers and add completion-specific raw/channel pool APIs.
- D7: Keep quote/angle include search-order priors intact and apply new include evidence as secondary ranking.
- D8: Keep source-safe observability summaries and avoid raw names/paths by default.

## 8. 需求跟踪矩阵

| Requirement | Sources | Scenarios | Design decisions | Plan tasks | Test/validation | Status |
|---|---|---|---|---|---|---|
| FR1 | UR1, eval Phase 4 | SC1-SC5 | D1, D3 | Task 1 | `cargo test -p fossilsense completion::tests::intent_ -- --nocapture` | 已验证 |
| FR2 | UR2, eval Phase 4 | SC1 | D3, D4 | Task 2 | `cargo test -p fossilsense completion::tests -- --nocapture` | 已验证 |
| FR3 | UR3, eval Phase 4 | SC2 | D3, D4 | Task 2 | `cargo test -p fossilsense completion::tests -- --nocapture` | 已验证 |
| FR4 | UR4, eval Phase 4 | SC3 | D3, D4 | Task 2 | `cargo test -p fossilsense completion::tests -- --nocapture` | 已验证 |
| FR5 | UR5, eval Phase 4 | SC4 | D3, D4 | Task 2 | `cargo test -p fossilsense completion::tests -- --nocapture` | 已验证 |
| FR6 | UR6, eval Phase 4 | SC5 | D3, D4 | Task 2 | `cargo test -p fossilsense completion::tests -- --nocapture` | 已验证 |
| FR7 | UR1, UX risk section | SC1-SC5 | D3 | Task 1, Task 2 | `cargo test -p fossilsense completion::tests -- --nocapture` | 已验证 |
| FR8 | UR1-UR6, Phase 2-3 architecture | SC1-SC5 | D1, D4 | Task 2 | `cargo test -p fossilsense completion::tests -- --nocapture` | 已验证 |
| FR9 | UR7, eval Phase 5 | SC6, SC7 | D5 | Task 3 | `cargo test -p fossilsense query::tests -- --nocapture` | 已验证 |
| FR10 | UR7, current `NameTable` facts | SC6 | D5, D6 | Task 3 | `cargo test -p fossilsense query::tests -- --nocapture` | 已验证 |
| FR11 | UR8, memo rules | SC6, SC7 | D5, D6 | Task 3 | `cargo test -p fossilsense server::tests -- --nocapture` | 已验证 |
| FR12 | UR11, `CLAUDE.md` short-prefix rules | SC7 | D5, D6 | Task 3 | `cargo test -p fossilsense query::tests -- --nocapture` | 已验证 |
| FR13 | UR8, large-workspace rule | SC6, SC7 | D5 | Task 3 | `cargo test -p fossilsense query::tests -- --nocapture` | 已验证 |
| FR14 | UR9, UR10, eval Phase 6 | SC8, SC9 | D7 | Task 4 | `cargo test -p fossilsense server::include_completion::tests -- --nocapture` | 已验证 |
| FR15 | UR10, include table generation facts | SC8-SC10 | D7 | Task 4 | `cargo test -p fossilsense server::tests -- --nocapture` | 已验证 |
| FR16 | UR11, privacy rules | SC10 | D8 | Task 2, Task 3, Task 4, Task 5 | `cargo test -p fossilsense completion::tests -- --nocapture`; `cargo test -p fossilsense server::include_completion::tests -- --nocapture` | 已验证 |
| FR17 | UR11, current server routing | SC8, SC9 | D2 | Task 4, Task 6 | `cargo test -p fossilsense server::tests -- --nocapture` | 已验证 |
| FR18 | docs consistency rules | SC1-SC10 | D1-D8 | Task 5, Task 6 | `rg -n "Phase 4-6|intent|multi-channel|include recent|sibling" README.md CLAUDE.md extensions/vscode/README.md` | 已验证 |
| NFR1 | `CLAUDE.md` candidate-not-binding rule | SC1-SC10 | D3, D8 | Task 5 | Documentation wording review plus `rg -n "binding|best-effort|intent" README.md CLAUDE.md extensions/vscode/README.md` | 已验证 |
| NFR2 | `CLAUDE.md` hot path rule | SC1-SC10 | D5, D6, D7 | Task 3, Task 4, Task 6 | Code review plus `cargo test -p fossilsense query::tests -- --nocapture`; `cargo test -p fossilsense server::include_completion::tests -- --nocapture` | 已验证 |
| NFR3 | UX stability risk | SC1-SC10 | D4, D5 | Task 2, Task 3 | `cargo test -p fossilsense completion::tests -- --nocapture`; `cargo test -p fossilsense query::tests -- --nocapture` | 已验证 |
| NFR4 | UR8 | SC6, SC7, SC10 | D5 | Task 3, Task 4, Task 6 | Perf summary review plus `cargo test -p fossilsense` | 已验证 |
| NFR5 | privacy rules | SC10 | D8 | Task 2, Task 4, Task 5 | Source-safe summary tests and docs grep | 已验证 |
| NFR6 | maintainability rules | SC1-SC10 | D1, D4, D7 | Task 1-4 | Pure logic tests: `completion::tests`, `query::tests`, `server::include_completion::tests` | 已验证 |
| NFR7 | compatibility rules | SC7-SC9 | D2, D6, D7 | Task 6 | `cargo test -p fossilsense` | 已验证 |
| NFR8 | `CLAUDE.md` model rules | SC1-SC10 | D3, D4, D6 | Task 1-5 | Code review for reused model vocabulary plus docs grep | 已验证 |
| NFR9 | docs consistency rules | SC1-SC10 | D8 | Task 5, Task 6 | `rg -n "intent classifier|multi-channel recall|include recent|Phase 4-6" README.md CLAUDE.md extensions/vscode/README.md` | 已验证 |

## 9. 风险与缓解

| Risk | Impact | Likelihood | Mitigation | Trigger |
|---|---|---|---|---|
| R1 Intent classifier misclassifies malformed C/C++ | Wrong-kind candidates rise and annoy users | Medium | Use confidence; low-confidence intent is neutral or small-weight only; never hard-filter by intent | Intent-specific tests show a non-target kind disappears |
| R2 Candidate pool expansion hurts latency | Completion becomes sluggish in large workspaces | Medium | Per-channel caps, total caps, prefix memo narrowing, and perf summary visibility | Recall or rank timing dominates completion summary |
| R3 Short-prefix noise returns | One- or two-character completion floods with fuzzy tail | Medium | Preserve raw match gate before channel quotas and add regression tests | Plain substring/subsequence appears for short prefix |
| R4 Channel quotas hide high-quality candidates | Reranker lacks enough strong candidates from a busy channel | Medium | Quotas are minimum representation plus bounded spillover, not rigid equal allocation | Strong exact/prefix match missing from internal pool |
| R5 Include ranking breaks quote/angle expectations | Users see external/workspace ordering that contradicts search form | Low | Keep form base priors and apply new signals secondarily | Angle include no longer prefers includePaths when comparable |
| R6 Include evidence becomes stale after index changes | Ranking reflects old sibling/frequency facts | Medium | Rebuild with existing include table generation and clear stale cache on rebuild failure | Include table generation changes without ranking evidence update |
| R7 Logs leak project identifiers or paths | Privacy and trust issue | Low | Default summaries use counts/classes/timings only; no raw names or paths | Perf/debug line contains candidate label or include path |
| R8 Scope or confidence concepts drift | Maintainers lose shared mental model | Medium | Reuse existing model enums and keep new intent as ranking evidence, not semantic binding | New code introduces independent semantic tier/confidence system |

## 10. 用户确认记录

- 2026-07-05: User requested Phase 4-6 advancement based on `docs/research/smart-completion-dev-eval.md`.
- 2026-07-05: User required `himupowers:brainstorming` for requirement determination.
- 2026-07-05: User selected `smart-completion-phase4-6` as the feature brief name.
- 2026-07-05: User approved the design scope: lightweight intent classifier, multi-channel recall quotas, and include completion ranking enhancement; member methods, local history, telemetry, ML, and auto include insertion excluded from this phase.
- 2026-07-05: User approved `docs/smart-completion-phase4-6/requirements.md` as the implementation basis and requested continuing into the implementation plan.
- 2026-07-05: Implementation verified with targeted Rust tests, full `cargo test -p fossilsense`, mini-c index smoke, VS Code extension compile, and VSIX package smoke. Generated VSIX: `dist/fossilsense-vscode-1.2.0_BUILD20260705_152610.vsix`.
