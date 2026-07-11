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
    pub declaration_start_byte: usize,
    pub declaration_end_byte: usize,
    pub body_start_byte: Option<usize>,
    pub body_end_byte: Option<usize>,
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

    pub fn all_anchors(&self) -> Result<Vec<CallableAnchorRow>> {
        self.anchor_query("", [])
    }

    pub fn all_call_sites(&self) -> Result<Vec<CallSiteRow>> {
        self.call_site_query("", [])
    }

    pub fn anchors_by_name(&self, name: &str) -> Result<Vec<CallableAnchorRow>> {
        self.anchor_query("WHERE a.name = ?1", [name])
    }

    pub fn anchors_by_entity_key(&self, entity_key: &str) -> Result<Vec<CallableAnchorRow>> {
        self.anchor_query("WHERE a.entity_key = ?1", [entity_key])
    }

    pub fn call_sites_by_caller(&self, entity_key: &str) -> Result<Vec<CallSiteRow>> {
        self.call_site_query("WHERE c.caller_entity_key = ?1", [entity_key])
    }

    pub fn call_sites_by_callee(&self, name: &str) -> Result<Vec<CallSiteRow>> {
        self.call_site_query("WHERE c.callee_name = ?1", [name])
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

    fn anchor_query<const N: usize>(
        &self,
        predicate: &str,
        params: [&str; N],
    ) -> Result<Vec<CallableAnchorRow>> {
        let sql = format!(
            "SELECT a.id, f.path, f.source, a.entity_key, a.anchor_fingerprint,
                    a.name, a.qualified_name, a.owner, a.owner_kind, a.kind, a.role,
                    a.linkage_kind, a.linkage_file, a.signature, a.min_arity, a.max_arity,
                    a.variadic, a.name_start_byte, a.name_end_byte, a.name_start_line,
                    a.name_start_col, a.name_end_line, a.name_end_col,
                    a.declaration_start_byte, a.declaration_end_byte,
                    a.body_start_byte, a.body_end_byte, a.guard, a.provenance,
                    a.syntax_error_overlap
             FROM callable_anchors a JOIN files f ON f.id = a.file_id {predicate}
             ORDER BY a.qualified_name, f.path, a.name_start_byte"
        );
        let mut stmt = self.store.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params), map_anchor)?;
        collect(rows)
    }

    fn call_site_query<const N: usize>(
        &self,
        predicate: &str,
        params: [&str; N],
    ) -> Result<Vec<CallSiteRow>> {
        let sql = format!(
            "SELECT c.id, f.path, f.source, c.caller_entity_key, c.site_fingerprint,
                    c.expression_start_byte, c.expression_end_byte, c.expression_start_line,
                    c.expression_start_col, c.expression_end_line, c.expression_end_col,
                    c.callee_start_byte, c.callee_end_byte, c.callee_start_line,
                    c.callee_start_col, c.callee_end_line, c.callee_end_col,
                    c.callee_name, c.qualified_name, c.call_form, c.argument_count,
                    c.guard, c.provenance, c.syntax_error_overlap
             FROM call_sites c JOIN files f ON f.id = c.file_id {predicate}
             ORDER BY f.path, c.expression_start_byte"
        );
        let mut stmt = self.store.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params), map_call_site)?;
        collect(rows)
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
        declaration_start_byte: row.get::<_, i64>(23)? as usize,
        declaration_end_byte: row.get::<_, i64>(24)? as usize,
        body_start_byte: row.get::<_, Option<i64>>(25)?.map(|value| value as usize),
        body_end_byte: row.get::<_, Option<i64>>(26)?.map(|value| value as usize),
        guard: row.get(27)?,
        provenance: row.get(28)?,
        syntax_error_overlap: row.get::<_, i64>(29)? != 0,
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

fn collect<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>> {
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}
