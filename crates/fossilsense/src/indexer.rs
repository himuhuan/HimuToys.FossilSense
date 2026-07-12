use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;

use crate::config::{resolve_include_roots, WorkspaceConfig};
use crate::pathing::{
    canonical_workspace, default_index_path, default_index_staging_path, normalize_abs_path,
    publish_default_index, relative_slash_path,
};
use crate::progress::{IndexStats, IndexStatus};
use crate::store::IndexStore;

mod candidates;
mod include_edges;
mod parse_pipeline;
mod progress_limiter;

use candidates::{
    candidate_for_path, canonicalize_existing_prefix, discover_candidates,
    discover_external_candidates, DEFAULT_EXTERNAL_MAX_BYTES, DEFAULT_EXTERNAL_MAX_FILES,
};
use include_edges::{build_include_edges, sql_affected_include_edge_sources};
use parse_pipeline::{parse_and_write_changed, parse_thread_count};
use progress_limiter::ProgressLimiter;

#[derive(Debug, Clone, Default)]
pub struct IndexOptions {
    pub db_path: Option<PathBuf>,
    pub force: bool,
    /// External include reference directories forwarded from the LSP client,
    /// merged with `fossilsense.json`'s `includePaths`.
    pub include_paths: Vec<String>,
    /// Override the per-root external file-count cap (defaults to ~20k).
    pub external_max_files: Option<usize>,
    /// Override the per-root external byte cap (defaults to ~512 MB).
    pub external_max_bytes: Option<u64>,
    /// Override parser worker count. Defaults to a small bounded pool.
    pub parse_threads: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirtyFileKind {
    Upsert,
    Delete,
}

#[derive(Debug, Clone)]
pub struct DirtyFileChange {
    pub absolute_path: PathBuf,
    pub kind: DirtyFileKind,
}

pub fn index_workspace(
    workspace: impl AsRef<Path>,
    options: IndexOptions,
    mut progress: impl FnMut(IndexStatus),
) -> Result<IndexStats> {
    let started = Instant::now();
    let workspace = canonical_workspace(workspace)?;
    let workspace_display = workspace.display().to_string();
    let explicit_db_path = options.db_path.clone();
    let (db_path, side_by_side_publication, previous_generation) =
        if let Some(path) = explicit_db_path.as_ref() {
            (path.clone(), false, 0)
        } else {
            let active = match default_index_path(&workspace) {
                Ok(path) => path,
                Err(_) if options.force => {
                    crate::pathing::default_index_directory(&workspace)?.join("index.sqlite")
                }
                Err(error) => return Err(error),
            };
            let active_exists = active.is_file();
            let current_schema =
                active_exists && IndexStore::has_current_schema(&active).unwrap_or(false);
            if options.force || !active_exists || !current_schema {
                let previous_generation = if active_exists {
                    IndexStore::open_readonly(&active)
                        .and_then(|store| store.semantic_generation())
                        .unwrap_or(0)
                } else {
                    latest_default_generation(&workspace)
                };
                (
                    default_index_staging_path(&workspace)?,
                    true,
                    previous_generation,
                )
            } else {
                (active, false, 0)
            }
        };
    let database_existed = db_path.exists();
    let mut stats = IndexStats::default();
    progress(IndexStatus::indexing_phase(
        workspace_display.clone(),
        &stats,
        "discovering",
    ));

    let (config, config_issue) = WorkspaceConfig::load(&workspace);
    if let Some(issue) = &config_issue {
        progress(IndexStatus::indexing_with_message(
            workspace_display.clone(),
            &stats,
            issue.message.clone(),
        ));
    }

    // External include reference directories: merge config + client-forwarded
    // entries, then validate against the filesystem. Invalid entries are skipped
    // with a note; never fatal.
    let mut include_entries = config.include_paths.clone();
    include_entries.extend(options.include_paths.iter().cloned());
    let (include_roots, include_issues) = resolve_include_roots(&include_entries);

    let max_files = options
        .external_max_files
        .unwrap_or(DEFAULT_EXTERNAL_MAX_FILES);
    let max_bytes = options
        .external_max_bytes
        .unwrap_or(DEFAULT_EXTERNAL_MAX_BYTES);

    let discover_started = Instant::now();
    let mut candidates = discover_candidates(&workspace, &config)?;
    let (external_candidates, external_issues) =
        discover_external_candidates(&include_roots, &config, max_files, max_bytes);
    candidates.extend(external_candidates);
    stats.discover_ms = discover_started.elapsed().as_millis();
    stats.total_files = candidates.len();

    for issue in include_issues.into_iter().chain(external_issues) {
        progress(IndexStatus::indexing_with_message(
            workspace_display.clone(),
            &stats,
            issue.message,
        ));
    }
    let seen_paths: HashSet<String> = candidates
        .iter()
        .map(|candidate| candidate.fingerprint.path.clone())
        .collect();

    // Explicit full-build databases have no in-process request readers. Default
    // full builds write an unpublished generation file and switch the manifest
    // only after facts, indexes, validation, and checkpointing all complete.
    let defer_call_indexes = side_by_side_publication
        || ((!database_existed || options.force) && explicit_db_path.is_some());
    let mut store = if defer_call_indexes {
        IndexStore::open_for_full_rebuild(&db_path, &workspace)?
    } else {
        IndexStore::open(&db_path, &workspace)?
    };
    if side_by_side_publication {
        store.seed_semantic_generation(previous_generation)?;
    }
    progress(IndexStatus::indexing_phase(
        workspace_display.clone(),
        &stats,
        "checking",
    ));

    let check_started = Instant::now();
    let mut changed = Vec::new();
    let mut check_progress = ProgressLimiter::new();
    let candidate_paths: Vec<String> = candidates
        .iter()
        .map(|candidate| candidate.fingerprint.path.clone())
        .collect();
    let stored_files = store.stored_files(&candidate_paths)?;
    let replace_all_files = side_by_side_publication || options.force || stored_files.is_empty();
    let build = store.begin_index_build(replace_all_files)?;
    for candidate in candidates {
        // Fast incremental check: reading and hashing every unchanged workspace
        // file defeats the point of an incremental pass. Size + mtime is the
        // cheap gate; content hash is recomputed only for files that pass it.
        let unchanged = stored_files
            .get(&candidate.fingerprint.path)
            .is_some_and(|stored| {
                candidate.fingerprint.mtime_ns != 0
                    && stored.size == candidate.fingerprint.size
                    && stored.mtime_ns == candidate.fingerprint.mtime_ns
            });

        if unchanged && !options.force {
            stats.skipped_files += 1;
            stats.processed_files += 1;
            check_progress.maybe_emit(&mut progress, &workspace_display, &stats, "checking");
        } else {
            changed.push(candidate);
        }
    }
    check_progress.emit_if_changed(&mut progress, &workspace_display, &stats, "checking");
    stats.check_ms = check_started.elapsed().as_millis();

    parse_and_write_changed(
        changed,
        parse_thread_count(options.parse_threads),
        build,
        &mut store,
        &workspace_display,
        &mut stats,
        &mut progress,
    )?;

    progress(IndexStatus::indexing_phase(
        workspace_display.clone(),
        &stats,
        "finalizing",
    ));
    stats.deleted_files = store.stage_delete_missing_files(build, &seen_paths)?;
    // Rebuild the full include graph that backs reachability scoping, and
    // derive the first-layer `directly_included` flag in the same pass.
    let include_edge_started = Instant::now();
    let include_graph = build_include_edges(&store, build, &include_roots, None)?;
    let commit = store.commit_index_build(build, &include_graph)?;
    stats.semantic_generation = commit.generation;
    stats.maintenance_warning = commit.cleanup_warning;
    stats.include_edge_ms = include_edge_started.elapsed().as_millis();
    if defer_call_indexes {
        progress(IndexStatus::indexing_phase(
            workspace_display.clone(),
            &stats,
            "building call indexes",
        ));
        let secondary_index_started = Instant::now();
        store.finalize_full_build_indexes()?;
        stats.secondary_index_ms = secondary_index_started.elapsed().as_millis();
    }
    stats.symbols = store.symbol_count()?;
    let call_coverage = store.call_fact_view().coverage()?;
    stats.callable_anchors = call_coverage.callable_anchors as usize;
    stats.call_sites = call_coverage.call_sites as usize;
    if side_by_side_publication {
        progress(IndexStatus::indexing_phase(
            workspace_display.clone(),
            &stats,
            "publishing database generation",
        ));
        let publication_started = Instant::now();
        store.prepare_full_build_publication()?;
        drop(store);
        publish_default_index(&workspace, &db_path, stats.semantic_generation)?;
        stats.publication_ms = publication_started.elapsed().as_millis();
    }
    stats.elapsed_ms = started.elapsed().as_millis();
    progress(IndexStatus::ready(workspace_display, &stats));
    Ok(stats)
}

fn latest_default_generation(workspace: &Path) -> u64 {
    let Ok(directory) = crate::pathing::default_index_directory(workspace) else {
        return 0;
    };
    let Ok(entries) = std::fs::read_dir(directory) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("index-g") && name.ends_with(".sqlite")
        })
        .filter_map(|entry| {
            IndexStore::open_readonly(&entry.path())
                .and_then(|store| store.semantic_generation())
                .ok()
        })
        .max()
        .unwrap_or(0)
}

pub fn index_dirty_files(
    workspace: impl AsRef<Path>,
    changes: Vec<DirtyFileChange>,
    options: IndexOptions,
    mut progress: impl FnMut(IndexStatus),
) -> Result<IndexStats> {
    let started = Instant::now();
    let workspace = canonical_workspace(workspace)?;
    let workspace_display = workspace.display().to_string();
    let db_path = match options.db_path {
        Some(path) => path,
        None => default_index_path(&workspace)?,
    };
    let mut stats = IndexStats {
        total_files: changes.len(),
        ..Default::default()
    };

    progress(IndexStatus::indexing_phase(
        workspace_display.clone(),
        &stats,
        "updating",
    ));

    let (config, config_issue) = WorkspaceConfig::load(&workspace);
    if let Some(issue) = &config_issue {
        progress(IndexStatus::indexing_with_message(
            workspace_display.clone(),
            &stats,
            issue.message.clone(),
        ));
    }

    let mut include_entries = config.include_paths.clone();
    include_entries.extend(options.include_paths.iter().cloned());
    let (include_roots, include_issues) = resolve_include_roots(&include_entries);
    for issue in include_issues {
        progress(IndexStatus::indexing_with_message(
            workspace_display.clone(),
            &stats,
            issue.message,
        ));
    }

    let mut store = IndexStore::open(&db_path, &workspace)?;
    let build = store.begin_index_build(false)?;
    let check_started = Instant::now();
    let mut upserts = Vec::new();
    let mut upsert_rels: Vec<String> = Vec::new();
    let mut deletes = Vec::new();
    let mut changed_rels: Vec<String> = Vec::new();
    let mut seen = HashSet::new();

    for change in changes {
        let absolute_path = if change.kind == DirtyFileKind::Upsert {
            change
                .absolute_path
                .canonicalize()
                .unwrap_or_else(|_| change.absolute_path.clone())
        } else {
            canonicalize_existing_prefix(&change.absolute_path)
        };
        let Ok(rel_slash) = relative_slash_path(&workspace, &absolute_path) else {
            continue;
        };
        if !seen.insert(rel_slash.clone()) {
            continue;
        }

        match change.kind {
            DirtyFileKind::Delete => {
                changed_rels.push(rel_slash.clone());
                deletes.push(rel_slash);
            }
            DirtyFileKind::Upsert => {
                if !absolute_path.is_file() || !config.is_in_scope(&rel_slash) {
                    changed_rels.push(rel_slash.clone());
                    deletes.push(rel_slash);
                    continue;
                }
                changed_rels.push(rel_slash.clone());
                upsert_rels.push(rel_slash.clone());
                upserts.push(candidate_for_path(&absolute_path, rel_slash)?);
            }
        }
    }

    stats.check_ms = check_started.elapsed().as_millis();

    for rel in deletes {
        let write_started = Instant::now();
        stats.deleted_files += store.stage_delete_file(build, &rel)?;
        stats.processed_files += 1;
        stats.write_ms = stats
            .write_ms
            .saturating_add(write_started.elapsed().as_millis());
    }

    parse_and_write_changed(
        upserts,
        parse_thread_count(options.parse_threads),
        build,
        &mut store,
        &workspace_display,
        &mut stats,
        &mut progress,
    )?;

    // Rebuild include edges for changed source files and for any existing source
    // whose raw include target could resolve to a newly added/deleted path.
    // Deletes cascade existing edges, but source files that used to include the
    // deleted header still need their unresolved/ambiguous count recomputed.
    // The same pass re-derives the first-layer `directly_included` flag from
    // `external_exact` edges — no separate loose-match matcher runs.
    let include_edge_started = Instant::now();
    let roots_slash: Vec<String> = include_roots
        .iter()
        .map(|root| normalize_abs_path(root))
        .collect();
    let affected_rels =
        sql_affected_include_edge_sources(&store, &roots_slash, &upsert_rels, &changed_rels)?;
    stats.include_edge_sources_rebuilt = affected_rels.clone();
    let include_graph = build_include_edges(&store, build, &include_roots, Some(&affected_rels))?;
    let commit = store.commit_index_build(build, &include_graph)?;
    stats.semantic_generation = commit.generation;
    stats.maintenance_warning = commit.cleanup_warning;
    stats.include_edge_ms = include_edge_started.elapsed().as_millis();
    stats.symbols = store.symbol_count()?;
    let call_coverage = store.call_fact_view().coverage()?;
    stats.callable_anchors = call_coverage.callable_anchors as usize;
    stats.call_sites = call_coverage.call_sites as usize;
    stats.elapsed_ms = started.elapsed().as_millis();
    progress(IndexStatus::ready(workspace_display, &stats));
    Ok(stats)
}

#[cfg(test)]
mod tests;
