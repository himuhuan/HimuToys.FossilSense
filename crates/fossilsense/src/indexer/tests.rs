use std::fs;

use tempfile::tempdir;

use super::{
    index_dirty_files, index_workspace, index_workspace_with_permit, DirtyFileChange,
    DirtyFileKind, IndexOptions,
};
use crate::store::IndexStore;

mod ambiguity;
mod basic;
mod go_packages;
mod include_edges;
mod slop_cases;
mod writer_lock;
