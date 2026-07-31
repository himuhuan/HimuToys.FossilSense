use crate::semantic_model::{DeclaratorShape, RecordKind};

use super::{RecordCandidate, TypeAliasCandidate, TypeAliasTarget};

pub(super) fn compose_aka_spelling(
    record: &RecordCandidate,
    trace: &[TypeAliasCandidate],
) -> Option<String> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Form {
        Plain,
        Pointer,
        Array,
    }

    let mut spelling = record.type_spelling();
    let mut form = Form::Plain;
    for alias in trace.iter().rev() {
        let target_surface = alias_target_surface(alias, record)?;
        let base_qualifiers = exact_base_qualifiers(&alias.underlying_spelling, &target_surface)?;

        // Base qualifiers belong to the type specifier rather than the
        // declarator. Applying them to an already-expanded pointer/array
        // alias requires placement information the limited shape does not
        // preserve (`const Ptr` is `T * const`, not `const T *`).
        if !base_qualifiers.is_empty() {
            if form != Form::Plain {
                return None;
            }
            spelling = format!("{} {spelling}", base_qualifiers.join(" "));
        }

        match &alias.declarator_shape {
            DeclaratorShape::Identity => {
                if !base_qualifiers.is_empty() {
                    return None;
                }
            }
            DeclaratorShape::Qualified { qualifiers } => {
                let qualifiers: Vec<_> = qualifiers
                    .iter()
                    .map(|qualifier| qualifier.trim())
                    .collect();
                if qualifiers.is_empty()
                    || qualifiers
                        .iter()
                        .copied()
                        .ne(base_qualifiers.iter().map(String::as_str))
                {
                    return None;
                }
            }
            DeclaratorShape::Pointer { qualifiers } => {
                if form == Form::Array {
                    return None;
                }
                spelling.push_str(" *");
                if !qualifiers.is_empty() {
                    spelling.push(' ');
                    spelling.push_str(&qualifiers.join(" "));
                }
                form = Form::Pointer;
            }
            DeclaratorShape::Array { extent_text } => {
                if form == Form::Array {
                    return None;
                }
                spelling.push('[');
                spelling.push_str(extent_text.trim());
                spelling.push(']');
                form = Form::Array;
            }
            DeclaratorShape::FunctionPointer { .. } | DeclaratorShape::Unsupported => return None,
        }
    }
    Some(spelling)
}

fn alias_target_surface(alias: &TypeAliasCandidate, record: &RecordCandidate) -> Option<String> {
    match &alias.target {
        TypeAliasTarget::StableRecord(identity) => {
            (identity == &record.identity).then(|| record_source_type_spelling(record))
        }
        TypeAliasTarget::NamedRecord { tag, kind } => (record.named_tag_matches(tag, *kind))
            .then(|| format!("{} {tag}", record_kind_keyword(*kind))),
        TypeAliasTarget::TypeName(name) => (!name.is_empty()).then(|| name.clone()),
    }
}

fn record_source_type_spelling(record: &RecordCandidate) -> String {
    match record.tag_name.as_deref() {
        Some(tag) => format!("{} {tag}", record_kind_keyword(record.kind)),
        None => record_kind_keyword(record.kind).to_string(),
    }
}

fn exact_base_qualifiers(underlying: &str, target: &str) -> Option<Vec<String>> {
    let underlying = compact_whitespace(underlying);
    let target = compact_whitespace(target);
    if underlying == target {
        return Some(Vec::new());
    }
    let prefix = underlying.strip_suffix(&target)?.trim_end();
    if prefix.len() == underlying.len() || prefix.is_empty() {
        return None;
    }
    let boundary = underlying.as_bytes().get(prefix.len()).copied()?;
    if !boundary.is_ascii_whitespace() {
        return None;
    }
    let qualifiers: Vec<String> = prefix.split_whitespace().map(str::to_string).collect();
    qualifiers
        .iter()
        .all(|qualifier| is_supported_base_qualifier(qualifier))
        .then_some(qualifiers)
}

fn compact_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_supported_base_qualifier(value: &str) -> bool {
    matches!(value, "const" | "volatile" | "restrict" | "_Atomic")
}

fn record_kind_keyword(kind: RecordKind) -> &'static str {
    match kind {
        RecordKind::Struct => "struct",
        RecordKind::Union => "union",
        RecordKind::Class => "class",
        RecordKind::Interface => "interface",
    }
}
