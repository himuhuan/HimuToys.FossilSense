use std::path::PathBuf;

use anyhow::Result;
use tower_lsp::lsp_types::Url;

use super::completion_documentation::{completion_popup_markdown, current_document_for_root};
use super::Backend;
use crate::candidate_service::CandidateHandle;
use crate::query;

impl Backend {
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
            let service = crate::candidate_service::CandidateQueryService::new(
                call_read_handle.as_deref(),
                &overlay,
                &current_rel,
                reach_scope.as_deref(),
                reach_graph.as_deref(),
            );
            let Some(candidate) = service.resolve_candidate_handle(&handle)? else {
                return Ok(None);
            };
            let presentation = query::DocumentationCandidate {
                candidate: candidate.as_definition_candidate(),
                signature: candidate
                    .fact
                    .canonical_signature
                    .clone()
                    .unwrap_or_else(|| candidate.fact.name.clone()),
            };
            let semantic = service.semantic_candidates(
                &candidate.fact.name,
                crate::candidate_service::SemanticIntent::Neutral,
            )?;
            let mut candidates: Vec<_> = semantic
                .all
                .iter()
                .flat_map(|group| group.candidates.iter())
                .filter(|related| {
                    related.fact.identity.logical_key == candidate.fact.identity.logical_key
                })
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
                    &root,
                    &current_rel,
                    &current_text,
                    &overlay,
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
        })
        .await;
        self.unwrap_query("candidate completion documentation", result)
            .await
            .flatten()
    }
}

fn declaration_signature(source: &str, range: crate::call_model::SourceRange) -> Option<&str> {
    source
        .get(range.start_byte..range.end_byte)
        .map(str::trim)
        .filter(|signature| !signature.is_empty())
}
