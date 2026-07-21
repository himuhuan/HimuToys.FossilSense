use super::*;

impl Backend {
    pub(in crate::server) async fn workspace_root_config(
        &self,
        root: &Path,
    ) -> WorkspaceRootConfig {
        if let Some(cached) = self.config_cache.lock().await.get(root).cloned() {
            return cached;
        }

        let root = root.to_path_buf();
        let load_root = root.clone();
        let loaded = tokio::task::spawn_blocking(move || WorkspaceRootConfig::load(&load_root))
            .await
            .unwrap_or_else(|_| WorkspaceRootConfig::fallback(&root));
        self.config_cache
            .lock()
            .await
            .entry(root)
            .or_insert_with(|| loaded.clone())
            .clone()
    }

    pub(in crate::server) async fn source_language_for_path(&self, path: &Path) -> SourceLanguage {
        let roots = self.workspace_roots.lock().await.clone();
        let containing = roots
            .iter()
            .filter(|root| pathing::path_is_within(root, path))
            .max_by_key(|root| root.components().count())
            .cloned();
        if let Some(root) = containing {
            return self
                .workspace_root_config(&root)
                .await
                .language
                .language_for_path(path);
        }

        for root in roots {
            if let Some(language) = self
                .workspace_root_config(&root)
                .await
                .language
                .overridden_language_for_path(path)
            {
                return language;
            }
        }
        SourceLanguage::default_for_path(path)
    }

    pub(in crate::server) async fn source_language_for_uri(&self, uri: &Url) -> SourceLanguage {
        match uri_to_path(uri) {
            Some(path) => self.source_language_for_path(&path).await,
            None => SourceLanguage::default_for_path(Path::new(uri.path())),
        }
    }

    pub(in crate::server) async fn include_roots_for_workspace(
        &self,
        workspace_root: Option<&Path>,
        client_paths: &[String],
    ) -> Vec<String> {
        let workspace_paths = match workspace_root {
            Some(root) => self
                .workspace_root_config(root)
                .await
                .workspace
                .include_paths
                .clone(),
            None => Vec::new(),
        };
        configured_include_paths(&workspace_paths, client_paths)
    }
}
