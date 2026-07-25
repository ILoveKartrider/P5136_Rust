//! Bounded P5136 `MyRoom` request and response codecs.
//!
//! `MyRoom` membership and fan-out belong to the server actor. This module only
//! accepts complete, structurally valid client packets and produces the
//! replies whose field order is independent of inventory/profile I/O.

use std::net::Ipv4Addr;

use thiserror::Error;

use crate::{
    adler32,
    packet::{PacketError, PacketReader, PacketWriter},
    room_protocol::{MAX_CLUB_NAME_UTF16_UNITS, MAX_RIDER_NICKNAME_UTF16_UNITS},
    startup::RIDER_ITEM_SNAPSHOT_WIRE_LENGTH,
};

pub const REENTER_MYROOM_REQUEST_NAME: &str = "ChReRqEnterMyRoomPacket";
pub const ENTER_RANDOM_MYROOM_REQUEST_NAME: &str = "ChRqEnterRandomMyRoomPacket";
pub const ENTER_MYROOM_REQUEST_NAME: &str = "ChRqEnterMyRoomPacket";
pub const ENTER_MYROOM_REPLY_NAME: &str = "ChRpEnterMyRoomPacket";
pub const FIRST_MYROOM_REQUEST_NAME: &str = "RmFirstRequestPacket";
pub const REQUEST_MYROOM_ITEMS_NAME: &str = "RmRequestItemsPacket";
pub const NOTIFY_MYROOM_INFO_NAME: &str = "RmNotiMyRoomInfoPacket";
pub const CHAR_POSITION_NAME: &str = "RmCharPosPacket";
pub const SECEDE_MYROOM_REQUEST_NAME: &str = "ChRqSecedeMyRoomPacket";
pub const SECEDE_MYROOM_REPLY_NAME: &str = "ChRpSecedeMyRoomPacket";
pub const RIDER_TALK_NAME: &str = "RmRiderTalkPacket";
pub const RIDER_ECHO_NAME: &str = "RmRiderEchoPacket";
pub const CHECK_PASSWORD_REQUEST_NAME: &str = "ChRqMyroomCheckPassEtcPacket";
pub const CHECK_PASSWORD_REPLY_NAME: &str = "ChRpMyroomCheckPassEtcPacket";
pub const REQUEST_EMBLEMS_NAME: &str = "RmRequestEmblemsPacket";
pub const OWNER_EMBLEMS_NAME: &str = "RmOwnerEmblemPacket";
pub const UPDATE_MAIN_EMBLEM_REQUEST_NAME: &str = "RmRqUpdateMainEmblemPacket";
pub const UPDATE_MAIN_EMBLEM_REPLY_NAME: &str = "RmRpUpdateMainEmblemPacket";
pub const SLOT_DATA_NAME: &str = "RmSlotDataPacket";
pub const OWNER_ITEM_ENCHANT_NAME: &str = "RmOwnerItemEnchantPacket";
pub const OWNER_ITEM_NAME: &str = "RmOwnerItemPacket";

pub const MAX_MYROOM_PASSWORD_UTF16_UNITS: usize = 64;
pub const MAX_MYROOM_TALK_UTF16_UNITS: usize = 256;
pub const MAX_MYROOM_EMBLEMS: usize = 65_535;
pub const MYROOM_ITEM_CHUNK_SIZE: usize = 26;
pub const MYROOM_SLOT_COUNT: usize = 8;
pub const MYROOM_EMPTY_SLOT_ZERO_LENGTH: usize = 122;
pub const MYROOM_EMPTY_SLOT_WIRE_LENGTH: usize = MYROOM_EMPTY_SLOT_ZERO_LENGTH + 1;
pub const MYROOM_PLAYER_RESERVED_LENGTH: usize = 29;

const _: () = assert!(RIDER_ITEM_SNAPSHOT_WIRE_LENGTH == 65);
const _: () = assert!(MYROOM_EMPTY_SLOT_WIRE_LENGTH == 123);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MyRoomRequest {
    Reenter,
    EnterRandom,
    Enter,
    FirstState,
    RequestItems,
    UpdateInfo,
    CharacterPosition,
    Secede,
    RiderTalk,
    CheckPassword,
    RequestEmblems,
    UpdateMainEmblem,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnterMyRoomRequest {
    pub owner_nickname: String,
    /// P5136 sometimes appends one unused dword.
    pub reserved: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MyRoomInfo {
    pub room_id: i16,
    pub bgm: u8,
    pub use_room_password: u8,
    pub use_item_password: u8,
    pub talk_lock: u8,
    pub room_password: String,
    pub item_password: String,
    pub kart_1: i16,
    pub kart_2: i16,
}

impl Default for MyRoomInfo {
    fn default() -> Self {
        Self {
            room_id: 0,
            bgm: 0,
            use_room_password: 0,
            use_item_password: 0,
            talk_lock: 1,
            room_password: String::new(),
            item_password: String::new(),
            kart_1: 0,
            kart_2: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CharacterPositionRequest {
    pub slot: u8,
    pub transform: [f32; 6],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiderTalkRequest {
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckPasswordRequest {
    pub password_kind: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateMainEmblemRequest {
    pub emblem_1: i16,
    pub emblem_2: i16,
}

/// The variable-length player form of one P5136 `RmSlotDataPacket` entry.
///
/// The secondary endpoint, 29 reserved bytes, and final zero byte are fixed by
/// the wire format and are therefore not caller-controlled fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MyRoomPlayerSlot {
    pub user_no: u32,
    pub p2p_address: Ipv4Addr,
    pub p2p_port: u16,
    pub nickname: String,
    pub rider_item_snapshot: [u8; RIDER_ITEM_SNAPSHOT_WIRE_LENGTH],
    pub rp: u32,
    pub club_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MyRoomSlot {
    Empty,
    Player(MyRoomPlayerSlot),
}

/// One 24-byte entry in `RmOwnerItemEnchantPacket`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MyRoomTune {
    pub item_id: i16,
    pub serial_number: i16,
    pub tune_1: i16,
    pub tune_2: i16,
    pub tune_3: i16,
    pub slot_1: i16,
    pub count_1: i16,
    pub slot_2: i16,
    pub count_2: i16,
}

/// One 18-byte kart entry in the Korean P5136 `RmOwnerItemPacket`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MyRoomKart {
    pub kart_id: u16,
    pub serial_number: u16,
}

/// One 40-byte legacy parts entry in the Korean P5136
/// `RmOwnerItemPacket`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MyRoomParts {
    pub item_id: i16,
    pub serial_number: i16,
    pub engine: i16,
    pub engine_grade: u8,
    pub engine_value: i16,
    pub handle: i16,
    pub handle_grade: u8,
    pub handle_value: i16,
    pub wheel: i16,
    pub wheel_grade: u8,
    pub wheel_value: i16,
    pub booster: i16,
    pub booster_grade: u8,
    pub booster_value: i16,
    pub coating: i16,
    pub tail_lamp: i16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EnterMyRoomStatus {
    Success = 0,
    Full = 1,
    OwnerUnavailable = 3,
    NoAvailableRoom = 5,
}

#[derive(Debug, Error)]
pub enum MyRoomProtocolError {
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

    #[error("MyRoom slot {0} is outside 0..=7")]
    InvalidSlot(i32),

    #[error("MyRoom transform element {index} is not finite")]
    NonFiniteTransform { index: usize },

    #[error("{field} has {actual} UTF-16 units; maximum is {maximum}")]
    StringTooLong {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },

    #[error("MyRoom emblem list has {actual} entries; maximum is {maximum}")]
    TooManyEmblems { actual: usize, maximum: usize },

    #[error("MyRoom slot data has {actual} slots; expected exactly {expected}")]
    InvalidSlotCount { actual: usize, expected: usize },

    #[error("MyRoom {field} collection has {actual} entries and cannot fit its wire counters")]
    ItemCollectionTooLarge { field: &'static str, actual: usize },
}

#[must_use]
pub fn classify_myroom_request(hash: u32) -> Option<MyRoomRequest> {
    [
        (REENTER_MYROOM_REQUEST_NAME, MyRoomRequest::Reenter),
        (ENTER_RANDOM_MYROOM_REQUEST_NAME, MyRoomRequest::EnterRandom),
        (ENTER_MYROOM_REQUEST_NAME, MyRoomRequest::Enter),
        (FIRST_MYROOM_REQUEST_NAME, MyRoomRequest::FirstState),
        (REQUEST_MYROOM_ITEMS_NAME, MyRoomRequest::RequestItems),
        (NOTIFY_MYROOM_INFO_NAME, MyRoomRequest::UpdateInfo),
        (CHAR_POSITION_NAME, MyRoomRequest::CharacterPosition),
        (SECEDE_MYROOM_REQUEST_NAME, MyRoomRequest::Secede),
        (RIDER_TALK_NAME, MyRoomRequest::RiderTalk),
        (CHECK_PASSWORD_REQUEST_NAME, MyRoomRequest::CheckPassword),
        (REQUEST_EMBLEMS_NAME, MyRoomRequest::RequestEmblems),
        (
            UPDATE_MAIN_EMBLEM_REQUEST_NAME,
            MyRoomRequest::UpdateMainEmblem,
        ),
    ]
    .into_iter()
    .find_map(|(name, request)| (adler32::packet_hash(name) == hash).then_some(request))
}

pub fn parse_reenter_request(packet: &[u8]) -> Result<(), MyRoomProtocolError> {
    parse_empty_request(packet, REENTER_MYROOM_REQUEST_NAME)
}

pub fn parse_enter_random_request(packet: &[u8]) -> Result<(), MyRoomProtocolError> {
    parse_empty_request(packet, ENTER_RANDOM_MYROOM_REQUEST_NAME)
}

pub fn parse_enter_request(packet: &[u8]) -> Result<EnterMyRoomRequest, MyRoomProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, ENTER_MYROOM_REQUEST_NAME)?;
    let owner_nickname = reader.read_utf16_bounded(MAX_RIDER_NICKNAME_UTF16_UNITS)?;
    let reserved = match reader.remaining().len() {
        0 => None,
        4 => Some(reader.read_i32()?),
        _ => {
            return Err(MyRoomProtocolError::TrailingBytes {
                name: ENTER_MYROOM_REQUEST_NAME,
                count: reader.remaining().len(),
            });
        }
    };
    ensure_exhausted(&reader, ENTER_MYROOM_REQUEST_NAME)?;
    Ok(EnterMyRoomRequest {
        owner_nickname,
        reserved,
    })
}

pub fn parse_first_request(packet: &[u8]) -> Result<(), MyRoomProtocolError> {
    parse_empty_request(packet, FIRST_MYROOM_REQUEST_NAME)
}

pub fn parse_request_items(packet: &[u8]) -> Result<(), MyRoomProtocolError> {
    parse_empty_request(packet, REQUEST_MYROOM_ITEMS_NAME)
}

pub fn parse_update_info(packet: &[u8]) -> Result<MyRoomInfo, MyRoomProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, NOTIFY_MYROOM_INFO_NAME)?;
    let info = read_myroom_info(&mut reader)?;
    ensure_exhausted(&reader, NOTIFY_MYROOM_INFO_NAME)?;
    Ok(info)
}

pub fn parse_character_position(
    packet: &[u8],
) -> Result<CharacterPositionRequest, MyRoomProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, CHAR_POSITION_NAME)?;
    let slot = reader.read_i32()?;
    let mut transform = [0.0; 6];
    for value in &mut transform {
        *value = reader.read_f32()?;
    }
    ensure_exhausted(&reader, CHAR_POSITION_NAME)?;
    let slot = validate_slot(slot)?;
    validate_transform(transform)?;
    Ok(CharacterPositionRequest { slot, transform })
}

pub fn parse_secede_request(packet: &[u8]) -> Result<(), MyRoomProtocolError> {
    parse_empty_request(packet, SECEDE_MYROOM_REQUEST_NAME)
}

pub fn parse_rider_talk(packet: &[u8]) -> Result<RiderTalkRequest, MyRoomProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, RIDER_TALK_NAME)?;
    let message = reader.read_utf16_bounded(MAX_MYROOM_TALK_UTF16_UNITS)?;
    ensure_exhausted(&reader, RIDER_TALK_NAME)?;
    Ok(RiderTalkRequest { message })
}

pub fn parse_check_password(packet: &[u8]) -> Result<CheckPasswordRequest, MyRoomProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, CHECK_PASSWORD_REQUEST_NAME)?;
    let request = CheckPasswordRequest {
        password_kind: reader.read_i32()?,
    };
    ensure_exhausted(&reader, CHECK_PASSWORD_REQUEST_NAME)?;
    Ok(request)
}

pub fn parse_request_emblems(packet: &[u8]) -> Result<(), MyRoomProtocolError> {
    parse_empty_request(packet, REQUEST_EMBLEMS_NAME)
}

pub fn parse_update_main_emblem(
    packet: &[u8],
) -> Result<UpdateMainEmblemRequest, MyRoomProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, UPDATE_MAIN_EMBLEM_REQUEST_NAME)?;
    let request = UpdateMainEmblemRequest {
        emblem_1: reader.read_i16()?,
        emblem_2: reader.read_i16()?,
    };
    ensure_exhausted(&reader, UPDATE_MAIN_EMBLEM_REQUEST_NAME)?;
    Ok(request)
}

pub fn serialize_enter_reply(
    owner_nickname: &str,
    status: EnterMyRoomStatus,
    info: &MyRoomInfo,
) -> Result<Vec<u8>, MyRoomProtocolError> {
    validate_string(
        "MyRoom owner nickname",
        owner_nickname,
        MAX_RIDER_NICKNAME_UTF16_UNITS,
    )?;
    validate_myroom_info(info)?;
    let mut packet = PacketWriter::named(ENTER_MYROOM_REPLY_NAME);
    packet.write_utf16(owner_nickname)?;
    packet.write_u8(status as u8);
    write_myroom_info(&mut packet, info)?;
    Ok(packet.into_inner())
}

pub fn serialize_enter_error(status: EnterMyRoomStatus) -> Result<Vec<u8>, MyRoomProtocolError> {
    serialize_enter_reply("", status, &MyRoomInfo::default())
}

pub fn serialize_myroom_info(info: &MyRoomInfo) -> Result<Vec<u8>, MyRoomProtocolError> {
    validate_myroom_info(info)?;
    let mut packet = PacketWriter::named(NOTIFY_MYROOM_INFO_NAME);
    write_myroom_info(&mut packet, info)?;
    Ok(packet.into_inner())
}

pub fn serialize_character_position(
    slot: i32,
    transform: [f32; 6],
) -> Result<Vec<u8>, MyRoomProtocolError> {
    let slot = validate_slot(slot)?;
    validate_transform(transform)?;
    let mut packet = PacketWriter::named(CHAR_POSITION_NAME);
    packet.write_i32(i32::from(slot));
    for value in transform {
        packet.write_f32(value);
    }
    Ok(packet.into_inner())
}

pub fn serialize_rider_echo(slot: i32, message: &str) -> Result<Vec<u8>, MyRoomProtocolError> {
    let slot = validate_slot(slot)?;
    validate_string("MyRoom talk message", message, MAX_MYROOM_TALK_UTF16_UNITS)?;
    let mut packet = PacketWriter::named(RIDER_ECHO_NAME);
    packet.write_i32(i32::from(slot));
    packet.write_utf16(message)?;
    Ok(packet.into_inner())
}

#[must_use]
pub fn serialize_secede_reply() -> Vec<u8> {
    let mut packet = PacketWriter::named(SECEDE_MYROOM_REPLY_NAME);
    packet.write_u8(1);
    packet.into_inner()
}

#[must_use]
pub fn serialize_check_password_reply(password_kind: i32) -> Vec<u8> {
    let mut packet = PacketWriter::named(CHECK_PASSWORD_REPLY_NAME);
    packet.write_i32(password_kind);
    packet.write_i32(i32::from(password_kind == 0));
    packet.into_inner()
}

pub fn serialize_owner_emblems(emblems: &[i16]) -> Result<Vec<u8>, MyRoomProtocolError> {
    if emblems.len() > MAX_MYROOM_EMBLEMS {
        return Err(MyRoomProtocolError::TooManyEmblems {
            actual: emblems.len(),
            maximum: MAX_MYROOM_EMBLEMS,
        });
    }
    let count = i32::try_from(emblems.len()).map_err(|_| MyRoomProtocolError::TooManyEmblems {
        actual: emblems.len(),
        maximum: MAX_MYROOM_EMBLEMS,
    })?;
    let mut packet = PacketWriter::named(OWNER_EMBLEMS_NAME);
    packet.write_i32(1);
    packet.write_i32(1);
    packet.write_i32(count);
    for emblem in emblems {
        packet.write_i16(*emblem);
    }
    Ok(packet.into_inner())
}

/// Serializes the exact Korean P5136 eight-slot `MyRoom` snapshot.
///
/// Empty entries are 122 zero bytes followed by `0xFF`. Player entries mirror
/// `MyRoom.WritePlayerSlot`: user number, primary IPv4 endpoint, a zero
/// secondary endpoint, nickname, the 65-byte rider snapshot, RP, 29 zero bytes,
/// club name, and one trailing zero byte.
pub fn serialize_slot_data(slots: &[MyRoomSlot]) -> Result<Vec<u8>, MyRoomProtocolError> {
    if slots.len() != MYROOM_SLOT_COUNT {
        return Err(MyRoomProtocolError::InvalidSlotCount {
            actual: slots.len(),
            expected: MYROOM_SLOT_COUNT,
        });
    }
    for slot in slots {
        if let MyRoomSlot::Player(player) = slot {
            validate_myroom_player_slot(player)?;
        }
    }

    let mut packet = PacketWriter::named(SLOT_DATA_NAME);
    for slot in slots {
        match slot {
            MyRoomSlot::Empty => write_empty_slot(&mut packet),
            MyRoomSlot::Player(player) => write_player_slot(&mut packet, player)?,
        }
    }
    Ok(packet.into_inner())
}

/// Serializes every non-empty 26-entry enchant chunk.
///
/// The original owner-present path emits no packet at all for an empty tune
/// list. The owner-missing response is a distinct, explicit zero-count packet
/// produced by [`serialize_missing_owner_items`].
pub fn serialize_owner_item_enchants(
    tunes: &[MyRoomTune],
) -> Result<Vec<Vec<u8>>, MyRoomProtocolError> {
    checked_collection_len("tune", tunes.len())?;
    let mut packets = Vec::with_capacity(tunes.len().div_ceil(MYROOM_ITEM_CHUNK_SIZE));
    for chunk in tunes.chunks(MYROOM_ITEM_CHUNK_SIZE) {
        let mut packet = PacketWriter::named(OWNER_ITEM_ENCHANT_NAME);
        packet.write_i32(wire_count("tune chunk", chunk.len())?);
        for tune in chunk {
            packet.write_i16(3);
            packet.write_i16(tune.item_id);
            packet.write_i16(tune.serial_number);
            packet.write_i16(0);
            packet.write_i16(0);
            packet.write_i16(tune.tune_1);
            packet.write_i16(tune.tune_2);
            packet.write_i16(tune.tune_3);
            packet.write_i16(tune.slot_1);
            packet.write_i16(tune.count_1);
            packet.write_i16(tune.slot_2);
            packet.write_i16(tune.count_2);
        }
        packets.push(packet.into_inner());
    }
    Ok(packets)
}

/// Serializes the Korean P5136 owner kart/parts stream.
///
/// Packet ordinals are global across both item types while the first two
/// counters are local to their type. P5136 deliberately excludes the later
/// `Parts12` form. It also preserves the original server's early-return quirk:
/// when no kart exists, one 28-byte empty body is emitted and every parts
/// entry is omitted.
pub fn serialize_owner_items(
    karts: &[MyRoomKart],
    parts: &[MyRoomParts],
    prevent_item: bool,
) -> Result<Vec<Vec<u8>>, MyRoomProtocolError> {
    checked_collection_len("kart", karts.len())?;
    checked_collection_len("parts", parts.len())?;

    if karts.is_empty() {
        let mut packet = PacketWriter::named(OWNER_ITEM_NAME);
        packet.write_i32(1);
        packet.write_i32(1);
        packet.write_i32(0);
        packet.write_bytes(&[0; 8]);
        packet.write_i32(1);
        packet.write_i32(1);
        return Ok(vec![packet.into_inner()]);
    }

    let kart_chunk_count = karts.len().div_ceil(MYROOM_ITEM_CHUNK_SIZE);
    let parts_chunk_count = parts.len().div_ceil(MYROOM_ITEM_CHUNK_SIZE);
    let total_chunk_count = kart_chunk_count.checked_add(parts_chunk_count).ok_or(
        MyRoomProtocolError::ItemCollectionTooLarge {
            field: "owner item packet",
            actual: karts.len().saturating_add(parts.len()),
        },
    )?;
    let all_count = wire_count("owner item packet", total_chunk_count)?;
    let mut packets = Vec::with_capacity(total_chunk_count);
    let kart_chunks = wire_chunk_count("kart", karts.len())?;
    let parts_chunks = wire_chunk_count("parts", parts.len())?;

    for (index, chunk) in karts.chunks(MYROOM_ITEM_CHUNK_SIZE).enumerate() {
        let mut packet = PacketWriter::named(OWNER_ITEM_NAME);
        packet.write_i32(kart_chunks);
        packet.write_i32(wire_index("kart", index)?);
        packet.write_i32(wire_count("kart chunk", chunk.len())?);
        for kart in chunk {
            packet.write_u16(3);
            packet.write_u16(kart.kart_id);
            packet.write_u16(kart.serial_number);
            packet.write_u16(1);
            packet.write_u8(u8::from(prevent_item));
            packet.write_u8(0);
            packet.write_i16(-1);
            packet.write_i16(0);
            packet.write_u8(0);
            packet.write_u8(0);
            packet.write_i16(0);
        }
        packet.write_bytes(&[0; 8]);
        packet.write_i32(all_count);
        packet.write_i32(wire_index("owner item packet", index)?);
        packets.push(packet.into_inner());
    }

    for (index, chunk) in parts.chunks(MYROOM_ITEM_CHUNK_SIZE).enumerate() {
        let global_index = kart_chunk_count.checked_add(index).ok_or(
            MyRoomProtocolError::ItemCollectionTooLarge {
                field: "owner item packet",
                actual: karts.len().saturating_add(parts.len()),
            },
        )?;
        let mut packet = PacketWriter::named(OWNER_ITEM_NAME);
        packet.write_i32(parts_chunks);
        packet.write_i32(wire_index("parts", index)?);
        packet.write_i32(0);
        packet.write_i32(0);
        packet.write_i32(wire_count("parts chunk", chunk.len())?);
        for part in chunk {
            write_owner_part(&mut packet, part);
        }
        packet.write_i32(all_count);
        packet.write_i32(wire_index("owner item packet", global_index)?);
        packets.push(packet.into_inner());
    }
    Ok(packets)
}

/// Produces the sole response used when the requested `MyRoom` owner no longer
/// exists. No `RmOwnerItemPacket` follows this packet.
#[must_use]
pub fn serialize_missing_owner_items() -> Vec<u8> {
    let mut packet = PacketWriter::named(OWNER_ITEM_ENCHANT_NAME);
    packet.write_i32(0);
    packet.into_inner()
}

#[must_use]
pub fn serialize_update_main_emblem_reply(success: bool) -> Vec<u8> {
    let mut packet = PacketWriter::named(UPDATE_MAIN_EMBLEM_REPLY_NAME);
    packet.write_u8(u8::from(success));
    packet.write_u8(0);
    packet.into_inner()
}

fn write_empty_slot(packet: &mut PacketWriter) {
    packet.write_bytes(&[0; MYROOM_EMPTY_SLOT_ZERO_LENGTH]);
    packet.write_u8(0xff);
}

fn write_player_slot(
    packet: &mut PacketWriter,
    player: &MyRoomPlayerSlot,
) -> Result<(), MyRoomProtocolError> {
    packet.write_u32(player.user_no);
    write_endpoint(packet, player.p2p_address, player.p2p_port);
    write_endpoint(packet, Ipv4Addr::UNSPECIFIED, 0);
    packet.write_utf16(&player.nickname)?;
    packet.write_bytes(&player.rider_item_snapshot);
    packet.write_u32(player.rp);
    packet.write_bytes(&[0; MYROOM_PLAYER_RESERVED_LENGTH]);
    packet.write_utf16(&player.club_name)?;
    packet.write_u8(0);
    Ok(())
}

fn write_endpoint(packet: &mut PacketWriter, address: Ipv4Addr, port: u16) {
    packet.write_bytes(&address.octets());
    packet.write_u16(port);
}

fn write_owner_part(packet: &mut PacketWriter, part: &MyRoomParts) {
    packet.write_i16(part.item_id);
    packet.write_i16(part.serial_number);
    packet.write_i16(0);
    packet.write_i16(-1);
    packet.write_i16(0);
    packet.write_i16(part.engine);
    packet.write_u8(part.engine_grade);
    packet.write_i16(part.engine_value);
    packet.write_i16(part.handle);
    packet.write_u8(part.handle_grade);
    packet.write_i16(part.handle_value);
    packet.write_i16(part.wheel);
    packet.write_u8(part.wheel_grade);
    packet.write_i16(part.wheel_value);
    packet.write_i16(part.booster);
    packet.write_u8(part.booster_grade);
    packet.write_i16(part.booster_value);
    packet.write_i16(part.coating);
    packet.write_u8(0);
    packet.write_i16(0);
    packet.write_i16(part.tail_lamp);
    packet.write_u8(0);
    packet.write_i16(0);
}

fn checked_collection_len(field: &'static str, len: usize) -> Result<(), MyRoomProtocolError> {
    wire_chunk_count(field, len).map(|_| ())
}

fn wire_chunk_count(field: &'static str, len: usize) -> Result<i32, MyRoomProtocolError> {
    i32::try_from(len.div_ceil(MYROOM_ITEM_CHUNK_SIZE))
        .map_err(|_| MyRoomProtocolError::ItemCollectionTooLarge { field, actual: len })
}

fn wire_count(field: &'static str, len: usize) -> Result<i32, MyRoomProtocolError> {
    i32::try_from(len)
        .map_err(|_| MyRoomProtocolError::ItemCollectionTooLarge { field, actual: len })
}

fn wire_index(field: &'static str, zero_based_index: usize) -> Result<i32, MyRoomProtocolError> {
    zero_based_index
        .checked_add(1)
        .and_then(|index| i32::try_from(index).ok())
        .ok_or(MyRoomProtocolError::ItemCollectionTooLarge {
            field,
            actual: zero_based_index,
        })
}

fn parse_empty_request(packet: &[u8], name: &'static str) -> Result<(), MyRoomProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, name)?;
    ensure_exhausted(&reader, name)
}

fn read_myroom_info(reader: &mut PacketReader<'_>) -> Result<MyRoomInfo, MyRoomProtocolError> {
    let room_id = reader.read_i16()?;
    let bgm = reader.read_u8()?;
    let use_room_password = reader.read_u8()?;
    let _reserved_flag = reader.read_u8()?;
    let use_item_password = reader.read_u8()?;
    let talk_lock = reader.read_u8()?;
    let room_password = reader.read_utf16_bounded(MAX_MYROOM_PASSWORD_UTF16_UNITS)?;
    let _reserved_password = reader.read_utf16_bounded(MAX_MYROOM_PASSWORD_UTF16_UNITS)?;
    let item_password = reader.read_utf16_bounded(MAX_MYROOM_PASSWORD_UTF16_UNITS)?;
    let kart_1 = reader.read_i16()?;
    let kart_2 = reader.read_i16()?;
    Ok(MyRoomInfo {
        room_id,
        bgm,
        use_room_password,
        use_item_password,
        talk_lock,
        room_password,
        item_password,
        kart_1,
        kart_2,
    })
}

fn write_myroom_info(
    packet: &mut PacketWriter,
    info: &MyRoomInfo,
) -> Result<(), MyRoomProtocolError> {
    packet.write_i16(info.room_id);
    packet.write_u8(info.bgm);
    packet.write_u8(info.use_room_password);
    packet.write_u8(0);
    packet.write_u8(info.use_item_password);
    packet.write_u8(info.talk_lock);
    packet.write_utf16(&info.room_password)?;
    packet.write_utf16("")?;
    packet.write_utf16(&info.item_password)?;
    packet.write_i16(info.kart_1);
    packet.write_i16(info.kart_2);
    Ok(())
}

/// Validates every variable-length field in a `MyRoom` info snapshot without
/// allocating or serializing it.
pub fn validate_myroom_info(info: &MyRoomInfo) -> Result<(), MyRoomProtocolError> {
    validate_string(
        "MyRoom room password",
        &info.room_password,
        MAX_MYROOM_PASSWORD_UTF16_UNITS,
    )?;
    validate_string(
        "MyRoom item password",
        &info.item_password,
        MAX_MYROOM_PASSWORD_UTF16_UNITS,
    )
}

/// Validates every variable-length field in a `MyRoom` player snapshot without
/// allocating or serializing it.
pub fn validate_myroom_player_slot(player: &MyRoomPlayerSlot) -> Result<(), MyRoomProtocolError> {
    validate_string(
        "MyRoom rider nickname",
        &player.nickname,
        MAX_RIDER_NICKNAME_UTF16_UNITS,
    )?;
    validate_string(
        "MyRoom club name",
        &player.club_name,
        MAX_CLUB_NAME_UTF16_UNITS,
    )
}

fn validate_string(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), MyRoomProtocolError> {
    let actual = value.encode_utf16().count();
    if actual > maximum {
        Err(MyRoomProtocolError::StringTooLong {
            field,
            actual,
            maximum,
        })
    } else {
        Ok(())
    }
}

fn validate_slot(slot: i32) -> Result<u8, MyRoomProtocolError> {
    u8::try_from(slot)
        .ok()
        .filter(|slot| usize::from(*slot) < MYROOM_SLOT_COUNT)
        .ok_or(MyRoomProtocolError::InvalidSlot(slot))
}

fn validate_transform(transform: [f32; 6]) -> Result<(), MyRoomProtocolError> {
    for (index, value) in transform.into_iter().enumerate() {
        if !value.is_finite() {
            return Err(MyRoomProtocolError::NonFiniteTransform { index });
        }
    }
    Ok(())
}

fn expect_hash(
    reader: &mut PacketReader<'_>,
    name: &'static str,
) -> Result<(), MyRoomProtocolError> {
    let actual = reader.read_u32()?;
    let expected = adler32::packet_hash(name);
    if actual == expected {
        Ok(())
    } else {
        Err(MyRoomProtocolError::UnexpectedPacketHash {
            name,
            expected,
            actual,
        })
    }
}

fn ensure_exhausted(
    reader: &PacketReader<'_>,
    name: &'static str,
) -> Result<(), MyRoomProtocolError> {
    if reader.remaining().is_empty() {
        Ok(())
    } else {
        Err(MyRoomProtocolError::TrailingBytes {
            name,
            count: reader.remaining().len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use sha2::{Digest, Sha256};

    use super::{
        CHAR_POSITION_NAME, CHECK_PASSWORD_REQUEST_NAME, ENTER_MYROOM_REQUEST_NAME,
        ENTER_RANDOM_MYROOM_REQUEST_NAME, EnterMyRoomStatus, FIRST_MYROOM_REQUEST_NAME,
        MAX_MYROOM_EMBLEMS, MAX_MYROOM_PASSWORD_UTF16_UNITS, MAX_MYROOM_TALK_UTF16_UNITS,
        MYROOM_EMPTY_SLOT_WIRE_LENGTH, MYROOM_ITEM_CHUNK_SIZE, MYROOM_SLOT_COUNT, MyRoomInfo,
        MyRoomKart, MyRoomParts, MyRoomPlayerSlot, MyRoomProtocolError, MyRoomRequest, MyRoomSlot,
        MyRoomTune, NOTIFY_MYROOM_INFO_NAME, OWNER_ITEM_ENCHANT_NAME, OWNER_ITEM_NAME,
        REENTER_MYROOM_REQUEST_NAME, REQUEST_EMBLEMS_NAME, REQUEST_MYROOM_ITEMS_NAME,
        RIDER_TALK_NAME, SECEDE_MYROOM_REQUEST_NAME, SLOT_DATA_NAME,
        UPDATE_MAIN_EMBLEM_REQUEST_NAME, classify_myroom_request, parse_character_position,
        parse_check_password, parse_enter_random_request, parse_enter_request, parse_first_request,
        parse_reenter_request, parse_request_emblems, parse_request_items, parse_rider_talk,
        parse_secede_request, parse_update_info, parse_update_main_emblem,
        serialize_character_position, serialize_check_password_reply, serialize_enter_error,
        serialize_enter_reply, serialize_missing_owner_items, serialize_myroom_info,
        serialize_owner_emblems, serialize_owner_item_enchants, serialize_owner_items,
        serialize_rider_echo, serialize_secede_reply, serialize_slot_data,
        serialize_update_main_emblem_reply, validate_myroom_info, validate_myroom_player_slot,
        wire_count, wire_index,
    };
    use crate::{
        adler32,
        packet::{PacketReader, PacketWriter},
        room_protocol::{MAX_CLUB_NAME_UTF16_UNITS, MAX_RIDER_NICKNAME_UTF16_UNITS},
    };

    fn sample_info() -> MyRoomInfo {
        MyRoomInfo {
            room_id: 17,
            bgm: 2,
            use_room_password: 1,
            use_item_password: 1,
            talk_lock: 0,
            room_password: "room".to_owned(),
            item_password: "item".to_owned(),
            kart_1: 513,
            kart_2: 514,
        }
    }

    fn info_body(writer: &mut PacketWriter, info: &MyRoomInfo) {
        writer.write_i16(info.room_id);
        writer.write_u8(info.bgm);
        writer.write_u8(info.use_room_password);
        writer.write_u8(0);
        writer.write_u8(info.use_item_password);
        writer.write_u8(info.talk_lock);
        writer.write_utf16(&info.room_password).unwrap();
        writer.write_utf16("").unwrap();
        writer.write_utf16(&info.item_password).unwrap();
        writer.write_i16(info.kart_1);
        writer.write_i16(info.kart_2);
    }

    #[test]
    fn classifier_covers_the_exact_twelve_p5136_request_hashes() {
        let fixtures = [
            (
                REENTER_MYROOM_REQUEST_NAME,
                1_733_888_222,
                MyRoomRequest::Reenter,
            ),
            (
                ENTER_RANDOM_MYROOM_REQUEST_NAME,
                2_423_851_656,
                MyRoomRequest::EnterRandom,
            ),
            (
                ENTER_MYROOM_REQUEST_NAME,
                1_466_239_015,
                MyRoomRequest::Enter,
            ),
            (
                FIRST_MYROOM_REQUEST_NAME,
                1_393_362_952,
                MyRoomRequest::FirstState,
            ),
            (
                REQUEST_MYROOM_ITEMS_NAME,
                1_397_032_962,
                MyRoomRequest::RequestItems,
            ),
            (
                NOTIFY_MYROOM_INFO_NAME,
                1_646_069_920,
                MyRoomRequest::UpdateInfo,
            ),
            (
                CHAR_POSITION_NAME,
                753_337_799,
                MyRoomRequest::CharacterPosition,
            ),
            (
                SECEDE_MYROOM_REQUEST_NAME,
                1_585_514_610,
                MyRoomRequest::Secede,
            ),
            (RIDER_TALK_NAME, 978_454_169, MyRoomRequest::RiderTalk),
            (
                CHECK_PASSWORD_REQUEST_NAME,
                2_610_694_874,
                MyRoomRequest::CheckPassword,
            ),
            (
                REQUEST_EMBLEMS_NAME,
                1_677_002_949,
                MyRoomRequest::RequestEmblems,
            ),
            (
                UPDATE_MAIN_EMBLEM_REQUEST_NAME,
                2_256_472_596,
                MyRoomRequest::UpdateMainEmblem,
            ),
        ];
        for (name, hash, request) in fixtures {
            assert_eq!(adler32::packet_hash(name), hash);
            assert_eq!(classify_myroom_request(hash), Some(request));
        }
        assert_eq!(classify_myroom_request(0xDEAD_BEEF), None);
    }

    #[test]
    fn parses_empty_enter_and_profile_requests_with_complete_consumption() {
        for (name, parser) in [
            (
                REENTER_MYROOM_REQUEST_NAME,
                parse_reenter_request as fn(&[u8]) -> _,
            ),
            (ENTER_RANDOM_MYROOM_REQUEST_NAME, parse_enter_random_request),
            (FIRST_MYROOM_REQUEST_NAME, parse_first_request),
            (REQUEST_MYROOM_ITEMS_NAME, parse_request_items),
            (SECEDE_MYROOM_REQUEST_NAME, parse_secede_request),
            (REQUEST_EMBLEMS_NAME, parse_request_emblems),
        ] {
            let packet = PacketWriter::named(name).into_inner();
            assert!(parser(&packet).is_ok());
            let mut trailing = packet;
            trailing.push(1);
            assert!(matches!(
                parser(&trailing),
                Err(MyRoomProtocolError::TrailingBytes { count: 1, .. })
            ));
        }

        let mut packet = PacketWriter::named(ENTER_MYROOM_REQUEST_NAME);
        packet.write_utf16("owner").unwrap();
        packet.write_i32(123);
        let request = parse_enter_request(packet.as_slice()).unwrap();
        assert_eq!(request.owner_nickname, "owner");
        assert_eq!(request.reserved, Some(123));

        let mut no_reserved = PacketWriter::named(ENTER_MYROOM_REQUEST_NAME);
        no_reserved.write_utf16("owner").unwrap();
        assert_eq!(
            parse_enter_request(no_reserved.as_slice())
                .unwrap()
                .reserved,
            None
        );
    }

    #[test]
    fn parses_info_position_talk_password_and_emblem_updates() {
        let info = sample_info();
        let mut update = PacketWriter::named(NOTIFY_MYROOM_INFO_NAME);
        info_body(&mut update, &info);
        assert_eq!(parse_update_info(update.as_slice()).unwrap(), info);

        let transform = [1.0, -2.0, 3.5, 4.0, 5.0, 6.0];
        let mut position = PacketWriter::named(CHAR_POSITION_NAME);
        position.write_i32(7);
        for value in transform {
            position.write_f32(value);
        }
        let parsed = parse_character_position(position.as_slice()).unwrap();
        assert_eq!(parsed.slot, 7);
        assert!(
            parsed
                .transform
                .into_iter()
                .zip(transform)
                .all(|(actual, expected)| actual.to_bits() == expected.to_bits())
        );

        let mut talk = PacketWriter::named(RIDER_TALK_NAME);
        talk.write_utf16("hello").unwrap();
        assert_eq!(parse_rider_talk(talk.as_slice()).unwrap().message, "hello");

        let mut password = PacketWriter::named(CHECK_PASSWORD_REQUEST_NAME);
        password.write_i32(1);
        assert_eq!(
            parse_check_password(password.as_slice())
                .unwrap()
                .password_kind,
            1
        );

        let mut emblem = PacketWriter::named(UPDATE_MAIN_EMBLEM_REQUEST_NAME);
        emblem.write_i16(-7);
        emblem.write_i16(5136);
        let parsed = parse_update_main_emblem(emblem.as_slice()).unwrap();
        assert_eq!(parsed.emblem_1, -7);
        assert_eq!(parsed.emblem_2, 5136);
    }

    #[test]
    fn small_replies_match_the_csharp_field_order() {
        let info = sample_info();
        let packet = serialize_enter_reply("owner", EnterMyRoomStatus::Success, &info).unwrap();
        let mut reader = PacketReader::new(&packet);
        assert_eq!(
            reader.read_u32().unwrap(),
            adler32::packet_hash("ChRpEnterMyRoomPacket")
        );
        assert_eq!(reader.read_utf16().unwrap(), "owner");
        assert_eq!(reader.read_u8().unwrap(), 0);
        assert_eq!(reader.read_i16().unwrap(), info.room_id);

        let error = serialize_enter_error(EnterMyRoomStatus::OwnerUnavailable).unwrap();
        let mut reader = PacketReader::new(&error);
        assert_eq!(reader.read_u32().unwrap(), 1_465_059_366);
        assert_eq!(reader.read_utf16().unwrap(), "");
        assert_eq!(reader.read_u8().unwrap(), 3);

        let info_packet = serialize_myroom_info(&info).unwrap();
        assert_eq!(
            &info_packet[..4],
            &adler32::packet_hash(NOTIFY_MYROOM_INFO_NAME).to_le_bytes()
        );

        let position = serialize_character_position(2, [1.0; 6]).unwrap();
        assert_eq!(position.len(), 4 + 4 + 24);
        let echo = serialize_rider_echo(2, "hello").unwrap();
        let mut reader = PacketReader::new(&echo);
        assert_eq!(reader.read_u32().unwrap(), 969_541_260);
        assert_eq!(reader.read_i32().unwrap(), 2);
        assert_eq!(reader.read_utf16().unwrap(), "hello");
        assert!(reader.remaining().is_empty());

        assert_eq!(serialize_secede_reply()[4..], [1]);
        assert_eq!(
            serialize_check_password_reply(0)[4..],
            [0, 0, 0, 0, 1, 0, 0, 0]
        );
        assert_eq!(
            serialize_check_password_reply(1)[4..],
            [1, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(serialize_update_main_emblem_reply(true)[4..], [1, 0]);
    }

    #[test]
    fn owner_emblem_reply_is_counted_and_bounded() {
        let packet = serialize_owner_emblems(&[-1, 7, 5136]).unwrap();
        let mut reader = PacketReader::new(&packet);
        assert_eq!(reader.read_u32().unwrap(), 1_236_207_476);
        assert_eq!(reader.read_i32().unwrap(), 1);
        assert_eq!(reader.read_i32().unwrap(), 1);
        assert_eq!(reader.read_i32().unwrap(), 3);
        assert_eq!(reader.read_i16().unwrap(), -1);
        assert_eq!(reader.read_i16().unwrap(), 7);
        assert_eq!(reader.read_i16().unwrap(), 5136);
        assert!(reader.remaining().is_empty());

        let excessive = vec![0; MAX_MYROOM_EMBLEMS + 1];
        assert!(matches!(
            serialize_owner_emblems(&excessive),
            Err(MyRoomProtocolError::TooManyEmblems { .. })
        ));
    }

    #[test]
    fn wire_counter_overflow_is_a_typed_error() {
        assert_eq!(wire_count("test", 26).unwrap(), 26);
        assert_eq!(wire_index("test", 25).unwrap(), 26);
        assert!(matches!(
            wire_count("test", usize::MAX),
            Err(MyRoomProtocolError::ItemCollectionTooLarge {
                field: "test",
                actual: usize::MAX,
            })
        ));
        assert!(matches!(
            wire_index("test", usize::MAX),
            Err(MyRoomProtocolError::ItemCollectionTooLarge {
                field: "test",
                actual: usize::MAX,
            })
        ));
    }

    #[test]
    fn owner_enchants_use_exact_24_byte_entries_and_nonempty_26_item_chunks() {
        let tunes: Vec<_> = (0..=MYROOM_ITEM_CHUNK_SIZE)
            .map(|index| MyRoomTune {
                item_id: i16::try_from(100 + index).unwrap(),
                serial_number: i16::try_from(200 + index).unwrap(),
                tune_1: 1,
                tune_2: 2,
                tune_3: 3,
                slot_1: 4,
                count_1: 5,
                slot_2: 6,
                count_2: 7,
            })
            .collect();
        let packets = serialize_owner_item_enchants(&tunes).unwrap();
        assert_eq!(packets.len(), 2);
        assert_eq!(adler32::packet_hash(OWNER_ITEM_ENCHANT_NAME), 1_961_625_970);
        assert_eq!(packets[0].len(), 4 + 4 + MYROOM_ITEM_CHUNK_SIZE * 24);
        assert_eq!(packets[1].len(), 4 + 4 + 24);

        let mut first = PacketReader::new(&packets[0]);
        assert_eq!(first.read_u32().unwrap(), 1_961_625_970);
        assert_eq!(first.read_i32().unwrap(), 26);
        assert_eq!(first.read_i16().unwrap(), 3);
        assert_eq!(first.read_i16().unwrap(), 100);
        assert_eq!(first.read_i16().unwrap(), 200);
        assert_eq!(first.read_i16().unwrap(), 0);
        assert_eq!(first.read_i16().unwrap(), 0);
        assert_eq!(first.read_i16().unwrap(), 1);
        assert_eq!(first.read_i16().unwrap(), 2);
        assert_eq!(first.read_i16().unwrap(), 3);
        assert_eq!(first.read_i16().unwrap(), 4);
        assert_eq!(first.read_i16().unwrap(), 5);
        assert_eq!(first.read_i16().unwrap(), 6);
        assert_eq!(first.read_i16().unwrap(), 7);

        let mut last = PacketReader::new(&packets[1]);
        assert_eq!(last.read_u32().unwrap(), 1_961_625_970);
        assert_eq!(last.read_i32().unwrap(), 1);
        assert_eq!(last.read_i16().unwrap(), 3);
        assert_eq!(last.read_i16().unwrap(), 126);
        assert_eq!(last.read_i16().unwrap(), 226);

        assert!(serialize_owner_item_enchants(&[]).unwrap().is_empty());
        let missing = serialize_missing_owner_items();
        assert_eq!(missing.len(), 8);
        assert_eq!(&missing[0..4], &1_961_625_970_u32.to_le_bytes());
        assert_eq!(&missing[4..8], &0_i32.to_le_bytes());
    }

    #[test]
    fn empty_owner_karts_preserve_the_p5136_early_return_and_omit_parts() {
        let parts = [MyRoomParts {
            item_id: 5136,
            ..MyRoomParts::default()
        }];
        let packets = serialize_owner_items(&[], &parts, false).unwrap();
        assert_eq!(packets.len(), 1);
        assert_eq!(adler32::packet_hash(OWNER_ITEM_NAME), 998_114_993);
        assert_eq!(packets[0].len(), 4 + 28);

        let mut reader = PacketReader::new(&packets[0]);
        assert_eq!(reader.read_u32().unwrap(), 998_114_993);
        assert_eq!(reader.read_i32().unwrap(), 1);
        assert_eq!(reader.read_i32().unwrap(), 1);
        assert_eq!(reader.read_i32().unwrap(), 0);
        assert_eq!(reader.read_bytes(8).unwrap(), &[0; 8]);
        assert_eq!(reader.read_i32().unwrap(), 1);
        assert_eq!(reader.read_i32().unwrap(), 1);
        assert!(reader.remaining().is_empty());
    }

    #[test]
    fn owner_karts_and_parts_share_global_ordinals_but_keep_local_chunk_counts() {
        let karts: Vec<_> = (0..=MYROOM_ITEM_CHUNK_SIZE)
            .map(|index| MyRoomKart {
                kart_id: u16::try_from(1000 + index).unwrap(),
                serial_number: u16::try_from(index + 1).unwrap(),
            })
            .collect();
        let parts: Vec<_> = (0..=MYROOM_ITEM_CHUNK_SIZE)
            .map(|index| MyRoomParts {
                item_id: i16::try_from(2000 + index).unwrap(),
                serial_number: i16::try_from(index + 1).unwrap(),
                engine: 11,
                engine_grade: 12,
                engine_value: 13,
                handle: 21,
                handle_grade: 22,
                handle_value: 23,
                wheel: 31,
                wheel_grade: 32,
                wheel_value: 33,
                booster: 41,
                booster_grade: 42,
                booster_value: 43,
                coating: 51,
                tail_lamp: 61,
            })
            .collect();
        let packets = serialize_owner_items(&karts, &parts, true).unwrap();
        assert_eq!(packets.len(), 4);
        assert_eq!(
            packets.iter().map(Vec::len).collect::<Vec<_>>(),
            [500, 50, 1_072, 72]
        );

        for (index, packet) in packets.iter().enumerate() {
            assert_eq!(&packet[0..4], &998_114_993_u32.to_le_bytes());
            assert_eq!(
                &packet[packet.len() - 8..packet.len() - 4],
                &4_i32.to_le_bytes()
            );
            assert_eq!(
                &packet[packet.len() - 4..],
                &i32::try_from(index + 1).unwrap().to_le_bytes()
            );
        }

        let mut second_kart = PacketReader::new(&packets[1]);
        assert_eq!(second_kart.read_u32().unwrap(), 998_114_993);
        assert_eq!(second_kart.read_i32().unwrap(), 2);
        assert_eq!(second_kart.read_i32().unwrap(), 2);
        assert_eq!(second_kart.read_i32().unwrap(), 1);
        assert_eq!(second_kart.read_u16().unwrap(), 3);
        assert_eq!(second_kart.read_u16().unwrap(), 1026);
        assert_eq!(second_kart.read_u16().unwrap(), 27);
        assert_eq!(second_kart.read_u16().unwrap(), 1);
        assert_eq!(second_kart.read_u8().unwrap(), 1);

        let mut last_part = PacketReader::new(&packets[3]);
        assert_eq!(last_part.read_u32().unwrap(), 998_114_993);
        assert_eq!(last_part.read_i32().unwrap(), 2);
        assert_eq!(last_part.read_i32().unwrap(), 2);
        assert_eq!(last_part.read_i32().unwrap(), 0);
        assert_eq!(last_part.read_i32().unwrap(), 0);
        assert_eq!(last_part.read_i32().unwrap(), 1);
        assert_eq!(last_part.read_i16().unwrap(), 2026);
        assert_eq!(last_part.read_i16().unwrap(), 27);
        assert_eq!(last_part.read_i16().unwrap(), 0);
        assert_eq!(last_part.read_i16().unwrap(), -1);
        assert_eq!(last_part.read_i16().unwrap(), 0);
        assert_eq!(last_part.read_i16().unwrap(), 11);
        assert_eq!(last_part.read_u8().unwrap(), 12);
        assert_eq!(last_part.read_i16().unwrap(), 13);
    }

    #[test]
    fn slot_data_matches_the_exact_p5136_player_and_empty_slot_layout() {
        let mut slots = vec![MyRoomSlot::Empty; MYROOM_SLOT_COUNT];
        slots[0] = MyRoomSlot::Player(MyRoomPlayerSlot {
            user_no: 0x1122_3344,
            p2p_address: Ipv4Addr::new(1, 2, 3, 4),
            p2p_port: 0x5678,
            nickname: "AB".to_owned(),
            rider_item_snapshot: std::array::from_fn(|index| {
                u8::try_from(index).expect("the 65-byte snapshot index fits in u8")
            }),
            rp: 0xa1b2_c3d4,
            club_name: "C".to_owned(),
        });

        let packet = serialize_slot_data(&slots).unwrap();
        assert_eq!(adler32::packet_hash(SLOT_DATA_NAME), 870_385_203);
        assert_eq!(&packet[0..4], &0x33e1_0633_u32.to_le_bytes());
        assert_eq!(packet.len(), 994);

        // First player slot starts immediately after the RTTI hash.
        assert_eq!(&packet[4..8], &0x1122_3344_u32.to_le_bytes());
        assert_eq!(&packet[8..12], &[1, 2, 3, 4]);
        assert_eq!(&packet[12..14], &0x5678_u16.to_le_bytes());
        assert_eq!(&packet[14..20], &[0; 6]);
        assert_eq!(&packet[20..24], &2_i32.to_le_bytes());
        assert_eq!(&packet[24..28], &[b'A', 0, b'B', 0]);
        assert_eq!(&packet[28..93], &(0_u8..65).collect::<Vec<_>>());
        assert_eq!(&packet[93..97], &0xa1b2_c3d4_u32.to_le_bytes());
        assert_eq!(&packet[97..126], &[0; 29]);
        assert_eq!(&packet[126..130], &1_i32.to_le_bytes());
        assert_eq!(&packet[130..132], &[b'C', 0]);
        assert_eq!(packet[132], 0);

        // Every remaining entry is exactly [0; 122] followed by 0xFF.
        for index in 0..7 {
            let start = 133 + index * MYROOM_EMPTY_SLOT_WIRE_LENGTH;
            assert_eq!(
                &packet[start..start + MYROOM_EMPTY_SLOT_WIRE_LENGTH - 1],
                &[0; MYROOM_EMPTY_SLOT_WIRE_LENGTH - 1]
            );
            assert_eq!(packet[start + MYROOM_EMPTY_SLOT_WIRE_LENGTH - 1], 0xff);
        }

        assert_eq!(
            format!("{:X}", Sha256::digest(&packet)),
            "F836C575D35E7ED5889E01E28A9FC861047E23587F25CB1AA79CCE629021C1C9"
        );
    }

    #[test]
    fn all_empty_slot_data_has_eight_exact_123_byte_sentinels() {
        let slots = vec![MyRoomSlot::Empty; MYROOM_SLOT_COUNT];
        let packet = serialize_slot_data(&slots).unwrap();
        assert_eq!(
            packet.len(),
            4 + MYROOM_SLOT_COUNT * MYROOM_EMPTY_SLOT_WIRE_LENGTH
        );
        for index in 0..MYROOM_SLOT_COUNT {
            let start = 4 + index * MYROOM_EMPTY_SLOT_WIRE_LENGTH;
            assert_eq!(
                &packet[start..start + MYROOM_EMPTY_SLOT_WIRE_LENGTH - 1],
                &[0; MYROOM_EMPTY_SLOT_WIRE_LENGTH - 1]
            );
            assert_eq!(packet[start + MYROOM_EMPTY_SLOT_WIRE_LENGTH - 1], 0xff);
        }
    }

    #[test]
    fn slot_data_rejects_wrong_counts_and_oversized_utf16_fields() {
        for count in [MYROOM_SLOT_COUNT - 1, MYROOM_SLOT_COUNT + 1] {
            let slots = vec![MyRoomSlot::Empty; count];
            assert!(matches!(
                serialize_slot_data(&slots),
                Err(MyRoomProtocolError::InvalidSlotCount {
                    actual,
                    expected: MYROOM_SLOT_COUNT
                }) if actual == count
            ));
        }

        let base = MyRoomPlayerSlot {
            user_no: 1,
            p2p_address: Ipv4Addr::LOCALHOST,
            p2p_port: 5136,
            nickname: String::new(),
            rider_item_snapshot: [0; 65],
            rp: 20_000_000,
            club_name: String::new(),
        };
        let mut slots = vec![MyRoomSlot::Empty; MYROOM_SLOT_COUNT];

        let mut oversized_nickname = base.clone();
        oversized_nickname.nickname = "x".repeat(MAX_RIDER_NICKNAME_UTF16_UNITS + 1);
        assert!(matches!(
            validate_myroom_player_slot(&oversized_nickname),
            Err(MyRoomProtocolError::StringTooLong {
                field: "MyRoom rider nickname",
                ..
            })
        ));
        slots[0] = MyRoomSlot::Player(oversized_nickname);
        assert!(matches!(
            serialize_slot_data(&slots),
            Err(MyRoomProtocolError::StringTooLong {
                field: "MyRoom rider nickname",
                actual,
                maximum: MAX_RIDER_NICKNAME_UTF16_UNITS
            }) if actual == MAX_RIDER_NICKNAME_UTF16_UNITS + 1
        ));

        let mut oversized_club = base;
        oversized_club.club_name = "x".repeat(MAX_CLUB_NAME_UTF16_UNITS + 1);
        assert!(matches!(
            validate_myroom_player_slot(&oversized_club),
            Err(MyRoomProtocolError::StringTooLong {
                field: "MyRoom club name",
                ..
            })
        ));
        slots[0] = MyRoomSlot::Player(oversized_club);
        assert!(matches!(
            serialize_slot_data(&slots),
            Err(MyRoomProtocolError::StringTooLong {
                field: "MyRoom club name",
                actual,
                maximum: MAX_CLUB_NAME_UTF16_UNITS
            }) if actual == MAX_CLUB_NAME_UTF16_UNITS + 1
        ));
    }

    #[test]
    fn malformed_bounds_and_nonfinite_transforms_are_rejected() {
        let mut wrong_hash = PacketWriter::named("not-myroom");
        wrong_hash.write_utf16("owner").unwrap();
        assert!(matches!(
            parse_enter_request(wrong_hash.as_slice()),
            Err(MyRoomProtocolError::UnexpectedPacketHash { .. })
        ));

        let mut partial_reserved = PacketWriter::named(ENTER_MYROOM_REQUEST_NAME);
        partial_reserved.write_utf16("owner").unwrap();
        partial_reserved.write_u8(1);
        assert!(matches!(
            parse_enter_request(partial_reserved.as_slice()),
            Err(MyRoomProtocolError::TrailingBytes { count: 1, .. })
        ));

        let mut invalid_slot = PacketWriter::named(CHAR_POSITION_NAME);
        invalid_slot.write_i32(8);
        for _ in 0..6 {
            invalid_slot.write_f32(0.0);
        }
        assert!(matches!(
            parse_character_position(invalid_slot.as_slice()),
            Err(MyRoomProtocolError::InvalidSlot(8))
        ));

        let mut nonfinite = PacketWriter::named(CHAR_POSITION_NAME);
        nonfinite.write_i32(0);
        for value in [0.0, 1.0, f32::NAN, 3.0, 4.0, 5.0] {
            nonfinite.write_f32(value);
        }
        assert!(matches!(
            parse_character_position(nonfinite.as_slice()),
            Err(MyRoomProtocolError::NonFiniteTransform { index: 2 })
        ));

        let oversized_password = "x".repeat(MAX_MYROOM_PASSWORD_UTF16_UNITS + 1);
        let mut info = sample_info();
        info.room_password = oversized_password;
        assert!(matches!(
            validate_myroom_info(&info),
            Err(MyRoomProtocolError::StringTooLong {
                field: "MyRoom room password",
                ..
            })
        ));
        assert!(matches!(
            serialize_myroom_info(&info),
            Err(MyRoomProtocolError::StringTooLong { .. })
        ));

        let oversized_message = "x".repeat(MAX_MYROOM_TALK_UTF16_UNITS + 1);
        assert!(matches!(
            serialize_rider_echo(0, &oversized_message),
            Err(MyRoomProtocolError::StringTooLong { .. })
        ));
    }
}
