use super::super::*;

// --- Completion pipeline integration (R7: real index -> NameTable -> ReachGraph -> tier ordering)

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
    let names = store.load_symbol_names_with_paths().expect("names");
    let table = NameTable::build_with_paths(names);

    let edges = store.load_include_edge_paths().expect("edges");
    let unresolved: Vec<String> = store.open_include_file_paths().unwrap_or_default();
    let ambiguous: Vec<String> = store.ambiguous_include_file_paths().unwrap_or_default();
    let graph = crate::reachability::ReachGraph::new(edges, unresolved, ambiguous);

    (table, graph)
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
