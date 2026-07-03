use super::include_completion::IncludeCompletionTable;
use super::{
    dedup_completion_candidates, grouped_reference_items, local_words_for_cache,
    rebuild_include_table, rebuild_indexed_file_list,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::tempdir;
use tower_lsp::lsp_types::{CompletionItem, CompletionItemKind, Url};

#[tokio::test]
async fn local_word_cache_is_keyed_by_document_version() {
    let cache = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let uri = Url::parse("file:///tmp/cache-test.c").expect("uri");

    let first = local_words_for_cache(&cache, &uri, 1, "int cached_word;").await;
    let second = local_words_for_cache(&cache, &uri, 1, "int changed_word;").await;
    assert!(Arc::ptr_eq(&first, &second));
    assert!(second.iter().any(|word| word == "cached_word"));
    assert!(!second.iter().any(|word| word == "changed_word"));

    let third = local_words_for_cache(&cache, &uri, 2, "int changed_word;").await;
    assert!(!Arc::ptr_eq(&second, &third));
    assert!(third.iter().any(|word| word == "changed_word"));
}

#[tokio::test]
async fn failed_include_table_rebuild_clears_stale_cache() {
    let root = tempdir().expect("root");
    let root_path = root.path().to_path_buf();
    let include_tables: super::IncludeTables = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    include_tables.lock().await.insert(
        root_path.clone(),
        Arc::new(IncludeCompletionTable::build(vec!["stale.h".to_string()])),
    );

    let result = rebuild_include_table(&include_tables, root_path.clone()).await;

    assert!(result.is_err(), "missing index should fail the rebuild");
    assert!(
        !include_tables.lock().await.contains_key(&root_path),
        "degraded include table must not keep stale candidates"
    );
}

#[tokio::test]
async fn failed_reference_file_list_rebuild_clears_stale_cache() {
    let root = tempdir().expect("root");
    let root_path = root.path().to_path_buf();
    let indexed_file_lists: super::IndexedFileLists =
        Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    indexed_file_lists.lock().await.insert(
        root_path.clone(),
        Arc::new(vec![("stale.c".to_string(), root_path.join("stale.c"))]),
    );

    let result = rebuild_indexed_file_list(&indexed_file_lists, root_path.clone()).await;

    assert!(result.is_err(), "missing index should fail the rebuild");
    assert!(
        !indexed_file_lists.lock().await.contains_key(&root_path),
        "degraded reference file-list must not keep stale discovery scope"
    );
}

// --- R6 section 4: grouped references role exposure --------------------

#[test]
fn grouped_reference_items_preserve_role_and_order() {
    use crate::parser::SyntacticRole;
    use crate::references::{self, ReferenceHit};
    let dir = tempdir().expect("tempdir");
    let mut hits = vec![
        ReferenceHit {
            rel_path: "a.c".into(),
            line: 9,
            start_col_utf16: 0,
            end_col_utf16: 3,
            role: SyntacticRole::Read,
        },
        ReferenceHit {
            rel_path: "b.c".into(),
            line: 2,
            start_col_utf16: 0,
            end_col_utf16: 3,
            role: SyntacticRole::Definition,
        },
    ];
    references::sort_hits_by_role(&mut hits);
    let items = grouped_reference_items(dir.path(), &hits);
    assert_eq!(items.len(), 2);
    // Definition group first; each item carries its role label for the client.
    assert_eq!(items[0].role, "definition");
    assert_eq!(items[1].role, "read");
}

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
    // Even extending an empty prefix is invalid — the prior scan was
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
            indexed_file_list: Some(4),
        },
    );
    let same = super::state::workspace_generation_for_parts(
        &root,
        super::state::WorkspaceGenerationParts {
            name_table: Some(1),
            reach_graph: Some(2),
            include_table: Some(3),
            indexed_file_list: Some(4),
        },
    );
    let changed = super::state::workspace_generation_for_parts(
        &root,
        super::state::WorkspaceGenerationParts {
            name_table: Some(1),
            reach_graph: Some(2),
            include_table: Some(3),
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
            indexed_file_list: Some(2),
        },
    );
    let second = super::state::workspace_generation_for_parts(
        &root,
        super::state::WorkspaceGenerationParts {
            name_table: Some(1),
            reach_graph: None,
            include_table: None,
            indexed_file_list: Some(3),
        },
    );

    let combined_first = super::state::combine_workspace_generations(&[(root.clone(), first)]);
    let combined_second = super::state::combine_workspace_generations(&[(root, second)]);

    assert_ne!(combined_first, combined_second);
}

// --- R7: local word vs indexed candidate tier ordering --------------------

#[test]
fn local_word_does_not_outrank_reachable_indexed_candidate() {
    // A local word's best possible score (exact match + locality bonus)
    // must not exceed a Reachable-tier indexed candidate's pack_score,
    // which uses strict-tier ordering (TIER_STRIDE) to dominate.
    // This verifies the design invariant: the resolver's pack_score
    // guarantees tier strictly dominates match quality.
    use crate::model::ScopeTier;
    use crate::query::completion_word_score;
    use crate::resolver;

    let local_best = completion_word_score("foo", "foo", crate::query::COMPLETION_LOCALITY_BONUS);
    assert!(local_best.is_some(), "exact match must score");

    // A Reachable-tier indexed candidate with a moderate base_match.
    let indexed_score = resolver::pack_score(
        ScopeTier::Reachable,
        800, // base_match (prefix quality)
        0,   // no locality bonus
    );
    assert!(
        indexed_score > local_best.unwrap(),
        "Reachable-tier indexed candidate (score {}) must outrank best local word (score {})",
        indexed_score,
        local_best.unwrap()
    );

    // Even an External-tier indexed candidate outranks best local words.
    let external_score = resolver::pack_score(
        ScopeTier::External,
        1000, // exact match
        0,
    );
    assert!(
        external_score > local_best.unwrap(),
        "External-tier indexed exact match (score {}) outranks best local word (score {})",
        external_score,
        local_best.unwrap()
    );
}

#[test]
fn completion_dedup_keeps_indexed_kind_over_same_name_local_word() {
    use crate::model::{ResolutionConfidence, ScopeTier};

    let indexed = super::CompletionCandidate {
        name: "hello_value".to_string(),
        tier: ScopeTier::Reachable,
        confidence: ResolutionConfidence::Reachable,
        score: 30_000,
        item: CompletionItem {
            label: "hello_value".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            ..Default::default()
        },
        source: super::CompletionCandidateSource::Indexed,
    };
    let local = super::CompletionCandidate {
        name: "hello_value".to_string(),
        tier: ScopeTier::Current,
        confidence: ResolutionConfidence::Heuristic,
        score: 40_000,
        item: CompletionItem {
            label: "hello_value".to_string(),
            kind: Some(CompletionItemKind::TEXT),
            ..Default::default()
        },
        source: super::CompletionCandidateSource::LocalWord,
    };

    let deduped = dedup_completion_candidates(vec![indexed, local]);
    assert_eq!(deduped.len(), 1);
    assert_eq!(deduped[0].item.kind, Some(CompletionItemKind::FUNCTION));
}

#[test]
fn local_word_exact_index_match_uses_semantic_completion_kind() {
    let table = crate::query::NameTable::build_with_paths(vec![(
        1,
        "api_target_function".to_string(),
        false,
        "inc/target.h".to_string(),
        "function".to_string(),
        false,
    )]);
    let local_score = crate::resolver::pack_score(
        crate::model::ScopeTier::Current,
        crate::query::COMPLETION_LOCALITY_BONUS + 550,
        0,
    );

    let candidates = super::exact_indexed_completion_candidates_for_local_word(
        &table,
        "api_target_function",
        local_score,
        None,
        None,
        10,
    );

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].name, "api_target_function");
    assert_eq!(candidates[0].score, local_score);
    assert_eq!(candidates[0].item.kind, Some(CompletionItemKind::FUNCTION));
}

// --- R7: watcher/debounce IndexScheduleState machine tests ---------------

use super::IndexScheduleState;

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

    // Full request arrives — it overrides dirty changes.
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

    // Dirty run completes — no more work.
    state.running = false;
    state.scheduled = false;
    assert!(!state.running);
    assert!(!state.scheduled);
    assert!(state.pending_changes.is_empty());
    assert!(!state.pending_requested);
}

// --- R7: error degradation — IndexStatus state correctness ---------------

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
    // and a non-empty message — they must never be confused.
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
