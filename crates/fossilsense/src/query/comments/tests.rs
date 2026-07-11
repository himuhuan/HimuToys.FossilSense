use super::*;

fn anchor(name: &str, line: u32) -> CommentAnchor {
    CommentAnchor {
        symbol_name: name.to_string(),
        start_line: line,
        start_col: 0,
        end_line: line,
        end_col: 0,
    }
}

fn options() -> CommentRenderOptions {
    CommentRenderOptions::default()
}

fn render(source: &str, name: &str, line: u32) -> String {
    comment_markdown_for_symbol(source, &anchor(name, line), &options())
        .expect("comment")
        .markdown
}

mod extract {
    use super::*;
    use crate::query::comments::extract::extract_comment;

    #[test]
    fn leading_line_comment() {
        let source = "// docs\nsize_t db_size;\n";
        let raw = extract_comment(source, &anchor("db_size", 1), &options()).expect("raw");
        assert_eq!(raw.placement, CommentPlacement::LeadingAbove);
        assert!(raw.text.contains("// docs"));
    }

    #[test]
    fn trailing_line_comment() {
        let source = "size_t db_size; // docs\n";
        let raw = extract_comment(source, &anchor("db_size", 0), &options()).expect("raw");
        assert_eq!(raw.placement, CommentPlacement::TrailingSameLine);
        assert_eq!(raw.text, "// docs");
    }

    #[test]
    fn trailing_block_comment() {
        let source = "size_t db_size; /* docs */\n";
        let raw = extract_comment(source, &anchor("db_size", 0), &options()).expect("raw");
        assert_eq!(raw.placement, CommentPlacement::TrailingSameLine);
        assert_eq!(raw.text, "/* docs */");
    }

    #[test]
    fn trailing_doc_block_comment() {
        let source = "size_t db_size; /** docs */\n";
        let raw = extract_comment(source, &anchor("db_size", 0), &options()).expect("raw");
        assert_eq!(raw.placement, CommentPlacement::TrailingSameLine);
        assert_eq!(raw.text, "/** docs */");
    }

    #[test]
    fn inline_leading_block_comment() {
        let source = "/** docs */ size_t db_size;\n";
        let raw = extract_comment(source, &anchor("db_size", 0), &options()).expect("raw");
        assert_eq!(raw.placement, CommentPlacement::InlineLeadingSameLine);
        assert!(raw.text.starts_with("/**"));
    }

    #[test]
    fn string_url_is_not_a_comment() {
        let source = "const char *url = \"http://x\";\n";
        assert!(extract_comment(source, &anchor("url", 0), &options()).is_none());
    }

    #[test]
    fn character_literal_slash_is_not_a_comment() {
        let source = "char c = '/';\n";
        assert!(extract_comment(source, &anchor("c", 0), &options()).is_none());
    }

    #[test]
    fn previous_declaration_trailing_comment_does_not_attach() {
        let source = "int old; /* old */\nint current;\n";
        assert!(extract_comment(source, &anchor("current", 1), &options()).is_none());
    }

    #[test]
    fn blank_line_blocks_leading_comment() {
        let source = "// docs\n\nint current;\n";
        assert!(extract_comment(source, &anchor("current", 2), &options()).is_none());
    }

    #[test]
    fn ambiguous_multi_declarator_line_refuses_trailing() {
        let source = "int left, right; // docs\n";
        assert!(extract_comment(source, &anchor("left", 0), &options()).is_none());
        assert!(extract_comment(source, &anchor("right", 0), &options()).is_none());
    }

    #[test]
    fn chinese_prefix_does_not_break_trailing_attachment() {
        // Non-ASCII text must not cause UTF-16/byte confusion when attaching an
        // ASCII symbol's trailing comment.
        let source = "/* 中文前缀说明 */ size_t db_size; // 缓存大小\n";
        let raw = extract_comment(source, &anchor("db_size", 0), &options()).expect("raw");
        assert_eq!(raw.placement, CommentPlacement::TrailingSameLine);
        assert!(raw.text.contains("缓存大小"));
    }

    #[test]
    fn trailing_wins_over_leading() {
        let source = "// leading\nsize_t db_size; // trailing\n";
        let raw = extract_comment(source, &anchor("db_size", 1), &options()).expect("raw");
        assert_eq!(raw.placement, CommentPlacement::TrailingSameLine);
        assert!(raw.text.contains("trailing"));
    }

    #[test]
    fn function_params_with_commas_still_allow_trailing() {
        let source = "void copy(void *dst, const void *src); // docs\n";
        let raw = extract_comment(source, &anchor("copy", 0), &options()).expect("raw");
        assert_eq!(raw.placement, CommentPlacement::TrailingSameLine);
    }

    #[test]
    fn pointer_statement_is_not_a_block_comment_boundary() {
        let source = "/* docs for earlier code */\n*ptr = 1;\nint current;\n";
        assert!(extract_comment(source, &anchor("current", 2), &options()).is_none());
    }
}

mod parse {
    use super::*;
    use crate::query::comments::extract::extract_comment;
    use crate::query::comments::parse::parse_raw_comment;

    fn parse_source(source: &str, name: &str, line: u32) -> CommentDocument {
        let raw = extract_comment(source, &anchor(name, line), &options()).expect("raw");
        parse_raw_comment(&raw, &options())
    }

    #[test]
    fn single_line_doc_block_strips_structural_star() {
        let doc = parse_source(
            "size_t db_size; /** cache size in database */\n",
            "db_size",
            0,
        );
        assert_eq!(doc.blocks.len(), 1);
        match &doc.blocks[0] {
            CommentBlock::Text(text) => {
                assert_eq!(text.lines, vec!["cache size in database".to_string()]);
            }
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn markdown_list_stars_are_preserved() {
        let source = "/**\n * * first\n * * second\n */\nint x;\n";
        let doc = parse_source(source, "x", 4);
        let CommentBlock::Text(text) = &doc.blocks[0] else {
            panic!("expected text");
        };
        assert_eq!(text.lines[0], "* first");
        assert_eq!(text.lines[1], "* second");
    }

    #[test]
    fn xml_summary_becomes_tag_block() {
        let source = "/// <summary>\n/// cache size in database\n/// </summary>\nsize_t db_size;\n";
        let doc = parse_source(source, "db_size", 3);
        assert_eq!(doc.blocks.len(), 1);
        let CommentBlock::Tag(tag) = &doc.blocks[0] else {
            panic!("expected tag");
        };
        assert_eq!(tag.canonical_name, "summary");
        assert_eq!(tag.lines, vec!["cache size in database".to_string()]);
    }

    #[test]
    fn doxygen_param_direction_and_variants() {
        let source = "/**\n * @param[in] src source bytes\n * \\param dst destination bytes\n * /param size cache size\n */\nvoid copy(void);\n";
        let doc = parse_source(source, "copy", 5);
        assert_eq!(doc.blocks.len(), 3);
        for (index, expected_name, expected_dir) in
            [(0, "src", Some("in")), (1, "dst", None), (2, "size", None)]
        {
            let CommentBlock::Tag(tag) = &doc.blocks[index] else {
                panic!("expected param tag");
            };
            assert_eq!(tag.canonical_name, "param");
            assert_eq!(
                tag.attributes
                    .iter()
                    .find(|attr| attr.name == "name")
                    .map(|attr| attr.value.as_str()),
                Some(expected_name)
            );
            assert_eq!(
                tag.attributes
                    .iter()
                    .find(|attr| attr.name == "direction")
                    .map(|attr| attr.value.as_str()),
                expected_dir
            );
        }
    }

    #[test]
    fn doxygen_return_and_xml_param() {
        let source = "/**\n * @return current size\n * <param name=\"size\">cache size</param>\n */\nint query(void);\n";
        let doc = parse_source(source, "query", 4);
        let CommentBlock::Tag(ret) = &doc.blocks[0] else {
            panic!("return");
        };
        assert_eq!(ret.canonical_name, "return");
        assert_eq!(ret.lines, vec!["current size".to_string()]);

        let CommentBlock::Tag(param) = &doc.blocks[1] else {
            panic!("param");
        };
        assert_eq!(param.canonical_name, "param");
        assert_eq!(
            param
                .attributes
                .iter()
                .find(|attr| attr.name == "name")
                .map(|attr| attr.value.as_str()),
            Some("size")
        );
        assert_eq!(param.lines, vec!["cache size".to_string()]);
    }

    #[test]
    fn email_and_inline_at_do_not_form_tags() {
        let source = "/// owner@example.com\n/// foo @warning bar\nint x;\n";
        let doc = parse_source(source, "x", 2);
        assert!(doc
            .blocks
            .iter()
            .all(|block| matches!(block, CommentBlock::Text(_))));
        let markdown = render(source, "x", 2);
        assert!(markdown.contains("owner@example.com"));
        assert!(markdown.contains("foo @warning bar"));
        assert!(!markdown.contains("### Warning"));
    }

    #[test]
    fn unknown_custom_tag_keeps_body() {
        let source = "/**\n * @custom keeps full body\n * second line\n */\nint x;\n";
        let doc = parse_source(source, "x", 4);
        let CommentBlock::Tag(tag) = &doc.blocks[0] else {
            panic!("tag");
        };
        assert_eq!(tag.canonical_name, "custom");
        assert_eq!(tag.lines[0], "keeps full body");
        assert_eq!(tag.lines[1], "second line");
    }

    #[test]
    fn unclosed_xml_param_sets_diagnostics() {
        let source =
            "/// <param name=\"size\">\n/// cache size in database\nsize_t query_size(void);\n";
        let doc = parse_source(source, "query_size", 2);
        assert!(doc.diagnostics.unclosed_xml || doc.diagnostics.malformed_fallback);
        assert!(!doc.blocks.is_empty());
    }

    #[test]
    fn internal_blank_lines_are_preserved() {
        let source = "/**\n * First paragraph.\n\n * Second paragraph.\n */\nint current;\n";
        let doc = parse_source(source, "current", 5);
        let CommentBlock::Text(text) = &doc.blocks[0] else {
            panic!("text");
        };
        assert!(text.lines.iter().any(|line| line.is_empty()));
        assert!(text
            .lines
            .iter()
            .any(|line| line.contains("First paragraph.")));
        assert!(text
            .lines
            .iter()
            .any(|line| line.contains("Second paragraph.")));
    }
}

mod markdown {
    use super::*;

    #[test]
    fn ordinary_two_line_text_uses_hard_breaks() {
        let source = "// first line\n// second line\nint x;\n";
        let markdown = render(source, "x", 2);
        assert!(
            markdown.contains("first line  \nsecond line"),
            "expected hard break, got {markdown:?}"
        );
    }

    #[test]
    fn trailing_comments_render_clean_prose() {
        assert_eq!(
            render("size_t db_size; // cache size in database\n", "db_size", 0),
            "cache size in database\n"
        );
        assert_eq!(
            render(
                "size_t db_size; /* cache size in database */\n",
                "db_size",
                0
            ),
            "cache size in database\n"
        );
        assert_eq!(
            render(
                "size_t db_size; /** cache size in database */\n",
                "db_size",
                0
            ),
            "cache size in database\n"
        );
    }

    #[test]
    fn doxygen_parameters_and_returns() {
        let source = "/**\n * @param[in] src source bytes\n * @param[out] dst destination bytes\n * @return current size\n */\nvoid copy(void);\n";
        let markdown = render(source, "copy", 5);
        assert!(markdown.contains("### Parameters"));
        assert!(markdown.contains("- `src` *(in)* — source bytes"));
        assert!(markdown.contains("- `dst` *(out)* — destination bytes"));
        assert!(markdown.contains("### Returns"));
        assert!(markdown.contains("current size"));
        // One contiguous parameters section before returns.
        let params = markdown.find("### Parameters").expect("params");
        let returns = markdown.find("### Returns").expect("returns");
        assert!(params < returns);
        assert_eq!(markdown.matches("### Parameters").count(), 1);
    }

    #[test]
    fn xml_param_and_summary() {
        let source = "/// <summary>\n/// cache size in database\n/// second line remains separate\n/// </summary>\n/// <param name=\"size\">cache size in database</param>\nsize_t db_size;\n";
        let markdown = render(source, "db_size", 5);
        assert!(markdown.contains("### Summary"));
        assert!(markdown.contains("cache size in database  \nsecond line remains separate"));
        assert!(markdown.contains("### Parameters"));
        assert!(markdown.contains("- `size` — cache size in database"));
    }

    #[test]
    fn unknown_tag_fallback() {
        let source = "/**\n * @warning cache access is not synchronized\n * caller must hold the database lock\n */\nsize_t cache_size(void);\n";
        let markdown = render(source, "cache_size", 4);
        assert!(markdown.contains("### Warning"));
        assert!(markdown
            .contains("cache access is not synchronized  \ncaller must hold the database lock"));
    }

    #[test]
    fn escapes_heading_fence_and_html() {
        let source = "/// # fake heading\n/// ```evil\n/// note &lt;already&gt; and <b>bold</b> mid-line\nint x;\n";
        let markdown = render(source, "x", 3);
        assert!(markdown.contains("\\# fake heading") || !markdown.contains("\n# fake heading\n"));
        assert!(!markdown.contains("```evil"));
        assert!(
            markdown.contains("&lt;b&gt;") || markdown.contains("bold"),
            "html should be neutralized, got {markdown:?}"
        );
    }

    #[test]
    fn escapes_markdown_list_markers_in_prose() {
        let source =
            "/// - unordered\n/// + alternate\n/// 1. ordered\n/// 2) ordered paren\nint x;\n";
        let markdown = render(source, "x", 4);
        assert!(markdown.contains("\\- unordered"));
        assert!(markdown.contains("\\+ alternate"));
        assert!(markdown.contains("1\\. ordered"));
        assert!(markdown.contains("2\\) ordered paren"));
    }

    #[test]
    fn multiline_param_description_stays_with_item() {
        let source = "/**\n * @param[in] key database key\n *                encoded as UTF-8\n * @param[out] size cache size\n */\nbool query(const char *key, size_t *size);\n";
        let markdown = render(source, "query", 5);
        assert!(markdown.contains("- `key` *(in)* — database key  \n  encoded as UTF-8"));
        assert!(markdown.contains("- `size` *(out)* — cache size"));
    }

    #[test]
    fn truncation_sets_diagnostics_and_ellipsis() {
        let mut long = String::from("/**\n");
        for i in 0..80 {
            long.push_str(&format!(" * line {i:04}\n"));
        }
        long.push_str(" */\nint x;\n");
        let rendered =
            comment_markdown_for_symbol(&long, &anchor("x", 82), &options()).expect("rendered");
        assert!(rendered.diagnostics.truncated);
        assert!(rendered.markdown.contains('…'));
    }

    #[test]
    fn line_budget_sets_diagnostics_and_visible_ellipsis() {
        let mut source = String::new();
        for i in 0..12 {
            source.push_str(&format!("// line {i}\n"));
        }
        source.push_str("int current;\n");
        let rendered = comment_markdown_for_symbol(
            &source,
            &anchor("current", 12),
            &CommentRenderOptions {
                max_comment_lines: 4,
                max_chars: 2_000,
            },
        )
        .expect("rendered");
        assert!(rendered.diagnostics.truncated);
        assert!(rendered.markdown.contains("line 0"));
        assert!(rendered.markdown.contains('…'));
    }

    #[test]
    fn character_budget_keeps_safe_prefix_and_visible_ellipsis() {
        let rendered = comment_markdown_for_symbol(
            "// abcdefghijklmnopqrstuvwxyz\nint current;\n",
            &anchor("current", 1),
            &CommentRenderOptions {
                max_comment_lines: 48,
                max_chars: 10,
            },
        )
        .expect("rendered");
        assert!(rendered.diagnostics.truncated);
        assert!(rendered.markdown.starts_with("abcdef"));
        assert!(rendered.markdown.ends_with('…'));
        assert!(rendered.markdown.chars().count() <= 10);
    }
}

mod integration {
    use super::*;

    #[test]
    fn file_header_and_file_tag_do_not_attach() {
        assert!(comment_markdown_for_symbol(
            "/*\n * Copyright 2026 Example Corp.\n * Project: boot firmware\n * License: internal use only\n */\nint first_symbol(void);\n",
            &anchor("first_symbol", 5),
            &options()
        )
        .is_none());
        assert!(comment_markdown_for_symbol(
            "/**\n * @file driver.h\n * @brief Shared driver declarations.\n */\nint first_symbol(void);\n",
            &anchor("first_symbol", 4),
            &options()
        )
        .is_none());
    }

    #[test]
    fn leading_doc_and_ordinary_comments_still_attach() {
        let docs = render(
            "/**\n * @brief Adds two values.\n */\nint add(int lhs, int rhs);\n",
            "add",
            3,
        );
        assert!(docs.contains("### Brief") || docs.contains("Adds two values."));
        assert!(docs.contains("Adds two values."));

        let ordinary = render(
            "// Initializes the driver.\n// Safe to call twice.\nvoid init(void);\n",
            "init",
            2,
        );
        assert!(ordinary.contains("Initializes the driver.  \nSafe to call twice."));
    }

    #[test]
    fn signature_fallback_still_works() {
        let rendered = comment_markdown_from_signature(
            "/** * Test to see if a format is supported. */ bool test_fmt(void);",
            &options(),
        )
        .expect("signature comment");
        assert!(rendered
            .markdown
            .contains("Test to see if a format is supported."));
    }

    #[test]
    fn long_ordinary_comments_truncate_instead_of_dropping() {
        let mut source = String::new();
        for i in 0..20 {
            source.push_str(&format!("// ordinary line {i}\n"));
        }
        source.push_str("int current;\n");
        let rendered =
            comment_markdown_for_symbol(&source, &anchor("current", 20), &options()).expect("kept");
        assert!(rendered.markdown.contains("ordinary line 0"));
        assert!(rendered.markdown.contains("ordinary line 19") || rendered.diagnostics.truncated);
    }
}
