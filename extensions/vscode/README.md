# FossilSense for VS Code

FossilSense gives large, difficult-to-build C, C++, and Go workspaces useful navigation without requiring a complete compiler setup. The `1.5.1` VSIX is self-contained: open a workspace and let the bundled native engine build its local index. Go support is experimental and does not require the Go toolchain or gopls.

Version 1.5.1 begins interactive-completion latency hardening with latest-request-wins admission: a newer document revision supersedes queued ordinary-completion work, and an in-progress compact-name recall cooperatively stops instead of publishing a stale partial list or memo pool.

It is designed for firmware, embedded systems, drivers, kernels, legacy code, vendored SDKs, and repositories where `compile_commands.json` is missing or unreliable.

## What you get

- Workspace and document symbols.
- Standard Go to Declaration plus ranked Go to Definition candidates. Their meanings stay stable: invoking Definition on a definition keeps that definition instead of toggling to its declaration. `goto` labels resolve only inside the enclosing function's label namespace.
- An explicit **Find All Possible Definitions / Declarations** QuickPick for the bounded variants suppressed by default navigation, including role, scope, linkage, guard, pairing, and coverage evidence.
- Identifier, C/C++ include-path, indexed Go import-path, local-variable, and limited member completion.
- Best-effort references grouped as definition, declaration, call, read, write, or type use.
- Function Hover and Signature Help with arity-aware candidates and rendered comments.
- Full bounded `struct`, `class`, and `union` Hover; unique `typedef` chains can show `aka`.
- One-hop incoming and outgoing call relations for direct C/C++ and Go calls, including call sites and evidence.
- Limited semantic coloring for macros, types, enum constants, parameters, and local variables.
- Unsaved open-document declarations included in candidate results.

FossilSense ranks evidence from the current file, reachable includes, direct external headers, and global fallback, and preserves how include edges were resolved: exact edges provide strong reachability; unique suffix matches and every possible target of an ambiguous include remain heuristic; and direct-external evidence is evaluated from the current query origin. Limited semantic coloring lets those bounded heuristic include targets contribute macro, type, and enum-kind evidence, while unrelated whole-workspace definitions remain excluded when the include scope is open; conflicting kind evidence stays uncolored. If an exact-name global window reaches its cap, Current and strongly reachable paths are recalled first. Indexed object candidates also distinguish declarations, C tentative definitions, full definitions, and unknown declaration/definition roles. When parsing or include information is incomplete, results degrade conservatively and expose ambiguity, confidence, or coverage instead of claiming compiler-level precision.

Go uses package/import reachability instead of pretending that imports are C includes. Same-package files, `go.mod`, `go.work`, workspace `vendor`, and explicitly configured external module roots provide bounded evidence. Unresolved imports, build constraints, target filename suffixes, and cgo boundaries keep coverage open or lower confidence instead of silently dropping other candidates. C/C++ and Go use separate semantic families, so same-name declarations do not leak across ordinary queries.

Version 1.5.0 routes Go document/workspace symbols, Declaration/Definition, Find All, ordinary/local/member/import completion, Hover, References, Signature Help, Semantic Tokens, and Call Hierarchy through the same candidate service and typed read model used by C/C++. The engine persists Go package, import, build guard, declaration, method, field, local-binding, and direct-call facts while retaining best-effort candidate semantics.

## Where symbols come from

The tolerant tree-sitter frontends extract typed declaration facts from C/C++ and Go source, with a conservative lexical fallback for a hard AST failure. The local SQLite index stores each declaration's stable ID, name, declaration/definition role, source range, signature, linkage or package identity, conditional guard, and file revision. Hover, navigation, Signature Help, Find All, and workspace symbols all use the same candidate service over those facts, include/package reachability, project evidence, and unsaved-document overlays.

Ordinary completion has a deliberately separate first stage because it runs on every keystroke. A compact in-memory index recalls only names, kinds, paths, scope signals, and canonical declaration IDs without loading every full declaration. Completion resolve then sends the selected ID and name through the same candidate service used by Hover and navigation. This is a split between fast recall and semantic hydration, not two semantic models, so resolved completion details keep the same signature, role, location, and live-overlay behavior as the other features.

C++ record methods intentionally participate in ordinary identifier recall as function-kind names. This broad recall makes method spellings discoverable without a receiver context; it does not claim receiver binding. `.` / `->` completion still filters through separate record-type evidence.

## Install and start

Install `fossilsense-vscode-1.5.1_BUILD*.vsix` with:

```text
Extensions -> ... -> Install from VSIX
```

Open a C, C++, or Go workspace and wait for the FossilSense status item to reach `ready`. The default scope covers common C/C++ extensions and `.go`, and excludes typical generated directories such as `.git`, `node_modules`, `target`, `out`, and `build`.

If an active clangd, Microsoft C/C++, ccls, or Go extension matches a source language currently open in the workspace, FossilSense shows a one-time coexistence warning because that extension can start an overlapping language server. For predictable results, use one primary provider for each language in the workspace.

## Commands

| Command | Purpose |
|---|---|
| `FossilSense: Start Server` | Start the workspace language server |
| `FossilSense: Stop Server` | Stop it for the current workspace |
| `FossilSense: Refresh Index` | Incrementally process changed files |
| `FossilSense: Full Rebuild Index` | Rebuild the full in-scope index |
| `FossilSense: Find All Possible Definitions / Declarations` | Inspect bounded variants and their uncertainty evidence |
| `FossilSense: Find References (Grouped by Role)` | Inspect best-effort reference roles |
| `FossilSense: Analyse Call Hierarchy` | Open incoming/outgoing direct-call relations and call sites in the FossilSense Relations panel |
| `FossilSense: Select Project Context` | Select automatic, manual, unspecified, or disabled project evidence |
| `FossilSense: Clear Completion History` | Remove local completion-ranking history |

## Workspace scope

An optional `fossilsense.json` at the workspace root controls source scope, external headers, and explicit external Go modules:

```json
{
  "include": ["src/", "include/"],
  "exclude": ["src/generated/"],
  "extensions": ["c", "h", "cpp", "hpp", "go"],
  "includePaths": ["C:/toolchain/include"],
  "goModulePaths": ["D:/shared/device-module"],
  "languageOverrides": [
    { "glob": "legacy-c/**/*.h", "language": "c" },
    { "glob": "generated/cpp/**/*.h", "language": "cpp" },
    { "glob": "generated/go/**/*.inc", "language": "go" }
  ]
}
```

All fields are optional. `.c` defaults to C; `.h`, `.inl`, and the standard C++ source/header extensions default to C++; `.go` defaults to Go. `languageOverrides` accepts `c`, `cpp`, or `go`, matches case-insensitively over normalized `/` paths, and applies the last matching rule. `goModulePaths` contains explicit absolute module roots and never triggers automatic GOPATH or machine module-cache discovery. Every root is independently file/byte capped; a root should contain `go.mod` for complete module import-path evidence. Invalid entries and rules are skipped with a visible warning without discarding other configuration fields.

## Main settings

- `fossilsense.mode`: `auto` starts normally and warns about another known C/C++ or Go provider; `on` starts without that warning; `off` disables FossilSense.
- `fossilsense.serverPath`: use a custom engine binary instead of the bundled one.
- `fossilsense.includePaths`: add absolute external header directories.
- `fossilsense.goModulePaths`: add explicit external Go module roots; these merge with `fossilsense.json` and use the same bounded scanning rules.
- `fossilsense.completion.mode`: enable or disable identifier, include/import, and member completion.
- `fossilsense.completion.prefixRanking`: `strict` prefers exact names and literal prefixes; `scopeFirst` gives scope evidence priority.
- `fossilsense.completionHistory.mode`: enable or disable local accepted-completion history.
- `fossilsense.projectContext.mode`: use automatic project evidence, prompt when ambiguous, or disable it.
- `fossilsense.semanticColoring.mode`: enable or disable FossilSense semantic coloring.
- `fossilsense.includeScoping.mode`: narrow coloring and completion using the current file's resolved `#include` graph. `auto` (default) accepts exact reachable definitions, direct external headers, and bounded heuristic include targets while excluding unrelated whole-workspace definitions when the scope is open; `off` reverts to whole-index behavior.
- `fossilsense.references.showRanges`: show line suffixes in grouped reference rows.
- `fossilsense.resourceMonitor.enabled`: show a status bar item with the server's process memory and the on-disk size of its index cache. On by default; updates every 5 seconds while the server is running. Turning it off only hides the item.
- `fossilsense.semanticIndex.memoryBudgetMB`: total target for the declaration semantic index. The always-resident compact completion recall index is charged first; the remainder caches canonical declaration payloads shared by completion resolve, Hover, and navigation. `0` retains recall and loads selected facts from the local database on demand.
- `fossilsense.debug.candidateReasons`: log definition-candidate scope, confidence, and reason.

## Current limitations

FossilSense is a best-effort navigation engine, not a compiler model. It does not support full C++ inheritance, template instantiation, overload resolution, macro expansion, access control, namespace binding, or complex expression type inference.

The Go backend does not perform interface dynamic dispatch, generic instantiation, embedded-member promotion, method-set proof, or expression type inference. Selectors, same-name methods, function values, and indirect calls remain multiple candidates or fallback when evidence is incomplete. FossilSense does not invoke the Go toolchain. Build expressions and GOOS/GOARCH filename suffixes are visible guard/ranking/coverage evidence, but there is no active target selection. `import "C"` exposes an unsupported-language boundary and does not infer Go/C bindings.

Declarations, Hover, navigation, coloring, document symbols, and call relations accept AST facts only. If tree-sitter still produces a usable tree with syntax errors, FossilSense keeps the Partial AST path instead of switching the whole document to lexical declaration scanning. A name that exists only inside an unsupported `ERROR` region may temporarily be absent from declarations, navigation, coloring, and completion until the edit forms a recognizable declaration. Lexical fallback is reserved for a hard AST failure and contributes only isolated, lowest-priority, non-navigable completion hints.

References start from whole-word text matches and can include same-name text in comments or strings. Function declaration/definition pairing requires compatible normalized signatures, linkage, and include evidence. C signature matching ignores parameter names and an unrelated standalone `extern`, while retaining parameter-type shape. Unsupported or ambiguous cases remain multiple ordinary candidates or fallbacks; they do not become a guessed unique result.

Find All is bounded discovery, not a compiler or linker result. Its QuickPick states the active limit and whether coverage is open, truncated, or incomplete. An unlimited cross-root set, complete macro state, C++ ABI/`extern "C"` binding, and active C/C++ or Go build-target selection remain unsupported.

Call relations formally cover direct, explicitly qualified, or parenthesized callable names. Interface dispatch, function values/pointers, callable objects, ambiguous receiver binding, and macro-generated calls use fallback behavior or remain unsupported.

## Privacy

Source indexing and completion history stay on the local machine. FossilSense does not upload source code, send telemetry, use cloud sync, or call a cloud ML ranker. Local completion history is bounded and can be disabled or cleared at any time.
