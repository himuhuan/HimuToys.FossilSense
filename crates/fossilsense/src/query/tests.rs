use super::*;
use crate::reachability::ReachScope;

#[cfg(windows)]
fn current_private_bytes() -> u64 {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    let mut counters = PROCESS_MEMORY_COUNTERS_EX {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    let loaded = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(),
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        )
    };
    assert_ne!(loaded, 0, "GetProcessMemoryInfo failed");
    counters.PrivateUsage as u64
}

#[cfg(not(windows))]
fn current_private_bytes() -> u64 {
    0
}

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
        .name_table_view()
        .visit_symbol_rows(|row| {
            builder.push(row);
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
        .name_table_view()
        .largest_symbol_path()
        .expect("largest symbol path")
        .map(|(path, _)| path)
        .expect("at least one symbol row");
    let fresh_rows = store
        .name_table_view()
        .symbol_rows_for_paths(std::slice::from_ref(&changed_path))
        .expect("load changed path rows");

    let paths = std::collections::HashSet::from([changed_path]);
    let mut dirty_us = Vec::new();
    let private_before = current_private_bytes();
    let peak_private = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(private_before));
    let stop_sampling = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sampler = {
        let peak_private = peak_private.clone();
        let stop_sampling = stop_sampling.clone();
        std::thread::spawn(move || {
            while !stop_sampling.load(std::sync::atomic::Ordering::Relaxed) {
                peak_private.fetch_max(
                    current_private_bytes(),
                    std::sync::atomic::Ordering::Relaxed,
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        })
    };
    for _ in 0..5 {
        let update_started = std::time::Instant::now();
        table = table.with_updated_path_rows(&paths, fresh_rows.clone());
        dirty_us.push(update_started.elapsed().as_micros());
        assert_eq!(table.len(), expected_len);
    }
    stop_sampling.store(true, std::sync::atomic::Ordering::Relaxed);
    sampler.join().expect("memory sampler");
    dirty_us.sort_unstable();

    while !table.needs_compaction() {
        table = table.with_updated_path_rows(&paths, fresh_rows.clone());
        assert_eq!(table.len(), expected_len);
    }
    let segments_before_compaction = table.delta_segment_count();
    let compaction_private_before = current_private_bytes();
    let compaction_peak_private =
        std::sync::Arc::new(std::sync::atomic::AtomicU64::new(compaction_private_before));
    let stop_compaction_sampling = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let compaction_sampler = {
        let peak_private = compaction_peak_private.clone();
        let stop_sampling = stop_compaction_sampling.clone();
        std::thread::spawn(move || {
            while !stop_sampling.load(std::sync::atomic::Ordering::Relaxed) {
                peak_private.fetch_max(
                    current_private_bytes(),
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
        reach: ReachScope {
            files: reachable.iter().map(|s| s.to_string()).collect(),
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
    let table = NameTable::build_from_store_view(&store.name_table_view(), None)
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

    let legacy = NameTable::build_from_rows_with_project_context(
        store.name_table_view().symbol_rows().expect("typed rows"),
        Some(&projects),
    );
    let streamed = NameTable::build_from_store_view(&store.name_table_view(), Some(&projects))
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
