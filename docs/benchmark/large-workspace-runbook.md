# Large-workspace performance runbook

status: current

scope: U-Boot / Wine full indexing, call relations, process peak memory

## Purpose

This runbook defines the reproducible large-workspace performance boundary. Public U-Boot and Wine checkouts, generated databases, and raw reports stay outside Git. Committed results may contain aggregate counters and anonymous case IDs, but must not contain source identifiers, snippets, or local workspace paths.

The benchmark implementation is `scripts/benchmark_large_workspace.ps1`. Read that script before changing or interpreting a case; this document does not replace its current arguments or metrics.

## Inputs

Build the current release binary:

```powershell
cargo build --release -p fossilsense
```

Place at least one large checkout at:

```text
samples/u-boot
samples/wine
```

These directories are local and git-ignored. Query cases additionally require current indexes under `target/benchmark/`; create them with the full-index cases instead of reusing a database from an older schema.

## Required 60-second gate

Every release, major feature, or architecture/index/storage/parser/query/concurrency change must run at least one release full-index case:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/benchmark_large_workspace.ps1 `
  -Repeats 1 -IncludeFullIndex -CaseFilter u-boot-full-index -TimeoutSeconds 60
```

or:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/benchmark_large_workspace.ps1 `
  -Repeats 1 -IncludeFullIndex -CaseFilter wine-full-index -TimeoutSeconds 60
```

The build is outside the measured process. The full-index process and reported `elapsed_ms` must both be at most `60,000 ms`. A timeout or any value above 60 seconds fails the feature; a faster mini-c run or a lower average does not override that result.

For a release candidate, run both cases when both checkouts are available. Record the source revision of the sample, machine CPU/RAM/storage, exact command, `elapsed_ms`, `write_ms`, phase timings, peak Working Set/Private Bytes, and final database size.

## Repeated diagnostic runs

After the hard gate passes, repeated runs can be used to compare branches:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/benchmark_large_workspace.ps1 `
  -Repeats 3 -IncludeFullIndex -CaseFilter wine-full-index -TimeoutSeconds 60
```

Compare medians while retaining individual runs. A change is meaningful only when it is larger than same-build run-to-run spread. A median below 60 seconds does not hide an individual required run above the gate.

The script writes JSON and Markdown reports under `target/benchmark/`. It starts a fresh process per repetition and samples Working Set and Private Bytes every 20 ms.

## Call-relation query cases

After building current full indexes, run bounded query cases with at least three repetitions:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/benchmark_large_workspace.ps1 -Repeats 3
```

The fixed cases cover low-frequency U-Boot incoming calls and medium/high fan-in Wine incoming calls. Review `relation_query_entities`, `relation_query_call_sites`, `relation_query_relations`, `relation_query_call_site_refs`, `relation_query_ms`, `query_us`, elapsed time, and peak memory.

## Correctness before performance

Performance data is invalid if focused correctness tests fail:

```powershell
cargo test -p fossilsense call_catalog --no-fail-fast
cargo test -p fossilsense store::tests --no-fail-fast
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/test_benchmark_entrypoints.ps1
```

Optional ignored release diagnostics:

```powershell
cargo test --release -p fossilsense benchmark_large_fan_in_catalog_and_cached_query -- --ignored --nocapture
```

For name-index publication diagnostics, point the ignored test at a current Wine rebuild database:

```powershell
$env:FOSSILSENSE_BENCH_DB = (Resolve-Path 'target/benchmark/index-wine-rebuild.sqlite').Path
cargo test --release -p fossilsense `
  query::tests::benchmark_large_name_table_build_and_dirty_update -- `
  --ignored --exact --nocapture
```

Report elapsed time together with memory and disk cost. An elapsed-only improvement is not sufficient when it causes unbounded memory, database growth, incomplete results without coverage, or correctness regression.
