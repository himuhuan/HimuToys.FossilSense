use anyhow::{Context, Result};
use std::collections::HashMap;

use rusqlite::{params, Connection};

use crate::semantic_model::AliasTarget;

use super::{
    include_normalized_metadata, member_confidence_to_str, member_kind_to_str, now_unix_secs,
    record_confidence_to_str, record_kind_to_str, symbol_kind, symbol_role, FileIndexPayload,
    FileIndexUpdate, IndexBuild,
};

pub(super) fn stage_file_updates(
    conn: &mut Connection,
    build: IndexBuild,
    updates: &[FileIndexUpdate<'_>],
    bulk_call_string_ids: Option<&mut HashMap<String, i64>>,
) -> Result<()> {
    if updates.is_empty() {
        return Ok(());
    }

    let tx = conn.transaction()?;
    let indexed_at = now_unix_secs();
    {
        let mut file_stmt = tx.prepare(
            "INSERT INTO file_entries (path, extension, size, mtime_ns, hash, indexed_at, status, error, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(path) DO NOTHING",
        )?;
        let mut file_id_stmt = tx.prepare("SELECT id FROM file_entries WHERE path = ?1")?;
        let mut revision_stmt = tx.prepare(
            "INSERT INTO file_revisions (
                file_id, extension, size, mtime_ns, hash, indexed_at, status, error, source,
                parser_version, fact_mask, parse_error_count, fallback_used
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10, ?11, ?12)",
        )?;
        let mut pending_stmt = tx.prepare(
            "INSERT INTO pending_file_revisions (build_id, file_id, revision_id)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(build_id, file_id) DO UPDATE SET revision_id = excluded.revision_id",
        )?;
        let mut symbol_stmt = tx.prepare(
            "INSERT INTO symbol_facts (
                    revision_id, file_id, name, kind, role, start_byte, end_byte,
                    start_line, start_col, end_line, end_col, signature, guard, container
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        )?;
        let mut include_stmt = tx
            .prepare("INSERT INTO include_facts (revision_id, file_id, line, target_text, target_form, target_normalized, target_basename) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)")?;
        let mut record_stmt = tx.prepare(
            "INSERT INTO record_facts (
                    revision_id, file_id, display_name, tag_name, typedef_name, kind, start_byte, end_byte,
                    start_line, start_col, end_line, end_col, signature, confidence
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        )?;
        let mut member_stmt = tx.prepare(
            "INSERT INTO member_facts (
                    record_id, name, kind, confidence, start_byte, end_byte,
                    start_line, start_col, end_line, end_col, signature, type_name
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        )?;
        let mut alias_stmt = tx.prepare(
            "INSERT INTO type_alias_facts (
                    revision_id, file_id, alias, start_byte, end_byte, start_line, start_col, end_line, end_col,
                    target_record_id, target_name, target_kind, confidence
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )?;
        let mut callable_stmt = tx.prepare(
            "INSERT INTO callable_anchor_facts (
                revision_id, file_id, entity_digest, anchor_digest, name_id, qualified_name_id,
                owner_id, owner_kind, kind, role, linkage_kind, linkage_file_id, signature_id,
                min_arity, max_arity, variadic,
                name_start_byte, name_end_byte, name_start_line, name_start_col,
                name_end_line, name_end_col, declaration_start_byte, declaration_end_byte,
                declaration_start_line, declaration_start_col, declaration_end_line,
                declaration_end_col, body_start_byte, body_end_byte, body_start_line,
                body_start_col, body_end_line, body_end_col, guard_id, flags
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25,
                ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35, ?36
             )",
        )?;
        let mut call_site_stmt = tx.prepare(
            "INSERT INTO call_site_facts (
                revision_id, file_id, caller_anchor_id,
                expression_start_byte, expression_end_byte,
                callee_start_byte, callee_end_byte, callee_start_line, callee_start_col,
                callee_end_line, callee_end_col, callee_name_id, qualified_name_id, call_form,
                argument_count, guard_id, flags
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                ?14, ?15, ?16, ?17
             )",
        )?;
        let bulk_call_strings = bulk_call_string_ids.is_some();
        let mut call_string_insert = tx.prepare(if bulk_call_strings {
            "INSERT INTO call_strings (text) VALUES (?1)"
        } else {
            "INSERT OR IGNORE INTO call_strings (text) VALUES (?1)"
        })?;
        let mut call_string_select = tx.prepare("SELECT id FROM call_strings WHERE text = ?1")?;
        let mut local_call_string_ids = HashMap::<String, i64>::new();
        let call_string_ids = bulk_call_string_ids.unwrap_or(&mut local_call_string_ids);
        let mut intern_call_string = |value: &str| -> Result<i64> {
            if let Some(id) = call_string_ids.get(value) {
                return Ok(*id);
            }
            call_string_insert.execute([value])?;
            let id = if bulk_call_strings {
                tx.last_insert_rowid()
            } else {
                call_string_select.query_row([value], |row| row.get(0))?
            };
            call_string_ids.insert(value.to_string(), id);
            Ok(id)
        };

        for update in updates {
            let fingerprint = update.fingerprint;
            let (status, error, fact_mask, parse_error_count, fallback_used) = match update.payload
            {
                FileIndexPayload::Ok(index) => (
                    "ok",
                    None,
                    index.persistence_diagnostics().fact_mask as i64,
                    index.persistence_diagnostics().parse_error_count as i64,
                    i64::from(index.persistence_diagnostics().fallback_used),
                ),
                FileIndexPayload::Error(error) => ("error", Some(error), 0, 0, 0),
            };
            file_stmt.execute(params![
                fingerprint.path.as_str(),
                fingerprint.extension.as_str(),
                fingerprint.size as i64,
                fingerprint.mtime_ns,
                fingerprint.hash.as_str(),
                indexed_at,
                status,
                error,
                update.source.as_str(),
            ])?;

            let file_id: i64 =
                file_id_stmt.query_row([fingerprint.path.as_str()], |row| row.get(0))?;
            revision_stmt.execute(params![
                file_id,
                fingerprint.extension.as_str(),
                fingerprint.size as i64,
                fingerprint.mtime_ns,
                fingerprint.hash.as_str(),
                indexed_at,
                status,
                error,
                update.source.as_str(),
                fact_mask,
                parse_error_count,
                fallback_used,
            ])?;
            let revision_id = tx.last_insert_rowid();
            pending_stmt.execute(params![build.id, file_id, revision_id])?;

            let FileIndexPayload::Ok(index) = update.payload else {
                continue;
            };
            let facts = index.persistent_facts();

            for symbol in facts.symbols {
                symbol_stmt.execute(params![
                    revision_id,
                    file_id,
                    symbol.name.as_str(),
                    symbol_kind(symbol.kind),
                    symbol_role(symbol.role),
                    symbol.start_byte as i64,
                    symbol.end_byte as i64,
                    symbol.start_line as i64,
                    symbol.start_col as i64,
                    symbol.end_line as i64,
                    symbol.end_col as i64,
                    symbol.signature.as_str(),
                    symbol.guard.as_deref(),
                    symbol.container.as_deref(),
                ])?;
            }

            for include in facts.includes {
                let (form, normalized, basename) =
                    include_normalized_metadata(&include.target_text);
                include_stmt.execute(params![
                    revision_id,
                    file_id,
                    include.line as i64,
                    include.target_text.as_str(),
                    form,
                    normalized,
                    basename,
                ])?;
            }

            let mut record_key_to_id = std::collections::HashMap::new();
            let mut record_name_to_ids: std::collections::HashMap<String, Vec<i64>> =
                std::collections::HashMap::new();
            for record in facts.records {
                record_stmt.execute(params![
                    revision_id,
                    file_id,
                    record.display_name.as_str(),
                    record.tag_name.as_deref(),
                    record.typedef_name.as_deref(),
                    record_kind_to_str(record.kind),
                    record.start_byte as i64,
                    record.end_byte as i64,
                    record.start_line as i64,
                    record.start_col as i64,
                    record.end_line as i64,
                    record.end_col as i64,
                    record.signature.as_str(),
                    record_confidence_to_str(record.confidence),
                ])?;
                let record_id = tx.last_insert_rowid();
                record_key_to_id.insert(record.record_key.clone(), record_id);
                let mut names = vec![record.display_name.as_str()];
                if let Some(tag) = record.tag_name.as_deref() {
                    names.push(tag);
                }
                if let Some(typedef) = record.typedef_name.as_deref() {
                    names.push(typedef);
                }
                names.sort_unstable();
                names.dedup();
                for name in names {
                    let ids = record_name_to_ids.entry(name.to_string()).or_default();
                    if !ids.contains(&record_id) {
                        ids.push(record_id);
                    }
                }
            }

            for member in facts.members {
                let record_id = record_key_to_id
                    .get(&member.record_key)
                    .copied()
                    .or_else(|| {
                        let owner = member.record_key.strip_prefix("owner:")?;
                        let ids = record_name_to_ids.get(owner)?;
                        (ids.len() == 1).then_some(ids[0])
                    });
                if let Some(rid) = record_id {
                    member_stmt.execute(params![
                        rid,
                        member.name.as_str(),
                        member_kind_to_str(member.kind),
                        member_confidence_to_str(member.confidence),
                        member.start_byte as i64,
                        member.end_byte as i64,
                        member.start_line as i64,
                        member.start_col as i64,
                        member.end_line as i64,
                        member.end_col as i64,
                        member.signature.as_str(),
                        member.type_name.as_deref(),
                    ])?;
                }
            }

            for alias in facts.aliases {
                let (target_record_id, target_name, target_kind, confidence) = match &alias.target {
                    AliasTarget::RecordKey(key) => {
                        let rid = record_key_to_id.get(key).copied();
                        (rid, None, None, "exact")
                    }
                    AliasTarget::NamedRecord { tag, kind } => {
                        let k_str = record_kind_to_str(*kind);
                        (None, Some(tag.as_str()), Some(k_str), "exact")
                    }
                    AliasTarget::UnresolvedTypeName(name) => {
                        (None, Some(name.as_str()), None, "heuristic")
                    }
                };
                alias_stmt.execute(params![
                    revision_id,
                    file_id,
                    alias.alias.as_str(),
                    alias.start_byte as i64,
                    alias.end_byte as i64,
                    alias.start_line as i64,
                    alias.start_col as i64,
                    alias.end_line as i64,
                    alias.end_col as i64,
                    target_record_id,
                    target_name,
                    target_kind,
                    confidence,
                ])?;
            }

            let mut callable_id_by_entity = std::collections::HashMap::new();
            for anchor in facts.callable_anchors {
                let (linkage_kind, linkage_file) = match &anchor.linkage {
                    crate::call_model::LinkageDomain::External => (1i64, None),
                    crate::call_model::LinkageDomain::Internal(path) => (2, Some(path.as_str())),
                    crate::call_model::LinkageDomain::Unknown => (0, None),
                };
                let name_id = intern_call_string(&anchor.name)?;
                let qualified_name_id = intern_call_string(&anchor.qualified_name)?;
                let owner_id = anchor
                    .owner
                    .as_deref()
                    .map(&mut intern_call_string)
                    .transpose()?;
                let linkage_file_id = linkage_file.map(&mut intern_call_string).transpose()?;
                let signature_id = intern_call_string(&anchor.signature.normalized)?;
                let guard_id = anchor
                    .guard
                    .as_deref()
                    .map(&mut intern_call_string)
                    .transpose()?;
                callable_stmt.execute(params![
                    revision_id,
                    file_id,
                    digest_bytes(&anchor.entity_key)?,
                    digest_bytes(&anchor.anchor_fingerprint)?,
                    name_id,
                    qualified_name_id,
                    owner_id,
                    anchor.owner_kind.map(owner_kind_code),
                    callable_kind_code(anchor.kind),
                    anchor_role_code(anchor.role),
                    linkage_kind,
                    linkage_file_id,
                    signature_id,
                    anchor.signature.min_arity.map(i64::from),
                    anchor.signature.max_arity.map(i64::from),
                    i64::from(anchor.signature.variadic),
                    anchor.name_range.start_byte as i64,
                    anchor.name_range.end_byte as i64,
                    anchor.name_range.start.line as i64,
                    anchor.name_range.start.character as i64,
                    anchor.name_range.end.line as i64,
                    anchor.name_range.end.character as i64,
                    anchor.declaration_range.start_byte as i64,
                    anchor.declaration_range.end_byte as i64,
                    anchor.declaration_range.start.line as i64,
                    anchor.declaration_range.start.character as i64,
                    anchor.declaration_range.end.line as i64,
                    anchor.declaration_range.end.character as i64,
                    anchor.body_range.map(|range| range.start_byte as i64),
                    anchor.body_range.map(|range| range.end_byte as i64),
                    anchor.body_range.map(|range| range.start.line as i64),
                    anchor.body_range.map(|range| range.start.character as i64),
                    anchor.body_range.map(|range| range.end.line as i64),
                    anchor.body_range.map(|range| range.end.character as i64),
                    guard_id,
                    fact_flags(anchor.provenance, anchor.syntax_error_overlap),
                ])?;
                let anchor_id = tx.last_insert_rowid();
                callable_id_by_entity
                    .entry(anchor.entity_key.as_str())
                    .or_insert(anchor_id);
            }

            for call in facts.call_sites {
                let caller_anchor_id = callable_id_by_entity
                    .get(call.caller_entity_key.as_str())
                    .copied()
                    .with_context(|| {
                        format!(
                            "call site caller {} has no anchor in the same revision",
                            call.caller_entity_key
                        )
                    })?;
                let callee_name_id = call
                    .callee_name
                    .as_deref()
                    .map(&mut intern_call_string)
                    .transpose()?;
                let qualified_name_id = call
                    .qualified_name
                    .as_deref()
                    .map(&mut intern_call_string)
                    .transpose()?;
                let guard_id = call
                    .guard
                    .as_deref()
                    .map(&mut intern_call_string)
                    .transpose()?;
                call_site_stmt.execute(params![
                    revision_id,
                    file_id,
                    caller_anchor_id,
                    call.expression_range.start_byte as i64,
                    call.expression_range.end_byte as i64,
                    call.callee_range.start_byte as i64,
                    call.callee_range.end_byte as i64,
                    call.callee_range.start.line as i64,
                    call.callee_range.start.character as i64,
                    call.callee_range.end.line as i64,
                    call.callee_range.end.character as i64,
                    callee_name_id,
                    qualified_name_id,
                    call_form_code(call.form),
                    call.argument_count.map(i64::from),
                    guard_id,
                    fact_flags(call.provenance, call.syntax_error_overlap),
                ])?;
            }
        }
    }
    tx.commit()?;
    Ok(())
}

fn digest_bytes(value: &str) -> Result<Vec<u8>> {
    if value.len() != 24 {
        anyhow::bail!("call digest must contain exactly 24 hexadecimal characters");
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair).context("call digest is not UTF-8")?;
            u8::from_str_radix(text, 16).context("call digest contains a non-hexadecimal byte")
        })
        .collect()
}

fn owner_kind_code(kind: crate::call_model::OwnerKindHint) -> i64 {
    match kind {
        crate::call_model::OwnerKindHint::Unknown => 0,
        crate::call_model::OwnerKindHint::Namespace => 1,
        crate::call_model::OwnerKindHint::Record => 2,
    }
}

fn callable_kind_code(kind: crate::call_model::CallableKind) -> i64 {
    match kind {
        crate::call_model::CallableKind::Function => 0,
        crate::call_model::CallableKind::SyntheticGlobalInitializer => 1,
        crate::call_model::CallableKind::SyntheticLambda => 2,
        crate::call_model::CallableKind::FunctionLikeMacro => 3,
    }
}

fn anchor_role_code(role: crate::call_model::AnchorRole) -> i64 {
    match role {
        crate::call_model::AnchorRole::Declaration => 0,
        crate::call_model::AnchorRole::Definition => 1,
        crate::call_model::AnchorRole::Synthetic => 2,
    }
}

fn call_form_code(form: crate::call_model::CallForm) -> i64 {
    match form {
        crate::call_model::CallForm::DirectName => 0,
        crate::call_model::CallForm::QualifiedName => 1,
        crate::call_model::CallForm::ParenthesizedName => 2,
        crate::call_model::CallForm::MemberDot => 3,
        crate::call_model::CallForm::MemberArrow => 4,
        crate::call_model::CallForm::StaticMember => 5,
        crate::call_model::CallForm::FunctionPointer => 6,
        crate::call_model::CallForm::CallableObject => 7,
        crate::call_model::CallForm::ExplicitConstruction => 8,
        crate::call_model::CallForm::Unsupported => 9,
    }
}

fn fact_flags(provenance: crate::call_model::FactProvenance, syntax_error: bool) -> i64 {
    let provenance = match provenance {
        crate::call_model::FactProvenance::Ast => 0,
        crate::call_model::FactProvenance::LexicalFallback => 1,
        crate::call_model::FactProvenance::Synthetic => 2,
    };
    provenance | (i64::from(syntax_error) << 8)
}
