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
    entries: Vec<CompletionAcceptEvent>,
}

impl CompletionHistorySnapshot {
    #[allow(dead_code)]
    pub fn total_accepts(&self) -> usize {
        self.entries.len()
    }

    pub fn accept_count(
        &self,
        candidate_key: u64,
        kind: &str,
        intent: &str,
        prefix_bucket: &str,
    ) -> usize {
        self.entries
            .iter()
            .filter(|entry| {
                candidate_hash_key_from_hex(&entry.candidate_hash) == Some(candidate_key)
                    && entry.kind == kind
                    && entry.intent == intent
                    && entry.prefix_bucket == prefix_bucket
            })
            .count()
    }

    #[allow(dead_code)]
    pub(crate) fn append_from(&mut self, other: CompletionHistorySnapshot) {
        self.entries.extend(other.entries);
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
        Self { entries }
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

    pub fn record_accept(&mut self, event: CompletionAcceptEvent) -> Result<()> {
        self.data.entries.push(event);
        self.data
            .entries
            .sort_by(|left, right| right.accepted_at.cmp(&left.accepted_at));
        self.data.entries.truncate(MAX_HISTORY_ENTRIES);
        self.persist()
    }

    #[allow(dead_code)]
    pub fn snapshot(&self, workspace_hash: &str) -> CompletionHistorySnapshot {
        CompletionHistorySnapshot {
            entries: self
                .data
                .entries
                .iter()
                .filter(|entry| entry.workspace_hash == workspace_hash)
                .cloned()
                .collect(),
        }
    }

    #[allow(dead_code)]
    pub fn clear_workspace(&mut self, workspace_hash: &str) -> Result<usize> {
        let before = self.data.entries.len();
        self.data
            .entries
            .retain(|entry| entry.workspace_hash != workspace_hash);
        let removed = before - self.data.entries.len();
        self.persist()?;
        Ok(removed)
    }

    pub fn clear_all(&mut self) -> Result<usize> {
        let removed = self.data.entries.len();
        self.data.entries.clear();
        self.persist()?;
        Ok(removed)
    }

    fn persist(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create history dir {}", parent.display()))?;
        }

        let tmp_path = self.path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(&self.data).context("failed to encode history")?;
        fs::write(&tmp_path, text)
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

    #[test]
    fn history_store_records_accepts_without_raw_label() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("history.json");
        let mut store = CompletionHistoryStore::open(&path).expect("store");

        store
            .record_accept(CompletionAcceptEvent {
                workspace_hash: "workspace".to_string(),
                candidate_hash: candidate_hash("Widget::resize", "method"),
                kind: "method".to_string(),
                intent: "call_target".to_string(),
                prefix_bucket: "r".to_string(),
                accepted_at: 10,
            })
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
            .record_accept(CompletionAcceptEvent {
                workspace_hash: "one".to_string(),
                candidate_hash: candidate_hash("first", "function"),
                kind: "function".to_string(),
                intent: "call_target".to_string(),
                prefix_bucket: "fi".to_string(),
                accepted_at: 10,
            })
            .expect("record one");
        store
            .record_accept(CompletionAcceptEvent {
                workspace_hash: "two".to_string(),
                candidate_hash: candidate_hash("second", "function"),
                kind: "function".to_string(),
                intent: "call_target".to_string(),
                prefix_bucket: "se".to_string(),
                accepted_at: 20,
            })
            .expect("record two");

        store.clear_workspace("one").expect("clear");

        assert_eq!(store.snapshot("one").total_accepts(), 0);
        assert_eq!(store.snapshot("two").total_accepts(), 1);
    }
}
