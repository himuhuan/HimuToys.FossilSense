use std::collections::HashSet;
use std::sync::Arc;

use crate::completion_history::{candidate_hash_key, CompletionHistorySnapshot};
use crate::language_builtins::{LanguageBuiltin, LanguageBuiltinCategory};
use crate::model;
use crate::parser::{self, FactAvailability, FactGroup, FileSemanticIndex};
use crate::project_context::ProjectContextKey;
use crate::query::{self, NameTable};
use crate::reachability;
use crate::resolver;

use super::{
    CandidateEvidence, CandidateSource, CompletionCandidateKind, CompletionIntent,
    CompletionPipelineMetrics, CompletionRankContext, PipelineCandidate,
};

type OrdinaryPipelineCandidate = PipelineCandidate<OrdinaryCompletionPresentation>;

#[derive(Clone)]
pub(crate) struct OrdinaryCompletionInput {
    pub prefix: String,
    pub text: String,
    pub line: u32,
    pub character: u32,
    pub parsed_document: Option<Arc<FileSemanticIndex>>,
    pub local_words: Arc<HashSet<String>>,
    pub tables: Vec<OrdinaryCompletionNameTable>,
    pub scope: Option<query::CompletionScope>,
    pub active_project_context: Option<ProjectContextKey>,
    pub prior_pools: Vec<Option<Vec<usize>>>,
    pub intent: CompletionIntent,
    pub history_enabled: bool,
    pub history: CompletionHistorySnapshot,
    pub prefix_bucket: String,
    pub limit: usize,
    pub locality_bonus: i32,
}

#[derive(Clone)]
pub(crate) struct OrdinaryCompletionNameTable {
    pub table: Arc<NameTable>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OrdinaryCompletionOutput {
    pub items: Vec<OrdinaryCompletionItem>,
    pub new_pools: Vec<Vec<usize>>,
    pub metrics: CompletionPipelineMetrics,
    pub recall_ms: u128,
    pub merge_rank_ms: u128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OrdinaryCompletionItem {
    pub label: String,
    pub kind: OrdinaryCompletionKind,
    pub detail: Option<String>,
    pub documentation: Option<String>,
    pub initial_sort_text: Option<String>,
    pub evidence: CandidateEvidence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OrdinaryCompletionPresentation {
    kind: OrdinaryCompletionKind,
    detail: Option<String>,
    documentation: Option<String>,
    initial_sort_text: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OrdinaryCompletionKind {
    Text,
    Keyword,
    Function,
    Macro,
    Type,
    Variable,
    EnumConstant,
}

fn completion_items_for_language_builtins(prefix: &str) -> Vec<OrdinaryPipelineCandidate> {
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

pub(crate) fn complete_ordinary_identifier(
    input: OrdinaryCompletionInput,
) -> OrdinaryCompletionOutput {
    let recall_started = std::time::Instant::now();
    let open_reason = input.scope.as_ref().and_then(|scope| scope.reach.reason);
    let mut candidates: Vec<OrdinaryPipelineCandidate> = Vec::new();
    let mut new_pools: Vec<Vec<usize>> = Vec::with_capacity(input.tables.len());
    let mut recall_channels = query::CompletionRecallMetrics::default();

    let quotas = query::CompletionRecallQuotas::default_for_completion_limit(input.limit);
    for (idx, table) in input.tables.iter().enumerate() {
        let prior = input.prior_pools.get(idx).and_then(|pool| pool.as_deref());
        let (hits, pool, metrics) = table.table.search_completion_recall_pooled(
            &input.prefix,
            quotas,
            input.scope.as_ref(),
            input.active_project_context.as_ref(),
            prior,
        );
        recall_channels.merge_from(metrics);
        new_pools.push(pool);
        candidates.extend(completion_items_for_indexed_hits(
            hits,
            open_reason,
            input.active_project_context.as_ref(),
        ));
    }

    let local_binding_hits = input
        .parsed_document
        .as_ref()
        .map(|index| {
            let request_facts = index.request_facts();
            let local_bindings = match index.fact_availability(FactGroup::LocalBindings) {
                FactAvailability::Available => request_facts.local_bindings,
                FactAvailability::NotRequested | FactAvailability::Unavailable(_) => &[],
            };
            query::local_completion_candidates(
                local_bindings,
                &input.text,
                input.line,
                input.character,
                &input.prefix,
                input.limit,
            )
        })
        .unwrap_or_default();
    candidates.extend(completion_items_for_local_bindings(local_binding_hits));

    let current_file_overlay_hits = input
        .parsed_document
        .as_ref()
        .map(|index| {
            query::current_file_overlay_candidates(
                index,
                &input.text,
                input.line,
                input.character,
                &input.prefix,
                input.limit,
            )
        })
        .unwrap_or_default();
    let current_file_text_overlay_names: HashSet<String> = current_file_overlay_hits
        .iter()
        .filter(|hit| !hit.semantic || hit.detail.as_deref() == Some("text"))
        .map(|hit| hit.name.clone())
        .collect();
    candidates.extend(completion_items_for_current_file_overlay(
        current_file_overlay_hits,
    ));
    candidates.extend(completion_items_for_language_builtins(&input.prefix));

    for word in input.local_words.iter() {
        if word == &input.prefix {
            continue;
        }
        let Some(word_score) =
            query::completion_word_score(&input.prefix, word, input.locality_bonus)
        else {
            continue;
        };
        let tier = model::ScopeTier::Global;
        let (confidence, _reason) = resolver::confidence_reason_for(tier, false, None);
        let sort_text = format!("{:08}", 100_000_000 - word_score);
        let mut exact_indexed = Vec::new();
        for table in &input.tables {
            exact_indexed.extend(exact_indexed_completion_candidates_for_local_word(
                table.table.as_ref(),
                word,
                word_score,
                input.scope.as_ref(),
                input.active_project_context.as_ref(),
                open_reason,
                input.limit,
            ));
        }
        if !exact_indexed.is_empty() {
            candidates.extend(exact_indexed);
            continue;
        }
        if current_file_text_overlay_names.contains(word.as_str()) {
            continue;
        }
        let mut evidence =
            CandidateEvidence::new(CandidateSource::LocalWord, tier, confidence, word_score);
        evidence.kind = CompletionCandidateKind::Text;
        set_completion_history_key(&mut evidence, word);
        candidates.push(OrdinaryPipelineCandidate::new(
            word.clone(),
            evidence,
            OrdinaryCompletionPresentation {
                kind: OrdinaryCompletionKind::Text,
                detail: None,
                documentation: None,
                initial_sort_text: Some(sort_text),
            },
        ));
    }

    let recall_ms = recall_started.elapsed().as_millis();
    let merge_rank_started = std::time::Instant::now();
    let mut output = super::run_evidence_aware_pipeline_with_context(
        candidates,
        input.limit,
        CompletionRankContext {
            intent: input.intent,
            history_enabled: input.history_enabled,
            history: input.history,
            prefix_bucket: input.prefix_bucket,
        },
    );
    output.metrics.recall_channels = recall_channels;
    let merge_rank_ms = merge_rank_started.elapsed().as_millis();
    let items = output
        .items
        .into_iter()
        .map(|candidate| {
            let payload = candidate.payload;
            OrdinaryCompletionItem {
                label: candidate.name,
                kind: payload.kind,
                detail: payload.detail,
                documentation: payload.documentation,
                initial_sort_text: payload.initial_sort_text,
                evidence: candidate.evidence,
            }
        })
        .collect();

    OrdinaryCompletionOutput {
        items,
        new_pools,
        metrics: output.metrics,
        recall_ms,
        merge_rank_ms,
    }
}

fn completion_items_for_local_bindings(
    hits: Vec<query::LocalCompletionCandidate>,
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
                },
            )
        })
        .collect()
}

fn completion_items_for_current_file_overlay(
    hits: Vec<query::CurrentFileOverlayCandidate>,
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
                },
            )
        })
        .collect()
}

fn completion_items_for_indexed_hits(
    hits: Vec<query::RankedNameHit>,
    open_reason: Option<reachability::OpenReason>,
    active_project_context: Option<&ProjectContextKey>,
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
                && hit.project_context.as_ref() == active_project_context
            {
                evidence.project_score = 350;
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
                },
            )
        })
        .collect()
}

fn exact_indexed_completion_candidates_for_local_word(
    table: &NameTable,
    word: &str,
    local_score: i32,
    scope: Option<&query::CompletionScope>,
    active_project_context: Option<&ProjectContextKey>,
    open_reason: Option<reachability::OpenReason>,
    limit: usize,
) -> Vec<OrdinaryPipelineCandidate> {
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
                && hit.project_context.as_ref() == active_project_context
            {
                evidence.project_score = 350;
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
                },
            )
        })
        .collect()
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

fn set_completion_history_key(evidence: &mut CandidateEvidence, label: &str) {
    evidence.history_key = Some(candidate_hash_key(
        label,
        evidence.kind.as_history_kind_str(),
    ));
}

#[cfg(test)]
mod tests;
