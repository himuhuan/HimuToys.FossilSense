> **Status: archived** (2026-07-10)
>
> 历史交付说明。当前版本交付说明见 dist/DELIVERY-NOTE-<version>.md。
# FossilSense v1.2.0 Delivery Note

Date: 2026-07-05
VSIX: `dist/fossilsense-vscode-1.2.0_BUILD20260705_020402.vsix`

## What Changed

v1.2.0 delivers the Smart Completion Foundation Phase 0-3 scope:

- Version updated to `1.2.0` for the Rust engine and VS Code extension.
- New `fossilsense.debug.completionRanking` setting for bounded completion evidence logs.
- Ordinary identifier completion can use current open-document top-level symbols from unsaved edits.
- Same-name completion candidates merge evidence from local bindings, open-document symbols, indexed symbols, and text fallback.
- Conservative same-tier intent ranking for call/type/value/macro/declaration-name contexts.
- Prefix-extension rank stability hints reduce small-delta list churn.
- Preselect is guarded to avoid plain text fallback, ambiguous/global-only, or weak candidates.

## Can Do

- Suggest current-function parameters and prior local variables from the open document.
- Suggest current open-document top-level functions, macros, types, enum constants, and global variables before they are indexed.
- Preserve strict scope-tier ordering: Current > Reachable > External > Unknown > Global.
- Explain completion ranking decisions through opt-in debug logs.
- Continue to work without clangd, compile commands, ctags, compiler arguments, or external services.

## Cannot Do

- It is not compiler-grade C/C++ name binding.
- It does not implement member completion v2, method indexing, inheritance, overload resolution, templates, namespaces, or access control.
- It does not record local accept history, telemetry, personalization, ML ranking, or LLM ranking.
- It does not perform broad disk IO or workspace scans per completion keystroke.

## Validation

Passed:

```powershell
cargo fmt
cargo check -p fossilsense
cargo test -p fossilsense
cargo run -p fossilsense -- index samples/mini-c --db target/v1-2-smart-completion-mini.sqlite --force
cargo run -p fossilsense -- index samples/mini-c --db target/v1-2-smart-completion-mini.sqlite
cd extensions/vscode
pnpm run compile
pnpm run test
pnpm run package
```

Results:

- Rust tests: 421 unit tests passed, 2 LSP smoke tests passed.
- mini-c force index: 2 files indexed, 13 symbols.
- mini-c incremental index: 2 files skipped, 13 symbols retained.
- VSIX includes `extension/bin/fossilsense.exe`.

## Install

```powershell
code --install-extension "F:\HimuToys\HimuToys.FossilSense\dist\fossilsense-vscode-1.2.0_BUILD20260705_020402.vsix"
```
