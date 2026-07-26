use super::*;
use crate::server::WorkspaceRootConfig;

pub(in crate::server) async fn watched_change_in_scope(
    roots: &[PathBuf],
    change: &FileEvent,
    config_cache: &Arc<tokio::sync::Mutex<HashMap<PathBuf, WorkspaceRootConfig>>>,
) -> Option<WatchDecision> {
    let path = uri_to_path(&change.uri)?;
    let root = roots
        .iter()
        .filter(|root| pathing::path_is_within(root, &path))
        .max_by_key(|root| root.components().count())?;
    let Ok(rel) = pathing::relative_slash_path(root, &path) else {
        return None;
    };

    if rel.eq_ignore_ascii_case("fossilsense.json") {
        // Invalidate the config cache entry for this root, so the next dirty
        // event re-reads the config. Nested files with this basename are not
        // workspace configuration unless their directory is itself a root.
        config_cache.lock().await.remove(root);
        return Some(WatchDecision::Full(root.clone()));
    }

    // Use cached config to avoid re-reading fossilsense.json on every event.
    let config = {
        let cache = config_cache.lock().await;
        cache.get(root).cloned()
    };
    let config = match config {
        Some(c) => c,
        None => {
            let load_root = root.clone();
            let conf = tokio::task::spawn_blocking(move || WorkspaceRootConfig::load(&load_root))
                .await
                .unwrap_or_else(|_| WorkspaceRootConfig::fallback(root));
            config_cache.lock().await.insert(root.clone(), conf.clone());
            conf
        }
    };

    let marker_name = path.file_name().and_then(|name| name.to_str());
    if marker_name.is_some_and(crate::project_context::is_supported_marker_file_name)
        && config.workspace.is_project_marker_in_scope(&rel)
    {
        if marker_name.is_some_and(|name| {
            name.eq_ignore_ascii_case("go.mod") || name.eq_ignore_ascii_case("go.work")
        }) {
            // Go module/workspace metadata affects both project evidence and
            // the package import graph, so a marker-only refresh is
            // insufficient.
            return Some(WatchDecision::Full(root.clone()));
        }
        return Some(WatchDecision::ProjectContext(root.clone()));
    }

    if config.workspace.is_in_scope(&rel) {
        let kind = if change.typ == FileChangeType::DELETED {
            indexer::DirtyFileKind::Delete
        } else {
            indexer::DirtyFileKind::Upsert
        };
        return Some(WatchDecision::Dirty(RootDirtyChange {
            root: root.clone(),
            rel_path: rel,
            change: indexer::DirtyFileChange {
                absolute_path: path,
                kind,
            },
        }));
    }

    None
}
