# Module Import Inventory

Generated: 2026-07-06 by Phase A manual inventory commands using `rg --files` and import/use scans. This is a baseline for later architecture fitness functions, not a proof of every transitive dependency.

Purpose:

- Provide the current Rust module import inventory.
- Provide the current VS Code extension import inventory.
- Identify likely Phase B fitness allowlist candidates before refactoring.
- Avoid unreviewed large-scale churn during v1.2.2.

## Rust module import inventory

| File | Lines | Internal crate deps | Notable external deps |
| --- | ---: | --- | --- |
| crates/fossilsense/src/coloring.rs | 932 | model, parser, query, reachability, store |  |
| crates/fossilsense/src/completion_history.rs | 362 |  | serde, serde_json, anyhow |
| crates/fossilsense/src/completion_words.rs | 284 |  |  |
| crates/fossilsense/src/completion.rs | 1583 | completion_history, model, query |  |
| crates/fossilsense/src/config.rs | 530 |  | serde, serde_json, directories |
| crates/fossilsense/src/includes.rs | 570 |  | directories |
| crates/fossilsense/src/indexer.rs | 283 | config, pathing, progress, store | anyhow, directories |
| crates/fossilsense/src/indexer/candidates.rs | 210 | config, pathing, store | rayon, ignore, anyhow, directories |
| crates/fossilsense/src/indexer/include_edges.rs | 163 | includes, pathing, store | anyhow |
| crates/fossilsense/src/indexer/parse_pipeline.rs | 144 | parser, progress, store | rayon, anyhow |
| crates/fossilsense/src/indexer/progress_limiter.rs | 58 | progress |  |
| crates/fossilsense/src/main.rs | 288 | store | tokio, anyhow, clap |
| crates/fossilsense/src/model.rs | 485 | parser, reachability, references, resolver |  |
| crates/fossilsense/src/parser.rs | 538 | config | rayon, tree_sitter, tree_sitter_c, tree_sitter_cpp |
| crates/fossilsense/src/parser/ast.rs | 750 |  | tree_sitter |
| crates/fossilsense/src/parser/lexical.rs | 406 |  | regex |
| crates/fossilsense/src/pathing.rs | 64 |  | anyhow, directories |
| crates/fossilsense/src/progress.rs | 172 |  | serde |
| crates/fossilsense/src/query.rs | 821 | model, parser, reachability, resolver |  |
| crates/fossilsense/src/query/current_file_overlay.rs | 493 | parser |  |
| crates/fossilsense/src/query/definitions.rs | 554 | model, reachability, resolver, store |  |
| crates/fossilsense/src/query/hover.rs | 706 | model, reachability, store |  |
| crates/fossilsense/src/query/local_completion.rs | 203 | model, parser, resolver |  |
| crates/fossilsense/src/query/lsp_kinds.rs | 74 | parser | tower_lsp |
| crates/fossilsense/src/query/signatures.rs | 676 | model, reachability, store |  |
| crates/fossilsense/src/query/text.rs | 286 |  |  |
| crates/fossilsense/src/reachability.rs | 549 | reachability |  |
| crates/fossilsense/src/references.rs | 521 | config, model, parser, pathing | rayon, grep-matcher, grep-regex, grep-searcher, anyhow |
| crates/fossilsense/src/resolver.rs | 800 | model, reachability |  |
| crates/fossilsense/src/scanner.rs | 108 | config, pathing | ignore, anyhow |
| crates/fossilsense/src/server.rs | 837 | completion, completion_history, completion_words, config, includes, model, parser, pathing, query, reachability, references, resolver, store | tower_lsp, tokio, serde, serde_json, anyhow, directories |
| crates/fossilsense/src/server/hover.rs | 205 | model, pathing, query, resolver, store | tower_lsp, tokio, anyhow |
| crates/fossilsense/src/server/include_completion.rs | 1239 | config, includes, indexer, pathing, store | tower_lsp, anyhow |
| crates/fossilsense/src/server/indexing.rs | 682 | indexer, pathing, progress, query, reachability, store | tower_lsp, tokio, anyhow |
| crates/fossilsense/src/server/indexing/cache.rs | 278 | pathing, server | tokio |
| crates/fossilsense/src/server/indexing/watch.rs | 55 | config, server | tokio |
| crates/fossilsense/src/server/language_server.rs | 1006 | completion, completion_history, model, resolver | tower_lsp, tokio, serde, serde_json |
| crates/fossilsense/src/server/lsp_adapters.rs | 132 | model, parser, query, references, store | tower_lsp, serde |
| crates/fossilsense/src/server/member_completion.rs | 565 | model, parser, pathing, query, resolver, store | tower_lsp, tokio, anyhow |
| crates/fossilsense/src/server/options.rs | 448 | completion_history, model, resolver | tower_lsp, serde, serde_json, directories |
| crates/fossilsense/src/server/semantic_tokens.rs | 115 | coloring, parser, query | tower_lsp, tokio, anyhow |
| crates/fossilsense/src/server/signature_help.rs | 254 | model, pathing, query, resolver, store | tower_lsp, tokio, anyhow |
| crates/fossilsense/src/server/state.rs | 58 |  |  |
| crates/fossilsense/src/store.rs | 516 | includes, parser | rusqlite, anyhow |
| crates/fossilsense/src/store/includes.rs | 375 | includes, reachability | rusqlite, anyhow |
| crates/fossilsense/src/store/queries.rs | 681 | model, parser, resolver | rusqlite, anyhow |
| crates/fossilsense/src/store/schema.rs | 147 |  | rusqlite |
| crates/fossilsense/src/store/writes.rs | 237 | parser | rusqlite, anyhow |

## VS Code extension import inventory

| File | Lines | Imports |
| --- | ---: | --- |
| extensions/vscode/src/completionHistory.ts | 27 | `./config` |
| extensions/vscode/src/config.ts | 8 |  |
| extensions/vscode/src/conflicts.ts | 7 |  |
| extensions/vscode/src/extension.ts | 463 | `./completionHistory`, `./config`, `./conflicts`, `./referencesView`, `./serverPath`, `./status`, `fs`, `vscode`, `vscode-languageclient/node` |
| extensions/vscode/src/referencesView.ts | 45 |  |
| extensions/vscode/src/serverPath.ts | 26 | `path` |
| extensions/vscode/src/status.ts | 28 |  |

## Phase B fitness allowlist candidates

Phase B fitness allowlist entries should be explicit, temporary, and include rule, path, and reason. Initial candidates from this inventory:

| Rule area | Path | Reason to review |
| --- | --- | --- |
| LSP boundary | `crates/fossilsense/src/query/lsp_kinds.rs` | Query currently contains a small LSP-kind adapter. Either allowlist temporarily or move under `server/lsp_adapters.rs` during a later behavior-preserving step. |
| Large source warning | `crates/fossilsense/src/completion.rs` | Current ordinary completion pipeline is large and hot; Phase D should extract a protocol-neutral service carefully. |
| Large source warning | `crates/fossilsense/src/server/include_completion.rs` | Include completion is large but out of required Phase D extraction scope. |
| Large source warning | `crates/fossilsense/src/server/language_server.rs` | Main LSP handler remains large until WorkspaceSession and completion boundary work lands. |
| Large source warning | `crates/fossilsense/src/coloring.rs` | Coloring is sizeable but not a target for v1.2.2 movement unless tests require it. |

## Boundary notes

- `tower_lsp` is expected in `server.rs`, `server/*`, and the current transitional `query/lsp_kinds.rs`.
- `rusqlite` is expected in `store.rs` and `store/*`.
- Parser depends on config and tree-sitter crates; it should not depend on store or server.
- Resolver depends on model/reachability; it should not depend on parser, indexer, store, or server details.
- Store depends on parser data shapes and model/query concepts where needed for row mapping; it should not depend on server handlers.
