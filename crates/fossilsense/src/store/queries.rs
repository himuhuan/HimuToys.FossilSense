#[cfg(test)]
use std::collections::HashMap;

use anyhow::Result;

use crate::model::{MemberCandidate, RecordCandidate};

use super::{IndexStore, SymbolRecord};

impl IndexStore {
    /// Load every symbol id + name (+ external flag) for building the in-memory
    /// fuzzy name table.
    ///
    /// Compatibility wrapper: the typed contract lives in
    /// [`crate::store::views::NameTableStoreView`].
    #[allow(dead_code)]
    pub fn load_symbol_names(&self) -> Result<Vec<(i64, String, bool)>> {
        self.name_table_view().symbol_name_rows()
    }

    /// Load every symbol id + name (+ external flag, path, kind) for building
    /// the in-memory fuzzy `NameTable` with per-symbol kind cached.
    ///
    /// Compatibility wrapper around the typed name-table read view.
    #[allow(clippy::type_complexity)]
    #[allow(dead_code)]
    pub fn load_symbol_names_with_paths(
        &self,
    ) -> Result<Vec<(i64, String, bool, String, String, bool)>> {
        self.name_table_view().symbol_rows().map(|rows| {
            rows.into_iter()
                .map(crate::store::views::NameTableSymbolRow::into_legacy_tuple)
                .collect()
        })
    }

    #[allow(clippy::type_complexity)]
    #[allow(dead_code)]
    pub fn load_symbol_names_for_paths(
        &self,
        paths: &[String],
    ) -> Result<Vec<(i64, String, bool, String, String, bool)>> {
        self.name_table_view()
            .symbol_rows_for_paths(paths)
            .map(|rows| {
                rows.into_iter()
                    .map(crate::store::views::NameTableSymbolRow::into_legacy_tuple)
                    .collect()
            })
    }

    /// Degraded member-completion fallback used when receiver inference fails.
    ///
    /// Compatibility wrapper around [`crate::store::views::MemberStoreView`].
    #[allow(dead_code)]
    pub fn fallback_field_candidates(
        &self,
        prefix: &str,
        limit: usize,
        ctx: Option<&crate::resolver::ResolveContext<'_>>,
    ) -> Result<Vec<(String, crate::model::ScopeTier)>> {
        self.member_view()
            .fallback_field_candidates(prefix, limit, ctx)
    }

    /// Scoped record/alias candidate lookup.
    ///
    /// Compatibility wrapper around [`crate::store::views::MemberStoreView`].
    #[allow(dead_code)]
    pub fn resolve_record_candidates(
        &self,
        names: &[&str],
        ctx: Option<&crate::resolver::ResolveContext<'_>>,
    ) -> Result<Vec<RecordCandidate>> {
        self.member_view().resolve_record_candidates(names, ctx)
    }

    #[allow(dead_code)]
    pub fn members_for_records(
        &self,
        record_ids: &[i64],
        prefix: Option<&str>,
        ctx: Option<&crate::resolver::ResolveContext<'_>>,
    ) -> Result<Vec<MemberCandidate>> {
        self.member_view()
            .members_for_records(record_ids, prefix, ctx)
    }

    #[allow(dead_code)]
    pub fn fallback_member_candidates(
        &self,
        prefix: &str,
        limit: usize,
        ctx: Option<&crate::resolver::ResolveContext<'_>>,
    ) -> Result<Vec<MemberCandidate>> {
        self.member_view()
            .fallback_member_candidates(prefix, limit, ctx)
    }

    #[allow(dead_code)]
    pub fn fields_for_records(&self, record_ids: &[i64]) -> Result<Vec<String>> {
        let mut names: Vec<String> = self
            .members_for_records(record_ids, None, None)?
            .into_iter()
            .filter(|member| member.kind == crate::parser::MemberKind::Field)
            .map(|member| member.name)
            .collect();
        names.sort();
        names.dedup();
        Ok(names)
    }

    /// Fetch full records for the given symbol ids, preserving caller order.
    /// Missing ids are silently omitted.
    #[allow(dead_code)]
    pub fn symbols_by_ids(&self, ids: &[i64]) -> Result<Vec<SymbolRecord>> {
        self.symbol_read_view().symbols_by_ids(ids)
    }

    /// Count, per name, how many *definitions* of each kind exist in the index.
    ///
    /// Returns `name -> (kind string -> definition count)`. Production coloring
    /// resolves kinds from the in-memory `NameTable`; this SQL form is retained
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

        for chunk in names.chunks(400) {
            let placeholders = vec!["?"; chunk.len()].join(",");
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
    #[allow(dead_code)]
    pub fn symbols_by_name(&self, name: &str) -> Result<Vec<SymbolRecord>> {
        self.symbol_read_view().symbols_by_name(name)
    }
}
