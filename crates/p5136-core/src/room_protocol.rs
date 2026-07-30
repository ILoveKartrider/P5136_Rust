//! P5136 room-list and initial room-entry packet codecs.
//!
//! This flow begins after `PrChannelMoveIn`. The stock client can request a
//! room page, create or join a room, and then sends `GrFirstRequestPacket`.
//! That final request is paired with `GrSessionDataPacket` followed by
//! `GrSlotDataPacket`. Ready, team-change, and race packets are intentionally
//! outside this module.

use std::{array, net::Ipv4Addr};

use thiserror::Error;

use crate::{
    adler32,
    packet::{PacketError, PacketReader, PacketWriter},
    startup::RIDER_ITEM_SNAPSHOT_WIRE_LENGTH,
};

pub const MAX_ROOM_LIST_PAGE: i32 = 100_000;
pub const MAX_ROOM_LIST_ENTRIES: usize = 10;
pub const MAX_ROOM_NAME_UTF16_UNITS: usize = 128;
pub const MAX_ROOM_PASSWORD_UTF16_UNITS: usize = 64;
pub const MAX_RIDER_NICKNAME_UTF16_UNITS: usize = 32;
pub const MAX_RIDER_CARD_UTF16_UNITS: usize = 128;
pub const MAX_CLUB_NAME_UTF16_UNITS: usize = 64;
pub const MAX_ROOM_AI_COUNT: i32 = 8;
pub const ROOM_DATA_LENGTH: usize = 32;
pub const ROOM_CONNECTION_CONTEXT_LENGTH: usize = 28;
pub const ROOM_SLOT_COUNT: usize = 8;
pub const ROOM_OBSERVER_COUNT: usize = 8;

const ROOM_LIST_REPLY_NAMES: &[&str] = &["ChGetRoomListReplyPacket"];
const CREATE_ROOM_REPLY_NAMES: &[&str] = &["ChCreateRoomReplyPacket"];
const JOIN_ROOM_REPLY_NAMES: &[&str] = &["ChJoinRoomReplyPacket"];
const LEAVE_ROOM_REPLY_NAMES: &[&str] = &["ChLeaveRoomReplyPacket"];
const FIRST_ROOM_STATE_REPLY_NAMES: &[&str] = &["GrSessionDataPacket", "GrSlotDataPacket"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomProtocolRequest {
    RoomList,
    CreateRoom,
    JoinRoom,
    LeaveRoom,
    FirstRoomState,
}

pub const ROOM_PROTOCOL_REQUESTS: &[RoomProtocolRequest] = &[
    RoomProtocolRequest::RoomList,
    RoomProtocolRequest::CreateRoom,
    RoomProtocolRequest::JoinRoom,
    RoomProtocolRequest::LeaveRoom,
    RoomProtocolRequest::FirstRoomState,
];

impl RoomProtocolRequest {
    #[must_use]
    pub const fn request_name(self) -> &'static str {
        match self {
            Self::RoomList => "ChGetRoomListRequestPacket",
            Self::CreateRoom => "ChCreateRoomRequestPacket",
            Self::JoinRoom => "ChJoinRoomRequestPacket",
            Self::LeaveRoom => "ChLeaveRoomRequestPacket",
            Self::FirstRoomState => "GrFirstRequestPacket",
        }
    }

    #[must_use]
    pub const fn reply_names(self) -> &'static [&'static str] {
        match self {
            Self::RoomList => ROOM_LIST_REPLY_NAMES,
            Self::CreateRoom => CREATE_ROOM_REPLY_NAMES,
            Self::JoinRoom => JOIN_ROOM_REPLY_NAMES,
            Self::LeaveRoom => LEAVE_ROOM_REPLY_NAMES,
            Self::FirstRoomState => FIRST_ROOM_STATE_REPLY_NAMES,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChGetRoomListRequest {
    pub page: i32,
    pub room_list_type: u8,
    pub room_list_mode: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomListEntry {
    pub room_id: i16,
    pub room_name: String,
    pub track: u32,
    pub locked: bool,
    pub game_type: u8,
    pub speed_type: u8,
    pub started: bool,
    pub available_slots: u8,
    pub player_count: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChCreateRoomRequest {
    pub room_name: String,
    pub password: String,
    pub game_type: u8,
    pub reserved_after_game_type: i32,
    pub ai_count: i32,
    pub room_data_header: u32,
    pub room_data: [u8; ROOM_DATA_LENGTH],
    pub connection_context: [u8; ROOM_CONNECTION_CONTEXT_LENGTH],
    pub reserved_before_ai_switch: u8,
    pub ai_switch: i32,
    pub reserved_after_ai_switch_1: u8,
    pub reserved_after_ai_switch_2: u8,
    pub reserved_tail: i32,
    pub reserved_last: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateRoomOutcome {
    Rejected,
    Created,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChJoinRoomRequest {
    pub room_id: u16,
    pub password: String,
    pub reserved: u8,
    pub connection_context: [u8; ROOM_CONNECTION_CONTEXT_LENGTH],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinRoomStatus {
    Success,
    Unavailable,
    Full,
    WrongPassword,
}

impl JoinRoomStatus {
    const fn code(self) -> u8 {
        match self {
            Self::Success => 0,
            Self::Unavailable => 1,
            Self::Full => 2,
            Self::WrongPassword => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChLeaveRoomRequest {
    pub reserved: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomSessionData {
    pub room_name: String,
    pub password: String,
    pub game_type: u8,
    pub speed_type: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomPlayer {
    pub player_type: i32,
    pub user_no: u32,
    pub p2p_address: Ipv4Addr,
    pub p2p_port: u16,
    pub nickname: String,
    pub emblem_1: u16,
    pub emblem_2: u16,
    pub emblem_3: u16,
    pub rider_item_snapshot: [u8; RIDER_ITEM_SNAPSHOT_WIRE_LENGTH],
    pub card: String,
    pub rp: u32,
    pub team: u8,
    pub ranking: i32,
    pub rider_school_level: u8,
    pub club_name: String,
    pub club_mark_logo: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoomAi {
    pub character: i16,
    pub rider: i16,
    pub kart: i16,
    pub balloon: i16,
    pub head_band: i16,
    pub goggle: i16,
    pub team: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RoomMember {
    Player(RoomPlayer),
    Ai(RoomAi),
    Closed {
        player_type: i32,
    },
    #[default]
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomObserver {
    pub player_type: i32,
    pub user_no: u32,
    pub p2p_address: Ipv4Addr,
    pub p2p_port: u16,
    pub nickname: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RoomObserverSlot {
    Player(RoomObserver),
    #[default]
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomSlotData {
    pub track: u32,
    pub room_data_header: u32,
    pub room_data: [u8; ROOM_DATA_LENGTH],
    pub room_master: i32,
    pub closed_slot_ids: Vec<u8>,
    pub members_by_id: [RoomMember; ROOM_SLOT_COUNT],
    pub observers: [RoomObserverSlot; ROOM_OBSERVER_COUNT],
    pub slot_positions: [i32; ROOM_SLOT_COUNT],
}

impl RoomSlotData {
    #[must_use]
    pub fn empty(
        track: u32,
        room_data_header: u32,
        room_data: [u8; ROOM_DATA_LENGTH],
        room_master: i32,
    ) -> Self {
        Self {
            track,
            room_data_header,
            room_data,
            room_master,
            closed_slot_ids: Vec::new(),
            members_by_id: array::from_fn(|_| RoomMember::Empty),
            observers: array::from_fn(|_| RoomObserverSlot::Empty),
            slot_positions: [-1; ROOM_SLOT_COUNT],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitialRoomStatePacketKind {
    SessionData,
    SlotData,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitialRoomStatePacket {
    pub kind: InitialRoomStatePacketKind,
    pub logical_packet: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum RoomProtocolError {
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

    #[error("room-list page {page} is outside 0..={maximum}")]
    InvalidPage { page: i32, maximum: i32 },

    #[error("AI count {count} is outside 0..={maximum}")]
    InvalidAiCount { count: i32, maximum: i32 },

    #[error("{field} length/count {actual} exceeds configured maximum {maximum}")]
    LimitExceeded {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },

    #[error("closed room slot ID {0} is outside 0..7")]
    InvalidClosedSlot(u8),

    #[error("closed room slot ID {0} appears more than once")]
    DuplicateClosedSlot(u8),

    #[error("room slot position {position} at index {index} is outside -1..=7")]
    InvalidSlotPosition { index: usize, position: i32 },

    #[error("{field} count overflowed the P5136 i32 field")]
    CountOverflow { field: &'static str },
}

#[must_use]
pub fn classify_room_protocol_request(hash: u32) -> Option<RoomProtocolRequest> {
    ROOM_PROTOCOL_REQUESTS
        .iter()
        .copied()
        .find(|request| adler32::packet_hash(request.request_name()) == hash)
}

pub fn parse_ch_get_room_list_request(
    packet: &[u8],
) -> Result<ChGetRoomListRequest, RoomProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, "ChGetRoomListRequestPacket")?;
    let page = reader.read_i32()?;
    validate_page(page)?;
    let request = ChGetRoomListRequest {
        page,
        room_list_type: reader.read_u8()?,
        room_list_mode: reader.read_u8()?,
    };
    ensure_exhausted(&reader, "ChGetRoomListRequestPacket")?;
    Ok(request)
}

pub fn parse_ch_create_room_request(
    packet: &[u8],
) -> Result<ChCreateRoomRequest, RoomProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, "ChCreateRoomRequestPacket")?;
    let room_name = reader.read_utf16_bounded(MAX_ROOM_NAME_UTF16_UNITS)?;
    let password = reader.read_utf16_bounded(MAX_ROOM_PASSWORD_UTF16_UNITS)?;
    let game_type = reader.read_encoded_u8()?;
    let reserved_after_game_type = reader.read_i32()?;
    let ai_count = reader.read_i32()?;
    if !(0..=MAX_ROOM_AI_COUNT).contains(&ai_count) {
        return Err(RoomProtocolError::InvalidAiCount {
            count: ai_count,
            maximum: MAX_ROOM_AI_COUNT,
        });
    }
    let room_data_header = reader.read_u32()?;
    let room_data = read_fixed(&mut reader)?;
    let connection_context = read_fixed(&mut reader)?;
    let request = ChCreateRoomRequest {
        room_name,
        password,
        game_type,
        reserved_after_game_type,
        ai_count,
        room_data_header,
        room_data,
        connection_context,
        reserved_before_ai_switch: reader.read_u8()?,
        ai_switch: reader.read_i32()?,
        reserved_after_ai_switch_1: reader.read_u8()?,
        reserved_after_ai_switch_2: reader.read_u8()?,
        reserved_tail: reader.read_i32()?,
        reserved_last: reader.read_u8()?,
    };
    ensure_exhausted(&reader, "ChCreateRoomRequestPacket")?;
    Ok(request)
}

pub fn parse_ch_join_room_request(packet: &[u8]) -> Result<ChJoinRoomRequest, RoomProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, "ChJoinRoomRequestPacket")?;
    let request = ChJoinRoomRequest {
        room_id: reader.read_u16()?,
        password: reader.read_utf16_bounded(MAX_ROOM_PASSWORD_UTF16_UNITS)?,
        reserved: reader.read_u8()?,
        connection_context: read_fixed(&mut reader)?,
    };
    ensure_exhausted(&reader, "ChJoinRoomRequestPacket")?;
    Ok(request)
}

pub fn parse_ch_leave_room_request(packet: &[u8]) -> Result<ChLeaveRoomRequest, RoomProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, "ChLeaveRoomRequestPacket")?;
    let request = ChLeaveRoomRequest {
        reserved: reader.read_u8()?,
    };
    ensure_exhausted(&reader, "ChLeaveRoomRequestPacket")?;
    Ok(request)
}

pub fn parse_gr_first_request(packet: &[u8]) -> Result<(), RoomProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, "GrFirstRequestPacket")?;
    ensure_exhausted(&reader, "GrFirstRequestPacket")
}

pub fn serialize_ch_get_room_list_reply(
    page: i32,
    rooms: &[RoomListEntry],
) -> Result<Vec<u8>, RoomProtocolError> {
    validate_page(page)?;
    enforce_limit("room-list entry", rooms.len(), MAX_ROOM_LIST_ENTRIES)?;
    for room in rooms {
        validate_utf16("room name", &room.room_name, MAX_ROOM_NAME_UTF16_UNITS)?;
        enforce_limit(
            "available room slot",
            usize::from(room.available_slots),
            ROOM_SLOT_COUNT,
        )?;
        enforce_limit(
            "room player",
            usize::from(room.player_count),
            ROOM_SLOT_COUNT,
        )?;
    }

    let mut packet = PacketWriter::named("ChGetRoomListReplyPacket");
    packet.write_i32(page);
    packet.write_i32(count_i32("room-list entry", rooms.len())?);
    for room in rooms {
        write_i16(&mut packet, room.room_id);
        packet.write_utf16(&room.room_name)?;
        packet.write_u32(room.track);
        packet.write_u8(u8::from(room.locked));
        packet.write_u8(room.game_type);
        packet.write_u8(room.speed_type);
        packet.write_u8(u8::from(room.started));
        packet.write_u8(room.available_slots);
        packet.write_u8(room.player_count);
        packet.write_bytes(&[0; 2]);
    }
    Ok(packet.into_inner())
}

#[must_use]
pub fn serialize_ch_create_room_reply(outcome: CreateRoomOutcome, game_type: u8) -> Vec<u8> {
    let created = matches!(outcome, CreateRoomOutcome::Created);
    let mut packet = PacketWriter::named("ChCreateRoomReplyPacket");
    packet.write_u8(u8::from(created));
    packet.write_u8(u8::from(created));
    packet.write_u8(if created {
        room_slot_hint(game_type)
    } else {
        0
    });
    packet.write_encoded_u8(game_type);
    packet.into_inner()
}

#[must_use]
pub fn serialize_ch_join_room_reply(status: JoinRoomStatus, game_type: u8) -> Vec<u8> {
    let success = status == JoinRoomStatus::Success;
    let mut packet = PacketWriter::named("ChJoinRoomReplyPacket");
    packet.write_u8(status.code());
    packet.write_u8(u8::from(success));
    packet.write_u8(if success {
        room_slot_hint(game_type)
    } else {
        0
    });
    packet.write_encoded_u8(game_type);
    packet.write_u8(0);
    packet.into_inner()
}

#[must_use]
pub fn serialize_ch_leave_room_reply(left: bool) -> Vec<u8> {
    let mut packet = PacketWriter::named("ChLeaveRoomReplyPacket");
    packet.write_u8(u8::from(left));
    packet.into_inner()
}

pub fn serialize_gr_session_data(session: &RoomSessionData) -> Result<Vec<u8>, RoomProtocolError> {
    validate_utf16("room name", &session.room_name, MAX_ROOM_NAME_UTF16_UNITS)?;
    validate_utf16(
        "room password",
        &session.password,
        MAX_ROOM_PASSWORD_UTF16_UNITS,
    )?;

    let mut packet = PacketWriter::named("GrSessionDataPacket");
    packet.write_utf16(&session.room_name)?;
    packet.write_utf16(&session.password)?;
    packet.write_u8(session.game_type);
    packet.write_u8(session.speed_type);
    packet.write_i32(0);
    packet.write_u8(0);
    packet.write_i32(0);
    packet.write_bytes(&[0; 6]);
    Ok(packet.into_inner())
}

pub fn serialize_gr_slot_data(slots: &RoomSlotData) -> Result<Vec<u8>, RoomProtocolError> {
    validate_slot_data(slots)?;

    let mut packet = PacketWriter::named("GrSlotDataPacket");
    packet.write_u32(slots.track);
    packet.write_u32(slots.room_data_header);
    packet.write_bytes(&slots.room_data);
    packet.write_i32(slots.room_master);
    packet.write_bytes(&[0; 11]);
    packet.write_i32(count_i32("closed room slot", slots.closed_slot_ids.len())?);
    packet.write_bytes(&slots.closed_slot_ids);
    packet.write_bytes(&[0; 16]);

    for member in &slots.members_by_id {
        match member {
            RoomMember::Player(player) => write_room_player(&mut packet, player)?,
            RoomMember::Ai(ai) => write_room_ai(&mut packet, ai),
            RoomMember::Closed { player_type } => packet.write_i32(*player_type),
            RoomMember::Empty => packet.write_i32(0),
        }
    }

    for observer in &slots.observers {
        match observer {
            RoomObserverSlot::Player(player) => write_room_observer(&mut packet, player)?,
            RoomObserverSlot::Empty => packet.write_i32(0),
        }
    }

    for position in slots.slot_positions {
        packet.write_i32(position);
    }
    Ok(packet.into_inner())
}

/// Builds the exact two-packet response to `GrFirstRequestPacket`.
///
/// The array order is significant: session data is first, slot data second.
pub fn serialize_initial_room_state(
    session: &RoomSessionData,
    slots: &RoomSlotData,
) -> Result<[InitialRoomStatePacket; 2], RoomProtocolError> {
    let session_data = serialize_gr_session_data(session)?;
    let slot_data = serialize_gr_slot_data(slots)?;
    Ok([
        InitialRoomStatePacket {
            kind: InitialRoomStatePacketKind::SessionData,
            logical_packet: session_data,
        },
        InitialRoomStatePacket {
            kind: InitialRoomStatePacketKind::SlotData,
            logical_packet: slot_data,
        },
    ])
}

fn validate_slot_data(slots: &RoomSlotData) -> Result<(), RoomProtocolError> {
    enforce_limit(
        "closed room slot",
        slots.closed_slot_ids.len(),
        ROOM_SLOT_COUNT,
    )?;
    let mut seen_closed_slots = [false; ROOM_SLOT_COUNT];
    for &slot in &slots.closed_slot_ids {
        let index = usize::from(slot);
        if index >= ROOM_SLOT_COUNT {
            return Err(RoomProtocolError::InvalidClosedSlot(slot));
        }
        if seen_closed_slots[index] {
            return Err(RoomProtocolError::DuplicateClosedSlot(slot));
        }
        seen_closed_slots[index] = true;
    }

    for member in &slots.members_by_id {
        if let RoomMember::Player(player) = member {
            validate_room_player(player)?;
        }
    }
    for observer in &slots.observers {
        if let RoomObserverSlot::Player(player) = observer {
            validate_utf16(
                "observer nickname",
                &player.nickname,
                MAX_RIDER_NICKNAME_UTF16_UNITS,
            )?;
        }
    }
    for (index, &position) in slots.slot_positions.iter().enumerate() {
        if !(-1..=7).contains(&position) {
            return Err(RoomProtocolError::InvalidSlotPosition { index, position });
        }
    }
    Ok(())
}

fn validate_room_player(player: &RoomPlayer) -> Result<(), RoomProtocolError> {
    validate_utf16(
        "rider nickname",
        &player.nickname,
        MAX_RIDER_NICKNAME_UTF16_UNITS,
    )?;
    validate_utf16("rider card", &player.card, MAX_RIDER_CARD_UTF16_UNITS)?;
    if player.club_mark_logo != 0 {
        validate_utf16("club name", &player.club_name, MAX_CLUB_NAME_UTF16_UNITS)?;
    }
    Ok(())
}

fn write_room_player(
    packet: &mut PacketWriter,
    player: &RoomPlayer,
) -> Result<(), RoomProtocolError> {
    packet.write_i32(player.player_type);
    packet.write_u32(player.user_no);
    write_endpoint(packet, player.p2p_address, player.p2p_port);
    write_endpoint(packet, Ipv4Addr::UNSPECIFIED, 0);
    packet.write_utf16(&player.nickname)?;
    packet.write_u16(player.emblem_1);
    packet.write_u16(player.emblem_2);
    packet.write_u16(player.emblem_3);
    packet.write_bytes(&player.rider_item_snapshot);
    packet.write_utf16(&player.card)?;
    packet.write_u32(player.rp);
    packet.write_u8(player.team);
    packet.write_i32(player.ranking);
    packet.write_bytes(&[0; 30]);
    packet.write_i32(1_500);
    packet.write_i32(1_499);
    packet.write_i32(0);
    packet.write_i32(2_000);
    packet.write_i32(5);
    packet.write_bytes(&[0xff, 0, 0, 0]);
    packet.write_u8(player.rider_school_level);
    if player.club_mark_logo == 0 {
        packet.write_utf16("")?;
        packet.write_i32(0);
    } else {
        packet.write_utf16(&player.club_name)?;
        packet.write_i32(player.club_mark_logo);
    }
    packet.write_bytes(&[0; 17]);
    Ok(())
}

fn write_room_ai(packet: &mut PacketWriter, ai: &RoomAi) {
    packet.write_i32(7);
    write_i16(packet, ai.character);
    write_i16(packet, ai.rider);
    write_i16(packet, ai.kart);
    write_i16(packet, ai.balloon);
    write_i16(packet, ai.head_band);
    write_i16(packet, ai.goggle);
    packet.write_u8(ai.team);
}

fn write_room_observer(
    packet: &mut PacketWriter,
    observer: &RoomObserver,
) -> Result<(), RoomProtocolError> {
    packet.write_i32(observer.player_type);
    packet.write_u32(observer.user_no);
    write_endpoint(packet, observer.p2p_address, observer.p2p_port);
    write_endpoint(packet, Ipv4Addr::UNSPECIFIED, 0);
    packet.write_utf16(&observer.nickname)?;
    Ok(())
}

fn expect_hash(reader: &mut PacketReader<'_>, name: &'static str) -> Result<(), RoomProtocolError> {
    let actual = reader.read_u32()?;
    let expected = adler32::packet_hash(name);
    if actual == expected {
        Ok(())
    } else {
        Err(RoomProtocolError::UnexpectedPacketHash {
            name,
            expected,
            actual,
        })
    }
}

fn ensure_exhausted(
    reader: &PacketReader<'_>,
    name: &'static str,
) -> Result<(), RoomProtocolError> {
    let count = reader.remaining().len();
    if count == 0 {
        Ok(())
    } else {
        Err(RoomProtocolError::TrailingBytes { name, count })
    }
}

fn read_fixed<const N: usize>(reader: &mut PacketReader<'_>) -> Result<[u8; N], PacketError> {
    let mut value = [0; N];
    value.copy_from_slice(reader.read_bytes(N)?);
    Ok(value)
}

fn validate_page(page: i32) -> Result<(), RoomProtocolError> {
    if (0..=MAX_ROOM_LIST_PAGE).contains(&page) {
        Ok(())
    } else {
        Err(RoomProtocolError::InvalidPage {
            page,
            maximum: MAX_ROOM_LIST_PAGE,
        })
    }
}

fn validate_utf16(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), RoomProtocolError> {
    let actual = value.encode_utf16().count();
    enforce_limit(field, actual, maximum)
}

fn enforce_limit(
    field: &'static str,
    actual: usize,
    maximum: usize,
) -> Result<(), RoomProtocolError> {
    if actual > maximum {
        Err(RoomProtocolError::LimitExceeded {
            field,
            actual,
            maximum,
        })
    } else {
        Ok(())
    }
}

fn count_i32(field: &'static str, count: usize) -> Result<i32, RoomProtocolError> {
    i32::try_from(count).map_err(|_| RoomProtocolError::CountOverflow { field })
}

const fn room_slot_hint(game_type: u8) -> u8 {
    if matches!(game_type, 3 | 4) { 2 } else { 8 }
}

fn write_endpoint(packet: &mut PacketWriter, address: Ipv4Addr, port: u16) {
    packet.write_bytes(&address.octets());
    packet.write_u16(port);
}

fn write_i16(packet: &mut PacketWriter, value: i16) {
    packet.write_bytes(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use sha2::{Digest, Sha256};

    use super::{
        ChCreateRoomRequest, CreateRoomOutcome, InitialRoomStatePacketKind, JoinRoomStatus,
        MAX_ROOM_AI_COUNT, MAX_ROOM_LIST_ENTRIES, MAX_ROOM_NAME_UTF16_UNITS,
        RIDER_ITEM_SNAPSHOT_WIRE_LENGTH, ROOM_DATA_LENGTH, RoomAi, RoomListEntry, RoomMember,
        RoomObserver, RoomObserverSlot, RoomPlayer, RoomProtocolError, RoomProtocolRequest,
        RoomSessionData, RoomSlotData, classify_room_protocol_request,
        parse_ch_create_room_request, parse_ch_get_room_list_request, parse_ch_join_room_request,
        parse_ch_leave_room_request, parse_gr_first_request, serialize_ch_create_room_reply,
        serialize_ch_get_room_list_reply, serialize_ch_join_room_reply,
        serialize_ch_leave_room_reply, serialize_gr_session_data, serialize_gr_slot_data,
        serialize_initial_room_state,
    };
    use crate::{
        adler32,
        packet::{PacketError, PacketReader},
    };

    #[test]
    fn dispatch_table_uses_the_exact_csharp_packet_names_and_hashes() {
        let fixtures = [
            (RoomProtocolRequest::RoomList, 2_266_696_261),
            (RoomProtocolRequest::CreateRoom, 2_096_892_381),
            (RoomProtocolRequest::JoinRoom, 1_787_234_585),
            (RoomProtocolRequest::LeaveRoom, 1_932_396_918),
            (RoomProtocolRequest::FirstRoomState, 1_385_170_946),
        ];
        for (request, expected_hash) in fixtures {
            assert_eq!(adler32::packet_hash(request.request_name()), expected_hash);
            assert_eq!(classify_room_protocol_request(expected_hash), Some(request));
        }
        assert_eq!(
            RoomProtocolRequest::FirstRoomState.reply_names(),
            ["GrSessionDataPacket", "GrSlotDataPacket"]
        );
        assert_eq!(classify_room_protocol_request(0xdead_beef), None);
    }

    #[test]
    fn parses_the_csharp_room_list_create_join_leave_and_first_requests() {
        let room_list =
            parse_ch_get_room_list_request(&decode_hex("450A1B87020000000107")).unwrap();
        assert_eq!(room_list.page, 2);
        assert_eq!(room_list.room_list_type, 1);
        assert_eq!(room_list.room_list_mode, 7);

        let create = parse_ch_create_room_request(&decode_hex(
            "DD09FC7C0200000029BC41000200000070007700894030201002000000DDCCBBAA\
             000102030405060708090A0B0C0D0E0F101112131415161718191A1B1C1D1E1F\
             202122232425262728292A2B2C2D2E2F303132333435363738393A3B6106000000\
             62634433221164",
        ))
        .unwrap();
        assert_eq!(
            create,
            ChCreateRoomRequest {
                room_name: "방A".to_owned(),
                password: "pw".to_owned(),
                game_type: 3,
                reserved_after_game_type: 0x1020_3040,
                ai_count: 2,
                room_data_header: 0xaabb_ccdd,
                room_data: array_from_range::<32>(0),
                connection_context: array_from_range::<28>(32),
                reserved_before_ai_switch: 0x61,
                ai_switch: 6,
                reserved_after_ai_switch_1: 0x62,
                reserved_after_ai_switch_2: 0x63,
                reserved_tail: 0x1122_3344,
                reserved_last: 0x64,
            }
        );

        let join = parse_ch_join_room_request(&decode_hex(
            "1909876A341202000000700077005A000102030405060708090A0B0C0D0E0F101112\
             131415161718191A1B",
        ))
        .unwrap();
        assert_eq!(join.room_id, 0x1234);
        assert_eq!(join.password, "pw");
        assert_eq!(join.reserved, 0x5a);
        assert_eq!(join.connection_context, array_from_range::<28>(0));

        let leave = parse_ch_leave_room_request(&decode_hex("76092E7301")).unwrap();
        assert_eq!(leave.reserved, 1);
        parse_gr_first_request(&decode_hex("02089052")).unwrap();
    }

    #[test]
    fn request_parsers_reject_bounds_truncation_and_trailing_data() {
        let mut oversized_name = decode_hex(
            "DD09FC7C0200000029BC41000200000070007700894030201002000000DDCCBBAA\
             000102030405060708090A0B0C0D0E0F101112131415161718191A1B1C1D1E1F\
             202122232425262728292A2B2C2D2E2F303132333435363738393A3B6106000000\
             62634433221164",
        );
        let oversized_name_units =
            i32::try_from(MAX_ROOM_NAME_UTF16_UNITS + 1).expect("test limit fits in i32");
        oversized_name[4..8].copy_from_slice(&oversized_name_units.to_le_bytes());
        assert!(matches!(
            parse_ch_create_room_request(&oversized_name),
            Err(RoomProtocolError::Packet(
                PacketError::StringLimitExceeded { .. }
            ))
        ));

        let mut invalid_ai = decode_hex(
            "DD09FC7C0200000029BC41000200000070007700894030201002000000DDCCBBAA\
             000102030405060708090A0B0C0D0E0F101112131415161718191A1B1C1D1E1F\
             202122232425262728292A2B2C2D2E2F303132333435363738393A3B6106000000\
             62634433221164",
        );
        invalid_ai[25..29].copy_from_slice(&(MAX_ROOM_AI_COUNT + 1).to_le_bytes());
        assert!(matches!(
            parse_ch_create_room_request(&invalid_ai),
            Err(RoomProtocolError::InvalidAiCount { count: 9, .. })
        ));

        let mut trailing = decode_hex("450A1B87020000000107");
        trailing.push(0);
        assert!(matches!(
            parse_ch_get_room_list_request(&trailing),
            Err(RoomProtocolError::TrailingBytes { count: 1, .. })
        ));
        assert!(matches!(
            parse_ch_join_room_request(&decode_hex("1909876A3412")),
            Err(RoomProtocolError::Packet(PacketError::Truncated { .. }))
        ));
    }

    #[test]
    fn room_list_reply_matches_the_csharp_p5136_golden() {
        let rooms = [
            RoomListEntry {
                room_id: 1,
                room_name: "방A".to_owned(),
                track: 0x1020_3040,
                locked: true,
                game_type: 1,
                speed_type: 7,
                started: false,
                available_slots: 8,
                player_count: 1,
            },
            RoomListEntry {
                room_id: 2,
                room_name: "Room2".to_owned(),
                track: 0xaabb_ccdd,
                locked: false,
                game_type: 3,
                speed_type: 5,
                started: true,
                available_slots: 2,
                player_count: 6,
            },
        ];
        let packet = serialize_ch_get_room_list_reply(2, &rooms).unwrap();
        assert_packet_golden(
            &packet,
            62,
            "1D52E7871B2DD5417242DC5695D1F35DF8DA89B620CD0E8E3164130C7AA8B1D8",
        );
        assert_eq!(
            packet,
            decode_hex(
                "68098672020000000200000001000200000029BC4100403020100101070008010000\
                 02000500000052006F006F006D003200DDCCBBAA0003050102060000"
            )
        );
    }

    #[test]
    fn create_join_and_leave_replies_match_csharp_goldens() {
        let create_cases = [
            (
                CreateRoomOutcome::Rejected,
                3,
                "3E110D33568F8EF3E93B95452C7A79E963B48A157FF106914913E024B521473E",
            ),
            (
                CreateRoomOutcome::Created,
                3,
                "D949CB9E91AB62A7B8BB396DADA70C001DF0F3FF1CDB380B25E2780A87C18408",
            ),
            (
                CreateRoomOutcome::Created,
                1,
                "95377F48DF47907B5A251F91C9D6022FD79CC75D5FE61535B278DBD59758B827",
            ),
        ];
        for (outcome, game_type, golden) in create_cases {
            assert_packet_golden(
                &serialize_ch_create_room_reply(outcome, game_type),
                8,
                golden,
            );
        }

        let join_cases = [
            (
                JoinRoomStatus::Success,
                "DCF4946388C830F8E7F13473AD758A1097AB60BF5710280E8085E14AF4B10E62",
            ),
            (
                JoinRoomStatus::Unavailable,
                "9413CF87974C0D3A7D768F92D3AD332FCD75F49C0253B0A5FBC39D136B244038",
            ),
            (
                JoinRoomStatus::Full,
                "CBD506E21AC126ACCF6A3CBCFD09D9E0E2DF7FF10234AB369095B5946C9A2A4C",
            ),
            (
                JoinRoomStatus::WrongPassword,
                "E3F3F27873CFD60521BAF323644274516F52D297477BBD8AB8F8201BD11DF835",
            ),
        ];
        for (status, golden) in join_cases {
            assert_packet_golden(&serialize_ch_join_room_reply(status, 3), 9, golden);
        }

        assert_packet_golden(
            &serialize_ch_leave_room_reply(true),
            5,
            "E39F33BE0C7C139312B1912A7811AEA38E09B72A6A05FFEAEA13FCE7695BCF86",
        );
        assert_packet_golden(
            &serialize_ch_leave_room_reply(false),
            5,
            "D21AB28E70F8ABADD2B0B11AE368F32904E88901C00D2CB4DC0272A76F08E681",
        );
    }

    #[test]
    fn first_room_state_session_and_slot_packets_match_csharp_goldens() {
        let session = fixture_session();
        let slots = fixture_slots();
        let session_packet = serialize_gr_session_data(&session).unwrap();
        assert_packet_golden(
            &session_packet,
            37,
            "0E0225AFA430523FF55C3B99E34A8CC0D50C6692BE250337398F53A605A217D7",
        );

        let slot_packet = serialize_gr_slot_data(&slots).unwrap();
        assert_packet_golden(
            &slot_packet,
            372,
            "9726B3C5B2F9F362B23626A74DEEC4BE8EFD98105F93CFF49EADDB7764357BC3",
        );

        let sequence = serialize_initial_room_state(&session, &slots).unwrap();
        assert_eq!(sequence[0].kind, InitialRoomStatePacketKind::SessionData);
        assert_eq!(sequence[1].kind, InitialRoomStatePacketKind::SlotData);
        assert_eq!(sequence[0].logical_packet, session_packet);
        assert_eq!(sequence[1].logical_packet, slot_packet);
    }

    #[test]
    fn ordinary_room_player_writes_nonzero_third_emblem_before_equipment() {
        let mut slots = fixture_slots();
        let RoomMember::Player(player) = &mut slots.members_by_id[0] else {
            panic!("fixture slot zero must contain an ordinary player");
        };
        player.emblem_3 = 0x9abc;
        let packet = serialize_gr_slot_data(&slots).unwrap();

        let mut reader = PacketReader::new(&packet);
        assert_eq!(
            reader.read_u32().unwrap(),
            adler32::packet_hash("GrSlotDataPacket")
        );
        assert_eq!(reader.read_u32().unwrap(), slots.track);
        assert_eq!(reader.read_u32().unwrap(), slots.room_data_header);
        assert_eq!(
            reader.read_bytes(ROOM_DATA_LENGTH).unwrap(),
            slots.room_data
        );
        assert_eq!(reader.read_i32().unwrap(), slots.room_master);
        reader.read_bytes(11).unwrap();
        assert_eq!(reader.read_i32().unwrap(), 1);
        assert_eq!(reader.read_bytes(1).unwrap(), &[7]);
        reader.read_bytes(16).unwrap();

        assert_eq!(reader.read_i32().unwrap(), 2);
        assert_eq!(reader.read_u32().unwrap(), 0x0102_0304);
        reader.read_bytes(6).unwrap();
        reader.read_bytes(6).unwrap();
        assert_eq!(reader.read_utf16().unwrap(), "Rider");
        assert_eq!(reader.read_u16().unwrap(), 0x1234);
        assert_eq!(reader.read_u16().unwrap(), 0x5678);
        assert_eq!(reader.read_u16().unwrap(), 0x9abc);
        assert_eq!(
            reader.read_bytes(RIDER_ITEM_SNAPSHOT_WIRE_LENGTH).unwrap(),
            array_from_range::<RIDER_ITEM_SNAPSHOT_WIRE_LENGTH>(0).as_slice()
        );
    }

    #[test]
    fn response_serializers_bound_strings_counts_and_slot_indices() {
        let entry = RoomListEntry {
            room_id: 1,
            room_name: "room".to_owned(),
            track: 0,
            locked: false,
            game_type: 1,
            speed_type: 7,
            started: false,
            available_slots: 8,
            player_count: 1,
        };
        assert!(matches!(
            serialize_ch_get_room_list_reply(0, &vec![entry.clone(); MAX_ROOM_LIST_ENTRIES + 1]),
            Err(RoomProtocolError::LimitExceeded {
                actual: 11,
                maximum: MAX_ROOM_LIST_ENTRIES,
                ..
            })
        ));

        let oversized = RoomListEntry {
            room_name: "x".repeat(MAX_ROOM_NAME_UTF16_UNITS + 1),
            ..entry
        };
        assert!(matches!(
            serialize_ch_get_room_list_reply(0, &[oversized]),
            Err(RoomProtocolError::LimitExceeded { .. })
        ));

        let mut slots = fixture_slots();
        slots.closed_slot_ids = vec![7, 7];
        assert!(matches!(
            serialize_gr_slot_data(&slots),
            Err(RoomProtocolError::DuplicateClosedSlot(7))
        ));
        slots.closed_slot_ids = vec![8];
        assert!(matches!(
            serialize_gr_slot_data(&slots),
            Err(RoomProtocolError::InvalidClosedSlot(8))
        ));
        slots.closed_slot_ids.clear();
        slots.slot_positions[7] = 8;
        assert!(matches!(
            serialize_gr_slot_data(&slots),
            Err(RoomProtocolError::InvalidSlotPosition {
                index: 7,
                position: 8
            })
        ));
    }

    #[test]
    fn slot_serializer_supports_ai_closed_and_observer_records() {
        let mut slots = RoomSlotData::empty(0, 0, [0; ROOM_DATA_LENGTH], 0);
        slots.members_by_id[0] = RoomMember::Ai(RoomAi {
            character: 1,
            rider: 2,
            kart: 3,
            balloon: 4,
            head_band: 5,
            goggle: 6,
            team: 2,
        });
        slots.members_by_id[1] = RoomMember::Closed { player_type: 1 };
        slots.observers[0] = RoomObserverSlot::Player(RoomObserver {
            player_type: 4,
            user_no: 10,
            p2p_address: Ipv4Addr::LOCALHOST,
            p2p_port: 39_312,
            nickname: "Observer".to_owned(),
        });
        slots.slot_positions[0] = 0;
        let packet = serialize_gr_slot_data(&slots).unwrap();
        assert_eq!(
            u32::from_le_bytes(packet[..4].try_into().unwrap()),
            863_766_061
        );
    }

    fn fixture_session() -> RoomSessionData {
        RoomSessionData {
            room_name: "방A".to_owned(),
            password: "pw".to_owned(),
            game_type: 3,
            speed_type: 7,
        }
    }

    fn fixture_slots() -> RoomSlotData {
        let mut slots = RoomSlotData::empty(0x1020_3040, 0xaabb_ccdd, array_from_range::<32>(0), 0);
        slots.closed_slot_ids.push(7);
        slots.members_by_id[0] = RoomMember::Player(RoomPlayer {
            player_type: 2,
            user_no: 0x0102_0304,
            p2p_address: Ipv4Addr::LOCALHOST,
            p2p_port: 39_312,
            nickname: "Rider".to_owned(),
            emblem_1: 0x1234,
            emblem_2: 0x5678,
            emblem_3: 0,
            rider_item_snapshot: array_from_range::<65>(0),
            card: "C".to_owned(),
            rp: 123_456,
            team: 2,
            ranking: 0,
            rider_school_level: 6,
            club_name: String::new(),
            club_mark_logo: 0,
        });
        slots.slot_positions[0] = 0;
        slots
    }

    fn array_from_range<const N: usize>(start: u8) -> [u8; N] {
        let mut value = [0; N];
        for (offset, byte) in value.iter_mut().enumerate() {
            *byte = start.wrapping_add(u8::try_from(offset).unwrap());
        }
        value
    }

    fn assert_packet_golden(packet: &[u8], expected_length: usize, expected_sha256: &str) {
        assert_eq!(packet.len(), expected_length);
        assert_eq!(format!("{:X}", Sha256::digest(packet)), expected_sha256);
    }

    fn decode_hex(input: &str) -> Vec<u8> {
        let compact = input
            .bytes()
            .filter(|byte| !byte.is_ascii_whitespace())
            .collect::<Vec<_>>();
        assert!(compact.len().is_multiple_of(2));
        compact
            .chunks_exact(2)
            .map(|pair| {
                let text = std::str::from_utf8(pair).unwrap();
                u8::from_str_radix(text, 16).unwrap()
            })
            .collect()
    }
}
