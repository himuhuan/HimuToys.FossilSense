use std::collections::{HashMap, HashSet};

use crate::completion_history::{candidate_hash_key, CompletionHistorySnapshot};
use crate::model::{ResolutionConfidence, ScopeTier};

#[allow(dead_code)]
const SOURCE_LOCAL_BINDING: i32 = 12_000;
#[allow(dead_code)]
const SOURCE_CURRENT_FILE_OVERLAY: i32 = 9_000;
#[allow(dead_code)]
const SOURCE_INDEXED: i32 = 5_000;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompletionIntentKind {
    Neutral,
    TypeName,
    ExpressionValue,
    CallTarget,
    MacroPreprocessor,
    DeclarationName,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum CompletionIntentConfidence {
    Low,
    Medium,
    High,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CompletionIntent {
    pub kind: CompletionIntentKind,
    pub confidence: CompletionIntentConfidence,
}

impl Default for CompletionIntent {
    fn default() -> Self {
        Self {
            kind: CompletionIntentKind::Neutral,
            confidence: CompletionIntentConfidence::Low,
        }
    }
}

impl CompletionIntentKind {
    pub(crate) fn as_summary_str(self) -> &'static str {
        match self {
            CompletionIntentKind::Neutral => "neutral",
            CompletionIntentKind::TypeName => "type_name",
            CompletionIntentKind::ExpressionValue => "expression_value",
            CompletionIntentKind::CallTarget => "call_target",
            CompletionIntentKind::MacroPreprocessor => "macro_preprocessor",
            CompletionIntentKind::DeclarationName => "declaration_name",
        }
    }
}

impl CompletionIntentConfidence {
    fn as_summary_str(self) -> &'static str {
        match self {
            CompletionIntentConfidence::Low => "low",
            CompletionIntentConfidence::Medium => "medium",
            CompletionIntentConfidence::High => "high",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompletionCandidateKind {
    Unknown,
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
            CompletionCandidateKind::Function => "function",
            CompletionCandidateKind::Macro => "macro",
            CompletionCandidateKind::Type => "type",
            CompletionCandidateKind::Variable => "variable",
            CompletionCandidateKind::EnumConstant => "enum_constant",
            CompletionCandidateKind::Text => "text",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompletionRankContext {
    pub intent: CompletionIntent,
    pub history_enabled: bool,
    pub history: CompletionHistorySnapshot,
    pub prefix_bucket: String,
}

impl CompletionRankContext {
    #[allow(dead_code)]
    pub(crate) fn for_intent(
        kind: CompletionIntentKind,
        confidence: CompletionIntentConfidence,
    ) -> Self {
        Self {
            intent: CompletionIntent { kind, confidence },
            history_enabled: false,
            history: CompletionHistorySnapshot::default(),
            prefix_bucket: String::new(),
        }
    }
}

impl Default for CompletionRankContext {
    fn default() -> Self {
        Self {
            intent: CompletionIntent::default(),
            history_enabled: false,
            history: CompletionHistorySnapshot::default(),
            prefix_bucket: String::new(),
        }
    }
}

pub(crate) fn classify_completion_intent(
    line_text: &str,
    character: u32,
    prefix: &str,
) -> CompletionIntent {
    let cursor = byte_index_for_utf16_position(line_text, character);
    let before_cursor = &line_text[..cursor];
    let after_cursor = &line_text[cursor..];
    let before_prefix = before_cursor
        .strip_suffix(prefix)
        .unwrap_or(before_cursor)
        .trim_end();
    let trimmed_before = before_cursor.trim_start();

    if is_preprocessor_macro_context(trimmed_before) {
        return CompletionIntent {
            kind: CompletionIntentKind::MacroPreprocessor,
            confidence: CompletionIntentConfidence::High,
        };
    }

    if after_cursor.trim_start().starts_with('(') {
        return CompletionIntent {
            kind: CompletionIntentKind::CallTarget,
            confidence: CompletionIntentConfidence::High,
        };
    }

    let previous_token = previous_token(before_prefix);
    if previous_token.as_deref().is_some_and(is_type_intent_cue) {
        return CompletionIntent {
            kind: CompletionIntentKind::TypeName,
            confidence: CompletionIntentConfidence::High,
        };
    }

    if previous_token
        .as_deref()
        .is_some_and(is_pointer_or_reference)
        && typeish_token_before_pointer_or_reference(before_prefix)
    {
        return CompletionIntent {
            kind: CompletionIntentKind::DeclarationName,
            confidence: CompletionIntentConfidence::Medium,
        };
    }

    if previous_token
        .as_deref()
        .is_some_and(is_expression_intent_cue)
    {
        return CompletionIntent {
            kind: CompletionIntentKind::ExpressionValue,
            confidence: CompletionIntentConfidence::Medium,
        };
    }

    if previous_token
        .as_deref()
        .is_some_and(is_typeish_declaration_token)
    {
        return CompletionIntent {
            kind: CompletionIntentKind::DeclarationName,
            confidence: CompletionIntentConfidence::Medium,
        };
    }

    CompletionIntent::default()
}

fn byte_index_for_utf16_position(text: &str, character: u32) -> usize {
    let mut utf16_units = 0;
    for (byte_idx, ch) in text.char_indices() {
        if utf16_units >= character {
            return byte_idx;
        }
        utf16_units += ch.len_utf16() as u32;
    }
    text.len()
}

fn is_preprocessor_macro_context(trimmed_before: &str) -> bool {
    let Some(rest) = trimmed_before.strip_prefix('#') else {
        return false;
    };
    let directive = rest.split_whitespace().next().unwrap_or_default();
    matches!(directive, "if" | "ifdef" | "ifndef" | "elif" | "define")
}

fn previous_token(before_prefix: &str) -> Option<String> {
    let trimmed = before_prefix.trim_end();
    if trimmed.is_empty() {
        return None;
    }
    let end = trimmed.len();
    while end > 0 {
        let ch = trimmed[..end].chars().next_back()?;
        if ch.is_alphanumeric() || ch == '_' || ch == '*' || ch == '&' {
            break;
        }
        return Some(ch.to_string());
    }
    let mut start = end;
    while start > 0 {
        let ch = trimmed[..start].chars().next_back()?;
        if ch.is_alphanumeric() || ch == '_' || ch == '*' || ch == '&' {
            start -= ch.len_utf8();
        } else {
            break;
        }
    }
    Some(trimmed[start..end].to_string())
}

fn is_type_intent_cue(token: &str) -> bool {
    matches!(
        token,
        "struct" | "union" | "enum" | "class" | "typedef" | "using" | "sizeof" | "new"
    )
}

fn is_expression_intent_cue(token: &str) -> bool {
    matches!(
        token,
        "=" | "return"
            | "("
            | "["
            | ","
            | "?"
            | ":"
            | "+"
            | "-"
            | "*"
            | "/"
            | "%"
            | "&"
            | "|"
            | "!"
            | "<"
            | ">"
    )
}

fn is_typeish_declaration_token(token: &str) -> bool {
    let trimmed = token.trim_matches(|ch| ch == '*' || ch == '&');
    trimmed
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_uppercase())
        || matches!(
            trimmed,
            "int"
                | "char"
                | "short"
                | "long"
                | "float"
                | "double"
                | "bool"
                | "void"
                | "size_t"
                | "uint8_t"
                | "uint16_t"
                | "uint32_t"
                | "uint64_t"
                | "int8_t"
                | "int16_t"
                | "int32_t"
                | "int64_t"
        )
}

fn is_pointer_or_reference(token: &str) -> bool {
    token.chars().all(|ch| ch == '*' || ch == '&')
}

fn typeish_token_before_pointer_or_reference(before_prefix: &str) -> bool {
    let mut trimmed = before_prefix.trim_end();
    while let Some(ch) = trimmed.chars().next_back() {
        if ch == '*' || ch == '&' || ch.is_whitespace() {
            trimmed = &trimmed[..trimmed.len() - ch.len_utf8()];
        } else {
            break;
        }
    }
    previous_token(trimmed)
        .as_deref()
        .is_some_and(is_typeish_declaration_token)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CandidateSource {
    Indexed,
    LocalBinding,
    #[allow(dead_code)]
    CurrentFileOverlay,
    LocalWord,
}

impl CandidateSource {
    fn priority(self) -> u8 {
        match self {
            CandidateSource::LocalBinding => 4,
            CandidateSource::CurrentFileOverlay => 3,
            CandidateSource::Indexed => 2,
            CandidateSource::LocalWord => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct EvidenceSources {
    pub indexed: bool,
    pub local_binding: bool,
    pub current_file_overlay: bool,
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
            CandidateSource::LocalWord => self.local_word = true,
        }
    }

    #[allow(dead_code)]
    fn merge(&mut self, other: EvidenceSources) {
        self.indexed |= other.indexed;
        self.local_binding |= other.local_binding;
        self.current_file_overlay |= other.current_file_overlay;
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
    /// Compatibility alias for existing callers during the Phase 2 migration.
    pub source: CandidateSource,
    pub tier: ScopeTier,
    pub confidence: ResolutionConfidence,
    pub score: i32,
    pub match_score: i32,
    pub locality_score: i32,
    pub proximity_score: i32,
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
            kind: CompletionCandidateKind::Unknown,
            history_key: None,
            history_score: 0,
        }
    }

    #[allow(dead_code)]
    fn merge_from(&mut self, other: CandidateEvidence) {
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
        if other.kind.priority() > self.kind.priority() {
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
    pub local_word: usize,
}

impl SourceCounts {
    fn increment(&mut self, source: CandidateSource) {
        match source {
            CandidateSource::Indexed => self.indexed += 1,
            CandidateSource::LocalBinding => self.local_binding += 1,
            CandidateSource::CurrentFileOverlay => self.current_file_overlay += 1,
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
            candidate.evidence.score = rank.min(LOW_TRUST_GLOBAL_TEXT_CAP_BELOW_REACHABLE);
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

fn candidate_beats(current: CandidateEvidence, previous: CandidateEvidence) -> bool {
    let rank = current.primary_source.priority();
    let prev_rank = previous.primary_source.priority();
    rank > prev_rank
        || (rank == prev_rank
            && ((current.tier, current.confidence) > (previous.tier, previous.confidence)
                || ((current.tier, current.confidence) == (previous.tier, previous.confidence)
                    && current.score > previous.score)))
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
            CompletionCandidateKind::Type => INTENT_BOUNDED_DEMOTION,
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
            } else if evidence.primary_source == CandidateSource::Indexed
                && evidence.tier == ScopeTier::Global
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
        "[perf] completion total={}ms context={}ms recall={}ms merge_rank={}ms render={}ms prefix_len={} hit={} intent={} intent_confidence={} history_enabled={} history_boosted={} history_max_boost={} candidates_in={} after_dedup={} returned={} indexed={} local_binding={} current_file_overlay={} local_word={} returned_indexed={} returned_local_binding={} returned_current_file_overlay={} returned_local_word={} recall_reachable={} recall_external={} recall_unknown={} recall_global={} recall_pool={} guarded_low_trust={} shadow_moved={} shadow_max_delta={}",
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
        metrics.input_sources.local_word,
        metrics.returned_sources.indexed,
        metrics.returned_sources.local_binding,
        metrics.returned_sources.current_file_overlay,
        metrics.returned_sources.local_word,
        metrics.recall_channels.reachable,
        metrics.recall_channels.external,
        metrics.recall_channels.unknown,
        metrics.recall_channels.global,
        metrics.recall_channels.pool_total,
        metrics.final_rank.guarded_low_trust,
        shadow.moved,
        shadow.max_delta,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion_history::{
        candidate_hash, candidate_hash_key, CompletionHistorySnapshot,
    };

    #[test]
    fn intent_classifies_preprocessor_macro_context() {
        let intent = classify_completion_intent("#if FS_", 7, "FS_");

        assert_eq!(intent.kind, CompletionIntentKind::MacroPreprocessor);
        assert_eq!(intent.confidence, CompletionIntentConfidence::High);
    }

    #[test]
    fn intent_classifies_call_target_before_open_paren() {
        let intent = classify_completion_intent("    FS_do(", 9, "FS_do");

        assert_eq!(intent.kind, CompletionIntentKind::CallTarget);
        assert!(intent.confidence >= CompletionIntentConfidence::Medium);
    }

    #[test]
    fn intent_classifies_type_and_declaration_name_contexts() {
        let type_intent = classify_completion_intent("    struct FS_", 14, "FS_");
        assert_eq!(type_intent.kind, CompletionIntentKind::TypeName);

        let decl_intent = classify_completion_intent("    FsWidget fs_", 16, "fs_");
        assert_eq!(decl_intent.kind, CompletionIntentKind::DeclarationName);
    }

    #[test]
    fn intent_classifies_pointer_and_reference_declaration_names() {
        let pointer_intent = classify_completion_intent("    FsWidget *fs_", 17, "fs_");
        assert_eq!(pointer_intent.kind, CompletionIntentKind::DeclarationName);

        let const_pointer_intent = classify_completion_intent("    const FsWidget *fs_", 23, "fs_");
        assert_eq!(
            const_pointer_intent.kind,
            CompletionIntentKind::DeclarationName
        );

        let reference_intent = classify_completion_intent("    FsWidget &fs_", 17, "fs_");
        assert_eq!(reference_intent.kind, CompletionIntentKind::DeclarationName);
    }

    #[test]
    fn intent_degrades_for_uncertain_expression_context() {
        let intent = classify_completion_intent("    value = FS_", 15, "FS_");

        assert!(matches!(
            intent.kind,
            CompletionIntentKind::ExpressionValue | CompletionIntentKind::Neutral
        ));
        assert!(intent.confidence <= CompletionIntentConfidence::Medium);
    }

    fn candidate(
        name: &str,
        source: CandidateSource,
        tier: ScopeTier,
        score: i32,
        payload: &'static str,
    ) -> PipelineCandidate<&'static str> {
        PipelineCandidate::new(
            name,
            CandidateEvidence::new(source, tier, ResolutionConfidence::Heuristic, score),
            payload,
        )
    }

    fn candidate_with_kind(
        name: &str,
        source: CandidateSource,
        tier: ScopeTier,
        score: i32,
        kind: CompletionCandidateKind,
        payload: &'static str,
    ) -> PipelineCandidate<&'static str> {
        let mut evidence =
            CandidateEvidence::new(source, tier, ResolutionConfidence::Heuristic, score);
        evidence.kind = kind;
        PipelineCandidate::new(name, evidence, payload)
    }

    fn candidate_with_history_key(
        name: &str,
        source: CandidateSource,
        tier: ScopeTier,
        score: i32,
        kind: CompletionCandidateKind,
        kind_str: &str,
        payload: &'static str,
    ) -> PipelineCandidate<&'static str> {
        let mut evidence =
            CandidateEvidence::new(source, tier, ResolutionConfidence::Heuristic, score);
        evidence.kind = kind;
        evidence.history_key = Some(candidate_hash_key(name, kind_str));
        PipelineCandidate::new(name, evidence, payload)
    }

    #[test]
    fn history_boost_lifts_comparable_candidate_but_not_current_local() {
        let history = CompletionHistorySnapshot::from_test_accepts(vec![(
            candidate_hash("global_fn", "function"),
            "function",
            "call_target",
            "gl",
            4,
        )]);

        let output = run_evidence_aware_pipeline_with_context(
            vec![
                candidate_with_history_key(
                    "global_fn",
                    CandidateSource::Indexed,
                    ScopeTier::Global,
                    820,
                    CompletionCandidateKind::Function,
                    "function",
                    "global",
                ),
                candidate_with_history_key(
                    "local_value",
                    CandidateSource::LocalBinding,
                    ScopeTier::Current,
                    760,
                    CompletionCandidateKind::Variable,
                    "variable",
                    "local",
                ),
            ],
            10,
            CompletionRankContext {
                intent: CompletionIntent {
                    kind: CompletionIntentKind::CallTarget,
                    confidence: CompletionIntentConfidence::High,
                },
                history_enabled: true,
                history,
                prefix_bucket: "gl".to_string(),
            },
        );

        assert_eq!(output.items[0].payload, "local");
        assert!(output.metrics.history_boosted >= 1);
        assert!(output.metrics.history_max_boost > 0);
    }

    #[test]
    fn merged_candidate_history_uses_final_kind_hash() {
        let history = CompletionHistorySnapshot::from_test_accepts(vec![(
            candidate_hash("same_name", "function"),
            "function",
            "call_target",
            "sa",
            2,
        )]);

        let output = run_evidence_aware_pipeline_with_context(
            vec![
                candidate_with_history_key(
                    "same_name",
                    CandidateSource::LocalBinding,
                    ScopeTier::Current,
                    760,
                    CompletionCandidateKind::Variable,
                    "variable",
                    "local",
                ),
                candidate_with_history_key(
                    "same_name",
                    CandidateSource::Indexed,
                    ScopeTier::Reachable,
                    820,
                    CompletionCandidateKind::Function,
                    "function",
                    "indexed",
                ),
            ],
            10,
            CompletionRankContext {
                intent: CompletionIntent {
                    kind: CompletionIntentKind::CallTarget,
                    confidence: CompletionIntentConfidence::High,
                },
                history_enabled: true,
                history,
                prefix_bucket: "sa".to_string(),
            },
        );

        assert_eq!(output.items.len(), 1);
        assert_eq!(
            output.items[0].evidence.kind,
            CompletionCandidateKind::Function
        );
        assert_eq!(output.metrics.history_boosted, 1);
        assert!(output.items[0].evidence.history_score > 0);
    }

    #[test]
    fn neutral_history_context_preserves_existing_order() {
        let candidates = vec![
            candidate(
                "alpha",
                CandidateSource::Indexed,
                ScopeTier::Reachable,
                700,
                "a",
            ),
            candidate(
                "beta",
                CandidateSource::Indexed,
                ScopeTier::Global,
                900,
                "b",
            ),
        ];
        let without = run_evidence_aware_pipeline(candidates.clone(), 10);
        let disabled = run_evidence_aware_pipeline_with_context(
            candidates,
            10,
            CompletionRankContext::default(),
        );

        assert_eq!(
            without
                .items
                .iter()
                .map(|candidate| candidate.payload)
                .collect::<Vec<_>>(),
            disabled
                .items
                .iter()
                .map(|candidate| candidate.payload)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn type_intent_lifts_type_candidates_without_hiding_values() {
        let output = run_evidence_aware_pipeline_with_context(
            vec![
                candidate_with_kind(
                    "FsWidget",
                    CandidateSource::Indexed,
                    ScopeTier::Global,
                    800,
                    CompletionCandidateKind::Type,
                    "type",
                ),
                candidate_with_kind(
                    "fs_widget_value",
                    CandidateSource::Indexed,
                    ScopeTier::Reachable,
                    650,
                    CompletionCandidateKind::Variable,
                    "value",
                ),
            ],
            10,
            CompletionRankContext::for_intent(
                CompletionIntentKind::TypeName,
                CompletionIntentConfidence::High,
            ),
        );

        assert_eq!(output.items[0].payload, "type");
        assert!(output
            .items
            .iter()
            .any(|candidate| candidate.payload == "value"));
    }

    #[test]
    fn expression_intent_demotes_type_only_candidates_but_keeps_them() {
        let output = run_evidence_aware_pipeline_with_context(
            vec![
                candidate_with_kind(
                    "FsWidget",
                    CandidateSource::Indexed,
                    ScopeTier::Reachable,
                    800,
                    CompletionCandidateKind::Type,
                    "type",
                ),
                candidate_with_kind(
                    "fs_value",
                    CandidateSource::Indexed,
                    ScopeTier::Reachable,
                    760,
                    CompletionCandidateKind::Variable,
                    "value",
                ),
            ],
            10,
            CompletionRankContext::for_intent(
                CompletionIntentKind::ExpressionValue,
                CompletionIntentConfidence::High,
            ),
        );

        assert_eq!(output.items[0].payload, "value");
        assert!(output
            .items
            .iter()
            .any(|candidate| candidate.payload == "type"));
    }

    #[test]
    fn call_intent_lifts_functions() {
        let output = run_evidence_aware_pipeline_with_context(
            vec![
                candidate_with_kind(
                    "FsRun",
                    CandidateSource::Indexed,
                    ScopeTier::Reachable,
                    720,
                    CompletionCandidateKind::Function,
                    "function",
                ),
                candidate_with_kind(
                    "fs_runtime",
                    CandidateSource::Indexed,
                    ScopeTier::Reachable,
                    780,
                    CompletionCandidateKind::Variable,
                    "variable",
                ),
            ],
            10,
            CompletionRankContext::for_intent(
                CompletionIntentKind::CallTarget,
                CompletionIntentConfidence::High,
            ),
        );

        assert_eq!(output.items[0].payload, "function");
    }

    #[test]
    fn macro_preprocessor_intent_lifts_macros() {
        let output = run_evidence_aware_pipeline_with_context(
            vec![
                candidate_with_kind(
                    "FS_WIDGET",
                    CandidateSource::Indexed,
                    ScopeTier::Reachable,
                    760,
                    CompletionCandidateKind::Type,
                    "type",
                ),
                candidate_with_kind(
                    "FS_ENABLED",
                    CandidateSource::Indexed,
                    ScopeTier::Reachable,
                    720,
                    CompletionCandidateKind::Macro,
                    "macro",
                ),
            ],
            10,
            CompletionRankContext::for_intent(
                CompletionIntentKind::MacroPreprocessor,
                CompletionIntentConfidence::High,
            ),
        );

        assert_eq!(output.items[0].payload, "macro");
    }

    #[test]
    fn declaration_name_intent_reduces_global_reuse_pressure() {
        let output = run_evidence_aware_pipeline_with_context(
            vec![
                candidate_with_kind(
                    "fs_widget",
                    CandidateSource::Indexed,
                    ScopeTier::Global,
                    900,
                    CompletionCandidateKind::Variable,
                    "global",
                ),
                candidate_with_kind(
                    "fs_working_name",
                    CandidateSource::LocalWord,
                    ScopeTier::Global,
                    860,
                    CompletionCandidateKind::Text,
                    "text",
                ),
            ],
            10,
            CompletionRankContext::for_intent(
                CompletionIntentKind::DeclarationName,
                CompletionIntentConfidence::High,
            ),
        );

        assert_eq!(output.items[0].payload, "text");
        assert!(output
            .items
            .iter()
            .any(|candidate| candidate.payload == "global"));
    }

    #[test]
    fn perf_summary_reports_intent_without_candidate_names() {
        let metrics = CompletionPipelineMetrics {
            intent_kind: CompletionIntentKind::CallTarget,
            intent_confidence: CompletionIntentConfidence::High,
            ..CompletionPipelineMetrics::default()
        };
        let line =
            completion_perf_summary("fs_", "cold", &CompletionStageTimings::default(), &metrics);

        assert!(line.contains("intent=call_target"));
        assert!(line.contains("intent_confidence=high"));
        assert!(!line.contains("fs_\""));
    }

    #[test]
    fn evidence_pipeline_merges_same_name_sources() {
        let output = run_evidence_aware_pipeline(
            vec![
                candidate(
                    "Widget",
                    CandidateSource::Indexed,
                    ScopeTier::Reachable,
                    800,
                    "indexed",
                ),
                candidate(
                    "Widget",
                    CandidateSource::CurrentFileOverlay,
                    ScopeTier::Current,
                    1000,
                    "overlay",
                ),
                candidate(
                    "Widget",
                    CandidateSource::LocalWord,
                    ScopeTier::Global,
                    750,
                    "word",
                ),
            ],
            10,
        );

        assert_eq!(output.items.len(), 1);
        let evidence = &output.items[0].evidence;
        assert!(evidence.sources.indexed);
        assert!(evidence.sources.current_file_overlay);
        assert!(evidence.sources.local_word);
        assert_eq!(output.items[0].payload, "overlay");
    }

    #[test]
    fn ranker_keeps_reachable_prefix_above_plain_global_fuzzy() {
        let output = run_evidence_aware_pipeline(
            vec![
                candidate(
                    "reachable_api",
                    CandidateSource::Indexed,
                    ScopeTier::Reachable,
                    800,
                    "reach",
                ),
                candidate(
                    "api_text_tail",
                    CandidateSource::LocalWord,
                    ScopeTier::Global,
                    250,
                    "text",
                ),
            ],
            10,
        );

        assert_eq!(output.items[0].payload, "reach");
        assert_eq!(output.metrics.final_rank.guarded_low_trust, 1);
    }

    #[test]
    fn current_overlay_exact_can_outrank_reachable_weak_match() {
        let output = run_evidence_aware_pipeline(
            vec![
                candidate(
                    "new_local_type",
                    CandidateSource::CurrentFileOverlay,
                    ScopeTier::Current,
                    1000,
                    "overlay",
                ),
                candidate(
                    "newLocalTypeFactory",
                    CandidateSource::Indexed,
                    ScopeTier::Reachable,
                    400,
                    "reach",
                ),
            ],
            10,
        );

        assert_eq!(output.items[0].payload, "overlay");
    }

    #[test]
    fn evidence_perf_summary_is_source_safe_and_reports_ranker_fields() {
        let metrics = CompletionPipelineMetrics {
            input_total: 4,
            after_dedup_total: 3,
            returned_total: 3,
            input_sources: SourceCounts {
                indexed: 1,
                local_binding: 1,
                current_file_overlay: 1,
                local_word: 1,
            },
            returned_sources: SourceCounts {
                indexed: 1,
                local_binding: 1,
                current_file_overlay: 1,
                local_word: 0,
            },
            final_rank: FinalRankSummary {
                guarded_low_trust: 2,
            },
            shadow: Some(ShadowRankSummary {
                moved: 1,
                max_delta: 2,
            }),
            ..CompletionPipelineMetrics::default()
        };
        let timings = CompletionStageTimings {
            total_ms: 9,
            context_ms: 1,
            recall_ms: 2,
            merge_rank_ms: 3,
            render_ms: 1,
        };

        let line = completion_perf_summary("Widget", "miss", &timings, &metrics);

        assert!(line.contains("current_file_overlay=1"));
        assert!(line.contains("returned_current_file_overlay=1"));
        assert!(line.contains("guarded_low_trust=2"));
        assert!(!line.contains("Widget\""));
        assert!(!line.contains("reachable_api"));
        assert!(!line.contains("new_local_type"));
    }

    #[test]
    fn compatible_pipeline_preserves_score_order_and_source_priority() {
        let candidates = vec![
            PipelineCandidate::new(
                "shared",
                CandidateEvidence::new(
                    CandidateSource::Indexed,
                    ScopeTier::Reachable,
                    ResolutionConfidence::Reachable,
                    30_000,
                ),
                "indexed",
            ),
            PipelineCandidate::new(
                "zeta",
                CandidateEvidence::new(
                    CandidateSource::LocalWord,
                    ScopeTier::Global,
                    ResolutionConfidence::Fallback,
                    900,
                ),
                "word",
            ),
            PipelineCandidate::new(
                "shared",
                CandidateEvidence::new(
                    CandidateSource::LocalBinding,
                    ScopeTier::Current,
                    ResolutionConfidence::Heuristic,
                    40_000,
                ),
                "local",
            ),
            PipelineCandidate::new(
                "alpha",
                CandidateEvidence::new(
                    CandidateSource::Indexed,
                    ScopeTier::Current,
                    ResolutionConfidence::Exact,
                    40_000,
                ),
                "alpha",
            ),
        ];

        let output = run_compatible_pipeline(candidates, 10);

        assert_eq!(
            output
                .items
                .iter()
                .map(|candidate| (candidate.name.as_str(), candidate.payload))
                .collect::<Vec<_>>(),
            vec![("alpha", "alpha"), ("shared", "local"), ("zeta", "word")]
        );
    }

    #[test]
    fn pipeline_metrics_count_sources_before_and_after_dedup() {
        let candidates = vec![
            PipelineCandidate::new(
                "same",
                CandidateEvidence::new(
                    CandidateSource::Indexed,
                    ScopeTier::Reachable,
                    ResolutionConfidence::Reachable,
                    10,
                ),
                (),
            ),
            PipelineCandidate::new(
                "same",
                CandidateEvidence::new(
                    CandidateSource::LocalBinding,
                    ScopeTier::Current,
                    ResolutionConfidence::Heuristic,
                    20,
                ),
                (),
            ),
            PipelineCandidate::new(
                "word",
                CandidateEvidence::new(
                    CandidateSource::LocalWord,
                    ScopeTier::Global,
                    ResolutionConfidence::Fallback,
                    5,
                ),
                (),
            ),
        ];

        let output = run_compatible_pipeline(candidates, 10);

        assert_eq!(output.metrics.input_total, 3);
        assert_eq!(output.metrics.after_dedup_total, 2);
        assert_eq!(output.metrics.returned_total, 2);
        assert_eq!(output.metrics.input_sources.indexed, 1);
        assert_eq!(output.metrics.input_sources.local_binding, 1);
        assert_eq!(output.metrics.input_sources.local_word, 1);
        assert_eq!(output.metrics.returned_sources.indexed, 0);
        assert_eq!(output.metrics.returned_sources.local_binding, 1);
        assert_eq!(output.metrics.returned_sources.local_word, 1);
    }

    #[test]
    fn shadow_comparison_reports_rank_movement() {
        let display = ["alpha".to_string(), "beta".to_string(), "gamma".to_string()];
        let shadow = ["beta".to_string(), "alpha".to_string(), "gamma".to_string()];

        let summary = compare_shadow_ranks(&display, &shadow);

        assert_eq!(summary.moved, 2);
        assert_eq!(summary.max_delta, 1);
    }

    #[test]
    fn completion_perf_summary_omits_candidate_names() {
        let metrics = CompletionPipelineMetrics {
            input_total: 3,
            after_dedup_total: 2,
            returned_total: 2,
            input_sources: SourceCounts {
                indexed: 1,
                local_binding: 1,
                current_file_overlay: 0,
                local_word: 1,
            },
            returned_sources: SourceCounts {
                indexed: 0,
                local_binding: 1,
                current_file_overlay: 0,
                local_word: 1,
            },
            final_rank: FinalRankSummary::default(),
            shadow: Some(ShadowRankSummary {
                moved: 2,
                max_delta: 1,
            }),
            ..CompletionPipelineMetrics::default()
        };
        let timings = CompletionStageTimings {
            total_ms: 9,
            context_ms: 1,
            recall_ms: 2,
            merge_rank_ms: 3,
            render_ms: 1,
        };

        let line = completion_perf_summary("foo", "pool", &timings, &metrics);

        assert!(line.contains("[perf] completion"));
        assert!(line.contains("prefix_len=3"));
        assert!(line.contains("hit=pool"));
        assert!(line.contains("indexed=1"));
        assert!(line.contains("local_binding=1"));
        assert!(line.contains("shadow_moved=2"));
        assert!(!line.contains("alpha"));
        assert!(!line.contains("beta"));
        assert!(!line.contains("foo\""));
    }
}
