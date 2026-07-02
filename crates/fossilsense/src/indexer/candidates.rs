use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use ignore::WalkBuilder;
use rayon::prelude::*;

use crate::config::{normalized_extension, ConfigIssue, WorkspaceConfig, DEFAULT_EXCLUDED_DIRS};
use crate::pathing::{normalize_abs_path, relative_slash_path};
use crate::store::{FileFingerprint, FileSource};

/// Default per-root caps for external include directories. A root over either
/// cap is indexed for path resolution only (no symbols), never an error.
pub(super) const DEFAULT_EXTERNAL_MAX_FILES: usize = 20_000;
pub(super) const DEFAULT_EXTERNAL_MAX_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(super) struct FileCandidate {
    pub(super) absolute_path: PathBuf,
    pub(super) fingerprint: FileFingerprint,
    pub(super) source: FileSource,
}

pub(super) fn discover_candidates(
    root: &Path,
    config: &WorkspaceConfig,
) -> Result<Vec<FileCandidate>> {
    let walk_config = config.clone();
    let filter_root = root.to_path_buf();
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .parents(true)
        .git_ignore(true)
        .git_global(true)
        .filter_entry(move |entry| {
            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            let rel = relative_slash_path(&filter_root, entry.path()).unwrap_or_default();
            walk_config.keep_during_walk(&rel, is_dir)
        });

    let mut paths = Vec::new();

    for entry in builder.build() {
        let entry = entry.with_context(|| format!("failed to walk under {}", root.display()))?;
        let path = entry.path();

        if !entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
        {
            continue;
        }

        let rel_slash = relative_slash_path(root, path)?;
        if !config.is_in_scope(&rel_slash) {
            continue;
        }

        paths.push((path.to_path_buf(), rel_slash));
    }

    let mut candidates: Vec<FileCandidate> = paths
        .into_par_iter()
        .map(|(path, rel_slash)| candidate_for_path(&path, rel_slash))
        .collect::<Result<Vec<_>>>()?;
    candidates.sort_by(|left, right| left.fingerprint.path.cmp(&right.fingerprint.path));
    Ok(candidates)
}

pub(super) fn candidate_for_path(path: &Path, rel_slash: String) -> Result<FileCandidate> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to read metadata for {}", path.display()))?;
    let mtime_ns = metadata_mtime_ns(&metadata);

    Ok(FileCandidate {
        absolute_path: path.to_path_buf(),
        source: FileSource::Workspace,
        fingerprint: FileFingerprint {
            path: rel_slash,
            extension: normalized_extension(path)
                .map(|ext| ext.to_ascii_lowercase())
                .unwrap_or_default(),
            size: metadata.len(),
            mtime_ns,
            hash: metadata_hash(metadata.len(), mtime_ns),
        },
    })
}

pub(super) fn canonicalize_existing_prefix(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }

    let mut cursor = path;
    let mut missing: Vec<OsString> = Vec::new();
    while !cursor.exists() {
        let Some(name) = cursor.file_name() else {
            return path.to_path_buf();
        };
        missing.push(name.to_owned());
        let Some(parent) = cursor.parent() else {
            return path.to_path_buf();
        };
        cursor = parent;
    }

    let Ok(mut canonical) = cursor.canonicalize() else {
        return path.to_path_buf();
    };
    for name in missing.iter().rev() {
        canonical.push(name);
    }
    canonical
}

/// Walk each external include root (ignoring `.gitignore`), filtered to the
/// configured extensions, into `External` candidates. A root that exceeds either
/// cap is dropped entirely (path-resolution-only) with an issue — never an
/// error. External fingerprints use a metadata-only hash so an unchanged pass
/// does not re-read large toolchain trees.
pub(super) fn discover_external_candidates(
    roots: &[PathBuf],
    config: &WorkspaceConfig,
    max_files: usize,
    max_bytes: u64,
) -> (Vec<FileCandidate>, Vec<ConfigIssue>) {
    let mut out = Vec::new();
    let mut issues = Vec::new();

    for root in roots {
        let mut root_candidates = Vec::new();
        let mut bytes = 0u64;
        let mut over_cap = false;

        let builder = WalkBuilder::new(root)
            .hidden(false)
            .parents(false)
            .ignore(false)
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .filter_entry(|entry| {
                let name = entry.file_name().to_string_lossy();
                !DEFAULT_EXCLUDED_DIRS
                    .iter()
                    .any(|excluded| name.eq_ignore_ascii_case(excluded))
            })
            .build();

        for entry in builder {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                continue;
            }
            let Some(ext) = normalized_extension(path) else {
                continue;
            };
            if !config
                .extensions
                .iter()
                .any(|allowed| ext.eq_ignore_ascii_case(allowed))
            {
                continue;
            }
            let Ok(candidate) = external_candidate_for_path(path) else {
                continue;
            };
            bytes = bytes.saturating_add(candidate.fingerprint.size);
            root_candidates.push(candidate);
            if root_candidates.len() > max_files || bytes > max_bytes {
                over_cap = true;
                break;
            }
        }

        if over_cap {
            issues.push(ConfigIssue {
                message: format!(
                    "includePaths root exceeds cap (>{max_files} files or >{max_bytes} bytes); indexing paths only, no symbols: {}",
                    root.display()
                ),
            });
        } else {
            out.extend(root_candidates);
        }
    }

    (out, issues)
}

/// Build an external candidate without reading file contents: the fingerprint
/// hash is derived from size+mtime so the unchanged check is cheap. The actual
/// content is only read for files that need (re)parsing.
fn external_candidate_for_path(path: &Path) -> Result<FileCandidate> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to read metadata for {}", path.display()))?;
    let mtime_ns = metadata_mtime_ns(&metadata);

    Ok(FileCandidate {
        absolute_path: path.to_path_buf(),
        source: FileSource::External,
        fingerprint: FileFingerprint {
            path: normalize_abs_path(path),
            extension: normalized_extension(path)
                .map(|ext| ext.to_ascii_lowercase())
                .unwrap_or_default(),
            size: metadata.len(),
            mtime_ns,
            hash: metadata_hash(metadata.len(), mtime_ns),
        },
    })
}

fn metadata_mtime_ns(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| {
            (duration.as_secs() as i64)
                .saturating_mul(1_000_000_000)
                .saturating_add(duration.subsec_nanos() as i64)
        })
        .unwrap_or_default()
}

fn metadata_hash(size: u64, mtime_ns: i64) -> String {
    blake3::hash(format!("{size}-{mtime_ns}").as_bytes())
        .to_hex()
        .to_string()
}
