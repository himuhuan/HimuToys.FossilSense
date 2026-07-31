use super::*;

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

impl CandidateQueryService<'_> {
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
                    let (record_rows, record_truncated) =
                        store.member_view().record_rows_by_name_family_limited(
                            &next_name,
                            self.semantic_family,
                            TYPE_CANDIDATE_LIMIT,
                        )?;
                    let (alias_rows, alias_truncated) =
                        store.member_view().alias_rows_by_name_family_limited(
                            &next_name,
                            self.semantic_family,
                            TYPE_CANDIDATE_LIMIT,
                        )?;
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

            let overlay_records = self
                .overlays
                .records_for_family(&next_name, self.semantic_family);
            let overlay_aliases = self
                .overlays
                .aliases_for_family(&next_name, self.semantic_family);
            scanned += overlay_records.len() + overlay_aliases.len();
            records.extend(overlay_records.into_iter().map(|fact| {
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
                .into_iter()
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
                    handle: crate::model::MemberCandidateHandle::new(
                        None, path, record_key, member,
                    ),
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
                store
                    .member_view()
                    .fallback_member_candidates_family_limited(
                        prefix,
                        limit,
                        Some(&resolve_context),
                        self.semantic_family,
                    )
            })?,
            None => (Vec::new(), false),
        };
        members.retain(|member| !self.overlays.shadows(&member.owner_path));

        let (overlay_members, overlay_truncated) =
            self.overlays.fallback_members_by_prefix_for_family_limited(
                prefix,
                self.semantic_family,
                MEMBER_FALLBACK_OVERLAY_SCAN_LIMIT,
            );
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
                handle: crate::model::MemberCandidateHandle::new(
                    None,
                    path,
                    &member.record_key,
                    member,
                ),
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
