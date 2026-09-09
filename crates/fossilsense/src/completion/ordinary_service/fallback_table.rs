//! In-memory fallback name projection, matching and bounded publication deltas.
use crate::memory_report::{hash_table_bytes, vec_bytes};
use crate::query;
use crate::semantic_model::{CompletionKindHint, SemanticFamily};
use crate::store::views::FallbackCompletionRow;
use std::collections::HashSet;
use std::mem::size_of;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FallbackCompletionName {
    pub name: String,
    pub kind_hint: CompletionKindHint,
    pub detail: Option<String>,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct IndexedFallbackCompletionName {
    value: FallbackCompletionName,
    lower: String,
    semantic_family: SemanticFamily,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FallbackCompletionNameTable {
    entries: Arc<[IndexedFallbackCompletionName]>,
    match_index: Arc<std::collections::HashMap<u32, Vec<usize>>>,
    by_path: Arc<std::collections::HashMap<String, Vec<usize>>>,
    base_bytes: usize,
    shadowed_paths: Arc<HashSet<String>>,
    overlay_entries: Arc<[IndexedFallbackCompletionName]>,
}

impl FallbackCompletionNameTable {
    pub(crate) fn build(rows: Vec<FallbackCompletionRow>) -> Self {
        let entries: Vec<_> = rows
            .into_iter()
            .filter_map(|row| {
                let kind_hint = match row.kind_hint {
                    0 => CompletionKindHint::Function,
                    1 => CompletionKindHint::Macro,
                    2 => CompletionKindHint::Type,
                    3 => CompletionKindHint::Object,
                    _ => return None,
                };
                Some((
                    FallbackCompletionName {
                        name: row.name,
                        kind_hint,
                        detail: row.detail,
                        path: row.path,
                    },
                    row.semantic_family,
                ))
            })
            .collect();
        Self::from_family_entries(entries)
    }

    #[cfg(test)]
    pub(super) fn from_entries(mut entries: Vec<FallbackCompletionName>) -> Self {
        sort_and_dedup_fallback_entries(&mut entries);
        Self::from_family_entries(
            entries
                .into_iter()
                .map(|entry| (entry, SemanticFamily::CFamily))
                .collect(),
        )
    }

    fn from_family_entries(mut entries: Vec<(FallbackCompletionName, SemanticFamily)>) -> Self {
        entries.sort_by(|left, right| fallback_entry_order(&left.0, &right.0));
        entries.dedup();
        let entries: Vec<_> = entries
            .into_iter()
            .map(|(entry, family)| IndexedFallbackCompletionName::new(entry, family))
            .collect();
        let match_index = fallback_match_index(&entries);
        let mut by_path = std::collections::HashMap::<String, Vec<usize>>::new();
        for (index, entry) in entries.iter().enumerate() {
            by_path
                .entry(entry.value.path.clone())
                .or_default()
                .push(index);
        }
        let base_bytes = indexed_entries_bytes(&entries)
            + hash_table_bytes::<u32, Vec<usize>>(match_index.capacity())
            + match_index
                .values()
                .map(|v| vec_bytes::<usize>(v.capacity()))
                .sum::<usize>()
            + hash_table_bytes::<String, Vec<usize>>(by_path.capacity())
            + by_path
                .iter()
                .map(|(p, v)| p.capacity() + vec_bytes::<usize>(v.capacity()))
                .sum::<usize>();
        Self {
            base_bytes,
            entries: entries.into(),
            match_index: Arc::new(match_index),
            by_path: Arc::new(by_path),
            shadowed_paths: Arc::new(HashSet::new()),
            overlay_entries: Arc::from([]),
        }
    }

    #[cfg(test)]
    pub(super) fn matching(&self, prefix: &str, limit: usize) -> Vec<ScoredFallbackCompletionName> {
        self.matching_for_family(prefix, limit, SemanticFamily::CFamily)
    }

    pub(super) fn matching_for_family(
        &self,
        prefix: &str,
        limit: usize,
        semantic_family: SemanticFamily,
    ) -> Vec<ScoredFallbackCompletionName> {
        if limit == 0 {
            return Vec::new();
        }
        let needle = prefix.to_ascii_lowercase();
        let Some(key) = fallback_match_key(needle.as_bytes()) else {
            return Vec::new();
        };
        let mut scored: Vec<_> = self
            .match_index
            .get(&key)
            .into_iter()
            .flatten()
            .filter_map(|&index| {
                let entry = &self.entries[index];
                (entry.semantic_family == semantic_family
                    && !self.shadowed_paths.contains(&entry.value.path))
                .then(|| score_indexed_fallback(&needle, entry))
                .flatten()
            })
            .chain(
                self.overlay_entries
                    .iter()
                    .filter(|entry| entry.semantic_family == semantic_family)
                    .filter_map(|entry| score_indexed_fallback(&needle, entry)),
            )
            .collect();
        sort_scored_fallbacks(&mut scored);
        scored.truncate(limit);
        scored
    }

    #[cfg(test)]
    pub(crate) fn with_updated_paths(
        &self,
        shadowed_paths: &HashSet<String>,
        overlay_entries: impl IntoIterator<Item = FallbackCompletionName>,
    ) -> Self {
        self.with_updated_family_paths(
            shadowed_paths,
            overlay_entries
                .into_iter()
                .map(|entry| (entry, SemanticFamily::CFamily)),
        )
    }

    pub(crate) fn with_updated_family_paths(
        &self,
        shadowed_paths: &HashSet<String>,
        overlay_entries: impl IntoIterator<Item = (FallbackCompletionName, SemanticFamily)>,
    ) -> Self {
        let mut overlay_entries: Vec<_> = self
            .overlay_entries
            .iter()
            .filter(|entry| !shadowed_paths.contains(&entry.value.path))
            .map(|entry| (entry.value.clone(), entry.semantic_family))
            .chain(overlay_entries)
            .collect();
        let mut merged_shadow = self.shadowed_paths.as_ref().clone();
        merged_shadow.extend(shadowed_paths.iter().cloned());
        overlay_entries.sort_by(|left, right| fallback_entry_order(&left.0, &right.0));
        overlay_entries.dedup();
        Self {
            entries: self.entries.clone(),
            match_index: self.match_index.clone(),
            by_path: self.by_path.clone(),
            base_bytes: self.base_bytes,
            shadowed_paths: Arc::new(merged_shadow),
            overlay_entries: overlay_entries
                .into_iter()
                .map(|(entry, family)| IndexedFallbackCompletionName::new(entry, family))
                .collect::<Vec<_>>()
                .into(),
        }
    }

    /// Apply a bounded, cumulative publication delta. None requests a full rebuild.
    pub(crate) fn update_published_rows(
        self: &Arc<Self>,
        paths: &[String],
        rows: Vec<FallbackCompletionRow>,
    ) -> Option<Arc<Self>> {
        const MAX_PATHS: usize = 256;
        const MAX_ROWS: usize = 8192;
        let fresh = Self::build(rows);
        let changed: HashSet<_> = paths.iter().cloned().collect();
        let mut old = Vec::new();
        for path in &changed {
            if !self.shadowed_paths.contains(path) {
                if let Some(indices) = self.by_path.get(path) {
                    if old.len().saturating_add(indices.len()) > MAX_ROWS {
                        return None;
                    }
                    old.extend(indices.iter().map(|index| &self.entries[*index]));
                }
            }
        }
        old.extend(
            self.overlay_entries
                .iter()
                .filter(|entry| changed.contains(&entry.value.path)),
        );
        old.sort_by(|left, right| fallback_entry_order(&left.value, &right.value));
        if old.len() == fresh.entries.len()
            && old.iter().zip(fresh.entries.iter()).all(|(a, b)| **a == *b)
        {
            return Some(self.clone());
        }
        let mut shadowed = self.shadowed_paths.as_ref().clone();
        shadowed.extend(paths.iter().cloned());
        if shadowed.len() > MAX_PATHS {
            return None;
        }
        let mut overlay: Vec<_> = self
            .overlay_entries
            .iter()
            .filter(|entry| !changed.contains(&entry.value.path))
            .cloned()
            .collect();
        overlay.extend(fresh.entries.iter().cloned());
        if overlay.len() > MAX_ROWS {
            return None;
        }
        let bytes: usize = overlay
            .iter()
            .map(|entry| {
                std::mem::size_of::<IndexedFallbackCompletionName>()
                    + entry.value.name.capacity()
                    + entry.value.path.capacity()
                    + entry.lower.capacity()
                    + entry.value.detail.as_ref().map_or(0, String::capacity)
            })
            .sum();
        if bytes > 4 * 1024 * 1024 {
            return None;
        }
        Some(Arc::new(
            self.with_updated_family_paths(
                &shadowed,
                overlay
                    .into_iter()
                    .map(|entry| (entry.value, entry.semantic_family)),
            ),
        ))
    }

    /// Structure-level estimate of the bytes this table holds, for memory
    /// observability. Not an allocator promise; the process-level gates stay
    /// authoritative.
    pub(crate) fn accounted_bytes(&self) -> usize {
        size_of::<Self>() + self.base_bytes + self.delta_bytes()
    }
    pub(crate) fn delta_partitions(&self) -> usize {
        self.shadowed_paths.len()
    }
    pub(crate) fn delta_bytes(&self) -> usize {
        indexed_entries_bytes(&self.overlay_entries)
            + hash_table_bytes::<String, ()>(self.shadowed_paths.capacity())
            + self
                .shadowed_paths
                .iter()
                .map(String::capacity)
                .sum::<usize>()
    }
}
fn indexed_entries_bytes(entries: &[IndexedFallbackCompletionName]) -> usize {
    vec_bytes::<IndexedFallbackCompletionName>(entries.len())
        + entries
            .iter()
            .map(|entry| {
                entry.value.name.capacity()
                    + entry.value.path.capacity()
                    + entry.lower.capacity()
                    + entry.value.detail.as_ref().map_or(0, String::capacity)
            })
            .sum::<usize>()
}

impl IndexedFallbackCompletionName {
    fn new(value: FallbackCompletionName, semantic_family: SemanticFamily) -> Self {
        let lower = value.name.to_ascii_lowercase();
        Self {
            value,
            lower,
            semantic_family,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ScoredFallbackCompletionName {
    pub(super) score: i32,
    pub(super) value: FallbackCompletionName,
}

pub(super) fn fallback_matches<'a>(
    entries: impl Iterator<Item = &'a FallbackCompletionName>,
    prefix: &str,
    limit: usize,
) -> Vec<ScoredFallbackCompletionName> {
    let mut scored: Vec<_> = entries
        .filter_map(|entry| {
            query::completion_word_score(prefix, &entry.name, 0).map(|score| {
                ScoredFallbackCompletionName {
                    score,
                    value: entry.clone(),
                }
            })
        })
        .collect();
    sort_scored_fallbacks(&mut scored);
    scored.truncate(limit);
    scored
}

fn score_indexed_fallback(
    needle: &str,
    entry: &IndexedFallbackCompletionName,
) -> Option<ScoredFallbackCompletionName> {
    query::completion_word_score_lowered(needle, &entry.value.name, &entry.lower, 0).map(|score| {
        ScoredFallbackCompletionName {
            score,
            value: entry.value.clone(),
        }
    })
}

fn sort_scored_fallbacks(entries: &mut [ScoredFallbackCompletionName]) {
    entries.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.value.name.cmp(&right.value.name))
            .then_with(|| left.value.path.cmp(&right.value.path))
    });
}

#[cfg(test)]
fn sort_and_dedup_fallback_entries(entries: &mut Vec<FallbackCompletionName>) {
    entries.sort_by(fallback_entry_order);
    entries.dedup();
}

fn fallback_entry_order(
    left: &FallbackCompletionName,
    right: &FallbackCompletionName,
) -> std::cmp::Ordering {
    left.name
        .to_ascii_lowercase()
        .cmp(&right.name.to_ascii_lowercase())
        .then_with(|| left.name.cmp(&right.name))
        .then_with(|| left.path.cmp(&right.path))
        .then_with(|| {
            completion_hint_rank(left.kind_hint).cmp(&completion_hint_rank(right.kind_hint))
        })
        .then_with(|| left.detail.cmp(&right.detail))
}

fn completion_hint_rank(kind: CompletionKindHint) -> u8 {
    match kind {
        CompletionKindHint::Function => 0,
        CompletionKindHint::Macro => 1,
        CompletionKindHint::Type => 2,
        CompletionKindHint::Object => 3,
    }
}

/// Index the first 1-2 bytes of every identifier boundary and every 3-byte
/// substring. Fallback scoring only accepts boundary substrings for short
/// prefixes and contiguous substrings thereafter, so this is a complete cold-
/// request candidate set without rescoring the entire table.
fn fallback_match_index(
    entries: &[IndexedFallbackCompletionName],
) -> std::collections::HashMap<u32, Vec<usize>> {
    let mut index: std::collections::HashMap<u32, Vec<usize>> = std::collections::HashMap::new();
    for (entry_index, entry) in entries.iter().enumerate() {
        let original = entry.value.name.as_bytes();
        let lower = entry.lower.as_bytes();
        let mut keys = HashSet::new();
        for start in 0..lower.len() {
            if fallback_name_boundary(original, start) {
                for width in 1..=2 {
                    if let Some(key) =
                        fallback_match_key(lower.get(start..start + width).unwrap_or_default())
                    {
                        keys.insert(key);
                    }
                }
            }
        }
        for bytes in lower.windows(3) {
            if let Some(key) = fallback_match_key(bytes) {
                keys.insert(key);
            }
        }
        for key in keys {
            index.entry(key).or_default().push(entry_index);
        }
    }
    index
}

fn fallback_match_key(bytes: &[u8]) -> Option<u32> {
    let width = bytes.len().min(3);
    if width == 0 || !bytes[..width].iter().all(u8::is_ascii) {
        return None;
    }
    let mut key = (width as u32) << 24;
    for (offset, byte) in bytes[..width].iter().enumerate() {
        key |= (*byte as u32) << (offset * 8);
    }
    Some(key)
}

fn fallback_name_boundary(bytes: &[u8], index: usize) -> bool {
    if index == 0 {
        return true;
    }
    let previous = bytes[index - 1];
    let current = bytes[index];
    (previous == b'_' && current != b'_')
        || (previous.is_ascii_lowercase() && current.is_ascii_uppercase())
        || (previous.is_ascii_alphabetic() && current.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fallback_table_accounted_bytes_grows_with_entries() {
        let empty = FallbackCompletionNameTable::default();
        let baseline = empty.accounted_bytes();

        let table = FallbackCompletionNameTable::from_entries(vec![
            FallbackCompletionName {
                name: "alpha_render".into(),
                kind_hint: crate::semantic_model::CompletionKindHint::Function,
                detail: Some("void alpha_render(void)".into()),
                path: "src/alpha.c".into(),
            },
            FallbackCompletionName {
                name: "ALPHA_FLAG".into(),
                kind_hint: crate::semantic_model::CompletionKindHint::Macro,
                detail: None,
                path: "include/alpha.h".into(),
            },
        ]);

        assert!(table.accounted_bytes() > baseline);
    }
    #[test]
    fn fallback_completion_table_filters_semantic_family_before_limit() {
        let table = FallbackCompletionNameTable::from_family_entries(vec![
            (
                FallbackCompletionName {
                    name: "SharedC".to_string(),
                    kind_hint: crate::semantic_model::CompletionKindHint::Function,
                    detail: None,
                    path: "broken.c".to_string(),
                },
                crate::config::SemanticFamily::CFamily,
            ),
            (
                FallbackCompletionName {
                    name: "SharedGo".to_string(),
                    kind_hint: crate::semantic_model::CompletionKindHint::Function,
                    detail: None,
                    path: "broken.go".to_string(),
                },
                crate::config::SemanticFamily::Go,
            ),
        ]);

        assert_eq!(
            table
                .matching_for_family("Shared", 1, crate::config::SemanticFamily::Go)
                .into_iter()
                .map(|entry| entry.value.name)
                .collect::<Vec<_>>(),
            vec!["SharedGo"]
        );
    }
    #[test]
    fn dirty_fallback_overlay_tombstones_stale_durable_hints() {
        let base = FallbackCompletionNameTable::from_entries(vec![
            FallbackCompletionName {
                name: "stale_guess".to_string(),
                kind_hint: crate::semantic_model::CompletionKindHint::Function,
                detail: None,
                path: "dirty.h".to_string(),
            },
            FallbackCompletionName {
                name: "clean_guess".to_string(),
                kind_hint: crate::semantic_model::CompletionKindHint::Object,
                detail: None,
                path: "clean.h".to_string(),
            },
        ]);
        let updated = base.with_updated_paths(
            &HashSet::from(["dirty.h".to_string()]),
            [FallbackCompletionName {
                name: "current_guess".to_string(),
                kind_hint: crate::semantic_model::CompletionKindHint::Type,
                detail: None,
                path: "dirty.h".to_string(),
            }],
        );
        assert!(
            Arc::ptr_eq(&base.entries, &updated.entries)
                && Arc::ptr_eq(&base.match_index, &updated.match_index),
            "dirty overlays must share the immutable fallback index"
        );
        let names: HashSet<_> = updated
            .matching("guess", 10)
            .into_iter()
            .map(|entry| entry.value.name)
            .collect();
        assert_eq!(
            names,
            HashSet::from(["clean_guess".to_string(), "current_guess".to_string()])
        );
    }
    #[test]
    fn fallback_match_index_is_equivalent_to_full_table_scoring() {
        let table = FallbackCompletionNameTable::from_entries(
            [
                "alpha",
                "AlphaBeta",
                "HTTPServer2",
                "alpha_beta",
                "xAlpha",
                "_privateValue",
                "banana",
            ]
            .into_iter()
            .map(|name| FallbackCompletionName {
                name: name.to_string(),
                kind_hint: crate::semantic_model::CompletionKindHint::Object,
                detail: None,
                path: format!("{name}.h"),
            })
            .collect(),
        );

        for prefix in [
            "a", "al", "h", "ht", "tps", "s", "se", "2", "b", "be", "bet", "pha", "na", "v", "val",
            "zz",
        ] {
            for limit in [1, 3, 100] {
                let indexed = table.matching(prefix, limit);
                let full = fallback_matches(
                    table.entries.iter().map(|entry| &entry.value),
                    prefix,
                    limit,
                );
                assert_eq!(indexed, full, "prefix={prefix:?}, limit={limit}");
            }
        }
    }
}
