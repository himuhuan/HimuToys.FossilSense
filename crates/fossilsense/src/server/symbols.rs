use super::*;

impl Backend {
    pub(super) async fn handle_workspace_symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> LspResult<Option<Vec<SymbolInformation>>> {
        let tables: Vec<(PathBuf, Arc<NameTable>)> = {
            let roots = self.workspace_roots.lock().await.clone();
            let mut tables = Vec::new();
            for root in roots {
                let snapshot = self.workspace_snapshot_for_root(root.clone()).await;
                if let Some(table) = snapshot.name_table {
                    tables.push((snapshot.root, table));
                }
            }
            tables
        };
        if tables.is_empty() {
            return Ok(None);
        }

        let query_text = params.query;
        let result = tokio::task::spawn_blocking(move || -> Result<Vec<SymbolInformation>> {
            let mut hits = Vec::new();
            for (root_index, (root, table)) in tables.iter().enumerate() {
                for hit in table.search_ranked(&query_text, query::WORKSPACE_SYMBOL_LIMIT) {
                    hits.push((root_index, root.clone(), hit));
                }
            }
            hits.sort_by(|a, b| {
                b.2.score
                    .cmp(&a.2.score)
                    .then(a.2.name_len.cmp(&b.2.name_len))
                    .then_with(|| a.2.name.cmp(&b.2.name))
                    .then(a.0.cmp(&b.0))
            });
            hits.truncate(query::WORKSPACE_SYMBOL_LIMIT);

            if hits.is_empty() {
                return Ok(Vec::new());
            }

            let mut records_by_root_and_id = HashMap::new();
            let mut ids_by_root: HashMap<PathBuf, Vec<i64>> = HashMap::new();
            for (_, root, hit) in &hits {
                ids_by_root.entry(root.clone()).or_default().push(hit.id);
            }

            for (root, ids) in ids_by_root {
                let db_path = pathing::default_index_path(&root)?;
                if !db_path.exists() {
                    continue;
                }
                let store = IndexStore::open_readonly(&db_path)?;
                for record in store.symbol_read_view().symbols_by_ids(&ids)? {
                    records_by_root_and_id.insert((root.clone(), record.id), record);
                }
            }

            Ok(hits
                .into_iter()
                .filter_map(|(_, root, hit)| {
                    records_by_root_and_id
                        .get(&(root.clone(), hit.id))
                        .and_then(|record| record_to_symbol_information(&root, record))
                })
                .collect())
        })
        .await;

        Ok(self.unwrap_query("workspace/symbol", result).await)
    }

    pub(super) async fn handle_document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> LspResult<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri;
        let Some((version, text)) = self.document_snapshot(&uri).await else {
            return Ok(None);
        };
        let Some(path) = uri_to_path(&uri) else {
            return Ok(None);
        };

        let started = tokio::time::Instant::now();
        // Live parse served from the in-memory cache (one parse per document
        // version, shared across semantic tokens, completion, and symbols).
        let index = self
            .get_or_parse_document(&uri, &path, version, &text)
            .await;
        let Some(index) = index else {
            return Ok(None);
        };
        // Extract persistent symbols synchronously from the cached index.
        let document_symbols: Vec<DocumentSymbol> = index
            .persistent_facts()
            .symbols
            .iter()
            .map(parsed_to_document_symbol)
            .collect();
        self.perf_log(|| {
            format!(
                "[perf] document_symbol total={}ms count={}",
                started.elapsed().as_millis(),
                document_symbols.len(),
            )
        })
        .await;
        Ok(Some(DocumentSymbolResponse::Nested(document_symbols)))
    }
}
