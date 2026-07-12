//! Narrow adapter from parser products to the storage ingestion port.

use anyhow::Result;

use crate::parser::FileSemanticIndex;
use crate::semantic_model::PersistentFacts;
use crate::store::{
    FileFingerprint, FileIndexPayload, FileIndexUpdate, FileSource, IndexStore,
    PersistableFileIndex, PersistenceDiagnostics,
};

impl PersistableFileIndex for FileSemanticIndex {
    fn persistent_facts(&self) -> PersistentFacts<'_> {
        FileSemanticIndex::persistent_facts(self)
    }

    fn persistence_diagnostics(&self) -> PersistenceDiagnostics {
        PersistenceDiagnostics {
            fact_mask: self.diagnostics.requested_facts.bits(),
            parse_error_count: self.diagnostics.parse_error_count,
            fallback_used: self.diagnostics.fallback_used,
        }
    }
}

impl IndexStore {
    #[allow(dead_code)]
    pub fn upsert_file_index(
        &mut self,
        fingerprint: &FileFingerprint,
        index: &FileSemanticIndex,
    ) -> Result<()> {
        self.upsert_file_index_with_source(fingerprint, index, FileSource::Workspace)
    }

    pub fn upsert_file_index_with_source(
        &mut self,
        fingerprint: &FileFingerprint,
        index: &FileSemanticIndex,
        source: FileSource,
    ) -> Result<()> {
        self.apply_file_updates(&[FileIndexUpdate {
            fingerprint,
            source,
            payload: FileIndexPayload::Ok(index),
        }])
    }
}
