use std::fs;

use tempfile::tempdir;

use super::{index_dirty_files, index_workspace, DirtyFileChange, DirtyFileKind, IndexOptions};
use crate::store::IndexStore;

mod ambiguity;
mod basic;
mod include_edges;
mod slop_cases;
