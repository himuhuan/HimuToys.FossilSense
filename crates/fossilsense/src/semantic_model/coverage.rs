//! File-revision coverage evidence, independent of parser and storage implementations.
use crate::call_model::SourceRange;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum FactGroup {
    Declarations,
    FallbackCompletions,
    Includes,
    Occurrences,
    Records,
    Fields,
    Members,
    Aliases,
    LocalDeclarations,
    LocalBindings,
    CallableAnchors,
    CallSites,
    BindingSites,
    ExplicitBases,
    IndirectAssignments,
    MacroFacts,
}

impl FactGroup {
    pub const ALL: [Self; 16] = [
        Self::Declarations,
        Self::FallbackCompletions,
        Self::Includes,
        Self::Occurrences,
        Self::Records,
        Self::Fields,
        Self::Members,
        Self::Aliases,
        Self::LocalDeclarations,
        Self::LocalBindings,
        Self::CallableAnchors,
        Self::CallSites,
        Self::BindingSites,
        Self::ExplicitBases,
        Self::IndirectAssignments,
        Self::MacroFacts,
    ];
    pub const fn bit(self) -> u16 {
        1 << self as u8
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactCoverage {
    Complete,
    Partial,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum CoverageReason {
    SyntaxRecoveryFailed,
    UnknownMacro,
    LanguageAmbiguity,
    UnsupportedDeclarator,
    BudgetExhausted,
}

impl CoverageReason {
    pub const ALL: [Self; 5] = [
        Self::SyntaxRecoveryFailed,
        Self::UnknownMacro,
        Self::LanguageAmbiguity,
        Self::UnsupportedDeclarator,
        Self::BudgetExhausted,
    ];
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SyntaxRecoveryFailed => "syntax_recovery_failed",
            Self::UnknownMacro => "unknown_macro",
            Self::LanguageAmbiguity => "language_ambiguity",
            Self::UnsupportedDeclarator => "unsupported_declarator",
            Self::BudgetExhausted => "budget_exhausted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageEvidence {
    Syntax,
    MacroInvocation,
    MacroType,
    Declarator,
    ScopeNotSupported,
    LanguageSelection,
    Recovery,
    UnexplainedDeclaration,
    Limit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageGap {
    pub range: SourceRange,
    pub groups: u16,
    pub reason: CoverageReason,
    pub evidence: CoverageEvidence,
    /// At most eight existing canonical locators, never guessed macro-generated names.
    pub related_declarations: Vec<String>,
    pub recovery_rules: Vec<String>,
}

/// Compact enough to carry with a file revision or a current-document overlay.
/// Detailed ranges are deliberately kept out of completion recall structures.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeclarationCoverageSummary {
    pub requested_groups: u16,
    pub checked_groups: u16,
    pub partial_groups: u16,
    pub reason_groups: [u16; 5],
    pub candidate_regions: u32,
    pub explained_regions: u32,
    pub non_declaration_regions: u32,
    pub uncovered_regions: u32,
    pub candidate_bytes: u64,
    pub uncovered_bytes: u64,
    pub node_visits: u32,
    pub recovered_regions: u32,
    pub truncated: bool,
}

impl DeclarationCoverageSummary {
    pub fn fact_coverage(&self, group: FactGroup) -> FactCoverage {
        if self.requested_groups & self.checked_groups & group.bit() == 0 {
            FactCoverage::Unknown
        } else if self.partial_groups & group.bit() != 0 {
            FactCoverage::Partial
        } else {
            FactCoverage::Complete
        }
    }

    pub fn reason_mask(&self, groups: u16) -> u8 {
        self.reason_groups
            .iter()
            .enumerate()
            .fold(0, |mask, (index, affected)| {
                mask | if affected & groups != 0 {
                    1 << index
                } else {
                    0
                }
            })
    }

    pub fn uncovered_ratio(&self) -> Option<f64> {
        (!self.truncated && self.candidate_bytes > 0)
            .then(|| self.uncovered_bytes as f64 / self.candidate_bytes as f64)
    }

    pub fn note_gap(&mut self, groups: u16, reason: CoverageReason) {
        self.partial_groups |= groups;
        self.reason_groups[reason as usize] |= groups;
        self.uncovered_regions = self.uncovered_regions.saturating_add(1);
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeclarationCoverage {
    pub summary: DeclarationCoverageSummary,
    pub gaps: Vec<CoverageGap>,
}
