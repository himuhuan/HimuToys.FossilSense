use std::collections::HashSet;
use std::ops::Range;

use super::ast::context::DeclarationContext;
use super::declarators::{decode_direct_declarators, DeclaratorFailureReason};
use super::{ParseFacts, RecoveryDiagnostic, RecoveryOutcome};
use crate::c_lexical::LexicalMap;
use crate::call_model::{SourcePosition, SourceRange};
use crate::config::SourceLanguage;
use crate::semantic_model::{
    CoverageEvidence, CoverageGap, CoverageReason, DeclarationCoverage, DeclarationCoverageSummary,
    DeclarationFact, FactGroup,
};

#[derive(Clone, Copy, Debug)]
pub(super) struct CoverageLimits {
    pub regions: usize,
    pub details: usize,
    pub nodes: usize,
}
impl Default for CoverageLimits {
    fn default() -> Self {
        Self {
            regions: 4096,
            details: 1024,
            nodes: 262_144,
        }
    }
}

#[cfg(test)]
thread_local! { static TEST_LIMITS: std::cell::Cell<Option<CoverageLimits>> = const { std::cell::Cell::new(None) }; }
#[cfg(test)]
pub(super) fn with_limits<T>(limits: CoverageLimits, work: impl FnOnce() -> T) -> T {
    struct Reset(Option<CoverageLimits>);
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_LIMITS.with(|slot| slot.set(self.0));
        }
    }
    let _reset = Reset(TEST_LIMITS.with(|slot| slot.replace(Some(limits))));
    work()
}
fn limits() -> CoverageLimits {
    #[cfg(test)]
    if let Some(limits) = TEST_LIMITS.with(|slot| slot.get()) {
        return limits;
    }
    CoverageLimits::default()
}

pub(super) fn requested_groups(facts: ParseFacts, fallback: bool) -> u16 {
    FactGroup::ALL
        .into_iter()
        .filter(|group| match group {
            FactGroup::BindingSites => facts.contains(ParseFacts::BINDING_SITES),
            FactGroup::ExplicitBases => facts.contains(ParseFacts::EXPLICIT_BASES),
            FactGroup::IndirectAssignments => facts.contains(ParseFacts::INDIRECT_ASSIGNMENTS),
            FactGroup::MacroFacts => facts.contains(ParseFacts::MACRO_FACTS),
            FactGroup::Includes => true,
            FactGroup::FallbackCompletions => fallback,
            FactGroup::Declarations => facts.contains(ParseFacts::DECLARATIONS),
            FactGroup::Occurrences => facts.contains(ParseFacts::OCCURRENCES),
            FactGroup::Records => facts.intersects(ParseFacts::RECORDS | ParseFacts::FIELDS),
            FactGroup::Fields | FactGroup::Members => facts.contains(ParseFacts::FIELDS),
            FactGroup::Aliases => facts.contains(ParseFacts::ALIASES),
            FactGroup::LocalDeclarations | FactGroup::LocalBindings => {
                facts.contains(ParseFacts::LOCAL_DECLS)
            }
            FactGroup::CallableAnchors | FactGroup::CallSites => {
                facts.contains(ParseFacts::CALL_RELATIONS)
            }
        })
        .fold(0, |mask, group| mask | group.bit())
}

fn semantic_groups() -> u16 {
    !(FactGroup::Includes.bit() | FactGroup::FallbackCompletions.bit())
}
fn local_groups() -> u16 {
    FactGroup::Occurrences.bit()
        | FactGroup::LocalDeclarations.bit()
        | FactGroup::LocalBindings.bit()
        | FactGroup::CallSites.bit()
}

pub(super) fn source_range(source: &str, lines: &[usize], range: Range<usize>) -> SourceRange {
    let position = |offset: usize| {
        let line = lines
            .partition_point(|start| *start <= offset)
            .saturating_sub(1);
        let start = lines.get(line).copied().unwrap_or(0);
        SourcePosition {
            line: line as u32,
            character: source[start..offset].encode_utf16().count() as u32,
        }
    };
    SourceRange {
        start: position(range.start),
        end: position(range.end),
        start_byte: range.start,
        end_byte: range.end,
    }
}

struct Audit<'a> {
    source: &'a str,
    lines: &'a [usize],
    declarations: &'a [&'a DeclarationFact],
    recoveries: &'a [RecoveryDiagnostic],
    limits: CoverageLimits,
    report: DeclarationCoverage,
    regions: Vec<Range<usize>>,
    stopped: bool,
    rejected_macro_ranges: Vec<Range<usize>>,
}

impl Audit<'_> {
    fn has_declaration(&self, range: &Range<usize>) -> bool {
        let start = self
            .declarations
            .partition_point(|fact| fact.name_range.start_byte < range.start);
        self.declarations
            .get(start)
            .is_some_and(|fact| fact.name_range.end_byte <= range.end)
    }
    fn region(&mut self, range: Range<usize>) -> bool {
        if self.report.summary.candidate_regions as usize >= self.limits.regions {
            self.stop(range.start);
            return false;
        }
        self.report.summary.candidate_regions += 1;
        self.regions.push(range);
        true
    }

    fn stop(&mut self, start: usize) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        self.report.summary.truncated = true;
        let groups = self.report.summary.requested_groups & semantic_groups();
        self.report
            .summary
            .note_gap(groups, CoverageReason::BudgetExhausted);
        let mut start = start;
        if self.report.gaps.len() >= self.limits.details.max(1) {
            if let Some(last) = self.report.gaps.pop() {
                start = start.min(last.range.start_byte);
            }
        }
        self.report.gaps.push(CoverageGap {
            range: source_range(self.source, self.lines, start..self.source.len()),
            groups,
            reason: CoverageReason::BudgetExhausted,
            evidence: CoverageEvidence::Limit,
            related_declarations: Vec::new(),
            recovery_rules: Vec::new(),
        });
    }

    fn gap(
        &mut self,
        range: Range<usize>,
        groups: u16,
        reason: CoverageReason,
        evidence: CoverageEvidence,
    ) {
        let groups = groups & self.report.summary.requested_groups;
        if groups == 0 || self.stopped {
            return;
        }
        if self.report.gaps.len() >= self.limits.details.max(1) {
            self.stop(range.start);
            return;
        }
        if self.report.gaps.iter().any(|gap| {
            gap.range.start_byte == range.start
                && gap.range.end_byte == range.end
                && gap.groups == groups
                && gap.reason == reason
        }) {
            return;
        }
        let start = self
            .declarations
            .partition_point(|fact| fact.name_range.start_byte < range.start);
        let related_declarations = if evidence == CoverageEvidence::MacroInvocation {
            Vec::new()
        } else {
            self.declarations[start..]
                .iter()
                .take(8)
                .take_while(|fact| fact.name_range.start_byte < range.end)
                .filter(|fact| fact.name_range.end_byte <= range.end)
                .map(|fact| fact.identity.locator.fingerprint.clone())
                .collect()
        };
        let mut recovery_rules: Vec<_> = self
            .recoveries
            .iter()
            .filter(|recovery| {
                recovery.affected_start_byte < range.end && range.start < recovery.affected_end_byte
            })
            .map(|recovery| format!("{:?}", recovery.rule))
            .take(5)
            .collect();
        recovery_rules.sort();
        recovery_rules.dedup();
        self.report.summary.note_gap(groups, reason);
        self.report.summary.truncated |= reason == CoverageReason::BudgetExhausted;
        self.report.gaps.push(CoverageGap {
            range: source_range(self.source, self.lines, range),
            groups,
            reason,
            evidence,
            related_declarations,
            recovery_rules,
        });
    }

    fn finish(mut self) -> CoverageAuditResult {
        let candidates = union_ranges(std::mem::take(&mut self.regions));
        self.report.summary.candidate_bytes =
            candidates.iter().map(|r| (r.end - r.start) as u64).sum();
        let uncovered = union_ranges(
            self.report
                .gaps
                .iter()
                .map(|gap| gap.range.start_byte..gap.range.end_byte)
                .collect(),
        );
        let (mut left, mut right) = (0, 0);
        while left < candidates.len() && right < uncovered.len() {
            let (candidate, gap) = (&candidates[left], &uncovered[right]);
            self.report.summary.uncovered_bytes += candidate
                .end
                .min(gap.end)
                .saturating_sub(candidate.start.max(gap.start))
                as u64;
            if candidate.end < gap.end {
                left += 1;
            } else {
                right += 1;
            }
        }
        self.report
            .gaps
            .sort_by_key(|gap| (gap.range.start_byte, gap.range.end_byte, gap.reason as u8));
        CoverageAuditResult {
            coverage: self.report,
            rejected_macro_ranges: self.rejected_macro_ranges,
        }
    }
}

fn union_ranges(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.sort_by_key(|range| (range.start, range.end));
    let mut union: Vec<Range<usize>> = Vec::new();
    for range in ranges {
        if let Some(last) = union.last_mut().filter(|last| range.start <= last.end) {
            last.end = last.end.max(range.end);
        } else {
            union.push(range);
        }
    }
    union
}

/// Bounded verification against the final AST and canonical facts. It shares
/// the original lexical map, decoder, context and recovery session evidence;
/// it never launches another parse or creates a declaration from a token hit.
pub(super) struct CoverageInput<'a> {
    pub root: tree_sitter::Node<'a>,
    pub source: &'a str,
    pub lines: &'a [usize],
    pub facts: ParseFacts,
    pub language: SourceLanguage,
    pub declarations: &'a [DeclarationFact],
    pub aliases: &'a [super::TypeAlias],
    pub lexical: &'a LexicalMap,
    pub recoveries: &'a [RecoveryDiagnostic],
}

pub(super) struct CoverageAuditResult {
    pub coverage: DeclarationCoverage,
    // Kept separately from capped display details: truncating a diagnostic must
    // never turn a rejected macro argument into a declaration.
    pub rejected_macro_ranges: Vec<Range<usize>>,
}

pub(super) fn analyze(input: CoverageInput<'_>) -> CoverageAuditResult {
    let CoverageInput {
        root,
        source,
        lines,
        facts,
        language,
        declarations,
        aliases,
        lexical,
        recoveries,
    } = input;
    let known_types: HashSet<&str> = aliases
        .iter()
        .map(|alias| alias.alias.as_str())
        .chain(
            declarations
                .iter()
                .filter(|fact| {
                    matches!(
                        fact.declaration_kind,
                        crate::semantic_model::SemanticDeclarationKind::Alias
                            | crate::semantic_model::SemanticDeclarationKind::Type
                    )
                })
                .map(|fact| fact.name.as_str()),
        )
        .collect();
    let requested = requested_groups(facts, false);
    let mut ordered: Vec<_> = declarations.iter().collect();
    ordered.sort_by_key(|fact| fact.name_range.start_byte);
    let mut audit = Audit {
        source,
        lines,
        declarations: &ordered,
        recoveries,
        limits: limits(),
        report: DeclarationCoverage {
            summary: DeclarationCoverageSummary {
                requested_groups: requested,
                checked_groups: requested,
                recovered_regions: recoveries
                    .iter()
                    .filter(|r| r.outcome == RecoveryOutcome::Applied)
                    .count() as u32,
                ..Default::default()
            },
            gaps: Vec::new(),
        },
        regions: Vec::new(),
        stopped: false,
        rejected_macro_ranges: Vec::new(),
    };
    for recovery in recoveries {
        if let RecoveryOutcome::Rejected(reason) = recovery.outcome {
            audit.gap(
                recovery.affected_start_byte..recovery.affected_end_byte,
                semantic_groups(),
                if reason.is_budget() {
                    CoverageReason::BudgetExhausted
                } else {
                    CoverageReason::SyntaxRecoveryFailed
                },
                CoverageEvidence::Recovery,
            );
        }
    }
    let mut context = DeclarationContext::default();
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        if audit.report.summary.node_visits as usize >= audit.limits.nodes {
            audit.stop(node.start_byte());
            break;
        }
        audit.report.summary.node_visits += 1;
        let local = context.local_scope(node).is_some();
        let kind = node.kind();
        if node.is_error() || node.is_missing() {
            let range = if node.start_byte() == node.end_byte() {
                node.parent().unwrap_or(node).byte_range()
            } else {
                node.byte_range()
            };
            if audit.region(range.clone()) {
                audit.gap(
                    range,
                    if local {
                        local_groups()
                    } else {
                        semantic_groups()
                    },
                    CoverageReason::SyntaxRecoveryFailed,
                    CoverageEvidence::Syntax,
                );
            }
        }
        let local_type = local
            && language == SourceLanguage::C
            && (kind == "type_definition"
                || (matches!(
                    kind,
                    "struct_specifier" | "union_specifier" | "enum_specifier"
                ) && node.child_by_field_name("body").is_some()));
        if local_type && audit.region(node.byte_range()) {
            let groups = if kind == "type_definition" {
                FactGroup::Aliases.bit()
            } else {
                FactGroup::Records.bit() | FactGroup::Fields.bit() | FactGroup::Members.bit()
            };
            audit.gap(
                node.byte_range(),
                groups,
                CoverageReason::UnsupportedDeclarator,
                CoverageEvidence::ScopeNotSupported,
            );
        } else if !local
            && matches!(
                kind,
                "declaration"
                    | "type_definition"
                    | "field_declaration"
                    | "function_definition"
                    | "preproc_def"
                    | "preproc_function_def"
                    | "expression_statement"
                    | "macro_type_specifier"
            )
        {
            let mut range = node.byte_range();
            if kind == "function_definition" {
                if let Some(body) = node.child_by_field_name("body") {
                    range.end = body.start_byte();
                }
            }
            if audit.region(range.clone()) {
                let groups = match kind {
                    "field_declaration" => FactGroup::Fields.bit() | FactGroup::Members.bit(),
                    "type_definition" => FactGroup::Declarations.bit() | FactGroup::Aliases.bit(),
                    _ => FactGroup::Declarations.bit() | FactGroup::CallableAnchors.bit(),
                };
                if let Some(name) = bare_invocation(source, lexical, range.clone()) {
                    if known_types.contains(name) {
                        if audit.has_declaration(&range) {
                            audit.report.summary.explained_regions += 1;
                        } else {
                            audit.gap(
                                range,
                                groups,
                                CoverageReason::UnsupportedDeclarator,
                                CoverageEvidence::Declarator,
                            );
                        }
                    } else {
                        audit.rejected_macro_ranges.push(range.clone());
                        audit.gap(
                            range,
                            groups,
                            CoverageReason::UnknownMacro,
                            CoverageEvidence::MacroInvocation,
                        );
                    }
                } else if kind == "macro_type_specifier" {
                    audit.gap(
                        range,
                        groups,
                        CoverageReason::UnknownMacro,
                        CoverageEvidence::MacroType,
                    );
                } else if kind == "expression_statement" {
                    audit.report.summary.non_declaration_regions += 1;
                } else {
                    let mut supported = audit.has_declaration(&range);
                    if language == SourceLanguage::C
                        && matches!(
                            kind,
                            "declaration"
                                | "type_definition"
                                | "field_declaration"
                                | "function_definition"
                        )
                    {
                        let decoded =
                            decode_direct_declarators(node, source, kind == "type_definition");
                        supported |= !decoded.entities.is_empty();
                        for failure in decoded.failures {
                            let reason = match failure.reason {
                                DeclaratorFailureReason::BudgetExhausted => {
                                    CoverageReason::BudgetExhausted
                                }
                                DeclaratorFailureReason::Malformed => {
                                    CoverageReason::SyntaxRecoveryFailed
                                }
                                DeclaratorFailureReason::Unsupported => {
                                    CoverageReason::UnsupportedDeclarator
                                }
                            };
                            audit.gap(failure.range, groups, reason, CoverageEvidence::Declarator);
                        }
                    }
                    // Record members and empty/tag-only declarations are
                    // structural facts even when declaration collection was not requested.
                    supported |= matches!(
                        kind,
                        "field_declaration" | "preproc_def" | "preproc_function_def"
                    ) || node.child_by_field_name("type").is_some_and(|ty| {
                        matches!(
                            ty.kind(),
                            "struct_specifier"
                                | "union_specifier"
                                | "enum_specifier"
                                | "class_specifier"
                        )
                    });
                    if supported {
                        audit.report.summary.explained_regions += 1;
                    } else {
                        audit.gap(
                            range,
                            groups,
                            CoverageReason::UnsupportedDeclarator,
                            CoverageEvidence::UnexplainedDeclaration,
                        );
                    }
                }
            }
        }
        if audit.stopped {
            break;
        }
        context.enter(node, source, lines);
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            context.exit(cursor.node());
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return audit.finish();
            }
        }
    }
    audit.finish()
}

fn bare_invocation<'a>(
    source: &'a str,
    lexical: &LexicalMap,
    range: Range<usize>,
) -> Option<&'a str> {
    let bytes = source.as_bytes();
    let next = |mut offset: usize| {
        while offset < range.end && (!lexical.code[offset] || bytes[offset].is_ascii_whitespace()) {
            offset += 1;
        }
        offset
    };
    let start = next(range.start);
    if start >= range.end || !(bytes[start].is_ascii_alphabetic() || bytes[start] == b'_') {
        return None;
    }
    let mut end = start + 1;
    while end < range.end && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
        end += 1;
    }
    if crate::language_builtins::is_language_keyword(&source[start..end]) {
        return None;
    }
    let open = next(end);
    if open >= range.end || bytes[open] != b'(' {
        return None;
    }
    let mut depth = 0usize;
    for offset in open..range.end.min(open.saturating_add(64 * 1024)) {
        if !lexical.code[offset] {
            continue;
        }
        match bytes[offset] {
            b'(' => {
                depth += 1;
                if depth > 64 {
                    return None;
                }
            }
            b')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let mut tail = next(offset + 1);
                    if tail < range.end && bytes[tail] == b';' {
                        tail = next(tail + 1);
                    }
                    return (tail == range.end).then_some(&source[start..end]);
                }
            }
            _ => {}
        }
    }
    None
}

pub(super) fn add_language_ambiguity(report: &mut DeclarationCoverage, source: &str) {
    let groups = report.summary.requested_groups & semantic_groups();
    if groups == 0 {
        return;
    }
    report
        .summary
        .note_gap(groups, CoverageReason::LanguageAmbiguity);
    report.summary.uncovered_bytes = report.summary.candidate_bytes;
    if report.gaps.len() >= limits().details.max(1) {
        report.summary.truncated = true;
        report
            .summary
            .note_gap(groups, CoverageReason::BudgetExhausted);
        report.gaps.pop();
        report.gaps.push(CoverageGap {
            range: source_range(source, &super::line_starts(source), 0..source.len()),
            groups,
            reason: CoverageReason::BudgetExhausted,
            evidence: CoverageEvidence::Limit,
            related_declarations: Vec::new(),
            recovery_rules: Vec::new(),
        });
        return;
    }
    report.gaps.push(CoverageGap {
        range: source_range(source, &super::line_starts(source), 0..source.len()),
        groups,
        reason: CoverageReason::LanguageAmbiguity,
        evidence: CoverageEvidence::LanguageSelection,
        related_declarations: Vec::new(),
        recovery_rules: Vec::new(),
    });
}

pub(super) fn unknown(source: &str, facts: ParseFacts, fallback: bool) -> DeclarationCoverage {
    let requested = requested_groups(facts, fallback);
    let mut report = DeclarationCoverage {
        summary: DeclarationCoverageSummary {
            requested_groups: requested,
            checked_groups: requested & !semantic_groups(),
            ..Default::default()
        },
        gaps: Vec::new(),
    };
    if fallback && !source.is_empty() {
        report.summary.note_gap(
            requested & semantic_groups(),
            CoverageReason::SyntaxRecoveryFailed,
        );
        report.gaps.push(CoverageGap {
            range: source_range(source, &super::line_starts(source), 0..source.len()),
            groups: requested & semantic_groups(),
            reason: CoverageReason::SyntaxRecoveryFailed,
            evidence: CoverageEvidence::Syntax,
            related_declarations: Vec::new(),
            recovery_rules: Vec::new(),
        });
    }
    report
}
