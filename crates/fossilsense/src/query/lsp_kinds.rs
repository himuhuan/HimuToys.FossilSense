use tower_lsp::lsp_types::CompletionItemKind as LspCompletionItemKind;
use tower_lsp::lsp_types::SymbolKind as LspSymbolKind;

use crate::parser::SymbolKind as ParserKind;

/// Map a stored kind string ("function"/"macro"/...) to an LSP symbol kind.
pub fn lsp_symbol_kind(kind: &str) -> LspSymbolKind {
    match kind {
        "function" => LspSymbolKind::FUNCTION,
        "macro" => LspSymbolKind::CONSTANT,
        "type" => LspSymbolKind::STRUCT,
        "enum_constant" => LspSymbolKind::ENUM_MEMBER,
        "global_variable" => LspSymbolKind::VARIABLE,
        _ => LspSymbolKind::VARIABLE,
    }
}

/// Map a freshly parsed symbol kind to an LSP symbol kind (document outline).
pub fn lsp_kind_from_parser(kind: ParserKind) -> LspSymbolKind {
    match kind {
        ParserKind::Function => LspSymbolKind::FUNCTION,
        ParserKind::Macro => LspSymbolKind::CONSTANT,
        ParserKind::Type => LspSymbolKind::STRUCT,
        ParserKind::EnumConstant => LspSymbolKind::ENUM_MEMBER,
        ParserKind::GlobalVariable => LspSymbolKind::VARIABLE,
        ParserKind::Field => LspSymbolKind::FIELD,
    }
}

#[allow(dead_code)]
pub fn lsp_completion_kind(kind: &str) -> LspCompletionItemKind {
    match kind {
        "function" => LspCompletionItemKind::FUNCTION,
        "macro" => LspCompletionItemKind::CONSTANT,
        "type" => LspCompletionItemKind::STRUCT,
        "enum_constant" => LspCompletionItemKind::ENUM_MEMBER,
        "global_variable" => LspCompletionItemKind::VARIABLE,
        _ => LspCompletionItemKind::TEXT,
    }
}

/// Direct enum -> LSP completion-item-kind mapping for the completion hot path.
/// Avoids a string round-trip when `RankedNameHit.kind` is already the parsed
/// `SymbolKind` cached in the in-memory `NameTable`.
pub fn lsp_completion_kind_from_parser(kind: ParserKind) -> LspCompletionItemKind {
    match kind {
        ParserKind::Function => LspCompletionItemKind::FUNCTION,
        ParserKind::Macro => LspCompletionItemKind::CONSTANT,
        ParserKind::Type => LspCompletionItemKind::STRUCT,
        ParserKind::EnumConstant => LspCompletionItemKind::ENUM_MEMBER,
        ParserKind::GlobalVariable => LspCompletionItemKind::VARIABLE,
        ParserKind::Field => LspCompletionItemKind::FIELD,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_kind_mapping() {
        assert_eq!(
            lsp_completion_kind("function"),
            LspCompletionItemKind::FUNCTION
        );
        assert_eq!(
            lsp_completion_kind("macro"),
            LspCompletionItemKind::CONSTANT
        );
        assert_eq!(lsp_completion_kind("type"), LspCompletionItemKind::STRUCT);
        assert_eq!(
            lsp_completion_kind("enum_constant"),
            LspCompletionItemKind::ENUM_MEMBER
        );
        assert_eq!(
            lsp_completion_kind("global_variable"),
            LspCompletionItemKind::VARIABLE
        );
        assert_eq!(lsp_completion_kind("unknown"), LspCompletionItemKind::TEXT);
    }
}
