//! Exact P5136 reward arithmetic and profile mutation.
//!
//! Random-number generation stays outside this module. A server can plan an
//! exact reward once, then use [`apply_race_reward_once`] to commit it through
//! the versioned [`crate::ProfileStore`] without applying a retry twice.

use std::{
    num::{NonZeroU32, NonZeroU64},
    path::PathBuf,
};

use p5136_core::nickname::{NicknameError, canonical_nickname_key, normalize_nickname};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use thiserror::Error;

use crate::{
    Profile, ProfileMutation, ProfileStore, ProfileStoreError, ProfileStoreId, ProfileTransaction,
    RaceRunGeneration, RaceRunLease,
};

pub const DEFAULT_RP: u32 = 20_000_000;
pub const MAX_TIME_REWARD_RP_ROLL: u8 = 50;
pub const MAX_TIME_REWARD_LUCCI_ROLL: u16 = 500;
pub const TIME_REWARD_BASELINE_RANK: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct TimeReward {
    earned_rp: u32,
    earned_lucci: u32,
}

impl TimeReward {
    pub fn new(earned_rp: u32, earned_lucci: u32) -> Result<Self, RewardAmountError> {
        if earned_rp > u32::from(MAX_TIME_REWARD_RP_ROLL) {
            return Err(RewardAmountError::Rp {
                actual: earned_rp,
                maximum: u32::from(MAX_TIME_REWARD_RP_ROLL),
            });
        }
        if earned_lucci > u32::from(MAX_TIME_REWARD_LUCCI_ROLL) {
            return Err(RewardAmountError::Lucci {
                actual: earned_lucci,
                maximum: u32::from(MAX_TIME_REWARD_LUCCI_ROLL),
            });
        }
        Ok(Self {
            earned_rp,
            earned_lucci,
        })
    }

    #[must_use]
    pub const fn earned_rp(self) -> u32 {
        self.earned_rp
    }

    #[must_use]
    pub const fn earned_lucci(self) -> u32 {
        self.earned_lucci
    }
}

impl<'de> Deserialize<'de> for TimeReward {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase", deny_unknown_fields)]
        struct PersistedTimeReward {
            earned_rp: u32,
            earned_lucci: u32,
        }

        let persisted = PersistedTimeReward::deserialize(deserializer)?;
        Self::new(persisted.earned_rp, persisted.earned_lucci).map_err(D::Error::custom)
    }
}

/// The exact economy values returned in the P5136 race-finish packets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct AppliedTimeReward {
    pub current_rp: u32,
    pub earned_rp: u32,
    pub earned_lucci: u32,
    pub current_lucci: u32,
}

/// The legacy random process-boot identifier used by the first receipt schema.
///
/// New reward keys use a durably ordered [`RaceRunGeneration`]. This type
/// remains deserializable so profiles written by the earlier schema stay
/// readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct RaceRunId([u8; 16]);

impl RaceRunId {
    /// Constructs a run ID, returning `None` for the reserved all-zero value.
    #[must_use]
    pub const fn new(bytes: [u8; 16]) -> Option<Self> {
        if u128::from_le_bytes(bytes) == 0 {
            None
        } else {
            Some(Self(bytes))
        }
    }

    #[must_use]
    pub const fn get(self) -> [u8; 16] {
        self.0
    }
}

impl<'de> Deserialize<'de> for RaceRunId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes = <[u8; 16]>::deserialize(deserializer)?;
        Self::new(bytes).ok_or_else(|| D::Error::custom("race run ID must not be all zero"))
    }
}

/// A monotonically increasing, process-global race epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GlobalRaceEpoch(NonZeroU64);

impl GlobalRaceEpoch {
    #[must_use]
    pub const fn new(value: u64) -> Option<Self> {
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

/// A store-issued binding between one canonical profile nickname and user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RaceRewardRecipient {
    canonical_store_root: PathBuf,
    store_id: ProfileStoreId,
    run_generation: RaceRunGeneration,
    nickname: String,
    canonical_nickname: String,
    user_no: NonZeroU32,
}

impl RaceRewardRecipient {
    #[must_use]
    pub fn nickname(&self) -> &str {
        &self.nickname
    }

    #[must_use]
    pub fn canonical_nickname(&self) -> &str {
        &self.canonical_nickname
    }

    #[must_use]
    pub const fn user_no(&self) -> u32 {
        self.user_no.get()
    }

    #[must_use]
    pub const fn run_generation(&self) -> RaceRunGeneration {
        self.run_generation
    }
}

impl ProfileStore {
    /// Validates and binds a canonical profile recipient to this store root.
    pub fn bind_race_reward_recipient(
        &self,
        lease: &RaceRunLease,
        nickname: &str,
        user_no: u32,
    ) -> Result<RaceRewardRecipient, RaceRewardRecipientError> {
        self.validate_race_run_lease(lease)?;
        let nickname = Self::normalize_storage_nickname(nickname)?;
        let user_no = NonZeroU32::new(user_no).ok_or(RaceRewardRecipientError::ZeroUserNo)?;
        if !self.profile_exists(&nickname)? {
            return Err(RaceRewardRecipientError::ProfileMissing { nickname });
        }
        Ok(RaceRewardRecipient {
            canonical_store_root: lease.root().to_owned(),
            store_id: lease.store_id(),
            run_generation: lease.generation(),
            canonical_nickname: canonical_nickname_key(&nickname),
            nickname,
            user_no,
        })
    }
}

#[derive(Debug, Error)]
pub enum RaceRewardRecipientError {
    #[error(transparent)]
    Store(#[from] ProfileStoreError),

    #[error(transparent)]
    Nickname(#[from] NicknameError),

    #[error("race reward user number must not be zero")]
    ZeroUserNo,

    #[error("race reward profile {nickname:?} does not exist")]
    ProfileMissing { nickname: String },
}

/// A globally unambiguous reward key for one bound user and one race.
///
/// The private enum makes the legacy and current persisted schemas disjoint:
/// a key can never contain a partial mixture of their fields.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RaceRewardKey {
    kind: RaceRewardKeyKind,
    room_id: NonZeroU32,
    race_epoch: GlobalRaceEpoch,
    user_no: NonZeroU32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum RaceRewardKeyKind {
    Legacy {
        run_id: RaceRunId,
    },
    Current {
        run_generation: RaceRunGeneration,
        store_id: ProfileStoreId,
        canonical_nickname: String,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct CurrentRaceRewardKeyRef<'a> {
    run_generation: RaceRunGeneration,
    store_id: ProfileStoreId,
    room_id: NonZeroU32,
    race_epoch: GlobalRaceEpoch,
    user_no: NonZeroU32,
    canonical_nickname: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct LegacyRaceRewardKeyRef {
    run_id: RaceRunId,
    room_id: NonZeroU32,
    race_epoch: GlobalRaceEpoch,
    user_no: NonZeroU32,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct CurrentRaceRewardKeyDto {
    run_generation: RaceRunGeneration,
    store_id: ProfileStoreId,
    room_id: NonZeroU32,
    race_epoch: GlobalRaceEpoch,
    user_no: NonZeroU32,
    canonical_nickname: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct LegacyRaceRewardKeyDto {
    run_id: RaceRunId,
    room_id: NonZeroU32,
    race_epoch: GlobalRaceEpoch,
    user_no: NonZeroU32,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RaceRewardKeyDto {
    Current(CurrentRaceRewardKeyDto),
    Legacy(LegacyRaceRewardKeyDto),
}

impl RaceRewardKey {
    /// Constructs a key from a store-issued recipient and durable run
    /// generation.
    pub fn new(
        recipient: &RaceRewardRecipient,
        lease: &RaceRunLease,
        room_id: u32,
        race_epoch: GlobalRaceEpoch,
    ) -> Result<Self, RaceRewardKeyError> {
        if recipient.canonical_store_root != lease.root() || recipient.store_id != lease.store_id()
        {
            return Err(RaceRewardKeyError::RecipientLeaseStoreMismatch);
        }
        if recipient.run_generation != lease.generation() {
            return Err(RaceRewardKeyError::RecipientLeaseGenerationMismatch {
                recipient_generation: recipient.run_generation,
                lease_generation: lease.generation(),
            });
        }
        let room_id = NonZeroU32::new(room_id).ok_or(RaceRewardKeyError::ZeroRoomId)?;
        Ok(Self {
            kind: RaceRewardKeyKind::Current {
                run_generation: lease.generation(),
                store_id: lease.store_id(),
                canonical_nickname: recipient.canonical_nickname.clone(),
            },
            room_id,
            race_epoch,
            user_no: recipient.user_no,
        })
    }

    #[must_use]
    pub const fn run_generation(&self) -> Option<RaceRunGeneration> {
        match &self.kind {
            RaceRewardKeyKind::Legacy { .. } => None,
            RaceRewardKeyKind::Current { run_generation, .. } => Some(*run_generation),
        }
    }

    #[must_use]
    pub const fn legacy_run_id(&self) -> Option<RaceRunId> {
        match &self.kind {
            RaceRewardKeyKind::Legacy { run_id } => Some(*run_id),
            RaceRewardKeyKind::Current { .. } => None,
        }
    }

    #[must_use]
    pub const fn store_id(&self) -> Option<ProfileStoreId> {
        match &self.kind {
            RaceRewardKeyKind::Legacy { .. } => None,
            RaceRewardKeyKind::Current { store_id, .. } => Some(*store_id),
        }
    }

    #[must_use]
    pub const fn room_id(&self) -> u32 {
        self.room_id.get()
    }

    #[must_use]
    pub const fn user_no(&self) -> u32 {
        self.user_no.get()
    }

    #[must_use]
    pub const fn race_epoch(&self) -> GlobalRaceEpoch {
        self.race_epoch
    }

    #[must_use]
    pub fn canonical_nickname(&self) -> Option<&str> {
        match &self.kind {
            RaceRewardKeyKind::Legacy { .. } => None,
            RaceRewardKeyKind::Current {
                canonical_nickname, ..
            } => Some(canonical_nickname),
        }
    }
}

impl Serialize for RaceRewardKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match &self.kind {
            RaceRewardKeyKind::Current {
                run_generation,
                store_id,
                canonical_nickname,
            } => CurrentRaceRewardKeyRef {
                run_generation: *run_generation,
                store_id: *store_id,
                room_id: self.room_id,
                race_epoch: self.race_epoch,
                user_no: self.user_no,
                canonical_nickname,
            }
            .serialize(serializer),
            RaceRewardKeyKind::Legacy { run_id } => LegacyRaceRewardKeyRef {
                run_id: *run_id,
                room_id: self.room_id,
                race_epoch: self.race_epoch,
                user_no: self.user_no,
            }
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for RaceRewardKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match RaceRewardKeyDto::deserialize(deserializer)? {
            RaceRewardKeyDto::Current(current) => {
                let normalized =
                    normalize_nickname(&current.canonical_nickname).map_err(D::Error::custom)?;
                if canonical_nickname_key(&normalized) != current.canonical_nickname {
                    return Err(D::Error::custom(
                        "current race reward key nickname must already be canonical",
                    ));
                }
                Ok(Self {
                    kind: RaceRewardKeyKind::Current {
                        run_generation: current.run_generation,
                        store_id: current.store_id,
                        canonical_nickname: current.canonical_nickname,
                    },
                    room_id: current.room_id,
                    race_epoch: current.race_epoch,
                    user_no: current.user_no,
                })
            }
            RaceRewardKeyDto::Legacy(legacy) => Ok(Self {
                kind: RaceRewardKeyKind::Legacy {
                    run_id: legacy.run_id,
                },
                room_id: legacy.room_id,
                race_epoch: legacy.race_epoch,
                user_no: legacy.user_no,
            }),
        }
    }
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum RaceRewardKeyError {
    #[error("race reward room ID must not be zero")]
    ZeroRoomId,

    #[error("race reward recipient and race-run lease belong to different profile stores")]
    RecipientLeaseStoreMismatch,

    #[error(
        "race reward recipient belongs to run generation {recipient_generation:?}, not lease generation {lease_generation:?}"
    )]
    RecipientLeaseGenerationMismatch {
        recipient_generation: RaceRunGeneration,
        lease_generation: RaceRunGeneration,
    },
}

/// The single bounded receipt retained in a profile.
///
/// `applied` is persisted rather than recomputed so a retry returns the exact
/// values that were used for the original final packets, even if it proposes a
/// different reward.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PersistedRaceRewardReceipt {
    pub key: RaceRewardKey,
    pub applied: AppliedTimeReward,
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum RaceRewardOrderError {
    #[error(
        "race reward run generation {attempted:?} is older than the latest generation {latest:?}"
    )]
    StaleRunGeneration {
        attempted: RaceRunGeneration,
        latest: RaceRunGeneration,
    },

    #[error(
        "race reward epoch {attempted:?} is older than the latest epoch {latest:?} in this run"
    )]
    StaleEpoch {
        attempted: GlobalRaceEpoch,
        latest: GlobalRaceEpoch,
    },

    #[error(
        "race reward room {attempted_room_id} conflicts with stored room {stored_room_id} at run generation {run_generation:?}, epoch {race_epoch:?}, user {user_no}"
    )]
    ConflictingKeyAtEpoch {
        run_generation: RaceRunGeneration,
        race_epoch: GlobalRaceEpoch,
        user_no: u32,
        attempted_room_id: u32,
        stored_room_id: u32,
    },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RaceRewardBindingError {
    #[error("race reward recipient was issued for store root {issued_for}, not {used_with}")]
    RecipientStoreMismatch {
        issued_for: PathBuf,
        used_with: PathBuf,
    },

    #[error(
        "race reward recipient belongs to run generation {recipient_generation:?}, not active generation {lease_generation:?}"
    )]
    RecipientRunGenerationMismatch {
        recipient_generation: RaceRunGeneration,
        lease_generation: RaceRunGeneration,
    },

    #[error("race reward key belongs to store {key_store:?}, not active store {lease_store:?}")]
    KeyStoreMismatch {
        key_store: Option<ProfileStoreId>,
        lease_store: ProfileStoreId,
    },

    #[error(
        "race reward key uses run generation {key_generation:?}, not active generation {lease_generation:?}"
    )]
    KeyRunGenerationMismatch {
        key_generation: Option<RaceRunGeneration>,
        lease_generation: RaceRunGeneration,
    },

    #[error("race reward key has no durable run generation")]
    MissingRunGeneration,

    #[error("race reward key has no canonical nickname binding")]
    MissingCanonicalNickname,

    #[error(
        "{context} recipient mismatch: expected {expected_nickname:?}/{expected_user_no}, found {actual_nickname:?}/{actual_user_no}"
    )]
    RecipientMismatch {
        context: &'static str,
        expected_nickname: String,
        expected_user_no: u32,
        actual_nickname: Option<String>,
        actual_user_no: u32,
    },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum InvalidStoredReceiptError {
    #[error("persisted earned RP {actual} exceeds {maximum}")]
    EarnedRpOutOfRange { actual: u32, maximum: u32 },

    #[error("persisted earned Lucci {actual} exceeds {maximum}")]
    EarnedLucciOutOfRange { actual: u32, maximum: u32 },

    #[error("persisted current RP {actual} is not the required normalized value {expected}")]
    CurrentRpNotNormalized { actual: u32, expected: u32 },
}

#[derive(Debug, Error)]
pub enum RaceRewardPersistenceError {
    #[error(transparent)]
    Store(#[from] ProfileStoreError),

    #[error(transparent)]
    Order(#[from] RaceRewardOrderError),

    #[error(transparent)]
    Binding(#[from] RaceRewardBindingError),

    #[error(transparent)]
    InvalidStoredReceipt(#[from] InvalidStoredReceiptError),

    #[error(
        "race reward was rejected ({rejection}) while profile directory durability was also uncertain ({durability})"
    )]
    RejectedButDurabilityUncertain {
        rejection: Box<RaceRewardPersistenceError>,
        durability: ProfileStoreError,
    },
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum RewardAmountError {
    #[error("RP reward amount {actual} exceeds {maximum}")]
    Rp { actual: u32, maximum: u32 },

    #[error("Lucci reward amount {actual} exceeds {maximum}")]
    Lucci { actual: u32, maximum: u32 },
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum RewardRollError {
    #[error("RP reward roll {actual} is outside 0..={maximum}")]
    Rp { actual: u8, maximum: u8 },

    #[error("Lucci reward roll {actual} is outside 0..={maximum}")]
    Lucci { actual: u16, maximum: u16 },
}

/// Reproduces `TimeReward.Reward` after its two inclusive random draws.
///
/// The original algorithm gives ranks zero through seven a decreasing bonus,
/// then clamps the final RP and Lucci values back to 50 and 500.
pub fn time_reward_from_rolls(
    player_ranking: usize,
    rp_roll: u8,
    lucci_roll: u16,
) -> Result<TimeReward, RewardRollError> {
    if rp_roll > MAX_TIME_REWARD_RP_ROLL {
        return Err(RewardRollError::Rp {
            actual: rp_roll,
            maximum: MAX_TIME_REWARD_RP_ROLL,
        });
    }
    if lucci_roll > MAX_TIME_REWARD_LUCCI_ROLL {
        return Err(RewardRollError::Lucci {
            actual: lucci_roll,
            maximum: MAX_TIME_REWARD_LUCCI_ROLL,
        });
    }

    let bonus = TIME_REWARD_BASELINE_RANK.saturating_sub(player_ranking);
    let bonus = u32::try_from(bonus).unwrap_or_default();
    let rp_bonus = bonus / 3;
    let lucci_bonus = bonus * 3;
    Ok(TimeReward {
        earned_rp: (u32::from(rp_roll) + rp_bonus).min(u32::from(MAX_TIME_REWARD_RP_ROLL)),
        earned_lucci: (u32::from(lucci_roll) + lucci_bonus)
            .min(u32::from(MAX_TIME_REWARD_LUCCI_ROLL)),
    })
}

/// Applies a race reward exactly as the P5136 C# server does.
///
/// Earned RP is reported on the wire but the persisted/current RP is always
/// normalized to 20,000,000. Lucci uses the C# default unchecked `uint`
/// addition semantics.
pub fn apply_time_reward(profile: &mut Profile, reward: TimeReward) -> AppliedTimeReward {
    profile.rider.rp = DEFAULT_RP;
    profile.rider.lucci = profile.rider.lucci.wrapping_add(reward.earned_lucci);
    AppliedTimeReward {
        current_rp: profile.rider.rp,
        earned_rp: reward.earned_rp,
        earned_lucci: reward.earned_lucci,
        current_lucci: profile.rider.lucci,
    }
}

/// Applies one already-planned exact reward under the profile transaction lock.
///
/// An exact-key retry returns the persisted receipt without publishing a new
/// revision, regardless of the newly proposed reward. Its unchanged
/// transaction explicitly re-syncs the profile directory before it can return
/// an ordinary durable result.
///
/// Older run generations can never replace a newer receipt, which prevents an
/// `A -> B -> retry A` replay from crediting A twice. Within one generation,
/// epochs remain strictly monotonic. The World coordinator must serialize at
/// most one outstanding reward per recipient: a never-applied older epoch that
/// arrives after a newer one is deliberately rejected as [`RaceRewardOrderError::StaleEpoch`].
pub fn apply_race_reward_once(
    store: &ProfileStore,
    lease: &RaceRunLease,
    recipient: &RaceRewardRecipient,
    key: &RaceRewardKey,
    proposed_reward: TimeReward,
) -> Result<ProfileTransaction<PersistedRaceRewardReceipt>, RaceRewardPersistenceError> {
    store.validate_race_run_lease(lease)?;
    validate_attempted_recipient(lease, recipient, key)?;
    let attempted_generation = key
        .run_generation()
        .ok_or(RaceRewardBindingError::MissingRunGeneration)?;
    let transaction = store.transaction(recipient.nickname(), |profile| {
        if let Some(stored) = profile.race_reward_receipt.as_ref() {
            if let Err(error) = validate_stored_receipt(stored) {
                return ProfileMutation::Unchanged(Err(error.into()));
            }
            if let Err(error) = validate_stored_recipient(recipient, &stored.key) {
                return ProfileMutation::Unchanged(Err(error.into()));
            }
            if &stored.key == key {
                return ProfileMutation::Unchanged(Ok(stored.clone()));
            }
            if let Some(stored_generation) = stored.key.run_generation() {
                if attempted_generation < stored_generation {
                    return ProfileMutation::Unchanged(Err(
                        RaceRewardOrderError::StaleRunGeneration {
                            attempted: attempted_generation,
                            latest: stored_generation,
                        }
                        .into(),
                    ));
                }
                if attempted_generation == stored_generation {
                    if key.user_no != stored.key.user_no {
                        return ProfileMutation::Unchanged(Err(recipient_mismatch(
                            "same-generation persisted",
                            recipient,
                            &stored.key,
                        )
                        .into()));
                    }
                    if key.race_epoch() < stored.key.race_epoch() {
                        return ProfileMutation::Unchanged(Err(RaceRewardOrderError::StaleEpoch {
                            attempted: key.race_epoch(),
                            latest: stored.key.race_epoch(),
                        }
                        .into()));
                    }
                    if key.race_epoch() == stored.key.race_epoch() {
                        return ProfileMutation::Unchanged(Err(
                            RaceRewardOrderError::ConflictingKeyAtEpoch {
                                run_generation: attempted_generation,
                                race_epoch: key.race_epoch(),
                                user_no: key.user_no(),
                                attempted_room_id: key.room_id(),
                                stored_room_id: stored.key.room_id(),
                            }
                            .into(),
                        ));
                    }
                }
            }
        }

        let mut next = profile.clone();
        let receipt = PersistedRaceRewardReceipt {
            key: key.clone(),
            applied: apply_time_reward(&mut next, proposed_reward),
        };
        next.race_reward_receipt = Some(receipt.clone());
        ProfileMutation::changed(Ok(receipt), next)
    })?;

    resolve_reward_transaction(transaction)
}

fn validate_attempted_recipient(
    lease: &RaceRunLease,
    recipient: &RaceRewardRecipient,
    key: &RaceRewardKey,
) -> Result<(), RaceRewardBindingError> {
    if recipient.canonical_store_root != lease.root() || recipient.store_id != lease.store_id() {
        return Err(RaceRewardBindingError::RecipientStoreMismatch {
            issued_for: recipient.canonical_store_root.clone(),
            used_with: lease.root().to_owned(),
        });
    }
    if recipient.run_generation != lease.generation() {
        return Err(RaceRewardBindingError::RecipientRunGenerationMismatch {
            recipient_generation: recipient.run_generation,
            lease_generation: lease.generation(),
        });
    }
    if key.store_id() != Some(lease.store_id()) {
        return Err(RaceRewardBindingError::KeyStoreMismatch {
            key_store: key.store_id(),
            lease_store: lease.store_id(),
        });
    }
    if key.run_generation() != Some(lease.generation()) {
        return Err(RaceRewardBindingError::KeyRunGenerationMismatch {
            key_generation: key.run_generation(),
            lease_generation: lease.generation(),
        });
    }
    let Some(canonical_nickname) = key.canonical_nickname() else {
        return Err(RaceRewardBindingError::MissingCanonicalNickname);
    };
    if canonical_nickname != recipient.canonical_nickname || key.user_no != recipient.user_no {
        return Err(recipient_mismatch("attempted", recipient, key));
    }
    Ok(())
}

fn validate_stored_recipient(
    recipient: &RaceRewardRecipient,
    key: &RaceRewardKey,
) -> Result<(), RaceRewardBindingError> {
    if let Some(store_id) = key.store_id()
        && store_id != recipient.store_id
    {
        return Err(RaceRewardBindingError::KeyStoreMismatch {
            key_store: Some(store_id),
            lease_store: recipient.store_id,
        });
    }
    let nickname_matches = key
        .canonical_nickname()
        .is_none_or(|nickname| nickname == recipient.canonical_nickname);
    if nickname_matches && key.user_no == recipient.user_no {
        Ok(())
    } else {
        Err(recipient_mismatch("persisted", recipient, key))
    }
}

fn validate_stored_receipt(
    receipt: &PersistedRaceRewardReceipt,
) -> Result<(), InvalidStoredReceiptError> {
    let maximum_rp = u32::from(MAX_TIME_REWARD_RP_ROLL);
    if receipt.applied.earned_rp > maximum_rp {
        return Err(InvalidStoredReceiptError::EarnedRpOutOfRange {
            actual: receipt.applied.earned_rp,
            maximum: maximum_rp,
        });
    }
    let maximum_lucci = u32::from(MAX_TIME_REWARD_LUCCI_ROLL);
    if receipt.applied.earned_lucci > maximum_lucci {
        return Err(InvalidStoredReceiptError::EarnedLucciOutOfRange {
            actual: receipt.applied.earned_lucci,
            maximum: maximum_lucci,
        });
    }
    if receipt.applied.current_rp != DEFAULT_RP {
        return Err(InvalidStoredReceiptError::CurrentRpNotNormalized {
            actual: receipt.applied.current_rp,
            expected: DEFAULT_RP,
        });
    }
    Ok(())
}

fn recipient_mismatch(
    context: &'static str,
    recipient: &RaceRewardRecipient,
    key: &RaceRewardKey,
) -> RaceRewardBindingError {
    RaceRewardBindingError::RecipientMismatch {
        context,
        expected_nickname: recipient.canonical_nickname.clone(),
        expected_user_no: recipient.user_no(),
        actual_nickname: key.canonical_nickname().map(str::to_owned),
        actual_user_no: key.user_no(),
    }
}

fn resolve_reward_transaction(
    transaction: ProfileTransaction<Result<PersistedRaceRewardReceipt, RaceRewardPersistenceError>>,
) -> Result<ProfileTransaction<PersistedRaceRewardReceipt>, RaceRewardPersistenceError> {
    match transaction {
        ProfileTransaction::Unchanged { value, profile } => Ok(ProfileTransaction::Unchanged {
            value: value?,
            profile,
        }),
        ProfileTransaction::Committed {
            value,
            profile,
            saved,
        } => Ok(ProfileTransaction::Committed {
            value: value?,
            profile,
            saved,
        }),
        ProfileTransaction::CommittedButDurabilityUncertain {
            value,
            profile,
            saved,
            error,
        } => match value {
            Ok(value) => Ok(ProfileTransaction::CommittedButDurabilityUncertain {
                value,
                profile,
                saved,
                error,
            }),
            Err(rejection) => Err(RaceRewardPersistenceError::RejectedButDurabilityUncertain {
                rejection: Box::new(rejection),
                durability: error,
            }),
        },
    }
}

/// Reproduces `TimeReward.FinishReward` for the two legacy reward types.
#[must_use]
pub const fn finish_reward(reward_type: u8) -> TimeReward {
    match reward_type {
        0 => TimeReward {
            earned_rp: 10,
            earned_lucci: 20,
        },
        1 => TimeReward {
            earned_rp: 20,
            earned_lucci: 50,
        },
        _ => TimeReward {
            earned_rp: 0,
            earned_lucci: 0,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use tempfile::tempdir;

    use super::{
        DEFAULT_RP, GlobalRaceEpoch, RaceRewardBindingError, RaceRewardKey, RaceRewardKeyError,
        RaceRewardOrderError, RaceRewardPersistenceError, RaceRewardRecipient,
        RaceRewardRecipientError, RaceRunId, RewardRollError, TimeReward, apply_race_reward_once,
        apply_time_reward, finish_reward, time_reward_from_rolls,
    };
    use crate::{
        Profile, ProfileStore, ProfileStoreError, ProfileTransaction, RaceRunGeneration,
        RaceRunLease,
    };

    fn bind_recipient(
        store: &ProfileStore,
        lease: &RaceRunLease,
        nickname: &str,
        user_no: u32,
    ) -> RaceRewardRecipient {
        store
            .bind_race_reward_recipient(lease, nickname, user_no)
            .unwrap()
    }

    fn reward_key(
        recipient: &RaceRewardRecipient,
        lease: &RaceRunLease,
        room_id: u32,
        epoch: u64,
    ) -> RaceRewardKey {
        RaceRewardKey::new(
            recipient,
            lease,
            room_id,
            GlobalRaceEpoch::new(epoch).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn rank_bonus_and_clamps_match_time_reward_csharp() {
        assert_eq!(
            time_reward_from_rolls(0, 0, 0).unwrap(),
            TimeReward {
                earned_rp: 2,
                earned_lucci: 24,
            }
        );
        assert_eq!(
            time_reward_from_rolls(7, 10, 20).unwrap(),
            TimeReward {
                earned_rp: 10,
                earned_lucci: 23,
            }
        );
        for rank in [8, 9, usize::MAX] {
            assert_eq!(
                time_reward_from_rolls(rank, 10, 20).unwrap(),
                TimeReward {
                    earned_rp: 10,
                    earned_lucci: 20,
                }
            );
        }
        assert_eq!(
            time_reward_from_rolls(0, 50, 500).unwrap(),
            TimeReward {
                earned_rp: 50,
                earned_lucci: 500,
            }
        );
    }

    #[test]
    fn invalid_injected_rolls_are_rejected() {
        assert_eq!(
            time_reward_from_rolls(0, 51, 0),
            Err(RewardRollError::Rp {
                actual: 51,
                maximum: 50,
            })
        );
        assert_eq!(
            time_reward_from_rolls(0, 0, 501),
            Err(RewardRollError::Lucci {
                actual: 501,
                maximum: 500,
            })
        );
    }

    #[test]
    fn profile_mutation_normalizes_rp_and_wraps_lucci_like_unchecked_uint() {
        let mut profile = Profile::default();
        profile.rider.rp = 123;
        profile.rider.lucci = u32::MAX - 4;
        let applied = apply_time_reward(
            &mut profile,
            TimeReward {
                earned_rp: 37,
                earned_lucci: 10,
            },
        );
        assert_eq!(profile.rider.rp, DEFAULT_RP);
        assert_eq!(profile.rider.lucci, 5);
        assert_eq!(applied.current_rp, DEFAULT_RP);
        assert_eq!(applied.earned_rp, 37);
        assert_eq!(applied.earned_lucci, 10);
        assert_eq!(applied.current_lucci, 5);
    }

    #[test]
    fn finish_reward_matches_the_three_csharp_branches() {
        assert_eq!(
            finish_reward(0),
            TimeReward {
                earned_rp: 10,
                earned_lucci: 20,
            }
        );
        assert_eq!(
            finish_reward(1),
            TimeReward {
                earned_rp: 20,
                earned_lucci: 50,
            }
        );
        for reward_type in [2, u8::MAX] {
            assert_eq!(
                finish_reward(reward_type),
                TimeReward {
                    earned_rp: 0,
                    earned_lucci: 0,
                }
            );
        }
    }

    #[test]
    fn persistence_key_rejects_reserved_values_and_reads_legacy_run_ids() {
        assert_eq!(RaceRunId::new([0; 16]), None);
        assert_eq!(RaceRunGeneration::new(0), None);
        assert_eq!(GlobalRaceEpoch::new(0), None);
        assert!(serde_json::from_str::<RaceRunId>("[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]").is_err());
        assert!(serde_json::from_str::<RaceRunGeneration>("0").is_err());
        assert!(serde_json::from_str::<GlobalRaceEpoch>("0").is_err());

        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let lease = store.acquire_race_run_lease().unwrap();
        store.load_or_create("Rider").unwrap();
        assert!(matches!(
            store.bind_race_reward_recipient(&lease, "Rider", 0),
            Err(RaceRewardRecipientError::ZeroUserNo)
        ));
        let recipient = bind_recipient(&store, &lease, "Rider", 42);
        let epoch = GlobalRaceEpoch::new(1).unwrap();
        assert_eq!(
            RaceRewardKey::new(&recipient, &lease, 0, epoch),
            Err(RaceRewardKeyError::ZeroRoomId)
        );
        let current = reward_key(&recipient, &lease, 7, 9);
        let encoded = serde_json::to_value(&current).unwrap();
        assert_eq!(encoded["RunGeneration"], 1);
        assert_eq!(encoded["CanonicalNickname"], "rider");
        assert!(encoded.get("StoreId").is_some());
        assert!(encoded.get("RunId").is_none());
        assert_eq!(
            serde_json::from_value::<RaceRewardKey>(encoded.clone()).unwrap(),
            current
        );

        let legacy: RaceRewardKey = serde_json::from_value(serde_json::json!({
            "RunId": [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
            "RoomId": 7,
            "RaceEpoch": 9,
            "UserNo": 42
        }))
        .unwrap();
        assert_eq!(legacy.run_generation(), None);
        assert!(legacy.legacy_run_id().is_some());
        assert_eq!(legacy.canonical_nickname(), None);
        assert_eq!(legacy.room_id(), 7);
        assert_eq!(legacy.user_no(), 42);

        let mut malformed = Vec::new();
        malformed.push(serde_json::json!({
            "RoomId": 7, "RaceEpoch": 9, "UserNo": 42
        }));
        let mut both = encoded.clone();
        both["RunId"] = serde_json::json!([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        malformed.push(both);
        let mut missing_store = encoded.clone();
        missing_store.as_object_mut().unwrap().remove("StoreId");
        malformed.push(missing_store);
        let mut missing_nickname = encoded.clone();
        missing_nickname
            .as_object_mut()
            .unwrap()
            .remove("CanonicalNickname");
        malformed.push(missing_nickname);
        let mut noncanonical = encoded;
        noncanonical["CanonicalNickname"] = serde_json::json!("Rider");
        malformed.push(noncanonical);
        let mut current_with_null_legacy = serde_json::to_value(&current).unwrap();
        current_with_null_legacy["RunId"] = serde_json::Value::Null;
        malformed.push(current_with_null_legacy);
        malformed.push(serde_json::json!({
            "RunId": [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
            "CanonicalNickname": "rider",
            "RoomId": 7, "RaceEpoch": 9, "UserNo": 42
        }));
        malformed.push(serde_json::json!({
            "RunId": [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
            "RunGeneration": null,
            "StoreId": null,
            "CanonicalNickname": null,
            "RoomId": 7, "RaceEpoch": 9, "UserNo": 42
        }));
        for value in malformed {
            assert!(serde_json::from_value::<RaceRewardKey>(value).is_err());
        }
        assert!(
            serde_json::from_value::<TimeReward>(
                serde_json::json!({"EarnedRp": 51, "EarnedLucci": 0})
            )
            .is_err()
        );
    }

    #[test]
    fn duplicate_key_returns_the_stored_outcome_without_mutating_economy_or_revision() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let lease = store.acquire_race_run_lease().unwrap();
        store.load_or_create("Rider").unwrap();
        let recipient = bind_recipient(&store, &lease, "Rider", 42);
        let key = reward_key(&recipient, &lease, 7, 11);

        let first = apply_race_reward_once(
            &store,
            &lease,
            &recipient,
            &key,
            TimeReward {
                earned_rp: 37,
                earned_lucci: 25,
            },
        )
        .unwrap();
        let receipt = match first {
            ProfileTransaction::Committed {
                value,
                profile,
                saved,
            } => {
                assert_eq!(saved.revision, 2);
                assert_eq!(profile.rider.lucci, 1_000_025);
                assert_eq!(profile.race_reward_receipt, Some(value.clone()));
                value
            }
            other => panic!("expected the first reward to commit, got {other:?}"),
        };

        let retry_store = ProfileStore::new(root.path());
        let retry_recipient = bind_recipient(&retry_store, &lease, "rIDER", 42);
        let duplicate = apply_race_reward_once(
            &retry_store,
            &lease,
            &retry_recipient,
            &key,
            TimeReward {
                earned_rp: 1,
                earned_lucci: 500,
            },
        )
        .unwrap();
        match duplicate {
            ProfileTransaction::Unchanged { value, profile } => {
                assert_eq!(value, receipt);
                assert_eq!(value.applied.earned_rp, 37);
                assert_eq!(value.applied.earned_lucci, 25);
                assert_eq!(profile.rider.lucci, 1_000_025);
            }
            other => panic!("expected a duplicate no-op, got {other:?}"),
        }

        let loaded = retry_store.load_or_create("Rider").unwrap();
        assert_eq!(loaded.revision, Some(2));
        assert_eq!(loaded.profile.rider.lucci, 1_000_025);
        assert_eq!(loaded.profile.race_reward_receipt, Some(receipt));
    }

    #[test]
    fn historical_receipt_survives_other_economy_changes_and_next_reward_uses_current_balance() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let lease = store.acquire_race_run_lease().unwrap();
        store.load_or_create("Rider").unwrap();
        let recipient = bind_recipient(&store, &lease, "Rider", 42);
        let first_key = reward_key(&recipient, &lease, 7, 11);
        let first = apply_race_reward_once(
            &store,
            &lease,
            &recipient,
            &first_key,
            TimeReward::new(3, 7).unwrap(),
        )
        .unwrap();
        let first_receipt = match first {
            ProfileTransaction::Committed { value, .. } => value,
            other => panic!("expected the first reward to commit, got {other:?}"),
        };

        store
            .update("Rider", |profile| {
                profile.rider.rp = 123;
                profile.rider.lucci += 100;
            })
            .unwrap();
        let retry = apply_race_reward_once(
            &store,
            &lease,
            &recipient,
            &first_key,
            TimeReward::new(50, 500).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            retry,
            ProfileTransaction::Unchanged { value, profile }
                if value == first_receipt
                    && profile.rider.rp == 123
                    && profile.rider.lucci == 1_000_107
        ));

        let second_key = reward_key(&recipient, &lease, 8, 12);
        let second = apply_race_reward_once(
            &store,
            &lease,
            &recipient,
            &second_key,
            TimeReward::new(5, 11).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            second,
            ProfileTransaction::Committed {
                value,
                profile,
                saved,
            } if value.key == second_key
                && value.applied.current_lucci == 1_000_118
                && profile.rider.rp == DEFAULT_RP
                && profile.rider.lucci == 1_000_118
                && saved.revision == 4
        ));
    }

    #[test]
    fn newer_run_applies_but_replaying_an_older_run_cannot_double_credit() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let first_lease = store.acquire_race_run_lease().unwrap();
        store.load_or_create("Rider").unwrap();
        let first_recipient = bind_recipient(&store, &first_lease, "Rider", 42);
        let first_key = reward_key(&first_recipient, &first_lease, 7, 10);

        apply_race_reward_once(
            &store,
            &first_lease,
            &first_recipient,
            &first_key,
            TimeReward {
                earned_rp: 3,
                earned_lucci: 7,
            },
        )
        .unwrap();
        drop(first_lease);
        let second_lease = store.acquire_race_run_lease().unwrap();
        assert!(matches!(
            RaceRewardKey::new(
                &first_recipient,
                &second_lease,
                8,
                GlobalRaceEpoch::new(1).unwrap(),
            ),
            Err(RaceRewardKeyError::RecipientLeaseGenerationMismatch {
                recipient_generation,
                lease_generation,
            }) if recipient_generation.get() == 1 && lease_generation.get() == 2
        ));
        let second_recipient = bind_recipient(&store, &second_lease, "rIDER", 42);
        let second_key = reward_key(&second_recipient, &second_lease, 8, 1);
        let second = apply_race_reward_once(
            &store,
            &second_lease,
            &second_recipient,
            &second_key,
            TimeReward {
                earned_rp: 5,
                earned_lucci: 11,
            },
        )
        .unwrap();

        match second {
            ProfileTransaction::Committed {
                value,
                profile,
                saved,
            } => {
                assert_eq!(saved.revision, 3);
                assert_eq!(value.key, second_key.clone());
                assert_eq!(value.applied.current_lucci, 1_000_018);
                assert_eq!(profile.rider.lucci, 1_000_018);
            }
            other => panic!("expected the new run reward to commit, got {other:?}"),
        }

        let error = apply_race_reward_once(
            &store,
            &second_lease,
            &first_recipient,
            &first_key,
            TimeReward {
                earned_rp: 50,
                earned_lucci: 500,
            },
        )
        .unwrap_err();
        assert!(matches!(
            error,
            RaceRewardPersistenceError::Binding(
                RaceRewardBindingError::RecipientRunGenerationMismatch {
                    recipient_generation: attempted,
                    lease_generation,
                }
            ) if attempted.get() == 1 && lease_generation.get() == 2
        ));
        let loaded = store.reload("Rider").unwrap();
        assert_eq!(loaded.revision, Some(3));
        assert_eq!(loaded.profile.rider.lucci, 1_000_018);
    }

    #[test]
    fn out_of_order_same_run_epoch_is_typed_and_requires_world_serialization() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let lease = store.acquire_race_run_lease().unwrap();
        store.load_or_create("Rider").unwrap();
        let recipient = bind_recipient(&store, &lease, "Rider", 42);
        apply_race_reward_once(
            &store,
            &lease,
            &recipient,
            &reward_key(&recipient, &lease, 7, 11),
            TimeReward {
                earned_rp: 3,
                earned_lucci: 7,
            },
        )
        .unwrap();

        let error = apply_race_reward_once(
            &store,
            &lease,
            &recipient,
            &reward_key(&recipient, &lease, 8, 10),
            TimeReward {
                earned_rp: 50,
                earned_lucci: 500,
            },
        )
        .unwrap_err();
        assert!(matches!(
            error,
            RaceRewardPersistenceError::Order(RaceRewardOrderError::StaleEpoch {
                attempted,
                latest,
            }) if attempted.get() == 10 && latest.get() == 11
        ));

        let loaded = store.load_or_create("Rider").unwrap();
        assert_eq!(loaded.revision, Some(2));
        assert_eq!(loaded.profile.rider.lucci, 1_000_007);
    }

    #[test]
    fn conflicting_same_run_epoch_is_typed_and_does_not_publish() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let lease = store.acquire_race_run_lease().unwrap();
        store.load_or_create("Rider").unwrap();
        let recipient = bind_recipient(&store, &lease, "Rider", 42);
        let stored = reward_key(&recipient, &lease, 7, 10);
        apply_race_reward_once(
            &store,
            &lease,
            &recipient,
            &stored,
            TimeReward {
                earned_rp: 3,
                earned_lucci: 7,
            },
        )
        .unwrap();
        let attempted = reward_key(&recipient, &lease, 8, 10);

        let error = apply_race_reward_once(
            &store,
            &lease,
            &recipient,
            &attempted,
            TimeReward {
                earned_rp: 50,
                earned_lucci: 500,
            },
        )
        .unwrap_err();
        assert!(matches!(
            error,
            RaceRewardPersistenceError::Order(
                RaceRewardOrderError::ConflictingKeyAtEpoch {
                    run_generation,
                    race_epoch,
                    user_no,
                    attempted_room_id,
                    stored_room_id,
                }
            ) if run_generation.get() == 1
                && race_epoch.get() == 10
                && user_no == 42
                && attempted_room_id == attempted.room_id()
                && stored_room_id == stored.room_id()
        ));
        assert_eq!(store.load_or_create("Rider").unwrap().revision, Some(2));
    }

    #[test]
    fn key_cannot_be_applied_to_a_different_canonical_profile_or_user() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let lease = store.acquire_race_run_lease().unwrap();
        store.load_or_create("Rider").unwrap();
        store.load_or_create("Other").unwrap();
        let rider = bind_recipient(&store, &lease, "Rider", 42);
        let key = reward_key(&rider, &lease, 7, 11);
        let other_profile = bind_recipient(&store, &lease, "Other", 42);

        let error = apply_race_reward_once(
            &store,
            &lease,
            &other_profile,
            &key,
            TimeReward {
                earned_rp: 1,
                earned_lucci: 25,
            },
        )
        .unwrap_err();
        assert!(matches!(
            error,
            RaceRewardPersistenceError::Binding(RaceRewardBindingError::RecipientMismatch {
                context: "attempted",
                ..
            })
        ));
        assert_eq!(
            store.load_or_create("Other").unwrap().profile.rider.lucci,
            1_000_000
        );

        let wrong_user = bind_recipient(&store, &lease, "rIDER", 7);
        let error = apply_race_reward_once(
            &store,
            &lease,
            &wrong_user,
            &key,
            TimeReward {
                earned_rp: 1,
                earned_lucci: 25,
            },
        )
        .unwrap_err();
        assert!(matches!(
            error,
            RaceRewardPersistenceError::Binding(RaceRewardBindingError::RecipientMismatch {
                context: "attempted",
                ..
            })
        ));
        assert_eq!(
            store.load_or_create("Rider").unwrap().profile.rider.lucci,
            1_000_000
        );
    }

    #[test]
    fn uncertain_duplicate_retries_remain_uncertain_until_resync_succeeds() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let lease = store.acquire_race_run_lease().unwrap();
        store.load_or_create("Rider").unwrap();
        let recipient = bind_recipient(&store, &lease, "Rider", 42);
        store.fail_next_directory_sync(io::ErrorKind::Other);
        let key = reward_key(&recipient, &lease, 7, 11);

        let first = apply_race_reward_once(
            &store,
            &lease,
            &recipient,
            &key,
            TimeReward {
                earned_rp: 37,
                earned_lucci: 40,
            },
        )
        .unwrap();
        let receipt = match first {
            ProfileTransaction::CommittedButDurabilityUncertain {
                value,
                profile,
                saved,
                error:
                    ProfileStoreError::CommittedButDurabilityUncertain {
                        revision, source, ..
                    },
            } => {
                assert_eq!(saved.revision, 2);
                assert_eq!(revision, 2);
                assert_eq!(source.kind(), io::ErrorKind::Other);
                assert_eq!(profile.rider.lucci, 1_000_040);
                value
            }
            other => panic!("expected a durability warning, got {other:?}"),
        };

        store.fail_next_directory_sync(io::ErrorKind::Other);
        let still_uncertain = apply_race_reward_once(
            &store,
            &lease,
            &recipient,
            &key,
            TimeReward {
                earned_rp: 1,
                earned_lucci: 500,
            },
        )
        .unwrap();
        assert!(matches!(
            still_uncertain,
            ProfileTransaction::CommittedButDurabilityUncertain {
                value,
                profile,
                saved,
                error: ProfileStoreError::CommittedButDurabilityUncertain {
                    revision: 2,
                    ..
                },
            } if value == receipt && profile.rider.lucci == 1_000_040 && saved.revision == 2
        ));

        let retry = apply_race_reward_once(
            &store,
            &lease,
            &recipient,
            &key,
            TimeReward {
                earned_rp: 2,
                earned_lucci: 499,
            },
        )
        .unwrap();
        assert!(matches!(
            retry,
            ProfileTransaction::Unchanged {
                value,
                profile,
            } if value == receipt && profile.rider.lucci == 1_000_040
        ));
        assert_eq!(store.load_or_create("Rider").unwrap().revision, Some(2));
    }

    #[test]
    fn rejected_reward_preserves_a_simultaneous_durability_failure() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let lease = store.acquire_race_run_lease().unwrap();
        store.load_or_create("Rider").unwrap();
        let recipient = bind_recipient(&store, &lease, "Rider", 42);
        apply_race_reward_once(
            &store,
            &lease,
            &recipient,
            &reward_key(&recipient, &lease, 7, 11),
            TimeReward::new(3, 7).unwrap(),
        )
        .unwrap();

        store.fail_next_directory_sync(io::ErrorKind::Other);
        let error = apply_race_reward_once(
            &store,
            &lease,
            &recipient,
            &reward_key(&recipient, &lease, 8, 10),
            TimeReward::new(50, 500).unwrap(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            RaceRewardPersistenceError::RejectedButDurabilityUncertain {
                rejection,
                durability:
                    ProfileStoreError::CommittedButDurabilityUncertain {
                        revision: 2,
                        ..
                    },
            } if matches!(
                *rejection,
                RaceRewardPersistenceError::Order(RaceRewardOrderError::StaleEpoch { .. })
            )
        ));
    }

    #[test]
    fn corrupt_persisted_applied_values_are_never_returned_as_a_retry() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let lease = store.acquire_race_run_lease().unwrap();
        store.load_or_create("Rider").unwrap();
        let recipient = bind_recipient(&store, &lease, "Rider", 42);
        let key = reward_key(&recipient, &lease, 7, 11);
        apply_race_reward_once(
            &store,
            &lease,
            &recipient,
            &key,
            TimeReward::new(3, 7).unwrap(),
        )
        .unwrap();
        store
            .update("Rider", |profile| {
                profile
                    .race_reward_receipt
                    .as_mut()
                    .unwrap()
                    .applied
                    .earned_lucci = 501;
            })
            .unwrap();

        assert!(matches!(
            apply_race_reward_once(
                &store,
                &lease,
                &recipient,
                &key,
                TimeReward::new(1, 1).unwrap(),
            ),
            Err(RaceRewardPersistenceError::InvalidStoredReceipt(
                super::InvalidStoredReceiptError::EarnedLucciOutOfRange { actual: 501, .. }
            ))
        ));
    }

    #[test]
    fn cross_root_key_and_recipient_provenance_cannot_poison_ordering() {
        let first_root = tempdir().unwrap();
        let second_root = tempdir().unwrap();
        let first_store = ProfileStore::new(first_root.path());
        let second_store = ProfileStore::new(second_root.path());
        let first_lease = first_store.acquire_race_run_lease().unwrap();
        drop(second_store.acquire_race_run_lease().unwrap());
        let second_lease = second_store.acquire_race_run_lease().unwrap();
        first_store.load_or_create("Rider").unwrap();
        second_store.load_or_create("Rider").unwrap();
        let first_recipient = bind_recipient(&first_store, &first_lease, "Rider", 42);
        let second_recipient = bind_recipient(&second_store, &second_lease, "Rider", 42);
        let second_key = reward_key(&second_recipient, &second_lease, 7, 1);

        assert_eq!(
            RaceRewardKey::new(
                &first_recipient,
                &second_lease,
                7,
                GlobalRaceEpoch::new(1).unwrap()
            ),
            Err(RaceRewardKeyError::RecipientLeaseStoreMismatch)
        );
        assert!(matches!(
            apply_race_reward_once(
                &first_store,
                &first_lease,
                &first_recipient,
                &second_key,
                TimeReward::new(1, 1).unwrap(),
            ),
            Err(RaceRewardPersistenceError::Binding(
                RaceRewardBindingError::KeyStoreMismatch { .. }
            ))
        ));
        assert_eq!(
            first_store
                .load_or_create("Rider")
                .unwrap()
                .profile
                .rider
                .lucci,
            1_000_000
        );
    }
}
