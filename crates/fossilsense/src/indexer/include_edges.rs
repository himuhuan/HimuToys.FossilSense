use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;

use crate::includes::{resolve_include, IncludeResolution, ResolutionKind};
use crate::pathing::normalize_abs_path;
use crate::store::{IncludeGraphUpdate, IndexBuild, IndexStore};

/// (Re)build the resolved file-to-file `#include` edges, the per-file
/// `unresolved_includes` / `ambiguous_includes` counts, and the inferred
/// `directly_included` first-layer flag — all from a single form-aware,
/// priority-ordered resolution pass. With `only = None` every file's edges are
/// rebuilt (full pass); with `only = Some(paths)` just those source files are
/// rebuilt (incremental pass) — deleted files' edges cascade away on their own.
///
/// For each `(src_file, include_target)` the indexer calls
/// [`crate::includes::resolve_include`] passing the **source file's directory**
/// and the form from [`crate::includes::normalize_include_target`]:
/// - an `Edge { dst, kind }` produces one row `(src_id, dst_id, kind)` — only
///   `Edge` outcomes become proven-reachable edges;
/// - an `Unresolved` outcome bumps the source's `unresolved_includes` count;
/// - an `Ambiguous { dsts }` outcome bumps the source's `ambiguous_includes`
///   count and records every possible target as a weak `SuffixMatch` edge.
///   Typed reachability keeps those paths heuristic: navigation can show every
///   bounded possibility, and coloring may use them only for kind evidence
///   without promoting a wrong twin to strong reachability.
///
/// After edges are written, `directly_included` is derived globally from the
/// full edge table (an external header is first-layer iff some workspace file
/// has an `ExternalExact` edge to it) — the loose form-blind second matcher is
/// deleted. Edges are derived for *all* indexed files (workspace and external)
/// so the closure can follow external→external includes (e.g. `ext.h`→`deep.h`):
/// an external includer's "own directory" is one of the configured roots, so a
/// quote include of a sibling external header resolves `RelativeExact` against
/// it, consistent with form/priority by construction.
pub(super) fn build_include_edges(
    store: &IndexStore,
    build: IndexBuild,
    roots: &[PathBuf],
    only: Option<&[String]>,
) -> Result<IncludeGraphUpdate> {
    let files = store.effective_files_with_ids(build)?;
    let id_of_path: HashMap<String, i64> = files
        .iter()
        .map(|(id, path, _)| (path.clone(), *id))
        .collect();
    let path_of_id: HashMap<i64, String> = files
        .iter()
        .map(|(id, path, _)| (*id, path.clone()))
        .collect();
    let all_paths: HashSet<String> = files.iter().map(|(_, path, _)| path.clone()).collect();
    let mut by_basename: HashMap<String, Vec<String>> = HashMap::new();
    for (_, path, source) in &files {
        if source == "workspace" {
            let last = path.rsplit('/').next().unwrap_or(path).to_string();
            by_basename.entry(last).or_default().push(path.clone());
        }
    }
    let roots_slash: Vec<String> = roots.iter().map(|root| normalize_abs_path(root)).collect();

    // Source file ids in scope: the listed paths (incremental) or all files.
    let src_ids: Option<Vec<i64>> = only.map(|paths| {
        paths
            .iter()
            .filter_map(|path| id_of_path.get(path).copied())
            .collect()
    });

    // Seed every in-scope source with an empty target list so files that lost
    // all includes still get their edges cleared and counts reset.
    let mut by_src: HashMap<i64, Vec<String>> = HashMap::new();
    match &src_ids {
        Some(ids) => {
            for id in ids {
                by_src.entry(*id).or_default();
            }
        }
        None => {
            for (id, _, _) in &files {
                by_src.entry(*id).or_default();
            }
        }
    }
    for (file_id, target) in store.effective_includes_with_file_ids(build, src_ids.as_deref())? {
        by_src.entry(file_id).or_default().push(target);
    }

    // A source can mention the same physical target through both an
    // ambiguous suffix and a later exact include.  Collapse by endpoint while
    // preserving the strongest resolution so SQLite's `(src,dst)` key cannot
    // let an earlier weak row hide later exact evidence.
    let mut edges_by_endpoint: HashMap<(i64, i64), ResolutionKind> = HashMap::new();
    let mut unresolved: Vec<(i64, i64)> = Vec::new();
    let mut ambiguous: Vec<(i64, i64)> = Vec::new();
    for (src_id, targets) in &by_src {
        let src_dir = path_of_id
            .get(src_id)
            .and_then(|p| p.rsplit_once('/'))
            .map(|(dir, _)| dir.to_string())
            .unwrap_or_default();
        let mut unresolved_count = 0i64;
        let mut ambiguous_count = 0i64;
        for target in targets {
            let resolution =
                resolve_include(target, &src_dir, &roots_slash, &all_paths, &by_basename);
            match resolution {
                IncludeResolution::Edge { dst, kind } => {
                    if let Some(&dst_id) = id_of_path.get(&dst) {
                        if dst_id != *src_id {
                            retain_strongest_edge(&mut edges_by_endpoint, *src_id, dst_id, kind);
                        }
                    }
                }
                IncludeResolution::Ambiguous { dsts } => {
                    ambiguous_count += 1;
                    for dst in dsts {
                        if let Some(&dst_id) = id_of_path.get(&dst) {
                            if dst_id != *src_id {
                                retain_strongest_edge(
                                    &mut edges_by_endpoint,
                                    *src_id,
                                    dst_id,
                                    ResolutionKind::SuffixMatch,
                                );
                            }
                        }
                    }
                }
                IncludeResolution::Unresolved => {
                    unresolved_count += 1;
                }
            }
        }
        // Only emit a count row when it is non-zero; the store resets counts for
        // the listed src_ids before re-applying, so a missing row is equivalent
        // to a zero row.
        if unresolved_count > 0 {
            unresolved.push((*src_id, unresolved_count));
        }
        if ambiguous_count > 0 {
            ambiguous.push((*src_id, ambiguous_count));
        }
    }

    let mut edges: Vec<_> = edges_by_endpoint
        .into_iter()
        .map(|((src_id, dst_id), resolution)| (src_id, dst_id, resolution.as_str().to_string()))
        .collect();
    edges.sort();

    let src_id_list: Vec<i64> = by_src.keys().copied().collect();
    // Full pass (only = None) wipes edges AND both counts first; incremental
    // pass only resets the listed src_ids. `directly_included` is derived
    // globally afterward from the new edge table.
    Ok(IncludeGraphUpdate {
        source_ids: src_id_list,
        edges,
        unresolved,
        ambiguous,
        clear_all: only.is_none(),
    })
}

fn retain_strongest_edge(
    edges: &mut HashMap<(i64, i64), ResolutionKind>,
    source: i64,
    target: i64,
    candidate: ResolutionKind,
) {
    edges
        .entry((source, target))
        .and_modify(|current| {
            if *current == ResolutionKind::SuffixMatch && candidate != ResolutionKind::SuffixMatch {
                *current = candidate;
            }
        })
        .or_insert(candidate);
}

/// Find source files whose include edges need rebuild because `changed_paths`
/// contains a path to which one of their raw include targets may resolve. Uses
/// persisted normalized include metadata (SQL-driven) plus the direct changed
/// sources as a conservative union. The old row-scanning path is deleted.
pub(super) fn sql_affected_include_edge_sources(
    store: &IndexStore,
    roots_slash: &[String],
    direct_sources: &[String],
    changed_paths: &[String],
) -> Result<Vec<String>> {
    let mut affected: HashSet<String> = direct_sources.iter().cloned().collect();

    if !changed_paths.is_empty() {
        // Collect changed workspace-relative and absolute paths.
        let mut rel_paths: Vec<String> = Vec::new();
        let mut abs_paths: HashSet<String> = HashSet::new();
        for path in changed_paths {
            if path.contains(':') || path.starts_with('/') {
                abs_paths.insert(path.clone());
            } else {
                rel_paths.push(path.clone());
            }
        }
        let sql_affected = store.affected_include_sources(&rel_paths, &abs_paths, roots_slash)?;
        affected.extend(sql_affected);
    }

    let mut out: Vec<String> = affected.into_iter().collect();
    out.sort();
    Ok(out)
}
