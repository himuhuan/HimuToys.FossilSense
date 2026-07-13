use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::call_model::SemanticGeneration;
use crate::candidate_service::{CandidateOverlaySnapshot, FileCandidateOverlay};
use crate::{parser, pathing};

use super::workspace::DocumentRequestSnapshot;
use super::{uri_to_path, Backend};

impl Backend {
    /// Capture every divergent open document in this workspace under one
    /// monotonic overlay epoch. The returned Arc is immutable and cached by
    /// `(root, semantic generation, overlay epoch)`.
    pub(super) async fn candidate_overlay_snapshot(
        &self,
        root: &Path,
        generation: SemanticGeneration,
        base_reach_graph: Option<&crate::reachability::ReachGraph>,
        indexed_workspace_files: Option<&[(String, PathBuf)]>,
    ) -> Arc<CandidateOverlaySnapshot> {
        let documents = self.session.documents.capture_request_snapshot(None).await;
        self.candidate_overlay_snapshot_from_documents(
            root,
            generation,
            base_reach_graph,
            indexed_workspace_files,
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

        let client_roots = self.include_paths.lock().await.clone();
        let root_for_paths = root.clone();
        let prepared = tokio::task::spawn_blocking(move || {
            let configured = super::configured_include_paths(Some(&root_for_paths), &client_roots);
            let (include_roots, _issues) = crate::config::resolve_include_roots(&configured);
            let include_root_strings: Vec<String> = include_roots
                .iter()
                .map(|path| pathing::normalize_abs_path(path))
                .collect();
            let mut prepared = Vec::new();
            for (uri, snapshot) in documents.all {
                if !snapshot.needs_relation_overlay(generation) {
                    continue;
                }
                let Some(path) = uri_to_path(&uri) else {
                    continue;
                };
                let overlay_path = if pathing::path_is_within(&root_for_paths, &path) {
                    pathing::relative_slash_path(&root_for_paths, &path).ok()
                } else {
                    include_roots.iter().find_map(|include_root| {
                        if !pathing::path_is_within(include_root, &path) {
                            return None;
                        }
                        let relative = pathing::relative_slash_path(include_root, &path).ok()?;
                        let base = pathing::normalize_abs_path(include_root);
                        Some(if relative.is_empty() {
                            base
                        } else {
                            format!("{}/{}", base.trim_end_matches('/'), relative)
                        })
                    })
                };
                if let Some(overlay_path) = overlay_path {
                    prepared.push((uri, path, overlay_path, snapshot));
                }
            }
            (include_root_strings, prepared)
        })
        .await
        .unwrap_or_default();

        let (include_roots, prepared_documents) = prepared;
        let mut parsed_documents = Vec::with_capacity(prepared_documents.len());
        for (uri, path, overlay_path, snapshot) in prepared_documents {
            let parsed = self
                .get_or_parse_document(
                    &uri,
                    &path,
                    snapshot.version,
                    &snapshot.text,
                    parser::ParseFacts::HOVER_SEMANTICS,
                )
                .await;
            parsed_documents.push((overlay_path, parsed, snapshot.text));
        }

        let fallback_documents = parsed_documents
            .iter()
            .map(|(path, _, text)| (path.clone(), text.clone()))
            .collect::<Vec<_>>();
        let built = tokio::task::spawn_blocking(move || {
            let files = parsed_documents
                .into_iter()
                .map(|(path, parsed, text)| match parsed {
                    Some(parsed) => FileCandidateOverlay::from_index_with_text(path, &parsed, text),
                    None => {
                        // A newer didChange may cancel this captured version's
                        // parse. Keep a tombstone so stale durable facts cannot
                        // leak through the dirty path.
                        FileCandidateOverlay::tombstone(path, text)
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
                    .map(|(path, text)| FileCandidateOverlay::tombstone(path, text))
                    .collect(),
            ))
        });

        self.session
            .cache
            .publish_candidate_overlay(root, generation, epoch, cache_revision, built)
            .await
    }
}
