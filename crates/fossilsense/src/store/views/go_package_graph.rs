use anyhow::Result;

use crate::reachability::OpenReason;
use crate::store::IndexStore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoPackageResolution {
    Exact,
    Heuristic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoPackageFileRow {
    pub package_key: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoPackageEdgeRow {
    pub source_package_key: String,
    pub target_package_key: String,
    pub resolution: GoPackageResolution,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoOpenPackageRow {
    pub package_key: String,
    pub reason: OpenReason,
}

pub struct GoPackageGraphStoreView<'a> {
    store: &'a IndexStore,
}

impl<'a> GoPackageGraphStoreView<'a> {
    pub(super) fn new(store: &'a IndexStore) -> Self {
        Self { store }
    }

    pub fn package_files(&self) -> Result<Vec<GoPackageFileRow>> {
        let mut stmt = self.store.conn.prepare(
            "SELECT f.path, p.name
             FROM packages p
             JOIN files f ON f.id = p.file_id
             ORDER BY f.path",
        )?;
        let rows = stmt.query_map([], |row| {
            let path: String = row.get(0)?;
            let package_name: String = row.get(1)?;
            Ok(GoPackageFileRow {
                package_key: physical_package_key(&path, &package_name),
                path,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn package_edges(&self) -> Result<Vec<GoPackageEdgeRow>> {
        let mut stmt = self.store.conn.prepare(
            "SELECT source_package_key, target_package_key, resolution
             FROM go_package_edges
             ORDER BY source_package_key, target_package_key",
        )?;
        let rows = stmt.query_map([], |row| {
            let resolution: String = row.get(2)?;
            Ok(GoPackageEdgeRow {
                source_package_key: row.get(0)?,
                target_package_key: row.get(1)?,
                resolution: if resolution == "exact" {
                    GoPackageResolution::Exact
                } else {
                    GoPackageResolution::Heuristic
                },
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn open_packages(&self) -> Result<Vec<GoOpenPackageRow>> {
        let mut stmt = self
            .store
            .conn
            .prepare("SELECT package_key, reason FROM go_open_packages ORDER BY package_key")?;
        let rows = stmt.query_map([], |row| {
            let reason: String = row.get(1)?;
            let reason = match reason.as_str() {
                "ambiguous_import" => OpenReason::AmbiguousInclude,
                "unsupported_language_boundary" => OpenReason::UnsupportedLanguageBoundary,
                "build_constraint_unknown" => OpenReason::BuildConstraintUnknown,
                _ => OpenReason::UnresolvedInclude,
            };
            Ok(GoOpenPackageRow {
                package_key: row.get(0)?,
                reason,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
}

impl IndexStore {
    pub fn go_package_graph_view(&self) -> GoPackageGraphStoreView<'_> {
        GoPackageGraphStoreView::new(self)
    }
}

fn physical_package_key(path: &str, package_name: &str) -> String {
    let directory = path
        .rsplit_once('/')
        .map(|(directory, _)| directory)
        .filter(|directory| !directory.is_empty())
        .unwrap_or(".");
    format!("{directory}#{package_name}")
}
