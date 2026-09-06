use crate::semantic_model::{CoverageGap, DeclarationCoverageSummary};
use crate::store::IndexStore;
use anyhow::Result;
use rusqlite::{params, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCoverageReadRow {
    pub path: String,
    pub revision_id: i64,
    pub content_hash: String,
    pub summary: DeclarationCoverageSummary,
}

pub struct DeclarationCoverageStoreView<'a> {
    store: &'a IndexStore,
}

impl IndexStore {
    pub fn coverage_view(&self) -> DeclarationCoverageStoreView<'_> {
        DeclarationCoverageStoreView { store: self }
    }
}

fn read_summary(
    row: &rusqlite::Row<'_>,
    column: usize,
) -> rusqlite::Result<DeclarationCoverageSummary> {
    match row.get::<_, Option<String>>(column)? {
        None => Ok(DeclarationCoverageSummary::default()),
        Some(json) => serde_json::from_str(&json).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                column,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        }),
    }
}

impl DeclarationCoverageStoreView<'_> {
    pub fn for_path(&self, path: &str) -> Result<Option<FileCoverageReadRow>> {
        Ok(self.store.conn.query_row("SELECT f.path, r.id, r.hash, r.coverage_summary FROM file_entries f
            JOIN active_file_revisions a ON a.file_id=f.id JOIN file_revisions r ON r.id=a.revision_id
            WHERE f.path=?1", [path], |row| Ok(FileCoverageReadRow {
                path: row.get(0)?, revision_id: row.get(1)?, content_hash: row.get(2)?, summary: read_summary(row, 3)?,
            })).optional()?)
    }

    /// Semantic detail requests only. Ordinary completion consumes its parse
    /// summary in memory and never calls this SQLite boundary.
    pub fn for_paths(&self, paths: &[String]) -> Result<Vec<FileCoverageReadRow>> {
        anyhow::ensure!(
            paths.len() <= 512,
            "coverage query exceeds 512 file summaries"
        );
        let mut result = Vec::new();
        for chunk in paths.chunks(400) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let mut statement = self.store.conn.prepare(&format!("SELECT f.path,r.id,r.hash,r.coverage_summary FROM file_entries f
                JOIN active_file_revisions a ON a.file_id=f.id JOIN file_revisions r ON r.id=a.revision_id
                WHERE f.path IN ({placeholders}) LIMIT 512"))?;
            let rows = statement.query_map(rusqlite::params_from_iter(chunk), |row| {
                Ok(FileCoverageReadRow {
                    path: row.get(0)?,
                    revision_id: row.get(1)?,
                    content_hash: row.get(2)?,
                    summary: read_summary(row, 3)?,
                })
            })?;
            result.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
        }
        Ok(result)
    }

    pub fn gaps_for_revision(
        &self,
        revision_id: i64,
        limit: usize,
    ) -> Result<(Vec<CoverageGap>, bool)> {
        let limit = limit.min(1024);
        let mut statement = self.store.conn.prepare(
            "SELECT payload FROM declaration_coverage_gaps
            WHERE revision_id=?1 ORDER BY start_byte,end_byte,id LIMIT ?2",
        )?;
        let rows = statement.query_map(params![revision_id, (limit + 1) as i64], |row| {
            let json: String = row.get(0)?;
            serde_json::from_str(&json).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })?;
        let mut gaps = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        let truncated = gaps.len() > limit;
        gaps.truncate(limit);
        Ok((gaps, truncated))
    }
}
