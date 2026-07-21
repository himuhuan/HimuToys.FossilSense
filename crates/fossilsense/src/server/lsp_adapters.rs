use std::path::Path;

use tower_lsp::lsp_types::{
    CompletionItemKind, DocumentSymbol, Location, Position, Range, SymbolInformation, SymbolKind,
    Url,
};

use crate::model;
use crate::references::{self, ReferenceHit};
use crate::semantic_model::{DeclarationFact, SemanticDeclarationKind};
use crate::store::views::DeclarationReadRow;

#[allow(dead_code)]
fn lsp_completion_kind(kind: &str) -> CompletionItemKind {
    match kind {
        "function" => CompletionItemKind::FUNCTION,
        "macro" => CompletionItemKind::CONSTANT,
        "type" => CompletionItemKind::STRUCT,
        "enum_constant" => CompletionItemKind::ENUM_MEMBER,
        "global_variable" => CompletionItemKind::VARIABLE,
        _ => CompletionItemKind::TEXT,
    }
}

/// Build an LSP `Location` from a labeled `DefinitionCandidate`. Positions are
/// already UTF-16 columns from the indexed symbol record.
pub(super) fn candidate_to_location(
    root: &Path,
    candidate: &model::DefinitionCandidate,
) -> Option<Location> {
    let relative = candidate.path.replace('/', std::path::MAIN_SEPARATOR_STR);
    let uri = Url::from_file_path(root.join(relative)).ok()?;
    Some(Location {
        uri,
        range: Range {
            start: Position {
                line: candidate.range.start_line,
                character: candidate.range.start_col,
            },
            end: Position {
                line: candidate.range.end_line,
                character: candidate.range.end_col,
            },
        },
    })
}

pub(super) fn hit_to_location(root: &Path, hit: &ReferenceHit) -> Option<Location> {
    let relative = hit.rel_path.replace('/', std::path::MAIN_SEPARATOR_STR);
    let uri = Url::from_file_path(root.join(relative)).ok()?;
    Some(Location {
        uri,
        range: Range {
            start: Position {
                line: hit.line,
                character: hit.start_col_utf16,
            },
            end: Position {
                line: hit.line,
                character: hit.end_col_utf16,
            },
        },
    })
}

/// One role-labeled reference hit for the grouped-references command. Carries
/// the standard LSP `Location` plus the best-effort syntactic `role` the plain
/// `textDocument/references` result cannot express.
#[derive(Debug, Clone, serde::Serialize)]
pub(super) struct GroupedReferenceItem {
    pub(super) location: Location,
    pub(super) role: &'static str,
}

/// Project role-sorted hits into serializable `{ location, role }` items for the
/// grouped-references command. Hits whose path cannot be turned into a URI are
/// dropped (same as `references`); the input order is preserved, so the caller
/// must sort with [`references::sort_hits_by_role`] first.
pub(super) fn grouped_reference_items(
    root: &Path,
    hits: &[ReferenceHit],
) -> Vec<GroupedReferenceItem> {
    hits.iter()
        .filter_map(|hit| {
            hit_to_location(root, hit).map(|location| GroupedReferenceItem {
                location,
                role: references::role_label(hit.role),
            })
        })
        .collect()
}

#[allow(deprecated)]
pub(super) fn declaration_to_symbol_information(
    root: &Path,
    row: &DeclarationReadRow,
) -> Option<SymbolInformation> {
    let declaration = &row.fact;
    let path = declaration.path.replace('/', std::path::MAIN_SEPARATOR_STR);
    let uri = Url::from_file_path(root.join(path)).ok()?;
    Some(SymbolInformation {
        name: declaration.name.clone(),
        kind: match declaration.declaration_kind {
            SemanticDeclarationKind::Function | SemanticDeclarationKind::Method => {
                SymbolKind::FUNCTION
            }
            SemanticDeclarationKind::Type | SemanticDeclarationKind::Alias => SymbolKind::STRUCT,
            SemanticDeclarationKind::Macro => SymbolKind::CONSTANT,
            SemanticDeclarationKind::EnumConstant => SymbolKind::ENUM_MEMBER,
            SemanticDeclarationKind::Object => SymbolKind::VARIABLE,
        },
        tags: None,
        deprecated: None,
        location: Location {
            uri,
            range: Range {
                start: Position {
                    line: declaration.name_range.start.line,
                    character: declaration.name_range.start.character,
                },
                end: Position {
                    line: declaration.name_range.end.line,
                    character: declaration.name_range.end.character,
                },
            },
        },
        container_name: None,
    })
}

#[allow(deprecated)]
pub(super) fn declaration_to_document_symbol(declaration: &DeclarationFact) -> DocumentSymbol {
    let range = Range {
        start: Position {
            line: declaration.declaration_range.start.line,
            character: declaration.declaration_range.start.character,
        },
        end: Position {
            line: declaration.declaration_range.end.line,
            character: declaration.declaration_range.end.character,
        },
    };
    let selection_range = Range {
        start: Position {
            line: declaration.name_range.start.line,
            character: declaration.name_range.start.character,
        },
        end: Position {
            line: declaration.name_range.end.line,
            character: declaration.name_range.end.character,
        },
    };
    DocumentSymbol {
        name: declaration.name.clone(),
        detail: declaration.canonical_signature.clone(),
        kind: match declaration.declaration_kind {
            SemanticDeclarationKind::Function | SemanticDeclarationKind::Method => {
                SymbolKind::FUNCTION
            }
            SemanticDeclarationKind::Object => SymbolKind::VARIABLE,
            SemanticDeclarationKind::Type | SemanticDeclarationKind::Alias => SymbolKind::STRUCT,
            SemanticDeclarationKind::EnumConstant => SymbolKind::ENUM_MEMBER,
            SemanticDeclarationKind::Macro => SymbolKind::CONSTANT,
        },
        tags: None,
        deprecated: None,
        range,
        selection_range,
        children: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_kind_mapping() {
        assert_eq!(
            lsp_completion_kind("function"),
            CompletionItemKind::FUNCTION
        );
        assert_eq!(lsp_completion_kind("macro"), CompletionItemKind::CONSTANT);
        assert_eq!(lsp_completion_kind("type"), CompletionItemKind::STRUCT);
        assert_eq!(
            lsp_completion_kind("enum_constant"),
            CompletionItemKind::ENUM_MEMBER
        );
        assert_eq!(
            lsp_completion_kind("global_variable"),
            CompletionItemKind::VARIABLE
        );
        assert_eq!(lsp_completion_kind("unknown"), CompletionItemKind::TEXT);
    }
}
