use super::*;

impl Backend {
    pub(super) async fn handle_references(
        &self,
        params: ReferenceParams,
    ) -> LspResult<Option<Vec<Location>>> {
        let position = params.text_document_position;
        let uri = position.text_document.uri;

        let Some(text) = self.document_text(&uri).await else {
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

        let client = self.client.clone();
        let search_word = word.clone();
        let role_cache = self.session.cache.reference_role_cache.clone();
        let search_cache = self.session.cache.reference_search_cache.clone();
        let snapshot = self.workspace_snapshot_for_root(root.clone()).await;
        let indexed_generation = snapshot.generation.as_u64();
        let indexed_files = snapshot
            .indexed_files
            .as_ref()
            .map(|files| (**files).clone());
        let result = tokio::task::spawn_blocking(
            move || -> Result<(Vec<Location>, bool, references::ReferencesTiming)> {
                let (mut hits, truncated, timing) =
                    references::search_references_with_result_cache_and_files(
                        &root,
                        &search_word,
                        &role_cache,
                        &search_cache,
                        indexed_generation,
                        indexed_files,
                    )?;
                // Group by role for the editor: definition/declaration first, then
                // call, write, type-use, and plain reads last; ties keep path/line
                // order so each file's hits stay contiguous. This reuses the
                // candidate-model vocabulary (role grouping is the reference-side
                // counterpart to `ResolutionConfidence`/`ResolutionReason`); a text
                // hit does not carry a `ScopeTier` and is not re-ranked by the
                // shared resolver. The grouped-references command uses the same sort.
                references::sort_hits_by_role(&mut hits);
                let locations: Vec<Location> = hits
                    .iter()
                    .filter_map(|hit| hit_to_location(&root, hit))
                    .collect();
                Ok((locations, truncated, timing))
            },
        )
        .await;

        match self.unwrap_query("references", result).await {
            Some((locations, truncated, timing)) => {
                self.perf_log(|| format!(
                    "[perf] references total={}ms discover={}ms search={}ms classify={}ms occs={} cached={} truncated={}",
                    timing.total_ms,
                    timing.discover_ms,
                    timing.search_ms,
                    timing.classify_ms,
                    timing.total_occurrences,
                    timing.cached,
                    truncated,
                ))
                .await;
                if truncated {
                    client
                        .log_message(
                            MessageType::INFO,
                            format!(
                                "FossilSense references for '{word}' returned more than {} results; output truncated",
                                references::REFERENCES_LIMIT
                            ),
                        )
                        .await;
                }
                Ok(Some(locations))
            }
            _ => Ok(None),
        }
    }

    pub(super) async fn handle_grouped_references_command(
        &self,
        params: ExecuteCommandParams,
    ) -> LspResult<Option<Value>> {
        // Role-grouped find-references: same cached search as the standard
        // `references` request, but the result carries each hit's role so
        // the client can group/label it (the LSP `Location` cannot).
        let Some(arg) = params.arguments.first() else {
            return Ok(None);
        };
        let Some(uri) = arg
            .get("uri")
            .and_then(|v| v.as_str())
            .and_then(|s| Url::parse(s).ok())
        else {
            return Ok(None);
        };
        let line = arg.get("line").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let character = arg.get("character").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

        let Some(text) = self.document_text(&uri).await else {
            return Ok(None);
        };
        let line_text = text.lines().nth(line as usize).unwrap_or_default();
        let Some(word) = query::word_at(line_text, character) else {
            return Ok(None);
        };
        let Some(root) = self.root_for_uri(&uri).await else {
            return Ok(None);
        };
        let role_cache = self.session.cache.reference_role_cache.clone();
        let search_cache = self.session.cache.reference_search_cache.clone();
        let snapshot = self.workspace_snapshot_for_root(root.clone()).await;
        let indexed_generation = snapshot.generation.as_u64();
        let indexed_files = snapshot
            .indexed_files
            .as_ref()
            .map(|files| (**files).clone());
        let result = tokio::task::spawn_blocking(
            move || -> Result<(Vec<GroupedReferenceItem>, bool, references::ReferencesTiming)> {
                // Reuses the full search-result cache shared with standard
                // references; on a cache hit this does not redo discovery or
                // the text-search pass.
                let (mut hits, truncated, timing) =
                    references::search_references_with_result_cache_and_files(
                        &root,
                        &word,
                        &role_cache,
                        &search_cache,
                        indexed_generation,
                        indexed_files,
                    )?;
                references::sort_hits_by_role(&mut hits);
                Ok((grouped_reference_items(&root, &hits), truncated, timing))
            },
        )
        .await;
        match self.unwrap_query("grouped references", result).await {
            Some((items, truncated, timing)) => {
                self.perf_log(|| format!(
                    "[perf] grouped_references total={}ms discover={}ms search={}ms classify={}ms occs={} cached={} truncated={}",
                    timing.total_ms,
                    timing.discover_ms,
                    timing.search_ms,
                    timing.classify_ms,
                    timing.total_occurrences,
                    timing.cached,
                    truncated,
                ))
                .await;
                Ok(Some(serde_json::to_value(items).unwrap_or(Value::Null)))
            }
            None => Ok(None),
        }
    }
}
