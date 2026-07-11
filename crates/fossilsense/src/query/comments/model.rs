//! Protocol-neutral comment models for source documentation recovery.
//!
//! These types describe best-effort comment attachment and structure. They are
//! not compiler-bound documentation bindings.

/// Source anchor for the symbol whose nearby comment should be recovered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentAnchor {
    pub symbol_name: String,
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

/// Where a recovered comment sat relative to the symbol declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentPlacement {
    TrailingSameLine,
    InlineLeadingSameLine,
    LeadingAbove,
    SignatureFallback,
}

/// Surface syntax of the recovered comment markers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentStyle {
    Line,
    DocLine,
    Block,
    DocBlock,
}

/// Raw comment text including original markers, before cleaning/parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawComment {
    pub text: String,
    pub placement: CommentPlacement,
    pub style: CommentStyle,
    pub start_line: u32,
    pub end_line: u32,
    /// Whether extraction stopped at the configured line budget.
    pub truncated: bool,
}

/// Structured comment after marker cleaning and tolerant tag parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentDocument {
    pub blocks: Vec<CommentBlock>,
    pub diagnostics: CommentDiagnostics,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentBlock {
    Text(TextBlock),
    Tag(TagBlock),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextBlock {
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagBlock {
    pub canonical_name: String,
    pub raw_name: String,
    pub syntax: TagSyntax,
    pub attributes: Vec<TagAttribute>,
    pub lines: Vec<String>,
    pub raw: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagSyntax {
    DoxygenAt,
    DoxygenBackslash,
    DoxygenSlash,
    Xml,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagAttribute {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommentDiagnostics {
    pub malformed_fallback: bool,
    pub truncated: bool,
    pub unclosed_xml: bool,
}

/// Budgets for request-time comment recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommentRenderOptions {
    pub max_comment_lines: usize,
    pub max_chars: usize,
}

impl Default for CommentRenderOptions {
    fn default() -> Self {
        Self {
            max_comment_lines: 48,
            max_chars: 2_000,
        }
    }
}

/// Rendered Hover comment Markdown plus recoverable diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedComment {
    pub markdown: String,
    pub diagnostics: CommentDiagnostics,
}

/// Rendered symbol documentation plus descriptions addressable by parameter name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedSymbolComment {
    pub markdown: String,
    pub parameters: Vec<RenderedParameterComment>,
    pub diagnostics: CommentDiagnostics,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedParameterComment {
    pub name: String,
    pub markdown: String,
}
