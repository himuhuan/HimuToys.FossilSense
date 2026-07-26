use super::*;

pub(super) struct NormalizedCallTarget<'tree> {
    pub(super) name_node: Option<tree_sitter::Node<'tree>>,
    pub(super) name: Option<String>,
    pub(super) qualified_name: Option<String>,
    pub(super) form: CallForm,
}

pub(super) fn normalize_call_target<'tree>(
    node: tree_sitter::Node<'tree>,
    source: &str,
) -> NormalizedCallTarget<'tree> {
    match node.kind() {
        "identifier" => NormalizedCallTarget {
            name_node: Some(node),
            name: text(node, source).map(str::to_string),
            qualified_name: None,
            form: CallForm::DirectName,
        },
        "qualified_identifier" => {
            let qualified = text(node, source).map(canonical_qualified_name);
            let name_node = node
                .child_by_field_name("name")
                .or_else(|| last_identifier(node));
            NormalizedCallTarget {
                name: name_node
                    .and_then(|name| text(name, source))
                    .map(str::to_string),
                name_node,
                qualified_name: qualified,
                form: CallForm::QualifiedName,
            }
        }
        "parenthesized_expression" => {
            let inner = named_children(node).into_iter().next();
            let Some(inner) = inner else {
                return unsupported_target(CallForm::Unsupported);
            };
            let mut target = normalize_call_target(inner, source);
            if matches!(target.form, CallForm::DirectName | CallForm::QualifiedName) {
                target.form = CallForm::ParenthesizedName;
            }
            target
        }
        "field_expression" => {
            let name_node = node
                .child_by_field_name("field")
                .or_else(|| last_identifier(node));
            let raw = text(node, source).unwrap_or_default();
            NormalizedCallTarget {
                name: name_node
                    .and_then(|name| text(name, source))
                    .map(str::to_string),
                name_node,
                qualified_name: None,
                form: if raw.contains("->") {
                    CallForm::MemberArrow
                } else {
                    CallForm::MemberDot
                },
            }
        }
        "pointer_expression" => unsupported_target(CallForm::FunctionPointer),
        _ => unsupported_target(CallForm::Unsupported),
    }
}

pub(super) fn canonical_qualified_name(raw: &str) -> String {
    raw.split("::")
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("::")
}

pub(super) fn unsupported_target(form: CallForm) -> NormalizedCallTarget<'static> {
    NormalizedCallTarget {
        name_node: None,
        name: None,
        qualified_name: None,
        form,
    }
}

pub(super) fn callable_name<'tree>(
    declarator: tree_sitter::Node<'tree>,
    source: &str,
) -> Option<(tree_sitter::Node<'tree>, Option<String>, String)> {
    if matches!(declarator.kind(), "identifier" | "field_identifier") {
        return Some((declarator, None, text(declarator, source)?.to_string()));
    }
    if declarator.kind() == "qualified_identifier" {
        let full = canonical_qualified_name(text(declarator, source)?);
        let name_node = declarator
            .child_by_field_name("name")
            .or_else(|| last_identifier(declarator))?;
        let name = text(name_node, source)?.to_string();
        let owner = full.rsplit_once("::").map(|(owner, _)| owner.to_string());
        return Some((name_node, owner, name));
    }
    if let Some(child) = declarator.child_by_field_name("declarator") {
        return callable_name(child, source);
    }
    let identifier = last_identifier(declarator)?;
    Some((identifier, None, text(identifier, source)?.to_string()))
}

pub(super) fn signature_shape(
    declarator: tree_sitter::Node<'_>,
    source: &str,
    is_cpp: bool,
) -> SignatureShape {
    let parameters = declarator
        .child_by_field_name("parameters")
        .or_else(|| find_descendant(declarator, "parameter_list"));
    let Some(parameters) = parameters else {
        return SignatureShape {
            normalized: String::new(),
            min_arity: None,
            max_arity: None,
            variadic: false,
        };
    };
    let normalized = compact_whitespace(text(parameters, source).unwrap_or_default());
    let children = named_children(parameters);
    if children.len() == 1 && text(children[0], source).is_some_and(|value| value.trim() == "void")
    {
        return SignatureShape {
            normalized,
            min_arity: Some(0),
            max_arity: Some(0),
            variadic: false,
        };
    }
    let mut min = 0u32;
    let mut max = 0u32;
    // Tree-sitter represents the C/C++ ellipsis as an unnamed token, so it is
    // absent from `named_children(parameters)`. Inspect the full subtree while
    // continuing to count only named parameter declarations below.
    let mut variadic = contains_syntax_kind(parameters, "...");
    for child in children {
        if child.kind().contains("variadic") {
            variadic = true;
            continue;
        }
        if child.kind().contains("parameter") {
            max += 1;
            // Only C++'s explicit optional-parameter AST node/field proves a
            // default argument. Looking for `=` in source text confuses
            // operators inside a required parameter's type (for example an
            // array extent containing `sizeof(1 == 1)`) with a default.
            let has_default = child.kind() == "optional_parameter_declaration"
                || child.child_by_field_name("default_value").is_some();
            if !has_default {
                min += 1;
            }
        }
    }
    let empty_c_parameters = !is_cpp && min == 0 && max == 0 && !variadic;
    SignatureShape {
        normalized,
        min_arity: (!empty_c_parameters).then_some(min),
        max_arity: if variadic || empty_c_parameters {
            None
        } else {
            Some(max)
        },
        variadic,
    }
}

pub(super) fn has_storage_class(node: tree_sitter::Node<'_>, source: &str, expected: &str) -> bool {
    named_children(node).into_iter().any(|child| {
        child.kind() == "storage_class_specifier" && text(child, source) == Some(expected)
    })
}

pub(super) fn declarator_is_pointer_like(node: tree_sitter::Node<'_>) -> bool {
    let declarator = node.child_by_field_name("declarator");
    declarator.is_some_and(|declarator| {
        matches!(
            declarator.kind(),
            "pointer_declarator" | "parenthesized_declarator"
        ) && find_descendant(declarator, "pointer_declarator").is_some()
    })
}

pub(super) fn find_descendant<'tree>(
    root: tree_sitter::Node<'tree>,
    kind: &str,
) -> Option<tree_sitter::Node<'tree>> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == kind {
            return Some(node);
        }
        stack.extend(named_children(node).into_iter().rev());
    }
    None
}

pub(super) fn contains_syntax_kind(root: tree_sitter::Node<'_>, kind: &str) -> bool {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == kind {
            return true;
        }
        for index in 0..node.child_count() {
            if let Some(child) = node.child(index) {
                stack.push(child);
            }
        }
    }
    false
}

pub(super) fn last_identifier(root: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    let mut found = None;
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "identifier" | "field_identifier") {
            found = Some(node);
        }
        stack.extend(named_children(node));
    }
    found
}

pub(super) fn named_argument_count(arguments: tree_sitter::Node<'_>) -> u32 {
    named_children(arguments).len() as u32
}

pub(super) fn named_children(node: tree_sitter::Node<'_>) -> Vec<tree_sitter::Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

pub(super) fn text<'a>(node: tree_sitter::Node<'_>, source: &'a str) -> Option<&'a str> {
    source.get(node.start_byte()..node.end_byte())
}

pub(super) fn compact_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn trim_ascii_whitespace_end(source: &str, start: usize, end: usize) -> usize {
    let mut trimmed = end.min(source.len());
    while trimmed > start && source.as_bytes()[trimmed - 1].is_ascii_whitespace() {
        trimmed -= 1;
    }
    trimmed
}

pub(super) fn canonical_full_signature(presentation: &str) -> String {
    let value = presentation.trim().trim_end_matches(';').trim_end();
    let mut output = String::with_capacity(value.len());
    let mut pending_space = false;
    for ch in value.chars() {
        if ch.is_whitespace() {
            pending_space = !output.is_empty();
            continue;
        }
        if matches!(ch, '(' | ')' | '[' | ']' | ',' | '*' | '&') {
            let preserves_token_boundary = pending_space
                && output
                    .chars()
                    .last()
                    .is_some_and(|previous| would_merge_operator_token(previous, ch));
            if preserves_token_boundary {
                output.push(' ');
            } else {
                while output.ends_with(' ') {
                    output.pop();
                }
            }
            output.push(ch);
            pending_space = false;
            continue;
        }
        if pending_space
            && !output.is_empty()
            && output.chars().last().is_some_and(|last| {
                would_merge_operator_token(last, ch) || !matches!(last, '(' | '[' | ',' | '*' | '&')
            })
        {
            output.push(' ');
        }
        output.push(ch);
        pending_space = false;
    }
    output
}

/// Callable identity ignores parameter identifiers in both C and C++. Names
/// are not part of either language's function type, and retaining them would
/// split a C++ header declaration from its C implementation merely because
/// only one side spells the names. C++-specific trailing qualifiers and
/// default expressions remain token-preserved.
pub(super) fn canonical_callable_signature(
    declaration: tree_sitter::Node<'_>,
    function_declarator: tree_sitter::Node<'_>,
    name_node: tree_sitter::Node<'_>,
    name: &str,
    source: &str,
    is_cpp: bool,
    _presentation: &str,
) -> String {
    let raw_prefix = source
        .get(declaration.start_byte()..name_node.start_byte())
        .unwrap_or_default();
    let raw_prefix = if is_cpp {
        strip_trailing_cpp_owner_qualification(raw_prefix)
    } else {
        raw_prefix
    };
    let prefix = raw_prefix
        .split_whitespace()
        .filter(|token| *token != "extern")
        .collect::<Vec<_>>()
        .join(" ");
    let parameters = function_declarator
        .child_by_field_name("parameters")
        .or_else(|| find_descendant(function_declarator, "parameter_list"));
    let parameter_shape = parameters.map_or_else(String::new, |parameters| {
        parameter_shape_without_names(parameters, source)
    });
    let trailing = if is_cpp {
        let declaration_end = declaration
            .child_by_field_name("body")
            .map(|body| {
                trim_ascii_whitespace_end(source, declaration.start_byte(), body.start_byte())
            })
            .unwrap_or_else(|| declaration.end_byte());
        parameters
            .and_then(|parameters| source.get(parameters.end_byte()..declaration_end))
            .unwrap_or_default()
    } else {
        ""
    };
    canonical_full_signature(&format!("{prefix} {name}{parameter_shape}{trailing}"))
}

pub(super) fn parameter_shape_without_names(
    parameters: tree_sitter::Node<'_>,
    source: &str,
) -> String {
    let mut value = source
        .get(parameters.start_byte()..parameters.end_byte())
        .unwrap_or_default()
        .to_string();
    let mut removals = Vec::new();
    let mut stack = vec![parameters];
    while let Some(node) = stack.pop() {
        if node.kind().contains("parameter") {
            if let Some(identifier) = node
                .child_by_field_name("declarator")
                .and_then(parameter_declarator_identifier)
            {
                removals.push((
                    identifier
                        .start_byte()
                        .saturating_sub(parameters.start_byte()),
                    identifier
                        .end_byte()
                        .saturating_sub(parameters.start_byte()),
                ));
            }
        }
        stack.extend(named_children(node));
    }
    removals.sort_unstable_by_key(|entry| std::cmp::Reverse(entry.0));
    removals.dedup();
    for (start, end) in removals {
        if start <= end && end <= value.len() {
            value.replace_range(start..end, "");
        }
    }
    value
}

pub(super) fn strip_trailing_cpp_owner_qualification(prefix: &str) -> &str {
    let trimmed = prefix.trim_end();
    if !trimmed.ends_with("::") {
        return prefix;
    }
    let Some(owner_start) = trimmed[..trimmed.len().saturating_sub(2)]
        .rfind(|ch: char| ch.is_whitespace() || matches!(ch, '*' | '&' | '('))
        .map(|index| index + 1)
    else {
        return prefix;
    };
    &prefix[..owner_start]
}

pub(super) fn parameter_declarator_identifier(
    declarator: tree_sitter::Node<'_>,
) -> Option<tree_sitter::Node<'_>> {
    if matches!(declarator.kind(), "identifier" | "field_identifier") {
        return Some(declarator);
    }
    if let Some(identifier) = declarator
        .child_by_field_name("declarator")
        .and_then(parameter_declarator_identifier)
    {
        return Some(identifier);
    }
    // `parenthesized_declarator` does not consistently expose a named
    // `declarator` field across the C/C++ grammars. Follow only declarator-
    // shaped children; never descend into a nested parameter list or a type
    // identifier, which would erase type information from an abstract
    // declarator.
    named_children(declarator).into_iter().find_map(|child| {
        (child.kind().ends_with("declarator")
            || matches!(child.kind(), "identifier" | "field_identifier"))
        .then(|| parameter_declarator_identifier(child))
        .flatten()
    })
}

/// Whitespace may be normalized only while preserving the C/C++ token stream.
/// In particular, joining `& &` into `&&` changes the meaning of default
/// expressions and could create a false strict declaration/definition pair.
pub(super) fn would_merge_operator_token(left: char, right: char) -> bool {
    matches!(
        (left, right),
        ('+', '+')
            | ('-', '-')
            | ('-', '>')
            | ('<', '<')
            | ('>', '>')
            | ('<', '=')
            | ('>', '=')
            | ('=', '=')
            | ('!', '=')
            | ('&', '&')
            | ('|', '|')
            | ('*', '=')
            | ('/', '=')
            | ('%', '=')
            | ('+', '=')
            | ('-', '=')
            | ('&', '=')
            | ('^', '=')
            | ('|', '=')
            | (':', ':')
            | ('.', '*')
            | ('>', '*')
            | ('#', '#')
            | ('/', '*')
            | ('/', '/')
            | ('<', ':')
            | (':', '>')
            | ('<', '%')
            | ('%', '>')
            | ('%', ':')
    )
}

pub(super) fn contains_error_or_missing(root: tree_sitter::Node<'_>) -> bool {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.is_error() || node.is_missing() {
            return true;
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    false
}

pub(super) fn digest(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..24].to_string()
}

pub(super) fn preprocessor_guard(node: tree_sitter::Node<'_>, source: &str) -> Option<String> {
    let mut guards = Vec::new();
    let mut parent = node.parent();
    while let Some(ancestor) = parent {
        if matches!(
            ancestor.kind(),
            "preproc_if" | "preproc_ifdef" | "preproc_ifndef" | "preproc_elif"
        ) {
            let line_end = source[ancestor.start_byte()..]
                .find('\n')
                .map_or(ancestor.end_byte(), |offset| ancestor.start_byte() + offset);
            if let Some(line) = source.get(ancestor.start_byte()..line_end) {
                guards.push(line.trim().to_string());
            }
        }
        parent = ancestor.parent();
    }
    guards.reverse();
    (!guards.is_empty()).then(|| guards.join("\n"))
}

pub(super) fn utf16_col(source: &str, line_starts: &[usize], row: usize, byte: usize) -> u32 {
    let line_start = line_starts.get(row).copied().unwrap_or(0).min(byte);
    source
        .get(line_start..byte)
        .unwrap_or_default()
        .encode_utf16()
        .count() as u32
}
