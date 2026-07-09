use std::collections::HashSet;
use std::path::Path;

use tower_lsp::lsp_types::CompletionItem;

use crate::store::IndexStore;

use super::presentation::push_include_candidate;
use super::{CurrentIncludeEvidence, IncludeCompletionMetrics, IncludeCompletionTable};

#[allow(clippy::too_many_arguments)]
pub(super) fn collect_workspace_include_candidates(
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
    let Ok(paths) = store.include_table_view().workspace_file_paths() else {
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
pub(super) struct IndexedIncludeCandidate {
    pub(super) name: String,
    pub(super) is_dir: bool,
    pub(super) rel_path: String,
}

pub(super) fn indexed_workspace_include_candidates(
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
                && super::looks_like_header(name)
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
        && (is_dir || super::looks_like_header(name))
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
