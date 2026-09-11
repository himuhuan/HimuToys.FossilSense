use std::sync::Arc;

use anyhow::Result;

use crate::declaration_index::SemanticDeclarationIndex;
use crate::declaration_read_handle::{
    CallReadHandle, DeclarationReadFailure, DeclarationReadFailureReason,
};
use crate::store::views::DeclarationReadRow;
use crate::store::IndexStore;

/// Immutable pairing of a database lease and the declaration index built for
/// that exact database identity and semantic generation. Feature services only
/// receive this context, so they cannot independently combine a payload cache
/// with a different read handle.
#[derive(Clone)]
pub struct DeclarationReadContext {
    handle: Arc<CallReadHandle>,
    declaration_index: Option<Arc<SemanticDeclarationIndex>>,
}

impl DeclarationReadContext {
    pub(crate) fn from_handle(handle: Arc<CallReadHandle>) -> Self {
        Self {
            handle,
            declaration_index: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn bind_index(
        handle: Arc<CallReadHandle>,
        declaration_index: SemanticDeclarationIndex,
    ) -> Result<Self> {
        let declaration_index = Arc::new(declaration_index.bind_identity(handle.identity())?);
        Ok(Self {
            handle,
            declaration_index: Some(declaration_index),
        })
    }

    pub(crate) fn from_bound_parts(
        handle: Arc<CallReadHandle>,
        declaration_index: Arc<SemanticDeclarationIndex>,
    ) -> Result<Self> {
        let Some(index_identity) = declaration_index.read_identity() else {
            return Err(anyhow::Error::new(DeclarationReadFailure::new(
                DeclarationReadFailureReason::IdentityMismatch,
                "declaration index is not bound to a database identity",
            )));
        };
        if index_identity != handle.identity() {
            return Err(anyhow::Error::new(DeclarationReadFailure::new(
                DeclarationReadFailureReason::IdentityMismatch,
                "declaration index and read handle identities differ",
            )));
        }
        Ok(Self {
            handle,
            declaration_index: Some(declaration_index),
        })
    }

    pub fn handle(&self) -> &CallReadHandle {
        &self.handle
    }

    pub fn declaration_index(&self) -> Option<&SemanticDeclarationIndex> {
        self.declaration_index.as_deref()
    }

    pub fn declaration_index_arc(&self) -> Option<Arc<SemanticDeclarationIndex>> {
        self.declaration_index.clone()
    }

    pub(crate) fn read<T>(&self, read: impl FnOnce(&IndexStore) -> Result<T>) -> Result<T> {
        self.handle.read(read)
    }

    pub fn payloads_by_ids(&self, ids: &[i64]) -> Result<Vec<Arc<DeclarationReadRow>>> {
        let Some(index) = &self.declaration_index else {
            return self
                .read(|store| store.declaration_view().by_ids(ids))
                .map(|rows| rows.into_iter().map(Arc::new).collect());
        };
        index.payloads_by_ids_bound(&self.handle, ids)
    }
}
