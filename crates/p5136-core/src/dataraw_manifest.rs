//! Content-free `DataRaw` compatibility manifest and private preflight wire format.
//!
//! Only normalized relative file names participate in the digest. File bytes,
//! lengths, and timestamps are deliberately excluded so the preflight remains
//! cheap even for a large unpacked client tree.

use std::{
    fmt::Write as _,
    fs, io,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};
use thiserror::Error;

pub const DATARAW_PREFLIGHT_VERSION: u16 = 1;
pub const DATARAW_PREFLIGHT_REQUEST_MAGIC: [u8; 4] = *b"P5DR";
pub const DATARAW_PREFLIGHT_RESPONSE_MAGIC: [u8; 4] = *b"P5DS";
pub const DATARAW_PREFLIGHT_FRAME_LENGTH: usize = 44;
pub const DATARAW_PREFLIGHT_ENABLED: u16 = 1;
pub const DATARAW_MAX_FILES: usize = 250_000;
const DATARAW_MAX_RELATIVE_PATH_BYTES: usize = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DataRawManifest {
    pub file_count: u32,
    pub list_digest: [u8; 32],
}

impl DataRawManifest {
    pub fn scan(root: &Path) -> Result<Self, DataRawManifestError> {
        if !root.is_dir() {
            return Err(DataRawManifestError::MissingRoot(root.to_path_buf()));
        }
        let mut pending = vec![root.to_path_buf()];
        let mut files = Vec::new();
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(&directory).map_err(|source| DataRawManifestError::Io {
                path: directory.clone(),
                source,
            })? {
                let entry = entry.map_err(|source| DataRawManifestError::Io {
                    path: directory.clone(),
                    source,
                })?;
                let path = entry.path();
                let metadata =
                    fs::symlink_metadata(&path).map_err(|source| DataRawManifestError::Io {
                        path: path.clone(),
                        source,
                    })?;
                if metadata_is_link(&metadata) {
                    return Err(DataRawManifestError::Symlink(path));
                }
                if metadata.is_dir() {
                    pending.push(path);
                } else if metadata.is_file() {
                    let relative = path
                        .strip_prefix(root)
                        .expect("walked path remains under root");
                    let normalized = normalize_relative_path(relative)?;
                    files.push(normalized);
                    if files.len() > DATARAW_MAX_FILES {
                        return Err(DataRawManifestError::TooManyFiles(files.len()));
                    }
                }
            }
        }
        files.sort_unstable();
        let mut digest = Sha256::new();
        for path in &files {
            let bytes = path.as_bytes();
            digest.update(
                u32::try_from(bytes.len())
                    .expect("path bound fits u32")
                    .to_le_bytes(),
            );
            digest.update(bytes);
        }
        Ok(Self {
            file_count: u32::try_from(files.len()).expect("file bound fits u32"),
            list_digest: digest.finalize().into(),
        })
    }

    #[must_use]
    pub fn digest_hex(&self) -> String {
        self.list_digest.iter().fold(
            String::with_capacity(self.list_digest.len() * 2),
            |mut output, byte| {
                write!(output, "{byte:02x}").expect("writing to String cannot fail");
                output
            },
        )
    }
}

fn metadata_is_link(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    false
}

fn normalize_relative_path(path: &Path) -> Result<String, DataRawManifestError> {
    let mut output = String::new();
    for (index, component) in path.components().enumerate() {
        let component = component
            .as_os_str()
            .to_str()
            .ok_or_else(|| DataRawManifestError::NonUnicodePath(path.to_path_buf()))?;
        if index != 0 {
            output.push('/');
        }
        output.extend(component.chars().flat_map(char::to_lowercase));
    }
    if output.len() > DATARAW_MAX_RELATIVE_PATH_BYTES {
        return Err(DataRawManifestError::PathTooLong(path.to_path_buf()));
    }
    Ok(output)
}

#[derive(Debug, Error)]
pub enum DataRawManifestError {
    #[error("DataRaw directory does not exist: {0}")]
    MissingRoot(PathBuf),
    #[error("failed to inspect DataRaw path {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("DataRaw symlinks/reparse links are not supported: {0}")]
    Symlink(PathBuf),
    #[error("DataRaw contains a non-Unicode path: {0}")]
    NonUnicodePath(PathBuf),
    #[error("DataRaw relative path exceeds the safety limit: {0}")]
    PathTooLong(PathBuf),
    #[error("DataRaw contains too many files: {0}")]
    TooManyFiles(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DataRawPreflightStatus {
    Match = 0,
    ServerDisabled = 1,
    ManifestMismatch = 2,
}

impl DataRawPreflightStatus {
    #[must_use]
    pub const fn from_byte(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Match),
            1 => Some(Self::ServerDisabled),
            2 => Some(Self::ManifestMismatch),
            _ => None,
        }
    }
}

#[must_use]
pub fn encode_dataraw_request(manifest: DataRawManifest) -> [u8; DATARAW_PREFLIGHT_FRAME_LENGTH] {
    let mut frame = [0_u8; DATARAW_PREFLIGHT_FRAME_LENGTH];
    frame[0..4].copy_from_slice(&DATARAW_PREFLIGHT_REQUEST_MAGIC);
    frame[4..6].copy_from_slice(&DATARAW_PREFLIGHT_VERSION.to_le_bytes());
    frame[6..8].copy_from_slice(&DATARAW_PREFLIGHT_ENABLED.to_le_bytes());
    frame[8..12].copy_from_slice(&manifest.file_count.to_le_bytes());
    frame[12..44].copy_from_slice(&manifest.list_digest);
    frame
}

#[must_use]
pub fn encode_dataraw_response(
    status: DataRawPreflightStatus,
    server_manifest: Option<DataRawManifest>,
) -> [u8; DATARAW_PREFLIGHT_FRAME_LENGTH] {
    let manifest = server_manifest.unwrap_or_default();
    let mut frame = [0_u8; DATARAW_PREFLIGHT_FRAME_LENGTH];
    frame[0..4].copy_from_slice(&DATARAW_PREFLIGHT_RESPONSE_MAGIC);
    frame[4..6].copy_from_slice(&DATARAW_PREFLIGHT_VERSION.to_le_bytes());
    frame[6] = status as u8;
    frame[7] = u8::from(server_manifest.is_some());
    frame[8..12].copy_from_slice(&manifest.file_count.to_le_bytes());
    frame[12..44].copy_from_slice(&manifest.list_digest);
    frame
}

#[must_use]
pub fn decode_dataraw_request(
    frame: &[u8; DATARAW_PREFLIGHT_FRAME_LENGTH],
) -> Option<DataRawManifest> {
    (frame[0..4] == DATARAW_PREFLIGHT_REQUEST_MAGIC
        && u16::from_le_bytes(frame[4..6].try_into().ok()?) == DATARAW_PREFLIGHT_VERSION
        && u16::from_le_bytes(frame[6..8].try_into().ok()?) == DATARAW_PREFLIGHT_ENABLED)
        .then(|| DataRawManifest {
            file_count: u32::from_le_bytes(frame[8..12].try_into().expect("fixed slice")),
            list_digest: frame[12..44].try_into().expect("fixed slice"),
        })
}

#[must_use]
pub fn decode_dataraw_response(
    frame: &[u8; DATARAW_PREFLIGHT_FRAME_LENGTH],
) -> Option<(DataRawPreflightStatus, Option<DataRawManifest>)> {
    if frame[0..4] != DATARAW_PREFLIGHT_RESPONSE_MAGIC
        || u16::from_le_bytes(frame[4..6].try_into().ok()?) != DATARAW_PREFLIGHT_VERSION
        || frame[7] > 1
    {
        return None;
    }
    let status = DataRawPreflightStatus::from_byte(frame[6])?;
    let manifest = (frame[7] == 1).then(|| DataRawManifest {
        file_count: u32::from_le_bytes(frame[8..12].try_into().expect("fixed slice")),
        list_digest: frame[12..44].try_into().expect("fixed slice"),
    });
    Some((status, manifest))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn manifest_tracks_names_but_not_file_contents() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join("Kart_")).unwrap();
        let model = root.path().join("Kart_/Model.1S");
        fs::write(&model, b"first bytes").unwrap();
        let first = DataRawManifest::scan(root.path()).unwrap();

        fs::write(&model, b"completely different content and length").unwrap();
        let content_changed = DataRawManifest::scan(root.path()).unwrap();
        assert_eq!(first, content_changed);

        fs::write(root.path().join("Kart_/extra.png"), b"x").unwrap();
        let path_added = DataRawManifest::scan(root.path()).unwrap();
        assert_eq!(path_added.file_count, first.file_count + 1);
        assert_ne!(path_added.list_digest, first.list_digest);
    }

    #[test]
    fn wire_round_trip_is_fixed_and_versioned() {
        let manifest = DataRawManifest {
            file_count: 42,
            list_digest: [0x5a; 32],
        };
        assert_eq!(
            decode_dataraw_request(&encode_dataraw_request(manifest)),
            Some(manifest)
        );
        let response =
            encode_dataraw_response(DataRawPreflightStatus::ManifestMismatch, Some(manifest));
        assert_eq!(
            decode_dataraw_response(&response),
            Some((DataRawPreflightStatus::ManifestMismatch, Some(manifest)))
        );
    }
}
