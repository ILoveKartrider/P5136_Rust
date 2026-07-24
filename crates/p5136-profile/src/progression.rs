//! Exact P5136 reward arithmetic and profile mutation.
//!
//! Random-number generation stays outside this module. A server can plan an
//! exact reward once, then use [`apply_race_reward_once`] to commit it through
//! the versioned [`crate::ProfileStore`] without applying a retry twice.

use std::num::{NonZeroU32, NonZeroU64};

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use thiserror::Error;

use crate::{Profile, ProfileMutation, ProfileStore, ProfileStoreError, ProfileTransaction};

pub const DEFAULT_RP: u32 = 20_000_000;
pub const MAX_TIME_REWARD_RP_ROLL: u8 = 50;
pub const MAX_TIME_REWARD_LUCCI_ROLL: u16 = 500;
pub const TIME_REWARD_BASELINE_RANK: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct TimeReward {
    pub earned_rp: u32,
    pub earned_lucci: u32,
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

/// A process-boot identifier used to separate race epochs across restarts.
///
/// The all-zero value is reserved so an accidentally uninitialized run ID
/// cannot become a valid persistence key.
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

/// A globally unambiguous reward key for one user and one race.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct RaceRewardKey {
    pub run_id: RaceRunId,
    room_id: NonZeroU32,
    pub race_epoch: GlobalRaceEpoch,
    user_no: NonZeroU32,
}

impl RaceRewardKey {
    /// Constructs a key, rejecting protocol sentinel values for room and user.
    pub fn new(
        run_id: RaceRunId,
        room_id: u32,
        race_epoch: GlobalRaceEpoch,
        user_no: u32,
    ) -> Result<Self, RaceRewardKeyError> {
        let room_id = NonZeroU32::new(room_id).ok_or(RaceRewardKeyError::ZeroRoomId)?;
        let user_no = NonZeroU32::new(user_no).ok_or(RaceRewardKeyError::ZeroUserNo)?;
        Ok(Self {
            run_id,
            room_id,
            race_epoch,
            user_no,
        })
    }

    #[must_use]
    pub const fn room_id(self) -> u32 {
        self.room_id.get()
    }

    #[must_use]
    pub const fn user_no(self) -> u32 {
        self.user_no.get()
    }
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum RaceRewardKeyError {
    #[error("race reward room ID must not be zero")]
    ZeroRoomId,

    #[error("race reward user number must not be zero")]
    ZeroUserNo,
}

/// The single bounded receipt retained in a profile.
///
/// `applied` is persisted rather than recomputed so a retry returns the exact
/// values that were used for the original final packets, even if it proposes a
/// different reward.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PersistedRaceRewardReceipt {
    pub key: RaceRewardKey,
    pub applied: AppliedTimeReward,
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum RaceRewardOrderError {
    #[error(
        "race reward epoch {attempted:?} is older than the latest epoch {latest:?} in this run"
    )]
    StaleEpoch {
        attempted: GlobalRaceEpoch,
        latest: GlobalRaceEpoch,
    },

    #[error(
        "race reward key {attempted:?} conflicts with stored key {stored:?} at the same run epoch"
    )]
    ConflictingKeyAtEpoch {
        attempted: RaceRewardKey,
        stored: RaceRewardKey,
    },

    #[error(
        "race reward user changed within one server run: attempted key {attempted:?}, stored key {stored:?}"
    )]
    UserChangedWithinRun {
        attempted: RaceRewardKey,
        stored: RaceRewardKey,
    },
}

#[derive(Debug, Error)]
pub enum RaceRewardPersistenceError {
    #[error(transparent)]
    Store(#[from] ProfileStoreError),

    #[error(transparent)]
    Order(#[from] RaceRewardOrderError),
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
    let rp_bonus = u32::try_from(bonus / 3).expect("the bounded rank bonus fits in u32");
    let lucci_bonus =
        u32::try_from(bonus * 3).expect("the bounded rank bonus multiplication fits in u32");
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
/// revision, regardless of the newly proposed reward. Within the same run,
/// older epochs and conflicting equal epochs are rejected without mutation.
/// A different run ID may start its epoch counter again and apply normally.
pub fn apply_race_reward_once(
    store: &ProfileStore,
    nickname: &str,
    key: RaceRewardKey,
    proposed_reward: TimeReward,
) -> Result<ProfileTransaction<PersistedRaceRewardReceipt>, RaceRewardPersistenceError> {
    let transaction = store.transaction(nickname, |profile| {
        if let Some(stored) = profile.race_reward_receipt {
            if stored.key == key {
                return ProfileMutation::Unchanged(Ok(stored));
            }
            if stored.key.run_id == key.run_id {
                if stored.key.user_no != key.user_no {
                    return ProfileMutation::Unchanged(Err(
                        RaceRewardOrderError::UserChangedWithinRun {
                            attempted: key,
                            stored: stored.key,
                        },
                    ));
                }
                if key.race_epoch < stored.key.race_epoch {
                    return ProfileMutation::Unchanged(Err(RaceRewardOrderError::StaleEpoch {
                        attempted: key.race_epoch,
                        latest: stored.key.race_epoch,
                    }));
                }
                if key.race_epoch == stored.key.race_epoch {
                    return ProfileMutation::Unchanged(Err(
                        RaceRewardOrderError::ConflictingKeyAtEpoch {
                            attempted: key,
                            stored: stored.key,
                        },
                    ));
                }
            }
        }

        let mut next = profile.clone();
        let receipt = PersistedRaceRewardReceipt {
            key,
            applied: apply_time_reward(&mut next, proposed_reward),
        };
        next.race_reward_receipt = Some(receipt);
        ProfileMutation::changed(Ok(receipt), next)
    })?;

    resolve_reward_transaction(transaction)
}

fn resolve_reward_transaction(
    transaction: ProfileTransaction<Result<PersistedRaceRewardReceipt, RaceRewardOrderError>>,
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
        } => Ok(ProfileTransaction::CommittedButDurabilityUncertain {
            value: value?,
            profile,
            saved,
            error,
        }),
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
        DEFAULT_RP, GlobalRaceEpoch, RaceRewardKey, RaceRewardKeyError, RaceRewardOrderError,
        RaceRewardPersistenceError, RaceRunId, RewardRollError, TimeReward, apply_race_reward_once,
        apply_time_reward, finish_reward, time_reward_from_rolls,
    };
    use crate::{Profile, ProfileStore, ProfileStoreError, ProfileTransaction};

    fn run_id(marker: u8) -> RaceRunId {
        let mut bytes = [0; 16];
        bytes[15] = marker;
        RaceRunId::new(bytes).unwrap()
    }

    fn reward_key(run_marker: u8, room_id: u32, epoch: u64, user_no: u32) -> RaceRewardKey {
        RaceRewardKey::new(
            run_id(run_marker),
            room_id,
            GlobalRaceEpoch::new(epoch).unwrap(),
            user_no,
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
    fn persistence_key_rejects_reserved_zero_values() {
        assert_eq!(RaceRunId::new([0; 16]), None);
        assert_eq!(GlobalRaceEpoch::new(0), None);
        assert!(serde_json::from_str::<RaceRunId>("[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]").is_err());
        assert!(serde_json::from_str::<GlobalRaceEpoch>("0").is_err());

        let run_id = run_id(1);
        let epoch = GlobalRaceEpoch::new(1).unwrap();
        assert_eq!(
            RaceRewardKey::new(run_id, 0, epoch, 1),
            Err(RaceRewardKeyError::ZeroRoomId)
        );
        assert_eq!(
            RaceRewardKey::new(run_id, 1, epoch, 0),
            Err(RaceRewardKeyError::ZeroUserNo)
        );
    }

    #[test]
    fn duplicate_key_returns_the_stored_outcome_without_mutating_economy_or_revision() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        store.load_or_create("Rider").unwrap();
        let key = reward_key(1, 7, 11, 42);

        let first = apply_race_reward_once(
            &store,
            "Rider",
            key,
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
                assert_eq!(profile.race_reward_receipt, Some(value));
                value
            }
            other => panic!("expected the first reward to commit, got {other:?}"),
        };

        let retry_store = ProfileStore::new(root.path());
        let duplicate = apply_race_reward_once(
            &retry_store,
            "rIDER",
            key,
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
    fn a_different_boot_run_can_restart_the_global_epoch_and_apply() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        store.load_or_create("Rider").unwrap();

        apply_race_reward_once(
            &store,
            "Rider",
            reward_key(1, 7, 10, 42),
            TimeReward {
                earned_rp: 3,
                earned_lucci: 7,
            },
        )
        .unwrap();
        let second = apply_race_reward_once(
            &store,
            "Rider",
            reward_key(2, 8, 1, 1),
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
                assert_eq!(value.key, reward_key(2, 8, 1, 1));
                assert_eq!(value.applied.current_lucci, 1_000_018);
                assert_eq!(profile.rider.lucci, 1_000_018);
            }
            other => panic!("expected the new run reward to commit, got {other:?}"),
        }
    }

    #[test]
    fn stale_same_run_epoch_is_typed_and_does_not_publish() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        store.load_or_create("Rider").unwrap();
        apply_race_reward_once(
            &store,
            "Rider",
            reward_key(1, 7, 10, 42),
            TimeReward {
                earned_rp: 3,
                earned_lucci: 7,
            },
        )
        .unwrap();

        let error = apply_race_reward_once(
            &store,
            "Rider",
            reward_key(1, 8, 9, 42),
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
            }) if attempted.get() == 9 && latest.get() == 10
        ));

        let loaded = store.load_or_create("Rider").unwrap();
        assert_eq!(loaded.revision, Some(2));
        assert_eq!(loaded.profile.rider.lucci, 1_000_007);
    }

    #[test]
    fn conflicting_same_run_epoch_is_typed_and_does_not_publish() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        store.load_or_create("Rider").unwrap();
        let stored = reward_key(1, 7, 10, 42);
        apply_race_reward_once(
            &store,
            "Rider",
            stored,
            TimeReward {
                earned_rp: 3,
                earned_lucci: 7,
            },
        )
        .unwrap();
        let attempted = reward_key(1, 8, 10, 42);

        let error = apply_race_reward_once(
            &store,
            "Rider",
            attempted,
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
                    attempted: actual_attempted,
                    stored: actual_stored,
                }
            ) if actual_attempted == attempted && actual_stored == stored
        ));
        assert_eq!(store.load_or_create("Rider").unwrap().revision, Some(2));
    }

    #[test]
    fn post_publish_sync_fault_preserves_receipt_and_makes_retry_a_noop() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        store.load_or_create("Rider").unwrap();
        store.fail_next_directory_sync(io::ErrorKind::Other);
        let key = reward_key(1, 7, 11, 42);

        let first = apply_race_reward_once(
            &store,
            "Rider",
            key,
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

        let retry = apply_race_reward_once(
            &store,
            "Rider",
            key,
            TimeReward {
                earned_rp: 1,
                earned_lucci: 500,
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
}
