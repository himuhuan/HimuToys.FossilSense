use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension, TransactionBehavior};

use crate::semantic_model::{
    MemberConfidence, MemberKind, PersistentFacts, RecordConfidence, RecordKind,
    PARSER_FACT_VERSION,
};

mod generation_lease;
mod generations;
mod go_package_graph;
mod includes;
mod queries;
mod schema;
pub mod views;
mod writes;

pub(crate) use generation_lease::{
    GenerationCleanupLease, GenerationPublicationLease, GenerationReadLease,
};

/// Whether an indexed file belongs to the workspace or to an external include
/// reference directory. Stored on `files.source`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileSource {
    Workspace,
    External,
}

impl FileSource {
    pub fn as_str(self) -> &'static str {
        match self {
            FileSource::Workspace => "workspace",
            FileSource::External => "external",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFingerprint {
    pub path: String,
    pub extension: String,
    pub size: u64,
    pub mtime_ns: i64,
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredFile {
    pub id: i64,
    pub size: u64,
    pub mtime_ns: i64,
    pub hash: String,
    pub language_code: i64,
}

#[derive(Debug, Clone, Copy)]
pub struct PersistenceDiagnostics {
    pub fact_mask: u8,
    pub parse_error_count: usize,
    pub fallback_used: bool,
}

pub trait PersistableFileIndex: Sync {
    fn persistent_facts(&self) -> PersistentFacts<'_>;
    fn persistence_diagnostics(&self) -> PersistenceDiagnostics;
}

pub enum FileIndexPayload<'a> {
    Ok(&'a dyn PersistableFileIndex),
    Error(&'a str),
}

pub struct FileIndexUpdate<'a> {
    pub fingerprint: &'a FileFingerprint,
    pub source: FileSource,
    pub payload: FileIndexPayload<'a>,
}

pub struct IndexStore {
    conn: Connection,
    legacy_full_build: Option<IndexBuild>,
    bulk_call_string_ids: Option<HashMap<String, i64>>,
    maintenance_blocked: bool,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MigrationFailpoint {
    AfterDeferredIndexDrop,
    AbortAfterDestructiveDrop,
}

#[cfg(test)]
thread_local! {
    static MIGRATION_FAILPOINT: std::cell::Cell<Option<MigrationFailpoint>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn take_migration_failpoint(expected: MigrationFailpoint) -> bool {
    MIGRATION_FAILPOINT.with(|slot| {
        if slot.get() == Some(expected) {
            slot.set(None);
            true
        } else {
            false
        }
    })
}

/// Cross-process writer lock for one logical index destination.
///
/// Every FossilSense writer holds an exclusive transaction on a small, stable
/// sibling lock database. Default indexes use the generation family's stable
/// `index.sqlite` fallback path as their logical destination; explicit
/// `--db` indexes use the requested destination. SQLite releases the
/// transaction after normal exit or process death, avoiding a stale PID-file
/// protocol. The lock database itself intentionally remains so no
/// close/delete/open race can split cooperating writers across two inodes.
pub struct IndexWriterLock {
    _connection: Connection,
    destination: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplicitIndexSnapshot {
    destination: PathBuf,
    generation: u64,
    state: ExplicitTargetState,
    files: SqliteFamilyIdentity,
}

impl ExplicitIndexSnapshot {
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExplicitTargetState {
    Missing,
    Database,
    ReplaceableCorrupt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SqliteFamilyIdentity {
    main: Option<FileIdentity>,
    wal: Option<FileIdentity>,
    shm: Option<FileIdentity>,
    journal: Option<FileIdentity>,
}

impl SqliteFamilyIdentity {
    fn has_sidecars(&self) -> bool {
        self.wal.is_some() || self.shm.is_some() || self.journal.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileIdentity {
    len: u64,
    modified: Option<SystemTime>,
    volume_or_device: u64,
    file_index_or_inode: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexBuild {
    pub id: i64,
    pub target_generation: u64,
    pub full_rebuild: bool,
}

/// Outcome of publishing one staged index generation.
///
/// Once `generation` is returned, the active manifest has already been
/// committed and must be treated as published. Post-publication cleanup is a
/// best-effort maintenance step: its failure is surfaced separately so callers
/// can warn without reporting the committed generation as failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexCommitOutcome {
    pub generation: u64,
    pub cleanup_warning: Option<String>,
}

#[derive(Debug, Default)]
pub struct IncludeGraphUpdate {
    pub source_ids: Vec<i64>,
    pub edges: Vec<(i64, i64, String)>,
    pub unresolved: Vec<(i64, i64)>,
    pub ambiguous: Vec<(i64, i64)>,
    pub clear_all: bool,
    pub go_package_edges: Vec<(String, String, String)>,
    pub go_open_packages: Vec<(String, String)>,
    pub go_importable_packages: Vec<(String, String)>,
    pub clear_all_go_packages: bool,
}

pub struct SemanticReadGuard<'a> {
    store: &'a IndexStore,
    generation: u64,
    active: bool,
}

impl<'a> SemanticReadGuard<'a> {
    #[allow(dead_code)] // Relation read views consume this in the next implementation stage.
    pub fn store(&self) -> &'a IndexStore {
        self.store
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn finish(mut self) -> Result<()> {
        self.store.conn.execute_batch("COMMIT")?;
        self.active = false;
        Ok(())
    }
}

impl Drop for SemanticReadGuard<'_> {
    fn drop(&mut self) {
        if self.active {
            let _ = self.store.conn.execute_batch("ROLLBACK");
        }
    }
}

/// Extract normalized include metadata from raw target text. Malformed or
/// macro-constructed targets produce `("unknown", "", "")` so dirty invalidation
/// gracefully skips them without error.
fn include_normalized_metadata(target_text: &str) -> (&'static str, String, String) {
    let Some((form, normalized)) = crate::includes::normalize_include_target(target_text) else {
        return ("unknown", String::new(), String::new());
    };
    let form_str = match form {
        crate::includes::IncludeForm::Quote => "quote",
        crate::includes::IncludeForm::Angle => "angle",
    };
    let basename = normalized
        .rsplit('/')
        .next()
        .unwrap_or(&normalized)
        .to_string();
    (form_str, normalized, basename)
}

impl IndexWriterLock {
    pub fn acquire(destination: &Path) -> Result<Self> {
        let destination = normalized_index_destination(destination)?;
        let lock_path = index_writer_lock_path(&destination);
        let connection = Connection::open(&lock_path)
            .with_context(|| format!("failed to open index writer lock {}", lock_path.display()))?;
        connection.busy_timeout(Duration::from_millis(250))?;
        let journal_mode: String =
            connection.query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))?;
        anyhow::ensure!(
            journal_mode.eq_ignore_ascii_case("delete"),
            "index writer lock kept unexpected journal mode {journal_mode}"
        );
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS writer_lock (
                 singleton INTEGER PRIMARY KEY CHECK (singleton = 1)
             );
             BEGIN EXCLUSIVE;",
            )
            .with_context(|| {
                format!(
                    "index destination {} is locked by another FossilSense writer",
                    destination.display()
                )
            })?;
        Ok(Self {
            _connection: connection,
            destination,
        })
    }

    pub fn destination_path(&self) -> &Path {
        &self.destination
    }

    pub fn capture_replacement_snapshot(&self) -> Result<ExplicitIndexSnapshot> {
        // Opening a clean WAL database can remove an empty leftover sidecar
        // when the connection closes. Retry once so that benign normalization
        // can settle, while continuous changes still fail closed.
        for attempt in 0..2 {
            let before = sqlite_family_identity(&self.destination)?;
            let (state, generation) = read_explicit_target_state(&self.destination, &before)?;
            let after = sqlite_family_identity(&self.destination)?;
            if before == after {
                return Ok(ExplicitIndexSnapshot {
                    destination: self.destination.clone(),
                    generation,
                    state,
                    files: after,
                });
            }
            if attempt == 1 {
                anyhow::bail!(
                    "explicit index target changed while its publication snapshot was captured: {}",
                    self.destination.display()
                );
            }
        }
        unreachable!("bounded snapshot retry returns or errors")
    }

    pub fn prepare_for_atomic_replacement(&self, snapshot: &ExplicitIndexSnapshot) -> Result<()> {
        anyhow::ensure!(
            snapshot.destination == self.destination,
            "explicit index snapshot belongs to a different destination"
        );
        let before = sqlite_family_identity(&self.destination)?;
        anyhow::ensure!(
            before == snapshot.files,
            "explicit index target changed during side-by-side build: {}",
            self.destination.display()
        );
        let (state, generation) = read_explicit_target_state(&self.destination, &before)?;
        let after_read = sqlite_family_identity(&self.destination)?;
        anyhow::ensure!(
            after_read == before,
            "explicit index target changed while publication was revalidated: {}",
            self.destination.display()
        );
        anyhow::ensure!(
            state == snapshot.state && generation == snapshot.generation,
            "explicit index target generation changed during side-by-side build: expected {}, observed {}",
            snapshot.generation,
            generation
        );

        match state {
            ExplicitTargetState::Missing => {}
            ExplicitTargetState::ReplaceableCorrupt => {
                anyhow::ensure!(
                    !before.has_sidecars(),
                    "refusing to replace a corrupt SQLite target with sidecars: {}",
                    self.destination.display()
                );
            }
            ExplicitTargetState::Database => {
                drain_sqlite_wal(&self.destination)?;
            }
        }
        ensure_sqlite_sidecars_absent(&self.destination)?;
        Ok(())
    }
}

impl IndexStore {
    pub fn open(path: &Path, workspace_root: &Path) -> Result<Self> {
        Self::open_with_deferred_indexes(path, workspace_root, true)
    }

    /// Open a full-build destination without maintaining fact lookup indexes
    /// while rows are inserted. The destination
    /// must not be visible to request readers until
    /// [`finalize_full_build_indexes`] returns.
    pub fn open_for_full_rebuild(path: &Path, workspace_root: &Path) -> Result<Self> {
        let new_database = !path.exists();
        let mut store = Self::open_with_deferred_indexes(path, workspace_root, false)?;
        // A full rebuild writes an unpublished database from start to finish.
        // Replaying the growing WAL into the main file every ~1,000 pages makes
        // bulk insertion highly sensitive to storage latency. Use an in-memory
        // rollback journal and an exclusive connection for this disposable
        // build target; normal/index-incremental opens restore WAL + NORMAL.
        // The completed side-by-side database is validated before publication.
        store.conn.pragma_update(None, "journal_mode", "MEMORY")?;
        store.conn.pragma_update(None, "synchronous", "OFF")?;
        store
            .conn
            .pragma_update(None, "locking_mode", "EXCLUSIVE")?;
        store.conn.pragma_update(None, "temp_store", "MEMORY")?;
        store.conn.pragma_update(None, "cache_size", -32_768)?;
        store.conn.pragma_update(None, "wal_autocheckpoint", 0)?;
        if new_database {
            store.bulk_call_string_ids = Some(HashMap::new());
        } else {
            store
                .conn
                .execute_batch(schema::CREATE_CALL_STRING_INDEX_SQL)?;
        }
        Ok(store)
    }

    fn open_with_deferred_indexes(
        path: &Path,
        workspace_root: &Path,
        create_deferred_indexes: bool,
    ) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create index directory {}", parent.display())
            })?;
        }

        let conn = Connection::open(path)
            .with_context(|| format!("failed to open SQLite index {}", path.display()))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;

        let mut store = Self {
            conn,
            legacy_full_build: None,
            bulk_call_string_ids: None,
            maintenance_blocked: false,
        };
        store.migrate(workspace_root, create_deferred_indexes)?;
        Ok(store)
    }

    #[cfg(test)]
    pub(crate) fn set_migration_failpoint_for_test(failpoint: MigrationFailpoint) {
        MIGRATION_FAILPOINT.with(|slot| {
            assert!(
                slot.replace(Some(failpoint)).is_none(),
                "migration failpoint already installed on this test thread"
            );
        });
    }

    pub fn finalize_full_build_indexes(&mut self) -> Result<()> {
        self.bulk_call_string_ids.take();
        self.conn.execute_batch(schema::CREATE_LOOKUP_INDEXES_SQL)?;
        self.conn
            .execute_batch(schema::CREATE_DEFERRED_LOOKUP_INDEXES_SQL)?;
        self.conn.execute_batch(
            "ANALYZE callable_anchor_facts;
             ANALYZE call_site_facts;
             PRAGMA optimize;",
        )?;
        Ok(())
    }

    /// Validate and checkpoint a side-by-side database before its file name can
    /// become visible through the active manifest.
    pub fn prepare_full_build_publication(&self) -> Result<()> {
        self.validate_full_build()?;
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }

    fn validate_full_build(&self) -> Result<()> {
        let check: String = self
            .conn
            .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
        anyhow::ensure!(check == "ok", "SQLite quick_check failed: {check}");
        let mut foreign_key_check = self.conn.prepare("PRAGMA foreign_key_check")?;
        anyhow::ensure!(
            !foreign_key_check.exists([])?,
            "SQLite foreign_key_check reported a violation"
        );
        drop(foreign_key_check);
        Ok(())
    }

    /// Finish an explicit-path bulk build whose database does not need the
    /// side-by-side manifest validation step. Full publication calls
    /// [`Self::prepare_full_build_publication`] instead.
    pub fn checkpoint_full_rebuild(&self) -> Result<()> {
        self.validate_full_build()?;
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }

    /// Open an existing index for read-only queries (no schema migration).
    ///
    /// The connection is opened read-write (without create) so it can read a
    /// WAL database even when no writer is currently attached; callers only
    /// issue SELECTs. Returns an error if the file does not exist.
    pub fn open_readonly(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("failed to open SQLite index {}", path.display()))?;
        Ok(Self {
            conn,
            legacy_full_build: None,
            bulk_call_string_ids: None,
            maintenance_blocked: false,
        })
    }

    pub fn has_current_schema(path: &Path) -> Result<bool> {
        let store = Self::open_readonly(path)?;
        let version: Option<i64> = store
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .and_then(|value| value.parse().ok());
        if version != Some(schema::SCHEMA_VERSION) {
            return Ok(false);
        }
        parser_facts_are_current(&store.conn)
    }

    /// Execute one durable read inside a SQLite snapshot pinned to the semantic
    /// generation captured by the request's engine snapshot.
    ///
    /// A generation mismatch is deliberately an error: returning rows from a
    /// newer active manifest together with an older in-memory reach/name model
    /// would expose a mixed request generation. Callers may recapture a newer
    /// request snapshot and retry, but must not silently drop the check.
    pub fn read_at_generation<T>(
        path: &Path,
        expected_generation: u64,
        read: impl FnOnce(&IndexStore) -> Result<T>,
    ) -> Result<T> {
        let store = Self::open_readonly(path)?;
        let guard = store.begin_semantic_read(Some(expected_generation))?;
        let value = read(guard.store())?;
        guard.finish()?;
        Ok(value)
    }

    /// Reachability-scoped variant of [`declaration_kind_counts_by_names`]: only definitions
    /// whose containing file path is in `scope` are counted. With `scope = None`
    /// this falls back to the unscoped `workspace OR directly_included` behavior.
    /// Retired from production; kept as the
    /// parity oracle for `query::NameTable::colorable_kind_counts`.
    #[cfg(test)]
    pub fn declaration_kind_counts_by_names_scoped(
        &self,
        names: &[&str],
        scope: Option<&HashSet<String>>,
    ) -> Result<HashMap<String, HashMap<String, usize>>> {
        let Some(scope) = scope else {
            return self.declaration_kind_counts_by_names(names);
        };
        let mut counts: HashMap<String, HashMap<String, usize>> = HashMap::new();
        if names.is_empty() {
            return Ok(counts);
        }

        // Stage the reachable file paths in a temp table so the count query is a
        // plain join — avoids a second giant IN-list alongside the name chunks.
        self.conn.execute_batch(
            "DROP TABLE IF EXISTS reach_scope; \
             CREATE TEMP TABLE reach_scope (path TEXT PRIMARY KEY);",
        )?;
        {
            let mut ins = self
                .conn
                .prepare("INSERT OR IGNORE INTO reach_scope (path) VALUES (?1)")?;
            for path in scope {
                ins.execute([path])?;
            }
        }

        for chunk in names.chunks(400) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!(
                "SELECT d.name,
                        CASE d.declaration_kind
                            WHEN 0 THEN 'function'
                            WHEN 1 THEN 'function'
                            WHEN 2 THEN 'global_variable'
                            WHEN 3 THEN 'type'
                            WHEN 4 THEN 'type'
                            WHEN 5 THEN 'enum_constant'
                            WHEN 6 THEN 'macro'
                        END,
                        COUNT(*)
                 FROM declarations d \
                 JOIN file_entries f ON f.id = d.file_id \
                 JOIN reach_scope r ON r.path = f.path \
                 WHERE d.role = 1 AND d.name IN ({placeholders}) \
                 GROUP BY d.name, d.declaration_kind"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows =
                stmt.query_map(rusqlite::params_from_iter(chunk.iter().copied()), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)? as usize,
                    ))
                })?;
            for row in rows {
                let (name, kind, count) = row?;
                counts.entry(name).or_default().insert(kind, count);
            }
        }

        self.conn
            .execute_batch("DROP TABLE IF EXISTS reach_scope;")?;
        Ok(counts)
    }

    /// Workspace files whose path equals `rel` or ends with `/rel` — the
    /// degraded "workspace headers" fallback for include-target resolution.
    #[allow(dead_code)]
    pub fn workspace_files_by_suffix(&self, rel: &str) -> Result<Vec<String>> {
        self.include_table_view().workspace_files_by_suffix(rel)
    }

    /// All indexed workspace file paths, used by degraded include completion to
    /// surface headers that live below common include roots.
    #[allow(dead_code)]
    pub fn workspace_file_paths(&self) -> Result<Vec<String>> {
        self.include_table_view().workspace_file_paths()
    }

    /// Indexed workspace files as relative paths, excluding external include
    /// files. Used by reference search discovery to avoid walking the
    /// workspace tree on each request.
    #[allow(dead_code)]
    pub fn indexed_workspace_files(&self) -> Result<Vec<String>> {
        self.reference_file_view()
            .indexed_workspace_files()
            .map(|rows| rows.into_iter().map(|row| row.path).collect())
    }

    /// Count of canonical declarations belonging to external files (test/diagnostic).
    #[allow(dead_code)]
    pub fn external_declaration_count(&self) -> Result<usize> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM declarations d \
                 JOIN file_revisions r ON r.id = d.revision_id \
                 WHERE r.source = 'external'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count as usize)
            .context("failed to count external declarations")
    }

    #[allow(dead_code)]
    pub fn stored_file(&self, path: &str) -> Result<Option<StoredFile>> {
        self.conn
            .query_row(
                "SELECT f.id, r.size, r.mtime_ns, r.hash, r.language FROM files f
                 JOIN active_file_revisions a ON a.file_id = f.id
                 JOIN file_revisions r ON r.id = a.revision_id
                 WHERE f.path = ?1",
                [path],
                |row| {
                    Ok(StoredFile {
                        id: row.get(0)?,
                        size: row.get::<_, i64>(1)? as u64,
                        mtime_ns: row.get(2)?,
                        hash: row.get(3)?,
                        language_code: row.get(4)?,
                    })
                },
            )
            .optional()
            .context("failed to load stored file metadata")
    }

    pub fn stored_files(&self, paths: &[String]) -> Result<HashMap<String, StoredFile>> {
        let mut files = HashMap::new();
        if paths.is_empty() {
            return Ok(files);
        }

        for chunk in paths.chunks(400) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!(
                "SELECT f.path, f.id, r.size, r.mtime_ns, r.hash, r.language FROM files f
                 JOIN active_file_revisions a ON a.file_id = f.id
                 JOIN file_revisions r ON r.id = a.revision_id
                 WHERE f.path IN ({placeholders})"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(
                rusqlite::params_from_iter(chunk.iter().map(String::as_str)),
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        StoredFile {
                            id: row.get(1)?,
                            size: row.get::<_, i64>(2)? as u64,
                            mtime_ns: row.get(3)?,
                            hash: row.get(4)?,
                            language_code: row.get(5)?,
                        },
                    ))
                },
            )?;
            for row in rows {
                let (path, stored) = row?;
                files.insert(path, stored);
            }
        }

        Ok(files)
    }

    #[allow(dead_code)]
    pub fn mark_file_error(&mut self, fingerprint: &FileFingerprint, error: &str) -> Result<()> {
        self.mark_file_error_with_source(fingerprint, error, FileSource::Workspace)
    }

    pub fn mark_file_error_with_source(
        &mut self,
        fingerprint: &FileFingerprint,
        error: &str,
        source: FileSource,
    ) -> Result<()> {
        self.apply_file_updates(&[FileIndexUpdate {
            fingerprint,
            source,
            payload: FileIndexPayload::Error(error),
        }])
    }

    pub fn apply_file_updates(&mut self, updates: &[FileIndexUpdate<'_>]) -> Result<()> {
        if let Some(build) = self.legacy_full_build {
            return self.stage_file_updates(build, updates);
        }
        let build = self.begin_index_build(false)?;
        self.stage_file_updates(build, updates)?;
        self.commit_index_build(build, &IncludeGraphUpdate::default())?;
        Ok(())
    }

    pub fn stage_file_updates(
        &mut self,
        build: IndexBuild,
        updates: &[FileIndexUpdate<'_>],
    ) -> Result<()> {
        writes::stage_file_updates(
            &mut self.conn,
            build,
            updates,
            self.bulk_call_string_ids.as_mut(),
        )
    }

    #[allow(dead_code)]
    pub fn begin_full_rebuild_load(&mut self) -> Result<()> {
        self.legacy_full_build = Some(self.begin_index_build(true)?);
        Ok(())
    }

    #[allow(dead_code)]
    pub fn finish_full_rebuild_load(&mut self) -> Result<()> {
        if let Some(build) = self.legacy_full_build.take() {
            self.commit_index_build(
                build,
                &IncludeGraphUpdate {
                    clear_all: true,
                    ..Default::default()
                },
            )?;
        }
        // Truncate the WAL after bulk load to control disk footprint.
        self.conn
            .pragma_update(None, "wal_checkpoint", "TRUNCATE")?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn delete_missing_files(&mut self, seen_paths: &HashSet<String>) -> Result<usize> {
        let build = self.begin_index_build(false)?;
        let deleted = self.stage_delete_missing_files(build, seen_paths)?;
        self.commit_index_build(build, &IncludeGraphUpdate::default())?;
        Ok(deleted)
    }

    #[allow(dead_code)]
    pub fn delete_file(&mut self, path: &str) -> Result<usize> {
        let build = self.begin_index_build(false)?;
        let deleted = self.stage_delete_file(build, path)?;
        self.commit_index_build(build, &IncludeGraphUpdate::default())?;
        Ok(deleted)
    }

    pub fn declaration_count(&self) -> Result<usize> {
        self.conn
            .query_row("SELECT COUNT(*) FROM declarations", [], |row| {
                row.get::<_, i64>(0)
            })
            .map(|count| count as usize)
            .context("failed to count declarations")
    }

    #[cfg(test)]
    pub fn declarations_by_name(&self, name: &str) -> Result<Vec<views::DeclarationReadRow>> {
        self.declaration_view().by_name(name)
    }

    #[cfg(test)]
    pub fn declarations_by_ids(&self, ids: &[i64]) -> Result<Vec<views::DeclarationReadRow>> {
        self.declaration_view().by_ids(ids)
    }

    #[cfg(test)]
    pub fn declaration_name_rows(&self) -> Result<Vec<views::DeclarationNameRow>> {
        self.declaration_view().all_name_rows()
    }

    fn migrate(&mut self, workspace_root: &Path, create_deferred_indexes: bool) -> Result<()> {
        // Ensure the meta table exists, then drop the data tables when the stored
        // schema version differs so the next index pass repopulates with the new
        // shape (e.g. the `container` column / `type_aliases` table).
        //
        // SQLite DDL is transactional. Keep the stored version, every destructive
        // drop, the replacement schema and the published metadata in one write
        // transaction so an error or process exit cannot expose a half-migrated
        // explicit database.
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            )",
            [],
        )?;
        let stored_version_text: Option<String> = transaction
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let stored_version = stored_version_text
            .as_deref()
            .map(|value| {
                value
                    .parse::<i64>()
                    .with_context(|| format!("invalid stored schema version {value:?}"))
            })
            .transpose()?;
        let schema_mismatch =
            stored_version.is_some_and(|version| version != schema::SCHEMA_VERSION);
        let parser_mismatch = stored_version == Some(schema::SCHEMA_VERSION)
            && !parser_facts_are_current(&transaction)?;
        if schema_mismatch || parser_mismatch {
            for name in [
                "call_sites",
                "callable_anchors",
                "type_aliases",
                "members",
                "record_defs",
                "imports",
                "packages",
                "includes",
                "fallback_completions",
                "symbols",
                "files",
            ] {
                let object_type: Option<String> = transaction
                    .query_row(
                        "SELECT type FROM sqlite_master WHERE name = ?1",
                        [name],
                        |row| row.get(0),
                    )
                    .optional()?;
                if let Some(object_type) = object_type {
                    let statement = match object_type.as_str() {
                        "view" => format!("DROP VIEW IF EXISTS {name}"),
                        _ => format!("DROP TABLE IF EXISTS {name}"),
                    };
                    transaction.execute_batch(&statement)?;
                }
            }
            transaction.execute_batch(schema::DROP_DATA_TABLES_SQL)?;
            #[cfg(test)]
            if take_migration_failpoint(MigrationFailpoint::AbortAfterDestructiveDrop) {
                let marker_path = std::env::var_os("FOSSILSENSE_TEST_MIGRATION_CRASH_MARKER")
                    .expect("migration crash marker path");
                let mut marker =
                    std::fs::File::create(marker_path).expect("create migration crash marker");
                std::io::Write::write_all(&mut marker, b"destructive-drop-complete\n")
                    .expect("write migration crash marker");
                marker.sync_all().expect("flush migration crash marker");
                drop(marker);
                std::process::abort();
            }
        }

        transaction.execute_batch(schema::CREATE_SCHEMA_SQL)?;
        if create_deferred_indexes {
            transaction.execute_batch(schema::CREATE_LOOKUP_INDEXES_SQL)?;
            transaction.execute_batch(schema::CREATE_DEFERRED_LOOKUP_INDEXES_SQL)?;
        } else {
            transaction.execute_batch(schema::DROP_LOOKUP_INDEXES_SQL)?;
            transaction.execute_batch(schema::DROP_DEFERRED_LOOKUP_INDEXES_SQL)?;
            transaction.execute_batch(schema::CREATE_FULL_BUILD_MAINTENANCE_INDEXES_SQL)?;
            #[cfg(test)]
            if take_migration_failpoint(MigrationFailpoint::AfterDeferredIndexDrop) {
                anyhow::bail!("injected failure after deferred index drop");
            }
        }

        transaction.execute(
            "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [schema::SCHEMA_VERSION.to_string()],
        )?;
        transaction.execute(
            "INSERT INTO meta (key, value) VALUES ('workspace_root', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [workspace_root.display().to_string()],
        )?;
        transaction.execute(
            "INSERT OR IGNORE INTO meta (key, value) VALUES ('semantic_generation', '0')",
            [],
        )?;
        if stored_version == Some(schema::SCHEMA_VERSION) && !schema_mismatch && !parser_mismatch {
            // Current-schema databases created before cleanup debt became durable
            // receive one conservative audit. INSERT OR IGNORE preserves the
            // state written by newer commits and failed cleanup attempts.
            transaction.execute(
                "INSERT OR IGNORE INTO meta (key, value)
                 VALUES ('cleanup_required', '1')",
                [],
            )?;
        } else {
            // A new or schema-reset database cannot contain inactive revisions.
            // Overwrite a stale marker left in meta when migration dropped all
            // generation-owned data.
            transaction.execute(
                "INSERT INTO meta (key, value) VALUES ('cleanup_required', '0')
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn reach_graph_view(&self) -> views::ReachGraphStoreView<'_> {
        views::ReachGraphStoreView::new(self)
    }

    pub fn include_table_view(&self) -> views::IncludeTableStoreView<'_> {
        views::IncludeTableStoreView::new(self)
    }

    pub fn fallback_completion_view(&self) -> views::FallbackCompletionStoreView<'_> {
        views::FallbackCompletionStoreView::new(self)
    }

    pub fn reference_file_view(&self) -> views::ReferenceFileStoreView<'_> {
        views::ReferenceFileStoreView::new(self)
    }

    pub fn member_view(&self) -> views::MemberStoreView<'_> {
        views::MemberStoreView::new(self)
    }

    pub fn call_fact_view(&self) -> views::CallFactStoreView<'_> {
        views::CallFactStoreView::new(self)
    }
}

fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    sidecar.into()
}

fn normalized_index_destination(destination: &Path) -> Result<PathBuf> {
    let file_name = destination
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("explicit index destination has no file name"))?;
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
    let directory = directory.canonicalize().with_context(|| {
        format!(
            "failed to canonicalize explicit index directory {}",
            directory.display()
        )
    })?;
    let candidate = directory.join(file_name);
    match fs::symlink_metadata(&candidate) {
        Ok(_) => candidate.canonicalize().with_context(|| {
            format!(
                "failed to resolve explicit index destination {}",
                candidate.display()
            )
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(candidate),
        Err(error) => Err(error).with_context(|| {
            format!(
                "failed to inspect explicit index destination {}",
                candidate.display()
            )
        }),
    }
}

fn index_writer_lock_path(destination: &Path) -> PathBuf {
    let mut hasher = blake3::Hasher::new();
    #[cfg(windows)]
    {
        hasher.update(
            destination
                .as_os_str()
                .to_string_lossy()
                .to_lowercase()
                .as_bytes(),
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        hasher.update(destination.as_os_str().as_bytes());
    }
    #[cfg(not(any(windows, unix)))]
    {
        hasher.update(destination.as_os_str().to_string_lossy().as_bytes());
    }
    let digest = hasher.finalize().to_hex();
    destination
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".fossilsense-index-{}.lock", &digest[..24]))
}

fn sqlite_family_identity(path: &Path) -> Result<SqliteFamilyIdentity> {
    let identity = SqliteFamilyIdentity {
        main: optional_file_identity(path, true)?,
        wal: optional_file_identity(&sqlite_sidecar_path(path, "-wal"), true)?,
        // Reader slots and locks mutate shared-memory contents without changing
        // committed database state. Track the SHM inode and size, not its mtime.
        shm: optional_file_identity(&sqlite_sidecar_path(path, "-shm"), false)?,
        journal: optional_file_identity(&sqlite_sidecar_path(path, "-journal"), true)?,
    };
    anyhow::ensure!(
        identity.main.is_some() || !identity.has_sidecars(),
        "SQLite sidecar exists without its main database: {}",
        path.display()
    );
    Ok(identity)
}

fn optional_file_identity(path: &Path, track_modified: bool) -> Result<Option<FileIdentity>> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect SQLite file {}", path.display()));
        }
    };
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect SQLite file {}", path.display()))?;
    anyhow::ensure!(
        metadata.is_file(),
        "SQLite path is not a regular file: {}",
        path.display()
    );
    let (volume_or_device, file_index_or_inode) = platform_file_identity(&file, &metadata)?;
    Ok(Some(FileIdentity {
        len: metadata.len(),
        modified: track_modified.then(|| metadata.modified().ok()).flatten(),
        volume_or_device,
        file_index_or_inode,
    }))
}

#[cfg(windows)]
fn platform_file_identity(file: &fs::File, _metadata: &fs::Metadata) -> Result<(u64, u64)> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    let succeeded = unsafe {
        GetFileInformationByHandle(file.as_raw_handle() as _, &mut information as *mut _)
    };
    if succeeded == 0 {
        return Err(std::io::Error::last_os_error()).context("failed to read SQLite file identity");
    }
    let file_index =
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow);
    Ok((u64::from(information.dwVolumeSerialNumber), file_index))
}

#[cfg(unix)]
fn platform_file_identity(_file: &fs::File, metadata: &fs::Metadata) -> Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(not(any(windows, unix)))]
fn platform_file_identity(_file: &fs::File, _metadata: &fs::Metadata) -> Result<(u64, u64)> {
    Ok((0, 0))
}

fn read_explicit_target_state(
    path: &Path,
    identity: &SqliteFamilyIdentity,
) -> Result<(ExplicitTargetState, u64)> {
    if identity.main.is_none() {
        return Ok((ExplicitTargetState::Missing, 0));
    }
    let generation = (|| -> rusqlite::Result<Option<String>> {
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let has_meta: bool = connection.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'meta'
             )",
            [],
            |row| row.get(0),
        )?;
        if !has_meta {
            return Ok(None);
        }
        connection
            .query_row(
                "SELECT value FROM meta WHERE key = 'semantic_generation'",
                [],
                |row| row.get(0),
            )
            .optional()
    })();
    match generation {
        Ok(value) => {
            let generation = value
                .map(|value| {
                    value.parse::<u64>().with_context(|| {
                        format!(
                            "invalid semantic generation in explicit index {}",
                            path.display()
                        )
                    })
                })
                .transpose()?
                .unwrap_or(0);
            Ok((ExplicitTargetState::Database, generation))
        }
        Err(rusqlite::Error::SqliteFailure(failure, _))
            if matches!(
                failure.code,
                ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase
            ) && !identity.has_sidecars() =>
        {
            Ok((ExplicitTargetState::ReplaceableCorrupt, 0))
        }
        Err(error) => Err(error).with_context(|| {
            format!(
                "failed to read semantic generation from explicit index {}",
                path.display()
            )
        }),
    }
}

fn drain_sqlite_wal(path: &Path) -> Result<()> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| {
        format!(
            "failed to open explicit SQLite database {} for replacement",
            path.display()
        )
    })?;
    connection.busy_timeout(Duration::from_secs(5))?;
    let journal_mode: String =
        connection.query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))?;
    anyhow::ensure!(
        journal_mode.eq_ignore_ascii_case("delete"),
        "failed to leave WAL mode before replacing {}: SQLite kept journal mode {journal_mode}",
        path.display()
    );
    drop(connection);
    ensure_sqlite_sidecars_absent(path)
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

pub(crate) fn semantic_language_storage_code(
    language: crate::semantic_model::SemanticLanguage,
) -> i64 {
    writes::semantic_language_code(language)
}

/// A schema can stay structurally current while the parser fact contract moves
/// forward. Every active revision must therefore carry the named fact version;
/// otherwise both the side-by-side index lifecycle and direct store opening
/// treat the database as a rebuild source, never as rows suitable for dual-read.
fn parser_facts_are_current(conn: &Connection) -> Result<bool> {
    let required_tables: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name IN ('active_file_revisions', 'file_revisions')",
        [],
        |row| row.get(0),
    )?;
    if required_tables != 2 {
        return Ok(false);
    }

    let stale_active_revisions: i64 = conn.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM active_file_revisions active
             LEFT JOIN file_revisions revision ON revision.id = active.revision_id
             WHERE revision.id IS NULL OR revision.parser_version <> ?1
         )",
        [PARSER_FACT_VERSION],
        |row| row.get(0),
    )?;
    Ok(stale_active_revisions == 0)
}

fn record_kind_to_str(k: RecordKind) -> &'static str {
    match k {
        RecordKind::Struct => "struct",
        RecordKind::Union => "union",
        RecordKind::Class => "class",
        RecordKind::Interface => "interface",
    }
}

fn record_kind_from_str(s: &str) -> Option<RecordKind> {
    match s {
        "struct" => Some(RecordKind::Struct),
        "union" => Some(RecordKind::Union),
        "class" => Some(RecordKind::Class),
        "interface" => Some(RecordKind::Interface),
        _ => None,
    }
}

fn record_confidence_to_str(c: RecordConfidence) -> &'static str {
    match c {
        RecordConfidence::NamedTag => "named_tag",
        RecordConfidence::AnonymousTypedef => "anonymous_typedef",
        RecordConfidence::Heuristic => "heuristic",
    }
}

fn member_kind_to_str(k: MemberKind) -> &'static str {
    k.as_str()
}

fn member_confidence_to_str(c: MemberConfidence) -> &'static str {
    c.as_str()
}

fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
