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
fn declaration_view_round_trips_canonical_identity_and_backing() {
    use crate::semantic_model::{DeclarationBacking, SemanticDeclarationKind};

    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    upsert_source(
        &mut store,
        "model.cpp",
        "#define LIMIT 4\n\
         struct Widget { void run(); int value; };\n\
         typedef Widget WidgetAlias;\n\
         enum Mode { Fast };\n\
         int global_value;\n\
         void free_fn(void) {}\n",
    );

    let reader = IndexStore::open_readonly(&db).expect("readonly");
    for (name, kind) in [
        ("LIMIT", SemanticDeclarationKind::Macro),
        ("Widget", SemanticDeclarationKind::Type),
        ("WidgetAlias", SemanticDeclarationKind::Alias),
        ("Fast", SemanticDeclarationKind::EnumConstant),
        ("global_value", SemanticDeclarationKind::Object),
        ("free_fn", SemanticDeclarationKind::Function),
        ("run", SemanticDeclarationKind::Method),
    ] {
        let (rows, truncated) = reader
            .declaration_view()
            .by_name_limited(name, 8)
            .expect("declaration rows");
        assert!(!truncated);
        let row = rows
            .into_iter()
            .find(|row| row.fact.declaration_kind == kind)
            .unwrap_or_else(|| panic!("missing canonical {kind:?} declaration for {name}"));
        assert!(!row.fact.identity.locator.fingerprint.is_empty());
        assert_eq!(row.fact.identity.logical_key.declaration_kind, kind);
        assert!(
            !matches!(row.fact.backing, DeclarationBacking::None),
            "{name} must retain an explicit specialized backing"
        );
        match row.fact.backing {
            DeclarationBacking::CallableAnchor { .. }
            | DeclarationBacking::Record { .. }
            | DeclarationBacking::TypeAlias { .. } => {
                assert!(row.backing_id.is_some(), "{name} backing id must resolve")
            }
            DeclarationBacking::SourceRange { .. } => {
                assert!(
                    row.backing_id.is_none(),
                    "source ranges have no database foreign key"
                )
            }
            DeclarationBacking::None => unreachable!(),
        }
        let (same_entity, limited) = reader
            .declaration_view()
            .by_logical_key_limited(&row.fact.identity.logical_key, 8)
            .expect("logical entity rows");
        assert!(!limited);
        assert!(same_entity.iter().any(|candidate| candidate.id == row.id));
    }
}

#[test]
fn declaration_name_view_exposes_typed_rows_and_streaming_parity() {
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
    let rows = reader.declaration_view().all_name_rows().expect("rows");
    let external = rows
        .iter()
        .find(|row| row.name == "ext_size_t")
        .expect("external row");
    assert!(external.external);
    assert_eq!(external.path, "C:/sdk/include/ext_size.h");
    assert_eq!(
        external.declaration_kind,
        crate::semantic_model::SemanticDeclarationKind::Alias
    );
    assert!(external.directly_included);

    let path_rows = reader
        .declaration_view()
        .name_rows_for_paths(&["src/main.c".to_string()])
        .expect("path rows");
    assert_eq!(path_rows.len(), 1);
    assert_eq!(path_rows[0].name, "main_entry");

    let mut visited = Vec::new();
    let visited_count = reader
        .declaration_view()
        .visit_name_rows(|row| {
            visited.push((
                row.id,
                row.name.to_string(),
                row.external,
                row.path.to_string(),
                row.declaration_kind,
                row.directly_included,
            ));
            Ok(())
        })
        .expect("visit rows");
    assert_eq!(visited_count, rows.len());

    let owned: Vec<_> = rows
        .into_iter()
        .map(|row| {
            (
                row.id,
                row.name,
                row.external,
                row.path,
                row.declaration_kind,
                row.directly_included,
            )
        })
        .collect();
    assert_eq!(visited, owned);
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
fn declaration_reference_and_member_read_views_preserve_domain_shapes() {
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
    let declarations = reader
        .declaration_view()
        .by_name("use_base")
        .expect("declaration");
    let ids: Vec<i64> = declarations
        .iter()
        .map(|declaration| declaration.id)
        .collect();
    assert_eq!(
        reader.declaration_view().by_ids(&ids).expect("ids"),
        declarations
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
fn bounded_exact_name_declaration_read_can_reserve_a_reachable_path() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    let noise = "extern int crowded_value;\n".repeat(300);
    upsert_source(&mut store, "aaa/noise.h", &noise);
    upsert_source(&mut store, "zzz/reachable.h", "int crowded_value = 1;\n");

    let (global, global_truncated) = store
        .declaration_view()
        .by_name_limited("crowded_value", 256)
        .expect("global exact-name rows");
    assert!(global_truncated);
    assert!(global.iter().all(|row| row.fact.path == "aaa/noise.h"));

    let (reachable, reachable_truncated) = store
        .declaration_view()
        .by_name_in_paths_limited("crowded_value", &["zzz/reachable.h".into()], 1)
        .expect("reachable exact-name rows");
    assert!(!reachable_truncated);
    assert_eq!(reachable.len(), 1);
    assert_eq!(reachable[0].fact.path, "zzz/reachable.h");
}
