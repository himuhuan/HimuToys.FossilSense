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

mod source;
pub(super) use source::{
    candidate_source_text_for_path, candidate_source_text_for_path_at_revision,
    candidate_source_text_for_path_with_overlay_at_revision,
};

pub(super) const HOVER_SOURCE_FILE_BYTE_LIMIT: u64 = 256 * 1024;

impl Backend {
    pub(super) async fn provide_hover(&self, params: HoverParams) -> LspResult<Option<Hover>> {
        let total_started = std::time::Instant::now();
        let position = params.text_document_position_params;
        let uri = position.text_document.uri;
        let documents = self
            .session
            .documents
            .capture_request_snapshot(Some(&uri))
            .await;
        let Some((version, text)) = self.document_snapshot_from_request(&uri, &documents).await
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
        let context = self.request_context_for_root(root.clone()).await;
        let current_abs = uri_to_path(&uri);
        let current_rel = current_abs
            .as_deref()
            .and_then(|path| pathing::relative_slash_path(&root, path).ok())
            .unwrap_or_default();
        let source_language = context.engine.workspace_semantics.language_for_uri(&uri);
        let semantic_family = source_language.semantic_family();
        let cursor_byte =
            query::byte_offset_at(&text, position.position.line, position.position.character);

        // C and C++ labels inhabit a function-local namespace. Hover shares
        // navigation's label proof instead of describing workspace symbols
        // that merely share the spelling.
        if super::navigation::label_navigation_syntax_hint(&text, &word, cursor_byte) {
            let label_path = current_rel.clone();
            let label_text = text.clone();
            let label_word = word.clone();
            let label_uri = uri.clone();
            match tokio::task::spawn_blocking(move || {
                super::navigation::label_navigation_location(
                    &label_uri,
                    &label_path,
                    &label_text,
                    &label_word,
                    cursor_byte,
                    source_language,
                )
            })
            .await
            {
                Ok(super::navigation::LabelNavigation::Found(location)) => {
                    let total_us = total_started.elapsed().as_micros();
                    self.perf_log(|| SemanticRequestPerf::default().log_line("hover", total_us))
                        .await;
                    return Ok(Some(markdown_hover(label_hover_markdown(
                        &current_rel,
                        &text,
                        &location,
                    ))));
                }
                // A proven `goto name` resolves only in the enclosing
                // function's label namespace; a missing label must not surface
                // unrelated workspace candidates named `name`.
                Ok(super::navigation::LabelNavigation::MissingDefinition) => {
                    let total_us = total_started.elapsed().as_micros();
                    self.perf_log(|| SemanticRequestPerf::default().log_line("hover", total_us))
                        .await;
                    return Ok(None);
                }
                Ok(super::navigation::LabelNavigation::NotLabelSyntax) | Err(_) => {}
            }
        }

        // Lexical bindings are proven by C scope rules and dominate every
        // workspace same-name candidate, exactly as in navigation and possible
        // targets.
        if super::navigation::ordinary_identifier_navigation_context(
            line_text,
            position.position.character,
        ) {
            if let Some(current_abs) = current_abs.as_deref() {
                if let Some(parsed) = self
                    .get_or_parse_document_with_language(
                        &uri,
                        current_abs,
                        version,
                        &text,
                        crate::parser::ParseFacts::LOCAL_DECLS,
                        source_language,
                    )
                    .await
                {
                    if let Some(binding) =
                        query::visible_local_binding(&parsed.local_bindings, &word, cursor_byte)
                    {
                        let total_us = total_started.elapsed().as_micros();
                        self.perf_log(|| {
                            SemanticRequestPerf::default().log_line("hover", total_us)
                        })
                        .await;
                        return Ok(Some(markdown_hover(local_binding_hover_markdown(
                            &current_rel,
                            &text,
                            binding,
                        ))));
                    }
                }
            }
        }

        let reach_started = std::time::Instant::now();
        let reach_scope = self
            .reach_scope_from_context(&uri, &context)
            .map(|(_, reach)| reach);
        let mut reach_us = reach_started.elapsed().as_micros();
        let project_context = context.engine.project_context.clone();
        let protobuf_c_enabled = context.engine.workspace_semantics.protobuf_c_enabled();
        let semantic_generation = context.engine.semantic_generation;
        let call_read_handle = context.engine.call_read_handle.clone();
        let declaration_index = context.engine.declaration_index.clone();
        let reach_graph = context.engine.reach_graph.clone();
        let overlay_started = std::time::Instant::now();
        let overlay = self
            .candidate_overlay_snapshot_from_documents(
                &root,
                semantic_generation,
                reach_graph.as_deref(),
                context.engine.indexed_files.as_deref().map(Vec::as_slice),
                context.engine.workspace_semantics.clone(),
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
                let service = CandidateQueryService::new_with_declarations_for_family(
                    call_read_handle.as_deref(),
                    declaration_index.as_deref(),
                    &overlay,
                    &current_rel,
                    reach_scope.as_deref(),
                    reach_graph.as_deref(),
                    semantic_family,
                );
                let call_context = service.complete_call_context_at(source_position)?;
                let is_call_site = call_context.is_some();
                let origin_anchor = service.anchor_at(source_position)?;
                let semantic_set = service.semantic_candidates(
                    &word,
                    if is_call_site || origin_anchor.is_some() {
                        crate::candidate_service::SemanticIntent::Call
                    } else {
                        crate::candidate_service::SemanticIntent::Neutral
                    },
                )?;
                let semantic_count = semantic_set
                    .all
                    .iter()
                    .map(|group| group.candidates.len())
                    .sum();
                let callable_fingerprints =
                    crate::candidate_service::focused_callable_fingerprints(&semantic_set);
                let protobuf_c_sources = if protobuf_c_enabled
                    && crate::candidate_service::focused_has_kind(&semantic_set, |kind| {
                        matches!(
                            kind,
                            crate::semantic_model::SemanticDeclarationKind::Type
                                | crate::semantic_model::SemanticDeclarationKind::Alias
                        )
                    }) {
                    service.protobuf_c_sources_for_set(&semantic_set)?
                } else {
                    (Vec::new(), false)
                };
                let callable_set = if callable_fingerprints.is_empty() {
                    None
                } else {
                    Some(service.callable_candidates(&word, call_context)?)
                };
                let mut perf = callable_set
                    .as_ref()
                    .map(SemanticRequestPerf::from_callable_set)
                    .unwrap_or_default();
                perf.reach_us = reach_us;
                let hydration_started = std::time::Instant::now();
                let mut hydration = HydrationStats::default();
                if let Some(callable_set) = callable_set.as_ref().filter(|set| {
                    !set.anchors.is_empty() && (origin_anchor.is_some() || is_call_site)
                }) {
                    let presentations: Vec<_> = query::hover_presentations(&callable_set.groups)
                        .into_iter()
                        .filter(|candidate| {
                            callable_fingerprints
                                .contains(candidate.anchor.anchor_fingerprint.as_str())
                        })
                        .collect();
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
                    return Ok((with_candidate_set_evidence(markdown, &semantic_set), perf));
                }

                let type_candidates =
                    if crate::candidate_service::focused_has_kind(&semantic_set, |kind| {
                        matches!(
                            kind,
                            crate::semantic_model::SemanticDeclarationKind::Type
                                | crate::semantic_model::SemanticDeclarationKind::Alias
                        )
                    }) {
                        Some(service.type_candidates_for_set(&word, &semantic_set)?)
                    } else {
                        None
                    };
                if let Some(type_candidates) = type_candidates.as_ref() {
                    perf.include_type_candidates(type_candidates);
                    if !type_candidates.aliases.candidates.is_empty()
                        || !type_candidates.records.candidates.is_empty()
                    {
                        perf.query_us = query_started.elapsed().as_micros();
                        let markdown = hover_markdown_for_type_candidates(
                            &root,
                            &current_rel,
                            current_text.as_ref(),
                            &overlay,
                            type_candidates,
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
                        let markdown = with_candidate_set_evidence(markdown, &semantic_set);
                        return Ok((
                            append_protobuf_c_sources(
                                markdown,
                                &root,
                                &protobuf_c_sources.0,
                                protobuf_c_sources.1,
                            ),
                            perf,
                        ));
                    }
                }
                if let Some(callable_set) =
                    callable_set.as_ref().filter(|set| !set.anchors.is_empty())
                {
                    let presentations: Vec<_> = query::hover_presentations(&callable_set.groups)
                        .into_iter()
                        .filter(|candidate| {
                            callable_fingerprints
                                .contains(candidate.anchor.anchor_fingerprint.as_str())
                        })
                        .collect();
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
                    return Ok((with_candidate_set_evidence(markdown, &semantic_set), perf));
                }

                let documentation_ranked: Vec<_> =
                    crate::candidate_service::focused_candidates(&semantic_set)
                        .iter()
                        .map(|candidate| query::RankedHoverCandidate {
                            candidate: candidate.as_definition_candidate(),
                            signature: candidate
                                .fact
                                .canonical_signature
                                .clone()
                                .unwrap_or_else(|| candidate.fact.name.clone()),
                            guard: candidate.fact.guard.clone(),
                        })
                        .collect();
                let source_paths: Vec<_> = documentation_ranked
                    .iter()
                    .map(|candidate| candidate.candidate.path.clone())
                    .collect();
                let source_revisions = service.source_revisions(&source_paths)?;
                perf.include_non_callable_candidates(semantic_count);
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
                let markdown = with_candidate_set_evidence(markdown, &semantic_set);
                Ok((
                    append_protobuf_c_sources(
                        markdown,
                        &root,
                        &protobuf_c_sources.0,
                        protobuf_c_sources.1,
                    ),
                    perf,
                ))
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

fn markdown_hover(value: String) -> Hover {
    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: None,
    }
}

/// Hover for a lexically proven parameter/local: the binding's own declaration
/// line, never a workspace candidate that merely shares the spelling.
fn local_binding_hover_markdown(
    current_rel: &str,
    text: &str,
    binding: &crate::parser::LocalBinding,
) -> String {
    let declaration = source_line_at_byte(text, binding.decl_start_byte);
    let binding_kind = match binding.kind {
        crate::parser::LocalBindingKind::Parameter => "parameter",
        crate::parser::LocalBindingKind::LocalVariable => "local variable",
        crate::parser::LocalBindingKind::LocalConstant => "local constant",
        crate::parser::LocalBindingKind::LocalType => "local type",
    };
    let type_note = binding
        .type_text
        .as_deref()
        .map(str::trim)
        .filter(|type_text| !type_text.is_empty())
        .map(|type_text| format!(" | type: {}", escape_html_text(type_text)))
        .unwrap_or_default();
    let mut out = String::new();
    out.push_str("```c\n");
    out.push_str(&format!("// In {current_rel}\n"));
    out.push_str(&declaration.replace("```", "'''"));
    out.push_str("\n```\n\n");
    out.push_str(&format!(
        "<small><span style=\"color: var(--vscode-descriptionForeground);\"><em>{binding_kind}{type_note} | tier: current | confidence: exact | reason: lexical_binding</em></span></small>"
    ));
    out
}

/// Hover for a proven label definition/use inside the enclosing function.
fn label_hover_markdown(
    current_rel: &str,
    text: &str,
    location: &tower_lsp::lsp_types::Location,
) -> String {
    let declaration = text
        .lines()
        .nth(location.range.start.line as usize)
        .unwrap_or_default()
        .trim();
    let mut out = String::new();
    out.push_str("```c\n");
    out.push_str(&format!("// In {current_rel}\n"));
    out.push_str(&declaration.replace("```", "'''"));
    out.push_str("\n```\n\n");
    out.push_str(
        "<small><span style=\"color: var(--vscode-descriptionForeground);\"><em>label | tier: current | confidence: exact | reason: label_namespace</em></span></small>",
    );
    out
}

fn source_line_at_byte(text: &str, byte: usize) -> &str {
    let byte = byte.min(text.len());
    let start = text[..byte].rfind('\n').map_or(0, |index| index + 1);
    let end = text[byte..]
        .find('\n')
        .map_or(text.len(), |offset| byte + offset);
    text[start..end].trim()
}

/// Append set-level uncertainty evidence to a hover produced from the shared
/// semantic candidate set. Scope-tier focus may suppress recalled same-name
/// candidates from presentation; that suppression must stay visible instead of
/// silently narrowing the answer, and truncated bounded recall must never read
/// as a complete match list.
fn with_candidate_set_evidence(
    markdown: Option<String>,
    set: &crate::model::CandidateSet<crate::candidate_service::ResolvedDeclarationCandidate>,
) -> Option<String> {
    let markdown = markdown?;
    let mut notes = Vec::new();
    if set.alternative_count > 0 {
        notes.push(format!(
            "{} same-name candidate(s) outside the focused result — run \"FossilSense: Find All Possible Definitions / Declarations\" to inspect them",
            set.alternative_count
        ));
    }
    if set.coverage.truncated {
        notes.push("bounded exact-name recall was truncated; matches may be incomplete".into());
    }
    if notes.is_empty() {
        return Some(markdown);
    }
    Some(format!(
        "{markdown}\n\n<small><span style=\"color: var(--vscode-descriptionForeground);\"><em>matches: {} | {}</em></span></small>",
        set.disposition.as_str(),
        notes.join(" | ")
    ))
}

fn append_protobuf_c_sources(
    markdown: Option<String>,
    workspace_root: &Path,
    sources: &[crate::store::views::ProtobufCSourceReadRow],
    truncated: bool,
) -> Option<String> {
    let mut markdown = markdown?;
    if sources.is_empty() {
        if truncated {
            markdown.push_str(
                "\n\n---\n\n**proto 来源**\n\n结果已截断，可能来源未保留在本次受限查询中。\n",
            );
        }
        return Some(markdown);
    }

    markdown.push_str("\n\n---\n\n**proto 来源**\n\n");
    if sources.len() > 1 || truncated {
        markdown.push_str("存在多个可能来源；FossilSense 保留全部受限查询范围内的合理候选。\n\n");
    }
    for source in sources {
        let path = Path::new(&source.proto_path);
        let display_path = pathing::relative_slash_path(workspace_root, path)
            .unwrap_or_else(|_| source.proto_path.clone());
        let display_line = source.start_line.saturating_add(1);
        let mut uri = tower_lsp::lsp_types::Url::from_file_path(path).ok();
        if let Some(uri) = uri.as_mut() {
            uri.set_fragment(Some(&format!("L{display_line}")));
        }
        let label = escape_markdown_link_label(&format!("{display_path}:{display_line}"));
        let location = uri.map_or_else(|| format!("`{label}`"), |uri| format!("[{label}]({uri})"));
        let evidence = if source.match_kind == "relative_path" {
            "相对路径匹配"
        } else {
            "同名文件匹配（较低可信度）"
        };
        markdown.push_str(&format!(
            "- {location} — {} `{}`；匹配依据：{evidence}\n",
            source.kind,
            sanitize_inline(&source.proto_name),
        ));
    }
    if truncated {
        markdown.push_str("\n结果已截断，可能仍有其他 proto 来源。\n");
    }
    Some(markdown)
}

fn escape_markdown_link_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
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
        crate::semantic_model::RecordKind::Interface => "interface",
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

fn escape_html_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
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
        let rendered = query::hover_markdown_for_candidate(
            &display,
            comment.as_ref().map(|comment| comment.markdown.as_str()),
        );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protobuf_c_hover_keeps_external_sources_clickable_and_marks_uncertainty() {
        let workspace = unique_temp_root("proto-hover-workspace");
        let external = unique_temp_root("proto-hover-external");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(&external).expect("external");
        let proto = external.join("device.proto");
        std::fs::write(&proto, "package demo; message Device {}\n").expect("proto");
        let proto_path = crate::pathing::normalize_abs_path(&proto);
        let source = crate::store::views::ProtobufCSourceReadRow {
            proto_path: proto_path.clone(),
            proto_name: "demo.Device".to_string(),
            c_name: "Demo__Device".to_string(),
            kind: "message".to_string(),
            start_byte: 14,
            end_byte: 31,
            start_line: 0,
            start_col: 14,
            end_line: 0,
            end_col: 31,
            match_kind: "same_basename".to_string(),
        };

        let markdown = append_protobuf_c_sources(
            Some("generated declaration".to_string()),
            &workspace,
            std::slice::from_ref(&source),
            true,
        )
        .expect("markdown");
        let mut expected_uri =
            tower_lsp::lsp_types::Url::from_file_path(&proto).expect("proto uri");
        expected_uri.set_fragment(Some("L1"));

        let _ = std::fs::remove_dir_all(&workspace);
        let _ = std::fs::remove_dir_all(&external);
        assert!(markdown.contains(&proto_path), "{markdown}");
        assert!(markdown.contains(expected_uri.as_str()), "{markdown}");
        assert!(markdown.contains("多个可能来源"), "{markdown}");
        assert!(markdown.contains("较低可信度"), "{markdown}");
        assert!(markdown.contains("结果已截断"), "{markdown}");
    }

    #[test]
    fn protobuf_c_hover_reports_truncation_even_when_no_source_row_was_retained() {
        let markdown = append_protobuf_c_sources(
            Some("generated declaration".to_string()),
            Path::new("F:/repo"),
            &[],
            true,
        )
        .expect("markdown");

        assert!(markdown.contains("proto 来源"), "{markdown}");
        assert!(markdown.contains("结果已截断"), "{markdown}");
        assert!(markdown.contains("可能来源"), "{markdown}");
    }

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
    fn local_binding_hover_escapes_type_text_inside_html_metadata() {
        let source = "void run(void) {\n    std::vector<int> &values = input;\n}\n";
        let declaration_byte = source.find("values").expect("binding");
        let binding = crate::parser::LocalBinding {
            name: "values".into(),
            kind: crate::parser::LocalBindingKind::LocalVariable,
            type_text: Some("std::vector<int> &".into()),
            decl_start_byte: declaration_byte,
            function_start_byte: 0,
            function_end_byte: source.len(),
            scope_start_byte: source.find('{').expect("scope"),
            scope_end_byte: source.rfind('}').expect("scope"),
        };

        let markdown = local_binding_hover_markdown("main.cpp", source, &binding);

        assert!(
            markdown.contains("std::vector<int> &values = input;"),
            "source excerpts stay verbatim inside their fenced code block: {markdown}"
        );
        assert!(
            markdown.contains("type: std::vector&lt;int&gt; &amp;"),
            "metadata embedded in raw HTML must be escaped as text: {markdown}"
        );
        assert!(
            !markdown.contains("type: std::vector<int> & |"),
            "raw source-controlled type text must not become HTML: {markdown}"
        );
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
