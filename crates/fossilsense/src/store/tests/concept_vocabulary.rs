use std::collections::HashSet;
use std::fs;
use std::path::Path;

fn read(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

fn rust_sources_under(dir: &Path, out: &mut Vec<String>) {
    for entry in fs::read_dir(dir).unwrap_or_else(|err| panic!("failed to read {dir:?}: {err}")) {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            rust_sources_under(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"));
            let rel = path
                .strip_prefix(root)
                .expect("source below manifest dir")
                .to_string_lossy()
                .replace('\\', "/");
            out.push(rel);
        }
    }
}

fn production_rust_sources() -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut paths = Vec::new();
    rust_sources_under(&root.join("src"), &mut paths);
    paths
        .into_iter()
        .filter(|path| !path.ends_with("/tests.rs") && !path.contains("/tests/"))
        .collect()
}

fn extract_type_name(line: &str) -> Option<String> {
    let line = line.trim_start();
    for keyword in ["enum ", "struct ", "type "] {
        let Some(index) = line.find(keyword) else {
            continue;
        };
        let after = &line[index + keyword.len()..];
        let name = after
            .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
            .next()
            .unwrap_or_default();
        if !name.is_empty() {
            return Some(name.to_string());
        }
    }
    None
}

fn concept_type_names(source: &str) -> Vec<String> {
    source
        .lines()
        .filter_map(extract_type_name)
        .filter(|name| {
            ["Confidence", "Reason", "Binding", "Scope", "Role"]
                .iter()
                .any(|term| name.contains(term))
        })
        .collect()
}

fn allowed_concept_type_names() -> HashSet<&'static str> {
    [
        "CompletionIntentConfidence",
        "CompletionScope",
        "CompletionScopeLabel",
        "FactUnavailableReason",
        "LocalBinding",
        "LocalBindingKind",
        "MemberConfidence",
        "OpenReason",
        "ReachScope",
        "RecordConfidence",
        "ReferenceRoleCache",
        "ResolutionConfidence",
        "ResolutionReason",
        "RoleCacheInner",
        "ScopeChannel",
        "ScopeTier",
        "SymbolRole",
        "SyntacticRole",
    ]
    .into_iter()
    .collect()
}

#[test]
fn concept_vocabulary_reuses_canonical_model_parser_reference_terms() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let allowed = allowed_concept_type_names();
    let mut unexpected = Vec::new();

    for rel_path in production_rust_sources() {
        let source = read(&root.join(&rel_path));
        for name in concept_type_names(&source) {
            if !allowed.contains(name.as_str()) {
                unexpected.push(format!("{rel_path}: {name}"));
            }
        }
    }

    assert!(
        unexpected.is_empty(),
        "new confidence/reason/binding/scope/role concepts must reuse canonical vocabulary or update the documented allowlist: {unexpected:?}"
    );
}

#[test]
fn concept_vocabulary_guard_fixture_catches_parallel_scope_type() {
    let names = concept_type_names("pub struct SmartScopeBinding { value: String }");

    assert_eq!(
        names,
        vec!["SmartScopeBinding"],
        "guard fixture should catch new parallel concept names"
    );
    assert!(!allowed_concept_type_names().contains("SmartScopeBinding"));
}
