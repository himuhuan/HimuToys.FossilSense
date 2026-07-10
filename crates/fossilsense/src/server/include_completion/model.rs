use std::collections::{HashMap, HashSet};

use tower_lsp::lsp_types::CompletionItem;

use crate::includes;
use crate::store::views::{IncludeCompletionPathRow, IncludeEdgeRow};

use super::{indexed_workspace_include_candidates, parent_slash, push_include_candidate};

#[derive(Debug, Clone, Default)]
pub(in crate::server) struct IncludeCompletionTable {
    workspace_paths: Vec<String>,
    basename_counts: HashMap<String, usize>,
    incoming_by_src_dir: HashMap<String, HashSet<String>>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(in crate::server) struct IncludeCompletionMetrics {
    pub(in crate::server) same_directory: usize,
    pub(in crate::server) recent: usize,
    pub(in crate::server) sibling: usize,
    pub(in crate::server) basename: usize,
    pub(in crate::server) depth_penalty: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct IncludeRankingSignals {
    same_directory: bool,
    recent: bool,
    sibling: bool,
    basename: bool,
    depth_penalty: bool,
}

impl IncludeCompletionTable {
    #[allow(dead_code)]
    pub(in crate::server) fn build(workspace_paths: Vec<String>) -> Self {
        Self::build_with_edges(workspace_paths, Vec::new())
    }

    pub(in crate::server) fn build_with_edges(
        mut workspace_paths: Vec<String>,
        include_edges: Vec<(String, String)>,
    ) -> Self {
        workspace_paths.sort();
        workspace_paths.dedup();
        let mut basename_counts = HashMap::new();
        for path in &workspace_paths {
            if let Some(name) = path.rsplit('/').next() {
                *basename_counts
                    .entry(name.to_ascii_lowercase())
                    .or_insert(0) += 1;
            }
        }
        let mut incoming_by_src_dir: HashMap<String, HashSet<String>> = HashMap::new();
        for (src, dst) in include_edges {
            let src_dir = parent_slash(&src).unwrap_or_default();
            incoming_by_src_dir.entry(src_dir).or_default().insert(dst);
        }
        Self {
            workspace_paths,
            basename_counts,
            incoming_by_src_dir,
        }
    }

    pub(in crate::server) fn build_from_rows(
        workspace_paths: Vec<IncludeCompletionPathRow>,
        include_edges: Vec<IncludeEdgeRow>,
    ) -> Self {
        Self::build_with_edges(
            workspace_paths.into_iter().map(|row| row.path).collect(),
            include_edges
                .into_iter()
                .map(|row| (row.source_path, row.target_path))
                .collect(),
        )
    }

    pub(in crate::server) fn len(&self) -> usize {
        self.workspace_paths.len()
    }

    #[cfg(test)]
    pub(in crate::server) fn edge_count(&self) -> usize {
        self.incoming_by_src_dir.values().map(HashSet::len).sum()
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::server) fn collect_candidates(
        &self,
        dir_part: &str,
        seg_lower: &str,
        seg: &str,
        base_score: i32,
        current_rel_dir: Option<&str>,
        evidence: Option<&CurrentIncludeEvidence>,
        metrics: &mut IncludeCompletionMetrics,
        seen: &mut HashSet<String>,
        scored: &mut Vec<(i32, String, CompletionItem)>,
    ) {
        for path in &self.workspace_paths {
            for candidate in indexed_workspace_include_candidates(path, dir_part, seg_lower) {
                let (boost, signals) = self.ranking_boost(
                    &candidate.rel_path,
                    &candidate.name,
                    current_rel_dir,
                    evidence,
                );
                if signals.recent {
                    metrics.recent += 1;
                }
                if signals.same_directory {
                    metrics.same_directory += 1;
                }
                if signals.sibling {
                    metrics.sibling += 1;
                }
                if signals.basename {
                    metrics.basename += 1;
                }
                if signals.depth_penalty {
                    metrics.depth_penalty += 1;
                }
                let score = base_score + boost;
                push_include_candidate(candidate.name, candidate.is_dir, score, seg, seen, scored);
            }
        }
    }

    fn ranking_boost(
        &self,
        rel_path: &str,
        label: &str,
        current_rel_dir: Option<&str>,
        evidence: Option<&CurrentIncludeEvidence>,
    ) -> (i32, IncludeRankingSignals) {
        let mut boost = 0;
        let mut signals = IncludeRankingSignals::default();
        if current_rel_dir.is_some_and(|dir| parent_slash(rel_path).as_deref() == Some(dir)) {
            boost += 35;
            signals.same_directory = true;
        }
        if let Some(evidence) = evidence {
            let rel_lower = rel_path.to_ascii_lowercase();
            let label_lower = label.to_ascii_lowercase();
            if evidence.recent_targets.contains(&rel_lower)
                || evidence.recent_basenames.contains(&label_lower)
            {
                boost += 30;
                signals.recent = true;
            }
            if evidence
                .source_dir
                .as_ref()
                .and_then(|dir| self.incoming_by_src_dir.get(dir))
                .is_some_and(|targets| targets.contains(rel_path))
            {
                boost += 25;
                signals.sibling = true;
            }
        }
        let frequency = self
            .basename_counts
            .get(&label.to_ascii_lowercase())
            .copied()
            .unwrap_or(0)
            .min(20) as i32;
        boost += frequency;
        signals.basename = frequency > 0;
        let depth_penalty = (rel_path.matches('/').count() as i32 * 3).min(20);
        boost -= depth_penalty;
        signals.depth_penalty = depth_penalty > 0;
        (boost.min(49), signals)
    }
}

#[derive(Debug, Clone, Default)]
pub(in crate::server) struct CurrentIncludeEvidence {
    source_dir: Option<String>,
    recent_targets: HashSet<String>,
    recent_basenames: HashSet<String>,
}

impl CurrentIncludeEvidence {
    pub(in crate::server) fn from_text(text: &str, current_rel_path: Option<&str>) -> Self {
        let source_dir = current_rel_path.and_then(parent_slash);
        let mut evidence = Self {
            source_dir,
            recent_targets: HashSet::new(),
            recent_basenames: HashSet::new(),
        };
        for line in text.lines() {
            let Some((_form, target)) = includes::parse_include_line(line) else {
                continue;
            };
            let target = target.replace('\\', "/");
            let target_lower = target.to_ascii_lowercase();
            evidence.recent_targets.insert(target_lower.clone());
            if let Some(dir) = &evidence.source_dir {
                if !target.contains('/') {
                    evidence
                        .recent_targets
                        .insert(format!("{dir}/{target}").to_ascii_lowercase());
                }
            }
            if let Some(name) = target.rsplit('/').next() {
                evidence.recent_basenames.insert(name.to_ascii_lowercase());
            }
        }
        evidence
    }
}
