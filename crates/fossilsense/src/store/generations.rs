use std::collections::HashSet;

use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, TransactionBehavior};

use super::{
    now_unix_secs, IncludeGraphUpdate, IndexBuild, IndexCommitOutcome, IndexStore,
    SemanticReadGuard,
};

impl IndexStore {
    pub fn begin_index_build(&mut self, full_rebuild: bool) -> Result<IndexBuild> {
        anyhow::ensure!(
            !self.maintenance_blocked,
            "index maintenance is blocked because foreign-key enforcement \
             could not be restored; reopen the index before writing"
        );
        self.discard_abandoned_index_builds()?;
        if self.cleanup_required()? {
            // A persisted marker means either the previous post-publication
            // cleanup failed, an abandoned staging build left unreachable
            // revisions, or this current-schema database predates the marker.
            // Recovery includes call-string GC even for an incremental build so
            // every kind of prior maintenance debt is settled before new facts
            // are staged.
            self.collect_inactive_revisions(true, None)?;
        }
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

    fn discard_abandoned_index_builds(&mut self) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let abandoned = tx.execute("DELETE FROM index_builds WHERE state = 'staging'", [])?;
        if abandoned > 0 {
            tx.execute(
                "INSERT INTO meta (key, value) VALUES ('cleanup_required', '1')
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn cleanup_required(&self) -> Result<bool> {
        let value: String = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'cleanup_required'",
                [],
                |row| row.get(0),
            )
            .context("cleanup_required metadata is missing")?;
        match value.as_str() {
            "0" => Ok(false),
            "1" => Ok(true),
            _ => anyhow::bail!("invalid cleanup_required metadata value {value:?}"),
        }
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

    /// Seed a fresh side-by-side database with the last published generation so
    /// its first build advances monotonically across database files.
    pub fn seed_semantic_generation(&self, generation: u64) -> Result<()> {
        let revisions: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM file_revisions", [], |row| row.get(0))?;
        anyhow::ensure!(
            revisions == 0,
            "semantic generation can only be seeded in an empty database"
        );
        self.conn.execute(
            "UPDATE meta SET value = ?1 WHERE key = 'semantic_generation'",
            [generation.to_string()],
        )?;
        Ok(())
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

        let mut cleanup_file_ids = HashSet::new();
        if build.full_rebuild {
            let active_file_ids = {
                let mut active = tx.prepare("SELECT file_id FROM active_file_revisions")?;
                let rows = active.query_map([], |row| row.get::<_, i64>(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            cleanup_file_ids.extend(active_file_ids);
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
            cleanup_file_ids.insert(file_id);
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
        if include_graph.clear_all_go_packages {
            tx.execute("DELETE FROM go_package_edges", [])?;
            tx.execute("DELETE FROM go_open_packages", [])?;
            tx.execute("DELETE FROM go_importable_packages", [])?;
        }
        for (source, target, resolution) in &include_graph.go_package_edges {
            tx.execute(
                "INSERT OR REPLACE INTO go_package_edges (
                    source_package_key, target_package_key, resolution
                 ) VALUES (?1, ?2, ?3)",
                rusqlite::params![source, target, resolution],
            )?;
        }
        for (package_key, reason) in &include_graph.go_open_packages {
            tx.execute(
                "INSERT OR REPLACE INTO go_open_packages (package_key, reason)
                 VALUES (?1, ?2)",
                rusqlite::params![package_key, reason],
            )?;
        }
        for (package_key, import_path) in &include_graph.go_importable_packages {
            tx.execute(
                "INSERT OR REPLACE INTO go_importable_packages (package_key, import_path)
                 VALUES (?1, ?2)",
                rusqlite::params![package_key, import_path],
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
        if let Some(protobuf_c_sources) = &include_graph.protobuf_c_sources {
            tx.execute("DELETE FROM protobuf_c_sources", [])?;
            let mut insert = tx.prepare(
                "INSERT INTO protobuf_c_sources (
                    declaration_id, proto_path, proto_name, c_name, kind,
                    start_byte, end_byte, start_line, start_col, end_line, end_col,
                    match_kind, source_truncated
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            )?;
            for source in protobuf_c_sources {
                insert.execute(rusqlite::params![
                    source.declaration_id,
                    source.proto_path,
                    source.proto_name,
                    source.c_name,
                    source.kind,
                    source.start_byte as i64,
                    source.end_byte as i64,
                    source.start_line as i64,
                    source.start_col as i64,
                    source.end_line as i64,
                    source.end_col as i64,
                    source.match_kind,
                    i64::from(source.source_truncated),
                ])?;
            }
        }
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
        // The active manifest and cleanup debt become durable together. If the
        // process exits after this commit, the next build retries maintenance
        // before accepting more staged facts.
        tx.execute(
            "INSERT INTO meta (key, value) VALUES ('cleanup_required', '1')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [],
        )?;
        tx.commit()?;
        let cleanup_warning = self
            .collect_inactive_revisions(build.full_rebuild, Some(&cleanup_file_ids))
            .err()
            .map(|error| format!("post-publication cleanup failed: {error:#}"));
        Ok(IndexCommitOutcome {
            generation: build.target_generation,
            cleanup_warning,
        })
    }

    fn collect_inactive_revisions(
        &mut self,
        collect_call_strings: bool,
        cleanup_file_ids: Option<&HashSet<i64>>,
    ) -> Result<()> {
        let foreign_keys_enabled: i64 =
            self.conn
                .pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
        anyhow::ensure!(
            foreign_keys_enabled == 1,
            "inactive revision cleanup requires foreign-key enforcement to start enabled"
        );

        // SQLite implements a parent DELETE by probing every child relation for
        // each deleted revision. Relying on ON DELETE CASCADE turns legacy debt
        // into repeated probes and previously degenerated to O(revisions ×
        // facts) when deferred indexes were absent during full builds. Disable
        // enforcement only around one explicit, validated transaction and
        // remove child rows in indexed sets before their parents.
        self.conn.pragma_update(None, "foreign_keys", "OFF")?;
        let cleanup_result = self.collect_inactive_revisions_without_foreign_keys(
            collect_call_strings,
            cleanup_file_ids,
        );
        let restore_result = (|| -> Result<()> {
            self.conn
                .pragma_update(None, "foreign_keys", "ON")
                .context("failed to restore foreign-key enforcement after cleanup")?;
            let enabled: i64 = self
                .conn
                .pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
            anyhow::ensure!(
                enabled == 1,
                "foreign-key enforcement remained disabled after cleanup"
            );
            Ok(())
        })();
        if restore_result.is_err() {
            // Do not let this connection silently continue writing with
            // enforcement disabled. The in-memory latch blocks its next build;
            // the best-effort durable marker asks a newly opened connection to
            // repeat the validated cleanup before writing.
            self.maintenance_blocked = true;
            let _ = self.conn.execute(
                "INSERT INTO meta (key, value) VALUES ('cleanup_required', '1')
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [],
            );
        }

        match (cleanup_result, restore_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(cleanup), Ok(())) => Err(cleanup),
            (Ok(()), Err(restore)) => Err(restore),
            (Err(cleanup), Err(restore)) => Err(anyhow::anyhow!(
                "inactive revision cleanup failed: {cleanup:#}; \
                 additionally failed to restore foreign-key enforcement: {restore:#}"
            )),
        }
    }

    fn collect_inactive_revisions_without_foreign_keys(
        &mut self,
        collect_call_strings: bool,
        cleanup_file_ids: Option<&HashSet<i64>>,
    ) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(
            "DROP TABLE IF EXISTS temp.cleanup_obsolete_revisions;
             DROP TABLE IF EXISTS temp.cleanup_orphan_files;
             DROP TABLE IF EXISTS temp.cleanup_scope_files;
             DROP TABLE IF EXISTS temp.cleanup_record_ids;
             DROP TABLE IF EXISTS temp.cleanup_anchor_ids;
             CREATE TEMP TABLE cleanup_obsolete_revisions (
                 revision_id INTEGER PRIMARY KEY,
                 file_id INTEGER NOT NULL
             ) WITHOUT ROWID;
             CREATE TEMP TABLE cleanup_orphan_files (
                 file_id INTEGER PRIMARY KEY
             ) WITHOUT ROWID;
             CREATE TEMP TABLE cleanup_scope_files (
                 file_id INTEGER PRIMARY KEY
             ) WITHOUT ROWID;
             CREATE TEMP TABLE cleanup_record_ids (
                 id INTEGER PRIMARY KEY
             ) WITHOUT ROWID;
             CREATE TEMP TABLE cleanup_anchor_ids (
                 id INTEGER PRIMARY KEY
             ) WITHOUT ROWID;",
        )?;
        if let Some(file_ids) = cleanup_file_ids {
            let mut insert_scope =
                tx.prepare("INSERT INTO cleanup_scope_files (file_id) VALUES (?1)")?;
            for file_id in file_ids {
                insert_scope.execute([file_id])?;
            }
        }

        if cleanup_file_ids.is_some() {
            // CROSS JOIN fixes the small temp scope as the outer loop; each
            // changed file then probes the permanent file_id index instead of
            // scanning every revision in the workspace.
            tx.execute_batch(
                "INSERT INTO cleanup_obsolete_revisions (revision_id, file_id)
                 SELECT r.id, r.file_id
                 FROM cleanup_scope_files scope
                 CROSS JOIN file_revisions r INDEXED BY idx_file_revisions_file_id
                 WHERE r.file_id = scope.file_id
                   AND r.id NOT IN (SELECT revision_id FROM active_file_revisions)
                   AND r.id NOT IN (
                       SELECT revision_id FROM pending_file_revisions
                       WHERE revision_id IS NOT NULL
                   );
                 INSERT INTO cleanup_orphan_files (file_id)
                 SELECT f.id
                 FROM cleanup_scope_files scope
                 CROSS JOIN file_entries f
                 WHERE f.id = scope.file_id
                   AND f.id NOT IN (SELECT file_id FROM active_file_revisions)
                   AND f.id NOT IN (SELECT file_id FROM pending_file_revisions)
                   AND NOT EXISTS (
                       SELECT 1 FROM file_revisions r
                       WHERE r.file_id = f.id
                         AND r.id NOT IN (
                             SELECT revision_id FROM cleanup_obsolete_revisions
                         )
                   );",
            )?;
        } else {
            // Recovery is exceptional: reconcile the durable marker against
            // the whole database so debt from an interrupted older binary or
            // an abandoned build cannot be missed.
            tx.execute_batch(
                "INSERT INTO cleanup_obsolete_revisions (revision_id, file_id)
                 SELECT id, file_id FROM file_revisions
                 WHERE id NOT IN (SELECT revision_id FROM active_file_revisions)
                   AND id NOT IN (
                       SELECT revision_id FROM pending_file_revisions
                       WHERE revision_id IS NOT NULL
                   );
                 INSERT INTO cleanup_orphan_files (file_id)
                 SELECT f.id FROM file_entries f
                 WHERE f.id NOT IN (SELECT file_id FROM active_file_revisions)
                   AND f.id NOT IN (SELECT file_id FROM pending_file_revisions)
                   AND NOT EXISTS (
                       SELECT 1 FROM file_revisions r
                       WHERE r.file_id = f.id
                         AND r.id NOT IN (
                             SELECT revision_id FROM cleanup_obsolete_revisions
                         )
                   );",
            )?;
        }

        let obsolete_revision_count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM cleanup_obsolete_revisions",
            [],
            |row| row.get(0),
        )?;
        let orphan_file_count: i64 =
            tx.query_row("SELECT COUNT(*) FROM cleanup_orphan_files", [], |row| {
                row.get(0)
            })?;

        if obsolete_revision_count > 0 || orphan_file_count > 0 {
            // Capture relation parents before deleting any direct facts. These
            // temp IDs reproduce call-site CASCADE and record SET NULL even
            // for schema-valid historical rows whose revision_id and file_id
            // point at different files.
            tx.execute(
                "INSERT INTO cleanup_record_ids (id)
                 SELECT id FROM record_facts
                 WHERE revision_id IN (
                           SELECT revision_id FROM cleanup_obsolete_revisions
                       )
                    OR file_id IN (
                           SELECT file_id FROM cleanup_orphan_files
                       )",
                [],
            )?;
            tx.execute(
                "INSERT INTO cleanup_anchor_ids (id)
                 SELECT id FROM callable_anchor_facts
                 WHERE revision_id IN (
                           SELECT revision_id FROM cleanup_obsolete_revisions
                       )
                    OR file_id IN (
                           SELECT file_id FROM cleanup_orphan_files
                       )",
                [],
            )?;
            tx.execute(
                "DELETE FROM call_site_facts
                 WHERE revision_id IN (
                           SELECT revision_id FROM cleanup_obsolete_revisions
                       )
                    OR file_id IN (
                           SELECT file_id FROM cleanup_orphan_files
                       )
                    OR caller_anchor_id IN (
                           SELECT id FROM cleanup_anchor_ids
                       )",
                [],
            )?;
            tx.execute(
                "DELETE FROM protobuf_c_sources
                 WHERE declaration_id IN (
                     SELECT id FROM declaration_facts
                     WHERE revision_id IN (
                               SELECT revision_id FROM cleanup_obsolete_revisions
                           )
                        OR file_id IN (
                               SELECT file_id FROM cleanup_orphan_files
                           )
                 )",
                [],
            )?;

            for table in [
                "fallback_completion_facts",
                "declaration_facts",
                "package_facts",
                "import_facts",
                "include_facts",
                "member_facts",
                "type_alias_facts",
            ] {
                let statement = format!(
                    "DELETE FROM {table}
                     WHERE revision_id IN (
                               SELECT revision_id
                               FROM cleanup_obsolete_revisions
                           )
                        OR file_id IN (
                               SELECT file_id FROM cleanup_orphan_files
                           )"
                );
                tx.execute(&statement, [])?;
            }

            tx.execute(
                "UPDATE member_facts SET record_id = NULL
                 WHERE record_id IN (SELECT id FROM cleanup_record_ids)",
                [],
            )?;
            tx.execute(
                "UPDATE type_alias_facts SET target_record_id = NULL
                 WHERE target_record_id IN (SELECT id FROM cleanup_record_ids)",
                [],
            )?;
            tx.execute(
                "DELETE FROM callable_anchor_facts
                 WHERE id IN (SELECT id FROM cleanup_anchor_ids)",
                [],
            )?;
            tx.execute(
                "DELETE FROM record_facts
                 WHERE id IN (SELECT id FROM cleanup_record_ids)",
                [],
            )?;
            tx.execute(
                "DELETE FROM file_revisions
                 WHERE id IN (
                     SELECT revision_id FROM cleanup_obsolete_revisions
                 )",
                [],
            )?;
        }

        // include_edges has two file-entry foreign keys. Remove both directions
        // explicitly before deleting entries while enforcement is suspended.
        if orphan_file_count > 0 {
            tx.execute(
                "DELETE FROM include_edges
                 WHERE src_file_id IN (SELECT file_id FROM cleanup_orphan_files)
                    OR dst_file_id IN (SELECT file_id FROM cleanup_orphan_files)",
                [],
            )?;
            tx.execute(
                "DELETE FROM file_entries
                 WHERE id IN (SELECT file_id FROM cleanup_orphan_files)",
                [],
            )?;
        }

        if collect_call_strings {
            tx.execute(
                "DELETE FROM call_strings WHERE id NOT IN (
                    SELECT name_id FROM callable_anchor_facts
                    UNION SELECT qualified_name_id FROM callable_anchor_facts
                    UNION SELECT owner_id FROM callable_anchor_facts WHERE owner_id IS NOT NULL
                    UNION SELECT linkage_file_id FROM callable_anchor_facts WHERE linkage_file_id IS NOT NULL
                    UNION SELECT signature_id FROM callable_anchor_facts
                    UNION SELECT canonical_signature_id FROM callable_anchor_facts
                    UNION SELECT presentation_signature_id FROM callable_anchor_facts
                    UNION SELECT guard_id FROM callable_anchor_facts WHERE guard_id IS NOT NULL
                    UNION SELECT callee_name_id FROM call_site_facts WHERE callee_name_id IS NOT NULL
                    UNION SELECT qualified_name_id FROM call_site_facts WHERE qualified_name_id IS NOT NULL
                    UNION SELECT guard_id FROM call_site_facts WHERE guard_id IS NOT NULL
                 )",
                [],
            )?;
        }

        if collect_call_strings || cleanup_file_ids.is_none() {
            let has_foreign_key_violation = {
                let mut check = tx.prepare("PRAGMA foreign_key_check")?;
                check.exists([])?
            };
            anyhow::ensure!(
                !has_foreign_key_violation,
                "bulk inactive revision cleanup introduced a foreign-key violation"
            );
        } else if obsolete_revision_count > 0 || orphan_file_count > 0 {
            // Online incremental cleanup validates only relationships whose
            // parent rows were touched. Every predicate is backed by a parent
            // ID, revision, file, caller, record-target, or include-edge index,
            // so latency scales with this commit's file scope rather than all
            // facts in the workspace.
            for table in [
                "fallback_completion_facts",
                "declaration_facts",
                "package_facts",
                "import_facts",
                "include_facts",
                "record_facts",
                "member_facts",
                "type_alias_facts",
                "callable_anchor_facts",
                "call_site_facts",
            ] {
                let statement = format!(
                    "SELECT EXISTS(
                         SELECT 1 FROM {table}
                         WHERE revision_id IN (
                                   SELECT revision_id
                                   FROM cleanup_obsolete_revisions
                               )
                            OR file_id IN (
                                   SELECT file_id FROM cleanup_orphan_files
                               )
                     )"
                );
                let dangling: bool = tx.query_row(&statement, [], |row| row.get(0))?;
                anyhow::ensure!(
                    !dangling,
                    "scoped cleanup left {table} attached to a deleted parent"
                );
            }
            let dangling_proto_source: bool = tx.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM protobuf_c_sources s
                     LEFT JOIN declaration_facts d ON d.id = s.declaration_id
                     WHERE d.id IS NULL
                 )",
                [],
                |row| row.get(0),
            )?;
            anyhow::ensure!(
                !dangling_proto_source,
                "scoped cleanup left protobuf-c sources without declarations"
            );
            let dangling_relation: bool = tx.query_row(
                "SELECT
                     EXISTS(
                         SELECT 1 FROM active_file_revisions
                         WHERE revision_id IN (
                                   SELECT revision_id
                                   FROM cleanup_obsolete_revisions
                               )
                            OR file_id IN (
                                   SELECT file_id FROM cleanup_orphan_files
                               )
                     )
                     OR EXISTS(
                         SELECT 1 FROM pending_file_revisions
                         WHERE revision_id IN (
                                   SELECT revision_id
                                   FROM cleanup_obsolete_revisions
                               )
                            OR file_id IN (
                                   SELECT file_id FROM cleanup_orphan_files
                               )
                     )
                     OR EXISTS(
                         SELECT 1 FROM file_revisions
                         WHERE id IN (
                                   SELECT revision_id
                                   FROM cleanup_obsolete_revisions
                               )
                            OR file_id IN (
                                   SELECT file_id FROM cleanup_orphan_files
                               )
                     )
                     OR EXISTS(
                         SELECT 1 FROM file_entries
                         WHERE id IN (
                             SELECT file_id FROM cleanup_orphan_files
                         )
                     )
                     OR EXISTS(
                         SELECT 1 FROM include_edges
                         WHERE src_file_id IN (
                                   SELECT file_id FROM cleanup_orphan_files
                               )
                            OR dst_file_id IN (
                                   SELECT file_id FROM cleanup_orphan_files
                               )
                     )
                     OR EXISTS(
                         SELECT 1 FROM call_site_facts
                         WHERE caller_anchor_id IN (
                             SELECT id FROM cleanup_anchor_ids
                         )
                     )
                     OR EXISTS(
                         SELECT 1 FROM member_facts
                         WHERE record_id IN (
                             SELECT id FROM cleanup_record_ids
                         )
                     )
                     OR EXISTS(
                         SELECT 1 FROM type_alias_facts
                         WHERE target_record_id IN (
                             SELECT id FROM cleanup_record_ids
                         )
                     )",
                [],
                |row| row.get(0),
            )?;
            anyhow::ensure!(
                !dangling_relation,
                "scoped cleanup left a relation attached to a deleted parent"
            );
        }
        tx.execute(
            "INSERT INTO meta (key, value) VALUES ('cleanup_required', '0')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [],
        )?;
        tx.execute_batch(
            "DROP TABLE temp.cleanup_obsolete_revisions;
             DROP TABLE temp.cleanup_orphan_files;
             DROP TABLE temp.cleanup_scope_files;
             DROP TABLE temp.cleanup_record_ids;
             DROP TABLE temp.cleanup_anchor_ids;",
        )?;
        tx.commit()?;
        Ok(())
    }
}
