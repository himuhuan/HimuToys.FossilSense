use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tower_lsp::lsp_types::{CompletionItem, CompletionItemKind, Documentation, Url};

use crate::call_model::SemanticGeneration;
use crate::completion::ordinary_service::{
    OrdinaryCompletionDocumentationTarget, OrdinaryCompletionItem, OrdinaryCompletionKind,
};

pub(in crate::server) fn apply_final_completion_sort_text(items: &mut [CompletionItem]) {
    for (index, item) in items.iter_mut().enumerate() {
        item.sort_text = Some(format!("{index:08}"));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(in crate::server) enum CompletionDocumentationData {
    Declaration {
        version: u8,
        root: String,
        uri: String,
        declaration_id: i64,
        declaration_name: String,
        semantic_generation: u64,
        overlay_epoch: u64,
        document_version: i32,
    },
    Candidate {
        version: u8,
        root: String,
        uri: String,
        handle: crate::candidate_service::CandidateHandle,
        semantic_generation: u64,
        overlay_epoch: u64,
        document_version: i32,
    },
    Member {
        version: u8,
        root: String,
        uri: String,
        owner_path: String,
        handle: crate::model::MemberCandidateHandle,
        semantic_generation: u64,
        owner_revision_hash: String,
        overlay_epoch: u64,
        document_version: i32,
    },
}

pub(in crate::server) fn ordinary_completion_item_to_lsp(
    item: OrdinaryCompletionItem,
    uri: &Url,
    table_roots: &[PathBuf],
    table_semantic_generations: &[SemanticGeneration],
    overlay_epoch: u64,
    document_version: i32,
) -> CompletionItem {
    let data = item.documentation_target.and_then(|target| {
        let target = match target {
            OrdinaryCompletionDocumentationTarget::Candidate {
                table_index,
                handle,
            } => CompletionDocumentationData::Candidate {
                version: 4,
                root: table_roots.get(table_index)?.to_string_lossy().into_owned(),
                uri: uri.to_string(),
                handle,
                semantic_generation: table_semantic_generations.get(table_index)?.0,
                overlay_epoch,
                document_version,
            },
            OrdinaryCompletionDocumentationTarget::Declaration {
                table_index,
                declaration_id,
                declaration_name,
            } => CompletionDocumentationData::Declaration {
                version: 6,
                root: table_roots.get(table_index)?.to_string_lossy().into_owned(),
                uri: uri.to_string(),
                declaration_id,
                declaration_name,
                semantic_generation: table_semantic_generations.get(table_index)?.0,
                overlay_epoch,
                document_version,
            },
            OrdinaryCompletionDocumentationTarget::CurrentDocument { start_line: _ } => {
                return None;
            }
        };
        serde_json::to_value(target).ok()
    });
    CompletionItem {
        label: item.label,
        kind: Some(ordinary_completion_kind_to_lsp(item.kind)),
        detail: item.detail,
        documentation: item.documentation.map(Documentation::String),
        sort_text: item.initial_sort_text,
        data,
        ..Default::default()
    }
}

fn ordinary_completion_kind_to_lsp(kind: OrdinaryCompletionKind) -> CompletionItemKind {
    match kind {
        OrdinaryCompletionKind::Text => CompletionItemKind::TEXT,
        OrdinaryCompletionKind::Keyword => CompletionItemKind::KEYWORD,
        OrdinaryCompletionKind::Function => CompletionItemKind::FUNCTION,
        OrdinaryCompletionKind::Macro => CompletionItemKind::CONSTANT,
        OrdinaryCompletionKind::Type => CompletionItemKind::STRUCT,
        OrdinaryCompletionKind::Variable => CompletionItemKind::VARIABLE,
        OrdinaryCompletionKind::EnumConstant => CompletionItemKind::ENUM_MEMBER,
    }
}
