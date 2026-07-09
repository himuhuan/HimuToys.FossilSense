use std::collections::HashSet;
use std::path::{Path, PathBuf};

use tower_lsp::lsp_types::CompletionItem;

use crate::includes::IncludeForm;

mod disk;
mod evidence;
mod paths;
mod presentation;
mod table;
#[cfg(test)]
mod tests;
mod workspace_candidates;

use disk::{collect_cached_disk_include_candidates, collect_disk_include_candidates};
use workspace_candidates::collect_workspace_include_candidates;

pub(super) use disk::{looks_like_header, ExternalIncludeDirCache};
pub(super) use evidence::CurrentIncludeEvidence;
pub(super) use paths::{configured_include_paths, location_at_file_start, resolve_include_paths};
pub(super) use table::{IncludeCompletionMetrics, IncludeCompletionTable};

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
