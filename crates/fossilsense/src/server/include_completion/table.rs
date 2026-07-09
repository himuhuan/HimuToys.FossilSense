use std::collections::{HashMap, HashSet};

use tower_lsp::lsp_types::CompletionItem;

use crate::store::views::{IncludeCompletionPathRow, IncludeEdgeRow};

use super::presentation::push_include_candidate;
use super::workspace_candidates::IndexedIncludeCandidate;
use super::{parent_slash, CurrentIncludeEvidence};

#[derive(Debug, Clone, Default)]
pub(in crate::server) struct IncludeCompletionTable {
    workspace_paths: Vec<String>,
    candidates_by_dir: HashMap<String, Vec<IndexedIncludeCandidate>>,
    basename_counts: HashMap<String, usize>,
    incoming_by_src_dir: HashMap<String, HashSet<String>>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(in crate::server) struct IncludeCompletionMetrics {
    pub same_directory: usize,
    pub recent: usize,
    pub sibling: usize,
    pub basename: usize,
    pub depth_penalty: usize,
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
        let candidates_by_dir = include_candidates_by_dir(&workspace_paths);
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
            candidates_by_dir,
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
    pub(super) fn collect_candidates(
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
        let key = normalize_dir_part(dir_part);
        if let Some(candidates) = self.candidates_by_dir.get(&key) {
            for candidate in candidates {
                if !candidate.name.to_ascii_lowercase().starts_with(seg_lower) {
                    continue;
                }
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
                push_include_candidate(
                    candidate.name.clone(),
                    candidate.is_dir,
                    score,
                    seg,
                    seen,
                    scored,
                );
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

fn normalize_dir_part(dir_part: &str) -> String {
    dir_part.replace('\\', "/").trim_matches('/').to_string()
}

fn include_candidates_by_dir(
    workspace_paths: &[String],
) -> HashMap<String, Vec<IndexedIncludeCandidate>> {
    let mut map: HashMap<String, Vec<IndexedIncludeCandidate>> = HashMap::new();
    let mut seen: HashSet<(String, String, bool, String)> = HashSet::new();
    for path in workspace_paths {
        let rel = path.replace('\\', "/");
        let segments: Vec<&str> = rel
            .split('/')
            .filter(|segment| !segment.is_empty())
            .collect();
        if segments.is_empty() {
            continue;
        }

        if segments.len() > 1 {
            push_indexed_candidate(
                &mut map,
                &mut seen,
                "",
                segments[0],
                true,
                segments[0].to_string(),
            );
        }
        if let Some(name) = segments.last().copied() {
            if super::looks_like_header(name) {
                push_indexed_candidate(&mut map, &mut seen, "", name, false, rel.clone());
            }
        }

        for start in 0..segments.len().saturating_sub(1) {
            for parent_end in start + 1..segments.len() {
                let key = segments[start..parent_end].join("/");
                let name = segments[parent_end];
                let is_dir = parent_end + 1 < segments.len();
                if !is_dir && !super::looks_like_header(name) {
                    continue;
                }
                let rel_path = if is_dir {
                    format!("{key}/{name}")
                } else {
                    rel.clone()
                };
                push_indexed_candidate(&mut map, &mut seen, &key, name, is_dir, rel_path);
            }
        }
    }
    for candidates in map.values_mut() {
        candidates.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then(a.rel_path.cmp(&b.rel_path))
                .then(a.is_dir.cmp(&b.is_dir))
        });
    }
    map
}

fn push_indexed_candidate(
    map: &mut HashMap<String, Vec<IndexedIncludeCandidate>>,
    seen: &mut HashSet<(String, String, bool, String)>,
    dir_key: &str,
    name: &str,
    is_dir: bool,
    rel_path: String,
) {
    let key = (
        dir_key.to_string(),
        name.to_string(),
        is_dir,
        rel_path.clone(),
    );
    if !seen.insert(key) {
        return;
    }
    map.entry(dir_key.to_string())
        .or_default()
        .push(IndexedIncludeCandidate {
            name: name.to_string(),
            is_dir,
            rel_path,
        });
}
