//! Deterministic writer for `Rh layer spec 1.1` archives.

use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use flate2::{Compression, write::ZlibEncoder};
use thiserror::Error;

use crate::{
    legacy::{
        LegacyRhoArchive, LegacyRhoError, LegacyRhoFileProperty, LegacyRhoLimits, adler32,
        decrypt_data, unicode_adler,
    },
    legacy_vectors::get_vector,
};

const HEADER_MAGIC: &str = "Rh layer spec 1.1";
const SECOND_TEXT: &str = "KartRider (veblush & dew)";
const VERSION_MAGIC: u32 = 0x0001_0001;
const END_MAGIC: u32 = 0xfc1f_9778;
const WHITENING_KEY: u32 = 0x3a92_13ac;
const ROOT_INDEX: u32 = u32::MAX;
const DIRECTORY_COLLISION_STEP: u32 = 0x5f03_e367;
const FILE_COLLISION_STEP: u32 = 0x4d21_cb4f;

/// One plaintext file to store in a legacy RHO archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyRhoWriteEntry {
    pub path: String,
    pub data: Vec<u8>,
    pub property: LegacyRhoFileProperty,
}

/// In-memory archive builder. Encoding is deterministic for a fixed output stem.
#[derive(Debug, Clone, Default)]
pub struct LegacyRhoWriter {
    entries: Vec<LegacyRhoWriteEntry>,
}

/// Encoded archive and the two values referenced by its `aaa.pk` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyRhoEncoded {
    bytes: Vec<u8>,
    rho_key: u32,
    data_hash: u32,
}

impl LegacyRhoEncoded {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    #[must_use]
    pub const fn rho_key(&self) -> u32 {
        self.rho_key
    }

    #[must_use]
    pub const fn data_hash(&self) -> u32 {
        self.data_hash
    }
}

impl LegacyRhoWriter {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn add(&mut self, entry: LegacyRhoWriteEntry) {
        self.entries.push(entry);
    }

    /// Materializes an existing archive while retaining file storage properties.
    pub fn from_archive(archive: &LegacyRhoArchive) -> Result<Self, LegacyRhoWriteError> {
        let mut writer = Self::new();
        for entry in archive.entries()? {
            writer.add(LegacyRhoWriteEntry {
                path: entry.normalized_path().to_owned(),
                data: archive.extract_entry(&entry)?,
                property: entry.file_property(),
            });
        }
        Ok(writer)
    }

    #[must_use]
    pub fn entries(&self) -> &[LegacyRhoWriteEntry] {
        &self.entries
    }

    /// Encodes an archive whose key is derived from `archive_stem`.
    pub fn encode(
        &self,
        archive_stem: &str,
        limits: LegacyRhoLimits,
    ) -> Result<Vec<u8>, LegacyRhoWriteError> {
        Ok(self
            .encode_with_metadata(archive_stem, limits)?
            .into_bytes())
    }

    /// Encodes an archive and exposes the `aaa.pk` key/hash metadata.
    pub fn encode_with_metadata(
        &self,
        archive_stem: &str,
        limits: LegacyRhoLimits,
    ) -> Result<LegacyRhoEncoded, LegacyRhoWriteError> {
        validate_writer_limits(limits)?;
        if archive_stem.is_empty() {
            return Err(LegacyRhoWriteError::InvalidArchiveStem);
        }
        let mut root = WriteDirectory::default();
        for entry in &self.entries {
            insert_entry(&mut root, entry, limits)?;
        }

        let rho_key = unicode_adler(archive_stem).wrapping_sub(0xa6ee_7565);
        let mut used = HashSet::new();
        assign_directory_indices(&mut root, "", &mut used)?;
        assign_file_indices(&mut root, &mut used)?;

        let mut blocks = Vec::new();
        emit_directory(&root, rho_key, limits, &mut blocks)?;
        if blocks.len() > limits.max_blocks {
            return Err(LegacyRhoWriteError::TooManyBlocks {
                actual: blocks.len(),
                maximum: limits.max_blocks,
            });
        }
        encode_archive(rho_key, blocks, limits)
    }

    /// Encodes and writes an archive. The destination file name determines its key.
    pub fn write_to(
        &self,
        path: impl AsRef<Path>,
        limits: LegacyRhoLimits,
    ) -> Result<(), LegacyRhoWriteError> {
        let path = path.as_ref();
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| LegacyRhoWriteError::InvalidArchivePath(path.to_path_buf()))?;
        let bytes = self.encode(stem, limits)?;
        fs::write(path, bytes).map_err(|source| LegacyRhoWriteError::Io {
            operation: "write legacy RHO archive",
            path: path.to_path_buf(),
            source,
        })
    }
}

#[derive(Debug, Error)]
pub enum LegacyRhoWriteError {
    #[error("legacy RHO output path has no valid Unicode file stem: {0}")]
    InvalidArchivePath(PathBuf),
    #[error("legacy RHO archive stem must not be empty")]
    InvalidArchiveStem,
    #[error("legacy RHO path is invalid: {0:?}")]
    InvalidPath(String),
    #[error("legacy RHO path component {component:?} exceeds {maximum} UTF-16 units")]
    NameTooLong { component: String, maximum: usize },
    #[error("legacy RHO extension {0:?} must be at most four printable ASCII bytes")]
    InvalidExtension(String),
    #[error("legacy RHO contains duplicate file path {0:?}")]
    DuplicatePath(String),
    #[error("legacy RHO path uses {0:?} as both a file and a directory")]
    FileDirectoryConflict(String),
    #[error("legacy RHO directory {path:?} has {actual} entries; maximum is {maximum}")]
    TooManyDirectoryEntries {
        path: String,
        actual: usize,
        maximum: usize,
    },
    #[error("legacy RHO file {path:?} has {actual} bytes; maximum is {maximum}")]
    PlaintextTooLarge {
        path: String,
        actual: usize,
        maximum: usize,
    },
    #[error("legacy RHO block has {actual} stored bytes; maximum is {maximum}")]
    StoredBlockTooLarge { actual: usize, maximum: usize },
    #[error("legacy RHO requires {actual} blocks; maximum is {maximum}")]
    TooManyBlocks { actual: usize, maximum: usize },
    #[error("legacy RHO output has {actual} bytes; maximum is {maximum}")]
    ArchiveTooLarge { actual: usize, maximum: usize },
    #[error("legacy RHO writer limit {0} must be nonzero")]
    InvalidLimit(&'static str),
    #[error("legacy RHO index space is exhausted")]
    IndexSpaceExhausted,
    #[error("legacy RHO integer conversion or offset calculation overflowed")]
    ArithmeticOverflow,
    #[error("failed to {operation} at {path}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Read(#[from] LegacyRhoError),
}

#[derive(Debug, Clone)]
struct WriteFile {
    stem: String,
    extension: String,
    extension_word: u32,
    data: Vec<u8>,
    property: LegacyRhoFileProperty,
    block_index: u32,
}

#[derive(Debug, Clone, Default)]
struct WriteDirectory {
    index: u32,
    directories: HashMap<String, WriteDirectory>,
    files: Vec<WriteFile>,
}

#[derive(Debug)]
struct StoredBlock {
    index: u32,
    data: Vec<u8>,
    plaintext_size: usize,
    property: u32,
    checksum: u32,
    relative_offset: usize,
}

fn validate_writer_limits(limits: LegacyRhoLimits) -> Result<(), LegacyRhoWriteError> {
    for (name, value) in [
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
            return Err(LegacyRhoWriteError::InvalidLimit(name));
        }
    }
    Ok(())
}

fn insert_entry(
    root: &mut WriteDirectory,
    entry: &LegacyRhoWriteEntry,
    limits: LegacyRhoLimits,
) -> Result<(), LegacyRhoWriteError> {
    let components = entry
        .path
        .split(['/', '\\'])
        .filter(|part| !part.is_empty() && *part != ".")
        .collect::<Vec<_>>();
    if components.is_empty()
        || components.len() > limits.max_path_components
        || components.contains(&"..")
    {
        return Err(LegacyRhoWriteError::InvalidPath(entry.path.clone()));
    }
    for component in &components {
        let length = component.encode_utf16().count();
        if length == 0 || length > limits.max_name_utf16_units || component.contains('\0') {
            return Err(LegacyRhoWriteError::NameTooLong {
                component: (*component).to_owned(),
                maximum: limits.max_name_utf16_units,
            });
        }
    }
    if entry.data.len() > limits.max_plaintext_block_bytes {
        return Err(LegacyRhoWriteError::PlaintextTooLarge {
            path: entry.path.clone(),
            actual: entry.data.len(),
            maximum: limits.max_plaintext_block_bytes,
        });
    }

    let (file_name, directory_parts) = components
        .split_last()
        .ok_or_else(|| LegacyRhoWriteError::InvalidPath(entry.path.clone()))?;
    let mut directory = root;
    let mut walked = Vec::new();
    for component in directory_parts {
        walked.push(*component);
        if directory
            .files
            .iter()
            .any(|file| compose_file_name(&file.stem, &file.extension) == *component)
        {
            return Err(LegacyRhoWriteError::FileDirectoryConflict(walked.join("/")));
        }
        let entry_count = directory
            .directories
            .len()
            .checked_add(directory.files.len())
            .ok_or(LegacyRhoWriteError::ArithmeticOverflow)?;
        if !directory.directories.contains_key(*component)
            && entry_count >= limits.max_entries_per_directory
        {
            return Err(LegacyRhoWriteError::TooManyDirectoryEntries {
                path: walked.join("/"),
                actual: entry_count + 1,
                maximum: limits.max_entries_per_directory,
            });
        }
        directory = directory
            .directories
            .entry((*component).to_owned())
            .or_default();
    }
    if directory.directories.contains_key(*file_name) {
        return Err(LegacyRhoWriteError::FileDirectoryConflict(
            entry.path.clone(),
        ));
    }
    let (stem, extension) = split_file_name(file_name)?;
    if directory
        .files
        .iter()
        .any(|file| file.stem == stem && file.extension == extension)
    {
        return Err(LegacyRhoWriteError::DuplicatePath(entry.path.clone()));
    }
    let entry_count = directory
        .directories
        .len()
        .checked_add(directory.files.len())
        .ok_or(LegacyRhoWriteError::ArithmeticOverflow)?;
    if entry_count >= limits.max_entries_per_directory {
        return Err(LegacyRhoWriteError::TooManyDirectoryEntries {
            path: directory_parts.join("/"),
            actual: entry_count + 1,
            maximum: limits.max_entries_per_directory,
        });
    }
    directory.files.push(WriteFile {
        stem,
        extension_word: extension_word(&extension)?,
        extension,
        data: entry.data.clone(),
        property: entry.property,
        block_index: 0,
    });
    Ok(())
}

fn split_file_name(name: &str) -> Result<(String, String), LegacyRhoWriteError> {
    let (stem, extension) = name.rsplit_once('.').map_or((name, ""), |parts| parts);
    if stem.is_empty() || (name.contains('.') && extension.is_empty()) {
        return Err(LegacyRhoWriteError::InvalidPath(name.to_owned()));
    }
    extension_word(extension)?;
    Ok((stem.to_owned(), extension.to_owned()))
}

fn extension_word(extension: &str) -> Result<u32, LegacyRhoWriteError> {
    let bytes = extension.as_bytes();
    if bytes.len() > 4 || bytes.iter().any(|byte| !byte.is_ascii_graphic()) {
        return Err(LegacyRhoWriteError::InvalidExtension(extension.to_owned()));
    }
    let mut word = [0_u8; 4];
    word[..bytes.len()].copy_from_slice(bytes);
    Ok(u32::from_le_bytes(word))
}

fn assign_directory_indices(
    directory: &mut WriteDirectory,
    full_path: &str,
    used: &mut HashSet<u32>,
) -> Result<(), LegacyRhoWriteError> {
    directory.index = if full_path.is_empty() {
        used.insert(ROOT_INDEX);
        ROOT_INDEX
    } else {
        reserve_index(
            unicode_adler(full_path),
            false,
            used,
            DIRECTORY_COLLISION_STEP,
        )?
    };
    let mut names = directory.directories.keys().cloned().collect::<Vec<_>>();
    names.sort_by(|left, right| utf16_cmp(left, right));
    for name in names {
        let child_path = if full_path.is_empty() {
            name.clone()
        } else {
            format!("{full_path}/{name}")
        };
        let child = directory
            .directories
            .get_mut(&name)
            .expect("name came from this directory");
        assign_directory_indices(child, &child_path, used)?;
    }
    Ok(())
}

fn assign_file_indices(
    directory: &mut WriteDirectory,
    used: &mut HashSet<u32>,
) -> Result<(), LegacyRhoWriteError> {
    let folder_index = if directory.index == ROOT_INDEX {
        0
    } else {
        directory.index
    };
    for file in &mut directory.files {
        let raw = unicode_adler(&file.stem)
            .wrapping_add(file.extension_word)
            .wrapping_add(folder_index);
        file.block_index = reserve_index(raw, true, used, FILE_COLLISION_STEP)?;
        if file.property == LegacyRhoFileProperty::PartialEncrypted && file.data.len() > 0x100 {
            used.insert(file.block_index.wrapping_add(1));
        }
    }
    let mut names = directory.directories.keys().cloned().collect::<Vec<_>>();
    names.sort_by(|left, right| utf16_cmp(left, right));
    for name in names {
        assign_file_indices(
            directory
                .directories
                .get_mut(&name)
                .expect("name came from this directory"),
            used,
        )?;
    }
    Ok(())
}

fn reserve_index(
    mut candidate: u32,
    require_next_free: bool,
    used: &mut HashSet<u32>,
    step: u32,
) -> Result<u32, LegacyRhoWriteError> {
    for _ in 0..=used.len() {
        if !used.contains(&candidate)
            && (!require_next_free || !used.contains(&candidate.wrapping_add(1)))
        {
            used.insert(candidate);
            return Ok(candidate);
        }
        candidate = candidate.wrapping_add(step);
    }
    Err(LegacyRhoWriteError::IndexSpaceExhausted)
}

fn emit_directory(
    directory: &WriteDirectory,
    rho_key: u32,
    limits: LegacyRhoLimits,
    blocks: &mut Vec<StoredBlock>,
) -> Result<(), LegacyRhoWriteError> {
    let mut files = directory.files.iter().collect::<Vec<_>>();
    files.sort_by(|left, right| {
        utf16_cmp(&left.stem, &right.stem)
            .then_with(|| utf16_cmp(&left.extension, &right.extension))
    });
    for file in &files {
        emit_file(file, rho_key, limits, blocks)?;
    }

    let mut directories = directory.directories.iter().collect::<Vec<_>>();
    directories.sort_by(|(left, _), (right, _)| utf16_cmp(left, right));
    for (_, child) in &directories {
        emit_directory(child, rho_key, limits, blocks)?;
    }

    let mut metadata = Vec::new();
    push_i32(&mut metadata, directories.len())?;
    for (name, child) in directories {
        push_utf16_z(&mut metadata, name);
        metadata.extend_from_slice(&child.index.to_le_bytes());
    }
    push_i32(&mut metadata, files.len())?;
    for file in files {
        push_utf16_z(&mut metadata, &file.stem);
        metadata.extend_from_slice(&file.extension_word.to_le_bytes());
        metadata.extend_from_slice(&(file.property as u32).to_le_bytes());
        metadata.extend_from_slice(&file.block_index.to_le_bytes());
        push_i32(&mut metadata, file.data.len())?;
    }
    check_stored_size(metadata.len(), limits)?;
    let checksum = adler32(&metadata);
    decrypt_data(&mut metadata, rho_key.wrapping_add(0x2593_a9f1));
    blocks.push(StoredBlock {
        index: directory.index,
        plaintext_size: metadata.len(),
        data: metadata,
        property: 5,
        checksum,
        relative_offset: 0,
    });
    Ok(())
}

fn emit_file(
    file: &WriteFile,
    rho_key: u32,
    limits: LegacyRhoLimits,
    blocks: &mut Vec<StoredBlock>,
) -> Result<(), LegacyRhoWriteError> {
    let key = unicode_adler(&file.stem)
        .wrapping_add(file.extension_word)
        .wrapping_add(rho_key.wrapping_sub(0x756d_e654));
    let checksum = match file.property {
        LegacyRhoFileProperty::Encrypted | LegacyRhoFileProperty::CompressedEncrypted => {
            adler32(&file.data)
        }
        _ => 0,
    };
    let mut stored = file.data.clone();
    match file.property {
        LegacyRhoFileProperty::Encrypted | LegacyRhoFileProperty::CompressedEncrypted => {
            decrypt_data(&mut stored, key);
        }
        LegacyRhoFileProperty::PartialEncrypted => {
            let encrypted_length = stored.len().min(0x100);
            decrypt_data(&mut stored[..encrypted_length], key);
        }
        LegacyRhoFileProperty::None | LegacyRhoFileProperty::Compressed => {}
    }
    if matches!(
        file.property,
        LegacyRhoFileProperty::Compressed | LegacyRhoFileProperty::CompressedEncrypted
    ) {
        stored = compress(&stored)?;
    }
    check_stored_size(stored.len(), limits)?;

    if file.property == LegacyRhoFileProperty::PartialEncrypted {
        let first_length = stored.len().min(0x100);
        blocks.push(StoredBlock {
            index: file.block_index,
            data: stored[..first_length].to_vec(),
            plaintext_size: first_length,
            property: 4,
            checksum: 0,
            relative_offset: 0,
        });
        if stored.len() > first_length {
            blocks.push(StoredBlock {
                index: file.block_index.wrapping_add(1),
                data: stored[first_length..].to_vec(),
                plaintext_size: stored.len() - first_length,
                property: 0,
                checksum: 0,
                relative_offset: 0,
            });
        }
    } else {
        let property = match file.property {
            LegacyRhoFileProperty::None => 0,
            LegacyRhoFileProperty::Compressed => 2,
            LegacyRhoFileProperty::Encrypted => 5,
            LegacyRhoFileProperty::CompressedEncrypted => 7,
            LegacyRhoFileProperty::PartialEncrypted => unreachable!(),
        };
        blocks.push(StoredBlock {
            index: file.block_index,
            data: stored,
            plaintext_size: file.data.len(),
            property,
            checksum,
            relative_offset: 0,
        });
    }
    Ok(())
}

fn encode_archive(
    rho_key: u32,
    mut blocks: Vec<StoredBlock>,
    limits: LegacyRhoLimits,
) -> Result<LegacyRhoEncoded, LegacyRhoWriteError> {
    let table_length = align(
        blocks
            .len()
            .checked_mul(0x20)
            .ok_or(LegacyRhoWriteError::ArithmeticOverflow)?,
    )?;
    let data_begin = 0x100_usize
        .checked_add(table_length)
        .ok_or(LegacyRhoWriteError::ArithmeticOverflow)?;
    let mut relative = 0_usize;
    for block in &mut blocks {
        block.relative_offset = relative;
        relative = align(
            relative
                .checked_add(block.data.len())
                .ok_or(LegacyRhoWriteError::ArithmeticOverflow)?,
        )?;
    }
    let data_length = blocks
        .last()
        .map(|block| {
            block
                .relative_offset
                .checked_add(block.data.len())
                .ok_or(LegacyRhoWriteError::ArithmeticOverflow)
        })
        .transpose()?
        .unwrap_or(0);
    let total_length = data_begin
        .checked_add(data_length)
        .ok_or(LegacyRhoWriteError::ArithmeticOverflow)?;
    if total_length > limits.max_archive_bytes {
        return Err(LegacyRhoWriteError::ArchiveTooLarge {
            actual: total_length,
            maximum: limits.max_archive_bytes,
        });
    }
    let mut output = vec![0_u8; total_length];
    write_utf16_at(&mut output, 0, HEADER_MAGIC);
    write_utf16_at(&mut output, 0x40, SECOND_TEXT);

    let data_hash = adler32_iter(blocks.iter().flat_map(|block| block.data.iter().copied()));
    let mut header = [0_u8; 0x80];
    header[4..8].copy_from_slice(&VERSION_MAGIC.to_le_bytes());
    header[8..12].copy_from_slice(&to_i32(blocks.len())?.to_le_bytes());
    header[12..16].copy_from_slice(&WHITENING_KEY.to_le_bytes());
    header[16..20].copy_from_slice(&1_u32.to_le_bytes());
    header[20..24].copy_from_slice(&rho_key.wrapping_sub(0x397e_40c3).to_le_bytes());
    header[24..28].copy_from_slice(&data_hash.to_le_bytes());
    header[28..32].copy_from_slice(&END_MAGIC.to_le_bytes());
    header[32..36].copy_from_slice(&0x7e_u32.to_le_bytes());
    let header_checksum = adler32(&header[4..]);
    header[0..4].copy_from_slice(&header_checksum.to_le_bytes());
    output[0x80..0x100].copy_from_slice(&encrypt_header_info(&header, rho_key));

    let mut table_key = rho_key ^ WHITENING_KEY;
    for (ordinal, block) in blocks.iter().enumerate() {
        let mut record = [0_u8; 0x20];
        record[0..4].copy_from_slice(&block.index.to_le_bytes());
        let physical = data_begin
            .checked_add(block.relative_offset)
            .ok_or(LegacyRhoWriteError::ArithmeticOverflow)?;
        let page =
            u32::try_from(physical >> 8).map_err(|_| LegacyRhoWriteError::ArithmeticOverflow)?;
        record[4..8].copy_from_slice(&page.to_le_bytes());
        record[8..12].copy_from_slice(&to_i32(block.data.len())?.to_le_bytes());
        record[12..16].copy_from_slice(&to_i32(block.plaintext_size)?.to_le_bytes());
        record[16..20].copy_from_slice(&block.property.to_le_bytes());
        record[20..24].copy_from_slice(&block.checksum.to_le_bytes());
        let start = 0x100 + ordinal * 0x20;
        output[start..start + 0x20].copy_from_slice(&encrypt_header_info(&record, table_key));
        table_key = table_key.wrapping_add(1);
        let data_start = data_begin + block.relative_offset;
        output[data_start..data_start + block.data.len()].copy_from_slice(&block.data);
    }
    Ok(LegacyRhoEncoded {
        bytes: output,
        rho_key,
        data_hash,
    })
}

fn encrypt_header_info(data: &[u8], key: u32) -> Vec<u8> {
    debug_assert_eq!(data.len() % 4, 0);
    let mut current_key = key;
    let mut accumulator = 0_u32;
    let mut output = Vec::with_capacity(data.len());
    for plaintext in data.chunks_exact(4) {
        let plaintext = u32::from_le_bytes(plaintext.try_into().expect("four-byte chunk"));
        let encrypted = plaintext ^ get_vector(current_key) ^ accumulator;
        output.extend_from_slice(&encrypted.to_le_bytes());
        accumulator = accumulator.wrapping_add(plaintext);
        current_key = current_key.wrapping_add(1);
    }
    output
}

fn compress(data: &[u8]) -> Result<Vec<u8>, LegacyRhoWriteError> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
    encoder
        .write_all(data)
        .map_err(|source| LegacyRhoWriteError::Io {
            operation: "compress legacy RHO block",
            path: PathBuf::from("<memory>"),
            source,
        })?;
    encoder.finish().map_err(|source| LegacyRhoWriteError::Io {
        operation: "finish legacy RHO block compression",
        path: PathBuf::from("<memory>"),
        source,
    })
}

fn check_stored_size(size: usize, limits: LegacyRhoLimits) -> Result<(), LegacyRhoWriteError> {
    if size > limits.max_compressed_block_bytes {
        return Err(LegacyRhoWriteError::StoredBlockTooLarge {
            actual: size,
            maximum: limits.max_compressed_block_bytes,
        });
    }
    Ok(())
}

fn push_i32(output: &mut Vec<u8>, value: usize) -> Result<(), LegacyRhoWriteError> {
    output.extend_from_slice(&to_i32(value)?.to_le_bytes());
    Ok(())
}

fn push_utf16_z(output: &mut Vec<u8>, value: &str) {
    for unit in value.encode_utf16().chain([0]) {
        output.extend_from_slice(&unit.to_le_bytes());
    }
}

fn write_utf16_at(output: &mut [u8], offset: usize, value: &str) {
    for (index, unit) in value.encode_utf16().enumerate() {
        let start = offset + index * 2;
        output[start..start + 2].copy_from_slice(&unit.to_le_bytes());
    }
}

fn to_i32(value: usize) -> Result<i32, LegacyRhoWriteError> {
    i32::try_from(value).map_err(|_| LegacyRhoWriteError::ArithmeticOverflow)
}

fn align(value: usize) -> Result<usize, LegacyRhoWriteError> {
    value
        .checked_add(0xff)
        .map(|value| value & !0xff)
        .ok_or(LegacyRhoWriteError::ArithmeticOverflow)
}

fn utf16_cmp(left: &str, right: &str) -> Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

fn compose_file_name(stem: &str, extension: &str) -> String {
    if extension.is_empty() {
        stem.to_owned()
    } else {
        format!("{stem}.{extension}")
    }
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

#[cfg(test)]
mod tests {
    use std::{env, fs, path::PathBuf};

    use tempfile::tempdir;

    use super::{LegacyRhoWriteEntry, LegacyRhoWriter};
    use crate::{LegacyRhoArchive, LegacyRhoFileProperty, LegacyRhoLimits};

    #[test]
    fn all_storage_properties_round_trip_through_reader() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("roundtrip.rho");
        let cases = [
            ("plain/readme.txt", LegacyRhoFileProperty::None, 31),
            ("models/body.1s", LegacyRhoFileProperty::Compressed, 2_049),
            ("config/value.xml", LegacyRhoFileProperty::Encrypted, 257),
            (
                "images/preview.png",
                LegacyRhoFileProperty::PartialEncrypted,
                513,
            ),
            (
                "spec/kart.bml",
                LegacyRhoFileProperty::CompressedEncrypted,
                4_113,
            ),
        ];
        let mut writer = LegacyRhoWriter::new();
        for (name, property, length) in cases {
            let data = (0..length)
                .map(|index| u8::try_from((index * 97 + 13) % 251).unwrap())
                .collect();
            writer.add(LegacyRhoWriteEntry {
                path: name.to_owned(),
                data,
                property,
            });
        }
        let limits = LegacyRhoLimits::default();
        writer.write_to(&path, limits).unwrap();
        let archive = LegacyRhoArchive::open(&path, limits).unwrap();
        let entries = archive.entries().unwrap();
        assert_eq!(entries.len(), cases.len());
        for (name, property, length) in cases {
            let entry = entries
                .iter()
                .find(|entry| entry.normalized_path() == name)
                .unwrap();
            assert_eq!(entry.file_property(), property);
            let expected = (0..length)
                .map(|index| u8::try_from((index * 97 + 13) % 251).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(archive.extract_entry(entry).unwrap(), expected);
        }
    }

    #[test]
    fn encoding_is_deterministic() {
        let mut writer = LegacyRhoWriter::new();
        writer.add(LegacyRhoWriteEntry {
            path: "kart/test.bml".to_owned(),
            data: b"same input, same archive".to_vec(),
            property: LegacyRhoFileProperty::CompressedEncrypted,
        });
        let encoded = writer
            .encode_with_metadata("stable", LegacyRhoLimits::default())
            .unwrap();
        let first = encoded.as_bytes().to_vec();
        let second = writer.encode("stable", LegacyRhoLimits::default()).unwrap();
        assert_eq!(first, second);

        let directory = tempdir().unwrap();
        let path = directory.path().join("stable.rho");
        fs::write(&path, first).unwrap();
        let archive = LegacyRhoArchive::open(path, LegacyRhoLimits::default()).unwrap();
        assert_eq!(archive.rho_key(), encoded.rho_key());
        assert_eq!(archive.data_hash(), encoded.data_hash());
        assert_eq!(
            archive.extract_exact("kart/test.bml").unwrap(),
            b"same input, same archive"
        );
    }

    #[test]
    #[ignore = "requires a local proprietary P5136 installation via P5136_DATA_DIR"]
    fn local_stock_legacy_archive_semantically_repackages() {
        let data = PathBuf::from(env::var_os("P5136_DATA_DIR").expect("P5136_DATA_DIR is set"));
        let source_path = data.join("character_.rho");
        let limits = LegacyRhoLimits {
            max_archive_bytes: 512 * 1024 * 1024,
            max_compressed_block_bytes: 128 * 1024 * 1024,
            max_plaintext_block_bytes: 256 * 1024 * 1024,
            ..LegacyRhoLimits::default()
        };
        let source = LegacyRhoArchive::open(&source_path, limits).unwrap();
        let writer = LegacyRhoWriter::from_archive(&source).unwrap();
        let directory = tempdir().unwrap();
        let output_path = directory.path().join("character_.rho");
        writer.write_to(&output_path, limits).unwrap();
        let output = LegacyRhoArchive::open(output_path, limits).unwrap();
        let source_entries = source.entries().unwrap();
        let output_entries = output.entries().unwrap();
        assert_eq!(source_entries, output_entries);
        for entry in &source_entries {
            assert_eq!(
                source.extract_entry(entry).unwrap(),
                output.extract_exact(entry.normalized_path()).unwrap()
            );
        }
    }
}
