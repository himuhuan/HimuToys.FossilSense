use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::WorkspaceConfig;
use crate::pathing::{relative_slash_path, workspace_hash};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectContextKey {
    pub workspace_root_id: String,
    pub project_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectContext {
    pub key: ProjectContextKey,
    pub marker_files: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ProjectContextSelection {
    #[default]
    Auto,
    Manual {
        key: ProjectContextKey,
    },
    Unspecified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectContextStatus {
    pub projects: Vec<ProjectContext>,
    pub selection: ProjectContextSelection,
    pub automatic_project: Option<ProjectContextKey>,
    pub active_project: Option<ProjectContextKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProjectContextIndex {
    workspace_root_id: String,
    projects: Vec<ProjectContext>,
}

impl ProjectContextIndex {
    pub fn new(workspace_root_id: String, mut projects: Vec<ProjectContext>) -> Self {
        projects.sort_by(|a, b| a.key.project_path.cmp(&b.key.project_path));
        Self {
            workspace_root_id,
            projects,
        }
    }

    pub fn projects(&self) -> &[ProjectContext] {
        &self.projects
    }

    pub fn contains_key(&self, key: &ProjectContextKey) -> bool {
        key.workspace_root_id == self.workspace_root_id
            && self
                .projects
                .iter()
                .any(|project| project.key.project_path == key.project_path)
    }

    pub fn nearest_for_file(&self, rel_file_path: &str) -> Option<ProjectContextKey> {
        let normalized = normalize_rel_slash(rel_file_path);
        self.projects
            .iter()
            .filter(|project| path_is_at_or_under(&normalized, &project.key.project_path))
            .max_by_key(|project| project.key.project_path.len())
            .map(|project| project.key.clone())
    }
}

pub fn is_supported_marker_file_name(file_name: &str) -> bool {
    matches!(file_name, "Makefile" | "makefile" | "GNUmakefile")
        || file_name == "CMakeLists.txt"
        || has_case_insensitive_suffix(file_name, ".pro")
        || has_case_insensitive_suffix(file_name, ".sln")
        || has_case_insensitive_suffix(file_name, ".vcxproj")
        || has_case_insensitive_suffix(file_name, ".vcproj")
}

pub fn is_ninja_marker_file_name(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    lower == "build.ninja" || lower.ends_with(".ninja")
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

    let mut by_dir: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for entry in walker {
        let entry =
            entry.with_context(|| format!("failed to read entry under {}", root.display()))?;
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }
        let Some(file_name) = entry.path().file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if is_ninja_marker_file_name(file_name) || !is_supported_marker_file_name(file_name) {
            continue;
        }

        let rel_file = relative_slash_path(&root, entry.path())?;
        if !config.is_path_allowed_by_scope_without_extension(&rel_file) {
            continue;
        }
        let project_path = entry
            .path()
            .parent()
            .map(|parent| relative_slash_path(&root, parent))
            .transpose()?
            .unwrap_or_default();
        by_dir
            .entry(normalize_rel_slash(&project_path))
            .or_default()
            .push(file_name.to_string());
    }

    let workspace_root_id = workspace_hash(&root);
    let projects = by_dir
        .into_iter()
        .map(|(project_path, mut marker_files)| {
            marker_files.sort();
            marker_files.dedup();
            ProjectContext {
                key: ProjectContextKey {
                    workspace_root_id: workspace_root_id.clone(),
                    project_path,
                },
                marker_files,
            }
        })
        .collect();

    Ok(ProjectContextIndex::new(workspace_root_id, projects))
}

fn has_case_insensitive_suffix(value: &str, suffix: &str) -> bool {
    value
        .get(value.len().saturating_sub(suffix.len())..)
        .is_some_and(|tail| tail.eq_ignore_ascii_case(suffix))
}

fn normalize_rel_slash(path: &str) -> String {
    path.replace('\\', "/").trim_matches('/').to_string()
}

fn path_is_at_or_under(path: &str, ancestor: &str) -> bool {
    if ancestor.is_empty() {
        return true;
    }
    path == ancestor || path.starts_with(&format!("{ancestor}/"))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use tempfile::tempdir;

    use crate::config::WorkspaceConfig;

    use super::{
        discover_project_contexts, is_ninja_marker_file_name, is_supported_marker_file_name,
        ProjectContext, ProjectContextKey, ProjectContextSelection,
    };

    #[test]
    fn marker_matching_supports_common_files_and_excludes_ninja() {
        assert!(is_supported_marker_file_name("Makefile"));
        assert!(is_supported_marker_file_name("makefile"));
        assert!(is_supported_marker_file_name("GNUmakefile"));
        assert!(is_supported_marker_file_name("CMakeLists.txt"));
        assert!(is_supported_marker_file_name("app.pro"));
        assert!(is_supported_marker_file_name("App.SLN"));
        assert!(is_supported_marker_file_name("driver.vcxproj"));
        assert!(is_supported_marker_file_name("legacy.vcproj"));
        assert!(is_ninja_marker_file_name("build.ninja"));
        assert!(is_ninja_marker_file_name("rules.NINJA"));
        assert!(!is_supported_marker_file_name("build.ninja"));
    }

    #[test]
    fn discovery_coalesces_markers_and_ignores_ninja() {
        let dir = tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("app")).expect("app");
        fs::create_dir_all(dir.path().join("build")).expect("build");
        fs::write(dir.path().join("app/Makefile"), "").expect("makefile");
        fs::write(dir.path().join("app/CMakeLists.txt"), "").expect("cmake");
        fs::write(dir.path().join("build/build.ninja"), "").expect("ninja");

        let index =
            discover_project_contexts(dir.path(), &WorkspaceConfig::default()).expect("discover");

        assert_eq!(index.projects().len(), 1);
        assert_eq!(index.projects()[0].key.project_path, "app");
        assert_eq!(index.projects()[0].marker_files.len(), 2);
    }

    #[test]
    fn discovery_respects_scope_exclusions() {
        let dir = tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("src/app")).expect("src");
        fs::create_dir_all(dir.path().join("vendor/lib")).expect("vendor");
        fs::write(
            dir.path().join("fossilsense.json"),
            r#"{"include":["src"]}"#,
        )
        .expect("config");
        fs::write(dir.path().join("src/app/Makefile"), "").expect("makefile");
        fs::write(dir.path().join("vendor/lib/CMakeLists.txt"), "").expect("cmake");

        let (config, issue) = WorkspaceConfig::load(dir.path());
        assert!(issue.is_none());
        let index = discover_project_contexts(dir.path(), &config).expect("discover");

        assert_eq!(index.projects().len(), 1);
        assert_eq!(index.projects()[0].key.project_path, "src/app");
    }

    #[test]
    fn nearest_project_selects_nested_marker_and_falls_back_to_none() {
        let dir = tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("third_party/lib/src")).expect("tree");
        fs::create_dir_all(dir.path().join("unmarked/src")).expect("unmarked");
        fs::write(dir.path().join("third_party/Makefile"), "").expect("parent");
        fs::write(dir.path().join("third_party/lib/app.pro"), "").expect("child");

        let index =
            discover_project_contexts(dir.path(), &WorkspaceConfig::default()).expect("discover");

        assert_eq!(
            index
                .nearest_for_file("third_party/lib/src/xxx.c")
                .expect("nearest")
                .project_path,
            "third_party/lib"
        );
        assert_eq!(
            index
                .nearest_for_file("third_party/other.c")
                .expect("parent")
                .project_path,
            "third_party"
        );
        assert!(index.nearest_for_file("unmarked/src/main.c").is_none());
    }

    #[test]
    fn project_context_json_contract_uses_lsp_camel_case_fields() {
        let project = ProjectContext {
            key: ProjectContextKey {
                workspace_root_id: "root-a".to_string(),
                project_path: "app".to_string(),
            },
            marker_files: vec!["Makefile".to_string()],
        };

        assert_eq!(
            serde_json::to_value(&project).expect("serialize project"),
            json!({
                "key": {
                    "workspaceRootId": "root-a",
                    "projectPath": "app"
                },
                "markerFiles": ["Makefile"]
            })
        );

        let selection: ProjectContextSelection = serde_json::from_value(json!({
            "kind": "manual",
            "key": {
                "workspaceRootId": "root-a",
                "projectPath": "app"
            }
        }))
        .expect("deserialize manual selection");

        assert_eq!(
            selection,
            ProjectContextSelection::Manual {
                key: ProjectContextKey {
                    workspace_root_id: "root-a".to_string(),
                    project_path: "app".to_string(),
                }
            }
        );
    }
}
