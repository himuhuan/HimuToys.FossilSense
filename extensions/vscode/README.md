# FossilSense (VS Code)

Best-effort C/C++ navigation and analysis for large Windows workspaces where full
IntelliSense or `compile_commands.json` is unavailable. The extension bundles a
native Rust indexing engine - no external tools (ctags / cscope / clangd) required.

This VSIX is self-contained: the `fossilsense` engine binary ships inside the
extension's `bin/` folder.

v1.3.2 is a parser hygiene fix release. It stops trailing comments and string
literals from becoming fake type symbols, prefers AST-exact type-name ranges
when a usable syntax tree exists, rejects language keywords as Go to Definition
targets, and bumps the index schema so stale polluted rows are rebuilt. Project
context and other completion evidence remain behavior-preserving outside this
fix.

## Current Capability

- Go to Definition / Workspace Symbols / Document Outline.
- One-hop Call Relations for C/C++ free functions: VS Code's standard Call
  Hierarchy is available, and **FossilSense: Show Call Relations** opens two
  native Explorer views. The first switches between incoming/outgoing targets;
  selecting one navigates to it and populates the second view with exact call
  sites, confidence, evidence, coverage, and budget state. Unsaved calls from
  every open document in the workspace participate. Member calls, function
  pointers, callable objects, macros, templates, and overload binding remain
  explicit unresolved/unsupported evidence rather than fabricated targets.
- Find All References: searches the workspace for whole-word textual matches of
  the identifier under the cursor and populates the standard References panel,
  ordered by best-effort syntactic role (definitions and declarations first). The
  standard panel shows locations only; for visible per-hit role labels run the
  **FossilSense: Find References (Grouped by Role)** command, which lists the same
  hits grouped and labeled by role (definition / declaration / call / write / type
  / read). The grouped QuickPick hides `:line` suffixes by default; enable
  `fossilsense.references.showRanges` to show them. Roles are syntactic guesses,
  not resolved bindings.
- Rich Hover: hovering an indexed identifier shows a Markdown candidate view with
  the stored signature, candidate tier/confidence/reason, and immediately leading
  Doxygen or ordinary comments when they can be recovered. This is a ranked
  exact-name candidate display, not type-aware semantic binding; unsupported or
  unreadable comment sources degrade to signature-only hover.
- Lightweight Completion: index-based, current-file overlay, and current-file
  word completion for C/C++. When the cursor is inside a detected function body,
  ordinary identifier completion also adds best-effort current-function
  parameters and local variables declared before the cursor from the open
  document snapshot. Current open-document facts such as unsaved macros,
  typedef/using aliases, enum constants, function declarations/definitions, and
  record/type definitions can also participate as structured current-file
  overlay evidence. Nearby identifier usage can raise text fallback candidates,
  but raw words remain visibly textual fallback and are not semantic bindings.
  A bounded static language-builtin source also offers common C/C++ keywords,
  builtin-style types, and literal-like constants such as `struct`, `sizeof`,
  `size_t`, `uint32_t`, and `NULL` when matching the current prefix. These items
  are fallback completion candidates with `keyword`, `builtin type`, or
  `builtin constant` details; they do not create index records, definition
  targets, workspace symbols, semantic tokens, or auto-include edits. Unsupported
  parse shapes degrade to the existing indexed, builtin, and word completion.
  The list is always marked `isIncomplete` so the editor re-queries with the full
  current prefix on every keystroke — longer-named symbols that fell outside the
  truncated top-N re-enter the window as you keep typing, and an empty first batch
  never sticks. Short prefixes (1–2 chars) only return exact / prefix /
  word-boundary-substring matches to avoid a noise tail; 3+ chars restore full
  fuzzy recall (including camelCase-initials subsequences). The hot path is
  in-memory only (symbol kind is cached), so each keystroke is one scan with zero
  disk I/O. The token currently being typed is not echoed back as a word candidate;
  raw current-file words are fallback text items and do not receive current-file scope
  priority over reachable or external indexed symbols. Indexed candidates outside the
  current file carry a best-effort scope tag in the item detail (`reachable` /
  `external` / `global` / `ambiguous`) with the full tier/confidence/reason in the
  item documentation; indexed current-file candidates are left unlabeled. These are
  ranked candidates, not semantic bindings or overload resolution.
  In v1.2.0, ordinary identifier completion uses the Phase 4-6 smart-completion
  pipeline: candidates are merged by evidence, de-duplicated, ranked with a
  deterministic evidence-aware ranker, and truncated. `ScopeTier` is now a soft
  prior for ordinary completion rather than the final strict packed ordering, and
  guard bands keep low-confidence global/text fallback from jumping ahead without
  strong current/local evidence. Lightweight rule-based intent ranking lifts
  type, expression-value, call-target, macro-preprocessor, and declaration-name
  candidates when local lexical cues support that context. Intent is only ranking
  evidence: it does not perform C/C++ type inference, overload resolution, or
  semantic binding, and it never hard-filters candidates. Indexed recall is now
  bounded multi-channel recall, preserving representation from reachable,
  external, unknown/open-scope, global, current/local, and text evidence before
  final reranking. Verbose perf logs report timings, source counts, intent bucket,
  recall channel counts, guard summaries, and shadow-rank movement without
  candidate names or snippets; language-builtin evidence is reported only as an
  aggregate source count. In v1.2.1, ordinary completion can also use
  local-only accepted-completion history as a small bounded ranking signal keyed
  by anonymous candidate hash, kind, intent, and prefix bucket. It is workspace
  local, clearable, disableable, and records positive accept feedback only. No
  telemetry, cloud sync, ML ranking, or auto-include insertion is enabled.
  Build-marker project context adds a separate bounded indexed recall/ranking
  signal. Automatic mode uses the completion URI's nearest ancestor project;
  duplicate labels can take their function/macro/type presentation from that
  project when stronger source/scope/confidence evidence is equal. Cross-project
  candidates remain eligible, and `Unspecified`, `off`, no-marker, or unavailable
  project models preserve baseline completion items and ordering.
- Project Context Status: a dedicated status item shows Auto, a manual project,
  Unspecified, Off, none, or unavailable. Its selector offers **Current Project
  (Auto)**, every discovered workspace-relative project, and **Unspecified**.
  Supported markers are Make/GNUmake, CMakeLists, QMake `.pro`, main
  `build.ninja`, Visual Studio solution/project files, `meson.build`, and Bazel
  BUILD/WORKSPACE main files. Discovery respects `.gitignore`, workspace scope,
  and default generated-directory exclusions. Marker contents are never parsed.
- Best-effort Signature Help: inside simple function calls, shows exact-name
  indexed function signatures ranked by the same include reachability tiers as
  Go to Definition. Candidates are hints, not overload resolution; there is no
  argument type matching, template or namespace lookup, function-like macro
  expansion, or function-pointer target inference. Unsupported call shapes or
  unsplittable signatures degrade to empty or whole-signature results.
- Limited Include Analysis (`fossilsense.includePaths`): point at external header
  directories (e.g. a MinGW/TDM-GCC or SDK include tree) to get header-path completion
  inside `#include "…"`/`<…>`, jump-to-header on an include line (ranked candidate list
  when ambiguous), and indexing of those headers' symbols (searchable but ranked after
  workspace symbols; first-layer directly-included headers also feed coloring). No
  preprocessor, no conditional/macro evaluation, no transitive include graph. Missing or
  wrong-platform headers are skipped — FossilSense never compiles, so they cannot error.
  Include-path completion runs under `fossilsense.completion.mode`; jump-to-header is
  always available. Completion ranking retains the quote/angle search-order prior
  and then applies bounded same-directory, sibling/component edge, recent include,
  basename frequency, and path-depth evidence. Include perf logs expose counts for
  those ranking signals without raw include paths.
- Degraded Member Completion (`.`/`->`): returns owner-scoped fields and first-version
  C++ method evidence for structs/classes/unions. It guesses the receiver's record type
  from simple declarations in the current file and can use a narrow weak receiver
  correlation when the indexed owner name is unique; cross-file definitions work through
  the member index. When the receiver cannot be inferred it falls back to prefix-matched
  global member names only (no subsequence), requires at least a 2-character prefix, caps
  the list, and marks it `isIncomplete` — a 1-char/empty prefix returns an empty
  incomplete list rather than dumping the whole index's members. This is owner evidence,
  not full C++ binding: no inheritance, overload resolution, templates, namespaces,
  access control, or expression type inference. Runs under `fossilsense.completion.mode`.
- Degraded Semantic Coloring: colors known macros, types (typedef / struct /
  enum / union / class), enum constants, and best-effort current-function
  parameters/local variables from the open document snapshot. Everything else —
  including struct/union fields — is left to the editor's TextMate grammar.
  Multi-meaning names are colored by a best guess and may occasionally be mis-colored. Runs under
  `fossilsense.semanticColoring.mode`.
- Workspace scope configuration: optional root-level `fossilsense.json` controls
  `include`, `exclude`, and `extensions` for both indexing and reference search.
- Manual index commands: `FossilSense: Refresh Index` is incremental;
  `FossilSense: Full Rebuild Index` forces a full in-scope rescan.
- Performance hardening: file/config events are scope-filtered, debounced, and
  coalesced before indexing; unchanged files skip before content reads; parser
  concurrency is bounded at up to 8 workers with enlarged stacks for deeply nested C files;
  full rebuilds use batched SQLite bulk-load; index completion logs timing metrics
  to the output panel.

References are best-effort text candidates with syntactic-role grouping, not resolved
semantic references. FossilSense has no compile parameters, so "Find All References"
performs a case-sensitive whole-word search over C/C++ source files; it does not filter
occurrences inside comments or string literals. Each hit is then classified with a
best-effort syntactic role (definition / declaration / call / write / type / read;
unparseable hits fall back to read) and the results are ordered by role. The standard
References panel renders locations only — it carries no per-item role label — so use the
**FossilSense: Find References (Grouped by Role)** command to see the roles. The grouped
QuickPick keeps labels quiet by default and still navigates to the full returned range.
Results are capped at 2000 matches; if capped, a message is logged to the output panel.

## Configuration (`fossilsense.json`)

Place this optional file at the workspace root:

```json
{
  "include": ["src/", "include/"],
  "exclude": ["src/generated/"],
  "extensions": ["c", "h", "cpp", "hpp"]
}
```

All fields are optional. Missing config means full-repo default scope. Bad JSON or invalid
field types fall back to defaults and show a warning in the output panel and status bar.

## Coexistence

FossilSense is a best-effort navigation engine, not a semantic compiler model. It can
be installed alongside clangd, Microsoft C/C++, and CMake Tools, but only one C/C++
language provider should be primary in a workspace. When a known C/C++ language server
is detected, FossilSense shows a one-time mutual-exclusion warning and offers to stop
its server or open settings. Different or duplicate navigation results are expected
because FossilSense returns ranked text/index candidates.

## Settings

- `fossilsense.serverPath` - override the engine binary path (default: bundled binary).
- `fossilsense.mode` - overall server mode: `"auto"` (default, start and show a
  one-time mutual-exclusion warning when clangd / C/C++ Tools / ccls is detected),
  `"on"` (start without that warning), `"off"` (do not start).
- `fossilsense.includePaths` - array of absolute external header reference directories
  (e.g. `C:\\TDM-GCC-64\\x86_64-w64-mingw32\\include`) for `#include` completion,
  jump-to-header, and degraded symbol indexing. Distinct from workspace scope `include`.
  Changing it restarts the server. Empty by default (no behavior change).
- `fossilsense.completion.mode` - controls FossilSense completion: `"auto"` (default,
  enabled), `"on"` (enabled), `"off"` (never enabled). This includes ordinary
  identifier completion, include-path completion, and degraded `.`/`->` field/method
  member completion. C/C++ language-server conflicts are reported by `fossilsense.mode`,
  not handled by silently disabling completion.
- `fossilsense.completionHistory.mode` - controls local-only accepted-completion
  history: `"auto"` (default, enabled), `"on"` (enabled), `"off"` (do not record or
  rank with history). History stays in the local workspace cache, is bounded, stores
  anonymous candidate hashes/buckets rather than raw labels or source, and can be removed
  with **FossilSense: Clear Completion History**.
- `fossilsense.projectContext.mode` - controls build-marker project evidence for
  ordinary identifier completion: `"auto"` (default), `"promptOnAmbiguous"`
  (prompt once per active local C/C++ URI when projects exist but Auto cannot
  resolve one), or `"off"` (strict baseline behavior). Status-bar choices are
  workspace-local and survive reload; `off` temporarily overrides but does not
  erase a saved valid choice. This setting does not affect definitions,
  references, coloring, workspace symbols, Hover, Signature Help, member, or
  include completion.
- `fossilsense.semanticColoring.mode` - controls FossilSense semantic coloring of macros,
  types, and enum constants: `"auto"` (default, enabled), `"on"` (enabled), `"off"`
  (never enabled). C/C++ language-server conflicts are reported by `fossilsense.mode`,
  not handled by silently disabling coloring.
- `fossilsense.references.showRanges` - when `true`, the grouped references QuickPick
  shows `:line` suffixes in row labels. Default `false` keeps labels focused on
  relative paths; selecting a row still navigates to the exact returned range.
- `fossilsense.debug.candidateReasons` - when `true`, **Go to Definition** logs each
  candidate's scope tier, confidence, and reason to the FossilSense output panel so you
  can see why candidates ranked as they did. A best-effort debug aid that never changes
  which definitions are returned. Default `false`; changing it restarts the server.
- `fossilsense.trace.server` - LSP protocol tracing (`off` / `messages` / `verbose`).

When `fossilsense.mode` is `"auto"` and a conflicting C/C++ extension is detected,
FossilSense shows a single warning and logs to the FossilSense output channel. Use the
warning action, `FossilSense: Stop Server`, or `fossilsense.mode: "off"` to make another
provider primary for that workspace.

See the repository `README.md` and `CLAUDE.md` for design, usage, and release status.
