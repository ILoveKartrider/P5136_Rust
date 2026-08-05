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
    room_protocol::{
        MAX_RIDER_NICKNAME_UTF16_UNITS, MAX_ROOM_NAME_UTF16_UNITS, MAX_ROOM_PASSWORD_UTF16_UNITS,
        ROOM_DATA_LENGTH, ROOM_SLOT_COUNT, RoomAi,
    },
};

pub const SET_SLOT_STATE_REQUEST_NAME: &str = "GrRequestSetSlotStatePacket";
pub const SET_SLOT_STATE_REPLY_NAME: &str = "GrReplySetSlotStatePacket";
pub const SLOT_STATE_PACKET_NAME: &str = "GrSlotStatePacket";
pub const CHANGE_TEAM_REQUEST_NAME: &str = "GrChangeTeamPacket";
pub const CHANGE_TEAM_REPLY_NAME: &str = "GrChangeTeamPacketReply";
pub const CHANGE_MASTER_REQUEST_NAME: &str = "PqRoomMasterChangePacket";
pub const START_ROOM_REQUEST_NAME: &str = "GrRequestStartPacket";
pub const START_ROOM_REPLY_NAME: &str = "GrReplyStartPacket";
pub const CHANGE_TRACK_REQUEST_NAME: &str = "GrChangeTrackPacket";
pub const BASIC_AI_REQUEST_NAME: &str = "GrRequestBasicAiPacket";
pub const BASIC_AI_SLOT_DATA_NAME: &str = "GrSlotDataBasicAi";
pub const BASIC_AI_REPLY_NAME: &str = "GrReplyBasicAiPacket";
pub const CLOSE_SLOT_REQUEST_NAME: &str = "GrRequestClosePacket";
pub const CLOSE_SLOT_REPLY_NAME: &str = "GrReplyClosePacket";
pub const RIDER_TALK_REQUEST_NAME: &str = "GrRiderTalkPacket";
pub const RIDER_ECHO_NAME: &str = "GrRiderEchoPacket";
pub const MACRO_CHAT_REQUEST_NAME: &str = "PqSendMacroChat";
pub const MACRO_CHAT_RELAY_NAME: &str = "PcSendMacroChat";
pub const CHANGE_ROOM_INFO_REQUEST_NAME: &str = "PqChangeRoomInfoPacket";
pub const CHANGE_ROOM_INFO_REPLY_NAME: &str = "PrChangeRoomInfoPacket";

pub const ROOM_OBSERVER_ID_END: i32 = 15;
pub const MAX_ROOM_CHAT_UTF16_UNITS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LobbyRequest {
    SetSlotState,
    ChangeTeam,
    ChangeMaster,
    StartRoom,
    ChangeTrack,
    BasicAi,
    CloseSlot,
    RiderTalk,
    MacroChat,
    ChangeRoomInfo,
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
pub struct ChangeTrackRequest {
    pub track: u32,
    pub room_data_header: u32,
    pub room_data: [u8; ROOM_DATA_LENGTH],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BasicAiRequest {
    pub player_id: u32,
    /// The reference server reads this byte but treats the presence of the
    /// requested AI ID as the authoritative add/remove decision.
    pub option: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseSlotOperation {
    Open,
    Close,
}

impl CloseSlotOperation {
    fn from_wire(value: u8) -> Result<Self, LobbyProtocolError> {
        match value {
            0 => Ok(Self::Open),
            1 => Ok(Self::Close),
            _ => Err(LobbyProtocolError::InvalidCloseSlotOperation(value)),
        }
    }

    #[must_use]
    pub const fn is_close(self) -> bool {
        matches!(self, Self::Close)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloseSlotRequest {
    pub first_member_id: u32,
    pub operation: CloseSlotOperation,
    pub first_slot_id: u32,
    pub second_member_id: u32,
    pub second_slot_id: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiderTalkRequest {
    pub message: String,
    pub reserved: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroChatRequest {
    pub chat_type: i32,
    pub message_id: u8,
    pub client_message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeRoomInfoRequest {
    pub room_name: String,
    pub password: String,
    pub limit_time: i32,
    pub r_key_allowed: u8,
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

    #[error("close-slot operation {0} is not open (0) or close (1)")]
    InvalidCloseSlotOperation(u8),

    #[error("room slot ID {0} is outside 0..7")]
    InvalidRoomSlotId(u8),

    #[error("room slot ID {0} appears more than once")]
    DuplicateRoomSlotId(u8),

    #[error("basic-AI update count {actual} exceeds the P5136 maximum {maximum}")]
    BasicAiCountLimit { actual: usize, maximum: usize },

    #[error("room chat has {actual} UTF-16 units; maximum is {maximum}")]
    ChatTooLong { actual: usize, maximum: usize },

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
        (CHANGE_TRACK_REQUEST_NAME, LobbyRequest::ChangeTrack),
        (BASIC_AI_REQUEST_NAME, LobbyRequest::BasicAi),
        (CLOSE_SLOT_REQUEST_NAME, LobbyRequest::CloseSlot),
        (RIDER_TALK_REQUEST_NAME, LobbyRequest::RiderTalk),
        (MACRO_CHAT_REQUEST_NAME, LobbyRequest::MacroChat),
        (CHANGE_ROOM_INFO_REQUEST_NAME, LobbyRequest::ChangeRoomInfo),
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

pub fn parse_change_track_request(packet: &[u8]) -> Result<ChangeTrackRequest, LobbyProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, CHANGE_TRACK_REQUEST_NAME)?;
    let track = reader.read_u32()?;
    let room_data_header = reader.read_u32()?;
    let room_data = reader
        .read_bytes(ROOM_DATA_LENGTH)?
        .try_into()
        .expect("the exact room-data byte count was read");
    ensure_exhausted(&reader, CHANGE_TRACK_REQUEST_NAME)?;
    Ok(ChangeTrackRequest {
        track,
        room_data_header,
        room_data,
    })
}

pub fn parse_basic_ai_request(packet: &[u8]) -> Result<BasicAiRequest, LobbyProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, BASIC_AI_REQUEST_NAME)?;
    let request = BasicAiRequest {
        player_id: reader.read_u32()?,
        option: reader.read_u8()?,
    };
    ensure_exhausted(&reader, BASIC_AI_REQUEST_NAME)?;
    Ok(request)
}

pub fn parse_close_slot_request(packet: &[u8]) -> Result<CloseSlotRequest, LobbyProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, CLOSE_SLOT_REQUEST_NAME)?;
    let first_member_id = reader.read_u32()?;
    let operation = CloseSlotOperation::from_wire(reader.read_u8()?)?;
    let first_slot_id = reader.read_u32()?;
    let second_member_id = reader.read_u32()?;
    let second_slot_id = reader.read_u32()?;
    ensure_exhausted(&reader, CLOSE_SLOT_REQUEST_NAME)?;
    Ok(CloseSlotRequest {
        first_member_id,
        operation,
        first_slot_id,
        second_member_id,
        second_slot_id,
    })
}

pub fn parse_rider_talk_request(packet: &[u8]) -> Result<RiderTalkRequest, LobbyProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, RIDER_TALK_REQUEST_NAME)?;
    let request = RiderTalkRequest {
        message: reader.read_utf16_bounded(MAX_ROOM_CHAT_UTF16_UNITS)?,
        reserved: reader.read_u32()?,
    };
    ensure_exhausted(&reader, RIDER_TALK_REQUEST_NAME)?;
    Ok(request)
}

pub fn parse_macro_chat_request(packet: &[u8]) -> Result<MacroChatRequest, LobbyProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, MACRO_CHAT_REQUEST_NAME)?;
    let request = MacroChatRequest {
        chat_type: reader.read_i32()?,
        message_id: reader.read_u8()?,
        client_message: reader.read_utf16_bounded(MAX_ROOM_CHAT_UTF16_UNITS)?,
    };
    ensure_exhausted(&reader, MACRO_CHAT_REQUEST_NAME)?;
    Ok(request)
}

pub fn parse_change_room_info_request(
    packet: &[u8],
) -> Result<ChangeRoomInfoRequest, LobbyProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, CHANGE_ROOM_INFO_REQUEST_NAME)?;
    let request = ChangeRoomInfoRequest {
        room_name: reader.read_utf16_bounded(MAX_ROOM_NAME_UTF16_UNITS)?,
        password: reader.read_utf16_bounded(MAX_ROOM_PASSWORD_UTF16_UNITS)?,
        limit_time: reader.read_i32()?,
        r_key_allowed: reader.read_u8()?,
    };
    ensure_exhausted(&reader, CHANGE_ROOM_INFO_REQUEST_NAME)?;
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

pub fn serialize_basic_ai_added(
    ais: &[(i32, RoomAi)],
    slot_positions: [i32; ROOM_SLOT_COUNT],
) -> Result<Vec<u8>, LobbyProtocolError> {
    const MAX_BASIC_AI_UPDATE_COUNT: usize = 2;
    if ais.is_empty() || ais.len() > MAX_BASIC_AI_UPDATE_COUNT {
        return Err(LobbyProtocolError::BasicAiCountLimit {
            actual: ais.len(),
            maximum: MAX_BASIC_AI_UPDATE_COUNT,
        });
    }
    validate_slot_positions(slot_positions)?;
    let mut packet = PacketWriter::named(BASIC_AI_SLOT_DATA_NAME);
    packet.write_i32(0);
    packet
        .write_u8(u8::try_from(ais.len()).expect("the validated basic-AI update count fits in u8"));
    for &(player_id, ai) in ais {
        validate_racer_player_id(player_id)?;
        packet.write_i32(player_id);
        write_room_ai_body(&mut packet, ai);
    }
    write_slot_positions(&mut packet, slot_positions);
    Ok(packet.into_inner())
}

pub fn serialize_basic_ai_removed(
    player_id: i32,
    slot_positions: [i32; ROOM_SLOT_COUNT],
) -> Result<Vec<u8>, LobbyProtocolError> {
    validate_racer_player_id(player_id)?;
    validate_slot_positions(slot_positions)?;
    let mut packet = PacketWriter::named(BASIC_AI_SLOT_DATA_NAME);
    packet.write_i32(1);
    packet.write_u8(1);
    packet.write_i32(player_id);
    packet.write_bytes(&[0; 13]);
    write_slot_positions(&mut packet, slot_positions);
    Ok(packet.into_inner())
}

#[must_use]
pub fn serialize_basic_ai_reply(changed: bool) -> Vec<u8> {
    let mut packet = PacketWriter::named(BASIC_AI_REPLY_NAME);
    packet.write_u8(u8::from(changed));
    packet.into_inner()
}

pub fn serialize_close_slot_reply(
    user_no: u32,
    accepted: bool,
    first_member_id: u32,
    second_member_id: u32,
    operation: CloseSlotOperation,
    closed_slot_ids: &[u8],
) -> Result<Vec<u8>, LobbyProtocolError> {
    validate_closed_slot_ids(closed_slot_ids)?;
    let mut packet = PacketWriter::named(CLOSE_SLOT_REPLY_NAME);
    packet.write_u32(user_no);
    packet.write_u8(u8::from(accepted));
    packet.write_u32(first_member_id);
    packet.write_u32(second_member_id);
    packet.write_i32(i32::from(operation.is_close() && accepted));
    packet.write_i32(
        i32::try_from(closed_slot_ids.len()).expect("the validated closed-slot count fits in i32"),
    );
    packet.write_bytes(closed_slot_ids);
    Ok(packet.into_inner())
}

pub fn serialize_rider_echo(player_id: i32, message: &str) -> Result<Vec<u8>, LobbyProtocolError> {
    validate_player_id(player_id)?;
    validate_chat(message)?;
    let mut packet = PacketWriter::named(RIDER_ECHO_NAME);
    packet.write_i32(player_id);
    packet.write_utf16(message)?;
    Ok(packet.into_inner())
}

pub fn serialize_macro_chat_relay(
    user_no: u32,
    chat_type: i32,
    message_id: u8,
    message: &str,
) -> Result<Vec<u8>, LobbyProtocolError> {
    validate_chat(message)?;
    let mut packet = PacketWriter::named(MACRO_CHAT_RELAY_NAME);
    packet.write_u32(user_no);
    packet.write_i32(chat_type);
    packet.write_u8(message_id);
    packet.write_utf16(message)?;
    Ok(packet.into_inner())
}

pub fn serialize_change_room_info_reply(
    request: &ChangeRoomInfoRequest,
) -> Result<Vec<u8>, LobbyProtocolError> {
    let room_name_units = request.room_name.encode_utf16().count();
    if room_name_units > MAX_ROOM_NAME_UTF16_UNITS {
        return Err(PacketError::StringLimitExceeded {
            length: room_name_units,
            maximum: MAX_ROOM_NAME_UTF16_UNITS,
        }
        .into());
    }
    let password_units = request.password.encode_utf16().count();
    if password_units > MAX_ROOM_PASSWORD_UTF16_UNITS {
        return Err(PacketError::StringLimitExceeded {
            length: password_units,
            maximum: MAX_ROOM_PASSWORD_UTF16_UNITS,
        }
        .into());
    }
    let mut packet = PacketWriter::named(CHANGE_ROOM_INFO_REPLY_NAME);
    packet.write_u8(1);
    packet.write_utf16(&request.room_name)?;
    packet.write_utf16(&request.password)?;
    packet.write_i32(request.limit_time);
    packet.write_u8(request.r_key_allowed);
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

fn validate_racer_player_id(player_id: i32) -> Result<(), LobbyProtocolError> {
    if (0..i32::try_from(ROOM_SLOT_COUNT).expect("room slot count fits in i32"))
        .contains(&player_id)
    {
        Ok(())
    } else {
        Err(LobbyProtocolError::InvalidPlayerId(player_id))
    }
}

fn validate_closed_slot_ids(slots: &[u8]) -> Result<(), LobbyProtocolError> {
    let mut seen = [false; ROOM_SLOT_COUNT];
    for &slot in slots {
        let index = usize::from(slot);
        if index >= ROOM_SLOT_COUNT {
            return Err(LobbyProtocolError::InvalidRoomSlotId(slot));
        }
        if seen[index] {
            return Err(LobbyProtocolError::DuplicateRoomSlotId(slot));
        }
        seen[index] = true;
    }
    Ok(())
}

fn validate_chat(message: &str) -> Result<(), LobbyProtocolError> {
    let actual = message.encode_utf16().count();
    if actual <= MAX_ROOM_CHAT_UTF16_UNITS {
        Ok(())
    } else {
        Err(LobbyProtocolError::ChatTooLong {
            actual,
            maximum: MAX_ROOM_CHAT_UTF16_UNITS,
        })
    }
}

fn write_room_ai_body(packet: &mut PacketWriter, ai: RoomAi) {
    packet.write_i16(ai.character);
    packet.write_i16(ai.rider);
    packet.write_i16(ai.kart);
    packet.write_i16(ai.balloon);
    packet.write_i16(ai.head_band);
    packet.write_i16(ai.goggle);
    packet.write_u8(ai.team);
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
        BASIC_AI_REQUEST_NAME, CHANGE_MASTER_REQUEST_NAME, CHANGE_ROOM_INFO_REPLY_NAME,
        CHANGE_ROOM_INFO_REQUEST_NAME, CHANGE_TEAM_REQUEST_NAME, CHANGE_TRACK_REQUEST_NAME,
        CLOSE_SLOT_REQUEST_NAME, CloseSlotOperation, LobbyProtocolError, LobbyRequest,
        MACRO_CHAT_REQUEST_NAME, PlayerSlotState, RIDER_TALK_REQUEST_NAME, RoomTeam,
        SET_SLOT_STATE_REQUEST_NAME, START_ROOM_REQUEST_NAME, StartRoomStatus,
        classify_lobby_request, parse_basic_ai_request, parse_change_master_request,
        parse_change_room_info_request, parse_change_team_request, parse_change_track_request,
        parse_close_slot_request, parse_macro_chat_request, parse_rider_talk_request,
        parse_set_slot_state_request, parse_start_room_request, serialize_basic_ai_added,
        serialize_basic_ai_removed, serialize_basic_ai_reply, serialize_change_room_info_reply,
        serialize_change_team_reply, serialize_close_slot_reply, serialize_macro_chat_relay,
        serialize_rider_echo, serialize_set_slot_state_reply, serialize_slot_state,
        serialize_start_room_reply,
    };
    use crate::{adler32, packet::PacketWriter, room_protocol::RoomAi};

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
            (
                CHANGE_TRACK_REQUEST_NAME,
                0x4734_074C,
                LobbyRequest::ChangeTrack,
            ),
            (BASIC_AI_REQUEST_NAME, 0x619A_0886, LobbyRequest::BasicAi),
            (
                CLOSE_SLOT_REQUEST_NAME,
                0x525E_07F0,
                LobbyRequest::CloseSlot,
            ),
            (
                RIDER_TALK_REQUEST_NAME,
                0x39E7_0693,
                LobbyRequest::RiderTalk,
            ),
            (
                MACRO_CHAT_REQUEST_NAME,
                0x2D36_05BD,
                LobbyRequest::MacroChat,
            ),
            (
                CHANGE_ROOM_INFO_REQUEST_NAME,
                0x6057_0888,
                LobbyRequest::ChangeRoomInfo,
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
    fn captured_room_info_changes_match_the_p5136_codec_and_reply_order() {
        let request_packet = decode_hex(concat!(
            "88085760",
            "11000000",
            "4B002F0041002F0052002F0054002F0052002F0049002F0044002F00200053003200",
            "00000000",
            "65007200",
            "00"
        ));
        let request = parse_change_room_info_request(&request_packet).unwrap();
        assert_eq!(request.room_name, "K/A/R/T/R/I/D/ S2");
        assert_eq!(request.password, "");
        assert_eq!(request.limit_time, 0x0072_0065);
        assert_eq!(request.r_key_allowed, 0);

        let reply = serialize_change_room_info_reply(&request).unwrap();
        assert_eq!(
            adler32::packet_hash(CHANGE_ROOM_INFO_REPLY_NAME),
            0x606C_0889
        );
        assert_eq!(reply[0..4], 0x606C_0889_u32.to_le_bytes());
        assert_eq!(reply[4], 1);
        assert_eq!(&reply[5..], &request_packet[4..]);
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
    fn captured_room_control_requests_parse_as_typed_values() {
        let track = parse_change_track_request(&decode_hex(concat!(
            "4C073447",
            "2303E622",
            "00000000",
            "B2A9564AD2A5DFCE0AA1265359F3FDD7",
            "EF980E5C97E3A59540E0F6640D0D0DE6"
        )))
        .unwrap();
        assert_eq!(track.track, 0x22E6_0323);
        assert_eq!(track.room_data_header, 0);
        assert_eq!(track.room_data[0], 0xB2);
        assert_eq!(track.room_data[31], 0xE6);

        let ai = parse_basic_ai_request(&decode_hex("86089A610100000000")).unwrap();
        assert_eq!(ai.player_id, 1);
        assert_eq!(ai.option, 0);

        let close =
            parse_close_slot_request(&decode_hex("F0075E52040000000104000000FFFFFFFF00000000"))
                .unwrap();
        assert_eq!(close.first_member_id, 4);
        assert_eq!(close.operation, CloseSlotOperation::Close);
        assert_eq!(close.first_slot_id, 4);
        assert_eq!(close.second_member_id, u32::MAX);
        assert_eq!(close.second_slot_id, 0);

        let talk = parse_rider_talk_request(&decode_hex("9306E73901000000410000000000")).unwrap();
        assert_eq!(talk.message, "A");
        assert_eq!(talk.reserved, 0);

        let macro_chat =
            parse_macro_chat_request(&decode_hex("BD05362D000000000100000000")).unwrap();
        assert_eq!(macro_chat.chat_type, 0);
        assert_eq!(macro_chat.message_id, 1);
        assert!(macro_chat.client_message.is_empty());
    }

    #[test]
    fn room_control_replies_match_csharp_field_order() {
        let ai = RoomAi {
            character: 1,
            rider: 2,
            kart: 3,
            balloon: 4,
            head_band: 5,
            goggle: 6,
            team: 0,
        };
        let positions = [0, 1, -1, -1, -1, -1, -1, -1];
        assert_eq!(
            serialize_basic_ai_added(&[(1, ai)], positions).unwrap(),
            decode_hex(concat!(
                "61068C39",
                "0000000001",
                "01000000",
                "01000200030004000500060000",
                "0000000001000000FFFFFFFFFFFFFFFF",
                "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF"
            ))
        );
        assert_eq!(
            serialize_basic_ai_removed(1, [0, -1, -1, -1, -1, -1, -1, -1]).unwrap(),
            decode_hex(concat!(
                "61068C39",
                "0100000001",
                "01000000",
                "00000000000000000000000000",
                "00000000FFFFFFFFFFFFFFFFFFFFFFFF",
                "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF"
            ))
        );
        assert_eq!(serialize_basic_ai_reply(true), decode_hex("A907904F01"));
        assert_eq!(
            serialize_close_slot_reply(6, true, 4, 4, CloseSlotOperation::Close, &[4],).unwrap(),
            decode_hex("13070E4206000000010400000004000000010000000100000004")
        );
        assert_eq!(
            serialize_rider_echo(0, "A").unwrap(),
            decode_hex("86065F3900000000010000004100")
        );
        assert_eq!(
            serialize_rider_echo(8, "A").unwrap(),
            decode_hex("86065F3908000000010000004100")
        );
        assert_eq!(
            serialize_macro_chat_relay(10, 0, 1, "").unwrap(),
            decode_hex("AF05722C0A000000000000000100000000")
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
        assert!(matches!(
            serialize_rider_echo(16, "invalid"),
            Err(LobbyProtocolError::InvalidPlayerId(16))
        ));

        let mut invalid_close = PacketWriter::named(CLOSE_SLOT_REQUEST_NAME);
        invalid_close.write_u32(0);
        invalid_close.write_u8(2);
        invalid_close.write_u32(0);
        invalid_close.write_u32(0);
        invalid_close.write_u32(0);
        assert!(matches!(
            parse_close_slot_request(invalid_close.as_slice()),
            Err(LobbyProtocolError::InvalidCloseSlotOperation(2))
        ));

        assert!(matches!(
            serialize_close_slot_reply(1, true, 0, 0, CloseSlotOperation::Close, &[2, 2]),
            Err(LobbyProtocolError::DuplicateRoomSlotId(2))
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
