use std::path::{Component, Path, PathBuf};

use anyhow::Result;
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::{CompletionItem, Documentation, MarkupContent, MarkupKind, Url};

use super::{Backend, CompletionDocumentationData};
use crate::model::CandidateRange;
use crate::project_context::ProjectContextIndex;
use crate::{pathing, query};

pub(super) struct PreferredSymbolDocumentation {
    pub presentation: query::DocumentationCandidate,
    pub comment: Option<query::RenderedSymbolComment>,
}

pub(super) fn preferred_symbol_documentation(
    root: &Path,
    current_rel: &str,
    current_text: &str,
    primary: &query::DocumentationCandidate,
    candidates: &[query::DocumentationCandidate],
    project_context: Option<&ProjectContextIndex>,
) -> PreferredSymbolDocumentation {
    let ranked = query::rank_documentation_candidates(primary, candidates, project_context);
    let presentation = ranked.first().cloned().unwrap_or_else(|| primary.clone());
    let comment = ranked.into_iter().find_map(|source_candidate| {
        let source = super::hover::candidate_source_text_for_path(
            root,
            current_rel,
            current_text,
            &source_candidate.candidate.path,
            &source_candidate.candidate.source,
        )?;
        query::comment_documentation_for_candidate_symbol(
            &source,
            &source_candidate.candidate.name,
            source_candidate.candidate.range.start_line,
            &source_candidate.candidate.range,
        )
    });
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
                root,
                uri,
                symbol_id,
            } => {
                let root = PathBuf::from(root);
                if !self.is_workspace_root(&root).await {
                    None
                } else {
                    let (current_rel, current_text) =
                        self.current_document_for_root(&uri, &root).await;
                    let request_uri = Url::parse(&uri).ok();
                    let reach_scope = match request_uri.as_ref() {
                        Some(uri) => self.reach_scope_for(uri).await.map(|(_, reach)| reach),
                        None => None,
                    };
                    let project_context = self
                        .request_context_for_root(root.clone())
                        .await
                        .engine
                        .project_context
                        .clone();
                    let root_for_query = root.clone();
                    let result = tokio::task::spawn_blocking(move || -> Result<Option<String>> {
                        let db_path = pathing::default_index_path(&root_for_query)?;
                        if !db_path.exists() {
                            return Ok(None);
                        }
                        let store = crate::store::IndexStore::open_readonly(&db_path)?;
                        let Some(record) = store
                            .symbol_read_view()
                            .symbols_by_ids(&[symbol_id])?
                            .into_iter()
                            .next()
                        else {
                            return Ok(None);
                        };
                        let primary_ranked = query::rank_hover_candidates(
                            vec![record.clone()],
                            &current_rel,
                            reach_scope.as_deref(),
                            1,
                        );
                        let Some(primary_ranked) = primary_ranked.into_iter().next() else {
                            return Ok(None);
                        };
                        let primary = query::DocumentationCandidate {
                            candidate: primary_ranked.candidate,
                            signature: primary_ranked.signature,
                        };
                        let candidates: Vec<_> = query::rank_hover_candidates(
                            store.symbol_read_view().symbols_by_name(&record.name)?,
                            &current_rel,
                            reach_scope.as_deref(),
                            32,
                        )
                        .into_iter()
                        .map(|candidate| query::DocumentationCandidate {
                            candidate: candidate.candidate,
                            signature: candidate.signature,
                        })
                        .collect();
                        let preferred = preferred_symbol_documentation(
                            &root_for_query,
                            &current_rel,
                            &current_text,
                            &primary,
                            &candidates,
                            project_context.as_deref(),
                        );
                        Ok(completion_popup_markdown(preferred))
                    })
                    .await;
                    self.unwrap_query("completion documentation", result)
                        .await
                        .flatten()
                }
            }
            CompletionDocumentationData::CurrentDocument { uri, start_line } => {
                let Ok(uri) = Url::parse(&uri) else {
                    return Ok(item);
                };
                let Some((_version, text)) = self.document_snapshot(&uri).await else {
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
                    let reach_scope = self.reach_scope_for(&uri).await.map(|(_, reach)| reach);
                    let project_context = self
                        .request_context_for_root(root.clone())
                        .await
                        .engine
                        .project_context
                        .clone();
                    let label = item.label.clone();
                    let result = tokio::task::spawn_blocking(move || -> Result<Option<String>> {
                        let parsed = crate::parser::parse(&path, &text);
                        let live_records = super::language_server::live_definition_records_for_word(
                            &parsed,
                            &label,
                            &current_rel,
                        );
                        let Some(primary_record) = live_records
                            .iter()
                            .find(|record| record.start_line == start_line)
                            .or_else(|| live_records.first())
                            .cloned()
                        else {
                            return Ok(render_symbol_comment(
                                &text,
                                &label,
                                start_line,
                                CandidateRange {
                                    start_line,
                                    start_col: 0,
                                    end_line: start_line,
                                    end_col: 0,
                                },
                            ));
                        };
                        let Some(primary_ranked) = query::rank_hover_candidates(
                            vec![primary_record],
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
                        let mut candidates = vec![primary.clone()];
                        let db_path = pathing::default_index_path(&root)?;
                        if db_path.exists() {
                            let store = crate::store::IndexStore::open_readonly(&db_path)?;
                            candidates.extend(
                                query::rank_hover_candidates(
                                    store.symbol_read_view().symbols_by_name(&label)?,
                                    &current_rel,
                                    reach_scope.as_deref(),
                                    32,
                                )
                                .into_iter()
                                .map(|candidate| {
                                    query::DocumentationCandidate {
                                        candidate: candidate.candidate,
                                        signature: candidate.signature,
                                    }
                                }),
                            );
                        }
                        let preferred = preferred_symbol_documentation(
                            &root,
                            &current_rel,
                            &text,
                            &primary,
                            &candidates,
                            project_context.as_deref(),
                        );
                        Ok(completion_popup_markdown(preferred))
                    })
                    .await;
                    self.unwrap_query("current completion documentation", result)
                        .await
                        .flatten()
                }
            }
            CompletionDocumentationData::Member {
                uri,
                owner_path,
                signature,
            } => {
                let roots = self.workspace_roots.lock().await.clone();
                let client_include_roots = self.include_paths.lock().await.clone();
                let allowed_external_roots: Vec<PathBuf> = roots
                    .iter()
                    .flat_map(|root| {
                        super::configured_include_paths(Some(root), &client_include_roots)
                    })
                    .map(PathBuf::from)
                    .filter(|path| path.is_absolute())
                    .collect();
                let owner = Path::new(&owner_path);
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
                let current_document = if let Ok(uri) = Url::parse(&uri) {
                    match super::uri_to_path(&uri) {
                        Some(path) => self
                            .document_snapshot(&uri)
                            .await
                            .map(|(_version, text)| (path, text)),
                        None => None,
                    }
                } else {
                    None
                };
                let label = item.label.clone();
                let result = tokio::task::spawn_blocking(move || -> Result<Option<String>> {
                    for root in roots {
                        let (current_rel, current_text) = current_document
                            .as_ref()
                            .map(|(uri_path, text)| {
                                (
                                    pathing::relative_slash_path(&root, uri_path)
                                        .unwrap_or_default(),
                                    text.as_str(),
                                )
                            })
                            .unwrap_or_else(|| (String::new(), ""));
                        let source_kind = if Path::new(&owner_path).is_absolute() {
                            "external"
                        } else {
                            "workspace"
                        };
                        let Some(source) = super::hover::candidate_source_text_for_path(
                            &root,
                            &current_rel,
                            current_text,
                            &owner_path,
                            source_kind,
                        ) else {
                            continue;
                        };
                        let parsed = crate::parser::parse(Path::new(&owner_path), &source);
                        let matching: Vec<_> = parsed
                            .members
                            .iter()
                            .filter(|member| member.name == label)
                            .collect();
                        let member = matching
                            .iter()
                            .copied()
                            .find(|member| member.signature == signature)
                            .or_else(|| (matching.len() == 1).then_some(matching[0]));
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

    async fn current_document_for_root(&self, uri: &str, root: &Path) -> (String, String) {
        let Ok(uri) = Url::parse(uri) else {
            return (String::new(), String::new());
        };
        let current_rel = super::uri_to_path(&uri)
            .and_then(|path| pathing::relative_slash_path(root, &path).ok())
            .unwrap_or_default();
        let text = self
            .document_snapshot(&uri)
            .await
            .map(|(_version, text)| text)
            .unwrap_or_default();
        (current_rel, text)
    }
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
