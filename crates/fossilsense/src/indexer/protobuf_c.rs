use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::Result;
use ignore::WalkBuilder;

use crate::config::{normalized_extension, ConfigIssue, DEFAULT_EXCLUDED_DIRS};
use crate::parser::{extract_protobuf_c_declarations, ProtobufCDeclaration};
use crate::pathing::{normalize_abs_path, relative_slash_path};
use crate::store::{
    GeneratedDeclarationRow, IncludeGraphUpdate, IndexBuild, IndexStore, ProtobufCSourceAssociation,
};

const MAX_SOURCES_PER_GENERATED_DECLARATION: usize = 64;
const MAX_PROTO_SOURCE_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_BUFFERED_SOURCES_PER_GENERATED_DECLARATION: usize =
    MAX_SOURCES_PER_GENERATED_DECLARATION + 1;
const MAX_PROTO_ASSOCIATION_COMPARISONS: usize = 1_000_000;

#[derive(Debug, Clone, Copy)]
pub(super) struct ProtoScanLimits {
    pub(super) max_files: usize,
    pub(super) max_bytes: u64,
}

#[derive(Debug, Clone)]
struct ProtoFile {
    absolute_path: PathBuf,
    normalized_path: String,
    relative_paths: Vec<String>,
    basename_lower: String,
    byte_len: u64,
}

#[derive(Debug, Clone)]
struct SourceCandidate {
    declaration: ProtobufCDeclaration,
    proto_path: String,
    match_kind: &'static str,
    extraction_truncated: bool,
}

#[derive(Debug, Clone)]
struct HeaderMatchPlan {
    header_id: i64,
    expected_basename: String,
    desired_paths: HashSet<String>,
}

#[derive(Debug, Default)]
struct HeaderMatchIndexes {
    strong_by_relative_path: HashMap<String, Vec<i64>>,
    weak_by_basename: HashMap<String, Vec<i64>>,
}

fn build_header_match_indexes(
    plans: &[HeaderMatchPlan],
    available_relative_paths: &HashSet<String>,
) -> HeaderMatchIndexes {
    let mut indexes = HeaderMatchIndexes::default();
    for plan in plans {
        let strong_paths: Vec<_> = plan
            .desired_paths
            .iter()
            .filter(|path| available_relative_paths.contains(*path))
            .cloned()
            .collect();
        if strong_paths.is_empty() {
            indexes
                .weak_by_basename
                .entry(plan.expected_basename.clone())
                .or_default()
                .push(plan.header_id);
        } else {
            for path in strong_paths {
                indexes
                    .strong_by_relative_path
                    .entry(path)
                    .or_default()
                    .push(plan.header_id);
            }
        }
    }
    for header_ids in indexes
        .strong_by_relative_path
        .values_mut()
        .chain(indexes.weak_by_basename.values_mut())
    {
        header_ids.sort_unstable();
        header_ids.dedup();
    }
    indexes
}

fn consume_association_comparison(comparisons: &mut usize, limit: usize) -> bool {
    if *comparisons >= limit {
        return false;
    }
    *comparisons += 1;
    true
}

fn header_matches_for_proto_file(
    file: &ProtoFile,
    indexes: &HeaderMatchIndexes,
    comparisons: &mut usize,
    limit: usize,
) -> Option<Vec<(i64, &'static str)>> {
    let mut matches = Vec::new();
    for relative_path in &file.relative_paths {
        if let Some(header_ids) = indexes
            .strong_by_relative_path
            .get(&relative_path.to_ascii_lowercase())
        {
            for header_id in header_ids {
                if !consume_association_comparison(comparisons, limit) {
                    return None;
                }
                matches.push((*header_id, "relative_path"));
            }
        }
    }
    if let Some(header_ids) = indexes.weak_by_basename.get(&file.basename_lower) {
        for header_id in header_ids {
            if !consume_association_comparison(comparisons, limit) {
                return None;
            }
            matches.push((*header_id, "same_basename"));
        }
    }
    matches.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| match_kind_rank(left.1).cmp(&match_kind_rank(right.1)))
    });
    matches.dedup_by(|left, right| left.0 == right.0);
    Some(matches)
}

pub(super) fn build_protobuf_c_sources(
    store: &IndexStore,
    build: IndexBuild,
    workspace: &Path,
    include_roots: &[PathBuf],
    proto_roots: &[PathBuf],
    include_graph: &IncludeGraphUpdate,
    limits: ProtoScanLimits,
) -> Result<(Vec<ProtobufCSourceAssociation>, Vec<ConfigIssue>)> {
    if proto_roots.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }

    let files = store.effective_files_with_ids(build)?;
    let path_by_id: HashMap<_, _> = files
        .iter()
        .map(|(id, path, _)| (*id, path.clone()))
        .collect();
    let id_by_path: HashMap<_, _> = files
        .iter()
        .map(|(id, path, _)| (path.clone(), *id))
        .collect();
    let all_paths: HashSet<_> = files.iter().map(|(_, path, _)| path.clone()).collect();
    let mut workspace_paths_by_basename = HashMap::<String, Vec<String>>::new();
    for (_, path, source) in &files {
        if source == "workspace" {
            workspace_paths_by_basename
                .entry(path.rsplit('/').next().unwrap_or(path).to_string())
                .or_default()
                .push(path.clone());
        }
    }
    let include_roots_slash: Vec<_> = include_roots
        .iter()
        .map(|root| normalize_abs_path(root))
        .collect();
    let include_edges = store.effective_include_edge_ids(build, include_graph)?;
    let include_edge_set: HashSet<_> = include_edges.iter().copied().collect();
    let included_header_ids: HashSet<_> = include_edges
        .iter()
        .filter_map(|(_, target)| {
            path_by_id
                .get(target)
                .is_some_and(|path| ends_with_ignore_ascii_case(path, ".pb-c.h"))
                .then_some(*target)
        })
        .collect();
    if included_header_ids.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }

    let generated_declarations: Vec<GeneratedDeclarationRow> = store
        .effective_generated_declarations(build)?
        .into_iter()
        .filter(|declaration| included_header_ids.contains(&declaration.file_id))
        .collect();
    if generated_declarations.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }

    let mut include_relative_proto_paths = HashMap::<i64, HashSet<String>>::new();
    for (source, include_target) in store.effective_includes_with_file_ids(build, None)? {
        let Some(source_path) = path_by_id.get(&source) else {
            continue;
        };
        let source_directory = source_path
            .rsplit_once('/')
            .map(|(directory, _)| directory)
            .unwrap_or_default();
        let resolved_paths = match crate::includes::resolve_include(
            &include_target,
            source_directory,
            &include_roots_slash,
            &all_paths,
            &workspace_paths_by_basename,
        ) {
            crate::includes::IncludeResolution::Edge { dst, .. } => vec![dst],
            crate::includes::IncludeResolution::Ambiguous { dsts } => dsts,
            crate::includes::IncludeResolution::Unresolved => continue,
        };
        let Some((_, normalized_target)) =
            crate::includes::normalize_include_target(&include_target)
        else {
            continue;
        };
        let Some(proto_path) = generated_proto_path(&normalized_target) else {
            continue;
        };
        for resolved_path in resolved_paths {
            let Some(target) = id_by_path.get(&resolved_path).copied() else {
                continue;
            };
            if !include_edge_set.contains(&(source, target))
                || !included_header_ids.contains(&target)
            {
                continue;
            }
            include_relative_proto_paths
                .entry(target)
                .or_default()
                .insert(proto_path.to_ascii_lowercase());
        }
    }

    let (proto_files, mut issues) =
        discover_proto_files(proto_roots, limits.max_files, limits.max_bytes);
    let mut sources_by_header_and_name = HashMap::<(i64, String), Vec<SourceCandidate>>::new();
    let mut source_collection_truncated = HashSet::<(i64, String)>::new();
    let mut header_match_plans = Vec::new();

    let declaration_names_by_file = generated_declarations.iter().fold(
        HashMap::<i64, HashSet<String>>::new(),
        |mut by_file, declaration| {
            by_file
                .entry(declaration.file_id)
                .or_default()
                .insert(declaration.name.clone());
            by_file
        },
    );

    for header_id in included_header_ids {
        let Some(stored_header_path) = path_by_id.get(&header_id) else {
            continue;
        };
        let Some(expected_basename) = generated_proto_basename(stored_header_path) else {
            continue;
        };
        let absolute_header = if Path::new(stored_header_path).is_absolute() {
            PathBuf::from(stored_header_path)
        } else {
            workspace.join(stored_header_path)
        };
        let mut desired_paths =
            desired_relative_proto_paths(stored_header_path, &absolute_header, include_roots);
        if let Some(include_paths) = include_relative_proto_paths.get(&header_id) {
            desired_paths.extend(include_paths.iter().cloned());
        }
        header_match_plans.push(HeaderMatchPlan {
            header_id,
            expected_basename,
            desired_paths,
        });
    }

    let available_relative_paths = proto_files
        .iter()
        .flat_map(|file| file.relative_paths.iter())
        .map(|path| path.to_ascii_lowercase())
        .collect();
    let header_match_indexes =
        build_header_match_indexes(&header_match_plans, &available_relative_paths);
    let mut association_comparisons = 0usize;
    let mut association_work_truncated = false;

    'proto_files: for file in &proto_files {
        let Some(header_matches) = header_matches_for_proto_file(
            file,
            &header_match_indexes,
            &mut association_comparisons,
            MAX_PROTO_ASSOCIATION_COMPARISONS,
        ) else {
            association_work_truncated = true;
            break 'proto_files;
        };
        if header_matches.is_empty() {
            continue;
        }
        if file.byte_len > MAX_PROTO_SOURCE_FILE_BYTES {
            issues.push(ConfigIssue {
                message: format!(
                    "protobuf-c source file exceeds the {} byte extraction cap; skipping {}",
                    MAX_PROTO_SOURCE_FILE_BYTES,
                    file.absolute_path.display()
                ),
            });
            continue;
        }
        let source_file = match fs::File::open(&file.absolute_path) {
            Ok(source_file) => source_file,
            Err(error) => {
                issues.push(ConfigIssue {
                    message: format!(
                        "protobuf-c source file could not be read, skipping {}: {error}",
                        file.absolute_path.display()
                    ),
                });
                continue;
            }
        };
        let mut source = String::with_capacity(file.byte_len as usize);
        match source_file
            .take(MAX_PROTO_SOURCE_FILE_BYTES + 1)
            .read_to_string(&mut source)
        {
            Ok(_) if source.len() as u64 <= MAX_PROTO_SOURCE_FILE_BYTES => {}
            Ok(_) => {
                issues.push(ConfigIssue {
                    message: format!(
                        "protobuf-c source file grew beyond the {} byte extraction cap; skipping {}",
                        MAX_PROTO_SOURCE_FILE_BYTES,
                        file.absolute_path.display()
                    ),
                });
                continue;
            }
            Err(error) => {
                issues.push(ConfigIssue {
                    message: format!(
                        "protobuf-c source file is not readable UTF-8, skipping {}: {error}",
                        file.absolute_path.display()
                    ),
                });
                continue;
            }
        }
        let extraction = extract_protobuf_c_declarations(&source);
        if extraction.truncated {
            issues.push(ConfigIssue {
                message: format!(
                    "protobuf-c declaration extraction reached its token budget; remaining declarations were skipped: {}",
                    file.absolute_path.display()
                ),
            });
        }
        for proto_declaration in extraction.declarations {
            for (header_id, match_kind) in &header_matches {
                if !consume_association_comparison(
                    &mut association_comparisons,
                    MAX_PROTO_ASSOCIATION_COMPARISONS,
                ) {
                    association_work_truncated = true;
                    break 'proto_files;
                }
                let Some(generated_names) = declaration_names_by_file.get(header_id) else {
                    continue;
                };
                if !generated_names.contains(&proto_declaration.c_name) {
                    continue;
                }
                let key = (*header_id, proto_declaration.c_name.clone());
                let candidates = sources_by_header_and_name.entry(key.clone()).or_default();
                if candidates.len() >= MAX_BUFFERED_SOURCES_PER_GENERATED_DECLARATION {
                    source_collection_truncated.insert(key);
                    continue;
                }
                candidates.push(SourceCandidate {
                    declaration: proto_declaration.clone(),
                    proto_path: file.normalized_path.clone(),
                    match_kind,
                    extraction_truncated: extraction.truncated,
                });
            }
        }
    }
    if association_work_truncated {
        issues.push(ConfigIssue {
            message: format!(
                "protobuf-c source association reached its {MAX_PROTO_ASSOCIATION_COMPARISONS} comparison budget; remaining source candidates were skipped"
            ),
        });
    }

    let mut associations = Vec::new();
    for generated in generated_declarations {
        let Some(sources) =
            sources_by_header_and_name.get_mut(&(generated.file_id, generated.name.clone()))
        else {
            continue;
        };
        sources.sort_by(|left, right| {
            match_kind_rank(left.match_kind)
                .cmp(&match_kind_rank(right.match_kind))
                .then_with(|| {
                    left.proto_path
                        .to_ascii_lowercase()
                        .cmp(&right.proto_path.to_ascii_lowercase())
                })
                .then_with(|| left.proto_path.cmp(&right.proto_path))
                .then_with(|| {
                    left.declaration
                        .start_line
                        .cmp(&right.declaration.start_line)
                })
                .then_with(|| left.declaration.start_col.cmp(&right.declaration.start_col))
                .then_with(|| {
                    left.declaration
                        .proto_name
                        .cmp(&right.declaration.proto_name)
                })
        });
        sources.dedup_by(|left, right| {
            left.proto_path == right.proto_path
                && left.declaration.start_byte == right.declaration.start_byte
                && left.declaration.end_byte == right.declaration.end_byte
                && left.declaration.proto_name == right.declaration.proto_name
        });
        let truncated = sources.len() > MAX_SOURCES_PER_GENERATED_DECLARATION
            || sources.iter().any(|source| source.extraction_truncated)
            || source_collection_truncated.contains(&(generated.file_id, generated.name.clone()))
            || association_work_truncated;
        for source in sources.iter().take(MAX_SOURCES_PER_GENERATED_DECLARATION) {
            associations.push(ProtobufCSourceAssociation {
                declaration_id: generated.declaration_id,
                proto_path: source.proto_path.clone(),
                proto_name: source.declaration.proto_name.clone(),
                c_name: source.declaration.c_name.clone(),
                kind: source.declaration.kind.as_str().to_string(),
                start_byte: source.declaration.start_byte,
                end_byte: source.declaration.end_byte,
                start_line: source.declaration.start_line,
                start_col: source.declaration.start_col,
                end_line: source.declaration.end_line,
                end_col: source.declaration.end_col,
                match_kind: source.match_kind.to_string(),
                source_truncated: truncated,
            });
        }
    }
    associations.sort_by(|left, right| {
        left.declaration_id
            .cmp(&right.declaration_id)
            .then_with(|| {
                match_kind_rank(&left.match_kind).cmp(&match_kind_rank(&right.match_kind))
            })
            .then_with(|| left.proto_path.cmp(&right.proto_path))
            .then_with(|| left.start_byte.cmp(&right.start_byte))
    });
    Ok((associations, issues))
}

fn discover_proto_files(
    roots: &[PathBuf],
    max_files: usize,
    max_bytes: u64,
) -> (Vec<ProtoFile>, Vec<ConfigIssue>) {
    let mut files_by_path = HashMap::<String, ProtoFile>::new();
    let mut issues = Vec::new();
    for root in roots {
        let mut root_files = Vec::new();
        let mut file_count = 0usize;
        let mut byte_count = 0u64;
        let mut over_cap = false;
        let mut scan_error = None;
        let walker = WalkBuilder::new(root)
            .hidden(false)
            .parents(false)
            .ignore(false)
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .filter_entry(|entry| {
                let name = entry.file_name().to_string_lossy();
                !DEFAULT_EXCLUDED_DIRS
                    .iter()
                    .any(|excluded| name.eq_ignore_ascii_case(excluded))
            })
            .build();
        for entry in walker {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    scan_error = Some(error);
                    break;
                }
            };
            if !entry
                .file_type()
                .is_some_and(|file_type| file_type.is_file())
            {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            file_count = file_count.saturating_add(1);
            byte_count = byte_count.saturating_add(metadata.len());
            if file_count > max_files || byte_count > max_bytes {
                over_cap = true;
                break;
            }
            if !normalized_extension(entry.path())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("proto"))
            {
                continue;
            }
            let relative = relative_slash_path(root, entry.path()).unwrap_or_default();
            let absolute = entry
                .path()
                .canonicalize()
                .unwrap_or_else(|_| entry.path().to_path_buf());
            root_files.push((absolute, relative, metadata.len()));
        }
        if over_cap {
            issues.push(ConfigIssue {
                message: format!(
                    "protobufC.protoPaths root exceeds cap (>{max_files} files or >{max_bytes} bytes); source tracing skipped for this root: {}",
                    root.display()
                ),
            });
            continue;
        }
        if let Some(error) = scan_error {
            issues.push(ConfigIssue {
                message: format!(
                    "protobufC.protoPaths root could not be scanned; source tracing skipped for this root: {}: {error}",
                    root.display()
                ),
            });
            continue;
        }
        for (absolute, relative, byte_len) in root_files {
            let normalized = normalize_abs_path(&absolute);
            let key = normalized.to_ascii_lowercase();
            let basename_lower = relative
                .rsplit('/')
                .next()
                .unwrap_or(&relative)
                .to_ascii_lowercase();
            files_by_path
                .entry(key)
                .and_modify(|file| {
                    if !file.relative_paths.contains(&relative) {
                        file.relative_paths.push(relative.clone());
                    }
                })
                .or_insert(ProtoFile {
                    absolute_path: absolute,
                    normalized_path: normalized,
                    relative_paths: vec![relative],
                    basename_lower,
                    byte_len,
                });
        }
    }
    let mut files: Vec<_> = files_by_path.into_values().collect();
    for file in &mut files {
        file.relative_paths
            .sort_by_key(|path| path.to_ascii_lowercase());
        file.relative_paths
            .dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    }
    files.sort_by(|left, right| {
        left.normalized_path
            .to_ascii_lowercase()
            .cmp(&right.normalized_path.to_ascii_lowercase())
            .then_with(|| left.normalized_path.cmp(&right.normalized_path))
    });
    (files, issues)
}

fn desired_relative_proto_paths(
    stored_header_path: &str,
    absolute_header: &Path,
    include_roots: &[PathBuf],
) -> HashSet<String> {
    let mut paths = HashSet::new();
    if !Path::new(stored_header_path).is_absolute() {
        if let Some(relative) = generated_proto_path(stored_header_path) {
            paths.insert(relative.to_ascii_lowercase());
        }
    }
    let canonical_header = absolute_header
        .canonicalize()
        .unwrap_or_else(|_| absolute_header.to_path_buf());
    for root in include_roots {
        let canonical_root = root.canonicalize().unwrap_or_else(|_| root.clone());
        if let Ok(relative) = canonical_header.strip_prefix(&canonical_root) {
            let relative = relative.to_string_lossy().replace('\\', "/");
            if let Some(proto_path) = generated_proto_path(&relative) {
                paths.insert(proto_path.to_ascii_lowercase());
            }
        }
    }
    paths
}

fn generated_proto_path(header_path: &str) -> Option<String> {
    let suffix = ".pb-c.h";
    ends_with_ignore_ascii_case(header_path, suffix)
        .then(|| format!("{}.proto", &header_path[..header_path.len() - suffix.len()]))
}

fn generated_proto_basename(header_path: &str) -> Option<String> {
    generated_proto_path(header_path).map(|path| {
        path.rsplit('/')
            .next()
            .unwrap_or(&path)
            .to_ascii_lowercase()
    })
}

fn ends_with_ignore_ascii_case(value: &str, suffix: &str) -> bool {
    value
        .get(value.len().saturating_sub(suffix.len())..)
        .is_some_and(|tail| tail.eq_ignore_ascii_case(suffix))
}

fn match_kind_rank(kind: &str) -> u8 {
    if kind == "relative_path" {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_match_index_is_linear_in_headers_instead_of_header_file_pairs() {
        let plans: Vec<_> = (0..100)
            .map(|header_id| HeaderMatchPlan {
                header_id,
                expected_basename: "device.proto".to_string(),
                desired_paths: HashSet::from([format!("missing/{header_id}/device.proto")]),
            })
            .collect();
        let available_relative_paths: HashSet<_> = (0..200)
            .map(|index| format!("candidate/{index}/device.proto"))
            .collect();

        let indexes = build_header_match_indexes(&plans, &available_relative_paths);
        let retained_header_links: usize = indexes
            .strong_by_relative_path
            .values()
            .chain(indexes.weak_by_basename.values())
            .map(Vec::len)
            .sum();

        assert_eq!(retained_header_links, plans.len());
        assert_eq!(indexes.weak_by_basename["device.proto"].len(), 100);
    }

    #[test]
    fn association_work_stops_at_a_fixed_comparison_budget() {
        let mut comparisons = 0;

        assert!(consume_association_comparison(&mut comparisons, 2));
        assert!(consume_association_comparison(&mut comparisons, 2));
        assert!(!consume_association_comparison(&mut comparisons, 2));
        assert_eq!(comparisons, 2);
    }

    #[test]
    fn empty_same_basename_files_consume_the_file_header_budget() {
        let indexes = HeaderMatchIndexes {
            strong_by_relative_path: HashMap::new(),
            weak_by_basename: HashMap::from([("device.proto".to_string(), (0..10).collect())]),
        };
        let files: Vec<_> = (0..20)
            .map(|index| ProtoFile {
                absolute_path: PathBuf::from(format!("candidate/{index}/device.proto")),
                normalized_path: format!("candidate/{index}/device.proto"),
                relative_paths: vec![format!("candidate/{index}/device.proto")],
                basename_lower: "device.proto".to_string(),
                byte_len: 0,
            })
            .collect();
        let mut comparisons = 0;
        let complete_files = files
            .iter()
            .take_while(|file| {
                header_matches_for_proto_file(file, &indexes, &mut comparisons, 37).is_some()
            })
            .count();

        assert_eq!(complete_files, 3);
        assert_eq!(comparisons, 37);
    }
}
