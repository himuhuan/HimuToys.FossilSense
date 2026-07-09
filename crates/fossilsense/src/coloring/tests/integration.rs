use super::super::*;

#[test]
fn pipeline_colors_indexed_macro_type_and_local_def() {
    use crate::parser::parse;
    use crate::store::{FileFingerprint, IndexStore};
    use std::path::Path;
    use tempfile::tempdir;

    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");

    // Index a header defining a macro (PAGE_SIZE) and a type (widget_t).
    let header = "#define PAGE_SIZE 4096\ntypedef struct widget widget_t;\n";
    let header_index = parse(Path::new("defs.h"), header);
    store
        .upsert_file_index(
            &FileFingerprint {
                path: "defs.h".to_string(),
                extension: "h".to_string(),
                size: header.len() as u64,
                mtime_ns: 1,
                hash: "h".to_string(),
            },
            &header_index,
        )
        .expect("upsert header");

    // Current file uses the indexed names plus a locally-#defined macro.
    let current = "#define LOCAL 1\nint use(widget_t *w) { return PAGE_SIZE + LOCAL; }\n";
    let targets = parse(Path::new("use.c"), current);
    let defs = targets.coloring_defs();

    let reader = IndexStore::open_readonly(&db).expect("readonly");
    let wanted: Vec<&str> = targets
        .occurrences
        .iter()
        .map(|occ| occ.name.as_str())
        .filter(|name| !defs.macro_defs.contains(*name) && !defs.type_defs.contains(*name))
        .collect();
    let counts = reader.kind_counts_by_names(&wanted).expect("counts");

    let tokens = classify_occurrences(
        &targets.occurrences,
        &defs.macro_defs,
        &defs.type_defs,
        &defs.enum_defs,
        &counts,
    );

    let kind_of = |name: &str| -> Option<u32> {
        let occ = targets.occurrences.iter().find(|occ| occ.name == name)?;
        tokens
            .iter()
            .find(|token| token.line == occ.line && token.start == occ.start_col)
            .map(|token| token.token_type)
    };

    assert_eq!(kind_of("PAGE_SIZE"), Some(TOKEN_TYPE_MACRO)); // from index
    assert_eq!(kind_of("widget_t"), Some(TOKEN_TYPE_TYPE)); // from index
    assert_eq!(kind_of("LOCAL"), Some(TOKEN_TYPE_MACRO)); // current-file definition
    assert_eq!(kind_of("use"), None); // function name is not colored
}

#[test]
fn pipeline_suppresses_variable_named_like_indexed_type() {
    use crate::parser::parse;
    use crate::store::{FileFingerprint, IndexStore};
    use std::path::Path;
    use tempfile::tempdir;

    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");

    // Index a header that defines `Color` as a type.
    let header = "typedef int Color;\n";
    let header_index = parse(Path::new("color.h"), header);
    store
        .upsert_file_index(
            &FileFingerprint {
                path: "color.h".to_string(),
                extension: "h".to_string(),
                size: header.len() as u64,
                mtime_ns: 1,
                hash: "h".to_string(),
            },
            &header_index,
        )
        .expect("upsert header");

    // Current file: a real type use on line 0, a variable declaration that
    // shadows the type name on line 1.
    let current = "Color c;\nint Color;\n";
    let targets = parse(Path::new("use.c"), current);
    let defs = targets.coloring_defs();

    let reader = IndexStore::open_readonly(&db).expect("readonly");
    let wanted: Vec<&str> = targets
        .occurrences
        .iter()
        .map(|occ| occ.name.as_str())
        .filter(|name| !defs.type_defs.contains(*name))
        .collect();
    let counts = reader.kind_counts_by_names(&wanted).expect("counts");

    let tokens = classify_occurrences(
        &targets.occurrences,
        &defs.macro_defs,
        &defs.type_defs,
        &defs.enum_defs,
        &counts,
    );

    let type_at_line = |line: u32| -> bool {
        tokens
            .iter()
            .any(|t| t.line == line && t.token_type == TOKEN_TYPE_TYPE)
    };
    assert!(type_at_line(0), "genuine type use colored");
    assert!(
        !type_at_line(1),
        "shadowing variable declaration not colored"
    );
}

#[test]
fn scoped_projection_keeps_role_gate_suppress_only_for_shadowing_names() {
    use crate::parser::parse;
    use crate::query::NameTable;
    use crate::store::{FileFingerprint, IndexStore};
    use std::collections::HashSet;
    use std::path::Path;
    use tempfile::tempdir;

    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");

    {
        let mut index_file = |path: &str, source: &str, mtime_ns: u64| {
            let parsed = parse(Path::new(path), source);
            store
                .upsert_file_index(
                    &FileFingerprint {
                        path: path.to_string(),
                        extension: path.rsplit('.').next().unwrap_or("").to_string(),
                        size: source.len() as u64,
                        mtime_ns: mtime_ns as i64,
                        hash: format!("hash-{mtime_ns}"),
                    },
                    &parsed,
                )
                .expect("upsert");
        };
        index_file("reachable/color.h", "typedef int Color;\n", 1);
        index_file("unreachable/color.h", "#define Color 1\n", 2);
    }

    let current = "Color value;\nint Color;\n";
    let targets = parse(Path::new("src/use.c"), current);
    let defs = targets.coloring_defs();
    let wanted: HashSet<&str> = targets
        .occurrences
        .iter()
        .map(|occ| occ.name.as_str())
        .collect();

    let reader = IndexStore::open_readonly(&db).expect("readonly");
    let table = NameTable::build_with_paths(reader.load_symbol_names_with_paths().expect("names"));
    let scope = crate::query::CompletionScope {
        current_path: Some("src/use.c".to_string()),
        reach: crate::reachability::ReachScope {
            files: ["src/use.c".to_string(), "reachable/color.h".to_string()]
                .into_iter()
                .collect(),
            open: false,
            reason: None,
        },
    };
    let counts = table.colorable_kind_counts(&wanted, Some(&scope));
    let tokens = classify_occurrences(
        &targets.occurrences,
        &defs.macro_defs,
        &defs.type_defs,
        &defs.enum_defs,
        &counts,
    );

    let type_lines: Vec<u32> = tokens
        .iter()
        .filter(|token| token.token_type == TOKEN_TYPE_TYPE)
        .map(|token| token.line)
        .collect();
    assert_eq!(
        type_lines,
        vec![0],
        "scope may choose the indexed type, but role gating only suppresses incompatible positions"
    );
}

#[test]
fn parity_in_memory_counts_match_sql_coloring() {
    use crate::parser::parse;
    use crate::query::NameTable;
    use crate::store::{FileFingerprint, IndexStore};
    use std::collections::HashSet;
    use std::path::Path;
    use tempfile::tempdir;

    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");

    let mut index_file = |path: &str, src: &str| {
        let idx = parse(Path::new(path), src);
        store
            .upsert_file_index(
                &FileFingerprint {
                    path: path.to_string(),
                    extension: path.rsplit('.').next().unwrap().to_string(),
                    size: src.len() as u64,
                    mtime_ns: 1,
                    hash: "h".to_string(),
                },
                &idx,
            )
            .expect("upsert");
    };

    index_file(
        "defs.h",
        "#define PAGE_SIZE 4096\ntypedef struct widget widget_t;\nenum Color { RED, GREEN };\n",
    );
    // `AMBI` is a macro in one header and a type tag in another -> a tie that
    // resolves to no color unless the scope drops one side.
    index_file("a.h", "#define AMBI 1\n");
    index_file("b.h", "struct AMBI { int x; };\n");

    let current = "widget_t w;\nint use(void) { return PAGE_SIZE + RED + AMBI; }\n";
    let targets = parse(Path::new("use.c"), current);
    let defs = targets.coloring_defs();

    let mut wanted: Vec<&str> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for occ in &targets.occurrences {
        if defs.macro_defs.contains(&occ.name)
            || defs.type_defs.contains(&occ.name)
            || defs.enum_defs.contains(&occ.name)
        {
            continue;
        }
        if seen.insert(occ.name.as_str()) {
            wanted.push(occ.name.as_str());
        }
    }
    let wanted_set: HashSet<&str> = wanted.iter().copied().collect();

    let reader = IndexStore::open_readonly(&db).expect("readonly");
    let table = NameTable::build_with_paths(reader.load_symbol_names_with_paths().expect("names"));

    let classify = |counts: &HashMap<String, HashMap<String, usize>>| {
        classify_occurrences(
            &targets.occurrences,
            &defs.macro_defs,
            &defs.type_defs,
            &defs.enum_defs,
            counts,
        )
    };

    // Unscoped: SQL `workspace OR directly_included` vs in-memory
    // `colorable_kind_counts` with `None` (synthesizes an all-workspace
    // reachable set, preserving the prior unscoped gate via `scope_tier`).
    let sql_unscoped = reader
        .kind_counts_by_names_scoped(&wanted, None)
        .expect("sql unscoped");
    let mem_unscoped = table.colorable_kind_counts(&wanted_set, None);
    assert_eq!(
        classify(&sql_unscoped),
        classify(&mem_unscoped),
        "unscoped coloring parity"
    );

    // Scoped: a determinate set including defs.h + a.h but excluding b.h, so
    // AMBI's type side drops and it resolves to a macro under both paths.
    let scope_files: HashSet<String> = ["use.c", "defs.h", "a.h"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let sql_scoped = reader
        .kind_counts_by_names_scoped(&wanted, Some(&scope_files))
        .expect("sql scoped");
    // Build a CompletionScope carrying the reachable set + closed scope,
    // routed through the shared `scope_tier` primitive.
    let mem_scope = crate::query::CompletionScope {
        current_path: Some("use.c".to_string()),
        reach: crate::reachability::ReachScope {
            files: scope_files,
            open: false,
            reason: None,
        },
    };
    let mem_scoped = table.colorable_kind_counts(&wanted_set, Some(&mem_scope));
    assert_eq!(
        classify(&sql_scoped),
        classify(&mem_scoped),
        "scoped coloring parity"
    );

    // Sanity: the scope actually changed AMBI's outcome (tie -> macro), so the
    // parity above is non-trivial.
    assert_ne!(
        classify(&sql_unscoped),
        classify(&sql_scoped),
        "scope is expected to change the colored set"
    );
}
