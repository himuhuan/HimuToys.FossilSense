# Architecture Fitness Functions

Status: current Phase B baseline for the v1.2.2 architecture health release.

The local architecture fitness entry point is:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/verify_architecture_fitness.ps1
```

The PowerShell wrapper calls `node scripts/architecture_fitness.js`. This uses only the Node runtime already required by the VS Code extension toolchain and does not add a runtime dependency to the Rust binary or packaged VSIX.

Useful direct commands:

```powershell
node scripts/architecture_fitness.js --format text
node scripts/architecture_fitness.js --format json
node scripts/test_architecture_fitness.js
```

The golden tests in `tests/architecture_fitness/` cover forbidden dependency reporting, allowlist reasons, warning-only large-file reporting, and the future ordinary-completion-service rule shape that rejects `tower_lsp` outside the server/LSP adapter boundary.

## Report Shape

Each finding prints:

- `status`: `FAIL`, `WARN`, or `ALLOWLISTED`.
- `severity`: the rule severity, currently `ERROR` or `WARN`.
- `rule`: stable rule name.
- `file`: repository-relative source path.
- `detail`: actionable reason for the finding.
- `allowlist`: explicit transitional reason, or `-`.

The command exits non-zero only for unallowlisted `FAIL` findings. Warnings and allowlisted findings are visible but do not fail the current v1.2.2 tree.

## Current Rules

| Rule | Severity | Contract |
| --- | --- | --- |
| `lsp-boundary` | `ERROR` | `tower_lsp` usage is limited to `crates/fossilsense/src/server.rs` and `crates/fossilsense/src/server/**`. |
| `ordinary-completion-service-lsp-boundary` | `ERROR` | The ordinary completion service at `crates/fossilsense/src/completion/ordinary_service.rs` must remain protocol-neutral and must not import `tower_lsp`. |
| `sqlite-boundary` | `ERROR` | `rusqlite` usage is limited to `crates/fossilsense/src/store.rs` and `crates/fossilsense/src/store/**`. |
| `core-dependency-direction` | `ERROR` | Parser must not depend on store/server/indexer; resolver must not depend on parser/store/server/indexer; model must not depend on store/server/indexer; store must not depend on server handlers. |
| `large-source-file` | `WARN` | Rust and extension source files above 800 lines are reported as warnings during v1.2.2. |

The current transitional allowlist is intentionally narrow:

| Rule | Path | Reason |
| --- | --- | --- |
| `lsp-boundary` | `crates/fossilsense/src/query/lsp_kinds.rs` | Query currently contains a transitional LSP-kind adapter; move it under `server/lsp_adapters.rs` during a later behavior-preserving step. |

## Verification Gate Relationship

The architecture fitness command is a guardrail, not a replacement for functional verification. Use it alongside the broader local gate:

```powershell
cargo test -p fossilsense
# LSP smoke tests: run the repository LSP smoke command used by the release phase.
cargo run -p fossilsense -- scan samples/mini-c
cargo run -p fossilsense -- index samples/mini-c --db target/mini.sqlite --force
Set-Location extensions/vscode
pnpm run compile
pnpm test
pnpm run package
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/verify_release_hardening.ps1
```

Phase H remains responsible for running the full gate, confirming the self-contained VSIX under `dist/`, and inspecting that the packaged extension includes the release `fossilsense.exe`.

## Baseline Result

Recorded on 2026-07-06 with:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/verify_architecture_fitness.ps1
```

Current Phase D result after extracting the ordinary completion service:

```text
Summary: fail=0 warn=9 allowlisted=1
```

Warnings are warning-only large source files:

| Path | Lines |
| --- | ---: |
| `crates/fossilsense/src/coloring.rs` | 1023 |
| `crates/fossilsense/src/completion.rs` | 1720 |
| `crates/fossilsense/src/parser/ast.rs` | 806 |
| `crates/fossilsense/src/query.rs` | 876 |
| `crates/fossilsense/src/query/tests.rs` | 816 |
| `crates/fossilsense/src/resolver.rs` | 851 |
| `crates/fossilsense/src/server/include_completion.rs` | 1327 |
| `crates/fossilsense/src/server/language_server.rs` | 880 |
| `crates/fossilsense/src/server/tests.rs` | 1766 |

The allowlisted finding is:

| Rule | Path | Reason |
| --- | --- | --- |
| `lsp-boundary` | `crates/fossilsense/src/query/lsp_kinds.rs` | Transitional LSP-kind adapter in query. |
