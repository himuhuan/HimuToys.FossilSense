use super::*;

pub(in crate::server) async fn watched_change_in_scope(
    roots: &[PathBuf],
    change: &FileEvent,
    config_cache: &Arc<tokio::sync::Mutex<HashMap<PathBuf, crate::config::WorkspaceConfig>>>,
) -> Option<WatchDecision> {
    let path = uri_to_path(&change.uri)?;

    for root in roots {
        if !path.starts_with(root) {
            continue;
        }

        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("fossilsense.json"))
        {
            // Invalidate the config cache entry for this root, so the next
            // dirty event re-reads the config.
            config_cache.lock().await.remove(root);
            return Some(WatchDecision::Full);
        }

        let Ok(rel) = pathing::relative_slash_path(root, &path) else {
            continue;
        };

        // Use cached config to avoid re-reading fossilsense.json on every event.
        let config = {
            let cache = config_cache.lock().await;
            cache.get(root).cloned()
        };
        let config = match config {
            Some(c) => c,
            None => {
                let (conf, _) = crate::config::WorkspaceConfig::load(root);
                config_cache.lock().await.insert(root.clone(), conf.clone());
                conf
            }
        };

        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                !crate::project_context::is_ninja_marker_file_name(name)
                    && crate::project_context::is_supported_marker_file_name(name)
            })
            && config.is_path_allowed_by_scope_without_extension(&rel)
        {
            return Some(WatchDecision::ProjectContext(root.clone()));
        }

        if config.is_in_scope(&rel) {
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
    }

    None
}
