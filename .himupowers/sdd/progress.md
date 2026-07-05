# Smart Completion v1.2.1 SDD Progress

Feature brief: `smart-completion-v1-2-1`
Requirements: `docs/smart-completion-v1-2-1/requirements.md`
Plan: `docs/smart-completion-v1-2-1/plans/2026-07-05--implementation-plan.md`
Base SHA: `54ac864`
Workspace: current checkout on `release/v1.2.1`, approved by user on 2026-07-05.
Execution mode: smart subagent dispatch, with main agent owning core linear implementation unless a task has clear parallel, review, or mechanical-verification value.

## Baseline

- 2026-07-05: `cargo test -p fossilsense` passed: 451 unit tests and 2 LSP smoke tests.
- 2026-07-05: `pnpm run compile` in `extensions/vscode` passed.

## Dispatch Notes

- Task 1-7 share core interfaces across parser, store, server, completion, and extension command plumbing. Main agent should execute them serially with TDD to avoid cross-agent contract drift.
- Subagents are best reserved for independent review after core member/history implementation, or for mechanical verification/log collection during Task 9.

## Tasks

- Task 1: completed by main agent. RED: `pnpm run compile` failed with missing `../completionHistory` module. GREEN: `pnpm run compile`, `node out/test/completionHistory.test.js`, `pnpm run test`, and `cargo check -p fossilsense` passed. Added v1.2.1 version facts, extension completion-history setting, clear-history command, pure helper module, and extension tests.
- Task 2: completed by main agent. RED: `cargo test -p fossilsense store::tests::resilience_schema::current_schema_has_members_table_and_version_9_or_newer -- --nocapture` and `cargo test -p fossilsense store::tests::members::struct_fields_are_persisted_as_field_members -- --nocapture` failed with missing `members_for_records`/members schema. GREEN: `cargo test -p fossilsense store::tests::resilience_schema -- --nocapture`, `cargo test -p fossilsense store::tests::members -- --nocapture`, `cargo test -p fossilsense store::tests -- --nocapture`, and `cargo test -p fossilsense parser::tests -- --nocapture` passed. Added schema v9, `members` table, parser/model member evidence types, field-as-member persistence, and field compatibility wrappers.
- Task 3: completed by main agent. RED: parser method tests failed because class-body methods were not `MemberKind::Method` and simple `Owner::method` facts were absent; store persistence test failed before owner-name mapping. GREEN: `cargo test -p fossilsense parser::tests -- --nocapture` and `cargo test -p fossilsense store::tests::members -- --nocapture` passed. Added in-body method/static-method extraction, simple out-of-class owner evidence, declaration-text method signatures, and same-file unique-owner persistence.
- Task 4: completed by main agent. RED: store/query tests failed with missing `fallback_member_candidates` and `normalized_receiver_record_hint`. GREEN: `cargo test -p fossilsense store::tests::members -- --nocapture` and `cargo test -p fossilsense query::tests -- --nocapture` passed cleanly. Added owner-tiered `members_for_records`, prefix-only capped `fallback_member_candidates`, member non-leakage coverage for ordinary NameTable, and a conservative receiver hint normalizer.
- Task 5: completed by main agent. RED: server member test failed while completion rendered field-only, fallback gate test and weak receiver helper test established guard rails. GREEN: `cargo test -p fossilsense server::tests -- --nocapture`, `cargo test -p fossilsense store::tests::members -- --nocapture`, and `cargo test -p fossilsense query::tests -- --nocapture` passed cleanly. Switched member completion rendering to `MemberCandidate`, added method/static-method LSP kinds, conservative weak receiver correlation, prefix-only fallback member rendering, and source-safe member completion perf counts.
- Task 6: completed by main agent. RED: `cargo test -p fossilsense completion_history -- --nocapture` and `cargo test -p fossilsense server::tests::execute_command_records_completion_accept_when_history_enabled -- --nocapture` failed with missing history store/types, command constants, and Backend state. GREEN: `cargo test -p fossilsense completion_history -- --nocapture`, `cargo test -p fossilsense server::tests -- --nocapture`, `pnpm run compile`, and `node out/test/completionHistory.test.js` passed. Added local-only bounded JSON accept history, workspace-local history path, mode parsing, LSP accept/clear commands, source-safe logging, and server command tests for enabled/disabled history.
- Task 7: not started.
- Task 8: not started.
- Task 9: not started.
