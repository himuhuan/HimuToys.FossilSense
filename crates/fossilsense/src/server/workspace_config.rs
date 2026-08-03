use super::*;

#[derive(Clone, Debug)]
struct AuthorizedExternalSourceRoot {
    identity_root: PathBuf,
    canonical_root: PathBuf,
}

#[derive(Clone, Debug)]
pub(in crate::server) struct AuthorizedExternalSourceIdentity {
    pub(in crate::server) identity_path: String,
    pub(in crate::server) go_only: bool,
}

#[derive(Clone, Debug)]
pub(in crate::server) struct AuthorizedExternalSourcePath {
    pub(in crate::server) identities: Vec<AuthorizedExternalSourceIdentity>,
    pub(in crate::server) canonical_path: PathBuf,
}

impl AuthorizedExternalSourcePath {
    pub(in crate::server) fn identity_for_requested_path(
        &self,
        requested: &Path,
    ) -> Option<PathBuf> {
        let requested = pathing::normalize_abs_path(requested);
        self.identities
            .iter()
            .find(|identity| {
                if cfg!(windows) {
                    identity.identity_path.eq_ignore_ascii_case(&requested)
                } else {
                    identity.identity_path == requested
                }
            })
            .or_else(|| self.identities.first())
            .map(|identity| PathBuf::from(&identity.identity_path))
    }
}

#[derive(Clone, Debug, Default)]
pub(in crate::server) struct AuthorizedExternalSourceRoots {
    include_roots: Vec<AuthorizedExternalSourceRoot>,
    go_module_roots: Vec<AuthorizedExternalSourceRoot>,
    path_cache: Arc<StdMutex<ExternalPathAuthorizationCache>>,
}

const MAX_EXTERNAL_PATH_AUTHORIZATION_CACHE_ENTRIES: usize = 512;

#[derive(Debug, Default)]
struct ExternalPathAuthorizationCache {
    entries: HashMap<String, Option<AuthorizedExternalSourcePath>>,
    revision: u64,
    #[cfg(test)]
    misses: usize,
    #[cfg(test)]
    publish_barriers: Option<(Arc<std::sync::Barrier>, Arc<std::sync::Barrier>)>,
}

#[derive(Clone, Debug)]
pub(in crate::server) struct PublishedWorkspaceSemantics {
    pub(in crate::server) workspace: WorkspaceConfig,
    pub(in crate::server) language: LanguageResolver,
    pub(in crate::server) external_roots: Arc<AuthorizedExternalSourceRoots>,
    index_configuration: Arc<crate::indexer::IndexConfigurationSnapshot>,
}

#[derive(Default)]
pub(in crate::server) struct WorkspaceSemanticsBootstrap {
    entries: HashMap<PathBuf, WorkspaceSemanticsBootstrapEntry>,
    next_attempt: u64,
    #[cfg(test)]
    preparation_counts: HashMap<PathBuf, usize>,
    #[cfg(test)]
    barriers: HashMap<PathBuf, (Arc<tokio::sync::Barrier>, Arc<tokio::sync::Barrier>)>,
    #[cfg(test)]
    finalize_barriers: HashMap<PathBuf, (Arc<tokio::sync::Barrier>, Arc<tokio::sync::Barrier>)>,
    #[cfg(test)]
    removal_barriers: HashMap<PathBuf, (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    #[cfg(test)]
    attempt_started: HashMap<PathBuf, Arc<tokio::sync::Notify>>,
}

enum WorkspaceSemanticsBootstrapEntry {
    Building {
        attempt: u64,
        completion: tokio::sync::watch::Sender<bool>,
    },
    Ready,
}

enum WorkspaceSemanticsBootstrapAction {
    Build {
        attempt: u64,
        completion: tokio::sync::watch::Sender<bool>,
        waiter: tokio::sync::watch::Receiver<bool>,
        #[cfg(test)]
        barriers: Option<(Arc<tokio::sync::Barrier>, Arc<tokio::sync::Barrier>)>,
        #[cfg(test)]
        finalize_barriers: Option<(Arc<tokio::sync::Barrier>, Arc<tokio::sync::Barrier>)>,
    },
    Wait(tokio::sync::watch::Receiver<bool>),
    Ready,
}

struct WorkspaceSemanticsBootstrapCompletion(tokio::sync::watch::Sender<bool>);

impl Drop for WorkspaceSemanticsBootstrapCompletion {
    fn drop(&mut self) {
        self.0.send_replace(true);
    }
}

#[cfg(test)]
#[derive(Default)]
pub(in crate::server) struct ExternalSourceRootsCache {
    entries: HashMap<PathBuf, ExternalSourceRootsCacheEntry>,
    revisions: HashMap<PathBuf, u64>,
}

#[cfg(test)]
enum ExternalSourceRootsCacheEntry {
    Building {
        revision: u64,
        completion: tokio::sync::watch::Sender<bool>,
    },
    Ready {
        revision: u64,
        value: Arc<AuthorizedExternalSourceRoots>,
    },
}

impl PublishedWorkspaceSemantics {
    pub(in crate::server) fn empty(workspace_root: &Path) -> Self {
        let workspace = workspace_root
            .canonicalize()
            .unwrap_or_else(|_| workspace_root.to_path_buf());
        let config = WorkspaceConfig::default();
        let index_configuration = Arc::new(crate::indexer::IndexConfigurationSnapshot {
            workspace: workspace.clone(),
            config: config.clone(),
            language_resolver: LanguageResolver::from_workspace_config(&workspace, &config),
            include_roots: Vec::new(),
            go_module_roots: Vec::new(),
            protobuf_c_enabled: false,
            proto_roots: Vec::new(),
            issues: Vec::new(),
        });
        Self {
            workspace: config.clone(),
            language: LanguageResolver::from_workspace_config(workspace_root, &config),
            external_roots: Arc::new(AuthorizedExternalSourceRoots::default()),
            index_configuration,
        }
    }

    pub(in crate::server) fn from_index_configuration(
        published_root: &Path,
        configuration: &crate::indexer::IndexConfigurationSnapshot,
    ) -> Self {
        Self {
            workspace: configuration.config.clone(),
            // The indexer canonicalizes its workspace before parsing. LSP
            // request paths retain the editor's root spelling, so rebuild the
            // equivalent resolver on that published identity to keep relative
            // override globs stable (notably across Windows `\\?\` paths).
            language: LanguageResolver::from_workspace_config(
                published_root,
                &configuration.config,
            ),
            external_roots: Arc::new(AuthorizedExternalSourceRoots {
                include_roots: authorized_external_root_pairs(
                    configuration.include_roots.clone(),
                    None,
                    false,
                ),
                // The indexer has already removed workspace overlaps and
                // canonical duplicates while preserving the first identity.
                go_module_roots: authorized_external_root_pairs(
                    configuration.go_module_roots.clone(),
                    None,
                    false,
                ),
                ..Default::default()
            }),
            index_configuration: Arc::new(configuration.clone()),
        }
    }

    #[cfg(test)]
    pub(in crate::server) fn load_current(
        workspace_root: &Path,
        client_include_paths: &[String],
        client_go_module_paths: &[String],
    ) -> Self {
        crate::indexer::prepare_index_configuration(
            workspace_root,
            client_include_paths,
            client_go_module_paths,
            None,
            &[],
        )
        .map(|configuration| Self::from_index_configuration(workspace_root, &configuration))
        .unwrap_or_else(|_| Self::empty(workspace_root))
    }

    pub(in crate::server) fn language_for_path(&self, path: &Path) -> SourceLanguage {
        let identity = self.external_roots.language_identity_for_path(path);
        self.language
            .language_for_path(identity.as_deref().unwrap_or(path))
    }

    pub(in crate::server) fn language_for_uri(&self, uri: &Url) -> SourceLanguage {
        uri_to_path(uri)
            .map(|path| self.language_for_path(&path))
            .unwrap_or_else(|| SourceLanguage::default_for_path(Path::new(uri.path())))
    }

    pub(in crate::server) fn protobuf_c_enabled(&self) -> bool {
        self.index_configuration.protobuf_c_enabled
    }

    pub(in crate::server) fn index_configuration_snapshot(
        &self,
    ) -> crate::indexer::IndexConfigurationSnapshot {
        self.index_configuration.as_ref().clone()
    }
}

impl AuthorizedExternalSourceRoots {
    fn language_identity_for_path(&self, path: &Path) -> Option<PathBuf> {
        self.include_roots
            .iter()
            .chain(self.go_module_roots.iter())
            .find_map(|root| {
                relative_path_under_spelling(&root.identity_root, path)
                    .or_else(|| relative_path_under_spelling(&root.canonical_root, path))
                    .map(|suffix| root.identity_root.join(suffix))
            })
    }

    pub(in crate::server) fn mapped_path(
        &self,
        path: &Path,
    ) -> Option<AuthorizedExternalSourcePath> {
        let cache_key = external_path_cache_key(path);
        #[cfg(test)]
        let (cached, revision, publish_barriers) = {
            let mut cache = self
                .path_cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let cached = cache.entries.get(&cache_key).cloned();
            let revision = cache.revision;
            let publish_barriers = cached
                .is_none()
                .then(|| cache.publish_barriers.take())
                .flatten();
            (cached, revision, publish_barriers)
        };
        #[cfg(not(test))]
        let (cached, revision) = {
            let cache = self
                .path_cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (cache.entries.get(&cache_key).cloned(), cache.revision)
        };
        if let Some(cached) = cached {
            return cached;
        }

        let mapped = self.mapped_path_uncached(path);
        #[cfg(test)]
        if let Some((started, resume)) = publish_barriers {
            started.wait();
            resume.wait();
        }
        let mut cache = self
            .path_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        #[cfg(test)]
        {
            cache.misses = cache.misses.saturating_add(1);
        }
        if cache.revision != revision {
            return mapped;
        }
        if cache.entries.len() >= MAX_EXTERNAL_PATH_AUTHORIZATION_CACHE_ENTRIES
            && !cache.entries.contains_key(&cache_key)
        {
            if let Some(evicted) = cache.entries.keys().next().cloned() {
                cache.entries.remove(&evicted);
            }
        }
        cache.entries.insert(cache_key, mapped.clone());
        mapped
    }

    fn mapped_path_uncached(&self, path: &Path) -> Option<AuthorizedExternalSourcePath> {
        let canonical_path = canonicalize_source_path(path)?;
        let mut identities =
            authorized_source_identities(&self.include_roots, &canonical_path, false);
        identities.extend(authorized_source_identities(
            &self.go_module_roots,
            &canonical_path,
            true,
        ));
        dedupe_authorized_identities(&mut identities);
        (!identities.is_empty()).then_some(AuthorizedExternalSourcePath {
            identities,
            canonical_path,
        })
    }

    pub(in crate::server) fn invalidate_path(&self, path: &Path) {
        let mut cache = self
            .path_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.revision = cache.revision.wrapping_add(1);
        cache.entries.remove(&external_path_cache_key(path));
    }

    #[cfg(test)]
    pub(in crate::server) fn authorization_miss_count_for_test(&self) -> usize {
        self.path_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .misses
    }

    #[cfg(test)]
    pub(in crate::server) fn authorization_cache_len_for_test(&self) -> usize {
        self.path_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entries
            .len()
    }

    #[cfg(test)]
    pub(in crate::server) fn set_authorization_publish_barriers_for_test(
        &self,
        started: Arc<std::sync::Barrier>,
        resume: Arc<std::sync::Barrier>,
    ) {
        self.path_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .publish_barriers = Some((started, resume));
    }

    pub(in crate::server) fn authorized_path(
        &self,
        path: &Path,
        family: crate::semantic_model::SemanticFamily,
    ) -> Option<AuthorizedExternalSourcePath> {
        let mut mapped = self.mapped_path(path)?;
        mapped.identities.retain(|identity| {
            !identity.go_only || family == crate::semantic_model::SemanticFamily::Go
        });
        (!mapped.identities.is_empty()).then_some(mapped)
    }

    pub(in crate::server) fn normalized_include_roots(&self) -> Vec<String> {
        self.include_roots
            .iter()
            .map(|root| pathing::normalize_abs_path(&root.identity_root))
            .collect()
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_workspace_semantics_bootstrap(
    client: Client,
    workspace_roots: Arc<Mutex<Vec<PathBuf>>>,
    cache: CacheLedger,
    include_paths: Arc<Mutex<Vec<String>>>,
    go_module_paths: Arc<Mutex<Vec<String>>>,
    bootstrap_state: Arc<Mutex<WorkspaceSemanticsBootstrap>>,
    root: PathBuf,
    attempt: u64,
    completion: tokio::sync::watch::Sender<bool>,
    #[cfg(test)] barriers: Option<(Arc<tokio::sync::Barrier>, Arc<tokio::sync::Barrier>)>,
    #[cfg(test)] finalize_barriers: Option<(Arc<tokio::sync::Barrier>, Arc<tokio::sync::Barrier>)>,
) {
    let _completion = WorkspaceSemanticsBootstrapCompletion(completion);
    #[cfg(test)]
    if let Some((started, resume)) = barriers {
        started.wait().await;
        resume.wait().await;
    }

    let include_paths = include_paths.lock().await.clone();
    let go_module_paths = go_module_paths.lock().await.clone();
    let prepare_root = root.clone();
    let prepared = tokio::task::spawn_blocking(move || {
        crate::indexer::prepare_index_configuration(
            &prepare_root,
            &include_paths,
            &go_module_paths,
            None,
            &[],
        )
    })
    .await;
    let configuration = match prepared {
        Ok(Ok(configuration)) => Some(configuration),
        Ok(Err(error)) => {
            client
                .log_message(
                    MessageType::WARNING,
                    format!(
                        "workspace configuration bootstrap failed for {}: {error:#}",
                        root.display()
                    ),
                )
                .await;
            None
        }
        Err(error) => {
            client
                .log_message(
                    MessageType::WARNING,
                    format!(
                        "workspace configuration bootstrap worker failed for {}: {error}",
                        root.display()
                    ),
                )
                .await;
            None
        }
    };

    let mut bootstrap = bootstrap_state.lock().await;
    let still_current = matches!(
        bootstrap.entries.get(&root),
        Some(WorkspaceSemanticsBootstrapEntry::Building {
            attempt: current_attempt,
            ..
        }) if *current_attempt == attempt
    );
    if !still_current {
        return;
    }
    #[cfg(test)]
    if let Some((started, resume)) = finalize_barriers {
        started.wait().await;
        resume.wait().await;
    }

    match configuration {
        Some(configuration) if workspace_roots.lock().await.contains(&root) => {
            let workspace_semantics = Arc::new(
                PublishedWorkspaceSemantics::from_index_configuration(&root, &configuration),
            );
            cache
                .publish_workspace_semantics_if_absent(root.clone(), workspace_semantics)
                .await;
            bootstrap
                .entries
                .insert(root, WorkspaceSemanticsBootstrapEntry::Ready);
        }
        _ => {
            bootstrap.entries.remove(&root);
        }
    }
}

impl Backend {
    pub(in crate::server) async fn ensure_workspace_semantics(&self, root: &Path) {
        let root = root.to_path_buf();
        if self
            .session
            .cache
            .current_engine_snapshot(&root)
            .await
            .is_some()
        {
            return;
        }

        let action = {
            let mut bootstrap = self.workspace_semantics_bootstrap.lock().await;
            let completed_without_cleanup = matches!(
                bootstrap.entries.get(&root),
                Some(WorkspaceSemanticsBootstrapEntry::Building { completion, .. })
                    if *completion.borrow()
            );
            if completed_without_cleanup {
                bootstrap.entries.remove(&root);
            }
            match bootstrap.entries.get(&root) {
                Some(WorkspaceSemanticsBootstrapEntry::Building { completion, .. }) => {
                    WorkspaceSemanticsBootstrapAction::Wait(completion.subscribe())
                }
                Some(WorkspaceSemanticsBootstrapEntry::Ready) => {
                    WorkspaceSemanticsBootstrapAction::Ready
                }
                None => {
                    bootstrap.next_attempt = bootstrap.next_attempt.wrapping_add(1).max(1);
                    let attempt = bootstrap.next_attempt;
                    let (completion, waiter) = tokio::sync::watch::channel(false);
                    #[cfg(test)]
                    let barriers = {
                        let count = bootstrap
                            .preparation_counts
                            .entry(root.clone())
                            .or_default();
                        *count = count.saturating_add(1);
                        bootstrap.barriers.remove(&root)
                    };
                    #[cfg(test)]
                    let finalize_barriers = bootstrap.finalize_barriers.remove(&root);
                    bootstrap.entries.insert(
                        root.clone(),
                        WorkspaceSemanticsBootstrapEntry::Building {
                            attempt,
                            completion: completion.clone(),
                        },
                    );
                    #[cfg(test)]
                    if let Some(started) = bootstrap.attempt_started.remove(&root) {
                        started.notify_one();
                    }
                    WorkspaceSemanticsBootstrapAction::Build {
                        attempt,
                        completion,
                        waiter,
                        #[cfg(test)]
                        barriers,
                        #[cfg(test)]
                        finalize_barriers,
                    }
                }
            }
        };
        let mut waiter = match action {
            WorkspaceSemanticsBootstrapAction::Build {
                attempt,
                completion,
                waiter,
                #[cfg(test)]
                barriers,
                #[cfg(test)]
                finalize_barriers,
            } => {
                tokio::spawn(run_workspace_semantics_bootstrap(
                    self.client.clone(),
                    self.workspace_roots.clone(),
                    self.session.cache.clone(),
                    self.include_paths.clone(),
                    self.go_module_paths.clone(),
                    self.workspace_semantics_bootstrap.clone(),
                    root,
                    attempt,
                    completion,
                    #[cfg(test)]
                    barriers,
                    #[cfg(test)]
                    finalize_barriers,
                ));
                waiter
            }
            WorkspaceSemanticsBootstrapAction::Wait(waiter) => waiter,
            WorkspaceSemanticsBootstrapAction::Ready => return,
        };
        if !*waiter.borrow() {
            let _ = waiter.changed().await;
        }
    }

    pub(in crate::server) async fn remove_workspace_runtime_roots(&self, roots: &[PathBuf]) {
        if roots.is_empty() {
            return;
        }
        let mut bootstrap = self.workspace_semantics_bootstrap.lock().await;
        for root in roots {
            if let Some(WorkspaceSemanticsBootstrapEntry::Building { completion, .. }) =
                bootstrap.entries.remove(root)
            {
                completion.send_replace(true);
            }
        }
        #[cfg(test)]
        {
            bootstrap
                .preparation_counts
                .retain(|root, _| !roots.contains(root));
            bootstrap.barriers.retain(|root, _| !roots.contains(root));
            bootstrap
                .finalize_barriers
                .retain(|root, _| !roots.contains(root));
        }
        #[cfg(test)]
        {
            let barriers: Vec<_> = roots
                .iter()
                .filter_map(|root| bootstrap.removal_barriers.remove(root))
                .collect();
            for (started, _) in &barriers {
                started.notify_one();
            }
            for (_, resume) in barriers {
                resume.notified().await;
            }
        }
        // Keep the bootstrap state locked through engine removal. This makes
        // invalidating the old attempt and deleting its snapshot one atomic
        // lifecycle transition for requests trying to re-add the same root.
        self.session.cache.remove_workspace_roots(roots).await;
    }

    #[cfg(test)]
    pub(in crate::server) async fn set_workspace_semantics_bootstrap_barriers_for_test(
        &self,
        root: &Path,
        started: Arc<tokio::sync::Barrier>,
        resume: Arc<tokio::sync::Barrier>,
    ) {
        self.workspace_semantics_bootstrap
            .lock()
            .await
            .barriers
            .insert(root.to_path_buf(), (started, resume));
    }

    #[cfg(test)]
    pub(in crate::server) async fn set_workspace_semantics_bootstrap_finalize_barriers_for_test(
        &self,
        root: &Path,
        started: Arc<tokio::sync::Barrier>,
        resume: Arc<tokio::sync::Barrier>,
    ) {
        self.workspace_semantics_bootstrap
            .lock()
            .await
            .finalize_barriers
            .insert(root.to_path_buf(), (started, resume));
    }

    #[cfg(test)]
    pub(in crate::server) async fn set_workspace_semantics_removal_barriers_for_test(
        &self,
        root: &Path,
        started: Arc<tokio::sync::Notify>,
        resume: Arc<tokio::sync::Notify>,
    ) {
        self.workspace_semantics_bootstrap
            .lock()
            .await
            .removal_barriers
            .insert(root.to_path_buf(), (started, resume));
    }

    #[cfg(test)]
    pub(in crate::server) async fn set_workspace_semantics_bootstrap_attempt_started_for_test(
        &self,
        root: &Path,
        started: Arc<tokio::sync::Notify>,
    ) {
        self.workspace_semantics_bootstrap
            .lock()
            .await
            .attempt_started
            .insert(root.to_path_buf(), started);
    }

    #[cfg(test)]
    pub(in crate::server) async fn workspace_semantics_bootstrap_preparation_count_for_test(
        &self,
        root: &Path,
    ) -> usize {
        self.workspace_semantics_bootstrap
            .lock()
            .await
            .preparation_counts
            .get(root)
            .copied()
            .unwrap_or(0)
    }

    pub(in crate::server) async fn invalidate_external_source_path_authorization(
        &self,
        path: &Path,
    ) {
        let roots = self.workspace_roots.lock().await.clone();
        for root in roots {
            if let Some(snapshot) = self.session.cache.current_engine_snapshot(&root).await {
                snapshot
                    .workspace_semantics
                    .external_roots
                    .invalidate_path(path);
            }
        }

        #[cfg(test)]
        {
            let current_config_roots = {
                let cache = self.external_source_roots_cache.lock().await;
                cache
                    .entries
                    .values()
                    .filter_map(|entry| match entry {
                        ExternalSourceRootsCacheEntry::Ready { value, .. } => Some(value.clone()),
                        ExternalSourceRootsCacheEntry::Building { .. } => None,
                    })
                    .collect::<Vec<_>>()
            };
            for roots in current_config_roots {
                roots.invalidate_path(path);
            }
        }
    }

    pub(in crate::server) async fn workspace_root_config(
        &self,
        root: &Path,
    ) -> WorkspaceRootConfig {
        if let Some(cached) = self.config_cache.lock().await.get(root).cloned() {
            return cached;
        }

        let root = root.to_path_buf();
        let load_root = root.clone();
        let loaded = tokio::task::spawn_blocking(move || WorkspaceRootConfig::load(&load_root))
            .await
            .unwrap_or_else(|_| WorkspaceRootConfig::fallback(&root));
        self.config_cache
            .lock()
            .await
            .entry(root)
            .or_insert_with(|| loaded.clone())
            .clone()
    }

    #[cfg(test)]
    pub(in crate::server) async fn source_language_for_path(&self, path: &Path) -> SourceLanguage {
        let roots = self.workspace_roots.lock().await.clone();
        let containing = roots
            .iter()
            .filter(|root| pathing::path_is_within(root, path))
            .max_by_key(|root| root.components().count())
            .cloned();
        if let Some(root) = containing {
            return self
                .workspace_root_config(&root)
                .await
                .language
                .language_for_path(path);
        }

        for root in roots {
            if let Some(language) = self
                .workspace_root_config(&root)
                .await
                .language
                .overridden_language_for_path(path)
            {
                return language;
            }
        }
        SourceLanguage::default_for_path(path)
    }

    #[cfg(test)]
    pub(in crate::server) async fn source_language_for_uri(&self, uri: &Url) -> SourceLanguage {
        match uri_to_path(uri) {
            Some(path) => self.source_language_for_path(&path).await,
            None => SourceLanguage::default_for_path(Path::new(uri.path())),
        }
    }

    #[cfg(test)]
    pub(in crate::server) async fn authorized_external_source_roots(
        &self,
        workspace_root: &Path,
    ) -> Arc<AuthorizedExternalSourceRoots> {
        loop {
            let (build_revision, build_completion, mut wait_for) = {
                let mut cache = self.external_source_roots_cache.lock().await;
                let revision = cache.revisions.get(workspace_root).copied().unwrap_or(0);
                match cache.entries.get(workspace_root) {
                    Some(ExternalSourceRootsCacheEntry::Ready {
                        revision: cached_revision,
                        value,
                    }) if *cached_revision == revision => return value.clone(),
                    Some(ExternalSourceRootsCacheEntry::Building {
                        revision: cached_revision,
                        completion,
                    }) if *cached_revision == revision => {
                        (None, None, Some(completion.subscribe()))
                    }
                    _ => {
                        let (completion, _receiver) = tokio::sync::watch::channel(false);
                        cache.entries.insert(
                            workspace_root.to_path_buf(),
                            ExternalSourceRootsCacheEntry::Building {
                                revision,
                                completion: completion.clone(),
                            },
                        );
                        (Some(revision), Some(completion), None)
                    }
                }
            };

            if let Some(receiver) = wait_for.as_mut() {
                if !*receiver.borrow_and_update() {
                    let _ = receiver.changed().await;
                }
                continue;
            }

            let revision = build_revision.expect("build revision");
            let completion = build_completion.expect("build completion");
            let built = self
                .build_authorized_external_source_roots(workspace_root)
                .await;
            let mut cache = self.external_source_roots_cache.lock().await;
            let current_revision = cache.revisions.get(workspace_root).copied().unwrap_or(0);
            let may_publish = current_revision == revision
                && matches!(
                    cache.entries.get(workspace_root),
                    Some(ExternalSourceRootsCacheEntry::Building {
                        revision: pending_revision,
                        ..
                    }) if *pending_revision == revision
                );
            if may_publish {
                cache.entries.insert(
                    workspace_root.to_path_buf(),
                    ExternalSourceRootsCacheEntry::Ready {
                        revision,
                        value: built.clone(),
                    },
                );
                let _ = completion.send(true);
                return built;
            }
            let _ = completion.send(true);
        }
    }

    #[cfg(test)]
    async fn build_authorized_external_source_roots(
        &self,
        workspace_root: &Path,
    ) -> Arc<AuthorizedExternalSourceRoots> {
        let client_include_paths = self.include_paths.lock().await.clone();
        let client_go_module_paths = self.go_module_paths.lock().await.clone();
        let workspace = self.workspace_root_config(workspace_root).await.workspace;
        let include_entries =
            configured_include_paths(&workspace.include_paths, &client_include_paths);
        let go_module_entries =
            configured_include_paths(&workspace.go_module_paths, &client_go_module_paths);
        let workspace_root = workspace_root.to_path_buf();
        let built = tokio::task::spawn_blocking(move || {
            let (include_roots, _include_issues) =
                crate::config::resolve_include_roots(&include_entries);
            let (go_module_roots, _go_module_issues) =
                crate::config::resolve_go_module_roots(&go_module_entries);
            AuthorizedExternalSourceRoots {
                include_roots: authorized_external_root_pairs(include_roots, None, false),
                go_module_roots: authorized_external_root_pairs(
                    go_module_roots,
                    workspace_root.canonicalize().ok().as_deref(),
                    true,
                ),
                ..Default::default()
            }
        })
        .await
        .unwrap_or_default();
        Arc::new(built)
    }

    #[cfg(test)]
    pub(in crate::server) async fn invalidate_external_source_root_cache(&self, roots: &[PathBuf]) {
        if roots.is_empty() {
            return;
        }
        let mut cache = self.external_source_roots_cache.lock().await;
        for root in roots {
            let revision = cache.revisions.entry(root.clone()).or_default();
            *revision = revision.wrapping_add(1).max(1);
            if let Some(ExternalSourceRootsCacheEntry::Building { completion, .. }) =
                cache.entries.remove(root)
            {
                let _ = completion.send(true);
            }
        }
    }
}

pub(in crate::server) fn authorized_workspace_source_path(
    workspace_root: &Path,
    path: &Path,
) -> Option<PathBuf> {
    let canonical_root = workspace_root.canonicalize().ok()?;
    let canonical = canonicalize_source_path(path)?;
    pathing::path_is_within(&canonical_root, &canonical).then_some(canonical)
}

pub(in crate::server) fn canonicalize_source_path(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    if let Ok(canonical) = path.canonicalize() {
        return Some(canonical);
    }
    for ancestor in path.ancestors().skip(1) {
        let Ok(mut canonical) = ancestor.canonicalize() else {
            continue;
        };
        let suffix = path.strip_prefix(ancestor).ok()?;
        for component in suffix.components() {
            match component {
                std::path::Component::Normal(segment) => canonical.push(segment),
                std::path::Component::CurDir => {}
                _ => return None,
            }
        }
        return Some(canonical);
    }
    None
}

fn authorized_external_root_pairs(
    roots: Vec<PathBuf>,
    workspace_root: Option<&Path>,
    dedupe_canonical: bool,
) -> Vec<AuthorizedExternalSourceRoot> {
    let mut seen = HashSet::new();
    roots
        .into_iter()
        .filter_map(|identity_root| {
            let canonical_root = identity_root.canonicalize().ok()?;
            if workspace_root.is_some_and(|workspace| {
                pathing::path_is_within(workspace, &canonical_root)
                    || pathing::path_is_within(&canonical_root, workspace)
            }) {
                return None;
            }
            let identity = pathing::normalize_abs_path(&canonical_root).to_ascii_lowercase();
            (!dedupe_canonical || seen.insert(identity)).then_some(AuthorizedExternalSourceRoot {
                identity_root,
                canonical_root,
            })
        })
        .collect()
}

fn authorized_source_identities(
    roots: &[AuthorizedExternalSourceRoot],
    canonical_path: &Path,
    go_only: bool,
) -> Vec<AuthorizedExternalSourceIdentity> {
    roots
        .iter()
        .filter_map(|root| {
            let suffix = relative_path_under(&root.canonical_root, canonical_path)?;
            Some(AuthorizedExternalSourceIdentity {
                identity_path: pathing::normalize_abs_path(&root.identity_root.join(suffix)),
                go_only,
            })
        })
        .collect()
}

fn dedupe_authorized_identities(identities: &mut Vec<AuthorizedExternalSourceIdentity>) {
    let mut positions: HashMap<String, usize> = HashMap::new();
    let mut deduped: Vec<AuthorizedExternalSourceIdentity> = Vec::with_capacity(identities.len());
    for identity in identities.drain(..) {
        let key = if cfg!(windows) {
            identity.identity_path.to_ascii_lowercase()
        } else {
            identity.identity_path.clone()
        };
        if let Some(index) = positions.get(&key).copied() {
            if !identity.go_only {
                deduped[index].go_only = false;
            }
        } else {
            positions.insert(key, deduped.len());
            deduped.push(identity);
        }
    }
    *identities = deduped;
}

fn relative_path_under(root: &Path, path: &Path) -> Option<PathBuf> {
    if let Ok(relative) = path.strip_prefix(root) {
        return Some(relative.to_path_buf());
    }
    #[cfg(windows)]
    if pathing::path_is_within(root, path) {
        return Some(path.components().skip(root.components().count()).collect());
    }
    None
}

fn external_path_cache_key(path: &Path) -> String {
    let key = pathing::normalize_abs_path(path);
    if cfg!(windows) {
        key.to_ascii_lowercase()
    } else {
        key
    }
}

fn relative_path_under_spelling(root: &Path, path: &Path) -> Option<PathBuf> {
    if let Some(relative) = relative_path_under(root, path) {
        return Some(relative);
    }
    #[cfg(windows)]
    {
        fn normalized(path: &Path) -> String {
            let path = pathing::normalize_abs_path(path);
            let path = if path
                .get(..8)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("//?/UNC/"))
            {
                format!("//{}", &path[8..])
            } else if let Some(stripped) = path.strip_prefix("//?/") {
                stripped.to_string()
            } else {
                path
            };
            path.trim_end_matches('/').to_string()
        }

        let root = normalized(root);
        let path = normalized(path);
        if path.eq_ignore_ascii_case(&root) {
            return Some(PathBuf::new());
        }
        let prefix = format!("{root}/");
        if path.len() > prefix.len()
            && path
                .get(..prefix.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(&prefix))
        {
            return Some(PathBuf::from(&path[prefix.len()..]));
        }
    }
    None
}
