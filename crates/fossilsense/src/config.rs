use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

pub const DEFAULT_EXTENSIONS: &[&str] = &["c", "h", "cpp", "hpp", "cc", "hh", "cxx", "hxx", "inl"];
pub const DEFAULT_EXCLUDED_DIRS: &[&str] =
    &[".git", ".vscode", "node_modules", "target", "out", "build"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigIssue {
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceConfig {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub extensions: Vec<String>,
    pub excluded_dirs: Vec<String>,
    /// External C/C++ header reference directories (absolute, `/`-separated).
    /// Distinct from `include`, which selects *workspace* subtrees. Empty by
    /// default; never affects workspace traversal.
    pub include_paths: Vec<String>,

    /// Precomputed lookup structures derived from include/exclude/extensions
    /// at load time. Avoids repeated lowercasing, allocation, and linear scans
    /// during traversal hot paths.
    pub matchers: PrecomputedMatchers,
}

/// Precomputed matchers built at config-load time to eliminate per-call
/// lowercasing, allocation, and linear extension scans during walk/filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PrecomputedMatchers {
    /// Lowercased non-glob exclude entries.
    pub(crate) exclude_lower: Vec<String>,
    /// Lowercased non-glob include entries.
    pub(crate) include_lower: Vec<String>,
    /// Directory basenames to skip (already lowercase) for O(1) lookup.
    pub(crate) excluded_dirs_set: HashSet<String>,
    /// In-scope extensions with leading dot (already lowercase).
    pub(crate) extension_set: HashSet<String>,
    /// Precomputed ancestor-dir prefixes for the include set. When `include` is
    /// non-empty, a directory is kept if its relative path starts_with any of
    /// these prefixes, or if an include entry directly matches it. Each prefix
    /// has a trailing `/` so `starts_with` naturally enforces a boundary.
    pub(crate) include_ancestor_prefixes: Vec<String>,
    /// Include entries that contain glob metacharacters (`*?[{`). These cannot
    /// use fast set/prefix matching and use the small wildcard matcher below.
    pub(crate) include_glob_entries: Vec<String>,
    /// Exclude entries that contain glob metacharacters (`*?[{`). Kept separate
    /// from include globs so the two filters cannot cross-match each other.
    pub(crate) exclude_glob_entries: Vec<String>,
}

impl Default for PrecomputedMatchers {
    fn default() -> Self {
        Self {
            exclude_lower: Vec::new(),
            include_lower: Vec::new(),
            excluded_dirs_set: DEFAULT_EXCLUDED_DIRS
                .iter()
                .map(|d| d.to_ascii_lowercase())
                .collect(),
            extension_set: DEFAULT_EXTENSIONS
                .iter()
                .map(|ext| format!(".{}", ext.to_ascii_lowercase()))
                .collect(),
            include_ancestor_prefixes: Vec::new(),
            include_glob_entries: Vec::new(),
            exclude_glob_entries: Vec::new(),
        }
    }
}

impl PrecomputedMatchers {
    /// Returns `true` when `entry` contains glob metacharacters (`*`, `?`,
    /// `[`, `{`) and therefore cannot use fast exact/prefix matching.
    fn entry_is_glob(entry: &str) -> bool {
        entry.contains('*') || entry.contains('?') || entry.contains('[') || entry.contains('{')
    }

    /// Build matchers from the loaded config fields. Called once during
    /// [`WorkspaceConfig::load`], so the O(n) lowercasing is a fixed cost.
    fn build(config: &WorkspaceConfig) -> Self {
        let mut include_lower = Vec::new();
        let mut include_ancestor_prefixes = Vec::new();
        let mut include_glob_entries = Vec::new();

        for entry in &config.include {
            if Self::entry_is_glob(entry) {
                include_glob_entries.push(entry.clone());
            } else {
                let lower = entry.to_ascii_lowercase();
                include_lower.push(lower.clone());
                // Precompute ancestor prefixes: for "src/core/inner",
                // generate "src/" and "src/core/" so a directory path like
                // "src" or "src/core" is recognized as an ancestor.
                let mut pos = 0;
                while let Some(slash) = lower[pos..].find('/') {
                    // `slash` is the offset within lower[pos..], so the
                    // absolute index of '/' in `lower` is `pos + slash`.
                    let prefix = format!("{}/", &lower[..pos + slash]);
                    include_ancestor_prefixes.push(prefix);
                    pos += slash + 1;
                }
            }
        }
        // Deduplicate ancestor prefixes (multiple entries may share ancestors).
        include_ancestor_prefixes.sort();
        include_ancestor_prefixes.dedup();

        let exclude_lower: Vec<String> = config
            .exclude
            .iter()
            .filter(|entry| !Self::entry_is_glob(entry))
            .map(|entry| entry.to_ascii_lowercase())
            .collect();

        let mut exclude_glob_entries = Vec::new();
        for entry in &config.exclude {
            if Self::entry_is_glob(entry) {
                exclude_glob_entries.push(entry.clone());
            }
        }

        let excluded_dirs_set: HashSet<String> = config
            .excluded_dirs
            .iter()
            .map(|d| d.to_ascii_lowercase())
            .collect();

        let extension_set: HashSet<String> = config
            .extensions
            .iter()
            .map(|ext| format!(".{}", ext.to_ascii_lowercase()))
            .collect();

        Self {
            exclude_lower,
            include_lower,
            excluded_dirs_set,
            extension_set,
            include_ancestor_prefixes,
            include_glob_entries,
            exclude_glob_entries,
        }
    }
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            include: Vec::new(),
            exclude: Vec::new(),
            extensions: DEFAULT_EXTENSIONS
                .iter()
                .map(|extension| extension.to_string())
                .collect(),
            excluded_dirs: DEFAULT_EXCLUDED_DIRS
                .iter()
                .map(|dir| dir.to_string())
                .collect(),
            include_paths: Vec::new(),
            matchers: PrecomputedMatchers::default(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawConfig {
    #[serde(default)]
    include: Option<Vec<String>>,
    #[serde(default)]
    exclude: Option<Vec<String>>,
    #[serde(default)]
    extensions: Option<Vec<String>>,
    #[serde(default, rename = "includePaths")]
    include_paths: Option<Vec<String>>,
}

impl WorkspaceConfig {
    pub fn load(root: &Path) -> (Self, Option<ConfigIssue>) {
        let path = root.join("fossilsense.json");
        let raw: RawConfig = match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str(&contents) {
                Ok(cfg) => cfg,
                Err(err) => {
                    return (
                        Self::default(),
                        Some(ConfigIssue {
                            message: format!(
                                "failed to parse fossilsense.json: {err}. Using defaults."
                            ),
                        }),
                    );
                }
            },
            Err(_) => return (Self::default(), None),
        };

        let mut config = Self::default();

        if let Some(include) = raw.include {
            config.include = include.into_iter().map(normalize_entry).collect();
        }

        if let Some(exclude) = raw.exclude {
            config.exclude = exclude.into_iter().map(normalize_entry).collect();
        }

        if let Some(extensions) = raw.extensions {
            config.extensions = extensions
                .into_iter()
                .map(normalize_extension_entry)
                .collect();
        }

        if let Some(include_paths) = raw.include_paths {
            let (deduped, duplicate_issues) = dedupe_include_paths_with_issues(
                include_paths.into_iter().map(normalize_include_path_entry),
            );
            config.include_paths = deduped;
            if let Some(issue) = duplicate_issues.into_iter().next() {
                return (config, Some(issue));
            }
        }

        config.matchers = PrecomputedMatchers::build(&config);
        (config, None)
    }

    /// Cheap traversal-layer filter shared by the indexer, reference search,
    /// and CLI scan: decides whether a walk entry is kept and, for
    /// directories, descended into. The precise include/exclude/extension
    /// verdict is finalized per file by [`WorkspaceConfig::is_in_scope`].
    ///
    /// - The workspace root (empty relative path) is never pruned.
    /// - Directories are pruned when the name matches a default excluded dir,
    ///   when the directory falls under an `exclude` entry, or when a
    ///   non-empty `include` proves the subtree cannot contain an included
    ///   path. Ancestors of included subtrees are still descended.
    /// - Files are kept only when their extension is in scope.
    pub fn keep_during_walk(&self, rel_slash_path: &str, is_dir: bool) -> bool {
        if rel_slash_path.is_empty() {
            return true;
        }

        // Lowercase once and reuse across all sub-checks.
        let path_lower = rel_slash_path.to_ascii_lowercase();

        if is_dir {
            let name = rel_slash_path.rsplit('/').next().unwrap_or(rel_slash_path);
            let name_lower = name.to_ascii_lowercase();

            // O(1) excluded-dir check via precomputed HashSet.
            if self.matchers.excluded_dirs_set.contains(&name_lower) {
                return false;
            }

            // Non-glob exclude entries: fast path with pre-lowercased matching.
            if self
                .matchers
                .exclude_lower
                .iter()
                .any(|entry_lower| path_matches_entry_lower(&path_lower, entry_lower))
            {
                return false;
            }

            // Glob exclude entries (rare) fall back to per-call lowercasing.
            if !self.matchers.exclude_glob_entries.is_empty()
                && self
                    .matchers
                    .exclude_glob_entries
                    .iter()
                    .any(|entry| path_matches_glob_entry(rel_slash_path, entry))
            {
                return false;
            }

            // Include check: empty include = keep everything.
            if self.include.is_empty() {
                return true;
            }

            // Non-glob include entries: direct match or ancestor prefix.
            let include_matches = self
                .matchers
                .include_lower
                .iter()
                .any(|entry_lower| path_matches_entry_lower(&path_lower, entry_lower));
            let ancestor_matches = self
                .matchers
                .include_ancestor_prefixes
                .iter()
                .any(|prefix| {
                    // A directory matches if it *is* the parent (path == "src"
                    // for prefix "src/") or it *descends into* it.
                    let parent = &prefix[..prefix.len() - 1];
                    path_lower == parent || path_lower.starts_with(prefix.as_str())
                });
            if include_matches || ancestor_matches {
                return true;
            }

            // Glob include entries can match descendants that are not obvious
            // from the current directory alone (`src/*.c` must keep `src`).
            // Stay conservative here; `is_in_scope` still filters each file.
            !self.matchers.include_glob_entries.is_empty()
        } else {
            // File: fast extension check via precomputed HashSet.
            extension_from_slash_path_lower(&path_lower)
                .is_some_and(|ext_lower| self.matchers.extension_set.contains(ext_lower))
        }
    }

    pub fn is_in_scope(&self, rel_slash_path: &str) -> bool {
        // Lowercase once for all sub-checks.
        let path_lower = rel_slash_path.to_ascii_lowercase();

        // Include check via precomputed matchers.
        if !self.include.is_empty() {
            let include_match = self
                .matchers
                .include_lower
                .iter()
                .any(|entry_lower| path_matches_entry_lower(&path_lower, entry_lower));
            let glob_match = !self.matchers.include_glob_entries.is_empty()
                && self
                    .matchers
                    .include_glob_entries
                    .iter()
                    .any(|entry| path_matches_glob_entry(rel_slash_path, entry));
            if !include_match && !glob_match {
                return false;
            }
        }

        // Exclude check via precomputed matchers.
        let exclude_match = self
            .matchers
            .exclude_lower
            .iter()
            .any(|entry_lower| path_matches_entry_lower(&path_lower, entry_lower));
        let glob_exclude = !self.matchers.exclude_glob_entries.is_empty()
            && self
                .matchers
                .exclude_glob_entries
                .iter()
                .any(|entry| path_matches_glob_entry(rel_slash_path, entry));
        if exclude_match || glob_exclude {
            return false;
        }

        // Extension check via precomputed HashSet.
        extension_from_slash_path_lower(&path_lower)
            .is_some_and(|ext_lower| self.matchers.extension_set.contains(ext_lower))
    }

    /// Rebuild `matchers` from the current config fields. Needed after
    /// constructing a `WorkspaceConfig` via struct literal (e.g., in tests).
    /// Production code calls `WorkspaceConfig::load()` which does this
    /// automatically.
    #[cfg(test)]
    pub fn rebuild_matchers(&mut self) {
        self.matchers = PrecomputedMatchers::build(self);
    }
}

fn normalize_entry(entry: String) -> String {
    let mut s = entry.replace('\\', "/");
    s = s.trim_start_matches("./").to_string();
    s = s.trim_start_matches('/').to_string();
    s = s.trim_end_matches('/').to_string();
    s
}

fn normalize_extension_entry(ext: String) -> String {
    ext.trim_start_matches('.').to_ascii_lowercase()
}

/// Normalize an external include directory entry: switch to `/` separators and
/// drop a trailing slash, but preserve the leading part (these are *absolute*
/// paths, unlike workspace-relative `include`/`exclude` entries).
fn normalize_include_path_entry(entry: String) -> String {
    let mut s = entry.trim().replace('\\', "/");
    while s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    s
}

/// Drop blank and case-insensitively duplicate entries, preserving first-seen
/// order.
fn dedupe_include_paths_with_issues(
    entries: impl Iterator<Item = String>,
) -> (Vec<String>, Vec<ConfigIssue>) {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut issues = Vec::new();
    for entry in entries {
        if entry.is_empty() {
            continue;
        }
        if seen.insert(entry.to_ascii_lowercase()) {
            out.push(entry);
        } else {
            issues.push(ConfigIssue {
                message: format!("includePaths entry is a duplicate, skipping: {entry}"),
            });
        }
    }
    (out, issues)
}

/// Validate already-normalized include-path entries against the filesystem,
/// returning the directories that exist alongside a `ConfigIssue` for every
/// entry that is missing, not a directory, or a duplicate. Never fails: an
/// unusable entry is skipped with a note so indexing always proceeds.
pub fn resolve_include_roots(entries: &[String]) -> (Vec<PathBuf>, Vec<ConfigIssue>) {
    let (deduped, mut issues) = dedupe_include_paths_with_issues(entries.iter().cloned());
    let mut roots = Vec::new();

    for entry in deduped {
        let path = PathBuf::from(&entry);
        if !path.is_absolute() {
            issues.push(ConfigIssue {
                message: format!("includePaths entry is not absolute, skipping: {entry}"),
            });
            continue;
        }
        match std::fs::metadata(&path) {
            Ok(meta) if meta.is_dir() => roots.push(path),
            Ok(_) => issues.push(ConfigIssue {
                message: format!("includePaths entry is not a directory, skipping: {entry}"),
            }),
            Err(_) => issues.push(ConfigIssue {
                message: format!("includePaths entry not found, skipping: {entry}"),
            }),
        }
    }

    (roots, issues)
}

/// Fast match against a pre-lowercased non-glob entry. `path_lower` is the
/// (already lowered) relative path; `entry_lower` is the (already lowered)
/// include/exclude candidate. Comparison is exact or prefix-boundary-only
/// (`entry_lower + "/"`), with zero allocation during the match.
fn path_matches_entry_lower(path_lower: &str, entry_lower: &str) -> bool {
    if path_lower == entry_lower {
        return true;
    }
    // prefix-boundary: the entry must be a full path component prefix.
    let prefix_len = entry_lower.len();
    path_lower.len() > prefix_len
        && path_lower.as_bytes().get(prefix_len) == Some(&b'/')
        && path_lower.starts_with(entry_lower)
}

/// Literal entry point that still lowercases on each call. Kept for tests that
/// pin the path-boundary semantics independently from the hot-path caller.
#[cfg(test)]
fn path_matches_entry(rel_slash_path: &str, entry: &str) -> bool {
    let path_lower = rel_slash_path.to_ascii_lowercase();
    let entry_lower = entry.to_ascii_lowercase();
    path_matches_entry_lower(&path_lower, &entry_lower)
}

fn path_matches_glob_entry(rel_slash_path: &str, entry: &str) -> bool {
    wildcard_match(
        rel_slash_path.to_ascii_lowercase().as_bytes(),
        entry.to_ascii_lowercase().as_bytes(),
    )
}

fn wildcard_match(path: &[u8], pattern: &[u8]) -> bool {
    let mut path_idx = 0usize;
    let mut pattern_idx = 0usize;
    let mut star: Option<usize> = None;
    let mut star_path_idx = 0usize;

    while path_idx < path.len() {
        if pattern_idx < pattern.len() {
            match pattern[pattern_idx] {
                b'?' => {
                    path_idx += 1;
                    pattern_idx += 1;
                    continue;
                }
                b'*' => {
                    star = Some(pattern_idx);
                    pattern_idx += 1;
                    star_path_idx = path_idx;
                    continue;
                }
                b'[' => {
                    if let Some(next_pattern_idx) =
                        char_class_matches(path[path_idx], pattern, pattern_idx)
                    {
                        path_idx += 1;
                        pattern_idx = next_pattern_idx;
                        continue;
                    }
                }
                literal if literal == path[path_idx] => {
                    path_idx += 1;
                    pattern_idx += 1;
                    continue;
                }
                _ => {}
            }
        }

        if let Some(star_idx) = star {
            pattern_idx = star_idx + 1;
            star_path_idx += 1;
            path_idx = star_path_idx;
        } else {
            return false;
        }
    }

    while pattern_idx < pattern.len() && pattern[pattern_idx] == b'*' {
        pattern_idx += 1;
    }
    pattern_idx == pattern.len()
}

fn char_class_matches(ch: u8, pattern: &[u8], start: usize) -> Option<usize> {
    let mut idx = start + 1;
    if idx >= pattern.len() {
        return None;
    }

    let negated = matches!(pattern[idx], b'!' | b'^');
    if negated {
        idx += 1;
    }

    let mut matched = false;
    let mut saw_end = false;
    while idx < pattern.len() {
        if pattern[idx] == b']' {
            saw_end = true;
            break;
        }

        if idx + 2 < pattern.len() && pattern[idx + 1] == b'-' && pattern[idx + 2] != b']' {
            let start_ch = pattern[idx];
            let end_ch = pattern[idx + 2];
            if start_ch <= ch && ch <= end_ch {
                matched = true;
            }
            idx += 3;
        } else {
            if pattern[idx] == ch {
                matched = true;
            }
            idx += 1;
        }
    }

    if saw_end && matched != negated {
        Some(idx + 1)
    } else {
        None
    }
}

fn extension_from_slash_path_lower<'a>(path_lower: &'a str) -> Option<&'a str> {
    let name = path_lower.rsplit('/').next().unwrap_or(path_lower);
    let pos = name.rfind('.')?;
    if pos == 0 || pos == name.len() - 1 {
        return None;
    }
    Some(&name[pos..])
}

/// Borrowing extension helper. Returns the extension as-is (borrowed from
/// `path`) without lowercasing. Callers that need lowercase or owned storage
/// should call `.to_ascii_lowercase()` explicitly.
pub fn normalized_extension(path: &Path) -> Option<&str> {
    path.extension()?.to_str()
}

#[cfg(test)]
mod tests;
