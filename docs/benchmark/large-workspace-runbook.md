# Large-workspace performance runbook

status: current

scope: U-Boot / Wine call relations, full indexing, process peak memory

## Purpose

This runbook fixes the measurement boundary used by the large-workspace performance work. It intentionally keeps the public U-Boot and Wine checkouts and generated SQLite files outside Git. Reports contain anonymous case IDs and aggregate counters only; they do not copy source identifiers, source snippets, or workspace paths.

## Prerequisites

Build the release binary and prepare the schema-14 baseline indexes described in `1.3.4-analyse.md`:

```powershell
cargo build --release -p fossilsense
```

Expected local inputs are `samples/u-boot`, `samples/wine`, `target/benchmark/index-u-boot.sqlite`, and `target/benchmark/index-wine.sqlite`. Missing cases are skipped with a warning.

## Query and memory benchmark

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/benchmark_large_workspace.ps1 -Repeats 3
```

The script launches a fresh process per run and samples Working Set and Private Bytes every 20 ms. It writes JSON and Markdown under `target/benchmark`; raw CLI output stays in process memory only until the script parses the whitelisted numeric metrics.

The fixed cases cover:

- U-Boot low-frequency incoming calls.
- Wine medium fan-in incoming calls.
- Wine high-frequency incoming calls.

Use at least three repetitions before comparing branches. Compare medians, retain the individual runs, and treat a result as meaningful only when the direction is larger than the same-build run-to-run spread.

## Full-index benchmark

The full rebuild is opt-in because it takes minutes and replaces dedicated benchmark databases:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/benchmark_large_workspace.ps1 `
  -Repeats 1 -IncludeFullIndex -TimeoutSeconds 1800
```

The relevant gates are `write_ms`, `elapsed_ms`, peak memory, final database bytes, and the call-fact `dbstat` bytes recorded by the accompanying analysis. A tuning result must report both elapsed time and memory/disk cost; elapsed-only wins are insufficient.

## Catalog phase metrics

The schema-14 `query calls` baseline exposes these temporary comparison metrics:

- `catalog_load_anchors_ms`
- `catalog_load_call_sites_ms`
- `catalog_group_entities_ms`
- `catalog_resolve_relations_ms`
- `catalog_finalize_ms`

They separate SQLite fact loading from candidate expansion and DTO/adjacency finalization. Phase 1 deletes this workspace-wide build from production paths; the fields remain useful only as the schema-14 oracle until the legacy catalog is removed.

## Correctness and synthetic gates

Run the focused semantic suite before accepting performance data:

```powershell
cargo test -p fossilsense call_catalog --no-fail-fast
cargo test -p fossilsense store::tests --no-fail-fast
```

The ignored release benchmark is diagnostic, not a pass/fail CI assertion:

```powershell
cargo test --release -p fossilsense benchmark_large_fan_in_catalog_and_cached_query -- --ignored --nocapture
```

High-ambiguity budget and pagination become hard CI gates in the lazy service phase. The schema-14 catalog benchmark exists only to preserve the before measurement.
