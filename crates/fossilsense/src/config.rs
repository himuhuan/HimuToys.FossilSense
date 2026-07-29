use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

pub use crate::semantic_model::SemanticFamily;
use crate::semantic_model::SemanticLanguage;

mod matching;
use matching::{language_override_glob_matches, path_matches_glob_entry};

pub const DEFAULT_EXCLUDED_DIRS: &[&str] =
    &[".git", ".vscode", "node_modules", "target", "out", "build"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceLanguage {
    C,
    Cpp,
    Go,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParserFrontend {
    CFamily,
    Go,
}

#[derive(Debug, Clone, Copy)]
pub struct LanguageBackend {
    pub language: SourceLanguage,
    pub config_name: &'static str,
    pub default_extensions: &'static [&'static str],
    pub semantic_family: SemanticFamily,
    pub parser_frontend: ParserFrontend,
    grammar: fn() -> tree_sitter::Language,
}

const BUILT_IN_LANGUAGE_BACKENDS: &[LanguageBackend] = &[
    LanguageBackend {
        language: SourceLanguage::C,
        config_name: "c",
        default_extensions: &["c"],
        semantic_family: SemanticFamily::CFamily,
        parser_frontend: ParserFrontend::CFamily,
        grammar: c_grammar,
    },
    LanguageBackend {
        language: SourceLanguage::Cpp,
        config_name: "cpp",
        default_extensions: &["h", "cpp", "hpp", "cc", "hh", "cxx", "hxx", "inl"],
        semantic_family: SemanticFamily::CFamily,
        parser_frontend: ParserFrontend::CFamily,
        grammar: cpp_grammar,
    },
    LanguageBackend {
        language: SourceLanguage::Go,
        config_name: "go",
        default_extensions: &["go"],
        semantic_family: SemanticFamily::Go,
        parser_frontend: ParserFrontend::Go,
        grammar: go_grammar,
    },
];

pub fn built_in_language_backends() -> &'static [LanguageBackend] {
    BUILT_IN_LANGUAGE_BACKENDS
}

impl LanguageBackend {
    pub fn tree_sitter_language(self) -> tree_sitter::Language {
        (self.grammar)()
    }
}

fn c_grammar() -> tree_sitter::Language {
    tree_sitter_c::LANGUAGE.into()
}

fn cpp_grammar() -> tree_sitter::Language {
    tree_sitter_cpp::LANGUAGE.into()
}

fn go_grammar() -> tree_sitter::Language {
    tree_sitter_go::LANGUAGE.into()
}

fn default_extensions() -> impl Iterator<Item = &'static str> {
    built_in_language_backends()
        .iter()
        .flat_map(|backend| backend.default_extensions.iter().copied())
}

impl SourceLanguage {
    pub fn default_for_path(path: &Path) -> Self {
        let extension = normalized_extension(path).map(str::to_ascii_lowercase);
        extension
            .as_deref()
            .and_then(|extension| {
                built_in_language_backends()
                    .iter()
                    .find(|backend| backend.default_extensions.contains(&extension))
                    .map(|backend| backend.language)
            })
            .unwrap_or(Self::C)
    }

    pub fn from_config_name(name: &str) -> Option<Self> {
        built_in_language_backends()
            .iter()
            .find(|backend| backend.config_name == name)
            .map(|backend| backend.language)
    }

    pub fn semantic_family(self) -> SemanticFamily {
        self.backend().semantic_family
    }

    pub fn semantic_language(self) -> SemanticLanguage {
        match self {
            Self::C => SemanticLanguage::C,
            Self::Cpp => SemanticLanguage::Cpp,
            Self::Go => SemanticLanguage::Go,
        }
    }

    pub fn parser_frontend(self) -> ParserFrontend {
        self.backend().parser_frontend
    }

    pub fn tree_sitter_language(self) -> tree_sitter::Language {
        self.backend().tree_sitter_language()
    }

    fn backend(self) -> &'static LanguageBackend {
        BUILT_IN_LANGUAGE_BACKENDS
            .iter()
            .find(|backend| backend.language == self)
            .expect("every SourceLanguage must have one built-in backend")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageOverride {
    pub glob: String,
    pub language: SourceLanguage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageResolver {
    workspace_root: Option<PathBuf>,
    overrides: Vec<LanguageOverride>,
}

impl LanguageResolver {
    pub fn new(workspace_root: Option<&Path>, overrides: Vec<LanguageOverride>) -> Self {
        Self {
            workspace_root: workspace_root.map(Path::to_path_buf),
            overrides,
        }
    }

    pub fn from_workspace_config(workspace_root: &Path, config: &WorkspaceConfig) -> Self {
        Self::new(Some(workspace_root), config.language_overrides.clone())
    }

    pub fn language_for_path(&self, path: &Path) -> SourceLanguage {
        self.overridden_language_for_path(path)
            .unwrap_or_else(|| SourceLanguage::default_for_path(path))
    }

    pub fn overridden_language_for_path(&self, path: &Path) -> Option<SourceLanguage> {
        let target = self.match_path(path);
        self.overrides
            .iter()
            .rev()
            .find(|rule| language_override_glob_matches(&target, &rule.glob))
            .map(|rule| rule.language)
    }

    fn match_path(&self, path: &Path) -> String {
        if let Some(root) = &self.workspace_root {
            if let Ok(relative) = path.strip_prefix(root) {
                return normalize_language_match_path(relative);
            }
        }
        normalize_language_match_path(path)
    }
}

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
    /// Explicit external Go module directories. They are independently capped
    /// and never inferred from the machine's GOPATH or module cache.
    pub go_module_paths: Vec<String>,
    pub language_overrides: Vec<LanguageOverride>,

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
            extension_set: default_extensions()
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
            extensions: default_extensions()
                .map(|extension| extension.to_string())
                .collect(),
            excluded_dirs: DEFAULT_EXCLUDED_DIRS
                .iter()
                .map(|dir| dir.to_string())
                .collect(),
            include_paths: Vec::new(),
            go_module_paths: Vec::new(),
            language_overrides: Vec::new(),
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
    #[serde(default, rename = "goModulePaths")]
    go_module_paths: Option<Vec<String>>,
    #[serde(default, rename = "languageOverrides")]
    language_overrides: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct RawLanguageOverride {
    glob: String,
    language: String,
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
        let mut issues = Vec::new();

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
            issues.extend(duplicate_issues);
        }

        if let Some(go_module_paths) = raw.go_module_paths {
            let (deduped, duplicate_issues) = dedupe_external_paths_with_issues(
                go_module_paths
                    .into_iter()
                    .map(normalize_include_path_entry),
                "goModulePaths",
            );
            config.go_module_paths = deduped;
            issues.extend(duplicate_issues);
        }

        if let Some(overrides) = raw.language_overrides {
            let overrides = match overrides {
                Value::Array(overrides) => overrides,
                _ => {
                    issues.push(ConfigIssue {
                        message: "languageOverrides must be an array; ignoring only that field"
                            .to_string(),
                    });
                    Vec::new()
                }
            };
            for raw_rule in overrides {
                let rule: RawLanguageOverride = match serde_json::from_value(raw_rule) {
                    Ok(rule) => rule,
                    Err(error) => {
                        issues.push(ConfigIssue {
                            message: format!(
                                "languageOverrides entry is malformed, skipping: {error}"
                            ),
                        });
                        continue;
                    }
                };
                let raw_glob = rule.glob;
                let glob = normalize_language_override_glob(raw_glob.clone());
                let normalized_language = rule.language.trim().to_ascii_lowercase();
                let language = SourceLanguage::from_config_name(&normalized_language);
                if glob.is_empty() || !valid_language_override_glob(&glob) {
                    issues.push(ConfigIssue {
                        message: format!(
                            "languageOverrides entry has an invalid glob, skipping: {}",
                            raw_glob
                        ),
                    });
                    continue;
                }
                let Some(language) = language else {
                    issues.push(ConfigIssue {
                        message: format!(
                            "languageOverrides entry has an invalid language, skipping: {}",
                            rule.language
                        ),
                    });
                    continue;
                };
                config
                    .language_overrides
                    .push(LanguageOverride { glob, language });
            }
        }

        config.matchers = PrecomputedMatchers::build(&config);
        let issue = (!issues.is_empty()).then(|| ConfigIssue {
            message: issues
                .into_iter()
                .map(|issue| issue.message)
                .collect::<Vec<_>>()
                .join("; "),
        });
        (config, issue)
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
        if !self.is_path_allowed_by_scope_without_extension(rel_slash_path) {
            return false;
        }

        let path_lower = rel_slash_path.to_ascii_lowercase();
        extension_from_slash_path_lower(&path_lower)
            .is_some_and(|ext_lower| self.matchers.extension_set.contains(ext_lower))
    }

    /// Apply the workspace include/exclude policy without requiring a source
    /// extension. Build-marker discovery uses this after the shared traversal
    /// filter has pruned default-excluded directories.
    pub fn is_path_allowed_by_scope_without_extension(&self, rel_slash_path: &str) -> bool {
        // Lowercase once for all sub-checks.
        let path_lower = rel_slash_path.to_ascii_lowercase();

        if path_lower
            .split('/')
            .any(|segment| self.matchers.excluded_dirs_set.contains(segment))
        {
            return false;
        }

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

        true
    }

    /// Decide whether a build marker may contribute a project root. A marker
    /// directory can be either inside the selected source scope or an ancestor
    /// of it (for example a root `CMakeLists.txt` with `include = ["src"]`).
    /// Full-path exclusion and default excluded-directory rules still apply.
    pub fn is_project_marker_in_scope(&self, rel_slash_path: &str) -> bool {
        let path_lower = rel_slash_path.to_ascii_lowercase();
        if path_lower
            .split('/')
            .any(|segment| self.matchers.excluded_dirs_set.contains(segment))
        {
            return false;
        }
        if self
            .matchers
            .exclude_lower
            .iter()
            .any(|entry_lower| path_matches_entry_lower(&path_lower, entry_lower))
            || self
                .matchers
                .exclude_glob_entries
                .iter()
                .any(|entry| path_matches_glob_entry(rel_slash_path, entry))
        {
            return false;
        }

        let parent = rel_slash_path
            .rsplit_once('/')
            .map_or("", |(parent, _)| parent);
        self.keep_during_walk(parent, true)
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

fn normalize_language_override_glob(entry: String) -> String {
    entry
        .trim()
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_string()
}

fn normalize_language_match_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn valid_language_override_glob(glob: &str) -> bool {
    let mut depth = 0usize;
    for ch in glob.chars() {
        match ch {
            '[' => depth += 1,
            ']' if depth == 0 => return false,
            ']' => depth -= 1,
            _ => {}
        }
    }
    depth == 0
}

/// Drop blank and case-insensitively duplicate entries, preserving first-seen
/// order.
fn dedupe_include_paths_with_issues(
    entries: impl Iterator<Item = String>,
) -> (Vec<String>, Vec<ConfigIssue>) {
    dedupe_external_paths_with_issues(entries, "includePaths")
}

fn dedupe_external_paths_with_issues(
    entries: impl Iterator<Item = String>,
    field_name: &str,
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
                message: format!("{field_name} entry is a duplicate, skipping: {entry}"),
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
    resolve_external_roots(entries, "includePaths")
}

pub fn resolve_go_module_roots(entries: &[String]) -> (Vec<PathBuf>, Vec<ConfigIssue>) {
    resolve_external_roots(entries, "goModulePaths")
}

fn resolve_external_roots(
    entries: &[String],
    field_name: &str,
) -> (Vec<PathBuf>, Vec<ConfigIssue>) {
    let (deduped, mut issues) =
        dedupe_external_paths_with_issues(entries.iter().cloned(), field_name);
    let mut roots = Vec::new();

    for entry in deduped {
        let path = PathBuf::from(&entry);
        if !path.is_absolute() {
            issues.push(ConfigIssue {
                message: format!("{field_name} entry is not absolute, skipping: {entry}"),
            });
            continue;
        }
        match std::fs::metadata(&path) {
            Ok(meta) if meta.is_dir() => roots.push(path),
            Ok(_) => issues.push(ConfigIssue {
                message: format!("{field_name} entry is not a directory, skipping: {entry}"),
            }),
            Err(_) => issues.push(ConfigIssue {
                message: format!("{field_name} entry not found, skipping: {entry}"),
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

fn extension_from_slash_path_lower(path_lower: &str) -> Option<&str> {
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
