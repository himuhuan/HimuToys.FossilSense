use super::*;
use tower_lsp::lsp_types::{Location, Position, Range};

async fn assert_entity_definition(
    files: &[(&str, &str)],
    source: &str,
    expected: &[(&str, u32, u32)],
) {
    let (dir, service, uri, line, col) =
        indexed_backend_with_open_doc(files, "caller.c", source).await;
    let result = service
        .inner()
        .goto_definition(goto_definition_params(uri, line, col))
        .await
        .unwrap()
        .expect("entity has an indexed location");
    let mut actual: Vec<_> = definition_locations(result)
        .into_iter()
        .map(|location| (location.uri, location.range.start, location.range.end))
        .collect();
    actual.sort();
    let mut expected: Vec<_> = expected
        .iter()
        .map(|(path, line, col)| {
            (
                Url::from_file_path(dir.path().join(path)).unwrap(),
                Position::new(*line, *col),
                Position::new(*line, *col + 8),
            )
        })
        .collect();
    expected.sort();
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn entity_location_guarded_header_reaches_implementation_outside_caller_scope() {
    assert_entity_definition(
        &[
            (
                "api.h",
                "#ifndef API_H\n#define API_H\nint run_task(void);\n#endif\n",
            ),
            (
                "impl.c",
                "#include \"api.h\"\nint run_task(void) { return 1; }\n",
            ),
        ],
        "#include \"api.h\"\nint use(void) { return run_task/*cursor*/(); }\n",
        &[("impl.c", 1, 4)],
    )
    .await;
}

#[tokio::test]
async fn entity_location_multiple_declarations_retain_one_implementation() {
    assert_entity_definition(
        &[
            ("api.h", "int run_task(void);\n"),
            ("extra.h", "int run_task(void);\n"),
            (
                "impl.c",
                "#include \"api.h\"\n#include \"extra.h\"\nint run_task(void) { return 1; }\n",
            ),
        ],
        "#include \"api.h\"\nint use(void) { return run_task/*cursor*/(); }\n",
        &[("impl.c", 2, 4)],
    )
    .await;
}

#[tokio::test]
async fn entity_location_parameter_names_do_not_separate_supported_function_identity() {
    assert_entity_definition(
        &[
            ("api.h", "int run_task(int input);\n"),
            (
                "impl.c",
                "#include \"api.h\"\nint run_task(int value) { return value; }\n",
            ),
        ],
        "#include \"api.h\"\nint use(void) { return run_task/*cursor*/(1); }\n",
        &[("impl.c", 1, 4)],
    )
    .await;
}

#[tokio::test]
async fn entity_location_two_compatible_implementations_remain_ambiguous() {
    assert_entity_definition(
        &[
            ("api.h", "int run_task(void);\n"),
            (
                "left.c",
                "#include \"api.h\"\nint run_task(void) { return 1; }\n",
            ),
            (
                "right.c",
                "#include \"api.h\"\nint run_task(void) { return 2; }\n",
            ),
            ("private.c", "static int run_task(void) { return 3; }\n"),
        ],
        "#include \"api.h\"\nint use(void) { return run_task/*cursor*/(); }\n",
        &[("left.c", 1, 4), ("right.c", 1, 4)],
    )
    .await;
}

#[tokio::test]
async fn entity_location_definition_in_header_is_selected_by_role() {
    assert_entity_definition(
        &[("api.h", "int run_task(void) { return 1; }\n")],
        "#include \"api.h\"\nint use(void) { return run_task/*cursor*/(); }\n",
        &[("api.h", 0, 4)],
    )
    .await;
}

#[tokio::test]
async fn entity_location_simple_method_uses_proven_owner_definition() {
    let (dir, service, uri, line, col) = indexed_backend_with_open_doc(
        &[
            ("device.h", "struct Device { int run_task(void); };\n"),
            (
                "device.cpp",
                "#include \"device.h\"\nint Device::run_task(void) { return 1; }\n",
            ),
            (
                "other.cpp",
                "struct Other { int run_task(void) { return 2; } };\n",
            ),
        ],
        "caller.cpp",
        "#include \"device.h\"\nint use(Device *p) { return p->run_task/*cursor*/(); }\n",
    )
    .await;
    let locations = definition_locations(
        service
            .inner()
            .goto_definition(goto_definition_params(uri, line, col))
            .await
            .unwrap()
            .unwrap(),
    );
    assert_eq!(locations.len(), 1);
    assert_eq!(
        locations[0].uri,
        Url::from_file_path(dir.path().join("device.cpp")).unwrap()
    );
    assert_eq!(
        locations[0].range,
        Range::new(Position::new(1, 12), Position::new(1, 20))
    );
}

#[tokio::test]
async fn entity_location_tag_forward_reaches_complete_definition_and_alias_stays_itself() {
    let (dir, service, uri, line, col) = indexed_backend_with_open_doc(
        &[
            ("api.h", "struct Device;\n"),
            ("types.h", "struct Device { int state; };\n"),
        ],
        "caller.c",
        "#include \"api.h\"\n#include \"types.h\"\nstruct Device/*cursor*/ *p;\n",
    )
    .await;
    let locations = definition_locations(
        service
            .inner()
            .goto_definition(goto_definition_params(uri, line, col))
            .await
            .unwrap()
            .unwrap(),
    );
    assert_eq!(locations.len(), 1);
    assert_eq!(
        locations[0].uri,
        Url::from_file_path(dir.path().join("types.h")).unwrap()
    );
    assert_eq!(
        locations[0].range,
        Range::new(Position::new(0, 7), Position::new(0, 13))
    );

    let (dir, service, uri, line, col) = indexed_backend_with_open_doc(
        &[(
            "types.h",
            "struct Device { int state; };\ntypedef struct Device Handle;\n",
        )],
        "caller.c",
        "#include \"types.h\"\nHandle/*cursor*/ *p;\n",
    )
    .await;
    let locations = definition_locations(
        service
            .inner()
            .goto_definition(goto_definition_params(uri, line, col))
            .await
            .unwrap()
            .unwrap(),
    );
    assert_eq!(locations.len(), 1);
    assert_eq!(
        locations[0].uri,
        Url::from_file_path(dir.path().join("types.h")).unwrap()
    );
    assert_eq!(
        locations[0].range,
        Range::new(Position::new(1, 22), Position::new(1, 28))
    );
}

#[tokio::test]
async fn entity_location_dirty_deleted_definition_is_not_revived() {
    let (dir, service, uri, line, col) = indexed_backend_with_open_doc(
        &[
            ("api.h", "int run_task(void);\n"),
            (
                "impl.c",
                "#include \"api.h\"\nint run_task(void) { return 1; }\n",
            ),
        ],
        "caller.c",
        "#include \"api.h\"\nint use(void) { return run_task/*cursor*/(); }\n",
    )
    .await;
    let impl_uri = Url::from_file_path(dir.path().join("impl.c")).unwrap();
    open_test_document(&service, impl_uri.clone(), 1, "#include \"api.h\"\n".into()).await;
    let result = service
        .inner()
        .goto_definition(goto_definition_params(uri, line, col))
        .await
        .unwrap();
    if let Some(result) = result {
        assert!(definition_locations(result)
            .iter()
            .all(|location| location.uri != impl_uri));
    }
}

#[tokio::test]
async fn entity_location_known_declaration_escapes_unrelated_name_bucket() {
    let mut owned = vec![(
        "z_impl.c".to_owned(),
        "#include \"api.h\"\nint run_task(void) { return 1; }\n".to_owned(),
    )];
    for i in 0..300 {
        owned.push((
            format!("a_noise_{i:03}.c"),
            "int run_task(int x, int y) { return x + y; }\n".into(),
        ));
    }
    let files: Vec<_> = owned
        .iter()
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect();
    let (dir, service, uri, line, col) =
        indexed_backend_with_open_doc(&files, "api.h", "int run_task/*cursor*/(void);\n").await;
    let locations = definition_locations(
        service
            .inner()
            .goto_definition(goto_definition_params(uri, line, col))
            .await
            .unwrap()
            .unwrap(),
    );
    assert_eq!(locations.len(), 1);
    assert_eq!(
        locations[0].uri,
        Url::from_file_path(dir.path().join("z_impl.c")).unwrap()
    );
    assert_eq!(
        locations[0].range,
        Range::new(Position::new(1, 4), Position::new(1, 12))
    );
}

#[tokio::test]
async fn entity_location_declaration_and_hover_keep_interface_identity() {
    let (dir, service, uri, line, col) = indexed_backend_with_open_doc(&[
        ("api.h", "#ifndef API_H\n#define API_H\n/** Public interface documentation. */\nint run_task(void);\n#endif\n"),
        ("impl.c", "#include \"api.h\"\nint run_task(void) { return 1; }\n"),
    ], "caller.c", "#include \"api.h\"\nint use(void) { return run_task/*cursor*/(); }\n").await;
    let definitions = definition_locations(
        service
            .inner()
            .goto_definition(goto_definition_params(uri.clone(), line, col))
            .await
            .unwrap()
            .unwrap(),
    );
    assert_eq!(definitions.len(), 1);
    assert_eq!(
        definitions[0].uri,
        Url::from_file_path(dir.path().join("impl.c")).unwrap()
    );
    let locations = definition_locations(
        service
            .inner()
            .goto_declaration(goto_definition_params(uri.clone(), line, col))
            .await
            .unwrap()
            .unwrap(),
    );
    assert_eq!(locations.len(), 1);
    assert_eq!(
        locations[0].uri,
        Url::from_file_path(dir.path().join("api.h")).unwrap()
    );
    assert_eq!(
        locations[0].range,
        Range::new(Position::new(3, 4), Position::new(3, 12))
    );
    let text = hover_text(
        service
            .inner()
            .hover(hover_params(uri, line, col))
            .await
            .unwrap()
            .unwrap()
            .contents,
    );
    assert!(text.contains("Public interface documentation."), "{text}");
    assert!(text.contains("api.h"), "{text}");
}

#[tokio::test]
async fn entity_location_real_conditional_guards_exclude_opposite_implementation() {
    assert_entity_definition(
        &[
            ("api.h", "#if FLAG\nint run_task(void);\n#endif\n"),
            (
                "impl.c",
                "#include \"api.h\"\n#if !FLAG\nint run_task(void) { return 1; }\n#endif\n",
            ),
        ],
        "#include \"api.h\"\nint use(void) { return run_task/*cursor*/(); }\n",
        &[("api.h", 1, 4)],
    )
    .await;
}

#[tokio::test]
async fn entity_location_defined_directive_keeps_keyword_boundary() {
    for positive in [
        "#if defined(FEATURE)",
        "#ifdef FEATURE",
        "#if defined FEATURE",
    ] {
        let header = format!("{positive}\nint run_task(void);\n#endif\n");
        assert_entity_definition(
            &[
                ("api.h", header.as_str()),
                ("impl.c", "#include \"api.h\"\n#if !defined(FEATURE)\nint run_task(void) { return 1; }\n#endif\n"),
            ],
            "#include \"api.h\"\nint use(void) { return run_task/*cursor*/(); }\n",
            &[("api.h", 1, 4)],
        ).await;
    }
}

#[tokio::test]
async fn entity_location_distinct_alias_occurrences_are_not_erased() {
    let (dir, service, uri, line, col) = indexed_backend_with_open_doc(
        &[
            ("one.h", "typedef int Item;\n"),
            ("two.h", "typedef double Item;\n"),
        ],
        "caller.c",
        "#include \"one.h\"\n#include \"two.h\"\nItem/*cursor*/ item;\n",
    )
    .await;
    let locations = definition_locations(
        service
            .inner()
            .goto_definition(goto_definition_params(uri, line, col))
            .await
            .unwrap()
            .unwrap(),
    );
    let mut actual: Vec<_> = locations
        .into_iter()
        .map(|item| (item.uri, item.range.start, item.range.end))
        .collect();
    actual.sort();
    let mut expected = vec![
        (
            Url::from_file_path(dir.path().join("one.h")).unwrap(),
            Position::new(0, 12),
            Position::new(0, 16),
        ),
        (
            Url::from_file_path(dir.path().join("two.h")).unwrap(),
            Position::new(0, 15),
            Position::new(0, 19),
        ),
    ];
    expected.sort();
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn entity_location_declaration_excludes_later_same_file_prototype() {
    let (_, service, uri, line, col) = indexed_backend_with_open_doc(&[], "caller.c",
        "int run_task(void);\nint use(void) { return run_task/*cursor*/(); }\nint run_task(void);\n").await;
    let locations = definition_locations(
        service
            .inner()
            .goto_declaration(goto_definition_params(uri.clone(), line, col))
            .await
            .unwrap()
            .unwrap(),
    );
    assert_eq!(
        locations,
        vec![Location {
            uri,
            range: Range::new(Position::new(0, 4), Position::new(0, 12))
        }]
    );
}

#[tokio::test]
async fn entity_location_negated_compound_condition_keeps_compatible_implementation() {
    assert_entity_definition(
        &[
            (
                "api.h",
                "#if A && B && C\n#else\nint run_task(void);\n#endif\n",
            ),
            (
                "impl.c",
                "#include \"api.h\"\n#if !B\nint run_task(void) { return 1; }\n#endif\n",
            ),
        ],
        "#include \"api.h\"\nint use(void) { return run_task/*cursor*/(); }\n",
        &[("impl.c", 2, 4)],
    )
    .await;
}

#[tokio::test]
async fn entity_location_c_tag_kind_excludes_unrelated_union_definition() {
    let (dir, service, uri, line, col) = indexed_backend_with_open_doc(
        &[
            ("api.h", "struct Device;\n"),
            ("other.c", "union Device { int state; };\n"),
        ],
        "caller.c",
        "#include \"api.h\"\nstruct Device/*cursor*/ *p;\n",
    )
    .await;
    let locations = definition_locations(
        service
            .inner()
            .goto_definition(goto_definition_params(uri, line, col))
            .await
            .unwrap()
            .unwrap(),
    );
    assert_eq!(
        locations,
        vec![Location {
            uri: Url::from_file_path(dir.path().join("api.h")).unwrap(),
            range: Range::new(Position::new(0, 7), Position::new(0, 13))
        }]
    );
}

#[tokio::test]
async fn entity_location_completion_details_find_guarded_interface_comments() {
    let (_dir, service, uri, line, col) = indexed_backend_with_open_doc(&[
        ("api.h", "#ifndef API_H\n#define API_H\n/** Public interface documentation. */\nint run_task(void);\n#endif\n"),
    ], "impl.c", "#include \"api.h\"\nint run_task(void) { return 1; }\nint use(void) { return run_t/*cursor*/; }\n").await;
    let response = service
        .inner()
        .completion(completion_params(uri, line, col))
        .await
        .unwrap()
        .unwrap();
    let item = completion_items(response)
        .into_iter()
        .find(|item| item.label == "run_task")
        .expect("completion witness");
    let resolved = service.inner().completion_resolve(item).await.unwrap();
    let text = documentation_text(resolved.documentation.expect("completion details"));
    assert!(text.contains("Public interface documentation."), "{text}");
}

#[tokio::test]
async fn entity_location_completion_details_keep_known_occurrence_beyond_name_cap() {
    let source = format!(
        "{}\n/** Last occurrence documentation. */\nint run_task(int value);\n",
        "int run_task(void);\n".repeat(299)
    );
    let (dir, service, uri, _, _) = indexed_backend_with_open_doc(
        &[("api.h", &source)],
        "caller.c",
        "#include \"api.h\"\nint use(void) { return run_t/*cursor*/; }\n",
    )
    .await;
    let engine = service
        .inner()
        .session
        .cache
        .current_engine_snapshot(&dir.path().to_path_buf())
        .await
        .unwrap();
    let row = engine
        .call_read_handle
        .as_ref()
        .unwrap()
        .read(|store| Ok(store.declaration_view().by_name("run_task")?.pop().unwrap()))
        .unwrap();
    assert!(row
        .fact
        .canonical_signature
        .as_ref()
        .unwrap()
        .contains("int"));
    let documents = service
        .inner()
        .session
        .documents
        .capture_request_snapshot(Some(&uri))
        .await;
    let text = service
        .inner()
        .declaration_completion_documentation(
            6,
            dir.path().to_string_lossy().into_owned(),
            uri.to_string(),
            row.id,
            "run_task".into(),
            engine.semantic_generation.0,
            documents.overlay_epoch,
            1,
        )
        .await
        .expect("selected declaration documentation");
    assert!(text.contains("Last occurrence documentation."), "{text}");
}
