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
pub const START_COLLECT_RECORD_REQUEST_NAME: &str = "PqStartCollectRecord";
pub const START_COLLECT_RECORD_REPLY_NAME: &str = "PrStartCollectRecord";
pub const REPORT_USER_COLLECTED_RECORD_NAME: &str = "PcReportUserCollectedRecord";
pub const REPORT_GAME_COLLECTED_RECORD_REQUEST_NAME: &str = "PqReportGameCollectedRecord";

/// The retained P5136 finish producer sends a 406-byte `GameControlPacket`:
/// 13 common bytes followed by a 393-byte state-2 result snapshot.  The
/// request parser retains a small future-compatible margin for other control
/// states without cloning the C# handler's unbounded ignored tail.
pub const MAX_GAME_CONTROL_TRAILING_BYTES: usize = 512;
pub const CANONICAL_GAME_CONTROL_BODY_LENGTH: usize = 81;
pub const GAME_CONTROL_FINISH_TRAILING_LENGTH: usize = 393;

const GAME_CONTROL_FINISH_SESSION_AUTH_WORDS: usize = 7;
const GAME_CONTROL_FINISH_RESULT_SUBOBJECT_LENGTH: usize = 54;
const GAME_CONTROL_FINISH_KART_SPEC_LENGTH: usize = 235;
const GAME_CONTROL_FINISH_SHARED_OBJECT_PAYLOAD_LENGTH: usize = 22;
const GAME_CONTROL_FINISH_PARTICIPANT_SLOT_COUNT: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RaceRequest {
    GameControl,
    AiGoalIn,
    TeamBoosterGauge,
    GameSlot,
    StartCollectRecord,
    ReportUserCollectedRecord,
    ReportGameCollectedRecord,
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
    pub kart_spec_snapshot: [u8; GAME_CONTROL_FINISH_KART_SPEC_LENGTH],
    pub shared_object: [u8; GAME_CONTROL_FINISH_SHARED_OBJECT_PAYLOAD_LENGTH],
    pub participant_slots: [u32; GAME_CONTROL_FINISH_PARTICIPANT_SLOT_COUNT],
    pub result_global_metric: u32,
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

/// Recorder statistics submitted by the stock client after a recorded race.
///
/// Native readers `0x00728430`/`0x0072B780` and writers
/// `0x0072E4A0`/`0x00730F30` serialize five consecutive little-endian dwords
/// at object offsets `0x10..=0x20`. Producer `0x00A84930` establishes the
/// first member as elapsed collection time. The four recorder-summary members
/// are retained for diagnostics only until their product semantics are known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserCollectedRecordReport {
    pub elapsed_ms: u32,
    pub recorder_metric_1: u32,
    pub recorder_metric_2: u32,
    pub recorder_metric_3: u32,
    pub recorder_metric_4: u32,
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

    #[error(
        "GameControl state-2 shared object declares {actual} bytes; expected exactly {expected}"
    )]
    InvalidGameControlSharedObjectLength { actual: u32, expected: usize },

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
        (
            START_COLLECT_RECORD_REQUEST_NAME,
            RaceRequest::StartCollectRecord,
        ),
        (
            REPORT_USER_COLLECTED_RECORD_NAME,
            RaceRequest::ReportUserCollectedRecord,
        ),
        (
            REPORT_GAME_COLLECTED_RECORD_REQUEST_NAME,
            RaceRequest::ReportGameCollectedRecord,
        ),
    ]
    .into_iter()
    .find_map(|(name, request)| (adler32::packet_hash(name) == hash).then_some(request))
}

/// Parses the native hash-only `PqStartCollectRecord` request.
///
/// The P5136 request class is 16 bytes in memory and all four codec vtable
/// slots delegate only to the packet base (`0x00578C50`). Consequently the
/// complete logical wire packet is exactly the four-byte RTTI-name hash.
pub fn parse_start_collect_record_request(packet: &[u8]) -> Result<(), RaceProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, START_COLLECT_RECORD_REQUEST_NAME)?;
    ensure_exhausted(&reader, START_COLLECT_RECORD_REQUEST_NAME)
}

/// Parses the exact 24-byte native `PcReportUserCollectedRecord` report.
pub fn parse_user_collected_record_report(
    packet: &[u8],
) -> Result<UserCollectedRecordReport, RaceProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, REPORT_USER_COLLECTED_RECORD_NAME)?;
    let report = UserCollectedRecordReport {
        elapsed_ms: reader.read_u32()?,
        recorder_metric_1: reader.read_u32()?,
        recorder_metric_2: reader.read_u32()?,
        recorder_metric_3: reader.read_u32()?,
        recorder_metric_4: reader.read_u32()?,
    };
    ensure_exhausted(&reader, REPORT_USER_COLLECTED_RECORD_NAME)?;
    Ok(report)
}

/// Parses the base-only native `PqReportGameCollectedRecord` request.
///
/// Its 16-byte in-memory class delegates all codec slots to the packet base,
/// so the complete logical wire packet contains only its four-byte name hash.
pub fn parse_report_game_collected_record_request(packet: &[u8]) -> Result<(), RaceProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, REPORT_GAME_COLLECTED_RECORD_REQUEST_NAME)?;
    ensure_exhausted(&reader, REPORT_GAME_COLLECTED_RECORD_REQUEST_NAME)
}

/// Serializes the native five-byte `PrStartCollectRecord` response.
///
/// Native readers `0x00593260`/`0x00593590` target object offset `0x10`, and
/// writers `0x005938C0`/`0x00593BF0` emit that member through a raw one-byte
/// primitive. The common `GameStage` consumer treats zero as false and every
/// nonzero value as true; this writer deliberately emits only canonical C++
/// boolean values 0 and 1.
#[must_use]
pub fn serialize_start_collect_record_reply(flag: bool) -> Vec<u8> {
    let mut packet = PacketWriter::named(START_COLLECT_RECORD_REPLY_NAME);
    packet.write_u8(u8::from(flag));
    packet.into_inner()
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
    let mut kart_spec_snapshot = [0; GAME_CONTROL_FINISH_KART_SPEC_LENGTH];
    kart_spec_snapshot.copy_from_slice(reader.read_bytes(GAME_CONTROL_FINISH_KART_SPEC_LENGTH)?);
    let shared_length = reader.read_u32()?;
    if shared_length
        != u32::try_from(GAME_CONTROL_FINISH_SHARED_OBJECT_PAYLOAD_LENGTH).unwrap_or(u32::MAX)
    {
        return Err(RaceProtocolError::InvalidGameControlSharedObjectLength {
            actual: shared_length,
            expected: GAME_CONTROL_FINISH_SHARED_OBJECT_PAYLOAD_LENGTH,
        });
    }
    let mut shared_object = [0; GAME_CONTROL_FINISH_SHARED_OBJECT_PAYLOAD_LENGTH];
    shared_object
        .copy_from_slice(reader.read_bytes(GAME_CONTROL_FINISH_SHARED_OBJECT_PAYLOAD_LENGTH)?);
    let mut participant_slots = [0; GAME_CONTROL_FINISH_PARTICIPANT_SLOT_COUNT];
    for slot in &mut participant_slots {
        *slot = reader.read_u32()?;
    }
    let result_global_metric = reader.read_u32()?;
    let local_kart_or_player_result = reader.read_u32()?;
    let terminal_client_state = reader.read_u8()?;
    ensure_exhausted(&reader, GAME_CONTROL_PACKET_NAME)?;

    Ok(Some(GameControlFinishSnapshot {
        value1,
        value2,
        session_auth,
        result_subobject,
        kart_spec_snapshot,
        shared_object,
        participant_slots,
        result_global_metric,
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
        GAME_CONTROL_FINISH_TRAILING_LENGTH, GAME_CONTROL_PACKET_NAME,
        REPORT_GAME_COLLECTED_RECORD_REQUEST_NAME, REPORT_USER_COLLECTED_RECORD_NAME,
        RaceProtocolError, RaceRequest, RaceTeam, START_COLLECT_RECORD_REPLY_NAME,
        START_COLLECT_RECORD_REQUEST_NAME, ServerGameControl, TEAM_BOOSTER_REQUEST_NAME,
        classify_race_request, parse_ai_goal_in_request, parse_game_control_finish_snapshot,
        parse_game_control_request, parse_report_game_collected_record_request,
        parse_start_collect_record_request, parse_team_booster_request,
        parse_user_collected_record_report, serialize_ai_master_notice, serialize_game_control,
        serialize_game_next_stage, serialize_race_time, serialize_start_collect_record_reply,
        serialize_team_booster_gauge,
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
            (
                START_COLLECT_RECORD_REQUEST_NAME,
                0x5291_07F4,
                RaceRequest::StartCollectRecord,
            ),
            (
                REPORT_USER_COLLECTED_RECORD_NAME,
                0x94F4_0ABC,
                RaceRequest::ReportUserCollectedRecord,
            ),
            (
                REPORT_GAME_COLLECTED_RECORD_REQUEST_NAME,
                0x93CA_0AA5,
                RaceRequest::ReportGameCollectedRecord,
            ),
        ];
        for (name, hash, request) in fixtures {
            assert_eq!(adler32::packet_hash(name), hash);
            assert_eq!(classify_race_request(hash), Some(request));
        }
    }

    #[test]
    fn start_collect_record_native_codec_is_hash_only_request_and_raw_boolean_reply() {
        let request = 0x5291_07F4_u32.to_le_bytes();
        parse_start_collect_record_request(&request).unwrap();
        assert_eq!(
            adler32::packet_hash(START_COLLECT_RECORD_REPLY_NAME),
            0x52A4_07F5
        );
        assert_eq!(
            serialize_start_collect_record_reply(false),
            [0xF5, 0x07, 0xA4, 0x52, 0]
        );
        assert_eq!(
            serialize_start_collect_record_reply(true),
            [0xF5, 0x07, 0xA4, 0x52, 1]
        );

        assert!(matches!(
            parse_start_collect_record_request(&request[..3]),
            Err(RaceProtocolError::Packet(_))
        ));
        let mut trailing = request.to_vec();
        trailing.push(0);
        assert!(matches!(
            parse_start_collect_record_request(&trailing),
            Err(RaceProtocolError::TrailingBytes {
                name: START_COLLECT_RECORD_REQUEST_NAME,
                count: 1,
            })
        ));
    }

    #[test]
    fn collected_record_finish_packets_match_native_codecs() {
        let captured = decode_hex("BC0AF494D294010000000000670000005F00000039010000");
        let report = parse_user_collected_record_report(&captured).unwrap();
        assert_eq!(report.elapsed_ms, 103_634);
        assert_eq!(report.recorder_metric_1, 0);
        assert_eq!(report.recorder_metric_2, 103);
        assert_eq!(report.recorder_metric_3, 95);
        assert_eq!(report.recorder_metric_4, 313);

        for length in 0..captured.len() {
            assert!(parse_user_collected_record_report(&captured[..length]).is_err());
        }
        let mut trailing_report = captured.clone();
        trailing_report.push(0);
        assert!(matches!(
            parse_user_collected_record_report(&trailing_report),
            Err(RaceProtocolError::TrailingBytes {
                name: REPORT_USER_COLLECTED_RECORD_NAME,
                count: 1,
            })
        ));

        let request = 0x93CA_0AA5_u32.to_le_bytes();
        parse_report_game_collected_record_request(&request).unwrap();
        assert!(parse_report_game_collected_record_request(&request[..3]).is_err());
        let mut trailing_request = request.to_vec();
        trailing_request.push(0);
        assert!(matches!(
            parse_report_game_collected_record_request(&trailing_request),
            Err(RaceProtocolError::TrailingBytes {
                name: REPORT_GAME_COLLECTED_RECORD_REQUEST_NAME,
                count: 1,
            })
        ));
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
        captured_finish.write_bytes(&[0x22; 235]);
        captured_finish.write_u32(22);
        captured_finish.write_bytes(&[0x33; 22]);
        for slot in 10..18 {
            captured_finish.write_u32(slot);
        }
        captured_finish.write_u32(0x5566_7788);
        captured_finish.write_u32(0x99AA_BBCC);
        captured_finish.write_u8(7);
        let captured_finish_bytes = captured_finish.into_inner();
        let captured_finish = parse_game_control_request(&captured_finish_bytes).unwrap();
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
        assert_eq!(snapshot.result_subobject, [0x11; 54]);
        assert_eq!(snapshot.kart_spec_snapshot, [0x22; 235]);
        assert_eq!(snapshot.shared_object, [0x33; 22]);
        assert_eq!(snapshot.result_global_metric, 0x5566_7788);
        assert_eq!(snapshot.participant_slots, [10, 11, 12, 13, 14, 15, 16, 17]);
        assert_eq!(snapshot.local_kart_or_player_result, 0x99AA_BBCC);
        assert_eq!(snapshot.terminal_client_state, 7);

        let mut malformed_shared_length = captured_finish_bytes;
        malformed_shared_length[339..343].copy_from_slice(&21_u32.to_le_bytes());
        let malformed_shared_length = parse_game_control_request(&malformed_shared_length).unwrap();
        assert!(matches!(
            parse_game_control_finish_snapshot(&malformed_shared_length),
            Err(RaceProtocolError::InvalidGameControlSharedObjectLength {
                actual: 21,
                expected: 22,
            })
        ));
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
