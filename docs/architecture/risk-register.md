# v1.2.2 Architecture Risk Register

Status: current for Phase A.

| Risk | User impact | Likelihood | Mitigation | Owner / follow-up area |
| --- | --- | --- | --- | --- |
| ordinary completion extraction changes ordering | Users see different first choices, `sortText`, or detail/documentation for the same prefix. | High | Add compatibility fixtures before extraction; compare labels, ordering, fallback detail, documentation, command attachment, metrics counts, and `isIncomplete=true`. | Phase D ordinary completion boundary |
| ordinary completion fallback drift | Raw text fallback, local bindings, current-file overlay, reachable/external/global tiers, ambiguity, or open scope are mislabeled. | Medium | Preserve `run_evidence_aware_pipeline_with_context`; test confidence/fallback/ambiguity/open scope labels. | Phase D ordinary completion boundary |
| ordinary completion history boost drift | Local completion history outranks high-confidence current/local evidence or records raw labels. | Medium | Keep capped boost tests and privacy checks; verify history boost is bounded and metadata-only. | Phase D and release hardening |
| ordinary completion metrics drift | Perf logs expose candidate names, snippets, or accepted raw text. | Medium | Regression checklist requires metadata-only metrics fields. | Phase D and Phase H privacy |
| ordinary completion hot-path performance regression | Completion starts doing per-keystroke SQLite, workspace scans, or unbounded parsing. | Medium | Inventory and fitness checks track SQLite boundaries; compatibility tests inspect hot-path assumptions. | Phase D and Phase B fitness |
| workspace cache invalidation misses dirty changes | Stale read models produce outdated definitions, completion, references, coloring, hover, or signature help. | High | `CacheLedger` tests for full index, dirty index, generation updates, completion memo, reference cache, and indexed file lists. | Phase C CacheLedger |
| stale read models after failed rebuild | Failed cache rebuild leaves old `NameTable`, `ReachGraph`, or `IncludeCompletionTable` presented as current. | Medium | Existing tests already cover some stale cache clearing; expand under CacheLedger. | Phase C CacheLedger |
| completion memo invalidation mistakes | Prefix reuse returns stale candidates after generation or prefix changes. | Medium | Preserve `WorkspaceGeneration` and `completion_memo_is_valid` tests. | Phase C CacheLedger |
| reference cache invalidation mistakes | References return old role classifications or file lists after edits/indexing. | Medium | Add reference-cache clearing tests for document and index changes. | Phase C CacheLedger |
| lock discipline regression | LSP requests hold document/cache locks across blocking work, causing stale snapshots or deadlocks. | Medium | `WorkspaceSnapshot` must clone `Arc` data before expensive work; add lock-discipline review and tests. | Phase C WorkspaceSession |
| documentation drift | Architecture docs, README, extension README, or `CLAUDE.md` stop matching implementation. | Medium | Phase H documentation sync and Phase A verification script. | Phase H docs |
| scope creep into optional phases | v1.2.2 starts parser fact split, IndexStore facade, or include policy consolidation without guardrails. | Medium | Keep Phase E/F/G optional; use OpenSpec tasks as required-scope boundary. | Final review |
| VSIX packaging regression | Release lacks a self-contained binary or expected `dist/fossilsense-vscode-1.2.2_BUILD*.vsix`. | Medium | Run `pnpm run package`; inspect VSIX for `bin/fossilsense.exe`; document artifact in release notes. | Phase H packaging |
