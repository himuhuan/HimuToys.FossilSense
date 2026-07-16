use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension};

use crate::semantic_model::{
    MemberConfidence, MemberKind, PersistentFacts, RecordConfidence, RecordKind, SymbolKind,
    SymbolRole, PARSER_FACT_VERSION,
};

mod generations;
mod includes;
mod queries;
mod schema;
pub mod views;
mod writes;

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

/// A symbol joined with its containing file path, ready for query responses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolRecord {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub role: String,
    pub path: String,
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
    pub signature: String,
    pub guard: Option<String>,
    /// `"workspace"` or `"external"` — drives workspace-before-external ranking.
    pub source: String,
    /// True when this is an external file directly `#include`d by a workspace
    /// file (first layer). Used by goto-definition to label first-layer external
    /// candidates; always `false` for workspace files.
    pub directly_included: bool,
}

const SELECT_SYMBOL_JOIN: &str = "SELECT s.id, s.name, s.kind, s.role, f.path, \
     s.start_line, s.start_col, s.end_line, s.end_col, s.signature, s.guard, f.source, \
     f.directly_included \
     FROM symbols s JOIN files f ON f.id = s.file_id";

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

impl IndexStore {
    pub fn open(path: &Path, workspace_root: &Path) -> Result<Self> {
        Self::open_with_call_indexes(path, workspace_root, true)
    }

    /// Open a full-build destination without maintaining the large call-fact
    /// secondary indexes while facts are inserted. The destination must not be
    /// visible to request readers until [`finalize_full_build_indexes`] returns.
    pub fn open_for_full_rebuild(path: &Path, workspace_root: &Path) -> Result<Self> {
        let new_database = !path.exists();
        let mut store = Self::open_with_call_indexes(path, workspace_root, false)?;
        if new_database {
            store.bulk_call_string_ids = Some(HashMap::new());
        } else {
            store
                .conn
                .execute_batch(schema::CREATE_CALL_STRING_INDEX_SQL)?;
        }
        Ok(store)
    }

    fn open_with_call_indexes(
        path: &Path,
        workspace_root: &Path,
        create_call_indexes: bool,
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

        let store = Self {
            conn,
            legacy_full_build: None,
            bulk_call_string_ids: None,
        };
        store.migrate(workspace_root, create_call_indexes)?;
        if !create_call_indexes {
            store
                .conn
                .execute_batch(schema::DROP_CALL_LOOKUP_INDEXES_SQL)?;
        }
        Ok(store)
    }

    pub fn finalize_full_build_indexes(&mut self) -> Result<()> {
        self.bulk_call_string_ids.take();
        self.conn
            .execute_batch(schema::CREATE_CALL_LOOKUP_INDEXES_SQL)?;
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

    /// Reachability-scoped variant of [`kind_counts_by_names`]: only definitions
    /// whose containing file path is in `scope` are counted. With `scope = None`
    /// this falls back to the unscoped `workspace OR directly_included` behavior.
    /// Retired from production alongside [`kind_counts_by_names`]; kept as the
    /// parity oracle for `query::NameTable::colorable_kind_counts`.
    #[cfg(test)]
    pub fn kind_counts_by_names_scoped(
        &self,
        names: &[&str],
        scope: Option<&HashSet<String>>,
    ) -> Result<HashMap<String, HashMap<String, usize>>> {
        let Some(scope) = scope else {
            return self.kind_counts_by_names(names);
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
                "SELECT s.name, s.kind, COUNT(*) FROM symbols s \
                 JOIN files f ON f.id = s.file_id \
                 JOIN reach_scope r ON r.path = f.path \
                 WHERE s.role = 'definition' AND s.name IN ({placeholders}) \
                 GROUP BY s.name, s.kind"
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

    /// Count of indexed symbols belonging to external files (test/diagnostic).
    #[allow(dead_code)]
    pub fn external_symbol_count(&self) -> Result<usize> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM symbols s JOIN files f ON f.id = s.file_id \
                 WHERE f.source = 'external'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count as usize)
            .context("failed to count external symbols")
    }

    #[allow(dead_code)]
    pub fn stored_file(&self, path: &str) -> Result<Option<StoredFile>> {
        self.conn
            .query_row(
                "SELECT f.id, r.size, r.mtime_ns, r.hash FROM files f
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
                "SELECT f.path, f.id, r.size, r.mtime_ns, r.hash FROM files f
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

    pub fn symbol_count(&self) -> Result<usize> {
        self.conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |row| {
                row.get::<_, i64>(0)
            })
            .map(|count| count as usize)
            .context("failed to count symbols")
    }

    fn migrate(&self, workspace_root: &Path, create_call_indexes: bool) -> Result<()> {
        // Ensure the meta table exists, then drop the data tables when the stored
        // schema version differs so the next index pass repopulates with the new
        // shape (e.g. the `container` column / `type_aliases` table).
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            )",
            [],
        )?;
        let stored_version: Option<i64> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .and_then(|value| value.parse().ok());
        let schema_mismatch =
            stored_version.is_some_and(|version| version != schema::SCHEMA_VERSION);
        let parser_mismatch = stored_version == Some(schema::SCHEMA_VERSION)
            && !parser_facts_are_current(&self.conn)?;
        if schema_mismatch || parser_mismatch {
            for name in [
                "call_sites",
                "callable_anchors",
                "type_aliases",
                "members",
                "record_defs",
                "includes",
                "symbols",
                "files",
            ] {
                let object_type: Option<String> = self
                    .conn
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
                    self.conn.execute_batch(&statement)?;
                }
            }
            self.conn.execute_batch(schema::DROP_DATA_TABLES_SQL)?;
        }

        self.conn.execute_batch(schema::CREATE_SCHEMA_SQL)?;
        self.create_lookup_indexes()?;
        if create_call_indexes {
            self.create_call_lookup_indexes()?;
        }

        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [schema::SCHEMA_VERSION.to_string()],
        )?;
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES ('workspace_root', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [workspace_root.display().to_string()],
        )?;
        self.conn.execute(
            "INSERT OR IGNORE INTO meta (key, value) VALUES ('semantic_generation', '0')",
            [],
        )?;
        Ok(())
    }

    fn create_lookup_indexes(&self) -> Result<()> {
        self.conn.execute_batch(schema::CREATE_LOOKUP_INDEXES_SQL)?;
        Ok(())
    }

    fn create_call_lookup_indexes(&self) -> Result<()> {
        self.conn
            .execute_batch(schema::CREATE_CALL_LOOKUP_INDEXES_SQL)?;
        Ok(())
    }

    pub fn name_table_view(&self) -> views::NameTableStoreView<'_> {
        views::NameTableStoreView::new(self)
    }

    pub fn reach_graph_view(&self) -> views::ReachGraphStoreView<'_> {
        views::ReachGraphStoreView::new(self)
    }

    pub fn include_table_view(&self) -> views::IncludeTableStoreView<'_> {
        views::IncludeTableStoreView::new(self)
    }

    pub fn symbol_read_view(&self) -> views::SymbolReadView<'_> {
        views::SymbolReadView::new(self)
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

fn map_symbol_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<SymbolRecord> {
    Ok(SymbolRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        kind: row.get(2)?,
        role: row.get(3)?,
        path: row.get(4)?,
        start_line: row.get::<_, i64>(5)? as u32,
        start_col: row.get::<_, i64>(6)? as u32,
        end_line: row.get::<_, i64>(7)? as u32,
        end_col: row.get::<_, i64>(8)? as u32,
        signature: row.get(9)?,
        guard: row.get(10)?,
        source: row.get(11)?,
        directly_included: row.get::<_, i64>(12)? != 0,
    })
}

fn symbol_kind(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Function => "function",
        SymbolKind::Macro => "macro",
        SymbolKind::Type => "type",
        SymbolKind::EnumConstant => "enum_constant",
        SymbolKind::GlobalVariable => "global_variable",
        SymbolKind::Field => "field",
    }
}

fn record_kind_to_str(k: RecordKind) -> &'static str {
    match k {
        RecordKind::Struct => "struct",
        RecordKind::Union => "union",
        RecordKind::Class => "class",
    }
}

fn record_kind_from_str(s: &str) -> Option<RecordKind> {
    match s {
        "struct" => Some(RecordKind::Struct),
        "union" => Some(RecordKind::Union),
        "class" => Some(RecordKind::Class),
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

fn symbol_role(role: SymbolRole) -> &'static str {
    match role {
        SymbolRole::Definition => "definition",
        SymbolRole::Declaration => "declaration",
        SymbolRole::TentativeDefinition => "tentative_definition",
        SymbolRole::UnknownDeclarationOrDefinition => "unknown_declaration_or_definition",
    }
}

fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
