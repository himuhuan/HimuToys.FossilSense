use anyhow::{Context, Result};
use rusqlite::params;

use crate::call_model::{LinkageDomain, SourcePosition, SourceRange};
use crate::semantic_model::{
    DeclarationBacking, DeclarationFact, DeclarationIdentity, DeclarationLocator, LanguageFidelity,
    LogicalEntityKey, SemanticDeclarationKind, SemanticDeclarationRole, SemanticFactFidelity,
    SemanticFactProvenance, SemanticLanguage,
};

use crate::store::IndexStore;

const SELECT: &str = "SELECT
    d.id, d.name, d.qualified_name, d.declaration_kind, d.role,
    d.name_start_byte, d.name_end_byte, d.name_start_line, d.name_start_col,
    d.name_end_line, d.name_end_col,
    d.declaration_start_byte, d.declaration_end_byte, d.declaration_start_line,
    d.declaration_start_col, d.declaration_end_line, d.declaration_end_col,
    d.canonical_signature, d.declarator_shape_json, d.has_initializer, d.owner,
    d.linkage_kind, d.guard, d.language, d.language_fidelity, d.provenance,
    d.fact_fidelity, d.logical_key_digest, d.locator_fingerprint,
    d.logical_linkage_domain, d.guard_fingerprint,
    d.backing_kind, d.backing_id, d.backing_key,
    d.backing_start_byte, d.backing_end_byte,
    f.path, rev.source, f.directly_included,
    rev.id, rev.size, rev.mtime_ns, rev.hash
    FROM declarations d
    JOIN file_entries f ON f.id = d.file_id
    JOIN file_revisions rev ON rev.id = d.revision_id";

const SELECT_NAME: &str = "SELECT
    d.id, d.name, d.declaration_kind, d.role,
    f.path, rev.source, f.directly_included
    FROM declarations d
    JOIN file_entries f ON f.id = d.file_id
    JOIN file_revisions rev ON rev.id = d.revision_id";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclarationReadRow {
    pub id: i64,
    pub fact: DeclarationFact,
    pub logical_key_digest: Vec<u8>,
    pub backing_kind: String,
    pub backing_id: Option<i64>,
    pub external: bool,
    pub directly_included: bool,
    pub revision_id: i64,
    pub revision_size: u64,
    pub revision_mtime_ns: i64,
    pub revision_hash: String,
}

/// Owned projection used only for changed-path deltas. Full cold publication
/// streams the borrowed counterpart directly into the compact name index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclarationNameRow {
    pub id: i64,
    pub name: String,
    pub declaration_kind: SemanticDeclarationKind,
    pub role: SemanticDeclarationRole,
    pub path: String,
    pub external: bool,
    pub directly_included: bool,
}

/// Borrowed declaration-name projection. It deliberately contains only recall
/// evidence; Hover, navigation, signature help, and completion resolve hydrate
/// the canonical [`DeclarationReadRow`] by ID instead of trusting this view as
/// a second semantic truth source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeclarationNameRef<'a> {
    pub id: i64,
    pub name: &'a str,
    pub declaration_kind: SemanticDeclarationKind,
    pub role: SemanticDeclarationRole,
    pub path: &'a str,
    pub external: bool,
    pub directly_included: bool,
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

    pub fn visit_name_rows<F>(&self, mut visitor: F) -> Result<usize>
    where
        F: for<'row> FnMut(DeclarationNameRef<'row>) -> Result<()>,
    {
        let sql = format!("{SELECT_NAME} ORDER BY d.id");
        let mut stmt = self.store.conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            let id = row.get(0)?;
            let name = row.get_ref(1)?.as_str()?;
            let path = row.get_ref(4)?.as_str()?;
            let source = row.get_ref(5)?.as_str()?;
            visitor(DeclarationNameRef {
                id,
                name,
                declaration_kind: declaration_kind(row.get(2)?)
                    .with_context(|| format!("invalid declaration kind for row {id}"))?,
                role: declaration_role(row.get(3)?)
                    .with_context(|| format!("invalid declaration role for row {id}"))?,
                path,
                external: source == "external",
                directly_included: row.get::<_, i64>(6)? != 0,
            })?;
            count += 1;
        }
        Ok(count)
    }

    #[cfg(test)]
    pub fn all_name_rows(&self) -> Result<Vec<DeclarationNameRow>> {
        let sql = format!("{SELECT_NAME} ORDER BY d.id");
        let mut stmt = self.store.conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        let mut output = Vec::new();
        while let Some(row) = rows.next()? {
            output.push(declaration_name_row(row)?);
        }
        Ok(output)
    }

    #[cfg(test)]
    pub fn largest_declaration_path(&self) -> Result<Option<(String, usize)>> {
        use rusqlite::OptionalExtension;

        let row: Option<(String, i64)> = self
            .store
            .conn
            .query_row(
                "SELECT f.path, COUNT(*) AS declaration_count
                 FROM declarations d
                 JOIN file_entries f ON f.id = d.file_id
                 GROUP BY f.id
                 ORDER BY declaration_count DESC, f.path ASC
                 LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(row.map(|(path, count)| (path, count.max(0) as usize)))
    }

    pub fn name_rows_for_paths(&self, paths: &[String]) -> Result<Vec<DeclarationNameRow>> {
        let mut output = Vec::new();
        for chunk in paths.chunks(400) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!("{SELECT_NAME} WHERE f.path IN ({placeholders}) ORDER BY d.id");
            let mut stmt = self.store.conn.prepare(&sql)?;
            let mut rows =
                stmt.query(rusqlite::params_from_iter(chunk.iter().map(String::as_str)))?;
            while let Some(row) = rows.next()? {
                output.push(declaration_name_row(row)?);
            }
        }
        Ok(output)
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

    #[cfg(test)]
    pub fn by_name(&self, name: &str) -> Result<Vec<DeclarationReadRow>> {
        let sql = format!("{SELECT} WHERE d.name = ?1 ORDER BY d.id");
        self.read(&sql, params![name])
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
        let mut by_id = std::collections::HashMap::new();
        for chunk in ids.chunks(400) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!("{SELECT} WHERE d.id IN ({placeholders}) ORDER BY d.id");
            for row in self.read(&sql, rusqlite::params_from_iter(chunk.iter().copied()))? {
                by_id.insert(row.id, row);
            }
        }
        Ok(ids.iter().filter_map(|id| by_id.get(id).cloned()).collect())
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
            output.push(declaration_row(row)?);
        }
        Ok(output)
    }
}

fn declaration_name_row(row: &rusqlite::Row<'_>) -> Result<DeclarationNameRow> {
    let id: i64 = row.get(0)?;
    Ok(DeclarationNameRow {
        id,
        name: row.get(1)?,
        declaration_kind: declaration_kind(row.get(2)?)
            .with_context(|| format!("invalid declaration kind for row {id}"))?,
        role: declaration_role(row.get(3)?)
            .with_context(|| format!("invalid declaration role for row {id}"))?,
        path: row.get(4)?,
        external: row.get::<_, String>(5)? == "external",
        directly_included: row.get::<_, i64>(6)? != 0,
    })
}

fn declaration_row(row: &rusqlite::Row<'_>) -> Result<DeclarationReadRow> {
    let id: i64 = row.get(0)?;
    let name: String = row.get(1)?;
    let qualified_name: String = row.get(2)?;
    let declaration_kind = declaration_kind(row.get(3)?)
        .with_context(|| format!("invalid declaration kind for row {id}"))?;
    let role = declaration_role(row.get(4)?)
        .with_context(|| format!("invalid declaration role for row {id}"))?;
    let name_range = source_range(row, 5)?;
    let declaration_range = source_range(row, 11)?;
    let canonical_signature: Option<String> = row.get(17)?;
    let declarator_shape = row
        .get::<_, Option<String>>(18)?
        .map(|value| serde_json::from_str(&value))
        .transpose()
        .with_context(|| format!("invalid declarator shape for declaration row {id}"))?;
    let has_initializer = row.get::<_, Option<i64>>(19)?.map(|value| value != 0);
    let owner: Option<String> = row.get(20)?;
    let path: String = row.get(36)?;
    let linkage = match row.get::<_, i64>(21)? {
        0 => LinkageDomain::External,
        1 => LinkageDomain::Internal(path.clone()),
        2 => LinkageDomain::Unknown,
        value => anyhow::bail!("invalid linkage kind {value} for declaration row {id}"),
    };
    let guard: Option<String> = row.get(22)?;
    let language = semantic_language(row.get(23)?)
        .with_context(|| format!("invalid language for declaration row {id}"))?;
    let language_fidelity = language_fidelity(row.get(24)?)
        .with_context(|| format!("invalid language fidelity for declaration row {id}"))?;
    let provenance = semantic_provenance(row.get(25)?)
        .with_context(|| format!("invalid provenance for declaration row {id}"))?;
    let fact_fidelity = semantic_fidelity(row.get(26)?)
        .with_context(|| format!("invalid fact fidelity for declaration row {id}"))?;
    let logical_key_digest: Vec<u8> = row.get(27)?;
    anyhow::ensure!(
        logical_key_digest.len() == 12,
        "invalid logical key digest for declaration row {id}"
    );
    let locator_fingerprint: String = row.get(28)?;
    let logical_linkage_domain: String = row.get(29)?;
    let guard_fingerprint: Option<String> = row.get(30)?;
    let backing_kind: String = row.get(31)?;
    let backing_id: Option<i64> = row.get(32)?;
    let backing_key: Option<String> = row.get(33)?;
    let backing_start: Option<i64> = row.get(34)?;
    let backing_end: Option<i64> = row.get(35)?;
    let backing = declaration_backing(
        id,
        &backing_kind,
        backing_key,
        backing_start,
        backing_end,
        name_range,
    )?;

    let logical_key = LogicalEntityKey {
        qualified_name: qualified_name.clone(),
        declaration_kind,
        owner: owner.clone(),
        canonical_signature: canonical_signature.clone(),
        linkage_domain: logical_linkage_domain,
        guard_fingerprint,
    };
    let identity = DeclarationIdentity {
        locator: DeclarationLocator {
            workspace_id: String::new(),
            path: path.clone(),
            range: declaration_range,
            fingerprint: locator_fingerprint,
        },
        logical_key,
        language,
        language_fidelity,
        provenance,
        fact_fidelity,
        role,
    };
    let fact = DeclarationFact {
        identity,
        name,
        qualified_name,
        declaration_kind,
        role,
        path,
        name_range,
        declaration_range,
        canonical_signature,
        declarator_shape,
        has_initializer,
        owner,
        linkage,
        guard,
        backing,
    };
    Ok(DeclarationReadRow {
        id,
        fact,
        logical_key_digest,
        backing_kind,
        backing_id,
        external: row.get::<_, String>(37)? == "external",
        directly_included: row.get::<_, i64>(38)? != 0,
        revision_id: row.get(39)?,
        revision_size: row.get::<_, i64>(40)? as u64,
        revision_mtime_ns: row.get(41)?,
        revision_hash: row.get(42)?,
    })
}

fn source_range(row: &rusqlite::Row<'_>, offset: usize) -> Result<SourceRange> {
    Ok(SourceRange {
        start: SourcePosition {
            line: row.get::<_, i64>(offset + 2)? as u32,
            character: row.get::<_, i64>(offset + 3)? as u32,
        },
        end: SourcePosition {
            line: row.get::<_, i64>(offset + 4)? as u32,
            character: row.get::<_, i64>(offset + 5)? as u32,
        },
        start_byte: row.get::<_, i64>(offset)? as usize,
        end_byte: row.get::<_, i64>(offset + 1)? as usize,
    })
}

fn declaration_backing(
    id: i64,
    kind: &str,
    key: Option<String>,
    start: Option<i64>,
    end: Option<i64>,
    name_range: SourceRange,
) -> Result<DeclarationBacking> {
    Ok(match kind {
        "callable_anchor" => DeclarationBacking::CallableAnchor {
            fingerprint: key.context("callable declaration backing is missing its fingerprint")?,
        },
        "record" => DeclarationBacking::Record {
            record_key: key.context("record declaration backing is missing its key")?,
        },
        "type_alias" => DeclarationBacking::TypeAlias {
            fingerprint: key.context("alias declaration backing is missing its fingerprint")?,
        },
        "source_range" => DeclarationBacking::SourceRange {
            range: SourceRange {
                start_byte: start.with_context(|| {
                    format!("source range declaration row {id} is missing start byte")
                })? as usize,
                end_byte: end.with_context(|| {
                    format!("source range declaration row {id} is missing end byte")
                })? as usize,
                ..name_range
            },
        },
        "none" => DeclarationBacking::None,
        value => anyhow::bail!("invalid backing kind {value:?} for declaration row {id}"),
    })
}

fn declaration_kind(value: i64) -> Result<SemanticDeclarationKind> {
    Ok(match value {
        0 => SemanticDeclarationKind::Function,
        1 => SemanticDeclarationKind::Method,
        2 => SemanticDeclarationKind::Object,
        3 => SemanticDeclarationKind::Type,
        4 => SemanticDeclarationKind::Alias,
        5 => SemanticDeclarationKind::EnumConstant,
        6 => SemanticDeclarationKind::Macro,
        _ => anyhow::bail!("unknown declaration kind code {value}"),
    })
}

fn declaration_role(value: i64) -> Result<SemanticDeclarationRole> {
    Ok(match value {
        0 => SemanticDeclarationRole::Declaration,
        1 => SemanticDeclarationRole::Definition,
        2 => SemanticDeclarationRole::TentativeDefinition,
        3 => SemanticDeclarationRole::Unknown,
        _ => anyhow::bail!("unknown declaration role code {value}"),
    })
}

fn semantic_language(value: i64) -> Result<SemanticLanguage> {
    Ok(match value {
        0 => SemanticLanguage::C,
        1 => SemanticLanguage::Cpp,
        2 => SemanticLanguage::Unknown,
        _ => anyhow::bail!("unknown semantic language code {value}"),
    })
}

fn language_fidelity(value: i64) -> Result<LanguageFidelity> {
    Ok(match value {
        0 => LanguageFidelity::Explicit,
        1 => LanguageFidelity::Inferred,
        2 => LanguageFidelity::Heuristic,
        3 => LanguageFidelity::Unknown,
        _ => anyhow::bail!("unknown language fidelity code {value}"),
    })
}

fn semantic_provenance(value: i64) -> Result<SemanticFactProvenance> {
    Ok(match value {
        0 => SemanticFactProvenance::Ast,
        _ => anyhow::bail!("unknown semantic provenance code {value}"),
    })
}

fn semantic_fidelity(value: i64) -> Result<SemanticFactFidelity> {
    Ok(match value {
        0 => SemanticFactFidelity::Authoritative,
        1 => SemanticFactFidelity::Incomplete,
        2 => SemanticFactFidelity::LowFidelity,
        _ => anyhow::bail!("unknown semantic fidelity code {value}"),
    })
}

fn truncate(mut rows: Vec<DeclarationReadRow>, limit: usize) -> (Vec<DeclarationReadRow>, bool) {
    let truncated = rows.len() > limit;
    rows.truncate(limit);
    (rows, truncated)
}

pub(crate) fn logical_key_digest(key: &LogicalEntityKey) -> Result<Vec<u8>> {
    let encoded = serde_json::to_vec(key)?;
    Ok(blake3::hash(&encoded).as_bytes()[..12].to_vec())
}
