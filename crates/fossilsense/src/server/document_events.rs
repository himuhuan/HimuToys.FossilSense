use super::*;

impl Backend {
    pub(super) async fn handle_did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        self.session
            .open_document(
                uri.clone(),
                params.text_document.version,
                params.text_document.text,
            )
            .await;
        self.client
            .log_message(MessageType::LOG, format!("opened {uri}"))
            .await;
    }

    pub(super) async fn handle_did_change(&self, params: DidChangeTextDocumentParams) {
        // FULL text sync: the final change carries the entire document.
        if let Some(change) = params.content_changes.into_iter().next_back() {
            self.session
                .change_document(
                    params.text_document.uri.clone(),
                    params.text_document.version,
                    change.text,
                )
                .await
        }
    }

    pub(super) async fn handle_did_close(&self, params: DidCloseTextDocumentParams) {
        self.session.close_document(&params.text_document.uri).await;
    }

    pub(super) async fn handle_did_change_watched_files(
        &self,
        params: DidChangeWatchedFilesParams,
    ) {
        let roots = self.workspace_roots.lock().await.clone();
        let mut dirty_changes = Vec::new();
        let mut project_context_roots = Vec::new();
        let mut needs_full = false;

        // Ensure the config cache is populated for each root (on first event)
        // and reused across events in this batch. The cache avoids re-reading
        // `fossilsense.json` on every file save. `fossilsense.json` changes
        // themselves trigger `WatchDecision::Full` which bypasses the cache.
        {
            let mut cache = self.config_cache.lock().await;
            for root in &roots {
                if !cache.contains_key(root) {
                    let (config, _) = WorkspaceConfig::load(root);
                    cache.insert(root.clone(), config);
                }
            }
        }

        for change in &params.changes {
            match watched_change_in_scope(&roots, change, &self.config_cache).await {
                Some(WatchDecision::Full) => needs_full = true,
                Some(WatchDecision::Dirty(dirty)) => dirty_changes.push(dirty),
                Some(WatchDecision::ProjectContext(root)) => project_context_roots.push(root),
                None => {}
            }
        }

        project_context_roots.sort();
        project_context_roots.dedup();

        let relevant_changes =
            dirty_changes.len() + project_context_roots.len() + usize::from(needs_full);
        let dirty_count = dirty_changes.len();
        if relevant_changes > 0 {
            self.session.cache.invalidate_references();
        }
        self.client
            .log_message(
                MessageType::LOG,
                format!(
                    "received {} watched file changes ({} in FossilSense scope, {} dirty files, {} project-context roots)",
                    params.changes.len(),
                    relevant_changes,
                    dirty_count,
                    project_context_roots.len()
                ),
            )
            .await;

        if needs_full {
            self.spawn_index_roots(None).await;
        } else if !project_context_roots.is_empty() {
            self.refresh_project_context_roots(project_context_roots)
                .await;
            if !dirty_changes.is_empty() {
                self.spawn_dirty_files(dirty_changes).await;
            }
        } else if !dirty_changes.is_empty() {
            self.spawn_dirty_files(dirty_changes).await;
        }
    }

    pub(super) async fn handle_did_save(&self, params: DidSaveTextDocumentParams) {
        self.session.cache.invalidate_references();
        self.client
            .log_message(
                MessageType::LOG,
                format!(
                    "saved {} (waiting for file watcher before reindex)",
                    params.text_document.uri
                ),
            )
            .await;
    }
}
