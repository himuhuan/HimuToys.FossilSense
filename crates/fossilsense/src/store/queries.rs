use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

use crate::model::RecordCandidate;

use super::{
    map_symbol_record, record_kind_from_str, IndexStore, SymbolRecord, SELECT_SYMBOL_JOIN,
};

impl IndexStore {
    /// Load every symbol id + name (+ external flag) for building the in-memory
    /// fuzzy name table.
    ///
    /// Fields are excluded: they only serve member completion via the dedicated
    /// record/field queries and must not surface in workspace symbol or ordinary
    /// completion. The external flag lets the name table rank workspace symbols
    /// ahead of external (toolchain) ones without filtering the latter out.
    pub fn load_symbol_names(&self) -> Result<Vec<(i64, String, bool)>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.name, f.source FROM symbols s JOIN files f ON f.id = s.file_id \
             WHERE s.kind != 'field'",
        )?;
        let rows = stmt.query_map([], |row| {
            let source: String = row.get(2)?;
            Ok((row.get(0)?, row.get(1)?, source == "external"))
        })?;
        let mut names = Vec::new();
        for row in rows {
            names.push(row?);
        }
        Ok(names)
    }

    /// Load every symbol id + name (+ external flag, path, kind) for building
    /// the in-memory fuzzy `NameTable` with per-symbol kind cached. The kind
    /// string lets the completion hot path render an icon without re-opening
    /// the store. Fields are excluded (member completion resolves them
    /// separately). The 5-tuple mirrors the `NameTable` entry shape.
    #[allow(clippy::type_complexity)]
    pub fn load_symbol_names_with_paths(
        &self,
    ) -> Result<Vec<(i64, String, bool, String, String, bool)>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.name, f.source, f.path, s.kind, f.directly_included FROM symbols s JOIN files f ON f.id = s.file_id \
             WHERE s.kind != 'field'",
        )?;
        let rows = stmt.query_map([], |row| {
            let source: String = row.get(2)?;
            Ok((
                row.get(0)?,
                row.get(1)?,
                source == "external",
                row.get(3)?,
                row.get(4)?,
                row.get::<_, i64>(5)? != 0,
            ))
        })?;
        let mut names = Vec::new();
        for row in rows {
            names.push(row?);
        }
        Ok(names)
    }

    #[allow(clippy::type_complexity)]
    pub fn load_symbol_names_for_paths(
        &self,
        paths: &[String],
    ) -> Result<Vec<(i64, String, bool, String, String, bool)>> {
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
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(
                rusqlite::params_from_iter(chunk.iter().map(String::as_str)),
                |row| {
                    let source: String = row.get(2)?;
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        source == "external",
                        row.get(3)?,
                        row.get(4)?,
                        row.get::<_, i64>(5)? != 0,
                    ))
                },
            )?;
            for row in rows {
                names.push(row?);
            }
        }

        Ok(names)
    }

    /// Degraded member-completion fallback used when receiver inference fails:
    /// indexed field names matching `prefix` (SQL `LIKE 'prefix%'`, no
    /// subsequence), each paired with the highest [`ScopeTier`](crate::model::ScopeTier)
    /// of any owning record under `ctx`. Tier-then-frequency ranked, capped at
    /// `limit`. This is a best-effort candidate set across record identities,
    /// not a claim that the names share one owner.
    pub fn fallback_field_candidates(
        &self,
        prefix: &str,
        limit: usize,
        ctx: Option<&crate::resolver::ResolveContext<'_>>,
    ) -> Result<Vec<(String, crate::model::ScopeTier)>> {
        let pattern = format!("{}%", prefix.replace('%', "\\%").replace('_', "\\_"));
        let mut stmt = self.conn.prepare(
            "SELECT fi.name, f.path, f.source, f.directly_included \
             FROM fields fi \
             JOIN record_defs r ON r.id = fi.record_id \
             JOIN files f ON f.id = r.file_id \
             WHERE fi.name LIKE ?1 ESCAPE '\\' COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([pattern], |row| {
            let source_str: String = row.get(2)?;
            let directly_included: i64 = row.get(3)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                source_str == "external",
                directly_included != 0,
            ))
        })?;

        struct FieldMeta {
            max_tier: crate::model::ScopeTier,
            freq: usize,
        }
        let mut map: HashMap<String, FieldMeta> = HashMap::new();

        for r in rows {
            let (name, path, external, directly_included) = r?;
            let tier = crate::resolver::scope_tier(&path, external, directly_included, ctx);
            let entry = map.entry(name).or_insert(FieldMeta {
                max_tier: crate::model::ScopeTier::Global,
                freq: 0,
            });
            entry.freq += 1;
            if tier.rank() > entry.max_tier.rank() {
                entry.max_tier = tier;
            }
        }

        let mut sorted: Vec<(String, FieldMeta)> = map.into_iter().collect();
        sorted.sort_by(|a, b| {
            let tier_cmp = b.1.max_tier.rank().cmp(&a.1.max_tier.rank());
            if tier_cmp != std::cmp::Ordering::Equal {
                tier_cmp
            } else {
                b.1.freq.cmp(&a.1.freq).then_with(|| a.0.cmp(&b.0))
            }
        });

        let names: Vec<(String, crate::model::ScopeTier)> = sorted
            .into_iter()
            .take(limit)
            .map(|(name, meta)| (name, meta.max_tier))
            .collect();
        Ok(names)
    }

    /// Scoped record/alias candidate lookup: the single production entry point
    /// for resolving a receiver type name (record tag/display/typedef or typedef
    /// alias) to record identities. Replaces the old global `resolve_alias` +
    /// `fields_by_record[_scoped]` string path. Direct record rows and alias
    /// rows are ranked by shared [`scope_tier`](crate::resolver::scope_tier)
    /// under `ctx`; alias rows resolve their `target_record_id` or recurse on
    /// `target_name` (cycle-guarded), never collapsing to one global winner.
    /// Same-tier candidates are kept; only exact duplicate record ids are
    /// deduped. Best-effort name candidates, not a semantic binding.
    pub fn resolve_record_candidates(
        &self,
        names: &[&str],
        ctx: Option<&crate::resolver::ResolveContext<'_>>,
    ) -> Result<Vec<RecordCandidate>> {
        let mut visited = HashSet::new();
        self.resolve_record_candidates_inner(names, ctx, &mut visited)
    }

    fn resolve_record_candidates_inner(
        &self,
        names: &[&str],
        ctx: Option<&crate::resolver::ResolveContext<'_>>,
        visited: &mut HashSet<String>,
    ) -> Result<Vec<RecordCandidate>> {
        let mut candidates = Vec::new();
        if names.is_empty() {
            return Ok(candidates);
        }

        let placeholders = vec!["?"; names.len()].join(",");
        let sql = format!(
            "SELECT r.id, r.display_name, r.tag_name, r.typedef_name, r.kind, f.path, f.source, f.directly_included, \
             r.start_byte, r.end_byte, r.start_line, r.start_col, r.end_line, r.end_col, r.confidence, r.signature \
             FROM record_defs r \
             JOIN files f ON f.id = r.file_id \
             WHERE r.display_name IN ({placeholders}) \
                OR r.tag_name IN ({placeholders}) \
                OR r.typedef_name IN ({placeholders})"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut params = Vec::new();
        for &name in names {
            params.push(name);
        }
        let mut all_params = Vec::new();
        for _ in 0..3 {
            for &p in &params {
                all_params.push(p);
            }
        }

        let rows = stmt.query_map(rusqlite::params_from_iter(all_params), |row| {
            let kind_str: String = row.get(4)?;
            let kind = match kind_str.as_str() {
                "union" => crate::parser::RecordKind::Union,
                "class" => crate::parser::RecordKind::Class,
                _ => crate::parser::RecordKind::Struct,
            };
            let source_str: String = row.get(6)?;
            let directly_included: i64 = row.get(7)?;
            let confidence_str: String = row.get(14)?;
            let confidence = match confidence_str.as_str() {
                "named_tag" => crate::parser::RecordConfidence::NamedTag,
                "anonymous_typedef" => crate::parser::RecordConfidence::AnonymousTypedef,
                _ => crate::parser::RecordConfidence::Heuristic,
            };

            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                kind,
                row.get::<_, String>(5)?,
                source_str == "external",
                directly_included != 0,
                row.get::<_, i64>(8)? as usize,
                row.get::<_, i64>(9)? as usize,
                row.get::<_, i64>(10)? as usize,
                row.get::<_, i64>(11)? as usize,
                row.get::<_, i64>(12)? as usize,
                row.get::<_, i64>(13)? as usize,
                confidence,
                row.get::<_, String>(15)?,
            ))
        })?;

        for r in rows {
            let (
                id,
                display_name,
                tag_name,
                typedef_name,
                kind,
                path,
                external,
                directly_included,
                start_byte,
                end_byte,
                start_line,
                start_col,
                end_line,
                end_col,
                confidence,
                signature,
            ) = r?;

            let tier = crate::resolver::scope_tier(&path, external, directly_included, ctx);

            candidates.push(RecordCandidate {
                id,
                display_name,
                tag_name,
                typedef_name,
                kind,
                path,
                start_byte,
                end_byte,
                start_line,
                start_col,
                end_line,
                end_col,
                confidence,
                signature,
                tier,
            });
        }
        drop(stmt);

        let sql_aliases = format!(
            "SELECT a.alias, f.path, f.source, f.directly_included, \
             a.target_record_id, a.target_name, a.target_kind \
             FROM type_aliases a \
             JOIN files f ON f.id = a.file_id \
             WHERE a.alias IN ({placeholders})"
        );
        let mut stmt_aliases = self.conn.prepare(&sql_aliases)?;
        let alias_rows = stmt_aliases.query_map(rusqlite::params_from_iter(params), |row| {
            let source_str: String = row.get(2)?;
            let directly_included: i64 = row.get(3)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                source_str == "external",
                directly_included != 0,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?;

        for r in alias_rows {
            let (
                _alias,
                path,
                external,
                directly_included,
                target_record_id,
                target_name,
                target_kind,
            ) = r?;
            let alias_tier = crate::resolver::scope_tier(&path, external, directly_included, ctx);

            if let Some(trid) = target_record_id {
                let rec_opt = self.fetch_record_by_id(trid, ctx)?;
                if let Some(mut rec) = rec_opt {
                    rec.tier = alias_tier;
                    candidates.push(rec);
                }
            } else if let Some(tname) = target_name {
                if visited.insert(tname.clone()) {
                    let resolved = self.resolve_record_candidates_inner(&[&tname], ctx, visited)?;
                    for mut rec in resolved {
                        if let Some(kind) = target_kind.as_deref().and_then(record_kind_from_str) {
                            if rec.kind != kind {
                                continue;
                            }
                        }
                        let min_rank = alias_tier.rank().min(rec.tier.rank());
                        rec.tier = match min_rank {
                            4 => crate::model::ScopeTier::Current,
                            3 => crate::model::ScopeTier::Reachable,
                            2 => crate::model::ScopeTier::External,
                            1 => crate::model::ScopeTier::Unknown,
                            _ => crate::model::ScopeTier::Global,
                        };
                        candidates.push(rec);
                    }
                    visited.remove(&tname);
                }
            }
        }

        candidates.sort_by(|a, b| {
            let tier_order = b.tier.rank().cmp(&a.tier.rank());
            if tier_order != std::cmp::Ordering::Equal {
                tier_order
            } else {
                a.id.cmp(&b.id)
            }
        });
        candidates.dedup_by_key(|c| c.id);

        Ok(candidates)
    }

    fn fetch_record_by_id(
        &self,
        record_id: i64,
        ctx: Option<&crate::resolver::ResolveContext<'_>>,
    ) -> Result<Option<RecordCandidate>> {
        self.conn.query_row(
            "SELECT r.id, r.display_name, r.tag_name, r.typedef_name, r.kind, f.path, f.source, f.directly_included, \
             r.start_byte, r.end_byte, r.start_line, r.start_col, r.end_line, r.end_col, r.confidence, r.signature \
             FROM record_defs r \
             JOIN files f ON f.id = r.file_id \
             WHERE r.id = ?1",
            [record_id],
            |row| {
                let kind_str: String = row.get(4)?;
                let kind = match kind_str.as_str() {
                    "union" => crate::parser::RecordKind::Union,
                    "class" => crate::parser::RecordKind::Class,
                    _ => crate::parser::RecordKind::Struct,
                };
                let source_str: String = row.get(6)?;
                let directly_included: i64 = row.get(7)?;
                let confidence_str: String = row.get(14)?;
                let confidence = match confidence_str.as_str() {
                    "named_tag" => crate::parser::RecordConfidence::NamedTag,
                    "anonymous_typedef" => crate::parser::RecordConfidence::AnonymousTypedef,
                    _ => crate::parser::RecordConfidence::Heuristic,
                };

                let path: String = row.get(5)?;
                let external = source_str == "external";
                let directly_included_bool = directly_included != 0;
                let tier = crate::resolver::scope_tier(&path, external, directly_included_bool, ctx);

                Ok(RecordCandidate {
                    id: row.get::<_, i64>(0)?,
                    display_name: row.get::<_, String>(1)?,
                    tag_name: row.get::<_, Option<String>>(2)?,
                    typedef_name: row.get::<_, Option<String>>(3)?,
                    kind,
                    path,
                    start_byte: row.get::<_, i64>(8)? as usize,
                    end_byte: row.get::<_, i64>(9)? as usize,
                    start_line: row.get::<_, i64>(10)? as usize,
                    start_col: row.get::<_, i64>(11)? as usize,
                    end_line: row.get::<_, i64>(12)? as usize,
                    end_col: row.get::<_, i64>(13)? as usize,
                    confidence,
                    signature: row.get::<_, String>(15)?,
                    tier,
                })
            }
        ).optional().context("failed to fetch record by id")
    }

    pub fn fields_for_records(&self, record_ids: &[i64]) -> Result<Vec<String>> {
        if record_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = vec!["?"; record_ids.len()].join(",");
        let sql = format!(
            "SELECT name FROM fields \
             WHERE record_id IN ({placeholders}) \
             GROUP BY name"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(record_ids.iter().copied()),
            |row| row.get::<_, String>(0),
        )?;
        let mut names = Vec::new();
        for row in rows {
            names.push(row?);
        }
        Ok(names)
    }

    /// Fetch full records for the given symbol ids, preserving caller order.
    /// Missing ids are silently omitted.
    pub fn symbols_by_ids(&self, ids: &[i64]) -> Result<Vec<SymbolRecord>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        // Load in chunks via IN-list, index by id, then emit in caller order.
        let mut id_to_record: HashMap<i64, SymbolRecord> = HashMap::new();
        for chunk in ids.chunks(400) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!("{SELECT_SYMBOL_JOIN} WHERE s.id IN ({placeholders})");
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(
                rusqlite::params_from_iter(chunk.iter().copied()),
                map_symbol_record,
            )?;
            for row in rows {
                let record = row?;
                id_to_record.insert(record.id, record);
            }
        }

        let records: Vec<SymbolRecord> = ids
            .iter()
            .filter_map(|id| id_to_record.get(id).cloned())
            .collect();
        Ok(records)
    }

    /// Count, per name, how many *definitions* of each kind exist in the index.
    ///
    /// Returns `name -> (kind string -> definition count)`. Production coloring
    /// resolves kinds from the in-memory `NameTable`
    /// (`query::NameTable::colorable_kind_counts`); this SQL form is retained
    /// only as the parity oracle for that path's tests.
    #[cfg(test)]
    pub fn kind_counts_by_names(
        &self,
        names: &[&str],
    ) -> Result<HashMap<String, HashMap<String, usize>>> {
        let mut counts: HashMap<String, HashMap<String, usize>> = HashMap::new();
        if names.is_empty() {
            return Ok(counts);
        }

        // SQLite caps bound variables (default 999); chunk well under that.
        for chunk in names.chunks(400) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            // Coloring only considers workspace symbols and first-layer external
            // headers (`directly_included`): a toolchain's transitive include
            // closure must not skew the multi-meaning tie-break.
            let sql = format!(
                "SELECT s.name, s.kind, COUNT(*) FROM symbols s \
                 JOIN files f ON f.id = s.file_id \
                 WHERE s.role = 'definition' AND s.name IN ({placeholders}) \
                 AND (f.source = 'workspace' OR f.directly_included = 1) \
                 GROUP BY s.name, s.kind"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows =
                stmt.query_map(rusqlite::params_from_iter(chunk.iter().copied()), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)? as usize,
                    ))
                })?;
            for row in rows {
                let (name, kind, count) = row?;
                counts.entry(name).or_default().insert(kind, count);
            }
        }

        Ok(counts)
    }

    /// Fetch all symbols with an exact name (definition candidate set).
    pub fn symbols_by_name(&self, name: &str) -> Result<Vec<SymbolRecord>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{SELECT_SYMBOL_JOIN} WHERE s.name = ?1"))?;
        let rows = stmt.query_map([name], map_symbol_record)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }
}
