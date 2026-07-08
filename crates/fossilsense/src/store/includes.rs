use std::collections::HashSet;

use anyhow::Result;
use rusqlite::params;

use super::IndexStore;

#[cfg(test)]
type IncludeEdgeRows = Vec<(String, String)>;
#[cfg(test)]
type IncludeOpenRows = Vec<(String, crate::reachability::OpenReason)>;

impl IndexStore {
    /// Reset the first-layer flag on every external file, then set it on the
    /// external files whose path is in `paths`. Idempotent; the production path
    /// is [`apply_directly_included_derivation`]; this curated setter stays as a
    /// test oracle for exercising the flag in isolation.
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn mark_directly_included(&mut self, paths: &[String]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE files SET directly_included = 0 WHERE source = 'external'",
            [],
        )?;
        {
            let mut stmt = tx.prepare(
                "UPDATE files SET directly_included = 1 WHERE source = 'external' AND path = ?1",
            )?;
            for path in paths {
                stmt.execute([path])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Every indexed file as `(id, path, source)` — drives in-memory resolution
    /// of `#include` targets to file ids when (re)building the include graph.
    pub fn files_with_ids(&self) -> Result<Vec<(i64, String, String)>> {
        let mut stmt = self.conn.prepare("SELECT id, path, source FROM files")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        let mut files = Vec::new();
        for row in rows {
            files.push(row?);
        }
        Ok(files)
    }

    /// Raw `(file_id, target_text)` include rows. With `only` set, restrict to
    /// those source files (incremental edge rebuild); otherwise return all.
    pub fn includes_with_file_ids(&self, only: Option<&[i64]>) -> Result<Vec<(i64, String)>> {
        let mut out = Vec::new();
        match only {
            None => {
                let mut stmt = self
                    .conn
                    .prepare("SELECT file_id, target_text FROM includes")?;
                let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
                for row in rows {
                    out.push(row?);
                }
            }
            Some(ids) => {
                for chunk in ids.chunks(400) {
                    let placeholders = vec!["?"; chunk.len()].join(",");
                    let sql = format!(
                        "SELECT file_id, target_text FROM includes WHERE file_id IN ({placeholders})"
                    );
                    let mut stmt = self.conn.prepare(&sql)?;
                    let rows = stmt
                        .query_map(rusqlite::params_from_iter(chunk.iter().copied()), |row| {
                            Ok((row.get(0)?, row.get(1)?))
                        })?;
                    for row in rows {
                        out.push(row?);
                    }
                }
            }
        }
        Ok(out)
    }

    /// Replace include edges and the per-file unresolved/ambiguous counts for a
    /// set of source files, persisting each edge's resolution kind. With
    /// `clear_all`, wipe every edge / reset both counts on every file first (full
    /// rebuild); otherwise only the listed `src_ids` are cleared first (incremental
    /// rebuild). `edges` are `(src_file_id, dst_file_id, resolution_kind_string)`
    /// triples; `unresolved` and `ambiguous` are `(src_file_id, count)` pairs; all
    /// three are scoped to `src_ids`. `Ambiguous` and `Unresolved` resolutions
    /// produce no edge row — they only bump the corresponding count.
    pub fn replace_include_edges(
        &mut self,
        src_ids: &[i64],
        edges: &[(i64, i64, String)],
        unresolved: &[(i64, i64)],
        ambiguous: &[(i64, i64)],
        clear_all: bool,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        if clear_all {
            tx.execute("DELETE FROM include_edges", [])?;
            tx.execute("UPDATE files SET unresolved_includes = 0", [])?;
            tx.execute("UPDATE files SET ambiguous_includes = 0", [])?;
        } else {
            let mut del_edges = tx.prepare("DELETE FROM include_edges WHERE src_file_id = ?1")?;
            let mut reset_unresolved =
                tx.prepare("UPDATE files SET unresolved_includes = 0 WHERE id = ?1")?;
            let mut reset_ambiguous =
                tx.prepare("UPDATE files SET ambiguous_includes = 0 WHERE id = ?1")?;
            for id in src_ids {
                del_edges.execute([id])?;
                reset_unresolved.execute([id])?;
                reset_ambiguous.execute([id])?;
            }
        }
        {
            let mut ins = tx.prepare(
                "INSERT OR IGNORE INTO include_edges (src_file_id, dst_file_id, resolution) \
                 VALUES (?1, ?2, ?3)",
            )?;
            for (src, dst, resolution) in edges {
                ins.execute(params![src, dst, resolution])?;
            }
            let mut set_unresolved =
                tx.prepare("UPDATE files SET unresolved_includes = ?2 WHERE id = ?1")?;
            for (src, count) in unresolved {
                set_unresolved.execute(params![src, count])?;
            }
            let mut set_ambiguous =
                tx.prepare("UPDATE files SET ambiguous_includes = ?2 WHERE id = ?1")?;
            for (src, count) in ambiguous {
                set_ambiguous.execute(params![src, count])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Derive the first-layer `directly_included` flag on external files from
    /// the resolved include edges: an external header is flagged iff some
    /// workspace file has an `external_exact` include edge to it. Consistent
    /// with include form/priority by construction (a quote include that
    /// resolved `relative_exact`/`workspace_exact` no longer flags an external
    /// twin). Global recompute over the full edge table — the flag is a
    /// workspace-wide property. Idempotent; run each index pass.
    pub fn apply_directly_included_derivation(&mut self) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE files SET directly_included = 0 WHERE source = 'external'",
            [],
        )?;
        tx.execute(
            "UPDATE files SET directly_included = 1 \
             WHERE source = 'external' AND path IN ( \
                 SELECT DISTINCT df.path FROM include_edges e \
                 JOIN files sf ON sf.id = e.src_file_id \
                 JOIN files df ON df.id = e.dst_file_id \
                 WHERE sf.source = 'workspace' \
                   AND df.source = 'external' \
                   AND e.resolution = 'external_exact' \
             )",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Resolved include edges as `(src_path, dst_path)` pairs for building the
    /// in-memory reachability graph. Resolution kind is *not* read here — the
    /// graph is a plain file-to-file closure; the kind is read where a decision
    /// needs it (e.g. [`apply_directly_included_derivation`] via in-row SQL).
    #[cfg(test)]
    pub fn load_include_edge_paths(&self) -> Result<Vec<(String, String)>> {
        self.reach_graph_view().include_edges().map(|rows| {
            rows.into_iter()
                .map(crate::store::views::IncludeEdgeRow::into_legacy_tuple)
                .collect()
        })
    }

    /// Resolved include edges as `(src_path, dst_path, resolution_kind)` — for
    /// tests and any caller that needs the recorded kind. Resolution strings
    /// round-trip the four [`crate::includes::ResolutionKind`] variants.
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn load_include_edge_paths_with_resolution(&self) -> Result<Vec<(String, String, String)>> {
        self.reach_graph_view()
            .include_edges_with_resolution()
            .map(|rows| {
                rows.into_iter()
                    .map(crate::store::views::IncludeEdgeResolutionRow::into_legacy_tuple)
                    .collect()
            })
    }

    /// Paths of files with at least one unresolved `#include` — one source of
    /// "open" (uncertain) nodes in the reachability graph.
    #[cfg(test)]
    pub fn open_include_file_paths(&self) -> Result<Vec<String>> {
        self.reach_graph_view()
            .unresolved_includes()
            .map(|rows| rows.into_iter().map(|row| row.source_path).collect())
    }

    /// Paths of files with at least one ambiguous (multi-hit, no exact-tier
    /// winner) `#include` — the second source of "open" (uncertain) nodes in
    /// the reachability graph. Mirrors [`open_include_file_paths`]; the
    /// first-cause precedence (`UnresolvedInclude` before `AmbiguousInclude`)
    /// is encoded by `ReachGraph::new`, not by this query.
    #[cfg(test)]
    pub fn ambiguous_include_file_paths(&self) -> Result<Vec<String>> {
        self.reach_graph_view()
            .ambiguous_includes()
            .map(|rows| rows.into_iter().map(|row| row.source_path).collect())
    }

    /// Find (distinct, sorted) source file paths whose include rows name a path
    /// in `changed_rel_paths` or `changed_abs_paths`. The query uses persisted
    /// `target_basename` and `target_normalized` for efficient indexed lookup.
    ///
    /// Candidate forms generated per changed path:
    /// - exact as-stored
    /// - basename (last segment)
    /// - suffix forms (any `/changed_path` suffix)
    /// - include-root-relative (for absolute external paths matching a root)
    ///
    /// `roots_slash` are the configured include-path roots in `/`-separated
    /// absolute form.
    pub fn affected_include_sources(
        &self,
        changed_rel_paths: &[String],
        changed_abs_paths: &HashSet<String>,
        roots_slash: &[String],
    ) -> Result<Vec<String>> {
        if changed_rel_paths.is_empty() && changed_abs_paths.is_empty() {
            return Ok(Vec::new());
        }

        // Build candidate names: exact path, basename, suffix forms.
        let mut cand_basenames: HashSet<&str> = HashSet::new();
        let mut cand_normalized: HashSet<&str> = HashSet::new();

        for path in changed_rel_paths {
            cand_normalized.insert(path.as_str());
            if let Some(basename) = path.rsplit('/').next() {
                cand_basenames.insert(basename);
            }
        }

        for path in changed_abs_paths {
            cand_normalized.insert(path.as_str());
            if let Some(basename) = path.rsplit('/').next() {
                cand_basenames.insert(basename);
            }
            // Include-root-relative forms: strip known roots so an external path
            // like `C:/mingw/include/sys/types.h` can be matched as `sys/types.h`.
            for root in roots_slash {
                let root_prefix = format!("{}/", root.trim_end_matches('/'));
                if let Some(rel) = path.strip_prefix(&root_prefix) {
                    cand_normalized.insert(rel);
                    if let Some(bn) = rel.rsplit('/').next() {
                        cand_basenames.insert(bn);
                    }
                }
            }
        }

        // Emptied by sending only empty lists (checked above) or by
        // strip_prefix producing values already in the set; proceed anyway
        // — the query returns nothing when no candidates.

        let mut affected: HashSet<String> = HashSet::new();

        // Query 1: match by basename (indexed). This catches suffix-match-style
        // includes where the normalizer also recorded the same basename.
        for chunk in cand_basenames.iter().collect::<Vec<_>>().chunks(400) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!(
                "SELECT DISTINCT f.path FROM includes i \
                 JOIN files f ON f.id = i.file_id \
                 WHERE i.target_basename IN ({placeholders})"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(chunk.iter().copied()), |row| {
                    row.get::<_, String>(0)
                })?;
            for row in rows {
                affected.insert(row?);
            }
        }

        // Query 2: match by normalized target (indexed). Covers exact and
        // root-relative forms.
        let norm_list: Vec<&&str> = cand_normalized.iter().collect();
        for chunk in norm_list.chunks(400) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!(
                "SELECT DISTINCT f.path FROM includes i \
                 JOIN files f ON f.id = i.file_id \
                 WHERE i.target_normalized IN ({placeholders})"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(chunk.iter().copied()), |row| {
                    row.get::<_, String>(0)
                })?;
            for row in rows {
                affected.insert(row?);
            }
        }

        let mut out: Vec<String> = affected.into_iter().collect();
        out.sort();
        Ok(out)
    }

    /// Load include edges and open status for a set of source paths, used by
    /// incremental `ReachGraph` refresh after dirty updates. Returns:
    /// `(edges: Vec<(src, dst)>, open_files: Vec<(src, OpenReason)>)`.
    ///
    /// `OpenReason::AmbiguousInclude` wins on tie: the caller should use the
    /// same `UnresolvedInclude` > `AmbiguousInclude` precedence as `ReachGraph::new`.
    #[cfg(test)]
    pub fn load_include_data_for_sources(
        &self,
        source_paths: &[String],
    ) -> Result<(IncludeEdgeRows, IncludeOpenRows)> {
        self.reach_graph_view()
            .include_data_for_sources(source_paths)
            .map(|(edges, open)| {
                (
                    edges
                        .into_iter()
                        .map(crate::store::views::IncludeEdgeRow::into_legacy_tuple)
                        .collect(),
                    open.into_iter()
                        .map(crate::store::views::OpenIncludeRow::into_legacy_tuple)
                        .collect(),
                )
            })
    }
}
