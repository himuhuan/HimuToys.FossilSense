//! Opt-in, in-process benchmark cases for the v1.4.2 semantic request path.
//!
//! The PowerShell harness runs the ignored dispatcher below one case at a
//! time.  Every value printed by this module is a privacy-safe aggregate
//! derived from a real parser, candidate resolver, source hydrator, reach
//! graph, or generation-pinned store operation.  It deliberately never emits
//! source text, symbol spellings, paths, or signatures.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Instant, UNIX_EPOCH};

use anyhow::{Context, Result};

use crate::call_model::{
    AnchorRole, CallForm, CallableAnchor, CallableKind, FactProvenance, LinkageDomain,
    SignatureFidelity, SignatureShape, SourcePosition, SourceRange,
};
use crate::call_service::CallReadHandle;
use crate::candidate_service::{
    CandidateOverlaySnapshot, CandidateQueryService, FileCandidateOverlay,
    DEFAULT_EXACT_NAME_CANDIDATE_LIMIT,
};
use crate::indexer::{index_workspace, IndexOptions};
use crate::query::{
    call_context_at, call_definition_presentations, hover_presentations, resolve_counterparts,
    signature_presentations, ArgumentState, CallSiteContext, CallableCandidateSet,
    ContextReliability, CounterpartEvidence, SourceExcerptOutcome, SourceExcerptRange,
    SourceExcerptReader, SourceExcerptRevision,
};
use crate::reachability::{OpenReason, ReachGraph, MAX_REACH_NODES};

const CASE_ENV: &str = "FOSSILSENSE_V142_BENCH_CASE";
const BENCHMARK_ROOT_ENV: &str = "FOSSILSENSE_V142_BENCHMARK_ROOT";

const HIGH_DUPLICATION_CASE: &str = "v142-high-duplication-callable-query";
const COUNTERPART_SCAN_CAP_CASE: &str = "v142-counterpart-scan-cap";
const LARGE_RECORD_HYDRATION_CASE: &str = "v142-large-record-range-hydration";
const MULTI_DIRTY_OVERLAY_CASE: &str = "v142-multi-dirty-overlay-merge";
const SIGNATURE_RETRIGGER_CASE: &str = "v142-signature-help-retrigger";
const CONCURRENT_PUBLICATION_CASE: &str = "v142-concurrent-publication-hover-definition";

#[derive(Debug, Default)]
struct BenchmarkMetrics {
    callable_query_us: u64,
    candidate_rows_scanned: u64,
    candidate_rows_filtered: u64,
    candidate_rows_grouped: u64,
    candidate_rows_returned: u64,
    candidate_raw: u64,
    candidate_filtered: u64,
    candidate_grouped: u64,
    candidate_returned: u64,
    candidate_query_truncated: u64,
    arity_compatible: u64,
    arity_unknown: u64,
    arity_incompatible: u64,
    arity_mismatch_fallback: u64,
    counterpart_graph_us: u64,
    counterpart_edges: u64,
    counterpart_groups: u64,
    counterpart_strict: u64,
    counterpart_ambiguous: u64,
    counterpart_incomplete: u64,
    candidate_scan_cap: u64,
    candidate_scan_observed: u64,
    reach_nodes_visited: u64,
    hydration_us: u64,
    hydration_count: u64,
    hydration_bytes: u64,
    hydration_sections: u64,
    hydration_file_bytes: u64,
    hydration_requested_bytes: u64,
    hydration_revision_rejections: u64,
    overlay_merge_us: u64,
    overlay_parse_us: u64,
    overlay_documents: u64,
    signature_help_requests: u64,
    signature_help_p50_us: u64,
    signature_help_p95_us: u64,
    concurrent_query_p50_us: u64,
    concurrent_query_p95_us: u64,
    publication_conflicts: u64,
    generation_mismatches: u64,
    query_us: u64,
    reach_us: u64,
    coverage_scanned: u64,
    coverage_open: u64,
    coverage_truncated: u64,
    fallback_used: u64,
}

impl BenchmarkMetrics {
    fn absorb_candidates(&mut self, set: &CallableCandidateSet, returned: usize) {
        let aggregate = set.metrics();
        self.candidate_rows_scanned = self
            .candidate_rows_scanned
            .saturating_add(aggregate.raw_candidates as u64);
        self.candidate_rows_filtered = self
            .candidate_rows_filtered
            .saturating_add(aggregate.filtered_candidates as u64);
        self.candidate_rows_grouped = self
            .candidate_rows_grouped
            .saturating_add(aggregate.grouped_candidates as u64);
        self.candidate_rows_returned = self.candidate_rows_returned.saturating_add(returned as u64);
        self.candidate_raw = self
            .candidate_raw
            .saturating_add(aggregate.raw_candidates as u64);
        self.candidate_filtered = self
            .candidate_filtered
            .saturating_add(aggregate.filtered_candidates as u64);
        self.candidate_grouped = self
            .candidate_grouped
            .saturating_add(aggregate.grouped_candidates as u64);
        self.candidate_returned = self.candidate_returned.saturating_add(returned as u64);
        self.arity_compatible = self
            .arity_compatible
            .saturating_add(aggregate.arity_compatible as u64);
        self.arity_unknown = self
            .arity_unknown
            .saturating_add(aggregate.arity_unknown as u64);
        self.arity_incompatible = self
            .arity_incompatible
            .saturating_add(aggregate.arity_incompatible as u64);
        self.counterpart_strict = self
            .counterpart_strict
            .saturating_add(aggregate.counterpart_strict as u64);
        self.counterpart_ambiguous = self
            .counterpart_ambiguous
            .saturating_add(aggregate.counterpart_ambiguous as u64);
        self.counterpart_groups = self
            .counterpart_groups
            .saturating_add(set.groups.len() as u64);
        self.counterpart_edges = self
            .counterpart_edges
            .saturating_add(counterpart_edge_count(&set.groups));
        self.coverage_scanned = self
            .coverage_scanned
            .saturating_add(set.coverage.scanned as u64);
        self.coverage_open = self
            .coverage_open
            .saturating_add(u64::from(set.coverage.scope_open));
        self.coverage_truncated = self
            .coverage_truncated
            .saturating_add(u64::from(set.coverage.truncated));
        self.candidate_query_truncated = self
            .candidate_query_truncated
            .saturating_add(u64::from(set.coverage.truncated));
        self.arity_mismatch_fallback = self
            .arity_mismatch_fallback
            .saturating_add(u64::from(set.arity_mismatch_fallback));
        self.fallback_used = self.fallback_used.saturating_add(u64::from(
            set.arity_mismatch_fallback || set.coverage.incomplete_reason.is_some(),
        ));
    }

    fn print(&self) {
        macro_rules! metric {
            ($field:ident) => {
                println!(concat!(stringify!($field), ": {}"), self.$field)
            };
        }
        metric!(callable_query_us);
        metric!(candidate_rows_scanned);
        metric!(candidate_rows_filtered);
        metric!(candidate_rows_grouped);
        metric!(candidate_rows_returned);
        metric!(candidate_raw);
        metric!(candidate_filtered);
        metric!(candidate_grouped);
        metric!(candidate_returned);
        metric!(candidate_query_truncated);
        metric!(arity_compatible);
        metric!(arity_unknown);
        metric!(arity_incompatible);
        metric!(arity_mismatch_fallback);
        metric!(counterpart_graph_us);
        metric!(counterpart_edges);
        metric!(counterpart_groups);
        metric!(counterpart_strict);
        metric!(counterpart_ambiguous);
        metric!(counterpart_incomplete);
        metric!(candidate_scan_cap);
        metric!(candidate_scan_observed);
        metric!(reach_nodes_visited);
        metric!(hydration_us);
        metric!(hydration_count);
        metric!(hydration_bytes);
        metric!(hydration_sections);
        metric!(hydration_file_bytes);
        metric!(hydration_requested_bytes);
        metric!(hydration_revision_rejections);
        metric!(overlay_merge_us);
        metric!(overlay_parse_us);
        metric!(overlay_documents);
        metric!(signature_help_requests);
        metric!(signature_help_p50_us);
        metric!(signature_help_p95_us);
        metric!(concurrent_query_p50_us);
        metric!(concurrent_query_p95_us);
        metric!(publication_conflicts);
        metric!(generation_mismatches);
        metric!(query_us);
        metric!(reach_us);
        metric!(coverage_scanned);
        metric!(coverage_open);
        metric!(coverage_truncated);
        metric!(fallback_used);
    }
}

#[test]
#[ignore = "opt-in v1.4.2 semantic benchmark; run through benchmark_v142_semantics.ps1"]
fn benchmark_v142_case() -> Result<()> {
    let case_id = std::env::var(CASE_ENV).context("semantic benchmark case was not selected")?;
    let metrics = match case_id.as_str() {
        HIGH_DUPLICATION_CASE => high_duplication_callable_query()?,
        COUNTERPART_SCAN_CAP_CASE => counterpart_scan_cap()?,
        LARGE_RECORD_HYDRATION_CASE => large_record_range_hydration()?,
        MULTI_DIRTY_OVERLAY_CASE => multi_dirty_overlay_merge()?,
        SIGNATURE_RETRIGGER_CASE => signature_help_retrigger()?,
        CONCURRENT_PUBLICATION_CASE => concurrent_publication_hover_definition()?,
        _ => anyhow::bail!("unknown semantic benchmark case"),
    };
    metrics.print();
    Ok(())
}

fn high_duplication_callable_query() -> Result<BenchmarkMetrics> {
    let anchors = (0..384)
        .map(|index| {
            let arity = if index % 2 == 0 { 1 } else { 2 };
            benchmark_anchor(
                "include/duplicates.h",
                "candidate",
                AnchorRole::Declaration,
                arity,
                index,
            )
        })
        .collect();
    let overlays = CandidateOverlaySnapshot::new(
        1,
        vec![FileCandidateOverlay::new(
            "include/duplicates.h".into(),
            anchors,
            Vec::new(),
        )],
    );
    let service = CandidateQueryService::new(None, &overlays, "src/request.c", None, None);
    let started = Instant::now();
    let set = service.callable_candidates("candidate", Some(complete_context("candidate", 1)))?;
    let elapsed = elapsed_us(started);
    let returned = call_definition_presentations(&set.groups).len();
    let mut metrics = BenchmarkMetrics {
        callable_query_us: elapsed,
        query_us: elapsed,
        ..BenchmarkMetrics::default()
    };
    metrics.absorb_candidates(&set, returned);
    Ok(metrics)
}

fn counterpart_scan_cap() -> Result<BenchmarkMetrics> {
    const EXTRA_DURABLE_CANDIDATES: usize = 128;

    // Populate a real schema-16 store with more exact-name anchors than one
    // request is allowed to materialize. This exercises the SQL LIMIT+1 path,
    // rather than constructing an already-unbounded in-memory candidate Vec.
    let workspace = benchmark_tempdir("counterpart-cap")?;
    let mut declarations = String::new();
    for index in 0..(DEFAULT_EXACT_NAME_CANDIDATE_LIMIT + EXTRA_DURABLE_CANDIDATES) {
        declarations.push_str(&format!("int counterpart(int value_{index:04});\n"));
    }
    fs::write(workspace.path().join("duplicates.h"), declarations)?;
    let database = workspace.path().join("counterpart.sqlite");
    index_workspace(
        workspace.path(),
        IndexOptions {
            db_path: Some(database.clone()),
            force: true,
            parse_threads: Some(1),
            ..IndexOptions::default()
        },
        |_| {},
    )?;
    let handle = CallReadHandle::capture(database)?;
    let (bounded_rows, bounded_truncated) = handle.read(|store| {
        store
            .call_fact_view()
            .anchors_by_name_limited("counterpart", DEFAULT_EXACT_NAME_CANDIDATE_LIMIT)
    })?;
    if !bounded_truncated || bounded_rows.len() != DEFAULT_EXACT_NAME_CANDIDATE_LIMIT {
        anyhow::bail!("durable exact-name candidate query did not stop at its configured cap");
    }

    let source_path = "000-source.c";
    let overlays = CandidateOverlaySnapshot::new(
        1,
        vec![FileCandidateOverlay::new(
            source_path.into(),
            vec![benchmark_anchor(
                source_path,
                "counterpart",
                AnchorRole::Definition,
                1,
                0,
            )],
            Vec::new(),
        )],
    );

    // A wide include fan-out exceeds the production reachability node budget.
    // The resulting open scope must disable counterpart uniqueness even though
    // the returned ordinary candidates remain navigable.
    let mut edges = Vec::with_capacity(MAX_REACH_NODES + 1);
    edges.push((source_path.to_string(), "duplicates.h".to_string()));
    for index in 0..MAX_REACH_NODES {
        edges.push((source_path.to_string(), format!("fanout/{index:05}.h")));
    }
    let graph = ReachGraph::new(edges, Vec::new(), Vec::new());
    let reach_started = Instant::now();
    let source_reach = graph.reachable(source_path);
    let reach_us = elapsed_us(reach_started);
    if !source_reach.open
        || source_reach.reason != Some(OpenReason::NodeLimit)
        || source_reach.files.len() != MAX_REACH_NODES
    {
        anyhow::bail!("reachability did not stop at the production node cap");
    }
    let service = CandidateQueryService::new(
        Some(&handle),
        &overlays,
        source_path,
        Some(source_reach.as_ref()),
        Some(&graph),
    );
    let query_started = Instant::now();
    let set = service.callable_candidates("counterpart", None)?;
    let query_us = elapsed_us(query_started);
    let counterpart_started = Instant::now();
    let regrouped = resolve_counterparts(
        &set.anchors,
        &HashMap::from([(source_path.to_string(), source_reach.as_ref().clone())]),
        &set.coverage,
    );
    let counterpart_graph_us = elapsed_us(counterpart_started);
    let counterpart_incomplete = regrouped
        .iter()
        .filter(|group| group.counterpart_evidence == CounterpartEvidence::IncompleteCoverage)
        .count();
    if !set.coverage.truncated
        || !set.coverage.scope_open
        || set.anchors.len() > DEFAULT_EXACT_NAME_CANDIDATE_LIMIT
        || counterpart_incomplete != regrouped.len()
    {
        anyhow::bail!("counterpart cap did not degrade to bounded incomplete coverage");
    }
    let returned = call_definition_presentations(&regrouped).len();
    let mut metrics = BenchmarkMetrics {
        callable_query_us: query_us,
        counterpart_graph_us,
        counterpart_incomplete: counterpart_incomplete as u64,
        candidate_scan_cap: DEFAULT_EXACT_NAME_CANDIDATE_LIMIT as u64,
        candidate_scan_observed: bounded_rows.len() as u64,
        reach_nodes_visited: source_reach.files.len() as u64,
        query_us,
        reach_us,
        ..BenchmarkMetrics::default()
    };
    metrics.absorb_candidates(&set, returned);
    Ok(metrics)
}

fn large_record_range_hydration() -> Result<BenchmarkMetrics> {
    const FILE_PADDING_BYTES: usize = 8 * 1024 * 1024;

    let workspace = benchmark_tempdir("large-record")?;
    let source_path = workspace.path().join("large-record.h");
    let mut record = String::from("typedef struct LargeRecord {\n");
    for index in 0..1_900 {
        record.push_str(&format!("    unsigned long field_{index:04};\n"));
    }
    record.push_str("} LargeRecord;\n");
    let mut file = File::create(&source_path)?;
    write_padding(&mut file, FILE_PADDING_BYTES)?;
    let range_start = FILE_PADDING_BYTES;
    file.write_all(record.as_bytes())?;
    let range_end = range_start + record.len();
    write_padding(&mut file, FILE_PADDING_BYTES)?;
    file.flush()?;
    drop(file);

    let expected = source_revision(&source_path, record.as_bytes())?;
    let range = SourceExcerptRange {
        start: range_start,
        end: range_end,
    };
    let reader = SourceExcerptReader::default();
    let hydration_count = 24_u64;
    let started = Instant::now();
    let mut hydration_bytes = 0_u64;
    for _ in 0..hydration_count {
        match reader.read_file(&source_path, range, expected) {
            SourceExcerptOutcome::Complete { bytes_read, .. } => {
                if bytes_read != record.len() {
                    anyhow::bail!("range hydration read outside the requested byte count");
                }
                hydration_bytes = hydration_bytes.saturating_add(bytes_read as u64);
            }
            SourceExcerptOutcome::Omitted(reason) => {
                anyhow::bail!("large record hydration was omitted: {}", reason.as_str());
            }
        }
    }

    // Mutate the real file after capturing its revision. A guarded range read
    // must reject the stale revision before presenting bytes from a different
    // file state.
    let mut changed = OpenOptions::new().append(true).open(&source_path)?;
    changed.write_all(b"\n")?;
    changed.flush()?;
    drop(changed);
    let hydration_revision_rejections = match reader.read_file(&source_path, range, expected) {
        SourceExcerptOutcome::Omitted(reason) if reason.as_str() == "source range is stale" => 1,
        _ => anyhow::bail!("range hydration accepted a stale on-disk revision"),
    };
    let hydration_us = elapsed_us(started);
    let expected_hydration_bytes = hydration_count.saturating_mul(record.len() as u64);
    if hydration_bytes != expected_hydration_bytes || expected.size <= record.len() as u64 {
        anyhow::bail!("range hydration was not bounded to the record source range");
    }
    Ok(BenchmarkMetrics {
        hydration_us,
        hydration_count,
        hydration_bytes,
        hydration_sections: hydration_count,
        hydration_file_bytes: expected.size,
        hydration_requested_bytes: record.len() as u64,
        hydration_revision_rejections,
        query_us: hydration_us,
        ..BenchmarkMetrics::default()
    })
}

fn multi_dirty_overlay_merge() -> Result<BenchmarkMetrics> {
    const DOCUMENTS: usize = 48;
    // Overlay preparation includes the live parse of every dirty buffer. The
    // timer intentionally starts before parsing so this measures the request
    // cost the server actually pays, not only HashMap assembly afterwards.
    let merge_started = Instant::now();
    let mut overlay_parse_us = 0_u64;
    let mut files = Vec::with_capacity(DOCUMENTS);
    let mut paths = Vec::with_capacity(DOCUMENTS);
    for index in 0..DOCUMENTS {
        let path = format!("dirty/d{index:03}.h");
        let include = if index + 1 < DOCUMENTS {
            format!("#include \"d{:03}.h\"\n", index + 1)
        } else {
            String::new()
        };
        let source: Arc<str> = format!(
            "{include}typedef struct Record{index} {{ int field; }} Alias{index};\nint overlay_call(int value);\n"
        )
        .into();
        let parse_started = Instant::now();
        let parsed = crate::parser::parse(Path::new(&path), source.as_ref());
        overlay_parse_us = overlay_parse_us.saturating_add(elapsed_us(parse_started));
        files.push(FileCandidateOverlay::from_index_with_text(
            path.clone(),
            &parsed,
            source,
        ));
        paths.push(path);
    }
    let mut overlays = CandidateOverlaySnapshot::new(1, files);
    let path_refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    overlays.refresh_reach_graph(None, path_refs.iter().copied(), &[]);
    let overlay_merge_us = elapsed_us(merge_started);
    let reach_started = Instant::now();
    let reach = overlays
        .effective_reach_graph(None)
        .context("dirty overlay did not produce a reach graph")?
        .reachable(&paths[0]);
    let reach_us = elapsed_us(reach_started);
    let service =
        CandidateQueryService::new(None, &overlays, &paths[0], Some(reach.as_ref()), None);
    let query_started = Instant::now();
    let set = service.callable_candidates("overlay_call", None)?;
    let query_us = elapsed_us(query_started);
    let returned = hover_presentations(&set.groups).len();
    let mut metrics = BenchmarkMetrics {
        callable_query_us: query_us,
        overlay_merge_us,
        overlay_parse_us,
        overlay_documents: DOCUMENTS as u64,
        query_us,
        reach_us,
        ..BenchmarkMetrics::default()
    };
    metrics.absorb_candidates(&set, returned);
    Ok(metrics)
}

fn signature_help_retrigger() -> Result<BenchmarkMetrics> {
    let anchors = (0..10)
        .map(|index| {
            benchmark_anchor(
                "include/signatures.h",
                "signature",
                AnchorRole::Declaration,
                index,
                index as usize,
            )
        })
        .collect();
    let overlays = CandidateOverlaySnapshot::new(
        1,
        vec![FileCandidateOverlay::new(
            "include/signatures.h".into(),
            anchors,
            Vec::new(),
        )],
    );
    let service = CandidateQueryService::new(None, &overlays, "src/request.c", None, None);
    let requests = [
        "signature(",
        "signature(1",
        "signature(1, ",
        "signature(1, 2",
        "signature((1 + 2), 3, ",
        "signature(1, nested(2, 3), 4",
    ];
    let request_count = 720_usize;
    let mut durations = Vec::with_capacity(request_count);
    let mut metrics = BenchmarkMetrics::default();
    for index in 0..request_count {
        let source = requests[index % requests.len()];
        let started = Instant::now();
        let context = partial_context(source)?;
        let set = service.callable_candidates("signature", Some(context))?;
        let returned = signature_presentations(&set.groups).len();
        let elapsed = elapsed_us(started);
        durations.push(elapsed);
        metrics.callable_query_us = metrics.callable_query_us.saturating_add(elapsed);
        metrics.query_us = metrics.query_us.saturating_add(elapsed);
        metrics.absorb_candidates(&set, returned);
    }
    metrics.signature_help_requests = request_count as u64;
    metrics.signature_help_p50_us = percentile(&mut durations, 50);
    metrics.signature_help_p95_us = percentile(&mut durations, 95);
    Ok(metrics)
}

fn concurrent_publication_hover_definition() -> Result<BenchmarkMetrics> {
    let workspace = tempfile::tempdir()?;
    let workspace_path = workspace.path().to_path_buf();
    write_publication_workspace(&workspace_path, false)?;
    index_workspace(
        &workspace_path,
        IndexOptions {
            force: true,
            parse_threads: Some(1),
            ..IndexOptions::default()
        },
        |_| {},
    )?;
    let index_directory = crate::pathing::default_index_directory(&workspace_path)?;
    let _cleanup = IndexFamilyCleanup(index_directory);
    let old_path = crate::pathing::default_index_path(&workspace_path)?;
    let old_handle = CallReadHandle::capture(old_path)?;
    let empty_overlay = CandidateOverlaySnapshot::default();
    let initial = query_store_candidates(&old_handle, &empty_overlay)?;
    let expected_old = candidate_signatures(&initial);
    if expected_old.is_empty() {
        anyhow::bail!("initial publication had no callable candidate");
    }

    let publisher_workspace = workspace_path.clone();
    let publisher = thread::spawn(move || -> Result<()> {
        write_publication_workspace(&publisher_workspace, true)?;
        index_workspace(
            &publisher_workspace,
            IndexOptions {
                force: true,
                parse_threads: Some(1),
                ..IndexOptions::default()
            },
            |_| {},
        )?;
        Ok(())
    });

    let mut durations = Vec::with_capacity(160);
    let mut metrics = BenchmarkMetrics::default();
    for _ in 0..160 {
        let started = Instant::now();
        match query_store_candidates(&old_handle, &empty_overlay) {
            Ok(set) => {
                let hover_count = hover_presentations(&set.groups).len();
                let definition_count = call_definition_presentations(&set.groups).len();
                if candidate_signatures(&set) != expected_old {
                    metrics.generation_mismatches += 1;
                }
                metrics.absorb_candidates(&set, hover_count + definition_count);
            }
            Err(_) => metrics.generation_mismatches += 1,
        }
        let elapsed = elapsed_us(started);
        durations.push(elapsed);
        metrics.query_us = metrics.query_us.saturating_add(elapsed);
        metrics.callable_query_us = metrics.callable_query_us.saturating_add(elapsed);
    }

    match publisher.join() {
        Ok(Ok(())) => {}
        Ok(Err(_)) | Err(_) => metrics.publication_conflicts += 1,
    }
    let new_path = crate::pathing::default_index_path(&workspace_path)?;
    let new_handle = CallReadHandle::capture(new_path)?;
    let published = query_store_candidates(&new_handle, &empty_overlay)?;
    let new_signatures = candidate_signatures(&published);
    if new_handle.generation.0 <= old_handle.generation.0
        || new_signatures.is_empty()
        || new_signatures == expected_old
    {
        metrics.generation_mismatches += 1;
    }
    metrics.concurrent_query_p50_us = percentile(&mut durations, 50);
    metrics.concurrent_query_p95_us = percentile(&mut durations, 95);
    drop(new_handle);
    drop(old_handle);
    Ok(metrics)
}

fn query_store_candidates(
    handle: &CallReadHandle,
    overlays: &CandidateOverlaySnapshot,
) -> Result<CallableCandidateSet> {
    CandidateQueryService::new(Some(handle), overlays, "main.c", None, None)
        .callable_candidates("published", None)
}

fn candidate_signatures(set: &CallableCandidateSet) -> Vec<String> {
    let mut signatures: Vec<_> = set
        .anchors
        .iter()
        .map(|candidate| candidate.anchor.canonical_signature.clone())
        .collect();
    signatures.sort();
    signatures
}

fn write_publication_workspace(root: &Path, revised: bool) -> Result<()> {
    let (return_type, parameter_type) = if revised {
        ("long", "long")
    } else {
        ("int", "int")
    };
    fs::write(
        root.join("api.h"),
        format!("{return_type} published({parameter_type} value);\n"),
    )?;
    fs::write(
        root.join("api.c"),
        format!(
            "#include \"api.h\"\n{return_type} published({parameter_type} value) {{ return value; }}\n"
        ),
    )?;
    fs::write(
        root.join("main.c"),
        "#include \"api.h\"\nint main(void) { return (int)published(1); }\n",
    )?;
    for index in 0..40 {
        fs::write(
            root.join(format!("filler-{index:02}.c")),
            format!("int filler_{index:02}(int value) {{ return value + {index}; }}\n"),
        )?;
    }
    Ok(())
}

struct IndexFamilyCleanup(PathBuf);

impl Drop for IndexFamilyCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn benchmark_tempdir(prefix: &str) -> Result<tempfile::TempDir> {
    let root = std::env::var_os(BENCHMARK_ROOT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    fs::create_dir_all(&root)
        .with_context(|| "failed to create the semantic benchmark scratch directory")?;
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(root)
        .with_context(|| "failed to create a semantic benchmark workspace")
}

fn write_padding(file: &mut File, byte_count: usize) -> Result<()> {
    const CHUNK_BYTES: usize = 64 * 1024;
    let chunk = vec![b' '; CHUNK_BYTES];
    let mut remaining = byte_count;
    while remaining > 0 {
        let count = remaining.min(chunk.len());
        file.write_all(&chunk[..count])?;
        remaining -= count;
    }
    Ok(())
}

fn source_revision(path: &Path, excerpt: &[u8]) -> Result<SourceExcerptRevision> {
    let metadata = fs::metadata(path)?;
    let mtime_ns = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0);
    Ok(SourceExcerptRevision {
        size: metadata.len(),
        mtime_ns,
        excerpt_hash: *blake3::hash(excerpt).as_bytes(),
    })
}

fn benchmark_anchor(
    path: &str,
    name: &str,
    role: AnchorRole,
    arity: u32,
    ordinal: usize,
) -> CallableAnchor {
    let parameters = (0..arity)
        .map(|index| format!("int p{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let signature = format!("int {name}({parameters})");
    let start = ordinal.saturating_mul(32);
    let range = SourceRange {
        start: SourcePosition {
            line: ordinal as u32,
            character: 4,
        },
        end: SourcePosition {
            line: ordinal as u32,
            character: 4 + name.len() as u32,
        },
        start_byte: start,
        end_byte: start + name.len(),
    };
    CallableAnchor {
        path: path.to_string(),
        name: name.to_string(),
        qualified_name: name.to_string(),
        owner: None,
        owner_kind: None,
        kind: CallableKind::Function,
        role,
        linkage: LinkageDomain::External,
        signature: SignatureShape {
            normalized: signature.clone(),
            min_arity: Some(arity),
            max_arity: Some(arity),
            variadic: false,
        },
        canonical_signature: signature.clone(),
        presentation_signature: signature.clone(),
        signature_fidelity: SignatureFidelity::AstExact,
        name_range: range,
        declaration_range: range,
        body_range: (role == AnchorRole::Definition).then_some(range),
        guard: None,
        provenance: FactProvenance::Ast,
        syntax_error_overlap: false,
        entity_key: format!("entity-{ordinal}"),
        anchor_fingerprint: format!("anchor-{ordinal}"),
    }
}

fn complete_context(name: &str, arity: u32) -> CallSiteContext {
    CallSiteContext {
        callee_name: name.to_string(),
        qualified_name: None,
        form: CallForm::DirectName,
        callee_range: benchmark_range(name.len()),
        argument_count: Some(arity),
        argument_state: ArgumentState::Complete,
        reliability: ContextReliability::Reliable,
    }
}

fn partial_context(source: &str) -> Result<CallSiteContext> {
    let character = source.encode_utf16().count() as u32;
    let context =
        call_context_at(source, 0, character).context("signature context was not parsed")?;
    Ok(CallSiteContext {
        callee_name: context.name,
        qualified_name: context.qualified_name,
        form: context.form,
        callee_range: benchmark_range(9),
        argument_count: None,
        argument_state: context.argument_state,
        reliability: ContextReliability::Reliable,
    })
}

fn benchmark_range(width: usize) -> SourceRange {
    SourceRange {
        start: SourcePosition {
            line: 0,
            character: 0,
        },
        end: SourcePosition {
            line: 0,
            character: width as u32,
        },
        start_byte: 0,
        end_byte: width,
    }
}

fn counterpart_edge_count(groups: &[crate::query::callables::CallableVariantGroup]) -> u64 {
    counterpart_edge_count_from_evidence(groups.iter().map(|group| group.counterpart_evidence))
}

fn counterpart_edge_count_from_evidence(
    evidence: impl IntoIterator<Item = CounterpartEvidence>,
) -> u64 {
    let mut strict_edges = 0_u64;
    let mut ambiguous_degree_sum = 0_u64;
    for evidence in evidence {
        match evidence {
            CounterpartEvidence::StrictOneToOne => strict_edges += 1,
            CounterpartEvidence::Ambiguous { candidate_edges } => {
                ambiguous_degree_sum = ambiguous_degree_sum.saturating_add(candidate_edges as u64);
            }
            CounterpartEvidence::Unpaired | CounterpartEvidence::IncompleteCoverage => {}
        }
    }
    debug_assert_eq!(
        ambiguous_degree_sum % 2,
        0,
        "a complete bipartite counterpart graph has two endpoint degrees per edge"
    );
    strict_edges.saturating_add(ambiguous_degree_sum / 2)
}

#[test]
fn counterpart_edge_metric_counts_each_bipartite_edge_once() -> Result<()> {
    let source_path = "src/ambiguous.c";
    let header_paths = ["include/a.h", "include/b.h"];
    let overlays = CandidateOverlaySnapshot::new(
        1,
        vec![
            FileCandidateOverlay::new(
                source_path.into(),
                vec![benchmark_anchor(
                    source_path,
                    "ambiguous",
                    AnchorRole::Definition,
                    1,
                    0,
                )],
                Vec::new(),
            ),
            FileCandidateOverlay::new(
                header_paths[0].into(),
                vec![benchmark_anchor(
                    header_paths[0],
                    "ambiguous",
                    AnchorRole::Declaration,
                    1,
                    1,
                )],
                Vec::new(),
            ),
            FileCandidateOverlay::new(
                header_paths[1].into(),
                vec![benchmark_anchor(
                    header_paths[1],
                    "ambiguous",
                    AnchorRole::Declaration,
                    1,
                    2,
                )],
                Vec::new(),
            ),
        ],
    );
    let graph = ReachGraph::new(
        header_paths
            .iter()
            .map(|header| (source_path.to_string(), (*header).to_string()))
            .collect(),
        Vec::new(),
        Vec::new(),
    );
    let service = CandidateQueryService::new(None, &overlays, source_path, None, Some(&graph));
    let set = service.callable_candidates("ambiguous", None)?;
    let ambiguous_degree_sum: usize = set
        .groups
        .iter()
        .filter_map(|group| match group.counterpart_evidence {
            CounterpartEvidence::Ambiguous { candidate_edges } => Some(candidate_edges),
            _ => None,
        })
        .sum();
    assert_eq!(ambiguous_degree_sum, 4);
    assert_eq!(counterpart_edge_count(&set.groups), 2);
    Ok(())
}

fn percentile(samples: &mut [u64], percentile: usize) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    samples.sort_unstable();
    let index = (samples.len() - 1).saturating_mul(percentile) / 100;
    samples[index]
}

fn elapsed_us(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}
