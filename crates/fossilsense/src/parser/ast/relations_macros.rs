use crate::call_model::*;
use crate::semantic_model::relations::MacroFact;

pub(super) fn append_macros(
    path: &str,
    macros: &[MacroFact],
    anchors: &mut Vec<CallableAnchor>,
    calls: &mut Vec<CallSiteFact>,
) {
    for fact in macros.iter().filter(|f| f.function_like && !f.undef) {
        let key = blake3::hash(
            format!(
                "macro|{path}|{}|{}",
                fact.name, fact.source.range.start_byte
            )
            .as_bytes(),
        )
        .to_hex()[..24]
            .to_owned();
        let range = fact.source.range;
        anchors.push(CallableAnchor {
            path: path.to_owned(),
            name: fact.name.clone(),
            qualified_name: fact.name.clone(),
            owner: None,
            owner_kind: None,
            kind: CallableKind::FunctionLikeMacro,
            role: AnchorRole::Definition,
            linkage: LinkageDomain::Internal(path.to_owned()),
            signature: SignatureShape {
                normalized: format!("{}(...)", fact.name),
                min_arity: None,
                max_arity: None,
                variadic: true,
            },
            canonical_signature: format!("macro {}", fact.name),
            presentation_signature: format!("#define {}(...)", fact.name),
            signature_fidelity: SignatureFidelity::AstExact,
            name_range: range,
            declaration_range: SourceRange {
                end: fact.replacement_range.end,
                end_byte: fact.replacement_range.end_byte,
                ..range
            },
            body_range: None,
            guard: fact.source.guard.clone(),
            provenance: FactProvenance::Ast,
            syntax_error_overlap: false,
            entity_key: key.clone(),
            anchor_fingerprint: key.clone(),
        });
        for (index, name) in fact.direct_calls.iter().enumerate() {
            calls.push(CallSiteFact {
                path: path.to_owned(),
                caller_entity_key: key.clone(),
                expression_range: fact.replacement_range,
                callee_range: fact.replacement_range,
                callee_name: Some(name.clone()),
                qualified_name: None,
                form: CallForm::DirectName,
                argument_count: None,
                guard: fact.source.guard.clone(),
                provenance: FactProvenance::Ast,
                syntax_error_overlap: false,
                site_fingerprint: blake3::hash(format!("{key}|replacement|{index}").as_bytes())
                    .to_hex()[..24]
                    .to_owned(),
            });
        }
    }
}
