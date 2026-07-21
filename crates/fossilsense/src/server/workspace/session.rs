use std::path::PathBuf;

use tower_lsp::lsp_types::{TextDocumentContentChangeEvent, Url};

use crate::call_model::SemanticGeneration;

use super::{CacheLedger, DocumentStore, RequestContext, RequestSettings};

#[derive(Clone)]
pub(in crate::server) struct WorkspaceSession {
    pub(in crate::server) documents: DocumentStore,
    pub(in crate::server) cache: CacheLedger,
}

impl WorkspaceSession {
    pub(in crate::server) fn new(documents: DocumentStore, cache: CacheLedger) -> Self {
        Self { documents, cache }
    }

    pub(in crate::server) async fn open_document(&self, uri: Url, version: i32, text: String) {
        self.documents.open_document(uri, version, text).await;
    }

    #[cfg(test)]
    pub(in crate::server) async fn change_document(&self, uri: Url, version: i32, text: String) {
        self.documents
            .change_document(uri.clone(), version, text)
            .await;
        self.cache.invalidate_references();
    }

    pub(in crate::server) async fn apply_document_changes(
        &self,
        uri: &Url,
        version: i32,
        changes: Vec<TextDocumentContentChangeEvent>,
    ) -> bool {
        let applied = self
            .documents
            .apply_document_changes(uri, version, changes)
            .await;
        if applied {
            self.cache.invalidate_references();
        }
        applied
    }

    pub(in crate::server) async fn close_document(&self, uri: &Url) {
        self.documents.close_document(uri).await;
        self.cache.clear_completion_memo(uri).await;
    }

    pub(in crate::server) async fn save_document(&self, uri: &Url, generation: SemanticGeneration) {
        self.documents.save_document(uri, generation).await;
        self.cache.invalidate_references();
    }

    #[cfg(test)]
    pub(in crate::server) async fn request_context_for_root(
        &self,
        root: PathBuf,
    ) -> RequestContext {
        self.cache
            .request_context(root, RequestSettings::default())
            .await
    }

    pub(in crate::server) async fn request_context_for_root_with_settings(
        &self,
        root: PathBuf,
        settings: RequestSettings,
    ) -> RequestContext {
        self.cache.request_context(root, settings).await
    }
}
