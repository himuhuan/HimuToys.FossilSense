use super::relation_facts::*;
use super::relation_references::*;
use super::relation_types::*;
use super::*;
use crate::query;
use crate::semantic_model::{relations::*, EntityIdentity};

fn indexed(source: &str, check: impl FnOnce(&RelationQueryContext<'_>, &FileSemanticIndex)) {
    indexed_sources(&[("main.cpp", source)], check);
}
fn indexed_sources(
    sources: &[(&str, &str)],
    check: impl FnOnce(&RelationQueryContext<'_>, &FileSemanticIndex),
) {
    let temp = tempfile::tempdir().unwrap();
    for (path, source) in sources {
        let path = temp.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }
    let db = temp.path().join("index.sqlite");
    crate::indexer::index_workspace(
        temp.path(),
        crate::indexer::IndexOptions {
            db_path: Some(db.clone()),
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .unwrap();
    let context = RelationQueryContext {
        read: Some(Arc::new(DeclarationReadContext::from_handle(Arc::new(
            CallReadHandle::capture(db).unwrap(),
        )))),
        overlays: Arc::new(CandidateOverlaySnapshot::default()),
        reach: None,
        family: SemanticFamily::CFamily,
    };
    check(
        &context,
        &crate::parser::parse(Path::new(sources[0].0), sources[0].1),
    );
}

#[test]
fn relation_foundation_include_strength_and_overlay_reverse_are_preserved() {
    use crate::includes::ResolutionKind;
    indexed_sources(
        &[
            (
                "main.cpp",
                "#include \"known.h\"\n#include \"config.h\"\n#include \"missing.h\"\n",
            ),
            ("known.h", "int known;"),
            ("a/config.h", "int a;"),
            ("b/config.h", "int b;"),
        ],
        |context, _| {
            let page = context
                .includes(
                    "main.cpp",
                    false,
                    &RelationCursor::default(),
                    &RelationControl::default(),
                )
                .unwrap();
            assert_eq!(page.edges.len(), 3);
            assert!(page.coverage.partial);
            assert_eq!(
                page.edges
                    .iter()
                    .filter(|e| e.resolution == ResolutionKind::SuffixMatch)
                    .count(),
                2
            );
            let reverse = context
                .includes(
                    "a/config.h",
                    true,
                    &RelationCursor::default(),
                    &RelationControl::default(),
                )
                .unwrap();
            assert_eq!(reverse.edges.len(), 1);
            assert!(
                reverse.coverage.partial,
                "a suffix candidate is not a complete strong relation"
            );
            let mut overlay = CandidateOverlaySnapshot::new(
                2,
                vec![FileCandidateOverlay::from_index(
                    "main.cpp".into(),
                    &crate::parser::parse(Path::new("main.cpp"), "#include \"known.h\"\n"),
                )],
            );
            overlay.refresh_reach_graph(
                None,
                Some(Arc::new(IncludePathIndex::build(
                    ["main.cpp", "known.h", "a/config.h", "b/config.h"]
                        .into_iter()
                        .map(|p| (p.to_owned(), true)),
                ))),
                &[],
            );
            let dirty = RelationQueryContext {
                read: context.read.clone(),
                overlays: Arc::new(overlay),
                reach: None,
                family: SemanticFamily::CFamily,
            };
            assert!(dirty
                .includes(
                    "a/config.h",
                    true,
                    &RelationCursor::default(),
                    &RelationControl::default()
                )
                .unwrap()
                .edges
                .is_empty());
            let live = dirty
                .includes(
                    "known.h",
                    true,
                    &RelationCursor::default(),
                    &RelationControl::default(),
                )
                .unwrap();
            assert_eq!(live.edges.len(), 1);
            assert_ne!(live.edges[0].resolution, ResolutionKind::SuffixMatch);
        },
    );
}

#[test]
fn relation_foundation_typed_handles_reject_stale_members_and_partial_bases() {
    indexed("struct Base {int field;}; using Alias=Base;\nstruct D: Base, Missing<int> {};\nstruct Cycle: Cycle {};\n",|context,_| {
        let bundle=context.type_of("main.cpp",&receiver("a","Alias"),&RelationControl::default()).unwrap().unwrap();
        assert_eq!(bundle.aliases.candidates.len(),1);
        let (mut members,_)=context.members("main.cpp",&receiver("a","Alias"),"field",&RelationControl::default()).unwrap();
        assert_eq!(members.len(),1);
        members[0].generation=crate::call_model::SemanticGeneration(u64::MAX);
        assert!(!context.valid(&context.target(RelationTarget::Member(members.remove(0)))));
        let mut d=context.service("main.cpp").type_candidates("D").unwrap().records.candidates.remove(0);
        let bases=context.inheritance(&d,false,&RelationCursor::default(),&RelationControl::default()).unwrap();
        assert!(bases.edges.iter().any(|e|e.base.is_some()));
        assert!(bases.edges.iter().any(|e|e.base.is_none()));
        assert!(bases.coverage.partial);
        d.declaration_hash=[0;32];
        assert!(context.inheritance(&d,false,&RelationCursor::default(),&RelationControl::default()).unwrap().coverage.stale);
        let (members,coverage)=context.members("main.cpp",&receiver("c","Cycle"),"missing",&RelationControl::default()).unwrap();
        assert!(members.is_empty()); assert!(coverage.cycle && coverage.partial);
    });
}

#[test]
fn relation_foundation_forward_internal_callable_does_not_read_another_file() {
    indexed_sources(
        &[
            ("a.cpp", "int x; static void f(){x++;}"),
            ("b.cpp", "int y; static void f(){y++;}"),
        ],
        |context, index| {
            let fact = index.declarations.iter().find(|d| d.name == "f").unwrap();
            let target = context.target(RelationTarget::Entity(EntityIdentity::from_declaration(
                fact,
            )));
            let page = context
                .relation_references(
                    &target,
                    false,
                    &RelationCursor::default(),
                    &RelationControl::default(),
                )
                .unwrap();
            assert_eq!(page.references.len(), 1);
            assert_eq!(page.references[0].path, "a.cpp");
            assert_eq!(page.references[0].site.spelling, "x");
        },
    );
}
fn receiver(name: &str, ty: &str) -> ReceiverFact {
    ReceiverFact {
        spelling: name.into(),
        type_name: Some(ty.into()),
        tag_domain: false,
        chain: Vec::new(),
        object_anchor: None,
        object_scope: None,
    }
}

#[test]
fn relation_foundation_type_of_c_tag_uses_tag_domain() {
    indexed_sources(
        &[("main.c", "struct A{int x;}; typedef int A; struct A *p;")],
        |context, _| {
            let mut input = receiver("p", "struct A *");
            input.tag_domain = true;
            let bundle = context
                .type_of("main.c", &input, &RelationControl::default())
                .unwrap()
                .unwrap();
            assert_eq!(bundle.records.candidates.len(), 1);
            assert!(bundle.aliases.candidates.is_empty());
        },
    );
}

#[test]
fn relation_foundation_type_of_cpp_record_uses_ordinary_type_lookup() {
    indexed("struct A{int x;}; A a;", |context, _| {
        let bundle = context
            .type_of("main.cpp", &receiver("a", "A"), &RelationControl::default())
            .unwrap()
            .unwrap();
        assert_eq!(bundle.records.candidates.len(), 1);
    });
}

#[test]
fn relation_foundation_persisted_fields_exclude_other_owners_and_keep_unknown_sites() {
    indexed("struct A { int state; };\nstruct B { int state; };\nvoid f(A a,B b) { a.state++; b.state = 2; unknown.state = 3; }\n",|context,_| {
        let (members,_)=context.members("main.cpp",&receiver("a","A"),"state",&RelationControl::default()).unwrap();
        assert_eq!(members.len(),1);
        let target=context.target(RelationTarget::Member(members[0].clone()));
        let page=context.relation_references(&target,true,&RelationCursor::default(),&RelationControl::default()).unwrap();
        let bound:Vec<_>=page.references.iter().filter(|r|!r.targets.is_empty()).collect();
        assert_eq!(bound.len(),1);
        assert_eq!(bound[0].site.role,ReferenceRole::ReadWrite);
        assert!(page.references.iter().any(|r|r.state==BindingState::Unresolved));
        assert!(page.coverage.partial);
    });
}

#[test]
fn relation_foundation_global_reference_ignores_shadowed_locals() {
    indexed(
        "int value;\nvoid f() { value += 2; }\nvoid g(int value) { value++; }\n",
        |context, index| {
            let fact = index
                .declarations
                .iter()
                .find(|d| d.name == "value")
                .unwrap();
            let target = context.target(RelationTarget::Entity(EntityIdentity::from_declaration(
                fact,
            )));
            let page = context
                .relation_references(
                    &target,
                    true,
                    &RelationCursor::default(),
                    &RelationControl::default(),
                )
                .unwrap();
            assert_eq!(page.references.len(), 1);
            assert_eq!(page.references[0].site.role, ReferenceRole::ReadWrite);
            assert_eq!(page.references[0].site.source.range.start.line, 1);
        },
    );
}

#[test]
fn relation_foundation_local_references_bind_document_version_and_scope() {
    let source = "void f(int x) { x++; { int x=1; x+=2; } x=3; }";
    indexed(source, |context, index| {
        let byte = source.find("x)").unwrap();
        let query::BindingResolution::Resolved(binding) =
            query::resolve_local_cursor(index, index.cursor.at(byte).unwrap(), "x", byte, 7)
        else {
            panic!("parameter must bind")
        };
        let target = context.target(RelationTarget::Local {
            path: "main.cpp".into(),
            binding,
            source_fingerprint: index.source_fingerprint,
        });
        let page = context.local_references(
            &target,
            "main.cpp",
            7,
            index,
            0,
            &RelationControl::default(),
        );
        assert_eq!(page.references.len(), 2);
        assert_eq!(page.references[0].site.role, ReferenceRole::ReadWrite);
        assert_eq!(page.references[1].site.role, ReferenceRole::Write);
        assert!(
            context
                .local_references(
                    &target,
                    "main.cpp",
                    8,
                    index,
                    0,
                    &RelationControl::default()
                )
                .coverage
                .stale
        );
    });
}

#[test]
fn relation_foundation_explicit_bases_and_aliases_are_bidirectional() {
    indexed("struct Base { int run(){return 1;} };\nusing Alias=Base;\nstruct Derived : public virtual Alias {};\n",|context,index| {
        assert_eq!(index.relations.explicit_bases.len(),1);
        assert!(index.relations.explicit_bases[0].is_virtual);
        let derived=context.service("main.cpp").type_candidates("Derived").unwrap().records.candidates.remove(0);
        let base=context.service("main.cpp").type_candidates("Base").unwrap().records.candidates.remove(0);
        let page=context.inheritance(&derived,false,&RelationCursor::default(),&RelationControl::default()).unwrap();
        assert_eq!(page.edges.len(),1);
        assert_eq!(page.edges[0].base.as_ref().expect("resolved alias base").identity,base.identity);
        let reverse=context.inheritance(&base,true,&RelationCursor::default(),&RelationControl::default()).unwrap();
        assert_eq!(reverse.edges.len(),1);
        let (members,_)=context.members("main.cpp",&receiver("d","Derived"),"run",&RelationControl::default()).unwrap();
        assert_eq!(members.len(),1);
    });
}

#[test]
fn relation_foundation_noise_pages_advance_and_cancellation_is_explicit() {
    indexed("struct A {int state;}; struct B {int state;};\nvoid f(B b,A a) { b.state++; b.state++; a.state++; }\n",|context,_| {
        let (members,_)=context.members("main.cpp",&receiver("a","A"),"state",&RelationControl::default()).unwrap();
        let target=context.target(RelationTarget::Member(members[0].clone()));
        let control=RelationControl{scan_limit:1,cancellation:None};
        let first=context.relation_references(&target,true,&RelationCursor::default(),&control).unwrap();
        assert!(first.references.is_empty());
        let next=first.coverage.next.expect("noise scan must advance");
        let second=context.relation_references(&target,true,&next,&control).unwrap();
        assert!(second.references.is_empty());
        let third=context.relation_references(&target,true,&second.coverage.next.unwrap(),&control).unwrap();
        assert_eq!(third.references.len(),1);
        struct Cancel; impl query::CompletionQueryCancellation for Cancel {fn is_cancelled(&self)->bool{true}}
        let cancelled=context.relation_references(&target,true,&RelationCursor::default(),&RelationControl{scan_limit:256,cancellation:Some(&Cancel)}).unwrap();
        assert!(cancelled.coverage.cancelled);assert!(cancelled.references.is_empty());
    });
}

#[test]
fn relation_foundation_overlay_replaces_all_relation_groups() {
    indexed(
        "struct Base {}; struct D : Base {}; int x; void f(){x++;}\n",
        |context, _| {
            let source = "struct Base {}; struct D {}; int x; void f(){}\n";
            let parsed = crate::parser::parse(Path::new("main.cpp"), source);
            let dirty = RelationQueryContext {
                read: context.read.clone(),
                overlays: Arc::new(CandidateOverlaySnapshot::new(
                    2,
                    vec![FileCandidateOverlay::from_index("main.cpp".into(), &parsed)],
                )),
                reach: None,
                family: SemanticFamily::CFamily,
            };
            let d = dirty
                .service("main.cpp")
                .type_candidates("D")
                .unwrap()
                .records
                .candidates
                .remove(0);
            assert!(dirty
                .inheritance(
                    &d,
                    false,
                    &RelationCursor::default(),
                    &RelationControl::default()
                )
                .unwrap()
                .edges
                .is_empty());
            let fact = parsed.declarations.iter().find(|f| f.name == "x").unwrap();
            let target = dirty.target(RelationTarget::Entity(EntityIdentity::from_declaration(
                fact,
            )));
            assert!(dirty
                .relation_references(
                    &target,
                    true,
                    &RelationCursor::default(),
                    &RelationControl::default()
                )
                .unwrap()
                .references
                .is_empty());
            assert!(!context.valid(&target));
        },
    );
}

#[test]
fn relation_foundation_review_unsupported_receiver_never_binds_global() {
    indexed(
        "int state; struct A {int state;}; void f(A *a){a[0].state++;}\n",
        |context, index| {
            let fact = index
                .declarations
                .iter()
                .find(|f| f.name == "state")
                .unwrap();
            let target = context.target(RelationTarget::Entity(EntityIdentity::from_declaration(
                fact,
            )));
            let page = context
                .relation_references(
                    &target,
                    true,
                    &RelationCursor::default(),
                    &RelationControl::default(),
                )
                .unwrap();
            assert!(page.references.iter().all(|r| r.targets.is_empty()));
            assert!(page.coverage.partial);
        },
    );
}

#[test]
fn relation_foundation_review_qualified_value_excludes_global() {
    indexed(
        "namespace A {extern int value;} int value; void f(){A::value++;}\n",
        |context, index| {
            let fact = index
                .declarations
                .iter()
                .find(|f| f.name == "value" && f.owner.is_none())
                .unwrap();
            let target = context.target(RelationTarget::Entity(EntityIdentity::from_declaration(
                fact,
            )));
            let page = context
                .relation_references(
                    &target,
                    true,
                    &RelationCursor::default(),
                    &RelationControl::default(),
                )
                .unwrap();
            assert!(page.references.is_empty());
        },
    );
}

#[test]
fn relation_foundation_review_forward_callable_reads_only_its_body() {
    indexed(
        "int x; int y; void f(){x++;} void g(){y++;}\n",
        |context, index| {
            let fact = index.declarations.iter().find(|f| f.name == "f").unwrap();
            let target = context.target(RelationTarget::Entity(EntityIdentity::from_declaration(
                fact,
            )));
            let page = context
                .relation_references(
                    &target,
                    false,
                    &RelationCursor::default(),
                    &RelationControl::default(),
                )
                .unwrap();
            assert_eq!(page.references.len(), 1);
            assert_eq!(page.references[0].site.spelling, "x");
        },
    );
}

#[test]
fn relation_foundation_review_unavailable_empty_overlay_is_not_complete() {
    indexed("int x;", |context, _| {
        let dirty = RelationQueryContext {
            read: context.read.clone(),
            overlays: Arc::new(CandidateOverlaySnapshot::new(
                3,
                vec![FileCandidateOverlay::tombstone(
                    "main.cpp".into(),
                    Arc::from("broken"),
                )],
            )),
            reach: None,
            family: SemanticFamily::CFamily,
        };
        let target = dirty.target(RelationTarget::File("main.cpp".into()));
        let page = dirty
            .relation_references(
                &target,
                false,
                &RelationCursor::default(),
                &RelationControl::default(),
            )
            .unwrap();
        assert!(page.coverage.partial || page.coverage.unavailable);
    });
}
