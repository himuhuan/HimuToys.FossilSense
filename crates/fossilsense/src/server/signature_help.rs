use anyhow::Result;
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::{
    Documentation, MarkupContent, MarkupKind, ParameterInformation, ParameterLabel, SignatureHelp,
    SignatureHelpParams, SignatureInformation,
};

use super::{uri_to_path, Backend};
use crate::pathing;
use crate::query;
use crate::store::IndexStore;

impl Backend {
    pub(super) async fn provide_signature_help(
        &self,
        params: SignatureHelpParams,
    ) -> LspResult<Option<SignatureHelp>> {
        let position = params.text_document_position_params;
        let uri = position.text_document.uri;
        let Some((_version, text)) = self.document_snapshot(&uri).await else {
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
        let context = self.request_context_for_root(root.clone()).await;
        let reach_scope = self
            .reach_scope_from_context(&uri, &context)
            .map(|(_, reach)| reach);
        let project_context = context.engine.project_context.clone();
        let semantic_generation = context.engine.semantic_generation.0;
        let active_argument = call.active_argument;
        let call_name = call.name;
        let limit = query::SIGNATURE_HELP_LIMIT;
        let current_text = text;

        let result = tokio::task::spawn_blocking(move || -> Result<Vec<SignatureInformation>> {
            let db_path = pathing::default_index_path(&root)?;
            if !db_path.exists() {
                return Ok(Vec::new());
            }
            let records = IndexStore::read_at_generation(&db_path, semantic_generation, |store| {
                store.symbol_read_view().symbols_by_name(&call_name)
            })?;
            let ranked = query::rank_function_signature_candidates(
                records.clone(),
                &current_rel,
                reach_scope.as_deref(),
                limit,
            );
            let documentation_candidates: Vec<_> = query::rank_function_signature_candidates(
                records,
                &current_rel,
                reach_scope.as_deref(),
                32,
            )
            .iter()
            .map(|candidate| query::DocumentationCandidate {
                candidate: candidate.candidate.clone(),
                signature: candidate.signature.clone(),
            })
            .collect();
            Ok(ranked
                .iter()
                .map(|candidate| {
                    let primary = query::DocumentationCandidate {
                        candidate: candidate.candidate.clone(),
                        signature: candidate.signature.clone(),
                    };
                    let preferred = super::completion_documentation::preferred_symbol_documentation(
                        &root,
                        &current_rel,
                        &current_text,
                        &primary,
                        &documentation_candidates,
                        project_context.as_deref(),
                    );
                    let mut presentation = candidate.clone();
                    presentation.signature = preferred.presentation.signature;
                    signature_information_for_with_comment(
                        &presentation,
                        active_argument,
                        preferred.comment.as_ref(),
                    )
                })
                .collect())
        })
        .await;

        match self.unwrap_query("signature help", result).await {
            Some(signatures) => Ok(signature_help_from_signatures(signatures)),
            _ => Ok(None),
        }
    }
}

fn signature_help_from_signatures(signatures: Vec<SignatureInformation>) -> Option<SignatureHelp> {
    if signatures.is_empty() {
        return None;
    }
    let active_parameter = signatures
        .first()
        .and_then(|signature| signature.active_parameter);
    Some(SignatureHelp {
        signatures,
        active_signature: Some(0),
        active_parameter,
    })
}

#[cfg(test)]
pub(super) fn signature_information_for(
    ranked: &query::RankedSignatureCandidate,
    active_argument: u32,
) -> SignatureInformation {
    signature_information_for_with_comment(ranked, active_argument, None)
}

fn signature_information_for_with_comment(
    ranked: &query::RankedSignatureCandidate,
    active_argument: u32,
    comment: Option<&query::RenderedSymbolComment>,
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
            ranked, comment,
        ))),
        parameters: (!parameters.is_empty()).then_some(parameters),
        active_parameter,
    }
}

fn signature_documentation(
    ranked: &query::RankedSignatureCandidate,
    comment: Option<&query::RenderedSymbolComment>,
) -> String {
    let evidence = format!(
        "*FossilSense: tier: {} | confidence: {} | reason: {}*",
        ranked.candidate.tier.as_str(),
        ranked.candidate.confidence.as_str(),
        ranked.candidate.reason.as_str()
    );
    match comment {
        Some(comment) if !comment.markdown.trim().is_empty() => {
            format!("{}\n\n---\n\n{evidence}", comment.markdown.trim_end())
        }
        _ => evidence,
    }
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
        let help = signature_help_from_signatures(signatures).expect("signature help");
        assert_eq!(help.active_signature, Some(0));
        assert_eq!(help.active_parameter, None);
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
