use super::*;

#[tokio::test]
async fn owner_member_navigation_and_hover_use_exact_field_identity() {
    for source in [
        "typedef int state;\nstruct S { int state; };\nint f(struct S *p) { return p->state/*cursor*/; }\n",
        "typedef int state;\nstruct S { int state; };\nint f(struct S s) { return s.state/*cursor*/; }\n",
        "typedef int state;\nstruct S { int state/*cursor*/; };\n",
        "typedef int state;\nstruct S { int state; };\nstruct S s = {.state/*cursor*/ = 1};\n",
        "typedef int state;\nstruct S { int state; };\nint f(struct S *p) { return (*p).state/*cursor*/; }\n",
        "typedef int state;\nstruct S { int state; };\nint f(struct S *p) { return (p)->state/*cursor*/; }\n",
    ] {
        let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(&[], "main.c", source).await;
        for response in [
            service
                .inner()
                .goto_definition(goto_definition_params(uri.clone(), line, character))
                .await
                .unwrap()
                .expect("owner-proven field definition"),
            service
                .inner()
                .goto_declaration(goto_definition_params(uri.clone(), line, character))
                .await
                .unwrap()
                .expect("owner-proven field declaration"),
        ] {
            let locations = definition_locations(response);
            assert_eq!(locations.len(), 1, "{source}: {locations:?}");
            assert_eq!(locations[0].uri, uri);
            assert_eq!(locations[0].range.start, Position::new(1, 15));
        }
        let hover = service.inner().hover(hover_params(uri, line, character)).await.unwrap().expect("field hover");
        let text = hover_text(hover.contents);
        assert!(text.contains("int state"), "{text}");
        assert!(!text.contains("typedef int state"), "{text}");
    }
}

#[tokio::test]
async fn owner_member_does_not_mix_same_named_fields_or_global_fallbacks() {
    let source = "typedef int state;\nstruct A { int state; };\nstruct B { double state; };\ndouble f(struct B *p) { return p->state/*cursor*/; }\n";
    let (_dir, service, uri, line, character) =
        indexed_backend_with_open_doc(&[], "main.c", source).await;
    let locations = definition_locations(
        service
            .inner()
            .goto_definition(goto_definition_params(uri.clone(), line, character))
            .await
            .unwrap()
            .expect("B field"),
    );
    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0].range.start, Position::new(2, 18));
    for source in [
        "typedef int state; struct S { int state; }; int f(void) { return unknown->state/*cursor*/; }",
        "typedef int state; struct S { int other; }; int f(struct S *p) { return p->state/*cursor*/; }",
        "typedef int state; struct S { int state; }; int f(void) { return get()->state/*cursor*/; }",
        "typedef int state; struct S { int state; }; int f(struct S *p) { return (p + 1)->state/*cursor*/; }",
    ] {
        let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(&[], "main.c", source).await;
        assert!(service.inner().goto_definition(goto_definition_params(uri.clone(), line, character)).await.unwrap().is_none(), "{source}");
        assert!(service.inner().hover(hover_params(uri, line, character)).await.unwrap().is_none(), "{source}");
    }
}

#[tokio::test]
async fn owner_member_respects_lexical_scope_and_alias_cycles() {
    for source in [
        "struct S { int state; }; void first(struct S *p) {} int second(void) { return p->state/*cursor*/; }",
        "struct S { int state; }; int f(struct S *p) { { int p; return p->state/*cursor*/; } }",
        "typedef B A; typedef A B; int f(A *p) { return p->state/*cursor*/; }",
    ] {
        let (_dir, service, uri, line, col) = indexed_backend_with_open_doc(&[], "main.c", source).await;
        assert!(service.inner().goto_definition(goto_definition_params(uri, line, col)).await.unwrap().is_none(), "{source}");
    }
}

#[tokio::test]
async fn owner_member_go_stays_in_proven_package() {
    let files = [
        (
            "other/types.go",
            "package other\ntype Device struct { State string }\n",
        ),
        (
            "pkg/types.go",
            "package main\ntype Device struct { State int }\n",
        ),
        ("types.c", "struct Device { double State; };\n"),
    ];
    let (_dir, service, uri, line, col) = indexed_backend_with_open_doc(
        &files,
        "pkg/main.go",
        "package main\nfunc f(p *Device) int { return p.State/*cursor*/ }\n",
    )
    .await;
    let locations = definition_locations(
        service
            .inner()
            .goto_definition(goto_definition_params(uri, line, col))
            .await
            .unwrap()
            .expect("same-package field"),
    );
    assert_eq!(locations.len(), 1);
    assert!(locations[0].uri.path().ends_with("/pkg/types.go"));
    assert_eq!(locations[0].range.start, Position::new(1, 21));
}

#[tokio::test]
async fn owner_member_dirty_owner_deletion_never_revives_stored_field() {
    let (dir, service, uri, line, col) = indexed_backend_with_open_doc(
        &[("api.h", "struct S { int state; };\n")],
        "main.c",
        "#include \"api.h\"\nint f(struct S *p) { return p->state/*cursor*/; }\n",
    )
    .await;
    assert!(service
        .inner()
        .goto_definition(goto_definition_params(uri.clone(), line, col))
        .await
        .unwrap()
        .is_some());
    let header = Url::from_file_path(dir.path().join("api.h")).unwrap();
    open_test_document(
        &service,
        header,
        1,
        "struct S { int replacement; };\n".into(),
    )
    .await;
    assert!(service
        .inner()
        .goto_definition(goto_definition_params(uri.clone(), line, col))
        .await
        .unwrap()
        .is_none());
    assert!(service
        .inner()
        .hover(hover_params(uri, line, col))
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn owner_member_old_session_keeps_its_owner_revision() {
    let (dir, service, uri, line, col) = indexed_backend_with_open_doc(
        &[("api.h", "struct S { int state; };\n")],
        "main.c",
        "#include \"api.h\"\nint f(struct S *p) { return p->state/*cursor*/; }\n",
    )
    .await;
    let backend = service.inner();
    let header = Url::from_file_path(dir.path().join("api.h")).unwrap();
    open_test_document(
        &service,
        header.clone(),
        1,
        "struct S { int state; };\n".into(),
    )
    .await;
    let captured = backend.capture_query_session(&uri).await.unwrap();
    let (version, text) = backend
        .document_snapshot_from_request(&uri, &captured.documents)
        .await
        .unwrap();
    let syntax = captured
        .bind_cursor(
            backend,
            &uri,
            (version, text.clone()),
            "state",
            crate::query::byte_offset_at(&text, line, col),
        )
        .await
        .unwrap()
        .syntax;
    backend
        .session
        .change_document(header, 2, "struct S { int replacement; };\n".into())
        .await;
    let mut timer = super::super::query_session::BindingTimer::new(backend, "member-test");
    let members = backend
        .bound_members(
            &captured,
            &uri,
            (version, text),
            &syntax,
            "state",
            &mut timer,
        )
        .await;
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].member.name, "state");
    assert_eq!(
        members[0].generation,
        captured.context.engine.semantic_generation
    );
    assert!(backend
        .goto_definition(goto_definition_params(uri, line, col))
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn owner_member_tag_and_typedef_with_same_name_are_distinct() {
    let source = "struct S { int state; };\nstruct Other { double state; };\ntypedef struct Other S;\nint f(struct S *p) { return p->state/*cursor*/; }\n";
    let (_dir, service, uri, line, col) =
        indexed_backend_with_open_doc(&[], "main.c", source).await;
    let locations = definition_locations(
        service
            .inner()
            .goto_definition(goto_definition_params(uri, line, col))
            .await
            .unwrap()
            .expect("tag owner"),
    );
    assert_eq!(locations.len(), 1, "{locations:?}");
    assert_eq!(locations[0].range.start, Position::new(0, 15));
}

#[tokio::test]
async fn owner_member_complex_initializer_cannot_fall_back_to_outer_field() {
    let source = "struct Inner { int state; }; struct S { struct Inner inner; int state; }; int f(struct Inner inner) { struct S s = {.inner.state/*cursor*/ = 1}; return 0; }";
    let (_dir, service, uri, line, col) =
        indexed_backend_with_open_doc(&[], "main.c", source).await;
    assert!(service
        .inner()
        .goto_definition(goto_definition_params(uri.clone(), line, col))
        .await
        .unwrap()
        .is_none());
    assert!(service
        .inner()
        .hover(hover_params(uri, line, col))
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn owner_member_simple_chain_keeps_terminal_owner() {
    let source = "struct Inner { int state; };\nstruct Outer { struct Inner inner; double state; };\nint f(struct Outer *p) { return p->inner.state/*cursor*/; }\n";
    let (_dir, service, uri, line, col) =
        indexed_backend_with_open_doc(&[], "main.c", source).await;
    let locations = definition_locations(
        service
            .inner()
            .goto_definition(goto_definition_params(uri, line, col))
            .await
            .unwrap()
            .expect("terminal member"),
    );
    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0].range.start, Position::new(0, 19));
}

#[tokio::test]
async fn owner_member_alias_domain_is_not_starved_by_same_named_tags() {
    let names: Vec<_> = (0..80).map(|i| format!("unrelated{i}.h")).collect();
    let files: Vec<_> = names
        .iter()
        .map(|name| (name.as_str(), "struct S { double wrong; };\n"))
        .collect();
    let source = "struct Other { int state; };\ntypedef struct Other S;\nint f(S *p) { return p->state/*cursor*/; }\n";
    let (_dir, service, uri, line, col) =
        indexed_backend_with_open_doc(&files, "main.c", source).await;
    let locations = definition_locations(
        service
            .inner()
            .goto_definition(goto_definition_params(uri, line, col))
            .await
            .unwrap()
            .expect("alias owner remains discoverable within its own domain"),
    );
    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0].range.start, Position::new(0, 19));
}

#[tokio::test]
async fn owner_member_hover_uses_comments_from_the_selected_revision() {
    let (dir, service, uri, line, col) = indexed_backend_with_open_doc(
        &[(
            "api.h",
            "struct S {\n/// Stored state description.\nint state;\n};\n",
        )],
        "main.c",
        "#include \"api.h\"\nint f(struct S *p) { return p->state/*cursor*/; }\n",
    )
    .await;
    let contents = hover_text(
        service
            .inner()
            .hover(hover_params(uri.clone(), line, col))
            .await
            .unwrap()
            .unwrap()
            .contents,
    );
    assert!(contents.contains("Stored state description"), "{contents}");
    let header = Url::from_file_path(dir.path().join("api.h")).unwrap();
    open_test_document(
        &service,
        header,
        1,
        "struct S {\n/// Unsaved state description.\nint state;\n};\n".into(),
    )
    .await;
    let contents = hover_text(
        service
            .inner()
            .hover(hover_params(uri, line, col))
            .await
            .unwrap()
            .unwrap()
            .contents,
    );
    assert!(
        contents.contains("Unsaved state description")
            && !contents.contains("Stored state description"),
        "{contents}"
    );
}

#[tokio::test]
async fn owner_member_simple_method_returns_its_proven_declaration() {
    let source = "struct W { int work(); };\nint f(W *p) { return p->work/*cursor*/(); }\n";
    let (_dir, service, uri, line, col) =
        indexed_backend_with_open_doc(&[], "main.cpp", source).await;
    for response in [
        service
            .inner()
            .goto_definition(goto_definition_params(uri.clone(), line, col))
            .await
            .unwrap(),
        service
            .inner()
            .goto_declaration(goto_definition_params(uri.clone(), line, col))
            .await
            .unwrap(),
    ] {
        let locations = definition_locations(response.expect("method declaration evidence"));
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].range.start, Position::new(0, 15));
    }
}

#[tokio::test]
async fn owner_member_unproven_subscript_and_go_qualified_types_stay_unsupported() {
    for (path, source) in [
        ("main.c", "struct S { int state; }; int f(struct S *p) { return p[0].state/*cursor*/; }"),
        ("main.go", "package main\nimport a \"other\"\ntype Device struct { State int }\nfunc f(p *a.Device) int { return p.State/*cursor*/ }\n"),
    ] {
        let (_dir, service, uri, line, col) = indexed_backend_with_open_doc(&[], path, source).await;
        assert!(service.inner().goto_definition(goto_definition_params(uri.clone(), line, col)).await.unwrap().is_none());
        assert!(service.inner().hover(hover_params(uri, line, col)).await.unwrap().is_none());
    }
}

#[tokio::test]
async fn owner_member_alias_chain_preserves_c_ordinary_namespace() {
    check_c_member_namespace(
        "struct A { int wrong; }; struct B { int right; }; typedef struct B A; typedef A T;",
        "T *p",
        "p->",
    )
    .await;
}

#[tokio::test]
async fn owner_member_chain_preserves_c_tag_field_type() {
    check_c_member_namespace("struct Inner { int right; }; struct Other { int wrong; }; typedef struct Other Inner; struct Outer { struct Inner inner; };", "struct Outer *p", "p->inner.").await;
}

async fn check_c_member_namespace(declarations: &str, parameter: &str, receiver: &str) {
    for closed in [false, true] {
        for (field, expected) in [("wrong", false), ("right", true)] {
            let indexed = if closed {
                vec![("types.h", declarations)]
            } else {
                Vec::new()
            };
            let prefix = if closed {
                "#include \"types.h\"\n"
            } else {
                declarations
            };
            let source =
                format!("{prefix}\nint f({parameter}) {{ return {receiver}{field}/*cursor*/; }}");
            let (_dir, service, uri, line, col) =
                indexed_backend_with_open_doc(&indexed, "main.c", &source).await;
            assert_eq!(
                service
                    .inner()
                    .goto_definition(goto_definition_params(uri.clone(), line, col))
                    .await
                    .unwrap()
                    .is_some(),
                expected,
                "closed={closed} {field}"
            );
            assert_eq!(
                service
                    .inner()
                    .hover(hover_params(uri, line, col))
                    .await
                    .unwrap()
                    .is_some(),
                expected,
                "closed={closed} {field}"
            );
        }
    }
}
