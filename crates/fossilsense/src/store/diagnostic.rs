//! An explicitly read-only, owned SQLite snapshot for bounded CLI diagnostics.
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Serialize;

use super::{schema, IndexStore};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DiagnosticIndexMetadata {
    pub schema_version: Option<i64>,
    pub generation: Option<u64>,
    pub workspace_root: Option<String>,
    pub compatible: bool,
}

pub(crate) struct DiagnosticReadSnapshot {
    pub store: IndexStore,
    pub metadata: DiagnosticIndexMetadata,
}

impl std::fmt::Debug for DiagnosticReadSnapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DiagnosticReadSnapshot")
            .field("metadata", &self.metadata)
            .finish_non_exhaustive()
    }
}

impl DiagnosticReadSnapshot {
    pub fn open(path: &Path) -> Result<Self> {
        // No CREATE, migration, WAL checkpoint, generation lease or cleanup.
        // Do not use immutable=1: committed WAL frames must remain visible.
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("failed to open diagnostic index {}", path.display()))?;
        conn.execute_batch("BEGIN DEFERRED")?;
        let store = IndexStore {
            conn,
            legacy_full_build: None,
            bulk_call_string_ids: None,
            maintenance_blocked: true,
        };
        // This SELECT establishes the snapshot before any later stage reads.
        let metadata = store.diagnostic_metadata()?;
        Ok(Self { store, metadata })
    }
}

impl Drop for DiagnosticReadSnapshot {
    fn drop(&mut self) {
        let _ = self.store.conn.execute_batch("ROLLBACK");
    }
}

impl IndexStore {
    pub(crate) fn diagnostic_metadata(&self) -> Result<DiagnosticIndexMetadata> {
        let has_meta: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='meta')",
            [],
            |row| row.get(0),
        )?;
        let value = |key: &str| -> Result<Option<String>> {
            if !has_meta {
                return Ok(None);
            }
            Ok(self
                .conn
                .query_row(
                    "SELECT value FROM meta WHERE key=?1 LIMIT 1",
                    [key],
                    |row| row.get(0),
                )
                .optional()?)
        };
        let schema_version = value("schema_version")?
            .map(|v| v.parse::<i64>())
            .transpose()
            .context("invalid index schema version")?;
        let generation = value("semantic_generation")?
            .map(|v| v.parse::<u64>())
            .transpose()
            .context("invalid index generation")?;
        Ok(DiagnosticIndexMetadata {
            schema_version,
            generation,
            workspace_root: value("workspace_root")?,
            compatible: schema_version == Some(schema::SCHEMA_VERSION) && generation.is_some(),
        })
    }

    /// Only inspect versions for the bounded request's paths, never scan all revisions.
    pub(crate) fn diagnostic_parser_versions(
        &self,
        paths: &[String],
    ) -> Result<Vec<(String, i64)>> {
        anyhow::ensure!(
            paths.len() <= 257,
            "diagnostic revision path limit exceeded"
        );
        let mut result = Vec::new();
        let mut statement = self.conn.prepare("SELECT r.parser_version FROM file_entries f
            JOIN active_file_revisions a ON a.file_id=f.id JOIN file_revisions r ON r.id=a.revision_id
            WHERE f.path=?1 LIMIT 1")?;
        for path in paths {
            if let Some(version) = statement.query_row([path], |row| row.get(0)).optional()? {
                result.push((path.clone(), version));
            }
        }
        Ok(result)
    }
}
