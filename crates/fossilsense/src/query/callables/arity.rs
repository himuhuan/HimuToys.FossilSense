use crate::call_model::{CallForm, CallSiteFact, SignatureShape, SourcePosition, SourceRange};
use crate::model::ResolutionConfidence;

use super::ResolvedCallableAnchor;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArgumentState {
    Complete,
    Partial {
        minimum_arity: u32,
        active_argument: u32,
    },
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContextReliability {
    Reliable,
    SyntaxErrorOverlap,
    UnsupportedCallForm,
}

impl ContextReliability {
    pub fn is_reliable(self) -> bool {
        self == Self::Reliable
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSiteContext {
    pub callee_name: String,
    pub qualified_name: Option<String>,
    pub form: CallForm,
    pub callee_range: SourceRange,
    pub argument_count: Option<u32>,
    pub argument_state: ArgumentState,
    pub reliability: ContextReliability,
}

impl CallSiteContext {
    pub fn from_complete_call(fact: &CallSiteFact, position: SourcePosition) -> Option<Self> {
        if !position_in_range(position, fact.callee_range) {
            return None;
        }
        let callee_name = fact.callee_name.clone()?;
        let supported = matches!(
            fact.form,
            CallForm::DirectName | CallForm::QualifiedName | CallForm::ParenthesizedName
        );
        let reliability = if fact.syntax_error_overlap {
            ContextReliability::SyntaxErrorOverlap
        } else if !supported {
            ContextReliability::UnsupportedCallForm
        } else {
            ContextReliability::Reliable
        };
        Some(Self {
            callee_name,
            qualified_name: fact.qualified_name.clone(),
            form: fact.form,
            callee_range: fact.callee_range,
            argument_count: fact.argument_count,
            argument_state: if fact.argument_count.is_some() {
                ArgumentState::Complete
            } else {
                ArgumentState::Unknown
            },
            reliability,
        })
    }

    pub fn partial(
        callee_name: String,
        form: CallForm,
        callee_range: SourceRange,
        minimum_arity: u32,
        active_argument: u32,
        reliability: ContextReliability,
    ) -> Self {
        Self {
            callee_name,
            qualified_name: None,
            form,
            callee_range,
            argument_count: None,
            argument_state: ArgumentState::Partial {
                minimum_arity,
                active_argument,
            },
            reliability,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArityCompatibility {
    Compatible,
    Unknown,
    Incompatible,
}

impl ArityCompatibility {
    pub(crate) fn rank(self) -> u8 {
        match self {
            Self::Compatible => 2,
            Self::Unknown => 1,
            Self::Incompatible => 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArityFilterOutcome {
    NotApplied,
    Filtered,
    UnknownOnly,
    MismatchFallback,
}

pub fn compatibility_for_signature(
    signature: &SignatureShape,
    context: Option<&CallSiteContext>,
) -> ArityCompatibility {
    let Some(context) = context else {
        return ArityCompatibility::Unknown;
    };
    if !context.reliability.is_reliable() {
        return ArityCompatibility::Unknown;
    }
    match context.argument_state {
        ArgumentState::Complete => {
            context
                .argument_count
                .map_or(ArityCompatibility::Unknown, |arity| {
                    match signature.accepts_arity(arity) {
                        Some(true) => ArityCompatibility::Compatible,
                        Some(false) => ArityCompatibility::Incompatible,
                        None => ArityCompatibility::Unknown,
                    }
                })
        }
        ArgumentState::Partial { minimum_arity, .. } => match signature.max_arity {
            Some(max) if max < minimum_arity => ArityCompatibility::Incompatible,
            Some(_) => ArityCompatibility::Compatible,
            None if signature.variadic && signature.min_arity.is_some() => {
                ArityCompatibility::Compatible
            }
            None => ArityCompatibility::Unknown,
        },
        ArgumentState::Unknown => ArityCompatibility::Unknown,
    }
}

pub fn apply_arity_policy(
    anchors: &mut Vec<ResolvedCallableAnchor>,
    context: Option<&CallSiteContext>,
) -> ArityFilterOutcome {
    for anchor in anchors.iter_mut() {
        anchor.arity_compatibility = compatibility_for_signature(&anchor.anchor.signature, context);
    }

    let can_apply = context.is_some_and(|context| {
        context.reliability.is_reliable()
            && !matches!(context.argument_state, ArgumentState::Unknown)
    });
    if !can_apply || anchors.is_empty() {
        sort_anchors(anchors);
        return ArityFilterOutcome::NotApplied;
    }

    let compatible = anchors
        .iter()
        .any(|anchor| anchor.arity_compatibility == ArityCompatibility::Compatible);
    let unknown = anchors
        .iter()
        .any(|anchor| anchor.arity_compatibility == ArityCompatibility::Unknown);
    let outcome = if compatible {
        anchors.retain(|anchor| anchor.arity_compatibility != ArityCompatibility::Incompatible);
        ArityFilterOutcome::Filtered
    } else if unknown {
        anchors.retain(|anchor| anchor.arity_compatibility == ArityCompatibility::Unknown);
        ArityFilterOutcome::UnknownOnly
    } else {
        // Keeping all-incompatible candidates is an intentional navigation
        // escape hatch, not a claim that any candidate matches the call.  Put
        // the downgrade on each retained production candidate so Hover and
        // Signature Help cannot accidentally continue displaying a stronger
        // name/scope confidence while the set-level bool remains available to
        // consumers that want an explicit fallback annotation.
        for anchor in anchors.iter_mut() {
            anchor.candidate.confidence = ResolutionConfidence::Fallback;
        }
        ArityFilterOutcome::MismatchFallback
    };

    sort_anchors(anchors);
    outcome
}

fn sort_anchors(anchors: &mut [ResolvedCallableAnchor]) {
    anchors.sort_by(|left, right| {
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
            .then_with(|| left.anchor.path.cmp(&right.anchor.path))
            .then_with(|| {
                left.anchor
                    .name_range
                    .start_byte
                    .cmp(&right.anchor.name_range.start_byte)
            })
    });
}

fn position_in_range(position: SourcePosition, range: SourceRange) -> bool {
    (position.line, position.character) >= (range.start.line, range.start.character)
        && (position.line, position.character) <= (range.end.line, range.end.character)
}
