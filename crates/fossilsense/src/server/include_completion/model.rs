use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::sync::Arc;

use tower_lsp::lsp_types::CompletionItem;

use crate::includes;
use crate::memory_report::{hash_table_bytes, vec_bytes};
use crate::store::views::{IncludeCompletionPathRow, IncludeEdgeRow};

use super::{
    indexed_workspace_include_candidates, parent_slash, push_include_candidate,
    IndexedIncludeCandidate,
};

#[derive(Debug, Clone, Default)]
pub(in crate::server) struct IncludeCompletionTable {
    pub(super) base: Arc<IncludeTableBase>,
    pub(super) delta: Arc<super::delta::IncludeDelta>,
}
#[derive(Debug, Default)]
pub(super) struct IncludeTableBase {
    pub(super) workspace_paths: Vec<String>,
    pub(super) basename_counts: HashMap<String, usize>,
    pub(super) incoming_by_src_dir: HashMap<String, HashMap<Arc<str>, usize>>,
    pub(super) outgoing_by_source: HashMap<String, Vec<Arc<str>>>,
    pub(super) candidates_by_dir: HashMap<String, Vec<IndexedIncludeCandidate>>,
    pub(super) bytes: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(in crate::server) struct IncludeCompletionMetrics {
    pub(in crate::server) inspected: usize,
    pub(in crate::server) truncated: bool,
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
    #[cfg(test)]
    pub(in crate::server) fn shares_base_for_test(&self, other: &Self) -> bool {
        self.base.workspace_paths.as_ptr() == other.base.workspace_paths.as_ptr()
    }

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
        workspace_paths.shrink_to_fit();
        let mut basename_counts = HashMap::new();
        for path in &workspace_paths {
            if let Some(name) = path.rsplit('/').next() {
                *basename_counts
                    .entry(name.to_ascii_lowercase())
                    .or_insert(0) += 1;
            }
        }
        let mut outgoing_by_source = HashMap::<String, Vec<Arc<str>>>::new();
        for (src, dst) in include_edges {
            outgoing_by_source
                .entry(src)
                .or_default()
                .push(Arc::from(dst));
        }
        let mut incoming_by_src_dir = HashMap::<String, HashMap<Arc<str>, usize>>::new();
        for (src, targets) in &mut outgoing_by_source {
            targets.sort();
            targets.dedup();
            targets.shrink_to_fit();
            let dir = parent_slash(src).unwrap_or_default();
            let incoming = incoming_by_src_dir.entry(dir).or_default();
            for target in targets {
                *incoming.entry(target.clone()).or_default() += 1;
            }
        }
        // These tables become immutable. Release geometric build capacity
        // before another generation is built beside them.
        for targets in incoming_by_src_dir.values_mut() {
            targets.shrink_to_fit();
        }
        incoming_by_src_dir.shrink_to_fit();
        outgoing_by_source.shrink_to_fit();
        basename_counts.shrink_to_fit();
        let candidates_by_dir = build_candidate_index(&workspace_paths);
        let mut base = IncludeTableBase {
            workspace_paths,
            basename_counts,
            incoming_by_src_dir,
            outgoing_by_source,
            candidates_by_dir,
            bytes: 0,
        };
        base.bytes = base_accounted_bytes(&base);
        Self {
            base: Arc::new(base),
            delta: Arc::new(super::delta::IncludeDelta::default()),
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
        self.base
            .workspace_paths
            .len()
            .saturating_add_signed(self.delta.path_count_change())
    }

    pub(in crate::server) fn accounted_bytes(&self) -> usize {
        self.base.bytes + self.delta.bytes()
    }
    pub(in crate::server) fn delta_bytes(&self) -> usize {
        self.delta.bytes()
    }
    pub(in crate::server) fn delta_partitions(&self) -> usize {
        self.delta.partitions()
    }
    pub(in crate::server) fn updated_paths(
        self: &Arc<Self>,
        paths: &[String],
        fresh: &[String],
        sources: &[String],
        edges: Vec<IncludeEdgeRow>,
    ) -> Option<Arc<Self>> {
        super::delta::updated(self, paths, fresh, sources, edges)
    }
    #[cfg(test)]
    pub(in crate::server) fn edge_count(&self) -> usize {
        let mut count: usize = self
            .base
            .incoming_by_src_dir
            .values()
            .map(HashMap::len)
            .sum();
        for (dir, targets) in &self.delta.incoming {
            for (target, change) in targets {
                let old = self
                    .base
                    .incoming_by_src_dir
                    .get(dir)
                    .and_then(|m| m.get(target))
                    .copied()
                    .unwrap_or(0);
                let new = old.saturating_add_signed(*change);
                if old == 0 && new > 0 {
                    count += 1;
                }
                if old > 0 && new == 0 {
                    count -= 1;
                }
            }
        }
        count
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
        let base = self
            .base
            .candidates_by_dir
            .get(dir_part)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let start = base.partition_point(|candidate| candidate.name_lower.as_str() < seg_lower);
        let mut added = self
            .delta
            .candidates
            .get(dir_part)
            .into_iter()
            .flatten()
            .filter(|entry| {
                entry.change > 0
                    && entry.candidate.name_lower.starts_with(seg_lower)
                    && base
                        .binary_search_by(|candidate| {
                            super::delta::candidate_order(candidate, &entry.candidate)
                        })
                        .is_err()
            })
            .map(|entry| &entry.candidate)
            .peekable();
        let mut existing = base[start..]
            .iter()
            .take_while(|candidate| candidate.name_lower.starts_with(seg_lower))
            .peekable();
        loop {
            if added.peek().is_none() && existing.peek().is_none() {
                break;
            }
            if metrics.inspected >= 16_384 {
                metrics.truncated = true;
                break;
            }
            let from_base = match (existing.peek(), added.peek()) {
                (Some(a), Some(b)) => super::delta::candidate_order(a, b).is_le(),
                (Some(_), None) => true,
                _ => false,
            };
            let candidate = if from_base {
                existing.next().unwrap()
            } else {
                added.next().unwrap()
            };
            metrics.inspected += 1;
            if from_base
                && candidate
                    .contributors
                    .saturating_add_signed(self.delta.candidate_change(dir_part, candidate))
                    == 0
            {
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
            if evidence.source_dir.as_ref().is_some_and(|dir| {
                let base = self
                    .base
                    .incoming_by_src_dir
                    .get(dir)
                    .and_then(|targets| targets.get(rel_path))
                    .copied()
                    .unwrap_or(0);
                base.saturating_add_signed(self.delta.incoming_change(dir, rel_path)) > 0
            }) {
                boost += 25;
                signals.sibling = true;
            }
        }
        let frequency = self
            .base
            .basename_counts
            .get(&label.to_ascii_lowercase())
            .copied()
            .unwrap_or(0)
            .saturating_add_signed(self.delta.basename_change(&label.to_ascii_lowercase()))
            .min(20) as i32;
        boost += frequency;
        signals.basename = frequency > 0;
        let depth_penalty = (rel_path.matches('/').count() as i32 * 3).min(20);
        boost -= depth_penalty;
        signals.depth_penalty = depth_penalty > 0;
        (boost.min(49), signals)
    }
}

fn base_accounted_bytes(base: &IncludeTableBase) -> usize {
    let mut bytes = size_of::<IncludeTableBase>()
        + size_of::<IncludeCompletionTable>()
        + vec_bytes::<String>(base.workspace_paths.capacity())
        + base
            .workspace_paths
            .iter()
            .map(String::capacity)
            .sum::<usize>()
        + hash_table_bytes::<String, usize>(base.basename_counts.capacity())
        + base
            .basename_counts
            .keys()
            .map(String::capacity)
            .sum::<usize>()
        + hash_table_bytes::<String, Vec<Arc<str>>>(base.outgoing_by_source.capacity())
        + hash_table_bytes::<String, HashMap<Arc<str>, usize>>(base.incoming_by_src_dir.capacity())
        + hash_table_bytes::<String, Vec<IndexedIncludeCandidate>>(
            base.candidates_by_dir.capacity(),
        );
    for (src, targets) in &base.outgoing_by_source {
        bytes += src.capacity()
            + targets.capacity() * size_of::<Arc<str>>()
            + targets
                .iter()
                .map(|target| target.len() + 2 * size_of::<usize>())
                .sum::<usize>();
    }
    for (dir, targets) in &base.incoming_by_src_dir {
        bytes += dir.capacity() + hash_table_bytes::<Arc<str>, usize>(targets.capacity());
    }
    for (dir, values) in &base.candidates_by_dir {
        bytes += dir.capacity() + values.capacity() * size_of::<IndexedIncludeCandidate>();
        bytes += values
            .iter()
            .map(|c| c.name.capacity() + c.name_lower.capacity() + c.rel_path.capacity())
            .sum::<usize>();
    }
    bytes
}

fn build_candidate_index(
    workspace_paths: &[String],
) -> HashMap<String, Vec<IndexedIncludeCandidate>> {
    let mut index: HashMap<String, Vec<IndexedIncludeCandidate>> = HashMap::new();
    for path in workspace_paths {
        index
            .entry(String::new())
            .or_default()
            .extend(indexed_workspace_include_candidates(path, "", ""));
        let components: Vec<&str> = path.split('/').collect();
        for start in 0..components.len().saturating_sub(1) {
            for end in (start + 1)..components.len() {
                let dir = format!("{}/", components[start..end].join("/"));
                index
                    .entry(dir.clone())
                    .or_default()
                    .extend(indexed_workspace_include_candidates(path, &dir, ""));
            }
        }
    }
    for candidates in index.values_mut() {
        candidates.sort_by(|left, right| {
            left.name_lower
                .cmp(&right.name_lower)
                .then_with(|| left.rel_path.cmp(&right.rel_path))
                .then_with(|| left.is_dir.cmp(&right.is_dir))
        });
        candidates.dedup_by(|left, right| {
            let same = left.name_lower == right.name_lower
                && left.rel_path == right.rel_path
                && left.is_dir == right.is_dir;
            if same {
                right.contributors += left.contributors;
            }
            same
        });
        candidates.shrink_to_fit();
    }
    index.shrink_to_fit();
    index
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounted_bytes_grows_with_paths_and_edges() {
        let empty = IncludeCompletionTable::default();
        let baseline = empty.accounted_bytes();

        let table = IncludeCompletionTable::build_with_edges(
            vec![
                "include/alpha.h".to_string(),
                "include/detail/beta.h".to_string(),
                "src/main.c".to_string(),
            ],
            vec![
                ("src/main.c".to_string(), "include/alpha.h".to_string()),
                (
                    "include/alpha.h".to_string(),
                    "include/detail/beta.h".to_string(),
                ),
            ],
        );

        assert_eq!(table.len(), 3);
        assert!(table.accounted_bytes() > baseline);
    }
}
