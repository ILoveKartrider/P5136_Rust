//! Exact P5136 reward arithmetic and profile mutation.
//!
//! Random-number generation and persistence are intentionally kept outside
//! this module. A server can inject reproducible rolls in tests, then commit
//! the returned mutation through the versioned [`crate::ProfileStore`].

use thiserror::Error;

use crate::Profile;

pub const DEFAULT_RP: u32 = 20_000_000;
pub const MAX_TIME_REWARD_RP_ROLL: u8 = 50;
pub const MAX_TIME_REWARD_LUCCI_ROLL: u16 = 500;
pub const TIME_REWARD_BASELINE_RANK: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeReward {
    pub earned_rp: u32,
    pub earned_lucci: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppliedTimeReward {
    pub current_rp: u32,
    pub earned_rp: u32,
    pub earned_lucci: u32,
    pub current_lucci: u32,
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
    use super::{
        DEFAULT_RP, RewardRollError, TimeReward, apply_time_reward, finish_reward,
        time_reward_from_rolls,
    };
    use crate::Profile;

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
}
