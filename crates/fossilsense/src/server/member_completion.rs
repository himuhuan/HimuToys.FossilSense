use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::{
    CompletionItem, CompletionItemKind, CompletionList, CompletionResponse, Documentation,
    Position, Url,
};

use super::{
    empty_completion_list, member_completion_is_incomplete, uri_to_path, Backend,
    CompletionDocumentationData,
};
use crate::call_service::CallReadHandle;
use crate::candidate_service::{CandidateOverlaySnapshot, CandidateQueryService};
use crate::model;
use crate::parser::{
    self, FactAvailability, FactGroup, FileSemanticIndex, MemberConfidence, MemberKind,
};
use crate::pathing;
use crate::query;

use super::workspace::DocumentRequestSnapshot;

const RESOLVED_MEMBER_SCAN_LIMIT: usize = 8_192;

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
        documents: DocumentRequestSnapshot,
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
        let uri_owned = uri.to_string();
        let completion_overlay_epoch = documents.overlay_epoch;
        let roots = self.workspace_roots.lock().await.clone();
        let primary_root = self.root_for_uri(uri).await;
        let mut member_root_contexts: HashMap<PathBuf, MemberRootQueryContext> = HashMap::new();
        let mut primary_context = None;
        for root in &roots {
            let context = self.request_context_for_root(root.clone()).await;
            if primary_root.as_ref() == Some(root) {
                primary_context = Some(context.clone());
            }
            let overlay = self
                .candidate_overlay_snapshot_from_documents(
                    root,
                    context.engine.semantic_generation,
                    context.engine.reach_graph.as_deref(),
                    context.engine.indexed_files.as_deref().map(Vec::as_slice),
                    documents.clone(),
                )
                .await;
            let current_path = path
                .as_deref()
                .and_then(|path| pathing::relative_slash_path(root, path).ok())
                .or_else(|| path.as_deref().map(pathing::normalize_abs_path))
                .unwrap_or_else(|| uri_owned.clone());
            member_root_contexts.insert(
                root.clone(),
                MemberRootQueryContext {
                    handle: context.engine.call_read_handle.clone(),
                    overlay,
                    current_path,
                    reach_graph: context.engine.reach_graph.clone(),
                },
            );
        }
        let limit = query::COMPLETION_LIMIT;
        let min_prefix = query::MEMBER_COMPLETION_MIN_PREFIX_LEN;

        // Member completion uses the same repository-relative current path and
        // open-scope semantics as normal completion/coloring. A closed scope can
        // prove non-reachability; an open scope softens out-of-set candidates to
        // Unknown rather than treating the request as unscoped.
        let reach_info = primary_context
            .as_ref()
            .and_then(|context| self.reach_scope_from_context(uri, context));
        let member_reach = reach_info.map(|(_, reach)| (*reach).clone());

        // Use the live-document parse cache; receiver inference only needs
        // local_declarations, but caching the full parse avoids re-parsing
        // when the same version is also needed by semantic tokens or document
        // symbols.
        let cached_index: Option<Arc<FileSemanticIndex>> = match (&receiver, &path) {
            (Some(_), Some(path)) => {
                self.get_or_parse_document(
                    uri,
                    path,
                    version,
                    &text_owned,
                    parser::ParseFacts::MEMBER,
                )
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

                // Every workspace root gets its own generation-pinned handle
                // and all-open overlay from the same document snapshot. This
                // keeps cross-root recall useful without allowing a secondary
                // root's dirty record/alias/member path to revive base facts.
                let mut owner_incomplete = false;
                let mut owner_ambiguous = false;
                let mut explicit_record_found = false;
                let mut record_candidates_by_root = Vec::new();
                let mut resolved_member_scan_remaining = RESOLVED_MEMBER_SCAN_LIMIT;
                if let Some(key) = record_key.as_deref() {
                    let type_names = [key.to_string()];
                    let resolution = resolve_record_names_across_roots(
                        &roots,
                        &type_names,
                        &member_root_contexts,
                    )?;
                    owner_incomplete |= resolution.incomplete;
                    owner_ambiguous |= resolution.ambiguous;
                    explicit_record_found =
                        resolution.authoritative || !resolution.candidates.is_empty();
                    record_candidates_by_root = resolution.candidates;
                    retain_global_highest_record_tier(&mut record_candidates_by_root);
                }

                let mut weak_owner_used = false;
                if record_candidates_by_root.is_empty()
                    && !explicit_record_found
                    && completed_members.is_empty()
                    && prefix.len() >= min_prefix
                {
                    if let Some(receiver_name) = receiver.as_deref() {
                        let lookup_names = weak_receiver_lookup_names(receiver_name);
                        let mut weak_matches = Vec::new();
                        let mut seen_weak = HashSet::new();
                        for root in &roots {
                            let Some(context) = member_root_contexts.get(root) else {
                                continue;
                            };
                            let service = context.service();
                            for lookup_name in &lookup_names {
                                let resolution =
                                    service.records_for_type_name_with_evidence(lookup_name)?;
                                owner_incomplete |= resolution.incomplete;
                                for candidate in resolution.records.into_iter().filter(|record| {
                                    weak_receiver_matches_record(receiver_name, record)
                                }) {
                                    if seen_weak.insert((root.clone(), candidate.identity.clone()))
                                    {
                                        weak_matches.push((root.clone(), candidate));
                                    }
                                }
                            }
                        }
                        if let [only] = weak_matches.as_slice() {
                            record_candidates_by_root =
                                vec![(only.0.clone(), vec![only.1.clone()])];
                            weak_owner_used = true;
                        }
                    }
                }

                if !completed_members.is_empty() && !record_candidates_by_root.is_empty() {
                    for member_name in completed_members {
                        let (type_names, member_read_limited) = member_type_names_for_segment(
                            &record_candidates_by_root,
                            &member_name,
                            &member_root_contexts,
                            &mut resolved_member_scan_remaining,
                        )?;
                        owner_incomplete |= member_read_limited;
                        owner_ambiguous |= type_names.len() > 1;
                        owner_incomplete |= type_names.len() > 1;
                        if type_names.is_empty() {
                            record_candidates_by_root.clear();
                            break;
                        }

                        let resolution = resolve_record_names_across_roots(
                            &roots,
                            &type_names,
                            &member_root_contexts,
                        )?;
                        owner_incomplete |= resolution.incomplete;
                        owner_ambiguous |= resolution.ambiguous;
                        explicit_record_found |= resolution.authoritative;
                        record_candidates_by_root = resolution.candidates;
                        retain_global_highest_record_tier(&mut record_candidates_by_root);
                        if record_candidates_by_root.is_empty() {
                            break;
                        }
                    }
                    explicit_record_found |= !record_candidates_by_root.is_empty();
                }

                if !weak_owner_used {
                    let highest_rank = record_candidates_by_root
                        .iter()
                        .flat_map(|(_, candidates)| {
                            candidates.iter().map(|candidate| candidate.tier.rank())
                        })
                        .max();
                    if let Some(highest_rank) = highest_rank {
                        let highest_count = record_candidates_by_root
                            .iter()
                            .flat_map(|(_, candidates)| candidates)
                            .filter(|candidate| candidate.tier.rank() == highest_rank)
                            .count();
                        owner_ambiguous |= highest_count > 1;
                        owner_incomplete |= highest_count > 1;
                    }
                }

                let mut member_to_best: HashMap<(String, MemberKind), MemberPresentation> =
                    HashMap::new();
                let highest_record_rank = record_candidates_by_root
                    .iter()
                    .flat_map(|(_, candidates)| {
                        candidates.iter().map(|candidate| candidate.tier.rank())
                    })
                    .max();
                if let Some(highest_rank) = highest_record_rank {
                    for (root, candidates) in &record_candidates_by_root {
                        let selected: Vec<_> = candidates
                            .iter()
                            .filter(|candidate| candidate.tier.rank() == highest_rank)
                            .cloned()
                            .collect();
                        if !selected.is_empty() {
                            let Some(context) = member_root_contexts.get(root) else {
                                continue;
                            };
                            if resolved_member_scan_remaining == 0 {
                                owner_incomplete = true;
                                break;
                            }
                            let read = context.service().members_for_records_limited(
                                &selected,
                                None,
                                resolved_member_scan_remaining,
                            )?;
                            resolved_member_scan_remaining =
                                resolved_member_scan_remaining.saturating_sub(read.scanned);
                            owner_incomplete |= read.truncated;
                            for member in read.candidates {
                                remember_member(
                                    &mut member_to_best,
                                    member,
                                    weak_owner_used,
                                    owner_ambiguous,
                                );
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
                } else if prefix.len() >= min_prefix && !explicit_record_found {
                    let mut fallback_best: HashMap<(String, MemberKind), MemberPresentation> =
                        HashMap::new();
                    for root in &roots {
                        let Some(context) = member_root_contexts.get(root) else {
                            continue;
                        };
                        let (candidates, fallback_truncated) = context
                            .service()
                            .fallback_member_candidates(&prefix, limit)?;
                        owner_incomplete |= fallback_truncated;
                        for candidate in candidates {
                            remember_member(&mut fallback_best, candidate, false, false);
                        }
                    }
                    (fallback_best.into_values().collect(), false, true)
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
                    let detail = member_detail(
                        member.kind,
                        label.as_ref(),
                        presentation.weak_receiver,
                        presentation.ambiguous_owner,
                    );
                    let documentation = member_documentation(
                        member.kind,
                        member.confidence,
                        label.as_ref(),
                        presentation.weak_receiver,
                        presentation.ambiguous_owner,
                    );
                    match member.kind {
                        MemberKind::Field => field_count += 1,
                        MemberKind::Method | MemberKind::StaticMethod => method_count += 1,
                        MemberKind::NestedType => {}
                    }
                    let documentation_data =
                        member
                            .owner_revision_hash
                            .clone()
                            .and_then(|owner_revision_hash| {
                                serde_json::to_value(CompletionDocumentationData::Member {
                                    version: 3,
                                    uri: uri_owned.clone(),
                                    owner_path: member.owner_path.clone(),
                                    signature: member.signature.clone(),
                                    owner_revision_hash,
                                    overlay_epoch: completion_overlay_epoch,
                                    document_version: version,
                                })
                                .ok()
                            });
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
                            data: documentation_data,
                            ..Default::default()
                        },
                    ));
                }
                scored.sort_by(|a, b| {
                    b.0.cmp(&a.0)
                        .then_with(|| a.1.cmp(&b.1))
                        .then_with(|| a.2.cmp(&b.2))
                });
                let is_incomplete = owner_incomplete
                    || member_completion_is_incomplete(resolved_hit, scored.len(), limit);
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
    ambiguous_owner: bool,
}

#[derive(Clone)]
struct MemberRootQueryContext {
    handle: Option<Arc<CallReadHandle>>,
    overlay: Arc<CandidateOverlaySnapshot>,
    current_path: String,
    reach_graph: Option<Arc<crate::reachability::ReachGraph>>,
}

impl MemberRootQueryContext {
    fn service(&self) -> CandidateQueryService<'_> {
        CandidateQueryService::new(
            self.handle.as_deref(),
            self.overlay.as_ref(),
            &self.current_path,
            None,
            self.reach_graph.as_deref(),
        )
    }
}

#[derive(Default)]
struct RootRecordResolution {
    candidates: Vec<(PathBuf, Vec<crate::query::RecordCandidate>)>,
    authoritative: bool,
    incomplete: bool,
    ambiguous: bool,
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
    ambiguous_owner: bool,
) {
    let key = (candidate.name.to_ascii_lowercase(), candidate.kind);
    let presentation = MemberPresentation {
        candidate,
        weak_receiver,
        ambiguous_owner,
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
        .then_with(|| incoming.owner_path.cmp(&current.owner_path))
        .then_with(|| {
            incoming
                .owner_revision_hash
                .cmp(&current.owner_revision_hash)
        })
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
    ambiguous_owner: bool,
) -> String {
    let mut parts = vec![member_kind_label(kind).to_string()];
    if let Some(label) = scope_label {
        parts.push(label.detail.to_string());
    }
    if weak_receiver {
        parts.push("heuristic receiver".to_string());
    }
    if ambiguous_owner {
        parts.push("ambiguous owner".to_string());
    }
    parts.join(" ")
}

fn member_documentation(
    kind: MemberKind,
    confidence: MemberConfidence,
    scope_label: Option<&model::CompletionScopeLabel>,
    weak_receiver: bool,
    ambiguous_owner: bool,
) -> String {
    let scope = scope_label
        .map(|label| label.documentation.as_str())
        .unwrap_or("FossilSense: current member candidate");
    let receiver = if weak_receiver {
        ", heuristic_receiver"
    } else {
        ""
    };
    let owner = if ambiguous_owner {
        ", ambiguous_owner"
    } else {
        ""
    };
    format!(
        "FossilSense: {} member candidate ({}, {}{}{})",
        member_kind_label(kind),
        scope,
        confidence.as_str(),
        receiver,
        owner,
    )
}

fn retain_global_highest_record_tier(
    candidates_by_root: &mut Vec<(PathBuf, Vec<crate::query::RecordCandidate>)>,
) {
    let highest_rank = candidates_by_root
        .iter()
        .flat_map(|(_, candidates)| candidates.iter().map(|candidate| candidate.tier.rank()))
        .max();
    let Some(highest_rank) = highest_rank else {
        return;
    };
    for (_, candidates) in candidates_by_root.iter_mut() {
        candidates.retain(|candidate| candidate.tier.rank() == highest_rank);
    }
    candidates_by_root.retain(|(_, candidates)| !candidates.is_empty());
}

fn member_type_names_for_segment(
    record_candidates_by_root: &[(PathBuf, Vec<crate::query::RecordCandidate>)],
    member_name: &str,
    member_root_contexts: &HashMap<PathBuf, MemberRootQueryContext>,
    scan_remaining: &mut usize,
) -> Result<(Vec<String>, bool)> {
    let mut names = Vec::new();
    let mut truncated = false;
    for (root, candidates) in record_candidates_by_root {
        let Some(highest_rank) = candidates
            .iter()
            .map(|candidate| candidate.tier.rank())
            .max()
        else {
            continue;
        };
        let selected: Vec<_> = candidates
            .iter()
            .filter(|candidate| candidate.tier.rank() == highest_rank)
            .cloned()
            .collect();
        if selected.is_empty() {
            continue;
        }
        let Some(context) = member_root_contexts.get(root) else {
            continue;
        };
        if *scan_remaining == 0 {
            truncated = true;
            break;
        }
        let read = context.service().members_for_records_limited(
            &selected,
            Some(member_name),
            *scan_remaining,
        )?;
        *scan_remaining = scan_remaining.saturating_sub(read.scanned);
        truncated |= read.truncated;
        for member in read.candidates {
            if member.kind == MemberKind::Field && member.name == member_name {
                if let Some(type_name) = member.type_name {
                    names.push(type_name);
                }
            }
        }
    }
    names.sort();
    names.dedup();
    Ok((names, truncated))
}

fn resolve_record_names_across_roots(
    roots: &[PathBuf],
    type_names: &[String],
    member_root_contexts: &HashMap<PathBuf, MemberRootQueryContext>,
) -> Result<RootRecordResolution> {
    const MULTI_ROOT_RECORD_LIMIT: usize = crate::query::TYPE_CANDIDATE_LIMIT * 4;

    let mut combined = RootRecordResolution::default();
    let mut frontier = VecDeque::new();
    let mut strongest_frontier = HashMap::new();
    for type_name in type_names {
        enqueue_type_frontier(
            &mut frontier,
            &mut strongest_frontier,
            type_name.clone(),
            crate::model::ScopeTier::Current,
        );
    }
    let mut candidates_by_root: HashMap<PathBuf, Vec<crate::query::RecordCandidate>> =
        HashMap::new();
    let mut type_queries = 0usize;
    let mut record_count = 0usize;

    'frontier: while let Some((type_name, tier_cap)) = frontier.pop_front() {
        if strongest_frontier.get(&type_name).copied() != Some(tier_cap) {
            continue;
        }
        for root in roots {
            if type_queries >= crate::query::ALIAS_RESOLUTION_MAX_VISITS {
                combined.incomplete = true;
                break 'frontier;
            }
            let Some(context) = member_root_contexts.get(root) else {
                continue;
            };
            type_queries += 1;
            let bundle = context.service().type_candidates(&type_name)?;
            combined.authoritative |= bundle.shadowed_evidence
                || !bundle.records.candidates.is_empty()
                || !bundle.aliases.candidates.is_empty();
            combined.incomplete |= !bundle.records.coverage.permits_uniqueness()
                || !bundle.aliases.coverage.permits_uniqueness();

            let mut records = bundle.records.candidates;
            for resolution in bundle.alias_resolutions {
                combined.ambiguous |=
                    resolution.status == crate::query::AliasResolutionStatus::AmbiguousRecord;
                combined.incomplete |=
                    resolution.status != crate::query::AliasResolutionStatus::UniqueRecord;
                records.extend(resolution.terminal_records);
            }
            for alias in bundle.aliases.candidates {
                let target_name = match alias.target {
                    crate::query::TypeAliasTarget::TypeName(name) => Some(name),
                    crate::query::TypeAliasTarget::NamedRecord { tag, .. } => Some(tag),
                    crate::query::TypeAliasTarget::StableRecord(_) => None,
                };
                if let Some(target_name) = target_name.filter(|name| !name.is_empty()) {
                    let next_tier = if tier_cap.rank() <= alias.tier.rank() {
                        tier_cap
                    } else {
                        alias.tier
                    };
                    enqueue_type_frontier(
                        &mut frontier,
                        &mut strongest_frontier,
                        target_name,
                        next_tier,
                    );
                }
            }

            let root_candidates = candidates_by_root.entry(root.clone()).or_default();
            for mut record in records {
                if tier_cap.rank() < record.tier.rank() {
                    record.tier = tier_cap;
                }
                if let Some(existing) = root_candidates
                    .iter_mut()
                    .find(|candidate| candidate.identity == record.identity)
                {
                    if record.tier.rank() > existing.tier.rank() {
                        *existing = record;
                    }
                    continue;
                }
                if record_count >= MULTI_ROOT_RECORD_LIMIT {
                    combined.incomplete = true;
                    break 'frontier;
                }
                record_count += 1;
                root_candidates.push(record);
            }
        }
    }

    for root in roots {
        if let Some(mut candidates) = candidates_by_root.remove(root) {
            candidates.sort_by(|left, right| {
                right
                    .tier
                    .rank()
                    .cmp(&left.tier.rank())
                    .then_with(|| left.path.cmp(&right.path))
                    .then_with(|| left.name_range.start_byte.cmp(&right.name_range.start_byte))
            });
            if !candidates.is_empty() {
                combined.candidates.push((root.clone(), candidates));
            }
        }
    }
    Ok(combined)
}

fn enqueue_type_frontier(
    frontier: &mut VecDeque<(String, crate::model::ScopeTier)>,
    strongest: &mut HashMap<String, crate::model::ScopeTier>,
    name: String,
    tier: crate::model::ScopeTier,
) {
    if name.is_empty()
        || strongest
            .get(&name)
            .is_some_and(|known| known.rank() >= tier.rank())
    {
        return;
    }
    strongest.insert(name.clone(), tier);
    frontier.push_back((name, tier));
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

fn weak_receiver_matches_record(
    receiver_name: &str,
    record: &crate::query::RecordCandidate,
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
    ) -> crate::query::RecordCandidate {
        let range = crate::call_model::SourceRange {
            start_byte: 0,
            end_byte: 1,
            start: crate::call_model::SourcePosition {
                line: 0,
                character: 0,
            },
            end: crate::call_model::SourcePosition {
                line: 0,
                character: 1,
            },
        };
        crate::query::RecordCandidate {
            identity: crate::query::RecordCandidateIdentity::Persistent(id),
            display_name: display_name.to_string(),
            tag_name: Some(display_name.to_string()),
            typedef_name: None,
            kind: crate::parser::RecordKind::Struct,
            path: format!("{display_name}.hpp"),
            name_range: range,
            body_range: range,
            declaration_range: range,
            declaration_hash: [0; 32],
            range_fidelity: crate::semantic_model::RecordRangeFidelity::AstExact,
            confidence: crate::parser::RecordConfidence::NamedTag,
            signature: format!("struct {display_name}"),
            tier,
            revision: None,
        }
    }

    #[test]
    fn weak_receiver_uses_unique_record_name_correlation_only() {
        let records = [record_candidate(
            "Widget",
            1,
            crate::model::ScopeTier::Reachable,
        )];
        assert_eq!(
            records
                .iter()
                .filter(|record| weak_receiver_matches_record("widget", record))
                .count(),
            1
        );

        let ambiguous = [
            record_candidate("Widget", 1, crate::model::ScopeTier::Reachable),
            record_candidate("Widget", 2, crate::model::ScopeTier::Global),
        ];
        assert_eq!(
            ambiguous
                .iter()
                .filter(|record| weak_receiver_matches_record("widget", record))
                .count(),
            2
        );
    }

    fn member_candidate(
        owner_path: &str,
        tier: crate::model::ScopeTier,
        confidence: MemberConfidence,
    ) -> crate::model::MemberCandidate {
        crate::model::MemberCandidate {
            name: "shared".into(),
            kind: MemberKind::Field,
            signature: "int shared".into(),
            type_name: Some("int".into()),
            tier,
            confidence,
            owner_path: owner_path.into(),
            owner_revision_hash: Some(format!("revision-{owner_path}")),
        }
    }

    #[test]
    fn global_member_merge_is_tier_first_and_root_order_independent() {
        let global = member_candidate(
            "root-a/global.h",
            crate::model::ScopeTier::Global,
            MemberConfidence::InBody,
        );
        let current = member_candidate(
            "root-b/current.h",
            crate::model::ScopeTier::Current,
            MemberConfidence::Heuristic,
        );

        for order in [
            vec![global.clone(), current.clone()],
            vec![current.clone(), global.clone()],
        ] {
            let mut merged = HashMap::new();
            for candidate in order {
                remember_member(&mut merged, candidate, false, false);
            }
            let selected = merged
                .get(&("shared".to_string(), MemberKind::Field))
                .expect("merged member");
            assert_eq!(selected.candidate.owner_path, "root-b/current.h");
            assert_eq!(selected.candidate.tier, crate::model::ScopeTier::Current);
        }
    }
}
