// Version 25 persists the import path of each indexed Go package. This powers
// bounded import-string completion without reading go.mod files or opening
// SQLite on the request hot path.
pub(crate) const SCHEMA_VERSION: i64 = 25;

pub(crate) const DROP_DATA_TABLES_SQL: &str = "
    DROP TABLE IF EXISTS pending_file_revisions;
    DROP TABLE IF EXISTS index_builds;
    DROP TABLE IF EXISTS active_file_revisions;
    DROP TABLE IF EXISTS symbol_facts;
    DROP TABLE IF EXISTS fallback_completion_facts;
    DROP TABLE IF EXISTS declaration_facts;
    DROP TABLE IF EXISTS import_facts;
    DROP TABLE IF EXISTS package_facts;
    DROP TABLE IF EXISTS type_alias_facts;
    DROP TABLE IF EXISTS call_site_facts;
    DROP TABLE IF EXISTS callable_anchor_facts;
    DROP TABLE IF EXISTS call_strings;
    DROP TABLE IF EXISTS member_facts;
    DROP TABLE IF EXISTS record_facts;
    DROP TABLE IF EXISTS go_open_packages;
    DROP TABLE IF EXISTS go_package_edges;
    DROP TABLE IF EXISTS go_importable_packages;
    DROP TABLE IF EXISTS include_edges;
    DROP TABLE IF EXISTS include_facts;
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
        parser_version INTEGER NOT NULL,
        language INTEGER NOT NULL DEFAULT 2 CHECK(language BETWEEN 0 AND 3),
        fact_mask INTEGER NOT NULL DEFAULT 0,
        parse_error_count INTEGER NOT NULL DEFAULT 0,
        fallback_used INTEGER NOT NULL DEFAULT 0 CHECK(fallback_used IN (0, 1)),
        build_guard TEXT
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

    CREATE TABLE IF NOT EXISTS fallback_completion_facts (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        revision_id INTEGER NOT NULL REFERENCES file_revisions(id) ON DELETE CASCADE,
        file_id INTEGER NOT NULL REFERENCES file_entries(id) ON DELETE CASCADE,
        name TEXT NOT NULL,
        kind_hint INTEGER NOT NULL CHECK(kind_hint BETWEEN 0 AND 3),
        start_byte INTEGER NOT NULL,
        end_byte INTEGER NOT NULL,
        start_line INTEGER NOT NULL,
        start_col INTEGER NOT NULL,
        end_line INTEGER NOT NULL,
        end_col INTEGER NOT NULL,
        detail TEXT
    );

    CREATE TABLE IF NOT EXISTS declaration_facts (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        revision_id INTEGER NOT NULL REFERENCES file_revisions(id) ON DELETE CASCADE,
        file_id INTEGER NOT NULL REFERENCES file_entries(id) ON DELETE CASCADE,
        name TEXT NOT NULL,
        qualified_name TEXT NOT NULL,
        declaration_kind INTEGER NOT NULL CHECK(declaration_kind BETWEEN 0 AND 6),
        role INTEGER NOT NULL CHECK(role BETWEEN 0 AND 3),
        name_start_byte INTEGER NOT NULL CHECK(name_start_byte >= 0),
        name_end_byte INTEGER NOT NULL CHECK(name_end_byte >= name_start_byte),
        name_start_line INTEGER NOT NULL,
        name_start_col INTEGER NOT NULL,
        name_end_line INTEGER NOT NULL,
        name_end_col INTEGER NOT NULL,
        declaration_start_byte INTEGER NOT NULL CHECK(declaration_start_byte >= 0),
        declaration_end_byte INTEGER NOT NULL CHECK(declaration_end_byte >= declaration_start_byte),
        declaration_start_line INTEGER NOT NULL,
        declaration_start_col INTEGER NOT NULL,
        declaration_end_line INTEGER NOT NULL,
        declaration_end_col INTEGER NOT NULL,
        canonical_signature TEXT,
        declarator_shape_json TEXT,
        has_initializer INTEGER CHECK(has_initializer IN (0, 1)),
        owner TEXT,
        linkage_kind INTEGER NOT NULL CHECK(linkage_kind BETWEEN 0 AND 3),
        guard TEXT,
        language INTEGER NOT NULL CHECK(language BETWEEN 0 AND 3),
        language_fidelity INTEGER NOT NULL CHECK(language_fidelity BETWEEN 0 AND 3),
        provenance INTEGER NOT NULL CHECK(provenance = 0),
        fact_fidelity INTEGER NOT NULL CHECK(fact_fidelity BETWEEN 0 AND 2),
        logical_key_digest BLOB NOT NULL
            CHECK(typeof(logical_key_digest) = 'blob' AND length(logical_key_digest) = 12),
        locator_fingerprint TEXT NOT NULL,
        logical_linkage_domain TEXT NOT NULL,
        guard_fingerprint TEXT,
        logical_canonical_signature TEXT,
        backing_kind TEXT NOT NULL,
        backing_id INTEGER,
        backing_key TEXT,
        backing_start_byte INTEGER,
        backing_end_byte INTEGER
    );

    CREATE TABLE IF NOT EXISTS package_facts (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        revision_id INTEGER NOT NULL UNIQUE REFERENCES file_revisions(id) ON DELETE CASCADE,
        file_id INTEGER NOT NULL REFERENCES file_entries(id) ON DELETE CASCADE,
        name TEXT NOT NULL,
        name_start_byte INTEGER NOT NULL CHECK(name_start_byte >= 0),
        name_end_byte INTEGER NOT NULL CHECK(name_end_byte >= name_start_byte),
        name_start_line INTEGER NOT NULL,
        name_start_col INTEGER NOT NULL,
        name_end_line INTEGER NOT NULL,
        name_end_col INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS import_facts (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        revision_id INTEGER NOT NULL REFERENCES file_revisions(id) ON DELETE CASCADE,
        file_id INTEGER NOT NULL REFERENCES file_entries(id) ON DELETE CASCADE,
        import_path TEXT NOT NULL,
        alias TEXT,
        path_start_byte INTEGER NOT NULL CHECK(path_start_byte >= 0),
        path_end_byte INTEGER NOT NULL CHECK(path_end_byte >= path_start_byte),
        path_start_line INTEGER NOT NULL,
        path_start_col INTEGER NOT NULL,
        path_end_line INTEGER NOT NULL,
        path_end_col INTEGER NOT NULL,
        declaration_start_byte INTEGER NOT NULL CHECK(declaration_start_byte >= 0),
        declaration_end_byte INTEGER NOT NULL CHECK(declaration_end_byte >= declaration_start_byte),
        declaration_start_line INTEGER NOT NULL,
        declaration_start_col INTEGER NOT NULL,
        declaration_end_line INTEGER NOT NULL,
        declaration_end_col INTEGER NOT NULL
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

    CREATE TABLE IF NOT EXISTS go_package_edges (
        source_package_key TEXT NOT NULL,
        target_package_key TEXT NOT NULL,
        resolution TEXT NOT NULL CHECK(resolution IN ('exact', 'heuristic')),
        PRIMARY KEY (source_package_key, target_package_key)
    ) WITHOUT ROWID;

    CREATE TABLE IF NOT EXISTS go_importable_packages (
        package_key TEXT PRIMARY KEY NOT NULL,
        import_path TEXT NOT NULL
    ) WITHOUT ROWID;

    CREATE TABLE IF NOT EXISTS go_open_packages (
        package_key TEXT PRIMARY KEY NOT NULL,
        reason TEXT NOT NULL CHECK(reason IN (
            'unresolved_import',
            'ambiguous_import',
            'unsupported_language_boundary',
            'build_constraint_unknown'
        ))
    ) WITHOUT ROWID;

    CREATE TABLE IF NOT EXISTS record_facts (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        revision_id INTEGER NOT NULL REFERENCES file_revisions(id) ON DELETE CASCADE,
        file_id INTEGER NOT NULL REFERENCES file_entries(id) ON DELETE CASCADE,
        record_key TEXT NOT NULL,
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
        body_start_byte INTEGER NOT NULL,
        body_end_byte INTEGER NOT NULL,
        body_start_line INTEGER NOT NULL,
        body_start_col INTEGER NOT NULL,
        body_end_line INTEGER NOT NULL,
        body_end_col INTEGER NOT NULL,
        declaration_start_byte INTEGER NOT NULL,
        declaration_end_byte INTEGER NOT NULL,
        declaration_start_line INTEGER NOT NULL,
        declaration_start_col INTEGER NOT NULL,
        declaration_end_line INTEGER NOT NULL,
        declaration_end_col INTEGER NOT NULL,
        range_fidelity TEXT NOT NULL,
        signature TEXT NOT NULL,
        confidence TEXT NOT NULL,
        declaration_hash BLOB NOT NULL
            CHECK(typeof(declaration_hash) = 'blob' AND length(declaration_hash) = 32)
    );

    CREATE TABLE IF NOT EXISTS member_facts (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        revision_id INTEGER NOT NULL REFERENCES file_revisions(id) ON DELETE CASCADE,
        file_id INTEGER NOT NULL REFERENCES file_entries(id) ON DELETE CASCADE,
        record_id INTEGER REFERENCES record_facts(id) ON DELETE SET NULL,
        record_key TEXT NOT NULL,
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
        declaration_start_byte INTEGER NOT NULL,
        declaration_end_byte INTEGER NOT NULL,
        declaration_start_line INTEGER NOT NULL,
        declaration_start_col INTEGER NOT NULL,
        declaration_end_line INTEGER NOT NULL,
        declaration_end_col INTEGER NOT NULL,
        underlying_spelling TEXT NOT NULL,
        declarator_shape TEXT NOT NULL,
        target_fidelity TEXT NOT NULL,
        fingerprint BLOB NOT NULL CHECK(typeof(fingerprint) = 'blob' AND length(fingerprint) = 12),
        target_record_id INTEGER REFERENCES record_facts(id) ON DELETE SET NULL,
        target_name TEXT,
        target_kind TEXT,
        confidence TEXT NOT NULL,
        declaration_hash BLOB NOT NULL
            CHECK(typeof(declaration_hash) = 'blob' AND length(declaration_hash) = 32)
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
        linkage_kind INTEGER NOT NULL CHECK(linkage_kind IN (0, 1, 2, 3)),
        linkage_file_id INTEGER REFERENCES call_strings(id),
        signature_id INTEGER NOT NULL REFERENCES call_strings(id),
        canonical_signature_id INTEGER NOT NULL REFERENCES call_strings(id),
        presentation_signature_id INTEGER NOT NULL REFERENCES call_strings(id),
        signature_fidelity INTEGER NOT NULL CHECK(signature_fidelity IN (0, 2)),
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
        flags INTEGER NOT NULL CHECK((flags & 255) IN (0, 2) AND (flags & -512) = 0)
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
        flags INTEGER NOT NULL CHECK((flags & 255) = 0 AND (flags & -512) = 0)
    );

    CREATE TRIGGER IF NOT EXISTS declaration_facts_require_ast_revision
    BEFORE INSERT ON declaration_facts
    WHEN (SELECT fallback_used FROM file_revisions WHERE id = NEW.revision_id) != 0
    BEGIN
        SELECT RAISE(ABORT, 'fallback revisions cannot store declarations');
    END;

    CREATE TRIGGER IF NOT EXISTS declaration_facts_update_requires_ast_revision
    BEFORE UPDATE ON declaration_facts
    WHEN (SELECT fallback_used FROM file_revisions WHERE id = NEW.revision_id) != 0
    BEGIN
        SELECT RAISE(ABORT, 'fallback revisions cannot store declarations');
    END;

    CREATE TRIGGER IF NOT EXISTS fallback_completion_facts_require_fallback_revision
    BEFORE INSERT ON fallback_completion_facts
    WHEN (SELECT fallback_used FROM file_revisions WHERE id = NEW.revision_id) != 1
    BEGIN
        SELECT RAISE(ABORT, 'AST revisions cannot store fallback completions');
    END;

    CREATE TRIGGER IF NOT EXISTS fallback_completion_facts_update_requires_fallback_revision
    BEFORE UPDATE ON fallback_completion_facts
    WHEN (SELECT fallback_used FROM file_revisions WHERE id = NEW.revision_id) != 1
    BEGIN
        SELECT RAISE(ABORT, 'AST revisions cannot store fallback completions');
    END;

    CREATE VIEW IF NOT EXISTS files AS
        SELECT f.* FROM file_entries f
        JOIN active_file_revisions a ON a.file_id = f.id;

    CREATE VIEW IF NOT EXISTS fallback_completions AS
        SELECT f.* FROM fallback_completion_facts f
        JOIN active_file_revisions a
          ON a.file_id = f.file_id AND a.revision_id = f.revision_id;

    CREATE VIEW IF NOT EXISTS declarations AS
        SELECT f.* FROM declaration_facts f
        JOIN active_file_revisions a
          ON a.file_id = f.file_id AND a.revision_id = f.revision_id;

    CREATE VIEW IF NOT EXISTS packages AS
        SELECT f.* FROM package_facts f
        JOIN active_file_revisions a
          ON a.file_id = f.file_id AND a.revision_id = f.revision_id;

    CREATE VIEW IF NOT EXISTS imports AS
        SELECT f.* FROM import_facts f
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
        JOIN active_file_revisions a
          ON a.file_id = m.file_id AND a.revision_id = m.revision_id;

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
    CREATE INDEX IF NOT EXISTS idx_fallback_completion_name ON fallback_completion_facts(name);
    CREATE INDEX IF NOT EXISTS idx_fallback_completion_file_id ON fallback_completion_facts(file_id);
    CREATE INDEX IF NOT EXISTS idx_declaration_facts_name ON declaration_facts(name);
    CREATE INDEX IF NOT EXISTS idx_declaration_facts_file_id ON declaration_facts(file_id);
    CREATE INDEX IF NOT EXISTS idx_declaration_facts_logical_key ON declaration_facts(logical_key_digest);
    CREATE INDEX IF NOT EXISTS idx_declaration_facts_locator ON declaration_facts(locator_fingerprint);
    CREATE INDEX IF NOT EXISTS idx_package_facts_name ON package_facts(name);
    CREATE INDEX IF NOT EXISTS idx_package_facts_file_id ON package_facts(file_id);
    CREATE INDEX IF NOT EXISTS idx_import_facts_path ON import_facts(import_path);
    CREATE INDEX IF NOT EXISTS idx_import_facts_file_id ON import_facts(file_id);
    CREATE INDEX IF NOT EXISTS idx_type_alias_facts_alias ON type_alias_facts(alias);
    CREATE INDEX IF NOT EXISTS idx_type_alias_facts_fingerprint ON type_alias_facts(fingerprint);
    CREATE INDEX IF NOT EXISTS idx_type_alias_facts_file_id ON type_alias_facts(file_id);
    CREATE INDEX IF NOT EXISTS idx_include_edges_src ON include_edges(src_file_id);
    CREATE INDEX IF NOT EXISTS idx_record_facts_display_name ON record_facts(display_name);
    CREATE INDEX IF NOT EXISTS idx_record_facts_tag_name ON record_facts(tag_name);
    CREATE INDEX IF NOT EXISTS idx_record_facts_typedef_name ON record_facts(typedef_name);
    CREATE INDEX IF NOT EXISTS idx_record_facts_file_id ON record_facts(file_id);
    CREATE INDEX IF NOT EXISTS idx_record_facts_record_key ON record_facts(record_key);
    CREATE INDEX IF NOT EXISTS idx_member_facts_record_id ON member_facts(record_id);
    CREATE INDEX IF NOT EXISTS idx_member_facts_record_key ON member_facts(record_key);
    CREATE INDEX IF NOT EXISTS idx_member_facts_file_id ON member_facts(file_id);
    CREATE INDEX IF NOT EXISTS idx_member_facts_name ON member_facts(name);
    CREATE INDEX IF NOT EXISTS idx_member_facts_kind ON member_facts(kind);
    CREATE INDEX IF NOT EXISTS idx_include_facts_target_basename ON include_facts(target_basename);
    CREATE INDEX IF NOT EXISTS idx_include_facts_target_normalized ON include_facts(target_normalized);
    CREATE INDEX IF NOT EXISTS idx_include_facts_file_id ON include_facts(file_id);
";

pub(crate) const CREATE_CALL_LOOKUP_INDEXES_SQL: &str = "
    CREATE UNIQUE INDEX IF NOT EXISTS idx_call_strings_text ON call_strings(text);
    CREATE INDEX IF NOT EXISTS idx_callable_anchor_name ON callable_anchor_facts(name_id);
    CREATE INDEX IF NOT EXISTS idx_callable_anchor_name_canonical ON callable_anchor_facts(name_id, canonical_signature_id);
    CREATE INDEX IF NOT EXISTS idx_callable_anchor_qualified_name ON callable_anchor_facts(qualified_name_id);
    CREATE INDEX IF NOT EXISTS idx_callable_anchor_entity_key ON callable_anchor_facts(entity_digest);
    CREATE INDEX IF NOT EXISTS idx_callable_anchor_canonical_signature ON callable_anchor_facts(canonical_signature_id);
    CREATE INDEX IF NOT EXISTS idx_callable_anchor_file_id ON callable_anchor_facts(file_id);
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
    DROP INDEX IF EXISTS idx_callable_anchor_name_canonical;
    DROP INDEX IF EXISTS idx_callable_anchor_qualified_name;
    DROP INDEX IF EXISTS idx_callable_anchor_entity_key;
    DROP INDEX IF EXISTS idx_callable_anchor_canonical_signature;
    DROP INDEX IF EXISTS idx_callable_anchor_file_id;
    DROP INDEX IF EXISTS idx_callable_anchor_revision;
    DROP INDEX IF EXISTS idx_call_site_caller;
    DROP INDEX IF EXISTS idx_call_site_callee_arity;
    DROP INDEX IF EXISTS idx_call_site_revision;
";
