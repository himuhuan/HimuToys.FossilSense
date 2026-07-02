use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::{
    CompletionItem, CompletionItemKind, CompletionList, CompletionResponse, Documentation,
    Position, Url,
};

use super::{empty_completion_list, member_completion_is_incomplete, uri_to_path, Backend};
use crate::model;
use crate::parser::{self, FileSemanticIndex};
use crate::pathing;
use crate::query;
use crate::store::IndexStore;

impl Backend {
    /// Member-access (`.`/`->`) completion: narrow to the receiver record's
    /// fields when current-file inference resolves the receiver type, otherwise
    /// degrade to the global field-candidate list. Fields only — never functions
    /// or macros, which are not valid after a member operator.
    pub(super) async fn complete_members(
        &self,
        uri: &Url,
        version: i32,
        text: &str,
        line_text: &str,
        position: Position,
    ) -> LspResult<Option<CompletionResponse>> {
        let receiver = query::member_receiver_name(line_text, position.character);
        let prefix = query::completion_prefix_at(line_text, position.character).unwrap_or_default();
        let byte_offset = query::byte_offset_at(text, position.line, position.character);
        let path = uri_to_path(uri);
        let text_owned = text.to_string();
        let roots = self.workspace_roots.lock().await.clone();
        let limit = query::COMPLETION_LIMIT;
        let min_prefix = query::MIN_PREFIX_LEN;

        // Member completion uses the same repository-relative current path and
        // open-scope semantics as normal completion/coloring. A closed scope can
        // prove non-reachability; an open scope softens out-of-set candidates to
        // Unknown rather than treating the request as unscoped.
        let reach_info = self.reach_scope_for(uri).await;
        let current_rel_path = reach_info.as_ref().map(|(rel, _)| rel.clone()).or_else(|| {
            let path = path.as_ref()?;
            roots
                .iter()
                .find(|root| path.starts_with(root))
                .and_then(|root| pathing::relative_slash_path(root, path).ok())
        });
        let member_reach = reach_info.map(|(_, reach)| (*reach).clone());

        // Use the live-document parse cache; receiver inference only needs
        // local_declarations, but caching the full parse avoids re-parsing
        // when the same version is also needed by semantic tokens or document
        // symbols.
        let cached_index: Option<Arc<FileSemanticIndex>> = match (&receiver, &path) {
            (Some(_), Some(path)) => {
                self.get_or_parse_document(uri, path, version, &text_owned)
                    .await
            }
            _ => None,
        };

        let result = tokio::task::spawn_blocking(move || -> Result<(Vec<CompletionItem>, bool)> {
            // Try to resolve the receiver record from the current file's AST.
            // The parse is served from the live-document cache when available.
            let record_key = match (&receiver, &path) {
                (Some(name), Some(_path)) => cached_index.as_ref().and_then(|index| {
                    parser::infer_receiver_record(&index.local_declarations, name, byte_offset)
                }),
                _ => None,
            };

            let mut record_candidates_by_db: Vec<(PathBuf, Vec<crate::model::RecordCandidate>)> =
                Vec::new();
            if let Some(key) = &record_key {
                for root in &roots {
                    let db_path = pathing::default_index_path(root)?;
                    if db_path.exists() {
                        let store = IndexStore::open_readonly(&db_path)?;
                        let ctx = crate::resolver::ResolveContext {
                            current_path: current_rel_path.as_deref(),
                            reach: member_reach.as_ref(),
                        };
                        let mut candidates =
                            store.resolve_record_candidates(&[key.as_str()], Some(&ctx))?;

                        if candidates.is_empty() && member_reach.is_some() {
                            let ctx_unscoped = crate::resolver::ResolveContext {
                                current_path: current_rel_path.as_deref(),
                                reach: None,
                            };
                            candidates = store
                                .resolve_record_candidates(&[key.as_str()], Some(&ctx_unscoped))?;
                        }

                        if !candidates.is_empty() {
                            record_candidates_by_db.push((db_path, candidates));
                        }
                    }
                }
            }

            let mut field_to_tier: HashMap<String, (crate::model::ScopeTier, String)> =
                HashMap::new();
            let highest_record_rank = record_candidates_by_db
                .iter()
                .flat_map(|(_, candidates)| {
                    candidates.iter().map(|candidate| candidate.tier.rank())
                })
                .max();
            if let Some(highest_rank) = highest_record_rank {
                for (db_path, candidates) in &record_candidates_by_db {
                    let record_ids: Vec<i64> = candidates
                        .iter()
                        .filter(|candidate| candidate.tier.rank() == highest_rank)
                        .map(|candidate| candidate.id)
                        .collect();
                    if !record_ids.is_empty() {
                        let store = IndexStore::open_readonly(db_path)?;
                        let fields = store.fields_for_records(&record_ids)?;
                        let tier = candidates
                            .iter()
                            .find(|candidate| candidate.tier.rank() == highest_rank)
                            .map(|candidate| candidate.tier)
                            .unwrap_or(crate::model::ScopeTier::Global);
                        for f in fields {
                            let entry = field_to_tier.entry(f.clone()).or_insert((tier, f));
                            if tier.rank() > entry.0.rank() {
                                entry.0 = tier;
                            }
                        }
                    }
                }
            }

            if field_to_tier.is_empty() && !record_candidates_by_db.is_empty() {
                for (db_path, candidates) in &record_candidates_by_db {
                    let record_ids: Vec<i64> =
                        candidates.iter().map(|candidate| candidate.id).collect();
                    if !record_ids.is_empty() {
                        let store = IndexStore::open_readonly(db_path)?;
                        for candidate in candidates {
                            let fields = store.fields_for_records(&[candidate.id])?;
                            for f in fields {
                                let entry = field_to_tier
                                    .entry(f.clone())
                                    .or_insert((crate::model::ScopeTier::Global, f));
                                if candidate.tier.rank() > entry.0.rank() {
                                    entry.0 = candidate.tier;
                                }
                            }
                        }
                    }
                }
            }

            let prefix_lower = prefix.to_ascii_lowercase();
            let (names_with_tiers, resolved_hit) = if !field_to_tier.is_empty() {
                let filtered: Vec<(String, crate::model::ScopeTier)> = field_to_tier
                    .into_values()
                    .filter(|(_, name)| {
                        prefix_lower.is_empty() || name.to_ascii_lowercase().contains(&prefix_lower)
                    })
                    .map(|(tier, name)| (name, tier))
                    .collect();
                (filtered, true)
            } else if prefix.len() >= min_prefix {
                let mut fallback: Vec<(String, crate::model::ScopeTier)> = Vec::new();
                for root in &roots {
                    let db_path = pathing::default_index_path(root)?;
                    if db_path.exists() {
                        let store = IndexStore::open_readonly(&db_path)?;
                        let ctx = crate::resolver::ResolveContext {
                            current_path: current_rel_path.as_deref(),
                            reach: member_reach.as_ref(),
                        };
                        fallback.extend(store.fallback_field_candidates(
                            &prefix,
                            limit,
                            Some(&ctx),
                        )?);
                    }
                }
                (fallback, false)
            } else {
                (Vec::new(), false)
            };

            // Open-scope cause for the member-completion reach set, so an
            // ambiguous-include `Unknown` field owner surfaces `ambiguous`.
            let member_open_reason = member_reach.as_ref().and_then(|r| r.reason);
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut scored: Vec<(i32, String, CompletionItem)> = Vec::new();
            for (name, tier) in names_with_tiers {
                let name_lower = name.to_ascii_lowercase();
                if !seen.insert(name_lower.clone()) {
                    continue;
                }
                let base_match = if name_lower == prefix_lower {
                    100
                } else if prefix_lower.is_empty() || name_lower.starts_with(&prefix_lower) {
                    50
                } else {
                    0
                };
                let score = crate::resolver::pack_score(tier, base_match, 0);
                // Same best-effort label as identifier completion, from the
                // field owner record's tier (non-current owners get a tag).
                let (confidence, reason) =
                    crate::resolver::confidence_reason_for(tier, false, member_open_reason);
                let label = model::completion_scope_label(tier, confidence, reason);
                scored.push((
                    score,
                    name.clone(),
                    CompletionItem {
                        label: name,
                        kind: Some(CompletionItemKind::FIELD),
                        sort_text: Some(format!("{:08}", 100_000_000 - score)),
                        detail: label.as_ref().map(|l| l.detail.to_string()),
                        documentation: label.map(|l| Documentation::String(l.documentation)),
                        ..Default::default()
                    },
                ));
            }
            scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            let is_incomplete = member_completion_is_incomplete(resolved_hit, scored.len(), limit);
            let items: Vec<CompletionItem> = scored
                .into_iter()
                .take(limit)
                .map(|(_, _, it)| it)
                .collect();
            Ok((items, is_incomplete))
        })
        .await;

        match self.unwrap_query("member completion", result).await {
            // Receiver-resolved fields are complete unless the cap truncated
            // them. Global-field fallback is always an incomplete guess.
            Some((items, false)) if !items.is_empty() => Ok(Some(CompletionResponse::Array(items))),
            Some((items, true)) if !items.is_empty() => {
                Ok(Some(CompletionResponse::List(CompletionList {
                    is_incomplete: true,
                    items,
                })))
            }
            // Empty list stays incomplete so the editor re-queries as more is typed.
            _ => Ok(Some(empty_completion_list(true))),
        }
    }
}
