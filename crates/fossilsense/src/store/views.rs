use std::collections::HashMap;

use anyhow::Result;
#[cfg(test)]
use rusqlite::OptionalExtension;

use crate::reachability::OpenReason;

mod call_facts;
mod member;

#[allow(unused_imports)]
pub use call_facts::{CallCoverageRow, CallFactStoreView, CallSiteRow, CallableAnchorRow};
#[allow(unused_imports)]
pub use member::{MemberReadRow, MemberStoreView, RecordReadRow};

use super::{map_symbol_record, IndexStore, SymbolRecord, SELECT_SYMBOL_JOIN};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameTableSymbolRow {
    pub symbol_id: i64,
    pub id: i64,
    pub label: String,
    pub external: bool,
    pub path: String,
    pub kind: String,
    pub directly_included: bool,
}

/// Borrowed projection used by cold name-index construction. The callback may
/// intern the SQLite text directly, avoiding a workspace-sized temporary
/// `Vec<NameTableSymbolRow>` and its four owned strings per symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NameTableSymbolRef<'a> {
    pub symbol_id: i64,
    pub label: &'a str,
    pub external: bool,
    pub path: &'a str,
    pub kind: &'a str,
    pub directly_included: bool,
}

impl NameTableSymbolRow {
    #[allow(dead_code)]
    pub fn into_legacy_tuple(self) -> (i64, String, bool, String, String, bool) {
        (
            self.symbol_id,
            self.label,
            self.external,
            self.path,
            self.kind,
            self.directly_included,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncludeEdgeRow {
    pub source_path: String,
    pub target_path: String,
}

impl IncludeEdgeRow {
    #[allow(dead_code)]
    pub fn into_legacy_tuple(self) -> (String, String) {
        (self.source_path, self.target_path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct IncludeEdgeResolutionRow {
    pub source_path: String,
    pub target_path: String,
    pub resolution: String,
}

impl IncludeEdgeResolutionRow {
    #[allow(dead_code)]
    pub fn into_legacy_tuple(self) -> (String, String, String) {
        (self.source_path, self.target_path, self.resolution)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenIncludeRow {
    pub source_path: String,
    pub reason: OpenReason,
}

impl OpenIncludeRow {
    #[allow(dead_code)]
    pub fn into_legacy_tuple(self) -> (String, OpenReason) {
        (self.source_path, self.reason)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncludeCompletionPathRow {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceFileRow {
    pub path: String,
}

pub struct NameTableStoreView<'a> {
    store: &'a IndexStore,
}

impl<'a> NameTableStoreView<'a> {
    pub(super) fn new(store: &'a IndexStore) -> Self {
        Self { store }
    }

    pub fn symbol_rows(&self) -> Result<Vec<NameTableSymbolRow>> {
        let mut stmt = self.store.conn.prepare(
            "SELECT s.id, s.name, f.source, f.path, s.kind, f.directly_included FROM symbols s JOIN files f ON f.id = s.file_id \
             WHERE s.kind != 'field'",
        )?;
        let rows = stmt.query_map([], name_table_symbol_row)?;
        collect_rows(rows)
    }

    pub fn visit_symbol_rows<F>(&self, mut visitor: F) -> Result<usize>
    where
        F: for<'row> FnMut(NameTableSymbolRef<'row>) -> Result<()>,
    {
        let mut stmt = self.store.conn.prepare(
            "SELECT s.id, s.name, f.source, f.path, s.kind, f.directly_included FROM symbols s JOIN files f ON f.id = s.file_id \
             WHERE s.kind != 'field'",
        )?;
        let mut rows = stmt.query([])?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            let label = row.get_ref(1)?.as_str()?;
            let source = row.get_ref(2)?.as_str()?;
            let path = row.get_ref(3)?.as_str()?;
            let kind = row.get_ref(4)?.as_str()?;
            visitor(NameTableSymbolRef {
                symbol_id: row.get(0)?,
                label,
                external: source == "external",
                path,
                kind,
                directly_included: row.get(5)?,
            })?;
            count += 1;
        }
        Ok(count)
    }

    #[cfg(test)]
    pub fn largest_symbol_path(&self) -> Result<Option<(String, usize)>> {
        let row: Option<(String, i64)> = self
            .store
            .conn
            .query_row(
                "SELECT f.path, COUNT(*) AS symbol_count FROM symbols s JOIN files f ON f.id = s.file_id \
                 WHERE s.kind != 'field' GROUP BY f.id ORDER BY symbol_count DESC, f.path ASC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(anyhow::Error::from)?;
        Ok(row.map(|(path, count)| (path, count.max(0) as usize)))
    }

    pub fn symbol_rows_for_paths(&self, paths: &[String]) -> Result<Vec<NameTableSymbolRow>> {
        let mut names = Vec::new();
        if paths.is_empty() {
            return Ok(names);
        }

        for chunk in paths.chunks(400) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!(
                "SELECT s.id, s.name, f.source, f.path, s.kind, f.directly_included FROM symbols s JOIN files f ON f.id = s.file_id \
                 WHERE s.kind != 'field' AND f.path IN ({placeholders})"
            );
            let mut stmt = self.store.conn.prepare(&sql)?;
            let rows = stmt.query_map(
                rusqlite::params_from_iter(chunk.iter().map(String::as_str)),
                name_table_symbol_row,
            )?;
            for row in rows {
                names.push(row?);
            }
        }

        Ok(names)
    }

    #[allow(dead_code)]
    pub fn symbol_name_rows(&self) -> Result<Vec<(i64, String, bool)>> {
        self.symbol_rows().map(|rows| {
            rows.into_iter()
                .map(|row| (row.symbol_id, row.label, row.external))
                .collect()
        })
    }
}

pub struct ReachGraphStoreView<'a> {
    store: &'a IndexStore,
}

impl<'a> ReachGraphStoreView<'a> {
    pub(super) fn new(store: &'a IndexStore) -> Self {
        Self { store }
    }

    pub fn include_edges(&self) -> Result<Vec<IncludeEdgeRow>> {
        let mut stmt = self.store.conn.prepare(
            "SELECT sf.path, df.path FROM include_edges e \
             JOIN files sf ON sf.id = e.src_file_id \
             JOIN files df ON df.id = e.dst_file_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(IncludeEdgeRow {
                source_path: row.get(0)?,
                target_path: row.get(1)?,
            })
        })?;
        collect_rows(rows)
    }

    #[cfg(test)]
    pub fn include_edges_with_resolution(&self) -> Result<Vec<IncludeEdgeResolutionRow>> {
        let mut stmt = self.store.conn.prepare(
            "SELECT sf.path, df.path, e.resolution FROM include_edges e \
             JOIN files sf ON sf.id = e.src_file_id \
             JOIN files df ON df.id = e.dst_file_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(IncludeEdgeResolutionRow {
                source_path: row.get(0)?,
                target_path: row.get(1)?,
                resolution: row.get(2)?,
            })
        })?;
        collect_rows(rows)
    }

    pub fn unresolved_includes(&self) -> Result<Vec<OpenIncludeRow>> {
        self.open_include_rows("unresolved_includes", OpenReason::UnresolvedInclude)
    }

    pub fn ambiguous_includes(&self) -> Result<Vec<OpenIncludeRow>> {
        self.open_include_rows("ambiguous_includes", OpenReason::AmbiguousInclude)
    }

    pub fn include_data_for_sources(
        &self,
        source_paths: &[String],
    ) -> Result<(Vec<IncludeEdgeRow>, Vec<OpenIncludeRow>)> {
        if source_paths.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }

        let mut edges = Vec::new();
        let mut open = Vec::new();

        for chunk in source_paths.chunks(400) {
            let placeholders = vec!["?"; chunk.len()].join(",");

            let edge_sql = format!(
                "SELECT sf.path, df.path FROM include_edges e \
                 JOIN files sf ON sf.id = e.src_file_id \
                 JOIN files df ON df.id = e.dst_file_id \
                 WHERE sf.path IN ({placeholders})"
            );
            let mut stmt = self.store.conn.prepare(&edge_sql)?;
            let rows = stmt.query_map(
                rusqlite::params_from_iter(chunk.iter().map(String::as_str)),
                |row| {
                    Ok(IncludeEdgeRow {
                        source_path: row.get(0)?,
                        target_path: row.get(1)?,
                    })
                },
            )?;
            for row in rows {
                edges.push(row?);
            }

            let open_sql = format!(
                "SELECT path, unresolved_includes, ambiguous_includes FROM files \
                 WHERE path IN ({placeholders})"
            );
            let mut open_stmt = self.store.conn.prepare(&open_sql)?;
            let open_rows = open_stmt.query_map(
                rusqlite::params_from_iter(chunk.iter().map(String::as_str)),
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )?;
            for row in open_rows {
                let (source_path, unresolved_count, ambiguous_count) = row?;
                if unresolved_count > 0 {
                    open.push(OpenIncludeRow {
                        source_path,
                        reason: OpenReason::UnresolvedInclude,
                    });
                } else if ambiguous_count > 0 {
                    open.push(OpenIncludeRow {
                        source_path,
                        reason: OpenReason::AmbiguousInclude,
                    });
                }
            }
        }

        Ok((edges, open))
    }

    fn open_include_rows(&self, column: &str, reason: OpenReason) -> Result<Vec<OpenIncludeRow>> {
        let sql = format!("SELECT path FROM files WHERE {column} > 0");
        let mut stmt = self.store.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            Ok(OpenIncludeRow {
                source_path: row.get(0)?,
                reason,
            })
        })?;
        collect_rows(rows)
    }
}

pub struct IncludeTableStoreView<'a> {
    store: &'a IndexStore,
}

impl<'a> IncludeTableStoreView<'a> {
    pub(super) fn new(store: &'a IndexStore) -> Self {
        Self { store }
    }

    pub fn workspace_paths(&self) -> Result<Vec<IncludeCompletionPathRow>> {
        let mut stmt = self
            .store
            .conn
            .prepare("SELECT path FROM files WHERE source = 'workspace' ORDER BY path")?;
        let rows = stmt.query_map([], |row| Ok(IncludeCompletionPathRow { path: row.get(0)? }))?;
        collect_rows(rows)
    }

    pub fn workspace_file_paths(&self) -> Result<Vec<String>> {
        self.workspace_paths()
            .map(|rows| rows.into_iter().map(|row| row.path).collect())
    }

    pub fn workspace_files_by_suffix(&self, rel: &str) -> Result<Vec<String>> {
        let like = format!(
            "%/{}",
            rel.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let mut stmt = self.store.conn.prepare(
            "SELECT path FROM files WHERE source = 'workspace' \
             AND (path = ?1 OR path LIKE ?2 ESCAPE '\\')",
        )?;
        let rows = stmt.query_map(rusqlite::params![rel, like], |row| row.get::<_, String>(0))?;
        collect_rows(rows)
    }

    #[allow(dead_code)]
    pub fn include_edges(&self) -> Result<Vec<IncludeEdgeRow>> {
        self.store.reach_graph_view().include_edges()
    }
}

pub struct SymbolReadView<'a> {
    store: &'a IndexStore,
}

impl<'a> SymbolReadView<'a> {
    pub(super) fn new(store: &'a IndexStore) -> Self {
        Self { store }
    }

    pub fn symbols_by_ids(&self, ids: &[i64]) -> Result<Vec<SymbolRecord>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        let mut id_to_record: HashMap<i64, SymbolRecord> = HashMap::new();
        for chunk in ids.chunks(400) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!("{SELECT_SYMBOL_JOIN} WHERE s.id IN ({placeholders})");
            let mut stmt = self.store.conn.prepare(&sql)?;
            let rows = stmt.query_map(
                rusqlite::params_from_iter(chunk.iter().copied()),
                map_symbol_record,
            )?;
            for row in rows {
                let record = row?;
                id_to_record.insert(record.id, record);
            }
        }

        Ok(ids
            .iter()
            .filter_map(|id| id_to_record.get(id).cloned())
            .collect())
    }

    pub fn symbols_by_name(&self, name: &str) -> Result<Vec<SymbolRecord>> {
        let mut stmt = self
            .store
            .conn
            .prepare(&format!("{SELECT_SYMBOL_JOIN} WHERE s.name = ?1"))?;
        let rows = stmt.query_map([name], map_symbol_record)?;
        collect_rows(rows)
    }
}

pub struct ReferenceFileStoreView<'a> {
    store: &'a IndexStore,
}

impl<'a> ReferenceFileStoreView<'a> {
    pub(super) fn new(store: &'a IndexStore) -> Self {
        Self { store }
    }

    pub fn indexed_workspace_files(&self) -> Result<Vec<ReferenceFileRow>> {
        self.store
            .include_table_view()
            .workspace_paths()
            .map(|rows| {
                rows.into_iter()
                    .map(|row| ReferenceFileRow { path: row.path })
                    .collect()
            })
    }

    pub fn indexed_workspace_files_for_paths(
        &self,
        paths: &[String],
    ) -> Result<Vec<ReferenceFileRow>> {
        let mut files = Vec::new();
        for chunk in paths.chunks(400) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!(
                "SELECT path FROM files WHERE source = 'workspace' AND path IN ({placeholders}) ORDER BY path"
            );
            let mut stmt = self.store.conn.prepare(&sql)?;
            let rows = stmt.query_map(
                rusqlite::params_from_iter(chunk.iter().map(String::as_str)),
                |row| Ok(ReferenceFileRow { path: row.get(0)? }),
            )?;
            files.extend(collect_rows(rows)?);
        }
        Ok(files)
    }
}

fn name_table_symbol_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<NameTableSymbolRow> {
    let id = row.get(0)?;
    let source: String = row.get(2)?;
    Ok(NameTableSymbolRow {
        symbol_id: id,
        id,
        label: row.get(1)?,
        external: source == "external",
        path: row.get(3)?,
        kind: row.get(4)?,
        directly_included: row.get::<_, i64>(5)? != 0,
    })
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>> {
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}
