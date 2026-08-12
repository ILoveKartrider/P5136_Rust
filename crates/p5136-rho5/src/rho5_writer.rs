use std::{collections::HashSet, io::Write, path::Path};

use flate2::{Compression, write::ZlibEncoder};
use md5::{Digest, Md5};
use thiserror::Error;

use crate::{
    DATA_ALIGNMENT, DOUBLE_ENCRYPTED_PREFIX, P5136_PACKED_ENTRY_FLAGS, RHO5_VERSION, Rho5Error,
    Rho5Limits, Rho5Region, align_up, archive_offsets,
    crypto::{KeyProvider, encrypt_in_place, packed_file_key_with_mixing},
    normalize_path, validate_limits,
};

const METADATA_BYTES: usize = 36;

#[derive(Debug, Clone)]
pub struct Rho5WriteEntry {
    pub path: String,
    pub data: Vec<u8>,
    pub flags: i32,
}

#[derive(Debug, Default)]
pub struct Rho5Writer {
    entries: Vec<Rho5WriteEntry>,
}

#[derive(Debug, Clone)]
pub struct Rho5Encoded {
    bytes: Vec<u8>,
    entries: usize,
}

impl Rho5Encoded {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub const fn entry_count(&self) -> usize {
        self.entries
    }
}

impl Rho5Writer {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn add(&mut self, entry: Rho5WriteEntry) {
        self.entries.push(entry);
    }

    #[allow(clippy::too_many_lines)]
    pub fn encode(
        &self,
        archive_name: &str,
        region: Rho5Region,
        limits: &Rho5Limits,
    ) -> Result<Rho5Encoded, Rho5WriteError> {
        validate_limits(limits)?;
        validate_archive_name(archive_name, limits)?;
        if self.entries.is_empty() {
            return Err(Rho5WriteError::EmptyArchive);
        }
        if self.entries.len() > limits.max_files_per_archive {
            return Err(Rho5WriteError::TooManyEntries {
                actual: self.entries.len(),
                maximum: limits.max_files_per_archive,
            });
        }

        let mut seen = HashSet::with_capacity(self.entries.len());
        let mut prepared = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            let path = normalize_path(&entry.path, limits)?;
            if entry.flags != P5136_PACKED_ENTRY_FLAGS {
                return Err(Rho5WriteError::UnsupportedFlags {
                    path,
                    actual: entry.flags,
                    expected: P5136_PACKED_ENTRY_FLAGS,
                });
            }
            let units = path.encode_utf16().count();
            if !seen.insert(path.to_ascii_lowercase()) {
                return Err(Rho5WriteError::DuplicatePath(path));
            }
            if entry.data.len() > limits.max_plaintext_bytes {
                return Err(Rho5WriteError::PlaintextTooLarge {
                    path,
                    actual: entry.data.len(),
                    maximum: limits.max_plaintext_bytes,
                });
            }
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(&entry.data)?;
            let compressed = encoder.finish()?;
            if compressed.len() > limits.max_compressed_bytes {
                return Err(Rho5WriteError::CompressedTooLarge {
                    path,
                    actual: compressed.len(),
                    maximum: limits.max_compressed_bytes,
                });
            }
            prepared.push(PreparedEntry {
                path,
                path_units: units,
                plaintext_size: entry.data.len(),
                plaintext_md5: Md5::digest(&entry.data).into(),
                flags: entry.flags,
                compressed,
                block_offset: 0,
                physical_offset: 0,
            });
        }
        prepared.sort_unstable_by(|left, right| {
            left.path
                .to_ascii_lowercase()
                .cmp(&right.path.to_ascii_lowercase())
                .then_with(|| left.path.cmp(&right.path))
        });

        let offsets = archive_offsets(archive_name)?;
        let table_size = prepared.iter().try_fold(0_u64, |total, entry| {
            let path_bytes = u64::try_from(entry.path_units)
                .map_err(|_| Rho5WriteError::ArithmeticOverflow)?
                .checked_mul(2)
                .ok_or(Rho5WriteError::ArithmeticOverflow)?;
            total
                .checked_add(4)
                .and_then(|value| value.checked_add(path_bytes))
                .and_then(|value| value.checked_add(METADATA_BYTES as u64))
                .ok_or(Rho5WriteError::ArithmeticOverflow)
        })?;
        if table_size > limits.max_table_bytes {
            return Err(Rho5WriteError::TableTooLarge {
                actual: table_size,
                maximum: limits.max_table_bytes,
            });
        }
        let table_end = offsets
            .table
            .checked_add(table_size)
            .ok_or(Rho5WriteError::ArithmeticOverflow)?;
        let data_base = align_up(table_end, DATA_ALIGNMENT)?;
        let mut cursor = data_base;
        for entry in &mut prepared {
            cursor = align_up(cursor, DATA_ALIGNMENT)?;
            entry.physical_offset = cursor;
            entry.block_offset = cursor
                .checked_sub(data_base)
                .ok_or(Rho5WriteError::ArithmeticOverflow)?
                / DATA_ALIGNMENT;
            cursor = cursor
                .checked_add(
                    u64::try_from(entry.compressed.len())
                        .map_err(|_| Rho5WriteError::ArithmeticOverflow)?,
                )
                .ok_or(Rho5WriteError::ArithmeticOverflow)?;
        }
        if cursor > limits.max_archive_bytes {
            return Err(Rho5WriteError::ArchiveTooLarge {
                actual: cursor,
                maximum: limits.max_archive_bytes,
            });
        }
        let archive_length =
            usize::try_from(cursor).map_err(|_| Rho5WriteError::ArithmeticOverflow)?;
        let mut output = vec![0_u8; archive_length];

        let file_count =
            i32::try_from(prepared.len()).map_err(|_| Rho5WriteError::ArithmeticOverflow)?;
        let mut header = Vec::with_capacity(12);
        header.extend_from_slice(&(i32::from(RHO5_VERSION).wrapping_add(file_count)).to_le_bytes());
        header.push(RHO5_VERSION);
        header.extend_from_slice(&file_count.to_le_bytes());
        encrypt_stream(
            &mut header,
            KeyProvider::for_header_with_mixing(archive_name, region.mixing_string()),
        );
        copy_at(&mut output, offsets.header, &header)?;

        let mut table = Vec::with_capacity(
            usize::try_from(table_size).map_err(|_| Rho5WriteError::ArithmeticOverflow)?,
        );
        for entry in &prepared {
            let path_units = entry.path.encode_utf16().collect::<Vec<_>>();
            table.extend_from_slice(
                &i32::try_from(path_units.len())
                    .map_err(|_| Rho5WriteError::ArithmeticOverflow)?
                    .to_le_bytes(),
            );
            for unit in path_units {
                table.extend_from_slice(&unit.to_le_bytes());
            }
            let unknown = entry.flags;
            let block_offset = i32::try_from(entry.block_offset)
                .map_err(|_| Rho5WriteError::ArithmeticOverflow)?;
            let plaintext_size = i32::try_from(entry.plaintext_size)
                .map_err(|_| Rho5WriteError::ArithmeticOverflow)?;
            let compressed_size = i32::try_from(entry.compressed.len())
                .map_err(|_| Rho5WriteError::ArithmeticOverflow)?;
            let checksum = entry.plaintext_md5.iter().fold(
                unknown
                    .wrapping_add(block_offset)
                    .wrapping_add(plaintext_size)
                    .wrapping_add(compressed_size),
                |value, byte| value.wrapping_add(i32::from(*byte)),
            );
            table.extend_from_slice(&checksum.to_le_bytes());
            table.extend_from_slice(&unknown.to_le_bytes());
            table.extend_from_slice(&block_offset.to_le_bytes());
            table.extend_from_slice(&plaintext_size.to_le_bytes());
            table.extend_from_slice(&compressed_size.to_le_bytes());
            table.extend_from_slice(&entry.plaintext_md5);
        }
        debug_assert_eq!(table.len() as u64, table_size);
        encrypt_stream(
            &mut table,
            KeyProvider::for_table_with_mixing(archive_name, region.mixing_string()),
        );
        copy_at(&mut output, offsets.table, &table)?;

        for entry in &prepared {
            let mut encrypted = entry.compressed.clone();
            let key = packed_file_key_with_mixing(
                &entry.plaintext_md5,
                &entry.path,
                region.mixing_string(),
            );
            encrypt_in_place(&mut encrypted, &key);
            let prefix = encrypted.len().min(DOUBLE_ENCRYPTED_PREFIX);
            encrypt_in_place(&mut encrypted[..prefix], &key);
            copy_at(&mut output, entry.physical_offset, &encrypted)?;
        }

        Ok(Rho5Encoded {
            bytes: output,
            entries: prepared.len(),
        })
    }
}

struct PreparedEntry {
    path: String,
    path_units: usize,
    plaintext_size: usize,
    plaintext_md5: [u8; 16],
    flags: i32,
    compressed: Vec<u8>,
    block_offset: u64,
    physical_offset: u64,
}

fn validate_archive_name(name: &str, limits: &Rho5Limits) -> Result<(), Rho5WriteError> {
    if Path::new(name).file_name().and_then(|value| value.to_str()) != Some(name)
        || !name.is_ascii()
        || !name.to_ascii_lowercase().ends_with(".rho5")
    {
        return Err(Rho5WriteError::InvalidArchiveName(name.to_owned()));
    }
    if name.len() > limits.max_archive_name_bytes {
        return Err(Rho5WriteError::ArchiveNameTooLong {
            actual: name.len(),
            maximum: limits.max_archive_name_bytes,
        });
    }
    Ok(())
}

fn encrypt_stream(bytes: &mut Vec<u8>, mut provider: KeyProvider) {
    let original_length = bytes.len();
    bytes.resize(original_length.next_multiple_of(4), 0);
    for chunk in bytes.chunks_exact_mut(4) {
        let encrypted = u32::from_le_bytes(chunk.try_into().expect("four-byte chunk"))
            .wrapping_add(provider.next_word())
            .to_le_bytes();
        chunk.copy_from_slice(&encrypted);
    }
}

fn copy_at(output: &mut [u8], offset: u64, bytes: &[u8]) -> Result<(), Rho5WriteError> {
    let start = usize::try_from(offset).map_err(|_| Rho5WriteError::ArithmeticOverflow)?;
    let end = start
        .checked_add(bytes.len())
        .ok_or(Rho5WriteError::ArithmeticOverflow)?;
    let destination = output
        .get_mut(start..end)
        .ok_or(Rho5WriteError::ArithmeticOverflow)?;
    destination.copy_from_slice(bytes);
    Ok(())
}

#[derive(Debug, Error)]
pub enum Rho5WriteError {
    #[error(transparent)]
    Read(#[from] Rho5Error),
    #[error("RHO5 output archive is empty")]
    EmptyArchive,
    #[error("invalid RHO5 archive name {0:?}")]
    InvalidArchiveName(String),
    #[error("RHO5 archive name has {actual} bytes; maximum is {maximum}")]
    ArchiveNameTooLong { actual: usize, maximum: usize },
    #[error("RHO5 writer has {actual} entries; maximum is {maximum}")]
    TooManyEntries { actual: usize, maximum: usize },
    #[error("RHO5 writer repeats path {0:?}")]
    DuplicatePath(String),
    #[error("RHO5 plaintext {path:?} has {actual} bytes; maximum is {maximum}")]
    PlaintextTooLarge {
        path: String,
        actual: usize,
        maximum: usize,
    },
    #[error("RHO5 compressed {path:?} has {actual} bytes; maximum is {maximum}")]
    CompressedTooLarge {
        path: String,
        actual: usize,
        maximum: usize,
    },
    #[error("RHO5 table has {actual} bytes; maximum is {maximum}")]
    TableTooLarge { actual: u64, maximum: u64 },
    #[error("RHO5 archive has {actual} bytes; maximum is {maximum}")]
    ArchiveTooLarge { actual: u64, maximum: u64 },
    #[error("RHO5 compression failed")]
    Compression(#[from] std::io::Error),
    #[error("RHO5 writer arithmetic overflow")]
    ArithmeticOverflow,
    #[error(
        "RHO5 output entry {path:?} has flags {actual:#010x}; the P5136 packed codec requires {expected:#010x}"
    )]
    UnsupportedFlags {
        path: String,
        actual: i32,
        expected: i32,
    },
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{Rho5WriteEntry, Rho5WriteError, Rho5Writer};
    use crate::{P5136_PACKED_ENTRY_FLAGS, Rho5Directory, Rho5Limits, Rho5Region};

    #[test]
    fn deterministic_archive_round_trips_through_stock_reader() {
        let mut writer = Rho5Writer::new();
        writer.add(Rho5WriteEntry {
            path: "kart_/fixture/param.xml".to_owned(),
            data: b"<BodyParam MaxSpeed='301'/>".to_vec(),
            flags: P5136_PACKED_ENTRY_FLAGS,
        });
        writer.add(Rho5WriteEntry {
            path: "character/fixture/model.bin".to_owned(),
            data: (0_u8..=255).cycle().take(3_000).collect(),
            flags: P5136_PACKED_ENTRY_FLAGS,
        });
        let limits = Rho5Limits::default();
        let first = writer
            .encode("DataPack4_00005.rho5", Rho5Region::Korea, &limits)
            .unwrap();
        let second = writer
            .encode("DataPack4_00005.rho5", Rho5Region::Korea, &limits)
            .unwrap();
        assert_eq!(first.as_bytes(), second.as_bytes());

        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("DataPack4_00005.rho5"),
            first.as_bytes(),
        )
        .unwrap();
        let decoded = Rho5Directory::scan_kr(directory.path(), limits).unwrap();
        assert_eq!(decoded.entries().len(), 2);
        assert_eq!(decoded.entries()[0].flags(), P5136_PACKED_ENTRY_FLAGS);
        assert_eq!(decoded.entries()[1].flags(), P5136_PACKED_ENTRY_FLAGS);
        assert_eq!(
            decoded.extract_exact("kart_/fixture/param.xml").unwrap(),
            b"<BodyParam MaxSpeed='301'/>"
        );
        assert_eq!(
            decoded
                .extract_exact("character/fixture/model.bin")
                .unwrap(),
            (0_u8..=255).cycle().take(3_000).collect::<Vec<_>>()
        );
    }

    #[test]
    fn rejects_flags_that_make_the_native_client_expose_ciphertext() {
        let mut writer = Rho5Writer::new();
        writer.add(Rho5WriteEntry {
            path: "kart_/fixture/model.1s".to_owned(),
            data: b"fixture".to_vec(),
            flags: 0,
        });
        assert!(matches!(
            writer.encode(
                "DataPack1_00002.rho5",
                Rho5Region::Korea,
                &Rho5Limits::default()
            ),
            Err(Rho5WriteError::UnsupportedFlags {
                actual: 0,
                expected: P5136_PACKED_ENTRY_FLAGS,
                ..
            })
        ));
    }
}
