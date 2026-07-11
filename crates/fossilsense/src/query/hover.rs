use crate::model::DefinitionCandidate;
use crate::reachability::ReachScope;
use crate::store::SymbolRecord;

#[cfg(test)]
use super::comments::{comment_markdown_for_symbol, CommentAnchor};
use super::comments::{comment_markdown_from_signature, CommentRenderOptions};
use super::definitions::rank_definition_records_with_scope;

pub const HOVER_CANDIDATE_LIMIT: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankedHoverCandidate {
    pub candidate: DefinitionCandidate,
    pub signature: String,
    pub guard: Option<String>,
}

pub fn rank_hover_candidates(
    records: Vec<SymbolRecord>,
    current_rel_path: &str,
    scope: Option<&ReachScope>,
    limit: usize,
) -> Vec<RankedHoverCandidate> {
    rank_definition_records_with_scope(records, current_rel_path, scope)
        .into_iter()
        .take(limit)
        .map(|ranked| RankedHoverCandidate {
            signature: ranked.record.signature,
            guard: ranked.record.guard,
            candidate: ranked.candidate,
        })
        .collect()
}

/// Recover comment Markdown near a symbol using the shared comments pipeline.
#[cfg(test)]
pub fn comment_markdown_for_hover_symbol(
    source: &str,
    symbol_name: &str,
    symbol_start_line: u32,
    range: &crate::model::CandidateRange,
) -> Option<String> {
    let rendered = comment_markdown_for_symbol(
        source,
        &CommentAnchor {
            symbol_name: symbol_name.to_string(),
            start_line: symbol_start_line,
            start_col: range.start_col,
            end_line: range.end_line,
            end_col: range.end_col,
        },
        &CommentRenderOptions::default(),
    )?;
    Some(rendered.markdown)
}

pub fn comment_documentation_for_candidate_symbol(
    source: &str,
    symbol_name: &str,
    line: u32,
    range: &crate::model::CandidateRange,
) -> Option<super::comments::RenderedSymbolComment> {
    super::comments::comment_documentation_for_symbol(
        source,
        &super::comments::CommentAnchor {
            symbol_name: symbol_name.to_string(),
            start_line: line,
            start_col: range.start_col,
            end_line: range.end_line,
            end_col: range.end_col,
        },
        &super::comments::CommentRenderOptions::default(),
    )
}

pub fn hover_markdown_for_candidate(
    ranked: &RankedHoverCandidate,
    comment_markdown: Option<&str>,
) -> String {
    let mut out = String::new();
    let signature_comment =
        comment_markdown_from_signature(&ranked.signature, &CommentRenderOptions::default())
            .map(|rendered| rendered.markdown);
    let comment_markdown = comment_markdown.or(signature_comment.as_deref());

    if let Some(comment) = comment_markdown.filter(|s| !s.trim().is_empty()) {
        out.push_str(comment.trim_end());
        out.push_str("\n\n");
    }

    out.push_str("```c\n");
    let guard = ranked
        .guard
        .as_deref()
        .filter(|guard| !guard.trim().is_empty())
        .filter(|guard| should_render_guard_wrapper(guard));
    if let Some(guard) = guard {
        out.push_str(&sanitize_markdown(guard.trim()));
        out.push('\n');
    }
    out.push_str(&format!("// In {}\n", ranked.candidate.path));
    out.push_str(&sanitize_markdown(&display_signature(
        &ranked.signature,
        &ranked.candidate.name,
    )));
    out.push('\n');
    if let Some(guard) = guard {
        out.push_str("#endif // ^^ ");
        out.push_str(&sanitize_markdown(&guard_closing_label(guard)));
        out.push_str(" ^^\n");
    }
    out.push_str("```\n\n");

    out.push_str(&format!(
        "<small><span style=\"color: var(--vscode-descriptionForeground);\"><em>tier: {} | confidence: {} | reason: {}</em></span></small>",
        ranked.candidate.tier.as_str(),
        ranked.candidate.confidence.as_str(),
        ranked.candidate.reason.as_str()
    ));
    out
}

fn display_signature(signature: &str, fallback_name: &str) -> String {
    let stripped = strip_leading_signature_comments(signature).trim();
    if stripped.is_empty() {
        fallback_name.to_string()
    } else {
        stripped.to_string()
    }
}

fn strip_leading_signature_comments(signature: &str) -> &str {
    let mut rest = signature.trim_start();
    loop {
        if rest.starts_with("/*") {
            let Some(end) = rest.find("*/") else {
                return "";
            };
            rest = rest[end + 2..].trim_start();
            continue;
        }
        if rest.starts_with("//") {
            let Some(end) = rest.find('\n') else {
                return "";
            };
            rest = rest[end + 1..].trim_start();
            continue;
        }
        return rest;
    }
}

fn should_render_guard_wrapper(guard: &str) -> bool {
    !is_header_guard_label(&guard_closing_label(guard))
}

fn is_header_guard_label(label: &str) -> bool {
    let label = label.trim();
    label.ends_with("_H")
}

fn guard_closing_label(guard: &str) -> String {
    let trimmed = guard.trim();
    for prefix in ["#ifndef", "#ifdef", "#define"] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            if let Some(token) = rest.split_whitespace().next() {
                return clean_guard_token(token);
            }
        }
    }
    if let Some(token) = defined_guard_token(trimmed) {
        return token;
    }
    trimmed.to_string()
}

fn defined_guard_token(value: &str) -> Option<String> {
    let index = value.find("defined")?;
    let after = value[index + "defined".len()..].trim_start();
    if let Some(rest) = after.strip_prefix('(') {
        let end = rest.find(')')?;
        return Some(clean_guard_token(&rest[..end]));
    }
    after.split_whitespace().next().map(clean_guard_token)
}

fn clean_guard_token(token: &str) -> String {
    token
        .trim()
        .trim_matches(|c: char| c == '(' || c == ')' || c == '!')
        .to_string()
}

fn sanitize_markdown(value: &str) -> String {
    value.replace("```", "'''")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbol_record(
        name: &str,
        kind: &str,
        role: &str,
        path: &str,
        line: u32,
        signature: &str,
    ) -> SymbolRecord {
        SymbolRecord {
            id: 0,
            name: name.to_string(),
            kind: kind.to_string(),
            role: role.to_string(),
            path: path.to_string(),
            start_line: line,
            start_col: 0,
            end_line: line,
            end_col: 0,
            signature: signature.to_string(),
            guard: None,
            source: "workspace".to_string(),
            directly_included: false,
        }
    }

    #[test]
    fn hover_candidates_preserve_signatures_and_scope_order() {
        let records = vec![
            symbol_record(
                "foo",
                "function",
                "definition",
                "other/foo.c",
                20,
                "int foo(float x)",
            ),
            symbol_record(
                "foo",
                "macro",
                "definition",
                "src/main.c",
                2,
                "#define foo(x) (x)",
            ),
            symbol_record(
                "foo",
                "function",
                "declaration",
                "inc/foo.h",
                7,
                "int foo(int x);",
            ),
        ];
        let reach = ReachScope {
            files: ["src/main.c".to_string(), "inc/foo.h".to_string()]
                .into_iter()
                .collect(),
            open: false,
            reason: None,
        };
        let ranked = rank_hover_candidates(records, "src/main.c", Some(&reach), 4);
        assert_eq!(ranked[0].candidate.path, "src/main.c");
        assert_eq!(ranked[0].signature, "#define foo(x) (x)");
        assert_eq!(ranked[1].candidate.path, "inc/foo.h");
        assert_eq!(ranked[1].signature, "int foo(int x);");
    }

    #[test]
    fn hover_candidates_cap_after_ranking() {
        let records = vec![
            symbol_record("foo", "function", "definition", "b.c", 0, "int foo(int b)"),
            symbol_record("foo", "function", "definition", "a.c", 0, "int foo(int a)"),
        ];
        let ranked = rank_hover_candidates(records, "main.c", None, 1);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].candidate.path, "a.c");
    }

    #[test]
    fn doxygen_comment_renders_structured_sections() {
        let source = "/**\n * @brief Adds two values.\n * @param lhs left side\n * @param rhs right side\n * @return the sum\n */\nint add(int lhs, int rhs);\n";
        let markdown = comment_markdown_for_hover_symbol(
            source,
            "add",
            6,
            &crate::model::CandidateRange {
                start_line: 6,
                start_col: 0,
                end_line: 6,
                end_col: 3,
            },
        )
        .expect("comment");
        assert!(markdown.contains("### Brief"));
        assert!(markdown.contains("Adds two values."));
        assert!(markdown.contains("### Parameters"));
        assert!(markdown.contains("- `lhs` — left side"));
        assert!(markdown.contains("- `rhs` — right side"));
        assert!(markdown.contains("### Returns"));
        assert!(markdown.contains("the sum"));
        assert!(!markdown.contains("*/"));
    }

    #[test]
    fn ordinary_line_comments_render_as_prose() {
        let source = "// Initializes the driver.\n// Safe to call twice.\nvoid init(void);\n";
        let markdown = comment_markdown_for_hover_symbol(
            source,
            "init",
            2,
            &crate::model::CandidateRange {
                start_line: 2,
                start_col: 0,
                end_line: 2,
                end_col: 4,
            },
        )
        .expect("comment");
        assert!(markdown.contains("Initializes the driver.  \nSafe to call twice."));
    }

    #[test]
    fn trailing_comments_attach_for_common_forms() {
        for source in [
            "size_t db_size; // cache size in database\n",
            "size_t db_size; /* cache size in database */\n",
            "size_t db_size; /** cache size in database */\n",
        ] {
            let markdown = comment_markdown_for_hover_symbol(
                source,
                "db_size",
                0,
                &crate::model::CandidateRange {
                    start_line: 0,
                    start_col: 0,
                    end_line: 0,
                    end_col: 7,
                },
            )
            .expect("comment");
            assert!(
                markdown.contains("cache size in database"),
                "missing prose in {markdown:?} for {source:?}"
            );
            assert!(!markdown.contains('*') || !markdown.trim_start().starts_with('*'));
        }
    }

    #[test]
    fn messy_comment_degrades_to_readable_text() {
        let source =
            "/* ***\n * @weird custom tag is kept readable\n * warning without marker\n */\nint odd(void);\n";
        let markdown = comment_markdown_for_hover_symbol(
            source,
            "odd",
            4,
            &crate::model::CandidateRange {
                start_line: 4,
                start_col: 0,
                end_line: 4,
                end_col: 3,
            },
        )
        .expect("comment");
        assert!(markdown.contains("### Weird") || markdown.contains("custom tag is kept readable"));
        assert!(markdown.contains("custom tag is kept readable"));
        assert!(markdown.contains("warning without marker"));
        assert!(!markdown.contains("/*"));
    }

    #[test]
    fn file_header_comment_does_not_attach_to_first_symbol() {
        let source = "/*\n * Copyright 2026 Example Corp.\n * Project: boot firmware\n * License: internal use only\n */\nint first_symbol(void);\n";
        assert!(comment_markdown_for_hover_symbol(
            source,
            "first_symbol",
            5,
            &crate::model::CandidateRange {
                start_line: 5,
                start_col: 0,
                end_line: 5,
                end_col: 12,
            },
        )
        .is_none());
    }

    #[test]
    fn doxygen_file_comment_does_not_attach_to_first_symbol() {
        let source =
            "/**\n * @file driver.h\n * @brief Shared driver declarations.\n */\nint first_symbol(void);\n";
        assert!(comment_markdown_for_hover_symbol(
            source,
            "first_symbol",
            4,
            &crate::model::CandidateRange {
                start_line: 4,
                start_col: 0,
                end_line: 4,
                end_col: 12,
            },
        )
        .is_none());
    }

    #[test]
    fn blank_line_between_comment_and_symbol_blocks_attachment() {
        let source = "// Docs for previous thing\n\nint current;\n";
        assert!(comment_markdown_for_hover_symbol(
            source,
            "current",
            2,
            &crate::model::CandidateRange {
                start_line: 2,
                start_col: 0,
                end_line: 2,
                end_col: 7,
            },
        )
        .is_none());
    }

    #[test]
    fn trailing_inline_block_comment_does_not_attach_to_next_symbol() {
        let source = "int old; /* note for old */\nint current;\n";
        assert!(comment_markdown_for_hover_symbol(
            source,
            "current",
            1,
            &crate::model::CandidateRange {
                start_line: 1,
                start_col: 0,
                end_line: 1,
                end_col: 7,
            },
        )
        .is_none());
    }

    #[test]
    fn block_comment_with_internal_blank_line_still_attaches() {
        let source = "/**\n * First paragraph.\n\n * Second paragraph.\n */\nint current;\n";
        let markdown = comment_markdown_for_hover_symbol(
            source,
            "current",
            5,
            &crate::model::CandidateRange {
                start_line: 5,
                start_col: 0,
                end_line: 5,
                end_col: 7,
            },
        )
        .expect("comment");
        assert!(markdown.contains("First paragraph."));
        assert!(markdown.contains("Second paragraph."));
    }

    #[test]
    fn inline_leading_block_comment_on_symbol_line_still_attaches() {
        let source = "/** Test to see if a format is supported. */ bool test_fmt(void);\n";
        let markdown = comment_markdown_for_hover_symbol(
            source,
            "test_fmt",
            0,
            &crate::model::CandidateRange {
                start_line: 0,
                start_col: 0,
                end_line: 0,
                end_col: 8,
            },
        )
        .expect("comment");
        assert!(markdown.contains("Test to see if a format is supported."));
    }

    #[test]
    fn closing_block_comment_on_symbol_line_still_attaches() {
        let source = "/**\n * Test to see if a format is supported.\n */ bool test_fmt(void);\n";
        let markdown = comment_markdown_for_hover_symbol(
            source,
            "test_fmt",
            2,
            &crate::model::CandidateRange {
                start_line: 2,
                start_col: 0,
                end_line: 2,
                end_col: 8,
            },
        )
        .expect("comment");
        assert!(markdown.contains("Test to see if a format is supported."));
    }

    #[test]
    fn doxygen_param_direction_renders_as_parameter_list() {
        let source = "/**\n * @brief Copies bytes.\n * @param[in] src source bytes\n * @param[out] dst destination bytes\n */\nvoid copy(void *dst, const void *src);\n";
        let markdown = comment_markdown_for_hover_symbol(
            source,
            "copy",
            5,
            &crate::model::CandidateRange {
                start_line: 5,
                start_col: 0,
                end_line: 5,
                end_col: 4,
            },
        )
        .expect("comment");
        assert!(markdown.contains("### Brief") || markdown.contains("Copies bytes."));
        assert!(markdown.contains("### Parameters"));
        assert!(markdown.contains("- `src` *(in)* — source bytes"));
        assert!(markdown.contains("- `dst` *(out)* — destination bytes"));
    }

    #[test]
    fn xml_summary_and_unknown_tags_use_fallback_headings() {
        let source = "/// <summary>\n/// cache size in database\n/// </summary>\nsize_t db_size;\n";
        let markdown = comment_markdown_for_hover_symbol(
            source,
            "db_size",
            3,
            &crate::model::CandidateRange {
                start_line: 3,
                start_col: 0,
                end_line: 3,
                end_col: 7,
            },
        )
        .expect("comment");
        assert!(markdown.contains("### Summary"));
        assert!(markdown.contains("cache size in database"));
    }

    #[test]
    fn code_line_between_comment_and_symbol_blocks_attachment() {
        let source = "// Docs for old thing\nint old;\nint current;\n";
        assert!(comment_markdown_for_hover_symbol(
            source,
            "current",
            2,
            &crate::model::CandidateRange {
                start_line: 2,
                start_col: 0,
                end_line: 2,
                end_col: 7,
            },
        )
        .is_none());
    }

    #[test]
    fn hover_markdown_uses_signature_and_hides_source_ranges() {
        let ranked = rank_hover_candidates(
            vec![symbol_record(
                "foo",
                "function",
                "definition",
                "src/foo.c",
                42,
                "int foo(int x)",
            )],
            "src/main.c",
            None,
            1,
        )
        .remove(0);
        let markdown = hover_markdown_for_candidate(&ranked, Some("Does work."));
        assert!(markdown.contains("```c\n// In src/foo.c\nint foo(int x)\n```"));
        assert!(markdown.contains("Does work."));
        assert!(markdown.contains("tier: global"));
        assert!(!markdown.contains(":43"));
        assert!(!markdown.contains("start_line"));
    }

    #[test]
    fn hover_markdown_renders_quiet_source_header_and_metadata() {
        let ranked = rank_hover_candidates(
            vec![symbol_record(
                "ff_sws_lut3d_test_fmt",
                "function",
                "definition",
                "libswscale/lut3d.c",
                12,
                "bool ff_sws_lut3d_test_fmt(enum AVPixelFormat fmt, int output)",
            )],
            "libswscale/lut3d.c",
            None,
            1,
        )
        .remove(0);

        let markdown = hover_markdown_for_candidate(&ranked, None);

        assert!(markdown.contains(
            "```c\n// In libswscale/lut3d.c\nbool ff_sws_lut3d_test_fmt(enum AVPixelFormat fmt, int output)\n```"
        ));
        assert!(markdown.contains(
            "<small><span style=\"color: var(--vscode-descriptionForeground);\"><em>tier: current | confidence: exact | reason: current_file</em></span></small>"
        ));
        assert!(!markdown.contains("FossilSense candidate"));
        assert!(!markdown.contains("function definition in"));
    }

    #[test]
    fn hover_markdown_splits_signature_comment_and_omits_header_guard_wrapper() {
        let mut ranked = rank_hover_candidates(
            vec![symbol_record(
                "ff_sws_lut3d_test_fmt",
                "function",
                "declaration",
                "libswscale/lut3d.h",
                5,
                "/** * Test to see if a given format is supported by the 3DLUT input/output code. */ bool ff_sws_lut3d_test_fmt(enum AVPixelFormat fmt, int output);",
            )],
            "libswscale/lut3d.c",
            None,
            1,
        )
        .remove(0);
        ranked.guard = Some("#ifndef SWSCALE_LUT3D_H".to_string());

        let markdown = hover_markdown_for_candidate(&ranked, None);

        assert!(markdown.contains(
            "Test to see if a given format is supported by the 3DLUT input/output code."
        ));
        assert!(markdown.contains(
            "```c\n// In libswscale/lut3d.h\nbool ff_sws_lut3d_test_fmt(enum AVPixelFormat fmt, int output);\n```"
        ));
        assert!(!markdown.contains("#ifndef SWSCALE_LUT3D_H"));
        assert!(!markdown.contains("#endif // ^^ SWSCALE_LUT3D_H ^^"));
        assert!(!markdown.contains("/**"));
        assert!(!markdown.contains("*/ bool"));
        assert!(!markdown.contains("guard:"));
    }

    #[test]
    fn hover_markdown_keeps_non_header_guard_wrapper() {
        let mut ranked = rank_hover_candidates(
            vec![symbol_record(
                "platform_init",
                "function",
                "declaration",
                "include/platform.h",
                8,
                "void platform_init(void);",
            )],
            "src/main.c",
            None,
            1,
        )
        .remove(0);
        ranked.guard = Some("#ifdef CONFIG_PLATFORM_INIT".to_string());

        let markdown = hover_markdown_for_candidate(&ranked, None);

        assert!(markdown.contains(
            "```c\n#ifdef CONFIG_PLATFORM_INIT\n// In include/platform.h\nvoid platform_init(void);\n#endif // ^^ CONFIG_PLATFORM_INIT ^^\n```"
        ));
    }
}
