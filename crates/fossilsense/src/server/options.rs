use tower_lsp::lsp_types::{
    CompletionList, CompletionResponse, InitializeParams, SignatureHelpOptions,
};

use crate::completion_history::CompletionHistoryMode;
use crate::model;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CompletionMode {
    Auto,
    On,
    Off,
}

impl CompletionMode {
    pub(super) fn is_enabled(self) -> bool {
        self != CompletionMode::Off
    }
}

pub(super) fn parse_completion_mode(params: &InitializeParams) -> CompletionMode {
    let Some(opts) = &params.initialization_options else {
        return CompletionMode::Auto;
    };
    let mode_val = opts
        .as_object()
        .and_then(|o| o.get("fossilsense"))
        .and_then(|v| v.as_object())
        .and_then(|o| o.get("completion"))
        .and_then(|v| v.as_object())
        .and_then(|o| o.get("mode"))
        .and_then(|v| v.as_str())
        .unwrap_or("auto");
    match mode_val {
        "on" => CompletionMode::On,
        "off" => CompletionMode::Off,
        _ => CompletionMode::Auto,
    }
}

pub(super) fn parse_completion_history_mode(params: &InitializeParams) -> CompletionHistoryMode {
    let Some(opts) = &params.initialization_options else {
        return CompletionHistoryMode::Auto;
    };
    let mode_val = opts
        .as_object()
        .and_then(|o| o.get("fossilsense"))
        .and_then(|v| v.as_object())
        .and_then(|o| o.get("completionHistory"))
        .and_then(|v| v.as_object())
        .and_then(|o| o.get("mode"))
        .and_then(|v| v.as_str())
        .unwrap_or("auto");
    match mode_val {
        "on" => CompletionHistoryMode::On,
        "off" => CompletionHistoryMode::Off,
        _ => CompletionHistoryMode::Auto,
    }
}

/// External include reference directories forwarded by the client as
/// `fossilsense.includePaths`. Non-string / non-array values are ignored
/// (never fatal); the indexer further validates each entry against the disk.
pub(super) fn parse_include_paths(params: &InitializeParams) -> Vec<String> {
    let Some(opts) = &params.initialization_options else {
        return Vec::new();
    };
    opts.as_object()
        .and_then(|o| o.get("fossilsense"))
        .and_then(|v| v.as_object())
        .and_then(|o| o.get("includePaths"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().replace('\\', "/"))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn completion_trigger_characters() -> Vec<String> {
    let mut chars = Vec::with_capacity(55);
    chars.extend(('a'..='z').map(|ch| ch.to_string()));
    chars.extend(('A'..='Z').map(|ch| ch.to_string()));
    chars.push("_".to_string());
    // Member access: `.` and the `>` of `->` trigger field completion.
    chars.push(".".to_string());
    chars.push(">".to_string());
    // Include directives: `"`, `<`, and `/` trigger header-path completion. The
    // completion handler still branches on context, so these are inert outside
    // an `#include` line.
    chars.push("\"".to_string());
    chars.push("<".to_string());
    chars.push("/".to_string());
    chars
}

pub(super) fn signature_help_options() -> SignatureHelpOptions {
    SignatureHelpOptions {
        trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
        retrigger_characters: Some(vec![",".to_string()]),
        ..Default::default()
    }
}

pub(super) fn empty_completion_list(is_incomplete: bool) -> CompletionResponse {
    CompletionResponse::List(CompletionList {
        is_incomplete,
        items: Vec::new(),
    })
}

pub(super) fn member_completion_is_incomplete(
    resolved_hit: bool,
    candidate_count: usize,
    limit: usize,
) -> bool {
    !resolved_hit || candidate_count > limit
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SemanticColoringMode {
    Auto,
    On,
    Off,
}

impl SemanticColoringMode {
    pub(super) fn is_enabled(self) -> bool {
        self != SemanticColoringMode::Off
    }
}

pub(super) fn parse_semantic_coloring_mode(params: &InitializeParams) -> SemanticColoringMode {
    let Some(opts) = &params.initialization_options else {
        return SemanticColoringMode::Auto;
    };
    let mode_val = opts
        .as_object()
        .and_then(|o| o.get("fossilsense"))
        .and_then(|v| v.as_object())
        .and_then(|o| o.get("semanticColoring"))
        .and_then(|v| v.as_object())
        .and_then(|o| o.get("mode"))
        .and_then(|v| v.as_str())
        .unwrap_or("auto");
    match mode_val {
        "on" => SemanticColoringMode::On,
        "off" => SemanticColoringMode::Off,
        _ => SemanticColoringMode::Auto,
    }
}

/// Limited include-reachability scoping mode from `fossilsense.includeScoping`.
/// `auto` (default) and `on` both enable scoping; `off` disables it (coloring
/// and completion revert to whole-index behavior).
pub(super) fn parse_include_scoping_enabled(params: &InitializeParams) -> bool {
    let Some(opts) = &params.initialization_options else {
        return true;
    };
    let mode_val = opts
        .as_object()
        .and_then(|o| o.get("fossilsense"))
        .and_then(|v| v.as_object())
        .and_then(|o| o.get("includeScoping"))
        .and_then(|v| v.as_object())
        .and_then(|o| o.get("mode"))
        .and_then(|v| v.as_str())
        .unwrap_or("auto");
    mode_val != "off"
}

/// Whether goto-definition should log each candidate's scope reasoning to the
/// output panel, from `fossilsense.debug.candidateReasons`. Default `false`
/// (any non-`true`/absent value), so ordinary use produces no extra output.
pub(super) fn parse_debug_candidate_reasons(params: &InitializeParams) -> bool {
    let Some(opts) = &params.initialization_options else {
        return false;
    };
    opts.as_object()
        .and_then(|o| o.get("fossilsense"))
        .and_then(|v| v.as_object())
        .and_then(|o| o.get("debug"))
        .and_then(|v| v.as_object())
        .and_then(|o| o.get("candidateReasons"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Whether opt-in performance logs should be mirrored to the LSP output panel.
///
/// There are two supported switches:
/// - `RUST_LOG=fossilsense=debug` / `RUST_LOG=debug` for CLI-driven launches.
/// - `fossilsense.trace.server=verbose`, forwarded by the VS Code extension as
///   `debug.perfLogs`, for packaged-extension workflows where setting process
///   environment variables is awkward.
pub(super) fn parse_debug_perf_logs(params: &InitializeParams) -> bool {
    let from_init = params
        .initialization_options
        .as_ref()
        .and_then(|opts| opts.as_object())
        .and_then(|o| o.get("fossilsense"))
        .and_then(|v| v.as_object())
        .and_then(|o| o.get("debug"))
        .and_then(|v| v.as_object())
        .and_then(|o| o.get("perfLogs"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    from_init || rust_log_enables_perf_logs()
}

fn rust_log_enables_perf_logs() -> bool {
    let Ok(value) = std::env::var("RUST_LOG") else {
        return false;
    };
    value.split(',').any(|directive| {
        let directive = directive.trim().to_ascii_lowercase();
        if directive.is_empty() {
            return false;
        }
        if let Some((target, level)) = directive.rsplit_once('=') {
            let target_matches = target == "fossilsense" || target.starts_with("fossilsense::");
            return target_matches && matches!(level, "debug" | "trace");
        }
        matches!(directive.as_str(), "debug" | "trace" | "fossilsense")
            || directive.starts_with("fossilsense::")
    })
}

/// Format one explanation line per definition candidate (in rank order) for the
/// debug output panel: `<path>:<line> [<tier>/<confidence>/<reason>]`. Returns
/// an empty vec when `enabled` is false, so the caller does no work and emits
/// nothing unless `fossilsense.debug.candidateReasons` is on.
pub(super) fn candidate_reason_log_lines(
    candidates: &[model::DefinitionCandidate],
    enabled: bool,
) -> Vec<String> {
    if !enabled {
        return Vec::new();
    }
    candidates
        .iter()
        .map(|c| {
            format!(
                "  {}:{} [{}/{}/{}]",
                c.path,
                c.range.start_line + 1,
                c.tier.as_str(),
                c.confidence.as_str(),
                c.reason.as_str()
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params_with_mode(mode: Option<&str>) -> InitializeParams {
        let initialization_options = mode.map(|mode| {
            serde_json::json!({
                "fossilsense": { "semanticColoring": { "mode": mode } }
            })
        });
        InitializeParams {
            initialization_options,
            ..Default::default()
        }
    }

    fn params_with_completion_history_mode(mode: Option<&str>) -> InitializeParams {
        let initialization_options = mode.map(|mode| {
            serde_json::json!({
                "fossilsense": { "completionHistory": { "mode": mode } }
            })
        });
        InitializeParams {
            initialization_options,
            ..Default::default()
        }
    }

    #[test]
    fn defaults_to_auto_when_unset() {
        assert_eq!(
            parse_semantic_coloring_mode(&params_with_mode(None)),
            SemanticColoringMode::Auto
        );
        assert!(parse_semantic_coloring_mode(&params_with_mode(None)).is_enabled());
    }

    #[test]
    fn parses_explicit_modes() {
        assert_eq!(
            parse_semantic_coloring_mode(&params_with_mode(Some("on"))),
            SemanticColoringMode::On
        );
        assert_eq!(
            parse_semantic_coloring_mode(&params_with_mode(Some("auto"))),
            SemanticColoringMode::Auto
        );
        assert_eq!(
            parse_semantic_coloring_mode(&params_with_mode(Some("off"))),
            SemanticColoringMode::Off
        );
    }

    #[test]
    fn off_mode_is_disabled() {
        // A disabled mode means initialize advertises no semantic-tokens provider.
        assert!(!parse_semantic_coloring_mode(&params_with_mode(Some("off"))).is_enabled());
    }

    #[test]
    fn unknown_value_falls_back_to_auto() {
        assert_eq!(
            parse_semantic_coloring_mode(&params_with_mode(Some("bogus"))),
            SemanticColoringMode::Auto
        );
    }

    #[test]
    fn parses_completion_history_modes() {
        assert_eq!(
            parse_completion_history_mode(&params_with_completion_history_mode(None)),
            CompletionHistoryMode::Auto
        );
        assert_eq!(
            parse_completion_history_mode(&params_with_completion_history_mode(Some("on"))),
            CompletionHistoryMode::On
        );
        assert_eq!(
            parse_completion_history_mode(&params_with_completion_history_mode(Some("off"))),
            CompletionHistoryMode::Off
        );
        assert_eq!(
            parse_completion_history_mode(&params_with_completion_history_mode(Some("bogus"))),
            CompletionHistoryMode::Auto
        );
        assert!(
            !parse_completion_history_mode(&params_with_completion_history_mode(Some("off")))
                .is_enabled()
        );
    }

    #[test]
    fn short_prefix_empty_completion_is_incomplete() {
        match empty_completion_list(true) {
            CompletionResponse::List(list) => {
                assert!(list.is_incomplete);
                assert!(list.items.is_empty());
            }
            other => panic!("expected completion list, got {other:?}"),
        }
    }

    #[test]
    fn member_completion_incomplete_when_fallback_or_truncated() {
        assert!(
            !member_completion_is_incomplete(true, 100, 100),
            "resolved candidates at the cap are still complete"
        );
        assert!(
            member_completion_is_incomplete(true, 101, 100),
            "resolved candidates beyond the cap are truncated"
        );
        assert!(
            member_completion_is_incomplete(false, 1, 100),
            "fallback candidates are always incomplete"
        );
    }

    #[test]
    fn parse_include_paths_reads_and_normalizes() {
        let params = InitializeParams {
            initialization_options: Some(serde_json::json!({
                "fossilsense": { "includePaths": ["C:\\a\\inc", "", "/usr/include"] }
            })),
            ..Default::default()
        };
        assert_eq!(
            parse_include_paths(&params),
            vec!["C:/a/inc".to_string(), "/usr/include".to_string()]
        );
        // Missing / non-array -> empty, never panics.
        assert!(parse_include_paths(&InitializeParams::default()).is_empty());
    }

    #[test]
    fn completion_triggers_on_identifier_start_and_member_characters() {
        let chars = completion_trigger_characters();

        // 52 letters + `_` + member-access `.`/`>` + include `"`/`<`/`/`.
        assert_eq!(chars.len(), 58);
        assert!(chars.contains(&"a".to_string()));
        assert!(chars.contains(&"Z".to_string()));
        assert!(chars.contains(&"_".to_string()));
        assert!(chars.contains(&".".to_string()));
        assert!(chars.contains(&">".to_string()));
        assert!(chars.contains(&"\"".to_string()));
        assert!(chars.contains(&"<".to_string()));
        assert!(chars.contains(&"/".to_string()));
        assert!(!chars.contains(&"0".to_string()));
    }

    #[test]
    fn signature_help_options_trigger_on_paren_and_comma() {
        let options = signature_help_options();
        assert_eq!(
            options.trigger_characters,
            Some(vec!["(".to_string(), ",".to_string()])
        );
        assert_eq!(options.retrigger_characters, Some(vec![",".to_string()]));
    }

    #[test]
    fn parse_debug_candidate_reasons_defaults_false_and_reads_bool() {
        // Absent -> false (no extra output by default).
        assert!(!parse_debug_candidate_reasons(&InitializeParams::default()));
        // Explicit true -> true.
        let on = InitializeParams {
            initialization_options: Some(serde_json::json!({
                "fossilsense": { "debug": { "candidateReasons": true } }
            })),
            ..Default::default()
        };
        assert!(parse_debug_candidate_reasons(&on));
        // Explicit false -> false.
        let off = InitializeParams {
            initialization_options: Some(serde_json::json!({
                "fossilsense": { "debug": { "candidateReasons": false } }
            })),
            ..Default::default()
        };
        assert!(!parse_debug_candidate_reasons(&off));
    }

    fn def_candidate(
        path: &str,
        line: u32,
        tier: crate::model::ScopeTier,
    ) -> crate::model::DefinitionCandidate {
        let (confidence, reason) = crate::resolver::confidence_reason_for(tier, true, None);
        crate::model::DefinitionCandidate {
            name: "foo".into(),
            kind: "function".into(),
            role: "definition".into(),
            path: path.into(),
            range: crate::model::CandidateRange {
                start_line: line,
                start_col: 0,
                end_line: line,
                end_col: 0,
            },
            source: "workspace".into(),
            tier,
            base_match: 0,
            confidence,
            reason,
        }
    }

    #[test]
    fn candidate_reason_log_lines_off_is_empty_on_lists_each() {
        use crate::model::ScopeTier;
        let cands = vec![
            def_candidate("src/main.c", 4, ScopeTier::Current),
            def_candidate("inc/b.h", 9, ScopeTier::Reachable),
        ];
        // Disabled -> no lines, so the handler emits nothing.
        assert!(candidate_reason_log_lines(&cands, false).is_empty());
        // Enabled -> one line per candidate naming path/line/tier/confidence/reason.
        let lines = candidate_reason_log_lines(&cands, true);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("src/main.c:5")); // 1-based line display
        assert!(lines[0].contains("current"));
        assert!(lines[1].contains("inc/b.h:10"));
        assert!(lines[1].contains("reachable"));
    }
}
