use std::{fs::File, io::Read, path::Path};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    file_safety::{
        ConnectorFileError, LEGACY_BACKUP_SUFFIX, PRISTINE_BACKUP_SUFFIX, append_suffix,
        read_bounded,
    },
    limits::CodecLimits,
    pin::{P5136_MINOR_VERSION, decode_shallow_pin_header_with_limits},
};

pub const P5136_EXECUTABLE_SHA256: &str =
    "629F084E2A12C6FA1FF0EA603B90F8768454D13A1BC2DF6A8504F8AA06FD6194";
pub const P5136_LOCALE_ID: u16 = 1002;
pub const P5136_CLIENT_LOCATION: u16 = 118;

pub(crate) const P5136_EXECUTABLE_SHA256_BYTES: [u8; 32] = [
    0x62, 0x9F, 0x08, 0x4E, 0x2A, 0x12, 0xC6, 0xFA, 0x1F, 0xF0, 0xEA, 0x60, 0x3B, 0x90, 0xF8, 0x76,
    0x84, 0x54, 0xD1, 0x3A, 0x1B, 0xC2, 0xDF, 0x6A, 0x85, 0x04, 0xF8, 0xAA, 0x06, 0xFD, 0x61, 0x94,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinDetectionSource {
    Live,
    PristineBackup,
    LegacyBackup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildEvidence {
    ExecutableSha256,
    PinHeader(PinDetectionSource),
}

#[derive(Debug, Error)]
pub enum BuildDetectionError {
    #[error("failed to inspect connector installation")]
    File(#[from] ConnectorFileError),
}

pub fn detect_p5136(
    game_directory: &Path,
    limits: &CodecLimits,
) -> Result<Option<BuildEvidence>, BuildDetectionError> {
    let executable_path = game_directory.join("KartRider.exe");
    if executable_path.is_file() && hash_file(&executable_path)? == P5136_EXECUTABLE_SHA256_BYTES {
        return Ok(Some(BuildEvidence::ExecutableSha256));
    }

    let pin_path = game_directory.join("KartRider.pin");
    let candidates = [
        (PinDetectionSource::Live, pin_path.clone()),
        (
            PinDetectionSource::PristineBackup,
            append_suffix(&pin_path, PRISTINE_BACKUP_SUFFIX),
        ),
        (
            PinDetectionSource::LegacyBackup,
            append_suffix(&pin_path, LEGACY_BACKUP_SUFFIX),
        ),
    ];
    for (source, path) in candidates {
        if !path.is_file() {
            continue;
        }
        let bytes = read_bounded(&path, limits.max_pin_file_bytes)?;
        let Ok(header) = decode_shallow_pin_header_with_limits(&bytes, limits) else {
            continue;
        };
        if header.locale_id == P5136_LOCALE_ID
            && header.client_location == P5136_CLIENT_LOCATION
            && header.minor_version == P5136_MINOR_VERSION
        {
            return Ok(Some(BuildEvidence::PinHeader(source)));
        }
    }
    Ok(None)
}

fn hash_file(path: &Path) -> Result<[u8; 32], ConnectorFileError> {
    let mut file = File::open(path).map_err(|source| ConnectorFileError::Io {
        operation: "open executable for hashing",
        path: path.to_owned(),
        source,
    })?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|source| ConnectorFileError::Io {
                operation: "hash executable",
                path: path.to_owned(),
                source,
            })?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(digest.finalize().into())
}

#[cfg(test)]
mod tests {
    use std::{fmt::Write as _, fs};

    use tempfile::tempdir;

    use super::{
        BuildEvidence, P5136_EXECUTABLE_SHA256, P5136_EXECUTABLE_SHA256_BYTES, PinDetectionSource,
        detect_p5136,
    };
    use crate::{
        encoded_block,
        file_safety::{PRISTINE_BACKUP_SUFFIX, append_suffix},
        limits::CodecLimits,
        pin::PinDocument,
        test_fixture::csharp_synthetic_pin,
    };

    #[test]
    fn published_executable_digest_constant_has_the_expected_bytes() {
        let mut encoded = String::with_capacity(64);
        for byte in P5136_EXECUTABLE_SHA256_BYTES {
            write!(&mut encoded, "{byte:02X}").unwrap();
        }
        assert_eq!(P5136_EXECUTABLE_SHA256, encoded);
    }

    #[test]
    fn falls_back_to_the_decoded_live_pin_header() {
        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("KartRider.exe"),
            b"not the real client",
        )
        .unwrap();
        fs::write(
            directory.path().join("KartRider.pin"),
            csharp_synthetic_pin(),
        )
        .unwrap();

        assert_eq!(
            detect_p5136(directory.path(), &CodecLimits::default()).unwrap(),
            Some(BuildEvidence::PinHeader(PinDetectionSource::Live))
        );
    }

    #[test]
    fn shallow_header_fallback_does_not_require_the_remaining_pin_body() {
        let directory = tempdir().unwrap();
        let fixture = csharp_synthetic_pin();
        let encoded_length =
            usize::try_from(i32::from_le_bytes(fixture[..4].try_into().unwrap())).unwrap();
        let decoded =
            encoded_block::decode(&fixture[4..4 + encoded_length], &CodecLimits::default())
                .unwrap();
        let shallow_payload = &decoded.bytes[..13];
        let shallow_encoded =
            encoded_block::encode(shallow_payload, decoded.encoding, &CodecLimits::default())
                .unwrap();
        let mut shallow_pin = Vec::with_capacity(4 + shallow_encoded.len());
        shallow_pin.extend_from_slice(&i32::try_from(shallow_encoded.len()).unwrap().to_le_bytes());
        shallow_pin.extend_from_slice(&shallow_encoded);
        assert!(PinDocument::decode(&shallow_pin).is_err());
        fs::write(directory.path().join("KartRider.pin"), shallow_pin).unwrap();

        assert_eq!(
            detect_p5136(directory.path(), &CodecLimits::default()).unwrap(),
            Some(BuildEvidence::PinHeader(PinDetectionSource::Live))
        );
    }

    #[test]
    fn can_identify_a_missing_live_pin_from_its_pristine_backup() {
        let directory = tempdir().unwrap();
        let pin_path = directory.path().join("KartRider.pin");
        fs::write(
            append_suffix(&pin_path, PRISTINE_BACKUP_SUFFIX),
            csharp_synthetic_pin(),
        )
        .unwrap();

        assert_eq!(
            detect_p5136(directory.path(), &CodecLimits::default()).unwrap(),
            Some(BuildEvidence::PinHeader(PinDetectionSource::PristineBackup))
        );
    }

    #[test]
    fn malformed_or_wrong_build_inputs_are_not_accepted() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("KartRider.exe"), b"wrong").unwrap();
        fs::write(directory.path().join("KartRider.pin"), b"\x04\0\0\0junk").unwrap();
        assert_eq!(
            detect_p5136(directory.path(), &CodecLimits::default()).unwrap(),
            None
        );
    }
}
