use super::*;

impl Backend {
    pub(super) async fn handle_watched_file_changes(&self, params: DidChangeWatchedFilesParams) {
        let roots = self.workspace_roots.lock().await.clone();
        let mut dirty_changes = Vec::new();
        let mut project_context_roots = Vec::new();
        let mut config_roots = Vec::new();
        let mut needs_full = false;

        // Populate each root once, then reuse the same snapshots throughout
        // this event batch. A configuration change chooses a full rebuild and
        // invalidates the cache through the normal watcher path.
        for root in &roots {
            self.workspace_root_config(root).await;
        }

        for change in &params.changes {
            match watched_change_in_scope(&roots, change, &self.config_cache).await {
                Some(WatchDecision::Full(root)) => {
                    needs_full = true;
                    config_roots.push(root);
                }
                Some(WatchDecision::ProjectContext(root)) => project_context_roots.push(root),
                Some(WatchDecision::Dirty(dirty)) => dirty_changes.push(dirty),
                None => {}
            }
        }

        let relevant_changes =
            dirty_changes.len() + project_context_roots.len() + usize::from(needs_full);
        let dirty_count = dirty_changes.len();
        if relevant_changes > 0 {
            self.session.cache.invalidate_references();
        }
        if !config_roots.is_empty() {
            config_roots.sort();
            config_roots.dedup();
            self.session
                .documents
                .invalidate_language_config_roots(&config_roots)
                .await;
            self.session
                .cache
                .invalidate_candidate_overlay_roots(&config_roots)
                .await;
        }
        self.client
            .log_message(
                MessageType::LOG,
                format!(
                    "received {} watched file changes ({} in FossilSense scope, {} dirty files)",
                    params.changes.len(),
                    relevant_changes,
                    dirty_count
                ),
            )
            .await;

        if needs_full {
            self.spawn_index_roots(None).await;
            return;
        }
        if !dirty_changes.is_empty() {
            self.spawn_dirty_files(dirty_changes).await;
        }
        if !project_context_roots.is_empty() {
            self.refresh_project_context_roots(project_context_roots)
                .await;
        }
    }
}
