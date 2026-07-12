use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::call_catalog::rows::{anchor_from_row, call_from_row};
use crate::call_catalog::{RelationPage, RelationQueryIndex};
use crate::call_model::{CallSiteFact, CallableAnchor, CoverageSummary, SourceRange};
use crate::call_model::{CallableLocator, RelationDirection, SemanticGeneration, SourcePosition};
use crate::pathing::IndexDbLease;
use crate::store::views::CallFactStoreView;
use crate::store::IndexStore;

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
}

pub struct CallRelationService<'a> {
    handle: &'a CallReadHandle,
    overlays: &'a [FileCallOverlay],
}

#[derive(Debug, Clone)]
pub struct FileCallOverlay {
    pub path: String,
    pub anchors: Vec<CallableAnchor>,
    pub calls: Vec<CallSiteFact>,
}

impl FileCallOverlay {
    pub fn new(
        path: String,
        mut anchors: Vec<CallableAnchor>,
        mut calls: Vec<CallSiteFact>,
    ) -> Self {
        for anchor in &mut anchors {
            anchor.path.clone_from(&path);
        }
        for call in &mut calls {
            call.path.clone_from(&path);
        }
        Self {
            path,
            anchors,
            calls,
        }
    }
}

impl<'a> CallRelationService<'a> {
    pub fn new(handle: &'a CallReadHandle) -> Self {
        Self {
            handle,
            overlays: &[],
        }
    }

    pub fn for_request(handle: &'a CallReadHandle, overlays: &'a [FileCallOverlay]) -> Self {
        Self { handle, overlays }
    }

    pub fn prepare_at(&self, path: &str, position: SourcePosition) -> Result<RelationQueryIndex> {
        let store = IndexStore::open_readonly(self.handle.db.path())?;
        let guard = store.begin_semantic_read(Some(self.handle.generation.0))?;
        let catalog = locator_catalog(
            &guard.store().call_fact_view(),
            self.overlays,
            path,
            position,
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
        let locator_catalog = locator_catalog(&view, self.overlays, path, position)?;
        let entity = locator_catalog
            .entity_at(path, position)
            .context("no callable at requested position")?;
        let key = entity.entity_key.clone();
        let name = entity.name.clone();

        let (catalog, page) = query_resolved(
            &view,
            &key,
            &name,
            direction,
            cursor,
            relation_limit,
            call_site_limit,
            self.overlays,
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
        let mut anchors: Vec<_> = view
            .anchors_by_entity_key(&locator.entity_key)?
            .into_iter()
            .filter(|row| !is_shadowed(self.overlays, &row.path))
            .map(anchor_from_row)
            .collect();
        if anchors.is_empty() {
            anchors = view
                .anchors_by_path(&locator.path)?
                .into_iter()
                .filter(|row| !is_shadowed(self.overlays, &row.path))
                .map(anchor_from_row)
                .collect();
        }
        anchors.extend(
            self.overlays
                .iter()
                .flat_map(|overlay| overlay.anchors.iter())
                .filter(|anchor| {
                    anchor.entity_key == locator.entity_key || anchor.path == locator.path
                })
                .cloned(),
        );
        let coverage = coverage_summary(view.request_coverage()?);
        let locator_catalog =
            RelationQueryIndex::build_from_facts(anchors, Vec::<CallSiteFact>::new(), coverage);
        let entity = locator_catalog
            .resolve_locator(locator)
            .context("callable locator is stale")?;
        let key = entity.entity_key.clone();
        let name = entity.name.clone();
        let (catalog, page) = query_resolved(
            &view,
            &key,
            &name,
            direction,
            cursor,
            relation_limit,
            call_site_limit,
            self.overlays,
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
        let mut anchors: Vec<_> = view
            .anchors_by_entity_key(key)?
            .into_iter()
            .filter(|row| !is_shadowed(self.overlays, &row.path))
            .map(anchor_from_row)
            .collect();
        anchors.extend(
            self.overlays
                .iter()
                .flat_map(|overlay| overlay.anchors.iter())
                .filter(|anchor| anchor.entity_key == key)
                .cloned(),
        );
        let coverage = coverage_summary(view.request_coverage()?);
        let locator_catalog =
            RelationQueryIndex::build_from_facts(anchors, Vec::<CallSiteFact>::new(), coverage);
        let entity = locator_catalog
            .entity(key)
            .context("callable key is stale")?;
        let resolved_key = entity.entity_key.clone();
        let name = entity.name.clone();
        let (catalog, page) = query_resolved(
            &view,
            &resolved_key,
            &name,
            direction,
            cursor,
            relation_limit,
            call_site_limit,
            self.overlays,
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
) -> Result<RelationQueryIndex> {
    let mut path_anchors: Vec<CallableAnchor> = if let Some(overlay) = overlay_for(overlays, path) {
        overlay
            .anchors
            .iter()
            .filter(|anchor| anchor_matches_position(anchor, position))
            .cloned()
            .collect()
    } else {
        view.anchors_at(path, position.line, position.character)?
            .into_iter()
            .map(anchor_from_row)
            .collect()
    };
    let path_calls: Vec<CallSiteFact> = if let Some(overlay) = overlay_for(overlays, path) {
        overlay
            .calls
            .iter()
            .filter(|call| position_in_range(position, call.callee_range))
            .cloned()
            .collect()
    } else {
        view.call_sites_at(path, position.line, position.character)?
            .into_iter()
            .map(call_from_row)
            .collect()
    };
    let names = unique_names(
        path_calls
            .iter()
            .filter_map(|call| call.callee_name.as_ref()),
    );
    let mut lookup_anchors: Vec<_> = view
        .anchors_by_names(&names)?
        .into_iter()
        .filter(|row| !is_shadowed(overlays, &row.path))
        .map(anchor_from_row)
        .collect();
    let name_set: HashSet<&str> = names.iter().map(String::as_str).collect();
    lookup_anchors.extend(
        overlays
            .iter()
            .flat_map(|overlay| overlay.anchors.iter())
            .filter(|anchor| name_set.contains(anchor.name.as_str()))
            .cloned(),
    );
    lookup_anchors.append(&mut path_anchors);
    Ok(RelationQueryIndex::build_from_facts(
        lookup_anchors,
        path_calls,
        coverage_summary(view.request_coverage()?),
    ))
}

fn query_resolved(
    view: &CallFactStoreView<'_>,
    key: &str,
    name: &str,
    direction: RelationDirection,
    cursor: usize,
    relation_limit: usize,
    call_site_limit: usize,
    overlays: &[FileCallOverlay],
) -> Result<(RelationQueryIndex, RelationPage)> {
    let (base_rows, mut scan_limited) = match direction {
        RelationDirection::Incoming => {
            view.call_sites_by_callee_limited(name, DEFAULT_SCANNED_SITE_LIMIT)?
        }
        RelationDirection::Outgoing => {
            view.call_sites_by_caller_limited(key, DEFAULT_SCANNED_SITE_LIMIT)?
        }
    };
    let mut calls: Vec<CallSiteFact> = base_rows
        .into_iter()
        .filter(|row| !is_shadowed(overlays, &row.path))
        .map(call_from_row)
        .collect();
    let overlay_calls: Vec<_> = overlays
        .iter()
        .flat_map(|overlay| overlay.calls.iter())
        .filter(|call| match direction {
            RelationDirection::Incoming => call.callee_name.as_deref() == Some(name),
            RelationDirection::Outgoing => call.caller_entity_key == key,
        })
        .cloned()
        .collect();
    calls.splice(0..0, overlay_calls);
    if calls.len() > DEFAULT_SCANNED_SITE_LIMIT {
        scan_limited = true;
        calls.truncate(DEFAULT_SCANNED_SITE_LIMIT);
    }
    let caller_keys = unique_names(calls.iter().map(|call| &call.caller_entity_key));
    let callee_names = unique_names(calls.iter().filter_map(|call| call.callee_name.as_ref()));
    let caller_set: HashSet<&str> = caller_keys.iter().map(String::as_str).collect();
    let callee_set: HashSet<&str> = callee_names.iter().map(String::as_str).collect();
    let mut anchors: Vec<CallableAnchor> = view
        .anchors_by_entity_keys(&caller_keys)?
        .into_iter()
        .chain(view.anchors_by_names(&callee_names)?)
        .filter(|row| !is_shadowed(overlays, &row.path))
        .map(anchor_from_row)
        .collect();
    anchors.extend(
        overlays
            .iter()
            .flat_map(|overlay| overlay.anchors.iter())
            .filter(|anchor| {
                caller_set.contains(anchor.entity_key.as_str())
                    || callee_set.contains(anchor.name.as_str())
            })
            .cloned(),
    );
    let candidate_limited =
        apply_candidate_expansion_budget(&mut calls, &anchors, DEFAULT_CANDIDATE_EXPANSION_LIMIT);
    let catalog = RelationQueryIndex::build_from_facts(
        anchors,
        calls,
        coverage_summary(view.request_coverage()?),
    );
    let mut page = catalog.relation_page(direction, key, cursor, relation_limit, call_site_limit);
    page.scan_limited = scan_limited;
    page.candidate_limited = candidate_limited;
    Ok((catalog, page))
}

fn apply_candidate_expansion_budget(
    calls: &mut Vec<CallSiteFact>,
    anchors: &[CallableAnchor],
    limit: usize,
) -> bool {
    let mut candidates: HashMap<&str, HashSet<&str>> = HashMap::new();
    for anchor in anchors {
        candidates
            .entry(anchor.name.as_str())
            .or_default()
            .insert(anchor.entity_key.as_str());
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
    use crate::indexer::{index_workspace, IndexOptions};

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
