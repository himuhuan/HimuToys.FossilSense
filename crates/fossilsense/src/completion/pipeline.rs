use std::collections::{HashMap, HashSet};

use crate::completion_history::{candidate_hash_key, CompletionHistorySnapshot};
use crate::model::{ResolutionConfidence, ScopeTier};

use super::prefix_ranking::{compare_name_match, CompletionPrefixRanking};
use super::{CompletionIntent, CompletionIntentConfidence, CompletionIntentKind};

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
pub(crate) const PROJECT_CONTEXT_MAX_BOOST: i32 = 350;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompletionCandidateKind {
    Unknown,
    Keyword,
    Function,
    Macro,
    Type,
    Variable,
    EnumConstant,
    Text,
}

impl CompletionCandidateKind {
    fn priority(self) -> u8 {
        match self {
            CompletionCandidateKind::Unknown => 0,
            CompletionCandidateKind::Text => 1,
            CompletionCandidateKind::Keyword => 2,
            CompletionCandidateKind::Variable => 2,
            CompletionCandidateKind::EnumConstant => 3,
            CompletionCandidateKind::Macro => 4,
            CompletionCandidateKind::Function => 5,
            CompletionCandidateKind::Type => 5,
        }
    }

    pub(crate) fn as_history_kind_str(self) -> &'static str {
        match self {
            CompletionCandidateKind::Unknown => "unknown",
            CompletionCandidateKind::Keyword => "keyword",
            CompletionCandidateKind::Function => "function",
            CompletionCandidateKind::Macro => "macro",
            CompletionCandidateKind::Type => "type",
            CompletionCandidateKind::Variable => "variable",
            CompletionCandidateKind::EnumConstant => "enum_constant",
            CompletionCandidateKind::Text => "text",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CompletionRankContext {
    pub intent: CompletionIntent,
    pub history_enabled: bool,
    pub history: CompletionHistorySnapshot,
    pub prefix_bucket: String,
    pub prefix: String,
    pub prefix_ranking: CompletionPrefixRanking,
}

impl CompletionRankContext {
    #[allow(dead_code)]
    pub(crate) fn for_intent(
        kind: CompletionIntentKind,
        confidence: CompletionIntentConfidence,
    ) -> Self {
        Self {
            intent: CompletionIntent { kind, confidence },
            ..Self::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CandidateSource {
    Indexed,
    LocalBinding,
    #[allow(dead_code)]
    CurrentFileOverlay,
    LanguageBuiltin,
    LocalWord,
}

impl CandidateSource {
    fn priority(self) -> u8 {
        match self {
            CandidateSource::LocalBinding => 5,
            CandidateSource::CurrentFileOverlay => 4,
            CandidateSource::Indexed => 3,
            CandidateSource::LanguageBuiltin => 2,
            CandidateSource::LocalWord => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct EvidenceSources {
    pub indexed: bool,
    pub local_binding: bool,
    pub current_file_overlay: bool,
    pub language_builtin: bool,
    pub local_word: bool,
}

impl EvidenceSources {
    fn single(source: CandidateSource) -> Self {
        let mut sources = Self::default();
        sources.add(source);
        sources
    }

    fn add(&mut self, source: CandidateSource) {
        match source {
            CandidateSource::Indexed => self.indexed = true,
            CandidateSource::LocalBinding => self.local_binding = true,
            CandidateSource::CurrentFileOverlay => self.current_file_overlay = true,
            CandidateSource::LanguageBuiltin => self.language_builtin = true,
            CandidateSource::LocalWord => self.local_word = true,
        }
    }

    #[allow(dead_code)]
    fn merge(&mut self, other: EvidenceSources) {
        self.indexed |= other.indexed;
        self.local_binding |= other.local_binding;
        self.current_file_overlay |= other.current_file_overlay;
        self.language_builtin |= other.language_builtin;
        self.local_word |= other.local_word;
    }

    #[allow(dead_code)]
    fn has_strong_current_or_local(self) -> bool {
        self.local_binding || self.current_file_overlay
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CandidateEvidence {
    pub primary_source: CandidateSource,
    pub sources: EvidenceSources,
    /// Compatibility alias for existing callers during the evidence migration.
    pub source: CandidateSource,
    pub tier: ScopeTier,
    pub confidence: ResolutionConfidence,
    pub score: i32,
    pub match_score: i32,
    pub locality_score: i32,
    pub proximity_score: i32,
    pub project_score: i32,
    pub kind: CompletionCandidateKind,
    pub history_key: Option<u64>,
    pub history_score: i32,
}

impl CandidateEvidence {
    pub(crate) fn new(
        source: CandidateSource,
        tier: ScopeTier,
        confidence: ResolutionConfidence,
        score: i32,
    ) -> Self {
        Self {
            primary_source: source,
            sources: EvidenceSources::single(source),
            source,
            tier,
            confidence,
            score,
            match_score: score,
            locality_score: 0,
            proximity_score: 0,
            project_score: 0,
            kind: CompletionCandidateKind::Unknown,
            history_key: None,
            history_score: 0,
        }
    }

    #[allow(dead_code)]
    fn merge_from(&mut self, other: CandidateEvidence) {
        let same_primary_source = self.primary_source == other.primary_source;
        self.sources.merge(other.sources);
        if candidate_beats(other, *self) {
            self.primary_source = other.primary_source;
            self.source = other.primary_source;
        }
        self.tier = self.tier.max(other.tier);
        self.confidence = self.confidence.max(other.confidence);
        self.score = self.score.max(other.score);
        self.match_score = self.match_score.max(other.match_score);
        self.locality_score = self.locality_score.max(other.locality_score);
        self.proximity_score = self.proximity_score.max(other.proximity_score);
        self.project_score = self.project_score.max(other.project_score);
        // Same-source duplicates already chose their presentation winner;
        // cross-source merges retain the established structured-kind upgrade.
        if !same_primary_source && other.kind.priority() > self.kind.priority() {
            self.kind = other.kind;
        }
        if self.history_key.is_none() {
            self.history_key = other.history_key;
        }
        self.history_score = self.history_score.max(other.history_score);
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PipelineCandidate<T> {
    pub name: String,
    pub evidence: CandidateEvidence,
    pub payload: T,
}

impl<T> PipelineCandidate<T> {
    pub(crate) fn new(name: impl Into<String>, evidence: CandidateEvidence, payload: T) -> Self {
        Self {
            name: name.into(),
            evidence,
            payload,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SourceCounts {
    pub indexed: usize,
    pub local_binding: usize,
    pub current_file_overlay: usize,
    pub language_builtin: usize,
    pub local_word: usize,
}

impl SourceCounts {
    fn increment(&mut self, source: CandidateSource) {
        match source {
            CandidateSource::Indexed => self.indexed += 1,
            CandidateSource::LocalBinding => self.local_binding += 1,
            CandidateSource::CurrentFileOverlay => self.current_file_overlay += 1,
            CandidateSource::LanguageBuiltin => self.language_builtin += 1,
            CandidateSource::LocalWord => self.local_word += 1,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ShadowRankSummary {
    pub moved: usize,
    pub max_delta: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FinalRankSummary {
    pub guarded_low_trust: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CompletionPipelineMetrics {
    pub input_total: usize,
    pub after_dedup_total: usize,
    pub returned_total: usize,
    pub input_sources: SourceCounts,
    pub returned_sources: SourceCounts,
    pub final_rank: FinalRankSummary,
    pub shadow: Option<ShadowRankSummary>,
    pub intent_kind: CompletionIntentKind,
    pub intent_confidence: CompletionIntentConfidence,
    pub history_enabled: bool,
    pub history_boosted: usize,
    pub history_max_boost: i32,
    pub project_boosted: usize,
    pub project_max_boost: i32,
    pub recall_channels: crate::query::CompletionRecallMetrics,
}

impl Default for CompletionPipelineMetrics {
    fn default() -> Self {
        Self {
            input_total: 0,
            after_dedup_total: 0,
            returned_total: 0,
            input_sources: SourceCounts::default(),
            returned_sources: SourceCounts::default(),
            final_rank: FinalRankSummary::default(),
            shadow: None,
            intent_kind: CompletionIntentKind::Neutral,
            intent_confidence: CompletionIntentConfidence::Low,
            history_enabled: false,
            history_boosted: 0,
            history_max_boost: 0,
            project_boosted: 0,
            project_max_boost: 0,
            recall_channels: crate::query::CompletionRecallMetrics::default(),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CompletionStageTimings {
    pub total_ms: u128,
    pub context_ms: u128,
    pub recall_ms: u128,
    pub merge_rank_ms: u128,
    pub render_ms: u128,
}

#[derive(Debug)]
pub(crate) struct CompletionPipelineOutput<T> {
    pub items: Vec<PipelineCandidate<T>>,
    pub metrics: CompletionPipelineMetrics,
}

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
    let mut project_boosted = 0;
    let mut project_max_boost = 0;
    for candidate in &mut kept {
        candidate.evidence.history_score =
            history_adjustment(&candidate.name, candidate.evidence, &context);
        if candidate.evidence.history_score > 0 {
            history_boosted += 1;
            history_max_boost = history_max_boost.max(candidate.evidence.history_score);
        }
        if candidate.evidence.project_score > 0 {
            project_boosted += 1;
            project_max_boost = project_max_boost.max(candidate.evidence.project_score);
        }
        let rank = final_rank_score(candidate.evidence, &context);
        if is_guarded_low_trust(candidate.evidence) {
            guarded_low_trust += 1;
            // Keep the bounded project distinction below reachable evidence;
            // zero project evidence preserves the exact pre-feature cap.
            let project_cap = candidate
                .evidence
                .project_score
                .clamp(0, PROJECT_CONTEXT_MAX_BOOST);
            let cap = LOW_TRUST_GLOBAL_TEXT_CAP_BELOW_REACHABLE + project_cap;
            candidate.evidence.score = rank.min(cap);
        } else {
            candidate.evidence.score = rank;
        }
    }
    metrics.final_rank = FinalRankSummary { guarded_low_trust };
    metrics.history_boosted = history_boosted;
    metrics.history_max_boost = history_max_boost;
    metrics.project_boosted = project_boosted;
    metrics.project_max_boost = project_max_boost;

    kept.sort_by(|a, b| {
        compare_name_match(&context.prefix, context.prefix_ranking, &a.name, &b.name)
            .then_with(|| b.evidence.score.cmp(&a.evidence.score))
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

fn candidate_beats(current: CandidateEvidence, previous: CandidateEvidence) -> bool {
    let rank = current.primary_source.priority();
    let prev_rank = previous.primary_source.priority();
    rank > prev_rank
        || (rank == prev_rank
            && ((current.tier, current.confidence) > (previous.tier, previous.confidence)
                || ((current.tier, current.confidence) == (previous.tier, previous.confidence)
                    && (current.project_score > previous.project_score
                        || (current.project_score == previous.project_score
                            && current.score > previous.score)))))
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
    if !context.history_enabled || evidence.history_key.is_none() {
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
    document_version: i32,
    engine_generation: u64,
    timings: &CompletionStageTimings,
    metrics: &CompletionPipelineMetrics,
) -> String {
    let shadow = metrics.shadow.unwrap_or_default();
    format!(
        "[perf] completion total={}ms context={}ms recall={}ms merge_rank={}ms render={}ms document_version={} engine_generation={} prefix_len={} hit={} intent={} intent_confidence={} history_enabled={} history_boosted={} history_max_boost={} project_boosted={} project_max_boost={} candidates_in={} after_dedup={} returned={} indexed={} local_binding={} current_file_overlay={} language_builtin={} local_word={} returned_indexed={} returned_local_binding={} returned_current_file_overlay={} returned_language_builtin={} returned_local_word={} recall_reachable={} recall_external={} recall_unknown={} recall_global={} recall_same_project={} recall_pool={} guarded_low_trust={} shadow_moved={} shadow_max_delta={}",
        timings.total_ms,
        timings.context_ms,
        timings.recall_ms,
        timings.merge_rank_ms,
        timings.render_ms,
        document_version,
        engine_generation,
        prefix.chars().count(),
        memo_hit,
        metrics.intent_kind.as_summary_str(),
        metrics.intent_confidence.as_summary_str(),
        metrics.history_enabled,
        metrics.history_boosted,
        metrics.history_max_boost,
        metrics.project_boosted,
        metrics.project_max_boost,
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
