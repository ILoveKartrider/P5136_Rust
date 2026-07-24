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

    #[error(
        "profile revision {revision} for {nickname} was published at {path}, but directory durability could not be confirmed"
    )]
    CommittedButDurabilityUncertain {
        nickname: String,
        revision: u64,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedProfile {
    pub nickname: String,
    pub profile: Profile,
    pub revision: Option<u64>,
    pub source_path: PathBuf,
    pub created: bool,
    pub recovered_revisions: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedProfile {
    pub nickname: String,
    pub revision: u64,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProfileMutation<T> {
    Changed { value: T, profile: Box<Profile> },
    Unchanged(T),
}

impl<T> ProfileMutation<T> {
    #[must_use]
    pub fn changed(value: T, profile: Profile) -> Self {
        Self::Changed {
            value,
            profile: Box::new(profile),
        }
    }
}

#[derive(Debug)]
pub enum ProfileTransaction<T> {
    Unchanged {
        value: T,
        profile: Profile,
    },
    Committed {
        value: T,
        profile: Profile,
        saved: SavedProfile,
    },
    CommittedButDurabilityUncertain {
        value: T,
        profile: Profile,
        saved: SavedProfile,
        error: ProfileStoreError,
    },
}

#[derive(Debug)]
enum SaveOutcome {
    Durable(SavedProfile),
    DurabilityUncertain {
        saved: SavedProfile,
        error: ProfileStoreError,
    },
}

#[derive(Debug)]
enum PreparedPublishOutcome {
    Published(SaveOutcome),
    DestinationExists,
}

#[derive(Debug)]
pub struct ProfileStore {
    root: PathBuf,
    maximum_bytes: u64,
    profile_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    cache: Mutex<HashMap<String, CachedProfile>>,
    #[cfg(test)]
    next_directory_sync_fault: Mutex<Option<io::ErrorKind>>,
}

#[derive(Debug, Clone)]
struct CachedProfile {
    profile: Profile,
    revision: Option<u64>,
    source_path: PathBuf,
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
            cache: Mutex::new(HashMap::new()),
            #[cfg(test)]
            next_directory_sync_fault: Mutex::new(None),
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

    /// Checks whether an operator-provisioned profile directory or cached
    /// profile already exists without creating a per-nickname lock or any
    /// filesystem state.
    pub fn profile_exists(&self, nickname: &str) -> Result<bool, ProfileStoreError> {
        let nickname = normalize_nickname(nickname)?;
        if self.cached_profile(&nickname)?.is_some() {
            return Ok(true);
        }

        let requested_key = canonical_nickname_key(&nickname);
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(source) => {
                return Err(ProfileStoreError::Io {
                    operation: "list profile root",
                    path: self.root.clone(),
                    source,
                });
            }
        };
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
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Drops one cached snapshot. The next load re-reads immutable revisions
    /// from disk, which is useful after an administrator restores files.
    pub fn invalidate(&self, nickname: &str) -> Result<(), ProfileStoreError> {
        let nickname = normalize_nickname(nickname)?;
        let profile_lock = self.profile_lock(&nickname)?;
        let _guard = lock(&profile_lock)?;
        self.cache
            .lock()
            .map_err(|_| ProfileStoreError::LockPoisoned)?
            .remove(&canonical_nickname_key(&nickname));
        Ok(())
    }

    pub fn reload(&self, nickname: &str) -> Result<LoadedProfile, ProfileStoreError> {
        self.invalidate(nickname)?;
        self.load_or_create(nickname)
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
        match self.save_locked(&nickname, profile)? {
            SaveOutcome::Durable(saved) => Ok(saved),
            SaveOutcome::DurabilityUncertain { error, .. } => Err(error),
        }
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
        match self.transaction(nickname, |profile| {
            let mut profile = profile.clone();
            update(&mut profile);
            ProfileMutation::changed((), profile)
        })? {
            ProfileTransaction::Committed { profile, saved, .. } => Ok((saved, profile)),
            ProfileTransaction::CommittedButDurabilityUncertain { error, .. } => Err(error),
            ProfileTransaction::Unchanged { .. } => {
                unreachable!("an unconditional profile update always requests a commit")
            }
        }
    }

    /// Runs one conditional read-modify-write transaction under the
    /// case-insensitive per-profile lock.
    ///
    /// The closure sees an immutable snapshot. A changed transaction must
    /// return its replacement snapshot explicitly, so an unchanged outcome
    /// cannot accidentally expose a mutation that was never persisted.
    ///
    /// `ProfileMutation::Unchanged` returns the loaded snapshot without
    /// publishing a new immutable revision. Once a revision has been
    /// published, a directory-sync failure is returned as a committed outcome
    /// so callers must not blindly reapply the mutation.
    pub fn transaction<T, F>(
        &self,
        nickname: &str,
        transaction: F,
    ) -> Result<ProfileTransaction<T>, ProfileStoreError>
    where
        F: FnOnce(&Profile) -> ProfileMutation<T>,
    {
        let nickname = normalize_nickname(nickname)?;
        let profile_lock = self.profile_lock(&nickname)?;
        let _guard = lock(&profile_lock)?;
        let profile = self.load_or_default_locked(&nickname)?.0;
        match transaction(&profile) {
            ProfileMutation::Unchanged(value) => {
                Ok(ProfileTransaction::Unchanged { value, profile })
            }
            ProfileMutation::Changed { value, profile } => {
                let profile = *profile;
                match self.save_locked(&nickname, &profile)? {
                    SaveOutcome::Durable(saved) => Ok(ProfileTransaction::Committed {
                        value,
                        profile,
                        saved,
                    }),
                    SaveOutcome::DurabilityUncertain { saved, error } => {
                        Ok(ProfileTransaction::CommittedButDurabilityUncertain {
                            value,
                            profile,
                            saved,
                            error,
                        })
                    }
                }
            }
        }
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
        if let Some(cached) = self.cached_profile(nickname)? {
            return Ok(LoadedProfile {
                nickname: nickname.to_owned(),
                profile: cached.profile,
                revision: cached.revision,
                source_path: cached.source_path,
                created: false,
                recovered_revisions: Vec::new(),
            });
        }

        let (profile, revision, source_path, recovered_revisions) =
            self.load_or_default_locked(nickname)?;
        if source_path.exists() {
            self.cache_profile(nickname, &profile, revision, source_path.clone())?;
            return Ok(LoadedProfile {
                nickname: nickname.to_owned(),
                profile,
                revision,
                source_path,
                created: false,
                recovered_revisions,
            });
        }

        let saved = match self.save_locked(nickname, &profile)? {
            SaveOutcome::Durable(saved) => saved,
            SaveOutcome::DurabilityUncertain { error, .. } => return Err(error),
        };
        Ok(LoadedProfile {
            nickname: nickname.to_owned(),
            profile,
            revision: Some(saved.revision),
            source_path: saved.path,
            created: true,
            recovered_revisions,
        })
    }

    fn load_or_default_locked(
        &self,
        nickname: &str,
    ) -> Result<(Profile, Option<u64>, PathBuf, Vec<PathBuf>), ProfileStoreError> {
        if let Some(cached) = self.cached_profile(nickname)? {
            return Ok((
                cached.profile,
                cached.revision,
                cached.source_path,
                Vec::new(),
            ));
        }

        let directory = self.profile_directory(nickname)?;
        create_dir_all(&directory)?;
        let mut recovered_revisions = Vec::new();
        let mut first_corruption = None;
        for (revision, path) in revisions_descending(&directory)? {
            match self.read_profile(&path) {
                Ok(profile) => {
                    return Ok((profile, Some(revision), path, recovered_revisions));
                }
                Err(
                    error @ (ProfileStoreError::Json { .. }
                    | ProfileStoreError::ProfileTooLarge { .. }),
                ) => {
                    recovered_revisions.push(path);
                    if first_corruption.is_none() {
                        first_corruption = Some(error);
                    }
                }
                Err(error) => return Err(error),
            }
        }

        let legacy = directory.join(LEGACY_FILENAME);
        if legacy.is_file() {
            return Ok((
                self.read_profile(&legacy)?,
                None,
                legacy,
                recovered_revisions,
            ));
        }
        if let Some(error) = first_corruption {
            return Err(error);
        }
        Ok((Profile::default(), None, legacy, recovered_revisions))
    }

    fn save_locked(
        &self,
        nickname: &str,
        profile: &Profile,
    ) -> Result<SaveOutcome, ProfileStoreError> {
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

            match self.publish_prepared_revision(
                nickname,
                profile,
                revision,
                &directory,
                &temporary_path,
                &final_path,
            )? {
                PreparedPublishOutcome::Published(outcome) => return Ok(outcome),
                PreparedPublishOutcome::DestinationExists => {
                    revision = revision
                        .checked_add(1)
                        .ok_or_else(|| ProfileStoreError::RevisionExhausted(nickname.to_owned()))?;
                }
            }
        }
    }

    fn publish_prepared_revision(
        &self,
        nickname: &str,
        profile: &Profile,
        revision: u64,
        directory: &Path,
        temporary_path: &Path,
        final_path: &Path,
    ) -> Result<PreparedPublishOutcome, ProfileStoreError> {
        // Acquire the cache guard before the atomic publish. This makes it
        // impossible to create the final immutable revision and then fail to
        // acquire the cache needed to expose it. File creation, writes, and
        // fsync happen before this point; unrelated profiles are serialized
        // only for this one hard-link syscall and cache insert.
        let Ok(mut cache) = self.cache.lock() else {
            let _ = fs::remove_file(temporary_path);
            return Err(ProfileStoreError::LockPoisoned);
        };
        match publish_new_file(temporary_path, final_path) {
            Ok(true) => {
                let saved = SavedProfile {
                    nickname: nickname.to_owned(),
                    revision,
                    path: final_path.to_owned(),
                };
                cache.insert(
                    canonical_nickname_key(nickname),
                    CachedProfile {
                        profile: profile.clone(),
                        revision: Some(revision),
                        source_path: final_path.to_owned(),
                    },
                );
                drop(cache);
                let _ = fs::remove_file(temporary_path);
                Ok(PreparedPublishOutcome::Published(
                    match self.sync_published_directory(directory) {
                        Ok(()) => SaveOutcome::Durable(saved),
                        Err(source) => SaveOutcome::DurabilityUncertain {
                            error: ProfileStoreError::CommittedButDurabilityUncertain {
                                nickname: nickname.to_owned(),
                                revision,
                                path: final_path.to_owned(),
                                source,
                            },
                            saved,
                        },
                    },
                ))
            }
            Ok(false) => {
                drop(cache);
                let _ = fs::remove_file(temporary_path);
                Ok(PreparedPublishOutcome::DestinationExists)
            }
            Err(source) => {
                drop(cache);
                let _ = fs::remove_file(temporary_path);
                Err(ProfileStoreError::Io {
                    operation: "publish profile revision",
                    path: final_path.to_owned(),
                    source,
                })
            }
        }
    }

    #[cfg_attr(not(test), allow(clippy::unused_self))]
    fn sync_published_directory(&self, directory: &Path) -> io::Result<()> {
        #[cfg(test)]
        if let Some(kind) = self
            .next_directory_sync_fault
            .lock()
            .map_err(|_| io::Error::other("directory sync fault lock was poisoned"))?
            .take()
        {
            return Err(io::Error::new(
                kind,
                "injected post-publication directory sync failure",
            ));
        }
        sync_directory(directory)
    }

    #[cfg(test)]
    fn fail_next_directory_sync(&self, kind: io::ErrorKind) {
        *self.next_directory_sync_fault.lock().unwrap() = Some(kind);
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

    fn cached_profile(&self, nickname: &str) -> Result<Option<CachedProfile>, ProfileStoreError> {
        Ok(self
            .cache
            .lock()
            .map_err(|_| ProfileStoreError::LockPoisoned)?
            .get(&canonical_nickname_key(nickname))
            .cloned())
    }

    fn cache_profile(
        &self,
        nickname: &str,
        profile: &Profile,
        revision: Option<u64>,
        source_path: PathBuf,
    ) -> Result<(), ProfileStoreError> {
        self.cache
            .lock()
            .map_err(|_| ProfileStoreError::LockPoisoned)?
            .insert(
                canonical_nickname_key(nickname),
                CachedProfile {
                    profile: profile.clone(),
                    revision,
                    source_path,
                },
            );
        Ok(())
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
    Ok(revisions_descending(directory)?.into_iter().next())
}

fn revisions_descending(directory: &Path) -> Result<Vec<(u64, PathBuf)>, ProfileStoreError> {
    let entries = fs::read_dir(directory).map_err(|source| ProfileStoreError::Io {
        operation: "list profile revisions",
        path: directory.to_owned(),
        source,
    })?;
    let mut revisions = Vec::new();
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
        revisions.push((revision, entry.path()));
    }
    revisions.sort_unstable_by(|left, right| right.0.cmp(&left.0));
    Ok(revisions)
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

/// Publishes a fully synced temporary file without ever replacing an existing
/// immutable revision. A same-directory hard link is an atomic create-if-absent
/// primitive on both Unix and Windows.
fn publish_new_file(temporary: &Path, destination: &Path) -> io::Result<bool> {
    match fs::hard_link(temporary, destination) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists || destination.exists() => {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn sync_directory(directory: &Path) -> io::Result<()> {
    File::open(directory).and_then(|file| file.sync_all())
}

#[cfg(not(unix))]
fn sync_directory(directory: &Path) -> io::Result<()> {
    fs::metadata(directory).map(|_| ())
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc, thread};

    use serde_json::json;
    use tempfile::tempdir;

    use super::{
        ProfileMutation, ProfileStore, ProfileStoreError, ProfileTransaction, publish_new_file,
        revisions_descending, version_filename,
    };

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
    fn existence_probe_is_case_insensitive_and_does_not_allocate_for_unknown_names() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());

        assert!(!store.profile_exists("Unknown").unwrap());
        assert!(store.profile_locks.lock().unwrap().is_empty());
        assert!(store.cache.lock().unwrap().is_empty());

        store.load_or_create("Rider").unwrap();
        assert!(store.profile_exists("rIDER").unwrap());
    }

    #[test]
    fn immutable_revision_publish_never_replaces_an_existing_destination() {
        let root = tempdir().unwrap();
        let temporary = root.path().join(".revision.tmp");
        let destination = root.path().join("Launcher.v1.json");
        fs::write(&temporary, b"new revision").unwrap();
        fs::write(&destination, b"existing revision").unwrap();

        assert!(!publish_new_file(&temporary, &destination).unwrap());
        assert_eq!(fs::read(&destination).unwrap(), b"existing revision");
        assert_eq!(fs::read(&temporary).unwrap(), b"new revision");

        fs::remove_file(&destination).unwrap();
        assert!(publish_new_file(&temporary, &destination).unwrap());
        assert_eq!(fs::read(&destination).unwrap(), b"new revision");
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
    fn unchanged_transaction_does_not_publish_a_revision() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let initial = store.load_or_create("Rider").unwrap();

        let outcome = store
            .transaction("rIDER", |profile| {
                ProfileMutation::Unchanged(profile.rider.lucci)
            })
            .unwrap();
        match outcome {
            ProfileTransaction::Unchanged { value, profile } => {
                assert_eq!(value, initial.profile.rider.lucci);
                assert_eq!(profile, initial.profile);
            }
            other => panic!("expected an unchanged transaction, got {other:?}"),
        }

        let loaded = store.load_or_create("Rider").unwrap();
        assert_eq!(loaded.revision, Some(1));
        assert_eq!(
            revisions_descending(root.path().join("Rider").as_path())
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn changed_transaction_returns_its_value_and_committed_profile() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        store.load_or_create("Rider").unwrap();

        let outcome = store
            .transaction("Rider", |profile| {
                let mut profile = profile.clone();
                profile.rider.lucci += 7;
                ProfileMutation::changed("reward-applied", profile)
            })
            .unwrap();
        match outcome {
            ProfileTransaction::Committed {
                value,
                profile,
                saved,
            } => {
                assert_eq!(value, "reward-applied");
                assert_eq!(profile.rider.lucci, 1_000_007);
                assert_eq!(saved.revision, 2);
                assert_eq!(
                    saved.path,
                    root.path().join("Rider").join(version_filename(2))
                );
            }
            other => panic!("expected a durable commit, got {other:?}"),
        }
        assert_eq!(
            store.load_or_create("Rider").unwrap().profile.rider.lucci,
            1_000_007
        );
    }

    #[test]
    fn duplicate_conditional_mutation_reuses_state_without_a_new_revision() {
        const MARKER: &str = "ConditionalRewardApplied";

        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        store.load_or_create("Rider").unwrap();

        let apply = || {
            store.transaction("Rider", |profile| {
                if profile.extra.contains_key(MARKER) {
                    ProfileMutation::Unchanged(profile.rider.lucci)
                } else {
                    let mut profile = profile.clone();
                    profile.rider.lucci += 25;
                    profile.extra.insert(MARKER.to_owned(), json!(true));
                    ProfileMutation::changed(profile.rider.lucci, profile)
                }
            })
        };
        assert!(matches!(
            apply().unwrap(),
            ProfileTransaction::Committed {
                value: 1_000_025,
                ..
            }
        ));
        assert!(matches!(
            apply().unwrap(),
            ProfileTransaction::Unchanged {
                value: 1_000_025,
                ..
            }
        ));

        let loaded = store.load_or_create("Rider").unwrap();
        assert_eq!(loaded.revision, Some(2));
        assert_eq!(loaded.profile.rider.lucci, 1_000_025);
    }

    #[test]
    fn post_publish_sync_failure_keeps_committed_cache_visible_and_retry_is_a_noop() {
        const MARKER: &str = "SyncFaultRewardApplied";

        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        store.load_or_create("Rider").unwrap();
        store.fail_next_directory_sync(std::io::ErrorKind::Other);

        let mutate_once = || {
            store.transaction("Rider", |profile| {
                if profile.extra.contains_key(MARKER) {
                    ProfileMutation::Unchanged(profile.rider.lucci)
                } else {
                    let mut profile = profile.clone();
                    profile.rider.lucci += 40;
                    profile.extra.insert(MARKER.to_owned(), json!(true));
                    ProfileMutation::changed(profile.rider.lucci, profile)
                }
            })
        };
        let outcome = mutate_once().unwrap();
        match outcome {
            ProfileTransaction::CommittedButDurabilityUncertain {
                value,
                profile,
                saved,
                error:
                    ProfileStoreError::CommittedButDurabilityUncertain {
                        nickname,
                        revision,
                        path,
                        source,
                    },
            } => {
                assert_eq!(value, 1_000_040);
                assert_eq!(profile.rider.lucci, 1_000_040);
                assert_eq!(saved.revision, 2);
                assert_eq!(nickname, "Rider");
                assert_eq!(revision, 2);
                assert_eq!(path, saved.path);
                assert_eq!(source.kind(), std::io::ErrorKind::Other);
            }
            other => panic!("expected a committed durability warning, got {other:?}"),
        }

        let cached = store.load_or_create("rIDER").unwrap();
        assert_eq!(cached.revision, Some(2));
        assert_eq!(cached.profile.rider.lucci, 1_000_040);
        assert!(matches!(
            mutate_once().unwrap(),
            ProfileTransaction::Unchanged {
                value: 1_000_040,
                ..
            }
        ));
        assert_eq!(store.load_or_create("Rider").unwrap().revision, Some(2));

        let from_disk = ProfileStore::new(root.path())
            .load_or_create("Rider")
            .unwrap();
        assert_eq!(from_disk.revision, Some(2));
        assert_eq!(from_disk.profile.rider.lucci, 1_000_040);
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
    fn concurrent_conditional_transactions_share_the_per_name_lock() {
        let root = tempdir().unwrap();
        let store = Arc::new(ProfileStore::new(root.path()));
        store.load_or_create("Rider").unwrap();
        let mut workers = Vec::new();
        for _ in 0..8 {
            let store = Arc::clone(&store);
            workers.push(thread::spawn(move || {
                let outcome = store
                    .transaction("rIDER", |profile| {
                        let mut profile = profile.clone();
                        profile.rider.lucci += 1;
                        ProfileMutation::changed(profile.rider.lucci, profile)
                    })
                    .unwrap();
                assert!(matches!(outcome, ProfileTransaction::Committed { .. }));
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }

        let loaded = store.load_or_create("Rider").unwrap();
        assert_eq!(loaded.profile.rider.lucci, 1_000_008);
        assert_eq!(loaded.revision, Some(9));
    }

    #[test]
    fn cache_is_stable_until_an_explicit_reload() {
        let root = tempdir().unwrap();
        let directory = root.path().join("Rider");
        fs::create_dir_all(&directory).unwrap();
        let legacy = directory.join("Launcher.json");
        fs::write(&legacy, br#"{"Rider":{"Lucci":42}}"#).unwrap();

        let store = ProfileStore::new(root.path());
        assert_eq!(
            store.load_or_create("Rider").unwrap().profile.rider.lucci,
            42
        );
        fs::write(&legacy, br#"{"Rider":{"Lucci":99}}"#).unwrap();
        assert_eq!(
            store.load_or_create("rIDER").unwrap().profile.rider.lucci,
            42
        );
        assert_eq!(store.reload("Rider").unwrap().profile.rider.lucci, 99);
    }

    #[test]
    fn corrupt_latest_revision_falls_back_without_overwriting_it() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let first = store.load_or_create("Rider").unwrap();
        let (second, _) = store
            .update("Rider", |profile| profile.rider.lucci = 77)
            .unwrap();
        fs::write(&second.path, b"{truncated").unwrap();
        drop(store);

        let recovered = ProfileStore::new(root.path())
            .load_or_create("Rider")
            .unwrap();
        assert_eq!(recovered.revision, Some(1));
        assert_eq!(recovered.profile, first.profile);
        assert_eq!(recovered.recovered_revisions, vec![second.path.clone()]);

        let store = ProfileStore::new(root.path());
        let saved = store.save("Rider", &recovered.profile).unwrap();
        assert_eq!(saved.revision, 3);
        assert_eq!(fs::read(second.path).unwrap(), b"{truncated");
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
