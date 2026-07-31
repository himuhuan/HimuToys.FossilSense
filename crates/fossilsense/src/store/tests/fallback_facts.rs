use super::*;

use crate::call_model::{SourcePosition, SourceRange};
use crate::parser::{FactSource, ParseFacts};
use crate::semantic_model::{CompletionKindHint, FallbackCompletionFact, ParseOutcome};

fn fingerprint(path: &str, source: &str) -> FileFingerprint {
    FileFingerprint {
        path: path.to_string(),
        extension: path.rsplit('.').next().unwrap_or("c").to_string(),
        size: source.len() as u64,
        mtime_ns: 1,
        hash: format!("{path}-fallback-test"),
    }
}

fn fallback_fact(name: &str) -> FallbackCompletionFact {
    FallbackCompletionFact {
        name: name.to_string(),
        kind_hint: CompletionKindHint::Function,
        range: SourceRange {
            start: SourcePosition {
                line: 0,
                character: 4,
            },
            end: SourcePosition {
                line: 0,
                character: 11,
            },
            start_byte: 4,
            end_byte: 11,
        },
        detail: Some("guessed(int)".to_string()),
    }
}

fn force_completion_only_fallback(index: &mut crate::parser::FileSemanticIndex) {
    index.declarations.clear();
    index.fallback_completions = vec![fallback_fact("guessed")];
    index.parse_outcome = ParseOutcome::LexicalFallback;
    index.occurrences.clear();
    index.records.clear();
    index.fields.clear();
    index.members.clear();
    index.aliases.clear();
    index.callable_anchors.clear();
    index.call_sites.clear();
    index.local_declarations.clear();
    index.local_bindings.clear();
    index.diagnostics.fallback_used = true;
    index.diagnostics.ast_source = FactSource::LexicalFallback;
    index.diagnostics.requested_facts = ParseFacts::ALL;
}

#[test]
fn fallback_rows_are_isolated_from_canonical_declarations() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    let source = "int guessed(int value);\n";
    let mut index = crate::parser::parse(std::path::Path::new("broken.c"), source);
    force_completion_only_fallback(&mut index);

    store
        .upsert_file_index(&fingerprint("broken.c", source), &index)
        .expect("persist fallback");
    assert_eq!(store.declaration_count().expect("declarations"), 0);
    let rows = store
        .fallback_completion_view()
        .all()
        .expect("fallback rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "guessed");
}

#[test]
fn persistence_rejects_cross_contamination_in_both_directions() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    let source = "int stable(void);\n";

    let mut ast = crate::parser::parse(std::path::Path::new("ast.c"), source);
    ast.fallback_completions.push(fallback_fact("stable"));
    let error = store
        .upsert_file_index(&fingerprint("ast.c", source), &ast)
        .expect_err("AST revision must reject fallback rows");
    assert!(error.to_string().contains("fallback completions"));

    let mut fallback = crate::parser::parse(std::path::Path::new("fallback.c"), source);
    fallback.diagnostics.fallback_used = true;
    fallback.parse_outcome = ParseOutcome::LexicalFallback;
    fallback.fallback_completions.push(fallback_fact("stable"));
    let error = store
        .upsert_file_index(&fingerprint("fallback.c", source), &fallback)
        .expect_err("fallback revision must reject declarations");
    assert!(error.to_string().contains("persist declarations"));

    let mut inconsistent = crate::parser::parse(std::path::Path::new("inconsistent.c"), source);
    inconsistent.diagnostics.fallback_used = true;
    let error = store
        .upsert_file_index(&fingerprint("inconsistent.c", source), &inconsistent)
        .expect_err("AST outcome must not be persisted as fallback");
    assert!(error.to_string().contains("AST parse outcome"));
}

#[test]
fn schema_has_only_the_isolated_fallback_and_declaration_fact_tables() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let store = IndexStore::open(&db, dir.path()).expect("store");
    let old_objects: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('symbol_facts', 'symbols')",
            [],
            |row| row.get(0),
        )
        .expect("old objects");
    let new_tables: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table'
               AND name IN ('declaration_facts', 'fallback_completion_facts')",
            [],
            |row| row.get(0),
        )
        .expect("new tables");
    assert_eq!(old_objects, 0);
    assert_eq!(new_tables, 2);
}

#[test]
fn sqlite_trigger_rejects_fallback_rows_for_ast_revisions() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    upsert_source(&mut store, "ast.c", "int stable(void);\n");
    let (file_id, revision_id): (i64, i64) = store
        .conn
        .query_row(
            "SELECT file_id, revision_id FROM active_file_revisions LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("active revision");
    let result = store.conn.execute(
        "INSERT INTO fallback_completion_facts (
            revision_id, file_id, name, kind_hint, start_byte, end_byte,
            start_line, start_col, end_line, end_col, detail
         ) VALUES (?1, ?2, 'bad', 0, 0, 3, 0, 0, 0, 3, NULL)",
        rusqlite::params![revision_id, file_id],
    );
    assert!(result.is_err());

    let fallback_source = "int guessed(int value);\n";
    let mut fallback = crate::parser::parse(std::path::Path::new("fallback.c"), fallback_source);
    force_completion_only_fallback(&mut fallback);
    store
        .upsert_file_index(&fingerprint("fallback.c", fallback_source), &fallback)
        .expect("fallback revision");
    let fallback_fact_id: i64 = store
        .conn
        .query_row(
            "SELECT id FROM fallback_completion_facts LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("fallback fact");
    let update = store.conn.execute(
        "UPDATE fallback_completion_facts SET revision_id = ?1 WHERE id = ?2",
        rusqlite::params![revision_id, fallback_fact_id],
    );
    assert!(update
        .expect_err("update must not move fallback facts onto an AST revision")
        .to_string()
        .contains("AST revisions cannot store fallback completions"));
}

#[test]
fn sqlite_trigger_rejects_declarations_for_fallback_revisions() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    upsert_source(&mut store, "ast.c", "int stable(void);\n");
    let declaration_id: i64 = store
        .conn
        .query_row("SELECT id FROM declaration_facts LIMIT 1", [], |row| {
            row.get(0)
        })
        .expect("declaration fact");
    store
        .conn
        .execute(
            "INSERT INTO file_entries (
                path, extension, size, mtime_ns, hash, indexed_at, status, source
             ) VALUES ('fallback.c', 'c', 0, 0, 'fallback', 0, 'ok', 'workspace')",
            [],
        )
        .expect("file");
    let file_id = store.conn.last_insert_rowid();
    store
        .conn
        .execute(
            "INSERT INTO file_revisions (
                file_id, extension, size, mtime_ns, hash, indexed_at, status, source,
                parser_version, fact_mask, parse_error_count, fallback_used
             ) VALUES (?1, 'c', 0, 0, 'fallback', 0, 'ok', 'workspace', ?2, 0, 0, 1)",
            rusqlite::params![file_id, crate::parser::PARSER_FACT_VERSION],
        )
        .expect("revision");
    let revision_id = store.conn.last_insert_rowid();
    let result = store.conn.execute(
        "INSERT INTO declaration_facts (
            revision_id, file_id, name, qualified_name, declaration_kind, role,
            name_start_byte, name_end_byte, name_start_line, name_start_col,
            name_end_line, name_end_col, declaration_start_byte, declaration_end_byte,
            declaration_start_line, declaration_start_col, declaration_end_line,
            declaration_end_col, linkage_kind, language, language_fidelity, provenance,
            fact_fidelity, logical_key_digest, locator_fingerprint,
            logical_linkage_domain, backing_kind
         ) VALUES (
            ?1, ?2, 'bad', 'bad', 0, 0, 0, 3, 0, 0, 0, 3, 0, 3,
            0, 0, 0, 3, 0, 0, 0, 0, 0, zeroblob(12), zeroblob(12), 'external', 4
         )",
        rusqlite::params![revision_id, file_id],
    );
    assert!(result.is_err());
    assert!(result
        .expect_err("fallback declaration rejected")
        .to_string()
        .contains("fallback revisions cannot store declarations"));

    let update = store.conn.execute(
        "UPDATE declaration_facts SET revision_id = ?1 WHERE id = ?2",
        rusqlite::params![revision_id, declaration_id],
    );
    assert!(update
        .expect_err("update must not move declarations onto a fallback revision")
        .to_string()
        .contains("fallback revisions cannot store declarations"));
}
