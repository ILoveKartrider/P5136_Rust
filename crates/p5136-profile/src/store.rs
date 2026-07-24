use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use p5136_core::nickname::{NicknameError, canonical_nickname_key, normalize_nickname};
use thiserror::Error;

use crate::Profile;

const LEGACY_FILENAME: &str = "Launcher.json";
const VERSION_PREFIX: &str = "Launcher.v";
const VERSION_SUFFIX: &str = ".json";
pub const DEFAULT_MAX_PROFILE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum ProfileStoreError {
    #[error(transparent)]
    Nickname(#[from] NicknameError),

    #[error("{operation} failed for {path}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("profile JSON at {path} is invalid")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("profile file {path} has {length} bytes; configured maximum is {maximum}")]
    ProfileTooLarge {
        path: PathBuf,
        length: u64,
        maximum: u64,
    },

    #[error("profile lock was poisoned")]
    LockPoisoned,

    #[error("profile revision counter is exhausted for {0}")]
    RevisionExhausted(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedProfile {
    pub nickname: String,
    pub profile: Profile,
    pub revision: Option<u64>,
    pub source_path: PathBuf,
    pub created: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedProfile {
    pub nickname: String,
    pub revision: u64,
    pub path: PathBuf,
}

#[derive(Debug)]
pub struct ProfileStore {
    root: PathBuf,
    maximum_bytes: u64,
    profile_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl ProfileStore {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_maximum_bytes(root, DEFAULT_MAX_PROFILE_BYTES)
    }

    #[must_use]
    pub fn with_maximum_bytes(root: impl Into<PathBuf>, maximum_bytes: u64) -> Self {
        Self {
            root: root.into(),
            maximum_bytes,
            profile_locks: Mutex::new(HashMap::new()),
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Loads the newest immutable revision, imports legacy `Launcher.json`, or
    /// creates revision one from the P5136 defaults.
    pub fn load_or_create(&self, nickname: &str) -> Result<LoadedProfile, ProfileStoreError> {
        let nickname = normalize_nickname(nickname)?;
        let profile_lock = self.profile_lock(&nickname)?;
        let _guard = lock(&profile_lock)?;
        self.load_or_create_locked(&nickname)
    }

    /// Writes a new immutable revision. An existing revision is never
    /// overwritten.
    pub fn save(
        &self,
        nickname: &str,
        profile: &Profile,
    ) -> Result<SavedProfile, ProfileStoreError> {
        let nickname = normalize_nickname(nickname)?;
        let profile_lock = self.profile_lock(&nickname)?;
        let _guard = lock(&profile_lock)?;
        self.save_locked(&nickname, profile)
    }

    /// Serializes read-modify-write operations for one case-insensitive
    /// nickname while allowing unrelated profiles to progress independently.
    pub fn update<F>(
        &self,
        nickname: &str,
        update: F,
    ) -> Result<(SavedProfile, Profile), ProfileStoreError>
    where
        F: FnOnce(&mut Profile),
    {
        let nickname = normalize_nickname(nickname)?;
        let profile_lock = self.profile_lock(&nickname)?;
        let _guard = lock(&profile_lock)?;
        let mut profile = self.load_or_default_locked(&nickname)?.0;
        update(&mut profile);
        let saved = self.save_locked(&nickname, &profile)?;
        Ok((saved, profile))
    }

    fn profile_lock(&self, nickname: &str) -> Result<Arc<Mutex<()>>, ProfileStoreError> {
        let mut locks = self
            .profile_locks
            .lock()
            .map_err(|_| ProfileStoreError::LockPoisoned)?;
        Ok(locks
            .entry(canonical_nickname_key(nickname))
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone())
    }

    fn load_or_create_locked(&self, nickname: &str) -> Result<LoadedProfile, ProfileStoreError> {
        let (profile, revision, source_path) = self.load_or_default_locked(nickname)?;
        if source_path.exists() {
            return Ok(LoadedProfile {
                nickname: nickname.to_owned(),
                profile,
                revision,
                source_path,
                created: false,
            });
        }

        let saved = self.save_locked(nickname, &profile)?;
        Ok(LoadedProfile {
            nickname: nickname.to_owned(),
            profile,
            revision: Some(saved.revision),
            source_path: saved.path,
            created: true,
        })
    }

    fn load_or_default_locked(
        &self,
        nickname: &str,
    ) -> Result<(Profile, Option<u64>, PathBuf), ProfileStoreError> {
        let directory = self.profile_directory(nickname)?;
        create_dir_all(&directory)?;
        if let Some((revision, path)) = newest_revision(&directory)? {
            return Ok((self.read_profile(&path)?, Some(revision), path));
        }

        let legacy = directory.join(LEGACY_FILENAME);
        if legacy.is_file() {
            return Ok((self.read_profile(&legacy)?, None, legacy));
        }
        Ok((Profile::default(), None, legacy))
    }

    fn save_locked(
        &self,
        nickname: &str,
        profile: &Profile,
    ) -> Result<SavedProfile, ProfileStoreError> {
        let directory = self.profile_directory(nickname)?;
        create_dir_all(&directory)?;
        let mut revision =
            newest_revision(&directory)?.map_or(1, |(value, _)| value.saturating_add(1));
        if revision == 0 {
            return Err(ProfileStoreError::RevisionExhausted(nickname.to_owned()));
        }

        let mut bytes =
            serde_json::to_vec_pretty(profile).map_err(|source| ProfileStoreError::Json {
                path: directory.join(version_filename(revision)),
                source,
            })?;
        bytes.push(b'\n');
        let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if length > self.maximum_bytes {
            return Err(ProfileStoreError::ProfileTooLarge {
                path: directory.join(version_filename(revision)),
                length,
                maximum: self.maximum_bytes,
            });
        }

        loop {
            let final_path = directory.join(version_filename(revision));
            if final_path.exists() {
                revision = revision
                    .checked_add(1)
                    .ok_or_else(|| ProfileStoreError::RevisionExhausted(nickname.to_owned()))?;
                continue;
            }
            let temporary_path = directory.join(format!(
                ".{}.{}.tmp",
                version_filename(revision),
                std::process::id()
            ));
            match write_new_file(&temporary_path, &bytes) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    revision = revision
                        .checked_add(1)
                        .ok_or_else(|| ProfileStoreError::RevisionExhausted(nickname.to_owned()))?;
                    continue;
                }
                Err(source) => {
                    return Err(ProfileStoreError::Io {
                        operation: "write temporary profile",
                        path: temporary_path,
                        source,
                    });
                }
            }

            match fs::rename(&temporary_path, &final_path) {
                Ok(()) => {
                    sync_directory(&directory)?;
                    return Ok(SavedProfile {
                        nickname: nickname.to_owned(),
                        revision,
                        path: final_path,
                    });
                }
                Err(_source) if final_path.exists() => {
                    let _ = fs::remove_file(&temporary_path);
                    revision = revision
                        .checked_add(1)
                        .ok_or_else(|| ProfileStoreError::RevisionExhausted(nickname.to_owned()))?;
                }
                Err(source) => {
                    let _ = fs::remove_file(&temporary_path);
                    return Err(ProfileStoreError::Io {
                        operation: "publish profile revision",
                        path: final_path,
                        source,
                    });
                }
            }
        }
    }

    fn read_profile(&self, path: &Path) -> Result<Profile, ProfileStoreError> {
        let file = File::open(path).map_err(|source| ProfileStoreError::Io {
            operation: "open profile",
            path: path.to_owned(),
            source,
        })?;
        let metadata = file.metadata().map_err(|source| ProfileStoreError::Io {
            operation: "inspect profile",
            path: path.to_owned(),
            source,
        })?;
        if metadata.len() > self.maximum_bytes {
            return Err(ProfileStoreError::ProfileTooLarge {
                path: path.to_owned(),
                length: metadata.len(),
                maximum: self.maximum_bytes,
            });
        }

        let mut bytes = Vec::with_capacity(
            usize::try_from(metadata.len()).unwrap_or(usize::MAX.min(64 * 1024)),
        );
        file.take(self.maximum_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|source| ProfileStoreError::Io {
                operation: "read profile",
                path: path.to_owned(),
                source,
            })?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > self.maximum_bytes {
            return Err(ProfileStoreError::ProfileTooLarge {
                path: path.to_owned(),
                length: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                maximum: self.maximum_bytes,
            });
        }
        serde_json::from_slice(&bytes).map_err(|source| ProfileStoreError::Json {
            path: path.to_owned(),
            source,
        })
    }

    fn profile_directory(&self, nickname: &str) -> Result<PathBuf, ProfileStoreError> {
        create_dir_all(&self.root)?;
        let requested_key = canonical_nickname_key(nickname);
        let entries = fs::read_dir(&self.root).map_err(|source| ProfileStoreError::Io {
            operation: "list profile root",
            path: self.root.clone(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| ProfileStoreError::Io {
                operation: "read profile root entry",
                path: self.root.clone(),
                source,
            })?;
            if !entry
                .file_type()
                .map_err(|source| ProfileStoreError::Io {
                    operation: "inspect profile root entry",
                    path: entry.path(),
                    source,
                })?
                .is_dir()
            {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if canonical_nickname_key(&name) == requested_key {
                return Ok(entry.path());
            }
        }
        Ok(self.root.join(nickname))
    }
}

fn lock(value: &Mutex<()>) -> Result<MutexGuard<'_, ()>, ProfileStoreError> {
    value.lock().map_err(|_| ProfileStoreError::LockPoisoned)
}

fn create_dir_all(path: &Path) -> Result<(), ProfileStoreError> {
    fs::create_dir_all(path).map_err(|source| ProfileStoreError::Io {
        operation: "create profile directory",
        path: path.to_owned(),
        source,
    })
}

fn newest_revision(directory: &Path) -> Result<Option<(u64, PathBuf)>, ProfileStoreError> {
    let entries = fs::read_dir(directory).map_err(|source| ProfileStoreError::Io {
        operation: "list profile revisions",
        path: directory.to_owned(),
        source,
    })?;
    let mut newest: Option<(u64, PathBuf)> = None;
    for entry in entries {
        let entry = entry.map_err(|source| ProfileStoreError::Io {
            operation: "read profile directory entry",
            path: directory.to_owned(),
            source,
        })?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(revision) = parse_revision(&name) else {
            continue;
        };
        if newest
            .as_ref()
            .is_none_or(|(current, _)| revision > *current)
        {
            newest = Some((revision, entry.path()));
        }
    }
    Ok(newest)
}

fn parse_revision(filename: &str) -> Option<u64> {
    filename
        .strip_prefix(VERSION_PREFIX)?
        .strip_suffix(VERSION_SUFFIX)?
        .parse()
        .ok()
}

fn version_filename(revision: u64) -> String {
    format!("{VERSION_PREFIX}{revision:020}{VERSION_SUFFIX}")
}

fn write_new_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()
}

#[cfg(unix)]
fn sync_directory(directory: &Path) -> Result<(), ProfileStoreError> {
    File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|source| ProfileStoreError::Io {
            operation: "sync profile directory",
            path: directory.to_owned(),
            source,
        })
}

#[cfg(not(unix))]
fn sync_directory(directory: &Path) -> Result<(), ProfileStoreError> {
    fs::metadata(directory).map_err(|source| ProfileStoreError::Io {
        operation: "verify published profile directory",
        path: directory.to_owned(),
        source,
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc, thread};

    use serde_json::json;
    use tempfile::tempdir;

    use super::{ProfileStore, ProfileStoreError};

    #[test]
    fn creates_and_loads_an_immutable_first_revision() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let created = store.load_or_create("Rider").unwrap();
        assert!(created.created);
        assert_eq!(created.revision, Some(1));
        assert!(created.source_path.is_file());

        let loaded = store.load_or_create("Rider").unwrap();
        assert!(!loaded.created);
        assert_eq!(loaded.revision, Some(1));
        assert_eq!(loaded.profile, created.profile);
    }

    #[test]
    fn imports_legacy_launcher_json_and_preserves_unknown_fields() {
        let root = tempdir().unwrap();
        let directory = root.path().join("Rider");
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("Launcher.json"),
            serde_json::to_vec(&json!({
                "Rider": {"Lucci": 42, "Future": "kept"},
                "FutureTop": true
            }))
            .unwrap(),
        )
        .unwrap();

        let store = ProfileStore::new(root.path());
        let loaded = store.load_or_create("Rider").unwrap();
        assert_eq!(loaded.revision, None);
        assert_eq!(loaded.profile.rider.lucci, 42);
        let saved = store.save("Rider", &loaded.profile).unwrap();
        assert_eq!(saved.revision, 1);

        let reloaded = store.load_or_create("Rider").unwrap();
        assert_eq!(reloaded.revision, Some(1));
        assert_eq!(reloaded.profile.rider.extra["Future"], "kept");
        assert_eq!(reloaded.profile.extra["FutureTop"], true);
    }

    #[test]
    fn concurrent_updates_for_one_name_do_not_lose_mutations() {
        let root = tempdir().unwrap();
        let store = Arc::new(ProfileStore::new(root.path()));
        store.load_or_create("Rider").unwrap();
        let mut workers = Vec::new();
        for _ in 0..16 {
            let store = Arc::clone(&store);
            workers.push(thread::spawn(move || {
                store
                    .update("rIDER", |profile| profile.rider.lucci += 1)
                    .unwrap();
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }

        let loaded = store.load_or_create("Rider").unwrap();
        assert_eq!(loaded.profile.rider.lucci, 1_000_016);
        assert_eq!(loaded.revision, Some(17));
    }

    #[test]
    fn size_validation_failure_does_not_publish_a_revision() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let profile = store.load_or_create("Rider").unwrap().profile;
        let bounded_store = ProfileStore::with_maximum_bytes(root.path(), 100);
        assert!(matches!(
            bounded_store.save("Rider", &profile),
            Err(ProfileStoreError::ProfileTooLarge { .. })
        ));
        assert_eq!(store.load_or_create("Rider").unwrap().revision, Some(1));
    }
}
