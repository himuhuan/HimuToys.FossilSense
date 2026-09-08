//! Immutable reference-file catalogue with a bounded, sorted path overlay.
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
type Entry = (String, PathBuf);
type Changes = BTreeMap<String, Option<Entry>>;
#[derive(Debug, Clone)]
pub(crate) struct IndexedFileList {
    base: Arc<Vec<Entry>>,
    changes: Arc<Changes>,
    count: usize,
    base_bytes: usize,
}
impl IndexedFileList {
    pub(crate) fn from_vec(mut entries: Vec<Entry>) -> Self {
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries.dedup_by(|a, b| a.0 == b.0);
        Self::from_shared(Arc::new(entries))
    }
    pub(crate) fn from_shared(base: Arc<Vec<Entry>>) -> Self {
        let base_bytes = base.capacity() * std::mem::size_of::<Entry>()
            + base
                .iter()
                .map(|(p, a)| p.capacity() + a.capacity())
                .sum::<usize>();
        Self {
            count: base.len(),
            base,
            changes: Arc::new(BTreeMap::new()),
            base_bytes,
        }
    }
    pub(crate) fn len(&self) -> usize {
        self.count
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.count == 0
    }
    fn base_entry(&self, path: &str) -> Option<&Entry> {
        self.base
            .binary_search_by(|(p, _)| p.as_str().cmp(path))
            .ok()
            .map(|i| &self.base[i])
    }
    pub(crate) fn contains_path(&self, path: &str) -> bool {
        self.changes
            .get(path)
            .map_or_else(|| self.base_entry(path).is_some(), Option::is_some)
    }
    pub(crate) fn update(
        self: &Arc<Self>,
        paths: &[String],
        fresh: Vec<Entry>,
    ) -> Option<Arc<Self>> {
        if paths.len() > 256 {
            return None;
        }
        let fresh: BTreeMap<_, _> = fresh
            .into_iter()
            .map(|entry| (entry.0.clone(), entry))
            .collect();
        if paths.iter().all(|path| {
            let old = self
                .changes
                .get(path)
                .map_or_else(|| self.base_entry(path), Option::as_ref);
            old == fresh.get(path)
        }) {
            return Some(self.clone());
        }
        let mut changes = self.changes.as_ref().clone();
        let mut count = self.count;
        let mut seen = std::collections::HashSet::new();
        for path in paths {
            if !seen.insert(path) {
                continue;
            }
            let new = fresh.get(path).cloned();
            let was_present = self.contains_path(path);
            if was_present && new.is_none() {
                count -= 1;
            }
            if !was_present && new.is_some() {
                count += 1;
            }
            if self.base_entry(path) == new.as_ref() {
                changes.remove(path);
            } else {
                changes.insert(path.clone(), new);
            }
        }
        if changes.len() > 256 || change_bytes(&changes) > 4 * 1024 * 1024 {
            return None;
        }
        Some(Arc::new(Self {
            base: self.base.clone(),
            changes: Arc::new(changes),
            count,
            base_bytes: self.base_bytes,
        }))
    }
    pub(crate) fn iter(&self) -> FileIter<'_> {
        FileIter {
            base: self.base.iter(),
            changes: self.changes.values(),
            shadow: &self.changes,
            pending_base: None,
            pending_delta: None,
            remaining: self.count,
        }
    }
    pub(crate) fn accounted_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.base_bytes + change_bytes(&self.changes)
    }
    pub(crate) fn delta_bytes(&self) -> usize {
        change_bytes(&self.changes)
    }
    pub(crate) fn delta_paths(&self) -> usize {
        self.changes.len()
    }
}
fn change_bytes(changes: &Changes) -> usize {
    // Conservative upper bound: charge a complete B-tree node per occupied key.
    changes.len()
        * (11 * std::mem::size_of::<(String, Option<Entry>)>() + 12 * std::mem::size_of::<usize>())
        + changes
            .iter()
            .map(|(key, value)| {
                key.capacity()
                    + value
                        .as_ref()
                        .map_or(0, |(p, a)| p.capacity() + a.capacity())
            })
            .sum::<usize>()
}
pub(crate) struct FileIter<'a> {
    base: std::slice::Iter<'a, Entry>,
    changes: std::collections::btree_map::Values<'a, String, Option<Entry>>,
    shadow: &'a Changes,
    pending_base: Option<&'a Entry>,
    pending_delta: Option<&'a Entry>,
    remaining: usize,
}
impl<'a> Iterator for FileIter<'a> {
    type Item = &'a Entry;
    fn next(&mut self) -> Option<Self::Item> {
        if self.pending_base.is_none() {
            self.pending_base = self.base.find(|(p, _)| !self.shadow.contains_key(p));
        }
        if self.pending_delta.is_none() {
            self.pending_delta = self.changes.find_map(Option::as_ref);
        }
        let out = match (self.pending_base, self.pending_delta) {
            (Some(a), Some(b)) if a.0 < b.0 => self.pending_base.take(),
            (_, Some(_)) => self.pending_delta.take(),
            (Some(_), None) => self.pending_base.take(),
            _ => None,
        };
        if out.is_some() {
            self.remaining -= 1;
        }
        out
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}
impl ExactSizeIterator for FileIter<'_> {}
impl<'a> IntoIterator for &'a IndexedFileList {
    type Item = &'a Entry;
    type IntoIter = FileIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn segmented_file_list_preserves_order_tombstones_and_old_snapshot() {
        let base = Arc::new(IndexedFileList::from_vec(
            (0..1000)
                .map(|n| {
                    let path = format!("d/{n:04}.c");
                    (path.clone(), PathBuf::from(path))
                })
                .collect(),
        ));
        let changed = vec!["d/0000.c".into(), "a/new.c".into()];
        let next = base
            .update(&changed, vec![("a/new.c".into(), PathBuf::from("a/new.c"))])
            .unwrap();
        assert!(Arc::ptr_eq(&base.base, &next.base));
        assert_eq!(next.delta_paths(), 2);
        assert_eq!(base.iter().next().unwrap().0, "d/0000.c");
        assert_eq!(next.iter().next().unwrap().0, "a/new.c");
        let paths: Vec<_> = next.iter().map(|e| &e.0).collect();
        assert_eq!(paths.len(), 1000);
        assert!(paths.windows(2).all(|p| p[0] < p[1]));
        assert!(!next.contains_path("d/0000.c"));
    }
    #[test]
    fn segmented_file_list_limits_cumulative_directory_and_reuses_noop() {
        let base = Arc::new(IndexedFileList::from_vec(Vec::new()));
        let mut current = base.clone();
        for n in 0..256 {
            let p = format!("d/{n}.c");
            current = current
                .update(
                    std::slice::from_ref(&p),
                    vec![(p.clone(), PathBuf::from(&p))],
                )
                .unwrap();
        }
        assert!(current
            .update(
                &["overflow.c".into()],
                vec![("overflow.c".into(), PathBuf::from("overflow.c"))]
            )
            .is_none());
        assert!(Arc::ptr_eq(
            &current,
            &current.update(&[], Vec::new()).unwrap()
        ));
        assert!(base.is_empty());
    }
    #[test]
    fn segmented_file_list_duplicate_events_count_each_path_once() {
        let base = Arc::new(IndexedFileList::from_vec(Vec::new()));
        let changed = base
            .update(
                &["one.c".into(), "one.c".into()],
                vec![("one.c".into(), PathBuf::from("one.c"))],
            )
            .unwrap();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed.iter().count(), 1);
    }
}
