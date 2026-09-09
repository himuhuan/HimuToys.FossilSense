//! Generation-pinned path index and request-local include lookup.
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
mod path_probe;
use path_probe::{
    external_path_probe_cache, ExternalPathProbeLookup, ExternalPathProbeObservation,
    EXTERNAL_PATH_PROBE_OBSERVATION_LIMIT,
};

/// Published include-resolution paths for one semantic generation. Exact
/// membership uses the sorted path vector; the basename posting stores compact
/// positions instead of cloning every full path into a second workspace map.
#[derive(Debug)]
pub(crate) struct IncludePathIndex {
    paths: Arc<Vec<String>>,
    workspace_by_basename: Arc<HashMap<String, Vec<usize>>>,
    updates: Arc<std::collections::BTreeMap<String, Option<bool>>>,
    base_bytes: usize,
}

const REQUEST_SUFFIX_SCAN_LIMIT: usize = 4_096;
const REQUEST_SUFFIX_CANDIDATE_LIMIT: usize = 256;

impl IncludePathIndex {
    #[cfg(test)]
    pub(crate) fn shares_base_for_test(&self, other: &Self) -> bool {
        self.paths.as_ptr() == other.paths.as_ptr()
    }

    /// Build from `(normalized path, participates in workspace suffix recall)`.
    /// External paths remain exact-matchable but never enter the suffix tier.
    pub(crate) fn build(paths: impl IntoIterator<Item = (String, bool)>) -> Self {
        let mut paths = paths.into_iter().collect::<Vec<_>>();
        paths.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        let mut deduplicated: Vec<(String, bool)> = Vec::with_capacity(paths.len());
        for (path, workspace) in paths {
            if let Some(last) = deduplicated.last_mut().filter(|last| last.0 == path) {
                last.1 |= workspace;
            } else {
                deduplicated.push((path, workspace));
            }
        }
        let mut workspace_by_basename: HashMap<String, Vec<usize>> = HashMap::new();
        for (position, (path, workspace)) in deduplicated.iter().enumerate() {
            if *workspace {
                if let Some(name) = path.rsplit('/').next() {
                    workspace_by_basename
                        .entry(name.to_string())
                        .or_default()
                        .push(position);
                }
            }
        }
        let paths: Vec<String> = deduplicated.into_iter().map(|(path, _)| path).collect();
        let base_bytes = std::mem::size_of::<Self>()
            + paths.capacity() * std::mem::size_of::<String>()
            + paths.iter().map(String::capacity).sum::<usize>()
            + crate::memory_report::hash_table_bytes::<String, Vec<usize>>(
                workspace_by_basename.capacity(),
            )
            + workspace_by_basename
                .iter()
                .map(|(name, positions)| {
                    name.capacity() + positions.capacity() * std::mem::size_of::<usize>()
                })
                .sum::<usize>();
        Self {
            paths: Arc::new(paths),
            workspace_by_basename: Arc::new(workspace_by_basename),
            updates: Arc::new(std::collections::BTreeMap::new()),
            base_bytes,
        }
    }

    fn base_membership(&self, path: &str) -> Option<bool> {
        let position = self
            .paths
            .binary_search_by(|value| value.as_str().cmp(path))
            .ok()?;
        let workspace = path
            .rsplit('/')
            .next()
            .and_then(|name| self.workspace_by_basename.get(name))
            .is_some_and(|positions| positions.binary_search(&position).is_ok());
        Some(workspace)
    }
    fn membership(&self, path: &str) -> Option<bool> {
        self.updates
            .get(path)
            .copied()
            .unwrap_or_else(|| self.base_membership(path))
    }
    pub(super) fn contains(&self, candidate: &str) -> bool {
        self.membership(candidate).is_some()
    }

    pub(crate) fn updated_paths(
        self: &Arc<Self>,
        paths: &[String],
        rows: Vec<(String, bool)>,
    ) -> Option<Arc<Self>> {
        if paths.len() > 256 {
            return None;
        }
        let fresh: HashMap<_, _> = rows.into_iter().collect();
        if paths
            .iter()
            .all(|path| self.membership(path) == fresh.get(path).copied())
        {
            return Some(self.clone());
        }
        let mut updates = self.updates.as_ref().clone();
        for path in paths {
            let value = fresh.get(path).copied();
            if self.base_membership(path) == value {
                updates.remove(path);
            } else {
                updates.insert(path.clone(), value);
            }
        }
        if updates.len() > 256 || Self::update_bytes(&updates) > 4 * 1024 * 1024 {
            return None;
        }
        Some(Arc::new(Self {
            paths: self.paths.clone(),
            workspace_by_basename: self.workspace_by_basename.clone(),
            updates: Arc::new(updates),
            base_bytes: self.base_bytes,
        }))
    }
    fn update_bytes(updates: &std::collections::BTreeMap<String, Option<bool>>) -> usize {
        updates.len()
            * (11 * std::mem::size_of::<(String, Option<bool>)>()
                + 12 * std::mem::size_of::<usize>())
            + updates.keys().map(String::capacity).sum::<usize>()
    }
    pub(crate) fn accounted_bytes(&self) -> usize {
        self.base_bytes + self.delta_bytes()
    }
    pub(crate) fn delta_bytes(&self) -> usize {
        Self::update_bytes(&self.updates)
    }
    pub(crate) fn delta_paths(&self) -> usize {
        self.updates.len()
    }

    pub(super) fn suffix_candidates_bounded(
        &self,
        basename: &str,
        rel: &str,
        suffix: &str,
        scan_limit: usize,
        candidate_limit: usize,
    ) -> crate::includes::SuffixCandidateLookup {
        let mut lookup = crate::includes::SuffixCandidateLookup::default();
        // Inspect new paths first so removed base postings cannot starve them.
        for (candidate, state) in self.updates.iter() {
            if *state != Some(true) || candidate.rsplit('/').next() != Some(basename) {
                continue;
            }
            if lookup.inspected >= scan_limit {
                lookup.truncated = true;
                break;
            }
            lookup.inspected += 1;
            if candidate != rel && !candidate.ends_with(suffix) {
                continue;
            }
            if lookup.candidates.len() >= candidate_limit {
                lookup.truncated = true;
                break;
            }
            lookup.candidates.push(candidate.clone());
        }
        if let Some(posting) = self.workspace_by_basename.get(basename) {
            for position in posting {
                if lookup.inspected >= scan_limit {
                    lookup.truncated = true;
                    break;
                }
                lookup.inspected += 1;
                let candidate = &self.paths[*position];
                if self.updates.contains_key(candidate)
                    || (candidate != rel && !candidate.ends_with(suffix))
                {
                    continue;
                }
                if lookup.candidates.len() >= candidate_limit {
                    lookup.truncated = true;
                    break;
                }
                lookup.candidates.push(candidate.clone());
            }
        }
        lookup.candidates.sort();
        lookup.candidates.dedup();
        lookup
    }
}

/// Request-local path delta over the generation-pinned published index. Dirty
/// paths are the only owned strings; all untouched workspace/external paths
/// remain behind the shared `Arc`.
#[derive(Debug)]
pub(super) struct IncludePathView {
    base: Option<Arc<IncludePathIndex>>,
    delta_paths: HashSet<String>,
    delta_by_basename: HashMap<String, Vec<String>>,
    probe_pending: AtomicBool,
    probe_enqueue_attempts: AtomicUsize,
    probe_observations: Mutex<Vec<ExternalPathProbeObservation>>,
    probe_observations_overflowed: AtomicBool,
}

impl IncludePathView {
    #[cfg(test)]
    pub(super) fn shape_for_test(&self, expected_base: &Arc<IncludePathIndex>) -> (bool, usize) {
        (
            self.base
                .as_ref()
                .is_some_and(|base| Arc::ptr_eq(base, expected_base)),
            self.delta_paths.len(),
        )
    }

    pub(super) fn new(
        base: Option<Arc<IncludePathIndex>>,
        overlay_paths: impl IntoIterator<Item = String>,
    ) -> Self {
        let mut delta_paths = HashSet::new();
        for path in overlay_paths {
            if !base.as_deref().is_some_and(|base| base.contains(&path)) {
                delta_paths.insert(path);
            }
        }
        let mut delta_by_basename: HashMap<String, Vec<String>> = HashMap::new();
        for path in &delta_paths {
            if let Some(name) = path.rsplit('/').next() {
                delta_by_basename
                    .entry(name.to_string())
                    .or_default()
                    .push(path.clone());
            }
        }
        for paths in delta_by_basename.values_mut() {
            paths.sort_unstable();
            paths.dedup();
        }
        Self {
            base,
            delta_paths,
            delta_by_basename,
            probe_pending: AtomicBool::new(false),
            probe_enqueue_attempts: AtomicUsize::new(0),
            probe_observations: Mutex::new(Vec::new()),
            probe_observations_overflowed: AtomicBool::new(false),
        }
    }

    pub(super) fn contains(&self, candidate: &str) -> bool {
        if self.delta_paths.contains(candidate)
            || self
                .base
                .as_deref()
                .is_some_and(|base| base.contains(candidate))
        {
            return true;
        }
        match external_path_probe_cache()
            .lookup_or_schedule(candidate, &self.probe_enqueue_attempts)
        {
            ExternalPathProbeLookup::NotApplicable => false,
            ExternalPathProbeLookup::Pending => {
                self.probe_pending.store(true, Ordering::Release);
                false
            }
            ExternalPathProbeLookup::Ready {
                exists,
                valid_until,
                version,
            } => {
                let mut observations = self
                    .probe_observations
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(observation) = observations
                    .iter_mut()
                    .find(|observation| observation.path == candidate)
                {
                    observation.version = version;
                    observation.valid_until = valid_until;
                } else if observations.len() < EXTERNAL_PATH_PROBE_OBSERVATION_LIMIT {
                    observations.push(ExternalPathProbeObservation {
                        path: candidate.to_string(),
                        version,
                        valid_until,
                    });
                } else {
                    self.probe_observations_overflowed
                        .store(true, Ordering::Release);
                }
                exists
            }
        }
    }

    pub(super) fn cacheable(&self) -> bool {
        if self.probe_pending.load(Ordering::Acquire)
            || self.probe_observations_overflowed.load(Ordering::Acquire)
        {
            return false;
        }
        let observations = self
            .probe_observations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        observations.is_empty()
            || external_path_probe_cache().observations_are_current(&observations)
    }

    #[cfg(test)]
    pub(super) fn probe_enqueue_count_for_test(&self) -> usize {
        self.probe_enqueue_attempts.load(Ordering::Acquire)
    }

    pub(super) fn suffix_candidates(
        &self,
        basename: &str,
        rel: &str,
        suffix: &str,
    ) -> crate::includes::SuffixCandidateLookup {
        let mut lookup = crate::includes::SuffixCandidateLookup::default();
        for candidate in self.delta_by_basename.get(basename).into_iter().flatten() {
            if lookup.inspected >= REQUEST_SUFFIX_SCAN_LIMIT {
                lookup.truncated = true;
                break;
            }
            lookup.inspected += 1;
            if candidate.as_str() != rel && !candidate.ends_with(suffix) {
                continue;
            }
            if lookup.candidates.len() >= REQUEST_SUFFIX_CANDIDATE_LIMIT {
                lookup.truncated = true;
                break;
            }
            lookup.candidates.push(candidate.clone());
        }
        if let Some(base) = self.base.as_deref() {
            let base_lookup = base.suffix_candidates_bounded(
                basename,
                rel,
                suffix,
                REQUEST_SUFFIX_SCAN_LIMIT.saturating_sub(lookup.inspected),
                REQUEST_SUFFIX_CANDIDATE_LIMIT.saturating_sub(lookup.candidates.len()),
            );
            lookup.inspected = lookup.inspected.saturating_add(base_lookup.inspected);
            lookup.truncated |= base_lookup.truncated;
            lookup.candidates.extend(base_lookup.candidates);
        }
        lookup.candidates.sort_unstable();
        lookup.candidates.dedup();
        lookup
    }

    pub(super) fn resolve_include(
        &self,
        target_text: &str,
        source_dir: &str,
        include_roots: &[String],
    ) -> crate::includes::IncludeResolution {
        crate::includes::resolve_include_with_lookup(
            target_text,
            source_dir,
            include_roots,
            |candidate| self.contains(candidate),
            |basename, (rel, suffix)| self.suffix_candidates(basename, rel, suffix),
        )
    }
}
