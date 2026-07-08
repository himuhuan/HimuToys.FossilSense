use std::fs;
use std::path::Path;

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
}

fn read_workspace_file(path: &str) -> String {
    let path = repo_root().join(path);
    fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

fn assert_contains(source: &str, path: &str, needle: &str) {
    assert!(source.contains(needle), "{path} should document `{needle}`");
}

#[test]
fn architecture_docs_record_final_fact_consumption_contracts() {
    let docs = read_workspace_file("docs/arch.md");

    for needle in [
        "Unified fact boundary, separate domain pipelines",
        "symbol/query pipeline consumes parser/store facts through projections, read views, typed rows, and resolver-backed candidate APIs",
        "reference pipeline stays whole-word text hits plus syntactic roles from request facts",
        "reference discovery keeps the historical path/line/column truncation cap before role presentation",
        "legacy broad IndexStore query wrappers are test-only parity helpers",
    ] {
        assert_contains(&docs, "docs/arch.md", needle);
    }
}

#[test]
fn claude_records_final_fact_consumption_contracts() {
    let docs = read_workspace_file("CLAUDE.md");

    for needle in [
        "Unified fact boundary, separate domain pipelines",
        "Parser facts must be consumed through persistent_facts(), request_facts(), fact_availability(...), or narrow helpers",
        "Durable reads must use store::views, typed rows, or domain loaders outside the store boundary",
        "References remain text hits annotated with syntactic roles; they do not use resolver ranking or ScopeTier",
        "Reference discovery keeps the historical path/line/column truncation cap; standard and grouped presentations sort the retained hits by role/path/line/column",
        "Legacy broad IndexStore query wrappers are restricted to #[cfg(test)] parity helpers",
    ] {
        assert_contains(&docs, "CLAUDE.md", needle);
    }
}
