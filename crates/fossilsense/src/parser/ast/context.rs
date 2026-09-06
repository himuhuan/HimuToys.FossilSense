use crate::call_model::SourceRange;

pub(in crate::parser) const MAX_CONDITION_DEPTH: usize = 128;
pub(in crate::parser) const MAX_GUARD_BYTES: usize = 8 * 1024;
const BUDGET_EXHAUSTED_GUARD: &str = "unknown:budget_exhausted";
const UNKNOWN_CONDITION_GUARD: &str = "unknown:conditional";

#[derive(Debug)]
enum ScopeFrame {
    File {
        node_id: usize,
    },
    Namespace {
        node_id: usize,
        name: String,
    },
    Record {
        node_id: usize,
        name: Option<String>,
    },
    Function {
        node_id: usize,
        body_range: Option<SourceRange>,
    },
    Block {
        node_id: usize,
        range: SourceRange,
    },
}

impl ScopeFrame {
    fn node_id(&self) -> usize {
        match self {
            Self::File { node_id }
            | Self::Namespace { node_id, .. }
            | Self::Record { node_id, .. }
            | Self::Function { node_id, .. }
            | Self::Block { node_id, .. } => *node_id,
        }
    }
}

#[derive(Debug)]
struct GuardFrame {
    node_id: usize,
    chain_root_id: usize,
    expression: String,
}

/// Shared parser context for C-family declaration collectors.
///
/// The AST walker owns one instance and updates it on enter/exit. Individual
/// collectors only project the current owner, local scope, and conditional
/// evidence; they do not walk ancestors and invent separate scope rules.
#[derive(Debug, Default)]
pub(in crate::parser) struct DeclarationContext {
    scopes: Vec<ScopeFrame>,
    guards: Vec<GuardFrame>,
    error_depth: usize,
}

impl DeclarationContext {
    pub(in crate::parser) fn enter(
        &mut self,
        node: tree_sitter::Node<'_>,
        source: &str,
        line_starts: &[usize],
    ) {
        if node.is_error() || node.is_missing() {
            self.error_depth += 1;
        }

        if is_conditional_node(node) {
            let (chain_root_id, expression) = branch_expression(node, source);
            self.guards.push(GuardFrame {
                node_id: node.id(),
                chain_root_id,
                expression,
            });
        }

        match node.kind() {
            "translation_unit" => self.scopes.push(ScopeFrame::File { node_id: node.id() }),
            "namespace_definition" => {
                let name = node
                    .child_by_field_name("name")
                    .and_then(|name| node_text(name, source))
                    .unwrap_or("<anonymous>")
                    .to_string();
                self.scopes.push(ScopeFrame::Namespace {
                    node_id: node.id(),
                    name,
                });
            }
            "struct_specifier" | "union_specifier" | "class_specifier"
                if node.child_by_field_name("body").is_some() =>
            {
                let name = node
                    .child_by_field_name("name")
                    .and_then(|name| node_text(name, source))
                    .map(str::to_string);
                self.scopes.push(ScopeFrame::Record {
                    node_id: node.id(),
                    name,
                });
            }
            "function_definition" => {
                let body_range = node
                    .child_by_field_name("body")
                    .map(|body| source_range(body, source, line_starts));
                self.scopes.push(ScopeFrame::Function {
                    node_id: node.id(),
                    body_range,
                });
            }
            "compound_statement" => self.scopes.push(ScopeFrame::Block {
                node_id: node.id(),
                range: source_range(node, source, line_starts),
            }),
            _ => {}
        }
    }

    pub(in crate::parser) fn exit(&mut self, node: tree_sitter::Node<'_>) {
        if self
            .scopes
            .last()
            .is_some_and(|scope| scope.node_id() == node.id())
        {
            self.scopes.pop();
        }
        if self
            .guards
            .last()
            .is_some_and(|guard| guard.node_id == node.id())
        {
            self.guards.pop();
        }
        if node.is_error() || node.is_missing() {
            self.error_depth = self.error_depth.saturating_sub(1);
        }
    }

    pub(in crate::parser) fn owner(&self) -> Option<String> {
        let names = self
            .scopes
            .iter()
            .filter_map(|scope| match scope {
                ScopeFrame::Namespace { name, .. } => Some(name.as_str()),
                ScopeFrame::Record {
                    name: Some(name), ..
                } => Some(name.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        (!names.is_empty()).then(|| names.join("::"))
    }

    pub(in crate::parser) fn local_scope(
        &self,
        node: tree_sitter::Node<'_>,
    ) -> Option<(SourceRange, SourceRange)> {
        let function = self.scopes.iter().rev().find_map(|scope| match scope {
            ScopeFrame::Function {
                body_range: Some(range),
                ..
            } if range.start_byte <= node.start_byte() && node.end_byte() <= range.end_byte => {
                Some(*range)
            }
            _ => None,
        })?;
        let lexical = self
            .scopes
            .iter()
            .rev()
            .find_map(|scope| match scope {
                ScopeFrame::Block { range, .. } => Some(*range),
                _ => None,
            })
            .unwrap_or(function);
        Some((function, lexical))
    }

    pub(in crate::parser) fn guard(&self) -> Option<String> {
        if self.guards.is_empty() {
            return None;
        }

        let mut selected: Vec<(usize, &str)> = Vec::new();
        for frame in &self.guards {
            if let Some((_, expression)) = selected
                .iter_mut()
                .find(|(chain_root_id, _)| *chain_root_id == frame.chain_root_id)
            {
                *expression = frame.expression.as_str();
            } else {
                selected.push((frame.chain_root_id, frame.expression.as_str()));
            }
        }
        if selected.len() > MAX_CONDITION_DEPTH {
            return Some(BUDGET_EXHAUSTED_GUARD.to_string());
        }
        if selected
            .iter()
            .any(|(_, expression)| *expression == BUDGET_EXHAUSTED_GUARD)
        {
            return Some(BUDGET_EXHAUSTED_GUARD.to_string());
        }

        let guard = selected
            .into_iter()
            .map(|(_, expression)| expression)
            .collect::<Vec<_>>()
            .join("\n");
        if guard.len() > MAX_GUARD_BYTES {
            Some(BUDGET_EXHAUSTED_GUARD.to_string())
        } else {
            Some(guard)
        }
    }

    #[allow(dead_code)]
    pub(in crate::parser) fn has_error_context(&self) -> bool {
        self.error_depth > 0
    }
}

pub(crate) fn guard_for_node(node: tree_sitter::Node<'_>, source: &str) -> Option<String> {
    let mut ancestors = Vec::new();
    let mut parent = node.parent();
    while let Some(ancestor) = parent {
        if is_conditional_node(ancestor) {
            ancestors.push(ancestor);
        }
        parent = ancestor.parent();
    }
    ancestors.reverse();
    let mut context = DeclarationContext::default();
    for ancestor in ancestors {
        let (chain_root_id, expression) = branch_expression(ancestor, source);
        context.guards.push(GuardFrame {
            node_id: ancestor.id(),
            chain_root_id,
            expression,
        });
    }
    context.guard()
}

fn is_conditional_node(node: tree_sitter::Node<'_>) -> bool {
    matches!(
        node.kind(),
        "preproc_if" | "preproc_ifdef" | "preproc_elif" | "preproc_elifdef" | "preproc_else"
    )
}

fn branch_expression(node: tree_sitter::Node<'_>, source: &str) -> (usize, String) {
    let mut chain = vec![node];
    if matches!(node.kind(), "preproc_if" | "preproc_ifdef") {
        return (
            node.id(),
            directive_line(node, source).unwrap_or_else(|| UNKNOWN_CONDITION_GUARD.to_string()),
        );
    }
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        if is_conditional_node(parent) {
            chain.push(parent);
            if matches!(parent.kind(), "preproc_if" | "preproc_ifdef") {
                break;
            }
        } else {
            break;
        }
        ancestor = parent.parent();
    }
    chain.reverse();
    let root_id = chain.first().map_or(node.id(), |root| root.id());
    if chain.len() == 1 {
        return (
            root_id,
            directive_line(node, source).unwrap_or_else(|| UNKNOWN_CONDITION_GUARD.to_string()),
        );
    }

    let Some(current) = directive_line(node, source) else {
        return (root_id, UNKNOWN_CONDITION_GUARD.to_string());
    };
    let mut terms = Vec::with_capacity(chain.len());
    for previous in &chain[..chain.len() - 1] {
        let Some(line) = directive_line(*previous, source) else {
            return (root_id, UNKNOWN_CONDITION_GUARD.to_string());
        };
        terms.push(format!("!({line})"));
    }
    if node.kind() == "preproc_else" {
        terms.push(current);
    } else {
        terms.push(format!("({current})"));
    }
    let expression = terms.join(" && ");
    if expression.len() > MAX_GUARD_BYTES {
        (root_id, BUDGET_EXHAUSTED_GUARD.to_string())
    } else {
        (root_id, expression)
    }
}

fn directive_line(node: tree_sitter::Node<'_>, source: &str) -> Option<String> {
    let start = node.start_byte();
    let line_end = source[start..]
        .find('\n')
        .map_or(node.end_byte(), |offset| start + offset)
        .min(node.end_byte());
    source
        .get(start..line_end)
        .map(str::trim)
        .filter(|line| line.starts_with('#') && !line.is_empty())
        .map(str::to_string)
}

fn node_text<'a>(node: tree_sitter::Node<'_>, source: &'a str) -> Option<&'a str> {
    source.get(node.start_byte()..node.end_byte())
}

fn source_range(node: tree_sitter::Node<'_>, source: &str, line_starts: &[usize]) -> SourceRange {
    let start = node.start_position();
    let end = node.end_position();
    SourceRange {
        start: crate::call_model::SourcePosition {
            line: start.row as u32,
            character: utf16_col(source, line_starts, start.row, node.start_byte()),
        },
        end: crate::call_model::SourcePosition {
            line: end.row as u32,
            character: utf16_col(source, line_starts, end.row, node.end_byte()),
        },
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
    }
}

fn utf16_col(source: &str, line_starts: &[usize], row: usize, byte: usize) -> u32 {
    let line_start = line_starts.get(row).copied().unwrap_or(0).min(byte);
    source
        .get(line_start..byte)
        .unwrap_or_default()
        .encode_utf16()
        .count() as u32
}
