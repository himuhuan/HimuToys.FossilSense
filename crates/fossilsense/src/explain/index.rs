use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{json, Value};

use crate::call_service::CallReadHandle;
use crate::candidate_service::{
    navigation_presentations, CandidateOverlaySnapshot, CandidateQueryService, SemanticIntent,
};
use crate::config::LanguageSelection;
use crate::semantic_model::PARSER_FACT_VERSION;

use super::{stage, FACT_LIMIT};

pub(super) fn observe(
    report: &mut Value,
    root: &Path,
    rel: &str,
    name: &str,
    db: Option<PathBuf>,
    hash: &str,
    selection: LanguageSelection,
) -> Result<()> {
    let path = match db {
        Some(path) => path,
        None => crate::pathing::default_index_path(root)?,
    };
    report["indexObservation"]["database"] = json!(path);
    if !path.try_exists()? {
        return not_run(report, "index_missing");
    }
    let handle = CallReadHandle::capture_diagnostic(path)?;
    let metadata: crate::store::DiagnosticIndexMetadata =
        handle.read(|store| store.diagnostic_metadata())?;
    report["indexObservation"]["metadata"] = serde_json::to_value(&metadata)?;
    report["indexObservation"]["generation"] = json!(metadata.generation);
    if !metadata.compatible {
        report["indexObservation"]["status"] = json!("unknown");
        return not_run(report, "index_schema_incompatible");
    }
    let workspace_matches = metadata.workspace_root.as_ref().is_some_and(|stored| {
        let stored = Path::new(stored);
        crate::pathing::path_is_within(root, stored) && crate::pathing::path_is_within(stored, root)
    });
    if !workspace_matches {
        report["indexObservation"]["status"] = json!("unknown");
        return not_run(report, "index_workspace_mismatch");
    }
    let (stored, coverage, versions) = handle.read(|store| {
        Ok((
            store.stored_file(rel)?,
            store.coverage_view().for_path(rel)?,
            store.diagnostic_parser_versions(&[rel.to_owned()])?,
        ))
    })?;
    let current_version = versions.first().map(|entry| entry.1);
    report["indexObservation"]["parserVersion"] = json!(current_version);
    if current_version.is_some_and(|version| version != PARSER_FACT_VERSION) {
        report["indexObservation"]["status"] = json!("unknown");
        return not_run(report, "index_parser_version_incompatible");
    }
    let matched = stored.as_ref().is_some_and(|file| {
        file.hash == hash && file.language_evidence == Some(selection.evidence)
    });
    let consistency = if stored.is_none() {
        "unknown"
    } else if matched {
        "matched"
    } else {
        "stale"
    };
    report["indexObservation"]["status"] = json!("found");
    report["indexObservation"]["consistency"] = json!(consistency);
    report["indexObservation"]["contentHash"] = json!(stored.as_ref().map(|file| &file.hash));
    report["indexObservation"]["languageEvidence"] =
        json!(stored.as_ref().and_then(|file| file.language_evidence));
    report["indexObservation"]["fileRevision"] =
        json!(coverage.as_ref().map(|row| row.revision_id));
    report["indexObservation"]["coverage"] = json!(coverage.as_ref().map(|row| row.summary));
    let (rows, limited) = handle.read(|store| {
        store.declaration_view().by_name_family_in_paths_limited(
            name,
            selection.semantic_family(),
            &[rel.to_owned()],
            FACT_LIMIT,
        )
    })?;
    report["stages"]["persistence"] = stage(
        if limited {
            "truncated"
        } else if !rows.is_empty() {
            "found"
        } else if consistency == "stale" {
            "unknown"
        } else {
            "absent"
        },
        if consistency == "stale" {
            "disk_index_mismatch"
        } else if stored.is_none() {
            "file_not_indexed"
        } else {
            "active_revision_facts"
        },
        rows.len(),
        FACT_LIMIT,
        json!({"declarationIds":rows.iter().map(|row|row.id).collect::<Vec<_>>(),"facts":rows.iter().map(|row| &row.fact).collect::<Vec<_>>(),"scope":"persistent_declarations_only"}),
    );
    let extraction = &report["stages"]["extraction"]["evidence"];
    if matched
        && rows.is_empty()
        && extraction["persistentDeclarations"] == 0
        && (extraction["members"].as_u64().unwrap_or(0) > 0
            || extraction["requestLocalBindings"].as_u64().unwrap_or(0) > 0)
    {
        report["stages"]["persistence"]["status"] = json!("not_run");
        report["stages"]["persistence"]["reason"] = json!("separate_member_or_request_local_fact");
    }
    let overlays = CandidateOverlaySnapshot::default();
    let service = CandidateQueryService::new_for_family(
        Some(&handle),
        &overlays,
        rel,
        None,
        None,
        selection.semantic_family(),
    );
    let set = service.semantic_candidates(name, SemanticIntent::Neutral)?;
    let admitted: Vec<_> = set.all.iter().flat_map(|group| &group.candidates).collect();
    let ids: Vec<_> = admitted
        .iter()
        .filter_map(|candidate| candidate.persistent_id)
        .collect();
    anyhow::ensure!(
        ids.len() <= FACT_LIMIT,
        "production exact-name candidate limit exceeded"
    );
    // Final evidence returns through canonical typed rows by ID. Never turn
    // compact recall rows into final presentation content.
    let hydrated = handle.read(|store| store.declaration_view().by_ids(&ids))?;
    anyhow::ensure!(
        hydrated.len() == ids.len(),
        "diagnostic snapshot lost a recalled declaration"
    );
    let rows_by_id: HashMap<_, _> = hydrated.iter().map(|row| (row.id, row)).collect();
    let paths: Vec<_> = hydrated
        .iter()
        .map(|row| row.fact.path.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let candidate_versions = handle.read(|store| store.diagnostic_parser_versions(&paths))?;
    let stale_facts = candidate_versions
        .iter()
        .any(|(_, version)| *version != PARSER_FACT_VERSION);
    report["indexObservation"]["candidateParserVersions"] = json!(candidate_versions);
    report["stages"]["recall"] = stage(
        if set.coverage.truncated {
            "truncated"
        } else if stale_facts {
            "unknown"
        } else if ids.is_empty() {
            "absent"
        } else {
            "found"
        },
        if stale_facts {
            "candidate_parser_version_incompatible"
        } else {
            "production_exact_name_query"
        },
        ids.len(),
        crate::candidate_service::DEFAULT_EXACT_NAME_CANDIDATE_LIMIT,
        json!({"declarationIds":ids,"scanned":set.coverage.scanned,"coverage":set.coverage.declaration_state,"coverageReasons":set.coverage.declaration_reason_labels(),"reachContext":"same_as_query_def_without_engine_snapshot","nameTableLoaded":false}),
    );
    let presented = navigation_presentations(&set, false, rel);
    let selected_ids: HashSet<_> = admitted
        .iter()
        .filter(|candidate| {
            presented.iter().any(|display| {
                let projection = candidate.as_definition_candidate();
                projection.path == display.path && projection.range == display.range
            })
        })
        .filter_map(|candidate| candidate.persistent_id)
        .collect();
    let omitted_ids: Vec<_> = ids
        .iter()
        .copied()
        .filter(|id| !selected_ids.contains(id))
        .collect();
    let candidates: Vec<_> = presented.iter().map(|display| {
        let candidate = admitted.iter().find(|candidate| {
            let projection = candidate.as_definition_candidate();
            projection.path==display.path && projection.range==display.range
        }).expect("navigation only presents admitted candidates");
        let row = rows_by_id[&candidate.persistent_id.expect("persistent-only CLI")];
        json!({"declarationId":row.id,"name":row.fact.name,"kind":display.kind,"role":display.role,"path":row.fact.path,
            "range":{"startLine":display.range.start_line,"startCol":display.range.start_col,"endLine":display.range.end_line,"endCol":display.range.end_col},
            "tier":display.tier.as_str(),"confidence":display.confidence.as_str(),"reason":display.reason.as_str(),"fileRevision":row.revision_id,"contentHash":row.revision_hash})
    }).collect();
    report["stages"]["presentation"] = stage(
        if stale_facts {
            "unknown"
        } else if candidates.is_empty() {
            "absent"
        } else {
            "found"
        },
        "production_navigation_policy",
        candidates.len(),
        FACT_LIMIT,
        json!({"candidates":candidates,"omittedIds":omitted_ids,"omissionPolicy":"focused_groups_and_declaration_role","unrecalledCandidatesAreNotOmissions":true}),
    );
    report["stages"]["presentation"]["counts"]["omitted"] = json!(omitted_ids.len());
    Ok(())
}

fn not_run(report: &mut Value, reason: &str) -> Result<()> {
    for key in ["persistence", "recall", "presentation"] {
        report["stages"][key] = stage("not_run", reason, 0, FACT_LIMIT, json!({}));
    }
    Ok(())
}
