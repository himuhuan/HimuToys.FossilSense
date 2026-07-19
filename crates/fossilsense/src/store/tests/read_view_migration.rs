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

fn assert_present(path: &str, required: &[&str]) {
    let source = read(path);
    for pattern in required {
        assert!(
            source.contains(pattern),
            "{path} must route semantic results through `{pattern}`"
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
    assert_present(
        "src/server/indexing/cache.rs",
        &["NameTable::build_from_declaration_view("],
    );
    assert_absent(
        "src/server/indexing/cache.rs",
        &[
            "visit_core_rows(",
            "core_rows_for_paths(",
            "DeclarationCoreRow",
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

#[test]
fn core_symbol_features_route_through_candidate_sets_and_stable_handles() {
    for path in [
        "src/server/hover.rs",
        "src/server/navigation.rs",
        "src/server/signature_help.rs",
        "src/server/possible_targets.rs",
    ] {
        assert_present(path, &["semantic_candidates("]);
        assert_absent(path, &["non_callable_symbols("]);
    }
    assert_absent(
        "src/server/language_server.rs",
        &[
            "hydrate_ordinary_completion_candidates(",
            "semantic_candidates(&item.label",
        ],
    );
    assert_present(
        "src/server.rs",
        &["CompletionDocumentationData::Declaration"],
    );
    assert_present(
        "src/server/completion_candidate_documentation.rs",
        &[
            "new_with_declarations(",
            "resolve_candidate_handle(&handle)",
            "semantic_candidates(",
            "persistent_id == Some(declaration_id)",
        ],
    );
    assert_absent(
        "src/server/completion_candidate_documentation.rs",
        &["core_by_id("],
    );
    assert_present(
        "src/candidate_service/semantic.rs",
        &["exact_name_hits_scoped(", "payloads_by_ids(handle, &ids)"],
    );
    assert_absent(
        "src/server/completion_documentation.rs",
        &[
            "CompletionDocumentationData::Indexed",
            "CompletionDocumentationData::CurrentDocument",
            "CompletionDocumentationData::Overlay",
            "member.name == label",
            "member.signature == signature",
        ],
    );
}

#[test]
fn completion_recall_core_cannot_grow_into_a_parallel_semantic_model() {
    for path in [
        "src/declaration_index.rs",
        "src/query.rs",
        "src/server/indexing/cache.rs",
        "src/server/language_server.rs",
        "src/server/lsp_adapters.rs",
        "src/store/views/declarations.rs",
    ] {
        assert_absent(path, &["DeclarationCoreRow", "core_by_id("]);
    }
    assert_present(
        "src/declaration_index.rs",
        &[
            "names: Arc<NameTable>",
            "payloads_by_ids(",
            "total_budget_bytes.saturating_sub(accounted_core_bytes)",
        ],
    );
    assert_present(
        "src/completion/ordinary_service/providers.rs",
        &[
            "OrdinaryCompletionDocumentationTarget::Declaration",
            "declaration_id: hit.id",
            "declaration_name: hit.name.clone()",
        ],
    );
}
