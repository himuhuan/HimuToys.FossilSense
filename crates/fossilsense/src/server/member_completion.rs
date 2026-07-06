use std::collections::{HashMap, HashSet};
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
use crate::parser::{
    self, FactAvailability, FactGroup, FileSemanticIndex, MemberConfidence, MemberKind,
};
use crate::pathing;
use crate::query;
use crate::store::IndexStore;

impl Backend {
    /// Member-access (`.`/`->`) completion: narrow to the receiver record's
    /// member evidence when current-file inference resolves the receiver type,
    /// otherwise use a conservative weak receiver correlation before degrading
    /// to the global member-candidate fallback.
    pub(super) async fn complete_members(
        &self,
        uri: &Url,
        version: i32,
        text: &str,
        line_text: &str,
        position: Position,
    ) -> LspResult<Option<CompletionResponse>> {
        let member_chain = query::member_access_chain_at(line_text, position.character);
        let receiver = member_chain.as_ref().map(|chain| chain.receiver.clone());
        let completed_members = member_chain
            .as_ref()
            .map(|chain| chain.completed_members.clone())
            .unwrap_or_default();
        let prefix = query::completion_prefix_at(line_text, position.character).unwrap_or_default();
        let byte_offset = query::byte_offset_at(text, position.line, position.character);
        let path = uri_to_path(uri);
        let text_owned = text.to_string();
        let roots = self.workspace_roots.lock().await.clone();
        let limit = query::COMPLETION_LIMIT;
        let min_prefix = query::MEMBER_COMPLETION_MIN_PREFIX_LEN;

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

        let started = tokio::time::Instant::now();
        let result = tokio::task::spawn_blocking(
            move || -> Result<(Vec<CompletionItem>, bool, MemberCompletionMetrics)> {
                // Try to resolve the receiver record from the current file's AST.
                // The parse is served from the live-document cache when available.
                let record_key = match (&receiver, &path) {
                    (Some(name), Some(_path)) => cached_index.as_ref().and_then(|index| {
                        if !matches!(
                            index.fact_availability(FactGroup::LocalDeclarations),
                            FactAvailability::Available
                        ) {
                            return None;
                        }
                        parser::infer_receiver_record(
                            index.request_facts().local_declarations,
                            name,
                            byte_offset,
                        )
                    }),
                    _ => None,
                };

                let mut record_candidates_by_db: Vec<(
                    PathBuf,
                    Vec<crate::model::RecordCandidate>,
                )> = Vec::new();
                if let Some(key) = &record_key {
                    for root in &roots {
                        let db_path = pathing::default_index_path(root)?;
                        if db_path.exists() {
                            let store = IndexStore::open_readonly(&db_path)?;
                            let ctx = crate::resolver::ResolveContext {
                                current_path: current_rel_path.as_deref(),
                                reach: member_reach.as_ref(),
                            };
                            let mut candidates = store
                                .member_view()
                                .resolve_record_candidates(&[key.as_str()], Some(&ctx))?;

                            if candidates.is_empty() && member_reach.is_some() {
                                let ctx_unscoped = crate::resolver::ResolveContext {
                                    current_path: current_rel_path.as_deref(),
                                    reach: None,
                                };
                                candidates = store.member_view().resolve_record_candidates(
                                    &[key.as_str()],
                                    Some(&ctx_unscoped),
                                )?;
                            }

                            if !candidates.is_empty() {
                                record_candidates_by_db.push((db_path, candidates));
                            }
                        }
                    }
                }

                let mut explicit_record_found =
                    record_key.is_some() && !record_candidates_by_db.is_empty();
                let mut weak_owner_used = false;
                if record_candidates_by_db.is_empty()
                    && completed_members.is_empty()
                    && prefix.len() >= min_prefix
                {
                    if let Some(receiver_name) = receiver.as_deref() {
                        let lookup_names = weak_receiver_lookup_names(receiver_name);
                        let lookup_refs: Vec<&str> =
                            lookup_names.iter().map(String::as_str).collect();
                        let mut weak_candidates_by_db = Vec::new();
                        let mut all_candidates = Vec::new();
                        for root in &roots {
                            let db_path = pathing::default_index_path(root)?;
                            if db_path.exists() {
                                let store = IndexStore::open_readonly(&db_path)?;
                                let ctx = crate::resolver::ResolveContext {
                                    current_path: current_rel_path.as_deref(),
                                    reach: member_reach.as_ref(),
                                };
                                let candidates = store
                                    .member_view()
                                    .resolve_record_candidates(&lookup_refs, Some(&ctx))?;
                                if !candidates.is_empty() {
                                    all_candidates.extend(candidates.iter().cloned());
                                    weak_candidates_by_db.push((db_path, candidates));
                                }
                            }
                        }
                        let weak_ids = weak_receiver_record_ids(receiver_name, &all_candidates);
                        if weak_ids.len() == 1 {
                            let weak_id = weak_ids[0];
                            let mut accepted = Vec::new();
                            for (db_path, candidates) in weak_candidates_by_db {
                                let filtered: Vec<_> = candidates
                                    .into_iter()
                                    .filter(|candidate| {
                                        candidate.id == weak_id
                                            && weak_receiver_matches_record(
                                                receiver_name,
                                                candidate,
                                            )
                                    })
                                    .collect();
                                if !filtered.is_empty() {
                                    accepted.push((db_path, filtered));
                                }
                            }
                            if !accepted.is_empty() {
                                record_candidates_by_db = accepted;
                                weak_owner_used = true;
                            }
                        }
                    }
                }

                if !completed_members.is_empty() && !record_candidates_by_db.is_empty() {
                    for member_name in completed_members {
                        let type_names = member_type_names_for_segment(
                            &record_candidates_by_db,
                            &member_name,
                            current_rel_path.as_deref(),
                            member_reach.as_ref(),
                        )?;
                        if type_names.is_empty() {
                            record_candidates_by_db.clear();
                            break;
                        }

                        record_candidates_by_db = resolve_record_names_across_roots(
                            &roots,
                            &type_names,
                            current_rel_path.as_deref(),
                            member_reach.as_ref(),
                        )?;
                        if record_candidates_by_db.is_empty() {
                            break;
                        }
                    }
                    explicit_record_found = !record_candidates_by_db.is_empty();
                }

                let mut member_to_best: HashMap<(String, MemberKind), MemberPresentation> =
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
                            let members = store.member_view().members_for_records(
                                &record_ids,
                                None,
                                Some(&crate::resolver::ResolveContext {
                                    current_path: current_rel_path.as_deref(),
                                    reach: member_reach.as_ref(),
                                }),
                            )?;
                            for member in members {
                                remember_member(&mut member_to_best, member, weak_owner_used);
                            }
                        }
                    }
                }

                if member_to_best.is_empty() && !record_candidates_by_db.is_empty() {
                    for (db_path, candidates) in &record_candidates_by_db {
                        let record_ids: Vec<i64> =
                            candidates.iter().map(|candidate| candidate.id).collect();
                        if !record_ids.is_empty() {
                            let store = IndexStore::open_readonly(db_path)?;
                            let members = store.member_view().members_for_records(
                                &record_ids,
                                None,
                                Some(&crate::resolver::ResolveContext {
                                    current_path: current_rel_path.as_deref(),
                                    reach: member_reach.as_ref(),
                                }),
                            )?;
                            for member in members {
                                remember_member(&mut member_to_best, member, weak_owner_used);
                            }
                        }
                    }
                }

                let prefix_lower = prefix.to_ascii_lowercase();
                let (members, resolved_hit, fallback_used) = if !member_to_best.is_empty() {
                    let filtered: Vec<MemberPresentation> = member_to_best
                        .into_values()
                        .filter(|presentation| {
                            prefix_lower.is_empty()
                                || presentation
                                    .candidate
                                    .name
                                    .to_ascii_lowercase()
                                    .starts_with(&prefix_lower)
                        })
                        .collect();
                    (filtered, explicit_record_found || weak_owner_used, false)
                } else if prefix.len() >= min_prefix {
                    let mut fallback: Vec<MemberPresentation> = Vec::new();
                    for root in &roots {
                        let db_path = pathing::default_index_path(root)?;
                        if db_path.exists() {
                            let store = IndexStore::open_readonly(&db_path)?;
                            let ctx = crate::resolver::ResolveContext {
                                current_path: current_rel_path.as_deref(),
                                reach: member_reach.as_ref(),
                            };
                            fallback.extend(
                                store
                                    .member_view()
                                    .fallback_member_candidates(&prefix, limit, Some(&ctx))?
                                    .into_iter()
                                    .map(|candidate| MemberPresentation {
                                        candidate,
                                        weak_receiver: false,
                                    }),
                            );
                        }
                    }
                    (fallback, false, true)
                } else {
                    (Vec::new(), false, false)
                };

                // Open-scope cause for the member-completion reach set, so an
                // ambiguous-include `Unknown` field owner surfaces `ambiguous`.
                let member_open_reason = member_reach.as_ref().and_then(|r| r.reason);
                let mut seen: HashSet<(String, MemberKind)> = HashSet::new();
                let mut scored: Vec<(i32, i32, String, CompletionItem)> = Vec::new();
                let mut field_count = 0usize;
                let mut method_count = 0usize;
                for presentation in members {
                    let member = presentation.candidate;
                    let name_lower = member.name.to_ascii_lowercase();
                    if !seen.insert((name_lower.clone(), member.kind)) {
                        continue;
                    }
                    let base_match = if name_lower == prefix_lower {
                        100
                    } else if prefix_lower.is_empty() || name_lower.starts_with(&prefix_lower) {
                        50
                    } else {
                        0
                    };
                    let score = crate::resolver::pack_score(member.tier, base_match, 0);
                    // Same best-effort label as identifier completion, from the
                    // member owner record's tier (non-current owners get a tag).
                    let (confidence, reason) = crate::resolver::confidence_reason_for(
                        member.tier,
                        false,
                        member_open_reason,
                    );
                    let label = model::completion_scope_label(member.tier, confidence, reason);
                    let detail =
                        member_detail(member.kind, label.as_ref(), presentation.weak_receiver);
                    let documentation = member_documentation(
                        member.kind,
                        member.confidence,
                        label.as_ref(),
                        presentation.weak_receiver,
                    );
                    match member.kind {
                        MemberKind::Field => field_count += 1,
                        MemberKind::Method | MemberKind::StaticMethod => method_count += 1,
                        MemberKind::NestedType => {}
                    }
                    scored.push((
                        score,
                        member_kind_rank(member.kind),
                        member.name.clone(),
                        CompletionItem {
                            label: member.name,
                            kind: Some(lsp_kind_for_member(member.kind)),
                            sort_text: Some(format!("{:08}", 100_000_000 - score)),
                            detail: Some(detail),
                            documentation: Some(Documentation::String(documentation)),
                            ..Default::default()
                        },
                    ));
                }
                scored.sort_by(|a, b| {
                    b.0.cmp(&a.0)
                        .then_with(|| a.1.cmp(&b.1))
                        .then_with(|| a.2.cmp(&b.2))
                });
                let is_incomplete =
                    member_completion_is_incomplete(resolved_hit, scored.len(), limit);
                let items: Vec<CompletionItem> = scored
                    .into_iter()
                    .take(limit)
                    .map(|(_, _, _, it)| it)
                    .collect();
                let returned = items.len();
                Ok((
                    items,
                    is_incomplete,
                    MemberCompletionMetrics {
                        resolved_owner: explicit_record_found,
                        weak_owner: weak_owner_used,
                        fallback: fallback_used,
                        fields: field_count,
                        methods: method_count,
                        returned,
                    },
                ))
            },
        )
        .await;
        let total_ms = started.elapsed().as_millis();
        let metrics = result
            .as_ref()
            .ok()
            .and_then(|inner| inner.as_ref().ok().map(|(_, _, metrics)| *metrics))
            .unwrap_or_default();
        self.perf_log(|| {
            format!(
                "[perf] member_completion total={}ms resolved_owner={} weak_owner={} fallback={} fields={} methods={} returned={}",
                total_ms,
                metrics.resolved_owner as u8,
                metrics.weak_owner as u8,
                metrics.fallback as u8,
                metrics.fields,
                metrics.methods,
                metrics.returned,
            )
        })
        .await;

        match self.unwrap_query("member completion", result).await {
            // Receiver-resolved fields are complete unless the cap truncated
            // them. Global-field fallback is always an incomplete guess.
            Some((items, false, _)) if !items.is_empty() => {
                Ok(Some(CompletionResponse::Array(items)))
            }
            Some((items, true, _)) if !items.is_empty() => {
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

#[derive(Clone)]
struct MemberPresentation {
    candidate: crate::model::MemberCandidate,
    weak_receiver: bool,
}

#[derive(Clone, Copy, Default)]
struct MemberCompletionMetrics {
    resolved_owner: bool,
    weak_owner: bool,
    fallback: bool,
    fields: usize,
    methods: usize,
    returned: usize,
}

fn remember_member(
    members: &mut HashMap<(String, MemberKind), MemberPresentation>,
    candidate: crate::model::MemberCandidate,
    weak_receiver: bool,
) {
    let key = (candidate.name.to_ascii_lowercase(), candidate.kind);
    let presentation = MemberPresentation {
        candidate,
        weak_receiver,
    };
    match members.get(&key) {
        Some(existing) if member_candidate_better(&existing.candidate, &presentation.candidate) => {
        }
        _ => {
            members.insert(key, presentation);
        }
    }
}

fn member_candidate_better(
    current: &crate::model::MemberCandidate,
    incoming: &crate::model::MemberCandidate,
) -> bool {
    current
        .tier
        .rank()
        .cmp(&incoming.tier.rank())
        .then_with(|| {
            member_confidence_rank(current.confidence)
                .cmp(&member_confidence_rank(incoming.confidence))
        })
        .then_with(|| incoming.signature.cmp(&current.signature))
        .is_gt()
}

fn member_confidence_rank(confidence: MemberConfidence) -> i32 {
    match confidence {
        MemberConfidence::InBody => 2,
        MemberConfidence::OutOfClassOwner => 1,
        MemberConfidence::Heuristic => 0,
    }
}

fn member_kind_rank(kind: MemberKind) -> i32 {
    match kind {
        MemberKind::Field => 0,
        MemberKind::Method => 1,
        MemberKind::StaticMethod => 2,
        MemberKind::NestedType => 3,
    }
}

fn lsp_kind_for_member(kind: MemberKind) -> CompletionItemKind {
    match kind {
        MemberKind::Field => CompletionItemKind::FIELD,
        MemberKind::Method | MemberKind::StaticMethod => CompletionItemKind::METHOD,
        MemberKind::NestedType => CompletionItemKind::CLASS,
    }
}

fn member_kind_label(kind: MemberKind) -> &'static str {
    match kind {
        MemberKind::Field => "field",
        MemberKind::Method => "method",
        MemberKind::StaticMethod => "static method",
        MemberKind::NestedType => "nested type",
    }
}

fn member_detail(
    kind: MemberKind,
    scope_label: Option<&model::CompletionScopeLabel>,
    weak_receiver: bool,
) -> String {
    let mut parts = vec![member_kind_label(kind).to_string()];
    if let Some(label) = scope_label {
        parts.push(label.detail.to_string());
    }
    if weak_receiver {
        parts.push("heuristic receiver".to_string());
    }
    parts.join(" ")
}

fn member_documentation(
    kind: MemberKind,
    confidence: MemberConfidence,
    scope_label: Option<&model::CompletionScopeLabel>,
    weak_receiver: bool,
) -> String {
    let scope = scope_label
        .map(|label| label.documentation.as_str())
        .unwrap_or("FossilSense: current member candidate");
    let receiver = if weak_receiver {
        ", heuristic_receiver"
    } else {
        ""
    };
    format!(
        "FossilSense: {} member candidate ({}, {}{})",
        member_kind_label(kind),
        scope,
        confidence.as_str(),
        receiver
    )
}

fn member_type_names_for_segment(
    record_candidates_by_db: &[(PathBuf, Vec<crate::model::RecordCandidate>)],
    member_name: &str,
    current_rel_path: Option<&str>,
    member_reach: Option<&crate::reachability::ReachScope>,
) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for (db_path, candidates) in record_candidates_by_db {
        let Some(highest_rank) = candidates
            .iter()
            .map(|candidate| candidate.tier.rank())
            .max()
        else {
            continue;
        };
        let record_ids: Vec<i64> = candidates
            .iter()
            .filter(|candidate| candidate.tier.rank() == highest_rank)
            .map(|candidate| candidate.id)
            .collect();
        if record_ids.is_empty() {
            continue;
        }
        let store = IndexStore::open_readonly(db_path)?;
        let members = store.member_view().members_for_records(
            &record_ids,
            Some(member_name),
            Some(&crate::resolver::ResolveContext {
                current_path: current_rel_path,
                reach: member_reach,
            }),
        )?;
        for member in members {
            if member.kind == MemberKind::Field && member.name == member_name {
                if let Some(type_name) = member.type_name {
                    names.push(type_name);
                }
            }
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}

fn resolve_record_names_across_roots(
    roots: &[PathBuf],
    type_names: &[String],
    current_rel_path: Option<&str>,
    member_reach: Option<&crate::reachability::ReachScope>,
) -> Result<Vec<(PathBuf, Vec<crate::model::RecordCandidate>)>> {
    let lookup_refs: Vec<&str> = type_names.iter().map(String::as_str).collect();
    let mut by_db = Vec::new();
    for root in roots {
        let db_path = pathing::default_index_path(root)?;
        if !db_path.exists() {
            continue;
        }
        let store = IndexStore::open_readonly(&db_path)?;
        let ctx = crate::resolver::ResolveContext {
            current_path: current_rel_path,
            reach: member_reach,
        };
        let mut candidates = store
            .member_view()
            .resolve_record_candidates(&lookup_refs, Some(&ctx))?;
        if candidates.is_empty() && member_reach.is_some() {
            let ctx_unscoped = crate::resolver::ResolveContext {
                current_path: current_rel_path,
                reach: None,
            };
            candidates = store
                .member_view()
                .resolve_record_candidates(&lookup_refs, Some(&ctx_unscoped))?;
        }
        if !candidates.is_empty() {
            by_db.push((db_path, candidates));
        }
    }
    Ok(by_db)
}

fn weak_receiver_lookup_names(receiver_name: &str) -> Vec<String> {
    let hint = query::normalized_receiver_record_hint(receiver_name);
    let mut names = Vec::new();
    if !hint.is_empty() {
        names.push(hint.clone());
        let mut chars = hint.chars();
        if let Some(first) = chars.next() {
            let pascal = format!("{}{}", first.to_ascii_uppercase(), chars.as_str());
            names.push(pascal);
        }
    }
    names.sort();
    names.dedup();
    names
}

fn weak_receiver_record_ids(
    receiver_name: &str,
    records: &[crate::model::RecordCandidate],
) -> Vec<i64> {
    let matches: Vec<&crate::model::RecordCandidate> = records
        .iter()
        .filter(|record| weak_receiver_matches_record(receiver_name, record))
        .collect();
    if matches.len() == 1 {
        vec![matches[0].id]
    } else {
        Vec::new()
    }
}

fn weak_receiver_matches_record(
    receiver_name: &str,
    record: &crate::model::RecordCandidate,
) -> bool {
    let hint = query::normalized_receiver_record_hint(receiver_name);
    if hint.is_empty() {
        return false;
    }
    record.display_name.eq_ignore_ascii_case(&hint)
        || record
            .tag_name
            .as_deref()
            .is_some_and(|name| name.eq_ignore_ascii_case(&hint))
        || record
            .typedef_name
            .as_deref()
            .is_some_and(|name| name.eq_ignore_ascii_case(&hint))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record_candidate(
        display_name: &str,
        id: i64,
        tier: crate::model::ScopeTier,
    ) -> crate::model::RecordCandidate {
        crate::model::RecordCandidate {
            id,
            display_name: display_name.to_string(),
            tag_name: Some(display_name.to_string()),
            typedef_name: None,
            kind: crate::parser::RecordKind::Struct,
            path: format!("{display_name}.hpp"),
            start_byte: 0,
            end_byte: 0,
            start_line: 0,
            start_col: 0,
            end_line: 0,
            end_col: 0,
            confidence: crate::parser::RecordConfidence::NamedTag,
            signature: format!("struct {display_name}"),
            tier,
        }
    }

    #[test]
    fn weak_receiver_uses_unique_record_name_correlation_only() {
        let records = vec![record_candidate(
            "Widget",
            1,
            crate::model::ScopeTier::Reachable,
        )];
        assert_eq!(weak_receiver_record_ids("widget", &records), vec![1]);

        let ambiguous = vec![
            record_candidate("Widget", 1, crate::model::ScopeTier::Reachable),
            record_candidate("Widget", 2, crate::model::ScopeTier::Global),
        ];
        assert!(weak_receiver_record_ids("widget", &ambiguous).is_empty());
    }
}
