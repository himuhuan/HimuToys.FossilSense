use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use directories::ProjectDirs;

static INDEX_DB_LEASES: OnceLock<Mutex<std::collections::HashMap<PathBuf, Weak<()>>>> =
    OnceLock::new();

#[derive(Debug, Clone)]
pub struct IndexDbLease {
    path: PathBuf,
    token: Arc<()>,
}

impl IndexDbLease {
    pub fn acquire(path: PathBuf) -> Self {
        let registry = INDEX_DB_LEASES.get_or_init(Default::default);
        let mut leases = registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let token = leases
            .get(&path)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| {
                let token = Arc::new(());
                leases.insert(path.clone(), Arc::downgrade(&token));
                token
            });
        Self { path, token }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for IndexDbLease {
    fn drop(&mut self) {
        if Arc::strong_count(&self.token) != 1 {
            return;
        }
        let Some(directory) = self.path.parent() else {
            return;
        };
        let registry = INDEX_DB_LEASES.get_or_init(Default::default);
        let mut leases = registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if Arc::strong_count(&self.token) != 1 {
            return;
        }
        leases.remove(&self.path);
        let _ = cleanup_index_directory_locked(directory, &mut leases, stale_temp_cutoff());
    }
}

pub fn canonical_workspace(root: impl AsRef<Path>) -> Result<PathBuf> {
    let root = root.as_ref();
    root.canonicalize()
        .with_context(|| format!("failed to canonicalize workspace root {}", root.display()))
}

pub fn default_index_path(workspace: &Path) -> Result<PathBuf> {
    let directory = default_index_directory(workspace)?;
    resolve_active_index(&directory)
}

fn resolve_active_index(directory: &Path) -> Result<PathBuf> {
    let manifest = directory.join("active-index");
    if !manifest.exists() {
        return Ok(directory.join("index.sqlite"));
    }
    let file_name = fs::read_to_string(&manifest)
        .with_context(|| format!("failed to read index manifest {}", manifest.display()))?;
    let file_name = file_name.trim();
    let relative = Path::new(file_name);
    let is_single_file = matches!(
        relative.components().collect::<Vec<_>>().as_slice(),
        [Component::Normal(_)]
    );
    if !is_single_file || !file_name.starts_with("index-g") || !file_name.ends_with(".sqlite") {
        return Err(anyhow!(
            "invalid active index manifest entry in {}",
            manifest.display()
        ));
    }
    let active = directory.join(relative);
    if !active.is_file() {
        return Err(anyhow!(
            "active index manifest points to missing database {}",
            active.display()
        ));
    }
    Ok(active)
}

pub fn default_index_directory(workspace: &Path) -> Result<PathBuf> {
    let project_dirs = ProjectDirs::from("com", "HimuToys", "FossilSense")
        .ok_or_else(|| anyhow!("failed to locate user cache directory"))?;
    let workspace = canonical_workspace(workspace)?;
    let hash = workspace_hash(&workspace);
    Ok(project_dirs.cache_dir().join("indexes").join(hash))
}

pub fn default_index_staging_path(workspace: &Path) -> Result<PathBuf> {
    let directory = default_index_directory(workspace)?;
    fs::create_dir_all(&directory)
        .with_context(|| format!("failed to create index directory {}", directory.display()))?;
    let _ = cleanup_index_directory(&directory, stale_temp_cutoff());
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(directory.join(format!("index-build-{}-{nanos}.sqlite", std::process::id())))
}

/// Publish a completed, closed staging database through the workspace's active
/// manifest. The database rename happens first; the manifest replacement is the
/// single visibility point. Older generation files are intentionally retained
/// because an in-flight engine snapshot may still carry their path.
pub fn publish_default_index(workspace: &Path, staging: &Path, generation: u64) -> Result<PathBuf> {
    let directory = default_index_directory(workspace)?;
    publish_index_in_directory(&directory, staging, generation)
}

fn publish_index_in_directory(
    directory: &Path,
    staging: &Path,
    generation: u64,
) -> Result<PathBuf> {
    let staging_parent = staging.parent().map(Path::to_path_buf);
    if staging_parent.as_deref() != Some(directory) || !staging.is_file() {
        return Err(anyhow!(
            "index publication staging path is outside the workspace cache family"
        ));
    }
    let staging_name = staging
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("index staging path has no UTF-8 file name"))?;
    let token = staging_name
        .strip_prefix("index-build-")
        .ok_or_else(|| anyhow!("index staging path does not use the expected build prefix"))?;
    let final_name = format!("index-g{generation}-{token}.sqlite");
    let final_path = directory.join(&final_name);
    fs::rename(staging, &final_path).with_context(|| {
        format!(
            "failed to seal index database {} as {}",
            staging.display(),
            final_path.display()
        )
    })?;

    let manifest = directory.join("active-index");
    let manifest_staging = directory.join(format!("active-index-{token}.tmp"));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&manifest_staging)
        .with_context(|| {
            format!(
                "failed to create index manifest staging file {}",
                manifest_staging.display()
            )
        })?;
    file.write_all(final_name.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);
    atomic_replace(&manifest_staging, &manifest)?;
    let _ = cleanup_index_directory(directory, stale_temp_cutoff());
    Ok(final_path)
}

fn stale_temp_cutoff() -> SystemTime {
    SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(24 * 60 * 60))
        .unwrap_or(UNIX_EPOCH)
}

fn cleanup_index_directory(directory: &Path, temp_cutoff: SystemTime) -> Result<usize> {
    let registry = INDEX_DB_LEASES.get_or_init(Default::default);
    let mut leases = registry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cleanup_index_directory_locked(directory, &mut leases, temp_cutoff)
}

fn cleanup_index_directory_locked(
    directory: &Path,
    leases: &mut std::collections::HashMap<PathBuf, Weak<()>>,
    temp_cutoff: SystemTime,
) -> Result<usize> {
    leases.retain(|_, lease| lease.strong_count() > 0);
    let active_name = directory
        .join("active-index")
        .is_file()
        .then(|| resolve_active_index(directory).ok())
        .flatten()
        .and_then(|path| path.file_name().map(|name| name.to_owned()));
    let mut removed = 0;
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let Some(name_text) = name.to_str() else {
            continue;
        };
        let generation_base = name_text
            .strip_suffix("-wal")
            .or_else(|| name_text.strip_suffix("-shm"))
            .unwrap_or(name_text);
        let is_generation = generation_base.starts_with("index-g")
            && generation_base.ends_with(".sqlite")
            && generation_base[7..generation_base.len() - 7]
                .split('-')
                .next()
                .is_some_and(|value| value.parse::<u64>().is_ok());
        if is_generation {
            // Without a valid manifest there is no safe way to distinguish an
            // old generation from the last recoverable database.
            let Some(active_name) = active_name.as_deref() else {
                continue;
            };
            if active_name == std::ffi::OsStr::new(generation_base) {
                continue;
            }
            let base_path = directory.join(generation_base);
            if leases.contains_key(&base_path) {
                continue;
            }
            if fs::remove_file(&path).is_ok() {
                removed += 1;
            }
            continue;
        }

        let is_staging = name_text.starts_with("index-build-")
            || (name_text.starts_with("active-index-") && name_text.ends_with(".tmp"));
        if is_staging
            && entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .is_ok_and(|modified| modified <= temp_cutoff)
            && fs::remove_file(&path).is_ok()
        {
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(windows)]
fn atomic_replace(source: &Path, destination: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let destination_display = destination.display().to_string();
    let source_wide: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let moved = unsafe {
        MoveFileExW(
            source_wide.as_ptr(),
            destination_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!(
                "failed to atomically replace index manifest {}",
                destination_display
            )
        });
    }
    Ok(())
}

#[cfg(not(windows))]
fn atomic_replace(source: &Path, destination: &Path) -> Result<()> {
    fs::rename(source, destination).with_context(|| {
        format!(
            "failed to atomically replace index manifest {}",
            destination.display()
        )
    })
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
    use std::fs;
    use std::path::Path;
    use std::time::SystemTime;

    use tempfile::tempdir;

    use super::{
        cleanup_index_directory, default_index_path, path_is_within, publish_index_in_directory,
        relative_slash_path, resolve_active_index, IndexDbLease,
    };

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
    fn generation_manifest_switch_keeps_old_database_and_resolves_new_one() {
        let dir = tempdir().expect("tempdir");
        assert_eq!(
            resolve_active_index(dir.path()).expect("legacy fallback"),
            dir.path().join("index.sqlite")
        );

        let first_staging = dir.path().join("index-build-first.sqlite");
        fs::write(&first_staging, b"first").expect("first staging");
        let first =
            publish_index_in_directory(dir.path(), &first_staging, 1).expect("publish first");
        let first_lease = IndexDbLease::acquire(first.clone());
        assert_eq!(resolve_active_index(dir.path()).unwrap(), first);

        let second_staging = dir.path().join("index-build-second.sqlite");
        fs::write(&second_staging, b"second").expect("second staging");
        let second =
            publish_index_in_directory(dir.path(), &second_staging, 2).expect("publish second");
        assert_eq!(resolve_active_index(dir.path()).unwrap(), second);
        assert_eq!(fs::read(&first).unwrap(), b"first");
        assert_eq!(fs::read(&second).unwrap(), b"second");
        assert_eq!(
            fs::read_to_string(dir.path().join("active-index"))
                .unwrap()
                .trim(),
            second.file_name().unwrap().to_string_lossy()
        );
        drop(first_lease);
        assert!(!first.exists(), "released old generation is cleaned");
        assert_eq!(fs::read(&second).unwrap(), b"second");
    }

    #[test]
    fn cleanup_respects_temp_cutoff_and_preserves_leased_generation() {
        let dir = tempdir().expect("tempdir");
        let active = dir.path().join("index-g3-active.sqlite");
        let leased = dir.path().join("index-g2-leased.sqlite");
        let old = dir.path().join("index-g1-old.sqlite");
        fs::write(&active, b"active").unwrap();
        fs::write(&leased, b"leased").unwrap();
        fs::write(&old, b"old").unwrap();
        fs::write(dir.path().join("active-index"), "index-g3-active.sqlite\n").unwrap();
        let stale_build = dir.path().join("index-build-stale.sqlite");
        let live_build = dir.path().join("index-build-live.sqlite");
        fs::write(&stale_build, b"stale").unwrap();
        fs::write(&live_build, b"live").unwrap();
        let lease = IndexDbLease::acquire(leased.clone());

        cleanup_index_directory(dir.path(), SystemTime::UNIX_EPOCH).expect("generation cleanup");
        assert!(active.exists());
        assert!(leased.exists());
        assert!(!old.exists());
        assert!(stale_build.exists());
        assert!(live_build.exists());
        cleanup_index_directory(
            dir.path(),
            SystemTime::now() + std::time::Duration::from_secs(1),
        )
        .expect("temp cleanup");
        assert!(!stale_build.exists());
        assert!(!live_build.exists());
        drop(lease);
        assert!(!leased.exists());
    }

    #[test]
    #[ignore = "diagnostic generation-family cleanup benchmark"]
    fn benchmark_generation_family_cleanup() {
        let dir = tempdir().expect("tempdir");
        let active = dir.path().join("index-g1001-active.sqlite");
        fs::write(&active, b"active").unwrap();
        fs::write(
            dir.path().join("active-index"),
            "index-g1001-active.sqlite\n",
        )
        .unwrap();
        let leased = dir.path().join("index-g500-leased.sqlite");
        for generation in 1..=1_000 {
            let path = if generation == 500 {
                leased.clone()
            } else {
                dir.path().join(format!("index-g{generation}-old.sqlite"))
            };
            fs::write(path, b"old").unwrap();
        }
        let lease = IndexDbLease::acquire(leased.clone());
        let started = std::time::Instant::now();
        let removed = cleanup_index_directory(dir.path(), SystemTime::now()).expect("cleanup");
        let elapsed_us = started.elapsed().as_micros();
        println!("generation_cleanup_files: {removed}");
        println!("generation_cleanup_us: {elapsed_us}");
        assert_eq!(removed, 999);
        assert!(active.exists());
        assert!(leased.exists());
        drop(lease);
        assert!(!leased.exists());
    }

    #[test]
    fn generation_manifest_rejects_traversal_and_missing_targets() {
        let dir = tempdir().expect("tempdir");
        let recoverable = dir.path().join("index-g8-recoverable.sqlite");
        fs::write(&recoverable, b"recoverable").unwrap();
        fs::write(dir.path().join("active-index"), "../outside.sqlite\n").expect("bad manifest");
        assert!(resolve_active_index(dir.path()).is_err());
        cleanup_index_directory(dir.path(), SystemTime::now()).expect("safe cleanup");
        assert!(
            recoverable.exists(),
            "invalid manifest must preserve recoverable generations"
        );
        fs::write(dir.path().join("active-index"), "index-g9-missing.sqlite\n")
            .expect("missing manifest");
        assert!(resolve_active_index(dir.path()).is_err());
        cleanup_index_directory(dir.path(), SystemTime::now()).expect("safe cleanup");
        assert!(recoverable.exists());
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
