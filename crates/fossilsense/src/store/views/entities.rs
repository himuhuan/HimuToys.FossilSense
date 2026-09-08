use crate::semantic_model::{EntityIdentity, SemanticDeclarationRole};
use crate::store::IndexStore;
use anyhow::Result;
use rusqlite::params;

pub const ENTITY_PAGE_LIMIT: usize = 256;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityOccurrenceReadRow {
    pub declaration_id: i64,
    pub revision_id: i64,
}
#[derive(Debug)]
pub struct EntityOccurrencePage {
    pub rows: Vec<EntityOccurrenceReadRow>,
    /// The page hit its budget; another page may exist.
    pub truncated: bool,
    pub after_id: Option<i64>,
}
pub struct EntityStoreView<'a> {
    store: &'a IndexStore,
}
impl IndexStore {
    pub fn entity_view(&self) -> EntityStoreView<'_> {
        EntityStoreView { store: self }
    }
}
impl EntityStoreView<'_> {
    #[cfg(test)]
    pub fn active_count(&self) -> Result<i64> {
        Ok(self.store.conn.query_row("SELECT COUNT(*) FROM entity_occurrences e JOIN active_file_revisions a ON a.revision_id=e.revision_id", [], |row| row.get(0))?)
    }

    pub fn incarnation(&self) -> Result<String> {
        Ok(self.store.conn.query_row(
            "SELECT value FROM meta WHERE key = 'entity_incarnation'",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn occurrences(
        &self,
        identity: &EntityIdentity,
        role: SemanticDeclarationRole,
        after_id: i64,
        limit: usize,
    ) -> Result<EntityOccurrencePage> {
        let limit = limit.min(ENTITY_PAGE_LIMIT);
        if limit == 0 {
            return Ok(EntityOccurrencePage {
                rows: Vec::new(),
                truncated: true,
                after_id: None,
            });
        }
        let mut statement = self.store.conn.prepare(
            "SELECT e.declaration_id, e.revision_id
             FROM entity_occurrences e
             JOIN active_file_revisions active ON active.revision_id = e.revision_id
             WHERE e.family = ?1 AND e.domain = ?2 AND e.identity_digest = ?3
               AND e.role = ?4 AND e.declaration_id > ?5
             ORDER BY e.declaration_id LIMIT ?6",
        )?;
        let rows = statement
            .query_map(
                params![
                    identity.family as u8,
                    identity.domain as u8,
                    identity.digest().as_slice(),
                    role_code(role),
                    after_id,
                    limit as i64
                ],
                |row| {
                    Ok(EntityOccurrenceReadRow {
                        declaration_id: row.get(0)?,
                        revision_id: row.get(1)?,
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(EntityOccurrencePage {
            truncated: rows.len() == limit,
            after_id: rows.last().map(|row| row.declaration_id),
            rows,
        })
    }
    pub fn identity_for_declaration(&self, declaration_id: i64) -> Result<Option<[u8; 12]>> {
        use rusqlite::OptionalExtension;
        let value: Option<Vec<u8>> = self
            .store
            .conn
            .query_row(
                "SELECT e.identity_digest FROM entity_occurrences e
             JOIN active_file_revisions active ON active.revision_id = e.revision_id
             WHERE e.declaration_id = ?1",
                [declaration_id],
                |row| row.get(0),
            )
            .optional()?;
        value
            .map(|bytes| {
                bytes
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("invalid entity identity digest"))
            })
            .transpose()
    }
}
fn role_code(role: SemanticDeclarationRole) -> i64 {
    match role {
        SemanticDeclarationRole::Declaration => 0,
        SemanticDeclarationRole::Definition => 1,
        SemanticDeclarationRole::TentativeDefinition => 2,
        SemanticDeclarationRole::Unknown => 3,
    }
}
