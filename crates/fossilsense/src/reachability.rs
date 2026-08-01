//! Limited `#include` reachability analysis.
//!
//! Two concerns, both kept free of `tower-lsp` request types so they unit-test
//! cleanly: (1) resolving a lexical `#include` target to the indexed file(s) it
//! names, and (2) computing, from the resolved file-to-file graph, the bounded
//! set of files reachable from a given file. The reachable set is the *scope*
//! that coloring and completion narrow their candidates to; a file whose include
//! picture we cannot fully resolve is marked "open" so callers soften the gate.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::includes::ResolutionKind;
use crate::store::views::{
    GoOpenPackageRow, GoPackageEdgeRow, GoPackageFileRow, GoPackageResolution, IncludeEdgeRow,
    OpenIncludeRow,
};

/// Maximum include depth followed before a reachable set is declared "open".
pub const MAX_REACH_DEPTH: usize = 32;
/// Maximum number of files in a reachable set before it is declared "open".
pub const MAX_REACH_NODES: usize = 4096;
/// Maximum Go package nodes visited by one request.
pub const MAX_REACH_PACKAGES: usize = 4096;
/// Maximum Go package dependency edges scanned by one request.
pub const MAX_REACH_PACKAGE_EDGES: usize = 16_384;

/// Why a reachable set is "open" (uncertain). Records the first cause detected
/// during the fixed-order BFS in [`ReachGraph::compute`]; a determinate (closed)
/// scope carries no reason. The reason explains the scope, never claims a
/// semantic binding. The fixed-cause precedence — applied when more than one
/// applies to the same node — is `UnsupportedLanguageBoundary`, then
/// `UnresolvedInclude`, `AmbiguousInclude`, and `BuildConstraintUnknown`, all
/// before the traversal caps (`DepthLimit` / `NodeLimit`). The latter two are
/// detected during the BFS so they can only ever follow graph evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenReason {
    /// A file in the reachable set has at least one unresolved `#include`.
    UnresolvedInclude,
    /// A file in the reachable set has an `#include` resolving to two or more
    /// candidate files with no exact-tier winner.
    AmbiguousInclude,
    /// A Go package imports the cgo pseudo-package `C`. FossilSense records the
    /// boundary but deliberately does not infer C declarations from Go.
    UnsupportedLanguageBoundary,
    /// A Go package contains files guarded by build expressions or filename
    /// target suffixes, and no active target evidence proves which variants
    /// participate. All variants remain candidates.
    BuildConstraintUnknown,
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
    /// File paths reached only through exact-resolution edges, including the
    /// start file itself.
    pub files: HashSet<String>,
    /// File paths whose best known path crosses at least one suffix-match edge.
    /// These are recall hints, not compiler-level reachability proof.
    pub heuristic_files: HashSet<String>,
    /// True when reachability could not be proven complete.
    pub open: bool,
    /// The first cause that opened the scope; `None` when the scope is
    /// determinate. Stable for a given graph generation (BFS visits in a fixed
    /// order).
    pub reason: Option<OpenReason>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReachEdge {
    target: String,
    resolution: ResolutionKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PackageReachEdge {
    target: String,
    heuristic: bool,
}

/// Sparse request-local replacement data layered over one immutable published
/// graph. Only dirty source paths and affected Go packages live here; every
/// untouched lookup falls through to `ReachGraph::base` without copying the
/// workspace graph.
#[derive(Debug)]
struct ReachGraphOverlay {
    sources: HashSet<String>,
    edges: HashMap<String, Vec<ReachEdge>>,
    open: HashMap<String, OpenReason>,
    package_by_file: HashMap<String, Option<String>>,
    affected_packages: HashSet<String>,
    open_packages: HashMap<String, OpenReason>,
    direct_external_source_counts: HashMap<String, usize>,
}

fn is_strong_resolution(resolution: ResolutionKind) -> bool {
    matches!(
        resolution,
        ResolutionKind::RelativeExact
            | ResolutionKind::WorkspaceExact
            | ResolutionKind::ExternalExact
    )
}

fn is_direct_external_edge(edge: &ReachEdge) -> bool {
    edge.resolution == ResolutionKind::ExternalExact
        && Path::new(edge.target.as_str()).is_absolute()
}

fn normalize_reach_edges(edges: &mut Vec<ReachEdge>) {
    // Direct external targets are the only request-time first-layer projection.
    // Keep them first so the projection can stop after MAX_REACH_NODES examined
    // edges without losing a later direct target behind unrelated fanout.
    edges.sort_by(|left, right| {
        (!is_direct_external_edge(left))
            .cmp(&!is_direct_external_edge(right))
            .then_with(|| left.target.cmp(&right.target))
            .then_with(|| left.resolution.as_str().cmp(right.resolution.as_str()))
    });
    edges.dedup();
}

fn direct_external_source_counts(
    edges: &HashMap<String, Vec<ReachEdge>>,
) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for (source, targets) in edges {
        if Path::new(source).is_absolute() {
            continue;
        }
        for edge in targets.iter().filter(|edge| is_direct_external_edge(edge)) {
            *counts.entry(edge.target.clone()).or_default() += 1;
        }
    }
    counts
}

fn insert_open_reason(open: &mut HashMap<String, OpenReason>, path: String, reason: OpenReason) {
    match reason {
        OpenReason::UnresolvedInclude => {
            open.insert(path, OpenReason::UnresolvedInclude);
        }
        OpenReason::AmbiguousInclude => {
            open.entry(path).or_insert(OpenReason::AmbiguousInclude);
        }
        _ => {
            open.entry(path).or_insert(reason);
        }
    }
}

#[cfg(test)]
fn legacy_strong_resolution(target: &str) -> ResolutionKind {
    if Path::new(target).is_absolute() {
        ResolutionKind::ExternalExact
    } else {
        ResolutionKind::WorkspaceExact
    }
}

/// In-memory file-to-file include graph with a memoized reachable-set cache.
///
/// One graph is built per workspace from the store after each index pass; a new
/// graph instance is a fresh "generation", so its cache starts empty and old
/// memoized sets are discarded simply by replacing the `Arc`. Published graphs
/// are immutable: an incremental refresh is prepared as a new graph so requests
/// holding an older engine snapshot cannot observe a mixed generation.
#[derive(Debug)]
pub struct ReachGraph {
    edges: HashMap<String, Vec<ReachEdge>>,
    /// First-cause `OpenReason` for every "open" node (an empty set means a
    /// determinate closure). A node that is both unresolved and ambiguous is
    /// stored once, under `UnresolvedInclude`, per the documented precedence.
    open: HashMap<String, OpenReason>,
    package_by_file: HashMap<String, String>,
    files_by_package: HashMap<String, Vec<String>>,
    package_edges: HashMap<String, Vec<PackageReachEdge>>,
    open_packages: HashMap<String, OpenReason>,
    direct_external_source_counts: HashMap<String, usize>,
    base: Option<Arc<ReachGraph>>,
    overlay: Option<ReachGraphOverlay>,
    cache: Mutex<HashMap<String, Arc<ReachScope>>>,
}

impl ReachGraph {
    /// Build from resolved `(src_path, dst_path)` edges and the open-node
    /// inputs: files with at least one unresolved `#include` and files with at
    /// least one ambiguous (multi-hit, no exact-tier winner) `#include`. A node
    /// present in both lists is recorded under `UnresolvedInclude` (the
    /// stronger statement of incompleteness).
    #[cfg(test)]
    pub fn new(
        edge_pairs: Vec<(String, String)>,
        unresolved_files: Vec<String>,
        ambiguous_files: Vec<String>,
    ) -> Self {
        Self::new_with_kinds(
            edge_pairs
                .into_iter()
                .map(|(src, dst)| {
                    let resolution = legacy_strong_resolution(&dst);
                    (src, dst, resolution)
                })
                .collect(),
            unresolved_files,
            ambiguous_files,
        )
    }

    /// Build from resolution-aware edges. The legacy [`ReachGraph::new`]
    /// constructor remains a strong-edge compatibility entry point for tests
    /// and callers that already proved their pairs elsewhere.
    pub(crate) fn new_with_kinds(
        edge_rows: Vec<(String, String, ResolutionKind)>,
        unresolved_files: Vec<String>,
        ambiguous_files: Vec<String>,
    ) -> Self {
        let mut edges: HashMap<String, Vec<ReachEdge>> = HashMap::new();
        for (src, dst, resolution) in edge_rows {
            edges.entry(src).or_default().push(ReachEdge {
                target: dst,
                resolution,
            });
        }
        for targets in edges.values_mut() {
            normalize_reach_edges(targets);
        }
        let direct_external_source_counts = direct_external_source_counts(&edges);
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
            package_by_file: HashMap::new(),
            files_by_package: HashMap::new(),
            package_edges: HashMap::new(),
            open_packages: HashMap::new(),
            direct_external_source_counts,
            base: None,
            overlay: None,
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn from_rows(
        edge_rows: Vec<IncludeEdgeRow>,
        unresolved_rows: Vec<OpenIncludeRow>,
        ambiguous_rows: Vec<OpenIncludeRow>,
    ) -> Self {
        let edges = edge_rows
            .into_iter()
            .map(|row| (row.source_path, row.target_path, row.resolution))
            .collect();
        let unresolved_files = unresolved_rows
            .into_iter()
            .map(|row| row.source_path)
            .collect();
        let ambiguous_files = ambiguous_rows
            .into_iter()
            .map(|row| row.source_path)
            .collect();
        Self::new_with_kinds(edges, unresolved_files, ambiguous_files)
    }

    pub fn from_rows_with_packages(
        edge_rows: Vec<IncludeEdgeRow>,
        unresolved_rows: Vec<OpenIncludeRow>,
        ambiguous_rows: Vec<OpenIncludeRow>,
        package_files: Vec<GoPackageFileRow>,
        package_edges: Vec<GoPackageEdgeRow>,
        open_packages: Vec<GoOpenPackageRow>,
    ) -> Self {
        let mut graph = Self::from_rows(edge_rows, unresolved_rows, ambiguous_rows);
        for row in package_files {
            graph
                .package_by_file
                .insert(row.path.clone(), row.package_key.clone());
            graph
                .files_by_package
                .entry(row.package_key)
                .or_default()
                .push(row.path);
        }
        for files in graph.files_by_package.values_mut() {
            files.sort();
            files.dedup();
        }
        for row in package_edges {
            graph
                .package_edges
                .entry(row.source_package_key)
                .or_default()
                .push(PackageReachEdge {
                    target: row.target_package_key,
                    heuristic: row.resolution == GoPackageResolution::Heuristic,
                });
        }
        for edges in graph.package_edges.values_mut() {
            edges.sort_by(|left, right| {
                left.target
                    .cmp(&right.target)
                    .then_with(|| left.heuristic.cmp(&right.heuristic))
            });
            edges.dedup();
        }
        graph.open_packages = open_packages
            .into_iter()
            .map(|row| (row.package_key, row.reason))
            .collect();
        graph
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
    #[cfg(test)]
    pub fn refresh_sources(
        &mut self,
        sources: &[String],
        edges: Vec<(String, String)>,
        open: Vec<(String, crate::reachability::OpenReason)>,
    ) {
        self.refresh_sources_with_kinds(
            sources,
            edges
                .into_iter()
                .map(|(src, dst)| {
                    let resolution = legacy_strong_resolution(&dst);
                    (src, dst, resolution)
                })
                .collect(),
            open,
        );
    }

    pub(crate) fn refresh_sources_with_kinds(
        &mut self,
        sources: &[String],
        edges: Vec<(String, String, ResolutionKind)>,
        open: Vec<(String, crate::reachability::OpenReason)>,
    ) {
        debug_assert!(self.base.is_none() && self.overlay.is_none());
        for source in sources {
            if Path::new(source).is_absolute() {
                continue;
            }
            let removed = self
                .edges
                .get(source)
                .into_iter()
                .flatten()
                .filter(|edge| is_direct_external_edge(edge))
                .map(|edge| edge.target.clone())
                .collect::<Vec<_>>();
            for target in removed {
                if let Some(count) = self.direct_external_source_counts.get_mut(&target) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        self.direct_external_source_counts.remove(&target);
                    }
                }
            }
        }
        // Remove stale out-edges for the refreshed sources.
        for src in sources {
            self.edges.remove(src);
            self.open.remove(src);
        }

        // Insert new edges.
        for (src, dst, resolution) in edges {
            self.edges.entry(src).or_default().push(ReachEdge {
                target: dst,
                resolution,
            });
        }
        for source in sources {
            if let Some(targets) = self.edges.get_mut(source) {
                normalize_reach_edges(targets);
            }
        }
        for source in sources {
            if Path::new(source).is_absolute() {
                continue;
            }
            for edge in self
                .edges
                .get(source)
                .into_iter()
                .flatten()
                .filter(|edge| is_direct_external_edge(edge))
            {
                *self
                    .direct_external_source_counts
                    .entry(edge.target.clone())
                    .or_default() += 1;
            }
        }

        // Apply open flags with UnresolvedInclude > AmbiguousInclude precedence.
        for (path, reason) in open {
            insert_open_reason(&mut self.open, path, reason);
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
        self.refresh_sources_with_kinds(
            sources,
            edges
                .into_iter()
                .map(|row| (row.source_path, row.target_path, row.resolution))
                .collect(),
            open.into_iter()
                .map(|row| (row.source_path, row.reason))
                .collect(),
        );
    }

    /// Create the next immutable graph generation by applying a source-scoped
    /// refresh to a copy of this graph. The memoized reachability cache is not
    /// copied. Runtime publication uses this method instead of mutating a graph
    /// that may already be visible through an older engine snapshot.
    pub fn with_refreshed_sources_from_rows(
        &self,
        sources: &[String],
        edges: Vec<IncludeEdgeRow>,
        open: Vec<OpenIncludeRow>,
    ) -> Self {
        debug_assert!(self.base.is_none() && self.overlay.is_none());
        let mut next = Self {
            edges: self.edges.clone(),
            open: self.open.clone(),
            package_by_file: self.package_by_file.clone(),
            files_by_package: self.files_by_package.clone(),
            package_edges: self.package_edges.clone(),
            open_packages: self.open_packages.clone(),
            direct_external_source_counts: self.direct_external_source_counts.clone(),
            base: None,
            overlay: None,
            cache: Mutex::new(HashMap::new()),
        };
        next.refresh_sources_from_rows(sources, edges, open);
        next
    }

    /// Build a sparse request-local graph over an immutable published base.
    /// Every dirty source replaces its durable out-edges and open flag. Go
    /// package overlays additionally replace membership and invalidate only the
    /// affected packages; untouched C edges and Go package maps stay shared.
    pub(crate) fn with_request_overrides(
        base: Arc<Self>,
        sources: &[String],
        edges: Vec<(String, String, ResolutionKind)>,
        open: Vec<(String, OpenReason)>,
        go_overlays: Vec<(String, Option<(String, OpenReason)>)>,
    ) -> Self {
        let source_set: HashSet<String> = sources.iter().cloned().collect();
        let mut edge_overrides: HashMap<String, Vec<ReachEdge>> = HashMap::new();
        for (source, target, resolution) in edges {
            edge_overrides
                .entry(source)
                .or_default()
                .push(ReachEdge { target, resolution });
        }
        for source in sources {
            if let Some(targets) = edge_overrides.get_mut(source) {
                normalize_reach_edges(targets);
            }
        }
        let mut open_overrides = HashMap::new();
        for (path, reason) in open {
            insert_open_reason(&mut open_overrides, path, reason);
        }

        let mut direct_deltas: HashMap<String, isize> = HashMap::new();
        for source in &source_set {
            if Path::new(source).is_absolute() {
                continue;
            }
            for edge in base
                .edges_for(source)
                .into_iter()
                .flatten()
                .filter(|edge| is_direct_external_edge(edge))
            {
                *direct_deltas.entry(edge.target.clone()).or_default() -= 1;
            }
            for edge in edge_overrides
                .get(source)
                .into_iter()
                .flatten()
                .filter(|edge| is_direct_external_edge(edge))
            {
                *direct_deltas.entry(edge.target.clone()).or_default() += 1;
            }
        }
        let direct_external_source_counts = direct_deltas
            .into_iter()
            .map(|(target, delta)| {
                let count = (base.direct_external_source_count(&target) as isize + delta).max(0);
                (target, count as usize)
            })
            .collect();

        let mut overlay = ReachGraphOverlay {
            sources: source_set,
            edges: edge_overrides,
            open: open_overrides,
            package_by_file: HashMap::new(),
            affected_packages: HashSet::new(),
            open_packages: HashMap::new(),
            direct_external_source_counts,
        };
        let mut affected_packages = HashSet::new();
        for (path, package) in go_overlays {
            if let Some(old_package) = base.package_for_file(&path) {
                affected_packages.insert(old_package.to_string());
            }
            overlay.open.remove(&path);
            match package {
                Some((package_key, reason)) => {
                    affected_packages.insert(package_key.clone());
                    overlay
                        .package_by_file
                        .insert(path, Some(package_key.clone()));
                    overlay.open_packages.insert(package_key, reason);
                }
                None => {
                    overlay.package_by_file.insert(path.clone(), None);
                    overlay.open.insert(path, OpenReason::UnresolvedInclude);
                }
            }
        }
        for package in affected_packages {
            overlay.affected_packages.insert(package.clone());
            overlay
                .open_packages
                .entry(package)
                .or_insert(OpenReason::UnresolvedInclude);
        }
        Self {
            edges: HashMap::new(),
            open: HashMap::new(),
            package_by_file: HashMap::new(),
            files_by_package: HashMap::new(),
            package_edges: HashMap::new(),
            open_packages: HashMap::new(),
            direct_external_source_counts: HashMap::new(),
            base: Some(base),
            overlay: Some(overlay),
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn edges_for(&self, source: &str) -> Option<&[ReachEdge]> {
        if let Some(overlay) = &self.overlay {
            if overlay.sources.contains(source) {
                return Some(
                    overlay
                        .edges
                        .get(source)
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                );
            }
            return self.base.as_deref().and_then(|base| base.edges_for(source));
        }
        self.edges.get(source).map(Vec::as_slice)
    }

    fn open_reason(&self, path: &str) -> Option<OpenReason> {
        if let Some(overlay) = &self.overlay {
            if overlay.sources.contains(path) {
                return overlay.open.get(path).copied();
            }
            return self.base.as_deref().and_then(|base| base.open_reason(path));
        }
        self.open.get(path).copied()
    }

    fn package_for_file<'a>(&'a self, path: &str) -> Option<&'a str> {
        if let Some(overlay) = &self.overlay {
            if let Some(package) = overlay.package_by_file.get(path) {
                return package.as_deref();
            }
            return self
                .base
                .as_deref()
                .and_then(|base| base.package_for_file(path));
        }
        self.package_by_file.get(path).map(String::as_str)
    }

    #[cfg(test)]
    fn files_for_package<'a>(&'a self, package: &str) -> Cow<'a, [String]> {
        self.files_for_package_bounded(package, usize::MAX).0
    }

    fn files_for_package_bounded<'a>(
        &'a self,
        package: &str,
        limit: usize,
    ) -> (Cow<'a, [String]>, bool) {
        let Some(overlay) = &self.overlay else {
            if let Some(base) = self.base.as_deref() {
                return base.files_for_package_bounded(package, limit);
            }
            let files = self
                .files_by_package
                .get(package)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let retained = files.len().min(limit);
            return (Cow::Borrowed(&files[..retained]), files.len() > retained);
        };
        if !overlay.affected_packages.contains(package) {
            return self.base.as_deref().map_or_else(
                || (Cow::Borrowed(&[] as &[String]), false),
                |base| base.files_for_package_bounded(package, limit),
            );
        }

        let mut files = overlay
            .package_by_file
            .iter()
            .filter(|(_, effective_package)| effective_package.as_deref() == Some(package))
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        files.sort_unstable();
        files.dedup();
        let overlay_files: HashSet<String> = files.iter().cloned().collect();
        let mut truncated = files.len() > limit;
        files.truncate(limit);

        if let Some(base) = self.base.as_deref() {
            let base_scan_limit = limit.saturating_add(overlay.package_by_file.len());
            let (base_files, base_truncated) =
                base.files_for_package_bounded(package, base_scan_limit);
            truncated |= base_truncated;
            for path in base_files.iter() {
                if self.package_for_file(path) != Some(package) || overlay_files.contains(path) {
                    continue;
                }
                if files.len() >= limit {
                    truncated = true;
                    break;
                }
                files.push(path.clone());
            }
        }
        files.sort_unstable();
        (Cow::Owned(files), truncated)
    }

    fn package_edges_for(&self, package: &str) -> Option<&[PackageReachEdge]> {
        if let Some(overlay) = &self.overlay {
            if overlay.affected_packages.contains(package) {
                return Some(&[]);
            }
            return self
                .base
                .as_deref()
                .and_then(|base| base.package_edges_for(package));
        }
        self.package_edges.get(package).map(Vec::as_slice)
    }

    fn open_package_reason(&self, package: &str) -> Option<OpenReason> {
        if let Some(overlay) = &self.overlay {
            if overlay.affected_packages.contains(package) {
                return overlay.open_packages.get(package).copied();
            }
            return self
                .base
                .as_deref()
                .and_then(|base| base.open_package_reason(package));
        }
        self.open_packages.get(package).copied()
    }

    pub(crate) fn direct_external_source_count(&self, target: &str) -> usize {
        if let Some(overlay) = &self.overlay {
            if let Some(count) = overlay.direct_external_source_counts.get(target) {
                return *count;
            }
            return self
                .base
                .as_deref()
                .map_or(0, |base| base.direct_external_source_count(target));
        }
        self.direct_external_source_counts
            .get(target)
            .copied()
            .unwrap_or_default()
    }

    pub(crate) fn direct_external_presence_overrides(&self) -> HashMap<String, bool> {
        let (Some(base), Some(overlay)) = (self.base.as_deref(), self.overlay.as_ref()) else {
            return HashMap::new();
        };
        overlay
            .direct_external_source_counts
            .iter()
            .filter_map(|(target, effective_count)| {
                let published = base.direct_external_source_count(target) > 0;
                let effective = *effective_count > 0;
                (published != effective).then(|| (target.clone(), effective))
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn request_overlay_shape_for_test(
        &self,
        expected_base: &Arc<ReachGraph>,
    ) -> (bool, usize, usize) {
        let shares_base = self
            .base
            .as_ref()
            .is_some_and(|base| Arc::ptr_eq(base, expected_base));
        self.overlay
            .as_ref()
            .map_or((shares_base, 0, 0), |overlay| {
                (shares_base, overlay.sources.len(), overlay.edges.len())
            })
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

    /// Return the workspace-wide first-layer external include targets carried
    /// by this graph generation. A durable `directly_included` bit is derived
    /// from the same shape (`workspace -> absolute external path`); exposing
    /// the request-local projection lets dirty include-edge replacements
    /// invalidate that bit without reading or mutating the published store.
    #[cfg(test)]
    pub(crate) fn directly_included_external_paths(&self) -> HashSet<String> {
        let mut paths = self
            .base
            .as_deref()
            .map_or_else(HashSet::new, ReachGraph::directly_included_external_paths);
        if let Some(overlay) = &self.overlay {
            for (target, count) in &overlay.direct_external_source_counts {
                if *count == 0 {
                    paths.remove(target);
                } else {
                    paths.insert(target.clone());
                }
            }
            return paths;
        }
        self.direct_external_source_counts.keys().cloned().collect()
    }

    /// Workspace-wide projection retained for the legacy completion name-table
    /// overlay. Request-scoped candidate resolution must use
    /// [`ReachGraph::directly_includes_external`] instead.
    pub(crate) fn any_workspace_directly_includes_external(&self, target: &str) -> bool {
        Path::new(target).is_absolute() && self.direct_external_source_count(target) > 0
    }

    /// Test origin-specific first-layer external include evidence. Only a
    /// direct `ExternalExact` edge is strong enough; another workspace source
    /// including the same target must not affect this request.
    pub(crate) fn directly_includes_external(&self, source: &str, target: &str) -> bool {
        Path::new(target).is_absolute()
            && self.edges_for(source).is_some_and(|targets| {
                targets.iter().any(|edge| {
                    edge.target == target && edge.resolution == ResolutionKind::ExternalExact
                })
            })
    }

    /// Strong direct external targets for one request origin. This is the
    /// completion/coloring counterpart to [`Self::directly_includes_external`]
    /// and deliberately does not project evidence from other workspace files.
    pub(crate) fn directly_included_external_paths_from(&self, source: &str) -> HashSet<String> {
        self.edges_for(source)
            .into_iter()
            .flatten()
            .take(MAX_REACH_NODES)
            .filter(|edge| is_direct_external_edge(edge))
            .map(|edge| edge.target.clone())
            .collect()
    }

    fn compute(&self, start: &str) -> ReachScope {
        if let Some(package_key) = self.package_for_file(start) {
            return self.compute_package(start, package_key);
        }
        let mut files = HashSet::new();
        files.insert(start.to_string());
        let mut heuristic_files = HashSet::new();
        let mut open = false;
        let mut reason: Option<OpenReason> = None;

        // The start node's own open status is the first possible cause: a file
        // with an unresolved or ambiguous include opens the scope immediately.
        // First-cause precedence (UnresolvedInclude before AmbiguousInclude)
        // is encoded by the order `ReachGraph::new` writes the `open` map: a
        // node present in both lists is stored under `UnresolvedInclude`.
        if let Some(cause) = self.open_reason(start) {
            open = true;
            reason = Some(cause);
        }

        let mark_open = |open: &mut bool, cause: OpenReason, reason: &mut Option<OpenReason>| {
            *open = true;
            if reason.is_none() {
                *reason = Some(cause);
            }
        };

        let mut queue: VecDeque<(String, usize, bool)> = VecDeque::new();
        queue.push_back((start.to_string(), 0, false));
        while let Some((node, depth, path_is_heuristic)) = queue.pop_front() {
            if depth >= MAX_REACH_DEPTH {
                // Stop descending; we cannot prove what lies deeper.
                mark_open(&mut open, OpenReason::DepthLimit, &mut reason);
                continue;
            }
            let Some(dsts) = self.edges_for(&node) else {
                continue;
            };
            for edge in dsts {
                let next_is_heuristic = path_is_heuristic || !is_strong_resolution(edge.resolution);
                let is_new_node =
                    !files.contains(&edge.target) && !heuristic_files.contains(&edge.target);
                if is_new_node && files.len() + heuristic_files.len() >= MAX_REACH_NODES {
                    mark_open(&mut open, OpenReason::NodeLimit, &mut reason);
                    break;
                }
                let inserted = if next_is_heuristic {
                    !files.contains(&edge.target) && heuristic_files.insert(edge.target.clone())
                } else if files.insert(edge.target.clone()) {
                    heuristic_files.remove(&edge.target);
                    true
                } else {
                    false
                };
                if inserted {
                    if let Some(cause) = self.open_reason(&edge.target) {
                        mark_open(&mut open, cause, &mut reason);
                    }
                    queue.push_back((edge.target.clone(), depth + 1, next_is_heuristic));
                }
            }
        }

        ReachScope {
            files,
            heuristic_files,
            open,
            reason,
        }
    }

    fn compute_package(&self, start: &str, start_package: &str) -> ReachScope {
        let mut files = HashSet::new();
        let mut heuristic_files = HashSet::new();
        let mut open = false;
        let mut reason = self.open_package_reason(start_package);
        open |= reason.is_some();
        let mark_open = |open: &mut bool, cause: OpenReason, reason: &mut Option<OpenReason>| {
            *open = true;
            if reason.is_none() {
                *reason = Some(cause);
            }
        };
        let mut seen_packages = HashSet::new();
        let mut queue = VecDeque::from([(start_package.to_string(), 0usize, false)]);
        seen_packages.insert(start_package.to_string());
        let mut scanned_package_edges = 0usize;

        'packages: while let Some((package, depth, path_is_heuristic)) = queue.pop_front() {
            if let Some(cause) = self.open_package_reason(&package) {
                mark_open(&mut open, cause, &mut reason);
            }
            let remaining = MAX_REACH_NODES.saturating_sub(files.len() + heuristic_files.len());
            let (package_files, package_files_truncated) =
                self.files_for_package_bounded(&package, remaining);
            for path in package_files.iter() {
                if files.len() + heuristic_files.len() >= MAX_REACH_NODES {
                    mark_open(&mut open, OpenReason::NodeLimit, &mut reason);
                    break;
                }
                if path_is_heuristic {
                    if !files.contains(path) {
                        heuristic_files.insert(path.clone());
                    }
                } else {
                    files.insert(path.clone());
                    heuristic_files.remove(path);
                }
            }
            if package_files_truncated {
                mark_open(&mut open, OpenReason::NodeLimit, &mut reason);
                break 'packages;
            }
            if depth >= MAX_REACH_DEPTH {
                mark_open(&mut open, OpenReason::DepthLimit, &mut reason);
                continue;
            }
            let outgoing = self.package_edges_for(&package);
            if files.len() + heuristic_files.len() >= MAX_REACH_NODES
                && outgoing.is_some_and(|edges| !edges.is_empty())
            {
                mark_open(&mut open, OpenReason::NodeLimit, &mut reason);
                break;
            }
            for edge in outgoing.into_iter().flatten() {
                if scanned_package_edges >= MAX_REACH_PACKAGE_EDGES {
                    mark_open(&mut open, OpenReason::NodeLimit, &mut reason);
                    break 'packages;
                }
                scanned_package_edges += 1;
                let next_is_heuristic = path_is_heuristic || edge.heuristic;
                if !seen_packages.contains(&edge.target)
                    && seen_packages.len() >= MAX_REACH_PACKAGES
                {
                    mark_open(&mut open, OpenReason::NodeLimit, &mut reason);
                    break 'packages;
                }
                if seen_packages.insert(edge.target.clone()) {
                    queue.push_back((edge.target.clone(), depth + 1, next_is_heuristic));
                }
            }
        }
        if !files.contains(start) && !heuristic_files.contains(start) {
            files.insert(start.to_string());
        }
        ReachScope {
            files,
            heuristic_files,
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

    fn absolute_test_path(name: &str) -> String {
        std::env::temp_dir()
            .join("fossilsense-reachability")
            .join(name)
            .to_string_lossy()
            .replace('\\', "/")
    }

    #[test]
    fn direct_external_projection_only_counts_workspace_first_layer_edges() {
        let direct = absolute_test_path("direct.h");
        let transitive = absolute_test_path("transitive.h");
        let graph = ReachGraph::new_with_kinds(
            vec![
                (
                    "main.c".into(),
                    direct.clone(),
                    ResolutionKind::ExternalExact,
                ),
                (
                    direct.clone(),
                    transitive.clone(),
                    ResolutionKind::ExternalExact,
                ),
            ],
            vec![],
            vec![],
        );

        assert_eq!(
            graph.directly_included_external_paths(),
            HashSet::from([direct.clone()])
        );
        assert!(graph.directly_includes_external("main.c", &direct));
        assert!(!graph.directly_includes_external("other.c", &direct));
        assert!(!graph.directly_includes_external("main.c", &transitive));
        assert!(!graph.directly_includes_external("main.c", "workspace/header.h"));
        assert_eq!(
            graph.directly_included_external_paths_from("main.c"),
            HashSet::from([direct])
        );
        assert!(graph
            .directly_included_external_paths_from("other.c")
            .is_empty());
    }

    #[test]
    fn request_direct_external_projection_is_bounded_by_the_reach_node_cap() {
        let edges = (0..(MAX_REACH_NODES + 64))
            .map(|index| {
                (
                    "main.c".to_string(),
                    absolute_test_path(&format!("external_{index:05}.h")),
                    ResolutionKind::ExternalExact,
                )
            })
            .collect();
        let graph = ReachGraph::new_with_kinds(edges, vec![], vec![]);

        assert_eq!(
            graph.directly_included_external_paths_from("main.c").len(),
            MAX_REACH_NODES,
            "completion scope preparation must not clone an unbounded direct-external fanout"
        );
        let bounded = graph.directly_included_external_paths_from("main.c");
        assert!(bounded.contains(&absolute_test_path("external_00000.h")));
        assert!(
            !bounded.contains(&absolute_test_path(&format!(
                "external_{:05}.h",
                MAX_REACH_NODES + 63
            ))),
            "the deterministic lexical cap must omit targets beyond the bounded prefix"
        );

        let mut noisy_edges = (0..(MAX_REACH_NODES + 64))
            .map(|index| {
                (
                    "noisy.c".to_string(),
                    format!("workspace/local_{index:05}.h"),
                    ResolutionKind::WorkspaceExact,
                )
            })
            .collect::<Vec<_>>();
        let late_external = absolute_test_path("late_external.h");
        noisy_edges.push((
            "noisy.c".to_string(),
            late_external.clone(),
            ResolutionKind::ExternalExact,
        ));
        let noisy_graph = ReachGraph::new_with_kinds(noisy_edges, vec![], vec![]);
        assert_eq!(
            noisy_graph.directly_included_external_paths_from("noisy.c"),
            HashSet::from([late_external]),
            "bounded traversal must prioritize direct-external edges before unrelated fanout"
        );
    }

    #[test]
    fn go_package_reach_stops_expanding_edges_when_the_file_node_cap_is_full() {
        let package_files = (0..MAX_REACH_NODES)
            .map(|index| GoPackageFileRow {
                package_key: "pkg0#pkg".into(),
                path: format!("pkg/file{index}.go"),
            })
            .collect();
        let package_edges = (0..(MAX_REACH_DEPTH + 8))
            .map(|index| GoPackageEdgeRow {
                source_package_key: format!("pkg{index}#pkg"),
                target_package_key: format!("pkg{}#pkg", index + 1),
                resolution: GoPackageResolution::Exact,
            })
            .collect();
        let graph = ReachGraph::from_rows_with_packages(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            package_files,
            package_edges,
            Vec::new(),
        );

        let scope = graph.reachable("pkg/file0.go");
        assert_eq!(scope.files.len(), MAX_REACH_NODES);
        assert!(scope.open);
        assert_eq!(scope.reason, Some(OpenReason::NodeLimit));
    }

    #[test]
    fn go_package_depth_cap_keeps_the_boundary_package_files() {
        let package_files = (0..=MAX_REACH_DEPTH)
            .map(|index| GoPackageFileRow {
                package_key: format!("pkg{index}#pkg"),
                path: format!("pkg{index}/file.go"),
            })
            .collect();
        let package_edges = (0..MAX_REACH_DEPTH)
            .map(|index| GoPackageEdgeRow {
                source_package_key: format!("pkg{index}#pkg"),
                target_package_key: format!("pkg{}#pkg", index + 1),
                resolution: GoPackageResolution::Exact,
            })
            .collect();
        let graph = ReachGraph::from_rows_with_packages(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            package_files,
            package_edges,
            Vec::new(),
        );

        let scope = graph.reachable("pkg0/file.go");
        assert!(scope
            .files
            .contains(&format!("pkg{MAX_REACH_DEPTH}/file.go")));
        assert!(scope.open);
        assert_eq!(scope.reason, Some(OpenReason::DepthLimit));
    }

    #[test]
    fn suffix_match_and_its_descendants_stay_heuristic() {
        let graph = ReachGraph::new_with_kinds(
            vec![
                ("a.c".into(), "b.h".into(), ResolutionKind::SuffixMatch),
                ("b.h".into(), "c.h".into(), ResolutionKind::RelativeExact),
            ],
            vec![],
            vec![],
        );

        let scope = graph.reachable("a.c");
        assert_eq!(scope.files, set(&["a.c"]));
        assert_eq!(scope.heuristic_files, set(&["b.h", "c.h"]));
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

    #[test]
    fn immutable_refresh_keeps_prior_generation_unchanged() {
        let graph = ReachGraph::new(vec![("a.c".into(), "old.h".into())], vec![], vec![]);

        let next = graph.with_refreshed_sources_from_rows(
            &["a.c".to_string()],
            vec![IncludeEdgeRow {
                source_path: "a.c".to_string(),
                target_path: "new.h".to_string(),
                resolution: ResolutionKind::WorkspaceExact,
            }],
            vec![],
        );

        let prior_scope = graph.reachable("a.c");
        assert!(prior_scope.files.contains("old.h"));
        assert!(!prior_scope.files.contains("new.h"));

        let next_scope = next.reachable("a.c");
        assert!(next_scope.files.contains("new.h"));
        assert!(!next_scope.files.contains("old.h"));
    }

    #[test]
    fn request_overlay_shares_large_base_and_stores_only_refreshed_sources() {
        let old_external = absolute_test_path("old.h");
        let new_external = absolute_test_path("new.h");
        let mut edges = (0..8_192)
            .map(|index| {
                (
                    format!("src/source_{index}.c"),
                    format!("include/header_{index}.h"),
                    ResolutionKind::WorkspaceExact,
                )
            })
            .collect::<Vec<_>>();
        edges.push((
            "main.c".to_string(),
            old_external.clone(),
            ResolutionKind::ExternalExact,
        ));
        let base = Arc::new(ReachGraph::new_with_kinds(edges, Vec::new(), Vec::new()));

        let overlay = ReachGraph::with_request_overrides(
            base.clone(),
            &["main.c".to_string()],
            vec![(
                "main.c".to_string(),
                new_external.clone(),
                ResolutionKind::ExternalExact,
            )],
            Vec::new(),
            Vec::new(),
        );

        assert!(Arc::ptr_eq(
            overlay.base.as_ref().expect("persistent base"),
            &base
        ));
        let overrides = overlay.overlay.as_ref().expect("request overrides");
        assert_eq!(overrides.sources.len(), 1);
        assert_eq!(overrides.edges.len(), 1);
        assert_eq!(overlay.direct_external_source_count(&old_external), 0);
        assert_eq!(overlay.direct_external_source_count(&new_external), 1);

        let effective = overlay.reachable("main.c");
        assert!(effective.files.contains(&new_external));
        assert!(!effective.files.contains(&old_external));
        let published = base.reachable("main.c");
        assert!(published.files.contains(&old_external));
        assert!(!published.files.contains(&new_external));
    }

    #[test]
    fn unrelated_c_request_overlay_borrows_large_go_package_membership() {
        let package_files = (0..(MAX_REACH_NODES + 1_000))
            .map(|index| GoPackageFileRow {
                package_key: "large#pkg".into(),
                path: format!("large/file_{index}.go"),
            })
            .collect();
        let base = Arc::new(ReachGraph::from_rows_with_packages(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            package_files,
            Vec::new(),
            Vec::new(),
        ));
        let overlay = ReachGraph::with_request_overrides(
            base,
            &["unrelated.c".into()],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );

        assert!(matches!(
            overlay.files_for_package("large#pkg"),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn request_overlay_matches_materialized_source_refresh_oracle() {
        let base = Arc::new(ReachGraph::new_with_kinds(
            vec![
                (
                    "main.c".into(),
                    "old.h".into(),
                    ResolutionKind::WorkspaceExact,
                ),
                (
                    "old.h".into(),
                    "transitive.h".into(),
                    ResolutionKind::WorkspaceExact,
                ),
                (
                    "other.c".into(),
                    "stable.h".into(),
                    ResolutionKind::WorkspaceExact,
                ),
            ],
            vec!["main.c".into()],
            Vec::new(),
        ));
        let sources = vec!["main.c".to_string(), "old.h".to_string()];
        let rows = vec![IncludeEdgeRow {
            source_path: "main.c".into(),
            target_path: "new.h".into(),
            resolution: ResolutionKind::WorkspaceExact,
        }];
        let expected = base.with_refreshed_sources_from_rows(
            &sources,
            rows.clone(),
            vec![OpenIncludeRow {
                source_path: "main.c".into(),
                reason: OpenReason::AmbiguousInclude,
            }],
        );
        let layered = ReachGraph::with_request_overrides(
            base,
            &sources,
            rows.into_iter()
                .map(|row| (row.source_path, row.target_path, row.resolution))
                .collect(),
            vec![("main.c".into(), OpenReason::AmbiguousInclude)],
            Vec::new(),
        );

        for source in ["main.c", "old.h", "other.c"] {
            assert_eq!(layered.reachable(source), expected.reachable(source));
        }
    }

    #[test]
    fn request_overlay_direct_external_count_keeps_other_source_evidence() {
        let external = absolute_test_path("shared.h");
        let base = Arc::new(ReachGraph::new_with_kinds(
            vec![
                (
                    "first.c".into(),
                    external.clone(),
                    ResolutionKind::ExternalExact,
                ),
                (
                    "second.c".into(),
                    external.clone(),
                    ResolutionKind::ExternalExact,
                ),
            ],
            Vec::new(),
            Vec::new(),
        ));
        let layered = ReachGraph::with_request_overrides(
            base,
            &["first.c".into()],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );

        assert_eq!(layered.direct_external_source_count(&external), 1);
        assert!(!layered
            .direct_external_presence_overrides()
            .contains_key(&external));
        assert!(!layered.directly_includes_external("first.c", &external));
        assert!(layered.directly_includes_external("second.c", &external));
    }
}
