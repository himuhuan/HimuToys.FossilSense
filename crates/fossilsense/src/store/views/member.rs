use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use rusqlite::types::Value;
use rusqlite::OptionalExtension;

use crate::model::{MemberCandidate, RecordCandidate, ScopeTier};
use crate::resolver::{self, ResolveContext};

use crate::store::{record_kind_from_str, IndexStore};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordReadRow {
    pub id: i64,
    pub display_name: String,
    pub tag_name: Option<String>,
    pub typedef_name: Option<String>,
    pub kind: crate::parser::RecordKind,
    pub path: String,
    pub external: bool,
    pub directly_included: bool,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
    pub confidence: crate::parser::RecordConfidence,
    pub signature: String,
}

impl RecordReadRow {
    fn into_candidate(self, ctx: Option<&ResolveContext<'_>>) -> RecordCandidate {
        let tier = resolver::scope_tier(&self.path, self.external, self.directly_included, ctx);
        RecordCandidate {
            id: self.id,
            display_name: self.display_name,
            tag_name: self.tag_name,
            typedef_name: self.typedef_name,
            kind: self.kind,
            path: self.path,
            start_byte: self.start_byte,
            end_byte: self.end_byte,
            start_line: self.start_line,
            start_col: self.start_col,
            end_line: self.end_line,
            end_col: self.end_col,
            confidence: self.confidence,
            signature: self.signature,
            tier,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberReadRow {
    pub name: String,
    pub kind: crate::parser::MemberKind,
    pub signature: String,
    pub confidence: crate::parser::MemberConfidence,
    pub type_name: Option<String>,
    pub owner_path: String,
    pub external: bool,
    pub directly_included: bool,
}

impl MemberReadRow {
    fn into_candidate(self, ctx: Option<&ResolveContext<'_>>) -> MemberCandidate {
        MemberCandidate {
            name: self.name,
            kind: self.kind,
            signature: self.signature,
            type_name: self.type_name,
            tier: resolver::scope_tier(
                &self.owner_path,
                self.external,
                self.directly_included,
                ctx,
            ),
            confidence: self.confidence,
            owner_path: self.owner_path,
        }
    }
}

pub struct MemberStoreView<'a> {
    store: &'a IndexStore,
}

impl<'a> MemberStoreView<'a> {
    pub(in crate::store) fn new(store: &'a IndexStore) -> Self {
        Self { store }
    }

    pub fn resolve_record_candidates(
        &self,
        names: &[&str],
        ctx: Option<&ResolveContext<'_>>,
    ) -> Result<Vec<RecordCandidate>> {
        let mut visited = HashSet::new();
        self.resolve_record_candidates_inner(names, ctx, &mut visited)
    }

    pub fn members_for_records(
        &self,
        record_ids: &[i64],
        prefix: Option<&str>,
        ctx: Option<&ResolveContext<'_>>,
    ) -> Result<Vec<MemberCandidate>> {
        if record_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = vec!["?"; record_ids.len()].join(",");
        let mut sql = format!(
            "SELECT m.name, m.kind, m.signature, m.confidence, m.type_name, f.path, f.source, f.directly_included \
             FROM members m \
             JOIN record_defs r ON r.id = m.record_id \
             JOIN files f ON f.id = r.file_id \
             WHERE m.record_id IN ({placeholders})"
        );
        let mut params: Vec<Value> = record_ids.iter().copied().map(Value::Integer).collect();
        if let Some(prefix) = prefix {
            sql.push_str(" AND m.name LIKE ? ESCAPE '\\' COLLATE NOCASE");
            params.push(Value::Text(format!(
                "{}%",
                prefix.replace('%', "\\%").replace('_', "\\_")
            )));
        }
        sql.push_str(" ORDER BY m.name, m.kind, m.signature");
        let mut stmt = self.store.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params), member_read_row)?;
        let mut members = Vec::new();
        for row in rows {
            members.push(row?.into_candidate(ctx));
        }
        sort_members_for_records(&mut members, prefix);
        Ok(members)
    }

    pub fn fallback_member_candidates(
        &self,
        prefix: &str,
        limit: usize,
        ctx: Option<&ResolveContext<'_>>,
    ) -> Result<Vec<MemberCandidate>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let pattern = format!("{}%", prefix.replace('%', "\\%").replace('_', "\\_"));
        let mut stmt = self.store.conn.prepare(
            "SELECT m.name, m.kind, m.confidence, m.signature, m.type_name, f.path, f.source, f.directly_included \
             FROM members m \
             JOIN record_defs r ON r.id = m.record_id \
             JOIN files f ON f.id = r.file_id \
             WHERE m.name LIKE ?1 ESCAPE '\\' COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([pattern], member_read_row)?;

        struct MemberMeta {
            candidate: MemberCandidate,
            freq: usize,
        }

        let mut by_member: HashMap<(String, crate::parser::MemberKind), MemberMeta> =
            HashMap::new();
        for row in rows {
            let candidate = row?.into_candidate(ctx);
            let key = (candidate.name.to_ascii_lowercase(), candidate.kind);
            let entry = by_member.entry(key).or_insert(MemberMeta {
                candidate: candidate.clone(),
                freq: 0,
            });
            entry.freq += 1;
            if candidate.tier.rank() > entry.candidate.tier.rank()
                || (candidate.tier == entry.candidate.tier
                    && candidate.signature < entry.candidate.signature)
            {
                entry.candidate = candidate;
            }
        }

        let mut sorted: Vec<MemberMeta> = by_member.into_values().collect();
        sorted.sort_by(|a, b| {
            b.candidate
                .tier
                .rank()
                .cmp(&a.candidate.tier.rank())
                .then_with(|| b.freq.cmp(&a.freq))
                .then_with(|| {
                    member_kind_rank(a.candidate.kind).cmp(&member_kind_rank(b.candidate.kind))
                })
                .then_with(|| a.candidate.name.cmp(&b.candidate.name))
                .then_with(|| a.candidate.signature.cmp(&b.candidate.signature))
        });
        Ok(sorted
            .into_iter()
            .take(limit)
            .map(|meta| meta.candidate)
            .collect())
    }

    #[cfg(test)]
    pub fn fallback_field_candidates(
        &self,
        prefix: &str,
        limit: usize,
        ctx: Option<&ResolveContext<'_>>,
    ) -> Result<Vec<(String, ScopeTier)>> {
        let pattern = format!("{}%", prefix.replace('%', "\\%").replace('_', "\\_"));
        let mut stmt = self.store.conn.prepare(
            "SELECT m.name, f.path, f.source, f.directly_included \
             FROM members m \
             JOIN record_defs r ON r.id = m.record_id \
             JOIN files f ON f.id = r.file_id \
             WHERE m.kind = 'field' AND m.name LIKE ?1 ESCAPE '\\' COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([pattern], |row| {
            let source_str: String = row.get(2)?;
            let directly_included: i64 = row.get(3)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                source_str == "external",
                directly_included != 0,
            ))
        })?;

        struct FieldMeta {
            max_tier: ScopeTier,
            freq: usize,
        }
        let mut map: HashMap<String, FieldMeta> = HashMap::new();

        for row in rows {
            let (name, path, external, directly_included) = row?;
            let tier = resolver::scope_tier(&path, external, directly_included, ctx);
            let entry = map.entry(name).or_insert(FieldMeta {
                max_tier: ScopeTier::Global,
                freq: 0,
            });
            entry.freq += 1;
            if tier.rank() > entry.max_tier.rank() {
                entry.max_tier = tier;
            }
        }

        let mut sorted: Vec<(String, FieldMeta)> = map.into_iter().collect();
        sorted.sort_by(|a, b| {
            let tier_cmp = b.1.max_tier.rank().cmp(&a.1.max_tier.rank());
            if tier_cmp != std::cmp::Ordering::Equal {
                tier_cmp
            } else {
                b.1.freq.cmp(&a.1.freq).then_with(|| a.0.cmp(&b.0))
            }
        });

        Ok(sorted
            .into_iter()
            .take(limit)
            .map(|(name, meta)| (name, meta.max_tier))
            .collect())
    }

    fn resolve_record_candidates_inner(
        &self,
        names: &[&str],
        ctx: Option<&ResolveContext<'_>>,
        visited: &mut HashSet<String>,
    ) -> Result<Vec<RecordCandidate>> {
        let mut candidates = Vec::new();
        if names.is_empty() {
            return Ok(candidates);
        }

        let placeholders = vec!["?"; names.len()].join(",");
        let sql = format!(
            "SELECT r.id, r.display_name, r.tag_name, r.typedef_name, r.kind, f.path, f.source, f.directly_included, \
             r.start_byte, r.end_byte, r.start_line, r.start_col, r.end_line, r.end_col, r.confidence, r.signature \
             FROM record_defs r \
             JOIN files f ON f.id = r.file_id \
             WHERE r.display_name IN ({placeholders}) \
                OR r.tag_name IN ({placeholders}) \
                OR r.typedef_name IN ({placeholders})"
        );
        let mut stmt = self.store.conn.prepare(&sql)?;
        let mut params = Vec::new();
        for &name in names {
            params.push(name);
        }
        let mut all_params = Vec::new();
        for _ in 0..3 {
            for &param in &params {
                all_params.push(param);
            }
        }

        let rows = stmt.query_map(rusqlite::params_from_iter(all_params), record_read_row)?;
        for row in rows {
            candidates.push(row?.into_candidate(ctx));
        }
        drop(stmt);

        let sql_aliases = format!(
            "SELECT a.alias, f.path, f.source, f.directly_included, \
             a.target_record_id, a.target_name, a.target_kind \
             FROM type_aliases a \
             JOIN files f ON f.id = a.file_id \
             WHERE a.alias IN ({placeholders})"
        );
        let mut stmt_aliases = self.store.conn.prepare(&sql_aliases)?;
        let alias_rows = stmt_aliases.query_map(rusqlite::params_from_iter(params), |row| {
            let source_str: String = row.get(2)?;
            let directly_included: i64 = row.get(3)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                source_str == "external",
                directly_included != 0,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?;

        for row in alias_rows {
            let (
                _alias,
                path,
                external,
                directly_included,
                target_record_id,
                target_name,
                target_kind,
            ) = row?;
            let alias_tier = resolver::scope_tier(&path, external, directly_included, ctx);

            if let Some(record_id) = target_record_id {
                if let Some(mut record) = self.fetch_record_by_id(record_id, ctx)? {
                    record.tier = alias_tier;
                    candidates.push(record);
                }
            } else if let Some(target_name) = target_name {
                if visited.insert(target_name.clone()) {
                    let resolved =
                        self.resolve_record_candidates_inner(&[&target_name], ctx, visited)?;
                    for mut record in resolved {
                        if let Some(kind) = target_kind.as_deref().and_then(record_kind_from_str) {
                            if record.kind != kind {
                                continue;
                            }
                        }
                        let min_rank = alias_tier.rank().min(record.tier.rank());
                        record.tier = match min_rank {
                            4 => ScopeTier::Current,
                            3 => ScopeTier::Reachable,
                            2 => ScopeTier::External,
                            1 => ScopeTier::Unknown,
                            _ => ScopeTier::Global,
                        };
                        candidates.push(record);
                    }
                    visited.remove(&target_name);
                }
            }
        }

        candidates.sort_by(|a, b| {
            let tier_order = b.tier.rank().cmp(&a.tier.rank());
            if tier_order != std::cmp::Ordering::Equal {
                tier_order
            } else {
                a.id.cmp(&b.id)
            }
        });
        candidates.dedup_by_key(|candidate| candidate.id);

        Ok(candidates)
    }

    fn fetch_record_by_id(
        &self,
        record_id: i64,
        ctx: Option<&ResolveContext<'_>>,
    ) -> Result<Option<RecordCandidate>> {
        self.store
            .conn
            .query_row(
                "SELECT r.id, r.display_name, r.tag_name, r.typedef_name, r.kind, f.path, f.source, f.directly_included, \
                 r.start_byte, r.end_byte, r.start_line, r.start_col, r.end_line, r.end_col, r.confidence, r.signature \
                 FROM record_defs r \
                 JOIN files f ON f.id = r.file_id \
                 WHERE r.id = ?1",
                [record_id],
                record_read_row,
            )
            .optional()
            .map(|row| row.map(|row| row.into_candidate(ctx)))
            .context("failed to fetch record by id")
    }
}

fn record_read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RecordReadRow> {
    let kind_str: String = row.get(4)?;
    let kind = match kind_str.as_str() {
        "union" => crate::parser::RecordKind::Union,
        "class" => crate::parser::RecordKind::Class,
        _ => crate::parser::RecordKind::Struct,
    };
    let source_str: String = row.get(6)?;
    let directly_included: i64 = row.get(7)?;
    let confidence_str: String = row.get(14)?;
    let confidence = match confidence_str.as_str() {
        "named_tag" => crate::parser::RecordConfidence::NamedTag,
        "anonymous_typedef" => crate::parser::RecordConfidence::AnonymousTypedef,
        _ => crate::parser::RecordConfidence::Heuristic,
    };

    Ok(RecordReadRow {
        id: row.get(0)?,
        display_name: row.get(1)?,
        tag_name: row.get(2)?,
        typedef_name: row.get(3)?,
        kind,
        path: row.get(5)?,
        external: source_str == "external",
        directly_included: directly_included != 0,
        start_byte: row.get::<_, i64>(8)? as usize,
        end_byte: row.get::<_, i64>(9)? as usize,
        start_line: row.get::<_, i64>(10)? as usize,
        start_col: row.get::<_, i64>(11)? as usize,
        end_line: row.get::<_, i64>(12)? as usize,
        end_col: row.get::<_, i64>(13)? as usize,
        confidence,
        signature: row.get(15)?,
    })
}

fn member_read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemberReadRow> {
    let kind_str: String = row.get(1)?;
    let confidence_str: String = row.get(3)?;
    let source_str: String = row.get(6)?;
    let directly_included: i64 = row.get(7)?;
    Ok(MemberReadRow {
        name: row.get(0)?,
        kind: member_kind_from_str(&kind_str),
        signature: row.get(2)?,
        confidence: member_confidence_from_str(&confidence_str),
        type_name: row.get(4)?,
        owner_path: row.get(5)?,
        external: source_str == "external",
        directly_included: directly_included != 0,
    })
}

fn member_kind_from_str(kind: &str) -> crate::parser::MemberKind {
    match kind {
        "method" => crate::parser::MemberKind::Method,
        "static_method" => crate::parser::MemberKind::StaticMethod,
        "nested_type" => crate::parser::MemberKind::NestedType,
        _ => crate::parser::MemberKind::Field,
    }
}

fn member_confidence_from_str(confidence: &str) -> crate::parser::MemberConfidence {
    match confidence {
        "out_of_class_owner" => crate::parser::MemberConfidence::OutOfClassOwner,
        "heuristic" => crate::parser::MemberConfidence::Heuristic,
        _ => crate::parser::MemberConfidence::InBody,
    }
}

fn member_kind_rank(kind: crate::parser::MemberKind) -> i32 {
    match kind {
        crate::parser::MemberKind::Field => 0,
        crate::parser::MemberKind::Method => 1,
        crate::parser::MemberKind::StaticMethod => 2,
        crate::parser::MemberKind::NestedType => 3,
    }
}

fn member_prefix_quality(name: &str, prefix: Option<&str>) -> i32 {
    let Some(prefix) = prefix else {
        return 0;
    };
    let name = name.to_ascii_lowercase();
    let prefix = prefix.to_ascii_lowercase();
    if name == prefix {
        2
    } else if name.starts_with(&prefix) {
        1
    } else {
        0
    }
}

fn sort_members_for_records(members: &mut [MemberCandidate], prefix: Option<&str>) {
    members.sort_by(|a, b| {
        b.tier
            .rank()
            .cmp(&a.tier.rank())
            .then_with(|| member_kind_rank(a.kind).cmp(&member_kind_rank(b.kind)))
            .then_with(|| {
                member_prefix_quality(&b.name, prefix).cmp(&member_prefix_quality(&a.name, prefix))
            })
            .then_with(|| a.signature.cmp(&b.signature))
            .then_with(|| a.name.cmp(&b.name))
    });
}
