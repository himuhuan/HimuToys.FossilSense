use anyhow::{Context, Result};
use rusqlite::params;

use crate::semantic_model::{DeclarationFact, LogicalEntityKey};

use crate::store::IndexStore;

const SELECT: &str = "SELECT d.id, d.fact_json, d.backing_kind, d.backing_id,
    rev.source, f.directly_included, rev.id, rev.size, rev.mtime_ns, rev.hash
    FROM declarations d
    JOIN file_entries f ON f.id = d.file_id
    JOIN file_revisions rev ON rev.id = d.revision_id";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclarationReadRow {
    pub id: i64,
    pub fact: DeclarationFact,
    pub backing_kind: String,
    pub backing_id: Option<i64>,
    pub external: bool,
    pub directly_included: bool,
    pub revision_id: i64,
    pub revision_size: u64,
    pub revision_mtime_ns: i64,
    pub revision_hash: String,
}

pub struct DeclarationStoreView<'a> {
    store: &'a IndexStore,
}

impl IndexStore {
    pub fn declaration_view(&self) -> DeclarationStoreView<'_> {
        DeclarationStoreView::new(self)
    }
}

impl<'a> DeclarationStoreView<'a> {
    pub(crate) fn new(store: &'a IndexStore) -> Self {
        Self { store }
    }

    pub fn by_name_limited(
        &self,
        name: &str,
        limit: usize,
    ) -> Result<(Vec<DeclarationReadRow>, bool)> {
        let sql = format!("{SELECT} WHERE d.name = ?1 ORDER BY d.id LIMIT ?2");
        let rows = self.read(&sql, params![name, limit.saturating_add(1) as i64])?;
        Ok(truncate(rows, limit))
    }

    /// Bounded exact-name read for request-priority paths. Callers use this
    /// only after the ordinary workspace-wide read proves that its cap hid
    /// rows, so current and reachable declarations cannot be starved by
    /// earlier unrelated declarations with the same spelling.
    pub fn by_name_in_paths_limited(
        &self,
        name: &str,
        paths: &[String],
        limit: usize,
    ) -> Result<(Vec<DeclarationReadRow>, bool)> {
        let mut output = Vec::new();
        for chunk in paths.chunks(399) {
            let probe_limit = limit.saturating_sub(output.len()).saturating_add(1);
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!(
                "{SELECT} WHERE d.name = ? AND f.path IN ({placeholders}) \
                 ORDER BY d.id LIMIT {probe_limit}"
            );
            let mut values = Vec::with_capacity(chunk.len() + 1);
            values.push(name);
            values.extend(chunk.iter().map(String::as_str));
            output.extend(self.read(&sql, rusqlite::params_from_iter(values))?);
            if output.len() > limit {
                break;
            }
        }
        Ok(truncate(output, limit))
    }

    pub fn by_ids(&self, ids: &[i64]) -> Result<Vec<DeclarationReadRow>> {
        let mut output = Vec::new();
        for chunk in ids.chunks(400) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!("{SELECT} WHERE d.id IN ({placeholders}) ORDER BY d.id");
            output.extend(self.read(&sql, rusqlite::params_from_iter(chunk.iter().copied()))?);
        }
        Ok(output)
    }

    pub fn by_logical_key_limited(
        &self,
        key: &LogicalEntityKey,
        limit: usize,
    ) -> Result<(Vec<DeclarationReadRow>, bool)> {
        let digest = logical_key_digest(key)?;
        let name = key
            .qualified_name
            .rsplit("::")
            .next()
            .unwrap_or(key.qualified_name.as_str());
        let sql = format!(
            "{SELECT} WHERE d.name = ?1 AND d.logical_key_digest = ?2 ORDER BY d.id LIMIT ?3"
        );
        let rows = self.read(&sql, params![name, digest, limit.saturating_add(1) as i64])?;
        let rows = rows
            .into_iter()
            .filter(|row| &row.fact.identity.logical_key == key)
            .collect();
        Ok(truncate(rows, limit))
    }

    fn read<P>(&self, sql: &str, params: P) -> Result<Vec<DeclarationReadRow>>
    where
        P: rusqlite::Params,
    {
        let mut stmt = self.store.conn.prepare(sql)?;
        let mut rows = stmt.query(params)?;
        let mut output = Vec::new();
        while let Some(row) = rows.next()? {
            let fact_json: String = row.get(1)?;
            let fact = serde_json::from_str(&fact_json).with_context(|| {
                format!(
                    "invalid declaration fact row {}",
                    row.get::<_, i64>(0).unwrap_or_default()
                )
            })?;
            output.push(DeclarationReadRow {
                id: row.get(0)?,
                fact,
                backing_kind: row.get(2)?,
                backing_id: row.get(3)?,
                external: row.get::<_, String>(4)? == "external",
                directly_included: row.get::<_, i64>(5)? != 0,
                revision_id: row.get(6)?,
                revision_size: row.get::<_, i64>(7)? as u64,
                revision_mtime_ns: row.get(8)?,
                revision_hash: row.get(9)?,
            });
        }
        Ok(output)
    }
}

fn truncate(mut rows: Vec<DeclarationReadRow>, limit: usize) -> (Vec<DeclarationReadRow>, bool) {
    let truncated = rows.len() > limit;
    rows.truncate(limit);
    (rows, truncated)
}

fn logical_key_digest(key: &LogicalEntityKey) -> Result<Vec<u8>> {
    let encoded = serde_json::to_vec(key)?;
    Ok(blake3::hash(&encoded).as_bytes()[..12].to_vec())
}
