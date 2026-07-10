use std::collections::HashSet;
use std::sync::Arc;

use crate::completion_history::CompletionHistorySnapshot;
use crate::model;
use crate::parser::{FactAvailability, FactGroup, FileSemanticIndex};
use crate::query::{self, NameTable};
use crate::resolver;

use super::{
    CandidateEvidence, CandidateSource, CompletionCandidateKind, CompletionIntent,
    CompletionPipelineMetrics, CompletionRankContext, PipelineCandidate,
};

mod providers;
use providers::{
    completion_items_for_current_file_overlay, completion_items_for_indexed_hits,
    completion_items_for_language_builtins, completion_items_for_local_bindings,
    exact_indexed_completion_candidates_for_local_word, set_completion_history_key,
};

type OrdinaryPipelineCandidate = PipelineCandidate<OrdinaryCompletionPresentation>;

#[derive(Clone)]
pub(crate) struct OrdinaryCompletionInput {
    pub prefix: String,
    pub text: String,
    pub line: u32,
    pub character: u32,
    pub parsed_document: Option<Arc<FileSemanticIndex>>,
    pub local_words: Arc<HashSet<String>>,
    pub tables: Vec<OrdinaryCompletionNameTable>,
    pub scope: Option<query::CompletionScope>,
    pub prior_pools: Vec<Option<Vec<usize>>>,
    pub intent: CompletionIntent,
    pub history_enabled: bool,
    pub history: CompletionHistorySnapshot,
    pub prefix_bucket: String,
    pub limit: usize,
    pub locality_bonus: i32,
}

#[derive(Clone)]
pub(crate) struct OrdinaryCompletionNameTable {
    pub table: Arc<NameTable>,
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OrdinaryCompletionPresentation {
    kind: OrdinaryCompletionKind,
    detail: Option<String>,
    documentation: Option<String>,
    initial_sort_text: Option<String>,
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

    let quotas = query::CompletionRecallQuotas::default_for_completion_limit(input.limit);
    for (idx, table) in input.tables.iter().enumerate() {
        let prior = input.prior_pools.get(idx).and_then(|pool| pool.as_deref());
        let (hits, pool, metrics) = table.table.search_completion_recall_pooled(
            &input.prefix,
            quotas,
            input.scope.as_ref(),
            prior,
        );
        recall_channels.merge_from(metrics);
        new_pools.push(pool);
        candidates.extend(completion_items_for_indexed_hits(hits, open_reason));
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
    candidates.extend(completion_items_for_local_bindings(local_binding_hits));

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
    candidates.extend(completion_items_for_current_file_overlay(
        current_file_overlay_hits,
    ));
    candidates.extend(completion_items_for_language_builtins(&input.prefix));

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
        for table in &input.tables {
            exact_indexed.extend(exact_indexed_completion_candidates_for_local_word(
                table.table.as_ref(),
                word,
                word_score,
                input.scope.as_ref(),
                open_reason,
                input.limit,
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
            },
        ));
    }

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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::Arc;

    use crate::completion::{CandidateSource, CompletionIntent};
    use crate::completion_history::CompletionHistorySnapshot;
    use crate::model::ScopeTier;
    use crate::parser;
    use crate::query::{CompletionScope, NameTable, COMPLETION_LIMIT, COMPLETION_LOCALITY_BONUS};
    use crate::reachability::{OpenReason, ReachScope};

    use super::{
        complete_ordinary_identifier, OrdinaryCompletionInput, OrdinaryCompletionKind,
        OrdinaryCompletionNameTable,
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
            reach: ReachScope {
                files: HashSet::from(["src/main.c".to_string(), "reachable.h".to_string()]),
                open: true,
                reason: Some(OpenReason::AmbiguousInclude),
            },
        };

        let line_text = text.lines().nth(line as usize).unwrap_or_default();
        let intent = crate::completion::classify_completion_intent(line_text, character, "fs");

        let output = complete_ordinary_identifier(OrdinaryCompletionInput {
            prefix: "fs".to_string(),
            text,
            line,
            character,
            parsed_document: Some(parsed),
            local_words,
            tables: vec![OrdinaryCompletionNameTable { table }],
            scope: Some(scope),
            prior_pools: vec![None],
            intent,
            history_enabled: true,
            history: CompletionHistorySnapshot::default(),
            prefix_bucket: "fs".to_string(),
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
            text: "int main(void) { zz_absent }".to_string(),
            line: 0,
            character: 26,
            parsed_document: None,
            local_words: Arc::new(HashSet::new()),
            tables: vec![OrdinaryCompletionNameTable {
                table: Arc::new(NameTable::build_with_paths(Vec::new())),
            }],
            scope: None,
            prior_pools: vec![None],
            intent: CompletionIntent::default(),
            history_enabled: false,
            history: CompletionHistorySnapshot::default(),
            prefix_bucket: "zz".to_string(),
            limit: COMPLETION_LIMIT,
            locality_bonus: COMPLETION_LOCALITY_BONUS,
        });

        assert!(output.items.is_empty());
        assert_eq!(output.metrics.input_total, 0);
        assert_eq!(output.metrics.returned_total, 0);
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
                text: prefix.to_string(),
                line: 0,
                character: prefix.len() as u32,
                parsed_document: None,
                local_words: Arc::new(HashSet::new()),
                tables: vec![OrdinaryCompletionNameTable {
                    table: Arc::new(NameTable::build_with_paths(Vec::new())),
                }],
                scope: None,
                prior_pools: vec![None],
                intent: CompletionIntent::default(),
                history_enabled: false,
                history: CompletionHistorySnapshot::default(),
                prefix_bucket: prefix.to_ascii_lowercase(),
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
            text: "si".to_string(),
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
            }],
            scope: None,
            prior_pools: vec![None],
            intent: CompletionIntent::default(),
            history_enabled: false,
            history: CompletionHistorySnapshot::default(),
            prefix_bucket: "si".to_string(),
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
            text,
            line,
            character,
            parsed_document: Some(parsed),
            local_words: Arc::new(HashSet::new()),
            tables: vec![OrdinaryCompletionNameTable {
                table: Arc::new(NameTable::build_with_paths(Vec::new())),
            }],
            scope: None,
            prior_pools: vec![None],
            intent: CompletionIntent::default(),
            history_enabled: false,
            history: CompletionHistorySnapshot::default(),
            prefix_bucket: "si".to_string(),
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
            text: "int si".to_string(),
            line: 0,
            character: 6,
            parsed_document: None,
            local_words: Arc::new(HashSet::from(["signal_name".to_string()])),
            tables: vec![OrdinaryCompletionNameTable {
                table: Arc::new(NameTable::build_with_paths(Vec::new())),
            }],
            scope: None,
            prior_pools: vec![None],
            intent: crate::completion::classify_completion_intent("int si", 6, "si"),
            history_enabled: false,
            history: CompletionHistorySnapshot::default(),
            prefix_bucket: "si".to_string(),
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
            text: "fs".to_string(),
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
            }],
            scope: Some(CompletionScope {
                current_path: Some("a.c".to_string()),
                reach: ReachScope {
                    files: HashSet::from(["a.c".to_string()]),
                    open: false,
                    reason: None,
                },
            }),
            prior_pools: vec![None],
            intent: CompletionIntent::default(),
            history_enabled: false,
            history: CompletionHistorySnapshot::default(),
            prefix_bucket: "fs".to_string(),
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
}
