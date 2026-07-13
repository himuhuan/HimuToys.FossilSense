//! Bounded, revision-aware source range hydration for Hover presentation.
//!
//! Identity and candidate selection happen before this module is called.  The
//! reader only materializes a proven byte range and reports why it declined to
//! do so; it never silently returns a partial definition that could look
//! complete to the user.

use std::fs::{File, Metadata};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::UNIX_EPOCH;

pub const SOURCE_EXCERPT_MAX_BYTES: usize = 128 * 1024;
pub const SOURCE_EXCERPT_MAX_LINES: usize = 2_048;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceRevision {
    pub size: u64,
    pub mtime_ns: i64,
    /// BLAKE3 identity of exactly the byte range requested from `read_file`.
    pub excerpt_hash: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceFileRevision {
    pub size: u64,
    pub mtime_ns: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceByteRange {
    pub start: usize,
    pub end: usize,
}

impl SourceByteRange {
    fn len(self) -> Option<usize> {
        self.end.checked_sub(self.start)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceExcerptOmission {
    InvalidRange,
    StaleRevision,
    ByteLimit,
    LineLimit,
    Unreadable,
    InvalidUtf8,
}

impl SourceExcerptOmission {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRange => "invalid source range",
            Self::StaleRevision => "source range is stale",
            Self::ByteLimit => "definition exceeds the byte budget",
            Self::LineLimit => "definition exceeds the line budget",
            Self::Unreadable => "source range is unreadable",
            Self::InvalidUtf8 => "source range is not valid UTF-8",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceExcerpt {
    Complete {
        text: String,
        bytes_read: usize,
        line_count: usize,
    },
    Omitted(SourceExcerptOmission),
}

impl SourceExcerpt {
    #[cfg(test)]
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Complete { text, .. } => Some(text),
            Self::Omitted(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceMetadata {
    pub is_file: bool,
    pub revision: SourceFileRevision,
    file_identity: Option<SourceFileIdentity>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceFileIdentity {
    volume: u64,
    file: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRangeRead {
    pub bytes: Vec<u8>,
    pub metadata_before: SourceMetadata,
    pub metadata_after: SourceMetadata,
}

pub trait SourceRangeProvider: Send + Sync {
    fn metadata(&self, path: &Path) -> io::Result<SourceMetadata>;
    fn read_range(&self, path: &Path, start: u64, len: usize) -> io::Result<SourceRangeRead>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct FileSourceRangeProvider;

impl SourceRangeProvider for FileSourceRangeProvider {
    fn metadata(&self, path: &Path) -> io::Result<SourceMetadata> {
        file_source_metadata(&File::open(path)?)
    }

    fn read_range(&self, path: &Path, start: u64, len: usize) -> io::Result<SourceRangeRead> {
        let mut file = File::open(path)?;
        let metadata_before = file_source_metadata(&file)?;
        if !metadata_before.is_file {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "source excerpt path is not a file",
            ));
        }
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::with_capacity(len);
        (&mut file).take(len as u64).read_to_end(&mut bytes)?;
        let metadata_after = file_source_metadata(&file)?;
        Ok(SourceRangeRead {
            bytes,
            metadata_before,
            metadata_after,
        })
    }
}

pub struct SourceExcerptReader<P = FileSourceRangeProvider> {
    provider: P,
    max_bytes: usize,
    max_lines: usize,
}

impl Default for SourceExcerptReader<FileSourceRangeProvider> {
    fn default() -> Self {
        Self::new(FileSourceRangeProvider)
    }
}

impl<P: SourceRangeProvider> SourceExcerptReader<P> {
    pub fn new(provider: P) -> Self {
        Self {
            provider,
            max_bytes: SOURCE_EXCERPT_MAX_BYTES,
            max_lines: SOURCE_EXCERPT_MAX_LINES,
        }
    }

    #[cfg(test)]
    fn with_limits(provider: P, max_bytes: usize, max_lines: usize) -> Self {
        Self {
            provider,
            max_bytes,
            max_lines,
        }
    }

    pub fn read_file(
        &self,
        path: &Path,
        range: SourceByteRange,
        expected: SourceRevision,
    ) -> SourceExcerpt {
        let Some(len) = range.len() else {
            return SourceExcerpt::Omitted(SourceExcerptOmission::InvalidRange);
        };
        if range.end as u64 > expected.size {
            return SourceExcerpt::Omitted(SourceExcerptOmission::StaleRevision);
        }
        if len > self.max_bytes {
            return SourceExcerpt::Omitted(SourceExcerptOmission::ByteLimit);
        }
        let Ok(metadata_before) = self.provider.metadata(path) else {
            return SourceExcerpt::Omitted(SourceExcerptOmission::Unreadable);
        };
        if !metadata_before.is_file {
            return SourceExcerpt::Omitted(SourceExcerptOmission::Unreadable);
        }
        if !metadata_matches_revision(metadata_before, expected) {
            return SourceExcerpt::Omitted(SourceExcerptOmission::StaleRevision);
        }
        let Ok(read) = self.provider.read_range(path, range.start as u64, len) else {
            return SourceExcerpt::Omitted(SourceExcerptOmission::Unreadable);
        };
        let Ok(metadata_after) = self.provider.metadata(path) else {
            return SourceExcerpt::Omitted(SourceExcerptOmission::StaleRevision);
        };
        if !metadata_matches_revision(read.metadata_before, expected)
            || !metadata_matches_revision(read.metadata_after, expected)
            || !metadata_matches_revision(metadata_after, expected)
            || !same_file(metadata_before, read.metadata_before)
            || !same_file(read.metadata_before, read.metadata_after)
            || !same_file(read.metadata_after, metadata_after)
        {
            return SourceExcerpt::Omitted(SourceExcerptOmission::StaleRevision);
        }
        if read.bytes.len() != len {
            return SourceExcerpt::Omitted(SourceExcerptOmission::StaleRevision);
        }
        if *blake3::hash(&read.bytes).as_bytes() != expected.excerpt_hash {
            return SourceExcerpt::Omitted(SourceExcerptOmission::StaleRevision);
        }
        self.finish(read.bytes)
    }

    pub fn read_buffer(&self, source: &str, range: SourceByteRange) -> SourceExcerpt {
        let Some(len) = range.len() else {
            return SourceExcerpt::Omitted(SourceExcerptOmission::InvalidRange);
        };
        if len > self.max_bytes {
            return SourceExcerpt::Omitted(SourceExcerptOmission::ByteLimit);
        }
        let Some(bytes) = source.as_bytes().get(range.start..range.end) else {
            return SourceExcerpt::Omitted(SourceExcerptOmission::StaleRevision);
        };
        if !source.is_char_boundary(range.start) || !source.is_char_boundary(range.end) {
            return SourceExcerpt::Omitted(SourceExcerptOmission::InvalidRange);
        }
        self.finish(bytes.to_vec())
    }

    fn finish(&self, bytes: Vec<u8>) -> SourceExcerpt {
        let line_count = source_line_count(&bytes);
        if line_count > self.max_lines {
            return SourceExcerpt::Omitted(SourceExcerptOmission::LineLimit);
        }
        let bytes_read = bytes.len();
        match String::from_utf8(bytes) {
            Ok(text) => SourceExcerpt::Complete {
                text,
                bytes_read,
                line_count,
            },
            Err(_) => SourceExcerpt::Omitted(SourceExcerptOmission::InvalidUtf8),
        }
    }
}

fn source_line_count(bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        0
    } else {
        bytes.iter().filter(|byte| **byte == b'\n').count() + 1
    }
}

fn metadata_matches_revision(metadata: SourceMetadata, expected: SourceRevision) -> bool {
    metadata.is_file
        && metadata.revision.size == expected.size
        && metadata.revision.mtime_ns == expected.mtime_ns
}

fn same_file(left: SourceMetadata, right: SourceMetadata) -> bool {
    match (left.file_identity, right.file_identity) {
        (Some(left), Some(right)) => left == right,
        (None, None) => true,
        _ => false,
    }
}

fn file_source_metadata(file: &File) -> io::Result<SourceMetadata> {
    let metadata = file.metadata()?;
    Ok(SourceMetadata {
        is_file: metadata.is_file(),
        revision: SourceFileRevision {
            size: metadata.len(),
            mtime_ns: metadata_mtime_ns(&metadata),
        },
        file_identity: metadata_file_identity(file, &metadata)?,
    })
}

#[cfg(unix)]
fn metadata_file_identity(
    _file: &File,
    metadata: &Metadata,
) -> io::Result<Option<SourceFileIdentity>> {
    use std::os::unix::fs::MetadataExt;

    Ok(Some(SourceFileIdentity {
        volume: metadata.dev(),
        file: metadata.ino(),
    }))
}

#[cfg(windows)]
fn metadata_file_identity(
    file: &File,
    _metadata: &Metadata,
) -> io::Result<Option<SourceFileIdentity>> {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION},
    };

    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `file` owns a valid open handle and `information` is a live,
    // writable output structure for the duration of the system call.
    let succeeded =
        unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut information) };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Some(SourceFileIdentity {
        volume: u64::from(information.dwVolumeSerialNumber),
        file: u64::from(information.nFileIndexHigh) << 32 | u64::from(information.nFileIndexLow),
    }))
}

#[cfg(not(any(unix, windows)))]
fn metadata_file_identity(
    _file: &File,
    _metadata: &Metadata,
) -> io::Result<Option<SourceFileIdentity>> {
    Ok(None)
}

fn metadata_mtime_ns(metadata: &Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    };

    use super::*;

    struct FakeProvider {
        metadata_before: SourceMetadata,
        metadata_after: SourceMetadata,
        range_metadata_before: SourceMetadata,
        range_metadata_after: SourceMetadata,
        bytes: Vec<u8>,
        reads: Mutex<Vec<(u64, usize)>>,
        metadata_reads: AtomicUsize,
    }

    impl SourceRangeProvider for FakeProvider {
        fn metadata(&self, _path: &Path) -> io::Result<SourceMetadata> {
            let call = self.metadata_reads.fetch_add(1, Ordering::SeqCst);
            Ok(if call == 0 {
                self.metadata_before
            } else {
                self.metadata_after
            })
        }

        fn read_range(&self, _path: &Path, start: u64, len: usize) -> io::Result<SourceRangeRead> {
            self.reads.lock().unwrap().push((start, len));
            let start = start as usize;
            let bytes = self
                .bytes
                .get(start..start.saturating_add(len))
                .unwrap_or_default()
                .to_vec();
            Ok(SourceRangeRead {
                bytes,
                metadata_before: self.range_metadata_before,
                metadata_after: self.range_metadata_after,
            })
        }
    }

    fn provider(source: &str) -> FakeProvider {
        let metadata = fake_metadata(source.len() as u64, 7, 11);
        FakeProvider {
            metadata_before: metadata,
            metadata_after: metadata,
            range_metadata_before: metadata,
            range_metadata_after: metadata,
            bytes: source.as_bytes().to_vec(),
            reads: Mutex::new(Vec::new()),
            metadata_reads: AtomicUsize::new(0),
        }
    }

    fn fake_metadata(size: u64, mtime_ns: i64, file: u64) -> SourceMetadata {
        SourceMetadata {
            is_file: true,
            revision: SourceFileRevision { size, mtime_ns },
            file_identity: Some(SourceFileIdentity { volume: 1, file }),
        }
    }

    fn expected_revision(metadata: SourceMetadata, excerpt: &[u8]) -> SourceRevision {
        SourceRevision {
            size: metadata.revision.size,
            mtime_ns: metadata.revision.mtime_ns,
            excerpt_hash: *blake3::hash(excerpt).as_bytes(),
        }
    }

    #[test]
    fn file_reader_fetches_only_the_requested_range() {
        let source = "prefix\nstruct Packet {\n  int size;\n};\nsuffix\n";
        let start = source.find("struct").unwrap();
        let end = source.find("\nsuffix").unwrap();
        let provider = provider(source);
        let expected = expected_revision(provider.metadata_before, &source.as_bytes()[start..end]);
        let reader = SourceExcerptReader::new(provider);
        let excerpt = reader.read_file(
            Path::new("packet.h"),
            SourceByteRange { start, end },
            expected,
        );
        assert_eq!(excerpt.text(), Some("struct Packet {\n  int size;\n};"));
        assert_eq!(
            *reader.provider.reads.lock().unwrap(),
            vec![(start as u64, end - start)]
        );
    }

    #[test]
    fn stale_revision_refuses_to_read_any_bytes() {
        let provider = provider("struct A {};\n");
        let reader = SourceExcerptReader::new(provider);
        let excerpt = reader.read_file(
            Path::new("a.h"),
            SourceByteRange { start: 0, end: 12 },
            SourceRevision {
                size: 12,
                mtime_ns: 99,
                excerpt_hash: *blake3::hash(b"struct A {};").as_bytes(),
            },
        );
        assert_eq!(
            excerpt,
            SourceExcerpt::Omitted(SourceExcerptOmission::StaleRevision)
        );
        assert!(reader.provider.reads.lock().unwrap().is_empty());
    }

    #[test]
    fn atomic_replacement_after_range_read_is_stale_even_with_same_revision() {
        let source = "struct A {};\n";
        let mut provider = provider(source);
        let expected = expected_revision(provider.metadata_before, source.as_bytes());
        provider.metadata_after = fake_metadata(expected.size, expected.mtime_ns, 22);
        let reader = SourceExcerptReader::new(provider);

        let excerpt = reader.read_file(
            Path::new("a.h"),
            SourceByteRange {
                start: 0,
                end: source.len(),
            },
            expected,
        );

        assert_eq!(
            excerpt,
            SourceExcerpt::Omitted(SourceExcerptOmission::StaleRevision)
        );
        assert_eq!(reader.provider.metadata_reads.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn revision_change_during_range_read_is_stale() {
        let source = "struct A {};\n";
        let mut provider = provider(source);
        let expected = expected_revision(provider.metadata_before, source.as_bytes());
        provider.range_metadata_after = fake_metadata(expected.size, 8, 11);
        provider.metadata_after = provider.range_metadata_after;
        let reader = SourceExcerptReader::new(provider);

        let excerpt = reader.read_file(
            Path::new("a.h"),
            SourceByteRange {
                start: 0,
                end: source.len(),
            },
            expected,
        );

        assert_eq!(
            excerpt,
            SourceExcerpt::Omitted(SourceExcerptOmission::StaleRevision)
        );
        assert_eq!(
            *reader.provider.reads.lock().unwrap(),
            vec![(0, source.len())]
        );
    }

    #[test]
    fn same_metadata_replacement_with_different_range_bytes_is_stale() {
        let original = "struct A {};\n";
        let replacement = "struct B {};\n";
        assert_eq!(original.len(), replacement.len());
        let mut provider = provider(replacement);
        let expected = expected_revision(provider.metadata_before, original.as_bytes());
        // The fake preserves size, mtime, and file identity, matching the
        // metadata blind spot that range-local content identity closes.
        provider.metadata_after = provider.metadata_before;
        provider.range_metadata_before = provider.metadata_before;
        provider.range_metadata_after = provider.metadata_before;
        let reader = SourceExcerptReader::new(provider);

        let excerpt = reader.read_file(
            Path::new("a.h"),
            SourceByteRange {
                start: 0,
                end: replacement.len(),
            },
            expected,
        );

        assert_eq!(
            excerpt,
            SourceExcerpt::Omitted(SourceExcerptOmission::StaleRevision)
        );
        assert_eq!(
            *reader.provider.reads.lock().unwrap(),
            vec![(0, replacement.len())]
        );
    }

    #[test]
    fn byte_and_line_limits_omit_instead_of_returning_partial_text() {
        let byte_reader = SourceExcerptReader::with_limits(provider("abcdef"), 4, 20);
        assert_eq!(
            byte_reader.read_buffer("abcdef", SourceByteRange { start: 0, end: 6 }),
            SourceExcerpt::Omitted(SourceExcerptOmission::ByteLimit)
        );

        let line_reader = SourceExcerptReader::with_limits(provider("a\nb\nc\n"), 64, 2);
        assert_eq!(
            line_reader.read_buffer("a\nb\nc\n", SourceByteRange { start: 0, end: 6 }),
            SourceExcerpt::Omitted(SourceExcerptOmission::LineLimit)
        );
    }

    #[test]
    fn buffer_reader_rejects_stale_and_non_utf8_boundaries() {
        let reader = SourceExcerptReader::new(provider("unused"));
        assert_eq!(
            reader.read_buffer("结构", SourceByteRange { start: 1, end: 3 }),
            SourceExcerpt::Omitted(SourceExcerptOmission::InvalidRange)
        );
        assert_eq!(
            reader.read_buffer("short", SourceByteRange { start: 0, end: 20 }),
            SourceExcerpt::Omitted(SourceExcerptOmission::StaleRevision)
        );
    }
}
