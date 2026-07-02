use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::Result;
use tower_lsp::lsp_types::{Range, SemanticToken, Url};

use super::{uri_to_path, Backend};
use crate::coloring;
use crate::parser::FileSemanticIndex;
use crate::query;

impl Backend {
    /// Compute semantic tokens for `uri`, optionally restricted to `range`.
    ///
    /// Returns `None` when coloring is disabled or the document/path cannot be
    /// resolved. A present index sharpens the result; its absence simply means
    /// only current-file macro/type definitions color.
    pub(super) async fn compute_semantic_tokens(
        &self,
        uri: &Url,
        range: Option<Range>,
    ) -> Option<Vec<SemanticToken>> {
        if !self.semantic_coloring_enabled.load(Ordering::Relaxed) {
            return None;
        }

        let started = tokio::time::Instant::now();
        let (version, text) = self.document_snapshot(uri).await?;
        let path = uri_to_path(uri)?;
        // Coloring kind resolution is served from the in-memory name table — no
        // per-request SQLite open. An absent table (not yet indexed) leaves only
        // current-file definitions to color, same as a missing index before.
        let name_table = match self.root_for_uri(uri).await {
            Some(root) => self.name_tables.lock().await.get(&root).cloned(),
            None => None,
        };

        // Reachability scope for coloring kind resolution: delegated to the
        // shared `scope_tier` primitive. A determinate scope restricts coloring
        // to in-set definitions (Current/Reachable) + first-layer external
        // (External); an open scope routes not-proven-reachable workspace
        // definitions to `Unknown` (hard gate: do not color); `None` (scoping
        // disabled or no graph) falls back to the unscoped `workspace OR
        // directly_included` behavior via a synthesized all-workspace context
        // inside `colorable_kind_counts`.
        let color_scope: Option<query::CompletionScope> =
            self.reach_scope_for(uri)
                .await
                .map(|(rel, reach)| query::CompletionScope {
                    current_path: Some(rel),
                    reach: (*reach).clone(),
                });

        let cached = self.get_or_parse_document(uri, &path, version, &text).await;
        let index: Option<Arc<FileSemanticIndex>> = cached;
        let Some(index) = index else {
            return None;
        };

        let result = tokio::task::spawn_blocking(move || -> Result<Vec<coloring::ColoredToken>> {
            let defs = index.coloring_defs();

            // Batch the index lookup over distinct names not already resolved by
            // a current-file definition.
            let mut wanted: Vec<&str> = Vec::new();
            let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
            for occ in &index.occurrences {
                if defs.macro_defs.contains(&occ.name)
                    || defs.type_defs.contains(&occ.name)
                    || defs.enum_defs.contains(&occ.name)
                {
                    continue;
                }
                if seen.insert(occ.name.as_str()) {
                    wanted.push(occ.name.as_str());
                }
            }

            // Colorable-kind counts from memory; reproduces the SQL
            // `kind_counts_by_names_scoped` for colorable kinds with zero I/O.
            let index_counts = match &name_table {
                Some(table) => {
                    let wanted_set: std::collections::HashSet<&str> =
                        wanted.iter().copied().collect();
                    table.colorable_kind_counts(&wanted_set, color_scope.as_ref())
                }
                None => HashMap::new(),
            };

            Ok(coloring::classify_occurrences(
                &index.occurrences,
                &defs.macro_defs,
                &defs.type_defs,
                &defs.enum_defs,
                &index_counts,
            ))
        })
        .await;

        let mut tokens = self.unwrap_query("semanticTokens", result).await?;
        if let Some(range) = range {
            tokens = coloring::filter_by_line_range(tokens, range.start.line, range.end.line);
        }

        let data: Vec<SemanticToken> = coloring::encode_relative(&tokens)
            .into_iter()
            .map(|token| SemanticToken {
                delta_line: token.delta_line,
                delta_start: token.delta_start,
                length: token.length,
                token_type: token.token_type,
                token_modifiers_bitset: token.token_modifiers,
            })
            .collect();
        self.perf_log(|| {
            format!(
                "[perf] semantic_tokens total={}ms count={}",
                started.elapsed().as_millis(),
                data.len(),
            )
        })
        .await;
        Some(data)
    }
}
