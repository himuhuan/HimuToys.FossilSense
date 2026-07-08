use std::fs;
use std::path::Path;

fn read(path: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(root.join(path)).unwrap_or_else(|err| panic!("failed to read {path}: {err}"))
}

fn production_source(path: &str) -> String {
    let source = read(path);
    if let Some((production, _tests)) = source.split_once("#[cfg(test)]\nmod tests") {
        production.to_string()
    } else {
        source
    }
}

fn assert_absent(path: &str, forbidden: &[&str]) {
    let source = production_source(path);
    for pattern in forbidden {
        assert!(
            !source.contains(pattern),
            "{path} should consume store::views/read views directly, found `{pattern}`"
        );
    }
}

fn rust_sources_under(dir: &Path, out: &mut Vec<String>) {
    for entry in fs::read_dir(dir).unwrap_or_else(|err| panic!("failed to read {dir:?}: {err}")) {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            rust_sources_under(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"));
            let rel = path
                .strip_prefix(root)
                .expect("source below manifest dir")
                .to_string_lossy()
                .replace('\\', "/");
            out.push(rel);
        }
    }
}

fn feature_production_sources() -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut paths = Vec::new();
    rust_sources_under(&root.join("src"), &mut paths);
    paths
        .into_iter()
        .filter(|path| {
            !path.ends_with("/tests.rs")
                && !path.contains("/tests/")
                && path != "src/store.rs"
                && !path.starts_with("src/store/")
        })
        .collect()
}

fn broad_wrapper_patterns() -> [&'static str; 19] {
    [
        "store.load_symbol_names(",
        "store.load_symbol_names_with_paths(",
        "store.load_symbol_names_for_paths(",
        "store.symbols_by_ids(",
        "store.symbols_by_name(",
        "store.resolve_record_candidates(",
        "store.members_for_records(",
        "store.fallback_member_candidates(",
        "store.fields_for_records(",
        "store.fallback_field_candidates(",
        "store.workspace_files_by_suffix(",
        "store.workspace_file_paths(",
        "store.indexed_workspace_files(",
        "store.load_include_edge_paths(",
        "store.open_include_file_paths(",
        "store.ambiguous_include_file_paths(",
        "store.load_include_data_for_sources(",
        "store.kind_counts_by_names(",
        "store.kind_counts_by_names_scoped(",
    ]
}

fn assert_wrappers_are_test_gated(path: &str, wrappers: &[&str]) {
    let source = read(path);
    for wrapper in wrappers {
        let needle = format!("pub fn {wrapper}");
        let Some(index) = source.find(&needle) else {
            continue;
        };
        let prefix = &source[..index];
        let window_start = prefix.len().saturating_sub(160);
        let attrs = &prefix[window_start..];
        assert!(
            attrs.contains("#[cfg(test)]"),
            "{path}::{wrapper} should be removed or restricted to test-only parity use"
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
fn broad_store_compatibility_wrappers_are_test_only_or_removed() {
    assert_wrappers_are_test_gated(
        "src/store/queries.rs",
        &[
            "load_symbol_names",
            "load_symbol_names_with_paths",
            "load_symbol_names_for_paths",
            "fallback_field_candidates",
            "resolve_record_candidates",
            "members_for_records",
            "fallback_member_candidates",
            "fields_for_records",
            "symbols_by_ids",
            "symbols_by_name",
        ],
    );
    assert_wrappers_are_test_gated(
        "src/store/includes.rs",
        &[
            "load_include_edge_paths",
            "open_include_file_paths",
            "ambiguous_include_file_paths",
            "load_include_data_for_sources",
        ],
    );
    assert_wrappers_are_test_gated(
        "src/store.rs",
        &[
            "workspace_files_by_suffix",
            "workspace_file_paths",
            "indexed_workspace_files",
        ],
    );
}

#[test]
fn production_features_do_not_use_broad_store_wrappers_when_views_exist() {
    let forbidden = broad_wrapper_patterns();
    for path in feature_production_sources() {
        assert_absent(&path, &forbidden);
    }
}

#[test]
fn read_view_guard_fixture_catches_broad_wrapper_usage() {
    let fixture = "fn bad(store: &IndexStore) { let _ = store.symbols_by_name(\"x\"); }";

    assert!(
        fixture.contains("store.symbols_by_name("),
        "guard fixture should model a broad durable read wrapper bypass"
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
