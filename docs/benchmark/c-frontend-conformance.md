# C frontend conformance

This repeatable check compares complete source declaration multisets rather than total symbol counts. It is a development/CI tool; Clang is not a runtime or VSIX dependency.

## Environment and commands

Use stable Rust and Node.js 22. Ordinary regression tests need no Clang:

```powershell
cargo test -p fossilsense --bin fossilsense c_frontend_
node scripts/test_c_frontend_conformance.mjs
node scripts/test_c_frontend_clang.mjs
```

For the independent oracle, install Clang **22.1.3**, then provide its executable explicitly:

```powershell
node scripts/verify_c_frontend_conformance.mjs --clang 'C:/Program Files/LLVM/bin/clang.exe'
```

The Windows CI job downloads the official LLVM installer and verifies its SHA-256 against `crates/fossilsense/tests/fixtures/c_frontend/manifest.json`. A missing executable, different version, compiler error, timeout, or comparison failure returns a nonzero exit code; it never falls back to another compiler. The manifest locks C11, `x86_64-pc-windows-msvc`, `-nostdinc`, known decorator stubs, and each `-D`/`-U` configuration.

The Node entry point invokes exactly:

```powershell
cargo test -p fossilsense --bin fossilsense parser::tests::conformance::export_c_frontend_observations -- --exact --ignored
```

Only that child receives the manifest, exclusive output path, and run ID through the three `FOSSILSENSE_CONFORMANCE_*` environment variables. The ignored test writes a separate JSON file atomically. Missing output, invalid JSON, stale run ID/manifest fingerprint, nonzero Cargo status, or a changed fixture fails the check, including Cargo's otherwise successful zero-test case. FNV-1a checks export freshness; the report independently records SHA-256 for the manifest, expectations, fixtures, stubs, and relevant code inputs.

## Supported observation scopes

| Corpus | Checked contract |
|---|---|
| `declarators.c` | Multiple functions/objects, mixed function pointers in both orders, factory function, function typedefs |
| `records.c` | Forward tag, record definition, fields/function-pointer fields, alias; repeated tag names remain distinct occurrences |
| `guards.c` | Raw condition evidence plus separate Clang projections with `-DFEATURE=1` and `-UFEATURE` |
| `local.c` | Persistent declarations and function-local bindings; local record members are explicitly unsupported regions |
| `decorated.pb-c.h` | Known protobuf-c markers, export macro, and alignment recovery in one source file |
| `unknown_macro.c` | Manual unknown-macro range/reason contract; no invented compiler stub or generated name |
| `shared.h` | Manual shared-header language ambiguity contract |
| `empty.c` | Checked empty groups; zero metric denominators stay `null` |

Persistent facts, request-local bindings, and unsupported regions are declared separately in the manifest. Clang's implicit/system declarations and prototype-only parameter bindings are excluded explicitly. Local record members may be excluded only inside a matching declared unsupported range, and exclusions are recorded in the report.

Positions include UTF-8 byte offsets and zero-based UTF-16 line/character pairs. Full declaration statements include semicolons; function definitions use their signature before the body. Forward tags use the existing canonical name range. Fields and local bindings expose name ranges only. Clang spans are normalized to this stated contract using their source locations; names, kinds, roles, and owners come from Clang. Raw preprocessor guard spelling is checked against the independent expectation file, because Clang removes directives; Clang separately verifies which declarations survive each explicit configuration.

The Rust regressions also insert a comment containing `__aligned`, convert LF to CRLF, add indentation, and swap sibling declarators. Expected positions are mapped from reviewed original offsets, not regenerated from parser output. The decorator fixture repeats these transformations while combining recovery rules. Recovery region/depth limits and unchanged neighboring facts have separate assertions.

## Results and acceptance

Each invocation creates `target/c-frontend-conformance/run-<id>/report.json`, the exclusive `observations.json`, and bounded child-process logs/AST dumps. Reports include source commit/status, input hashes, compiler version, target, exact arguments, elapsed times, differences, exclusions, and separate metrics. CI retains the directory even on failure. Tests never overwrite the expectation file.

| Metric | Required result for fully supported fixed samples |
|---|---|
| Full-tuple recall / precision | 100% independently; no missing or extra source occurrence |
| Name / kind / role / scope / position / guard correctness | 100% for each measured dimension |
| Uncovered candidate bytes | 0 |
| False Complete claims | 0 |
| Empty denominator | `null`, never an invented 100% |
| Truncated coverage | Ratio unknown; cannot pass as complete |

Unsupported samples must match their explicit gaps and available facts exactly. These rates describe this fixed corpus, not whole-workspace accuracy. Clang guard-spelling metrics are `null` because that dimension is measured by the separate raw-source contract.

The quality check does not replace performance acceptance. After parser/indexing changes, run the release U-Boot full-index, engine-hydration, and production-completion-replay cases documented in this directory. Preserve the independent 120-second indexing, 384/512-MiB memory, and 50-ms completion P95 gates.
