use std::fs;
use std::path::Path;

fn inventory() -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");
    let path = root
        .join("openspec")
        .join("changes")
        .join("unify-fact-consumption-contracts")
        .join("fact-consumption-inventory.md");

    fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

fn assert_contains(source: &str, needle: &str) {
    assert!(
        source.contains(needle),
        "fact-consumption inventory should record `{needle}`"
    );
}

#[test]
fn fact_consumption_inventory_records_group_1_boundary_targets() {
    let source = inventory();

    for heading in [
        "## Scope and Non-Goals",
        "## Parser Field Access Inventory",
        "## Durable Read Wrapper Inventory",
        "## Reference Role Behavior To Preserve",
        "## Final Raw Parser Field Allowlist",
        "## Migration Targets For Next Groups",
    ] {
        assert_contains(&source, heading);
    }

    for target in [
        "MIGRATE(parser): src/references.rs",
        "MIGRATE(parser): src/query/current_file_overlay.rs",
        "MIGRATE(store-api): src/store/queries.rs",
        "MIGRATE(store-api): src/store/includes.rs",
    ] {
        assert_contains(&source, target);
    }
}
