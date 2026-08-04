//! Structural client-side readers for the room list, admission replies, and
//! initial room snapshot.
//!
//! These layouts are currently backed by C# goldens plus live-client
//! progression, not by a fully recovered native consumer. They intentionally
//! remain a lower evidence grade than `game_result`.

use std::net::Ipv4Addr;

use crate::{DecodeError, cursor::Cursor, legacy_scalar};

const ROOM_LIST_HASH: u32 = 0x7286_0968;
const CREATE_ROOM_HASH: u32 = 0x6937_0900;
const JOIN_ROOM_HASH: u32 = 0x584A_083C;
const SESSION_DATA_HASH: u32 = 0x498E_076F;
const SLOT_DATA_HASH: u32 = 0x337C_062D;
const ROOM_SLOTS: usize = 8;
const OBSERVER_SLOTS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomList {
    pub page: i32,
    pub rooms: Vec<RoomListEntry>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreateRoomReply {
    pub created: bool,
    pub echoed_created: bool,
    pub slot_hint: u8,
    pub game_type: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JoinRoomReply {
    pub status: u8,
    pub success: bool,
    pub slot_hint: u8,
    pub game_type: u8,
    pub terminal: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionData {
    pub room_name: String,
    pub password: String,
    pub game_type: u8,
    pub speed_type: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub address: Ipv4Addr,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Player {
    pub player_type: i32,
    pub user_no: u32,
    pub p2p: Endpoint,
    pub secondary: Endpoint,
    pub nickname: String,
    pub emblems: [u16; 3],
    pub rider_items: [u8; 65],
    pub card: String,
    pub rp: u32,
    pub team: u8,
    pub ranking: i32,
    pub rider_school_level: u8,
    pub club_name: String,
    pub club_mark_logo: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ai {
    pub character: i16,
    pub rider: i16,
    pub kart: i16,
    pub balloon: i16,
    pub head_band: i16,
    pub goggle: i16,
    pub team: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Member {
    Empty,
    Closed { player_type: i32 },
    Player(Player),
    Ai(Ai),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observer {
    pub player_type: i32,
    pub user_no: u32,
    pub p2p: Endpoint,
    pub secondary: Endpoint,
    pub nickname: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotData {
    pub track: u32,
    pub room_data_header: u32,
    pub room_data: [u8; 32],
    pub room_master: i32,
    pub closed_slot_ids: Vec<u8>,
    pub members: Vec<Member>,
    pub observers: Vec<Option<Observer>>,
    pub slot_positions: [i32; ROOM_SLOTS],
}

pub fn decode_room_list(packet: &[u8]) -> Result<RoomList, DecodeError> {
    let mut reader = Cursor::new(packet);
    reader.expect_hash("ChGetRoomListReplyPacket", ROOM_LIST_HASH)?;
    let page = reader.i32()?;
    let count = reader.count("room-list entry", 10)?;
    let mut rooms = Vec::with_capacity(count);
    for _ in 0..count {
        let room_id = reader.i16()?;
        let room_name = reader.utf16("room name", 128)?;
        let track = reader.u32()?;
        let locked = reader.u8()? != 0;
        let game_type = reader.u8()?;
        let speed_type = reader.u8()?;
        let started = reader.u8()? != 0;
        let available_slots = reader.u8()?;
        let player_count = reader.u8()?;
        reader.bytes(2)?;
        rooms.push(RoomListEntry {
            room_id,
            room_name,
            track,
            locked,
            game_type,
            speed_type,
            started,
            available_slots,
            player_count,
        });
    }
    reader.finish()?;
    Ok(RoomList { page, rooms })
}

pub fn decode_create_room_reply(packet: &[u8]) -> Result<CreateRoomReply, DecodeError> {
    let mut reader = Cursor::new(packet);
    reader.expect_hash("ChCreateRoomReplyPacket", CREATE_ROOM_HASH)?;
    let created = reader.u8()? != 0;
    let echoed_created = reader.u8()? != 0;
    let slot_hint = reader.u8()?;
    let game_type = legacy_scalar::decode_u8(reader.u8()?);
    reader.finish()?;
    Ok(CreateRoomReply {
        created,
        echoed_created,
        slot_hint,
        game_type,
    })
}

pub fn decode_join_room_reply(packet: &[u8]) -> Result<JoinRoomReply, DecodeError> {
    let mut reader = Cursor::new(packet);
    reader.expect_hash("ChJoinRoomReplyPacket", JOIN_ROOM_HASH)?;
    let status = reader.u8()?;
    let success = reader.u8()? != 0;
    let slot_hint = reader.u8()?;
    let game_type = legacy_scalar::decode_u8(reader.u8()?);
    let terminal = reader.u8()?;
    reader.finish()?;
    Ok(JoinRoomReply {
        status,
        success,
        slot_hint,
        game_type,
        terminal,
    })
}

pub fn decode_session_data(packet: &[u8]) -> Result<SessionData, DecodeError> {
    let mut reader = Cursor::new(packet);
    reader.expect_hash("GrSessionDataPacket", SESSION_DATA_HASH)?;
    let room_name = reader.utf16("room name", 128)?;
    let password = reader.utf16("room password", 64)?;
    let game_type = reader.u8()?;
    let speed_type = reader.u8()?;
    reader.i32()?;
    reader.u8()?;
    reader.i32()?;
    reader.bytes(6)?;
    reader.finish()?;
    Ok(SessionData {
        room_name,
        password,
        game_type,
        speed_type,
    })
}

pub fn decode_slot_data(packet: &[u8]) -> Result<SlotData, DecodeError> {
    let mut reader = Cursor::new(packet);
    reader.expect_hash("GrSlotDataPacket", SLOT_DATA_HASH)?;
    let track = reader.u32()?;
    let room_data_header = reader.u32()?;
    let room_data = reader.array()?;
    let room_master = reader.i32()?;
    reader.bytes(11)?;
    let closed_count = reader.count("closed slot", ROOM_SLOTS)?;
    let closed_slot_ids = reader.bytes(closed_count)?.to_vec();
    reader.bytes(16)?;

    let mut members = Vec::with_capacity(ROOM_SLOTS);
    for _ in 0..ROOM_SLOTS {
        members.push(read_member(&mut reader)?);
    }
    let mut observers = Vec::with_capacity(OBSERVER_SLOTS);
    for _ in 0..OBSERVER_SLOTS {
        observers.push(read_observer(&mut reader)?);
    }
    let mut slot_positions = [0; ROOM_SLOTS];
    for position in &mut slot_positions {
        *position = reader.i32()?;
    }
    reader.finish()?;
    Ok(SlotData {
        track,
        room_data_header,
        room_data,
        room_master,
        closed_slot_ids,
        members,
        observers,
        slot_positions,
    })
}

fn read_member(reader: &mut Cursor<'_>) -> Result<Member, DecodeError> {
    let player_type = reader.i32()?;
    match player_type {
        0 => Ok(Member::Empty),
        1 => Ok(Member::Closed { player_type }),
        7 => Ok(Member::Ai(Ai {
            character: reader.i16()?,
            rider: reader.i16()?,
            kart: reader.i16()?,
            balloon: reader.i16()?,
            head_band: reader.i16()?,
            goggle: reader.i16()?,
            team: reader.u8()?,
        })),
        _ => read_player(reader, player_type).map(Member::Player),
    }
}

fn read_player(reader: &mut Cursor<'_>, player_type: i32) -> Result<Player, DecodeError> {
    let user_no = reader.u32()?;
    let p2p = read_endpoint(reader)?;
    let secondary = read_endpoint(reader)?;
    let nickname = reader.utf16("rider nickname", 32)?;
    let emblems = [reader.u16()?, reader.u16()?, reader.u16()?];
    let rider_items = reader.array()?;
    let card = reader.utf16("rider card", 128)?;
    let rp = reader.u32()?;
    let team = reader.u8()?;
    let ranking = reader.i32()?;
    reader.bytes(30)?;
    for _ in 0..5 {
        reader.i32()?;
    }
    reader.bytes(4)?;
    let rider_school_level = reader.u8()?;
    let club_name = reader.utf16("club name", 64)?;
    let club_mark_logo = reader.i32()?;
    reader.bytes(17)?;
    Ok(Player {
        player_type,
        user_no,
        p2p,
        secondary,
        nickname,
        emblems,
        rider_items,
        card,
        rp,
        team,
        ranking,
        rider_school_level,
        club_name,
        club_mark_logo,
    })
}

fn read_observer(reader: &mut Cursor<'_>) -> Result<Option<Observer>, DecodeError> {
    let player_type = reader.i32()?;
    if player_type == 0 {
        return Ok(None);
    }
    Ok(Some(Observer {
        player_type,
        user_no: reader.u32()?,
        p2p: read_endpoint(reader)?,
        secondary: read_endpoint(reader)?,
        nickname: reader.utf16("observer nickname", 32)?,
    }))
}

fn read_endpoint(reader: &mut Cursor<'_>) -> Result<Endpoint, DecodeError> {
    Ok(Endpoint {
        address: Ipv4Addr::from(reader.array::<4>()?),
        port: reader.u16()?,
    })
}
