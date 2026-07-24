use std::{
    collections::HashMap,
    fmt::Write as _,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    num::NonZeroU64,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

use fs2::FileExt;
use p5136_core::nickname::{NicknameError, canonical_nickname_key, normalize_nickname};
use rand::random;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use thiserror::Error;

use crate::Profile;

const LEGACY_FILENAME: &str = "Launcher.json";
const VERSION_PREFIX: &str = "Launcher.v";
const VERSION_SUFFIX: &str = ".json";
const RACE_RUN_GENERATION_PREFIX: &str = ".P5136RustRaceRun.v";
const RACE_RUN_GENERATION_SUFFIX: &str = ".marker";
const RACE_RUN_LOCK_FILENAME: &str = ".P5136RustRaceRun.lock";
const STORE_ID_FILENAME: &str = ".P5136RustProfileStoreId";
const STORE_ID_HEADER: &str = "P5136-PROFILE-STORE-V1";
const RACE_RUN_MARKER_HEADER: &str = "P5136-RACE-RUN-V1";
const INTERNAL_STORAGE_NICKNAME_PREFIX: &str = ".p5136rust";
const MAX_TRANSACTION_CAS_ATTEMPTS: usize = 16;
const MAX_CREATE_CAS_ATTEMPTS: usize = 16;
const MAX_SAVE_CAS_ATTEMPTS: usize = 16;
const MAX_TEMPORARY_FILE_ATTEMPTS: usize = 128;
const MAX_PROFILE_DIRECTORY_ENTRIES: usize = 4_096;
const MAX_PROFILE_REVISIONS: usize = 1_024;
const PROFILE_REVISION_RETENTION: usize = 64;
const MAX_PROFILE_ROOT_ENTRIES: usize = 16_384;
const MAX_RACE_RUN_GENERATION_MARKERS: usize = 64;
const MAX_STORE_METADATA_BYTES: u64 = 256;
const MAX_RACE_RUN_MARKER_BYTES: u64 = 256;
static TEMPORARY_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(1);
pub const DEFAULT_MAX_PROFILE_BYTES: u64 = 16 * 1024 * 1024;

/// The durable identity of one profile-store root.
///
/// This value is persisted once in the root and copied into current reward
/// receipts. Runtime authorization still requires a live [`RaceRunLease`];
/// deserializing this identifier does not create that capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ProfileStoreId([u8; 16]);

impl ProfileStoreId {
    fn new(bytes: [u8; 16]) -> Option<Self> {
        (bytes != [0; 16]).then_some(Self(bytes))
    }

    #[must_use]
    pub const fn get(self) -> [u8; 16] {
        self.0
    }
}

impl<'de> Deserialize<'de> for ProfileStoreId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes = <[u8; 16]>::deserialize(deserializer)?;
        Self::new(bytes).ok_or_else(|| D::Error::custom("profile store ID must not be all zero"))
    }
}

/// A strictly ordered server-run generation allocated durably from one
/// profile-store root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RaceRunGeneration(NonZeroU64);

impl RaceRunGeneration {
    #[must_use]
    pub(crate) const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// A process-lifetime, root-bound capability for one active server run.
///
/// The underlying OS-exclusive file lock is retained until this value is
/// dropped. A profile root intentionally supports exactly one active server;
/// active-active deployments need a transactional database backend instead.
#[derive(Debug)]
pub struct RaceRunLease {
    generation: RaceRunGeneration,
    store_id: ProfileStoreId,
    canonical_root: PathBuf,
    lock_file: File,
}

impl RaceRunLease {
    #[must_use]
    pub const fn generation(&self) -> RaceRunGeneration {
        self.generation
    }

    #[must_use]
    pub const fn store_id(&self) -> ProfileStoreId {
        self.store_id
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.canonical_root
    }
}

impl Drop for RaceRunLease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.lock_file);
    }
}

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
        "profile transaction for {nickname} could not win a revision compare-and-swap after {attempts} attempts"
    )]
    TransactionContention { nickname: String, attempts: usize },

    #[error(
        "profile creation for {nickname} could not win a revision compare-and-swap after {attempts} attempts"
    )]
    ProfileCreationContention { nickname: String, attempts: usize },

    #[error(
        "profile save for {nickname} could not win a revision compare-and-swap after {attempts} attempts"
    )]
    ProfileSaveContention { nickname: String, attempts: usize },

    #[error("temporary file allocation in {directory} exhausted after {attempts} attempts")]
    TemporaryFileContention { directory: PathBuf, attempts: usize },

    #[error("{directory} contains more than the supported {maximum} directory entries")]
    DirectoryEntryLimitExceeded { directory: PathBuf, maximum: usize },

    #[error("{directory} contains more than the supported {maximum} immutable profile revisions")]
    ProfileRevisionLimitExceeded { directory: PathBuf, maximum: usize },

    #[error("profile storage entry at {path} is invalid: {reason}")]
    InvalidStorageEntry { path: PathBuf, reason: &'static str },

    #[error("nickname {nickname:?} is reserved for profile-store metadata")]
    ReservedStorageNickname { nickname: String },

    #[error(
        "canonical nickname {nickname:?} maps to both profile directories {first} and {second}"
    )]
    AmbiguousProfileDirectories {
        nickname: String,
        first: PathBuf,
        second: PathBuf,
    },

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

    #[error("race run generation counter is exhausted at {path}")]
    RaceRunGenerationExhausted { path: PathBuf },

    #[error("another server already holds the race-run lease for profile root {root}")]
    RaceRunLeaseBusy { root: PathBuf },

    #[error("the profile store at {root} does not support atomic same-directory hard links")]
    AtomicPublishUnsupported {
        root: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("profile-store metadata at {path} is invalid: {reason}")]
    InvalidStoreMetadata { path: PathBuf, reason: &'static str },

    #[error("race-run generation marker at {path} is invalid: {reason}")]
    InvalidRaceRunGenerationMarker { path: PathBuf, reason: &'static str },

    #[error("profile store root {used_with} does not match lease root {issued_for}")]
    RaceRunLeaseStoreMismatch {
        issued_for: PathBuf,
        used_with: PathBuf,
    },

    #[error(
        "profile store identity changed while a race-run lease was active (expected {expected:?}, found {actual:?})"
    )]
    ProfileStoreIdentityChanged {
        expected: ProfileStoreId,
        actual: ProfileStoreId,
    },

    #[error("internal profile-store invariant failed: {message}")]
    InternalInvariant { message: &'static str },

    #[error(
        "race run generation {generation} was published at {path}, but root-directory durability could not be confirmed"
    )]
    RaceRunGenerationDurabilityUncertain {
        generation: u64,
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
struct DiskProfileSnapshot {
    profile: Profile,
    revision: Option<u64>,
    head_revision: Option<u64>,
    source_path: PathBuf,
    recovered_revisions: Vec<PathBuf>,
    directory: PathBuf,
}

#[derive(Debug)]
pub struct ProfileStore {
    root: PathBuf,
    maximum_bytes: u64,
    profile_locks: Mutex<HashMap<String, Weak<Mutex<()>>>>,
    #[cfg(test)]
    next_directory_sync_fault: Mutex<Option<io::ErrorKind>>,
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
            #[cfg(test)]
            next_directory_sync_fault: Mutex::new(None),
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn normalize_storage_nickname(nickname: &str) -> Result<String, ProfileStoreError> {
        let nickname = normalize_nickname(nickname)?;
        if canonical_nickname_key(&nickname).starts_with(INTERNAL_STORAGE_NICKNAME_PREFIX) {
            return Err(ProfileStoreError::ReservedStorageNickname { nickname });
        }
        Ok(nickname)
    }

    /// Acquires the process-lifetime server lease and allocates its generation.
    ///
    /// The lease uses a safe OS-exclusive file lock and must be retained until
    /// the World coordinator has stopped. Generation allocation, marker
    /// validation, and a same-directory hard-link capability probe all happen
    /// while the lock is held. This storage backend is intended for local
    /// filesystems whose lock, hard-link, and `fsync` behavior matches the host
    /// OS contract; unsupported filesystems fail startup explicitly.
    pub fn acquire_race_run_lease(&self) -> Result<RaceRunLease, ProfileStoreError> {
        create_dir_all_durable(&self.root)?;
        let canonical_root =
            fs::canonicalize(&self.root).map_err(|source| ProfileStoreError::Io {
                operation: "canonicalize profile root",
                path: self.root.clone(),
                source,
            })?;
        let lock_path = canonical_root.join(RACE_RUN_LOCK_FILENAME);
        let (lock_file, created) = open_or_create_lock_file(&lock_path)?;
        if created {
            lock_file
                .sync_all()
                .map_err(|source| ProfileStoreError::Io {
                    operation: "sync race-run lock file",
                    path: lock_path.clone(),
                    source,
                })?;
            sync_directory(&canonical_root).map_err(|source| ProfileStoreError::Io {
                operation: "sync race-run lock directory entry",
                path: canonical_root.clone(),
                source,
            })?;
        }
        match lock_file.try_lock_exclusive() {
            Ok(()) => {}
            Err(source) if is_lock_contended(&source) => {
                return Err(ProfileStoreError::RaceRunLeaseBusy {
                    root: canonical_root,
                });
            }
            Err(source) => {
                return Err(ProfileStoreError::Io {
                    operation: "acquire exclusive race-run lease",
                    path: lock_path,
                    source,
                });
            }
        }

        verify_atomic_publish_support(&canonical_root)?;
        let store_id = load_or_create_store_id(&canonical_root)?;
        let generation = self.allocate_race_run_generation_locked(&canonical_root, store_id)?;
        Ok(RaceRunLease {
            generation,
            store_id,
            canonical_root,
            lock_file,
        })
    }

    pub(crate) fn validate_race_run_lease(
        &self,
        lease: &RaceRunLease,
    ) -> Result<(), ProfileStoreError> {
        create_dir_all_durable(&self.root)?;
        let canonical_root =
            fs::canonicalize(&self.root).map_err(|source| ProfileStoreError::Io {
                operation: "canonicalize profile root",
                path: self.root.clone(),
                source,
            })?;
        if canonical_root != lease.canonical_root {
            return Err(ProfileStoreError::RaceRunLeaseStoreMismatch {
                issued_for: lease.canonical_root.clone(),
                used_with: canonical_root,
            });
        }
        let actual = read_store_id(&lease.canonical_root)?;
        if actual != lease.store_id {
            return Err(ProfileStoreError::ProfileStoreIdentityChanged {
                expected: lease.store_id,
                actual,
            });
        }
        Ok(())
    }

    fn allocate_race_run_generation_locked(
        &self,
        root: &Path,
        store_id: ProfileStoreId,
    ) -> Result<RaceRunGeneration, ProfileStoreError> {
        let generations = race_run_generations(root, store_id)?;
        let current = generations.last().map(|(generation, _)| *generation);
        compact_race_run_generations_before_publish(root, &generations, current)?;
        let next = match current {
            Some(current) => current.get().checked_add(1).ok_or_else(|| {
                ProfileStoreError::RaceRunGenerationExhausted {
                    path: root.to_owned(),
                }
            })?,
            None => 1,
        };
        let Some(generation) = RaceRunGeneration::new(next) else {
            return Err(ProfileStoreError::InternalInvariant {
                message: "checked race-run generation successor must be nonzero",
            });
        };
        let final_name = race_run_generation_filename(generation);
        let final_path = root.join(&final_name);
        let marker = race_run_marker_bytes(store_id, generation);
        let temporary_path = write_unique_temporary(root, &final_name, &marker)?;
        match publish_new_file(&temporary_path, &final_path) {
            Ok(true) => {
                let _ = fs::remove_file(&temporary_path);
                if let Err(source) = self.sync_published_directory(root) {
                    return Err(ProfileStoreError::RaceRunGenerationDurabilityUncertain {
                        generation: generation.get(),
                        path: final_path,
                        source,
                    });
                }
                Ok(generation)
            }
            Ok(false) => {
                let _ = fs::remove_file(&temporary_path);
                Err(ProfileStoreError::InternalInvariant {
                    message: "exclusive lease allocated an existing race-run generation",
                })
            }
            Err(source) => {
                let _ = fs::remove_file(&temporary_path);
                Err(ProfileStoreError::Io {
                    operation: "publish race run generation",
                    path: final_path,
                    source,
                })
            }
        }
    }

    /// Explicitly re-syncs the directory containing the newest valid profile
    /// revision.
    ///
    /// This is also performed automatically for every unchanged transaction.
    /// It lets an exact-key retry confirm a prior
    /// [`ProfileStoreError::CommittedButDurabilityUncertain`] outcome without
    /// publishing another revision.
    pub fn confirm_latest_revision_durable(
        &self,
        nickname: &str,
    ) -> Result<Option<SavedProfile>, ProfileStoreError> {
        let nickname = Self::normalize_storage_nickname(nickname)?;
        let profile_lock = self.profile_lock(&nickname)?;
        let _guard = lock(&profile_lock)?;
        let snapshot = self.load_or_default_from_disk(&nickname)?;
        self.confirm_snapshot_durability(&nickname, &snapshot)
    }

    /// Loads the newest immutable revision, imports legacy `Launcher.json`, or
    /// creates revision one from the P5136 defaults.
    pub fn load_or_create(&self, nickname: &str) -> Result<LoadedProfile, ProfileStoreError> {
        let nickname = Self::normalize_storage_nickname(nickname)?;
        let profile_lock = self.profile_lock(&nickname)?;
        let _guard = lock(&profile_lock)?;
        self.load_or_create_locked(&nickname)
    }

    /// Checks whether an operator-provisioned profile directory exists without
    /// creating a per-nickname lock or any filesystem state.
    pub fn profile_exists(&self, nickname: &str) -> Result<bool, ProfileStoreError> {
        let nickname = Self::normalize_storage_nickname(nickname)?;
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
        let mut matched: Option<PathBuf> = None;
        for (index, entry) in entries.enumerate() {
            if index >= MAX_PROFILE_ROOT_ENTRIES {
                return Err(ProfileStoreError::DirectoryEntryLimitExceeded {
                    directory: self.root.clone(),
                    maximum: MAX_PROFILE_ROOT_ENTRIES,
                });
            }
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
                let path = entry.path();
                if let Some(first) = matched.as_ref() {
                    return Err(ProfileStoreError::AmbiguousProfileDirectories {
                        nickname,
                        first: first.clone(),
                        second: path,
                    });
                }
                matched = Some(path);
            }
        }
        Ok(matched.is_some())
    }

    /// Retained for API compatibility. Loads are always fresh from disk, so
    /// invalidation only validates and serializes with an in-process update.
    pub fn invalidate(&self, nickname: &str) -> Result<(), ProfileStoreError> {
        let nickname = Self::normalize_storage_nickname(nickname)?;
        let profile_lock = self.profile_lock(&nickname)?;
        let _guard = lock(&profile_lock)?;
        Ok(())
    }

    pub fn reload(&self, nickname: &str) -> Result<LoadedProfile, ProfileStoreError> {
        self.load_or_create(nickname)
    }

    /// Writes the supplied snapshot as a new immutable revision. An existing
    /// revision is never overwritten.
    ///
    /// This is an unconditional append, not an atomic read-modify-write. Use
    /// [`Self::transaction`] when the new value depends on current profile
    /// state shared with other store instances or processes.
    pub fn save(
        &self,
        nickname: &str,
        profile: &Profile,
    ) -> Result<SavedProfile, ProfileStoreError> {
        let nickname = Self::normalize_storage_nickname(nickname)?;
        let profile_lock = self.profile_lock(&nickname)?;
        let _guard = lock(&profile_lock)?;
        match self.save_locked(&nickname, profile)? {
            SaveOutcome::Durable(saved) => Ok(saved),
            SaveOutcome::DurabilityUncertain { error, .. } => Err(error),
        }
    }

    /// Atomically updates one case-insensitive nickname while allowing
    /// unrelated profiles to progress independently.
    ///
    /// Like [`Self::transaction`], `update` may call its closure again after a
    /// cross-instance or cross-process compare-and-swap conflict, so the
    /// closure must not have externally visible side effects.
    pub fn update<F>(
        &self,
        nickname: &str,
        mut update: F,
    ) -> Result<(SavedProfile, Profile), ProfileStoreError>
    where
        F: FnMut(&mut Profile),
    {
        match self.transaction(nickname, |profile| {
            let mut profile = profile.clone();
            update(&mut profile);
            ProfileMutation::changed((), profile)
        })? {
            ProfileTransaction::Committed { profile, saved, .. } => Ok((saved, profile)),
            ProfileTransaction::CommittedButDurabilityUncertain { error, .. } => Err(error),
            ProfileTransaction::Unchanged { .. } => Err(ProfileStoreError::InternalInvariant {
                message: "an unconditional profile update must request a commit",
            }),
        }
    }

    /// Runs one optimistic conditional read-modify-write transaction.
    ///
    /// The closure sees an immutable disk snapshot and may be called more than
    /// once if another store or process wins the next immutable revision.
    /// Therefore it must not perform externally visible side effects. A
    /// changed transaction publishes only when its expected head revision
    /// still wins the create-if-absent compare-and-swap.
    ///
    /// `ProfileMutation::Unchanged` does not publish a revision, but it does
    /// explicitly sync the containing directory. A failure is returned as
    /// `CommittedButDurabilityUncertain` for the loaded revision, preserving a
    /// previous uncertain commit until a later retry confirms durability.
    pub fn transaction<T, F>(
        &self,
        nickname: &str,
        mut transaction: F,
    ) -> Result<ProfileTransaction<T>, ProfileStoreError>
    where
        F: FnMut(&Profile) -> ProfileMutation<T>,
    {
        let nickname = Self::normalize_storage_nickname(nickname)?;
        let profile_lock = self.profile_lock(&nickname)?;
        let _guard = lock(&profile_lock)?;
        for _ in 0..MAX_TRANSACTION_CAS_ATTEMPTS {
            let snapshot = self.load_or_default_from_disk(&nickname)?;
            match transaction(&snapshot.profile) {
                ProfileMutation::Unchanged(value) => {
                    let durability = self.confirm_snapshot_durability(&nickname, &snapshot);
                    return match durability {
                        Ok(_) => Ok(ProfileTransaction::Unchanged {
                            value,
                            profile: snapshot.profile,
                        }),
                        Err(error @ ProfileStoreError::CommittedButDurabilityUncertain { .. }) => {
                            let Some(saved) = saved_profile_from_snapshot(&nickname, &snapshot)
                            else {
                                return Err(ProfileStoreError::InternalInvariant {
                                    message: "a committed durability warning must reference a revision",
                                });
                            };
                            Ok(ProfileTransaction::CommittedButDurabilityUncertain {
                                value,
                                profile: snapshot.profile,
                                saved,
                                error,
                            })
                        }
                        Err(error) => Err(error),
                    };
                }
                ProfileMutation::Changed { value, profile } => {
                    let profile = *profile;
                    match self.save_compare_and_swap_locked(
                        &nickname,
                        &profile,
                        snapshot.head_revision,
                        snapshot.revision,
                    )? {
                        PreparedPublishOutcome::Published(SaveOutcome::Durable(saved)) => {
                            return Ok(ProfileTransaction::Committed {
                                value,
                                profile,
                                saved,
                            });
                        }
                        PreparedPublishOutcome::Published(SaveOutcome::DurabilityUncertain {
                            saved,
                            error,
                        }) => {
                            return Ok(ProfileTransaction::CommittedButDurabilityUncertain {
                                value,
                                profile,
                                saved,
                                error,
                            });
                        }
                        PreparedPublishOutcome::DestinationExists => std::thread::yield_now(),
                    }
                }
            }
        }
        Err(ProfileStoreError::TransactionContention {
            nickname,
            attempts: MAX_TRANSACTION_CAS_ATTEMPTS,
        })
    }

    fn profile_lock(&self, nickname: &str) -> Result<Arc<Mutex<()>>, ProfileStoreError> {
        let mut locks = self
            .profile_locks
            .lock()
            .map_err(|_| ProfileStoreError::LockPoisoned)?;
        locks.retain(|_, profile_lock| profile_lock.strong_count() != 0);
        let key = canonical_nickname_key(nickname);
        if let Some(profile_lock) = locks.get(&key).and_then(Weak::upgrade) {
            return Ok(profile_lock);
        }
        let profile_lock = Arc::new(Mutex::new(()));
        locks.insert(key, Arc::downgrade(&profile_lock));
        Ok(profile_lock)
    }

    fn load_or_create_locked(&self, nickname: &str) -> Result<LoadedProfile, ProfileStoreError> {
        for _ in 0..MAX_CREATE_CAS_ATTEMPTS {
            let snapshot = self.load_or_default_from_disk(nickname)?;
            let source_exists =
                snapshot
                    .source_path
                    .try_exists()
                    .map_err(|source| ProfileStoreError::Io {
                        operation: "check loaded profile source",
                        path: snapshot.source_path.clone(),
                        source,
                    })?;
            if source_exists {
                self.confirm_snapshot_read_durability(nickname, &snapshot)?;
                return Ok(LoadedProfile {
                    nickname: nickname.to_owned(),
                    profile: snapshot.profile,
                    revision: snapshot.revision,
                    source_path: snapshot.source_path,
                    created: false,
                    recovered_revisions: snapshot.recovered_revisions,
                });
            }

            match self.save_compare_and_swap_locked(
                nickname,
                &snapshot.profile,
                snapshot.head_revision,
                snapshot.revision,
            )? {
                PreparedPublishOutcome::Published(SaveOutcome::Durable(saved)) => {
                    return Ok(LoadedProfile {
                        nickname: nickname.to_owned(),
                        profile: snapshot.profile,
                        revision: Some(saved.revision),
                        source_path: saved.path,
                        created: true,
                        recovered_revisions: snapshot.recovered_revisions,
                    });
                }
                PreparedPublishOutcome::Published(SaveOutcome::DurabilityUncertain {
                    error,
                    ..
                }) => return Err(error),
                PreparedPublishOutcome::DestinationExists => std::thread::yield_now(),
            }
        }
        Err(ProfileStoreError::ProfileCreationContention {
            nickname: nickname.to_owned(),
            attempts: MAX_CREATE_CAS_ATTEMPTS,
        })
    }

    fn load_or_default_from_disk(
        &self,
        nickname: &str,
    ) -> Result<DiskProfileSnapshot, ProfileStoreError> {
        let directory = self.profile_directory(nickname)?;
        create_dir_all_durable(&directory)?;
        let revisions = revisions_descending(&directory)?;
        let head_revision = revisions.first().map(|(revision, _)| *revision);
        let mut recovered_revisions = Vec::new();
        let mut first_corruption = None;
        for (revision, path) in revisions {
            match self.read_profile(&path) {
                Ok(profile) => {
                    return Ok(DiskProfileSnapshot {
                        profile,
                        revision: Some(revision),
                        head_revision,
                        source_path: path,
                        recovered_revisions,
                        directory,
                    });
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
        if regular_profile_file_exists(&legacy)? {
            return Ok(DiskProfileSnapshot {
                profile: self.read_profile(&legacy)?,
                revision: None,
                head_revision,
                source_path: legacy,
                recovered_revisions,
                directory,
            });
        }
        if let Some(error) = first_corruption {
            return Err(error);
        }
        Ok(DiskProfileSnapshot {
            profile: Profile::default(),
            revision: None,
            head_revision,
            source_path: legacy,
            recovered_revisions,
            directory,
        })
    }

    fn save_locked(
        &self,
        nickname: &str,
        profile: &Profile,
    ) -> Result<SaveOutcome, ProfileStoreError> {
        for _ in 0..MAX_SAVE_CAS_ATTEMPTS {
            let snapshot = self.load_or_default_from_disk(nickname)?;
            match self.save_compare_and_swap_locked(
                nickname,
                profile,
                snapshot.head_revision,
                snapshot.revision,
            )? {
                PreparedPublishOutcome::Published(outcome) => return Ok(outcome),
                PreparedPublishOutcome::DestinationExists => std::thread::yield_now(),
            }
        }
        Err(ProfileStoreError::ProfileSaveContention {
            nickname: nickname.to_owned(),
            attempts: MAX_SAVE_CAS_ATTEMPTS,
        })
    }

    fn save_compare_and_swap_locked(
        &self,
        nickname: &str,
        profile: &Profile,
        expected_head_revision: Option<u64>,
        protected_source_revision: Option<u64>,
    ) -> Result<PreparedPublishOutcome, ProfileStoreError> {
        let directory = self.profile_directory(nickname)?;
        create_dir_all_durable(&directory)?;
        let revision = match expected_head_revision {
            Some(revision) => revision
                .checked_add(1)
                .ok_or_else(|| ProfileStoreError::RevisionExhausted(nickname.to_owned()))?,
            None => 1,
        };
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

        let final_path = directory.join(version_filename(revision));
        let temporary_path =
            write_unique_temporary(&directory, &version_filename(revision), &bytes)?;

        let current_revisions = revisions_descending(&directory)?;
        let current_head = current_revisions.first().map(|(revision, _)| *revision);
        if current_head != expected_head_revision {
            let _ = fs::remove_file(&temporary_path);
            return Ok(PreparedPublishOutcome::DestinationExists);
        }
        if let Err(error) =
            precompact_profile_revisions(&directory, &current_revisions, protected_source_revision)
        {
            let _ = fs::remove_file(&temporary_path);
            return Err(error);
        }

        self.publish_prepared_revision(nickname, revision, &directory, &temporary_path, &final_path)
    }

    fn publish_prepared_revision(
        &self,
        nickname: &str,
        revision: u64,
        directory: &Path,
        temporary_path: &Path,
        final_path: &Path,
    ) -> Result<PreparedPublishOutcome, ProfileStoreError> {
        match publish_new_file(temporary_path, final_path) {
            Ok(true) => {
                let saved = SavedProfile {
                    nickname: nickname.to_owned(),
                    revision,
                    path: final_path.to_owned(),
                };
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
                let _ = fs::remove_file(temporary_path);
                Ok(PreparedPublishOutcome::DestinationExists)
            }
            Err(source) => {
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
    pub(crate) fn fail_next_directory_sync(&self, kind: io::ErrorKind) {
        *self.next_directory_sync_fault.lock().unwrap() = Some(kind);
    }

    fn confirm_snapshot_durability(
        &self,
        nickname: &str,
        snapshot: &DiskProfileSnapshot,
    ) -> Result<Option<SavedProfile>, ProfileStoreError> {
        let Some(saved) = saved_profile_from_snapshot(nickname, snapshot) else {
            return Ok(None);
        };
        self.sync_published_directory(&snapshot.directory)
            .map_err(
                |source| ProfileStoreError::CommittedButDurabilityUncertain {
                    nickname: nickname.to_owned(),
                    revision: saved.revision,
                    path: saved.path.clone(),
                    source,
                },
            )?;
        Ok(Some(saved))
    }

    fn confirm_snapshot_read_durability(
        &self,
        nickname: &str,
        snapshot: &DiskProfileSnapshot,
    ) -> Result<(), ProfileStoreError> {
        if saved_profile_from_snapshot(nickname, snapshot).is_some() {
            self.confirm_snapshot_durability(nickname, snapshot)?;
            return Ok(());
        }
        self.sync_published_directory(&snapshot.directory)
            .map_err(|source| ProfileStoreError::Io {
                operation: "sync profile directory before returning a legacy profile",
                path: snapshot.directory.clone(),
                source,
            })
    }

    fn read_profile(&self, path: &Path) -> Result<Profile, ProfileStoreError> {
        if !regular_profile_file_exists(path)? {
            return Err(ProfileStoreError::Io {
                operation: "open profile",
                path: path.to_owned(),
                source: io::Error::new(
                    io::ErrorKind::NotFound,
                    "profile file disappeared before it could be opened",
                ),
            });
        }
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
        create_dir_all_durable(&self.root)?;
        let requested_key = canonical_nickname_key(nickname);
        let entries = fs::read_dir(&self.root).map_err(|source| ProfileStoreError::Io {
            operation: "list profile root",
            path: self.root.clone(),
            source,
        })?;
        let mut matched: Option<PathBuf> = None;
        for (index, entry) in entries.enumerate() {
            if index >= MAX_PROFILE_ROOT_ENTRIES {
                return Err(ProfileStoreError::DirectoryEntryLimitExceeded {
                    directory: self.root.clone(),
                    maximum: MAX_PROFILE_ROOT_ENTRIES,
                });
            }
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
                let path = entry.path();
                if let Some(first) = matched.as_ref() {
                    return Err(ProfileStoreError::AmbiguousProfileDirectories {
                        nickname: nickname.to_owned(),
                        first: first.clone(),
                        second: path,
                    });
                }
                matched = Some(path);
            }
        }
        Ok(matched.unwrap_or_else(|| self.root.join(requested_key)))
    }
}

fn lock(value: &Mutex<()>) -> Result<MutexGuard<'_, ()>, ProfileStoreError> {
    value.lock().map_err(|_| ProfileStoreError::LockPoisoned)
}

fn saved_profile_from_snapshot(
    nickname: &str,
    snapshot: &DiskProfileSnapshot,
) -> Option<SavedProfile> {
    Some(SavedProfile {
        nickname: nickname.to_owned(),
        revision: snapshot.revision?,
        path: snapshot.source_path.clone(),
    })
}

fn regular_profile_file_exists(path: &Path) -> Result<bool, ProfileStoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => Err(ProfileStoreError::InvalidStorageEntry {
            path: path.to_owned(),
            reason: "profile data must be a regular non-symbolic-link file",
        }),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(ProfileStoreError::Io {
            operation: "inspect profile data entry",
            path: path.to_owned(),
            source,
        }),
    }
}

fn create_dir_all_durable(path: &Path) -> Result<(), ProfileStoreError> {
    create_dir_all_durable_with(path, sync_directory)
}

fn create_dir_all_durable_with<F>(path: &Path, mut sync: F) -> Result<(), ProfileStoreError>
where
    F: FnMut(&Path) -> io::Result<()>,
{
    let absolute = std::path::absolute(path).map_err(|source| ProfileStoreError::Io {
        operation: "resolve profile directory",
        path: path.to_owned(),
        source,
    })?;
    let mut missing = Vec::new();
    let mut cursor = absolute.as_path();
    loop {
        match fs::symlink_metadata(cursor) {
            Ok(metadata) if metadata.file_type().is_dir() => break,
            Ok(_) => {
                return Err(ProfileStoreError::InvalidStorageEntry {
                    path: cursor.to_owned(),
                    reason: "expected a real directory, not a file or symbolic link",
                });
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                missing.push(cursor.to_owned());
                let Some(parent) = cursor.parent() else {
                    return Err(ProfileStoreError::Io {
                        operation: "find existing profile directory ancestor",
                        path: cursor.to_owned(),
                        source,
                    });
                };
                cursor = parent;
            }
            Err(source) => {
                return Err(ProfileStoreError::Io {
                    operation: "inspect profile directory",
                    path: cursor.to_owned(),
                    source,
                });
            }
        }
    }

    // The existing anchor may be the directory left behind by a prior call
    // whose mkdir succeeded but whose parent sync failed. Re-syncing its
    // parent on every call makes that failure idempotently recoverable. Since
    // this helper returns at the first failed component sync, at most this
    // nearest existing anchor can be awaiting parent-entry confirmation.
    if let Some(parent) = cursor.parent() {
        sync(parent).map_err(|source| ProfileStoreError::Io {
            operation: "sync existing profile directory anchor",
            path: parent.to_owned(),
            source,
        })?;
    }

    for directory in missing.iter().rev() {
        match fs::create_dir(directory) {
            Ok(()) => {}
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(ProfileStoreError::Io {
                    operation: "create profile directory",
                    path: directory.clone(),
                    source,
                });
            }
        }
        let metadata = fs::symlink_metadata(directory).map_err(|source| ProfileStoreError::Io {
            operation: "inspect created profile directory",
            path: directory.clone(),
            source,
        })?;
        if !metadata.file_type().is_dir() {
            return Err(ProfileStoreError::InvalidStorageEntry {
                path: directory.clone(),
                reason: "a file or symbolic link won the directory creation race",
            });
        }
        let Some(parent) = directory.parent() else {
            return Err(ProfileStoreError::InternalInvariant {
                message: "a newly created directory must have a parent",
            });
        };
        sync(parent).map_err(|source| ProfileStoreError::Io {
            operation: "sync newly created directory entry",
            path: parent.to_owned(),
            source,
        })?;
    }
    Ok(())
}

fn revisions_descending(directory: &Path) -> Result<Vec<(u64, PathBuf)>, ProfileStoreError> {
    cleanup_published_temporary_files(directory, MAX_PROFILE_DIRECTORY_ENTRIES)?;
    let entries = fs::read_dir(directory).map_err(|source| ProfileStoreError::Io {
        operation: "list profile revisions",
        path: directory.to_owned(),
        source,
    })?;
    let mut revisions = Vec::new();
    for (index, entry) in entries.enumerate() {
        if index >= MAX_PROFILE_DIRECTORY_ENTRIES {
            return Err(ProfileStoreError::DirectoryEntryLimitExceeded {
                directory: directory.to_owned(),
                maximum: MAX_PROFILE_DIRECTORY_ENTRIES,
            });
        }
        let entry = entry.map_err(|source| ProfileStoreError::Io {
            operation: "read profile directory entry",
            path: directory.to_owned(),
            source,
        })?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(revision) = parse_revision(&name) else {
            if has_ascii_case_insensitive_prefix(&name, VERSION_PREFIX) {
                return Err(ProfileStoreError::InvalidStorageEntry {
                    path: entry.path(),
                    reason: "reserved profile revision filename is malformed or noncanonical",
                });
            }
            continue;
        };
        let path = entry.path();
        let file_type = entry.file_type().map_err(|source| ProfileStoreError::Io {
            operation: "inspect profile revision entry",
            path: path.clone(),
            source,
        })?;
        if !file_type.is_file() {
            return Err(ProfileStoreError::InvalidStorageEntry {
                path,
                reason: "profile revision must be a regular non-symbolic-link file",
            });
        }
        if revisions.len() >= MAX_PROFILE_REVISIONS {
            return Err(ProfileStoreError::ProfileRevisionLimitExceeded {
                directory: directory.to_owned(),
                maximum: MAX_PROFILE_REVISIONS,
            });
        }
        revisions.push((revision, path));
    }
    revisions.sort_unstable_by(|left, right| right.0.cmp(&left.0));
    Ok(revisions)
}

fn precompact_profile_revisions(
    directory: &Path,
    revisions: &[(u64, PathBuf)],
    protected_source_revision: Option<u64>,
) -> Result<(), ProfileStoreError> {
    let mut directory_changed = false;
    // Leave one slot for the revision that will be published only after this
    // deletion set and its directory entry removals are durable. If loading
    // recovered through a corrupt tail, keep that exact valid source until
    // the replacement revision has been published.
    let old_revision_slots = PROFILE_REVISION_RETENTION.saturating_sub(1);
    let protected_position = protected_source_revision.and_then(|protected| {
        revisions
            .iter()
            .position(|(revision, _)| *revision == protected)
    });
    let newest_slots = if protected_position.is_some_and(|position| position >= old_revision_slots)
    {
        old_revision_slots.saturating_sub(1)
    } else {
        old_revision_slots
    };
    for (index, (revision, path)) in revisions.iter().enumerate() {
        if index < newest_slots || Some(*revision) == protected_source_revision {
            continue;
        }
        match fs::remove_file(path) {
            Ok(()) => directory_changed = true,
            Err(source) if source.kind() == io::ErrorKind::NotFound => directory_changed = true,
            Err(source) => {
                return Err(ProfileStoreError::Io {
                    operation: "pre-compact immutable profile revision",
                    path: path.clone(),
                    source,
                });
            }
        }
    }
    if directory_changed {
        sync_directory(directory).map_err(|source| ProfileStoreError::Io {
            operation: "sync pre-compacted immutable profile revisions",
            path: directory.to_owned(),
            source,
        })?;
    }
    Ok(())
}

fn parse_revision(filename: &str) -> Option<u64> {
    let revision = filename
        .strip_prefix(VERSION_PREFIX)?
        .strip_suffix(VERSION_SUFFIX)?
        .parse()
        .ok()?;
    (revision != 0 && filename == version_filename(revision)).then_some(revision)
}

fn has_ascii_case_insensitive_prefix(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
}

fn version_filename(revision: u64) -> String {
    format!("{VERSION_PREFIX}{revision:020}{VERSION_SUFFIX}")
}

fn race_run_generation_filename(generation: RaceRunGeneration) -> String {
    format!(
        "{RACE_RUN_GENERATION_PREFIX}{:020}{RACE_RUN_GENERATION_SUFFIX}",
        generation.get()
    )
}

fn parse_race_run_generation(filename: &str) -> Option<RaceRunGeneration> {
    let value = filename
        .strip_prefix(RACE_RUN_GENERATION_PREFIX)?
        .strip_suffix(RACE_RUN_GENERATION_SUFFIX)?
        .parse()
        .ok()?;
    RaceRunGeneration::new(value)
}

fn cleanup_published_temporary_files(
    directory: &Path,
    maximum_entries: usize,
) -> Result<(), ProfileStoreError> {
    // A temporary whose immutable destination already exists can no longer
    // win its CAS, so removing it is safe even if its writer is still alive.
    // An unpublished temporary is indistinguishable from a paused
    // cross-process writer and is intentionally retained; the bounded
    // directory-entry limit turns repeated crash debris into a typed
    // maintenance failure rather than unsafe automatic deletion.
    let entries = fs::read_dir(directory).map_err(|source| ProfileStoreError::Io {
        operation: "list temporary immutable files",
        path: directory.to_owned(),
        source,
    })?;
    let mut removed = false;
    for (index, entry) in entries.enumerate() {
        if index >= maximum_entries {
            return Err(ProfileStoreError::DirectoryEntryLimitExceeded {
                directory: directory.to_owned(),
                maximum: maximum_entries,
            });
        }
        let entry = entry.map_err(|source| ProfileStoreError::Io {
            operation: "read temporary immutable file entry",
            path: directory.to_owned(),
            source,
        })?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(destination_name) = published_temporary_destination(&name) else {
            continue;
        };
        let destination = directory.join(destination_name);
        match fs::symlink_metadata(&destination) {
            Ok(_) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(ProfileStoreError::Io {
                    operation: "inspect temporary immutable file destination",
                    path: destination,
                    source,
                });
            }
        }
        let path = entry.path();
        let file_type = entry.file_type().map_err(|source| ProfileStoreError::Io {
            operation: "inspect temporary immutable file",
            path: path.clone(),
            source,
        })?;
        if !file_type.is_file() {
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => removed = true,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(ProfileStoreError::Io {
                    operation: "clean published temporary immutable file",
                    path,
                    source,
                });
            }
        }
    }
    if removed {
        sync_directory(directory).map_err(|source| ProfileStoreError::Io {
            operation: "sync published temporary immutable file cleanup",
            path: directory.to_owned(),
            source,
        })?;
    }
    Ok(())
}

fn published_temporary_destination(filename: &str) -> Option<&str> {
    let body = filename.strip_prefix('.')?.strip_suffix(".tmp")?;
    let (body, sequence) = body.rsplit_once('.')?;
    let (destination, process_id) = body.rsplit_once('.')?;
    sequence.parse::<u64>().ok()?;
    process_id.parse::<u32>().ok()?;
    (parse_revision(destination).is_some()
        || parse_race_run_generation(destination).is_some()
        || destination == STORE_ID_FILENAME)
        .then_some(destination)
}

fn open_or_create_lock_file(path: &Path) -> Result<(File, bool), ProfileStoreError> {
    match OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(file) => Ok((file, true)),
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path).map_err(|source| ProfileStoreError::Io {
                operation: "inspect existing race-run lock file",
                path: path.to_owned(),
                source,
            })?;
            if !metadata.file_type().is_file() {
                return Err(ProfileStoreError::InvalidStorageEntry {
                    path: path.to_owned(),
                    reason: "race-run lock must be a regular non-symbolic-link file",
                });
            }
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .map(|file| (file, false))
                .map_err(|source| ProfileStoreError::Io {
                    operation: "open existing race-run lock file",
                    path: path.to_owned(),
                    source,
                })
        }
        Err(source) => Err(ProfileStoreError::Io {
            operation: "create race-run lock file",
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

fn verify_atomic_publish_support(root: &Path) -> Result<(), ProfileStoreError> {
    let temporary = write_unique_temporary(
        root,
        ".P5136RustAtomicPublishProbe",
        b"P5136 atomic publish probe\n",
    )?;
    let Some(file_name) = temporary.file_name().and_then(|name| name.to_str()) else {
        let _ = fs::remove_file(&temporary);
        return Err(ProfileStoreError::InternalInvariant {
            message: "generated atomic-publish probe path must have a UTF-8 filename",
        });
    };
    let destination = root.join(format!("{file_name}.link"));
    match publish_new_file(&temporary, &destination) {
        Ok(true) => {}
        Ok(false) => {
            let _ = fs::remove_file(&temporary);
            return Err(ProfileStoreError::AtomicPublishUnsupported {
                root: root.to_owned(),
                source: io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "atomic-publish probe destination unexpectedly existed",
                ),
            });
        }
        Err(source) => {
            let _ = fs::remove_file(&temporary);
            return Err(ProfileStoreError::AtomicPublishUnsupported {
                root: root.to_owned(),
                source,
            });
        }
    }
    fs::remove_file(&destination).map_err(|source| ProfileStoreError::Io {
        operation: "remove atomic-publish probe link",
        path: destination,
        source,
    })?;
    fs::remove_file(&temporary).map_err(|source| ProfileStoreError::Io {
        operation: "remove atomic-publish probe temporary",
        path: temporary,
        source,
    })?;
    sync_directory(root).map_err(|source| ProfileStoreError::Io {
        operation: "sync atomic-publish probe cleanup",
        path: root.to_owned(),
        source,
    })
}

fn load_or_create_store_id(root: &Path) -> Result<ProfileStoreId, ProfileStoreError> {
    let path = root.join(STORE_ID_FILENAME);
    match fs::metadata(&path) {
        Ok(_) => return read_store_id(root),
        Err(source) if source.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(ProfileStoreError::Io {
                operation: "inspect profile-store identity",
                path,
                source,
            });
        }
    }
    let mut store_id = None;
    for _ in 0..16 {
        store_id = ProfileStoreId::new(random());
        if store_id.is_some() {
            break;
        }
    }
    let Some(store_id) = store_id else {
        return Err(ProfileStoreError::InternalInvariant {
            message: "OS randomness repeatedly returned the reserved zero store identity",
        });
    };
    let bytes = store_id_file_bytes(store_id);
    let temporary = write_unique_temporary(root, STORE_ID_FILENAME, &bytes)?;
    match publish_new_file(&temporary, &path) {
        Ok(true) => {
            let _ = fs::remove_file(&temporary);
            sync_directory(root).map_err(|source| ProfileStoreError::Io {
                operation: "sync profile-store identity",
                path: root.to_owned(),
                source,
            })?;
            Ok(store_id)
        }
        Ok(false) => {
            let _ = fs::remove_file(&temporary);
            read_store_id(root)
        }
        Err(source) => {
            let _ = fs::remove_file(&temporary);
            Err(ProfileStoreError::Io {
                operation: "publish profile-store identity",
                path,
                source,
            })
        }
    }
}

fn read_store_id(root: &Path) -> Result<ProfileStoreId, ProfileStoreError> {
    let path = root.join(STORE_ID_FILENAME);
    let bytes = match read_bounded_metadata_file(&path, MAX_STORE_METADATA_BYTES) {
        Ok(bytes) => bytes,
        Err(MetadataReadError::Io(source)) => {
            return Err(ProfileStoreError::Io {
                operation: "read profile-store identity",
                path,
                source,
            });
        }
        Err(MetadataReadError::Invalid(reason)) => {
            return Err(ProfileStoreError::InvalidStoreMetadata { path, reason });
        }
    };
    parse_store_id_file(&bytes).ok_or(ProfileStoreError::InvalidStoreMetadata {
        path,
        reason: "content must be the versioned header and one nonzero lowercase hexadecimal ID",
    })
}

fn store_id_file_bytes(store_id: ProfileStoreId) -> Vec<u8> {
    format!("{STORE_ID_HEADER}\n{}\n", store_id_hex(store_id)).into_bytes()
}

fn parse_store_id_file(bytes: &[u8]) -> Option<ProfileStoreId> {
    let text = std::str::from_utf8(bytes).ok()?;
    let encoded = text
        .strip_prefix(STORE_ID_HEADER)?
        .strip_prefix('\n')?
        .strip_suffix('\n')?;
    parse_store_id_hex(encoded)
}

fn store_id_hex(store_id: ProfileStoreId) -> String {
    let mut output = String::with_capacity(32);
    for byte in store_id.get() {
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

fn parse_store_id_hex(encoded: &str) -> Option<ProfileStoreId> {
    if encoded.len() != 32
        || !encoded
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let mut bytes = [0_u8; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&encoded[index * 2..index * 2 + 2], 16).ok()?;
    }
    ProfileStoreId::new(bytes)
}

fn race_run_marker_bytes(store_id: ProfileStoreId, generation: RaceRunGeneration) -> Vec<u8> {
    format!(
        "{RACE_RUN_MARKER_HEADER}\n{}\n{}\n",
        store_id_hex(store_id),
        generation.get()
    )
    .into_bytes()
}

fn validate_race_run_marker(
    path: &Path,
    store_id: ProfileStoreId,
    generation: RaceRunGeneration,
) -> Result<(), ProfileStoreError> {
    let bytes = match read_bounded_metadata_file(path, MAX_RACE_RUN_MARKER_BYTES) {
        Ok(bytes) => bytes,
        Err(MetadataReadError::Io(source)) => {
            return Err(ProfileStoreError::Io {
                operation: "read race-run generation marker",
                path: path.to_owned(),
                source,
            });
        }
        Err(MetadataReadError::Invalid(reason)) => {
            return Err(ProfileStoreError::InvalidRaceRunGenerationMarker {
                path: path.to_owned(),
                reason,
            });
        }
    };
    if bytes != race_run_marker_bytes(store_id, generation) {
        return Err(ProfileStoreError::InvalidRaceRunGenerationMarker {
            path: path.to_owned(),
            reason: "content does not match the marker filename and durable store identity",
        });
    }
    Ok(())
}

enum MetadataReadError {
    Io(io::Error),
    Invalid(&'static str),
}

fn read_bounded_metadata_file(path: &Path, maximum: u64) -> Result<Vec<u8>, MetadataReadError> {
    let path_metadata = fs::symlink_metadata(path).map_err(MetadataReadError::Io)?;
    if !path_metadata.file_type().is_file() {
        return Err(MetadataReadError::Invalid(
            "entry must be a regular non-symbolic-link file",
        ));
    }
    let file = File::open(path).map_err(MetadataReadError::Io)?;
    let metadata = file.metadata().map_err(MetadataReadError::Io)?;
    if !metadata.is_file() {
        return Err(MetadataReadError::Invalid("entry must be a regular file"));
    }
    if metadata.len() > maximum {
        return Err(MetadataReadError::Invalid(
            "file exceeds the bounded metadata size",
        ));
    }
    let capacity = usize::try_from(metadata.len()).unwrap_or(usize::MAX.min(256));
    let mut bytes = Vec::with_capacity(capacity);
    file.take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(MetadataReadError::Io)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum {
        return Err(MetadataReadError::Invalid(
            "file exceeds the bounded metadata size",
        ));
    }
    Ok(bytes)
}

fn race_run_generations(
    root: &Path,
    store_id: ProfileStoreId,
) -> Result<Vec<(RaceRunGeneration, PathBuf)>, ProfileStoreError> {
    cleanup_published_temporary_files(root, MAX_PROFILE_ROOT_ENTRIES)?;
    let entries = fs::read_dir(root).map_err(|source| ProfileStoreError::Io {
        operation: "list race run generations",
        path: root.to_owned(),
        source,
    })?;
    let mut generations = Vec::new();
    for (index, entry) in entries.enumerate() {
        if index >= MAX_PROFILE_ROOT_ENTRIES {
            return Err(ProfileStoreError::DirectoryEntryLimitExceeded {
                directory: root.to_owned(),
                maximum: MAX_PROFILE_ROOT_ENTRIES,
            });
        }
        let entry = entry.map_err(|source| ProfileStoreError::Io {
            operation: "read race run generation entry",
            path: root.to_owned(),
            source,
        })?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(generation) = parse_race_run_generation(&name) else {
            if has_ascii_case_insensitive_prefix(&name, RACE_RUN_GENERATION_PREFIX) {
                return Err(ProfileStoreError::InvalidRaceRunGenerationMarker {
                    path: entry.path(),
                    reason: "reserved marker filename is malformed or noncanonical",
                });
            }
            continue;
        };
        if name != race_run_generation_filename(generation) {
            return Err(ProfileStoreError::InvalidRaceRunGenerationMarker {
                path: entry.path(),
                reason: "marker filename is not in canonical fixed-width form",
            });
        }
        if generations.len() >= MAX_RACE_RUN_GENERATION_MARKERS {
            return Err(ProfileStoreError::DirectoryEntryLimitExceeded {
                directory: root.to_owned(),
                maximum: MAX_RACE_RUN_GENERATION_MARKERS,
            });
        }
        let path = entry.path();
        let file_type = entry.file_type().map_err(|source| ProfileStoreError::Io {
            operation: "inspect race run generation marker",
            path: path.clone(),
            source,
        })?;
        if !file_type.is_file() {
            return Err(ProfileStoreError::InvalidRaceRunGenerationMarker {
                path,
                reason: "marker must be a regular file",
            });
        }
        validate_race_run_marker(&path, store_id, generation)?;
        generations.push((generation, path));
    }
    generations.sort_unstable_by_key(|(generation, _)| *generation);
    Ok(generations)
}

fn compact_race_run_generations_before_publish(
    root: &Path,
    generations: &[(RaceRunGeneration, PathBuf)],
    retained: Option<RaceRunGeneration>,
) -> Result<(), ProfileStoreError> {
    let mut removed = false;
    for (generation, path) in generations {
        if Some(*generation) == retained {
            continue;
        }
        match fs::remove_file(path) {
            Ok(()) => removed = true,
            Err(source) => {
                return Err(ProfileStoreError::Io {
                    operation: "pre-compact race run generation",
                    path: path.clone(),
                    source,
                });
            }
        }
    }
    if removed {
        sync_directory(root).map_err(|source| ProfileStoreError::Io {
            operation: "sync pre-compacted race run generations",
            path: root.to_owned(),
            source,
        })?;
    }
    Ok(())
}

fn write_unique_temporary(
    directory: &Path,
    final_name: &str,
    bytes: &[u8],
) -> Result<PathBuf, ProfileStoreError> {
    for _ in 0..MAX_TEMPORARY_FILE_ATTEMPTS {
        let sequence = TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!(
            ".{final_name}.{}.{}.tmp",
            std::process::id(),
            sequence
        ));
        match write_new_file(&path, bytes) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                let _ = fs::remove_file(&path);
                return Err(ProfileStoreError::Io {
                    operation: "write temporary immutable file",
                    path,
                    source,
                });
            }
        }
    }
    Err(ProfileStoreError::TemporaryFileContention {
        directory: directory.to_owned(),
        attempts: MAX_TEMPORARY_FILE_ATTEMPTS,
    })
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

#[cfg(windows)]
fn sync_directory(directory: &Path) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(directory)
        .and_then(|file| file.sync_all())
}

#[cfg(not(any(unix, windows)))]
fn sync_directory(directory: &Path) -> io::Result<()> {
    fs::metadata(directory).map(|_| ())
}

#[cfg(test)]
mod tests {
    use std::{
        fs, io,
        sync::{
            Arc, Barrier,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
    };

    use serde_json::json;
    use tempfile::tempdir;

    use super::{
        PROFILE_REVISION_RETENTION, ProfileMutation, ProfileStore, ProfileStoreError,
        ProfileStoreId, ProfileTransaction, RaceRunGeneration, STORE_ID_FILENAME,
        create_dir_all_durable_with, publish_new_file, race_run_generation_filename,
        race_run_generations, race_run_marker_bytes, revisions_descending, store_id_file_bytes,
        version_filename,
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

        store.load_or_create("Rider").unwrap();
        assert!(store.profile_exists("rIDER").unwrap());
    }

    #[test]
    fn unicode_normalization_variants_share_one_profile_identity() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let composed = "\u{00e9}";
        let decomposed = "e\u{0301}";

        let first = store.load_or_create(composed).unwrap();
        let second = store.load_or_create(decomposed).unwrap();
        assert_eq!(first.source_path, second.source_path);
        assert_eq!(second.nickname, composed);
        assert_eq!(
            fs::read_dir(root.path())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                .count(),
            1
        );
    }

    #[test]
    fn internal_metadata_nickname_prefix_is_reserved_on_every_host() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        for nickname in [
            ".P5136RustProfileStoreId",
            ".p5136rustracerun.lock",
            ".P5136RUST-future",
        ] {
            assert!(matches!(
                store.load_or_create(nickname),
                Err(ProfileStoreError::ReservedStorageNickname { .. })
            ));
        }
        assert!(fs::read_dir(root.path()).unwrap().next().is_none());
    }

    #[test]
    fn durable_directory_creation_retries_the_uncertain_existing_anchor() {
        let root = tempdir().unwrap();
        let directory = root.path().join("profile");
        let mut sync_count = 0;
        let first = create_dir_all_durable_with(&directory, |_| {
            sync_count += 1;
            if sync_count == 2 {
                Err(io::Error::other("injected created-directory sync fault"))
            } else {
                Ok(())
            }
        });
        assert!(first.is_err());
        assert!(directory.is_dir());

        let mut retry_syncs = Vec::new();
        create_dir_all_durable_with(&directory, |path| {
            retry_syncs.push(path.to_owned());
            Ok(())
        })
        .unwrap();
        assert!(retry_syncs.contains(&root.path().to_owned()));
    }

    #[test]
    fn durable_directory_creation_handles_creation_race_and_nested_retry() {
        let race_root = tempdir().unwrap();
        let raced_directory = race_root.path().join("profile");
        let mut race_sync_count = 0;
        let raced_clone = raced_directory.clone();
        let first = create_dir_all_durable_with(&raced_directory, |_| {
            race_sync_count += 1;
            if race_sync_count == 1 {
                fs::create_dir(&raced_clone).unwrap();
                Ok(())
            } else {
                Err(io::Error::other("injected creation-race sync fault"))
            }
        });
        assert!(first.is_err());
        create_dir_all_durable_with(&raced_directory, |_| Ok(())).unwrap();

        let nested_root = tempdir().unwrap();
        let first_component = nested_root.path().join("a");
        let nested = first_component.join("b");
        let mut nested_sync_count = 0;
        let first = create_dir_all_durable_with(&nested, |_| {
            nested_sync_count += 1;
            if nested_sync_count == 2 {
                Err(io::Error::other("injected nested-ancestor sync fault"))
            } else {
                Ok(())
            }
        });
        assert!(first.is_err());
        assert!(first_component.is_dir());
        assert!(!nested.exists());

        let mut retry_syncs = Vec::new();
        create_dir_all_durable_with(&nested, |path| {
            retry_syncs.push(path.to_owned());
            Ok(())
        })
        .unwrap();
        assert_eq!(retry_syncs.first(), Some(&nested_root.path().to_owned()));
        assert!(nested.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn ambiguous_case_directories_and_profile_symlinks_are_rejected() {
        use std::os::unix::fs::symlink;

        let ambiguous_root = tempdir().unwrap();
        fs::create_dir(ambiguous_root.path().join("Rider")).unwrap();
        fs::create_dir(ambiguous_root.path().join("rider")).unwrap();
        assert!(matches!(
            ProfileStore::new(ambiguous_root.path()).profile_exists("RIDER"),
            Err(ProfileStoreError::AmbiguousProfileDirectories { .. })
        ));

        let symlink_root = tempdir().unwrap();
        let target = tempdir().unwrap();
        symlink(target.path(), symlink_root.path().join("rider")).unwrap();
        assert!(matches!(
            ProfileStore::new(symlink_root.path()).load_or_create("Rider"),
            Err(ProfileStoreError::InvalidStorageEntry { .. })
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn ambiguous_legacy_unicode_normalization_directories_are_rejected() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join("\u{00e9}")).unwrap();
        fs::create_dir(root.path().join("e\u{0301}")).unwrap();
        assert!(matches!(
            ProfileStore::new(root.path()).profile_exists("\u{00e9}"),
            Err(ProfileStoreError::AmbiguousProfileDirectories { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn revision_legacy_and_lock_symlinks_are_rejected() {
        use std::os::unix::fs::symlink;

        let revision_root = tempdir().unwrap();
        let revision_directory = revision_root.path().join("rider");
        fs::create_dir(&revision_directory).unwrap();
        let revision_target = revision_root.path().join("revision-target.json");
        fs::write(&revision_target, b"{}").unwrap();
        symlink(
            &revision_target,
            revision_directory.join(version_filename(1)),
        )
        .unwrap();
        assert!(matches!(
            ProfileStore::new(revision_root.path()).load_or_create("Rider"),
            Err(ProfileStoreError::InvalidStorageEntry { .. })
        ));

        let legacy_root = tempdir().unwrap();
        let legacy_directory = legacy_root.path().join("rider");
        fs::create_dir(&legacy_directory).unwrap();
        let legacy_target = legacy_root.path().join("legacy-target.json");
        fs::write(&legacy_target, b"{}").unwrap();
        symlink(
            &legacy_target,
            legacy_directory.join(super::LEGACY_FILENAME),
        )
        .unwrap();
        assert!(matches!(
            ProfileStore::new(legacy_root.path()).load_or_create("Rider"),
            Err(ProfileStoreError::InvalidStorageEntry { .. })
        ));

        let lock_root = tempdir().unwrap();
        let lock_target = lock_root.path().join("lock-target");
        fs::write(&lock_target, b"").unwrap();
        symlink(
            &lock_target,
            lock_root.path().join(super::RACE_RUN_LOCK_FILENAME),
        )
        .unwrap();
        assert!(matches!(
            ProfileStore::new(lock_root.path()).acquire_race_run_lease(),
            Err(ProfileStoreError::InvalidStorageEntry { .. })
        ));
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
            revisions_descending(root.path().join("rider").as_path())
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
                    root.path().join("rider").join(version_filename(2))
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
    fn post_publish_sync_failure_is_visible_from_fresh_disk_and_retry_is_a_noop() {
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

        let fresh = store.load_or_create("rIDER").unwrap();
        assert_eq!(fresh.revision, Some(2));
        assert_eq!(fresh.profile.rider.lucci, 1_000_040);
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
    fn normal_writes_precompact_a_bounded_recovery_tail_before_publish() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        store.load_or_create("Rider").unwrap();
        for _ in 0..(PROFILE_REVISION_RETENTION + 20) {
            store
                .update("Rider", |profile| profile.rider.lucci += 1)
                .unwrap();
        }

        let revisions = revisions_descending(root.path().join("rider").as_path()).unwrap();
        assert_eq!(revisions.len(), PROFILE_REVISION_RETENTION);
        assert_eq!(
            store.load_or_create("Rider").unwrap().profile.rider.lucci,
            1_000_000 + u32::try_from(PROFILE_REVISION_RETENTION).unwrap() + 20
        );
    }

    #[test]
    fn precompaction_retains_the_valid_recovery_source_until_publish() {
        let root = tempdir().unwrap();
        let directory = root.path().join("rider");
        fs::create_dir(&directory).unwrap();
        let mut initial = serde_json::to_vec_pretty(&crate::Profile::default()).unwrap();
        initial.push(b'\n');
        fs::write(directory.join(version_filename(1)), initial).unwrap();
        for revision in 2..=70 {
            fs::write(directory.join(version_filename(revision)), b"{truncated").unwrap();
        }

        let store = ProfileStore::new(root.path());
        let (saved, profile) = store
            .update("Rider", |profile| profile.rider.lucci += 7)
            .unwrap();
        assert_eq!(saved.revision, 71);
        assert_eq!(profile.rider.lucci, 1_000_007);
        assert!(directory.join(version_filename(1)).is_file());
        assert_eq!(
            revisions_descending(&directory).unwrap().len(),
            PROFILE_REVISION_RETENTION
        );

        fs::write(&saved.path, b"{truncated").unwrap();
        for revision in 72..=140 {
            fs::write(directory.join(version_filename(revision)), b"{truncated").unwrap();
        }
        let saved = store.save("Rider", &profile).unwrap();
        assert_eq!(saved.revision, 141);
        assert!(directory.join(version_filename(1)).is_file());
        assert_eq!(
            revisions_descending(&directory).unwrap().len(),
            PROFILE_REVISION_RETENTION
        );
    }

    #[test]
    fn malformed_reserved_revision_names_fail_closed() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        store.load_or_create("Rider").unwrap();
        let directory = root.path().join("rider");
        fs::write(directory.join("Launcher.vnot-a-revision.json"), b"{}").unwrap();
        assert!(matches!(
            store.load_or_create("Rider"),
            Err(ProfileStoreError::InvalidStorageEntry { .. })
        ));
    }

    #[test]
    fn cleanup_removes_only_temporary_files_with_a_published_destination() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        store.load_or_create("Rider").unwrap();
        let directory = root.path().join("rider");
        let published = directory.join(format!(".{}.123.1.tmp", version_filename(1)));
        let unpublished = directory.join(format!(".{}.123.2.tmp", version_filename(2)));
        fs::write(&published, b"published duplicate").unwrap();
        fs::write(&unpublished, b"possibly live writer").unwrap();

        store.load_or_create("Rider").unwrap();
        assert!(!published.exists());
        assert!(unpublished.is_file());
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
    fn independent_store_instances_retry_revision_cas_without_losing_updates() {
        let root = tempdir().unwrap();
        ProfileStore::new(root.path())
            .load_or_create("Rider")
            .unwrap();
        let stores = [
            Arc::new(ProfileStore::new(root.path())),
            Arc::new(ProfileStore::new(root.path())),
        ];
        let barrier = Arc::new(Barrier::new(2));
        let closure_calls = Arc::new(AtomicUsize::new(0));
        let mut workers = Vec::new();
        for store in stores {
            let barrier = Arc::clone(&barrier);
            let closure_calls = Arc::clone(&closure_calls);
            workers.push(thread::spawn(move || {
                let mut first_evaluation = true;
                store
                    .transaction("rIDER", |profile| {
                        closure_calls.fetch_add(1, Ordering::Relaxed);
                        if first_evaluation {
                            first_evaluation = false;
                            barrier.wait();
                        }
                        let mut next = profile.clone();
                        next.rider.lucci += 1;
                        ProfileMutation::changed(next.rider.lucci, next)
                    })
                    .unwrap()
            }));
        }
        for worker in workers {
            assert!(matches!(
                worker.join().unwrap(),
                ProfileTransaction::Committed { .. }
            ));
        }

        let loaded = ProfileStore::new(root.path())
            .load_or_create("Rider")
            .unwrap();
        assert_eq!(loaded.profile.rider.lucci, 1_000_002);
        assert_eq!(loaded.revision, Some(3));
        assert_eq!(closure_calls.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn unchanged_transaction_repeats_directory_sync_until_confirmed() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        store.load_or_create("Rider").unwrap();
        store.fail_next_directory_sync(io::ErrorKind::Other);

        let first = store
            .transaction("Rider", |profile| {
                ProfileMutation::Unchanged(profile.rider.lucci)
            })
            .unwrap();
        assert!(matches!(
            first,
            ProfileTransaction::CommittedButDurabilityUncertain {
                value: 1_000_000,
                saved,
                error: ProfileStoreError::CommittedButDurabilityUncertain {
                    revision: 1,
                    ..
                },
                ..
            } if saved.revision == 1
        ));

        assert!(matches!(
            store
                .transaction("Rider", |profile| {
                    ProfileMutation::Unchanged(profile.rider.lucci)
                })
                .unwrap(),
            ProfileTransaction::Unchanged {
                value: 1_000_000,
                ..
            }
        ));
        assert_eq!(
            store
                .confirm_latest_revision_durable("Rider")
                .unwrap()
                .unwrap()
                .revision,
            1
        );
    }

    #[test]
    fn race_run_lease_is_exclusive_and_generations_are_ordered_and_precompacted() {
        let root = tempdir().unwrap();
        let first_store = ProfileStore::new(root.path());
        let second_store = ProfileStore::new(root.path());

        let first = first_store.acquire_race_run_lease().unwrap();
        assert_eq!(first.generation().get(), 1);
        assert!(matches!(
            second_store.acquire_race_run_lease(),
            Err(ProfileStoreError::RaceRunLeaseBusy { .. })
        ));
        let store_id = first.store_id();
        drop(first);
        let second = second_store.acquire_race_run_lease().unwrap();
        assert_eq!(second.generation().get(), 2);
        drop(second);
        let third = first_store.acquire_race_run_lease().unwrap();
        assert_eq!(third.generation().get(), 3);
        assert_eq!(
            race_run_generations(root.path(), store_id)
                .unwrap()
                .iter()
                .map(|(generation, _)| generation.get())
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    #[test]
    fn lease_accepts_an_equivalent_root_path_and_detects_store_id_replacement() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let lease = store.acquire_race_run_lease().unwrap();
        let alias = ProfileStore::new(root.path().join("."));
        alias.validate_race_run_lease(&lease).unwrap();

        let replacement = if lease.store_id() == ProfileStoreId::new([2; 16]).unwrap() {
            ProfileStoreId::new([3; 16]).unwrap()
        } else {
            ProfileStoreId::new([2; 16]).unwrap()
        };
        fs::write(
            root.path().join(STORE_ID_FILENAME),
            store_id_file_bytes(replacement),
        )
        .unwrap();
        assert!(matches!(
            store.validate_race_run_lease(&lease),
            Err(ProfileStoreError::ProfileStoreIdentityChanged {
                expected,
                actual,
            }) if expected == lease.store_id() && actual == replacement
        ));
    }

    #[test]
    fn uncertain_run_generation_is_skipped_by_the_next_lease() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        store.fail_next_directory_sync(io::ErrorKind::Other);

        assert!(matches!(
            store.acquire_race_run_lease(),
            Err(ProfileStoreError::RaceRunGenerationDurabilityUncertain { generation: 1, .. })
        ));
        let lease = store.acquire_race_run_lease().unwrap();
        assert_eq!(lease.generation().get(), 2);
        let markers = race_run_generations(root.path(), lease.store_id()).unwrap();
        assert_eq!(
            markers
                .iter()
                .map(|(generation, _)| generation.get())
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn race_run_generation_exhaustion_is_checked() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let lease = store.acquire_race_run_lease().unwrap();
        let store_id = lease.store_id();
        drop(lease);
        let maximum = RaceRunGeneration::new(u64::MAX).unwrap();
        fs::write(
            root.path().join(race_run_generation_filename(maximum)),
            race_run_marker_bytes(store_id, maximum),
        )
        .unwrap();

        assert!(matches!(
            ProfileStore::new(root.path()).acquire_race_run_lease(),
            Err(ProfileStoreError::RaceRunGenerationExhausted { .. })
        ));
    }

    #[test]
    fn malformed_generation_marker_cannot_poison_the_counter() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        drop(store.acquire_race_run_lease().unwrap());
        let maximum = RaceRunGeneration::new(u64::MAX).unwrap();
        fs::write(
            root.path().join(race_run_generation_filename(maximum)),
            format!("{}\n", maximum.get()),
        )
        .unwrap();

        assert!(matches!(
            store.acquire_race_run_lease(),
            Err(ProfileStoreError::InvalidRaceRunGenerationMarker { .. })
        ));
    }

    #[test]
    fn malformed_reserved_generation_marker_name_fails_closed() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        drop(store.acquire_race_run_lease().unwrap());
        fs::write(
            root.path()
                .join(".P5136RustRaceRun.vnot-a-generation.marker"),
            b"invalid",
        )
        .unwrap();

        assert!(matches!(
            store.acquire_race_run_lease(),
            Err(ProfileStoreError::InvalidRaceRunGenerationMarker { .. })
        ));
    }

    #[test]
    fn loads_are_fresh_without_explicit_invalidation() {
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
            99
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
