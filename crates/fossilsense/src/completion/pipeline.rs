use std::collections::{HashMap, HashSet};

use crate::completion_history::candidate_hash_key;
use crate::model::{ResolutionConfidence, ScopeTier};

use super::evidence::{
    candidate_beats, CandidateEvidence, CandidateSource, CompletionCandidateKind,
    CompletionPipelineMetrics, CompletionPipelineOutput, CompletionRankContext,
    CompletionStageTimings, FinalRankSummary, PipelineCandidate, ShadowRankSummary, SourceCounts,
};
use super::intent::{CompletionIntent, CompletionIntentConfidence, CompletionIntentKind};

#[allow(dead_code)]
const SOURCE_LOCAL_BINDING: i32 = 12_000;
#[allow(dead_code)]
const SOURCE_CURRENT_FILE_OVERLAY: i32 = 9_000;
#[allow(dead_code)]
const SOURCE_INDEXED: i32 = 5_000;
#[allow(dead_code)]
const SOURCE_LANGUAGE_BUILTIN: i32 = 2_000;
#[allow(dead_code)]
const SOURCE_LOCAL_WORD: i32 = 0;

#[allow(dead_code)]
const SCOPE_CURRENT: i32 = 6_000;
#[allow(dead_code)]
const SCOPE_REACHABLE: i32 = 4_800;
#[allow(dead_code)]
const SCOPE_EXTERNAL: i32 = 4_200;
#[allow(dead_code)]
const SCOPE_UNKNOWN: i32 = 3_200;
#[allow(dead_code)]
const SCOPE_GLOBAL: i32 = 2_400;

#[allow(dead_code)]
const CONFIDENCE_EXACT: i32 = 2_000;
#[allow(dead_code)]
const CONFIDENCE_REACHABLE: i32 = 1_500;
#[allow(dead_code)]
const CONFIDENCE_HEURISTIC: i32 = 1_000;
#[allow(dead_code)]
const CONFIDENCE_AMBIGUOUS: i32 = 500;
#[allow(dead_code)]
const CONFIDENCE_FALLBACK: i32 = 0;

#[allow(dead_code)]
const PROXIMITY_SCORE_CAP: i32 = 750;
#[allow(dead_code)]
const LOW_TRUST_GLOBAL_TEXT_CAP_BELOW_REACHABLE: i32 = 8_000;
#[allow(dead_code)]
const INTENT_STRONG_MATCH: i32 = 1_600;
#[allow(dead_code)]
const INTENT_MEDIUM_MATCH: i32 = 900;
#[allow(dead_code)]
const INTENT_BOUNDED_DEMOTION: i32 = -450;
#[allow(dead_code)]
const DECLARATION_GLOBAL_REUSE_DEMOTION: i32 = -5_000;
const HISTORY_MAX_BOOST: i32 = 700;
const HISTORY_REPEAT_STEP: i32 = 120;
const PROJECT_CONTEXT_MAX_BOOST: i32 = 350;

#[cfg(test)]
pub(crate) fn run_compatible_pipeline<T>(
    candidates: Vec<PipelineCandidate<T>>,
    limit: usize,
) -> CompletionPipelineOutput<T> {
    let mut metrics = CompletionPipelineMetrics {
        input_total: candidates.len(),
        input_sources: count_sources(candidates.iter()),
        ..CompletionPipelineMetrics::default()
    };

    let mut kept = dedup_candidates(candidates);
    metrics.after_dedup_total = kept.len();

    kept.sort_by(|a, b| {
        b.evidence
            .score
            .cmp(&a.evidence.score)
            .then_with(|| a.name.cmp(&b.name))
    });
    let display_order: Vec<String> = kept
        .iter()
        .map(|candidate| candidate.name.clone())
        .collect();
    let shadow_order = display_order.clone();
    metrics.shadow = Some(compare_shadow_ranks(&display_order, &shadow_order));

    kept.truncate(limit);
    metrics.returned_total = kept.len();
    metrics.returned_sources = count_sources(kept.iter());

    CompletionPipelineOutput {
        items: kept,
        metrics,
    }
}

#[allow(dead_code)]
pub(crate) fn run_evidence_aware_pipeline<T>(
    candidates: Vec<PipelineCandidate<T>>,
    limit: usize,
) -> CompletionPipelineOutput<T> {
    run_evidence_aware_pipeline_with_context(candidates, limit, CompletionRankContext::default())
}

#[allow(dead_code)]
pub(crate) fn run_evidence_aware_pipeline_with_context<T>(
    candidates: Vec<PipelineCandidate<T>>,
    limit: usize,
    context: CompletionRankContext,
) -> CompletionPipelineOutput<T> {
    let mut metrics = CompletionPipelineMetrics {
        input_total: candidates.len(),
        input_sources: count_sources(candidates.iter()),
        intent_kind: context.intent.kind,
        intent_confidence: context.intent.confidence,
        history_enabled: context.history_enabled,
        ..CompletionPipelineMetrics::default()
    };

    let mut kept = merge_candidates(candidates);
    metrics.after_dedup_total = kept.len();
    let shadow_order = compatibility_order(&kept);

    let mut guarded_low_trust = 0;
    let mut history_boosted = 0;
    let mut history_max_boost = 0;
    for candidate in &mut kept {
        candidate.evidence.history_score =
            history_adjustment(&candidate.name, candidate.evidence, &context);
        if candidate.evidence.history_score > 0 {
            history_boosted += 1;
            history_max_boost = history_max_boost.max(candidate.evidence.history_score);
        }
        let rank = final_rank_score(candidate.evidence, &context);
        if is_guarded_low_trust(candidate.evidence) {
            guarded_low_trust += 1;
            let cap = LOW_TRUST_GLOBAL_TEXT_CAP_BELOW_REACHABLE
                + candidate
                    .evidence
                    .project_score
                    .clamp(0, PROJECT_CONTEXT_MAX_BOOST);
            candidate.evidence.score = rank.min(cap);
        } else {
            candidate.evidence.score = rank;
        }
    }
    metrics.final_rank = FinalRankSummary { guarded_low_trust };
    metrics.history_boosted = history_boosted;
    metrics.history_max_boost = history_max_boost;

    kept.sort_by(|a, b| {
        b.evidence
            .score
            .cmp(&a.evidence.score)
            .then_with(|| {
                b.evidence
                    .primary_source
                    .priority()
                    .cmp(&a.evidence.primary_source.priority())
            })
            .then_with(|| b.evidence.match_score.cmp(&a.evidence.match_score))
            .then_with(|| a.name.chars().count().cmp(&b.name.chars().count()))
            .then_with(|| a.name.cmp(&b.name))
    });
    let display_order: Vec<String> = kept
        .iter()
        .map(|candidate| candidate.name.clone())
        .collect();
    metrics.shadow = Some(compare_shadow_ranks(&display_order, &shadow_order));

    kept.truncate(limit);
    metrics.returned_total = kept.len();
    metrics.returned_sources = count_sources(kept.iter());

    CompletionPipelineOutput {
        items: kept,
        metrics,
    }
}

#[cfg(test)]
fn dedup_candidates<T>(candidates: Vec<PipelineCandidate<T>>) -> Vec<PipelineCandidate<T>> {
    let mut best_by_name: HashMap<String, usize> = HashMap::new();
    let mut survivors: Vec<Option<PipelineCandidate<T>>> =
        candidates.into_iter().map(Some).collect();
    for i in 0..survivors.len() {
        let Some((name, evidence)) = survivors[i]
            .as_ref()
            .map(|candidate| (candidate.name.clone(), candidate.evidence))
        else {
            continue;
        };
        match best_by_name.get(&name) {
            None => {
                best_by_name.insert(name, i);
            }
            Some(&prev_i) => {
                let prev_evidence = survivors[prev_i]
                    .as_ref()
                    .expect("survivor present")
                    .evidence;
                if candidate_beats(evidence, prev_evidence) {
                    survivors[prev_i] = None;
                    best_by_name.insert(name, i);
                } else {
                    survivors[i] = None;
                }
            }
        }
    }
    survivors.into_iter().flatten().collect()
}

#[allow(dead_code)]
fn merge_candidates<T>(candidates: Vec<PipelineCandidate<T>>) -> Vec<PipelineCandidate<T>> {
    let mut best_by_name: HashMap<String, usize> = HashMap::new();
    let mut survivors: Vec<Option<PipelineCandidate<T>>> =
        candidates.into_iter().map(Some).collect();
    for i in 0..survivors.len() {
        let Some((name, evidence)) = survivors[i]
            .as_ref()
            .map(|candidate| (candidate.name.clone(), candidate.evidence))
        else {
            continue;
        };
        match best_by_name.get(&name) {
            None => {
                best_by_name.insert(name, i);
            }
            Some(&prev_i) => {
                let prev_evidence = survivors[prev_i]
                    .as_ref()
                    .expect("survivor present")
                    .evidence;
                if candidate_beats(evidence, prev_evidence) {
                    let previous = survivors[prev_i].take().expect("survivor present");
                    let winner = survivors[i].as_mut().expect("current survivor present");
                    winner.evidence.merge_from(previous.evidence);
                    best_by_name.insert(name, i);
                } else {
                    let current = survivors[i].take().expect("current survivor present");
                    let winner = survivors[prev_i].as_mut().expect("survivor present");
                    winner.evidence.merge_from(current.evidence);
                }
            }
        }
    }
    survivors.into_iter().flatten().collect()
}

#[allow(dead_code)]
fn compatibility_order<T>(candidates: &[PipelineCandidate<T>]) -> Vec<String> {
    let mut ranks: Vec<_> = candidates
        .iter()
        .map(|candidate| (candidate.name.as_str(), candidate.evidence.score))
        .collect();
    ranks.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    ranks
        .into_iter()
        .map(|(name, _)| name.to_string())
        .collect()
}

#[allow(dead_code)]
fn final_rank_score(evidence: CandidateEvidence, context: &CompletionRankContext) -> i32 {
    source_prior(evidence.primary_source)
        + scope_prior(evidence.tier)
        + confidence_prior(evidence.confidence)
        + evidence.match_score
        + evidence.proximity_score.clamp(0, PROXIMITY_SCORE_CAP)
        + evidence.project_score.clamp(0, PROJECT_CONTEXT_MAX_BOOST)
        + intent_adjustment(evidence, context.intent)
        + evidence.history_score
}

fn history_adjustment(
    label: &str,
    evidence: CandidateEvidence,
    context: &CompletionRankContext,
) -> i32 {
    if !context.history_enabled {
        return 0;
    }
    if evidence.history_key.is_none() {
        return 0;
    }
    let history_key = candidate_hash_key(label, evidence.kind.as_history_kind_str());
    let accepts = context.history.accept_count(
        history_key,
        evidence.kind.as_history_kind_str(),
        context.intent.kind.as_summary_str(),
        &context.prefix_bucket,
    );
    ((accepts as i32) * HISTORY_REPEAT_STEP).min(HISTORY_MAX_BOOST)
}

fn intent_adjustment(evidence: CandidateEvidence, intent: CompletionIntent) -> i32 {
    if intent.kind == CompletionIntentKind::Neutral {
        return 0;
    }
    let adjustment = match intent.kind {
        CompletionIntentKind::Neutral => 0,
        CompletionIntentKind::TypeName => match evidence.kind {
            CompletionCandidateKind::Type | CompletionCandidateKind::EnumConstant => {
                INTENT_STRONG_MATCH + INTENT_MEDIUM_MATCH
            }
            CompletionCandidateKind::Keyword => INTENT_MEDIUM_MATCH,
            CompletionCandidateKind::Variable
            | CompletionCandidateKind::Function
            | CompletionCandidateKind::Macro => INTENT_BOUNDED_DEMOTION,
            CompletionCandidateKind::Unknown | CompletionCandidateKind::Text => 0,
        },
        CompletionIntentKind::ExpressionValue => match evidence.kind {
            CompletionCandidateKind::Variable
            | CompletionCandidateKind::Function
            | CompletionCandidateKind::Macro
            | CompletionCandidateKind::EnumConstant => INTENT_STRONG_MATCH,
            CompletionCandidateKind::Type | CompletionCandidateKind::Keyword => {
                INTENT_BOUNDED_DEMOTION
            }
            CompletionCandidateKind::Unknown | CompletionCandidateKind::Text => 0,
        },
        CompletionIntentKind::CallTarget => match evidence.kind {
            CompletionCandidateKind::Function | CompletionCandidateKind::Macro => {
                INTENT_STRONG_MATCH
            }
            CompletionCandidateKind::Variable => INTENT_BOUNDED_DEMOTION,
            _ => 0,
        },
        CompletionIntentKind::MacroPreprocessor => match evidence.kind {
            CompletionCandidateKind::Macro => INTENT_STRONG_MATCH,
            CompletionCandidateKind::Keyword => INTENT_MEDIUM_MATCH,
            CompletionCandidateKind::Type | CompletionCandidateKind::Variable => {
                INTENT_BOUNDED_DEMOTION
            }
            _ => 0,
        },
        CompletionIntentKind::DeclarationName => {
            if evidence.sources.has_strong_current_or_local()
                || evidence.primary_source == CandidateSource::LocalWord
            {
                INTENT_MEDIUM_MATCH
            } else if evidence.primary_source == CandidateSource::LanguageBuiltin
                || (evidence.primary_source == CandidateSource::Indexed
                    && evidence.tier == ScopeTier::Global)
            {
                DECLARATION_GLOBAL_REUSE_DEMOTION
            } else {
                0
            }
        }
    };
    match intent.confidence {
        CompletionIntentConfidence::High => adjustment,
        CompletionIntentConfidence::Medium => adjustment / 2,
        CompletionIntentConfidence::Low => adjustment / 4,
    }
}

#[allow(dead_code)]
fn is_guarded_low_trust(evidence: CandidateEvidence) -> bool {
    evidence.tier == ScopeTier::Global
        && !evidence.sources.has_strong_current_or_local()
        && (evidence.sources.local_word || evidence.confidence == ResolutionConfidence::Fallback)
}

#[allow(dead_code)]
fn source_prior(source: CandidateSource) -> i32 {
    match source {
        CandidateSource::LocalBinding => SOURCE_LOCAL_BINDING,
        CandidateSource::CurrentFileOverlay => SOURCE_CURRENT_FILE_OVERLAY,
        CandidateSource::Indexed => SOURCE_INDEXED,
        CandidateSource::LanguageBuiltin => SOURCE_LANGUAGE_BUILTIN,
        CandidateSource::LocalWord => SOURCE_LOCAL_WORD,
    }
}

#[allow(dead_code)]
fn scope_prior(tier: ScopeTier) -> i32 {
    match tier {
        ScopeTier::Current => SCOPE_CURRENT,
        ScopeTier::Reachable => SCOPE_REACHABLE,
        ScopeTier::External => SCOPE_EXTERNAL,
        ScopeTier::Unknown => SCOPE_UNKNOWN,
        ScopeTier::Global => SCOPE_GLOBAL,
    }
}

#[allow(dead_code)]
fn confidence_prior(confidence: ResolutionConfidence) -> i32 {
    match confidence {
        ResolutionConfidence::Exact => CONFIDENCE_EXACT,
        ResolutionConfidence::Reachable => CONFIDENCE_REACHABLE,
        ResolutionConfidence::Heuristic => CONFIDENCE_HEURISTIC,
        ResolutionConfidence::Ambiguous => CONFIDENCE_AMBIGUOUS,
        ResolutionConfidence::Fallback => CONFIDENCE_FALLBACK,
    }
}

fn count_sources<'a, T: 'a>(
    candidates: impl IntoIterator<Item = &'a PipelineCandidate<T>>,
) -> SourceCounts {
    let mut counts = SourceCounts::default();
    for candidate in candidates {
        counts.increment(candidate.evidence.primary_source);
    }
    counts
}

pub(crate) fn compare_shadow_ranks(display: &[String], shadow: &[String]) -> ShadowRankSummary {
    let shadow_ranks: HashMap<&str, usize> = shadow
        .iter()
        .enumerate()
        .map(|(idx, name)| (name.as_str(), idx))
        .collect();
    let mut moved_names = HashSet::new();
    let mut max_delta = 0;
    for (display_idx, name) in display.iter().enumerate() {
        if let Some(shadow_idx) = shadow_ranks.get(name.as_str()) {
            let delta = display_idx.abs_diff(*shadow_idx);
            if delta > 0 {
                moved_names.insert(name.as_str());
                max_delta = max_delta.max(delta);
            }
        }
    }
    ShadowRankSummary {
        moved: moved_names.len(),
        max_delta,
    }
}

pub(crate) fn completion_perf_summary(
    prefix: &str,
    memo_hit: &str,
    timings: &CompletionStageTimings,
    metrics: &CompletionPipelineMetrics,
) -> String {
    let shadow = metrics.shadow.unwrap_or_default();
    format!(
        "[perf] completion total={}ms context={}ms recall={}ms merge_rank={}ms render={}ms prefix_len={} hit={} intent={} intent_confidence={} history_enabled={} history_boosted={} history_max_boost={} candidates_in={} after_dedup={} returned={} indexed={} local_binding={} current_file_overlay={} language_builtin={} local_word={} returned_indexed={} returned_local_binding={} returned_current_file_overlay={} returned_language_builtin={} returned_local_word={} recall_reachable={} recall_external={} recall_unknown={} recall_global={} recall_same_project={} recall_pool={} guarded_low_trust={} shadow_moved={} shadow_max_delta={}",
        timings.total_ms,
        timings.context_ms,
        timings.recall_ms,
        timings.merge_rank_ms,
        timings.render_ms,
        prefix.chars().count(),
        memo_hit,
        metrics.intent_kind.as_summary_str(),
        metrics.intent_confidence.as_summary_str(),
        metrics.history_enabled,
        metrics.history_boosted,
        metrics.history_max_boost,
        metrics.input_total,
        metrics.after_dedup_total,
        metrics.returned_total,
        metrics.input_sources.indexed,
        metrics.input_sources.local_binding,
        metrics.input_sources.current_file_overlay,
        metrics.input_sources.language_builtin,
        metrics.input_sources.local_word,
        metrics.returned_sources.indexed,
        metrics.returned_sources.local_binding,
        metrics.returned_sources.current_file_overlay,
        metrics.returned_sources.language_builtin,
        metrics.returned_sources.local_word,
        metrics.recall_channels.reachable,
        metrics.recall_channels.external,
        metrics.recall_channels.unknown,
        metrics.recall_channels.global,
        metrics.recall_channels.same_project,
        metrics.recall_channels.pool_total,
        metrics.final_rank.guarded_low_trust,
        shadow.moved,
        shadow.max_delta,
    )
}
