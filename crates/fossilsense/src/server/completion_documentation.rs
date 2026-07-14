use std::path::{Component, Path, PathBuf};

use anyhow::Result;
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::{CompletionItem, Documentation, MarkupContent, MarkupKind, Url};

use super::{Backend, CompletionDocumentationData};
use crate::model::CandidateRange;
use crate::project_context::ProjectContextIndex;
use crate::query::callables::CallableVariantGroup;
use crate::{pathing, query};

pub(super) struct PreferredSymbolDocumentation {
    pub presentation: query::DocumentationCandidate,
    pub comment: Option<query::RenderedSymbolComment>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn preferred_symbol_documentation(
    root: &Path,
    current_rel: &str,
    current_text: &str,
    primary: &query::DocumentationCandidate,
    candidates: &[query::DocumentationCandidate],
    project_context: Option<&ProjectContextIndex>,
    overlay: Option<&crate::candidate_service::CandidateOverlaySnapshot>,
    revisions: &std::collections::HashMap<String, query::CandidateRevision>,
    mut hydration: Option<&mut super::HydrationStats>,
) -> PreferredSymbolDocumentation {
    let ranked = query::rank_documentation_candidates(primary, candidates, project_context);
    let presentation = ranked.first().cloned().unwrap_or_else(|| primary.clone());
    let mut comment = None;
    for source_candidate in ranked {
        let source = match overlay {
            Some(overlay) => super::hover::candidate_source_text_for_path_with_overlay_at_revision(
                root,
                current_rel,
                current_text,
                overlay,
                &source_candidate.candidate.path,
                &source_candidate.candidate.source,
                revisions.get(&source_candidate.candidate.path),
            ),
            None => super::hover::candidate_source_text_for_path_at_revision(
                root,
                current_rel,
                current_text,
                &source_candidate.candidate.path,
                &source_candidate.candidate.source,
                revisions.get(&source_candidate.candidate.path),
            ),
        };
        if let Some(hydration) = hydration.as_deref_mut() {
            hydration.record(source.as_deref());
        }
        let Some(source) = source else {
            continue;
        };
        comment = query::comment_documentation_for_candidate_symbol(
            &source,
            &source_candidate.candidate.name,
            source_candidate.candidate.range.start_line,
            &source_candidate.candidate.range,
        );
        if comment.is_some() {
            break;
        }
    }
    PreferredSymbolDocumentation {
        presentation,
        comment,
    }
}

impl Backend {
    pub(super) async fn resolve_completion_documentation(
        &self,
        item: CompletionItem,
    ) -> LspResult<CompletionItem> {
        let Some(data) = item
            .data
            .clone()
            .and_then(|value| serde_json::from_value::<CompletionDocumentationData>(value).ok())
        else {
            return Ok(item);
        };

        let comment = match data {
            CompletionDocumentationData::Indexed {
                version,
                root,
                uri,
                label,
                symbol_id,
                semantic_generation,
                overlay_epoch,
                document_version,
            } => {
                let root = PathBuf::from(root);
                if version != 3 || !self.is_workspace_root(&root).await {
                    None
                } else {
                    let request_uri = Url::parse(&uri).ok();
                    let documents = self
                        .session
                        .documents
                        .capture_request_snapshot(request_uri.as_ref())
                        .await;
                    if !completion_snapshot_matches(&documents, overlay_epoch, document_version) {
                        return Ok(item);
                    }
                    let (current_rel, current_text) = current_document_for_root(
                        request_uri.as_ref(),
                        &root,
                        documents.current.as_ref(),
                    );
                    let context = self.request_context_for_root(root.clone()).await;
                    let generation = crate::call_model::SemanticGeneration(semantic_generation);
                    // The payload is a locator into one exact generation. A
                    // later publication may supply a different ReachGraph even
                    // while the old SQLite generation remains leased. Do not
                    // mix those inputs; conservatively leave documentation
                    // unattached and let a fresh completion carry a fresh locator.
                    if generation != context.engine.semantic_generation {
                        return Ok(item);
                    }
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
                    let root_for_query = root.clone();
                    let result = tokio::task::spawn_blocking(move || -> Result<Option<String>> {
                        let Some(handle) = call_read_handle else {
                            return Ok(None);
                        };
                        let record = handle.read(|store| {
                            Ok(store
                                .symbol_read_view()
                                .symbols_by_ids(&[symbol_id])?
                                .into_iter()
                                .next())
                        })?;
                        let Some(record) = record else {
                            return Ok(None);
                        };
                        if record.name != label {
                            return Ok(None);
                        }
                        if overlay.shadows(&record.path) {
                            return Ok(None);
                        }
                        let service = crate::candidate_service::CandidateQueryService::new(
                            Some(&handle),
                            &overlay,
                            &current_rel,
                            reach_scope.as_deref(),
                            reach_graph.as_deref(),
                        );
                        if record.kind == "function" {
                            let callable = service.callable_candidates(&record.name, None)?;
                            let Some(candidate) = indexed_callable_presentation(
                                &callable.groups,
                                &record.path,
                                CandidateRange {
                                    start_line: record.start_line,
                                    start_col: record.start_col,
                                    end_line: record.end_line,
                                    end_col: record.end_col,
                                },
                            ) else {
                                // The payload identifies a concrete symbol in this
                                // generation. Never attach documentation from a
                                // different same-label callable group when that
                                // locator is absent (for example after dirty shadowing).
                                return Ok(None);
                            };
                            let signature =
                                if candidate.anchor.presentation_signature.trim().is_empty() {
                                    candidate.anchor.signature.normalized.clone()
                                } else {
                                    candidate.anchor.presentation_signature.clone()
                                };
                            let revisions = service
                                .source_revisions(std::slice::from_ref(&candidate.candidate.path))?;
                            let source = super::hover::candidate_source_text_for_path_with_overlay_at_revision(
                                &root_for_query,
                                &current_rel,
                                &current_text,
                                &overlay,
                                &candidate.candidate.path,
                                &candidate.candidate.source,
                                revisions.get(&candidate.candidate.path),
                            );
                            let comment = source.as_deref().and_then(|source| {
                                query::comment_documentation_for_candidate_symbol(
                                    source,
                                    &candidate.candidate.name,
                                    candidate.candidate.range.start_line,
                                    &candidate.candidate.range,
                                )
                            });
                            return Ok(completion_popup_markdown(PreferredSymbolDocumentation {
                                presentation: query::DocumentationCandidate {
                                    candidate: candidate.candidate.clone(),
                                    signature,
                                },
                                comment,
                            }));
                        }
                        let Some(primary_ranked) = query::rank_hover_candidates(
                            vec![record],
                            &current_rel,
                            reach_scope.as_deref(),
                            1,
                        )
                        .into_iter()
                        .next() else {
                            return Ok(None);
                        };
                        let primary = query::DocumentationCandidate {
                            candidate: primary_ranked.candidate,
                            signature: primary_ranked.signature,
                        };
                        let revisions = service
                            .source_revisions(std::slice::from_ref(&primary.candidate.path))?;
                        let preferred = preferred_symbol_documentation(
                            &root_for_query,
                            &current_rel,
                            &current_text,
                            &primary,
                            std::slice::from_ref(&primary),
                            None,
                            Some(&overlay),
                            &revisions,
                            None,
                        );
                        Ok(completion_popup_markdown(preferred))
                    })
                    .await;
                    self.unwrap_query("completion documentation", result)
                        .await
                        .flatten()
                }
            }
            CompletionDocumentationData::CurrentDocument {
                version,
                uri,
                start_line,
                overlay_epoch,
                document_version,
            } => {
                if version != 3 {
                    return Ok(item);
                }
                let Ok(uri) = Url::parse(&uri) else {
                    return Ok(item);
                };
                let documents = self
                    .session
                    .documents
                    .capture_request_snapshot(Some(&uri))
                    .await;
                if !completion_snapshot_matches(&documents, overlay_epoch, document_version) {
                    return Ok(item);
                }
                let Some(text) = documents
                    .current
                    .as_ref()
                    .map(|snapshot| snapshot.text.clone())
                else {
                    return Ok(item);
                };
                if text.len() as u64 > super::hover::HOVER_SOURCE_FILE_BYTE_LIMIT {
                    None
                } else {
                    let root = self.root_for_uri(&uri).await;
                    let Some(root) = root else {
                        let direct_comment = render_symbol_comment(
                            &text,
                            &item.label,
                            start_line,
                            CandidateRange {
                                start_line,
                                start_col: 0,
                                end_line: start_line,
                                end_col: 0,
                            },
                        );
                        return Ok(with_comment_if_present(item, direct_comment));
                    };
                    let path = super::uri_to_path(&uri).unwrap_or_else(|| root.clone());
                    let current_rel =
                        pathing::relative_slash_path(&root, &path).unwrap_or_default();
                    let context = self.request_context_for_root(root.clone()).await;
                    let reach_scope = self
                        .reach_scope_from_context(&uri, &context)
                        .map(|(_, reach)| reach);
                    let call_read_handle = context.engine.call_read_handle.clone();
                    let reach_graph = context.engine.reach_graph.clone();
                    let overlay = self
                        .candidate_overlay_snapshot_from_documents(
                            &root,
                            context.engine.semantic_generation,
                            reach_graph.as_deref(),
                            context.engine.indexed_files.as_deref().map(Vec::as_slice),
                            documents,
                        )
                        .await;
                    let label = item.label.clone();
                    let result = tokio::task::spawn_blocking(move || -> Result<Option<String>> {
                        let service = crate::candidate_service::CandidateQueryService::new(
                            call_read_handle.as_deref(),
                            &overlay,
                            &current_rel,
                            reach_scope.as_deref(),
                            reach_graph.as_deref(),
                        );
                        let callable = service.callable_candidates(&label, None)?;
                        if let Some(candidate) = current_document_callable_presentation(
                            &callable.groups,
                            &current_rel,
                            start_line,
                        ) {
                            let signature =
                                if candidate.anchor.presentation_signature.trim().is_empty() {
                                    candidate.anchor.signature.normalized.clone()
                                } else {
                                    candidate.anchor.presentation_signature.clone()
                                };
                            let revisions = service.source_revisions(std::slice::from_ref(
                                &candidate.candidate.path,
                            ))?;
                            let source = super::hover::candidate_source_text_for_path_with_overlay_at_revision(
                                &root,
                                &current_rel,
                                &text,
                                &overlay,
                                &candidate.candidate.path,
                                &candidate.candidate.source,
                                revisions.get(&candidate.candidate.path),
                            );
                            let comment = source.as_deref().and_then(|source| {
                                query::comment_documentation_for_candidate_symbol(
                                    source,
                                    &candidate.candidate.name,
                                    candidate.candidate.range.start_line,
                                    &candidate.candidate.range,
                                )
                            });
                            return Ok(completion_popup_markdown(PreferredSymbolDocumentation {
                                presentation: query::DocumentationCandidate {
                                    candidate: candidate.candidate.clone(),
                                    signature,
                                },
                                comment,
                            }));
                        }
                        Ok(render_symbol_comment(
                            &text,
                            &label,
                            start_line,
                            CandidateRange {
                                start_line,
                                start_col: 0,
                                end_line: start_line,
                                end_col: 0,
                            },
                        ))
                    })
                    .await;
                    self.unwrap_query("current completion documentation", result)
                        .await
                        .flatten()
                }
            }
            CompletionDocumentationData::Overlay {
                version,
                root,
                uri,
                label,
                path,
                start_line,
                start_col,
                end_line,
                end_col,
                semantic_generation,
                overlay_epoch,
                document_version,
            } => {
                let root = PathBuf::from(root);
                if version != 3 || !self.is_workspace_root(&root).await {
                    return Ok(item);
                }
                let request_uri = Url::parse(&uri).ok();
                let documents = self
                    .session
                    .documents
                    .capture_request_snapshot(request_uri.as_ref())
                    .await;
                if !completion_snapshot_matches(&documents, overlay_epoch, document_version) {
                    return Ok(item);
                }
                let context = self.request_context_for_root(root.clone()).await;
                if context.engine.semantic_generation.0 != semantic_generation {
                    return Ok(item);
                }
                let overlay = self
                    .candidate_overlay_snapshot_from_documents(
                        &root,
                        context.engine.semantic_generation,
                        context.engine.reach_graph.as_deref(),
                        context.engine.indexed_files.as_deref().map(Vec::as_slice),
                        documents,
                    )
                    .await;
                let expected = CandidateRange {
                    start_line,
                    start_col,
                    end_line,
                    end_col,
                };
                let fact = overlay.symbols(&label).iter().find(|fact| {
                    fact.path == path
                        && CandidateRange {
                            start_line: fact.symbol.start_line as u32,
                            start_col: fact.symbol.start_col as u32,
                            end_line: fact.symbol.end_line as u32,
                            end_col: fact.symbol.end_col as u32,
                        } == expected
                });
                fact.and_then(|fact| {
                    let source = overlay.source_text(&fact.path)?;
                    render_symbol_comment(source, &label, start_line, expected)
                })
            }
            CompletionDocumentationData::Member {
                version,
                uri,
                owner_path,
                signature,
                owner_revision_hash,
                overlay_epoch,
                document_version,
            } => {
                if version != 3 {
                    return Ok(item);
                }
                let roots = self.workspace_roots.lock().await.clone();
                let client_include_roots = self.include_paths.lock().await.clone();
                let config_roots = roots.clone();
                let allowed_external_roots: Vec<PathBuf> = tokio::task::spawn_blocking(move || {
                    config_roots
                        .iter()
                        .flat_map(|root| {
                            super::configured_include_paths(Some(root), &client_include_roots)
                        })
                        .map(PathBuf::from)
                        .filter(|path| path.is_absolute())
                        .collect()
                })
                .await
                .unwrap_or_default();
                let owner = Path::new(&owner_path);
                let owner_is_absolute = owner.is_absolute();
                let owner_allowed = if owner.is_absolute() {
                    allowed_external_roots
                        .iter()
                        .any(|root| pathing::path_is_within(root, owner))
                } else {
                    !owner.components().any(|component| {
                        matches!(
                            component,
                            Component::ParentDir | Component::RootDir | Component::Prefix(_)
                        )
                    })
                };
                if !owner_allowed {
                    return Ok(item);
                }
                let request_uri = Url::parse(&uri).ok();
                let documents = self
                    .session
                    .documents
                    .capture_request_snapshot(request_uri.as_ref())
                    .await;
                if !completion_snapshot_matches(&documents, overlay_epoch, document_version) {
                    return Ok(item);
                }
                let current_document = request_uri.as_ref().and_then(super::uri_to_path).zip(
                    documents
                        .current
                        .as_ref()
                        .map(|snapshot| snapshot.text.clone()),
                );
                let open_documents: Vec<_> = documents
                    .all
                    .into_iter()
                    .filter_map(|(uri, snapshot)| {
                        super::uri_to_path(&uri).map(|path| (path, snapshot.text))
                    })
                    .collect();
                let normalized_owner =
                    owner_is_absolute.then(|| pathing::normalize_abs_path(Path::new(&owner_path)));
                let label = item.label.clone();
                let result = tokio::task::spawn_blocking(move || -> Result<Option<String>> {
                    for root in roots {
                        let (current_rel, current_text) = current_document
                            .as_ref()
                            .map(|(uri_path, text)| {
                                (
                                    pathing::relative_slash_path(&root, uri_path)
                                        .unwrap_or_default(),
                                    text.as_ref(),
                                )
                            })
                            .unwrap_or_else(|| (String::new(), ""));
                        let source_kind = if owner_is_absolute {
                            "external"
                        } else {
                            "workspace"
                        };
                        let open_source = open_documents.iter().find_map(|(path, text)| {
                            let matches = if owner_is_absolute {
                                normalized_owner.as_ref().is_some_and(|owner| {
                                    pathing::normalize_abs_path(path) == *owner
                                })
                            } else {
                                pathing::relative_slash_path(&root, path)
                                    .is_ok_and(|relative| relative == owner_path)
                            };
                            matches.then(|| text.to_string())
                        });
                        let source = open_source.or_else(|| {
                            super::hover::candidate_source_text_for_path(
                                &root,
                                &current_rel,
                                current_text,
                                &owner_path,
                                source_kind,
                            )
                        });
                        let Some(source) = source else {
                            continue;
                        };
                        if blake3::hash(source.as_bytes()).to_hex().as_str() != owner_revision_hash
                        {
                            continue;
                        }
                        let parsed = crate::parser::parse(Path::new(&owner_path), &source);
                        let matching: Vec<_> = parsed
                            .members
                            .iter()
                            .filter(|member| member.name == label)
                            .collect();
                        let member = matching
                            .iter()
                            .copied()
                            .find(|member| member.signature == signature);
                        let Some(member) = member else {
                            continue;
                        };
                        return Ok(render_symbol_comment(
                            &source,
                            &member.name,
                            member.start_line as u32,
                            CandidateRange {
                                start_line: member.start_line as u32,
                                start_col: member.start_col as u32,
                                end_line: member.end_line as u32,
                                end_col: member.end_col as u32,
                            },
                        ));
                    }
                    Ok(None)
                })
                .await;
                self.unwrap_query("member completion documentation", result)
                    .await
                    .flatten()
            }
        };

        Ok(with_comment_if_present(item, comment))
    }

    async fn is_workspace_root(&self, requested: &Path) -> bool {
        self.workspace_roots
            .lock()
            .await
            .iter()
            .any(|root| root == requested)
    }
}

fn completion_snapshot_matches(
    documents: &super::workspace::DocumentRequestSnapshot,
    overlay_epoch: u64,
    document_version: i32,
) -> bool {
    documents.overlay_epoch == overlay_epoch
        && documents
            .current
            .as_ref()
            .is_some_and(|snapshot| snapshot.version == document_version)
}

fn current_document_for_root(
    uri: Option<&Url>,
    root: &Path,
    snapshot: Option<&super::workspace::DocumentSnapshot>,
) -> (String, String) {
    let current_rel = uri
        .and_then(super::uri_to_path)
        .and_then(|path| pathing::relative_slash_path(root, &path).ok())
        .unwrap_or_default();
    let text = snapshot
        .map(|snapshot| snapshot.text.to_string())
        .unwrap_or_default();
    (current_rel, text)
}

fn with_comment_if_present(mut item: CompletionItem, comment: Option<String>) -> CompletionItem {
    if let Some(comment) = comment {
        item.documentation = Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: combine_documentation(&comment, item.documentation.as_ref()),
        }));
    }
    item
}

fn indexed_callable_presentation<'a>(
    groups: &'a [CallableVariantGroup],
    target_path: &str,
    target_range: CandidateRange,
) -> Option<&'a query::ResolvedCallableAnchor> {
    callable_presentation_for_matching_group(groups, |candidate| {
        candidate.candidate.path == target_path
            && (candidate.candidate.range == target_range
                || callable_declaration_range(candidate) == target_range)
    })
}

fn callable_declaration_range(candidate: &query::ResolvedCallableAnchor) -> CandidateRange {
    CandidateRange {
        start_line: candidate.anchor.declaration_range.start.line,
        start_col: candidate.anchor.declaration_range.start.character,
        end_line: candidate.anchor.declaration_range.end.line,
        end_col: candidate.anchor.declaration_range.end.character,
    }
}

fn current_document_callable_presentation<'a>(
    groups: &'a [CallableVariantGroup],
    current_path: &str,
    start_line: u32,
) -> Option<&'a query::ResolvedCallableAnchor> {
    callable_presentation_for_matching_group(groups, |candidate| {
        candidate.candidate.path == current_path
            && candidate.candidate.range.start_line == start_line
    })
}

fn callable_presentation_for_matching_group(
    groups: &[CallableVariantGroup],
    matches: impl Fn(&query::ResolvedCallableAnchor) -> bool,
) -> Option<&query::ResolvedCallableAnchor> {
    let group = groups.iter().find(|group| group.variants().any(&matches))?;
    query::signature_presentations(std::slice::from_ref(group))
        .into_iter()
        .next()
}

fn render_symbol_comment(
    source: &str,
    name: &str,
    start_line: u32,
    range: CandidateRange,
) -> Option<String> {
    query::comment_documentation_for_candidate_symbol(source, name, start_line, &range)
        .map(|comment| comment.markdown)
}

fn completion_popup_markdown(preferred: PreferredSymbolDocumentation) -> Option<String> {
    let comment = preferred.comment.map(|comment| comment.markdown);
    let presentation = preferred.presentation;
    let is_header_declaration = presentation.candidate.role == "declaration"
        && Path::new(&presentation.candidate.path)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                matches!(
                    extension.to_ascii_lowercase().as_str(),
                    "h" | "hh" | "hpp" | "hxx" | "inl"
                )
            });
    if !is_header_declaration {
        return comment;
    }
    let declaration = presentation.signature.replace("```", "'''");
    let declaration_block = format!(
        "```c\n// In {}\n{}\n```",
        presentation.candidate.path, declaration
    );
    Some(match comment {
        Some(comment) if !comment.trim().is_empty() => {
            format!("{}\n\n{declaration_block}", comment.trim_end())
        }
        _ => declaration_block,
    })
}

fn combine_documentation(comment: &str, existing: Option<&Documentation>) -> String {
    let existing = existing.map(|documentation| match documentation {
        Documentation::String(value) => value.as_str(),
        Documentation::MarkupContent(markup) => markup.value.as_str(),
    });
    match existing.filter(|value| !value.trim().is_empty()) {
        Some(existing) => format!("{}\n\n---\n\n{existing}", comment.trim_end()),
        None => comment.trim_end().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::candidate_service::{
        CandidateOverlaySnapshot, CandidateQueryService, FileCandidateOverlay,
    };
    use crate::parser::{parse_with_handle, ParseFacts};
    use crate::reachability::ReachGraph;

    fn callable_candidates(
        files: &[(&str, &str)],
        current_path: &str,
        reach_graph: Option<&ReachGraph>,
    ) -> query::CallableCandidateSet {
        let overlays = files
            .iter()
            .map(|(path, source)| {
                let parsed =
                    parse_with_handle(Path::new(path), source, None, ParseFacts::HOVER_SEMANTICS);
                FileCandidateOverlay::from_index((*path).to_string(), &parsed)
            })
            .collect();
        let snapshot = CandidateOverlaySnapshot::new(1, overlays);
        let reach = reach_graph.map(|graph| graph.reachable(current_path));
        CandidateQueryService::new(None, &snapshot, current_path, reach.as_deref(), reach_graph)
            .callable_candidates("choose", None)
            .expect("callable candidates")
    }

    #[test]
    fn indexed_callable_locator_selects_its_group_instead_of_first_same_label_group() {
        let candidates = callable_candidates(
            &[
                ("a.h", "int choose(int value);\n"),
                ("b.h", "int choose(double value);\n"),
            ],
            "main.c",
            None,
        );
        assert_eq!(
            query::signature_presentations(&candidates.groups)[0]
                .candidate
                .path,
            "a.h"
        );
        let target = candidates
            .groups
            .iter()
            .flat_map(CallableVariantGroup::variants)
            .find(|candidate| candidate.candidate.path == "b.h")
            .expect("b.h overload");
        let target_range = callable_declaration_range(target);
        assert_ne!(target_range, target.candidate.range);

        let selected = indexed_callable_presentation(&candidates.groups, "b.h", target_range)
            .expect("matching presentation");
        assert_eq!(selected.candidate.path, "b.h");
        assert!(selected.anchor.presentation_signature.contains("double"));
    }

    #[test]
    fn indexed_source_locator_uses_header_representative_of_the_matching_group() {
        let graph = ReachGraph::new(
            vec![("api.c".into(), "api.h".into())],
            Vec::new(),
            Vec::new(),
        );
        let candidates = callable_candidates(
            &[
                ("api.h", "int choose(int value);\n"),
                ("api.c", "int choose(int value) { return value; }\n"),
            ],
            "api.c",
            Some(&graph),
        );
        let source = candidates
            .groups
            .iter()
            .flat_map(CallableVariantGroup::variants)
            .find(|candidate| candidate.candidate.path == "api.c")
            .expect("source variant");
        let source_range = callable_declaration_range(source);

        let selected = indexed_callable_presentation(&candidates.groups, "api.c", source_range)
            .expect("matching logical group");
        assert_eq!(selected.candidate.path, "api.h");
        assert_eq!(selected.candidate.role, "declaration");
    }

    #[test]
    fn current_document_locator_uses_path_and_line_instead_of_first_overload() {
        let candidates = callable_candidates(
            &[(
                "current.h",
                "int choose(int value);\nint choose(double value);\n",
            )],
            "current.h",
            None,
        );

        let selected = current_document_callable_presentation(&candidates.groups, "current.h", 1)
            .expect("second overload presentation");
        assert_eq!(selected.candidate.range.start_line, 1);
        assert!(selected.anchor.presentation_signature.contains("double"));
    }

    #[test]
    fn callable_source_read_prefers_dirty_overlay_over_saved_file() {
        let root = unique_temp_root("completion-overlay-source");
        let include = root.join("include");
        std::fs::create_dir_all(&include).expect("create include directory");
        std::fs::write(
            include.join("api.h"),
            "/// Old saved documentation.\nint choose(int value);\n",
        )
        .expect("write saved header");
        let dirty: Arc<str> = Arc::from("/// New dirty documentation.\nint choose(int value);\n");
        let parsed = parse_with_handle(
            Path::new("include/api.h"),
            &dirty,
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let overlay = CandidateOverlaySnapshot::new(
            2,
            vec![FileCandidateOverlay::from_index_with_text(
                "include/api.h".into(),
                &parsed,
                Arc::clone(&dirty),
            )],
        );

        let source = super::super::hover::candidate_source_text_for_path_with_overlay_at_revision(
            &root,
            "src/main.c",
            "",
            &overlay,
            "include/api.h",
            "workspace",
            None,
        )
        .expect("dirty source");
        let _ = std::fs::remove_dir_all(&root);
        assert!(source.contains("New dirty documentation."));
        assert!(!source.contains("Old saved documentation."));
    }

    fn unique_temp_root(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("fossilsense-{name}-{}-{nanos}", std::process::id()))
    }
}
