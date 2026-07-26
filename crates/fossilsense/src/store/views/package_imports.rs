#![cfg_attr(not(test), allow(dead_code))]

use anyhow::Result;
use rusqlite::OptionalExtension;

use crate::call_model::{SourcePosition, SourceRange};

use super::IndexStore;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageReadRow {
    pub id: i64,
    pub path: String,
    pub name: String,
    pub name_range: SourceRange,
    pub build_guard: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReadRow {
    pub id: i64,
    pub source_path: String,
    pub import_path: String,
    pub alias: Option<String>,
    pub path_range: SourceRange,
    pub declaration_range: SourceRange,
}

pub struct PackageImportStoreView<'a> {
    store: &'a IndexStore,
}

impl<'a> PackageImportStoreView<'a> {
    pub(super) fn new(store: &'a IndexStore) -> Self {
        Self { store }
    }

    pub fn package_for_path(&self, path: &str) -> Result<Option<PackageReadRow>> {
        let mut stmt = self.store.conn.prepare(
            "SELECT p.id, f.path, p.name,
                    p.name_start_byte, p.name_end_byte,
                    p.name_start_line, p.name_start_col, p.name_end_line, p.name_end_col,
                    rev.build_guard
             FROM packages p
             JOIN files f ON f.id = p.file_id
             JOIN file_revisions rev ON rev.id = p.revision_id
             WHERE f.path = ?1
             LIMIT 1",
        )?;
        let mut rows = stmt.query([path])?;
        Ok(rows.next()?.map(package_row).transpose()?)
    }

    #[allow(dead_code)] // Reserved for bounded package-name selector evidence.
    pub fn packages_named(&self, name: &str, limit: usize) -> Result<(Vec<PackageReadRow>, bool)> {
        if limit == 0 {
            return Ok((Vec::new(), false));
        }
        let mut stmt = self.store.conn.prepare(
            "SELECT p.id, f.path, p.name,
                    p.name_start_byte, p.name_end_byte,
                    p.name_start_line, p.name_start_col, p.name_end_line, p.name_end_col,
                    rev.build_guard
             FROM packages p
             JOIN files f ON f.id = p.file_id
             JOIN file_revisions rev ON rev.id = p.revision_id
             WHERE p.name = ?1
             ORDER BY f.path, p.id
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![
                name,
                i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX)
            ],
            package_row,
        )?;
        truncate_rows(rows, limit)
    }

    pub fn build_guard_for_path(&self, path: &str) -> Result<Option<String>> {
        Ok(self
            .store
            .conn
            .query_row(
                "SELECT rev.build_guard
                 FROM files f
                 JOIN active_file_revisions active ON active.file_id = f.id
                 JOIN file_revisions rev ON rev.id = active.revision_id
                 WHERE f.path = ?1",
                [path],
                |row| row.get(0),
            )
            .optional()?
            .flatten())
    }

    pub fn imports_for_path(&self, path: &str, limit: usize) -> Result<(Vec<ImportReadRow>, bool)> {
        if limit == 0 {
            return Ok((Vec::new(), false));
        }
        let mut stmt = self.store.conn.prepare(
            "SELECT i.id, f.path, i.import_path, i.alias,
                    i.path_start_byte, i.path_end_byte,
                    i.path_start_line, i.path_start_col, i.path_end_line, i.path_end_col,
                    i.declaration_start_byte, i.declaration_end_byte,
                    i.declaration_start_line, i.declaration_start_col,
                    i.declaration_end_line, i.declaration_end_col
             FROM imports i
             JOIN files f ON f.id = i.file_id
             WHERE f.path = ?1
             ORDER BY i.declaration_start_byte, i.id
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![
                path,
                i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX)
            ],
            import_row,
        )?;
        truncate_rows(rows, limit)
    }
}

impl IndexStore {
    pub fn package_import_view(&self) -> PackageImportStoreView<'_> {
        PackageImportStoreView::new(self)
    }
}

fn package_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PackageReadRow> {
    Ok(PackageReadRow {
        id: row.get(0)?,
        path: row.get(1)?,
        name: row.get(2)?,
        name_range: source_range(row, 3)?,
        build_guard: row.get(9)?,
    })
}

fn import_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ImportReadRow> {
    Ok(ImportReadRow {
        id: row.get(0)?,
        source_path: row.get(1)?,
        import_path: row.get(2)?,
        alias: row.get(3)?,
        path_range: source_range(row, 4)?,
        declaration_range: source_range(row, 10)?,
    })
}

fn source_range(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<SourceRange> {
    Ok(SourceRange {
        start_byte: row.get::<_, i64>(offset)? as usize,
        end_byte: row.get::<_, i64>(offset + 1)? as usize,
        start: SourcePosition {
            line: row.get::<_, i64>(offset + 2)? as u32,
            character: row.get::<_, i64>(offset + 3)? as u32,
        },
        end: SourcePosition {
            line: row.get::<_, i64>(offset + 4)? as u32,
            character: row.get::<_, i64>(offset + 5)? as u32,
        },
    })
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>> {
    let mut output = Vec::new();
    for row in rows {
        output.push(row?);
    }
    Ok(output)
}

fn truncate_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
    limit: usize,
) -> Result<(Vec<T>, bool)> {
    let mut output = collect_rows(rows)?;
    let truncated = output.len() > limit;
    output.truncate(limit);
    Ok((output, truncated))
}
