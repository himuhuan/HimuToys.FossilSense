#[cfg(test)]
use std::path::Path;

use crate::model::DefinitionCandidate;
use crate::project_context::ProjectContextIndex;

/// A semantic candidate projected into the narrower documentation-source policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentationCandidate {
    pub candidate: DefinitionCandidate,
    pub signature: String,
}

/// Keep documentation attached to the already-selected presentation anchor.
/// Callable header/source selection is performed once by the strict candidate
/// graph; this legacy-shaped helper must never infer a counterpart from path,
/// project, or signature similarity.
pub fn rank_documentation_candidates(
    primary: &DocumentationCandidate,
    candidates: &[DocumentationCandidate],
    project_context: Option<&ProjectContextIndex>,
) -> Vec<DocumentationCandidate> {
    let _ = (candidates, project_context);
    vec![primary.clone()]
}

#[cfg(test)]
pub(super) fn signatures_compatible(left: &str, right: &str) -> bool {
    normalize_signature(left) == normalize_signature(right)
}

#[cfg(test)]
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
    fn selected_primary_is_not_replaced_by_a_similar_header() {
        let mut source = candidate("lib/ops_chain.c", "int lookup(int value)", "definition");
        source.candidate.tier = ScopeTier::Current;
        let header = candidate("lib/ops_chain.h", "int lookup(int value);", "declaration");
        let ranked = rank_documentation_candidates(
            &source,
            &[source.clone(), header.clone()],
            Some(&projects()),
        );
        assert_eq!(ranked, vec![source]);
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
