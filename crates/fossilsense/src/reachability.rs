//! Limited `#include` reachability analysis.
//!
//! Two concerns, both kept free of `tower-lsp` request types so they unit-test
//! cleanly: (1) resolving a lexical `#include` target to the indexed file(s) it
//! names, and (2) computing, from the resolved file-to-file graph, the bounded
//! set of files reachable from a given file. The reachable set is the *scope*
//! that coloring and completion narrow their candidates to; a file whose include
//! picture we cannot fully resolve is marked "open" so callers soften the gate.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use crate::store::views::{IncludeEdgeRow, OpenIncludeRow};

/// Maximum include depth followed before a reachable set is declared "open".
pub const MAX_REACH_DEPTH: usize = 32;
/// Maximum number of files in a reachable set before it is declared "open".
pub const MAX_REACH_NODES: usize = 4096;

/// Why a reachable set is "open" (uncertain). Records the first cause detected
/// during the fixed-order BFS in [`ReachGraph::compute`]; a determinate (closed)
/// scope carries no reason. The reason explains the scope, never claims a
/// semantic binding. The fixed-cause precedence — applied when more than one
/// applies to the same node — is `UnresolvedInclude` before `AmbiguousInclude`,
/// both before the traversal caps (`DepthLimit` / `NodeLimit`), the latter two
/// detected during the BFS so they can only ever follow the include causes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenReason {
    /// A file in the reachable set has at least one unresolved `#include`.
    UnresolvedInclude,
    /// A file in the reachable set has an `#include` resolving to two or more
    /// candidate files with no exact-tier winner.
    AmbiguousInclude,
    /// Traversal reached `MAX_REACH_DEPTH` before exhausting the graph.
    DepthLimit,
    /// Traversal reached `MAX_REACH_NODES` before exhausting the graph.
    NodeLimit,
}

/// The bounded set of files reachable from a start file, plus whether the set is
/// "open" (uncertain) because some file in it has an unresolved include or a
/// traversal cap was hit. `reason` explains an open scope (first cause only); a
/// determinate scope keeps `open = false, reason = None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReachScope {
    /// Reachable file paths, including the start file itself.
    pub files: HashSet<String>,
    /// True when reachability could not be proven complete.
    pub open: bool,
    /// The first cause that opened the scope; `None` when the scope is
    /// determinate. Stable for a given graph generation (BFS visits in a fixed
    /// order).
    pub reason: Option<OpenReason>,
}

/// In-memory file-to-file include graph with a memoized reachable-set cache.
///
/// One graph is built per workspace from the store after each index pass; a new
/// graph instance is a fresh "generation", so its cache starts empty and old
/// memoized sets are discarded simply by replacing the `Arc`.
pub struct ReachGraph {
    edges: HashMap<String, Vec<String>>,
    /// First-cause `OpenReason` for every "open" node (an empty set means a
    /// determinate closure). A node that is both unresolved and ambiguous is
    /// stored once, under `UnresolvedInclude`, per the documented precedence.
    open: HashMap<String, OpenReason>,
    cache: Mutex<HashMap<String, Arc<ReachScope>>>,
}

impl ReachGraph {
    /// Build from resolved `(src_path, dst_path)` edges and the open-node
    /// inputs: files with at least one unresolved `#include` and files with at
    /// least one ambiguous (multi-hit, no exact-tier winner) `#include`. A node
    /// present in both lists is recorded under `UnresolvedInclude` (the
    /// stronger statement of incompleteness).
    pub fn new(
        edge_pairs: Vec<(String, String)>,
        unresolved_files: Vec<String>,
        ambiguous_files: Vec<String>,
    ) -> Self {
        let mut edges: HashMap<String, Vec<String>> = HashMap::new();
        for (src, dst) in edge_pairs {
            edges.entry(src).or_default().push(dst);
        }
        let mut open: HashMap<String, OpenReason> = HashMap::new();
        for path in ambiguous_files {
            open.insert(path, OpenReason::AmbiguousInclude);
        }
        // Unresolved wins on precedence — overwrite any AmbiguousInclude entry.
        for path in unresolved_files {
            open.insert(path, OpenReason::UnresolvedInclude);
        }
        Self {
            edges,
            open,
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn from_rows(
        edge_rows: Vec<IncludeEdgeRow>,
        unresolved_rows: Vec<OpenIncludeRow>,
        ambiguous_rows: Vec<OpenIncludeRow>,
    ) -> Self {
        let edge_pairs = edge_rows
            .into_iter()
            .map(|row| (row.source_path, row.target_path))
            .collect();
        let unresolved_files = unresolved_rows
            .into_iter()
            .map(|row| row.source_path)
            .collect();
        let ambiguous_files = ambiguous_rows
            .into_iter()
            .map(|row| row.source_path)
            .collect();
        Self::new(edge_pairs, unresolved_files, ambiguous_files)
    }

    /// Replace the out-edges and open flags for the given source paths, clearing
    /// the memoized reachable-set cache so subsequent queries recompute from the
    /// updated graph state. Sources not in `sources` retain their current edges
    /// and open flags. After refresh, the graph produces the same `ReachScope`
    /// that a full rebuild from the store would produce.
    ///
    /// `edges` are `(src, dst)` pairs for the sources being refreshed; any
    /// existing edge originating at one of `sources` is removed before the new
    /// edges are added. `open` are `(src, OpenReason)` pairs for sources whose
    /// open status changed; a source not listed here has its open flag removed.
    pub fn refresh_sources(
        &mut self,
        sources: &[String],
        edges: Vec<(String, String)>,
        open: Vec<(String, crate::reachability::OpenReason)>,
    ) {
        // Remove stale out-edges for the refreshed sources.
        for src in sources {
            self.edges.remove(src);
            self.open.remove(src);
        }

        // Insert new edges.
        for (src, dst) in edges {
            self.edges.entry(src).or_default().push(dst);
        }

        // Apply open flags with UnresolvedInclude > AmbiguousInclude precedence.
        for (path, reason) in open {
            match reason {
                crate::reachability::OpenReason::UnresolvedInclude => {
                    self.open
                        .insert(path, crate::reachability::OpenReason::UnresolvedInclude);
                }
                crate::reachability::OpenReason::AmbiguousInclude => {
                    self.open
                        .entry(path)
                        .or_insert(crate::reachability::OpenReason::AmbiguousInclude);
                }
                _ => {
                    self.open.entry(path).or_insert(reason);
                }
            }
        }

        // Clear the cache so subsequent reachable() calls recompute.
        self.cache = Mutex::new(HashMap::new());
    }

    pub fn refresh_sources_from_rows(
        &mut self,
        sources: &[String],
        edges: Vec<IncludeEdgeRow>,
        open: Vec<OpenIncludeRow>,
    ) {
        self.refresh_sources(
            sources,
            edges
                .into_iter()
                .map(|row| (row.source_path, row.target_path))
                .collect(),
            open.into_iter()
                .map(|row| (row.source_path, row.reason))
                .collect(),
        );
    }

    /// Reachable set for `start`, memoized for this graph generation.
    pub fn reachable(&self, start: &str) -> Arc<ReachScope> {
        if let Some(hit) = self.cache.lock().unwrap().get(start) {
            return hit.clone();
        }
        let scope = Arc::new(self.compute(start));
        self.cache
            .lock()
            .unwrap()
            .insert(start.to_string(), scope.clone());
        scope
    }

    fn compute(&self, start: &str) -> ReachScope {
        let mut files = HashSet::new();
        files.insert(start.to_string());
        let mut open = false;
        let mut reason: Option<OpenReason> = None;

        // The start node's own open status is the first possible cause: a file
        // with an unresolved or ambiguous include opens the scope immediately.
        // First-cause precedence (UnresolvedInclude before AmbiguousInclude)
        // is encoded by the order `ReachGraph::new` writes the `open` map: a
        // node present in both lists is stored under `UnresolvedInclude`.
        if let Some(cause) = self.open.get(start) {
            open = true;
            reason = Some(*cause);
        }

        let mark_open = |open: &mut bool, cause: OpenReason, reason: &mut Option<OpenReason>| {
            *open = true;
            if reason.is_none() {
                *reason = Some(cause);
            }
        };

        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
        queue.push_back((start.to_string(), 0));
        while let Some((node, depth)) = queue.pop_front() {
            if depth >= MAX_REACH_DEPTH {
                // Stop descending; we cannot prove what lies deeper.
                mark_open(&mut open, OpenReason::DepthLimit, &mut reason);
                continue;
            }
            let Some(dsts) = self.edges.get(&node) else {
                continue;
            };
            for dst in dsts {
                if files.len() >= MAX_REACH_NODES {
                    mark_open(&mut open, OpenReason::NodeLimit, &mut reason);
                    break;
                }
                if files.insert(dst.clone()) {
                    if let Some(cause) = self.open.get(dst) {
                        mark_open(&mut open, *cause, &mut reason);
                    }
                    queue.push_back((dst.clone(), depth + 1));
                }
            }
        }

        ReachScope {
            files,
            open,
            reason,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(values: &[&str]) -> HashSet<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn reachable_includes_transitive_files() {
        // a.c -> b.h -> c.h ; all resolved.
        let graph = ReachGraph::new(
            vec![("a.c".into(), "b.h".into()), ("b.h".into(), "c.h".into())],
            vec![],
            vec![],
        );
        let scope = graph.reachable("a.c");
        assert_eq!(scope.files, set(&["a.c", "b.h", "c.h"]));
        assert!(!scope.open, "fully resolved closure is determinate");
        assert!(
            scope.reason.is_none(),
            "a determinate scope carries no reason"
        );
    }

    #[test]
    fn unresolved_in_closure_marks_open() {
        // a.c -> b.h, and b.h has an unresolved include.
        let graph = ReachGraph::new(
            vec![("a.c".into(), "b.h".into())],
            vec!["b.h".into()],
            vec![],
        );
        let scope = graph.reachable("a.c");
        assert!(scope.files.contains("b.h"));
        assert!(scope.open, "an unresolved include in the closure opens it");
        assert_eq!(
            scope.reason,
            Some(OpenReason::UnresolvedInclude),
            "an unresolved include is the open reason"
        );
    }

    #[test]
    fn ambiguous_in_closure_marks_open_with_ambiguous_reason() {
        // a.c -> b.h, and b.h has an ambiguous (multi-hit) include.
        let graph = ReachGraph::new(
            vec![("a.c".into(), "b.h".into())],
            vec![],
            vec!["b.h".into()],
        );
        let scope = graph.reachable("a.c");
        assert!(scope.files.contains("b.h"));
        assert!(scope.open, "an ambiguous include in the closure opens it");
        assert_eq!(
            scope.reason,
            Some(OpenReason::AmbiguousInclude),
            "an ambiguous include is the open reason"
        );
    }

    #[test]
    fn start_file_unresolved_include_is_first_reason() {
        // The start file itself is open: that is the first cause.
        let graph = ReachGraph::new(vec![], vec!["a.c".into()], vec![]);
        let scope = graph.reachable("a.c");
        assert!(scope.open);
        assert_eq!(scope.reason, Some(OpenReason::UnresolvedInclude));
    }

    #[test]
    fn start_file_ambiguous_include_is_reason() {
        let graph = ReachGraph::new(vec![], vec![], vec!["a.c".into()]);
        let scope = graph.reachable("a.c");
        assert!(scope.open);
        assert_eq!(scope.reason, Some(OpenReason::AmbiguousInclude));
    }

    #[test]
    fn ambiguous_without_edges_is_determinate() {
        // Determinate scope (no open node, all closure edges resolved):
        // `open = false, reason = None`.
        let graph = ReachGraph::new(vec![], vec![], vec![]);
        let scope = graph.reachable("lonely.c");
        assert_eq!(scope.files, set(&["lonely.c"]));
        assert!(!scope.open);
        assert!(scope.reason.is_none());
    }

    #[test]
    fn unresolved_takes_precedence_over_ambiguous_for_same_node() {
        // A node that is both unresolved and ambiguous reports UnresolvedInclude.
        let graph = ReachGraph::new(vec![], vec!["a.c".into()], vec!["a.c".into()]);
        let scope = graph.reachable("a.c");
        assert!(scope.open);
        assert_eq!(
            scope.reason,
            Some(OpenReason::UnresolvedInclude),
            "UnresolvedInclude precedes AmbiguousInclude for the same node"
        );
    }

    #[test]
    fn unresolved_takes_precedence_over_ambiguous_in_closure() {
        // a.c -> b.h, b.h is both unresolved and ambiguous in this generation.
        let graph = ReachGraph::new(
            vec![("a.c".into(), "b.h".into())],
            vec!["b.h".into()],
            vec!["b.h".into()],
        );
        let scope = graph.reachable("a.c");
        assert!(scope.open, "the open set must stay open under either cause");
        assert_eq!(
            scope.reason,
            Some(OpenReason::UnresolvedInclude),
            "precedence: unresolved before ambiguous in the closure"
        );
    }

    #[test]
    fn depth_cap_opens_with_depth_limit_reason() {
        // A chain deeper than MAX_REACH_DEPTH forces the scope open at the cap.
        // a0 -> a1 -> ... -> a_{MAX_REACH_DEPTH+1}; no unresolved includes.
        let edges: Vec<(String, String)> = (0..=MAX_REACH_DEPTH)
            .map(|i| (format!("a{i}.h"), format!("a{}.h", i + 1)))
            .collect();
        let graph = ReachGraph::new(edges, vec![], vec![]);
        let scope = graph.reachable("a0.h");
        assert!(scope.open, "depth cap opens the scope");
        assert_eq!(
            scope.reason,
            Some(OpenReason::DepthLimit),
            "depth cap is the open reason"
        );
    }

    #[test]
    fn node_cap_opens_with_node_limit_reason() {
        // A star graph wider than MAX_REACH_NODES forces the scope open at the cap.
        // start -> d0..d{MAX_REACH_NODES} (one more than the cap), no unresolved.
        let edges: Vec<(String, String)> = (0..=MAX_REACH_NODES)
            .map(|i| ("start.h".to_string(), format!("d{i}.h")))
            .collect();
        let graph = ReachGraph::new(edges, vec![], vec![]);
        let scope = graph.reachable("start.h");
        assert!(scope.open, "node cap opens the scope");
        assert_eq!(
            scope.reason,
            Some(OpenReason::NodeLimit),
            "node cap is the open reason"
        );
    }

    #[test]
    fn first_cause_is_reported_deterministically() {
        // Two open conditions apply: the start node has an unresolved include,
        // and the chain is deeper than MAX_REACH_DEPTH. The start-node cause is
        // detected before any depth cap, so it wins and stays stable on repeats.
        let edges: Vec<(String, String)> = (0..=MAX_REACH_DEPTH)
            .map(|i| (format!("a{i}.h"), format!("a{}.h", i + 1)))
            .collect();
        let graph = ReachGraph::new(edges, vec!["a0.h".into()], vec![]);
        let first = graph.reachable("a0.h");
        let second = graph.reachable("a0.h");
        assert_eq!(first.reason, second.reason, "first cause is stable");
        assert_eq!(
            first.reason,
            Some(OpenReason::UnresolvedInclude),
            "start-node unresolved include is the first cause, not the depth cap"
        );
    }

    #[test]
    fn ambiguous_before_depth_cap_when_start_node_ambiguous() {
        // Start node is ambiguous and the chain is deeper than the cap:
        // the start-node cause (AmbiguousInclude) wins over the depth cap.
        let edges: Vec<(String, String)> = (0..=MAX_REACH_DEPTH)
            .map(|i| (format!("a{i}.h"), format!("a{}.h", i + 1)))
            .collect();
        let graph = ReachGraph::new(edges, vec![], vec!["a0.h".into()]);
        let scope = graph.reachable("a0.h");
        assert_eq!(scope.reason, Some(OpenReason::AmbiguousInclude));
    }

    #[test]
    fn start_file_without_edges_is_itself_only() {
        let graph = ReachGraph::new(vec![], vec![], vec![]);
        let scope = graph.reachable("lonely.c");
        assert_eq!(scope.files, set(&["lonely.c"]));
        assert!(!scope.open);
    }

    #[test]
    fn cycles_terminate() {
        let graph = ReachGraph::new(
            vec![("a.h".into(), "b.h".into()), ("b.h".into(), "a.h".into())],
            vec![],
            vec![],
        );
        let scope = graph.reachable("a.h");
        assert_eq!(scope.files, set(&["a.h", "b.h"]));
        assert!(!scope.open);
    }

    #[test]
    fn cache_returns_consistent_scope() {
        let graph = ReachGraph::new(vec![("a.c".into(), "b.h".into())], vec![], vec![]);
        let first = graph.reachable("a.c");
        let second = graph.reachable("a.c");
        assert_eq!(first, second);
    }

    // --- R7: error degradation — empty/malformed ReachGraph must be safe ------

    #[test]
    fn empty_reach_graph_is_well_formed() {
        let graph = ReachGraph::new(vec![], vec![], vec![]);
        let scope = graph.reachable("any_file.c");
        assert!(!scope.open, "empty graph yields determinate (closed) scope");
        assert_eq!(scope.files.len(), 1);
        assert!(
            scope.files.contains("any_file.c"),
            "start file is always in scope"
        );
        // Query a different start file — also safe.
        let scope2 = graph.reachable("other.c");
        assert!(!scope2.open);
        assert!(scope2.files.contains("other.c"));
    }

    #[test]
    fn reach_graph_with_orphan_edges_is_safe() {
        // Edges referencing nonexistent start files (never in the graph)
        // should not cause BFS to panic.
        let graph = ReachGraph::new(
            vec![
                ("a.c".into(), "b.h".into()),
                ("ghost.c".into(), "phantom.h".into()),
            ],
            vec![],
            vec![],
        );
        let scope = graph.reachable("a.c");
        assert!(!scope.open);
        assert!(scope.files.contains("a.c"));
        assert!(scope.files.contains("b.h"));
        // ghost.c's edge doesn't cause trouble since BFS starts from a.c.
    }

    #[test]
    fn reach_graph_with_unresolved_and_ambiguous_same_node() {
        // Unresolved takes precedence over Ambiguous per ReachGraph::new.
        let graph = ReachGraph::new(
            vec![],
            vec!["open.c".into()], // unresolved
            vec!["open.c".into()], // ambiguous (overwritten)
        );
        let scope = graph.reachable("open.c");
        assert!(scope.open);
        assert_eq!(scope.reason, Some(OpenReason::UnresolvedInclude));
    }

    // --- Phase 5: reach graph incremental refresh ---------------------------

    #[test]
    fn refresh_removes_stale_edge() {
        let mut graph = ReachGraph::new(vec![("a.c".into(), "old.h".into())], vec![], vec![]);
        let scope_before = graph.reachable("a.c");
        assert!(scope_before.files.contains("old.h"));

        // Refresh a.c with no edges — old edge to old.h should be gone.
        graph.refresh_sources(&["a.c".to_string()], vec![], vec![]);
        let scope_after = graph.reachable("a.c");
        assert!(
            !scope_after.files.contains("old.h"),
            "stale edge must be removed"
        );
    }

    #[test]
    fn refresh_adds_new_edge_and_updates_open_reason() {
        let mut graph = ReachGraph::new(
            vec![("a.c".into(), "old.h".into())],
            vec!["a.c".into()], // unresolved
            vec![],
        );
        let scope_before = graph.reachable("a.c");
        assert_eq!(scope_before.reason, Some(OpenReason::UnresolvedInclude));

        // Refresh: old.h -> new.h, unresolved -> resolved (no open).
        graph.refresh_sources(
            &["a.c".to_string()],
            vec![("a.c".to_string(), "new.h".to_string())],
            vec![], // no open
        );
        let scope_after = graph.reachable("a.c");
        assert!(scope_after.files.contains("new.h"));
        assert!(!scope_after.files.contains("old.h"));
        assert!(!scope_after.open, "open flag cleared by refresh");
        assert!(scope_after.reason.is_none());
    }

    #[test]
    fn refresh_changes_open_from_ambiguous_to_resolved() {
        let mut graph = ReachGraph::new(
            vec![("x.c".into(), "lib.h".into())],
            vec![],
            vec!["x.c".into()], // ambiguous
        );
        assert_eq!(
            graph.reachable("x.c").reason,
            Some(OpenReason::AmbiguousInclude)
        );

        // Refresh x.c with no open flags → becomes determinate.
        graph.refresh_sources(
            &["x.c".to_string()],
            vec![("x.c".to_string(), "lib.h".to_string())],
            vec![], // open cleared
        );
        let scope = graph.reachable("x.c");
        assert!(!scope.open);
        assert!(scope.reason.is_none());
    }

    #[test]
    fn refresh_clears_memoized_cache() {
        let mut graph = ReachGraph::new(vec![("a.c".into(), "b.h".into())], vec![], vec![]);
        let first = graph.reachable("a.c");
        assert!(first.files.contains("b.h"));

        // Refresh: change edges, cache must be invalidated.
        graph.refresh_sources(
            &["a.c".to_string()],
            vec![("a.c".to_string(), "c.h".to_string())],
            vec![],
        );
        let second = graph.reachable("a.c");
        assert!(second.files.contains("c.h"));
        assert!(!second.files.contains("b.h"));
        assert_ne!(first, second, "refreshed scope must differ from cached");
    }

    #[test]
    fn refresh_preserves_other_sources() {
        let mut graph = ReachGraph::new(
            vec![("a.c".into(), "b.h".into()), ("d.c".into(), "e.h".into())],
            vec![],
            vec![],
        );

        // Only refresh a.c — d.c should keep its edges.
        graph.refresh_sources(
            &["a.c".to_string()],
            vec![], // remove all a.c edges
            vec![],
        );

        let scope_a = graph.reachable("a.c");
        assert!(!scope_a.files.contains("b.h"), "a.c edges removed");

        let scope_d = graph.reachable("d.c");
        assert!(scope_d.files.contains("e.h"), "d.c edges preserved");
    }

    #[test]
    fn refresh_unresolved_takes_precedence_over_ambiguous() {
        let mut graph = ReachGraph::new(vec![], vec![], vec![]);

        graph.refresh_sources(
            &["open.c".to_string()],
            vec![],
            vec![
                ("open.c".to_string(), OpenReason::AmbiguousInclude),
                ("open.c".to_string(), OpenReason::UnresolvedInclude),
            ],
        );

        let scope = graph.reachable("open.c");
        assert!(scope.open);
        assert_eq!(
            scope.reason,
            Some(OpenReason::UnresolvedInclude),
            "UnresolvedInclude must take precedence"
        );
    }
}
