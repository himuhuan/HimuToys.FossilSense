use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::Result;
use tower_lsp::lsp_types::{CompletionItem, CompletionItemKind, Location, Position, Range, Url};

use crate::config::WorkspaceConfig;
use crate::includes::{self, IncludeForm};
use crate::pathing;
use crate::store::IndexStore;

pub(super) type ExternalIncludeDirCache = Arc<StdMutex<HashMap<String, CachedDirListing>>>;

#[derive(Debug, Clone, Default)]
pub(super) struct IncludeCompletionTable {
    workspace_paths: Vec<String>,
    basename_counts: HashMap<String, usize>,
    incoming_by_src_dir: HashMap<String, HashSet<String>>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct IncludeCompletionMetrics {
    pub recent: usize,
    pub sibling: usize,
    pub basename: usize,
    pub depth_penalty: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct IncludeRankingSignals {
    recent: bool,
    sibling: bool,
    basename: bool,
    depth_penalty: bool,
}

impl IncludeCompletionTable {
    #[allow(dead_code)]
    pub(super) fn build(workspace_paths: Vec<String>) -> Self {
        Self::build_with_edges(workspace_paths, Vec::new())
    }

    pub(super) fn build_with_edges(
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

    pub(super) fn len(&self) -> usize {
        self.workspace_paths.len()
    }

    #[cfg(test)]
    pub(super) fn edge_count(&self) -> usize {
        self.incoming_by_src_dir.values().map(HashSet::len).sum()
    }

    fn collect_candidates(
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
        (boost, signals)
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct CurrentIncludeEvidence {
    source_dir: Option<String>,
    recent_targets: HashSet<String>,
    recent_basenames: HashSet<String>,
}

impl CurrentIncludeEvidence {
    pub(super) fn from_text(text: &str, current_rel_path: Option<&str>) -> Self {
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

#[derive(Debug, Clone)]
pub(super) struct CachedDirListing {
    mtime_ns: u64,
    entries: Vec<(String, bool)>,
}

pub(super) fn configured_include_paths(
    workspace_root: Option<&Path>,
    client_paths: &[String],
) -> Vec<String> {
    let mut paths = workspace_root
        .map(|root| WorkspaceConfig::load(root).0.include_paths)
        .unwrap_or_default();
    paths.extend(client_paths.iter().cloned());

    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for mut path in paths {
        path = path.trim().replace('\\', "/");
        while path.len() > 1 && path.ends_with('/') {
            path.pop();
        }
        if path.is_empty() {
            continue;
        }
        if seen.insert(path.to_ascii_lowercase()) {
            out.push(path);
        }
    }
    out
}

/// Resolve an include target to existing header file(s), ranked by the delimiter
/// form's search order (quote: local dir -> workspace -> include paths; angle:
/// include paths -> workspace -> local dir), de-duplicated, workspace-relative
/// candidates resolved against `workspace_root`. Existence is checked on disk so
/// path-resolution-only (capped) roots still resolve.
pub(super) fn resolve_include_paths(
    form: IncludeForm,
    rel: &str,
    current_dir: Option<&Path>,
    workspace_root: Option<&Path>,
    include_roots: &[String],
    db_path: Option<&Path>,
) -> Result<Vec<PathBuf>> {
    let dir_candidate: Vec<PathBuf> = current_dir.map(|dir| dir.join(rel)).into_iter().collect();
    let root_candidates: Vec<PathBuf> = include_roots
        .iter()
        .map(|root| PathBuf::from(root).join(rel))
        .collect();
    let ws_candidates: Vec<PathBuf> = match (workspace_root, db_path) {
        (Some(ws), Some(db)) if db.exists() => {
            let store = IndexStore::open_readonly(db)?;
            store
                .workspace_files_by_suffix(rel)?
                .into_iter()
                .map(|rel| ws.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR)))
                .collect()
        }
        _ => Vec::new(),
    };

    let ordered: Vec<PathBuf> = match form {
        IncludeForm::Quote => [dir_candidate, ws_candidates, root_candidates].concat(),
        IncludeForm::Angle => [root_candidates, ws_candidates, dir_candidate].concat(),
    };

    let mut out: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for candidate in ordered {
        if !candidate.is_file() {
            continue;
        }
        let key = pathing::normalize_abs_path(&candidate).to_ascii_lowercase();
        if seen.insert(key) {
            out.push(candidate);
        }
    }
    Ok(out)
}

/// A `Location` pointing at the very start of `path`.
pub(super) fn location_at_file_start(path: &Path) -> Option<Location> {
    let uri = Url::from_file_path(path).ok()?;
    Some(Location {
        uri,
        range: Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: Position {
                line: 0,
                character: 0,
            },
        },
    })
}

/// Whether a filename looks like an includable header: a known header extension,
/// or extensionless (C++ standard library headers such as `<vector>`).
pub(super) fn looks_like_header(name: &str) -> bool {
    match name.rsplit_once('.') {
        Some((_, ext)) => matches!(
            ext.to_ascii_lowercase().as_str(),
            "h" | "hpp" | "hh" | "hxx" | "inl" | "inc" | "ipp" | "tcc" | "def"
        ),
        None => true,
    }
}

/// List header files and sub-directories matching the typed include partial,
/// across the form-ordered base directories. Disk-based so it works regardless
/// of index freshness and for path-resolution-only roots.
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(super) fn collect_include_candidates(
    form: IncludeForm,
    dir_part: &str,
    seg: &str,
    current_dir: Option<&Path>,
    workspace_root: Option<&Path>,
    include_roots: &[String],
    db_path: Option<&Path>,
    limit: usize,
) -> Vec<CompletionItem> {
    collect_include_candidates_with_table(
        form,
        dir_part,
        seg,
        current_dir,
        workspace_root,
        include_roots,
        db_path,
        None,
        None,
        limit,
    )
}

#[allow(clippy::too_many_arguments)]
#[allow(dead_code)]
pub(super) fn collect_include_candidates_with_table(
    form: IncludeForm,
    dir_part: &str,
    seg: &str,
    current_dir: Option<&Path>,
    workspace_root: Option<&Path>,
    include_roots: &[String],
    db_path: Option<&Path>,
    include_table: Option<&IncludeCompletionTable>,
    external_cache: Option<&ExternalIncludeDirCache>,
    limit: usize,
) -> Vec<CompletionItem> {
    collect_include_candidates_with_table_and_evidence(
        form,
        dir_part,
        seg,
        current_dir,
        workspace_root,
        include_roots,
        db_path,
        include_table,
        external_cache,
        None,
        None,
        limit,
    )
    .0
}

#[allow(clippy::too_many_arguments)]
pub(super) fn collect_include_candidates_with_table_and_evidence(
    form: IncludeForm,
    dir_part: &str,
    seg: &str,
    current_dir: Option<&Path>,
    workspace_root: Option<&Path>,
    include_roots: &[String],
    db_path: Option<&Path>,
    include_table: Option<&IncludeCompletionTable>,
    external_cache: Option<&ExternalIncludeDirCache>,
    current_rel_dir: Option<&str>,
    evidence: Option<&CurrentIncludeEvidence>,
    limit: usize,
) -> (Vec<CompletionItem>, IncludeCompletionMetrics) {
    let cur = current_dir.map(|p| p.to_path_buf());
    let ws = workspace_root.map(|p| p.to_path_buf());
    let roots: Vec<PathBuf> = include_roots.iter().map(PathBuf::from).collect();

    let dir_native = dir_part.replace('/', std::path::MAIN_SEPARATOR_STR);
    let seg_lower = seg.to_ascii_lowercase();
    let mut seen: HashSet<String> = HashSet::new();
    let mut scored: Vec<(i32, String, CompletionItem)> = Vec::new();
    let mut metrics = IncludeCompletionMetrics::default();

    match form {
        IncludeForm::Quote => {
            if let Some(cur) = cur {
                collect_disk_include_candidates(
                    &cur,
                    &dir_native,
                    &seg_lower,
                    seg,
                    300,
                    &mut seen,
                    &mut scored,
                );
            }
            if let Some(ws) = &ws {
                collect_disk_include_candidates(
                    ws,
                    &dir_native,
                    &seg_lower,
                    seg,
                    250,
                    &mut seen,
                    &mut scored,
                );
            }
            collect_workspace_include_candidates(
                db_path,
                include_table,
                dir_part,
                &seg_lower,
                seg,
                250,
                current_rel_dir,
                evidence,
                &mut metrics,
                &mut seen,
                &mut scored,
            );
            for root in roots {
                collect_cached_disk_include_candidates(
                    &root,
                    &dir_native,
                    &seg_lower,
                    seg,
                    200,
                    external_cache,
                    &mut seen,
                    &mut scored,
                );
            }
        }
        IncludeForm::Angle => {
            for root in roots {
                collect_cached_disk_include_candidates(
                    &root,
                    &dir_native,
                    &seg_lower,
                    seg,
                    300,
                    external_cache,
                    &mut seen,
                    &mut scored,
                );
            }
            if let Some(ws) = &ws {
                collect_disk_include_candidates(
                    ws,
                    &dir_native,
                    &seg_lower,
                    seg,
                    250,
                    &mut seen,
                    &mut scored,
                );
            }
            collect_workspace_include_candidates(
                db_path,
                include_table,
                dir_part,
                &seg_lower,
                seg,
                250,
                current_rel_dir,
                evidence,
                &mut metrics,
                &mut seen,
                &mut scored,
            );
            if let Some(cur) = cur {
                collect_disk_include_candidates(
                    &cur,
                    &dir_native,
                    &seg_lower,
                    seg,
                    200,
                    &mut seen,
                    &mut scored,
                );
            }
        }
    }

    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let items = scored
        .into_iter()
        .take(limit)
        .map(|(_, _, it)| it)
        .collect();
    (items, metrics)
}

fn collect_disk_include_candidates(
    base: &Path,
    dir_native: &str,
    seg_lower: &str,
    seg: &str,
    base_score: i32,
    seen: &mut HashSet<String>,
    scored: &mut Vec<(i32, String, CompletionItem)>,
) {
    let dir = base.join(dir_native);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.to_ascii_lowercase().starts_with(seg_lower) {
            continue;
        }
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if !is_dir && !looks_like_header(&name) {
            continue;
        }
        push_include_candidate(name, is_dir, base_score, seg, seen, scored);
    }
}

fn collect_cached_disk_include_candidates(
    base: &Path,
    dir_native: &str,
    seg_lower: &str,
    seg: &str,
    base_score: i32,
    cache: Option<&ExternalIncludeDirCache>,
    seen: &mut HashSet<String>,
    scored: &mut Vec<(i32, String, CompletionItem)>,
) {
    let Some(cache) = cache else {
        collect_disk_include_candidates(base, dir_native, seg_lower, seg, base_score, seen, scored);
        return;
    };

    let dir = base.join(dir_native);
    let Some(entries) = cached_dir_entries(&dir, cache) else {
        return;
    };
    for (name, is_dir) in entries {
        if !name.to_ascii_lowercase().starts_with(seg_lower) {
            continue;
        }
        if !is_dir && !looks_like_header(&name) {
            continue;
        }
        push_include_candidate(name, is_dir, base_score, seg, seen, scored);
    }
}

fn cached_dir_entries(dir: &Path, cache: &ExternalIncludeDirCache) -> Option<Vec<(String, bool)>> {
    let meta = std::fs::metadata(dir).ok()?;
    if !meta.is_dir() {
        return None;
    }
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let key = pathing::normalize_abs_path(dir);

    if let Ok(cache_guard) = cache.lock() {
        if let Some(cached) = cache_guard.get(&key) {
            if cached.mtime_ns == mtime_ns {
                return Some(cached.entries.clone());
            }
        }
    }

    let entries: Vec<(String, bool)> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            (name, is_dir)
        })
        .collect();

    if let Ok(mut cache_guard) = cache.lock() {
        cache_guard.insert(
            key,
            CachedDirListing {
                mtime_ns,
                entries: entries.clone(),
            },
        );
    }
    Some(entries)
}

fn collect_workspace_include_candidates(
    db_path: Option<&Path>,
    include_table: Option<&IncludeCompletionTable>,
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
    if let Some(table) = include_table {
        table.collect_candidates(
            dir_part,
            seg_lower,
            seg,
            base_score,
            current_rel_dir,
            evidence,
            metrics,
            seen,
            scored,
        );
        return;
    }

    let Some(db_path) = db_path.filter(|path| path.exists()) else {
        return;
    };
    let Ok(store) = IndexStore::open_readonly(db_path) else {
        return;
    };
    let Ok(paths) = store.workspace_file_paths() else {
        return;
    };
    for path in paths {
        for candidate in indexed_workspace_include_candidates(&path, dir_part, seg_lower) {
            push_include_candidate(
                candidate.name,
                candidate.is_dir,
                base_score,
                seg,
                seen,
                scored,
            );
        }
    }
}

#[derive(Debug, Clone)]
struct IndexedIncludeCandidate {
    name: String,
    is_dir: bool,
    rel_path: String,
}

fn indexed_workspace_include_candidates(
    rel_path: &str,
    dir_part: &str,
    seg_lower: &str,
) -> Vec<IndexedIncludeCandidate> {
    let rel = rel_path.replace('\\', "/");
    let mut out = Vec::new();

    if dir_part.is_empty() {
        if let Some((first, _)) = rel.split_once('/') {
            if !first.is_empty() && first.to_ascii_lowercase().starts_with(seg_lower) {
                out.push(IndexedIncludeCandidate {
                    name: first.to_string(),
                    is_dir: true,
                    rel_path: first.to_string(),
                });
            }
        }
        if let Some(name) = rel.rsplit('/').next() {
            if !name.is_empty()
                && name.to_ascii_lowercase().starts_with(seg_lower)
                && looks_like_header(name)
            {
                out.push(IndexedIncludeCandidate {
                    name: name.to_string(),
                    is_dir: false,
                    rel_path: rel.clone(),
                });
            }
        }
        return out;
    }

    let remainder = if let Some(rest) = rel.strip_prefix(dir_part) {
        rest
    } else if let Some(pos) = rel.find(&format!("/{dir_part}")) {
        &rel[(pos + dir_part.len() + 1)..]
    } else {
        return out;
    };
    let (name, is_dir) = match remainder.split_once('/') {
        Some((first, _)) => (first, true),
        None => (remainder, false),
    };
    if !name.is_empty()
        && name.to_ascii_lowercase().starts_with(seg_lower)
        && (is_dir || looks_like_header(name))
    {
        let candidate_path = if is_dir {
            format!(
                "{}/{}",
                dir_part.trim_end_matches('/'),
                name.trim_matches('/')
            )
        } else {
            rel.clone()
        };
        out.push(IndexedIncludeCandidate {
            name: name.to_string(),
            is_dir,
            rel_path: candidate_path,
        });
    }
    out
}

fn parent_slash(path: &str) -> Option<String> {
    path.rsplit_once('/')
        .map(|(parent, _)| parent.to_string())
        .filter(|parent| !parent.is_empty())
}

#[cfg(test)]
fn collect_include_candidates_ranked_for_test(
    form: IncludeForm,
    dir_part: &str,
    seg: &str,
    current_rel_dir: Option<&str>,
    include_table: Option<&IncludeCompletionTable>,
    evidence: Option<&CurrentIncludeEvidence>,
    limit: usize,
) -> Vec<CompletionItem> {
    let seg_lower = seg.to_ascii_lowercase();
    let mut seen = HashSet::new();
    let mut scored = Vec::new();
    let mut metrics = IncludeCompletionMetrics::default();
    let base_score = match form {
        IncludeForm::Quote => 250,
        IncludeForm::Angle => 250,
    };
    if let Some(table) = include_table {
        table.collect_candidates(
            dir_part,
            &seg_lower,
            seg,
            base_score,
            current_rel_dir,
            evidence,
            &mut metrics,
            &mut seen,
            &mut scored,
        );
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, _, it)| it)
        .collect()
}

fn push_include_candidate(
    name: String,
    is_dir: bool,
    base_score: i32,
    seg: &str,
    seen: &mut HashSet<String>,
    scored: &mut Vec<(i32, String, CompletionItem)>,
) {
    if !seen.insert(name.to_ascii_lowercase()) {
        return;
    }
    let kind = if is_dir {
        CompletionItemKind::FOLDER
    } else {
        CompletionItemKind::FILE
    };
    let mut score = base_score;
    if name.eq_ignore_ascii_case(seg) {
        score += 100;
    } else if name
        .to_ascii_lowercase()
        .starts_with(&seg.to_ascii_lowercase())
    {
        score += 50;
    }
    scored.push((
        score,
        name.clone(),
        CompletionItem {
            label: name,
            kind: Some(kind),
            sort_text: Some(format!("{:06}", 10000 - score)),
            ..Default::default()
        },
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::{self, IndexOptions};
    use std::collections::HashMap;
    use std::fs;
    use std::sync::{Arc, Mutex as StdMutex};
    use tempfile::tempdir;
    use tower_lsp::lsp_types::CompletionItemKind;

    #[test]
    fn looks_like_header_accepts_headers_and_extensionless() {
        assert!(looks_like_header("stdio.h"));
        assert!(looks_like_header("vector")); // C++ stdlib, extensionless
        assert!(!looks_like_header("main.c"));
        assert!(!looks_like_header("readme.txt"));
    }

    #[test]
    fn resolve_include_quote_prefers_local_then_root() {
        let cur = tempdir().expect("cur");
        let root = tempdir().expect("root");
        fs::write(cur.path().join("config.h"), "x").expect("local");
        fs::write(root.path().join("config.h"), "x").expect("root copy");
        let root_str = root.path().to_string_lossy().replace('\\', "/");

        let resolved = resolve_include_paths(
            IncludeForm::Quote,
            "config.h",
            Some(cur.path()),
            None,
            &[root_str],
            None,
        )
        .expect("resolve");

        // Both exist; the local directory ranks first for a quoted include.
        assert_eq!(resolved.len(), 2);
        assert!(resolved[0].starts_with(cur.path()));
    }

    #[test]
    fn resolve_include_angle_prefers_include_root() {
        let cur = tempdir().expect("cur");
        let root = tempdir().expect("root");
        fs::write(root.path().join("stdio.h"), "x").expect("root header");
        let root_str = root.path().to_string_lossy().replace('\\', "/");

        let resolved = resolve_include_paths(
            IncludeForm::Angle,
            "stdio.h",
            Some(cur.path()),
            None,
            &[root_str],
            None,
        )
        .expect("resolve");

        assert_eq!(resolved.len(), 1);
        assert!(resolved[0].starts_with(root.path()));
    }

    #[test]
    fn resolve_include_unresolved_is_empty() {
        let resolved =
            resolve_include_paths(IncludeForm::Angle, "nope/missing.h", None, None, &[], None)
                .expect("resolve");
        assert!(resolved.is_empty());
    }

    #[test]
    fn include_candidates_are_headers_and_subdirs_only() {
        let root = tempdir().expect("root");
        fs::write(root.path().join("stdio.h"), "x").expect("stdio");
        fs::write(root.path().join("stdlib.h"), "x").expect("stdlib");
        fs::write(root.path().join("notes.txt"), "x").expect("txt");
        fs::create_dir_all(root.path().join("sys")).expect("sys");
        let root_str = root.path().to_string_lossy().replace('\\', "/");

        let items = collect_include_candidates(
            IncludeForm::Angle,
            "",
            "std",
            None,
            None,
            std::slice::from_ref(&root_str),
            None,
            100,
        );
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"stdio.h"));
        assert!(labels.contains(&"stdlib.h"));
        assert!(!labels.contains(&"notes.txt"), "non-header file excluded");

        // Sub-path completion lists entries under the typed directory.
        fs::write(root.path().join("sys/types.h"), "x").expect("types");
        let sub = collect_include_candidates(
            IncludeForm::Angle,
            "sys/",
            "",
            None,
            None,
            &[root_str],
            None,
            100,
        );
        assert!(sub.iter().any(|i| i.label == "types.h"));
        // The `sys` directory itself surfaces as a folder candidate at the root.
        let top = collect_include_candidates(
            IncludeForm::Angle,
            "",
            "sys",
            None,
            None,
            &[root.path().to_string_lossy().replace('\\', "/")],
            None,
            100,
        );
        assert!(top
            .iter()
            .any(|i| i.label == "sys" && i.kind == Some(CompletionItemKind::FOLDER)));
    }

    #[test]
    fn configured_include_paths_merge_fossilsense_json_and_client() {
        let ws = tempdir().expect("ws");
        let from_json = tempdir().expect("json inc");
        let from_client = tempdir().expect("client inc");
        let json_path = from_json.path().to_string_lossy().replace('\\', "/");
        let client_path = from_client.path().to_string_lossy().replace('\\', "/");
        fs::write(
            ws.path().join("fossilsense.json"),
            format!(r#"{{"includePaths": ["{}"]}}"#, json_path),
        )
        .expect("config");

        let paths = configured_include_paths(Some(ws.path()), std::slice::from_ref(&client_path));
        assert!(paths.contains(&json_path));
        assert!(paths.contains(&client_path));
    }

    #[test]
    fn include_candidates_use_indexed_workspace_headers_below_subdirs() {
        let ws = tempdir().expect("ws");
        fs::create_dir_all(ws.path().join("include/sys")).expect("include");
        fs::write(ws.path().join("include/foo.h"), "typedef int foo_t;\n").expect("foo");
        fs::write(
            ws.path().join("include/sys/types.h"),
            "typedef int type_t;\n",
        )
        .expect("types");
        let db = ws.path().join("index.sqlite");
        indexer::index_workspace(
            ws.path(),
            IndexOptions {
                db_path: Some(db.clone()),
                force: true,
                ..Default::default()
            },
            |_| {},
        )
        .expect("index");

        let items = collect_include_candidates(
            IncludeForm::Angle,
            "",
            "fo",
            None,
            Some(ws.path()),
            &[],
            Some(db.as_path()),
            100,
        );
        assert!(items
            .iter()
            .any(|i| i.label == "foo.h" && i.kind == Some(CompletionItemKind::FILE)));

        let sub = collect_include_candidates(
            IncludeForm::Angle,
            "sys/",
            "ty",
            None,
            Some(ws.path()),
            &[],
            Some(db.as_path()),
            100,
        );
        assert!(sub
            .iter()
            .any(|i| i.label == "types.h" && i.kind == Some(CompletionItemKind::FILE)));
    }

    #[test]
    fn include_completion_table_matches_indexed_workspace_candidates() {
        let table = IncludeCompletionTable::build(vec![
            "include/api.h".to_string(),
            "include/detail/deep.h".to_string(),
            "src/main.c".to_string(),
            "vendor/api.h".to_string(),
        ]);

        let top = collect_include_candidates_with_table(
            IncludeForm::Quote,
            "",
            "in",
            None,
            None,
            &[],
            None,
            Some(&table),
            None,
            20,
        );
        assert!(top
            .iter()
            .any(|i| i.label == "include" && i.kind == Some(CompletionItemKind::FOLDER)));

        let nested = collect_include_candidates_with_table(
            IncludeForm::Quote,
            "include/",
            "de",
            None,
            None,
            &[],
            None,
            Some(&table),
            None,
            20,
        );
        assert!(nested
            .iter()
            .any(|i| i.label == "detail" && i.kind == Some(CompletionItemKind::FOLDER)));

        let deep = collect_include_candidates_with_table(
            IncludeForm::Quote,
            "include/detail/",
            "de",
            None,
            None,
            &[],
            None,
            Some(&table),
            None,
            20,
        );
        assert!(deep
            .iter()
            .any(|i| i.label == "deep.h" && i.kind == Some(CompletionItemKind::FILE)));
    }

    #[test]
    fn quote_include_prefers_same_directory_and_sibling_patterns() {
        let table = IncludeCompletionTable::build_with_edges(
            vec![
                "src/driver/main.c".to_string(),
                "src/driver/main.h".to_string(),
                "src/driver/config.h".to_string(),
                "vendor/config.h".to_string(),
            ],
            vec![(
                "src/driver/main.c".to_string(),
                "src/driver/config.h".to_string(),
            )],
        );
        let evidence =
            CurrentIncludeEvidence::from_text("#include \"config.h\"\n", Some("src/driver/main.c"));

        let items = collect_include_candidates_ranked_for_test(
            IncludeForm::Quote,
            "",
            "con",
            Some("src/driver"),
            Some(&table),
            Some(&evidence),
            20,
        );

        assert_eq!(items[0].label, "config.h");
    }

    #[test]
    fn basename_frequency_breaks_workspace_ties_without_overriding_form_priority() {
        let table = IncludeCompletionTable::build_with_edges(
            vec![
                "src/a/common.h".to_string(),
                "src/b/common.h".to_string(),
                "src/c/common.h".to_string(),
                "src/driver/config.h".to_string(),
            ],
            Vec::new(),
        );

        let items = collect_include_candidates_ranked_for_test(
            IncludeForm::Quote,
            "",
            "c",
            None,
            Some(&table),
            None,
            20,
        );

        assert_eq!(items[0].label, "common.h");
        assert!(items.iter().any(|item| item.label == "config.h"));
    }

    #[test]
    fn path_depth_penalty_prefers_shallow_comparable_headers() {
        let table = IncludeCompletionTable::build_with_edges(
            vec![
                "include/api.h".to_string(),
                "include/detail/internal/api.h".to_string(),
            ],
            Vec::new(),
        );

        let items = collect_include_candidates_ranked_for_test(
            IncludeForm::Quote,
            "include/",
            "api",
            None,
            Some(&table),
            None,
            20,
        );

        assert_eq!(items[0].label, "api.h");
    }

    #[test]
    fn angle_include_keeps_external_root_base_priority() {
        let root = tempdir().expect("root");
        fs::write(root.path().join("config.h"), "x").expect("external");
        let root_str = root.path().to_string_lossy().replace('\\', "/");
        let table = IncludeCompletionTable::build_with_edges(
            vec!["src/driver/config.h".to_string()],
            Vec::new(),
        );

        let items = collect_include_candidates_with_table(
            IncludeForm::Angle,
            "",
            "con",
            None,
            None,
            &[root_str],
            None,
            Some(&table),
            None,
            20,
        );

        assert_eq!(items[0].label, "config.h");
    }

    #[test]
    fn empty_include_completion_table_is_safe() {
        let table = IncludeCompletionTable::default();
        let items = collect_include_candidates_with_table(
            IncludeForm::Quote,
            "",
            "api",
            None,
            None,
            &[],
            None,
            Some(&table),
            None,
            20,
        );
        assert!(items.is_empty());
    }

    #[test]
    fn external_include_directory_cache_reuses_listing() {
        let root = tempdir().expect("root");
        fs::write(root.path().join("stdio.h"), "x").expect("stdio");
        let root_str = root.path().to_string_lossy().replace('\\', "/");
        let cache = Arc::new(StdMutex::new(HashMap::new()));

        let first = collect_include_candidates_with_table(
            IncludeForm::Angle,
            "",
            "std",
            None,
            None,
            std::slice::from_ref(&root_str),
            None,
            None,
            Some(&cache),
            20,
        );
        assert!(first.iter().any(|i| i.label == "stdio.h"));
        assert_eq!(cache.lock().expect("cache").len(), 1);

        let second = collect_include_candidates_with_table(
            IncludeForm::Angle,
            "",
            "std",
            None,
            None,
            std::slice::from_ref(&root_str),
            None,
            None,
            Some(&cache),
            20,
        );
        assert!(second.iter().any(|i| i.label == "stdio.h"));
        assert_eq!(cache.lock().expect("cache").len(), 1);
    }

    #[test]
    fn external_include_directory_cache_skips_invalid_root() {
        let root = tempdir().expect("root");
        let missing = root
            .path()
            .join("missing")
            .to_string_lossy()
            .replace('\\', "/");
        let cache = Arc::new(StdMutex::new(HashMap::new()));
        let items = collect_include_candidates_with_table(
            IncludeForm::Angle,
            "",
            "std",
            None,
            None,
            &[missing],
            None,
            None,
            Some(&cache),
            20,
        );
        assert!(items.is_empty());
        assert!(cache.lock().expect("cache").is_empty());
    }
}
