# Large-workspace performance runbook

status: current

scope: U-Boot / Wine call relations, full indexing, process peak memory

## Purpose

This runbook fixes the measurement boundary used by the large-workspace performance work. It intentionally keeps the public U-Boot and Wine checkouts and generated SQLite files outside Git. Reports contain anonymous case IDs and aggregate counters only; they do not copy source identifiers, source snippets, or workspace paths.

## Prerequisites

Build the release binary. Query-only comparison requires prepared schema-15 indexes under `target/benchmark/`; a filtered full-index run does not require query databases:

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

To isolate one full-index case without first running query cases:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/benchmark_large_workspace.ps1 `
  -Repeats 1 -IncludeFullIndex -CaseFilter wine-full-index -TimeoutSeconds 1800
```

Full-index cases remove only their dedicated database and WAL/SHM sidecars under the resolved benchmark root before every repetition. This guarantees that bulk-writer measurements start with an empty schema and cannot accidentally exercise an older database's online indexes.

The relevant gates are `write_ms`, `elapsed_ms`, peak memory, final database bytes, and the call-fact `dbstat` bytes recorded by the accompanying analysis. A tuning result must report both elapsed time and memory/disk cost; elapsed-only wins are insufficient.

## Relation-query metrics

Current `query calls` output exposes only bounded request-index counters: `relation_query_entities`, `relation_query_call_sites`, `relation_query_relations`, `relation_query_call_site_refs`, `relation_query_ms`, and `query_us`. Benchmark query cases must use schema-15 `*-rebuild.sqlite` databases.

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

High-ambiguity budget and pagination are covered by the lazy call-service tests and the ignored release benchmark above.

## Name-index publication benchmark

The ignored release benchmark measures schema-15 SQLite-to-name-index streaming construction and repeated replacement of the single file with the most symbol rows. It prints aggregate counts/timings only:

```powershell
$env:FOSSILSENSE_BENCH_DB = (Resolve-Path 'target/benchmark/index-wine-rebuild.sqlite').Path
cargo test --release -p fossilsense `
  query::tests::benchmark_large_name_table_build_and_dirty_update -- `
  --ignored --exact --nocapture
```

`name_stream_build_ms` covers the complete borrowed SQLite visitor plus final immutable index construction; there is no separate owned-row load phase. `name_sql_visit_ms` and `name_finalize_ms` split intern/row visitation from immutable arena/posting finalization without adding production timers. On Windows the test samples its own Private Bytes every millisecond around dirty replacement. `name_dirty_private_delta_bytes` is the process peak above the fully built base table, not an allocator estimate. After preserving the five-update dirty measurement, the test accumulates worst-case replacements until the background-compaction threshold is reached and reports `name_compaction_input_segments`, `name_compaction_ms`, and `name_compaction_private_delta_bytes` independently.
