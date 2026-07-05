use std::collections::{HashMap, HashSet};

use crate::model::{ResolutionConfidence, ScopeTier};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CandidateSource {
    Indexed,
    LocalBinding,
    LocalWord,
}

impl CandidateSource {
    fn priority(self) -> u8 {
        match self {
            CandidateSource::LocalBinding => 3,
            CandidateSource::Indexed => 2,
            CandidateSource::LocalWord => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CandidateEvidence {
    pub source: CandidateSource,
    pub tier: ScopeTier,
    pub confidence: ResolutionConfidence,
    pub score: i32,
}

impl CandidateEvidence {
    pub(crate) fn new(
        source: CandidateSource,
        tier: ScopeTier,
        confidence: ResolutionConfidence,
        score: i32,
    ) -> Self {
        Self {
            source,
            tier,
            confidence,
            score,
        }
    }
}

#[derive(Debug)]
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
    pub local_word: usize,
}

impl SourceCounts {
    fn increment(&mut self, source: CandidateSource) {
        match source {
            CandidateSource::Indexed => self.indexed += 1,
            CandidateSource::LocalBinding => self.local_binding += 1,
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
pub(crate) struct CompletionPipelineMetrics {
    pub input_total: usize,
    pub after_dedup_total: usize,
    pub returned_total: usize,
    pub input_sources: SourceCounts,
    pub returned_sources: SourceCounts,
    pub shadow: Option<ShadowRankSummary>,
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

fn candidate_beats(current: CandidateEvidence, previous: CandidateEvidence) -> bool {
    let rank = current.source.priority();
    let prev_rank = previous.source.priority();
    rank > prev_rank
        || (rank == prev_rank
            && ((current.tier, current.confidence) > (previous.tier, previous.confidence)
                || ((current.tier, current.confidence) == (previous.tier, previous.confidence)
                    && current.score > previous.score)))
}

fn count_sources<'a, T: 'a>(
    candidates: impl IntoIterator<Item = &'a PipelineCandidate<T>>,
) -> SourceCounts {
    let mut counts = SourceCounts::default();
    for candidate in candidates {
        counts.increment(candidate.evidence.source);
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
        "[perf] completion total={}ms context={}ms recall={}ms merge_rank={}ms render={}ms prefix_len={} hit={} candidates_in={} after_dedup={} returned={} indexed={} local_binding={} local_word={} returned_indexed={} returned_local_binding={} returned_local_word={} shadow_moved={} shadow_max_delta={}",
        timings.total_ms,
        timings.context_ms,
        timings.recall_ms,
        timings.merge_rank_ms,
        timings.render_ms,
        prefix.chars().count(),
        memo_hit,
        metrics.input_total,
        metrics.after_dedup_total,
        metrics.returned_total,
        metrics.input_sources.indexed,
        metrics.input_sources.local_binding,
        metrics.input_sources.local_word,
        metrics.returned_sources.indexed,
        metrics.returned_sources.local_binding,
        metrics.returned_sources.local_word,
        shadow.moved,
        shadow.max_delta,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
                local_word: 1,
            },
            returned_sources: SourceCounts {
                indexed: 0,
                local_binding: 1,
                local_word: 1,
            },
            shadow: Some(ShadowRankSummary {
                moved: 2,
                max_delta: 1,
            }),
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
