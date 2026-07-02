use super::*;

// --- Slop pin: acceptance criteria for R3 -------------------------------

fn write_ambiguity_fixture(dir: &std::path::Path) {
    // Replicates the `samples/ambiguity/` fixture in a tempdir so the test
    // does not depend on the samples tree staying in sync with the spec.
    use std::fs as fs2;
    fs2::create_dir_all(dir.join("src/a")).expect("src/a");
    fs2::create_dir_all(dir.join("vendor")).expect("vendor");
    fs2::create_dir_all(dir.join("multi")).expect("multi");

    fs2::write(
        dir.join("src/a/util.h"),
        "#ifndef UTIL_H\n#define UTIL_H\nint util_value(void){ return 1; }\n#endif\n",
    )
    .expect("src/a/util.h");
    fs2::write(
        dir.join("vendor/util.h"),
        "#ifndef VENDOR_UTIL_H\n#define VENDOR_UTIL_H\nint util_value(void){ return 2; }\n#endif\n",
    )
    .expect("vendor/util.h");
    fs2::write(
            dir.join("src/a/foo.c"),
            "#include \"util.h\"\n#include <nope/missing.h>\nint use_local(void){ return util_value(); }\n",
        )
        .expect("src/a/foo.c");
    fs2::write(
        dir.join("multi/multi_hit.c"),
        "#include \"util.h\"\nint multi_hit_caller(void);\n",
    )
    .expect("multi/multi_hit.c");
}

#[test]
fn coloring_hard_gate_excludes_wrong_twin_in_same_basename_sample() {
    // Acceptance criterion for R3: with form/priority resolution the wrong
    // same-basename twin is NOT colored as a determinate-reachable symbol.
    // `src/a/foo.c` includes `"util.h"` -> resolves `RelativeExact` to
    // `src/a/util.h`. The reachable set carries `src/a/util.h`; coloring's
    // hard gate `kind_counts_by_names_scoped` filters by reachable path, so
    // only the local definition counts -- never the vendor twin.
    use crate::reachability::ReachGraph;

    let dir = tempdir().expect("tempdir");
    write_ambiguity_fixture(dir.path());
    let db = dir.path().join("index.sqlite");

    index_workspace(
        dir.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            force: false,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let store = IndexStore::open_readonly(&db).expect("readonly");
    let edges = store.load_include_edge_paths().expect("edges");
    // foo.c -> src/a/util.h is the only same-basename edge; vendor/util.h
    // is not in any proven edge from foo.c.
    assert!(
        edges.contains(&("src/a/foo.c".to_string(), "src/a/util.h".to_string())),
        "local RelativeExact edge present; got {edges:?}"
    );
    assert!(
        !edges.iter().any(|(_, dst)| dst == "vendor/util.h"),
        "vendor/util.h twin must not be a proven reachable edge"
    );

    let graph = ReachGraph::new(
        store.load_include_edge_paths().expect("edges"),
        store.open_include_file_paths().expect("open"),
        store.ambiguous_include_file_paths().expect("ambiguous"),
    );
    let scope = graph.reachable("src/a/foo.c");
    assert!(scope.files.contains("src/a/util.h"));
    assert!(!scope.files.contains("vendor/util.h"));

    // Coloring's hard gate: scoped counts exclude the vendor twin
    // (not in the reachable set) -- only the local definition counts.
    let scoped = store
        .kind_counts_by_names_scoped(&["util_value"], Some(&scope.files))
        .expect("scoped");
    assert_eq!(scoped["util_value"].get("function").copied(), Some(1));

    // Multi-hit file's scope is open with AmbiguousInclude; neither twin is
    // a proven reachable member of its set (no edges from ambiguity).
    let multi_scope = graph.reachable("multi/multi_hit.c");
    assert!(multi_scope.open);
    assert!(!multi_scope.files.contains("vendor/util.h"));
    assert!(!multi_scope.files.contains("src/a/util.h"));
    use crate::reachability::OpenReason;
    assert_eq!(multi_scope.reason, Some(OpenReason::AmbiguousInclude));
}

#[test]
fn scoping_off_preserves_global_counts_and_ambiguous_twins_stay_unknown() {
    // Two anti-slop pins: (a) the scoping-off path is unchanged -- unscoped
    // `kind_counts_by_names` still sees every workspace definition, and the
    // list is never emptied by ambiguity; (b) an ambiguous twin surfaces at
    // `Unknown` tier (soft path -- never hidden), never `Reachable`.
    use crate::model::ScopeTier;
    use crate::reachability::{OpenReason, ReachGraph};
    use crate::resolver::{scope_tier, ResolveContext};

    let dir = tempdir().expect("tempdir");
    write_ambiguity_fixture(dir.path());
    let db = dir.path().join("index.sqlite");

    index_workspace(
        dir.path(),
        IndexOptions {
            db_path: Some(db.clone()),
            force: false,
            ..Default::default()
        },
        |_| {},
    )
    .expect("index");

    let store = IndexStore::open_readonly(&db).expect("readonly");
    // Unscoped (scoping-off) counts: both workspace util_value defs count.
    let unscoped = store
        .kind_counts_by_names(&["util_value"])
        .expect("unscoped");
    assert_eq!(unscoped["util_value"].get("function").copied(), Some(2));

    let graph = ReachGraph::new(
        store.load_include_edge_paths().expect("edges"),
        store.open_include_file_paths().expect("open"),
        store.ambiguous_include_file_paths().expect("ambiguous"),
    );
    let scope = graph.reachable("multi/multi_hit.c");
    // Multi-hit scope is open with AmbiguousInclude; the twins are not
    // proven reachable, so `scope_tier` classifies them as `Unknown` (the
    // open-scope soft path) -- surfaced for navigation, NOT proven colored.
    assert_eq!(scope.reason, Some(OpenReason::AmbiguousInclude));
    let ctx = ResolveContext {
        current_path: Some("multi/multi_hit.c"),
        reach: Some(&scope),
    };
    assert_eq!(
        scope_tier("vendor/util.h", false, false, Some(&ctx)),
        ScopeTier::Unknown,
        "ambiguous twin surfaces at Unknown tier (soft path, never hidden)"
    );
    assert_eq!(
        scope_tier("src/a/util.h", false, false, Some(&ctx)),
        ScopeTier::Unknown,
        "the other twin also surfaces at Unknown (no proven edges either)"
    );
    // The calling file itself stays `Current` -- its own navigation is
    // unaffected by the open scope.
    assert_eq!(
        scope_tier("multi/multi_hit.c", false, false, Some(&ctx)),
        ScopeTier::Current
    );
}
