# Store Read Views and Read Models

Status: current

The SQLite index remains owned by `IndexStore`. The read-view refactor did not
change the schema version, index path convention, WAL-compatible read behavior,
or connection ownership model.

## Read Views

Durable read use cases that cross module boundaries now have narrow views under
`store::views`:

- `NameTableStoreView`
- `ReachGraphStoreView`
- `IncludeTableStoreView`
- `SymbolReadView`
- `ReferenceFileStoreView`
- `MemberStoreView`

`IndexStore` exposes constructors such as `name_table_view()`,
`reach_graph_view()`, `include_table_view()`, `symbol_read_view()`,
`reference_file_view()`, and `member_view()`.

## Typed Rows

Typed rows live at the store/query boundary:

- `NameTableSymbolRow` names symbol id, label, source/external flag, path, kind,
  and directly-included evidence.
- `IncludeEdgeRow`, `IncludeEdgeResolutionRow`, and `OpenIncludeRow` preserve
  reach-graph edge and open-scope reason data.
- `IncludeCompletionPathRow` represents workspace paths used by include
  completion table rebuilds.
- `ReferenceFileRow` represents indexed workspace files used by reference
  discovery.
- `RecordReadRow` and `MemberReadRow` are internal member-read rows that convert
  to existing `RecordCandidate` and `MemberCandidate` domain objects.

Compatibility wrappers on `IndexStore` may remain while parity tests still use
them as behavior oracles. Wrappers should delegate to the read views or shared
typed-row loaders.

## Builder and Feature Expectations

- `NameTable`, `ReachGraph`, and `IncludeCompletionTable` rebuild paths consume
  typed rows or narrow read views rather than tuple-shaped SQL contracts.
- Definition, hover, signature, CLI symbol lookup, reference-file loading, and
  member completion use narrow read views where durable exact-store queries are
  needed.
- Member read paths preserve alias recursion, same-tier deduplication, prefix
  filtering, field/method/static-method ordering, fallback caps, and
  `ScopeTier` computation through `resolver`.
- Feature code must not depend on `rusqlite`, SQL column ordering, or LSP
  presentation types.

## Verification

Focused coverage for this contract lives in:

- `store::tests::read_views`
- `store::tests::read_model_parity`
- `store::tests::read_view_migration`
- `store::tests::parser_consumer_migration`

The architecture fitness check also verifies that `rusqlite` remains isolated to
store/persistence modules.
