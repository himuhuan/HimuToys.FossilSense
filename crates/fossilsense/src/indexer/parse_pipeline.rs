use std::env;
use std::fs;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use anyhow::{Context, Result};
use rayon::prelude::*;
use rayon::ThreadPoolBuilder;

use super::candidates::FileCandidate;
use super::ProgressLimiter;
use crate::config::LanguageResolver;
use crate::parser::{parse_thread_local_with_selection_budget, FileSemanticIndex, ParseFacts};
use crate::progress::{IndexStats, IndexStatus};
use crate::store::{FileIndexPayload, FileIndexUpdate, FileSource, IndexBuild, IndexStore};

const DEFAULT_MAX_PARSE_THREADS: usize = 8;
const PARSER_THREAD_STACK_SIZE: usize = 8 * 1024 * 1024;
const WRITE_BATCH_SIZE: usize = 128;
const WRITE_BATCH_BYTES: usize = 16 * 1024 * 1024;

fn batch_would_overflow(files: usize, bytes: usize, next: usize) -> bool {
    files > 0 && (files >= WRITE_BATCH_SIZE || bytes.saturating_add(next) > WRITE_BATCH_BYTES)
}

struct ParserPool {
    pool: Option<rayon::ThreadPool>,
    workers: std::sync::Arc<std::sync::Mutex<Vec<std::thread::JoinHandle<()>>>>,
}

impl ParserPool {
    fn new(threads: usize) -> Result<Self> {
        Self::build(threads, None)
    }

    fn build(
        threads: usize,
        exit_hook: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
    ) -> Result<Self> {
        let workers = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let handles = workers.clone();
        let pool = ThreadPoolBuilder::new()
            .num_threads(threads)
            .stack_size(PARSER_THREAD_STACK_SIZE)
            .thread_name(|idx| format!("fossilsense-parser-{idx}"))
            .spawn_handler(move |thread| {
                let mut builder = std::thread::Builder::new();
                if let Some(name) = thread.name() {
                    builder = builder.name(name.to_owned());
                }
                if let Some(stack) = thread.stack_size() {
                    builder = builder.stack_size(stack);
                }
                let hook = exit_hook.clone();
                let handle = builder.spawn(move || {
                    thread.run();
                    if let Some(hook) = hook {
                        hook();
                    }
                })?;
                handles.lock().unwrap().push(handle);
                Ok(())
            })
            .build();
        let mut owned = Self {
            pool: None,
            workers,
        };
        owned.pool = Some(pool.context("failed to create parser thread pool")?);
        Ok(owned)
    }

    fn install<T: Send>(&self, operation: impl FnOnce() -> T + Send) -> T {
        self.pool
            .as_ref()
            .expect("live parser pool")
            .install(operation)
    }
}

impl Drop for ParserPool {
    fn drop(&mut self) {
        drop(self.pool.take());
        // Rayon only signals termination in ThreadPool::drop. Join our actual
        // OS threads before releasing their stack estimate or starting another
        // pool, including their thread-local parser destructors.
        let workers = std::mem::take(&mut *self.workers.lock().unwrap());
        for worker in workers {
            let _ = worker.join();
        }
    }
}

fn reserve_wave(
    permit: &crate::build_coordinator::BuildPermit,
    bytes: usize,
) -> Result<crate::build_coordinator::BuildReservationLease> {
    Ok(permit.reserve_scoped(bytes)?)
}

fn check_fact_capacity(retained: usize, _estimated: usize) -> Result<()> {
    anyhow::ensure!(
        retained <= MAX_PARSE_BYTES,
        "ResourceBudgetExceeded: facts exceed exclusive capacity"
    );
    Ok(())
}

#[derive(Debug)]
struct ParsedFile {
    fingerprint: crate::store::FileFingerprint,
    source: FileSource,
    result: Result<FileSemanticIndex, String>,
}

pub(super) struct ParsePipelineConfig {
    pub parse_threads: usize,
    pub language_resolver: LanguageResolver,
    pub cancellation: Option<crate::build_coordinator::BuildCancellation>,
    pub permit: Option<crate::build_coordinator::BuildPermit>,
}

pub(super) fn parse_and_write_changed(
    changed: Vec<FileCandidate>,
    config: ParsePipelineConfig,
    build: IndexBuild,
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

    let mut parse_threads = config
        .parse_threads
        .clamp(1, DEFAULT_MAX_PARSE_THREADS)
        .min(changed.len());
    if let Some(permit) = &config.permit {
        let smallest = changed
            .iter()
            .map(|candidate| candidate.fingerprint.size)
            .min()
            .unwrap_or(0);
        let minimum_work = parse_reservation(smallest)?;
        loop {
            match reserve_wave(
                permit,
                parse_threads
                    .saturating_mul(PARSER_THREAD_STACK_SIZE)
                    .saturating_add(minimum_work),
            ) {
                Ok(probe) => {
                    drop(probe);
                    break;
                }
                Err(_) if parse_threads > 1 && !permit.is_cancelled() => parse_threads -= 1,
                Err(error) => return Err(error),
            }
        }
    }
    let parse_started = Instant::now();
    let make_pool = ParserPool::new;
    let mut pool = Some(make_pool(parse_threads)?);
    stats.parse_stack_bytes_reserved = stats
        .parse_stack_bytes_reserved
        .max(parse_threads.saturating_mul(PARSER_THREAD_STACK_SIZE));
    // Bounded waves have no producer waiting on a consumer that is itself
    // waiting to fill a batch. Only file descriptors are retained between waves.
    let mut pending: std::collections::VecDeque<_> = changed.into();
    let mut index_progress = ProgressLimiter::new();
    let stopped = AtomicBool::new(false);
    let active = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);
    let attempts = AtomicUsize::new(0);
    let cancelled = || {
        config
            .cancellation
            .as_ref()
            .is_some_and(|c| c.is_cancelled())
    };
    let mut exclusive_retries = 0usize;
    while pending.front().is_some() {
        anyhow::ensure!(!cancelled(), "index build cancelled");
        let mut wave = Vec::new();
        let mut reserved = parse_threads.saturating_mul(PARSER_THREAD_STACK_SIZE);
        while let Some(candidate) = pending.front() {
            let next = parse_reservation(candidate.fingerprint.size)?;
            let limit = if wave.is_empty() {
                MAX_PARSE_BYTES
            } else {
                NORMAL_PARSE_BYTES
            };
            if !wave.is_empty()
                && (wave.len()
                    == (if exclusive_retries > 0 {
                        1
                    } else {
                        WRITE_BATCH_SIZE
                    })
                    || next > WRITE_BATCH_BYTES
                    || reserved.saturating_add(next) > limit)
            {
                break;
            }
            reserved = reserved.saturating_add(next);
            wave.push(pending.pop_front().expect("peeked candidate"));
            if next > WRITE_BATCH_BYTES {
                break;
            }
        }
        anyhow::ensure!(
            reserved <= MAX_PARSE_BYTES,
            "ResourceBudgetExceeded: wave requires {reserved} bytes"
        );
        if wave.len() == 1 && config.permit.is_none() {
            reserved = MAX_PARSE_BYTES;
        }
        let lease = loop {
            let admission = config
                .permit
                .as_ref()
                .map(|permit| {
                    if wave.len() == 1 {
                        permit
                            .reserve_scoped_up_to(MAX_PARSE_BYTES, reserved)
                            .map(|(lease, admitted)| {
                                reserved = admitted;
                                lease
                            })
                            .map_err(anyhow::Error::from)
                    } else {
                        reserve_wave(permit, reserved)
                    }
                })
                .transpose();
            match admission {
                Ok(lease) => break lease,
                Err(error) if wave.len() > 1 => {
                    if config
                        .cancellation
                        .as_ref()
                        .is_some_and(|c| c.is_cancelled())
                    {
                        return Err(error);
                    }
                    let removed = wave.pop().expect("multiple candidates");
                    reserved =
                        reserved.saturating_sub(parse_reservation(removed.fingerprint.size)?);
                    pending.push_front(removed);
                }
                Err(error)
                    if parse_threads > 1
                        && !cancelled()
                        && matches!(
                            error.downcast_ref::<crate::build_coordinator::ReservationError>(),
                            Some(crate::build_coordinator::ReservationError::ProcessPressure)
                        ) =>
                {
                    // Pressure can rise after startup. Every preceding wave
                    // has joined and released its facts; retire the idle pool
                    // before admitting fewer worker stacks for this same file.
                    drop(pool.take());
                    parse_threads -= 1;
                    reserved = reserved.saturating_sub(PARSER_THREAD_STACK_SIZE);
                }
                Err(error) => {
                    return Err(error)
                        .context("admit parse wave after reducing files and worker stacks")
                }
            }
        };
        if pool.is_none() {
            pool = Some(make_pool(parse_threads)?);
        }
        stats.parse_reserved_bytes_peak = stats.parse_reserved_bytes_peak.max(reserved);
        let input_bytes = wave.iter().fold(0usize, |bytes, candidate| {
            bytes.saturating_add(candidate.fingerprint.size as usize)
        });
        let stack_bytes = parse_threads.saturating_mul(PARSER_THREAD_STACK_SIZE);
        stats.parse_input_bytes_reserved_peak =
            stats.parse_input_bytes_reserved_peak.max(input_bytes);
        stats.parse_stack_bytes_reserved = stats.parse_stack_bytes_reserved.max(stack_bytes);
        stats.parse_workspace_bytes_reserved_peak = stats.parse_workspace_bytes_reserved_peak.max(
            reserved
                .saturating_sub(stack_bytes)
                .saturating_sub(input_bytes),
        );
        stats.parse_exclusive_files += usize::from(wave.len() == 1);

        let retry_candidates = wave.clone();
        let exclusive = wave.len() == 1;
        let outcomes = pool.as_ref().expect("admitted parser pool").install(|| {
            wave.into_par_iter()
                .map(|candidate| {
                    anyhow::ensure!(!cancelled(), "index build cancelled");
                    let _activity = ParserActivity::enter(&active, &peak);
                    attempts.fetch_add(1, Ordering::Relaxed);
                    let cancel = config.cancellation.as_ref().map_or(&stopped, |c| c.flag());
                    let fact_limit = if exclusive {
                        reserved
                            .saturating_sub(stack_bytes)
                            .saturating_sub((candidate.fingerprint.size as usize).saturating_mul(4))
                    } else {
                        parse_reservation(candidate.fingerprint.size)?
                    };
                    parse_candidate_control(
                        candidate,
                        &config.language_resolver,
                        cancel,
                        fact_limit,
                    )
                })
                .collect::<Vec<_>>()
        });
        stats.parse_attempts = attempts.load(Ordering::Relaxed);
        stats.active_parsers_peak = stats.active_parsers_peak.max(peak.load(Ordering::Relaxed));
        let mut parsed = Vec::with_capacity(outcomes.len());
        let mut retry = Vec::new();
        for (candidate, outcome) in retry_candidates.into_iter().zip(outcomes) {
            match outcome {
                Ok(file) => parsed.push(file),
                Err(error) if !exclusive && error.is::<ParseFactBudgetExceeded>() => {
                    retry.push(candidate);
                }
                Err(error) => return Err(error),
            }
        }
        // Keep successful products inside this wave's admitted capacity. Only
        // files that exceeded their own estimate need a larger exclusive retry.
        // Staged successes remain invisible if any later file ultimately fails.
        stats.parse_fact_budget_retries += retry.len();
        exclusive_retries = exclusive_retries.saturating_sub(1);
        let result = (|| {
            anyhow::ensure!(!cancelled(), "index build cancelled");
            let retained = parsed
                .iter()
                .fold(0usize, |n, p| n.saturating_add(p.retained_bytes()));
            check_fact_capacity(retained, reserved)?;
            if let Some(lease) = &lease {
                lease.retain(retained)?;
            }
            stats.parse_fact_bytes_peak = stats.parse_fact_bytes_peak.max(retained);
            let mut batch = Vec::new();
            let mut batch_bytes = 0usize;
            for item in parsed {
                let bytes = item.retained_bytes();
                if batch_would_overflow(batch.len(), batch_bytes, bytes) {
                    stats.parse_batch_bytes_peak = stats.parse_batch_bytes_peak.max(batch_bytes);
                    write_parsed_batch(
                        &batch,
                        build,
                        store,
                        workspace_display,
                        stats,
                        progress,
                        &mut index_progress,
                    )?;
                    batch.clear();
                    batch_bytes = 0;
                }
                batch_bytes = batch_bytes.saturating_add(bytes);
                batch.push(item);
            }
            if !batch.is_empty() {
                stats.parse_batch_bytes_peak = stats.parse_batch_bytes_peak.max(batch_bytes);
                write_parsed_batch(
                    &batch,
                    build,
                    store,
                    workspace_display,
                    stats,
                    progress,
                    &mut index_progress,
                )?;
            }
            Ok(())
        })();
        // All products are dropped before returning the aggregate wave lease.
        drop(lease);
        result?;
        exclusive_retries += retry.len();
        for candidate in retry.into_iter().rev() {
            pending.push_front(candidate);
        }
    }
    stats.parse_ms = parse_started
        .elapsed()
        .as_millis()
        .saturating_sub(stats.write_ms);
    index_progress.emit_if_changed(progress, workspace_display, stats, "indexing");
    Ok(())
}

fn write_parsed_batch(
    batch: &[ParsedFile],
    build: IndexBuild,
    store: &mut IndexStore,
    workspace_display: &str,
    stats: &mut IndexStats,
    progress: &mut impl FnMut(IndexStatus),
    index_progress: &mut ProgressLimiter,
) -> Result<()> {
    let mut updates = Vec::with_capacity(batch.len());
    let mut chunk_declarations = 0usize;
    for parsed in batch {
        match &parsed.result {
            Ok(index) => {
                chunk_declarations += index.persistent_facts().declarations.len();
                updates.push(FileIndexUpdate {
                    fingerprint: &parsed.fingerprint,
                    source: parsed.source,
                    payload: FileIndexPayload::Ok(index),
                });
            }
            Err(error) => updates.push(FileIndexUpdate {
                fingerprint: &parsed.fingerprint,
                source: parsed.source,
                payload: FileIndexPayload::Error(error.as_str()),
            }),
        }
    }

    stats.parse_writer_updates_bytes_peak = stats.parse_writer_updates_bytes_peak.max(
        updates
            .capacity()
            .saturating_mul(std::mem::size_of::<FileIndexUpdate>()),
    );
    stats.parse_write_batches += 1;
    let write_started = Instant::now();
    store.stage_file_updates(build, &updates)?;
    stats.write_ms = stats
        .write_ms
        .saturating_add(write_started.elapsed().as_millis());
    stats.declarations += chunk_declarations;
    stats.indexed_files += batch.len();
    stats.processed_files += batch.len();
    index_progress.maybe_emit(progress, workspace_display, stats, "indexing");
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

struct ParserActivity<'a>(&'a AtomicUsize);
impl<'a> ParserActivity<'a> {
    fn enter(active: &'a AtomicUsize, peak: &AtomicUsize) -> Self {
        let count = active.fetch_add(1, Ordering::Relaxed) + 1;
        peak.fetch_max(count, Ordering::Relaxed);
        Self(active)
    }
}
impl Drop for ParserActivity<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

#[derive(Debug)]
struct ParseFactBudgetExceeded {
    limit: usize,
    observed: usize,
}
impl std::fmt::Display for ParseFactBudgetExceeded {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "ResourceBudgetExceeded: parser fact capacity {} exceeds admitted {} bytes",
            self.observed, self.limit
        )
    }
}
impl std::error::Error for ParseFactBudgetExceeded {}

const NORMAL_PARSE_BYTES: usize = 128 * 1024 * 1024;
const MAX_PARSE_BYTES: usize = 256 * 1024 * 1024;

fn parse_reservation(input_bytes: u64) -> Result<usize> {
    // Includes lossy UTF-8 copies, AST/recovery workspace, fact growth and stack.
    // This is conservative admission, not an allocator-level AST memory cap.
    let bytes = usize::try_from(input_bytes)
        .unwrap_or(usize::MAX)
        .saturating_mul(16)
        .saturating_add(64 * 1024);
    anyhow::ensure!(
        bytes <= MAX_PARSE_BYTES,
        "ResourceBudgetExceeded: input requires {bytes} reserved bytes (limit {MAX_PARSE_BYTES})"
    );
    Ok(bytes)
}

impl ParsedFile {
    fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            .saturating_add(self.fingerprint.path.capacity())
            .saturating_add(self.fingerprint.extension.capacity())
            .saturating_add(self.fingerprint.hash.capacity())
            .saturating_add(match &self.result {
                Ok(index) => index
                    .retained_bytes()
                    .saturating_sub(std::mem::size_of::<FileSemanticIndex>()),
                Err(e) => e.capacity(),
            })
    }
}

#[cfg(test)]
fn parse_candidate(candidate: FileCandidate, language_resolver: &LanguageResolver) -> ParsedFile {
    parse_candidate_control(
        candidate,
        language_resolver,
        &AtomicBool::new(false),
        MAX_PARSE_BYTES,
    )
    .expect("resource budget")
}

fn parse_candidate_control(
    candidate: FileCandidate,
    language_resolver: &LanguageResolver,
    cancel: &AtomicBool,
    fact_limit: usize,
) -> Result<ParsedFile> {
    let mut fingerprint = candidate.fingerprint;
    parse_reservation(fingerprint.size)?;
    let result = match fs::File::open(&candidate.absolute_path) {
        Ok(mut file) => {
            let before = file.metadata()?;
            anyhow::ensure!(
                before.len() == fingerprint.size
                    && (fingerprint.mtime_ns == 0
                        || super::candidates::metadata_mtime_ns(&before) == fingerprint.mtime_ns),
                "InputChanged: file revision changed before parsing {}",
                candidate.absolute_path.display()
            );
            let mut bytes = Vec::with_capacity(usize::try_from(fingerprint.size).unwrap_or(0));
            (&mut file)
                .take(fingerprint.size.saturating_add(1))
                .read_to_end(&mut bytes)?;
            anyhow::ensure!(
                bytes.len() as u64 <= fingerprint.size,
                "ResourceBudgetExceeded: input grew while reading {}",
                candidate.absolute_path.display()
            );
            let after = file.metadata()?;
            anyhow::ensure!(
                bytes.len() as u64 == fingerprint.size
                    && after.len() == before.len()
                    && super::candidates::metadata_mtime_ns(&before)
                        == super::candidates::metadata_mtime_ns(&after),
                "InputChanged: file revision changed while reading {}",
                candidate.absolute_path.display()
            );
            if candidate.source == FileSource::Workspace {
                fingerprint.hash = blake3::hash(&bytes).to_hex().to_string();
            }
            let source = String::from_utf8_lossy(&bytes);
            let selection =
                language_resolver.selection_for_source(&candidate.absolute_path, &source);
            let identity_path = if selection.language == crate::config::SourceLanguage::Go {
                std::path::Path::new(&fingerprint.path)
            } else {
                candidate.absolute_path.as_path()
            };
            let mut index = parse_thread_local_with_selection_budget(
                identity_path,
                &source,
                selection,
                ParseFacts::INDEX,
                cancel,
                fact_limit,
            )
            .map_err(|observed| ParseFactBudgetExceeded {
                limit: fact_limit,
                observed,
            })?
            .ok_or_else(|| anyhow::anyhow!("index build cancelled"))?;
            if candidate.source == FileSource::External {
                index.retain_external_call_declarations();
            }
            Ok(index)
        }
        Err(error) => Err(format!(
            "failed to read {}: {error:#}",
            candidate.absolute_path.display()
        )),
    };
    Ok(ParsedFile {
        fingerprint,
        source: candidate.source,
        result,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::call_model::LinkageDomain;
    use crate::config::WorkspaceConfig;
    use crate::store::FileFingerprint;

    #[test]
    fn parser_pool_joins_exiting_workers_before_replacement() {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = std::sync::Mutex::new(release_rx);
        let (replaced_tx, replaced_rx) = std::sync::mpsc::channel();
        let replacement = std::thread::spawn(move || {
            let hook = std::sync::Arc::new(move || {
                entered_tx.send(()).unwrap();
                release_rx.lock().unwrap().recv().unwrap();
            });
            let pool = ParserPool::build(1, Some(hook)).unwrap();
            pool.install(|| {});
            drop(pool);
            replaced_tx.send(()).unwrap();
        });
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let prematurely_replaced = replaced_rx
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_ok();
        release_tx.send(()).unwrap();
        replacement.join().unwrap();
        assert!(
            !prematurely_replaced,
            "old worker stack was counted as released before its thread exited"
        );
    }

    #[test]
    fn parse_pipeline_budget_preserves_parent_memory_when_sampling_unavailable() {
        use crate::build_coordinator::{BuildCoordinator, BuildKind, BuildPolicy};
        let coordinator = BuildCoordinator::with_policy_and_sampler(BuildPolicy::default(), || 0);
        let permit = coordinator
            .try_acquire("root".into(), BuildKind::FullIndex)
            .unwrap();
        permit.reserve(0, 384 * 1024 * 1024).unwrap();
        assert!(reserve_wave(&permit, 128 * 1024 * 1024).is_err());
        assert_eq!(
            coordinator.snapshot().active_retained_bytes,
            384 * 1024 * 1024
        );
    }

    #[test]
    fn parse_pipeline_budget_allows_dense_facts_within_exclusive_capacity() {
        assert!(check_fact_capacity(128 * 1024 * 1024, 32 * 1024 * 1024).is_ok());
        assert!(check_fact_capacity(MAX_PARSE_BYTES + 1, 32 * 1024 * 1024).is_err());
    }

    #[test]
    fn parse_pipeline_budget_lease_restores_parent_even_after_cancellation() {
        use crate::build_coordinator::{BuildCoordinator, BuildKind, BuildPolicy};
        let coordinator = BuildCoordinator::with_policy_and_sampler(BuildPolicy::default(), || 0);
        let permit = coordinator
            .try_acquire("root".into(), BuildKind::FullIndex)
            .unwrap();
        permit.reserve(8 * 1024 * 1024, 100 * 1024 * 1024).unwrap();
        let lease = reserve_wave(&permit, 32 * 1024 * 1024).unwrap();
        assert_eq!(
            coordinator.snapshot().active_reserved_bytes,
            40 * 1024 * 1024
        );
        lease.retain(64 * 1024 * 1024).unwrap();
        assert_eq!(
            coordinator.snapshot().active_retained_bytes,
            164 * 1024 * 1024
        );
        permit.cancellation().cancel();
        drop(lease);
        assert_eq!(
            coordinator.snapshot().active_reserved_bytes,
            8 * 1024 * 1024
        );
        assert_eq!(
            coordinator.snapshot().active_retained_bytes,
            100 * 1024 * 1024
        );
    }

    #[test]
    fn writer_failure_releases_wave_and_does_not_parse_remaining_workspace() {
        use crate::build_coordinator::{BuildCoordinator, BuildKind, BuildPolicy};
        let ws = tempdir().unwrap();
        let path = ws.path().join("main.c");
        let source = "int stable;\n";
        fs::write(&path, source).unwrap();
        let db = ws.path().join("index.sqlite");
        let mut store = IndexStore::open(&db, ws.path()).unwrap();
        let build = store.begin_index_build(false).unwrap();
        crate::store::test_support::install_file_revision_write_failure(&db).unwrap();
        let coordinator = BuildCoordinator::with_policy_and_sampler(BuildPolicy::default(), || 0);
        let permit = coordinator
            .try_acquire(ws.path().into(), BuildKind::FullIndex)
            .unwrap();
        permit.reserve(1024, 2048).unwrap();
        let candidate = FileCandidate {
            absolute_path: path,
            fingerprint: FileFingerprint {
                path: "main.c".into(),
                extension: "c".into(),
                size: source.len() as u64,
                mtime_ns: 0,
                hash: String::new(),
            },
            source: FileSource::Workspace,
        };
        let mut stats = IndexStats::default();
        let error = parse_and_write_changed(
            vec![candidate; 1000],
            ParsePipelineConfig {
                parse_threads: 2,
                language_resolver: LanguageResolver::from_workspace_config(
                    ws.path(),
                    &WorkspaceConfig::default(),
                ),
                cancellation: Some(permit.cancellation()),
                permit: Some(permit),
            },
            build,
            &mut store,
            "test",
            &mut stats,
            &mut |_| {},
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("injected writer failure"));
        assert!(stats.parse_attempts > 0 && stats.parse_attempts <= 128);
        assert_eq!(stats.indexed_files, 0);
        assert_eq!(coordinator.snapshot().active_builds, 0);
    }

    #[test]
    fn exclusive_dense_parse_obeys_parent_admission_before_full_fact_growth() {
        use crate::build_coordinator::{BuildCoordinator, BuildKind, BuildPolicy};
        let ws = tempdir().unwrap();
        let path = ws.path().join("dense.c");
        let source: String = (0..50_000).map(|i| format!("int v{i};\n")).collect();
        fs::write(&path, &source).unwrap();
        let mut store = IndexStore::open(&ws.path().join("index.sqlite"), ws.path()).unwrap();
        let build = store.begin_index_build(false).unwrap();
        let coordinator = BuildCoordinator::with_policy_and_sampler(BuildPolicy::default(), || 0);
        let permit = coordinator
            .try_acquire(ws.path().into(), BuildKind::FullIndex)
            .unwrap();
        permit.reserve(0, 455 * 1024 * 1024).unwrap();
        let mut stats = IndexStats::default();
        let error = parse_and_write_changed(
            vec![FileCandidate {
                absolute_path: path,
                fingerprint: FileFingerprint {
                    path: "dense.c".into(),
                    extension: "c".into(),
                    size: source.len() as u64,
                    mtime_ns: 0,
                    hash: String::new(),
                },
                source: FileSource::Workspace,
            }],
            ParsePipelineConfig {
                parse_threads: 32,
                language_resolver: LanguageResolver::from_workspace_config(
                    ws.path(),
                    &WorkspaceConfig::default(),
                ),
                cancellation: Some(permit.cancellation()),
                permit: Some(permit.clone()),
            },
            build,
            &mut store,
            "test",
            &mut stats,
            &mut |_| {},
        )
        .unwrap_err();
        let exceeded = error
            .downcast_ref::<ParseFactBudgetExceeded>()
            .expect("growth stops at the admitted limit");
        assert!(exceeded.limit < 25 * 1024 * 1024);
        assert!(
            exceeded.observed
                < 50_000 * std::mem::size_of::<crate::semantic_model::DeclarationFact>()
        );
        assert_eq!(stats.active_parsers_peak, 1);
        assert_eq!(stats.indexed_files, 0);
        assert_eq!(
            coordinator.snapshot().active_retained_bytes,
            455 * 1024 * 1024
        );
        assert_eq!(coordinator.snapshot().active_reserved_bytes, 0);
    }

    #[test]
    fn parent_pressure_reduces_pool_for_multiple_small_files() {
        use crate::build_coordinator::{BuildCoordinator, BuildKind, BuildPolicy};
        let ws = tempdir().unwrap();
        let mut candidates = Vec::new();
        for i in 0..8 {
            let path = ws.path().join(format!("small_{i}.c"));
            let source = format!("int small_{i};\n");
            fs::write(&path, &source).unwrap();
            candidates.push(FileCandidate {
                absolute_path: path,
                fingerprint: FileFingerprint {
                    path: format!("small_{i}.c"),
                    extension: "c".into(),
                    size: source.len() as u64,
                    mtime_ns: 0,
                    hash: String::new(),
                },
                source: FileSource::Workspace,
            });
        }
        let mut store = IndexStore::open(&ws.path().join("index.sqlite"), ws.path()).unwrap();
        let build = store.begin_index_build(false).unwrap();
        let coordinator = BuildCoordinator::with_policy_and_sampler(BuildPolicy::default(), || 0);
        let permit = coordinator
            .try_acquire(ws.path().into(), BuildKind::FullIndex)
            .unwrap();
        permit.reserve(0, 455 * 1024 * 1024).unwrap();
        let mut stats = IndexStats::default();
        parse_and_write_changed(
            candidates,
            ParsePipelineConfig {
                parse_threads: 32,
                language_resolver: LanguageResolver::from_workspace_config(
                    ws.path(),
                    &WorkspaceConfig::default(),
                ),
                cancellation: Some(permit.cancellation()),
                permit: Some(permit.clone()),
            },
            build,
            &mut store,
            "test",
            &mut stats,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(stats.indexed_files, 8);
        assert!((1..=3).contains(&stats.active_parsers_peak));
        assert!(stats.parse_stack_bytes_reserved <= 24 * 1024 * 1024);
        assert_eq!(
            coordinator.snapshot().active_retained_bytes,
            455 * 1024 * 1024
        );
    }

    #[test]
    fn rising_process_pressure_reduces_an_already_created_parser_pool() {
        use crate::build_coordinator::{BuildCoordinator, BuildKind, BuildPolicy};
        let ws = tempdir().unwrap();
        let mut candidates = Vec::new();
        for i in 0..8 {
            let path = ws.path().join(format!("small_{i}.c"));
            let source = format!("int small_{i};\n");
            fs::write(&path, &source).unwrap();
            candidates.push(FileCandidate {
                absolute_path: path,
                fingerprint: FileFingerprint {
                    path: format!("small_{i}.c"),
                    extension: "c".into(),
                    size: source.len() as u64,
                    mtime_ns: 0,
                    hash: String::new(),
                },
                source: FileSource::Workspace,
            });
        }
        let mut store = IndexStore::open(&ws.path().join("index.sqlite"), ws.path()).unwrap();
        let build = store.begin_index_build(false).unwrap();
        let samples = std::sync::Arc::new(AtomicUsize::new(0));
        let coordinator =
            BuildCoordinator::with_policy_and_sampler(BuildPolicy::default(), move || {
                if samples.fetch_add(1, Ordering::SeqCst) == 0 {
                    0
                } else {
                    455 * 1024 * 1024
                }
            });
        let permit = coordinator
            .try_acquire(ws.path().into(), BuildKind::FullIndex)
            .unwrap();
        let mut stats = IndexStats::default();
        parse_and_write_changed(
            candidates,
            ParsePipelineConfig {
                parse_threads: 32,
                language_resolver: LanguageResolver::from_workspace_config(
                    ws.path(),
                    &WorkspaceConfig::default(),
                ),
                cancellation: Some(permit.cancellation()),
                permit: Some(permit.clone()),
            },
            build,
            &mut store,
            "test",
            &mut stats,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(stats.indexed_files, 8);
        assert!((1..=3).contains(&stats.active_parsers_peak));
        assert_eq!(
            stats.parse_stack_bytes_reserved,
            64 * 1024 * 1024,
            "record the initial pool peak honestly"
        );
        assert_eq!(coordinator.snapshot().active_retained_bytes, 0);
    }

    #[test]
    #[ignore = "release stress fixture: 100000 declarations"]
    fn dense_generated_file_retains_every_declaration_within_exclusive_budget() {
        let source: String = (0..100_000).map(|i| format!("int v{i};\n")).collect();
        let parsed = parse_header_source("dense.c", &source, &WorkspaceConfig::default());
        assert_eq!(parsed.declarations.len(), 100_000);
        check_fact_capacity(
            parsed.retained_bytes(),
            parse_reservation(source.len() as u64).unwrap(),
        )
        .unwrap();
        eprintln!(
            "dense_source_bytes={} dense_fact_bytes={}",
            source.len(),
            parsed.retained_bytes()
        );
    }

    #[test]
    fn byte_batch_flushes_before_128_files() {
        assert!(batch_would_overflow(2, WRITE_BATCH_BYTES - 1, 2));
        assert!(!batch_would_overflow(0, 0, WRITE_BATCH_BYTES + 1));
        assert!(!batch_would_overflow(2, 10, 10));
    }

    fn parse_header_source(
        path: &str,
        source: &str,
        config: &WorkspaceConfig,
    ) -> FileSemanticIndex {
        let workspace = tempdir().expect("workspace");
        let absolute_path = workspace.path().join(path);
        fs::write(&absolute_path, source).expect("source");
        let resolver = LanguageResolver::from_workspace_config(workspace.path(), config);
        parse_candidate(
            FileCandidate {
                absolute_path,
                fingerprint: FileFingerprint {
                    path: path.into(),
                    extension: "h".into(),
                    size: source.len() as u64,
                    mtime_ns: 0,
                    hash: String::new(),
                },
                source: FileSource::Workspace,
            },
            &resolver,
        )
        .result
        .expect("parsed")
    }

    #[test]
    fn language_evidence_pipeline_generated_header_enters_c_recovery() {
        let parsed = parse_header_source(
            "sensor.pb-c.h",
            "PROTOBUF_C__BEGIN_DECLS\ntypedef struct Sensor Sensor;\nPROTOBUF_C__END_DECLS\n",
            &WorkspaceConfig::default(),
        );
        assert_eq!(parsed.language, crate::semantic_model::SemanticLanguage::C);
        let sensor = parsed
            .declarations
            .iter()
            .find(|fact| {
                fact.name == "Sensor"
                    && fact.declaration_kind
                        == crate::semantic_model::SemanticDeclarationKind::Alias
            })
            .expect("first typedef survives default pipeline");
        assert_eq!(
            sensor.identity.language_fidelity,
            crate::semantic_model::LanguageFidelity::Inferred
        );
        assert!(!parsed.diagnostics.recovery.is_empty());
    }

    #[test]
    fn language_evidence_pipeline_defaults_are_not_explicit() {
        for (path, fidelity) in [
            ("unit.c", crate::semantic_model::LanguageFidelity::Inferred),
            (
                "shared.h",
                crate::semantic_model::LanguageFidelity::Heuristic,
            ),
            (
                "shared.inl",
                crate::semantic_model::LanguageFidelity::Heuristic,
            ),
            (
                "unit.cpp",
                crate::semantic_model::LanguageFidelity::Inferred,
            ),
        ] {
            let parsed = parse_header_source(path, "int visible;\n", &WorkspaceConfig::default());
            assert!(!parsed.declarations.is_empty());
            assert!(
                parsed
                    .declarations
                    .iter()
                    .all(|fact| fact.identity.language_fidelity == fidelity),
                "{path}"
            );
        }
    }

    #[test]
    fn go_parse_pipeline_uses_workspace_relative_identity_path() {
        let workspace = tempdir().expect("workspace");
        let absolute_path = workspace.path().join("src/sensor/read.go");
        fs::create_dir_all(absolute_path.parent().expect("parent")).expect("src");
        fs::write(&absolute_path, "package sensor\nfunc Read() {}\n").expect("source");
        let candidate = FileCandidate {
            absolute_path,
            fingerprint: FileFingerprint {
                path: "src/sensor/read.go".to_string(),
                extension: "go".to_string(),
                size: "package sensor\nfunc Read() {}\n".len() as u64,
                mtime_ns: 0,
                hash: "hash".to_string(),
            },
            source: FileSource::Workspace,
        };
        let resolver =
            LanguageResolver::from_workspace_config(workspace.path(), &WorkspaceConfig::default());

        let parsed = parse_candidate(candidate, &resolver)
            .result
            .expect("parsed Go file");
        let read = parsed
            .declarations
            .iter()
            .find(|declaration| declaration.name == "Read")
            .expect("Read declaration");
        assert_eq!(read.path, "src/sensor/read.go");
        assert_eq!(
            read.linkage,
            LinkageDomain::Package("src/sensor#sensor".to_string())
        );
    }
}
