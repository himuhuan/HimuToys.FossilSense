use super::*;
use tower_lsp::LanguageServer as _;

#[tokio::test]
async fn goto_definition_uses_live_current_document_typedef_when_index_is_stale() {
    let dir = tempdir().expect("tempdir");
    write_workspace_file(dir.path(), "main.c", "void indexed_only(void) {}\n");
    crate::indexer::index_workspace(
        dir.path(),
        crate::indexer::IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let uri = Url::from_file_path(dir.path().join("main.c")).expect("file uri");
    let service = test_backend_service();
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(dir.path().to_path_buf());

    let (src, line, character) = text_and_position(
        "typedef struct {\n\
             int value;\n\
         } Boom;\n\
         \n\
         void f(void) {\n\
             Boom/*cursor*/ b;\n\
         }\n",
    );
    open_test_document(&service, uri.clone(), 2, src).await;

    let response = service
        .inner()
        .goto_definition(goto_definition_params(uri.clone(), line, character))
        .await
        .expect("goto definition")
        .expect("definition response");
    let locations = match response {
        GotoDefinitionResponse::Array(locations) => locations,
        GotoDefinitionResponse::Scalar(location) => vec![location],
        GotoDefinitionResponse::Link(_) => panic!("unexpected location links"),
    };

    assert!(
        locations
            .iter()
            .any(|location| location.uri == uri && location.range.start.line == 0),
        "live typedef definition should be returned even when the persisted index is stale"
    );
}

#[tokio::test]
async fn goto_definition_finds_first_typedef_after_multiline_macro_from_index() {
    let (_dir, service, uri, line, character) = indexed_backend_with_open_doc(
        &[],
        "macro_typedef.h",
        r#"#define FREE(ptr)                                                              \
    do                                                                         \
    {                                                                          \
        if ((ptr) != NULL)                                                     \
        {                                                                      \
            free(ptr);                                                         \
            (ptr) = NULL;                                                      \
        }                                                                      \
    } while (0)

typedef struct xxx {
    int value;
} xxx_t;

void use_type(void) {
    xxx_t/*cursor*/ item;
}
"#,
    )
    .await;

    let response = service
        .inner()
        .goto_definition(goto_definition_params(uri.clone(), line, character))
        .await
        .expect("goto definition")
        .expect("definition response");
    let locations = match response {
        GotoDefinitionResponse::Array(locations) => locations,
        GotoDefinitionResponse::Scalar(location) => vec![location],
        GotoDefinitionResponse::Link(_) => panic!("unexpected location links"),
    };

    assert!(
        locations
            .iter()
            .any(|location| location.uri == uri && location.range.start.line == 10),
        "indexed typedef immediately after multiline macro should be a goto-definition target"
    );
}

#[tokio::test]
async fn definition_hover_and_signature_preserve_read_view_candidate_order_and_labels() {
    let dir = tempdir().expect("tempdir");
    write_workspace_file(
        dir.path(),
        "src/main.c",
        "#include \"api.h\"\n\
         int target(void);\n\
         void call(void) {\n\
             target();\n\
         }\n",
    );
    write_workspace_file(dir.path(), "src/api.h", "int target(int reachable_arg);\n");
    write_workspace_file(
        dir.path(),
        "other/target.c",
        "int target(float global_arg) { return 0; }\n",
    );
    crate::indexer::index_workspace(
        dir.path(),
        crate::indexer::IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let uri = Url::from_file_path(dir.path().join("src/main.c")).expect("file uri");
    let service = test_backend_service();
    service
        .inner()
        .workspace_roots
        .lock()
        .await
        .push(dir.path().to_path_buf());
    let (open_text, hover_line, hover_character) = text_and_position(
        "#include \"api.h\"\n\
         int target(void);\n\
         void call(void) {\n\
             target/*cursor*/();\n\
         }\n",
    );
    let (sig_line, sig_character) = position_after(&open_text, "target(");
    open_test_document(&service, uri.clone(), 2, open_text).await;

    let response = service
        .inner()
        .goto_definition(goto_definition_params(
            uri.clone(),
            hover_line,
            hover_character,
        ))
        .await
        .expect("goto definition")
        .expect("definition response");
    let locations = match response {
        GotoDefinitionResponse::Array(locations) => locations,
        GotoDefinitionResponse::Scalar(location) => vec![location],
        GotoDefinitionResponse::Link(_) => panic!("unexpected location links"),
    };
    assert_eq!(locations[0].uri, uri);
    assert_eq!(locations[0].range.start.line, 1);

    let hover = service
        .inner()
        .hover(hover_params(uri.clone(), hover_line, hover_character))
        .await
        .expect("hover")
        .expect("hover response");
    let hover_text = match hover.contents {
        HoverContents::Markup(markup) => markup.value,
        HoverContents::Scalar(value) => marked_string_text(value),
        HoverContents::Array(values) => values
            .into_iter()
            .map(marked_string_text)
            .collect::<Vec<_>>()
            .join("\n"),
    };
    assert!(hover_text.contains("// In src/main.c\nint target(void);"));
    assert!(hover_text.contains("tier: current | confidence: exact | reason: current_file"));

    let signature = service
        .inner()
        .signature_help(signature_help_params(uri, sig_line, sig_character))
        .await
        .expect("signature help")
        .expect("signature response");
    assert_eq!(signature.active_signature, Some(0));
    assert_eq!(signature.signatures[0].label, "int target(void);");
    let documentation = match signature.signatures[0]
        .documentation
        .clone()
        .expect("signature docs")
    {
        Documentation::String(value) => value,
        Documentation::MarkupContent(markup) => markup.value,
    };
    assert!(documentation.contains("tier: current"));
    assert!(documentation.contains("confidence: exact"));
    assert!(documentation.contains("reason: current_file"));
}
