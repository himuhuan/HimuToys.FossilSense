> **Status: archived** (2026-07-10)
>
> 历史交付说明。当前版本交付说明见 dist/DELIVERY-NOTE-<version>.md。
# FossilSense v1.2.3 Delivery Note

Date: 2026-07-06
VSIX artifact: `dist/fossilsense-vscode-1.2.3_BUILD20260706_191726.vsix`

## Release Scope

v1.2.3 is a parser and member-completion quality release. It is not a
behavior-preserving-only architecture release: it intentionally improves
best-effort struct extraction and nested member completion for common C code
shapes while preserving the existing candidate/confidence/fallback model.

## What Changed

- Version updated to `1.2.3` for the Rust engine and VS Code extension.
- Multiline `typedef struct { ... } Name;` extraction is more resilient when
  member comments, strings, character literals, or preprocessor lines contain
  braces.
- Record typedef alias extraction handles the declarator list after the closing
  record body instead of relying only on the last identifier in the compacted
  statement.
- Member completion now parses common C lvalue chains such as `a.mem1[n].`,
  `arr[i].`, and `(*ptr).inner.`.
- Named anonymous nested `struct` / `union` members produce synthetic
  best-effort record evidence, allowing chains like `a.mem1[n].xxx` to resolve
  inner fields.
- Function-pointer fields remain field members and are not presented as methods.
- Go to Definition now merges the current open document's live symbols with the
  persisted index, so newly typed or stale-index typedefs can still jump from a
  later use back to the typedef definition when completion/document symbols
  already see them.
- Multiline preprocessor macro bodies are isolated from top-level statement and
  brace tracking, so a block-like macro such as `FREE(ptr)` no longer swallows
  the immediately following `typedef struct xxx { ... } xxx_t;` definition in
  the persisted index or Go to Definition.

## Verification performed

Passed:

```powershell
cargo fmt
cargo test -p fossilsense
cd extensions/vscode
pnpm test
pnpm run package
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/verify_release_hardening.ps1
```

Results:

- Rust tests: 505 unit tests passed.
- LSP smoke tests: 2 tests passed.
- Extension compile/tests: `pnpm test` passed.
- Package: `pnpm run package` produced
  `dist/fossilsense-vscode-1.2.3_BUILD20260706_191726.vsix`.
- Native binary inspection: release hardening verified a non-empty
  `extension/bin/fossilsense.exe` inside the VSIX.

## Unchanged capabilities

- Best-effort Go to Definition, Workspace Symbols, Document Outline, Hover,
  Signature Help, semantic coloring, references, include analysis, ordinary
  completion, include completion, member completion, and local-only completion
  history remain in scope.
- Candidate results remain ranked best-effort evidence, not compile-accurate
  semantic bindings.
- Completion logging remains metadata-only by default and does not log candidate
  names, source snippets, accepted labels, or raw include paths.
- The VSIX remains self-contained and does not require users to install Rust,
  clangd, ctags, cscope, or compile commands.

## Known non-goals

- No complete C/C++ semantic model: inheritance, overload resolution, templates,
  namespaces, access control, expression type inference, macro expansion, and
  compile-accurate binding remain out of scope.
- No function-call-result member type inference, complex cast inference, or
  general expression evaluator.
- No ML ranking, telemetry, cloud sync, or auto include insertion.

## Install

```powershell
code --install-extension "F:\HimuToys\HimuToys.FossilSense\dist\fossilsense-vscode-1.2.3_BUILD20260706_191726.vsix"
```
