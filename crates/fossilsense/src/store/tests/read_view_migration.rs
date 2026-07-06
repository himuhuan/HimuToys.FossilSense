use std::fs;
use std::path::Path;

fn read(path: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(root.join(path)).unwrap_or_else(|err| panic!("failed to read {path}: {err}"))
}

fn assert_absent(path: &str, forbidden: &[&str]) {
    let source = read(path);
    for pattern in forbidden {
        assert!(
            !source.contains(pattern),
            "{path} should consume store::views/read views directly, found `{pattern}`"
        );
    }
}

#[test]
fn read_model_cache_rebuilds_use_typed_store_views() {
    assert_absent(
        "src/server/indexing/cache.rs",
        &[
            "store.load_symbol_names_with_paths(",
            "store.load_symbol_names_for_paths(",
            "store.load_include_data_for_sources(",
            "store.load_include_edge_paths(",
            "store.open_include_file_paths(",
            "store.ambiguous_include_file_paths(",
            "store.workspace_file_paths(",
            "store.indexed_workspace_files(",
        ],
    );
}

#[test]
fn feature_and_cli_call_sites_use_read_views_for_exact_store_queries() {
    assert_absent(
        "src/server/language_server.rs",
        &["store.symbols_by_name(", "store.symbols_by_ids("],
    );
    assert_absent("src/server/hover.rs", &["store.symbols_by_name("]);
    assert_absent("src/server/signature_help.rs", &["store.symbols_by_name("]);
    assert_absent(
        "src/server/member_completion.rs",
        &[
            "store.resolve_record_candidates(",
            "store.members_for_records(",
            "store.fallback_member_candidates(",
        ],
    );
    assert_absent(
        "src/server/include_completion.rs",
        &[
            "store.workspace_file_paths(",
            "store.workspace_files_by_suffix(",
        ],
    );
    assert_absent(
        "src/main.rs",
        &[
            "store.load_symbol_names(",
            "store.symbols_by_ids(",
            "store.symbols_by_name(",
        ],
    );
}
