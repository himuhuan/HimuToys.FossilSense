#![allow(dead_code)] // The service consumer lands after the durable fact contract.

use anyhow::Result;

use crate::call_model::{SourcePosition, SourceRange};

use super::super::IndexStore;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallableAnchorRow {
    pub id: i64,
    pub path: String,
    pub source: String,
    pub entity_key: String,
    pub anchor_fingerprint: String,
    pub name: String,
    pub qualified_name: String,
    pub owner: Option<String>,
    pub owner_kind: Option<String>,
    pub kind: String,
    pub role: String,
    pub linkage_kind: String,
    pub linkage_file: Option<String>,
    pub signature: String,
    pub min_arity: Option<u32>,
    pub max_arity: Option<u32>,
    pub variadic: bool,
    pub name_range: SourceRange,
    pub declaration_range: SourceRange,
    pub body_range: Option<SourceRange>,
    pub guard: Option<String>,
    pub provenance: String,
    pub syntax_error_overlap: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSiteRow {
    pub id: i64,
    pub path: String,
    pub source: String,
    pub caller_entity_key: String,
    pub site_fingerprint: String,
    pub expression_range: SourceRange,
    pub callee_range: SourceRange,
    pub callee_name: Option<String>,
    pub qualified_name: Option<String>,
    pub call_form: String,
    pub argument_count: Option<u32>,
    pub guard: Option<String>,
    pub provenance: String,
    pub syntax_error_overlap: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallCoverageRow {
    pub eligible_files: u64,
    pub analyzed_files: u64,
    pub fallback_files: u64,
    pub callable_anchors: u64,
    pub call_sites: u64,
}

pub struct CallFactStoreView<'a> {
    store: &'a IndexStore,
}

impl<'a> CallFactStoreView<'a> {
    pub(in crate::store) fn new(store: &'a IndexStore) -> Self {
        Self { store }
    }

    #[cfg(test)]
    pub fn all_anchors(&self) -> Result<Vec<CallableAnchorRow>> {
        self.anchor_query("", [])
    }

    #[cfg(test)]
    pub fn all_call_sites(&self) -> Result<Vec<CallSiteRow>> {
        self.call_site_query("", [])
    }

    #[cfg(test)]
    pub fn visit_all_anchors(
        &self,
        visitor: impl FnMut(CallableAnchorRow) -> Result<()>,
    ) -> Result<()> {
        self.visit_anchors("", &[], visitor)
    }

    #[cfg(test)]
    pub fn visit_all_call_sites(
        &self,
        visitor: impl FnMut(CallSiteRow) -> Result<()>,
    ) -> Result<()> {
        self.visit_call_sites("", [], None, visitor)
    }

    pub fn anchors_by_name(&self, name: &str) -> Result<Vec<CallableAnchorRow>> {
        self.anchor_query("WHERE a.name = ?1", [name])
    }

    pub fn anchors_by_entity_key(&self, entity_key: &str) -> Result<Vec<CallableAnchorRow>> {
        self.anchor_query("WHERE a.entity_key = ?1", [entity_key])
    }

    pub fn anchors_by_path(&self, path: &str) -> Result<Vec<CallableAnchorRow>> {
        self.anchor_query("WHERE f.path = ?1", [path])
    }

    pub fn anchors_at(
        &self,
        path: &str,
        line: u32,
        character: u32,
    ) -> Result<Vec<CallableAnchorRow>> {
        let line = line.to_string();
        let character = character.to_string();
        self.anchor_query(
            "WHERE a.revision_id = (
                SELECT active.revision_id FROM active_file_revisions active
                JOIN file_entries entry ON entry.id = active.file_id
                WHERE entry.path = ?1
             ) AND (
                (a.name_start_line = ?2 AND a.name_end_line = ?2
                 AND a.name_start_col <= ?3 AND a.name_end_col >= ?3)
                OR (a.declaration_start_line <= ?2 AND a.declaration_end_line >= ?2)
                OR (a.body_start_line <= ?2 AND a.body_end_line >= ?2)
            )",
            [path, line.as_str(), character.as_str()],
        )
    }

    pub fn anchors_by_names(&self, names: &[String]) -> Result<Vec<CallableAnchorRow>> {
        self.anchors_by_values("a.name", names)
    }

    pub fn anchors_by_entity_keys(&self, keys: &[String]) -> Result<Vec<CallableAnchorRow>> {
        self.anchors_by_values("a.entity_key", keys)
    }

    pub fn call_sites_by_caller(&self, entity_key: &str) -> Result<Vec<CallSiteRow>> {
        self.call_site_query("WHERE c.caller_entity_key = ?1", [entity_key])
    }

    pub fn call_sites_by_caller_limited(
        &self,
        entity_key: &str,
        limit: usize,
    ) -> Result<(Vec<CallSiteRow>, bool)> {
        self.call_site_query_limited("WHERE c.caller_entity_key = ?1", [entity_key], limit)
    }

    pub fn call_sites_by_callee(&self, name: &str) -> Result<Vec<CallSiteRow>> {
        self.call_site_query("WHERE c.callee_name = ?1", [name])
    }

    pub fn call_sites_by_callee_limited(
        &self,
        name: &str,
        limit: usize,
    ) -> Result<(Vec<CallSiteRow>, bool)> {
        self.call_site_query_limited("WHERE c.callee_name = ?1", [name], limit)
    }

    pub fn call_sites_by_path(&self, path: &str) -> Result<Vec<CallSiteRow>> {
        self.call_site_query("WHERE f.path = ?1", [path])
    }

    pub fn call_sites_at(&self, path: &str, line: u32, character: u32) -> Result<Vec<CallSiteRow>> {
        let line = line.to_string();
        let character = character.to_string();
        self.call_site_query(
            "WHERE c.revision_id = (
                SELECT active.revision_id FROM active_file_revisions active
                JOIN file_entries entry ON entry.id = active.file_id
                WHERE entry.path = ?1
             ) AND c.callee_start_line = ?2
             AND c.callee_end_line = ?2 AND c.callee_start_col <= ?3
             AND c.callee_end_col >= ?3",
            [path, line.as_str(), character.as_str()],
        )
    }

    fn anchors_by_values(&self, column: &str, values: &[String]) -> Result<Vec<CallableAnchorRow>> {
        let mut output = Vec::new();
        for chunk in values.chunks(400) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let predicate = format!("WHERE {column} IN ({placeholders})");
            let params: Vec<&str> = chunk.iter().map(String::as_str).collect();
            self.visit_anchors(&predicate, &params, |row| {
                output.push(row);
                Ok(())
            })?;
        }
        Ok(output)
    }

    pub fn coverage(&self) -> Result<CallCoverageRow> {
        self.store
            .conn
            .query_row(
                "SELECT
                COUNT(*),
                SUM(CASE WHEN r.fallback_used = 0 AND (r.fact_mask & 128) != 0 THEN 1 ELSE 0 END),
                SUM(CASE WHEN r.fallback_used != 0 THEN 1 ELSE 0 END),
                (SELECT COUNT(*) FROM callable_anchors),
                (SELECT COUNT(*) FROM call_sites)
             FROM active_file_revisions a
             JOIN file_revisions r ON r.id = a.revision_id",
                [],
                |row| {
                    Ok(CallCoverageRow {
                        eligible_files: row.get::<_, i64>(0)? as u64,
                        analyzed_files: row.get::<_, Option<i64>>(1)?.unwrap_or(0) as u64,
                        fallback_files: row.get::<_, Option<i64>>(2)?.unwrap_or(0) as u64,
                        callable_anchors: row.get::<_, i64>(3)? as u64,
                        call_sites: row.get::<_, i64>(4)? as u64,
                    })
                },
            )
            .map_err(Into::into)
    }

    /// Request-path coverage avoids counting every active call fact. Exact fact
    /// totals are catalog-build diagnostics, not a prerequisite for one-hop
    /// coverage and would turn each lazy query back into an O(workspace) scan.
    pub fn request_coverage(&self) -> Result<CallCoverageRow> {
        self.store
            .conn
            .query_row(
                "SELECT COUNT(*),
                        SUM(CASE WHEN r.fallback_used = 0 AND (r.fact_mask & 128) != 0 THEN 1 ELSE 0 END),
                        SUM(CASE WHEN r.fallback_used != 0 THEN 1 ELSE 0 END)
                 FROM active_file_revisions a
                 JOIN file_revisions r ON r.id = a.revision_id",
                [],
                |row| {
                    Ok(CallCoverageRow {
                        eligible_files: row.get::<_, i64>(0)? as u64,
                        analyzed_files: row.get::<_, Option<i64>>(1)?.unwrap_or(0) as u64,
                        fallback_files: row.get::<_, Option<i64>>(2)?.unwrap_or(0) as u64,
                        callable_anchors: 0,
                        call_sites: 0,
                    })
                },
            )
            .map_err(Into::into)
    }

    fn anchor_query<const N: usize>(
        &self,
        predicate: &str,
        params: [&str; N],
    ) -> Result<Vec<CallableAnchorRow>> {
        let mut output = Vec::new();
        self.visit_anchors(predicate, &params, |row| {
            output.push(row);
            Ok(())
        })?;
        Ok(output)
    }

    fn visit_anchors(
        &self,
        predicate: &str,
        params: &[&str],
        mut visitor: impl FnMut(CallableAnchorRow) -> Result<()>,
    ) -> Result<()> {
        let sql = format!(
            "SELECT a.id, f.path, f.source, a.entity_key, a.anchor_fingerprint,
                    a.name, a.qualified_name, a.owner, a.owner_kind, a.kind, a.role,
                    a.linkage_kind, a.linkage_file, a.signature, a.min_arity, a.max_arity,
                    a.variadic, a.name_start_byte, a.name_end_byte, a.name_start_line,
                    a.name_start_col, a.name_end_line, a.name_end_col,
                    a.declaration_start_byte, a.declaration_end_byte,
                    a.declaration_start_line, a.declaration_start_col,
                    a.declaration_end_line, a.declaration_end_col,
                    a.body_start_byte, a.body_end_byte, a.body_start_line,
                    a.body_start_col, a.body_end_line, a.body_end_col,
                    a.guard, a.provenance, a.syntax_error_overlap
             FROM callable_anchors a JOIN files f ON f.id = a.file_id {predicate}
             ORDER BY a.qualified_name, f.path, a.name_start_byte"
        );
        let mut stmt = self.store.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(params.iter().copied()),
            map_anchor,
        )?;
        for row in rows {
            visitor(row?)?;
        }
        Ok(())
    }

    fn call_site_query<const N: usize>(
        &self,
        predicate: &str,
        params: [&str; N],
    ) -> Result<Vec<CallSiteRow>> {
        let mut output = Vec::new();
        self.visit_call_sites(predicate, params, None, |row| {
            output.push(row);
            Ok(())
        })?;
        Ok(output)
    }

    fn call_site_query_limited<const N: usize>(
        &self,
        predicate: &str,
        params: [&str; N],
        limit: usize,
    ) -> Result<(Vec<CallSiteRow>, bool)> {
        let mut output = Vec::new();
        self.visit_call_sites(predicate, params, Some(limit.saturating_add(1)), |row| {
            output.push(row);
            Ok(())
        })?;
        let limited = output.len() > limit;
        output.truncate(limit);
        Ok((output, limited))
    }

    fn visit_call_sites<const N: usize>(
        &self,
        predicate: &str,
        params: [&str; N],
        limit: Option<usize>,
        mut visitor: impl FnMut(CallSiteRow) -> Result<()>,
    ) -> Result<()> {
        let limit_clause = limit.map_or_else(String::new, |limit| format!(" LIMIT {limit}"));
        let sql = format!(
            "SELECT c.id, f.path, f.source, c.caller_entity_key, c.site_fingerprint,
                    c.expression_start_byte, c.expression_end_byte, c.expression_start_line,
                    c.expression_start_col, c.expression_end_line, c.expression_end_col,
                    c.callee_start_byte, c.callee_end_byte, c.callee_start_line,
                    c.callee_start_col, c.callee_end_line, c.callee_end_col,
                    c.callee_name, c.qualified_name, c.call_form, c.argument_count,
                    c.guard, c.provenance, c.syntax_error_overlap
             FROM call_sites c JOIN files f ON f.id = c.file_id {predicate}
             ORDER BY c.id{limit_clause}"
        );
        let mut stmt = self.store.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params), map_call_site)?;
        for row in rows {
            visitor(row?)?;
        }
        Ok(())
    }
}

fn map_anchor(row: &rusqlite::Row<'_>) -> rusqlite::Result<CallableAnchorRow> {
    Ok(CallableAnchorRow {
        id: row.get(0)?,
        path: row.get(1)?,
        source: row.get(2)?,
        entity_key: row.get(3)?,
        anchor_fingerprint: row.get(4)?,
        name: row.get(5)?,
        qualified_name: row.get(6)?,
        owner: row.get(7)?,
        owner_kind: row.get(8)?,
        kind: row.get(9)?,
        role: row.get(10)?,
        linkage_kind: row.get(11)?,
        linkage_file: row.get(12)?,
        signature: row.get(13)?,
        min_arity: row.get::<_, Option<i64>>(14)?.map(|value| value as u32),
        max_arity: row.get::<_, Option<i64>>(15)?.map(|value| value as u32),
        variadic: row.get::<_, i64>(16)? != 0,
        name_range: range(row, 17)?,
        declaration_range: range(row, 23)?,
        body_range: optional_range(row, 29)?,
        guard: row.get(35)?,
        provenance: row.get(36)?,
        syntax_error_overlap: row.get::<_, i64>(37)? != 0,
    })
}

fn map_call_site(row: &rusqlite::Row<'_>) -> rusqlite::Result<CallSiteRow> {
    Ok(CallSiteRow {
        id: row.get(0)?,
        path: row.get(1)?,
        source: row.get(2)?,
        caller_entity_key: row.get(3)?,
        site_fingerprint: row.get(4)?,
        expression_range: range(row, 5)?,
        callee_range: range(row, 11)?,
        callee_name: row.get(17)?,
        qualified_name: row.get(18)?,
        call_form: row.get(19)?,
        argument_count: row.get::<_, Option<i64>>(20)?.map(|value| value as u32),
        guard: row.get(21)?,
        provenance: row.get(22)?,
        syntax_error_overlap: row.get::<_, i64>(23)? != 0,
    })
}

fn range(row: &rusqlite::Row<'_>, start: usize) -> rusqlite::Result<SourceRange> {
    Ok(SourceRange {
        start_byte: row.get::<_, i64>(start)? as usize,
        end_byte: row.get::<_, i64>(start + 1)? as usize,
        start: SourcePosition {
            line: row.get::<_, i64>(start + 2)? as u32,
            character: row.get::<_, i64>(start + 3)? as u32,
        },
        end: SourcePosition {
            line: row.get::<_, i64>(start + 4)? as u32,
            character: row.get::<_, i64>(start + 5)? as u32,
        },
    })
}

fn optional_range(row: &rusqlite::Row<'_>, start: usize) -> rusqlite::Result<Option<SourceRange>> {
    let Some(start_byte) = row.get::<_, Option<i64>>(start)? else {
        return Ok(None);
    };
    Ok(Some(SourceRange {
        start_byte: start_byte as usize,
        end_byte: row.get::<_, i64>(start + 1)? as usize,
        start: SourcePosition {
            line: row.get::<_, i64>(start + 2)? as u32,
            character: row.get::<_, i64>(start + 3)? as u32,
        },
        end: SourcePosition {
            line: row.get::<_, i64>(start + 4)? as u32,
            character: row.get::<_, i64>(start + 5)? as u32,
        },
    }))
}
