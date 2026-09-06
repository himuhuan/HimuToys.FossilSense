use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

pub(super) const SOURCE_LIMIT: usize = 16 * 1024 * 1024;
pub(super) const POSITION_LIMIT: usize = 64;

#[derive(Debug)]
pub(crate) struct InputError(pub String);
impl fmt::Display for InputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for InputError {}

pub(super) fn invalid(message: &str) -> anyhow::Error {
    InputError(message.into()).into()
}

pub(super) fn paths(
    workspace: &Path,
    file: &Path,
    name: &str,
) -> Result<(PathBuf, PathBuf, String)> {
    if name.is_empty()
        || name.len() > 256
        || !name
            .bytes()
            .enumerate()
            .all(|(i, b)| b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit()))
    {
        return Err(invalid("name must be one identifier of at most 256 bytes"));
    }
    if file.as_os_str().is_empty()
        || file
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(invalid(
            "source file must be workspace-relative without parent components",
        ));
    }
    let root = workspace
        .canonicalize()
        .context("failed to resolve workspace")?;
    if !root.is_dir() {
        return Err(invalid("workspace must be a directory"));
    }
    let relative: PathBuf = file
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .collect();
    let absolute = root.join(&relative);
    let real = absolute
        .canonicalize()
        .context("failed to resolve source file")?;
    if !crate::pathing::path_is_within(&root, &real) {
        return Err(invalid("source resolves outside workspace"));
    }
    if !real.is_file() {
        return Err(invalid("source file must be a regular file"));
    }
    // Preserve the filesystem's spelling for SQLite path identity on Windows.
    // A distinct in-workspace symlink remains the requested scanner path.
    if crate::pathing::path_is_within(&real, &absolute)
        && crate::pathing::path_is_within(&absolute, &real)
    {
        let relative = crate::pathing::relative_slash_path(&root, &real)?;
        return Ok((root, real, relative));
    }
    Ok((
        root,
        absolute,
        crate::pathing::normalize_path_string(&relative),
    ))
}

pub(super) fn read_source(path: &Path) -> Result<(Vec<u8>, bool)> {
    let mut bytes = Vec::new();
    File::open(path)
        .context("failed to read source file")?
        .take(SOURCE_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)?;
    let truncated = bytes.len() > SOURCE_LIMIT;
    if truncated {
        bytes.clear();
    }
    Ok((bytes, truncated))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct NamePosition {
    pub line: u32,
    pub col: u32,
    pub start_byte: usize,
    pub end_byte: usize,
}

pub(super) fn positions(
    source: &str,
    name: &str,
    line: Option<u32>,
    col: Option<u32>,
    limit: usize,
) -> Result<(Vec<NamePosition>, bool)> {
    if line.is_some() != col.is_some() || line == Some(0) || col == Some(0) {
        return Err(invalid("line and col must be paired and 1-based"));
    }
    let requested_byte = match (line, col) {
        (Some(line), Some(col)) => {
            let mut start = 0;
            let text = source
                .split_inclusive('\n')
                .nth((line - 1) as usize)
                .ok_or_else(|| invalid("position is outside source"))?;
            for previous in source.split_inclusive('\n').take((line - 1) as usize) {
                start += previous.len();
            }
            let mut units = 0;
            let mut offset = None;
            for (byte, ch) in text.char_indices() {
                if units == col - 1 {
                    offset = Some(start + byte);
                    break;
                }
                units += ch.len_utf16() as u32;
                if units > col - 1 {
                    break;
                }
            }
            Some(offset.ok_or_else(|| invalid("column must address a UTF-16 character boundary"))?)
        }
        _ => None,
    };
    let bytes = source.as_bytes();
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut output = Vec::new();
    let mut truncated = false;
    let mut previous = 0;
    let mut line_number = 1;
    let mut line_start = 0;
    for (start, _) in source.match_indices(name) {
        let end = start + name.len();
        if (start > 0 && ident(bytes[start - 1])) || bytes.get(end).is_some_and(|b| ident(*b)) {
            continue;
        }
        if requested_byte.is_some_and(|at| at < start || at >= end) {
            continue;
        }
        if output.len() == limit {
            truncated = true;
            break;
        }
        for (offset, b) in bytes[previous..start].iter().enumerate() {
            if *b == b'\n' {
                line_number += 1;
                line_start = previous + offset + 1;
            }
        }
        previous = start;
        output.push(NamePosition {
            line: line_number,
            col: source[line_start..start].encode_utf16().count() as u32 + 1,
            start_byte: start,
            end_byte: end,
        });
        if requested_byte.is_some() {
            break;
        }
    }
    if requested_byte.is_some() && output.is_empty() {
        return Err(invalid("position does not address the requested name"));
    }
    Ok((output, truncated))
}
