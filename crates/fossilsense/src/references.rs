//! Text-level reference search using the ripgrep kernel, with best-effort
//! syntactic-role classification layered on top.
//!
//! Discovery stays a case-sensitive whole-word search (`\b<ident>\b`) over the
//! workspace. The returned positions are exactly those of the text search; each
//! is then annotated with a [`SyntacticRole`] derived on demand by parsing only
//! the files that produced hits (cached per file fingerprint). Classification is
//! best-effort: an unparseable file leaves its hits as `Read` rather than
//! dropping them, per the project's "honest fallback" principle.
//!
//! References are text hits, not resolved definitions: a [`ReferenceHit`] does
//! not carry a [`ScopeTier`](crate::model::ScopeTier) and is not re-ranked by
//! the shared resolver. The role grouping below reuses the candidate-model
//! vocabulary (`ScopeTier` / `ResolutionReason` terms) in its doc-comments so
//! the codebase has one scope/ranking vocabulary, but the grouping order itself
//! is unchanged and a text hit is never presented as a bound reference.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use grep_matcher::Matcher;
use grep_regex::RegexMatcher;
use grep_searcher::sinks::Lossy;
use grep_searcher::SearcherBuilder;
use rayon::prelude::*;

use crate::config::WorkspaceConfig;
use crate::parser::{self, SyntacticRole};
use crate::pathing;

/// Per-phase timing for a references search.
#[derive(Debug, Clone, Default)]
pub struct ReferencesTiming {
    /// Total wall-clock time spent in the reference search path.
    pub total_ms: u64,
    /// Time spent discovering candidate files (workspace walk / index lookup).
    pub discover_ms: u64,
    /// Time spent in the ripgrep text-search pass.
    pub search_ms: u64,
    /// Time spent classifying hits by syntactic role (parse + position lookup).
    pub classify_ms: u64,
    /// Total number of occurrences classified across all files. Proportionally
    /// large values suggest per-occurrence `String` allocation may be material.
    pub total_occurrences: u64,
    /// Whether this result came from the complete search-result cache.
    pub cached: bool,
}

/// Default cap on reference results.
pub const REFERENCES_LIMIT: usize = 2000;

/// Max number of files whose classified occurrences are kept in memory.
const ROLE_CACHE_CAP: usize = 256;

/// Max number of full reference-search result sets kept in memory. This cache is
/// intentionally small: it exists so a grouped-references command immediately
/// after the standard references request can reuse the same text hits without a
/// second workspace discovery pass.
const SEARCH_CACHE_CAP: usize = 32;

/// A single text-level reference hit, annotated with its best-effort role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceHit {
    /// Repository-relative path with `/` separators.
    pub rel_path: String,
    /// 0-based line number.
    pub line: u32,
    /// 0-based UTF-16 code-unit column where the identifier starts.
    pub start_col_utf16: u32,
    /// 0-based UTF-16 code-unit column where the identifier ends.
    pub end_col_utf16: u32,
    /// Best-effort syntactic role at this position; `Read` when the file could
    /// not be parsed or the position did not map to a parsed occurrence.
    pub role: SyntacticRole,
}

/// Grouping order for the editor: definition/declaration first, then call,
/// write, type-use, and plain reads last. Lower sorts earlier. This is the
/// reference-side counterpart to the candidate model's
/// `ResolutionConfidence`/`ResolutionReason` ordering — it groups text hits by
/// syntactic role (not a resolved `ScopeTier`), so a `Read` hit is never
/// implied to be a bound reference.
pub fn role_rank(role: SyntacticRole) -> u8 {
    match role {
        SyntacticRole::Definition => 0,
        SyntacticRole::Declaration => 1,
        SyntacticRole::Call => 2,
        SyntacticRole::Write => 3,
        SyntacticRole::TypeUse => 4,
        SyntacticRole::Read => 5,
    }
}

/// Short, user-facing label for a syntactic role, used by the grouped-references
/// command to head each role group. A best-effort syntactic classification, not
/// a resolved binding — `read` is the fallback for unparsed or unclassified
/// hits.
pub fn role_label(role: SyntacticRole) -> &'static str {
    match role {
        SyntacticRole::Definition => "definition",
        SyntacticRole::Declaration => "declaration",
        SyntacticRole::Call => "call",
        SyntacticRole::Write => "write",
        SyntacticRole::TypeUse => "type",
        SyntacticRole::Read => "read",
    }
}

/// Sort hits into role groups (definition/declaration first, then call, write,
/// type-use, and plain reads last); ties keep path/line/column order so each
/// file's hits stay contiguous. Shared by `textDocument/references` and the
/// grouped-references command so both present the same order. This groups text
/// hits by syntactic role — it does not re-rank by a resolved `ScopeTier`.
pub fn sort_hits_by_role(hits: &mut [ReferenceHit]) {
    hits.sort_by(|a, b| {
        role_rank(a.role)
            .cmp(&role_rank(b.role))
            .then_with(|| a.rel_path.cmp(&b.rel_path))
            .then(a.line.cmp(&b.line))
            .then(a.start_col_utf16.cmp(&b.start_col_utf16))
    });
}

/// A bounded, fingerprint-keyed cache of per-file classified occurrence roles,
/// so repeated/overlapping reference queries do not re-parse unchanged files.
/// Keyed by absolute path string; value is a position → role map for every
/// identifier in the file (reusable across different reference words).
#[derive(Default)]
pub struct ReferenceRoleCache {
    inner: Mutex<RoleCacheInner>,
}

#[derive(Default)]
struct RoleCacheInner {
    entries: HashMap<String, CacheEntry>,
    order: VecDeque<String>,
}

struct CacheEntry {
    fingerprint: (u64, u64),
    roles: Arc<HashMap<(u32, u32), SyntacticRole>>,
}

impl ReferenceRoleCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn get(
        &self,
        key: &str,
        fingerprint: (u64, u64),
    ) -> Option<Arc<HashMap<(u32, u32), SyntacticRole>>> {
        let inner = self.inner.lock().ok()?;
        match inner.entries.get(key) {
            Some(entry) if entry.fingerprint == fingerprint => Some(entry.roles.clone()),
            _ => None,
        }
    }

    fn put(
        &self,
        key: String,
        fingerprint: (u64, u64),
        roles: Arc<HashMap<(u32, u32), SyntacticRole>>,
    ) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if !inner.entries.contains_key(&key) {
            inner.order.push_back(key.clone());
        }
        inner.entries.insert(key, CacheEntry { fingerprint, roles });
        while inner.entries.len() > ROLE_CACHE_CAP {
            match inner.order.pop_front() {
                Some(old) => {
                    inner.entries.remove(&old);
                }
                None => break,
            }
        }
    }
}

/// Cache for complete text-search results keyed by `(root, identifier)`.
///
/// This is separate from [`ReferenceRoleCache`]: the role cache prevents
/// re-parsing matched files, while this result cache prevents a repeated
/// references surface from rediscovering files and re-running the text search.
#[derive(Default)]
pub struct ReferenceSearchCache {
    inner: Mutex<SearchCacheInner>,
}

#[derive(Default)]
struct SearchCacheInner {
    entries: HashMap<SearchCacheKey, SearchCacheEntry>,
    order: VecDeque<SearchCacheKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SearchCacheKey {
    root: String,
    identifier: String,
    generation: u64,
}

#[derive(Clone)]
struct SearchCacheEntry {
    hits: Arc<Vec<ReferenceHit>>,
    truncated: bool,
}

impl ReferenceSearchCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&self) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner.entries.clear();
        inner.order.clear();
    }

    fn get(&self, key: &SearchCacheKey) -> Option<(Vec<ReferenceHit>, bool)> {
        let inner = self.inner.lock().ok()?;
        inner
            .entries
            .get(key)
            .map(|entry| ((*entry.hits).clone(), entry.truncated))
    }

    fn put(&self, key: SearchCacheKey, hits: Vec<ReferenceHit>, truncated: bool) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if !inner.entries.contains_key(&key) {
            inner.order.push_back(key.clone());
        }
        inner.entries.insert(
            key,
            SearchCacheEntry {
                hits: Arc::new(hits),
                truncated,
            },
        );
        while inner.entries.len() > SEARCH_CACHE_CAP {
            match inner.order.pop_front() {
                Some(old) => {
                    inner.entries.remove(&old);
                }
                None => break,
            }
        }
    }
}

/// Search `root` for whole-word occurrences of `identifier`.
///
/// Returns the hits and a flag indicating whether results were truncated to
/// `REFERENCES_LIMIT`. Hits are sorted by path, line, and column.
pub fn search_references(
    root: impl AsRef<Path>,
    identifier: &str,
) -> Result<(Vec<ReferenceHit>, bool, ReferencesTiming)> {
    search_references_inner(root, identifier, None, None)
}

/// Cache-backed variant of [`search_references`]: classified occurrences for
/// unchanged files are reused across queries via `cache`.
#[cfg(test)]
pub fn search_references_cached(
    root: impl AsRef<Path>,
    identifier: &str,
    cache: &ReferenceRoleCache,
) -> Result<(Vec<ReferenceHit>, bool, ReferencesTiming)> {
    search_references_inner(root, identifier, Some(cache), None)
}

/// Cache-backed references search for server surfaces that may be invoked back
/// to back (standard references, then grouped references). A cache hit returns
/// the same text hits without re-discovering files or re-running the grep pass.
#[cfg(test)]
pub fn search_references_with_result_cache(
    root: impl AsRef<Path>,
    identifier: &str,
    role_cache: &ReferenceRoleCache,
    search_cache: &ReferenceSearchCache,
) -> Result<(Vec<ReferenceHit>, bool, ReferencesTiming)> {
    search_references_with_result_cache_and_files(
        root,
        identifier,
        role_cache,
        search_cache,
        0,
        None,
    )
}

pub fn search_references_with_result_cache_and_files(
    root: impl AsRef<Path>,
    identifier: &str,
    role_cache: &ReferenceRoleCache,
    search_cache: &ReferenceSearchCache,
    generation: u64,
    indexed_files: Option<Vec<(String, PathBuf)>>,
) -> Result<(Vec<ReferenceHit>, bool, ReferencesTiming)> {
    let root = root.as_ref().canonicalize().with_context(|| {
        format!(
            "failed to canonicalize workspace root {}",
            root.as_ref().display()
        )
    })?;
    let key = SearchCacheKey {
        root: pathing::normalize_abs_path(&root),
        identifier: identifier.to_string(),
        generation,
    };
    if let Some(cached) = search_cache.get(&key) {
        return Ok((
            cached.0,
            cached.1,
            ReferencesTiming {
                cached: true,
                ..Default::default()
            },
        ));
    }
    let (hits, truncated, timing) =
        search_references_inner(&root, identifier, Some(role_cache), indexed_files)?;
    search_cache.put(key, hits.clone(), truncated);
    Ok((hits, truncated, timing))
}

fn search_references_inner(
    root: impl AsRef<Path>,
    identifier: &str,
    cache: Option<&ReferenceRoleCache>,
    indexed_files: Option<Vec<(String, PathBuf)>>,
) -> Result<(Vec<ReferenceHit>, bool, ReferencesTiming)> {
    let root = root.as_ref().canonicalize().with_context(|| {
        format!(
            "failed to canonicalize workspace root {}",
            root.as_ref().display()
        )
    })?;

    if identifier.is_empty() {
        return Ok((Vec::new(), false, ReferencesTiming::default()));
    }
    let total_started = Instant::now();

    let pattern = format!(r"\b{}\b", regex::escape(identifier));
    let matcher = RegexMatcher::new(&pattern)
        .with_context(|| format!("failed to compile reference pattern {pattern}"))?;

    let (config, _config_issue) = WorkspaceConfig::load(&root);
    let discover_started = Instant::now();
    let candidates = match indexed_files {
        Some(files) if !files.is_empty() => files,
        _ => discover_reference_files(&root, &config),
    };
    let mut timing = ReferencesTiming {
        discover_ms: discover_started.elapsed().as_millis() as u64,
        ..Default::default()
    };

    let file_results: Result<Vec<_>> = candidates
        .par_iter()
        .map(|(rel_path, path)| {
            let mut file_hits = Vec::new();
            let search_started = Instant::now();
            search_file_references(&matcher, path, rel_path, &mut file_hits)?;
            let search_elapsed = search_started.elapsed();

            let mut classify_elapsed = Duration::ZERO;
            let mut occurrence_count = 0usize;
            if !file_hits.is_empty() {
                let classify_started = Instant::now();
                occurrence_count = classify_file_hits(path, &mut file_hits, cache);
                classify_elapsed = classify_started.elapsed();
            }

            Ok((
                file_hits,
                search_elapsed,
                classify_elapsed,
                occurrence_count,
            ))
        })
        .collect();

    let mut hits = Vec::new();
    let mut search_acc = Duration::ZERO;
    let mut classify_acc = Duration::ZERO;
    for (mut file_hits, search_elapsed, classify_elapsed, occurrence_count) in file_results? {
        search_acc += search_elapsed;
        classify_acc += classify_elapsed;
        timing.total_occurrences += occurrence_count as u64;
        hits.append(&mut file_hits);
    }

    timing.search_ms = search_acc.as_millis() as u64;
    timing.classify_ms = classify_acc.as_millis() as u64;
    hits.sort_by(|a, b| {
        a.rel_path
            .cmp(&b.rel_path)
            .then(a.line.cmp(&b.line))
            .then(a.start_col_utf16.cmp(&b.start_col_utf16))
    });
    let truncated = hits.len() > REFERENCES_LIMIT;
    hits.truncate(REFERENCES_LIMIT);
    timing.total_ms = total_started.elapsed().as_millis() as u64;

    Ok((hits, truncated, timing))
}

/// Annotate `file_hits` with syntactic roles by parsing `abs_path` once.
/// Leaves hits at their default `Read` role when the file cannot be read,
/// cannot be parsed, or a hit position does not map to a parsed occurrence.
/// Returns the number of occurrences (role positions) in the file, or 0 on
/// classification failure — useful for profiling per-file occurrence cost.
fn classify_file_hits(
    abs_path: &Path,
    file_hits: &mut [ReferenceHit],
    cache: Option<&ReferenceRoleCache>,
) -> usize {
    let Some(roles) = position_roles(abs_path, cache) else {
        return 0;
    };
    let count = roles.len();
    for hit in file_hits.iter_mut() {
        if let Some(role) = roles.get(&(hit.line, hit.start_col_utf16)) {
            hit.role = *role;
        }
    }
    count
}

/// Position → role map for every identifier in `abs_path`. Served from `cache`
/// when the file fingerprint is unchanged, otherwise parsed and cached.
fn position_roles(
    abs_path: &Path,
    cache: Option<&ReferenceRoleCache>,
) -> Option<Arc<HashMap<(u32, u32), SyntacticRole>>> {
    let fingerprint = file_fingerprint(abs_path)?;
    let key = abs_path.to_string_lossy().into_owned();

    if let Some(cache) = cache {
        if let Some(roles) = cache.get(&key, fingerprint) {
            return Some(roles);
        }
    }

    let source = std::fs::read_to_string(abs_path).ok()?;
    let occurrences = parser::parse(abs_path, &source).occurrences;
    let mut map: HashMap<(u32, u32), SyntacticRole> = HashMap::with_capacity(occurrences.len());
    for occ in occurrences {
        map.entry((occ.line, occ.start_col)).or_insert(occ.role);
    }
    let roles = Arc::new(map);

    if let Some(cache) = cache {
        cache.put(key, fingerprint, roles.clone());
    }
    Some(roles)
}

/// Cheap content fingerprint: (mtime in ns, byte length). `None` when the file
/// cannot be stat'd, which makes classification fall back to default roles.
fn file_fingerprint(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    Some((mtime, meta.len()))
}

fn discover_reference_files(root: &Path, config: &WorkspaceConfig) -> Vec<(String, PathBuf)> {
    let walk_config = config.clone();
    let filter_root = root.to_path_buf();
    let scope_config = config.clone();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .parents(true)
        .git_ignore(true)
        .git_global(true)
        .filter_entry(move |entry| {
            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            let rel = pathing::relative_slash_path(&filter_root, entry.path()).unwrap_or_default();
            walk_config.keep_during_walk(&rel, is_dir)
        })
        .build();

    let mut candidates = Vec::new();
    for entry in walker {
        let Ok(entry) = entry else {
            continue;
        };
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path().to_path_buf();
        let Ok(rel_path) = pathing::relative_slash_path(root, &path) else {
            continue;
        };
        if !scope_config.is_in_scope(&rel_path) {
            continue;
        }
        candidates.push((rel_path, path));
    }

    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    candidates
}

fn search_file_references(
    matcher: &RegexMatcher,
    path: &Path,
    rel_path: &str,
    hits: &mut Vec<ReferenceHit>,
) -> Result<()> {
    let mut searcher = SearcherBuilder::new().line_number(true).build();

    // `Lossy` keeps files with invalid UTF-8 (e.g. GBK comments next to
    // ASCII code) searchable; the column on such lines is approximate.
    searcher.search_path(
        matcher,
        path,
        Lossy(|line_no, line| {
            matcher
                .find_iter(line.as_bytes(), |m| {
                    let start_col = byte_offset_to_utf16_col(line.as_bytes(), m.start());
                    let end_col = byte_offset_to_utf16_col(line.as_bytes(), m.end());
                    hits.push(ReferenceHit {
                        rel_path: rel_path.to_string(),
                        line: line_no.saturating_sub(1) as u32,
                        start_col_utf16: start_col,
                        end_col_utf16: end_col,
                        // Default; upgraded by `classify_file_hits` after the
                        // file is parsed.
                        role: SyntacticRole::Read,
                    });
                    hits.len() <= REFERENCES_LIMIT
                })
                .map_err(|err| std::io::Error::other(err.to_string()))?;
            Ok(hits.len() <= REFERENCES_LIMIT)
        }),
    )?;

    Ok(())
}

/// Convert a byte offset inside a line into a UTF-16 code-unit column.
///
/// Invalid UTF-8 is decoded lossily; the result may be slightly off but the
/// function never panics.
fn byte_offset_to_utf16_col(line_bytes: &[u8], byte_offset: usize) -> u32 {
    let end = byte_offset.min(line_bytes.len());
    let prefix = &line_bytes[..end];
    let text = String::from_utf8_lossy(prefix);
    text.chars().map(|ch| ch.len_utf16() as u32).sum()
}

#[cfg(test)]
mod tests;
