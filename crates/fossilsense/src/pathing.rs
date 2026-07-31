use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use directories::ProjectDirs;

static EXPLICIT_INDEX_BUILD_SEQUENCE: AtomicU64 = AtomicU64::new(0);

mod leases;
pub use leases::IndexDbLease;

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

/// Owns a fresh sibling database used to replace an explicit `--db` target.
///
/// Keeping the staging file in the destination directory gives publication a
/// single-filesystem atomic rename. If indexing or validation fails, dropping
/// this guard removes only the uniquely named staging database and its SQLite
/// sidecars; the previously published destination remains untouched.
#[derive(Debug)]
pub struct ExplicitIndexPublication {
    destination: PathBuf,
    staging: PathBuf,
    published: bool,
}

impl ExplicitIndexPublication {
    pub fn new(destination: impl Into<PathBuf>) -> Result<Self> {
        let destination = destination.into();
        destination
            .file_name()
            .ok_or_else(|| anyhow!("explicit index destination has no file name"))?;
        let directory = destination
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(directory).with_context(|| {
            format!(
                "failed to create explicit index directory {}",
                directory.display()
            )
        })?;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = EXPLICIT_INDEX_BUILD_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let staging = directory.join(format!(
            ".fossilsense-index-build-{}-{nanos}-{sequence}.sqlite",
            std::process::id()
        ));
        let staging_exists = staging.try_exists().with_context(|| {
            format!(
                "failed to inspect explicit index staging path {}",
                staging.display()
            )
        })?;
        anyhow::ensure!(
            staging != destination && !staging_exists,
            "explicit index staging path already exists: {}",
            staging.display()
        );
        Ok(Self {
            destination,
            staging,
            published: false,
        })
    }

    pub fn staging_path(&self) -> &Path {
        &self.staging
    }

    pub fn publish(mut self) -> Result<()> {
        let staging_metadata = fs::metadata(&self.staging).with_context(|| {
            format!(
                "failed to inspect explicit index staging database {}",
                self.staging.display()
            )
        })?;
        anyhow::ensure!(
            staging_metadata.is_file(),
            "explicit index staging database is not a regular file: {}",
            self.staging.display()
        );
        ensure_sqlite_sidecars_absent(&self.staging)?;
        ensure_sqlite_sidecars_absent(&self.destination)?;
        atomic_replace(&self.staging, &self.destination)?;
        self.published = true;
        Ok(())
    }
}

impl Drop for ExplicitIndexPublication {
    fn drop(&mut self) {
        if !self.published {
            remove_sqlite_file_family(&self.staging);
        }
    }
}

#[derive(Debug)]
struct UnpublishedDefaultIndex {
    database: PathBuf,
    manifest_staging: Option<PathBuf>,
    published: bool,
}

impl UnpublishedDefaultIndex {
    fn new(database: PathBuf) -> Self {
        Self {
            database,
            manifest_staging: None,
            published: false,
        }
    }

    fn own_manifest_staging(&mut self, path: PathBuf) {
        self.manifest_staging = Some(path);
    }

    fn mark_published(mut self) {
        self.published = true;
    }
}

impl Drop for UnpublishedDefaultIndex {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        if let Some(manifest_staging) = self.manifest_staging.as_deref() {
            let _ = fs::remove_file(manifest_staging);
        }
        remove_sqlite_file_family(&self.database);
    }
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
    let publication_lease = crate::store::GenerationPublicationLease::acquire(directory)?;
    fs::rename(staging, &final_path).with_context(|| {
        format!(
            "failed to seal index database {} as {}",
            staging.display(),
            final_path.display()
        )
    })?;
    let mut unpublished = UnpublishedDefaultIndex::new(final_path.clone());

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
    unpublished.own_manifest_staging(manifest_staging.clone());
    file.write_all(final_name.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);
    atomic_replace(&manifest_staging, &manifest)?;
    unpublished.mark_published();
    drop(publication_lease);
    let _ = cleanup_index_directory(directory, stale_temp_cutoff());
    Ok(final_path)
}

fn stale_temp_cutoff() -> SystemTime {
    SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(24 * 60 * 60))
        .unwrap_or(UNIX_EPOCH)
}

fn cleanup_index_directory(directory: &Path, temp_cutoff: SystemTime) -> Result<usize> {
    let generation_cleanup = crate::store::GenerationCleanupLease::try_acquire(directory)?;
    cleanup_index_directory_with_lease(directory, temp_cutoff, generation_cleanup)
}

fn cleanup_index_directory_after_reader_release(
    directory: &Path,
    temp_cutoff: SystemTime,
) -> Result<usize> {
    let generation_cleanup =
        crate::store::GenerationCleanupLease::acquire_after_reader_release(directory)?;
    cleanup_index_directory_with_lease(directory, temp_cutoff, Some(generation_cleanup))
}

fn cleanup_index_directory_with_lease(
    directory: &Path,
    temp_cutoff: SystemTime,
    generation_cleanup: Option<crate::store::GenerationCleanupLease>,
) -> Result<usize> {
    let active_name = directory
        .join("active-index")
        .is_file()
        .then(|| resolve_active_index(directory).ok())
        .flatten()
        .and_then(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
        });
    let mut removed = 0;
    let mut generation_files = BTreeMap::<String, Vec<PathBuf>>::new();
    let mut generation_lease_databases = BTreeSet::<PathBuf>::new();
    let mut stale_staging = Vec::new();
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
        if let Some(lease_database) =
            crate::store::GenerationCleanupLease::lease_database_for_entry(directory, name_text)
        {
            generation_lease_databases.insert(lease_database);
            continue;
        }
        let generation_base = name_text
            .strip_suffix("-wal")
            .or_else(|| name_text.strip_suffix("-shm"))
            .or_else(|| name_text.strip_suffix("-journal"))
            .unwrap_or(name_text);
        let is_generation = is_generation_database_name(generation_base);
        if is_generation {
            generation_files
                .entry(generation_base.to_owned())
                .or_default()
                .push(path);
            continue;
        }

        let is_staging = name_text.starts_with("index-build-")
            || (name_text.starts_with("active-index-") && name_text.ends_with(".tmp"));
        if is_staging
            && entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .is_ok_and(|modified| modified <= temp_cutoff)
        {
            stale_staging.push(path);
        }
    }

    if let (Some(cleanup), Some(active_name)) =
        (generation_cleanup.as_ref(), active_name.as_deref())
    {
        for (generation_name, paths) in generation_files {
            if generation_name == active_name {
                continue;
            }
            let database = directory.join(&generation_name);
            let Some(deletion) = cleanup.try_acquire_generation(&database)? else {
                continue;
            };
            for path in paths {
                if fs::remove_file(path).is_ok() {
                    removed += 1;
                }
            }
            if !database.exists() {
                deletion.remove_lease_database();
            }
        }
    }
    for path in stale_staging {
        if fs::remove_file(path).is_ok() {
            removed += 1;
        }
    }
    if let Some(cleanup) = generation_cleanup.as_ref() {
        for lease_database in generation_lease_databases {
            let _ = cleanup.try_remove_lease_database(&lease_database)?;
        }
    }
    drop(generation_cleanup);
    Ok(removed)
}

fn generation_index_directory(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    is_generation_database_name(name).then(|| path.parent().map(Path::to_path_buf))?
}

fn is_generation_database_name(name: &str) -> bool {
    name.starts_with("index-g")
        && name.ends_with(".sqlite")
        && name[7..name.len() - 7]
            .split('-')
            .next()
            .is_some_and(|value| value.parse::<u64>().is_ok())
}

fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    sidecar.into()
}

fn ensure_sqlite_sidecars_absent(path: &Path) -> Result<()> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let sidecar = sqlite_sidecar_path(path, suffix);
        match sidecar.try_exists() {
            Ok(false) => {}
            Ok(true) => anyhow::bail!(
                "refusing to replace SQLite database while sidecar exists: {}",
                sidecar.display()
            ),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to inspect SQLite sidecar {}", sidecar.display())
                });
            }
        }
    }
    Ok(())
}

fn remove_sqlite_file_family(path: &Path) {
    let _ = fs::remove_file(path);
    for suffix in ["-wal", "-shm", "-journal"] {
        let _ = fs::remove_file(sqlite_sidecar_path(path, suffix));
    }
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
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to atomically replace {}", destination_display));
    }
    Ok(())
}

#[cfg(not(windows))]
fn atomic_replace(source: &Path, destination: &Path) -> Result<()> {
    fs::rename(source, destination)
        .with_context(|| format!("failed to atomically replace {}", destination.display()))
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
mod lease_tests;
#[cfg(all(test, windows))]
mod windows_tests;

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::sync::{Arc, Barrier};
    use std::time::SystemTime;

    use tempfile::tempdir;

    use super::{
        cleanup_index_directory, default_index_path, path_is_within, publish_index_in_directory,
        relative_slash_path, resolve_active_index, ExplicitIndexPublication, IndexDbLease,
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
        let first_lease =
            IndexDbLease::acquire_default_generation(first.clone()).expect("first lease");
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
    fn manifest_staging_collision_reclaims_generation_without_deleting_conflicting_file() {
        let dir = tempdir().expect("tempdir");
        let first_staging = dir.path().join("index-build-first.sqlite");
        fs::write(&first_staging, b"first").expect("first staging");
        let first =
            publish_index_in_directory(dir.path(), &first_staging, 1).expect("publish first");

        let conflicting_manifest_staging = dir.path().join("active-index-collision.tmp");
        fs::write(&conflicting_manifest_staging, b"foreign").expect("conflicting manifest staging");
        let second_staging = dir.path().join("index-build-collision.sqlite");
        fs::write(&second_staging, b"second").expect("second staging");
        let error = publish_index_in_directory(dir.path(), &second_staging, 2)
            .expect_err("create_new must reject conflicting manifest staging");

        assert!(
            error
                .to_string()
                .contains("failed to create index manifest staging file"),
            "unexpected publication error: {error:#}"
        );
        assert_eq!(resolve_active_index(dir.path()).unwrap(), first);
        assert_eq!(
            fs::read(&conflicting_manifest_staging).expect("preserved conflicting file"),
            b"foreign",
            "guard must only remove manifest staging it created"
        );
        assert!(
            !dir.path().join("index-g2-collision.sqlite").exists(),
            "unpublished generation must be reclaimed after create_new failure"
        );
    }

    #[test]
    fn explicit_publication_replaces_only_the_destination_database() {
        let dir = tempdir().expect("tempdir");
        let destination = dir.path().join("explicit.sqlite");
        fs::write(&destination, b"old").expect("old destination");
        let publication = ExplicitIndexPublication::new(destination.clone()).expect("publication");
        fs::write(publication.staging_path(), b"new").expect("new staging");
        publication.publish().expect("publish");

        assert_eq!(
            fs::read(destination).expect("published destination"),
            b"new"
        );
    }

    #[test]
    fn explicit_publication_refuses_old_wal_and_discards_only_staging() {
        let dir = tempdir().expect("tempdir");
        let destination = dir.path().join("explicit.sqlite");
        let wal = dir.path().join("explicit.sqlite-wal");
        fs::write(&destination, b"old").expect("old destination");
        fs::write(&wal, b"pending").expect("old WAL");
        let publication = ExplicitIndexPublication::new(destination.clone()).expect("publication");
        let staging = publication.staging_path().to_path_buf();
        fs::write(&staging, b"new").expect("new staging");
        let error = publication.publish().expect_err("WAL must block publish");

        assert!(error.to_string().contains("sidecar exists"));
        assert_eq!(
            fs::read(destination).expect("preserved destination"),
            b"old"
        );
        assert_eq!(fs::read(wal).expect("preserved WAL"), b"pending");
        assert!(!staging.exists(), "failed staging is owned by the guard");
    }

    #[test]
    fn explicit_publication_staging_name_has_a_fixed_component_budget() {
        let dir = tempdir().expect("tempdir");
        let long_name = format!("{}.sqlite", "x".repeat(220));
        let destination = dir.path().join(long_name);
        let publication = ExplicitIndexPublication::new(destination).expect("publication");
        let staging_name = publication
            .staging_path()
            .file_name()
            .expect("staging file name")
            .to_string_lossy();

        assert!(
            staging_name.len() <= 96,
            "staging component unexpectedly copied the destination name: {staging_name}"
        );
    }

    #[test]
    fn explicit_generation_shaped_database_does_not_create_default_family_lease() {
        let dir = tempdir().expect("tempdir");
        let explicit = dir.path().join("index-g1-custom.sqlite");
        fs::write(&explicit, b"explicit").expect("explicit database");

        let lease = IndexDbLease::acquire(explicit);

        assert!(
            !dir.path()
                .join(".fossilsense-generation-leases.sqlite")
                .exists(),
            "an explicit path must not turn its parent into a default index family"
        );
        drop(lease);
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
        let lease =
            IndexDbLease::acquire_default_generation(leased.clone()).expect("leased generation");

        cleanup_index_directory(dir.path(), SystemTime::UNIX_EPOCH).expect("generation cleanup");
        assert!(active.exists());
        assert!(leased.exists());
        assert!(!old.exists(), "an unleased old generation is reclaimable");
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
    fn releasing_old_generation_reclaims_it_while_active_generation_remains_leased() {
        let dir = tempdir().expect("tempdir");
        let old = dir.path().join("index-g1-old.sqlite");
        let active = dir.path().join("index-g2-active.sqlite");
        fs::write(&old, b"old").expect("old generation");
        fs::write(&active, b"active").expect("active generation");
        fs::write(dir.path().join("active-index"), "index-g2-active.sqlite\n")
            .expect("active manifest");
        let old_lease =
            IndexDbLease::acquire_default_generation(old.clone()).expect("old generation lease");
        let active_lease = IndexDbLease::acquire_default_generation(active.clone())
            .expect("active generation lease");

        drop(old_lease);

        assert!(
            !old.exists(),
            "the active generation lease must not defer cleanup of an unleased old generation"
        );
        assert!(
            active.exists(),
            "cleanup must preserve the active generation"
        );
        drop(active_lease);
    }

    #[test]
    fn default_generation_lease_rejects_a_database_deleted_before_acquisition() {
        let dir = tempdir().expect("tempdir");
        let missing = dir.path().join("index-g1-missing.sqlite");

        let lease = IndexDbLease::acquire_default_generation(missing);

        assert!(
            lease.is_err(),
            "a successful default-generation lease must always reference a readable file"
        );
        let orphan_leases = fs::read_dir(dir.path())
            .expect("lease directory")
            .flatten()
            .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
            .filter(|name| {
                name.starts_with(".fossilsense-generation-")
                    && name != ".fossilsense-generation-leases.sqlite"
            })
            .collect::<Vec<_>>();
        assert!(
            orphan_leases.is_empty(),
            "failed acquisition leaked per-generation lease databases: {orphan_leases:?}"
        );
    }

    #[test]
    fn concurrent_final_clone_release_always_reclaims_the_old_generation() {
        let dir = tempdir().expect("tempdir");
        let active = dir.path().join("index-g1000-active.sqlite");
        fs::write(&active, b"active").expect("active generation");
        fs::write(
            dir.path().join("active-index"),
            "index-g1000-active.sqlite\n",
        )
        .expect("active manifest");
        let active_lease =
            IndexDbLease::acquire_default_generation(active).expect("active generation lease");

        for generation in 1..=64 {
            let old = dir.path().join(format!("index-g{generation}-old.sqlite"));
            fs::write(&old, b"old").expect("old generation");
            let first = IndexDbLease::acquire_default_generation(old.clone())
                .expect("old generation lease");
            let second = first.clone();
            let barrier = Arc::new(Barrier::new(3));
            let first_barrier = Arc::clone(&barrier);
            let first_dropper = std::thread::spawn(move || {
                first_barrier.wait();
                drop(first);
            });
            let second_barrier = Arc::clone(&barrier);
            let second_dropper = std::thread::spawn(move || {
                second_barrier.wait();
                drop(second);
            });
            barrier.wait();
            first_dropper.join().expect("first lease dropper");
            second_dropper.join().expect("second lease dropper");
            assert!(
                !old.exists(),
                "concurrent final clone release skipped generation {generation} cleanup"
            );
        }
        drop(active_lease);
    }

    #[test]
    fn concurrent_distinct_generation_release_does_not_miss_either_generation() {
        let dir = tempdir().expect("tempdir");
        let active = dir.path().join("index-g1000-active.sqlite");
        fs::write(&active, b"active").expect("active generation");
        fs::write(
            dir.path().join("active-index"),
            "index-g1000-active.sqlite\n",
        )
        .expect("active manifest");
        let active_lease =
            IndexDbLease::acquire_default_generation(active).expect("active generation lease");

        for pair in 0..64 {
            let first_path = dir
                .path()
                .join(format!("index-g{}-first.sqlite", pair * 2 + 1));
            let second_path = dir
                .path()
                .join(format!("index-g{}-second.sqlite", pair * 2 + 2));
            fs::write(&first_path, b"first").expect("first old generation");
            fs::write(&second_path, b"second").expect("second old generation");
            let first = IndexDbLease::acquire_default_generation(first_path.clone())
                .expect("first old generation lease");
            let second = IndexDbLease::acquire_default_generation(second_path.clone())
                .expect("second old generation lease");
            let barrier = Arc::new(Barrier::new(3));
            let first_barrier = Arc::clone(&barrier);
            let first_dropper = std::thread::spawn(move || {
                first_barrier.wait();
                drop(first);
            });
            let second_barrier = Arc::clone(&barrier);
            let second_dropper = std::thread::spawn(move || {
                second_barrier.wait();
                drop(second);
            });
            barrier.wait();
            first_dropper.join().expect("first generation dropper");
            second_dropper.join().expect("second generation dropper");
            assert!(
                !first_path.exists() && !second_path.exists(),
                "concurrent distinct generation release skipped pair {pair} cleanup"
            );
        }
        drop(active_lease);
    }

    #[test]
    fn generation_cleanup_removes_all_sqlite_sidecars() {
        let dir = tempdir().expect("tempdir");
        let active = dir.path().join("index-g2-active.sqlite");
        let old = dir.path().join("index-g1-old.sqlite");
        fs::write(&active, b"active").expect("active generation");
        fs::write(&old, b"old").expect("old generation");
        fs::write(dir.path().join("active-index"), "index-g2-active.sqlite\n")
            .expect("active manifest");
        let sidecars = ["-wal", "-shm", "-journal"]
            .map(|suffix| dir.path().join(format!("index-g1-old.sqlite{suffix}")));
        for sidecar in &sidecars {
            fs::write(sidecar, b"sidecar").expect("generation sidecar");
        }

        cleanup_index_directory(dir.path(), SystemTime::now()).expect("generation cleanup");

        assert!(!old.exists(), "old generation must be removed");
        for sidecar in sidecars {
            assert!(
                !sidecar.exists(),
                "old generation sidecar survived cleanup: {}",
                sidecar.display()
            );
        }
    }

    #[test]
    fn later_cleanup_reclaims_an_orphaned_generation_lease_database() {
        let dir = tempdir().expect("tempdir");
        let active = dir.path().join("index-g2-active.sqlite");
        let orphaned = dir.path().join("index-g1-orphaned.sqlite");
        fs::write(&active, b"active").expect("active generation");
        fs::write(&orphaned, b"orphaned").expect("orphaned generation");
        fs::write(dir.path().join("active-index"), "index-g2-active.sqlite\n")
            .expect("active manifest");
        let raw_lease =
            crate::store::GenerationReadLease::acquire(&orphaned).expect("raw generation lease");
        drop(raw_lease);
        fs::remove_file(&orphaned).expect("simulate generation removed before process exit");
        let orphan_before = fs::read_dir(dir.path())
            .expect("lease directory")
            .flatten()
            .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
            .filter(|name| {
                name.starts_with(".fossilsense-generation-")
                    && name != ".fossilsense-generation-leases.sqlite"
            })
            .collect::<Vec<_>>();
        assert_eq!(
            orphan_before.len(),
            1,
            "test setup must leave one valid hashed lease database"
        );

        cleanup_index_directory(dir.path(), SystemTime::now()).expect("later cleanup");

        let orphan_after = fs::read_dir(dir.path())
            .expect("lease directory")
            .flatten()
            .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
            .filter(|name| {
                name.starts_with(".fossilsense-generation-")
                    && name != ".fossilsense-generation-leases.sqlite"
            })
            .collect::<Vec<_>>();
        assert!(
            orphan_after.is_empty(),
            "later cleanup left orphan generation leases: {orphan_after:?}"
        );
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
        for generation in 1..=1_000 {
            let path = dir.path().join(format!("index-g{generation}-old.sqlite"));
            fs::write(path, b"old").unwrap();
        }
        let started = std::time::Instant::now();
        let removed = cleanup_index_directory(dir.path(), SystemTime::now()).expect("cleanup");
        let elapsed_us = started.elapsed().as_micros();
        println!("generation_cleanup_files: {removed}");
        println!("generation_cleanup_us: {elapsed_us}");
        assert_eq!(removed, 1_000);
        assert!(active.exists());
        assert!(!dir.path().join("index-g500-old.sqlite").exists());
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
