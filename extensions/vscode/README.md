# FossilSense (VS Code)

Best-effort C/C++ navigation and analysis for large Windows workspaces where full
IntelliSense or `compile_commands.json` is unavailable. The extension bundles a
native Rust indexing engine - no external tools (ctags / cscope / clangd) required.

This VSIX is self-contained: the `fossilsense` engine binary ships inside the
extension's `bin/` folder.

## Current Capability

- Go to Definition / Workspace Symbols / Document Outline.
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
- Lightweight Completion: index-based and current-file word completion for C/C++.
  When the cursor is inside a detected function body, ordinary identifier
  completion also adds best-effort current-function parameters and local
  variables declared before the cursor from the open document snapshot. These
  structured local candidates are distinct from raw current-file word fallback;
  unsupported parse shapes degrade to the existing indexed and word completion.
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
  always available.
- Degraded Member Completion (`.`/`->`): returns struct/union fields only. It guesses
  the receiver's record type from a simple declaration in the current file and lists
  that record's fields (resolved from the global index, so cross-file definitions work);
  when the receiver cannot be inferred it falls back to prefix-matched global field
  names only (no subsequence), requires at least a 2-character prefix, caps the list,
  and marks it `isIncomplete` — a 1-char/empty prefix returns an empty incomplete list
  rather than dumping the whole index's fields. C-oriented; it never infers expression
  types, so it may list the wrong record's fields. Runs under
  `fossilsense.completion.mode`.
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
  enabled), `"on"` (enabled), `"off"` (never enabled). C/C++ language-server conflicts
  are reported by `fossilsense.mode`, not handled by silently disabling completion.
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
