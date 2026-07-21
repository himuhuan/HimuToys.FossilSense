use std::collections::HashMap;
use std::path::PathBuf;

use tower_lsp::lsp_types::CompletionItemKind;

use crate::model;
use crate::parser::{MemberConfidence, MemberKind};

use super::MemberPresentation;

pub(super) fn remember_member(
    members: &mut HashMap<(String, MemberKind), MemberPresentation>,
    candidate: crate::model::MemberCandidate,
    root: PathBuf,
    semantic_generation: crate::call_model::SemanticGeneration,
    weak_receiver: bool,
    ambiguous_owner: bool,
) {
    let key = (candidate.name.to_ascii_lowercase(), candidate.kind);
    let presentation = MemberPresentation {
        candidate,
        root,
        semantic_generation,
        weak_receiver,
        ambiguous_owner,
    };
    match members.get(&key) {
        Some(existing) if member_candidate_better(&existing.candidate, &presentation.candidate) => {
        }
        _ => {
            members.insert(key, presentation);
        }
    }
}

fn member_candidate_better(
    current: &crate::model::MemberCandidate,
    incoming: &crate::model::MemberCandidate,
) -> bool {
    current
        .tier
        .rank()
        .cmp(&incoming.tier.rank())
        .then_with(|| {
            member_confidence_rank(current.confidence)
                .cmp(&member_confidence_rank(incoming.confidence))
        })
        .then_with(|| incoming.signature.cmp(&current.signature))
        .then_with(|| incoming.owner_path.cmp(&current.owner_path))
        .then_with(|| {
            incoming
                .owner_revision_hash
                .cmp(&current.owner_revision_hash)
        })
        .is_gt()
}

fn member_confidence_rank(confidence: MemberConfidence) -> i32 {
    match confidence {
        MemberConfidence::InBody => 2,
        MemberConfidence::OutOfClassOwner => 1,
        MemberConfidence::Heuristic => 0,
    }
}

pub(super) fn member_kind_rank(kind: MemberKind) -> i32 {
    match kind {
        MemberKind::Field => 0,
        MemberKind::Method => 1,
        MemberKind::StaticMethod => 2,
        MemberKind::NestedType => 3,
    }
}

pub(super) fn lsp_kind_for_member(kind: MemberKind) -> CompletionItemKind {
    match kind {
        MemberKind::Field => CompletionItemKind::FIELD,
        MemberKind::Method | MemberKind::StaticMethod => CompletionItemKind::METHOD,
        MemberKind::NestedType => CompletionItemKind::CLASS,
    }
}

fn member_kind_label(kind: MemberKind) -> &'static str {
    match kind {
        MemberKind::Field => "field",
        MemberKind::Method => "method",
        MemberKind::StaticMethod => "static method",
        MemberKind::NestedType => "nested type",
    }
}

pub(super) fn member_detail(
    kind: MemberKind,
    scope_label: Option<&model::CompletionScopeLabel>,
    weak_receiver: bool,
    ambiguous_owner: bool,
) -> String {
    let mut parts = vec![member_kind_label(kind).to_string()];
    if let Some(label) = scope_label {
        parts.push(label.detail.to_string());
    }
    if weak_receiver {
        parts.push("heuristic receiver".to_string());
    }
    if ambiguous_owner {
        parts.push("ambiguous owner".to_string());
    }
    parts.join(" ")
}

pub(super) fn member_documentation(
    kind: MemberKind,
    confidence: MemberConfidence,
    scope_label: Option<&model::CompletionScopeLabel>,
    weak_receiver: bool,
    ambiguous_owner: bool,
) -> String {
    let scope = scope_label
        .map(|label| label.documentation.as_str())
        .unwrap_or("FossilSense: current member candidate");
    let receiver = if weak_receiver {
        ", heuristic_receiver"
    } else {
        ""
    };
    let owner = if ambiguous_owner {
        ", ambiguous_owner"
    } else {
        ""
    };
    format!(
        "FossilSense: {} member candidate ({}, {}{}{})",
        member_kind_label(kind),
        scope,
        confidence.as_str(),
        receiver,
        owner,
    )
}

pub(super) fn retain_global_highest_record_tier(
    candidates_by_root: &mut Vec<(PathBuf, Vec<crate::query::RecordCandidate>)>,
) {
    let highest_rank = candidates_by_root
        .iter()
        .flat_map(|(_, candidates)| candidates.iter().map(|candidate| candidate.tier.rank()))
        .max();
    let Some(highest_rank) = highest_rank else {
        return;
    };
    for (_, candidates) in candidates_by_root.iter_mut() {
        candidates.retain(|candidate| candidate.tier.rank() == highest_rank);
    }
    candidates_by_root.retain(|(_, candidates)| !candidates.is_empty());
}
