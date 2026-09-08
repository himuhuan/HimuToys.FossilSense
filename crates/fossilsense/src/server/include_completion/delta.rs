//! Bounded contribution changes over an immutable include-completion table.
use super::model::IncludeCompletionTable;
use super::{indexed_workspace_include_candidates, parent_slash, IndexedIncludeCandidate};
use crate::store::views::IncludeEdgeRow;
use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap, HashSet},
    mem::size_of,
    sync::Arc,
};

#[derive(Debug, Clone)]
pub(super) struct CandidateChange {
    pub(super) candidate: IndexedIncludeCandidate,
    pub(super) change: isize,
}
#[derive(Debug, Clone, Default)]
pub(super) struct IncludeDelta {
    paths: BTreeMap<String, bool>,
    path_balance: isize,
    pub(super) candidates: BTreeMap<String, Vec<CandidateChange>>,
    basenames: BTreeMap<String, isize>,
    outgoing: BTreeMap<String, Vec<Arc<str>>>,
    pub(super) incoming: BTreeMap<String, BTreeMap<Arc<str>, isize>>,
}
pub(super) fn candidate_order(
    a: &IndexedIncludeCandidate,
    b: &IndexedIncludeCandidate,
) -> Ordering {
    a.name_lower
        .cmp(&b.name_lower)
        .then_with(|| a.rel_path.cmp(&b.rel_path))
        .then_with(|| a.is_dir.cmp(&b.is_dir))
}
fn node_bytes<K, V>(len: usize) -> usize {
    len.saturating_mul(11 * size_of::<(K, V)>() + 12 * size_of::<usize>())
}
impl IncludeDelta {
    pub(super) fn path_count_change(&self) -> isize {
        self.path_balance
    }
    pub(super) fn basename_change(&self, name: &str) -> isize {
        self.basenames.get(name).copied().unwrap_or(0)
    }
    pub(super) fn incoming_change(&self, dir: &str, target: &str) -> isize {
        self.incoming
            .get(dir)
            .and_then(|targets| targets.get(target))
            .copied()
            .unwrap_or(0)
    }
    pub(super) fn candidate_change(&self, dir: &str, candidate: &IndexedIncludeCandidate) -> isize {
        self.candidates
            .get(dir)
            .and_then(|values| {
                values
                    .binary_search_by(|v| candidate_order(&v.candidate, candidate))
                    .ok()
                    .map(|i| values[i].change)
            })
            .unwrap_or(0)
    }
    pub(super) fn partitions(&self) -> usize {
        self.paths.len()
            + self.candidates.len()
            + self.basenames.len()
            + self.outgoing.len()
            + self.incoming.len()
    }
    pub(super) fn bytes(&self) -> usize {
        let mut bytes = size_of::<Self>()
            + node_bytes::<String, bool>(self.paths.len())
            + node_bytes::<String, Vec<CandidateChange>>(self.candidates.len())
            + node_bytes::<String, isize>(self.basenames.len())
            + node_bytes::<String, Vec<Arc<str>>>(self.outgoing.len())
            + node_bytes::<String, BTreeMap<Arc<str>, isize>>(self.incoming.len());
        bytes += self
            .paths
            .keys()
            .chain(self.basenames.keys())
            .map(String::capacity)
            .sum::<usize>();
        for (dir, values) in &self.candidates {
            bytes += dir.capacity() + values.capacity() * size_of::<CandidateChange>();
            bytes += values
                .iter()
                .map(|v| {
                    v.candidate.name.capacity()
                        + v.candidate.name_lower.capacity()
                        + v.candidate.rel_path.capacity()
                })
                .sum::<usize>();
        }
        for (src, values) in &self.outgoing {
            bytes += src.capacity()
                + values.capacity() * size_of::<Arc<str>>()
                + values
                    .iter()
                    .map(|v| v.len() + 2 * size_of::<usize>())
                    .sum::<usize>();
        }
        for (dir, values) in &self.incoming {
            bytes += dir.capacity()
                + node_bytes::<Arc<str>, isize>(values.len())
                + values
                    .keys()
                    .map(|v| v.len() + 2 * size_of::<usize>())
                    .sum::<usize>();
        }
        bytes
    }
    fn adjust_candidate(&mut self, dir: String, candidate: IndexedIncludeCandidate, change: isize) {
        let values = self.candidates.entry(dir.clone()).or_default();
        match values.binary_search_by(|value| candidate_order(&value.candidate, &candidate)) {
            Ok(index) => {
                values[index].change += change;
                if values[index].change == 0 {
                    values.remove(index);
                }
            }
            Err(index) => values.insert(index, CandidateChange { candidate, change }),
        }
        if values.is_empty() {
            self.candidates.remove(&dir);
        }
    }
    fn adjust_incoming(&mut self, dir: &str, target: Arc<str>, change: isize) {
        let values = self.incoming.entry(dir.to_owned()).or_default();
        let value = values.entry(target.clone()).or_default();
        *value += change;
        if *value == 0 {
            values.remove(&target);
        }
        if values.is_empty() {
            self.incoming.remove(dir);
        }
    }
}
fn contributions(path: &str) -> Option<Vec<(String, IndexedIncludeCandidate)>> {
    let components: Vec<_> = path.split('/').collect();
    if components.len() > 32 {
        return None;
    }
    let mut output: Vec<_> = indexed_workspace_include_candidates(path, "", "")
        .into_iter()
        .map(|c| (String::new(), c))
        .collect();
    for start in 0..components.len().saturating_sub(1) {
        for end in start + 1..components.len() {
            let dir = format!("{}/", components[start..end].join("/"));
            output.extend(
                indexed_workspace_include_candidates(path, &dir, "")
                    .into_iter()
                    .map(|c| (dir.clone(), c)),
            );
            if output.len() > 1024 {
                return None;
            }
        }
    }
    Some(output)
}
pub(super) fn updated(
    table: &Arc<IncludeCompletionTable>,
    paths: &[String],
    fresh: &[String],
    sources: &[String],
    edges: Vec<IncludeEdgeRow>,
) -> Option<Arc<IncludeCompletionTable>> {
    if paths.len() > 256 || sources.len() > 256 || edges.len() > 8192 {
        return None;
    }
    let fresh: HashSet<_> = fresh.iter().map(String::as_str).collect();
    let mut delta: Option<IncludeDelta> = None;
    let mut seen_paths = HashSet::new();
    for path in paths {
        if !seen_paths.insert(path) {
            continue;
        }
        let in_base = table.base.workspace_paths.binary_search(path).is_ok();
        let old = table.delta.paths.get(path).copied().unwrap_or(in_base);
        let new = fresh.contains(path.as_str());
        if old == new {
            continue;
        }
        let changes = contributions(path)?;
        let value = delta.get_or_insert_with(|| table.delta.as_ref().clone());
        let sign = if new { 1 } else { -1 };
        value.path_balance += sign;
        if new == in_base {
            value.paths.remove(path);
        } else {
            value.paths.insert(path.clone(), new);
        }
        if let Some(name) = path.rsplit('/').next() {
            let name = name.to_ascii_lowercase();
            let amount = value.basenames.entry(name.clone()).or_default();
            *amount += sign;
            if *amount == 0 {
                value.basenames.remove(&name);
            }
        }
        for (dir, candidate) in changes {
            value.adjust_candidate(dir, candidate, sign);
        }
        if value.partitions() > 256 || value.bytes() > 4 * 1024 * 1024 {
            return None;
        }
    }
    let mut by_source = HashMap::<String, Vec<Arc<str>>>::new();
    for edge in edges {
        by_source
            .entry(edge.source_path)
            .or_default()
            .push(Arc::from(edge.target_path));
    }
    let mut seen_sources = HashSet::new();
    for source in sources {
        if !seen_sources.insert(source) {
            continue;
        }
        let old = table
            .delta
            .outgoing
            .get(source)
            .or_else(|| table.base.outgoing_by_source.get(source))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let mut new = by_source.remove(source).unwrap_or_default();
        new.sort();
        new.dedup();
        if old == new {
            continue;
        }
        let value = delta.get_or_insert_with(|| table.delta.as_ref().clone());
        let dir = parent_slash(source).unwrap_or_default();
        for target in old {
            value.adjust_incoming(&dir, target.clone(), -1);
        }
        for target in &new {
            value.adjust_incoming(&dir, target.clone(), 1);
        }
        let base = table
            .base
            .outgoing_by_source
            .get(source)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if base == new {
            value.outgoing.remove(source);
        } else {
            value.outgoing.insert(source.clone(), new);
        }
        if value.partitions() > 256 || value.bytes() > 4 * 1024 * 1024 {
            return None;
        }
    }
    Some(match delta {
        None => table.clone(),
        Some(delta) => Arc::new(IncludeCompletionTable {
            base: table.base.clone(),
            delta: Arc::new(delta),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::super::{collect_include_candidates_ranked_for_test, CurrentIncludeEvidence};
    use super::*;
    fn results(table: &IncludeCompletionTable, dir: &str) -> serde_json::Value {
        let evidence = CurrentIncludeEvidence::from_text("", Some("src/a.c"));
        serde_json::to_value(collect_include_candidates_ranked_for_test(
            crate::includes::IncludeForm::Quote,
            dir,
            "",
            Some("src"),
            Some(table),
            Some(&evidence),
            1000,
        ))
        .unwrap()
    }
    #[test]
    fn segmented_include_added_duplicate_keeps_full_rebuild_order() {
        let paths = vec!["a/foo.h".into(), "b/bar.h".into()];
        let edges = vec![
            ("src/a.c".into(), "a/foo.h".into()),
            ("src/b.c".into(), "b/bar.h".into()),
        ];
        let base = Arc::new(IncludeCompletionTable::build_with_edges(
            paths.clone(),
            edges.clone(),
        ));
        let added = vec!["z/foo.h".into()];
        let updated = base.updated_paths(&added, &added, &[], vec![]).unwrap();
        let mut all = paths;
        all.extend(added);
        let rebuilt = IncludeCompletionTable::build_with_edges(all, edges);
        assert_eq!(results(&updated, ""), results(&rebuilt, ""));
    }
    #[test]
    fn segmented_include_contributions_match_full_rebuild_after_delete_and_move() {
        let paths: Vec<String> = ["include/shared.h", "include/left.h", "src/a.c", "src/b.c"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let base = Arc::new(IncludeCompletionTable::build_with_edges(
            paths.clone(),
            vec![
                ("src/a.c".into(), "include/shared.h".into()),
                ("src/b.c".into(), "include/shared.h".into()),
            ],
        ));
        let next = base
            .updated_paths(&[], &[], &["src/a.c".into()], Vec::new())
            .unwrap();
        assert_eq!(
            next.edge_count(),
            1,
            "the other source still contributes the edge"
        );
        let fresh = IncludeCompletionTable::build_with_edges(
            paths.clone(),
            vec![("src/b.c".into(), "include/shared.h".into())],
        );
        for dir in ["", "include/", "src/"] {
            assert_eq!(results(&next, dir), results(&fresh, dir));
        }
        let moved = next
            .updated_paths(
                &["include/left.h".into(), "new/left.h".into()],
                &["new/left.h".into()],
                &[],
                Vec::new(),
            )
            .unwrap();
        let fresh = IncludeCompletionTable::build_with_edges(
            vec![
                "include/shared.h".into(),
                "new/left.h".into(),
                "src/a.c".into(),
                "src/b.c".into(),
            ],
            vec![("src/b.c".into(), "include/shared.h".into())],
        );
        assert!(base.shares_base_for_test(&moved));
        for dir in ["", "include/", "new/", "src/"] {
            assert_eq!(results(&moved, dir), results(&fresh, dir));
        }
    }
}
