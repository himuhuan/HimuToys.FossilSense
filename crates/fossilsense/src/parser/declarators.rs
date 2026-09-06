use std::ops::Range;

use crate::semantic_model::DeclaratorShape;

pub(super) const MAX_DIRECT_DECLARATORS: usize = 256;
pub(super) const MAX_DERIVED_LAYERS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DerivedLayerKind {
    Pointer,
    Array,
    Function,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DeclaredEntityKind {
    Function,
    Object,
    Alias,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DeclaratorFailureReason {
    Unsupported,
    Malformed,
    BudgetExhausted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DeclaratorFailure {
    pub(super) range: Range<usize>,
    pub(super) reason: DeclaratorFailureReason,
}

#[derive(Debug, Clone)]
pub(super) struct DerivedLayer<'tree> {
    pub(super) kind: DerivedLayerKind,
    pub(super) node: tree_sitter::Node<'tree>,
}

#[derive(Debug, Clone)]
pub(super) struct DecodedDeclarator<'tree> {
    pub(super) declarator: tree_sitter::Node<'tree>,
    pub(super) name_node: tree_sitter::Node<'tree>,
    pub(super) name: String,
    pub(super) kind: DeclaredEntityKind,
    pub(super) layers: Vec<DerivedLayer<'tree>>,
    pub(super) initializer_range: Option<Range<usize>>,
}

#[derive(Debug, Clone)]
pub(super) struct DecodedDeclaration<'tree> {
    pub(super) common_prefix_range: Range<usize>,
    has_trailing_semicolon: bool,
    pub(super) entities: Vec<DecodedDeclarator<'tree>>,
    pub(super) failures: Vec<DeclaratorFailure>,
}

impl DecodedDeclarator<'_> {
    pub(super) fn function_node(&self) -> Option<tree_sitter::Node<'_>> {
        self.layers
            .iter()
            .find(|layer| layer.kind == DerivedLayerKind::Function)
            .map(|layer| layer.node)
    }

    pub(super) fn core_declarator(&self) -> tree_sitter::Node<'_> {
        if self.declarator.kind() == "init_declarator" {
            self.declarator
                .child_by_field_name("declarator")
                .unwrap_or(self.declarator)
        } else {
            self.declarator
        }
    }

    pub(super) fn declarator_shape(&self, source: &str) -> DeclaratorShape {
        let signature = || {
            source
                .get(self.core_declarator().byte_range())
                .unwrap_or_default()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        };
        let Some(first) = self.layers.first() else {
            return DeclaratorShape::Identity;
        };
        if first.kind == DerivedLayerKind::Function {
            return DeclaratorShape::Function {
                signature: signature(),
            };
        }
        if self
            .layers
            .iter()
            .any(|layer| layer.kind == DerivedLayerKind::Function)
        {
            return DeclaratorShape::FunctionPointer {
                signature: signature(),
            };
        }
        if self.layers.len() > 1 {
            return DeclaratorShape::Unsupported;
        }
        match first.kind {
            DerivedLayerKind::Pointer => DeclaratorShape::Pointer {
                qualifiers: direct_qualifiers(first.node, source),
            },
            DerivedLayerKind::Array => DeclaratorShape::Array {
                extent_text: first
                    .node
                    .child_by_field_name("size")
                    .and_then(|size| source.get(size.byte_range()))
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
            },
            DerivedLayerKind::Function => unreachable!("handled above"),
        }
    }
}

impl DecodedDeclaration<'_> {
    pub(super) fn has_failures(&self) -> bool {
        self.failures.iter().any(|failure| {
            failure.range.start <= failure.range.end
                && matches!(
                    failure.reason,
                    DeclaratorFailureReason::Unsupported
                        | DeclaratorFailureReason::Malformed
                        | DeclaratorFailureReason::BudgetExhausted
                )
        })
    }

    pub(super) fn presentation_signature(
        &self,
        entity: &DecodedDeclarator<'_>,
        source: &str,
    ) -> String {
        let prefix = source
            .get(self.common_prefix_range.clone())
            .unwrap_or_default();
        let declarator = entity.core_declarator();
        let declarator = source
            .get(declarator.start_byte()..declarator.end_byte())
            .unwrap_or(entity.name.as_str());
        let mut signature = format!("{prefix}{declarator}").trim().to_string();
        if self.has_trailing_semicolon {
            signature.push(';');
        }
        signature
    }
}

fn direct_qualifiers(node: tree_sitter::Node<'_>, source: &str) -> Vec<String> {
    let mut cursor = node.walk();
    let mut qualifiers: Vec<_> = node
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "type_qualifier")
        .filter_map(|child| source.get(child.byte_range()).map(str::to_string))
        .collect();
    qualifiers.sort();
    qualifiers.dedup();
    qualifiers
}

pub(super) fn decode_direct_declarators<'tree>(
    declaration: tree_sitter::Node<'tree>,
    source: &str,
    is_typedef: bool,
) -> DecodedDeclaration<'tree> {
    let mut cursor = declaration.walk();
    let mut direct = declaration.children_by_field_name("declarator", &mut cursor);
    let first = direct.next();
    let prefix_end = first
        .map(|node| node.start_byte())
        .unwrap_or_else(|| declaration.end_byte());
    let mut decoded = DecodedDeclaration {
        common_prefix_range: declaration.start_byte()..prefix_end,
        has_trailing_semicolon: source
            .get(declaration.byte_range())
            .is_some_and(|text| text.trim_end().ends_with(';')),
        entities: Vec::new(),
        failures: Vec::new(),
    };

    for (index, declarator) in first.into_iter().chain(direct).enumerate() {
        if index >= MAX_DIRECT_DECLARATORS {
            decoded.failures.push(DeclaratorFailure {
                range: declarator.start_byte()..declaration.end_byte(),
                reason: DeclaratorFailureReason::BudgetExhausted,
            });
            break;
        }
        match decode_declarator(declarator, source, is_typedef) {
            Ok(entity) => decoded.entities.push(entity),
            Err(failure) => decoded.failures.push(failure),
        }
    }
    decoded
}

fn decode_declarator<'tree>(
    declarator: tree_sitter::Node<'tree>,
    source: &str,
    is_typedef: bool,
) -> Result<DecodedDeclarator<'tree>, DeclaratorFailure> {
    let failure_range = declarator.start_byte()..declarator.end_byte();
    let initializer_range = (declarator.kind() == "init_declarator")
        .then(|| declarator.child_by_field_name("value"))
        .flatten()
        .map(|value| value.start_byte()..value.end_byte());
    let mut current = declarator;
    let mut outer_to_inner = Vec::new();
    let mut traversed = 0usize;

    loop {
        if current.is_error() || current.is_missing() {
            return Err(DeclaratorFailure {
                range: failure_range,
                reason: DeclaratorFailureReason::Malformed,
            });
        }
        if matches!(
            current.kind(),
            "identifier" | "field_identifier" | "type_identifier" | "primitive_type"
        ) {
            if current.kind() == "primitive_type" && !is_typedef {
                return Err(DeclaratorFailure {
                    range: failure_range,
                    reason: DeclaratorFailureReason::Unsupported,
                });
            }
            break;
        }

        traversed += 1;
        if traversed > MAX_DERIVED_LAYERS {
            return Err(DeclaratorFailure {
                range: failure_range,
                reason: DeclaratorFailureReason::BudgetExhausted,
            });
        }

        let layer = match current.kind() {
            "pointer_declarator" | "abstract_pointer_declarator" => Some(DerivedLayerKind::Pointer),
            "array_declarator" | "abstract_array_declarator" => Some(DerivedLayerKind::Array),
            "function_declarator" | "abstract_function_declarator" => {
                Some(DerivedLayerKind::Function)
            }
            "init_declarator" | "parenthesized_declarator" | "attributed_declarator" => None,
            _ => {
                return Err(DeclaratorFailure {
                    range: failure_range,
                    reason: DeclaratorFailureReason::Unsupported,
                });
            }
        };
        if let Some(kind) = layer {
            outer_to_inner.push(DerivedLayer {
                kind,
                node: current,
            });
        }
        let inner = current.child_by_field_name("declarator").or_else(|| {
            matches!(
                current.kind(),
                "parenthesized_declarator" | "attributed_declarator"
            )
            .then(|| current.named_child(0))
            .flatten()
        });
        let Some(inner) = inner else {
            return Err(DeclaratorFailure {
                range: failure_range,
                reason: if contains_malformed(current) {
                    DeclaratorFailureReason::Malformed
                } else {
                    DeclaratorFailureReason::Unsupported
                },
            });
        };
        current = inner;
    }

    outer_to_inner.reverse();
    let kind = if is_typedef {
        DeclaredEntityKind::Alias
    } else if outer_to_inner
        .first()
        .is_some_and(|layer| layer.kind == DerivedLayerKind::Function)
    {
        DeclaredEntityKind::Function
    } else {
        DeclaredEntityKind::Object
    };
    let Some(name) = source.get(current.start_byte()..current.end_byte()) else {
        return Err(DeclaratorFailure {
            range: failure_range,
            reason: DeclaratorFailureReason::Malformed,
        });
    };
    if name.is_empty() {
        return Err(DeclaratorFailure {
            range: failure_range,
            reason: DeclaratorFailureReason::Malformed,
        });
    }

    Ok(DecodedDeclarator {
        declarator,
        name_node: current,
        name: name.to_string(),
        kind,
        layers: outer_to_inner,
        initializer_range,
    })
}

fn contains_malformed(root: tree_sitter::Node<'_>) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn with_decoded(source: &str, check: impl FnOnce(DecodedDeclaration<'_>)) {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_c::LANGUAGE.into())
            .expect("C grammar");
        let tree = parser.parse(source, None).expect("C syntax tree");
        let root = tree.root_node();
        let declaration = root.named_child(0).expect("declaration");
        check(decode_direct_declarators(declaration, source, false));
    }

    #[test]
    fn direct_declarator_budget_keeps_first_256_and_reports_the_rest() {
        let names = (0..257)
            .map(|index| format!("item_{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let source = format!("int {names};");
        with_decoded(&source, |decoded| {
            assert_eq!(decoded.entities.len(), MAX_DIRECT_DECLARATORS);
            assert!(decoded.failures.iter().any(|failure| {
                failure.reason == DeclaratorFailureReason::BudgetExhausted
                    && source
                        .get(failure.range.clone())
                        .is_some_and(|text| text.contains("item_256"))
            }));
        });
    }

    #[test]
    fn derived_layer_budget_reports_the_original_declarator_range() {
        let source = format!("int {}deep;", "*".repeat(MAX_DERIVED_LAYERS + 1));
        with_decoded(&source, |decoded| {
            assert!(decoded.entities.is_empty());
            assert_eq!(decoded.failures.len(), 1);
            assert_eq!(
                decoded.failures[0].reason,
                DeclaratorFailureReason::BudgetExhausted
            );
            assert!(source
                .get(decoded.failures[0].range.clone())
                .is_some_and(|text| text.contains("deep")));
        });
    }

    #[test]
    fn layers_are_ordered_from_the_declared_name_outward() {
        let source = "int (*factory(void))(int); int *items[3], (*matrix)[3];";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_c::LANGUAGE.into())
            .expect("C grammar");
        let tree = parser.parse(source, None).expect("C syntax tree");
        let root = tree.root_node();
        let factory = decode_direct_declarators(root.named_child(0).unwrap(), source, false);
        assert_eq!(factory.entities[0].kind, DeclaredEntityKind::Function);
        assert_eq!(
            factory.entities[0]
                .layers
                .iter()
                .map(|layer| layer.kind)
                .collect::<Vec<_>>(),
            vec![
                DerivedLayerKind::Function,
                DerivedLayerKind::Pointer,
                DerivedLayerKind::Function
            ]
        );

        let objects = decode_direct_declarators(root.named_child(1).unwrap(), source, false);
        assert_eq!(
            objects.entities[0]
                .layers
                .iter()
                .map(|layer| layer.kind)
                .collect::<Vec<_>>(),
            vec![DerivedLayerKind::Array, DerivedLayerKind::Pointer]
        );
        assert_eq!(
            objects.entities[1]
                .layers
                .iter()
                .map(|layer| layer.kind)
                .collect::<Vec<_>>(),
            vec![DerivedLayerKind::Pointer, DerivedLayerKind::Array]
        );
    }
}
