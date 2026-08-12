use std::path::Path;
use thiserror::Error;

use crate::{
    file_safety::{
        ConnectorFileError, LEGACY_BACKUP_SUFFIX, PRISTINE_BACKUP_SUFFIX, append_suffix,
        read_bounded,
    },
    limits::CodecLimits,
    pin::{P5136_MINOR_VERSION, decode_shallow_pin_header_with_limits},
};

pub const P5136_LOCALE_ID: u16 = 1002;
pub const P5136_CLIENT_LOCATION: u16 = 118;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinDetectionSource {
    Live,
    PristineBackup,
    LegacyBackup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildEvidence {
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

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{BuildEvidence, PinDetectionSource, detect_p5136};
    use crate::{
        encoded_block,
        file_safety::{PRISTINE_BACKUP_SUFFIX, append_suffix},
        limits::CodecLimits,
        pin::PinDocument,
        test_fixture::csharp_synthetic_pin,
    };

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
