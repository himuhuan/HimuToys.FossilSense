//! Compact request-only syntax evidence. No AST or workspace symbols are kept.
use crate::semantic_model::SemanticDeclarationKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookupDomain {
    Value,
    DeclarationName,
    Type,
    Tag,
    Member,
    QualifiedValue,
    QualifiedType,
    Label,
    NonCode,
    Unsupported,
}

impl LookupDomain {
    pub fn accepts(self, kind: SemanticDeclarationKind) -> bool {
        use SemanticDeclarationKind::*;
        match self {
            Self::Value | Self::DeclarationName | Self::QualifiedValue => {
                matches!(kind, Function | Method | Object | EnumConstant | Macro)
            }
            Self::Type | Self::QualifiedType => matches!(kind, Type | Alias),
            Self::Tag => kind == Type,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorSyntax {
    pub start_byte: usize,
    pub end_byte: usize,
    pub domain: LookupDomain,
    pub qualifier: Option<String>,
    pub conditional: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CursorFacts {
    pub spans: Vec<CursorSyntax>,
    pub truncated: bool,
}

impl CursorFacts {
    pub fn at(&self, byte: usize) -> Option<&CursorSyntax> {
        let end = self.spans.partition_point(|span| span.start_byte <= byte);
        self.spans[..end]
            .last()
            .filter(|span| byte <= span.end_byte)
    }
}

pub(super) fn collect(root: tree_sitter::Node<'_>, source: &str) -> CursorFacts {
    const MAX_VISITS: usize = 131_072;
    const MAX_SPANS: usize = 32_768;
    let mut facts = CursorFacts::default();
    let mut cursor = root.walk();
    let mut visits = 0usize;
    let mut conditional_ends = Vec::new();
    loop {
        visits += 1;
        if visits > MAX_VISITS || facts.spans.len() == MAX_SPANS {
            facts.truncated = true;
            break;
        }
        let node = cursor.node();
        while conditional_ends
            .last()
            .is_some_and(|end| *end <= node.start_byte())
        {
            conditional_ends.pop();
        }
        if matches!(
            node.kind(),
            "preproc_if" | "preproc_ifdef" | "preproc_elif" | "preproc_else"
        ) {
            conditional_ends.push(node.end_byte());
        }
        let non_code = matches!(
            node.kind(),
            "comment"
                | "string_literal"
                | "raw_string_literal"
                | "interpreted_string_literal"
                | "char_literal"
        );
        if non_code
            || matches!(
                node.kind(),
                "identifier"
                    | "type_identifier"
                    | "field_identifier"
                    | "statement_identifier"
                    | "namespace_identifier"
            )
        {
            let mut domain = if non_code {
                LookupDomain::NonCode
            } else {
                match node.kind() {
                    "field_identifier" => LookupDomain::Member,
                    "statement_identifier" => LookupDomain::Label,
                    "type_identifier" => LookupDomain::Type,
                    "namespace_identifier" => LookupDomain::Unsupported,
                    _ => LookupDomain::Value,
                }
            };
            let mut qualifier = None;
            if !non_code {
                if let Some(parent) = node.parent() {
                    match parent.kind() {
                        "field_expression" | "selector_expression"
                            if parent.child_by_field_name("field") == Some(node) =>
                        {
                            domain = LookupDomain::Member
                        }
                        "field_designator" | "field_declaration"
                            if node.kind() == "field_identifier" =>
                        {
                            domain = LookupDomain::Member
                        }
                        "goto_statement" | "labeled_statement"
                            if parent.child_by_field_name("label") == Some(node) =>
                        {
                            domain = LookupDomain::Label
                        }
                        "struct_specifier" | "union_specifier" | "enum_specifier"
                            if parent.child_by_field_name("name") == Some(node) =>
                        {
                            domain = LookupDomain::Tag
                        }
                        "scoped_identifier" | "scoped_type_identifier" | "qualified_identifier" => {
                            if parent.child_by_field_name("name") == Some(node) {
                                qualifier = parent
                                    .child_by_field_name("scope")
                                    .and_then(|scope| source.get(scope.byte_range()))
                                    .filter(|scope| scope.len() <= 512)
                                    .map(str::to_owned);
                                domain = if qualifier.is_none() {
                                    LookupDomain::Unsupported
                                } else if node.kind() == "type_identifier" {
                                    LookupDomain::QualifiedType
                                } else {
                                    LookupDomain::QualifiedValue
                                };
                            } else {
                                domain = LookupDomain::Unsupported;
                            }
                        }
                        "qualified_type" if parent.child_by_field_name("name") == Some(node) => {
                            qualifier = parent
                                .child_by_field_name("package")
                                .and_then(|package| source.get(package.byte_range()))
                                .filter(|package| package.len() <= 512)
                                .map(str::to_owned);
                            domain = if qualifier.is_some() {
                                LookupDomain::QualifiedType
                            } else {
                                LookupDomain::Unsupported
                            };
                        }
                        "init_declarator" | "function_declarator" | "parameter_declaration"
                            if parent.child_by_field_name("declarator") == Some(node)
                                && domain == LookupDomain::Value =>
                        {
                            domain = LookupDomain::DeclarationName
                        }
                        _ => {}
                    }
                    if parent.is_error() {
                        domain = LookupDomain::Unsupported;
                    }
                }
            }
            facts.spans.push(CursorSyntax {
                start_byte: node.start_byte(),
                end_byte: node.end_byte(),
                domain,
                qualifier,
                conditional: !conditional_ends.is_empty(),
            });
        }
        if !non_code && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return facts;
            }
        }
    }
    facts
}
