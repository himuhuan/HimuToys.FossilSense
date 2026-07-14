# FossilSense for VS Code

FossilSense gives large, difficult-to-build C and C++ workspaces useful navigation without requiring a complete compiler setup. The `1.4.3` VSIX is self-contained: open a workspace and let the bundled native engine build its local index.

It is designed for firmware, embedded systems, drivers, kernels, legacy code, vendored SDKs, and repositories where `compile_commands.json` is missing or unreliable.

## What you get

- Workspace and document symbols.
- Ranked Go to Definition candidates.
- Identifier, include-path, local-variable, and limited member completion.
- Best-effort references grouped as definition, declaration, call, read, write, or type use.
- Function Hover and Signature Help with arity-aware candidates and rendered comments.
- Full bounded `struct`, `class`, and `union` Hover; unique `typedef` chains can show `aka`.
- One-hop incoming and outgoing call relations for C/C++ free functions, including call sites and evidence.
- Limited semantic coloring for macros, types, enum constants, parameters, and local variables.
- Unsaved open-document declarations included in candidate results.

FossilSense ranks evidence from the current file, reachable includes, direct external headers, and global fallback. When parsing or include information is incomplete, results degrade conservatively and expose ambiguity, confidence, or coverage instead of claiming compiler-level precision.

## Install and start

Install `fossilsense-vscode-1.4.3_BUILD*.vsix` with:

```text
Extensions -> ... -> Install from VSIX
```

Open a C or C++ workspace and wait for the FossilSense status item to reach `ready`. The default scope covers common C/C++ extensions and excludes typical generated directories such as `.git`, `node_modules`, `target`, `out`, and `build`.

If clangd, Microsoft C/C++, or ccls is active, FossilSense shows a one-time coexistence warning. For predictable results, use one primary C/C++ language provider per workspace.

## Commands

| Command | Purpose |
|---|---|
| `FossilSense: Start Server` | Start the workspace language server |
| `FossilSense: Stop Server` | Stop it for the current workspace |
| `FossilSense: Refresh Index` | Incrementally process changed files |
| `FossilSense: Full Rebuild Index` | Rebuild the full in-scope index |
| `FossilSense: Find References (Grouped by Role)` | Inspect best-effort reference roles |
| `FossilSense: Analyse Call Hierarchy` | Open incoming/outgoing free-function relations and call sites |
| `FossilSense: Select Project Context` | Select automatic, manual, unspecified, or disabled project evidence |
| `FossilSense: Clear Completion History` | Remove local completion-ranking history |

## Workspace scope

An optional `fossilsense.json` at the workspace root controls source scope and external headers:

```json
{
  "include": ["src/", "include/"],
  "exclude": ["src/generated/"],
  "extensions": ["c", "h", "cpp", "hpp"],
  "includePaths": ["C:/toolchain/include"]
}
```

All fields are optional. Invalid configuration falls back to safe defaults and produces a visible warning.

## Main settings

- `fossilsense.mode`: `auto` starts normally and warns about another C/C++ provider; `on` starts without that warning; `off` disables FossilSense.
- `fossilsense.serverPath`: use a custom engine binary instead of the bundled one.
- `fossilsense.includePaths`: add absolute external header directories.
- `fossilsense.completion.mode`: enable or disable identifier, include, and member completion.
- `fossilsense.completion.prefixRanking`: `strict` prefers exact names and literal prefixes; `scopeFirst` gives scope evidence priority.
- `fossilsense.completionHistory.mode`: enable or disable local accepted-completion history.
- `fossilsense.projectContext.mode`: use automatic project evidence, prompt when ambiguous, or disable it.
- `fossilsense.semanticColoring.mode`: enable or disable FossilSense semantic coloring.
- `fossilsense.references.showRanges`: show line suffixes in grouped reference rows.
- `fossilsense.debug.candidateReasons`: log definition-candidate scope, confidence, and reason.

## Current limitations

FossilSense is a best-effort navigation engine, not a compiler model. It does not support full C++ inheritance, template instantiation, overload resolution, macro expansion, access control, namespace binding, or complex expression type inference.

References start from whole-word text matches and can include same-name text in comments or strings. Strict `.h/.c` counterpart pairing requires matching signatures, external linkage, closed include reachability, and a bidirectionally unique match. Unsupported or ambiguous cases remain ordinary candidates; they do not become a guessed unique result.

Call relations formally cover direct, explicitly qualified, or parenthesized free-function names. Member calls, function pointers, callable objects, and macro-generated calls use fallback behavior or remain unsupported.

## Privacy

Source indexing and completion history stay on the local machine. FossilSense does not upload source code, send telemetry, use cloud sync, or call a cloud ML ranker. Local completion history is bounded and can be disabled or cleared at any time.
