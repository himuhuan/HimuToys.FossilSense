use std::collections::HashSet;
use std::sync::Arc;

use crate::completion_history::CompletionHistorySnapshot;
use crate::model;
use crate::parser::{FactAvailability, FactGroup, FileSemanticIndex};
use crate::project_context::ProjectKey;
use crate::query::{self, NameTable};
use crate::resolver;
use crate::semantic_model::{CompletionKindHint, SemanticFamily};
use crate::store::views::FallbackCompletionRow;

use super::{
    CandidateEvidence, CandidateSource, CompletionCandidateKind, CompletionIntent,
    CompletionPipelineMetrics, CompletionPrefixRanking, CompletionRankContext, PipelineCandidate,
};

mod providers;
use providers::{
    completion_items_for_current_file_overlay, completion_items_for_indexed_hits,
    completion_items_for_language_builtins, completion_items_for_local_bindings,
    exact_indexed_completion_candidates_for_local_word, set_completion_history_key,
    IndexedCompletionContext,
};

type OrdinaryPipelineCandidate = PipelineCandidate<OrdinaryCompletionPresentation>;

/// A closed primary stage with this many distinct visible names is useful
/// enough to hide workspace/heuristic rescue rows from the default list. A
/// smaller result still admits rescue automatically.
const PRIMARY_COMPLETION_SUFFICIENCY: usize = 8;

#[derive(Clone)]
pub(crate) struct OrdinaryCompletionInput {
    pub prefix: String,
    pub text: Arc<str>,
    pub line: u32,
    pub character: u32,
    pub parsed_document: Option<Arc<FileSemanticIndex>>,
    pub local_words: Arc<HashSet<String>>,
    pub tables: Vec<OrdinaryCompletionNameTable>,
    pub scope: Option<query::CompletionScope>,
    pub active_project_context: Option<ProjectKey>,
    pub prior_pools: Vec<Option<Vec<usize>>>,
    pub intent: CompletionIntent,
    pub history_enabled: bool,
    pub history: CompletionHistorySnapshot,
    pub prefix_bucket: String,
    pub prefix_ranking: CompletionPrefixRanking,
    pub limit: usize,
    pub locality_bonus: i32,
}

#[derive(Clone)]
pub(crate) struct OrdinaryCompletionNameTable {
    pub table: Arc<NameTable>,
    pub overlay_handles: std::collections::HashMap<i64, crate::candidate_service::CandidateHandle>,
    pub fallback_table: Arc<FallbackCompletionNameTable>,
}

impl OrdinaryCompletionNameTable {
    #[cfg(test)]
    fn test(table: Arc<NameTable>) -> Self {
        Self {
            table,
            overlay_handles: std::collections::HashMap::new(),
            fallback_table: Arc::new(FallbackCompletionNameTable::default()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FallbackCompletionName {
    pub name: String,
    pub kind_hint: CompletionKindHint,
    pub detail: Option<String>,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct IndexedFallbackCompletionName {
    value: FallbackCompletionName,
    lower: String,
    semantic_family: SemanticFamily,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FallbackCompletionNameTable {
    entries: Arc<[IndexedFallbackCompletionName]>,
    match_index: Arc<std::collections::HashMap<u32, Vec<usize>>>,
    shadowed_paths: Arc<HashSet<String>>,
    overlay_entries: Arc<[IndexedFallbackCompletionName]>,
}

impl FallbackCompletionNameTable {
    pub(crate) fn build(rows: Vec<FallbackCompletionRow>) -> Self {
        let entries: Vec<_> = rows
            .into_iter()
            .filter_map(|row| {
                let kind_hint = match row.kind_hint {
                    0 => CompletionKindHint::Function,
                    1 => CompletionKindHint::Macro,
                    2 => CompletionKindHint::Type,
                    3 => CompletionKindHint::Object,
                    _ => return None,
                };
                Some((
                    FallbackCompletionName {
                        name: row.name,
                        kind_hint,
                        detail: row.detail,
                        path: row.path,
                    },
                    row.semantic_family,
                ))
            })
            .collect();
        Self::from_family_entries(entries)
    }

    #[cfg(test)]
    fn from_entries(mut entries: Vec<FallbackCompletionName>) -> Self {
        sort_and_dedup_fallback_entries(&mut entries);
        Self::from_family_entries(
            entries
                .into_iter()
                .map(|entry| (entry, SemanticFamily::CFamily))
                .collect(),
        )
    }

    fn from_family_entries(mut entries: Vec<(FallbackCompletionName, SemanticFamily)>) -> Self {
        entries.sort_by(|left, right| fallback_entry_order(&left.0, &right.0));
        entries.dedup();
        let entries: Vec<_> = entries
            .into_iter()
            .map(|(entry, family)| IndexedFallbackCompletionName::new(entry, family))
            .collect();
        let match_index = fallback_match_index(&entries);
        Self {
            entries: entries.into(),
            match_index: Arc::new(match_index),
            shadowed_paths: Arc::new(HashSet::new()),
            overlay_entries: Arc::from([]),
        }
    }

    #[cfg(test)]
    fn matching(&self, prefix: &str, limit: usize) -> Vec<ScoredFallbackCompletionName> {
        self.matching_for_family(prefix, limit, SemanticFamily::CFamily)
    }

    fn matching_for_family(
        &self,
        prefix: &str,
        limit: usize,
        semantic_family: SemanticFamily,
    ) -> Vec<ScoredFallbackCompletionName> {
        if limit == 0 {
            return Vec::new();
        }
        let needle = prefix.to_ascii_lowercase();
        let Some(key) = fallback_match_key(needle.as_bytes()) else {
            return Vec::new();
        };
        let mut scored: Vec<_> = self
            .match_index
            .get(&key)
            .into_iter()
            .flatten()
            .filter_map(|&index| {
                let entry = &self.entries[index];
                (entry.semantic_family == semantic_family
                    && !self.shadowed_paths.contains(&entry.value.path))
                .then(|| score_indexed_fallback(&needle, entry))
                .flatten()
            })
            .chain(
                self.overlay_entries
                    .iter()
                    .filter(|entry| entry.semantic_family == semantic_family)
                    .filter_map(|entry| score_indexed_fallback(&needle, entry)),
            )
            .collect();
        sort_scored_fallbacks(&mut scored);
        scored.truncate(limit);
        scored
    }

    #[cfg(test)]
    pub(crate) fn with_updated_paths(
        &self,
        shadowed_paths: &HashSet<String>,
        overlay_entries: impl IntoIterator<Item = FallbackCompletionName>,
    ) -> Self {
        self.with_updated_family_paths(
            shadowed_paths,
            overlay_entries
                .into_iter()
                .map(|entry| (entry, SemanticFamily::CFamily)),
        )
    }

    pub(crate) fn with_updated_family_paths(
        &self,
        shadowed_paths: &HashSet<String>,
        overlay_entries: impl IntoIterator<Item = (FallbackCompletionName, SemanticFamily)>,
    ) -> Self {
        let mut overlay_entries: Vec<_> = overlay_entries.into_iter().collect();
        overlay_entries.sort_by(|left, right| fallback_entry_order(&left.0, &right.0));
        overlay_entries.dedup();
        Self {
            entries: self.entries.clone(),
            match_index: self.match_index.clone(),
            shadowed_paths: Arc::new(shadowed_paths.clone()),
            overlay_entries: overlay_entries
                .into_iter()
                .map(|(entry, family)| IndexedFallbackCompletionName::new(entry, family))
                .collect::<Vec<_>>()
                .into(),
        }
    }
}

impl IndexedFallbackCompletionName {
    fn new(value: FallbackCompletionName, semantic_family: SemanticFamily) -> Self {
        let lower = value.name.to_ascii_lowercase();
        Self {
            value,
            lower,
            semantic_family,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ScoredFallbackCompletionName {
    score: i32,
    value: FallbackCompletionName,
}

fn fallback_matches<'a>(
    entries: impl Iterator<Item = &'a FallbackCompletionName>,
    prefix: &str,
    limit: usize,
) -> Vec<ScoredFallbackCompletionName> {
    let mut scored: Vec<_> = entries
        .filter_map(|entry| {
            query::completion_word_score(prefix, &entry.name, 0).map(|score| {
                ScoredFallbackCompletionName {
                    score,
                    value: entry.clone(),
                }
            })
        })
        .collect();
    sort_scored_fallbacks(&mut scored);
    scored.truncate(limit);
    scored
}

fn score_indexed_fallback(
    needle: &str,
    entry: &IndexedFallbackCompletionName,
) -> Option<ScoredFallbackCompletionName> {
    query::completion_word_score_lowered(needle, &entry.value.name, &entry.lower, 0).map(|score| {
        ScoredFallbackCompletionName {
            score,
            value: entry.value.clone(),
        }
    })
}

fn sort_scored_fallbacks(entries: &mut [ScoredFallbackCompletionName]) {
    entries.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.value.name.cmp(&right.value.name))
            .then_with(|| left.value.path.cmp(&right.value.path))
    });
}

#[cfg(test)]
fn sort_and_dedup_fallback_entries(entries: &mut Vec<FallbackCompletionName>) {
    entries.sort_by(fallback_entry_order);
    entries.dedup();
}

fn fallback_entry_order(
    left: &FallbackCompletionName,
    right: &FallbackCompletionName,
) -> std::cmp::Ordering {
    left.name
        .to_ascii_lowercase()
        .cmp(&right.name.to_ascii_lowercase())
        .then_with(|| left.name.cmp(&right.name))
        .then_with(|| left.path.cmp(&right.path))
        .then_with(|| {
            completion_hint_rank(left.kind_hint).cmp(&completion_hint_rank(right.kind_hint))
        })
        .then_with(|| left.detail.cmp(&right.detail))
}

fn completion_hint_rank(kind: CompletionKindHint) -> u8 {
    match kind {
        CompletionKindHint::Function => 0,
        CompletionKindHint::Macro => 1,
        CompletionKindHint::Type => 2,
        CompletionKindHint::Object => 3,
    }
}

/// Index the first 1-2 bytes of every identifier boundary and every 3-byte
/// substring. Fallback scoring only accepts boundary substrings for short
/// prefixes and contiguous substrings thereafter, so this is a complete cold-
/// request candidate set without rescoring the entire table.
fn fallback_match_index(
    entries: &[IndexedFallbackCompletionName],
) -> std::collections::HashMap<u32, Vec<usize>> {
    let mut index: std::collections::HashMap<u32, Vec<usize>> = std::collections::HashMap::new();
    for (entry_index, entry) in entries.iter().enumerate() {
        let original = entry.value.name.as_bytes();
        let lower = entry.lower.as_bytes();
        let mut keys = HashSet::new();
        for start in 0..lower.len() {
            if fallback_name_boundary(original, start) {
                for width in 1..=2 {
                    if let Some(key) =
                        fallback_match_key(lower.get(start..start + width).unwrap_or_default())
                    {
                        keys.insert(key);
                    }
                }
            }
        }
        for bytes in lower.windows(3) {
            if let Some(key) = fallback_match_key(bytes) {
                keys.insert(key);
            }
        }
        for key in keys {
            index.entry(key).or_default().push(entry_index);
        }
    }
    index
}

fn fallback_match_key(bytes: &[u8]) -> Option<u32> {
    let width = bytes.len().min(3);
    if width == 0 || !bytes[..width].iter().all(u8::is_ascii) {
        return None;
    }
    let mut key = (width as u32) << 24;
    for (offset, byte) in bytes[..width].iter().enumerate() {
        key |= (*byte as u32) << (offset * 8);
    }
    Some(key)
}

fn fallback_name_boundary(bytes: &[u8], index: usize) -> bool {
    if index == 0 {
        return true;
    }
    let previous = bytes[index - 1];
    let current = bytes[index];
    (previous == b'_' && current != b'_')
        || (previous.is_ascii_lowercase() && current.is_ascii_uppercase())
        || (previous.is_ascii_alphabetic() && current.is_ascii_digit())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OrdinaryCompletionOutput {
    pub items: Vec<OrdinaryCompletionItem>,
    pub new_pools: Vec<Vec<usize>>,
    pub metrics: CompletionPipelineMetrics,
    pub recall_ms: u128,
    pub merge_rank_ms: u128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OrdinaryCompletionItem {
    pub label: String,
    pub kind: OrdinaryCompletionKind,
    pub detail: Option<String>,
    pub documentation: Option<String>,
    pub initial_sort_text: Option<String>,
    pub evidence: CandidateEvidence,
    pub documentation_target: Option<OrdinaryCompletionDocumentationTarget>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OrdinaryCompletionDocumentationTarget {
    Declaration {
        table_index: usize,
        declaration_id: i64,
        declaration_name: String,
    },
    CurrentDocument {
        start_line: u32,
    },
    Candidate {
        table_index: usize,
        handle: crate::candidate_service::CandidateHandle,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OrdinaryCompletionPresentation {
    kind: OrdinaryCompletionKind,
    detail: Option<String>,
    documentation: Option<String>,
    initial_sort_text: Option<String>,
    documentation_target: Option<OrdinaryCompletionDocumentationTarget>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OrdinaryCompletionKind {
    Text,
    Keyword,
    Function,
    Macro,
    Type,
    Variable,
    EnumConstant,
}

pub(crate) fn complete_ordinary_identifier(
    input: OrdinaryCompletionInput,
) -> OrdinaryCompletionOutput {
    let recall_started = std::time::Instant::now();
    let open_reason = input.scope.as_ref().and_then(|scope| scope.reach.reason);
    let mut candidates: Vec<OrdinaryPipelineCandidate> = Vec::new();
    let mut new_pools: Vec<Vec<usize>> = Vec::with_capacity(input.tables.len());
    let mut recall_channels = query::CompletionRecallMetrics::default();
    let semantic_family = input
        .parsed_document
        .as_ref()
        .map(|index| index.language.semantic_family())
        .or_else(|| {
            input.scope.as_ref().and_then(|scope| {
                scope.current_path.as_deref().map(|path| {
                    crate::config::SourceLanguage::default_for_path(std::path::Path::new(path))
                        .semantic_family()
                })
            })
        })
        .unwrap_or(SemanticFamily::CFamily);

    for (idx, table) in input.tables.iter().enumerate() {
        // A manual/automatic key belongs to exactly one workspace root. Only
        // that root receives the additional same-project recall budget; other
        // tables must keep their baseline cap instead of admitting unrelated
        // tail candidates in multi-root workspaces.
        let table_project_context = input
            .active_project_context
            .as_ref()
            .filter(|key| table.table.project_indices(key).is_some());
        let quotas = if table_project_context.is_some() {
            query::CompletionRecallQuotas::with_project_context(input.limit)
        } else {
            query::CompletionRecallQuotas::default_for_completion_limit(input.limit)
        };
        let prior = input.prior_pools.get(idx).and_then(|pool| pool.as_deref());
        let (mut hits, pool, metrics) = table
            .table
            .search_completion_recall_pooled_with_project_for_family(
                &input.prefix,
                quotas,
                input.scope.as_ref(),
                table_project_context,
                prior,
                semantic_family,
            );
        if input.parsed_document.is_some() {
            // The parsed current-document overlay below is source-order aware.
            // Suppress the positionless durable `Current` projection so a
            // declaration after the cursor cannot regain the strongest tier.
            hits.retain(|hit| hit.tier != model::ScopeTier::Current);
        }
        recall_channels.merge_from(metrics);
        new_pools.push(pool);
        candidates.extend(completion_items_for_indexed_hits(
            hits,
            IndexedCompletionContext {
                table_index: idx,
                overlay_handles: &table.overlay_handles,
                active_project_context: table_project_context,
                open_reason,
            },
        ));
    }

    let local_binding_hits = input
        .parsed_document
        .as_ref()
        .map(|index| {
            let request_facts = index.request_facts();
            let local_bindings = match index.fact_availability(FactGroup::LocalBindings) {
                FactAvailability::Available => request_facts.local_bindings,
                FactAvailability::NotRequested | FactAvailability::Unavailable(_) => &[],
            };
            query::local_completion_candidates(
                local_bindings,
                &input.text,
                input.line,
                input.character,
                &input.prefix,
                input.limit,
            )
        })
        .unwrap_or_default();
    candidates.extend(completion_items_for_local_bindings(
        local_binding_hits,
        &input.text,
    ));

    let current_file_overlay_hits = input
        .parsed_document
        .as_ref()
        .map(|index| {
            query::current_file_overlay_candidates(
                index,
                &input.text,
                input.line,
                input.character,
                &input.prefix,
                input.limit,
            )
        })
        .unwrap_or_default();
    let current_file_text_overlay_names: HashSet<String> = current_file_overlay_hits
        .iter()
        .filter(|hit| !hit.semantic || hit.detail.as_deref() == Some("text"))
        .map(|hit| hit.name.clone())
        .collect();
    let current_table_index = (!input.tables.is_empty()).then_some(0);
    let use_canonical_overlay_detail = input
        .tables
        .first()
        .is_some_and(|table| table.table.len() > 0);
    candidates.extend(completion_items_for_current_file_overlay(
        current_file_overlay_hits,
        &input.text,
        input.parsed_document.as_deref(),
        current_table_index,
        use_canonical_overlay_detail,
    ));
    candidates.extend(completion_items_for_language_builtins(&input.prefix));

    let mut fallback_names = Vec::new();
    for table in &input.tables {
        fallback_names.extend(table.fallback_table.matching_for_family(
            &input.prefix,
            input.limit,
            semantic_family,
        ));
    }
    if let Some(parsed) = input.parsed_document.as_ref() {
        let current: Vec<_> = parsed
            .fallback_completions
            .iter()
            .map(|fact| FallbackCompletionName {
                name: fact.name.clone(),
                kind_hint: fact.kind_hint,
                detail: fact.detail.clone(),
                path: String::new(),
            })
            .collect();
        fallback_names.extend(fallback_matches(current.iter(), &input.prefix, input.limit));
    }
    for fallback in fallback_names {
        let score = fallback.score;
        let fallback = fallback.value;
        let mut evidence = CandidateEvidence::new(
            CandidateSource::LexicalFallback,
            model::ScopeTier::Global,
            model::ResolutionConfidence::Fallback,
            score,
        );
        evidence.kind = match fallback.kind_hint {
            CompletionKindHint::Function => CompletionCandidateKind::Function,
            CompletionKindHint::Macro => CompletionCandidateKind::Macro,
            CompletionKindHint::Type => CompletionCandidateKind::Type,
            CompletionKindHint::Object => CompletionCandidateKind::Variable,
        };
        candidates.push(OrdinaryPipelineCandidate::new(
            fallback.name,
            evidence,
            OrdinaryCompletionPresentation {
                kind: ordinary_kind_from_hint(fallback.kind_hint),
                detail: fallback.detail,
                documentation: None,
                initial_sort_text: None,
                documentation_target: None,
            },
        ));
    }

    for word in input.local_words.iter() {
        if word == &input.prefix {
            continue;
        }
        let Some(word_score) =
            query::completion_word_score(&input.prefix, word, input.locality_bonus)
        else {
            continue;
        };
        let tier = model::ScopeTier::Global;
        let (confidence, _reason) = resolver::confidence_reason_for(tier, false, None);
        let sort_text = format!("{:08}", 100_000_000 - word_score);
        let mut exact_indexed = Vec::new();
        for (table_index, table) in input.tables.iter().enumerate() {
            exact_indexed.extend(exact_indexed_completion_candidates_for_local_word(
                table.table.as_ref(),
                word,
                word_score,
                input.scope.as_ref(),
                input.limit,
                semantic_family,
                IndexedCompletionContext {
                    table_index,
                    overlay_handles: &table.overlay_handles,
                    active_project_context: input.active_project_context.as_ref(),
                    open_reason,
                },
            ));
        }
        if !exact_indexed.is_empty() {
            candidates.extend(exact_indexed);
            continue;
        }
        if current_file_text_overlay_names.contains(word.as_str()) {
            continue;
        }
        let mut evidence =
            CandidateEvidence::new(CandidateSource::LocalWord, tier, confidence, word_score);
        evidence.kind = CompletionCandidateKind::Text;
        set_completion_history_key(&mut evidence, word);
        candidates.push(OrdinaryPipelineCandidate::new(
            word.clone(),
            evidence,
            OrdinaryCompletionPresentation {
                kind: OrdinaryCompletionKind::Text,
                detail: None,
                documentation: None,
                initial_sort_text: Some(sort_text),
                documentation_target: None,
            },
        ));
    }

    suppress_rescue_when_primary_is_sufficient(&mut candidates, input.scope.as_ref(), input.limit);

    let recall_ms = recall_started.elapsed().as_millis();
    let merge_rank_started = std::time::Instant::now();
    let mut output = super::run_evidence_aware_pipeline_with_context(
        candidates,
        input.limit,
        CompletionRankContext {
            intent: input.intent,
            history_enabled: input.history_enabled,
            history: input.history,
            prefix_bucket: input.prefix_bucket,
            prefix: input.prefix,
            prefix_ranking: input.prefix_ranking,
        },
    );
    output.metrics.recall_channels = recall_channels;
    let merge_rank_ms = merge_rank_started.elapsed().as_millis();
    let items = output
        .items
        .into_iter()
        .map(|candidate| {
            let payload = candidate.payload;
            OrdinaryCompletionItem {
                label: candidate.name,
                kind: payload.kind,
                detail: payload.detail,
                documentation: payload.documentation,
                initial_sort_text: payload.initial_sort_text,
                evidence: candidate.evidence,
                documentation_target: payload.documentation_target,
            }
        })
        .collect();

    OrdinaryCompletionOutput {
        items,
        new_pools,
        metrics: output.metrics,
        recall_ms,
        merge_rank_ms,
    }
}

fn ordinary_kind_from_hint(kind: CompletionKindHint) -> OrdinaryCompletionKind {
    match kind {
        CompletionKindHint::Function => OrdinaryCompletionKind::Function,
        CompletionKindHint::Macro => OrdinaryCompletionKind::Macro,
        CompletionKindHint::Type => OrdinaryCompletionKind::Type,
        CompletionKindHint::Object => OrdinaryCompletionKind::Variable,
    }
}

fn suppress_rescue_when_primary_is_sufficient(
    candidates: &mut Vec<OrdinaryPipelineCandidate>,
    scope: Option<&query::CompletionScope>,
    limit: usize,
) {
    if scope.is_none_or(|scope| scope.reach.open) {
        return;
    }
    let required = PRIMARY_COMPLETION_SUFFICIENCY.min(limit);
    if required == 0 {
        return;
    }
    let primary_names: HashSet<&str> = candidates
        .iter()
        .filter(|candidate| is_primary_completion_evidence(candidate.evidence))
        .map(|candidate| candidate.name.as_str())
        .collect();
    if primary_names.len() < required {
        return;
    }
    candidates.retain(|candidate| !is_rescue_completion_evidence(candidate.evidence));
}

fn is_primary_completion_evidence(evidence: CandidateEvidence) -> bool {
    match evidence.primary_source {
        CandidateSource::LocalBinding
        | CandidateSource::CurrentFileOverlay
        | CandidateSource::LanguageBuiltin => true,
        CandidateSource::Indexed => matches!(
            evidence.tier,
            model::ScopeTier::Current | model::ScopeTier::Reachable | model::ScopeTier::External
        ),
        CandidateSource::LocalWord => false,
        CandidateSource::LexicalFallback => false,
    }
}

fn is_rescue_completion_evidence(evidence: CandidateEvidence) -> bool {
    evidence.primary_source == CandidateSource::LocalWord
        || evidence.primary_source == CandidateSource::LexicalFallback
        || (evidence.primary_source == CandidateSource::Indexed
            && matches!(
                evidence.tier,
                model::ScopeTier::Unknown | model::ScopeTier::Global
            ))
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;
    use std::sync::Arc;

    use crate::completion::{CandidateSource, CompletionIntent, CompletionPrefixRanking};
    use crate::completion_history::CompletionHistorySnapshot;
    use crate::model::ScopeTier;
    use crate::parser;
    use crate::project_context::{ProjectContext, ProjectContextIndex, ProjectKey};
    use crate::query::{CompletionScope, NameTable, COMPLETION_LIMIT, COMPLETION_LOCALITY_BONUS};
    use crate::reachability::{OpenReason, ReachScope};
    use crate::semantic_model::{SemanticDeclarationKind, SemanticDeclarationRole};
    use crate::store::views::DeclarationNameRow;

    use super::{
        complete_ordinary_identifier, fallback_matches, FallbackCompletionName,
        FallbackCompletionNameTable, OrdinaryCompletionDocumentationTarget,
        OrdinaryCompletionInput, OrdinaryCompletionKind, OrdinaryCompletionNameTable,
        PRIMARY_COMPLETION_SUFFICIENCY,
    };

    fn text_and_position(marked: &str) -> (String, u32, u32) {
        let marker = "/*cursor*/";
        let cursor_byte = marked.find(marker).expect("cursor marker");
        let text = marked.replacen(marker, "", 1);
        let before = &text[..cursor_byte];
        let line = before.bytes().filter(|byte| *byte == b'\n').count() as u32;
        let line_start = before.rfind('\n').map_or(0, |index| index + 1);
        let character = before[line_start..]
            .chars()
            .map(|ch| ch.len_utf16() as u32)
            .sum();
        (text, line, character)
    }

    fn indexed_duplicate_winner(rows: Vec<DeclarationNameRow>) -> (i64, parser::SymbolRole) {
        let output = complete_ordinary_identifier(OrdinaryCompletionInput {
            prefix: "shared_api".to_string(),
            text: Arc::from("shared_api"),
            line: 0,
            character: 10,
            parsed_document: None,
            local_words: Arc::new(HashSet::new()),
            tables: vec![OrdinaryCompletionNameTable {
                table: Arc::new(
                    NameTable::build_from_declaration_name_rows_with_project_context(rows, None),
                ),
                overlay_handles: HashMap::new(),
                fallback_table: Arc::new(FallbackCompletionNameTable::default()),
            }],
            scope: None,
            active_project_context: None,
            prior_pools: vec![None],
            intent: CompletionIntent::default(),
            history_enabled: false,
            history: CompletionHistorySnapshot::default(),
            prefix_bucket: "shared_api".to_string(),
            prefix_ranking: CompletionPrefixRanking::Strict,
            limit: 10,
            locality_bonus: COMPLETION_LOCALITY_BONUS,
        });
        assert_eq!(output.items.len(), 1);
        let item = &output.items[0];
        let declaration_id = match item.documentation_target {
            Some(OrdinaryCompletionDocumentationTarget::Declaration { declaration_id, .. }) => {
                declaration_id
            }
            ref other => panic!("expected indexed documentation target, got {other:?}"),
        };
        (declaration_id, item.evidence.role.expect("indexed role"))
    }

    #[test]
    fn indexed_header_declaration_stably_wins_over_source_definition() {
        let header = DeclarationNameRow {
            id: 11,
            name: "shared_api".to_string(),
            external: false,
            path: "include/shared.h".to_string(),
            declaration_kind: SemanticDeclarationKind::Function,
            role: SemanticDeclarationRole::Declaration,
            semantic_family: crate::config::SemanticFamily::CFamily,
            directly_included: false,
        };
        let source = DeclarationNameRow {
            id: 22,
            name: "shared_api".to_string(),
            external: false,
            path: "src/shared.c".to_string(),
            declaration_kind: SemanticDeclarationKind::Function,
            role: SemanticDeclarationRole::Definition,
            semantic_family: crate::config::SemanticFamily::CFamily,
            directly_included: false,
        };

        for rows in [
            vec![source.clone(), header.clone()],
            vec![header.clone(), source.clone()],
        ] {
            assert_eq!(
                indexed_duplicate_winner(rows),
                (11, parser::SymbolRole::Declaration)
            );
        }
    }

    #[test]
    fn go_completion_recall_excludes_c_family_before_ranking() {
        let (text, line, character) =
            text_and_position("package main\nfunc main() {\n    SharedO/*cursor*/\n}\n");
        let parsed = Arc::new(parser::parse(&PathBuf::from("src/main.go"), &text));
        let rows = vec![
            DeclarationNameRow {
                id: 11,
                name: "SharedOpen".to_string(),
                external: false,
                path: "src/shared.c".to_string(),
                declaration_kind: SemanticDeclarationKind::Function,
                role: SemanticDeclarationRole::Definition,
                semantic_family: crate::config::SemanticFamily::CFamily,
                directly_included: false,
            },
            DeclarationNameRow {
                id: 22,
                name: "SharedOpen".to_string(),
                external: false,
                path: "src/shared.go".to_string(),
                declaration_kind: SemanticDeclarationKind::Function,
                role: SemanticDeclarationRole::Definition,
                semantic_family: crate::config::SemanticFamily::Go,
                directly_included: false,
            },
        ];
        let output = complete_ordinary_identifier(OrdinaryCompletionInput {
            prefix: "SharedO".to_string(),
            text: Arc::from(text),
            line,
            character,
            parsed_document: Some(parsed),
            local_words: Arc::new(HashSet::new()),
            tables: vec![OrdinaryCompletionNameTable {
                table: Arc::new(
                    NameTable::build_from_declaration_name_rows_with_project_context(rows, None),
                ),
                overlay_handles: HashMap::new(),
                fallback_table: Arc::new(FallbackCompletionNameTable::default()),
            }],
            scope: None,
            active_project_context: None,
            prior_pools: vec![None],
            intent: CompletionIntent::default(),
            history_enabled: false,
            history: CompletionHistorySnapshot::default(),
            prefix_bucket: "sharedo".to_string(),
            prefix_ranking: CompletionPrefixRanking::Strict,
            limit: 10,
            locality_bonus: COMPLETION_LOCALITY_BONUS,
        });

        let indexed = output
            .items
            .iter()
            .find(|item| item.label == "SharedOpen")
            .expect("Go indexed completion");
        assert!(matches!(
            indexed.documentation_target,
            Some(OrdinaryCompletionDocumentationTarget::Declaration {
                declaration_id: 22,
                ..
            })
        ));
    }

    #[test]
    fn fallback_completion_table_filters_semantic_family_before_limit() {
        let table = FallbackCompletionNameTable::from_family_entries(vec![
            (
                FallbackCompletionName {
                    name: "SharedC".to_string(),
                    kind_hint: crate::semantic_model::CompletionKindHint::Function,
                    detail: None,
                    path: "broken.c".to_string(),
                },
                crate::config::SemanticFamily::CFamily,
            ),
            (
                FallbackCompletionName {
                    name: "SharedGo".to_string(),
                    kind_hint: crate::semantic_model::CompletionKindHint::Function,
                    detail: None,
                    path: "broken.go".to_string(),
                },
                crate::config::SemanticFamily::Go,
            ),
        ]);

        assert_eq!(
            table
                .matching_for_family("Shared", 1, crate::config::SemanticFamily::Go)
                .into_iter()
                .map(|entry| entry.value.name)
                .collect::<Vec<_>>(),
            vec!["SharedGo"]
        );
    }

    #[test]
    fn lexical_fallback_is_below_local_words_and_has_no_resolve_target() {
        let fallback_table =
            FallbackCompletionNameTable::from_entries(vec![FallbackCompletionName {
                name: "fallback_regex".to_string(),
                kind_hint: crate::semantic_model::CompletionKindHint::Function,
                detail: Some("regex guess".to_string()),
                path: "broken.c".to_string(),
            }]);
        let output = complete_ordinary_identifier(OrdinaryCompletionInput {
            prefix: "fallback_".to_string(),
            text: Arc::from("fallback_"),
            line: 0,
            character: 9,
            parsed_document: None,
            local_words: Arc::new(HashSet::from(["fallback_local".to_string()])),
            tables: vec![OrdinaryCompletionNameTable {
                table: Arc::new(NameTable::build(Vec::new())),
                overlay_handles: HashMap::new(),
                fallback_table: Arc::new(fallback_table),
            }],
            scope: None,
            active_project_context: None,
            prior_pools: vec![None],
            intent: CompletionIntent::default(),
            history_enabled: false,
            history: CompletionHistorySnapshot::default(),
            prefix_bucket: "fallback_".to_string(),
            prefix_ranking: CompletionPrefixRanking::Strict,
            limit: 10,
            locality_bonus: COMPLETION_LOCALITY_BONUS,
        });

        let local = output
            .items
            .iter()
            .position(|item| item.label == "fallback_local")
            .expect("local word");
        let fallback = output
            .items
            .iter()
            .position(|item| item.label == "fallback_regex")
            .expect("fallback completion");
        assert!(local < fallback);
        assert_eq!(
            output.items[fallback].evidence.primary_source,
            CandidateSource::LexicalFallback
        );
        assert!(output.items[fallback].documentation_target.is_none());
        assert!(output.items[fallback].documentation.is_none());
    }

    #[test]
    fn dirty_fallback_overlay_tombstones_stale_durable_hints() {
        let base = FallbackCompletionNameTable::from_entries(vec![
            FallbackCompletionName {
                name: "stale_guess".to_string(),
                kind_hint: crate::semantic_model::CompletionKindHint::Function,
                detail: None,
                path: "dirty.h".to_string(),
            },
            FallbackCompletionName {
                name: "clean_guess".to_string(),
                kind_hint: crate::semantic_model::CompletionKindHint::Object,
                detail: None,
                path: "clean.h".to_string(),
            },
        ]);
        let updated = base.with_updated_paths(
            &HashSet::from(["dirty.h".to_string()]),
            [FallbackCompletionName {
                name: "current_guess".to_string(),
                kind_hint: crate::semantic_model::CompletionKindHint::Type,
                detail: None,
                path: "dirty.h".to_string(),
            }],
        );
        assert!(
            Arc::ptr_eq(&base.entries, &updated.entries)
                && Arc::ptr_eq(&base.match_index, &updated.match_index),
            "dirty overlays must share the immutable fallback index"
        );
        let names: HashSet<_> = updated
            .matching("guess", 10)
            .into_iter()
            .map(|entry| entry.value.name)
            .collect();
        assert_eq!(
            names,
            HashSet::from(["clean_guess".to_string(), "current_guess".to_string()])
        );
    }

    #[test]
    fn fallback_match_index_is_equivalent_to_full_table_scoring() {
        let table = FallbackCompletionNameTable::from_entries(
            [
                "alpha",
                "AlphaBeta",
                "HTTPServer2",
                "alpha_beta",
                "xAlpha",
                "_privateValue",
                "banana",
            ]
            .into_iter()
            .map(|name| FallbackCompletionName {
                name: name.to_string(),
                kind_hint: crate::semantic_model::CompletionKindHint::Object,
                detail: None,
                path: format!("{name}.h"),
            })
            .collect(),
        );

        for prefix in [
            "a", "al", "h", "ht", "tps", "s", "se", "2", "b", "be", "bet", "pha", "na", "v", "val",
            "zz",
        ] {
            for limit in [1, 3, 100] {
                let indexed = table.matching(prefix, limit);
                let full = fallback_matches(
                    table.entries.iter().map(|entry| &entry.value),
                    prefix,
                    limit,
                );
                assert_eq!(indexed, full, "prefix={prefix:?}, limit={limit}");
            }
        }
    }

    #[test]
    fn service_fixture_captures_metrics_relevant_counts() {
        let (text, line, character) = text_and_position(
            "#include \"reachable.h\"\n\
             #define fs_overlay_macro 1\n\
             typedef int fs_overlay_type;\n\
             int fixture(int fs_param) {\n\
                 int fs_local_value;\n\
                 fs_text_word();\n\
                 fs/*cursor*/\n\
             }\n",
        );
        let parsed = Arc::new(parser::parse(&PathBuf::from("src/main.c"), &text));
        let local_words = Arc::new(crate::completion_words::extract_words(&text));
        let table = Arc::new(NameTable::build_with_paths(vec![
            (
                1,
                "fs_reachable_index".to_string(),
                false,
                "reachable.h".to_string(),
                "function".to_string(),
                false,
            ),
            (
                2,
                "fs_external_index".to_string(),
                true,
                "sdk/external.h".to_string(),
                "type".to_string(),
                true,
            ),
            (
                3,
                "fs_unknown_index".to_string(),
                false,
                "ambiguous/unknown.h".to_string(),
                "enum_constant".to_string(),
                false,
            ),
            (
                4,
                "fs_global_index".to_string(),
                false,
                "global.c".to_string(),
                "macro".to_string(),
                false,
            ),
        ]));
        let scope = CompletionScope {
            current_path: Some("src/main.c".to_string()),
            direct_external_files: HashSet::from(["sdk/external.h".to_string()]),
            reach: ReachScope {
                files: HashSet::from(["src/main.c".to_string(), "reachable.h".to_string()]),
                heuristic_files: Default::default(),
                open: true,
                reason: Some(OpenReason::AmbiguousInclude),
            },
        };

        let line_text = text.lines().nth(line as usize).unwrap_or_default();
        let intent = crate::completion::classify_completion_intent(line_text, character, "fs");

        let output = complete_ordinary_identifier(OrdinaryCompletionInput {
            prefix: "fs".to_string(),
            text: text.into(),
            line,
            character,
            parsed_document: Some(parsed),
            local_words,
            tables: vec![OrdinaryCompletionNameTable::test(table)],
            scope: Some(scope),
            active_project_context: None,
            prior_pools: vec![None],
            intent,
            history_enabled: true,
            history: CompletionHistorySnapshot::default(),
            prefix_bucket: "fs".to_string(),
            prefix_ranking: CompletionPrefixRanking::Strict,
            limit: COMPLETION_LIMIT,
            locality_bonus: COMPLETION_LOCALITY_BONUS,
        });

        let labels: Vec<_> = output
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        assert_eq!(
            labels,
            vec![
                "fs_param",
                "fs_local_value",
                "fs_overlay_type",
                "fs_overlay_macro",
                "fs_reachable_index",
                "fs_external_index",
                "fs_global_index",
                "fs_unknown_index",
                "fs_text_word",
            ]
        );
        assert_eq!(
            output
                .items
                .iter()
                .find(|item| item.label == "fs_text_word")
                .expect("text fallback")
                .kind,
            OrdinaryCompletionKind::Text
        );
        assert_eq!(output.metrics.input_total, 13);
        assert_eq!(output.metrics.after_dedup_total, 9);
        assert_eq!(output.metrics.returned_total, 9);
        assert_eq!(output.metrics.input_sources.indexed, 4);
        assert_eq!(output.metrics.input_sources.local_binding, 2);
        assert_eq!(output.metrics.input_sources.current_file_overlay, 2);
        assert_eq!(output.metrics.input_sources.local_word, 5);
        assert_eq!(output.metrics.returned_sources.indexed, 4);
        assert_eq!(output.metrics.returned_sources.local_binding, 2);
        assert_eq!(output.metrics.returned_sources.current_file_overlay, 2);
        assert_eq!(output.metrics.returned_sources.local_word, 1);
        assert_eq!(output.metrics.recall_channels.reachable, 1);
        assert_eq!(output.metrics.recall_channels.external, 1);
        assert_eq!(output.metrics.recall_channels.unknown, 2);
        assert_eq!(output.metrics.recall_channels.global, 0);
        assert_eq!(output.metrics.recall_channels.pool_total, 4);
        assert!(output.metrics.history_enabled);
        assert_eq!(output.metrics.history_boosted, 0);
        assert_eq!(output.metrics.final_rank.guarded_low_trust, 1);
        assert_eq!(output.new_pools.len(), 1);
        assert_eq!(output.new_pools[0].len(), 4);
        assert!(output
            .items
            .iter()
            .all(|item| item.evidence.history_key.is_some()));
    }

    #[test]
    fn service_empty_result_still_returns_metrics_for_incomplete_lsp_adapter() {
        let output = complete_ordinary_identifier(OrdinaryCompletionInput {
            prefix: "zz_absent".to_string(),
            text: Arc::from("int main(void) { zz_absent }"),
            line: 0,
            character: 26,
            parsed_document: None,
            local_words: Arc::new(HashSet::new()),
            tables: vec![OrdinaryCompletionNameTable {
                table: Arc::new(NameTable::build_with_paths(Vec::new())),
                overlay_handles: HashMap::new(),
                fallback_table: Arc::new(FallbackCompletionNameTable::default()),
            }],
            scope: None,
            active_project_context: None,
            prior_pools: vec![None],
            intent: CompletionIntent::default(),
            history_enabled: false,
            history: CompletionHistorySnapshot::default(),
            prefix_bucket: "zz".to_string(),
            prefix_ranking: CompletionPrefixRanking::Strict,
            limit: COMPLETION_LIMIT,
            locality_bonus: COMPLETION_LOCALITY_BONUS,
        });

        assert!(output.items.is_empty());
        assert_eq!(output.metrics.input_total, 0);
        assert_eq!(output.metrics.returned_total, 0);
    }

    #[test]
    fn closed_sufficient_primary_stage_hides_rescue_but_open_scope_restores_it() {
        let mut rows = (0..PRIMARY_COMPLETION_SUFFICIENCY)
            .map(|index| {
                (
                    index as i64,
                    format!("api_visible_{index}"),
                    false,
                    "api.h".to_string(),
                    "function".to_string(),
                    false,
                )
            })
            .collect::<Vec<_>>();
        rows.push((
            100,
            "api_workspace_fallback".to_string(),
            false,
            "other.c".to_string(),
            "function".to_string(),
            false,
        ));
        let table = Arc::new(NameTable::build_with_paths(rows));

        let complete = |open| {
            complete_ordinary_identifier(OrdinaryCompletionInput {
                prefix: "api".to_string(),
                text: Arc::from("api"),
                line: 0,
                character: 3,
                parsed_document: None,
                local_words: Arc::new(HashSet::new()),
                tables: vec![OrdinaryCompletionNameTable {
                    table: table.clone(),
                    overlay_handles: HashMap::new(),
                    fallback_table: Arc::new(FallbackCompletionNameTable::default()),
                }],
                scope: Some(CompletionScope {
                    current_path: Some("main.c".to_string()),
                    direct_external_files: HashSet::new(),
                    reach: ReachScope {
                        files: HashSet::from(["main.c".to_string(), "api.h".to_string()]),
                        heuristic_files: HashSet::new(),
                        open,
                        reason: open.then_some(OpenReason::UnresolvedInclude),
                    },
                }),
                active_project_context: None,
                prior_pools: vec![None],
                intent: CompletionIntent::default(),
                history_enabled: false,
                history: CompletionHistorySnapshot::default(),
                prefix_bucket: "api".to_string(),
                prefix_ranking: CompletionPrefixRanking::Strict,
                limit: COMPLETION_LIMIT,
                locality_bonus: COMPLETION_LOCALITY_BONUS,
            })
        };

        let closed = complete(false);
        assert!(closed
            .items
            .iter()
            .all(|item| item.label != "api_workspace_fallback"));
        let open = complete(true);
        assert!(open
            .items
            .iter()
            .any(|item| item.label == "api_workspace_fallback"));
    }

    #[test]
    fn current_file_index_does_not_reintroduce_a_declaration_after_the_cursor() {
        let (text, line, character) = text_and_position(
            "void f(void) { fs/*cursor*/; }\n\
             int fs_later(void);\n",
        );
        let parsed = Arc::new(parser::parse(&PathBuf::from("src/main.c"), &text));
        let table = Arc::new(NameTable::build_with_paths(vec![(
            1,
            "fs_later".to_string(),
            false,
            "src/main.c".to_string(),
            "function".to_string(),
            false,
        )]));
        let scope = CompletionScope {
            current_path: Some("src/main.c".to_string()),
            direct_external_files: Default::default(),
            reach: ReachScope {
                files: HashSet::from(["src/main.c".to_string()]),
                heuristic_files: HashSet::new(),
                open: false,
                reason: None,
            },
        };

        let output = complete_ordinary_identifier(OrdinaryCompletionInput {
            prefix: "fs".to_string(),
            text: text.into(),
            line,
            character,
            parsed_document: Some(parsed),
            local_words: Arc::new(HashSet::new()),
            tables: vec![OrdinaryCompletionNameTable::test(table)],
            scope: Some(scope),
            active_project_context: None,
            prior_pools: vec![None],
            intent: CompletionIntent::default(),
            history_enabled: false,
            history: CompletionHistorySnapshot::default(),
            prefix_bucket: "fs".to_string(),
            prefix_ranking: CompletionPrefixRanking::Strict,
            limit: COMPLETION_LIMIT,
            locality_bonus: COMPLETION_LOCALITY_BONUS,
        });

        assert!(output.items.iter().all(|item| item.label != "fs_later"));
    }

    #[test]
    fn service_adds_static_language_builtin_candidates() {
        for (prefix, expected, expected_kind) in [
            ("str", "struct", None),
            ("si", "size_t", Some(OrdinaryCompletionKind::Type)),
            ("NU", "NULL", Some(OrdinaryCompletionKind::EnumConstant)),
        ] {
            let output = complete_ordinary_identifier(OrdinaryCompletionInput {
                prefix: prefix.to_string(),
                text: Arc::from(prefix),
                line: 0,
                character: prefix.len() as u32,
                parsed_document: None,
                local_words: Arc::new(HashSet::new()),
                tables: vec![OrdinaryCompletionNameTable {
                    table: Arc::new(NameTable::build_with_paths(Vec::new())),
                    overlay_handles: HashMap::new(),
                    fallback_table: Arc::new(FallbackCompletionNameTable::default()),
                }],
                scope: None,
                active_project_context: None,
                prior_pools: vec![None],
                intent: CompletionIntent::default(),
                history_enabled: false,
                history: CompletionHistorySnapshot::default(),
                prefix_bucket: prefix.to_ascii_lowercase(),
                prefix_ranking: CompletionPrefixRanking::Strict,
                limit: COMPLETION_LIMIT,
                locality_bonus: COMPLETION_LOCALITY_BONUS,
            });

            let item = output
                .items
                .iter()
                .find(|item| item.label == expected)
                .unwrap_or_else(|| panic!("{expected} language builtin completion"));
            if let Some(expected_kind) = expected_kind {
                assert_eq!(item.kind, expected_kind);
            }
        }
    }

    #[test]
    fn service_dedups_indexed_size_t_over_language_builtin_fallback() {
        let output = complete_ordinary_identifier(OrdinaryCompletionInput {
            prefix: "si".to_string(),
            text: Arc::from("si"),
            line: 0,
            character: 2,
            parsed_document: None,
            local_words: Arc::new(HashSet::new()),
            tables: vec![OrdinaryCompletionNameTable {
                table: Arc::new(NameTable::build_with_paths(vec![(
                    1,
                    "size_t".to_string(),
                    false,
                    "stddef.h".to_string(),
                    "type".to_string(),
                    false,
                )])),
                overlay_handles: HashMap::new(),
                fallback_table: Arc::new(FallbackCompletionNameTable::default()),
            }],
            scope: None,
            active_project_context: None,
            prior_pools: vec![None],
            intent: CompletionIntent::default(),
            history_enabled: false,
            history: CompletionHistorySnapshot::default(),
            prefix_bucket: "si".to_string(),
            prefix_ranking: CompletionPrefixRanking::Strict,
            limit: COMPLETION_LIMIT,
            locality_bonus: COMPLETION_LOCALITY_BONUS,
        });

        let size_t_items: Vec<_> = output
            .items
            .iter()
            .filter(|item| item.label == "size_t")
            .collect();
        assert_eq!(size_t_items.len(), 1);
        assert_eq!(
            size_t_items[0].evidence.primary_source,
            CandidateSource::Indexed
        );
        assert!(
            output.metrics.input_total > output.metrics.after_dedup_total,
            "static size_t fallback should participate before dedup"
        );
    }

    #[test]
    fn service_ranks_current_local_evidence_above_language_builtins() {
        let (text, line, character) = text_and_position(
            "void fixture(void) {\n\
                 int signal_value;\n\
                 si/*cursor*/\n\
             }\n",
        );
        let parsed = Arc::new(parser::parse(&PathBuf::from("src/main.c"), &text));
        let output = complete_ordinary_identifier(OrdinaryCompletionInput {
            prefix: "si".to_string(),
            text: text.into(),
            line,
            character,
            parsed_document: Some(parsed),
            local_words: Arc::new(HashSet::new()),
            tables: vec![OrdinaryCompletionNameTable {
                table: Arc::new(NameTable::build_with_paths(Vec::new())),
                overlay_handles: HashMap::new(),
                fallback_table: Arc::new(FallbackCompletionNameTable::default()),
            }],
            scope: None,
            active_project_context: None,
            prior_pools: vec![None],
            intent: CompletionIntent::default(),
            history_enabled: false,
            history: CompletionHistorySnapshot::default(),
            prefix_bucket: "si".to_string(),
            prefix_ranking: CompletionPrefixRanking::Strict,
            limit: COMPLETION_LIMIT,
            locality_bonus: COMPLETION_LOCALITY_BONUS,
        });

        let labels: Vec<_> = output
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        let signal_index = labels
            .iter()
            .position(|label| *label == "signal_value")
            .expect("local binding completion");
        let size_index = labels
            .iter()
            .position(|label| *label == "size_t")
            .expect("language builtin type completion");
        assert!(signal_index < size_index);
    }

    #[test]
    fn service_demotes_language_builtins_for_declaration_names() {
        let output = complete_ordinary_identifier(OrdinaryCompletionInput {
            prefix: "si".to_string(),
            text: Arc::from("int si"),
            line: 0,
            character: 6,
            parsed_document: None,
            local_words: Arc::new(HashSet::from(["signal_name".to_string()])),
            tables: vec![OrdinaryCompletionNameTable {
                table: Arc::new(NameTable::build_with_paths(Vec::new())),
                overlay_handles: HashMap::new(),
                fallback_table: Arc::new(FallbackCompletionNameTable::default()),
            }],
            scope: None,
            active_project_context: None,
            prior_pools: vec![None],
            intent: crate::completion::classify_completion_intent("int si", 6, "si"),
            history_enabled: false,
            history: CompletionHistorySnapshot::default(),
            prefix_bucket: "si".to_string(),
            prefix_ranking: CompletionPrefixRanking::Strict,
            limit: COMPLETION_LIMIT,
            locality_bonus: COMPLETION_LOCALITY_BONUS,
        });

        let labels: Vec<_> = output
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        let signal_index = labels
            .iter()
            .position(|label| *label == "signal_name")
            .expect("raw declaration-name candidate");
        let size_index = labels
            .iter()
            .position(|label| *label == "size_t")
            .expect("language builtin type completion");
        assert!(signal_index < size_index);
    }

    #[test]
    fn service_short_prefix_fixture_preserves_representative_candidates() {
        let output = complete_ordinary_identifier(OrdinaryCompletionInput {
            prefix: "fs".to_string(),
            text: Arc::from("fs"),
            line: 0,
            character: 2,
            parsed_document: None,
            local_words: Arc::new(HashSet::new()),
            tables: vec![OrdinaryCompletionNameTable {
                table: Arc::new(NameTable::build_with_paths(vec![
                    (
                        1,
                        "fs_exact_prefix".to_string(),
                        false,
                        "a.c".to_string(),
                        "function".to_string(),
                        false,
                    ),
                    (
                        2,
                        "noise_fs_substring".to_string(),
                        false,
                        "a.c".to_string(),
                        "function".to_string(),
                        false,
                    ),
                    (
                        3,
                        "noisefs_substring".to_string(),
                        false,
                        "a.c".to_string(),
                        "function".to_string(),
                        false,
                    ),
                ])),
                overlay_handles: HashMap::new(),
                fallback_table: Arc::new(FallbackCompletionNameTable::default()),
            }],
            scope: Some(CompletionScope {
                current_path: Some("a.c".to_string()),
                direct_external_files: Default::default(),
                reach: ReachScope {
                    files: HashSet::from(["a.c".to_string()]),
                    heuristic_files: Default::default(),
                    open: false,
                    reason: None,
                },
            }),
            active_project_context: None,
            prior_pools: vec![None],
            intent: CompletionIntent::default(),
            history_enabled: false,
            history: CompletionHistorySnapshot::default(),
            prefix_bucket: "fs".to_string(),
            prefix_ranking: CompletionPrefixRanking::Strict,
            limit: COMPLETION_LIMIT,
            locality_bonus: COMPLETION_LOCALITY_BONUS,
        });

        assert_eq!(
            output
                .items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            vec!["fs_exact_prefix", "noise_fs_substring"]
        );
        assert_eq!(output.items[0].evidence.tier, ScopeTier::Current);
    }

    fn underscore_prefix_ranking_fixture(
        prefix_ranking: CompletionPrefixRanking,
        external_fuzzy_count: usize,
        limit: usize,
    ) -> Vec<String> {
        let mut rows = vec![(
            1,
            "wns_ipc_send".to_string(),
            false,
            "src/wns_ipc.c".to_string(),
            "function".to_string(),
            false,
        )];
        for index in 0..external_fuzzy_count {
            rows.push((
                index as i64 + 2,
                format!("wns__ipc_rsp_init_{index:03}"),
                true,
                format!("sdk/include/wns_{index:03}.h"),
                "function".to_string(),
                true,
            ));
        }
        let output = complete_ordinary_identifier(OrdinaryCompletionInput {
            prefix: "wns_ipc".to_string(),
            text: Arc::from("wns_ipc"),
            line: 0,
            character: 7,
            parsed_document: None,
            local_words: Arc::new(HashSet::new()),
            tables: vec![OrdinaryCompletionNameTable {
                table: Arc::new(NameTable::build_with_paths(rows)),
                overlay_handles: HashMap::new(),
                fallback_table: Arc::new(FallbackCompletionNameTable::default()),
            }],
            scope: None,
            active_project_context: None,
            prior_pools: vec![None],
            intent: CompletionIntent::default(),
            history_enabled: false,
            history: CompletionHistorySnapshot::default(),
            prefix_bucket: "wns_ipc".to_string(),
            prefix_ranking,
            limit,
            locality_bonus: COMPLETION_LOCALITY_BONUS,
        });
        output.items.into_iter().map(|item| item.label).collect()
    }

    #[test]
    fn strict_prefix_default_beats_external_cross_separator_fuzzy_match() {
        let labels = underscore_prefix_ranking_fixture(CompletionPrefixRanking::Strict, 1, 10);
        assert_eq!(
            labels,
            vec!["wns_ipc_send", "wns__ipc_rsp_init_000"],
            "literal global prefix must outrank an External candidate that skips an extra '_'"
        );
    }

    #[test]
    fn scope_first_preserves_external_over_global_legacy_order() {
        let labels = underscore_prefix_ranking_fixture(CompletionPrefixRanking::ScopeFirst, 1, 10);
        assert_eq!(
            labels,
            vec!["wns__ipc_rsp_init_000", "wns_ipc_send"],
            "explicit compatibility mode must retain the existing tier-first result"
        );
    }

    #[test]
    fn dense_external_fuzzy_recall_does_not_truncate_global_strict_prefix() {
        let labels = underscore_prefix_ranking_fixture(CompletionPrefixRanking::Strict, 400, 1);
        assert_eq!(labels, vec!["wns_ipc_send"]);
    }

    fn duplicate_project_fixture() -> (Arc<NameTable>, ProjectKey, ProjectKey) {
        let root_id = "root".to_string();
        let server_key = ProjectKey {
            workspace_root_id: root_id.clone(),
            project_path: "https/server".to_string(),
        };
        let library_key = ProjectKey {
            workspace_root_id: root_id.clone(),
            project_path: "third_party/libxxxx".to_string(),
        };
        let context = |key: ProjectKey, marker: &str| ProjectContext {
            key,
            workspace_name: "workspace".to_string(),
            marker_files: vec![marker.to_string()],
        };
        let projects = ProjectContextIndex::new(
            root_id,
            "workspace".to_string(),
            vec![
                context(server_key.clone(), "Makefile"),
                context(library_key.clone(), "CMakeLists.txt"),
            ],
        );
        let table = NameTable::build_with_paths_and_project_context(
            vec![
                (
                    1,
                    "get_xxx".to_string(),
                    false,
                    "https/server/src/server.h".to_string(),
                    "function".to_string(),
                    false,
                ),
                (
                    2,
                    "get_xxx".to_string(),
                    false,
                    "third_party/libxxxx/src/xxx.h".to_string(),
                    "macro".to_string(),
                    false,
                ),
            ],
            &projects,
        );
        (Arc::new(table), server_key, library_key)
    }

    fn complete_duplicate_fixture(
        table: Arc<NameTable>,
        active_project_context: Option<ProjectKey>,
        scope: Option<CompletionScope>,
    ) -> super::OrdinaryCompletionOutput {
        complete_ordinary_identifier(OrdinaryCompletionInput {
            prefix: "get".to_string(),
            text: Arc::from("get"),
            line: 0,
            character: 3,
            parsed_document: None,
            local_words: Arc::new(HashSet::new()),
            tables: vec![OrdinaryCompletionNameTable::test(table)],
            scope,
            active_project_context,
            prior_pools: vec![None],
            intent: CompletionIntent::default(),
            history_enabled: false,
            history: CompletionHistorySnapshot::default(),
            prefix_bucket: "get".to_string(),
            prefix_ranking: CompletionPrefixRanking::Strict,
            limit: COMPLETION_LIMIT,
            locality_bonus: COMPLETION_LOCALITY_BONUS,
        })
    }

    #[test]
    fn project_context_selects_function_or_macro_presentation_for_duplicate_label() {
        let (table, server_key, library_key) = duplicate_project_fixture();
        let server = complete_duplicate_fixture(table.clone(), Some(server_key), None);
        let library = complete_duplicate_fixture(table, Some(library_key), None);

        assert_eq!(server.items.len(), 1);
        assert_eq!(server.items[0].label, "get_xxx");
        assert_eq!(server.items[0].kind, OrdinaryCompletionKind::Function);
        assert_eq!(server.metrics.project_boosted, 1);
        assert_eq!(library.items.len(), 1);
        assert_eq!(library.items[0].kind, OrdinaryCompletionKind::Macro);
        assert_eq!(
            library.items[0].evidence.kind,
            crate::completion::CompletionCandidateKind::Macro
        );
        assert_eq!(library.metrics.project_boosted, 1);
    }

    #[test]
    fn project_context_promotes_a_comparable_global_name_and_keeps_cross_project_results() {
        let root_id = "root".to_string();
        let selected_key = ProjectKey {
            workspace_root_id: root_id.clone(),
            project_path: "selected".to_string(),
        };
        let projects = ProjectContextIndex::new(
            root_id,
            "workspace".to_string(),
            vec![ProjectContext {
                key: selected_key.clone(),
                workspace_name: "workspace".to_string(),
                marker_files: vec!["Makefile".to_string()],
            }],
        );
        let table = Arc::new(NameTable::build_with_paths_and_project_context(
            vec![
                (
                    1,
                    "api_alpha".to_string(),
                    false,
                    "other/api.c".to_string(),
                    "function".to_string(),
                    false,
                ),
                (
                    2,
                    "api_zebra".to_string(),
                    false,
                    "selected/api.c".to_string(),
                    "function".to_string(),
                    false,
                ),
            ],
            &projects,
        ));

        let output = complete_ordinary_identifier(OrdinaryCompletionInput {
            prefix: "api".to_string(),
            text: Arc::from("api"),
            line: 0,
            character: 3,
            parsed_document: None,
            local_words: Arc::new(HashSet::new()),
            tables: vec![OrdinaryCompletionNameTable::test(table)],
            scope: None,
            active_project_context: Some(selected_key),
            prior_pools: vec![None],
            intent: CompletionIntent::default(),
            history_enabled: false,
            history: CompletionHistorySnapshot::default(),
            prefix_bucket: "api".to_string(),
            prefix_ranking: CompletionPrefixRanking::Strict,
            limit: COMPLETION_LIMIT,
            locality_bonus: COMPLETION_LOCALITY_BONUS,
        });

        assert_eq!(
            output
                .items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            vec!["api_zebra", "api_alpha"]
        );
        assert_eq!(output.metrics.project_boosted, 1);
        assert_eq!(output.metrics.recall_channels.same_project, 1);
    }

    #[test]
    fn project_context_does_not_expand_unrelated_workspace_recall() {
        let root_id = "selected-root".to_string();
        let selected_key = ProjectKey {
            workspace_root_id: root_id.clone(),
            project_path: "selected".to_string(),
        };
        let projects = ProjectContextIndex::new(
            root_id,
            "selected-workspace".to_string(),
            vec![ProjectContext {
                key: selected_key.clone(),
                workspace_name: "selected-workspace".to_string(),
                marker_files: vec!["Makefile".to_string()],
            }],
        );
        let selected_table = Arc::new(NameTable::build_with_paths_and_project_context(
            vec![(
                1,
                "api_selected".to_string(),
                false,
                "selected/api.c".to_string(),
                "function".to_string(),
                false,
            )],
            &projects,
        ));
        let unrelated_table = Arc::new(NameTable::build_with_paths(
            (0..7)
                .map(|index| {
                    (
                        index + 10,
                        format!("api_other_{index}"),
                        false,
                        format!("other/{index}.c"),
                        "function".to_string(),
                        false,
                    )
                })
                .collect(),
        ));

        let output = complete_ordinary_identifier(OrdinaryCompletionInput {
            prefix: "api".to_string(),
            text: Arc::from("api"),
            line: 0,
            character: 3,
            parsed_document: None,
            local_words: Arc::new(HashSet::new()),
            tables: vec![
                OrdinaryCompletionNameTable {
                    table: selected_table,
                    overlay_handles: HashMap::new(),
                    fallback_table: Arc::new(FallbackCompletionNameTable::default()),
                },
                OrdinaryCompletionNameTable {
                    table: unrelated_table,
                    overlay_handles: HashMap::new(),
                    fallback_table: Arc::new(FallbackCompletionNameTable::default()),
                },
            ],
            scope: None,
            active_project_context: Some(selected_key),
            prior_pools: vec![None, None],
            intent: CompletionIntent::default(),
            history_enabled: false,
            history: CompletionHistorySnapshot::default(),
            prefix_bucket: "api".to_string(),
            prefix_ranking: CompletionPrefixRanking::Strict,
            limit: 2,
            locality_bonus: COMPLETION_LOCALITY_BONUS,
        });

        // The selected table contributes one project candidate. The unrelated
        // table retains the baseline 3x-limit cap (six), not the project-aware
        // seven-candidate cap.
        assert_eq!(output.metrics.recall_channels.indexed_returned, 7);
        assert_eq!(output.metrics.recall_channels.same_project, 1);
    }

    #[test]
    fn stronger_reachability_beats_project_tie_break_for_duplicate_presentation() {
        let (table, _server_key, library_key) = duplicate_project_fixture();
        let output = complete_duplicate_fixture(
            table,
            Some(library_key),
            Some(CompletionScope {
                current_path: Some("https/server/src/server.c".to_string()),
                direct_external_files: Default::default(),
                reach: ReachScope {
                    files: HashSet::from([
                        "https/server/src/server.c".to_string(),
                        "https/server/src/server.h".to_string(),
                    ]),
                    heuristic_files: Default::default(),
                    open: false,
                    reason: None,
                },
            }),
        );

        assert_eq!(output.items[0].kind, OrdinaryCompletionKind::Function);
        assert_eq!(output.items[0].evidence.tier, ScopeTier::Reachable);
    }

    #[test]
    fn every_no_project_state_matches_untagged_baseline_items_and_metrics() {
        let (tagged, _, _) = duplicate_project_fixture();
        let untagged = Arc::new(NameTable::build_with_paths(vec![
            (
                1,
                "get_xxx".to_string(),
                false,
                "https/server/src/server.h".to_string(),
                "function".to_string(),
                false,
            ),
            (
                2,
                "get_xxx".to_string(),
                false,
                "third_party/libxxxx/src/xxx.h".to_string(),
                "macro".to_string(),
                false,
            ),
        ]));

        let empty_projects =
            ProjectContextIndex::new("root".to_string(), "workspace".to_string(), Vec::new());
        let no_marker = Arc::new(NameTable::build_with_paths_and_project_context(
            vec![
                (
                    1,
                    "get_xxx".to_string(),
                    false,
                    "https/server/src/server.h".to_string(),
                    "function".to_string(),
                    false,
                ),
                (
                    2,
                    "get_xxx".to_string(),
                    false,
                    "third_party/libxxxx/src/xxx.h".to_string(),
                    "macro".to_string(),
                    false,
                ),
            ],
            &empty_projects,
        ));
        let baseline = complete_duplicate_fixture(untagged.clone(), None, None);
        let cases = [
            ("unspecified", tagged.clone()),
            ("off", tagged),
            ("no-marker", no_marker),
            ("unavailable-model", untagged.clone()),
            ("project-context-disabled-baseline", untagged),
        ];

        for (case, table) in cases {
            let actual = complete_duplicate_fixture(table, None, None);
            assert_eq!(actual.items, baseline.items, "items differ for {case}");
            assert_eq!(
                actual.new_pools, baseline.new_pools,
                "recall pools differ for {case}"
            );
            assert_eq!(
                actual.metrics, baseline.metrics,
                "metrics differ for {case}"
            );
        }
    }
}
