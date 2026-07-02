use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use directories::ProjectDirs;

pub fn canonical_workspace(root: impl AsRef<Path>) -> Result<PathBuf> {
    let root = root.as_ref();
    root.canonicalize()
        .with_context(|| format!("failed to canonicalize workspace root {}", root.display()))
}

pub fn default_index_path(workspace: &Path) -> Result<PathBuf> {
    let project_dirs = ProjectDirs::from("com", "HimuToys", "FossilSense")
        .ok_or_else(|| anyhow!("failed to locate user cache directory"))?;
    let workspace = canonical_workspace(workspace)?;
    let hash = workspace_hash(&workspace);
    Ok(project_dirs
        .cache_dir()
        .join("indexes")
        .join(hash)
        .join("index.sqlite"))
}

pub fn workspace_hash(workspace: &Path) -> String {
    let normalized = normalize_path_string(workspace);
    blake3::hash(normalized.as_bytes()).to_hex()[..16].to_string()
}

pub fn relative_slash_path(root: &Path, path: &Path) -> Result<String> {
    let relative = path.strip_prefix(root).with_context(|| {
        format!(
            "failed to make {} relative to {}",
            path.display(),
            root.display()
        )
    })?;
    Ok(normalize_path_string(relative))
}

pub fn normalize_path_string(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().replace('\\', "/"))
        .collect::<Vec<_>>()
        .join("/")
}

/// Normalize an *absolute* path (e.g. an external include file outside the
/// workspace) to a `/`-separated string. Unlike [`relative_slash_path`], this
/// does not strip a workspace prefix: external files cannot be made
/// workspace-relative, so they are stored as full, slash-normalized paths.
pub fn normalize_abs_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::default_index_path;

    #[test]
    fn default_index_path_uses_canonical_workspace_hash() {
        let dir = tempdir().expect("tempdir");
        let raw = dir.path().to_path_buf();
        let canonical = raw.canonicalize().expect("canonical");

        assert_eq!(
            default_index_path(&raw).expect("raw path"),
            default_index_path(&canonical).expect("canonical path")
        );
    }
}
