# ADR 0002: SQLite Durable Index And In-Memory Read Models

Status: Accepted

## Context

The Rust binary stores durable workspace facts in SQLite and serves hot LSP requests from in-memory read models. This is already visible in `IndexStore`, `NameTable`, `ReachGraph`, `IncludeCompletionTable`, indexed file lists, local word caches, reference caches, and completion memo state.

Large Windows workspaces make per-keystroke disk IO, SQLite queries, or workspace scans unacceptable for ordinary completion and most request-time ranking.

## Decision

SQLite remains the durable index. It owns persistence for symbols, records, members, aliases, includes, include edges, file fingerprints, source labels, and schema migration.

Hot request paths use in-memory read models instead of direct SQLite access:

- `NameTable` supports workspace symbol lookup and ordinary completion recall.
- `ReachGraph` supports include reachability, ambiguity, fallback, and open scope.
- `IncludeCompletionTable` supports include completion and include ranking evidence.
- Indexed file lists support references without re-enumerating the workspace.
- Local word and completion memo caches support open-document completion behavior.

The server may rebuild read models from SQLite after indexing, but ordinary completion must not open SQLite on each keystroke. Query handlers that still need durable rows should keep that access visible at the store boundary.

## Consequences

This keeps interactive latency predictable and supports privacy defaults because request-time logs can report counts and timings without candidate names or source snippets.

The cost is cache invalidation complexity. Phase C must make cache invalidation and generation updates more explicit without changing current behavior.
