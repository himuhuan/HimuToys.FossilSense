use std::path::Path;

use tower_lsp::lsp_types::{DocumentSymbol, Location, Position, Range, SymbolInformation, Url};

use crate::model;
use crate::parser::Symbol;
use crate::query;
use crate::references::{self, ReferenceHit};
use crate::store::SymbolRecord;

pub(super) fn record_range(record: &SymbolRecord) -> Range {
    Range {
        start: Position {
            line: record.start_line,
            character: record.start_col,
        },
        end: Position {
            line: record.end_line,
            character: record.end_col,
        },
    }
}

pub(super) fn record_to_location(root: &Path, record: &SymbolRecord) -> Option<Location> {
    let relative = record.path.replace('/', std::path::MAIN_SEPARATOR_STR);
    let uri = Url::from_file_path(root.join(relative)).ok()?;
    Some(Location {
        uri,
        range: record_range(record),
    })
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
pub(super) fn record_to_symbol_information(
    root: &Path,
    record: &SymbolRecord,
) -> Option<SymbolInformation> {
    let location = record_to_location(root, record)?;
    Some(SymbolInformation {
        name: record.name.clone(),
        kind: query::lsp_symbol_kind(&record.kind),
        tags: None,
        deprecated: None,
        location,
        // Surface the conditional-compilation guard as the container hint.
        container_name: record.guard.clone(),
    })
}

#[allow(deprecated)]
pub(super) fn parsed_to_document_symbol(symbol: &Symbol) -> DocumentSymbol {
    let start = Position {
        line: symbol.start_line as u32,
        character: symbol.start_col as u32,
    };
    let range = Range {
        start,
        end: Position {
            line: symbol.end_line as u32,
            character: symbol.end_col as u32,
        },
    };
    DocumentSymbol {
        name: symbol.name.clone(),
        detail: Some(symbol.signature.clone()),
        kind: query::lsp_kind_from_parser(symbol.kind),
        tags: None,
        deprecated: None,
        range,
        // selection_range must be contained within range.
        selection_range: Range { start, end: start },
        children: None,
    }
}
