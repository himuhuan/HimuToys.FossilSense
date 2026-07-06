## 1. Baseline and Parity Tests

- [x] 1.1 Review `plan-healthy-v122-architecture-refactor` outputs and confirm this follow-up starts from the A-D/H boundaries already implemented.
  - Evidence: `openspec/changes/plan-healthy-v122-architecture-refactor/tasks.md` has Phases A, B, C, D, H, and final review complete, with E/F/G deferred as follow-up scope.
- [x] 1.2 Add or extend store tests that capture current `IndexStore::open_readonly` behavior, including missing, malformed, empty, and WAL-backed index cases.
- [x] 1.3 Add parity tests for current `NameTable` rebuild inputs, including source, path, kind, and directly-included evidence.
- [x] 1.4 Add parity tests for current reach graph inputs, including resolved edges, unresolved includes, ambiguous includes, and incremental source refresh.
- [x] 1.5 Add parity tests for current include completion table inputs, including workspace paths and include edge ordering.
- [x] 1.6 Add parity tests for current record/member queries, including alias recursion, same-tier deduplication, member prefix filtering, method/field ordering, and fallback caps.
- [x] 1.7 Add parser tests that document current `ParseFacts::INDEX`, `ParseFacts::COLOR_REF`, `ParseFacts::MEMBER`, and `ParseFacts::ALL` field contents.
- [x] 1.8 Add parser fallback/skipped-facts tests that demonstrate the current ambiguity between empty, skipped, and fallback AST fact vectors.

## 2. Phase E - Store Read Views and Typed Rows

- [x] 2.1 Choose the low-churn module layout for store read views, such as `store::views` or focused submodules under `store::queries`.
- [x] 2.2 Define typed row or DTO structs for name-table symbol rows.
- [x] 2.3 Define typed row or DTO structs for include edge, unresolved include, ambiguous include, and include-completion path rows.
- [x] 2.4 Define typed row or DTO structs for definition, symbol lookup, record, member, and reference-file read use cases where tuple-shaped leakage currently crosses modules.
- [x] 2.5 Add a name-table read view and route `load_symbol_names_with_paths` / path-scoped symbol-name loading through it without changing returned data.
- [x] 2.6 Add a reach-graph read view and route include-edge/open-include loading through it without changing graph rebuild behavior.
- [x] 2.7 Add an include-table read view and route workspace-path/include-edge loading through it without changing include completion table behavior.
- [x] 2.8 Add definition/symbol/reference read views for exact-name lookup, symbol-id lookup, and indexed file/reference file loading where applicable.
- [x] 2.9 Add a member read view for `resolve_record_candidates`, `members_for_records`, and `fallback_member_candidates` while preserving existing domain candidate ordering.
- [x] 2.10 Keep existing `IndexStore` methods as compatibility wrappers around the new read views until all related call sites are migrated and tested.
- [x] 2.11 Update store and architecture tests so `rusqlite` usage remains isolated to store/persistence modules after adding read views.

## 3. Phase E - Read-Model and Feature Migration

- [x] 3.1 Migrate `NameTable` full rebuild and incremental path update code to consume typed name-table rows.
- [x] 3.2 Migrate `ReachGraph` full rebuild and incremental refresh code to consume typed include edge and open-include rows.
- [x] 3.3 Migrate `IncludeCompletionTable` rebuild code to consume typed path and include edge rows.
- [x] 3.4 Migrate indexed-file-list and reference-file loading code to the relevant read view without changing cache generation behavior.
- [x] 3.5 Migrate lower-risk feature query call sites such as definition, hover, signature, and symbol-id lookup to read views.
- [x] 3.6 Migrate member completion query call sites to the member read view after member parity tests are in place.
- [x] 3.7 Remove compatibility wrappers only for use cases whose call sites have fully migrated and whose parity tests cover the old behavior.
  - No compatibility wrappers were removed in Group 3; they remain as test/compatibility oracles while parity coverage and non-production callers still exercise them.
- [x] 3.8 Run the focused store/read-model tests after each migrated use case and fix any behavior drift before continuing.

## 4. Phase F - Parser Fact Projections

- [x] 4.1 Define parser projection types for persistent facts and request-time facts while keeping `FileSemanticIndex` fields intact.
- [x] 4.2 Define `FactAvailability` and fallback/skipped reason types with `NotRequested`, `Available`, and unavailable-with-reason states.
- [x] 4.3 Add projection methods on `FileSemanticIndex` or adjacent parser helpers for persistent facts, request facts, and per-group availability.
- [x] 4.4 Extend `ParseDiagnostics` or associated metadata so callers can distinguish skipped facts from lexical fallback without changing existing diagnostic meanings.
- [x] 4.5 Preserve `parse(path, source)`, `parse_with_handle`, and `parse_thread_local_with_facts` entry-point behavior while wiring availability metadata.
- [x] 4.6 Update parser tests so `ParseFacts::INDEX` still skips occurrences, local declarations, and local bindings while reporting them as not requested.
- [x] 4.7 Update parser tests so requested-but-fallback AST groups report unavailable with a fallback reason and available empty groups remain distinguishable.

## 5. Phase F - Parser Consumer Migration

- [x] 5.1 Migrate indexer parse pipeline storage code to consume the persistent facts projection.
- [x] 5.2 Migrate semantic coloring paths that depend on occurrences or coloring definitions to use request facts or explicit availability where useful.
- [x] 5.3 Migrate reference role classification paths to use request facts or explicit availability where useful.
- [x] 5.4 Migrate member completion receiver inference paths to use request facts or explicit availability where useful.
- [x] 5.5 Migrate local completion/current-document evidence paths to use request facts or explicit availability where useful.
- [x] 5.6 Keep unmigrated parser consumers behavior-compatible through `FileSemanticIndex` field access until they have focused tests.
  - Remaining direct field access is intentionally limited to parser internals, parser fact-shape tests, and compatibility/fallback tests that mutate or compare legacy fields.
- [x] 5.7 Remove projection compatibility shims only when all major parser consumers have migrated or the remaining field access is intentionally documented.
  - No projection compatibility shims were removed in Group 5; `FileSemanticIndex` fields remain the compatibility shape while the new consumer guard prevents migrated production paths from regressing.

## 6. Verification and Documentation

- [x] 6.1 Run `cargo fmt` and fix formatting issues.
  - Evidence: `cargo fmt` completed successfully.
- [x] 6.2 Run focused store tests and parser tests for the new read-view and fact-availability contracts.
  - Evidence: focused `cargo test -p fossilsense read_views`, `read_model_parity`, `parser_consumer_migration`, `parse_fact_masks_document_current_field_contents`, `records_only_mask_keeps_member_facts_not_requested`, and `availability_distinguishes_empty_skipped_and_fallback_ast_vectors` all passed.
- [x] 6.3 Run `cargo test -p fossilsense` and resolve any failures.
  - Evidence: `cargo test -p fossilsense` passed, including 521 unit tests and 2 LSP smoke tests.
- [x] 6.4 Run focused CLI smoke for `samples/mini-c`, including scan and forced index with a target sqlite database.
  - Evidence: `cargo run -p fossilsense -- scan samples/mini-c` found 2 files; `cargo run -p fossilsense -- index samples/mini-c --db target/mini.sqlite --force` indexed 2 files and 13 symbols.
- [x] 6.5 Run architecture fitness checks and resolve or document any remaining transitional allowlist entries.
  - Evidence: `scripts/verify_architecture_fitness.ps1` passed with `fail=0`, `warn=11`, `allowlisted=1`. The remaining allowlist is the pre-existing `query/lsp_kinds.rs` LSP-kind adapter; large-file warnings are documented in `docs/architecture/regression-checklist.md`.
- [x] 6.6 Review completion, definition, coloring, reference, include, and member behavior against the regression checklist to confirm no intentional user-visible behavior change slipped in.
  - Evidence: review captured in `docs/architecture/regression-checklist.md`; focused and full tests cover completion, definition, coloring, references, include, and member behavior.
- [x] 6.7 Update architecture docs and ADRs for the final store read-view and parser fact projection contracts.
  - Evidence: added current architecture docs under `docs/architecture/`, including read-model, parser-facts, regression checklist, and ADR notes.
- [x] 6.8 Update `CLAUDE.md` if canonical module responsibilities, parser contracts, or store contracts changed.
  - Evidence: updated `CLAUDE.md` module, store read-view, parser facts, member durable-read, and documentation-rule contracts.
- [x] 6.9 Re-run `openspec status --change plan-healthy-v122-store-parser-facts` and confirm all required artifacts remain valid.
  - Evidence: `openspec status --change "plan-healthy-v122-store-parser-facts" --json` returned `isComplete=true`; proposal, design, specs, and tasks artifacts are `done`.
