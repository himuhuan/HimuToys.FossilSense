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

    let walk_config = config.clone();
    let filter_root = root.clone();

    let mut files = Vec::new();
    let mut extension_counts = BTreeMap::new();

    // Walk with the same `ignore`-based semantics as the indexer and reference
    // search (respects `.gitignore` + scope config) so all three paths agree
    // on the file set.
    let walker = ignore::WalkBuilder::new(&root)
        .hidden(false)
        .parents(true)
        .git_ignore(true)
        .git_global(true)
        .filter_entry(move |entry| {
            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            let rel = relative_slash_path(&filter_root, entry.path()).unwrap_or_default();
            walk_config.keep_during_walk(&rel, is_dir)
        })
        .build();

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

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::scan_workspace;

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
        fs::write(dir.path().join("src/readme.txt"), "ignored").expect("txt");
        fs::write(dir.path().join("target/generated.c"), "ignored();").expect("generated");

        let (summary, _) = scan_workspace(dir.path()).expect("scan");

        assert_eq!(summary.files.len(), 2);
        assert!(summary
            .files
            .iter()
            .any(|file| file.ends_with("src/main.c")));
        assert!(summary
            .files
            .iter()
            .any(|file| file.ends_with("src/lib.HPP")));
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
