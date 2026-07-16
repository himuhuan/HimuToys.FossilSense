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
fn name_table_read_view_exposes_typed_symbol_rows_and_legacy_wrapper_parity() {
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
    let rows = reader.name_table_view().symbol_rows().expect("rows");
    let external = rows
        .iter()
        .find(|row| row.label == "ext_size_t")
        .expect("external row");
    assert_eq!(external.symbol_id, external.id);
    assert!(external.external);
    assert_eq!(external.path, "C:/sdk/include/ext_size.h");
    assert_eq!(external.kind, "type");
    assert!(external.directly_included);

    let path_rows = reader
        .name_table_view()
        .symbol_rows_for_paths(&["src/main.c".to_string()])
        .expect("path rows");
    assert_eq!(path_rows.len(), 1);
    assert_eq!(path_rows[0].label, "main_entry");

    let mut visited = Vec::new();
    let visited_count = reader
        .name_table_view()
        .visit_symbol_rows(|row| {
            visited.push((
                row.symbol_id,
                row.label.to_string(),
                row.external,
                row.path.to_string(),
                row.kind.to_string(),
                row.directly_included,
            ));
            Ok(())
        })
        .expect("visit rows");
    assert_eq!(visited_count, rows.len());

    let view_tuples: Vec<_> = rows
        .into_iter()
        .map(crate::store::views::NameTableSymbolRow::into_legacy_tuple)
        .collect();
    assert_eq!(
        view_tuples,
        reader
            .load_symbol_names_with_paths()
            .expect("compat wrapper"),
        "compatibility wrapper must preserve the old tuple shape and ordering"
    );
    assert_eq!(visited, view_tuples);
}

#[test]
fn include_read_views_expose_typed_reach_and_completion_rows() {
    use crate::reachability::OpenReason;

    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    for path in ["src/a.c", "include/b.h", "src/c.c", "include/d.h"] {
        upsert_source(&mut store, path, "int marker;\n");
    }

    store
        .replace_include_edges(
            &[file_id(&store, "src/a.c"), file_id(&store, "src/c.c")],
            &[
                (
                    file_id(&store, "src/a.c"),
                    file_id(&store, "include/b.h"),
                    "workspace_exact".to_string(),
                ),
                (
                    file_id(&store, "src/c.c"),
                    file_id(&store, "include/d.h"),
                    "relative_exact".to_string(),
                ),
            ],
            &[(file_id(&store, "src/a.c"), 1)],
            &[(file_id(&store, "src/c.c"), 1)],
            true,
        )
        .expect("seed edges");

    let reach = store.reach_graph_view();
    assert_eq!(
        reach.include_edges().expect("edges"),
        vec![
            crate::store::views::IncludeEdgeRow {
                source_path: "src/a.c".to_string(),
                target_path: "include/b.h".to_string(),
                resolution: crate::includes::ResolutionKind::WorkspaceExact,
            },
            crate::store::views::IncludeEdgeRow {
                source_path: "src/c.c".to_string(),
                target_path: "include/d.h".to_string(),
                resolution: crate::includes::ResolutionKind::RelativeExact,
            },
        ]
    );
    assert_eq!(
        reach.unresolved_includes().expect("unresolved"),
        vec![crate::store::views::OpenIncludeRow {
            source_path: "src/a.c".to_string(),
            reason: OpenReason::UnresolvedInclude,
        }]
    );
    assert_eq!(
        reach.ambiguous_includes().expect("ambiguous"),
        vec![crate::store::views::OpenIncludeRow {
            source_path: "src/c.c".to_string(),
            reason: OpenReason::AmbiguousInclude,
        }]
    );

    let include_table = store.include_table_view();
    assert_eq!(
        include_table.workspace_paths().expect("paths"),
        vec![
            crate::store::views::IncludeCompletionPathRow {
                path: "include/b.h".to_string(),
            },
            crate::store::views::IncludeCompletionPathRow {
                path: "include/d.h".to_string(),
            },
            crate::store::views::IncludeCompletionPathRow {
                path: "src/a.c".to_string(),
            },
            crate::store::views::IncludeCompletionPathRow {
                path: "src/c.c".to_string(),
            },
        ]
    );
}

#[test]
fn symbol_reference_and_member_read_views_preserve_existing_domain_shapes() {
    use crate::parser::MemberKind;

    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    upsert_source(
        &mut store,
        "base.hpp",
        "struct Base { int width; void wide(); static int widget_count(); int worm; };\n\
         typedef Base BaseAlias;\n\
         int use_base(Base *b) { return b->width; }\n",
    );

    let reader = IndexStore::open_readonly(&db).expect("readonly");
    let symbols = reader
        .symbol_read_view()
        .symbols_by_name("use_base")
        .expect("symbol");
    assert_eq!(symbols, reader.symbols_by_name("use_base").expect("compat"));
    let ids: Vec<i64> = symbols.iter().map(|symbol| symbol.id).collect();
    assert_eq!(
        reader.symbol_read_view().symbols_by_ids(&ids).expect("ids"),
        reader.symbols_by_ids(&ids).expect("compat ids")
    );

    assert_eq!(
        reader
            .reference_file_view()
            .indexed_workspace_files()
            .expect("files"),
        vec![crate::store::views::ReferenceFileRow {
            path: "base.hpp".to_string(),
        }]
    );

    let member_view = reader.member_view();
    let records = member_view
        .resolve_record_candidates(&["Base", "BaseAlias"], None)
        .expect("records");
    assert_eq!(
        records,
        reader
            .resolve_record_candidates(&["Base", "BaseAlias"], None)
            .expect("compat records")
    );
    let members = member_view
        .members_for_records(&[records[0].id], Some("wi"), None)
        .expect("members");
    assert_eq!(
        members
            .iter()
            .map(|m| (&m.name, m.kind))
            .collect::<Vec<_>>(),
        vec![
            (&"width".to_string(), MemberKind::Field),
            (&"wide".to_string(), MemberKind::Method),
            (&"widget_count".to_string(), MemberKind::StaticMethod),
        ]
    );
    assert_eq!(
        members,
        reader
            .members_for_records(&[records[0].id], Some("wi"), None)
            .expect("compat members")
    );
    assert_eq!(
        member_view
            .fallback_member_candidates("wi", 2, None)
            .expect("fallback"),
        reader
            .fallback_member_candidates("wi", 2, None)
            .expect("compat fallback")
    );
}

#[test]
fn bounded_exact_name_symbol_read_can_reserve_a_reachable_path() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    let noise = "extern int crowded_value;\n".repeat(300);
    upsert_source(&mut store, "aaa/noise.h", &noise);
    upsert_source(&mut store, "zzz/reachable.h", "int crowded_value = 1;\n");

    let (global, global_truncated) = store
        .symbol_read_view()
        .symbols_by_name_limited("crowded_value", 256)
        .expect("global exact-name rows");
    assert!(global_truncated);
    assert!(global.iter().all(|row| row.path == "aaa/noise.h"));

    let (reachable, reachable_truncated) = store
        .symbol_read_view()
        .symbols_by_name_in_paths_limited("crowded_value", &["zzz/reachable.h".into()], 1)
        .expect("reachable exact-name rows");
    assert!(!reachable_truncated);
    assert_eq!(reachable.len(), 1);
    assert_eq!(reachable[0].path, "zzz/reachable.h");
}
