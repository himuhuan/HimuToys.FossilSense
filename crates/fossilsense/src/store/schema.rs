pub(crate) const SCHEMA_VERSION: i64 = 9;

pub(crate) const DROP_DATA_TABLES_SQL: &str = "
    DROP TABLE IF EXISTS type_aliases;
    DROP TABLE IF EXISTS members;
    DROP TABLE IF EXISTS fields;
    DROP TABLE IF EXISTS record_defs;
    DROP TABLE IF EXISTS include_edges;
    DROP TABLE IF EXISTS includes;
    DROP TABLE IF EXISTS symbols;
    DROP TABLE IF EXISTS files;
";

pub(crate) const CREATE_SCHEMA_SQL: &str = "
    CREATE TABLE IF NOT EXISTS meta (
        key TEXT PRIMARY KEY NOT NULL,
        value TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS files (
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

    CREATE TABLE IF NOT EXISTS symbols (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
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

    CREATE TABLE IF NOT EXISTS includes (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
        line INTEGER NOT NULL,
        target_text TEXT NOT NULL,
        target_form TEXT NOT NULL DEFAULT 'unknown',
        target_normalized TEXT NOT NULL DEFAULT '',
        target_basename TEXT NOT NULL DEFAULT ''
    );

    CREATE TABLE IF NOT EXISTS include_edges (
        src_file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
        dst_file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
        resolution TEXT NOT NULL DEFAULT 'suffix_match',
        PRIMARY KEY (src_file_id, dst_file_id)
    ) WITHOUT ROWID;

    CREATE TABLE IF NOT EXISTS record_defs (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
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

    CREATE TABLE IF NOT EXISTS members (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        record_id INTEGER NOT NULL REFERENCES record_defs(id) ON DELETE CASCADE,
        name TEXT NOT NULL,
        kind TEXT NOT NULL,
        confidence TEXT NOT NULL,
        start_byte INTEGER NOT NULL,
        end_byte INTEGER NOT NULL,
        start_line INTEGER NOT NULL,
        start_col INTEGER NOT NULL,
        end_line INTEGER NOT NULL,
        end_col INTEGER NOT NULL,
        signature TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS type_aliases (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
        alias TEXT NOT NULL,
        start_byte INTEGER NOT NULL,
        end_byte INTEGER NOT NULL,
        start_line INTEGER NOT NULL,
        start_col INTEGER NOT NULL,
        end_line INTEGER NOT NULL,
        end_col INTEGER NOT NULL,
        target_record_id INTEGER REFERENCES record_defs(id) ON DELETE SET NULL,
        target_name TEXT,
        target_kind TEXT,
        confidence TEXT NOT NULL
    );
";

pub(crate) const DROP_LOOKUP_INDEXES_SQL: &str = "
    DROP INDEX IF EXISTS idx_files_source;
    DROP INDEX IF EXISTS idx_symbols_name;
    DROP INDEX IF EXISTS idx_symbols_file_id;
    DROP INDEX IF EXISTS idx_symbols_container;
    DROP INDEX IF EXISTS idx_type_aliases_alias;
    DROP INDEX IF EXISTS idx_type_aliases_file_id;
    DROP INDEX IF EXISTS idx_include_edges_src;
    DROP INDEX IF EXISTS idx_record_defs_display_name;
    DROP INDEX IF EXISTS idx_record_defs_tag_name;
    DROP INDEX IF EXISTS idx_record_defs_typedef_name;
    DROP INDEX IF EXISTS idx_record_defs_file_id;
    DROP INDEX IF EXISTS idx_members_record_id;
    DROP INDEX IF EXISTS idx_members_name;
    DROP INDEX IF EXISTS idx_members_kind;
    DROP INDEX IF EXISTS idx_fields_record_id;
    DROP INDEX IF EXISTS idx_fields_name;
    DROP INDEX IF EXISTS idx_includes_target_basename;
    DROP INDEX IF EXISTS idx_includes_target_normalized;
    DROP INDEX IF EXISTS idx_includes_file_id;
";

pub(crate) const CREATE_LOOKUP_INDEXES_SQL: &str = "
    CREATE INDEX IF NOT EXISTS idx_files_source ON files(source);
    CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
    CREATE INDEX IF NOT EXISTS idx_symbols_file_id ON symbols(file_id);
    CREATE INDEX IF NOT EXISTS idx_type_aliases_alias ON type_aliases(alias);
    CREATE INDEX IF NOT EXISTS idx_type_aliases_file_id ON type_aliases(file_id);
    CREATE INDEX IF NOT EXISTS idx_include_edges_src ON include_edges(src_file_id);
    CREATE INDEX IF NOT EXISTS idx_record_defs_display_name ON record_defs(display_name);
    CREATE INDEX IF NOT EXISTS idx_record_defs_tag_name ON record_defs(tag_name);
    CREATE INDEX IF NOT EXISTS idx_record_defs_typedef_name ON record_defs(typedef_name);
    CREATE INDEX IF NOT EXISTS idx_record_defs_file_id ON record_defs(file_id);
    CREATE INDEX IF NOT EXISTS idx_members_record_id ON members(record_id);
    CREATE INDEX IF NOT EXISTS idx_members_name ON members(name);
    CREATE INDEX IF NOT EXISTS idx_members_kind ON members(kind);
    CREATE INDEX IF NOT EXISTS idx_includes_target_basename ON includes(target_basename);
    CREATE INDEX IF NOT EXISTS idx_includes_target_normalized ON includes(target_normalized);
    CREATE INDEX IF NOT EXISTS idx_includes_file_id ON includes(file_id);
";
