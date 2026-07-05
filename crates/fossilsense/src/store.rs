use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};

use crate::parser::{FileSemanticIndex, SymbolKind, SymbolRole};

mod includes;
mod queries;
mod schema;
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

pub enum FileIndexPayload<'a> {
    Ok(&'a FileSemanticIndex),
    Error(&'a str),
}

pub struct FileIndexUpdate<'a> {
    pub fingerprint: &'a FileFingerprint,
    pub source: FileSource,
    pub payload: FileIndexPayload<'a>,
}

pub struct IndexStore {
    conn: Connection,
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

        let store = Self { conn };
        store.migrate(workspace_root)?;
        Ok(store)
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
        Ok(Self { conn })
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
    pub fn workspace_files_by_suffix(&self, rel: &str) -> Result<Vec<String>> {
        let like = format!(
            "%/{}",
            rel.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let mut stmt = self.conn.prepare(
            "SELECT path FROM files WHERE source = 'workspace' \
             AND (path = ?1 OR path LIKE ?2 ESCAPE '\\')",
        )?;
        let rows = stmt.query_map(params![rel, like], |row| row.get::<_, String>(0))?;
        let mut paths = Vec::new();
        for row in rows {
            paths.push(row?);
        }
        Ok(paths)
    }

    /// All indexed workspace file paths, used by degraded include completion to
    /// surface headers that live below common include roots.
    pub fn workspace_file_paths(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path FROM files WHERE source = 'workspace' ORDER BY path")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut paths = Vec::new();
        for row in rows {
            paths.push(row?);
        }
        Ok(paths)
    }

    /// Indexed workspace files as relative paths, excluding external include
    /// files. Used by reference search discovery to avoid walking the
    /// workspace tree on each request.
    pub fn indexed_workspace_files(&self) -> Result<Vec<String>> {
        self.workspace_file_paths()
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
                "SELECT id, size, mtime_ns, hash FROM files WHERE path = ?1",
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
                "SELECT path, id, size, mtime_ns, hash FROM files WHERE path IN ({placeholders})"
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

    /// Workspace-source convenience wrapper, used by tests and any caller that
    /// does not distinguish sources.
    #[allow(dead_code)]
    pub fn upsert_file_index(
        &mut self,
        fingerprint: &FileFingerprint,
        index: &FileSemanticIndex,
    ) -> Result<()> {
        self.upsert_file_index_with_source(fingerprint, index, FileSource::Workspace)
    }

    pub fn upsert_file_index_with_source(
        &mut self,
        fingerprint: &FileFingerprint,
        index: &FileSemanticIndex,
        source: FileSource,
    ) -> Result<()> {
        self.apply_file_updates(&[FileIndexUpdate {
            fingerprint,
            source,
            payload: FileIndexPayload::Ok(index),
        }])
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
        self.apply_file_updates_inner(updates, true)
    }

    pub fn apply_fresh_file_updates(&mut self, updates: &[FileIndexUpdate<'_>]) -> Result<()> {
        self.apply_file_updates_inner(updates, false)
    }

    fn apply_file_updates_inner(
        &mut self,
        updates: &[FileIndexUpdate<'_>],
        delete_existing_rows: bool,
    ) -> Result<()> {
        writes::apply_file_updates_inner(&mut self.conn, updates, delete_existing_rows)
    }

    pub fn begin_full_rebuild_load(&mut self) -> Result<()> {
        self.drop_lookup_indexes()?;
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM type_aliases", [])?;
        tx.execute("DELETE FROM members", [])?;
        tx.execute("DELETE FROM record_defs", [])?;
        tx.execute("DELETE FROM include_edges", [])?;
        tx.execute("DELETE FROM includes", [])?;
        tx.execute("DELETE FROM symbols", [])?;
        tx.execute("DELETE FROM files", [])?;
        tx.commit()?;
        Ok(())
    }

    pub fn finish_full_rebuild_load(&self) -> Result<()> {
        self.create_lookup_indexes()?;
        // Truncate the WAL after bulk load to control disk footprint.
        // Dirty updates do NOT run this checkpoint.
        self.conn
            .pragma_update(None, "wal_checkpoint", "TRUNCATE")?;
        Ok(())
    }

    pub fn delete_missing_files(&mut self, seen_paths: &HashSet<String>) -> Result<usize> {
        if seen_paths.is_empty() {
            // Nothing seen → delete all files. Use a fast path.
            let deleted: i64 = self
                .conn
                .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))?;
            let tx = self.conn.transaction()?;
            tx.execute("DELETE FROM files", [])?;
            tx.commit()?;
            return Ok(deleted as usize);
        }

        let tx = self.conn.transaction()?;
        // Stage seen paths in a temp table for anti-join delete.
        tx.execute_batch("CREATE TEMP TABLE seen_paths (path TEXT PRIMARY KEY);")?;
        {
            let mut ins = tx.prepare("INSERT OR IGNORE INTO seen_paths (path) VALUES (?1)")?;
            for path in seen_paths {
                ins.execute([path.as_str()])?;
            }
        }
        let deleted = tx.execute(
            "DELETE FROM files WHERE path NOT IN (SELECT path FROM seen_paths)",
            [],
        )?;
        tx.execute_batch("DROP TABLE IF EXISTS seen_paths;")?;
        tx.commit()?;
        Ok(deleted)
    }

    #[allow(dead_code)]
    pub fn delete_file(&mut self, path: &str) -> Result<usize> {
        self.conn
            .execute("DELETE FROM files WHERE path = ?1", [path])
            .context("failed to delete indexed file")
    }

    pub fn symbol_count(&self) -> Result<usize> {
        self.conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |row| {
                row.get::<_, i64>(0)
            })
            .map(|count| count as usize)
            .context("failed to count symbols")
    }

    fn migrate(&self, workspace_root: &Path) -> Result<()> {
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
        if let Some(version) = stored_version {
            if version != schema::SCHEMA_VERSION {
                self.conn.execute_batch(schema::DROP_DATA_TABLES_SQL)?;
            }
        }

        self.conn.execute_batch(schema::CREATE_SCHEMA_SQL)?;
        self.create_lookup_indexes()?;

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
        Ok(())
    }

    fn drop_lookup_indexes(&self) -> Result<()> {
        self.conn.execute_batch(schema::DROP_LOOKUP_INDEXES_SQL)?;
        Ok(())
    }

    fn create_lookup_indexes(&self) -> Result<()> {
        self.conn.execute_batch(schema::CREATE_LOOKUP_INDEXES_SQL)?;
        Ok(())
    }
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

fn record_kind_to_str(k: crate::parser::RecordKind) -> &'static str {
    match k {
        crate::parser::RecordKind::Struct => "struct",
        crate::parser::RecordKind::Union => "union",
        crate::parser::RecordKind::Class => "class",
    }
}

fn record_kind_from_str(s: &str) -> Option<crate::parser::RecordKind> {
    match s {
        "struct" => Some(crate::parser::RecordKind::Struct),
        "union" => Some(crate::parser::RecordKind::Union),
        "class" => Some(crate::parser::RecordKind::Class),
        _ => None,
    }
}

fn record_confidence_to_str(c: crate::parser::RecordConfidence) -> &'static str {
    match c {
        crate::parser::RecordConfidence::NamedTag => "named_tag",
        crate::parser::RecordConfidence::AnonymousTypedef => "anonymous_typedef",
        crate::parser::RecordConfidence::Heuristic => "heuristic",
    }
}

fn member_kind_to_str(k: crate::parser::MemberKind) -> &'static str {
    k.as_str()
}

fn member_confidence_to_str(c: crate::parser::MemberConfidence) -> &'static str {
    c.as_str()
}

fn symbol_role(role: SymbolRole) -> &'static str {
    match role {
        SymbolRole::Definition => "definition",
        SymbolRole::Declaration => "declaration",
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
