# FossilSense

FossilSense is a best-effort C/C++ navigation and analysis engine for large Windows workspaces where full IntelliSense or `compile_commands.json` is unavailable.

## Version facts

- Rust engine crate: `crates/fossilsense/Cargo.toml` (`1.0.0` at this snapshot).
- VS Code package / VSIX: `extensions/vscode/package.json` (`1.0.2` at this snapshot).
- The VSIX version is the installable extension package version. It bundles the Rust engine binary built from this workspace; the two version numbers are intentionally tracked separately until the release process changes them together.

Current baseline capabilities: workspace symbol search, document outline, ranked
go-to-definition candidates, **role-grouped find-all-references** (text candidates
grouped by best-effort syntactic role), **lightweight completion**
(including degraded `.`/`->` member completion), **best-effort signature help /
parameter hints** for indexed functions, **degraded semantic coloring**
(macros, types, and enum constants), **limited `#include` analysis** (header-path
completion, jump-to-header, and indexing of external reference headers), optional
workspace scope configuration via `fossilsense.json`, manual incremental refresh /
full rebuild commands, debounced/coalesced reindex scheduling, CLI/LSP timing
metrics, and self-contained VSIX packaging.

- a Rust `fossilsense` binary with `scan`, `index`, and `lsp` modes
- a VS Code extension that starts the local language server
- a tiny C sample workspace for end-to-end checks
- a persistent SQLite symbol index with incremental rebuilds
- workspace scope configuration via `fossilsense.json`
- manual incremental refresh and full rebuild commands
- debounced/coalesced LSP reindex scheduling
- CLI/LSP index timing metrics for release checks
- lightweight text/index-level completion with active C/C++ provider conflict reporting
- degraded `.`/`->` member completion: a current-file receiver-type guess narrows
  to the record's fields, with a global field fallback (pure-C oriented)
- degraded semantic coloring of macros, types, and enum constants
- limited `#include` analysis: header-path completion inside `#include "…"`/`<…>`,
  jump-to-header on an include line (candidate list when ambiguous), and indexing of
  external reference headers from `includePaths` (searchable but ranked after workspace)

## Include analysis (`includePaths`)

Point FossilSense at one or more **external** header reference directories (e.g. a
MinGW/TDM-GCC or SDK include tree) to get `#include` path completion, jump-to-header,
and degraded symbol coverage of those headers. Set it as a VS Code array setting
`fossilsense.includePaths`, or as `includePaths` in `fossilsense.json`.

- **What it does**: file-name/sub-path completion inside `#include` delimiters (quote
  vs angle affects ranking only); go-to-definition on an `#include` line opens the
  resolved header (multiple matches return a ranked candidate list); external header
  symbols become searchable/completable but rank **after** workspace symbols. Headers
  directly `#include`d by a workspace file (the "first layer") also feed semantic
  coloring; transitively-included ones do not.
- **What it does not do**: no preprocessor, no `#if`/conditional evaluation, no macro
  expansion to pick a winning include, no transitive include graph, no auto-detection
  of system include paths (you point at them explicitly).
- **Never an error**: missing/invalid/duplicate paths are skipped with a note. Because
  FossilSense never compiles, mismatched-platform headers (e.g. MinGW headers while
  targeting Linux) are inert reference text — they cannot cause an error. Oversized
  roots degrade to path-resolution-only (no symbols) rather than stalling.
- Distinct from scope `include` below, which selects *workspace* subtrees.

## Configuration (`fossilsense.json`)

Place an optional `fossilsense.json` at the workspace root to control which files
enter the index and reference search. All fields are optional; missing files or
malformed JSON fall back to defaults with a visible warning.

```json
{
  "include": ["src/", "include/"],
  "exclude": ["src/generated/"],
  "extensions": ["c", "h", "cpp", "hpp"]
}
```

| Field | Type | Default | Description |
|---|---|---|---|
| `include` | `string[]` | `[]` (all repo) | Subtree prefixes to limit scanning. Non-empty = only files under these directories. |
| `exclude` | `string[]` | `[]` | Directory prefixes to exclude, applied on top of `.gitignore` and built-in excludes. |
| `extensions` | `string[]` | `["c","h","cpp","hpp","cc","hh","cxx","hxx","inl"]` | File extensions to include. Leading `.` is stripped and comparison is case-insensitive. Providing this field replaces the default set. |
| `includePaths` | `string[]` | `[]` | **Absolute** external header reference directories (see *Include analysis* above). Distinct from `include`. Merged with the VS Code `fossilsense.includePaths` setting; invalid entries are skipped. |

- Paths are repository-relative, `/`-separated, case-insensitive (ASCII).
- `include`/`exclude` use segment-boundary prefix matching: `"src"` matches `src/a.c` but NOT `src_gen/b.c`.
- If `fossilsense.json` is deleted, the next reindex restores full-repo scope.
- Configuration changes (create/edit/delete) trigger automatic reindexing.

## Commands

| Command | Description |
|---|---|
| `FossilSense: Start Server` | Start the language server manually. |
| `FossilSense: Stop Server` | Stop the language server. |
| `FossilSense: Refresh Index` | Incrementally refresh the index, skipping unchanged files. |
| `FossilSense: Full Rebuild Index` | Force a full rescan and reindex (respects `fossilsense.json` scope). |

## Completion

FossilSense provides lightweight, index-based and text-based completion for C/C++
files. It is **not** a semantic language service. Specifically:

- Completion candidates come from the indexed symbol table (functions, macros, types,
  enum constants, global variables) and identifier words found in the currently
  open file. The token currently being typed is not echoed back as a word candidate;
  raw current-file words are fallback text items and do not receive current-file scope
  priority over reachable or external indexed symbols. Indexed candidates outside the
  current file carry a best-effort scope tag in the item detail (`reachable` /
  `external` / `global` / `ambiguous`) with the full tier/confidence/reason in the
  item documentation; indexed current-file candidates are unlabeled. The tag is
  presentation only — it never changes ranking.
- **Member completion (`.`/`->`) is degraded and C-oriented.** When the cursor follows
  a member operator, FossilSense returns struct/union **fields only** (never functions
  or macros). It tries to resolve the receiver's record type from a simple declaration
  in the current file (`struct Foo *p; p->`), then lists that record's fields — pulled
  from the global index, so cross-file definitions resolve even behind a forward
  declaration. When the receiver can't be resolved (a call result `get()->`, a chain
  `a.b.`, or an unknown variable), it falls back to a prefix-filtered list of all field
  names. It never infers expression types, so it may occasionally offer the wrong
  record's fields — a candidate list, not a precise one.
- **Signature help / parameter hints are best-effort.** When the cursor is inside
  a simple function call, FossilSense finds exact-name indexed function
  declaration/definition candidates, ranks them with the same include
  reachability tiers used by Go to Definition, and shows stored signatures with
  the active argument index when the parameter list can be split conservatively.
  It does not perform overload resolution, argument type matching, template or
  namespace lookup, function-like macro expansion, or function-pointer target
  inference. When a signature is too complex to split safely, the whole stored
  signature is shown without fabricated parameter labels.
- Snippet expansion, auto-import, and C++-specific member access (scoped enums,
  static members, nested types) are not special-cased.
- Local-variable scope is not tracked for ordinary completion; words from the current
  file are a flat bag of identifiers, filtered only for C/C++ keywords.

The `fossilsense.mode` setting controls the whole FossilSense server:

| Value | Behavior |
|---|---|
| `"auto"` (default) | Starts FossilSense and shows a one-time mutual-exclusion warning when clangd, Microsoft C/C++, or ccls is detected. FossilSense does not silently disable individual providers. |
| `"on"` | Starts FossilSense without the mutual-exclusion warning. |
| `"off"` | Does not start FossilSense. |

The `fossilsense.completion.mode` setting controls only FossilSense completion:

| Value | Behavior |
|---|---|
| `"auto"` (default) | Enables completion. C/C++ language-server conflicts are reported by `fossilsense.mode`, not handled by silently disabling completion. |
| `"on"` | Enables completion. |
| `"off"` | Never provides completion; other FossilSense features remain active. |

When another C/C++ language server is detected, FossilSense shows a single warning
with actions to stop FossilSense or open settings. Choose one primary C/C++ provider
for a workspace to avoid duplicate completion/navigation results.

## Semantic Coloring

FossilSense adds a deliberately narrow, text/index-level semantic colorer for C/C++.
It exists to correct the identifier classes TextMate most reliably gets wrong —
**macros**, **types**, and **enum constants** — and nothing else:

- Only known macros, known type names (typedef / struct / enum / union / class), and
  known enum constants are colored. Functions, variables, parameters, **struct/union
  fields**, locals, and any unrecognized identifier are left untouched for the editor's
  TextMate grammar. (Enum constants fit this model because, in C, they are unscoped
  global identifiers — a name match is high-confidence, just like a macro. Fields do
  not: a bare name matching a field would too often be a local, so they are not colored.)
- Classification combines the current file's tree-sitter definitions (which win) with
  the global symbol index. When a name has several meanings in the index, the most
  frequently defined colorable kind is chosen as a **best guess**; an exact tie is
  left uncolored. This means a wrapper macro that shadows a real function, or a name
  that is both a macro and an enum constant, may occasionally be mis-colored — an
  accepted trade-off, not a bug.
- Coloring uses the standard `macro`, `type`, and `enumMember` LSP token types with no
  modifiers, so it follows your theme with zero extra configuration. There is **no**
  inactive-region (`#if 0`) graying, scope-aware inference, or per-position type analysis.
- Tokens are computed only for open/visible files and recomputed as you edit.

The `fossilsense.semanticColoring.mode` setting controls when coloring is active:

| Value | Behavior |
|---|---|
| `"auto"` (default) | Enables coloring. C/C++ language-server conflicts are reported by `fossilsense.mode`, not handled by silently disabling coloring. |
| `"on"` | Enables coloring. |
| `"off"` | Never provides semantic coloring; other FossilSense features remain active. |

Semantic coloring follows the whole-server mutual-exclusion notice from
`fossilsense.mode`; it is no longer auto-disabled independently.

## Find All References

Find-all-references is a best-effort **text candidate** search with syntactic-role
grouping — not resolved semantic references. Discovery is a case-sensitive whole-word
search; each hit is then classified with a best-effort syntactic role
(definition / declaration / call / write / type / read; unparseable hits fall back to
read) and results are ordered by role (definitions and declarations first). The
standard References panel renders **locations only** and carries no per-item role
label; run **FossilSense: Find References (Grouped by Role)** to see the hits grouped
and labeled by role. Results are capped at 2000 matches.

For Go to Definition, enabling `fossilsense.debug.candidateReasons` logs each
candidate's scope tier, confidence, and reason to the output panel — a debug aid that
never changes which definitions are returned.

## Coexistence With C/C++ Tooling

FossilSense is a best-effort navigation engine, not a replacement for a semantic
compiler model. It can be installed alongside Microsoft C/C++, clangd, and CMake
Tools, but only one C/C++ language provider should be primary in a workspace:

- Keep clangd/IntelliSense enabled when they work; they remain the better source
  for diagnostics, completion, hover, semantic references, and exact overload
  resolution.
- Use FossilSense when `compile_commands.json` is missing, stale, or too costly
  for a large Windows workspace. Its providers are intentionally limited to
  workspace symbols, document symbols, ranked definition candidates, and
  role-grouped text-candidate references (not resolved semantic references).
- Duplicate or different results are expected. FossilSense does not claim
  semantic precision; it returns stable text/index candidates that are useful
  when full IntelliSense cannot build the project.
- If another extension's provider is preferred for a specific workspace, leave
  FossilSense installed but stop its server with `FossilSense: Stop Server` or set
  `fossilsense.mode` to `"off"` for that workspace.

## Performance Notes

- Source/config file events are scope-filtered, debounced, and coalesced before
  reindexing. Saves rely on the file watcher, avoiding a duplicate save+watcher
  index request for the same edit.
- Manual `FossilSense: Refresh Index` and ordinary file events incrementally
  refresh the index. `FossilSense: Full Rebuild Index` and `fossilsense index
  --force` force a full parse of in-scope files.
- Incremental checks use cheap metadata first and only read/hash file contents
  for files that need parsing; stored file metadata is fetched in batched
  lookups instead of one query per file. Parser concurrency is bounded by
  default (8 workers, clamped to available CPU parallelism); set
  `FOSSILSENSE_PARSE_THREADS` to override the worker count for diagnostics.
- Parser worker threads run with enlarged stacks so deeply nested C/C++ files
  parse without overflowing, and the per-file index walk collects fields, enum
  constants, and typedef aliases in a single iterative pass over the syntax tree.
- Full rebuilds use a bulk-load path: SQLite lookup indexes are dropped up front,
  symbols are imported in batched transactions, then the indexes are rebuilt once
  at the end instead of being maintained row by row.
- `include` entries prune traversal when possible. For example, an include of
  `["src/core/"]` keeps ancestors such as `src/` but does not descend unrelated
  top-level directories.
- CLI `index` prints `elapsed_ms`, `discover_ms`, `parse_ms`, and `write_ms`;
  the VS Code output panel logs the same metrics when indexing completes.

## Build & Check

```powershell
cargo build
cargo test
cargo run -p fossilsense -- scan samples/mini-c
cargo run -p fossilsense -- index samples/mini-c --db target/release-check-mini.sqlite --force
cargo run -p fossilsense -- index samples/mini-c --db target/release-check-mini.sqlite
cd extensions/vscode
pnpm install
pnpm compile
pnpm run package
```

To run the extension, open this repository in VS Code, press `F5`, open `samples/mini-c` in the Extension Development Host, and run `FossilSense: Start Server`.
The status bar reports indexing phases (`discovering`, `checking`, `parsing`,
`indexing`, `finalizing`), then `ready` or `failed` from the language server's
`fossilsense/indexStatus` notification, and shows `[!]` with a warning tooltip
when `fossilsense.json` falls back to defaults.

For local release benchmarking, run the same `index --force` and incremental
`index` commands against `example/HimuOS` when that private sample is available.
Record file count, symbol count, and the four timing fields in the release report.

## Packaging a VSIX

Releases ship an installable, self-contained `.vsix` for hands-on testing
(see `CLAUDE.md` for the delivery contract). To build one:

```powershell
cd extensions/vscode
pnpm install
pnpm run package
```

This builds the release engine, bundles it into the extension, and emits
`dist/fossilsense-vscode-<version>.vsix` at the repo root. Install it via VS Code →
Extensions → `...` → *Install from VSIX*, or `code --install-extension dist/<name>.vsix`.
