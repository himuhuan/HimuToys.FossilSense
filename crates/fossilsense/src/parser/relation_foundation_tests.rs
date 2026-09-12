use super::*;
use crate::semantic_model::relations::*;

#[test]
fn relation_foundation_large_initializer_stops_at_fact_checkpoints() {
    let source = format!(
        "struct Ops{{int (*send)();}}; int tx(); struct Ops ops={{{}}};",
        ".send=tx,".repeat(20_000)
    );
    let result = parse_thread_local_with_selection_budget(
        Path::new("large.c"),
        &source,
        LanguageSelection::explicit(SourceLanguage::C),
        ParseFacts::INDEX,
        &AtomicBool::new(false),
        32 * 1024,
    );
    let peak = result.expect_err("large initializer must exceed the requested fact budget");
    assert!(
        peak < 2 * 1024 * 1024,
        "one initializer bypassed streaming checkpoints: {peak}"
    );
}

#[test]
fn relation_foundation_fact_selection_roles_and_language_boundaries() {
    let source="int tx(){return 1;}\nstruct Ops { int (*send)(); };\nstatic Ops ops={.send=tx};\n#define SEND() tx()\nvoid f(){int (*fp)()=&tx; fp=tx; fp(); ops.send();}\n";
    let parsed = parse(Path::new("main.cpp"), source);
    assert!(parsed
        .relations
        .binding_sites
        .iter()
        .any(|f| f.spelling == "fp" && f.role == ReferenceRole::Call));
    assert!(parsed
        .relations
        .indirect_assignments
        .iter()
        .any(|f| f.member.as_deref() == Some("send")
            && f.target_name.as_deref() == Some("tx")
            && f.slot.object_anchor.is_some()));
    assert_eq!(parsed.relations.macros.len(), 1);
    assert_eq!(parsed.relations.macros[0].direct_calls, vec!["tx"]);
    let skipped = parse_with_handle(Path::new("main.cpp"), source, None, ParseFacts::COMPLETION);
    assert_eq!(
        skipped.fact_availability(FactGroup::BindingSites),
        FactAvailability::NotRequested
    );
    assert!(skipped.relations.binding_sites.is_empty());
    for (path, source) in [
        ("plain.c", "struct A {int x;};"),
        (
            "main.go",
            "package main\ntype A struct{}\ntype B struct { A }\n",
        ),
    ] {
        assert!(parse(Path::new(path), source)
            .relations
            .explicit_bases
            .is_empty());
    }
}

#[test]
fn relation_foundation_macro_paste_and_parameter_targets_are_not_guessed() {
    let parsed=parse(Path::new("main.c"),"#define PASTE(x) tx##x()\n#define STRING(x) #x\n#define FORWARD(fn) fn()\n#undef FORWARD\n");
    assert_eq!(parsed.relations.macros.len(), 4);
    assert!(parsed
        .relations
        .macros
        .iter()
        .all(|f| f.direct_calls.is_empty()));
    assert!(parsed.relations.macros.last().unwrap().undef);
}

#[test]
fn relation_foundation_declarator_names_and_macro_member_tokens_are_not_references() {
    let parsed = parse(
        Path::new("main.c"),
        "int x,y;\n#define CALL(obj) obj . run()\n",
    );
    assert!(parsed
        .relations
        .binding_sites
        .iter()
        .all(|s| s.spelling != "x" && s.spelling != "y"));
    assert!(parsed.relations.macros[0].direct_calls.is_empty());
}
