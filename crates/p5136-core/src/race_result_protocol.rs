//! Exact P5136 race-settlement packet encoding.
//!
//! The Korean P5136 client expects a fixed 217-byte record for each human
//! racer and a fixed 22-byte record for each AI racer. Later clients added a
//! trailing dword, so this module deliberately emits only the five-byte
//! P5136 tail.

use std::collections::HashSet;

use thiserror::Error;

use crate::packet::PacketWriter;

pub const GAME_RESULT_PACKET_NAME: &str = "GameResultPacket";
pub const HUMAN_RESULT_RECORD_LENGTH: usize = 217;
pub const AI_RESULT_RECORD_LENGTH: usize = 22;
pub const EMPTY_GAME_RESULT_LENGTH: usize = 52;
pub const MAX_RACE_RESULT_PARTICIPANTS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ResultTeam {
    Red = 1,
    Blue = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HumanRaceResult {
    pub player_id: i32,
    pub finish_time: u32,
    pub kart_id: u16,
    pub rank: i32,
    pub current_rp: u32,
    pub earned_rp: u32,
    pub earned_lucci: u32,
    pub current_lucci: u32,
    pub team: Option<ResultTeam>,
    pub team_points: i32,
    pub character_id: u16,
    pub club_mark_logo: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AiRaceResult {
    pub player_id: i32,
    pub finish_time: u32,
    pub kart_id: i16,
    pub rank: i32,
    pub team: Option<ResultTeam>,
    pub team_points: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameResult<'a> {
    /// `None` is used by individual modes. Team modes use the winning team.
    pub winning_team: Option<ResultTeam>,
    pub humans: &'a [HumanRaceResult],
    pub ais: &'a [AiRaceResult],
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RaceResultProtocolError {
    #[error("race result has {actual} participants; P5136 rooms support at most {maximum}")]
    TooManyParticipants { actual: usize, maximum: usize },

    #[error("race player ID {0} is outside the eight racer slots 0..=7")]
    InvalidPlayerId(i32),

    #[error("race rank {0} is outside 0..=7")]
    InvalidRank(i32),

    #[error("race rank {rank} is outside the participant count {participant_count}")]
    RankOutsideParticipantCount { rank: i32, participant_count: usize },

    #[error("race result contains duplicate rank {0}")]
    DuplicateRank(i32),

    #[error("race result contains duplicate player ID {0}")]
    DuplicatePlayerId(i32),

    #[error("race result mixes individual and team-mode fields")]
    InconsistentTeamMode,

    #[error("team result for player ID {player_id} has invalid points {points}; expected 0..=10")]
    InvalidTeamPoints { player_id: i32, points: i32 },

    #[error("winning team has no participant in the race result")]
    WinningTeamNotPresent,

    #[error("individual result for player ID {player_id} has non-zero team points {points}")]
    SoloResultHasTeamPoints { player_id: i32, points: i32 },
}

pub fn serialize_game_result(result: &GameResult<'_>) -> Result<Vec<u8>, RaceResultProtocolError> {
    validate_result(result)?;

    let mut packet = PacketWriter::named(GAME_RESULT_PACKET_NAME);
    packet.write_u8(result.winning_team.map_or(0, |team| team as u8));
    packet.write_i32(
        i32::try_from(result.humans.len())
            .expect("the validated P5136 participant count always fits in i32"),
    );
    for human in result.humans {
        write_human_result(&mut packet, *human);
    }

    packet.write_i32(
        i32::try_from(result.ais.len())
            .expect("the validated P5136 participant count always fits in i32"),
    );
    for ai in result.ais {
        write_ai_result(&mut packet, *ai);
    }

    // Unknown result-stage state retained exactly from the Korean P5136
    // server. The final dword present in modern clients must not be appended.
    packet.write_bytes(&[0; 34]);
    packet.write_u32(u32::MAX);
    packet.write_u8(0);

    let expected_length = EMPTY_GAME_RESULT_LENGTH
        + result.humans.len() * HUMAN_RESULT_RECORD_LENGTH
        + result.ais.len() * AI_RESULT_RECORD_LENGTH;
    debug_assert_eq!(packet.as_slice().len(), expected_length);
    Ok(packet.into_inner())
}

fn validate_result(result: &GameResult<'_>) -> Result<(), RaceResultProtocolError> {
    let participant_count = result.humans.len().saturating_add(result.ais.len());
    if participant_count > MAX_RACE_RESULT_PARTICIPANTS {
        return Err(RaceResultProtocolError::TooManyParticipants {
            actual: participant_count,
            maximum: MAX_RACE_RESULT_PARTICIPANTS,
        });
    }

    let mut player_ids = HashSet::with_capacity(participant_count);
    let mut ranks = HashSet::with_capacity(participant_count);
    let team_mode = result.winning_team.is_some();
    let mut winning_team_present = false;
    for (player_id, rank, team, team_points) in result
        .humans
        .iter()
        .map(|entry| (entry.player_id, entry.rank, entry.team, entry.team_points))
        .chain(
            result
                .ais
                .iter()
                .map(|entry| (entry.player_id, entry.rank, entry.team, entry.team_points)),
        )
    {
        if !(0..=7).contains(&player_id) {
            return Err(RaceResultProtocolError::InvalidPlayerId(player_id));
        }
        if !(0..=7).contains(&rank) {
            return Err(RaceResultProtocolError::InvalidRank(rank));
        }
        if usize::try_from(rank).map_or(true, |rank| rank >= participant_count) {
            return Err(RaceResultProtocolError::RankOutsideParticipantCount {
                rank,
                participant_count,
            });
        }
        if !ranks.insert(rank) {
            return Err(RaceResultProtocolError::DuplicateRank(rank));
        }
        if !player_ids.insert(player_id) {
            return Err(RaceResultProtocolError::DuplicatePlayerId(player_id));
        }
        if team.is_some() != team_mode {
            return Err(RaceResultProtocolError::InconsistentTeamMode);
        }
        if !team_mode && team_points != 0 {
            return Err(RaceResultProtocolError::SoloResultHasTeamPoints {
                player_id,
                points: team_points,
            });
        }
        if team_mode && !(0..=10).contains(&team_points) {
            return Err(RaceResultProtocolError::InvalidTeamPoints {
                player_id,
                points: team_points,
            });
        }
        if team == result.winning_team {
            winning_team_present = true;
        }
    }
    if team_mode && !winning_team_present {
        return Err(RaceResultProtocolError::WinningTeamNotPresent);
    }
    Ok(())
}

fn write_human_result(packet: &mut PacketWriter, result: HumanRaceResult) {
    let start = packet.as_slice().len();
    packet.write_i32(result.player_id);
    packet.write_u32(result.finish_time);
    packet.write_u8(0);
    packet.write_u16(result.kart_id);
    packet.write_i32(result.rank);
    packet.write_i16(if result.team.is_some() { 2 } else { 0 });
    packet.write_u8(0);
    packet.write_u32(result.current_rp);
    packet.write_u32(result.earned_rp);
    packet.write_u32(result.earned_lucci);
    packet.write_u32(result.current_lucci);
    packet.write_bytes(&[0; 29]);
    packet.write_i32(if result.team.is_some() {
        result.team_points
    } else {
        0
    });
    packet.write_u8(result.team.map_or(0, |team| team as u8));
    packet.write_bytes(&[0; 12]);
    packet.write_i32(1);
    packet.write_u8(0);
    packet.write_u16(result.character_id);
    packet.write_bytes(&[0; 49]);
    packet.write_u8(u8::MAX);
    packet.write_bytes(&[0; 37]);
    packet.write_i32(result.club_mark_logo);
    packet.write_bytes(&[0; 39]);
    debug_assert_eq!(packet.as_slice().len() - start, HUMAN_RESULT_RECORD_LENGTH);
}

fn write_ai_result(packet: &mut PacketWriter, result: AiRaceResult) {
    let start = packet.as_slice().len();
    packet.write_i32(result.player_id);
    packet.write_u32(result.finish_time);
    packet.write_u8(0);
    packet.write_i16(result.kart_id);
    packet.write_i32(result.rank);
    packet.write_i16(0);
    packet.write_u8(result.team.map_or(0, |team| team as u8));
    packet.write_i32(if result.team.is_some() {
        result.team_points
    } else {
        0
    });
    debug_assert_eq!(packet.as_slice().len() - start, AI_RESULT_RECORD_LENGTH);
}

#[cfg(test)]
mod tests {
    use super::{
        AI_RESULT_RECORD_LENGTH, AiRaceResult, EMPTY_GAME_RESULT_LENGTH, GAME_RESULT_PACKET_NAME,
        GameResult, HUMAN_RESULT_RECORD_LENGTH, HumanRaceResult, MAX_RACE_RESULT_PARTICIPANTS,
        RaceResultProtocolError, ResultTeam, serialize_game_result,
    };
    use crate::adler32;

    #[test]
    fn empty_result_matches_the_exact_p5136_fixture_without_modern_tail() {
        let packet = serialize_game_result(&GameResult {
            winning_team: None,
            humans: &[],
            ais: &[],
        })
        .unwrap();

        assert_eq!(
            packet,
            decode_hex(concat!(
                "51065C34",
                "00",
                "00000000",
                "00000000",
                "00000000000000000000000000000000",
                "00000000000000000000000000000000",
                "0000",
                "FFFFFFFF",
                "00"
            ))
        );
        assert_eq!(packet.len(), EMPTY_GAME_RESULT_LENGTH);
        assert_eq!(
            u32::from_le_bytes(packet[..4].try_into().unwrap()),
            adler32::packet_hash(GAME_RESULT_PACKET_NAME)
        );
    }

    #[test]
    fn human_and_ai_records_use_the_csharp_offsets_and_fixed_lengths() {
        let human = HumanRaceResult {
            player_id: 2,
            finish_time: 0x1234_5678,
            kart_id: 1_401,
            rank: 0,
            current_rp: 900,
            earned_rp: 12,
            earned_lucci: 34,
            current_lucci: 5_678,
            team: Some(ResultTeam::Red),
            team_points: 10,
            character_id: 42,
            club_mark_logo: -7,
        };
        let ai = AiRaceResult {
            player_id: 5,
            finish_time: u32::MAX,
            kart_id: 1_410,
            rank: 1,
            team: Some(ResultTeam::Blue),
            team_points: 0,
        };
        let packet = serialize_game_result(&GameResult {
            winning_team: Some(ResultTeam::Red),
            humans: &[human],
            ais: &[ai],
        })
        .unwrap();

        assert_eq!(
            packet.len(),
            EMPTY_GAME_RESULT_LENGTH + HUMAN_RESULT_RECORD_LENGTH + AI_RESULT_RECORD_LENGTH
        );
        assert_eq!(packet[4], ResultTeam::Red as u8);
        assert_eq!(&packet[5..9], &1_i32.to_le_bytes());

        let human_start = 9;
        assert_eq!(
            &packet[human_start..human_start + 4],
            &human.player_id.to_le_bytes()
        );
        assert_eq!(
            &packet[human_start + 4..human_start + 8],
            &human.finish_time.to_le_bytes()
        );
        assert_eq!(
            &packet[human_start + 9..human_start + 11],
            &human.kart_id.to_le_bytes()
        );
        assert_eq!(
            &packet[human_start + 15..human_start + 17],
            &2_i16.to_le_bytes()
        );
        assert_eq!(
            &packet[human_start + 63..human_start + 67],
            &human.team_points.to_le_bytes()
        );
        assert_eq!(packet[human_start + 67], ResultTeam::Red as u8);
        assert_eq!(
            &packet[human_start + 80..human_start + 84],
            &1_i32.to_le_bytes()
        );
        assert_eq!(
            &packet[human_start + 85..human_start + 87],
            &human.character_id.to_le_bytes()
        );
        assert_eq!(packet[human_start + 136], u8::MAX);
        assert_eq!(
            &packet[human_start + 174..human_start + 178],
            &human.club_mark_logo.to_le_bytes()
        );

        let ai_count = human_start + HUMAN_RESULT_RECORD_LENGTH;
        assert_eq!(&packet[ai_count..ai_count + 4], &1_i32.to_le_bytes());
        let ai_start = ai_count + 4;
        assert_eq!(&packet[ai_start..ai_start + 4], &ai.player_id.to_le_bytes());
        assert_eq!(
            &packet[ai_start + 9..ai_start + 11],
            &ai.kart_id.to_le_bytes()
        );
        assert_eq!(packet[ai_start + 17], ResultTeam::Blue as u8);
        assert_eq!(
            &packet[ai_start + 18..ai_start + 22],
            &ai.team_points.to_le_bytes()
        );
        assert_eq!(&packet[packet.len() - 5..], &[0xff, 0xff, 0xff, 0xff, 0]);
    }

    #[test]
    fn solo_records_zero_the_team_only_wire_fields() {
        let human = HumanRaceResult {
            player_id: 0,
            finish_time: 100,
            kart_id: 1,
            rank: 0,
            current_rp: 1,
            earned_rp: 2,
            earned_lucci: 3,
            current_lucci: 4,
            team: None,
            team_points: 0,
            character_id: 5,
            club_mark_logo: 6,
        };
        let packet = serialize_game_result(&GameResult {
            winning_team: None,
            humans: &[human],
            ais: &[],
        })
        .unwrap();
        let start = 9;
        assert_eq!(&packet[start + 15..start + 17], &[0, 0]);
        assert_eq!(&packet[start + 63..start + 67], &[0, 0, 0, 0]);
        assert_eq!(packet[start + 67], 0);
    }

    #[test]
    fn invalid_counts_ids_ranks_and_team_fields_are_rejected() {
        let base = HumanRaceResult {
            player_id: 0,
            finish_time: 0,
            kart_id: 0,
            rank: 0,
            current_rp: 0,
            earned_rp: 0,
            earned_lucci: 0,
            current_lucci: 0,
            team: None,
            team_points: 0,
            character_id: 0,
            club_mark_logo: 0,
        };

        let too_many = vec![base; MAX_RACE_RESULT_PARTICIPANTS + 1];
        assert_eq!(
            serialize_game_result(&GameResult {
                winning_team: None,
                humans: &too_many,
                ais: &[],
            }),
            Err(RaceResultProtocolError::TooManyParticipants {
                actual: MAX_RACE_RESULT_PARTICIPANTS + 1,
                maximum: MAX_RACE_RESULT_PARTICIPANTS,
            })
        );

        for (entry, expected) in [
            (
                HumanRaceResult {
                    player_id: 16,
                    ..base
                },
                RaceResultProtocolError::InvalidPlayerId(16),
            ),
            (
                HumanRaceResult {
                    player_id: 8,
                    ..base
                },
                RaceResultProtocolError::InvalidPlayerId(8),
            ),
            (
                HumanRaceResult { rank: 8, ..base },
                RaceResultProtocolError::InvalidRank(8),
            ),
            (
                HumanRaceResult {
                    team_points: 1,
                    ..base
                },
                RaceResultProtocolError::SoloResultHasTeamPoints {
                    player_id: 0,
                    points: 1,
                },
            ),
        ] {
            assert_eq!(
                serialize_game_result(&GameResult {
                    winning_team: None,
                    humans: &[entry],
                    ais: &[],
                }),
                Err(expected)
            );
        }

        assert_eq!(
            serialize_game_result(&GameResult {
                winning_team: None,
                humans: &[base],
                ais: &[AiRaceResult {
                    player_id: 0,
                    finish_time: 0,
                    kart_id: 0,
                    rank: 1,
                    team: None,
                    team_points: 0,
                }],
            }),
            Err(RaceResultProtocolError::DuplicatePlayerId(0))
        );
    }

    #[test]
    fn ranking_and_team_mode_invariants_reject_impossible_settlements() {
        let solo = HumanRaceResult {
            player_id: 0,
            finish_time: 100,
            kart_id: 1,
            rank: 0,
            current_rp: 0,
            earned_rp: 0,
            earned_lucci: 0,
            current_lucci: 0,
            team: None,
            team_points: 0,
            character_id: 1,
            club_mark_logo: 0,
        };
        let second = HumanRaceResult {
            player_id: 1,
            rank: 0,
            ..solo
        };
        assert_eq!(
            serialize_game_result(&GameResult {
                winning_team: None,
                humans: &[solo, second],
                ais: &[],
            }),
            Err(RaceResultProtocolError::DuplicateRank(0))
        );
        assert_eq!(
            serialize_game_result(&GameResult {
                winning_team: None,
                humans: &[HumanRaceResult { rank: 1, ..solo }],
                ais: &[],
            }),
            Err(RaceResultProtocolError::RankOutsideParticipantCount {
                rank: 1,
                participant_count: 1,
            })
        );
        assert_eq!(
            serialize_game_result(&GameResult {
                winning_team: Some(ResultTeam::Red),
                humans: &[solo],
                ais: &[],
            }),
            Err(RaceResultProtocolError::InconsistentTeamMode)
        );
        assert_eq!(
            serialize_game_result(&GameResult {
                winning_team: Some(ResultTeam::Red),
                humans: &[HumanRaceResult {
                    team: Some(ResultTeam::Blue),
                    ..solo
                }],
                ais: &[],
            }),
            Err(RaceResultProtocolError::WinningTeamNotPresent)
        );
        assert_eq!(
            serialize_game_result(&GameResult {
                winning_team: Some(ResultTeam::Red),
                humans: &[HumanRaceResult {
                    team: Some(ResultTeam::Red),
                    team_points: 11,
                    ..solo
                }],
                ais: &[],
            }),
            Err(RaceResultProtocolError::InvalidTeamPoints {
                player_id: 0,
                points: 11,
            })
        );
    }

    fn decode_hex(input: &str) -> Vec<u8> {
        input
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }
}
