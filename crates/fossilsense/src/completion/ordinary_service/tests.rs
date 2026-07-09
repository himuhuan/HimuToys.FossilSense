use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use crate::completion::{CandidateSource, CompletionIntent};
use crate::completion_history::CompletionHistorySnapshot;
use crate::model::ScopeTier;
use crate::parser;
use crate::project_context::{ProjectContext, ProjectContextIndex, ProjectContextKey};
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

fn project_key(path: &str) -> ProjectContextKey {
    ProjectContextKey {
        workspace_root_id: "workspace".to_string(),
        project_path: path.to_string(),
    }
}

fn project_index() -> ProjectContextIndex {
    ProjectContextIndex::new(
        "workspace".to_string(),
        vec![
            ProjectContext {
                key: project_key("app"),
                marker_files: vec!["Makefile".to_string()],
            },
            ProjectContext {
                key: project_key("third_party/lib"),
                marker_files: vec!["CMakeLists.txt".to_string()],
            },
        ],
    )
}

fn name_row(id: i64, label: &str, path: &str) -> crate::store::views::NameTableSymbolRow {
    crate::store::views::NameTableSymbolRow {
        symbol_id: id,
        id,
        label: label.to_string(),
        external: false,
        path: path.to_string(),
        kind: "function".to_string(),
        directly_included: false,
    }
}

fn project_completion_labels(
    table: Arc<NameTable>,
    active_project_context: Option<ProjectContextKey>,
    limit: usize,
) -> Vec<String> {
    complete_ordinary_identifier(OrdinaryCompletionInput {
        prefix: "get_".to_string(),
        text: "get_".to_string(),
        line: 0,
        character: 4,
        parsed_document: None,
        local_words: Arc::new(HashSet::new()),
        tables: vec![OrdinaryCompletionNameTable { table }],
        scope: None,
        active_project_context,
        prior_pools: vec![None],
        intent: CompletionIntent::default(),
        history_enabled: false,
        history: CompletionHistorySnapshot::default(),
        prefix_bucket: "get".to_string(),
        limit,
        locality_bonus: COMPLETION_LOCALITY_BONUS,
    })
    .items
    .into_iter()
    .map(|item| item.label)
    .collect()
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
        active_project_context: None,
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
        active_project_context: None,
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
            active_project_context: None,
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
        active_project_context: None,
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
        active_project_context: None,
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
        active_project_context: None,
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
        active_project_context: None,
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

#[test]
fn project_context_promotes_same_project_indexed_candidate_without_filtering_others() {
    let index = project_index();
    let table = Arc::new(NameTable::build_from_rows_with_project_context(
        vec![
            name_row(1, "get_other_value", "third_party/lib/src/other.c"),
            name_row(2, "get_app_value", "app/src/app.c"),
        ],
        Some(&index),
    ));

    let labels = project_completion_labels(table, Some(project_key("app")), COMPLETION_LIMIT);

    assert_eq!(labels[0], "get_app_value");
    assert!(labels.iter().any(|label| label == "get_other_value"));
}

#[test]
fn stronger_local_evidence_can_outrank_same_project_evidence() {
    let (text, line, character) =
        text_and_position("void f(void) { int get_other_value; get_/*cursor*/ }");
    let parsed = Arc::new(parser::parse(&PathBuf::from("app/src/app.c"), &text));
    let index = project_index();
    let table = Arc::new(NameTable::build_from_rows_with_project_context(
        vec![name_row(1, "get_app_value", "app/src/app.c")],
        Some(&index),
    ));

    let output = complete_ordinary_identifier(OrdinaryCompletionInput {
        prefix: "get_".to_string(),
        text,
        line,
        character,
        parsed_document: Some(parsed),
        local_words: Arc::new(HashSet::new()),
        tables: vec![OrdinaryCompletionNameTable { table }],
        scope: None,
        active_project_context: Some(project_key("app")),
        prior_pools: vec![None],
        intent: CompletionIntent::default(),
        history_enabled: false,
        history: CompletionHistorySnapshot::default(),
        prefix_bucket: "get".to_string(),
        limit: COMPLETION_LIMIT,
        locality_bonus: COMPLETION_LOCALITY_BONUS,
    });

    assert_eq!(output.items[0].label, "get_other_value");
}

#[test]
fn unspecified_project_context_matches_disabled_ordering() {
    let index = project_index();
    let rows = vec![
        name_row(1, "get_other_value", "third_party/lib/src/other.c"),
        name_row(2, "get_app_value", "app/src/app.c"),
    ];
    let annotated = Arc::new(NameTable::build_from_rows_with_project_context(
        rows.clone(),
        Some(&index),
    ));
    let disabled = Arc::new(NameTable::build_from_rows(rows));

    assert_eq!(
        project_completion_labels(annotated, None, COMPLETION_LIMIT),
        project_completion_labels(disabled, None, COMPLETION_LIMIT)
    );
}

#[test]
fn same_project_recall_metric_is_aggregate_only() {
    let index = project_index();
    let table = Arc::new(NameTable::build_from_rows_with_project_context(
        vec![
            name_row(1, "get_other_value", "third_party/lib/src/other.c"),
            name_row(2, "get_app_value", "app/src/app.c"),
        ],
        Some(&index),
    ));

    let output = complete_ordinary_identifier(OrdinaryCompletionInput {
        prefix: "get_".to_string(),
        text: "get_".to_string(),
        line: 0,
        character: 4,
        parsed_document: None,
        local_words: Arc::new(HashSet::new()),
        tables: vec![OrdinaryCompletionNameTable { table }],
        scope: None,
        active_project_context: Some(project_key("app")),
        prior_pools: vec![None],
        intent: CompletionIntent::default(),
        history_enabled: false,
        history: CompletionHistorySnapshot::default(),
        prefix_bucket: "get".to_string(),
        limit: COMPLETION_LIMIT,
        locality_bonus: COMPLETION_LOCALITY_BONUS,
    });

    assert_eq!(output.metrics.recall_channels.same_project, 1);
}
