use std::fs;
use std::path::Path;

fn read(path: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(root.join(path)).unwrap_or_else(|err| panic!("failed to read {path}: {err}"))
}

fn production_source(path: &str) -> String {
    let source = read(path);
    if let Some((production, _tests)) = source.split_once("#[cfg(test)]\nmod tests") {
        production.to_string()
    } else {
        source
    }
}

fn assert_absent(path: &str, forbidden: &[&str]) {
    let source = production_source(path);
    for pattern in forbidden {
        assert!(
            !source.contains(pattern),
            "{path} should consume parser fact projections, found `{pattern}`"
        );
    }
}

fn assert_present(path: &str, required: &[&str]) {
    let source = production_source(path);
    for pattern in required {
        assert!(
            source.contains(pattern),
            "{path} should make parser fact projection usage explicit, missing `{pattern}`"
        );
    }
}

#[test]
fn index_storage_paths_consume_persistent_parser_facts() {
    assert_absent("src/indexer/parse_pipeline.rs", &["index.symbols"]);
    assert_present("src/indexer/parse_pipeline.rs", &["persistent_facts()"]);

    assert_absent(
        "src/store/writes.rs",
        &[
            "&index.symbols",
            "&index.includes",
            "&index.records",
            "&index.members",
            "&index.aliases",
        ],
    );
    assert_present("src/store/writes.rs", &["persistent_facts()"]);
}

#[test]
fn live_parser_consumers_consume_request_facts_and_availability() {
    assert_absent(
        "src/server/semantic_tokens.rs",
        &["index.occurrences", "index.local_bindings"],
    );
    assert_present(
        "src/server/semantic_tokens.rs",
        &[
            "request_facts()",
            "fact_availability",
            "FactGroup::Occurrences",
            "FactGroup::LocalBindings",
        ],
    );

    assert_absent(
        "src/references.rs",
        &["parser::parse(abs_path, &source).occurrences"],
    );
    assert_present(
        "src/references.rs",
        &[
            "ParseFacts::COLOR_REF",
            "request_facts()",
            "fact_availability",
            "FactGroup::Occurrences",
        ],
    );

    assert_absent(
        "src/server/member_completion.rs",
        &["index.local_declarations"],
    );
    assert_present(
        "src/server/member_completion.rs",
        &[
            "request_facts()",
            "fact_availability",
            "FactGroup::LocalDeclarations",
        ],
    );

    assert_absent(
        "src/completion/ordinary_service.rs",
        &["index.local_bindings"],
    );
    assert_present(
        "src/completion/ordinary_service.rs",
        &[
            "request_facts()",
            "fact_availability",
            "FactGroup::LocalBindings",
        ],
    );

    assert_absent(
        "src/query/current_file_overlay.rs",
        &[
            "&index.symbols",
            "&index.aliases",
            "&index.records",
            "&index.occurrences",
            "index.occurrences.is_empty()",
        ],
    );
    assert_present(
        "src/query/current_file_overlay.rs",
        &[
            "persistent_facts()",
            "request_facts()",
            "fact_availability",
            "FactGroup::Occurrences",
        ],
    );

    assert_absent(
        "src/server/language_server.rs",
        &["index.symbols", "index.aliases", "index.records"],
    );
    assert_present("src/server/language_server.rs", &["persistent_facts()"]);
}
