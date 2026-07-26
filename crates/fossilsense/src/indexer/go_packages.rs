use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::store::{IndexBuild, IndexStore};

#[derive(Debug, Default)]
pub(super) struct GoPackageGraphUpdate {
    pub edges: Vec<(String, String, String)>,
    pub open_packages: Vec<(String, String)>,
    pub importable_packages: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
struct PackageNode {
    key: String,
    name: String,
    path: String,
    source: String,
}

pub(super) fn build_go_package_graph(
    store: &IndexStore,
    build: IndexBuild,
    workspace: &Path,
    external_module_roots: &[PathBuf],
) -> Result<GoPackageGraphUpdate> {
    let packages = store.effective_go_packages(build)?;
    let imports = store.effective_go_imports(build)?;
    let mut nodes_by_key: HashMap<String, PackageNode> = HashMap::new();
    let mut guarded_packages = HashSet::new();
    for package in packages {
        if package
            .build_guard
            .as_deref()
            .is_some_and(|guard| !guard.trim().is_empty())
        {
            guarded_packages.insert(package.package_key.clone());
        }
        nodes_by_key
            .entry(package.package_key.clone())
            .or_insert(PackageNode {
                key: package.package_key,
                name: package.package_name,
                path: package.path,
                source: package.source,
            });
    }

    let canonical_workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let canonical_external_roots: Vec<_> = external_module_roots
        .iter()
        .map(|root| PathBuf::from(crate::pathing::normalize_abs_path(root)))
        .collect();
    let go_work_modules = read_go_work_module_dirs(&canonical_workspace);
    let mut module_cache: HashMap<PathBuf, Option<(PathBuf, String)>> = HashMap::new();
    let mut import_targets: HashMap<String, Vec<String>> = HashMap::new();
    let mut importable_packages = Vec::new();
    for node in nodes_by_key.values() {
        if node.name == "main" || (node.name.ends_with("_test") && node.path.ends_with("_test.go"))
        {
            continue;
        }
        let Some(import_path) = package_import_path(
            node,
            &canonical_workspace,
            &canonical_external_roots,
            go_work_modules.as_ref(),
            &mut module_cache,
        ) else {
            continue;
        };
        import_targets
            .entry(import_path.clone())
            .or_default()
            .push(node.key.clone());
        importable_packages.push((node.key.clone(), import_path));
    }
    for targets in import_targets.values_mut() {
        targets.sort();
        targets.dedup();
    }

    let known_packages: HashSet<_> = nodes_by_key.keys().cloned().collect();
    let mut edges: HashMap<(String, String), bool> = HashMap::new();
    let mut open: HashMap<String, &'static str> = HashMap::new();
    for package in guarded_packages {
        retain_open_reason(&mut open, package, "build_constraint_unknown");
    }
    let mut seen_imports = HashSet::new();
    for import in imports {
        if !known_packages.contains(&import.source_package_key)
            || !seen_imports.insert((
                import.source_package_key.clone(),
                import.import_path.clone(),
            ))
        {
            continue;
        }
        if import.import_path == "C" {
            retain_open_reason(
                &mut open,
                import.source_package_key,
                "unsupported_language_boundary",
            );
            continue;
        }
        match import_targets.get(&import.import_path).map(Vec::as_slice) {
            Some([target]) => {
                if target != &import.source_package_key {
                    edges.insert((import.source_package_key, target.clone()), false);
                }
            }
            Some(targets) if !targets.is_empty() => {
                retain_open_reason(
                    &mut open,
                    import.source_package_key.clone(),
                    "ambiguous_import",
                );
                for target in targets {
                    if target != &import.source_package_key {
                        edges
                            .entry((import.source_package_key.clone(), target.clone()))
                            .or_insert(true);
                    }
                }
            }
            _ => {
                retain_open_reason(&mut open, import.source_package_key, "unresolved_import");
            }
        }
    }

    let mut edges: Vec<_> = edges
        .into_iter()
        .map(|((source, target), heuristic)| {
            (
                source,
                target,
                if heuristic { "heuristic" } else { "exact" }.to_string(),
            )
        })
        .collect();
    edges.sort();
    let mut open_packages: Vec<_> = open
        .into_iter()
        .map(|(package, reason)| (package, reason.to_string()))
        .collect();
    open_packages.sort();
    importable_packages.sort();
    importable_packages.dedup();
    Ok(GoPackageGraphUpdate {
        edges,
        open_packages,
        importable_packages,
    })
}

fn package_import_path(
    node: &PackageNode,
    workspace: &Path,
    external_roots: &[PathBuf],
    go_work_modules: Option<&HashSet<PathBuf>>,
    module_cache: &mut HashMap<PathBuf, Option<(PathBuf, String)>>,
) -> Option<String> {
    let source_path = if node.source == "workspace" {
        workspace.join(node.path.replace('/', std::path::MAIN_SEPARATOR_STR))
    } else {
        PathBuf::from(&node.path)
    };
    let directory = source_path.parent()?.to_path_buf();
    let boundary = if node.source == "workspace" {
        Some(workspace)
    } else {
        external_roots
            .iter()
            .filter(|root| directory.starts_with(root))
            .max_by_key(|root| root.components().count())
            .map(PathBuf::as_path)
    }?;
    let vendor_path = if node.source == "workspace" {
        node.path.clone()
    } else {
        source_path
            .strip_prefix(boundary)
            .ok()?
            .to_string_lossy()
            .replace('\\', "/")
    };
    if let Some(vendor_suffix) = vendor_import_suffix(&vendor_path) {
        return Some(vendor_suffix);
    }
    let (module_dir, module_path) = nearest_module(&directory, boundary, module_cache)?;
    if node.source == "workspace"
        && go_work_modules.is_some_and(|modules| !modules.contains(&module_dir))
    {
        return None;
    }
    let relative = directory.strip_prefix(&module_dir).ok()?;
    let relative = relative.to_string_lossy().replace('\\', "/");
    if relative.is_empty() {
        Some(module_path)
    } else {
        Some(format!(
            "{}/{}",
            module_path.trim_end_matches('/'),
            relative
        ))
    }
}

fn vendor_import_suffix(path: &str) -> Option<String> {
    let components: Vec<_> = path.split('/').collect();
    let directories = components.get(..components.len().checked_sub(1)?)?;
    let boundary = directories
        .iter()
        .enumerate()
        .filter_map(|(index, component)| {
            (*component == "vendor" && index + 1 < directories.len()).then_some(index)
        })
        .next_back()?;
    let package_components = directories.get(boundary + 1..)?;
    (!package_components.is_empty()).then(|| package_components.join("/"))
}

#[cfg(test)]
mod vendor_import_suffix_tests {
    use super::vendor_import_suffix;

    #[test]
    fn uses_the_last_vendor_that_has_a_package_directory_after_it() {
        assert_eq!(
            vendor_import_suffix("vendor/outer/vendor/example.com/nested/pkg/pkg.go").as_deref(),
            Some("example.com/nested/pkg")
        );
        assert_eq!(
            vendor_import_suffix("vendor/example.com/org/vendor/vendor.go").as_deref(),
            Some("example.com/org/vendor")
        );
        assert_eq!(
            vendor_import_suffix("vendor/example.com/vendor/sensor/sensor.go").as_deref(),
            Some("sensor")
        );
    }
}

fn nearest_module(
    directory: &Path,
    boundary: &Path,
    cache: &mut HashMap<PathBuf, Option<(PathBuf, String)>>,
) -> Option<(PathBuf, String)> {
    if let Some(cached) = cache.get(directory) {
        return cached.clone();
    }
    let mut cursor = directory.to_path_buf();
    let mut visited = Vec::new();
    let mut found = None;
    for _ in 0..64 {
        visited.push(cursor.clone());
        if let Some(module_path) = read_module_path(&cursor.join("go.mod")) {
            found = Some((cursor.clone(), module_path));
            break;
        }
        if cursor == boundary || !cursor.starts_with(boundary) || !cursor.pop() {
            break;
        }
    }
    for path in visited {
        cache.insert(path, found.clone());
    }
    found
}

fn read_module_path(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > 1024 * 1024 {
        return None;
    }
    let source = std::fs::read_to_string(path).ok()?;
    source.lines().find_map(|line| {
        let line = line.split("//").next()?.trim();
        let mut fields = line.split_whitespace();
        if fields.next()? != "module" {
            return None;
        }
        let value = fields.next()?;
        if value.is_empty() {
            return None;
        }
        Some(value.trim_matches(['"', '`']).to_string())
    })
}

fn read_go_work_module_dirs(workspace: &Path) -> Option<HashSet<PathBuf>> {
    let path = workspace.join("go.work");
    match std::fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() && metadata.len() <= 1024 * 1024 => {}
        Ok(_) => return Some(HashSet::new()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return Some(HashSet::new()),
    }
    let source = match std::fs::read_to_string(path) {
        Ok(source) => source,
        Err(_) => return Some(HashSet::new()),
    };
    let mut directories = HashSet::new();
    let mut in_use_block = false;
    for raw_line in source.lines() {
        let line = raw_line.split("//").next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        if in_use_block {
            let closes = line.ends_with(')');
            let value = line.trim_end_matches(')').trim();
            if !value.is_empty() {
                retain_go_work_directory(&mut directories, workspace, value);
            }
            if closes {
                in_use_block = false;
            }
            continue;
        }
        let mut fields = line.splitn(2, char::is_whitespace);
        if fields.next() != Some("use") {
            continue;
        }
        let rest = fields.next().unwrap_or_default().trim();
        if let Some(block) = rest.strip_prefix('(') {
            let closes = block.ends_with(')');
            let block = block.trim_end_matches(')').trim();
            for value in block.split_whitespace() {
                retain_go_work_directory(&mut directories, workspace, value);
            }
            in_use_block = !closes;
        } else if !rest.is_empty() {
            retain_go_work_directory(&mut directories, workspace, rest);
        }
    }
    Some(directories)
}

fn retain_go_work_directory(directories: &mut HashSet<PathBuf>, workspace: &Path, value: &str) {
    let value = value.trim_matches(['"', '`']);
    if value.is_empty() {
        return;
    }
    let candidate = workspace.join(value);
    let candidate = candidate.canonicalize().unwrap_or(candidate);
    if candidate.starts_with(workspace) {
        directories.insert(candidate);
    }
}

fn retain_open_reason(
    open: &mut HashMap<String, &'static str>,
    package: String,
    reason: &'static str,
) {
    let rank = |reason: &str| match reason {
        "unsupported_language_boundary" => 3,
        "unresolved_import" => 2,
        "ambiguous_import" => 1,
        "build_constraint_unknown" => 0,
        _ => 0,
    };
    match open.get(&package) {
        Some(current) if rank(current) >= rank(reason) => {}
        _ => {
            open.insert(package, reason);
        }
    }
}
