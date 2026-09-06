use super::*;

impl CandidateQueryService<'_> {
    pub(super) fn declaration_coverage_for_candidates(
        &self,
        candidates: &[ResolvedDeclarationCandidate],
    ) -> Result<(crate::semantic_model::FactCoverage, u8)> {
        use crate::semantic_model::{CoverageReason, FactCoverage, FactGroup};
        if self.semantic_family == crate::semantic_model::SemanticFamily::Go {
            return Ok((
                if self.overlays.has_unavailable_facts() {
                    FactCoverage::Partial
                } else {
                    FactCoverage::Complete
                },
                0,
            ));
        }
        const FILE_LIMIT: usize = 512;
        let mut paths = Vec::new();
        let mut seen = HashSet::new();
        let mut limited = false;
        let reachable = self
            .current_reach
            .iter()
            .flat_map(|reach| reach.files.iter().chain(reach.heuristic_files.iter()));
        for path in std::iter::once(self.current_path)
            .filter(|path| !path.is_empty())
            .chain(
                candidates
                    .iter()
                    .map(|candidate| candidate.fact.path.as_str()),
            )
            .chain(reachable.map(String::as_str))
        {
            if seen.contains(path) {
                continue;
            }
            if paths.len() >= FILE_LIMIT {
                limited = true;
                break;
            }
            seen.insert(path.to_string());
            paths.push(path.to_string());
        }
        let mut unknown = limited || paths.is_empty();
        let mut partial = false;
        let mut reasons = if limited {
            1 << CoverageReason::BudgetExhausted as u8
        } else {
            0
        };
        let mut persisted = Vec::new();
        let mut observe = |summary: crate::semantic_model::DeclarationCoverageSummary| {
            match summary.fact_coverage(FactGroup::Declarations) {
                FactCoverage::Complete => {}
                FactCoverage::Partial => partial = true,
                FactCoverage::Unknown => unknown = true,
            }
            reasons |= summary.reason_mask(FactGroup::Declarations.bit());
        };
        for path in &paths {
            if let Some(summary) = self.overlays.declaration_coverage_for_path(path) {
                observe(summary);
            } else if !self.overlays.shadows(path) {
                persisted.push(path.clone());
            }
        }
        let rows = match self.handle {
            Some(handle) => handle.read(|store| store.coverage_view().for_paths(&persisted))?,
            None => Vec::new(),
        };
        for row in &rows {
            observe(row.summary);
        }
        unknown |= rows.len() < persisted.len();
        unknown |= paths.iter().any(|path| {
            self.overlays.shadows(path)
                && self.overlays.declaration_coverage_for_path(path).is_none()
        });
        Ok((
            if unknown {
                FactCoverage::Unknown
            } else if partial {
                FactCoverage::Partial
            } else {
                FactCoverage::Complete
            },
            reasons,
        ))
    }
}
