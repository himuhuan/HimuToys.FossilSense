use super::*;
use crate::parser::{
    line_starts, parse_with_language, LocalBindingKind, LocalBindingNamespace, RecoveryOutcome,
};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::Write;

fn fingerprint(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    });
    format!("fnv1a64:{hash:016x}")
}

fn fixture_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/c_frontend")
}

fn observe_manifest(path: &Path) -> Result<Vec<Value>> {
    let bytes = std::fs::read(path).context("read conformance manifest")?;
    anyhow::ensure!(bytes.len() <= 512 * 1024, "manifest exceeds size limit");
    let manifest: Value = serde_json::from_slice(&bytes)?;
    anyhow::ensure!(
        manifest["formatVersion"] == 1,
        "unsupported manifest version"
    );
    let files = manifest["files"].as_array().context("manifest files")?;
    anyhow::ensure!(
        !files.is_empty() && files.len() <= 128,
        "invalid fixture count"
    );
    let root = path.parent().context("manifest directory")?;
    files
        .iter()
        .map(|file| {
            let name = file["path"].as_str().context("fixture path")?;
            anyhow::ensure!(
                Path::new(name)
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_))),
                "fixture paths must stay relative"
            );
            let source = std::fs::read(root.join(name))?;
            anyhow::ensure!(source.len() <= 1024 * 1024, "fixture exceeds source limit");
            let source = std::str::from_utf8(&source)?;
            observe_source(
                name,
                source,
                file["selection"].as_str().unwrap_or("default"),
            )
        })
        .collect()
}

fn group_name(group: FactGroup) -> String {
    serde_json::to_value(group)
        .unwrap()
        .as_str()
        .unwrap()
        .to_string()
}

fn observe_source(name: &str, source: &str, selection: &str) -> Result<Value> {
    let index = match selection {
        "default" => parse(Path::new(name), source),
        "c" => parse_with_language(
            Path::new(name),
            source,
            crate::config::SourceLanguage::C,
            ParseFacts::ALL,
        ),
        "cpp" => parse_with_language(
            Path::new(name),
            source,
            crate::config::SourceLanguage::Cpp,
            ParseFacts::ALL,
        ),
        _ => anyhow::bail!("unsupported fixture language selection"),
    };
    let mut observations: Vec<Value> = index
        .declarations
        .iter()
        .map(|fact| {
            json!({
                "name": fact.name, "kind": fact.declaration_kind, "role": fact.role,
                "owner": fact.owner, "scope": "persistent", "namespace": null, "guard": fact.guard,
                "nameRange": fact.name_range, "declarationRange": fact.declaration_range,
            })
        })
        .collect();
    for member in &index.members {
        let owner = index
            .records
            .iter()
            .find(|record| record.record_key == member.record_key)
            .context("member must reference its parsed record")?;
        let range = super::super::coverage::source_range(
            source,
            &line_starts(source),
            member.start_byte..member.end_byte,
        );
        observations.push(json!({"name": member.name, "kind": member.kind.as_str(),
            "role": "definition", "owner": owner.display_name, "scope": "persistent",
            "namespace": null, "guard": member.guard, "nameRange": range, "declarationRange": null}));
    }
    for binding in &index.local_bindings {
        let owner = index.callable_anchors.iter().find(|anchor| {
            anchor
                .body_range
                .is_some_and(|body| body.start_byte == binding.function_start_byte)
        });
        let end = binding.decl_start_byte + binding.name.len();
        anyhow::ensure!(
            source.get(binding.decl_start_byte..end) == Some(binding.name.as_str()),
            "local binding source range"
        );
        let range = super::super::coverage::source_range(
            source,
            &line_starts(source),
            binding.decl_start_byte..end,
        );
        let kind = match binding.kind {
            LocalBindingKind::Parameter => "parameter",
            LocalBindingKind::LocalVariable => "local_variable",
            LocalBindingKind::LocalConstant => "local_constant",
            LocalBindingKind::LocalType => "local_type",
        };
        let namespace = match binding.namespace {
            LocalBindingNamespace::Ordinary => "ordinary",
            LocalBindingNamespace::Tag => "tag",
        };
        observations.push(
            json!({"name": binding.name, "kind": kind, "role": "definition",
            "owner": owner.map(|anchor| anchor.name.as_str()), "scope": "request_local", "namespace": namespace,
            "guard": owner.and_then(|anchor| anchor.guard.as_deref()), "nameRange": range, "declarationRange": null}),
        );
    }
    sort_observations(&mut observations);
    let mut groups = serde_json::Map::new();
    let mut state = crate::semantic_model::FactCoverage::Complete;
    for group in [
        FactGroup::Declarations,
        FactGroup::Records,
        FactGroup::Fields,
        FactGroup::Members,
        FactGroup::Aliases,
        FactGroup::LocalBindings,
        FactGroup::CallSites,
    ] {
        let coverage = index.fact_coverage(group);
        if coverage == crate::semantic_model::FactCoverage::Unknown
            || (coverage == crate::semantic_model::FactCoverage::Partial
                && state == crate::semantic_model::FactCoverage::Complete)
        {
            state = coverage;
        }
        groups.insert(group_name(group), serde_json::to_value(coverage)?);
    }
    let gaps: Vec<Value> = index
        .diagnostics
        .coverage
        .gaps
        .iter()
        .map(|gap| {
            let groups: Vec<_> = FactGroup::ALL
                .into_iter()
                .filter(|group| gap.groups & group.bit() != 0)
                .map(group_name)
                .collect();
            json!({"range":gap.range,"groups":groups,"reason":gap.reason,"evidence":gap.evidence})
        })
        .collect();
    let mut recovery_rules: Vec<_> = index
        .diagnostics
        .recovery
        .iter()
        .filter(|entry| entry.outcome == RecoveryOutcome::Applied)
        .map(|entry| format!("{:?}", entry.rule))
        .collect();
    recovery_rules.sort();
    recovery_rules.dedup();
    Ok(
        json!({"path":name,"sourceHash":fingerprint(source.as_bytes()),"observations":observations,
        "languageEvidence":{"sourceKind":index.language_evidence.source_kind,"fidelity":index.language_evidence.fidelity,"ambiguous":index.language_evidence.ambiguous},
        "recoveryRules":recovery_rules,"coverage":{"state":state,"groups":groups,"gaps":gaps,
        "truncated":index.diagnostics.coverage.summary.truncated,"uncoveredRatio":index.diagnostics.coverage.summary.uncovered_ratio()}}),
    )
}

fn sort_observations(observations: &mut [Value]) {
    observations.sort_by(|left, right| {
        left["nameRange"]["startByte"]
            .as_u64()
            .cmp(&right["nameRange"]["startByte"].as_u64())
            .then_with(|| left["kind"].as_str().cmp(&right["kind"].as_str()))
    });
}

#[test]
fn c_frontend_manual_contracts_match_complete_observation_sets() {
    let root = fixture_root();
    let manifest = root.join("manifest.json");
    let actual = observe_manifest(&manifest).expect("fixture observations");
    let expected: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("expected.json")).expect("independent reviewed expectations"),
    )
    .unwrap();
    let expected = expected.as_array().unwrap();
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(
            actual, expected,
            "complete observations for {}",
            expected["path"]
        );
    }
}

#[test]
fn c_frontend_fingerprint_has_fixed_cross_language_vectors() {
    assert_eq!(fingerprint(b""), "fnv1a64:cbf29ce484222325");
    assert_eq!(fingerprint(b"hello"), "fnv1a64:a430d84680aabd0b");
}

#[test]
#[ignore = "explicit conformance script export, never a production CLI command"]
fn export_c_frontend_observations() {
    let manifest = std::path::PathBuf::from(
        std::env::var("FOSSILSENSE_CONFORMANCE_MANIFEST").expect("manifest environment"),
    );
    let output = std::path::PathBuf::from(
        std::env::var("FOSSILSENSE_CONFORMANCE_OUTPUT").expect("output environment"),
    );
    let run_id = std::env::var("FOSSILSENSE_CONFORMANCE_RUN_ID").expect("run id environment");
    assert!(
        !run_id.is_empty()
            && run_id.len() <= 80
            && run_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    );
    assert!(!output.exists(), "output must be exclusive to this run");
    assert_eq!(output.file_name().unwrap(), "observations.json");
    let bytes = std::fs::read(&manifest).unwrap();
    let value = json!({"formatVersion":1,"runId":run_id,"manifestHash":fingerprint(&bytes),
        "observations":observe_manifest(&manifest).unwrap()});
    let data = serde_json::to_vec_pretty(&value).unwrap();
    assert!(data.len() <= 16 * 1024 * 1024, "export size limit");
    let directory = output.parent().unwrap();
    std::fs::create_dir_all(directory).unwrap();
    let mut temporary = tempfile::NamedTempFile::new_in(directory).unwrap();
    temporary.write_all(&data).unwrap();
    temporary.as_file().sync_all().unwrap();
    temporary.persist_noclobber(&output).unwrap();
    println!("conformance_export: {}", output.display());
}

#[test]
fn c_frontend_marker_region_stops_at_outer_statement_and_honors_limits() {
    use crate::c_lexical::LexicalMap;
    use crate::parser::recovery::{following_declaration_end, MAX_RECOVERY_REGION_BYTES};
    use crate::parser::RecoveryFailureReason;
    let marker = "PROTOBUF_C__END_DECLS";
    for statement in [
        "struct R { int first; /* ; */ int second; };",
        "const char *items[] = { \"};\", \";\" };",
        "int work(void) { return 0; }",
    ] {
        let source = format!("{marker}\n{statement}\nint unrelated;\n");
        let end = following_declaration_end(
            &source,
            &LexicalMap::new(&source, false),
            &(0..marker.len()),
            true,
        )
        .unwrap();
        assert_eq!(end, marker.len() + 1 + statement.len(), "{statement}");
    }
    let source = format!(
        "{marker}{}int later;",
        " ".repeat(MAX_RECOVERY_REGION_BYTES)
    );
    assert_eq!(
        following_declaration_end(
            &source,
            &LexicalMap::new(&source, false),
            &(0..marker.len()),
            true
        ),
        Err(RecoveryFailureReason::RegionBudgetExceeded)
    );
    let source = format!("{marker}\nint x{};", "[".repeat(65));
    assert_eq!(
        following_declaration_end(
            &source,
            &LexicalMap::new(&source, false),
            &(0..marker.len()),
            true
        ),
        Err(RecoveryFailureReason::ParenthesisDepthExceeded)
    );
    let source = format!("{marker}\nint x{};", "([".repeat(33));
    assert_eq!(
        following_declaration_end(
            &source,
            &LexicalMap::new(&source, false),
            &(0..marker.len()),
            true
        ),
        Err(RecoveryFailureReason::ParenthesisDepthExceeded)
    );
    assert_eq!(
        following_declaration_end(
            marker,
            &LexicalMap::new(marker, false),
            &(0..marker.len()),
            true
        )
        .unwrap(),
        marker.len()
    );
}

#[test]
fn c_frontend_combined_recovery_preserves_neighboring_fact_fidelity() {
    let decorated = std::fs::read_to_string(fixture_root().join("decorated.pb-c.h")).unwrap();
    let source = format!("int before;\n{decorated}int after;\n");
    let parsed = parse(Path::new("neighbors.pb-c.h"), &source);
    for name in ["before", "after"] {
        let fact = parsed
            .declarations
            .iter()
            .find(|fact| fact.name == name)
            .expect("healthy neighbor");
        assert_eq!(
            fact.identity.fact_fidelity,
            crate::semantic_model::SemanticFactFidelity::Authoritative
        );
        assert_eq!(
            &source[fact.name_range.start_byte..fact.name_range.end_byte],
            name
        );
    }
    assert!(parsed.declarations.iter().any(|fact| fact.name == "Public"));
    assert!(parsed
        .declarations
        .iter()
        .any(|fact| fact.name == "aligned"));
    assert!(!parsed.diagnostics.recovery_budget_exhausted);
}

// Expected positions are mapped from reviewed golden byte offsets with an
// independent UTF-16 calculation, never copied from the parser's new output.
fn mapped_range(source: &str, start: usize, end: usize) -> Value {
    let position = |offset: usize| {
        let prefix = &source[..offset];
        json!({"line":prefix.bytes().filter(|byte|*byte==b'\n').count(),
            "character":prefix.rsplit('\n').next().unwrap().encode_utf16().count()})
    };
    json!({"startByte":start,"endByte":end,"start":position(start),"end":position(end)})
}

fn relocate_ranges(value: &mut Value, source: &str, map: impl Fn(usize) -> usize + Copy) {
    if let (Some(start), Some(end)) = (
        value.get("startByte").and_then(Value::as_u64),
        value.get("endByte").and_then(Value::as_u64),
    ) {
        *value = mapped_range(source, map(start as usize), map(end as usize));
        return;
    }
    match value {
        Value::Array(values) => {
            for value in values {
                relocate_ranges(value, source, map);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                relocate_ranges(value, source, map);
            }
        }
        _ => {}
    }
}

#[test]
fn c_frontend_comment_crlf_and_whitespace_preserve_entities_and_mapped_positions() {
    let root = fixture_root();
    let golden: Vec<Value> =
        serde_json::from_slice(&std::fs::read(root.join("expected.json")).unwrap()).unwrap();
    for path in ["declarators.c", "decorated.pb-c.h"] {
        let source = std::fs::read_to_string(root.join(path)).unwrap();
        let original = golden.iter().find(|file| file["path"] == path).unwrap();
        for variant in ["comment", "crlf", "indent"] {
            let prefix = "/* __aligned(32) stays a comment */\n";
            let transformed = match variant {
                "comment" => format!("{prefix}{source}"),
                "crlf" => source.replace('\n', "\r\n"),
                _ => source
                    .split_inclusive('\n')
                    .map(|line| format!("  {line}"))
                    .collect::<String>(),
            };
            let map = |offset: usize| match variant {
                "comment" => prefix.len() + offset,
                "crlf" => {
                    offset
                        + source.as_bytes()[..offset]
                            .iter()
                            .filter(|byte| **byte == b'\n')
                            .count()
                }
                _ => {
                    offset
                        + 2 * (1 + source.as_bytes()[..offset]
                            .iter()
                            .filter(|byte| **byte == b'\n')
                            .count())
                }
            };
            let mut expected = original.clone();
            relocate_ranges(&mut expected, &transformed, map);
            expected["sourceHash"] = json!(fingerprint(transformed.as_bytes()));
            let actual = observe_source(path, &transformed, "default").unwrap();
            assert_eq!(actual, expected, "{path} {variant}");
        }
    }
}

#[test]
fn c_frontend_sibling_order_preserves_factory_and_function_pointer_kinds() {
    let root = fixture_root();
    let path = "declarators.c";
    let source = std::fs::read_to_string(root.join(path)).unwrap();
    let transformed = source.replace(
        "int (*factory(void))(int), (*cb)(int);",
        "int (*cb)(int), (*factory(void))(int);",
    );
    assert_eq!(source.len(), transformed.len());
    let golden: Vec<Value> =
        serde_json::from_slice(&std::fs::read(root.join("expected.json")).unwrap()).unwrap();
    let mut expected = golden
        .into_iter()
        .find(|file| file["path"] == path)
        .unwrap();
    for observation in expected["observations"].as_array_mut().unwrap() {
        let name = observation["name"].as_str().unwrap().to_string();
        if ["factory", "cb"].contains(&name.as_str()) {
            let start = transformed.find(&name).unwrap();
            observation["nameRange"] = mapped_range(&transformed, start, start + name.len());
        }
    }
    sort_observations(expected["observations"].as_array_mut().unwrap());
    expected["sourceHash"] = json!(fingerprint(transformed.as_bytes()));
    assert_eq!(
        observe_source(path, &transformed, "default").unwrap(),
        expected
    );
}
