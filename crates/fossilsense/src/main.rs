mod coloring;
mod completion;
mod completion_history;
mod completion_words;
mod config;
mod includes;
mod indexer;
mod language_builtins;
mod model;
mod parser;
mod pathing;
mod progress;
mod query;
mod reachability;
mod references;
mod resolver;
mod scanner;
mod server;
mod store;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::store::IndexStore;

#[derive(Debug, Parser)]
#[command(name = "fossilsense")]
#[command(about = "FossilSense best-effort C/C++ navigation and analysis")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start the Language Server Protocol server over stdio.
    Lsp,
    /// Build or update the persistent FossilSense index for a workspace.
    Index {
        /// Workspace root to index.
        workspace: PathBuf,
        /// Override the SQLite index path for testing or diagnostics.
        #[arg(long)]
        db: Option<PathBuf>,
        /// Rebuild all source files even if fingerprints are unchanged.
        #[arg(long)]
        force: bool,
    },
    /// Scan a workspace and report C/C++ files that would enter the index.
    Scan {
        /// Workspace root to scan.
        workspace: PathBuf,
    },
    /// Query an existing index headlessly (no editor) for debugging.
    Query {
        #[command(subcommand)]
        kind: QueryCommand,
    },
}

#[derive(Debug, Subcommand)]
enum QueryCommand {
    /// Fuzzy workspace symbol search over the in-memory name table.
    Symbol {
        /// Workspace root whose index to query.
        workspace: PathBuf,
        /// Fuzzy search text.
        text: String,
        /// Override the SQLite index path (defaults to the cache location).
        #[arg(long)]
        db: Option<PathBuf>,
    },
    /// Resolve the definition candidates for the identifier at a position.
    Def {
        /// Workspace root whose index to query.
        workspace: PathBuf,
        /// Source file, relative to the workspace root.
        file: PathBuf,
        /// 1-based line number of the cursor.
        line: usize,
        /// 1-based column of the cursor.
        col: usize,
        /// Override the SQLite index path (defaults to the cache location).
        #[arg(long)]
        db: Option<PathBuf>,
    },
    /// Find best-effort text-candidate references for the identifier at a
    /// position (whole-word matches; not resolved semantic references).
    Refs {
        /// Workspace root to search.
        workspace: PathBuf,
        /// Source file, relative to the workspace root.
        file: PathBuf,
        /// 1-based line number of the cursor.
        line: usize,
        /// 1-based column of the cursor.
        col: usize,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Lsp => server::run_stdio().await,
        Command::Index {
            workspace,
            db,
            force,
        } => {
            let stats = indexer::index_workspace(
                workspace,
                indexer::IndexOptions {
                    db_path: db,
                    force,
                    ..Default::default()
                },
                |status| {
                    // During indexing a populated message denotes a scope-config
                    // warning (see WorkspaceConfig::load); surface it to stderr and
                    // skip the progress line for that synthetic status.
                    if let Some(message) = &status.message {
                        eprintln!("warning: {message}");
                        return;
                    }
                    if matches!(status.state, progress::IndexState::Indexing) {
                        let phase = status.phase.as_deref().unwrap_or("indexing");
                        if status.total_files == 0 {
                            println!("{phase} files...");
                            return;
                        }
                        println!(
                            "{phase} {}/{} files (indexed {}, skipped {}, symbols {})",
                            status.processed_files,
                            status.total_files,
                            status.indexed_files,
                            status.skipped_files,
                            status.symbols
                        );
                    }
                },
            )?;

            println!("FossilSense index");
            println!("files: {}", stats.total_files);
            println!("indexed: {}", stats.indexed_files);
            println!("skipped: {}", stats.skipped_files);
            println!("deleted: {}", stats.deleted_files);
            println!("symbols: {}", stats.symbols);
            println!("elapsed_ms: {}", stats.elapsed_ms);
            println!("discover_ms: {}", stats.discover_ms);
            println!("parse_ms: {}", stats.parse_ms);
            println!("write_ms: {}", stats.write_ms);
            println!("check_ms: {}", stats.check_ms);
            println!("include_edge_ms: {}", stats.include_edge_ms);
            println!("name_table_ms: {}", stats.name_table_ms);
            println!("reach_graph_ms: {}", stats.reach_graph_ms);
            Ok(())
        }
        Command::Scan { workspace } => {
            let (summary, config_issue) = scanner::scan_workspace(&workspace)?;
            if let Some(issue) = &config_issue {
                eprintln!("warning: {}", issue.message);
            }
            println!("FossilSense scan");
            println!("root: {}", summary.root.display());
            println!("files: {}", summary.files.len());

            for file in &summary.files {
                println!("{}", file.display());
            }

            Ok(())
        }
        Command::Query { kind } => run_query(kind),
    }
}

fn run_query(kind: QueryCommand) -> Result<()> {
    match kind {
        QueryCommand::Symbol {
            workspace,
            text,
            db,
        } => {
            let db_path = resolve_db_path(db, &workspace)?;
            let store = IndexStore::open_readonly(&db_path)?;
            let table = query::NameTable::build_from_rows(store.name_table_view().symbol_rows()?);
            let ids: Vec<i64> = table.search(&text, query::WORKSPACE_SYMBOL_LIMIT);
            let records = store.symbol_read_view().symbols_by_ids(&ids)?;

            println!("symbols: {} (of {} names)", records.len(), table.len());
            for record in records {
                print_record(&record);
            }
            Ok(())
        }
        QueryCommand::Def {
            workspace,
            file,
            line,
            col,
            db,
        } => {
            let db_path = resolve_db_path(db, &workspace)?;
            let abs = workspace.join(&file);
            let content = fs::read_to_string(&abs)
                .with_context(|| format!("failed to read {}", abs.display()))?;
            let line_index = line.checked_sub(1).context("line is 1-based")?;
            let line_text = content.lines().nth(line_index).unwrap_or_default();
            let character = col.saturating_sub(1) as u32;
            let word = query::word_at(line_text, character)
                .with_context(|| format!("no identifier at {}:{}:{}", file.display(), line, col))?;

            let store = IndexStore::open_readonly(&db_path)?;
            let rel = pathing::normalize_path_string(&file);
            let candidates = query::rank_definitions_into_candidates_with_scope(
                store.symbol_read_view().symbols_by_name(&word)?,
                &rel,
                None,
            );

            println!("identifier: {word}");
            println!("candidates: {}", candidates.len());
            for candidate in &candidates {
                print_candidate(candidate);
            }
            Ok(())
        }
        QueryCommand::Refs {
            workspace,
            file,
            line,
            col,
        } => {
            let abs = workspace.join(&file);
            let content = fs::read_to_string(&abs)
                .with_context(|| format!("failed to read {}", abs.display()))?;
            let line_index = line.checked_sub(1).context("line is 1-based")?;
            let line_text = content.lines().nth(line_index).unwrap_or_default();
            let character = col.saturating_sub(1) as u32;
            let word = query::word_at(line_text, character)
                .with_context(|| format!("no identifier at {}:{}:{}", file.display(), line, col))?;

            let (hits, truncated, _) = references::search_references(&workspace, &word)?;

            println!("identifier: {word}");
            println!(
                "hits: {}{}",
                hits.len(),
                if truncated { " (truncated)" } else { "" }
            );
            for hit in hits {
                println!(
                    "{}:{}:{}",
                    hit.rel_path,
                    hit.line + 1,
                    hit.start_col_utf16 + 1
                );
            }
            Ok(())
        }
    }
}

fn resolve_db_path(db: Option<PathBuf>, workspace: &Path) -> Result<PathBuf> {
    match db {
        Some(path) => Ok(path),
        None => {
            let workspace = pathing::canonical_workspace(workspace)?;
            pathing::default_index_path(&workspace)
        }
    }
}

fn print_record(record: &store::SymbolRecord) {
    let guard = record
        .guard
        .as_deref()
        .map(|guard| format!("  [{guard}]"))
        .unwrap_or_default();
    println!(
        "{}\t{}\t{}\t{}:{}{}",
        record.name,
        record.kind,
        record.role,
        record.path,
        record.start_line + 1,
        guard
    );
}

/// Print a labeled goto-definition candidate with its confidence and reason.
fn print_candidate(candidate: &model::DefinitionCandidate) {
    println!(
        "{}\t{}\t{}\t{}:{}\t{}:{}",
        candidate.name,
        candidate.kind,
        candidate.role,
        candidate.path,
        candidate.range.start_line + 1,
        candidate.confidence.as_str(),
        candidate.reason.as_str(),
    );
}
