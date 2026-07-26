use anyhow::Result;

use crate::includes::ResolutionKind;
use crate::reachability::OpenReason;

mod call_facts;
mod declarations;
mod go_package_graph;
mod member;
mod package_imports;

#[allow(unused_imports)]
pub use call_facts::{CallCoverageRow, CallFactStoreView, CallSiteRow, CallableAnchorRow};
#[allow(unused_imports)]
pub use declarations::{
    DeclarationNameRef, DeclarationNameRow, DeclarationReadRow, DeclarationStoreView,
};
#[allow(unused_imports)]
pub use go_package_graph::{
    GoImportablePackageRow, GoOpenPackageRow, GoPackageEdgeRow, GoPackageFileRow,
    GoPackageGraphStoreView, GoPackageResolution,
};
#[allow(unused_imports)]
pub use member::{MemberReadRow, MemberStoreView, RecordReadRow, TypeAliasReadRow};
#[allow(unused_imports)]
pub use package_imports::{ImportReadRow, PackageImportStoreView, PackageReadRow};

use super::IndexStore;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncludeEdgeRow {
    pub source_path: String,
    pub target_path: String,
    pub resolution: ResolutionKind,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackCompletionRow {
    pub id: i64,
    pub name: String,
    pub kind_hint: i64,
    pub path: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
    pub detail: Option<String>,
    pub semantic_family: crate::semantic_model::SemanticFamily,
}

pub struct FallbackCompletionStoreView<'a> {
    store: &'a IndexStore,
}

impl<'a> FallbackCompletionStoreView<'a> {
    pub(super) fn new(store: &'a IndexStore) -> Self {
        Self { store }
    }

    pub fn all(&self) -> Result<Vec<FallbackCompletionRow>> {
        let mut stmt = self.store.conn.prepare(
            "SELECT c.id, c.name, c.kind_hint, f.path, c.start_byte, c.end_byte,
                    c.start_line, c.start_col, c.end_line, c.end_col, c.detail,
                    rev.language
             FROM fallback_completions c
             JOIN files f ON f.id = c.file_id
             JOIN file_revisions rev ON rev.id = c.revision_id
             ORDER BY lower(c.name), c.name, f.path, c.start_byte, c.id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(FallbackCompletionRow {
                id: row.get(0)?,
                name: row.get(1)?,
                kind_hint: row.get(2)?,
                path: row.get(3)?,
                start_byte: row.get::<_, i64>(4)? as usize,
                end_byte: row.get::<_, i64>(5)? as usize,
                start_line: row.get::<_, i64>(6)? as u32,
                start_col: row.get::<_, i64>(7)? as u32,
                end_line: row.get::<_, i64>(8)? as u32,
                end_col: row.get::<_, i64>(9)? as u32,
                detail: row.get(10)?,
                semantic_family: semantic_family_from_language_code(row.get(11)?)?,
            })
        })?;
        collect_rows(rows)
    }
}

fn semantic_family_from_language_code(
    value: i64,
) -> rusqlite::Result<crate::semantic_model::SemanticFamily> {
    match value {
        0 | 1 | 2 => Ok(crate::semantic_model::SemanticFamily::CFamily),
        3 => Ok(crate::semantic_model::SemanticFamily::Go),
        _ => Err(rusqlite::Error::IntegralValueOutOfRange(11, value)),
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
            "SELECT sf.path, df.path, e.resolution FROM include_edges e \
             JOIN files sf ON sf.id = e.src_file_id \
             JOIN files df ON df.id = e.dst_file_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(IncludeEdgeRow {
                source_path: row.get(0)?,
                target_path: row.get(1)?,
                resolution: ResolutionKind::from_str(&row.get::<_, String>(2)?),
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
                "SELECT sf.path, df.path, e.resolution FROM include_edges e \
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
                        resolution: ResolutionKind::from_str(&row.get::<_, String>(2)?),
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

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>> {
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}
