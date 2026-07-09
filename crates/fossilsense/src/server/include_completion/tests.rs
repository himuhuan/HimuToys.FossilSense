use super::*;
use crate::indexer::{self, IndexOptions};
use std::collections::HashMap;
use std::fs;
use std::sync::{Arc, Mutex as StdMutex};
use tempfile::tempdir;
use tower_lsp::lsp_types::CompletionItemKind;

#[test]
fn looks_like_header_accepts_headers_and_extensionless() {
    assert!(looks_like_header("stdio.h"));
    assert!(looks_like_header("vector"));
    assert!(!looks_like_header("main.c"));
    assert!(!looks_like_header("readme.txt"));
}

#[test]
fn resolve_include_quote_prefers_local_then_root() {
    let cur = tempdir().expect("cur");
    let root = tempdir().expect("root");
    fs::write(cur.path().join("config.h"), "x").expect("local");
    fs::write(root.path().join("config.h"), "x").expect("root copy");
    let root_str = root.path().to_string_lossy().replace('\\', "/");

    let resolved = resolve_include_paths(
        IncludeForm::Quote,
        "config.h",
        Some(cur.path()),
        None,
        &[root_str],
        None,
    )
    .expect("resolve");

    assert_eq!(resolved.len(), 2);
    assert!(resolved[0].starts_with(cur.path()));
}

#[test]
fn resolve_include_angle_prefers_include_root() {
    let cur = tempdir().expect("cur");
    let root = tempdir().expect("root");
    fs::write(root.path().join("stdio.h"), "x").expect("root header");
    let root_str = root.path().to_string_lossy().replace('\\', "/");

    let resolved = resolve_include_paths(
        IncludeForm::Angle,
        "stdio.h",
        Some(cur.path()),
        None,
        &[root_str],
        None,
    )
    .expect("resolve");

    assert_eq!(resolved.len(), 1);
    assert!(resolved[0].starts_with(root.path()));
}

#[test]
fn resolve_include_unresolved_is_empty() {
    let resolved =
        resolve_include_paths(IncludeForm::Angle, "nope/missing.h", None, None, &[], None)
            .expect("resolve");
    assert!(resolved.is_empty());
}

#[test]
fn include_candidates_are_headers_and_subdirs_only() {
    let root = tempdir().expect("root");
    fs::write(root.path().join("stdio.h"), "x").expect("stdio");
    fs::write(root.path().join("stdlib.h"), "x").expect("stdlib");
    fs::write(root.path().join("notes.txt"), "x").expect("txt");
    fs::create_dir_all(root.path().join("sys")).expect("sys");
    let root_str = root.path().to_string_lossy().replace('\\', "/");

    let items = collect_include_candidates(
        IncludeForm::Angle,
        "",
        "std",
        None,
        None,
        std::slice::from_ref(&root_str),
        None,
        100,
    );
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(labels.contains(&"stdio.h"));
    assert!(labels.contains(&"stdlib.h"));
    assert!(!labels.contains(&"notes.txt"), "non-header file excluded");

    fs::write(root.path().join("sys/types.h"), "x").expect("types");
    let sub = collect_include_candidates(
        IncludeForm::Angle,
        "sys/",
        "",
        None,
        None,
        &[root_str],
        None,
        100,
    );
    assert!(sub.iter().any(|i| i.label == "types.h"));
    let top = collect_include_candidates(
        IncludeForm::Angle,
        "",
        "sys",
        None,
        None,
        &[root.path().to_string_lossy().replace('\\', "/")],
        None,
        100,
    );
    assert!(top
        .iter()
        .any(|i| i.label == "sys" && i.kind == Some(CompletionItemKind::FOLDER)));
}

#[test]
fn configured_include_paths_merge_fossilsense_json_and_client() {
    let ws = tempdir().expect("ws");
    let from_json = tempdir().expect("json inc");
    let from_client = tempdir().expect("client inc");
    let json_path = from_json.path().to_string_lossy().replace('\\', "/");
    let client_path = from_client.path().to_string_lossy().replace('\\', "/");
    fs::write(
        ws.path().join("fossilsense.json"),
        format!(r#"{{"includePaths": ["{}"]}}"#, json_path),
    )
    .expect("config");

    let paths = configured_include_paths(Some(ws.path()), std::slice::from_ref(&client_path));
    assert!(paths.contains(&json_path));
    assert!(paths.contains(&client_path));
}

#[test]
fn include_candidates_use_indexed_workspace_headers_below_subdirs() {
    let ws = tempdir().expect("ws");
    fs::create_dir_all(ws.path().join("include/sys")).expect("include");
    fs::write(ws.path().join("include/foo.h"), "typedef int foo_t;\n").expect("foo");
    fs::write(
        ws.path().join("include/sys/types.h"),
        "typedef int type_t;\n",
    )
    .expect("types");
    let db = ws.path().join("index.sqlite");
    indexer::index_workspace(
        ws.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let items = collect_include_candidates(
        IncludeForm::Angle,
        "",
        "fo",
        None,
        Some(ws.path()),
        &[],
        Some(db.as_path()),
        100,
    );
    assert!(items
        .iter()
        .any(|i| i.label == "foo.h" && i.kind == Some(CompletionItemKind::FILE)));

    let sub = collect_include_candidates(
        IncludeForm::Angle,
        "sys/",
        "ty",
        None,
        Some(ws.path()),
        &[],
        Some(db.as_path()),
        100,
    );
    assert!(sub
        .iter()
        .any(|i| i.label == "types.h" && i.kind == Some(CompletionItemKind::FILE)));
}

#[test]
fn include_completion_table_matches_indexed_workspace_candidates() {
    let table = IncludeCompletionTable::build(vec![
        "include/api.h".to_string(),
        "include/detail/deep.h".to_string(),
        "src/main.c".to_string(),
        "vendor/api.h".to_string(),
    ]);

    let top = collect_include_candidates_with_table(
        IncludeForm::Quote,
        "",
        "in",
        None,
        None,
        &[],
        None,
        Some(&table),
        None,
        20,
    );
    assert!(top
        .iter()
        .any(|i| i.label == "include" && i.kind == Some(CompletionItemKind::FOLDER)));

    let nested = collect_include_candidates_with_table(
        IncludeForm::Quote,
        "include/",
        "de",
        None,
        None,
        &[],
        None,
        Some(&table),
        None,
        20,
    );
    assert!(nested
        .iter()
        .any(|i| i.label == "detail" && i.kind == Some(CompletionItemKind::FOLDER)));

    let deep = collect_include_candidates_with_table(
        IncludeForm::Quote,
        "include/detail/",
        "de",
        None,
        None,
        &[],
        None,
        Some(&table),
        None,
        20,
    );
    assert!(deep
        .iter()
        .any(|i| i.label == "deep.h" && i.kind == Some(CompletionItemKind::FILE)));
}

#[test]
fn quote_include_prefers_same_directory_and_sibling_patterns() {
    let table = IncludeCompletionTable::build_with_edges(
        vec![
            "src/driver/main.c".to_string(),
            "src/driver/main.h".to_string(),
            "src/driver/config.h".to_string(),
            "vendor/config.h".to_string(),
        ],
        vec![(
            "src/driver/main.c".to_string(),
            "src/driver/config.h".to_string(),
        )],
    );
    let evidence =
        CurrentIncludeEvidence::from_text("#include \"config.h\"\n", Some("src/driver/main.c"));

    let items = collect_include_candidates_ranked_for_test(
        IncludeForm::Quote,
        "",
        "con",
        Some("src/driver"),
        Some(&table),
        Some(&evidence),
        20,
    );

    assert_eq!(items[0].label, "config.h");
}

#[test]
fn basename_frequency_breaks_workspace_ties_without_overriding_form_priority() {
    let table = IncludeCompletionTable::build_with_edges(
        vec![
            "src/a/common.h".to_string(),
            "src/b/common.h".to_string(),
            "src/c/common.h".to_string(),
            "src/driver/config.h".to_string(),
        ],
        Vec::new(),
    );

    let items = collect_include_candidates_ranked_for_test(
        IncludeForm::Quote,
        "",
        "c",
        None,
        Some(&table),
        None,
        20,
    );

    assert_eq!(items[0].label, "common.h");
    assert!(items.iter().any(|item| item.label == "config.h"));
}

#[test]
fn path_depth_penalty_prefers_shallow_comparable_headers() {
    let table = IncludeCompletionTable::build_with_edges(
        vec![
            "include/api.h".to_string(),
            "include/detail/internal/api.h".to_string(),
        ],
        Vec::new(),
    );

    let items = collect_include_candidates_ranked_for_test(
        IncludeForm::Quote,
        "include/",
        "api",
        None,
        Some(&table),
        None,
        20,
    );

    assert_eq!(items[0].label, "api.h");
}

#[test]
fn angle_include_keeps_external_root_base_priority() {
    let root = tempdir().expect("root");
    fs::write(root.path().join("config.h"), "x").expect("external");
    let root_str = root.path().to_string_lossy().replace('\\', "/");
    let table = IncludeCompletionTable::build_with_edges(
        vec!["src/driver/config.h".to_string()],
        Vec::new(),
    );

    let items = collect_include_candidates_with_table(
        IncludeForm::Angle,
        "",
        "con",
        None,
        None,
        &[root_str],
        None,
        Some(&table),
        None,
        20,
    );

    assert_eq!(items[0].label, "config.h");
}

#[test]
fn angle_include_external_bucket_stays_above_boosted_workspace_candidate() {
    let root = tempdir().expect("root");
    fs::write(root.path().join("core.h"), "x").expect("external");
    let root_str = root.path().to_string_lossy().replace('\\', "/");
    let table = IncludeCompletionTable::build_with_edges(
        vec![
            "src/driver/main.c".to_string(),
            "src/driver/config.h".to_string(),
        ],
        vec![(
            "src/driver/main.c".to_string(),
            "src/driver/config.h".to_string(),
        )],
    );
    let evidence =
        CurrentIncludeEvidence::from_text("#include \"config.h\"\n", Some("src/driver/main.c"));

    let (items, metrics) = collect_include_candidates_with_table_and_evidence(
        IncludeForm::Angle,
        "",
        "c",
        None,
        None,
        &[root_str],
        None,
        Some(&table),
        None,
        Some("src/driver"),
        Some(&evidence),
        20,
    );

    assert_eq!(items[0].label, "core.h");
    assert!(items.iter().any(|item| item.label == "config.h"));
    assert!(metrics.same_directory > 0);
}

#[test]
fn quote_include_current_dir_bucket_stays_above_boosted_workspace_candidate() {
    let current = tempdir().expect("current");
    fs::write(current.path().join("core.h"), "x").expect("local");
    let table = IncludeCompletionTable::build_with_edges(
        vec![
            "src/driver/main.c".to_string(),
            "src/driver/config.h".to_string(),
        ],
        vec![(
            "src/driver/main.c".to_string(),
            "src/driver/config.h".to_string(),
        )],
    );
    let evidence =
        CurrentIncludeEvidence::from_text("#include \"config.h\"\n", Some("src/driver/main.c"));

    let (items, metrics) = collect_include_candidates_with_table_and_evidence(
        IncludeForm::Quote,
        "",
        "c",
        Some(current.path()),
        None,
        &[],
        None,
        Some(&table),
        None,
        Some("src/driver"),
        Some(&evidence),
        20,
    );

    assert_eq!(items[0].label, "core.h");
    assert!(items.iter().any(|item| item.label == "config.h"));
    assert!(metrics.same_directory > 0);
}

#[test]
fn empty_include_completion_table_is_safe() {
    let table = IncludeCompletionTable::default();
    let items = collect_include_candidates_with_table(
        IncludeForm::Quote,
        "",
        "api",
        None,
        None,
        &[],
        None,
        Some(&table),
        None,
        20,
    );
    assert!(items.is_empty());
}

#[test]
fn external_include_directory_cache_reuses_listing() {
    let root = tempdir().expect("root");
    fs::write(root.path().join("stdio.h"), "x").expect("stdio");
    let root_str = root.path().to_string_lossy().replace('\\', "/");
    let cache = Arc::new(StdMutex::new(HashMap::new()));

    let first = collect_include_candidates_with_table(
        IncludeForm::Angle,
        "",
        "std",
        None,
        None,
        std::slice::from_ref(&root_str),
        None,
        None,
        Some(&cache),
        20,
    );
    assert!(first.iter().any(|i| i.label == "stdio.h"));
    assert_eq!(cache.lock().expect("cache").len(), 1);

    let second = collect_include_candidates_with_table(
        IncludeForm::Angle,
        "",
        "std",
        None,
        None,
        std::slice::from_ref(&root_str),
        None,
        None,
        Some(&cache),
        20,
    );
    assert!(second.iter().any(|i| i.label == "stdio.h"));
    assert_eq!(cache.lock().expect("cache").len(), 1);
}

#[test]
fn external_include_directory_cache_skips_invalid_root() {
    let root = tempdir().expect("root");
    let missing = root
        .path()
        .join("missing")
        .to_string_lossy()
        .replace('\\', "/");
    let cache = Arc::new(StdMutex::new(HashMap::new()));
    let items = collect_include_candidates_with_table(
        IncludeForm::Angle,
        "",
        "std",
        None,
        None,
        &[missing],
        None,
        None,
        Some(&cache),
        20,
    );
    assert!(items.is_empty());
    assert!(cache.lock().expect("cache").is_empty());
}
