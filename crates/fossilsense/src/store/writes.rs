use anyhow::Result;
use rusqlite::{params, Connection};

use crate::parser::AliasTarget;

use super::{
    include_normalized_metadata, now_unix_secs, record_confidence_to_str, record_kind_to_str,
    symbol_kind, symbol_role, FileIndexPayload, FileIndexUpdate,
};

pub(super) fn apply_file_updates_inner(
    conn: &mut Connection,
    updates: &[FileIndexUpdate<'_>],
    delete_existing_rows: bool,
) -> Result<()> {
    if updates.is_empty() {
        return Ok(());
    }

    let tx = conn.transaction()?;
    let indexed_at = now_unix_secs();
    {
        let mut ok_file_stmt = tx.prepare(
            "INSERT INTO files (path, extension, size, mtime_ns, hash, indexed_at, status, error, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'ok', NULL, ?7)
             ON CONFLICT(path) DO UPDATE SET
                extension = excluded.extension,
                size = excluded.size,
                mtime_ns = excluded.mtime_ns,
                hash = excluded.hash,
                indexed_at = excluded.indexed_at,
                status = excluded.status,
                error = excluded.error",
        )?;
        let mut error_file_stmt = tx.prepare(
            "INSERT INTO files (path, extension, size, mtime_ns, hash, indexed_at, status, error, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'error', ?7, ?8)
             ON CONFLICT(path) DO UPDATE SET
                extension = excluded.extension,
                size = excluded.size,
                mtime_ns = excluded.mtime_ns,
                hash = excluded.hash,
                indexed_at = excluded.indexed_at,
                status = excluded.status,
                error = excluded.error",
        )?;
        let mut file_id_stmt = tx.prepare("SELECT id FROM files WHERE path = ?1")?;
        let mut delete_symbols_stmt = tx.prepare("DELETE FROM symbols WHERE file_id = ?1")?;
        let mut delete_includes_stmt = tx.prepare("DELETE FROM includes WHERE file_id = ?1")?;
        let mut delete_aliases_stmt = tx.prepare("DELETE FROM type_aliases WHERE file_id = ?1")?;
        let mut symbol_stmt = tx.prepare(
            "INSERT INTO symbols (
                    file_id, name, kind, role, start_byte, end_byte,
                    start_line, start_col, end_line, end_col, signature, guard, container
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )?;
        let mut include_stmt = tx
            .prepare("INSERT INTO includes (file_id, line, target_text, target_form, target_normalized, target_basename) VALUES (?1, ?2, ?3, ?4, ?5, ?6)")?;
        let mut record_stmt = tx.prepare(
            "INSERT INTO record_defs (
                    file_id, display_name, tag_name, typedef_name, kind, start_byte, end_byte,
                    start_line, start_col, end_line, end_col, signature, confidence
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )?;
        let mut field_stmt = tx.prepare(
            "INSERT INTO fields (
                    record_id, name, start_byte, end_byte, start_line, start_col, end_line, end_col, signature
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;
        let mut alias_stmt = tx.prepare(
            "INSERT INTO type_aliases (
                    file_id, alias, start_byte, end_byte, start_line, start_col, end_line, end_col,
                    target_record_id, target_name, target_kind, confidence
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        )?;
        let mut delete_records_stmt = tx.prepare("DELETE FROM record_defs WHERE file_id = ?1")?;

        for update in updates {
            let fingerprint = update.fingerprint;
            match update.payload {
                FileIndexPayload::Ok(_) => {
                    ok_file_stmt.execute(params![
                        fingerprint.path.as_str(),
                        fingerprint.extension.as_str(),
                        fingerprint.size as i64,
                        fingerprint.mtime_ns,
                        fingerprint.hash.as_str(),
                        indexed_at,
                        update.source.as_str(),
                    ])?;
                }
                FileIndexPayload::Error(error) => {
                    error_file_stmt.execute(params![
                        fingerprint.path.as_str(),
                        fingerprint.extension.as_str(),
                        fingerprint.size as i64,
                        fingerprint.mtime_ns,
                        fingerprint.hash.as_str(),
                        indexed_at,
                        error,
                        update.source.as_str(),
                    ])?;
                }
            }

            let file_id: i64 =
                file_id_stmt.query_row([fingerprint.path.as_str()], |row| row.get(0))?;
            if delete_existing_rows {
                delete_symbols_stmt.execute([file_id])?;
                delete_includes_stmt.execute([file_id])?;
                delete_aliases_stmt.execute([file_id])?;
                delete_records_stmt.execute([file_id])?;
            }

            let FileIndexPayload::Ok(index) = update.payload else {
                continue;
            };

            for symbol in &index.symbols {
                symbol_stmt.execute(params![
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

            for include in &index.includes {
                let (form, normalized, basename) =
                    include_normalized_metadata(&include.target_text);
                include_stmt.execute(params![
                    file_id,
                    include.line as i64,
                    include.target_text.as_str(),
                    form,
                    normalized,
                    basename,
                ])?;
            }

            let mut record_key_to_id = std::collections::HashMap::new();
            for record in &index.records {
                record_stmt.execute(params![
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
            }

            for field in &index.fields {
                let record_id = record_key_to_id.get(&field.record_key).copied();
                if let Some(rid) = record_id {
                    field_stmt.execute(params![
                        rid,
                        field.name.as_str(),
                        field.start_byte as i64,
                        field.end_byte as i64,
                        field.start_line as i64,
                        field.start_col as i64,
                        field.end_line as i64,
                        field.end_col as i64,
                        field.signature.as_str(),
                    ])?;
                }
            }

            for alias in &index.aliases {
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
        }
    }
    tx.commit()?;
    Ok(())
}
