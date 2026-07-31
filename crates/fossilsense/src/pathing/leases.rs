use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;

use crate::store::GenerationReadLease;

use super::{
    cleanup_index_directory_after_reader_release, generation_index_directory, stale_temp_cutoff,
};

#[derive(Debug)]
struct IndexDbLeaseToken {
    generation_directory: PathBuf,
    read_lease: Option<GenerationReadLease>,
}

impl Drop for IndexDbLeaseToken {
    fn drop(&mut self) {
        // Arc destroys this token exactly once after the final clone releases
        // it. Close the shared per-generation transaction before cleanup tries
        // to acquire that generation's exclusive deletion lease.
        drop(self.read_lease.take());
        let _ = cleanup_index_directory_after_reader_release(
            &self.generation_directory,
            stale_temp_cutoff(),
        );
    }
}

#[derive(Debug, Clone)]
pub struct IndexDbLease {
    path: PathBuf,
    _token: Option<Arc<IndexDbLeaseToken>>,
}

impl IndexDbLease {
    pub fn acquire(path: PathBuf) -> Self {
        Self { path, _token: None }
    }

    pub(crate) fn acquire_default_generation(path: PathBuf) -> Result<Self> {
        let generation_directory = generation_index_directory(&path).ok_or_else(|| {
            anyhow::anyhow!(
                "default index path is not a canonical generation database: {}",
                path.display()
            )
        })?;
        let read_lease = GenerationReadLease::acquire(&path)?;
        Ok(Self {
            path,
            _token: Some(Arc::new(IndexDbLeaseToken {
                generation_directory,
                read_lease: Some(read_lease),
            })),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}
