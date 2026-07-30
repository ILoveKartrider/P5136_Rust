#![forbid(unsafe_code)]

mod crypto;

use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use crypto::{KeyProvider, decrypt_in_place, packed_file_key};
use flate2::bufread::ZlibDecoder;
use md5::{Digest, Md5};
use thiserror::Error;

const RHO5_VERSION: u8 = 2;
const DATA_ALIGNMENT: u64 = 0x400;
const DOUBLE_ENCRYPTED_PREFIX: usize = 0x400;
const TABLE_FIXED_ENTRY_BYTES: u64 = 0x28;

/// Resource limits applied while scanning and extracting RHO5 entries.
///
/// Every value is enforced before its corresponding allocation or seek.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Rho5Limits {
    pub max_directory_entries: usize,
    pub max_archives: usize,
    pub max_archive_bytes: u64,
    pub max_archive_name_bytes: usize,
    pub max_files_per_archive: usize,
    pub max_total_files: usize,
    pub max_path_utf16_units: usize,
    pub max_normalized_path_bytes: usize,
    pub max_table_bytes: u64,
    pub max_compressed_bytes: usize,
    pub max_plaintext_bytes: usize,
    pub max_total_declared_compressed_bytes: u64,
    pub max_total_declared_plaintext_bytes: u64,
}

impl Default for Rho5Limits {
    fn default() -> Self {
        Self {
            max_directory_entries: 8_192,
            max_archives: 512,
            max_archive_bytes: 4 * 1024 * 1024 * 1024,
            max_archive_name_bytes: 255,
            max_files_per_archive: 250_000,
            max_total_files: 1_000_000,
            max_path_utf16_units: 4_096,
            max_normalized_path_bytes: 16 * 1024,
            max_table_bytes: 256 * 1024 * 1024,
            max_compressed_bytes: 512 * 1024 * 1024,
            max_plaintext_bytes: 1024 * 1024 * 1024,
            max_total_declared_compressed_bytes: 64 * 1024 * 1024 * 1024,
            max_total_declared_plaintext_bytes: 128 * 1024 * 1024 * 1024,
        }
    }
}

/// Physical locations derived from a RHO5 archive file name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rho5Offsets {
    pub header: u64,
    pub table: u64,
}

/// Metadata for one indexed RHO5 entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Rho5Entry {
    normalized_path: String,
    archive_path: PathBuf,
    archive_name: String,
    archive_length: u64,
    physical_data_offset: u64,
    compressed_size: usize,
    plaintext_size: usize,
    plaintext_md5: [u8; 16],
    key: [u8; 16],
}

impl Rho5Entry {
    #[must_use]
    pub fn normalized_path(&self) -> &str {
        &self.normalized_path
    }

    #[must_use]
    pub fn archive_path(&self) -> &Path {
        &self.archive_path
    }

    #[must_use]
    pub fn archive_name(&self) -> &str {
        &self.archive_name
    }

    #[must_use]
    pub fn physical_data_offset(&self) -> u64 {
        self.physical_data_offset
    }

    #[must_use]
    pub fn compressed_size(&self) -> usize {
        self.compressed_size
    }

    #[must_use]
    pub fn plaintext_size(&self) -> usize {
        self.plaintext_size
    }

    #[must_use]
    pub fn plaintext_md5(&self) -> [u8; 16] {
        self.plaintext_md5
    }
}

/// A bounded, immutable index of every `*.rho5` archive in one directory.
#[derive(Clone, Debug)]
pub struct Rho5Directory {
    directory: PathBuf,
    archive_count: usize,
    entries: Vec<Rho5Entry>,
    limits: Rho5Limits,
}

impl Rho5Directory {
    /// Scans all regular `*.rho5` files in `directory` using the KR P5136 keys.
    pub fn scan_kr(directory: impl AsRef<Path>, limits: Rho5Limits) -> Result<Self, Rho5Error> {
        validate_limits(&limits)?;
        let requested_directory = directory.as_ref();
        let canonical_directory =
            fs::canonicalize(requested_directory).map_err(|source| Rho5Error::Io {
                operation: "canonicalize RHO5 directory",
                path: requested_directory.to_path_buf(),
                source,
            })?;
        let metadata = fs::metadata(&canonical_directory).map_err(|source| Rho5Error::Io {
            operation: "inspect RHO5 directory",
            path: canonical_directory.clone(),
            source,
        })?;
        if !metadata.is_dir() {
            return Err(Rho5Error::NotDirectory {
                path: canonical_directory,
            });
        }
        let archive_paths = collect_archive_paths(&canonical_directory, &limits)?;
        let entries = index_archives(&archive_paths, &limits)?;

        Ok(Self {
            directory: canonical_directory,
            archive_count: archive_paths.len(),
            entries,
            limits,
        })
    }

    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    #[must_use]
    pub fn archive_count(&self) -> usize {
        self.archive_count
    }

    #[must_use]
    pub fn entries(&self) -> &[Rho5Entry] {
        &self.entries
    }

    /// Returns the only entry whose normalized path exactly matches `path`.
    ///
    /// Matching remains case-sensitive. Slash direction, repeated separators,
    /// leading separators, `.` segments, and Unicode canonical composition are
    /// normalized before the exact comparison.
    pub fn unique_entry(&self, path: &str) -> Result<&Rho5Entry, Rho5Error> {
        let normalized = normalize_path(path, &self.limits)?;
        let mut matches = self
            .entries
            .iter()
            .filter(|entry| entry.normalized_path == normalized);
        let Some(first) = matches.next() else {
            return Err(Rho5Error::EntryNotFound { path: normalized });
        };
        let duplicate_count = 1 + matches.count();
        if duplicate_count != 1 {
            return Err(Rho5Error::DuplicateEntry {
                path: normalized,
                count: duplicate_count,
            });
        }
        Ok(first)
    }

    /// Extracts and authenticates the only exact normalized match for `path`.
    pub fn extract_exact(&self, path: &str) -> Result<Vec<u8>, Rho5Error> {
        let entry = self.unique_entry(path)?;
        extract_entry(entry, &self.limits)
    }
}

/// Computes the encrypted header and file-table positions for a P5136 RHO5 name.
pub fn archive_offsets(archive_name: &str) -> Result<Rho5Offsets, Rho5Error> {
    if archive_name.is_empty() || !archive_name.is_ascii() {
        return Err(Rho5Error::UnsupportedArchiveName {
            name: archive_name.to_owned(),
        });
    }
    let sum = archive_name
        .to_ascii_lowercase()
        .bytes()
        .try_fold(0_u64, |sum, byte| sum.checked_add(u64::from(byte)))
        .ok_or(Rho5Error::ArithmeticOverflow {
            field: "archive-name checksum",
        })?;
    let header = (sum % 0x138)
        .checked_add(0x1e)
        .ok_or(Rho5Error::ArithmeticOverflow {
            field: "header offset",
        })?;
    let table_delta = sum.checked_mul(3).ok_or(Rho5Error::ArithmeticOverflow {
        field: "file-table offset",
    })? % 0xd4
        + 0x2a;
    let table = header
        .checked_add(table_delta)
        .ok_or(Rho5Error::ArithmeticOverflow {
            field: "file-table position",
        })?;
    Ok(Rho5Offsets { header, table })
}

#[derive(Debug, Error)]
pub enum Rho5Error {
    #[error("{path} is not a directory")]
    NotDirectory { path: PathBuf },
    #[error("RHO5 directory contains more than the configured {limit} entries")]
    TooManyDirectoryEntries { limit: usize },
    #[error("RHO5 directory contains more than the configured {limit} archives")]
    TooManyArchives { limit: usize },
    #[error("archive {path} is {size} bytes, exceeding the configured {limit}-byte limit")]
    ArchiveTooLarge {
        path: PathBuf,
        size: u64,
        limit: u64,
    },
    #[error("archive name is longer than the configured {limit}-byte limit: {path}")]
    ArchiveNameTooLong { path: PathBuf, limit: usize },
    #[error("unsupported non-ASCII or empty RHO5 archive name {name:?}")]
    UnsupportedArchiveName { name: String },
    #[error("archive name is not valid Unicode: {path}")]
    NonUnicodeArchiveName { path: PathBuf },
    #[error("archive {path} is truncated: need byte {needed}, length is {actual}")]
    ArchiveTruncated {
        path: PathBuf,
        needed: u64,
        actual: u64,
    },
    #[error("archive {path} changed after it was indexed")]
    ArchiveChanged { path: PathBuf },
    #[error("archive {path} has invalid header checksum {actual:#010x}; expected {expected:#010x}")]
    HeaderChecksumMismatch {
        path: PathBuf,
        actual: i32,
        expected: i32,
    },
    #[error("archive {path} uses unsupported RHO5 version {version}")]
    UnsupportedVersion { path: PathBuf, version: u8 },
    #[error("archive {path} declares negative file count {count}")]
    NegativeFileCount { path: PathBuf, count: i32 },
    #[error("archive {path} contains {count} files, exceeding the configured {limit} limit")]
    TooManyFiles {
        path: PathBuf,
        count: usize,
        limit: usize,
    },
    #[error("RHO5 scan exceeds the configured total file limit {limit}")]
    TooManyTotalFiles { limit: usize },
    #[error("archive {path} entry {entry_index} has invalid UTF-16 path length {units}")]
    InvalidPathLength {
        path: PathBuf,
        entry_index: usize,
        units: i32,
    },
    #[error(
        "archive {path} entry {entry_index} path has {units} UTF-16 units, exceeding limit {limit}"
    )]
    PathTooLong {
        path: PathBuf,
        entry_index: usize,
        units: usize,
        limit: usize,
    },
    #[error("archive {path} entry {entry_index} contains invalid UTF-16")]
    InvalidUtf16Path { path: PathBuf, entry_index: usize },
    #[error("invalid normalized RHO5 path {path:?}: {reason}")]
    InvalidNormalizedPath { path: String, reason: &'static str },
    #[error("normalized RHO5 path exceeds the configured {limit}-byte limit")]
    NormalizedPathTooLong { limit: usize },
    #[error("archive {path} table exceeds the configured {limit}-byte limit")]
    TableTooLarge { path: PathBuf, limit: u64 },
    #[error(
        "archive {path} entry {entry_path:?} has invalid table checksum {actual:#010x}; expected {expected:#010x}"
    )]
    EntryChecksumMismatch {
        path: PathBuf,
        entry_path: String,
        actual: i32,
        expected: i32,
    },
    #[error("archive {path} entry {entry_path:?} has negative block offset {offset}")]
    NegativeBlockOffset {
        path: PathBuf,
        entry_path: String,
        offset: i32,
    },
    #[error("archive {path} entry {entry_path:?} has negative compressed size {size}")]
    NegativeCompressedSize {
        path: PathBuf,
        entry_path: String,
        size: i32,
    },
    #[error("archive {path} entry {entry_path:?} has negative plaintext size {size}")]
    NegativePlaintextSize {
        path: PathBuf,
        entry_path: String,
        size: i32,
    },
    #[error(
        "archive {path} entry {entry_path:?} compressed size {size} exceeds configured limit {limit}"
    )]
    CompressedSizeTooLarge {
        path: PathBuf,
        entry_path: String,
        size: usize,
        limit: usize,
    },
    #[error(
        "archive {path} entry {entry_path:?} plaintext size {size} exceeds configured limit {limit}"
    )]
    PlaintextSizeTooLarge {
        path: PathBuf,
        entry_path: String,
        size: usize,
        limit: usize,
    },
    #[error(
        "archive {path} entry {entry_path:?} data [{offset}, {end}) exceeds archive length {archive_length}"
    )]
    EntryOutOfBounds {
        path: PathBuf,
        entry_path: String,
        offset: u64,
        end: u64,
        archive_length: u64,
    },
    #[error("total declared compressed size {size} exceeds the configured {limit}-byte limit")]
    TotalDeclaredCompressedSizeTooLarge { size: u64, limit: u64 },
    #[error("total declared plaintext size {size} exceeds the configured {limit}-byte limit")]
    TotalDeclaredPlaintextSizeTooLarge { size: u64, limit: u64 },
    #[error("no RHO5 entry exactly matches normalized path {path:?}")]
    EntryNotFound { path: String },
    #[error("{count} RHO5 entries match normalized path {path:?}")]
    DuplicateEntry { path: String, count: usize },
    #[error("entry {path:?} decompressed past its declared {expected}-byte plaintext length")]
    DecompressedSizeLimitExceeded { path: String, expected: usize },
    #[error("entry {path:?} decompressed to {actual} bytes; expected {expected}")]
    PlaintextSizeMismatch {
        path: String,
        actual: usize,
        expected: usize,
    },
    #[error("entry {path:?} zlib stream consumed {consumed} of {compressed_size} compressed bytes")]
    TrailingCompressedData {
        path: String,
        consumed: u64,
        compressed_size: usize,
    },
    #[error("entry {path:?} plaintext MD5 does not match the authenticated table value")]
    PlaintextMd5Mismatch { path: String },
    #[error("integer overflow while computing {field}")]
    ArithmeticOverflow { field: &'static str },
    #[error("RHO5 resource limit {field} must be nonzero")]
    InvalidLimits { field: &'static str },
    #[error("{operation} failed for {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("zlib decompression failed for {path:?}: {source}")]
    Decompression {
        path: String,
        #[source]
        source: io::Error,
    },
}

fn validate_limits(limits: &Rho5Limits) -> Result<(), Rho5Error> {
    let required_nonzero = [
        ("max_directory_entries", limits.max_directory_entries),
        ("max_archives", limits.max_archives),
        ("max_archive_name_bytes", limits.max_archive_name_bytes),
        ("max_files_per_archive", limits.max_files_per_archive),
        ("max_total_files", limits.max_total_files),
        ("max_path_utf16_units", limits.max_path_utf16_units),
        (
            "max_normalized_path_bytes",
            limits.max_normalized_path_bytes,
        ),
        ("max_compressed_bytes", limits.max_compressed_bytes),
        ("max_plaintext_bytes", limits.max_plaintext_bytes),
    ];
    if let Some((field, _)) = required_nonzero.into_iter().find(|(_, value)| *value == 0) {
        return Err(Rho5Error::InvalidLimits { field });
    }
    if limits.max_archive_bytes == 0
        || limits.max_table_bytes == 0
        || limits.max_total_declared_compressed_bytes == 0
        || limits.max_total_declared_plaintext_bytes == 0
    {
        return Err(Rho5Error::InvalidLimits {
            field: "u64 resource limit",
        });
    }
    Ok(())
}

fn has_rho5_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("rho5"))
}

fn collect_archive_paths(directory: &Path, limits: &Rho5Limits) -> Result<Vec<PathBuf>, Rho5Error> {
    let reader = fs::read_dir(directory).map_err(|source| Rho5Error::Io {
        operation: "read RHO5 directory",
        path: directory.to_path_buf(),
        source,
    })?;
    let mut scanned_count = 0_usize;
    let mut archive_paths = Vec::new();
    for result in reader {
        scanned_count = scanned_count
            .checked_add(1)
            .ok_or(Rho5Error::ArithmeticOverflow {
                field: "directory entry count",
            })?;
        if scanned_count > limits.max_directory_entries {
            return Err(Rho5Error::TooManyDirectoryEntries {
                limit: limits.max_directory_entries,
            });
        }
        let entry = result.map_err(|source| Rho5Error::Io {
            operation: "read RHO5 directory entry",
            path: directory.to_path_buf(),
            source,
        })?;
        let file_type = entry.file_type().map_err(|source| Rho5Error::Io {
            operation: "inspect RHO5 directory entry",
            path: entry.path(),
            source,
        })?;
        if file_type.is_file() && has_rho5_extension(&entry.path()) {
            archive_paths.push(entry.path());
            if archive_paths.len() > limits.max_archives {
                return Err(Rho5Error::TooManyArchives {
                    limit: limits.max_archives,
                });
            }
        }
    }
    archive_paths.sort_unstable();
    Ok(archive_paths)
}

fn index_archives(
    archive_paths: &[PathBuf],
    limits: &Rho5Limits,
) -> Result<Vec<Rho5Entry>, Rho5Error> {
    let mut entries = Vec::new();
    let mut total_compressed = 0_u64;
    let mut total_plaintext = 0_u64;
    for archive_path in archive_paths {
        let mut archive_entries = parse_archive(archive_path, limits)?;
        let new_total = entries.len().checked_add(archive_entries.len()).ok_or(
            Rho5Error::ArithmeticOverflow {
                field: "total indexed file count",
            },
        )?;
        if new_total > limits.max_total_files {
            return Err(Rho5Error::TooManyTotalFiles {
                limit: limits.max_total_files,
            });
        }
        for entry in &archive_entries {
            total_compressed = add_declared_size(
                total_compressed,
                entry.compressed_size,
                limits.max_total_declared_compressed_bytes,
                true,
            )?;
            total_plaintext = add_declared_size(
                total_plaintext,
                entry.plaintext_size,
                limits.max_total_declared_plaintext_bytes,
                false,
            )?;
        }
        entries.append(&mut archive_entries);
    }
    Ok(entries)
}

fn add_declared_size(
    total: u64,
    size: usize,
    limit: u64,
    compressed: bool,
) -> Result<u64, Rho5Error> {
    let size = u64::try_from(size).map_err(|_| Rho5Error::ArithmeticOverflow {
        field: "declared RHO5 entry size",
    })?;
    let new_total = total
        .checked_add(size)
        .ok_or(Rho5Error::ArithmeticOverflow {
            field: "total declared RHO5 size",
        })?;
    if new_total > limit {
        return if compressed {
            Err(Rho5Error::TotalDeclaredCompressedSizeTooLarge {
                size: new_total,
                limit,
            })
        } else {
            Err(Rho5Error::TotalDeclaredPlaintextSizeTooLarge {
                size: new_total,
                limit,
            })
        };
    }
    Ok(new_total)
}

fn parse_archive(path: &Path, limits: &Rho5Limits) -> Result<Vec<Rho5Entry>, Rho5Error> {
    let descriptor = read_archive_descriptor(path, limits)?;
    let pending = read_archive_table(path, &descriptor, limits)?;
    finalize_entries(path, &descriptor, pending)
}

fn read_archive_descriptor(
    path: &Path,
    limits: &Rho5Limits,
) -> Result<ArchiveDescriptor, Rho5Error> {
    let archive_metadata = fs::metadata(path).map_err(|source| Rho5Error::Io {
        operation: "inspect RHO5 archive",
        path: path.to_path_buf(),
        source,
    })?;
    let archive_length = archive_metadata.len();
    if archive_length > limits.max_archive_bytes {
        return Err(Rho5Error::ArchiveTooLarge {
            path: path.to_path_buf(),
            size: archive_length,
            limit: limits.max_archive_bytes,
        });
    }
    let archive_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Rho5Error::NonUnicodeArchiveName {
            path: path.to_path_buf(),
        })?;
    if archive_name.len() > limits.max_archive_name_bytes {
        return Err(Rho5Error::ArchiveNameTooLong {
            path: path.to_path_buf(),
            limit: limits.max_archive_name_bytes,
        });
    }
    let offsets = archive_offsets(archive_name)?;
    ensure_file_range(path, archive_length, offsets.header, 9)?;
    ensure_file_range(path, archive_length, offsets.table, 1)?;

    let mut header_file = open_archive(path)?;
    header_file
        .seek(SeekFrom::Start(offsets.header))
        .map_err(|source| Rho5Error::Io {
            operation: "seek RHO5 header",
            path: path.to_path_buf(),
            source,
        })?;
    let mut header_reader = DecryptReader::new(
        header_file,
        KeyProvider::for_header(archive_name),
        offsets.header,
    );
    let header_checksum = header_reader.read_i32(path, "read RHO5 header checksum")?;
    let version = header_reader.read_u8(path, "read RHO5 version")?;
    let file_count_i32 = header_reader.read_i32(path, "read RHO5 file count")?;
    let expected_header_checksum = i32::from(version).wrapping_add(file_count_i32);
    if header_checksum != expected_header_checksum {
        return Err(Rho5Error::HeaderChecksumMismatch {
            path: path.to_path_buf(),
            actual: header_checksum,
            expected: expected_header_checksum,
        });
    }
    if version != RHO5_VERSION {
        return Err(Rho5Error::UnsupportedVersion {
            path: path.to_path_buf(),
            version,
        });
    }
    let file_count = usize::try_from(file_count_i32).map_err(|_| Rho5Error::NegativeFileCount {
        path: path.to_path_buf(),
        count: file_count_i32,
    })?;
    if file_count > limits.max_files_per_archive {
        return Err(Rho5Error::TooManyFiles {
            path: path.to_path_buf(),
            count: file_count,
            limit: limits.max_files_per_archive,
        });
    }
    let minimum_table_bytes = (file_count as u64)
        .checked_mul(TABLE_FIXED_ENTRY_BYTES)
        .ok_or(Rho5Error::ArithmeticOverflow {
            field: "minimum RHO5 table length",
        })?;
    if minimum_table_bytes > limits.max_table_bytes {
        return Err(Rho5Error::TableTooLarge {
            path: path.to_path_buf(),
            limit: limits.max_table_bytes,
        });
    }
    ensure_file_range(path, archive_length, offsets.table, minimum_table_bytes)?;

    Ok(ArchiveDescriptor {
        archive_name: archive_name.to_owned(),
        archive_length,
        offsets,
        file_count,
    })
}

fn read_archive_table(
    path: &Path,
    descriptor: &ArchiveDescriptor,
    limits: &Rho5Limits,
) -> Result<PendingTable, Rho5Error> {
    let mut table_file = open_archive(path)?;
    table_file
        .seek(SeekFrom::Start(descriptor.offsets.table))
        .map_err(|source| Rho5Error::Io {
            operation: "seek RHO5 file table",
            path: path.to_path_buf(),
            source,
        })?;
    let mut table_reader = DecryptReader::new(
        table_file,
        KeyProvider::for_table(&descriptor.archive_name),
        descriptor.offsets.table,
    );
    let mut pending = Vec::with_capacity(descriptor.file_count);
    for entry_index in 0..descriptor.file_count {
        pending.push(read_pending_entry(
            &mut table_reader,
            path,
            entry_index,
            limits,
        )?);
        let table_bytes = table_reader
            .logical_position()
            .checked_sub(descriptor.offsets.table)
            .ok_or(Rho5Error::ArithmeticOverflow {
                field: "RHO5 table length",
            })?;
        if table_bytes > limits.max_table_bytes {
            return Err(Rho5Error::TableTooLarge {
                path: path.to_path_buf(),
                limit: limits.max_table_bytes,
            });
        }
    }

    let table_end = table_reader.logical_position();
    let data_base = align_up(table_end, DATA_ALIGNMENT)?;
    if data_base > descriptor.archive_length {
        return Err(Rho5Error::ArchiveTruncated {
            path: path.to_path_buf(),
            needed: data_base,
            actual: descriptor.archive_length,
        });
    }

    Ok(PendingTable {
        entries: pending,
        data_base,
    })
}

fn read_pending_entry(
    reader: &mut DecryptReader,
    archive_path: &Path,
    entry_index: usize,
    limits: &Rho5Limits,
) -> Result<PendingEntry, Rho5Error> {
    let raw_path = read_entry_path(reader, archive_path, entry_index, limits)?;
    let normalized_path = normalize_path(&raw_path, limits)?;
    let metadata = read_entry_metadata(reader, archive_path)?;
    verify_entry_checksum(archive_path, &normalized_path, &metadata)?;

    let block_offset =
        u64::try_from(metadata.block_offset).map_err(|_| Rho5Error::NegativeBlockOffset {
            path: archive_path.to_path_buf(),
            entry_path: normalized_path.clone(),
            offset: metadata.block_offset,
        })?;
    let compressed_size = usize::try_from(metadata.compressed_size).map_err(|_| {
        Rho5Error::NegativeCompressedSize {
            path: archive_path.to_path_buf(),
            entry_path: normalized_path.clone(),
            size: metadata.compressed_size,
        }
    })?;
    let plaintext_size =
        usize::try_from(metadata.plaintext_size).map_err(|_| Rho5Error::NegativePlaintextSize {
            path: archive_path.to_path_buf(),
            entry_path: normalized_path.clone(),
            size: metadata.plaintext_size,
        })?;
    if compressed_size > limits.max_compressed_bytes {
        return Err(Rho5Error::CompressedSizeTooLarge {
            path: archive_path.to_path_buf(),
            entry_path: normalized_path,
            size: compressed_size,
            limit: limits.max_compressed_bytes,
        });
    }
    if plaintext_size > limits.max_plaintext_bytes {
        return Err(Rho5Error::PlaintextSizeTooLarge {
            path: archive_path.to_path_buf(),
            entry_path: normalized_path,
            size: plaintext_size,
            limit: limits.max_plaintext_bytes,
        });
    }
    Ok(PendingEntry {
        normalized_path,
        raw_path,
        block_offset,
        compressed_size,
        plaintext_size,
        plaintext_md5: metadata.plaintext_md5,
    })
}

fn read_entry_path(
    reader: &mut DecryptReader,
    archive_path: &Path,
    entry_index: usize,
    limits: &Rho5Limits,
) -> Result<String, Rho5Error> {
    let units_i32 = reader.read_i32(archive_path, "read RHO5 path length")?;
    let units = usize::try_from(units_i32).map_err(|_| Rho5Error::InvalidPathLength {
        path: archive_path.to_path_buf(),
        entry_index,
        units: units_i32,
    })?;
    if units == 0 {
        return Err(Rho5Error::InvalidPathLength {
            path: archive_path.to_path_buf(),
            entry_index,
            units: units_i32,
        });
    }
    if units > limits.max_path_utf16_units {
        return Err(Rho5Error::PathTooLong {
            path: archive_path.to_path_buf(),
            entry_index,
            units,
            limit: limits.max_path_utf16_units,
        });
    }
    let path_byte_count = units.checked_mul(2).ok_or(Rho5Error::ArithmeticOverflow {
        field: "UTF-16 path byte count",
    })?;
    let mut path_bytes = vec![0_u8; path_byte_count];
    reader.read_exact(archive_path, "read RHO5 UTF-16 path", &mut path_bytes)?;
    let utf16: Vec<u16> = path_bytes
        .chunks_exact(2)
        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
        .collect();
    String::from_utf16(&utf16).map_err(|_| Rho5Error::InvalidUtf16Path {
        path: archive_path.to_path_buf(),
        entry_index,
    })
}

fn read_entry_metadata(
    reader: &mut DecryptReader,
    archive_path: &Path,
) -> Result<RawEntryMetadata, Rho5Error> {
    let entry_checksum = reader.read_i32(archive_path, "read RHO5 entry checksum")?;
    let unknown = reader.read_i32(archive_path, "read RHO5 entry flags")?;
    let block_offset = reader.read_i32(archive_path, "read RHO5 entry block offset")?;
    let plaintext_size = reader.read_i32(archive_path, "read RHO5 plaintext size")?;
    let compressed_size = reader.read_i32(archive_path, "read RHO5 compressed size")?;
    let mut plaintext_md5 = [0_u8; 16];
    reader.read_exact(archive_path, "read RHO5 plaintext MD5", &mut plaintext_md5)?;
    Ok(RawEntryMetadata {
        entry_checksum,
        unknown,
        block_offset,
        plaintext_size,
        compressed_size,
        plaintext_md5,
    })
}

fn verify_entry_checksum(
    archive_path: &Path,
    normalized_path: &str,
    metadata: &RawEntryMetadata,
) -> Result<(), Rho5Error> {
    let expected = metadata.plaintext_md5.iter().fold(
        metadata
            .unknown
            .wrapping_add(metadata.block_offset)
            .wrapping_add(metadata.plaintext_size)
            .wrapping_add(metadata.compressed_size),
        |checksum, byte| checksum.wrapping_add(i32::from(*byte)),
    );
    if metadata.entry_checksum != expected {
        return Err(Rho5Error::EntryChecksumMismatch {
            path: archive_path.to_path_buf(),
            entry_path: normalized_path.to_owned(),
            actual: metadata.entry_checksum,
            expected,
        });
    }
    Ok(())
}

fn finalize_entries(
    path: &Path,
    descriptor: &ArchiveDescriptor,
    pending: PendingTable,
) -> Result<Vec<Rho5Entry>, Rho5Error> {
    let archive_length = descriptor.archive_length;
    let PendingTable {
        entries: pending_entries,
        data_base,
    } = pending;
    let mut entries = Vec::with_capacity(pending_entries.len());
    for pending_entry in pending_entries {
        let relative_offset = pending_entry
            .block_offset
            .checked_mul(DATA_ALIGNMENT)
            .ok_or(Rho5Error::ArithmeticOverflow {
                field: "RHO5 entry block offset",
            })?;
        let physical_data_offset =
            data_base
                .checked_add(relative_offset)
                .ok_or(Rho5Error::ArithmeticOverflow {
                    field: "RHO5 entry physical offset",
                })?;
        let data_end = physical_data_offset
            .checked_add(u64::try_from(pending_entry.compressed_size).map_err(|_| {
                Rho5Error::ArithmeticOverflow {
                    field: "RHO5 entry compressed size",
                }
            })?)
            .ok_or(Rho5Error::ArithmeticOverflow {
                field: "RHO5 entry data end",
            })?;
        if data_end > archive_length {
            return Err(Rho5Error::EntryOutOfBounds {
                path: path.to_path_buf(),
                entry_path: pending_entry.normalized_path,
                offset: physical_data_offset,
                end: data_end,
                archive_length,
            });
        }
        let key = packed_file_key(&pending_entry.plaintext_md5, &pending_entry.raw_path);
        entries.push(Rho5Entry {
            normalized_path: pending_entry.normalized_path,
            archive_path: path.to_path_buf(),
            archive_name: descriptor.archive_name.clone(),
            archive_length,
            physical_data_offset,
            compressed_size: pending_entry.compressed_size,
            plaintext_size: pending_entry.plaintext_size,
            plaintext_md5: pending_entry.plaintext_md5,
            key,
        });
    }
    Ok(entries)
}

fn extract_entry(entry: &Rho5Entry, limits: &Rho5Limits) -> Result<Vec<u8>, Rho5Error> {
    if entry.compressed_size > limits.max_compressed_bytes {
        return Err(Rho5Error::CompressedSizeTooLarge {
            path: entry.archive_path.clone(),
            entry_path: entry.normalized_path.clone(),
            size: entry.compressed_size,
            limit: limits.max_compressed_bytes,
        });
    }
    if entry.plaintext_size > limits.max_plaintext_bytes {
        return Err(Rho5Error::PlaintextSizeTooLarge {
            path: entry.archive_path.clone(),
            entry_path: entry.normalized_path.clone(),
            size: entry.plaintext_size,
            limit: limits.max_plaintext_bytes,
        });
    }
    let compressed = read_decrypted_compressed(entry)?;
    decompress_and_authenticate(entry, &compressed)
}

fn read_decrypted_compressed(entry: &Rho5Entry) -> Result<Vec<u8>, Rho5Error> {
    let mut archive = open_archive(&entry.archive_path)?;
    let current_length = archive
        .metadata()
        .map_err(|source| Rho5Error::Io {
            operation: "inspect indexed RHO5 archive",
            path: entry.archive_path.clone(),
            source,
        })?
        .len();
    if current_length != entry.archive_length {
        return Err(Rho5Error::ArchiveChanged {
            path: entry.archive_path.clone(),
        });
    }
    let data_end = entry
        .physical_data_offset
        .checked_add(u64::try_from(entry.compressed_size).map_err(|_| {
            Rho5Error::ArithmeticOverflow {
                field: "RHO5 extraction compressed size",
            }
        })?)
        .ok_or(Rho5Error::ArithmeticOverflow {
            field: "RHO5 extraction range",
        })?;
    if data_end > current_length {
        return Err(Rho5Error::ArchiveChanged {
            path: entry.archive_path.clone(),
        });
    }
    archive
        .seek(SeekFrom::Start(entry.physical_data_offset))
        .map_err(|source| Rho5Error::Io {
            operation: "seek RHO5 entry data",
            path: entry.archive_path.clone(),
            source,
        })?;
    let mut compressed = vec![0_u8; entry.compressed_size];
    archive
        .read_exact(&mut compressed)
        .map_err(|source| Rho5Error::Io {
            operation: "read RHO5 entry data",
            path: entry.archive_path.clone(),
            source,
        })?;

    // RHO5 applies one key stream to all compressed bytes, plus a fresh key
    // stream to the first 0x400 bytes. Both streams start at word zero.
    let prefix_length = compressed.len().min(DOUBLE_ENCRYPTED_PREFIX);
    decrypt_in_place(&mut compressed[..prefix_length], &entry.key);
    decrypt_in_place(&mut compressed, &entry.key);
    Ok(compressed)
}

fn decompress_and_authenticate(entry: &Rho5Entry, compressed: &[u8]) -> Result<Vec<u8>, Rho5Error> {
    let mut decoder = ZlibDecoder::new(compressed);
    let mut plaintext = Vec::with_capacity(entry.plaintext_size);
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = decoder
            .read(&mut buffer)
            .map_err(|source| Rho5Error::Decompression {
                path: entry.normalized_path.clone(),
                source,
            })?;
        if read == 0 {
            break;
        }
        let new_length =
            plaintext
                .len()
                .checked_add(read)
                .ok_or(Rho5Error::ArithmeticOverflow {
                    field: "decompressed RHO5 entry length",
                })?;
        if new_length > entry.plaintext_size {
            return Err(Rho5Error::DecompressedSizeLimitExceeded {
                path: entry.normalized_path.clone(),
                expected: entry.plaintext_size,
            });
        }
        plaintext.extend_from_slice(&buffer[..read]);
    }
    let consumed = decoder.total_in();
    if consumed
        != u64::try_from(entry.compressed_size).map_err(|_| Rho5Error::ArithmeticOverflow {
            field: "RHO5 compressed stream length",
        })?
    {
        return Err(Rho5Error::TrailingCompressedData {
            path: entry.normalized_path.clone(),
            consumed,
            compressed_size: entry.compressed_size,
        });
    }
    if plaintext.len() != entry.plaintext_size {
        return Err(Rho5Error::PlaintextSizeMismatch {
            path: entry.normalized_path.clone(),
            actual: plaintext.len(),
            expected: entry.plaintext_size,
        });
    }
    let actual_md5: [u8; 16] = Md5::digest(&plaintext).into();
    if actual_md5 != entry.plaintext_md5 {
        return Err(Rho5Error::PlaintextMd5Mismatch {
            path: entry.normalized_path.clone(),
        });
    }
    Ok(plaintext)
}

fn normalize_path(path: &str, limits: &Rho5Limits) -> Result<String, Rho5Error> {
    use unicode_normalization::UnicodeNormalization;

    if path.contains('\0') {
        return Err(Rho5Error::InvalidNormalizedPath {
            path: path.to_owned(),
            reason: "NUL is not allowed",
        });
    }
    let replaced = path.replace('\\', "/");
    let mut segments = Vec::new();
    for segment in replaced.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                return Err(Rho5Error::InvalidNormalizedPath {
                    path: path.to_owned(),
                    reason: "parent traversal is not allowed",
                });
            }
            value => segments.push(value),
        }
    }
    if segments.is_empty() {
        return Err(Rho5Error::InvalidNormalizedPath {
            path: path.to_owned(),
            reason: "path is empty after normalization",
        });
    }
    let normalized: String = segments.join("/").nfc().collect();
    if normalized.len() > limits.max_normalized_path_bytes {
        return Err(Rho5Error::NormalizedPathTooLong {
            limit: limits.max_normalized_path_bytes,
        });
    }
    Ok(normalized)
}

fn open_archive(path: &Path) -> Result<File, Rho5Error> {
    File::open(path).map_err(|source| Rho5Error::Io {
        operation: "open RHO5 archive",
        path: path.to_path_buf(),
        source,
    })
}

fn ensure_file_range(
    path: &Path,
    archive_length: u64,
    offset: u64,
    size: u64,
) -> Result<(), Rho5Error> {
    let needed = offset
        .checked_add(size)
        .ok_or(Rho5Error::ArithmeticOverflow {
            field: "RHO5 file range",
        })?;
    if needed > archive_length {
        return Err(Rho5Error::ArchiveTruncated {
            path: path.to_path_buf(),
            needed,
            actual: archive_length,
        });
    }
    Ok(())
}

fn align_up(value: u64, alignment: u64) -> Result<u64, Rho5Error> {
    let mask = alignment
        .checked_sub(1)
        .ok_or(Rho5Error::ArithmeticOverflow {
            field: "RHO5 alignment",
        })?;
    value
        .checked_add(mask)
        .map(|sum| sum & !mask)
        .ok_or(Rho5Error::ArithmeticOverflow {
            field: "aligned RHO5 offset",
        })
}

struct PendingEntry {
    normalized_path: String,
    raw_path: String,
    block_offset: u64,
    compressed_size: usize,
    plaintext_size: usize,
    plaintext_md5: [u8; 16],
}

struct RawEntryMetadata {
    entry_checksum: i32,
    unknown: i32,
    block_offset: i32,
    plaintext_size: i32,
    compressed_size: i32,
    plaintext_md5: [u8; 16],
}

struct PendingTable {
    entries: Vec<PendingEntry>,
    data_base: u64,
}

struct ArchiveDescriptor {
    archive_name: String,
    archive_length: u64,
    offsets: Rho5Offsets,
    file_count: usize,
}

struct DecryptReader {
    file: File,
    provider: KeyProvider,
    decrypted_word: [u8; 4],
    word_position: usize,
    logical_position: u64,
}

impl DecryptReader {
    fn new(file: File, provider: KeyProvider, logical_position: u64) -> Self {
        Self {
            file,
            provider,
            decrypted_word: [0; 4],
            word_position: 4,
            logical_position,
        }
    }

    fn logical_position(&self) -> u64 {
        self.logical_position
    }

    fn read_i32(&mut self, path: &Path, operation: &'static str) -> Result<i32, Rho5Error> {
        let mut bytes = [0_u8; 4];
        self.read_exact(path, operation, &mut bytes)?;
        Ok(i32::from_le_bytes(bytes))
    }

    fn read_u8(&mut self, path: &Path, operation: &'static str) -> Result<u8, Rho5Error> {
        let mut byte = [0_u8; 1];
        self.read_exact(path, operation, &mut byte)?;
        Ok(byte[0])
    }

    fn read_exact(
        &mut self,
        path: &Path,
        operation: &'static str,
        destination: &mut [u8],
    ) -> Result<(), Rho5Error> {
        let mut written = 0_usize;
        while written < destination.len() {
            if self.word_position == self.decrypted_word.len() {
                let mut encrypted = [0_u8; 4];
                self.file
                    .read_exact(&mut encrypted)
                    .map_err(|source| Rho5Error::Io {
                        operation,
                        path: path.to_path_buf(),
                        source,
                    })?;
                let decrypted = u32::from_le_bytes(encrypted)
                    .wrapping_sub(self.provider.next_word())
                    .to_le_bytes();
                self.decrypted_word = decrypted;
                self.word_position = 0;
            }
            let available = self.decrypted_word.len() - self.word_position;
            let copy_length = available.min(destination.len() - written);
            destination[written..written + copy_length].copy_from_slice(
                &self.decrypted_word[self.word_position..self.word_position + copy_length],
            );
            self.word_position += copy_length;
            written += copy_length;
            self.logical_position = self
                .logical_position
                .checked_add(copy_length as u64)
                .ok_or(Rho5Error::ArithmeticOverflow {
                    field: "decrypted RHO5 stream position",
                })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
