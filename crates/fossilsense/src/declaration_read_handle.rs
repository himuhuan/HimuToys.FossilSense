use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Result;

use crate::call_model::SemanticGeneration;
use crate::pathing::IndexDbLease;
use crate::store::IndexStore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclarationReadFailureReason {
    IdentityMismatch,
    SnapshotUnavailable,
    ExecutionFailed,
    Cancelled,
}

#[derive(Debug)]
pub struct DeclarationReadFailure {
    reason: DeclarationReadFailureReason,
    detail: String,
}

impl DeclarationReadFailure {
    pub(crate) fn new(reason: DeclarationReadFailureReason, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: detail.into(),
        }
    }

    pub fn reason(&self) -> DeclarationReadFailureReason {
        self.reason
    }
}

impl std::fmt::Display for DeclarationReadFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "declaration read {:?}: {}",
            self.reason, self.detail
        )
    }
}

impl std::error::Error for DeclarationReadFailure {}

pub fn declaration_read_failure_reason(
    error: &anyhow::Error,
) -> Option<DeclarationReadFailureReason> {
    error
        .downcast_ref::<DeclarationReadFailure>()
        .map(DeclarationReadFailure::reason)
        .or_else(|| {
            error
                .chain()
                .any(|cause| cause.to_string().to_ascii_lowercase().contains("cancelled"))
                .then_some(DeclarationReadFailureReason::Cancelled)
        })
}

fn read_error(reason: DeclarationReadFailureReason, detail: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(DeclarationReadFailure::new(reason, detail))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeclarationReadIdentity {
    Persistent {
        database_path: PathBuf,
        database_incarnation: Arc<str>,
        generation: SemanticGeneration,
    },
    Diagnostic {
        database_path: PathBuf,
        snapshot_id: u64,
        generation: SemanticGeneration,
    },
}

static NEXT_DIAGNOSTIC_SNAPSHOT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct CallReadHandle {
    db: IndexDbLease,
    pub generation: SemanticGeneration,
    identity: DeclarationReadIdentity,
    diagnostic: Option<Arc<Mutex<crate::store::DiagnosticReadSnapshot>>>,
}

impl CallReadHandle {
    pub(crate) fn identity(&self) -> &DeclarationReadIdentity {
        &self.identity
    }

    pub(crate) fn database_incarnation(&self) -> Option<&str> {
        match &self.identity {
            DeclarationReadIdentity::Persistent {
                database_incarnation,
                ..
            } => Some(database_incarnation),
            DeclarationReadIdentity::Diagnostic { .. } => None,
        }
    }

    pub(crate) fn database_path(&self) -> &Path {
        self.db.path()
    }

    #[cfg(test)]
    pub fn at_generation(db_path: PathBuf, generation: SemanticGeneration) -> Result<Self> {
        Self::from_lease(IndexDbLease::acquire(db_path), generation)
    }

    pub(crate) fn at_default_generation(
        db_path: PathBuf,
        generation: SemanticGeneration,
    ) -> Result<Self> {
        Self::from_lease(
            IndexDbLease::acquire_default_generation(db_path)?,
            generation,
        )
    }

    fn from_lease(db: IndexDbLease, generation: SemanticGeneration) -> Result<Self> {
        let store = IndexStore::open_readonly(db.path()).map_err(|error| {
            read_error(
                DeclarationReadFailureReason::SnapshotUnavailable,
                format!("failed to open {}: {error:#}", db.path().display()),
            )
        })?;
        let guard = store
            .begin_semantic_read(Some(generation.0))
            .map_err(|error| {
                read_error(
                    DeclarationReadFailureReason::IdentityMismatch,
                    format!(
                        "semantic generation is incompatible for {}: {error:#}",
                        db.path().display()
                    ),
                )
            })?;
        let database_incarnation = guard.store().entity_view().incarnation().map_err(|error| {
            read_error(
                DeclarationReadFailureReason::ExecutionFailed,
                format!(
                    "failed to read database identity for {}: {error:#}",
                    db.path().display()
                ),
            )
        })?;
        guard.finish().map_err(|error| {
            read_error(
                DeclarationReadFailureReason::ExecutionFailed,
                format!("failed to finish identity read: {error:#}"),
            )
        })?;
        Ok(Self {
            identity: DeclarationReadIdentity::Persistent {
                database_path: db.path().to_path_buf(),
                database_incarnation: Arc::from(database_incarnation),
                generation,
            },
            db,
            generation,
            diagnostic: None,
        })
    }

    pub fn capture(db_path: PathBuf) -> Result<Self> {
        let db = IndexDbLease::acquire(db_path);
        let store = IndexStore::open_readonly(db.path()).map_err(|error| {
            read_error(
                DeclarationReadFailureReason::SnapshotUnavailable,
                format!("failed to open {}: {error:#}", db.path().display()),
            )
        })?;
        let guard = store.begin_semantic_read(None).map_err(|error| {
            read_error(
                DeclarationReadFailureReason::SnapshotUnavailable,
                format!("failed to capture semantic snapshot: {error:#}"),
            )
        })?;
        let generation = SemanticGeneration(guard.generation());
        let database_incarnation = guard.store().entity_view().incarnation().map_err(|error| {
            read_error(
                DeclarationReadFailureReason::ExecutionFailed,
                format!("failed to read database identity: {error:#}"),
            )
        })?;
        guard.finish().map_err(|error| {
            read_error(
                DeclarationReadFailureReason::ExecutionFailed,
                format!("failed to finish snapshot capture: {error:#}"),
            )
        })?;
        Ok(Self {
            identity: DeclarationReadIdentity::Persistent {
                database_path: db.path().to_path_buf(),
                database_incarnation: Arc::from(database_incarnation),
                generation,
            },
            db,
            generation,
            diagnostic: None,
        })
    }

    pub(crate) fn capture_default_generation(db_path: PathBuf) -> Result<Self> {
        let db = IndexDbLease::acquire_default_generation(db_path)?;
        let store = IndexStore::open_readonly(db.path()).map_err(|error| {
            read_error(
                DeclarationReadFailureReason::SnapshotUnavailable,
                format!("failed to open {}: {error:#}", db.path().display()),
            )
        })?;
        let guard = store.begin_semantic_read(None).map_err(|error| {
            read_error(
                DeclarationReadFailureReason::SnapshotUnavailable,
                format!("failed to capture semantic snapshot: {error:#}"),
            )
        })?;
        let generation = SemanticGeneration(guard.generation());
        let database_incarnation = guard.store().entity_view().incarnation().map_err(|error| {
            read_error(
                DeclarationReadFailureReason::ExecutionFailed,
                format!("failed to read database identity: {error:#}"),
            )
        })?;
        guard.finish().map_err(|error| {
            read_error(
                DeclarationReadFailureReason::ExecutionFailed,
                format!("failed to finish snapshot capture: {error:#}"),
            )
        })?;
        Ok(Self {
            identity: DeclarationReadIdentity::Persistent {
                database_path: db.path().to_path_buf(),
                database_incarnation: Arc::from(database_incarnation),
                generation,
            },
            db,
            generation,
            diagnostic: None,
        })
    }

    /// CLI-only observation: one owned read transaction spans every candidate
    /// and presentation read, without acquiring a writable generation lease.
    pub(crate) fn capture_diagnostic(db_path: PathBuf) -> Result<Self> {
        let snapshot = crate::store::DiagnosticReadSnapshot::open(&db_path).map_err(|error| {
            read_error(
                DeclarationReadFailureReason::SnapshotUnavailable,
                format!("failed to capture diagnostic snapshot: {error:#}"),
            )
        })?;
        let generation = SemanticGeneration(snapshot.metadata.generation.unwrap_or(0));
        let snapshot_id = NEXT_DIAGNOSTIC_SNAPSHOT_ID.fetch_add(1, Ordering::Relaxed);
        Ok(Self {
            db: IndexDbLease::acquire(db_path.clone()),
            generation,
            identity: DeclarationReadIdentity::Diagnostic {
                database_path: db_path,
                snapshot_id,
                generation,
            },
            diagnostic: Some(Arc::new(Mutex::new(snapshot))),
        })
    }

    /// Run a typed read against the exact database identity and semantic
    /// generation captured by this handle.
    pub(crate) fn read<T>(&self, read: impl FnOnce(&IndexStore) -> Result<T>) -> Result<T> {
        REQUEST_READ_SESSIONS.with(|slot| {
            if let Some(counter) = slot.borrow().as_ref() {
                counter.fetch_add(1, Ordering::Relaxed);
            }
        });
        if let Some(snapshot) = &self.diagnostic {
            let snapshot = snapshot.lock().map_err(|_| {
                read_error(
                    DeclarationReadFailureReason::SnapshotUnavailable,
                    "diagnostic snapshot lock poisoned",
                )
            })?;
            return read(&snapshot.store).map_err(|error| {
                read_error(
                    DeclarationReadFailureReason::ExecutionFailed,
                    format!("diagnostic declaration read failed: {error:#}"),
                )
            });
        }

        let store = IndexStore::open_readonly(self.db.path()).map_err(|error| {
            read_error(
                DeclarationReadFailureReason::SnapshotUnavailable,
                format!(
                    "captured database is unavailable at {}: {error:#}",
                    self.db.path().display()
                ),
            )
        })?;
        let guard = store
            .begin_semantic_read(Some(self.generation.0))
            .map_err(|error| {
                read_error(
                    DeclarationReadFailureReason::IdentityMismatch,
                    format!("captured semantic generation is incompatible: {error:#}"),
                )
            })?;
        let observed_incarnation = guard.store().entity_view().incarnation().map_err(|error| {
            read_error(
                DeclarationReadFailureReason::ExecutionFailed,
                format!("failed to validate database identity: {error:#}"),
            )
        })?;
        let expected_incarnation = self.database_incarnation().ok_or_else(|| {
            read_error(
                DeclarationReadFailureReason::SnapshotUnavailable,
                "persistent read handle has no database identity",
            )
        })?;
        if observed_incarnation != expected_incarnation {
            return Err(read_error(
                DeclarationReadFailureReason::IdentityMismatch,
                format!(
                    "database identity mismatch at {}: expected {expected_incarnation}, observed {observed_incarnation}",
                    self.db.path().display()
                ),
            ));
        }
        let value = read(guard.store()).map_err(|error| {
            read_error(
                DeclarationReadFailureReason::ExecutionFailed,
                format!("declaration read execution failed: {error:#}"),
            )
        })?;
        guard.finish().map_err(|error| {
            read_error(
                DeclarationReadFailureReason::ExecutionFailed,
                format!("failed to finish declaration read: {error:#}"),
            )
        })?;
        Ok(value)
    }
}

thread_local! {
    static REQUEST_READ_SESSIONS: std::cell::RefCell<Option<Arc<std::sync::atomic::AtomicUsize>>> = const { std::cell::RefCell::new(None) };
}

/// Counts typed SQLite read sessions on one blocking request thread. A session
/// may execute several statements; this is deliberately not a SQL statement count.
pub(crate) struct ReadSessionProbe {
    previous: Option<Arc<std::sync::atomic::AtomicUsize>>,
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl ReadSessionProbe {
    pub(crate) fn enter(counter: Arc<std::sync::atomic::AtomicUsize>) -> Self {
        Self {
            previous: REQUEST_READ_SESSIONS.with(|slot| slot.replace(Some(counter))),
            _thread_bound: std::marker::PhantomData,
        }
    }
}

impl Drop for ReadSessionProbe {
    fn drop(&mut self) {
        REQUEST_READ_SESSIONS.with(|slot| {
            slot.replace(self.previous.take());
        });
    }
}
