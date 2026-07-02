use super::*;

#[test]
fn records_source_and_absolute_path_for_external_files() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");

    upsert_with_source(
        &mut store,
        "src/main.c",
        "int main(void){return 0;}\n",
        FileSource::Workspace,
    );
    upsert_with_source(
        &mut store,
        "C:/mingw/include/stddef.h",
        "typedef unsigned long size_t;\n",
        FileSource::External,
    );

    let reader = IndexStore::open_readonly(&db).expect("readonly");
    let ext = reader.symbols_by_name("size_t").expect("size_t");
    assert_eq!(ext.len(), 1);
    assert_eq!(ext[0].source, "external");
    assert_eq!(ext[0].path, "C:/mingw/include/stddef.h");

    let ws = reader.symbols_by_name("main").expect("main");
    assert_eq!(ws[0].source, "workspace");
    assert_eq!(ws[0].path, "src/main.c");

    let indexed_files = reader
        .indexed_workspace_files()
        .expect("indexed workspace files");
    assert_eq!(indexed_files, vec!["src/main.c".to_string()]);
}

#[test]
fn external_symbols_color_only_when_first_layer() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");

    let ext_path = "C:/mingw/include/stddef.h";
    upsert_with_source(
        &mut store,
        ext_path,
        "typedef unsigned long size_t;\n",
        FileSource::External,
    );

    // Transitively-only external symbol: excluded from coloring counts.
    let before = store.kind_counts_by_names(&["size_t"]).expect("before");
    assert!(!before.contains_key("size_t"));

    // Promote to first layer; now it contributes a `type` count.
    store
        .mark_directly_included(&[ext_path.to_string()])
        .expect("mark");
    let after = store.kind_counts_by_names(&["size_t"]).expect("after");
    assert_eq!(
        after.get("size_t").and_then(|m| m.get("type")).copied(),
        Some(1)
    );

    // Clearing (re-derivation with no match) demotes it again.
    store.mark_directly_included(&[]).expect("clear");
    let cleared = store.kind_counts_by_names(&["size_t"]).expect("cleared");
    assert!(!cleared.contains_key("size_t"));
}

#[test]
fn workspace_files_by_suffix_matches_exact_and_nested() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    upsert_with_source(
        &mut store,
        "include/sys/types.h",
        "typedef int x;\n",
        FileSource::Workspace,
    );
    upsert_with_source(
        &mut store,
        "types.h",
        "typedef int y;\n",
        FileSource::Workspace,
    );

    let by_nested = store
        .workspace_files_by_suffix("sys/types.h")
        .expect("nested");
    assert_eq!(by_nested, vec!["include/sys/types.h".to_string()]);

    let by_exact = store.workspace_files_by_suffix("types.h").expect("exact");
    // Matches both the exact top-level file and the nested suffix.
    assert!(by_exact.contains(&"types.h".to_string()));
    assert!(by_exact.contains(&"include/sys/types.h".to_string()));
}

#[test]
fn directly_included_derivation_only_flags_external_exact_dsts() {
    // The first-layer `directly_included` flag is derived from
    // `include_edges` rows whose `resolution = 'external_exact'` and whose
    // src is workspace. A relative/workspace_exact edge to an external
    // twin MUST NOT flag it (consistent with form/priority by construction),
    // and a non-external edge never flags its dst.
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    upsert_with_source(&mut store, "src/a.c", "int a;\n", FileSource::Workspace);
    upsert_with_source(&mut store, "src/b.c", "int b;\n", FileSource::Workspace);
    upsert_with_source(
        &mut store,
        "C:/mingw/include/stddef.h",
        "typedef unsigned long size_t;\n",
        FileSource::External,
    );
    let files = store.files_with_ids().expect("files");
    let id = |p: &str| files.iter().find(|(_, path, _)| path == p).unwrap().0;

    // src/a.c has an ExternalExact edge to stddef.h  → flag stddef.h.
    // src/b.c only has a SuffixMatch-style edge to stddef.h → must NOT
    //   flag it (form/priority say it was not a direct external include).
    // src/a.c has a WorkspaceExact edge to src/b.c → external-flag-irrelevant.
    store
        .replace_include_edges(
            &[id("src/a.c"), id("src/b.c")],
            &[
                (
                    id("src/a.c"),
                    id("C:/mingw/include/stddef.h"),
                    "external_exact".to_string(),
                ),
                (
                    id("src/b.c"),
                    id("C:/mingw/include/stddef.h"),
                    "suffix_match".to_string(),
                ),
                (id("src/a.c"), id("src/b.c"), "workspace_exact".to_string()),
            ],
            &[],
            &[],
            true,
        )
        .expect("replace");
    store.apply_directly_included_derivation().expect("derive");

    let reader = IndexStore::open_readonly(&db).expect("readonly");
    let defs = reader.symbols_by_name("size_t").expect("size_t");
    assert_eq!(defs.len(), 1);
    assert!(
        defs[0].directly_included,
        "ExternalExact edge → external stddef.h is first-layer"
    );

    // Counts: with the derivation, the kind_counts coloring loop includes
    // directly_included externals — size_t now colors.
    let counts = reader.kind_counts_by_names(&["size_t"]).expect("counts");
    assert_eq!(counts["size_t"].get("type").copied(), Some(1));

    // Removing the ExternalExact edge and re-deriving clears the flag.
    store
        .replace_include_edges(
            &[id("src/a.c"), id("src/b.c")],
            &[(
                id("src/b.c"),
                id("C:/mingw/include/stddef.h"),
                "suffix_match".to_string(),
            )],
            &[],
            &[],
            true,
        )
        .expect("clear replace");
    store
        .apply_directly_included_derivation()
        .expect("re-derive");
    let reader = IndexStore::open_readonly(&db).expect("readonly2");
    let cleared = reader.kind_counts_by_names(&["size_t"]).expect("cleared");
    assert!(
        !cleared.contains_key("size_t"),
        "without an ExternalExact edge, the external twin is no longer first-layer"
    );
}
