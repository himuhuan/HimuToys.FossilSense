use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use tower_lsp::lsp_types::MessageType;
use tower_lsp::Client;

use crate::call_model::SemanticGeneration;
use crate::call_service::CallReadHandle;
use crate::declaration_index::SemanticDeclarationIndex;
use crate::pathing;
use crate::project_context::{self, ProjectContextIndex};
use crate::store::IndexStore;

pub(super) async fn load_semantic_generation(root: PathBuf) -> Result<SemanticGeneration> {
    tokio::task::spawn_blocking(move || -> Result<SemanticGeneration> {
        let db_path = pathing::default_index_path(&root)?;
        let store = IndexStore::open_readonly(&db_path)?;
        let guard = store.begin_semantic_read(None)?;
        let generation = SemanticGeneration(guard.generation());
        guard.finish()?;
        Ok(generation)
    })
    .await?
}

pub(super) async fn rebuild_fallback_completion_table(
    root: PathBuf,
) -> Result<Arc<crate::completion::ordinary_service::FallbackCompletionNameTable>> {
    tokio::task::spawn_blocking(move || -> Result<_> {
        let db_path = pathing::default_index_path(&root)?;
        let store = IndexStore::open_readonly(&db_path)?;
        Ok(Arc::new(
            crate::completion::ordinary_service::FallbackCompletionNameTable::build(
                store.fallback_completion_view().all()?,
            ),
        ))
    })
    .await?
}

pub(super) async fn rebuild_declaration_index(
    root: PathBuf,
    project_context: Option<Arc<ProjectContextIndex>>,
    total_budget_bytes: usize,
) -> Result<Arc<SemanticDeclarationIndex>> {
    let built = tokio::task::spawn_blocking(move || -> Result<SemanticDeclarationIndex> {
        let db_path = pathing::default_index_path(&root)?;
        build_declaration_index_from_db(&db_path, project_context.as_deref(), total_budget_bytes)
    })
    .await;

    match built {
        Ok(Ok(table)) => Ok(Arc::new(table)),
        Ok(Err(err)) => Err(err),
        Err(err) => Err(err.into()),
    }
}

pub(super) fn build_declaration_index_from_db(
    db_path: &Path,
    project_context: Option<&ProjectContextIndex>,
    total_budget_bytes: usize,
) -> Result<SemanticDeclarationIndex> {
    let store = IndexStore::open_readonly(db_path)?;
    let names = crate::query::NameTable::build_from_declaration_view(
        &store.declaration_view(),
        project_context,
    )?;
    Ok(SemanticDeclarationIndex::build(names, total_budget_bytes))
}

pub(super) fn capture_call_read_handle(
    root: &Path,
    generation: SemanticGeneration,
) -> Result<Arc<CallReadHandle>> {
    Ok(Arc::new(CallReadHandle::at_default_generation(
        pathing::default_index_path(root)?,
        generation,
    )?))
}

pub(super) async fn update_declaration_index_paths(
    previous: Option<&SemanticDeclarationIndex>,
    root: PathBuf,
    paths: &[String],
    project_context: Option<Arc<ProjectContextIndex>>,
    total_budget_bytes: usize,
) -> Result<Arc<SemanticDeclarationIndex>> {
    let Some(previous) = previous else {
        return rebuild_declaration_index(root, project_context, total_budget_bytes).await;
    };

    let paths_vec = paths.to_vec();
    let query_root = root.clone();
    let built = tokio::task::spawn_blocking(move || -> Result<_> {
        let db_path = pathing::default_index_path(&query_root)?;
        let store = IndexStore::open_readonly(&db_path)?;
        store.declaration_view().name_rows_for_paths(&paths_vec)
    })
    .await;

    let fresh_names = match built {
        Ok(Ok(rows)) => rows,
        Ok(Err(err)) => return Err(err),
        Err(err) => return Err(err.into()),
    };
    let path_set: HashSet<String> = paths.iter().cloned().collect();
    let index = previous.with_updated_paths(
        &path_set,
        fresh_names,
        project_context.as_deref(),
        total_budget_bytes,
    );
    Ok(Arc::new(index))
}

pub(super) async fn rebuild_project_context(
    client: &Client,
    root: PathBuf,
    config: crate::config::WorkspaceConfig,
) -> Option<Arc<ProjectContextIndex>> {
    let build_root = root.clone();
    let built = tokio::task::spawn_blocking(move || -> Result<ProjectContextIndex> {
        project_context::discover_project_contexts(&build_root, &config)
    })
    .await;

    match built {
        Ok(Ok(index)) => Some(Arc::new(index)),
        Ok(Err(err)) => {
            client
                .log_message(
                    MessageType::WARNING,
                    format!("project context discovery failed: {err:#}"),
                )
                .await;
            None
        }
        Err(err) => {
            client
                .log_message(
                    MessageType::WARNING,
                    format!("project context task failed: {err}"),
                )
                .await;
            None
        }
    }
}
