//! Bounded asynchronous external-file existence cache; no candidate semantics.
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const EXTERNAL_PATH_PROBE_QUEUE_CAPACITY: usize = 64;
const EXTERNAL_PATH_PROBE_CACHE_CAPACITY: usize = 1_024;
const EXTERNAL_PATH_PROBE_MAX_PATH_BYTES: usize = 4 * 1_024;
const EXTERNAL_PATH_PROBE_PER_VIEW_ENQUEUE_LIMIT: usize = 16;
pub(super) const EXTERNAL_PATH_PROBE_OBSERVATION_LIMIT: usize = 64;
const EXTERNAL_PATH_PRESENT_TTL: Duration = Duration::from_secs(30);
const EXTERNAL_PATH_MISSING_TTL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy)]
enum ExternalPathProbeStatus {
    Pending,
    Deferred,
    Ready {
        exists: bool,
        valid_until: Instant,
        version: u64,
    },
}

#[derive(Debug)]
struct ExternalPathProbeEntry {
    status: ExternalPathProbeStatus,
    last_used: u64,
}

#[derive(Debug, Default)]
struct ExternalPathProbeState {
    entries: HashMap<String, ExternalPathProbeEntry>,
    deferred_paths: VecDeque<String>,
    deferred_set: HashSet<String>,
    clock: u64,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum ExternalPathProbeLookup {
    NotApplicable,
    Pending,
    Ready {
        exists: bool,
        valid_until: Instant,
        version: u64,
    },
}

#[derive(Debug, Clone)]
pub(super) struct ExternalPathProbeObservation {
    pub(super) path: String,
    pub(super) version: u64,
    pub(super) valid_until: Instant,
}

/// Process-wide, bounded exact-path probe cache for configured external roots
/// that intentionally remain path-resolution-only. The completion request
/// only performs an in-memory lookup and a non-blocking enqueue; one background
/// worker owns every filesystem probe across engine generations.
pub(super) struct ExternalPathProbeCache {
    state: Arc<Mutex<ExternalPathProbeState>>,
    sender: SyncSender<String>,
}

impl ExternalPathProbeCache {
    fn new() -> Self {
        let state = Arc::new(Mutex::new(ExternalPathProbeState::default()));
        let (sender, receiver) = sync_channel::<String>(EXTERNAL_PATH_PROBE_QUEUE_CAPACITY);
        let worker_state = state.clone();
        let _ = std::thread::Builder::new()
            .name("fossilsense-path-probe".to_string())
            .spawn(move || {
                let mut deferred_path = None;
                loop {
                    let path = match deferred_path.take() {
                        Some(path) => path,
                        None => match receiver.recv() {
                            Ok(path) => path,
                            Err(_) => break,
                        },
                    };
                    let exists = Path::new(&path).is_file();
                    let valid_until = Instant::now()
                        + if exists {
                            EXTERNAL_PATH_PRESENT_TTL
                        } else {
                            EXTERNAL_PATH_MISSING_TTL
                        };
                    let mut state = worker_state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    state.clock = state.clock.wrapping_add(1).max(1);
                    let last_used = state.clock;
                    if let Some(entry) = state.entries.get_mut(&path) {
                        entry.status = ExternalPathProbeStatus::Ready {
                            exists,
                            valid_until,
                            version: last_used,
                        };
                        entry.last_used = last_used;
                    }
                    while let Some(candidate) = state.deferred_paths.pop_front() {
                        state.deferred_set.remove(&candidate);
                        let Some(entry) = state.entries.get_mut(&candidate) else {
                            continue;
                        };
                        if matches!(entry.status, ExternalPathProbeStatus::Deferred) {
                            entry.status = ExternalPathProbeStatus::Pending;
                            deferred_path = Some(candidate);
                            break;
                        }
                    }
                }
            });
        Self { state, sender }
    }

    pub(super) fn lookup_or_schedule(
        &self,
        candidate: &str,
        enqueue_attempts: &AtomicUsize,
    ) -> ExternalPathProbeLookup {
        if candidate.len() > EXTERNAL_PATH_PROBE_MAX_PATH_BYTES
            || !Path::new(candidate).is_absolute()
        {
            return ExternalPathProbeLookup::NotApplicable;
        }

        let now = Instant::now();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.clock = state.clock.wrapping_add(1).max(1);
        let last_used = state.clock;
        if let Some(entry) = state.entries.get_mut(candidate) {
            entry.last_used = last_used;
            match entry.status {
                ExternalPathProbeStatus::Pending | ExternalPathProbeStatus::Deferred => {
                    return ExternalPathProbeLookup::Pending;
                }
                ExternalPathProbeStatus::Ready {
                    exists,
                    valid_until,
                    version,
                } if valid_until > now => {
                    return ExternalPathProbeLookup::Ready {
                        exists,
                        valid_until,
                        version,
                    };
                }
                ExternalPathProbeStatus::Ready { .. } => {}
            }
        }
        state.entries.remove(candidate);

        if state.entries.len() >= EXTERNAL_PATH_PROBE_CACHE_CAPACITY {
            let evicted = state
                .entries
                .iter()
                .filter(|(_, entry)| matches!(entry.status, ExternalPathProbeStatus::Ready { .. }))
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(path, _)| path.clone());
            if let Some(evicted) = evicted {
                state.entries.remove(&evicted);
            } else {
                return ExternalPathProbeLookup::Pending;
            }
        }

        if enqueue_attempts
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |attempts| {
                (attempts < EXTERNAL_PATH_PROBE_PER_VIEW_ENQUEUE_LIMIT)
                    .then_some(attempts.saturating_add(1))
            })
            .is_err()
        {
            let candidate = candidate.to_string();
            state.entries.insert(
                candidate.clone(),
                ExternalPathProbeEntry {
                    status: ExternalPathProbeStatus::Deferred,
                    last_used,
                },
            );
            if state.deferred_set.insert(candidate.clone()) {
                state.deferred_paths.push_back(candidate);
            }
            return ExternalPathProbeLookup::Pending;
        }

        state.entries.insert(
            candidate.to_string(),
            ExternalPathProbeEntry {
                status: ExternalPathProbeStatus::Pending,
                last_used,
            },
        );
        drop(state);

        match self.sender.try_send(candidate.to_string()) {
            Ok(()) => ExternalPathProbeLookup::Pending,
            Err(TrySendError::Full(path) | TrySendError::Disconnected(path)) => {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(entry) = state.entries.get_mut(&path) {
                    if matches!(entry.status, ExternalPathProbeStatus::Pending) {
                        entry.status = ExternalPathProbeStatus::Deferred;
                        if state.deferred_set.insert(path.clone()) {
                            state.deferred_paths.push_back(path);
                        }
                    }
                }
                ExternalPathProbeLookup::Pending
            }
        }
    }

    pub(super) fn observations_are_current(
        &self,
        observations: &[ExternalPathProbeObservation],
    ) -> bool {
        let now = Instant::now();
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        observations.iter().all(|observation| {
            observation.valid_until > now
                && state.entries.get(&observation.path).is_some_and(|entry| {
                    matches!(
                        entry.status,
                        ExternalPathProbeStatus::Ready {
                            valid_until,
                            version,
                            ..
                        } if valid_until > now && version == observation.version
                    )
                })
        })
    }
}

pub(super) fn external_path_probe_cache() -> &'static ExternalPathProbeCache {
    static CACHE: OnceLock<ExternalPathProbeCache> = OnceLock::new();
    CACHE.get_or_init(ExternalPathProbeCache::new)
}
