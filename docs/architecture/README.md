# FossilSense Architecture Baseline

Status: current baseline for the v1.2.2 architecture health release.

This document freezes the current architecture before Phase C and Phase D refactoring. FossilSense remains a VS Code extension plus LSP over stdio to a single Rust binary. v1.2.2 is behavior-preserving: it MUST NOT intentionally change user-visible navigation, completion, coloring, references, configuration, privacy, or VSIX packaging behavior.

Related guardrails:

- ADRs: `docs/architecture/adr/`
- Risk register: `docs/architecture/risk-register.md`
- Regression checklist: `docs/architecture/regression-checklist.md`
- Import inventory: `docs/architecture/import-inventory.md`
- Architecture fitness functions: `docs/architecture/fitness-functions.md`
- Final review follow-ups: `docs/architecture/follow-ups.md`
- Release hardening verification: `scripts/verify_release_hardening.ps1`

## Product Boundary

FossilSense is for large Windows C/C++ workspaces where a reliable compile model is not available. It returns best-effort candidate results and exposes confidence, fallback, ambiguity, open scope, truncation, and cache invalidation behavior instead of claiming compile-accurate semantic binding.

No Phase A artifact changes runtime behavior. The baseline documents the behavior that later refactoring must preserve.

## High-Level Shape

```text
VS Code extension (TypeScript, extensions/vscode)
        |
        | LSP over stdio
        v
single Rust binary (crates/fossilsense)
  - CLI: scan, index
  - LSP: lsp
```

The extension owns activation, process launch, configuration bridge, status UI, commands, grouped references UI, completion-history command wiring, and conflict prompts. The Rust binary owns scanning, tolerant parsing, SQLite indexing, in-memory read models, query ranking, completion ranking, references, coloring, hover, signature help, and LSP request handling.

## Current Module Boundaries

| Boundary | Current owner | Notes |
| --- | --- | --- |
| Extension process management | `extensions/vscode/src/extension.ts`, `serverPath.ts` | Resolves `fossilsense.serverPath`, bundled `bin/`, then repo `target`; starts the LSP client. |
| LSP server boundary | `crates/fossilsense/src/server.rs`, `server/language_server.rs`, `server/*` | Owns tower-lsp types, request routing, document maps, cache maps, status notifications, commands, and LSP presentation. |
| CLI boundary | `crates/fossilsense/src/main.rs` | Provides `scan`, `index`, and `lsp` entry points. |
| Parser boundary | `parser.rs`, `parser/ast.rs`, `parser/lexical.rs` | The single normal parse entry is `parse(path, source) -> FileSemanticIndex`; parse failures degrade to lexical facts. |
| Store boundary | `store.rs`, `store/*` | Owns SQLite connections, schema, migrations, bulk writes, symbol/include/member/alias queries, and cleanup. |
| Indexer boundary | `indexer.rs`, `indexer/*` | Scans candidates, detects dirty files, parses changed files, writes SQLite payloads, and rebuilds include edges. |
| Read model boundary | `query::NameTable`, `reachability::ReachGraph`, `server::include_completion::IncludeCompletionTable` | Request-time data structures published after indexing so hot paths do not query SQLite per keystroke. |
| Resolver boundary | `resolver.rs`, `model.rs`, `reachability.rs` | Canonical source of `ScopeTier`, confidence/reason projection, and candidate ranking primitives. |
| Query boundary | `query.rs`, `query/*`, `references.rs`, `coloring.rs` | Protocol-neutral query logic is mixed with a small transitional LSP-kind adapter in `query/lsp_kinds.rs`. |
| Completion boundary | `completion.rs`, `completion_words.rs`, server completion routing | Ordinary completion currently combines LSP routing with evidence collection in the server and uses `completion` as canonical merge/rank/truncate pipeline. |

## Canonical Concepts

The current architecture uses these canonical model/resolver concepts and later phases must reuse them rather than inventing parallel "smart" or "semantic" concepts:

- `DefinitionCandidate`
- `ScopeTier`
- `ResolutionConfidence`
- `ResolutionReason`
- `ReachScope`
- `OpenReason`
- `Occurrence`
- `ReferenceHit`
- `RecordCandidate`
- record/member/alias models from parser/store/query

These are candidate semantics. A candidate with high confidence is still a ranked result, not proof of a compile-time binding.

## Durable Index And Read Models

SQLite is the durable index. It stores workspace-relative paths, external header paths, symbols, records, members, aliases, include facts, file fingerprints, and source labels. The default database location remains under the user cache directory; CLI `--db` is for tests and debugging.

Hot request paths use read models:

- `NameTable` for symbol-name recall and completion recall.
- `ReachGraph` for include reachability and open scope detection.
- `IncludeCompletionTable` for include-path completion and include ranking evidence.
- Indexed file lists for references.
- Local word and completion memo caches for open-document completion.

This split preserves performance and privacy: completion and most request routing use in-memory data and bounded current-document facts, not per-keystroke SQLite access or workspace scans.

## Startup Flow

1. VS Code activates `extensions/vscode/src/extension.ts`.
2. The extension reads configuration, detects C/C++ language-server conflicts, resolves the server binary, and creates a `LanguageClient`.
3. Initialization options bridge settings such as include paths, include scoping, debug candidate reasons, completion modes, semantic coloring mode, reference range display, perf logs, and completion history mode.
4. The Rust `lsp` command enters `server::run_stdio()`, constructs shared maps for open documents, stores, read models, roots, local words, reference caches, completion history, completion memo, and index scheduling.
5. `initialize` records workspace folders and capabilities. `initialized` schedules index work for roots and preloads completion history.
6. Status notifications flow back through `fossilsense/indexStatus` so the extension status bar can display discovering, checking, parsing, indexing, finalizing, ready, or failed.

Ownership: extension owns process/UI; server owns LSP state and index scheduling; indexer/store/parser own persistent facts and read-model rebuild inputs.

## Full Index Flow

1. A full index is scheduled from startup, `FossilSense: Full Rebuild Index`, or an explicit LSP command.
2. `server/indexing.rs` opens or creates the `IndexStore`, sends status, and calls `indexer::index_workspace` with current config and include paths.
3. `indexer` scans workspace and external headers, computes fingerprints, parses changed files through the parser boundary, writes file payloads to SQLite, and rebuilds include edges.
4. `server/indexing/cache.rs` rebuilds `NameTable`, `ReachGraph`, `IncludeCompletionTable`, indexed file list, and `WorkspaceGeneration`.
5. The server invalidates affected completion memo and reference caches so later requests do not reuse stale data.

Data movement: filesystem -> scanner/indexer candidates -> parser `FileSemanticIndex` -> store rows -> read models -> LSP request handlers.

## Dirty Index Flow

1. File watcher and save events enter `server/language_server.rs` and `server/indexing/watch.rs`.
2. Scope checks filter out changes outside configured workspace scope.
3. Dirty changes are debounced and coalesced by `IndexScheduleState`.
4. `indexer::index_dirty_files` updates only affected files and include edges.
5. Cache refresh tries to update read models incrementally where available, or rebuilds them when needed.
6. Cache invalidation clears stale completion memo/reference data for changed generations.

Dirty indexing must preserve the same visible state transitions and fallback behavior as full indexing. A failed read-model rebuild must not leave stale read models masquerading as ready.

## Query Flow

Definition, hover, signature help, semantic coloring, workspace symbols, references, and document symbols currently enter through LSP handlers in `server/language_server.rs` or helper files under `server/`.

Typical query data movement:

1. Convert LSP URI and position to a path and byte/text context.
2. Prefer open-document text when present; otherwise read from disk or durable index as the existing handler requires.
3. Resolve workspace root and available read models.
4. Compute `ReachScope` when include scoping is active.
5. Query store rows or in-memory tables as appropriate.
6. Rank through query/resolver helpers and project confidence/reason labels.
7. Map protocol-neutral results to LSP locations, markdown, semantic tokens, completion items, or command payloads.

Fallback is explicit. Missing parse facts, missing read models, unresolved includes, ambiguous includes, open scope, oversized files, bad config, or unavailable source comments should degrade to current documented behavior rather than fail hiddenly or claim stronger precision.

## Ordinary Completion Flow

Ordinary completion is separate from include completion and member completion routing:

1. `server/language_server.rs` receives a completion request.
2. The server first routes include contexts and member contexts to their specialized paths.
3. For ordinary identifiers, the server gets the current document snapshot, prefix, local word set, root/read-model maps, workspace generation, reach scope, history snapshot, and settings.
4. Indexed recall uses the in-memory `NameTable`; local bindings, current-file overlay, and local words come from bounded current-document parsing/text facts.
5. Candidate evidence is merged into `completion::PipelineCandidate` values.
6. `run_evidence_aware_pipeline_with_context` remains the canonical ordinary completion merge, deduplication, rank, guard-band, history boost, shadow-rank observation, and truncation step.
7. The server maps ranked candidates to LSP `CompletionItem`s, attaches completion-history accept commands when enabled, and returns `CompletionList { is_incomplete: true }`.

Important invariants:

- `isIncomplete=true` for success, empty results, and truncated results.
- Short-prefix behavior, truncation, prefix narrowing, evidence-aware ranking, history boost limits, raw text fallback labeling, guard band, shadow-rank metrics, and metadata-only perf logs remain unchanged.
- Open scope and ambiguity are soft-ranking signals, not hard filters that make candidates disappear.
- Ordinary completion must not add per-keystroke SQLite, full workspace scans, or unbounded parsing.

## Behavior Freeze For v1.2.2

v1.2.2 is a behavior-preserving architecture health release. It may add documentation, tests, fitness checks, and internal boundaries, but it must preserve:

- navigation order and candidate labels unless a later task adds explicit compatibility evidence;
- ordinary/include/member completion labels, order, detail, documentation, `sortText`, command attachment, and `isIncomplete`;
- reference grouping and role fallback;
- semantic coloring scope and fallback;
- hover/signature candidate behavior;
- configuration and conflict handling;
- privacy defaults and metadata-only logs;
- self-contained VSIX packaging.

When future code disagrees with this baseline, the code must either restore behavior or update the OpenSpec artifacts and release notes with an explicit accepted behavior change. Phase A does not accept such a behavior change.
