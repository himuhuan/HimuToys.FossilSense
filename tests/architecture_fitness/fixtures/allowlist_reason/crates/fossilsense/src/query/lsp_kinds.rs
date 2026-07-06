use tower_lsp::lsp_types::CompletionItemKind as LspCompletionItemKind;

pub fn transitional_kind() -> LspCompletionItemKind {
    LspCompletionItemKind::FUNCTION
}
