use super::*;

use crate::project_context::{ProjectContextSelection, ProjectContextStatus, ProjectKey};

impl Backend {
    pub(super) async fn effective_project_for_uri(
        &self,
        uri: &Url,
        contexts: &[RequestContext],
    ) -> (Option<ProjectKey>, u64) {
        let selection = self.project_context_selection.lock().await.clone();
        let selection_epoch = self.project_context_selection_epoch.load(Ordering::Relaxed);
        let automatic = automatic_project_for_uri(uri, contexts);
        let effective = match selection {
            ProjectContextSelection::Auto => automatic,
            ProjectContextSelection::Manual { key } => canonical_project_key(contexts, &key),
            ProjectContextSelection::Unspecified => None,
        };
        (effective, selection_epoch)
    }

    pub(super) async fn project_context_status(&self, uri: Option<&Url>) -> ProjectContextStatus {
        let contexts = self.all_request_contexts().await;
        let automatic_project = uri.and_then(|uri| automatic_project_for_uri(uri, &contexts));
        let (selection, selection_changed) = {
            let mut stored = self.project_context_selection.lock().await;
            let mut selection = stored.clone();
            if let ProjectContextSelection::Manual { key } = &selection {
                let canonical = canonical_project_key(&contexts, key);
                if canonical.as_ref() != Some(key) {
                    selection = canonical.map_or(ProjectContextSelection::Auto, |key| {
                        ProjectContextSelection::Manual { key }
                    });
                    *stored = selection.clone();
                    (selection, true)
                } else {
                    (selection, false)
                }
            } else {
                (selection, false)
            }
        };
        if selection_changed {
            self.project_context_selection_epoch
                .fetch_add(1, Ordering::Relaxed);
            self.session.cache.clear_all_completion_memos().await;
        }

        let mut projects = contexts
            .iter()
            .filter_map(|context| context.engine.project_context.as_ref())
            .flat_map(|index| index.projects().iter().cloned())
            .collect::<Vec<_>>();
        projects.sort_by(|a, b| {
            a.workspace_name
                .to_ascii_lowercase()
                .cmp(&b.workspace_name.to_ascii_lowercase())
                .then_with(|| {
                    a.key
                        .project_path
                        .to_ascii_lowercase()
                        .cmp(&b.key.project_path.to_ascii_lowercase())
                })
        });
        let active_project = match &selection {
            ProjectContextSelection::Auto => automatic_project.clone(),
            ProjectContextSelection::Manual { key } => Some(key.clone()),
            ProjectContextSelection::Unspecified => None,
        };
        // `projects` and manual selection span every workspace root. Keep the
        // selector available whenever any immutable project model exists; an
        // active URI in a root whose model is degraded still has baseline Auto
        // behavior and may explicitly select a project discovered elsewhere.
        let available = contexts
            .iter()
            .any(|context| context.engine.project_context.is_some());

        ProjectContextStatus {
            available,
            projects,
            selection,
            automatic_project,
            active_project,
        }
    }

    pub(super) async fn set_project_context_selection(
        &self,
        requested: ProjectContextSelection,
        uri: Option<&Url>,
    ) -> ProjectContextStatus {
        let contexts = self.all_request_contexts().await;
        let selection = match requested {
            ProjectContextSelection::Manual { key } => canonical_project_key(&contexts, &key)
                .map_or(ProjectContextSelection::Auto, |key| {
                    ProjectContextSelection::Manual { key }
                }),
            other => other,
        };
        let changed = {
            let mut stored = self.project_context_selection.lock().await;
            if *stored == selection {
                false
            } else {
                *stored = selection;
                true
            }
        };
        if changed {
            self.project_context_selection_epoch
                .fetch_add(1, Ordering::Relaxed);
            self.session.cache.clear_all_completion_memos().await;
        }
        self.project_context_status(uri).await
    }

    async fn all_request_contexts(&self) -> Vec<RequestContext> {
        let roots = self.workspace_roots.lock().await.clone();
        let mut contexts = Vec::with_capacity(roots.len());
        for root in roots {
            contexts.push(self.request_context_for_root(root).await);
        }
        contexts
    }
}

fn canonical_project_key(contexts: &[RequestContext], key: &ProjectKey) -> Option<ProjectKey> {
    contexts.iter().find_map(|context| {
        context
            .engine
            .project_context
            .as_ref()
            .and_then(|index| index.canonical_key(key))
    })
}

pub(super) fn project_context_command_uri(arg: Option<&Value>) -> Option<Url> {
    arg.and_then(|value| value.get("uri").or(Some(value)))
        .and_then(Value::as_str)
        .and_then(|raw| Url::parse(raw).ok())
}

pub(super) fn project_context_selection_arg(
    arg: Option<&Value>,
) -> Option<ProjectContextSelection> {
    let value = arg?;
    serde_json::from_value(value.get("selection").unwrap_or(value).clone()).ok()
}

fn automatic_project_for_uri(uri: &Url, contexts: &[RequestContext]) -> Option<ProjectKey> {
    let (context, rel) = containing_context_for_uri(uri, contexts)?;
    context
        .engine
        .project_context
        .as_ref()?
        .nearest_for_file(&rel)
}

fn containing_context_for_uri<'a>(
    uri: &Url,
    contexts: &'a [RequestContext],
) -> Option<(&'a RequestContext, String)> {
    let path = uri_to_path(uri)?;
    let context = contexts
        .iter()
        .filter(|context| pathing::path_is_within(&context.engine.root, &path))
        .max_by_key(|context| context.engine.root.components().count())?;
    let rel = pathing::relative_slash_path(&context.engine.root, &path).ok()?;
    Some((context, rel))
}
