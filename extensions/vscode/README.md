# FossilSense for VS Code

FossilSense gives large, difficult-to-build C, C++, and Go workspaces useful navigation without requiring a complete compiler setup. The `1.7.0` VSIX is self-contained: open a workspace and let the bundled native engine build its local index. Go support is experimental and does not require the Go toolchain or gopls.

Version 1.7.0 unifies tolerant C declaration discovery. Multiple declarators, function pointers, typedefs, conditional branches, and local scopes retain consistent identities and original positions. Header language selection uses configuration and source evidence. Healthy declarations remain available when other regions are only partially understood, with partial or unknown coverage made visible.

The new read-only `query explain` command separates disk observations, stale indexes, bounded recall, and navigation omissions. Reviewed C fixtures and an independent Clang oracle check the supported regression contracts; this development validation adds no Clang requirement for users. Opt-in protobuf-c source tracing remains available. FossilSense does not expand arbitrary macros or claim complete C++ compiler semantics.

It is designed for firmware, embedded systems, drivers, kernels, legacy code, vendored SDKs, and repositories where `compile_commands.json` is missing or unreliable.

## What you get

- Workspace and document symbols.
- Standard Go to Declaration plus ranked Go to Definition candidates. Their meanings stay stable: invoking Definition on a definition keeps that definition instead of toggling to its declaration. `goto` labels resolve only inside the enclosing function's label namespace.
- An explicit **Find All Possible Definitions / Declarations** QuickPick for the bounded variants suppressed by default navigation, including role, scope, linkage, guard, pairing, and coverage evidence.
- Identifier, C/C++ include-path, indexed Go import-path, local-variable, and limited member completion.
- Best-effort references grouped as definition, declaration, call, read, write, or type use.
- Function Hover and Signature Help with arity-aware candidates and rendered comments.
- Full bounded `struct`, `class`, and `union` Hover; unique `typedef` chains can show `aka`.
- Optional protobuf-c generated-type Hover sources, including the proto file, declaration line, match evidence, and visible ambiguity or truncation.
- One-hop incoming and outgoing call relations for direct C/C++ and Go calls, including call sites and evidence.
- Limited semantic coloring for macros, types, enum constants, parameters, and local variables.
- Unsaved open-document declarations included in candidate results.

FossilSense ranks evidence from the current file, reachable includes, direct external headers, and global fallback, and preserves how include edges were resolved: exact edges provide strong reachability; unique suffix matches and every possible target of an ambiguous include remain heuristic; and direct-external evidence is evaluated from the current query origin. Limited semantic coloring lets those bounded heuristic include targets contribute macro, type, and enum-kind evidence, while unrelated whole-workspace definitions remain excluded when the include scope is open; conflicting kind evidence stays uncolored. If an exact-name global window reaches its cap, Current and strongly reachable paths are recalled first. Indexed object candidates also distinguish declarations, C tentative definitions, full definitions, and unknown declaration/definition roles. When parsing or include information is incomplete, results degrade conservatively and expose ambiguity, confidence, or coverage instead of claiming compiler-level precision.

Go uses package/import reachability instead of pretending that imports are C includes. Same-package files, `go.mod`, `go.work`, workspace `vendor`, and explicitly configured external module roots provide bounded evidence. Unresolved imports, build constraints, target filename suffixes, and cgo boundaries keep coverage open or lower confidence instead of silently dropping other candidates. C/C++ and Go use separate semantic families, so same-name declarations do not leak across ordinary queries.

The experimental Go backend routes Go document/workspace symbols, Declaration/Definition, Find All, ordinary/local/member/import completion, Hover, References, Signature Help, Semantic Tokens, and Call Hierarchy through the same candidate service and typed read model used by C/C++. The engine persists Go package, import, build guard, declaration, method, field, local-binding, and direct-call facts while retaining best-effort candidate semantics.

## Where symbols come from

Navigation and hover share the current document's parse and distinguish values, types, labels, and members at the cursor. An unresolved lookup within a known domain returns no result; names inside comments and strings do not trigger ordinary symbol navigation. Default member navigation requires evidence of the owning type. **Find All Possible Definitions / Declarations** remains available for broader exploration, and member completion retains its candidate suggestions.

Field and direct-method hover, Definition, and Declaration share owner evidence and target the member token. Hover comments come from the same source revision. Supported cases include explicitly typed local objects and parameters, pointers, bounded member chains, member declarations, and simple designated initializers such as `struct S s = {.state = 1}`. Go member navigation is limited to proven types in the same package. Array subscripts, complex initializers, and unproven owners remain unsupported for navigation; member completion keeps exploratory suggestions. Method targets are evidenced locations, not a guarantee that an implementation has been found. Existing indexes rebuild after upgrading so member type namespaces can be preserved.

The tolerant tree-sitter frontends extract typed declaration facts from C/C++ and Go source, with a conservative lexical fallback for a hard AST failure. For simple C global declarations that it can identify safely, that fallback preserves multiple declared names as lowest-priority completion hints, but still gives those hints no navigable declaration identity. In files resolved as C, every supported direct declarator in one statement is decoded separately. This covers multiple functions and objects, mixed function/function-pointer declarations, functions returning function pointers, function and function-pointer typedefs, field function pointers, and array/pointer binding order without promoting parameter, extent, or initializer names. A statement processes at most 256 direct declarators and a declarator chain at most 128 derived layers; over-budget, malformed, and unsupported shapes are not guessed, while already decoded siblings remain available. File-scope `struct`, `union`, and `enum` forward tags are stored as declarations, so Declaration and Definition can target the forward tag and full definition separately; ordinary type uses and function-local forward tags are not promoted by that rule. C enum values, records, and typedefs defined inside a function remain available to live local queries but are not stored in the workspace index. Functions, objects, macros, records, typedefs, enum values, and members retain the same conditional-compilation evidence. `#elif`, `#else`, and nested branches remain separate candidates: FossilSense does not evaluate those conditions or use them to claim a unique result. A guard is bounded to 128 nested conditions and 8 KiB; over-budget evidence is reported as unknown. A standalone GNU C `__attribute__((weak))` no longer splits one function's declaration and definition identities; presentation signatures retain the source attribute, and ABI attributes remain identity-bearing. Protobuf-c generated headers parsed as C tolerate a single-token export macro between `struct` and the real type name, so fields and type definitions remain attached to the generated type; standalone `PROTOBUF_C__BEGIN_DECLS` and `PROTOBUF_C__END_DECLS` lines no longer interfere with the first following `typedef`. The local SQLite index stores each declaration's stable ID, name, declaration/definition role, source range, signature, linkage or package identity, conditional guard, and file revision. Hover, navigation, Signature Help, Find All, and workspace symbols all use the same candidate service over those facts, include/package reachability, project evidence, and unsaved-document overlays.

Ordinary completion has a deliberately separate first stage because it runs on every keystroke. A compact in-memory index recalls only names, kinds, paths, scope signals, and canonical declaration IDs without loading every full declaration. Completion resolve then sends the selected ID and name through the same candidate service used by Hover and navigation. This is a split between fast recall and semantic hydration, not two semantic models, so resolved completion details keep the same signature, role, location, and live-overlay behavior as the other features.

Local queries do not imply complete local-record navigation. Enum values and `typedef` names participate in existing local queries; record tags retain function/block and C tag-namespace evidence without overriding same-named ordinary variables, but full navigation for those tags is not provided in this release.

C++ record methods intentionally participate in ordinary identifier recall as function-kind names. This broad recall makes method spellings discoverable without a receiver context; it does not claim receiver binding. `.` / `->` completion still filters through separate record-type evidence.

## Install and start

Install `fossilsense-vscode-1.7.0_BUILD*.vsix` with:

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

An optional `fossilsense.json` at the workspace root controls source scope, external headers, explicit external Go modules, and opt-in protobuf-c source tracing:

```json
{
  "include": ["src/", "include/"],
  "exclude": ["src/generated/"],
  "extensions": ["c", "h", "cpp", "hpp", "go"],
  "includePaths": ["C:/toolchain/include"],
  "goModulePaths": ["D:/shared/device-module"],
  "protobufC": {
    "enabled": true,
    "protoPaths": ["proto", "D:/shared/protocols"]
  },
  "languageOverrides": [
    { "glob": "legacy-c/**/*.h", "language": "c" },
    { "glob": "generated/cpp/**/*.h", "language": "cpp" },
    { "glob": "generated/go/**/*.inc", "language": "go" }
  ]
}
```

All fields are optional. `.c` defaults to C; `.h`, `.inl`, and the standard C++ source/header extensions default to C++; `.go` defaults to Go. `languageOverrides` accepts `c`, `cpp`, or `go`, matches case-insensitively over normalized `/` paths, and applies the last matching rule. Without an override, a `.pb-c.h` file containing a complete `PROTOBUF_C__BEGIN_DECLS` code token in its first 64 KiB selects C and uses the existing protobuf-c declaration recovery. Comments, strings, and preprocessor directives do not count as evidence. This parsing rule does not require `protobufC.enabled`.

Saved indexes and unsaved documents share the same language selection and retain its source and rule version. Explicit overrides take priority; extension defaults and generated-file recognition remain inferences. Ordinary `.h` and `.inl` files retain the C++ default with an ambiguity flag, so known C directories should use an override. Changing overrides reparses unchanged source files as well. An explicit grammar choice does not imply compiler-level declaration accuracy.

`goModulePaths` contains explicit absolute module roots and never triggers automatic GOPATH or machine module-cache discovery. `protobufC.enabled` defaults to `false`; project proto paths may be workspace-relative or absolute. Only generated `*.pb-c.h` files present in the parsed include graph are traced. Every external root is independently file/byte capped; each proto source extraction is also capped at 16 MiB and a fixed token budget. Invalid or over-budget inputs are skipped with a visible warning without discarding other configuration fields.

## Main settings

- `fossilsense.mode`: `auto` starts normally and warns about another known C/C++ or Go provider; `on` starts without that warning; `off` disables FossilSense.
- `fossilsense.serverPath`: use a custom engine binary instead of the bundled one.
- `fossilsense.includePaths`: add absolute external header directories.
- `fossilsense.goModulePaths`: add explicit external Go module roots; these merge with `fossilsense.json` and use the same bounded scanning rules.
- `fossilsense.protobufC.enabled`: explicitly enable or disable protobuf-c source tracing. An explicitly configured editor value overrides the project value; otherwise the setting inherits `fossilsense.json`.
- `fossilsense.protobufC.protoPaths`: add absolute proto source directories. They merge with project proto paths, then normalize and de-duplicate.
- `fossilsense.completion.mode`: enable or disable identifier, include/import, and member completion.
- `fossilsense.completion.prefixRanking`: `strict` prefers exact names and literal prefixes; `scopeFirst` gives scope evidence priority.
- `fossilsense.completionHistory.mode`: enable or disable local accepted-completion history.
- `fossilsense.projectContext.mode`: use automatic project evidence, prompt when ambiguous, or disable it.
- `fossilsense.semanticColoring.mode`: enable or disable FossilSense semantic coloring.
- `fossilsense.includeScoping.mode`: narrow coloring and completion using the current file's resolved `#include` graph. `auto` (default) accepts exact reachable definitions, direct external headers, and bounded heuristic include targets while excluding unrelated whole-workspace definitions when the scope is open; `off` reverts to whole-index behavior.
- `fossilsense.references.showRanges`: show line suffixes in grouped reference rows.
- `fossilsense.resourceMonitor.enabled`: show a status bar item with the server's process memory and the on-disk size of its index cache. On by default; updates every 2 seconds while the server is running. Hover the item for per-category memory details. When available, the name-index details group name strings, paths/projects, recall postings, and fixed overhead; these are structural estimates, while Windows Private Bytes or Linux/macOS RSS remains the process-memory gate. An older server without these fields keeps the existing summary. Turning the setting off only hides the item.
- `fossilsense.semanticIndex.memoryBudgetMB`: total target for the declaration semantic index. The always-resident compact completion recall index is charged first; the remainder caches canonical declaration payloads shared by completion resolve, Hover, and navigation. `0` retains recall and loads selected facts from the local database on demand.

Full indexing, dirty indexing, read-model construction, name compaction, and project-context refresh share one process-wide heavy-build slot. When admission is temporarily unavailable, the status item shows `waiting for resources`; each workspace retains only its newest deferred target and retries after resources are released. Saved-file changes remain merged for a later dirty pass and can cooperatively cancel stale background name compaction. Removing a workspace or stopping the server cancels waiting and interruptible work. The previous immutable index continues serving Hover, navigation, and completion until a complete replacement passes its final checks and is swapped in one step.

Index parsing uses byte-reserved parallel waves and releases each wave after writing. When available memory decreases, it reduces batch size and parser concurrency, joining the old workers before creating a smaller pool. Only files exceeding their estimated capacity retry exclusively; successful neighboring files are not parsed again. A resource failure or an input revision change aborts the update while the published index remains usable. Background compaction retry delays do not prevent new edits from joining the build queue. Reservations are internal estimates, not a hard cap on total process memory.

Heavy builds share a 256 MiB temporary reservation, use a 512 MiB process-pressure target, and retain 32 MiB for unattributed runtime/query overhead. Reservation is an admission estimate, not an allocator-enforced hard cap. `semanticIndex.memoryBudgetMB` keeps its narrower meaning and does not include runtime overhead, file graphs, open documents, or the process peak while old and new generations coexist. Release gates use Private Bytes on Windows and RSS on Linux/macOS; an unavailable sample is reported explicitly rather than treated as zero.
- `fossilsense.debug.candidateReasons`: log definition-candidate scope, confidence, and reason.

Some large workspaces may be unable to complete a full rebuild while the previous index remains in service because memory admission is insufficient. The previous index remains queryable after failure, but workspace results can remain at the earlier revision; successful standalone indexing does not establish successful hot rebuilding.

Small name-update histories are consolidated separately while sharing the existing base index; substantial replacements or deletions still require full compaction.

## Current limitations

FossilSense is a best-effort navigation engine, not a compiler model. It does not support full C++ inheritance, template instantiation, overload resolution, macro expansion, access control, namespace binding, or complex expression type inference.

The Go backend does not perform interface dynamic dispatch, generic instantiation, embedded-member promotion, method-set proof, or expression type inference. Selectors, same-name methods, function values, and indirect calls remain multiple candidates or fallback when evidence is incomplete. FossilSense does not invoke the Go toolchain. Build expressions and GOOS/GOARCH filename suffixes are visible guard/ranking/coverage evidence, but there is no active target selection. `import "C"` exposes an unsupported-language boundary and does not infer Go/C bindings.

Protobuf-c tracing recognizes only proto `package`, top-level or nested `message`, and `enum` declarations, and preserves multiple plausible sources. It does not analyze fields, enum values, `service`, `oneof`, `map`, options, imports, or generated functions. It does not watch proto roots or use unsaved proto text; rebuild the index after proto changes. Go to Definition continues to target the generated C/C++ declaration.

The fixed C regression corpus checks complete declaration sets, roles, scope, and original positions against reviewed expectations and a pinned Clang oracle under explicit configurations. This development check covers those samples and configurations; it does not establish accuracy for every macro or branch in a real workspace. Installing and using the extension still requires no Clang. Protobuf-c boundary markers, record export macros, and alignment attributes can be recovered together while preserving neighboring healthy declarations and their positions.

For an explicit disk/index diagnostic, run the bundled or locally built engine with `fossilsense query explain <workspace> <file> <name> [--line N --col N] [--db PATH] [--json]`. The source path must be workspace-relative; parent components and links escaping the real workspace are rejected before reading source content. Optional line and UTF-16 column are paired and 1-based. Without a position, up to 64 whole-word locations are reported with ambiguity. The command separates inclusion, language, region coverage, extraction, persistence, recall, and presentation, using found, absent, partial, unknown, not_run, and truncated states. Disk/index content or language-configuration mismatches are explicit rather than treated as lost storage facts.

The diagnostic reads saved disk text and one read-only SQLite snapshot. It does not see unsaved editor text, change configuration, migrate a database, or rebuild an index. Its durable stages share `query def`'s canonical declaration and navigation policy; member and request-local facts are identified as separate paths rather than missing declarations. Limits are 256 facts per stage, 256 region details, 2 MiB total JSON, and a 16 MiB source observation. Reaching a limit is visible; candidates outside the unchanged 256-item production exact-name window are never called presentation omissions. Exit codes are 0 for a completed diagnostic (including absent, missing-index, or truncated observations), 2 for invalid arguments or path escape, and 1 for actual read/runtime failures.

Available declarations and complete declaration coverage are separate states. Healthy declarations remain queryable beside syntax errors or unknown declaration macros; macro arguments are never treated as proof of generated names. Hover and Find All retain complete, partial, or unknown declaration coverage and reasons for failed recovery, unknown macros, language ambiguity, unsupported declarators, and exhausted budgets. Unrequested facts and missing coverage metadata never imply completeness.

Coverage checks are capped per file at 4,096 candidate regions, 262,144 syntax nodes, and 1,024 detailed gaps. Gaps retain original byte and UTF-16 ranges; unsaved-document evidence follows the captured document version. Ordinary completion does not read gap details from SQLite. The Find All LSP command optionally accepts `includeCoverageDetails: true` to return up to 64 current-file gaps with version, content fingerprint, and truncation status. Local C type definitions and typedef-based parenthesized spellings that the grammar represents as expressions may still lack persistent declarations; these remain explicit coverage gaps. Complete coverage does not prove a unique compiler-level binding.

Declarations, Hover, navigation, coloring, document symbols, and call relations accept AST facts only. If tree-sitter still produces a usable tree with syntax errors, FossilSense keeps the Partial AST path instead of switching the whole document to lexical declaration scanning. A name that exists only inside an unsupported `ERROR` region may temporarily be absent from declarations, navigation, coloring, and completion until the edit forms a recognizable declaration. Lexical fallback is reserved for a hard AST failure and contributes only isolated, lowest-priority, non-navigable completion hints.

References start from whole-word text matches and can include same-name text in comments or strings. Function declaration/definition pairing requires compatible normalized signatures, linkage, and include evidence. C signature matching ignores parameter names and an unrelated standalone `extern`, while retaining parameter-type shape. Unsupported or ambiguous cases remain multiple ordinary candidates or fallbacks; they do not become a guessed unique result.

Find All is bounded discovery, not a compiler or linker result. Its QuickPick states the active limit and whether coverage is open, truncated, or incomplete. An unlimited cross-root set, complete macro state, C++ ABI/`extern "C"` binding, and active C/C++ or Go build-target selection remain unsupported.

Call relations formally cover direct, explicitly qualified, or parenthesized callable names. Interface dispatch, function values/pointers, callable objects, ambiguous receiver binding, and macro-generated calls use fallback behavior or remain unsupported.

## Privacy

Source indexing and completion history stay on the local machine. FossilSense does not upload source code, send telemetry, use cloud sync, or call a cloud ML ranker. Local completion history is bounded and can be disabled or cleared at any time.
