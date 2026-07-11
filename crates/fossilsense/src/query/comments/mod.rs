//! Best-effort comment extraction, parsing, and Markdown rendering for editor popups.
//!
//! Pipeline:
//! `source + CommentAnchor -> RawComment -> CommentDocument -> RenderedComment`
//!
//! This module is protocol-neutral and must not depend on `tower-lsp`, SQLite,
//! or server/store types.

mod extract;
mod markdown;
mod model;
mod parse;

#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub use model::{
    CommentAnchor, CommentBlock, CommentDiagnostics, CommentDocument, CommentPlacement,
    CommentRenderOptions, CommentStyle, RawComment, RenderedComment, RenderedParameterComment,
    RenderedSymbolComment, TagAttribute, TagBlock, TagSyntax, TextBlock,
};

/// Recover and render the most trustworthy nearby comment for a symbol.
#[cfg(test)]
pub fn comment_markdown_for_symbol(
    source: &str,
    anchor: &CommentAnchor,
    options: &CommentRenderOptions,
) -> Option<RenderedComment> {
    let rendered = comment_documentation_for_symbol(source, anchor, options)?;
    Some(RenderedComment {
        markdown: rendered.markdown,
        diagnostics: rendered.diagnostics,
    })
}

/// Recover documentation for a symbol and retain parameter descriptions for
/// Signature Help's active-parameter popup.
pub fn comment_documentation_for_symbol(
    source: &str,
    anchor: &CommentAnchor,
    options: &CommentRenderOptions,
) -> Option<RenderedSymbolComment> {
    let raw = extract::extract_comment(source, anchor, options)?;
    render_raw_symbol_comment(&raw, options)
}

/// Compatibility fallback when only a stored signature still carries a leading comment.
pub fn comment_markdown_from_signature(
    signature: &str,
    options: &CommentRenderOptions,
) -> Option<RenderedComment> {
    let raw = extract::extract_signature_fallback(signature)?;
    render_raw_comment(&raw, options)
}

fn render_raw_comment(raw: &RawComment, options: &CommentRenderOptions) -> Option<RenderedComment> {
    let document = parse::parse_raw_comment(raw, options);
    if !parse::is_attachable_document(&document) {
        return None;
    }
    markdown::render_document(&document, options)
}

fn render_raw_symbol_comment(
    raw: &RawComment,
    options: &CommentRenderOptions,
) -> Option<RenderedSymbolComment> {
    let document = parse::parse_raw_comment(raw, options);
    if !parse::is_attachable_document(&document) {
        return None;
    }
    let rendered = markdown::render_document(&document, options)?;
    let parameters = document
        .blocks
        .iter()
        .filter_map(|block| {
            let CommentBlock::Tag(tag) = block else {
                return None;
            };
            if tag.canonical_name != "param" {
                return None;
            }
            let name = tag
                .attributes
                .iter()
                .find(|attribute| attribute.name == "name")?
                .value
                .clone();
            let parameter_document = CommentDocument {
                blocks: vec![CommentBlock::Text(TextBlock {
                    lines: tag.lines.clone(),
                })],
                diagnostics: CommentDiagnostics::default(),
            };
            let parameter = markdown::render_document(&parameter_document, options)?;
            Some(RenderedParameterComment {
                name,
                markdown: parameter.markdown,
            })
        })
        .collect();
    Some(RenderedSymbolComment {
        markdown: rendered.markdown,
        parameters,
        diagnostics: rendered.diagnostics,
    })
}
