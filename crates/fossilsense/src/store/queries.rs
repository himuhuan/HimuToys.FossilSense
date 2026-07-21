#[cfg(test)]
use std::collections::HashMap;

use anyhow::Result;

use crate::model::{MemberCandidate, RecordCandidate};

use super::IndexStore;

impl IndexStore {
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
            .filter(|member| member.kind == crate::semantic_model::MemberKind::Field)
            .map(|member| member.name)
            .collect();
        names.sort();
        names.dedup();
        Ok(names)
    }

    /// Count, per name, how many *definitions* of each kind exist in the index.
    ///
    /// Returns `name -> (kind string -> definition count)`. Production coloring
    /// resolves kinds from the in-memory `NameTable`; this SQL form is retained
    /// only as the parity oracle for that path's tests.
    #[cfg(test)]
    pub fn declaration_kind_counts_by_names(
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
                "SELECT d.name,
                        CASE d.declaration_kind
                            WHEN 0 THEN 'function'
                            WHEN 1 THEN 'function'
                            WHEN 2 THEN 'global_variable'
                            WHEN 3 THEN 'type'
                            WHEN 4 THEN 'type'
                            WHEN 5 THEN 'enum_constant'
                            WHEN 6 THEN 'macro'
                            ELSE 'unknown'
                        END,
                        COUNT(*)
                 FROM declarations d
                 JOIN file_entries f ON f.id = d.file_id
                 WHERE d.role = 1 AND d.name IN ({placeholders})
                 AND (f.source = 'workspace' OR f.directly_included = 1)
                 GROUP BY d.name, d.declaration_kind"
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
}
