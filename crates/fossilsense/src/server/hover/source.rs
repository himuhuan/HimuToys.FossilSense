use std::path::{Path, PathBuf};

use crate::query;

use super::HOVER_SOURCE_FILE_BYTE_LIMIT;

pub(in crate::server) fn candidate_source_text_for_path(
    root: &Path,
    current_rel: &str,
    current_text: &str,
    candidate_path: &str,
    source: &str,
) -> Option<String> {
    if candidate_path == current_rel {
        if current_text.len() as u64 > HOVER_SOURCE_FILE_BYTE_LIMIT {
            return None;
        }
        return Some(current_text.to_string());
    }
    let path = candidate_source_path(root, candidate_path, source);
    let metadata = std::fs::metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() > HOVER_SOURCE_FILE_BYTE_LIMIT {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

#[allow(clippy::too_many_arguments)]
pub(in crate::server) fn candidate_source_text_for_path_with_overlay_at_revision(
    root: &Path,
    current_rel: &str,
    current_text: &str,
    overlay: &crate::candidate_service::CandidateOverlaySnapshot,
    candidate_path: &str,
    source: &str,
    revision: Option<&query::CandidateRevision>,
) -> Option<String> {
    if let Some(text) = overlay.source_text(candidate_path) {
        if text.len() as u64 > HOVER_SOURCE_FILE_BYTE_LIMIT {
            return None;
        }
        return Some(text.to_string());
    }
    candidate_source_text_for_path_at_revision(
        root,
        current_rel,
        current_text,
        candidate_path,
        source,
        revision,
    )
}

pub(in crate::server) fn candidate_source_text_for_path_at_revision(
    root: &Path,
    current_rel: &str,
    current_text: &str,
    candidate_path: &str,
    source: &str,
    revision: Option<&query::CandidateRevision>,
) -> Option<String> {
    if candidate_path == current_rel {
        if current_text.len() as u64 > HOVER_SOURCE_FILE_BYTE_LIMIT {
            return None;
        }
        return Some(current_text.to_string());
    }
    let revision = revision?;
    let path = candidate_source_path(root, candidate_path, source);
    let metadata_before = std::fs::metadata(&path).ok()?;
    if !metadata_before.is_file()
        || metadata_before.len() != revision.size
        || metadata_before.len() > HOVER_SOURCE_FILE_BYTE_LIMIT
    {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    let metadata_after = std::fs::metadata(&path).ok()?;
    if metadata_before.len() != metadata_after.len()
        || metadata_before.modified().ok() != metadata_after.modified().ok()
        || bytes.len() as u64 != revision.size
        || blake3::hash(&bytes).to_hex().as_str() != revision.hash.as_str()
    {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn candidate_source_path(root: &Path, path: &str, source: &str) -> PathBuf {
    if source == "external" {
        return PathBuf::from(path);
    }
    let mut out = root.to_path_buf();
    for segment in path.split('/') {
        if !segment.is_empty() {
            out.push(segment);
        }
    }
    out
}
