use super::*;

/// Generation-pinned facade that recalls narrow durable rows, shadows them
/// with all divergent open documents, and hands a single candidate set to all
/// callable consumers. The pure arity/counterpart policy remains in `query`.
pub struct CandidateQueryService<'a> {
    pub(super) handle: Option<&'a CallReadHandle>,
    pub(super) declaration_index: Option<&'a crate::declaration_index::SemanticDeclarationIndex>,
    pub(super) overlays: &'a CandidateOverlaySnapshot,
    pub(super) current_path: &'a str,
    pub(super) current_reach: Option<Arc<ReachScope>>,
    pub(super) reach_graph: Option<&'a ReachGraph>,
    pub(super) exact_name_limit: usize,
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
            declaration_index: None,
            overlays,
            current_path,
            current_reach,
            reach_graph,
            exact_name_limit: DEFAULT_EXACT_NAME_CANDIDATE_LIMIT,
        }
    }

    pub fn new_with_declarations(
        handle: Option<&'a CallReadHandle>,
        declaration_index: Option<&'a crate::declaration_index::SemanticDeclarationIndex>,
        overlays: &'a CandidateOverlaySnapshot,
        current_path: &'a str,
        current_reach: Option<&'a ReachScope>,
        reach_graph: Option<&'a ReachGraph>,
    ) -> Self {
        let mut service = Self::new(handle, overlays, current_path, current_reach, reach_graph);
        service.declaration_index = declaration_index;
        service
    }

    /// Durable paths that must be recalled before a workspace-wide exact-name
    /// cap is allowed to spend the request budget. Dirty paths are omitted:
    /// their overlay is authoritative even when it is an empty tombstone.
    pub(super) fn durable_priority_path_groups(&self) -> (Vec<String>, Vec<String>) {
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
        let (base_rows, mut truncated) = match self.handle {
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

                Ok((anchors, anchor_truncated))
            })?,
            None => (Vec::new(), false),
        };
        let scanned = base_rows.len() + self.overlays.callable_anchors(name).len();
        let resolve_context = ResolveContext {
            current_path: Some(self.current_path),
            reach: self.current_reach.as_deref(),
            direct_external_files: None,
        };
        let base_anchors: Vec<ResolvedCallableAnchor> = base_rows
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
        let overlay_anchors = self
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
                None
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
    pub(super) fn path_evidence(
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
