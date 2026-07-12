use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tower_lsp::lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyItem, CallHierarchyOutgoingCall, Position, Range,
    SymbolKind, Url,
};

use super::{uri_to_path, Backend};
use crate::call_catalog::{RelationPage, RelationQueryIndex};
use crate::call_model::{
    BudgetState, CallRelation, CallableEntity, CallableLocator, CoverageSummary, RelationDirection,
    RelationRevision, SourcePosition, SourceRange, RELATION_PROTOCOL_VERSION,
};
use crate::call_service::{CallReadHandle, CallRelationService, FileCallOverlay};
use crate::pathing;
use crate::reachability::ReachGraph;

const STANDARD_RELATION_LIMIT: usize = 500;
const STANDARD_CALL_SITE_LIMIT: usize = 200;
const RICH_RELATION_PAGE_SIZE: usize = 200;
const CALL_SITE_LIMIT: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ItemData {
    entity_key: String,
    locator: CallableLocator,
}

pub(super) struct RelationRequestState {
    pub(super) root: PathBuf,
    pub(super) handle: Arc<CallReadHandle>,
    overlays: Arc<Vec<FileCallOverlay>>,
    pub(super) revision: RelationRevision,
    reach_graph: Option<std::sync::Arc<ReachGraph>>,
}

impl RelationRequestState {
    async fn relation_page_at(
        &self,
        path: &str,
        position: SourcePosition,
        direction: RelationDirection,
        cursor: usize,
        relation_limit: usize,
        call_site_limit: usize,
    ) -> anyhow::Result<(RelationQueryIndex, String, RelationPage)> {
        let handle = self.handle.clone();
        let overlays = self.overlays.clone();
        let path = path.to_string();
        let (catalog, key, mut page) = tokio::task::spawn_blocking(move || {
            CallRelationService::for_request(&handle, &overlays).query_at(
                &path,
                position,
                direction,
                cursor,
                relation_limit,
                call_site_limit,
            )
        })
        .await??;
        self.with_reachability(&mut page.relations);
        Ok((catalog, key, page))
    }

    async fn relation_page_by_key(
        &self,
        key: &str,
        direction: RelationDirection,
        cursor: usize,
        relation_limit: usize,
        call_site_limit: usize,
    ) -> anyhow::Result<(RelationQueryIndex, String, RelationPage)> {
        let handle = self.handle.clone();
        let overlays = self.overlays.clone();
        let key = key.to_string();
        let (catalog, key, mut page) = tokio::task::spawn_blocking(move || {
            CallRelationService::for_request(&handle, &overlays).query_key(
                &key,
                direction,
                cursor,
                relation_limit,
                call_site_limit,
            )
        })
        .await??;
        self.with_reachability(&mut page.relations);
        Ok((catalog, key, page))
    }

    async fn relation_page_by_locator(
        &self,
        locator: &CallableLocator,
        direction: RelationDirection,
        cursor: usize,
        relation_limit: usize,
        call_site_limit: usize,
    ) -> anyhow::Result<(RelationQueryIndex, String, RelationPage)> {
        let handle = self.handle.clone();
        let overlays = self.overlays.clone();
        let locator = locator.clone();
        let (catalog, key, mut page) = tokio::task::spawn_blocking(move || {
            CallRelationService::for_request(&handle, &overlays).query_locator(
                &locator,
                direction,
                cursor,
                relation_limit,
                call_site_limit,
            )
        })
        .await??;
        self.with_reachability(&mut page.relations);
        Ok((catalog, key, page))
    }

    fn with_reachability(&self, relations: &mut [CallRelation]) {
        let Some(graph) = &self.reach_graph else {
            return;
        };
        for relation in relations {
            let Some(callee) = &relation.callee else {
                continue;
            };
            let scope = graph.reachable(&relation.caller.primary_anchor.path);
            if scope.files.contains(&callee.primary_anchor.path) {
                if !relation
                    .evidence
                    .supports
                    .contains(&crate::call_model::EvidenceCode::ReachableDeclaration)
                {
                    relation
                        .evidence
                        .supports
                        .push(crate::call_model::EvidenceCode::ReachableDeclaration);
                }
                if relation.confidence == crate::call_model::RelationConfidence::Low {
                    relation.confidence = crate::call_model::RelationConfidence::Medium;
                }
            } else if scope.open
                && !relation
                    .evidence
                    .unknowns
                    .contains(&crate::call_model::EvidenceCode::OpenIncludeScope)
            {
                relation
                    .evidence
                    .unknowns
                    .push(crate::call_model::EvidenceCode::OpenIncludeScope);
            }
        }
    }
}

impl Backend {
    pub(super) async fn rich_relations_command(&self, arg: &Value) -> Option<Value> {
        let uri = arg
            .get("uri")
            .and_then(Value::as_str)
            .and_then(|value| Url::parse(value).ok())?;
        let state = self.relation_state_for_uri(&uri).await?;
        let direction = match arg.get("direction").and_then(Value::as_str) {
            Some("incoming") => RelationDirection::Incoming,
            _ => RelationDirection::Outgoing,
        };
        let cursor = match arg.get("cursor") {
            Some(Value::String(cursor)) => decode_cursor(cursor, state.revision, direction)?,
            Some(_) => return None,
            None => 0,
        };
        let (catalog, _, page) = if let Some(key) = arg.get("entityKey").and_then(Value::as_str) {
            state
                .relation_page_by_key(
                    key,
                    direction,
                    cursor,
                    RICH_RELATION_PAGE_SIZE,
                    CALL_SITE_LIMIT,
                )
                .await
                .ok()?
        } else {
            let line = arg.get("line").and_then(Value::as_u64).unwrap_or(0) as u32;
            let character = arg.get("character").and_then(Value::as_u64).unwrap_or(0) as u32;
            let path = uri_to_path(&uri)?;
            let rel = catalog_path(&state.root, &path)?;
            state
                .relation_page_at(
                    &rel,
                    SourcePosition { line, character },
                    direction,
                    cursor,
                    RICH_RELATION_PAGE_SIZE,
                    CALL_SITE_LIMIT,
                )
                .await
                .ok()?
        };
        Some(RichRelationResponse::to_value(
            state.revision,
            page,
            catalog.coverage().clone(),
            cursor,
        ))
    }

    pub(super) async fn relation_state_for_uri(&self, uri: &Url) -> Option<RelationRequestState> {
        let root = self.root_for_uri(uri).await?;
        let context = self.request_context_for_root(root.clone()).await;
        let handle = context.engine.call_read_handle.clone()?;
        let mut snapshots: Vec<_> = self
            .session
            .documents
            .all_snapshots()
            .await
            .into_iter()
            .filter(|(document_uri, snapshot)| {
                snapshot.needs_relation_overlay(context.engine.semantic_generation)
                    && uri_to_path(document_uri)
                        .is_some_and(|path| pathing::path_is_within(&root, &path))
            })
            .collect();
        snapshots.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        let mut hasher = DefaultHasher::new();
        for (document_uri, snapshot) in &snapshots {
            document_uri.as_str().hash(&mut hasher);
            snapshot.version.hash(&mut hasher);
        }
        let overlay_epoch = if snapshots.is_empty() {
            0
        } else {
            hasher.finish()
        };
        let mut overlays = Vec::with_capacity(snapshots.len());
        for (document_uri, snapshot) in snapshots {
            let absolute = uri_to_path(&document_uri)?;
            let rel = pathing::relative_slash_path(&root, &absolute).ok()?;
            let parsed = self
                .get_or_parse_document(
                    &document_uri,
                    &absolute,
                    snapshot.version,
                    &snapshot.text,
                    crate::parser::ParseFacts::CALL_RELATIONS,
                )
                .await?;
            overlays.push(FileCallOverlay::new(
                rel,
                parsed.callable_anchors.clone(),
                parsed.call_sites.clone(),
            ));
        }
        Some(RelationRequestState {
            root,
            handle,
            overlays: Arc::new(overlays),
            revision: RelationRevision {
                engine_epoch: context.engine.epoch.as_u64(),
                semantic_generation: context.engine.semantic_generation,
                overlay_epoch,
                resolver_version: RELATION_PROTOCOL_VERSION,
            },
            reach_graph: context.engine.reach_graph.clone(),
        })
    }

    pub(super) async fn prepare_call_items(
        &self,
        uri: &Url,
        position: Position,
    ) -> Option<Vec<CallHierarchyItem>> {
        let state = self.relation_state_for_uri(uri).await?;
        let path = uri_to_path(uri)?;
        let rel = catalog_path(&state.root, &path)?;
        let handle = state.handle.clone();
        let overlays = state.overlays.clone();
        let prepare_position = SourcePosition {
            line: position.line,
            character: position.character,
        };
        let prepare_rel = rel.clone();
        let catalog = tokio::task::spawn_blocking(move || {
            CallRelationService::for_request(&handle, &overlays)
                .prepare_at(&prepare_rel, prepare_position)
        })
        .await
        .ok()?
        .ok()?;
        let entities = catalog.entities_at(
            &rel,
            SourcePosition {
                line: position.line,
                character: position.character,
            },
        );
        let items: Vec<_> = entities
            .into_iter()
            .filter_map(|entity| entity_to_item(&state.root, entity))
            .collect();
        (!items.is_empty()).then_some(items)
    }

    pub(super) async fn standard_incoming(
        &self,
        item: &CallHierarchyItem,
    ) -> Option<Vec<CallHierarchyIncomingCall>> {
        let state = self.relation_state_for_uri(&item.uri).await?;
        let data = serde_json::from_value::<ItemData>(item.data.clone()?).ok()?;
        let (_, _, page) = state
            .relation_page_by_locator(
                &data.locator,
                RelationDirection::Incoming,
                0,
                STANDARD_RELATION_LIMIT,
                STANDARD_CALL_SITE_LIMIT,
            )
            .await
            .ok()?;
        Some(
            page.relations
                .into_iter()
                .filter_map(|relation| {
                    Some(CallHierarchyIncomingCall {
                        from: entity_to_item(&state.root, &relation.caller)?,
                        from_ranges: relation
                            .call_sites
                            .iter()
                            .map(|site| source_range(site.callee_range))
                            .collect(),
                    })
                })
                .collect(),
        )
    }

    pub(super) async fn standard_outgoing(
        &self,
        item: &CallHierarchyItem,
    ) -> Option<Vec<CallHierarchyOutgoingCall>> {
        let state = self.relation_state_for_uri(&item.uri).await?;
        let data = serde_json::from_value::<ItemData>(item.data.clone()?).ok()?;
        let (_, _, page) = state
            .relation_page_by_locator(
                &data.locator,
                RelationDirection::Outgoing,
                0,
                STANDARD_RELATION_LIMIT,
                STANDARD_CALL_SITE_LIMIT,
            )
            .await
            .ok()?;
        Some(
            page.relations
                .into_iter()
                .filter_map(|relation| {
                    Some(CallHierarchyOutgoingCall {
                        to: entity_to_item(&state.root, relation.callee.as_ref()?)?,
                        from_ranges: relation
                            .call_sites
                            .iter()
                            .map(|site| source_range(site.callee_range))
                            .collect(),
                    })
                })
                .collect(),
        )
    }
}

pub(super) fn entity_to_item(root: &Path, entity: &CallableEntity) -> Option<CallHierarchyItem> {
    let anchor = &entity.primary_anchor;
    let path = if Path::new(&anchor.path).is_absolute() {
        PathBuf::from(&anchor.path)
    } else {
        root.join(anchor.path.replace('/', std::path::MAIN_SEPARATOR_STR))
    };
    Some(CallHierarchyItem {
        name: entity.name.clone(),
        kind: SymbolKind::FUNCTION,
        tags: None,
        detail: Some(entity.signature.normalized.clone()),
        uri: Url::from_file_path(path).ok()?,
        range: source_range(anchor.declaration_range),
        selection_range: source_range(anchor.name_range),
        data: serde_json::to_value(ItemData {
            entity_key: entity.entity_key.clone(),
            locator: CallableLocator {
                workspace_id: pathing::workspace_hash(root),
                path: anchor.path.clone(),
                entity_key: entity.entity_key.clone(),
                anchor_fingerprint: anchor.anchor_fingerprint.clone(),
                old_start_byte: anchor.name_range.start_byte,
                signature_digest: crate::call_catalog::signature_digest(&entity.signature),
            },
        })
        .ok(),
    })
}

pub(super) fn source_range(range: SourceRange) -> Range {
    Range::new(
        Position::new(range.start.line, range.start.character),
        Position::new(range.end.line, range.end.character),
    )
}

pub(super) fn catalog_path(root: &Path, path: &Path) -> Option<String> {
    pathing::relative_slash_path(root, path).ok().or_else(|| {
        path.is_absolute()
            .then(|| pathing::normalize_abs_path(path))
    })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RichRelationResponse {
    pub protocol_version: u32,
    pub revision: RelationRevision,
    pub entities: BTreeMap<u32, CallableEntity>,
    pub relations: Vec<CompactRelationDto>,
    pub complete: bool,
    pub budget_state: BudgetState,
    pub coverage: RichCoverage,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CompactRelationDto {
    pub caller_id: u32,
    pub callee_id: Option<u32>,
    pub direction: RelationDirection,
    pub call_sites: Vec<crate::call_model::CallSiteFact>,
    pub confidence: crate::call_model::RelationConfidence,
    pub evidence: crate::call_model::EvidenceLedger,
    pub ambiguity_set_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RichCoverage {
    pub eligible_files: u64,
    pub analyzed_files: u64,
    pub fallback_files: u64,
    pub external_bodies_limited: bool,
    pub semantic_generation: u64,
    pub incomplete_reason: Option<&'static str>,
}

impl RichRelationResponse {
    pub(super) fn to_value(
        revision: RelationRevision,
        page: RelationPage,
        coverage: CoverageSummary,
        cursor: usize,
    ) -> Value {
        let RelationPage {
            relations,
            total,
            site_limited,
            scan_limited,
            candidate_limited,
        } = page;
        let relation_limited = cursor.saturating_add(relations.len()) < total;
        let limited = relation_limited || site_limited || scan_limited || candidate_limited;
        let next_cursor = relation_limited.then(|| {
            encode_cursor(
                revision,
                relations
                    .first()
                    .map_or(RelationDirection::Outgoing, |relation| relation.direction),
                cursor.saturating_add(relations.len()),
            )
        });
        let (entities, relations) = compact_relations(relations);
        serde_json::to_value(Self {
            protocol_version: RELATION_PROTOCOL_VERSION,
            revision,
            entities,
            relations,
            complete: !limited,
            budget_state: if scan_limited {
                BudgetState::ScanLimited
            } else if candidate_limited {
                BudgetState::CandidateLimited
            } else if limited {
                BudgetState::PageLimited
            } else {
                BudgetState::Complete
            },
            coverage: RichCoverage {
                eligible_files: coverage.eligible_files,
                analyzed_files: coverage.analyzed_files,
                fallback_files: coverage.fallback_files,
                external_bodies_limited: coverage.external_bodies_limited,
                semantic_generation: revision.semantic_generation.0,
                incomplete_reason: if scan_limited {
                    Some("scan_limit")
                } else if candidate_limited {
                    Some("candidate_limit")
                } else if relation_limited {
                    Some("page_limit")
                } else if site_limited {
                    Some("site_limit")
                } else {
                    None
                },
            },
            next_cursor,
        })
        .unwrap_or(Value::Null)
    }
}

fn compact_relations(
    relations: Vec<CallRelation>,
) -> (BTreeMap<u32, CallableEntity>, Vec<CompactRelationDto>) {
    let mut entities = BTreeMap::new();
    let mut ids = HashMap::<String, u32>::new();
    let mut next_id = 1u32;
    let mut compact = Vec::with_capacity(relations.len());
    for relation in relations {
        let caller_id = intern_entity(&mut entities, &mut ids, &mut next_id, relation.caller);
        let callee_id = relation
            .callee
            .map(|entity| intern_entity(&mut entities, &mut ids, &mut next_id, entity));
        compact.push(CompactRelationDto {
            caller_id,
            callee_id,
            direction: relation.direction,
            call_sites: relation.call_sites,
            confidence: relation.confidence,
            evidence: relation.evidence,
            ambiguity_set_id: relation.ambiguity_set_id,
        });
    }
    (entities, compact)
}

fn intern_entity(
    entities: &mut BTreeMap<u32, CallableEntity>,
    ids: &mut HashMap<String, u32>,
    next_id: &mut u32,
    entity: CallableEntity,
) -> u32 {
    if let Some(id) = ids.get(&entity.entity_key) {
        return *id;
    }
    let id = *next_id;
    *next_id = next_id.saturating_add(1);
    ids.insert(entity.entity_key.clone(), id);
    entities.insert(id, entity);
    id
}

fn encode_cursor(
    revision: RelationRevision,
    direction: RelationDirection,
    offset: usize,
) -> String {
    let direction = match direction {
        RelationDirection::Incoming => 'i',
        RelationDirection::Outgoing => 'o',
    };
    format!(
        "{:x}.{:x}.{:x}.{}.{:x}",
        revision.semantic_generation.0,
        revision.overlay_epoch,
        revision.resolver_version,
        direction,
        offset
    )
}

fn decode_cursor(
    cursor: &str,
    revision: RelationRevision,
    direction: RelationDirection,
) -> Option<usize> {
    let mut parts = cursor.split('.');
    let generation = u64::from_str_radix(parts.next()?, 16).ok()?;
    let overlay = u64::from_str_radix(parts.next()?, 16).ok()?;
    let resolver = u32::from_str_radix(parts.next()?, 16).ok()?;
    let encoded_direction = parts.next()?;
    let offset = usize::from_str_radix(parts.next()?, 16).ok()?;
    if parts.next().is_some()
        || generation != revision.semantic_generation.0
        || overlay != revision.overlay_epoch
        || resolver != revision.resolver_version
        || encoded_direction
            != match direction {
                RelationDirection::Incoming => "i",
                RelationDirection::Outgoing => "o",
            }
    {
        return None;
    }
    Some(offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_cursor_is_revision_and_direction_bound() {
        let revision = RelationRevision {
            engine_epoch: 9,
            semantic_generation: crate::call_model::SemanticGeneration(7),
            overlay_epoch: 3,
            resolver_version: RELATION_PROTOCOL_VERSION,
        };
        let cursor = encode_cursor(revision, RelationDirection::Incoming, 200);
        assert_eq!(
            decode_cursor(&cursor, revision, RelationDirection::Incoming),
            Some(200)
        );
        assert_eq!(
            decode_cursor(&cursor, revision, RelationDirection::Outgoing),
            None
        );
        assert_eq!(
            decode_cursor(
                &cursor,
                RelationRevision {
                    semantic_generation: crate::call_model::SemanticGeneration(8),
                    ..revision
                },
                RelationDirection::Incoming,
            ),
            None
        );
    }
}
