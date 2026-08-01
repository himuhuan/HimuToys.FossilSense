use std::collections::{HashMap, HashSet};

use super::{CompletionPipelineMetrics, CompletionStageTimings, ShadowRankSummary};

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
        "[perf] completion total={}ms context={}ms parse={}ms local_words={}ms overlay={}ms admission_wait={}ms worker={}ms recall={}ms merge_rank={}ms render={}ms document_version={} engine_generation={} prefix_len={} hit={} intent={} intent_confidence={} history_enabled={} history_boosted={} history_max_boost={} project_boosted={} project_max_boost={} candidates_in={} after_dedup={} returned={} indexed={} local_binding={} current_file_overlay={} language_builtin={} local_word={} returned_indexed={} returned_local_binding={} returned_current_file_overlay={} returned_language_builtin={} returned_local_word={} recall_reachable={} recall_external={} recall_unknown={} recall_global={} recall_same_project={} recall_pool={} recall_entries_inspected={} recall_prefix_entries={} recall_fuzzy_entries={} recall_fuzzy_posting_entries={} recall_fuzzy_sample_entries={} recall_priority_source_probes={} recall_priority_source_attempts={} recall_priority_sources_initialized={} recall_priority_fuzzy_name_probes={} recall_priority_fuzzy_declaration_probes={} recall_selection_entries={} recall_active_entries={} recall_candidate_budget={} recall_truncated={} recall_cancel_checks={} guarded_low_trust={} shadow_moved={} shadow_max_delta={}",
        timings.total_ms,
        timings.context_ms,
        timings.parse_ms,
        timings.local_words_ms,
        timings.overlay_ms,
        timings.admission_wait_ms,
        timings.worker_ms,
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
        metrics.recall_channels.entries_inspected,
        metrics.recall_channels.prefix_entries_inspected,
        metrics.recall_channels.fuzzy_entries_inspected,
        metrics.recall_channels.fuzzy_posting_entries_inspected,
        metrics.recall_channels.fuzzy_sample_entries_inspected,
        metrics.recall_channels.priority_source_probes,
        metrics.recall_channels.priority_source_attempts,
        metrics.recall_channels.priority_sources_initialized,
        metrics.recall_channels.priority_fuzzy_name_probes,
        metrics.recall_channels.priority_fuzzy_declaration_probes,
        metrics.recall_channels.selection_entries_inspected,
        metrics.recall_channels.active_entries_total,
        metrics.recall_channels.candidate_budget,
        metrics.recall_channels.truncated,
        metrics.recall_channels.cancellation_checks,
        metrics.final_rank.guarded_low_trust,
        shadow.moved,
        shadow.max_delta,
    )
}
