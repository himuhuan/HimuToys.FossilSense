//! Narrow, revision-bound source fact windows. Scan cursors advance over noise.
use super::super::IndexStore;
use crate::semantic_model::{relations::*, SemanticFamily};
use anyhow::Result;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

pub const RELATION_SCAN_LIMIT: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelationFact {
    Binding(BindingSiteFact),
    Base(ExplicitBaseFact),
    Assignment(IndirectAssignmentFact),
    Macro(MacroFact),
}
impl RelationFact {
    pub fn source(&self) -> &RelationSource {
        match self {
            Self::Binding(f) => &f.source,
            Self::Base(f) => &f.source,
            Self::Assignment(f) => &f.source,
            Self::Macro(f) => &f.source,
        }
    }
}
#[derive(Debug, Clone)]
pub struct RelationFactRow {
    pub id: i64,
    pub path: String,
    #[allow(dead_code)] // Captured revision identity for downstream relation projections.
    pub revision_hash: String,
    pub fact: RelationFact,
}
#[derive(Debug, Default)]
pub struct RelationFactPage {
    pub rows: Vec<RelationFactRow>,
    pub next: Option<i64>,
    pub unavailable: bool,
    pub partial: bool,
}

#[derive(Clone, Copy)]
pub enum RelationFactLookup<'a> {
    Name(&'a str),
    #[allow(dead_code)]
    Path(&'a str),
    Target(&'a str),
    Caller(&'a str),
}

pub struct RelationFactStoreView<'a> {
    store: &'a IndexStore,
}
impl IndexStore {
    pub fn relation_fact_view(&self) -> RelationFactStoreView<'_> {
        RelationFactStoreView { store: self }
    }
}
impl RelationFactStoreView<'_> {
    #[cfg(test)]
    pub fn benchmark_counts(&self) -> Result<(u64, u64)> {
        Ok(self.store.conn.query_row("SELECT count(*),coalesce(sum(length(payload)),0) FROM relation_source_facts s JOIN active_file_revisions a ON a.revision_id=s.revision_id",[],|r|Ok((r.get::<_,i64>(0)? as u64,r.get::<_,i64>(1)? as u64)))?)
    }
    pub fn available(&self) -> Result<bool> {
        Ok(self
            .store
            .conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='relation_source_facts'",
                [],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false))
    }
    pub fn scan(
        &self,
        kind: u8,
        lookup: RelationFactLookup<'_>,
        family: SemanticFamily,
        after: i64,
        limit: usize,
    ) -> Result<RelationFactPage> {
        if !self.available()? {
            return Ok(RelationFactPage {
                unavailable: true,
                ..Default::default()
            });
        }
        let limit = limit.min(RELATION_SCAN_LIMIT);
        let (column, value) = match lookup {
            RelationFactLookup::Name(v) => ("s.name", v),
            RelationFactLookup::Path(v) => ("f.path", v),
            RelationFactLookup::Target(v) => ("s.target_name", v),
            RelationFactLookup::Caller(v) => ("s.caller", v),
        };
        let language = match family {
            SemanticFamily::CFamily => "rev.language <> 3",
            SemanticFamily::Go => "rev.language = 3",
        };
        let sql = format!(
            "SELECT s.id,f.path,rev.hash,s.payload FROM relation_source_facts s
            JOIN active_file_revisions a ON a.revision_id=s.revision_id
            JOIN file_entries f ON f.id=s.file_id JOIN file_revisions rev ON rev.id=s.revision_id
            WHERE s.kind=?1 AND {column}=?2 AND s.id>?3 AND {language} ORDER BY s.id LIMIT ?4"
        );
        let mut statement = self.store.conn.prepare(&sql)?;
        let mut rows = statement.query(params![kind, value, after, (limit + 1) as i64])?;
        let mut result = RelationFactPage::default();
        let path = match lookup {
            RelationFactLookup::Path(p) => Some(p),
            _ => None,
        };
        let scope = if path.is_some() {
            "AND f.path=?2"
        } else {
            "AND ?2 IS NULL"
        };
        let coverage_sql=format!("SELECT c.state FROM relation_file_coverage c JOIN active_file_revisions a ON a.revision_id=c.revision_id JOIN file_entries f ON f.id=c.file_id JOIN file_revisions rev ON rev.id=c.revision_id WHERE c.kind=?1 AND c.state>0 AND {language} {scope} ORDER BY c.state DESC LIMIT 1");
        let state: Option<i64> = self
            .store
            .conn
            .query_row(&coverage_sql, params![kind, path], |row| row.get(0))
            .optional()?;
        result.partial = state.is_some();
        result.unavailable = path.is_some() && state == Some(2);

        while let Some(row) = rows.next()? {
            if result.rows.len() == limit {
                result.next = Some(result.rows.last().map_or(after, |r| r.id));
                break;
            }
            let payload: String = row.get(3)?;
            result.rows.push(RelationFactRow {
                id: row.get(0)?,
                path: row.get(1)?,
                revision_hash: row.get(2)?,
                fact: serde_json::from_str(&payload)?,
            });
        }
        Ok(result)
    }
}
