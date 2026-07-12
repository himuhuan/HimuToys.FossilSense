use std::collections::HashSet;

use anyhow::Result;
use rusqlite::OptionalExtension;

use super::{
    now_unix_secs, IncludeGraphUpdate, IndexBuild, IndexCommitOutcome, IndexStore,
    SemanticReadGuard,
};

impl IndexStore {
    pub fn begin_index_build(&mut self, full_rebuild: bool) -> Result<IndexBuild> {
        self.conn
            .execute("DELETE FROM index_builds WHERE state = 'staging'", [])?;
        let current = self.semantic_generation()?;
        let target = current.saturating_add(1).max(1);
        self.conn.execute(
            "INSERT INTO index_builds (target_generation, full_rebuild, state, created_at)
             VALUES (?1, ?2, 'staging', ?3)",
            rusqlite::params![target as i64, i64::from(full_rebuild), now_unix_secs()],
        )?;
        Ok(IndexBuild {
            id: self.conn.last_insert_rowid(),
            target_generation: target,
            full_rebuild,
        })
    }

    pub fn semantic_generation(&self) -> Result<u64> {
        let value: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'semantic_generation'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value.and_then(|value| value.parse().ok()).unwrap_or(0))
    }

    pub fn begin_semantic_read(
        &self,
        expected_generation: Option<u64>,
    ) -> Result<SemanticReadGuard<'_>> {
        self.conn.execute_batch("BEGIN DEFERRED")?;
        let generation = match self.semantic_generation() {
            Ok(generation) => generation,
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                return Err(error);
            }
        };
        if expected_generation.is_some_and(|expected| expected != generation) {
            self.conn.execute_batch("ROLLBACK")?;
            anyhow::bail!(
                "semantic generation mismatch: expected {}, observed {generation}",
                expected_generation.unwrap_or_default()
            );
        }
        Ok(SemanticReadGuard {
            store: self,
            generation,
            active: true,
        })
    }

    pub fn stage_delete_file(&mut self, build: IndexBuild, path: &str) -> Result<usize> {
        let file_id: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM file_entries WHERE path = ?1",
                [path],
                |row| row.get(0),
            )
            .optional()?;
        let Some(file_id) = file_id else {
            return Ok(0);
        };
        self.conn.execute(
            "INSERT INTO pending_file_revisions (build_id, file_id, revision_id)
             VALUES (?1, ?2, NULL)
             ON CONFLICT(build_id, file_id) DO UPDATE SET revision_id = NULL",
            rusqlite::params![build.id, file_id],
        )?;
        Ok(1)
    }

    pub fn stage_delete_missing_files(
        &mut self,
        build: IndexBuild,
        seen_paths: &HashSet<String>,
    ) -> Result<usize> {
        let active_paths: Vec<String> = {
            let mut stmt = self.conn.prepare(
                "SELECT f.path FROM file_entries f JOIN active_file_revisions a ON a.file_id = f.id",
            )?;
            let rows = stmt.query_map([], |row| row.get(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut deleted = 0;
        for path in active_paths {
            if !seen_paths.contains(&path) {
                deleted += self.stage_delete_file(build, &path)?;
            }
        }
        Ok(deleted)
    }

    pub fn commit_index_build(
        &mut self,
        build: IndexBuild,
        include_graph: &IncludeGraphUpdate,
    ) -> Result<IndexCommitOutcome> {
        let tx = self.conn.transaction()?;
        let state: String = tx.query_row(
            "SELECT state FROM index_builds WHERE id = ?1 AND target_generation = ?2",
            rusqlite::params![build.id, build.target_generation as i64],
            |row| row.get(0),
        )?;
        anyhow::ensure!(state == "staging", "index build is not staging");

        if build.full_rebuild {
            tx.execute("DELETE FROM active_file_revisions", [])?;
        }
        let changes = {
            let mut pending = tx.prepare(
                "SELECT file_id, revision_id FROM pending_file_revisions WHERE build_id = ?1",
            )?;
            let rows = pending.query_map([build.id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (file_id, revision_id) in changes {
            match revision_id {
                Some(revision_id) => {
                    tx.execute(
                        "INSERT INTO active_file_revisions (file_id, revision_id)
                         VALUES (?1, ?2)
                         ON CONFLICT(file_id) DO UPDATE SET revision_id = excluded.revision_id",
                        rusqlite::params![file_id, revision_id],
                    )?;
                    tx.execute(
                        "UPDATE file_entries SET
                            extension = r.extension, size = r.size, mtime_ns = r.mtime_ns,
                            hash = r.hash, indexed_at = r.indexed_at, status = r.status,
                            error = r.error, source = r.source
                         FROM file_revisions r WHERE file_entries.id = ?1 AND r.id = ?2",
                        rusqlite::params![file_id, revision_id],
                    )?;
                }
                None => {
                    tx.execute(
                        "DELETE FROM active_file_revisions WHERE file_id = ?1",
                        [file_id],
                    )?;
                }
            }
        }

        if include_graph.clear_all {
            tx.execute("DELETE FROM include_edges", [])?;
            tx.execute(
                "UPDATE file_entries SET unresolved_includes = 0, ambiguous_includes = 0",
                [],
            )?;
        } else {
            for id in &include_graph.source_ids {
                tx.execute("DELETE FROM include_edges WHERE src_file_id = ?1", [id])?;
                tx.execute(
                    "UPDATE file_entries SET unresolved_includes = 0, ambiguous_includes = 0 WHERE id = ?1",
                    [id],
                )?;
            }
        }
        tx.execute(
            "DELETE FROM include_edges
             WHERE src_file_id NOT IN (SELECT file_id FROM active_file_revisions)
                OR dst_file_id NOT IN (SELECT file_id FROM active_file_revisions)",
            [],
        )?;
        for (src, dst, resolution) in &include_graph.edges {
            tx.execute(
                "INSERT OR IGNORE INTO include_edges (src_file_id, dst_file_id, resolution)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![src, dst, resolution],
            )?;
        }
        for (src, count) in &include_graph.unresolved {
            tx.execute(
                "UPDATE file_entries SET unresolved_includes = ?2 WHERE id = ?1",
                rusqlite::params![src, count],
            )?;
        }
        for (src, count) in &include_graph.ambiguous {
            tx.execute(
                "UPDATE file_entries SET ambiguous_includes = ?2 WHERE id = ?1",
                rusqlite::params![src, count],
            )?;
        }
        tx.execute(
            "UPDATE file_entries SET directly_included = 0 WHERE source = 'external'",
            [],
        )?;
        tx.execute(
            "UPDATE file_entries SET directly_included = 1
             WHERE source = 'external' AND id IN (
                 SELECT DISTINCT e.dst_file_id FROM include_edges e
                 JOIN file_entries sf ON sf.id = e.src_file_id
                 JOIN file_entries df ON df.id = e.dst_file_id
                 WHERE sf.source = 'workspace' AND df.source = 'external'
                   AND e.resolution = 'external_exact'
             )",
            [],
        )?;
        tx.execute(
            "INSERT INTO meta (key, value) VALUES ('semantic_generation', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [build.target_generation.to_string()],
        )?;
        tx.execute(
            "UPDATE index_builds SET state = 'committed' WHERE id = ?1",
            [build.id],
        )?;
        tx.execute(
            "DELETE FROM pending_file_revisions WHERE build_id = ?1",
            [build.id],
        )?;
        tx.commit()?;
        let cleanup_warning = self
            .collect_inactive_revisions(build.full_rebuild)
            .err()
            .map(|error| format!("post-publication cleanup failed: {error:#}"));
        Ok(IndexCommitOutcome {
            generation: build.target_generation,
            cleanup_warning,
        })
    }

    fn collect_inactive_revisions(&mut self, collect_call_strings: bool) -> Result<()> {
        self.conn.execute(
            "DELETE FROM file_revisions
             WHERE id NOT IN (SELECT revision_id FROM active_file_revisions)
               AND id NOT IN (
                   SELECT revision_id FROM pending_file_revisions WHERE revision_id IS NOT NULL
               )",
            [],
        )?;
        self.conn.execute(
            "DELETE FROM file_entries
             WHERE id NOT IN (SELECT file_id FROM active_file_revisions)
               AND id NOT IN (SELECT file_id FROM pending_file_revisions)",
            [],
        )?;
        if collect_call_strings {
            self.conn.execute(
                "DELETE FROM call_strings WHERE id NOT IN (
                    SELECT name_id FROM callable_anchor_facts
                    UNION SELECT qualified_name_id FROM callable_anchor_facts
                    UNION SELECT owner_id FROM callable_anchor_facts WHERE owner_id IS NOT NULL
                    UNION SELECT linkage_file_id FROM callable_anchor_facts WHERE linkage_file_id IS NOT NULL
                    UNION SELECT signature_id FROM callable_anchor_facts
                    UNION SELECT guard_id FROM callable_anchor_facts WHERE guard_id IS NOT NULL
                    UNION SELECT callee_name_id FROM call_site_facts WHERE callee_name_id IS NOT NULL
                    UNION SELECT qualified_name_id FROM call_site_facts WHERE qualified_name_id IS NOT NULL
                    UNION SELECT guard_id FROM call_site_facts WHERE guard_id IS NOT NULL
                 )",
                [],
            )?;
        }
        Ok(())
    }
}
