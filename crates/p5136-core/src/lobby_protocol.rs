//! P5136 ready-stage room state codecs.
//!
//! Room admission and the first snapshot live in [`crate::room_protocol`].
//! This module covers the small, stateful request surface between that
//! snapshot and race start: ready/preparing state, team selection, room-master
//! selection, and the start request/reply.

use thiserror::Error;

use crate::{
    adler32,
    packet::{PacketError, PacketReader, PacketWriter},
    room_protocol::{MAX_RIDER_NICKNAME_UTF16_UNITS, ROOM_SLOT_COUNT},
};

pub const SET_SLOT_STATE_REQUEST_NAME: &str = "GrRequestSetSlotStatePacket";
pub const SET_SLOT_STATE_REPLY_NAME: &str = "GrReplySetSlotStatePacket";
pub const SLOT_STATE_PACKET_NAME: &str = "GrSlotStatePacket";
pub const CHANGE_TEAM_REQUEST_NAME: &str = "GrChangeTeamPacket";
pub const CHANGE_TEAM_REPLY_NAME: &str = "GrChangeTeamPacketReply";
pub const CHANGE_MASTER_REQUEST_NAME: &str = "PqRoomMasterChangePacket";
pub const START_ROOM_REQUEST_NAME: &str = "GrRequestStartPacket";
pub const START_ROOM_REPLY_NAME: &str = "GrReplyStartPacket";

pub const ROOM_OBSERVER_ID_END: i32 = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LobbyRequest {
    SetSlotState,
    ChangeTeam,
    ChangeMaster,
    StartRoom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerSlotState {
    /// Used by the room master and by a guest cancelling ready.
    NotReady = 2,
    Ready = 3,
    Observer = 4,
    Preparing = 5,
}

impl PlayerSlotState {
    fn from_wire(value: i32) -> Result<Self, LobbyProtocolError> {
        match value {
            2 => Ok(Self::NotReady),
            3 => Ok(Self::Ready),
            4 => Ok(Self::Observer),
            5 => Ok(Self::Preparing),
            _ => Err(LobbyProtocolError::InvalidPlayerSlotState(value)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomTeam {
    Red = 1,
    Blue = 2,
}

impl RoomTeam {
    fn from_wire(value: u8) -> Result<Self, LobbyProtocolError> {
        match value {
            1 => Ok(Self::Red),
            2 => Ok(Self::Blue),
            _ => Err(LobbyProtocolError::InvalidTeam(value)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetSlotStateRequest {
    pub state: PlayerSlotState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeTeamRequest {
    pub team: RoomTeam,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeMasterRequest {
    pub target_nickname: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartRoomRequest {
    /// The active P5136 client sends one unused signed integer.
    pub reserved: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartRoomStatus {
    Success = 0,
    NotAllReady = 2,
}

#[derive(Debug, Error)]
pub enum LobbyProtocolError {
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

    #[error("player slot state {0} is not one of 2, 3, 4, or 5")]
    InvalidPlayerSlotState(i32),

    #[error("room team {0} is not red (1) or blue (2)")]
    InvalidTeam(u8),

    #[error("room player ID {0} is outside 0..=15")]
    InvalidPlayerId(i32),

    #[error("room slot state {state} at ID {id} is invalid")]
    InvalidSnapshotSlotState { id: usize, state: i32 },

    #[error("room slot position {position} at index {index} is outside -1..=7")]
    InvalidSlotPosition { index: usize, position: i32 },

    #[error("master nickname has {actual} UTF-16 units; maximum is {maximum}")]
    NicknameTooLong { actual: usize, maximum: usize },
}

#[must_use]
pub fn classify_lobby_request(hash: u32) -> Option<LobbyRequest> {
    [
        (SET_SLOT_STATE_REQUEST_NAME, LobbyRequest::SetSlotState),
        (CHANGE_TEAM_REQUEST_NAME, LobbyRequest::ChangeTeam),
        (CHANGE_MASTER_REQUEST_NAME, LobbyRequest::ChangeMaster),
        (START_ROOM_REQUEST_NAME, LobbyRequest::StartRoom),
    ]
    .into_iter()
    .find_map(|(name, request)| (adler32::packet_hash(name) == hash).then_some(request))
}

pub fn parse_set_slot_state_request(
    packet: &[u8],
) -> Result<SetSlotStateRequest, LobbyProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, SET_SLOT_STATE_REQUEST_NAME)?;
    let request = SetSlotStateRequest {
        state: PlayerSlotState::from_wire(reader.read_i32()?)?,
    };
    ensure_exhausted(&reader, SET_SLOT_STATE_REQUEST_NAME)?;
    Ok(request)
}

pub fn parse_change_team_request(packet: &[u8]) -> Result<ChangeTeamRequest, LobbyProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, CHANGE_TEAM_REQUEST_NAME)?;
    let request = ChangeTeamRequest {
        team: RoomTeam::from_wire(reader.read_u8()?)?,
    };
    ensure_exhausted(&reader, CHANGE_TEAM_REQUEST_NAME)?;
    Ok(request)
}

pub fn parse_change_master_request(
    packet: &[u8],
) -> Result<ChangeMasterRequest, LobbyProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, CHANGE_MASTER_REQUEST_NAME)?;
    let request = ChangeMasterRequest {
        target_nickname: reader.read_utf16_bounded(MAX_RIDER_NICKNAME_UTF16_UNITS)?,
    };
    ensure_exhausted(&reader, CHANGE_MASTER_REQUEST_NAME)?;
    Ok(request)
}

pub fn parse_start_room_request(packet: &[u8]) -> Result<StartRoomRequest, LobbyProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, START_ROOM_REQUEST_NAME)?;
    let request = StartRoomRequest {
        reserved: reader.read_i32()?,
    };
    ensure_exhausted(&reader, START_ROOM_REQUEST_NAME)?;
    Ok(request)
}

pub fn serialize_slot_state(
    states_by_id: [i32; ROOM_SLOT_COUNT],
) -> Result<Vec<u8>, LobbyProtocolError> {
    for (id, state) in states_by_id.into_iter().enumerate() {
        if !matches!(state, 0 | 1 | 2 | 3 | 4 | 5 | 7) {
            return Err(LobbyProtocolError::InvalidSnapshotSlotState { id, state });
        }
    }
    let mut packet = PacketWriter::named(SLOT_STATE_PACKET_NAME);
    for state in states_by_id {
        packet.write_i32(state);
    }
    packet.write_bytes(&[0; 32]);
    Ok(packet.into_inner())
}

pub fn serialize_set_slot_state_reply(
    user_no: u32,
    accepted: bool,
    player_id: i32,
    state: PlayerSlotState,
) -> Result<Vec<u8>, LobbyProtocolError> {
    validate_player_id(player_id)?;
    let mut packet = PacketWriter::named(SET_SLOT_STATE_REPLY_NAME);
    packet.write_u32(user_no);
    packet.write_u8(u8::from(accepted));
    packet.write_i32(player_id);
    packet.write_i32(state as i32);
    Ok(packet.into_inner())
}

pub fn serialize_change_team_reply(
    player_id: i32,
    team: RoomTeam,
    slot_positions: [i32; ROOM_SLOT_COUNT],
) -> Result<Vec<u8>, LobbyProtocolError> {
    validate_player_id(player_id)?;
    validate_slot_positions(slot_positions)?;
    let mut packet = PacketWriter::named(CHANGE_TEAM_REPLY_NAME);
    packet.write_i32(player_id);
    packet.write_u8(team as u8);
    write_slot_positions(&mut packet, slot_positions);
    Ok(packet.into_inner())
}

#[must_use]
pub fn serialize_start_room_reply(status: StartRoomStatus) -> Vec<u8> {
    let mut packet = PacketWriter::named(START_ROOM_REPLY_NAME);
    packet.write_i32(status as i32);
    packet.into_inner()
}

fn expect_hash(
    reader: &mut PacketReader<'_>,
    name: &'static str,
) -> Result<(), LobbyProtocolError> {
    let actual = reader.read_u32()?;
    let expected = adler32::packet_hash(name);
    if actual == expected {
        Ok(())
    } else {
        Err(LobbyProtocolError::UnexpectedPacketHash {
            name,
            expected,
            actual,
        })
    }
}

fn ensure_exhausted(
    reader: &PacketReader<'_>,
    name: &'static str,
) -> Result<(), LobbyProtocolError> {
    if reader.remaining().is_empty() {
        Ok(())
    } else {
        Err(LobbyProtocolError::TrailingBytes {
            name,
            count: reader.remaining().len(),
        })
    }
}

fn validate_player_id(player_id: i32) -> Result<(), LobbyProtocolError> {
    if (0..=ROOM_OBSERVER_ID_END).contains(&player_id) {
        Ok(())
    } else {
        Err(LobbyProtocolError::InvalidPlayerId(player_id))
    }
}

fn validate_slot_positions(positions: [i32; ROOM_SLOT_COUNT]) -> Result<(), LobbyProtocolError> {
    for (index, position) in positions.into_iter().enumerate() {
        if !(-1..=7).contains(&position) {
            return Err(LobbyProtocolError::InvalidSlotPosition { index, position });
        }
    }
    Ok(())
}

fn write_slot_positions(packet: &mut PacketWriter, positions: [i32; ROOM_SLOT_COUNT]) {
    for position in positions {
        packet.write_i32(position);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CHANGE_MASTER_REQUEST_NAME, CHANGE_TEAM_REQUEST_NAME, LobbyProtocolError, LobbyRequest,
        PlayerSlotState, RoomTeam, SET_SLOT_STATE_REQUEST_NAME, START_ROOM_REQUEST_NAME,
        StartRoomStatus, classify_lobby_request, parse_change_master_request,
        parse_change_team_request, parse_set_slot_state_request, parse_start_room_request,
        serialize_change_team_reply, serialize_set_slot_state_reply, serialize_slot_state,
        serialize_start_room_reply,
    };
    use crate::{adler32, packet::PacketWriter};

    #[test]
    fn dispatch_uses_the_exact_p5136_packet_names_and_hashes() {
        let fixtures = [
            (
                SET_SLOT_STATE_REQUEST_NAME,
                0x95F9_0AC9,
                LobbyRequest::SetSlotState,
            ),
            (
                CHANGE_TEAM_REQUEST_NAME,
                0x3F8F_06DE,
                LobbyRequest::ChangeTeam,
            ),
            (
                CHANGE_MASTER_REQUEST_NAME,
                0x74C7_0968,
                LobbyRequest::ChangeMaster,
            ),
            (
                START_ROOM_REQUEST_NAME,
                0x5341_0808,
                LobbyRequest::StartRoom,
            ),
        ];
        for (name, expected_hash, request) in fixtures {
            assert_eq!(adler32::packet_hash(name), expected_hash);
            assert_eq!(classify_lobby_request(expected_hash), Some(request));
        }
        assert_eq!(classify_lobby_request(0xDEAD_BEEF), None);
    }

    #[test]
    fn ready_team_master_and_start_requests_match_csharp_goldens() {
        assert_eq!(
            parse_set_slot_state_request(&decode_hex("C90AF99503000000"))
                .unwrap()
                .state,
            PlayerSlotState::Ready
        );
        assert_eq!(
            parse_change_team_request(&decode_hex("DE068F3F02"))
                .unwrap()
                .team,
            RoomTeam::Blue
        );
        assert_eq!(
            parse_change_master_request(&decode_hex("6809C774040000005000650065007200"))
                .unwrap()
                .target_nickname,
            "Peer"
        );
        assert_eq!(
            parse_start_room_request(&decode_hex("0808415300000000"))
                .unwrap()
                .reserved,
            0
        );
    }

    #[test]
    fn ready_stage_replies_match_csharp_field_order() {
        let positions = [5, 4, -1, -1, -1, -1, -1, -1];
        let ready_reply =
            serialize_set_slot_state_reply(17, true, 2, PlayerSlotState::Ready).unwrap();
        assert_eq!(ready_reply.len(), 17);
        assert_eq!(
            ready_reply,
            decode_hex("EC099E7F11000000010200000003000000")
        );
        assert_eq!(
            serialize_change_team_reply(2, RoomTeam::Blue, positions).unwrap(),
            decode_hex(concat!(
                "EA08B4670200000002",
                "0500000004000000FFFFFFFFFFFFFFFF",
                "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF"
            ))
        );
        assert_eq!(
            serialize_start_room_reply(StartRoomStatus::Success),
            decode_hex("2B07F14200000000")
        );
        assert_eq!(
            serialize_start_room_reply(StartRoomStatus::NotAllReady),
            decode_hex("2B07F14202000000")
        );
    }

    #[test]
    fn slot_state_snapshot_is_eight_i32_values_then_thirty_two_zero_bytes() {
        let packet = serialize_slot_state([2, 3, 0, 7, 0, 0, 0, 0]).unwrap();
        assert_eq!(
            packet,
            decode_hex(concat!(
                "B406643B",
                "02000000030000000000000007000000",
                "00000000000000000000000000000000",
                "00000000000000000000000000000000",
                "00000000000000000000000000000000"
            ))
        );
    }

    #[test]
    fn request_and_response_validation_rejects_spoofed_shapes() {
        let mut invalid_state = PacketWriter::named(SET_SLOT_STATE_REQUEST_NAME);
        invalid_state.write_i32(6);
        assert!(matches!(
            parse_set_slot_state_request(invalid_state.as_slice()),
            Err(LobbyProtocolError::InvalidPlayerSlotState(6))
        ));

        let mut invalid_team = PacketWriter::named(CHANGE_TEAM_REQUEST_NAME);
        invalid_team.write_u8(3);
        assert!(matches!(
            parse_change_team_request(invalid_team.as_slice()),
            Err(LobbyProtocolError::InvalidTeam(3))
        ));

        let mut trailing = PacketWriter::named(START_ROOM_REQUEST_NAME);
        trailing.write_i32(0);
        trailing.write_u8(0);
        assert!(matches!(
            parse_start_room_request(trailing.as_slice()),
            Err(LobbyProtocolError::TrailingBytes { count: 1, .. })
        ));

        assert!(matches!(
            serialize_slot_state([0, 0, 0, 0, 0, 0, 0, 6]),
            Err(LobbyProtocolError::InvalidSnapshotSlotState { id: 7, state: 6 })
        ));
        assert!(matches!(
            serialize_change_team_reply(16, RoomTeam::Red, [-1; 8]),
            Err(LobbyProtocolError::InvalidPlayerId(16))
        ));
        assert!(matches!(
            serialize_set_slot_state_reply(1, true, 16, PlayerSlotState::Ready),
            Err(LobbyProtocolError::InvalidPlayerId(16))
        ));
    }

    #[test]
    fn master_target_length_is_bounded_before_string_allocation() {
        let mut packet = PacketWriter::named(CHANGE_MASTER_REQUEST_NAME);
        packet.write_i32(33);
        assert!(matches!(
            parse_change_master_request(packet.as_slice()),
            Err(LobbyProtocolError::Packet(
                crate::packet::PacketError::StringLimitExceeded {
                    length: 33,
                    maximum: 32,
                }
            ))
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
