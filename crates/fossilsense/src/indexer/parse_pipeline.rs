use std::env;
use std::fs;
use std::time::Instant;

use anyhow::{Context, Result};
use rayon::prelude::*;
use rayon::ThreadPoolBuilder;

use super::candidates::FileCandidate;
use super::ProgressLimiter;
use crate::parser::{parse_thread_local_with_facts, FileSemanticIndex, ParseFacts};
use crate::progress::{IndexStats, IndexStatus};
use crate::store::{FileIndexPayload, FileIndexUpdate, FileSource, IndexStore};

const DEFAULT_MAX_PARSE_THREADS: usize = 8;
const PARSER_THREAD_STACK_SIZE: usize = 8 * 1024 * 1024;
const WRITE_BATCH_SIZE: usize = 512;

#[derive(Debug)]
struct ParsedFile {
    fingerprint: crate::store::FileFingerprint,
    source: FileSource,
    result: Result<FileSemanticIndex, String>,
}

pub(super) fn parse_and_write_changed(
    changed: Vec<FileCandidate>,
    parse_threads: usize,
    replace_all_files: bool,
    store: &mut IndexStore,
    workspace_display: &str,
    stats: &mut IndexStats,
    progress: &mut impl FnMut(IndexStatus),
) -> Result<()> {
    if changed.is_empty() {
        return Ok(());
    }

    progress(IndexStatus::indexing_phase(
        workspace_display.to_string(),
        stats,
        "parsing",
    ));

    let parse_started = Instant::now();
    let pool = ThreadPoolBuilder::new()
        .num_threads(parse_threads)
        .stack_size(PARSER_THREAD_STACK_SIZE)
        .thread_name(|idx| format!("fossilsense-parser-{idx}"))
        .build()
        .context("failed to create parser thread pool")?;
    let parsed_files: Vec<ParsedFile> =
        pool.install(|| changed.into_par_iter().map(parse_candidate).collect());
    stats.parse_ms = parse_started.elapsed().as_millis();

    progress(IndexStatus::indexing_phase(
        workspace_display.to_string(),
        stats,
        "indexing",
    ));
    let full_rebuild_load = replace_all_files && stats.skipped_files == 0;
    if full_rebuild_load {
        store.begin_full_rebuild_load()?;
    }

    let mut index_progress = ProgressLimiter::new();
    let write_result = (|| -> Result<()> {
        for chunk in parsed_files.chunks(WRITE_BATCH_SIZE) {
            let mut updates = Vec::with_capacity(chunk.len());
            let mut chunk_symbols = 0usize;
            for parsed in chunk {
                match &parsed.result {
                    Ok(index) => {
                        chunk_symbols += index.persistent_facts().symbols.len();
                        updates.push(FileIndexUpdate {
                            fingerprint: &parsed.fingerprint,
                            source: parsed.source,
                            payload: FileIndexPayload::Ok(index),
                        });
                    }
                    Err(error) => {
                        updates.push(FileIndexUpdate {
                            fingerprint: &parsed.fingerprint,
                            source: parsed.source,
                            payload: FileIndexPayload::Error(error.as_str()),
                        });
                    }
                }
            }

            let write_started = Instant::now();
            if full_rebuild_load {
                store.apply_fresh_file_updates(&updates)?;
            } else {
                store.apply_file_updates(&updates)?;
            }
            stats.write_ms = stats
                .write_ms
                .saturating_add(write_started.elapsed().as_millis());
            stats.symbols += chunk_symbols;
            stats.indexed_files += chunk.len();
            stats.processed_files += chunk.len();
            index_progress.maybe_emit(progress, workspace_display, stats, "indexing");
        }
        Ok(())
    })();
    if full_rebuild_load {
        store.finish_full_rebuild_load()?;
    }
    write_result?;
    index_progress.emit_if_changed(progress, workspace_display, stats, "indexing");
    Ok(())
}

pub(super) fn parse_thread_count(override_threads: Option<usize>) -> usize {
    let requested = override_threads
        .or_else(|| {
            env::var("FOSSILSENSE_PARSE_THREADS")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
        })
        .unwrap_or(DEFAULT_MAX_PARSE_THREADS);
    let available = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1);
    requested.max(1).min(available)
}

fn parse_candidate(candidate: FileCandidate) -> ParsedFile {
    let mut fingerprint = candidate.fingerprint;
    let result = match fs::read(&candidate.absolute_path) {
        Ok(bytes) => {
            if candidate.source == FileSource::Workspace {
                fingerprint.hash = blake3::hash(&bytes).to_hex().to_string();
            }
            let source = String::from_utf8_lossy(&bytes);
            // `parse_thread_local_with_facts` uses the INDEX mask, skipping
            // request-time occurrence and local-declaration collection (those
            // vectors would be cleared before writing anyway).
            // It is infallible for ordinary parse problems (degrades to the
            // lexical-fallback product), so the only error here is the file read.
            let index =
                parse_thread_local_with_facts(&candidate.absolute_path, &source, ParseFacts::INDEX);
            Ok(index)
        }
        Err(error) => Err(format!(
            "failed to read {}: {error:#}",
            candidate.absolute_path.display()
        )),
    };

    ParsedFile {
        fingerprint,
        source: candidate.source,
        result,
    }
}
