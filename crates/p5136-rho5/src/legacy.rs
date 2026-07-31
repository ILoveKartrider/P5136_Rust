//! Bounded read-only access to the legacy `Rh layer spec 1.1` archives used
//! by the Korean P5136 `item.rho`.

use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use flate2::bufread::ZlibDecoder;
use thiserror::Error;

use crate::legacy_vectors::get_vector;

const HEADER_MAGIC_11: &str = "Rh layer spec 1.1";
const HEADER_INFO_OFFSET: usize = 0x80;
const HEADER_INFO_LENGTH: usize = 0x80;
const BLOCK_TABLE_OFFSET: usize = 0x100;
const BLOCK_INFO_LENGTH: usize = 0x20;
const VERSION_MAGIC_11: u32 = 0x0001_0001;
const END_MAGIC: u32 = 0xfc1f_9778;
const ROOT_DIRECTORY_INDEX: u32 = u32::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegacyRhoLimits {
    pub max_archive_bytes: usize,
    pub max_blocks: usize,
    pub max_compressed_block_bytes: usize,
    pub max_plaintext_block_bytes: usize,
    pub max_entries_per_directory: usize,
    pub max_path_components: usize,
    pub max_name_utf16_units: usize,
}

impl Default for LegacyRhoLimits {
    fn default() -> Self {
        Self {
            max_archive_bytes: 64 * 1024 * 1024,
            max_blocks: 65_536,
            max_compressed_block_bytes: 16 * 1024 * 1024,
            max_plaintext_block_bytes: 32 * 1024 * 1024,
            max_entries_per_directory: 4_096,
            max_path_components: 16,
            max_name_utf16_units: 260,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LegacyRhoArchive {
    path: PathBuf,
    bytes: Box<[u8]>,
    rho_key: u32,
    blocks: HashMap<u32, LegacyBlock>,
    limits: LegacyRhoLimits,
}

#[derive(Debug, Clone, Copy)]
struct LegacyBlock {
    index: u32,
    offset: usize,
    data_size: usize,
    uncompressed_size: usize,
    property: u32,
    checksum: u32,
}

#[derive(Debug)]
struct LegacyDirectory {
    directories: Vec<(String, u32)>,
    files: Vec<LegacyFile>,
}

#[derive(Debug)]
struct LegacyFile {
    name: String,
    full_name: String,
    extension_word: u32,
    block_index: u32,
    plaintext_size: usize,
}

impl LegacyRhoArchive {
    pub fn open(path: impl AsRef<Path>, limits: LegacyRhoLimits) -> Result<Self, LegacyRhoError> {
        validate_limits(limits)?;
        let requested_path = path.as_ref();
        let mut file = File::open(requested_path).map_err(|source| LegacyRhoError::Io {
            operation: "open legacy RHO archive",
            path: requested_path.to_path_buf(),
            source,
        })?;
        let metadata = file.metadata().map_err(|source| LegacyRhoError::Io {
            operation: "inspect legacy RHO archive",
            path: requested_path.to_path_buf(),
            source,
        })?;
        if !metadata.is_file() {
            return Err(LegacyRhoError::NotFile {
                path: requested_path.to_path_buf(),
            });
        }
        let archive_size =
            usize::try_from(metadata.len()).map_err(|_| LegacyRhoError::ArchiveTooLarge {
                actual: usize::MAX,
                maximum: limits.max_archive_bytes,
            })?;
        if archive_size > limits.max_archive_bytes {
            return Err(LegacyRhoError::ArchiveTooLarge {
                actual: archive_size,
                maximum: limits.max_archive_bytes,
            });
        }
        let read_limit = u64::try_from(limits.max_archive_bytes)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let mut bytes = Vec::with_capacity(archive_size);
        (&mut file)
            .take(read_limit)
            .read_to_end(&mut bytes)
            .map_err(|source| LegacyRhoError::Io {
                operation: "read legacy RHO archive",
                path: requested_path.to_path_buf(),
                source,
            })?;
        if bytes.len() > limits.max_archive_bytes {
            return Err(LegacyRhoError::ArchiveTooLarge {
                actual: bytes.len(),
                maximum: limits.max_archive_bytes,
            });
        }
        if bytes.len() != archive_size {
            return Err(LegacyRhoError::ArchiveChanged {
                expected: archive_size,
                actual: bytes.len(),
            });
        }
        let (rho_key, blocks) = parse_archive_index(requested_path, &bytes, limits)?;

        Ok(Self {
            path: requested_path.to_path_buf(),
            bytes: bytes.into_boxed_slice(),
            rho_key,
            blocks,
            limits,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn extract_exact(&self, path: &str) -> Result<Vec<u8>, LegacyRhoError> {
        let components = normalize_path(path, self.limits)?;
        let (file_name, directory_components) = components
            .split_last()
            .ok_or_else(|| LegacyRhoError::InvalidPath(path.to_owned()))?;
        let directory_key = self.rho_key.wrapping_add(0x2593_a9f1);
        let mut directory_index = ROOT_DIRECTORY_INDEX;
        for component in directory_components {
            let bytes = self.read_block(directory_index, directory_key)?;
            let directory = parse_directory(&bytes, self.limits)?;
            directory_index = unique_directory_index(&directory, component)?.ok_or_else(|| {
                LegacyRhoError::EntryNotFound {
                    path: path.to_owned(),
                }
            })?;
        }
        let bytes = self.read_block(directory_index, directory_key)?;
        let directory = parse_directory(&bytes, self.limits)?;
        let file =
            unique_file(&directory, file_name)?.ok_or_else(|| LegacyRhoError::EntryNotFound {
                path: path.to_owned(),
            })?;
        let key = unicode_adler(&file.name)
            .wrapping_add(file.extension_word)
            .wrapping_add(self.rho_key.wrapping_sub(0x756d_e654));
        let plaintext = self.read_block(file.block_index, key)?;
        if plaintext.len() != file.plaintext_size {
            return Err(LegacyRhoError::FileSizeMismatch {
                path: file.full_name.clone(),
                actual: plaintext.len(),
                expected: file.plaintext_size,
            });
        }
        Ok(plaintext)
    }

    fn read_block(&self, index: u32, key: u32) -> Result<Vec<u8>, LegacyRhoError> {
        let block = self
            .blocks
            .get(&index)
            .copied()
            .ok_or(LegacyRhoError::MissingBlock(index))?;
        let end = block
            .offset
            .checked_add(block.data_size)
            .ok_or(LegacyRhoError::ArithmeticOverflow)?;
        let stored = slice(&self.bytes, block.offset, end, "block data")?;
        let mut plaintext = if block.property & 2 != 0 {
            let mut zlib = ZlibDecoder::new(stored);
            let maximum = u64::try_from(block.uncompressed_size)
                .map_err(|_| LegacyRhoError::ArithmeticOverflow)?
                .saturating_add(1);
            let mut decoded_bytes = Vec::with_capacity(block.uncompressed_size);
            (&mut zlib)
                .take(maximum)
                .read_to_end(&mut decoded_bytes)
                .map_err(|source| LegacyRhoError::Decompress { index, source })?;
            if decoded_bytes.len() != block.uncompressed_size {
                return Err(LegacyRhoError::BlockSizeMismatch {
                    index,
                    actual: decoded_bytes.len(),
                    expected: block.uncompressed_size,
                });
            }
            let consumed =
                usize::try_from(zlib.total_in()).map_err(|_| LegacyRhoError::ArithmeticOverflow)?;
            if consumed != stored.len() {
                return Err(LegacyRhoError::TrailingCompressedBytes {
                    index,
                    consumed,
                    stored: stored.len(),
                });
            }
            decoded_bytes
        } else {
            stored.to_vec()
        };
        if block.property & 4 != 0 {
            decrypt_data(&mut plaintext, key);
        }
        if block.property == 4
            && let Some(second) = index
                .checked_add(1)
                .and_then(|second_index| self.blocks.get(&second_index))
                .copied()
        {
            if second.property != 0 {
                return Err(LegacyRhoError::InvalidPartialSecondBlock {
                    index: second.index,
                    property: second.property,
                });
            }
            let second_end = second
                .offset
                .checked_add(second.data_size)
                .ok_or(LegacyRhoError::ArithmeticOverflow)?;
            plaintext.extend_from_slice(slice(
                &self.bytes,
                second.offset,
                second_end,
                "partial-encryption second block",
            )?);
        }
        if plaintext.len() > self.limits.max_plaintext_block_bytes {
            return Err(LegacyRhoError::PlaintextBlockTooLarge {
                index,
                actual: plaintext.len(),
                maximum: self.limits.max_plaintext_block_bytes,
            });
        }
        if block.checksum != 0 {
            let actual = adler32(&plaintext);
            if actual != block.checksum {
                return Err(LegacyRhoError::BlockChecksum {
                    index,
                    actual,
                    expected: block.checksum,
                });
            }
        }
        Ok(plaintext)
    }
}

fn parse_archive_index(
    path: &Path,
    bytes: &[u8],
    limits: LegacyRhoLimits,
) -> Result<(u32, HashMap<u32, LegacyBlock>), LegacyRhoError> {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| LegacyRhoError::InvalidArchiveName {
            path: path.to_path_buf(),
        })?;
    let rho_key = unicode_adler(stem).wrapping_sub(0xa6ee_7565);
    validate_magic(bytes)?;
    let header_end = HEADER_INFO_OFFSET
        .checked_add(HEADER_INFO_LENGTH)
        .ok_or(LegacyRhoError::ArithmeticOverflow)?;
    let encrypted_header = slice(bytes, HEADER_INFO_OFFSET, header_end, "header info")?;
    let header = decrypt_header_info(encrypted_header, rho_key);
    let expected_header_checksum = read_u32(&header, 0, "header checksum")?;
    let actual_header_checksum = adler32(&header[4..]);
    if actual_header_checksum != expected_header_checksum {
        return Err(LegacyRhoError::HeaderChecksum {
            actual: actual_header_checksum,
            expected: expected_header_checksum,
        });
    }
    let version_magic = read_u32(&header, 4, "version magic")?;
    if version_magic != VERSION_MAGIC_11 {
        return Err(LegacyRhoError::UnsupportedVersionMagic(version_magic));
    }
    let block_count_i32 = read_i32(&header, 8, "block count")?;
    let block_count =
        usize::try_from(block_count_i32).map_err(|_| LegacyRhoError::InvalidBlockCount {
            actual: block_count_i32,
            maximum: limits.max_blocks,
        })?;
    if block_count > limits.max_blocks {
        return Err(LegacyRhoError::InvalidBlockCount {
            actual: block_count_i32,
            maximum: limits.max_blocks,
        });
    }
    let whitening_key = read_u32(&header, 12, "block whitening key")?;
    let end_magic = read_u32(&header, 28, "end magic")?;
    if end_magic != END_MAGIC {
        return Err(LegacyRhoError::InvalidEndMagic(end_magic));
    }
    let table_bytes = block_count
        .checked_mul(BLOCK_INFO_LENGTH)
        .ok_or(LegacyRhoError::ArithmeticOverflow)?;
    let table_end = BLOCK_TABLE_OFFSET
        .checked_add(table_bytes)
        .ok_or(LegacyRhoError::ArithmeticOverflow)?;
    slice(bytes, BLOCK_TABLE_OFFSET, table_end, "block table")?;
    let minimum_data_offset = table_end
        .checked_add(0xff)
        .ok_or(LegacyRhoError::ArithmeticOverflow)?
        & !0xff;

    let mut blocks = HashMap::with_capacity(block_count);
    let mut block_key = rho_key ^ whitening_key;
    for ordinal in 0..block_count {
        let start = BLOCK_TABLE_OFFSET + ordinal * BLOCK_INFO_LENGTH;
        let encrypted = &bytes[start..start + BLOCK_INFO_LENGTH];
        let decoded = decrypt_header_info(encrypted, block_key);
        block_key = block_key.wrapping_add(1);
        let block = parse_block(&decoded, bytes.len(), minimum_data_offset, limits)?;
        if blocks.insert(block.index, block).is_some() {
            return Err(LegacyRhoError::DuplicateBlockIndex(block.index));
        }
    }
    validate_block_ranges(&blocks)?;
    Ok((rho_key, blocks))
}

#[derive(Debug, Error)]
pub enum LegacyRhoError {
    #[error("failed to {operation} at {path}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("legacy RHO path is not a regular file: {path}")]
    NotFile { path: PathBuf },
    #[error("legacy RHO archive name is not valid Unicode: {path}")]
    InvalidArchiveName { path: PathBuf },
    #[error("legacy RHO archive has {actual} bytes; maximum is {maximum}")]
    ArchiveTooLarge { actual: usize, maximum: usize },
    #[error(
        "legacy RHO archive changed while it was read: expected {expected} bytes, got {actual}"
    )]
    ArchiveChanged { expected: usize, actual: usize },
    #[error("legacy RHO limits are invalid: {field} must be nonzero")]
    InvalidLimit { field: &'static str },
    #[error("legacy RHO is truncated while reading {context}: need end {end}, length is {actual}")]
    Truncated {
        context: &'static str,
        end: usize,
        actual: usize,
    },
    #[error("legacy RHO supports Rh layer spec 1.1 only; got {actual:?}")]
    UnsupportedMagic { actual: String },
    #[error("legacy RHO header checksum is {actual:#010x}; expected {expected:#010x}")]
    HeaderChecksum { actual: u32, expected: u32 },
    #[error("legacy RHO version magic {0:#010x} is unsupported")]
    UnsupportedVersionMagic(u32),
    #[error("legacy RHO end magic {0:#010x} is invalid")]
    InvalidEndMagic(u32),
    #[error("legacy RHO block count {actual} is outside 0..={maximum}")]
    InvalidBlockCount { actual: i32, maximum: usize },
    #[error("legacy RHO repeats block index {0:#010x}")]
    DuplicateBlockIndex(u32),
    #[error("legacy RHO block {index:#010x} has invalid {field} value {value}")]
    InvalidBlockField {
        index: u32,
        field: &'static str,
        value: i32,
    },
    #[error("legacy RHO block {index:#010x} has unsupported property {property}")]
    InvalidBlockProperty { index: u32, property: u32 },
    #[error("legacy RHO block {index:#010x} ends at {end}, beyond archive length {archive}")]
    BlockOutOfBounds {
        index: u32,
        end: usize,
        archive: usize,
    },
    #[error(
        "legacy RHO block {index:#010x} starts at {offset}, before data region {minimum_offset}"
    )]
    BlockBeforeDataRegion {
        index: u32,
        offset: usize,
        minimum_offset: usize,
    },
    #[error(
        "legacy RHO blocks {first:#010x} and {second:#010x} overlap at archive offset {offset}"
    )]
    OverlappingBlocks {
        first: u32,
        second: u32,
        offset: usize,
    },
    #[error(
        "legacy RHO uncompressed block {index:#010x} declares sizes {data_size} and {uncompressed_size}"
    )]
    InconsistentUncompressedSize {
        index: u32,
        data_size: usize,
        uncompressed_size: usize,
    },
    #[error("legacy RHO block {index:#010x} has {actual} stored bytes; maximum is {maximum}")]
    CompressedBlockTooLarge {
        index: u32,
        actual: usize,
        maximum: usize,
    },
    #[error("legacy RHO block {index:#010x} declares {actual} plain bytes; maximum is {maximum}")]
    PlaintextBlockTooLarge {
        index: u32,
        actual: usize,
        maximum: usize,
    },
    #[error("legacy RHO block {0:#010x} is missing")]
    MissingBlock(u32),
    #[error("failed to decompress legacy RHO block {index:#010x}")]
    Decompress {
        index: u32,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "legacy RHO compressed block {index:#010x} consumed {consumed} of {stored} stored bytes"
    )]
    TrailingCompressedBytes {
        index: u32,
        consumed: usize,
        stored: usize,
    },
    #[error("legacy RHO block {index:#010x} decoded to {actual} bytes; expected {expected}")]
    BlockSizeMismatch {
        index: u32,
        actual: usize,
        expected: usize,
    },
    #[error(
        "legacy RHO partial-encryption second block {index:#010x} has property {property}, expected 0"
    )]
    InvalidPartialSecondBlock { index: u32, property: u32 },
    #[error("legacy RHO block {index:#010x} checksum is {actual:#010x}; expected {expected:#010x}")]
    BlockChecksum {
        index: u32,
        actual: u32,
        expected: u32,
    },
    #[error("legacy RHO directory {kind} count {actual} is outside 0..={maximum}")]
    InvalidDirectoryCount {
        kind: &'static str,
        actual: i32,
        maximum: usize,
    },
    #[error("legacy RHO directory name exceeds {maximum} UTF-16 units or is unterminated")]
    NameTooLong { maximum: usize },
    #[error("legacy RHO directory name contains invalid UTF-16")]
    InvalidUtf16,
    #[error("legacy RHO file extension {0:#010x} is not ASCII")]
    InvalidExtension(u32),
    #[error("legacy RHO directory entry has invalid plaintext size {0}")]
    InvalidFileSize(i32),
    #[error("legacy RHO path is invalid: {0:?}")]
    InvalidPath(String),
    #[error("legacy RHO entry was not found: {path:?}")]
    EntryNotFound { path: String },
    #[error("legacy RHO path component {component:?} is duplicated")]
    DuplicateEntry { component: String },
    #[error("legacy RHO file {path:?} decoded to {actual} bytes; expected {expected}")]
    FileSizeMismatch {
        path: String,
        actual: usize,
        expected: usize,
    },
    #[error("legacy RHO arithmetic overflow")]
    ArithmeticOverflow,
}

fn validate_limits(limits: LegacyRhoLimits) -> Result<(), LegacyRhoError> {
    for (field, value) in [
        ("max_archive_bytes", limits.max_archive_bytes),
        ("max_blocks", limits.max_blocks),
        (
            "max_compressed_block_bytes",
            limits.max_compressed_block_bytes,
        ),
        (
            "max_plaintext_block_bytes",
            limits.max_plaintext_block_bytes,
        ),
        (
            "max_entries_per_directory",
            limits.max_entries_per_directory,
        ),
        ("max_path_components", limits.max_path_components),
        ("max_name_utf16_units", limits.max_name_utf16_units),
    ] {
        if value == 0 {
            return Err(LegacyRhoError::InvalidLimit { field });
        }
    }
    Ok(())
}

fn validate_magic(bytes: &[u8]) -> Result<(), LegacyRhoError> {
    let byte_length = HEADER_MAGIC_11.encode_utf16().count() * 2;
    let encoded = slice(bytes, 0, byte_length, "archive magic")?;
    let units = encoded
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect::<Vec<_>>();
    let actual = String::from_utf16_lossy(&units);
    if actual != HEADER_MAGIC_11 {
        return Err(LegacyRhoError::UnsupportedMagic { actual });
    }
    Ok(())
}

fn parse_block(
    bytes: &[u8],
    archive_length: usize,
    minimum_data_offset: usize,
    limits: LegacyRhoLimits,
) -> Result<LegacyBlock, LegacyRhoError> {
    let index = read_u32(bytes, 0, "block index")?;
    let offset_pages = read_u32(bytes, 4, "block offset")?;
    let offset_u64 = u64::from(offset_pages) << 8;
    let offset = usize::try_from(offset_u64).map_err(|_| LegacyRhoError::ArithmeticOverflow)?;
    let data_size = checked_block_size(bytes, 8, index, "data size")?;
    let uncompressed_size = checked_block_size(bytes, 12, index, "uncompressed size")?;
    let property = read_u32(bytes, 16, "block property")?;
    if !matches!(property, 0 | 2 | 4 | 5 | 7) {
        return Err(LegacyRhoError::InvalidBlockProperty { index, property });
    }
    if offset < minimum_data_offset {
        return Err(LegacyRhoError::BlockBeforeDataRegion {
            index,
            offset,
            minimum_offset: minimum_data_offset,
        });
    }
    if property & 2 == 0 && data_size != uncompressed_size {
        return Err(LegacyRhoError::InconsistentUncompressedSize {
            index,
            data_size,
            uncompressed_size,
        });
    }
    if data_size > limits.max_compressed_block_bytes {
        return Err(LegacyRhoError::CompressedBlockTooLarge {
            index,
            actual: data_size,
            maximum: limits.max_compressed_block_bytes,
        });
    }
    if uncompressed_size > limits.max_plaintext_block_bytes {
        return Err(LegacyRhoError::PlaintextBlockTooLarge {
            index,
            actual: uncompressed_size,
            maximum: limits.max_plaintext_block_bytes,
        });
    }
    let end = offset
        .checked_add(data_size)
        .ok_or(LegacyRhoError::ArithmeticOverflow)?;
    if end > archive_length {
        return Err(LegacyRhoError::BlockOutOfBounds {
            index,
            end,
            archive: archive_length,
        });
    }
    Ok(LegacyBlock {
        index,
        offset,
        data_size,
        uncompressed_size,
        property,
        checksum: read_u32(bytes, 20, "block checksum")?,
    })
}

fn validate_block_ranges(blocks: &HashMap<u32, LegacyBlock>) -> Result<(), LegacyRhoError> {
    let mut ranges = blocks
        .values()
        .filter(|block| block.data_size != 0)
        .map(|block| {
            let end = block
                .offset
                .checked_add(block.data_size)
                .ok_or(LegacyRhoError::ArithmeticOverflow)?;
            Ok((block.offset, end, block.index))
        })
        .collect::<Result<Vec<_>, LegacyRhoError>>()?;
    ranges.sort_unstable_by_key(|(start, _, index)| (*start, *index));
    for pair in ranges.windows(2) {
        let (_, first_end, first_index) = pair[0];
        let (second_start, _, second_index) = pair[1];
        if first_end > second_start {
            return Err(LegacyRhoError::OverlappingBlocks {
                first: first_index,
                second: second_index,
                offset: second_start,
            });
        }
    }
    Ok(())
}

fn checked_block_size(
    bytes: &[u8],
    offset: usize,
    index: u32,
    field: &'static str,
) -> Result<usize, LegacyRhoError> {
    let value = read_i32(bytes, offset, field)?;
    usize::try_from(value).map_err(|_| LegacyRhoError::InvalidBlockField {
        index,
        field,
        value,
    })
}

fn parse_directory(
    bytes: &[u8],
    limits: LegacyRhoLimits,
) -> Result<LegacyDirectory, LegacyRhoError> {
    let mut reader = SliceReader::new(bytes);
    let directory_count = reader.count("subdirectory", limits.max_entries_per_directory)?;
    let mut directories = Vec::with_capacity(directory_count);
    let mut directory_names = HashSet::with_capacity(directory_count);
    for _ in 0..directory_count {
        let name = reader.utf16_z(limits.max_name_utf16_units)?;
        let index = reader.u32("directory block index")?;
        if !directory_names.insert(name.clone()) {
            return Err(LegacyRhoError::DuplicateEntry { component: name });
        }
        directories.push((name, index));
    }
    let file_count = reader.count("file", limits.max_entries_per_directory)?;
    let mut files = Vec::with_capacity(file_count);
    let mut file_names = HashSet::with_capacity(file_count);
    for _ in 0..file_count {
        let name = reader.utf16_z(limits.max_name_utf16_units)?;
        let extension_word = reader.u32("file extension")?;
        let extension = extension(extension_word)?;
        let _file_property = reader.u32("file property")?;
        let block_index = reader.u32("file block index")?;
        let file_size = reader.i32("file size")?;
        let plaintext_size =
            usize::try_from(file_size).map_err(|_| LegacyRhoError::InvalidFileSize(file_size))?;
        if plaintext_size > limits.max_plaintext_block_bytes {
            return Err(LegacyRhoError::PlaintextBlockTooLarge {
                index: block_index,
                actual: plaintext_size,
                maximum: limits.max_plaintext_block_bytes,
            });
        }
        let full_name = if extension.is_empty() {
            name.clone()
        } else {
            format!("{name}.{extension}")
        };
        if !file_names.insert(full_name.clone()) {
            return Err(LegacyRhoError::DuplicateEntry {
                component: full_name,
            });
        }
        files.push(LegacyFile {
            name,
            full_name,
            extension_word,
            block_index,
            plaintext_size,
        });
    }
    Ok(LegacyDirectory { directories, files })
}

fn unique_directory_index(
    directory: &LegacyDirectory,
    name: &str,
) -> Result<Option<u32>, LegacyRhoError> {
    let mut matches = directory
        .directories
        .iter()
        .filter(|(candidate, _)| candidate == name);
    let first = matches.next().map(|(_, index)| *index);
    if matches.next().is_some() {
        return Err(LegacyRhoError::DuplicateEntry {
            component: name.to_owned(),
        });
    }
    Ok(first)
}

fn unique_file<'a>(
    directory: &'a LegacyDirectory,
    name: &str,
) -> Result<Option<&'a LegacyFile>, LegacyRhoError> {
    let mut matches = directory
        .files
        .iter()
        .filter(|candidate| candidate.full_name == name);
    let first = matches.next();
    if matches.next().is_some() {
        return Err(LegacyRhoError::DuplicateEntry {
            component: name.to_owned(),
        });
    }
    Ok(first)
}

fn normalize_path(path: &str, limits: LegacyRhoLimits) -> Result<Vec<String>, LegacyRhoError> {
    let components = path
        .split(['/', '\\'])
        .filter(|component| !component.is_empty() && *component != ".")
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if components.is_empty()
        || components.len() > limits.max_path_components
        || components.iter().any(|component| component == "..")
    {
        return Err(LegacyRhoError::InvalidPath(path.to_owned()));
    }
    Ok(components)
}

fn extension(word: u32) -> Result<String, LegacyRhoError> {
    let bytes = word.to_le_bytes();
    let length = bytes.iter().position(|byte| *byte == 0).unwrap_or(4);
    if bytes[..length].iter().any(|byte| !byte.is_ascii_graphic()) {
        return Err(LegacyRhoError::InvalidExtension(word));
    }
    Ok(String::from_utf8_lossy(&bytes[..length]).into_owned())
}

fn decrypt_header_info(data: &[u8], key: u32) -> Vec<u8> {
    debug_assert_eq!(data.len() % 4, 0);
    let mut current_key = key;
    let mut accumulator = 0_u32;
    let mut output = Vec::with_capacity(data.len());
    for encrypted in data.chunks_exact(4) {
        let encrypted = u32::from_le_bytes(encrypted.try_into().expect("four-byte chunk"));
        let plaintext = encrypted ^ get_vector(current_key) ^ accumulator;
        output.extend_from_slice(&plaintext.to_le_bytes());
        accumulator = accumulator.wrapping_add(plaintext);
        current_key = current_key.wrapping_add(1);
    }
    output
}

fn decrypt_data(data: &mut [u8], key: u32) {
    let mut expanded = [0_u8; 64];
    let mut current = key ^ 0x8473_fbc1;
    for chunk in expanded.chunks_exact_mut(4) {
        chunk.copy_from_slice(&current.to_le_bytes());
        current = current.wrapping_sub(0x7b8c_043f);
    }
    for (index, byte) in data.iter_mut().enumerate() {
        *byte ^= expanded[index & 63];
    }
}

fn unicode_adler(value: &str) -> u32 {
    adler32_iter(value.encode_utf16().flat_map(u16::to_le_bytes))
}

fn adler32(bytes: &[u8]) -> u32 {
    adler32_iter(bytes.iter().copied())
}

fn adler32_iter(bytes: impl IntoIterator<Item = u8>) -> u32 {
    const MODULUS: u32 = 65_521;
    let mut a = 0_u32;
    let mut b = 0_u32;
    for byte in bytes {
        a = (a + u32::from(byte)) % MODULUS;
        b = (b + a) % MODULUS;
    }
    a | (b << 16)
}

fn read_u32(bytes: &[u8], offset: usize, context: &'static str) -> Result<u32, LegacyRhoError> {
    let end = offset
        .checked_add(4)
        .ok_or(LegacyRhoError::ArithmeticOverflow)?;
    let word = slice(bytes, offset, end, context)?;
    Ok(u32::from_le_bytes(
        word.try_into().expect("validated four-byte slice"),
    ))
}

fn read_i32(bytes: &[u8], offset: usize, context: &'static str) -> Result<i32, LegacyRhoError> {
    read_u32(bytes, offset, context).map(|value| i32::from_le_bytes(value.to_le_bytes()))
}

fn slice<'a>(
    bytes: &'a [u8],
    start: usize,
    end: usize,
    context: &'static str,
) -> Result<&'a [u8], LegacyRhoError> {
    bytes.get(start..end).ok_or(LegacyRhoError::Truncated {
        context,
        end,
        actual: bytes.len(),
    })
}

struct SliceReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> SliceReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn u32(&mut self, context: &'static str) -> Result<u32, LegacyRhoError> {
        let value = read_u32(self.bytes, self.position, context)?;
        self.position += 4;
        Ok(value)
    }

    fn i32(&mut self, context: &'static str) -> Result<i32, LegacyRhoError> {
        self.u32(context)
            .map(|value| i32::from_le_bytes(value.to_le_bytes()))
    }

    fn count(&mut self, kind: &'static str, maximum: usize) -> Result<usize, LegacyRhoError> {
        let actual = self.i32("directory entry count")?;
        let count = usize::try_from(actual).map_err(|_| LegacyRhoError::InvalidDirectoryCount {
            kind,
            actual,
            maximum,
        })?;
        if count > maximum {
            return Err(LegacyRhoError::InvalidDirectoryCount {
                kind,
                actual,
                maximum,
            });
        }
        Ok(count)
    }

    fn utf16_z(&mut self, maximum: usize) -> Result<String, LegacyRhoError> {
        let mut units = Vec::new();
        for _ in 0..=maximum {
            let end = self
                .position
                .checked_add(2)
                .ok_or(LegacyRhoError::ArithmeticOverflow)?;
            let pair = slice(self.bytes, self.position, end, "UTF-16 directory name")?;
            self.position = end;
            let unit = u16::from_le_bytes([pair[0], pair[1]]);
            if unit == 0 {
                return String::from_utf16(&units).map_err(|_| LegacyRhoError::InvalidUtf16);
            }
            units.push(unit);
        }
        Err(LegacyRhoError::NameTooLong { maximum })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        LegacyRhoError, LegacyRhoLimits, adler32, decrypt_data, parse_block, parse_directory,
        unicode_adler, validate_block_ranges,
    };

    #[test]
    fn zero_seeded_adler_and_data_cipher_match_known_operations() {
        assert_eq!(adler32(b"GameSlotPacket"), 0x27c0_0574);
        assert_eq!(unicode_adler("item"), 0x086e_01af);
        let mut bytes = b"bounded legacy rho".to_vec();
        let original = bytes.clone();
        decrypt_data(&mut bytes, 0x5136_5136);
        assert_ne!(bytes, original);
        decrypt_data(&mut bytes, 0x5136_5136);
        assert_eq!(bytes, original);
    }

    #[test]
    fn block_metadata_rejects_unsupported_inconsistent_and_overlapping_ranges() {
        let limits = LegacyRhoLimits {
            max_archive_bytes: 4_096,
            max_blocks: 8,
            max_compressed_block_bytes: 512,
            max_plaintext_block_bytes: 1_024,
            max_entries_per_directory: 8,
            max_path_components: 4,
            max_name_utf16_units: 32,
        };
        let mut invalid_property = block_info(1, 1, 32, 32, 3);
        assert!(matches!(
            parse_block(&invalid_property, 4_096, 256, limits),
            Err(LegacyRhoError::InvalidBlockProperty {
                index: 1,
                property: 3
            })
        ));

        invalid_property[16..20].copy_from_slice(&0_u32.to_le_bytes());
        invalid_property[12..16].copy_from_slice(&31_i32.to_le_bytes());
        assert!(matches!(
            parse_block(&invalid_property, 4_096, 256, limits),
            Err(LegacyRhoError::InconsistentUncompressedSize { index: 1, .. })
        ));

        let first = parse_block(&block_info(1, 1, 384, 384, 0), 4_096, 256, limits).unwrap();
        let second = parse_block(&block_info(2, 2, 256, 256, 0), 4_096, 256, limits).unwrap();
        let blocks = HashMap::from([(first.index, first), (second.index, second)]);
        assert!(matches!(
            validate_block_ranges(&blocks),
            Err(LegacyRhoError::OverlappingBlocks {
                first: 1,
                second: 2,
                offset: 512,
            })
        ));
    }

    #[test]
    fn directory_parser_rejects_duplicate_names_and_invalid_utf16() {
        let limits = LegacyRhoLimits::default();
        let mut duplicate = Vec::new();
        duplicate.extend_from_slice(&2_i32.to_le_bytes());
        for index in [1_u32, 2] {
            push_utf16_z(&mut duplicate, "slot");
            duplicate.extend_from_slice(&index.to_le_bytes());
        }
        duplicate.extend_from_slice(&0_i32.to_le_bytes());
        assert!(matches!(
            parse_directory(&duplicate, limits),
            Err(LegacyRhoError::DuplicateEntry { component }) if component == "slot"
        ));

        let mut invalid_utf16 = Vec::new();
        invalid_utf16.extend_from_slice(&1_i32.to_le_bytes());
        invalid_utf16.extend_from_slice(&0xd800_u16.to_le_bytes());
        invalid_utf16.extend_from_slice(&0_u16.to_le_bytes());
        invalid_utf16.extend_from_slice(&1_u32.to_le_bytes());
        invalid_utf16.extend_from_slice(&0_i32.to_le_bytes());
        assert!(matches!(
            parse_directory(&invalid_utf16, limits),
            Err(LegacyRhoError::InvalidUtf16)
        ));
    }

    fn block_info(
        index: u32,
        offset_pages: u32,
        data_size: i32,
        uncompressed_size: i32,
        property: u32,
    ) -> [u8; 32] {
        let mut bytes = [0_u8; 32];
        bytes[0..4].copy_from_slice(&index.to_le_bytes());
        bytes[4..8].copy_from_slice(&offset_pages.to_le_bytes());
        bytes[8..12].copy_from_slice(&data_size.to_le_bytes());
        bytes[12..16].copy_from_slice(&uncompressed_size.to_le_bytes());
        bytes[16..20].copy_from_slice(&property.to_le_bytes());
        bytes
    }

    fn push_utf16_z(bytes: &mut Vec<u8>, value: &str) {
        for unit in value.encode_utf16().chain([0]) {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
    }
}
