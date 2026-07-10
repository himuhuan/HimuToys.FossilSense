use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

/// Narrowing state for one document's in-flight completion. `generation`
/// identifies the indexed workspace-state instances the `pools` index into;
/// rebuilding any derived state for those roots invalidates the memo.
#[derive(Clone)]
pub(super) struct CompletionMemo {
    pub(super) prefix: String,
    pub(super) generation: u64,
    pub(super) pools: Vec<Vec<usize>>,
}

/// Monotonic identity of one atomically published workspace engine snapshot.
/// Zero is reserved for the pre-index empty snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct EngineEpoch(u64);

impl EngineEpoch {
    pub(super) fn missing() -> Self {
        Self(0)
    }

    pub(super) fn published(value: u64) -> Self {
        debug_assert_ne!(value, 0, "published engine epochs reserve zero");
        Self(value)
    }

    pub(super) fn as_u64(self) -> u64 {
        self.0
    }
}

pub(super) fn combine_workspace_generations(generations: &[(PathBuf, EngineEpoch)]) -> u64 {
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
