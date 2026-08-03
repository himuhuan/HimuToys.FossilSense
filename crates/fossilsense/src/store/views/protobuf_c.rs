use std::collections::HashSet;

use anyhow::Result;
use rusqlite::types::Value;

use crate::store::IndexStore;

pub const MAX_PROTOBUF_C_SOURCE_QUERY_LIMIT: usize = 64;
/// Covers the full production exact-name candidate window so a source on the
/// last retained semantic candidate is not silently skipped.
pub const MAX_PROTOBUF_C_SOURCE_DECLARATION_IDS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtobufCSourceReadRow {
    pub proto_path: String,
    pub proto_name: String,
    pub c_name: String,
    pub kind: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
    pub match_kind: String,
}

pub struct ProtobufCSourceStoreView<'a> {
    store: &'a IndexStore,
}

impl<'a> ProtobufCSourceStoreView<'a> {
    pub(crate) fn new(store: &'a IndexStore) -> Self {
        Self { store }
    }

    pub fn sources_for_declaration_ids(
        &self,
        declaration_ids: &[i64],
        requested_limit: usize,
    ) -> Result<(Vec<ProtobufCSourceReadRow>, bool)> {
        let limit = requested_limit.min(MAX_PROTOBUF_C_SOURCE_QUERY_LIMIT);
        if limit == 0 || declaration_ids.is_empty() {
            return Ok((Vec::new(), false));
        }
        let mut seen_ids = HashSet::new();
        let ids: Vec<_> = declaration_ids
            .iter()
            .copied()
            .filter(|id| seen_ids.insert(*id))
            .take(MAX_PROTOBUF_C_SOURCE_DECLARATION_IDS + 1)
            .collect();
        let mut truncated = ids.len() > MAX_PROTOBUF_C_SOURCE_DECLARATION_IDS;
        let ids = &ids[..ids.len().min(MAX_PROTOBUF_C_SOURCE_DECLARATION_IDS)];
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = format!(
            "SELECT DISTINCT s.proto_path, s.proto_name, s.c_name, s.kind,
                    s.start_byte, s.end_byte, s.start_line, s.start_col,
                    s.end_line, s.end_col, s.match_kind, s.source_truncated
             FROM protobuf_c_sources s
             JOIN declarations d ON d.id = s.declaration_id
             WHERE s.declaration_id IN ({placeholders})
             ORDER BY CASE s.match_kind WHEN 'relative_path' THEN 0 ELSE 1 END,
                      lower(s.proto_path), s.proto_path, s.start_line, s.start_col,
                      s.proto_name
             LIMIT ?"
        );
        let mut parameters: Vec<Value> = ids.iter().copied().map(Value::Integer).collect();
        parameters.push(Value::Integer((limit + 1) as i64));
        let mut statement = self.store.conn.prepare(&sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(parameters), |row| {
            Ok((
                ProtobufCSourceReadRow {
                    proto_path: row.get(0)?,
                    proto_name: row.get(1)?,
                    c_name: row.get(2)?,
                    kind: row.get(3)?,
                    start_byte: row.get::<_, i64>(4)?.max(0) as usize,
                    end_byte: row.get::<_, i64>(5)?.max(0) as usize,
                    start_line: row.get::<_, i64>(6)?.max(0) as u32,
                    start_col: row.get::<_, i64>(7)?.max(0) as u32,
                    end_line: row.get::<_, i64>(8)?.max(0) as u32,
                    end_col: row.get::<_, i64>(9)?.max(0) as u32,
                    match_kind: row.get(10)?,
                },
                row.get::<_, i64>(11)? != 0,
            ))
        })?;
        let mut sources = Vec::new();
        for row in rows {
            let (source, source_truncated) = row?;
            truncated |= source_truncated;
            sources.push(source);
        }
        if sources.len() > limit {
            truncated = true;
            sources.truncate(limit);
        }
        Ok((sources, truncated))
    }
}
