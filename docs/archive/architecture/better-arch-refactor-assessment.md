> **Status: superseded** (2026-07-10)
>
> 权威事实以仓库根目录 CLAUDE.md 与当前代码为准。本文是历史过程/评估文档，只保留决策痕迹，不得当作 backlog、实现规范或自动复活的愿景来源。
# Better Architecture Refactor Assessment

Status: current implementation plan, 2026-07-10

This assessment compares `docs/research/better-arch-refactor.md` with the current
source tree. The source, tests, and `CLAUDE.md` are authoritative. Historical
planning artifacts are intentionally excluded.

## Compatibility Contract

The refactor may change internal Rust types, module layout, SQLite schema, and
private protocol payloads. It must preserve C-side/editor behavior:

- the advertised LSP capabilities, commands, configuration keys, and index-state
  transitions remain unchanged;
- completion labels, kinds, detail/documentation, ordering, truncation, and
  `isIncomplete` behavior remain unchanged;
- definition, hover, signature, references, include navigation, member
  completion, semantic coloring, fallback, ambiguity, and open-scope behavior
  remain unchanged;
- CLI scan/index meaning and index durability remain unchanged;
- failures may degrade a capability, but must not publish half-updated read
  models or report a failed cache as ready.

## Recommendation Matrix

| Recommendation | Current evidence | Decision |
|---|---|---|
| Keep one Rust process and a modular monolith | The extension is thin and the Rust binary owns indexing and LSP | Keep; do not introduce services |
| Publish one immutable workspace snapshot | A `WorkspaceSnapshot` exists, but dirty include refresh mutates an `Arc<RwLock<ReachGraph>>` already held by older snapshots; component staging maps are independently mutable | Implement now as the highest-priority correctness boundary |
| Capture one request context | Request handlers already capture workspace snapshots in important paths, but document/config/history capture is still assembled per handler | Introduce the engine-side request context now; fold remaining request inputs into it incrementally |
| Split persistent/document/quality parser products | `PersistentFacts`, `RequestFacts`, `FactAvailability`, and `ParseDiagnostics` already provide typed projections over the single parse result | Keep the current entry point and projections; do not perform a disruptive duplicate rewrite |
| Isolate storage behind ports/read views | Typed `store::views` exist and production consumers are guarded by tests | Keep and strengthen; do not add repository interfaces that only rename existing narrow views |
| Unify candidate evidence but keep feature-specific policies | Completion already has `CandidateEvidence`; `resolver` owns scope/confidence primitives; coloring remains suppress-only | Continue incrementally; do not create a universal score or force references through resolver ranking |
| Turn ordinary completion into providers + normalize/rank/budget/presentation stages | `ordinary_service` is a boundary, but provider functions and the 1,736-line completion core remain tightly grouped | Implement after snapshot consistency, locked by the existing presentation fixture and ranking tests |
| Create an include domain | Include parsing, persistence, reachability, completion, and LSP presentation are still spread across layers | Adopt ownership rules now; reorganize after snapshot publication no longer depends on mutable graph state |
| Add layered semantic fingerprints and a revision vector | Only whole-file/index generations are explicit today | Defer until invalidation counters and equivalence tests establish which fan-out is worth optimizing |
| Add priority scheduling and propagated cancellation | Blocking work is moved off Tokio workers, but there is no unified priority/cancellation executor | Adopt in a later measured phase; preserve current request behavior first |
| Add capability-specific health | `DegradedCapabilities` already distinguishes reach graph, include table, and reference file list | Extend the internal snapshot health model now; change client presentation only with compatibility tests |
| Upgrade bundled SQLite for the WAL-reset fix | `rusqlite 0.32.1` bundles SQLite 3.46.0; SQLite documents the WAL-reset corruption bug through 3.51.2 and its fix in 3.51.3 | Implement now and enforce the fixed runtime version in tests |
| Dense `FileId`, reverse graph, bitmap reachability, SCC compression | Current bounded string graph is simple and covered; no profiling proves it is the dominant cost | Do not implement without large-workspace graph measurements |
| Incremental tree-sitter for open documents | Open-document parses are cached by version but rebuilt from text | Defer until document revision/cancellation semantics are explicit |
| Crate split or Salsa | Module boundaries are still moving and manual invalidation has not been measured | Explicitly reject for this phase; reassess only after stable engine boundaries |
| Versioned SQL rows for hot-path hydration | Completion and common scope reads already use memory models; cold features use narrow short-lived reads | Do not add schema complexity unless a request is proven to mix generations |
| Architecture fitness and equivalence tests | Dependency guards exist; generation atomicity and incremental/full equivalence coverage are incomplete | Add focused invariants throughout the refactor |

## Implemented In This Refactor

- Replaced the independently visible name/reach/include/reference caches with a
  single immutable `EngineSnapshot` map. A serialized publisher builds every
  component off to the side and assigns an explicit monotonic `EngineEpoch`
  immediately before one atomic publication.
- Made dirty reach-graph refresh functional: the next graph copies the prior
  edges/open state and applies changed sources without mutating any graph held by
  an in-flight request.
- Added `RequestContext` as the request-side owner of one engine snapshot and one
  settings snapshot. Completion perf logs now identify both the document version
  and combined engine generation.
- Enforced latest-document-revision-wins for live parse and local-word cache
  publication. Old work may finish for its own request, but cannot overwrite the
  cache after `didChange` advances the document.
- Split ordinary completion into `intent`, `pipeline`, and provider-conversion
  modules while retaining the existing evidence fields, numeric policy,
  guard-band, history cap, deterministic tie-breaks, quotas, truncation, and LSP
  presentation fixture.
- Extracted the include completion read model/evidence into its own domain model
  module. Include resolution, disk adapter, store adapter, and LSP presentation
  remain separate concerns without changing lookup or ranking order.
- Moved the remaining LSP symbol-kind mapping out of `query` and into
  `server/lsp_adapters`, then removed the architecture allowlist.
- Upgraded `rusqlite` to 0.39.0 / bundled SQLite 3.51.3 and added a resilience
  test that rejects versions predating the WAL-reset fix.

The existing parser persistent/request/quality projections and typed store read
views were retained because the current source already satisfies those parts of
the recommendation; duplicating them under new names would create concept drift.

## Deliberately Deferred Or Rejected

- Layered semantic fingerprints and revision vectors are deferred until local
  invalidation fan-out is measured. The current refactor establishes the epoch
  and immutability boundary they require.
- A priority executor, provider deadlines, and propagated cancellation are
  deferred until request latency/cancellation baselines exist. `spawn_blocking`
  continues to keep synchronous parsing and SQLite work off Tokio workers.
- Dense file IDs, reverse adjacency, bitmaps, and SCC compression require large
  include-graph profiling; the bounded deterministic graph remains the simpler
  correct implementation today.
- Incremental tree-sitter for open documents remains a follow-up after edit-range
  transport and cancellation are defined; version-keyed whole-document parsing
  remains behaviorally stable.
- Crate splitting, Salsa, services, versioned SQL rows, and new client-visible
  capability protocols are rejected for this refactor because they add migration
  or compatibility cost without evidence that they solve the present defects.

## Verification Evidence

- `cargo test -p fossilsense`: 531 unit tests and 2 LSP smoke tests passed.
- Atomicity tests prove dirty publication leaves the old engine graph unchanged
  and that stale document work cannot overwrite the latest revision caches.
- Completion intent/ranking/provider tests and the LSP presentation compatibility
  fixture passed without expectation changes.
- Architecture fitness golden tests passed; repository report is
  `fail=0`, `allowlisted=0`.
- CLI smoke still scans 2 files and force-indexes 13 symbols from
  `samples/mini-c`.
- Bundled SQLite runtime guard passed at version 3.51.3 (`3051003`).

## Phased Implementation

### Phase 0/1: state consistency and observability

- Replace independently visible cache state with one immutable
  `EngineSnapshot` per workspace.
- Build all read-model parts off to the side and publish them under one lock.
- Replace pointer-derived generations with an explicit monotonic engine epoch.
- Ensure dirty reach-graph updates create a new graph instead of mutating an old
  snapshot.
- Serialize snapshot publishers and keep the prior snapshot visible until the
  new one is complete.
- Test that an in-flight snapshot keeps its old name/reachability generation
  across dirty publication.

Exit condition: every request-side indexed read comes from one published engine
snapshot, and old snapshots cannot be mutated by later indexing.

### Phase 2: request and data-product boundaries

- Capture document revision, engine epoch, settings, and bounded request budget
  in a request context.
- Keep `parse()` as the only parser entry point while moving consumers to the
  existing persistent/request/quality projections.
- Finish LSP type relocation from protocol-independent query modules.

### Phase 3: completion and evidence pipeline

- Extract intent, evidence, normalization/dedup, rank policy, metrics, and
  provider modules without changing the golden presentation fixture.
- Give each provider explicit recall output and quota; keep current quotas,
  ordering, guard bands, history cap, and deterministic tie-breaks.
- Add Top-K churn as a local/CI metric only; no telemetry.

### Phase 4: measured incrementality and scheduling

- Add invalidation fan-out counters and incremental/full equivalence tests.
- Introduce layered fingerprints only for proven independent products.
- Propagate cancellation/budgets into expensive providers and graph/reference
  traversals before considering a custom scheduler.

## Verification Gates

- `cargo test -p fossilsense`
- focused snapshot immutability and publication tests
- completion presentation/ranking tests and LSP smoke tests
- `scripts/verify_architecture_fitness.ps1 -Format text`
- CLI scan and forced-index smoke on `samples/mini-c`
- final grep/audit that request paths do not access staging read models
