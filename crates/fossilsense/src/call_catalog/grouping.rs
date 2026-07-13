//! Conservative callable-variant grouping for the request-scoped relation
//! catalog. Parser entity keys are family hints; only the shared strict
//! counterpart resolver may combine concrete anchors into one entity.

use std::collections::{BTreeMap, HashMap};

use crate::call_model::{AnchorRole, CallableAnchor};
use crate::model::{CandidateRange, DefinitionCandidate, ScopeTier};
use crate::query::{
    resolve_counterparts, CandidateCoverage, CandidateIncompleteReason, CandidateOrigin,
    ResolvedCallableAnchor,
};
use crate::reachability::ReachGraph;
use crate::resolver;

const GROUP_KEY_SEPARATOR: &str = "::fossilsense-group::";

// Relation queries are already narrowed by their store reads, but a request
// can still contain many distinct callable names or a highly duplicated name.
// Only this many concrete anchors may participate in strict counterpart work;
// all anchors are still retained as conservative singleton entities.
pub(super) const STRICT_COUNTERPART_ANCHOR_BUDGET: usize = 1_024;

// `resolve_counterparts` compares source definitions with header declarations.
// Reserve an upper bound before invoking it so adversarial duplication cannot
// turn one request into an unbounded Cartesian product.
pub(super) const STRICT_COUNTERPART_EDGE_BUDGET: usize = 16_384;

pub(super) struct SemanticAnchorGroup {
    pub variants: Vec<CallableAnchor>,
    pub candidate_limited: bool,
}

pub(super) struct SemanticAnchorGroups {
    pub groups: Vec<SemanticAnchorGroup>,
}

pub(super) fn semantic_anchor_groups(
    mut anchors: Vec<CallableAnchor>,
    reach_graph: Option<&ReachGraph>,
    incomplete: bool,
) -> SemanticAnchorGroups {
    anchors.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.name_range.start_byte.cmp(&right.name_range.start_byte))
            .then_with(|| left.anchor_fingerprint.cmp(&right.anchor_fingerprint))
    });
    anchors.dedup_by(|left, right| {
        left.path == right.path
            && left.anchor_fingerprint == right.anchor_fingerprint
            && left.name_range == right.name_range
    });

    // Exact names are independent candidate families. Keeping their coverage
    // local prevents an open `bar` scope from invalidating a closed, unique
    // `foo` declaration/definition pair and avoids comparing unrelated names.
    let mut buckets: BTreeMap<String, Vec<CallableAnchor>> = BTreeMap::new();
    for anchor in anchors {
        buckets.entry(anchor.name.clone()).or_default().push(anchor);
    }

    let mut remaining_anchor_budget = STRICT_COUNTERPART_ANCHOR_BUDGET;
    let mut remaining_edge_budget = STRICT_COUNTERPART_EDGE_BUDGET;
    let mut grouped = Vec::new();
    for bucket in buckets.into_values() {
        let resolved: Vec<_> = bucket
            .into_iter()
            .map(resolved_anchor_for_relation_grouping)
            .collect();
        let source_paths: Vec<_> = resolved
            .iter()
            .filter(|anchor| {
                anchor.anchor.role == AnchorRole::Definition
                    && crate::query::is_source_path(&anchor.anchor.path)
            })
            .map(|anchor| anchor.anchor.path.clone())
            .collect();
        // This intentionally over-counts declarations that later prove not to
        // be headers or signature-compatible. It is a cheap upper bound that
        // can be computed without performing the Cartesian comparison itself.
        let declaration_count = resolved
            .iter()
            .filter(|anchor| anchor.anchor.role == AnchorRole::Declaration)
            .count();
        let edge_cost = source_paths.len().saturating_mul(declaration_count);
        let budget_limited =
            resolved.len() > remaining_anchor_budget || edge_cost > remaining_edge_budget;

        if !incomplete && !budget_limited {
            remaining_anchor_budget -= resolved.len();
            remaining_edge_budget -= edge_cost;
        }

        let mut source_reach = HashMap::new();
        if !incomplete && !budget_limited {
            if let Some(graph) = reach_graph {
                for path in &source_paths {
                    source_reach
                        .entry(path.clone())
                        .or_insert_with(|| graph.reachable(path).as_ref().clone());
                }
            }
        }
        let coverage = CandidateCoverage {
            scanned: resolved.len(),
            truncated: budget_limited,
            scope_open: !incomplete
                && !budget_limited
                && source_paths
                    .iter()
                    .any(|path| source_reach.get(path).is_none_or(|scope| scope.open)),
            incomplete_reason: if budget_limited {
                Some(CandidateIncompleteReason::CandidateBudget)
            } else {
                incomplete.then_some(CandidateIncompleteReason::Cancelled)
            },
        };
        grouped.extend(
            resolve_counterparts(&resolved, &source_reach, &coverage)
                .into_iter()
                .map(|group| {
                    let variants = group
                        .variants()
                        .map(|anchor| anchor.anchor.clone())
                        .collect::<Vec<_>>();
                    SemanticAnchorGroup {
                        variants,
                        candidate_limited: budget_limited,
                    }
                }),
        );
    }

    // Resolver output is stable within each exact-name bucket. Restore a
    // request-wide path/location order so introducing name buckets does not
    // make relation entity IDs depend on hash-map or input ordering.
    grouped.sort_by(|left, right| {
        group_stable_key(&left.variants).cmp(&group_stable_key(&right.variants))
    });
    SemanticAnchorGroups { groups: grouped }
}

fn group_stable_key(group: &[CallableAnchor]) -> (&str, usize, &str) {
    group.first().map_or(("", 0, ""), |anchor| {
        (
            anchor.path.as_str(),
            anchor.name_range.start_byte,
            anchor.anchor_fingerprint.as_str(),
        )
    })
}

fn resolved_anchor_for_relation_grouping(anchor: CallableAnchor) -> ResolvedCallableAnchor {
    let tier = ScopeTier::Global;
    let (confidence, reason) = resolver::confidence_reason_for(tier, true, None);
    let candidate = DefinitionCandidate {
        name: anchor.name.clone(),
        kind: anchor.kind.as_str().into(),
        role: anchor.role.as_str().into(),
        path: anchor.path.clone(),
        range: CandidateRange {
            start_line: anchor.name_range.start.line,
            start_col: anchor.name_range.start.character,
            end_line: anchor.name_range.end.line,
            end_col: anchor.name_range.end.character,
        },
        source: if std::path::Path::new(&anchor.path).is_absolute() {
            "external".into()
        } else {
            "workspace".into()
        },
        tier,
        base_match: if anchor.role == AnchorRole::Definition {
            1_000
        } else {
            900
        },
        confidence,
        reason,
    };
    ResolvedCallableAnchor::new(anchor, candidate, CandidateOrigin::Base)
}

pub(super) fn derived_group_key(raw_key: &str, variants: &[CallableAnchor]) -> String {
    let mut identities: Vec<_> = variants
        .iter()
        .map(|anchor| {
            (
                anchor.path.as_str(),
                anchor.name_range.start_byte,
                anchor.anchor_fingerprint.as_str(),
            )
        })
        .collect();
    identities.sort_unstable();
    let mut hasher = blake3::Hasher::new();
    for (path, start_byte, fingerprint) in identities {
        hasher.update(path.as_bytes());
        hasher.update(&[0]);
        hasher.update(&start_byte.to_le_bytes());
        hasher.update(&[0]);
        hasher.update(fingerprint.as_bytes());
        hasher.update(&[0xff]);
    }
    format!(
        "{raw_key}{GROUP_KEY_SEPARATOR}{}",
        &hasher.finalize().to_hex()[..24]
    )
}

pub(crate) fn raw_entity_key(logical_key: &str) -> &str {
    logical_key
        .split_once(GROUP_KEY_SEPARATOR)
        .map_or(logical_key, |(raw, _)| raw)
}
