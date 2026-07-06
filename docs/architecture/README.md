# FossilSense Architecture Notes

Status: current

This directory records the current architecture contracts that are expected to
stay stable across behavior-preserving refactors. Research material under
`docs/research/` is useful background, but these notes are the canonical
maintenance reference for implemented boundaries.

## Current Shape

FossilSense remains one Rust native binary plus a thin VS Code extension:

- `extensions/vscode` owns process management, configuration bridging, status
  UI, commands, and conflict prompts.
- `crates/fossilsense` owns scanning, parsing, indexing, SQLite storage,
  in-memory read models, query logic, and the LSP server.
- CLI commands (`scan`, `index`, `lsp`) exercise the same scanner, parser,
  indexer, store, and query paths used by the extension.

## Core Boundaries

- `model` and `resolver` are the candidate semantics center:
  `DefinitionCandidate`, `ScopeTier`, `ResolutionConfidence`,
  `ResolutionReason`, `ReachScope`, `OpenReason`, `RecordCandidate`, and
  related ranking helpers stay there.
- `parser` is tolerant and independent from store/server/indexer details. It
  exposes one compatibility parse product, `FileSemanticIndex`, plus
  persistent/request fact projections.
- `store` owns SQLite schema, migrations, writes, and SQL-to-row/domain
  conversion. Durable reads that cross module boundaries go through narrow read
  views and typed rows.
- `query`, `reachability`, `coloring`, `references`, `completion`, and
  `server` consume FossilSense domain concepts, not raw SQL rows.
- `tower_lsp` stays at the server boundary, with the current
  `query/lsp_kinds.rs` adapter documented as a transitional allowlist item.
- `rusqlite` stays inside `store`.

## Implemented Contract Notes

- Store read-view contract: [read-models.md](read-models.md)
- Parser fact projection contract: [parser-facts.md](parser-facts.md)
- Regression checklist: [regression-checklist.md](regression-checklist.md)
- ADR: [adr/0001-store-read-views-and-parser-facts.md](adr/0001-store-read-views-and-parser-facts.md)

## Fitness Checks

Run:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/verify_architecture_fitness.ps1
```

The check currently fails on boundary violations, warns on large source files,
and reports transitional allowlist entries. Warnings are not permission to grow
those files further; they are a prompt to extract pure modules when future work
adds meaningful behavior.
