use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

pub(crate) const DEFAULT_MAX_ACTIVE_BUILDS: usize = 1;
pub(crate) const DEFAULT_TEMPORARY_RESERVATION_BYTES: usize = 256 * 1024 * 1024;
pub(crate) const DEFAULT_PROCESS_PRESSURE_TARGET_BYTES: usize = 512 * 1024 * 1024;
pub(crate) const DEFAULT_UNATTRIBUTED_HEADROOM_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const DEFAULT_FIRST_BUILD_RESERVATION_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const DEFAULT_INCREMENTAL_BUILD_RESERVATION_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFERRED_RETRY_BACKOFF: Duration = Duration::from_secs(2);
pub(crate) const CANCELLATION_CHECK_INTERVAL: usize = 1_024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BuildKind {
    FullIndex,
    DirtyIndex,
    ReadModel,
    NameCompaction,
    ProjectContext,
    WorkspaceSemantics,
    CliIndex,
    CliMemory,
}

#[derive(Clone, Debug)]
pub(crate) struct BuildCancellation {
    inner: Arc<CancellationInner>,
}

#[derive(Debug)]
struct CancellationInner {
    cancelled: AtomicBool,
    notify: Notify,
    #[cfg(test)]
    checks_before_cancel: AtomicUsize,
}

impl Default for CancellationInner {
    fn default() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            notify: Notify::new(),
            #[cfg(test)]
            checks_before_cancel: AtomicUsize::new(usize::MAX),
        }
    }
}

impl Default for BuildCancellation {
    fn default() -> Self {
        Self::new()
    }
}

impl BuildCancellation {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(CancellationInner::default()),
        }
    }

    pub(crate) fn cancel(&self) {
        if !self.inner.cancelled.swap(true, Ordering::AcqRel) {
            self.inner.notify.notify_waiters();
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        #[cfg(test)]
        {
            let remaining = self.inner.checks_before_cancel.load(Ordering::Acquire);
            if remaining != usize::MAX && remaining > 0 {
                let previous = self
                    .inner
                    .checks_before_cancel
                    .fetch_sub(1, Ordering::AcqRel);
                if previous == 1 {
                    self.cancel();
                }
            }
        }
        self.inner.cancelled.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn cancel_after_checks_for_test(&self, checks: usize) {
        self.inner
            .checks_before_cancel
            .store(checks.max(1), Ordering::Release);
    }

    pub(crate) async fn cancelled(&self) {
        while !self.is_cancelled() {
            self.inner.notify.notified().await;
        }
    }
}

impl BuildKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::FullIndex => "full-index",
            Self::DirtyIndex => "dirty-index",
            Self::ReadModel => "read-model",
            Self::NameCompaction => "name-compaction",
            Self::ProjectContext => "project-context",
            Self::WorkspaceSemantics => "workspace-semantics",
            Self::CliIndex => "cli-index",
            Self::CliMemory => "cli-memory",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BuildPolicy {
    pub(crate) max_active_builds: usize,
    pub(crate) temporary_reservation_bytes: usize,
    pub(crate) process_pressure_target_bytes: usize,
    pub(crate) unattributed_headroom_bytes: usize,
    pub(crate) wait_timeout: Duration,
}

impl Default for BuildPolicy {
    fn default() -> Self {
        Self {
            max_active_builds: DEFAULT_MAX_ACTIVE_BUILDS,
            temporary_reservation_bytes: DEFAULT_TEMPORARY_RESERVATION_BYTES,
            process_pressure_target_bytes: DEFAULT_PROCESS_PRESSURE_TARGET_BYTES,
            unattributed_headroom_bytes: DEFAULT_UNATTRIBUTED_HEADROOM_BYTES,
            wait_timeout: DEFAULT_WAIT_TIMEOUT,
        }
    }
}

#[derive(Clone)]
pub(crate) struct BuildCoordinator {
    inner: Arc<CoordinatorInner>,
}

impl fmt::Debug for BuildCoordinator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BuildCoordinator")
            .field("policy", &self.inner.policy)
            .field("snapshot", &self.snapshot())
            .finish()
    }
}

struct CoordinatorInner {
    state: Mutex<CoordinatorState>,
    notify: Notify,
    policy: BuildPolicy,
    memory_sampler: Arc<dyn Fn() -> u64 + Send + Sync>,
}

#[derive(Default)]
struct CoordinatorState {
    next_ticket: u64,
    active: Option<ActiveBuild>,
    waiters: VecDeque<QueuedBuild>,
    last_granted_root: Option<PathBuf>,
    deferred: HashMap<PathBuf, DeferredBuild>,
    compactions: HashMap<PathBuf, CompactionState>,
    shutting_down: bool,
    peak_active_builds: usize,
    peak_reserved_bytes: usize,
    peak_retained_bytes: usize,
    sampling_unavailable_count: u64,
    budget_denial_count: u64,
    cancellation_count: u64,
    last_index_elapsed_ms: u128,
    last_index_write_ms: u128,
}

struct ActiveBuild {
    ticket: u64,
    root: PathBuf,
    kind: BuildKind,
    reserved_bytes: usize,
    retained_bytes: usize,
    cancellation: BuildCancellation,
}

struct QueuedBuild {
    ticket: u64,
    root: PathBuf,
    kind: BuildKind,
    cancellation: BuildCancellation,
}

#[derive(Clone, Copy)]
struct DeferredBuild {
    kind: BuildKind,
    requested_bytes: usize,
    retry_after: Instant,
}

#[derive(Default)]
struct CompactionState {
    worker_running: bool,
    current_epoch: Option<u64>,
    latest_epoch: u64,
    current_cancellation: Option<BuildCancellation>,
}

struct PermitLease {
    coordinator: BuildCoordinator,
    ticket: u64,
    root: PathBuf,
    cancellation: BuildCancellation,
}

impl Drop for PermitLease {
    fn drop(&mut self) {
        self.coordinator.release(self.ticket);
    }
}

#[derive(Clone)]
pub(crate) struct BuildPermit {
    lease: Arc<PermitLease>,
    stage: BuildKind,
}

impl fmt::Debug for BuildPermit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BuildPermit")
            .field("root", &self.lease.root)
            .field("stage", &self.stage)
            .finish()
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TryAcquireError {
    Busy,
    ShuttingDown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AcquireError {
    Cancelled,
    Deferred,
    ShuttingDown,
}

impl fmt::Display for AcquireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("build admission cancelled"),
            Self::Deferred => formatter.write_str("build admission deferred after waiting"),
            Self::ShuttingDown => formatter.write_str("build coordinator is shutting down"),
        }
    }
}

impl std::error::Error for AcquireError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReservationError {
    Cancelled,
    TemporaryBudgetExceeded,
    ProcessPressure,
    StalePermit,
}

impl fmt::Display for ReservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("build reservation cancelled"),
            Self::TemporaryBudgetExceeded => {
                formatter.write_str("temporary build reservation exceeds 256 MiB")
            }
            Self::ProcessPressure => {
                formatter.write_str("process pressure target leaves insufficient build space")
            }
            Self::StalePermit => formatter.write_str("build permit is no longer active"),
        }
    }
}

impl std::error::Error for ReservationError {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BuildCoordinatorSnapshot {
    pub(crate) active_builds: usize,
    pub(crate) active_kind: Option<BuildKind>,
    pub(crate) peak_active_builds: usize,
    pub(crate) waiting_builds: usize,
    pub(crate) deferred_roots: usize,
    pub(crate) deferred_requested_bytes: usize,
    pub(crate) active_reserved_bytes: usize,
    pub(crate) active_retained_bytes: usize,
    pub(crate) peak_reserved_bytes: usize,
    pub(crate) peak_retained_bytes: usize,
    pub(crate) compaction_roots: usize,
    pub(crate) sampling_unavailable_count: u64,
    pub(crate) budget_denial_count: u64,
    pub(crate) cancellation_count: u64,
    pub(crate) sampling_available: bool,
    pub(crate) process_memory_bytes: usize,
    pub(crate) shutting_down: bool,
    pub(crate) last_index_elapsed_ms: u128,
    pub(crate) last_index_write_ms: u128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompactionRequest {
    StartWorker,
    UpdatedPending,
    IgnoredStale,
}

impl Default for BuildCoordinator {
    fn default() -> Self {
        Self::with_policy_and_sampler(BuildPolicy::default(), || {
            crate::resource::current_process_memory_bytes()
        })
    }
}

impl BuildCoordinator {
    pub(crate) fn with_policy_and_sampler(
        policy: BuildPolicy,
        memory_sampler: impl Fn() -> u64 + Send + Sync + 'static,
    ) -> Self {
        assert!(policy.max_active_builds > 0);
        Self {
            inner: Arc::new(CoordinatorInner {
                state: Mutex::new(CoordinatorState::default()),
                notify: Notify::new(),
                policy,
                memory_sampler: Arc::new(memory_sampler),
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn try_acquire(
        &self,
        root: PathBuf,
        kind: BuildKind,
    ) -> Result<BuildPermit, TryAcquireError> {
        let cancellation = BuildCancellation::new();
        let mut state = self.lock_state();
        if state.shutting_down {
            return Err(TryAcquireError::ShuttingDown);
        }
        if state.active.is_some() || !state.waiters.is_empty() {
            return Err(TryAcquireError::Busy);
        }
        let ticket = Self::next_ticket(&mut state);
        Ok(self.grant(&mut state, ticket, root, kind, cancellation))
    }

    pub(crate) async fn acquire(
        &self,
        root: PathBuf,
        kind: BuildKind,
        cancellation: BuildCancellation,
    ) -> Result<BuildPermit, AcquireError> {
        if kind != BuildKind::NameCompaction {
            let mut state = self.lock_state();
            if state.shutting_down {
                return Err(AcquireError::ShuttingDown);
            }
            let stale_compaction = state.active.as_ref().is_some_and(|active| {
                active.kind == BuildKind::NameCompaction && !active.cancellation.is_cancelled()
            });
            if stale_compaction {
                state
                    .active
                    .as_ref()
                    .expect("active compaction checked")
                    .cancellation
                    .cancel();
                state.cancellation_count = state.cancellation_count.saturating_add(1);
            }
        }
        loop {
            let notified = self.inner.notify.notified();
            tokio::pin!(notified);
            let retry_after = {
                let state = self.lock_state();
                if state.shutting_down {
                    return Err(AcquireError::ShuttingDown);
                }
                state
                    .deferred
                    .get(&root)
                    .map(|deferred| deferred.retry_after)
            };
            let Some(retry_after) = retry_after.filter(|retry_after| *retry_after > Instant::now())
            else {
                break;
            };
            tokio::select! {
                _ = &mut notified => {}
                _ = cancellation.cancelled() => return Err(AcquireError::Cancelled),
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(retry_after)) => {}
            }
        }
        let ticket = {
            let mut state = self.lock_state();
            if state.shutting_down {
                return Err(AcquireError::ShuttingDown);
            }
            if kind != BuildKind::NameCompaction {
                let stale_compaction = state.active.as_ref().is_some_and(|active| {
                    active.kind == BuildKind::NameCompaction && !active.cancellation.is_cancelled()
                });
                if stale_compaction {
                    state
                        .active
                        .as_ref()
                        .expect("active compaction checked")
                        .cancellation
                        .cancel();
                    state.cancellation_count = state.cancellation_count.saturating_add(1);
                }
            }
            let ticket = Self::next_ticket(&mut state);
            state.waiters.push_back(QueuedBuild {
                ticket,
                root,
                kind,
                cancellation: cancellation.clone(),
            });
            ticket
        };
        self.inner.notify.notify_waiters();
        let deadline = tokio::time::Instant::now() + self.inner.policy.wait_timeout;

        loop {
            let notified = self.inner.notify.notified();
            tokio::pin!(notified);
            {
                let mut state = self.lock_state();
                if state.shutting_down {
                    Self::remove_waiter(&mut state, ticket);
                    return Err(AcquireError::ShuttingDown);
                }
                if cancellation.is_cancelled() {
                    Self::remove_waiter(&mut state, ticket);
                    state.cancellation_count = state.cancellation_count.saturating_add(1);
                    return Err(AcquireError::Cancelled);
                }
                if state.active.is_none() {
                    if let Some(index) = Self::next_waiter_index(&state) {
                        if state.waiters[index].ticket == ticket {
                            let queued = state
                                .waiters
                                .remove(index)
                                .expect("selected build waiter must exist");
                            return Ok(self.grant(
                                &mut state,
                                ticket,
                                queued.root,
                                queued.kind,
                                queued.cancellation,
                            ));
                        }
                    }
                }
            }

            tokio::select! {
                _ = &mut notified => {}
                _ = cancellation.cancelled() => {}
                _ = tokio::time::sleep_until(deadline) => {
                    let mut state = self.lock_state();
                    let removed = Self::remove_waiter(&mut state, ticket);
                    if let Some(queued) = removed {
                        state.deferred.insert(
                            queued.root,
                            DeferredBuild {
                                kind: queued.kind,
                                requested_bytes: 0,
                                retry_after: Instant::now() + DEFERRED_RETRY_BACKOFF,
                            },
                        );
                    }
                    return Err(AcquireError::Deferred);
                }
            }
        }
    }

    pub(crate) fn snapshot(&self) -> BuildCoordinatorSnapshot {
        let sampled = (self.inner.memory_sampler)();
        let state = self.lock_state();
        let active = state.active.as_ref();
        BuildCoordinatorSnapshot {
            active_builds: usize::from(active.is_some()),
            active_kind: active.map(|build| build.kind),
            peak_active_builds: state.peak_active_builds,
            waiting_builds: state.waiters.len(),
            deferred_roots: state.deferred.len(),
            deferred_requested_bytes: state.deferred.values().fold(0usize, |bytes, deferred| {
                let _kind = deferred.kind;
                bytes.saturating_add(deferred.requested_bytes)
            }),
            active_reserved_bytes: active.map_or(0, |build| build.reserved_bytes),
            active_retained_bytes: active.map_or(0, |build| build.retained_bytes),
            peak_reserved_bytes: state.peak_reserved_bytes,
            peak_retained_bytes: state.peak_retained_bytes,
            compaction_roots: state.compactions.len(),
            sampling_unavailable_count: state.sampling_unavailable_count,
            budget_denial_count: state.budget_denial_count,
            cancellation_count: state.cancellation_count,
            sampling_available: sampled != 0,
            process_memory_bytes: usize::try_from(sampled).unwrap_or(usize::MAX),
            shutting_down: state.shutting_down,
            last_index_elapsed_ms: state.last_index_elapsed_ms,
            last_index_write_ms: state.last_index_write_ms,
        }
    }

    pub(crate) fn record_index_stats(&self, elapsed_ms: u128, write_ms: u128) {
        let mut state = self.lock_state();
        state.last_index_elapsed_ms = elapsed_ms;
        state.last_index_write_ms = write_ms;
    }

    pub(crate) fn shutdown(&self) {
        let mut state = self.lock_state();
        state.shutting_down = true;
        if let Some(active) = &state.active {
            active.cancellation.cancel();
        }
        for waiter in &state.waiters {
            waiter.cancellation.cancel();
        }
        for compaction in state.compactions.values() {
            if let Some(cancellation) = &compaction.current_cancellation {
                cancellation.cancel();
            }
        }
        drop(state);
        self.inner.notify.notify_waiters();
    }

    pub(crate) fn remove_root(&self, root: &Path) {
        let mut state = self.lock_state();
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.root == root)
        {
            state
                .active
                .as_ref()
                .expect("checked active build")
                .cancellation
                .cancel();
        }
        state.waiters.retain(|waiter| waiter.root != root);
        state.deferred.remove(root);
        if let Some(compaction) = state.compactions.remove(root) {
            if let Some(cancellation) = compaction.current_cancellation {
                cancellation.cancel();
            }
        }
        drop(state);
        self.inner.notify.notify_waiters();
    }

    pub(crate) fn request_compaction(&self, root: PathBuf, epoch: u64) -> CompactionRequest {
        let mut state = self.lock_state();
        if state.shutting_down {
            return CompactionRequest::IgnoredStale;
        }
        let compaction = state.compactions.entry(root).or_default();
        if epoch <= compaction.latest_epoch {
            return CompactionRequest::IgnoredStale;
        }
        compaction.latest_epoch = epoch;
        if compaction.worker_running {
            if let Some(cancellation) = &compaction.current_cancellation {
                cancellation.cancel();
            }
            CompactionRequest::UpdatedPending
        } else {
            compaction.worker_running = true;
            CompactionRequest::StartWorker
        }
    }

    pub(crate) fn begin_next_compaction(&self, root: &Path) -> Option<(u64, BuildCancellation)> {
        let mut state = self.lock_state();
        let compaction = state.compactions.get_mut(root)?;
        if compaction.current_epoch == Some(compaction.latest_epoch) {
            return None;
        }
        let cancellation = BuildCancellation::new();
        compaction.current_epoch = Some(compaction.latest_epoch);
        compaction.current_cancellation = Some(cancellation.clone());
        Some((compaction.latest_epoch, cancellation))
    }

    pub(crate) fn finish_compaction(&self, root: &Path, epoch: u64) -> bool {
        let mut state = self.lock_state();
        let Some(compaction) = state.compactions.get_mut(root) else {
            return false;
        };
        if compaction.current_epoch == Some(epoch) {
            compaction.current_epoch = None;
            compaction.current_cancellation = None;
        }
        if compaction.latest_epoch > epoch {
            true
        } else {
            state.compactions.remove(root);
            false
        }
    }

    pub(crate) fn retry_compaction(&self, root: &Path, epoch: u64) -> bool {
        let mut state = self.lock_state();
        if state.shutting_down {
            return false;
        }
        let Some(compaction) = state.compactions.get_mut(root) else {
            return false;
        };
        if compaction.current_epoch == Some(epoch) {
            compaction.current_epoch = None;
            compaction.current_cancellation = None;
        }
        true
    }

    fn grant(
        &self,
        state: &mut CoordinatorState,
        ticket: u64,
        root: PathBuf,
        kind: BuildKind,
        cancellation: BuildCancellation,
    ) -> BuildPermit {
        debug_assert!(state.active.is_none());
        state.last_granted_root = Some(root.clone());
        state.active = Some(ActiveBuild {
            ticket,
            root: root.clone(),
            kind,
            reserved_bytes: 0,
            retained_bytes: 0,
            cancellation: cancellation.clone(),
        });
        state.peak_active_builds = state.peak_active_builds.max(1);
        BuildPermit {
            lease: Arc::new(PermitLease {
                coordinator: self.clone(),
                ticket,
                root,
                cancellation,
            }),
            stage: kind,
        }
    }

    fn release(&self, ticket: u64) {
        let mut state = self.lock_state();
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.ticket == ticket)
        {
            state.active = None;
        }
        drop(state);
        self.inner.notify.notify_waiters();
    }

    fn next_ticket(state: &mut CoordinatorState) -> u64 {
        state.next_ticket = state.next_ticket.wrapping_add(1).max(1);
        state.next_ticket
    }

    fn next_waiter_index(state: &CoordinatorState) -> Option<usize> {
        let last_root = state.last_granted_root.as_ref();
        state
            .waiters
            .iter()
            .position(|waiter| Some(&waiter.root) != last_root)
            .or_else(|| (!state.waiters.is_empty()).then_some(0))
    }

    fn remove_waiter(state: &mut CoordinatorState, ticket: u64) -> Option<QueuedBuild> {
        let index = state
            .waiters
            .iter()
            .position(|waiter| waiter.ticket == ticket)?;
        state.waiters.remove(index)
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, CoordinatorState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl BuildPermit {
    pub(crate) fn inherit(&self, kind: BuildKind) -> Self {
        Self {
            lease: self.lease.clone(),
            stage: kind,
        }
    }

    pub(crate) fn cancellation(&self) -> BuildCancellation {
        self.lease.cancellation.clone()
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.lease.cancellation.is_cancelled()
    }

    pub(crate) fn check_cancelled(&self) -> Result<(), ReservationError> {
        if self.is_cancelled() {
            Err(ReservationError::Cancelled)
        } else {
            Ok(())
        }
    }

    pub(crate) fn reserve(
        &self,
        reserved_bytes: usize,
        retained_bytes: usize,
    ) -> Result<(), ReservationError> {
        self.check_cancelled()?;
        let coordinator = &self.lease.coordinator;
        let sampled = (coordinator.inner.memory_sampler)();
        let sampled = usize::try_from(sampled).unwrap_or(usize::MAX);
        let mut state = coordinator.lock_state();
        let policy = coordinator.inner.policy;
        if state
            .active
            .as_ref()
            .is_none_or(|active| active.ticket != self.lease.ticket)
        {
            return Err(ReservationError::StalePermit);
        }
        if reserved_bytes > policy.temporary_reservation_bytes {
            let kind = state.active.as_ref().expect("active permit checked").kind;
            state.budget_denial_count = state.budget_denial_count.saturating_add(1);
            state.deferred.insert(
                self.lease.root.clone(),
                DeferredBuild {
                    kind,
                    requested_bytes: reserved_bytes,
                    retry_after: Instant::now() + DEFERRED_RETRY_BACKOFF,
                },
            );
            return Err(ReservationError::TemporaryBudgetExceeded);
        }
        if sampled == 0 {
            state.sampling_unavailable_count = state.sampling_unavailable_count.saturating_add(1);
        }
        let accounted_live = sampled.max(retained_bytes);
        let pressure = accounted_live
            .saturating_add(reserved_bytes)
            .saturating_add(policy.unattributed_headroom_bytes);
        if pressure > policy.process_pressure_target_bytes {
            let kind = state.active.as_ref().expect("active permit checked").kind;
            state.budget_denial_count = state.budget_denial_count.saturating_add(1);
            state.deferred.insert(
                self.lease.root.clone(),
                DeferredBuild {
                    kind,
                    requested_bytes: reserved_bytes,
                    retry_after: Instant::now() + DEFERRED_RETRY_BACKOFF,
                },
            );
            return Err(ReservationError::ProcessPressure);
        }
        let active = state.active.as_mut().expect("active permit checked");
        active.reserved_bytes = reserved_bytes;
        active.retained_bytes = retained_bytes;
        state.peak_reserved_bytes = state.peak_reserved_bytes.max(reserved_bytes);
        state.peak_retained_bytes = state.peak_retained_bytes.max(retained_bytes);
        state.deferred.remove(&self.lease.root);
        Ok(())
    }

    pub(crate) async fn reserve_with_wait(
        &self,
        reserved_bytes: usize,
        retained_bytes: usize,
    ) -> Result<(), ReservationError> {
        let deadline =
            tokio::time::Instant::now() + self.lease.coordinator.inner.policy.wait_timeout;
        loop {
            match self.reserve(reserved_bytes, retained_bytes) {
                Ok(()) => return Ok(()),
                Err(ReservationError::ProcessPressure)
                    if tokio::time::Instant::now() < deadline =>
                {
                    let notified = self.lease.coordinator.inner.notify.notified();
                    tokio::pin!(notified);
                    tokio::select! {
                        _ = &mut notified => {}
                        _ = self.lease.cancellation.cancelled() => {
                            return Err(ReservationError::Cancelled);
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn release_reservation(&self) {
        let coordinator = &self.lease.coordinator;
        let mut state = coordinator.lock_state();
        if let Some(active) = state
            .active
            .as_mut()
            .filter(|active| active.ticket == self.lease.ticket)
        {
            active.reserved_bytes = 0;
            active.retained_bytes = 0;
        }
        drop(state);
        coordinator.inner.notify.notify_waiters();
    }
}
