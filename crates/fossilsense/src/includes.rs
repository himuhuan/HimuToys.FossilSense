//! Pure helpers for limited `#include` analysis: parsing/normalizing include
//! targets, form-aware/priority-ordered resolution of an include target to
//! indexed file(s), detecting an include-path completion context, and splitting
//! a typed partial path. Kept free of `tower-lsp` and store types so the lexical
//! logic unit-tests cleanly; the filesystem/index work lives in
//! `server` / `indexer`.

use std::collections::{HashMap, HashSet};

/// Delimiter form of an `#include` directive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncludeForm {
    /// `#include "..."`
    Quote,
    /// `#include <...>`
    Angle,
}

/// How an include target matched an indexed file. The four kinds cover every
/// *exact*-tier or unique-suffix tier that produces one proven target. An
/// exactly-matching tier is never relabeled as a suffix match — suffix is the
/// weak last-resort path recorded so its weakness stays visible to tests and
/// future confidence projection, never laundered into an exact kind.
///
/// All variants are best-effort path resolution against the indexed corpus,
/// **not** a compiler-level binding: FossilSense never compiles, so real `-I`
/// search paths beyond configured `includePaths` stay unknown, which is why
/// suffix matching exists as a last resort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionKind {
    /// `src_dir/rel` exact match against an indexed file (quote form only —
    /// the including file's own directory takes priority).
    RelativeExact,
    /// `rel` is itself an indexed workspace path (workspace exact). Both forms
    /// use this tier after their higher-priority exact tier misses.
    WorkspaceExact,
    /// `root/rel` matches against a configured include path root (external).
    /// Angle form considers this tier first.
    ExternalExact,
    /// Exactly one indexed file carries the include's basename / `/rel` suffix
    /// (the weak last-resort tier).
    SuffixMatch,
}

impl ResolutionKind {
    /// Stable lowercase string used in tests and persisted on `include_edges`.
    /// Never localized; never reordered.
    pub fn as_str(self) -> &'static str {
        match self {
            ResolutionKind::RelativeExact => "relative_exact",
            ResolutionKind::WorkspaceExact => "workspace_exact",
            ResolutionKind::ExternalExact => "external_exact",
            ResolutionKind::SuffixMatch => "suffix_match",
        }
    }

    /// Reverse of [`ResolutionKind::as_str`]; unknown strings (a future or
    /// hand-edited row) fall back to [`ResolutionKind::SuffixMatch`] so a load
    /// path never panics.
    #[allow(dead_code)]
    pub fn from_str(s: &str) -> Self {
        match s {
            "relative_exact" => ResolutionKind::RelativeExact,
            "workspace_exact" => ResolutionKind::WorkspaceExact,
            "external_exact" => ResolutionKind::ExternalExact,
            "suffix_match" => ResolutionKind::SuffixMatch,
            _ => ResolutionKind::SuffixMatch,
        }
    }
}

/// Outcome of resolving one `#include` target against the indexed corpus. An
/// include is resolved in a single priority/form-driven pass; the result is
/// *one* `Edge` (a proven target) when a unique match is found, `Ambiguous`
/// when two or more candidates match with no exact-tier winner, or `Unresolved`
/// when nothing matched at all. An `Ambiguous` resolution produces **no**
/// proven-reachable edge: its candidates stay heuristic. Coloring may use those
/// bounded candidates only as kind evidence and never promotes them to strong
/// reachability. All variants are best-effort path resolution, not semantic
/// binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IncludeResolution {
    /// One proven target with the kind that matched it. The first exact tier
    /// producing exactly one target short-circuits — even if a lower tier
    /// would also have matched — so a local exact hit is the semantically
    /// correct target, never a co-equal ambiguous candidate.
    Edge { dst: String, kind: ResolutionKind },
    /// Two or more candidates match with no exact-tier winner (only the suffix
    /// tier can produce `Ambiguous`). The candidates are recorded for
    /// navigation/diagnosis but no proven-reachable edge is produced.
    Ambiguous { dsts: Vec<String> },
    /// No indexed file matched at any tier.
    Unresolved,
}

/// Resolve one raw `#include` target text (e.g. `<sys/types.h>`, `"util.h"`)
/// against the indexed corpus using a form-driven, priority-ordered search.
///
/// Priority (per the schedule in [`normalize_include_target`]'s form):
/// - **Quote** (`"…"`): ① `src_dir/rel` exact (`RelativeExact`) → ② `rel`
///   itself as an indexed workspace path (`WorkspaceExact`) → ③ `root/rel`
///   exact against any configured root (`ExternalExact`) → ④ basename/suffix
///   match across workspace files.
/// - **Angle** (`<…>`): ① `root/rel` exact (`ExternalExact`) → ② `rel` itself
///   as an indexed workspace path (`WorkspaceExact`) → ③ basename/suffix
///   match across workspace files.
///
/// The first exact tier producing exactly one target **short-circuits** and is
/// never ambiguous (this is what fixes the same-basename case: `"util.h"` next
/// to its header resolves `RelativeExact`, never against `vendor/util.h`).
/// Only the suffix tier can produce `Ambiguous` (two or more candidates);
/// exactly one suffix candidate yields a `SuffixMatch` edge.
///
/// `roots_slash` are the configured include-path roots already normalized to
/// `/`-separated absolute strings; `all_paths` is every indexed file path
/// (workspace and external); `by_basename` maps a workspace file's final path
/// segment to its full paths (the cheap suffix-match index over workspace
/// files only — external files always have a root prefix so they are matched
/// exactly in the external tier). `src_dir` is the including file's directory
/// in the same `/`-separated workspace-relative form (empty for an external
/// includer, which then behaves like the angle form).
pub fn resolve_include(
    target_text: &str,
    src_dir: &str,
    roots_slash: &[String],
    all_paths: &HashSet<String>,
    by_basename: &HashMap<String, Vec<String>>,
) -> IncludeResolution {
    let Some((form, rel)) = normalize_include_target(target_text) else {
        // Macro-constructed or malformed include text: cannot be matched
        // best-effort, so it counts as unresolved.
        return IncludeResolution::Unresolved;
    };

    // Helper: an exact `root/rel` match against any configured root.
    let external_exact = || -> Option<String> {
        for root in roots_slash {
            let candidate = format!("{}/{}", root.trim_end_matches('/'), rel);
            if all_paths.contains(&candidate) {
                return Some(candidate);
            }
        }
        None
    };
    // Helper: an exact `src_dir/rel` match (quote form's first tier).
    let relative_exact = || -> Option<String> {
        if src_dir.is_empty() {
            return None;
        }
        let candidate = format!("{src_dir}/{rel}");
        if all_paths.contains(&candidate) {
            Some(candidate)
        } else {
            None
        }
    };
    // Helper: `rel` itself is an indexed workspace path. Works for any form
    // since `rel` may already be a workspace-relative path; we accept any
    // indexed file (workspace or external) here because the indexer feeds all
    // paths through — external exactitudes still round-trip cleanly.
    let workspace_exact = || -> Option<String> {
        if all_paths.contains(&rel) {
            Some(rel.clone())
        } else {
            None
        }
    };

    // Drive the appropriate priority order, short-circuiting on the first exact
    // tier that yields exactly one target.
    match form {
        IncludeForm::Quote => {
            if let Some(dst) = relative_exact() {
                return IncludeResolution::Edge {
                    dst,
                    kind: ResolutionKind::RelativeExact,
                };
            }
            if let Some(dst) = workspace_exact() {
                return IncludeResolution::Edge {
                    dst,
                    kind: ResolutionKind::WorkspaceExact,
                };
            }
            if let Some(dst) = external_exact() {
                return IncludeResolution::Edge {
                    dst,
                    kind: ResolutionKind::ExternalExact,
                };
            }
        }
        IncludeForm::Angle => {
            if let Some(dst) = external_exact() {
                return IncludeResolution::Edge {
                    dst,
                    kind: ResolutionKind::ExternalExact,
                };
            }
            if let Some(dst) = workspace_exact() {
                return IncludeResolution::Edge {
                    dst,
                    kind: ResolutionKind::WorkspaceExact,
                };
            }
        }
    }

    // Suffix tier: the weak last-resort path across workspace files. Only the
    // basename/suffix tier can produce `Ambiguous` — every exact tier above
    // short-circuits on a single hit.
    let last = rel.rsplit('/').next().unwrap_or(&rel).to_string();
    let suffix = format!("/{rel}");
    let mut candidates: Vec<String> = by_basename
        .get(&last)
        .into_iter()
        .flatten()
        .filter(|candidate| candidate == &&rel || candidate.ends_with(&suffix))
        .cloned()
        .collect();
    candidates.sort();
    candidates.dedup();
    match candidates.len() {
        0 => IncludeResolution::Unresolved,
        1 => IncludeResolution::Edge {
            dst: candidates.into_iter().next().unwrap(),
            kind: ResolutionKind::SuffixMatch,
        },
        _ => IncludeResolution::Ambiguous { dsts: candidates },
    }
}

/// True when `trimmed` (already left-trimmed) begins an `#include` directive,
/// tolerating spaces between `#` and `include` (e.g. `#  include`).
fn is_include_line(trimmed: &str) -> bool {
    let Some(rest) = trimmed.strip_prefix('#') else {
        return false;
    };
    rest.trim_start().starts_with("include")
}

/// Parse the inner header path out of text that *contains* an include target,
/// e.g. the parser's stored `target_text` (`<stdio.h>`, `"util.h"`, possibly
/// with a trailing comment). Returns the delimiter form and the slash-normalized
/// inner path, or `None` when no well-formed target is present.
pub fn normalize_include_target(text: &str) -> Option<(IncludeForm, String)> {
    let open = text.find(['"', '<'])?;
    let open_ch = text[open..].chars().next()?;
    let (form, close_ch) = if open_ch == '"' {
        (IncludeForm::Quote, '"')
    } else {
        (IncludeForm::Angle, '>')
    };
    let after = &text[open + open_ch.len_utf8()..];
    let close = after.find(close_ch)?;
    let path = after[..close].trim().replace('\\', "/");
    if path.is_empty() {
        return None;
    }
    Some((form, path))
}

/// Parse an `#include` directive from a full source line. `None` when the line
/// is not an include directive or has no well-formed target.
pub fn parse_include_line(line: &str) -> Option<(IncludeForm, String)> {
    if !is_include_line(line.trim_start()) {
        return None;
    }
    normalize_include_target(line)
}

/// If `character` (a UTF-16 column) sits inside the delimiters of an `#include`
/// directive on `line`, return the delimiter form and the partial header path
/// typed *before* the cursor. `None` otherwise.
///
/// The partial may include sub-directories (`sys/ty`) and is slash-normalized.
pub fn include_completion_context(line: &str, character: u32) -> Option<(IncludeForm, String)> {
    if !is_include_line(line.trim_start()) {
        return None;
    }
    let chars: Vec<char> = line.chars().collect();
    let cursor = char_index_at_utf16(&chars, character).min(chars.len());

    // The most recent opening delimiter before the cursor decides the form.
    let mut open: Option<(usize, IncludeForm)> = None;
    for (i, &ch) in chars.iter().enumerate().take(cursor) {
        match ch {
            '"' => open = Some((i, IncludeForm::Quote)),
            '<' => open = Some((i, IncludeForm::Angle)),
            _ => {}
        }
    }
    let (open_idx, form) = open?;

    // A closing delimiter between the opener and the cursor means the directive
    // is already closed: the cursor is past it, not inside the path.
    let close_ch = match form {
        IncludeForm::Quote => '"',
        IncludeForm::Angle => '>',
    };
    if chars[(open_idx + 1)..cursor].contains(&close_ch) {
        return None;
    }

    let partial: String = chars[(open_idx + 1)..cursor].iter().collect();
    Some((form, partial.replace('\\', "/")))
}

/// Split a typed include partial into its directory component (with trailing
/// slash, or empty) and the final-segment prefix. `"sys/ty"` → `("sys/", "ty")`;
/// `"std"` → `("", "std")`.
pub fn split_partial(partial: &str) -> (String, String) {
    match partial.rfind('/') {
        Some(i) => (partial[..=i].to_string(), partial[i + 1..].to_string()),
        None => (String::new(), partial.to_string()),
    }
}

/// Map a UTF-16 column to a char index within `chars` (mirrors `query`'s helper;
/// duplicated here to keep this module dependency-free).
fn char_index_at_utf16(chars: &[char], character: u32) -> usize {
    let mut units = 0u32;
    for (idx, ch) in chars.iter().enumerate() {
        if units >= character {
            return idx;
        }
        units += ch.len_utf16() as u32;
        if units > character {
            return idx;
        }
    }
    chars.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_quote_and_angle_targets() {
        assert_eq!(
            normalize_include_target("\"hello.h\""),
            Some((IncludeForm::Quote, "hello.h".to_string()))
        );
        assert_eq!(
            normalize_include_target("<sys/types.h>"),
            Some((IncludeForm::Angle, "sys/types.h".to_string()))
        );
    }

    #[test]
    fn normalize_tolerates_trailing_comment_and_backslashes() {
        assert_eq!(
            normalize_include_target("<windows.h> // platform"),
            Some((IncludeForm::Angle, "windows.h".to_string()))
        );
        assert_eq!(
            normalize_include_target("\"sub\\dir\\a.h\""),
            Some((IncludeForm::Quote, "sub/dir/a.h".to_string()))
        );
    }

    #[test]
    fn normalize_rejects_malformed() {
        assert_eq!(normalize_include_target("stdio.h"), None);
        assert_eq!(normalize_include_target("<unterminated"), None);
        assert_eq!(normalize_include_target("<>"), None);
    }

    #[test]
    fn parse_include_line_handles_spacing_and_non_includes() {
        assert_eq!(
            parse_include_line("#  include <stdio.h>"),
            Some((IncludeForm::Angle, "stdio.h".to_string()))
        );
        assert_eq!(
            parse_include_line("   #include \"util.h\""),
            Some((IncludeForm::Quote, "util.h".to_string()))
        );
        assert_eq!(parse_include_line("int x = a < b;"), None);
        assert_eq!(parse_include_line("#define FOO 1"), None);
    }

    #[test]
    fn completion_context_detects_quote_and_angle() {
        // Cursor right after the partial `fo` inside quotes.
        let line = "#include \"fo\"";
        let col = line.find("fo").unwrap() as u32 + 2;
        assert_eq!(
            include_completion_context(line, col),
            Some((IncludeForm::Quote, "fo".to_string()))
        );

        let line = "#include <sys/ty>";
        let col = line.find("ty").unwrap() as u32 + 2;
        assert_eq!(
            include_completion_context(line, col),
            Some((IncludeForm::Angle, "sys/ty".to_string()))
        );
    }

    #[test]
    fn completion_context_empty_partial_right_after_delimiter() {
        let line = "#include <";
        assert_eq!(
            include_completion_context(line, line.chars().count() as u32),
            Some((IncludeForm::Angle, String::new()))
        );
    }

    #[test]
    fn completion_context_none_outside_include_or_past_close() {
        // Non-include line.
        assert_eq!(include_completion_context("int a = b;", 5), None);
        // Cursor after the closing delimiter is not a path context.
        let line = "#include <stdio.h> ";
        assert_eq!(
            include_completion_context(line, line.chars().count() as u32),
            None
        );
    }

    #[test]
    fn split_partial_separates_dir_and_segment() {
        assert_eq!(
            split_partial("sys/ty"),
            ("sys/".to_string(), "ty".to_string())
        );
        assert_eq!(split_partial("std"), (String::new(), "std".to_string()));
        assert_eq!(split_partial("a/b/"), ("a/b/".to_string(), String::new()));
    }

    // --- resolve_include: form-aware, priority-ordered resolution ---------

    fn resolve_corpus(paths: &[&str]) -> (HashSet<String>, HashMap<String, Vec<String>>) {
        let all: HashSet<String> = paths.iter().map(|p| p.to_string()).collect();
        let mut by_basename: HashMap<String, Vec<String>> = HashMap::new();
        for p in paths {
            let last = p.rsplit('/').next().unwrap_or(p).to_string();
            by_basename.entry(last).or_default().push(p.to_string());
        }
        // Each basename bucket stays sorted + deduped so resolve_include's
        // suffix tier returns a deterministic set.
        for bucket in by_basename.values_mut() {
            bucket.sort();
            bucket.dedup();
        }
        (all, by_basename)
    }

    #[test]
    fn quote_resolves_local_dir_over_same_basename_elsewhere() {
        // src/a/foo.c #include "util.h"; both src/a/util.h and vendor/util.h
        // are indexed. RelativeExact wins; vendor/util.h is not a candidate.
        let (all, by_base) = resolve_corpus(&["src/a/foo.c", "src/a/util.h", "vendor/util.h"]);
        let roots: Vec<String> = Vec::new();
        let res = resolve_include("\"util.h\"", "src/a", &roots, &all, &by_base);
        assert_eq!(
            res,
            IncludeResolution::Edge {
                dst: "src/a/util.h".to_string(),
                kind: ResolutionKind::RelativeExact,
            }
        );
    }

    #[test]
    fn angle_prefers_external_root_over_workspace_basename() {
        // src/a.c #include <stdio.h>; stdio.h exists under a configured root
        // AND a workspace file also carries that basename. ExternalExact wins.
        let (all, by_base) = resolve_corpus(&["src/a.c", "deep/stdio.h"]);
        let roots = vec!["C:/mingw/include".to_string()];
        // Add the external stdio.h path (it is not a workspace file, so it is
        // absent from by_basename regardless — external_exact matches it).
        let mut all = all;
        all.insert("C:/mingw/include/stdio.h".to_string());
        let res = resolve_include("<stdio.h>", "src", &roots, &all, &by_base);
        assert_eq!(
            res,
            IncludeResolution::Edge {
                dst: "C:/mingw/include/stdio.h".to_string(),
                kind: ResolutionKind::ExternalExact,
            }
        );
    }

    #[test]
    fn quote_with_external_root_only_after_local_and_workspace_miss() {
        // Quote form still considers the external root once the higher tiers
        // miss. "#{includePaths}" root holds the header.
        let (all, by_base) = resolve_corpus(&["src/a.c"]);
        let roots = vec!["C:/mingw/include".to_string()];
        let mut all = all;
        all.insert("C:/mingw/include/stdio.h".to_string());
        let res = resolve_include("\"stdio.h\"", "src", &roots, &all, &by_base);
        assert_eq!(
            res,
            IncludeResolution::Edge {
                dst: "C:/mingw/include/stdio.h".to_string(),
                kind: ResolutionKind::ExternalExact,
            }
        );
    }

    #[test]
    fn workspace_relative_path_resolves_exactly() {
        // a.c #include "inc/util.h"; inc/util.h is an indexed workspace path.
        let (all, by_base) = resolve_corpus(&["a.c", "inc/util.h"]);
        let roots: Vec<String> = Vec::new();
        let res = resolve_include("\"inc/util.h\"", "", &roots, &all, &by_base);
        assert_eq!(
            res,
            IncludeResolution::Edge {
                dst: "inc/util.h".to_string(),
                kind: ResolutionKind::WorkspaceExact,
            }
        );
    }

    #[test]
    fn unique_suffix_match_yields_suffix_edge() {
        // a.c #include "only.h"; no exact tier, exactly one workspace file
        // carries the basename.
        let (all, by_base) = resolve_corpus(&["a.c", "deep/dir/only.h"]);
        let roots: Vec<String> = Vec::new();
        let res = resolve_include("\"only.h\"", "", &roots, &all, &by_base);
        assert_eq!(
            res,
            IncludeResolution::Edge {
                dst: "deep/dir/only.h".to_string(),
                kind: ResolutionKind::SuffixMatch,
            }
        );
    }

    #[test]
    fn multi_hit_suffix_is_ambiguous_no_edge() {
        // src/x.c #include "util.h"; no exact tier, and both lib/util.h and
        // vendor/util.h carry the basename → Ambiguous, no proven edge.
        let (all, by_base) = resolve_corpus(&["src/x.c", "lib/util.h", "vendor/util.h"]);
        let roots: Vec<String> = Vec::new();
        let res = resolve_include("\"util.h\"", "src/x", &roots, &all, &by_base);
        let dsts = match res {
            IncludeResolution::Ambiguous { dsts } => dsts,
            other => panic!("expected Ambiguous, got {other:?}"),
        };
        assert_eq!(
            dsts,
            vec!["lib/util.h".to_string(), "vendor/util.h".to_string()]
        );
    }

    #[test]
    fn zero_hit_is_unresolved() {
        // a.c #include <unconfigured.h>; nothing matches at any tier.
        let (all, by_base) = resolve_corpus(&["a.c"]);
        let roots: Vec<String> = Vec::new();
        let res = resolve_include("<unconfigured.h>", "", &roots, &all, &by_base);
        assert_eq!(res, IncludeResolution::Unresolved);
    }

    #[test]
    fn macro_constructed_or_malformed_is_unresolved() {
        // `SOME_MACRO` is not a well-formed include target (no delimiters).
        let (all, by_base) = resolve_corpus(&["a.c"]);
        let roots: Vec<String> = Vec::new();
        let res = resolve_include("SOME_MACRO", "", &roots, &all, &by_base);
        assert_eq!(res, IncludeResolution::Unresolved);
    }

    #[test]
    fn resolution_kind_round_trips_through_stable_strings() {
        // The kind persisted on edges MUST round-trip through `as_str`/`from_str`
        // so a future schema never silently relabels an edge.
        let kinds = [
            (ResolutionKind::RelativeExact, "relative_exact"),
            (ResolutionKind::WorkspaceExact, "workspace_exact"),
            (ResolutionKind::ExternalExact, "external_exact"),
            (ResolutionKind::SuffixMatch, "suffix_match"),
        ];
        for (kind, s) in kinds {
            assert_eq!(kind.as_str(), s);
            assert_eq!(ResolutionKind::from_str(s), kind);
        }
        // Unknown persisted strings fall back to SuffixMatch (the weakest
        // proven kind), so a hand-edited or future row can never panic a
        // load path.
        assert_eq!(
            ResolutionKind::from_str("not_a_real_kind"),
            ResolutionKind::SuffixMatch
        );
    }

    #[test]
    fn quote_short_circuits_in_local_dir_even_if_lower_tier_also_matches() {
        // Same-basename guarantee: if `src/a/util.h` exists, `"util.h"` from
        // src/a resolves to it and `vendor/util.h` is NOT included as a
        // co-equal candidate, even though the suffix tier would also match.
        let (all, by_base) = resolve_corpus(&["src/a/foo.c", "src/a/util.h", "vendor/util.h"]);
        let roots: Vec<String> = Vec::new();
        let res = resolve_include("\"util.h\"", "src/a", &roots, &all, &by_base);
        assert!(
            matches!(
                res,
                IncludeResolution::Edge {
                    kind: ResolutionKind::RelativeExact,
                    ..
                }
            ),
            "RelativeExact short-circuits; Ambiguous must not surface"
        );
    }
}
