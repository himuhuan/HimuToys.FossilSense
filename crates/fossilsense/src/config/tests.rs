use std::fs;

use tempfile::tempdir;

use super::*;

// ---- normalization helpers ----

#[test]
fn normalize_entry_handles_backslashes() {
    assert_eq!(normalize_entry("src\\main".into()), "src/main");
}

#[test]
fn normalize_entry_strips_leading_dot_slash() {
    assert_eq!(normalize_entry("./src".into()), "src");
}

#[test]
fn normalize_entry_strips_surrounding_slashes() {
    assert_eq!(normalize_entry("/src/".into()), "src");
}

#[test]
fn normalize_extension_entry_strips_dot_and_lowercases() {
    assert_eq!(normalize_extension_entry(".C".into()), "c");
    assert_eq!(normalize_extension_entry("HPP".into()), "hpp");
}

// ---- path_matches_entry ----

#[test]
fn path_matches_entry_exact() {
    assert!(path_matches_entry("src", "src"));
    assert!(!path_matches_entry("src", "src_gen"));
}

#[test]
fn path_matches_entry_prefix_boundary() {
    assert!(path_matches_entry("src/main.c", "src"));
    assert!(!path_matches_entry("src_gen/b.c", "src"));
}

#[test]
fn path_matches_entry_case_insensitive() {
    assert!(path_matches_entry("SRC/main.c", "src"));
    assert!(path_matches_entry("src/Main.C", "SRC"));
}

#[test]
fn path_matches_glob_entry_wildcards() {
    assert!(path_matches_glob_entry("src/main.c", "src/*.c"));
    assert!(path_matches_glob_entry("src/a1.c", "src/a?.c"));
    assert!(path_matches_glob_entry("src/a.c", "src/[ab].c"));
    assert!(!path_matches_glob_entry("src/c.c", "src/[ab].c"));
}

#[test]
fn source_language_defaults_make_headers_and_inl_cpp() {
    assert_eq!(
        SourceLanguage::default_for_path(Path::new("include/widget.h")),
        SourceLanguage::Cpp
    );
    assert_eq!(
        SourceLanguage::default_for_path(Path::new("include/widget.inl")),
        SourceLanguage::Cpp
    );
    assert_eq!(
        SourceLanguage::default_for_path(Path::new("src/widget.c")),
        SourceLanguage::C
    );
    assert_eq!(
        SourceLanguage::default_for_path(Path::new("src/widget.go")),
        SourceLanguage::Go
    );
    assert_eq!(
        SourceLanguage::default_for_path(Path::new("generated/widget.custom")),
        SourceLanguage::C
    );
    assert!(WorkspaceConfig::default()
        .extensions
        .iter()
        .any(|extension| extension == "go"));
}

#[test]
fn built_in_language_registry_declares_go_as_a_separate_semantic_family() {
    let backends = super::built_in_language_backends();
    assert_eq!(backends.len(), 3);
    assert_eq!(backends[0].language, SourceLanguage::C);
    assert_eq!(backends[1].language, SourceLanguage::Cpp);
    assert_eq!(backends[2].language, SourceLanguage::Go);
    assert_eq!(
        SourceLanguage::C.semantic_family(),
        super::SemanticFamily::CFamily
    );
    assert_eq!(
        SourceLanguage::Cpp.semantic_family(),
        super::SemanticFamily::CFamily
    );
    assert_eq!(
        SourceLanguage::Go.semantic_family(),
        super::SemanticFamily::Go
    );
    assert_eq!(
        SourceLanguage::from_config_name("go"),
        Some(SourceLanguage::Go)
    );
    assert!(backends[2].default_extensions.contains(&"go"));
    assert_eq!(
        backends[2].tree_sitter_language(),
        tree_sitter_go::LANGUAGE.into()
    );
    let registered_extensions: std::collections::HashSet<_> = backends
        .iter()
        .flat_map(|backend| backend.default_extensions.iter().copied())
        .collect();
    let configured_extensions: std::collections::HashSet<_> =
        WorkspaceConfig::default().extensions.into_iter().collect();
    assert_eq!(
        registered_extensions,
        configured_extensions
            .iter()
            .map(String::as_str)
            .collect::<std::collections::HashSet<_>>()
    );
}

#[test]
fn language_overrides_use_relative_globs_and_last_match_wins() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    fs::write(
        root.join("fossilsense.json"),
        r#"{
          "languageOverrides": [
            { "glob": "legacy-c/**/*.h", "language": "c" },
            { "glob": "legacy-c/special/*.h", "language": "cpp" },
            { "glob": "legacy-c/special/final.h", "language": "c" },
            { "glob": "generated/**/*.inc", "language": "go" }
          ]
        }"#,
    )
    .expect("config");
    let (config, issue) = WorkspaceConfig::load(root);
    assert!(issue.is_none());
    let resolver = LanguageResolver::from_workspace_config(root, &config);

    assert_eq!(
        resolver.language_for_path(&root.join("legacy-c/direct.h")),
        SourceLanguage::C,
        "** must match zero intermediate directories"
    );
    assert_eq!(
        resolver.language_for_path(&root.join("legacy-c/special/other.h")),
        SourceLanguage::Cpp
    );
    assert_eq!(
        resolver.language_for_path(&root.join("legacy-c/special/final.h")),
        SourceLanguage::C,
        "the final matching rule must win"
    );
    assert_eq!(
        resolver.language_for_path(&root.join("generated/runtime.inc")),
        SourceLanguage::Go
    );
}

#[test]
fn language_override_external_paths_match_normalized_absolute_globs() {
    let dir = tempdir().expect("tempdir");
    let workspace = dir.path().join("workspace");
    let external = dir.path().join("sdk/include/api.h");
    fs::create_dir_all(&workspace).expect("workspace");
    let absolute_glob = normalize_language_match_path(&dir.path().join("sdk/**/*.h"));
    let resolver = LanguageResolver::new(
        Some(&workspace),
        vec![LanguageOverride {
            glob: absolute_glob,
            language: SourceLanguage::C,
        }],
    );
    assert_eq!(resolver.language_for_path(&external), SourceLanguage::C);
}

#[test]
fn malformed_language_rules_do_not_discard_other_config_fields() {
    let dir = tempdir().expect("tempdir");
    fs::write(
        dir.path().join("fossilsense.json"),
        r#"{
          "include": ["src"],
          "extensions": ["c", "h", "custom"],
          "languageOverrides": [
            { "glob": "legacy/**/*.h", "language": "rust" },
            { "glob": 42, "language": "c" },
            { "glob": "generated/**/*.h", "language": "cpp" }
          ]
        }"#,
    )
    .expect("config");
    let (config, issue) = WorkspaceConfig::load(dir.path());
    let issue = issue.expect("invalid rules must be reported");
    assert!(issue.message.contains("invalid language"));
    assert!(issue.message.contains("malformed"));
    assert_eq!(config.include, vec!["src"]);
    assert_eq!(config.extensions, vec!["c", "h", "custom"]);
    assert_eq!(config.language_overrides.len(), 1);
    assert_eq!(config.language_overrides[0].language, SourceLanguage::Cpp);
}

// ---- is_in_scope ----

#[test]
fn is_in_scope_empty_include_is_full_repo() {
    let config = WorkspaceConfig::default();
    assert!(config.is_in_scope("src/main.c"));
    assert!(config.is_in_scope("third_party/lib.cpp"));
}

#[test]
fn is_in_scope_include_limits_to_subtree() {
    let mut config = WorkspaceConfig {
        include: vec!["src/".into(), "include/".into()]
            .into_iter()
            .map(normalize_entry)
            .collect(),
        ..WorkspaceConfig::default()
    };
    config.rebuild_matchers();
    assert!(config.is_in_scope("src/main.c"));
    assert!(config.is_in_scope("include/header.h"));
    assert!(!config.is_in_scope("third_party/foo.c"));
}

#[test]
fn is_in_scope_boundary_no_false_match() {
    let mut config = WorkspaceConfig {
        include: vec!["src".into()]
            .into_iter()
            .map(normalize_entry)
            .collect(),
        ..WorkspaceConfig::default()
    };
    config.rebuild_matchers();
    assert!(config.is_in_scope("src/a.c"));
    assert!(!config.is_in_scope("src_gen/b.c"));
}

#[test]
fn is_in_scope_exclude_inside_include() {
    let mut config = WorkspaceConfig {
        include: vec!["src/".into()]
            .into_iter()
            .map(normalize_entry)
            .collect(),
        exclude: vec!["src/generated/".into()]
            .into_iter()
            .map(normalize_entry)
            .collect(),
        ..WorkspaceConfig::default()
    };
    config.rebuild_matchers();
    assert!(config.is_in_scope("src/main.c"));
    assert!(!config.is_in_scope("src/generated/auto.c"));
}

#[test]
fn is_in_scope_glob_include_and_exclude_do_not_cross_match() {
    let mut config = WorkspaceConfig {
        include: vec!["src/*".into()]
            .into_iter()
            .map(normalize_entry)
            .collect(),
        exclude: vec!["build/*".into()]
            .into_iter()
            .map(normalize_entry)
            .collect(),
        ..WorkspaceConfig::default()
    };
    config.rebuild_matchers();
    assert!(config.is_in_scope("src/main.c"));
    assert!(!config.is_in_scope("build/main.c"));
}

#[test]
fn is_in_scope_glob_include_matches_wildcard_pattern() {
    let mut config = WorkspaceConfig {
        include: vec!["src/*.c".into()]
            .into_iter()
            .map(normalize_entry)
            .collect(),
        ..WorkspaceConfig::default()
    };
    config.rebuild_matchers();
    assert!(config.is_in_scope("src/main.c"));
    assert!(!config.is_in_scope("src/main.h"));
}

#[test]
fn is_in_scope_extension_normalization() {
    let mut config = WorkspaceConfig {
        extensions: vec!["C".into(), ".H".into()]
            .into_iter()
            .map(normalize_extension_entry)
            .collect(),
        ..WorkspaceConfig::default()
    };
    config.rebuild_matchers();
    assert!(config.is_in_scope("src/a.c"));
    assert!(config.is_in_scope("inc/b.h"));
    assert!(!config.is_in_scope("src/a.cpp"));
}

// ---- keep_during_walk (traversal-layer pruning) ----

#[test]
fn keep_during_walk_root_always_kept() {
    let mut config = WorkspaceConfig {
        // Even if the root basename would match an excluded dir, the empty
        // relative path (the root itself) must never be pruned.
        exclude: vec!["build".into()]
            .into_iter()
            .map(normalize_entry)
            .collect(),
        ..WorkspaceConfig::default()
    };
    config.rebuild_matchers();
    assert!(config.keep_during_walk("", true));
}

#[test]
fn keep_during_walk_prunes_default_excluded_dirs() {
    let config = WorkspaceConfig::default();
    assert!(!config.keep_during_walk("target", true));
    assert!(config.keep_during_walk("src", true));
}

#[test]
fn keep_during_walk_prunes_excluded_subtree() {
    let mut config = WorkspaceConfig {
        exclude: vec!["third_party/".into()]
            .into_iter()
            .map(normalize_entry)
            .collect(),
        ..WorkspaceConfig::default()
    };
    config.rebuild_matchers();
    assert!(!config.keep_during_walk("third_party", true));
    assert!(!config.keep_during_walk("third_party/vendor", true));
    assert!(config.keep_during_walk("src", true));
}

#[test]
fn keep_during_walk_prunes_dirs_outside_include() {
    let mut config = WorkspaceConfig {
        include: vec!["src/core".into(), "include".into()]
            .into_iter()
            .map(normalize_entry)
            .collect(),
        ..WorkspaceConfig::default()
    };
    config.rebuild_matchers();
    assert!(config.keep_during_walk("src", true));
    assert!(config.keep_during_walk("src/core", true));
    assert!(config.keep_during_walk("include", true));
    assert!(!config.keep_during_walk("src_gen", true));
    assert!(!config.keep_during_walk("third_party", true));
}

#[test]
fn keep_during_walk_keeps_dirs_inside_include() {
    let mut config = WorkspaceConfig {
        include: vec!["src".into()]
            .into_iter()
            .map(normalize_entry)
            .collect(),
        ..WorkspaceConfig::default()
    };
    config.rebuild_matchers();
    assert!(config.keep_during_walk("src", true));
    assert!(config.keep_during_walk("src/nested", true));
}

#[test]
fn keep_during_walk_files_checked_by_extension() {
    let config = WorkspaceConfig::default();
    assert!(config.keep_during_walk("src/a.c", false));
    assert!(!config.keep_during_walk("src/readme.txt", false));
}

#[test]
fn keep_during_walk_keeps_dirs_when_include_glob_present() {
    let mut config = WorkspaceConfig {
        include: vec!["src/*.c".into()]
            .into_iter()
            .map(normalize_entry)
            .collect(),
        ..WorkspaceConfig::default()
    };
    config.rebuild_matchers();
    assert!(config.keep_during_walk("src", true));
    assert!(config.keep_during_walk("third_party", true));
    assert!(config.is_in_scope("src/main.c"));
    assert!(!config.is_in_scope("third_party/main.c"));
}

#[test]
fn is_in_scope_custom_extensions_replace_not_merge() {
    let mut config = WorkspaceConfig {
        extensions: vec!["c".into(), "h".into()],
        ..WorkspaceConfig::default()
    };
    config.rebuild_matchers();
    assert!(config.is_in_scope("a.c"));
    assert!(config.is_in_scope("b.h"));
    assert!(!config.is_in_scope("c.cpp"));
}

// ---- load ----

#[test]
fn load_missing_file_returns_default() {
    let dir = tempdir().expect("tempdir");
    let (config, issue) = WorkspaceConfig::load(dir.path());
    assert_eq!(config, WorkspaceConfig::default());
    assert!(issue.is_none());
}

#[test]
fn load_partial_fields() {
    let dir = tempdir().expect("tempdir");
    fs::write(
        dir.path().join("fossilsense.json"),
        r#"{"exclude": ["build/"]}"#,
    )
    .expect("write");
    let (config, issue) = WorkspaceConfig::load(dir.path());
    assert!(issue.is_none());
    assert!(config.include.is_empty()); // default: all repo
    assert_eq!(config.exclude, vec!["build"]);
    assert!(!config.is_in_scope("build/out.c"));
}

#[test]
fn load_broken_json_returns_default_with_issue() {
    let dir = tempdir().expect("tempdir");
    fs::write(dir.path().join("fossilsense.json"), "not json").expect("write");
    let (config, issue) = WorkspaceConfig::load(dir.path());
    assert_eq!(config, WorkspaceConfig::default());
    assert!(issue.is_some());
    assert!(issue.unwrap().message.contains("failed to parse"));
}

#[test]
fn load_invalid_field_type_returns_default_with_issue() {
    let dir = tempdir().expect("tempdir");
    fs::write(
        dir.path().join("fossilsense.json"),
        r#"{"include": "not-an-array"}"#,
    )
    .expect("write");
    let (config, issue) = WorkspaceConfig::load(dir.path());
    assert_eq!(config, WorkspaceConfig::default());
    assert!(issue.is_some());
}

// ---- include paths (external header reference directories) ----

#[test]
fn normalize_include_path_keeps_absolute_and_switches_separators() {
    assert_eq!(
        normalize_include_path_entry("C:\\TDM-GCC-64\\include\\".into()),
        "C:/TDM-GCC-64/include"
    );
    assert_eq!(
        normalize_include_path_entry("/usr/include/".into()),
        "/usr/include"
    );
}

#[test]
fn load_parses_and_dedupes_include_paths() {
    let dir = tempdir().expect("tempdir");
    fs::write(
        dir.path().join("fossilsense.json"),
        r#"{"includePaths": ["C:\\a\\inc", "C:/a/inc/", "C:/b/inc"]}"#,
    )
    .expect("write");
    let (config, issue) = WorkspaceConfig::load(dir.path());
    assert!(issue.is_some());
    // The first two normalize to the same path and collapse to one entry.
    assert_eq!(config.include_paths, vec!["C:/a/inc", "C:/b/inc"]);
}

#[test]
fn load_default_has_empty_include_paths() {
    let dir = tempdir().expect("tempdir");
    let (config, _) = WorkspaceConfig::load(dir.path());
    assert!(config.include_paths.is_empty());
}

#[test]
fn non_array_include_paths_falls_back_to_empty_with_issue() {
    let dir = tempdir().expect("tempdir");
    fs::write(
        dir.path().join("fossilsense.json"),
        r#"{"includePaths": "C:/not/an/array"}"#,
    )
    .expect("write");
    let (config, issue) = WorkspaceConfig::load(dir.path());
    assert!(config.include_paths.is_empty());
    assert!(issue.is_some());
}

#[test]
fn resolve_include_roots_skips_missing_without_error() {
    let dir = tempdir().expect("tempdir");
    let existing = dir.path().join("inc");
    fs::create_dir_all(&existing).expect("inc");
    let existing_norm = existing.to_string_lossy().replace('\\', "/");
    let missing = format!(
        "{}/does-not-exist",
        dir.path().to_string_lossy().replace('\\', "/")
    );

    let (roots, issues) = resolve_include_roots(&[existing_norm.clone(), missing.clone()]);
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0], PathBuf::from(&existing_norm));
    assert_eq!(issues.len(), 1);
    assert!(issues[0].message.contains("not found"));
}

#[test]
fn resolve_include_roots_flags_non_directory() {
    let dir = tempdir().expect("tempdir");
    let file = dir.path().join("header.h");
    fs::write(&file, "x").expect("file");
    let file_norm = file.to_string_lossy().replace('\\', "/");

    let (roots, issues) = resolve_include_roots(&[file_norm]);
    assert!(roots.is_empty());
    assert_eq!(issues.len(), 1);
    assert!(issues[0].message.contains("not a directory"));
}

#[test]
fn resolve_include_roots_flags_relative_and_duplicate_entries() {
    let dir = tempdir().expect("tempdir");
    let existing = dir.path().join("inc");
    fs::create_dir_all(&existing).expect("inc");
    let existing_norm = existing.to_string_lossy().replace('\\', "/");

    let (roots, issues) = resolve_include_roots(&[
        existing_norm.clone(),
        existing_norm.clone(),
        "relative/include".to_string(),
    ]);
    assert_eq!(roots, vec![PathBuf::from(existing_norm)]);
    assert_eq!(issues.len(), 2);
    assert!(issues
        .iter()
        .any(|issue| issue.message.contains("duplicate")));
    assert!(issues
        .iter()
        .any(|issue| issue.message.contains("not absolute")));
}
