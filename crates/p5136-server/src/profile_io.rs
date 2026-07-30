//! Bounded, cancellation-safe profile persistence runtime.
//!
//! A submitted job is owned by this runtime until its blocking filesystem work
//! finishes. Dropping the requester's future only drops its reply receiver; it
//! never detaches a `spawn_blocking` task or releases a profile lane early.

use std::{
    any::Any,
    collections::HashMap,
    fmt,
    num::NonZeroUsize,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::{Arc, Mutex, Weak},
};

use p5136_core::nickname::{NicknameError, canonical_nickname_key, normalize_nickname};
use p5136_profile::{
    AppliedTimeReward, PersistedRaceRewardReceipt, Profile, ProfileStore, ProfileStoreError,
    RaceRewardKey, RaceRewardKeyError, RaceRewardPersistenceError, RaceRewardRecipientError,
    RaceRunLease, apply_race_reward_once, rider_item_snapshot,
};
use thiserror::Error;
use tokio::{
    sync::{OwnedMutexGuard, Semaphore, mpsc, oneshot},
    task::{JoinError, JoinHandle, JoinSet},
};

use crate::{
    identity::IdentityOperationLease, myroom_hub::MyRoomProfilePresentation,
    world::RewardSettlementTask,
};

const MAX_BLOCKING_PROFILE_JOBS: usize = 32;
const REWARD_PERSISTENCE_OPERATION: &str = "persist race reward";

#[must_use]
pub(crate) fn myroom_profile_presentation(profile: &Profile) -> MyRoomProfilePresentation {
    MyRoomProfilePresentation::new(
        u16::try_from(profile.rider.p2p_port).unwrap_or_default(),
        rider_item_snapshot(&profile.rider_item),
        profile.rider.rp,
        profile.rider.club_name.clone(),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProfileIoLimits {
    lanes: NonZeroUsize,
    queued_jobs: NonZeroUsize,
    in_flight_jobs: NonZeroUsize,
}

impl ProfileIoLimits {
    pub(crate) fn for_server(
        maximum_sessions: usize,
        maximum_reward_jobs: usize,
    ) -> Result<Self, ProfileIoConfigError> {
        if maximum_sessions == 0 {
            return Err(ProfileIoConfigError::ZeroSessions);
        }
        let total = maximum_sessions
            .checked_add(maximum_reward_jobs)
            .ok_or(ProfileIoConfigError::CapacityOverflow)?;
        if total > Semaphore::MAX_PERMITS {
            return Err(ProfileIoConfigError::CapacityExceedsRuntimeLimit {
                configured: total,
                maximum: Semaphore::MAX_PERMITS,
            });
        }
        let total = NonZeroUsize::new(total).ok_or(ProfileIoConfigError::ZeroTotalCapacity)?;
        let in_flight = total.get().min(MAX_BLOCKING_PROFILE_JOBS);
        Ok(Self {
            lanes: total,
            queued_jobs: total,
            in_flight_jobs: NonZeroUsize::new(in_flight)
                .ok_or(ProfileIoConfigError::ZeroTotalCapacity)?,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_tests(maximum_lanes: usize, maximum_jobs: usize) -> Self {
        Self {
            lanes: NonZeroUsize::new(maximum_lanes).unwrap(),
            queued_jobs: NonZeroUsize::new(maximum_jobs).unwrap(),
            in_flight_jobs: NonZeroUsize::new(maximum_jobs).unwrap(),
        }
    }
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum ProfileIoConfigError {
    #[error("profile I/O requires at least one login-session slot")]
    ZeroSessions,

    #[error("profile I/O session and reward capacities overflow usize")]
    CapacityOverflow,

    #[error("profile I/O queue capacity {configured} exceeds the Tokio runtime limit {maximum}")]
    CapacityExceedsRuntimeLimit { configured: usize, maximum: usize },

    #[error("profile I/O session and reward capacities must not both be zero")]
    ZeroTotalCapacity,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProfileIoError {
    #[error(transparent)]
    InvalidNickname(#[from] NicknameError),

    #[error("profile lane registry lock was poisoned")]
    LaneRegistryPoisoned,

    #[error("profile lane capacity {maximum} is exhausted")]
    LaneCapacityExhausted { maximum: usize },

    #[error("profile I/O runtime is shutting down before {operation} was accepted")]
    ShuttingDown { operation: &'static str },

    #[error("profile I/O runtime stopped before {operation} completed")]
    RuntimeStopped { operation: &'static str },

    #[error("profile I/O worker panicked during {operation}: {message}")]
    WorkerPanicked {
        operation: &'static str,
        message: String,
    },

    #[error("profile I/O completion type did not match operation {operation}")]
    CompletionTypeMismatch { operation: &'static str },
}

#[derive(Debug, Error)]
pub enum ProfileIoRuntimeError {
    #[error("profile I/O worker panicked during {operation}: {message}")]
    WorkerPanicked {
        operation: &'static str,
        message: String,
    },

    #[error("profile I/O blocking task failed")]
    WorkerTask(#[source] JoinError),
}

#[derive(Debug, Error)]
pub enum ProfileIoShutdownError {
    #[error("profile I/O supervisor task failed")]
    SupervisorTask(#[source] JoinError),

    #[error(transparent)]
    Runtime(#[from] ProfileIoRuntimeError),
}

#[derive(Debug, Error)]
pub(crate) enum ProfileRewardJobError {
    #[error(transparent)]
    Recipient(#[from] RaceRewardRecipientError),

    #[error(transparent)]
    Key(#[from] RaceRewardKeyError),

    #[error(transparent)]
    Persistence(#[from] RaceRewardPersistenceError),
}

/// An exact durable profile receipt minted only by the profile I/O boundary.
///
/// Keeping both the opaque World task and the persisted key prevents callers
/// from turning a bare [`AppliedTimeReward`] value into a durability claim.
#[derive(Debug)]
pub(crate) struct DurableRewardReceipt {
    task: RewardSettlementTask,
    persisted: PersistedRaceRewardReceipt,
    profile: Option<MyRoomProfileLease>,
}

impl DurableRewardReceipt {
    fn new(
        task: RewardSettlementTask,
        persisted: PersistedRaceRewardReceipt,
        profile: MyRoomProfileLease,
    ) -> Self {
        Self {
            task,
            persisted,
            profile: Some(profile),
        }
    }

    pub(crate) fn task(&self) -> &RewardSettlementTask {
        &self.task
    }

    pub(crate) fn key(&self) -> &RaceRewardKey {
        &self.persisted.key
    }

    pub(crate) const fn applied(&self) -> AppliedTimeReward {
        self.persisted.applied
    }

    pub(crate) fn profile(&self) -> Option<&MyRoomProfileLease> {
        self.profile.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        task: RewardSettlementTask,
        key: RaceRewardKey,
        applied: AppliedTimeReward,
    ) -> Self {
        Self {
            task,
            persisted: PersistedRaceRewardReceipt { key, applied },
            profile: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test_with_profile(
        task: RewardSettlementTask,
        key: RaceRewardKey,
        applied: AppliedTimeReward,
        presentation: MyRoomProfilePresentation,
        nickname: &str,
    ) -> Self {
        Self {
            task,
            persisted: PersistedRaceRewardReceipt { key, applied },
            profile: Some(MyRoomProfileLease::for_test(presentation, nickname)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RewardFailureClassification {
    Retryable,
    JobFatal,
    InfrastructureFatal,
}

#[derive(Debug, Error)]
enum RewardPersistenceFailureSource {
    #[error(transparent)]
    ProfileIo(#[from] ProfileIoError),

    #[error(transparent)]
    Reward(#[from] ProfileRewardJobError),
}

/// A failed persistence attempt that returns ownership of the exact World task.
#[derive(Debug, Error)]
#[error("reward persistence failed ({classification:?}): {source}")]
pub(crate) struct RewardPersistenceFailure {
    task: RewardSettlementTask,
    classification: RewardFailureClassification,
    #[source]
    source: RewardPersistenceFailureSource,
}

impl RewardPersistenceFailure {
    fn profile_io(task: RewardSettlementTask, source: ProfileIoError) -> Self {
        Self {
            task,
            classification: classify_profile_io_error(&source),
            source: source.into(),
        }
    }

    fn reward(task: RewardSettlementTask, source: ProfileRewardJobError) -> Self {
        Self {
            task,
            classification: classify_reward_job_error(&source),
            source: source.into(),
        }
    }

    pub(crate) const fn classification(&self) -> RewardFailureClassification {
        self.classification
    }

    pub(crate) fn task(&self) -> &RewardSettlementTask {
        &self.task
    }

    pub(crate) fn into_task(self) -> RewardSettlementTask {
        self.task
    }
}

const fn classify_profile_io_error(error: &ProfileIoError) -> RewardFailureClassification {
    match error {
        ProfileIoError::LaneCapacityExhausted { .. } => RewardFailureClassification::Retryable,
        ProfileIoError::InvalidNickname(_) => RewardFailureClassification::JobFatal,
        ProfileIoError::LaneRegistryPoisoned
        | ProfileIoError::ShuttingDown { .. }
        | ProfileIoError::RuntimeStopped { .. }
        | ProfileIoError::WorkerPanicked { .. }
        | ProfileIoError::CompletionTypeMismatch { .. } => {
            RewardFailureClassification::InfrastructureFatal
        }
    }
}

fn classify_reward_job_error(error: &ProfileRewardJobError) -> RewardFailureClassification {
    match error {
        ProfileRewardJobError::Recipient(error) => classify_reward_recipient_error(error),
        ProfileRewardJobError::Key(_) => RewardFailureClassification::InfrastructureFatal,
        ProfileRewardJobError::Persistence(error) => classify_reward_persistence_error(error),
    }
}

fn classify_reward_recipient_error(
    error: &RaceRewardRecipientError,
) -> RewardFailureClassification {
    match error {
        RaceRewardRecipientError::Store(error) => classify_profile_store_error(error),
        RaceRewardRecipientError::Nickname(_) | RaceRewardRecipientError::ProfileMissing { .. } => {
            RewardFailureClassification::JobFatal
        }
        RaceRewardRecipientError::ZeroUserNo => RewardFailureClassification::InfrastructureFatal,
    }
}

fn classify_reward_persistence_error(
    error: &RaceRewardPersistenceError,
) -> RewardFailureClassification {
    match error {
        RaceRewardPersistenceError::Store(error) => classify_profile_store_error(error),
        RaceRewardPersistenceError::Order(_)
        | RaceRewardPersistenceError::InvalidStoredReceipt(_) => {
            RewardFailureClassification::JobFatal
        }
        RaceRewardPersistenceError::Binding(_) => RewardFailureClassification::InfrastructureFatal,
        RaceRewardPersistenceError::RejectedButDurabilityUncertain {
            rejection,
            durability,
        } => {
            let rejection = classify_reward_persistence_error(rejection);
            let durability = classify_profile_store_error(durability);
            if matches!(
                (rejection, durability),
                (RewardFailureClassification::InfrastructureFatal, _)
                    | (_, RewardFailureClassification::InfrastructureFatal)
            ) {
                RewardFailureClassification::InfrastructureFatal
            } else {
                // Retrying after a rejected transaction whose directory
                // durability is uncertain could duplicate a committed write.
                RewardFailureClassification::JobFatal
            }
        }
    }
}

const fn classify_profile_store_error(error: &ProfileStoreError) -> RewardFailureClassification {
    match error {
        ProfileStoreError::Io { .. }
        | ProfileStoreError::TransactionContention { .. }
        | ProfileStoreError::ProfileCreationContention { .. }
        | ProfileStoreError::ProfileSaveContention { .. }
        | ProfileStoreError::TemporaryFileContention { .. }
        | ProfileStoreError::CommittedButDurabilityUncertain { .. } => {
            RewardFailureClassification::Retryable
        }
        ProfileStoreError::Nickname(_)
        | ProfileStoreError::Json { .. }
        | ProfileStoreError::ProfileTooLarge { .. }
        | ProfileStoreError::RevisionExhausted(_)
        | ProfileStoreError::DirectoryEntryLimitExceeded { .. }
        | ProfileStoreError::ProfileRevisionLimitExceeded { .. }
        | ProfileStoreError::InvalidStorageEntry { .. }
        | ProfileStoreError::ReservedStorageNickname { .. }
        | ProfileStoreError::AmbiguousProfileDirectories { .. } => {
            RewardFailureClassification::JobFatal
        }
        ProfileStoreError::LockPoisoned
        | ProfileStoreError::RaceRunGenerationExhausted { .. }
        | ProfileStoreError::RaceRunLeaseBusy { .. }
        | ProfileStoreError::AtomicPublishUnsupported { .. }
        | ProfileStoreError::InvalidStoreMetadata { .. }
        | ProfileStoreError::InvalidRaceRunGenerationMarker { .. }
        | ProfileStoreError::RaceRunLeaseStoreMismatch { .. }
        | ProfileStoreError::ProfileStoreIdentityChanged { .. }
        | ProfileStoreError::InternalInvariant { .. }
        | ProfileStoreError::RaceRunGenerationDurabilityUncertain { .. } => {
            RewardFailureClassification::InfrastructureFatal
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ProfileKey(Arc<str>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProfileSubject {
    nickname: Arc<str>,
    key: ProfileKey,
}

impl ProfileSubject {
    fn new(nickname: &str) -> Result<Self, NicknameError> {
        let nickname = normalize_nickname(nickname)?;
        let key = canonical_nickname_key(&nickname);
        Ok(Self {
            nickname: Arc::from(nickname),
            key: ProfileKey(Arc::from(key)),
        })
    }

    pub(crate) fn nickname(&self) -> &str {
        &self.nickname
    }

    pub(crate) fn matches_nickname(&self, nickname: &str) -> Result<bool, NicknameError> {
        let nickname = normalize_nickname(nickname)?;
        Ok(self.key.0.as_ref() == canonical_nickname_key(&nickname))
    }
}

#[derive(Debug)]
struct ProfileLaneTable {
    maximum: NonZeroUsize,
    lanes: Mutex<HashMap<ProfileKey, Weak<tokio::sync::Mutex<()>>>>,
}

impl ProfileLaneTable {
    fn new(maximum: NonZeroUsize) -> Self {
        Self {
            maximum,
            lanes: Mutex::new(HashMap::new()),
        }
    }

    async fn acquire(&self, subject: ProfileSubject) -> Result<ProfileLanePermit, ProfileIoError> {
        let lane = {
            let mut lanes = self
                .lanes
                .lock()
                .map_err(|_| ProfileIoError::LaneRegistryPoisoned)?;
            if let Some(lane) = lanes.get(&subject.key).and_then(Weak::upgrade) {
                lane
            } else {
                lanes.retain(|_, lane| lane.strong_count() != 0);
                if let Some(lane) = lanes.get(&subject.key).and_then(Weak::upgrade) {
                    lane
                } else {
                    if lanes.len() >= self.maximum.get() {
                        return Err(ProfileIoError::LaneCapacityExhausted {
                            maximum: self.maximum.get(),
                        });
                    }
                    let lane = Arc::new(tokio::sync::Mutex::new(()));
                    lanes.insert(subject.key.clone(), Arc::downgrade(&lane));
                    lane
                }
            }
        };
        let guard = lane.lock_owned().await;
        Ok(ProfileLanePermit {
            subject,
            guard,
            identity_operation: None,
        })
    }

    #[cfg(test)]
    fn entry_count(&self) -> usize {
        self.lanes.lock().unwrap().len()
    }
}

pub(crate) struct ProfileLanePermit {
    subject: ProfileSubject,
    #[expect(dead_code, reason = "the owned guard's drop releases the profile lane")]
    guard: OwnedMutexGuard<()>,
    /// A profile operation admitted by an authenticated frame retains that
    /// exact identity generation until the lane and all completion/publication
    /// state derived from it are retired.
    identity_operation: Option<IdentityOperationLease>,
}

impl fmt::Debug for ProfileLanePermit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProfileLanePermit")
            .field("subject", &self.subject)
            .finish_non_exhaustive()
    }
}

impl ProfileLanePermit {
    pub(crate) fn subject(&self) -> &ProfileSubject {
        &self.subject
    }

    fn retain_identity_operation(&mut self, operation: IdentityOperationLease) {
        self.identity_operation = Some(operation);
    }
}

/// Profile-owned `MyRoom` fields paired with the canonical lane that produced
/// them. Moving this lease into a World command prevents a queued older
/// presentation from racing past a newer same-profile operation.
#[derive(Debug)]
pub(crate) struct MyRoomProfileLease {
    presentation: MyRoomProfilePresentation,
    subject: ProfileSubject,
    #[expect(dead_code, reason = "drop retains the canonical profile lane")]
    lane: Option<ProfileLanePermit>,
}

impl MyRoomProfileLease {
    pub(crate) fn new(presentation: MyRoomProfilePresentation, lane: ProfileLanePermit) -> Self {
        Self {
            presentation,
            subject: lane.subject().clone(),
            lane: Some(lane),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(presentation: MyRoomProfilePresentation, nickname: &str) -> Self {
        Self {
            presentation,
            subject: ProfileSubject::new(nickname).unwrap(),
            lane: None,
        }
    }

    pub(crate) fn presentation(&self) -> &MyRoomProfilePresentation {
        &self.presentation
    }

    pub(crate) fn subject(&self) -> &ProfileSubject {
        &self.subject
    }
}

#[derive(Debug)]
struct ProfileResources {
    store: Arc<ProfileStore>,
    lease: Arc<RaceRunLease>,
}

#[derive(Debug)]
pub(crate) struct ProfileIoBootstrap {
    resources: Arc<ProfileResources>,
    lanes: Arc<ProfileLaneTable>,
    limits: ProfileIoLimits,
}

impl ProfileIoBootstrap {
    pub(crate) fn acquire(
        root: PathBuf,
        limits: ProfileIoLimits,
    ) -> Result<Self, ProfileStoreError> {
        let store = Arc::new(ProfileStore::new(root));
        let lease = Arc::new(store.acquire_race_run_lease()?);
        Ok(Self {
            resources: Arc::new(ProfileResources { store, lease }),
            lanes: Arc::new(ProfileLaneTable::new(limits.lanes)),
            limits,
        })
    }

    pub(crate) fn spawn(self) -> (ProfileIoHandle, ProfileIoRuntime) {
        let (jobs, receiver) = mpsc::channel(self.limits.queued_jobs.get());
        let (stop, stopped) = oneshot::channel();
        let handle = ProfileIoHandle {
            jobs,
            lanes: Arc::clone(&self.lanes),
            #[cfg(test)]
            reward_test_hook: None,
        };
        let task = tokio::spawn(run_profile_io(
            receiver,
            stopped,
            self.resources,
            self.limits.in_flight_jobs,
        ));
        (
            handle,
            ProfileIoRuntime {
                stop: Some(stop),
                task,
            },
        )
    }
}

#[cfg(test)]
type RewardTestHook = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone)]
pub(crate) struct ProfileIoHandle {
    jobs: mpsc::Sender<ProfileJob>,
    lanes: Arc<ProfileLaneTable>,
    #[cfg(test)]
    reward_test_hook: Option<RewardTestHook>,
}

impl fmt::Debug for ProfileIoHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProfileIoHandle")
            .field("maximum_capacity", &self.jobs.max_capacity())
            .finish_non_exhaustive()
    }
}

impl ProfileIoHandle {
    #[cfg(test)]
    pub(crate) fn with_reward_test_hook(mut self, hook: RewardTestHook) -> Self {
        self.reward_test_hook = Some(hook);
        self
    }

    pub(crate) async fn admit(
        &self,
        nickname: &str,
        operation: &'static str,
    ) -> Result<ProfileJobAdmission, ProfileIoError> {
        let subject = ProfileSubject::new(nickname)?;
        let slot = self
            .jobs
            .clone()
            .reserve_owned()
            .await
            .map_err(|_| ProfileIoError::ShuttingDown { operation })?;
        let lane = self.lanes.acquire(subject).await?;
        Ok(ProfileJobAdmission { slot, lane })
    }

    /// Persists one exact World reward task and returns a writer-minted durable
    /// receipt. Every error returns ownership of the original opaque task.
    pub(crate) async fn persist_reward(
        &self,
        task: RewardSettlementTask,
    ) -> Result<DurableRewardReceipt, RewardPersistenceFailure> {
        let admission = match self
            .admit(task.nickname(), REWARD_PERSISTENCE_OPERATION)
            .await
        {
            Ok(admission) => admission,
            Err(error) => return Err(RewardPersistenceFailure::profile_io(task, error)),
        };
        let user_no = task.user_no().get();
        let fence = task.fence();
        let room_id = fence.room_id().0;
        let race_epoch = fence.race_epoch();
        let proposed_reward = task.proposed_reward();
        #[cfg(test)]
        let reward_test_hook = self.reward_test_hook.clone();
        let completed = admission
            .run(
                REWARD_PERSISTENCE_OPERATION,
                move |store, lease, subject| {
                    #[cfg(test)]
                    if let Some(hook) = reward_test_hook {
                        hook(subject.nickname());
                    }
                    let recipient =
                        store.bind_race_reward_recipient(lease, subject.nickname(), user_no)?;
                    let key = RaceRewardKey::new(&recipient, lease, room_id, race_epoch)?;
                    let transaction =
                        apply_race_reward_once(store, lease, &recipient, &key, proposed_reward)?;
                    let (persisted, profile) = match transaction {
                        p5136_profile::ProfileTransaction::Unchanged { value, profile, .. }
                        | p5136_profile::ProfileTransaction::Committed { value, profile, .. } => {
                            (value, profile)
                        }
                        p5136_profile::ProfileTransaction::CommittedButDurabilityUncertain {
                            error,
                            ..
                        } => {
                            return Err(ProfileRewardJobError::Persistence(
                                RaceRewardPersistenceError::Store(error),
                            ));
                        }
                    };
                    Ok((persisted, myroom_profile_presentation(&profile)))
                },
            )
            .await;
        let completed = match completed {
            Ok(completed) => completed,
            Err(error) => return Err(RewardPersistenceFailure::profile_io(task, error)),
        };
        let (result, lane) = completed.into_parts();
        match result {
            Ok((persisted, presentation)) => Ok(DurableRewardReceipt::new(
                task,
                persisted,
                MyRoomProfileLease::new(presentation, lane),
            )),
            Err(error) => {
                drop(lane);
                Err(RewardPersistenceFailure::reward(task, error))
            }
        }
    }

    #[cfg(test)]
    fn lane_entry_count(&self) -> usize {
        self.lanes.entry_count()
    }
}

pub(crate) struct ProfileJobAdmission {
    slot: mpsc::OwnedPermit<ProfileJob>,
    lane: ProfileLanePermit,
}

impl fmt::Debug for ProfileJobAdmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProfileJobAdmission")
            .field("lane", &self.lane)
            .finish_non_exhaustive()
    }
}

impl ProfileJobAdmission {
    pub(crate) fn subject(&self) -> &ProfileSubject {
        self.lane.subject()
    }

    /// Binds this already-reserved profile job and its lane to one exact
    /// authenticated identity operation. The lease then follows the lane
    /// through the worker completion and any actor-side publication.
    pub(crate) fn retain_identity_operation(mut self, operation: IdentityOperationLease) -> Self {
        self.lane.retain_identity_operation(operation);
        self
    }

    pub(crate) async fn run<T, F>(
        self,
        operation: &'static str,
        run: F,
    ) -> Result<ProfileIoCompletion<T>, ProfileIoError>
    where
        T: Send + 'static,
        F: FnOnce(&ProfileStore, &RaceRunLease, &ProfileSubject) -> T + Send + 'static,
    {
        let (reply, response) = oneshot::channel();
        self.submit_with_completion(operation, run, move |result| {
            let _ = reply.send(result);
        });
        response
            .await
            .map_err(|_| ProfileIoError::RuntimeStopped { operation })?
    }

    /// Transfers an accepted job, its profile lane, and its completion
    /// capability to the profile runtime.
    ///
    /// Unlike [`Self::run`], the completion is not tied to a request future.
    /// Once this method returns, cancelling that requester cannot discard the
    /// callback or release the lane before the blocking job completes.
    ///
    /// The callback executes on the profile supervisor task and must remain
    /// non-blocking. A panic is caught, drains already accepted work, and then
    /// terminates the profile runtime as an infrastructure failure.
    pub(crate) fn submit_with_completion<T, F, C>(
        self,
        operation: &'static str,
        run: F,
        completion: C,
    ) where
        T: Send + 'static,
        F: FnOnce(&ProfileStore, &RaceRunLease, &ProfileSubject) -> T + Send + 'static,
        C: FnOnce(Result<ProfileIoCompletion<T>, ProfileIoError>) + Send + 'static,
    {
        let Self { slot, lane } = self;
        let erased_run: ErasedRun = Box::new(move |resources| {
            let subject = lane.subject.clone();
            let value = run(&resources.store, &resources.lease, &subject);
            Box::new(ProfileIoCompletion { value, lane })
        });
        let erased_completion: ErasedCompletion = Box::new(move |result| {
            let result = match result {
                Ok(value) => value
                    .downcast::<ProfileIoCompletion<T>>()
                    .map(|value| *value)
                    .map_err(|_| ProfileIoError::CompletionTypeMismatch { operation }),
                Err(error) => Err(error),
            };
            completion(result);
        });
        let _sender = slot.send(ProfileJob {
            operation,
            run: erased_run,
            completion: erased_completion,
        });
    }
}

#[derive(Debug)]
pub(crate) struct ProfileIoCompletion<T> {
    value: T,
    lane: ProfileLanePermit,
}

impl<T> ProfileIoCompletion<T> {
    pub(crate) fn into_parts(self) -> (T, ProfileLanePermit) {
        (self.value, self.lane)
    }
}

pub(crate) struct ProfileIoRuntime {
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<Result<(), ProfileIoRuntimeError>>,
}

impl fmt::Debug for ProfileIoRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProfileIoRuntime")
            .finish_non_exhaustive()
    }
}

impl ProfileIoRuntime {
    pub(crate) async fn wait(&mut self) -> Result<(), ProfileIoShutdownError> {
        (&mut self.task)
            .await
            .map_err(ProfileIoShutdownError::SupervisorTask)??;
        Ok(())
    }

    pub(crate) async fn shutdown(mut self) -> Result<(), ProfileIoShutdownError> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.task
            .await
            .map_err(ProfileIoShutdownError::SupervisorTask)??;
        Ok(())
    }

    pub(crate) fn finish_completed(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

type ErasedValue = Box<dyn Any + Send>;
type ErasedRun = Box<dyn FnOnce(&ProfileResources) -> ErasedValue + Send>;
type ErasedCompletion = Box<dyn FnOnce(Result<ErasedValue, ProfileIoError>) + Send>;

struct ProfileJob {
    operation: &'static str,
    run: ErasedRun,
    completion: ErasedCompletion,
}

struct BlockingCompletion {
    operation: &'static str,
    result: Result<ErasedValue, ProfileIoError>,
    completion: ErasedCompletion,
}

fn execute_profile_job(job: ProfileJob, resources: &ProfileResources) -> BlockingCompletion {
    let ProfileJob {
        operation,
        run,
        completion,
    } = job;
    let result = catch_unwind(AssertUnwindSafe(|| run(resources))).map_err(|payload| {
        ProfileIoError::WorkerPanicked {
            operation,
            message: panic_payload_message(payload.as_ref()),
        }
    });
    BlockingCompletion {
        operation,
        result,
        completion,
    }
}

fn panic_payload_message(payload: &(dyn Any + Send)) -> String {
    payload.downcast_ref::<&str>().map_or_else(
        || {
            payload
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_else(|| "non-string panic payload".to_owned())
        },
        |message| (*message).to_owned(),
    )
}

async fn run_profile_io(
    mut receiver: mpsc::Receiver<ProfileJob>,
    mut stop: oneshot::Receiver<()>,
    resources: Arc<ProfileResources>,
    maximum_in_flight: NonZeroUsize,
) -> Result<(), ProfileIoRuntimeError> {
    let mut workers: JoinSet<BlockingCompletion> = JoinSet::new();
    let mut stopping = false;
    let mut input_done = false;
    let mut first_failure = None;

    loop {
        if input_done && workers.is_empty() {
            break;
        }
        tokio::select! {
            biased;
            _ = &mut stop, if !stopping => {
                stopping = true;
                receiver.close();
            }
            completed = workers.join_next(), if !workers.is_empty() => {
                match completed {
                    Some(Ok(completed)) => {
                        if let Err(ProfileIoError::WorkerPanicked { operation, message }) =
                            &completed.result
                            && first_failure.is_none()
                        {
                            first_failure = Some(ProfileIoRuntimeError::WorkerPanicked {
                                operation,
                                message: message.clone(),
                            });
                            stopping = true;
                            receiver.close();
                        }
                        let completion_result = catch_unwind(AssertUnwindSafe(|| {
                            (completed.completion)(completed.result);
                        }));
                        if completion_result.is_err() && first_failure.is_none() {
                            first_failure = Some(ProfileIoRuntimeError::WorkerPanicked {
                                operation: completed.operation,
                                message: "profile completion callback panicked".to_owned(),
                            });
                            stopping = true;
                            receiver.close();
                        }
                    }
                    Some(Err(source)) => {
                        if first_failure.is_none() {
                            first_failure = Some(ProfileIoRuntimeError::WorkerTask(source));
                            stopping = true;
                            receiver.close();
                        }
                    }
                    None => {}
                }
            }
            job = receiver.recv(),
                if !input_done && workers.len() < maximum_in_flight.get() =>
            {
                if let Some(job) = job {
                    let resources = Arc::clone(&resources);
                    workers.spawn_blocking(move || execute_profile_job(job, &resources));
                } else {
                    input_done = true;
                    stopping = true;
                }
            }
        }
    }

    match first_failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io,
        num::NonZeroU64,
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use tokio::sync::oneshot;

    use p5136_profile::{
        GlobalRaceEpoch, Profile, ProfileStore, ProfileStoreError, RaceRewardBindingError,
        RaceRewardKeyError, RaceRewardPersistenceError, RaceRewardRecipientError, TimeReward,
    };

    use super::{
        ProfileIoBootstrap, ProfileIoError, ProfileIoLimits, ProfileIoRuntimeError,
        ProfileIoShutdownError, ProfileRewardJobError, RewardFailureClassification,
        classify_profile_io_error, classify_profile_store_error, classify_reward_job_error,
    };
    use crate::{RoomId, UserNo, world::RewardSettlementTask};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn independent_profiles_run_while_same_alias_waits() {
        let root = tempfile::tempdir().unwrap();
        let bootstrap =
            ProfileIoBootstrap::acquire(root.path().to_owned(), ProfileIoLimits::for_tests(4, 4))
                .unwrap();
        let (handle, runtime) = bootstrap.spawn();
        let first = handle.admit("RiderA", "first").await.unwrap();
        let alias_handle = handle.clone();
        let (entered, entered_rx) = oneshot::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        let first_task = tokio::spawn(async move {
            first
                .run("first", move |_, _, _| {
                    let _ = entered.send(());
                    release_rx.recv().unwrap();
                    1_u8
                })
                .await
        });
        entered_rx.await.unwrap();

        let mut alias =
            tokio::spawn(async move { alias_handle.admit("ridera", "alias").await.unwrap() });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut alias)
                .await
                .is_err()
        );

        let other = handle.admit("RiderB", "other").await.unwrap();
        let other = other.run("other", |_, _, _| 2_u8).await.unwrap();
        assert_eq!(other.into_parts().0, 2);

        release.send(()).unwrap();
        let first = first_task.await.unwrap().unwrap();
        assert_eq!(first.into_parts().0, 1);
        drop(alias.await.unwrap());
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_request_remains_owned_and_shutdown_drains_it() {
        let root = tempfile::tempdir().unwrap();
        let bootstrap =
            ProfileIoBootstrap::acquire(root.path().to_owned(), ProfileIoLimits::for_tests(2, 2))
                .unwrap();
        let (handle, runtime) = bootstrap.spawn();
        let admission = handle.admit("Rider", "blocked").await.unwrap();
        let (entered, entered_rx) = oneshot::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        let completed = Arc::new(AtomicUsize::new(0));
        let completed_worker = Arc::clone(&completed);
        let request = tokio::spawn(async move {
            admission
                .run("blocked", move |_, _, _| {
                    let _ = entered.send(());
                    release_rx.recv().unwrap();
                    completed_worker.fetch_add(1, Ordering::SeqCst);
                })
                .await
        });
        entered_rx.await.unwrap();
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());

        let mut shutdown = tokio::spawn(runtime.shutdown());
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut shutdown)
                .await
                .is_err()
        );
        release.send(()).unwrap();
        shutdown.await.unwrap().unwrap();
        assert_eq!(completed.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn accepted_completion_callback_survives_requester_cancellation() {
        let root = tempfile::tempdir().unwrap();
        let bootstrap =
            ProfileIoBootstrap::acquire(root.path().to_owned(), ProfileIoLimits::for_tests(2, 2))
                .unwrap();
        let (handle, runtime) = bootstrap.spawn();
        let admission = handle.admit("Rider", "callback ownership").await.unwrap();
        let (entered, entered_rx) = oneshot::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        let (submitted, submitted_rx) = oneshot::channel();
        let (completion, completion_rx) = oneshot::channel();
        let requester = tokio::spawn(async move {
            admission.submit_with_completion(
                "callback ownership",
                move |_, _, _| {
                    let _ = entered.send(());
                    release_rx.recv().unwrap();
                    5_136_u16
                },
                move |result| {
                    let _ = completion.send(result);
                },
            );
            let _ = submitted.send(());
            std::future::pending::<()>().await;
        });
        submitted_rx.await.unwrap();
        entered_rx.await.unwrap();
        requester.abort();
        assert!(requester.await.unwrap_err().is_cancelled());

        release.send(()).unwrap();
        let completed = completion_rx.await.unwrap().unwrap();
        assert_eq!(completed.value, 5_136);

        let mut same_lane = Box::pin(handle.admit("rider", "completion owns lane"));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut same_lane)
                .await
                .is_err(),
            "the callback consumer must retain the canonical lane"
        );
        drop(completed);
        drop(
            tokio::time::timeout(Duration::from_secs(1), same_lane)
                .await
                .unwrap()
                .unwrap(),
        );
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn dead_lane_entries_are_evicted_without_growing_the_table() {
        let root = tempfile::tempdir().unwrap();
        let bootstrap =
            ProfileIoBootstrap::acquire(root.path().to_owned(), ProfileIoLimits::for_tests(2, 2))
                .unwrap();
        let (handle, runtime) = bootstrap.spawn();
        for index in 0..32 {
            let admission = handle
                .admit(&format!("Rider{index}"), "eviction")
                .await
                .unwrap();
            drop(admission);
            assert!(handle.lane_entry_count() <= 2);
        }
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn admission_capacity_includes_lane_waiters_and_unsubmitted_jobs() {
        let root = tempfile::tempdir().unwrap();
        let bootstrap =
            ProfileIoBootstrap::acquire(root.path().to_owned(), ProfileIoLimits::for_tests(3, 1))
                .unwrap();
        let (handle, runtime) = bootstrap.spawn();
        let first = handle.admit("RiderA", "first").await.unwrap();
        let waiting_handle = handle.clone();
        let mut waiting =
            tokio::spawn(async move { waiting_handle.admit("RiderB", "waiting").await });

        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut waiting)
                .await
                .is_err(),
            "an unsubmitted admission must continue to consume bounded queue capacity"
        );
        drop(first);
        drop(waiting.await.unwrap().unwrap());
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn bootstrap_lease_is_exclusive_until_the_runtime_finishes_draining() {
        let root = tempfile::tempdir().unwrap();
        let limits = ProfileIoLimits::for_tests(2, 2);
        let bootstrap = ProfileIoBootstrap::acquire(root.path().to_owned(), limits).unwrap();
        assert!(matches!(
            ProfileIoBootstrap::acquire(root.path().to_owned(), limits),
            Err(ProfileStoreError::RaceRunLeaseBusy { .. })
        ));

        let (handle, runtime) = bootstrap.spawn();
        drop(handle);
        runtime.shutdown().await.unwrap();

        let restarted = ProfileIoBootstrap::acquire(root.path().to_owned(), limits).unwrap();
        drop(restarted);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn worker_panic_closes_admission_and_drains_accepted_work()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let root = tempfile::tempdir()?;
        let bootstrap =
            ProfileIoBootstrap::acquire(root.path().to_owned(), ProfileIoLimits::for_tests(2, 2))?;
        let (handle, runtime) = bootstrap.spawn();
        let panic_admission = handle.admit("Rider", "panic test").await?;
        let accepted_admission = handle.admit("AcceptedRider", "accepted test").await?;
        let (panic_entered, panic_entered_rx) = oneshot::channel();
        let (panic_release, panic_release_rx) = std::sync::mpsc::channel();
        let panic_request = tokio::spawn(panic_admission.run("panic test", move |_, _, _| -> () {
            let _ = panic_entered.send(());
            let _ = panic_release_rx.recv();
            panic!("expected worker panic")
        }));
        let (accepted_entered, accepted_entered_rx) = oneshot::channel();
        let (accepted_release, accepted_release_rx) = std::sync::mpsc::channel();
        let accepted_request =
            tokio::spawn(accepted_admission.run("accepted test", move |_, _, _| {
                let _ = accepted_entered.send(());
                let _ = accepted_release_rx.recv();
                42_u8
            }));
        assert!(panic_entered_rx.await.is_ok());
        assert!(accepted_entered_rx.await.is_ok());
        assert!(panic_release.send(()).is_ok());
        let error = panic_request.await?;
        assert!(matches!(
            error,
            Err(ProfileIoError::WorkerPanicked {
                operation: "panic test",
                ref message,
            }) if message == "expected worker panic"
        ));
        assert!(matches!(
            handle.admit("OtherRider", "after panic").await,
            Err(ProfileIoError::ShuttingDown {
                operation: "after panic"
            })
        ));
        assert!(accepted_release.send(()).is_ok());
        let accepted = accepted_request.await??;
        assert_eq!(accepted.into_parts().0, 42);

        assert!(matches!(
            runtime.shutdown().await,
            Err(ProfileIoShutdownError::Runtime(
                ProfileIoRuntimeError::WorkerPanicked {
                    operation: "panic test",
                    ref message,
                }
            )) if message == "expected worker panic"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn reward_submission_is_tracked_and_an_exact_key_is_applied_once() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let initial = Profile::default();
        let initial_lucci = initial.rider.lucci;
        store.save("RewardRider", &initial).unwrap();

        let bootstrap =
            ProfileIoBootstrap::acquire(root.path().to_owned(), ProfileIoLimits::for_tests(2, 2))
                .unwrap();
        let (handle, runtime) = bootstrap.spawn();
        let epoch = GlobalRaceEpoch::new(1).unwrap();
        let first_task = RewardSettlementTask::for_test(
            RoomId(11),
            epoch,
            NonZeroU64::MIN,
            UserNo::new(7).unwrap(),
            "RewardRider",
            TimeReward::new(3, 7).unwrap(),
        );
        let first = handle.persist_reward(first_task).await.unwrap();
        assert_eq!(first.applied().earned_rp, 3);
        assert_eq!(first.applied().earned_lucci, 7);
        assert_eq!(first.key().room_id(), 11);
        assert_eq!(first.key().race_epoch(), epoch);
        assert_eq!(first.key().user_no(), 7);
        assert_eq!(first.key().canonical_nickname(), Some("rewardrider"));
        let first_applied = first.applied();
        let first_key = first.key().clone();

        let waiting_handle = handle.clone();
        let mut waiting = tokio::spawn(async move {
            waiting_handle
                .admit("rewardrider", "wait behind durable reward receipt")
                .await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut waiting)
                .await
                .is_err(),
            "durable reward receipt released its profile lane before World acknowledgement"
        );
        drop(first);
        drop(waiting.await.unwrap().unwrap());

        let mut externally_updated = store.load_or_create("RewardRider").unwrap().profile;
        externally_updated.rider.p2p_port = 45_136;
        externally_updated.rider.rp = 515_136;
        externally_updated.rider.club_name = "NewerProfile".to_owned();
        externally_updated.rider_item.character = 321;
        store.save("RewardRider", &externally_updated).unwrap();

        let retry_task = RewardSettlementTask::for_test(
            RoomId(11),
            epoch,
            NonZeroU64::new(2).unwrap(),
            UserNo::new(7).unwrap(),
            "rewardrider",
            TimeReward::new(1, 1).unwrap(),
        );
        let retry = handle.persist_reward(retry_task).await.unwrap();
        assert_eq!(retry.applied(), first_applied);
        assert_eq!(retry.key(), &first_key);
        let presentation = retry.profile().unwrap().presentation();
        assert_eq!(presentation.rp(), 515_136);

        let persisted = store.load_or_create("RewardRider").unwrap();
        assert_eq!(persisted.revision, Some(3));
        assert_eq!(persisted.profile.rider.lucci, initial_lucci + 7);
        drop(retry);
        runtime.shutdown().await.unwrap();
    }

    fn retryable_store_error() -> ProfileStoreError {
        ProfileStoreError::Io {
            operation: "test read",
            path: PathBuf::from("profile"),
            source: io::Error::new(io::ErrorKind::Interrupted, "retry"),
        }
    }

    #[test]
    fn profile_store_failures_are_classified_by_retry_semantics() {
        assert_eq!(
            classify_profile_store_error(&retryable_store_error()),
            RewardFailureClassification::Retryable
        );
        assert_eq!(
            classify_profile_store_error(&ProfileStoreError::TransactionContention {
                nickname: "Rider".to_owned(),
                attempts: 4,
            }),
            RewardFailureClassification::Retryable
        );
        assert_eq!(
            classify_profile_store_error(&ProfileStoreError::InternalInvariant {
                message: "test invariant",
            }),
            RewardFailureClassification::InfrastructureFatal
        );
        assert_eq!(
            classify_profile_store_error(&ProfileStoreError::LockPoisoned),
            RewardFailureClassification::InfrastructureFatal
        );
        assert_eq!(
            classify_profile_store_error(&ProfileStoreError::AtomicPublishUnsupported {
                root: PathBuf::from("profile"),
                source: io::Error::new(io::ErrorKind::Unsupported, "no atomic publish"),
            }),
            RewardFailureClassification::InfrastructureFatal
        );
        assert_eq!(
            classify_profile_store_error(&ProfileStoreError::InvalidStoreMetadata {
                path: PathBuf::from("profile"),
                reason: "test corruption",
            }),
            RewardFailureClassification::InfrastructureFatal
        );
        assert_eq!(
            classify_profile_store_error(&ProfileStoreError::RaceRunLeaseStoreMismatch {
                issued_for: PathBuf::from("first"),
                used_with: PathBuf::from("second"),
            }),
            RewardFailureClassification::InfrastructureFatal
        );
        assert_eq!(
            classify_profile_store_error(&ProfileStoreError::ProfileTooLarge {
                path: PathBuf::from("profile"),
                length: 2,
                maximum: 1,
            }),
            RewardFailureClassification::JobFatal
        );
    }

    #[test]
    fn profile_runtime_failures_are_classified_by_retry_semantics() {
        assert_eq!(
            classify_profile_io_error(&ProfileIoError::LaneCapacityExhausted { maximum: 1 }),
            RewardFailureClassification::Retryable
        );
        assert_eq!(
            classify_profile_io_error(&ProfileIoError::RuntimeStopped { operation: "test" }),
            RewardFailureClassification::InfrastructureFatal
        );
        assert_eq!(
            classify_profile_io_error(&ProfileIoError::LaneRegistryPoisoned),
            RewardFailureClassification::InfrastructureFatal
        );
        assert_eq!(
            classify_profile_io_error(&ProfileIoError::ShuttingDown { operation: "test" }),
            RewardFailureClassification::InfrastructureFatal
        );
        assert_eq!(
            classify_profile_io_error(&ProfileIoError::WorkerPanicked {
                operation: "test",
                message: "worker failed".to_owned(),
            }),
            RewardFailureClassification::InfrastructureFatal
        );
        assert_eq!(
            classify_profile_io_error(&ProfileIoError::CompletionTypeMismatch {
                operation: "test",
            }),
            RewardFailureClassification::InfrastructureFatal
        );
    }

    #[test]
    fn nested_reward_failures_are_classified_by_retry_semantics() {
        assert_eq!(
            classify_reward_job_error(&ProfileRewardJobError::Recipient(
                RaceRewardRecipientError::Store(retryable_store_error())
            )),
            RewardFailureClassification::Retryable
        );
        assert_eq!(
            classify_reward_job_error(&ProfileRewardJobError::Recipient(
                RaceRewardRecipientError::Store(ProfileStoreError::LockPoisoned)
            )),
            RewardFailureClassification::InfrastructureFatal
        );
        assert_eq!(
            classify_reward_job_error(&ProfileRewardJobError::Recipient(
                RaceRewardRecipientError::ProfileMissing {
                    nickname: "missing".to_owned(),
                }
            )),
            RewardFailureClassification::JobFatal
        );
        assert_eq!(
            classify_reward_job_error(&ProfileRewardJobError::Recipient(
                RaceRewardRecipientError::ZeroUserNo
            )),
            RewardFailureClassification::InfrastructureFatal
        );
        assert_eq!(
            classify_reward_job_error(&ProfileRewardJobError::Key(RaceRewardKeyError::ZeroRoomId)),
            RewardFailureClassification::InfrastructureFatal
        );
        assert_eq!(
            classify_reward_job_error(&ProfileRewardJobError::Persistence(
                RaceRewardPersistenceError::Binding(RaceRewardBindingError::MissingRunGeneration)
            )),
            RewardFailureClassification::InfrastructureFatal
        );
        assert_eq!(
            classify_reward_job_error(&ProfileRewardJobError::Persistence(
                RaceRewardPersistenceError::RejectedButDurabilityUncertain {
                    rejection: Box::new(RaceRewardPersistenceError::Binding(
                        RaceRewardBindingError::MissingCanonicalNickname,
                    )),
                    durability: retryable_store_error(),
                }
            )),
            RewardFailureClassification::InfrastructureFatal
        );
        assert_eq!(
            classify_reward_job_error(&ProfileRewardJobError::Persistence(
                RaceRewardPersistenceError::RejectedButDurabilityUncertain {
                    rejection: Box::new(
                        RaceRewardPersistenceError::Store(retryable_store_error(),)
                    ),
                    durability: retryable_store_error(),
                }
            )),
            RewardFailureClassification::JobFatal
        );
    }
}
