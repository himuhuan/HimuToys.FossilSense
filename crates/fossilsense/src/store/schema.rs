// Version 15 interns repeated call strings, stores digests as BLOBs, uses
// integer enums/flags, and relates sites to compact caller anchor IDs.
pub(crate) const SCHEMA_VERSION: i64 = 15;

pub(crate) const DROP_DATA_TABLES_SQL: &str = "
    DROP TABLE IF EXISTS pending_file_revisions;
    DROP TABLE IF EXISTS index_builds;
    DROP TABLE IF EXISTS active_file_revisions;
    DROP TABLE IF EXISTS type_alias_facts;
    DROP TABLE IF EXISTS call_site_facts;
    DROP TABLE IF EXISTS callable_anchor_facts;
    DROP TABLE IF EXISTS call_strings;
    DROP TABLE IF EXISTS member_facts;
    DROP TABLE IF EXISTS record_facts;
    DROP TABLE IF EXISTS include_edges;
    DROP TABLE IF EXISTS include_facts;
    DROP TABLE IF EXISTS symbol_facts;
    DROP TABLE IF EXISTS file_revisions;
    DROP TABLE IF EXISTS fields;
    DROP TABLE IF EXISTS file_entries;
";

pub(crate) const CREATE_SCHEMA_SQL: &str = "
    CREATE TABLE IF NOT EXISTS meta (
        key TEXT PRIMARY KEY NOT NULL,
        value TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS file_entries (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        path TEXT NOT NULL UNIQUE,
        extension TEXT NOT NULL,
        size INTEGER NOT NULL,
        mtime_ns INTEGER NOT NULL,
        hash TEXT NOT NULL,
        indexed_at INTEGER NOT NULL,
        status TEXT NOT NULL,
        error TEXT,
        source TEXT NOT NULL DEFAULT 'workspace',
        directly_included INTEGER NOT NULL DEFAULT 0,
        unresolved_includes INTEGER NOT NULL DEFAULT 0,
        ambiguous_includes INTEGER NOT NULL DEFAULT 0
    );

    CREATE TABLE IF NOT EXISTS file_revisions (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        file_id INTEGER NOT NULL REFERENCES file_entries(id) ON DELETE CASCADE,
        extension TEXT NOT NULL,
        size INTEGER NOT NULL,
        mtime_ns INTEGER NOT NULL,
        hash TEXT NOT NULL,
        indexed_at INTEGER NOT NULL,
        status TEXT NOT NULL,
        error TEXT,
        source TEXT NOT NULL,
        parser_version INTEGER NOT NULL DEFAULT 1,
        fact_mask INTEGER NOT NULL DEFAULT 0,
        parse_error_count INTEGER NOT NULL DEFAULT 0,
        fallback_used INTEGER NOT NULL DEFAULT 0
    );

    CREATE TABLE IF NOT EXISTS active_file_revisions (
        file_id INTEGER PRIMARY KEY REFERENCES file_entries(id) ON DELETE CASCADE,
        revision_id INTEGER NOT NULL UNIQUE REFERENCES file_revisions(id) ON DELETE CASCADE
    ) WITHOUT ROWID;

    CREATE TABLE IF NOT EXISTS index_builds (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        target_generation INTEGER NOT NULL UNIQUE,
        full_rebuild INTEGER NOT NULL,
        state TEXT NOT NULL,
        created_at INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS pending_file_revisions (
        build_id INTEGER NOT NULL REFERENCES index_builds(id) ON DELETE CASCADE,
        file_id INTEGER NOT NULL REFERENCES file_entries(id) ON DELETE CASCADE,
        revision_id INTEGER REFERENCES file_revisions(id) ON DELETE CASCADE,
        PRIMARY KEY (build_id, file_id)
    ) WITHOUT ROWID;

    CREATE TABLE IF NOT EXISTS symbol_facts (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        revision_id INTEGER NOT NULL REFERENCES file_revisions(id) ON DELETE CASCADE,
        file_id INTEGER NOT NULL REFERENCES file_entries(id) ON DELETE CASCADE,
        name TEXT NOT NULL,
        kind TEXT NOT NULL,
        role TEXT NOT NULL,
        start_byte INTEGER NOT NULL,
        end_byte INTEGER NOT NULL,
        start_line INTEGER NOT NULL,
        start_col INTEGER NOT NULL,
        end_line INTEGER NOT NULL,
        end_col INTEGER NOT NULL,
        signature TEXT NOT NULL,
        guard TEXT,
        container TEXT
    );

    CREATE TABLE IF NOT EXISTS include_facts (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        revision_id INTEGER NOT NULL REFERENCES file_revisions(id) ON DELETE CASCADE,
        file_id INTEGER NOT NULL REFERENCES file_entries(id) ON DELETE CASCADE,
        line INTEGER NOT NULL,
        target_text TEXT NOT NULL,
        target_form TEXT NOT NULL DEFAULT 'unknown',
        target_normalized TEXT NOT NULL DEFAULT '',
        target_basename TEXT NOT NULL DEFAULT ''
    );

    CREATE TABLE IF NOT EXISTS include_edges (
        src_file_id INTEGER NOT NULL REFERENCES file_entries(id) ON DELETE CASCADE,
        dst_file_id INTEGER NOT NULL REFERENCES file_entries(id) ON DELETE CASCADE,
        resolution TEXT NOT NULL DEFAULT 'suffix_match',
        PRIMARY KEY (src_file_id, dst_file_id)
    ) WITHOUT ROWID;

    CREATE TABLE IF NOT EXISTS record_facts (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        revision_id INTEGER NOT NULL REFERENCES file_revisions(id) ON DELETE CASCADE,
        file_id INTEGER NOT NULL REFERENCES file_entries(id) ON DELETE CASCADE,
        display_name TEXT NOT NULL,
        tag_name TEXT,
        typedef_name TEXT,
        kind TEXT NOT NULL,
        start_byte INTEGER NOT NULL,
        end_byte INTEGER NOT NULL,
        start_line INTEGER NOT NULL,
        start_col INTEGER NOT NULL,
        end_line INTEGER NOT NULL,
        end_col INTEGER NOT NULL,
        signature TEXT NOT NULL,
        confidence TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS member_facts (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        record_id INTEGER NOT NULL REFERENCES record_facts(id) ON DELETE CASCADE,
        name TEXT NOT NULL,
        kind TEXT NOT NULL,
        confidence TEXT NOT NULL,
        start_byte INTEGER NOT NULL,
        end_byte INTEGER NOT NULL,
        start_line INTEGER NOT NULL,
        start_col INTEGER NOT NULL,
        end_line INTEGER NOT NULL,
        end_col INTEGER NOT NULL,
        signature TEXT NOT NULL,
        type_name TEXT
    );

    CREATE TABLE IF NOT EXISTS type_alias_facts (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        revision_id INTEGER NOT NULL REFERENCES file_revisions(id) ON DELETE CASCADE,
        file_id INTEGER NOT NULL REFERENCES file_entries(id) ON DELETE CASCADE,
        alias TEXT NOT NULL,
        start_byte INTEGER NOT NULL,
        end_byte INTEGER NOT NULL,
        start_line INTEGER NOT NULL,
        start_col INTEGER NOT NULL,
        end_line INTEGER NOT NULL,
        end_col INTEGER NOT NULL,
        target_record_id INTEGER REFERENCES record_facts(id) ON DELETE SET NULL,
        target_name TEXT,
        target_kind TEXT,
        confidence TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS call_strings (
        id INTEGER PRIMARY KEY,
        text TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS callable_anchor_facts (
        id INTEGER PRIMARY KEY,
        revision_id INTEGER NOT NULL REFERENCES file_revisions(id) ON DELETE CASCADE,
        file_id INTEGER NOT NULL REFERENCES file_entries(id) ON DELETE CASCADE,
        entity_digest BLOB NOT NULL CHECK(typeof(entity_digest) = 'blob' AND length(entity_digest) = 12),
        anchor_digest BLOB NOT NULL CHECK(typeof(anchor_digest) = 'blob' AND length(anchor_digest) = 12),
        name_id INTEGER NOT NULL REFERENCES call_strings(id),
        qualified_name_id INTEGER NOT NULL REFERENCES call_strings(id),
        owner_id INTEGER REFERENCES call_strings(id),
        owner_kind INTEGER CHECK(owner_kind IS NULL OR owner_kind IN (0, 1, 2)),
        kind INTEGER NOT NULL CHECK(kind IN (0, 1, 2, 3)),
        role INTEGER NOT NULL CHECK(role IN (0, 1, 2)),
        linkage_kind INTEGER NOT NULL CHECK(linkage_kind IN (0, 1, 2)),
        linkage_file_id INTEGER REFERENCES call_strings(id),
        signature_id INTEGER NOT NULL REFERENCES call_strings(id),
        min_arity INTEGER,
        max_arity INTEGER,
        variadic INTEGER NOT NULL CHECK(variadic IN (0, 1)),
        name_start_byte INTEGER NOT NULL CHECK(name_start_byte >= 0),
        name_end_byte INTEGER NOT NULL CHECK(name_end_byte >= name_start_byte),
        name_start_line INTEGER NOT NULL,
        name_start_col INTEGER NOT NULL,
        name_end_line INTEGER NOT NULL,
        name_end_col INTEGER NOT NULL,
        declaration_start_byte INTEGER NOT NULL,
        declaration_end_byte INTEGER NOT NULL,
        declaration_start_line INTEGER NOT NULL,
        declaration_start_col INTEGER NOT NULL,
        declaration_end_line INTEGER NOT NULL,
        declaration_end_col INTEGER NOT NULL,
        body_start_byte INTEGER,
        body_end_byte INTEGER,
        body_start_line INTEGER,
        body_start_col INTEGER,
        body_end_line INTEGER,
        body_end_col INTEGER,
        guard_id INTEGER REFERENCES call_strings(id),
        flags INTEGER NOT NULL CHECK((flags & 255) IN (0, 1, 2) AND (flags & -512) = 0)
    );

    CREATE TABLE IF NOT EXISTS call_site_facts (
        id INTEGER PRIMARY KEY,
        revision_id INTEGER NOT NULL REFERENCES file_revisions(id) ON DELETE CASCADE,
        file_id INTEGER NOT NULL REFERENCES file_entries(id) ON DELETE CASCADE,
        caller_anchor_id INTEGER NOT NULL REFERENCES callable_anchor_facts(id) ON DELETE CASCADE,
        expression_start_byte INTEGER NOT NULL CHECK(expression_start_byte >= 0),
        expression_end_byte INTEGER NOT NULL CHECK(expression_end_byte >= expression_start_byte),
        callee_start_byte INTEGER NOT NULL CHECK(callee_start_byte >= expression_start_byte),
        callee_end_byte INTEGER NOT NULL CHECK(callee_end_byte >= callee_start_byte AND callee_end_byte <= expression_end_byte),
        callee_start_line INTEGER NOT NULL,
        callee_start_col INTEGER NOT NULL,
        callee_end_line INTEGER NOT NULL,
        callee_end_col INTEGER NOT NULL,
        callee_name_id INTEGER REFERENCES call_strings(id),
        qualified_name_id INTEGER REFERENCES call_strings(id),
        call_form INTEGER NOT NULL CHECK(call_form BETWEEN 0 AND 9),
        argument_count INTEGER,
        guard_id INTEGER REFERENCES call_strings(id),
        flags INTEGER NOT NULL CHECK((flags & 255) IN (0, 1, 2) AND (flags & -512) = 0)
    );

    CREATE VIEW IF NOT EXISTS files AS
        SELECT f.* FROM file_entries f
        JOIN active_file_revisions a ON a.file_id = f.id;

    CREATE VIEW IF NOT EXISTS symbols AS
        SELECT f.* FROM symbol_facts f
        JOIN active_file_revisions a
          ON a.file_id = f.file_id AND a.revision_id = f.revision_id;

    CREATE VIEW IF NOT EXISTS includes AS
        SELECT f.* FROM include_facts f
        JOIN active_file_revisions a
          ON a.file_id = f.file_id AND a.revision_id = f.revision_id;

    CREATE VIEW IF NOT EXISTS record_defs AS
        SELECT f.* FROM record_facts f
        JOIN active_file_revisions a
          ON a.file_id = f.file_id AND a.revision_id = f.revision_id;

    CREATE VIEW IF NOT EXISTS members AS
        SELECT m.* FROM member_facts m
        JOIN record_facts r ON r.id = m.record_id
        JOIN active_file_revisions a
          ON a.file_id = r.file_id AND a.revision_id = r.revision_id;

    CREATE VIEW IF NOT EXISTS type_aliases AS
        SELECT f.* FROM type_alias_facts f
        JOIN active_file_revisions a
          ON a.file_id = f.file_id AND a.revision_id = f.revision_id;

    CREATE VIEW IF NOT EXISTS callable_anchors AS
        SELECT f.* FROM callable_anchor_facts f
        JOIN active_file_revisions a
          ON a.file_id = f.file_id AND a.revision_id = f.revision_id;

    CREATE VIEW IF NOT EXISTS call_sites AS
        SELECT f.* FROM call_site_facts f
        JOIN active_file_revisions a
          ON a.file_id = f.file_id AND a.revision_id = f.revision_id;
";

pub(crate) const CREATE_LOOKUP_INDEXES_SQL: &str = "
    CREATE INDEX IF NOT EXISTS idx_files_source ON file_entries(source);
    CREATE INDEX IF NOT EXISTS idx_file_revisions_file_id ON file_revisions(file_id);
    CREATE INDEX IF NOT EXISTS idx_symbol_facts_name ON symbol_facts(name);
    CREATE INDEX IF NOT EXISTS idx_symbol_facts_file_id ON symbol_facts(file_id);
    CREATE INDEX IF NOT EXISTS idx_type_alias_facts_alias ON type_alias_facts(alias);
    CREATE INDEX IF NOT EXISTS idx_type_alias_facts_file_id ON type_alias_facts(file_id);
    CREATE INDEX IF NOT EXISTS idx_include_edges_src ON include_edges(src_file_id);
    CREATE INDEX IF NOT EXISTS idx_record_facts_display_name ON record_facts(display_name);
    CREATE INDEX IF NOT EXISTS idx_record_facts_tag_name ON record_facts(tag_name);
    CREATE INDEX IF NOT EXISTS idx_record_facts_typedef_name ON record_facts(typedef_name);
    CREATE INDEX IF NOT EXISTS idx_record_facts_file_id ON record_facts(file_id);
    CREATE INDEX IF NOT EXISTS idx_member_facts_record_id ON member_facts(record_id);
    CREATE INDEX IF NOT EXISTS idx_member_facts_name ON member_facts(name);
    CREATE INDEX IF NOT EXISTS idx_member_facts_kind ON member_facts(kind);
    CREATE INDEX IF NOT EXISTS idx_include_facts_target_basename ON include_facts(target_basename);
    CREATE INDEX IF NOT EXISTS idx_include_facts_target_normalized ON include_facts(target_normalized);
    CREATE INDEX IF NOT EXISTS idx_include_facts_file_id ON include_facts(file_id);
";

pub(crate) const CREATE_CALL_LOOKUP_INDEXES_SQL: &str = "
    CREATE UNIQUE INDEX IF NOT EXISTS idx_call_strings_text ON call_strings(text);
    CREATE INDEX IF NOT EXISTS idx_callable_anchor_name ON callable_anchor_facts(name_id);
    CREATE INDEX IF NOT EXISTS idx_callable_anchor_qualified_name ON callable_anchor_facts(qualified_name_id);
    CREATE INDEX IF NOT EXISTS idx_callable_anchor_entity_key ON callable_anchor_facts(entity_digest);
    CREATE INDEX IF NOT EXISTS idx_callable_anchor_revision ON callable_anchor_facts(revision_id);
    CREATE INDEX IF NOT EXISTS idx_call_site_caller ON call_site_facts(caller_anchor_id);
    CREATE INDEX IF NOT EXISTS idx_call_site_callee_arity ON call_site_facts(callee_name_id, argument_count);
    CREATE INDEX IF NOT EXISTS idx_call_site_revision ON call_site_facts(revision_id);
";

pub(crate) const CREATE_CALL_STRING_INDEX_SQL: &str = "
    CREATE UNIQUE INDEX IF NOT EXISTS idx_call_strings_text ON call_strings(text);
";

pub(crate) const DROP_CALL_LOOKUP_INDEXES_SQL: &str = "
    DROP INDEX IF EXISTS idx_call_strings_text;
    DROP INDEX IF EXISTS idx_callable_anchor_name;
    DROP INDEX IF EXISTS idx_callable_anchor_qualified_name;
    DROP INDEX IF EXISTS idx_callable_anchor_entity_key;
    DROP INDEX IF EXISTS idx_callable_anchor_revision;
    DROP INDEX IF EXISTS idx_call_site_caller;
    DROP INDEX IF EXISTS idx_call_site_callee_arity;
    DROP INDEX IF EXISTS idx_call_site_revision;
";
