//! Protocol-neutral record and typedef candidate resolution.
//!
//! This module consumes already-recalled, generation-pinned facts.  It does
//! not read SQLite, source files, or dirty buffers itself.  Base and overlay
//! facts are normalized into stable candidate identities before entering this
//! layer, so the ranking, uniqueness, and alias-trace rules are shared by
//! Hover and Definition without either consumer inventing semantic policy.

use std::collections::{HashMap, HashSet, VecDeque};

use super::callables::{CandidateCoverage, CandidateIncompleteReason};
use crate::call_model::{SourcePosition, SourceRange};
use crate::model::ScopeTier;
use crate::semantic_model::{
    AliasTarget, AliasTargetFidelity, DeclaratorShape, RecordConfidence, RecordDef, RecordKind,
    RecordRangeFidelity, TypeAlias,
};
use crate::store::views::{RecordReadRow, TypeAliasReadRow};

pub const TYPE_CANDIDATE_LIMIT: usize = 128;
pub const ALIAS_RESOLUTION_MAX_DEPTH: usize = 16;
pub const ALIAS_RESOLUTION_MAX_VISITS: usize = 128;

/// Stable identity for a record candidate within one request snapshot.
///
/// SQLite IDs are generation-local but stable for the lifetime of the pinned
/// request.  Dirty facts have no SQLite ID, so their parser record key is
/// scoped by path.  Upstream overlay shadowing removes the base fact for that
/// path before these identities are compared.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RecordCandidateIdentity {
    Persistent(i64),
    ParserKey { path: String, record_key: String },
}

/// Stable identity for one typedef declarator.
///
/// The fingerprint is declarator-specific (rather than name-specific), which
/// is essential for both multi-declarator typedefs and cycle detection.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TypeAliasCandidateIdentity {
    Persistent { id: i64, fingerprint: String },
    ParserFingerprint { path: String, fingerprint: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateRevision {
    pub id: i64,
    pub size: u64,
    pub mtime_ns: i64,
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordCandidate {
    pub identity: RecordCandidateIdentity,
    pub display_name: String,
    pub tag_name: Option<String>,
    pub typedef_name: Option<String>,
    pub kind: RecordKind,
    pub path: String,
    pub name_range: SourceRange,
    pub body_range: SourceRange,
    pub declaration_range: SourceRange,
    pub declaration_hash: [u8; 32],
    pub range_fidelity: RecordRangeFidelity,
    pub confidence: RecordConfidence,
    pub signature: String,
    pub tier: ScopeTier,
    /// Present for a durable fact and absent for a current-buffer fact.
    pub revision: Option<CandidateRevision>,
}

impl RecordCandidate {
    pub fn from_read_row(row: RecordReadRow, tier: ScopeTier) -> Self {
        let name_range = SourceRange {
            start: SourcePosition {
                line: row.start_line as u32,
                character: row.start_col as u32,
            },
            end: SourcePosition {
                line: row.end_line as u32,
                character: row.end_col as u32,
            },
            start_byte: row.start_byte,
            end_byte: row.end_byte,
        };
        Self {
            identity: RecordCandidateIdentity::Persistent(row.id),
            display_name: row.display_name,
            tag_name: row.tag_name,
            typedef_name: row.typedef_name,
            kind: row.kind,
            path: row.path,
            name_range,
            body_range: row.body_range,
            declaration_range: row.declaration_range,
            declaration_hash: row.declaration_hash,
            range_fidelity: row.range_fidelity,
            confidence: row.confidence,
            signature: row.signature,
            tier,
            revision: Some(CandidateRevision {
                id: row.revision_id,
                size: row.revision_size,
                mtime_ns: row.revision_mtime_ns,
                hash: row.revision_hash,
            }),
        }
    }

    pub fn from_overlay(path: String, record: RecordDef, tier: ScopeTier) -> Self {
        let name_range = source_range_from_parts(
            record.start_byte,
            record.end_byte,
            record.start_line,
            record.start_col,
            record.end_line,
            record.end_col,
        );
        Self {
            identity: RecordCandidateIdentity::ParserKey {
                path: path.clone(),
                record_key: record.record_key,
            },
            display_name: record.display_name,
            tag_name: record.tag_name,
            typedef_name: record.typedef_name,
            kind: record.kind,
            path,
            name_range,
            body_range: record.body_range,
            declaration_range: record.declaration_range,
            declaration_hash: record.declaration_hash,
            range_fidelity: record.range_fidelity,
            confidence: record.confidence,
            signature: record.signature,
            tier,
            revision: None,
        }
    }

    pub fn matches_exact_name(&self, name: &str) -> bool {
        self.display_name == name
            || self.tag_name.as_deref() == Some(name)
            || self.typedef_name.as_deref() == Some(name)
    }

    fn named_tag_matches(&self, tag: &str, kind: RecordKind) -> bool {
        self.tag_name.as_deref() == Some(tag) && self.kind == kind
    }

    fn type_spelling(&self) -> String {
        let keyword = record_kind_keyword(self.kind);
        match self.tag_name.as_deref() {
            Some(tag) => format!("{keyword} {tag}"),
            None => format!("anonymous {keyword}"),
        }
    }
}

/// Query-normalized alias target.  A direct record reference retains stable
/// identity; a named tag retains its record namespace and kind; and a bare
/// type name is resolved only through exact-name typedef candidates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeAliasTarget {
    StableRecord(RecordCandidateIdentity),
    NamedRecord { tag: String, kind: RecordKind },
    TypeName(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeAliasCandidate {
    pub identity: TypeAliasCandidateIdentity,
    pub alias: String,
    pub target: TypeAliasTarget,
    pub path: String,
    pub name_range: SourceRange,
    pub declaration_range: SourceRange,
    pub declaration_hash: [u8; 32],
    pub underlying_spelling: String,
    pub declarator_shape: DeclaratorShape,
    pub target_fidelity: AliasTargetFidelity,
    pub fingerprint: String,
    pub tier: ScopeTier,
    /// Present for a durable fact and absent for a current-buffer fact.
    pub revision: Option<CandidateRevision>,
}

impl TypeAliasCandidate {
    pub fn from_read_row(row: TypeAliasReadRow, tier: ScopeTier) -> Option<Self> {
        let target = if let Some(record_id) = row.target_record_id {
            TypeAliasTarget::StableRecord(RecordCandidateIdentity::Persistent(record_id))
        } else if let (Some(tag), Some(kind)) = (row.target_name.clone(), row.target_kind) {
            TypeAliasTarget::NamedRecord { tag, kind }
        } else if let Some(name) = row.target_name.clone() {
            TypeAliasTarget::TypeName(name)
        } else {
            // Keep the declaration candidate even when a malformed durable
            // row no longer has a usable target. The empty exact-name lookup
            // deterministically resolves to `Unresolved` without inventing a
            // record or dropping the typedef's own Hover identity.
            TypeAliasTarget::TypeName(String::new())
        };
        Some(Self {
            identity: TypeAliasCandidateIdentity::Persistent {
                id: row.id,
                fingerprint: row.fingerprint.clone(),
            },
            alias: row.alias,
            target,
            path: row.path,
            name_range: row.name_range,
            declaration_range: row.declaration_range,
            declaration_hash: row.declaration_hash,
            underlying_spelling: row.underlying_spelling,
            declarator_shape: row.declarator_shape,
            target_fidelity: row.target_fidelity,
            fingerprint: row.fingerprint,
            tier,
            revision: Some(CandidateRevision {
                id: row.revision_id,
                size: row.revision_size,
                mtime_ns: row.revision_mtime_ns,
                hash: row.revision_hash,
            }),
        })
    }

    pub fn from_overlay(path: String, alias: TypeAlias, tier: ScopeTier) -> Self {
        let target = match alias.target {
            AliasTarget::RecordKey(record_key) => {
                TypeAliasTarget::StableRecord(RecordCandidateIdentity::ParserKey {
                    path: path.clone(),
                    record_key,
                })
            }
            AliasTarget::NamedRecord { tag, kind } => TypeAliasTarget::NamedRecord { tag, kind },
            AliasTarget::UnresolvedTypeName(name) => TypeAliasTarget::TypeName(name),
        };
        let name_range = source_range_from_parts(
            alias.start_byte,
            alias.end_byte,
            alias.start_line,
            alias.start_col,
            alias.end_line,
            alias.end_col,
        );
        Self {
            identity: TypeAliasCandidateIdentity::ParserFingerprint {
                path: path.clone(),
                fingerprint: alias.fingerprint.clone(),
            },
            alias: alias.alias,
            target,
            path,
            name_range,
            declaration_range: alias.declaration_range,
            declaration_hash: alias.declaration_hash,
            underlying_spelling: alias.underlying_spelling,
            declarator_shape: alias.declarator_shape,
            target_fidelity: alias.target_fidelity,
            fingerprint: alias.fingerprint,
            tier,
            revision: None,
        }
    }

    pub fn matches_exact_name(&self, name: &str) -> bool {
        self.alias == name
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordCandidateSet {
    pub candidates: Vec<RecordCandidate>,
    pub coverage: CandidateCoverage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeAliasCandidateSet {
    pub candidates: Vec<TypeAliasCandidate>,
    pub coverage: CandidateCoverage,
}

/// Keep only exact-name record candidates, rank by scope evidence, deduplicate
/// by stable identity, and enforce a deterministic working-set bound.
pub fn record_candidates_exact(
    name: &str,
    candidates: Vec<RecordCandidate>,
    coverage: CandidateCoverage,
    limit: usize,
) -> RecordCandidateSet {
    let inspected = candidates.len();
    let mut candidates: Vec<_> = candidates
        .into_iter()
        .filter(|candidate| candidate.matches_exact_name(name))
        .collect();
    sort_record_candidates(&mut candidates);
    let mut identities = HashSet::new();
    candidates.retain(|candidate| identities.insert(candidate.identity.clone()));
    let coverage = enforce_bound(coverage, inspected, candidates.len(), limit);
    candidates.truncate(limit);
    RecordCandidateSet {
        candidates,
        coverage,
    }
}

/// Keep only exact-name typedef candidates, rank by scope evidence,
/// deduplicate per declarator fingerprint/id, and enforce a deterministic
/// working-set bound.
pub fn type_alias_candidates_exact(
    name: &str,
    candidates: Vec<TypeAliasCandidate>,
    coverage: CandidateCoverage,
    limit: usize,
) -> TypeAliasCandidateSet {
    let inspected = candidates.len();
    let mut candidates: Vec<_> = candidates
        .into_iter()
        .filter(|candidate| candidate.matches_exact_name(name))
        .collect();
    sort_alias_candidates(&mut candidates);
    let mut identities = HashSet::new();
    candidates.retain(|candidate| identities.insert(candidate.identity.clone()));
    let coverage = enforce_bound(coverage, inspected, candidates.len(), limit);
    candidates.truncate(limit);
    TypeAliasCandidateSet {
        candidates,
        coverage,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AliasResolutionLimits {
    pub max_depth: usize,
    pub max_visits: usize,
}

impl Default for AliasResolutionLimits {
    fn default() -> Self {
        Self {
            max_depth: ALIAS_RESOLUTION_MAX_DEPTH,
            max_visits: ALIAS_RESOLUTION_MAX_VISITS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AliasResolutionStatus {
    UniqueRecord,
    AmbiguousRecord,
    Unresolved,
    Cycle,
    UnsupportedDeclarator,
    /// The resolver or an upstream candidate query could not inspect the
    /// complete uniqueness domain.  This also covers an open reach scope: the
    /// known terminal may be useful, but it is not safe to claim uniqueness.
    Truncated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasResolution {
    pub alias: TypeAliasCandidate,
    /// Deterministic breadth-first trace.  The root alias is included as the
    /// first entry; branching exact-name candidates follow in ranked order.
    pub chain: Vec<TypeAliasCandidate>,
    pub terminal_records: Vec<RecordCandidate>,
    pub winning_tier: Option<ScopeTier>,
    pub aka_spelling: Option<String>,
    pub status: AliasResolutionStatus,
    pub coverage: CandidateCoverage,
}

/// Resolve an alias over an already bounded candidate snapshot.
pub fn resolve_type_alias(
    alias: TypeAliasCandidate,
    aliases: &[TypeAliasCandidate],
    records: &[RecordCandidate],
    coverage: CandidateCoverage,
) -> AliasResolution {
    resolve_type_alias_with_limits(
        alias,
        aliases,
        records,
        coverage,
        AliasResolutionLimits::default(),
    )
}

pub fn resolve_type_alias_with_limits(
    alias: TypeAliasCandidate,
    aliases: &[TypeAliasCandidate],
    records: &[RecordCandidate],
    mut coverage: CandidateCoverage,
    limits: AliasResolutionLimits,
) -> AliasResolution {
    let mut queue = VecDeque::new();
    let mut root_seen = HashSet::new();
    root_seen.insert(alias.identity.clone());
    queue.push_back(Branch {
        current: alias.clone(),
        trace: vec![alias.clone()],
        effective_tier: alias.tier,
        visited: root_seen,
        exact_target: alias.target_fidelity == AliasTargetFidelity::AstExact,
    });

    let mut chain = Vec::new();
    let mut chain_seen = HashSet::new();
    let mut terminals = Vec::new();
    let mut issues = Vec::new();
    let mut visits = 0usize;

    while let Some(branch) = queue.pop_front() {
        if visits >= limits.max_visits {
            mark_resolution_truncated(&mut coverage);
            issues.push(ResolutionIssue {
                tier: branch.effective_tier,
                kind: ResolutionIssueKind::Truncated,
            });
            break;
        }
        visits += 1;

        if chain_seen.insert(branch.current.identity.clone()) {
            chain.push(branch.current.clone());
        }

        match &branch.current.target {
            TypeAliasTarget::StableRecord(identity) => {
                let matches: Vec<_> = records
                    .iter()
                    .filter(|record| &record.identity == identity)
                    .cloned()
                    .collect();
                if matches.is_empty() {
                    issues.push(ResolutionIssue {
                        tier: branch.effective_tier,
                        kind: ResolutionIssueKind::Unresolved,
                    });
                } else {
                    push_terminal_matches(&mut terminals, matches, &branch);
                }
            }
            TypeAliasTarget::NamedRecord { tag, kind } => {
                let matches: Vec<_> = records
                    .iter()
                    .filter(|record| record.named_tag_matches(tag, *kind))
                    .cloned()
                    .collect();
                if matches.is_empty() {
                    issues.push(ResolutionIssue {
                        tier: branch.effective_tier,
                        kind: ResolutionIssueKind::Unresolved,
                    });
                } else {
                    push_terminal_matches(&mut terminals, matches, &branch);
                }
            }
            TypeAliasTarget::TypeName(name) => {
                if branch.trace.len() >= limits.max_depth {
                    mark_resolution_truncated(&mut coverage);
                    issues.push(ResolutionIssue {
                        tier: branch.effective_tier,
                        kind: ResolutionIssueKind::Truncated,
                    });
                    continue;
                }

                let mut next: Vec<_> = aliases
                    .iter()
                    .filter(|candidate| candidate.alias == *name)
                    .cloned()
                    .collect();
                sort_alias_candidates(&mut next);
                let mut identities = HashSet::new();
                next.retain(|candidate| identities.insert(candidate.identity.clone()));
                if let Some(strongest) = next.first().map(|candidate| candidate.tier) {
                    next.retain(|candidate| candidate.tier == strongest);
                }
                if next.is_empty() {
                    issues.push(ResolutionIssue {
                        tier: branch.effective_tier,
                        kind: ResolutionIssueKind::Unresolved,
                    });
                    continue;
                }

                // Multiple same-tier alias definitions are not a proof of one
                // logical target, even if they happen to converge later.
                if next.len() > 1 {
                    issues.push(ResolutionIssue {
                        tier: min_tier(branch.effective_tier, next[0].tier),
                        kind: ResolutionIssueKind::Ambiguous,
                    });
                }

                for candidate in next {
                    let effective_tier = min_tier(branch.effective_tier, candidate.tier);
                    if branch.visited.contains(&candidate.identity) {
                        issues.push(ResolutionIssue {
                            tier: effective_tier,
                            kind: ResolutionIssueKind::Cycle,
                        });
                        continue;
                    }
                    let mut visited = branch.visited.clone();
                    visited.insert(candidate.identity.clone());
                    let mut trace = branch.trace.clone();
                    trace.push(candidate.clone());
                    let exact_target = branch.exact_target
                        && candidate.target_fidelity == AliasTargetFidelity::AstExact;
                    queue.push_back(Branch {
                        current: candidate,
                        trace,
                        effective_tier,
                        visited,
                        exact_target,
                    });
                }
            }
        }
    }

    let terminals = deduplicate_terminals(terminals);
    let winning_tier = terminals.iter().map(|terminal| terminal.record.tier).max();
    let winning: Vec<_> = winning_tier
        .into_iter()
        .flat_map(|tier| {
            terminals
                .iter()
                .filter(move |terminal| terminal.record.tier == tier)
        })
        .collect();
    let relevant_issues: Vec<_> = issues
        .iter()
        .filter(|issue| winning_tier.is_none_or(|tier| issue.tier >= tier))
        .collect();

    let status = resolution_status(&coverage, &winning, &relevant_issues);
    let aka_spelling = if status == AliasResolutionStatus::UniqueRecord {
        winning.first().and_then(|terminal| terminal.aka.clone())
    } else {
        None
    };
    let terminal_records = terminals
        .into_iter()
        .map(|terminal| terminal.record)
        .collect();

    AliasResolution {
        alias,
        chain,
        terminal_records,
        winning_tier,
        aka_spelling,
        status,
        coverage,
    }
}

#[derive(Clone)]
struct Branch {
    current: TypeAliasCandidate,
    trace: Vec<TypeAliasCandidate>,
    effective_tier: ScopeTier,
    visited: HashSet<TypeAliasCandidateIdentity>,
    exact_target: bool,
}

#[derive(Debug, Clone)]
struct Terminal {
    record: RecordCandidate,
    aka: Option<String>,
    unsupported_shape: bool,
    inexact_target: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolutionIssueKind {
    Ambiguous,
    Unresolved,
    Cycle,
    Truncated,
}

#[derive(Debug, Clone, Copy)]
struct ResolutionIssue {
    tier: ScopeTier,
    kind: ResolutionIssueKind,
}

fn push_terminal_matches(
    terminals: &mut Vec<Terminal>,
    matches: Vec<RecordCandidate>,
    branch: &Branch,
) {
    for mut record in matches {
        record.tier = min_tier(branch.effective_tier, record.tier);
        let aka = if branch.exact_target {
            compose_aka_spelling(&record, &branch.trace)
        } else {
            None
        };
        terminals.push(Terminal {
            record,
            unsupported_shape: aka.is_none()
                && branch
                    .trace
                    .iter()
                    .any(|alias| matches!(alias.declarator_shape, DeclaratorShape::Unsupported)),
            inexact_target: !branch.exact_target,
            aka,
        });
    }
}

fn deduplicate_terminals(terminals: Vec<Terminal>) -> Vec<Terminal> {
    let mut by_identity: HashMap<RecordCandidateIdentity, Terminal> = HashMap::new();
    for terminal in terminals {
        match by_identity.get_mut(&terminal.record.identity) {
            None => {
                by_identity.insert(terminal.record.identity.clone(), terminal);
            }
            Some(existing) if terminal.record.tier > existing.record.tier => {
                *existing = terminal;
            }
            Some(existing) if terminal.record.tier == existing.record.tier => {
                if existing.aka != terminal.aka {
                    existing.aka = None;
                    existing.unsupported_shape = true;
                }
                existing.unsupported_shape |= terminal.unsupported_shape;
                existing.inexact_target |= terminal.inexact_target;
            }
            Some(_) => {}
        }
    }
    let mut terminals: Vec<_> = by_identity.into_values().collect();
    terminals.sort_by(|a, b| compare_record_candidates(&a.record, &b.record));
    terminals
}

fn resolution_status(
    coverage: &CandidateCoverage,
    winning: &[&Terminal],
    issues: &[&ResolutionIssue],
) -> AliasResolutionStatus {
    if !coverage.permits_uniqueness()
        || issues
            .iter()
            .any(|issue| issue.kind == ResolutionIssueKind::Truncated)
    {
        return AliasResolutionStatus::Truncated;
    }
    if issues
        .iter()
        .any(|issue| issue.kind == ResolutionIssueKind::Cycle)
    {
        return AliasResolutionStatus::Cycle;
    }
    if winning.len() > 1
        || issues
            .iter()
            .any(|issue| issue.kind == ResolutionIssueKind::Ambiguous)
    {
        return AliasResolutionStatus::AmbiguousRecord;
    }
    if winning.is_empty()
        || issues
            .iter()
            .any(|issue| issue.kind == ResolutionIssueKind::Unresolved)
    {
        return AliasResolutionStatus::Unresolved;
    }
    let terminal = winning[0];
    if terminal.unsupported_shape || terminal.aka.is_none() {
        return if terminal.inexact_target {
            AliasResolutionStatus::Unresolved
        } else {
            AliasResolutionStatus::UnsupportedDeclarator
        };
    }
    AliasResolutionStatus::UniqueRecord
}

fn compose_aka_spelling(record: &RecordCandidate, trace: &[TypeAliasCandidate]) -> Option<String> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Form {
        Plain,
        Pointer,
        Array,
    }

    let mut spelling = record.type_spelling();
    let mut form = Form::Plain;
    for alias in trace.iter().rev() {
        let target_surface = alias_target_surface(alias, record)?;
        let base_qualifiers = exact_base_qualifiers(&alias.underlying_spelling, &target_surface)?;

        // Base qualifiers belong to the type specifier rather than the
        // declarator.  Applying them to an already-expanded pointer/array
        // alias requires placement information the limited shape does not
        // preserve (`const Ptr` is `T * const`, not `const T *`).
        if !base_qualifiers.is_empty() {
            if form != Form::Plain {
                return None;
            }
            spelling = format!("{} {spelling}", base_qualifiers.join(" "));
        }

        match &alias.declarator_shape {
            DeclaratorShape::Identity => {
                if !base_qualifiers.is_empty() {
                    return None;
                }
            }
            DeclaratorShape::Qualified { qualifiers } => {
                let qualifiers: Vec<_> = qualifiers
                    .iter()
                    .map(|qualifier| qualifier.trim())
                    .collect();
                if qualifiers.is_empty()
                    || qualifiers
                        .iter()
                        .copied()
                        .ne(base_qualifiers.iter().map(String::as_str))
                {
                    return None;
                }
            }
            DeclaratorShape::Pointer { qualifiers } => {
                // Pointer-to-array requires parentheses (`(*)[N]`), which this
                // intentionally-limited formatter does not synthesize.
                if form == Form::Array {
                    return None;
                }
                spelling.push_str(" *");
                if !qualifiers.is_empty() {
                    spelling.push(' ');
                    spelling.push_str(&qualifiers.join(" "));
                }
                form = Form::Pointer;
            }
            DeclaratorShape::Array { extent_text } => {
                // Nested typedef arrays reverse dimensions unless the complete
                // parenthesized declarator is retained.  Stay conservative.
                if form == Form::Array {
                    return None;
                }
                spelling.push('[');
                spelling.push_str(extent_text.trim());
                spelling.push(']');
                form = Form::Array;
            }
            DeclaratorShape::Unsupported => return None,
        }
    }
    Some(spelling)
}

fn alias_target_surface(alias: &TypeAliasCandidate, record: &RecordCandidate) -> Option<String> {
    match &alias.target {
        TypeAliasTarget::StableRecord(identity) => {
            (identity == &record.identity).then(|| record_source_type_spelling(record))
        }
        TypeAliasTarget::NamedRecord { tag, kind } => (record.named_tag_matches(tag, *kind))
            .then(|| format!("{} {tag}", record_kind_keyword(*kind))),
        TypeAliasTarget::TypeName(name) => (!name.is_empty()).then(|| name.clone()),
    }
}

fn record_source_type_spelling(record: &RecordCandidate) -> String {
    match record.tag_name.as_deref() {
        Some(tag) => format!("{} {tag}", record_kind_keyword(record.kind)),
        // `underlying_spelling` omits an anonymous record body, leaving the
        // source keyword. Presentation later uses the explicit `anonymous`
        // label from `RecordCandidate::type_spelling()`.
        None => record_kind_keyword(record.kind).to_string(),
    }
}

fn exact_base_qualifiers(underlying: &str, target: &str) -> Option<Vec<String>> {
    let underlying = compact_whitespace(underlying);
    let target = compact_whitespace(target);
    if underlying == target {
        return Some(Vec::new());
    }
    let prefix = underlying.strip_suffix(&target)?.trim_end();
    if prefix.len() == underlying.len() || prefix.is_empty() {
        return None;
    }
    // Prevent a suffix-only textual match such as `NotFoo` -> `Foo`.
    let boundary = underlying.as_bytes().get(prefix.len()).copied()?;
    if !boundary.is_ascii_whitespace() {
        return None;
    }
    let qualifiers: Vec<String> = prefix.split_whitespace().map(str::to_string).collect();
    qualifiers
        .iter()
        .all(|qualifier| is_supported_base_qualifier(qualifier))
        .then_some(qualifiers)
}

fn compact_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_supported_base_qualifier(value: &str) -> bool {
    matches!(value, "const" | "volatile" | "restrict" | "_Atomic")
}

fn enforce_bound(
    mut coverage: CandidateCoverage,
    inspected: usize,
    unique_matches: usize,
    limit: usize,
) -> CandidateCoverage {
    coverage.scanned = coverage.scanned.max(inspected);
    if unique_matches > limit {
        coverage.truncated = true;
        if coverage.incomplete_reason.is_none() {
            coverage.incomplete_reason = Some(CandidateIncompleteReason::CandidateBudget);
        }
    }
    coverage
}

fn mark_resolution_truncated(coverage: &mut CandidateCoverage) {
    coverage.truncated = true;
    if coverage.incomplete_reason.is_none() {
        coverage.incomplete_reason = Some(CandidateIncompleteReason::CandidateBudget);
    }
}

fn sort_record_candidates(candidates: &mut [RecordCandidate]) {
    candidates.sort_by(compare_record_candidates);
}

fn compare_record_candidates(a: &RecordCandidate, b: &RecordCandidate) -> std::cmp::Ordering {
    b.tier
        .cmp(&a.tier)
        .then_with(|| a.path.cmp(&b.path))
        .then_with(|| {
            a.declaration_range
                .start_byte
                .cmp(&b.declaration_range.start_byte)
        })
        .then_with(|| record_kind_rank(a.kind).cmp(&record_kind_rank(b.kind)))
        .then_with(|| a.identity.cmp(&b.identity))
}

fn sort_alias_candidates(candidates: &mut [TypeAliasCandidate]) {
    candidates.sort_by(|a, b| {
        b.tier
            .cmp(&a.tier)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| {
                a.declaration_range
                    .start_byte
                    .cmp(&b.declaration_range.start_byte)
            })
            .then_with(|| a.identity.cmp(&b.identity))
    });
}

fn min_tier(a: ScopeTier, b: ScopeTier) -> ScopeTier {
    if a <= b {
        a
    } else {
        b
    }
}

fn record_kind_keyword(kind: RecordKind) -> &'static str {
    match kind {
        RecordKind::Struct => "struct",
        RecordKind::Union => "union",
        RecordKind::Class => "class",
    }
}

fn record_kind_rank(kind: RecordKind) -> u8 {
    match kind {
        RecordKind::Struct => 0,
        RecordKind::Union => 1,
        RecordKind::Class => 2,
    }
}

fn source_range_from_parts(
    start_byte: usize,
    end_byte: usize,
    start_line: usize,
    start_col: usize,
    end_line: usize,
    end_col: usize,
) -> SourceRange {
    SourceRange {
        start: SourcePosition {
            line: start_line as u32,
            character: start_col as u32,
        },
        end: SourcePosition {
            line: end_line as u32,
            character: end_col as u32,
        },
        start_byte,
        end_byte,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(byte: usize) -> SourceRange {
        SourceRange {
            start: SourcePosition {
                line: byte as u32,
                character: 0,
            },
            end: SourcePosition {
                line: byte as u32,
                character: 1,
            },
            start_byte: byte,
            end_byte: byte + 1,
        }
    }

    fn record(
        id: i64,
        tag: Option<&str>,
        typedef_name: Option<&str>,
        tier: ScopeTier,
    ) -> RecordCandidate {
        let display_name = tag.or(typedef_name).unwrap_or("anonymous").to_string();
        RecordCandidate {
            identity: RecordCandidateIdentity::Persistent(id),
            display_name,
            tag_name: tag.map(str::to_string),
            typedef_name: typedef_name.map(str::to_string),
            kind: RecordKind::Struct,
            path: format!("record-{id}.h"),
            name_range: range(id as usize),
            body_range: range(id as usize + 10),
            declaration_range: range(id as usize + 20),
            declaration_hash: [id as u8; 32],
            range_fidelity: RecordRangeFidelity::AstExact,
            confidence: if tag.is_some() {
                RecordConfidence::NamedTag
            } else {
                RecordConfidence::AnonymousTypedef
            },
            signature: String::new(),
            tier,
            revision: None,
        }
    }

    fn alias(
        id: i64,
        name: &str,
        target: TypeAliasTarget,
        shape: DeclaratorShape,
        tier: ScopeTier,
    ) -> TypeAliasCandidate {
        let target_surface = match &target {
            TypeAliasTarget::NamedRecord { tag, kind } => {
                format!("{} {tag}", record_kind_keyword(*kind))
            }
            TypeAliasTarget::TypeName(name) => name.clone(),
            TypeAliasTarget::StableRecord(_) => name.to_string(),
        };
        let underlying_spelling = match &shape {
            DeclaratorShape::Qualified { qualifiers } => {
                format!("{} {target_surface}", qualifiers.join(" "))
            }
            _ => target_surface,
        };
        TypeAliasCandidate {
            identity: TypeAliasCandidateIdentity::Persistent {
                id,
                fingerprint: format!("fingerprint-{id}"),
            },
            alias: name.to_string(),
            target,
            path: format!("alias-{id}.h"),
            name_range: range(id as usize),
            declaration_range: range(id as usize + 30),
            declaration_hash: [id as u8; 32],
            underlying_spelling,
            declarator_shape: shape,
            target_fidelity: AliasTargetFidelity::AstExact,
            fingerprint: format!("fingerprint-{id}"),
            tier,
            revision: None,
        }
    }

    #[test]
    fn exact_record_candidates_are_scope_ranked_deduplicated_and_bounded() {
        let reachable = record(1, Some("Packet"), None, ScopeTier::Reachable);
        let duplicate = reachable.clone();
        let global = record(2, Some("Packet"), None, ScopeTier::Global);
        let unrelated = record(3, Some("Other"), None, ScopeTier::Current);
        let set = record_candidates_exact(
            "Packet",
            vec![global, unrelated, duplicate, reachable],
            CandidateCoverage::complete(0),
            1,
        );

        assert_eq!(set.candidates.len(), 1);
        assert_eq!(
            set.candidates[0].identity,
            RecordCandidateIdentity::Persistent(1)
        );
        assert!(set.coverage.truncated);
        assert_eq!(
            set.coverage.incomplete_reason,
            Some(CandidateIncompleteReason::CandidateBudget)
        );
        assert_eq!(set.coverage.scanned, 4);
    }

    #[test]
    fn exact_alias_candidates_use_the_same_deterministic_scope_policy() {
        let current = alias(
            1,
            "PacketT",
            TypeAliasTarget::TypeName("Packet".into()),
            DeclaratorShape::Identity,
            ScopeTier::Current,
        );
        let duplicate = current.clone();
        let reachable = alias(
            2,
            "PacketT",
            TypeAliasTarget::TypeName("Packet".into()),
            DeclaratorShape::Identity,
            ScopeTier::Reachable,
        );
        let unrelated = alias(
            3,
            "Other",
            TypeAliasTarget::TypeName("Packet".into()),
            DeclaratorShape::Identity,
            ScopeTier::Current,
        );

        let set = type_alias_candidates_exact(
            "PacketT",
            vec![reachable, unrelated, duplicate, current],
            CandidateCoverage::complete(0),
            TYPE_CANDIDATE_LIMIT,
        );

        assert_eq!(set.candidates.len(), 2);
        assert_eq!(set.candidates[0].tier, ScopeTier::Current);
        assert_eq!(set.candidates[1].tier, ScopeTier::Reachable);
        assert!(!set.coverage.truncated);
    }

    #[test]
    fn reachable_unique_record_wins_over_global_twin() {
        let root = alias(
            10,
            "PacketT",
            TypeAliasTarget::NamedRecord {
                tag: "Packet".into(),
                kind: RecordKind::Struct,
            },
            DeclaratorShape::Identity,
            ScopeTier::Current,
        );
        let records = vec![
            record(1, Some("Packet"), None, ScopeTier::Global),
            record(2, Some("Packet"), None, ScopeTier::Reachable),
        ];

        let resolved = resolve_type_alias(
            root,
            &[],
            &records,
            CandidateCoverage::complete(records.len()),
        );

        assert_eq!(resolved.status, AliasResolutionStatus::UniqueRecord);
        assert_eq!(resolved.winning_tier, Some(ScopeTier::Reachable));
        assert_eq!(resolved.aka_spelling.as_deref(), Some("struct Packet"));
        assert_eq!(resolved.terminal_records.len(), 2);
    }

    #[test]
    fn same_tier_same_tag_records_remain_ambiguous() {
        let root = alias(
            10,
            "PacketT",
            TypeAliasTarget::NamedRecord {
                tag: "Packet".into(),
                kind: RecordKind::Struct,
            },
            DeclaratorShape::Identity,
            ScopeTier::Current,
        );
        let records = vec![
            record(1, Some("Packet"), None, ScopeTier::Reachable),
            record(2, Some("Packet"), None, ScopeTier::Reachable),
        ];

        let resolved = resolve_type_alias(
            root,
            &[],
            &records,
            CandidateCoverage::complete(records.len()),
        );

        assert_eq!(resolved.status, AliasResolutionStatus::AmbiguousRecord);
        assert_eq!(resolved.aka_spelling, None);
        assert_eq!(resolved.terminal_records.len(), 2);
    }

    #[test]
    fn alias_cycle_is_guarded_by_declarator_identity() {
        let a = alias(
            1,
            "A",
            TypeAliasTarget::TypeName("B".into()),
            DeclaratorShape::Identity,
            ScopeTier::Reachable,
        );
        let b = alias(
            2,
            "B",
            TypeAliasTarget::TypeName("A".into()),
            DeclaratorShape::Identity,
            ScopeTier::Reachable,
        );

        let resolved = resolve_type_alias(a.clone(), &[a, b], &[], CandidateCoverage::complete(2));

        assert_eq!(resolved.status, AliasResolutionStatus::Cycle);
        assert_eq!(resolved.chain.len(), 2);
        assert!(resolved.terminal_records.is_empty());
        assert_eq!(resolved.aka_spelling, None);
    }

    #[test]
    fn same_named_tag_alias_resolves_tag_without_false_self_cycle() {
        let root = alias(
            1,
            "Foo",
            TypeAliasTarget::NamedRecord {
                tag: "Foo".into(),
                kind: RecordKind::Struct,
            },
            DeclaratorShape::Identity,
            ScopeTier::Current,
        );
        let foo = record(7, Some("Foo"), None, ScopeTier::Current);

        let resolved = resolve_type_alias(
            root.clone(),
            &[root],
            &[foo],
            CandidateCoverage::complete(2),
        );

        assert_eq!(resolved.status, AliasResolutionStatus::UniqueRecord);
        assert_eq!(resolved.aka_spelling.as_deref(), Some("struct Foo"));
    }

    #[test]
    fn identity_alias_chain_keeps_trace_and_conservative_tier() {
        let inner = alias(
            1,
            "FooT",
            TypeAliasTarget::NamedRecord {
                tag: "Foo".into(),
                kind: RecordKind::Struct,
            },
            DeclaratorShape::Identity,
            ScopeTier::Reachable,
        );
        let outer = alias(
            2,
            "PublicFoo",
            TypeAliasTarget::TypeName("FooT".into()),
            DeclaratorShape::Identity,
            ScopeTier::Current,
        );

        let resolved = resolve_type_alias(
            outer,
            &[inner],
            &[record(7, Some("Foo"), None, ScopeTier::Current)],
            CandidateCoverage::complete(2),
        );

        assert_eq!(resolved.status, AliasResolutionStatus::UniqueRecord);
        assert_eq!(resolved.winning_tier, Some(ScopeTier::Reachable));
        assert_eq!(resolved.chain.len(), 2);
        assert_eq!(resolved.aka_spelling.as_deref(), Some("struct Foo"));
    }

    #[test]
    fn direct_qualified_and_array_shapes_are_exact() {
        let qualified = alias(
            1,
            "FooConst",
            TypeAliasTarget::NamedRecord {
                tag: "Foo".into(),
                kind: RecordKind::Struct,
            },
            DeclaratorShape::Qualified {
                qualifiers: vec!["const".into()],
            },
            ScopeTier::Current,
        );
        let array = alias(
            2,
            "FooArray",
            TypeAliasTarget::NamedRecord {
                tag: "Foo".into(),
                kind: RecordKind::Struct,
            },
            DeclaratorShape::Array {
                extent_text: "4".into(),
            },
            ScopeTier::Current,
        );
        let foo = record(7, Some("Foo"), None, ScopeTier::Current);

        let qualified = resolve_type_alias(
            qualified,
            &[],
            std::slice::from_ref(&foo),
            CandidateCoverage::complete(1),
        );
        let array = resolve_type_alias(array, &[], &[foo], CandidateCoverage::complete(1));

        assert_eq!(qualified.aka_spelling.as_deref(), Some("const struct Foo"));
        assert_eq!(array.aka_spelling.as_deref(), Some("struct Foo[4]"));
    }

    #[test]
    fn pointer_and_anonymous_typedefs_preserve_shape_without_inventing_a_tag() {
        let pointer = alias(
            1,
            "FooPtr",
            TypeAliasTarget::NamedRecord {
                tag: "Foo".into(),
                kind: RecordKind::Struct,
            },
            DeclaratorShape::Pointer {
                qualifiers: Vec::new(),
            },
            ScopeTier::Current,
        );
        let pointer_resolution = resolve_type_alias(
            pointer,
            &[],
            &[record(1, Some("Foo"), None, ScopeTier::Current)],
            CandidateCoverage::complete(1),
        );
        assert_eq!(
            pointer_resolution.aka_spelling.as_deref(),
            Some("struct Foo *")
        );

        let mut const_pointer = alias(
            3,
            "ConstFooPtr",
            TypeAliasTarget::NamedRecord {
                tag: "Foo".into(),
                kind: RecordKind::Struct,
            },
            DeclaratorShape::Pointer {
                qualifiers: Vec::new(),
            },
            ScopeTier::Current,
        );
        const_pointer.underlying_spelling = "const struct Foo".into();
        let const_pointer_resolution = resolve_type_alias(
            const_pointer,
            &[],
            &[record(1, Some("Foo"), None, ScopeTier::Current)],
            CandidateCoverage::complete(1),
        );
        assert_eq!(
            const_pointer_resolution.aka_spelling.as_deref(),
            Some("const struct Foo *")
        );

        let identity = RecordCandidateIdentity::ParserKey {
            path: "buffer.h".into(),
            record_key: "anonymous-1".into(),
        };
        let mut anonymous = record(2, None, Some("Buffer"), ScopeTier::Current);
        anonymous.identity = identity.clone();
        anonymous.path = "buffer.h".into();
        let buffer = alias(
            2,
            "Buffer",
            TypeAliasTarget::StableRecord(identity),
            DeclaratorShape::Identity,
            ScopeTier::Current,
        );
        let mut buffer = buffer;
        buffer.underlying_spelling = "struct".into();
        let anonymous_resolution =
            resolve_type_alias(buffer, &[], &[anonymous], CandidateCoverage::complete(1));
        assert_eq!(
            anonymous_resolution.aka_spelling.as_deref(),
            Some("anonymous struct")
        );
        assert_ne!(
            anonymous_resolution.aka_spelling.as_deref(),
            Some("struct Buffer")
        );
    }

    #[test]
    fn unsupported_declarator_keeps_terminal_but_never_forges_aka() {
        let root = alias(
            1,
            "Callback",
            TypeAliasTarget::NamedRecord {
                tag: "Foo".into(),
                kind: RecordKind::Struct,
            },
            DeclaratorShape::Unsupported,
            ScopeTier::Current,
        );
        let resolved = resolve_type_alias(
            root,
            &[],
            &[record(1, Some("Foo"), None, ScopeTier::Current)],
            CandidateCoverage::complete(1),
        );

        assert_eq!(
            resolved.status,
            AliasResolutionStatus::UnsupportedDeclarator
        );
        assert_eq!(resolved.terminal_records.len(), 1);
        assert_eq!(resolved.aka_spelling, None);
    }

    #[test]
    fn qualifier_placement_across_pointer_alias_is_not_guessed() {
        let pointer = alias(
            1,
            "FooPtr",
            TypeAliasTarget::NamedRecord {
                tag: "Foo".into(),
                kind: RecordKind::Struct,
            },
            DeclaratorShape::Pointer {
                qualifiers: Vec::new(),
            },
            ScopeTier::Current,
        );
        let qualified_pointer = alias(
            2,
            "ConstFooPtr",
            TypeAliasTarget::TypeName("FooPtr".into()),
            DeclaratorShape::Qualified {
                qualifiers: vec!["const".into()],
            },
            ScopeTier::Current,
        );

        let resolved = resolve_type_alias(
            qualified_pointer,
            &[pointer],
            &[record(1, Some("Foo"), None, ScopeTier::Current)],
            CandidateCoverage::complete(2),
        );

        assert_eq!(
            resolved.status,
            AliasResolutionStatus::UnsupportedDeclarator
        );
        assert_eq!(resolved.aka_spelling, None);
        assert_eq!(resolved.terminal_records.len(), 1);
    }

    #[test]
    fn depth_guard_is_bounded_and_marks_coverage_incomplete() {
        let a = alias(
            1,
            "A",
            TypeAliasTarget::TypeName("B".into()),
            DeclaratorShape::Identity,
            ScopeTier::Current,
        );
        let b = alias(
            2,
            "B",
            TypeAliasTarget::TypeName("C".into()),
            DeclaratorShape::Identity,
            ScopeTier::Current,
        );
        let c = alias(
            3,
            "C",
            TypeAliasTarget::NamedRecord {
                tag: "Foo".into(),
                kind: RecordKind::Struct,
            },
            DeclaratorShape::Identity,
            ScopeTier::Current,
        );

        let resolved = resolve_type_alias_with_limits(
            a.clone(),
            &[a, b, c],
            &[record(1, Some("Foo"), None, ScopeTier::Current)],
            CandidateCoverage::complete(4),
            AliasResolutionLimits {
                max_depth: 2,
                max_visits: 8,
            },
        );

        assert_eq!(resolved.status, AliasResolutionStatus::Truncated);
        assert!(resolved.coverage.truncated);
        assert_eq!(
            resolved.coverage.incomplete_reason,
            Some(CandidateIncompleteReason::CandidateBudget)
        );
    }

    #[test]
    fn open_scope_never_claims_alias_uniqueness() {
        let root = alias(
            1,
            "FooT",
            TypeAliasTarget::NamedRecord {
                tag: "Foo".into(),
                kind: RecordKind::Struct,
            },
            DeclaratorShape::Identity,
            ScopeTier::Reachable,
        );
        let mut coverage = CandidateCoverage::complete(1);
        coverage.scope_open = true;

        let resolved = resolve_type_alias(
            root,
            &[],
            &[record(1, Some("Foo"), None, ScopeTier::Reachable)],
            coverage,
        );

        assert_eq!(resolved.status, AliasResolutionStatus::Truncated);
        assert_eq!(resolved.aka_spelling, None);
    }
}
