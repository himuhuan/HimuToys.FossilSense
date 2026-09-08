use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::call_model::SemanticGeneration;
use crate::candidate_service::{
    completion_recall_universe_id, CandidateOverlaySnapshot, FileCandidateOverlay, RecallUniverseId,
};
use crate::{parser, pathing};

use super::workspace::DocumentRequestSnapshot;
use super::{uri_to_path, Backend};

pub(super) const MAX_EXTERNAL_OVERLAY_PARSED_IDENTITIES: usize = 8;

pub(super) struct CompletionOverlayRequest<'a> {
    pub(super) root: &'a Path,
    pub(super) current_uri: &'a tower_lsp::lsp_types::Url,
    pub(super) engine_epoch: super::state::EngineEpoch,
    pub(super) generation: SemanticGeneration,
    pub(super) base_reach_graph: Option<&'a crate::reachability::ReachGraph>,
    pub(super) indexed_workspace_files: Option<&'a crate::indexed_files::IndexedFileList>,
    pub(super) workspace_semantics: Arc<super::workspace_config::PublishedWorkspaceSemantics>,
}

impl Backend {
    /// Build the completion-only projection of divergent documents. Unlike the
    /// complete semantic overlay below, this cache is keyed by stable recall
    /// facts rather than the document epoch. Ordinary body typing can therefore
    /// reuse the expensive reach/name projection while the request still keeps
    /// its exact epoch for cancellation and completion-resolve freshness.
    pub(super) async fn completion_overlay_snapshot_from_documents(
        &self,
        request: CompletionOverlayRequest<'_>,
        documents: DocumentRequestSnapshot,
    ) -> (Arc<CandidateOverlaySnapshot>, RecallUniverseId) {
        let CompletionOverlayRequest {
            root,
            current_uri,
            engine_epoch,
            generation,
            base_reach_graph,
            indexed_workspace_files,
            workspace_semantics,
        } = request;
        let root = root.to_path_buf();
        let epoch = documents.overlay_epoch;
        let current_uri = current_uri.clone();
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
                let is_current = uri == current_uri;
                let is_external = !pathing::path_is_within(&root_for_paths, &path);
                let overlay_targets = if !is_external {
                    let language = language_resolver.selection_for_source(&path, &snapshot.text);
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
                                    let language = language_resolver.selection_for_source(
                                        Path::new(&identity.identity_path),
                                        &snapshot.text,
                                    );
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
                    prepared.push((
                        uri,
                        path,
                        overlay_targets,
                        is_external,
                        is_current,
                        snapshot,
                    ));
                }
            }
            (include_root_strings, prepared)
        })
        .await
        .unwrap_or_default();

        let (include_roots, prepared_documents) = prepared;
        let mut parsed_documents = Vec::with_capacity(prepared_documents.len());
        for (uri, path, overlay_targets, is_external, is_current, snapshot) in prepared_documents {
            if !is_external {
                let Some((overlay_path, language)) = overlay_targets.into_iter().next() else {
                    continue;
                };
                let language = language.expect("workspace overlay language");
                let identity_path = if language.language == crate::config::SourceLanguage::Go {
                    PathBuf::from(&overlay_path)
                } else {
                    path
                };
                let parsed = self
                    .get_or_parse_captured_document_with_selection(
                        &uri,
                        &identity_path,
                        snapshot.version,
                        &snapshot.text,
                        parser::ParseFacts::COMPLETION,
                        language,
                    )
                    .await;
                parsed_documents.push((
                    overlay_path,
                    parsed,
                    language.semantic_family(),
                    is_current,
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
                    language.semantic_family(),
                    is_current,
                ));
            }
        }

        let fallback_documents = parsed_documents
            .iter()
            .map(|(path, _, family, _)| (path.clone(), *family))
            .collect::<Vec<_>>();
        let (files, universe) = tokio::task::spawn_blocking(move || {
            let files: Vec<_> = parsed_documents
                .into_iter()
                .map(|(path, parsed, family, is_current)| match parsed {
                    Some(parsed) => {
                        FileCandidateOverlay::from_completion_index(path, &parsed, !is_current)
                    }
                    None => FileCandidateOverlay::completion_tombstone_for_family(path, family),
                })
                .collect();
            let universe = completion_recall_universe_id(&files);
            (files, universe)
        })
        .await
        .unwrap_or_else(|_| {
            let files: Vec<_> = fallback_documents
                .iter()
                .map(|(path, family)| {
                    FileCandidateOverlay::completion_tombstone_for_family(path.clone(), *family)
                })
                .collect();
            let universe = completion_recall_universe_id(&files);
            (files, universe)
        });

        let (cached, cache_revision) = self
            .session
            .cache
            .completion_overlay(&root, engine_epoch, generation, epoch, universe)
            .await;
        if let Some(cached) = cached {
            return (cached, universe);
        }

        // Retain exact Arc ownership only for the request's captured engine.
        // A concurrent publication invalidates this cache and forces the old
        // request to build conservatively without borrowing the newer graph.
        let published = self.session.cache.current_engine_snapshot(&root).await;
        let owned_reach_graph = published.as_ref().and_then(|snapshot| {
            (snapshot.epoch == engine_epoch && snapshot.semantic_generation == generation)
                .then(|| snapshot.reach_graph.clone())
                .flatten()
                .filter(|graph| {
                    base_reach_graph
                        .is_some_and(|requested| std::ptr::eq(graph.as_ref(), requested))
                })
        });
        let owned_indexed_files = published.as_ref().and_then(|snapshot| {
            (snapshot.epoch == engine_epoch && snapshot.semantic_generation == generation)
                .then(|| snapshot.indexed_files.clone())
                .flatten()
                .filter(|indexed| {
                    indexed_workspace_files
                        .is_some_and(|requested| std::ptr::eq(indexed.as_ref(), requested))
                })
        });
        let owned_include_path_index = published.as_ref().and_then(|snapshot| {
            (snapshot.epoch == engine_epoch
                && snapshot.semantic_generation == generation
                && owned_indexed_files.is_some())
            .then(|| snapshot.include_path_index.clone())
            .flatten()
        });

        let built = tokio::task::spawn_blocking(move || {
            let mut overlay = CandidateOverlaySnapshot::new(epoch, files);
            overlay.refresh_reach_graph(
                owned_reach_graph,
                owned_include_path_index,
                &include_roots,
            );
            Arc::new(overlay)
        })
        .await
        .unwrap_or_else(|_| {
            Arc::new(CandidateOverlaySnapshot::new(
                epoch,
                fallback_documents
                    .into_iter()
                    .map(|(path, family)| {
                        FileCandidateOverlay::completion_tombstone_for_family(path, family)
                    })
                    .collect(),
            ))
        });
        let published = self
            .session
            .cache
            .publish_completion_overlay(super::workspace::CompletionOverlayPublication {
                root,
                engine_epoch,
                semantic_generation: generation,
                overlay_epoch: epoch,
                universe,
                expected_cache_revision: cache_revision,
                snapshot: built,
            })
            .await;
        (published, universe)
    }

    /// Capture every divergent open document in this workspace under one
    /// monotonic overlay epoch. The returned Arc is immutable and cached by
    /// `(root, semantic generation, overlay epoch)`.
    #[cfg(test)]
    pub(super) async fn candidate_overlay_snapshot(
        &self,
        root: &Path,
        generation: SemanticGeneration,
        base_reach_graph: Option<&crate::reachability::ReachGraph>,
        indexed_workspace_files: Option<&crate::indexed_files::IndexedFileList>,
    ) -> Arc<CandidateOverlaySnapshot> {
        let documents = self.session.documents.capture_request_snapshot(None).await;
        let published = self
            .session
            .cache
            .current_engine_snapshot(&root.to_path_buf())
            .await
            .filter(|snapshot| snapshot.semantic_generation == generation);
        let workspace_semantics = if let Some(snapshot) = &published {
            snapshot.workspace_semantics.clone()
        } else {
            let include_paths = self.include_paths.lock().await.clone();
            let go_module_paths = self.go_module_paths.lock().await.clone();
            let mut semantics = super::workspace_config::PublishedWorkspaceSemantics::load_current(
                root,
                &include_paths,
                &go_module_paths,
            );
            semantics.external_roots = self.authorized_external_source_roots(root).await;
            Arc::new(semantics)
        };
        let engine = published
            .filter(|snapshot| {
                (match (&snapshot.reach_graph, base_reach_graph) {
                    (Some(a), Some(b)) => std::ptr::eq(a.as_ref(), b),
                    (None, None) => true,
                    _ => false,
                }) && (match (&snapshot.indexed_files, indexed_workspace_files) {
                    (Some(a), Some(b)) => std::ptr::eq(a.as_ref(), b),
                    (None, None) => true,
                    _ => false,
                })
            })
            .unwrap_or_else(|| {
                let mut engine = super::workspace::EngineSnapshot::empty(root.to_path_buf());
                engine.semantic_generation = generation;
                engine.workspace_semantics = workspace_semantics;
                Arc::new(engine)
            });
        self.candidate_overlay_snapshot_from_documents(root, engine, documents)
            .await
    }

    /// Build an overlay from a caller-owned atomic document capture. This is
    /// used when a request also consumes current-buffer text, ensuring that the
    /// text and the all-open shadow/tombstone set come from the same lock view.
    pub(super) async fn candidate_overlay_snapshot_from_documents(
        &self,
        root: &Path,
        engine: Arc<super::workspace::EngineSnapshot>,
        documents: DocumentRequestSnapshot,
    ) -> Arc<CandidateOverlaySnapshot> {
        let root = root.to_path_buf();
        let epoch = documents.overlay_epoch;
        let generation = engine.semantic_generation;
        let engine_epoch = engine.epoch;
        let workspace_semantics = engine.workspace_semantics.clone();
        let (cached, cache_revision) = self
            .session
            .cache
            .candidate_overlay_for_engine(&root, generation, epoch, engine_epoch)
            .await;
        if let Some(cached) = cached {
            return cached;
        }

        let owned_reach_graph = engine.reach_graph.clone();
        let owned_include_path_index = engine.include_path_index.clone();

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
                    let language = language_resolver.selection_for_source(&path, &snapshot.text);
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
                                    let language = language_resolver.selection_for_source(
                                        Path::new(&identity.identity_path),
                                        &snapshot.text,
                                    );
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
                let identity_path = if language.language == crate::config::SourceLanguage::Go {
                    PathBuf::from(&overlay_path)
                } else {
                    path
                };
                let parsed = self
                    .get_or_parse_captured_document_with_selection(
                        &uri,
                        &identity_path,
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
                owned_reach_graph,
                owned_include_path_index,
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
            .publish_candidate_overlay_for_engine(
                root,
                generation,
                epoch,
                engine_epoch,
                cache_revision,
                built,
            )
            .await
    }
}
