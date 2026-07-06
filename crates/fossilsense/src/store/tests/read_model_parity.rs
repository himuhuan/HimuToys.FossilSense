use super::*;

fn file_id(store: &IndexStore, path: &str) -> i64 {
    store
        .files_with_ids()
        .expect("files")
        .into_iter()
        .find(|(_, file_path, _)| file_path == path)
        .map(|(id, _, _)| id)
        .unwrap_or_else(|| panic!("missing file id for {path}"))
}

#[test]
fn open_readonly_reads_committed_wal_backed_index() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    store
        .conn
        .pragma_update(None, "wal_autocheckpoint", 0)
        .expect("disable autocheckpoint");
    let journal_mode: String = store
        .conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("journal mode");
    assert_eq!(journal_mode.to_ascii_lowercase(), "wal");

    upsert_source(
        &mut store,
        "src/wal.c",
        "int wal_backed_symbol(void) { return 1; }\n",
    );
    assert!(
        db.with_extension("sqlite-wal").exists(),
        "writer should leave committed data in the WAL while it remains open"
    );

    let reader = IndexStore::open_readonly(&db).expect("readonly");
    let symbols = reader
        .symbols_by_name("wal_backed_symbol")
        .expect("symbols");
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].path, "src/wal.c");
}

#[test]
fn name_table_loader_rows_preserve_source_path_kind_and_direct_external_evidence() {
    use crate::model::ScopeTier;
    use crate::query::{CompletionScope, NameTable};

    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    upsert_with_source(
        &mut store,
        "src/main.c",
        "int main_entry(void) { return 0; }\n",
        FileSource::Workspace,
    );
    upsert_with_source(
        &mut store,
        "C:/sdk/include/ext_size.h",
        "typedef unsigned long ext_size_t;\n",
        FileSource::External,
    );
    store
        .mark_directly_included(&["C:/sdk/include/ext_size.h".to_string()])
        .expect("direct external");

    let reader = IndexStore::open_readonly(&db).expect("readonly");
    let rows = reader.load_symbol_names_with_paths().expect("names");
    let main = rows
        .iter()
        .find(|(_, name, _, _, _, _)| name == "main_entry")
        .expect("main row");
    assert_eq!(
        (main.2, main.3.as_str(), main.4.as_str(), main.5),
        (false, "src/main.c", "function", false)
    );
    let external = rows
        .iter()
        .find(|(_, name, _, _, _, _)| name == "ext_size_t")
        .expect("external row");
    assert_eq!(
        (
            external.2,
            external.3.as_str(),
            external.4.as_str(),
            external.5
        ),
        (true, "C:/sdk/include/ext_size.h", "type", true)
    );

    let table = NameTable::build_with_paths(rows);
    let scope = CompletionScope {
        current_path: Some("src/main.c".to_string()),
        reach: crate::reachability::ReachScope {
            files: ["src/main.c".to_string()].into_iter().collect(),
            open: false,
            reason: None,
        },
    };
    let main_hit = table
        .search_ranked_scoped("main_entry", 10, Some(&scope))
        .into_iter()
        .find(|hit| hit.name == "main_entry")
        .expect("main hit");
    assert_eq!(main_hit.tier, ScopeTier::Current);
    assert_eq!(main_hit.kind, crate::parser::SymbolKind::Function);

    let external_hit = table
        .search_ranked_scoped("ext_size", 10, Some(&scope))
        .into_iter()
        .find(|hit| hit.name == "ext_size_t")
        .expect("external hit");
    assert_eq!(external_hit.tier, ScopeTier::External);
    assert_eq!(external_hit.kind, crate::parser::SymbolKind::Type);
}

#[test]
fn reach_graph_store_inputs_preserve_edges_open_reasons_and_incremental_refresh_rows() {
    use crate::reachability::{OpenReason, ReachGraph};

    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    for path in ["src/a.c", "include/b.h", "src/c.c", "include/d.h"] {
        upsert_source(&mut store, path, "int marker;\n");
    }
    let id = |store: &IndexStore, path: &str| file_id(store, path);

    store
        .replace_include_edges(
            &[id(&store, "src/a.c"), id(&store, "src/c.c")],
            &[
                (
                    id(&store, "src/a.c"),
                    id(&store, "include/b.h"),
                    "workspace_exact".to_string(),
                ),
                (
                    id(&store, "src/c.c"),
                    id(&store, "include/d.h"),
                    "relative_exact".to_string(),
                ),
            ],
            &[(id(&store, "src/a.c"), 1)],
            &[(id(&store, "src/c.c"), 1)],
            true,
        )
        .expect("seed edges");

    let mut graph = ReachGraph::new(
        store.load_include_edge_paths().expect("edge rows"),
        store.open_include_file_paths().expect("unresolved rows"),
        store
            .ambiguous_include_file_paths()
            .expect("ambiguous rows"),
    );
    assert_eq!(
        graph.reachable("src/a.c").reason,
        Some(OpenReason::UnresolvedInclude)
    );
    assert_eq!(
        graph.reachable("src/c.c").reason,
        Some(OpenReason::AmbiguousInclude)
    );

    store
        .replace_include_edges(
            &[id(&store, "src/a.c")],
            &[(
                id(&store, "src/a.c"),
                id(&store, "include/d.h"),
                "workspace_exact".to_string(),
            )],
            &[],
            &[],
            false,
        )
        .expect("refresh a");
    let sources = vec!["src/a.c".to_string()];
    let (edges, open) = store
        .load_include_data_for_sources(&sources)
        .expect("incremental rows");
    assert_eq!(
        edges,
        vec![("src/a.c".to_string(), "include/d.h".to_string())]
    );
    assert!(open.is_empty());

    graph.refresh_sources(&sources, edges, open);
    let refreshed = graph.reachable("src/a.c");
    assert!(refreshed.files.contains("include/d.h"));
    assert!(!refreshed.files.contains("include/b.h"));
    assert!(!refreshed.open);
    assert_eq!(
        graph.reachable("src/c.c").reason,
        Some(OpenReason::AmbiguousInclude),
        "refreshing a source preserves unrelated open sources"
    );
}

#[test]
fn include_completion_store_inputs_preserve_workspace_paths_and_source_destination_edges() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    upsert_with_source(
        &mut store,
        "include/z.h",
        "int z_header;\n",
        FileSource::Workspace,
    );
    upsert_with_source(
        &mut store,
        "src/main.c",
        "int main_file;\n",
        FileSource::Workspace,
    );
    upsert_with_source(
        &mut store,
        "C:/sdk/include/external.h",
        "int external_header;\n",
        FileSource::External,
    );

    let paths = store.workspace_file_paths().expect("workspace paths");
    assert_eq!(
        paths,
        vec!["include/z.h".to_string(), "src/main.c".to_string()]
    );

    store
        .replace_include_edges(
            &[file_id(&store, "src/main.c")],
            &[(
                file_id(&store, "src/main.c"),
                file_id(&store, "include/z.h"),
                "workspace_exact".to_string(),
            )],
            &[],
            &[],
            true,
        )
        .expect("edge");
    assert_eq!(
        store.load_include_edge_paths().expect("edges"),
        vec![("src/main.c".to_string(), "include/z.h".to_string())],
        "include table input is ordered as (source path, destination path)"
    );
}

#[test]
fn record_member_queries_preserve_alias_recursion_dedup_prefix_ordering_and_caps() {
    use crate::parser::MemberKind;

    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    upsert_source(
        &mut store,
        "base.hpp",
        "struct Base { int width; void wide(); static int widget_count(); int worm; };\n\
         typedef Base BaseAlias;\n\
         typedef BaseAlias PublicBase;\n",
    );

    let reader = IndexStore::open_readonly(&db).expect("readonly");
    let records = reader
        .resolve_record_candidates(&["Base", "BaseAlias", "PublicBase"], None)
        .expect("records");
    assert_eq!(
        records.len(),
        1,
        "direct record plus recursive aliases dedupe to one record id"
    );

    let prefixed = reader
        .members_for_records(&[records[0].id], Some("wi"), None)
        .expect("prefixed members");
    let names_and_kinds: Vec<_> = prefixed
        .iter()
        .map(|member| (member.name.as_str(), member.kind))
        .collect();
    assert_eq!(
        names_and_kinds,
        vec![
            ("width", MemberKind::Field),
            ("wide", MemberKind::Method),
            ("widget_count", MemberKind::StaticMethod),
        ],
        "same-tier members sort by field/method/static-method rank after prefix filtering"
    );
    assert!(prefixed.iter().all(|member| member.name.starts_with("wi")));

    let fallback = reader
        .fallback_member_candidates("wi", 2, None)
        .expect("fallback");
    assert_eq!(fallback.len(), 2);
    assert!(fallback.iter().all(|member| member.name.starts_with("wi")));
    assert!(
        fallback.iter().all(|member| member.name != "worm"),
        "fallback remains prefix-only, not subsequence/fuzzy"
    );
}
