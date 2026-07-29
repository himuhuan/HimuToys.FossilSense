use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::call_model::SemanticGeneration;
use crate::candidate_service::{CandidateOverlaySnapshot, FileCandidateOverlay};
use crate::{parser, pathing};

use super::workspace::DocumentRequestSnapshot;
use super::{uri_to_path, Backend};

pub(super) const MAX_EXTERNAL_OVERLAY_PARSED_IDENTITIES: usize = 8;

impl Backend {
    /// Capture every divergent open document in this workspace under one
    /// monotonic overlay epoch. The returned Arc is immutable and cached by
    /// `(root, semantic generation, overlay epoch)`.
    #[cfg(test)]
    pub(super) async fn candidate_overlay_snapshot(
        &self,
        root: &Path,
        generation: SemanticGeneration,
        base_reach_graph: Option<&crate::reachability::ReachGraph>,
        indexed_workspace_files: Option<&[(String, PathBuf)]>,
    ) -> Arc<CandidateOverlaySnapshot> {
        let documents = self.session.documents.capture_request_snapshot(None).await;
        let workspace_semantics = match self
            .session
            .cache
            .current_engine_snapshot(&root.to_path_buf())
            .await
            .filter(|snapshot| snapshot.semantic_generation == generation)
        {
            Some(snapshot) => snapshot.workspace_semantics.clone(),
            None => {
                let include_paths = self.include_paths.lock().await.clone();
                let go_module_paths = self.go_module_paths.lock().await.clone();
                let mut semantics =
                    super::workspace_config::PublishedWorkspaceSemantics::load_current(
                        root,
                        &include_paths,
                        &go_module_paths,
                    );
                semantics.external_roots = self.authorized_external_source_roots(root).await;
                Arc::new(semantics)
            }
        };
        self.candidate_overlay_snapshot_from_documents(
            root,
            generation,
            base_reach_graph,
            indexed_workspace_files,
            workspace_semantics,
            documents,
        )
        .await
    }

    /// Build an overlay from a caller-owned atomic document capture. This is
    /// used when a request also consumes current-buffer text, ensuring that the
    /// text and the all-open shadow/tombstone set come from the same lock view.
    pub(super) async fn candidate_overlay_snapshot_from_documents(
        &self,
        root: &Path,
        generation: SemanticGeneration,
        base_reach_graph: Option<&crate::reachability::ReachGraph>,
        indexed_workspace_files: Option<&[(String, PathBuf)]>,
        workspace_semantics: Arc<super::workspace_config::PublishedWorkspaceSemantics>,
        documents: DocumentRequestSnapshot,
    ) -> Arc<CandidateOverlaySnapshot> {
        let root = root.to_path_buf();
        let epoch = documents.overlay_epoch;
        let (cached, cache_revision) = self
            .session
            .cache
            .candidate_overlay(&root, generation, epoch)
            .await;
        if let Some(cached) = cached {
            return cached;
        }

        // Recover owned inputs only when they are the exact objects supplied
        // by the request's EngineSnapshot. If publication won the race, do not
        // substitute the newer graph/list into the older request; rebuilding
        // without that optional evidence is conservative and generation-safe.
        let published = self.session.cache.current_engine_snapshot(&root).await;
        let owned_reach_graph = published.as_ref().and_then(|snapshot| {
            (snapshot.semantic_generation == generation)
                .then(|| snapshot.reach_graph.clone())
                .flatten()
                .filter(|graph| {
                    base_reach_graph
                        .is_some_and(|requested| std::ptr::eq(graph.as_ref(), requested))
                })
        });
        let owned_indexed_files = published.as_ref().and_then(|snapshot| {
            (snapshot.semantic_generation == generation)
                .then(|| snapshot.indexed_files.clone())
                .flatten()
                .filter(|files| {
                    indexed_workspace_files.is_some_and(|requested| {
                        std::ptr::eq::<[(String, PathBuf)]>(files.as_slice(), requested)
                    })
                })
        });

        let external_roots = workspace_semantics.external_roots.clone();
        let language_resolver = workspace_semantics.language.clone();
        let root_for_paths = root.clone();
        let prepared = tokio::task::spawn_blocking(move || {
            let include_root_strings = external_roots.normalized_include_roots();
            let mut prepared = Vec::new();
            for (uri, snapshot) in documents.all {
                if !snapshot.needs_relation_overlay(generation) {
                    continue;
                }
                let Some(path) = uri_to_path(&uri) else {
                    continue;
                };
                let is_external = !pathing::path_is_within(&root_for_paths, &path);
                let overlay_targets = if !is_external {
                    let language = language_resolver.language_for_path(&path);
                    pathing::relative_slash_path(&root_for_paths, &path)
                        .ok()
                        .map(|path| vec![(path, Some(language))])
                } else {
                    external_roots
                        .mapped_path(&path)
                        .map(|authorized| {
                            authorized
                                .identities
                                .into_iter()
                                .filter_map(|identity| {
                                    let language = language_resolver
                                        .language_for_path(Path::new(&identity.identity_path));
                                    (!identity.go_only
                                        || language.semantic_family()
                                            == crate::semantic_model::SemanticFamily::Go)
                                        .then_some((identity.identity_path, Some(language)))
                                })
                                .collect::<Vec<_>>()
                        })
                        .filter(|targets| !targets.is_empty())
                };
                if let Some(overlay_targets) = overlay_targets {
                    prepared.push((uri, path, overlay_targets, is_external, snapshot));
                }
            }
            (include_root_strings, prepared)
        })
        .await
        .unwrap_or_default();

        let (include_roots, prepared_documents) = prepared;
        let mut parsed_documents = Vec::with_capacity(prepared_documents.len());
        for (uri, path, overlay_targets, is_external, snapshot) in prepared_documents {
            if !is_external {
                let Some((overlay_path, language)) = overlay_targets.into_iter().next() else {
                    continue;
                };
                let language = language.expect("workspace overlay language");
                let parsed = self
                    .get_or_parse_document_with_language(
                        &uri,
                        &path,
                        snapshot.version,
                        &snapshot.text,
                        parser::ParseFacts::HOVER_SEMANTICS,
                        language,
                    )
                    .await;
                parsed_documents.push((
                    overlay_path,
                    parsed,
                    snapshot.text,
                    language.semantic_family(),
                ));
                continue;
            }

            for (identity_index, (overlay_path, language)) in
                overlay_targets.into_iter().enumerate()
            {
                let language = language.expect("external overlay language");
                let parsed = if snapshot.text.len() as u64
                    > super::hover::HOVER_SOURCE_FILE_BYTE_LIMIT
                    || identity_index >= MAX_EXTERNAL_OVERLAY_PARSED_IDENTITIES
                {
                    None
                } else {
                    self.get_or_parse_external_overlay_document(
                        &uri,
                        &overlay_path,
                        snapshot.version,
                        &snapshot.text,
                        language,
                    )
                    .await
                };
                parsed_documents.push((
                    overlay_path,
                    parsed,
                    snapshot.text.clone(),
                    language.semantic_family(),
                ));
            }
        }

        let fallback_documents = parsed_documents
            .iter()
            .map(|(path, _, text, family)| (path.clone(), text.clone(), *family))
            .collect::<Vec<_>>();
        let built = tokio::task::spawn_blocking(move || {
            let files = parsed_documents
                .into_iter()
                .map(|(path, parsed, text, family)| match parsed {
                    Some(parsed) => FileCandidateOverlay::from_index_with_text(path, &parsed, text),
                    None => {
                        // A newer didChange may cancel this captured version's
                        // parse. Keep a tombstone so stale durable facts cannot
                        // leak through the dirty path.
                        FileCandidateOverlay::tombstone_for_family(path, text, family)
                    }
                })
                .collect();
            let mut overlay = CandidateOverlaySnapshot::new(epoch, files);
            overlay.refresh_reach_graph(
                owned_reach_graph.as_deref(),
                owned_indexed_files
                    .as_deref()
                    .into_iter()
                    .flatten()
                    .map(|(path, _)| path.as_str()),
                &include_roots,
            );
            Arc::new(overlay)
        })
        .await
        .unwrap_or_else(|_| {
            // Worker failure is rare, but the safe fallback must still retain
            // every dirty-path tombstone rather than expose durable stale rows.
            Arc::new(CandidateOverlaySnapshot::new(
                epoch,
                fallback_documents
                    .into_iter()
                    .map(|(path, text, family)| {
                        FileCandidateOverlay::tombstone_for_family(path, text, family)
                    })
                    .collect(),
            ))
        });

        self.session
            .cache
            .publish_candidate_overlay(root, generation, epoch, cache_revision, built)
            .await
    }
}
