use anyhow::Result;
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::{
    Documentation, MarkupContent, MarkupKind, ParameterInformation, ParameterLabel, SignatureHelp,
    SignatureHelpParams, SignatureInformation,
};

use super::{uri_to_path, Backend, HydrationStats, SemanticRequestPerf};
use crate::call_model::{SourcePosition, SourceRange};
use crate::candidate_service::CandidateQueryService;
use crate::pathing;
use crate::query;

impl Backend {
    pub(super) async fn provide_signature_help(
        &self,
        params: SignatureHelpParams,
    ) -> LspResult<Option<SignatureHelp>> {
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
        let call: query::CallContext = match query::call_context_at(
            &text,
            position.position.line,
            position.position.character,
        ) {
            Some(call) => call,
            None => return Ok(None),
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
        let active_argument = call.active_argument;
        let call_name = call.name;
        let call_site_context = match call.argument_state {
            query::ArgumentState::Partial {
                minimum_arity,
                active_argument,
            } => {
                let reliability = if matches!(
                    call.form,
                    crate::call_model::CallForm::DirectName
                        | crate::call_model::CallForm::QualifiedName
                        | crate::call_model::CallForm::ParenthesizedName
                ) {
                    query::ContextReliability::Reliable
                } else {
                    query::ContextReliability::UnsupportedCallForm
                };
                query::CallSiteContext::partial(
                    call_name.clone(),
                    call.form,
                    SourceRange {
                        start: SourcePosition {
                            line: position.position.line,
                            character: position.position.character,
                        },
                        end: SourcePosition {
                            line: position.position.line,
                            character: position.position.character,
                        },
                        start_byte: 0,
                        end_byte: 0,
                    },
                    minimum_arity,
                    active_argument,
                    reliability,
                )
            }
            query::ArgumentState::Unknown => query::CallSiteContext {
                callee_name: call_name.clone(),
                qualified_name: None,
                form: call.form,
                callee_range: SourceRange {
                    start: SourcePosition {
                        line: position.position.line,
                        character: position.position.character,
                    },
                    end: SourcePosition {
                        line: position.position.line,
                        character: position.position.character,
                    },
                    start_byte: 0,
                    end_byte: 0,
                },
                argument_count: None,
                argument_state: query::ArgumentState::Unknown,
                reliability: query::ContextReliability::SyntaxErrorOverlap,
            },
            query::ArgumentState::Complete => return Ok(None),
        };
        let mut call_site_context = call_site_context;
        call_site_context.qualified_name = call.qualified_name;
        let current_text = text;

        let result = tokio::task::spawn_blocking(
            move || -> Result<(Vec<SignatureInformation>, usize, SemanticRequestPerf)> {
                let query_started = std::time::Instant::now();
                let service = CandidateQueryService::new(
                    call_read_handle.as_deref(),
                    &overlay,
                    &current_rel,
                    reach_scope.as_deref(),
                    reach_graph.as_deref(),
                );
                let candidates =
                    service.callable_candidates(&call_name, Some(call_site_context))?;
                let mut perf = SemanticRequestPerf::from_callable_set(&candidates);
                perf.reach_us = reach_us;
                let presentations = query::signature_presentations(&candidates.groups);
                let presentations =
                    &presentations[..presentations.len().min(query::SIGNATURE_HELP_LIMIT)];
                let mut source_paths: Vec<_> = presentations
                    .iter()
                    .map(|candidate| candidate.candidate.path.clone())
                    .collect();
                source_paths.sort();
                source_paths.dedup();
                let source_revisions = service.source_revisions(&source_paths)?;
                perf.query_us = query_started.elapsed().as_micros();
                let active_signature = query::signature_active_index(presentations);
                let hydration_started = std::time::Instant::now();
                let mut hydration = HydrationStats::default();
                let mut signatures = Vec::with_capacity(presentations.len());
                for candidate in presentations {
                    let signature = if candidate.anchor.presentation_signature.trim().is_empty() {
                        candidate.anchor.signature.normalized.clone()
                    } else {
                        candidate.anchor.presentation_signature.clone()
                    };
                    let ranked = query::RankedSignatureCandidate {
                        candidate: candidate.candidate.clone(),
                        signature,
                    };
                    let source =
                        super::hover::candidate_source_text_for_path_with_overlay_at_revision(
                            &root,
                            &current_rel,
                            &current_text,
                            &overlay,
                            &candidate.candidate.path,
                            &candidate.candidate.source,
                            source_revisions.get(&candidate.candidate.path),
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
                    signatures.push(signature_information_for_with_comment(
                        &ranked,
                        active_argument,
                        comment.as_ref(),
                        candidates.arity_mismatch_fallback,
                    ));
                }
                perf.returned = signatures.len();
                perf.hydration_us = hydration_started.elapsed().as_micros();
                perf.hydration_count = hydration.count;
                perf.hydration_bytes = hydration.bytes;
                Ok((signatures, active_signature, perf))
            },
        )
        .await;

        let metrics = result
            .as_ref()
            .ok()
            .and_then(|result| result.as_ref().ok().map(|(_, _, metrics)| *metrics))
            .unwrap_or_default();
        self.perf_log(|| metrics.log_line("signature_help", total_started.elapsed().as_micros()))
            .await;

        match self.unwrap_query("signature help", result).await {
            Some((signatures, active_signature, _)) => {
                Ok(signature_help_from_signatures(signatures, active_signature))
            }
            _ => Ok(None),
        }
    }
}

fn signature_help_from_signatures(
    signatures: Vec<SignatureInformation>,
    active_signature: usize,
) -> Option<SignatureHelp> {
    if signatures.is_empty() {
        return None;
    }
    let active_signature = active_signature.min(signatures.len() - 1);
    let active_parameter = signatures
        .get(active_signature)
        .and_then(|signature| signature.active_parameter);
    Some(SignatureHelp {
        signatures,
        active_signature: Some(active_signature as u32),
        active_parameter,
    })
}

#[cfg(test)]
pub(super) fn signature_information_for(
    ranked: &query::RankedSignatureCandidate,
    active_argument: u32,
) -> SignatureInformation {
    signature_information_for_with_comment(ranked, active_argument, None, false)
}

fn signature_information_for_with_comment(
    ranked: &query::RankedSignatureCandidate,
    active_argument: u32,
    comment: Option<&query::RenderedSymbolComment>,
    arity_mismatch_fallback: bool,
) -> SignatureInformation {
    let parts: query::SignatureParts = if ranked.candidate.name.is_empty() {
        query::signature_parts(&ranked.signature)
    } else {
        query::signature_parts_for_name(&ranked.signature, &ranked.candidate.name)
    };
    let spans: &[query::ParameterSpan] = &parts.parameters;
    let parameters: Vec<ParameterInformation> = spans
        .iter()
        .map(|span| ParameterInformation {
            label: ParameterLabel::LabelOffsets([
                byte_offset_to_utf16(&parts.label, span.start),
                byte_offset_to_utf16(&parts.label, span.end),
            ]),
            documentation: comment.and_then(|comment| {
                parameter_comment_for_span(&parts.label, span, comment).map(markdown_documentation)
            }),
        })
        .collect();
    let active_parameter = if parameters.is_empty() || active_argument as usize >= parameters.len()
    {
        None
    } else {
        Some(active_argument)
    };
    SignatureInformation {
        label: parts.label,
        documentation: Some(markdown_documentation(signature_documentation(
            ranked,
            comment,
            arity_mismatch_fallback,
        ))),
        parameters: (!parameters.is_empty()).then_some(parameters),
        active_parameter,
    }
}

fn signature_documentation(
    ranked: &query::RankedSignatureCandidate,
    comment: Option<&query::RenderedSymbolComment>,
    arity_mismatch_fallback: bool,
) -> String {
    let evidence = format!(
        "*FossilSense: tier: {} | confidence: {} | reason: {}*",
        ranked.candidate.tier.as_str(),
        ranked.candidate.confidence.as_str(),
        ranked.candidate.reason.as_str()
    );
    let mut documentation = match comment {
        Some(comment) if !comment.markdown.trim().is_empty() => {
            format!("{}\n\n---\n\n{evidence}", comment.markdown.trim_end())
        }
        _ => evidence,
    };
    if arity_mismatch_fallback {
        documentation.push_str(
            "\n\n> **Arity mismatch fallback:** no callable candidate matched the available argument-count evidence; showing conservative signatures.",
        );
    }
    documentation
}

fn markdown_documentation(value: String) -> Documentation {
    Documentation::MarkupContent(MarkupContent {
        kind: MarkupKind::Markdown,
        value,
    })
}

fn parameter_comment_for_span(
    label: &str,
    span: &query::ParameterSpan,
    comment: &query::RenderedSymbolComment,
) -> Option<String> {
    let start = (span.start as usize).min(label.len());
    let end = (span.end as usize).min(label.len());
    let parameter = label.get(start..end)?;
    comment
        .parameters
        .iter()
        .find(|documentation| contains_identifier(parameter, &documentation.name))
        .map(|documentation| documentation.markdown.clone())
}

fn contains_identifier(text: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    text.match_indices(name).any(|(start, matched)| {
        let end = start + matched.len();
        let left = text[..start].chars().next_back();
        let right = text[end..].chars().next();
        !left.is_some_and(|ch| ch == '_' || ch.is_ascii_alphanumeric())
            && !right.is_some_and(|ch| ch == '_' || ch.is_ascii_alphanumeric())
    })
}

fn byte_offset_to_utf16(label: &str, byte_offset: u32) -> u32 {
    let target = (byte_offset as usize).min(label.len());
    let mut units = 0u32;
    for (idx, ch) in label.char_indices() {
        if idx >= target {
            break;
        }
        units += ch.len_utf16() as u32;
    }
    units
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(
        signature: &str,
        tier: crate::model::ScopeTier,
    ) -> crate::query::RankedSignatureCandidate {
        candidate_named("foo", signature, tier)
    }

    fn candidate_named(
        name: &str,
        signature: &str,
        tier: crate::model::ScopeTier,
    ) -> crate::query::RankedSignatureCandidate {
        let (confidence, reason) = crate::resolver::confidence_reason_for(tier, true, None);
        crate::query::RankedSignatureCandidate {
            signature: signature.to_string(),
            candidate: crate::model::DefinitionCandidate {
                name: name.to_string(),
                kind: "function".to_string(),
                role: "definition".to_string(),
                path: "inc/foo.h".to_string(),
                range: crate::model::CandidateRange {
                    start_line: 0,
                    start_col: 0,
                    end_line: 0,
                    end_col: 0,
                },
                source: "workspace".to_string(),
                tier,
                base_match: 1000,
                confidence,
                reason,
            },
        }
    }

    #[test]
    fn signature_information_uses_parameter_offsets() {
        let info = signature_information_for(
            &candidate(
                "int foo(int a, const char *name)",
                crate::model::ScopeTier::Reachable,
            ),
            1,
        );
        assert_eq!(info.label, "int foo(int a, const char *name)");
        let params = info.parameters.expect("parameters");
        assert_eq!(params.len(), 2);
        assert_eq!(info.active_parameter, Some(1));
    }

    #[test]
    fn signature_information_documents_rank_reason() {
        let info = signature_information_for(
            &candidate("int foo(int a)", crate::model::ScopeTier::External),
            0,
        );
        let doc = match info.documentation.expect("documentation") {
            Documentation::String(value) => value,
            Documentation::MarkupContent(markup) => markup.value,
        };
        assert!(doc.contains("tier: external"));
        assert!(doc.contains("confidence: heuristic"));
        assert!(doc.contains("reason: external_first_layer"));
    }

    #[test]
    fn signature_information_renders_symbol_and_parameter_comments() {
        let source = "/**\n * @brief Copies bytes.\n * @param size number of bytes\n */\nint foo(int size);\n";
        let comment = query::comment_documentation_for_candidate_symbol(
            source,
            "foo",
            4,
            &crate::model::CandidateRange {
                start_line: 4,
                start_col: 0,
                end_line: 4,
                end_col: 3,
            },
        )
        .expect("comment");
        let info = signature_information_for_with_comment(
            &candidate("int foo(int size);", crate::model::ScopeTier::Reachable),
            0,
            Some(&comment),
            false,
        );
        let documentation = match info.documentation.expect("documentation") {
            Documentation::String(value) => value,
            Documentation::MarkupContent(markup) => markup.value,
        };
        assert!(documentation.contains("Copies bytes."));
        let parameter = info
            .parameters
            .expect("parameters")
            .remove(0)
            .documentation
            .expect("parameter documentation");
        let parameter = match parameter {
            Documentation::String(value) => value,
            Documentation::MarkupContent(markup) => markup.value,
        };
        assert!(parameter.contains("number of bytes"));
    }

    #[test]
    fn signature_information_omits_out_of_range_active_parameter() {
        let info = signature_information_for(
            &candidate("int foo(int a)", crate::model::ScopeTier::Global),
            3,
        );
        assert_eq!(info.active_parameter, None);
    }

    #[test]
    fn signature_help_active_parameter_uses_active_signature_only() {
        let signatures = vec![
            signature_information_for(
                &candidate("int foo(int a)", crate::model::ScopeTier::Current),
                2,
            ),
            signature_information_for(
                &candidate(
                    "int foo(int a, int b, int c)",
                    crate::model::ScopeTier::Global,
                ),
                2,
            ),
        ];
        let help = signature_help_from_signatures(signatures, 1).expect("signature help");
        assert_eq!(help.active_signature, Some(1));
        assert_eq!(help.active_parameter, Some(2));
        assert_eq!(help.signatures[0].active_parameter, None);
        assert_eq!(help.signatures[1].active_parameter, Some(2));
    }

    #[test]
    fn signature_information_does_not_label_function_pointer_return_parameters() {
        let info = signature_information_for(
            &candidate_named(
                "factory",
                "int (*factory(void))(int);",
                crate::model::ScopeTier::Reachable,
            ),
            0,
        );
        assert_eq!(info.label, "int (*factory(void))(int);");
        assert!(info.parameters.is_none());
        assert_eq!(info.active_parameter, None);
    }

    #[test]
    fn signature_information_converts_parameter_byte_offsets_to_utf16() {
        let info = signature_information_for(
            &candidate(
                "int foo(int 名称, const char *name)",
                crate::model::ScopeTier::Reachable,
            ),
            0,
        );
        let params = info.parameters.expect("parameters");
        let ParameterLabel::LabelOffsets(offsets) = params[0].label.clone() else {
            panic!("expected label offsets");
        };
        let byte_start = info.label.find("int 名称").expect("parameter") as u32;
        let byte_end = byte_start + "int 名称".len() as u32;
        assert_ne!(offsets, [byte_start, byte_end]);
        assert_eq!(utf16_slice(&info.label, offsets), "int 名称");
    }

    fn utf16_slice(label: &str, offsets: [u32; 2]) -> String {
        let units: Vec<u16> = label.encode_utf16().collect();
        String::from_utf16(&units[offsets[0] as usize..offsets[1] as usize]).expect("utf16")
    }
}
