//! Explicit disk/index diagnostics; no editor state and no index maintenance.
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{json, Value};

use crate::config::{LanguageResolver, WorkspaceConfig};
use crate::parser::{parse_thread_local_with_selection, ParseFacts};
use crate::semantic_model::{FactCoverage, FactGroup, PARSER_FACT_VERSION};

mod index;
mod input;
mod output;
pub(crate) use input::InputError;

const FACT_LIMIT: usize = 256;
const REGION_LIMIT: usize = 256;
const SCAN_LIMIT: usize = 16_384;
const JSON_LIMIT: usize = 2 * 1024 * 1024;
const STAGES: [&str; 7] = [
    "inclusion",
    "language",
    "region",
    "extraction",
    "persistence",
    "recall",
    "presentation",
];

pub(crate) fn run(
    workspace: PathBuf,
    file: PathBuf,
    name: String,
    line: Option<u32>,
    col: Option<u32>,
    db: Option<PathBuf>,
    json_output: bool,
) -> Result<()> {
    let report = observe(&workspace, &file, &name, line, col, db)?;
    let bytes = output::encode(report, JSON_LIMIT)?;
    if json_output {
        println!("{}", String::from_utf8(bytes)?);
    } else {
        let report: Value = serde_json::from_slice(&bytes)?;
        println!(
            "Declaration discovery: {} in {}",
            name,
            report["request"]["file"].as_str().unwrap_or_default()
        );
        println!("Disk observation only; editor unsaved text is unavailable.");
        println!(
            "Index consistency: {}",
            report["indexObservation"]["consistency"]
                .as_str()
                .unwrap_or("unknown")
        );
        println!(
            "Name positions: {}{}",
            report["request"]["positions"]
                .as_array()
                .map_or(0, Vec::len),
            if report["request"]["ambiguous"] == true {
                " (ambiguous; use --line/--col)"
            } else {
                ""
            }
        );
        for stage in STAGES {
            let item = &report["stages"][stage];
            println!(
                "{stage}: {} — {} (returned {}, limit {})",
                item["status"].as_str().unwrap_or("unknown"),
                item["reason"].as_str().unwrap_or("unknown"),
                item["counts"]["returned"],
                item["limit"]
            );
        }
    }
    Ok(())
}

fn stage(status: &str, reason: &str, returned: usize, limit: usize, evidence: Value) -> Value {
    json!({"status":status,"reason":reason,"counts":{"returned":returned},"limit":limit,"evidence":evidence})
}

fn observe(
    workspace: &Path,
    file: &Path,
    name: &str,
    line: Option<u32>,
    col: Option<u32>,
    db: Option<PathBuf>,
) -> Result<Value> {
    let (root, absolute, rel) = input::paths(workspace, file, name)?;
    let (config, config_issue) = WorkspaceConfig::load(&root);
    let inclusion = crate::scanner::file_inclusion(&root, &absolute, &config, SCAN_LIMIT)?;
    let mut stages = serde_json::Map::new();
    for key in STAGES {
        stages.insert(
            key.into(),
            stage("not_run", "source_unavailable", 0, FACT_LIMIT, json!({})),
        );
    }
    stages.insert("inclusion".into(), stage(if inclusion.truncated {"truncated"} else if inclusion.included {"found"} else {"absent"},
        if inclusion.truncated {"scan_budget"} else if inclusion.included {"included_by_scan_rules"} else {"excluded_by_scan_rules"},
        usize::from(inclusion.included), SCAN_LIMIT, json!({"visitedEntries":inclusion.inspected, "configIssue":config_issue.map(|issue|issue.message)})));
    let mut report = json!({"formatVersion":1,"request":{"file":rel,"name":name,"line":line,"col":col,"positions":[],"ambiguous":false,"truncated":false,"positionLimit":input::POSITION_LIMIT},
        "diskObservation":{"status":"not_run","source":"disk","parserVersion":PARSER_FACT_VERSION,"sourceByteLimit":input::SOURCE_LIMIT},
        "indexObservation":{"status":"not_run","consistency":"unknown"},"stages":stages,"outputLimitBytes":JSON_LIMIT,"outputTruncated":false});
    let (bytes, oversized) = input::read_source(&absolute)?;
    if oversized {
        report["diskObservation"]["status"] = json!("truncated");
        for key in &STAGES[1..] {
            report["stages"][*key] = stage(
                "not_run",
                "source_byte_budget",
                0,
                input::SOURCE_LIMIT,
                json!({}),
            );
        }
        return Ok(report);
    }
    let source = String::from_utf8_lossy(&bytes);
    let (positions, truncated) = input::positions(&source, name, line, col, input::POSITION_LIMIT)?;
    report["request"]["ambiguous"] = json!(positions.len() > 1 || truncated);
    report["request"]["truncated"] = json!(truncated);
    report["request"]["positions"] = serde_json::to_value(positions)?;
    let hash = blake3::hash(&bytes).to_hex().to_string();
    let selection = LanguageResolver::from_workspace_config(&root, &config)
        .selection_for_source(&absolute, &source);
    report["diskObservation"] = json!({"status":"found","source":"disk","contentHash":hash,"byteLength":bytes.len(),"sourceByteLimit":input::SOURCE_LIMIT,
        "parserVersion":PARSER_FACT_VERSION,"languageEvidence":selection.evidence,"encoding":"utf8_lossy"});
    report["stages"]["language"] = stage(
        if selection.evidence.ambiguous {
            "partial"
        } else {
            "found"
        },
        if selection.evidence.ambiguous {
            "language_ambiguity"
        } else {
            "source_language_selected"
        },
        1,
        1,
        json!({"language":selection.language.semantic_language(),"evidence":selection.evidence}),
    );
    let parsed = parse_thread_local_with_selection(&absolute, &source, selection, ParseFacts::ALL);
    let coverage = &parsed.diagnostics.coverage;
    let region_truncated = coverage.summary.truncated || coverage.gaps.len() > REGION_LIMIT;
    report["stages"]["region"] = stage(
        if region_truncated {
            "truncated"
        } else if coverage.summary.partial_groups != 0 {
            "partial"
        } else if coverage.summary.checked_groups == 0 {
            "unknown"
        } else {
            "found"
        },
        "parser_coverage",
        coverage.gaps.len().min(REGION_LIMIT),
        REGION_LIMIT,
        json!({"summary":coverage.summary,"uncoveredRatio":coverage.summary.uncovered_ratio(),"gaps":coverage.gaps.iter().take(REGION_LIMIT).collect::<Vec<_>>(),"recoveryBudgetExhausted":parsed.diagnostics.recovery_budget_exhausted}),
    );
    let facts: Vec<_> = parsed
        .declarations
        .iter()
        .filter(|fact| fact.name == name)
        .take(FACT_LIMIT + 1)
        .collect();
    let members: Vec<_> = parsed
        .members
        .iter()
        .filter(|member| member.name == name)
        .take(FACT_LIMIT + 1)
        .collect();
    let locals: Vec<_> = parsed
        .local_bindings
        .iter()
        .filter(|binding| binding.name == name)
        .take(FACT_LIMIT + 1)
        .collect();
    let mut evidence = Vec::new();
    for fact in &facts {
        if evidence.len() == FACT_LIMIT {
            break;
        }
        evidence.push(json!({"scope":"persistent","fact":fact}));
    }
    for member in &members {
        if evidence.len() == FACT_LIMIT {
            break;
        }
        evidence.push(json!({"scope":"persistent_member","name":member.name,"recordKey":member.record_key,"startByte":member.start_byte,"endByte":member.end_byte}));
    }
    for binding in &locals {
        if evidence.len() == FACT_LIMIT {
            break;
        }
        evidence.push(json!({"scope":"request_local","name":binding.name,"startByte":binding.decl_start_byte,"functionStartByte":binding.function_start_byte}));
    }
    let found_count = facts.len() + members.len() + locals.len();
    let state = parsed.fact_coverage(FactGroup::Declarations);
    report["stages"]["extraction"] = stage(
        if found_count > FACT_LIMIT {
            "truncated"
        } else if !evidence.is_empty() {
            "found"
        } else if state == FactCoverage::Complete {
            "absent"
        } else if state == FactCoverage::Partial {
            "partial"
        } else {
            "unknown"
        },
        if found_count == 0 && state != FactCoverage::Complete {
            "coverage_incomplete"
        } else {
            "observed_parser_facts"
        },
        evidence.len(),
        FACT_LIMIT,
        json!({"facts":evidence,"declarationCoverage":state,"persistentDeclarations":facts.len(),"members":members.len(),"requestLocalBindings":locals.len()}),
    );
    index::observe(&mut report, &root, &rel, name, db, &hash, selection)?;
    Ok(report)
}
