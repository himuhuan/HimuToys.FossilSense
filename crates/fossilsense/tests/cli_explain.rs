use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

struct Fixture {
    _temp: tempfile::TempDir,
    workspace: PathBuf,
    db: PathBuf,
}

impl Fixture {
    fn new(source: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        fs::write(workspace.join("main.c"), source).unwrap();
        let db = temp.path().join("index.sqlite");
        Self {
            _temp: temp,
            workspace,
            db,
        }
    }

    fn index(&self) {
        let output = Command::new(env!("CARGO_BIN_EXE_fossilsense"))
            .arg("index")
            .arg(&self.workspace)
            .arg("--db")
            .arg(&self.db)
            .arg("--force")
            .output()
            .unwrap();
        assert_success(&output);
    }

    fn explain(&self, file: &Path, name: &str, extra: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_fossilsense"))
            .args(["query", "explain"])
            .arg(&self.workspace)
            .arg(file)
            .arg(name)
            .arg("--db")
            .arg(&self.db)
            .arg("--json")
            .args(extra)
            .output()
            .unwrap()
    }

    fn json(&self, name: &str) -> Value {
        let output = self.explain(Path::new("main.c"), name, &[]);
        assert_success(&output);
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status={}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn explain_no_index_reports_all_stages_and_bounded_ambiguity_without_creating_files() {
    let f = Fixture::new("int item;\nint caller(void) { return item; }\n");
    let report = f.json("item");
    assert_eq!(report["formatVersion"], 1);
    assert_eq!(report["request"]["ambiguous"], true);
    assert_eq!(report["request"]["positions"].as_array().unwrap().len(), 2);
    assert_eq!(
        report["diskObservation"]["contentHash"],
        blake3::hash(&fs::read(f.workspace.join("main.c")).unwrap())
            .to_hex()
            .as_str()
    );
    for stage in [
        "inclusion",
        "language",
        "region",
        "extraction",
        "persistence",
        "recall",
        "presentation",
    ] {
        assert!(
            report["stages"][stage]["status"].is_string(),
            "{stage}: {report}"
        );
        assert!(report["stages"][stage]["reason"].is_string());
        assert!(report["stages"][stage]["counts"].is_object());
        assert!(report["stages"][stage].get("limit").is_some());
    }
    assert_eq!(report["stages"]["extraction"]["status"], "found");
    for stage in ["persistence", "recall", "presentation"] {
        assert_eq!(report["stages"][stage]["status"], "not_run");
    }
    assert!(!f.db.exists());
    assert_eq!(
        fs::read_dir(f.workspace.parent().unwrap()).unwrap().count(),
        1
    );
}

#[test]
fn explain_validates_utf16_positions_and_paired_arguments() {
    let f = Fixture::new("/*😀*/ int item;\n");
    let output = f.explain(Path::new("main.c"), "item", &["--line", "1", "--col", "12"]);
    assert_success(&output);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["request"]["ambiguous"], false);
    assert_eq!(report["request"]["positions"].as_array().unwrap().len(), 1);
    for extra in [
        vec!["--line", "1"],
        vec!["--col", "12"],
        vec!["--line", "0", "--col", "12"],
        vec!["--line", "2", "--col", "1"],
        vec!["--line", "1", "--col", "4"],
        vec!["--line", "1", "--col", "1"],
    ] {
        assert_eq!(
            f.explain(Path::new("main.c"), "item", &extra).status.code(),
            Some(2),
            "{extra:?}"
        );
    }
}

#[test]
fn explain_rejects_absolute_root_and_parent_escape_before_reading_source() {
    let f = Fixture::new("int item;\n");
    let outside = f.workspace.parent().unwrap().join("outside.c");
    fs::write(&outside, [0xff, 0xfe, 0x00]).unwrap();
    for path in [
        outside,
        PathBuf::from("../outside.c"),
        PathBuf::from("/outside.c"),
    ] {
        let out = f.explain(&path, "item", &[]);
        assert_eq!(out.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&out.stderr).contains("workspace-relative"));
    }
}

#[test]
fn explain_rejects_link_escape_using_real_paths() {
    let f = Fixture::new("int item;\n");
    let outside = f.workspace.parent().unwrap().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret.c"), [0xff, 0xfe]).unwrap();
    let link = f.workspace.join("link");
    #[cfg(windows)]
    {
        let out = Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&outside)
            .output()
            .unwrap();
        assert_success(&out);
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let out = f.explain(Path::new("link/secret.c"), "item", &[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("outside workspace"));
}

#[test]
fn explain_distinguishes_exclusion_unknown_macro_and_absent_name() {
    let f = Fixture::new(include_str!("fixtures/c_frontend/unknown_macro.c"));
    fs::write(
        f.workspace.join("fossilsense.json"),
        r#"{"exclude":["main.c"]}"#,
    )
    .unwrap();
    let report = f.json("net");
    assert_eq!(report["stages"]["inclusion"]["status"], "absent");
    assert_eq!(report["stages"]["region"]["status"], "partial");
    assert!(report["stages"]["region"]["evidence"]
        .to_string()
        .contains("unknown_macro"));
    assert_ne!(report["stages"]["extraction"]["status"], "absent");
    let healthy = Fixture::new("int item;\n");
    assert_eq!(
        healthy.json("missing")["stages"]["extraction"]["status"],
        "absent"
    );
}

#[test]
fn explain_preserves_readonly_database_wal_source_and_config() {
    let f = Fixture::new("int item;\n");
    let config = f.workspace.join("fossilsense.json");
    fs::write(&config, "{}\n").unwrap();
    f.index();
    let writer = rusqlite::Connection::open(&f.db).unwrap();
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL; UPDATE meta SET value='99' WHERE key='semantic_generation';",
        )
        .unwrap();
    let wal = PathBuf::from(format!("{}-wal", f.db.display()));
    assert!(wal.exists());
    let paths = [&f.db, &wal, &config, &f.workspace.join("main.c")];
    let before: Vec<_> = paths.iter().map(|p| fs::read(p).unwrap()).collect();
    let mut permissions = fs::metadata(&f.db).unwrap().permissions();
    let original_permissions = permissions.clone();
    permissions.set_readonly(true);
    fs::set_permissions(&f.db, permissions).unwrap();
    let report = f.json("item");
    fs::set_permissions(&f.db, original_permissions).unwrap();
    assert_eq!(report["indexObservation"]["generation"], 99);
    assert_eq!(report["indexObservation"]["consistency"], "matched");
    assert_eq!(report["stages"]["persistence"]["status"], "found");
    for (path, bytes) in paths.iter().zip(before) {
        assert_eq!(fs::read(path).unwrap(), bytes, "{} changed", path.display());
    }
    assert_eq!(
        f.json("item"),
        report,
        "static observations must be deterministic"
    );
    assert!(!f
        .workspace
        .parent()
        .unwrap()
        .join("index.sqlite.writer-lock")
        .exists());
}

#[test]
fn explain_stale_disk_is_not_reported_as_storage_loss() {
    let f = Fixture::new("int old_name;\n");
    f.index();
    fs::write(f.workspace.join("main.c"), "int new_name;\n").unwrap();
    let report = f.json("new_name");
    assert_eq!(report["indexObservation"]["consistency"], "stale");
    assert_eq!(report["stages"]["extraction"]["status"], "found");
    assert_eq!(
        report["stages"]["persistence"]["reason"],
        "disk_index_mismatch"
    );
    assert!(!report.to_string().contains("storage_loss"));
}

#[test]
fn explain_old_schema_is_unknown_and_corrupt_database_is_runtime_error() {
    let f = Fixture::new("int item;\n");
    let old = rusqlite::Connection::open(&f.db).unwrap();
    old.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT); INSERT INTO meta VALUES('schema_version','1');").unwrap();
    drop(old);
    let before = fs::read(&f.db).unwrap();
    let report = f.json("item");
    assert_eq!(report["indexObservation"]["status"], "unknown");
    assert_eq!(report["stages"]["persistence"]["status"], "not_run");
    assert_eq!(fs::read(&f.db).unwrap(), before);
    fs::write(&f.db, "this is not SQLite").unwrap();
    assert_eq!(
        f.explain(Path::new("main.c"), "item", &[]).status.code(),
        Some(1)
    );
}

#[test]
fn explain_reports_recall_and_diagnostic_truncation_separately() {
    let f = Fixture::new(&"int repeated(void);\n".repeat(300));
    f.index();
    let report = f.json("repeated");
    assert_eq!(report["request"]["positions"].as_array().unwrap().len(), 64);
    assert_eq!(report["request"]["truncated"], true);
    assert_eq!(report["stages"]["persistence"]["limit"], 256);
    assert_eq!(report["stages"]["persistence"]["status"], "truncated");
    assert_eq!(report["stages"]["recall"]["limit"], 256);
    assert_eq!(report["stages"]["recall"]["status"], "truncated");
    assert!(
        report["stages"]["presentation"]["counts"]["omitted"]
            .as_u64()
            .unwrap()
            <= 256
    );
    let admitted = report["stages"]["recall"]["evidence"]["declarationIds"]
        .as_array()
        .unwrap();
    for omitted in report["stages"]["presentation"]["evidence"]["omittedIds"]
        .as_array()
        .unwrap()
    {
        assert!(admitted.contains(omitted));
    }
}

#[test]
fn explain_presentation_matches_query_def_and_exposes_role_omissions() {
    let f = Fixture::new("int target(void);\nint target(void) { return 1; }\n");
    f.index();
    let report = f.json("target");
    assert_eq!(report["stages"]["recall"]["counts"]["returned"], 2);
    assert_eq!(report["stages"]["presentation"]["counts"]["returned"], 1);
    assert_eq!(report["stages"]["presentation"]["counts"]["omitted"], 1);
    let old = Command::new(env!("CARGO_BIN_EXE_fossilsense"))
        .args(["query", "def"])
        .arg(&f.workspace)
        .args(["main.c", "1", "5", "--db"])
        .arg(&f.db)
        .output()
        .unwrap();
    assert_success(&old);
    let output = String::from_utf8(old.stdout).unwrap();
    assert!(output.contains("candidates: 1"));
    let presented = &report["stages"]["presentation"]["evidence"]["candidates"][0];
    assert_eq!(presented["path"], "main.c");
    assert_eq!(presented["range"]["startLine"], 1);
    assert!(output.contains("main.c:2"));
}

#[cfg(windows)]
#[test]
fn explain_windows_case_variants_use_the_same_indexed_file() {
    let f = Fixture::new("int item;\n");
    f.index();
    let out = f.explain(Path::new("MAIN.C"), "item", &[]);
    assert_success(&out);
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["indexObservation"]["consistency"], "matched");
    assert_eq!(report["stages"]["persistence"]["counts"]["returned"], 1);
}

#[test]
fn explain_configuration_mismatch_and_old_parser_version_are_explicit() {
    let f = Fixture::new("int item;\n");
    f.index();
    fs::write(
        f.workspace.join("fossilsense.json"),
        r#"{"languageOverrides":[{"glob":"*.c","language":"cpp"}]}"#,
    )
    .unwrap();
    let report = f.json("item");
    assert_eq!(report["indexObservation"]["consistency"], "stale");
    let db = rusqlite::Connection::open(&f.db).unwrap();
    db.execute("UPDATE file_revisions SET parser_version=1", [])
        .unwrap();
    drop(db);
    let report = f.json("item");
    assert_eq!(report["indexObservation"]["status"], "unknown");
    assert_eq!(
        report["stages"]["persistence"]["reason"],
        "index_parser_version_incompatible"
    );
}

#[test]
fn explain_focused_omissions_only_contain_actually_recalled_ids() {
    let f = Fixture::new("static int pick(void) { return 1; }\n");
    fs::write(
        f.workspace.join("other.c"),
        "static int pick(void) { return 2; }\n",
    )
    .unwrap();
    f.index();
    let report = f.json("pick");
    assert_eq!(report["stages"]["recall"]["counts"]["returned"], 2);
    assert_eq!(report["stages"]["presentation"]["counts"]["returned"], 1);
    assert_eq!(report["stages"]["presentation"]["counts"]["omitted"], 1);
    let ids = report["stages"]["recall"]["evidence"]["declarationIds"]
        .as_array()
        .unwrap();
    let omitted = report["stages"]["presentation"]["evidence"]["omittedIds"]
        .as_array()
        .unwrap();
    assert!(ids.contains(&omitted[0]));
    assert_eq!(
        report["stages"]["presentation"]["evidence"]["candidates"][0]["path"],
        "main.c"
    );
}

#[test]
fn explain_caps_region_details_and_skips_oversized_source_with_valid_json() {
    let f = Fixture::new(&format!(
        "{}\nint healthy;\n",
        "DECLARE_UNKNOWN(value);\n".repeat(300)
    ));
    let report = f.json("healthy");
    assert_eq!(report["stages"]["region"]["status"], "truncated");
    assert_eq!(report["stages"]["region"]["counts"]["returned"], 256);
    let file = fs::File::create(f.workspace.join("main.c")).unwrap();
    file.set_len(16 * 1024 * 1024 + 1).unwrap();
    drop(file);
    let report = f.json("healthy");
    assert_eq!(report["diskObservation"]["status"], "truncated");
    assert_eq!(report["stages"]["extraction"]["status"], "not_run");
    assert!(serde_json::to_vec(&report).unwrap().len() <= 2 * 1024 * 1024);
}

#[test]
fn explain_does_not_call_member_or_request_local_facts_missing_declarations() {
    let f = Fixture::new("struct Box { int field; };\nint work(int local) { return local; }\n");
    f.index();
    for name in ["field", "local"] {
        let report = f.json(name);
        assert_eq!(report["stages"]["extraction"]["status"], "found");
        assert_eq!(report["stages"]["persistence"]["status"], "not_run");
        assert_eq!(
            report["stages"]["persistence"]["reason"],
            "separate_member_or_request_local_fact"
        );
    }
}
