use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// Narrowing state for one document's in-flight completion. `generation`
/// identifies the indexed workspace-state instances the `pools` index into;
/// rebuilding any derived state for those roots invalidates the memo.
#[derive(Clone)]
pub(super) struct CompletionMemo {
    pub(super) prefix: String,
    pub(super) generation: u64,
    pub(super) pools: Vec<Vec<usize>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WorkspaceGeneration(u64);

impl WorkspaceGeneration {
    pub(super) fn missing() -> Self {
        Self(0)
    }

    pub(super) fn as_u64(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct WorkspaceGenerationParts {
    pub(super) name_table: Option<usize>,
    pub(super) reach_graph: Option<usize>,
    pub(super) include_table: Option<usize>,
    pub(super) project_context: Option<usize>,
    pub(super) indexed_file_list: Option<usize>,
}

pub(super) fn workspace_generation_for_parts(
    root: &Path,
    parts: WorkspaceGenerationParts,
) -> WorkspaceGeneration {
    let mut hasher = DefaultHasher::new();
    root.hash(&mut hasher);
    parts.name_table.hash(&mut hasher);
    parts.reach_graph.hash(&mut hasher);
    parts.include_table.hash(&mut hasher);
    parts.project_context.hash(&mut hasher);
    parts.indexed_file_list.hash(&mut hasher);
    WorkspaceGeneration(hasher.finish())
}

pub(super) fn combine_workspace_generations(generations: &[(PathBuf, WorkspaceGeneration)]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for (root, generation) in generations {
        root.hash(&mut hasher);
        generation.as_u64().hash(&mut hasher);
    }
    hasher.finish()
}

pub(super) fn completion_memo_is_valid(
    prior_generation: u64,
    new_generation: u64,
    prior_prefix: &str,
    new_prefix: &str,
) -> bool {
    prior_generation == new_generation
        && !prior_prefix.is_empty()
        && new_prefix.starts_with(prior_prefix)
}
