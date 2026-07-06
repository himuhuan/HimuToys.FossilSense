# Behavior Regression Checklist

Status: current

Use this checklist for behavior-preserving architecture work. A checked item
means the current change was reviewed against existing tests or focused smoke
coverage; it does not authorize intentional user-visible behavior changes.

## User-Visible Contracts

- [x] Completion: ordinary identifier completion still uses the evidence-aware
  pipeline, keeps `isIncomplete = true`, preserves short-prefix gating, uses
  current/local evidence before local-word fallback, and does not add SQLite IO
  to the per-key hot path.
- [x] Definition: exact-name candidates still preserve current, reachable,
  external, unknown, and global tier ordering through `resolver`.
- [x] Coloring: macro, type, enum, parameter, and local-variable coloring still
  uses request-time parser facts and keeps conservative fallback behavior.
- [x] References: role classification still uses request-time occurrences when
  available and falls back to read role on unavailable parse facts.
- [x] Include behavior: include resolution, open-scope reasons, ambiguous
  include handling, reach graph rebuild, and include completion table inputs are
  preserved.
- [x] Member behavior: resolved receiver candidates, alias recursion,
  same-tier deduplication, prefix filtering, field/method/static-method
  ordering, fallback prefix gate, and fallback caps are preserved.
- [x] Store behavior: SQLite schema, read-only open behavior, WAL-backed reads,
  and durable index path conventions are unchanged.
- [x] Parser behavior: lexical symbols/includes remain unconditional; skipped
  request facts, genuinely empty AST facts, and lexical fallback are now
  distinguishable without changing tolerant parsing.

## Verification Used For This Review

- Focused store/read-view and read-model parity tests.
- Focused parser fact-mask and fact-availability tests.
- Parser consumer migration guard tests.
- Full `cargo test -p fossilsense`, including LSP smoke tests.
- CLI smoke on `samples/mini-c` for `scan` and forced `index`.
- Architecture fitness check with no failing boundary violations.

## Remaining Transitional Items

- One architecture allowlist remains: `crates/fossilsense/src/query/lsp_kinds.rs`
  imports `tower_lsp` as a transitional LSP-kind adapter. The allowlist reason
  is encoded in `scripts/architecture_fitness.js`.
- Large-file warnings remain as architecture fitness warnings. They are
  pre-existing pressure points and should be reduced by future focused
  extraction, not by this behavior-preserving store/parser contract change.
