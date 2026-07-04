# Function Local Completion Requirements

Status: requirements-approved
Date: 2026-07-04
Feature brief name: function-local-completion

## 1. 需求来源和背景

- User request, 2026-07-04: "考虑有限纳入函数内的参数、局部变量的提示，这里是指用户光标所在的函数内部". The requested outcome is a bounded completion enhancement for parameters and local variables inside the function that contains the user's cursor.
- User decision, 2026-07-04: selected `function-local-completion` as the feature brief name.
- User decision, 2026-07-04: selected phase-one surface "仅标识符补全", meaning this requirement covers ordinary identifier completion only.
- User decision, 2026-07-04: selected local visibility policy "声明早于光标", meaning local variable candidates are limited to parameters and declarations whose declared identifier starts before the cursor.
- User approval, 2026-07-04: approved the presented requirements design for writing to this document.
- `AGENTS.md` delegates project instructions to `CLAUDE.md`.
- `CLAUDE.md` positions FossilSense as a best-effort C/C++ navigation and analysis tool for large Windows workspaces without reliable compile environments. This feature must not require clangd, compile commands, ctags, compiler arguments, or a build-system model.
- `CLAUDE.md` requires completion behavior to preserve candidate honesty: FossilSense must not present heuristic local candidates as compiler-grade semantic bindings, and completion changes must document can/cannot/fallback behavior.
- `CLAUDE.md` requires completion hot paths to use in-memory state, keep `isIncomplete = true`, recompute top-N against the current full prefix, and avoid disk IO per keystroke.
- `README.md` and `extensions/vscode/README.md` describe current lightweight completion as index-based plus current-file word completion. The current local word path is fallback text completion and does not distinguish current-function parameters or local variables.
- Code inspection shows ordinary completion is implemented in `crates/fossilsense/src/server/language_server.rs`, combines in-memory `NameTable` hits with cached current-file words, deduplicates through `CompletionCandidateSource`, and keeps completion lists incomplete.
- Code inspection shows `crates/fossilsense/src/server.rs` currently treats raw local words as `CompletionCandidateSource::LocalWord` and prefers indexed same-name candidates over local words.
- Code inspection shows `crates/fossilsense/src/server.rs` has `get_or_parse_document`, a live parse cache keyed by open-document version, already used by request-time features such as member completion and suitable for unsaved local binding extraction.
- Code inspection shows `crates/fossilsense/src/parser.rs` and `crates/fossilsense/src/parser/ast.rs` already collect `local_declarations` for record-typed local/parameter declarations used by member completion receiver inference, but this model is too narrow for ordinary local variable completion because primitive, pointer, array, and non-record locals are excluded.
- The repository does not contain `templates/requirements-template.md`; this document follows the required section structure used by the existing requirements documents.

## 2. 用户需求

- UR1: While editing inside a C/C++ function body, the user can see the current function's parameters in ordinary identifier completion.
- UR2: While editing inside a C/C++ function body, the user can see local variables declared before the cursor in ordinary identifier completion.
- UR3: Current-function local candidates are bounded and do not turn into all-file word promotion or whole-workspace local inference.
- UR4: Local candidates are useful enough to rank ahead of raw current-file word fallback and same-name non-local candidates, while still remaining best-effort completion candidates rather than semantic bindings.
- UR5: The enhancement works with unsaved open-document content and does not depend on a completed index rebuild for the current edit.
- UR6: Existing completion behavior remains stable for include completion, member completion, indexed symbol completion, short-prefix filtering, truncation, and degraded fallback.

## 3. 范围与非范围

In scope:

- Enhance ordinary identifier completion only.
- Activate local binding completion only when the cursor is inside a detected function body in an open C/C++ document.
- Include function parameters and local variable declarations whose declared identifier starts before the cursor.
- Use the open document text, including unsaved changes, through the existing live parse cache or an equivalent request-time parse path.
- Support common C/C++ declarator shapes where a binding identifier can be conservatively extracted, including primitive variables, typedef-named variables, record-typed variables, pointers, arrays, initialized declarators, and parameters.
- Preserve the existing raw current-file word fallback for names that are not recognized as structured local bindings.
- Deduplicate same-name completion candidates so a current-function local binding can win over raw local words and non-local indexed candidates.
- Keep `CompletionList.isIncomplete = true`.
- Update user-facing documentation to describe current-function local completion and its best-effort limits.
- Add focused tests for extraction, filtering, ranking, deduplication, and degraded fallback.

Out of scope:

- Hover, signature help, references, go-to-definition, semantic coloring, include completion, and member completion behavior changes.
- Compile-grade C/C++ scope analysis.
- Full block-scope lifetime modeling for nested `{}` blocks.
- Argument type inference, expression type inference, overload resolution, templates, namespaces, inheritance, access control, and preprocessor branch evaluation.
- Persisting local variables or parameters into SQLite.
- Workspace scans or disk reads on each completion request.
- Auto-import, snippets, diagnostics, or rename support.

## 4. 场景与预期

| ID | Role | Trigger | Preconditions | Action | Expected result |
|---|---|---|---|---|---|
| SC1 | C/C++ developer | Types `cou` inside `void f(int count) { cou }` | Cursor is inside the function body | Request ordinary identifier completion | FossilSense includes `count` as a parameter candidate with a variable-like kind and current-function evidence. |
| SC2 | C/C++ developer | Types `cur` after `int cursor_limit;` | The local declaration appears before the cursor in the same function | Request ordinary identifier completion | FossilSense includes `cursor_limit` as a local variable candidate. |
| SC3 | C/C++ developer | Types a prefix before a later declaration | The same function declares `future_value` after the cursor | Request ordinary identifier completion | `future_value` is not added as a structured local candidate because its declaration starts after the cursor. |
| SC4 | C/C++ developer | Types inside file scope, a struct body, or an unsupported parse position | No enclosing function body can be detected | Request ordinary identifier completion | FossilSense keeps existing indexed and raw local word completion behavior without structured function-local candidates. |
| SC5 | C/C++ developer | Types a name that matches both a local variable and an indexed symbol | A current-function local binding and non-local indexed candidate share the same label | Request ordinary identifier completion | The local binding wins same-name deduplication because it has current-function evidence. |
| SC6 | C/C++ developer | Types inside a member access or include directive | Cursor is in `obj.` / `obj->` or `#include "..."` context | Request completion | Existing include or member completion paths take precedence; structured function-local identifier completion does not run. |
| SC7 | C/C++ developer | Edits an unsaved function | The local variable exists only in the open document buffer | Request ordinary identifier completion | FossilSense can suggest the local binding from the open document without waiting for indexing. |
| SC8 | C/C++ developer | Works in malformed or partially typed code | Tree-sitter parse is missing the needed function/declaration shape | Request ordinary identifier completion | FossilSense degrades to existing indexed and raw local word completion rather than failing the request. |

## 5. 功能性需求

- FR1 Surface gating: the feature must affect ordinary identifier completion only. Include-path completion and member completion must continue to short-circuit before function-local completion.
- FR2 Enclosing function detection: completion must detect whether the cursor is inside a function body in the current open document. If no function body can be detected, no structured function-local candidates are added.
- FR3 Parameter extraction: when inside a function body, completion must extract parameter binding names from the containing function and include those that match the current prefix.
- FR4 Local declaration extraction: completion must extract local variable binding names from declarations inside the containing function and include only declarations whose identifier start byte is before the cursor byte offset.
- FR5 Conservative declarator handling: extraction must only add a candidate when it can identify a binding identifier. Complex or unsupported declarations must be skipped or reduced without request failure.
- FR6 Candidate shape: local binding completion items must use an appropriate variable-like LSP completion kind where possible, and may include concise detail such as `parameter`, `local`, or recovered type text.
- FR7 Ranking and deduplication: structured function-local candidates must rank ahead of raw current-file word fallback and same-name non-local indexed candidates. They must not cause raw local words to outrank reachable or current indexed candidates with different names through unrelated score inflation.
- FR8 Prefix filtering: local binding candidates must use the existing identifier completion prefix and short-prefix recall policy, or an equivalent policy with the same noise limits.
- FR9 Incomplete list behavior: responses must remain `isIncomplete = true`, including empty and truncated results.
- FR10 Unsaved document support: local binding extraction must use the current open document snapshot rather than the persisted indexed file.
- FR11 Degraded behavior: missing roots, missing indexes, parse fallback, malformed declarators, unsupported cursor positions, and unavailable live parse results must return existing completion behavior or an empty incomplete list rather than a failed request.
- FR12 Documentation update: repository and extension documentation must describe the new current-function local completion behavior, its limits, and the fact that candidates remain best-effort.

## 6. 非功能性需求

- NFR1 No external toolchain: the feature must not require clangd, a compiler, compile commands, ctags, or build-system metadata.
- NFR2 No storage migration: local bindings for this feature must not be persisted in SQLite in phase one.
- NFR3 Completion hot-path safety: a completion request must not scan the workspace or perform broad disk IO. Work must be bounded to current open-document parsing or cached parse data plus existing in-memory completion tables.
- NFR4 Large workspace safety: local binding work must be proportional to the current document and enclosing function, not total workspace size.
- NFR5 Honest best-effort semantics: UI text and documentation must not describe local candidates as exact compiler scope resolution.
- NFR6 Compatibility: existing tests for indexed completion, include completion, member completion, signature help, hover, references, semantic coloring, and reachability ranking must continue to pass.
- NFR7 Maintainability: local binding extraction, filtering, and candidate ranking must live in focused helpers with unit tests rather than being embedded as opaque logic in the LSP handler.
- NFR8 Noise control: the feature must preserve short-prefix filtering and avoid promoting arbitrary current-file words to current-function local bindings.

## 7. 技术方案

### Recommended: request-time current-function local bindings

Extend the request-time parser facts to expose structured local bindings for ordinary completion:

- Add a parser-facing local binding model that can represent parameters and local variables, including name, binding kind, declaration byte offset, enclosing function byte range, and optional type text.
- Reuse or extend the existing `local_declarations` collection rather than creating a parallel semantic model disconnected from member completion.
- Add pure helper logic that filters local bindings by cursor byte offset, enclosing function range, prefix match, and declaration-before-cursor policy.
- Add a new completion candidate source such as `LocalBinding` so structured local candidates are distinct from raw `LocalWord`.
- Inject local binding candidates into the ordinary completion path after include/member context checks and before raw local word fallback.
- Adjust deduplication so `LocalBinding` beats `LocalWord` and same-name non-local indexed candidates, while preserving indexed semantic kind/detail when no structured current-function binding exists.
- Keep the completion response incomplete and capped by `COMPLETION_LIMIT`.
- Cover parser extraction and completion ordering with focused unit tests.

Trade-off: this gives immediate value for unsaved code without schema migration and keeps the feature aligned with FossilSense's current request-time best-effort model. It does not attempt full C/C++ scope resolution, so some nested block and shadowing edge cases remain approximate.

### Alternative A: boost current-function raw words

Detect the containing function range textually and give words found in that range a higher local-word score.

Trade-off: this is small, but it cannot distinguish parameters and variables from ordinary words, type names, macro names, or incidental identifiers. It would blur the project distinction between structured candidates and raw text fallback.

### Alternative B: persist local bindings in SQLite

Index parameters and local variables into the store and query them during completion.

Trade-off: persisted local bindings would be queryable without parsing the open document, but they would be stale for unsaved edits, require schema migration, and enlarge incremental invalidation surface for data that is only meaningful at request time.

### Alternative C: full block-scope local completion

Track exact nested block scope, shadowing, `for` initializers, and lifetime boundaries inside the current function.

Trade-off: this is more precise, but it expands the first phase into compile-like scope modeling. It is better deferred until current-function locals prove useful.

### Design decisions

- D1: Use the recommended request-time current-function local binding path.
- D2: Limit phase one to ordinary identifier completion.
- D3: Include parameters and local declarations whose identifier starts before the cursor.
- D4: Do not persist locals in SQLite.
- D5: Treat local binding candidates as best-effort current-function evidence, not semantic bindings.
- D6: Preserve existing include/member completion precedence and existing raw local word fallback.

## 8. 需求跟踪矩阵

| Requirement | Sources | Scenarios | Design decisions | Plan tasks | Test/validation | Status |
|---|---|---|---|---|---|---|
| FR1 | UR6, `CLAUDE.md` completion compatibility | SC6 | D2, D6 | Task 4 completion integration; Task 6 integration verification | `cargo test -p fossilsense server::tests::local_binding_pipeline_uses_open_document_bindings_before_local_words -- --nocapture`; manual include/member smoke | 已计划 |
| FR2 | UR1, UR2, user clarification "光标所在的函数内部" | SC1, SC2, SC4, SC8 | D1, D2 | Task 1 parser local binding model; Task 2 filtering | `cargo test -p fossilsense parser::tests::local_bindings -- --nocapture`; `cargo test -p fossilsense query::local_completion -- --nocapture` | 已计划 |
| FR3 | UR1 | SC1, SC7 | D1, D3 | Task 1 parser local binding model; Task 2 filtering; Task 4 completion integration | `cargo test -p fossilsense parser::tests::local_bindings -- --nocapture`; `cargo test -p fossilsense server::tests::local_binding_pipeline_uses_open_document_bindings_before_local_words -- --nocapture` | 已计划 |
| FR4 | UR2, user selected "声明早于光标" | SC2, SC3, SC7 | D1, D3 | Task 1 parser local binding model; Task 2 filtering; Task 4 completion integration | `cargo test -p fossilsense query::local_completion -- --nocapture`; manual before/after declaration smoke | 已计划 |
| FR5 | UR3, `CLAUDE.md` graceful degradation | SC8 | D5 | Task 1 parser local binding model; Task 2 filtering | `cargo test -p fossilsense parser::tests::local_bindings_are_empty_on_lexical_fallback -- --nocapture`; `cargo test -p fossilsense query::local_completion -- --nocapture` | 已计划 |
| FR6 | UR4 | SC1, SC2 | D5 | Task 3 candidate source and rendering | `cargo test -p fossilsense server::tests::local_binding_candidates_render_variable_kind_and_detail -- --nocapture` | 已计划 |
| FR7 | UR4, current dedup code inspection | SC5 | D5, D6 | Task 3 candidate source and dedup; Task 4 completion integration | `cargo test -p fossilsense server::tests::completion_dedup_keeps_local_binding_over_same_name_indexed_and_local_word -- --nocapture` | 已计划 |
| FR8 | UR3, `CLAUDE.md` short-prefix completion rules | SC1-SC3 | D6 | Task 2 filtering; Task 4 completion integration | `cargo test -p fossilsense query::local_completion -- --nocapture`; `cargo test -p fossilsense query::text::tests::local_word_short_prefix_rejects_plain_substring -- --nocapture` | 已计划 |
| FR9 | `CLAUDE.md` completion rules | SC1-SC8 | D6 | Task 4 completion integration; Task 6 integration verification | `cargo test -p fossilsense server::tests::completion_memo -- --nocapture`; `cargo test -p fossilsense` | 已计划 |
| FR10 | UR5, live parse cache code inspection | SC7 | D1, D4 | Task 1 parser local binding model; Task 4 completion integration | `cargo test -p fossilsense server::tests::local_binding_pipeline_uses_open_document_bindings_before_local_words -- --nocapture`; manual unsaved document smoke | 已计划 |
| FR11 | UR6, `CLAUDE.md` degradation policy | SC4, SC8 | D5, D6 | Task 1 parser local binding model; Task 2 filtering; Task 4 completion integration | `cargo test -p fossilsense query::local_completion -- --nocapture`; `cargo test -p fossilsense parser::tests::local_bindings_are_empty_on_lexical_fallback -- --nocapture` | 已计划 |
| FR12 | `CLAUDE.md`, README current capability text | SC1-SC8 | D5 | Task 5 documentation | `rg -n "current-function|局部变量|local variables declared before the cursor" README.md extensions/vscode/README.md`; `cd extensions/vscode && pnpm run compile` | 已计划 |
| NFR1 | `CLAUDE.md`, README positioning | SC1-SC8 | D1, D4 | Task 5 documentation; Task 6 integration verification | Dependency review; `cargo test -p fossilsense` | 已计划 |
| NFR2 | Code inspection of SQLite-backed index | SC7 | D4 | Task 1 parser local binding model; Task 6 integration verification | Schema review showing no migration; `cargo run -p fossilsense -- index samples/mini-c --db target/function-local-completion-mini.sqlite --force` | 已计划 |
| NFR3 | `CLAUDE.md` completion hot path | SC1-SC8 | D1, D4 | Task 2 filtering; Task 4 completion integration | Code review for no workspace scan/disk IO; `cargo test -p fossilsense server::tests::completion_memo -- --nocapture` | 已计划 |
| NFR4 | Large workspace positioning | SC1-SC8 | D1 | Task 1 parser model; Task 2 filtering | `cargo test -p fossilsense query::local_completion -- --nocapture`; parser tests bounded to current document/function helpers | 已计划 |
| NFR5 | `CLAUDE.md` candidate-not-binding rule | SC5, SC8 | D5 | Task 3 completion item rendering; Task 5 documentation | `cargo test -p fossilsense server::tests::local_binding_candidates_render_variable_kind_and_detail -- --nocapture`; README wording review | 已计划 |
| NFR6 | Existing feature compatibility | SC6 | D2, D6 | Task 4 completion integration; Task 6 verification | `cargo test -p fossilsense`; `cd extensions/vscode && pnpm run compile`; manual include/member smoke | 已计划 |
| NFR7 | `CLAUDE.md` maintainability rules | SC1-SC8 | D1 | Task 1 parser model; Task 2 query helper; Task 3 server helper; Task 4 integration | Focused parser/query/server tests listed in Tasks 1-4 | 已计划 |
| NFR8 | `CLAUDE.md` short-prefix and noise rules | SC1-SC3 | D3, D6 | Task 2 filtering; Task 4 completion integration | `cargo test -p fossilsense query::local_completion -- --nocapture`; `cargo test -p fossilsense query::text::tests::local_word_short_prefix_rejects_plain_substring -- --nocapture` | 已计划 |

## 9. 风险与缓解

| Risk | Impact | Likelihood | Mitigation | Trigger |
|---|---|---|---|---|
| R1 Tree-sitter cannot reliably detect the containing function in malformed code | Missing local candidates | Medium | Return existing completion behavior when no function body is detected | Cursor is in incomplete function syntax or parse fallback |
| R2 Complex declarators produce wrong binding names | Misleading local candidate | Medium | Only add a candidate when `declarator_identifier` or equivalent conservative extraction finds a clear identifier | Function pointer, macro-expanded, or unsupported declarator shape |
| R3 Current-function approximation ignores nested block lifetime | Candidate appears after leaving an inner block | Medium | Phase one documents current-function, declaration-before-cursor semantics; full block scope remains out of scope | Local declaration appears in an inner block before cursor but outside current lexical block |
| R4 Local binding beats an indexed same-name symbol the user wanted | Ranking surprise | Medium | Restrict this priority to current-function bindings only and keep detail labeling as `local` or `parameter` | Same-name local and global/indexed symbol exist |
| R5 Local binding extraction slows completion | Completion latency | Low | Reuse live parse cache, avoid disk IO, and keep helper work bounded to current document/function | Large open file with frequent completion requests |
| R6 Documentation overstates precision | User trust mismatch | Medium | Use best-effort wording and explicitly state no compile-grade scope analysis | README or extension README implies exact semantic binding |

## 10. 用户确认记录

- 2026-07-04: User requested bounded inclusion of parameters and local variables inside the function containing the cursor.
- 2026-07-04: User selected `function-local-completion` as the feature brief name.
- 2026-07-04: User selected ordinary identifier completion as the phase-one feature surface.
- 2026-07-04: User selected the visibility policy that only parameters and local declarations before the cursor are included.
- 2026-07-04: User approved the requirements design before this document was written.
