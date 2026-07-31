use anyhow::Result;

use super::{IndexBuild, IndexStore};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EffectiveGoPackage {
    pub file_id: i64,
    pub path: String,
    pub source: String,
    pub package_key: String,
    pub package_name: String,
    pub build_guard: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EffectiveGoImport {
    pub source_package_key: String,
    pub import_path: String,
}

impl IndexStore {
    pub(crate) fn effective_go_packages(
        &self,
        build: IndexBuild,
    ) -> Result<Vec<EffectiveGoPackage>> {
        let sql = effective_revision_query(
            build,
            "SELECT p.file_id, f.path, f.source, p.name, r.build_guard
             FROM package_facts p
             JOIN effective e ON e.file_id = p.file_id AND e.revision_id = p.revision_id
             JOIN file_entries f ON f.id = p.file_id
             JOIN file_revisions r ON r.id = p.revision_id
             ORDER BY f.path",
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([build.id], |row| {
            let path: String = row.get(1)?;
            let package_name: String = row.get(3)?;
            Ok(EffectiveGoPackage {
                file_id: row.get(0)?,
                source: row.get(2)?,
                package_key: physical_package_key(&path, &package_name),
                path,
                package_name,
                build_guard: row.get(4)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub(crate) fn effective_go_imports(&self, build: IndexBuild) -> Result<Vec<EffectiveGoImport>> {
        let sql = effective_revision_query(
            build,
            "SELECT f.path, p.name, i.import_path
             FROM import_facts i
             JOIN effective e ON e.file_id = i.file_id AND e.revision_id = i.revision_id
             JOIN package_facts p
               ON p.file_id = i.file_id AND p.revision_id = i.revision_id
             JOIN file_entries f ON f.id = i.file_id
             ORDER BY f.path, i.id",
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([build.id], |row| {
            let path: String = row.get(0)?;
            let package_name: String = row.get(1)?;
            Ok(EffectiveGoImport {
                source_package_key: physical_package_key(&path, &package_name),
                import_path: row.get(2)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
}

fn effective_revision_query(build: IndexBuild, projection: &str) -> String {
    let effective = if build.full_rebuild {
        "SELECT p.file_id, p.revision_id
         FROM pending_file_revisions p
         WHERE p.build_id = ?1 AND p.revision_id IS NOT NULL"
    } else {
        "SELECT a.file_id, a.revision_id FROM active_file_revisions a
         WHERE NOT EXISTS (
             SELECT 1 FROM pending_file_revisions p
             WHERE p.build_id = ?1 AND p.file_id = a.file_id
         )
         UNION ALL
         SELECT p.file_id, p.revision_id FROM pending_file_revisions p
         WHERE p.build_id = ?1 AND p.revision_id IS NOT NULL"
    };
    format!("WITH effective AS ({effective}) {projection}")
}

fn physical_package_key(path: &str, package_name: &str) -> String {
    let directory = path
        .rsplit_once('/')
        .map(|(directory, _)| directory)
        .filter(|directory| !directory.is_empty())
        .unwrap_or(".");
    format!("{directory}#{package_name}")
}
