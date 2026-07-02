use std::fs;

use tempfile::tempdir;

use super::*;

#[test]
fn word_boundary_excludes_substring() {
    let dir = tempdir().expect("tempdir");
    fs::write(dir.path().join("a.c"), "Page PageTable KePage\n").expect("write");

    let (hits, truncated, _) = search_references(dir.path(), "Page").expect("search");
    assert!(!truncated);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].rel_path, "a.c");
    assert_eq!(hits[0].line, 0);
}

#[test]
fn skips_non_source_files_and_excluded_dirs() {
    let dir = tempdir().expect("tempdir");
    fs::write(dir.path().join("a.c"), "hello\n").expect("write");
    fs::write(dir.path().join("a.txt"), "hello\n").expect("write");
    fs::create_dir_all(dir.path().join("target")).expect("target");
    fs::write(dir.path().join("target/b.c"), "hello\n").expect("write");

    let (hits, _, _) = search_references(dir.path(), "hello").expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].rel_path, "a.c");
}

#[test]
fn utf16_col_with_chinese_prefix() {
    let dir = tempdir().expect("tempdir");
    // "中文" contributes 2 UTF-16 code units, ";" contributes 1.
    // The identifier "foo" therefore starts at column 3 and ends at column 6.
    fs::write(dir.path().join("a.c"), "中文;foo\n").expect("write");

    let (hits, _, _) = search_references(dir.path(), "foo").expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].start_col_utf16, 3);
    assert_eq!(hits[0].end_col_utf16, 6);
}

#[test]
fn truncates_to_limit_and_reports_truncated() {
    let dir = tempdir().expect("tempdir");
    let mut contents = String::new();
    for i in 0..REFERENCES_LIMIT + 100 {
        contents.push_str(&format!("ident {i}\n"));
    }
    fs::write(dir.path().join("a.c"), contents).expect("write");

    let (hits, truncated, _) = search_references(dir.path(), "ident").expect("search");
    assert!(truncated);
    assert_eq!(hits.len(), REFERENCES_LIMIT);
}

#[test]
fn invalid_utf8_line_still_yields_match() {
    let dir = tempdir().expect("tempdir");
    // A line mixing invalid UTF-8 bytes (think a GBK comment) with an ASCII
    // identifier must still match rather than dropping the whole file.
    let mut bytes: Vec<u8> = vec![0x2F, 0x2F, 0x20, 0xC0, 0xC1, 0x20]; // "// " + invalid + " "
    bytes.extend_from_slice(b"foo\n");
    fs::write(dir.path().join("a.c"), bytes).expect("write");

    let (hits, _, _) = search_references(dir.path(), "foo").expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].rel_path, "a.c");
    assert_eq!(hits[0].line, 0);
}

#[test]
fn respects_fossilsense_json_scope() {
    let dir = tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("src")).expect("src");
    fs::create_dir_all(dir.path().join("third_party")).expect("third_party");
    fs::write(
        dir.path().join("fossilsense.json"),
        r#"{"include": ["src/"]}"#,
    )
    .expect("config");
    fs::write(dir.path().join("src/main.c"), "hello\n").expect("main");
    fs::write(dir.path().join("third_party/foo.c"), "hello\n").expect("foo");

    let (hits, _, _) = search_references(dir.path(), "hello").expect("search");
    assert_eq!(hits.len(), 1, "only src/ files should appear");
    assert_eq!(hits[0].rel_path, "src/main.c");
}

#[test]
fn classifies_declaration_and_call() {
    let dir = tempdir().expect("tempdir");
    fs::write(
        dir.path().join("a.c"),
        "int foo(void);\nint main(void) { return foo(); }\n",
    )
    .expect("write");

    let (hits, _, _) = search_references(dir.path(), "foo").expect("search");
    assert_eq!(hits.len(), 2, "prototype + call");
    let decl = hits.iter().find(|h| h.line == 0).expect("decl hit");
    let call = hits.iter().find(|h| h.line == 1).expect("call hit");
    assert_eq!(decl.role, SyntacticRole::Declaration);
    assert_eq!(call.role, SyntacticRole::Call);
}

#[test]
fn classification_preserves_text_search_positions() {
    let dir = tempdir().expect("tempdir");
    fs::write(
        dir.path().join("a.c"),
        "int foo(void);\nint main(void) { return foo(); }\n",
    )
    .expect("write");

    let (hits, _, _) = search_references(dir.path(), "foo").expect("search");
    // Classification must not add, drop, or move positions: exactly the two
    // whole-word matches at their original lines remain.
    let mut lines: Vec<u32> = hits.iter().map(|h| h.line).collect();
    lines.sort_unstable();
    assert_eq!(lines, vec![0, 1]);
    assert!(hits.iter().all(|h| h.end_col_utf16 > h.start_col_utf16));
}

#[test]
fn unparseable_file_falls_back_to_read() {
    let dir = tempdir().expect("tempdir");
    // Invalid UTF-8 (e.g. a GBK comment) makes the file unparseable, but the
    // lossy text search still finds the identifier; its role degrades to Read.
    let mut bytes: Vec<u8> = vec![0x2F, 0x2F, 0x20, 0xC0, 0xC1, 0x20]; // "// " + invalid + " "
    bytes.extend_from_slice(b"foo\n");
    fs::write(dir.path().join("a.c"), bytes).expect("write");

    let (hits, _, _) = search_references(dir.path(), "foo").expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].role, SyntacticRole::Read);
}

#[test]
fn cached_search_matches_uncached_and_repeats() {
    let dir = tempdir().expect("tempdir");
    fs::write(
        dir.path().join("a.c"),
        "int foo(void);\nint main(void) { return foo(); }\n",
    )
    .expect("write");

    let cache = ReferenceRoleCache::new();
    let (first, _, _) = search_references_cached(dir.path(), "foo", &cache).expect("search");
    // Second query for the same unchanged file reuses the cache and agrees.
    let (second, _, _) = search_references_cached(dir.path(), "foo", &cache).expect("search");
    assert_eq!(first, second);
    assert!(first.iter().any(|h| h.role == SyntacticRole::Declaration));
    assert!(first.iter().any(|h| h.role == SyntacticRole::Call));
}

#[test]
fn indexed_file_list_limits_reference_discovery() {
    let dir = tempdir().expect("tempdir");
    let a = dir.path().join("a.c");
    let b = dir.path().join("b.c");
    fs::write(&a, "hello\n").expect("a");
    fs::write(&b, "hello\n").expect("b");

    let role_cache = ReferenceRoleCache::new();
    let search_cache = ReferenceSearchCache::new();
    let (hits, truncated, _) = search_references_with_result_cache_and_files(
        dir.path(),
        "hello",
        &role_cache,
        &search_cache,
        1,
        Some(vec![("a.c".to_string(), a)]),
    )
    .expect("search");

    assert!(!truncated);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].rel_path, "a.c");
}

#[test]
fn empty_indexed_file_list_falls_back_to_walk() {
    let dir = tempdir().expect("tempdir");
    fs::write(dir.path().join("a.c"), "hello\n").expect("a");
    fs::write(dir.path().join("b.c"), "hello\n").expect("b");

    let role_cache = ReferenceRoleCache::new();
    let search_cache = ReferenceSearchCache::new();
    let (hits, truncated, _) = search_references_with_result_cache_and_files(
        dir.path(),
        "hello",
        &role_cache,
        &search_cache,
        1,
        Some(Vec::new()),
    )
    .expect("search");

    assert!(!truncated);
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].rel_path, "a.c");
    assert_eq!(hits[1].rel_path, "b.c");
}

#[test]
fn result_cache_reuses_search_hits_until_cleared() {
    let dir = tempdir().expect("tempdir");
    let file = dir.path().join("a.c");
    fs::write(&file, "int foo;\nfoo = 1;\n").expect("write");
    let role_cache = ReferenceRoleCache::new();
    let search_cache = ReferenceSearchCache::new();

    let (first, _, _) =
        search_references_with_result_cache(dir.path(), "foo", &role_cache, &search_cache)
            .expect("first search");
    assert_eq!(first.len(), 2);

    // Without clearing, the second query returns the cached hit set and does
    // not rediscover files or re-run the text search against changed disk
    // contents.
    fs::write(&file, "int bar;\nbar = 1;\n").expect("rewrite");
    let (cached, _, _) =
        search_references_with_result_cache(dir.path(), "foo", &role_cache, &search_cache)
            .expect("cached search");
    assert_eq!(cached, first);

    search_cache.clear();
    let (after_clear, _, _) =
        search_references_with_result_cache(dir.path(), "foo", &role_cache, &search_cache)
            .expect("search after clear");
    assert!(after_clear.is_empty());
}

#[test]
fn result_cache_generation_change_bypasses_stale_hits() {
    let dir = tempdir().expect("tempdir");
    let a = dir.path().join("a.c");
    let b = dir.path().join("b.c");
    fs::write(&a, "hello\n").expect("a");
    fs::write(&b, "hello\n").expect("b");

    let role_cache = ReferenceRoleCache::new();
    let search_cache = ReferenceSearchCache::new();
    let (first, _, first_timing) = search_references_with_result_cache_and_files(
        dir.path(),
        "hello",
        &role_cache,
        &search_cache,
        1,
        Some(vec![("a.c".to_string(), a.clone())]),
    )
    .expect("first search");
    assert_eq!(first.len(), 1);
    assert!(!first_timing.cached);

    let (stale_generation, _, stale_timing) = search_references_with_result_cache_and_files(
        dir.path(),
        "hello",
        &role_cache,
        &search_cache,
        1,
        Some(vec![
            ("a.c".to_string(), a.clone()),
            ("b.c".to_string(), b.clone()),
        ]),
    )
    .expect("same generation search");
    assert_eq!(stale_generation.len(), 1);
    assert!(stale_timing.cached);

    let (new_generation, _, new_timing) = search_references_with_result_cache_and_files(
        dir.path(),
        "hello",
        &role_cache,
        &search_cache,
        2,
        Some(vec![("a.c".to_string(), a), ("b.c".to_string(), b)]),
    )
    .expect("new generation search");
    assert_eq!(new_generation.len(), 2);
    assert!(!new_timing.cached);
}

#[test]
fn role_cache_hits_on_match_and_misses_on_change() {
    let cache = ReferenceRoleCache::new();
    let mut map = HashMap::new();
    map.insert((1u32, 2u32), SyntacticRole::Call);
    let roles = Arc::new(map);
    cache.put("a.c".to_string(), (10, 20), roles);

    // Same fingerprint → cache hit.
    assert!(cache.get("a.c", (10, 20)).is_some());
    // Changed fingerprint (file edited) → miss, forcing a re-parse.
    assert!(cache.get("a.c", (11, 20)).is_none());
    // Unknown file → miss.
    assert!(cache.get("b.c", (10, 20)).is_none());
}

// --- R6: role label + role-grouped sort -------------------------------

// --- R7: enhanced cache observability tests -------------------------------

#[test]
fn role_cache_miss_on_size_change() {
    // Same mtime, different size → cache miss (file was edited).
    let cache = ReferenceRoleCache::new();
    let mut map = HashMap::new();
    map.insert((1, 2), SyntacticRole::Call);
    let roles = Arc::new(map);
    cache.put("a.c".to_string(), (100, 200), roles);

    // Same mtime, different size → miss.
    assert!(
        cache.get("a.c", (100, 200)).is_some(),
        "original fingerprint hits"
    );
    assert!(
        cache.get("a.c", (100, 201)).is_none(),
        "size increased → miss"
    );
    assert!(
        cache.get("a.c", (100, 199)).is_none(),
        "size decreased → miss"
    );
}

#[test]
fn role_cache_miss_on_mtime_change() {
    // Same size, different mtime → cache miss (file touched/recompiled).
    let cache = ReferenceRoleCache::new();
    let mut map = HashMap::new();
    map.insert((1, 2), SyntacticRole::Call);
    let roles = Arc::new(map);
    cache.put("a.c".to_string(), (100, 300), roles);

    assert!(cache.get("a.c", (100, 300)).is_some(), "original hits");
    assert!(
        cache.get("a.c", (101, 300)).is_none(),
        "mtime changed → miss"
    );
    assert!(
        cache.get("a.c", (99, 300)).is_none(),
        "mtime decreased → miss"
    );
}

#[test]
fn search_cache_clear_returns_fresh_disk_results() {
    // After clearing the search cache, a re-search against changed disk
    // content produces fresh results — it does NOT return stale data.
    let dir = tempdir().expect("tempdir");
    let file = dir.path().join("a.c");
    fs::write(&file, "int foo;\nfoo = 1;\n").expect("write");

    let role_cache = ReferenceRoleCache::new();
    let search_cache = ReferenceSearchCache::new();

    let (first, _, _) =
        search_references_with_result_cache(dir.path(), "foo", &role_cache, &search_cache)
            .expect("first search");
    assert_eq!(first.len(), 2);

    // Change the file on disk.
    fs::write(&file, "int bar;\nbar = 1;\n").expect("rewrite");
    // Clear the search cache — this is what happens on file watcher events.
    search_cache.clear();

    let (after_clear, _, _) =
        search_references_with_result_cache(dir.path(), "foo", &role_cache, &search_cache)
            .expect("search after clear");
    assert!(
        after_clear.is_empty(),
        "after clear, re-search sees current disk content (foo is gone)"
    );
}

fn hit(rel: &str, line: u32, role: SyntacticRole) -> ReferenceHit {
    ReferenceHit {
        rel_path: rel.to_string(),
        line,
        start_col_utf16: 0,
        end_col_utf16: 3,
        role,
    }
}

#[test]
fn role_label_is_distinct_for_every_role() {
    let roles = [
        SyntacticRole::Definition,
        SyntacticRole::Declaration,
        SyntacticRole::Call,
        SyntacticRole::Write,
        SyntacticRole::TypeUse,
        SyntacticRole::Read,
    ];
    let labels: Vec<&str> = roles.iter().map(|r| role_label(*r)).collect();
    assert_eq!(
        labels,
        vec!["definition", "declaration", "call", "write", "type", "read"]
    );
    let mut sorted = labels.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), roles.len(), "labels are distinct");
}

#[test]
fn sort_hits_by_role_groups_definitions_first() {
    let mut hits = vec![
        hit("z.c", 9, SyntacticRole::Read),
        hit("a.c", 1, SyntacticRole::Call),
        hit("b.c", 2, SyntacticRole::Definition),
        hit("a.c", 5, SyntacticRole::Declaration),
    ];
    sort_hits_by_role(&mut hits);
    let order: Vec<SyntacticRole> = hits.iter().map(|h| h.role).collect();
    assert_eq!(
        order,
        vec![
            SyntacticRole::Definition,
            SyntacticRole::Declaration,
            SyntacticRole::Call,
            SyntacticRole::Read,
        ],
        "definition/declaration sort before call/read"
    );
}
