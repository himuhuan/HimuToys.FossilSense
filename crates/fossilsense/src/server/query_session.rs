use super::*;

#[derive(Clone, Debug, Default, serde::Serialize)]
pub(super) struct BindingObservation {
    pub feature: &'static str,
    pub total_us: u128,
    pub capture_us: u128,
    pub parse_us: u128,
    pub cache_hit: bool,
    pub binding_us: u128,
    pub overlay_us: u128,
    pub query_us: u128,
    pub hydration_us: u128,
    pub render_us: u128,
    pub sqlite_read_sessions: usize,
    pub completed: bool,
    pub returned: bool,
}

/// Records the whole request even when its future is dropped at an await.
pub(super) struct BindingTimer<'a> {
    backend: &'a Backend,
    started: std::time::Instant,
    pub observation: BindingObservation,
    pub reads: Arc<std::sync::atomic::AtomicUsize>,
}

impl<'a> BindingTimer<'a> {
    pub fn new(backend: &'a Backend, feature: &'static str) -> Self {
        Self {
            backend,
            started: std::time::Instant::now(),
            observation: BindingObservation {
                feature,
                ..Default::default()
            },
            reads: Default::default(),
        }
    }

    pub async fn log(&self) {
        self.backend
            .perf_log(|| {
                let mut observation = self.observation.clone();
                observation.total_us = self.started.elapsed().as_micros();
                observation.sqlite_read_sessions = self.reads.load(Ordering::Relaxed);
                format!(
                    "[perf] cursor_binding {}",
                    serde_json::to_string(&observation).unwrap_or_default()
                )
            })
            .await;
    }
}

impl Drop for BindingTimer<'_> {
    fn drop(&mut self) {
        self.observation.total_us = self.started.elapsed().as_micros();
        self.observation.sqlite_read_sessions = self.reads.load(Ordering::Relaxed);
        let mut observations = self
            .backend
            .session
            .binding_observations
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if observations.len() == 128 {
            observations.pop_front();
        }
        observations.push_back(self.observation.clone());
        drop(observations);
    }
}

/// One owned engine/document capture. All later parsing and overlay work uses
/// these exact inputs; a generation number alone cannot identify an engine.
pub(super) struct QuerySession {
    pub root: PathBuf,
    pub context: RequestContext,
    pub documents: DocumentRequestSnapshot,
}

pub(super) struct CursorBinding {
    pub parsed: Arc<FileSemanticIndex>,
    pub syntax: parser::CursorSyntax,
    pub resolution: query::BindingResolution<query::LocalBindingRef>,
    pub parse_us: u128,
    pub binding_us: u128,
    pub cache_hit: bool,
}

impl QuerySession {
    pub(super) async fn bind_cursor(
        &self,
        backend: &Backend,
        uri: &Url,
        document: (i32, Arc<str>),
        word: &str,
        byte: usize,
    ) -> Option<CursorBinding> {
        let (version, text) = document;
        let path = uri_to_path(uri)?;
        let selection = self
            .context
            .engine
            .workspace_semantics
            .selection_for_uri(uri, &text);
        let identity_path = if selection.language == SourceLanguage::Go {
            PathBuf::from(pathing::relative_slash_path(&self.root, &path).ok()?)
        } else {
            path
        };
        let started = std::time::Instant::now();
        let (parsed, cache_event) = backend
            .get_or_parse_captured_document_with_selection_observed(
                uri,
                &identity_path,
                version,
                &text,
                parser::ParseFacts::CURSOR | parser::ParseFacts::LOCAL_DECLS,
                selection,
            )
            .await?;
        let parse_us = started.elapsed().as_micros();
        let started = std::time::Instant::now();
        let syntax = parsed.cursor.at(byte)?.clone();
        let resolution = query::resolve_local_cursor(&parsed, &syntax, word, byte, version);
        Some(CursorBinding {
            parsed,
            syntax,
            resolution,
            parse_us,
            binding_us: started.elapsed().as_micros(),
            cache_hit: !matches!(cache_event, LiveParseCacheEvent::Miss),
        })
    }
}

impl Backend {
    pub(super) async fn capture_query_session(&self, uri: &Url) -> Option<QuerySession> {
        self.capture_query_session_with_hook(uri, |_, _| std::future::ready(()))
            .await
    }

    pub(super) async fn capture_query_session_with_hook<F, Fut>(
        &self,
        uri: &Url,
        mut hook: F,
    ) -> Option<QuerySession>
    where
        F: FnMut(usize, bool) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let root = self.root_for_uri(uri).await?;
        for attempt in 0..3 {
            let context = self.request_context_for_root(root.clone()).await;
            hook(attempt, false).await;
            let documents = self
                .session
                .documents
                .capture_request_snapshot(Some(uri))
                .await;
            hook(attempt, true).await;
            let after = self.request_context_for_root(root.clone()).await;
            let both_empty = context.engine.epoch == state::EngineEpoch::missing()
                && after.engine.epoch == state::EngineEpoch::missing();
            if both_empty || Arc::ptr_eq(&context.engine, &after.engine) {
                return Some(QuerySession {
                    root,
                    context,
                    documents,
                });
            }
        }
        None
    }
}
