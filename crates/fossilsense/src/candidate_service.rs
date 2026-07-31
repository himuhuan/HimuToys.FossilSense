//! Generation-pinned semantic candidate recall and dirty-document overlays.
//!
//! It contains canonical parser facts for only divergent open documents,
//! shadows the matching durable path even when the new document no longer
//! contains a fact, and supports exact-name lookup without scanning all open
//! buffers on every request. Completion-only fallback hints remain a separate
//! typed projection and are never read by semantic candidate APIs.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

use crate::call_catalog::rows::{anchor_from_row, call_from_row};
use crate::call_model::{CallSiteFact, CallableAnchor, LinkageDomain, SourcePosition, SourceRange};
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
use crate::semantic_model::{
    DeclarationFact, FallbackCompletionFact, ImportFact, Include, MemberDef, PackageFact,
    RecordDef, SemanticFamily, TypeAlias,
};

mod callable_queries;
mod semantic;
mod type_queries;
pub use callable_queries::CandidateQueryService;
#[allow(unused_imports)]
pub use semantic::CandidateHandleLocator;
pub use semantic::{
    focused_callable_fingerprints, focused_candidates, focused_has_kind, navigation_presentations,
    CandidateHandle, ResolvedDeclarationCandidate, SemanticIntent,
};
#[allow(unused_imports)]
pub use type_queries::{BoundedMemberCandidates, TypeCandidateBundle, TypeRecordResolution};

pub const DEFAULT_EXACT_NAME_CANDIDATE_LIMIT: usize = 256;
const MEMBER_FALLBACK_OVERLAY_SCAN_LIMIT: usize = 8_192;

#[derive(Debug, Clone)]
pub struct FileCandidateOverlay {
    pub path: String,
    pub semantic_family: SemanticFamily,
    pub package: Option<PackageFact>,
    pub imports: Vec<ImportFact>,
    pub declarations: Vec<DeclarationFact>,
    pub anchors: Vec<CallableAnchor>,
    pub calls: Vec<CallSiteFact>,
    pub records: Vec<RecordDef>,
    pub members: Vec<MemberDef>,
    pub aliases: Vec<TypeAlias>,
    pub includes: Vec<Include>,
    pub fallback_completions: Vec<FallbackCompletionFact>,
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
            semantic_family: crate::config::SourceLanguage::default_for_path(Path::new(&path))
                .semantic_family(),
            path,
            package: None,
            imports: Vec::new(),
            declarations: Vec::new(),
            anchors,
            calls,
            records: Vec::new(),
            members: Vec::new(),
            aliases: Vec::new(),
            includes: Vec::new(),
            fallback_completions: Vec::new(),
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
        overlay.semantic_family = index.language.semantic_family();
        overlay.package.clone_from(&index.package);
        overlay.imports.clone_from(&index.imports);
        overlay.declarations = index
            .declarations
            .iter()
            .cloned()
            .map(|mut declaration| {
                declaration.path.clone_from(&overlay.path);
                declaration.identity.locator.path.clone_from(&overlay.path);
                if matches!(declaration.linkage, LinkageDomain::Internal(_)) {
                    declaration.linkage = LinkageDomain::Internal(overlay.path.clone());
                    declaration.identity.logical_key.linkage_domain =
                        format!("internal:{}", overlay.path);
                }
                declaration
            })
            .collect();
        overlay.records.clone_from(&index.records);
        overlay.members.clone_from(&index.members);
        overlay.aliases.clone_from(&index.aliases);
        overlay.includes.clone_from(&index.includes);
        overlay
            .fallback_completions
            .clone_from(&index.fallback_completions);
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

    pub fn tombstone_for_family(
        path: String,
        text: Arc<str>,
        semantic_family: SemanticFamily,
    ) -> Self {
        let mut overlay = Self::tombstone(path, text);
        overlay.semantic_family = semantic_family;
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
pub struct OverlayDeclarationFact {
    pub path: String,
    pub fact: DeclarationFact,
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
    pub semantic_family: SemanticFamily,
    pub external: bool,
    pub directly_included: bool,
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
    pub candidate_handle: Option<CandidateHandle>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayFallbackCompletionFact {
    pub path: String,
    pub fact: FallbackCompletionFact,
}

#[derive(Debug, Clone, Default)]
pub struct CandidateOverlaySnapshot {
    #[allow(dead_code)] // Captured for request tracing and cross-snapshot diagnostics.
    pub epoch: u64,
    shadowed_paths: HashSet<String>,
    semantic_family_by_path: HashMap<String, SemanticFamily>,
    go_overlay_packages: HashMap<String, Option<(String, crate::reachability::OpenReason)>>,
    callable_by_name: HashMap<String, Vec<CallableAnchor>>,
    callable_by_path: HashMap<String, Vec<CallableAnchor>>,
    declaration_by_name: HashMap<String, Vec<OverlayDeclarationFact>>,
    declaration_by_fingerprint: HashMap<String, OverlayDeclarationFact>,
    record_by_name: HashMap<String, Vec<OverlayRecordFact>>,
    record_by_key: HashMap<(String, String), OverlayRecordFact>,
    records_by_path: HashMap<String, Vec<OverlayRecordFact>>,
    members_by_record_key: HashMap<(String, String), Vec<MemberDef>>,
    member_prefix_index: Vec<OverlayMemberFact>,
    alias_by_name: HashMap<String, Vec<OverlayAliasFact>>,
    call_sites_by_path: HashMap<String, Vec<CallSiteFact>>,
    source_by_path: HashMap<String, Arc<str>>,
    includes_by_path: HashMap<String, Vec<Include>>,
    fallback_completions: Vec<OverlayFallbackCompletionFact>,
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
            snapshot
                .semantic_family_by_path
                .insert(file.path.clone(), file.semantic_family);
            if file.semantic_family == SemanticFamily::Go {
                let reason = if file.imports.iter().any(|import| import.path == "C") {
                    crate::reachability::OpenReason::UnsupportedLanguageBoundary
                } else {
                    crate::reachability::OpenReason::UnresolvedInclude
                };
                snapshot.go_overlay_packages.insert(
                    file.path.clone(),
                    file.package
                        .as_ref()
                        .map(|package| (physical_package_key(&file.path, &package.name), reason)),
                );
            }
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
                .fallback_completions
                .extend(file.fallback_completions.into_iter().map(|fact| {
                    OverlayFallbackCompletionFact {
                        path: file.path.clone(),
                        fact,
                    }
                }));
            snapshot
                .callable_by_path
                .insert(file.path.clone(), file.anchors.clone());
            for declaration in file.declarations {
                let entry = OverlayDeclarationFact {
                    path: file.path.clone(),
                    fact: declaration,
                };
                snapshot.declaration_by_fingerprint.insert(
                    entry.fact.identity.locator.fingerprint.clone(),
                    entry.clone(),
                );
                snapshot
                    .declaration_by_name
                    .entry(entry.fact.name.clone())
                    .or_default()
                    .push(entry);
            }
            for anchor in file.anchors {
                snapshot
                    .callable_by_name
                    .entry(anchor.name.clone())
                    .or_default()
                    .push(anchor);
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
        for declarations in snapshot.declaration_by_name.values_mut() {
            declarations.sort_by(|left, right| {
                left.path
                    .cmp(&right.path)
                    .then_with(|| {
                        left.fact
                            .name_range
                            .start_byte
                            .cmp(&right.fact.name_range.start_byte)
                    })
                    .then_with(|| {
                        left.fact
                            .identity
                            .locator
                            .fingerprint
                            .cmp(&right.fact.identity.locator.fingerprint)
                    })
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
        let mut graph = match base {
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
        if !self.go_overlay_packages.is_empty() {
            graph = graph.with_refreshed_go_overlays(
                self.go_overlay_packages
                    .iter()
                    .map(|(path, package)| (path.clone(), package.clone()))
                    .collect(),
            );
        }
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
                semantic_family: self
                    .semantic_family_for_path(&path)
                    .unwrap_or(SemanticFamily::CFamily),
                package: None,
                imports: Vec::new(),
                anchors: self
                    .callable_by_path
                    .get(&path)
                    .cloned()
                    .unwrap_or_default(),
                declarations: Vec::new(),
                calls: self
                    .call_sites_by_path
                    .get(&path)
                    .cloned()
                    .unwrap_or_default(),
                records: Vec::new(),
                members: Vec::new(),
                aliases: Vec::new(),
                includes: Vec::new(),
                fallback_completions: Vec::new(),
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

    pub fn semantic_family_for_path(&self, path: &str) -> Option<SemanticFamily> {
        self.semantic_family_by_path.get(path).copied()
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

    pub fn callable_anchors_for_family(
        &self,
        name: &str,
        family: SemanticFamily,
    ) -> Vec<&CallableAnchor> {
        self.callable_anchors(name)
            .iter()
            .filter(|anchor| self.semantic_family_for_path(&anchor.path) == Some(family))
            .collect()
    }

    pub fn declarations(&self, name: &str) -> &[OverlayDeclarationFact] {
        self.declaration_by_name
            .get(name)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn declarations_for_family(
        &self,
        name: &str,
        family: SemanticFamily,
    ) -> Vec<&OverlayDeclarationFact> {
        self.declarations(name)
            .iter()
            .filter(|entry| entry.fact.identity.language.semantic_family() == family)
            .collect()
    }

    pub fn declaration_by_fingerprint(&self, fingerprint: &str) -> Option<&OverlayDeclarationFact> {
        self.declaration_by_fingerprint.get(fingerprint)
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

    pub fn records_for_family(
        &self,
        name: &str,
        family: SemanticFamily,
    ) -> Vec<&OverlayRecordFact> {
        self.records(name)
            .iter()
            .filter(|entry| self.semantic_family_for_path(&entry.path) == Some(family))
            .collect()
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

    #[cfg(test)]
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

    fn fallback_members_by_prefix_for_family_limited(
        &self,
        prefix: &str,
        family: SemanticFamily,
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
            if self.semantic_family_for_path(&fact.path) != Some(family) {
                continue;
            }
            if matches.len() >= limit {
                truncated = true;
                break;
            }
            matches.push(fact);
        }
        (matches, truncated)
    }

    /// Stable projection used to replace shadowed NameTable paths in ordinary
    /// completion. Do not truncate before NameTable applies the request's
    /// actual matcher: every dirty path is tombstoned, so dropping a later
    /// overlay symbol here could erase its only current representation.
    /// Negative ids are request-local locators and can never be mistaken for
    /// durable SQLite symbol ids.
    pub fn completion_names(&self) -> Vec<OverlayCompletionName> {
        let mut facts: Vec<_> = self.declaration_by_name.values().flatten().collect();
        facts.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| {
                    left.fact
                        .name_range
                        .start_byte
                        .cmp(&right.fact.name_range.start_byte)
                })
                .then_with(|| left.fact.name.cmp(&right.fact.name))
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
                    name: fact.fact.name.clone(),
                    path: fact.path.clone(),
                    kind: declaration_kind_name(fact.fact.declaration_kind).to_string(),
                    semantic_family: fact.fact.identity.language.semantic_family(),
                    external,
                    directly_included,
                    start_line: fact.fact.name_range.start.line,
                    start_col: fact.fact.name_range.start.character,
                    end_line: fact.fact.name_range.end.line,
                    end_col: fact.fact.name_range.end.character,
                    candidate_handle: Some(CandidateHandle {
                        locator: CandidateHandleLocator::Overlay {
                            fingerprint: fact.fact.identity.locator.fingerprint.clone(),
                        },
                        logical_key: fact.fact.identity.logical_key.clone(),
                        locator_fingerprint: fact.fact.identity.locator.fingerprint.clone(),
                        semantic_family: fact.fact.identity.language.semantic_family(),
                    }),
                }
            })
            .collect()
    }

    /// Completion-only degraded hints from dirty documents. Semantic candidate
    /// APIs deliberately do not consult this projection.
    pub fn fallback_completion_facts(&self) -> &[OverlayFallbackCompletionFact] {
        &self.fallback_completions
    }

    pub fn aliases(&self, name: &str) -> &[OverlayAliasFact] {
        self.alias_by_name
            .get(name)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn aliases_for_family(&self, name: &str, family: SemanticFamily) -> Vec<&OverlayAliasFact> {
        self.aliases(name)
            .iter()
            .filter(|entry| self.semantic_family_for_path(&entry.path) == Some(family))
            .collect()
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

fn declaration_kind_name(kind: crate::semantic_model::SemanticDeclarationKind) -> &'static str {
    match kind {
        crate::semantic_model::SemanticDeclarationKind::Function => "function",
        crate::semantic_model::SemanticDeclarationKind::Method => "method",
        crate::semantic_model::SemanticDeclarationKind::Macro => "macro",
        crate::semantic_model::SemanticDeclarationKind::Type => "type",
        crate::semantic_model::SemanticDeclarationKind::Alias => "type",
        crate::semantic_model::SemanticDeclarationKind::EnumConstant => "enum_constant",
        crate::semantic_model::SemanticDeclarationKind::Object => "global_variable",
    }
}

fn position_in_range(position: SourcePosition, range: SourceRange) -> bool {
    (position.line, position.character) >= (range.start.line, range.start.character)
        && (position.line, position.character) <= (range.end.line, range.end.character)
}

fn physical_package_key(path: &str, package_name: &str) -> String {
    let directory = path
        .rsplit_once('/')
        .map(|(directory, _)| directory)
        .filter(|directory| !directory.is_empty())
        .unwrap_or(".");
    format!("{directory}#{package_name}")
}

fn candidate_origin_priority(origin: CandidateOrigin) -> u8 {
    match origin {
        CandidateOrigin::Base => 0,
        CandidateOrigin::Overlay => 1,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tempfile::tempdir;

    use super::*;
    use crate::parser::{parse_with_handle, ParseFacts};
    use crate::store::views::{GoPackageEdgeRow, GoPackageFileRow, GoPackageResolution};
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
        parsed.parse_outcome = crate::semantic_model::ParseOutcome::LexicalFallback;
        parsed.declarations.clear();
        parsed.fallback_completions = vec![FallbackCompletionFact {
            name: "api".to_string(),
            kind_hint: crate::semantic_model::CompletionKindHint::Function,
            range: SourceRange {
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
            detail: Some("api(int value)".to_string()),
        }];
        parsed.callable_anchors.clear();
        parsed.call_sites.clear();
        parsed.records.clear();
        parsed.aliases.clear();
        let overlay = FileCandidateOverlay::from_index("fallback.h".into(), &parsed);
        assert!(!overlay.facts_complete);
        let snapshot = CandidateOverlaySnapshot::new(1, vec![overlay]);
        assert!(snapshot.has_incomplete_facts());
        assert_eq!(snapshot.fallback_completion_facts().len(), 1);
        let candidates = CandidateQueryService::new(None, &snapshot, "fallback.h", None, None)
            .callable_candidates("api", None)
            .expect("lexical callable fallback");
        assert!(
            candidates.anchors.is_empty(),
            "lexical fallback must never enter semantic callable candidates"
        );
        assert!(
            CandidateQueryService::new(None, &snapshot, "fallback.h", None, None)
                .semantic_candidates("api", SemanticIntent::Neutral)
                .expect("semantic candidates")
                .all
                .is_empty()
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
    fn dirty_go_file_drops_stale_published_import_reach_and_opens_its_package() {
        let published = ReachGraph::from_rows_with_packages(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![
                GoPackageFileRow {
                    package_key: "app#main".into(),
                    path: "app/main.go".into(),
                },
                GoPackageFileRow {
                    package_key: "app#main".into(),
                    path: "app/helper.go".into(),
                },
                GoPackageFileRow {
                    package_key: "lib#lib".into(),
                    path: "lib/lib.go".into(),
                },
            ],
            vec![GoPackageEdgeRow {
                source_package_key: "app#main".into(),
                target_package_key: "lib#lib".into(),
                resolution: GoPackageResolution::Exact,
            }],
            Vec::new(),
        );
        assert!(published
            .reachable("app/main.go")
            .files
            .contains("lib/lib.go"));

        let dirty = parse_with_handle(
            Path::new("app/main.go"),
            "package main\n\nfunc main() {}\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let mut snapshot = CandidateOverlaySnapshot::new(
            8,
            vec![FileCandidateOverlay::from_index(
                "app/main.go".into(),
                &dirty,
            )],
        );
        snapshot.refresh_reach_graph(
            Some(&published),
            ["app/main.go", "app/helper.go", "lib/lib.go"],
            &[],
        );

        let scope = snapshot
            .effective_reach_graph(Some(&published))
            .expect("overlay graph")
            .reachable("app/main.go");
        assert!(scope.files.contains("app/main.go"));
        assert!(scope.files.contains("app/helper.go"));
        assert!(!scope.files.contains("lib/lib.go"));
        assert!(scope.open);
        assert_eq!(
            scope.reason,
            Some(crate::reachability::OpenReason::UnresolvedInclude)
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
            .semantic_candidates("EXTERNAL_FLAG", SemanticIntent::Value)
            .expect("removed include declaration")
            .all
            .into_iter()
            .flat_map(|group| group.candidates)
            .next()
            .expect("removed include declaration candidate");
        assert!(removed_symbol.external);
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
            .semantic_candidates("EXTERNAL_FLAG", SemanticIntent::Value)
            .expect("added include declaration")
            .all
            .into_iter()
            .flat_map(|group| group.candidates)
            .next()
            .expect("added include declaration candidate");
        assert!(added_symbol.external);
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

        let symbols: Vec<_> = service
            .semantic_candidates("crowded_value", SemanticIntent::Value)
            .expect("value declaration candidates")
            .all
            .into_iter()
            .flat_map(|group| group.candidates)
            .collect();
        assert!(symbols.len() <= DEFAULT_EXACT_NAME_CANDIDATE_LIMIT);
        assert!(symbols
            .iter()
            .any(|candidate| candidate.fact.path == "zzz/reachable.h"));

        let semantic_callables = service
            .semantic_candidates("crowded", SemanticIntent::Call)
            .expect("semantic callable candidates");
        assert!(semantic_callables.coverage.truncated);
        assert!(semantic_callables
            .all
            .iter()
            .flat_map(|group| &group.candidates)
            .any(|candidate| candidate.fact.path == "zzz/reachable.h"));

        let semantic_values = service
            .semantic_candidates("crowded_value", SemanticIntent::Value)
            .expect("semantic value candidates");
        assert!(semantic_values.coverage.truncated);
        assert!(semantic_values
            .all
            .iter()
            .flat_map(|group| &group.candidates)
            .any(|candidate| candidate.fact.path == "zzz/reachable.h"));

        let current_service =
            CandidateQueryService::new(Some(&handle), &clean, "zzz/reachable.h", None, None);
        assert!(current_service
            .callable_candidates("crowded", None)
            .expect("current-file callable candidates")
            .anchors
            .iter()
            .any(|candidate| candidate.anchor.path == "zzz/reachable.h"));
        assert!(current_service
            .semantic_candidates("crowded_value", SemanticIntent::Value)
            .expect("current-file value candidates")
            .all
            .iter()
            .flat_map(|group| &group.candidates)
            .any(|candidate| candidate.fact.path == "zzz/reachable.h"));

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
            .semantic_candidates("crowded_value", SemanticIntent::Value)
            .expect("dirty value candidates")
            .all
            .iter()
            .flat_map(|group| &group.candidates)
            .all(|candidate| candidate.fact.path != "zzz/reachable.h"));
    }

    #[test]
    fn declaration_index_batches_cold_payloads_once_and_warm_query_reads_zero_sql() {
        let dir = tempdir().expect("tempdir");
        let db = dir.path().join("index.sqlite");
        let mut store = IndexStore::open(&db, dir.path()).expect("store");
        upsert_candidate_test_file(&mut store, "api.h", "int shared_api(int value);\n");
        upsert_candidate_test_file(
            &mut store,
            "api.c",
            "int shared_api(int value) { return value; }\n",
        );
        drop(store);

        let reader = IndexStore::open_readonly(&db).expect("readonly");
        let names =
            crate::query::NameTable::build_from_declaration_view(&reader.declaration_view(), None)
                .expect("declaration names");
        drop(reader);
        let index = crate::declaration_index::SemanticDeclarationIndex::build(names, 1024 * 1024);
        let handle = CallReadHandle::capture(db).expect("read handle");
        let overlay = CandidateOverlaySnapshot::default();
        let service = CandidateQueryService::new_with_declarations(
            Some(&handle),
            Some(&index),
            &overlay,
            "main.c",
            None,
            None,
        );

        let cold = service
            .semantic_candidates("shared_api", SemanticIntent::Neutral)
            .expect("cold semantic candidates");
        let recall = index
            .name_table()
            .exact_name_hits_scoped("shared_api", 10, None);
        let mut recall_ids: Vec<_> = recall.iter().map(|hit| hit.id).collect();
        let semantic_candidates: Vec<_> = cold
            .all
            .iter()
            .flat_map(|group| group.candidates.iter())
            .collect();
        let mut semantic_ids: Vec<_> = semantic_candidates
            .iter()
            .filter_map(|candidate| candidate.persistent_id)
            .collect();
        recall_ids.sort_unstable();
        semantic_ids.sort_unstable();
        assert_eq!(
            recall_ids, semantic_ids,
            "completion recall IDs must hydrate the exact candidate set shared by Hover/navigation"
        );
        for hit in &recall {
            let candidate = semantic_candidates
                .iter()
                .find(|candidate| candidate.persistent_id == Some(hit.id))
                .expect("every completion recall ID must hydrate");
            assert_eq!(candidate.fact.name, hit.name);
            assert_eq!(
                candidate.fact.role,
                match hit.role {
                    crate::parser::SymbolRole::Declaration => {
                        crate::semantic_model::SemanticDeclarationRole::Declaration
                    }
                    crate::parser::SymbolRole::Definition => {
                        crate::semantic_model::SemanticDeclarationRole::Definition
                    }
                    crate::parser::SymbolRole::TentativeDefinition => {
                        crate::semantic_model::SemanticDeclarationRole::TentativeDefinition
                    }
                    crate::parser::SymbolRole::UnknownDeclarationOrDefinition => {
                        crate::semantic_model::SemanticDeclarationRole::Unknown
                    }
                },
                "recall presentation evidence must be projected from the same persisted declaration"
            );
        }
        assert_eq!(semantic_candidates.len(), 2);
        let after_cold = index.payload_cache_stats();
        assert_eq!(after_cold.sql_reads, 1);

        let warm = service
            .semantic_candidates("shared_api", SemanticIntent::Neutral)
            .expect("warm semantic candidates");
        assert_eq!(
            warm.all
                .iter()
                .map(|group| group.candidates.len())
                .sum::<usize>(),
            2
        );
        let after_warm = index.payload_cache_stats();
        assert_eq!(after_warm.sql_reads, 1, "warm query must read zero SQL");
        assert!(after_warm.hits >= 2);
    }

    #[test]
    fn navigation_preserves_same_role_physical_alternatives() {
        let first = parse_with_handle(
            Path::new("first.c"),
            "int shared_value = 1;\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let second = parse_with_handle(
            Path::new("second.c"),
            "int shared_value = 2;\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let snapshot = CandidateOverlaySnapshot::new(
            1,
            vec![
                FileCandidateOverlay::from_index("first.c".into(), &first),
                FileCandidateOverlay::from_index("second.c".into(), &second),
            ],
        );
        let semantic = CandidateQueryService::new(None, &snapshot, "main.c", None, None)
            .semantic_candidates("shared_value", SemanticIntent::Value)
            .expect("semantic object candidates");

        let presentations = navigation_presentations(&semantic, false, "main.c");
        assert_eq!(
            presentations
                .iter()
                .map(|candidate| candidate.path.as_str())
                .collect::<Vec<_>>(),
            vec!["first.c", "second.c"]
        );
    }

    #[test]
    fn go_and_c_candidate_families_do_not_cross_overlay_or_stale_handle_boundaries() {
        let c = parse_with_handle(
            Path::new("shared.c"),
            "int SharedOpen(void) { return 1; }\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let go = parse_with_handle(
            Path::new("shared.go"),
            "package shared\nfunc SharedOpen() int { return 1 }\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let snapshot = CandidateOverlaySnapshot::new(
            1,
            vec![
                FileCandidateOverlay::from_index("shared.c".into(), &c),
                FileCandidateOverlay::from_index("shared.go".into(), &go),
            ],
        );

        let go_service = CandidateQueryService::new(None, &snapshot, "main.go", None, None);
        let go_candidates = go_service
            .semantic_candidates("SharedOpen", SemanticIntent::Call)
            .expect("Go candidates");
        let go_candidate = go_candidates
            .all
            .iter()
            .flat_map(|group| &group.candidates)
            .next()
            .expect("Go candidate");
        assert_eq!(
            go_candidates
                .all
                .iter()
                .flat_map(|group| &group.candidates)
                .map(|candidate| candidate.fact.identity.language)
                .collect::<Vec<_>>(),
            vec![crate::semantic_model::SemanticLanguage::Go]
        );
        let stale_go_handle = CandidateHandle {
            locator: CandidateHandleLocator::Overlay {
                fingerprint: go_candidate.fact.identity.locator.fingerprint.clone(),
            },
            logical_key: go_candidate.fact.identity.logical_key.clone(),
            locator_fingerprint: go_candidate.fact.identity.locator.fingerprint.clone(),
            semantic_family: crate::config::SemanticFamily::Go,
        };

        let c_service = CandidateQueryService::new(None, &snapshot, "main.c", None, None);
        assert!(c_service
            .resolve_candidate_handle(&stale_go_handle)
            .expect("stale handle resolution")
            .is_none());
        let callables = go_service
            .callable_candidates("SharedOpen", None)
            .expect("Go callables");
        assert!(callables
            .anchors
            .iter()
            .all(|anchor| anchor.anchor.path.ends_with(".go")));
    }

    #[test]
    fn unguarded_go_variant_ranks_before_guarded_variant_without_filtering_either() {
        let guarded = parse_with_handle(
            Path::new("pkg/a_guarded.go"),
            "//go:build tinygo\n\npackage pkg\nfunc Open() {}\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let unguarded = parse_with_handle(
            Path::new("pkg/z_unguarded.go"),
            "package pkg\nfunc Open() {}\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let snapshot = CandidateOverlaySnapshot::new(
            1,
            vec![
                FileCandidateOverlay::from_index("pkg/a_guarded.go".into(), &guarded),
                FileCandidateOverlay::from_index("pkg/z_unguarded.go".into(), &unguarded),
            ],
        );
        let graph = ReachGraph::from_rows_with_packages(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![
                GoPackageFileRow {
                    package_key: "pkg#pkg".into(),
                    path: "pkg/use.go".into(),
                },
                GoPackageFileRow {
                    package_key: "pkg#pkg".into(),
                    path: "pkg/a_guarded.go".into(),
                },
                GoPackageFileRow {
                    package_key: "pkg#pkg".into(),
                    path: "pkg/z_unguarded.go".into(),
                },
            ],
            Vec::new(),
            Vec::new(),
        );
        let reach = graph.reachable("pkg/use.go");
        let semantic = CandidateQueryService::new(
            None,
            &snapshot,
            "pkg/use.go",
            Some(reach.as_ref()),
            Some(&graph),
        )
        .semantic_candidates("Open", SemanticIntent::Call)
        .expect("Go variants");
        let candidates: Vec<_> = semantic
            .all
            .iter()
            .flat_map(|group| &group.candidates)
            .collect();

        assert_eq!(candidates.len(), 2, "guards are evidence, not filters");
        assert_eq!(candidates[0].fact.path, "pkg/z_unguarded.go");
        assert!(candidates[0].fact.guard.is_none());
        assert_eq!(candidates[1].fact.guard.as_deref(), Some("tinygo"));
    }

    #[test]
    fn go_and_c_candidate_families_do_not_cross_durable_reads() {
        let dir = tempdir().expect("tempdir");
        let db = dir.path().join("index.sqlite");
        let mut store = IndexStore::open(&db, dir.path()).expect("store");
        upsert_candidate_test_file(
            &mut store,
            "src/shared.c",
            "int SharedOpen(void) { return 1; }\n",
        );
        upsert_candidate_test_file(
            &mut store,
            "src/shared.go",
            "package shared\nfunc SharedOpen() int { return 1 }\n",
        );
        drop(store);

        let handle = CallReadHandle::capture(db).expect("read handle");
        let snapshot = CandidateOverlaySnapshot::default();
        let service = CandidateQueryService::new(Some(&handle), &snapshot, "main.go", None, None);

        let semantic = service
            .semantic_candidates("SharedOpen", SemanticIntent::Call)
            .expect("Go declarations");
        assert_eq!(
            semantic
                .all
                .iter()
                .flat_map(|group| &group.candidates)
                .map(|candidate| candidate.fact.identity.language)
                .collect::<Vec<_>>(),
            vec![crate::semantic_model::SemanticLanguage::Go]
        );
        let callables = service
            .callable_candidates("SharedOpen", None)
            .expect("Go callables");
        assert!(!callables.anchors.is_empty());
        assert!(callables
            .anchors
            .iter()
            .all(|candidate| candidate.anchor.path.ends_with(".go")));
    }

    #[test]
    fn go_and_c_record_and_member_facts_do_not_cross_durable_reads() {
        let dir = tempdir().expect("tempdir");
        let db = dir.path().join("index.sqlite");
        let mut store = IndexStore::open(&db, dir.path()).expect("store");
        upsert_candidate_test_file(
            &mut store,
            "src/shared.c",
            "struct Shared { int c_field; };\n",
        );
        upsert_candidate_test_file(
            &mut store,
            "src/shared.go",
            "package shared\ntype Shared struct { GoField int }\n",
        );
        drop(store);

        let handle = CallReadHandle::capture(db).expect("read handle");
        let snapshot = CandidateOverlaySnapshot::default();
        let service = CandidateQueryService::new(Some(&handle), &snapshot, "main.go", None, None);
        let records = service
            .records_for_type_name_with_evidence("Shared")
            .expect("Go records")
            .records;
        assert!(!records.is_empty());
        assert!(records.iter().all(|record| record.path.ends_with(".go")));
        let members = service
            .members_for_records_limited(&records, None, 16)
            .expect("Go members");
        assert!(members
            .candidates
            .iter()
            .any(|member| member.name == "GoField"));
        assert!(members
            .candidates
            .iter()
            .all(|member| member.name != "c_field"));
    }

    #[test]
    fn navigation_prefers_function_definitions_in_source_files() {
        let header = parse_with_handle(
            Path::new("api.h"),
            "int shared_api(void) { return 1; }\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let source = parse_with_handle(
            Path::new("api.c"),
            "int shared_api(void) { return 2; }\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let snapshot = CandidateOverlaySnapshot::new(
            1,
            vec![
                FileCandidateOverlay::from_index("api.h".into(), &header),
                FileCandidateOverlay::from_index("api.c".into(), &source),
            ],
        );
        let semantic = CandidateQueryService::new(None, &snapshot, "main.c", None, None)
            .semantic_candidates("shared_api", SemanticIntent::Call)
            .expect("semantic function candidates");

        let presentations = navigation_presentations(&semantic, false, "main.c");
        assert_eq!(presentations[0].path, "api.c");
        assert!(presentations[0].base_match > presentations[1].base_match);
    }

    #[test]
    fn navigation_uses_path_locality_after_tier_and_match_quality() {
        let near = parse_with_handle(
            Path::new("src/shared.c"),
            "int shared_value = 1;\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let far = parse_with_handle(
            Path::new("lib/shared.c"),
            "int shared_value = 2;\n",
            None,
            ParseFacts::HOVER_SEMANTICS,
        );
        let snapshot = CandidateOverlaySnapshot::new(
            1,
            vec![
                FileCandidateOverlay::from_index("lib/shared.c".into(), &far),
                FileCandidateOverlay::from_index("src/shared.c".into(), &near),
            ],
        );
        let semantic = CandidateQueryService::new(None, &snapshot, "src/main.c", None, None)
            .semantic_candidates("shared_value", SemanticIntent::Value)
            .expect("semantic object candidates");

        let presentations = navigation_presentations(&semantic, false, "src/main.c");
        assert_eq!(
            presentations
                .iter()
                .map(|candidate| candidate.path.as_str())
                .collect::<Vec<_>>(),
            vec!["src/shared.c", "lib/shared.c"]
        );
    }
}
