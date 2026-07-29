use super::*;

impl Backend {
    pub(in crate::server) async fn goto_include(
        &self,
        uri: &Url,
        form: IncludeForm,
        rel: String,
    ) -> LspResult<Option<GotoDefinitionResponse>> {
        let current_dir = uri_to_path(uri).and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let workspace_root = self.root_for_uri(uri).await;
        let request_context = match &workspace_root {
            Some(root) => Some(self.request_context_for_root(root.clone()).await),
            None => None,
        };
        let semantic_generation = request_context
            .as_ref()
            .map(|context| context.engine.semantic_generation.0);
        let include_roots = match request_context.as_ref() {
            Some(context) => context
                .engine
                .workspace_semantics
                .external_roots
                .normalized_include_roots(),
            None => {
                let client_include_roots = self.include_paths.lock().await.clone();
                configured_include_paths(&[], &client_include_roots)
            }
        };
        let db_path = workspace_root
            .as_ref()
            .and_then(|root| pathing::default_index_path(root).ok());

        let result = tokio::task::spawn_blocking(move || -> Result<Vec<Location>> {
            let resolved = resolve_include_paths(
                form,
                &rel,
                current_dir.as_deref(),
                workspace_root.as_deref(),
                &include_roots,
                db_path.as_deref(),
                semantic_generation,
            )?;
            Ok(resolved
                .iter()
                .filter_map(|path| location_at_file_start(path))
                .collect())
        })
        .await;

        match self.unwrap_query("include definition", result).await {
            Some(locations) if !locations.is_empty() => {
                Ok(Some(GotoDefinitionResponse::Array(locations)))
            }
            _ => Ok(None),
        }
    }

    pub(in crate::server) async fn complete_include(
        &self,
        uri: &Url,
        form: IncludeForm,
        partial: String,
        text: &str,
    ) -> LspResult<Option<CompletionResponse>> {
        let (dir_part, seg) = includes::split_partial(&partial);
        let current_dir = uri_to_path(uri).and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let workspace_root = self.root_for_uri(uri).await;
        let current_rel_path = workspace_root.as_ref().and_then(|root| {
            uri_to_path(uri).and_then(|path| pathing::relative_slash_path(root, &path).ok())
        });
        let current_rel_dir = current_rel_path
            .as_deref()
            .and_then(|path| path.rsplit_once('/').map(|(dir, _)| dir.to_string()));
        let request_context = match &workspace_root {
            Some(root) => Some(self.request_context_for_root(root.clone()).await),
            None => None,
        };
        let include_table = request_context
            .as_ref()
            .and_then(|context| context.engine.include_table.clone());
        let semantic_generation = request_context
            .as_ref()
            .map(|context| context.engine.semantic_generation.0);
        let include_roots = match request_context.as_ref() {
            Some(context) => context
                .engine
                .workspace_semantics
                .external_roots
                .normalized_include_roots(),
            None => {
                let client_include_roots = self.include_paths.lock().await.clone();
                configured_include_paths(&[], &client_include_roots)
            }
        };
        let db_path = workspace_root
            .as_ref()
            .and_then(|root| pathing::default_index_path(root).ok());
        let external_cache = self.external_include_dir_cache.clone();
        let text = text.to_string();

        let started = tokio::time::Instant::now();
        let hit_memory = include_table.is_some();
        let hit_db = db_path.as_ref().is_some_and(|path| path.exists());
        let result = tokio::task::spawn_blocking(
            move || -> Result<(Vec<CompletionItem>, include_completion::IncludeCompletionMetrics)> {
                let evidence =
                    CurrentIncludeEvidence::from_text(&text, current_rel_path.as_deref());
                Ok(collect_include_candidates_with_table_and_evidence(
                    form,
                    &dir_part,
                    &seg,
                    current_dir.as_deref(),
                    workspace_root.as_deref(),
                    &include_roots,
                    db_path.as_deref(),
                    semantic_generation,
                    include_table.as_deref(),
                    Some(&external_cache),
                    current_rel_dir.as_deref(),
                    Some(&evidence),
                    query::COMPLETION_LIMIT,
                ))
            },
        )
        .await;
        let total_ms = started.elapsed().as_millis();
        let metrics = result
            .as_ref()
            .ok()
            .and_then(|inner| inner.as_ref().ok().map(|(_, metrics)| *metrics))
            .unwrap_or_default();
        self.perf_log(|| {
            format!(
                "[perf] include_completion total={}ms workspace_table={} workspace_index={} same_directory={} recent={} sibling={} basename={} depth_penalty={}",
                total_ms,
                if hit_memory { "memory" } else { "unavailable" },
                if hit_db { "available" } else { "unavailable" },
                metrics.same_directory,
                metrics.recent,
                metrics.sibling,
                metrics.basename,
                metrics.depth_penalty,
            )
        })
        .await;

        match self.unwrap_query("include completion", result).await {
            Some((items, _)) if !items.is_empty() => Ok(Some(CompletionResponse::Array(items))),
            _ => Ok(Some(empty_completion_list(true))),
        }
    }
}
