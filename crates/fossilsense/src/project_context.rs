//! Best-effort build-marker project ownership.
//!
//! A project key is completion ranking evidence, not a C/C++ binding or a
//! replacement for [`crate::model::ScopeTier`]. Discovery happens while read
//! models are built; request-time lookup is entirely in memory.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::WorkspaceConfig;
use crate::pathing::{relative_slash_path, workspace_hash};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectKey {
    pub workspace_root_id: String,
    pub project_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectContext {
    pub key: ProjectKey,
    pub workspace_name: String,
    pub marker_files: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ProjectContextSelection {
    #[default]
    Auto,
    Manual {
        key: ProjectKey,
    },
    Unspecified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectContextStatus {
    pub available: bool,
    pub projects: Vec<ProjectContext>,
    pub selection: ProjectContextSelection,
    pub automatic_project: Option<ProjectKey>,
    pub active_project: Option<ProjectKey>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectContextIndex {
    workspace_root_id: String,
    workspace_name: String,
    projects: Vec<ProjectContext>,
}

impl ProjectContextIndex {
    pub fn new(
        workspace_root_id: String,
        workspace_name: String,
        mut projects: Vec<ProjectContext>,
    ) -> Self {
        projects.sort_by(|a, b| {
            a.key
                .project_path
                .to_ascii_lowercase()
                .cmp(&b.key.project_path.to_ascii_lowercase())
                .then_with(|| a.key.project_path.cmp(&b.key.project_path))
        });
        Self {
            workspace_root_id,
            workspace_name,
            projects,
        }
    }

    pub fn projects(&self) -> &[ProjectContext] {
        &self.projects
    }

    pub fn canonical_key(&self, key: &ProjectKey) -> Option<ProjectKey> {
        if key.workspace_root_id != self.workspace_root_id {
            return None;
        }
        self.projects
            .iter()
            .find(|project| {
                project
                    .key
                    .project_path
                    .eq_ignore_ascii_case(&key.project_path)
            })
            .map(|project| project.key.clone())
    }

    pub fn nearest_for_file(&self, rel_file_path: &str) -> Option<ProjectKey> {
        let normalized = normalize_rel_slash(rel_file_path);
        self.projects
            .iter()
            .filter(|project| path_is_at_or_under(&normalized, &project.key.project_path))
            .max_by_key(|project| path_depth(&project.key.project_path))
            .map(|project| project.key.clone())
    }
}

pub fn is_supported_marker_file_name(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "makefile"
            | "gnumakefile"
            | "cmakelists.txt"
            | "build.ninja"
            | "meson.build"
            | "build"
            | "build.bazel"
            | "workspace"
            | "workspace.bazel"
    ) || [".pro", ".sln", ".vcxproj", ".vcproj"]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
}

pub fn discover_project_contexts(
    workspace_root: impl AsRef<Path>,
    config: &WorkspaceConfig,
) -> Result<ProjectContextIndex> {
    let root = workspace_root.as_ref();
    let root = root
        .canonicalize()
        .with_context(|| format!("failed to canonicalize workspace root {}", root.display()))?;
    let filter_root = root.clone();
    let walk_config = config.clone();

    let walker = ignore::WalkBuilder::new(&root)
        .hidden(false)
        .parents(true)
        .git_ignore(true)
        .git_global(true)
        .filter_entry(move |entry| {
            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            if !is_dir {
                return true;
            }
            let rel = relative_slash_path(&filter_root, entry.path()).unwrap_or_default();
            walk_config.keep_during_walk(&rel, true)
        })
        .build();

    // The lowercase directory is the Windows-compatible coalescing key; the
    // first filesystem spelling is retained for user-visible relative paths.
    let mut by_dir: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    for entry in walker {
        let entry =
            entry.with_context(|| format!("failed to read entry under {}", root.display()))?;
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }
        let Some(file_name) = entry.path().file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !is_supported_marker_file_name(file_name) {
            continue;
        }

        let rel_file = relative_slash_path(&root, entry.path())?;
        if !config.is_project_marker_in_scope(&rel_file) {
            continue;
        }
        let project_path = entry
            .path()
            .parent()
            .map(|parent| relative_slash_path(&root, parent))
            .transpose()?
            .unwrap_or_default();
        let project_path = normalize_rel_slash(&project_path);
        by_dir
            .entry(project_path.to_ascii_lowercase())
            .or_insert_with(|| (project_path, Vec::new()))
            .1
            .push(file_name.to_string());
    }

    let workspace_root_id = workspace_hash(&root);
    let workspace_name = root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("workspace")
        .to_string();
    let projects = by_dir
        .into_values()
        .map(|(project_path, mut marker_files)| {
            marker_files.sort_by_key(|name| name.to_ascii_lowercase());
            marker_files.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
            ProjectContext {
                key: ProjectKey {
                    workspace_root_id: workspace_root_id.clone(),
                    project_path,
                },
                workspace_name: workspace_name.clone(),
                marker_files,
            }
        })
        .collect();

    Ok(ProjectContextIndex::new(
        workspace_root_id,
        workspace_name,
        projects,
    ))
}

fn normalize_rel_slash(path: &str) -> String {
    path.replace('\\', "/").trim_matches('/').to_string()
}

fn path_depth(path: &str) -> usize {
    if path.is_empty() {
        0
    } else {
        path.split('/').count()
    }
}

fn path_is_at_or_under(path: &str, ancestor: &str) -> bool {
    if ancestor.is_empty() {
        return true;
    }
    if path.eq_ignore_ascii_case(ancestor) {
        return true;
    }
    path.get(..ancestor.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(ancestor))
        && path.as_bytes().get(ancestor.len()) == Some(&b'/')
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn marker_policy_covers_main_build_files_and_rejects_fragments() {
        for marker in [
            "Makefile",
            "MAKEFILE",
            "GNUmakefile",
            "CMakeLists.txt",
            "app.pro",
            "build.ninja",
            "App.SLN",
            "driver.vcxproj",
            "legacy.vcproj",
            "meson.build",
            "BUILD",
            "BUILD.bazel",
            "WORKSPACE",
            "WORKSPACE.bazel",
        ] {
            assert!(is_supported_marker_file_name(marker), "{marker}");
        }
        for fragment in [
            "rules.mk",
            "shared.pri",
            "rules.ninja",
            "compile_commands.json",
            "CMakeCache.txt",
        ] {
            assert!(!is_supported_marker_file_name(fragment), "{fragment}");
        }
    }

    #[test]
    fn discovery_coalesces_markers_and_respects_default_excludes() {
        let dir = tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("app")).expect("app");
        fs::create_dir_all(dir.path().join("build")).expect("build");
        fs::write(dir.path().join("app/Makefile"), "").expect("make");
        fs::write(dir.path().join("app/CMakeLists.txt"), "").expect("cmake");
        fs::write(dir.path().join("build/build.ninja"), "").expect("ninja");

        let index =
            discover_project_contexts(dir.path(), &WorkspaceConfig::default()).expect("discover");
        assert_eq!(index.projects().len(), 1);
        assert_eq!(index.projects()[0].key.project_path, "app");
        assert_eq!(index.projects()[0].marker_files.len(), 2);
    }

    #[test]
    fn discovery_respects_configured_include_scope() {
        let dir = tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("src/app")).expect("src");
        fs::create_dir_all(dir.path().join("vendor/lib")).expect("vendor");
        fs::write(
            dir.path().join("fossilsense.json"),
            r#"{"include":["src"]}"#,
        )
        .expect("config");
        fs::write(dir.path().join("src/app/Makefile"), "").expect("make");
        fs::write(dir.path().join("vendor/lib/CMakeLists.txt"), "").expect("cmake");
        let (config, issue) = WorkspaceConfig::load(dir.path());
        assert!(issue.is_none());

        let index = discover_project_contexts(dir.path(), &config).expect("discover");
        assert_eq!(index.projects().len(), 1);
        assert_eq!(index.projects()[0].key.project_path, "src/app");
    }

    #[test]
    fn discovery_respects_gitignore_and_explicit_exclude() {
        let dir = tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join(".git")).expect("git dir");
        fs::create_dir_all(dir.path().join("ignored/lib")).expect("ignored");
        fs::create_dir_all(dir.path().join("vendor/lib")).expect("vendor");
        fs::write(dir.path().join(".gitignore"), "ignored/\n").expect("gitignore");
        fs::write(
            dir.path().join("fossilsense.json"),
            r#"{"exclude":["vendor"]}"#,
        )
        .expect("config");
        fs::write(dir.path().join("ignored/lib/Makefile"), "").expect("ignored marker");
        fs::write(dir.path().join("vendor/lib/CMakeLists.txt"), "").expect("excluded marker");
        let (config, issue) = WorkspaceConfig::load(dir.path());
        assert!(issue.is_none());

        let index = discover_project_contexts(dir.path(), &config).expect("discover");
        assert!(index.projects().is_empty());
    }

    #[test]
    fn discovery_failure_is_reported_instead_of_fabricating_an_empty_model() {
        let dir = tempdir().expect("tempdir");
        let missing = dir.path().join("missing");
        assert!(discover_project_contexts(missing, &WorkspaceConfig::default()).is_err());
    }

    #[test]
    fn root_marker_can_own_an_included_source_subtree_without_admitting_siblings() {
        let dir = tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("src/app")).expect("src");
        fs::create_dir_all(dir.path().join("vendor/lib")).expect("vendor");
        fs::write(
            dir.path().join("fossilsense.json"),
            r#"{"include":["src"]}"#,
        )
        .expect("config");
        fs::write(dir.path().join("CMakeLists.txt"), "").expect("root cmake");
        fs::write(dir.path().join("vendor/lib/Makefile"), "").expect("vendor make");
        let (config, issue) = WorkspaceConfig::load(dir.path());
        assert!(issue.is_none());

        let index = discover_project_contexts(dir.path(), &config).expect("discover");
        assert_eq!(index.projects().len(), 1);
        assert_eq!(index.projects()[0].key.project_path, "");
        assert_eq!(
            index
                .nearest_for_file("src/app/main.c")
                .expect("root project")
                .project_path,
            ""
        );
    }

    #[test]
    fn nearest_project_handles_root_nested_case_and_path_boundaries() {
        let root_id = "root".to_string();
        let context = |path: &str| ProjectContext {
            key: ProjectKey {
                workspace_root_id: root_id.clone(),
                project_path: path.to_string(),
            },
            workspace_name: "ws".to_string(),
            marker_files: vec!["Makefile".to_string()],
        };
        let index = ProjectContextIndex::new(
            root_id.clone(),
            "ws".to_string(),
            vec![
                context(""),
                context("Third_Party"),
                context("third_party/lib"),
            ],
        );

        assert_eq!(
            index
                .nearest_for_file("THIRD_PARTY/lib/src/x.c")
                .expect("nested")
                .project_path,
            "third_party/lib"
        );
        assert_eq!(
            index
                .nearest_for_file("third_party2/x.c")
                .expect("root")
                .project_path,
            ""
        );
    }

    #[test]
    fn no_marker_returns_none_and_distinct_roots_have_distinct_keys() {
        let empty = ProjectContextIndex::new("a".into(), "a".into(), Vec::new());
        assert!(empty.nearest_for_file("src/main.c").is_none());
        let left = ProjectKey {
            workspace_root_id: "left".into(),
            project_path: "src/app".into(),
        };
        let right = ProjectKey {
            workspace_root_id: "right".into(),
            project_path: "src/app".into(),
        };
        assert_ne!(left, right);
    }

    #[test]
    fn validated_key_uses_discovered_path_casing() {
        let canonical = ProjectKey {
            workspace_root_id: "root".into(),
            project_path: "Src/Server".into(),
        };
        let index = ProjectContextIndex::new(
            "root".into(),
            "workspace".into(),
            vec![ProjectContext {
                key: canonical.clone(),
                workspace_name: "workspace".into(),
                marker_files: vec!["Makefile".into()],
            }],
        );
        let stored = ProjectKey {
            workspace_root_id: "root".into(),
            project_path: "src/server".into(),
        };

        assert_eq!(index.canonical_key(&stored), Some(canonical));
    }

    #[test]
    fn dto_json_contract_is_camel_case_and_tagged() {
        let key = ProjectKey {
            workspace_root_id: "root-a".into(),
            project_path: "app".into(),
        };
        assert_eq!(
            serde_json::to_value(ProjectContextSelection::Manual { key: key.clone() })
                .expect("selection"),
            json!({
                "kind": "manual",
                "key": {"workspaceRootId": "root-a", "projectPath": "app"}
            })
        );
        let status = ProjectContextStatus {
            available: true,
            projects: Vec::new(),
            selection: ProjectContextSelection::Auto,
            automatic_project: Some(key.clone()),
            active_project: Some(key),
        };
        let value = serde_json::to_value(status).expect("status");
        assert_eq!(value["available"], true);
        assert!(value.get("automaticProject").is_some());
        assert!(value.get("activeProject").is_some());
    }
}
