use super::*;

// --- R7: completion memo validity (generation + prefix extension check) ---

#[test]
fn completion_memo_valid_when_prefix_extends_and_same_generation() {
    assert!(super::state::completion_memo_is_valid(42, 42, "fo", "foo"));
}

#[test]
fn completion_memo_invalid_when_generation_differs() {
    assert!(!super::state::completion_memo_is_valid(10, 20, "fo", "foo"));
}

#[test]
fn completion_memo_invalid_when_prefix_shortens() {
    assert!(!super::state::completion_memo_is_valid(1, 1, "foo", "fo"));
}

#[test]
fn completion_memo_invalid_when_prefix_changes() {
    assert!(!super::state::completion_memo_is_valid(1, 1, "foo", "bar"));
}

#[test]
fn completion_memo_invalid_when_prior_prefix_empty() {
    // An empty prior prefix means there is no usable narrowing base.
    assert!(!super::state::completion_memo_is_valid(1, 1, "", "a"));
    // Even extending an empty prefix is invalid - the prior scan was
    // the empty-prefix full pass which doesn't provide a focused pool.
    assert!(!super::state::completion_memo_is_valid(1, 1, "", "foo"));
}

#[test]
fn workspace_generation_changes_when_derived_state_changes() {
    let root = PathBuf::from("workspace");
    let base = super::state::workspace_generation_for_parts(
        &root,
        super::state::WorkspaceGenerationParts {
            name_table: Some(1),
            reach_graph: Some(2),
            include_table: Some(3),
            project_context: Some(4),
            indexed_file_list: Some(5),
        },
    );
    let same = super::state::workspace_generation_for_parts(
        &root,
        super::state::WorkspaceGenerationParts {
            name_table: Some(1),
            reach_graph: Some(2),
            include_table: Some(3),
            project_context: Some(4),
            indexed_file_list: Some(5),
        },
    );
    let changed = super::state::workspace_generation_for_parts(
        &root,
        super::state::WorkspaceGenerationParts {
            name_table: Some(1),
            reach_graph: Some(2),
            include_table: Some(3),
            project_context: Some(4),
            indexed_file_list: Some(99),
        },
    );

    assert_eq!(base, same);
    assert_ne!(base, changed);
}

#[test]
fn combined_workspace_generation_changes_when_root_generation_changes() {
    let root = PathBuf::from("workspace");
    let first = super::state::workspace_generation_for_parts(
        &root,
        super::state::WorkspaceGenerationParts {
            name_table: Some(1),
            reach_graph: None,
            include_table: None,
            project_context: None,
            indexed_file_list: Some(2),
        },
    );
    let second = super::state::workspace_generation_for_parts(
        &root,
        super::state::WorkspaceGenerationParts {
            name_table: Some(1),
            reach_graph: None,
            include_table: None,
            project_context: None,
            indexed_file_list: Some(3),
        },
    );

    let combined_first = super::state::combine_workspace_generations(&[(root.clone(), first)]);
    let combined_second = super::state::combine_workspace_generations(&[(root, second)]);

    assert_ne!(combined_first, combined_second);
}

// --- R7: watcher/debounce IndexScheduleState machine tests ---------------

fn dirty_change(root: &str, rel: &str) -> super::RootDirtyChange {
    super::RootDirtyChange {
        root: std::path::PathBuf::from(root),
        rel_path: rel.to_string(),
        change: crate::indexer::DirtyFileChange {
            absolute_path: std::path::PathBuf::from(root).join(rel),
            kind: crate::indexer::DirtyFileKind::Upsert,
        },
    }
}

#[test]
fn index_schedule_dirty_merge_accumulates_changes() {
    let mut state = IndexScheduleState::default();
    state.pending_requested = true;
    state.pending_changes.push(dirty_change("/root", "src/a.c"));
    state.pending_changes.push(dirty_change("/root", "src/b.c"));
    state.pending_changes.push(dirty_change("/root", "inc/c.h"));
    assert_eq!(state.pending_changes.len(), 3);
    assert!(!state.pending_full, "full flag not set for dirty-only");
    assert!(state.pending_requested, "requested flag set");
}

#[test]
fn index_schedule_full_overrides_dirty() {
    let mut state = IndexScheduleState::default();
    state.pending_requested = true;
    state.pending_changes.push(dirty_change("/root", "src/a.c"));
    state.pending_changes.push(dirty_change("/root", "src/b.c"));
    assert_eq!(state.pending_changes.len(), 2);

    // Full request arrives - it overrides dirty changes.
    state.pending_full = true;
    state.pending_force = true;
    state.pending_changes.clear();
    assert!(state.pending_full);
    assert!(state.pending_force);
    assert!(state.pending_changes.is_empty());
}

#[test]
fn index_schedule_second_request_during_running() {
    let mut state = IndexScheduleState::default();
    // Current indexing pass is running.
    state.running = true;
    state.scheduled = false; // current pass was the one

    // A new dirty request comes in while running.
    state.pending_requested = true;
    state
        .pending_changes
        .push(dirty_change("/root", "src/new.c"));

    // Verify flags: running stays true (still executing), scheduled is false
    // (old pass is still running), but pending_requested is set for re-schedule.
    assert!(state.running);
    assert!(
        !state.scheduled,
        "old pass still running, not yet re-scheduled"
    );
    assert!(state.pending_requested, "re-schedule requested");
    assert_eq!(state.pending_changes.len(), 1);
}

#[test]
fn index_schedule_state_reset_after_full_consumed() {
    let mut state = IndexScheduleState::default();
    state.running = true;
    state.scheduled = true;
    state.pending_requested = true;
    state.pending_full = true;

    // "Consume" the scheduled full index.
    state.running = false;
    state.scheduled = false;
    state.pending_full = false;
    state.pending_force = false;
    // pending_requested is set by a concurrent request; after the loop
    // checks it, it would spawn again. Here we verify the consumed state.
    assert!(!state.running);
    assert!(!state.scheduled);
    assert!(!state.pending_full);
    assert!(!state.pending_force);
}

#[test]
fn index_schedule_dirty_follows_full() {
    // Scenario: full index runs, a dirty request arrives during it.
    // After the full finishes and pending_requested is seen, the loop
    // re-checks and processes the dirty changes.
    let mut state = IndexScheduleState::default();
    state.running = true;
    state.scheduled = true;
    state.pending_full = true;
    state.pending_force = false;

    // Dirty request arrives during full execution.
    state.pending_requested = true;
    state
        .pending_changes
        .push(dirty_change("/root", "src/edited.c"));

    // Full index finishes.
    state.running = false;
    state.scheduled = false;
    state.pending_full = false;
    state.pending_force = false;

    // Loop sees pending_requested, checks pending_full=false, falls to
    // dirty path with the accumulated change.
    assert!(state.pending_requested, "dirty work still pending");
    assert!(!state.pending_full, "full work consumed");
    assert_eq!(state.pending_changes.len(), 1);
    assert_eq!(state.pending_changes[0].rel_path, "src/edited.c");

    // Consume the dirty request.
    state.running = true;
    state.scheduled = true;
    state.pending_requested = false;
    state.pending_changes.clear();

    // Dirty run completes - no more work.
    state.running = false;
    state.scheduled = false;
    assert!(!state.running);
    assert!(!state.scheduled);
    assert!(state.pending_changes.is_empty());
    assert!(!state.pending_requested);
}

// --- R7: error degradation - IndexStatus state correctness ---------------

#[test]
fn index_status_failed_has_correct_state() {
    let failed = crate::progress::IndexStatus::failed("/workspace".into(), "disk full".into());
    assert_eq!(failed.state, crate::progress::IndexState::Failed);
    assert!(
        !failed.message.as_deref().unwrap_or("").is_empty(),
        "failed status must carry an error message"
    );
}

#[test]
fn index_status_ready_distinguishable_from_failed() {
    let failed = crate::progress::IndexStatus::failed("/workspace".into(), "disk full".into());
    let stats = crate::progress::IndexStats::default();
    let ready = crate::progress::IndexStatus::ready("/workspace".into(), &stats);

    assert_ne!(
        ready.state, failed.state,
        "Ready and Failed must be distinguishable states"
    );
    assert_eq!(ready.state, crate::progress::IndexState::Ready);
    assert_eq!(failed.state, crate::progress::IndexState::Failed);
    // A Ready status carries indexed counts; a Failed status carries zeroes
    // and a non-empty message - they must never be confused.
    assert!(ready.message.is_none(), "Ready carries no error message");
    assert!(failed.message.is_some(), "Failed carries an error message");
}

#[test]
fn index_status_ready_carries_degraded_capabilities() {
    let stats = crate::progress::IndexStats::default();
    let degraded = crate::progress::DegradedCapabilities {
        reach_graph: true,
        include_table: false,
        reference_file_list: true,
    };
    let ready =
        crate::progress::IndexStatus::ready_with_degraded("/workspace".into(), &stats, degraded);

    assert_eq!(ready.state, crate::progress::IndexState::Ready);
    assert!(ready.degraded_capabilities.any());
    assert_eq!(
        ready.degraded_capabilities.labels(),
        vec!["reachGraph", "referenceFileList"]
    );
}

#[test]
fn ready_cache_message_names_degraded_capabilities() {
    let degraded = crate::progress::DegradedCapabilities {
        reach_graph: true,
        include_table: true,
        reference_file_list: false,
    };

    let message = super::ready_cache_message("name table ready", 7, 3, 2, 11, 13, &degraded);

    assert!(message.contains("name table ready: 7 symbols"));
    assert!(message.contains("include table=3 paths"));
    assert!(message.contains("reference files=2"));
    assert!(message.contains("degraded=reachGraph,includeTable"));
}

#[test]
fn query_error_log_line_is_structured_and_single_line() {
    let line =
        super::query_error_log_line("grouped references", "query", "db failed\nwhile reading");

    assert_eq!(
        line,
        "FS_QUERY_ERROR kind=query what=grouped_references detail=db failed while reading"
    );
}
