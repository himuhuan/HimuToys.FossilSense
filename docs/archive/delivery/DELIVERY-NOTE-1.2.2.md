> **Status: archived** (2026-07-10)
>
> 历史交付说明。当前版本交付说明见 dist/DELIVERY-NOTE-<version>.md。
# FossilSense v1.2.2 Delivery Note

Date: 2026-07-06
VSIX artifact: `dist/fossilsense-vscode-1.2.2_BUILD20260706_014824.vsix`

## Release Scope

v1.2.2 is a behavior-preserving architecture health release. It packages the Phase A-D architecture refactor work and Phase H release hardening without intentional user-visible changes to navigation, completion, references, semantic coloring, configuration, privacy defaults, or packaging behavior.

## What Changed

- Version updated to `1.2.2` for the Rust engine and VS Code extension.
- Architecture baseline, ADRs, risk register, regression checklist, import inventory, and architecture fitness functions are current for the v1.2.2 release.
- Workspace state access is guarded by the `DocumentStore`, `CacheLedger`, `WorkspaceSnapshot`, and `WorkspaceSession` boundaries introduced during the architecture health work.
- Ordinary identifier completion has a protocol-neutral service boundary while preserving the existing evidence-aware merge/rank/truncate pipeline and LSP presentation.
- Release hardening now includes `scripts/verify_release_hardening.ps1`, which checks version metadata, release notes, the v1.2.2 VSIX artifact, and the bundled native binary.
- Final Review follow-up candidates for optional Phases E, F, and G are recorded at `docs/architecture/follow-ups.md`; they are not required v1.2.2 work.

## Verification performed

Passed:

```powershell
cargo fmt
cargo test -p fossilsense
cargo test -p fossilsense --test lsp_smoke
cargo run -p fossilsense -- scan samples/mini-c
cargo run -p fossilsense -- index samples/mini-c --db target/mini.sqlite --force
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/verify_architecture_fitness.ps1
cd extensions/vscode
pnpm run compile
pnpm test
pnpm run package
```

Results:

- Rust tests: 488 unit tests passed.
- LSP smoke tests: 2 tests passed in the full Rust gate and again in the dedicated smoke command.
- CLI scan: `samples/mini-c` found 2 files.
- CLI forced index: 2 files indexed, 13 symbols, database `target/mini.sqlite`.
- Architecture fitness baseline: `fail=0 warn=9 allowlisted=1`.
- Extension compile/tests: `pnpm run compile` passed; `pnpm test` passed after synchronizing the release-version assertion to `1.2.2`.
- Package: `pnpm run package` produced `dist/fossilsense-vscode-1.2.2_BUILD20260706_014824.vsix`.
- Native binary inspection: the VSIX includes non-empty `extension/bin/fossilsense.exe` built by `cargo build --release -p fossilsense`.

## Unchanged capabilities

- Best-effort Go to Definition, Workspace Symbols, Document Outline, Hover, Signature Help, semantic coloring, references, include analysis, ordinary completion, include completion, member completion, and local-only completion history remain in scope.
- Candidate results remain ranked best-effort evidence, not compile-accurate semantic bindings.
- Completion remains metadata-only in verbose/perf logs by default and does not log candidate names, source snippets, accepted labels, or raw include paths.
- The VSIX remains self-contained and does not require users to install Rust, clangd, ctags, cscope, or compile commands.

## Known non-goals

- No complete C++ semantic model: inheritance, overload resolution, templates, namespaces, access control, expression type inference, and compile-accurate binding remain out of scope.
- No ML ranking, telemetry, cloud sync, or auto include insertion.
- No user-visible behavior change is intended for v1.2.2; architecture changes are internal guardrails for safer future work.

## Install

```powershell
code --install-extension "F:\HimuToys\HimuToys.FossilSense\dist\fossilsense-vscode-1.2.2_BUILD20260706_014824.vsix"
```
