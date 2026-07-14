use std::collections::HashSet;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Arc;

use anyhow::Result;
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::{Hover, HoverContents, HoverParams, MarkupContent, MarkupKind};

use super::{uri_to_path, Backend, HydrationStats, SemanticRequestPerf};
use crate::call_model::SourcePosition;
use crate::candidate_service::CandidateQueryService;
use crate::pathing;
use crate::query;

pub(super) const HOVER_SOURCE_FILE_BYTE_LIMIT: u64 = 256 * 1024;

impl Backend {
    pub(super) async fn provide_hover(&self, params: HoverParams) -> LspResult<Option<Hover>> {
        let position = params.text_document_position_params;
        let uri = position.text_document.uri;
        let documents = self
            .session
            .documents
            .capture_request_snapshot(Some(&uri))
            .await;
        let Some((_version, text)) = self.document_snapshot_from_request(&uri, &documents).await
        else {
            return Ok(None);
        };
        let line_text = text
            .lines()
            .nth(position.position.line as usize)
            .unwrap_or_default();
        let Some(word) = query::word_at(line_text, position.position.character) else {
            return Ok(None);
        };
        let Some(root) = self.root_for_uri(&uri).await else {
            return Ok(None);
        };
        let current_rel = uri_to_path(&uri)
            .and_then(|path| pathing::relative_slash_path(&root, &path).ok())
            .unwrap_or_default();
        let total_started = std::time::Instant::now();
        let context = self.request_context_for_root(root.clone()).await;
        let reach_started = std::time::Instant::now();
        let reach_scope = self
            .reach_scope_from_context(&uri, &context)
            .map(|(_, reach)| reach);
        let mut reach_us = reach_started.elapsed().as_micros();
        let project_context = context.engine.project_context.clone();
        let semantic_generation = context.engine.semantic_generation;
        let call_read_handle = context.engine.call_read_handle.clone();
        let reach_graph = context.engine.reach_graph.clone();
        let overlay_started = std::time::Instant::now();
        let overlay = self
            .candidate_overlay_snapshot_from_documents(
                &root,
                semantic_generation,
                reach_graph.as_deref(),
                context.engine.indexed_files.as_deref().map(Vec::as_slice),
                documents,
            )
            .await;
        reach_us = reach_us.saturating_add(overlay_started.elapsed().as_micros());
        let source_position = SourcePosition {
            line: position.position.line,
            character: position.position.character,
        };
        let current_text = text;

        let result = tokio::task::spawn_blocking(
            move || -> Result<(Option<String>, SemanticRequestPerf)> {
                let query_started = std::time::Instant::now();
                let service = CandidateQueryService::new(
                    call_read_handle.as_deref(),
                    &overlay,
                    &current_rel,
                    reach_scope.as_deref(),
                    reach_graph.as_deref(),
                );
                let call_context = service.complete_call_context_at(source_position)?;
                let is_call_site = call_context.is_some();
                let origin_anchor = service.anchor_at(source_position)?;
                let callable_set = service.callable_candidates(&word, call_context)?;
                let mut perf = SemanticRequestPerf::from_callable_set(&callable_set);
                perf.reach_us = reach_us;
                let hydration_started = std::time::Instant::now();
                let mut hydration = HydrationStats::default();
                if !callable_set.anchors.is_empty() && (origin_anchor.is_some() || is_call_site) {
                    let presentations = query::hover_presentations(&callable_set.groups);
                    let source_paths = presentation_paths(&presentations);
                    let source_revisions = service.source_revisions(&source_paths)?;
                    perf.query_us = query_started.elapsed().as_micros();
                    perf.returned = presentations.len().min(query::HOVER_CANDIDATE_LIMIT);
                    let markdown = hover_markdown_for_callable_presentations(
                        &root,
                        &current_rel,
                        current_text.as_ref(),
                        &overlay,
                        &presentations,
                        callable_set.arity_mismatch_fallback,
                        &source_revisions,
                        &mut hydration,
                    );
                    perf.hydration_us = hydration_started.elapsed().as_micros();
                    perf.hydration_count = hydration.count;
                    perf.hydration_bytes = hydration.bytes;
                    return Ok((markdown, perf));
                }

                let type_candidates = service.type_candidates(&word)?;
                perf.include_type_candidates(&type_candidates);
                if !type_candidates.aliases.candidates.is_empty()
                    || !type_candidates.records.candidates.is_empty()
                {
                    perf.query_us = query_started.elapsed().as_micros();
                    let markdown = hover_markdown_for_type_candidates(
                        &root,
                        &current_rel,
                        current_text.as_ref(),
                        &overlay,
                        &type_candidates,
                        &mut hydration,
                    );
                    perf.returned = type_candidates
                        .aliases
                        .candidates
                        .len()
                        .saturating_add(type_candidates.records.candidates.len())
                        .min(query::HOVER_CANDIDATE_LIMIT);
                    perf.hydration_us = hydration_started.elapsed().as_micros();
                    perf.hydration_count = hydration.count;
                    perf.hydration_bytes = hydration.bytes;
                    return Ok((markdown, perf));
                }
                if !callable_set.anchors.is_empty() {
                    let presentations = query::hover_presentations(&callable_set.groups);
                    let source_paths = presentation_paths(&presentations);
                    let source_revisions = service.source_revisions(&source_paths)?;
                    perf.query_us = query_started.elapsed().as_micros();
                    perf.returned = presentations.len().min(query::HOVER_CANDIDATE_LIMIT);
                    let markdown = hover_markdown_for_callable_presentations(
                        &root,
                        &current_rel,
                        current_text.as_ref(),
                        &overlay,
                        &presentations,
                        callable_set.arity_mismatch_fallback,
                        &source_revisions,
                        &mut hydration,
                    );
                    perf.hydration_us = hydration_started.elapsed().as_micros();
                    perf.hydration_count = hydration.count;
                    perf.hydration_bytes = hydration.bytes;
                    return Ok((markdown, perf));
                }

                // Types, macros, enum constants and variables intentionally keep
                // the generic symbol path. Functions never fall back to it.
                let non_callable_symbols = service.non_callable_symbols(&word)?;
                let source_paths: Vec<_> = non_callable_symbols
                    .iter()
                    .map(|candidate| candidate.path.clone())
                    .collect();
                let non_callable_count = non_callable_symbols.len();
                let source_revisions = service.source_revisions(&source_paths)?;
                let documentation_ranked = query::rank_hover_candidates(
                    non_callable_symbols,
                    &current_rel,
                    service.effective_current_reach(),
                    32,
                );
                perf.include_non_callable_candidates(non_callable_count);
                perf.query_us = query_started.elapsed().as_micros();
                let candidates: Vec<_> = documentation_ranked
                    .iter()
                    .take(query::HOVER_CANDIDATE_LIMIT)
                    .cloned()
                    .collect();
                perf.returned = candidates.len();
                let markdown = hover_markdown_for_candidates_with_project(
                    &root,
                    &current_rel,
                    current_text.as_ref(),
                    &candidates,
                    project_context.as_deref(),
                    Some(&documentation_ranked),
                    Some(&overlay),
                    &source_revisions,
                    Some(&mut hydration),
                );
                perf.hydration_us = hydration_started.elapsed().as_micros();
                perf.hydration_count = hydration.count;
                perf.hydration_bytes = hydration.bytes;
                Ok((markdown, perf))
            },
        )
        .await;

        let metrics = result
            .as_ref()
            .ok()
            .and_then(|result| result.as_ref().ok().map(|(_, metrics)| *metrics))
            .unwrap_or_default();
        self.perf_log(|| metrics.log_line("hover", total_started.elapsed().as_micros()))
            .await;

        match self.unwrap_query("hover", result).await {
            Some((Some(value), _)) => Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value,
                }),
                range: None,
            })),
            _ => Ok(None),
        }
    }
}

fn hover_markdown_for_type_candidates(
    root: &Path,
    current_rel: &str,
    current_text: &str,
    overlay: &crate::candidate_service::CandidateOverlaySnapshot,
    bundle: &crate::candidate_service::TypeCandidateBundle,
    hydration: &mut HydrationStats,
) -> Option<String> {
    let mut sections = Vec::new();
    for resolution in bundle
        .alias_resolutions
        .iter()
        .take(query::HOVER_CANDIDATE_LIMIT)
    {
        let alias = &resolution.alias;
        let (declaration, omission) = read_type_excerpt(
            root,
            current_rel,
            current_text,
            overlay,
            TypeExcerptIdentity {
                path: &alias.path,
                range: alias.declaration_range,
                declaration_hash: alias.declaration_hash,
                revision: alias.revision.as_ref(),
            },
        );
        hydration.record(declaration.as_deref());
        let mut section = String::new();
        section.push_str(&format!("### typedef `{}`\n\n", alias.alias));
        if let Some(comment) = type_candidate_comment(
            root,
            current_rel,
            current_text,
            overlay,
            &alias.path,
            &alias.alias,
            alias.name_range,
            alias.declaration_range.start.line,
            alias.revision.as_ref(),
            hydration,
        ) {
            section.push_str(comment.markdown.trim_end());
            section.push_str("\n\n");
        }
        if let Some(declaration) = declaration {
            push_source_code(&mut section, &alias.path, &declaration);
        } else {
            section.push_str(&format!(
                "```c\n// In {}\ntypedef … {};\n```\n",
                alias.path, alias.alias
            ));
            if let Some(reason) = omission {
                section.push_str(&format!("\n_Definition omitted: {reason}._\n"));
            }
        }
        if resolution.status == query::AliasResolutionStatus::UniqueRecord {
            if let Some(aka) = &resolution.aka_spelling {
                section.push_str(&format!("\n`(aka. {})`\n", sanitize_inline(aka)));
            }
        }
        let (confidence, reason) = crate::resolver::confidence_reason_for(alias.tier, true, None);
        section.push_str(&format!(
            "\n<small><em>path: {} | tier: {} | confidence: {} | reason: {} | alias resolution: {}</em></small>",
            sanitize_inline(&alias.path),
            alias.tier.as_str(),
            confidence.as_str(),
            reason.as_str(),
            alias_status_label(resolution.status)
        ));
        sections.push(section);

        for record in resolution
            .terminal_records
            .iter()
            .take(query::HOVER_CANDIDATE_LIMIT.saturating_sub(sections.len()))
        {
            sections.push(record_hover_section(
                root,
                current_rel,
                current_text,
                overlay,
                record,
                hydration,
            ));
        }
        if sections.len() >= query::HOVER_CANDIDATE_LIMIT {
            break;
        }
    }

    if sections.len() < query::HOVER_CANDIDATE_LIMIT {
        for record in bundle
            .records
            .candidates
            .iter()
            .filter(|record| {
                !bundle.alias_resolutions.iter().any(|resolution| {
                    resolution
                        .terminal_records
                        .iter()
                        .any(|terminal| terminal.identity == record.identity)
                })
            })
            .take(query::HOVER_CANDIDATE_LIMIT - sections.len())
        {
            sections.push(record_hover_section(
                root,
                current_rel,
                current_text,
                overlay,
                record,
                hydration,
            ));
        }
    }
    (!sections.is_empty()).then(|| sections.join("\n\n---\n\n"))
}

fn record_hover_section(
    root: &Path,
    current_rel: &str,
    current_text: &str,
    overlay: &crate::candidate_service::CandidateOverlaySnapshot,
    record: &query::RecordCandidate,
    hydration: &mut HydrationStats,
) -> String {
    let (definition, omission) = read_type_excerpt(
        root,
        current_rel,
        current_text,
        overlay,
        TypeExcerptIdentity {
            path: &record.path,
            range: record.declaration_range,
            declaration_hash: record.declaration_hash,
            revision: record.revision.as_ref(),
        },
    );
    hydration.record(definition.as_deref());
    let kind = match record.kind {
        crate::semantic_model::RecordKind::Struct => "struct",
        crate::semantic_model::RecordKind::Union => "union",
        crate::semantic_model::RecordKind::Class => "class",
    };
    let mut section = format!("### {kind} `{}`\n\n", record.display_name);
    if let Some(comment) = type_candidate_comment(
        root,
        current_rel,
        current_text,
        overlay,
        &record.path,
        &record.display_name,
        record.name_range,
        record.declaration_range.start.line,
        record.revision.as_ref(),
        hydration,
    ) {
        section.push_str(comment.markdown.trim_end());
        section.push_str("\n\n");
    }
    if let Some(definition) = definition {
        push_source_code(&mut section, &record.path, &definition);
    } else {
        push_source_code(&mut section, &record.path, &record.signature);
        if let Some(reason) = omission {
            section.push_str(&format!("\n_Definition omitted: {reason}._\n"));
        }
    }
    let (confidence, reason) = crate::resolver::confidence_reason_for(record.tier, true, None);
    section.push_str(&format!(
        "\n<small><em>path: {} | tier: {} | confidence: {} | reason: {} | fact confidence: {} | range: {}</em></small>",
        sanitize_inline(&record.path),
        record.tier.as_str(),
        confidence.as_str(),
        reason.as_str(),
        record_confidence_label(record.confidence),
        match record.range_fidelity {
            crate::semantic_model::RecordRangeFidelity::AstExact => "ast_exact",
            crate::semantic_model::RecordRangeFidelity::Malformed => "malformed",
        }
    ));
    section
}

#[allow(clippy::too_many_arguments)]
fn type_candidate_comment(
    root: &Path,
    current_rel: &str,
    current_text: &str,
    overlay: &crate::candidate_service::CandidateOverlaySnapshot,
    candidate_path: &str,
    name: &str,
    name_range: crate::call_model::SourceRange,
    declaration_start_line: u32,
    revision: Option<&query::CandidateRevision>,
    hydration: &mut HydrationStats,
) -> Option<query::RenderedSymbolComment> {
    let source_kind = if Path::new(candidate_path).is_absolute() {
        "external"
    } else {
        "workspace"
    };
    let source = candidate_source_text_for_path_with_overlay_at_revision(
        root,
        current_rel,
        current_text,
        overlay,
        candidate_path,
        source_kind,
        revision,
    )?;
    hydration.record(Some(&source));
    let range = crate::model::CandidateRange {
        start_line: name_range.start.line,
        start_col: name_range.start.character,
        end_line: name_range.end.line,
        end_col: name_range.end.character,
    };
    query::comment_documentation_for_candidate_symbol(&source, name, name_range.start.line, &range)
        .or_else(|| {
            query::comment_documentation_for_candidate_symbol(
                &source,
                name,
                declaration_start_line,
                &range,
            )
        })
}

#[derive(Debug, Clone, Copy)]
struct TypeExcerptIdentity<'a> {
    path: &'a str,
    range: crate::call_model::SourceRange,
    declaration_hash: [u8; 32],
    revision: Option<&'a query::CandidateRevision>,
}

fn read_type_excerpt(
    root: &Path,
    current_rel: &str,
    current_text: &str,
    overlay: &crate::candidate_service::CandidateOverlaySnapshot,
    identity: TypeExcerptIdentity<'_>,
) -> (Option<String>, Option<String>) {
    let reader: query::SourceExcerptReader = Default::default();
    let byte_range = query::SourceExcerptRange {
        start: identity.range.start_byte,
        end: identity.range.end_byte,
    };
    let outcome = if let Some(source) = overlay.source_text(identity.path) {
        reader.read_buffer(source, byte_range)
    } else if identity.path == current_rel {
        reader.read_buffer(current_text, byte_range)
    } else if let Some(revision) = identity.revision {
        let path = bounded_candidate_source_path(
            root,
            identity.path,
            Path::new(identity.path).is_absolute(),
        );
        reader.read_file(
            &path,
            byte_range,
            query::SourceExcerptRevision {
                size: revision.size,
                mtime_ns: revision.mtime_ns,
                excerpt_hash: identity.declaration_hash,
            },
        )
    } else {
        return (None, Some("source revision is unavailable".into()));
    };
    match outcome {
        query::SourceExcerptOutcome::Complete { text, .. } => (Some(text), None),
        query::SourceExcerptOutcome::Omitted(reason) => (None, Some(reason.as_str().to_string())),
    }
}

fn push_source_code(out: &mut String, path: &str, source: &str) {
    out.push_str("```c\n");
    out.push_str(&format!("// In {}\n", path));
    out.push_str(&source.replace("```", "'''"));
    if !source.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("```\n");
}

fn bounded_candidate_source_path(root: &Path, path: &str, absolute: bool) -> PathBuf {
    if absolute {
        return PathBuf::from(path);
    }
    path.split('/').filter(|segment| !segment.is_empty()).fold(
        root.to_path_buf(),
        |mut output, segment| {
            output.push(segment);
            output
        },
    )
}

fn alias_status_label(status: query::AliasResolutionStatus) -> &'static str {
    match status {
        query::AliasResolutionStatus::UniqueRecord => "unique_record",
        query::AliasResolutionStatus::AmbiguousRecord => "ambiguous_record",
        query::AliasResolutionStatus::Unresolved => "unresolved",
        query::AliasResolutionStatus::Cycle => "cycle",
        query::AliasResolutionStatus::UnsupportedDeclarator => "unsupported_declarator",
        query::AliasResolutionStatus::Truncated => "incomplete",
    }
}

fn record_confidence_label(confidence: crate::semantic_model::RecordConfidence) -> &'static str {
    match confidence {
        crate::semantic_model::RecordConfidence::NamedTag => "named_tag",
        crate::semantic_model::RecordConfidence::AnonymousTypedef => "anonymous_typedef",
        crate::semantic_model::RecordConfidence::Heuristic => "heuristic",
    }
}

fn sanitize_inline(value: &str) -> String {
    value.replace('`', "\\`")
}

#[allow(clippy::too_many_arguments)] // Keeps revision/overlay evidence explicit at hydration boundary.
fn hover_markdown_for_callable_presentations(
    root: &Path,
    current_rel: &str,
    current_text: &str,
    overlay: &crate::candidate_service::CandidateOverlaySnapshot,
    candidates: &[&query::ResolvedCallableAnchor],
    arity_mismatch_fallback: bool,
    revisions: &std::collections::HashMap<String, query::CandidateRevision>,
    hydration: &mut HydrationStats,
) -> Option<String> {
    let mut sections = Vec::new();
    for candidate in candidates.iter().take(query::HOVER_CANDIDATE_LIMIT) {
        let signature = if candidate.anchor.presentation_signature.trim().is_empty() {
            candidate.anchor.signature.normalized.clone()
        } else {
            candidate.anchor.presentation_signature.clone()
        };
        let source = candidate_source_text_for_path_with_overlay_at_revision(
            root,
            current_rel,
            current_text,
            overlay,
            &candidate.candidate.path,
            &candidate.candidate.source,
            revisions.get(&candidate.candidate.path),
        );
        hydration.record(source.as_deref());
        let comment = source.as_deref().and_then(|source| {
            query::comment_documentation_for_candidate_symbol(
                source,
                &candidate.candidate.name,
                candidate.candidate.range.start_line,
                &candidate.candidate.range,
            )
        });
        let display = query::RankedHoverCandidate {
            candidate: candidate.candidate.clone(),
            signature,
            guard: candidate.anchor.guard.clone(),
        };
        let mut rendered = query::hover_markdown_for_candidate(
            &display,
            comment.as_ref().map(|comment| comment.markdown.as_str()),
        );
        if candidate.anchor.signature_fidelity
            == crate::call_model::SignatureFidelity::LexicalFallback
        {
            rendered.push_str(
                "\n_Low-fidelity lexical fallback: callable AST facts were unavailable._\n",
            );
        }
        sections.push(rendered);
    }
    if sections.is_empty() {
        return None;
    }
    let joined = sections.join("\n\n---\n\n");
    if arity_mismatch_fallback {
        Some(format!(
            "> **Arity mismatch fallback:** no callable candidate matched the available argument-count evidence; showing conservative navigation candidates.\n\n{joined}"
        ))
    } else {
        Some(joined)
    }
}

fn presentation_paths(candidates: &[&query::ResolvedCallableAnchor]) -> Vec<String> {
    let mut paths: Vec<_> = candidates
        .iter()
        .map(|candidate| candidate.candidate.path.clone())
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

#[cfg(test)]
pub(super) fn hover_markdown_for_candidates(
    root: &Path,
    current_rel: &str,
    current_text: &str,
    candidates: &[query::RankedHoverCandidate],
) -> Option<String> {
    hover_markdown_for_candidates_with_project(
        root,
        current_rel,
        current_text,
        candidates,
        None,
        None,
        None,
        &std::collections::HashMap::new(),
        None,
    )
}

#[allow(clippy::too_many_arguments)] // Keeps ranking inputs separate from source hydration evidence.
fn hover_markdown_for_candidates_with_project(
    root: &Path,
    current_rel: &str,
    current_text: &str,
    candidates: &[query::RankedHoverCandidate],
    project_context: Option<&crate::project_context::ProjectContextIndex>,
    documentation_ranked: Option<&[query::RankedHoverCandidate]>,
    overlay: Option<&crate::candidate_service::CandidateOverlaySnapshot>,
    revisions: &std::collections::HashMap<String, query::CandidateRevision>,
    mut hydration: Option<&mut HydrationStats>,
) -> Option<String> {
    if candidates.is_empty() {
        return None;
    }
    let documentation_candidates: Vec<_> = documentation_ranked
        .unwrap_or(candidates)
        .iter()
        .map(|candidate| query::DocumentationCandidate {
            candidate: candidate.candidate.clone(),
            signature: candidate.signature.clone(),
        })
        .collect();
    let mut seen = HashSet::new();
    let mut sections = Vec::new();
    for candidate in candidates {
        let primary = query::DocumentationCandidate {
            candidate: candidate.candidate.clone(),
            signature: candidate.signature.clone(),
        };
        let preferred = super::completion_documentation::preferred_symbol_documentation(
            root,
            current_rel,
            current_text,
            &primary,
            &documentation_candidates,
            project_context,
            overlay,
            revisions,
            hydration.as_deref_mut(),
        );
        let presentation = &preferred.presentation;
        let key = (
            presentation.candidate.path.clone(),
            presentation.candidate.range.start_line,
            presentation.signature.clone(),
        );
        if !seen.insert(key) {
            continue;
        }
        let guard = documentation_ranked
            .unwrap_or(candidates)
            .iter()
            .find(|candidate| {
                candidate.candidate.path == presentation.candidate.path
                    && candidate.candidate.range == presentation.candidate.range
            })
            .and_then(|candidate| candidate.guard.clone());
        let display = query::RankedHoverCandidate {
            candidate: presentation.candidate.clone(),
            signature: presentation.signature.clone(),
            guard,
        };
        let comment = preferred.comment.map(|comment| comment.markdown);
        sections.push(query::hover_markdown_for_candidate(
            &display,
            comment.as_deref(),
        ));
    }
    Some(sections.join("\n\n---\n\n"))
}

pub(super) fn candidate_source_text_for_path(
    root: &Path,
    current_rel: &str,
    current_text: &str,
    candidate_path: &str,
    source: &str,
) -> Option<String> {
    if candidate_path == current_rel {
        if current_text.len() as u64 > HOVER_SOURCE_FILE_BYTE_LIMIT {
            return None;
        }
        return Some(current_text.to_string());
    }
    let path = candidate_source_path(root, candidate_path, source);
    let metadata = std::fs::metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() > HOVER_SOURCE_FILE_BYTE_LIMIT {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn candidate_source_text_for_path_with_overlay_at_revision(
    root: &Path,
    current_rel: &str,
    current_text: &str,
    overlay: &crate::candidate_service::CandidateOverlaySnapshot,
    candidate_path: &str,
    source: &str,
    revision: Option<&query::CandidateRevision>,
) -> Option<String> {
    if let Some(text) = overlay.source_text(candidate_path) {
        if text.len() as u64 > HOVER_SOURCE_FILE_BYTE_LIMIT {
            return None;
        }
        return Some(text.to_string());
    }
    candidate_source_text_for_path_at_revision(
        root,
        current_rel,
        current_text,
        candidate_path,
        source,
        revision,
    )
}

pub(super) fn candidate_source_text_for_path_at_revision(
    root: &Path,
    current_rel: &str,
    current_text: &str,
    candidate_path: &str,
    source: &str,
    revision: Option<&query::CandidateRevision>,
) -> Option<String> {
    if candidate_path == current_rel {
        if current_text.len() as u64 > HOVER_SOURCE_FILE_BYTE_LIMIT {
            return None;
        }
        return Some(current_text.to_string());
    }
    let revision = revision?;
    let path = candidate_source_path(root, candidate_path, source);
    let metadata_before = std::fs::metadata(&path).ok()?;
    if !metadata_before.is_file()
        || metadata_before.len() != revision.size
        || metadata_before.len() > HOVER_SOURCE_FILE_BYTE_LIMIT
    {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    let metadata_after = std::fs::metadata(&path).ok()?;
    if metadata_before.len() != metadata_after.len()
        || metadata_before.modified().ok() != metadata_after.modified().ok()
        || bytes.len() as u64 != revision.size
        || blake3::hash(&bytes).to_hex().as_str() != revision.hash.as_str()
    {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn candidate_source_path(root: &Path, path: &str, source: &str) -> PathBuf {
    if source == "external" {
        return PathBuf::from(path);
    }
    let mut out = root.to_path_buf();
    for segment in path.split('/') {
        if !segment.is_empty() {
            out.push(segment);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(path: &str, line: u32, signature: &str) -> query::RankedHoverCandidate {
        candidate_named("foo", path, line, signature)
    }

    fn candidate_named(
        name: &str,
        path: &str,
        line: u32,
        signature: &str,
    ) -> query::RankedHoverCandidate {
        let (confidence, reason) =
            crate::resolver::confidence_reason_for(crate::model::ScopeTier::Current, true, None);
        query::RankedHoverCandidate {
            signature: signature.to_string(),
            guard: None,
            candidate: crate::model::DefinitionCandidate {
                name: name.to_string(),
                kind: "function".to_string(),
                role: "definition".to_string(),
                path: path.to_string(),
                range: crate::model::CandidateRange {
                    start_line: line,
                    start_col: 0,
                    end_line: line,
                    end_col: 0,
                },
                source: "workspace".to_string(),
                tier: crate::model::ScopeTier::Current,
                base_match: 1000,
                confidence,
                reason,
            },
        }
    }

    #[test]
    fn hover_markdown_for_candidates_uses_current_document_comments() {
        let source = "/// @brief Current buffer docs\nint foo(void);\n";
        let markdown = hover_markdown_for_candidates(
            Path::new("F:/repo"),
            "src/main.c",
            source,
            &[candidate("src/main.c", 1, "int foo(void);")],
        )
        .expect("hover markdown");
        assert!(markdown.contains("Current buffer docs"));
        assert!(markdown.contains("```c\n// In src/main.c\nint foo(void);\n```"));
        assert!(markdown.contains("tier: current"));
    }

    #[test]
    fn hover_markdown_for_candidates_recovers_trailing_comments() {
        let source = "int foo(void); // Helps from trailing comment\n";
        let markdown = hover_markdown_for_candidates(
            Path::new("F:/repo"),
            "src/main.c",
            source,
            &[candidate("src/main.c", 0, "int foo(void);")],
        )
        .expect("hover markdown");
        assert!(markdown.contains("Helps from trailing comment"));
        assert!(markdown.contains("```c\n// In src/main.c\nint foo(void);\n```"));
    }

    #[test]
    fn generic_hover_never_pairs_functions_from_project_membership_alone() {
        let root = unique_temp_root("header-doc-preference");
        let lib = root.join("lib");
        std::fs::create_dir_all(&lib).expect("create lib");
        std::fs::write(
            lib.join("ops_chain.h"),
            "/// Header API documentation.\nint foo(int value);\n",
        )
        .expect("write header");
        let current = "int foo(int value) { return value; }\n";
        let source_candidate = candidate_named("foo", "lib/ops_chain.c", 0, "int foo(int value)");
        let mut header_candidate =
            candidate_named("foo", "lib/ops_chain.h", 1, "int foo(int value);");
        header_candidate.candidate.role = "declaration".to_string();
        header_candidate.candidate.tier = crate::model::ScopeTier::Reachable;
        let project_key = crate::project_context::ProjectKey {
            workspace_root_id: "workspace".to_string(),
            project_path: "lib".to_string(),
        };
        let projects = crate::project_context::ProjectContextIndex::new(
            "workspace".to_string(),
            "test".to_string(),
            vec![crate::project_context::ProjectContext {
                key: project_key,
                workspace_name: "lib".to_string(),
                marker_files: vec!["lib/CMakeLists.txt".to_string()],
            }],
        );

        let markdown = hover_markdown_for_candidates_with_project(
            &root,
            "lib/ops_chain.c",
            current,
            &[source_candidate, header_candidate],
            Some(&projects),
            None,
            None,
            &std::collections::HashMap::new(),
            None,
        )
        .expect("hover");
        let first = markdown
            .split("\n\n---\n\n")
            .next()
            .expect("first candidate");
        let _ = std::fs::remove_dir_all(&root);
        assert!(!first.contains("Header API documentation."));
        assert!(first.contains("// In lib/ops_chain.c"));
        assert!(first.contains("int foo(int value)"));
    }

    #[test]
    fn hover_markdown_for_candidates_recovers_trailing_in_multiline_buffer() {
        let source = "#define VALUE 1\n/// @brief Helps the smoke test.\n/// <param name=\"unused\">structured param</param>\nvoid helper(void);\nint trailing_docs(void); // trailing hover comment\n";
        let markdown = hover_markdown_for_candidates(
            Path::new("F:/repo"),
            "defs.h",
            source,
            &[candidate_named(
                "trailing_docs",
                "defs.h",
                4,
                "int trailing_docs(void);",
            )],
        )
        .expect("hover markdown");
        assert!(
            markdown.contains("trailing hover comment"),
            "missing trailing comment in {markdown}"
        );
    }

    #[test]
    fn hover_markdown_for_candidates_renders_structured_xml_param() {
        let source = "/// <param name=\"size\">cache size</param>\nint foo(int size);\n";
        let markdown = hover_markdown_for_candidates(
            Path::new("F:/repo"),
            "src/main.c",
            source,
            &[candidate("src/main.c", 1, "int foo(int size);")],
        )
        .expect("hover markdown");
        assert!(markdown.contains("### Parameters"));
        assert!(markdown.contains("- `size` — cache size"));
    }

    #[test]
    fn hover_markdown_for_candidates_keeps_signature_when_file_unreadable() {
        let markdown = hover_markdown_for_candidates(
            Path::new("F:/repo"),
            "src/main.c",
            "",
            &[candidate("include/missing.h", 9, "int foo(int x);")],
        )
        .expect("hover markdown");
        assert!(markdown.contains("int foo(int x);"));
        assert!(!markdown.contains("Parameters"));
    }

    #[test]
    fn hover_markdown_for_candidates_skips_oversized_candidate_source_files() {
        let root = unique_temp_root("huge-hover-source");
        let include_dir = root.join("include");
        std::fs::create_dir_all(&include_dir).expect("create temp include dir");

        let filler_lines = 30_000usize;
        let filler = "int filler;\n".repeat(filler_lines);
        let source = format!("{filler}/// Huge docs that should not be read\nint foo(void);\n");
        std::fs::write(include_dir.join("huge.h"), source).expect("write huge source");

        let markdown = hover_markdown_for_candidates(
            &root,
            "src/main.c",
            "",
            &[candidate(
                "include/huge.h",
                filler_lines as u32 + 1,
                "int foo(void);",
            )],
        )
        .expect("hover markdown");

        let _ = std::fs::remove_dir_all(&root);
        assert!(markdown.contains("int foo(void);"));
        assert!(!markdown.contains("Huge docs that should not be read"));
    }

    #[test]
    fn hover_markdown_for_candidates_skips_oversized_current_buffer_comments() {
        let filler_lines = 30_000usize;
        let filler = "int filler;\n".repeat(filler_lines);
        let source =
            format!("{filler}/// Huge current docs that should not be read\nint foo(void);\n");
        assert!(source.len() as u64 > HOVER_SOURCE_FILE_BYTE_LIMIT);

        let markdown = hover_markdown_for_candidates(
            Path::new("F:/repo"),
            "src/main.c",
            &source,
            &[candidate(
                "src/main.c",
                filler_lines as u32 + 1,
                "int foo(void);",
            )],
        )
        .expect("hover markdown");

        assert!(markdown.contains("int foo(void);"));
        assert!(!markdown.contains("Huge current docs that should not be read"));
    }

    #[test]
    fn hover_markdown_for_candidates_returns_none_for_empty_candidates() {
        assert!(
            hover_markdown_for_candidates(Path::new("F:/repo"), "src/main.c", "", &[]).is_none()
        );
    }

    #[test]
    fn type_hover_keeps_alias_and_record_comments_with_scope_evidence() {
        let source = "/** Packet wire-format documentation. */\ntypedef struct Packet {\n    int id;\n} PacketT;\n";
        let parsed = crate::parser::parse_with_handle(
            Path::new("include/packet.h"),
            source,
            None,
            crate::parser::ParseFacts::HOVER_SEMANTICS,
        );
        let overlay = crate::candidate_service::CandidateOverlaySnapshot::new(
            1,
            vec![
                crate::candidate_service::FileCandidateOverlay::from_index_with_text(
                    "include/packet.h".into(),
                    &parsed,
                    Arc::from(source),
                ),
            ],
        );
        let service = crate::candidate_service::CandidateQueryService::new(
            None,
            &overlay,
            "include/packet.h",
            None,
            None,
        );
        let bundle = service.type_candidates("PacketT").expect("type candidates");
        let mut hydration = HydrationStats::default();
        let markdown = hover_markdown_for_type_candidates(
            Path::new("F:/repo"),
            "include/packet.h",
            source,
            &overlay,
            &bundle,
            &mut hydration,
        )
        .expect("type hover");

        assert!(markdown.contains("Packet wire-format documentation."));
        assert!(markdown.contains("path: include/packet.h"));
        assert!(markdown.contains("tier: current"));
        assert!(markdown.contains("confidence: exact"));
        assert!(markdown.contains("reason: current_file"));
        assert!(markdown.contains("fact confidence: named_tag"));
        assert!(hydration.count > 0);
        assert!(hydration.bytes >= source.len());
    }

    fn unique_temp_root(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("fossilsense-{name}-{}-{nanos}", std::process::id()))
    }
}
