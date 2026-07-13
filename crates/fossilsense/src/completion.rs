mod intent;
pub(crate) mod ordinary_service;
mod pipeline;
mod prefix_ranking;

pub(crate) use intent::{
    classify_completion_intent, CompletionIntent, CompletionIntentConfidence, CompletionIntentKind,
};
pub(crate) use pipeline::*;
pub(crate) use prefix_ranking::CompletionPrefixRanking;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion_history::{
        candidate_hash, candidate_hash_key, CompletionHistorySnapshot,
    };
    use crate::model::{ResolutionConfidence, ScopeTier};

    #[test]
    fn intent_classifies_preprocessor_macro_context() {
        let intent = classify_completion_intent("#if FS_", 7, "FS_");

        assert_eq!(intent.kind, CompletionIntentKind::MacroPreprocessor);
        assert_eq!(intent.confidence, CompletionIntentConfidence::High);
    }

    #[test]
    fn intent_classifies_call_target_before_open_paren() {
        let intent = classify_completion_intent("    FS_do(", 9, "FS_do");

        assert_eq!(intent.kind, CompletionIntentKind::CallTarget);
        assert!(intent.confidence >= CompletionIntentConfidence::Medium);
    }

    #[test]
    fn intent_classifies_type_and_declaration_name_contexts() {
        let type_intent = classify_completion_intent("    struct FS_", 14, "FS_");
        assert_eq!(type_intent.kind, CompletionIntentKind::TypeName);

        let decl_intent = classify_completion_intent("    FsWidget fs_", 16, "fs_");
        assert_eq!(decl_intent.kind, CompletionIntentKind::DeclarationName);
    }

    #[test]
    fn intent_classifies_pointer_and_reference_declaration_names() {
        let pointer_intent = classify_completion_intent("    FsWidget *fs_", 17, "fs_");
        assert_eq!(pointer_intent.kind, CompletionIntentKind::DeclarationName);

        let const_pointer_intent = classify_completion_intent("    const FsWidget *fs_", 23, "fs_");
        assert_eq!(
            const_pointer_intent.kind,
            CompletionIntentKind::DeclarationName
        );

        let reference_intent = classify_completion_intent("    FsWidget &fs_", 17, "fs_");
        assert_eq!(reference_intent.kind, CompletionIntentKind::DeclarationName);
    }

    #[test]
    fn intent_degrades_for_uncertain_expression_context() {
        let intent = classify_completion_intent("    value = FS_", 15, "FS_");

        assert!(matches!(
            intent.kind,
            CompletionIntentKind::ExpressionValue | CompletionIntentKind::Neutral
        ));
        assert!(intent.confidence <= CompletionIntentConfidence::Medium);
    }

    fn candidate(
        name: &str,
        source: CandidateSource,
        tier: ScopeTier,
        score: i32,
        payload: &'static str,
    ) -> PipelineCandidate<&'static str> {
        PipelineCandidate::new(
            name,
            CandidateEvidence::new(source, tier, ResolutionConfidence::Heuristic, score),
            payload,
        )
    }

    fn candidate_with_kind(
        name: &str,
        source: CandidateSource,
        tier: ScopeTier,
        score: i32,
        kind: CompletionCandidateKind,
        payload: &'static str,
    ) -> PipelineCandidate<&'static str> {
        let mut evidence =
            CandidateEvidence::new(source, tier, ResolutionConfidence::Heuristic, score);
        evidence.kind = kind;
        PipelineCandidate::new(name, evidence, payload)
    }

    fn candidate_with_history_key(
        name: &str,
        source: CandidateSource,
        tier: ScopeTier,
        score: i32,
        kind: CompletionCandidateKind,
        kind_str: &str,
        payload: &'static str,
    ) -> PipelineCandidate<&'static str> {
        let mut evidence =
            CandidateEvidence::new(source, tier, ResolutionConfidence::Heuristic, score);
        evidence.kind = kind;
        evidence.history_key = Some(candidate_hash_key(name, kind_str));
        PipelineCandidate::new(name, evidence, payload)
    }

    #[test]
    fn history_boost_lifts_comparable_candidate_but_not_current_local() {
        let history = CompletionHistorySnapshot::from_test_accepts(vec![(
            candidate_hash("global_fn", "function"),
            "function",
            "call_target",
            "gl",
            4,
        )]);

        let output = run_evidence_aware_pipeline_with_context(
            vec![
                candidate_with_history_key(
                    "global_fn",
                    CandidateSource::Indexed,
                    ScopeTier::Global,
                    820,
                    CompletionCandidateKind::Function,
                    "function",
                    "global",
                ),
                candidate_with_history_key(
                    "local_value",
                    CandidateSource::LocalBinding,
                    ScopeTier::Current,
                    760,
                    CompletionCandidateKind::Variable,
                    "variable",
                    "local",
                ),
            ],
            10,
            CompletionRankContext {
                intent: CompletionIntent {
                    kind: CompletionIntentKind::CallTarget,
                    confidence: CompletionIntentConfidence::High,
                },
                history_enabled: true,
                history,
                prefix_bucket: "gl".to_string(),
                // This test isolates the bounded history signal. Name-match
                // guard behavior is covered independently below.
                prefix: String::new(),
                prefix_ranking: CompletionPrefixRanking::Strict,
            },
        );

        assert_eq!(output.items[0].payload, "local");
        assert!(output.metrics.history_boosted >= 1);
        assert!(output.metrics.history_max_boost > 0);
    }

    #[test]
    fn merged_candidate_history_uses_final_kind_hash() {
        let history = CompletionHistorySnapshot::from_test_accepts(vec![(
            candidate_hash("same_name", "function"),
            "function",
            "call_target",
            "sa",
            2,
        )]);

        let output = run_evidence_aware_pipeline_with_context(
            vec![
                candidate_with_history_key(
                    "same_name",
                    CandidateSource::LocalBinding,
                    ScopeTier::Current,
                    760,
                    CompletionCandidateKind::Variable,
                    "variable",
                    "local",
                ),
                candidate_with_history_key(
                    "same_name",
                    CandidateSource::Indexed,
                    ScopeTier::Reachable,
                    820,
                    CompletionCandidateKind::Function,
                    "function",
                    "indexed",
                ),
            ],
            10,
            CompletionRankContext {
                intent: CompletionIntent {
                    kind: CompletionIntentKind::CallTarget,
                    confidence: CompletionIntentConfidence::High,
                },
                history_enabled: true,
                history,
                prefix_bucket: "sa".to_string(),
                prefix: "sa".to_string(),
                prefix_ranking: CompletionPrefixRanking::Strict,
            },
        );

        assert_eq!(output.items.len(), 1);
        assert_eq!(
            output.items[0].evidence.kind,
            CompletionCandidateKind::Function
        );
        assert_eq!(output.metrics.history_boosted, 1);
        assert!(output.items[0].evidence.history_score > 0);
    }

    #[test]
    fn neutral_history_context_preserves_existing_order() {
        let candidates = vec![
            candidate(
                "alpha",
                CandidateSource::Indexed,
                ScopeTier::Reachable,
                700,
                "a",
            ),
            candidate(
                "beta",
                CandidateSource::Indexed,
                ScopeTier::Global,
                900,
                "b",
            ),
        ];
        let without = run_evidence_aware_pipeline(candidates.clone(), 10);
        let disabled = run_evidence_aware_pipeline_with_context(
            candidates,
            10,
            CompletionRankContext::default(),
        );

        assert_eq!(
            without
                .items
                .iter()
                .map(|candidate| candidate.payload)
                .collect::<Vec<_>>(),
            disabled
                .items
                .iter()
                .map(|candidate| candidate.payload)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn type_intent_lifts_type_candidates_without_hiding_values() {
        let output = run_evidence_aware_pipeline_with_context(
            vec![
                candidate_with_kind(
                    "FsWidget",
                    CandidateSource::Indexed,
                    ScopeTier::Global,
                    800,
                    CompletionCandidateKind::Type,
                    "type",
                ),
                candidate_with_kind(
                    "fs_widget_value",
                    CandidateSource::Indexed,
                    ScopeTier::Reachable,
                    650,
                    CompletionCandidateKind::Variable,
                    "value",
                ),
            ],
            10,
            CompletionRankContext::for_intent(
                CompletionIntentKind::TypeName,
                CompletionIntentConfidence::High,
            ),
        );

        assert_eq!(output.items[0].payload, "type");
        assert!(output
            .items
            .iter()
            .any(|candidate| candidate.payload == "value"));
    }

    #[test]
    fn expression_intent_demotes_type_only_candidates_but_keeps_them() {
        let output = run_evidence_aware_pipeline_with_context(
            vec![
                candidate_with_kind(
                    "FsWidget",
                    CandidateSource::Indexed,
                    ScopeTier::Reachable,
                    800,
                    CompletionCandidateKind::Type,
                    "type",
                ),
                candidate_with_kind(
                    "fs_value",
                    CandidateSource::Indexed,
                    ScopeTier::Reachable,
                    760,
                    CompletionCandidateKind::Variable,
                    "value",
                ),
            ],
            10,
            CompletionRankContext::for_intent(
                CompletionIntentKind::ExpressionValue,
                CompletionIntentConfidence::High,
            ),
        );

        assert_eq!(output.items[0].payload, "value");
        assert!(output
            .items
            .iter()
            .any(|candidate| candidate.payload == "type"));
    }

    #[test]
    fn call_intent_lifts_functions() {
        let output = run_evidence_aware_pipeline_with_context(
            vec![
                candidate_with_kind(
                    "FsRun",
                    CandidateSource::Indexed,
                    ScopeTier::Reachable,
                    720,
                    CompletionCandidateKind::Function,
                    "function",
                ),
                candidate_with_kind(
                    "fs_runtime",
                    CandidateSource::Indexed,
                    ScopeTier::Reachable,
                    780,
                    CompletionCandidateKind::Variable,
                    "variable",
                ),
            ],
            10,
            CompletionRankContext::for_intent(
                CompletionIntentKind::CallTarget,
                CompletionIntentConfidence::High,
            ),
        );

        assert_eq!(output.items[0].payload, "function");
    }

    #[test]
    fn macro_preprocessor_intent_lifts_macros() {
        let output = run_evidence_aware_pipeline_with_context(
            vec![
                candidate_with_kind(
                    "FS_WIDGET",
                    CandidateSource::Indexed,
                    ScopeTier::Reachable,
                    760,
                    CompletionCandidateKind::Type,
                    "type",
                ),
                candidate_with_kind(
                    "FS_ENABLED",
                    CandidateSource::Indexed,
                    ScopeTier::Reachable,
                    720,
                    CompletionCandidateKind::Macro,
                    "macro",
                ),
            ],
            10,
            CompletionRankContext::for_intent(
                CompletionIntentKind::MacroPreprocessor,
                CompletionIntentConfidence::High,
            ),
        );

        assert_eq!(output.items[0].payload, "macro");
    }

    #[test]
    fn declaration_name_intent_reduces_global_reuse_pressure() {
        let output = run_evidence_aware_pipeline_with_context(
            vec![
                candidate_with_kind(
                    "fs_widget",
                    CandidateSource::Indexed,
                    ScopeTier::Global,
                    900,
                    CompletionCandidateKind::Variable,
                    "global",
                ),
                candidate_with_kind(
                    "fs_working_name",
                    CandidateSource::LocalWord,
                    ScopeTier::Global,
                    860,
                    CompletionCandidateKind::Text,
                    "text",
                ),
            ],
            10,
            CompletionRankContext::for_intent(
                CompletionIntentKind::DeclarationName,
                CompletionIntentConfidence::High,
            ),
        );

        assert_eq!(output.items[0].payload, "text");
        assert!(output
            .items
            .iter()
            .any(|candidate| candidate.payload == "global"));
    }

    #[test]
    fn perf_summary_reports_intent_without_candidate_names() {
        let metrics = CompletionPipelineMetrics {
            intent_kind: CompletionIntentKind::CallTarget,
            intent_confidence: CompletionIntentConfidence::High,
            ..CompletionPipelineMetrics::default()
        };
        let line = completion_perf_summary(
            "fs_",
            "cold",
            7,
            42,
            &CompletionStageTimings::default(),
            &metrics,
        );

        assert!(line.contains("intent=call_target"));
        assert!(line.contains("intent_confidence=high"));
        assert!(line.contains("document_version=7"));
        assert!(line.contains("engine_generation=42"));
        assert!(!line.contains("fs_\""));
    }

    #[test]
    fn evidence_pipeline_merges_same_name_sources() {
        let output = run_evidence_aware_pipeline(
            vec![
                candidate(
                    "Widget",
                    CandidateSource::Indexed,
                    ScopeTier::Reachable,
                    800,
                    "indexed",
                ),
                candidate(
                    "Widget",
                    CandidateSource::CurrentFileOverlay,
                    ScopeTier::Current,
                    1000,
                    "overlay",
                ),
                candidate(
                    "Widget",
                    CandidateSource::LocalWord,
                    ScopeTier::Global,
                    750,
                    "word",
                ),
            ],
            10,
        );

        assert_eq!(output.items.len(), 1);
        let evidence = &output.items[0].evidence;
        assert!(evidence.sources.indexed);
        assert!(evidence.sources.current_file_overlay);
        assert!(evidence.sources.local_word);
        assert_eq!(output.items[0].payload, "overlay");
    }

    #[test]
    fn ranker_keeps_reachable_prefix_above_plain_global_fuzzy() {
        let output = run_evidence_aware_pipeline(
            vec![
                candidate(
                    "reachable_api",
                    CandidateSource::Indexed,
                    ScopeTier::Reachable,
                    800,
                    "reach",
                ),
                candidate(
                    "api_text_tail",
                    CandidateSource::LocalWord,
                    ScopeTier::Global,
                    250,
                    "text",
                ),
            ],
            10,
        );

        assert_eq!(output.items[0].payload, "reach");
        assert_eq!(output.metrics.final_rank.guarded_low_trust, 1);
    }

    #[test]
    fn strict_prefix_ranking_guards_exact_then_prefix_above_fuzzy_scope() {
        let output = run_evidence_aware_pipeline_with_context(
            vec![
                candidate(
                    "wns__ipc_rsp_init",
                    CandidateSource::Indexed,
                    ScopeTier::Current,
                    200,
                    "fuzzy",
                ),
                candidate(
                    "wns_ipc_send",
                    CandidateSource::Indexed,
                    ScopeTier::External,
                    800,
                    "prefix",
                ),
                candidate(
                    "wns_ipc",
                    CandidateSource::Indexed,
                    ScopeTier::Global,
                    1000,
                    "exact",
                ),
            ],
            10,
            CompletionRankContext {
                prefix: "wns_ipc".to_string(),
                prefix_ranking: CompletionPrefixRanking::Strict,
                ..CompletionRankContext::default()
            },
        );

        assert_eq!(
            output
                .items
                .iter()
                .map(|candidate| candidate.payload)
                .collect::<Vec<_>>(),
            vec!["exact", "prefix", "fuzzy"]
        );
    }

    #[test]
    fn current_overlay_exact_can_outrank_reachable_weak_match() {
        let output = run_evidence_aware_pipeline(
            vec![
                candidate(
                    "new_local_type",
                    CandidateSource::CurrentFileOverlay,
                    ScopeTier::Current,
                    1000,
                    "overlay",
                ),
                candidate(
                    "newLocalTypeFactory",
                    CandidateSource::Indexed,
                    ScopeTier::Reachable,
                    400,
                    "reach",
                ),
            ],
            10,
        );

        assert_eq!(output.items[0].payload, "overlay");
    }

    #[test]
    fn evidence_perf_summary_is_source_safe_and_reports_ranker_fields() {
        let metrics = CompletionPipelineMetrics {
            input_total: 5,
            after_dedup_total: 4,
            returned_total: 4,
            input_sources: SourceCounts {
                indexed: 1,
                local_binding: 1,
                current_file_overlay: 1,
                language_builtin: 1,
                local_word: 1,
            },
            returned_sources: SourceCounts {
                indexed: 1,
                local_binding: 1,
                current_file_overlay: 1,
                language_builtin: 1,
                local_word: 0,
            },
            final_rank: FinalRankSummary {
                guarded_low_trust: 2,
            },
            shadow: Some(ShadowRankSummary {
                moved: 1,
                max_delta: 2,
            }),
            ..CompletionPipelineMetrics::default()
        };
        let timings = CompletionStageTimings {
            total_ms: 9,
            context_ms: 1,
            recall_ms: 2,
            merge_rank_ms: 3,
            render_ms: 1,
        };

        let line = completion_perf_summary("Widget", "miss", 3, 9, &timings, &metrics);

        assert!(line.contains("current_file_overlay=1"));
        assert!(line.contains("language_builtin=1"));
        assert!(line.contains("returned_current_file_overlay=1"));
        assert!(line.contains("returned_language_builtin=1"));
        assert!(line.contains("guarded_low_trust=2"));
        assert!(!line.contains("Widget\""));
        assert!(!line.contains("reachable_api"));
        assert!(!line.contains("new_local_type"));
    }

    #[test]
    fn compatible_pipeline_preserves_score_order_and_source_priority() {
        let candidates = vec![
            PipelineCandidate::new(
                "shared",
                CandidateEvidence::new(
                    CandidateSource::Indexed,
                    ScopeTier::Reachable,
                    ResolutionConfidence::Reachable,
                    30_000,
                ),
                "indexed",
            ),
            PipelineCandidate::new(
                "zeta",
                CandidateEvidence::new(
                    CandidateSource::LocalWord,
                    ScopeTier::Global,
                    ResolutionConfidence::Fallback,
                    900,
                ),
                "word",
            ),
            PipelineCandidate::new(
                "shared",
                CandidateEvidence::new(
                    CandidateSource::LocalBinding,
                    ScopeTier::Current,
                    ResolutionConfidence::Heuristic,
                    40_000,
                ),
                "local",
            ),
            PipelineCandidate::new(
                "alpha",
                CandidateEvidence::new(
                    CandidateSource::Indexed,
                    ScopeTier::Current,
                    ResolutionConfidence::Exact,
                    40_000,
                ),
                "alpha",
            ),
        ];

        let output = run_compatible_pipeline(candidates, 10);

        assert_eq!(
            output
                .items
                .iter()
                .map(|candidate| (candidate.name.as_str(), candidate.payload))
                .collect::<Vec<_>>(),
            vec![("alpha", "alpha"), ("shared", "local"), ("zeta", "word")]
        );
    }

    #[test]
    fn pipeline_metrics_count_sources_before_and_after_dedup() {
        let candidates = vec![
            PipelineCandidate::new(
                "same",
                CandidateEvidence::new(
                    CandidateSource::Indexed,
                    ScopeTier::Reachable,
                    ResolutionConfidence::Reachable,
                    10,
                ),
                (),
            ),
            PipelineCandidate::new(
                "same",
                CandidateEvidence::new(
                    CandidateSource::LocalBinding,
                    ScopeTier::Current,
                    ResolutionConfidence::Heuristic,
                    20,
                ),
                (),
            ),
            PipelineCandidate::new(
                "word",
                CandidateEvidence::new(
                    CandidateSource::LocalWord,
                    ScopeTier::Global,
                    ResolutionConfidence::Fallback,
                    5,
                ),
                (),
            ),
        ];

        let output = run_compatible_pipeline(candidates, 10);

        assert_eq!(output.metrics.input_total, 3);
        assert_eq!(output.metrics.after_dedup_total, 2);
        assert_eq!(output.metrics.returned_total, 2);
        assert_eq!(output.metrics.input_sources.indexed, 1);
        assert_eq!(output.metrics.input_sources.local_binding, 1);
        assert_eq!(output.metrics.input_sources.local_word, 1);
        assert_eq!(output.metrics.returned_sources.indexed, 0);
        assert_eq!(output.metrics.returned_sources.local_binding, 1);
        assert_eq!(output.metrics.returned_sources.local_word, 1);
    }

    #[test]
    fn shadow_comparison_reports_rank_movement() {
        let display = ["alpha".to_string(), "beta".to_string(), "gamma".to_string()];
        let shadow = ["beta".to_string(), "alpha".to_string(), "gamma".to_string()];

        let summary = compare_shadow_ranks(&display, &shadow);

        assert_eq!(summary.moved, 2);
        assert_eq!(summary.max_delta, 1);
    }

    #[test]
    fn completion_perf_summary_omits_candidate_names() {
        let metrics = CompletionPipelineMetrics {
            input_total: 3,
            after_dedup_total: 2,
            returned_total: 2,
            input_sources: SourceCounts {
                indexed: 1,
                local_binding: 1,
                current_file_overlay: 0,
                language_builtin: 0,
                local_word: 1,
            },
            returned_sources: SourceCounts {
                indexed: 0,
                local_binding: 1,
                current_file_overlay: 0,
                language_builtin: 0,
                local_word: 1,
            },
            final_rank: FinalRankSummary::default(),
            shadow: Some(ShadowRankSummary {
                moved: 2,
                max_delta: 1,
            }),
            ..CompletionPipelineMetrics::default()
        };
        let timings = CompletionStageTimings {
            total_ms: 9,
            context_ms: 1,
            recall_ms: 2,
            merge_rank_ms: 3,
            render_ms: 1,
        };

        let line = completion_perf_summary("foo", "pool", 1, 2, &timings, &metrics);

        assert!(line.contains("[perf] completion"));
        assert!(line.contains("prefix_len=3"));
        assert!(line.contains("hit=pool"));
        assert!(line.contains("indexed=1"));
        assert!(line.contains("local_binding=1"));
        assert!(line.contains("shadow_moved=2"));
        assert!(!line.contains("alpha"));
        assert!(!line.contains("beta"));
        assert!(!line.contains("foo\""));
    }
}
