//! Generation-pinned semantic candidate recall and dirty-document overlays.
//!
//! The overlay index is deliberately independent from the ordinary completion
//! overlay.  It contains canonical parser facts for only divergent open
//! documents, shadows the matching durable path even when the new document no
//! longer contains a fact, and supports exact-name lookup without scanning all
//! open buffers on every request.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

use crate::call_catalog::rows::{anchor_from_row, call_from_row};
use crate::call_model::{
    AnchorRole, CallSiteFact, CallableAnchor, CallableKind, FactProvenance, LinkageDomain,
    SignatureFidelity, SignatureShape, SourcePosition, SourceRange,
};
use crate::call_service::CallReadHandle;
use crate::model::{CandidateRange, DefinitionCandidate, MemberCandidate};
use crate::parser::{FactAvailability, FactGroup, FileSemanticIndex};
use crate::query::{
    record_candidates_exact, resolve_callable_candidates, resolve_type_alias,
    type_alias_candidates_exact, AliasResolution, AliasResolutionStatus, CallSiteContext,
    CallableCandidateSet, CallableQueryInput, CandidateCoverage, CandidateOrigin,
    CandidateRevision, ContextReliability, RecordCandidate, RecordCandidateIdentity,
    RecordCandidateSet, ResolvedCallableAnchor, TypeAliasCandidate, TypeAliasCandidateSet,
    TypeAliasTarget, ALIAS_RESOLUTION_MAX_VISITS, TYPE_CANDIDATE_LIMIT,
};
use crate::reachability::{ReachGraph, ReachScope};
use crate::resolver::{self, ResolveContext};
use crate::semantic_model::{Include, MemberDef, RecordDef, Symbol, SymbolRole, TypeAlias};
use crate::store::SymbolRecord;

pub const DEFAULT_EXACT_NAME_CANDIDATE_LIMIT: usize = 256;
const MEMBER_FALLBACK_OVERLAY_SCAN_LIMIT: usize = 8_192;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeCandidateBundle {
    pub records: RecordCandidateSet,
    pub aliases: TypeAliasCandidateSet,
    pub alias_resolutions: Vec<AliasResolution>,
    /// Durable evidence hidden by a dirty-path tombstone anywhere in the
    /// bounded resolution trace. This distinguishes an authoritative deletion
    /// from a genuine miss and prevents legacy readers reviving stale rows.
    pub shadowed_evidence: bool,
    /// The complete bounded working set used to resolve alias chains. Root
    /// presentation should use `records`/`aliases`, not expose this directly.
    pub trace_records: Vec<RecordCandidate>,
}

/// Terminal record evidence retained for member completion. `authoritative`
/// distinguishes a genuine miss from a dirty-path tombstone, while
/// `incomplete` and `ambiguous` prevent a merged best-effort member list from
/// being presented as a closed, compiler-bound result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeRecordResolution {
    pub records: Vec<RecordCandidate>,
    pub authoritative: bool,
    pub incomplete: bool,
    pub ambiguous: bool,
}

/// One bounded resolved-owner member read. `scanned` is the shared-budget
/// charge across overlay and durable rows; `truncated` means at least one
/// additional row was deliberately left unread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedMemberCandidates {
    pub candidates: Vec<MemberCandidate>,
    pub scanned: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone)]
pub struct FileCandidateOverlay {
    pub path: String,
    pub anchors: Vec<CallableAnchor>,
    pub calls: Vec<CallSiteFact>,
    pub symbols: Vec<Symbol>,
    pub records: Vec<RecordDef>,
    pub members: Vec<MemberDef>,
    pub aliases: Vec<TypeAlias>,
    pub includes: Vec<Include>,
    pub text: Option<Arc<str>>,
    /// False when any semantic fact group needed by the candidate facade is
    /// unavailable. This includes cancelled parses and lexical fallback: an
    /// empty vector is not evidence that the dirty file contains no facts.
    pub facts_complete: bool,
}

impl FileCandidateOverlay {
    pub fn new(
        path: String,
        mut anchors: Vec<CallableAnchor>,
        mut calls: Vec<CallSiteFact>,
    ) -> Self {
        for anchor in &mut anchors {
            anchor.path.clone_from(&path);
        }
        for call in &mut calls {
            call.path.clone_from(&path);
        }
        Self {
            path,
            anchors,
            calls,
            symbols: Vec::new(),
            records: Vec::new(),
            members: Vec::new(),
            aliases: Vec::new(),
            includes: Vec::new(),
            text: None,
            facts_complete: true,
        }
    }

    pub fn from_index(path: String, index: &FileSemanticIndex) -> Self {
        let mut overlay = Self::new(
            path,
            index.callable_anchors.clone(),
            index.call_sites.clone(),
        );
        overlay.symbols.clone_from(&index.symbols);
        overlay.records.clone_from(&index.records);
        overlay.members.clone_from(&index.members);
        overlay.aliases.clone_from(&index.aliases);
        overlay.includes.clone_from(&index.includes);
        overlay.facts_complete = [
            FactGroup::CallableAnchors,
            FactGroup::CallSites,
            FactGroup::Records,
            FactGroup::Members,
            FactGroup::Aliases,
        ]
        .into_iter()
        .all(|group| index.fact_availability(group) == FactAvailability::Available);
        overlay
    }

    pub fn from_index_with_text(path: String, index: &FileSemanticIndex, text: Arc<str>) -> Self {
        let mut overlay = Self::from_index(path, index);
        overlay.text = Some(text);
        overlay
    }

    pub fn tombstone(path: String, text: Arc<str>) -> Self {
        let mut overlay = Self::new(path, Vec::new(), Vec::new());
        overlay.text = Some(text);
        overlay.facts_complete = false;
        overlay
    }
}

#[derive(Debug, Clone)]
pub struct OverlayRecordFact {
    pub path: String,
    pub record: RecordDef,
}

#[derive(Debug, Clone)]
pub struct OverlayAliasFact {
    pub path: String,
    pub alias: TypeAlias,
}

#[derive(Debug, Clone)]
pub struct OverlaySymbolFact {
    pub path: String,
    pub symbol: Symbol,
}

#[derive(Debug, Clone)]
struct OverlayMemberFact {
    path: String,
    name_lower: String,
    member: MemberDef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayCompletionName {
    pub id: i64,
    pub name: String,
    pub path: String,
    pub kind: String,
    pub external: bool,
    pub directly_included: bool,
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

#[derive(Debug, Clone, Default)]
pub struct CandidateOverlaySnapshot {
    #[allow(dead_code)] // Captured for request tracing and cross-snapshot diagnostics.
    pub epoch: u64,
    shadowed_paths: HashSet<String>,
    callable_by_name: HashMap<String, Vec<CallableAnchor>>,
    callable_by_path: HashMap<String, Vec<CallableAnchor>>,
    symbol_by_name: HashMap<String, Vec<OverlaySymbolFact>>,
    record_by_name: HashMap<String, Vec<OverlayRecordFact>>,
    record_by_key: HashMap<(String, String), OverlayRecordFact>,
    records_by_path: HashMap<String, Vec<OverlayRecordFact>>,
    members_by_record_key: HashMap<(String, String), Vec<MemberDef>>,
    member_prefix_index: Vec<OverlayMemberFact>,
    alias_by_name: HashMap<String, Vec<OverlayAliasFact>>,
    call_sites_by_path: HashMap<String, Vec<CallSiteFact>>,
    source_by_path: HashMap<String, Arc<str>>,
    includes_by_path: HashMap<String, Vec<Include>>,
    incomplete_paths: HashSet<String>,
    effective_reach_graph: Option<Arc<ReachGraph>>,
    /// Only external paths whose workspace-wide first-layer status differs
    /// from the published graph. Ordinary completion applies this sparse map
    /// over its immutable NameTable instead of rebuilding or scanning it.
    direct_include_overrides: HashMap<String, bool>,
}

impl CandidateOverlaySnapshot {
    pub fn new(epoch: u64, files: Vec<FileCandidateOverlay>) -> Self {
        let mut snapshot = Self {
            epoch,
            ..Self::default()
        };
        for file in files {
            snapshot.shadowed_paths.insert(file.path.clone());
            if let Some(text) = file.text.clone() {
                snapshot.source_by_path.insert(file.path.clone(), text);
            }
            if !file.facts_complete {
                snapshot.incomplete_paths.insert(file.path.clone());
            }
            snapshot
                .includes_by_path
                .insert(file.path.clone(), file.includes.clone());
            snapshot
                .callable_by_path
                .insert(file.path.clone(), file.anchors.clone());
            for anchor in file.anchors {
                snapshot
                    .callable_by_name
                    .entry(anchor.name.clone())
                    .or_default()
                    .push(anchor);
            }
            for symbol in file.symbols {
                snapshot
                    .symbol_by_name
                    .entry(symbol.name.clone())
                    .or_default()
                    .push(OverlaySymbolFact {
                        path: file.path.clone(),
                        symbol,
                    });
            }
            for record in file.records {
                let fact = OverlayRecordFact {
                    path: file.path.clone(),
                    record,
                };
                snapshot.record_by_key.insert(
                    (file.path.clone(), fact.record.record_key.clone()),
                    fact.clone(),
                );
                snapshot
                    .records_by_path
                    .entry(file.path.clone())
                    .or_default()
                    .push(fact.clone());
                let mut names = vec![fact.record.display_name.clone()];
                if let Some(name) = &fact.record.tag_name {
                    names.push(name.clone());
                }
                if let Some(name) = &fact.record.typedef_name {
                    names.push(name.clone());
                }
                names.sort_unstable();
                names.dedup();
                for name in names {
                    snapshot
                        .record_by_name
                        .entry(name)
                        .or_default()
                        .push(fact.clone());
                }
            }
            for member in file.members {
                snapshot.member_prefix_index.push(OverlayMemberFact {
                    path: file.path.clone(),
                    name_lower: member.name.to_ascii_lowercase(),
                    member: member.clone(),
                });
                let record_key = member
                    .record_key
                    .strip_prefix("owner:")
                    .and_then(|owner| {
                        let mut matches =
                            snapshot
                                .records_by_path
                                .get(&file.path)?
                                .iter()
                                .filter(|fact| {
                                    fact.record.display_name == owner
                                        || fact.record.tag_name.as_deref() == Some(owner)
                                        || fact.record.typedef_name.as_deref() == Some(owner)
                                });
                        let found = matches.next()?;
                        matches
                            .next()
                            .is_none()
                            .then(|| found.record.record_key.clone())
                    })
                    .unwrap_or_else(|| member.record_key.clone());
                snapshot
                    .members_by_record_key
                    .entry((file.path.clone(), record_key))
                    .or_default()
                    .push(member);
            }
            for alias in file.aliases {
                snapshot
                    .alias_by_name
                    .entry(alias.alias.clone())
                    .or_default()
                    .push(OverlayAliasFact {
                        path: file.path.clone(),
                        alias,
                    });
            }
            snapshot.call_sites_by_path.insert(file.path, file.calls);
        }
        // `DocumentStore` snapshots originate from a hash map, so file order is
        // intentionally unspecified. Exact-name queries must nevertheless have
        // stable truncation and presentation order across identical requests.
        for anchors in snapshot.callable_by_name.values_mut() {
            anchors.sort_by(|left, right| {
                left.path
                    .cmp(&right.path)
                    .then_with(|| left.name_range.start_byte.cmp(&right.name_range.start_byte))
                    .then_with(|| left.anchor_fingerprint.cmp(&right.anchor_fingerprint))
            });
        }
        for symbols in snapshot.symbol_by_name.values_mut() {
            symbols.sort_by(|left, right| {
                left.path
                    .cmp(&right.path)
                    .then_with(|| left.symbol.start_byte.cmp(&right.symbol.start_byte))
                    .then_with(|| left.symbol.end_byte.cmp(&right.symbol.end_byte))
            });
        }
        for records in snapshot.record_by_name.values_mut() {
            records.sort_by(|left, right| {
                left.path
                    .cmp(&right.path)
                    .then_with(|| left.record.start_byte.cmp(&right.record.start_byte))
                    .then_with(|| left.record.record_key.cmp(&right.record.record_key))
            });
        }
        for records in snapshot.records_by_path.values_mut() {
            records.sort_by(|left, right| {
                left.record
                    .start_byte
                    .cmp(&right.record.start_byte)
                    .then_with(|| left.record.record_key.cmp(&right.record.record_key))
            });
        }
        for members in snapshot.members_by_record_key.values_mut() {
            members.sort_by(|left, right| {
                left.start_byte
                    .cmp(&right.start_byte)
                    .then_with(|| left.name.cmp(&right.name))
                    .then_with(|| left.kind.as_str().cmp(right.kind.as_str()))
            });
        }
        snapshot.member_prefix_index.sort_by(|left, right| {
            left.name_lower
                .cmp(&right.name_lower)
                .then_with(|| left.member.name.cmp(&right.member.name))
                .then_with(|| left.path.cmp(&right.path))
                .then_with(|| left.member.start_byte.cmp(&right.member.start_byte))
                .then_with(|| left.member.kind.as_str().cmp(right.member.kind.as_str()))
                .then_with(|| left.member.signature.cmp(&right.member.signature))
        });
        for aliases in snapshot.alias_by_name.values_mut() {
            aliases.sort_by(|left, right| {
                left.path
                    .cmp(&right.path)
                    .then_with(|| left.alias.start_byte.cmp(&right.alias.start_byte))
                    .then_with(|| left.alias.fingerprint.cmp(&right.alias.fingerprint))
            });
        }
        snapshot
    }

    /// Build the immutable request-local include graph for every shadowed
    /// document. Durable out-edges for those paths are replaced, never mutated
    /// in place; unresolved/ambiguous live includes open the affected scope.
    pub fn refresh_reach_graph<'p>(
        &mut self,
        base: Option<&ReachGraph>,
        indexed_workspace_paths: impl IntoIterator<Item = &'p str>,
        include_roots: &[String],
    ) {
        if self.shadowed_paths.is_empty() {
            return;
        }
        self.direct_include_overrides.clear();
        let mut workspace_paths: HashSet<String> = indexed_workspace_paths
            .into_iter()
            .map(str::to_string)
            .collect();
        workspace_paths.extend(self.shadowed_paths.iter().cloned());
        let mut by_basename: HashMap<String, Vec<String>> = HashMap::new();
        for path in &workspace_paths {
            if let Some(name) = path.rsplit('/').next() {
                by_basename
                    .entry(name.to_string())
                    .or_default()
                    .push(path.clone());
            }
        }
        for paths in by_basename.values_mut() {
            paths.sort();
            paths.dedup();
        }
        let mut all_paths = workspace_paths;
        for include in self.includes_by_path.values().flatten() {
            let Some((_form, relative)) =
                crate::includes::normalize_include_target(&include.target_text)
            else {
                continue;
            };
            for root in include_roots {
                let candidate = format!("{}/{}", root.trim_end_matches('/'), relative);
                if Path::new(&candidate).is_file() {
                    all_paths.insert(candidate);
                }
            }
        }

        let mut sources: Vec<String> = self.shadowed_paths.iter().cloned().collect();
        sources.sort();
        let mut edges = Vec::new();
        let mut open = Vec::new();
        for source in &sources {
            if self.incomplete_paths.contains(source) {
                open.push((
                    source.clone(),
                    crate::reachability::OpenReason::UnresolvedInclude,
                ));
                continue;
            }
            let source_dir = source.rsplit_once('/').map_or("", |(dir, _)| dir);
            let mut reason = None;
            for include in self.includes_by_path.get(source).into_iter().flatten() {
                match crate::includes::resolve_include(
                    &include.target_text,
                    source_dir,
                    include_roots,
                    &all_paths,
                    &by_basename,
                ) {
                    crate::includes::IncludeResolution::Edge { dst, kind } => {
                        edges.push((source.clone(), dst, kind));
                    }
                    crate::includes::IncludeResolution::Ambiguous { dsts } => {
                        edges.extend(dsts.into_iter().map(|dst| {
                            (
                                source.clone(),
                                dst,
                                crate::includes::ResolutionKind::SuffixMatch,
                            )
                        }));
                        if reason.is_none() {
                            reason = Some(crate::reachability::OpenReason::AmbiguousInclude);
                        }
                    }
                    crate::includes::IncludeResolution::Unresolved => {
                        reason = Some(crate::reachability::OpenReason::UnresolvedInclude);
                    }
                }
            }
            if let Some(reason) = reason {
                open.push((source.clone(), reason));
            }
        }
        let graph = match base {
            Some(base) => base.with_refreshed_sources_with_kinds(&sources, edges, open),
            None => {
                let unresolved = open
                    .iter()
                    .filter(|(_, reason)| {
                        *reason == crate::reachability::OpenReason::UnresolvedInclude
                    })
                    .map(|(path, _)| path.clone())
                    .collect();
                let ambiguous = open
                    .iter()
                    .filter(|(_, reason)| {
                        *reason == crate::reachability::OpenReason::AmbiguousInclude
                    })
                    .map(|(path, _)| path.clone())
                    .collect();
                ReachGraph::new_with_kinds(edges, unresolved, ambiguous)
            }
        };
        if let Some(base) = base {
            let published = base.directly_included_external_paths();
            let effective = graph.directly_included_external_paths();
            for path in published.symmetric_difference(&effective) {
                self.direct_include_overrides
                    .insert(path.clone(), effective.contains(path));
            }
        }
        self.effective_reach_graph = Some(Arc::new(graph));
    }

    pub fn effective_reach_graph<'a>(
        &'a self,
        fallback: Option<&'a ReachGraph>,
    ) -> Option<&'a ReachGraph> {
        self.effective_reach_graph.as_deref().or(fallback)
    }

    /// Return the immutable request-local reach graph while preserving Arc
    /// ownership across a blocking Call Hierarchy worker. Dirty include edges
    /// win over the published fallback graph.
    pub(crate) fn effective_reach_graph_arc(
        &self,
        fallback: Option<Arc<ReachGraph>>,
    ) -> Option<Arc<ReachGraph>> {
        self.effective_reach_graph.clone().or(fallback)
    }

    /// Project only the call-relation delta needed by the lazy one-hop query
    /// service. The returned files retain tombstone completeness so a
    /// cancelled/lexical dirty parse can shadow durable facts and disable
    /// uniqueness proofs without exposing the other candidate indexes.
    pub(crate) fn call_relation_overlays(&self) -> Vec<FileCandidateOverlay> {
        let mut paths: Vec<_> = self.shadowed_paths.iter().cloned().collect();
        paths.sort();
        paths
            .into_iter()
            .map(|path| FileCandidateOverlay {
                anchors: self
                    .callable_by_path
                    .get(&path)
                    .cloned()
                    .unwrap_or_default(),
                calls: self
                    .call_sites_by_path
                    .get(&path)
                    .cloned()
                    .unwrap_or_default(),
                symbols: Vec::new(),
                records: Vec::new(),
                members: Vec::new(),
                aliases: Vec::new(),
                includes: Vec::new(),
                text: self.source_by_path.get(&path).cloned(),
                facts_complete: !self.incomplete_paths.contains(&path),
                path,
            })
            .collect()
    }

    pub fn shadows(&self, path: &str) -> bool {
        self.shadowed_paths.contains(path)
    }

    pub fn shadowed_paths(&self) -> &HashSet<String> {
        &self.shadowed_paths
    }

    pub fn has_incomplete_facts(&self) -> bool {
        !self.incomplete_paths.is_empty()
    }

    /// Sparse request-local replacement for durable first-layer external
    /// flags. Paths absent here retain their published value.
    pub fn direct_include_overrides(&self) -> &HashMap<String, bool> {
        &self.direct_include_overrides
    }

    pub fn callable_anchors(&self, name: &str) -> &[CallableAnchor] {
        self.callable_by_name
            .get(name)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn callable_by_path(&self, path: &str) -> &[CallableAnchor] {
        self.callable_by_path
            .get(path)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn records(&self, name: &str) -> &[OverlayRecordFact] {
        self.record_by_name
            .get(name)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn record_by_parser_key(&self, path: &str, record_key: &str) -> Option<&OverlayRecordFact> {
        self.record_by_key
            .get(&(path.to_string(), record_key.to_string()))
    }

    pub fn records_for_path(&self, path: &str) -> &[OverlayRecordFact] {
        self.records_by_path
            .get(path)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn members_for_parser_record(&self, path: &str, record_key: &str) -> &[MemberDef] {
        self.members_by_record_key
            .get(&(path.to_string(), record_key.to_string()))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    fn fallback_members_by_prefix_limited(
        &self,
        prefix: &str,
        limit: usize,
    ) -> (Vec<&OverlayMemberFact>, bool) {
        if limit == 0 {
            return (Vec::new(), false);
        }
        let needle = prefix.to_ascii_lowercase();
        let start = self
            .member_prefix_index
            .partition_point(|fact| fact.name_lower.as_str() < needle.as_str());
        let mut matches = Vec::new();
        let mut truncated = false;
        for fact in &self.member_prefix_index[start..] {
            if !fact.name_lower.starts_with(&needle) {
                break;
            }
            if matches.len() >= limit {
                truncated = true;
                break;
            }
            matches.push(fact);
        }
        (matches, truncated)
    }

    pub fn symbols(&self, name: &str) -> &[OverlaySymbolFact] {
        self.symbol_by_name
            .get(name)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Stable projection used to replace shadowed NameTable paths in ordinary
    /// completion. Do not truncate before NameTable applies the request's
    /// actual matcher: every dirty path is tombstoned, so dropping a later
    /// overlay symbol here could erase its only current representation.
    /// Negative ids are request-local locators and can never be mistaken for
    /// durable SQLite symbol ids.
    pub fn completion_names(&self) -> Vec<OverlayCompletionName> {
        let mut facts: Vec<_> = self.symbol_by_name.values().flatten().collect();
        facts.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.symbol.start_byte.cmp(&right.symbol.start_byte))
                .then_with(|| left.symbol.name.cmp(&right.symbol.name))
                .then_with(|| {
                    symbol_kind_name(left.symbol.kind).cmp(symbol_kind_name(right.symbol.kind))
                })
        });
        facts
            .into_iter()
            .enumerate()
            .map(|(index, fact)| {
                let external = Path::new(&fact.path).is_absolute();
                let directly_included = external
                    && self.effective_reach_graph.as_deref().is_some_and(|graph| {
                        graph.any_workspace_directly_includes_external(&fact.path)
                    });
                OverlayCompletionName {
                    id: -((index as i64) + 1),
                    name: fact.symbol.name.clone(),
                    path: fact.path.clone(),
                    kind: symbol_kind_name(fact.symbol.kind).to_string(),
                    external,
                    directly_included,
                    start_line: fact.symbol.start_line as u32,
                    start_col: fact.symbol.start_col as u32,
                    end_line: fact.symbol.end_line as u32,
                    end_col: fact.symbol.end_col as u32,
                }
            })
            .collect()
    }

    pub fn aliases(&self, name: &str) -> &[OverlayAliasFact] {
        self.alias_by_name
            .get(name)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn source_text(&self, path: &str) -> Option<&str> {
        self.source_by_path.get(path).map(AsRef::as_ref)
    }

    pub fn call_sites_at(&self, path: &str, position: SourcePosition) -> Vec<&CallSiteFact> {
        self.call_sites_by_path
            .get(path)
            .into_iter()
            .flatten()
            .filter(|call| position_in_range(position, call.callee_range))
            .collect()
    }
}

fn symbol_kind_name(kind: crate::semantic_model::SymbolKind) -> &'static str {
    match kind {
        crate::semantic_model::SymbolKind::Function => "function",
        crate::semantic_model::SymbolKind::Macro => "macro",
        crate::semantic_model::SymbolKind::Type => "type",
        crate::semantic_model::SymbolKind::EnumConstant => "enum_constant",
        crate::semantic_model::SymbolKind::GlobalVariable => "global_variable",
        crate::semantic_model::SymbolKind::Field => "field",
    }
}

fn position_in_range(position: SourcePosition, range: SourceRange) -> bool {
    (position.line, position.character) >= (range.start.line, range.start.character)
        && (position.line, position.character) <= (range.end.line, range.end.character)
}

fn candidate_origin_priority(origin: CandidateOrigin) -> u8 {
    match origin {
        CandidateOrigin::Base => 0,
        CandidateOrigin::Overlay => 1,
    }
}

/// Generation-pinned facade that recalls narrow durable rows, shadows them
/// with all divergent open documents, and hands a single candidate set to all
/// callable consumers. The pure arity/counterpart policy remains in `query`.
pub struct CandidateQueryService<'a> {
    handle: Option<&'a CallReadHandle>,
    overlays: &'a CandidateOverlaySnapshot,
    current_path: &'a str,
    current_reach: Option<Arc<ReachScope>>,
    reach_graph: Option<&'a ReachGraph>,
    exact_name_limit: usize,
}

impl<'a> CandidateQueryService<'a> {
    pub fn new(
        handle: Option<&'a CallReadHandle>,
        overlays: &'a CandidateOverlaySnapshot,
        current_path: &'a str,
        current_reach: Option<&'a ReachScope>,
        reach_graph: Option<&'a ReachGraph>,
    ) -> Self {
        let reach_graph = overlays.effective_reach_graph(reach_graph);
        let current_reach = reach_graph
            .map(|graph| graph.reachable(current_path))
            .or_else(|| current_reach.cloned().map(Arc::new));
        Self {
            handle,
            overlays,
            current_path,
            current_reach,
            reach_graph,
            exact_name_limit: DEFAULT_EXACT_NAME_CANDIDATE_LIMIT,
        }
    }

    /// Durable paths that must be recalled before a workspace-wide exact-name
    /// cap is allowed to spend the request budget. Dirty paths are omitted:
    /// their overlay is authoritative even when it is an empty tombstone.
    fn durable_priority_path_groups(&self) -> (Vec<String>, Vec<String>) {
        let current = if self.overlays.shadows(self.current_path) {
            Vec::new()
        } else {
            vec![self.current_path.to_string()]
        };
        let mut reachable: Vec<_> = self
            .current_reach
            .as_ref()
            .into_iter()
            .flat_map(|scope| scope.files.iter())
            .filter(|path| path.as_str() != self.current_path && !self.overlays.shadows(path))
            .cloned()
            .collect();
        reachable.sort();
        reachable.dedup();
        (current, reachable)
    }

    pub fn callable_candidates(
        &self,
        name: &str,
        call_context: Option<CallSiteContext>,
    ) -> Result<CallableCandidateSet> {
        if call_context.as_ref().is_some_and(|context| {
            context.reliability == ContextReliability::UnsupportedCallForm
                || !matches!(
                    context.form,
                    crate::call_model::CallForm::DirectName
                        | crate::call_model::CallForm::QualifiedName
                        | crate::call_model::CallForm::ParenthesizedName
                )
        }) {
            return Ok(CallableCandidateSet {
                anchors: Vec::new(),
                groups: Vec::new(),
                coverage: CandidateCoverage::default(),
                arity_mismatch_fallback: false,
            });
        }
        let (current_paths, reachable_paths) = self.durable_priority_path_groups();
        let (base_rows, fallback_symbol_rows, mut truncated) = match self.handle {
            Some(handle) => handle.read(|store| {
                let call_view = store.call_fact_view();
                let (global_anchors, mut anchor_truncated) =
                    call_view.anchors_by_name_limited(name, self.exact_name_limit)?;
                let mut anchors = Vec::new();
                if anchor_truncated {
                    for paths in [&current_paths, &reachable_paths] {
                        let remaining = self.exact_name_limit.saturating_sub(anchors.len());
                        let (rows, limited) =
                            call_view.anchors_by_name_in_paths_limited(name, paths, remaining)?;
                        anchors.extend(rows);
                        anchor_truncated |= limited;
                    }
                }
                // The global LIMIT+1 read remains the ordinary fast path. A
                // scope-priority rescue runs only after it proves truncation.
                anchors.extend(global_anchors);
                let mut seen_anchor_ids = HashSet::new();
                anchors.retain(|row| seen_anchor_ids.insert(row.id));

                let symbol_view = store.symbol_read_view();
                let (global_symbols, mut symbol_truncated) =
                    symbol_view.symbols_by_name_limited(name, self.exact_name_limit)?;
                let mut symbols = Vec::new();
                if symbol_truncated {
                    for paths in [&current_paths, &reachable_paths] {
                        let remaining = self.exact_name_limit.saturating_sub(symbols.len());
                        let (rows, limited) =
                            symbol_view.symbols_by_name_in_paths_limited(name, paths, remaining)?;
                        symbols.extend(rows);
                        symbol_truncated |= limited;
                    }
                }
                symbols.extend(global_symbols);
                let mut seen_symbol_ids = HashSet::new();
                symbols.retain(|record| seen_symbol_ids.insert(record.id));
                Ok((anchors, symbols, anchor_truncated || symbol_truncated))
            })?,
            None => (Vec::new(), Vec::new(), false),
        };
        let scanned = base_rows.len()
            + fallback_symbol_rows.len()
            + self.overlays.callable_anchors(name).len()
            + self.overlays.symbols(name).len();
        let resolve_context = ResolveContext {
            current_path: Some(self.current_path),
            reach: self.current_reach.as_deref(),
            direct_external_files: None,
        };
        let mut base_anchors: Vec<ResolvedCallableAnchor> = base_rows
            .into_iter()
            .filter(|row| !self.overlays.shadows(&row.path))
            .map(|row| {
                let source = row.source.clone();
                let (external, directly_included) =
                    self.path_evidence(&row.path, source == "external", row.directly_included);
                let tier = resolver::scope_tier(
                    &row.path,
                    external,
                    directly_included,
                    Some(&resolve_context),
                );
                let anchor = anchor_from_row(row);
                resolved_anchor(anchor, source, tier, CandidateOrigin::Base)
            })
            .collect();
        let base_fact_paths: HashSet<_> = base_anchors
            .iter()
            .map(|candidate| candidate.anchor.path.clone())
            .collect();
        let mut fallback_used = false;
        for mut record in fallback_symbol_rows.into_iter().filter(|record| {
            record.kind == "function"
                && !self.overlays.shadows(&record.path)
                && !base_fact_paths.contains(&record.path)
        }) {
            let (external, directly_included) = self.path_evidence(
                &record.path,
                record.source == "external",
                record.directly_included,
            );
            record.directly_included = directly_included;
            let tier = resolver::scope_tier(
                &record.path,
                external,
                directly_included,
                Some(&resolve_context),
            );
            base_anchors.push(resolved_lexical_function(
                record,
                tier,
                CandidateOrigin::Base,
            ));
            fallback_used = true;
        }
        let mut overlay_anchors = self
            .overlays
            .callable_anchors(name)
            .iter()
            .cloned()
            .map(|anchor| {
                let (external, directly_included) =
                    self.path_evidence(&anchor.path, Path::new(&anchor.path).is_absolute(), false);
                let tier = resolver::scope_tier(
                    &anchor.path,
                    external,
                    directly_included,
                    Some(&resolve_context),
                );
                let source = if external { "external" } else { "workspace" };
                resolved_anchor(anchor, source.into(), tier, CandidateOrigin::Overlay)
            })
            .collect::<Vec<_>>();
        let overlay_fact_paths: HashSet<_> = overlay_anchors
            .iter()
            .map(|candidate| candidate.anchor.path.clone())
            .collect();
        for fact in self.overlays.symbols(name).iter().filter(|fact| {
            fact.symbol.kind == crate::semantic_model::SymbolKind::Function
                && !overlay_fact_paths.contains(&fact.path)
        }) {
            let mut record = overlay_symbol_record(fact);
            let (external, directly_included) = self.path_evidence(
                &fact.path,
                record.source == "external",
                record.directly_included,
            );
            record.directly_included = directly_included;
            let tier = resolver::scope_tier(
                &fact.path,
                external,
                directly_included,
                Some(&resolve_context),
            );
            overlay_anchors.push(resolved_lexical_function(
                record,
                tier,
                CandidateOrigin::Overlay,
            ));
            fallback_used = true;
        }
        // Spend the final candidate budget by semantic tier, across both
        // durable and live facts. Overlay freshness wins only within an equal
        // tier; a dirty Global candidate cannot displace a durable Current or
        // Reachable candidate merely because overlays are merged first.
        let mut recalled = Vec::with_capacity(base_anchors.len() + overlay_anchors.len());
        recalled.extend(base_anchors);
        recalled.extend(overlay_anchors);
        recalled.sort_by(|left, right| {
            right
                .candidate
                .tier
                .rank()
                .cmp(&left.candidate.tier.rank())
                .then_with(|| {
                    candidate_origin_priority(right.origin)
                        .cmp(&candidate_origin_priority(left.origin))
                })
                .then_with(|| left.anchor.path.cmp(&right.anchor.path))
                .then_with(|| {
                    left.anchor
                        .name_range
                        .start_byte
                        .cmp(&right.anchor.name_range.start_byte)
                })
                .then_with(|| {
                    left.anchor
                        .anchor_fingerprint
                        .cmp(&right.anchor.anchor_fingerprint)
                })
        });
        if recalled.len() > self.exact_name_limit {
            recalled.truncate(self.exact_name_limit);
            truncated = true;
        }
        let mut base_anchors = Vec::new();
        let mut overlay_anchors = Vec::new();
        for candidate in recalled {
            match candidate.origin {
                CandidateOrigin::Base => base_anchors.push(candidate),
                CandidateOrigin::Overlay => overlay_anchors.push(candidate),
            }
        }
        let source_paths: HashSet<_> = base_anchors
            .iter()
            .chain(overlay_anchors.iter())
            .filter(|candidate| crate::query::is_source_path(&candidate.anchor.path))
            .map(|candidate| candidate.anchor.path.clone())
            .collect();
        let mut source_reach: HashMap<String, ReachScope> = HashMap::new();
        if let Some(graph) = self.reach_graph {
            for path in &source_paths {
                source_reach
                    .entry(path.clone())
                    .or_insert_with(|| graph.reachable(path).as_ref().clone());
            }
        }
        let coverage = CandidateCoverage {
            scanned,
            truncated,
            // Counterpart uniqueness needs a closed scope for every source
            // that could match a declaration, not only the current request.
            scope_open: source_paths
                .iter()
                .any(|path| source_reach.get(path).is_none_or(|scope| scope.open)),
            incomplete_reason: if self.overlays.has_incomplete_facts() {
                Some(crate::query::CandidateIncompleteReason::Cancelled)
            } else {
                fallback_used.then_some(crate::query::CandidateIncompleteReason::FactsUnavailable)
            },
        };
        let mut visible_internal_paths = self
            .current_reach
            .as_ref()
            .map(|scope| scope.files.clone())
            .unwrap_or_default();
        visible_internal_paths.insert(self.current_path.to_string());
        Ok(resolve_callable_candidates(CallableQueryInput {
            base_anchors,
            overlay_anchors,
            shadowed_paths: self.overlays.shadowed_paths().clone(),
            call_context,
            source_reach,
            visible_internal_paths,
            coverage,
        }))
    }

    /// Request-local reach scope after dirty include edges replace their
    /// published counterparts. Generic consumers use this instead of ranking
    /// live symbols against the stale base graph.
    pub fn effective_current_reach(&self) -> Option<&ReachScope> {
        self.current_reach.as_deref()
    }

    /// Normalize source provenance and first-layer evidence against the same
    /// request-local graph used for reachability. Durable bits are retained
    /// only when no graph exists; once dirty edges have produced an effective
    /// graph, it is the authoritative evidence for bounded candidate ranking.
    fn path_evidence(
        &self,
        path: &str,
        durable_external: bool,
        durable_directly_included: bool,
    ) -> (bool, bool) {
        let external = durable_external || Path::new(path).is_absolute();
        if !external {
            return (false, false);
        }
        let directly_included = self.reach_graph.map_or(durable_directly_included, |graph| {
            graph.directly_includes_external(self.current_path, path)
        });
        (true, directly_included)
    }

    /// Return parser-produced complete-call evidence only when the cursor is
    /// on the callee token. A shadowed path never falls through to stale rows.
    pub fn complete_call_context_at(
        &self,
        position: SourcePosition,
    ) -> Result<Option<CallSiteContext>> {
        let calls = if self.overlays.shadows(self.current_path) {
            self.overlays
                .call_sites_at(self.current_path, position)
                .into_iter()
                .cloned()
                .collect()
        } else {
            match self.handle {
                Some(handle) => handle.read(|store| {
                    let (rows, _) = store.call_fact_view().call_sites_at_limited(
                        self.current_path,
                        position.line,
                        position.character,
                        DEFAULT_EXACT_NAME_CANDIDATE_LIMIT,
                    )?;
                    Ok(rows.into_iter().map(call_from_row).collect())
                })?,
                None => Vec::new(),
            }
        };
        Ok(calls
            .iter()
            .find_map(|call| CallSiteContext::from_complete_call(call, position)))
    }

    /// Find an exact callable anchor under the cursor for the special
    /// declaration/definition opposite-only Definition policy.
    pub fn anchor_at(&self, position: SourcePosition) -> Result<Option<CallableAnchor>> {
        if self.overlays.shadows(self.current_path) {
            return Ok(self
                .overlays
                .callable_by_path(self.current_path)
                .iter()
                .find(|anchor| position_in_range(position, anchor.name_range))
                .cloned());
        }
        let Some(handle) = self.handle else {
            return Ok(None);
        };
        handle.read(|store| {
            let (rows, _) = store.call_fact_view().anchors_at_limited(
                self.current_path,
                position.line,
                position.character,
                DEFAULT_EXACT_NAME_CANDIDATE_LIMIT,
            )?;
            Ok(rows
                .into_iter()
                .map(anchor_from_row)
                .find(|anchor| position_in_range(position, anchor.name_range)))
        })
    }

    /// Durable + live fallback symbols for non-callable features. Function
    /// consumers must use `callable_candidates`; filtering here prevents the
    /// legacy symbol table from silently becoming a second callable source.
    pub fn non_callable_symbols(&self, name: &str) -> Result<Vec<SymbolRecord>> {
        let mut records: Vec<_> = self
            .overlays
            .symbols(name)
            .iter()
            .filter(|fact| fact.symbol.kind != crate::semantic_model::SymbolKind::Function)
            .map(|fact| {
                let mut record = overlay_symbol_record(fact);
                let (external, directly_included) = self.path_evidence(
                    &record.path,
                    record.source == "external",
                    record.directly_included,
                );
                record.source = if external { "external" } else { "workspace" }.into();
                record.directly_included = directly_included;
                record
            })
            .filter(|record| self.non_callable_record_is_visible(record))
            .collect();
        let (current_paths, reachable_paths) = self.durable_priority_path_groups();
        let mut base = match self.handle {
            Some(handle) => handle.read(|store| {
                let view = store.symbol_read_view();
                let (global, truncated) =
                    view.symbols_by_name_limited(name, DEFAULT_EXACT_NAME_CANDIDATE_LIMIT)?;
                let mut base = Vec::new();
                if truncated {
                    for paths in [&current_paths, &reachable_paths] {
                        let remaining =
                            DEFAULT_EXACT_NAME_CANDIDATE_LIMIT.saturating_sub(base.len());
                        let (rows, _) =
                            view.symbols_by_name_in_paths_limited(name, paths, remaining)?;
                        base.extend(rows);
                    }
                }
                // Preserve a bounded unscoped fallback for projects whose
                // reach graph is absent/open, then remove overlap by row id.
                base.extend(global);
                let mut seen_ids = HashSet::new();
                base.retain(|record| seen_ids.insert(record.id));
                Ok(base)
            })?,
            None => Vec::new(),
        };
        base.retain(|record| {
            record.kind != "function"
                && !self.overlays.shadows(&record.path)
                && self.non_callable_record_is_visible(record)
        });
        for record in &mut base {
            let (external, directly_included) = self.path_evidence(
                &record.path,
                record.source == "external",
                record.directly_included,
            );
            record.source = if external { "external" } else { "workspace" }.into();
            record.directly_included = directly_included;
        }
        records.extend(base);
        let resolve_context = ResolveContext {
            current_path: Some(self.current_path),
            reach: self.current_reach.as_deref(),
            direct_external_files: None,
        };
        records.sort_by(|left, right| {
            let left_tier = resolver::scope_tier(
                &left.path,
                left.source == "external",
                left.directly_included,
                Some(&resolve_context),
            );
            let right_tier = resolver::scope_tier(
                &right.path,
                right.source == "external",
                right.directly_included,
                Some(&resolve_context),
            );
            right_tier
                .rank()
                .cmp(&left_tier.rank())
                .then_with(|| left.path.cmp(&right.path))
                .then(left.start_line.cmp(&right.start_line))
                .then(left.start_col.cmp(&right.start_col))
                .then(left.kind.cmp(&right.kind))
        });
        records.dedup_by(|left, right| {
            left.path == right.path
                && left.start_line == right.start_line
                && left.start_col == right.start_col
                && left.kind == right.kind
        });
        records.truncate(DEFAULT_EXACT_NAME_CANDIDATE_LIMIT);
        Ok(records)
    }

    fn non_callable_record_is_visible(&self, record: &SymbolRecord) -> bool {
        !symbol_record_has_internal_linkage(record)
            || self.path_is_in_current_translation_unit(&record.path)
    }

    fn path_is_in_current_translation_unit(&self, path: &str) -> bool {
        path == self.current_path
            || self
                .current_reach
                .as_ref()
                .is_some_and(|scope| scope.files.contains(path))
    }

    /// Revision evidence for bounded lazy source hydration. The metadata comes
    /// from the same generation-pinned handle as candidate recall, so a later
    /// disk edit cannot be mistaken for the candidate's source revision.
    pub fn source_revisions(&self, paths: &[String]) -> Result<HashMap<String, CandidateRevision>> {
        let Some(handle) = self.handle else {
            return Ok(HashMap::new());
        };
        handle.read(|store| {
            store.stored_files(paths).map(|files| {
                files
                    .into_iter()
                    .map(|(path, file)| {
                        (
                            path,
                            CandidateRevision {
                                id: file.id,
                                size: file.size,
                                mtime_ns: file.mtime_ns,
                                hash: file.hash,
                            },
                        )
                    })
                    .collect()
            })
        })
    }

    /// Recall record and alias facts for one exact-name request, expanding
    /// only exact alias targets under a strict visit bound. All durable rows
    /// remain generation-pinned and every dirty path shadows its base rows.
    pub fn type_candidates(&self, name: &str) -> Result<TypeCandidateBundle> {
        let resolve_context = ResolveContext {
            current_path: Some(self.current_path),
            reach: self.current_reach.as_deref(),
            direct_external_files: None,
        };
        let mut names = vec![name.to_string()];
        let mut visited_names = HashSet::new();
        let mut records = Vec::new();
        let mut aliases = Vec::new();
        let mut scanned = 0usize;
        let mut truncated = false;
        let mut shadowed_evidence = false;

        while let Some(next_name) = names.pop() {
            if visited_names.len() >= ALIAS_RESOLUTION_MAX_VISITS
                || !visited_names.insert(next_name.clone())
            {
                if visited_names.len() >= ALIAS_RESOLUTION_MAX_VISITS {
                    truncated = true;
                }
                continue;
            }
            let (base_records, record_truncated, base_aliases, alias_truncated) = match self.handle
            {
                Some(handle) => handle.read(|store| {
                    let (record_rows, record_truncated) = store
                        .member_view()
                        .record_rows_by_name_limited(&next_name, TYPE_CANDIDATE_LIMIT)?;
                    let (alias_rows, alias_truncated) = store
                        .member_view()
                        .alias_rows_by_name_limited(&next_name, TYPE_CANDIDATE_LIMIT)?;
                    Ok((record_rows, record_truncated, alias_rows, alias_truncated))
                })?,
                None => (Vec::new(), false, Vec::new(), false),
            };
            scanned += base_records.len() + base_aliases.len();
            truncated |= record_truncated || alias_truncated;
            for row in base_records {
                if self.overlays.shadows(&row.path) {
                    shadowed_evidence = true;
                    continue;
                }
                let (external, directly_included) =
                    self.path_evidence(&row.path, row.external, row.directly_included);
                let tier = resolver::scope_tier(
                    &row.path,
                    external,
                    directly_included,
                    Some(&resolve_context),
                );
                records.push(RecordCandidate::from_read_row(row, tier));
            }
            let mut converted_aliases = Vec::new();
            for row in base_aliases {
                if self.overlays.shadows(&row.path) {
                    shadowed_evidence = true;
                    continue;
                }
                if let Some(alias) = {
                    let (external, directly_included) =
                        self.path_evidence(&row.path, row.external, row.directly_included);
                    let tier = resolver::scope_tier(
                        &row.path,
                        external,
                        directly_included,
                        Some(&resolve_context),
                    );
                    TypeAliasCandidate::from_read_row(row, tier)
                } {
                    converted_aliases.push(alias);
                }
            }
            enqueue_alias_targets(&converted_aliases, &mut names);
            aliases.extend(converted_aliases);

            let overlay_records = self.overlays.records(&next_name);
            let overlay_aliases = self.overlays.aliases(&next_name);
            scanned += overlay_records.len() + overlay_aliases.len();
            records.extend(overlay_records.iter().map(|fact| {
                let (external, directly_included) =
                    self.path_evidence(&fact.path, Path::new(&fact.path).is_absolute(), false);
                let tier = resolver::scope_tier(
                    &fact.path,
                    external,
                    directly_included,
                    Some(&resolve_context),
                );
                RecordCandidate::from_overlay(fact.path.clone(), fact.record.clone(), tier)
            }));
            let converted_overlay_aliases: Vec<_> = overlay_aliases
                .iter()
                .map(|fact| {
                    let (external, directly_included) =
                        self.path_evidence(&fact.path, Path::new(&fact.path).is_absolute(), false);
                    let tier = resolver::scope_tier(
                        &fact.path,
                        external,
                        directly_included,
                        Some(&resolve_context),
                    );
                    let mut alias = TypeAliasCandidate::from_overlay(
                        fact.path.clone(),
                        fact.alias.clone(),
                        tier,
                    );
                    bind_overlay_alias_to_unique_same_file_record(&mut alias, self.overlays);
                    alias
                })
                .collect();
            enqueue_alias_targets(&converted_overlay_aliases, &mut names);
            aliases.extend(converted_overlay_aliases);
        }

        let stable_targets: HashSet<_> = aliases
            .iter()
            .filter_map(|alias| match &alias.target {
                TypeAliasTarget::StableRecord(identity) => Some(identity.clone()),
                _ => None,
            })
            .collect();
        for identity in stable_targets {
            if records.iter().any(|record| record.identity == identity) {
                continue;
            }
            match identity {
                RecordCandidateIdentity::Persistent(id) => {
                    let row = match self.handle {
                        Some(handle) => {
                            handle.read(|store| store.member_view().record_row_by_id(id))?
                        }
                        None => None,
                    };
                    if let Some(row) = row {
                        if self.overlays.shadows(&row.path) {
                            shadowed_evidence = true;
                            if let Some(fact) = unique_overlay_replacement_for_record(
                                self.overlays.records_for_path(&row.path),
                                &row,
                            ) {
                                scanned += 1;
                                let (external, directly_included) = self.path_evidence(
                                    &row.path,
                                    row.external,
                                    row.directly_included,
                                );
                                let tier = resolver::scope_tier(
                                    &row.path,
                                    external,
                                    directly_included,
                                    Some(&resolve_context),
                                );
                                let replacement = RecordCandidate::from_overlay(
                                    row.path.clone(),
                                    fact.record.clone(),
                                    tier,
                                );
                                remap_persistent_alias_targets(
                                    &mut aliases,
                                    id,
                                    replacement.identity.clone(),
                                );
                                records.push(replacement);
                            }
                        } else {
                            scanned += 1;
                            let (external, directly_included) =
                                self.path_evidence(&row.path, row.external, row.directly_included);
                            let tier = resolver::scope_tier(
                                &row.path,
                                external,
                                directly_included,
                                Some(&resolve_context),
                            );
                            records.push(RecordCandidate::from_read_row(row, tier));
                        }
                    }
                }
                RecordCandidateIdentity::ParserKey { path, record_key } => {
                    if let Some(fact) = self.overlays.record_by_parser_key(&path, &record_key) {
                        scanned += 1;
                        let (external, directly_included) =
                            self.path_evidence(&path, Path::new(&path).is_absolute(), false);
                        let tier = resolver::scope_tier(
                            &path,
                            external,
                            directly_included,
                            Some(&resolve_context),
                        );
                        records.push(RecordCandidate::from_overlay(
                            path,
                            fact.record.clone(),
                            tier,
                        ));
                    }
                }
            }
        }

        let coverage = CandidateCoverage {
            scanned,
            truncated,
            scope_open: self.current_reach.as_ref().is_some_and(|scope| scope.open),
            incomplete_reason: if self.overlays.has_incomplete_facts() {
                Some(crate::query::CandidateIncompleteReason::Cancelled)
            } else {
                truncated.then_some(crate::query::CandidateIncompleteReason::CandidateBudget)
            },
        };
        let root_records = record_candidates_exact(
            name,
            records.clone(),
            coverage.clone(),
            TYPE_CANDIDATE_LIMIT,
        );
        let root_aliases = type_alias_candidates_exact(
            name,
            aliases.clone(),
            coverage.clone(),
            TYPE_CANDIDATE_LIMIT,
        );
        let alias_resolutions = root_aliases
            .candidates
            .iter()
            .cloned()
            .map(|alias| resolve_type_alias(alias, &aliases, &records, coverage.clone()))
            .collect();
        Ok(TypeCandidateBundle {
            records: root_records,
            aliases: root_aliases,
            alias_resolutions,
            shadowed_evidence,
            trace_records: records,
        })
    }

    /// Resolve terminal records while retaining whether the shared candidate
    /// facade found authoritative root or tombstone evidence. An empty record
    /// list with `true` means “resolved to no live terminal”, not “try a stale
    /// generation-unaware fallback”.
    pub fn records_for_type_name_with_evidence(&self, name: &str) -> Result<TypeRecordResolution> {
        let bundle = self.type_candidates(name)?;
        let authoritative = bundle.shadowed_evidence
            || !bundle.records.candidates.is_empty()
            || !bundle.aliases.candidates.is_empty();
        let mut incomplete = !bundle.records.coverage.permits_uniqueness()
            || !bundle.aliases.coverage.permits_uniqueness();
        let mut ambiguous = false;
        let mut records = bundle.records.candidates;
        for resolution in bundle.alias_resolutions {
            ambiguous |= resolution.status == AliasResolutionStatus::AmbiguousRecord;
            incomplete |= resolution.status != AliasResolutionStatus::UniqueRecord;
            records.extend(resolution.terminal_records);
        }
        records.sort_by(|left, right| {
            right
                .tier
                .rank()
                .cmp(&left.tier.rank())
                .then_with(|| left.path.cmp(&right.path))
                .then_with(|| left.name_range.start_byte.cmp(&right.name_range.start_byte))
        });
        let mut identities = HashSet::new();
        records.retain(|record| identities.insert(record.identity.clone()));
        if let Some(highest_rank) = records.iter().map(|record| record.tier.rank()).max() {
            ambiguous |= records
                .iter()
                .filter(|record| record.tier.rank() == highest_rank)
                .count()
                > 1;
        }
        incomplete |= ambiguous;
        Ok(TypeRecordResolution {
            records,
            authoritative,
            incomplete,
            ambiguous,
        })
    }

    /// Fetch member evidence for request-local record identities. Persistent
    /// IDs are read from the pinned generation; parser identities read only
    /// the dirty overlay and therefore naturally replace stale base fields.
    /// Fetch resolved-owner members under one scan budget shared by live
    /// parser records and pinned durable rows. Live facts are consumed first
    /// so a large clean record cannot crowd a dirty/current owner out of the
    /// bounded working set.
    pub fn members_for_records_limited(
        &self,
        records: &[RecordCandidate],
        member_name: Option<&str>,
        scan_limit: usize,
    ) -> Result<BoundedMemberCandidates> {
        let resolve_context = ResolveContext {
            current_path: Some(self.current_path),
            reach: self.current_reach.as_deref(),
            direct_external_files: None,
        };
        let mut persistent_ids: Vec<_> = records
            .iter()
            .filter_map(|record| match record.identity {
                RecordCandidateIdentity::Persistent(id) => Some(id),
                RecordCandidateIdentity::ParserKey { .. } => None,
            })
            .collect();
        persistent_ids.sort_unstable();
        persistent_ids.dedup();
        let mut tier_by_path = HashMap::new();
        for record in records {
            tier_by_path
                .entry(record.path.as_str())
                .and_modify(|tier: &mut crate::model::ScopeTier| {
                    if record.tier.rank() > tier.rank() {
                        *tier = record.tier;
                    }
                })
                .or_insert(record.tier);
        }
        let mut members = Vec::new();
        let mut scanned = 0usize;
        let mut truncated = false;
        let mut seen_parser_records = HashSet::new();
        for record in records {
            let RecordCandidateIdentity::ParserKey { path, record_key } = &record.identity else {
                continue;
            };
            if !seen_parser_records.insert((path.as_str(), record_key.as_str())) {
                continue;
            }
            let owner_revision_hash = self
                .overlays
                .source_text(path)
                .map(|source| blake3::hash(source.as_bytes()).to_hex().to_string());
            for member in self.overlays.members_for_parser_record(path, record_key) {
                if scanned >= scan_limit {
                    truncated = true;
                    break;
                }
                scanned += 1;
                if member_name.is_some_and(|name| member.name != name) {
                    continue;
                }
                members.push(MemberCandidate {
                    name: member.name.clone(),
                    kind: member.kind,
                    signature: member.signature.clone(),
                    type_name: member.type_name.clone(),
                    tier: record.tier,
                    confidence: member.confidence,
                    owner_path: path.clone(),
                    owner_revision_hash: owner_revision_hash.clone(),
                });
            }
            if truncated {
                break;
            }
        }

        if !truncated && !persistent_ids.is_empty() {
            let remaining = scan_limit.saturating_sub(scanned);
            let (mut durable, durable_scanned, durable_truncated) = match self.handle {
                Some(handle) => handle.read(|store| {
                    store.member_view().members_for_records_limited(
                        &persistent_ids,
                        member_name,
                        Some(&resolve_context),
                        remaining,
                    )
                })?,
                None => (Vec::new(), 0, false),
            };
            scanned = scanned.saturating_add(durable_scanned);
            truncated |= durable_truncated;
            durable.retain(|member| !self.overlays.shadows(&member.owner_path));
            for member in &mut durable {
                if let Some(tier) = tier_by_path.get(member.owner_path.as_str()) {
                    member.tier = *tier;
                }
            }
            members.extend(durable);
        }
        members.sort_by(|left, right| {
            right
                .tier
                .rank()
                .cmp(&left.tier.rank())
                .then_with(|| left.owner_path.cmp(&right.owner_path))
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.kind.as_str().cmp(right.kind.as_str()))
        });
        members.dedup_by(|left, right| {
            left.owner_path == right.owner_path
                && left.name == right.name
                && left.kind == right.kind
                && left.signature == right.signature
        });
        Ok(BoundedMemberCandidates {
            candidates: members,
            scanned,
            truncated,
        })
    }

    /// Bounded global member fallback with the same all-open tombstones as
    /// exact owner resolution. Durable rows from every dirty owner path are
    /// removed, then current-buffer member facts are added back.
    pub fn fallback_member_candidates(
        &self,
        prefix: &str,
        limit: usize,
    ) -> Result<(Vec<MemberCandidate>, bool)> {
        let resolve_context = ResolveContext {
            current_path: Some(self.current_path),
            reach: self.current_reach.as_deref(),
            direct_external_files: None,
        };
        let (mut members, mut truncated) = match self.handle {
            Some(handle) => handle.read(|store| {
                store.member_view().fallback_member_candidates_limited(
                    prefix,
                    limit,
                    Some(&resolve_context),
                )
            })?,
            None => (Vec::new(), false),
        };
        members.retain(|member| !self.overlays.shadows(&member.owner_path));

        let (overlay_members, overlay_truncated) = self
            .overlays
            .fallback_members_by_prefix_limited(prefix, MEMBER_FALLBACK_OVERLAY_SCAN_LIMIT);
        truncated |= overlay_truncated;
        for fact in overlay_members {
            let path = &fact.path;
            let member = &fact.member;
            let (external, directly_included) =
                self.path_evidence(path, Path::new(path).is_absolute(), false);
            let tier =
                resolver::scope_tier(path, external, directly_included, Some(&resolve_context));
            let owner_revision_hash = self
                .overlays
                .source_text(path)
                .map(|source| blake3::hash(source.as_bytes()).to_hex().to_string());
            members.push(MemberCandidate {
                name: member.name.clone(),
                kind: member.kind,
                signature: member.signature.clone(),
                type_name: member.type_name.clone(),
                tier,
                confidence: member.confidence,
                owner_path: path.clone(),
                owner_revision_hash,
            });
        }
        members.sort_by(|left, right| {
            right
                .tier
                .rank()
                .cmp(&left.tier.rank())
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.owner_path.cmp(&right.owner_path))
                .then_with(|| left.signature.cmp(&right.signature))
        });
        members.dedup_by(|left, right| {
            left.owner_path == right.owner_path
                && left.name == right.name
                && left.kind == right.kind
                && left.signature == right.signature
        });
        truncated |= members.len() > limit;
        members.truncate(limit);
        Ok((members, truncated))
    }
}

fn unique_overlay_replacement_for_record<'a>(
    facts: &'a [OverlayRecordFact],
    row: &crate::store::views::RecordReadRow,
) -> Option<&'a OverlayRecordFact> {
    let mut matches = facts.iter().filter(|fact| {
        fact.record.kind == row.kind
            && (fact.record.display_name == row.display_name
                || row
                    .tag_name
                    .as_ref()
                    .is_some_and(|name| fact.record.tag_name.as_ref() == Some(name))
                || row
                    .typedef_name
                    .as_ref()
                    .is_some_and(|name| fact.record.typedef_name.as_ref() == Some(name)))
    });
    let found = matches.next()?;
    matches.next().is_none().then_some(found)
}

/// Tree-sitter represents `typedef B Active` as an unresolved type spelling.
/// In C++ (and in C after a prior typedef), that spelling may name a record
/// directly. Bind it only when this same dirty file contains exactly one
/// matching parser record; otherwise preserve the ordinary alias-chain path.
/// This keeps a dirty typedef retarget on the same immutable overlay instead
/// of falling back to the stale durable target record.
fn bind_overlay_alias_to_unique_same_file_record(
    alias: &mut TypeAliasCandidate,
    overlays: &CandidateOverlaySnapshot,
) {
    let TypeAliasTarget::TypeName(target_name) = &alias.target else {
        return;
    };
    let mut matching = overlays
        .records(target_name)
        .iter()
        .filter(|fact| fact.path == alias.path);
    let Some(record) = matching.next() else {
        return;
    };
    if matching.next().is_some() {
        return;
    }
    alias.target = TypeAliasTarget::StableRecord(RecordCandidateIdentity::ParserKey {
        path: record.path.clone(),
        record_key: record.record.record_key.clone(),
    });
}

fn remap_persistent_alias_targets(
    aliases: &mut [TypeAliasCandidate],
    persistent_id: i64,
    replacement: RecordCandidateIdentity,
) {
    for alias in aliases {
        if alias.target
            == TypeAliasTarget::StableRecord(RecordCandidateIdentity::Persistent(persistent_id))
        {
            alias.target = TypeAliasTarget::StableRecord(replacement.clone());
        }
    }
}

fn enqueue_alias_targets(aliases: &[TypeAliasCandidate], names: &mut Vec<String>) {
    for alias in aliases {
        match &alias.target {
            TypeAliasTarget::NamedRecord { tag, .. } | TypeAliasTarget::TypeName(tag) => {
                names.push(tag.clone());
            }
            TypeAliasTarget::StableRecord(_) => {}
        }
    }
}

fn overlay_symbol_record(fact: &OverlaySymbolFact) -> SymbolRecord {
    SymbolRecord {
        // Negative ids are request-local and are never sent back as durable
        // identifiers. Range/path form the fallback identity.
        id: -1,
        name: fact.symbol.name.clone(),
        kind: match fact.symbol.kind {
            crate::semantic_model::SymbolKind::Function => "function",
            crate::semantic_model::SymbolKind::Macro => "macro",
            crate::semantic_model::SymbolKind::Type => "type",
            crate::semantic_model::SymbolKind::EnumConstant => "enum_constant",
            crate::semantic_model::SymbolKind::GlobalVariable => "global_variable",
            crate::semantic_model::SymbolKind::Field => "field",
        }
        .into(),
        role: match fact.symbol.role {
            SymbolRole::Definition => "definition",
            SymbolRole::Declaration => "declaration",
            SymbolRole::TentativeDefinition => "tentative_definition",
            SymbolRole::UnknownDeclarationOrDefinition => "unknown_declaration_or_definition",
        }
        .into(),
        path: fact.path.clone(),
        start_line: fact.symbol.start_line as u32,
        start_col: fact.symbol.start_col as u32,
        end_line: fact.symbol.end_line as u32,
        end_col: fact.symbol.end_col as u32,
        signature: fact.symbol.signature.clone(),
        guard: fact.symbol.guard.clone(),
        source: if Path::new(&fact.path).is_absolute() {
            "external"
        } else {
            "workspace"
        }
        .into(),
        directly_included: false,
    }
}

fn symbol_record_has_internal_linkage(record: &SymbolRecord) -> bool {
    record.kind == "global_variable"
        && record
            .signature
            .split(|ch: char| ch != '_' && !ch.is_ascii_alphanumeric())
            .any(|token| token == "static")
}

fn resolved_anchor(
    anchor: CallableAnchor,
    source: String,
    tier: crate::model::ScopeTier,
    origin: CandidateOrigin,
) -> ResolvedCallableAnchor {
    let base_match = if anchor.role == crate::call_model::AnchorRole::Definition {
        1_000
    } else {
        900
    };
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
        source,
        tier,
        base_match,
        confidence,
        reason,
    };
    ResolvedCallableAnchor::new(anchor, candidate, origin)
}

fn resolved_lexical_function(
    record: SymbolRecord,
    tier: crate::model::ScopeTier,
    origin: CandidateOrigin,
) -> ResolvedCallableAnchor {
    let position_range = SourceRange {
        start: SourcePosition {
            line: record.start_line,
            character: record.start_col,
        },
        end: SourcePosition {
            line: record.end_line,
            character: record.end_col,
        },
        start_byte: 0,
        end_byte: 0,
    };
    let role = if record.role == "definition" {
        AnchorRole::Definition
    } else {
        AnchorRole::Declaration
    };
    let fingerprint = blake3::hash(
        format!(
            "lexical|{}|{}|{}|{}|{}|{}",
            record.path,
            record.name,
            record.start_line,
            record.start_col,
            record.end_line,
            record.end_col
        )
        .as_bytes(),
    )
    .to_hex()[..24]
        .to_string();
    let entity_key = blake3::hash(
        format!(
            "lexical|{}|{}|{}",
            record.path, record.name, record.signature
        )
        .as_bytes(),
    )
    .to_hex()[..24]
        .to_string();
    let linkage = if record
        .signature
        .split(|ch: char| !(ch == '_' || ch.is_ascii_alphanumeric()))
        .any(|token| token == "static")
    {
        LinkageDomain::Internal(record.path.clone())
    } else {
        LinkageDomain::External
    };
    let anchor = CallableAnchor {
        path: record.path.clone(),
        name: record.name.clone(),
        qualified_name: record.name.clone(),
        owner: None,
        owner_kind: None,
        kind: CallableKind::Function,
        role,
        linkage,
        signature: SignatureShape {
            normalized: record.signature.clone(),
            min_arity: None,
            max_arity: None,
            variadic: false,
        },
        canonical_signature: String::new(),
        presentation_signature: record.signature.clone(),
        signature_fidelity: SignatureFidelity::LexicalFallback,
        name_range: position_range,
        declaration_range: position_range,
        body_range: None,
        guard: record.guard.clone(),
        provenance: FactProvenance::LexicalFallback,
        syntax_error_overlap: true,
        entity_key,
        anchor_fingerprint: fingerprint,
    };
    resolved_anchor(anchor, record.source, tier, origin)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tempfile::tempdir;

    use super::*;
    use crate::parser::{parse_with_handle, ParseFacts};
    use crate::store::{FileFingerprint, FileSource, IndexStore};

    fn absolute_test_path(name: &str) -> String {
        std::env::temp_dir()
            .join("fossilsense-candidate-overlay")
            .join(name)
            .to_string_lossy()
            .replace('\\', "/")
    }

    fn upsert_candidate_test_file(store: &mut IndexStore, path: &str, source: &str) {
        let parsed = parse_with_handle(Path::new(path), source, None, ParseFacts::HOVER_SEMANTICS);
        store
            .upsert_file_index_with_source(
                &FileFingerprint {
                    path: path.to_string(),
                    extension: path.rsplit('.').next().unwrap_or("c").to_string(),
                    size: source.len() as u64,
                    mtime_ns: 1,
                    hash: format!("{path}-scope-recall"),
                },
                &parsed,
                FileSource::Workspace,
            )
            .expect("upsert candidate fixture");
    }

    #[test]
    fn completion_projection_keeps_late_dirty_symbol_after_large_unrelated_prefix() {
        let mut source = String::new();
        for index in 0..(DEFAULT_EXACT_NAME_CANDIDATE_LIMIT * 8 + 32) {
            source.push_str(&format!("int unrelated_{index}(void);\n"));
        }
        source.push_str("int late_overlay_target(void);\n");
        let parsed = parse_with_handle(Path::new("late.h"), &source, None, ParseFacts::COMPLETION);
        let snapshot = CandidateOverlaySnapshot::new(
            1,
            vec![FileCandidateOverlay::from_index("late.h".into(), &parsed)],
        );

        let names = snapshot.completion_names();
        assert!(names.len() > DEFAULT_EXACT_NAME_CANDIDATE_LIMIT * 8);
        assert!(names
            .iter()
            .any(|entry| entry.name == "late_overlay_target"));
    }

    #[test]
    fn overlay_member_prefix_index_is_stable_and_reports_its_scan_cap() {
        let parsed = parse_with_handle(
            Path::new("members.h"),
            "struct Record { int alpine; int alpha; int beta; };\n",
            None,
            ParseFacts::MEMBER,
        );
        let snapshot = CandidateOverlaySnapshot::new(
            1,
            vec![FileCandidateOverlay::from_index(
                "members.h".into(),
                &parsed,
            )],
        );

        let (one, truncated) = snapshot.fallback_members_by_prefix_limited("al", 1);
        assert!(truncated);
        assert_eq!(one[0].member.name, "alpha");
        let (all, truncated) = snapshot.fallback_members_by_prefix_limited("al", 2);
        assert!(!truncated);
        assert_eq!(
            all.iter()
                .map(|fact| fact.member.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "alpine"]
        );
    }

    #[test]
    fn exact_name_indexes_merge_all_dirty_documents_and_shadow_empty_paths() {
        let first = parse_with_handle(
            Path::new("first.h"),
            "struct Packet { int size; };\ntypedef struct Packet PacketT;\nint pick(int x);\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let second = parse_with_handle(
            Path::new("second.h"),
            "int pick(int x, int y);\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let empty = parse_with_handle(
            Path::new("deleted.h"),
            "// the indexed declaration was deleted\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let snapshot = CandidateOverlaySnapshot::new(
            9,
            vec![
                FileCandidateOverlay::from_index("first.h".into(), &first),
                FileCandidateOverlay::from_index("second.h".into(), &second),
                FileCandidateOverlay::from_index("deleted.h".into(), &empty),
            ],
        );

        assert_eq!(snapshot.epoch, 9);
        assert_eq!(snapshot.callable_anchors("pick").len(), 2);
        assert_eq!(snapshot.records("Packet").len(), 1);
        assert_eq!(snapshot.aliases("PacketT").len(), 1);
        assert!(snapshot.shadows("deleted.h"));
        assert!(snapshot.callable_anchors("deleted").is_empty());
    }

    #[test]
    fn call_site_lookup_is_path_and_callee_range_bounded() {
        let parsed = parse_with_handle(
            Path::new("main.c"),
            "int pick(int); int main(void) { return pick(1); }\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let call = parsed.call_sites.first().expect("call site");
        let snapshot = CandidateOverlaySnapshot::new(
            1,
            vec![FileCandidateOverlay::from_index("main.c".into(), &parsed)],
        );
        assert_eq!(
            snapshot
                .call_sites_at("main.c", call.callee_range.start)
                .len(),
            1
        );
        assert!(snapshot
            .call_sites_at(
                "main.c",
                SourcePosition {
                    line: 0,
                    character: 0,
                },
            )
            .is_empty());
    }

    #[test]
    fn facade_applies_the_same_complete_call_arity_to_overlay_candidates() {
        let one = parse_with_handle(
            Path::new("one.h"),
            "int pick(int value);\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let two = parse_with_handle(
            Path::new("two.h"),
            "int pick(int left, int right);\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let main = parse_with_handle(
            Path::new("main.c"),
            "int main(void) { return pick(1, 2); }\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let call = main.call_sites.first().expect("pick call");
        let snapshot = CandidateOverlaySnapshot::new(
            4,
            vec![
                FileCandidateOverlay::from_index("one.h".into(), &one),
                FileCandidateOverlay::from_index("two.h".into(), &two),
                FileCandidateOverlay::from_index("main.c".into(), &main),
            ],
        );
        let service = CandidateQueryService::new(None, &snapshot, "main.c", None, None);
        let context = service
            .complete_call_context_at(call.callee_range.start)
            .expect("context query")
            .expect("complete call context");
        let candidates = service
            .callable_candidates("pick", Some(context))
            .expect("candidate query");
        assert_eq!(candidates.anchors.len(), 1);
        assert_eq!(candidates.anchors[0].anchor.signature.max_arity, Some(2));
    }

    #[test]
    fn facade_builds_strict_counterpart_groups_from_source_reach() {
        let header = parse_with_handle(
            Path::new("api.h"),
            "int api(int value);\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let source = parse_with_handle(
            Path::new("api.c"),
            "int api(int value) { return value; }\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let snapshot = CandidateOverlaySnapshot::new(
            5,
            vec![
                FileCandidateOverlay::from_index("api.h".into(), &header),
                FileCandidateOverlay::from_index("api.c".into(), &source),
            ],
        );
        let graph = ReachGraph::new(
            vec![("api.c".into(), "api.h".into())],
            Vec::new(),
            Vec::new(),
        );
        let current_reach = graph.reachable("api.c");
        let service = CandidateQueryService::new(
            None,
            &snapshot,
            "api.c",
            Some(&current_reach),
            Some(&graph),
        );
        let candidates = service
            .callable_candidates("api", None)
            .expect("candidate query");
        assert_eq!(candidates.groups.len(), 1);
        assert_eq!(
            candidates.groups[0].counterpart_evidence,
            crate::query::CounterpartEvidence::StrictOneToOne
        );
        assert_eq!(
            crate::query::hover_presentations(&candidates.groups)[0]
                .anchor
                .path,
            "api.h"
        );
        assert_eq!(
            crate::query::call_definition_presentations(&candidates.groups)[0]
                .anchor
                .path,
            "api.c"
        );
    }

    #[test]
    fn incomplete_dirty_facts_disable_callable_and_alias_uniqueness() {
        let header = parse_with_handle(
            Path::new("api.h"),
            "typedef struct Packet { int id; } PacketT;\nint api(int value);\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let source = parse_with_handle(
            Path::new("api.c"),
            "int api(int value) { return value; }\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let snapshot = CandidateOverlaySnapshot::new(
            5,
            vec![
                FileCandidateOverlay::from_index("api.h".into(), &header),
                FileCandidateOverlay::from_index("api.c".into(), &source),
                FileCandidateOverlay::tombstone("second.h".into(), Arc::from("int api(int);\n")),
            ],
        );
        let graph = ReachGraph::new(
            vec![("api.c".into(), "api.h".into())],
            Vec::new(),
            Vec::new(),
        );
        let service = CandidateQueryService::new(None, &snapshot, "api.c", None, Some(&graph));
        let callable = service
            .callable_candidates("api", None)
            .expect("callable candidates");
        assert_eq!(
            callable.coverage.incomplete_reason,
            Some(crate::query::CandidateIncompleteReason::Cancelled)
        );
        assert!(callable.groups.iter().all(|group| {
            group.counterpart_evidence != crate::query::CounterpartEvidence::StrictOneToOne
        }));

        let types = service.type_candidates("PacketT").expect("type candidates");
        assert_eq!(
            types.alias_resolutions[0].status,
            crate::query::AliasResolutionStatus::Truncated
        );
        assert!(types.alias_resolutions[0].aka_spelling.is_none());
    }

    #[test]
    fn lexical_fallback_overlay_is_not_complete_semantic_evidence() {
        let mut parsed = parse_with_handle(
            Path::new("fallback.h"),
            "int api(int value);\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        parsed.diagnostics.fallback_used = true;
        parsed.diagnostics.ast_source = crate::parser::FactSource::LexicalFallback;
        parsed.callable_anchors.clear();
        parsed.call_sites.clear();
        parsed.records.clear();
        parsed.aliases.clear();
        let overlay = FileCandidateOverlay::from_index("fallback.h".into(), &parsed);
        assert!(!overlay.facts_complete);
        let snapshot = CandidateOverlaySnapshot::new(1, vec![overlay]);
        assert!(snapshot.has_incomplete_facts());
        let candidates = CandidateQueryService::new(None, &snapshot, "fallback.h", None, None)
            .callable_candidates("api", None)
            .expect("lexical callable fallback");
        assert_eq!(candidates.anchors.len(), 1);
        assert_eq!(
            candidates.anchors[0].anchor.signature_fidelity,
            SignatureFidelity::LexicalFallback
        );
        assert_eq!(
            candidates.coverage.incomplete_reason,
            Some(crate::query::CandidateIncompleteReason::Cancelled)
        );
    }

    #[test]
    fn unsupported_partial_member_call_never_binds_a_free_function() {
        let parsed = parse_with_handle(
            Path::new("api.h"),
            "int run(int value);\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let snapshot = CandidateOverlaySnapshot::new(
            1,
            vec![FileCandidateOverlay::from_index("api.h".into(), &parsed)],
        );
        let service = CandidateQueryService::new(None, &snapshot, "main.cpp", None, None);
        let context = CallSiteContext::partial(
            "run".into(),
            crate::call_model::CallForm::MemberDot,
            SourceRange {
                start: SourcePosition {
                    line: 0,
                    character: 4,
                },
                end: SourcePosition {
                    line: 0,
                    character: 7,
                },
                start_byte: 4,
                end_byte: 7,
            },
            0,
            0,
            ContextReliability::Reliable,
        );
        let candidates = service
            .callable_candidates("run", Some(context))
            .expect("candidate query");
        assert!(candidates.anchors.is_empty());
    }

    #[test]
    fn dirty_include_edges_replace_published_reach_for_the_whole_request() {
        let header = parse_with_handle(
            Path::new("api.h"),
            "int api(int value);\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let source_without_include = parse_with_handle(
            Path::new("api.c"),
            "int api(int value) { return value; }\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let published = ReachGraph::new(
            vec![("api.c".into(), "api.h".into())],
            Vec::new(),
            Vec::new(),
        );
        let mut removed = CandidateOverlaySnapshot::new(
            6,
            vec![
                FileCandidateOverlay::from_index("api.h".into(), &header),
                FileCandidateOverlay::from_index("api.c".into(), &source_without_include),
            ],
        );
        removed.refresh_reach_graph(Some(&published), ["api.c", "api.h"], &[]);
        let removed_set = CandidateQueryService::new(
            None,
            &removed,
            "api.c",
            Some(&published.reachable("api.c")),
            Some(&published),
        )
        .callable_candidates("api", None)
        .expect("removed include candidates");
        assert!(removed_set.groups.iter().all(|group| {
            group.counterpart_evidence != crate::query::CounterpartEvidence::StrictOneToOne
        }));

        let source_with_include = parse_with_handle(
            Path::new("api.c"),
            "#include \"api.h\"\nint api(int value) { return value; }\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let empty_published = ReachGraph::new(Vec::new(), Vec::new(), Vec::new());
        let mut added = CandidateOverlaySnapshot::new(
            7,
            vec![
                FileCandidateOverlay::from_index("api.h".into(), &header),
                FileCandidateOverlay::from_index("api.c".into(), &source_with_include),
            ],
        );
        added.refresh_reach_graph(Some(&empty_published), ["api.c", "api.h"], &[]);
        let added_set =
            CandidateQueryService::new(None, &added, "api.c", None, Some(&empty_published))
                .callable_candidates("api", None)
                .expect("added include candidates");
        assert_eq!(
            added_set.groups[0].counterpart_evidence,
            crate::query::CounterpartEvidence::StrictOneToOne
        );
    }

    #[test]
    fn dirty_suffix_include_stays_heuristic() {
        let header = parse_with_handle(
            Path::new("inc/api.h"),
            "int api(int value);\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let source = parse_with_handle(
            Path::new("src/api.c"),
            "#include \"api.h\"\nint api(int value) { return value; }\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let base = ReachGraph::new(Vec::new(), Vec::new(), Vec::new());
        let mut snapshot = CandidateOverlaySnapshot::new(
            8,
            vec![
                FileCandidateOverlay::from_index("inc/api.h".into(), &header),
                FileCandidateOverlay::from_index("src/api.c".into(), &source),
            ],
        );
        snapshot.refresh_reach_graph(Some(&base), ["src/api.c", "inc/api.h"], &[]);

        let scope = snapshot
            .effective_reach_graph(Some(&base))
            .expect("effective graph")
            .reachable("src/api.c");
        assert!(!scope.files.contains("inc/api.h"));
        assert!(scope.heuristic_files.contains("inc/api.h"));
    }

    #[test]
    fn dirty_ambiguous_include_retains_every_heuristic_target() {
        let first = parse_with_handle(
            Path::new("first/api.h"),
            "int first_api(void);\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let second = parse_with_handle(
            Path::new("second/api.h"),
            "int second_api(void);\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let source = parse_with_handle(
            Path::new("src/main.c"),
            "#include \"api.h\"\nint main(void) { return 0; }\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let base = ReachGraph::new(Vec::new(), Vec::new(), Vec::new());
        let mut snapshot = CandidateOverlaySnapshot::new(
            9,
            vec![
                FileCandidateOverlay::from_index("first/api.h".into(), &first),
                FileCandidateOverlay::from_index("second/api.h".into(), &second),
                FileCandidateOverlay::from_index("src/main.c".into(), &source),
            ],
        );
        snapshot.refresh_reach_graph(
            Some(&base),
            ["src/main.c", "first/api.h", "second/api.h"],
            &[],
        );

        let scope = snapshot
            .effective_reach_graph(Some(&base))
            .expect("effective graph")
            .reachable("src/main.c");
        assert!(scope.open);
        assert_eq!(
            scope.reason,
            Some(crate::reachability::OpenReason::AmbiguousInclude)
        );
        assert!(!scope.files.contains("first/api.h"));
        assert!(!scope.files.contains("second/api.h"));
        assert!(scope.heuristic_files.contains("first/api.h"));
        assert!(scope.heuristic_files.contains("second/api.h"));
    }

    #[test]
    fn dirty_external_overlay_uses_effective_direct_include_and_source_evidence() {
        let external = absolute_test_path("external_api.h");
        let external_parsed = parse_with_handle(
            Path::new(&external),
            "#define EXTERNAL_FLAG 1\nstruct ExternalRecord { int field; };\nint external_api(int value);\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let main_without_include = parse_with_handle(
            Path::new("main.c"),
            "int local_api(void);\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let published = ReachGraph::new(
            vec![("main.c".into(), external.clone())],
            Vec::new(),
            Vec::new(),
        );
        let mut removed = CandidateOverlaySnapshot::new(
            8,
            vec![
                FileCandidateOverlay::from_index(external.clone(), &external_parsed),
                FileCandidateOverlay::from_index("main.c".into(), &main_without_include),
            ],
        );
        removed.refresh_reach_graph(Some(&published), ["main.c"], &[]);

        assert_eq!(
            removed.direct_include_overrides().get(&external),
            Some(&false),
            "removing the dirty include must clear the published first-layer bit"
        );
        let removed_names = removed.completion_names();
        let removed_name = removed_names
            .iter()
            .find(|entry| entry.name == "external_api")
            .expect("external overlay completion name");
        assert!(removed_name.external);
        assert!(!removed_name.directly_included);
        let published_main_reach = published.reachable("main.c");
        let removed_service = CandidateQueryService::new(
            None,
            &removed,
            "main.c",
            Some(&published_main_reach),
            Some(&published),
        );
        let removed_candidates = removed_service
            .callable_candidates("external_api", None)
            .expect("removed include candidates");
        assert_eq!(removed_candidates.anchors[0].candidate.source, "external");
        assert_eq!(
            removed_candidates.anchors[0].candidate.tier,
            crate::model::ScopeTier::Global
        );
        let removed_symbol = removed_service
            .non_callable_symbols("EXTERNAL_FLAG")
            .expect("removed include symbol")
            .into_iter()
            .next()
            .expect("removed include symbol candidate");
        assert_eq!(removed_symbol.source, "external");
        assert!(!removed_symbol.directly_included);
        assert_eq!(
            removed_service
                .type_candidates("ExternalRecord")
                .expect("removed include type")
                .records
                .candidates[0]
                .tier,
            crate::model::ScopeTier::Global
        );

        let include_text = format!(
            "#include <{}>\nint local_api(void);\n",
            external.rsplit('/').next().expect("external basename")
        );
        let main_with_include = parse_with_handle(
            Path::new("main.c"),
            &include_text,
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let empty_published = ReachGraph::new(Vec::new(), Vec::new(), Vec::new());
        let mut added = CandidateOverlaySnapshot::new(
            9,
            vec![
                FileCandidateOverlay::from_index(external.clone(), &external_parsed),
                FileCandidateOverlay::from_index("main.c".into(), &main_with_include),
            ],
        );
        let external_root = external
            .rsplit_once('/')
            .expect("external parent")
            .0
            .to_string();
        added.refresh_reach_graph(Some(&empty_published), ["main.c"], &[external_root]);

        assert_eq!(
            added.direct_include_overrides().get(&external),
            Some(&true),
            "adding the dirty include must create request-local first-layer evidence"
        );
        let added_name = added
            .completion_names()
            .into_iter()
            .find(|entry| entry.name == "external_api")
            .expect("external overlay completion name");
        assert!(added_name.external);
        assert!(added_name.directly_included);
        let added_candidates =
            CandidateQueryService::new(None, &added, "main.c", None, Some(&empty_published))
                .callable_candidates("external_api", None)
                .expect("added include candidates");
        assert_eq!(added_candidates.anchors[0].candidate.source, "external");
        assert_eq!(
            added_candidates.anchors[0].candidate.tier,
            crate::model::ScopeTier::External
        );
        let unrelated_candidates =
            CandidateQueryService::new(None, &added, "other.c", None, Some(&empty_published))
                .callable_candidates("external_api", None)
                .expect("unrelated-origin external candidates");
        assert_eq!(
            unrelated_candidates.anchors[0].candidate.tier,
            crate::model::ScopeTier::Global,
            "another workspace source must not inherit main.c's direct external evidence"
        );
        let added_service =
            CandidateQueryService::new(None, &added, "main.c", None, Some(&empty_published));
        let added_symbol = added_service
            .non_callable_symbols("EXTERNAL_FLAG")
            .expect("added include symbol")
            .into_iter()
            .next()
            .expect("added include symbol candidate");
        assert_eq!(added_symbol.source, "external");
        assert!(added_symbol.directly_included);
        assert_eq!(
            added_service
                .type_candidates("ExternalRecord")
                .expect("added include type")
                .records
                .candidates[0]
                .tier,
            crate::model::ScopeTier::External
        );

        let local_candidates =
            CandidateQueryService::new(None, &added, "main.c", None, Some(&empty_published))
                .callable_candidates("local_api", None)
                .expect("workspace overlay candidates");
        assert_eq!(local_candidates.anchors[0].candidate.source, "workspace");
        assert_eq!(
            local_candidates.anchors[0].candidate.tier,
            crate::model::ScopeTier::Current
        );
    }

    #[test]
    fn name_table_sparse_direct_include_override_changes_external_tier_only() {
        let external = absolute_test_path("completion_external.h");
        let table = crate::query::NameTable::build_with_paths(vec![
            (
                1,
                "external_name".into(),
                true,
                external.clone(),
                "function".into(),
                true,
            ),
            (
                2,
                "workspace_name".into(),
                false,
                "other.h".into(),
                "function".into(),
                true,
            ),
        ]);
        let overrides = HashMap::from([(external.clone(), false)]);
        let effective = table.with_direct_include_overrides(&overrides);
        assert_eq!(
            effective.exact_name_hits_scoped("external_name", 1, None)[0].tier,
            crate::model::ScopeTier::Global
        );
        assert_eq!(
            effective.exact_name_hits_scoped("workspace_name", 1, None)[0].tier,
            crate::model::ScopeTier::Global,
            "a workspace path must not become External even if a malformed durable bit is set"
        );

        let added = table.with_direct_include_overrides(&HashMap::from([(external, true)]));
        assert_eq!(
            added.exact_name_hits_scoped("external_name", 1, None)[0].tier,
            crate::model::ScopeTier::External
        );
    }

    #[test]
    fn facade_resolves_dirty_typedef_to_its_record_without_durable_rows() {
        let parsed = parse_with_handle(
            Path::new("packet.h"),
            "typedef struct Packet { int id; } PacketT;\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let snapshot = CandidateOverlaySnapshot::new(
            6,
            vec![FileCandidateOverlay::from_index("packet.h".into(), &parsed)],
        );
        let service = CandidateQueryService::new(None, &snapshot, "main.c", None, None);
        let candidates = service.type_candidates("PacketT").expect("type candidates");
        assert_eq!(candidates.alias_resolutions.len(), 1);
        assert_eq!(
            candidates.alias_resolutions[0].status,
            crate::query::AliasResolutionStatus::UniqueRecord
        );
        assert_eq!(candidates.alias_resolutions[0].terminal_records.len(), 1);
    }

    #[test]
    fn facade_reads_members_from_the_same_dirty_typedef_record_snapshot() {
        let parsed = parse_with_handle(
            Path::new("packet.h"),
            "typedef struct Packet { int live_field; void refresh(void); } PacketT;\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let snapshot = CandidateOverlaySnapshot::new(
            7,
            vec![FileCandidateOverlay::from_index("packet.h".into(), &parsed)],
        );
        let service = CandidateQueryService::new(None, &snapshot, "main.c", None, None);
        let records = service
            .records_for_type_name_with_evidence("PacketT")
            .expect("receiver records")
            .records;
        let members = service
            .members_for_records_limited(&records, None, usize::MAX)
            .expect("overlay members");

        assert!(members
            .candidates
            .iter()
            .any(|member| member.name == "live_field"));
        assert!(members
            .candidates
            .iter()
            .any(|member| member.name == "refresh"));
        assert!(members
            .candidates
            .iter()
            .all(|member| member.owner_path == "packet.h"));
    }

    #[test]
    fn resolved_overlay_member_read_stops_at_the_shared_scan_budget() {
        let parsed = parse_with_handle(
            Path::new("bounded.h"),
            "struct Bounded { int alpha; int beta; int gamma; };\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let snapshot = CandidateOverlaySnapshot::new(
            8,
            vec![FileCandidateOverlay::from_index(
                "bounded.h".into(),
                &parsed,
            )],
        );
        let service = CandidateQueryService::new(None, &snapshot, "main.c", None, None);
        let records = service
            .records_for_type_name_with_evidence("Bounded")
            .expect("receiver records")
            .records;

        let read = service
            .members_for_records_limited(&records, None, 2)
            .expect("bounded overlay members");

        assert_eq!(read.scanned, 2);
        assert_eq!(read.candidates.len(), 2);
        assert!(read.truncated);
    }

    #[test]
    fn scoped_exact_name_recall_survives_global_cap_and_dirty_tombstones() {
        let dir = tempdir().expect("tempdir");
        let db = dir.path().join("index.sqlite");
        let mut store = IndexStore::open(&db, dir.path()).expect("store");
        let mut noise = String::new();
        for _ in 0..300 {
            noise.push_str("int crowded(void);\n");
            noise.push_str("extern int crowded_value;\n");
        }
        upsert_candidate_test_file(&mut store, "aaa/noise.h", &noise);
        upsert_candidate_test_file(
            &mut store,
            "zzz/reachable.h",
            "int crowded(void);\nint crowded_value = 1;\n",
        );
        drop(store);

        let graph = ReachGraph::new(
            vec![("main.c".into(), "zzz/reachable.h".into())],
            Vec::new(),
            Vec::new(),
        );
        let reach = graph.reachable("main.c");
        let handle = CallReadHandle::capture(db).expect("read handle");
        let clean = CandidateOverlaySnapshot::default();
        let service = CandidateQueryService::new(
            Some(&handle),
            &clean,
            "main.c",
            Some(reach.as_ref()),
            Some(&graph),
        );

        let callables = service
            .callable_candidates("crowded", None)
            .expect("callable candidates");
        assert!(callables.coverage.truncated);
        assert!(callables.anchors.len() <= DEFAULT_EXACT_NAME_CANDIDATE_LIMIT);
        assert!(callables
            .anchors
            .iter()
            .any(|candidate| candidate.anchor.path == "zzz/reachable.h"));

        let symbols = service
            .non_callable_symbols("crowded_value")
            .expect("non-callable candidates");
        assert!(symbols.len() <= DEFAULT_EXACT_NAME_CANDIDATE_LIMIT);
        assert!(symbols
            .iter()
            .any(|candidate| candidate.path == "zzz/reachable.h"));

        let current_service =
            CandidateQueryService::new(Some(&handle), &clean, "zzz/reachable.h", None, None);
        assert!(current_service
            .callable_candidates("crowded", None)
            .expect("current-file callable candidates")
            .anchors
            .iter()
            .any(|candidate| candidate.anchor.path == "zzz/reachable.h"));
        assert!(current_service
            .non_callable_symbols("crowded_value")
            .expect("current-file non-callable candidates")
            .iter()
            .any(|candidate| candidate.path == "zzz/reachable.h"));

        let dirty = parse_with_handle(
            Path::new("zzz/reachable.h"),
            "int replacement_value = 2;\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let tombstone = CandidateOverlaySnapshot::new(
            9,
            vec![FileCandidateOverlay::from_index(
                "zzz/reachable.h".into(),
                &dirty,
            )],
        );
        let dirty_service = CandidateQueryService::new(
            Some(&handle),
            &tombstone,
            "main.c",
            Some(reach.as_ref()),
            Some(&graph),
        );
        assert!(dirty_service
            .callable_candidates("crowded", None)
            .expect("dirty callable candidates")
            .anchors
            .iter()
            .all(|candidate| candidate.anchor.path != "zzz/reachable.h"));
        assert!(dirty_service
            .non_callable_symbols("crowded_value")
            .expect("dirty non-callable candidates")
            .iter()
            .all(|candidate| candidate.path != "zzz/reachable.h"));
    }
}
