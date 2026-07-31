use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{Connection, ErrorCode};

const GENERATION_FAMILY_DATABASE: &str = ".fossilsense-generation-leases.sqlite";
const GENERATION_LEASE_PREFIX: &str = ".fossilsense-generation-";
const GENERATION_LEASE_SUFFIX: &str = ".sqlite";
const LEASE_GUARD_TABLE: &str = "generation_lease_guard";
const COORDINATION_TIMEOUT: Duration = Duration::from_secs(5);
const READER_RELEASE_CLEANUP_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug)]
struct GenerationFamilyLease {
    _connection: Connection,
}

impl GenerationFamilyLease {
    fn acquire(directory: &Path, timeout: Duration) -> Result<Self> {
        let connection = open_guard_database(&directory.join(GENERATION_FAMILY_DATABASE), timeout)?;
        connection
            .execute_batch("BEGIN EXCLUSIVE")
            .with_context(|| {
                format!(
                    "failed to coordinate generation leases in {}",
                    directory.display()
                )
            })?;
        Ok(Self {
            _connection: connection,
        })
    }

    fn try_acquire(directory: &Path) -> Result<Option<Self>> {
        match Self::acquire(directory, Duration::ZERO) {
            Ok(lease) => Ok(Some(lease)),
            Err(error) if anyhow_lock_conflict(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

#[derive(Debug)]
pub(crate) struct GenerationReadLease {
    _connection: Mutex<Connection>,
}

impl GenerationReadLease {
    pub(crate) fn acquire(database: &Path) -> Result<Self> {
        let directory = database.parent().with_context(|| {
            format!(
                "generation database has no parent directory: {}",
                database.display()
            )
        })?;
        let family = GenerationFamilyLease::acquire(directory, COORDINATION_TIMEOUT)?;
        let lease_path = generation_lease_path(database)?;
        let connection = open_guard_database(&lease_path, COORDINATION_TIMEOUT)?;
        connection.execute_batch("BEGIN")?;
        read_guard(&connection)?;
        let metadata = match fs::metadata(database) {
            Ok(metadata) => metadata,
            Err(error) => {
                drop(connection);
                try_remove_guard_database(&lease_path).with_context(|| {
                    format!(
                        "failed to reclaim the unused lease for missing generation {}",
                        database.display()
                    )
                })?;
                return Err(error).with_context(|| {
                    format!(
                        "generation database disappeared before its lease was acquired: {}",
                        database.display()
                    )
                });
            }
        };
        if !metadata.is_file() {
            drop(connection);
            try_remove_guard_database(&lease_path).with_context(|| {
                format!(
                    "failed to reclaim the unused lease for invalid generation {}",
                    database.display()
                )
            })?;
            anyhow::bail!(
                "generation database is not a regular file: {}",
                database.display()
            );
        }
        drop(family);
        Ok(Self {
            _connection: Mutex::new(connection),
        })
    }
}

#[derive(Debug)]
pub(crate) struct GenerationPublicationLease {
    _family: GenerationFamilyLease,
}

impl GenerationPublicationLease {
    pub(crate) fn acquire(directory: &Path) -> Result<Self> {
        Ok(Self {
            _family: GenerationFamilyLease::acquire(directory, COORDINATION_TIMEOUT)?,
        })
    }
}

#[derive(Debug)]
pub(crate) struct GenerationCleanupLease {
    _family: GenerationFamilyLease,
    directory: PathBuf,
}

impl GenerationCleanupLease {
    pub(crate) fn acquire_after_reader_release(directory: &Path) -> Result<Self> {
        Ok(Self {
            _family: GenerationFamilyLease::acquire(directory, READER_RELEASE_CLEANUP_TIMEOUT)?,
            directory: directory.to_path_buf(),
        })
    }

    pub(crate) fn try_acquire(directory: &Path) -> Result<Option<Self>> {
        Ok(
            GenerationFamilyLease::try_acquire(directory)?.map(|family| Self {
                _family: family,
                directory: directory.to_path_buf(),
            }),
        )
    }

    pub(crate) fn try_acquire_generation(
        &self,
        database: &Path,
    ) -> Result<Option<GenerationDeletionLease>> {
        anyhow::ensure!(
            database.parent() == Some(self.directory.as_path()),
            "generation cleanup target escaped its coordinated directory: {}",
            database.display()
        );
        let lease_path = generation_lease_path(database)?;
        let connection = match open_guard_database(&lease_path, Duration::ZERO) {
            Ok(connection) => connection,
            Err(error) if anyhow_lock_conflict(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        match connection.execute_batch("BEGIN EXCLUSIVE") {
            Ok(()) => Ok(Some(GenerationDeletionLease {
                connection: Some(connection),
                lease_path,
            })),
            Err(error) if sqlite_lock_conflict(&error) => Ok(None),
            Err(error) => Err(error).with_context(|| {
                format!(
                    "failed to acquire cleanup lease for generation {}",
                    database.display()
                )
            }),
        }
    }

    pub(crate) fn lease_database_for_entry(directory: &Path, entry_name: &str) -> Option<PathBuf> {
        let base_name = entry_name
            .strip_suffix("-wal")
            .or_else(|| entry_name.strip_suffix("-shm"))
            .or_else(|| entry_name.strip_suffix("-journal"))
            .unwrap_or(entry_name);
        let digest = base_name
            .strip_prefix(GENERATION_LEASE_PREFIX)?
            .strip_suffix(GENERATION_LEASE_SUFFIX)?;
        (digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then(|| directory.join(base_name))
    }

    pub(crate) fn try_remove_lease_database(&self, lease_path: &Path) -> Result<bool> {
        anyhow::ensure!(
            lease_path.parent() == Some(self.directory.as_path()),
            "generation lease cleanup target escaped its coordinated directory: {}",
            lease_path.display()
        );
        if !sqlite_file_family_exists(lease_path) {
            return Ok(false);
        }
        try_remove_guard_database(lease_path)
    }
}

#[derive(Debug)]
pub(crate) struct GenerationDeletionLease {
    connection: Option<Connection>,
    lease_path: PathBuf,
}

impl GenerationDeletionLease {
    pub(crate) fn remove_lease_database(mut self) {
        drop(self.connection.take());
        remove_sqlite_file_family(&self.lease_path);
    }
}

fn open_guard_database(path: &Path, timeout: Duration) -> Result<Connection> {
    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory).with_context(|| {
            format!(
                "failed to create generation lease directory {}",
                directory.display()
            )
        })?;
    }
    let connection = Connection::open(path).with_context(|| {
        format!(
            "failed to open generation lease database {}",
            path.display()
        )
    })?;
    connection.busy_timeout(timeout)?;
    let journal_mode: String =
        connection.query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))?;
    anyhow::ensure!(
        journal_mode.eq_ignore_ascii_case("delete"),
        "generation lease database kept unexpected journal mode {journal_mode}"
    );

    let initialized: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_schema
         WHERE type = 'table' AND name = ?1",
        [LEASE_GUARD_TABLE],
        |row| row.get(0),
    )?;
    if initialized == 0 {
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS generation_lease_guard (
                     singleton INTEGER PRIMARY KEY CHECK (singleton = 1)
                 ) WITHOUT ROWID;
                 INSERT OR IGNORE INTO generation_lease_guard (singleton) VALUES (1);",
            )
            .with_context(|| {
                format!(
                    "failed to initialize generation lease database {}",
                    path.display()
                )
            })?;
    }
    Ok(connection)
}

fn read_guard(connection: &Connection) -> Result<()> {
    let singleton: i64 = connection.query_row(
        "SELECT singleton FROM generation_lease_guard WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    anyhow::ensure!(
        singleton == 1,
        "generation lease database is missing its singleton guard"
    );
    Ok(())
}

fn generation_lease_path(database: &Path) -> Result<PathBuf> {
    let directory = database.parent().with_context(|| {
        format!(
            "generation database has no parent directory: {}",
            database.display()
        )
    })?;
    let name = database
        .file_name()
        .and_then(|name| name.to_str())
        .with_context(|| {
            format!(
                "generation database has no UTF-8 file name: {}",
                database.display()
            )
        })?;
    let digest = blake3::hash(name.as_bytes()).to_hex();
    Ok(directory.join(format!(
        "{GENERATION_LEASE_PREFIX}{digest}{GENERATION_LEASE_SUFFIX}"
    )))
}

fn remove_sqlite_file_family(path: &Path) {
    let _ = fs::remove_file(path);
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        let _ = fs::remove_file(PathBuf::from(sidecar));
    }
}

fn sqlite_file_family_exists(path: &Path) -> bool {
    path.exists()
        || ["-wal", "-shm", "-journal"].iter().any(|suffix| {
            let mut sidecar = path.as_os_str().to_os_string();
            sidecar.push(suffix);
            PathBuf::from(sidecar).exists()
        })
}

fn try_remove_guard_database(path: &Path) -> Result<bool> {
    let connection = match open_guard_database(path, Duration::ZERO) {
        Ok(connection) => connection,
        Err(error) if anyhow_lock_conflict(&error) => return Ok(false),
        Err(error) => return Err(error),
    };
    match connection.execute_batch("BEGIN EXCLUSIVE") {
        Ok(()) => {
            drop(connection);
            remove_sqlite_file_family(path);
            Ok(true)
        }
        Err(error) if sqlite_lock_conflict(&error) => Ok(false),
        Err(error) => Err(error).with_context(|| {
            format!(
                "failed to acquire orphan lease cleanup for {}",
                path.display()
            )
        }),
    }
}

fn anyhow_lock_conflict(error: &anyhow::Error) -> bool {
    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<rusqlite::Error>())
        .any(sqlite_lock_conflict)
}

fn sqlite_lock_conflict(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked,
                ..
            },
            _
        )
    )
}
