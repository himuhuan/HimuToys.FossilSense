use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use tower_lsp::lsp_types::{Location, Position, Range, Url};

use crate::config::WorkspaceConfig;
use crate::includes::IncludeForm;
use crate::pathing;
use crate::store::IndexStore;

pub(in crate::server) fn configured_include_paths(
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
pub(in crate::server) fn resolve_include_paths(
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
                .include_table_view()
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
pub(in crate::server) fn location_at_file_start(path: &Path) -> Option<Location> {
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
