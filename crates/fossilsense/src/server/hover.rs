use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::{Hover, HoverContents, HoverParams, MarkupContent, MarkupKind};

use super::{uri_to_path, Backend};
use crate::pathing;
use crate::query;
use crate::store::IndexStore;

pub(super) const HOVER_SOURCE_FILE_BYTE_LIMIT: u64 = 256 * 1024;

impl Backend {
    pub(super) async fn provide_hover(&self, params: HoverParams) -> LspResult<Option<Hover>> {
        let position = params.text_document_position_params;
        let uri = position.text_document.uri;
        let Some((_version, text)) = self.document_snapshot(&uri).await else {
            return Ok(None);
        };
        let line_text = text
            .lines()
            .nth(position.position.line as usize)
            .unwrap_or_default();
        let Some(word) = query::word_at(line_text, position.position.character) else {
            return Ok(None);
        };
        let Some(root) = self.root_for_uri(&uri).await else {
            return Ok(None);
        };
        let current_rel = uri_to_path(&uri)
            .and_then(|path| pathing::relative_slash_path(&root, &path).ok())
            .unwrap_or_default();
        let reach_scope = self.reach_scope_for(&uri).await.map(|(_, reach)| reach);
        let project_context = self
            .request_context_for_root(root.clone())
            .await
            .engine
            .project_context
            .clone();
        let current_text = Arc::new(text);

        let result = tokio::task::spawn_blocking(move || -> Result<Option<String>> {
            let db_path = pathing::default_index_path(&root)?;
            if !db_path.exists() {
                return Ok(None);
            }
            let store = IndexStore::open_readonly(&db_path)?;
            let documentation_ranked = query::rank_hover_candidates(
                store.symbol_read_view().symbols_by_name(&word)?,
                &current_rel,
                reach_scope.as_deref(),
                32,
            );
            let candidates: Vec<_> = documentation_ranked
                .iter()
                .take(query::HOVER_CANDIDATE_LIMIT)
                .cloned()
                .collect();
            Ok(hover_markdown_for_candidates_with_project(
                &root,
                &current_rel,
                current_text.as_ref(),
                &candidates,
                project_context.as_deref(),
                Some(&documentation_ranked),
            ))
        })
        .await;

        match self.unwrap_query("hover", result).await {
            Some(Some(value)) => Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value,
                }),
                range: None,
            })),
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
pub(super) fn hover_markdown_for_candidates(
    root: &Path,
    current_rel: &str,
    current_text: &str,
    candidates: &[query::RankedHoverCandidate],
) -> Option<String> {
    hover_markdown_for_candidates_with_project(
        root,
        current_rel,
        current_text,
        candidates,
        None,
        None,
    )
}

fn hover_markdown_for_candidates_with_project(
    root: &Path,
    current_rel: &str,
    current_text: &str,
    candidates: &[query::RankedHoverCandidate],
    project_context: Option<&crate::project_context::ProjectContextIndex>,
    documentation_ranked: Option<&[query::RankedHoverCandidate]>,
) -> Option<String> {
    if candidates.is_empty() {
        return None;
    }
    let documentation_candidates: Vec<_> = documentation_ranked
        .unwrap_or(candidates)
        .iter()
        .map(|candidate| query::DocumentationCandidate {
            candidate: candidate.candidate.clone(),
            signature: candidate.signature.clone(),
        })
        .collect();
    let mut seen = HashSet::new();
    let mut sections = Vec::new();
    for candidate in candidates {
        let primary = query::DocumentationCandidate {
            candidate: candidate.candidate.clone(),
            signature: candidate.signature.clone(),
        };
        let preferred = super::completion_documentation::preferred_symbol_documentation(
            root,
            current_rel,
            current_text,
            &primary,
            &documentation_candidates,
            project_context,
        );
        let presentation = &preferred.presentation;
        let key = (
            presentation.candidate.path.clone(),
            presentation.candidate.range.start_line,
            presentation.signature.clone(),
        );
        if !seen.insert(key) {
            continue;
        }
        let guard = documentation_ranked
            .unwrap_or(candidates)
            .iter()
            .find(|candidate| {
                candidate.candidate.path == presentation.candidate.path
                    && candidate.candidate.range == presentation.candidate.range
            })
            .and_then(|candidate| candidate.guard.clone());
        let display = query::RankedHoverCandidate {
            candidate: presentation.candidate.clone(),
            signature: presentation.signature.clone(),
            guard,
        };
        let comment = preferred.comment.map(|comment| comment.markdown);
        sections.push(query::hover_markdown_for_candidate(
            &display,
            comment.as_deref(),
        ));
    }
    Some(sections.join("\n\n---\n\n"))
}

pub(super) fn candidate_source_text_for_path(
    root: &Path,
    current_rel: &str,
    current_text: &str,
    candidate_path: &str,
    source: &str,
) -> Option<String> {
    if candidate_path == current_rel {
        if current_text.len() as u64 > HOVER_SOURCE_FILE_BYTE_LIMIT {
            return None;
        }
        return Some(current_text.to_string());
    }
    let path = candidate_source_path(root, candidate_path, source);
    let metadata = std::fs::metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() > HOVER_SOURCE_FILE_BYTE_LIMIT {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

fn candidate_source_path(root: &Path, path: &str, source: &str) -> PathBuf {
    if source == "external" {
        return PathBuf::from(path);
    }
    let mut out = root.to_path_buf();
    for segment in path.split('/') {
        if !segment.is_empty() {
            out.push(segment);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(path: &str, line: u32, signature: &str) -> query::RankedHoverCandidate {
        candidate_named("foo", path, line, signature)
    }

    fn candidate_named(
        name: &str,
        path: &str,
        line: u32,
        signature: &str,
    ) -> query::RankedHoverCandidate {
        let (confidence, reason) =
            crate::resolver::confidence_reason_for(crate::model::ScopeTier::Current, true, None);
        query::RankedHoverCandidate {
            signature: signature.to_string(),
            guard: None,
            candidate: crate::model::DefinitionCandidate {
                name: name.to_string(),
                kind: "function".to_string(),
                role: "definition".to_string(),
                path: path.to_string(),
                range: crate::model::CandidateRange {
                    start_line: line,
                    start_col: 0,
                    end_line: line,
                    end_col: 0,
                },
                source: "workspace".to_string(),
                tier: crate::model::ScopeTier::Current,
                base_match: 1000,
                confidence,
                reason,
            },
        }
    }

    #[test]
    fn hover_markdown_for_candidates_uses_current_document_comments() {
        let source = "/// @brief Current buffer docs\nint foo(void);\n";
        let markdown = hover_markdown_for_candidates(
            Path::new("F:/repo"),
            "src/main.c",
            source,
            &[candidate("src/main.c", 1, "int foo(void);")],
        )
        .expect("hover markdown");
        assert!(markdown.contains("Current buffer docs"));
        assert!(markdown.contains("```c\n// In src/main.c\nint foo(void);\n```"));
        assert!(markdown.contains("tier: current"));
    }

    #[test]
    fn hover_markdown_for_candidates_recovers_trailing_comments() {
        let source = "int foo(void); // Helps from trailing comment\n";
        let markdown = hover_markdown_for_candidates(
            Path::new("F:/repo"),
            "src/main.c",
            source,
            &[candidate("src/main.c", 0, "int foo(void);")],
        )
        .expect("hover markdown");
        assert!(markdown.contains("Helps from trailing comment"));
        assert!(markdown.contains("```c\n// In src/main.c\nint foo(void);\n```"));
    }

    #[test]
    fn hover_on_source_definition_prefers_same_project_header_documentation() {
        let root = unique_temp_root("header-doc-preference");
        let lib = root.join("lib");
        std::fs::create_dir_all(&lib).expect("create lib");
        std::fs::write(
            lib.join("ops_chain.h"),
            "/// Header API documentation.\nint foo(int value);\n",
        )
        .expect("write header");
        let current = "int foo(int value) { return value; }\n";
        let source_candidate = candidate_named("foo", "lib/ops_chain.c", 0, "int foo(int value)");
        let mut header_candidate =
            candidate_named("foo", "lib/ops_chain.h", 1, "int foo(int value);");
        header_candidate.candidate.role = "declaration".to_string();
        header_candidate.candidate.tier = crate::model::ScopeTier::Reachable;
        let project_key = crate::project_context::ProjectKey {
            workspace_root_id: "workspace".to_string(),
            project_path: "lib".to_string(),
        };
        let projects = crate::project_context::ProjectContextIndex::new(
            "workspace".to_string(),
            "test".to_string(),
            vec![crate::project_context::ProjectContext {
                key: project_key,
                workspace_name: "lib".to_string(),
                marker_files: vec!["lib/CMakeLists.txt".to_string()],
            }],
        );

        let markdown = hover_markdown_for_candidates_with_project(
            &root,
            "lib/ops_chain.c",
            current,
            &[source_candidate, header_candidate],
            Some(&projects),
            None,
        )
        .expect("hover");
        let first = markdown
            .split("\n\n---\n\n")
            .next()
            .expect("first candidate");
        let _ = std::fs::remove_dir_all(&root);
        assert!(first.contains("Header API documentation."));
        assert!(first.contains("// In lib/ops_chain.h"));
        assert!(first.contains("int foo(int value);"));
    }

    #[test]
    fn hover_markdown_for_candidates_recovers_trailing_in_multiline_buffer() {
        let source = "#define VALUE 1\n/// @brief Helps the smoke test.\n/// <param name=\"unused\">structured param</param>\nvoid helper(void);\nint trailing_docs(void); // trailing hover comment\n";
        let markdown = hover_markdown_for_candidates(
            Path::new("F:/repo"),
            "defs.h",
            source,
            &[candidate_named(
                "trailing_docs",
                "defs.h",
                4,
                "int trailing_docs(void);",
            )],
        )
        .expect("hover markdown");
        assert!(
            markdown.contains("trailing hover comment"),
            "missing trailing comment in {markdown}"
        );
    }

    #[test]
    fn hover_markdown_for_candidates_renders_structured_xml_param() {
        let source = "/// <param name=\"size\">cache size</param>\nint foo(int size);\n";
        let markdown = hover_markdown_for_candidates(
            Path::new("F:/repo"),
            "src/main.c",
            source,
            &[candidate("src/main.c", 1, "int foo(int size);")],
        )
        .expect("hover markdown");
        assert!(markdown.contains("### Parameters"));
        assert!(markdown.contains("- `size` — cache size"));
    }

    #[test]
    fn hover_markdown_for_candidates_keeps_signature_when_file_unreadable() {
        let markdown = hover_markdown_for_candidates(
            Path::new("F:/repo"),
            "src/main.c",
            "",
            &[candidate("include/missing.h", 9, "int foo(int x);")],
        )
        .expect("hover markdown");
        assert!(markdown.contains("int foo(int x);"));
        assert!(!markdown.contains("Parameters"));
    }

    #[test]
    fn hover_markdown_for_candidates_skips_oversized_candidate_source_files() {
        let root = unique_temp_root("huge-hover-source");
        let include_dir = root.join("include");
        std::fs::create_dir_all(&include_dir).expect("create temp include dir");

        let filler_lines = 30_000usize;
        let filler = "int filler;\n".repeat(filler_lines);
        let source = format!("{filler}/// Huge docs that should not be read\nint foo(void);\n");
        std::fs::write(include_dir.join("huge.h"), source).expect("write huge source");

        let markdown = hover_markdown_for_candidates(
            &root,
            "src/main.c",
            "",
            &[candidate(
                "include/huge.h",
                filler_lines as u32 + 1,
                "int foo(void);",
            )],
        )
        .expect("hover markdown");

        let _ = std::fs::remove_dir_all(&root);
        assert!(markdown.contains("int foo(void);"));
        assert!(!markdown.contains("Huge docs that should not be read"));
    }

    #[test]
    fn hover_markdown_for_candidates_skips_oversized_current_buffer_comments() {
        let filler_lines = 30_000usize;
        let filler = "int filler;\n".repeat(filler_lines);
        let source =
            format!("{filler}/// Huge current docs that should not be read\nint foo(void);\n");
        assert!(source.len() as u64 > HOVER_SOURCE_FILE_BYTE_LIMIT);

        let markdown = hover_markdown_for_candidates(
            Path::new("F:/repo"),
            "src/main.c",
            &source,
            &[candidate(
                "src/main.c",
                filler_lines as u32 + 1,
                "int foo(void);",
            )],
        )
        .expect("hover markdown");

        assert!(markdown.contains("int foo(void);"));
        assert!(!markdown.contains("Huge current docs that should not be read"));
    }

    #[test]
    fn hover_markdown_for_candidates_returns_none_for_empty_candidates() {
        assert!(
            hover_markdown_for_candidates(Path::new("F:/repo"), "src/main.c", "", &[]).is_none()
        );
    }

    fn unique_temp_root(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("fossilsense-{name}-{}-{nanos}", std::process::id()))
    }
}
