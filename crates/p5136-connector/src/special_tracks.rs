use std::path::{Path, PathBuf};

use p5136_core::{
    bml::{BmlError, BmlLimits, BmlNode},
    packet::{PacketReader, PacketWriter},
};
use p5136_rho5::{
    LegacyRhoArchive, LegacyRhoError, LegacyRhoLimits, P5136_PACKED_ENTRY_FLAGS, Rho5Directory,
    Rho5Error, Rho5Limits, Rho5Region, Rho5WriteEntry, Rho5WriteError, Rho5Writer,
};
use thiserror::Error;

use crate::file_safety::{
    ConnectorFileError, PersistentFilePreparation, atomic_write, prepare_persistent_file,
    restore_persistent_file,
};

pub const SPECIAL_TRACK_OVERLAY_FILE: &str = "DataPack1_00014.rho5";
const TRACK_COMMON_ARCHIVE: &str = "track_common.rho";
const TRACK_LOCALE_PATH: &str = "trackLocale@kr.bml";
const TRACK_LOCALE_OVERLAY_PATH: &str = "track/common/trackLocale@kr.bml";

/// Rows whose P5136 track definition and archive are both present. Story-only
/// S tracks and locale rows without a normal `gameType` definition are not
/// unlocked because exposing them in the ordinary selector can crash it.
pub const SAFE_BLOCKED_TRACK_IDS: [&str; 6] = [
    "desert_R05",
    "fairy_I06",
    "village_I13",
    "village_R08",
    "village_R09",
    "wkc_I02",
];

const TRANSFORMER_TRACKS: [(&str, &str); 3] = [
    ("transFormer_I01", "TF 오토봇 기지"),
    ("transFormer_R01", "TF 갈바트론의 지구 습격"),
    ("transFormer_R02", "TF 사이버트론 행성의 비밀"),
];

const BML_LIMITS: BmlLimits = BmlLimits {
    max_depth: 32,
    max_nodes: 200_000,
    max_attributes_per_node: 512,
    max_children_per_node: 100_000,
    max_string_code_units: 8_192,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecialTrackPatchReport {
    pub enabled: bool,
    pub overlay_path: PathBuf,
    pub overlay_pristine: PersistentFilePreparation,
    pub unblocked_tracks: usize,
    pub restored_transformer_tracks: usize,
}

#[derive(Debug, Error)]
pub enum SpecialTrackPatchError {
    #[error(transparent)]
    File(#[from] ConnectorFileError),

    #[error("failed to read P5136 track_common.rho")]
    LegacyRho(#[from] LegacyRhoError),

    #[error("failed to decode or encode the Korean track locale")]
    Bml(#[from] BmlError),

    #[error("failed to encode the special-track RHO5 overlay")]
    Rho5(#[from] Rho5WriteError),

    #[error("failed to read a track catalog RHO5 overlay")]
    Rho5Read(#[from] Rho5Error),

    #[error("Korean track locale contains {0} trailing bytes")]
    TrailingBmlBytes(usize),

    #[error("Korean track locale root is {actual:?}, expected trackList")]
    InvalidTrackLocaleRoot { actual: String },
}

pub(crate) fn prepare_special_tracks(
    game_directory: &Path,
    enabled: bool,
    maximum_bytes: usize,
) -> Result<SpecialTrackPatchReport, SpecialTrackPatchError> {
    let data_directory = game_directory.join("Data");
    let overlay_path = data_directory.join(SPECIAL_TRACK_OVERLAY_FILE);
    let overlay_pristine = prepare_persistent_file(&overlay_path, false, maximum_bytes)?;

    if !enabled {
        restore_persistent_file(&overlay_path, &overlay_pristine, maximum_bytes)?;
        return Ok(SpecialTrackPatchReport {
            enabled,
            overlay_path,
            overlay_pristine,
            unblocked_tracks: 0,
            restored_transformer_tracks: 0,
        });
    }

    let locale_bytes = load_base_track_locale(&data_directory)?;
    let (patched_locale, unblocked_tracks, restored_transformer_tracks) =
        patch_korean_track_locale(&locale_bytes)?;

    let mut writer = Rho5Writer::new();
    writer.add(Rho5WriteEntry {
        path: TRACK_LOCALE_OVERLAY_PATH.to_owned(),
        data: patched_locale,
        flags: P5136_PACKED_ENTRY_FLAGS,
    });
    let encoded = writer.encode(
        SPECIAL_TRACK_OVERLAY_FILE,
        Rho5Region::Korea,
        &Rho5Limits::default(),
    )?;
    atomic_write(&overlay_path, encoded.as_bytes())?;

    Ok(SpecialTrackPatchReport {
        enabled,
        overlay_path,
        overlay_pristine,
        unblocked_tracks,
        restored_transformer_tracks,
    })
}

fn load_base_track_locale(data_directory: &Path) -> Result<Vec<u8>, SpecialTrackPatchError> {
    let directory = Rho5Directory::scan_kr(data_directory, Rho5Limits::default())?;
    let overlay = directory
        .entries()
        .iter()
        .filter(|entry| {
            entry
                .normalized_path()
                .eq_ignore_ascii_case(TRACK_LOCALE_OVERLAY_PATH)
                && !entry
                    .archive_name()
                    .eq_ignore_ascii_case(SPECIAL_TRACK_OVERLAY_FILE)
        })
        .max_by(|left, right| {
            left.archive_name()
                .to_ascii_lowercase()
                .cmp(&right.archive_name().to_ascii_lowercase())
                .then_with(|| left.archive_name().cmp(right.archive_name()))
        });
    if let Some(entry) = overlay {
        return Ok(directory.extract_entry_with_legacy_padding(entry)?);
    }

    let archive = LegacyRhoArchive::open(
        data_directory.join(TRACK_COMMON_ARCHIVE),
        LegacyRhoLimits::default(),
    )?;
    Ok(archive.extract_exact(TRACK_LOCALE_PATH)?)
}

fn patch_korean_track_locale(
    input: &[u8],
) -> Result<(Vec<u8>, usize, usize), SpecialTrackPatchError> {
    let mut reader = PacketReader::new(input);
    let mut root = BmlNode::decode_with_limits(&mut reader, BML_LIMITS)?;
    if !reader.remaining().is_empty() {
        return Err(SpecialTrackPatchError::TrailingBmlBytes(
            reader.remaining().len(),
        ));
    }
    if !root.name.eq_ignore_ascii_case("trackList") {
        return Err(SpecialTrackPatchError::InvalidTrackLocaleRoot { actual: root.name });
    }

    let mut unblocked_tracks = 0;
    for node in &mut root.children {
        let id = attribute(node, "id");
        if id.is_some_and(|id| SAFE_BLOCKED_TRACK_IDS.contains(&id)) {
            let before = node.attributes.len();
            node.attributes
                .retain(|(name, _)| !name.eq_ignore_ascii_case("blocked"));
            unblocked_tracks += usize::from(node.attributes.len() != before);
        }
    }

    let mut restored_transformer_tracks = 0;
    for (id, name) in TRANSFORMER_TRACKS {
        if root
            .children
            .iter()
            .any(|node| attribute(node, "id") == Some(id))
        {
            continue;
        }
        let mut node = BmlNode::new("track", "");
        node.attributes.push(("id".to_owned(), id.to_owned()));
        node.attributes.push(("name".to_owned(), name.to_owned()));
        node.attributes
            .push(("basicAi".to_owned(), "false".to_owned()));
        root.children.push(node);
        restored_transformer_tracks += 1;
    }

    let mut output = PacketWriter::new();
    root.encode_with_limits(&mut output, BML_LIMITS)?;
    let output = output.into_inner();

    // Reject a malformed patch before the live overlay is replaced.
    let mut verification = PacketReader::new(&output);
    BmlNode::decode_with_limits(&mut verification, BML_LIMITS)?;
    if !verification.remaining().is_empty() {
        return Err(SpecialTrackPatchError::TrailingBmlBytes(
            verification.remaining().len(),
        ));
    }
    Ok((output, unblocked_tracks, restored_transformer_tracks))
}

fn attribute<'a>(node: &'a BmlNode, name: &str) -> Option<&'a str> {
    node.attributes
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

#[cfg(test)]
mod tests {
    use std::{env, fs};

    use p5136_core::{
        bml::BmlNode,
        packet::{PacketReader, PacketWriter},
    };
    use p5136_rho5::{
        LegacyRhoFileProperty, LegacyRhoLimits, LegacyRhoWriteEntry, LegacyRhoWriter,
        P5136_PACKED_ENTRY_FLAGS, Rho5Directory, Rho5Limits, Rho5Region, Rho5WriteEntry,
        Rho5Writer,
    };
    use tempfile::tempdir;

    use super::{
        BML_LIMITS, SAFE_BLOCKED_TRACK_IDS, SPECIAL_TRACK_OVERLAY_FILE, TRACK_LOCALE_OVERLAY_PATH,
        TRANSFORMER_TRACKS, patch_korean_track_locale, prepare_special_tracks,
    };

    #[test]
    fn patch_unblocks_only_the_curated_normal_rows_and_adds_transformer_tracks() {
        let mut root = BmlNode::new("trackList", "");
        root.children.push(track("desert_R05", "사막", true));
        root.children.push(track("maple_S01", "스토리", true));
        let mut writer = PacketWriter::new();
        root.encode_with_limits(&mut writer, BML_LIMITS).unwrap();

        let (output, unblocked, restored) = patch_korean_track_locale(writer.as_slice()).unwrap();
        assert_eq!(unblocked, 1);
        assert_eq!(restored, TRANSFORMER_TRACKS.len());

        let mut reader = PacketReader::new(&output);
        let patched = BmlNode::decode_with_limits(&mut reader, BML_LIMITS).unwrap();
        assert!(reader.remaining().is_empty());
        assert!(patched.children.iter().any(|node| {
            super::attribute(node, "id") == Some(SAFE_BLOCKED_TRACK_IDS[0])
                && super::attribute(node, "blocked").is_none()
        }));
        assert!(patched.children.iter().any(|node| {
            super::attribute(node, "id") == Some("maple_S01")
                && super::attribute(node, "blocked") == Some("true")
        }));
        for (id, name) in TRANSFORMER_TRACKS {
            assert!(patched.children.iter().any(|node| {
                super::attribute(node, "id") == Some(id)
                    && super::attribute(node, "name") == Some(name)
            }));
        }
    }

    #[test]
    fn prepare_writes_a_decodable_overlay_and_unchecked_mode_restores_absence() {
        let directory = tempdir().unwrap();
        let data = directory.path().join("Data");
        fs::create_dir(&data).unwrap();

        let mut root = BmlNode::new("trackList", "");
        root.children.push(track("desert_R05", "사막", true));
        let mut locale = PacketWriter::new();
        root.encode_with_limits(&mut locale, BML_LIMITS).unwrap();
        let mut track_common = LegacyRhoWriter::new();
        track_common.add(LegacyRhoWriteEntry {
            path: "trackLocale@kr.bml".to_owned(),
            data: locale.into_inner(),
            property: LegacyRhoFileProperty::CompressedEncrypted,
        });
        track_common
            .write_to(data.join("track_common.rho"), LegacyRhoLimits::default())
            .unwrap();

        let enabled = prepare_special_tracks(directory.path(), true, 64 * 1024 * 1024).unwrap();
        assert_eq!(enabled.unblocked_tracks, 1);
        assert_eq!(enabled.restored_transformer_tracks, 3);
        let overlays = Rho5Directory::scan_kr(&data, Rho5Limits::default()).unwrap();
        let output = overlays.extract_exact(TRACK_LOCALE_OVERLAY_PATH).unwrap();
        let mut reader = PacketReader::new(&output);
        let patched = BmlNode::decode_with_limits(&mut reader, BML_LIMITS).unwrap();
        assert!(reader.remaining().is_empty());
        assert_eq!(
            patched
                .children
                .iter()
                .filter(|node| {
                    super::attribute(node, "id").is_some_and(|id| id.starts_with("transFormer_"))
                })
                .count(),
            3
        );

        let disabled = prepare_special_tracks(directory.path(), false, 64 * 1024 * 1024).unwrap();
        assert!(!disabled.enabled);
        assert!(!data.join(SPECIAL_TRACK_OVERLAY_FILE).exists());
    }

    #[test]
    fn special_track_patch_preserves_rows_from_the_track_import_catalog() {
        let directory = tempdir().unwrap();
        let data = directory.path().join("Data");
        fs::create_dir(&data).unwrap();

        let mut legacy_root = BmlNode::new("trackList", "");
        legacy_root
            .children
            .push(track("desert_R05", "desert_R05", true));
        write_legacy_track_common(&data, &legacy_root);

        let mut imported_root = legacy_root;
        imported_root
            .children
            .push(track("fengshen_I01", "fengshen_I01", false));
        let mut imported_locale = PacketWriter::new();
        imported_root
            .encode_with_limits(&mut imported_locale, BML_LIMITS)
            .unwrap();
        let mut catalog = Rho5Writer::new();
        catalog.add(Rho5WriteEntry {
            path: TRACK_LOCALE_OVERLAY_PATH.to_owned(),
            data: imported_locale.into_inner(),
            flags: P5136_PACKED_ENTRY_FLAGS,
        });
        let encoded = catalog
            .encode(
                "DataPack1_00013.rho5",
                Rho5Region::Korea,
                &Rho5Limits::default(),
            )
            .unwrap();
        fs::write(data.join("DataPack1_00013.rho5"), encoded.as_bytes()).unwrap();

        prepare_special_tracks(directory.path(), true, 64 * 1024 * 1024).unwrap();
        let overlays = Rho5Directory::scan_kr(&data, Rho5Limits::default()).unwrap();
        let entry = overlays
            .entries()
            .iter()
            .find(|entry| {
                entry
                    .archive_name()
                    .eq_ignore_ascii_case(SPECIAL_TRACK_OVERLAY_FILE)
                    && entry
                        .normalized_path()
                        .eq_ignore_ascii_case(TRACK_LOCALE_OVERLAY_PATH)
            })
            .unwrap();
        let output = overlays.extract_entry_with_legacy_padding(entry).unwrap();
        let mut reader = PacketReader::new(&output);
        let patched = BmlNode::decode_with_limits(&mut reader, BML_LIMITS).unwrap();
        assert!(patched.children.iter().any(|node| {
            super::attribute(node, "id") == Some("fengshen_I01")
                && super::attribute(node, "name") == Some("fengshen_I01")
        }));
    }

    #[test]
    #[ignore = "requires P5136_TEST_CLIENT_ROOT pointing at a stock P5136 client"]
    fn live_p5136_track_common_builds_and_restores_the_reserved_overlay_slot() {
        let source = env::var_os("P5136_TEST_CLIENT_ROOT")
            .map(std::path::PathBuf::from)
            .expect("P5136_TEST_CLIENT_ROOT");
        let directory = tempdir().unwrap();
        let data = directory.path().join("Data");
        fs::create_dir(&data).unwrap();
        fs::copy(
            source.join("Data/track_common.rho"),
            data.join("track_common.rho"),
        )
        .unwrap();
        fs::copy(
            source.join("Data").join(SPECIAL_TRACK_OVERLAY_FILE),
            data.join(SPECIAL_TRACK_OVERLAY_FILE),
        )
        .unwrap();
        let pristine = fs::read(data.join(SPECIAL_TRACK_OVERLAY_FILE)).unwrap();

        let enabled = prepare_special_tracks(directory.path(), true, 64 * 1024 * 1024).unwrap();
        assert_eq!(enabled.unblocked_tracks, SAFE_BLOCKED_TRACK_IDS.len());
        assert_eq!(
            enabled.restored_transformer_tracks,
            TRANSFORMER_TRACKS.len()
        );
        let overlays = Rho5Directory::scan_kr(&data, Rho5Limits::default()).unwrap();
        let locale = overlays.extract_exact(TRACK_LOCALE_OVERLAY_PATH).unwrap();
        let mut reader = PacketReader::new(&locale);
        let patched = BmlNode::decode_with_limits(&mut reader, BML_LIMITS).unwrap();
        assert!(reader.remaining().is_empty());
        for (id, name) in TRANSFORMER_TRACKS {
            assert!(patched.children.iter().any(|node| {
                super::attribute(node, "id") == Some(id)
                    && super::attribute(node, "name") == Some(name)
            }));
        }

        prepare_special_tracks(directory.path(), false, 64 * 1024 * 1024).unwrap();
        assert_eq!(
            fs::read(data.join(SPECIAL_TRACK_OVERLAY_FILE)).unwrap(),
            pristine
        );
    }

    fn track(id: &str, name: &str, blocked: bool) -> BmlNode {
        let mut node = BmlNode::new("track", "");
        node.attributes.push(("id".to_owned(), id.to_owned()));
        node.attributes.push(("name".to_owned(), name.to_owned()));
        if blocked {
            node.attributes
                .push(("blocked".to_owned(), "true".to_owned()));
        }
        node
    }

    fn write_legacy_track_common(data: &std::path::Path, root: &BmlNode) {
        let mut locale = PacketWriter::new();
        root.encode_with_limits(&mut locale, BML_LIMITS).unwrap();
        let mut track_common = LegacyRhoWriter::new();
        track_common.add(LegacyRhoWriteEntry {
            path: "trackLocale@kr.bml".to_owned(),
            data: locale.into_inner(),
            property: LegacyRhoFileProperty::CompressedEncrypted,
        });
        track_common
            .write_to(data.join("track_common.rho"), LegacyRhoLimits::default())
            .unwrap();
    }
}
