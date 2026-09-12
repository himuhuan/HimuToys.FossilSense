#![cfg(test)]
//! Contract tests through the persisted call service, shared by both directions.
use super::*;
use crate::call_model::CallableKind;
use crate::indexer::{index_workspace, IndexOptions};
use std::sync::Arc;

fn with_source(source: &str, check: impl FnOnce(&CallRelationService<'_>)) {
    with_sources(&[("main.cpp", source)], check);
}

#[test]
fn relation_foundation_unknown_callback_member_does_not_invent_owner() {
    with_source("int tx(){return 1;}\nstruct Ops {};\nOps ops;\nint caller(){ops.send=tx; return ops.send();}\n",|service|{
        let outgoing=page(service,3,RelationDirection::Outgoing);
        assert!(outgoing.relations.iter().all(|r|r.callee.as_ref().is_none_or(|c|c.name!="tx")),"{outgoing:?}");
        assert!(outgoing.relations.iter().any(|r|r.callee.is_none()));
    });
}

#[test]
fn relation_foundation_pointer_receiver_reassignment_never_proves_object_slot() {
    with_source("int tx(){return 1;} int other(){return 2;}\nstruct Ops{int (*send)();};\nint caller(){Ops first{},second{.send=other}; Ops *p=&first; p->send=tx; p=&second; return p->send();}\n",|service|{
        let outgoing=page(service,2,RelationDirection::Outgoing);
        assert!(outgoing.relations.iter().all(|r|r.callee.is_none()),"{outgoing:?}");
        assert!(outgoing.candidate_limited || outgoing.relations.iter().any(|r|r.callee.is_none()));
    });
}

#[test]
fn relation_foundation_parenthesized_function_does_not_expand_same_named_macro() {
    with_source(
        "int RUN(){return 1;}\n#define RUN() tx()\nint caller(){return (RUN)();}\n",
        |service| {
            let outgoing = page(service, 2, RelationDirection::Outgoing);
            assert!(
                outgoing.relations.iter().any(|r| r
                    .callee
                    .as_ref()
                    .is_some_and(|c| c.kind == CallableKind::Function)),
                "{outgoing:?}"
            );
            assert!(outgoing.relations.iter().all(|r| r
                .callee
                .as_ref()
                .is_none_or(|c| c.kind != CallableKind::FunctionLikeMacro)));
        },
    );
}

fn with_sources(sources: &[(&str, &str)], check: impl FnOnce(&CallRelationService<'_>)) {
    let temp = tempfile::tempdir().unwrap();
    for (path, source) in sources {
        std::fs::write(temp.path().join(path), source).unwrap();
    }
    let db = temp.path().join("index.sqlite");
    index_workspace(
        temp.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .unwrap();
    let context =
        DeclarationReadContext::from_handle(Arc::new(CallReadHandle::capture(db).unwrap()));
    check(&CallRelationService::new(&context));
}

#[test]
fn relation_foundation_same_signature_and_offset_do_not_mix_source_receivers() {
    let first="struct A{int run(){return 1;}};\nstruct B{int run(){return 2;}};\nvoid caller(){A a; a.run();}\n";
    let second = first.replace("A a; a.run()", "B b; b.run()");
    with_sources(&[("a.cpp", first), ("b.cpp", &second)], |service| {
        let outgoing = service
            .query_at(
                "b.cpp",
                SourcePosition {
                    line: 2,
                    character: 6,
                },
                RelationDirection::Outgoing,
                0,
                100,
                100,
            )
            .unwrap()
            .2;
        assert!(
            outgoing.relations.iter().any(|r| r
                .callee
                .as_ref()
                .is_some_and(|c| c.qualified_name == "B::run")),
            "{outgoing:?}"
        );
        assert!(outgoing.relations.iter().all(|r| r
            .callee
            .as_ref()
            .is_none_or(|c| c.qualified_name != "A::run")));
    });
}

fn page(
    service: &CallRelationService<'_>,
    line: u32,
    direction: RelationDirection,
) -> RelationPage {
    service
        .query_at(
            "main.cpp",
            SourcePosition { line, character: 5 },
            direction,
            0,
            100,
            100,
        )
        .unwrap()
        .2
}

#[test]
fn relation_foundation_member_call_keeps_owner_and_reverse_identity() {
    with_source(
        "struct A { int run() { return 1; } };\nstruct B { int run() { return 2; } };\nint caller(A a) { return a.run(); }\n",
        |service| {
            let outgoing = page(service, 2, RelationDirection::Outgoing);
            assert_eq!(outgoing.relations.len(), 1);
            let target = outgoing.relations[0].callee.as_ref().expect("typed member target");
            assert!(target.qualified_name.contains("A"), "{target:?}");
            let (_, _, incoming) = service.query_key(&target.entity_key, RelationDirection::Incoming, 0, 100, 100).unwrap();
            assert_eq!(incoming.relations.len(), 1);
            assert_eq!(incoming.relations[0].caller.name, "caller");
        },
    );
}

#[test]
fn relation_foundation_direct_function_pointer_is_a_reverse_queryable_candidate() {
    with_source(
        "int tx() { return 1; }\nint other() { return 2; }\nint caller() { int (*fp)() = &tx; return fp(); }\n",
        |service| {
            let outgoing = page(service, 2, RelationDirection::Outgoing);
            assert!(outgoing.relations.iter().any(|r| r.callee.as_ref().is_some_and(|c| c.name == "tx")), "{outgoing:?}");
            assert!(outgoing.relations.iter().all(|r| r.confidence != crate::call_model::RelationConfidence::High));
            assert_eq!(page(service, 0, RelationDirection::Incoming).relations.len(), 1);
            assert!(page(service, 1, RelationDirection::Incoming).relations.is_empty());
        },
    );
}

#[test]
fn relation_foundation_macro_invocation_keeps_navigable_definition() {
    with_source(
        "int tx() { return 1; }\n#define SEND() tx()\nint caller() { return SEND(); }\n",
        |service| {
            let outgoing = page(service, 2, RelationDirection::Outgoing);
            assert!(
                outgoing
                    .relations
                    .iter()
                    .any(|r| r.callee.as_ref().is_some_and(|c| c.name == "SEND")),
                "{outgoing:?}"
            );
        },
    );
}

#[test]
fn relation_foundation_operations_tables_keep_object_identity_and_sources() {
    with_source("int tx(){return 1;}\nint other(){return 2;}\nstruct Ops {int (*send)();}; static Ops first={.send=tx}; static Ops second={.send=other};\nint caller(){return first.send();}\n",|service|{
        let outgoing=page(service,3,RelationDirection::Outgoing);
        let target=outgoing.relations.iter().find(|r|r.callee.as_ref().is_some_and(|c|c.name=="tx")).expect("designated callback target");
        assert!(target.target_sources.len()>=2);
        assert!(outgoing.relations.iter().all(|r|r.callee.as_ref().is_none_or(|c|c.name!="other")));
        assert_eq!(page(service,0,RelationDirection::Incoming).relations.len(),1);
        assert!(page(service,1,RelationDirection::Incoming).relations.is_empty());
    });
}

#[test]
fn relation_foundation_branch_assignments_and_escape_stay_candidates() {
    with_source("int a(){return 1;}\nint b(){return 2;}\nvoid escape(int (**)());\nint caller(int condition){int (*fp)()=&a; if(condition){fp=b;} escape(&fp); return fp();}\n",|service|{
        let outgoing=page(service,3,RelationDirection::Outgoing);
        for name in ["a","b"] {
            let relation=outgoing.relations.iter().find(|r|r.callee.as_ref().is_some_and(|c|c.name==name)).expect("branch assignment candidate");
            assert_ne!(relation.confidence,crate::call_model::RelationConfidence::High);
            assert!(relation.evidence.unknowns.contains(&crate::call_model::EvidenceCode::RuntimeDispatchUnknown));
        }
    });
}

#[test]
fn relation_foundation_static_members_preserve_namespace_free_calls() {
    with_source("namespace N {int run(){return 0;}}\nstruct A {static int run(){return 1;}};\nstruct B {static int run(){return 2;}};\nint caller(){return N::run()+A::run();}\n",|service|{
        let outgoing=page(service,3,RelationDirection::Outgoing);
        for name in ["N::run","A::run"] {assert!(outgoing.relations.iter().any(|r|r.callee.as_ref().is_some_and(|c|c.qualified_name==name)),"{outgoing:?}");}
        assert!(outgoing.relations.iter().all(|r|r.callee.as_ref().is_none_or(|c|c.qualified_name!="B::run")));
    });
}

#[test]
fn relation_foundation_virtual_member_target_keeps_dispatch_unknown() {
    with_source(
        "struct A {virtual int run(){return 1;}};\nint caller(A *a){return a->run();}\n",
        |service| {
            let outgoing = page(service, 1, RelationDirection::Outgoing);
            let target = outgoing
                .relations
                .iter()
                .find(|r| r.callee.is_some())
                .unwrap();
            assert_ne!(
                target.confidence,
                crate::call_model::RelationConfidence::High
            );
            assert!(target
                .evidence
                .unknowns
                .contains(&crate::call_model::EvidenceCode::RuntimeDispatchUnknown));
        },
    );
}

#[test]
fn relation_foundation_local_callback_alias_cannot_bind_same_named_function() {
    with_source("using Fn=int(*)();\nint a(){return 1;}\nint b(){return 2;}\nint caller(){Fn a=b; Fn fp=a; return fp();}\n",|service|{
        let outgoing=page(service,3,RelationDirection::Outgoing);
        assert!(outgoing.relations.iter().all(|r|r.callee.is_none()),"{outgoing:?}");
    });
}

#[test]
fn relation_foundation_nested_callback_slots_keep_their_complete_path() {
    with_source("int tx(){return 1;}\nint other(){return 2;}\nstruct Ops{int (*send)();}; struct Pair{Ops left;Ops right;}; static Pair ops;\nint caller(){ops.right.send=other; ops.left.send=tx; return ops.right.send();}\n",|service|{
        let outgoing=page(service,3,RelationDirection::Outgoing);
        assert!(outgoing.relations.iter().any(|r|r.callee.as_ref().is_some_and(|c|c.name=="other")),"{outgoing:?}");
        assert!(outgoing.relations.iter().all(|r|r.callee.as_ref().is_none_or(|c|c.name!="tx")));
    });
}

#[test]
fn relation_foundation_cancelled_call_query_returns_cancellation_error() {
    with_source(
        "int tx(){return 1;}\nint caller(){return tx();}\n",
        |service| {
            struct Cancel;
            impl crate::query::CompletionQueryCancellation for Cancel {
                fn is_cancelled(&self) -> bool {
                    true
                }
            }
            let controlled =
                CallRelationService::new(service.read_context).with_cancellation(&Cancel);
            let error = controlled
                .query_at(
                    "main.cpp",
                    SourcePosition {
                        line: 1,
                        character: 5,
                    },
                    RelationDirection::Outgoing,
                    0,
                    100,
                    100,
                )
                .unwrap_err();
            assert!(error.to_string().contains("cancelled"));
        },
    );
}

#[test]
fn relation_foundation_macro_body_candidate_and_undef_are_distinct() {
    with_source("int tx(){return 1;}\n#define SEND() tx()\nint caller(){return SEND();}\n#undef SEND\nint later(){return SEND();}\n",|service|{
        let outgoing=page(service,2,RelationDirection::Outgoing);
        let macro_target=outgoing.relations[0].callee.as_ref().unwrap();
        let body=service.query_key(&macro_target.entity_key,RelationDirection::Outgoing,0,100,100).unwrap().2;
        let candidate=body.relations.iter().find(|r|r.callee.as_ref().is_some_and(|c|c.name=="tx")).unwrap();
        assert!(candidate.evidence.unknowns.contains(&crate::call_model::EvidenceCode::ExpansionNotEvaluated));
        assert!(page(service,4,RelationDirection::Outgoing).relations.iter().all(|r|r.callee.is_none()));
    });
}
