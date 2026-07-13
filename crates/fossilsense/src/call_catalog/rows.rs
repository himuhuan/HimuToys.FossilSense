use crate::call_model::{
    AnchorRole, CallForm, CallSiteFact, CallableAnchor, CallableKind, FactProvenance,
    LinkageDomain, OwnerKindHint, SignatureShape,
};
use crate::store::views::{CallSiteRow, CallableAnchorRow};

pub(crate) fn anchor_from_row(row: CallableAnchorRow) -> CallableAnchor {
    let linkage = match row.linkage_kind.as_str() {
        "external" => LinkageDomain::External,
        // The parser sees an absolute path while active store rows expose the
        // canonical workspace-relative path. Normalize the domain here so a
        // same-file static call compares like with like.
        "internal" => LinkageDomain::Internal(row.path.clone()),
        _ => LinkageDomain::Unknown,
    };
    CallableAnchor {
        path: row.path,
        name: row.name,
        qualified_name: row.qualified_name,
        owner: row.owner,
        owner_kind: parse_owner_kind(row.owner_kind.as_deref()),
        kind: parse_callable_kind(&row.kind),
        role: parse_anchor_role(&row.role),
        linkage,
        signature: SignatureShape {
            normalized: row.signature,
            min_arity: row.min_arity,
            max_arity: row.max_arity,
            variadic: row.variadic,
        },
        canonical_signature: row.canonical_signature,
        presentation_signature: row.presentation_signature,
        signature_fidelity: row.signature_fidelity,
        name_range: row.name_range,
        declaration_range: row.declaration_range,
        body_range: row.body_range,
        guard: row.guard,
        provenance: parse_provenance(&row.provenance),
        syntax_error_overlap: row.syntax_error_overlap,
        entity_key: row.entity_key,
        anchor_fingerprint: row.anchor_fingerprint,
    }
}

pub(crate) fn call_from_row(row: CallSiteRow) -> CallSiteFact {
    CallSiteFact {
        path: row.path,
        caller_entity_key: row.caller_entity_key,
        expression_range: row.expression_range,
        callee_range: row.callee_range,
        callee_name: row.callee_name,
        qualified_name: row.qualified_name,
        form: parse_call_form(&row.call_form),
        argument_count: row.argument_count,
        guard: row.guard,
        provenance: parse_provenance(&row.provenance),
        syntax_error_overlap: row.syntax_error_overlap,
        site_fingerprint: row.site_fingerprint,
    }
}

fn parse_callable_kind(value: &str) -> CallableKind {
    match value {
        "synthetic_global_initializer" => CallableKind::SyntheticGlobalInitializer,
        "synthetic_lambda" => CallableKind::SyntheticLambda,
        "function_like_macro" => CallableKind::FunctionLikeMacro,
        _ => CallableKind::Function,
    }
}

fn parse_anchor_role(value: &str) -> AnchorRole {
    match value {
        "definition" => AnchorRole::Definition,
        "synthetic" => AnchorRole::Synthetic,
        _ => AnchorRole::Declaration,
    }
}

fn parse_owner_kind(value: Option<&str>) -> Option<OwnerKindHint> {
    value.map(|value| match value {
        "namespace" => OwnerKindHint::Namespace,
        "record" => OwnerKindHint::Record,
        _ => OwnerKindHint::Unknown,
    })
}

fn parse_provenance(value: &str) -> FactProvenance {
    match value {
        "synthetic" => FactProvenance::Synthetic,
        "lexical_fallback" => FactProvenance::LexicalFallback,
        _ => FactProvenance::Ast,
    }
}

fn parse_call_form(value: &str) -> CallForm {
    match value {
        "direct_name" => CallForm::DirectName,
        "qualified_name" => CallForm::QualifiedName,
        "parenthesized_name" => CallForm::ParenthesizedName,
        "member_dot" => CallForm::MemberDot,
        "member_arrow" => CallForm::MemberArrow,
        "static_member" => CallForm::StaticMember,
        "function_pointer" => CallForm::FunctionPointer,
        "callable_object" => CallForm::CallableObject,
        "explicit_construction" => CallForm::ExplicitConstruction,
        _ => CallForm::Unsupported,
    }
}
