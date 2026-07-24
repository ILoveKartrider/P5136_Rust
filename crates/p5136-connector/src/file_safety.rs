use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use fs2::FileExt;
use tempfile::{Builder, NamedTempFile};
use thiserror::Error;

pub(crate) const LOCK_FILE_NAME: &str = ".p5136-connector.lock";
pub(crate) const LEGACY_BACKUP_SUFFIX: &str = ".launcher-v2.bak";
pub(crate) const LEGACY_CREATED_SUFFIX: &str = ".launcher-v2.created";
pub(crate) const LEGACY_TEMPORARY_SUFFIX: &str = ".launcher-v2.tmp";
pub(crate) const PRISTINE_BACKUP_SUFFIX: &str = ".pristine.bak";
pub(crate) const PRISTINE_ABSENT_SUFFIX: &str = ".pristine.absent";

const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Debug, Error)]
pub enum ConnectorFileError {
    #[error("{operation} failed for {path}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("{path} is {actual} bytes; configured maximum is {maximum}")]
    FileTooLarge {
        path: PathBuf,
        actual: u64,
        maximum: usize,
    },

    #[error("timed out after {timeout:?} waiting for connector lock {path}")]
    LockTimeout { path: PathBuf, timeout: Duration },

    #[error("both pristine backup and pristine-absent marker exist for {0}")]
    PristineStateConflict(PathBuf),

    #[error("required file is missing and has no pristine backup: {0}")]
    MissingRequiredFile(PathBuf),

    #[error("legacy marker says required file was originally absent: {0}")]
    RequiredFileWasLegacyCreated(PathBuf),
}

pub(crate) struct InstallationLock {
    file: File,
}

impl InstallationLock {
    pub(crate) fn acquire(
        game_directory: &Path,
        timeout: Duration,
    ) -> Result<Self, ConnectorFileError> {
        fs::create_dir_all(game_directory).map_err(|source| ConnectorFileError::Io {
            operation: "create game directory",
            path: game_directory.to_owned(),
            source,
        })?;
        let path = game_directory.join(LOCK_FILE_NAME);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|source| ConnectorFileError::Io {
                operation: "open connector lock",
                path: path.clone(),
                source,
            })?;
        let started = Instant::now();

        loop {
            match FileExt::try_lock_exclusive(&file) {
                Ok(()) => return Ok(Self { file }),
                Err(source) if is_lock_contended(&source) => {
                    let elapsed = started.elapsed();
                    if elapsed >= timeout {
                        return Err(ConnectorFileError::LockTimeout { path, timeout });
                    }
                    thread::sleep(LOCK_POLL_INTERVAL.min(timeout.saturating_sub(elapsed)));
                }
                Err(source) => {
                    return Err(ConnectorFileError::Io {
                        operation: "acquire connector lock",
                        path,
                        source,
                    });
                }
            }
        }
    }
}

impl Drop for InstallationLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PristineState {
    Backup,
    Absent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PristineAction {
    Reused,
    CreatedBackup,
    RecordedAbsence,
    MigratedLegacyBackup,
    MigratedLegacyAbsence,
    RecoveredRequiredFile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistentFilePreparation {
    pub state: PristineState,
    pub action: PristineAction,
}

pub(crate) fn prepare_persistent_file(
    path: &Path,
    required: bool,
    maximum_bytes: usize,
) -> Result<PersistentFilePreparation, ConnectorFileError> {
    let parent = parent_directory(path)?;
    fs::create_dir_all(parent).map_err(|source| ConnectorFileError::Io {
        operation: "create parent directory",
        path: parent.to_owned(),
        source,
    })?;

    let backup_path = append_suffix(path, PRISTINE_BACKUP_SUFFIX);
    let absent_path = append_suffix(path, PRISTINE_ABSENT_SUFFIX);
    let mut has_backup = backup_path.is_file();
    let mut was_absent = absent_path.is_file();
    if has_backup && was_absent {
        return Err(ConnectorFileError::PristineStateConflict(path.to_owned()));
    }

    let mut action = PristineAction::Reused;
    if !has_backup && !was_absent {
        let legacy_backup_path = append_suffix(path, LEGACY_BACKUP_SUFFIX);
        let legacy_created_path = append_suffix(path, LEGACY_CREATED_SUFFIX);
        if legacy_backup_path.is_file() {
            create_backup_once(&legacy_backup_path, &backup_path, maximum_bytes)?;
            atomic_copy(&legacy_backup_path, path, maximum_bytes)?;
            remove_file_if_exists(&legacy_backup_path, "remove legacy backup")?;
            remove_file_if_exists(&legacy_created_path, "remove legacy created marker")?;
            has_backup = true;
            action = PristineAction::MigratedLegacyBackup;
        } else if legacy_created_path.is_file() {
            if required {
                return Err(ConnectorFileError::RequiredFileWasLegacyCreated(
                    path.to_owned(),
                ));
            }
            create_absent_marker_once(&absent_path)?;
            remove_file_if_exists(&legacy_created_path, "remove legacy created marker")?;
            was_absent = true;
            action = PristineAction::MigratedLegacyAbsence;
        } else if path.is_file() {
            create_backup_once(path, &backup_path, maximum_bytes)?;
            has_backup = true;
            action = PristineAction::CreatedBackup;
        } else if required {
            return Err(ConnectorFileError::MissingRequiredFile(path.to_owned()));
        } else {
            create_absent_marker_once(&absent_path)?;
            was_absent = true;
            action = PristineAction::RecordedAbsence;
        }
    }

    remove_file_if_exists(
        &append_suffix(path, LEGACY_TEMPORARY_SUFFIX),
        "remove stale legacy temporary file",
    )?;

    if required && !path.is_file() {
        if has_backup {
            atomic_copy(&backup_path, path, maximum_bytes)?;
            action = PristineAction::RecoveredRequiredFile;
        } else {
            return Err(ConnectorFileError::MissingRequiredFile(path.to_owned()));
        }
    }

    Ok(PersistentFilePreparation {
        state: if has_backup {
            PristineState::Backup
        } else {
            debug_assert!(was_absent);
            PristineState::Absent
        },
        action,
    })
}

pub(crate) fn read_bounded(
    path: &Path,
    maximum_bytes: usize,
) -> Result<Vec<u8>, ConnectorFileError> {
    let file = File::open(path).map_err(|source| ConnectorFileError::Io {
        operation: "open file",
        path: path.to_owned(),
        source,
    })?;
    let metadata = file.metadata().map_err(|source| ConnectorFileError::Io {
        operation: "read file metadata",
        path: path.to_owned(),
        source,
    })?;
    enforce_file_limit(path, metadata.len(), maximum_bytes)?;

    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(usize::try_from(metadata.len()).map_err(|_| {
            ConnectorFileError::FileTooLarge {
                path: path.to_owned(),
                actual: metadata.len(),
                maximum: maximum_bytes,
            }
        })?)
        .map_err(|source| ConnectorFileError::Io {
            operation: "reserve file buffer",
            path: path.to_owned(),
            source: io::Error::other(source),
        })?;
    let read_limit = u64::try_from(maximum_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|source| ConnectorFileError::Io {
            operation: "read file",
            path: path.to_owned(),
            source,
        })?;
    enforce_file_limit(
        path,
        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        maximum_bytes,
    )?;
    Ok(bytes)
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ConnectorFileError> {
    let parent = parent_directory(path)?;
    fs::create_dir_all(parent).map_err(|source| ConnectorFileError::Io {
        operation: "create parent directory",
        path: parent.to_owned(),
        source,
    })?;
    let mut temporary = new_temporary_file(parent, path)?;
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.as_file_mut().sync_all())
        .map_err(|source| ConnectorFileError::Io {
            operation: "write temporary file",
            path: path.to_owned(),
            source,
        })?;
    temporary
        .persist(path)
        .map_err(|error| ConnectorFileError::Io {
            operation: "atomically replace file",
            path: path.to_owned(),
            source: error.error,
        })?;
    Ok(())
}

pub(crate) fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value: OsString = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

fn create_backup_once(
    source_path: &Path,
    backup_path: &Path,
    maximum_bytes: usize,
) -> Result<(), ConnectorFileError> {
    let parent = parent_directory(backup_path)?;
    let mut source = File::open(source_path).map_err(|source| ConnectorFileError::Io {
        operation: "open pristine source",
        path: source_path.to_owned(),
        source,
    })?;
    let metadata = source.metadata().map_err(|source| ConnectorFileError::Io {
        operation: "read pristine source metadata",
        path: source_path.to_owned(),
        source,
    })?;
    enforce_file_limit(source_path, metadata.len(), maximum_bytes)?;

    let mut temporary = new_temporary_file(parent, backup_path)?;
    copy_bounded(
        &mut source,
        temporary.as_file_mut(),
        source_path,
        maximum_bytes,
    )?;
    temporary
        .as_file_mut()
        .sync_all()
        .map_err(|source| ConnectorFileError::Io {
            operation: "sync pristine backup",
            path: backup_path.to_owned(),
            source,
        })?;
    match temporary.persist_noclobber(backup_path) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(ConnectorFileError::Io {
            operation: "create pristine backup",
            path: backup_path.to_owned(),
            source: error.error,
        }),
    }
}

fn create_absent_marker_once(path: &Path) -> Result<(), ConnectorFileError> {
    match OpenOptions::new().create_new(true).write(true).open(path) {
        Ok(file) => file.sync_all().map_err(|source| ConnectorFileError::Io {
            operation: "sync pristine-absent marker",
            path: path.to_owned(),
            source,
        }),
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(source) => Err(ConnectorFileError::Io {
            operation: "create pristine-absent marker",
            path: path.to_owned(),
            source,
        }),
    }
}

fn atomic_copy(
    source_path: &Path,
    destination_path: &Path,
    maximum_bytes: usize,
) -> Result<(), ConnectorFileError> {
    let mut source = File::open(source_path).map_err(|source| ConnectorFileError::Io {
        operation: "open copy source",
        path: source_path.to_owned(),
        source,
    })?;
    let parent = parent_directory(destination_path)?;
    let mut temporary = new_temporary_file(parent, destination_path)?;
    copy_bounded(
        &mut source,
        temporary.as_file_mut(),
        source_path,
        maximum_bytes,
    )?;
    temporary
        .as_file_mut()
        .sync_all()
        .map_err(|source| ConnectorFileError::Io {
            operation: "sync copied file",
            path: destination_path.to_owned(),
            source,
        })?;
    temporary
        .persist(destination_path)
        .map_err(|error| ConnectorFileError::Io {
            operation: "atomically replace copied file",
            path: destination_path.to_owned(),
            source: error.error,
        })?;
    Ok(())
}

fn copy_bounded(
    source: &mut File,
    destination: &mut File,
    source_path: &Path,
    maximum_bytes: usize,
) -> Result<(), ConnectorFileError> {
    let read_limit = u64::try_from(maximum_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let copied = io::copy(&mut source.take(read_limit), destination).map_err(|source| {
        ConnectorFileError::Io {
            operation: "copy file",
            path: source_path.to_owned(),
            source,
        }
    })?;
    enforce_file_limit(source_path, copied, maximum_bytes)
}

fn new_temporary_file(
    parent: &Path,
    destination: &Path,
) -> Result<NamedTempFile, ConnectorFileError> {
    Builder::new()
        .prefix(".p5136-connector-")
        .suffix(".tmp")
        .tempfile_in(parent)
        .map_err(|source| ConnectorFileError::Io {
            operation: "create same-directory temporary file",
            path: destination.to_owned(),
            source,
        })
}

fn parent_directory(path: &Path) -> Result<&Path, ConnectorFileError> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| ConnectorFileError::Io {
            operation: "resolve parent directory",
            path: path.to_owned(),
            source: io::Error::new(io::ErrorKind::InvalidInput, "path has no parent directory"),
        })
}

fn enforce_file_limit(path: &Path, actual: u64, maximum: usize) -> Result<(), ConnectorFileError> {
    if actual > u64::try_from(maximum).unwrap_or(u64::MAX) {
        Err(ConnectorFileError::FileTooLarge {
            path: path.to_owned(),
            actual,
            maximum,
        })
    } else {
        Ok(())
    }
}

fn remove_file_if_exists(path: &Path, operation: &'static str) -> Result<(), ConnectorFileError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(ConnectorFileError::Io {
            operation,
            path: path.to_owned(),
            source,
        }),
    }
}

fn is_lock_contended(source: &io::Error) -> bool {
    let expected = fs2::lock_contended_error();
    match (source.raw_os_error(), expected.raw_os_error()) {
        (Some(actual), Some(expected)) => actual == expected,
        _ => source.kind() == expected.kind(),
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, time::Duration};

    use tempfile::tempdir;

    use super::{
        ConnectorFileError, InstallationLock, LEGACY_BACKUP_SUFFIX, LEGACY_CREATED_SUFFIX,
        LEGACY_TEMPORARY_SUFFIX, PRISTINE_ABSENT_SUFFIX, PRISTINE_BACKUP_SUFFIX, PristineAction,
        PristineState, append_suffix, prepare_persistent_file,
    };

    #[test]
    fn lock_is_exclusive_across_separate_file_handles() {
        let directory = tempdir().unwrap();
        let first = InstallationLock::acquire(directory.path(), Duration::ZERO).unwrap();
        assert!(matches!(
            InstallationLock::acquire(directory.path(), Duration::ZERO),
            Err(ConnectorFileError::LockTimeout { .. })
        ));
        drop(first);
        InstallationLock::acquire(directory.path(), Duration::ZERO).unwrap();
    }

    #[test]
    fn pristine_backup_is_created_once_and_never_overwritten() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("KartRider.xml");
        fs::write(&path, b"stock").unwrap();

        let first = prepare_persistent_file(&path, false, 1024).unwrap();
        assert_eq!(first.state, PristineState::Backup);
        assert_eq!(first.action, PristineAction::CreatedBackup);
        fs::write(&path, b"patched").unwrap();
        let second = prepare_persistent_file(&path, false, 1024).unwrap();
        assert_eq!(second.action, PristineAction::Reused);
        assert_eq!(
            fs::read(append_suffix(&path, PRISTINE_BACKUP_SUFFIX)).unwrap(),
            b"stock"
        );
    }

    #[test]
    fn absent_file_gets_a_stable_marker() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("Profile/kr/launcher.xml");

        let first = prepare_persistent_file(&path, false, 1024).unwrap();
        assert_eq!(first.state, PristineState::Absent);
        assert_eq!(first.action, PristineAction::RecordedAbsence);
        assert!(append_suffix(&path, PRISTINE_ABSENT_SUFFIX).is_file());
        assert_eq!(
            prepare_persistent_file(&path, false, 1024).unwrap().action,
            PristineAction::Reused
        );
    }

    #[test]
    fn legacy_backup_and_absence_markers_are_migrated() {
        let directory = tempdir().unwrap();
        let backup_path = directory.path().join("KartRider.xml");
        fs::write(&backup_path, b"old patched").unwrap();
        fs::write(append_suffix(&backup_path, LEGACY_BACKUP_SUFFIX), b"stock").unwrap();
        fs::write(
            append_suffix(&backup_path, LEGACY_TEMPORARY_SUFFIX),
            b"stale",
        )
        .unwrap();

        let migrated = prepare_persistent_file(&backup_path, false, 1024).unwrap();
        assert_eq!(migrated.action, PristineAction::MigratedLegacyBackup);
        assert_eq!(fs::read(&backup_path).unwrap(), b"stock");
        assert_eq!(
            fs::read(append_suffix(&backup_path, PRISTINE_BACKUP_SUFFIX)).unwrap(),
            b"stock"
        );
        assert!(!append_suffix(&backup_path, LEGACY_BACKUP_SUFFIX).exists());
        assert!(!append_suffix(&backup_path, LEGACY_TEMPORARY_SUFFIX).exists());

        let absent_path = directory.path().join("Profile/kr/launcher.xml");
        fs::create_dir_all(absent_path.parent().unwrap()).unwrap();
        fs::write(append_suffix(&absent_path, LEGACY_CREATED_SUFFIX), b"").unwrap();
        let migrated = prepare_persistent_file(&absent_path, false, 1024).unwrap();
        assert_eq!(migrated.action, PristineAction::MigratedLegacyAbsence);
        assert!(append_suffix(&absent_path, PRISTINE_ABSENT_SUFFIX).is_file());
        assert!(!append_suffix(&absent_path, LEGACY_CREATED_SUFFIX).exists());
    }

    #[test]
    fn conflicting_pristine_state_is_rejected_without_overwriting_either_record() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("KartRider.xml");
        let backup_path = append_suffix(&path, PRISTINE_BACKUP_SUFFIX);
        let absent_path = append_suffix(&path, PRISTINE_ABSENT_SUFFIX);
        fs::write(&backup_path, b"stock").unwrap();
        fs::write(&absent_path, b"").unwrap();

        assert!(matches!(
            prepare_persistent_file(&path, false, 1024),
            Err(ConnectorFileError::PristineStateConflict(_))
        ));
        assert_eq!(fs::read(backup_path).unwrap(), b"stock");
        assert!(absent_path.is_file());
    }
}
