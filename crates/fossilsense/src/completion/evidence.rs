use crate::completion_history::CompletionHistorySnapshot;
use crate::model::{ResolutionConfidence, ScopeTier};

use super::intent::{CompletionIntent, CompletionIntentConfidence, CompletionIntentKind};

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
    pub(super) fn priority(self) -> u8 {
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
    pub(super) fn has_strong_current_or_local(self) -> bool {
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
    pub(super) fn merge_from(&mut self, other: CandidateEvidence) {
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
        if other.kind.priority() > self.kind.priority() {
            self.kind = other.kind;
        }
        if self.history_key.is_none() {
            self.history_key = other.history_key;
        }
        self.history_score = self.history_score.max(other.history_score);
    }
}

pub(super) fn candidate_beats(current: CandidateEvidence, previous: CandidateEvidence) -> bool {
    let rank = current.primary_source.priority();
    let prev_rank = previous.primary_source.priority();
    rank > prev_rank
        || (rank == prev_rank
            && ((current.tier, current.confidence) > (previous.tier, previous.confidence)
                || ((current.tier, current.confidence) == (previous.tier, previous.confidence)
                    && current.score > previous.score)))
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
    pub(super) fn increment(&mut self, source: CandidateSource) {
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
