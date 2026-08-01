//! Core P5136 race-control codecs.
//!
//! Kart movement stays on the opaque UDP relay path. These packets drive the
//! TCP-side start clock, finish reporting, team booster gauge, and settlement
//! stage transition.

use thiserror::Error;

use crate::{
    adler32,
    game_slot_protocol::GAME_SLOT_PACKET_NAME,
    packet::{PacketError, PacketReader, PacketWriter},
};

pub const GAME_CONTROL_PACKET_NAME: &str = "GameControlPacket";
pub const GAME_AI_GOAL_IN_PACKET_NAME: &str = "GameAiGoalinPacket";
pub const GAME_RACE_TIME_PACKET_NAME: &str = "GameRaceTimePacket";
pub const TEAM_BOOSTER_REQUEST_NAME: &str = "GameTeamBoosterRequestAddGaugePacket";
pub const TEAM_BOOSTER_REPLY_NAME: &str = "GameTeamBoosterSetGaugePacket";
pub const GAME_AI_MASTER_NOTICE_NAME: &str = "GameAiMasterSlotNoticePacket";
pub const GAME_NEXT_STAGE_PACKET_NAME: &str = "GameNextStagePacket";

/// The retained P5136 finish producer sends a 406-byte `GameControlPacket`:
/// 13 common bytes followed by a 393-byte state-2 result snapshot.  The
/// request parser retains a small future-compatible margin for other control
/// states without cloning the C# handler's unbounded ignored tail.
pub const MAX_GAME_CONTROL_TRAILING_BYTES: usize = 512;
pub const CANONICAL_GAME_CONTROL_BODY_LENGTH: usize = 81;
pub const GAME_CONTROL_FINISH_TRAILING_LENGTH: usize = 393;

const GAME_CONTROL_FINISH_SESSION_AUTH_WORDS: usize = 7;
const GAME_CONTROL_FINISH_RESULT_SUBOBJECT_LENGTH: usize = 54;
const GAME_CONTROL_FINISH_KART_PHYSICS_LENGTH: usize = 243;
const GAME_CONTROL_FINISH_SHARED_TIMESTAMP_LENGTH: usize = 18;
const GAME_CONTROL_FINISH_PARTICIPANT_SLOT_COUNT: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RaceRequest {
    GameControl,
    AiGoalIn,
    TeamBoosterGauge,
    GameSlot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameControlRequest {
    pub state: i32,
    pub optional_pair: Option<(i32, i32)>,
    pub value0: u32,
    /// P5136 sends additional versioned state in some control packets. The
    /// runtime does not interpret it, but the bounded copy keeps parsing
    /// forward-compatible.
    pub trailing: Vec<u8>,
}

/// The fixed state-2 portion of a captured P5136 `GameControlPacket`.
///
/// This is diagnostic input only. Race settlement remains authoritative on
/// the server, and no field in this snapshot is used to alter the result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameControlFinishSnapshot {
    pub value1: u32,
    pub value2: u32,
    pub session_auth: [u32; GAME_CONTROL_FINISH_SESSION_AUTH_WORDS],
    pub result_subobject: [u8; GAME_CONTROL_FINISH_RESULT_SUBOBJECT_LENGTH],
    pub result_global_metric: u32,
    pub kart_physics_snapshot: [u8; GAME_CONTROL_FINISH_KART_PHYSICS_LENGTH],
    pub shared_timestamp: [u8; GAME_CONTROL_FINISH_SHARED_TIMESTAMP_LENGTH],
    pub participant_slots: [u32; GAME_CONTROL_FINISH_PARTICIPANT_SLOT_COUNT],
    pub local_kart_or_player_result: u32,
    pub terminal_client_state: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AiGoalInRequest {
    pub player_id: i32,
    pub race_time: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TeamBoosterGaugeRequest {
    pub team: RaceTeam,
    pub contribution: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RaceTeam {
    Red = 1,
    Blue = 2,
}

impl RaceTeam {
    fn from_wire(value: u8) -> Result<Self, RaceProtocolError> {
        match value {
            1 => Ok(Self::Red),
            2 => Ok(Self::Blue),
            _ => Err(RaceProtocolError::InvalidTeam(value)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerGameControl {
    RaceStart = 1,
    BeginSettlement = 3,
    FinalStage = 4,
}

#[derive(Debug, Error)]
pub enum RaceProtocolError {
    #[error(transparent)]
    Packet(#[from] PacketError),

    #[error("expected {name} hash 0x{expected:08X}, received 0x{actual:08X}")]
    UnexpectedPacketHash {
        name: &'static str,
        expected: u32,
        actual: u32,
    },

    #[error("packet {name} has {count} unexpected trailing bytes")]
    TrailingBytes { name: &'static str, count: usize },

    #[error("GameControl trailing state has {actual} bytes; configured maximum is {maximum}")]
    GameControlTailTooLarge { actual: usize, maximum: usize },

    #[error("race player ID {0} is outside 0..=15")]
    InvalidPlayerId(i32),

    #[error("race team {0} is not red (1) or blue (2)")]
    InvalidTeam(u8),

    #[error("team-booster contribution must be finite and non-negative")]
    InvalidBoosterContribution,

    #[error("team-booster gauge must be finite and inside 0..=1")]
    InvalidBoosterGauge,
}

#[must_use]
pub fn classify_race_request(hash: u32) -> Option<RaceRequest> {
    [
        (GAME_CONTROL_PACKET_NAME, RaceRequest::GameControl),
        (GAME_AI_GOAL_IN_PACKET_NAME, RaceRequest::AiGoalIn),
        (TEAM_BOOSTER_REQUEST_NAME, RaceRequest::TeamBoosterGauge),
        (GAME_SLOT_PACKET_NAME, RaceRequest::GameSlot),
    ]
    .into_iter()
    .find_map(|(name, request)| (adler32::packet_hash(name) == hash).then_some(request))
}

pub fn parse_game_control_request(packet: &[u8]) -> Result<GameControlRequest, RaceProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, GAME_CONTROL_PACKET_NAME)?;
    let state = reader.read_i32()?;
    let optional_pair = if reader.read_u8()? == 0 {
        None
    } else {
        Some((reader.read_i32()?, reader.read_i32()?))
    };
    let value0 = reader.read_u32()?;
    let trailing = reader.remaining();
    if trailing.len() > MAX_GAME_CONTROL_TRAILING_BYTES {
        return Err(RaceProtocolError::GameControlTailTooLarge {
            actual: trailing.len(),
            maximum: MAX_GAME_CONTROL_TRAILING_BYTES,
        });
    }
    Ok(GameControlRequest {
        state,
        optional_pair,
        value0,
        trailing: trailing.to_vec(),
    })
}

/// Decodes the observed fixed state-2 finish snapshot when the packet has the
/// captured 406-byte shape. Other control-state extensions deliberately
/// remain retained-but-uninterpreted for forward compatibility.
pub fn parse_game_control_finish_snapshot(
    request: &GameControlRequest,
) -> Result<Option<GameControlFinishSnapshot>, RaceProtocolError> {
    if request.state != 2
        || request.optional_pair.is_some()
        || request.trailing.len() != GAME_CONTROL_FINISH_TRAILING_LENGTH
    {
        return Ok(None);
    }

    let mut reader = PacketReader::new(&request.trailing);
    let value1 = reader.read_u32()?;
    let value2 = reader.read_u32()?;
    if reader.read_u8()? == 0 {
        // The captured 393-byte form necessarily contains seven session-auth
        // words. Treat a different producer form as an unknown extension
        // instead of consuming it with an invented layout.
        return Ok(None);
    }

    let mut session_auth = [0; GAME_CONTROL_FINISH_SESSION_AUTH_WORDS];
    for word in &mut session_auth {
        *word = reader.read_u32()?;
    }
    let mut result_subobject = [0; GAME_CONTROL_FINISH_RESULT_SUBOBJECT_LENGTH];
    result_subobject
        .copy_from_slice(reader.read_bytes(GAME_CONTROL_FINISH_RESULT_SUBOBJECT_LENGTH)?);
    let result_global_metric = reader.read_u32()?;
    let mut kart_physics_snapshot = [0; GAME_CONTROL_FINISH_KART_PHYSICS_LENGTH];
    kart_physics_snapshot
        .copy_from_slice(reader.read_bytes(GAME_CONTROL_FINISH_KART_PHYSICS_LENGTH)?);
    let mut shared_timestamp = [0; GAME_CONTROL_FINISH_SHARED_TIMESTAMP_LENGTH];
    shared_timestamp
        .copy_from_slice(reader.read_bytes(GAME_CONTROL_FINISH_SHARED_TIMESTAMP_LENGTH)?);
    let mut participant_slots = [0; GAME_CONTROL_FINISH_PARTICIPANT_SLOT_COUNT];
    for slot in &mut participant_slots {
        *slot = reader.read_u32()?;
    }
    let local_kart_or_player_result = reader.read_u32()?;
    let terminal_client_state = reader.read_u8()?;
    ensure_exhausted(&reader, GAME_CONTROL_PACKET_NAME)?;

    Ok(Some(GameControlFinishSnapshot {
        value1,
        value2,
        session_auth,
        result_subobject,
        result_global_metric,
        kart_physics_snapshot,
        shared_timestamp,
        participant_slots,
        local_kart_or_player_result,
        terminal_client_state,
    }))
}

pub fn parse_ai_goal_in_request(packet: &[u8]) -> Result<AiGoalInRequest, RaceProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, GAME_AI_GOAL_IN_PACKET_NAME)?;
    let request = AiGoalInRequest {
        player_id: reader.read_i32()?,
        race_time: reader.read_u32()?,
    };
    validate_player_id(request.player_id)?;
    ensure_exhausted(&reader, GAME_AI_GOAL_IN_PACKET_NAME)?;
    Ok(request)
}

pub fn parse_team_booster_request(
    packet: &[u8],
) -> Result<TeamBoosterGaugeRequest, RaceProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, TEAM_BOOSTER_REQUEST_NAME)?;
    let request = TeamBoosterGaugeRequest {
        team: RaceTeam::from_wire(reader.read_u8()?)?,
        contribution: reader.read_f32()?,
    };
    ensure_exhausted(&reader, TEAM_BOOSTER_REQUEST_NAME)?;
    if !request.contribution.is_finite() || request.contribution < 0.0 {
        return Err(RaceProtocolError::InvalidBoosterContribution);
    }
    Ok(request)
}

/// Serializes the canonical 81-byte non-result body used by the active P5136
/// server for control types 1, 3, and 4.
#[must_use]
pub fn serialize_game_control(control: ServerGameControl, value0: u32) -> Vec<u8> {
    let mut packet = PacketWriter::named(GAME_CONTROL_PACKET_NAME);
    packet.write_i32(control as i32);
    packet.write_u8(0);
    packet.write_u32(value0);
    packet.write_i32(0);
    packet.write_i32(0);
    packet.write_u8(0);
    packet.write_i32(0);
    packet.write_bytes(&[0; 40]);
    packet.write_bytes(&[0; 10]);
    packet.write_i32(0);
    packet.write_i32(0);
    packet.write_encoded_u8(0);
    debug_assert_eq!(
        packet.as_slice().len(),
        4 + CANONICAL_GAME_CONTROL_BODY_LENGTH
    );
    packet.into_inner()
}

#[must_use]
pub fn serialize_ai_master_notice() -> Vec<u8> {
    let mut packet = PacketWriter::named(GAME_AI_MASTER_NOTICE_NAME);
    packet.write_i32(0);
    packet.into_inner()
}

pub fn serialize_race_time(player_id: i32, race_time: u32) -> Result<Vec<u8>, RaceProtocolError> {
    validate_player_id(player_id)?;
    let mut packet = PacketWriter::named(GAME_RACE_TIME_PACKET_NAME);
    packet.write_i32(player_id);
    packet.write_u32(race_time);
    Ok(packet.into_inner())
}

pub fn serialize_team_booster_gauge(
    team: RaceTeam,
    gauge: f32,
) -> Result<Vec<u8>, RaceProtocolError> {
    if !gauge.is_finite() || !(0.0..=1.0).contains(&gauge) {
        return Err(RaceProtocolError::InvalidBoosterGauge);
    }
    let mut packet = PacketWriter::named(TEAM_BOOSTER_REPLY_NAME);
    packet.write_u8(team as u8);
    packet.write_f32(gauge);
    Ok(packet.into_inner())
}

#[must_use]
pub fn serialize_game_next_stage(game_type: u8) -> Vec<u8> {
    let mut packet = PacketWriter::named(GAME_NEXT_STAGE_PACKET_NAME);
    packet.write_u8(game_type);
    packet.write_i32(0);
    packet.write_i32(0);
    packet.into_inner()
}

fn expect_hash(reader: &mut PacketReader<'_>, name: &'static str) -> Result<(), RaceProtocolError> {
    let actual = reader.read_u32()?;
    let expected = adler32::packet_hash(name);
    if actual == expected {
        Ok(())
    } else {
        Err(RaceProtocolError::UnexpectedPacketHash {
            name,
            expected,
            actual,
        })
    }
}

fn ensure_exhausted(
    reader: &PacketReader<'_>,
    name: &'static str,
) -> Result<(), RaceProtocolError> {
    if reader.remaining().is_empty() {
        Ok(())
    } else {
        Err(RaceProtocolError::TrailingBytes {
            name,
            count: reader.remaining().len(),
        })
    }
}

fn validate_player_id(player_id: i32) -> Result<(), RaceProtocolError> {
    if (0..=15).contains(&player_id) {
        Ok(())
    } else {
        Err(RaceProtocolError::InvalidPlayerId(player_id))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CANONICAL_GAME_CONTROL_BODY_LENGTH, GAME_AI_GOAL_IN_PACKET_NAME,
        GAME_CONTROL_FINISH_TRAILING_LENGTH, GAME_CONTROL_PACKET_NAME, RaceProtocolError,
        RaceRequest, RaceTeam, ServerGameControl, TEAM_BOOSTER_REQUEST_NAME, classify_race_request,
        parse_ai_goal_in_request, parse_game_control_finish_snapshot, parse_game_control_request,
        parse_team_booster_request, serialize_ai_master_notice, serialize_game_control,
        serialize_game_next_stage, serialize_race_time, serialize_team_booster_gauge,
    };
    use crate::{
        adler32, encoded, game_slot_protocol::GAME_SLOT_PACKET_NAME, packet::PacketWriter,
    };

    #[test]
    fn dispatch_hashes_match_the_p5136_packet_table() {
        let fixtures = [
            (
                GAME_CONTROL_PACKET_NAME,
                0x3ACB_06B3,
                RaceRequest::GameControl,
            ),
            (
                GAME_AI_GOAL_IN_PACKET_NAME,
                0x3ED6_06D6,
                RaceRequest::AiGoalIn,
            ),
            (
                TEAM_BOOSTER_REQUEST_NAME,
                0x0269_0E12,
                RaceRequest::TeamBoosterGauge,
            ),
            (GAME_SLOT_PACKET_NAME, 0x27C0_0574, RaceRequest::GameSlot),
        ];
        for (name, hash, request) in fixtures {
            assert_eq!(adler32::packet_hash(name), hash);
            assert_eq!(classify_race_request(hash), Some(request));
        }
    }

    #[test]
    fn client_control_minimum_and_optional_pair_layouts_parse() {
        let start = parse_game_control_request(&decode_hex("B306CB3A000000000078563412")).unwrap();
        assert_eq!(start.state, 0);
        assert_eq!(start.optional_pair, None);
        assert_eq!(start.value0, 0x1234_5678);
        assert!(start.trailing.is_empty());

        let finish = parse_game_control_request(&decode_hex(
            "B306CB3A020000000101000000FFFFFFFFEFBEADDEAA55",
        ))
        .unwrap();
        assert_eq!(finish.state, 2);
        assert_eq!(finish.optional_pair, Some((1, -1)));
        assert_eq!(finish.value0, 0xDEAD_BEEF);
        assert_eq!(finish.trailing, [0xaa, 0x55]);

        let mut captured_finish = PacketWriter::named(GAME_CONTROL_PACKET_NAME);
        captured_finish.write_i32(2);
        captured_finish.write_u8(0);
        captured_finish.write_u32(0x0001_C9F8);
        captured_finish.write_u32(0xAABB_CCDD);
        captured_finish.write_u32(0x0102_0304);
        captured_finish.write_u8(1);
        for word in 0..7 {
            captured_finish.write_u32(word);
        }
        captured_finish.write_bytes(&[0x11; 54]);
        captured_finish.write_u32(0x5566_7788);
        captured_finish.write_bytes(&[0x22; 243]);
        captured_finish.write_bytes(&[0x33; 18]);
        for slot in 10..18 {
            captured_finish.write_u32(slot);
        }
        captured_finish.write_u32(0x99AA_BBCC);
        captured_finish.write_u8(7);
        let captured_finish = parse_game_control_request(captured_finish.as_slice()).unwrap();
        assert_eq!(captured_finish.state, 2);
        assert_eq!(captured_finish.value0, 0x0001_C9F8);
        assert_eq!(
            captured_finish.trailing.len(),
            GAME_CONTROL_FINISH_TRAILING_LENGTH
        );
        let snapshot = parse_game_control_finish_snapshot(&captured_finish)
            .unwrap()
            .unwrap();
        assert_eq!(snapshot.value1, 0xAABB_CCDD);
        assert_eq!(snapshot.value2, 0x0102_0304);
        assert_eq!(snapshot.session_auth, [0, 1, 2, 3, 4, 5, 6]);
        assert_eq!(snapshot.result_global_metric, 0x5566_7788);
        assert_eq!(snapshot.participant_slots, [10, 11, 12, 13, 14, 15, 16, 17]);
        assert_eq!(snapshot.local_kart_or_player_result, 0x99AA_BBCC);
        assert_eq!(snapshot.terminal_client_state, 7);
    }

    #[test]
    fn canonical_server_control_body_matches_csharp_shape() {
        let packet = serialize_game_control(ServerGameControl::RaceStart, 0x1234_5678);
        assert_eq!(packet.len(), 4 + CANONICAL_GAME_CONTROL_BODY_LENGTH);
        assert_eq!(&packet[..13], &decode_hex("B306CB3A010000000078563412"));
        assert!(packet[13..84].iter().all(|byte| *byte == 0));
        assert_eq!(packet[84], encoded::encode_u8(0));
    }

    #[test]
    fn goal_time_gauge_and_next_stage_match_csharp_goldens() {
        assert_eq!(
            parse_ai_goal_in_request(&decode_hex("D606D63E0200000078563412"))
                .unwrap()
                .race_time,
            0x1234_5678
        );
        assert_eq!(
            serialize_race_time(2, 0x1234_5678).unwrap(),
            decode_hex("DC06823F0200000078563412")
        );

        let gauge_request = parse_team_booster_request(&decode_hex("120E6902010000003F")).unwrap();
        assert_eq!(gauge_request.team, RaceTeam::Red);
        assert_eq!(gauge_request.contribution.to_bits(), 0.5_f32.to_bits());
        assert_eq!(
            serialize_team_booster_gauge(RaceTeam::Blue, 0.75).unwrap(),
            decode_hex("4C0B1EA7020000403F")
        );
        assert_eq!(
            serialize_game_next_stage(3),
            decode_hex("65079148030000000000000000")
        );
        assert_eq!(serialize_ai_master_notice(), decode_hex("EC0A179B00000000"));
    }

    #[test]
    fn malformed_race_controls_and_values_are_rejected() {
        assert!(parse_game_control_request(&[0; 8]).is_err());

        let mut oversized = PacketWriter::named(GAME_CONTROL_PACKET_NAME);
        oversized.write_i32(0);
        oversized.write_u8(0);
        oversized.write_u32(0);
        oversized.write_bytes(&[0; 513]);
        assert!(matches!(
            parse_game_control_request(oversized.as_slice()),
            Err(RaceProtocolError::GameControlTailTooLarge {
                actual: 513,
                maximum: 512,
            })
        ));

        let mut invalid_team = PacketWriter::named(TEAM_BOOSTER_REQUEST_NAME);
        invalid_team.write_u8(3);
        invalid_team.write_f32(1.0);
        assert!(matches!(
            parse_team_booster_request(invalid_team.as_slice()),
            Err(RaceProtocolError::InvalidTeam(3))
        ));

        let mut invalid_value = PacketWriter::named(TEAM_BOOSTER_REQUEST_NAME);
        invalid_value.write_u8(1);
        invalid_value.write_f32(f32::NAN);
        assert!(matches!(
            parse_team_booster_request(invalid_value.as_slice()),
            Err(RaceProtocolError::InvalidBoosterContribution)
        ));
        assert!(matches!(
            serialize_team_booster_gauge(RaceTeam::Red, 1.1),
            Err(RaceProtocolError::InvalidBoosterGauge)
        ));
        assert!(matches!(
            serialize_race_time(16, 0),
            Err(RaceProtocolError::InvalidPlayerId(16))
        ));
    }

    fn decode_hex(input: &str) -> Vec<u8> {
        input
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }
}
