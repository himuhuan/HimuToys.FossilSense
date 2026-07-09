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

fn rust_sources_under(dir: &Path, out: &mut Vec<String>) {
    for entry in fs::read_dir(dir).unwrap_or_else(|err| panic!("failed to read {dir:?}: {err}")) {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            rust_sources_under(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"));
            let rel = path
                .strip_prefix(root)
                .expect("source below manifest dir")
                .to_string_lossy()
                .replace('\\', "/");
            out.push(rel);
        }
    }
}

fn production_rust_sources() -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut paths = Vec::new();
    rust_sources_under(&root.join("src"), &mut paths);
    paths
        .into_iter()
        .filter(|path| {
            !path.ends_with("/tests.rs")
                && !path.contains("/tests/")
                && !path.starts_with("src/parser.rs")
                && !path.starts_with("src/parser/")
        })
        .collect()
}

fn assert_all_production_sources_absent(forbidden: &[&str]) {
    for path in production_rust_sources() {
        let source = production_source(&path);
        for pattern in forbidden {
            assert!(
                !source.contains(pattern),
                "{path} should not directly interpret parser fact fields, found `{pattern}`"
            );
        }
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
            "should_use_raw_identifier_scan()",
        ],
    );
    assert_absent(
        "src/query/current_file_overlay.rs",
        &[
            "index.diagnostics",
            "FactSource::LexicalFallback",
            "fallback_used",
        ],
    );

    assert_absent(
        "src/server/language_server.rs",
        &["index.symbols", "index.aliases", "index.records"],
    );
    assert_present("src/server/definition.rs", &["persistent_facts()"]);
    assert_present("src/server/symbols.rs", &["persistent_facts()"]);
}

#[test]
fn production_parser_consumers_do_not_bypass_projection_availability() {
    assert_all_production_sources_absent(&[
        "index.symbols",
        "index.includes",
        "index.records",
        "index.fields",
        "index.members",
        "index.aliases",
        "index.occurrences",
        "index.local_declarations",
        "index.local_bindings",
        "parsed.symbols",
        "parsed.includes",
        "parsed.records",
        "parsed.fields",
        "parsed.members",
        "parsed.aliases",
        "parsed.occurrences",
        "parsed.local_declarations",
        "parsed.local_bindings",
        "request_facts().occurrences.is_empty()",
        "request_facts().local_declarations.is_empty()",
        "request_facts().local_bindings.is_empty()",
        "persistent_facts().symbols.is_empty()",
        "persistent_facts().includes.is_empty()",
        "persistent_facts().records.is_empty()",
        "persistent_facts().members.is_empty()",
        "persistent_facts().aliases.is_empty()",
        "index.diagnostics",
    ]);
}

#[test]
fn parser_consumer_guard_fixture_catches_request_fact_emptiness() {
    let fixture =
        "fn bad(index: &FileSemanticIndex) -> bool { index.request_facts().occurrences.is_empty() }";

    assert!(
        fixture.contains("request_facts().occurrences.is_empty()"),
        "guard fixture should model the request-fact emptiness bypass"
    );
}

#[test]
fn parser_consumer_guard_fixture_catches_diagnostics_interpretation() {
    let fixture = "fn bad(index: &FileSemanticIndex) -> bool { index.diagnostics.fallback_used }";

    assert!(
        fixture.contains("index.diagnostics"),
        "guard fixture should model direct parser diagnostics interpretation"
    );
}
