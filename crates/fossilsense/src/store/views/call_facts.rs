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

    pub fn anchors_by_entity_key(&self, entity_key: &str) -> Result<Vec<CallableAnchorRow>> {
        self.anchor_query("WHERE a.entity_digest = unhex(?1)", [entity_key])
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
        self.anchors_by_values("name_text.text", names, false)
    }

    pub fn anchors_by_entity_keys(&self, keys: &[String]) -> Result<Vec<CallableAnchorRow>> {
        self.anchors_by_values("a.entity_digest", keys, true)
    }

    pub fn call_sites_by_caller_limited(
        &self,
        entity_key: &str,
        limit: usize,
    ) -> Result<(Vec<CallSiteRow>, bool)> {
        self.call_site_query_limited(
            "WHERE caller.entity_digest = unhex(?1)",
            [entity_key],
            limit,
        )
    }

    pub fn call_sites_by_callee_limited(
        &self,
        name: &str,
        limit: usize,
    ) -> Result<(Vec<CallSiteRow>, bool)> {
        self.call_site_query_limited(
            "WHERE c.callee_name_id = (SELECT id FROM call_strings WHERE text = ?1)",
            [name],
            limit,
        )
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

    fn anchors_by_values(
        &self,
        column: &str,
        values: &[String],
        hex_values: bool,
    ) -> Result<Vec<CallableAnchorRow>> {
        let mut output = Vec::new();
        for chunk in values.chunks(400) {
            let placeholder = if hex_values { "unhex(?)" } else { "?" };
            let placeholders = vec![placeholder; chunk.len()].join(",");
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
            "SELECT a.id, f.path, f.source, lower(hex(a.entity_digest)),
                    lower(hex(a.anchor_digest)), name_text.text, qualified_text.text,
                    owner_text.text,
                    CASE a.owner_kind WHEN 1 THEN 'namespace' WHEN 2 THEN 'record'
                         WHEN 0 THEN 'unknown' END,
                    CASE a.kind WHEN 1 THEN 'synthetic_global_initializer'
                         WHEN 2 THEN 'synthetic_lambda' WHEN 3 THEN 'function_like_macro'
                         ELSE 'function' END,
                    CASE a.role WHEN 1 THEN 'definition' WHEN 2 THEN 'synthetic'
                         ELSE 'declaration' END,
                    CASE a.linkage_kind WHEN 1 THEN 'external' WHEN 2 THEN 'internal'
                         ELSE 'unknown' END,
                    linkage_text.text, signature_text.text, a.min_arity, a.max_arity,
                    a.variadic, a.name_start_byte, a.name_end_byte, a.name_start_line,
                    a.name_start_col, a.name_end_line, a.name_end_col,
                    a.declaration_start_byte, a.declaration_end_byte,
                    a.declaration_start_line, a.declaration_start_col,
                    a.declaration_end_line, a.declaration_end_col,
                    a.body_start_byte, a.body_end_byte, a.body_start_line,
                    a.body_start_col, a.body_end_line, a.body_end_col,
                    guard_text.text,
                    CASE (a.flags & 255) WHEN 1 THEN 'lexical_fallback'
                         WHEN 2 THEN 'synthetic' ELSE 'ast' END,
                    ((a.flags & 256) != 0)
             FROM callable_anchors a
             JOIN files f ON f.id = a.file_id
             JOIN call_strings name_text ON name_text.id = a.name_id
             JOIN call_strings qualified_text ON qualified_text.id = a.qualified_name_id
             JOIN call_strings signature_text ON signature_text.id = a.signature_id
             LEFT JOIN call_strings owner_text ON owner_text.id = a.owner_id
             LEFT JOIN call_strings linkage_text ON linkage_text.id = a.linkage_file_id
             LEFT JOIN call_strings guard_text ON guard_text.id = a.guard_id
             {predicate}
             ORDER BY qualified_text.text, f.path, a.name_start_byte"
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
            "SELECT c.id, f.path, f.source, lower(hex(caller.entity_digest)),
                    printf('%s@%d', f.path, c.callee_start_byte),
                    c.expression_start_byte, c.expression_end_byte, c.callee_start_line,
                    c.callee_start_col, c.callee_end_line, c.callee_end_col,
                    c.callee_start_byte, c.callee_end_byte, c.callee_start_line,
                    c.callee_start_col, c.callee_end_line, c.callee_end_col,
                    callee_text.text, qualified_text.text,
                    CASE c.call_form WHEN 0 THEN 'direct_name' WHEN 1 THEN 'qualified_name'
                         WHEN 2 THEN 'parenthesized_name' WHEN 3 THEN 'member_dot'
                         WHEN 4 THEN 'member_arrow' WHEN 5 THEN 'static_member'
                         WHEN 6 THEN 'function_pointer' WHEN 7 THEN 'callable_object'
                         WHEN 8 THEN 'explicit_construction' ELSE 'unsupported' END,
                    c.argument_count, guard_text.text,
                    CASE (c.flags & 255) WHEN 1 THEN 'lexical_fallback'
                         WHEN 2 THEN 'synthetic' ELSE 'ast' END,
                    ((c.flags & 256) != 0)
             FROM call_sites c
             JOIN files f ON f.id = c.file_id
             JOIN callable_anchor_facts caller ON caller.id = c.caller_anchor_id
             LEFT JOIN call_strings callee_text ON callee_text.id = c.callee_name_id
             LEFT JOIN call_strings qualified_text ON qualified_text.id = c.qualified_name_id
             LEFT JOIN call_strings guard_text ON guard_text.id = c.guard_id
             {predicate}
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
