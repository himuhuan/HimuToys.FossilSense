use super::*;

const TEST_RELATION_LIMIT: usize = 100_000;
const TEST_SITE_LIMIT: usize = 10_000;

fn relations(
    index: &RelationQueryIndex,
    direction: RelationDirection,
    entity_key: &str,
) -> Vec<CallRelation> {
    index
        .relation_page(
            direction,
            entity_key,
            0,
            TEST_RELATION_LIMIT,
            TEST_SITE_LIMIT,
        )
        .relations
}

fn range(start: usize, end: usize) -> SourceRange {
    SourceRange {
        start: SourcePosition {
            line: 0,
            character: start as u32,
        },
        end: SourcePosition {
            line: 0,
            character: end as u32,
        },
        start_byte: start,
        end_byte: end,
    }
}
fn anchor(key: &str, name: &str, path: &str, role: AnchorRole, arity: u32) -> CallableAnchorRow {
    CallableAnchorRow {
        id: 0,
        path: path.into(),
        source: "workspace".into(),
        directly_included: false,
        entity_key: key.into(),
        anchor_fingerprint: format!("{key}-{role:?}"),
        name: name.into(),
        qualified_name: name.into(),
        owner: None,
        owner_kind: None,
        kind: "function".into(),
        role: role.as_str().into(),
        linkage_kind: "external".into(),
        linkage_file: None,
        signature: format!("{name}()"),
        canonical_signature: format!("{name}()"),
        presentation_signature: format!("{name}();"),
        signature_fidelity: crate::call_model::SignatureFidelity::AstExact,
        min_arity: Some(arity),
        max_arity: Some(arity),
        variadic: false,
        name_range: range(4, 4 + name.len()),
        declaration_range: range(0, 20),
        body_range: (role == AnchorRole::Definition).then(|| range(20, 40)),
        guard: None,
        provenance: "ast".into(),
        syntax_error_overlap: false,
    }
}
fn call(caller: &str, callee: &str, path: &str, arity: u32) -> CallSiteRow {
    CallSiteRow {
        id: 0,
        path: path.into(),
        source: "workspace".into(),
        caller_entity_key: caller.into(),
        site_fingerprint: format!("{caller}-{callee}"),
        expression_range: range(25, 35),
        callee_range: range(25, 25 + callee.len()),
        callee_name: Some(callee.into()),
        qualified_name: None,
        call_form: "direct_name".into(),
        argument_count: Some(arity),
        guard: None,
        provenance: "ast".into(),
        syntax_error_overlap: false,
    }
}

fn build_with_reach(
    anchors: Vec<CallableAnchorRow>,
    calls: Vec<CallSiteRow>,
    graph: &crate::reachability::ReachGraph,
    incomplete: bool,
) -> RelationQueryIndex {
    RelationQueryIndex::build_from_facts_with_context(
        anchors.into_iter().map(anchor_from_row).collect(),
        calls.into_iter().map(call_from_row),
        CoverageSummary::default(),
        Some(graph),
        incomplete,
        false,
    )
}

#[test]
fn query_index_groups_variants_and_resolves_one_hop_both_directions() {
    let graph =
        crate::reachability::ReachGraph::new(vec![("b.c".into(), "b.h".into())], vec![], vec![]);
    let catalog = build_with_reach(
        vec![
            anchor("a", "caller", "a.c", AnchorRole::Definition, 0),
            anchor("b", "target", "b.h", AnchorRole::Declaration, 1),
            anchor("b", "target", "b.c", AnchorRole::Definition, 1),
        ],
        vec![call("a", "target", "a.c", 1)],
        &graph,
        false,
    );
    assert_eq!(catalog.len(), 2);
    assert_eq!(catalog.entity("b").unwrap().variants.len(), 2);
    assert_eq!(
        catalog.entity("b").unwrap().primary_anchor.path,
        "b.c",
        "call relations must keep the source definition as the primary anchor"
    );
    let outgoing = relations(&catalog, RelationDirection::Outgoing, "a");
    assert_eq!(outgoing.len(), 1);
    assert_eq!(outgoing[0].callee.as_ref().unwrap().entity_key, "b");
    assert_eq!(
        relations(&catalog, RelationDirection::Incoming, "b")[0]
            .caller
            .entity_key,
        "a"
    );
}

#[test]
fn relation_page_propagates_candidate_recall_truncation() {
    let graph = crate::reachability::ReachGraph::new(Vec::new(), Vec::new(), Vec::new());
    let catalog = RelationQueryIndex::build_from_facts_with_context(
        vec![
            anchor("a", "caller", "a.c", AnchorRole::Definition, 0),
            anchor("b", "target", "b.c", AnchorRole::Definition, 0),
        ]
        .into_iter()
        .map(anchor_from_row)
        .collect(),
        [call("a", "target", "a.c", 0)]
            .into_iter()
            .map(call_from_row),
        CoverageSummary::default(),
        Some(&graph),
        false,
        true,
    );

    let page = catalog.relation_page(RelationDirection::Outgoing, "a", 0, 10, 10);
    assert!(page.candidate_limited);
    assert_eq!(page.relations.len(), 1);
}

#[test]
fn same_parser_family_does_not_merge_one_to_many_or_many_to_one_variants() {
    let one_to_many = crate::reachability::ReachGraph::new(
        vec![
            ("impl.c".into(), "one.h".into()),
            ("impl.c".into(), "two.h".into()),
        ],
        vec![],
        vec![],
    );
    let catalog = build_with_reach(
        vec![
            anchor("caller", "caller", "caller.c", AnchorRole::Definition, 0),
            anchor("family", "target", "impl.c", AnchorRole::Definition, 0),
            anchor("family", "target", "one.h", AnchorRole::Declaration, 0),
            anchor("family", "target", "two.h", AnchorRole::Declaration, 0),
        ],
        vec![call("caller", "target", "caller.c", 0)],
        &one_to_many,
        false,
    );
    let targets = &catalog.by_name["target"];
    assert_eq!(targets.len(), 3);
    assert!(targets
        .iter()
        .all(|id| catalog.entity_by_id(*id).variants.len() == 1));
    assert_eq!(
        relations(&catalog, RelationDirection::Outgoing, "caller")
            .iter()
            .filter(|relation| relation.confidence == RelationConfidence::Ambiguous)
            .count(),
        3
    );

    let many_to_one = crate::reachability::ReachGraph::new(
        vec![
            ("one.c".into(), "api.h".into()),
            ("two.c".into(), "api.h".into()),
        ],
        vec![],
        vec![],
    );
    let catalog = build_with_reach(
        vec![
            anchor("family", "target", "one.c", AnchorRole::Definition, 0),
            anchor("family", "target", "two.c", AnchorRole::Definition, 0),
            anchor("family", "target", "api.h", AnchorRole::Declaration, 0),
        ],
        vec![],
        &many_to_one,
        false,
    );
    let targets = &catalog.by_name["target"];
    assert_eq!(targets.len(), 3);
    assert!(targets
        .iter()
        .all(|id| catalog.entity_by_id(*id).variants.len() == 1));
}

#[test]
fn incomplete_dirty_overlay_disables_otherwise_unique_pairing() {
    let graph = crate::reachability::ReachGraph::new(
        vec![("impl.c".into(), "api.h".into())],
        vec![],
        vec![],
    );
    let catalog = build_with_reach(
        vec![
            anchor("family", "target", "impl.c", AnchorRole::Definition, 0),
            anchor("family", "target", "api.h", AnchorRole::Declaration, 0),
        ],
        vec![],
        &graph,
        true,
    );
    assert_eq!(catalog.by_name["target"].len(), 2);
    assert!(catalog.by_name["target"]
        .iter()
        .all(|id| catalog.entity_by_id(*id).variants.len() == 1));
}

#[test]
fn truncated_candidate_recall_disables_otherwise_unique_pairing() {
    let graph = crate::reachability::ReachGraph::new(
        vec![("impl.c".into(), "api.h".into())],
        vec![],
        vec![],
    );
    let catalog = RelationQueryIndex::build_from_facts_with_context(
        vec![
            anchor("family", "target", "impl.c", AnchorRole::Definition, 0),
            anchor("family", "target", "api.h", AnchorRole::Declaration, 0),
        ]
        .into_iter()
        .map(anchor_from_row)
        .collect(),
        Vec::<CallSiteFact>::new(),
        CoverageSummary::default(),
        Some(&graph),
        false,
        true,
    );

    assert_eq!(catalog.by_name["target"].len(), 2);
    assert!(catalog.by_name["target"].iter().all(|id| {
        catalog.entity_candidate_limited[*id as usize]
            && catalog.entity_by_id(*id).variants.len() == 1
    }));
}

#[test]
fn open_same_signature_source_without_an_edge_does_not_revoke_a_direct_pair() {
    let graph = crate::reachability::ReachGraph::new(
        vec![("closed.c".into(), "api.h".into())],
        vec!["open.c".into()],
        vec![],
    );
    let catalog = build_with_reach(
        vec![
            anchor("family", "target", "closed.c", AnchorRole::Definition, 0),
            anchor("family", "target", "open.c", AnchorRole::Definition, 0),
            anchor("family", "target", "api.h", AnchorRole::Declaration, 0),
        ],
        vec![],
        &graph,
        false,
    );
    assert_eq!(catalog.by_name["target"].len(), 2);
    let mut variant_counts = catalog.by_name["target"]
        .iter()
        .map(|id| catalog.entity_by_id(*id).variants.len())
        .collect::<Vec<_>>();
    variant_counts.sort_unstable();
    assert_eq!(variant_counts, vec![1, 2]);
}

#[test]
fn unrelated_open_name_does_not_disable_closed_unique_pairing() {
    let graph = crate::reachability::ReachGraph::new(
        vec![("foo.c".into(), "foo.h".into())],
        vec!["bar.c".into()],
        vec![],
    );
    let catalog = build_with_reach(
        vec![
            anchor("foo-family", "foo", "foo.c", AnchorRole::Definition, 0),
            anchor("foo-family", "foo", "foo.h", AnchorRole::Declaration, 0),
            anchor("bar-family", "bar", "bar.c", AnchorRole::Definition, 0),
            anchor("bar-family", "bar", "bar.h", AnchorRole::Declaration, 0),
        ],
        vec![],
        &graph,
        false,
    );

    assert_eq!(catalog.by_name["foo"].len(), 1);
    assert_eq!(catalog.entity("foo-family").unwrap().variants.len(), 2);
    assert_eq!(
        catalog.by_name["bar"].len(),
        2,
        "only the exact-name bucket with open reach must lose uniqueness"
    );
}

#[test]
fn strict_counterpart_anchor_budget_retains_every_anchor_as_a_singleton() {
    let graph = crate::reachability::ReachGraph::new(
        vec![("target.c".into(), "target.h".into())],
        vec![],
        vec![],
    );
    let mut anchors = vec![
        anchor(
            "target-family",
            "target",
            "target.c",
            AnchorRole::Definition,
            0,
        ),
        anchor(
            "target-family",
            "target",
            "target.h",
            AnchorRole::Declaration,
            0,
        ),
    ];
    for index in 0..super::grouping::STRICT_COUNTERPART_ANCHOR_BUDGET - 1 {
        anchors.push(anchor(
            &format!("filler-{index}"),
            "target",
            &format!("filler-{index}.txt"),
            AnchorRole::Definition,
            index as u32,
        ));
    }
    let concrete: Vec<_> = anchors.into_iter().map(anchor_from_row).collect();
    let expected = concrete.len();

    let result = super::grouping::semantic_anchor_groups(concrete, Some(&graph), false);

    assert_eq!(result.groups.len(), expected);
    assert!(result
        .groups
        .iter()
        .all(|group| { group.candidate_limited && group.variants.len() == 1 }));
}

#[test]
fn strict_counterpart_edge_budget_bounds_high_duplication_cartesian_work() {
    let mut pair_count = 1usize;
    while pair_count.saturating_mul(pair_count) <= super::grouping::STRICT_COUNTERPART_EDGE_BUDGET {
        pair_count += 1;
    }
    assert!(
        pair_count.saturating_mul(2) < super::grouping::STRICT_COUNTERPART_ANCHOR_BUDGET,
        "the edge-budget fixture must not also hit the anchor budget"
    );

    let mut edges = Vec::with_capacity(pair_count);
    let mut anchors = Vec::with_capacity(pair_count * 2);
    for index in 0..pair_count {
        let source_path = format!("impl-{index}.c");
        let header_path = format!("api-{index}.h");
        edges.push((source_path.clone(), header_path.clone()));
        for (path, role) in [
            (source_path.as_str(), AnchorRole::Definition),
            (header_path.as_str(), AnchorRole::Declaration),
        ] {
            let mut row = anchor(
                &format!("target-family-{index}"),
                "target",
                path,
                role,
                index as u32,
            );
            row.signature = format!("target(type_{index})");
            row.canonical_signature = row.signature.clone();
            row.presentation_signature = format!("{};", row.signature);
            anchors.push(anchor_from_row(row));
        }
    }
    let graph = crate::reachability::ReachGraph::new(edges.clone(), vec![], vec![]);
    let expected = anchors.len();

    let result = super::grouping::semantic_anchor_groups(anchors.clone(), Some(&graph), false);

    assert_eq!(result.groups.len(), expected);
    assert!(
        result
            .groups
            .iter()
            .all(|group| group.candidate_limited && group.variants.len() == 1),
        "an edge-budget hit must disable uniqueness for the whole exact-name bucket"
    );

    let mut catalog_anchors = anchors;
    catalog_anchors.push(anchor_from_row(anchor(
        "small-family",
        "small",
        "small.c",
        AnchorRole::Definition,
        0,
    )));
    catalog_anchors.push(anchor_from_row(anchor(
        "small-family",
        "small",
        "small.h",
        AnchorRole::Declaration,
        0,
    )));
    edges.push(("small.c".to_string(), "small.h".to_string()));
    let graph = crate::reachability::ReachGraph::new(edges, vec![], vec![]);
    let catalog = RelationQueryIndex::build_from_facts_with_context(
        catalog_anchors,
        Vec::<CallSiteFact>::new(),
        CoverageSummary::default(),
        Some(&graph),
        false,
        false,
    );
    let entity_key = catalog
        .entity_by_id(catalog.by_name["target"][0])
        .entity_key
        .clone();
    assert!(
        catalog
            .relation_page(RelationDirection::Outgoing, &entity_key, 0, 10, 10)
            .candidate_limited,
        "the relation page must expose an internal counterpart grouping limit"
    );
    let small_key = catalog
        .entity_by_id(catalog.by_name["small"][0])
        .entity_key
        .clone();
    assert_eq!(catalog.entity(&small_key).unwrap().variants.len(), 2);
    assert!(
        !catalog
            .relation_page(RelationDirection::Outgoing, &small_key, 0, 10, 10)
            .candidate_limited,
        "a budget hit in another exact-name bucket must not taint this entity's coverage"
    );
}

#[test]
fn shared_raw_caller_family_is_disambiguated_by_definition_path() {
    let catalog = RelationQueryIndex::build(
        vec![
            anchor("family", "worker", "one.c", AnchorRole::Definition, 0),
            anchor("family", "worker", "two.c", AnchorRole::Definition, 0),
            anchor("left", "left_target", "left.c", AnchorRole::Definition, 0),
            anchor(
                "right",
                "right_target",
                "right.c",
                AnchorRole::Definition,
                0,
            ),
        ],
        vec![
            call("family", "left_target", "one.c", 0),
            call("family", "right_target", "two.c", 0),
        ],
    );
    let one = catalog
        .entity_at(
            "one.c",
            SourcePosition {
                line: 0,
                character: 5,
            },
        )
        .unwrap();
    let two = catalog
        .entity_at(
            "two.c",
            SourcePosition {
                line: 0,
                character: 5,
            },
        )
        .unwrap();
    assert_ne!(one.entity_key, two.entity_key);
    let one_outgoing = relations(&catalog, RelationDirection::Outgoing, &one.entity_key);
    let two_outgoing = relations(&catalog, RelationDirection::Outgoing, &two.entity_key);
    assert_eq!(one_outgoing.len(), 1);
    assert_eq!(two_outgoing.len(), 1);
    assert_eq!(one_outgoing[0].callee.as_ref().unwrap().name, "left_target");
    assert_eq!(
        two_outgoing[0].callee.as_ref().unwrap().name,
        "right_target"
    );
}

#[test]
fn incompatible_arity_is_unresolved_and_duplicate_names_are_ambiguous() {
    let catalog = RelationQueryIndex::build(
        vec![
            anchor("a", "caller", "a.c", AnchorRole::Definition, 0),
            anchor("b", "target", "b.c", AnchorRole::Definition, 1),
            anchor("c", "target", "c.c", AnchorRole::Definition, 1),
        ],
        vec![
            call("a", "target", "a.c", 1),
            call("a", "missing", "a.c", 2),
        ],
    );
    let outgoing = relations(&catalog, RelationDirection::Outgoing, "a");
    assert_eq!(
        outgoing
            .iter()
            .filter(|r| r.confidence == RelationConfidence::Ambiguous)
            .count(),
        2
    );
    assert!(outgoing
        .iter()
        .any(|r| r.confidence == RelationConfidence::Unresolved));
}

#[test]
fn internal_linkage_uses_active_row_path_not_parser_absolute_path() {
    let caller = anchor("a", "caller", "src/a.c", AnchorRole::Definition, 0);
    let mut target = anchor("b", "target", "src/a.c", AnchorRole::Definition, 0);
    target.linkage_kind = "internal".into();
    target.linkage_file = Some("C:/workspace/src/a.c".into());
    let catalog = RelationQueryIndex::build(
        vec![caller, target],
        vec![call("a", "target", "src/a.c", 0)],
    );
    let outgoing = relations(&catalog, RelationDirection::Outgoing, "a");
    assert_eq!(outgoing.len(), 1);
    assert_eq!(outgoing[0].callee.as_ref().unwrap().entity_key, "b");
    assert!(outgoing[0]
        .evidence
        .supports
        .contains(&EvidenceCode::InternalLinkage));
}

#[test]
fn stale_entity_key_remaps_by_path_signature_and_nearest_anchor() {
    let catalog = RelationQueryIndex::build(
        vec![anchor(
            "fresh",
            "target",
            "src/a.c",
            AnchorRole::Definition,
            1,
        )],
        vec![],
    );
    let entity = catalog.entity("fresh").unwrap();
    let locator = CallableLocator {
        workspace_id: "workspace".into(),
        path: "src/a.c".into(),
        entity_key: "stale".into(),
        anchor_fingerprint: "stale-anchor".into(),
        old_start_byte: entity.primary_anchor.name_range.start_byte + 2,
        signature_digest: signature_digest(&entity.signature),
    };
    assert_eq!(
        catalog.resolve_locator(&locator).unwrap().entity_key,
        "fresh"
    );
}

#[test]
fn root_lookup_only_tests_variants_from_the_requested_file() {
    let header = anchor("shared", "shared", "api.h", AnchorRole::Declaration, 0);
    let mut definition = anchor("shared", "shared", "impl.c", AnchorRole::Definition, 0);
    definition.name_range.start.line = 5;
    definition.name_range.end.line = 5;
    definition.declaration_range.start.line = 5;
    definition.declaration_range.end.line = 6;
    definition.body_range.as_mut().unwrap().start.line = 5;
    definition.body_range.as_mut().unwrap().end.line = 6;
    let local = anchor("local", "local", "impl.c", AnchorRole::Definition, 0);
    let catalog = RelationQueryIndex::build(vec![header, definition, local], vec![]);
    assert_eq!(
        catalog
            .entity_at(
                "impl.c",
                SourcePosition {
                    line: 0,
                    character: 4,
                },
            )
            .unwrap()
            .entity_key,
        "local"
    );
}

#[test]
fn compact_relation_storage_is_shared_by_both_directions_and_pages_before_materializing() {
    let catalog = RelationQueryIndex::build(
        vec![
            anchor("a", "caller", "a.c", AnchorRole::Definition, 0),
            anchor("b", "target", "b.c", AnchorRole::Definition, 0),
        ],
        vec![
            call("a", "target", "a.c", 0),
            call("a", "target", "a.c", 0),
            call("a", "target", "a.c", 0),
        ],
    );

    let stats = catalog.stats();
    assert_eq!(stats.relations, 1);
    assert_eq!(stats.relation_call_site_refs, 3);
    assert_eq!(
        relations(&catalog, RelationDirection::Outgoing, "a").len(),
        1
    );
    assert_eq!(
        relations(&catalog, RelationDirection::Incoming, "b").len(),
        1
    );

    let page = catalog.relation_page(RelationDirection::Outgoing, "a", 0, 1, 2);
    assert_eq!(page.total, 1);
    assert!(page.site_limited);
    assert_eq!(page.relations.len(), 1);
    assert_eq!(page.relations[0].call_sites.len(), 2);
}

#[test]
fn high_ambiguity_expansion_stays_compact_and_materializes_only_the_requested_page() {
    const CANDIDATES: usize = 64;
    const CALLS: usize = 32;
    let mut anchors = vec![anchor(
        "caller",
        "caller",
        "caller.c",
        AnchorRole::Definition,
        0,
    )];
    for index in 0..CANDIDATES {
        anchors.push(anchor(
            &format!("target-{index}"),
            "shared",
            &format!("target-{index}.c"),
            AnchorRole::Definition,
            0,
        ));
    }
    let calls = (0..CALLS)
        .map(|index| {
            let mut call = call("caller", "shared", "caller.c", 0);
            call.site_fingerprint = format!("site-{index}");
            call
        })
        .collect();

    let catalog = RelationQueryIndex::build(anchors, calls);
    let stats = catalog.stats();
    assert_eq!(stats.relations, CANDIDATES * CALLS);
    assert_eq!(stats.relation_call_site_refs, CANDIDATES * CALLS);

    let page = catalog.relation_page(RelationDirection::Outgoing, "caller", 0, 20, 1);
    assert_eq!(page.total, CANDIDATES * CALLS);
    assert_eq!(page.relations.len(), 20);
    assert!(page
        .relations
        .iter()
        .all(|relation| relation.call_sites.len() == 1));
}

#[test]
#[ignore = "diagnostic scale benchmark; run explicitly in release mode"]
fn benchmark_large_fan_in_query_index_and_cached_page() {
    const CALLERS: usize = 5_000;
    let mut anchors = Vec::with_capacity(CALLERS + 1);
    let mut calls = Vec::with_capacity(CALLERS);
    anchors.push(anchor(
        "target",
        "shared_target",
        "target.c",
        AnchorRole::Definition,
        0,
    ));
    for index in 0..CALLERS {
        let key = format!("caller-{index}");
        let path = format!("src/caller-{index}.c");
        anchors.push(anchor(
            &key,
            &format!("caller_{index}"),
            &path,
            AnchorRole::Definition,
            0,
        ));
        calls.push(call(&key, "shared_target", &path, 0));
    }
    let build_started = std::time::Instant::now();
    let catalog = RelationQueryIndex::build(anchors, calls);
    let build_ms = build_started.elapsed().as_millis();
    let query_started = std::time::Instant::now();
    let incoming = catalog.relation_page(RelationDirection::Incoming, "target", 0, 200, 200);
    let query_us = query_started.elapsed().as_micros();
    let stats = catalog.stats();
    eprintln!(
        "call_relation_benchmark callers={CALLERS} relations={} refs={} build_ms={build_ms} paged_incoming_us={query_us}",
        stats.relations,
        stats.relation_call_site_refs,
    );
    assert_eq!(incoming.total, CALLERS);
    assert_eq!(incoming.relations.len(), 200);
}
