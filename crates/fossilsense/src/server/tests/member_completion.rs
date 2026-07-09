use super::*;
use tower_lsp::LanguageServer as _;

#[tokio::test]
async fn member_completion_returns_fields_and_methods_for_resolved_receiver() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[(
            "widget.hpp",
            "struct Widget { int width; void resize(); };\n",
        )],
        "main.cpp",
        "#include \"widget.hpp\"\nvoid f(Widget *w) { w->/*cursor*/ }\n",
    )
    .await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let items = completion_items(response);

    assert!(items
        .iter()
        .any(|item| item.label == "resize" && item.kind == Some(CompletionItemKind::METHOD)));
    assert!(items
        .iter()
        .any(|item| item.label == "width" && item.kind == Some(CompletionItemKind::FIELD)));
}

#[tokio::test]
async fn member_completion_resolves_simple_nested_member_chain() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[(
            "nested.hpp",
            "struct Inner { int value; };\nstruct Outer { struct Inner mem1; };\n",
        )],
        "main.cpp",
        "#include \"nested.hpp\"\nvoid f(Outer *a) { a->mem1./*cursor*/ }\n",
    )
    .await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let items = completion_items(response);

    assert!(items
        .iter()
        .any(|item| item.label == "value" && item.kind == Some(CompletionItemKind::FIELD)));
}

#[tokio::test]
async fn member_completion_resolves_indexed_anonymous_nested_member_chain() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[(
            "nested.h",
            "typedef struct { struct { int xxx; } mem1[4]; } A;\n",
        )],
        "main.c",
        "#include \"nested.h\"\nvoid f(void) { A a; a.mem1[0]./*cursor*/ }\n",
    )
    .await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let items = completion_items(response);

    assert!(items
        .iter()
        .any(|item| item.label == "xxx" && item.kind == Some(CompletionItemKind::FIELD)));
}

#[tokio::test]
async fn member_completion_falls_back_when_chain_parse_fails() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[("widget.hpp", "struct Widget { int width; int window; };\n")],
        "main.cpp",
        "void f(void) { make_widget()->wi/*cursor*/ }\n",
    )
    .await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let items = completion_items(response);

    assert!(items
        .iter()
        .any(|item| item.label == "width" && item.kind == Some(CompletionItemKind::FIELD)));
    assert!(items
        .iter()
        .any(|item| item.label == "window" && item.kind == Some(CompletionItemKind::FIELD)));
}

#[tokio::test]
async fn member_completion_does_not_leak_global_owner_when_reachable_owner_lacks_prefix() {
    let (dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[
            ("reachable.hpp", "struct W { int width; };\n"),
            ("global.hpp", "struct W { int height; };\n"),
        ],
        "main.cpp",
        "#include \"reachable.hpp\"\nvoid f(W *w) { w->he/*cursor*/ }\n",
    )
    .await;
    service
        .inner()
        .session
        .cache
        .reach_graphs
        .lock()
        .await
        .insert(
            dir.path().to_path_buf(),
            Arc::new(std::sync::RwLock::new(
                crate::reachability::ReachGraph::new(
                    vec![("main.cpp".to_string(), "reachable.hpp".to_string())],
                    vec![],
                    vec![],
                ),
            )),
        );

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    let items = completion_items(response);

    assert!(
        !items.iter().any(|item| item.label == "height"),
        "global W::height must not leak when reachable W has members but no 'he' member"
    );
    assert!(
        items.is_empty(),
        "resolved receiver should return an empty incomplete list instead of falling back"
    );
}

#[tokio::test]
async fn member_fallback_still_blocks_one_character_prefix() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[("widget.hpp", "struct Widget { int width; void wipe(); };\n")],
        "main.cpp",
        "void f(void) { make_widget()->w/*cursor*/ }\n",
    )
    .await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");
    assert!(completion_items(response).is_empty());
}

#[tokio::test]
async fn weak_receiver_uses_member_fallback_min_prefix_gate() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[("widget.hpp", "struct Widget { int width; int window; };\n")],
        "main.cpp",
        "void f(void) { widget->w/*cursor*/ }\n",
    )
    .await;

    let response = service
        .inner()
        .completion(completion_params(uri, line, character))
        .await
        .expect("completion request")
        .expect("completion response");

    assert!(
        completion_items(response).is_empty(),
        "weak receiver correlation must not bypass the member fallback short-prefix gate"
    );
}
