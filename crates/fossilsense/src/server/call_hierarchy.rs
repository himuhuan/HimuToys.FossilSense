use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tower_lsp::lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyItem, CallHierarchyOutgoingCall, Position, Range,
    SymbolKind, Url,
};

use super::{uri_to_path, Backend};
use crate::call_catalog::RelationCatalog;
use crate::call_model::{
    BudgetState, CallRelation, CallableEntity, CallableLocator, CoverageSummary, RelationRevision,
    SourcePosition, SourceRange, RELATION_PROTOCOL_VERSION,
};
use crate::pathing;
use crate::reachability::ReachGraph;

const STANDARD_RELATION_LIMIT: usize = 500;
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
    pub(super) catalog: RelationCatalog,
    pub(super) revision: RelationRevision,
    reach_graph: Option<std::sync::Arc<ReachGraph>>,
}

impl RelationRequestState {
    pub(super) fn outgoing(&self, entity_key: &str) -> Vec<CallRelation> {
        self.with_reachability(self.catalog.outgoing(entity_key))
    }

    pub(super) fn incoming(&self, entity_key: &str) -> Vec<CallRelation> {
        self.with_reachability(self.catalog.incoming(entity_key))
    }

    fn with_reachability(&self, mut relations: Vec<CallRelation>) -> Vec<CallRelation> {
        let Some(graph) = &self.reach_graph else {
            return relations;
        };
        for relation in &mut relations {
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
        relations
    }
}

impl Backend {
    pub(super) async fn rich_relations_command(&self, arg: &Value) -> Option<Value> {
        let uri = arg
            .get("uri")
            .and_then(Value::as_str)
            .and_then(|value| Url::parse(value).ok())?;
        let state = self.relation_state_for_uri(&uri).await?;
        let entity_key = if let Some(key) = arg.get("entityKey").and_then(Value::as_str) {
            key.to_string()
        } else {
            let line = arg.get("line").and_then(Value::as_u64).unwrap_or(0) as u32;
            let character = arg.get("character").and_then(Value::as_u64).unwrap_or(0) as u32;
            let path = uri_to_path(&uri)?;
            let rel = catalog_path(&state.root, &path)?;
            let entities = state
                .catalog
                .entities_at(&rel, SourcePosition { line, character });
            let [entity] = entities.as_slice() else {
                return None;
            };
            entity.entity_key.clone()
        };
        let relations = match arg.get("direction").and_then(Value::as_str) {
            Some("incoming") => state.incoming(&entity_key),
            _ => state.outgoing(&entity_key),
        };
        Some(RichRelationResponse::new(
            state.revision,
            relations,
            state.catalog.coverage().clone(),
            arg.get("cursor").and_then(Value::as_u64).unwrap_or(0) as usize,
        ))
    }

    pub(super) async fn relation_state_for_uri(&self, uri: &Url) -> Option<RelationRequestState> {
        let root = self.root_for_uri(uri).await?;
        let context = self.request_context_for_root(root.clone()).await;
        let base = context.engine.relation_catalog.as_deref()?.clone();
        let mut snapshots: Vec<_> = self
            .session
            .documents
            .all_snapshots()
            .await
            .into_iter()
            .filter(|(document_uri, _)| {
                uri_to_path(document_uri).is_some_and(|path| pathing::path_is_within(&root, &path))
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
        let catalog = if overlay_epoch == 0 {
            base
        } else if let Some(cached) = self
            .session
            .cache
            .cached_relation_overlay(&root, context.engine.epoch, overlay_epoch)
            .await
        {
            (*cached).clone()
        } else {
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
                    )
                    .await?;
                overlays.push((
                    rel,
                    parsed.callable_anchors.clone(),
                    parsed.call_sites.clone(),
                ));
            }
            let catalog = base.with_overlays(overlays);
            self.session
                .cache
                .store_relation_overlay(
                    root.clone(),
                    context.engine.epoch,
                    overlay_epoch,
                    std::sync::Arc::new(catalog.clone()),
                )
                .await;
            catalog
        };
        Some(RelationRequestState {
            root,
            catalog,
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
        let entities = state.catalog.entities_at(
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
        let key = item_key(&state.catalog, item)?;
        Some(
            state
                .incoming(&key)
                .into_iter()
                .take(STANDARD_RELATION_LIMIT)
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
        let key = item_key(&state.catalog, item)?;
        Some(
            state
                .outgoing(&key)
                .into_iter()
                .take(STANDARD_RELATION_LIMIT)
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

pub(super) fn item_key(catalog: &RelationCatalog, item: &CallHierarchyItem) -> Option<String> {
    let data = serde_json::from_value::<ItemData>(item.data.clone()?).ok()?;
    catalog
        .resolve_locator(&data.locator)
        .map(|entity| entity.entity_key.clone())
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
    pub relations: Vec<CallRelation>,
    pub complete: bool,
    pub budget_state: BudgetState,
    pub coverage: CoverageSummary,
    pub next_cursor: Option<usize>,
}

impl RichRelationResponse {
    pub(super) fn new(
        revision: RelationRevision,
        relations: Vec<CallRelation>,
        coverage: CoverageSummary,
        cursor: usize,
    ) -> Value {
        let total = relations.len();
        let mut relations: Vec<_> = relations
            .into_iter()
            .skip(cursor)
            .take(RICH_RELATION_PAGE_SIZE)
            .collect();
        let relation_limited = cursor.saturating_add(relations.len()) < total;
        let mut site_limited = false;
        for relation in &mut relations {
            if relation.call_sites.len() > CALL_SITE_LIMIT {
                relation.call_sites.truncate(CALL_SITE_LIMIT);
                site_limited = true;
            }
        }
        let limited = relation_limited || site_limited;
        let next_cursor = relation_limited.then_some(cursor.saturating_add(relations.len()));
        serde_json::to_value(Self {
            protocol_version: RELATION_PROTOCOL_VERSION,
            revision,
            relations,
            complete: !limited,
            budget_state: if limited {
                BudgetState::PageLimited
            } else {
                BudgetState::Complete
            },
            coverage,
            next_cursor,
        })
        .unwrap_or(Value::Null)
    }
}
