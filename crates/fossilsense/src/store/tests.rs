use tempfile::tempdir;

use crate::parser::parse;

use super::{FileFingerprint, FileSource, IndexStore};
use std::collections::HashSet;

mod basic;
mod call_facts;
mod generations;
mod maintenance;
mod members;
mod parser_consumer_migration;
mod query_scoping;
mod read_model_parity;
mod read_view_migration;
mod read_views;
mod resilience_schema;
mod schema_aliases;
mod scoping;
mod sources_includes;

fn upsert_source(store: &mut IndexStore, path: &str, source: &str) {
    upsert_with_source(store, path, source, FileSource::Workspace);
}

fn upsert_with_source(store: &mut IndexStore, path: &str, source: &str, file_source: FileSource) {
    let index = parse(std::path::Path::new(path), source);
    store
        .upsert_file_index_with_source(
            &FileFingerprint {
                path: path.to_string(),
                extension: path.rsplit('.').next().unwrap_or("c").to_string(),
                size: source.len() as u64,
                mtime_ns: 1,
                hash: format!("{path}-hash"),
            },
            &index,
            file_source,
        )
        .expect("upsert");
}

const TEST_CALL_READ_LIMIT: usize = 10_000;

fn test_anchors_by_name(
    store: &IndexStore,
    name: &str,
) -> Vec<crate::store::views::CallableAnchorRow> {
    let (rows, limited) = store
        .call_fact_view()
        .anchors_by_names_limited(&[name.to_string()], TEST_CALL_READ_LIMIT)
        .expect("anchors by name");
    assert!(!limited, "test fixture exceeded bounded anchor read");
    rows
}

fn test_call_sites_by_callee(
    store: &IndexStore,
    name: &str,
) -> Vec<crate::store::views::CallSiteRow> {
    let (rows, limited) = store
        .call_fact_view()
        .call_sites_by_callee_limited(name, TEST_CALL_READ_LIMIT)
        .expect("call sites by callee");
    assert!(!limited, "test fixture exceeded bounded call read");
    rows
}

fn test_call_sites_by_caller(
    store: &IndexStore,
    entity_key: &str,
) -> Vec<crate::store::views::CallSiteRow> {
    let (rows, limited) = store
        .call_fact_view()
        .call_sites_by_caller_limited(entity_key, TEST_CALL_READ_LIMIT)
        .expect("call sites by caller");
    assert!(!limited, "test fixture exceeded bounded call read");
    rows
}

/// Test convenience over the production record/field APIs: resolve record
/// candidates for `names` (unscoped) and read their fields by id, sorted.
/// Mirrors what `complete_members` does on the receiver-resolved path.
fn fields_by_record_names(store: &IndexStore, names: &[&str]) -> Vec<String> {
    let candidates = store
        .resolve_record_candidates(names, None)
        .expect("records");
    let ids: Vec<i64> = candidates.iter().map(|c| c.id).collect();
    let mut fields = store.fields_for_records(&ids).expect("fields");
    fields.sort();
    fields
}

/// Like [`fields_by_record_names`] but scoped to a determinate reachable set:
/// resolves under a closed `ReachScope` and keeps only candidates whose
/// defining file is in `scope`, then reads their fields.
fn fields_by_record_names_scoped(
    store: &IndexStore,
    names: &[&str],
    scope: &HashSet<String>,
) -> Vec<String> {
    let reach = crate::reachability::ReachScope {
        files: scope.clone(),
        heuristic_files: Default::default(),
        open: false,
        reason: None,
    };
    let ctx = crate::resolver::ResolveContext {
        current_path: None,
        reach: Some(&reach),
        direct_external_files: None,
    };
    let candidates = store
        .resolve_record_candidates(names, Some(&ctx))
        .expect("records");
    let ids: Vec<i64> = candidates
        .iter()
        .filter(|c| scope.contains(&c.path))
        .map(|c| c.id)
        .collect();
    let mut fields = store.fields_for_records(&ids).expect("fields");
    fields.sort();
    fields
}

/// Test convenience over `fallback_field_candidates`: just the names of the
/// prefix-matched fallback field candidates (drops the tier annotation).
fn field_prefix_names(store: &IndexStore, prefix: &str, limit: usize) -> Vec<String> {
    store
        .fallback_field_candidates(prefix, limit, None)
        .expect("fallback")
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}
