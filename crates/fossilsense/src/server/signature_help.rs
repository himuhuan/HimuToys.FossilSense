use anyhow::Result;
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::{
    Documentation, ParameterInformation, ParameterLabel, SignatureHelp, SignatureHelpParams,
    SignatureInformation,
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
        let reach_scope = self.reach_scope_for(&uri).await.map(|(_, reach)| reach);
        let active_argument = call.active_argument;
        let call_name = call.name;
        let limit = query::SIGNATURE_HELP_LIMIT;

        let result = tokio::task::spawn_blocking(move || -> Result<Vec<SignatureInformation>> {
            let db_path = pathing::default_index_path(&root)?;
            if !db_path.exists() {
                return Ok(Vec::new());
            }
            let store = IndexStore::open_readonly(&db_path)?;
            let ranked = query::rank_function_signature_candidates(
                store.symbol_read_view().symbols_by_name(&call_name)?,
                &current_rel,
                reach_scope.as_deref(),
                limit,
            );
            Ok(ranked
                .iter()
                .map(|candidate| signature_information_for(candidate, active_argument))
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

pub(super) fn signature_information_for(
    ranked: &query::RankedSignatureCandidate,
    active_argument: u32,
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
            documentation: None,
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
        documentation: Some(Documentation::String(format!(
            "tier: {}\nconfidence: {}\nreason: {}",
            ranked.candidate.tier.as_str(),
            ranked.candidate.confidence.as_str(),
            ranked.candidate.reason.as_str()
        ))),
        parameters: (!parameters.is_empty()).then_some(parameters),
        active_parameter,
    }
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
