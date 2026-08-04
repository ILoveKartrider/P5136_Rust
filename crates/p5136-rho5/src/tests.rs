use std::{fs, io::Write, path::Path};

use flate2::{Compression, write::ZlibEncoder};
use md5::{Digest, Md5};
use tempfile::TempDir;

use super::{
    DATA_ALIGNMENT, DOUBLE_ENCRYPTED_PREFIX, RHO5_VERSION, Rho5Directory, Rho5Error, Rho5Limits,
    align_up, archive_offsets,
    crypto::{KeyProvider, packed_file_key},
};

#[derive(Clone)]
struct EntrySpec {
    path_units: Vec<u16>,
    key_path: String,
    plaintext: Vec<u8>,
    declared_plaintext_size: Option<i32>,
    declared_compressed_size: Option<i32>,
    md5: Option<[u8; 16]>,
    compressed_suffix: Vec<u8>,
    checksum_adjustment: i32,
    block_offset: Option<i32>,
}

impl EntrySpec {
    fn new(path: &str, plaintext: impl Into<Vec<u8>>) -> Self {
        Self {
            path_units: path.encode_utf16().collect(),
            key_path: path.to_owned(),
            plaintext: plaintext.into(),
            declared_plaintext_size: None,
            declared_compressed_size: None,
            md5: None,
            compressed_suffix: Vec::new(),
            checksum_adjustment: 0,
            block_offset: None,
        }
    }
}

struct PreparedEntry {
    spec: EntrySpec,
    compressed: Vec<u8>,
    checksum: [u8; 16],
    block_offset: i32,
}

#[test]
fn p5136_archive_offsets_match_golden() {
    assert_eq!(
        archive_offsets("DataPack1_00000.rho5").expect("valid archive name"),
        super::Rho5Offsets {
            header: 107,
            table: 184,
        }
    );
}

#[test]
fn scans_and_extracts_utf16_path_with_fresh_two_layer_decryption() {
    let temp = TempDir::new().expect("temporary directory");
    let mut randomish = Vec::with_capacity(16 * 1024);
    let mut state = 0x1234_5678_u32;
    for _ in 0..16 * 1024 {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        randomish.push((state >> 24) as u8);
    }
    let path = "etc_/한글/café.xml";
    write_archive(
        temp.path(),
        "DataPack1_00000.rho5",
        &[EntrySpec::new(path, randomish.clone())],
    );

    let directory =
        Rho5Directory::scan_kr(temp.path(), Rho5Limits::default()).expect("scan archive");
    let entry = directory
        .unique_entry("/etc_//한글/cafe\u{301}.xml")
        .expect("NFC-normalized exact match");
    assert!(
        entry.compressed_size() > DOUBLE_ENCRYPTED_PREFIX,
        "fixture must exercise encrypted prefix and encrypted tail"
    );
    assert_eq!(
        directory
            .extract_exact("\\etc_\\한글\\cafe\u{301}.xml")
            .expect("authenticated extraction"),
        randomish
    );
}

#[test]
fn exact_lookup_distinguishes_missing_and_duplicate_entries() {
    let temp = TempDir::new().expect("temporary directory");
    let path = "etc_/same.xml";
    write_archive(
        temp.path(),
        "DataPack1_00000.rho5",
        &[EntrySpec::new(path, b"first".to_vec())],
    );
    write_archive(
        temp.path(),
        "DataPack2_00000.rho5",
        &[EntrySpec::new(path, b"second".to_vec())],
    );
    let directory =
        Rho5Directory::scan_kr(temp.path(), Rho5Limits::default()).expect("scan archives");

    assert!(matches!(
        directory.unique_entry("etc_/missing.xml"),
        Err(Rho5Error::EntryNotFound { .. })
    ));
    assert!(matches!(
        directory.unique_entry(path),
        Err(Rho5Error::DuplicateEntry { count: 2, .. })
    ));
}

#[test]
fn rejects_file_count_before_table_allocation() {
    let temp = TempDir::new().expect("temporary directory");
    write_header_only(temp.path(), "DataPack1_00000.rho5", 2, 10);
    let limits = Rho5Limits {
        max_files_per_archive: 4,
        ..Rho5Limits::default()
    };
    assert!(matches!(
        Rho5Directory::scan_kr(temp.path(), limits),
        Err(Rho5Error::TooManyFiles {
            count: 10,
            limit: 4,
            ..
        })
    ));
}

#[test]
fn rejects_oversized_path_before_allocating_it() {
    let temp = TempDir::new().expect("temporary directory");
    write_archive(
        temp.path(),
        "DataPack1_00000.rho5",
        &[EntrySpec::new(&"x".repeat(32), Vec::new())],
    );
    let limits = Rho5Limits {
        max_path_utf16_units: 8,
        ..Rho5Limits::default()
    };
    assert!(matches!(
        Rho5Directory::scan_kr(temp.path(), limits),
        Err(Rho5Error::PathTooLong {
            units: 32,
            limit: 8,
            ..
        })
    ));
}

#[test]
fn rejects_unpaired_utf16_path() {
    let temp = TempDir::new().expect("temporary directory");
    let mut spec = EntrySpec::new("unused", Vec::new());
    spec.path_units = vec![0xd800];
    spec.key_path = "unused".to_owned();
    write_archive(temp.path(), "DataPack1_00000.rho5", &[spec]);
    assert!(matches!(
        Rho5Directory::scan_kr(temp.path(), Rho5Limits::default()),
        Err(Rho5Error::InvalidUtf16Path { entry_index: 0, .. })
    ));
}

#[test]
fn rejects_table_checksum_mismatch() {
    let temp = TempDir::new().expect("temporary directory");
    let mut spec = EntrySpec::new("etc_/bad.xml", b"data".to_vec());
    spec.checksum_adjustment = 1;
    write_archive(temp.path(), "DataPack1_00000.rho5", &[spec]);
    assert!(matches!(
        Rho5Directory::scan_kr(temp.path(), Rho5Limits::default()),
        Err(Rho5Error::EntryChecksumMismatch { .. })
    ));
}

#[test]
fn rejects_entry_range_outside_archive() {
    let temp = TempDir::new().expect("temporary directory");
    let mut spec = EntrySpec::new("etc_/outside.xml", b"data".to_vec());
    spec.declared_compressed_size = Some(1_000_000);
    write_archive(temp.path(), "DataPack1_00000.rho5", &[spec]);
    assert!(matches!(
        Rho5Directory::scan_kr(temp.path(), Rho5Limits::default()),
        Err(Rho5Error::EntryOutOfBounds { .. })
    ));
}

#[test]
fn rejects_trailing_zlib_bytes() {
    let temp = TempDir::new().expect("temporary directory");
    let mut spec = EntrySpec::new("etc_/trailing.xml", b"payload".to_vec());
    spec.compressed_suffix = vec![0xaa, 0xbb, 0xcc];
    write_archive(temp.path(), "DataPack1_00000.rho5", &[spec]);
    let directory =
        Rho5Directory::scan_kr(temp.path(), Rho5Limits::default()).expect("scan archive");
    assert!(matches!(
        directory.extract_exact("etc_/trailing.xml"),
        Err(Rho5Error::TrailingCompressedData { .. })
    ));
    assert_eq!(
        directory
            .extract_entry_with_legacy_padding(&directory.entries()[0])
            .expect("explicit legacy-packer compatibility"),
        b"payload"
    );
}

#[test]
fn enforces_exact_decompressed_length_and_hard_cap() {
    let too_short = TempDir::new().expect("temporary directory");
    let mut spec = EntrySpec::new("etc_/short.xml", b"payload".to_vec());
    spec.declared_plaintext_size = Some(8);
    write_archive(too_short.path(), "DataPack1_00000.rho5", &[spec]);
    let directory =
        Rho5Directory::scan_kr(too_short.path(), Rho5Limits::default()).expect("scan archive");
    assert!(matches!(
        directory.extract_exact("etc_/short.xml"),
        Err(Rho5Error::PlaintextSizeMismatch {
            actual: 7,
            expected: 8,
            ..
        })
    ));

    let too_long = TempDir::new().expect("temporary directory");
    let mut spec = EntrySpec::new("etc_/long.xml", b"payload".to_vec());
    spec.declared_plaintext_size = Some(6);
    write_archive(too_long.path(), "DataPack1_00000.rho5", &[spec]);
    let directory =
        Rho5Directory::scan_kr(too_long.path(), Rho5Limits::default()).expect("scan archive");
    assert!(matches!(
        directory.extract_exact("etc_/long.xml"),
        Err(Rho5Error::DecompressedSizeLimitExceeded { expected: 6, .. })
    ));
}

#[test]
fn verifies_plaintext_md5_after_decompression() {
    let temp = TempDir::new().expect("temporary directory");
    let mut spec = EntrySpec::new("etc_/md5.xml", b"payload".to_vec());
    spec.md5 = Some([0x5a; 16]);
    write_archive(temp.path(), "DataPack1_00000.rho5", &[spec]);
    let directory =
        Rho5Directory::scan_kr(temp.path(), Rho5Limits::default()).expect("scan archive");
    assert!(matches!(
        directory.extract_exact("etc_/md5.xml"),
        Err(Rho5Error::PlaintextMd5Mismatch { .. })
    ));
}

#[test]
fn rejects_parent_path_normalization() {
    let temp = TempDir::new().expect("temporary directory");
    write_archive(
        temp.path(),
        "DataPack1_00000.rho5",
        &[EntrySpec::new("../escape.xml", Vec::new())],
    );
    assert!(matches!(
        Rho5Directory::scan_kr(temp.path(), Rho5Limits::default()),
        Err(Rho5Error::InvalidNormalizedPath { .. })
    ));
}

#[test]
#[ignore = "requires a local proprietary P5136 installation via P5136_DATA_DIR"]
fn local_kr_emblem_fixture_matches_recorded_metadata() {
    let Some(directory) = std::env::var_os("P5136_DATA_DIR") else {
        return;
    };
    let index =
        Rho5Directory::scan_kr(directory, Rho5Limits::default()).expect("scan local P5136 data");
    let entry = index
        .unique_entry("etc_/emblem/emblem@kr.xml")
        .expect("unique KR emblem catalog");
    assert_eq!(entry.archive_name(), "DataPack1_00000.rho5");
    assert_eq!(entry.physical_data_offset(), 5_766_144);
    assert_eq!(entry.compressed_size(), 19_632);
    assert_eq!(entry.plaintext_size(), 116_076);
    assert_eq!(
        entry.plaintext_md5(),
        [
            0x4e, 0xb1, 0xba, 0xe1, 0x05, 0x9a, 0x03, 0xba, 0xfc, 0xe8, 0x04, 0xe0, 0xe8, 0xf3,
            0x92, 0x77,
        ]
    );
    let plaintext = index
        .extract_exact("etc_/emblem/emblem@kr.xml")
        .expect("extract authenticated KR emblem catalog");
    assert_eq!(plaintext.len(), 116_076);
}

fn write_header_only(directory: &Path, name: &str, version: u8, count: i32) {
    let offsets = archive_offsets(name).expect("valid synthetic archive name");
    let mut archive = vec![0_u8; usize_from_u64(offsets.table + 4)];
    let checksum = i32::from(version).wrapping_add(count);
    let mut header = Vec::new();
    header.extend_from_slice(&checksum.to_le_bytes());
    header.push(version);
    header.extend_from_slice(&count.to_le_bytes());
    encrypt_with_provider(&mut header, KeyProvider::for_header(name));
    let header_offset = usize_from_u64(offsets.header);
    archive[header_offset..header_offset + header.len()].copy_from_slice(&header);
    fs::write(directory.join(name), archive).expect("write synthetic header");
}

fn write_archive(directory: &Path, name: &str, specs: &[EntrySpec]) {
    let offsets = archive_offsets(name).expect("valid synthetic archive name");
    let mut next_block = 0_u64;
    let mut prepared = Vec::with_capacity(specs.len());
    for spec in specs {
        let checksum: [u8; 16] = spec
            .md5
            .unwrap_or_else(|| Md5::digest(&spec.plaintext).into());
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(&spec.plaintext)
            .expect("compress synthetic plaintext");
        let mut compressed = encoder.finish().expect("finish synthetic zlib stream");
        compressed.extend_from_slice(&spec.compressed_suffix);
        let block_offset = spec
            .block_offset
            .unwrap_or_else(|| i32::try_from(next_block).expect("synthetic block offset"));
        next_block = align_up(
            next_block * DATA_ALIGNMENT + compressed.len() as u64,
            DATA_ALIGNMENT,
        )
        .expect("align synthetic data")
            / DATA_ALIGNMENT;
        prepared.push(PreparedEntry {
            spec: spec.clone(),
            compressed,
            checksum,
            block_offset,
        });
    }

    let mut table = Vec::new();
    for entry in &prepared {
        table.extend_from_slice(
            &i32::try_from(entry.spec.path_units.len())
                .expect("synthetic path length")
                .to_le_bytes(),
        );
        for unit in &entry.spec.path_units {
            table.extend_from_slice(&unit.to_le_bytes());
        }
        let plaintext_size = entry.spec.declared_plaintext_size.unwrap_or_else(|| {
            i32::try_from(entry.spec.plaintext.len()).expect("synthetic plaintext size")
        });
        let compressed_size = entry.spec.declared_compressed_size.unwrap_or_else(|| {
            i32::try_from(entry.compressed.len()).expect("synthetic compressed size")
        });
        let unknown = 7_i32;
        let table_checksum = entry
            .checksum
            .iter()
            .fold(
                unknown
                    .wrapping_add(entry.block_offset)
                    .wrapping_add(plaintext_size)
                    .wrapping_add(compressed_size),
                |sum, byte| sum.wrapping_add(i32::from(*byte)),
            )
            .wrapping_add(entry.spec.checksum_adjustment);
        table.extend_from_slice(&table_checksum.to_le_bytes());
        table.extend_from_slice(&unknown.to_le_bytes());
        table.extend_from_slice(&entry.block_offset.to_le_bytes());
        table.extend_from_slice(&plaintext_size.to_le_bytes());
        table.extend_from_slice(&compressed_size.to_le_bytes());
        table.extend_from_slice(&entry.checksum);
    }

    let data_base =
        align_up(offsets.table + table.len() as u64, DATA_ALIGNMENT).expect("data base");
    let mut archive_length = data_base;
    for entry in &prepared {
        let offset = data_base + u64_from_i32(entry.block_offset) * DATA_ALIGNMENT;
        archive_length = archive_length.max(offset + entry.compressed.len() as u64);
    }
    let mut archive = vec![0_u8; usize_from_u64(archive_length)];

    let count = i32::try_from(prepared.len()).expect("synthetic file count");
    let mut header = Vec::new();
    header.extend_from_slice(&i32::from(RHO5_VERSION).wrapping_add(count).to_le_bytes());
    header.push(RHO5_VERSION);
    header.extend_from_slice(&count.to_le_bytes());
    encrypt_with_provider(&mut header, KeyProvider::for_header(name));
    let header_offset = usize_from_u64(offsets.header);
    archive[header_offset..header_offset + header.len()].copy_from_slice(&header);

    encrypt_with_provider(&mut table, KeyProvider::for_table(name));
    let table_offset = usize_from_u64(offsets.table);
    archive[table_offset..table_offset + table.len()].copy_from_slice(&table);

    for mut entry in prepared {
        let key = packed_file_key(&entry.checksum, &entry.spec.key_path);
        let prefix = entry.compressed.len().min(DOUBLE_ENCRYPTED_PREFIX);
        super::crypto::encrypt_in_place(&mut entry.compressed[..prefix], &key);
        super::crypto::encrypt_in_place(&mut entry.compressed, &key);
        let physical_offset = data_base + u64_from_i32(entry.block_offset) * DATA_ALIGNMENT;
        let offset = usize_from_u64(physical_offset);
        archive[offset..offset + entry.compressed.len()].copy_from_slice(&entry.compressed);
    }
    fs::write(directory.join(name), archive).expect("write synthetic RHO5 archive");
}

fn encrypt_with_provider(data: &mut [u8], mut provider: KeyProvider) {
    for chunk in data.chunks_mut(4) {
        let mut bytes = [0_u8; 4];
        bytes[..chunk.len()].copy_from_slice(chunk);
        let encrypted = u32::from_le_bytes(bytes)
            .wrapping_add(provider.next_word())
            .to_le_bytes();
        chunk.copy_from_slice(&encrypted[..chunk.len()]);
    }
}

fn usize_from_u64(value: u64) -> usize {
    usize::try_from(value).expect("synthetic archive fits in memory")
}

fn u64_from_i32(value: i32) -> u64 {
    u64::try_from(value).expect("synthetic block offset is nonnegative")
}
