use std::path::PathBuf;

use anyhow::Result;
use tower_lsp::lsp_types::Url;

use super::completion_documentation::{completion_popup_markdown, current_document_for_root};
use super::Backend;
use crate::candidate_service::CandidateHandle;
use crate::query;

impl Backend {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn declaration_completion_documentation(
        &self,
        version: u8,
        root: String,
        uri: String,
        declaration_id: i64,
        semantic_generation: u64,
        overlay_epoch: u64,
        document_version: i32,
    ) -> Option<String> {
        if version != 5 {
            return None;
        }
        let root = PathBuf::from(root);
        if !self.is_workspace_root(&root).await {
            return None;
        }
        let request_uri = Url::parse(&uri).ok();
        let documents = self
            .session
            .documents
            .capture_request_snapshot(request_uri.as_ref())
            .await;
        if documents.overlay_epoch < overlay_epoch
            || documents
                .current
                .as_ref()
                .is_none_or(|snapshot| snapshot.version != document_version)
        {
            return None;
        }
        let context = self.request_context_for_root(root.clone()).await;
        if context.engine.semantic_generation.0 != semantic_generation {
            return None;
        }
        let declaration_index = context.engine.declaration_index.clone()?;
        let declaration_name = declaration_index.core_by_id(declaration_id)?.name.clone();
        let generation = context.engine.semantic_generation;
        let (current_rel, current_text) =
            current_document_for_root(request_uri.as_ref(), &root, documents.current.as_ref());
        let reach_scope = request_uri.as_ref().and_then(|uri| {
            self.reach_scope_from_context(uri, &context)
                .map(|(_, reach)| reach)
        });
        let reach_graph = context.engine.reach_graph.clone();
        let call_read_handle = context.engine.call_read_handle.clone();
        let query_index = declaration_index.clone();
        let overlay = self
            .candidate_overlay_snapshot_from_documents(
                &root,
                generation,
                reach_graph.as_deref(),
                context.engine.indexed_files.as_deref().map(Vec::as_slice),
                documents,
            )
            .await;
        let cache_before = declaration_index.payload_cache_stats();
        let query_started = std::time::Instant::now();
        let result = tokio::task::spawn_blocking(move || -> Result<Option<String>> {
            let service = crate::candidate_service::CandidateQueryService::new_with_declarations(
                call_read_handle.as_deref(),
                Some(&query_index),
                &overlay,
                &current_rel,
                reach_scope.as_deref(),
                reach_graph.as_deref(),
            );
            let semantic = service.semantic_candidates(
                &declaration_name,
                crate::candidate_service::SemanticIntent::Neutral,
            )?;
            let Some(candidate) = semantic
                .all
                .iter()
                .flat_map(|group| group.candidates.iter())
                .find(|candidate| candidate.persistent_id == Some(declaration_id))
                .cloned()
            else {
                return Ok(None);
            };
            render_candidate_popup(
                &service,
                candidate,
                &root,
                &current_rel,
                &current_text,
                &overlay,
                Some(semantic),
            )
        })
        .await;
        let documentation = self
            .unwrap_query("completion declaration payload", result)
            .await
            .flatten();
        let query_us = query_started.elapsed().as_micros();
        let cache_after = declaration_index.payload_cache_stats();
        self.perf_log(|| {
            format!(
                "[perf] declaration_payload feature=completion_resolve query_us={query_us} cache_hit={} sql_reads={} evictions={} cache_entries={} cache_bytes={}",
                (cache_after.hits > cache_before.hits) as u8,
                cache_after.sql_reads.saturating_sub(cache_before.sql_reads),
                cache_after.evictions.saturating_sub(cache_before.evictions),
                cache_after.entries,
                cache_after.bytes,
            )
        })
        .await;
        documentation
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn candidate_completion_documentation(
        &self,
        version: u8,
        root: String,
        uri: String,
        handle: CandidateHandle,
        semantic_generation: u64,
        overlay_epoch: u64,
        document_version: i32,
    ) -> Option<String> {
        let root = PathBuf::from(root);
        let request_uri = Url::parse(&uri).ok();
        let standalone_root = request_uri
            .as_ref()
            .and_then(super::uri_to_path)
            .and_then(|path| path.parent().map(PathBuf::from))
            .is_some_and(|parent| parent == root);
        if version != 4 {
            return None;
        }
        if !self.is_workspace_root(&root).await && !standalone_root {
            return None;
        }
        let documents = self
            .session
            .documents
            .capture_request_snapshot(request_uri.as_ref())
            .await;
        if documents.overlay_epoch < overlay_epoch
            || documents
                .current
                .as_ref()
                .is_none_or(|snapshot| snapshot.version != document_version)
        {
            return None;
        }
        let context = self.request_context_for_root(root.clone()).await;
        let generation = crate::call_model::SemanticGeneration(semantic_generation);
        if context.engine.semantic_generation != generation {
            return None;
        }
        let (current_rel, current_text) =
            current_document_for_root(request_uri.as_ref(), &root, documents.current.as_ref());
        let reach_scope = request_uri.as_ref().and_then(|uri| {
            self.reach_scope_from_context(uri, &context)
                .map(|(_, reach)| reach)
        });
        let reach_graph = context.engine.reach_graph.clone();
        let call_read_handle = context.engine.call_read_handle.clone();
        let declaration_index = context.engine.declaration_index.clone();
        let overlay = self
            .candidate_overlay_snapshot_from_documents(
                &root,
                generation,
                reach_graph.as_deref(),
                context.engine.indexed_files.as_deref().map(Vec::as_slice),
                documents,
            )
            .await;
        let result = tokio::task::spawn_blocking(move || -> Result<Option<String>> {
            let service = crate::candidate_service::CandidateQueryService::new_with_declarations(
                call_read_handle.as_deref(),
                declaration_index.as_deref(),
                &overlay,
                &current_rel,
                reach_scope.as_deref(),
                reach_graph.as_deref(),
            );
            let Some(candidate) = service.resolve_candidate_handle(&handle)? else {
                return Ok(None);
            };
            render_candidate_popup(
                &service,
                candidate,
                &root,
                &current_rel,
                &current_text,
                &overlay,
                None,
            )
        })
        .await;
        self.unwrap_query("candidate completion documentation", result)
            .await
            .flatten()
    }
}

fn render_candidate_popup(
    service: &crate::candidate_service::CandidateQueryService<'_>,
    candidate: crate::candidate_service::ResolvedDeclarationCandidate,
    root: &std::path::Path,
    current_rel: &str,
    current_text: &str,
    overlay: &crate::candidate_service::CandidateOverlaySnapshot,
    semantic: Option<
        crate::model::CandidateSet<crate::candidate_service::ResolvedDeclarationCandidate>,
    >,
) -> Result<Option<String>> {
    let presentation = query::DocumentationCandidate {
        candidate: candidate.as_definition_candidate(),
        signature: candidate
            .fact
            .canonical_signature
            .clone()
            .unwrap_or_else(|| candidate.fact.name.clone()),
    };
    let semantic = match semantic {
        Some(semantic) => semantic,
        None => service.semantic_candidates(
            &candidate.fact.name,
            crate::candidate_service::SemanticIntent::Neutral,
        )?,
    };
    let mut candidates: Vec<_> = semantic
        .all
        .iter()
        .flat_map(|group| group.candidates.iter())
        .filter(|related| related.fact.identity.logical_key == candidate.fact.identity.logical_key)
        .map(|related| {
            (
                related.fact.role,
                related.fact.declaration_range,
                query::DocumentationCandidate {
                    candidate: related.as_definition_candidate(),
                    signature: related
                        .fact
                        .canonical_signature
                        .clone()
                        .unwrap_or_else(|| related.fact.name.clone()),
                },
            )
        })
        .collect();
    candidates.sort_by_key(|(role, _, candidate)| {
        (
            match role {
                crate::semantic_model::SemanticDeclarationRole::Declaration => 0,
                crate::semantic_model::SemanticDeclarationRole::Definition => 1,
                crate::semantic_model::SemanticDeclarationRole::TentativeDefinition => 2,
                crate::semantic_model::SemanticDeclarationRole::Unknown => 3,
            },
            candidate.candidate.path.clone(),
        )
    });
    let paths: Vec<_> = candidates
        .iter()
        .map(|(_, _, candidate)| candidate.candidate.path.clone())
        .collect();
    let revisions = service.source_revisions(&paths)?;
    for (_, declaration_range, mut source_candidate) in candidates {
        let source = super::hover::candidate_source_text_for_path_with_overlay_at_revision(
            root,
            current_rel,
            current_text,
            overlay,
            &source_candidate.candidate.path,
            &source_candidate.candidate.source,
            revisions.get(&source_candidate.candidate.path),
        );
        if let Some(signature) = source
            .as_deref()
            .and_then(|source| declaration_signature(source, declaration_range))
        {
            source_candidate.signature = signature.to_string();
        }
        let comment = source.as_deref().and_then(|source| {
            query::comment_documentation_for_candidate_symbol(
                source,
                &source_candidate.candidate.name,
                source_candidate.candidate.range.start_line,
                &source_candidate.candidate.range,
            )
        });
        if comment.is_some() {
            return Ok(completion_popup_markdown(
                super::completion_documentation::PreferredSymbolDocumentation {
                    presentation: source_candidate,
                    comment,
                },
            ));
        }
    }
    Ok(completion_popup_markdown(
        super::completion_documentation::PreferredSymbolDocumentation {
            presentation,
            comment: None,
        },
    ))
}

fn declaration_signature(source: &str, range: crate::call_model::SourceRange) -> Option<&str> {
    source
        .get(range.start_byte..range.end_byte)
        .map(str::trim)
        .filter(|signature| !signature.is_empty())
}
