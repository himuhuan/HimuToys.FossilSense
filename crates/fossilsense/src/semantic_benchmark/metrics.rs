use std::time::Instant;

use crate::query::CallableCandidateSet;

use super::{counterpart_edge_count, BenchmarkMetrics};

pub(super) fn candidate_signatures(set: &CallableCandidateSet) -> Vec<String> {
    let mut signatures: Vec<_> = set
        .anchors
        .iter()
        .map(|candidate| candidate.anchor.canonical_signature.clone())
        .collect();
    signatures.sort();
    signatures
}

impl BenchmarkMetrics {
    pub(super) fn absorb_candidates(&mut self, set: &CallableCandidateSet, returned: usize) {
        let aggregate = set.metrics();
        self.candidate_rows_scanned = self
            .candidate_rows_scanned
            .saturating_add(aggregate.raw_candidates as u64);
        self.candidate_rows_filtered = self
            .candidate_rows_filtered
            .saturating_add(aggregate.filtered_candidates as u64);
        self.candidate_rows_grouped = self
            .candidate_rows_grouped
            .saturating_add(aggregate.grouped_candidates as u64);
        self.candidate_rows_returned = self.candidate_rows_returned.saturating_add(returned as u64);
        self.candidate_raw = self
            .candidate_raw
            .saturating_add(aggregate.raw_candidates as u64);
        self.candidate_filtered = self
            .candidate_filtered
            .saturating_add(aggregate.filtered_candidates as u64);
        self.candidate_grouped = self
            .candidate_grouped
            .saturating_add(aggregate.grouped_candidates as u64);
        self.candidate_returned = self.candidate_returned.saturating_add(returned as u64);
        self.arity_compatible = self
            .arity_compatible
            .saturating_add(aggregate.arity_compatible as u64);
        self.arity_unknown = self
            .arity_unknown
            .saturating_add(aggregate.arity_unknown as u64);
        self.arity_incompatible = self
            .arity_incompatible
            .saturating_add(aggregate.arity_incompatible as u64);
        self.counterpart_strict = self
            .counterpart_strict
            .saturating_add(aggregate.counterpart_strict as u64);
        self.counterpart_ambiguous = self
            .counterpart_ambiguous
            .saturating_add(aggregate.counterpart_ambiguous as u64);
        self.counterpart_groups = self
            .counterpart_groups
            .saturating_add(set.groups.len() as u64);
        self.counterpart_edges = self
            .counterpart_edges
            .saturating_add(counterpart_edge_count(&set.groups));
        self.coverage_scanned = self
            .coverage_scanned
            .saturating_add(set.coverage.scanned as u64);
        self.coverage_open = self
            .coverage_open
            .saturating_add(u64::from(set.coverage.scope_open));
        self.coverage_truncated = self
            .coverage_truncated
            .saturating_add(u64::from(set.coverage.truncated));
        self.candidate_query_truncated = self
            .candidate_query_truncated
            .saturating_add(u64::from(set.coverage.truncated));
        self.arity_mismatch_fallback = self
            .arity_mismatch_fallback
            .saturating_add(u64::from(set.arity_mismatch_fallback));
        self.fallback_used = self.fallback_used.saturating_add(u64::from(
            set.arity_mismatch_fallback || set.coverage.incomplete_reason.is_some(),
        ));
    }

    pub(super) fn print(&self) {
        macro_rules! metric {
            ($field:ident) => {
                println!(concat!(stringify!($field), ": {}"), self.$field)
            };
        }
        metric!(callable_query_us);
        metric!(candidate_rows_scanned);
        metric!(candidate_rows_filtered);
        metric!(candidate_rows_grouped);
        metric!(candidate_rows_returned);
        metric!(candidate_raw);
        metric!(candidate_filtered);
        metric!(candidate_grouped);
        metric!(candidate_returned);
        metric!(candidate_query_truncated);
        metric!(arity_compatible);
        metric!(arity_unknown);
        metric!(arity_incompatible);
        metric!(arity_mismatch_fallback);
        metric!(counterpart_graph_us);
        metric!(counterpart_edges);
        metric!(counterpart_groups);
        metric!(counterpart_strict);
        metric!(counterpart_ambiguous);
        metric!(counterpart_incomplete);
        metric!(candidate_scan_cap);
        metric!(candidate_scan_observed);
        metric!(reach_nodes_visited);
        metric!(hydration_us);
        metric!(hydration_count);
        metric!(hydration_bytes);
        metric!(hydration_sections);
        metric!(hydration_file_bytes);
        metric!(hydration_requested_bytes);
        metric!(hydration_revision_rejections);
        metric!(overlay_merge_us);
        metric!(overlay_parse_us);
        metric!(overlay_documents);
        metric!(signature_help_requests);
        metric!(signature_help_p50_us);
        metric!(signature_help_p95_us);
        metric!(concurrent_query_p50_us);
        metric!(concurrent_query_p95_us);
        metric!(publication_conflicts);
        metric!(generation_mismatches);
        metric!(query_us);
        metric!(reach_us);
        metric!(coverage_scanned);
        metric!(coverage_open);
        metric!(coverage_truncated);
        metric!(fallback_used);
    }
}

pub(super) fn percentile(samples: &mut [u64], percentile: usize) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    samples.sort_unstable();
    let index = (samples.len() - 1).saturating_mul(percentile) / 100;
    samples[index]
}

pub(super) fn elapsed_us(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}
