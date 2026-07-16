use crate::call_model::AnchorRole;

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
    let mut definitions: Vec<_> = groups
        .iter()
        .flat_map(|group| {
            group
                .variants()
                .filter(|anchor| anchor.anchor.role == AnchorRole::Definition)
                .map(move |anchor| (group, anchor))
        })
        .collect();
    if !definitions.is_empty() {
        retain_strongest_group_tier(&mut definitions);
        if definitions
            .iter()
            .any(|(group, _)| group.counterpart_evidence == CounterpartEvidence::StrictOneToOne)
        {
            definitions.retain(|(group, _)| {
                group.counterpart_evidence == CounterpartEvidence::StrictOneToOne
            });
        }
        return sort_selected_presentations(definitions, false);
    }

    // A declaration is a conservative Definition fallback only when no
    // implementation anchor survived. Never mix it into a result that already
    // contains a definition.
    let mut declarations = groups
        .iter()
        .flat_map(|group| {
            group
                .variants()
                .filter(|anchor| anchor.anchor.role == AnchorRole::Declaration)
                .map(move |anchor| (group, anchor))
        })
        .collect();
    retain_strongest_group_tier(&mut declarations);
    sort_selected_presentations(declarations, true)
}

/// Select declaration targets for standard Go to Declaration semantics.
///
/// A proven strict counterpart contributes its header declaration and
/// suppresses unrelated fallbacks. Without such a relation, choose the
/// strongest declaration tier and the nearest declaration within that tier;
/// only an all-definition candidate set falls back to a definition anchor.
#[cfg(test)]
pub fn call_declaration_presentations(
    groups: &[CallableVariantGroup],
) -> Vec<&ResolvedCallableAnchor> {
    call_declaration_presentations_for_origin(groups, None)
}

/// Cursor-aware Declaration selection used by the LSP path. Declarations in
/// the origin file must precede (or contain) the use site; declarations in an
/// included file are ordered by reach tier because their byte offsets are not
/// comparable with the origin cursor.
pub fn call_declaration_presentations_at<'a>(
    groups: &'a [CallableVariantGroup],
    origin_path: &str,
    cursor_byte: usize,
) -> Vec<&'a ResolvedCallableAnchor> {
    call_declaration_presentations_for_origin(groups, Some((origin_path, cursor_byte)))
}

fn call_declaration_presentations_for_origin<'a>(
    groups: &'a [CallableVariantGroup],
    origin: Option<(&str, usize)>,
) -> Vec<&'a ResolvedCallableAnchor> {
    let mut declarations: Vec<_> = groups
        .iter()
        .flat_map(|group| {
            group
                .variants()
                .filter(|anchor| anchor.anchor.role == AnchorRole::Declaration)
                .filter(|anchor| declaration_is_visible(anchor, origin))
                .map(move |anchor| (group, anchor))
        })
        .collect();
    retain_strongest_candidate_tier(&mut declarations);
    let strict_headers: Vec<_> = declarations
        .iter()
        .copied()
        .filter(|(group, anchor)| {
            group.counterpart_evidence == CounterpartEvidence::StrictOneToOne
                && is_header_path(&anchor.anchor.path)
        })
        .collect();
    if !strict_headers.is_empty() {
        return sort_selected_presentations(strict_headers, true);
    }
    if let Some(declaration) = strongest_nearest_presentation(declarations) {
        return vec![declaration];
    }

    let mut definitions = groups
        .iter()
        .flat_map(|group| {
            group
                .variants()
                .filter(|anchor| anchor.anchor.role == AnchorRole::Definition)
                .map(move |anchor| (group, anchor))
        })
        .collect();
    retain_strongest_candidate_tier(&mut definitions);
    strongest_nearest_presentation(definitions)
        .into_iter()
        .collect()
}

fn declaration_is_visible(anchor: &ResolvedCallableAnchor, origin: Option<(&str, usize)>) -> bool {
    origin.is_none_or(|(origin_path, cursor_byte)| {
        anchor.anchor.path != origin_path || anchor.anchor.name_range.start_byte <= cursor_byte
    })
}

fn retain_strongest_group_tier<'a>(
    selected: &mut Vec<(&'a CallableVariantGroup, &'a ResolvedCallableAnchor)>,
) {
    let Some(best) = selected
        .iter()
        .map(|(group, _)| group.group_tier.rank())
        .max()
    else {
        return;
    };
    selected.retain(|(group, _)| group.group_tier.rank() == best);
}

fn retain_strongest_candidate_tier<'a>(
    selected: &mut Vec<(&'a CallableVariantGroup, &'a ResolvedCallableAnchor)>,
) {
    let Some(best) = selected
        .iter()
        .map(|(_, anchor)| anchor.candidate.tier.rank())
        .max()
    else {
        return;
    };
    selected.retain(|(_, anchor)| anchor.candidate.tier.rank() == best);
}

/// Return the sole opposite anchor only for a proven strict counterpart pair.
/// `None` tells the Definition consumer to use its normal multi-candidate path.
#[cfg(test)]
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
    let selected: Vec<_> = groups
        .iter()
        .filter_map(|group| choose(group).map(|anchor| (group, anchor)))
        .collect();
    sort_selected_presentations(selected, header_first)
}

fn sort_selected_presentations<'a>(
    mut selected: Vec<(&'a CallableVariantGroup, &'a ResolvedCallableAnchor)>,
    header_first: bool,
) -> Vec<&'a ResolvedCallableAnchor> {
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

fn strongest_nearest_presentation<'a>(
    mut selected: Vec<(&'a CallableVariantGroup, &'a ResolvedCallableAnchor)>,
) -> Option<&'a ResolvedCallableAnchor> {
    selected.sort_by(|(_, left), (_, right)| {
        right
            .candidate
            .tier
            .rank()
            .cmp(&left.candidate.tier.rank())
            .then_with(|| {
                right
                    .arity_compatibility
                    .rank()
                    .cmp(&left.arity_compatibility.rank())
            })
            .then_with(|| right.candidate.base_match.cmp(&left.candidate.base_match))
            .then_with(|| {
                if left.anchor.path == right.anchor.path {
                    // Within one file, the later declaration is the nearest
                    // available approximation without a cursor position.
                    right
                        .anchor
                        .name_range
                        .start_byte
                        .cmp(&left.anchor.name_range.start_byte)
                } else {
                    left.anchor.path.cmp(&right.anchor.path)
                }
            })
            .then_with(|| {
                left.anchor
                    .name_range
                    .start_byte
                    .cmp(&right.anchor.name_range.start_byte)
            })
    });
    selected.first().map(|(_, anchor)| *anchor)
}

fn role_preference(anchor: &ResolvedCallableAnchor, header_first: bool) -> u8 {
    if header_first {
        u8::from(is_header_path(&anchor.anchor.path))
    } else {
        u8::from(is_source_path(&anchor.anchor.path))
    }
}
