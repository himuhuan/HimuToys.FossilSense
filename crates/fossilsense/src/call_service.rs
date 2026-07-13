use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::call_catalog::rows::{anchor_from_row, call_from_row};
use crate::call_catalog::{raw_entity_key, RelationPage, RelationQueryIndex};
use crate::call_model::{CallSiteFact, CallableAnchor, CoverageSummary, SourceRange};
use crate::call_model::{CallableLocator, RelationDirection, SemanticGeneration, SourcePosition};
use crate::pathing::IndexDbLease;
use crate::reachability::ReachGraph;
use crate::store::views::CallFactStoreView;
use crate::store::IndexStore;

pub use crate::candidate_service::FileCandidateOverlay as FileCallOverlay;

const DEFAULT_SCANNED_SITE_LIMIT: usize = 8_192;
const DEFAULT_CANDIDATE_EXPANSION_LIMIT: usize = 32_768;

#[derive(Debug, Clone)]
pub struct CallReadHandle {
    db: IndexDbLease,
    pub generation: SemanticGeneration,
}

impl CallReadHandle {
    pub fn at_generation(db_path: PathBuf, generation: SemanticGeneration) -> Self {
        Self {
            db: IndexDbLease::acquire(db_path),
            generation,
        }
    }

    pub fn capture(db_path: PathBuf) -> Result<Self> {
        let store = IndexStore::open_readonly(&db_path)?;
        let guard = store.begin_semantic_read(None)?;
        let generation = SemanticGeneration(guard.generation());
        guard.finish()?;
        Ok(Self {
            db: IndexDbLease::acquire(db_path),
            generation,
        })
    }

    /// Run a typed read against the exact semantic generation captured by
    /// this handle. Candidate and relation requests share this boundary so a
    /// publication that happens mid-request cannot mix durable generations.
    pub(crate) fn read<T>(&self, read: impl FnOnce(&IndexStore) -> Result<T>) -> Result<T> {
        let store = IndexStore::open_readonly(self.db.path())?;
        let guard = store.begin_semantic_read(Some(self.generation.0))?;
        let value = read(guard.store())?;
        guard.finish()?;
        Ok(value)
    }
}

pub struct CallRelationService<'a> {
    handle: &'a CallReadHandle,
    overlays: &'a [FileCallOverlay],
    reach_graph: Option<&'a ReachGraph>,
}

impl<'a> CallRelationService<'a> {
    pub fn new(handle: &'a CallReadHandle) -> Self {
        Self {
            handle,
            overlays: &[],
            reach_graph: None,
        }
    }

    #[cfg(test)]
    pub fn for_request(handle: &'a CallReadHandle, overlays: &'a [FileCallOverlay]) -> Self {
        Self {
            handle,
            overlays,
            reach_graph: None,
        }
    }

    pub fn for_request_with_reach(
        handle: &'a CallReadHandle,
        overlays: &'a [FileCallOverlay],
        reach_graph: Option<&'a ReachGraph>,
    ) -> Self {
        Self {
            handle,
            overlays,
            reach_graph,
        }
    }

    pub fn prepare_at(&self, path: &str, position: SourcePosition) -> Result<RelationQueryIndex> {
        let store = IndexStore::open_readonly(self.handle.db.path())?;
        let guard = store.begin_semantic_read(Some(self.handle.generation.0))?;
        let catalog = locator_catalog(
            &guard.store().call_fact_view(),
            self.overlays,
            path,
            position,
            self.reach_graph,
        )?;
        guard.finish()?;
        Ok(catalog)
    }

    pub fn query_at(
        &self,
        path: &str,
        position: SourcePosition,
        direction: RelationDirection,
        cursor: usize,
        relation_limit: usize,
        call_site_limit: usize,
    ) -> Result<(RelationQueryIndex, String, RelationPage)> {
        let store = IndexStore::open_readonly(self.handle.db.path())?;
        let guard = store.begin_semantic_read(Some(self.handle.generation.0))?;
        let view = guard.store().call_fact_view();
        let locator_catalog =
            locator_catalog(&view, self.overlays, path, position, self.reach_graph)?;
        let entity = locator_catalog
            .entity_at(path, position)
            .context("no callable at requested position")?;
        let key = entity.entity_key.clone();
        let name = entity.name.clone();
        let raw_keys = locator_catalog
            .raw_keys_for_entity(&key)
            .context("callable group lost its parser identity")?
            .to_vec();

        let (catalog, page) = query_resolved(
            &view,
            ResolvedQuery {
                key: &key,
                raw_keys: &raw_keys,
                name: &name,
                direction,
                cursor,
                relation_limit,
                call_site_limit,
                overlays: self.overlays,
                reach_graph: self.reach_graph,
            },
        )?;
        guard.finish()?;
        Ok((catalog, key, page))
    }

    pub fn query_locator(
        &self,
        locator: &CallableLocator,
        direction: RelationDirection,
        cursor: usize,
        relation_limit: usize,
        call_site_limit: usize,
    ) -> Result<(RelationQueryIndex, String, RelationPage)> {
        let store = IndexStore::open_readonly(self.handle.db.path())?;
        let guard = store.begin_semantic_read(Some(self.handle.generation.0))?;
        let view = guard.store().call_fact_view();
        let raw_key = raw_entity_key(&locator.entity_key);
        let mut anchors = Vec::new();
        let mut seen = HashSet::new();
        let mut candidate_recall_limited = append_anchors_bounded(
            &mut anchors,
            &mut seen,
            self.overlays
                .iter()
                .flat_map(|overlay| overlay.anchors.iter())
                .filter(|anchor| anchor.entity_key == raw_key || anchor.path == locator.path)
                .cloned(),
            DEFAULT_CANDIDATE_EXPANSION_LIMIT,
        );
        let remaining = DEFAULT_CANDIDATE_EXPANSION_LIMIT.saturating_sub(anchors.len());
        let (rows, durable_limited) = view.anchors_by_entity_key_limited(raw_key, remaining)?;
        candidate_recall_limited |= durable_limited;
        append_anchors_bounded(
            &mut anchors,
            &mut seen,
            rows.into_iter()
                .filter(|row| !is_shadowed(self.overlays, &row.path))
                .map(anchor_from_row),
            DEFAULT_CANDIDATE_EXPANSION_LIMIT,
        );
        if anchors.is_empty() && !candidate_recall_limited {
            let remaining = DEFAULT_CANDIDATE_EXPANSION_LIMIT.saturating_sub(anchors.len());
            let (rows, path_limited) = view.anchors_by_path_limited(&locator.path, remaining)?;
            candidate_recall_limited |= path_limited;
            append_anchors_bounded(
                &mut anchors,
                &mut seen,
                rows.into_iter()
                    .filter(|row| !is_shadowed(self.overlays, &row.path))
                    .map(anchor_from_row),
                DEFAULT_CANDIDATE_EXPANSION_LIMIT,
            );
        }
        let (anchors, expansion_limited) = expand_anchor_names(&view, self.overlays, anchors)?;
        candidate_recall_limited |= expansion_limited;
        let coverage = coverage_summary(view.request_coverage()?);
        let locator_catalog = RelationQueryIndex::build_from_facts_with_context(
            anchors,
            Vec::<CallSiteFact>::new(),
            coverage,
            self.reach_graph,
            overlays_incomplete(self.overlays),
            candidate_recall_limited,
        );
        let entity = locator_catalog
            .resolve_locator(locator)
            .context("callable locator is stale")?;
        let key = entity.entity_key.clone();
        let name = entity.name.clone();
        let raw_keys = locator_catalog
            .raw_keys_for_entity(&key)
            .context("callable group lost its parser identity")?
            .to_vec();
        let (catalog, page) = query_resolved(
            &view,
            ResolvedQuery {
                key: &key,
                raw_keys: &raw_keys,
                name: &name,
                direction,
                cursor,
                relation_limit,
                call_site_limit,
                overlays: self.overlays,
                reach_graph: self.reach_graph,
            },
        )?;
        guard.finish()?;
        Ok((catalog, key, page))
    }

    pub fn query_key(
        &self,
        key: &str,
        direction: RelationDirection,
        cursor: usize,
        relation_limit: usize,
        call_site_limit: usize,
    ) -> Result<(RelationQueryIndex, String, RelationPage)> {
        let store = IndexStore::open_readonly(self.handle.db.path())?;
        let guard = store.begin_semantic_read(Some(self.handle.generation.0))?;
        let view = guard.store().call_fact_view();
        let raw_key = raw_entity_key(key);
        let mut anchors = Vec::new();
        let mut seen = HashSet::new();
        let mut candidate_recall_limited = append_anchors_bounded(
            &mut anchors,
            &mut seen,
            self.overlays
                .iter()
                .flat_map(|overlay| overlay.anchors.iter())
                .filter(|anchor| anchor.entity_key == raw_key)
                .cloned(),
            DEFAULT_CANDIDATE_EXPANSION_LIMIT,
        );
        let remaining = DEFAULT_CANDIDATE_EXPANSION_LIMIT.saturating_sub(anchors.len());
        let (rows, durable_limited) = view.anchors_by_entity_key_limited(raw_key, remaining)?;
        candidate_recall_limited |= durable_limited;
        append_anchors_bounded(
            &mut anchors,
            &mut seen,
            rows.into_iter()
                .filter(|row| !is_shadowed(self.overlays, &row.path))
                .map(anchor_from_row),
            DEFAULT_CANDIDATE_EXPANSION_LIMIT,
        );
        let (anchors, expansion_limited) = expand_anchor_names(&view, self.overlays, anchors)?;
        candidate_recall_limited |= expansion_limited;
        let coverage = coverage_summary(view.request_coverage()?);
        let locator_catalog = RelationQueryIndex::build_from_facts_with_context(
            anchors,
            Vec::<CallSiteFact>::new(),
            coverage,
            self.reach_graph,
            overlays_incomplete(self.overlays),
            candidate_recall_limited,
        );
        let entity = locator_catalog
            .entity(key)
            .or_else(|| locator_catalog.entity_for_unique_raw_key(raw_key))
            .context("callable key is stale")?;
        let resolved_key = entity.entity_key.clone();
        let name = entity.name.clone();
        let raw_keys = locator_catalog
            .raw_keys_for_entity(&resolved_key)
            .context("callable group lost its parser identity")?
            .to_vec();
        let (catalog, page) = query_resolved(
            &view,
            ResolvedQuery {
                key: &resolved_key,
                raw_keys: &raw_keys,
                name: &name,
                direction,
                cursor,
                relation_limit,
                call_site_limit,
                overlays: self.overlays,
                reach_graph: self.reach_graph,
            },
        )?;
        guard.finish()?;
        Ok((catalog, resolved_key, page))
    }
}

fn locator_catalog(
    view: &CallFactStoreView<'_>,
    overlays: &[FileCallOverlay],
    path: &str,
    position: SourcePosition,
    reach_graph: Option<&ReachGraph>,
) -> Result<RelationQueryIndex> {
    let (path_anchors, mut candidate_recall_limited): (Vec<CallableAnchor>, bool) =
        if let Some(overlay) = overlay_for(overlays, path) {
            let mut anchors: Vec<_> = overlay
                .anchors
                .iter()
                .filter(|anchor| anchor_matches_position(anchor, position))
                .take(DEFAULT_CANDIDATE_EXPANSION_LIMIT.saturating_add(1))
                .cloned()
                .collect();
            let limited = anchors.len() > DEFAULT_CANDIDATE_EXPANSION_LIMIT;
            anchors.truncate(DEFAULT_CANDIDATE_EXPANSION_LIMIT);
            (anchors, limited)
        } else {
            let (rows, limited) = view.anchors_at_limited(
                path,
                position.line,
                position.character,
                DEFAULT_CANDIDATE_EXPANSION_LIMIT,
            )?;
            (rows.into_iter().map(anchor_from_row).collect(), limited)
        };
    let (path_calls, path_calls_limited): (Vec<CallSiteFact>, bool) =
        if let Some(overlay) = overlay_for(overlays, path) {
            let mut calls: Vec<_> = overlay
                .calls
                .iter()
                .filter(|call| position_in_range(position, call.callee_range))
                .take(DEFAULT_SCANNED_SITE_LIMIT.saturating_add(1))
                .cloned()
                .collect();
            let limited = calls.len() > DEFAULT_SCANNED_SITE_LIMIT;
            calls.truncate(DEFAULT_SCANNED_SITE_LIMIT);
            (calls, limited)
        } else {
            let (rows, limited) = view.call_sites_at_limited(
                path,
                position.line,
                position.character,
                DEFAULT_SCANNED_SITE_LIMIT,
            )?;
            (rows.into_iter().map(call_from_row).collect(), limited)
        };
    candidate_recall_limited |= path_calls_limited;
    let names = unique_names(
        path_calls
            .iter()
            .filter_map(|call| call.callee_name.as_ref())
            .chain(path_anchors.iter().map(|anchor| &anchor.name)),
    );
    let mut lookup_anchors = Vec::new();
    let mut seen = HashSet::new();
    candidate_recall_limited |= append_anchors_bounded(
        &mut lookup_anchors,
        &mut seen,
        path_anchors,
        DEFAULT_CANDIDATE_EXPANSION_LIMIT,
    );
    let name_set: HashSet<&str> = names.iter().map(String::as_str).collect();
    candidate_recall_limited |= append_anchors_bounded(
        &mut lookup_anchors,
        &mut seen,
        overlays
            .iter()
            .flat_map(|overlay| overlay.anchors.iter())
            .filter(|anchor| name_set.contains(anchor.name.as_str()))
            .cloned(),
        DEFAULT_CANDIDATE_EXPANSION_LIMIT,
    );
    let remaining = DEFAULT_CANDIDATE_EXPANSION_LIMIT.saturating_sub(lookup_anchors.len());
    let (rows, durable_limited) = view.anchors_by_names_limited(&names, remaining)?;
    candidate_recall_limited |= durable_limited;
    candidate_recall_limited |= append_anchors_bounded(
        &mut lookup_anchors,
        &mut seen,
        rows.into_iter()
            .filter(|row| !is_shadowed(overlays, &row.path))
            .map(anchor_from_row),
        DEFAULT_CANDIDATE_EXPANSION_LIMIT,
    );
    Ok(RelationQueryIndex::build_from_facts_with_context(
        lookup_anchors,
        path_calls,
        coverage_summary(view.request_coverage()?),
        reach_graph,
        overlays_incomplete(overlays),
        candidate_recall_limited,
    ))
}

struct ResolvedQuery<'a> {
    key: &'a str,
    raw_keys: &'a [String],
    name: &'a str,
    direction: RelationDirection,
    cursor: usize,
    relation_limit: usize,
    call_site_limit: usize,
    overlays: &'a [FileCallOverlay],
    reach_graph: Option<&'a ReachGraph>,
}

fn query_resolved(
    view: &CallFactStoreView<'_>,
    query: ResolvedQuery<'_>,
) -> Result<(RelationQueryIndex, RelationPage)> {
    let ResolvedQuery {
        key,
        raw_keys,
        name,
        direction,
        cursor,
        relation_limit,
        call_site_limit,
        overlays,
        reach_graph,
    } = query;
    let (base_rows, mut scan_limited) = match direction {
        RelationDirection::Incoming => {
            view.call_sites_by_callee_limited(name, DEFAULT_SCANNED_SITE_LIMIT)?
        }
        RelationDirection::Outgoing => {
            let mut rows = Vec::new();
            let mut limited = false;
            for raw_key in raw_keys {
                let remaining = DEFAULT_SCANNED_SITE_LIMIT.saturating_sub(rows.len());
                if remaining == 0 {
                    limited = true;
                    break;
                }
                let (mut next, next_limited) =
                    view.call_sites_by_caller_limited(raw_key, remaining)?;
                rows.append(&mut next);
                limited |= next_limited;
            }
            (rows, limited)
        }
    };
    let mut calls: Vec<CallSiteFact> = base_rows
        .into_iter()
        .filter(|row| !is_shadowed(overlays, &row.path))
        .map(call_from_row)
        .collect();
    let mut overlay_calls: Vec<_> = overlays
        .iter()
        .flat_map(|overlay| overlay.calls.iter())
        .filter(|call| match direction {
            RelationDirection::Incoming => call.callee_name.as_deref() == Some(name),
            RelationDirection::Outgoing => raw_keys.contains(&call.caller_entity_key),
        })
        .take(DEFAULT_SCANNED_SITE_LIMIT.saturating_add(1))
        .cloned()
        .collect();
    if overlay_calls.len() > DEFAULT_SCANNED_SITE_LIMIT {
        scan_limited = true;
        overlay_calls.truncate(DEFAULT_SCANNED_SITE_LIMIT);
    }
    calls.splice(0..0, overlay_calls);
    if calls.len() > DEFAULT_SCANNED_SITE_LIMIT {
        scan_limited = true;
        calls.truncate(DEFAULT_SCANNED_SITE_LIMIT);
    }
    let mut caller_keys = unique_names(calls.iter().map(|call| &call.caller_entity_key));
    for raw_key in raw_keys {
        if !caller_keys.contains(raw_key) {
            caller_keys.push(raw_key.clone());
        }
    }
    let mut callee_names = unique_names(calls.iter().filter_map(|call| call.callee_name.as_ref()));
    if direction == RelationDirection::Incoming && !callee_names.iter().any(|value| value == name) {
        callee_names.push(name.to_string());
    }
    let caller_set: HashSet<&str> = caller_keys.iter().map(String::as_str).collect();
    let callee_set: HashSet<&str> = callee_names.iter().map(String::as_str).collect();
    let mut anchors = Vec::new();
    let mut seen = HashSet::new();
    let mut candidate_recall_limited = append_anchors_bounded(
        &mut anchors,
        &mut seen,
        overlays
            .iter()
            .flat_map(|overlay| overlay.anchors.iter())
            .filter(|anchor| {
                caller_set.contains(anchor.entity_key.as_str())
                    || callee_set.contains(anchor.name.as_str())
            })
            .cloned(),
        DEFAULT_CANDIDATE_EXPANSION_LIMIT,
    );
    let remaining = DEFAULT_CANDIDATE_EXPANSION_LIMIT.saturating_sub(anchors.len());
    let (caller_rows, caller_limited) =
        view.anchors_by_entity_keys_limited(&caller_keys, remaining)?;
    candidate_recall_limited |= caller_limited;
    candidate_recall_limited |= append_anchors_bounded(
        &mut anchors,
        &mut seen,
        caller_rows
            .into_iter()
            .filter(|row| !is_shadowed(overlays, &row.path))
            .map(anchor_from_row),
        DEFAULT_CANDIDATE_EXPANSION_LIMIT,
    );
    let remaining = DEFAULT_CANDIDATE_EXPANSION_LIMIT.saturating_sub(anchors.len());
    let (callee_rows, callee_limited) = view.anchors_by_names_limited(&callee_names, remaining)?;
    candidate_recall_limited |= callee_limited;
    candidate_recall_limited |= append_anchors_bounded(
        &mut anchors,
        &mut seen,
        callee_rows
            .into_iter()
            .filter(|row| !is_shadowed(overlays, &row.path))
            .map(anchor_from_row),
        DEFAULT_CANDIDATE_EXPANSION_LIMIT,
    );
    let candidate_expansion_limited =
        apply_candidate_expansion_budget(&mut calls, &anchors, DEFAULT_CANDIDATE_EXPANSION_LIMIT);
    let catalog = RelationQueryIndex::build_from_facts_with_context(
        anchors,
        calls,
        coverage_summary(view.request_coverage()?),
        reach_graph,
        overlays_incomplete(overlays),
        candidate_recall_limited,
    );
    let mut page = catalog.relation_page(direction, key, cursor, relation_limit, call_site_limit);
    page.scan_limited = scan_limited;
    page.candidate_limited |= candidate_expansion_limited;
    Ok((catalog, page))
}

fn apply_candidate_expansion_budget(
    calls: &mut Vec<CallSiteFact>,
    anchors: &[CallableAnchor],
    limit: usize,
) -> bool {
    // Parser entity_key is only a signature-family hint: a 1:N/N:1 family
    // becomes several conservative relation entities. Count concrete anchors
    // here (strict pairs may be conservatively over-counted) so a shared raw
    // key cannot bypass the expansion cap.
    let mut candidates: HashMap<&str, HashSet<(&str, &str, usize)>> = HashMap::new();
    for anchor in anchors {
        candidates.entry(anchor.name.as_str()).or_default().insert((
            anchor.path.as_str(),
            anchor.anchor_fingerprint.as_str(),
            anchor.name_range.start_byte,
        ));
    }
    let mut expansions = 0usize;
    let mut keep = 0usize;
    for call in calls.iter() {
        let cost = call
            .callee_name
            .as_deref()
            .and_then(|name| candidates.get(name))
            .map_or(1, |entities| entities.len().max(1));
        if expansions.saturating_add(cost) > limit {
            break;
        }
        expansions = expansions.saturating_add(cost);
        keep += 1;
    }
    let limited = keep < calls.len();
    calls.truncate(keep);
    limited
}

fn expand_anchor_names(
    view: &CallFactStoreView<'_>,
    overlays: &[FileCallOverlay],
    anchors: Vec<CallableAnchor>,
) -> Result<(Vec<CallableAnchor>, bool)> {
    let mut expanded = Vec::new();
    let mut seen = HashSet::new();
    let mut candidate_limited = append_anchors_bounded(
        &mut expanded,
        &mut seen,
        anchors,
        DEFAULT_CANDIDATE_EXPANSION_LIMIT,
    );
    let names = unique_names(expanded.iter().map(|anchor| &anchor.name));
    let name_set: HashSet<_> = names.iter().map(String::as_str).collect();
    candidate_limited |= append_anchors_bounded(
        &mut expanded,
        &mut seen,
        overlays
            .iter()
            .flat_map(|overlay| overlay.anchors.iter())
            .filter(|anchor| name_set.contains(anchor.name.as_str()))
            .cloned(),
        DEFAULT_CANDIDATE_EXPANSION_LIMIT,
    );
    let remaining = DEFAULT_CANDIDATE_EXPANSION_LIMIT.saturating_sub(expanded.len());
    let (rows, durable_limited) = view.anchors_by_names_limited(&names, remaining)?;
    candidate_limited |= durable_limited;
    candidate_limited |= append_anchors_bounded(
        &mut expanded,
        &mut seen,
        rows.into_iter()
            .filter(|row| !is_shadowed(overlays, &row.path))
            .map(anchor_from_row),
        DEFAULT_CANDIDATE_EXPANSION_LIMIT,
    );
    Ok((expanded, candidate_limited))
}

fn append_anchors_bounded(
    output: &mut Vec<CallableAnchor>,
    seen: &mut HashSet<(String, String, usize)>,
    anchors: impl IntoIterator<Item = CallableAnchor>,
    limit: usize,
) -> bool {
    for anchor in anchors {
        let identity = (
            anchor.path.clone(),
            anchor.anchor_fingerprint.clone(),
            anchor.name_range.start_byte,
        );
        if seen.contains(&identity) {
            continue;
        }
        if output.len() >= limit {
            return true;
        }
        seen.insert(identity);
        output.push(anchor);
    }
    false
}

fn overlays_incomplete(overlays: &[FileCallOverlay]) -> bool {
    overlays.iter().any(|overlay| !overlay.facts_complete)
}

fn overlay_for<'a>(overlays: &'a [FileCallOverlay], path: &str) -> Option<&'a FileCallOverlay> {
    overlays.iter().find(|overlay| overlay.path == path)
}

fn is_shadowed(overlays: &[FileCallOverlay], path: &str) -> bool {
    overlay_for(overlays, path).is_some()
}

fn anchor_matches_position(anchor: &CallableAnchor, position: SourcePosition) -> bool {
    position_in_range(position, anchor.name_range)
        || position_in_range(position, anchor.declaration_range)
        || anchor
            .body_range
            .is_some_and(|range| position_in_range(position, range))
}

fn position_in_range(position: SourcePosition, range: SourceRange) -> bool {
    (position.line, position.character) >= (range.start.line, range.start.character)
        && (position.line, position.character) <= (range.end.line, range.end.character)
}

fn coverage_summary(row: crate::store::views::CallCoverageRow) -> CoverageSummary {
    CoverageSummary {
        eligible_files: row.eligible_files,
        analyzed_files: row.analyzed_files,
        fallback_files: row.fallback_files,
        external_bodies_limited: true,
    }
}

fn unique_names<'a>(values: impl Iterator<Item = &'a String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .filter(|value| seen.insert((*value).clone()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate_service::CandidateOverlaySnapshot;
    use crate::indexer::{index_workspace, IndexOptions};
    use std::sync::Arc;

    #[test]
    fn lazy_store_query_and_overlay_merge_preserve_expected_relation() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            workspace.join("target.c"),
            "int target(int value) { return value + 1; }\n",
        )
        .unwrap();
        std::fs::write(
            workspace.join("caller.c"),
            "int target(int);\nint caller(void) { return target(1); }\n",
        )
        .unwrap();
        let db_path = temp.path().join("index.sqlite");
        index_workspace(
            &workspace,
            IndexOptions {
                db_path: Some(db_path.clone()),
                force: true,
                ..Default::default()
            },
            |_| {},
        )
        .unwrap();

        let handle = CallReadHandle::capture(db_path).unwrap();
        let service = CallRelationService::new(&handle);
        let prepared = service
            .prepare_at(
                "target.c",
                SourcePosition {
                    line: 0,
                    character: 5,
                },
            )
            .unwrap();
        let root = prepared
            .entity_at(
                "target.c",
                SourcePosition {
                    line: 0,
                    character: 5,
                },
            )
            .unwrap();
        let (_, _, actual) = service
            .query_at(
                "target.c",
                SourcePosition {
                    line: 0,
                    character: 5,
                },
                RelationDirection::Incoming,
                0,
                100,
                100,
            )
            .unwrap();

        assert_eq!(actual.total, 1);
        assert_eq!(actual.relations.len(), 1);
        assert_eq!(actual.relations[0].caller.name, "caller");
        assert_eq!(
            actual.relations[0]
                .callee
                .as_ref()
                .expect("resolved target")
                .entity_key,
            root.entity_key
        );

        let clean_caller_replacement = crate::parser::parse(
            &workspace.join("caller.c"),
            "int target(int);\nint caller(void) { return 0; }\n",
        );
        let shadow = FileCallOverlay::new(
            "caller.c".into(),
            clean_caller_replacement.callable_anchors,
            clean_caller_replacement.call_sites,
        );
        let (_, _, shadowed_page) = CallRelationService::for_request(&handle, &[shadow])
            .query_at(
                "target.c",
                SourcePosition {
                    line: 0,
                    character: 5,
                },
                RelationDirection::Incoming,
                0,
                100,
                100,
            )
            .unwrap();
        assert_eq!(shadowed_page.total, 0, "dirty path must shadow base facts");

        let dirty_other = crate::parser::parse(
            &workspace.join("other.c"),
            "int target(int);\nint other(void) { return target(2); }\n",
        );
        let other = FileCallOverlay::new(
            "other.c".into(),
            dirty_other.callable_anchors,
            dirty_other.call_sites,
        );
        let (_, _, merged_page) = CallRelationService::for_request(&handle, &[other])
            .query_at(
                "target.c",
                SourcePosition {
                    line: 0,
                    character: 5,
                },
                RelationDirection::Incoming,
                0,
                100,
                100,
            )
            .unwrap();
        assert_eq!(
            merged_page.total, 2,
            "incoming must merge calls from other dirty documents"
        );
    }

    #[test]
    fn lazy_query_marks_raw_scan_budget_without_counting_the_full_tail() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("target.c"), "void target(void) {}\n").unwrap();
        let mut caller = String::from("void target(void);\nvoid caller(void) {");
        for _ in 0..(DEFAULT_SCANNED_SITE_LIMIT + 16) {
            caller.push_str("target();");
        }
        caller.push_str("}\n");
        std::fs::write(workspace.join("caller.c"), caller).unwrap();
        let db_path = temp.path().join("index.sqlite");
        index_workspace(
            &workspace,
            IndexOptions {
                db_path: Some(db_path.clone()),
                force: true,
                ..Default::default()
            },
            |_| {},
        )
        .unwrap();

        let handle = CallReadHandle::capture(db_path).unwrap();
        let (catalog, _, page) = CallRelationService::new(&handle)
            .query_at(
                "target.c",
                SourcePosition {
                    line: 0,
                    character: 6,
                },
                RelationDirection::Incoming,
                0,
                100,
                100,
            )
            .unwrap();
        assert!(page.scan_limited);
        assert_eq!(catalog.stats().call_sites, DEFAULT_SCANNED_SITE_LIMIT);
        assert!(page.total < DEFAULT_SCANNED_SITE_LIMIT + 16);
    }

    #[test]
    fn dirty_overlay_call_scan_is_bounded_before_relation_materialization() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("target.c"), "void target(void) {}\n").unwrap();
        let db_path = temp.path().join("index.sqlite");
        index_workspace(
            &workspace,
            IndexOptions {
                db_path: Some(db_path.clone()),
                force: true,
                ..Default::default()
            },
            |_| {},
        )
        .unwrap();

        let parsed = crate::parser::parse(
            std::path::Path::new("dirty.c"),
            "void target(void); void caller(void) { target(); }\n",
        );
        let template = parsed.call_sites[0].clone();
        let calls = (0..DEFAULT_SCANNED_SITE_LIMIT + 16)
            .map(|index| {
                let mut call = template.clone();
                call.site_fingerprint = format!("dirty-site-{index}");
                call
            })
            .collect();
        let overlay = FileCallOverlay::new("dirty.c".into(), parsed.callable_anchors, calls);
        let handle = CallReadHandle::capture(db_path).unwrap();
        let (catalog, _, page) = CallRelationService::for_request(&handle, &[overlay])
            .query_at(
                "target.c",
                SourcePosition {
                    line: 0,
                    character: 6,
                },
                RelationDirection::Incoming,
                0,
                100,
                100,
            )
            .unwrap();

        assert!(page.scan_limited);
        assert_eq!(catalog.stats().call_sites, DEFAULT_SCANNED_SITE_LIMIT);
    }

    #[test]
    fn strict_counterpart_group_keeps_source_body_as_primary() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("api.h"), "int target(int value);\n").unwrap();
        std::fs::write(
            workspace.join("impl.c"),
            "#include \"api.h\"\nint target(int value) { return value; }\n",
        )
        .unwrap();
        let db_path = temp.path().join("index.sqlite");
        index_workspace(
            &workspace,
            IndexOptions {
                db_path: Some(db_path.clone()),
                force: true,
                ..Default::default()
            },
            |_| {},
        )
        .unwrap();

        let graph = ReachGraph::new(vec![("impl.c".into(), "api.h".into())], vec![], vec![]);
        let handle = CallReadHandle::capture(db_path).unwrap();
        let catalog = CallRelationService::for_request_with_reach(&handle, &[], Some(&graph))
            .prepare_at(
                "impl.c",
                SourcePosition {
                    line: 1,
                    character: 5,
                },
            )
            .unwrap();
        let entity = catalog
            .entity_at(
                "impl.c",
                SourcePosition {
                    line: 1,
                    character: 5,
                },
            )
            .unwrap();
        assert_eq!(entity.variants.len(), 2);
        assert_eq!(entity.primary_anchor.path, "impl.c");
        assert!(entity.primary_anchor.body_range.is_some());

        let (catalog, key, page) =
            CallRelationService::for_request_with_reach(&handle, &[], Some(&graph))
                .query_at(
                    "impl.c",
                    SourcePosition {
                        line: 1,
                        character: 5,
                    },
                    RelationDirection::Incoming,
                    0,
                    100,
                    100,
                )
                .unwrap();
        assert_eq!(page.total, 0);
        assert_eq!(catalog.entity(&key).unwrap().variants.len(), 2);
    }

    #[test]
    fn dirty_include_graph_retargets_pair_and_cancelled_tombstone_disables_uniqueness() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        for header in ["old.h", "new.h"] {
            std::fs::write(workspace.join(header), "int target(int value);\n").unwrap();
        }
        std::fs::write(
            workspace.join("impl.c"),
            "#include \"old.h\"\nint target(int value) { return value; }\n",
        )
        .unwrap();
        let db_path = temp.path().join("index.sqlite");
        index_workspace(
            &workspace,
            IndexOptions {
                db_path: Some(db_path.clone()),
                force: true,
                ..Default::default()
            },
            |_| {},
        )
        .unwrap();
        let handle = CallReadHandle::capture(db_path).unwrap();

        let base = ReachGraph::new(vec![("impl.c".into(), "old.h".into())], vec![], vec![]);
        let dirty_text: Arc<str> =
            Arc::from("#include \"new.h\"\nint target(int value) { return value + 1; }\n");
        let dirty = crate::parser::parse_with_handle(
            std::path::Path::new("impl.c"),
            &dirty_text,
            None,
            crate::parser::ParseFacts::HOVER_SEMANTICS,
        );
        let mut snapshot = CandidateOverlaySnapshot::new(
            1,
            vec![FileCallOverlay::from_index_with_text(
                "impl.c".into(),
                &dirty,
                dirty_text,
            )],
        );
        snapshot.refresh_reach_graph(Some(&base), ["impl.c", "old.h", "new.h"], &[]);
        let reach = snapshot.effective_reach_graph_arc(None).unwrap();
        let overlays = snapshot.call_relation_overlays();
        let catalog =
            CallRelationService::for_request_with_reach(&handle, &overlays, Some(reach.as_ref()))
                .prepare_at(
                    "impl.c",
                    SourcePosition {
                        line: 1,
                        character: 5,
                    },
                )
                .unwrap();
        let entity = catalog
            .entity_at(
                "impl.c",
                SourcePosition {
                    line: 1,
                    character: 5,
                },
            )
            .unwrap();
        let variant_paths: HashSet<_> = entity
            .variants
            .iter()
            .map(|anchor| anchor.path.as_str())
            .collect();
        assert_eq!(variant_paths, HashSet::from(["impl.c", "new.h"]));

        let base = ReachGraph::new(
            vec![
                ("impl.c".into(), "old.h".into()),
                ("impl.c".into(), "new.h".into()),
            ],
            vec![],
            vec![],
        );
        let mut snapshot = CandidateOverlaySnapshot::new(
            2,
            vec![FileCallOverlay::tombstone(
                "new.h".into(),
                Arc::from("int target(int value);\n"),
            )],
        );
        snapshot.refresh_reach_graph(Some(&base), ["impl.c", "old.h", "new.h"], &[]);
        let reach = snapshot.effective_reach_graph_arc(None).unwrap();
        let overlays = snapshot.call_relation_overlays();
        let catalog =
            CallRelationService::for_request_with_reach(&handle, &overlays, Some(reach.as_ref()))
                .prepare_at(
                    "impl.c",
                    SourcePosition {
                        line: 1,
                        character: 5,
                    },
                )
                .unwrap();
        let source = catalog
            .entity_at(
                "impl.c",
                SourcePosition {
                    line: 1,
                    character: 5,
                },
            )
            .expect("a tombstone in another dirty file must not erase the source callable");
        assert_eq!(
            source.variants.len(),
            1,
            "incomplete overlay coverage must not prove a now-apparently unique pair"
        );
    }

    #[test]
    fn candidate_budget_prevents_high_ambiguity_cross_product() {
        let target =
            crate::parser::parse(std::path::Path::new("target.c"), "void shared(void) {}\n");
        let caller = crate::parser::parse(
            std::path::Path::new("caller.c"),
            "void shared(void); void caller(void) { shared(); }\n",
        );
        let caller_anchor = caller
            .callable_anchors
            .iter()
            .find(|anchor| anchor.name == "caller")
            .unwrap()
            .clone();
        let template_target = target.callable_anchors[0].clone();
        let template_call = caller.call_sites[0].clone();
        let mut anchors = vec![caller_anchor];
        for index in 0..64 {
            let mut anchor = template_target.clone();
            anchor.entity_key = format!("target-{index}");
            anchor.anchor_fingerprint = format!("anchor-{index}");
            anchor.path = format!("target-{index}.c");
            anchors.push(anchor);
        }
        let mut calls: Vec<_> = (0..10_000)
            .map(|index| {
                let mut call = template_call.clone();
                call.site_fingerprint = format!("site-{index}");
                call
            })
            .collect();

        assert!(apply_candidate_expansion_budget(
            &mut calls,
            &anchors,
            DEFAULT_CANDIDATE_EXPANSION_LIMIT,
        ));
        assert_eq!(calls.len(), DEFAULT_CANDIDATE_EXPANSION_LIMIT / 64);
        let started = std::time::Instant::now();
        let catalog =
            RelationQueryIndex::build_from_facts(anchors, calls, CoverageSummary::default());
        let elapsed_ms = started.elapsed().as_millis();
        assert!(catalog.stats().relations <= DEFAULT_CANDIDATE_EXPANSION_LIMIT);
        eprintln!(
            "high_ambiguity_budget candidates=64 raw_calls=10000 expanded_relations={} build_ms={elapsed_ms}",
            catalog.stats().relations
        );
    }
}
