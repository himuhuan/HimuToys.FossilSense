//! Shared owner and member evidence; no protocol types or editor state.
use super::{CandidateOverlaySnapshot, CandidateQueryService};
use crate::call_service::CallReadHandle;
use crate::parser::MemberKind;
use crate::{parser, query};
use anyhow::Result;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

pub(crate) const MEMBER_SCAN_LIMIT: usize = 256;
pub(crate) const OWNER_VISIT_LIMIT: usize = 64;
pub(crate) const MEMBER_CHAIN_LIMIT: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemberPolicy {
    BoundNavigation,
    ExploratoryCompletion,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct MemberTypeName {
    name: String,
    domain: Option<parser::LookupDomain>,
}

pub(crate) struct MemberResolutionService<'a> {
    roots: &'a [PathBuf],
    contexts: &'a HashMap<PathBuf, MemberRootQueryContext>,
    policy: MemberPolicy,
    owner_remaining: usize,
    strict_c: bool,
    pub(crate) member_remaining: usize,
}

impl<'a> MemberResolutionService<'a> {
    pub(crate) fn new(
        roots: &'a [PathBuf],
        contexts: &'a HashMap<PathBuf, MemberRootQueryContext>,
        policy: MemberPolicy,
    ) -> Self {
        Self {
            roots,
            contexts,
            policy,
            owner_remaining: OWNER_VISIT_LIMIT,
            strict_c: false,
            member_remaining: MEMBER_SCAN_LIMIT,
        }
    }
    pub(crate) fn resolve_names(&mut self, names: &[String]) -> Result<RootRecordResolution> {
        let hints: Vec<_> = names
            .iter()
            .map(|name| MemberTypeName {
                name: name.clone(),
                domain: self.strict_c.then_some(parser::LookupDomain::Type),
            })
            .collect();
        self.resolve_types(&hints)
    }
    pub(crate) fn resolve_types(
        &mut self,
        names: &[MemberTypeName],
    ) -> Result<RootRecordResolution> {
        resolve_record_names_across_roots(
            self.roots,
            names,
            self.contexts,
            &mut self.owner_remaining,
            self.strict_c,
        )
    }
    pub(crate) fn resolve_owner_spelling(
        &mut self,
        spelling: &str,
        c_ordinary: bool,
    ) -> Result<RootRecordResolution> {
        let tag = spelling
            .strip_prefix("struct ")
            .or_else(|| spelling.strip_prefix("union "));
        let (name, domain) = match tag {
            Some(name) => (name.trim(), Some(parser::LookupDomain::Tag)),
            None => (spelling, c_ordinary.then_some(parser::LookupDomain::Type)),
        };
        self.strict_c = self.policy == MemberPolicy::BoundNavigation && c_ordinary;
        self.resolve_types(&[MemberTypeName {
            name: name.to_owned(),
            domain,
        }])
    }
    pub(crate) fn resolve_receiver(
        &mut self,
        index: &parser::FileSemanticIndex,
        receiver: &str,
        byte: usize,
    ) -> Result<RootRecordResolution> {
        let Some(name) = Self::receiver_record(index, receiver, byte, self.policy) else {
            return Ok(RootRecordResolution::default());
        };
        let spelling = index
            .request_facts()
            .local_bindings
            .iter()
            .filter(|binding| {
                binding.name == receiver
                    && query::local_binding_visible_for_completion(binding, byte)
            })
            .max_by_key(|binding| binding.decl_start_byte)
            .and_then(|binding| binding.type_text.as_deref());
        let tag = spelling
            .filter(|spelling| spelling.starts_with("struct ") || spelling.starts_with("union "));
        self.resolve_owner_spelling(
            tag.unwrap_or(&name),
            index.language == crate::semantic_model::SemanticLanguage::C,
        )
    }

    pub(crate) fn next_types(
        &mut self,
        owners: &[(PathBuf, Vec<query::RecordCandidate>)],
        name: &str,
    ) -> Result<(Vec<MemberTypeName>, bool)> {
        member_type_names_for_segment(
            owners,
            name,
            self.contexts,
            &mut self.member_remaining,
            self.policy,
            self.strict_c,
        )
    }
    pub(crate) fn fallback(
        &mut self,
        root: &PathBuf,
        prefix: &str,
        limit: usize,
    ) -> Result<(Vec<crate::model::MemberCandidate>, bool)> {
        if self.policy != MemberPolicy::ExploratoryCompletion {
            return Ok((Vec::new(), false));
        }
        if self.member_remaining == 0 {
            return Ok((Vec::new(), true));
        }
        match self.contexts.get(root) {
            Some(context) => context.service().fallback_member_candidates_with_budget(
                prefix,
                limit,
                &mut self.member_remaining,
            ),
            None => Ok((Vec::new(), false)),
        }
    }

    pub(crate) fn receiver_record(
        index: &parser::FileSemanticIndex,
        name: &str,
        byte: usize,
        policy: MemberPolicy,
    ) -> Option<String> {
        if index.fact_availability(parser::FactGroup::LocalDeclarations)
            != parser::FactAvailability::Available
            || (policy == MemberPolicy::BoundNavigation
                && index.fact_availability(parser::FactGroup::LocalBindings)
                    != parser::FactAvailability::Available)
        {
            return None;
        }
        let request = index.request_facts();
        // The nearest declaration in another function is not receiver evidence.
        // A nearer scalar binding also shadows an older record declaration.
        let binding = index
            .request_facts()
            .local_bindings
            .iter()
            .filter(|binding| {
                binding.name == name && query::local_binding_visible_for_completion(binding, byte)
            })
            .max_by_key(|binding| binding.decl_start_byte);
        let Some(binding) = binding else {
            return (policy == MemberPolicy::ExploratoryCompletion)
                .then(|| parser::infer_receiver_record(request.local_declarations, name, byte))
                .flatten();
        };
        if index
            .cursor
            .at(binding.decl_start_byte)
            .is_some_and(|syntax| syntax.conditional)
        {
            return None;
        }
        let declaration = request.local_declarations.iter().find(|declaration| {
            declaration.name == name && declaration.decl_start_byte == binding.decl_start_byte
        })?;
        parser::infer_receiver_record(std::slice::from_ref(declaration), name, byte)
    }
}

#[derive(Clone)]
pub(crate) struct MemberRootQueryContext {
    pub(crate) handle: Option<Arc<CallReadHandle>>,
    pub(crate) declaration_index: Option<Arc<crate::declaration_index::SemanticDeclarationIndex>>,
    pub(crate) overlay: Arc<CandidateOverlaySnapshot>,
    pub(crate) current_path: String,
    pub(crate) reach_graph: Option<Arc<crate::reachability::ReachGraph>>,
    pub(crate) semantic_generation: crate::call_model::SemanticGeneration,
    pub(crate) semantic_family: crate::semantic_model::SemanticFamily,
}

impl MemberRootQueryContext {
    pub(crate) fn service(&self) -> CandidateQueryService<'_> {
        CandidateQueryService::new_with_declarations_for_family(
            self.handle.as_deref(),
            self.declaration_index.as_deref(),
            self.overlay.as_ref(),
            &self.current_path,
            None,
            self.reach_graph.as_deref(),
            self.semantic_family,
        )
    }
}

#[derive(Default)]
pub(crate) struct RootRecordResolution {
    pub(crate) candidates: Vec<(PathBuf, Vec<crate::query::RecordCandidate>)>,
    pub(crate) authoritative: bool,
    pub(crate) incomplete: bool,
    pub(crate) budget_exhausted: bool,
    pub(crate) ambiguous: bool,
}

fn member_type_names_for_segment(
    record_candidates_by_root: &[(PathBuf, Vec<crate::query::RecordCandidate>)],
    member_name: &str,
    member_root_contexts: &HashMap<PathBuf, MemberRootQueryContext>,
    scan_remaining: &mut usize,
    policy: MemberPolicy,
    strict_c: bool,
) -> Result<(Vec<MemberTypeName>, bool)> {
    let mut names = Vec::new();
    let mut truncated = false;
    for (root, candidates) in record_candidates_by_root {
        let Some(highest_rank) = candidates
            .iter()
            .map(|candidate| candidate.tier.rank())
            .max()
        else {
            continue;
        };
        let selected: Vec<_> = candidates
            .iter()
            .filter(|candidate| {
                policy == MemberPolicy::BoundNavigation || candidate.tier.rank() == highest_rank
            })
            .cloned()
            .collect();
        if selected.is_empty() {
            continue;
        }
        let Some(context) = member_root_contexts.get(root) else {
            continue;
        };
        if *scan_remaining == 0 {
            truncated = true;
            break;
        }
        let read = context.service().members_for_records_limited(
            &selected,
            Some(member_name),
            *scan_remaining,
        )?;
        *scan_remaining = scan_remaining.saturating_sub(read.scanned);
        truncated |= read.truncated;
        for member in read.candidates {
            if member.kind == MemberKind::Field && member.name == member_name {
                if let Some(type_name) = member.type_name {
                    if policy == MemberPolicy::BoundNavigation && member.type_domain.is_none() {
                        truncated = true;
                        continue;
                    }
                    let domain = match member.type_domain {
                        Some(crate::semantic_model::TypeNameDomain::Tag) => {
                            Some(parser::LookupDomain::Tag)
                        }
                        Some(crate::semantic_model::TypeNameDomain::Ordinary) if strict_c => {
                            Some(parser::LookupDomain::Type)
                        }
                        _ => None,
                    };
                    names.push(MemberTypeName {
                        name: type_name,
                        domain,
                    });
                }
            }
        }
    }
    names.sort_by(|a, b| a.name.cmp(&b.name).then(a.domain.cmp(&b.domain)));
    names.dedup();
    Ok((names, truncated))
}

fn resolve_record_names_across_roots(
    roots: &[PathBuf],
    type_names: &[MemberTypeName],
    member_root_contexts: &HashMap<PathBuf, MemberRootQueryContext>,
    owner_remaining: &mut usize,
    strict_c: bool,
) -> Result<RootRecordResolution> {
    const MULTI_ROOT_RECORD_LIMIT: usize = crate::query::TYPE_CANDIDATE_LIMIT * 4;

    let mut combined = RootRecordResolution::default();
    let mut frontier = VecDeque::new();
    let mut strongest_frontier = HashMap::new();
    for type_name in type_names {
        enqueue_type_frontier(
            &mut frontier,
            &mut strongest_frontier,
            type_name.clone(),
            crate::model::ScopeTier::Current,
        );
    }
    let mut candidates_by_root: HashMap<PathBuf, Vec<crate::query::RecordCandidate>> =
        HashMap::new();
    let mut record_count = 0usize;

    'frontier: while let Some((hint, tier_cap)) = frontier.pop_front() {
        if strongest_frontier.get(&hint).copied() != Some(tier_cap) {
            continue;
        }
        for root in roots {
            if *owner_remaining == 0 {
                combined.incomplete = true;
                combined.budget_exhausted = true;
                break 'frontier;
            }
            let Some(context) = member_root_contexts.get(root) else {
                continue;
            };
            let bundle = match hint.domain {
                Some(domain) => context.service().type_candidates_for_member_domain(
                    &hint.name,
                    domain,
                    owner_remaining,
                )?,
                None => context
                    .service()
                    .type_candidates_with_budget(&hint.name, owner_remaining)?,
            };
            combined.authoritative |= bundle.shadowed_evidence
                || !bundle.records.candidates.is_empty()
                || !bundle.aliases.candidates.is_empty();
            combined.budget_exhausted |=
                bundle.records.coverage.truncated || bundle.aliases.coverage.truncated;
            combined.incomplete |= !bundle.records.coverage.permits_uniqueness()
                || !bundle.aliases.coverage.permits_uniqueness();

            let mut records = bundle.records.candidates;
            for resolution in bundle.alias_resolutions {
                combined.ambiguous |=
                    resolution.status == crate::query::AliasResolutionStatus::AmbiguousRecord;
                combined.incomplete |=
                    resolution.status != crate::query::AliasResolutionStatus::UniqueRecord;
                records.extend(resolution.terminal_records);
            }
            for alias in bundle.aliases.candidates {
                let target_name = match alias.target {
                    crate::query::TypeAliasTarget::TypeName(name) => Some(MemberTypeName {
                        name,
                        domain: strict_c.then_some(parser::LookupDomain::Type),
                    }),
                    crate::query::TypeAliasTarget::NamedRecord { tag, .. } => {
                        Some(MemberTypeName {
                            name: tag,
                            domain: strict_c.then_some(parser::LookupDomain::Tag),
                        })
                    }
                    crate::query::TypeAliasTarget::StableRecord(_) => None,
                };
                if let Some(target_name) = target_name.filter(|hint| !hint.name.is_empty()) {
                    let next_tier = if tier_cap.rank() <= alias.tier.rank() {
                        tier_cap
                    } else {
                        alias.tier
                    };
                    enqueue_type_frontier(
                        &mut frontier,
                        &mut strongest_frontier,
                        target_name,
                        next_tier,
                    );
                }
            }

            let root_candidates = candidates_by_root.entry(root.clone()).or_default();
            for mut record in records {
                if tier_cap.rank() < record.tier.rank() {
                    record.tier = tier_cap;
                }
                if let Some(existing) = root_candidates
                    .iter_mut()
                    .find(|candidate| candidate.identity == record.identity)
                {
                    if record.tier.rank() > existing.tier.rank() {
                        *existing = record;
                    }
                    continue;
                }
                if record_count >= MULTI_ROOT_RECORD_LIMIT {
                    combined.incomplete = true;
                    break 'frontier;
                }
                record_count += 1;
                root_candidates.push(record);
            }
        }
    }

    for root in roots {
        if let Some(mut candidates) = candidates_by_root.remove(root) {
            candidates.sort_by(|left, right| {
                right
                    .tier
                    .rank()
                    .cmp(&left.tier.rank())
                    .then_with(|| left.path.cmp(&right.path))
                    .then_with(|| left.name_range.start_byte.cmp(&right.name_range.start_byte))
            });
            if !candidates.is_empty() {
                combined.candidates.push((root.clone(), candidates));
            }
        }
    }
    Ok(combined)
}

fn enqueue_type_frontier(
    frontier: &mut VecDeque<(MemberTypeName, crate::model::ScopeTier)>,
    strongest: &mut HashMap<MemberTypeName, crate::model::ScopeTier>,
    name: MemberTypeName,
    tier: crate::model::ScopeTier,
) {
    if name.name.is_empty()
        || strongest
            .get(&name)
            .is_some_and(|known| known.rank() >= tier.rank())
    {
        return;
    }
    strongest.insert(name.clone(), tier);
    frontier.push_back((name, tier));
}

pub(crate) fn weak_receiver_lookup_names(receiver_name: &str) -> Vec<String> {
    let hint = query::normalized_receiver_record_hint(receiver_name);
    let mut names = Vec::new();
    if !hint.is_empty() {
        names.push(hint.clone());
        let mut chars = hint.chars();
        if let Some(first) = chars.next() {
            let pascal = format!("{}{}", first.to_ascii_uppercase(), chars.as_str());
            names.push(pascal);
        }
    }
    names.sort();
    names.dedup();
    names
}

pub(crate) fn weak_receiver_matches_record(
    receiver_name: &str,
    record: &crate::query::RecordCandidate,
) -> bool {
    let hint = query::normalized_receiver_record_hint(receiver_name);
    if hint.is_empty() {
        return false;
    }
    record.display_name.eq_ignore_ascii_case(&hint)
        || record
            .tag_name
            .as_deref()
            .is_some_and(|name| name.eq_ignore_ascii_case(&hint))
        || record
            .typedef_name
            .as_deref()
            .is_some_and(|name| name.eq_ignore_ascii_case(&hint))
}

/// Typed member occurrence. The owner identity and generation travel with the
/// member table handle; its integer ID is never used as a declaration-table ID.
#[derive(Clone)]
pub(crate) struct OwnerMemberRef {
    pub(crate) root: PathBuf,
    pub(crate) owner: query::RecordCandidateIdentity,
    pub(crate) member: crate::model::MemberCandidate,
    pub(crate) generation: crate::call_model::SemanticGeneration,
    pub(crate) family: crate::semantic_model::SemanticFamily,
    pub(crate) captured_source: Option<Arc<str>>,
}

impl MemberResolutionService<'_> {
    pub(crate) fn resolve_selected(
        &mut self,
        mut owners: Vec<(PathBuf, Vec<query::RecordCandidate>)>,
        chain: &[String],
        selector: &str,
    ) -> Result<query::BindingResolution<OwnerMemberRef>> {
        let unsupported = || query::BindingResolution::Unsupported {
            domain_hint: parser::LookupDomain::Member,
        };
        if chain.len() > MEMBER_CHAIN_LIMIT {
            return Ok(unsupported());
        }
        for segment in chain {
            let (names, truncated) = self.next_types(&owners, segment)?;
            if truncated {
                return Ok(unsupported());
            }
            let resolution = self.resolve_types(&names)?;
            if resolution.budget_exhausted {
                return Ok(unsupported());
            }
            owners = resolution.candidates;
        }
        let mut members = Vec::new();
        for (root, records) in owners {
            let Some(context) = self.contexts.get(&root) else {
                continue;
            };
            for record in records {
                if self.member_remaining == 0 {
                    return Ok(unsupported());
                }
                let found = context.service().members_for_records_limited(
                    std::slice::from_ref(&record),
                    Some(selector),
                    self.member_remaining,
                )?;
                self.member_remaining = self.member_remaining.saturating_sub(found.scanned);
                if found.truncated {
                    return Ok(unsupported());
                }
                for member in found
                    .candidates
                    .into_iter()
                    .filter(|member| member.name == selector)
                {
                    let captured_source = context
                        .overlay
                        .source_by_path
                        .get(&member.owner_path)
                        .cloned();
                    members.push(OwnerMemberRef {
                        captured_source,
                        root: root.clone(),
                        owner: record.identity.clone(),
                        member,
                        generation: context.semantic_generation,
                        family: context.semantic_family,
                    });
                }
            }
        }
        members.sort_by(|a, b| {
            a.member
                .owner_path
                .cmp(&b.member.owner_path)
                .then(a.member.handle.start_line.cmp(&b.member.handle.start_line))
                .then(a.member.handle.start_col.cmp(&b.member.handle.start_col))
        });
        members.dedup_by(|a, b| {
            a.root == b.root && a.owner == b.owner && a.member.handle == b.member.handle
        });
        Ok(match members.len() {
            0 => query::BindingResolution::UnresolvedWithinDomain {
                domain: parser::LookupDomain::Member,
                reason: query::BindingReason::DedicatedDomain,
            },
            1 => query::BindingResolution::Resolved(members.remove(0)),
            _ => query::BindingResolution::Ambiguous(members),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context(source: &str) -> MemberRootQueryContext {
        let parsed = parser::parse_with_language(
            std::path::Path::new("main.c"),
            source,
            crate::config::SourceLanguage::C,
            parser::ParseFacts::HOVER_SEMANTICS,
        );
        MemberRootQueryContext {
            handle: None,
            declaration_index: None,
            overlay: Arc::new(CandidateOverlaySnapshot::new(
                1,
                vec![crate::candidate_service::FileCandidateOverlay::from_index(
                    "main.c".into(),
                    &parsed,
                )],
            )),
            current_path: "main.c".into(),
            reach_graph: None,
            semantic_generation: crate::call_model::SemanticGeneration(1),
            semantic_family: crate::semantic_model::SemanticFamily::CFamily,
        }
    }
    #[test]
    fn owner_member_receiver_in_incomplete_cpp_body() {
        let source = "void use_remote(RemoteAlias value) { value.f }\n";
        let parsed = parser::parse_with_language(
            std::path::Path::new("main.cpp"),
            source,
            crate::config::SourceLanguage::Cpp,
            parser::ParseFacts::MEMBER,
        );
        assert_eq!(
            MemberResolutionService::receiver_record(
                &parsed,
                "value",
                source.find("value.f").unwrap() + 7,
                MemberPolicy::ExploratoryCompletion
            ),
            Some("RemoteAlias".into()),
            "bindings={:?}, declarations={:?}",
            parsed.local_bindings,
            parsed.local_declarations
        );
    }
    #[test]
    fn owner_member_budget_is_shared_across_roots_and_requests() {
        let roots: Vec<_> = (0..100)
            .map(|i| PathBuf::from(format!("root{i}")))
            .collect();
        let ctx = context("struct S { int state; };\n");
        let contexts: HashMap<_, _> = roots
            .iter()
            .map(|root| (root.clone(), ctx.clone()))
            .collect();
        let mut resolver =
            MemberResolutionService::new(&roots, &contexts, MemberPolicy::BoundNavigation);
        let resolution = resolver.resolve_names(&["S".into()]).unwrap();
        assert!(resolution.budget_exhausted);
        assert_eq!(resolver.owner_remaining, 0);
        assert!(
            resolution
                .candidates
                .iter()
                .map(|(_, items)| items.len())
                .sum::<usize>()
                <= OWNER_VISIT_LIMIT
        );
        assert!(resolver
            .resolve_names(&["S".into()])
            .unwrap()
            .candidates
            .is_empty());
    }
    #[test]
    fn owner_member_bound_policy_cannot_enter_global_fallback() {
        let root = PathBuf::from("root");
        let roots = [root.clone()];
        let contexts = HashMap::from([(root.clone(), context("struct S { int state; };\n"))]);
        let mut bound =
            MemberResolutionService::new(&roots, &contexts, MemberPolicy::BoundNavigation);
        assert!(bound.fallback(&root, "sta", 100).unwrap().0.is_empty());
        assert_eq!(bound.member_remaining, MEMBER_SCAN_LIMIT);
        let mut exploratory =
            MemberResolutionService::new(&roots, &contexts, MemberPolicy::ExploratoryCompletion);
        assert_eq!(
            exploratory.fallback(&root, "sta", 100).unwrap().0[0].name,
            "state"
        );
        assert!(exploratory.member_remaining < MEMBER_SCAN_LIMIT);
    }
    #[test]
    fn owner_member_nested_alias_expansion_uses_the_same_budget() {
        let mut source = String::from("struct S { int state; }; typedef struct S A0;\n");
        for i in 1..100 {
            source.push_str(&format!("typedef A{} A{};\n", i - 1, i));
        }
        let root = PathBuf::from("root");
        let roots = [root.clone()];
        let contexts = HashMap::from([(root, context(&source))]);
        let mut resolver =
            MemberResolutionService::new(&roots, &contexts, MemberPolicy::BoundNavigation);
        let resolution = resolver.resolve_names(&["A99".into()]).unwrap();
        assert!(resolution.budget_exhausted);
        assert_eq!(resolver.owner_remaining, 0);
    }
    #[test]
    fn owner_member_fallback_candidate_budget_is_shared_across_roots() {
        let source = format!(
            "struct S {{ {} }};",
            (0..400)
                .map(|i| format!("int state{i};"))
                .collect::<String>()
        );
        let roots = [PathBuf::from("a"), PathBuf::from("b")];
        let ctx = context(&source);
        let contexts = HashMap::from([(roots[0].clone(), ctx.clone()), (roots[1].clone(), ctx)]);
        let mut resolver =
            MemberResolutionService::new(&roots, &contexts, MemberPolicy::ExploratoryCompletion);
        let (found, truncated) = resolver.fallback(&roots[0], "state", 1000).unwrap();
        assert!(truncated);
        assert_eq!(found.len(), MEMBER_SCAN_LIMIT);
        assert_eq!(resolver.member_remaining, 0);
        assert!(resolver
            .fallback(&roots[1], "state", 1000)
            .unwrap()
            .0
            .is_empty());
    }
}
