use std::collections::HashSet;

use anyhow::Result;

use super::{IncludeGraphUpdate, IndexBuild, IndexStore};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GeneratedDeclarationRow {
    pub(crate) declaration_id: i64,
    pub(crate) file_id: i64,
    pub(crate) name: String,
}

impl IndexStore {
    pub(crate) fn effective_generated_declarations(
        &self,
        build: IndexBuild,
    ) -> Result<Vec<GeneratedDeclarationRow>> {
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
        let sql = format!(
            "WITH effective(file_id, revision_id) AS ({effective})
             SELECT d.id, d.file_id, d.name
             FROM declaration_facts d
             JOIN effective e ON e.file_id = d.file_id AND e.revision_id = d.revision_id
             WHERE d.language <> 3 AND d.declaration_kind IN (3, 4)
             ORDER BY d.file_id, d.name, d.id"
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map([build.id], |row| {
            Ok(GeneratedDeclarationRow {
                declaration_id: row.get(0)?,
                file_id: row.get(1)?,
                name: row.get(2)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub(crate) fn effective_include_edge_ids(
        &self,
        build: IndexBuild,
        update: &IncludeGraphUpdate,
    ) -> Result<Vec<(i64, i64)>> {
        let effective_files: HashSet<i64> = self
            .effective_files_with_ids(build)?
            .into_iter()
            .map(|(id, _, _)| id)
            .collect();
        let replaced_sources: HashSet<i64> = update.source_ids.iter().copied().collect();
        let mut edges = Vec::new();
        if !update.clear_all {
            let mut statement = self
                .conn
                .prepare("SELECT src_file_id, dst_file_id FROM include_edges")?;
            let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            for row in rows {
                let (source, target) = row?;
                if !replaced_sources.contains(&source)
                    && effective_files.contains(&source)
                    && effective_files.contains(&target)
                {
                    edges.push((source, target));
                }
            }
        }
        edges.extend(
            update
                .edges
                .iter()
                .filter(|(source, target, _)| {
                    effective_files.contains(source) && effective_files.contains(target)
                })
                .map(|(source, target, _)| (*source, *target)),
        );
        edges.sort_unstable();
        edges.dedup();
        Ok(edges)
    }
}
