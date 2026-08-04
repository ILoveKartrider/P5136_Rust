//! Reconstructed stock-consumer branches for the five read-only club replies.

use crate::{DecodeError, cursor::Cursor};

const MY_CLUB_STATE_HASH: u32 = 0x718B_0945;
const PENDING_JOIN_HASH: u32 = 0xB4E2_0BC2;
const CREATE_CONDITION_HASH: u32 = 0xC998_0C79;
const CLUB_LIST_COUNT_HASH: u32 = 0x72E0_0965;
const WAITING_CREW_COUNT_HASH: u32 = 0xBF7C_0C2D;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Membership {
    NoClub,
    Club(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingJoin {
    LookupFailed,
    None,
    Club(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateCondition {
    Allowed,
    InsufficientRp,
    InsufficientLucci,
    Unavailable,
    RefreshRequired,
    Unknown(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClubListCount {
    LocalPageFallback,
    Count(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitingCrewCapacity {
    CanJoin { current: u32, capacity: u32 },
    FullOrUnavailable { current: u32, capacity: u32 },
}

pub fn decode_membership(packet: &[u8]) -> Result<Membership, DecodeError> {
    let mut reader = Cursor::new(packet);
    reader.expect_hash("PrCheckMyClubStatePacket", MY_CLUB_STATE_HASH)?;
    let club_code = reader.u32()?;
    reader.utf16("club name", 64)?;
    reader.u32()?;
    reader.u32()?;
    reader.u16()?;
    reader.utf16("club master", 32)?;
    reader.u32()?;
    reader.u8()?;
    reader.finish()?;
    Ok(if club_code == 0 {
        Membership::NoClub
    } else {
        Membership::Club(club_code)
    })
}

pub fn decode_pending_join(packet: &[u8]) -> Result<PendingJoin, DecodeError> {
    let mut reader = Cursor::new(packet);
    reader.expect_hash("PrGetUserWaitingJoinClubPacket", PENDING_JOIN_HASH)?;
    let status = reader.u32()?;
    let club_code = reader.u32()?;
    reader.utf16("pending club name", 64)?;
    reader.finish()?;
    Ok(if status == 0 {
        PendingJoin::LookupFailed
    } else if club_code == 0 {
        PendingJoin::None
    } else {
        PendingJoin::Club(club_code)
    })
}

pub fn decode_create_condition(packet: &[u8]) -> Result<CreateCondition, DecodeError> {
    let mut reader = Cursor::new(packet);
    reader.expect_hash("PrCheckCreateClubConditionPacket", CREATE_CONDITION_HASH)?;
    let status = reader.u32()?;
    reader.finish()?;
    Ok(match status {
        0 => CreateCondition::Allowed,
        1 => CreateCondition::InsufficientRp,
        2 => CreateCondition::InsufficientLucci,
        3 => CreateCondition::Unavailable,
        4 => CreateCondition::RefreshRequired,
        value => CreateCondition::Unknown(value),
    })
}

pub fn decode_club_list_count(packet: &[u8]) -> Result<ClubListCount, DecodeError> {
    let mut reader = Cursor::new(packet);
    reader.expect_hash("PrGetClubListCountPacket", CLUB_LIST_COUNT_HASH)?;
    let count = reader.u32()?;
    reader.u32()?;
    reader.finish()?;
    Ok(if count == 0 {
        ClubListCount::LocalPageFallback
    } else {
        ClubListCount::Count(count)
    })
}

pub fn decode_waiting_crew_capacity(packet: &[u8]) -> Result<WaitingCrewCapacity, DecodeError> {
    let mut reader = Cursor::new(packet);
    reader.expect_hash("PrGetClubWaitingCrewCountPacket", WAITING_CREW_COUNT_HASH)?;
    let current = reader.u32()?;
    let capacity = reader.u32()?;
    reader.finish()?;
    Ok(if current < capacity {
        WaitingCrewCapacity::CanJoin { current, capacity }
    } else {
        WaitingCrewCapacity::FullOrUnavailable { current, capacity }
    })
}
