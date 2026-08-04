use super::*;
use crate::reachability::ReachScope;
use crate::resource::current_process_memory_bytes;
use crate::semantic_model::SemanticFamily;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
#[ignore = "diagnostic large-workspace NameTable benchmark; set FOSSILSENSE_BENCH_DB"]
fn benchmark_large_name_table_build_and_dirty_update() {
    let db = std::env::var_os("FOSSILSENSE_BENCH_DB")
        .map(std::path::PathBuf::from)
        .expect("set FOSSILSENSE_BENCH_DB to a schema-15 benchmark database");
    let store = crate::store::IndexStore::open_readonly(&db).expect("benchmark database");

    let build_started = std::time::Instant::now();
    let mut builder = name_index_builder::NameIndexBuilder::new(None);
    let visit_started = std::time::Instant::now();
    store
        .declaration_view()
        .visit_name_rows(|row| {
            builder.push_declaration(row);
            Ok(())
        })
        .expect("stream name rows into builder");
    let sql_visit_ms = visit_started.elapsed().as_millis();
    let finalize_started = std::time::Instant::now();
    let mut table = builder.finish();
    let finalize_ms = finalize_started.elapsed().as_millis();
    let stream_build_ms = build_started.elapsed().as_millis();
    let expected_len = table.len();

    let changed_path = store
        .declaration_view()
        .largest_declaration_path()
        .expect("largest symbol path")
        .map(|(path, _)| path)
        .expect("at least one symbol row");
    let fresh_rows = store
        .declaration_view()
        .name_rows_for_paths(std::slice::from_ref(&changed_path))
        .expect("load changed path rows");

    let paths = std::collections::HashSet::from([changed_path]);
    let mut dirty_us = Vec::new();
    let private_before = current_process_memory_bytes();
    let peak_private = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(private_before));
    let stop_sampling = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sampler = {
        let peak_private = peak_private.clone();
        let stop_sampling = stop_sampling.clone();
        std::thread::spawn(move || {
            while !stop_sampling.load(std::sync::atomic::Ordering::Relaxed) {
                peak_private.fetch_max(
                    current_process_memory_bytes(),
                    std::sync::atomic::Ordering::Relaxed,
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        })
    };
    for _ in 0..5 {
        let update_started = std::time::Instant::now();
        table = table.with_updated_declaration_name_rows_with_project_context(
            &paths,
            fresh_rows.clone(),
            None,
        );
        dirty_us.push(update_started.elapsed().as_micros());
        assert_eq!(table.len(), expected_len);
    }
    stop_sampling.store(true, std::sync::atomic::Ordering::Relaxed);
    sampler.join().expect("memory sampler");
    dirty_us.sort_unstable();

    while !table.needs_compaction() {
        table = table.with_updated_declaration_name_rows_with_project_context(
            &paths,
            fresh_rows.clone(),
            None,
        );
        assert_eq!(table.len(), expected_len);
    }
    let segments_before_compaction = table.delta_segment_count();
    let compaction_private_before = current_process_memory_bytes();
    let compaction_peak_private =
        std::sync::Arc::new(std::sync::atomic::AtomicU64::new(compaction_private_before));
    let stop_compaction_sampling = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let compaction_sampler = {
        let peak_private = compaction_peak_private.clone();
        let stop_sampling = stop_compaction_sampling.clone();
        std::thread::spawn(move || {
            while !stop_sampling.load(std::sync::atomic::Ordering::Relaxed) {
                peak_private.fetch_max(
                    current_process_memory_bytes(),
                    std::sync::atomic::Ordering::Relaxed,
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        })
    };
    let compaction_started = std::time::Instant::now();
    let compacted = table.compacted();
    let compaction_ms = compaction_started.elapsed().as_millis();
    stop_compaction_sampling.store(true, std::sync::atomic::Ordering::Relaxed);
    compaction_sampler
        .join()
        .expect("compaction memory sampler");
    assert_eq!(compacted.len(), expected_len);
    assert_eq!(compacted.delta_segment_count(), 0);

    println!("name_rows: {expected_len}");
    println!("name_changed_rows: {}", fresh_rows.len());
    println!(
        "name_compact_entry_bytes: {}",
        std::mem::size_of::<CompactNameEntry>()
    );
    println!(
        "name_owned_entry_bytes: {}",
        std::mem::size_of::<NameEntry>()
    );
    println!("name_unique_names: {}", table.base.names.len());
    println!("name_unique_paths: {}", table.base.paths.len());
    println!("name_unique_projects: {}", table.base.projects.len());
    println!("name_sql_visit_ms: {sql_visit_ms}");
    println!("name_finalize_ms: {finalize_ms}");
    println!("name_stream_build_ms: {stream_build_ms}");
    println!("name_dirty_update_us: {}", dirty_us[dirty_us.len() / 2]);
    println!(
        "name_dirty_private_delta_bytes: {}",
        peak_private
            .load(std::sync::atomic::Ordering::Relaxed)
            .saturating_sub(private_before)
    );
    println!("name_compaction_input_segments: {segments_before_compaction}");
    println!("name_compaction_ms: {compaction_ms}");
    println!(
        "name_compaction_private_delta_bytes: {}",
        compaction_peak_private
            .load(std::sync::atomic::Ordering::Relaxed)
            .saturating_sub(compaction_private_before)
    );
}

fn table() -> NameTable {
    NameTable::build(vec![
        (1, "hello_value".to_string(), false),
        (2, "KePmmAllocPages".to_string(), false),
        (3, "KeKvaInit".to_string(), false),
        (4, "main".to_string(), false),
        (5, "hello".to_string(), false),
    ])
}

#[test]
fn compact_name_entry_stays_within_three_ids_and_flags_layout() {
    assert!(
        std::mem::size_of::<CompactNameEntry>() <= 24,
        "compact entries must not regain per-symbol pointers"
    );
}

#[test]
fn compact_name_flags_round_trip_family_and_scope_evidence() {
    let cases = [
        (SemanticFamily::CFamily, false, false, false),
        (SemanticFamily::CFamily, false, true, false),
        (SemanticFamily::CFamily, true, false, false),
        (SemanticFamily::CFamily, true, true, true),
        (SemanticFamily::Go, false, false, false),
        (SemanticFamily::Go, false, true, false),
        (SemanticFamily::Go, true, false, false),
        (SemanticFamily::Go, true, true, true),
    ];

    assert_eq!(std::mem::size_of::<CompactNameFlags>(), 1);
    for (semantic_family, external, directly_included, expected_directly_included) in cases {
        let flags = CompactNameFlags::new(semantic_family, external, directly_included);
        assert_eq!(flags.semantic_family(), semantic_family);
        assert_eq!(flags.external(), external);
        assert_eq!(
            flags.directly_included(),
            expected_directly_included,
            "workspace entries cannot carry direct-external evidence"
        );
    }
}

#[test]
fn bounded_top_selection_matches_full_sort_at_scale() {
    let names = (0..10_000)
        .map(|index| {
            (
                index as i64,
                format!("symbol_{:05}", (index * 7919) % 10_000),
                false,
            )
        })
        .collect();
    let table = NameTable::build(names);
    let candidates: Vec<ScoredCandidate> = (0..table.len())
        .map(|index| ScoredCandidate {
            score: ((index * 104_729) % 50_000) as i32,
            name_len: table.active_entry(index).name.len(),
            index,
            tier: ScopeTier::Global,
            base_match: 0,
        })
        .collect();
    let mut oracle = candidates.clone();
    sort_scored(&mut oracle, &table);
    oracle.truncate(200);

    assert_eq!(
        top_scored(candidates, 200, &table)
            .into_iter()
            .map(|candidate| candidate.index)
            .collect::<Vec<_>>(),
        oracle
            .into_iter()
            .map(|candidate| candidate.index)
            .collect::<Vec<_>>()
    );
}

#[test]
fn exact_and_prefix_rank_above_subsequence() {
    let table = table();
    let hits = table.search("hello", 10);
    // "hello" (exact) before "hello_value" (prefix).
    assert_eq!(hits.first().copied(), Some(5));
    assert!(hits.contains(&1));
}

#[test]
fn camel_initials_match_as_subsequence() {
    let table = table();
    let hits = table.search("kpa", 10);
    assert_eq!(hits.first().copied(), Some(2)); // KePmmAllocPages
}

#[test]
fn boundary_initials_match_is_not_lost_to_an_earlier_internal_character() {
    let table = NameTable::build(vec![(1, "device_bind_driver_to_node".to_string(), false)]);

    let hits = table.search_ranked("dbdtn", 10);

    assert_eq!(hits.first().map(|hit| hit.id), Some(1));
    assert_eq!(
        hits[0].base_match, 400,
        "an available all-boundary subsequence must keep the initials tier"
    );
}

#[test]
fn non_subsequence_is_rejected() {
    let table = table();
    let hits = table.search("zzz", 10);
    assert!(hits.is_empty());
}

#[test]
fn empty_query_returns_capped_sorted() {
    let table = table();
    let hits = table.search("   ", 2);
    assert_eq!(hits.len(), 2);
}

// --- Reachability-scoped completion (limited #include analysis) -----------

fn scoped_table() -> NameTable {
    // Two same-prefixed symbols defined in different files; one reachable
    // from the current file, one not.
    NameTable::build_with_paths(vec![
        (
            1,
            "widget_make".to_string(),
            false,
            "inc/b.h".to_string(),
            "function".to_string(),
            false,
        ),
        (
            2,
            "widget_zzz".to_string(),
            false,
            "other/c.h".to_string(),
            "function".to_string(),
            false,
        ),
    ])
}

fn scope(current: &str, reachable: &[&str], open: bool) -> CompletionScope {
    CompletionScope {
        current_path: Some(current.to_string()),
        direct_external_files: Default::default(),
        reach: ReachScope {
            files: reachable.iter().map(|s| s.to_string()).collect(),
            heuristic_files: Default::default(),
            open,
            reason: None,
        },
    }
}

#[test]
fn reachable_candidate_outranks_unreachable() {
    let table = scoped_table();
    // Current file reaches inc/b.h but not other/c.h; set is determinate.
    let sc = scope("src/a.c", &["src/a.c", "inc/b.h"], false);
    let hits = table.search_ranked_scoped("widget", 10, Some(&sc));
    assert_eq!(hits[0].name, "widget_make", "reachable symbol ranks first");
    // The unreachable symbol is demoted but NOT dropped.
    assert!(
        hits.iter().any(|h| h.name == "widget_zzz"),
        "unreachable symbol still present"
    );
}

#[test]
fn open_scope_does_not_bury_unreachable() {
    let table = scoped_table();
    // Open (uncertain) scope: widget_zzz is not proven reachable, so it
    // routes to `Unknown` tier; under a determinate (closed) scope it
    // routes to `Global`. Both rank below `Reachable` (widget_make), but
    // `Unknown` outranks `Global`, so the open-scope score is higher.
    let sc = scope("src/a.c", &["src/a.c"], true);
    let determinate = scope("src/a.c", &["src/a.c"], false);

    let open_hits = table.search_ranked_scoped("widget", 10, Some(&sc));
    let det_hits = table.search_ranked_scoped("widget", 10, Some(&determinate));

    let open_zzz = open_hits.iter().find(|h| h.name == "widget_zzz").unwrap();
    let det_zzz = det_hits.iter().find(|h| h.name == "widget_zzz").unwrap();
    assert_eq!(open_zzz.tier, crate::model::ScopeTier::Unknown);
    assert_eq!(det_zzz.tier, crate::model::ScopeTier::Global);
    assert!(
        open_zzz.score > det_zzz.score,
        "Unknown tier outranks Global tier: open scope softens the demotion"
    );
}

#[test]
fn scoping_never_empties_the_list() {
    let table = scoped_table();
    // Even when nothing is reachable, determinate scoping must not drop the
    // global (fallback) candidates — they are only demoted.
    let sc = scope("src/lonely.c", &["src/lonely.c"], false);
    let hits = table.search_ranked_scoped("widget", 10, Some(&sc));
    assert_eq!(hits.len(), 2, "both candidates remain, just demoted");
}

#[test]
fn unscoped_search_is_unchanged_by_scoping_path() {
    // Passing None reproduces the legacy ranking exactly.
    let table = scoped_table();
    let with_none = table.search_ranked_scoped("widget", 10, None);
    let legacy = table.search_ranked("widget", 10);
    assert_eq!(with_none, legacy);
}

#[test]
fn name_table_tags_workspace_entries_and_keeps_external_entries_unowned() {
    use crate::project_context::{ProjectContext, ProjectContextIndex, ProjectKey};

    let root_id = "root-a".to_string();
    let key = ProjectKey {
        workspace_root_id: root_id.clone(),
        project_path: "app".to_string(),
    };
    let projects = ProjectContextIndex::new(
        root_id,
        "workspace".to_string(),
        vec![ProjectContext {
            key: key.clone(),
            workspace_name: "workspace".to_string(),
            marker_files: vec!["Makefile".to_string()],
        }],
    );
    let table = NameTable::build_with_paths_and_project_context(
        vec![
            (
                1,
                "project_api".to_string(),
                false,
                "app/src/api.c".to_string(),
                "function".to_string(),
                false,
            ),
            (
                2,
                "external_api".to_string(),
                true,
                "C:/sdk/api.h".to_string(),
                "function".to_string(),
                true,
            ),
        ],
        &projects,
    );

    let hits = table.search_ranked("api", 10);
    assert_eq!(
        hits.iter()
            .find(|hit| hit.id == 1)
            .and_then(|hit| hit.project_key.as_ref()),
        Some(&key)
    );
    assert!(hits
        .iter()
        .find(|hit| hit.id == 2)
        .expect("external")
        .project_key
        .is_none());
    assert_eq!(
        table.project_indices(&key).map(|indices| indices.len()),
        Some(1)
    );
}

// --- Prefix index + incremental narrowing (completion performance) --------

#[test]
fn prefix_candidates_match_full_scan_exact_prefix() {
    let table = NameTable::build(vec![
        (1, "foo_a".to_string(), false),
        (2, "foo_b".to_string(), false),
        (3, "xfooy".to_string(), false), // substring, not prefix
        (4, "bar".to_string(), false),
    ]);
    let mut ids: Vec<i64> = table
        .prefix_candidates("foo")
        .iter()
        .map(|&i| table.active_entry(i).id)
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 2], "only exact/prefix entries, not substrings");
}

#[test]
fn name_id_counting_sort_preserves_spelling_order_and_duplicate_stability() {
    let table = NameTable::build(vec![
        (1, "beta".to_string(), false),
        (2, "Alpha".to_string(), false),
        (3, "alpha".to_string(), false),
        (4, "Alpha".to_string(), false),
        (5, "ALPHA".to_string(), false),
    ]);
    let ids: Vec<i64> = table
        .prefix_candidates("a")
        .into_iter()
        .map(|index| table.active_entry(index).id)
        .collect();
    assert_eq!(ids, vec![5, 2, 4, 3]);
}

#[test]
fn prefix_index_fast_path_matches_full_scan() {
    // When prefix candidates fill the limit, the fast path must return the
    // same ranked hits the full scan would.
    let table = NameTable::build(vec![
        (1, "foo_a".to_string(), false),
        (2, "foo_b".to_string(), false),
        (3, "foo_c".to_string(), false),
        (4, "xfooy".to_string(), false), // substring tail, must be excluded
    ]);
    let fast = table.search_ranked("foo", 3);
    let full = table.search_ranked_scoped_pooled("foo", 3, None, None).0;
    assert_eq!(fast, full);
    assert!(
        fast.iter().all(|h| h.id != 4),
        "substring tail truncated out"
    );
}

#[test]
fn short_prefix_fast_path_matches_full_scan() {
    // len 2 with enough prefix candidates: boundary/plain substrings that the
    // short-prefix gate would consider are still correctly truncated out.
    let table = NameTable::build(vec![
        (1, "foo".to_string(), false),
        (2, "fox".to_string(), false),
        (3, "fob".to_string(), false),
        (4, "barfo".to_string(), false),
    ]);
    let fast = table.search_ranked("fo", 3);
    let full = table.search_ranked_scoped_pooled("fo", 3, None, None).0;
    assert_eq!(fast, full);
}

#[test]
fn fast_path_falls_back_when_candidates_below_limit() {
    let table = table();
    // "hello" has < 10 prefix candidates, so search_ranked uses the full scan
    // and still includes subsequence/substring recall identical to the pooled
    // baseline.
    let fast = table.search_ranked("hello", 10);
    let full = table.search_ranked_scoped_pooled("hello", 10, None, None).0;
    assert_eq!(fast, full);
}

#[test]
fn narrowing_from_prior_pool_matches_cold_scan() {
    let table = NameTable::build(vec![
        (1, "foobar".to_string(), false),
        (2, "foobaz".to_string(), false),
        (3, "foxtrot".to_string(), false),
        (4, "other".to_string(), false),
    ]);
    // Pool for "fo" is tier-agnostic (every Some match).
    let (_, pool) = table.search_ranked_scoped_pooled("fo", 10, None, None);
    // Extending to "foob": narrowing the pool must equal a cold full scan.
    let narrowed = table
        .search_ranked_scoped_pooled("foob", 10, None, Some(&pool))
        .0;
    let cold = table.search_ranked_scoped_pooled("foob", 10, None, None).0;
    assert_eq!(narrowed, cold);
}

#[test]
fn narrowing_keeps_subsequence_across_short_to_long_prefix() {
    // A name that only subsequence-matches at len 2 is gated out of the len-2
    // *hits*, but must remain in the pool so it is recalled at len 3.
    let table = NameTable::build(vec![
        (1, "Foobar".to_string(), false),
        (2, "affob".to_string(), false), // substring "fo" at len2 (gated), subseq "fob" at len3
    ]);
    let (hits2, pool) = table.search_ranked_scoped_pooled("fo", 10, None, None);
    // At len 2 "affob" is a plain substring (score 500) → gated out of hits.
    assert!(hits2.iter().all(|h| h.id != 2));
    // But it stayed in the pool (tier-agnostic), so len-3 narrowing recalls it.
    let narrowed = table
        .search_ranked_scoped_pooled("fob", 10, None, Some(&pool))
        .0;
    let cold = table.search_ranked_scoped_pooled("fob", 10, None, None).0;
    assert_eq!(narrowed, cold);
    assert!(
        narrowed.iter().any(|h| h.id == 2),
        "subsequence recalled at len 3 from the len-2 pool"
    );
}

#[test]
fn channel_recall_keeps_reachable_and_global_representation() {
    let table = NameTable::build_with_paths(vec![
        (
            1,
            "api_reachable".to_string(),
            false,
            "inc/a.h".to_string(),
            "function".to_string(),
            false,
        ),
        (
            2,
            "api_global".to_string(),
            false,
            "other/b.c".to_string(),
            "function".to_string(),
            false,
        ),
    ]);
    let scope = scope("src/main.c", &["src/main.c", "inc/a.h"], false);
    let quotas = CompletionRecallQuotas {
        total_indexed: 4,
        reachable: 2,
        external: 1,
        unknown: 1,
        global: 2,
        same_project: 0,
    };

    let (hits, pool, metrics) =
        table.search_completion_recall_pooled("api", quotas, Some(&scope), None);

    assert!(hits.iter().any(|hit| hit.name == "api_reachable"));
    assert!(hits.iter().any(|hit| hit.name == "api_global"));
    assert_eq!(metrics.reachable, 1);
    assert_eq!(metrics.global, 1);
    assert!(!pool.is_empty());
}

#[test]
fn channel_recall_preserves_short_prefix_noise_gate() {
    let table = NameTable::build(vec![
        (1, "FooBar".to_string(), false),
        (2, "Foobar".to_string(), false),
    ]);
    let quotas = CompletionRecallQuotas::default_for_completion_limit(100);

    let (hits, _, _) = table.search_completion_recall_pooled("ba", quotas, None, None);
    let names: Vec<_> = hits.iter().map(|hit| hit.name.as_str()).collect();

    assert!(names.contains(&"FooBar"));
    assert!(!names.contains(&"Foobar"));
}

#[test]
fn channel_recall_narrowing_matches_cold_scan() {
    let table = NameTable::build(vec![
        (1, "foobar".to_string(), false),
        (2, "foobaz".to_string(), false),
        (3, "foxtrot".to_string(), false),
    ]);
    let quotas = CompletionRecallQuotas::default_for_completion_limit(100);
    let (_, pool) = table.search_ranked_scoped_pooled("fo", 100, None, None);

    let narrowed = table
        .search_completion_recall_pooled("foob", quotas, None, Some(&pool))
        .0;
    let cold = table
        .search_completion_recall_pooled("foob", quotas, None, None)
        .0;

    assert_eq!(narrowed, cold);
}

#[derive(Default)]
struct CancelAfterFirstRecallBlock {
    checks: AtomicUsize,
}

struct CancelOnRecallCheck {
    checks: AtomicUsize,
    cancel_on: usize,
}

impl CompletionQueryCancellation for CancelOnRecallCheck {
    fn is_cancelled(&self) -> bool {
        self.checks.fetch_add(1, Ordering::Relaxed) + 1 >= self.cancel_on
    }
}

struct NeverCancelRecall;

struct CountRecallChecks {
    checks: AtomicUsize,
}

impl CompletionQueryCancellation for NeverCancelRecall {
    fn is_cancelled(&self) -> bool {
        false
    }
}

impl CompletionQueryCancellation for CountRecallChecks {
    fn is_cancelled(&self) -> bool {
        self.checks.fetch_add(1, Ordering::Relaxed);
        false
    }
}

impl CompletionQueryCancellation for CancelAfterFirstRecallBlock {
    fn is_cancelled(&self) -> bool {
        self.checks.fetch_add(1, Ordering::Relaxed) > 0
    }
}

#[test]
fn production_completion_recall_reports_work_and_observes_cooperative_cancellation() {
    let table = NameTable::build(
        (0..10_000)
            .map(|index| {
                (
                    i64::from(index),
                    format!("completion_candidate_{index:05}"),
                    false,
                )
            })
            .collect(),
    );
    let quotas = CompletionRecallQuotas::default_for_completion_limit(100);

    let (_, _, cold_metrics) =
        table.search_completion_recall_pooled("completion", quotas, None, None);
    assert_eq!(cold_metrics.entries_inspected, table.len());
    assert!(!cold_metrics.cancelled);

    let cancellation = CancelAfterFirstRecallBlock::default();
    let (hits, pool, cancelled_metrics) =
        table.search_completion_recall_pooled_controlled(CompletionRecallQuery {
            query: "completion",
            quotas,
            scope: None,
            active_project: None,
            prior_pool: None,
            semantic_family: None,
            cancellation: Some(&cancellation),
            candidate_budget: usize::MAX,
        });
    assert!(cancelled_metrics.cancelled);
    assert!(
        cancelled_metrics.entries_inspected <= COMPLETION_CANCELLATION_CHECK_INTERVAL,
        "cooperative cancellation must bound stale scan work"
    );
    assert!(hits.is_empty(), "partial stale results must not escape");
    assert!(
        pool.is_empty(),
        "partial stale pools must not enter the memo"
    );
}

#[test]
fn production_completion_recall_checks_cancellation_during_post_scan_selection() {
    let table = NameTable::build(
        (0..10_000)
            .map(|index| {
                (
                    i64::from(index),
                    format!("completion_candidate_{index:05}"),
                    false,
                )
            })
            .collect(),
    );
    let quotas = CompletionRecallQuotas::default_for_completion_limit(100);
    let scan_checks = table.len().div_ceil(COMPLETION_CANCELLATION_CHECK_INTERVAL) + 1;
    let cancellation = CancelOnRecallCheck {
        checks: AtomicUsize::new(0),
        // Two outer phase checks and the controlled selector's entry check
        // follow the scan. Flip after its first 256-entry selection block.
        cancel_on: scan_checks + 4,
    };

    let (hits, pool, metrics) =
        table.search_completion_recall_pooled_controlled(CompletionRecallQuery {
            query: "completion",
            quotas,
            scope: None,
            active_project: None,
            prior_pool: None,
            semantic_family: None,
            cancellation: Some(&cancellation),
            candidate_budget: usize::MAX,
        });

    assert!(metrics.cancelled, "selection must observe stale requests");
    assert!(hits.is_empty(), "post-scan cancellation must discard hits");
    assert!(
        pool.is_empty(),
        "post-scan cancellation must discard memo pools"
    );
    assert!(
        (1..=COMPLETION_CANCELLATION_CHECK_INTERVAL).contains(&metrics.selection_entries_inspected),
        "stale selection work must be bounded to one cooperative block, got {}",
        metrics.selection_entries_inspected
    );
    assert!(
        metrics.cancellation_checks > scan_checks,
        "selection phases need checkpoints after the final scan check"
    );
}

#[test]
fn production_completion_recall_counts_and_cancels_sparse_channel_filtering() {
    let table = NameTable::build(
        (0..10_000)
            .map(|index| {
                (
                    i64::from(index),
                    format!("completion_candidate_{index:05}"),
                    false,
                )
            })
            .collect(),
    );
    let quotas = CompletionRecallQuotas::default_for_completion_limit(100);
    let scan_checks = table.len().div_ceil(COMPLETION_CANCELLATION_CHECK_INTERVAL) + 1;
    let global_selection_checks = 1 + table.len().div_ceil(COMPLETION_CANCELLATION_CHECK_INTERVAL);
    let cancellation = CancelOnRecallCheck {
        checks: AtomicUsize::new(0),
        // After scan, two outer checks, and the complete global reducer,
        // cancel after the first source block of the empty Reachable channel.
        cancel_on: scan_checks + 2 + global_selection_checks + 2,
    };

    let (hits, pool, metrics) =
        table.search_completion_recall_pooled_controlled(CompletionRecallQuery {
            query: "completion",
            quotas,
            scope: None,
            active_project: None,
            prior_pool: None,
            semantic_family: None,
            cancellation: Some(&cancellation),
            candidate_budget: usize::MAX,
        });

    assert!(metrics.cancelled);
    assert!(hits.is_empty());
    assert!(pool.is_empty());
    assert_eq!(
        metrics.selection_entries_inspected,
        table.len() + COMPLETION_CANCELLATION_CHECK_INTERVAL,
        "filtered-out source entries must still consume the cancellation budget"
    );
}

#[test]
fn bounded_completion_recall_matches_full_scan_when_budget_covers_table() {
    let table = NameTable::build(vec![
        (1, "NeedleTarget".to_string(), false),
        (2, "needle_prefix".to_string(), false),
        (3, "boundary_needle".to_string(), false),
        (4, "NotEveryLetter".to_string(), false),
        (5, "unrelated".to_string(), false),
    ]);
    let quotas = CompletionRecallQuotas::default_for_completion_limit(100);
    let oracle = table.search_completion_recall_pooled("needle", quotas, None, None);
    let cancellation = NeverCancelRecall;
    let bounded = table.search_completion_recall_pooled_controlled(CompletionRecallQuery {
        query: "needle",
        quotas,
        scope: None,
        active_project: None,
        prior_pool: None,
        semantic_family: None,
        cancellation: Some(&cancellation),
        candidate_budget: 64,
    });

    assert_eq!(bounded.0, oracle.0);
    assert_eq!(bounded.1, oracle.1);
    assert!(!bounded.2.truncated);
    assert_eq!(bounded.2.active_entries_total, table.len());
    assert_eq!(bounded.2.candidate_budget, 64);
    assert_eq!(bounded.2.entries_inspected, table.len());
}

#[test]
fn bounded_completion_recall_matches_full_scan_across_match_tiers() {
    let table = NameTable::build(vec![
        (1, "needle".to_string(), false),
        (2, "needle_prefix".to_string(), false),
        (3, "word_needle_suffix".to_string(), false),
        (4, "preneedlepost".to_string(), false),
        (5, "NetworkEventDispatcher".to_string(), false),
        (6, "narrow_even_deeper_link".to_string(), false),
        (7, "alpha_beta".to_string(), false),
        (8, "unrelated".to_string(), false),
    ]);
    let quotas = CompletionRecallQuotas::default_for_completion_limit(100);
    let cancellation = NeverCancelRecall;

    for query in ["needle", "nee", "edle", "ned", "ndl", "ab", "alpha_b"] {
        let oracle = table.search_completion_recall_pooled(query, quotas, None, None);
        let bounded = table.search_completion_recall_pooled_controlled(CompletionRecallQuery {
            query,
            quotas,
            scope: None,
            active_project: None,
            prior_pool: None,
            semantic_family: None,
            cancellation: Some(&cancellation),
            candidate_budget: table.len(),
        });

        assert_eq!(bounded.0, oracle.0, "ranked hits diverged for {query}");
        assert_eq!(bounded.1, oracle.1, "candidate pool diverged for {query}");
        assert!(
            !bounded.2.truncated,
            "complete scan marked {query} truncated"
        );
        assert_eq!(
            bounded.2.prefix_entries_inspected + bounded.2.fuzzy_entries_inspected,
            bounded.2.entries_inspected,
            "candidate work accounting diverged for {query}"
        );
        assert!(
            bounded.2.selection_entries_inspected <= bounded.2.entries_inspected.saturating_mul(6),
            "selection work escaped its fixed channel bound for {query}: {:?}",
            bounded.2
        );
    }
}

#[test]
fn accounted_segment_split_tracks_base_and_delta_segments() {
    use crate::semantic_model::{SemanticDeclarationKind, SemanticDeclarationRole};
    use crate::store::views::DeclarationNameRow;

    fn row(id: i64, name: &str, path: &str) -> DeclarationNameRow {
        DeclarationNameRow {
            id,
            name: name.into(),
            declaration_kind: SemanticDeclarationKind::Function,
            role: SemanticDeclarationRole::Definition,
            path: path.into(),
            external: false,
            directly_included: false,
            semantic_family: SemanticFamily::CFamily,
        }
    }

    let table = NameTable::build_from_declaration_name_rows_with_project_context(
        vec![row(1, "alpha_main", "src/main.c")],
        None,
    );
    let (base, deltas, delta_count) = table.accounted_segment_split();
    assert!(base > 0);
    assert_eq!(deltas, 0);
    assert_eq!(delta_count, 0);
    assert!(table.accounted_bytes() >= base);

    let updated = table.with_updated_declaration_name_rows_with_project_context(
        &std::collections::HashSet::from(["src/main.c".to_string()]),
        vec![
            row(1, "alpha_main", "src/main.c"),
            row(2, "alpha_util", "src/main.c"),
        ],
        None,
    );
    let (updated_base, updated_deltas, updated_delta_count) = updated.accounted_segment_split();
    assert_eq!(updated_base, base);
    assert!(updated_deltas > 0);
    assert_eq!(updated_delta_count, 1);
    assert!(updated.accounted_bytes() >= updated_base.saturating_add(updated_deltas));
}

#[test]
fn bounded_completion_recall_filters_semantic_family_before_spending_budget() {
    use crate::semantic_model::{SemanticDeclarationKind, SemanticDeclarationRole};
    use crate::store::views::DeclarationNameRow;

    let mut rows: Vec<_> = (0..400)
        .map(|index| DeclarationNameRow {
            id: i64::from(index),
            name: format!("api_aaa_go_{index:04}"),
            declaration_kind: SemanticDeclarationKind::Function,
            role: SemanticDeclarationRole::Definition,
            path: format!("go/{index}.go"),
            external: false,
            directly_included: false,
            semantic_family: SemanticFamily::Go,
        })
        .collect();
    rows.push(DeclarationNameRow {
        id: 10_000,
        name: "api_zzz_c_target".to_string(),
        declaration_kind: SemanticDeclarationKind::Function,
        role: SemanticDeclarationRole::Definition,
        path: "src/target.c".to_string(),
        external: false,
        directly_included: false,
        semantic_family: SemanticFamily::CFamily,
    });
    let table = NameTable::build_from_declaration_name_rows_with_project_context(rows, None);
    let cancellation = NeverCancelRecall;
    let (hits, _, metrics) =
        table.search_completion_recall_pooled_controlled(CompletionRecallQuery {
            query: "api",
            quotas: CompletionRecallQuotas::default_for_completion_limit(10),
            scope: None,
            active_project: None,
            prior_pool: None,
            semantic_family: Some(SemanticFamily::CFamily),
            cancellation: Some(&cancellation),
            candidate_budget: 32,
        });

    assert_eq!(
        hits.iter().map(|hit| hit.id).collect::<Vec<_>>(),
        vec![10_000]
    );
    assert!(metrics.entries_inspected <= 32, "{metrics:?}");
    assert!(
        !metrics.truncated,
        "the only C-family candidate fits in budget: {metrics:?}"
    );
}

#[test]
fn selected_project_presence_is_language_partitioned_without_copying_indices() {
    use crate::project_context::{ProjectContext, ProjectContextIndex, ProjectKey};
    use crate::semantic_model::{SemanticDeclarationKind, SemanticDeclarationRole};
    use crate::store::views::DeclarationNameRow;

    let key = ProjectKey {
        workspace_root_id: "root".to_string(),
        project_path: "selected".to_string(),
    };
    let projects = ProjectContextIndex::new(
        "root".to_string(),
        "workspace".to_string(),
        vec![ProjectContext {
            key: key.clone(),
            workspace_name: "workspace".to_string(),
            marker_files: vec!["selected/go.mod".to_string()],
        }],
    );
    let table = NameTable::build_from_declaration_name_rows_with_project_context(
        (0..500)
            .map(|index| DeclarationNameRow {
                id: i64::from(index),
                name: format!("ProjectGo{index:03}"),
                declaration_kind: SemanticDeclarationKind::Function,
                role: SemanticDeclarationRole::Definition,
                path: format!("selected/pkg{index:03}.go"),
                external: false,
                directly_included: false,
                semantic_family: SemanticFamily::Go,
            })
            .collect(),
        Some(&projects),
    );

    assert!(table.has_project_for_family(&key, SemanticFamily::Go));
    assert!(
        !table.has_project_for_family(&key, SemanticFamily::CFamily),
        "a C completion must not enable selected-project quotas from Go-only postings"
    );
    assert_eq!(
        table.active_project_family_count(&key, SemanticFamily::Go),
        500
    );

    let shadowed_paths = (0..499)
        .map(|index| format!("selected/pkg{index:03}.go"))
        .collect::<HashSet<_>>();
    let updated = table.with_updated_declaration_name_rows_with_project_context(
        &shadowed_paths,
        Vec::new(),
        Some(&projects),
    );
    assert_eq!(
        updated.active_project_family_count(&key, SemanticFamily::Go),
        1,
        "project/family presence must be maintained from active rows at publication time"
    );
    assert!(updated.has_project_for_family(&key, SemanticFamily::Go));

    let last_path = HashSet::from(["selected/pkg499.go".to_string()]);
    let switched = updated.with_updated_declaration_name_rows_with_project_context(
        &last_path,
        vec![DeclarationNameRow {
            id: 10_000,
            name: "ProjectCReplacement".to_string(),
            declaration_kind: SemanticDeclarationKind::Function,
            role: SemanticDeclarationRole::Definition,
            path: "selected/pkg499.go".to_string(),
            external: false,
            directly_included: false,
            semantic_family: SemanticFamily::CFamily,
        }],
        Some(&projects),
    );
    assert_eq!(
        switched.active_project_family_count(&key, SemanticFamily::Go),
        0
    );
    assert_eq!(
        switched.active_project_family_count(&key, SemanticFamily::CFamily),
        1
    );
    assert!(!switched.has_project_for_family(&key, SemanticFamily::Go));
    assert!(switched.has_project_for_family(&key, SemanticFamily::CFamily));

    let compacted = switched.compacted();
    assert_eq!(
        compacted.active_project_family_count(&key, SemanticFamily::Go),
        0
    );
    assert_eq!(
        compacted.active_project_family_count(&key, SemanticFamily::CFamily),
        1,
        "compaction must rebuild active project/family counts from live rows"
    );

    let unowned = compacted.with_project_context(None);
    assert_eq!(
        unowned.active_project_family_count(&key, SemanticFamily::CFamily),
        0,
        "removing marker ownership must clear the active project summary"
    );
    let reassigned = unowned.with_project_context(Some(&projects));
    assert_eq!(
        reassigned.active_project_family_count(&key, SemanticFamily::CFamily),
        1,
        "marker refresh must rebuild the active project summary"
    );
}

#[test]
fn bounded_single_character_recall_preserves_static_top_quality() {
    let mut names: Vec<_> = (0..1_000)
        .map(|index| {
            (
                i64::from(index),
                format!("caaaaaaaa_workspace_candidate_{index:04}"),
                false,
            )
        })
        .collect();
    names.extend((0..20).map(|index| (10_000 + i64::from(index), format!("czz{index:02}"), false)));
    let table = NameTable::build(names);
    let quotas = CompletionRecallQuotas::default_for_completion_limit(20);
    let oracle = table.search_completion_recall_pooled("c", quotas, None, None);
    let cancellation = NeverCancelRecall;
    let bounded = table.search_completion_recall_pooled_controlled(CompletionRecallQuery {
        query: "c",
        quotas,
        scope: None,
        active_project: None,
        prior_pool: None,
        semantic_family: Some(SemanticFamily::CFamily),
        cancellation: Some(&cancellation),
        candidate_budget: 128,
    });
    let oracle_top: Vec<_> = oracle.0.iter().take(20).map(|hit| hit.id).collect();
    let bounded_top: Vec<_> = bounded.0.iter().take(20).map(|hit| hit.id).collect();

    assert_eq!(bounded_top, oracle_top);
    assert!(bounded.2.truncated);
    assert!(bounded.2.entries_inspected <= 128, "{:?}", bounded.2);
}

#[test]
fn bounded_single_character_postings_respect_delta_shadow_and_tombstone() {
    let mut names: Vec<_> = (0..100)
        .map(|index| {
            (
                i64::from(index),
                format!("c_workspace_candidate_{index:04}"),
                false,
                format!("src/{index}.c"),
                "function".to_string(),
                false,
            )
        })
        .collect();
    names.push((
        1_000,
        "c_old".to_string(),
        false,
        "src/changed.c".to_string(),
        "function".to_string(),
        false,
    ));
    let table = NameTable::build_with_paths(names);
    let changed = HashSet::from(["src/changed.c".to_string()]);
    let updated = table.with_updated_paths(
        &changed,
        vec![(
            2_000,
            "c_new".to_string(),
            false,
            "src/changed.c".to_string(),
            "function".to_string(),
            false,
        )],
    );
    let cancellation = NeverCancelRecall;
    let recall = |table: &NameTable| {
        table.search_completion_recall_pooled_controlled(CompletionRecallQuery {
            query: "c",
            quotas: CompletionRecallQuotas::default_for_completion_limit(10),
            scope: None,
            active_project: None,
            prior_pool: None,
            semantic_family: Some(SemanticFamily::CFamily),
            cancellation: Some(&cancellation),
            candidate_budget: 16,
        })
    };

    let (hits, _, metrics) = recall(&updated);
    assert!(hits.iter().any(|hit| hit.id == 2_000));
    assert!(hits.iter().all(|hit| hit.id != 1_000));
    assert!(metrics.entries_inspected <= 16, "{metrics:?}");

    let deleted = updated.with_updated_paths(&changed, vec![]);
    let (hits, _, metrics) = recall(&deleted);
    assert!(hits.iter().all(|hit| hit.id != 1_000 && hit.id != 2_000));
    assert!(metrics.entries_inspected <= 16, "{metrics:?}");
}

#[test]
fn bounded_prefix_budget_cannot_be_consumed_by_shadowed_base_rows() {
    let stale_path = "src/generated_api.h".to_string();
    let table = NameTable::build_with_paths(
        (0..80)
            .map(|index| {
                (
                    i64::from(index),
                    format!("aa_old_{index:03}"),
                    false,
                    stale_path.clone(),
                    "function".to_string(),
                    false,
                )
            })
            .collect(),
    );
    let updated = table.with_updated_paths(
        &HashSet::from([stale_path.clone()]),
        vec![(
            10_000,
            "aazzz_live".to_string(),
            false,
            stale_path,
            "function".to_string(),
            false,
        )],
    );
    let cancellation = NeverCancelRecall;
    let (hits, _, metrics) =
        updated.search_completion_recall_pooled_controlled(CompletionRecallQuery {
            query: "aa",
            quotas: CompletionRecallQuotas::default_for_completion_limit(10),
            scope: None,
            active_project: None,
            prior_pool: None,
            semantic_family: Some(SemanticFamily::CFamily),
            cancellation: Some(&cancellation),
            candidate_budget: 32,
        });

    assert!(
        hits.iter().any(|hit| hit.id == 10_000),
        "the only active delta declaration was starved by stale base rows: {metrics:?}"
    );
    assert!(hits.iter().all(|hit| hit.id < 0 || hit.id >= 10_000));
    assert!(metrics.entries_inspected <= 32, "{metrics:?}");
}

#[test]
fn bounded_short_prefix_budget_cannot_be_consumed_by_shadowed_base_rows() {
    let stale_path = "src/generated_api.h".to_string();
    let table = NameTable::build_with_paths(
        (0..80)
            .map(|index| {
                (
                    i64::from(index),
                    format!("a{index:03}"),
                    false,
                    stale_path.clone(),
                    "function".to_string(),
                    false,
                )
            })
            .collect(),
    );
    let updated = table.with_updated_paths(
        &HashSet::from([stale_path.clone()]),
        vec![(
            10_000,
            "azzz_live".to_string(),
            false,
            stale_path,
            "function".to_string(),
            false,
        )],
    );
    let cancellation = NeverCancelRecall;
    let (hits, _, metrics) =
        updated.search_completion_recall_pooled_controlled(CompletionRecallQuery {
            query: "a",
            quotas: CompletionRecallQuotas::default_for_completion_limit(10),
            scope: None,
            active_project: None,
            prior_pool: None,
            semantic_family: Some(SemanticFamily::CFamily),
            cancellation: Some(&cancellation),
            candidate_budget: 32,
        });

    assert!(
        hits.iter().any(|hit| hit.id == 10_000),
        "the active single-character posting was starved by stale base rows: {metrics:?}"
    );
    assert!(metrics.entries_inspected <= 32, "{metrics:?}");
}

fn mixed_delta_after_subset_tombstone(stale_names: Vec<String>, live_name: &str) -> NameTable {
    let base = NameTable::build_with_paths(
        (0..96)
            .map(|index| {
                (
                    i64::from(index),
                    format!("zz_unrelated_{index:03}"),
                    false,
                    format!("src/base_{index:03}.c"),
                    "function".to_string(),
                    false,
                )
            })
            .collect(),
    );
    let stale_path = "generated/stale.h".to_string();
    let live_path = "generated/live.h".to_string();
    let mut mixed_entries: Vec<_> = stale_names
        .into_iter()
        .enumerate()
        .map(|(index, name)| {
            (
                10_000 + index as i64,
                name,
                false,
                stale_path.clone(),
                "function".to_string(),
                false,
            )
        })
        .collect();
    mixed_entries.push((
        99_999,
        live_name.to_string(),
        false,
        live_path.clone(),
        "function".to_string(),
        false,
    ));
    let mixed = base.with_updated_paths(
        &HashSet::from([stale_path.clone(), live_path]),
        mixed_entries,
    );
    mixed.with_updated_paths(&HashSet::from([stale_path]), vec![])
}

fn assert_mixed_delta_live_candidate_is_recalled(table: &NameTable, query: &str) {
    let cancellation = NeverCancelRecall;
    let (hits, _, metrics) =
        table.search_completion_recall_pooled_controlled(CompletionRecallQuery {
            query,
            quotas: CompletionRecallQuotas::default_for_completion_limit(10),
            scope: None,
            active_project: None,
            prior_pool: None,
            semantic_family: Some(SemanticFamily::CFamily),
            cancellation: Some(&cancellation),
            candidate_budget: 32,
        });

    assert!(
        hits.iter().any(|hit| hit.id == 99_999),
        "the live path behind a tombstoned sibling in the same delta was starved: {metrics:?}"
    );
    assert!(
        hits.iter().all(|hit| hit.id < 10_000 || hit.id == 99_999),
        "tombstoned sibling declarations escaped recall: {hits:?}"
    );
    assert!(
        metrics.priority_source_attempts <= metrics.candidate_budget / 8,
        "priority source setup escaped its request-local share: {metrics:?}"
    );
    assert!(
        metrics.priority_source_probes <= metrics.candidate_budget,
        "priority source probes escaped their bounded multiplier: {metrics:?}"
    );
    assert!(
        metrics.priority_sources_initialized <= metrics.priority_source_attempts,
        "initialized cursor count cannot exceed unique bounded attempts: {metrics:?}"
    );
    assert!(
        metrics.priority_fuzzy_name_probes <= COMPLETION_PRIORITY_METADATA_PROBE_LIMIT,
        "priority fuzzy metadata probes escaped their hard cap: {metrics:?}"
    );
    assert!(
        metrics.priority_fuzzy_declaration_probes <= COMPLETION_PRIORITY_METADATA_PROBE_LIMIT,
        "priority fuzzy multi-path probes escaped their hard cap: {metrics:?}"
    );
    assert!(metrics.entries_inspected <= 32, "{metrics:?}");
}

#[test]
fn bounded_prefix_recall_skips_tombstoned_sibling_path_inside_one_delta() {
    let table = mixed_delta_after_subset_tombstone(
        (0..80).map(|index| format!("aa_old_{index:03}")).collect(),
        "aazzz_live",
    );
    assert_mixed_delta_live_candidate_is_recalled(&table, "aa");
}

#[test]
fn bounded_short_prefix_recall_skips_tombstoned_sibling_path_inside_one_delta() {
    let table = mixed_delta_after_subset_tombstone(
        (0..80).map(|index| format!("a{index:03}")).collect(),
        "azzz_live_with_longer_static_rank",
    );
    assert_mixed_delta_live_candidate_is_recalled(&table, "a");
}

#[test]
fn bounded_fuzzy_recall_skips_tombstoned_sibling_path_inside_one_delta() {
    let table = mixed_delta_after_subset_tombstone(
        (0..80)
            .map(|index| format!("pre_needle_old_{index:03}"))
            .collect(),
        "zz_needle_live_with_a_lexically_late_long_suffix",
    );
    assert_mixed_delta_live_candidate_is_recalled(&table, "needle");
}

fn base_after_subset_tombstone(stale_names: Vec<String>, live_name: &str) -> NameTable {
    let stale_path = "generated/stale.h".to_string();
    let live_path = "generated/live.h".to_string();
    let mut entries: Vec<_> = stale_names
        .into_iter()
        .enumerate()
        .map(|(index, name)| {
            (
                10_000 + index as i64,
                name,
                false,
                stale_path.clone(),
                "function".to_string(),
                false,
            )
        })
        .collect();
    entries.push((
        99_999,
        live_name.to_string(),
        false,
        live_path,
        "function".to_string(),
        false,
    ));
    entries.extend((0..96).map(|index| {
        (
            i64::from(index),
            format!("zz_unrelated_{index:03}"),
            false,
            format!("src/base_{index:03}.c"),
            "function".to_string(),
            false,
        )
    }));
    NameTable::build_with_paths(entries).with_updated_paths(&HashSet::from([stale_path]), vec![])
}

#[test]
fn bounded_prefix_recall_skips_tombstoned_sibling_path_inside_base() {
    let table = base_after_subset_tombstone(
        (0..80).map(|index| format!("aa_old_{index:03}")).collect(),
        "aazzz_live",
    );
    assert_mixed_delta_live_candidate_is_recalled(&table, "aa");
}

#[test]
fn bounded_short_prefix_recall_skips_tombstoned_sibling_path_inside_base() {
    let table = base_after_subset_tombstone(
        (0..80).map(|index| format!("a{index:03}")).collect(),
        "azzz_live_with_longer_static_rank",
    );
    assert_mixed_delta_live_candidate_is_recalled(&table, "a");
}

#[test]
fn bounded_fuzzy_recall_skips_tombstoned_sibling_path_inside_base() {
    let table = base_after_subset_tombstone(
        (0..80)
            .map(|index| format!("pre_needle_old_{index:03}"))
            .collect(),
        "zz_needle_live_with_a_lexically_late_long_suffix",
    );
    assert_mixed_delta_live_candidate_is_recalled(&table, "needle");
}

fn base_with_active_head_stale_middle_and_active_tail(
    active_head: &str,
    stale_names: Vec<String>,
    active_tail: &str,
) -> NameTable {
    let live_path = "generated/live.h".to_string();
    let stale_path = "generated/stale.h".to_string();
    let mut entries = vec![
        (
            9_998,
            active_head.to_string(),
            false,
            live_path.clone(),
            "function".to_string(),
            false,
        ),
        (
            99_999,
            active_tail.to_string(),
            false,
            live_path,
            "function".to_string(),
            false,
        ),
    ];
    entries.extend(stale_names.into_iter().enumerate().map(|(index, name)| {
        (
            10_000 + index as i64,
            name,
            false,
            stale_path.clone(),
            "function".to_string(),
            false,
        )
    }));
    entries.extend((0..128).map(|index| {
        (
            i64::from(index),
            format!("zz_unrelated_{index:03}"),
            false,
            format!("src/base_{index:03}.c"),
            "function".to_string(),
            false,
        )
    }));
    NameTable::build_with_paths(entries).with_updated_paths(&HashSet::from([stale_path]), vec![])
}

#[test]
fn bounded_prefix_recall_crosses_a_stale_middle_after_an_active_base_head() {
    let table = base_with_active_head_stale_middle_and_active_tail(
        "aa000_live_head",
        (0..80)
            .map(|index| format!("aa100_old_{index:03}"))
            .collect(),
        "aazzz_live_target",
    );
    assert_mixed_delta_live_candidate_is_recalled(&table, "aa");
}

#[test]
fn bounded_short_prefix_recall_crosses_a_stale_middle_after_an_active_base_head() {
    let table = base_with_active_head_stale_middle_and_active_tail(
        "a0",
        (0..80)
            .map(|index| format!("a100_old_{index:03}"))
            .collect(),
        "azzz_live_target_with_longer_static_rank",
    );
    assert_mixed_delta_live_candidate_is_recalled(&table, "a");
}

#[test]
fn bounded_fuzzy_recall_crosses_a_stale_middle_after_an_active_base_head() {
    let table = base_with_active_head_stale_middle_and_active_tail(
        "needle",
        (0..80)
            .map(|index| format!("pre_needle_old_{index:03}"))
            .collect(),
        "zz_needle_live_target_with_a_longer_static_rank",
    );
    assert_mixed_delta_live_candidate_is_recalled(&table, "needle");
}

#[test]
fn bounded_prefix_recovery_does_not_spend_sources_on_unrelated_earlier_paths() {
    let stale_path = "middle/stale.h".to_string();
    let mut entries = (0..80)
        .map(|index| {
            (
                10_000 + i64::from(index),
                format!("aa_old_{index:03}"),
                false,
                stale_path.clone(),
                "function".to_string(),
                false,
            )
        })
        .collect::<Vec<_>>();
    entries.push((
        99_999,
        "aazzz_live".to_string(),
        false,
        "zzz/live.h".to_string(),
        "function".to_string(),
        false,
    ));
    entries.extend((0..32).map(|index| {
        (
            i64::from(index),
            format!("zz_unrelated_{index:03}"),
            false,
            format!("aaa/unrelated_{index:03}.h"),
            "function".to_string(),
            false,
        )
    }));
    let table = NameTable::build_with_paths(entries)
        .with_updated_paths(&HashSet::from([stale_path]), vec![]);

    assert_mixed_delta_live_candidate_is_recalled(&table, "aa");
}

#[test]
fn bounded_fuzzy_recovery_uses_token_matches_not_unrelated_path_heads() {
    let stale_path = "generated/stale.h".to_string();
    let live_path = "generated/live.h".to_string();
    let mut entries = (0..80)
        .map(|index| {
            (
                10_000 + i64::from(index),
                format!("pre_needle_old_{index:03}"),
                false,
                stale_path.clone(),
                "function".to_string(),
                false,
            )
        })
        .collect::<Vec<_>>();
    entries.extend((0..16).map(|index| {
        (
            5_000 + i64::from(index),
            format!("aa_unrelated_live_{index:03}"),
            false,
            live_path.clone(),
            "function".to_string(),
            false,
        )
    }));
    entries.push((
        99_999,
        "zz_needle_live_target".to_string(),
        false,
        live_path,
        "function".to_string(),
        false,
    ));
    entries.extend((0..96).map(|index| {
        (
            i64::from(index),
            format!("zz_workspace_noise_{index:03}"),
            false,
            format!("src/noise_{index:03}.c"),
            "function".to_string(),
            false,
        )
    }));
    let table = NameTable::build_with_paths(entries)
        .with_updated_paths(&HashSet::from([stale_path]), vec![]);

    assert_mixed_delta_live_candidate_is_recalled(&table, "needle");
}

#[test]
fn bounded_fuzzy_recovery_skips_stale_locals_for_a_multi_path_name() {
    let stale_path = "generated/stale.h".to_string();
    let live_path = "generated/live.h".to_string();
    let mut entries = (0..80)
        .map(|index| {
            (
                10_000 + i64::from(index),
                "pre_needle_shared".to_string(),
                false,
                stale_path.clone(),
                "function".to_string(),
                false,
            )
        })
        .collect::<Vec<_>>();
    entries.push((
        99_999,
        "pre_needle_shared".to_string(),
        false,
        live_path,
        "function".to_string(),
        false,
    ));
    entries.extend((0..128).map(|index| {
        (
            i64::from(index),
            format!("zz_workspace_noise_{index:03}"),
            false,
            format!("src/noise_{index:03}.c"),
            "function".to_string(),
            false,
        )
    }));
    let table = NameTable::build_with_paths(entries)
        .with_updated_paths(&HashSet::from([stale_path]), vec![]);

    assert_mixed_delta_live_candidate_is_recalled(&table, "needle");
}

#[test]
fn selected_project_recall_skips_tombstoned_sibling_path_inside_base() {
    use crate::project_context::{ProjectContext, ProjectContextIndex, ProjectKey};

    let key = ProjectKey {
        workspace_root_id: "root".to_string(),
        project_path: "selected".to_string(),
    };
    let projects = ProjectContextIndex::new(
        "root".to_string(),
        "workspace".to_string(),
        vec![ProjectContext {
            key: key.clone(),
            workspace_name: "workspace".to_string(),
            marker_files: vec!["selected/Makefile".to_string()],
        }],
    );
    let stale_path = "selected/000-stale.h".to_string();
    let mut names = (0..80)
        .map(|index| {
            (
                10_000 + i64::from(index),
                format!("aa_old_{index:03}"),
                false,
                stale_path.clone(),
                "function".to_string(),
                false,
            )
        })
        .collect::<Vec<_>>();
    names.push((
        99_999,
        "aazzz_project_live".to_string(),
        false,
        "selected/001-live.h".to_string(),
        "function".to_string(),
        false,
    ));
    names.extend((0..96).map(|index| {
        (
            i64::from(index),
            format!("aa_unrelated_{index:03}"),
            false,
            format!("zzz/global_{index:03}.h"),
            "function".to_string(),
            false,
        )
    }));
    let table = NameTable::build_with_paths_and_project_context(names, &projects)
        .with_updated_paths(&HashSet::from([stale_path]), vec![]);
    let cancellation = NeverCancelRecall;
    let (hits, _, metrics) =
        table.search_completion_recall_pooled_controlled(CompletionRecallQuery {
            query: "aa",
            quotas: CompletionRecallQuotas::with_project_context(10),
            scope: None,
            active_project: Some(&key),
            prior_pool: None,
            semantic_family: Some(SemanticFamily::CFamily),
            cancellation: Some(&cancellation),
            candidate_budget: 32,
        });

    assert!(
        hits.iter().any(|hit| hit.id == 99_999),
        "selected-project base recovery was pinned behind a tombstoned sibling: {metrics:?}"
    );
    assert_eq!(metrics.same_project, 1, "{metrics:?}");
    assert!(metrics.priority_source_attempts <= 4, "{metrics:?}");
    assert!(metrics.entries_inspected <= 32, "{metrics:?}");
}

#[test]
fn selected_project_recall_keeps_a_source_after_unmatched_scope_fanout() {
    use crate::project_context::{ProjectContext, ProjectContextIndex, ProjectKey};

    let key = ProjectKey {
        workspace_root_id: "root".to_string(),
        project_path: "selected".to_string(),
    };
    let projects = ProjectContextIndex::new(
        "root".to_string(),
        "workspace".to_string(),
        vec![ProjectContext {
            key: key.clone(),
            workspace_name: "workspace".to_string(),
            marker_files: vec!["selected/Makefile".to_string()],
        }],
    );
    let mut names = (0..64)
        .map(|index| {
            (
                i64::from(index),
                format!("aa000_global_{index:03}"),
                false,
                format!("global/{index:03}.h"),
                "function".to_string(),
                false,
            )
        })
        .collect::<Vec<_>>();
    names.extend((0..32).map(|index| {
        (
            1_000 + i64::from(index),
            format!("zz_scope_{index:03}"),
            false,
            format!("scope/{index:03}.h"),
            "function".to_string(),
            false,
        )
    }));
    names.push((
        99_999,
        "aazzz_project_target".to_string(),
        false,
        "selected/live.h".to_string(),
        "function".to_string(),
        false,
    ));
    let table = NameTable::build_with_paths_and_project_context(names, &projects);
    let scope = CompletionScope {
        current_path: None,
        reach: ReachScope {
            files: (0..32).map(|index| format!("scope/{index:03}.h")).collect(),
            heuristic_files: HashSet::new(),
            open: false,
            reason: None,
        },
        direct_external_files: HashSet::new(),
    };
    let cancellation = NeverCancelRecall;
    let (hits, _, metrics) =
        table.search_completion_recall_pooled_controlled(CompletionRecallQuery {
            query: "aa",
            quotas: CompletionRecallQuotas::with_project_context(10),
            scope: Some(&scope),
            active_project: Some(&key),
            prior_pool: None,
            semantic_family: Some(SemanticFamily::CFamily),
            cancellation: Some(&cancellation),
            candidate_budget: 32,
        });

    assert!(
        hits.iter().any(|hit| hit.id == 99_999),
        "unmatched scope fanout starved the selected-project source: {metrics:?}"
    );
    assert_eq!(metrics.same_project, 1, "{metrics:?}");
    assert!(metrics.priority_source_probes <= 32, "{metrics:?}");
    assert!(metrics.priority_source_attempts <= 4, "{metrics:?}");
    assert!(metrics.entries_inspected <= 32, "{metrics:?}");
}

#[test]
fn reachable_recall_keeps_a_source_after_selected_project_fanout() {
    use crate::project_context::{ProjectContext, ProjectContextIndex, ProjectKey};
    use crate::semantic_model::{SemanticDeclarationKind, SemanticDeclarationRole};
    use crate::store::views::DeclarationNameRow;

    let key = ProjectKey {
        workspace_root_id: "root".to_string(),
        project_path: "selected".to_string(),
    };
    let projects = ProjectContextIndex::new(
        "root".to_string(),
        "workspace".to_string(),
        vec![ProjectContext {
            key: key.clone(),
            workspace_name: "workspace".to_string(),
            marker_files: vec!["selected/Makefile".to_string()],
        }],
    );
    let mut rows = (0..64)
        .map(|index| DeclarationNameRow {
            id: i64::from(index),
            name: format!("aa000_global_{index:03}"),
            declaration_kind: SemanticDeclarationKind::Function,
            role: SemanticDeclarationRole::Definition,
            semantic_family: SemanticFamily::CFamily,
            path: format!("global/{index:03}.h"),
            external: false,
            directly_included: false,
        })
        .collect::<Vec<_>>();
    rows.push(DeclarationNameRow {
        id: 99_999,
        name: "aazzz_scope_target".to_string(),
        declaration_kind: SemanticDeclarationKind::Function,
        role: SemanticDeclarationRole::Definition,
        semantic_family: SemanticFamily::CFamily,
        path: "reachable/target.h".to_string(),
        external: false,
        directly_included: false,
    });
    rows.push(DeclarationNameRow {
        id: 10_000,
        name: "aa_project_base".to_string(),
        declaration_kind: SemanticDeclarationKind::Function,
        role: SemanticDeclarationRole::Definition,
        semantic_family: SemanticFamily::CFamily,
        path: "selected/base.h".to_string(),
        external: false,
        directly_included: false,
    });
    let mut table =
        NameTable::build_from_declaration_name_rows_with_project_context(rows, Some(&projects));
    for index in 0..3 {
        let path = format!("selected/delta_{index}.h");
        table = table.with_updated_declaration_name_rows_with_project_context(
            &HashSet::from([path.clone()]),
            vec![DeclarationNameRow {
                id: 10_001 + i64::from(index),
                name: format!("aa_project_delta_{index}"),
                declaration_kind: SemanticDeclarationKind::Function,
                role: SemanticDeclarationRole::Definition,
                semantic_family: SemanticFamily::CFamily,
                path,
                external: false,
                directly_included: false,
            }],
            Some(&projects),
        );
    }
    let scope = CompletionScope {
        current_path: Some("reachable/current.c".to_string()),
        reach: ReachScope {
            files: HashSet::from(["reachable/target.h".to_string()]),
            heuristic_files: HashSet::new(),
            open: false,
            reason: None,
        },
        direct_external_files: HashSet::new(),
    };
    let cancellation = NeverCancelRecall;
    let (hits, _, metrics) =
        table.search_completion_recall_pooled_controlled(CompletionRecallQuery {
            query: "aa",
            quotas: CompletionRecallQuotas::with_project_context(10),
            scope: Some(&scope),
            active_project: Some(&key),
            prior_pool: None,
            semantic_family: Some(SemanticFamily::CFamily),
            cancellation: Some(&cancellation),
            candidate_budget: 32,
        });

    assert!(
        hits.iter().any(|hit| hit.id == 99_999),
        "selected-project fanout starved the reachable source: {metrics:?}"
    );
    assert_eq!(metrics.reachable, 1, "{metrics:?}");
    assert!(metrics.priority_source_probes <= 32, "{metrics:?}");
    assert!(metrics.priority_source_attempts <= 4, "{metrics:?}");
    assert!(metrics.entries_inspected <= 32, "{metrics:?}");
}

#[test]
fn selected_project_recall_keeps_candidate_budget_after_long_scope_cursor() {
    use crate::project_context::{ProjectContext, ProjectContextIndex, ProjectKey};

    let key = ProjectKey {
        workspace_root_id: "root".to_string(),
        project_path: "selected".to_string(),
    };
    let projects = ProjectContextIndex::new(
        "root".to_string(),
        "workspace".to_string(),
        vec![ProjectContext {
            key: key.clone(),
            workspace_name: "workspace".to_string(),
            marker_files: vec!["selected/Makefile".to_string()],
        }],
    );
    let mut names = (0..100)
        .map(|index| {
            (
                i64::from(index),
                format!("aa000_scope_{index:03}"),
                false,
                "reachable/many.h".to_string(),
                "function".to_string(),
                false,
            )
        })
        .collect::<Vec<_>>();
    names.extend((0..64).map(|index| {
        (
            1_000 + i64::from(index),
            format!("aa001_global_{index:03}"),
            false,
            format!("global/{index:03}.h"),
            "function".to_string(),
            false,
        )
    }));
    names.push((
        99_999,
        "aazzz_project_target".to_string(),
        false,
        "selected/live.h".to_string(),
        "function".to_string(),
        false,
    ));
    let table = NameTable::build_with_paths_and_project_context(names, &projects);
    let scope = CompletionScope {
        current_path: Some("reachable/current.c".to_string()),
        reach: ReachScope {
            files: HashSet::from(["reachable/many.h".to_string()]),
            heuristic_files: HashSet::new(),
            open: false,
            reason: None,
        },
        direct_external_files: HashSet::new(),
    };
    let cancellation = NeverCancelRecall;
    let (hits, _, metrics) =
        table.search_completion_recall_pooled_controlled(CompletionRecallQuery {
            query: "aa",
            quotas: CompletionRecallQuotas::with_project_context(10),
            scope: Some(&scope),
            active_project: Some(&key),
            prior_pool: None,
            semantic_family: Some(SemanticFamily::CFamily),
            cancellation: Some(&cancellation),
            candidate_budget: 32,
        });

    assert!(
        hits.iter().any(|hit| hit.id == 99_999),
        "a long scope cursor consumed the selected-project candidate share: {metrics:?}"
    );
    assert_eq!(metrics.same_project, 1, "{metrics:?}");
    assert!(metrics.entries_inspected <= 32, "{metrics:?}");
}

#[test]
fn project_posting_carries_its_segment_id_for_constant_time_prefix_recovery() {
    use crate::project_context::{ProjectContext, ProjectContextIndex, ProjectKey};

    let contexts = (0..256)
        .map(|index| ProjectContext {
            key: ProjectKey {
                workspace_root_id: "root".to_string(),
                project_path: format!("projects/{index:03}"),
            },
            workspace_name: "workspace".to_string(),
            marker_files: vec![format!("projects/{index:03}/Makefile")],
        })
        .collect::<Vec<_>>();
    let selected = contexts
        .last()
        .expect("contexts must not be empty")
        .key
        .clone();
    let projects = ProjectContextIndex::new("root".to_string(), "workspace".to_string(), contexts);
    let table = NameTable::build_with_paths_and_project_context(
        (0..256)
            .map(|index| {
                (
                    i64::from(index),
                    format!("aa_project_{index:03}"),
                    false,
                    format!("projects/{index:03}/decl.h"),
                    "function".to_string(),
                    false,
                )
            })
            .collect(),
        &projects,
    );
    let posting = table
        .base
        .by_project
        .get(&selected)
        .expect("selected project posting must exist");

    assert_eq!(
        table.base.projects[posting.project_id as usize], selected,
        "prefix recovery must not linearly search every segment project key"
    );
}

#[test]
fn priority_source_setup_deduplicates_scope_and_mixed_segment_recovery() {
    let table = mixed_delta_after_subset_tombstone(
        (0..80).map(|index| format!("aa_old_{index:03}")).collect(),
        "aazzz_live",
    );
    let scope = CompletionScope {
        current_path: Some("generated/live.h".to_string()),
        reach: ReachScope {
            files: HashSet::from(["generated/live.h".to_string()]),
            heuristic_files: HashSet::new(),
            open: false,
            reason: None,
        },
        direct_external_files: HashSet::new(),
    };
    let cancellation = NeverCancelRecall;
    let (hits, _, metrics) =
        table.search_completion_recall_pooled_controlled(CompletionRecallQuery {
            query: "aa",
            quotas: CompletionRecallQuotas::default_for_completion_limit(10),
            scope: Some(&scope),
            active_project: None,
            prior_pool: None,
            semantic_family: Some(SemanticFamily::CFamily),
            cancellation: Some(&cancellation),
            candidate_budget: 32,
        });

    assert!(hits.iter().any(|hit| hit.id == 99_999), "{metrics:?}");
    assert_eq!(metrics.priority_source_attempts, 1, "{metrics:?}");
    assert_eq!(metrics.priority_sources_initialized, 1, "{metrics:?}");
}

#[test]
fn priority_source_setup_observes_cancellation_before_scope_fanout() {
    let names = (0..100)
        .map(|index| {
            (
                i64::from(index),
                format!("api_{index:03}"),
                false,
                format!("scope/{index:03}.h"),
                "function".to_string(),
                false,
            )
        })
        .collect::<Vec<_>>();
    let scope = CompletionScope {
        current_path: None,
        reach: ReachScope {
            files: (0..100)
                .map(|index| format!("scope/{index:03}.h"))
                .collect(),
            heuristic_files: HashSet::new(),
            open: false,
            reason: None,
        },
        direct_external_files: HashSet::new(),
    };
    let table = NameTable::build_with_paths(names);
    let cancellation = CancelOnRecallCheck {
        checks: AtomicUsize::new(0),
        cancel_on: 1,
    };
    let (hits, pool, metrics) =
        table.search_completion_recall_pooled_controlled(CompletionRecallQuery {
            query: "api",
            quotas: CompletionRecallQuotas::default_for_completion_limit(10),
            scope: Some(&scope),
            active_project: None,
            prior_pool: None,
            semantic_family: Some(SemanticFamily::CFamily),
            cancellation: Some(&cancellation),
            candidate_budget: 32,
        });

    assert!(metrics.cancelled, "{metrics:?}");
    assert_eq!(metrics.priority_source_attempts, 0, "{metrics:?}");
    assert_eq!(metrics.entries_inspected, 0, "{metrics:?}");
    assert!(hits.is_empty());
    assert!(pool.is_empty());

    let never_cancel = NeverCancelRecall;
    let (_, _, bounded_metrics) =
        table.search_completion_recall_pooled_controlled(CompletionRecallQuery {
            query: "api",
            quotas: CompletionRecallQuotas::default_for_completion_limit(10),
            scope: Some(&scope),
            active_project: None,
            prior_pool: None,
            semantic_family: Some(SemanticFamily::CFamily),
            cancellation: Some(&never_cancel),
            candidate_budget: 32,
        });
    assert_eq!(
        bounded_metrics.priority_source_attempts, 4,
        "{bounded_metrics:?}"
    );
    assert_eq!(
        bounded_metrics.priority_sources_initialized, 4,
        "{bounded_metrics:?}"
    );
}

#[test]
fn priority_recovery_counts_inactive_path_probes_and_cancels_cooperatively() {
    let stale_paths = (0..600)
        .map(|index| format!("aaa/stale_{index:03}.h"))
        .collect::<HashSet<_>>();
    let mut names = stale_paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            (
                index as i64,
                format!("aa_old_{index:03}"),
                false,
                path.clone(),
                "function".to_string(),
                false,
            )
        })
        .collect::<Vec<_>>();
    names.push((
        99_999,
        "aazzz_live_target".to_string(),
        false,
        "zzz/live.h".to_string(),
        "function".to_string(),
        false,
    ));
    let table = NameTable::build_with_paths(names).with_updated_paths(&stale_paths, vec![]);
    let never_cancel = NeverCancelRecall;
    let (_, _, bounded_metrics) =
        table.search_completion_recall_pooled_controlled(CompletionRecallQuery {
            query: "aa",
            quotas: CompletionRecallQuotas::default_for_completion_limit(10),
            scope: None,
            active_project: None,
            prior_pool: None,
            semantic_family: Some(SemanticFamily::CFamily),
            cancellation: Some(&never_cancel),
            candidate_budget: 32,
        });
    assert_eq!(
        bounded_metrics.priority_source_probes, 32,
        "inactive path-pair examination must consume the bounded metadata budget: {bounded_metrics:?}"
    );
    assert!(bounded_metrics.truncated, "{bounded_metrics:?}");

    let cancellation = CancelOnRecallCheck {
        checks: AtomicUsize::new(0),
        cancel_on: 2,
    };
    let (hits, pool, cancelled_metrics) =
        table.search_completion_recall_pooled_controlled(CompletionRecallQuery {
            query: "aa",
            quotas: CompletionRecallQuotas::default_for_completion_limit(10),
            scope: None,
            active_project: None,
            prior_pool: None,
            semantic_family: Some(SemanticFamily::CFamily),
            cancellation: Some(&cancellation),
            candidate_budget: 512,
        });
    assert!(cancelled_metrics.cancelled, "{cancelled_metrics:?}");
    assert_eq!(
        cancelled_metrics.priority_source_probes, 256,
        "recovery metadata must check cancellation every probe block: {cancelled_metrics:?}"
    );
    assert_eq!(cancellation.checks.load(Ordering::Relaxed), 2);
    assert!(hits.is_empty());
    assert!(pool.is_empty());
}

#[test]
fn priority_scope_counts_missing_path_probes_and_cancels_cooperatively() {
    let table = NameTable::build_with_paths(
        (0..600)
            .map(|index| {
                (
                    i64::from(index),
                    format!("aa_global_{index:03}"),
                    false,
                    format!("global/{index:03}.h"),
                    "function".to_string(),
                    false,
                )
            })
            .collect(),
    );
    let scope = CompletionScope {
        current_path: None,
        reach: ReachScope {
            files: (0..600)
                .map(|index| format!("missing/{index:03}.h"))
                .collect(),
            heuristic_files: HashSet::new(),
            open: false,
            reason: None,
        },
        direct_external_files: HashSet::new(),
    };
    let cancellation = CancelOnRecallCheck {
        checks: AtomicUsize::new(0),
        cancel_on: 2,
    };
    let (hits, pool, metrics) =
        table.search_completion_recall_pooled_controlled(CompletionRecallQuery {
            query: "aa",
            quotas: CompletionRecallQuotas::default_for_completion_limit(10),
            scope: Some(&scope),
            active_project: None,
            prior_pool: None,
            semantic_family: Some(SemanticFamily::CFamily),
            cancellation: Some(&cancellation),
            candidate_budget: 512,
        });

    assert!(metrics.cancelled, "{metrics:?}");
    assert_eq!(metrics.priority_source_probes, 256, "{metrics:?}");
    assert_eq!(metrics.entries_inspected, 0, "{metrics:?}");
    assert_eq!(cancellation.checks.load(Ordering::Relaxed), 2);
    assert!(hits.is_empty());
    assert!(pool.is_empty());
}

#[test]
fn priority_candidate_deferral_checks_cancellation_before_heap_fanout() {
    use crate::project_context::{ProjectContext, ProjectContextIndex, ProjectKey};

    let key = ProjectKey {
        workspace_root_id: "root".to_string(),
        project_path: "selected".to_string(),
    };
    let projects = ProjectContextIndex::new(
        "root".to_string(),
        "workspace".to_string(),
        vec![ProjectContext {
            key: key.clone(),
            workspace_name: "workspace".to_string(),
            marker_files: vec!["selected/Makefile".to_string()],
        }],
    );
    let mut names = Vec::with_capacity(4_515);
    for index in 0..257 {
        let path = format!("scope/{index:03}.h");
        names.push((
            i64::from(index) * 2,
            format!("aa000_head_{index:03}"),
            false,
            path.clone(),
            "function".to_string(),
            false,
        ));
        names.push((
            i64::from(index) * 2 + 1,
            format!("aa999_tail_{index:03}"),
            false,
            path,
            "function".to_string(),
            false,
        ));
    }
    names.extend((0..4_000).map(|index| {
        (
            10_000 + i64::from(index),
            format!("zz_global_{index:04}"),
            false,
            format!("global/{index:04}.h"),
            "function".to_string(),
            false,
        )
    }));
    names.push((
        99_999,
        "aazzz_project_target".to_string(),
        false,
        "selected/live.h".to_string(),
        "function".to_string(),
        false,
    ));
    let table = NameTable::build_with_paths_and_project_context(names, &projects);
    let scope = CompletionScope {
        current_path: None,
        reach: ReachScope {
            files: (0..257)
                .map(|index| format!("scope/{index:03}.h"))
                .collect(),
            heuristic_files: HashSet::new(),
            open: false,
            reason: None,
        },
        direct_external_files: HashSet::new(),
    };
    let cancellation = CancelOnRecallCheck {
        checks: AtomicUsize::new(0),
        cancel_on: 5,
    };
    let (hits, pool, metrics) =
        table.search_completion_recall_pooled_controlled(CompletionRecallQuery {
            query: "aa",
            quotas: CompletionRecallQuotas::with_project_context(10),
            scope: Some(&scope),
            active_project: Some(&key),
            prior_pool: None,
            semantic_family: Some(SemanticFamily::CFamily),
            cancellation: Some(&cancellation),
            candidate_budget: 4_112,
        });

    assert!(metrics.cancelled, "{metrics:?}");
    assert_eq!(
        metrics.entries_inspected, 257,
        "saturated cursor deferral must observe cancellation before moving hundreds of heap heads: {metrics:?}"
    );
    assert_eq!(cancellation.checks.load(Ordering::Relaxed), 5);
    assert!(hits.is_empty());
    assert!(pool.is_empty());

    let counted = CountRecallChecks {
        checks: AtomicUsize::new(0),
    };
    let (_, _, counted_metrics) =
        table.search_completion_recall_pooled_controlled(CompletionRecallQuery {
            query: "aa",
            quotas: CompletionRecallQuotas::with_project_context(10),
            scope: Some(&scope),
            active_project: Some(&key),
            prior_pool: None,
            semantic_family: Some(SemanticFamily::CFamily),
            cancellation: Some(&counted),
            candidate_budget: 4_096,
        });
    assert!(
        counted_metrics.cancellation_checks < 64,
        "a saturated 256-row share must not poll cancellation once per deferred cursor: {counted_metrics:?}"
    );
    assert_eq!(
        counted_metrics.cancellation_checks,
        counted.checks.load(Ordering::Relaxed)
    );
}

#[test]
fn bounded_recall_scores_each_index_once_across_priority_and_global_sources() {
    use crate::project_context::{ProjectContext, ProjectContextIndex, ProjectKey};

    let key = ProjectKey {
        workspace_root_id: "root".to_string(),
        project_path: "selected".to_string(),
    };
    let projects = ProjectContextIndex::new(
        "root".to_string(),
        "workspace".to_string(),
        vec![ProjectContext {
            key: key.clone(),
            workspace_name: "workspace".to_string(),
            marker_files: vec!["selected/Makefile".to_string()],
        }],
    );
    let mut names = (0..64)
        .map(|index| {
            (
                i64::from(index),
                format!("aa_global_{index:03}"),
                false,
                format!("global/{index:03}.h"),
                "function".to_string(),
                false,
            )
        })
        .collect::<Vec<_>>();
    names.push((
        99_999,
        "aa000_selected_reachable".to_string(),
        false,
        "selected/live.h".to_string(),
        "function".to_string(),
        false,
    ));
    let table = NameTable::build_with_paths_and_project_context(names, &projects);
    let scope = CompletionScope {
        current_path: Some("selected/current.c".to_string()),
        reach: ReachScope {
            files: HashSet::from(["selected/live.h".to_string()]),
            heuristic_files: HashSet::new(),
            open: false,
            reason: None,
        },
        direct_external_files: HashSet::new(),
    };
    let cancellation = NeverCancelRecall;
    let (hits, pool, metrics) =
        table.search_completion_recall_pooled_controlled(CompletionRecallQuery {
            query: "aa",
            quotas: CompletionRecallQuotas::with_project_context(10),
            scope: Some(&scope),
            active_project: Some(&key),
            prior_pool: None,
            semantic_family: Some(SemanticFamily::CFamily),
            cancellation: Some(&cancellation),
            candidate_budget: 32,
        });

    let unique_pool = pool.iter().copied().collect::<HashSet<_>>();
    assert_eq!(
        pool.len(),
        unique_pool.len(),
        "one declaration was scored repeatedly across project/scope/global sources: {metrics:?}"
    );
    assert_eq!(
        hits.iter().filter(|hit| hit.id == 99_999).count(),
        1,
        "selected/reachable candidate must remain unique: {metrics:?}"
    );
}

#[test]
fn bounded_fuzzy_recall_preserves_boundary_initial_candidates() {
    let mut names: Vec<_> = (0..1_000)
        .map(|index| {
            (
                i64::from(index),
                format!("unrelated_workspace_candidate_{index:04}"),
                false,
            )
        })
        .collect();
    names.extend((0..20).map(|index| {
        (
            10_000 + i64::from(index),
            format!("DeviceBindDriverToNode_{index:02}"),
            false,
        )
    }));
    let table = NameTable::build(names);
    let quotas = CompletionRecallQuotas::default_for_completion_limit(20);
    let oracle = table.search_completion_recall_pooled("dbdtn", quotas, None, None);
    let cancellation = NeverCancelRecall;
    let bounded = table.search_completion_recall_pooled_controlled(CompletionRecallQuery {
        query: "dbdtn",
        quotas,
        scope: None,
        active_project: None,
        prior_pool: None,
        semantic_family: Some(SemanticFamily::CFamily),
        cancellation: Some(&cancellation),
        candidate_budget: 128,
    });

    assert_eq!(
        bounded
            .0
            .iter()
            .take(20)
            .map(|hit| hit.id)
            .collect::<Vec<_>>(),
        oracle
            .0
            .iter()
            .take(20)
            .map(|hit| hit.id)
            .collect::<Vec<_>>()
    );
    assert!(bounded.2.truncated);
    assert!(bounded.2.entries_inspected <= 128, "{:?}", bounded.2);
}

#[test]
fn bounded_fuzzy_recall_preserves_contiguous_substring_candidates() {
    let mut names: Vec<_> = (0..1_000)
        .map(|index| {
            (
                i64::from(index),
                format!("unrelated_workspace_candidate_{index:04}"),
                false,
            )
        })
        .collect();
    names.extend((0..20).map(|index| {
        (
            20_000 + i64::from(index),
            format!("pre_needle_chunk_{index:02}"),
            false,
        )
    }));
    let table = NameTable::build(names);
    let quotas = CompletionRecallQuotas::default_for_completion_limit(20);
    let oracle = table.search_completion_recall_pooled("needle", quotas, None, None);
    let cancellation = NeverCancelRecall;
    let bounded = table.search_completion_recall_pooled_controlled(CompletionRecallQuery {
        query: "needle",
        quotas,
        scope: None,
        active_project: None,
        prior_pool: None,
        semantic_family: Some(SemanticFamily::CFamily),
        cancellation: Some(&cancellation),
        candidate_budget: 128,
    });

    assert_eq!(
        bounded
            .0
            .iter()
            .take(20)
            .map(|hit| hit.id)
            .collect::<Vec<_>>(),
        oracle
            .0
            .iter()
            .take(20)
            .map(|hit| hit.id)
            .collect::<Vec<_>>()
    );
    assert!(bounded.2.truncated);
    assert!(bounded.2.entries_inspected <= 128, "{:?}", bounded.2);
}

#[test]
fn bounded_fuzzy_name_posting_expansion_cannot_escape_candidate_budget() {
    let mut names: Vec<_> = (0..1_000)
        .map(|index| {
            (
                i64::from(index),
                "pre_needle_chunk".to_string(),
                false,
                format!("src/duplicate_{index:04}.c"),
                "function".to_string(),
                false,
            )
        })
        .collect();
    names.extend((0..1_000).map(|index| {
        (
            10_000 + i64::from(index),
            format!("unrelated_workspace_candidate_{index:04}"),
            false,
            format!("src/noise_{index:04}.c"),
            "function".to_string(),
            false,
        )
    }));
    let table = NameTable::build_with_paths(names);
    let cancellation = NeverCancelRecall;

    let (hits, _, metrics) =
        table.search_completion_recall_pooled_controlled(CompletionRecallQuery {
            query: "needle",
            quotas: CompletionRecallQuotas::default_for_completion_limit(100),
            scope: None,
            active_project: None,
            prior_pool: None,
            semantic_family: Some(SemanticFamily::CFamily),
            cancellation: Some(&cancellation),
            candidate_budget: 64,
        });

    assert!(!hits.is_empty());
    assert!(hits.iter().all(|hit| hit.name == "pre_needle_chunk"));
    assert!(metrics.truncated, "{metrics:?}");
    assert!(
        metrics.entries_inspected <= 64,
        "one compact name posting expanded beyond the request budget: {metrics:?}"
    );
    assert!(
        metrics.fuzzy_entries_inspected <= metrics.entries_inspected,
        "fuzzy expansion accounting diverged: {metrics:?}"
    );
}

#[test]
fn bounded_fuzzy_postings_respect_delta_shadow_and_tombstone() {
    let mut names: Vec<_> = (0..200)
        .map(|index| {
            (
                i64::from(index),
                format!("unrelated_workspace_candidate_{index:04}"),
                false,
                format!("src/noise_{index:04}.c"),
                "function".to_string(),
                false,
            )
        })
        .collect();
    names.push((
        1_000,
        "pre_needle_old".to_string(),
        false,
        "src/changed.c".to_string(),
        "function".to_string(),
        false,
    ));
    let table = NameTable::build_with_paths(names);
    let changed = HashSet::from(["src/changed.c".to_string()]);
    let updated = table.with_updated_paths(
        &changed,
        vec![(
            2_000,
            "pre_replacement_new".to_string(),
            false,
            "src/changed.c".to_string(),
            "function".to_string(),
            false,
        )],
    );
    let cancellation = NeverCancelRecall;
    let recall = |table: &NameTable| {
        table.search_completion_recall_pooled_controlled(CompletionRecallQuery {
            query: "replacement",
            quotas: CompletionRecallQuotas::default_for_completion_limit(10),
            scope: None,
            active_project: None,
            prior_pool: None,
            semantic_family: Some(SemanticFamily::CFamily),
            cancellation: Some(&cancellation),
            candidate_budget: 32,
        })
    };

    let (hits, _, metrics) = recall(&updated);
    assert!(hits.iter().any(|hit| hit.id == 2_000), "{hits:?}");
    assert!(hits.iter().all(|hit| hit.id != 1_000));
    assert!(metrics.entries_inspected <= 32, "{metrics:?}");

    let deleted = updated.with_updated_paths(&changed, vec![]);
    let (hits, _, metrics) = recall(&deleted);
    assert!(hits.iter().all(|hit| hit.id != 1_000 && hit.id != 2_000));
    assert!(metrics.entries_inspected <= 32, "{metrics:?}");
}

#[test]
fn bounded_fuzzy_budget_cannot_be_consumed_by_shadowed_base_rows() {
    let stale_path = "src/generated_api.h".to_string();
    let table = NameTable::build_with_paths(
        (0..80)
            .map(|index| {
                (
                    i64::from(index),
                    format!("pre_needle_old_{index:03}"),
                    false,
                    stale_path.clone(),
                    "function".to_string(),
                    false,
                )
            })
            .collect(),
    );
    let updated = table.with_updated_paths(
        &HashSet::from([stale_path.clone()]),
        vec![(
            10_000,
            "zz_needle_live_with_a_lexically_late_long_suffix".to_string(),
            false,
            stale_path,
            "function".to_string(),
            false,
        )],
    );
    let cancellation = NeverCancelRecall;
    let (hits, _, metrics) =
        updated.search_completion_recall_pooled_controlled(CompletionRecallQuery {
            query: "needle",
            quotas: CompletionRecallQuotas::default_for_completion_limit(10),
            scope: None,
            active_project: None,
            prior_pool: None,
            semantic_family: Some(SemanticFamily::CFamily),
            cancellation: Some(&cancellation),
            candidate_budget: 32,
        });

    assert!(
        hits.iter().any(|hit| hit.id == 10_000),
        "the active fuzzy posting was starved by stale base rows: {metrics:?}"
    );
    assert!(metrics.entries_inspected <= 32, "{metrics:?}");
}

#[test]
fn bounded_recall_reserves_work_for_reachable_prefix_candidates() {
    let mut names: Vec<_> = (0..200)
        .map(|index| {
            (
                i64::from(index),
                format!("api_aaa_global_{index:03}"),
                false,
                format!("global/{index:03}.h"),
                "function".to_string(),
                false,
            )
        })
        .collect();
    names.push((
        10_000,
        "api_zzz_reachable_target".to_string(),
        false,
        "reachable/api.h".to_string(),
        "function".to_string(),
        false,
    ));
    let table = NameTable::build_with_paths(names);
    let scope = CompletionScope {
        current_path: Some("src/main.c".to_string()),
        reach: ReachScope {
            files: HashSet::from(["reachable/api.h".to_string()]),
            heuristic_files: HashSet::new(),
            open: false,
            reason: None,
        },
        direct_external_files: HashSet::new(),
    };
    let cancellation = NeverCancelRecall;
    let (hits, _, metrics) =
        table.search_completion_recall_pooled_controlled(CompletionRecallQuery {
            query: "api",
            quotas: CompletionRecallQuotas::default_for_completion_limit(10),
            scope: Some(&scope),
            active_project: None,
            prior_pool: None,
            semantic_family: Some(SemanticFamily::CFamily),
            cancellation: Some(&cancellation),
            candidate_budget: 32,
        });

    let target = hits
        .iter()
        .find(|hit| hit.id == 10_000)
        .expect("reachable candidate must have its own bounded recall channel");
    assert_eq!(target.tier, ScopeTier::Reachable);
    assert!(metrics.entries_inspected <= 32, "{metrics:?}");
}

#[test]
fn bounded_recall_reserves_work_for_selected_project_prefix_candidates() {
    use crate::project_context::{ProjectContext, ProjectContextIndex, ProjectKey};

    let key = ProjectKey {
        workspace_root_id: "root".to_string(),
        project_path: "selected".to_string(),
    };
    let projects = ProjectContextIndex::new(
        "root".to_string(),
        "workspace".to_string(),
        vec![ProjectContext {
            key: key.clone(),
            workspace_name: "workspace".to_string(),
            marker_files: vec!["selected/Makefile".to_string()],
        }],
    );
    let mut names: Vec<_> = (0..200)
        .map(|index| {
            (
                i64::from(index),
                format!("api_aaa_global_{index:03}"),
                false,
                format!("global/{index:03}.h"),
                "function".to_string(),
                false,
            )
        })
        .collect();
    names.push((
        10_000,
        "api_zzz_project_target".to_string(),
        false,
        "selected/api.h".to_string(),
        "function".to_string(),
        false,
    ));
    let table = NameTable::build_with_paths_and_project_context(names, &projects);
    let cancellation = NeverCancelRecall;
    let (hits, _, metrics) =
        table.search_completion_recall_pooled_controlled(CompletionRecallQuery {
            query: "api",
            quotas: CompletionRecallQuotas::with_project_context(10),
            scope: None,
            active_project: Some(&key),
            prior_pool: None,
            semantic_family: Some(SemanticFamily::CFamily),
            cancellation: Some(&cancellation),
            candidate_budget: 32,
        });

    assert!(
        hits.iter().any(|hit| hit.id == 10_000),
        "selected-project candidate must have its own bounded recall channel: {metrics:?}"
    );
    assert_eq!(metrics.same_project, 1);
    assert!(metrics.entries_inspected <= 32, "{metrics:?}");
}

#[test]
fn bounded_completion_recall_has_a_hard_budget_and_keeps_full_query_matching() {
    let mut names: Vec<_> = (0..50_000)
        .map(|index| {
            (
                i64::from(index),
                format!("zz_workspace_candidate_{index:05}"),
                false,
            )
        })
        .collect();
    names.push((60_001, "needle_target".to_string(), false));
    names.push((60_002, "needle_targe_noise".to_string(), false));
    let table = NameTable::build(names);
    let quotas = CompletionRecallQuotas::default_for_completion_limit(10);
    let cancellation = NeverCancelRecall;

    let (hits, pool, metrics) =
        table.search_completion_recall_pooled_controlled(CompletionRecallQuery {
            query: "needle_target",
            quotas,
            scope: None,
            active_project: None,
            prior_pool: None,
            semantic_family: None,
            cancellation: Some(&cancellation),
            candidate_budget: 128,
        });

    assert!(
        metrics.truncated,
        "large cold recall must expose truncation"
    );
    assert_eq!(metrics.active_entries_total, table.len());
    assert_eq!(metrics.candidate_budget, 128);
    assert!(
        metrics.entries_inspected <= metrics.candidate_budget,
        "candidate generation exceeded its hard budget: {metrics:?}"
    );
    assert!(metrics.prefix_entries_inspected >= 1);
    assert!(hits.iter().any(|hit| hit.id == 60_001));
    assert!(pool
        .iter()
        .any(|index| table.active_entry(*index).id == 60_001));
    assert!(
        hits.iter().all(|hit| hit.id != 60_002),
        "the final matcher must consume the query's trailing character"
    );
}

#[test]
fn same_project_quota_adds_a_representative_without_filtering_global() {
    use crate::project_context::{ProjectContext, ProjectContextIndex, ProjectKey};

    let root_id = "root".to_string();
    let key = ProjectKey {
        workspace_root_id: root_id.clone(),
        project_path: "selected".to_string(),
    };
    let projects = ProjectContextIndex::new(
        root_id,
        "workspace".to_string(),
        vec![ProjectContext {
            key: key.clone(),
            workspace_name: "workspace".to_string(),
            marker_files: vec!["Makefile".to_string()],
        }],
    );
    let table = NameTable::build_with_paths_and_project_context(
        vec![
            (
                1,
                "api_alpha".to_string(),
                false,
                "other/a.c".to_string(),
                "function".to_string(),
                false,
            ),
            (
                2,
                "api_selected".to_string(),
                false,
                "selected/z.c".to_string(),
                "function".to_string(),
                false,
            ),
        ],
        &projects,
    );
    let quotas = CompletionRecallQuotas {
        total_indexed: 2,
        reachable: 0,
        external: 0,
        unknown: 0,
        global: 1,
        same_project: 1,
    };
    let (hits, _, metrics) =
        table.search_completion_recall_pooled_with_project("api", quotas, None, Some(&key), None);

    assert_eq!(hits.len(), 2);
    assert!(hits.iter().any(|hit| hit.name == "api_alpha"));
    assert!(hits.iter().any(|hit| hit.name == "api_selected"));
    assert_eq!(metrics.same_project, 1);
}

#[test]
fn locality_breaks_ties_without_dropping() {
    let table = NameTable::build_with_paths(vec![
        (
            1,
            "widget_a".to_string(),
            false,
            "src/sub/here.c".to_string(),
            "function".to_string(),
            false,
        ),
        (
            2,
            "widget_b".to_string(),
            false,
            "far/away.c".to_string(),
            "function".to_string(),
            false,
        ),
    ]);
    // Both reachable (same tier); widget_a shares more path with the current
    // file, so it edges ahead — and nothing is dropped.
    let sc = scope("src/sub/main.c", &["src/sub/here.c", "far/away.c"], false);
    let hits = table.search_ranked_scoped("widget", 10, Some(&sc));
    assert_eq!(hits.len(), 2, "locality never filters");
    assert_eq!(hits[0].name, "widget_a", "closer file ranks first");
}

#[test]
fn name_table_ranks_first_layer_external_above_global_workspace() {
    // R2: a first-layer external (External tier) outranks a global
    // workspace symbol (Global tier) of the same name, regardless of fuzzy
    // quality. (Renamed from `name_table_ranks_workspace_before_external`;
    // the old "workspace before external" rule is reversed by strict-tier
    // ordering for first-layer externals.)
    let table = NameTable::build_with_paths(vec![
        (
            1,
            "Frobnicate".to_string(),
            true, // external
            "C:/toolchain/include/frob.h".to_string(),
            "function".to_string(),
            true, // directly_included → first-layer external
        ),
        (
            2,
            "Frobnicate".to_string(),
            false, // workspace
            "src/util.c".to_string(),
            "function".to_string(),
            false,
        ),
    ]);
    let hits = table.search_ranked("Frobnicate", 10);
    assert_eq!(
        hits.first().map(|h| h.id),
        Some(1),
        "first-layer external outranks global workspace"
    );
    assert_eq!(hits[0].tier, crate::model::ScopeTier::External);
    assert_eq!(hits[1].tier, crate::model::ScopeTier::Global);
}

#[test]
fn name_table_replaces_entries_for_dirty_paths() {
    let table = NameTable::build_with_paths(vec![
        (
            1,
            "old_name".to_string(),
            false,
            "src/a.c".to_string(),
            "function".to_string(),
            false,
        ),
        (
            2,
            "keep_name".to_string(),
            false,
            "src/b.c".to_string(),
            "function".to_string(),
            false,
        ),
    ]);
    let paths = std::collections::HashSet::from(["src/a.c".to_string()]);
    let updated = table.with_updated_paths(
        &paths,
        vec![(
            3,
            "new_name".to_string(),
            false,
            "src/a.c".to_string(),
            "function".to_string(),
            false,
        )],
    );

    assert!(updated.search("old", 10).is_empty());
    assert_eq!(updated.search("new", 10), vec![3]);
    assert_eq!(updated.search("keep", 10), vec![2]);
}

#[test]
fn name_table_repeated_segments_shadow_then_tombstone_one_path() {
    let table = NameTable::build_with_paths(vec![
        (
            1,
            "old_name".to_string(),
            false,
            "src/a.c".to_string(),
            "function".to_string(),
            false,
        ),
        (
            2,
            "keep_name".to_string(),
            false,
            "src/b.c".to_string(),
            "function".to_string(),
            false,
        ),
    ]);
    let paths = std::collections::HashSet::from(["src/a.c".to_string()]);
    let first = table.with_updated_paths(
        &paths,
        vec![(
            3,
            "first_delta".to_string(),
            false,
            "src/a.c".to_string(),
            "function".to_string(),
            false,
        )],
    );
    let second = first.with_updated_paths(
        &paths,
        vec![(
            4,
            "second_delta".to_string(),
            false,
            "src/a.c".to_string(),
            "function".to_string(),
            false,
        )],
    );
    assert_eq!(second.len(), 2);
    assert!(second.search("old", 10).is_empty());
    assert!(second.search("first", 10).is_empty());
    assert_eq!(second.search("second", 10), vec![4]);
    assert_eq!(second.search("keep", 10), vec![2]);

    let deleted = second.with_updated_paths(&paths, vec![]);
    assert_eq!(deleted.len(), 1);
    assert!(deleted.search("second", 10).is_empty());
    assert_eq!(deleted.search("keep", 10), vec![2]);
    assert_eq!(deleted.delta_segment_count(), 3);
}

#[test]
fn name_table_segmented_prefix_and_narrowing_match_cold_search() {
    let table = NameTable::build_with_paths(vec![
        (
            1,
            "foo_base".to_string(),
            false,
            "src/base.c".to_string(),
            "function".to_string(),
            false,
        ),
        (
            2,
            "old_delta".to_string(),
            false,
            "src/changed.c".to_string(),
            "function".to_string(),
            false,
        ),
    ]);
    let paths = std::collections::HashSet::from(["src/changed.c".to_string()]);
    let table = table.with_updated_paths(
        &paths,
        vec![(
            3,
            "foo_delta".to_string(),
            false,
            "src/changed.c".to_string(),
            "function".to_string(),
            false,
        )],
    );
    let mut prefix_ids: Vec<i64> = table
        .prefix_candidates("foo")
        .into_iter()
        .map(|index| table.active_entry(index).id)
        .collect();
    prefix_ids.sort_unstable();
    assert_eq!(prefix_ids, vec![1, 3]);

    let (_, pool) = table.search_ranked_scoped_pooled("fo", 10, None, None);
    let narrowed = table
        .search_ranked_scoped_pooled("foo", 10, None, Some(&pool))
        .0;
    let cold = table.search_ranked_scoped_pooled("foo", 10, None, None).0;
    assert_eq!(narrowed, cold);
    assert!(cold.iter().all(|hit| hit.id != 2));
}

#[test]
fn name_table_compaction_preserves_active_results_and_removes_segments() {
    let mut table = NameTable::build_with_paths(vec![
        (
            1,
            "base_name".to_string(),
            false,
            "src/base.c".to_string(),
            "function".to_string(),
            false,
        ),
        (
            2,
            "changed_0".to_string(),
            false,
            "src/changed.c".to_string(),
            "function".to_string(),
            false,
        ),
    ]);
    let paths = std::collections::HashSet::from(["src/changed.c".to_string()]);
    for revision in 1..=64 {
        table = table.with_updated_paths(
            &paths,
            vec![(
                2 + revision,
                format!("changed_{revision}"),
                false,
                "src/changed.c".to_string(),
                "function".to_string(),
                false,
            )],
        );
    }
    assert!(table.needs_compaction());
    let before = table.search_ranked("changed", 10);
    let compacted = table.compacted();
    assert_eq!(compacted.delta_segment_count(), 0);
    assert!(!compacted.needs_compaction());
    assert_eq!(compacted.len(), 2);
    assert_eq!(compacted.search_ranked("changed", 10), before);
    assert_eq!(compacted.search("base", 10), vec![1]);
}

#[test]
fn identifier_completion_starts_at_one_character() {
    assert_eq!(MIN_PREFIX_LEN, 1);
}

#[test]
fn normalized_receiver_record_hint_strips_only_digits_and_underscores() {
    assert_eq!(normalized_receiver_record_hint("widget"), "widget");
    assert_eq!(normalized_receiver_record_hint("_widget"), "widget");
    assert_eq!(normalized_receiver_record_hint("2Widget"), "widget");
    assert_eq!(normalized_receiver_record_hint("pWidget"), "pwidget");
}

#[test]
fn short_prefix_keeps_exact_prefix_boundary_substr_only() {
    // At len < 3, only exact (1000), prefix (800), and word-boundary-
    // substring (650) hits survive; plain substrings (500) and all
    // subsequence tiers (400/200) are dropped by the min-score threshold.
    let table = NameTable::build(vec![
        (10, "Foobar".to_string(), false),
        (11, "FooBar".to_string(), false),
    ]);

    // "fo" (len 2): prefix of both -> score 800, both kept.
    let fo = table.search_ranked("fo", 10);
    assert!(fo.iter().any(|h| h.id == 10), "prefix of Foobar kept");
    assert!(fo.iter().any(|h| h.id == 11), "prefix of FooBar kept");

    // "ba" (len 2): boundary-substr of "FooBar" (at 'B', score 650, kept),
    // plain substr of "Foobar" (at 'b', score 500, dropped).
    let ba = table.search_ranked("ba", 10);
    assert!(
        ba.iter().any(|h| h.id == 11),
        "boundary-substr should survive at len 2"
    );
    assert!(
        ba.iter().all(|h| h.id != 10),
        "plain substr should be dropped at len 2"
    );

    // "fb" (len 2): subsequence-only of both (scores 200/400), all dropped.
    let fb = table.search_ranked("fb", 10);
    assert!(fb.is_empty(), "subsequence tiers must be dropped at len 2");
}

#[test]
fn long_prefix_restores_subsequence_recall() {
    // At len >= 3 the full tier set is restored, including subsequence
    // matches (camelCase initials). "fob" (len 3) is a subsequence of
    // "Foobar" that is neither a prefix nor a substring — it must be
    // recalled now that the threshold no longer suppresses it.
    let seq_table = NameTable::build(vec![(10, "Foobar".to_string(), false)]);
    let fob = seq_table.search_ranked("fob", 10);
    assert!(
        fob.iter().any(|h| h.id == 10),
        "subsequence should be recalled at len >= 3"
    );

    // The existing camelCase-initials path also still works at len 3.
    let camel_table = table();
    let kpa = camel_table.search_ranked("kpa", 10);
    assert_eq!(kpa.first().map(|h| h.id), Some(2)); // KePmmAllocPages
}

#[test]
fn ranked_name_hit_carries_kind_and_tie_break_unchanged() {
    // build_with_paths caches the kind string -> SymbolKind enum; hits
    // carry it out so the completion hot path can map to an LSP completion
    // item kind without re-opening the store.
    let table = NameTable::build_with_paths(vec![
        (
            1,
            "foo".to_string(),
            false,
            "a.c".to_string(),
            "function".to_string(),
            false,
        ),
        (
            2,
            "foo".to_string(),
            false,
            "b.c".to_string(),
            "macro".to_string(),
            false,
        ),
        (
            3,
            "foobar".to_string(),
            false,
            "c.c".to_string(),
            "type".to_string(),
            false,
        ),
    ]);
    // "foo" exact-matches ids 1 and 2 (score 1000 each), prefix-matches
    // id 3 (score 800). Tie-break: equal score -> shorter name first; the
    // prefix hit "foobar" sorts after both exacts. Truncation at limit=2
    // keeps only the two 1000-scored entries.
    let hits = table.search_ranked("foo", 2);
    assert_eq!(hits.len(), 2);
    assert!(
        hits.iter().all(|h| h.score == 1000),
        "truncation keeps the top-scored exact hits"
    );
    let kinds: Vec<ParserKind> = hits.iter().map(|h| h.kind).collect();
    assert!(kinds.contains(&ParserKind::Function));
    assert!(kinds.contains(&ParserKind::Macro));
    // The prefix hit (foobar, type) is truncated out.
    assert!(!hits.iter().any(|h| h.id == 3));

    // A single hit carries the right kind for a non-trivial kind.
    let hits = table.search_ranked("foobar", 10);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].kind, ParserKind::Type);
}

// --- Completion pipeline integration (R7: real index → NameTable → ReachGraph → tier ordering)

/// Helper: index a small workspace, build the NameTable and ReachGraph from
/// the store, and return them together with the current file name so tests
/// can construct a [`CompletionScope`] and run scoped/pooled searches.
fn build_table_and_scope(
    dir: &std::path::Path,
    files: &[(&str, &str)],
) -> (NameTable, crate::reachability::ReachGraph) {
    build_table_and_scope_with_options(dir, files, crate::indexer::IndexOptions::default())
}

fn build_table_and_scope_with_options(
    dir: &std::path::Path,
    files: &[(&str, &str)],
    mut options: crate::indexer::IndexOptions,
) -> (NameTable, crate::reachability::ReachGraph) {
    use std::fs;
    for (rel, content) in files {
        let abs = dir.join(rel);
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(&abs, content).expect("write");
    }
    let db = dir.join("index.sqlite");
    options.db_path = Some(db.clone());
    crate::indexer::index_workspace(dir, options, |_| {}).expect("index");

    let store = crate::store::IndexStore::open_readonly(&db).expect("readonly");
    let table = NameTable::build_from_declaration_view(&store.declaration_view(), None)
        .expect("streamed name table");

    let edges = store.load_include_edge_paths().expect("edges");
    let unresolved: Vec<String> = store.open_include_file_paths().unwrap_or_default();
    let ambiguous: Vec<String> = store.ambiguous_include_file_paths().unwrap_or_default();
    let graph = crate::reachability::ReachGraph::new(edges, unresolved, ambiguous);

    (table, graph)
}

#[test]
fn streamed_name_index_matches_typed_row_builder_with_project_context() {
    use crate::project_context::{ProjectContext, ProjectContextIndex, ProjectKey};

    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    for (path, source) in [
        (
            "app/src/main.c",
            "#define APP_FLAG 1\nint project_api(void) { return APP_FLAG; }\n",
        ),
        ("other/helper.c", "int helper_api(void) { return 2; }\n"),
    ] {
        let absolute = dir.path().join(path);
        std::fs::create_dir_all(absolute.parent().expect("parent")).expect("mkdir");
        std::fs::write(absolute, source).expect("write source");
    }
    let options = crate::indexer::IndexOptions {
        db_path: Some(db.clone()),
        ..Default::default()
    };
    crate::indexer::index_workspace(dir.path(), options, |_| {}).expect("index");
    let store = crate::store::IndexStore::open_readonly(&db).expect("readonly");
    let key = ProjectKey {
        workspace_root_id: "root".to_string(),
        project_path: "app".to_string(),
    };
    let projects = ProjectContextIndex::new(
        "root".to_string(),
        "workspace".to_string(),
        vec![ProjectContext {
            key: key.clone(),
            workspace_name: "workspace".to_string(),
            marker_files: vec!["app/Makefile".to_string()],
        }],
    );

    let legacy = NameTable::build_from_declaration_name_rows_with_project_context(
        store
            .declaration_view()
            .all_name_rows()
            .expect("typed rows"),
        Some(&projects),
    );
    let streamed =
        NameTable::build_from_declaration_view(&store.declaration_view(), Some(&projects))
            .expect("streamed rows");

    assert_eq!(streamed.len(), legacy.len());
    for query in ["api", "APP", "helper", "project"] {
        assert_eq!(
            streamed.search_ranked(query, 100),
            legacy.search_ranked(query, 100),
            "streamed and typed-row builders diverged for {query}"
        );
    }
    assert_eq!(streamed.project_indices(&key), legacy.project_indices(&key));
}

#[test]
fn compact_name_recall_filters_c_family_and_go_before_spending_budget() {
    use crate::config::SemanticFamily;
    use crate::semantic_model::{SemanticDeclarationKind, SemanticDeclarationRole};
    use crate::store::views::DeclarationNameRow;

    let table = NameTable::build_from_declaration_name_rows_with_project_context(
        vec![
            DeclarationNameRow {
                id: 1,
                name: "SharedOpen".to_string(),
                declaration_kind: SemanticDeclarationKind::Function,
                role: SemanticDeclarationRole::Definition,
                path: "src/open.c".to_string(),
                external: false,
                directly_included: false,
                semantic_family: SemanticFamily::CFamily,
            },
            DeclarationNameRow {
                id: 2,
                name: "SharedOpen".to_string(),
                declaration_kind: SemanticDeclarationKind::Function,
                role: SemanticDeclarationRole::Definition,
                path: "src/open.go".to_string(),
                external: false,
                directly_included: false,
                semantic_family: SemanticFamily::Go,
            },
        ],
        None,
    );

    let c_hits =
        table.exact_name_hits_scoped_for_family("SharedOpen", 1, None, SemanticFamily::CFamily);
    let go_hits =
        table.exact_name_hits_scoped_for_family("SharedOpen", 1, None, SemanticFamily::Go);
    assert_eq!(c_hits.iter().map(|hit| hit.id).collect::<Vec<_>>(), vec![1]);
    assert_eq!(
        go_hits.iter().map(|hit| hit.id).collect::<Vec<_>>(),
        vec![2]
    );
}

#[test]
fn completion_reachable_outranks_unreachable_from_real_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (table, graph) = build_table_and_scope(
        dir.path(),
        &[
            (
                "src/main.c",
                "#include \"reachable.h\"\nint local_helper(void) { return 1; }\n",
            ),
            ("src/reachable.h", "int widget_start(void);\n"),
            ("other/away.c", "int widget_end(void) { return 42; }\n"),
        ],
    );
    let reach = graph.reachable("src/main.c");
    let scope = CompletionScope {
        current_path: Some("src/main.c".to_string()),
        direct_external_files: graph.directly_included_external_paths_from("src/main.c"),
        reach: (*reach).clone(),
    };
    let hits = table.search_ranked_scoped("widget", 10, Some(&scope));
    // widget_start (in reachable.h) must outrank widget_end (in unreachable other/away.c)
    let start_hit = hits.iter().find(|h| h.name == "widget_start");
    let end_hit = hits.iter().find(|h| h.name == "widget_end");
    assert!(
        start_hit.is_some(),
        "widget_start from reachable header must be present"
    );
    assert!(
        end_hit.is_some(),
        "widget_end from unreachable file must still be present (never dropped)"
    );
    let si = hits.iter().position(|h| h.name == "widget_start").unwrap();
    let ei = hits.iter().position(|h| h.name == "widget_end").unwrap();
    assert!(
        si < ei,
        "reachable widget_start outranks unreachable widget_end"
    );
    assert_eq!(
        start_hit.unwrap().tier,
        ScopeTier::Reachable,
        "widget_start is Reachable tier"
    );
    // widget_end is either Global (if scope closed) or Unknown (if open).
    // Either way it must be below Reachable.
    assert!(
        end_hit.unwrap().tier < ScopeTier::Reachable || end_hit.unwrap().tier == ScopeTier::Unknown,
        "widget_end tier is below Reachable"
    );
}

#[test]
fn completion_external_demotes_below_workspace_reachable() {
    // Verify: workspace reachable > external > global. Uses an external
    // include path to index a "system" header, included by the workspace
    // source, producing an ExternalExact edge.
    let dir = tempfile::tempdir().expect("tempdir");
    let ext_dir = dir.path().join("sysroot");
    std::fs::create_dir_all(&ext_dir).expect("sysroot");
    std::fs::write(ext_dir.join("helper.h"), "int ext_helper(void);\n").expect("ext header");

    let (table, graph) = build_table_and_scope_with_options(
        dir.path(),
        &[
            (
                "src/main.c",
                "#include \"local.h\"\n#include <helper.h>\nint main_local(void);\n",
            ),
            ("src/local.h", "int local_helper(void);\n"),
        ],
        crate::indexer::IndexOptions {
            include_paths: vec![ext_dir.to_string_lossy().replace('\\', "/")],
            ..Default::default()
        },
    );
    let reach = graph.reachable("src/main.c");
    let scope = CompletionScope {
        current_path: Some("src/main.c".to_string()),
        direct_external_files: graph.directly_included_external_paths_from("src/main.c"),
        reach: (*reach).clone(),
    };
    let hits = table.search_ranked_scoped("helper", 10, Some(&scope));
    // ext_helper from the external header is indexed as External, while
    // local_helper is reachable through a workspace header and must outrank it.
    let local_pos = hits
        .iter()
        .position(|h| h.name == "local_helper")
        .expect("local_helper from reachable workspace header must be present");
    let ext_pos = hits.iter().position(|h| h.name == "ext_helper");
    let ext_pos = ext_pos.expect("ext_helper from configured external header must be present");
    assert!(
        local_pos < ext_pos,
        "workspace reachable local_helper outranks external ext_helper"
    );
    assert_eq!(hits[local_pos].tier, ScopeTier::Reachable);
    assert_eq!(hits[ext_pos].tier, ScopeTier::External);
}

#[test]
fn completion_direct_external_evidence_is_origin_specific() {
    let external = "C:/sdk/include/shared.h".to_string();
    let table = NameTable::build_with_paths(vec![(
        1,
        "shared_api".to_string(),
        true,
        external.clone(),
        "function".to_string(),
        true,
    )]);
    let reach = ReachScope {
        files: HashSet::from(["src/main.c".to_string()]),
        heuristic_files: HashSet::new(),
        open: false,
        reason: None,
    };
    let including_origin = CompletionScope {
        current_path: Some("src/main.c".to_string()),
        direct_external_files: HashSet::from([external]),
        reach: reach.clone(),
    };
    let unrelated_origin = CompletionScope {
        current_path: Some("src/other.c".to_string()),
        direct_external_files: HashSet::new(),
        reach,
    };

    assert_eq!(
        table.exact_name_hits_scoped("shared_api", 1, Some(&including_origin))[0].tier,
        ScopeTier::External
    );
    assert_eq!(
        table.exact_name_hits_scoped("shared_api", 1, Some(&unrelated_origin))[0].tier,
        ScopeTier::Global
    );
}

#[test]
fn completion_is_truncated_at_limit() {
    // When more candidates match than the limit, the result is truncated.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut files: Vec<(&str, String)> = Vec::new();
    files.push((
        "src/main.c",
        "#include \"many.h\"\nint main_use(void) { return 0; }\n".to_string(),
    ));
    let mut header = String::from("/* many symbols */\n");
    for i in 1..=30 {
        header.push_str(&format!("int api_func_{:02}(void);\n", i));
    }
    files.push(("src/many.h", header));
    let file_refs: Vec<(&str, &str)> = files.iter().map(|(p, s)| (*p, s.as_str())).collect();
    let (table, graph) = build_table_and_scope(dir.path(), &file_refs);
    let reach = graph.reachable("src/main.c");
    let scope = CompletionScope {
        current_path: Some("src/main.c".to_string()),
        direct_external_files: graph.directly_included_external_paths_from("src/main.c"),
        reach: (*reach).clone(),
    };
    let limit = 10;
    let hits = table.search_ranked_scoped("api_func", limit, Some(&scope));
    assert_eq!(
        hits.len(),
        limit,
        "result must be truncated to the requested limit"
    );
    // All 30 api_func_* symbols have identical score (same tier, exact match
    // quality for "api_func" prefix), so 20 are truncated.
    assert!(
        hits.len() < 30,
        "10 of 30 matching symbols truncated, confirming isIncomplete semantics"
    );
}

#[test]
fn exact_name_lookup_recovers_symbol_truncated_from_dense_prefix() {
    let mut names = Vec::new();
    for i in 0..150 {
        names.push((
            i,
            format!("api_common_{i:03}"),
            false,
            format!("inc/api_{i:03}.h"),
            "function".to_string(),
            false,
        ));
    }
    names.push((
        1000,
        "api_target_function".to_string(),
        false,
        "inc/target.h".to_string(),
        "function".to_string(),
        false,
    ));
    let table = NameTable::build_with_paths(names);

    let prefix_hits = table.search_ranked_scoped("api", 100, None);
    assert!(
        prefix_hits
            .iter()
            .all(|hit| hit.name != "api_target_function"),
        "dense prefix top-N should reproduce the truncation observed by completion"
    );

    let exact_hits = table.exact_name_hits_scoped("api_target_function", 10, None);
    assert_eq!(exact_hits.len(), 1);
    assert_eq!(exact_hits[0].name, "api_target_function");
    assert_eq!(exact_hits[0].kind, ParserKind::Function);
}

#[test]
fn completion_same_name_ranks_higher_tier_first() {
    // Same-name symbol appears in both reachable and unreachable files.
    // NameTable preserves both entries for callers that need candidates,
    // but the higher-tier entry must rank first.
    let dir = tempfile::tempdir().expect("tempdir");
    let (table, graph) = build_table_and_scope(
        dir.path(),
        &[
            ("src/main.c", "#include \"reachable.h\"\n"),
            (
                "src/reachable.h",
                "int dual_name(void);\n", // Reachable tier
            ),
            (
                "other/lost.c",
                "int dual_name(int x) { return x; }\n", // Global/Unknown tier
            ),
        ],
    );
    let reach = graph.reachable("src/main.c");
    let scope = CompletionScope {
        current_path: Some("src/main.c".to_string()),
        direct_external_files: graph.directly_included_external_paths_from("src/main.c"),
        reach: (*reach).clone(),
    };
    let hits = table.search_ranked_scoped("dual_name", 10, Some(&scope));
    let duals: Vec<&RankedNameHit> = hits.iter().filter(|h| h.name == "dual_name").collect();
    assert_eq!(
        duals.len(),
        2,
        "NameTable preserves distinct same-name candidates before server-level dedup"
    );
    // The highest-tier dual_name should be from reachable.h (Reachable tier).
    let best = duals.first().unwrap();
    assert_eq!(
        best.tier,
        ScopeTier::Reachable,
        "best dual_name is Reachable tier"
    );
    assert!(
        duals[1].tier < ScopeTier::Reachable || duals[1].tier == ScopeTier::Unknown,
        "lower-ranked dual_name is below Reachable"
    );
}

// --- R7: error degradation — empty NameTable must be well-formed ----------

#[test]
fn name_table_from_empty_store_is_valid_and_empty() {
    let table = NameTable::build_with_paths(vec![]);
    assert_eq!(table.len(), 0);
    let hits = table.search_ranked("anything", 10);
    assert!(hits.is_empty(), "empty table produces empty search results");
    // No panic on any method.
    let hits = table.search_ranked_scoped("x", 10, None);
    assert!(hits.is_empty());
}

#[test]
fn name_table_with_updated_paths_on_empty_set_keeps_all() {
    let table = NameTable::build_with_paths(vec![(
        1,
        "keep_name".to_string(),
        false,
        "src/b.c".to_string(),
        "function".to_string(),
        false,
    )]);
    let paths = std::collections::HashSet::new();
    let updated = table.with_updated_paths(&paths, vec![]);
    // Empty path set means no entries removed, empty names means none added.
    // The original entry must survive.
    assert_eq!(updated.search_ranked("keep", 10).len(), 1);
}
