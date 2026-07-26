use super::*;

use crate::reachability::ReachGraph;

fn graph_from_store(store: &IndexStore) -> ReachGraph {
    let include = store.reach_graph_view();
    let packages = store.go_package_graph_view();
    ReachGraph::from_rows_with_packages(
        include.include_edges().expect("include edges"),
        include.unresolved_includes().expect("unresolved includes"),
        include.ambiguous_includes().expect("ambiguous includes"),
        packages.package_files().expect("package files"),
        packages.package_edges().expect("package edges"),
        packages.open_packages().expect("open packages"),
    )
}

#[test]
fn go_module_import_reaches_whole_target_package_without_file_pair_edges() {
    let dir = tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("cmd/app")).expect("cmd");
    fs::create_dir_all(dir.path().join("lib")).expect("lib");
    fs::create_dir_all(dir.path().join("other")).expect("other");
    fs::write(
        dir.path().join("go.mod"),
        "module example.com/acme\n\ngo 1.24\n",
    )
    .expect("go.mod");
    fs::write(
        dir.path().join("cmd/app/main.go"),
        "package main\nimport \"example.com/acme/lib\"\nfunc main() { lib.Open() }\n",
    )
    .expect("main.go");
    fs::write(
        dir.path().join("lib/open.go"),
        "package lib\nfunc Open() {}\n",
    )
    .expect("open.go");
    fs::write(
        dir.path().join("lib/tagged.go"),
        "//go:build tinygo\n\npackage lib\nvar Tagged = 1\n",
    )
    .expect("tagged.go");
    fs::write(
        dir.path().join("other/other.go"),
        "package other\nvar Hidden = 1\n",
    )
    .expect("other.go");
    let db = dir.path().join("index.sqlite");

    index_workspace(
        dir.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let store = IndexStore::open_readonly(&db).expect("readonly");
    let package_edges = store
        .go_package_graph_view()
        .package_edges()
        .expect("package edges");
    assert_eq!(
        package_edges.len(),
        1,
        "one import creates one package edge, not N×M file edges"
    );
    let graph = graph_from_store(&store);
    let scope = graph.reachable("cmd/app/main.go");
    assert!(scope.files.contains("cmd/app/main.go"));
    assert!(scope.files.contains("lib/open.go"));
    assert!(scope.files.contains("lib/tagged.go"));
    assert!(!scope.files.contains("other/other.go"));
    assert!(scope.open);
    assert_eq!(
        scope.reason,
        Some(crate::reachability::OpenReason::BuildConstraintUnknown)
    );
}

#[test]
fn go_vendor_import_resolves_and_cgo_is_detected_as_an_unsupported_boundary() {
    let dir = tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("cmd/app")).expect("cmd");
    fs::create_dir_all(dir.path().join("vendor/example.com/dependency/device"))
        .expect("vendor package");
    fs::write(dir.path().join("go.mod"), "module example.com/acme\n").expect("go.mod");
    fs::write(
        dir.path().join("cmd/app/main.go"),
        "package main\nimport (\n\"C\"\n\"example.com/dependency/device\"\n)\nfunc main() { device.Open() }\n",
    )
    .expect("main.go");
    fs::write(
        dir.path()
            .join("vendor/example.com/dependency/device/device.go"),
        "package device\nfunc Open() {}\n",
    )
    .expect("device.go");
    let db = dir.path().join("index.sqlite");

    index_workspace(
        dir.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let store = IndexStore::open_readonly(&db).expect("readonly");
    let graph = graph_from_store(&store);
    let scope = graph.reachable("cmd/app/main.go");
    assert!(scope
        .files
        .contains("vendor/example.com/dependency/device/device.go"));
    assert!(scope.open);
    assert_eq!(
        scope.reason,
        Some(crate::reachability::OpenReason::UnsupportedLanguageBoundary)
    );
}

#[test]
fn unresolved_go_import_opens_package_scope_without_guessing_a_target() {
    let dir = tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("cmd/app")).expect("cmd");
    fs::write(dir.path().join("go.mod"), "module example.com/acme\n").expect("go.mod");
    fs::write(
        dir.path().join("cmd/app/main.go"),
        "package main\nimport \"example.com/missing/pkg\"\nfunc main() {}\n",
    )
    .expect("main.go");
    let db = dir.path().join("index.sqlite");

    index_workspace(
        dir.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let store = IndexStore::open_readonly(&db).expect("readonly");
    let scope = graph_from_store(&store).reachable("cmd/app/main.go");
    assert!(scope.open);
    assert_eq!(
        scope.reason,
        Some(crate::reachability::OpenReason::UnresolvedInclude)
    );
}

#[test]
fn explicit_external_go_module_root_is_bounded_and_resolves_without_a_go_toolchain() {
    let dir = tempdir().expect("tempdir");
    let workspace = dir.path().join("workspace");
    let external = dir.path().join("external-device");
    fs::create_dir_all(workspace.join("cmd/app")).expect("workspace");
    fs::create_dir_all(external.join("device")).expect("external");
    fs::write(workspace.join("go.mod"), "module example.com/acme\n").expect("workspace go.mod");
    fs::write(
        workspace.join("cmd/app/main.go"),
        "package main\nimport \"example.com/external/device\"\nfunc main() { device.Open() }\n",
    )
    .expect("main.go");
    fs::write(external.join("go.mod"), "module example.com/external\n").expect("external go.mod");
    fs::write(
        external.join("device/device.go"),
        "package device\nfunc Open() {}\n",
    )
    .expect("device.go");
    fs::write(
        workspace.join("fossilsense.json"),
        serde_json::json!({
            "goModulePaths": [external.to_string_lossy()]
        })
        .to_string(),
    )
    .expect("config");
    let db = workspace.join("index.sqlite");

    index_workspace(
        &workspace,
        IndexOptions {
            db_path: Some(db.clone()),
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let store = IndexStore::open_readonly(&db).expect("readonly");
    let package_view = store.go_package_graph_view();
    let package_files = package_view.package_files().expect("package files");
    let package_edges = package_view.package_edges().expect("package edges");
    let open_packages = package_view.open_packages().expect("open packages");
    let scope = graph_from_store(&store).reachable("cmd/app/main.go");
    let external_file = crate::pathing::normalize_abs_path(&external.join("device/device.go"));
    assert!(
        scope.files.contains(&external_file),
        "external={external_file:?} scope={scope:?} package_files={package_files:?} \
         package_edges={package_edges:?} open_packages={open_packages:?}"
    );
    assert!(!scope.open);
}

#[test]
fn external_go_module_root_over_cap_contributes_no_declarations_and_reports_it() {
    let dir = tempdir().expect("tempdir");
    let workspace = dir.path().join("workspace");
    let external = dir.path().join("external");
    fs::create_dir_all(&workspace).expect("workspace");
    fs::create_dir_all(&external).expect("external");
    fs::write(
        workspace.join("main.go"),
        "package main\nfunc WorkspaceOnly() {}\n",
    )
    .expect("main.go");
    fs::write(external.join("go.mod"), "module example.com/external\n").expect("go.mod");
    for name in ["a.go", "b.go"] {
        fs::write(
            external.join(name),
            format!("package external\nfunc External{name}() {{}}\n"),
        )
        .expect("external go");
    }
    fs::write(
        workspace.join("fossilsense.json"),
        serde_json::json!({
            "goModulePaths": [external.to_string_lossy()]
        })
        .to_string(),
    )
    .expect("config");
    let db = workspace.join("index.sqlite");
    let mut messages = Vec::new();

    index_workspace(
        &workspace,
        IndexOptions {
            db_path: Some(db.clone()),
            external_max_files: Some(1),
            ..Default::default()
        },
        |status| {
            if let Some(message) = status.message {
                messages.push(message);
            }
        },
    )
    .expect("index");

    let store = IndexStore::open_readonly(&db).expect("readonly");
    assert_eq!(
        store.external_declaration_count().expect("external count"),
        0
    );
    assert!(messages.iter().any(|message| {
        message.contains("goModulePaths root exceeds cap")
            && message.contains("indexing paths only, no declarations")
    }));
}

#[test]
fn go_module_path_inside_workspace_is_not_indexed_again_as_an_external_tree() {
    let dir = tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("cmd/app")).expect("app");
    fs::create_dir_all(dir.path().join("dep")).expect("dep");
    fs::write(dir.path().join("go.mod"), "module example.com/app\n").expect("app go.mod");
    fs::write(
        dir.path().join("cmd/app/main.go"),
        "package main\nimport \"example.com/dep\"\nfunc main() { dep.Open() }\n",
    )
    .expect("main.go");
    fs::write(dir.path().join("dep/go.mod"), "module example.com/dep\n").expect("dep go.mod");
    fs::write(
        dir.path().join("dep/dep.go"),
        "package dep\nfunc Open() {}\n",
    )
    .expect("dep.go");
    fs::write(
        dir.path().join("fossilsense.json"),
        serde_json::json!({
            "goModulePaths": [dir.path().join("dep").to_string_lossy()]
        })
        .to_string(),
    )
    .expect("config");
    let db = dir.path().join("index.sqlite");

    index_workspace(
        dir.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let store = IndexStore::open_readonly(&db).expect("readonly");
    let package_files = store
        .go_package_graph_view()
        .package_files()
        .expect("package files");
    assert_eq!(
        package_files
            .iter()
            .filter(|row| row.path.ends_with("dep/dep.go"))
            .count(),
        1,
        "one physical workspace file must have one semantic package identity"
    );
    assert_eq!(
        store.external_declaration_count().expect("external count"),
        0
    );
    let scope = graph_from_store(&store).reachable("cmd/app/main.go");
    assert!(scope.files.contains("dep/dep.go"));
    assert!(!scope.open);
}

#[test]
fn go_module_path_ancestor_of_workspace_is_not_rescanned_as_external() {
    let dir = tempdir().expect("tempdir");
    let workspace = dir.path().join("workspace");
    fs::create_dir_all(workspace.join("cmd/app")).expect("app");
    fs::write(workspace.join("go.mod"), "module example.com/app\n").expect("go.mod");
    fs::write(
        workspace.join("cmd/app/main.go"),
        "package main\nfunc main() {}\n",
    )
    .expect("main.go");
    fs::write(
        workspace.join("fossilsense.json"),
        serde_json::json!({
            "goModulePaths": [dir.path().to_string_lossy()]
        })
        .to_string(),
    )
    .expect("config");
    let db = workspace.join("index.sqlite");

    index_workspace(
        &workspace,
        IndexOptions {
            db_path: Some(db.clone()),
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let store = IndexStore::open_readonly(&db).expect("readonly");
    let package_files = store
        .go_package_graph_view()
        .package_files()
        .expect("package files");
    assert_eq!(
        package_files
            .iter()
            .filter(|row| row.path.ends_with("cmd/app/main.go"))
            .count(),
        1,
        "an ancestor goModulePaths root must not create an absolute duplicate"
    );
    assert_eq!(
        store.external_declaration_count().expect("external count"),
        0
    );
}

#[test]
fn go_work_multi_module_workspace_resolves_each_nested_module() {
    let dir = tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("app")).expect("app");
    fs::create_dir_all(dir.path().join("lib")).expect("lib");
    fs::write(
        dir.path().join("go.work"),
        "go 1.24\nuse (\n./app\n./lib\n)\n",
    )
    .expect("go.work");
    fs::write(dir.path().join("app/go.mod"), "module example.com/app\n").expect("app go.mod");
    fs::write(dir.path().join("lib/go.mod"), "module example.com/lib\n").expect("lib go.mod");
    fs::write(
        dir.path().join("app/main.go"),
        "package main\nimport \"example.com/lib\"\nfunc main() { lib.Open() }\n",
    )
    .expect("main.go");
    fs::write(
        dir.path().join("lib/lib.go"),
        "package lib\nfunc Open() {}\n",
    )
    .expect("lib.go");
    let db = dir.path().join("index.sqlite");

    index_workspace(
        dir.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let store = IndexStore::open_readonly(&db).expect("readonly");
    let scope = graph_from_store(&store).reachable("app/main.go");
    assert!(scope.files.contains("lib/lib.go"));
    assert!(!scope.open);
}

#[test]
fn go_work_does_not_treat_an_unlisted_nested_module_as_a_workspace_dependency() {
    let dir = tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("app")).expect("app");
    fs::create_dir_all(dir.path().join("hidden")).expect("hidden");
    fs::write(dir.path().join("go.work"), "go 1.24\nuse ./app\n").expect("go.work");
    fs::write(dir.path().join("app/go.mod"), "module example.com/app\n").expect("app go.mod");
    fs::write(
        dir.path().join("hidden/go.mod"),
        "module example.com/hidden\n",
    )
    .expect("hidden go.mod");
    fs::write(
        dir.path().join("app/main.go"),
        "package main\nimport \"example.com/hidden\"\nfunc main() {}\n",
    )
    .expect("main.go");
    fs::write(
        dir.path().join("hidden/hidden.go"),
        "package hidden\nfunc ShouldNotBeReachable() {}\n",
    )
    .expect("hidden.go");
    let db = dir.path().join("index.sqlite");

    index_workspace(
        dir.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let store = IndexStore::open_readonly(&db).expect("readonly");
    let scope = graph_from_store(&store).reachable("app/main.go");
    assert!(!scope.files.contains("hidden/hidden.go"));
    assert!(scope.open);
    assert_eq!(
        scope.reason,
        Some(crate::reachability::OpenReason::UnresolvedInclude)
    );
}
