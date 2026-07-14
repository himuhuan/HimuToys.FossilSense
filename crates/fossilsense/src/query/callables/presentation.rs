use super::counterpart::{is_header_path, is_source_path};
use super::{CallableVariantGroup, CounterpartEvidence, ResolvedCallableAnchor};

pub fn hover_presentations(groups: &[CallableVariantGroup]) -> Vec<&ResolvedCallableAnchor> {
    sorted_presentations(
        groups,
        |group| {
            group
                .header_declaration
                .as_ref()
                .or(group.source_definition.as_ref())
                .or(group.other_variants.first())
        },
        true,
    )
}

pub fn signature_presentations(groups: &[CallableVariantGroup]) -> Vec<&ResolvedCallableAnchor> {
    hover_presentations(groups)
}

/// Choose the active slot without changing the scope-first presentation
/// order. A proven compatible signature is more useful than an Unknown one
/// even when the latter belongs to a stronger scope tier; the list itself
/// remains ordered by the normal cross-feature candidate contract.
pub fn signature_active_index(presentations: &[&ResolvedCallableAnchor]) -> usize {
    presentations
        .iter()
        .position(|candidate| {
            candidate.arity_compatibility == super::ArityCompatibility::Compatible
        })
        .or_else(|| {
            presentations.iter().position(|candidate| {
                candidate.arity_compatibility == super::ArityCompatibility::Unknown
            })
        })
        .unwrap_or(0)
}

pub fn call_definition_presentations(
    groups: &[CallableVariantGroup],
) -> Vec<&ResolvedCallableAnchor> {
    sorted_presentations(
        groups,
        |group| {
            group
                .source_definition
                .as_ref()
                .or(group.header_declaration.as_ref())
                .or(group.other_variants.first())
        },
        false,
    )
}

/// Return the sole opposite anchor only for a proven strict counterpart pair.
/// `None` tells the Definition consumer to use its normal multi-candidate path.
pub fn anchor_opposite_definition<'a>(
    groups: &'a [CallableVariantGroup],
    origin_fingerprint: &str,
) -> Option<&'a ResolvedCallableAnchor> {
    groups.iter().find_map(|group| {
        if group.counterpart_evidence != CounterpartEvidence::StrictOneToOne {
            return None;
        }
        match (
            group.header_declaration.as_ref(),
            group.source_definition.as_ref(),
        ) {
            (Some(header), Some(source))
                if header.anchor.anchor_fingerprint == origin_fingerprint =>
            {
                Some(source)
            }
            (Some(header), Some(source))
                if source.anchor.anchor_fingerprint == origin_fingerprint =>
            {
                Some(header)
            }
            _ => None,
        }
    })
}

fn sorted_presentations<'a>(
    groups: &'a [CallableVariantGroup],
    choose: impl Fn(&'a CallableVariantGroup) -> Option<&'a ResolvedCallableAnchor>,
    header_first: bool,
) -> Vec<&'a ResolvedCallableAnchor> {
    let mut selected: Vec<_> = groups
        .iter()
        .filter_map(|group| choose(group).map(|anchor| (group, anchor)))
        .collect();
    selected.sort_by(|(left_group, left), (right_group, right)| {
        right_group
            .group_tier
            .rank()
            .cmp(&left_group.group_tier.rank())
            .then_with(|| {
                right_group
                    .strongest_arity_compatibility()
                    .rank()
                    .cmp(&left_group.strongest_arity_compatibility().rank())
            })
            .then_with(|| {
                let left_role = role_preference(left, header_first);
                let right_role = role_preference(right, header_first);
                right_role.cmp(&left_role)
            })
            .then_with(|| left.anchor.path.cmp(&right.anchor.path))
            .then_with(|| {
                left.anchor
                    .name_range
                    .start_byte
                    .cmp(&right.anchor.name_range.start_byte)
            })
    });
    selected.into_iter().map(|(_, anchor)| anchor).collect()
}

fn role_preference(anchor: &ResolvedCallableAnchor, header_first: bool) -> u8 {
    if header_first {
        u8::from(is_header_path(&anchor.anchor.path))
    } else {
        u8::from(is_source_path(&anchor.anchor.path))
    }
}
