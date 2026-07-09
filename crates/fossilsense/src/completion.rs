mod evidence;
mod intent;
pub(crate) mod ordinary_service;
mod pipeline;
#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub(crate) use evidence::EvidenceSources;
#[allow(unused_imports)]
pub(crate) use evidence::{
    CandidateEvidence, CandidateSource, CompletionCandidateKind, CompletionPipelineMetrics,
    CompletionPipelineOutput, CompletionRankContext, CompletionStageTimings, FinalRankSummary,
    PipelineCandidate, ShadowRankSummary, SourceCounts,
};
#[allow(unused_imports)]
pub(crate) use intent::{
    classify_completion_intent, CompletionIntent, CompletionIntentConfidence, CompletionIntentKind,
};
#[cfg(test)]
pub(crate) use pipeline::run_compatible_pipeline;
#[allow(unused_imports)]
pub(crate) use pipeline::{compare_shadow_ranks, run_evidence_aware_pipeline};
pub(crate) use pipeline::{completion_perf_summary, run_evidence_aware_pipeline_with_context};
