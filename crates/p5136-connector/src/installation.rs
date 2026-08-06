use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use thiserror::Error;

use crate::{
    codec_error::PinCodecError,
    detection::{BuildDetectionError, BuildEvidence, detect_p5136},
    file_safety::{
        ConnectorFileError, InstallationLock, PersistentFilePreparation, atomic_write,
        prepare_persistent_file, read_bounded,
    },
    identity::{IdentityError, normalize_nickname},
    limits::CodecLimits,
    pin::{PinPatchOptions, PinPatchReport, patch_p5136_pin_with_limits},
    xml::{launcher_profile_xml, server_config_xml},
};
use std::net::SocketAddrV4;

pub const DEFAULT_INSTALLATION_LOCK_TIMEOUT: Duration = Duration::from_secs(10);
pub const DEFAULT_MAXIMUM_PERSISTENT_FILE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstallationOptions {
    pub remove_ngs_on: bool,
    pub lock_timeout: Duration,
    pub maximum_persistent_file_bytes: usize,
    pub codec_limits: CodecLimits,
}

impl Default for InstallationOptions {
    fn default() -> Self {
        Self {
            // Matches the original connector's default Setting.NgsOn=false.
            remove_ngs_on: true,
            lock_timeout: DEFAULT_INSTALLATION_LOCK_TIMEOUT,
            maximum_persistent_file_bytes: DEFAULT_MAXIMUM_PERSISTENT_FILE_BYTES,
            codec_limits: CodecLimits::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedInstallation {
    pub build_evidence: BuildEvidence,
    pub pin_path: PathBuf,
    pub game_config_path: PathBuf,
    pub launcher_profile_path: PathBuf,
    pub pin_pristine: PersistentFilePreparation,
    pub game_config_pristine: PersistentFilePreparation,
    pub launcher_profile_pristine: PersistentFilePreparation,
    pub pin_patch: PinPatchReport,
}

#[derive(Debug, Error)]
pub enum InstallationError {
    #[error("installation is not recognized as KartRider P5136")]
    UnsupportedBuild,

    #[error("build detection failed")]
    Detection(#[from] BuildDetectionError),

    #[error("safe file preparation failed")]
    File(#[from] ConnectorFileError),

    #[error("invalid connector nickname")]
    Identity(#[from] IdentityError),

    #[error("PIN preparation failed")]
    Pin(#[from] PinCodecError),
}

pub fn prepare_installation(
    game_directory: &Path,
    login_endpoint: SocketAddrV4,
    nickname: &str,
    options: &InstallationOptions,
) -> Result<PreparedInstallation, InstallationError> {
    let nickname = normalize_nickname(nickname)?;
    let _lock = InstallationLock::acquire(game_directory, options.lock_timeout)?;
    let build_evidence = detect_p5136(game_directory, &options.codec_limits)?
        .ok_or(InstallationError::UnsupportedBuild)?;

    let pin_path = game_directory.join("KartRider.pin");
    let game_config_path = game_directory.join("KartRider.xml");
    let launcher_profile_path = game_directory.join("Profile/kr/launcher.xml");

    let pin_pristine =
        prepare_persistent_file(&pin_path, true, options.codec_limits.max_pin_file_bytes)?;
    let game_config_pristine = prepare_persistent_file(
        &game_config_path,
        false,
        options.maximum_persistent_file_bytes,
    )?;
    let launcher_profile_pristine = prepare_persistent_file(
        &launcher_profile_path,
        false,
        options.maximum_persistent_file_bytes,
    )?;

    let pin_input = read_bounded(&pin_path, options.codec_limits.max_pin_file_bytes)?;
    let (patched_pin, patch_report) = patch_p5136_pin_with_limits(
        &pin_input,
        login_endpoint,
        PinPatchOptions {
            remove_ngs_on: options.remove_ngs_on,
            override_storage: true,
        },
        &options.codec_limits,
    )?;
    let game_config = server_config_xml(login_endpoint);
    let launcher_profile = launcher_profile_xml(&nickname);

    // All three outputs are fully generated and the PIN has been reparsed
    // before the first live file is replaced.
    atomic_write(&pin_path, &patched_pin)?;
    atomic_write(&game_config_path, &game_config)?;
    atomic_write(&launcher_profile_path, &launcher_profile)?;

    Ok(PreparedInstallation {
        build_evidence,
        pin_path,
        game_config_path,
        launcher_profile_path,
        pin_pristine,
        game_config_pristine,
        launcher_profile_pristine,
        pin_patch: patch_report,
    })
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        net::{Ipv4Addr, SocketAddrV4},
    };

    use tempfile::tempdir;

    use super::{InstallationOptions, prepare_installation};
    use crate::{
        P5136_RIDER_DATA_DIRECTORY, P5136_SCREENSHOT_DIRECTORY, P5136_STORAGE_ROOT,
        detection::{BuildEvidence, PinDetectionSource},
        file_safety::{
            PRISTINE_ABSENT_SUFFIX, PRISTINE_BACKUP_SUFFIX, PristineAction, append_suffix,
        },
        pin::PinDocument,
        test_fixture::csharp_synthetic_pin,
        xml::{launcher_profile_xml, server_config_xml},
    };

    #[test]
    fn prepares_all_three_files_and_keeps_pristine_state_across_repatches() {
        let directory = tempdir().unwrap();
        let pin_path = directory.path().join("KartRider.pin");
        let game_config_path = directory.path().join("KartRider.xml");
        let launcher_profile_path = directory.path().join("Profile/kr/launcher.xml");
        let pristine_pin = csharp_synthetic_pin();
        let pristine_game_config = b"<config><stock value='1'/></config>";
        fs::write(directory.path().join("KartRider.exe"), b"wrong hash").unwrap();
        fs::write(&pin_path, &pristine_pin).unwrap();
        fs::write(&game_config_path, pristine_game_config).unwrap();

        let first_endpoint = SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 20), 46_001);
        let mut options = InstallationOptions {
            remove_ngs_on: true,
            ..InstallationOptions::default()
        };
        let first =
            prepare_installation(directory.path(), first_endpoint, "first-user", &options).unwrap();
        assert_eq!(
            first.build_evidence,
            BuildEvidence::PinHeader(PinDetectionSource::Live)
        );
        assert_eq!(first.pin_patch.authentication_methods, 2);
        assert_eq!(first.pin_patch.removed_ngs_on_entries, 1);
        assert!(first.pin_patch.storage_overridden);
        assert_eq!(
            fs::read(append_suffix(&pin_path, PRISTINE_BACKUP_SUFFIX)).unwrap(),
            pristine_pin
        );
        assert_eq!(
            fs::read(append_suffix(&game_config_path, PRISTINE_BACKUP_SUFFIX)).unwrap(),
            pristine_game_config
        );
        assert!(append_suffix(&launcher_profile_path, PRISTINE_ABSENT_SUFFIX).is_file());
        assert_eq!(
            fs::read(&game_config_path).unwrap(),
            server_config_xml(first_endpoint)
        );
        assert_eq!(
            fs::read(&launcher_profile_path).unwrap(),
            launcher_profile_xml("first-user")
        );
        let patched = PinDocument::decode(&fs::read(&pin_path).unwrap()).unwrap();
        assert!(
            patched
                .auth_methods
                .iter()
                .all(|auth| auth.login_servers == [first_endpoint])
        );
        assert_p5136_storage(&patched);

        let second_endpoint = SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 21), 46_002);
        options.remove_ngs_on = false;
        let second =
            prepare_installation(directory.path(), second_endpoint, "second-user", &options)
                .unwrap();
        assert_eq!(second.pin_pristine.action, PristineAction::Reused);
        assert_eq!(second.game_config_pristine.action, PristineAction::Reused);
        assert_eq!(
            second.launcher_profile_pristine.action,
            PristineAction::Reused
        );
        assert_eq!(
            fs::read(append_suffix(&pin_path, PRISTINE_BACKUP_SUFFIX)).unwrap(),
            pristine_pin
        );
        assert_eq!(
            fs::read(&game_config_path).unwrap(),
            server_config_xml(second_endpoint)
        );
        assert_eq!(
            fs::read(&launcher_profile_path).unwrap(),
            launcher_profile_xml("second-user")
        );
        let repatched = PinDocument::decode(&fs::read(&pin_path).unwrap()).unwrap();
        assert_p5136_storage(&repatched);
        for parent in [
            directory.path().to_owned(),
            directory.path().join("Profile/kr"),
        ] {
            assert!(fs::read_dir(parent).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".p5136-connector-")
            }));
        }
    }

    fn assert_p5136_storage(pin: &PinDocument) {
        let storage = pin.storage_config.as_ref().unwrap();
        assert_eq!(storage.name, "storage");
        assert!(storage.attributes.is_empty());
        assert_eq!(storage.children.len(), 1);
        let document = &storage.children[0];
        assert_eq!(document.name, "document");
        assert_eq!(
            document.attributes,
            [
                ("root".to_owned(), P5136_STORAGE_ROOT.to_owned()),
                (
                    "screenShot".to_owned(),
                    P5136_SCREENSHOT_DIRECTORY.to_owned()
                ),
                (
                    "riderData".to_owned(),
                    P5136_RIDER_DATA_DIRECTORY.to_owned()
                ),
            ]
        );
    }

    #[test]
    fn detects_and_recovers_a_missing_required_pin_from_pristine_backup() {
        let directory = tempdir().unwrap();
        let pin_path = directory.path().join("KartRider.pin");
        let pristine = csharp_synthetic_pin();
        fs::write(append_suffix(&pin_path, PRISTINE_BACKUP_SUFFIX), &pristine).unwrap();
        let endpoint = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 39_312);

        let prepared = prepare_installation(
            directory.path(),
            endpoint,
            "recovered",
            &InstallationOptions::default(),
        )
        .unwrap();
        assert_eq!(
            prepared.build_evidence,
            BuildEvidence::PinHeader(PinDetectionSource::PristineBackup)
        );
        assert_eq!(
            prepared.pin_pristine.action,
            PristineAction::RecoveredRequiredFile
        );
        assert_eq!(
            fs::read(append_suffix(&pin_path, PRISTINE_BACKUP_SUFFIX)).unwrap(),
            pristine
        );
        let live = PinDocument::decode(&fs::read(pin_path).unwrap()).unwrap();
        assert!(
            live.auth_methods
                .iter()
                .all(|auth| auth.login_servers == [endpoint])
        );
    }

    #[test]
    fn unsupported_installation_is_not_modified() {
        let directory = tempdir().unwrap();
        let pin_path = directory.path().join("KartRider.pin");
        fs::write(&pin_path, b"not a pin").unwrap();
        assert!(
            prepare_installation(
                directory.path(),
                SocketAddrV4::new(Ipv4Addr::LOCALHOST, 39_312),
                "user",
                &InstallationOptions::default()
            )
            .is_err()
        );
        assert_eq!(fs::read(pin_path).unwrap(), b"not a pin");
        assert!(!directory.path().join("KartRider.xml").exists());
    }
}
