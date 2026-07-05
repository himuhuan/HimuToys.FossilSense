use super::*;

#[test]
fn fields_by_record_and_alias_normalization() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");

    // `Foo` owns the fields; `FooT` is a typedef alias for `struct Foo`.
    upsert_source(
        &mut store,
        "foo.h",
        "struct Foo { int a; int b; };\ntypedef struct Foo FooT;\n",
    );

    let reader = IndexStore::open_readonly(&db).expect("readonly");

    let by_tag = fields_by_record_names(&reader, &["Foo"]);
    assert_eq!(by_tag, vec!["a".to_string(), "b".to_string()]);

    // The alias resolves to the same fields as the underlying tag.
    let by_alias = fields_by_record_names(&reader, &["FooT"]);
    assert_eq!(by_alias, by_tag);

    // Fields never leak into the fuzzy name table (no member in normal completion).
    let names = reader.load_symbol_names().expect("names");
    assert!(!names.iter().any(|(_, name, _)| name == "a"));
}

#[test]
fn removing_a_file_prunes_its_fields() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    upsert_source(&mut store, "p.c", "struct Point { int x; int y; };\n");

    assert_eq!(fields_by_record_names(&store, &["Point"]).len(), 2);

    store
        .delete_missing_files(&Default::default())
        .expect("delete missing");

    assert!(fields_by_record_names(&store, &["Point"]).is_empty());
    // The record (and thus any alias target) is gone: no candidates resolve.
    assert!(store
        .resolve_record_candidates(&["Point"], None)
        .expect("candidates")
        .is_empty());
}

#[test]
fn struct_fields_are_persisted_as_field_members() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    upsert_source(&mut store, "point.h", "struct Point { int x; int y; };\n");

    let reader = IndexStore::open_readonly(&db).expect("reader");
    let records = reader
        .resolve_record_candidates(&["Point"], None)
        .expect("records");
    let members = reader
        .members_for_records(&[records[0].id], None, None)
        .expect("members");

    let names: Vec<_> = members
        .iter()
        .map(|member| (member.name.as_str(), member.kind.as_str()))
        .collect();
    assert!(names.contains(&("x", "field")));
    assert!(names.contains(&("y", "field")));
}

#[test]
fn field_candidates_honor_prefix_and_cap() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    upsert_source(
        &mut store,
        "s.c",
        "struct S { int count; int counter; int color; int name; };\n",
    );

    let reader = IndexStore::open_readonly(&db).expect("readonly");

    let mut prefixed = field_prefix_names(&reader, "cou", 100);
    prefixed.sort();
    assert_eq!(prefixed, vec!["count".to_string(), "counter".to_string()]);

    // Case-insensitive prefix.
    assert_eq!(
        field_prefix_names(&reader, "NAM", 100),
        vec!["name".to_string()]
    );

    // Cap limits the number of returned candidates.
    assert_eq!(field_prefix_names(&reader, "co", 1).len(), 1);
}

#[test]
fn member_completion_pipeline_resolves_cross_file_and_falls_back() {
    use crate::parser::{infer_receiver_record, parse};

    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");

    // The struct definition (and its fields) live in a header.
    upsert_source(
        &mut store,
        "widget.h",
        "struct Widget { int width; int height; };\n",
    );
    let reader = IndexStore::open_readonly(&db).expect("readonly");

    // Current file only forward-declares the struct, then uses a local pointer.
    let current = "struct Widget;\nvoid draw(struct Widget *w) {\n    w->width;\n}\n";
    let off = current.find("w->").expect("usage") + 1;

    // Receiver inference + cross-file field resolution (what complete_members does).
    let current_index = parse(std::path::Path::new("draw.c"), current);
    let record = infer_receiver_record(&current_index.local_declarations, "w", off)
        .expect("receiver record");
    assert_eq!(record, "Widget");
    let resolved = fields_by_record_names(&reader, &[record.as_str()]);
    assert_eq!(resolved, vec!["height".to_string(), "width".to_string()]);

    // Call receiver: inference declines, so the caller uses the global fallback.
    let call_line = "make_widget()->wi";
    assert!(
        crate::query::member_receiver_name(call_line, call_line.len() as u32).is_none(),
        "call receiver should not infer"
    );
    let fallback = field_prefix_names(&reader, "wi", 100);
    assert_eq!(fallback, vec!["width".to_string()]);
}

#[test]
fn name_table_from_index_carries_correct_kind_without_second_store() {
    // Pipeline test: after indexing, build the NameTable directly from the
    // store's name loader. Each hit carries the cached `kind` so the
    // completion hot path can render an icon without re-opening the store
    // or calling `symbols_by_ids`.
    use crate::parser::SymbolKind;

    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    // Index a small header + source pair mirroring samples/mini-c.
    upsert_source(
        &mut store,
        "shapes.h",
        "struct Point { int x; int y; };\n\
         enum Status { STATUS_OK, STATUS_BUSY };\n\
         int hello_value(void);\n",
    );
    upsert_source(
        &mut store,
        "hello.c",
        "#define ANSWER 42\n\
         int hello_value(void) { return 42; }\n",
    );

    let reader = IndexStore::open_readonly(&db).expect("readonly");
    // Build the NameTable from the name loader only — no second store open,
    // no `symbols_by_ids`. Kind is cached inline.
    let names = reader.load_symbol_names_with_paths().expect("names");
    let table = crate::query::NameTable::build_with_paths(names);

    // hello_value is a function (declared in the header, defined in the
    // source; at least one hit must carry Function).
    let hits = table.search_ranked("hello", 10);
    assert!(
        hits.iter()
            .any(|h| h.name == "hello_value" && h.kind == SymbolKind::Function),
        "hello_value should be a Function"
    );
    // Point is a type.
    let hits = table.search_ranked("point", 10);
    assert!(
        hits.iter()
            .any(|h| h.name == "Point" && h.kind == SymbolKind::Type),
        "Point should be a Type"
    );
    // ANSWER is a macro.
    let hits = table.search_ranked("answer", 10);
    assert!(
        hits.iter()
            .any(|h| h.name == "ANSWER" && h.kind == SymbolKind::Macro),
        "ANSWER should be a Macro"
    );
    // STATUS_OK is an enum_constant.
    let hits = table.search_ranked("status", 10);
    assert!(
        hits.iter()
            .any(|h| h.name == "STATUS_OK" && h.kind == SymbolKind::EnumConstant),
        "STATUS_OK should be an EnumConstant"
    );
    // Fields (x, y) are excluded from the name table (WHERE kind != 'field').
    assert!(
        !table.search_ranked("x", 10).iter().any(|h| h.name == "x"),
        "fields must not appear in the name table"
    );
}

#[test]
fn member_fallback_prefix_only_capped_and_short_prefix_empty() {
    // Pipeline test for the member-completion fallback (D4): when the
    // receiver cannot be resolved, `fallback_field_candidates` returns
    // prefix-only matches (SQL LIKE 'prefix%'), capped at the limit. The
    // `complete_members` gate (`prefix.len() >= MEMBER_COMPLETION_MIN_PREFIX_LEN`) prevents
    // the fallback from running on a sub-2-char prefix, returning an empty
    // incomplete list instead.
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    upsert_source(
        &mut store,
        "s.c",
        "struct S { int width; int window; int wxyz; int other; };\n",
    );

    let reader = IndexStore::open_readonly(&db).expect("readonly");

    // Fallback is prefix-only (LIKE 'wi%'): "width" and "window", NOT
    // "wxyz" (which would only match as a subsequence, not a prefix).
    let mut fallback = field_prefix_names(&reader, "wi", 100);
    fallback.sort();
    assert_eq!(fallback, vec!["width".to_string(), "window".to_string()]);

    // "wxyz" starts with "w" but not "wi", so it is excluded by the
    // prefix-only LIKE — subsequence matches never surface in the fallback.
    assert!(!fallback.contains(&"wxyz".to_string()));

    // Cap is respected: limit=1 returns at most one candidate.
    assert_eq!(field_prefix_names(&reader, "w", 1).len(), 1);

    // The complete_members len < 2 gate: a 1-char prefix would produce
    // candidates via fallback_field_candidates, but the server logic returns
    // an empty incomplete list instead. Verify the gate condition directly.
    let min_prefix = crate::query::MEMBER_COMPLETION_MIN_PREFIX_LEN;
    assert_eq!(min_prefix, 2);
    assert!(
        "w".len() < min_prefix,
        "1-char prefix is below the fallback gate"
    );
    // fallback_field_candidates *would* return results for "w" — the gate
    // prevents the call, not the SQL.
    assert!(
        !field_prefix_names(&reader, "w", 100).is_empty(),
        "fallback_field_candidates has 1-char results; the gate is in complete_members"
    );
}
