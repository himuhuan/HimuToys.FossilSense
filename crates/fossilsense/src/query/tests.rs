use super::*;
use crate::reachability::ReachScope;

mod name_table_integration;

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
        .map(|&i| table.entries[i].id)
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 2], "only exact/prefix entries, not substrings");
}

#[test]
fn prefix_indexed_recall_matches_full_scan_when_dense() {
    // Dense prefix recall must return the same top-N as the full scoring path.
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
fn short_prefix_indexed_recall_matches_full_scan() {
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
fn indexed_recall_preserves_fuzzy_matches_when_candidates_below_limit() {
    let table = table();
    // "hello" has < 10 prefix candidates but still includes
    // subsequence/substring recall identical to the pooled baseline.
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
        table.search_completion_recall_pooled("api", quotas, Some(&scope), None, None);

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

    let (hits, _, _) = table.search_completion_recall_pooled("ba", quotas, None, None, None);
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
        .search_completion_recall_pooled("foob", quotas, None, None, Some(&pool))
        .0;
    let cold = table
        .search_completion_recall_pooled("foob", quotas, None, None, None)
        .0;

    assert_eq!(narrowed, cold);
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
fn external_boundary_substring_is_not_hidden_by_dense_global_prefixes() {
    let table = NameTable::build_with_paths(vec![
        (
            1,
            "foo_alpha".to_string(),
            false,
            "src/a.c".to_string(),
            "function".to_string(),
            false,
        ),
        (
            2,
            "foo_beta".to_string(),
            false,
            "src/b.c".to_string(),
            "function".to_string(),
            false,
        ),
        (
            3,
            "foo_gamma".to_string(),
            false,
            "src/c.c".to_string(),
            "function".to_string(),
            false,
        ),
        (
            4,
            "sdkFoo".to_string(),
            true,
            "C:/sdk/sdk_foo.h".to_string(),
            "function".to_string(),
            true,
        ),
    ]);

    let hits = table.search_ranked("foo", 3);

    assert_eq!(
        hits.first().map(|hit| hit.id),
        Some(4),
        "External tier must outrank Global prefix candidates even for boundary-substring matches"
    );
    assert!(
        hits.iter().any(|hit| hit.id == 4),
        "External candidate must not be skipped when prefix candidates fill the limit"
    );
}

#[test]
fn completion_recall_cold_pool_is_bounded_for_dense_large_prefix() {
    let rows = (0..8000)
        .map(|idx| {
            (
                idx,
                format!("api_symbol_{idx:04}"),
                false,
                format!("src/{idx:04}.h"),
                "function".to_string(),
                false,
            )
        })
        .collect();
    let table = NameTable::build_with_paths(rows);
    let quotas = CompletionRecallQuotas::default_for_completion_limit(COMPLETION_LIMIT);

    let (_hits, pool, metrics) =
        table.search_completion_recall_pooled("api", quotas, None, None, None);

    assert!(
        pool.is_empty(),
        "large-table bounded recall should not cache a non-exhaustive narrowing pool"
    );
    assert_eq!(metrics.pool_total, 0);
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
