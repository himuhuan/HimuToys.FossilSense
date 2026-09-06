use std::ops::Range;

use super::{ranges_overlap, ProposedEdit};

pub(in crate::parser) fn attach_affected_ranges(
    root: tree_sitter::Node<'_>,
    edits: &mut [ProposedEdit],
) {
    for edit in edits {
        let syntax_range = syntax_affected_range(root, &edit.range);
        edit.affected_range = edit.affected_range.start.min(syntax_range.start)
            ..edit.affected_range.end.max(syntax_range.end);
    }
}

pub(super) fn alignment_decorator_range(
    root: tree_sitter::Node<'_>,
    source: &str,
    edit: &Range<usize>,
) -> Option<Range<usize>> {
    let token_end = edit.start.checked_add("__aligned".len())?;
    let identifier = root.descendant_for_byte_range(edit.start, token_end)?;
    if !identifier.kind().ends_with("identifier")
        || identifier.start_byte() != edit.start
        || identifier.end_byte() != token_end
    {
        return None;
    }

    let parent = identifier.parent()?;
    if parent.kind() == "init_declarator"
        && parent
            .child_by_field_name("declarator")
            .is_some_and(|declarator| declarator.byte_range() == identifier.byte_range())
    {
        let declaration = parent.parent()?;
        let known_record_suffix = declaration.kind() == "declaration"
            && declaration.child_by_field_name("type").is_some_and(|kind| {
                matches!(kind.kind(), "struct_specifier" | "union_specifier")
                    && kind.child_by_field_name("body").is_some()
            });
        return known_record_suffix.then(|| declaration.byte_range());
    }
    if parent.kind() == "call_expression"
        && parent
            .child_by_field_name("function")
            .is_some_and(|function| function.byte_range() == identifier.byte_range())
    {
        let statement = parent.parent()?;
        if statement.kind() != "expression_statement" {
            return None;
        }
        let previous = statement.prev_named_sibling()?;
        if matches!(
            previous.kind(),
            "declaration" | "field_declaration" | "type_definition"
        ) && has_missing_terminal_semicolon(previous)
            && source
                .get(previous.end_byte()..edit.start)
                .is_some_and(|gap| gap.chars().all(char::is_whitespace))
        {
            return Some(previous.start_byte()..statement.end_byte());
        }
        return None;
    }

    let mut node = parent;
    let function_declarator = loop {
        if node.kind() == "function_declarator"
            && node
                .child_by_field_name("declarator")
                .is_some_and(|declarator| declarator.byte_range() == identifier.byte_range())
        {
            break node;
        }
        node = node.parent()?;
    };
    let mut declaration = function_declarator.parent()?;
    while !matches!(declaration.kind(), "declaration" | "field_declaration") {
        declaration = declaration.parent()?;
    }
    if !declaration.has_error() {
        return None;
    }
    let known_record_suffix = declaration.kind() == "declaration"
        && declaration.child_by_field_name("type").is_some_and(|kind| {
            matches!(kind.kind(), "struct_specifier" | "union_specifier")
                && kind.child_by_field_name("body").is_some()
        });
    let known_field_suffix = declaration.kind() == "field_declaration"
        && (0..declaration.named_child_count()).any(|index| {
            declaration.named_child(index).is_some_and(|child| {
                child.kind() == "ERROR" && child.end_byte() <= function_declarator.start_byte()
            })
        });
    let known_prior_declarator = has_value_identifier_before(declaration, identifier.start_byte());
    (known_record_suffix || known_field_suffix || known_prior_declarator)
        .then(|| declaration.byte_range())
}

fn has_missing_terminal_semicolon(declaration: tree_sitter::Node<'_>) -> bool {
    (0..declaration.child_count()).any(|index| {
        declaration.child(index).is_some_and(|child| {
            child.is_missing() && child.kind() == ";" && child.end_byte() == declaration.end_byte()
        })
    })
}

fn has_value_identifier_before(root: tree_sitter::Node<'_>, before: usize) -> bool {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "identifier" | "field_identifier") && node.end_byte() <= before {
            return true;
        }
        for index in 0..node.named_child_count() {
            if let Some(child) = node.named_child(index) {
                stack.push(child);
            }
        }
    }
    false
}

fn syntax_affected_range(root: tree_sitter::Node<'_>, edit: &Range<usize>) -> Range<usize> {
    let Some(mut node) = root.descendant_for_byte_range(edit.start, edit.end) else {
        return edit.clone();
    };
    loop {
        if matches!(
            node.kind(),
            "declaration"
                | "field_declaration"
                | "type_definition"
                | "function_definition"
                | "expression_statement"
        ) || node.kind().starts_with("preproc_")
        {
            return node.byte_range();
        }
        let Some(parent) = node.parent() else {
            return edit.clone();
        };
        node = parent;
    }
}

#[derive(Debug, PartialEq, Eq)]
struct HealthyObservation {
    kind: String,
    parent_kind: String,
    range: Range<usize>,
    name: Vec<u8>,
    leading_context: Vec<u8>,
    trailing_context: Vec<u8>,
}

pub(in crate::parser) fn healthy_observations_unchanged(
    before: tree_sitter::Node<'_>,
    after: tree_sitter::Node<'_>,
    original: &str,
    affected: &[Range<usize>],
) -> bool {
    healthy_observations(before, original, affected)
        == healthy_observations(after, original, affected)
}

fn healthy_observations(
    root: tree_sitter::Node<'_>,
    original: &str,
    affected: &[Range<usize>],
) -> Vec<HealthyObservation> {
    let bytes = original.as_bytes();
    let mut observations = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind().ends_with("identifier")
            && !node.has_error()
            && !node.is_missing()
            && !affected
                .iter()
                .any(|range| ranges_overlap(&node.byte_range(), range))
        {
            let range = node.byte_range();
            if let Some(name) = bytes.get(range.clone()) {
                observations.push(HealthyObservation {
                    kind: node.kind().to_string(),
                    parent_kind: node.parent().map_or("", |parent| parent.kind()).to_string(),
                    range: range.clone(),
                    name: name.to_vec(),
                    leading_context: bytes[range.start.saturating_sub(16)..range.start].to_vec(),
                    trailing_context: bytes[range.end..(range.end + 16).min(bytes.len())].to_vec(),
                });
            }
        }
        for index in (0..node.named_child_count()).rev() {
            if let Some(child) = node.named_child(index) {
                stack.push(child);
            }
        }
    }
    observations
}
