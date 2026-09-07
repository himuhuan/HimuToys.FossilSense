use std::env;
use std::fs;
use std::sync::mpsc;
use std::time::Instant;

use anyhow::{Context, Result};
use rayon::prelude::*;
use rayon::ThreadPoolBuilder;

use super::candidates::FileCandidate;
use super::ProgressLimiter;
use crate::config::LanguageResolver;
use crate::parser::{parse_thread_local_with_selection, FileSemanticIndex, ParseFacts};
use crate::progress::{IndexStats, IndexStatus};
use crate::store::{FileIndexPayload, FileIndexUpdate, FileSource, IndexBuild, IndexStore};

const DEFAULT_MAX_PARSE_THREADS: usize = 8;
const PARSER_THREAD_STACK_SIZE: usize = 8 * 1024 * 1024;
const WRITE_BATCH_SIZE: usize = 128;

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

    let parse_started = Instant::now();
    let pool = ThreadPoolBuilder::new()
        .num_threads(config.parse_threads)
        .stack_size(PARSER_THREAD_STACK_SIZE)
        .thread_name(|idx| format!("fossilsense-parser-{idx}"))
        .build()
        .context("failed to create parser thread pool")?;
    let channel_capacity = config.parse_threads.saturating_mul(2).max(1);
    let (sender, receiver) = mpsc::sync_channel::<ParsedFile>(channel_capacity);
    let mut index_progress = ProgressLimiter::new();
    let cancellation = config.cancellation.clone();
    std::thread::scope(|scope| -> Result<()> {
        let producer = scope.spawn(move || {
            pool.install(|| {
                changed
                    .into_par_iter()
                    .for_each_with(sender, |sender, candidate| {
                        if config
                            .cancellation
                            .as_ref()
                            .is_some_and(|cancellation| cancellation.is_cancelled())
                        {
                            return;
                        }
                        // A closed receiver means the SQLite consumer failed; stop
                        // retaining parsed products and let the producer drain.
                        let _ = sender.send(parse_candidate(candidate, &config.language_resolver));
                    });
            });
        });

        let mut batch = Vec::with_capacity(WRITE_BATCH_SIZE);
        for parsed in receiver {
            if cancellation
                .as_ref()
                .is_some_and(|cancellation| cancellation.is_cancelled())
            {
                break;
            }
            batch.push(parsed);
            if batch.len() == WRITE_BATCH_SIZE {
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
            }
        }
        if !batch.is_empty() {
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
        producer
            .join()
            .map_err(|_| anyhow::anyhow!("parser producer thread panicked"))?;
        anyhow::ensure!(
            !cancellation
                .as_ref()
                .is_some_and(|cancellation| cancellation.is_cancelled()),
            "index build cancelled"
        );
        Ok(())
    })?;
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

fn parse_candidate(candidate: FileCandidate, language_resolver: &LanguageResolver) -> ParsedFile {
    let mut fingerprint = candidate.fingerprint;
    let result = match fs::read(&candidate.absolute_path) {
        Ok(bytes) => {
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
            // The thread-local parser uses the INDEX mask, skipping
            // request-time occurrence and local-declaration collection (those
            // vectors would be cleared before writing anyway).
            // It is infallible for ordinary parse problems (degrades to the
            // isolated completion fallback), so the only error here is the file read.
            let mut index = parse_thread_local_with_selection(
                identity_path,
                &source,
                selection,
                ParseFacts::INDEX,
            );
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

    ParsedFile {
        fingerprint,
        source: candidate.source,
        result,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::call_model::LinkageDomain;
    use crate::config::WorkspaceConfig;
    use crate::store::FileFingerprint;

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
                    mtime_ns: 1,
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
                size: 30,
                mtime_ns: 1,
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
