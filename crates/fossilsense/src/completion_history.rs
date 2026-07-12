use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const MAX_HISTORY_ENTRIES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionHistoryMode {
    Auto,
    On,
    Off,
}

impl CompletionHistoryMode {
    pub fn is_enabled(self) -> bool {
        self != CompletionHistoryMode::Off
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CompletionAcceptEvent {
    pub workspace_hash: String,
    pub candidate_hash: String,
    pub kind: String,
    pub intent: String,
    pub prefix_bucket: String,
    pub accepted_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompletionHistorySnapshot {
    counts: HashMap<(u64, String, String, String), usize>,
    total_accepts: usize,
}

impl CompletionHistorySnapshot {
    #[allow(dead_code)]
    pub fn total_accepts(&self) -> usize {
        self.total_accepts
    }

    pub fn accept_count(
        &self,
        candidate_key: u64,
        kind: &str,
        intent: &str,
        prefix_bucket: &str,
    ) -> usize {
        self.counts
            .get(&(
                candidate_key,
                kind.to_string(),
                intent.to_string(),
                prefix_bucket.to_string(),
            ))
            .copied()
            .unwrap_or(0)
    }

    #[allow(dead_code)]
    pub(crate) fn append_from(&mut self, other: CompletionHistorySnapshot) {
        self.total_accepts += other.total_accepts;
        for (key, count) in other.counts {
            *self.counts.entry(key).or_default() += count;
        }
    }

    #[cfg(test)]
    pub(crate) fn from_test_accepts(
        accepts: Vec<(String, &'static str, &'static str, &'static str, usize)>,
    ) -> Self {
        let mut entries = Vec::new();
        for (candidate_hash, kind, intent, prefix_bucket, count) in accepts {
            for index in 0..count {
                entries.push(CompletionAcceptEvent {
                    workspace_hash: "test".to_string(),
                    candidate_hash: candidate_hash.clone(),
                    kind: kind.to_string(),
                    intent: intent.to_string(),
                    prefix_bucket: prefix_bucket.to_string(),
                    accepted_at: index as i64,
                });
            }
        }
        Self::from_entries(entries)
    }

    fn from_entries(entries: Vec<CompletionAcceptEvent>) -> Self {
        let mut snapshot = Self::default();
        for entry in entries {
            let Some(candidate_key) = candidate_hash_key_from_hex(&entry.candidate_hash) else {
                continue;
            };
            *snapshot
                .counts
                .entry((candidate_key, entry.kind, entry.intent, entry.prefix_bucket))
                .or_default() += 1;
            snapshot.total_accepts += 1;
        }
        snapshot
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct HistoryFile {
    version: u32,
    entries: Vec<CompletionAcceptEvent>,
}

impl Default for HistoryFile {
    fn default() -> Self {
        Self {
            version: 1,
            entries: Vec::new(),
        }
    }
}

pub struct CompletionHistoryStore {
    path: PathBuf,
    data: HistoryFile,
}

pub struct CompletionHistoryWrite {
    path: PathBuf,
    text: String,
}

impl CompletionHistoryWrite {
    pub fn persist(self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create history dir {}", parent.display()))?;
        }
        let tmp_path = self.path.with_extension("json.tmp");
        fs::write(&tmp_path, self.text)
            .with_context(|| format!("failed to write history temp {}", tmp_path.display()))?;
        if self.path.exists() {
            fs::remove_file(&self.path)
                .with_context(|| format!("failed to replace history {}", self.path.display()))?;
        }
        fs::rename(&tmp_path, &self.path).with_context(|| {
            format!(
                "failed to move history temp {} to {}",
                tmp_path.display(),
                self.path.display()
            )
        })?;
        Ok(())
    }
}

impl CompletionHistoryStore {
    pub fn open(path: &Path) -> Result<Self> {
        let data = if path.exists() {
            let text = fs::read_to_string(path)
                .with_context(|| format!("failed to read history {}", path.display()))?;
            if text.trim().is_empty() {
                HistoryFile::default()
            } else {
                serde_json::from_str(&text)
                    .with_context(|| format!("failed to parse history {}", path.display()))?
            }
        } else {
            HistoryFile::default()
        };

        Ok(Self {
            path: path.to_path_buf(),
            data,
        })
    }

    pub fn empty(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            data: HistoryFile::default(),
        }
    }

    #[cfg(test)]
    pub fn record_accept(&mut self, event: CompletionAcceptEvent) -> Result<()> {
        self.record_accept_deferred(event)?.persist()
    }

    pub fn record_accept_deferred(
        &mut self,
        event: CompletionAcceptEvent,
    ) -> Result<CompletionHistoryWrite> {
        self.data.entries.push(event);
        self.data
            .entries
            .sort_by(|left, right| right.accepted_at.cmp(&left.accepted_at));
        self.data.entries.truncate(MAX_HISTORY_ENTRIES);
        self.prepare_persist()
    }

    #[allow(dead_code)]
    pub fn snapshot(&self, workspace_hash: &str) -> CompletionHistorySnapshot {
        CompletionHistorySnapshot::from_entries(
            self.data
                .entries
                .iter()
                .filter(|entry| entry.workspace_hash == workspace_hash)
                .cloned()
                .collect(),
        )
    }

    #[allow(dead_code)]
    pub fn clear_workspace(&mut self, workspace_hash: &str) -> Result<usize> {
        let before = self.data.entries.len();
        self.data
            .entries
            .retain(|entry| entry.workspace_hash != workspace_hash);
        let removed = before - self.data.entries.len();
        self.prepare_persist()?.persist()?;
        Ok(removed)
    }

    #[cfg(test)]
    pub fn clear_all(&mut self) -> Result<usize> {
        let (removed, write) = self.clear_all_deferred()?;
        write.persist()?;
        Ok(removed)
    }

    pub fn clear_all_deferred(&mut self) -> Result<(usize, CompletionHistoryWrite)> {
        let removed = self.data.entries.len();
        self.data.entries.clear();
        Ok((removed, self.prepare_persist()?))
    }

    fn prepare_persist(&self) -> Result<CompletionHistoryWrite> {
        let text = serde_json::to_string_pretty(&self.data).context("failed to encode history")?;
        Ok(CompletionHistoryWrite {
            path: self.path.clone(),
            text,
        })
    }
}

#[allow(dead_code)]
pub fn candidate_hash(label: &str, kind: &str) -> String {
    candidate_hash_from_key(candidate_hash_key(label, kind))
}

pub fn candidate_hash_key(label: &str, kind: &str) -> u64 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kind.as_bytes());
    hasher.update(b"\0");
    hasher.update(label.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest.as_bytes()[..8]);
    u64::from_be_bytes(bytes)
}

pub fn candidate_hash_from_key(key: u64) -> String {
    format!("{key:016x}")
}

pub fn candidate_hash_key_from_hex(hash: &str) -> Option<u64> {
    if hash.len() != 16 || !hash.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(hash, 16).ok()
}

pub fn prefix_bucket(prefix: &str) -> String {
    let bucket: String = prefix
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .map(|ch| ch.to_ascii_lowercase())
        .take(2)
        .collect();
    if bucket.is_empty() {
        "none".to_string()
    } else {
        bucket
    }
}

pub fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn accept_event(
        workspace_hash: &str,
        label: &str,
        kind: &str,
        intent: &str,
        prefix_bucket: &str,
        accepted_at: i64,
    ) -> CompletionAcceptEvent {
        CompletionAcceptEvent {
            workspace_hash: workspace_hash.to_string(),
            candidate_hash: candidate_hash(label, kind),
            kind: kind.to_string(),
            intent: intent.to_string(),
            prefix_bucket: prefix_bucket.to_string(),
            accepted_at,
        }
    }

    #[test]
    fn history_store_records_accepts_without_raw_label() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("history.json");
        let mut store = CompletionHistoryStore::open(&path).expect("store");

        store
            .record_accept(accept_event(
                "workspace",
                "Widget::resize",
                "method",
                "call_target",
                "r",
                10,
            ))
            .expect("record");

        let text = std::fs::read_to_string(&path).expect("read history");
        assert!(!text.contains("Widget::resize"));
        assert!(text.contains("call_target"));
    }

    #[test]
    fn history_clear_removes_events_for_workspace() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("history.json");
        let mut store = CompletionHistoryStore::open(&path).expect("store");

        store
            .record_accept(accept_event(
                "one",
                "first",
                "function",
                "call_target",
                "fi",
                10,
            ))
            .expect("record one");
        store
            .record_accept(accept_event(
                "two",
                "second",
                "function",
                "call_target",
                "se",
                20,
            ))
            .expect("record two");

        store.clear_workspace("one").expect("clear");

        assert_eq!(store.snapshot("one").total_accepts(), 0);
        assert_eq!(store.snapshot("two").total_accepts(), 1);
    }

    #[test]
    fn history_record_accept_enforces_entry_cap() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("history.json");
        let mut store = CompletionHistoryStore::open(&path).expect("store");
        store.data.entries = (0..(MAX_HISTORY_ENTRIES + 5))
            .map(|index| {
                accept_event(
                    "workspace",
                    &format!("candidate_{index}"),
                    "function",
                    "call_target",
                    "ca",
                    index as i64,
                )
            })
            .collect();

        store
            .record_accept(accept_event(
                "workspace",
                "newest",
                "function",
                "call_target",
                "ne",
                999_999,
            ))
            .expect("record newest");

        assert_eq!(
            store.snapshot("workspace").total_accepts(),
            MAX_HISTORY_ENTRIES
        );
        let newest_hash = candidate_hash("newest", "function");
        assert_eq!(
            store
                .data
                .entries
                .first()
                .map(|entry| entry.candidate_hash.as_str()),
            Some(newest_hash.as_str())
        );
    }

    #[test]
    fn history_clear_all_resets_accept_counts() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("history.json");
        let mut store = CompletionHistoryStore::open(&path).expect("store");
        let candidate = candidate_hash_key("printf", "function");
        store
            .record_accept(accept_event(
                "workspace",
                "printf",
                "function",
                "call_target",
                "pr",
                10,
            ))
            .expect("record");
        assert_eq!(
            store
                .snapshot("workspace")
                .accept_count(candidate, "function", "call_target", "pr"),
            1
        );

        store.clear_all().expect("clear");

        assert_eq!(
            store
                .snapshot("workspace")
                .accept_count(candidate, "function", "call_target", "pr"),
            0
        );
    }
}
