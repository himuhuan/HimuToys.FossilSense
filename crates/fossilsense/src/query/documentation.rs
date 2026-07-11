use std::cmp::Ordering;
use std::path::Path;

use crate::model::DefinitionCandidate;
use crate::project_context::ProjectContextIndex;

/// A semantic candidate projected into the narrower documentation-source policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentationCandidate {
    pub candidate: DefinitionCandidate,
    pub signature: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DocumentationSourcePreference {
    CompatibleSource,
    Primary,
    HeaderDeclaration,
}

/// Rank sources for documentation without changing definition/completion ranking.
/// A compatible header declaration in the same project/module is the public API
/// documentation source; the current `.c` definition remains the semantic target.
pub fn rank_documentation_candidates(
    primary: &DocumentationCandidate,
    candidates: &[DocumentationCandidate],
    project_context: Option<&ProjectContextIndex>,
) -> Vec<DocumentationCandidate> {
    let mut compatible: Vec<_> = candidates
        .iter()
        .filter(|candidate| candidate.candidate.name == primary.candidate.name)
        .filter(|candidate| candidate.candidate.kind == primary.candidate.kind)
        .filter(|candidate| signatures_compatible(&candidate.signature, &primary.signature))
        .filter(|candidate| {
            same_documentation_project(
                &primary.candidate.path,
                &candidate.candidate.path,
                project_context,
            )
        })
        .cloned()
        .collect();

    if !compatible.iter().any(|candidate| candidate == primary) {
        compatible.push(primary.clone());
    }
    compatible.sort_by(|left, right| compare_documentation_candidates(primary, left, right));
    compatible.dedup_by(|left, right| {
        left.candidate.path == right.candidate.path
            && left.candidate.range == right.candidate.range
            && left.signature == right.signature
    });
    compatible
}

fn compare_documentation_candidates(
    primary: &DocumentationCandidate,
    left: &DocumentationCandidate,
    right: &DocumentationCandidate,
) -> Ordering {
    documentation_preference(primary, right)
        .cmp(&documentation_preference(primary, left))
        .then_with(|| {
            same_module_stem(&primary.candidate.path, &right.candidate.path).cmp(&same_module_stem(
                &primary.candidate.path,
                &left.candidate.path,
            ))
        })
        .then_with(|| {
            (right.candidate.role == "declaration").cmp(&(left.candidate.role == "declaration"))
        })
        .then_with(|| right.candidate.tier.cmp(&left.candidate.tier))
        .then_with(|| left.candidate.path.cmp(&right.candidate.path))
        .then_with(|| {
            left.candidate
                .range
                .start_line
                .cmp(&right.candidate.range.start_line)
        })
}

fn documentation_preference(
    primary: &DocumentationCandidate,
    candidate: &DocumentationCandidate,
) -> DocumentationSourcePreference {
    if is_header_path(&candidate.candidate.path) {
        DocumentationSourcePreference::HeaderDeclaration
    } else if candidate == primary {
        DocumentationSourcePreference::Primary
    } else {
        DocumentationSourcePreference::CompatibleSource
    }
}

pub(super) fn same_documentation_project(
    primary_path: &str,
    candidate_path: &str,
    project_context: Option<&ProjectContextIndex>,
) -> bool {
    if primary_path == candidate_path {
        return true;
    }
    let Some(index) = project_context else {
        return false;
    };
    matches!(
        (
            index.nearest_for_file(primary_path),
            index.nearest_for_file(candidate_path),
        ),
        (Some(primary), Some(candidate)) if primary == candidate
    )
}

fn same_module_stem(left: &str, right: &str) -> bool {
    let left = Path::new(left).file_stem().and_then(|value| value.to_str());
    let right = Path::new(right)
        .file_stem()
        .and_then(|value| value.to_str());
    left.zip(right)
        .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right))
}

pub(super) fn is_header_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "h" | "hh" | "hpp" | "hxx" | "inl"
            )
        })
}

pub(super) fn signatures_compatible(left: &str, right: &str) -> bool {
    normalize_signature(left) == normalize_signature(right)
}

fn normalize_signature(signature: &str) -> String {
    let signature = signature
        .split_once('{')
        .map_or(signature, |(declaration, _body)| declaration)
        .trim()
        .trim_end_matches([';', '{'])
        .trim();
    let mut out = String::with_capacity(signature.len());
    let mut pending_space = false;
    for ch in signature.chars() {
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space && !matches!(ch, ')' | ']' | ',' | ';') && !out.ends_with(['(', '[']) {
            out.push(' ');
        }
        if matches!(ch, '(' | ')' | '[' | ']' | ',' | '*' | '&') {
            while out.ends_with(' ') {
                out.pop();
            }
        }
        out.push(ch);
        pending_space = matches!(ch, ',');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CandidateRange, ResolutionConfidence, ResolutionReason, ScopeTier};
    use crate::project_context::{ProjectContext, ProjectContextIndex, ProjectKey};

    fn projects() -> ProjectContextIndex {
        let key = ProjectKey {
            workspace_root_id: "workspace".to_string(),
            project_path: "lib".to_string(),
        };
        ProjectContextIndex::new(
            "workspace".to_string(),
            "test".to_string(),
            vec![ProjectContext {
                key,
                workspace_name: "lib".to_string(),
                marker_files: vec!["lib/CMakeLists.txt".to_string()],
            }],
        )
    }

    fn candidate(path: &str, signature: &str, role: &str) -> DocumentationCandidate {
        DocumentationCandidate {
            candidate: DefinitionCandidate {
                name: "lookup".to_string(),
                kind: "function".to_string(),
                role: role.to_string(),
                path: path.to_string(),
                range: CandidateRange {
                    start_line: 0,
                    start_col: 0,
                    end_line: 0,
                    end_col: 6,
                },
                source: "workspace".to_string(),
                tier: ScopeTier::Reachable,
                base_match: 1_000,
                confidence: ResolutionConfidence::Reachable,
                reason: ResolutionReason::ReachableInclude,
            },
            signature: signature.to_string(),
        }
    }

    #[test]
    fn compatible_header_declaration_precedes_current_source_definition() {
        let mut source = candidate("lib/ops_chain.c", "int lookup(int value)", "definition");
        source.candidate.tier = ScopeTier::Current;
        let header = candidate("lib/ops_chain.h", "int lookup(int value);", "declaration");
        let ranked = rank_documentation_candidates(
            &source,
            &[source.clone(), header.clone()],
            Some(&projects()),
        );
        assert_eq!(ranked[0], header);
    }

    #[test]
    fn incompatible_overload_does_not_supply_documentation() {
        let source = candidate("lib/ops_chain.c", "int lookup(int value)", "definition");
        let header = candidate(
            "lib/ops_chain.h",
            "int lookup(const char *value);",
            "declaration",
        );
        let ranked = rank_documentation_candidates(&source, &[header], Some(&projects()));
        assert_eq!(ranked, vec![source]);
    }

    #[test]
    fn missing_project_model_disables_header_preference() {
        let source = candidate("lib/ops_chain.c", "int lookup(int value)", "definition");
        let header = candidate("lib/ops_chain.h", "int lookup(int value);", "declaration");
        let ranked = rank_documentation_candidates(&source, &[header], None);
        assert_eq!(ranked, vec![source]);
    }

    #[test]
    fn different_project_header_cannot_supply_documentation() {
        let source = candidate("lib/ops_chain.c", "int lookup(int value)", "definition");
        let header = candidate("other/ops_chain.h", "int lookup(int value);", "declaration");
        let key = ProjectKey {
            workspace_root_id: "workspace".to_string(),
            project_path: "other".to_string(),
        };
        let projects = ProjectContextIndex::new(
            "workspace".to_string(),
            "test".to_string(),
            vec![
                projects().projects()[0].clone(),
                ProjectContext {
                    key,
                    workspace_name: "other".to_string(),
                    marker_files: vec!["other/CMakeLists.txt".to_string()],
                },
            ],
        );
        let ranked = rank_documentation_candidates(&source, &[header], Some(&projects));
        assert_eq!(ranked, vec![source]);
    }

    #[test]
    fn parser_signatures_for_header_and_source_pair_are_compatible() {
        let header = crate::parser::parse(
            Path::new("lib/ops_chain.h"),
            "/** docs */\nint pair_lookup(int value);\n",
        );
        let source = crate::parser::parse(
            Path::new("lib/ops_chain.c"),
            "int pair_lookup(int value) { return value; }\n",
        );
        let header = header
            .symbols
            .iter()
            .find(|symbol| symbol.name == "pair_lookup")
            .expect("header symbol");
        let source = source
            .symbols
            .iter()
            .find(|symbol| symbol.name == "pair_lookup")
            .expect("source symbol");
        assert!(
            signatures_compatible(&header.signature, &source.signature),
            "header={:?}, source={:?}",
            header.signature,
            source.signature
        );
    }
}
