use std::collections::{HashMap, HashSet};

use crate::project_context::ProjectContextKey;

use super::NameEntry;
use crate::query::{text::is_boundary, SHORT_PREFIX_MIN_LEN};

const SUBSTRING_KEY_LEN: usize = 3;
const SUBSEQUENCE_KEY_LEN: usize = 3;
const SUBSEQUENCE_SOURCE_CHAR_LIMIT: usize = 32;
const SUBSEQUENCE_KEYS_PER_ENTRY_LIMIT: usize = 256;

#[derive(Default)]
pub(in crate::query::name_table) struct NameRecallIndex {
    /// Boundary substring prefixes of length 1 and 2, e.g. `fs` in
    /// `noise_fs_symbol`. This preserves short-prefix boundary recall without
    /// scanning every name for plain substrings.
    pub(in crate::query::name_table) boundary_prefix: HashMap<String, Vec<usize>>,
    /// Three-character contiguous lowercase substrings for len >= 3 recall.
    pub(in crate::query::name_table) substring3: HashMap<String, Vec<usize>>,
    /// Bounded three-character subsequence keys for initials-style recall.
    pub(in crate::query::name_table) subsequence3: HashMap<String, Vec<usize>>,
    /// Entry indices by defining path; used to force current/reachable scoped
    /// candidates into completion recall even when broad global indexes are
    /// dense.
    pub(in crate::query::name_table) by_path: HashMap<String, Vec<usize>>,
    /// Entry indices by inferred project context for same-project recall.
    pub(in crate::query::name_table) by_project: HashMap<ProjectContextKey, Vec<usize>>,
}

impl NameRecallIndex {
    pub(in crate::query::name_table) fn build(entries: &[NameEntry]) -> Self {
        let mut recall = Self::default();
        for (index, entry) in entries.iter().enumerate() {
            index_boundary_prefixes(entry, index, &mut recall.boundary_prefix);
            index_substrings(entry, index, &mut recall.substring3);
            index_subsequences(entry, index, &mut recall.subsequence3);
            recall
                .by_path
                .entry(entry.path.clone())
                .or_default()
                .push(index);
            if let Some(project) = &entry.project_context {
                recall
                    .by_project
                    .entry(project.clone())
                    .or_default()
                    .push(index);
            }
        }
        recall
    }
}

pub(in crate::query::name_table) fn add_indices(
    indices: impl IntoIterator<Item = usize>,
    cap: Option<usize>,
    seen: &mut HashSet<usize>,
    out: &mut Vec<usize>,
) {
    let mut taken = 0usize;
    for index in indices {
        if seen.insert(index) {
            out.push(index);
            taken += 1;
            if cap.is_some_and(|cap| taken >= cap) {
                break;
            }
        }
    }
}

pub(in crate::query::name_table) fn leading_chars(value: &str, len: usize) -> Option<String> {
    let mut chars = value.chars();
    let mut key = String::new();
    for _ in 0..len {
        key.push(chars.next()?);
    }
    Some(key)
}

fn index_boundary_prefixes(entry: &NameEntry, index: usize, map: &mut HashMap<String, Vec<usize>>) {
    let mut seen = HashSet::new();
    for (byte_index, _) in entry.name.char_indices() {
        if !is_boundary(entry.name.as_bytes(), byte_index) {
            continue;
        }
        let Some(tail) = entry.lower.get(byte_index..) else {
            continue;
        };
        for len in 1..SHORT_PREFIX_MIN_LEN {
            let Some(key) = leading_chars(tail, len) else {
                break;
            };
            if seen.insert(key.clone()) {
                map.entry(key).or_default().push(index);
            }
        }
    }
}

fn index_substrings(entry: &NameEntry, index: usize, map: &mut HashMap<String, Vec<usize>>) {
    let chars: Vec<char> = entry.lower.chars().collect();
    if chars.len() < SUBSTRING_KEY_LEN {
        return;
    }
    let mut seen = HashSet::new();
    for start in 0..=chars.len() - SUBSTRING_KEY_LEN {
        let key: String = chars[start..start + SUBSTRING_KEY_LEN].iter().collect();
        if seen.insert(key.clone()) {
            map.entry(key).or_default().push(index);
        }
    }
}

fn index_subsequences(entry: &NameEntry, index: usize, map: &mut HashMap<String, Vec<usize>>) {
    let chars: Vec<char> = entry
        .lower
        .chars()
        .take(SUBSEQUENCE_SOURCE_CHAR_LIMIT)
        .collect();
    if chars.len() < SUBSEQUENCE_KEY_LEN {
        return;
    }
    let mut seen = HashSet::new();
    'outer: for first in 0..chars.len() - 2 {
        for second in first + 1..chars.len() - 1 {
            for third in second + 1..chars.len() {
                let mut key = String::new();
                key.push(chars[first]);
                key.push(chars[second]);
                key.push(chars[third]);
                if seen.insert(key.clone()) {
                    map.entry(key).or_default().push(index);
                    if seen.len() >= SUBSEQUENCE_KEYS_PER_ENTRY_LIMIT {
                        break 'outer;
                    }
                }
            }
        }
    }
}
