use std::path::Path;

use super::{
    infer_receiver_record, parse, FileSemanticIndex, Occurrence, SymbolKind, SyntacticRole,
};

mod lexical_provenance_fact_masks;
mod local_declarations_bindings;
mod occurrence_roles_coloring;
mod records_members_aliases;

/// Role of the (single) occurrence of `name` in a parsed buffer.
fn role_of(path: &str, source: &str, name: &str) -> Option<SyntacticRole> {
    let index = parse(Path::new(path), source);
    index
        .occurrences
        .iter()
        .find(|occ| occ.name == name)
        .map(|occ| occ.role)
}

/// Role of `name`'s occurrence on a specific (0-based) line. The occurrence
/// vec is not in source order, so position-keyed lookup is deterministic.
fn role_at_line(path: &str, source: &str, name: &str, line: u32) -> Option<SyntacticRole> {
    let index = parse(Path::new(path), source);
    index
        .occurrences
        .iter()
        .find(|occ| occ.name == name && occ.line == line)
        .map(|occ| occ.role)
}

fn field_containers(index: &FileSemanticIndex, name: &str) -> Vec<String> {
    index
        .fields
        .iter()
        .filter(|f| f.name == name)
        .map(|f| {
            index
                .records
                .iter()
                .find(|r| r.record_key == f.record_key)
                .map(|r| r.display_name.clone())
                .unwrap_or_default()
        })
        .collect()
}

/// Receiver inference over the parsed product's local declarations (the same
/// data the server feeds `infer_receiver_record`).
fn infer_in(path: &str, source: &str, name: &str, byte_offset: usize) -> Option<String> {
    let index = parse(Path::new(path), source);
    infer_receiver_record(&index.local_declarations, name, byte_offset)
}

fn occurrence_lines(occurrences: &[Occurrence], name: &str) -> Vec<u32> {
    occurrences
        .iter()
        .filter(|occ| occ.name == name)
        .map(|occ| occ.line)
        .collect()
}

fn fact_mask_source() -> &'static str {
    r#"#include "api.h"
#define FLAG 1
enum Color { RED };
struct Widget { int width; void resize(); static int count(); };
typedef Widget WidgetAlias;
int use(Widget *w, int count) {
    int local_value = count;
    w->width = local_value;
    return FLAG + RED;
}
"#
}

fn has_symbol(index: &FileSemanticIndex, name: &str, kind: SymbolKind) -> bool {
    index
        .symbols
        .iter()
        .any(|symbol| symbol.name == name && symbol.kind == kind)
}
