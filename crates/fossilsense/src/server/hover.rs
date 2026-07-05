use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::{Hover, HoverContents, HoverParams, MarkupContent, MarkupKind};

use super::{uri_to_path, Backend};
use crate::pathing;
use crate::query;
use crate::store::IndexStore;

const HOVER_SOURCE_FILE_BYTE_LIMIT: u64 = 256 * 1024;

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
        let current_text = Arc::new(text);

        let result = tokio::task::spawn_blocking(move || -> Result<Option<String>> {
            let db_path = pathing::default_index_path(&root)?;
            if !db_path.exists() {
                return Ok(None);
            }
            let store = IndexStore::open_readonly(&db_path)?;
            let candidates = query::rank_hover_candidates(
                store.symbols_by_name(&word)?,
                &current_rel,
                reach_scope.as_deref(),
                query::HOVER_CANDIDATE_LIMIT,
            );
            Ok(hover_markdown_for_candidates(
                &root,
                &current_rel,
                current_text.as_ref(),
                &candidates,
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

pub(super) fn hover_markdown_for_candidates(
    root: &Path,
    current_rel: &str,
    current_text: &str,
    candidates: &[query::RankedHoverCandidate],
) -> Option<String> {
    if candidates.is_empty() {
        return None;
    }
    let sections: Vec<String> = candidates
        .iter()
        .map(|candidate| {
            let source = candidate_source_text(root, current_rel, current_text, candidate);
            let comment = source.as_deref().and_then(|text| {
                query::leading_comment_markdown(text, candidate.candidate.range.start_line)
            });
            query::hover_markdown_for_candidate(candidate, comment.as_deref())
        })
        .collect();
    Some(sections.join("\n\n---\n\n"))
}

fn candidate_source_text(
    root: &Path,
    current_rel: &str,
    current_text: &str,
    candidate: &query::RankedHoverCandidate,
) -> Option<String> {
    if candidate.candidate.path == current_rel {
        return Some(current_text.to_string());
    }
    let path = candidate_source_path(root, &candidate.candidate.path, &candidate.candidate.source);
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
        let (confidence, reason) =
            crate::resolver::confidence_reason_for(crate::model::ScopeTier::Current, true, None);
        query::RankedHoverCandidate {
            signature: signature.to_string(),
            guard: None,
            candidate: crate::model::DefinitionCandidate {
                name: "foo".to_string(),
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
