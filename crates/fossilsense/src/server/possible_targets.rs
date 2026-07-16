use super::*;

use crate::call_model::LinkageDomain;
use crate::candidate_service::{CandidateQueryService, DEFAULT_EXACT_NAME_CANDIDATE_LIMIT};
use crate::model::{DefinitionCandidate, ScopeTier};
use crate::query::callables::{ArityCompatibility, CallableVariantGroup, CounterpartEvidence};
use crate::reachability::{OpenReason, ReachScope};
use crate::store::SymbolRecord;

const POSSIBLE_TARGETS_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PossibleTargetsResponse {
    protocol_version: u32,
    name: String,
    items: Vec<PossibleTargetItem>,
    coverage: PossibleTargetsCoverage,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PossibleTargetItem {
    location: Location,
    name: String,
    kind: String,
    role: String,
    scope_tier: String,
    linkage: String,
    guard: Option<String>,
    signature: String,
    reason: String,
    visibility: String,
    source: String,
    confidence: String,
    arity_compatibility: Option<String>,
    pairing_evidence: Option<String>,
    origin: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PossibleTargetsCoverage {
    /// The command deliberately performs bounded exact-name recall. `false`
    /// is reserved for a lexical binding proven without workspace recall.
    bounded: bool,
    limit: usize,
    scanned: usize,
    truncated: bool,
    open: bool,
    open_reason: Option<String>,
    incomplete_reason: Option<String>,
    semantic_generation: u64,
    overlay_epoch: u64,
    resolver_version: u32,
}

impl Backend {
    /// Return the complete operation-neutral variant set available inside the
    /// bounded candidate snapshot. Unlike standard Declaration/Definition,
    /// this path deliberately does not call an operation presentation helper.
    pub(super) async fn possible_targets_command(&self, arg: &Value) -> Option<Value> {
        let (uri, line, character) = possible_targets_position(arg)?;
        let documents = self
            .session
            .documents
            .capture_request_snapshot(Some(&uri))
            .await;
        let overlay_epoch = documents.overlay_epoch;
        let (_version, text) = self
            .document_snapshot_from_request(&uri, &documents)
            .await?;
        let line_text = text.lines().nth(line as usize).unwrap_or_default();
        if includes::parse_include_line(line_text).is_some() {
            return None;
        }
        let word = query::word_at(line_text, character)?;
        if crate::language_builtins::is_language_keyword(&word) {
            return None;
        }

        let root = self.root_for_uri(&uri).await?;
        let current_path = uri_to_path(&uri)
            .and_then(|path| pathing::relative_slash_path(&root, &path).ok())
            .unwrap_or_default();
        let context = self.request_context_for_root(root.clone()).await;
        let semantic_generation = context.engine.semantic_generation.0;
        let cursor = crate::call_model::SourcePosition { line, character };
        let lsp_cursor = tower_lsp::lsp_types::Position { line, character };
        let cursor_byte = query::byte_offset_at(&text, line, character);

        if navigation::label_navigation_syntax_hint(&text, &word, cursor_byte) {
            let label_uri = uri.clone();
            let label_path = current_path.clone();
            let label_text = text.clone();
            let label_word = word.clone();
            match tokio::task::spawn_blocking(move || {
                navigation::label_navigation_location(
                    &label_uri,
                    &label_path,
                    &label_text,
                    &label_word,
                    cursor_byte,
                )
            })
            .await
            {
                Ok(navigation::LabelNavigation::Found(location)) => {
                    return serde_json::to_value(proven_local_response(
                        word,
                        location,
                        "label",
                        "label_namespace",
                        semantic_generation,
                        overlay_epoch,
                    ))
                    .ok();
                }
                Ok(navigation::LabelNavigation::MissingDefinition) => return None,
                Ok(navigation::LabelNavigation::NotLabelSyntax) | Err(_) => {}
            }
        }

        // Lexical shadow is a C-language proof, not a ranking preference. Once
        // it succeeds, workspace same-name symbols are not possible bindings.
        if navigation::ordinary_identifier_navigation_context(line_text, character) {
            let local_uri = uri.clone();
            let local_path = current_path.clone();
            let local_text = text.clone();
            let local_word = word.clone();
            let local = tokio::task::spawn_blocking(move || {
                navigation::local_binding_location(
                    &local_uri,
                    &local_path,
                    &local_text,
                    &local_word,
                    lsp_cursor,
                )
            })
            .await
            .ok()
            .flatten();
            if let Some(location) = local {
                return serde_json::to_value(proven_local_response(
                    word,
                    location,
                    "local_binding",
                    "lexical_binding",
                    semantic_generation,
                    overlay_epoch,
                ))
                .ok();
            }
        }

        let reach_scope: Option<Arc<ReachScope>> = self
            .reach_scope_from_context(&uri, &context)
            .map(|(_, reach)| reach);
        let call_read_handle = context.engine.call_read_handle.clone();
        let reach_graph = context.engine.reach_graph.clone();
        let semantic_epoch = context.engine.semantic_generation;
        let indexed_files = context.engine.indexed_files.clone();
        let overlay = self
            .candidate_overlay_snapshot_from_documents(
                &root,
                semantic_epoch,
                reach_graph.as_deref(),
                indexed_files.as_deref().map(Vec::as_slice),
                documents,
            )
            .await;

        let result = tokio::task::spawn_blocking(move || -> Result<PossibleTargetsResponse> {
            let service = CandidateQueryService::new(
                call_read_handle.as_deref(),
                &overlay,
                &current_path,
                reach_scope.as_deref(),
                reach_graph.as_deref(),
            );
            let call_context = service.complete_call_context_at(cursor)?;
            let callable_set = service.callable_candidates(&word, call_context)?;
            if !callable_set.anchors.is_empty() {
                let items = callable_items(&root, &callable_set.groups, &current_path, cursor_byte);
                let coverage = callable_coverage(
                    &callable_set,
                    service.effective_current_reach(),
                    semantic_generation,
                    overlay_epoch,
                );
                return Ok(PossibleTargetsResponse {
                    protocol_version: POSSIBLE_TARGETS_PROTOCOL_VERSION,
                    name: word,
                    items,
                    coverage,
                });
            }

            let records = service.non_callable_symbols(&word)?;
            let coverage = non_callable_coverage(
                records.len(),
                overlay.has_incomplete_facts(),
                service.effective_current_reach(),
                semantic_generation,
                overlay_epoch,
            );
            let items = non_callable_items(
                &root,
                records,
                &current_path,
                service.effective_current_reach(),
                cursor,
            );
            Ok(PossibleTargetsResponse {
                protocol_version: POSSIBLE_TARGETS_PROTOCOL_VERSION,
                name: word,
                items,
                coverage,
            })
        })
        .await;

        self.unwrap_query("possible targets", result)
            .await
            .and_then(|response| serde_json::to_value(response).ok())
    }
}

fn possible_targets_position(arg: &Value) -> Option<(Url, u32, u32)> {
    let uri = arg
        .get("uri")?
        .as_str()
        .and_then(|raw| Url::parse(raw).ok())?;
    let line = arg.get("line")?.as_u64()?.try_into().ok()?;
    let character = arg.get("character")?.as_u64()?.try_into().ok()?;
    Some((uri, line, character))
}

fn proven_local_response(
    name: String,
    location: Location,
    kind: &str,
    reason: &str,
    semantic_generation: u64,
    overlay_epoch: u64,
) -> PossibleTargetsResponse {
    PossibleTargetsResponse {
        protocol_version: POSSIBLE_TARGETS_PROTOCOL_VERSION,
        name: name.clone(),
        items: vec![PossibleTargetItem {
            location,
            name,
            kind: kind.into(),
            role: "definition".into(),
            scope_tier: "current".into(),
            linkage: "local".into(),
            guard: None,
            signature: String::new(),
            reason: reason.into(),
            visibility: "current_visible".into(),
            source: "current_document".into(),
            confidence: "exact".into(),
            arity_compatibility: None,
            pairing_evidence: None,
            origin: "current_document".into(),
        }],
        coverage: PossibleTargetsCoverage {
            bounded: false,
            limit: 1,
            scanned: 1,
            truncated: false,
            open: false,
            open_reason: None,
            incomplete_reason: None,
            semantic_generation,
            overlay_epoch,
            resolver_version: query::CALLABLE_CANDIDATE_RESOLVER_VERSION,
        },
    }
}

fn callable_items(
    root: &Path,
    groups: &[CallableVariantGroup],
    current_path: &str,
    cursor_byte: usize,
) -> Vec<PossibleTargetItem> {
    groups
        .iter()
        .flat_map(|group| {
            group.variants().filter_map(move |anchor| {
                let candidate = &anchor.candidate;
                let location = candidate_to_location(root, candidate)?;
                Some(PossibleTargetItem {
                    location,
                    name: anchor.anchor.name.clone(),
                    kind: anchor.anchor.kind.as_str().into(),
                    role: anchor.anchor.role.as_str().into(),
                    scope_tier: candidate.tier.as_str().into(),
                    linkage: linkage_label(&anchor.anchor.linkage).into(),
                    guard: anchor.anchor.guard.clone(),
                    signature: anchor.anchor.presentation_signature.clone(),
                    reason: candidate.reason.as_str().into(),
                    visibility: visibility_for_byte(
                        candidate.tier,
                        &anchor.anchor.path,
                        anchor.anchor.name_range.start_byte,
                        current_path,
                        cursor_byte,
                    )
                    .into(),
                    source: candidate.source.clone(),
                    confidence: candidate.confidence.as_str().into(),
                    arity_compatibility: Some(
                        arity_compatibility_label(anchor.arity_compatibility).into(),
                    ),
                    pairing_evidence: Some(pairing_evidence_label(group.counterpart_evidence)),
                    origin: match anchor.origin {
                        query::CandidateOrigin::Base => "indexed",
                        query::CandidateOrigin::Overlay => "overlay",
                    }
                    .into(),
                })
            })
        })
        .collect()
}

fn non_callable_items(
    root: &Path,
    records: Vec<SymbolRecord>,
    current_path: &str,
    scope: Option<&ReachScope>,
    cursor: crate::call_model::SourcePosition,
) -> Vec<PossibleTargetItem> {
    let ranked =
        query::rank_definitions_into_candidates_with_scope(records.clone(), current_path, scope);
    ranked
        .into_iter()
        .filter_map(|candidate| {
            let record = matching_record(&records, &candidate)?;
            let location = candidate_to_location(root, &candidate)?;
            Some(PossibleTargetItem {
                location,
                name: candidate.name.clone(),
                kind: candidate.kind.clone(),
                role: candidate.role.clone(),
                scope_tier: candidate.tier.as_str().into(),
                linkage: non_callable_linkage(record).into(),
                guard: record.guard.clone(),
                signature: record.signature.clone(),
                reason: candidate.reason.as_str().into(),
                visibility: visibility_for_position(
                    candidate.tier,
                    &candidate.path,
                    (candidate.range.start_line, candidate.range.start_col),
                    current_path,
                    (cursor.line, cursor.character),
                )
                .into(),
                source: candidate.source.clone(),
                confidence: candidate.confidence.as_str().into(),
                arity_compatibility: None,
                pairing_evidence: None,
                origin: if record.id < 0 { "overlay" } else { "indexed" }.into(),
            })
        })
        .collect()
}

fn matching_record<'a>(
    records: &'a [SymbolRecord],
    candidate: &DefinitionCandidate,
) -> Option<&'a SymbolRecord> {
    records.iter().find(|record| {
        record.path == candidate.path
            && record.kind == candidate.kind
            && record.role == candidate.role
            && record.start_line == candidate.range.start_line
            && record.start_col == candidate.range.start_col
    })
}

fn callable_coverage(
    set: &query::CallableCandidateSet,
    origin_scope: Option<&ReachScope>,
    semantic_generation: u64,
    overlay_epoch: u64,
) -> PossibleTargetsCoverage {
    PossibleTargetsCoverage {
        bounded: true,
        limit: DEFAULT_EXACT_NAME_CANDIDATE_LIMIT,
        scanned: set.coverage.scanned,
        truncated: set.coverage.truncated,
        open: set.coverage.scope_open || origin_scope.is_some_and(|scope| scope.open),
        open_reason: origin_scope
            .and_then(|scope| scope.reason)
            .map(open_reason_label),
        incomplete_reason: set.coverage.incomplete_reason.map(incomplete_reason_label),
        semantic_generation,
        overlay_epoch,
        resolver_version: query::CALLABLE_CANDIDATE_RESOLVER_VERSION,
    }
}

fn non_callable_coverage(
    count: usize,
    facts_incomplete: bool,
    origin_scope: Option<&ReachScope>,
    semantic_generation: u64,
    overlay_epoch: u64,
) -> PossibleTargetsCoverage {
    PossibleTargetsCoverage {
        bounded: true,
        limit: DEFAULT_EXACT_NAME_CANDIDATE_LIMIT,
        scanned: count,
        // The compatibility facade currently discards its store truncation
        // bit. Treat a full page as truncated until exact recall exposes it.
        truncated: count >= DEFAULT_EXACT_NAME_CANDIDATE_LIMIT,
        open: origin_scope.is_some_and(|scope| scope.open),
        open_reason: origin_scope
            .and_then(|scope| scope.reason)
            .map(open_reason_label),
        incomplete_reason: facts_incomplete.then(|| "facts_unavailable".into()),
        semantic_generation,
        overlay_epoch,
        resolver_version: query::CALLABLE_CANDIDATE_RESOLVER_VERSION,
    }
}

fn linkage_label(linkage: &LinkageDomain) -> &'static str {
    match linkage {
        LinkageDomain::External => "external",
        LinkageDomain::Internal(_) => "internal",
        LinkageDomain::Unknown => "unknown",
    }
}

fn non_callable_linkage(record: &SymbolRecord) -> &'static str {
    match record.kind.as_str() {
        "global_variable" if has_storage_class(&record.signature, "static") => "internal",
        "global_variable" => "external",
        "macro" => "preprocessor",
        _ => "not_applicable",
    }
}

fn has_storage_class(signature: &str, storage_class: &str) -> bool {
    signature
        .split(|ch: char| !(ch == '_' || ch.is_ascii_alphanumeric()))
        .any(|token| token == storage_class)
}

fn visibility_for_byte(
    tier: ScopeTier,
    path: &str,
    start_byte: usize,
    current_path: &str,
    cursor_byte: usize,
) -> &'static str {
    if path == current_path && start_byte > cursor_byte {
        "not_currently_visible"
    } else {
        visibility_for_tier(tier)
    }
}

fn visibility_for_position(
    tier: ScopeTier,
    path: &str,
    start: (u32, u32),
    current_path: &str,
    cursor: (u32, u32),
) -> &'static str {
    if path == current_path && start > cursor {
        "not_currently_visible"
    } else {
        visibility_for_tier(tier)
    }
}

fn visibility_for_tier(tier: ScopeTier) -> &'static str {
    match tier {
        ScopeTier::Current => "current_visible",
        ScopeTier::Reachable => "reachable",
        ScopeTier::External => "external_first_layer",
        ScopeTier::Unknown => "uncertain",
        ScopeTier::Global => "workspace_fallback",
    }
}

fn arity_compatibility_label(compatibility: ArityCompatibility) -> &'static str {
    match compatibility {
        ArityCompatibility::Compatible => "compatible",
        ArityCompatibility::Unknown => "unknown",
        ArityCompatibility::Incompatible => "incompatible",
    }
}

fn pairing_evidence_label(evidence: CounterpartEvidence) -> String {
    match evidence {
        CounterpartEvidence::Unpaired => "unpaired".into(),
        CounterpartEvidence::IncompleteCoverage => "incomplete_coverage".into(),
        CounterpartEvidence::Ambiguous { candidate_edges } => {
            format!("ambiguous:{candidate_edges}")
        }
        CounterpartEvidence::StrictOneToOne => "strict_one_to_one".into(),
    }
}

fn open_reason_label(reason: OpenReason) -> String {
    match reason {
        OpenReason::UnresolvedInclude => "unresolved_include",
        OpenReason::AmbiguousInclude => "ambiguous_include",
        OpenReason::DepthLimit => "depth_limit",
        OpenReason::NodeLimit => "node_limit",
    }
    .into()
}

fn incomplete_reason_label(reason: query::CandidateIncompleteReason) -> String {
    match reason {
        query::CandidateIncompleteReason::ScanLimit => "scan_limit",
        query::CandidateIncompleteReason::CandidateBudget => "candidate_budget",
        query::CandidateIncompleteReason::TimeBudget => "time_budget",
        query::CandidateIncompleteReason::Cancelled => "cancelled",
        query::CandidateIncompleteReason::FactsUnavailable => "facts_unavailable",
        query::CandidateIncompleteReason::GenerationMismatch => "generation_mismatch",
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_binding_response_is_complete_and_not_workspace_bounded() {
        let uri = Url::parse("file:///workspace/main.c").unwrap();
        let location = Location {
            uri,
            range: tower_lsp::lsp_types::Range::default(),
        };
        let response = proven_local_response(
            "value".into(),
            location,
            "local_binding",
            "lexical_binding",
            7,
            3,
        );
        assert_eq!(response.items.len(), 1);
        assert_eq!(response.items[0].reason, "lexical_binding");
        assert!(!response.coverage.bounded);
        assert!(!response.coverage.truncated);
    }

    #[test]
    fn full_non_callable_page_is_reported_as_bounded_and_truncated() {
        let coverage = non_callable_coverage(DEFAULT_EXACT_NAME_CANDIDATE_LIMIT, false, None, 9, 4);
        assert!(coverage.bounded);
        assert!(coverage.truncated);
        assert_eq!(coverage.limit, DEFAULT_EXACT_NAME_CANDIDATE_LIMIT);
    }

    #[test]
    fn visibility_distinguishes_after_cursor_from_current() {
        assert_eq!(
            visibility_for_byte(ScopeTier::Current, "main.c", 20, "main.c", 10),
            "not_currently_visible"
        );
        assert_eq!(
            visibility_for_byte(ScopeTier::Current, "main.c", 5, "main.c", 10),
            "current_visible"
        );
    }
}
