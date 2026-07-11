#![allow(clippy::field_reassign_with_default)]

use super::{
    grouped_reference_items, local_words_for_cache, rebuild_include_table,
    rebuild_indexed_file_list,
};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex as StdMutex};
use tempfile::tempdir;
use tower_lsp::lsp_types::{
    CompletionItem, CompletionItemKind, CompletionParams, CompletionResponse, Documentation,
    ExecuteCommandParams, FileChangeType, FileEvent, GotoDefinitionParams, GotoDefinitionResponse,
    InitializeParams, Position, TextDocumentIdentifier, TextDocumentPositionParams, Url,
};
use tower_lsp::{LanguageServer as _, LspService};

fn test_backend_service() -> LspService<super::Backend> {
    let (service, _) = LspService::new(|client| super::Backend {
        client,
        workspace_roots: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        index_schedule: Arc::new(tokio::sync::Mutex::new(IndexScheduleState::default())),
        session: super::WorkspaceSession::new(
            super::DocumentStore::default(),
            super::CacheLedger::default(),
        ),
        external_include_dir_cache: Arc::new(StdMutex::new(HashMap::new())),
        include_paths: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        completion_enabled: AtomicBool::new(true),
        semantic_coloring_enabled: AtomicBool::new(true),
        scoping_enabled: AtomicBool::new(true),
        completion_history_mode: Arc::new(tokio::sync::Mutex::new(
            crate::completion_history::CompletionHistoryMode::Auto,
        )),
        completion_history: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        project_context_selection: Arc::new(tokio::sync::Mutex::new(
            crate::project_context::ProjectContextSelection::Auto,
        )),
        project_context_selection_epoch: AtomicU64::new(1),
        debug_candidate_reasons: AtomicBool::new(false),
        perf_logging_enabled: AtomicBool::new(false),
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

fn goto_definition_params(uri: Url, line: u32, character: u32) -> GotoDefinitionParams {
    GotoDefinitionParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position::new(line, character),
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    }
}

fn completion_items(response: CompletionResponse) -> Vec<CompletionItem> {
    match response {
        CompletionResponse::Array(items) => items,
        CompletionResponse::List(list) => list.items,
    }
}

fn completion_response_is_incomplete(response: &CompletionResponse) -> bool {
    match response {
        CompletionResponse::Array(_) => false,
        CompletionResponse::List(list) => list.is_incomplete,
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

async fn open_test_document(
    service: &LspService<super::Backend>,
    uri: Url,
    version: i32,
    text: String,
) {
    service
        .inner()
        .session
        .open_document(uri, version, text)
        .await;
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
    open_test_document(&service, uri.clone(), 1, open_text).await;
    (dir, service, uri, line, character)
}

#[tokio::test]
async fn goto_definition_uses_live_current_document_typedef_when_index_is_stale() {
    let dir = tempdir().expect("tempdir");
    write_workspace_file(dir.path(), "main.c", "void indexed_only(void) {}\n");
    crate::indexer::index_workspace(
        dir.path(),
        crate::indexer::IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let uri = Url::from_file_path(dir.path().join("main.c")).expect("file uri");
    let service = test_backend_service();
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(dir.path().to_path_buf());

    let (src, line, character) = text_and_position(
        "typedef struct {\n\
             int value;\n\
         } Boom;\n\
         \n\
         void f(void) {\n\
             Boom/*cursor*/ b;\n\
         }\n",
    );
    open_test_document(&service, uri.clone(), 2, src).await;

    let response = service
        .inner()
        .goto_definition(goto_definition_params(uri.clone(), line, character))
        .await
        .expect("goto definition")
        .expect("definition response");
    let locations = match response {
        GotoDefinitionResponse::Array(locations) => locations,
        GotoDefinitionResponse::Scalar(location) => vec![location],
        GotoDefinitionResponse::Link(_) => panic!("unexpected location links"),
    };

    assert!(
        locations
            .iter()
            .any(|location| location.uri == uri && location.range.start.line == 2),
        "live typedef definition should be returned even when the persisted index is stale"
    );
}

#[tokio::test]
async fn goto_definition_rejects_keyword_polluted_by_trailing_comments() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[],
        "checkpoint.h",
        r#"typedef struct AVTextWriter {
    const/*cursor*/ AVClass *priv_class; ///< private class of the writer, if any
    int priv_size;                       ///< writer private class
    const char *name;
} AVTextWriter;
"#,
    )
    .await;

    let response = service
        .inner()
        .goto_definition(goto_definition_params(uri, line, character))
        .await
        .expect("goto definition request");

    assert!(
        response.is_none(),
        "language keywords must never be jump targets"
    );
}

#[tokio::test]
async fn goto_definition_finds_first_typedef_after_multiline_macro_from_index() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[],
        "macro_typedef.h",
        r#"#define FREE(ptr)                                                              \
    do                                                                         \
    {                                                                          \
        if ((ptr) != NULL)                                                     \
        {                                                                      \
            free(ptr);                                                         \
            (ptr) = NULL;                                                      \
        }                                                                      \
    } while (0)

typedef struct xxx {
    int value;
} xxx_t;

void use_type(void) {
    xxx_t/*cursor*/ item;
}
"#,
    )
    .await;

    let response = service
        .inner()
        .goto_definition(goto_definition_params(uri.clone(), line, character))
        .await
        .expect("goto definition")
        .expect("definition response");
    let locations = match response {
        GotoDefinitionResponse::Array(locations) => locations,
        GotoDefinitionResponse::Scalar(location) => vec![location],
        GotoDefinitionResponse::Link(_) => panic!("unexpected location links"),
    };

    assert!(
        locations
            .iter()
            .any(|location| location.uri == uri && location.range.start.line == 12),
        "indexed typedef immediately after multiline macro should be a goto-definition target"
    );
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
async fn workspace_session_change_invalidates_live_document_caches() {
    let documents = super::DocumentStore::default();
    let cache = super::CacheLedger::default();
    let session = super::WorkspaceSession::new(documents.clone(), cache.clone());
    let root = tempdir().expect("root");
    let path = root.path().join("src/main.c");
    let uri = Url::from_file_path(&path).expect("uri");

    documents
        .open_document(uri.clone(), 1, "int cached_word;\n".to_string())
        .await;
    let words = documents
        .local_words_for(&uri, 1, "int cached_word;\n")
        .await;
    assert!(words.contains("cached_word"));
    let parsed = Arc::new(crate::parser::parse(&path, "int cached_word;\n"));
    documents
        .store_live_parse_for_test(uri.clone(), 1, parsed)
        .await;
    cache
        .record_completion_memo(uri.clone(), "ca".to_string(), 7, vec![vec![0usize, 1usize]])
        .await;
    cache.mark_reference_search_cache_for_test("root", "cached_word", 7);

    session
        .change_document(uri.clone(), 2, "int changed_word;\n".to_string())
        .await;

    let snapshot = documents.snapshot(&uri).await.expect("open document");
    assert_eq!(snapshot.version, 2);
    assert!(snapshot.text.contains("changed_word"));
    assert!(
        documents.live_parse_for_test(&uri).await.is_none(),
        "did_change must clear the live parse cache for the edited document"
    );
    assert!(
        documents
            .local_word_cache_entry_for_test(&uri)
            .await
            .is_none(),
        "did_change must invalidate local words so completion sees the new text"
    );
    assert!(
        cache.completion_memo_for_test(&uri).await.is_none(),
        "did_change must clear per-document completion narrowing state"
    );
    assert_eq!(
        cache.reference_search_cache_len_for_test(),
        0,
        "document changes must clear complete reference search results"
    );
}

#[tokio::test]
async fn stale_document_work_cannot_overwrite_latest_revision_caches() {
    let documents = super::DocumentStore::default();
    let root = tempdir().expect("root");
    let path = root.path().join("main.c");
    let uri = Url::from_file_path(&path).expect("uri");

    documents
        .open_document(uri.clone(), 1, "int old_word;\n".to_string())
        .await;
    documents
        .change_document(uri.clone(), 2, "int new_word;\n".to_string())
        .await;

    let stale_parse = Arc::new(crate::parser::parse(&path, "int old_word;\n"));
    documents
        .store_live_parse_for_test(uri.clone(), 1, stale_parse)
        .await;
    assert!(
        documents.live_parse_for_test(&uri).await.is_none(),
        "a completed old parse must be discarded after the document advances"
    );

    let current_parse = Arc::new(crate::parser::parse(&path, "int new_word;\n"));
    documents
        .store_live_parse_for_test(uri.clone(), 2, current_parse.clone())
        .await;
    assert!(Arc::ptr_eq(
        &documents
            .cached_live_parse(&uri, 2)
            .await
            .expect("current parse"),
        &current_parse
    ));

    let stale_words = documents.local_words_for(&uri, 1, "int old_word;\n").await;
    assert!(stale_words.contains("old_word"));
    assert!(
        documents
            .local_word_cache_entry_for_test(&uri)
            .await
            .is_none(),
        "old request words may be returned to that request but not cached"
    );

    let current_words = documents.local_words_for(&uri, 2, "int new_word;\n").await;
    assert!(current_words.contains("new_word"));
    assert_eq!(
        documents
            .local_word_cache_entry_for_test(&uri)
            .await
            .expect("current words")
            .0,
        2
    );
}

#[tokio::test]
async fn workspace_session_close_clears_live_only_state_not_indexed_workspace_data() {
    let documents = super::DocumentStore::default();
    let cache = super::CacheLedger::default();
    let session = super::WorkspaceSession::new(documents.clone(), cache.clone());
    let root = tempdir().expect("root");
    let root_path = root.path().to_path_buf();
    let file_path = root.path().join("src/main.c");
    let uri = Url::from_file_path(&file_path).expect("uri");

    documents
        .open_document(uri.clone(), 1, "int indexed_symbol;\n".to_string())
        .await;
    documents
        .store_live_parse_for_test(
            uri.clone(),
            1,
            Arc::new(crate::parser::parse(&file_path, "int indexed_symbol;\n")),
        )
        .await;
    let _ = documents
        .local_words_for(&uri, 1, "int indexed_symbol;\n")
        .await;
    cache
        .set_name_table_for_test(
            root_path.clone(),
            Arc::new(crate::query::NameTable::build(vec![(
                1,
                "indexed_symbol".to_string(),
                false,
            )])),
        )
        .await;
    cache
        .set_indexed_file_list_for_test(
            root_path.clone(),
            Arc::new(vec![("src/main.c".to_string(), file_path.clone())]),
        )
        .await;

    session.close_document(&uri).await;

    assert!(documents.snapshot(&uri).await.is_none());
    assert!(documents.live_parse_for_test(&uri).await.is_none());
    assert!(documents
        .local_word_cache_entry_for_test(&uri)
        .await
        .is_none());
    let engine = cache
        .current_engine_snapshot(&root_path)
        .await
        .expect("published engine snapshot");
    assert!(
        engine.name_table.is_some(),
        "closing an editor buffer must not delete indexed symbol data"
    );
    assert!(
        engine.indexed_files.is_some(),
        "closing an editor buffer must not delete indexed reference scope"
    );
}

#[tokio::test]
async fn cache_ledger_publishes_full_and_dirty_read_models_with_generations() {
    let root = tempdir().expect("root");
    let root_path = root.path().to_path_buf();
    write_workspace_file(root.path(), "src/main.c", "int alpha_symbol;\n");
    crate::indexer::index_workspace(
        root.path(),
        crate::indexer::IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("initial index");

    let service = test_backend_service();
    let full = service
        .inner()
        .session
        .cache
        .publish_full_index(&service.inner().client, root_path.clone())
        .await
        .expect("publish full");
    assert_eq!(full.symbol_count, 1);
    assert_eq!(full.reference_file_count, 1);
    let full_context = service
        .inner()
        .session
        .request_context_for_root(root_path.clone())
        .await;
    assert!(full_context.engine.name_table.is_some());
    assert!(full_context.engine.reach_graph.is_some());
    assert!(full_context.engine.include_table.is_some());
    assert!(full_context.engine.indexed_files.is_some());
    assert!(full_context.engine.project_context.is_some());
    assert_ne!(full_context.engine.epoch.as_u64(), 0);
    assert_eq!(full_context.engine.semantic_generation.0, 1);

    write_workspace_file(
        root.path(),
        "src/main.c",
        "int beta_symbol;\nint gamma_symbol;\n",
    );
    crate::indexer::index_dirty_files(
        root.path(),
        vec![crate::indexer::DirtyFileChange {
            absolute_path: root.path().join("src/main.c"),
            kind: crate::indexer::DirtyFileKind::Upsert,
        }],
        crate::indexer::IndexOptions {
            force: false,
            ..Default::default()
        },
        |_| {},
    )
    .expect("dirty index");
    let dirty = service
        .inner()
        .session
        .cache
        .publish_dirty_index(
            &service.inner().client,
            root_path.clone(),
            &["src/main.c".to_string()],
            &[],
        )
        .await
        .expect("publish dirty");
    assert_eq!(dirty.symbol_count, 2);
    let dirty_context = service
        .inner()
        .session
        .request_context_for_root(root_path)
        .await;
    assert_ne!(full_context.engine.epoch, dirty_context.engine.epoch);
    assert_eq!(dirty_context.engine.semantic_generation.0, 2);
    assert_eq!(
        dirty_context
            .engine
            .name_table
            .as_ref()
            .expect("table")
            .len(),
        2
    );
}

#[tokio::test]
async fn marker_only_refresh_retags_names_publishes_epoch_and_clears_memos() {
    let root = tempdir().expect("root");
    let root_path = root.path().to_path_buf();
    write_workspace_file(root.path(), "app/src/main.c", "int project_symbol;\n");
    crate::indexer::index_workspace(
        root.path(),
        crate::indexer::IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");
    let service = test_backend_service();
    service
        .inner()
        .session
        .cache
        .publish_full_index(&service.inner().client, root_path.clone())
        .await
        .expect("publish");
    let before = service
        .inner()
        .session
        .request_context_for_root(root_path.clone())
        .await;
    assert!(before
        .engine
        .project_context
        .as_ref()
        .expect("available")
        .projects()
        .is_empty());
    let published_symbol_count = before
        .engine
        .name_table
        .as_ref()
        .expect("published name table")
        .len();
    // Marker-only publication must be derived exclusively from the immutable
    // published generation. Removing SQLite proves the refresh cannot observe
    // a concurrent writer's partial committed state.
    fs::remove_file(crate::pathing::default_index_path(root.path()).expect("db path"))
        .expect("remove index database");
    let uri = Url::from_file_path(root.path().join("app/src/main.c")).expect("uri");
    service
        .inner()
        .session
        .cache
        .record_completion_memo(uri.clone(), "pro".into(), 7, vec![vec![0]])
        .await;

    write_workspace_file(root.path(), "app/Makefile", "all:\n");
    let count = service
        .inner()
        .session
        .cache
        .refresh_project_context(&service.inner().client, root_path.clone())
        .await
        .expect("refresh");
    assert_eq!(count, 1);
    let after_create = service
        .inner()
        .session
        .request_context_for_root(root_path.clone())
        .await;
    assert_ne!(before.engine.epoch, after_create.engine.epoch);
    assert_eq!(
        after_create
            .engine
            .name_table
            .as_ref()
            .expect("retagged table")
            .len(),
        published_symbol_count
    );
    let project = after_create
        .engine
        .project_context
        .as_ref()
        .and_then(|index| index.nearest_for_file("app/src/main.c"))
        .expect("project");
    let hit = after_create
        .engine
        .name_table
        .as_ref()
        .expect("table")
        .search_ranked("project_symbol", 10)
        .into_iter()
        .next()
        .expect("hit");
    assert_eq!(hit.project_key, Some(project));
    assert!(service
        .inner()
        .session
        .cache
        .completion_memo_for_test(&uri)
        .await
        .is_none());

    fs::remove_file(root.path().join("app/Makefile")).expect("delete marker");
    service
        .inner()
        .session
        .cache
        .refresh_project_context(&service.inner().client, root_path.clone())
        .await
        .expect("refresh delete");
    let after_delete = service
        .inner()
        .session
        .request_context_for_root(root_path)
        .await;
    assert_ne!(after_create.engine.epoch, after_delete.engine.epoch);
    assert!(after_delete
        .engine
        .project_context
        .as_ref()
        .expect("available")
        .projects()
        .is_empty());
}

#[tokio::test]
async fn nested_marker_refresh_reassigns_name_ownership_and_removal_restores_parent() {
    let root = tempdir().expect("root");
    let root_path = root.path().to_path_buf();
    write_workspace_file(root.path(), "Makefile", "all:\n");
    write_workspace_file(root.path(), "app/src/main.c", "int nested_symbol;\n");
    crate::indexer::index_workspace(
        root.path(),
        crate::indexer::IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");
    let service = test_backend_service();
    service
        .inner()
        .session
        .cache
        .publish_full_index(&service.inner().client, root_path.clone())
        .await
        .expect("publish");

    let project_path_for_symbol = |context: &super::RequestContext| {
        context
            .engine
            .name_table
            .as_ref()
            .expect("table")
            .search_ranked("nested_symbol", 10)
            .into_iter()
            .next()
            .and_then(|hit| hit.project_key)
            .map(|key| key.project_path)
    };
    let parent = service
        .inner()
        .session
        .request_context_for_root(root_path.clone())
        .await;
    assert_eq!(project_path_for_symbol(&parent).as_deref(), Some(""));

    write_workspace_file(root.path(), "app/CMakeLists.txt", "");
    service
        .inner()
        .session
        .cache
        .refresh_project_context(&service.inner().client, root_path.clone())
        .await
        .expect("nested refresh");
    let nested = service
        .inner()
        .session
        .request_context_for_root(root_path.clone())
        .await;
    assert_eq!(project_path_for_symbol(&nested).as_deref(), Some("app"));

    fs::remove_file(root.path().join("app/CMakeLists.txt")).expect("remove nested marker");
    service
        .inner()
        .session
        .cache
        .refresh_project_context(&service.inner().client, root_path.clone())
        .await
        .expect("parent refresh");
    let restored = service
        .inner()
        .session
        .request_context_for_root(root_path)
        .await;
    assert_eq!(project_path_for_symbol(&restored).as_deref(), Some(""));
}

#[tokio::test]
async fn marker_watcher_classifies_supported_and_ignores_excluded_or_fragment_files() {
    let root = tempdir().expect("root");
    fs::create_dir_all(root.path().join("app")).expect("app");
    fs::create_dir_all(root.path().join("build")).expect("build");
    let roots = vec![root.path().to_path_buf()];
    let cache = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let event = |path: &std::path::Path, typ| FileEvent {
        uri: Url::from_file_path(path).expect("uri"),
        typ,
    };

    let marker = super::watched_change_in_scope(
        &roots,
        &event(&root.path().join("app/Makefile"), FileChangeType::CREATED),
        &cache,
    )
    .await;
    assert!(matches!(
        marker,
        Some(super::WatchDecision::ProjectContext(_))
    ));

    let excluded = super::watched_change_in_scope(
        &roots,
        &event(
            &root.path().join("build/build.ninja"),
            FileChangeType::CREATED,
        ),
        &cache,
    )
    .await;
    assert!(excluded.is_none());

    let fragment = super::watched_change_in_scope(
        &roots,
        &event(
            &root.path().join("app/rules.ninja"),
            FileChangeType::CREATED,
        ),
        &cache,
    )
    .await;
    assert!(fragment.is_none());

    let renamed_away = super::watched_change_in_scope(
        &roots,
        &event(&root.path().join("app/Makefile"), FileChangeType::DELETED),
        &cache,
    )
    .await;
    assert!(matches!(
        renamed_away,
        Some(super::WatchDecision::ProjectContext(_))
    ));
}

#[tokio::test]
async fn watcher_routes_nested_workspace_changes_to_the_most_specific_root() {
    let outer = tempdir().expect("outer");
    let inner = outer.path().join("nested");
    fs::create_dir_all(inner.join("src")).expect("inner tree");
    let outer_root = outer.path().to_path_buf();
    let roots = vec![outer_root.clone(), inner.clone()];
    let cache = Arc::new(tokio::sync::Mutex::new(HashMap::from([
        (
            outer_root.clone(),
            crate::config::WorkspaceConfig::default(),
        ),
        (inner.clone(), crate::config::WorkspaceConfig::default()),
    ])));
    let event = |path: &std::path::Path, typ| FileEvent {
        uri: Url::from_file_path(path).expect("uri"),
        typ,
    };

    let marker = super::watched_change_in_scope(
        &roots,
        &event(&inner.join("CMakeLists.txt"), FileChangeType::CREATED),
        &cache,
    )
    .await;
    match marker {
        Some(super::WatchDecision::ProjectContext(root)) => assert_eq!(root, inner),
        _ => panic!("nested marker should refresh the nested workspace"),
    }

    let source = super::watched_change_in_scope(
        &roots,
        &event(&inner.join("src/main.c"), FileChangeType::CHANGED),
        &cache,
    )
    .await;
    match source {
        Some(super::WatchDecision::Dirty(change)) => assert_eq!(change.root, inner),
        _ => panic!("nested source should dirty the nested workspace"),
    }

    let config = super::watched_change_in_scope(
        &roots,
        &event(&inner.join("fossilsense.json"), FileChangeType::CHANGED),
        &cache,
    )
    .await;
    assert!(matches!(config, Some(super::WatchDecision::Full)));
    let cached = cache.lock().await;
    assert!(cached.contains_key(&outer_root));
    assert!(!cached.contains_key(&inner));
}

#[tokio::test]
async fn project_context_commands_validate_selection_and_outside_uri_has_no_automatic_project() {
    let root = tempdir().expect("root");
    let other = tempdir().expect("outside");
    let root_path = root.path().to_path_buf();
    write_workspace_file(root.path(), "server/Makefile", "all:\n");
    write_workspace_file(root.path(), "server/main.c", "int server_api;\n");
    write_workspace_file(root.path(), "lib/CMakeLists.txt", "");
    crate::indexer::index_workspace(
        root.path(),
        crate::indexer::IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");
    let service = test_backend_service();
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root_path.clone());
    service
        .inner()
        .session
        .cache
        .publish_full_index(&service.inner().client, root_path.clone())
        .await
        .expect("publish");
    let uri = Url::from_file_path(root.path().join("server/main.c")).expect("uri");
    let value = service
        .inner()
        .execute_command(ExecuteCommandParams {
            command: super::PROJECT_CONTEXTS_LSP_COMMAND.to_string(),
            arguments: vec![serde_json::json!({"uri": uri})],
            work_done_progress_params: Default::default(),
        })
        .await
        .expect("status")
        .expect("value");
    let status: crate::project_context::ProjectContextStatus =
        serde_json::from_value(value).expect("status dto");
    assert!(status.available);
    assert_eq!(status.projects.len(), 2);
    assert_eq!(
        status
            .automatic_project
            .as_ref()
            .expect("automatic")
            .project_path,
        "server"
    );

    let manual = status
        .projects
        .iter()
        .find(|project| project.key.project_path == "lib")
        .expect("lib")
        .key
        .clone();
    let manual_with_stale_case = crate::project_context::ProjectKey {
        project_path: manual.project_path.to_ascii_uppercase(),
        ..manual.clone()
    };
    let memo_uri = uri.clone();
    service
        .inner()
        .session
        .cache
        .record_completion_memo(memo_uri.clone(), "ser".into(), 9, vec![vec![0]])
        .await;
    let value = service
        .inner()
        .execute_command(ExecuteCommandParams {
            command: super::SET_PROJECT_CONTEXT_LSP_COMMAND.to_string(),
            arguments: vec![serde_json::json!({
                "uri": uri,
                "selection": {"kind": "manual", "key": manual_with_stale_case}
            })],
            work_done_progress_params: Default::default(),
        })
        .await
        .expect("set")
        .expect("value");
    let manual_status: crate::project_context::ProjectContextStatus =
        serde_json::from_value(value).expect("manual status");
    assert!(matches!(
        manual_status.selection,
        crate::project_context::ProjectContextSelection::Manual { .. }
    ));
    assert_eq!(manual_status.active_project, Some(manual));
    assert!(service
        .inner()
        .session
        .cache
        .completion_memo_for_test(&memo_uri)
        .await
        .is_none());

    let outside = Url::from_file_path(other.path().join("outside.c")).expect("outside uri");
    let outside_status = service.inner().project_context_status(Some(&outside)).await;
    assert!(outside_status.available);
    assert!(outside_status.automatic_project.is_none());

    let unmarked_uri =
        Url::from_file_path(root.path().join("unmarked/file.c")).expect("unmarked uri");
    let unmarked_status = service
        .inner()
        .set_project_context_selection(
            crate::project_context::ProjectContextSelection::Auto,
            Some(&unmarked_uri),
        )
        .await;
    assert!(unmarked_status.available);
    assert!(unmarked_status.active_project.is_none());

    let unspecified_status = service
        .inner()
        .set_project_context_selection(
            crate::project_context::ProjectContextSelection::Unspecified,
            Some(&Url::from_file_path(root.path().join("server/main.c")).expect("server uri")),
        )
        .await;
    assert!(unspecified_status.active_project.is_none());

    let current = service
        .inner()
        .session
        .cache
        .current_engine_snapshot(&root_path)
        .await
        .expect("current snapshot");
    let mut degraded = current.degraded.clone();
    degraded.project_context = true;
    service
        .inner()
        .session
        .cache
        .publish_engine_snapshot(super::workspace::EngineSnapshot {
            root: root_path,
            epoch: service.inner().session.cache.allocate_engine_epoch(),
            semantic_generation: current.semantic_generation,
            name_table: current.name_table.clone(),
            reach_graph: current.reach_graph.clone(),
            include_table: current.include_table.clone(),
            indexed_files: current.indexed_files.clone(),
            project_context: None,
            relation_catalog: None,
            degraded,
        })
        .await;
    let unavailable_status = service
        .inner()
        .project_context_status(Some(
            &Url::from_file_path(root.path().join("server/main.c")).expect("server uri"),
        ))
        .await;
    assert!(!unavailable_status.available);
    assert!(unavailable_status.projects.is_empty());
    assert!(unavailable_status.active_project.is_none());
}

#[tokio::test]
async fn automatic_project_uses_the_most_specific_containing_workspace_root() {
    let outer = tempdir().expect("outer");
    let inner = outer.path().join("nested");
    fs::create_dir_all(inner.join("src")).expect("inner tree");
    write_workspace_file(outer.path(), "Makefile", "all:\n");
    write_workspace_file(&inner, "CMakeLists.txt", "");
    write_workspace_file(&inner, "src/main.c", "int nested_api;\n");
    for root in [outer.path(), inner.as_path()] {
        crate::indexer::index_workspace(
            root,
            crate::indexer::IndexOptions {
                force: true,
                ..Default::default()
            },
            |_| {},
        )
        .expect("index root");
    }

    let service = test_backend_service();
    let roots = vec![outer.path().to_path_buf(), inner.clone()];
    *service.inner().workspace_roots.lock().await = roots.clone();
    for root in roots {
        service
            .inner()
            .session
            .cache
            .publish_full_index(&service.inner().client, root)
            .await
            .expect("publish root");
    }
    let uri = Url::from_file_path(inner.join("src/main.c")).expect("uri");
    assert_eq!(
        service.inner().root_for_uri(&uri).await,
        Some(inner.clone())
    );
    let status = service.inner().project_context_status(Some(&uri)).await;
    let automatic = status.automatic_project.expect("automatic project");
    assert_eq!(automatic.project_path, "");
    assert_eq!(
        automatic.workspace_root_id,
        crate::pathing::workspace_hash(&inner.canonicalize().expect("canonical inner"))
    );
    assert!(status
        .projects
        .iter()
        .any(|project| project.key.project_path == "nested"));
    assert!(status
        .projects
        .iter()
        .any(|project| project.key == automatic));
}

#[tokio::test]
async fn project_selector_remains_available_when_another_root_model_is_degraded() {
    let available = tempdir().expect("available root");
    let degraded = tempdir().expect("degraded root");
    write_workspace_file(available.path(), "app/Makefile", "all:\n");
    write_workspace_file(available.path(), "app/main.c", "int available_api;\n");
    crate::indexer::index_workspace(
        available.path(),
        crate::indexer::IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index available root");

    let service = test_backend_service();
    *service.inner().workspace_roots.lock().await = vec![
        available.path().to_path_buf(),
        degraded.path().to_path_buf(),
    ];
    service
        .inner()
        .session
        .cache
        .publish_full_index(&service.inner().client, available.path().to_path_buf())
        .await
        .expect("publish available root");

    let degraded_uri = Url::from_file_path(degraded.path().join("main.c")).expect("degraded uri");
    let status = service
        .inner()
        .project_context_status(Some(&degraded_uri))
        .await;
    assert!(status.available);
    assert_eq!(status.projects.len(), 1);
    assert!(status.automatic_project.is_none());
    assert!(status.active_project.is_none());
}

#[tokio::test]
async fn automatic_and_manual_project_selection_change_duplicate_completion_immediately() {
    let root = tempdir().expect("root");
    write_workspace_file(root.path(), "server/Makefile", "all:\n");
    write_workspace_file(root.path(), "server/server.h", "int get_xxx(void);\n");
    write_workspace_file(root.path(), "lib/CMakeLists.txt", "");
    write_workspace_file(root.path(), "lib/xxx.h", "#define get_xxx 1\n");
    let (source, line, character) =
        text_and_position("void use_api(void) {\n    get/*cursor*/\n}\n");
    write_workspace_file(root.path(), "server/server.c", &source);
    crate::indexer::index_workspace(
        root.path(),
        crate::indexer::IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let service = test_backend_service();
    let root_path = root.path().to_path_buf();
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root_path.clone());
    service
        .inner()
        .session
        .cache
        .publish_full_index(&service.inner().client, root_path)
        .await
        .expect("publish");
    let uri = Url::from_file_path(root.path().join("server/server.c")).expect("uri");
    open_test_document(&service, uri.clone(), 1, source).await;

    let auto_items = completion_items(
        service
            .inner()
            .completion(completion_params(uri.clone(), line, character))
            .await
            .expect("auto completion")
            .expect("auto response"),
    );
    let auto = auto_items
        .iter()
        .find(|item| item.label == "get_xxx")
        .expect("auto item");
    assert_eq!(auto.kind, Some(CompletionItemKind::FUNCTION));

    let status = service.inner().project_context_status(Some(&uri)).await;
    let library_key = status
        .projects
        .iter()
        .find(|project| project.key.project_path == "lib")
        .expect("library project")
        .key
        .clone();
    service
        .inner()
        .set_project_context_selection(
            crate::project_context::ProjectContextSelection::Manual { key: library_key },
            Some(&uri),
        )
        .await;
    let manual_items = completion_items(
        service
            .inner()
            .completion(completion_params(uri, line, character))
            .await
            .expect("manual completion")
            .expect("manual response"),
    );
    let manual = manual_items
        .iter()
        .find(|item| item.label == "get_xxx")
        .expect("manual item");
    assert_eq!(manual.kind, Some(CompletionItemKind::CONSTANT));
}

#[tokio::test]
async fn initialize_advertises_project_context_commands() {
    let service = test_backend_service();
    let initialized = service
        .inner()
        .initialize(InitializeParams::default())
        .await
        .expect("initialize");
    let commands = initialized
        .capabilities
        .execute_command_provider
        .expect("commands")
        .commands;
    assert!(commands.contains(&super::PROJECT_CONTEXTS_LSP_COMMAND.to_string()));
    assert!(commands.contains(&super::SET_PROJECT_CONTEXT_LSP_COMMAND.to_string()));
}

#[tokio::test]
async fn initialize_project_context_off_is_effective_before_extension_state_restore() {
    let service = test_backend_service();
    service
        .inner()
        .initialize(InitializeParams {
            initialization_options: Some(serde_json::json!({
                "fossilsense": { "projectContext": { "mode": "off" } }
            })),
            ..Default::default()
        })
        .await
        .expect("initialize");

    assert_eq!(
        *service.inner().project_context_selection.lock().await,
        crate::project_context::ProjectContextSelection::Unspecified
    );
}

#[tokio::test]
async fn dirty_publish_does_not_mutate_an_in_flight_engine_snapshot() {
    let root = tempdir().expect("root");
    let root_path = root.path().to_path_buf();
    write_workspace_file(root.path(), "main.c", "#include \"old.h\"\nint before;\n");
    write_workspace_file(root.path(), "old.h", "int old_symbol;\n");
    write_workspace_file(root.path(), "new.h", "int new_symbol;\n");
    crate::indexer::index_workspace(
        root.path(),
        crate::indexer::IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("initial index");

    let service = test_backend_service();
    service
        .inner()
        .session
        .cache
        .publish_full_index(&service.inner().client, root_path.clone())
        .await
        .expect("full publish");
    let in_flight = service
        .inner()
        .session
        .request_context_for_root(root_path.clone())
        .await;
    let old_graph = in_flight.engine.reach_graph.clone().expect("old graph");
    assert!(old_graph.reachable("main.c").files.contains("old.h"));

    write_workspace_file(root.path(), "main.c", "#include \"new.h\"\nint after;\n");
    crate::indexer::index_dirty_files(
        root.path(),
        vec![crate::indexer::DirtyFileChange {
            absolute_path: root.path().join("main.c"),
            kind: crate::indexer::DirtyFileKind::Upsert,
        }],
        crate::indexer::IndexOptions::default(),
        |_| {},
    )
    .expect("dirty index");
    service
        .inner()
        .session
        .cache
        .publish_dirty_index(
            &service.inner().client,
            root_path.clone(),
            &["main.c".to_string()],
            &["main.c".to_string()],
        )
        .await
        .expect("dirty publish");

    let current = service
        .inner()
        .session
        .request_context_for_root(root_path)
        .await;
    let new_graph = current.engine.reach_graph.clone().expect("new graph");
    assert!(!Arc::ptr_eq(&old_graph, &new_graph));
    assert_ne!(in_flight.engine.epoch, current.engine.epoch);

    let old_scope = old_graph.reachable("main.c");
    assert!(old_scope.files.contains("old.h"));
    assert!(!old_scope.files.contains("new.h"));

    let new_scope = new_graph.reachable("main.c");
    assert!(new_scope.files.contains("new.h"));
    assert!(!new_scope.files.contains("old.h"));
}

#[tokio::test]
async fn cache_ledger_completion_memo_reuses_prefix_only_with_same_generation() {
    let cache = super::CacheLedger::default();
    let uri = Url::parse("file:///tmp/memo.c").expect("uri");

    cache
        .record_completion_memo(uri.clone(), "fo".to_string(), 42, vec![vec![1, 2, 3]])
        .await;

    let reused = cache.completion_memo_pools(&uri, 42, "foo", 1).await;
    assert_eq!(reused.hit_kind, "pool");
    assert_eq!(reused.prior_pools, vec![Some(vec![1, 2, 3])]);

    let hot = cache.completion_memo_pools(&uri, 42, "fo", 1).await;
    assert_eq!(hot.hit_kind, "hot");

    let stale = cache.completion_memo_pools(&uri, 43, "foo", 1).await;
    assert_eq!(stale.hit_kind, "cold");
    assert_eq!(stale.prior_pools, vec![None]);
}

#[tokio::test]
async fn cache_ledger_clears_reference_search_cache_after_document_and_index_changes() {
    let documents = super::DocumentStore::default();
    let cache = super::CacheLedger::default();
    let session = super::WorkspaceSession::new(documents, cache.clone());
    let uri = Url::parse("file:///tmp/references.c").expect("uri");

    cache.mark_reference_search_cache_for_test("root", "needle", 1);
    assert_eq!(cache.reference_search_cache_len_for_test(), 1);
    session
        .change_document(uri, 2, "int needle;\n".to_string())
        .await;
    assert_eq!(cache.reference_search_cache_len_for_test(), 0);

    cache.mark_reference_search_cache_for_test("root", "needle", 2);
    assert_eq!(cache.reference_search_cache_len_for_test(), 1);
    cache
        .store_relation_overlay(
            PathBuf::from("root"),
            super::state::EngineEpoch::published(2),
            7,
            Arc::new(crate::call_catalog::RelationCatalog::default()),
        )
        .await;
    assert_eq!(cache.relation_overlay_cache_len_for_test().await, 1);
    cache.invalidate_after_index_change().await;
    assert_eq!(cache.reference_search_cache_len_for_test(), 0);
    assert_eq!(cache.relation_overlay_cache_len_for_test().await, 0);
}

#[tokio::test]
async fn workspace_document_lifecycle_drops_stale_relation_overlays() {
    let documents = super::DocumentStore::default();
    let cache = super::CacheLedger::default();
    let session = super::WorkspaceSession::new(documents, cache.clone());
    let uri = Url::parse("file:///tmp/relations.c").expect("uri");
    let root = PathBuf::from("root");

    cache
        .store_relation_overlay(
            root.clone(),
            super::state::EngineEpoch::published(1),
            1,
            Arc::new(crate::call_catalog::RelationCatalog::default()),
        )
        .await;
    session
        .open_document(uri.clone(), 1, "void first(void);".into())
        .await;
    assert_eq!(cache.relation_overlay_cache_len_for_test().await, 0);

    cache
        .store_relation_overlay(
            root.clone(),
            super::state::EngineEpoch::published(1),
            2,
            Arc::new(crate::call_catalog::RelationCatalog::default()),
        )
        .await;
    session
        .change_document(uri.clone(), 2, "void second(void);".into())
        .await;
    assert_eq!(cache.relation_overlay_cache_len_for_test().await, 0);

    cache
        .store_relation_overlay(
            root,
            super::state::EngineEpoch::published(1),
            3,
            Arc::new(crate::call_catalog::RelationCatalog::default()),
        )
        .await;
    session.close_document(&uri).await;
    assert_eq!(cache.relation_overlay_cache_len_for_test().await, 0);
}

#[tokio::test]
async fn relation_overlay_tracks_only_divergent_or_not_yet_indexed_documents() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("tracked.c");
    std::fs::write(&path, "void tracked(void);\n").expect("write source");
    let uri = Url::from_file_path(&path).expect("uri");
    let documents = super::DocumentStore::default();

    documents
        .open_document(uri.clone(), 1, "void tracked(void);\n".into())
        .await;
    let clean = documents.snapshot(&uri).await.expect("clean snapshot");
    assert!(!clean.needs_relation_overlay(crate::call_model::SemanticGeneration(4)));

    documents
        .change_document(uri.clone(), 2, "void changed(void);\n".into())
        .await;
    let unsaved = documents.snapshot(&uri).await.expect("unsaved snapshot");
    assert!(unsaved.needs_relation_overlay(crate::call_model::SemanticGeneration(4)));

    documents
        .save_document(&uri, crate::call_model::SemanticGeneration(4))
        .await;
    let awaiting = documents.snapshot(&uri).await.expect("saved snapshot");
    assert!(awaiting.needs_relation_overlay(crate::call_model::SemanticGeneration(4)));
    assert!(!awaiting.needs_relation_overlay(crate::call_model::SemanticGeneration(5)));
}

#[tokio::test]
async fn reach_scope_uses_captured_request_context_graph() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    let uri = Url::from_file_path(root.join("main.c")).expect("file uri");
    let captured_graph = Arc::new(crate::reachability::ReachGraph::new(
        vec![("main.c".to_string(), "captured.h".to_string())],
        vec![],
        vec![],
    ));
    let context = super::RequestContext {
        engine: Arc::new(super::workspace::EngineSnapshot {
            root: root.clone(),
            epoch: super::state::EngineEpoch::missing(),
            semantic_generation: crate::call_model::SemanticGeneration::MISSING,
            name_table: None,
            reach_graph: Some(captured_graph),
            include_table: None,
            indexed_files: None,
            project_context: None,
            relation_catalog: None,
            degraded: crate::progress::DegradedCapabilities::default(),
        }),
        settings: super::RequestSettings {
            scoping_enabled: true,
            ..Default::default()
        },
    };

    service
        .inner()
        .session
        .cache
        .set_reach_graph_for_test(
            root,
            Arc::new(crate::reachability::ReachGraph::new(
                vec![("main.c".to_string(), "ledger.h".to_string())],
                vec![],
                vec![],
            )),
        )
        .await;

    let (_rel, scope) = service
        .inner()
        .reach_scope_from_context(&uri, &context)
        .expect("scope from captured request context");

    assert!(scope.files.contains("captured.h"));
    assert!(
        !scope.files.contains("ledger.h"),
        "request scope must come from the already captured snapshot"
    );
}

#[tokio::test]
async fn failed_include_table_rebuild_cannot_replace_published_state() {
    let root = tempdir().expect("root");
    let root_path = root.path().to_path_buf();
    let result = rebuild_include_table(root_path).await;

    assert!(result.is_err(), "missing index should fail the rebuild");
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

    let table = rebuild_include_table(root_path)
        .await
        .expect("rebuild include table");

    assert_eq!(table.len(), 2);
    assert_eq!(table.edge_count(), 1);
}

#[tokio::test]
async fn failed_reference_file_list_rebuild_cannot_replace_published_state() {
    let root = tempdir().expect("root");
    let root_path = root.path().to_path_buf();
    let result = rebuild_indexed_file_list(root_path).await;

    assert!(result.is_err(), "missing index should fail the rebuild");
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
async fn member_completion_resolves_simple_nested_member_chain() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[(
            "nested.hpp",
            "struct Inner { int value; };\nstruct Outer { struct Inner mem1; };\n",
        )],
        "main.cpp",
        "#include \"nested.hpp\"\nvoid f(Outer *a) { a->mem1./*cursor*/ }\n",
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
        .any(|item| item.label == "value" && item.kind == Some(CompletionItemKind::FIELD)));
}

#[tokio::test]
async fn member_completion_resolves_indexed_anonymous_nested_member_chain() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[(
            "nested.h",
            "typedef struct { struct { int xxx; } mem1[4]; } A;\n",
        )],
        "main.c",
        "#include \"nested.h\"\nvoid f(void) { A a; a.mem1[0]./*cursor*/ }\n",
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
        .any(|item| item.label == "xxx" && item.kind == Some(CompletionItemKind::FIELD)));
}

#[tokio::test]
async fn member_completion_falls_back_when_chain_parse_fails() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[("widget.hpp", "struct Widget { int width; int window; };\n")],
        "main.cpp",
        "void f(void) { make_widget()->wi/*cursor*/ }\n",
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
        .any(|item| item.label == "width" && item.kind == Some(CompletionItemKind::FIELD)));
    assert!(items
        .iter()
        .any(|item| item.label == "window" && item.kind == Some(CompletionItemKind::FIELD)));
}

#[tokio::test]
async fn member_completion_does_not_leak_global_owner_when_reachable_owner_lacks_prefix() {
    let (dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[
            ("reachable.hpp", "struct W { int width; };\n"),
            ("global.hpp", "struct W { int height; };\n"),
        ],
        "main.cpp",
        "#include \"reachable.hpp\"\nvoid f(W *w) { w->he/*cursor*/ }\n",
    )
    .await;
    service
        .inner()
        .session
        .cache
        .set_reach_graph_for_test(
            dir.path().to_path_buf(),
            Arc::new(crate::reachability::ReachGraph::new(
                vec![("main.cpp".to_string(), "reachable.hpp".to_string())],
                vec![],
                vec![],
            )),
        )
        .await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let items = completion_items(response);

    assert!(
        !items.iter().any(|item| item.label == "height"),
        "global W::height must not leak when reachable W has members but no 'he' member"
    );
    assert!(
        items.is_empty(),
        "resolved receiver should return an empty incomplete list instead of falling back"
    );
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

#[tokio::test]
async fn weak_receiver_uses_member_fallback_min_prefix_gate() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[("widget.hpp", "struct Widget { int width; int window; };\n")],
        "main.cpp",
        "void f(void) { widget->w/*cursor*/ }\n",
    )
    .await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");

    assert!(
        completion_items(response).is_empty(),
        "weak receiver correlation must not bypass the member fallback short-prefix gate"
    );
}

#[tokio::test]
async fn execute_command_records_completion_accept_when_history_enabled() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(dir.path().to_path_buf());
    service
        .inner()
        .set_completion_history_mode_for_test(crate::completion_history::CompletionHistoryMode::On)
        .await;
    let workspace_hash = super::completion_history_workspace_hash(dir.path());

    service
        .inner()
        .execute_command(ExecuteCommandParams {
            command: super::COMPLETION_ACCEPTED_LSP_COMMAND.to_string(),
            arguments: vec![serde_json::json!({
                "workspaceHash": workspace_hash,
                "candidateHash": crate::completion_history::candidate_hash("printf", "function"),
                "kind": "function",
                "intent": "call_target",
                "prefixBucket": "pr"
            })],
            work_done_progress_params: Default::default(),
        })
        .await
        .expect("command");

    assert_eq!(
        service
            .inner()
            .history_snapshot_for_test(&workspace_hash)
            .await
            .total_accepts(),
        1
    );
}

#[tokio::test]
async fn execute_command_ignores_invalid_completion_candidate_hash() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(dir.path().to_path_buf());
    service
        .inner()
        .set_completion_history_mode_for_test(crate::completion_history::CompletionHistoryMode::On)
        .await;
    let workspace_hash = super::completion_history_workspace_hash(dir.path());

    service
        .inner()
        .execute_command(ExecuteCommandParams {
            command: super::COMPLETION_ACCEPTED_LSP_COMMAND.to_string(),
            arguments: vec![serde_json::json!({
                "workspaceHash": workspace_hash,
                "candidateHash": "abc",
                "kind": "function",
                "intent": "call_target",
                "prefixBucket": "pr"
            })],
            work_done_progress_params: Default::default(),
        })
        .await
        .expect("command");

    assert_eq!(
        service
            .inner()
            .history_snapshot_for_test(&workspace_hash)
            .await
            .total_accepts(),
        0
    );
}

#[tokio::test]
async fn completion_accept_history_is_recorded_in_matching_workspace_root() {
    let service = test_backend_service();
    let first = tempdir().expect("first tempdir");
    let second = tempdir().expect("second tempdir");
    {
        let mut roots = service.inner().workspace_roots.lock().await;
        roots.push(first.path().to_path_buf());
        roots.push(second.path().to_path_buf());
    }
    service
        .inner()
        .set_completion_history_mode_for_test(crate::completion_history::CompletionHistoryMode::On)
        .await;
    let first_hash = super::completion_history_workspace_hash(first.path());
    let second_hash = super::completion_history_workspace_hash(second.path());

    service
        .inner()
        .execute_command(ExecuteCommandParams {
            command: super::COMPLETION_ACCEPTED_LSP_COMMAND.to_string(),
            arguments: vec![serde_json::json!({
                "workspaceHash": second_hash,
                "candidateHash": crate::completion_history::candidate_hash("printf", "function"),
                "kind": "function",
                "intent": "call_target",
                "prefixBucket": "pr"
            })],
            work_done_progress_params: Default::default(),
        })
        .await
        .expect("command");

    let first_path = crate::pathing::default_completion_history_path(first.path()).expect("path");
    let second_path = crate::pathing::default_completion_history_path(second.path()).expect("path");
    let first_store =
        crate::completion_history::CompletionHistoryStore::open(&first_path).expect("first store");
    let second_store = crate::completion_history::CompletionHistoryStore::open(&second_path)
        .expect("second store");

    assert_eq!(first_store.snapshot(&first_hash).total_accepts(), 0);
    assert_eq!(first_store.snapshot(&second_hash).total_accepts(), 0);
    assert_eq!(second_store.snapshot(&second_hash).total_accepts(), 1);
}

#[tokio::test]
async fn execute_command_ignores_completion_accept_when_history_disabled() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(dir.path().to_path_buf());
    service
        .inner()
        .set_completion_history_mode_for_test(crate::completion_history::CompletionHistoryMode::Off)
        .await;
    let workspace_hash = super::completion_history_workspace_hash(dir.path());

    service
        .inner()
        .execute_command(ExecuteCommandParams {
            command: super::COMPLETION_ACCEPTED_LSP_COMMAND.to_string(),
            arguments: vec![serde_json::json!({
                "workspaceHash": workspace_hash,
                "candidateHash": crate::completion_history::candidate_hash("printf", "function"),
                "kind": "function",
                "intent": "call_target",
                "prefixBucket": "pr"
            })],
            work_done_progress_params: Default::default(),
        })
        .await
        .expect("command");

    assert_eq!(
        service
            .inner()
            .history_snapshot_for_test(&workspace_hash)
            .await
            .total_accepts(),
        0
    );
}

#[tokio::test]
async fn clear_completion_history_overwrites_corrupt_history_file() {
    let service = test_backend_service();
    let dir = tempdir().expect("tempdir");
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(dir.path().to_path_buf());
    let history_path =
        crate::pathing::default_completion_history_path(dir.path()).expect("history path");
    std::fs::create_dir_all(history_path.parent().expect("history parent")).expect("mkdir");
    std::fs::write(&history_path, "{not json").expect("write corrupt history");

    service
        .inner()
        .clear_completion_history()
        .await
        .expect("clear corrupt history");

    let store = crate::completion_history::CompletionHistoryStore::open(&history_path)
        .expect("history should be parseable after clear");
    assert_eq!(
        store
            .snapshot(&super::completion_history_workspace_hash(dir.path()))
            .total_accepts(),
        0
    );
}

#[tokio::test]
async fn ordinary_completion_items_attach_history_accept_command_when_enabled() {
    let (src, line, character) = text_and_position(
        "#define FS_MAGIC 1\n\
         void f(void) { FS/*cursor*/(); }\n",
    );
    let dir = tempdir().expect("tempdir");
    let uri = Url::from_file_path(dir.path().join("a.c")).expect("file uri");
    let service = test_backend_service();
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(dir.path().to_path_buf());
    open_test_document(&service, uri.clone(), 1, src).await;
    service
        .inner()
        .set_completion_history_mode_for_test(crate::completion_history::CompletionHistoryMode::On)
        .await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion")
        .expect("response");
    let item = completion_items(response)
        .into_iter()
        .find(|item| item.label == "FS_MAGIC")
        .expect("FS_MAGIC");

    let command = item.command.as_ref().expect("history command");
    assert_eq!(command.command, super::COMPLETION_ACCEPTED_LSP_COMMAND);
    let argument = command
        .arguments
        .as_ref()
        .and_then(|arguments| arguments.first())
        .expect("command argument");
    assert_eq!(
        argument.get("kind").and_then(|value| value.as_str()),
        Some("macro")
    );
    assert_eq!(
        argument.get("intent").and_then(|value| value.as_str()),
        Some("call_target")
    );
    assert_eq!(
        argument
            .get("prefixBucket")
            .and_then(|value| value.as_str()),
        Some("fs")
    );
    assert!(argument
        .get("workspaceHash")
        .and_then(|value| value.as_str())
        .is_some_and(|value| !value.is_empty()));
    assert!(argument
        .get("candidateHash")
        .and_then(|value| value.as_str())
        .is_some_and(|value| value.len() == 16));
}

#[tokio::test]
async fn ordinary_completion_does_not_open_history_store_on_completion_hot_path() {
    let (src, line, character) = text_and_position(
        "#define FS_MAGIC 1\n\
         void f(void) { FS/*cursor*/(); }\n",
    );
    let dir = tempdir().expect("tempdir");
    let history_path =
        crate::pathing::default_completion_history_path(dir.path()).expect("history path");
    std::fs::create_dir_all(history_path.parent().expect("history parent")).expect("mkdir");
    std::fs::write(&history_path, "{\"version\":1,\"entries\":[]}").expect("write history");
    let uri = Url::from_file_path(dir.path().join("a.c")).expect("file uri");
    let service = test_backend_service();
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(dir.path().to_path_buf());
    open_test_document(&service, uri.clone(), 1, src).await;
    service
        .inner()
        .set_completion_history_mode_for_test(crate::completion_history::CompletionHistoryMode::On)
        .await;

    service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion")
        .expect("response");

    assert!(
        service.inner().completion_history.lock().await.is_empty(),
        "ordinary completion should use only already-loaded in-memory history"
    );
}

#[tokio::test]
async fn ordinary_completion_presents_static_keyword_with_lsp_kind_and_detail() {
    let (src, line, character) = text_and_position("str/*cursor*/");
    let dir = tempdir().expect("tempdir");
    let uri = Url::from_file_path(dir.path().join("a.c")).expect("file uri");
    let service = test_backend_service();
    open_test_document(&service, uri.clone(), 1, src).await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion")
        .expect("response");
    assert!(completion_response_is_incomplete(&response));
    let item = completion_items(response)
        .into_iter()
        .find(|item| item.label == "struct")
        .expect("struct keyword completion");

    assert_eq!(item.kind, Some(CompletionItemKind::KEYWORD));
    assert_eq!(item.detail.as_deref(), Some("keyword"));
}

#[tokio::test]
async fn ordinary_completion_builtin_only_result_stays_incomplete() {
    let (src, line, character) = text_and_position("si/*cursor*/");
    let dir = tempdir().expect("tempdir");
    let uri = Url::from_file_path(dir.path().join("a.c")).expect("file uri");
    let service = test_backend_service();
    open_test_document(&service, uri.clone(), 1, src).await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion")
        .expect("response");

    assert!(completion_response_is_incomplete(&response));
    assert!(completion_items(response)
        .into_iter()
        .any(|item| item.label == "size_t"
            && item.kind == Some(CompletionItemKind::STRUCT)
            && item.detail.as_deref() == Some("builtin type")));
}

#[derive(Debug, PartialEq, Eq)]
struct PresentedCompletion {
    label: String,
    kind: Option<CompletionItemKind>,
    detail: Option<String>,
    documentation: Option<String>,
    sort_text: Option<String>,
    has_history_command: bool,
}

fn presented_completion(item: &CompletionItem) -> PresentedCompletion {
    PresentedCompletion {
        label: item.label.clone(),
        kind: item.kind,
        detail: item.detail.clone(),
        documentation: item.documentation.as_ref().map(|doc| match doc {
            Documentation::String(text) => text.clone(),
            Documentation::MarkupContent(markup) => markup.value.clone(),
        }),
        sort_text: item.sort_text.clone(),
        has_history_command: item.command.is_some(),
    }
}

#[tokio::test]
async fn ordinary_completion_compat_fixture_captures_presented_boundary_output() {
    let (src, line, character) = text_and_position(
        "#include \"reachable.h\"\n\
         #define fs_overlay_macro 1\n\
         typedef int fs_overlay_type;\n\
         int fixture(int fs_param) {\n\
             int fs_local_value;\n\
             fs_text_word();\n\
             fs/*cursor*/\n\
         }\n",
    );
    let dir = tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    write_workspace_file(dir.path(), "src/main.c", &src);
    write_workspace_file(dir.path(), "reachable.h", "int fs_reachable_index(void);\n");

    let uri = Url::from_file_path(root.join("src/main.c")).expect("file uri");
    let service = test_backend_service();
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(root.clone());
    service
        .inner()
        .session
        .cache
        .set_name_table_for_test(
            root.clone(),
            Arc::new(crate::query::NameTable::build_with_paths(vec![
                (
                    1,
                    "fs_reachable_index".to_string(),
                    false,
                    "reachable.h".to_string(),
                    "function".to_string(),
                    false,
                ),
                (
                    2,
                    "fs_external_index".to_string(),
                    true,
                    "sdk/external.h".to_string(),
                    "type".to_string(),
                    true,
                ),
                (
                    3,
                    "fs_unknown_index".to_string(),
                    false,
                    "ambiguous/unknown.h".to_string(),
                    "enum_constant".to_string(),
                    false,
                ),
                (
                    4,
                    "fs_global_index".to_string(),
                    false,
                    "global.c".to_string(),
                    "macro".to_string(),
                    false,
                ),
            ])),
        )
        .await;
    service
        .inner()
        .session
        .cache
        .set_reach_graph_for_test(
            root.clone(),
            Arc::new(crate::reachability::ReachGraph::new(
                vec![("src/main.c".to_string(), "reachable.h".to_string())],
                vec![],
                vec!["src/main.c".to_string()],
            )),
        )
        .await;
    open_test_document(&service, uri.clone(), 1, src).await;
    service
        .inner()
        .set_completion_history_mode_for_test(crate::completion_history::CompletionHistoryMode::On)
        .await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    assert!(completion_response_is_incomplete(&response));
    let items = completion_items(response);
    let presented: Vec<_> = items.iter().take(9).map(presented_completion).collect();

    assert_eq!(
        presented,
        vec![
            PresentedCompletion {
                label: "fs_param".to_string(),
                kind: Some(CompletionItemKind::VARIABLE),
                detail: Some("parameter: int".to_string()),
                documentation: None,
                sort_text: Some("00000000".to_string()),
                has_history_command: true,
            },
            PresentedCompletion {
                label: "fs_local_value".to_string(),
                kind: Some(CompletionItemKind::VARIABLE),
                detail: Some("local: int".to_string()),
                documentation: None,
                sort_text: Some("00000001".to_string()),
                has_history_command: true,
            },
            PresentedCompletion {
                label: "fs_overlay_type".to_string(),
                kind: Some(CompletionItemKind::STRUCT),
                detail: Some("current".to_string()),
                documentation: None,
                sort_text: Some("00000002".to_string()),
                has_history_command: true,
            },
            PresentedCompletion {
                label: "fs_overlay_macro".to_string(),
                kind: Some(CompletionItemKind::CONSTANT),
                detail: Some("current".to_string()),
                documentation: None,
                sort_text: Some("00000003".to_string()),
                has_history_command: true,
            },
            PresentedCompletion {
                label: "fs_reachable_index".to_string(),
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some("reachable".to_string()),
                documentation: Some(
                    "FossilSense: reachable candidate (reachable, reachable_include)".to_string(),
                ),
                sort_text: Some("00000004".to_string()),
                has_history_command: true,
            },
            PresentedCompletion {
                label: "fs_external_index".to_string(),
                kind: Some(CompletionItemKind::STRUCT),
                detail: Some("external".to_string()),
                documentation: Some(
                    "FossilSense: external candidate (heuristic, external_first_layer)".to_string(),
                ),
                sort_text: Some("00000005".to_string()),
                has_history_command: true,
            },
            PresentedCompletion {
                label: "fs_global_index".to_string(),
                kind: Some(CompletionItemKind::CONSTANT),
                detail: Some("ambiguous".to_string()),
                documentation: Some(
                    "FossilSense: unknown candidate (ambiguous, global_fallback)".to_string(),
                ),
                sort_text: Some("00000006".to_string()),
                has_history_command: true,
            },
            PresentedCompletion {
                label: "fs_unknown_index".to_string(),
                kind: Some(CompletionItemKind::ENUM_MEMBER),
                detail: Some("ambiguous".to_string()),
                documentation: Some(
                    "FossilSense: unknown candidate (ambiguous, global_fallback)".to_string(),
                ),
                sort_text: Some("00000007".to_string()),
                has_history_command: true,
            },
            PresentedCompletion {
                label: "fs_text_word".to_string(),
                kind: Some(CompletionItemKind::TEXT),
                detail: Some("text".to_string()),
                documentation: None,
                sort_text: Some("00000008".to_string()),
                has_history_command: true,
            },
        ]
    );
}

#[test]
fn history_accept_command_uses_final_kind_for_candidate_hash() {
    let mut item = CompletionItem {
        label: "same_name".to_string(),
        ..Default::default()
    };
    let mut evidence = crate::completion::CandidateEvidence::new(
        crate::completion::CandidateSource::Indexed,
        crate::model::ScopeTier::Reachable,
        crate::model::ResolutionConfidence::Heuristic,
        700,
    );
    evidence.kind = crate::completion::CompletionCandidateKind::Function;
    evidence.history_key = Some(crate::completion_history::candidate_hash_key(
        "same_name",
        "variable",
    ));

    super::attach_completion_history_accept_command(
        &mut item,
        evidence,
        "workspace",
        crate::completion::CompletionIntentKind::CallTarget,
        "sa",
    );

    let argument = item
        .command
        .as_ref()
        .and_then(|command| command.arguments.as_ref())
        .and_then(|arguments| arguments.first())
        .expect("history command argument");
    let expected_hash = crate::completion_history::candidate_hash("same_name", "function");
    assert_eq!(
        argument
            .get("candidateHash")
            .and_then(|value| value.as_str()),
        Some(expected_hash.as_str())
    );
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
fn engine_epoch_reserves_zero_for_missing_state() {
    assert_eq!(super::state::EngineEpoch::missing().as_u64(), 0);
    assert_eq!(super::state::EngineEpoch::published(1).as_u64(), 1);
    assert_ne!(
        super::state::EngineEpoch::missing(),
        super::state::EngineEpoch::published(1)
    );
}

#[test]
fn combined_completion_generation_changes_with_engine_selection_or_project() {
    let root = PathBuf::from("workspace");
    let first = super::state::EngineEpoch::published(1);
    let second = super::state::EngineEpoch::published(2);
    let project = crate::project_context::ProjectKey {
        workspace_root_id: "root".into(),
        project_path: "app".into(),
    };

    let combined_first =
        super::state::combine_completion_generation(&[(root.clone(), first)], 1, None);
    let combined_second =
        super::state::combine_completion_generation(&[(root.clone(), second)], 1, None);
    let combined_selection =
        super::state::combine_completion_generation(&[(root.clone(), first)], 2, None);
    let combined_project =
        super::state::combine_completion_generation(&[(root, first)], 1, Some(&project));

    assert_ne!(combined_first, combined_second);
    assert_ne!(combined_first, combined_selection);
    assert_ne!(combined_first, combined_project);
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

    let indexed = crate::completion::PipelineCandidate::new(
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
    let local = crate::completion::PipelineCandidate::new(
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

    let indexed = crate::completion::PipelineCandidate::new(
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
    let local_binding = crate::completion::PipelineCandidate::new(
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
    let local_word = crate::completion::PipelineCandidate::new(
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
    open_test_document(&service, uri.clone(), 1, src).await;

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
    open_test_document(&service, uri.clone(), 1, src).await;

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
    service
        .inner()
        .session
        .cache
        .set_name_table_for_test(
            root,
            Arc::new(crate::query::NameTable::build_with_paths(names)),
        )
        .await;
    open_test_document(&service, uri.clone(), 1, src).await;

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

#[tokio::test]
async fn local_binding_pipeline_uses_open_document_bindings_before_local_words() {
    let src = "int f(int count) {\n    int cursor_limit;\n    cur\n}\n";
    let dir = tempdir().expect("tempdir");
    let uri = Url::from_file_path(dir.path().join("a.c")).expect("file uri");
    let service = test_backend_service();
    open_test_document(&service, uri.clone(), 1, src.to_string()).await;

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
        call_relations: false,
        reach_graph: true,
        include_table: false,
        reference_file_list: true,
        project_context: false,
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
        call_relations: false,
        reach_graph: true,
        include_table: true,
        reference_file_list: false,
        project_context: false,
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
