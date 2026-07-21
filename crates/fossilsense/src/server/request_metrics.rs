use crate::query;

#[derive(Clone, Copy, Debug, Default)]
pub(in crate::server) struct SemanticRequestPerf {
    pub(in crate::server) candidates: query::CallableCandidateMetrics,
    pub(in crate::server) returned: usize,
    pub(in crate::server) hydration_count: usize,
    pub(in crate::server) hydration_bytes: usize,
    pub(in crate::server) query_us: u128,
    pub(in crate::server) hydration_us: u128,
    pub(in crate::server) reach_us: u128,
    pub(in crate::server) coverage_open: bool,
    pub(in crate::server) coverage_truncated: bool,
    pub(in crate::server) coverage_incomplete: bool,
    pub(in crate::server) coverage_reason: u8,
    pub(in crate::server) arity_fallback: bool,
}

impl SemanticRequestPerf {
    pub(in crate::server) fn from_callable_set(set: &query::CallableCandidateSet) -> Self {
        Self {
            candidates: set.metrics(),
            coverage_open: set.coverage.scope_open,
            coverage_truncated: set.coverage.truncated,
            coverage_incomplete: set.coverage.incomplete_reason.is_some(),
            coverage_reason: coverage_reason_code(set.coverage.incomplete_reason),
            arity_fallback: set.arity_mismatch_fallback,
            ..Self::default()
        }
    }

    pub(in crate::server) fn log_line(self, feature: &'static str, total_us: u128) -> String {
        format!(
            "[perf] semantic_candidates feature={feature} total_us={total_us} query_us={} reach_us={} hydration_us={} raw={} filtered={} grouped={} returned={} arity_compatible={} arity_unknown={} arity_incompatible={} counterpart_strict={} counterpart_ambiguous={} hydration_count={} hydration_bytes={} coverage_open={} coverage_truncated={} coverage_incomplete={} coverage_reason={} arity_fallback={}",
            self.query_us,
            self.reach_us,
            self.hydration_us,
            self.candidates.raw_candidates,
            self.candidates.filtered_candidates,
            self.candidates.grouped_candidates,
            self.returned,
            self.candidates.arity_compatible,
            self.candidates.arity_unknown,
            self.candidates.arity_incompatible,
            self.candidates.counterpart_strict,
            self.candidates.counterpart_ambiguous,
            self.hydration_count,
            self.hydration_bytes,
            self.coverage_open as u8,
            self.coverage_truncated as u8,
            self.coverage_incomplete as u8,
            self.coverage_reason,
            self.arity_fallback as u8,
        )
    }

    pub(in crate::server) fn include_type_candidates(
        &mut self,
        bundle: &crate::candidate_service::TypeCandidateBundle,
    ) {
        let coverage = if bundle.records.coverage.scanned >= bundle.aliases.coverage.scanned {
            &bundle.records.coverage
        } else {
            &bundle.aliases.coverage
        };
        self.candidates.raw_candidates = self
            .candidates
            .raw_candidates
            .saturating_add(coverage.scanned);
        self.candidates.filtered_candidates = self
            .candidates
            .filtered_candidates
            .saturating_add(bundle.records.candidates.len())
            .saturating_add(bundle.aliases.candidates.len());
        self.candidates.grouped_candidates = self
            .candidates
            .grouped_candidates
            .saturating_add(bundle.records.candidates.len())
            .saturating_add(bundle.alias_resolutions.len());
        self.coverage_open |= coverage.scope_open;
        self.coverage_truncated |= coverage.truncated;
        self.coverage_incomplete |= coverage.incomplete_reason.is_some();
        if self.coverage_reason == 0 {
            self.coverage_reason = coverage_reason_code(coverage.incomplete_reason);
        }
    }

    pub(in crate::server) fn include_non_callable_candidates(&mut self, count: usize) {
        self.candidates.raw_candidates = self.candidates.raw_candidates.saturating_add(count);
        self.candidates.filtered_candidates =
            self.candidates.filtered_candidates.saturating_add(count);
        self.candidates.grouped_candidates =
            self.candidates.grouped_candidates.saturating_add(count);
    }
}

fn coverage_reason_code(reason: Option<query::CandidateIncompleteReason>) -> u8 {
    match reason {
        None => 0,
        Some(query::CandidateIncompleteReason::ScanLimit) => 1,
        Some(query::CandidateIncompleteReason::CandidateBudget) => 2,
        Some(query::CandidateIncompleteReason::TimeBudget) => 3,
        Some(query::CandidateIncompleteReason::Cancelled) => 4,
        Some(query::CandidateIncompleteReason::FactsUnavailable) => 5,
        Some(query::CandidateIncompleteReason::GenerationMismatch) => 6,
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(in crate::server) struct HydrationStats {
    pub(in crate::server) count: usize,
    pub(in crate::server) bytes: usize,
}

impl HydrationStats {
    pub(in crate::server) fn record(&mut self, source: Option<&str>) {
        if let Some(source) = source {
            self.count += 1;
            self.bytes = self.bytes.saturating_add(source.len());
        }
    }
}

#[derive(Clone, Copy)]
pub(in crate::server) enum LiveParseCacheEvent {
    Hit,
    Coalesced,
    Miss,
}

pub(in crate::server) fn live_parse_cache_log(event: LiveParseCacheEvent) -> &'static str {
    match event {
        LiveParseCacheEvent::Hit => "[perf] live_parse_cache state=hit",
        LiveParseCacheEvent::Coalesced => "[perf] live_parse_cache state=coalesced",
        LiveParseCacheEvent::Miss => "[perf] live_parse_cache state=miss",
    }
}
