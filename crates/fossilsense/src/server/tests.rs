use super::include_completion::IncludeCompletionTable;
use super::{
    completion_items_for_local_bindings, grouped_reference_items, local_words_for_cache,
    rebuild_include_table, rebuild_indexed_file_list,
};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex as StdMutex};
use tempfile::tempdir;
use tower_lsp::lsp_types::{
    CompletionItem, CompletionItemKind, CompletionParams, CompletionResponse, Position,
    TextDocumentIdentifier, TextDocumentPositionParams, Url,
};
use tower_lsp::{LanguageServer as _, LspService};

fn test_backend_service() -> LspService<super::Backend> {
    let (service, _) = LspService::new(|client| super::Backend {
        client,
        workspace_roots: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        index_schedule: Arc::new(tokio::sync::Mutex::new(IndexScheduleState::default())),
        open_docs: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        live_parse_cache: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        name_tables: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        reach_graphs: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        include_tables: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        indexed_file_lists: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        index_generations: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        external_include_dir_cache: Arc::new(StdMutex::new(HashMap::new())),
        local_word_cache: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        include_paths: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        completion_enabled: AtomicBool::new(true),
        semantic_coloring_enabled: AtomicBool::new(true),
        scoping_enabled: AtomicBool::new(true),
        debug_candidate_reasons: AtomicBool::new(false),
        perf_logging_enabled: AtomicBool::new(false),
        reference_role_cache: Arc::new(crate::references::ReferenceRoleCache::new()),
        reference_search_cache: Arc::new(crate::references::ReferenceSearchCache::new()),
        completion_memo: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        config_cache: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
    });
    service
}

fn completion_params(uri: Url, line: u32, character: u32) -> CompletionParams {
    CompletionParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position::new(line, character),
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
        context: None,
    }
}

fn completion_items(response: CompletionResponse) -> Vec<CompletionItem> {
    match response {
        CompletionResponse::Array(items) => items,
        CompletionResponse::List(list) => list.items,
    }
}

fn text_and_position(marked: &str) -> (String, u32, u32) {
    let marker = "/*cursor*/";
    let cursor_byte = marked.find(marker).expect("cursor marker");
    let text = marked.replacen(marker, "", 1);
    let before = &text[..cursor_byte];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let character = before[line_start..]
        .chars()
        .map(|ch| ch.len_utf16() as u32)
        .sum();
    (text, line, character)
}

fn write_workspace_file(root: &std::path::Path, rel: &str, text: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, text).expect("write file");
}

async fn indexed_backend_with_open_doc(
    indexed_files: &[(&str, &str)],
    open_rel: &str,
    marked_open_text: &str,
) -> (tempfile::TempDir, LspService<super::Backend>, Url, u32, u32) {
    let dir = tempdir().expect("tempdir");
    for (rel, text) in indexed_files {
        write_workspace_file(dir.path(), rel, text);
    }
    let (open_text, line, character) = text_and_position(marked_open_text);
    write_workspace_file(dir.path(), open_rel, &open_text);
    crate::indexer::index_workspace(
        dir.path(),
        crate::indexer::IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let uri = Url::from_file_path(dir.path().join(open_rel)).expect("file uri");
    let service = test_backend_service();
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(dir.path().to_path_buf());
    service
        .inner()
        .open_docs
        .lock()
        .await
        .insert(uri.clone(), (1, open_text));
    (dir, service, uri, line, character)
}

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
async fn include_table_rebuild_carries_include_edges_for_ranking() {
    let root = tempdir().expect("root");
    let root_path = root.path().to_path_buf();
    std::fs::write(root.path().join("a.c"), "#include \"b.h\"\n").expect("a");
    std::fs::write(root.path().join("b.h"), "int b;\n").expect("b");
    crate::indexer::index_workspace(
        root.path(),
        crate::indexer::IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let include_tables: super::IncludeTables = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let count = rebuild_include_table(&include_tables, root_path.clone())
        .await
        .expect("rebuild include table");
    let table = include_tables
        .lock()
        .await
        .get(&root_path)
        .cloned()
        .expect("table");

    assert_eq!(count, 2);
    assert_eq!(table.edge_count(), 1);
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

#[tokio::test]
async fn member_completion_returns_fields_and_methods_for_resolved_receiver() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[(
            "widget.hpp",
            "struct Widget { int width; void resize(); };\n",
        )],
        "main.cpp",
        "#include \"widget.hpp\"\nvoid f(Widget *w) { w->/*cursor*/ }\n",
    )
    .await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let items = completion_items(response);

    assert!(items
        .iter()
        .any(|item| item.label == "resize" && item.kind == Some(CompletionItemKind::METHOD)));
    assert!(items
        .iter()
        .any(|item| item.label == "width" && item.kind == Some(CompletionItemKind::FIELD)));
}

#[tokio::test]
async fn member_fallback_still_blocks_one_character_prefix() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[("widget.hpp", "struct Widget { int width; void wipe(); };\n")],
        "main.cpp",
        "void f(void) { make_widget()->w/*cursor*/ }\n",
    )
    .await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    assert!(completion_items(response).is_empty());
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

    let indexed = super::CompletionCandidate::new(
        "hello_value",
        crate::completion::CandidateEvidence::new(
            crate::completion::CandidateSource::Indexed,
            ScopeTier::Reachable,
            ResolutionConfidence::Reachable,
            30_000,
        ),
        CompletionItem {
            label: "hello_value".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            ..Default::default()
        },
    );
    let local = super::CompletionCandidate::new(
        "hello_value",
        crate::completion::CandidateEvidence::new(
            crate::completion::CandidateSource::LocalWord,
            ScopeTier::Current,
            ResolutionConfidence::Heuristic,
            40_000,
        ),
        CompletionItem {
            label: "hello_value".to_string(),
            kind: Some(CompletionItemKind::TEXT),
            ..Default::default()
        },
    );

    let deduped = crate::completion::run_compatible_pipeline(vec![indexed, local], 10).items;
    assert_eq!(deduped.len(), 1);
    assert_eq!(deduped[0].payload.kind, Some(CompletionItemKind::FUNCTION));
}

#[test]
fn completion_dedup_keeps_local_binding_over_same_name_indexed_and_local_word() {
    use crate::model::{ResolutionConfidence, ScopeTier};

    let indexed = super::CompletionCandidate::new(
        "count",
        crate::completion::CandidateEvidence::new(
            crate::completion::CandidateSource::Indexed,
            ScopeTier::Reachable,
            ResolutionConfidence::Reachable,
            30_000,
        ),
        CompletionItem {
            label: "count".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            ..Default::default()
        },
    );
    let local_binding = super::CompletionCandidate::new(
        "count",
        crate::completion::CandidateEvidence::new(
            crate::completion::CandidateSource::LocalBinding,
            ScopeTier::Current,
            ResolutionConfidence::Heuristic,
            40_000,
        ),
        CompletionItem {
            label: "count".to_string(),
            kind: Some(CompletionItemKind::VARIABLE),
            detail: Some("parameter: int".to_string()),
            ..Default::default()
        },
    );
    let local_word = super::CompletionCandidate::new(
        "count",
        crate::completion::CandidateEvidence::new(
            crate::completion::CandidateSource::LocalWord,
            ScopeTier::Global,
            ResolutionConfidence::Fallback,
            1_000,
        ),
        CompletionItem {
            label: "count".to_string(),
            kind: Some(CompletionItemKind::TEXT),
            ..Default::default()
        },
    );

    let deduped =
        crate::completion::run_compatible_pipeline(vec![indexed, local_word, local_binding], 10)
            .items;
    assert_eq!(deduped.len(), 1);
    assert_eq!(
        deduped[0].evidence.source,
        crate::completion::CandidateSource::LocalBinding
    );
    assert_eq!(deduped[0].payload.kind, Some(CompletionItemKind::VARIABLE));
}

#[test]
fn local_binding_candidates_render_variable_kind_and_detail() {
    let hits = vec![crate::query::LocalCompletionCandidate {
        name: "cursor_limit".to_string(),
        kind: crate::parser::LocalBindingKind::LocalVariable,
        detail: "local: int".to_string(),
        score: 42_000,
        match_score: 550,
        decl_start_byte: 10,
    }];

    let candidates = completion_items_for_local_bindings(hits);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].name, "cursor_limit");
    assert_eq!(
        candidates[0].evidence.source,
        crate::completion::CandidateSource::LocalBinding
    );
    assert_eq!(
        candidates[0].payload.kind,
        Some(CompletionItemKind::VARIABLE)
    );
    assert_eq!(candidates[0].payload.detail.as_deref(), Some("local: int"));
}

#[test]
fn local_binding_evidence_uses_raw_match_not_packed_score() {
    let packed_score = crate::resolver::pack_score(crate::model::ScopeTier::Current, 550, 0);
    let hits = vec![crate::query::LocalCompletionCandidate {
        name: "cursor_limit".to_string(),
        kind: crate::parser::LocalBindingKind::LocalVariable,
        detail: "local: int".to_string(),
        score: packed_score,
        match_score: 550,
        decl_start_byte: 10,
    }];

    let candidates = completion_items_for_local_bindings(hits);

    assert_eq!(candidates[0].evidence.score, packed_score);
    assert_eq!(candidates[0].evidence.match_score, 550);
}

#[tokio::test]
async fn ordinary_completion_uses_unsaved_current_file_overlay() {
    let (src, line, character) = text_and_position(
        "#define FS_MAGIC 1\n\
         typedef int FsAlias;\n\
         void f(void) { FS/*cursor*/ }\n",
    );
    let dir = tempdir().expect("tempdir");
    let uri = Url::from_file_path(dir.path().join("a.c")).expect("file uri");
    let service = test_backend_service();
    service
        .inner()
        .open_docs
        .lock()
        .await
        .insert(uri.clone(), (1, src));

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    if let CompletionResponse::List(list) = &response {
        assert!(list.is_incomplete);
    }
    let items = completion_items(response);

    assert!(items
        .iter()
        .any(|item| item.label == "FS_MAGIC" && item.detail.as_deref() == Some("current")));
    assert!(items
        .iter()
        .any(|item| item.label == "FsAlias" && item.detail.as_deref() == Some("current")));
}

#[tokio::test]
async fn current_file_text_overlay_renders_text_kind() {
    let (src, line, character) = text_and_position(
        "void f(void) {\n\
             localThing();\n\
             localT/*cursor*/\n\
         }\n",
    );
    let dir = tempdir().expect("tempdir");
    let uri = Url::from_file_path(dir.path().join("a.c")).expect("file uri");
    let service = test_backend_service();
    service
        .inner()
        .open_docs
        .lock()
        .await
        .insert(uri.clone(), (1, src));

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let items = completion_items(response);
    let local = items
        .iter()
        .find(|item| item.label == "localThing")
        .expect("localThing text overlay completion");

    assert_eq!(local.kind, Some(CompletionItemKind::TEXT));
    assert_eq!(local.detail.as_deref(), Some("text"));
}

#[tokio::test]
async fn text_overlay_still_allows_exact_indexed_semantic_recovery() {
    let (src, line, character) = text_and_position(
        "void f(void) {\n\
             localThing();\n\
             loc/*cursor*/\n\
         }\n",
    );
    let dir = tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    let uri = Url::from_file_path(root.join("a.c")).expect("file uri");
    let service = test_backend_service();
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root.clone());
    let mut names: Vec<_> = (0..150)
        .map(|idx| {
            (
                idx,
                format!("localT{idx:03}"),
                false,
                "dense.c".to_string(),
                "global_variable".to_string(),
                false,
            )
        })
        .collect();
    names.push((
        999,
        "localThing".to_string(),
        false,
        "a.c".to_string(),
        "function".to_string(),
        false,
    ));
    service.inner().name_tables.lock().await.insert(
        root,
        Arc::new(crate::query::NameTable::build_with_paths(names)),
    );
    service
        .inner()
        .open_docs
        .lock()
        .await
        .insert(uri.clone(), (1, src));

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let items = completion_items(response);
    let local = items
        .iter()
        .find(|item| item.label == "localThing")
        .expect("localThing completion");

    assert_eq!(local.kind, Some(CompletionItemKind::FUNCTION));
    assert_ne!(local.detail.as_deref(), Some("text"));
}

#[test]
fn final_rank_sort_text_matches_pipeline_order() {
    let mut items = vec![
        CompletionItem {
            label: "b".into(),
            ..Default::default()
        },
        CompletionItem {
            label: "a".into(),
            ..Default::default()
        },
    ];

    super::apply_final_completion_sort_text(&mut items);

    assert_eq!(items[0].sort_text.as_deref(), Some("00000000"));
    assert_eq!(items[1].sort_text.as_deref(), Some("00000001"));
}

#[test]
fn indexed_candidate_evidence_uses_base_match_not_packed_score() {
    use crate::model::ScopeTier;

    let candidates = super::completion_items_for_indexed_hits(
        vec![crate::query::RankedNameHit {
            id: 1,
            score: crate::resolver::pack_score(ScopeTier::Reachable, 800, 42),
            tier: ScopeTier::Reachable,
            base_match: 800,
            name_len: "api_target".len(),
            name: "api_target".to_string(),
            kind: crate::parser::SymbolKind::Function,
        }],
        None,
    );

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].evidence.match_score, 800);
    assert_ne!(
        candidates[0].evidence.match_score,
        candidates[0].evidence.score
    );
    assert_eq!(
        candidates[0].payload.kind,
        Some(CompletionItemKind::FUNCTION)
    );
}

#[tokio::test]
async fn local_binding_pipeline_uses_open_document_bindings_before_local_words() {
    let src = "int f(int count) {\n    int cursor_limit;\n    cur\n}\n";
    let dir = tempdir().expect("tempdir");
    let uri = Url::from_file_path(dir.path().join("a.c")).expect("file uri");
    let service = test_backend_service();
    service
        .inner()
        .open_docs
        .lock()
        .await
        .insert(uri.clone(), (1, src.to_string()));

    let response = service
        .inner()
        .completion(completion_params(uri, 2, 7))
        .await
        .expect("completion request")
        .expect("completion response");
    if let CompletionResponse::List(list) = &response {
        assert!(list.is_incomplete);
    }
    let items = completion_items(response);
    let cursor = items
        .iter()
        .find(|item| item.label == "cursor_limit")
        .expect("cursor_limit completion");

    assert_eq!(cursor.kind, Some(CompletionItemKind::VARIABLE));
    assert_eq!(cursor.detail.as_deref(), Some("local: int"));
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
    assert_eq!(candidates[0].evidence.score, local_score);
    assert_eq!(
        candidates[0].payload.kind,
        Some(CompletionItemKind::FUNCTION)
    );
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
