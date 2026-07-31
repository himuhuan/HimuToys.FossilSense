use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;

const OLD_REVISION_CLEANUP_GUARD: &str = "reject_old_revision_cleanup";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExplicitReplacementState {
    pub trigger_count: i64,
    pub revision_count: i64,
}

pub(crate) fn install_old_revision_cleanup_guard(database: &Path) -> Result<()> {
    let connection = Connection::open(database)
        .with_context(|| format!("failed to open explicit database {}", database.display()))?;
    connection
        .execute_batch(
            "CREATE TRIGGER reject_old_revision_cleanup
             BEFORE DELETE ON file_revisions
             BEGIN
                 SELECT RAISE(ABORT, 'old database cleanup must not run');
             END;",
        )
        .context("failed to install old-database cleanup guard")
}

pub(crate) fn inspect_explicit_replacement(database: &Path) -> Result<ExplicitReplacementState> {
    let connection = Connection::open(database)
        .with_context(|| format!("failed to inspect explicit database {}", database.display()))?;
    let trigger_count = connection.query_row(
        "SELECT COUNT(*)
         FROM sqlite_schema
         WHERE type = 'trigger' AND name = ?1",
        [OLD_REVISION_CLEANUP_GUARD],
        |row| row.get(0),
    )?;
    let revision_count =
        connection.query_row("SELECT COUNT(*) FROM file_revisions", [], |row| row.get(0))?;
    Ok(ExplicitReplacementState {
        trigger_count,
        revision_count,
    })
}

pub(crate) struct ExternalWalWriter {
    connection: Connection,
}

impl ExternalWalWriter {
    pub(crate) fn release(self) -> Result<()> {
        self.connection
            .execute_batch("ROLLBACK")
            .context("failed to release external WAL writer")
    }
}

pub(crate) fn hold_external_wal_writer(database: &Path) -> Result<ExternalWalWriter> {
    let connection = Connection::open(database)
        .with_context(|| format!("failed to open WAL blocker {}", database.display()))?;
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .context("failed to enable WAL for external writer")?;
    connection
        .execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE unpublished_external_write (value INTEGER);",
        )
        .context("failed to hold external WAL writer")?;
    Ok(ExternalWalWriter { connection })
}
