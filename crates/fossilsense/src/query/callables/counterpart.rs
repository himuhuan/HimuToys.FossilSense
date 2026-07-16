use std::collections::{HashMap, HashSet};

use crate::call_model::{
    AnchorRole, CallableKind, LinkageDomain, OwnerKindHint, SignatureFidelity,
};
use crate::model::ScopeTier;
use crate::reachability::ReachScope;

use super::{CallableVariantGroup, CandidateCoverage, ResolvedCallableAnchor};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CounterpartEvidence {
    Unpaired,
    IncompleteCoverage,
    Ambiguous { candidate_edges: usize },
    StrictOneToOne,
}

pub fn resolve_counterparts(
    anchors: &[ResolvedCallableAnchor],
    source_reach: &HashMap<String, ReachScope>,
    coverage: &CandidateCoverage,
) -> Vec<CallableVariantGroup> {
    // Coverage is also the work-budget boundary.  Do not derive the
    // potentially quadratic source/header compatibility matrix after the
    // caller has already reported a truncated or otherwise incomplete
    // candidate bucket.
    // `scope_open` describes uncertainty outside the files already reached; it
    // does not revoke a concrete source -> header relationship that is present
    // in `ReachScope::files`. Only recall truncation or another explicit
    // incomplete reason means the candidate set itself cannot support a
    // uniqueness claim.
    if coverage.truncated || coverage.incomplete_reason.is_some() {
        return anchors
            .iter()
            .cloned()
            .map(|anchor| singleton_group(anchor, CounterpartEvidence::IncompleteCoverage))
            .collect();
    }

    let sources: Vec<usize> = anchors
        .iter()
        .enumerate()
        .filter_map(|(index, anchor)| is_source_definition(anchor).then_some(index))
        .collect();
    let headers: Vec<usize> = anchors
        .iter()
        .enumerate()
        .filter_map(|(index, anchor)| is_header_declaration(anchor).then_some(index))
        .collect();

    let mut source_edges: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut header_edges: HashMap<usize, Vec<usize>> = HashMap::new();

    for &source in &sources {
        for &header in &headers {
            if strict_edge(&anchors[source], &anchors[header], source_reach) {
                source_edges.entry(source).or_default().push(header);
                header_edges.entry(header).or_default().push(source);
            }
        }
    }

    let mut paired = HashSet::new();
    let mut groups = Vec::new();
    for index in 0..anchors.len() {
        if paired.contains(&index) {
            continue;
        }
        let counterpart = source_edges
            .get(&index)
            .filter(|edges| edges.len() == 1)
            .and_then(|edges| edges.first().copied())
            .filter(|header| {
                header_edges
                    .get(header)
                    .is_some_and(|edges| edges.len() == 1)
            });
        if let Some(header) = counterpart {
            paired.insert(index);
            paired.insert(header);
            groups.push(pair_group(anchors[index].clone(), anchors[header].clone()));
            continue;
        }

        let counterpart = header_edges
            .get(&index)
            .filter(|edges| edges.len() == 1)
            .and_then(|edges| edges.first().copied())
            .filter(|source| {
                source_edges
                    .get(source)
                    .is_some_and(|edges| edges.len() == 1)
            });
        if let Some(source) = counterpart {
            paired.insert(index);
            paired.insert(source);
            groups.push(pair_group(anchors[source].clone(), anchors[index].clone()));
            continue;
        }

        let degree = source_edges
            .get(&index)
            .or_else(|| header_edges.get(&index))
            .map_or(0, Vec::len);
        let evidence = if degree == 0 {
            CounterpartEvidence::Unpaired
        } else {
            CounterpartEvidence::Ambiguous {
                candidate_edges: degree,
            }
        };
        groups.push(singleton_group(anchors[index].clone(), evidence));
    }

    groups.sort_by(|left, right| {
        right
            .group_tier
            .rank()
            .cmp(&left.group_tier.rank())
            .then_with(|| group_stable_key(left).cmp(&group_stable_key(right)))
    });
    groups
}

fn strict_edge(
    source: &ResolvedCallableAnchor,
    header: &ResolvedCallableAnchor,
    source_reach: &HashMap<String, ReachScope>,
) -> bool {
    if !strict_identity_compatible(source, header) {
        return false;
    }
    source_reach
        .get(&source.anchor.path)
        .is_some_and(|scope| scope.files.contains(&header.anchor.path))
}

fn strict_identity_compatible(
    source: &ResolvedCallableAnchor,
    header: &ResolvedCallableAnchor,
) -> bool {
    !(source.anchor.name != header.anchor.name
        || source.anchor.kind != header.anchor.kind
        || source.anchor.kind != CallableKind::Function
        || !is_free_function(source)
        || !is_free_function(header)
        || source.anchor.qualified_name != header.anchor.qualified_name
        || source.canonical_signature().is_empty()
        || source.canonical_signature() != header.canonical_signature()
        || source.anchor.signature_fidelity != SignatureFidelity::AstExact
        || header.anchor.signature_fidelity != SignatureFidelity::AstExact
        || !matches!(source.anchor.linkage, LinkageDomain::External)
        || !matches!(header.anchor.linkage, LinkageDomain::External))
}

fn is_free_function(anchor: &ResolvedCallableAnchor) -> bool {
    matches!(
        anchor.anchor.owner_kind,
        None | Some(OwnerKindHint::Namespace)
    )
}

fn is_source_definition(anchor: &ResolvedCallableAnchor) -> bool {
    anchor.anchor.role == AnchorRole::Definition && is_source_path(&anchor.anchor.path)
}

fn is_header_declaration(anchor: &ResolvedCallableAnchor) -> bool {
    anchor.anchor.role == AnchorRole::Declaration && is_header_path(&anchor.anchor.path)
}

pub(crate) fn is_header_path(path: &str) -> bool {
    extension(path).is_some_and(|extension| {
        ["h", "hh", "hpp", "hxx", "inl"]
            .iter()
            .any(|expected| extension.eq_ignore_ascii_case(expected))
    })
}

pub(crate) fn is_source_path(path: &str) -> bool {
    extension(path).is_some_and(|extension| {
        ["c", "cc", "cpp", "cxx"]
            .iter()
            .any(|expected| extension.eq_ignore_ascii_case(expected))
    })
}

fn extension(path: &str) -> Option<&str> {
    path.rsplit_once('.').map(|(_, extension)| extension)
}

fn pair_group(
    source: ResolvedCallableAnchor,
    header: ResolvedCallableAnchor,
) -> CallableVariantGroup {
    CallableVariantGroup {
        group_tier: stronger_tier(source.candidate.tier, header.candidate.tier),
        header_declaration: Some(header),
        source_definition: Some(source),
        other_variants: Vec::new(),
        counterpart_evidence: CounterpartEvidence::StrictOneToOne,
    }
}

fn singleton_group(
    anchor: ResolvedCallableAnchor,
    counterpart_evidence: CounterpartEvidence,
) -> CallableVariantGroup {
    let tier = anchor.candidate.tier;
    if is_header_declaration(&anchor) {
        CallableVariantGroup {
            header_declaration: Some(anchor),
            source_definition: None,
            other_variants: Vec::new(),
            group_tier: tier,
            counterpart_evidence,
        }
    } else if is_source_definition(&anchor) {
        CallableVariantGroup {
            header_declaration: None,
            source_definition: Some(anchor),
            other_variants: Vec::new(),
            group_tier: tier,
            counterpart_evidence,
        }
    } else {
        CallableVariantGroup {
            header_declaration: None,
            source_definition: None,
            other_variants: vec![anchor],
            group_tier: tier,
            counterpart_evidence,
        }
    }
}

fn stronger_tier(left: ScopeTier, right: ScopeTier) -> ScopeTier {
    if left.rank() >= right.rank() {
        left
    } else {
        right
    }
}

fn group_stable_key(group: &CallableVariantGroup) -> (&str, usize) {
    group
        .header_declaration
        .as_ref()
        .or(group.source_definition.as_ref())
        .or(group.other_variants.first())
        .map_or(("", 0), |anchor| {
            (
                anchor.anchor.path.as_str(),
                anchor.anchor.name_range.start_byte,
            )
        })
}
