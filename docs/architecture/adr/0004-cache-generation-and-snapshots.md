# ADR 0004: Cache Generation And Per-Request Snapshots

Status: Accepted

## Context

The current server keeps maps for documents, stores, `NameTable`, `ReachGraph`, `IncludeCompletionTable`, indexed file lists, `WorkspaceGeneration`, local word cache, reference cache, and completion memo. Request handlers currently assemble the pieces directly.

Later refactoring will introduce `DocumentStore`, `CacheLedger`, `WorkspaceSnapshot`, and `WorkspaceSession` style boundaries. Phase A documents the intended semantics before those moves.

## Decision

`WorkspaceGeneration` remains the compatibility token for cache invalidation. Successful full or dirty indexing updates the generation after read models are rebuilt or refreshed. Completion memo and reference cache validity depends on generation and request-specific inputs such as prefix and document version.

A future `WorkspaceSnapshot` SHALL be an immutable per-request view containing the root, generation, settings needed by the request, and `Arc` clones of available read models. Missing read models remain explicit so existing fallback behavior can continue.

Request handlers should clone snapshot data before expensive work or `spawn_blocking`. They should not hold document/cache locks across blocking query, parse, completion, or indexing work.

Open scope remains part of the snapshot-visible query context through `ReachScope`; it is not cache invalidation by itself, but it affects confidence, fallback, ambiguity, and ranking semantics.

## Consequences

This preserves current behavior while creating a target shape for Phase C. The main risk is stale data after dirty indexing; Phase C tests must cover cache invalidation for read models, completion memo, reference cache, local words, did_change, did_close, full index, and dirty index.
