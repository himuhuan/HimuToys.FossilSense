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

pub fn default_completion_history_path(workspace: &Path) -> Result<PathBuf> {
    Ok(default_index_path(workspace)?.with_file_name("completion_history.json"))
}

pub fn workspace_hash(workspace: &Path) -> String {
    let normalized = normalize_path_string(workspace);
    blake3::hash(normalized.as_bytes()).to_hex()[..16].to_string()
}

pub fn relative_slash_path(root: &Path, path: &Path) -> Result<String> {
    if let Ok(relative) = path.strip_prefix(root) {
        return Ok(normalize_path_string(relative));
    }

    // Windows paths are case-insensitive, but `Path::strip_prefix` compares
    // components byte-for-byte. File URIs can preserve a different drive or
    // directory spelling from the canonical workspace root, so fall back to a
    // component-wise comparison before deriving the relative suffix.
    #[cfg(windows)]
    if path_is_within(root, path) {
        let root_depth = root.components().count();
        let relative = path
            .components()
            .skip(root_depth)
            .map(|component| component.as_os_str().to_string_lossy().replace('\\', "/"))
            .collect::<Vec<_>>()
            .join("/");
        return Ok(relative);
    }

    Err(anyhow!(
        "failed to make {} relative to {}",
        path.display(),
        root.display()
    ))
}

/// Whether `path` is equal to or nested under `root` on the host filesystem.
/// Windows comparison is ASCII-case-insensitive and always respects component
/// boundaries; other platforms retain `Path::starts_with` semantics.
pub fn path_is_within(root: &Path, path: &Path) -> bool {
    if path.starts_with(root) {
        return true;
    }

    #[cfg(windows)]
    {
        let root_components = root
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>();
        let path_components = path
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>();
        root_components.len() <= path_components.len()
            && root_components
                .iter()
                .zip(path_components.iter())
                .all(|(left, right)| left.eq_ignore_ascii_case(right))
    }

    #[cfg(not(windows))]
    {
        false
    }
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
    use std::path::Path;

    use tempfile::tempdir;

    use super::{default_index_path, path_is_within, relative_slash_path};

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

    #[test]
    fn path_containment_respects_component_boundaries() {
        assert!(path_is_within(
            Path::new("workspace/root"),
            Path::new("workspace/root/src/main.c")
        ));
        assert!(!path_is_within(
            Path::new("workspace/root"),
            Path::new("workspace/root-other/main.c")
        ));
    }

    #[cfg(windows)]
    #[test]
    fn windows_workspace_paths_accept_case_variants() {
        let root = Path::new(r"C:\Work\Firmware");
        let file = Path::new(r"c:\work\FIRMWARE\Src\Main.c");
        assert!(path_is_within(root, file));
        assert_eq!(
            relative_slash_path(root, file).expect("relative"),
            "Src/Main.c"
        );
    }
}
