use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
use tower_lsp::lsp_types::Url;

use crate::query::CompletionQueryCancellation;

#[derive(Clone)]
pub(super) struct CompletionRuntime {
    inner: Arc<CompletionRuntimeInner>,
}

struct CompletionRuntimeInner {
    next_request_id: AtomicU64,
    active: Mutex<HashMap<Url, ActiveRequest>>,
    foreground: Arc<Semaphore>,
    metrics: CompletionRuntimeMetricCounters,
}

#[derive(Clone)]
struct ActiveRequest {
    request_id: u64,
    cancellation: CompletionRequestCancellation,
}

#[derive(Default)]
struct CompletionRuntimeMetricCounters {
    started: AtomicU64,
    superseded: AtomicU64,
    cancelled_before_admission: AtomicU64,
    cancelled_before_worker: AtomicU64,
    workers_started: AtomicU64,
    workers_completed: AtomicU64,
    workers_cancelled: AtomicU64,
    worker_failures: AtomicU64,
    stale_entries_inspected: AtomicU64,
}

#[derive(Clone)]
pub(super) struct CompletionRequestCancellation {
    state: Arc<CompletionCancellationState>,
}

struct CompletionCancellationState {
    cancelled: AtomicBool,
    notify: Notify,
}

pub(super) struct CompletionRequest {
    runtime: CompletionRuntime,
    uri: Url,
    request_id: u64,
    cancellation: CompletionRequestCancellation,
    finished: bool,
}

pub(super) struct CompletionWorkerGuard {
    runtime: CompletionRuntime,
    cancellation: CompletionRequestCancellation,
    permit: Option<OwnedSemaphorePermit>,
    recorded: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct CompletionRuntimeMetrics {
    pub(super) started: u64,
    pub(super) superseded: u64,
    pub(super) cancelled_before_admission: u64,
    pub(super) cancelled_before_worker: u64,
    pub(super) workers_started: u64,
    pub(super) workers_completed: u64,
    pub(super) workers_cancelled: u64,
    pub(super) worker_failures: u64,
    pub(super) stale_entries_inspected: u64,
}

impl CompletionRuntimeMetrics {
    pub(super) fn perf_summary(
        self,
        stage: &str,
        document_version: i32,
        recall_ms: u128,
        request_entries_inspected: usize,
    ) -> String {
        format!(
            "[perf] completion_cancelled stage={stage} document_version={document_version} recall={}ms request_entries_inspected={} started={} superseded={} cancelled_before_admission={} cancelled_before_worker={} workers_started={} workers_completed={} workers_cancelled={} worker_failures={} stale_entries_inspected={}",
            recall_ms,
            request_entries_inspected,
            self.started,
            self.superseded,
            self.cancelled_before_admission,
            self.cancelled_before_worker,
            self.workers_started,
            self.workers_completed,
            self.workers_cancelled,
            self.worker_failures,
            self.stale_entries_inspected,
        )
    }
}

impl Default for CompletionRuntime {
    fn default() -> Self {
        let available = std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(1);
        // Preserve one logical foreground lane on small machines and cap
        // completion CPU fan-out at two. Background indexing retains its own
        // existing budget; this admission gate prevents bursts of stale
        // ordinary-completion jobs from filling Tokio's blocking pool.
        let permits = available.saturating_sub(1).clamp(1, 2);
        Self::with_permits(permits)
    }
}

impl CompletionRuntime {
    fn with_permits(permits: usize) -> Self {
        assert!(permits > 0, "completion runtime needs a foreground permit");
        Self {
            inner: Arc::new(CompletionRuntimeInner {
                next_request_id: AtomicU64::new(1),
                active: Mutex::new(HashMap::new()),
                foreground: Arc::new(Semaphore::new(permits)),
                metrics: CompletionRuntimeMetricCounters::default(),
            }),
        }
    }

    #[cfg(test)]
    pub(super) fn with_permits_for_test(permits: usize) -> Self {
        Self::with_permits(permits)
    }

    /// Register at the completion RPC entry point, before its first await.
    /// Request ids therefore reflect server request order rather than the
    /// order in which asynchronous context preparation happens to finish.
    pub(super) fn begin(&self, uri: Url) -> CompletionRequest {
        let request_id = self.inner.next_request_id.fetch_add(1, Ordering::Relaxed);
        let cancellation = CompletionRequestCancellation::new();
        let active = ActiveRequest {
            request_id,
            cancellation: cancellation.clone(),
        };
        let previous = self
            .inner
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(uri.clone(), active);
        if previous.is_some_and(|request| request.cancellation.cancel()) {
            self.inner
                .metrics
                .superseded
                .fetch_add(1, Ordering::Relaxed);
        }
        self.inner.metrics.started.fetch_add(1, Ordering::Relaxed);
        CompletionRequest {
            runtime: self.clone(),
            uri,
            request_id,
            cancellation,
            finished: false,
        }
    }

    /// Supersede any ordinary completion for this document as soon as a new
    /// document revision arrives, without waiting for the next completion RPC.
    pub(super) fn supersede(&self, uri: &Url) {
        let previous = self
            .inner
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(uri);
        if previous.is_some_and(|request| request.cancellation.cancel()) {
            self.inner
                .metrics
                .superseded
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    fn remove_if_current(&self, uri: &Url, request_id: u64) -> bool {
        let mut active = self
            .inner
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if active
            .get(uri)
            .is_some_and(|request| request.request_id == request_id)
        {
            active.remove(uri);
            true
        } else {
            false
        }
    }

    #[cfg(test)]
    fn is_current(&self, uri: &Url, request_id: u64) -> bool {
        self.inner
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(uri)
            .is_some_and(|request| request.request_id == request_id)
    }

    pub(super) fn metrics(&self) -> CompletionRuntimeMetrics {
        CompletionRuntimeMetrics {
            started: self.inner.metrics.started.load(Ordering::Relaxed),
            superseded: self.inner.metrics.superseded.load(Ordering::Relaxed),
            cancelled_before_admission: self
                .inner
                .metrics
                .cancelled_before_admission
                .load(Ordering::Relaxed),
            cancelled_before_worker: self
                .inner
                .metrics
                .cancelled_before_worker
                .load(Ordering::Relaxed),
            workers_started: self.inner.metrics.workers_started.load(Ordering::Relaxed),
            workers_completed: self.inner.metrics.workers_completed.load(Ordering::Relaxed),
            workers_cancelled: self.inner.metrics.workers_cancelled.load(Ordering::Relaxed),
            worker_failures: self.inner.metrics.worker_failures.load(Ordering::Relaxed),
            stale_entries_inspected: self
                .inner
                .metrics
                .stale_entries_inspected
                .load(Ordering::Relaxed),
        }
    }

    #[cfg(test)]
    pub(super) fn metrics_for_test(&self) -> CompletionRuntimeMetrics {
        self.metrics()
    }
}

impl CompletionRequestCancellation {
    fn new() -> Self {
        Self {
            state: Arc::new(CompletionCancellationState {
                cancelled: AtomicBool::new(false),
                notify: Notify::new(),
            }),
        }
    }

    fn cancel(&self) -> bool {
        if self.state.cancelled.swap(true, Ordering::AcqRel) {
            return false;
        }
        // `notify_one` stores a permit when the admission future has not been
        // polled yet, closing the check/register race for queued supersession.
        self.state.notify.notify_one();
        true
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::Acquire)
    }
}

impl CompletionQueryCancellation for CompletionRequestCancellation {
    fn is_cancelled(&self) -> bool {
        self.is_cancelled()
    }
}

impl CompletionRequest {
    pub(super) async fn acquire(&self) -> Option<OwnedSemaphorePermit> {
        if self.is_cancelled() {
            self.record_cancelled_before_admission();
            return None;
        }

        let notified = self.cancellation.state.notify.notified();
        tokio::pin!(notified);
        let permit = tokio::select! {
            permit = self.runtime.inner.foreground.clone().acquire_owned() => permit.ok(),
            _ = &mut notified => None,
        };
        let Some(permit) = permit else {
            self.record_cancelled_before_admission();
            return None;
        };
        if self.is_cancelled() {
            drop(permit);
            self.record_cancelled_before_admission();
            return None;
        }
        Some(permit)
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    #[cfg(test)]
    pub(super) fn is_current(&self) -> bool {
        !self.is_cancelled() && self.runtime.is_current(&self.uri, self.request_id)
    }

    /// Run a non-blocking publication step while holding the runtime's current
    /// request lock. A document event or newer request cannot linearize between
    /// the currentness check and the publication performed by `action`.
    pub(super) fn run_if_current<T>(&self, action: impl FnOnce() -> T) -> Option<T> {
        if self.is_cancelled() {
            return None;
        }
        let active = self
            .runtime
            .inner
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.is_cancelled()
            || active
                .get(&self.uri)
                .is_none_or(|request| request.request_id != self.request_id)
        {
            return None;
        }
        Some(action())
    }

    pub(super) fn cancellation(&self) -> CompletionRequestCancellation {
        self.cancellation.clone()
    }

    pub(super) fn stop_before_worker(&self) -> bool {
        if !self.is_cancelled() {
            return false;
        }
        self.runtime
            .inner
            .metrics
            .cancelled_before_worker
            .fetch_add(1, Ordering::Relaxed);
        true
    }

    pub(super) fn worker(&self, permit: OwnedSemaphorePermit) -> CompletionWorkerGuard {
        self.runtime
            .inner
            .metrics
            .workers_started
            .fetch_add(1, Ordering::Relaxed);
        CompletionWorkerGuard {
            runtime: self.runtime.clone(),
            cancellation: self.cancellation.clone(),
            permit: Some(permit),
            recorded: false,
        }
    }

    /// Mark a fully rendered result publishable only if this request is still
    /// the latest request for the document.
    pub(super) fn finish(&mut self) -> bool {
        if self.is_cancelled() {
            return false;
        }
        let current = self.runtime.remove_if_current(&self.uri, self.request_id);
        self.finished = current;
        current
    }

    fn record_cancelled_before_admission(&self) {
        self.runtime
            .inner
            .metrics
            .cancelled_before_admission
            .fetch_add(1, Ordering::Relaxed);
    }
}

impl Drop for CompletionRequest {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.cancellation.cancel();
        self.runtime.remove_if_current(&self.uri, self.request_id);
    }
}

impl CompletionWorkerGuard {
    pub(super) fn finish(mut self, cancelled: bool, entries_inspected: usize) {
        let cancelled = cancelled || self.cancellation.is_cancelled();
        if cancelled {
            self.runtime
                .inner
                .metrics
                .workers_cancelled
                .fetch_add(1, Ordering::Relaxed);
            self.runtime
                .inner
                .metrics
                .stale_entries_inspected
                .fetch_add(entries_inspected as u64, Ordering::Relaxed);
        } else {
            self.runtime
                .inner
                .metrics
                .workers_completed
                .fetch_add(1, Ordering::Relaxed);
        }
        self.recorded = true;
        self.permit.take();
    }
}

impl Drop for CompletionWorkerGuard {
    fn drop(&mut self) {
        if !self.recorded {
            self.runtime
                .inner
                .metrics
                .worker_failures
                .fetch_add(1, Ordering::Relaxed);
        }
        self.permit.take();
    }
}
