use std::process::{Command, Output};

fn successful_stdout(output: Output, label: &str) -> String {
    assert!(
        output.status.success(),
        "{label} failed with status {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("fossilsense stdout must be UTF-8")
}

#[test]
fn query_calls_prints_caller_for_incoming_and_callee_for_outgoing() {
    let temp = tempfile::tempdir().expect("temporary workspace");
    let workspace = temp.path().join("workspace");
    let db = temp.path().join("index.sqlite");
    std::fs::create_dir(&workspace).expect("workspace directory");
    std::fs::write(
        workspace.join("main.c"),
        "int target(void) {\n\
             return 1;\n\
         }\n\
         int caller(void) {\n\
             return target();\n\
         }\n",
    )
    .expect("C fixture");

    let bin = env!("CARGO_BIN_EXE_fossilsense");
    let index = Command::new(bin)
        .arg("index")
        .arg(&workspace)
        .arg("--db")
        .arg(&db)
        .arg("--force")
        .output()
        .expect("run fossilsense index");
    successful_stdout(index, "index");

    let incoming = Command::new(bin)
        .arg("query")
        .arg("calls")
        .arg(&workspace)
        .arg("main.c")
        .arg("1")
        .arg("5")
        .arg("--incoming")
        .arg("--db")
        .arg(&db)
        .output()
        .expect("run incoming call query");
    let incoming = successful_stdout(incoming, "incoming query");
    assert!(
        incoming.lines().any(|line| line.starts_with("caller\t")),
        "incoming relation must print the caller, got:\n{incoming}"
    );
    assert!(
        !incoming.lines().any(|line| line.starts_with("target\t")),
        "incoming relation must not print the root callee, got:\n{incoming}"
    );

    let outgoing = Command::new(bin)
        .arg("query")
        .arg("calls")
        .arg(&workspace)
        .arg("main.c")
        .arg("4")
        .arg("5")
        .arg("--db")
        .arg(&db)
        .output()
        .expect("run outgoing call query");
    let outgoing = successful_stdout(outgoing, "outgoing query");
    assert!(
        outgoing.lines().any(|line| line.starts_with("target\t")),
        "outgoing relation must print the callee, got:\n{outgoing}"
    );
}
