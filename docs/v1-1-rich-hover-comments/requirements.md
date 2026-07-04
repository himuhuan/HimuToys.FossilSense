# v1.1 Rich Hover Comments Requirements

Status: implemented
Date: 2026-07-04
Feature brief name: v1-1-rich-hover-comments

## 1. 需求来源和背景

- User request, 2026-07-04: advance the next version without waiting for further decisions; bump to v1.1.0; add beautiful rich hover rendering for users, Doxygen comments, and ordinary comments; handle nonstandard comments gracefully; hide UI display of related identifier ranges unless enabled by a switch; deliver spec, plan, code, and a report explaining the approach.
- `AGENTS.md` delegates project instructions to `CLAUDE.md`.
- `CLAUDE.md` positions FossilSense as a best-effort C/C++ navigation tool for large Windows workspaces with no reliable compile environment. Hover must therefore use indexed text candidates and current document text, not compiler-grade semantic binding.
- `CLAUDE.md` requires user-visible candidate features to expose confidence, fallback, and ambiguity without pretending that name matches are exact semantic bindings.
- Current code has signature help and go-to-definition ranking through `DefinitionCandidate`, `ScopeTier`, `ResolutionConfidence`, `ResolutionReason`, and `ReachScope`; hover should reuse that same model.
- Current code does not advertise LSP `textDocument/hover`, and no comment/documentation field is stored in SQLite. A minimal v1.1.0 path should avoid schema migration by extracting leading comments from the candidate source file at request time.
- Current VS Code grouped-references UI displays `relative/path:line` for each role-labeled reference. The v1.1.0 UI change hides the line/range suffix by default while keeping exact navigation behavior.

## 2. 用户需求

- UR1: While coding C/C++, hovering an identifier shows a polished Markdown hover with the best indexed candidate information.
- UR2: Hover renders nearby Doxygen-style comments and ordinary leading comments as readable rich text.
- UR3: Nonstandard, partial, decorative, or malformed comments degrade into reasonable plain Markdown instead of disappearing or corrupting the hover.
- UR4: Hover remains honest about best-effort ranking, showing candidate scope evidence without claiming compiler-level semantic binding.
- UR5: Related-reference UI hides line/range noise by default and exposes a setting for users who want to see it.
- UR6: Version facts and package versions move to v1.1.0 consistently enough for a self-contained VSIX build.

## 3. 范围与非范围

In scope:

- Register LSP hover support for C/C++ files when FossilSense is active.
- Detect the identifier under the hover position using existing text helpers.
- Query exact-name indexed symbols, rank them through existing definition ranking, and show a capped candidate list.
- Render Markdown hover content with a code-fenced signature, symbol kind/role, best-effort scope evidence, and extracted leading documentation comments.
- Extract comments from the current unsaved document when the top candidate is in the current file, and from disk for other candidate files.
- Support common Doxygen markers including `@brief`, `\\brief`, `@param`, `\\param`, `@return`, `\\return`, `@retval`, `@note`, `@warning`, and unknown commands as readable text.
- Support ordinary contiguous `//` comments and `/* ... */` block comments immediately above a symbol.
- Cap comment and candidate rendering so hover stays cheap and bounded, including a per-file byte limit before reading candidate sources from disk.
- Add a VS Code setting that controls whether grouped-reference QuickPick labels show line/range suffixes.
- Update README files, extension manifest, package test script, and version fields.

Out of scope:

- Compile-grade symbol binding, overload resolution, template/namespace lookup, macro expansion, or type-aware hover.
- Persisting documentation comments in SQLite schema.
- Rendering arbitrary Markdown extensions, HTML, images, or cross-reference links from Doxygen.
- Hover for comments themselves independent of an identifier.
- A custom VS Code webview hover UI.
- Reworking the references panel or standard LSP references response.
- Changing completion, semantic coloring, include analysis, or signature-help semantics except for documentation references to hover.

## 4. 场景与预期

| ID | Role | Trigger | Preconditions | Action | Expected result |
|---|---|---|---|---|---|
| SC1 | C/C++ developer | Hovers `foo` | `foo` is indexed as a function | Request hover on the identifier | Hover shows a Markdown code block for the function signature and candidate evidence. |
| SC2 | C/C++ developer | Hovers a function with `/** ... */` docs | The candidate source has a leading Doxygen block | Request hover | Hover shows prose, parameters, and return text in readable Markdown. |
| SC3 | C/C++ developer | Hovers a symbol with ordinary `//` comments | Contiguous comments directly precede the declaration | Request hover | Hover renders the comments as prose, not raw comment syntax. |
| SC4 | C/C++ developer | Hovers a symbol with messy comments | The comment has decorative `*`, unknown tags, missing punctuation, or incomplete formatting | Request hover | Hover still shows a compact, readable fallback without panicking or hiding the signature. |
| SC5 | C/C++ developer | Hovers duplicate names | Multiple indexed candidates exist | Request hover | Hover lists ranked best-effort candidates, with current/reachable/external/global evidence visible. |
| SC6 | C/C++ developer | Uses grouped references | References exist in multiple files | Run grouped references command | By default labels hide `:line`; enabling the setting restores line suffix display while navigation remains exact. |
| SC7 | Maintainer | Builds v1.1.0 | Workspace is clean | Run tests/package | Rust tests, TypeScript tests/compile, index smoke, and package command succeed or report environment gating clearly. |

## 5. 功能性需求

- FR1 Hover capability: the server must advertise and handle LSP hover requests.
- FR2 Hover target detection: hover must use the identifier at the LSP position and return no hover for whitespace, numbers, or unsupported positions.
- FR3 Candidate ranking: hover candidates must use the existing exact-name store query and `rank_definitions_into_candidates_with_scope`.
- FR4 Markdown rendering: hover must return `MarkupKind::Markdown` with stable, readable sections and no raw debug formatting.
- FR5 Comment extraction: hover must extract only comments immediately leading the candidate, allowing blank separator lines only when they are part of the contiguous leading block.
- FR6 Doxygen cleanup: hover must convert common Doxygen commands into readable Markdown and strip comment delimiters/prefixes.
- FR7 Messy comment fallback: malformed or nonstandard comments must render as prose when text can be recovered; failure to read comments must not suppress the signature.
- FR8 Bounded work: hover must cap candidate count, comment lines, and comment character length.
- FR9 No hover range noise: hover must not expose internal candidate source ranges in the visible Markdown; range details remain a debug concern.
- FR10 References UI switch: grouped-reference labels must hide line/range suffixes by default and show them only when the new setting is enabled.
- FR11 Version bump: Rust crate, VS Code package, README version facts, and package-lock metadata must move to v1.1.0 where applicable.
- FR12 Documentation update: repository and extension docs must describe hover can/cannot/fallback behavior and the new references UI setting.

## 6. 非功能性需求

- NFR1 No external toolchain: hover must not require clangd, ctags, compile commands, or a build system.
- NFR2 Honest best-effort semantics: hover wording must describe ranked candidates, not semantic bindings.
- NFR3 Large workspace safety: request-time work must be exact-name lookup plus capped candidate-file reads, never workspace scans.
- NFR4 Compatibility: existing completion, signature help, definition, references, semantic coloring, and include behavior must continue to pass tests.
- NFR5 Maintainability: comment cleanup and hover rendering must live in focused pure helpers with unit tests.
- NFR6 Graceful IO handling: unreadable candidate files, invalid paths, and missing index DBs must return reduced hover or no hover rather than request failure.
- NFR7 UI quietness: the references setting default must reduce noise for ordinary users.
- NFR8 Release readiness: package output must be a self-contained VSIX when packaging is not blocked by local dependency policy.

## 7. 技术方案

### Recommended: request-time Markdown hover over ranked indexed candidates

Add `query::hover` pure helpers for ranking hover candidates, extracting/cleaning leading comments, and assembling bounded Markdown. Add `server::hover` for LSP orchestration. Avoid schema migration by reading the candidate source text only for the few ranked candidates shown in hover.

Trade-off: hover documentation for unopened external files depends on readable source files at request time, but the feature ships quickly, keeps the index stable, and remains bounded.

### Alternative A: persist comments in SQLite

Extend parser and schema to store comments alongside symbols at index time.

Trade-off: faster hover and available docs even when source files move, but it requires schema migration, more parser surface, and a larger index before v1.1.0 proves value.

### Alternative B: local-file-only hover

Render comments only for symbols in the current open document.

Trade-off: simpler and very fast, but it misses the common navigation case where declarations live in headers and calls live in source files.

### Design decisions

- D1: Use the recommended request-time Markdown hover path.
- D2: Keep comment storage out of SQLite for v1.1.0.
- D3: Show at most four hover candidates.
- D4: Render comments best-effort and never let comment parsing block signature display.
- D5: Add one boolean extension setting, `fossilsense.references.showRanges`, default `false`.
- D6: Treat user authorization in this goal as approval to proceed through requirements, plan, implementation, verification, and report without further decision gates.

## 8. 需求跟踪矩阵

| Requirement | Sources | Scenarios | Design decisions | Plan tasks | Test/validation | Status |
|---|---|---|---|---|---|---|
| FR1 | UR1 | SC1, SC5 | D1 | Task 3 | `cargo test -p fossilsense server::hover -- --nocapture` | 已验证 |
| FR2 | UR1 | SC1 | D1 | Task 3 | `cargo test -p fossilsense server::hover -- --nocapture` | 已验证 |
| FR3 | UR4 | SC5 | D1 | Task 1, Task 3 | `cargo test -p fossilsense query::hover -- --nocapture` | 已验证 |
| FR4 | UR1, UR2 | SC1-SC5 | D1 | Task 2, Task 3 | `cargo test -p fossilsense query::hover -- --nocapture` | 已验证 |
| FR5 | UR2, UR3 | SC2, SC3 | D1, D4 | Task 2 | `cargo test -p fossilsense query::hover -- --nocapture` | 已验证 |
| FR6 | UR2 | SC2 | D4 | Task 2 | `cargo test -p fossilsense query::hover -- --nocapture` | 已验证 |
| FR7 | UR3 | SC4 | D4 | Task 2 | `cargo test -p fossilsense query::hover -- --nocapture` | 已验证 |
| FR8 | UR1, UR3 | SC1-SC5 | D3 | Task 1, Task 2, Task 3 | `cargo test -p fossilsense query::hover server::hover -- --nocapture` | 已验证 |
| FR9 | UR5 | SC1-SC6 | D5 | Task 3, Task 4 | hover Markdown assertions; `pnpm test` | 已验证 |
| FR10 | UR5 | SC6 | D5 | Task 4 | `pnpm test` | 已验证 |
| FR11 | UR6 | SC7 | D6 | Task 5 | `rg -n "1\\.0\\.[02]" README.md extensions/vscode/package.json crates/fossilsense/Cargo.toml` | 已验证 |
| FR12 | UR1-UR6 | SC1-SC7 | D1-D6 | Task 5 | README review; stale wording search | 已验证 |
| NFR1 | CLAUDE.md | SC1-SC7 | D1, D2 | Task 1-5 | dependency review; `cargo test -p fossilsense` | 已验证 |
| NFR2 | CLAUDE.md | SC5 | D1 | Task 1, Task 2, Task 5 | hover text assertions; README wording | 已验证 |
| NFR3 | CLAUDE.md | SC1-SC5 | D1, D3 | Task 1, Task 3 | code review; exact-name lookup tests; oversized file test | 已验证 |
| NFR4 | CLAUDE.md | SC7 | D6 | Task 6 | `cargo test`; TypeScript checks | 已验证 |
| NFR5 | CLAUDE.md | SC2-SC4 | D4 | Task 2 | focused query hover tests | 已验证 |
| NFR6 | CLAUDE.md | SC4, SC7 | D4 | Task 3, Task 6 | missing DB/file tests; oversized file fallback; index smoke | 已验证 |
| NFR7 | User request | SC6 | D5 | Task 4 | referencesView tests | 已验证 |
| NFR8 | CLAUDE.md | SC7 | D6 | Task 6 | `pnpm run package`; VSIX existence check | 已验证 |

## 9. 风险与缓解

| Risk | Impact | Likelihood | Mitigation | Trigger |
|---|---|---|---|---|
| R1 Comment extraction associates a nearby non-doc comment with a symbol | Slightly misleading hover prose | Medium | Treat blank/code lines as attachment boundaries; only accept comment-shaped block lines; keep signature and candidate evidence primary | Blank/code line between comment and symbol |
| R2 Doxygen syntax is richer than v1.1 parser | Some tags render plainly | High | Convert common commands and degrade unknown tags to readable text | Unknown `@command` appears |
| R3 Reading candidate files on hover adds latency | Hover feels slow on network or huge files | Medium | Cap candidates, comment scan, and candidate source file byte size; no workspace scan; tolerate read failures | Candidate file read exceeds practical delay |
| R4 Users interpret hover as exact binding | Trust mismatch | Medium | Use "FossilSense candidate" wording and tier/confidence/reason evidence | Multiple same-name candidates |
| R5 Hiding line suffixes makes duplicate reference rows less distinguishable | QuickPick rows may look similar | Medium | Keep role grouping and exact navigation; provide opt-in range setting | User needs precise row labels |
| R6 Packaging blocked by local pnpm approve-builds policy | VSIX not produced in this run | Medium | Record policy failure precisely; still verify Rust and TypeScript via direct commands when possible | pnpm install refuses ignored builds |

## 10. 用户确认记录

- 2026-07-04: User explicitly authorized autonomous next-version work, requested skill/subagent usage, requested a new branch, and stated no further decisions are required.
- 2026-07-04: This document records the selected autonomous scope for v1.1.0: Markdown hover, comment rendering, quiet grouped-reference labels, version bump, documentation, verification, and implementation report.
