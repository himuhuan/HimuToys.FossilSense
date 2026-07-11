use super::*;

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

#[test]
fn catalog_groups_variants_and_resolves_one_hop_both_directions() {
    let catalog = RelationCatalog::build(
        vec![
            anchor("a", "caller", "a.c", AnchorRole::Definition, 0),
            anchor("b", "target", "b.h", AnchorRole::Declaration, 1),
            anchor("b", "target", "b.c", AnchorRole::Definition, 1),
        ],
        vec![call("a", "target", "a.c", 1)],
    );
    assert_eq!(catalog.len(), 2);
    assert_eq!(catalog.entity("b").unwrap().variants.len(), 2);
    let outgoing = catalog.outgoing("a");
    assert_eq!(outgoing.len(), 1);
    assert_eq!(outgoing[0].callee.as_ref().unwrap().entity_key, "b");
    assert_eq!(catalog.incoming("b")[0].caller.entity_key, "a");
}

#[test]
fn incompatible_arity_is_unresolved_and_duplicate_names_are_ambiguous() {
    let catalog = RelationCatalog::build(
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
    let outgoing = catalog.outgoing("a");
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
    let catalog = RelationCatalog::build(
        vec![caller, target],
        vec![call("a", "target", "src/a.c", 0)],
    );
    let outgoing = catalog.outgoing("a");
    assert_eq!(outgoing.len(), 1);
    assert_eq!(outgoing[0].callee.as_ref().unwrap().entity_key, "b");
    assert!(outgoing[0]
        .evidence
        .supports
        .contains(&EvidenceCode::InternalLinkage));
}

#[test]
fn stale_entity_key_remaps_by_path_signature_and_nearest_anchor() {
    let catalog = RelationCatalog::build(
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
    let catalog = RelationCatalog::build(vec![header, definition, local], vec![]);
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
#[ignore = "diagnostic scale benchmark; run explicitly in release mode"]
fn benchmark_large_fan_in_catalog_and_cached_query() {
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
    let catalog = RelationCatalog::build(anchors, calls);
    let build_ms = build_started.elapsed().as_millis();
    let query_started = std::time::Instant::now();
    let incoming = catalog.incoming("target");
    let query_us = query_started.elapsed().as_micros();
    eprintln!(
        "call_relation_benchmark callers={CALLERS} build_ms={build_ms} cached_incoming_us={query_us}"
    );
    assert_eq!(incoming.len(), CALLERS);
}
