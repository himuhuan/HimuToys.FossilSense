use crate::c_lexical::LexicalMap;
use std::ops::Range;

mod health;

use health::alignment_decorator_range;
pub(super) use health::{attach_affected_ranges, healthy_observations_unchanged};

pub(super) const MAX_RECOVERY_REPARSES: usize = 2;
pub(super) const MAX_RECOVERY_EDITS: usize = 256;
pub(super) const MAX_RECOVERY_REGION_BYTES: usize = 64 * 1024;
pub(super) const MAX_RECOVERY_PAREN_DEPTH: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryRule {
    ProtobufCMarker,
    ProtobufCExport,
    AlignmentAttribute,
    CallingConvention,
    ConditionalInitializer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryFailureReason {
    ConflictingEdit,
    InvalidRange,
    UnsafeLexicalBoundary,
    UnsafeDeclarationBoundary,
    EditBudgetExceeded,
    RegionBudgetExceeded,
    ParenthesisDepthExceeded,
    ReparseBudgetExceeded,
    ParseFailed,
    HealthCheckFailed,
    Cancelled,
}

impl RecoveryFailureReason {
    fn is_budget(self) -> bool {
        matches!(
            self,
            Self::EditBudgetExceeded
                | Self::RegionBudgetExceeded
                | Self::ParenthesisDepthExceeded
                | Self::ReparseBudgetExceeded
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryOutcome {
    Applied,
    Rejected(RecoveryFailureReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryMapping {
    Identity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryDiagnostic {
    pub start_byte: usize,
    pub end_byte: usize,
    pub affected_start_byte: usize,
    pub affected_end_byte: usize,
    pub rule: RecoveryRule,
    pub mapping: RecoveryMapping,
    pub outcome: RecoveryOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProposedEdit {
    pub range: Range<usize>,
    pub affected_range: Range<usize>,
    pub rule: RecoveryRule,
}

impl ProposedEdit {
    pub(super) fn new(range: Range<usize>, rule: RecoveryRule) -> Self {
        Self::with_affected_range(range.clone(), range, rule)
    }

    pub(super) fn with_affected_range(
        range: Range<usize>,
        affected_range: Range<usize>,
        rule: RecoveryRule,
    ) -> Self {
        Self {
            range,
            affected_range,
            rule,
        }
    }

    fn diagnostic(&self, outcome: RecoveryOutcome) -> RecoveryDiagnostic {
        RecoveryDiagnostic {
            start_byte: self.range.start,
            end_byte: self.range.end,
            affected_start_byte: self.affected_range.start,
            affected_end_byte: self.affected_range.end,
            rule: self.rule,
            mapping: RecoveryMapping::Identity,
            outcome,
        }
    }
}

#[derive(Debug)]
pub(super) struct RecoveryTransaction {
    source: String,
    edits: Vec<ProposedEdit>,
}

impl RecoveryTransaction {
    pub(super) fn source(&self) -> &str {
        &self.source
    }

    pub(super) fn affected_ranges(&self) -> impl Iterator<Item = &Range<usize>> {
        self.edits.iter().map(|edit| &edit.affected_range)
    }
}

pub(super) struct RecoverySession<'source> {
    original: &'source str,
    current: Vec<u8>,
    lexical: LexicalMap,
    applied: Vec<ProposedEdit>,
    diagnostics: Vec<RecoveryDiagnostic>,
    reparses: usize,
    budget_exhausted: bool,
}

impl<'source> RecoverySession<'source> {
    pub(super) fn new(original: &'source str, is_cpp: bool) -> Self {
        Self {
            original,
            current: original.as_bytes().to_vec(),
            lexical: LexicalMap::new(original, is_cpp),
            applied: Vec::new(),
            diagnostics: Vec::new(),
            reparses: 0,
            budget_exhausted: false,
        }
    }

    pub(super) fn source(&self) -> &str {
        std::str::from_utf8(&self.current).expect("equal-length masking preserves UTF-8")
    }

    pub(super) fn alignment_attribute_edits(&self, root: tree_sitter::Node<'_>) -> EditDiscovery {
        alignment_attribute_edits_with_map(self.source(), &self.lexical, Some(root))
    }

    pub(super) fn prepare(
        &mut self,
        proposals: Vec<ProposedEdit>,
    ) -> Result<RecoveryTransaction, RecoveryFailureReason> {
        let mut edits = Vec::new();
        for proposal in proposals {
            if self
                .applied
                .iter()
                .chain(&edits)
                .any(|edit| edit.range == proposal.range && edit.rule == proposal.rule)
            {
                continue;
            }
            if proposal.range.start >= proposal.range.end
                || proposal.range.end > self.original.len()
                || proposal.affected_range.start > proposal.range.start
                || proposal.affected_range.end < proposal.range.end
                || proposal.affected_range.end > self.original.len()
            {
                return self.fail_prepare(&[proposal], RecoveryFailureReason::InvalidRange);
            }
            if proposal.affected_range.len() > MAX_RECOVERY_REGION_BYTES {
                return self.fail_prepare(&[proposal], RecoveryFailureReason::RegionBudgetExceeded);
            }
            if !self.lexical.edit_is_allowed(&proposal) {
                return self
                    .fail_prepare(&[proposal], RecoveryFailureReason::UnsafeLexicalBoundary);
            }
            if self
                .applied
                .iter()
                .chain(&edits)
                .any(|edit| ranges_overlap(&edit.range, &proposal.range))
            {
                return self.fail_prepare(&[proposal], RecoveryFailureReason::ConflictingEdit);
            }
            edits.push(proposal);
        }

        if self.applied.len() + edits.len() > MAX_RECOVERY_EDITS {
            return self.fail_prepare(&edits, RecoveryFailureReason::EditBudgetExceeded);
        }
        if self.reparses >= MAX_RECOVERY_REPARSES {
            return self.fail_prepare(&edits, RecoveryFailureReason::ReparseBudgetExceeded);
        }

        let mut source = self.current.clone();
        for edit in &edits {
            mask_non_newlines(&mut source, edit.range.clone());
        }
        self.reparses += 1;
        Ok(RecoveryTransaction {
            source: String::from_utf8(source).expect("equal-length masking preserves UTF-8"),
            edits,
        })
    }

    fn fail_prepare<T>(
        &mut self,
        proposals: &[ProposedEdit],
        reason: RecoveryFailureReason,
    ) -> Result<T, RecoveryFailureReason> {
        self.record_rejections(proposals, reason);
        Err(reason)
    }

    pub(super) fn commit(&mut self, transaction: RecoveryTransaction) {
        self.current = transaction.source.into_bytes();
        for edit in transaction.edits {
            self.diagnostics
                .push(edit.diagnostic(RecoveryOutcome::Applied));
            self.applied.push(edit);
        }
    }

    pub(super) fn reject(
        &mut self,
        transaction: RecoveryTransaction,
        reason: RecoveryFailureReason,
    ) {
        self.record_rejections(&transaction.edits, reason);
    }

    pub(super) fn record_discovery_failures(&mut self, failures: &[DiscoveryFailure]) {
        for failure in failures {
            if failure.reason.is_budget() {
                self.budget_exhausted = true;
            }
            self.diagnostics.push(RecoveryDiagnostic {
                start_byte: failure.range.start,
                end_byte: failure.range.end,
                affected_start_byte: failure.range.start,
                affected_end_byte: failure.range.end,
                rule: failure.rule,
                mapping: RecoveryMapping::Identity,
                outcome: RecoveryOutcome::Rejected(failure.reason),
            });
        }
    }

    fn record_rejections(&mut self, proposals: &[ProposedEdit], reason: RecoveryFailureReason) {
        if reason.is_budget() {
            self.budget_exhausted = true;
        }
        for edit in proposals {
            self.diagnostics
                .push(edit.diagnostic(RecoveryOutcome::Rejected(reason)));
        }
    }

    pub(super) fn applied_edits(&self) -> &[ProposedEdit] {
        &self.applied
    }

    #[cfg(test)]
    pub(super) fn budget_exhausted(&self) -> bool {
        self.budget_exhausted
    }

    pub(super) fn finish(self) -> (Vec<RecoveryDiagnostic>, bool) {
        (self.diagnostics, self.budget_exhausted)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DiscoveryFailure {
    pub(super) range: Range<usize>,
    pub(super) rule: RecoveryRule,
    pub(super) reason: RecoveryFailureReason,
}

impl DiscoveryFailure {
    pub(super) fn new(
        range: Range<usize>,
        rule: RecoveryRule,
        reason: RecoveryFailureReason,
    ) -> Self {
        Self {
            range,
            rule,
            reason,
        }
    }
}

pub(super) struct EditDiscovery {
    pub edits: Vec<ProposedEdit>,
    pub failures: Vec<DiscoveryFailure>,
}

#[cfg(test)]
pub(super) fn alignment_attribute_edits(source: &str, is_cpp: bool) -> EditDiscovery {
    let lexical = LexicalMap::new(source, is_cpp);
    alignment_attribute_edits_with_map(source, &lexical, None)
}

fn alignment_attribute_edits_with_map(
    source: &str,
    lexical: &LexicalMap,
    root: Option<tree_sitter::Node<'_>>,
) -> EditDiscovery {
    let bytes = source.as_bytes();
    let mut edits = Vec::new();
    let mut failures = Vec::new();

    'attributes: for (start, attribute) in source.match_indices("__aligned") {
        let end = start + attribute.len();
        if !lexical
            .code
            .get(start..end)
            .is_some_and(|state| state.iter().all(|code| *code))
            || lexical.preprocessor.get(start).copied().unwrap_or(true)
            || bytes
                .get(start.wrapping_sub(1))
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            || bytes
                .get(end)
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            continue;
        }

        let Some(mut index) = lexical.next_code_token(bytes, end) else {
            continue;
        };
        if bytes[index] != b'(' {
            continue;
        }

        let mut depth = 0usize;
        let mut failure = None;
        while index < bytes.len() {
            if index.saturating_sub(start) > MAX_RECOVERY_REGION_BYTES {
                failure = Some(RecoveryFailureReason::RegionBudgetExceeded);
                break;
            }
            if lexical.preprocessor[index] {
                failure = Some(RecoveryFailureReason::UnsafeLexicalBoundary);
                break;
            }
            if !lexical.code[index] {
                index += 1;
                continue;
            }
            match bytes[index] {
                b'(' => {
                    depth += 1;
                    if depth > MAX_RECOVERY_PAREN_DEPTH {
                        failure = Some(RecoveryFailureReason::ParenthesisDepthExceeded);
                        break;
                    }
                }
                b')' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        let range = start..index + 1;
                        let affected = if let Some(root) = root {
                            let Some(affected) = alignment_decorator_range(root, source, &range)
                            else {
                                continue 'attributes;
                            };
                            affected
                        } else {
                            range.clone()
                        };
                        if edits.len() >= MAX_RECOVERY_EDITS {
                            failures.push(DiscoveryFailure {
                                range,
                                rule: RecoveryRule::AlignmentAttribute,
                                reason: RecoveryFailureReason::EditBudgetExceeded,
                            });
                            break 'attributes;
                        }
                        edits.push(ProposedEdit::with_affected_range(
                            range,
                            affected,
                            RecoveryRule::AlignmentAttribute,
                        ));
                        break;
                    }
                }
                b';' | b'{' | b'}' => {
                    failure = Some(RecoveryFailureReason::UnsafeDeclarationBoundary);
                    break;
                }
                _ => {}
            }
            index += 1;
        }
        if depth != 0 && failure.is_none() {
            failure = Some(RecoveryFailureReason::UnsafeDeclarationBoundary);
        }
        if let Some(reason) = failure {
            failures.push(DiscoveryFailure {
                range: start..index.min(bytes.len()),
                rule: RecoveryRule::AlignmentAttribute,
                reason,
            });
        }
    }

    EditDiscovery { edits, failures }
}

impl LexicalMap {
    fn edit_is_allowed(&self, edit: &ProposedEdit) -> bool {
        if edit.rule == RecoveryRule::ConditionalInitializer {
            return true;
        }
        self.code.get(edit.range.start).copied().unwrap_or(false)
            && !self
                .preprocessor
                .get(edit.range.clone())
                .is_some_and(|states| states.iter().any(|state| *state))
    }
}

fn mask_non_newlines(bytes: &mut [u8], range: Range<usize>) {
    for byte in &mut bytes[range] {
        if !matches!(*byte, b'\r' | b'\n') {
            *byte = b' ';
        }
    }
}

fn ranges_overlap(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_session_rejects_edit_region_and_reparse_budget_overruns() {
        let source = "x".repeat(MAX_RECOVERY_REGION_BYTES + 2);
        let mut session = RecoverySession::new(&source, false);
        let edits = (0..=MAX_RECOVERY_EDITS)
            .map(|start| ProposedEdit::new(start..start + 1, RecoveryRule::AlignmentAttribute))
            .collect();
        assert_eq!(
            session.prepare(edits).expect_err("257 edits must fail"),
            RecoveryFailureReason::EditBudgetExceeded
        );
        assert_eq!(session.source(), source);

        assert_eq!(
            session
                .prepare(vec![ProposedEdit::with_affected_range(
                    0..1,
                    0..MAX_RECOVERY_REGION_BYTES + 1,
                    RecoveryRule::AlignmentAttribute,
                )])
                .expect_err("oversized affected region must fail"),
            RecoveryFailureReason::RegionBudgetExceeded
        );

        let one = vec![ProposedEdit::new(0..1, RecoveryRule::AlignmentAttribute)];
        for _ in 0..MAX_RECOVERY_REPARSES {
            let transaction = session.prepare(one.clone()).expect("within budget");
            session.reject(transaction, RecoveryFailureReason::ParseFailed);
        }
        assert_eq!(
            session.prepare(one).expect_err("third reparse must fail"),
            RecoveryFailureReason::ReparseBudgetExceeded
        );
        assert!(session.budget_exhausted());
    }

    #[test]
    fn recovery_session_rejects_conflicts_and_abandoned_transactions_do_not_commit() {
        let source = "__aligned(8) value;";
        let mut session = RecoverySession::new(source, false);
        let conflict = vec![
            ProposedEdit::new(0..12, RecoveryRule::AlignmentAttribute),
            ProposedEdit::new(2..6, RecoveryRule::CallingConvention),
        ];
        assert_eq!(
            session.prepare(conflict).expect_err("overlap must fail"),
            RecoveryFailureReason::ConflictingEdit
        );
        assert_eq!(session.source(), source);

        let transaction = session
            .prepare(vec![ProposedEdit::new(
                0..12,
                RecoveryRule::AlignmentAttribute,
            )])
            .expect("valid transaction");
        session.reject(transaction, RecoveryFailureReason::Cancelled);
        assert_eq!(session.source(), source);
        assert!(session.applied_edits().is_empty());
    }

    #[test]
    fn alignment_discovery_rejects_excessive_parenthesis_depth() {
        let source = format!(
            "int value __aligned({});",
            "(".repeat(MAX_RECOVERY_PAREN_DEPTH + 1)
        );
        let discovery = alignment_attribute_edits(&source, false);
        assert!(discovery.edits.is_empty());
        assert!(discovery
            .failures
            .iter()
            .any(|failure| { failure.reason == RecoveryFailureReason::ParenthesisDepthExceeded }));
    }

    #[test]
    fn alignment_discovery_stops_materializing_edits_at_the_shared_cap() {
        let source = (0..=MAX_RECOVERY_EDITS)
            .map(|index| format!("int value_{index} __aligned(8);\n"))
            .collect::<String>();
        let discovery = alignment_attribute_edits(&source, false);

        assert_eq!(discovery.edits.len(), MAX_RECOVERY_EDITS);
        assert!(discovery
            .failures
            .iter()
            .any(|failure| { failure.reason == RecoveryFailureReason::EditBudgetExceeded }));
    }

    #[test]
    fn lexical_discovery_ignores_comments_literals_raw_strings_and_preprocessor_lines() {
        let source = "/* __aligned(1) */\n\
const char *text = \"__aligned(2)\";\n\
const char *raw = R\"x(__aligned(3))x\";\n\
#define ATTR __aligned( \\\n  4)\n\
int value __aligned(8);\n";
        let discovery = alignment_attribute_edits(source, true);
        assert_eq!(discovery.edits.len(), 1);
        assert_eq!(&source[discovery.edits[0].range.clone()], "__aligned(8)");
        assert!(discovery.failures.is_empty());
    }

    #[test]
    fn masking_preserves_length_and_every_cr_lf_offset() {
        let source = "int value __aligned(\r\n  8\r\n);\r\n";
        let discovery = alignment_attribute_edits(source, false);
        let mut session = RecoverySession::new(source, false);
        let transaction = session.prepare(discovery.edits).expect("valid edit");
        assert_eq!(transaction.source().len(), source.len());
        for (offset, byte) in source.bytes().enumerate() {
            if matches!(byte, b'\r' | b'\n') {
                assert_eq!(transaction.source().as_bytes()[offset], byte);
            }
        }
    }
}

#[cfg(test)]
#[path = "recovery/health_tests.rs"]
mod health_tests;
