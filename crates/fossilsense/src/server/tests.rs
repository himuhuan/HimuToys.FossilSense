#![allow(clippy::field_reassign_with_default)]

use super::include_completion::IncludeCompletionTable;
use super::{
    apply_final_completion_sort_text, attach_completion_history_accept_command,
    completion_history_workspace_hash, grouped_reference_items, local_words_for_cache,
    query_error_log_line, ready_cache_message, rebuild_include_table, rebuild_indexed_file_list,
    state, CacheLedger, DocumentStore, IncludeTables, IndexScheduleState, IndexedFileLists,
    RootDirtyChange, WorkspaceSession, WorkspaceSnapshot, WorkspaceSnapshotSettings,
    COMPLETION_ACCEPTED_LSP_COMMAND, PROJECT_CONTEXTS_LSP_COMMAND, SET_PROJECT_CONTEXT_LSP_COMMAND,
};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};
use tempfile::tempdir;
use tower_lsp::lsp_types::{
    CompletionItem, CompletionItemKind, CompletionParams, CompletionResponse, Documentation,
    ExecuteCommandParams, GotoDefinitionParams, GotoDefinitionResponse, HoverContents, HoverParams,
    Position, SignatureHelpParams, TextDocumentIdentifier, TextDocumentPositionParams, Url,
};
use tower_lsp::LspService;

mod completion_history;
mod grouped_references;
mod indexing_state;
mod member_completion;
mod navigation_definition;
mod ordinary_completion;
mod project_context;
mod session_cache;

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
        debug_candidate_reasons: AtomicBool::new(false),
        perf_logging_enabled: AtomicBool::new(false),
        config_cache: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        project_context_selection: Arc::new(tokio::sync::Mutex::new(
            crate::project_context::ProjectContextSelection::Auto,
        )),
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

fn hover_params(uri: Url, line: u32, character: u32) -> HoverParams {
    HoverParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position::new(line, character),
        },
        work_done_progress_params: Default::default(),
    }
}

fn signature_help_params(uri: Url, line: u32, character: u32) -> SignatureHelpParams {
    SignatureHelpParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position::new(line, character),
        },
        work_done_progress_params: Default::default(),
        context: None,
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

fn position_after(text: &str, needle: &str) -> (u32, u32) {
    let byte = text.rfind(needle).expect("needle") + needle.len();
    let before = &text[..byte];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let character = before[line_start..]
        .chars()
        .map(|ch| ch.len_utf16() as u32)
        .sum();
    (line, character)
}

fn marked_string_text(value: tower_lsp::lsp_types::MarkedString) -> String {
    match value {
        tower_lsp::lsp_types::MarkedString::String(value) => value,
        tower_lsp::lsp_types::MarkedString::LanguageString(value) => value.value,
    }
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
