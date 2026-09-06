use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::{normalized_extension, ConfigIssue, WorkspaceConfig};
use crate::pathing::relative_slash_path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanSummary {
    pub root: PathBuf,
    pub files: Vec<PathBuf>,
    pub extension_counts: BTreeMap<String, usize>,
}

pub fn scan_workspace(root: impl AsRef<Path>) -> Result<(ScanSummary, Option<ConfigIssue>)> {
    let root = root.as_ref();
    let root = root
        .canonicalize()
        .with_context(|| format!("failed to canonicalize workspace root {}", root.display()))?;

    let (config, config_issue) = WorkspaceConfig::load(&root);

    let mut files = Vec::new();
    let mut extension_counts = BTreeMap::new();

    // Walk with the same `ignore`-based semantics as the indexer and reference
    // search (respects `.gitignore` + scope config) so all three paths agree
    // on the file set.
    let walker = workspace_walk_builder(&root, &config).build();

    for entry in walker {
        let entry =
            entry.with_context(|| format!("failed to read entry under {}", root.display()))?;
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }

        let rel_slash = relative_slash_path(&root, entry.path())?;
        if !config.is_in_scope(&rel_slash) {
            continue;
        }

        if let Some(ext) = normalized_extension(entry.path()) {
            *extension_counts
                .entry(ext.to_ascii_lowercase())
                .or_insert(0) += 1;
        }

        files.push(PathBuf::from(&rel_slash));
    }

    files.sort();

    Ok((
        ScanSummary {
            root,
            files,
            extension_counts,
        },
        config_issue,
    ))
}

fn workspace_walk_builder(root: &Path, config: &WorkspaceConfig) -> ignore::WalkBuilder {
    let walk_config = config.clone();
    let filter_root = root.to_path_buf();
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .hidden(false)
        .parents(true)
        .git_ignore(true)
        .git_global(true)
        .filter_entry(move |entry| {
            let is_dir = entry.file_type().is_some_and(|ft| ft.is_dir());
            let rel = relative_slash_path(&filter_root, entry.path()).unwrap_or_default();
            walk_config.keep_during_walk(&rel, is_dir)
        });
    builder
}

#[derive(Debug, Default)]
pub(crate) struct FileInclusion {
    pub included: bool,
    pub inspected: usize,
    pub truncated: bool,
}

/// Reuse the scanner's ignore/config matcher, visiting only the target's
/// ancestor directories. The public visitor can stop and prune before any
/// unrelated subtree is enumerated; explicit file roots would bypass ignores.
pub(crate) fn file_inclusion(
    root: &Path,
    target: &Path,
    config: &WorkspaceConfig,
    limit: usize,
) -> Result<FileInclusion> {
    let rel = relative_slash_path(root, target)?;
    if !config.is_in_scope(&rel) {
        return Ok(FileInclusion::default());
    }
    let state = std::sync::Mutex::new((FileInclusion::default(), None));
    workspace_walk_builder(root, config)
        .threads(1)
        .build_parallel()
        .run(|| {
            Box::new(|entry| {
                let mut state = state.lock().expect("single scanner visitor");
                if state.0.inspected >= limit {
                    state.0.truncated = true;
                    return ignore::WalkState::Quit;
                }
                state.0.inspected += 1;
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        state.1 = Some(error);
                        return ignore::WalkState::Quit;
                    }
                };
                if entry.file_type().is_some_and(|kind| kind.is_dir()) {
                    return if crate::pathing::path_is_within(entry.path(), target) {
                        ignore::WalkState::Continue
                    } else {
                        ignore::WalkState::Skip
                    };
                }
                if entry.file_type().is_some_and(|kind| kind.is_file())
                    && crate::pathing::path_is_within(entry.path(), target)
                    && crate::pathing::path_is_within(target, entry.path())
                {
                    state.0.included = true;
                    return ignore::WalkState::Quit;
                }
                ignore::WalkState::Continue
            })
        });
    let (result, error) = state.into_inner().expect("single scanner visitor");
    if let Some(error) = error {
        return Err(error.into());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use tempfile::tempdir;

    use super::scan_workspace;

    #[test]
    fn single_file_inclusion_matches_real_scan_rules_and_honors_visit_limit() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::create_dir(dir.path().join("nested")).unwrap();
        fs::write(dir.path().join(".gitignore"), "ignored.c\n").unwrap();
        fs::write(dir.path().join("nested/.ignore"), "hidden.c\n").unwrap();
        for file in ["main.c", "ignored.c", "nested/good.c", "nested/hidden.c"] {
            fs::write(dir.path().join(file), "int value;\n").unwrap();
        }
        let root = dir.path().canonicalize().unwrap();
        let (config, _) = crate::config::WorkspaceConfig::load(&root);
        let (scan, _) = scan_workspace(&root).unwrap();
        for file in ["main.c", "ignored.c", "nested/good.c", "nested/hidden.c"] {
            let actual = super::file_inclusion(&root, &root.join(file), &config, 128).unwrap();
            assert_eq!(
                actual.included,
                scan.files.contains(&PathBuf::from(file)),
                "{file}"
            );
            assert!(!actual.truncated);
        }
        let bounded =
            super::file_inclusion(&root, &root.join("nested/good.c"), &config, 0).unwrap();
        assert!(bounded.truncated);
        assert_eq!(bounded.inspected, 0);
    }

    #[test]
    fn scans_cpp_like_files_and_skips_default_excludes() {
        let dir = tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("src")).expect("src");
        fs::create_dir_all(dir.path().join("target")).expect("target");
        fs::write(
            dir.path().join("src/main.c"),
            "int main(void) { return 0; }",
        )
        .expect("main");
        fs::write(dir.path().join("src/lib.HPP"), "#pragma once").expect("header");
        fs::write(
            dir.path().join("src/device.go"),
            "package device\n\nfunc Ready() bool { return true }\n",
        )
        .expect("go source");
        fs::write(dir.path().join("src/readme.txt"), "ignored").expect("txt");
        fs::write(dir.path().join("target/generated.c"), "ignored();").expect("generated");

        let (summary, _) = scan_workspace(dir.path()).expect("scan");

        assert_eq!(summary.files.len(), 3);
        assert!(summary
            .files
            .iter()
            .any(|file| file.ends_with("src/main.c")));
        assert!(summary
            .files
            .iter()
            .any(|file| file.ends_with("src/lib.HPP")));
        assert!(summary
            .files
            .iter()
            .any(|file| file.ends_with("src/device.go")));
        assert_eq!(summary.extension_counts.get("go"), Some(&1));
    }

    #[test]
    fn respects_fossilsense_json_include() {
        let dir = tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("src")).expect("src");
        fs::create_dir_all(dir.path().join("lib")).expect("lib");
        fs::write(
            dir.path().join("fossilsense.json"),
            r#"{"include": ["src/"]}"#,
        )
        .expect("config");
        fs::write(dir.path().join("src/main.c"), "hello").expect("main");
        fs::write(dir.path().join("lib/util.c"), "hello").expect("util");

        let (summary, _) = scan_workspace(dir.path()).expect("scan");
        assert_eq!(summary.files.len(), 1);
        assert!(summary.files[0].ends_with("src/main.c"));
    }
}
