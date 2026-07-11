use crate::completion::PROJECT_CONTEXT_MAX_BOOST;
use crate::completion_history::candidate_hash_key;
use crate::language_builtins::{LanguageBuiltin, LanguageBuiltinCategory};
use crate::model;
use crate::parser;
use crate::project_context::ProjectKey;
use crate::query::{self, NameTable};
use crate::reachability;
use crate::resolver;

use super::{
    CandidateEvidence, CandidateSource, CompletionCandidateKind,
    OrdinaryCompletionDocumentationTarget, OrdinaryCompletionKind, OrdinaryCompletionPresentation,
    OrdinaryPipelineCandidate,
};

pub(super) fn completion_items_for_language_builtins(
    prefix: &str,
) -> Vec<OrdinaryPipelineCandidate> {
    crate::language_builtins::language_builtins()
        .iter()
        .filter_map(|builtin| completion_item_for_language_builtin(*builtin, prefix))
        .collect()
}

fn completion_item_for_language_builtin(
    builtin: LanguageBuiltin,
    prefix: &str,
) -> Option<OrdinaryPipelineCandidate> {
    if builtin.label.eq_ignore_ascii_case(prefix) {
        return None;
    }
    let score = query::completion_word_score(prefix, builtin.label, 0)?;
    let mut evidence = CandidateEvidence::new(
        CandidateSource::LanguageBuiltin,
        model::ScopeTier::Global,
        model::ResolutionConfidence::Fallback,
        score,
    );
    evidence.match_score = score;
    evidence.kind = completion_kind_for_language_builtin(builtin.category);
    set_completion_history_key(&mut evidence, builtin.label);

    Some(OrdinaryPipelineCandidate::new(
        builtin.label,
        evidence,
        OrdinaryCompletionPresentation {
            kind: ordinary_kind_for_language_builtin(builtin.category),
            detail: Some(detail_for_language_builtin(builtin.category).to_string()),
            documentation: None,
            initial_sort_text: Some(format!("{:08}", 100_000_000 - score)),
            documentation_target: None,
        },
    ))
}

fn completion_kind_for_language_builtin(
    category: LanguageBuiltinCategory,
) -> CompletionCandidateKind {
    match category {
        LanguageBuiltinCategory::Keyword => CompletionCandidateKind::Keyword,
        LanguageBuiltinCategory::BuiltinType => CompletionCandidateKind::Type,
        LanguageBuiltinCategory::BuiltinConstant => CompletionCandidateKind::EnumConstant,
    }
}

fn ordinary_kind_for_language_builtin(category: LanguageBuiltinCategory) -> OrdinaryCompletionKind {
    match category {
        LanguageBuiltinCategory::Keyword => OrdinaryCompletionKind::Keyword,
        LanguageBuiltinCategory::BuiltinType => OrdinaryCompletionKind::Type,
        LanguageBuiltinCategory::BuiltinConstant => OrdinaryCompletionKind::EnumConstant,
    }
}

fn detail_for_language_builtin(category: LanguageBuiltinCategory) -> &'static str {
    match category {
        LanguageBuiltinCategory::Keyword => "keyword",
        LanguageBuiltinCategory::BuiltinType => "builtin type",
        LanguageBuiltinCategory::BuiltinConstant => "builtin constant",
    }
}

pub(super) fn completion_items_for_local_bindings(
    hits: Vec<query::LocalCompletionCandidate>,
    text: &str,
) -> Vec<OrdinaryPipelineCandidate> {
    hits.into_iter()
        .map(|hit| {
            let mut evidence = CandidateEvidence::new(
                CandidateSource::LocalBinding,
                model::ScopeTier::Current,
                model::ResolutionConfidence::Heuristic,
                hit.score,
            );
            evidence.match_score = hit.match_score;
            evidence.kind = CompletionCandidateKind::Variable;
            set_completion_history_key(&mut evidence, &hit.name);
            OrdinaryPipelineCandidate::new(
                hit.name.clone(),
                evidence,
                OrdinaryCompletionPresentation {
                    kind: OrdinaryCompletionKind::Variable,
                    detail: Some(hit.detail),
                    documentation: None,
                    initial_sort_text: Some(format!("{:08}", 100_000_000 - hit.score)),
                    documentation_target: Some(
                        OrdinaryCompletionDocumentationTarget::CurrentDocument {
                            start_line: line_for_byte(text, hit.decl_start_byte),
                        },
                    ),
                },
            )
        })
        .collect()
}

pub(super) fn completion_items_for_current_file_overlay(
    hits: Vec<query::CurrentFileOverlayCandidate>,
    text: &str,
) -> Vec<OrdinaryPipelineCandidate> {
    hits.into_iter()
        .map(|hit| {
            let is_text = !hit.semantic || hit.detail.as_deref() == Some("text");
            let source = if is_text {
                CandidateSource::LocalWord
            } else {
                CandidateSource::CurrentFileOverlay
            };
            let tier = if is_text {
                model::ScopeTier::Global
            } else {
                model::ScopeTier::Current
            };
            let confidence = if is_text {
                model::ResolutionConfidence::Fallback
            } else {
                model::ResolutionConfidence::Heuristic
            };
            let mut evidence = CandidateEvidence::new(source, tier, confidence, hit.match_score);
            evidence.match_score = hit.match_score;
            evidence.proximity_score = hit.proximity_score;
            evidence.kind = if is_text {
                CompletionCandidateKind::Text
            } else {
                completion_candidate_kind_from_parser(hit.kind)
            };
            set_completion_history_key(&mut evidence, &hit.name);

            OrdinaryPipelineCandidate::new(
                hit.name.clone(),
                evidence,
                OrdinaryCompletionPresentation {
                    kind: if is_text {
                        OrdinaryCompletionKind::Text
                    } else {
                        ordinary_kind_from_parser(hit.kind)
                    },
                    detail: hit.detail,
                    documentation: None,
                    initial_sort_text: None,
                    documentation_target: (!is_text).then_some(
                        OrdinaryCompletionDocumentationTarget::CurrentDocument {
                            start_line: line_for_byte(text, hit.source_start_byte),
                        },
                    ),
                },
            )
        })
        .collect()
}

pub(super) fn completion_items_for_indexed_hits(
    hits: Vec<query::RankedNameHit>,
    open_reason: Option<reachability::OpenReason>,
    active_project_context: Option<&ProjectKey>,
    table_index: usize,
) -> Vec<OrdinaryPipelineCandidate> {
    hits.into_iter()
        .map(|hit| {
            let (confidence, reason) =
                resolver::confidence_reason_for(hit.tier, false, open_reason);
            let label = model::completion_scope_label(hit.tier, confidence, reason);
            let mut evidence =
                CandidateEvidence::new(CandidateSource::Indexed, hit.tier, confidence, hit.score);
            evidence.match_score = hit.base_match;
            if active_project_context.is_some()
                && hit.project_key.as_ref() == active_project_context
            {
                evidence.project_score = PROJECT_CONTEXT_MAX_BOOST;
            }
            evidence.kind = completion_candidate_kind_from_parser(hit.kind);
            set_completion_history_key(&mut evidence, &hit.name);
            OrdinaryPipelineCandidate::new(
                hit.name.clone(),
                evidence,
                OrdinaryCompletionPresentation {
                    kind: ordinary_kind_from_parser(hit.kind),
                    initial_sort_text: Some(format!("{:08}", 100_000_000 - hit.score)),
                    detail: label.as_ref().map(|value| value.detail.to_string()),
                    documentation: label.map(|value| value.documentation),
                    documentation_target: Some(OrdinaryCompletionDocumentationTarget::Indexed {
                        table_index,
                        symbol_id: hit.id,
                    }),
                },
            )
        })
        .collect()
}

pub(super) fn exact_indexed_completion_candidates_for_local_word(
    table: (&NameTable, usize),
    word: &str,
    local_score: i32,
    scope: Option<&query::CompletionScope>,
    active_project_context: Option<&ProjectKey>,
    open_reason: Option<reachability::OpenReason>,
    limit: usize,
) -> Vec<OrdinaryPipelineCandidate> {
    let (table, table_index) = table;
    table
        .exact_name_hits_scoped(word, limit, scope)
        .into_iter()
        .map(|hit| {
            let (confidence, reason) =
                resolver::confidence_reason_for(hit.tier, false, open_reason);
            let label = model::completion_scope_label(hit.tier, confidence, reason);
            let mut evidence =
                CandidateEvidence::new(CandidateSource::Indexed, hit.tier, confidence, local_score);
            evidence.match_score = hit.base_match;
            if active_project_context.is_some()
                && hit.project_key.as_ref() == active_project_context
            {
                evidence.project_score = PROJECT_CONTEXT_MAX_BOOST;
            }
            evidence.kind = completion_candidate_kind_from_parser(hit.kind);
            set_completion_history_key(&mut evidence, &hit.name);
            OrdinaryPipelineCandidate::new(
                hit.name.clone(),
                evidence,
                OrdinaryCompletionPresentation {
                    kind: ordinary_kind_from_parser(hit.kind),
                    initial_sort_text: Some(format!("{:08}", 100_000_000 - local_score)),
                    detail: label.as_ref().map(|value| value.detail.to_string()),
                    documentation: label.map(|value| value.documentation),
                    documentation_target: Some(OrdinaryCompletionDocumentationTarget::Indexed {
                        table_index,
                        symbol_id: hit.id,
                    }),
                },
            )
        })
        .collect()
}

fn line_for_byte(text: &str, byte: usize) -> u32 {
    text.as_bytes()[..byte.min(text.len())]
        .iter()
        .filter(|value| **value == b'\n')
        .count() as u32
}

fn completion_candidate_kind_from_parser(kind: parser::SymbolKind) -> CompletionCandidateKind {
    match kind {
        parser::SymbolKind::Function => CompletionCandidateKind::Function,
        parser::SymbolKind::Macro => CompletionCandidateKind::Macro,
        parser::SymbolKind::Type => CompletionCandidateKind::Type,
        parser::SymbolKind::EnumConstant => CompletionCandidateKind::EnumConstant,
        parser::SymbolKind::GlobalVariable | parser::SymbolKind::Field => {
            CompletionCandidateKind::Variable
        }
    }
}

fn ordinary_kind_from_parser(kind: parser::SymbolKind) -> OrdinaryCompletionKind {
    match kind {
        parser::SymbolKind::Function => OrdinaryCompletionKind::Function,
        parser::SymbolKind::Macro => OrdinaryCompletionKind::Macro,
        parser::SymbolKind::Type => OrdinaryCompletionKind::Type,
        parser::SymbolKind::EnumConstant => OrdinaryCompletionKind::EnumConstant,
        parser::SymbolKind::GlobalVariable | parser::SymbolKind::Field => {
            OrdinaryCompletionKind::Variable
        }
    }
}

pub(super) fn set_completion_history_key(evidence: &mut CandidateEvidence, label: &str) {
    evidence.history_key = Some(candidate_hash_key(
        label,
        evidence.kind.as_history_kind_str(),
    ));
}
