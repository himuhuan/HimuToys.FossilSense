use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex};

use tower_lsp::lsp_types::CompletionItem;

use crate::pathing;

use super::presentation::push_include_candidate;

pub(in crate::server) type ExternalIncludeDirCache =
    Arc<StdMutex<HashMap<String, CachedDirListing>>>;

#[derive(Debug, Clone)]
pub(in crate::server) struct CachedDirListing {
    mtime_ns: u64,
    entries: Vec<(String, bool)>,
}

/// Whether a filename looks like an includable header: a known header extension,
/// or extensionless (C++ standard library headers such as `<vector>`).
pub(in crate::server) fn looks_like_header(name: &str) -> bool {
    match name.rsplit_once('.') {
        Some((_, ext)) => matches!(
            ext.to_ascii_lowercase().as_str(),
            "h" | "hpp" | "hh" | "hxx" | "inl" | "inc" | "ipp" | "tcc" | "def"
        ),
        None => true,
    }
}

pub(super) fn collect_disk_include_candidates(
    base: &Path,
    dir_native: &str,
    seg_lower: &str,
    seg: &str,
    base_score: i32,
    seen: &mut HashSet<String>,
    scored: &mut Vec<(i32, String, CompletionItem)>,
) {
    let dir = base.join(dir_native);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.to_ascii_lowercase().starts_with(seg_lower) {
            continue;
        }
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if !is_dir && !looks_like_header(&name) {
            continue;
        }
        push_include_candidate(name, is_dir, base_score, seg, seen, scored);
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn collect_cached_disk_include_candidates(
    base: &Path,
    dir_native: &str,
    seg_lower: &str,
    seg: &str,
    base_score: i32,
    cache: Option<&ExternalIncludeDirCache>,
    seen: &mut HashSet<String>,
    scored: &mut Vec<(i32, String, CompletionItem)>,
) {
    let Some(cache) = cache else {
        collect_disk_include_candidates(base, dir_native, seg_lower, seg, base_score, seen, scored);
        return;
    };

    let dir = base.join(dir_native);
    let Some(entries) = cached_dir_entries(&dir, cache) else {
        return;
    };
    for (name, is_dir) in entries {
        if !name.to_ascii_lowercase().starts_with(seg_lower) {
            continue;
        }
        if !is_dir && !looks_like_header(&name) {
            continue;
        }
        push_include_candidate(name, is_dir, base_score, seg, seen, scored);
    }
}

fn cached_dir_entries(dir: &Path, cache: &ExternalIncludeDirCache) -> Option<Vec<(String, bool)>> {
    let meta = std::fs::metadata(dir).ok()?;
    if !meta.is_dir() {
        return None;
    }
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let key = pathing::normalize_abs_path(dir);

    if let Ok(cache_guard) = cache.lock() {
        if let Some(cached) = cache_guard.get(&key) {
            if cached.mtime_ns == mtime_ns {
                return Some(cached.entries.clone());
            }
        }
    }

    let entries: Vec<(String, bool)> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            (name, is_dir)
        })
        .collect();

    if let Ok(mut cache_guard) = cache.lock() {
        cache_guard.insert(
            key,
            CachedDirListing {
                mtime_ns,
                entries: entries.clone(),
            },
        );
    }
    Some(entries)
}
