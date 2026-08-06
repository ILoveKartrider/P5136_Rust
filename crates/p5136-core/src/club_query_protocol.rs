//! Strict read-only P5136 club-query packet primitives.
//!
//! Stock-executable producer and consumer evidence establishes eight request
//! shapes and their reply layouts. The Rust server does not yet have an
//! authoritative club repository, so this module normally exposes only honest
//! empty or unavailable replies. `PrInitClubInfoPacket` is the one deliberate
//! compatibility exception: its byte-exact legacy C# response is required to
//! let the stock client finish and leave the otherwise nonfunctional club UI.

use std::num::NonZeroU32;

use thiserror::Error;

use crate::{
    packet::{PacketError, PacketReader, PacketWriter},
    room_protocol::{MAX_CLUB_NAME_UTF16_UNITS, MAX_RIDER_NICKNAME_UTF16_UNITS},
};

pub const CHECK_MY_CLUB_STATE_REQUEST_NAME: &str = "PqCheckMyClubStatePacket";
pub const CHECK_MY_CLUB_STATE_REPLY_NAME: &str = "PrCheckMyClubStatePacket";
pub const INIT_CLUB_REQUEST_NAME: &str = "PqInitClubPacket";
pub const INIT_CLUB_REPLY_NAME: &str = "PrInitClubPacket";
pub const INIT_CLUB_INFO_REQUEST_NAME: &str = "PqInitClubInfoPacket";
pub const INIT_CLUB_INFO_REPLY_NAME: &str = "PrInitClubInfoPacket";
pub const GET_USER_WAITING_JOIN_CLUB_REQUEST_NAME: &str = "PqGetUserWaitingJoinClubPacket";
pub const GET_USER_WAITING_JOIN_CLUB_REPLY_NAME: &str = "PrGetUserWaitingJoinClubPacket";
pub const CHECK_CREATE_CLUB_CONDITION_REQUEST_NAME: &str = "PqCheckCreateClubConditionPacket";
pub const CHECK_CREATE_CLUB_CONDITION_REPLY_NAME: &str = "PrCheckCreateClubConditionPacket";
pub const GET_CLUB_LIST_COUNT_REQUEST_NAME: &str = "PqGetClubListCountPacket";
pub const GET_CLUB_LIST_COUNT_REPLY_NAME: &str = "PrGetClubListCountPacket";
pub const GET_CLUB_WAITING_CREW_COUNT_REQUEST_NAME: &str = "PqGetClubWaitingCrewCountPacket";
pub const GET_CLUB_WAITING_CREW_COUNT_REPLY_NAME: &str = "PrGetClubWaitingCrewCountPacket";
pub const SEARCH_CLUB_LIST_REQUEST_NAME: &str = "PqSearchClubListPacket";
pub const SEARCH_CLUB_LIST_REPLY_NAME: &str = "PrSearchClubListPacket";

pub const CHECK_MY_CLUB_STATE_REQUEST_HASH: u32 = 0x7174_0944;
pub const CHECK_MY_CLUB_STATE_REPLY_HASH: u32 = 0x718B_0945;
pub const INIT_CLUB_REQUEST_HASH: u32 = 0x33AB_0633;
pub const INIT_CLUB_REPLY_HASH: u32 = 0x33BA_0634;
pub const INIT_CLUB_INFO_REQUEST_HASH: u32 = 0x5008_07BF;
pub const INIT_CLUB_INFO_REPLY_HASH: u32 = 0x501B_07C0;
pub const GET_USER_WAITING_JOIN_CLUB_REQUEST_HASH: u32 = 0xB4C5_0BC1;
pub const GET_USER_WAITING_JOIN_CLUB_REPLY_HASH: u32 = 0xB4E2_0BC2;
pub const CHECK_CREATE_CLUB_CONDITION_REQUEST_HASH: u32 = 0xC979_0C78;
pub const CHECK_CREATE_CLUB_CONDITION_REPLY_HASH: u32 = 0xC998_0C79;
pub const GET_CLUB_LIST_COUNT_REQUEST_HASH: u32 = 0x72C9_0964;
pub const GET_CLUB_LIST_COUNT_REPLY_HASH: u32 = 0x72E0_0965;
pub const GET_CLUB_WAITING_CREW_COUNT_REQUEST_HASH: u32 = 0xBF5E_0C2C;
pub const GET_CLUB_WAITING_CREW_COUNT_REPLY_HASH: u32 = 0xBF7C_0C2D;
pub const SEARCH_CLUB_LIST_REQUEST_HASH: u32 = 0x60E1_0891;
pub const SEARCH_CLUB_LIST_REPLY_HASH: u32 = 0x60F6_0892;

/// A conservative server-side bound for the stock club-name search field.
///
/// The stock UI and serializer establish the field meaning and wire type but
/// do not expose a static maximum. Reusing the existing club-name invariant
/// keeps allocation bounded without inventing a second domain limit.
pub const MAX_CLUB_LIST_NAME_FILTER_UTF16_UNITS: usize = MAX_CLUB_NAME_UTF16_UNITS;

/// A conservative server-side bound for the stock club-master search field.
///
/// This reuses the existing rider-nickname invariant; it is a Rust resource
/// policy rather than a claim that the stock serializer enforces this limit.
pub const MAX_CLUB_MASTER_FILTER_UTF16_UNITS: usize = MAX_RIDER_NICKNAME_UTF16_UNITS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClubQueryRequest {
    InitClub,
    InitClubInfo,
    CheckMyClubState,
    GetUserWaitingJoinClub,
    CheckCreateClubCondition,
    GetClubListCount,
    GetClubWaitingCrewCount,
    SearchClubList,
}

impl ClubQueryRequest {
    #[must_use]
    pub const fn request_name(self) -> &'static str {
        match self {
            Self::InitClub => INIT_CLUB_REQUEST_NAME,
            Self::InitClubInfo => INIT_CLUB_INFO_REQUEST_NAME,
            Self::CheckMyClubState => CHECK_MY_CLUB_STATE_REQUEST_NAME,
            Self::GetUserWaitingJoinClub => GET_USER_WAITING_JOIN_CLUB_REQUEST_NAME,
            Self::CheckCreateClubCondition => CHECK_CREATE_CLUB_CONDITION_REQUEST_NAME,
            Self::GetClubListCount => GET_CLUB_LIST_COUNT_REQUEST_NAME,
            Self::GetClubWaitingCrewCount => GET_CLUB_WAITING_CREW_COUNT_REQUEST_NAME,
            Self::SearchClubList => SEARCH_CLUB_LIST_REQUEST_NAME,
        }
    }

    #[must_use]
    pub const fn reply_name(self) -> &'static str {
        match self {
            Self::InitClub => INIT_CLUB_REPLY_NAME,
            Self::InitClubInfo => INIT_CLUB_INFO_REPLY_NAME,
            Self::CheckMyClubState => CHECK_MY_CLUB_STATE_REPLY_NAME,
            Self::GetUserWaitingJoinClub => GET_USER_WAITING_JOIN_CLUB_REPLY_NAME,
            Self::CheckCreateClubCondition => CHECK_CREATE_CLUB_CONDITION_REPLY_NAME,
            Self::GetClubListCount => GET_CLUB_LIST_COUNT_REPLY_NAME,
            Self::GetClubWaitingCrewCount => GET_CLUB_WAITING_CREW_COUNT_REPLY_NAME,
            Self::SearchClubList => SEARCH_CLUB_LIST_REPLY_NAME,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
enum ParsedClubQueryFields {
    None,
    ClubListCount {
        club_name_filter: String,
        club_master_filter: String,
    },
    ClubWaitingCrewCount {
        club_code: NonZeroU32,
    },
    SearchClubList {
        query_type: i32,
        page: i32,
        club_name_filter: String,
        club_master_filter: String,
    },
}

/// A fully validated and exactly consumed stock club-query request.
///
/// Fields and constructors stay private so downstream code cannot manufacture
/// a parsed proof. `Debug` is deliberately omitted because the two club-list
/// search strings are user-entered data.
#[derive(Clone, PartialEq, Eq)]
pub struct ParsedClubQueryRequest {
    kind: ClubQueryRequest,
    fields: ParsedClubQueryFields,
}

impl ParsedClubQueryRequest {
    #[must_use]
    pub const fn kind(&self) -> ClubQueryRequest {
        self.kind
    }

    #[must_use]
    pub fn club_list_filters(&self) -> Option<(&str, &str)> {
        match &self.fields {
            ParsedClubQueryFields::SearchClubList {
                club_name_filter,
                club_master_filter,
                ..
            }
            | ParsedClubQueryFields::ClubListCount {
                club_name_filter,
                club_master_filter,
            } => Some((club_name_filter, club_master_filter)),
            ParsedClubQueryFields::None | ParsedClubQueryFields::ClubWaitingCrewCount { .. } => {
                None
            }
        }
    }

    #[must_use]
    pub fn club_code(&self) -> Option<NonZeroU32> {
        match &self.fields {
            ParsedClubQueryFields::ClubWaitingCrewCount { club_code } => Some(*club_code),
            ParsedClubQueryFields::None
            | ParsedClubQueryFields::ClubListCount { .. }
            | ParsedClubQueryFields::SearchClubList { .. } => None,
        }
    }

    #[must_use]
    pub fn search_page(&self) -> Option<(i32, i32)> {
        match &self.fields {
            ParsedClubQueryFields::SearchClubList {
                query_type, page, ..
            } => Some((*query_type, *page)),
            ParsedClubQueryFields::None
            | ParsedClubQueryFields::ClubListCount { .. }
            | ParsedClubQueryFields::ClubWaitingCrewCount { .. } => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum ClubQueryProtocolError {
    #[error(transparent)]
    Packet(#[from] PacketError),

    #[error("unsupported P5136 club-query packet hash 0x{actual:08X}")]
    UnsupportedPacketHash { actual: u32 },

    #[error("PqGetClubWaitingCrewCountPacket club code must be nonzero")]
    ZeroClubCode,

    #[error("packet {name} has {count} unexpected trailing bytes")]
    TrailingBytes { name: &'static str, count: usize },
}

#[must_use]
pub const fn classify_club_query_request(hash: u32) -> Option<ClubQueryRequest> {
    match hash {
        INIT_CLUB_REQUEST_HASH => Some(ClubQueryRequest::InitClub),
        INIT_CLUB_INFO_REQUEST_HASH => Some(ClubQueryRequest::InitClubInfo),
        CHECK_MY_CLUB_STATE_REQUEST_HASH => Some(ClubQueryRequest::CheckMyClubState),
        GET_USER_WAITING_JOIN_CLUB_REQUEST_HASH => Some(ClubQueryRequest::GetUserWaitingJoinClub),
        CHECK_CREATE_CLUB_CONDITION_REQUEST_HASH => {
            Some(ClubQueryRequest::CheckCreateClubCondition)
        }
        GET_CLUB_LIST_COUNT_REQUEST_HASH => Some(ClubQueryRequest::GetClubListCount),
        GET_CLUB_WAITING_CREW_COUNT_REQUEST_HASH => Some(ClubQueryRequest::GetClubWaitingCrewCount),
        SEARCH_CLUB_LIST_REQUEST_HASH => Some(ClubQueryRequest::SearchClubList),
        _ => None,
    }
}

/// Parses only the exact request forms emitted by the stock P5136 producers.
///
/// The two search strings are bounded before allocation. A waiting-crew query
/// requires a nonzero club code because the stock producer does not send that
/// request when no club is selected.
pub fn parse_club_query_request(
    packet: &[u8],
) -> Result<ParsedClubQueryRequest, ClubQueryProtocolError> {
    let mut reader = PacketReader::new(packet);
    let hash = reader.read_u32()?;
    let kind = classify_club_query_request(hash)
        .ok_or(ClubQueryProtocolError::UnsupportedPacketHash { actual: hash })?;
    let fields = match kind {
        ClubQueryRequest::InitClub
        | ClubQueryRequest::InitClubInfo
        | ClubQueryRequest::CheckMyClubState
        | ClubQueryRequest::GetUserWaitingJoinClub
        | ClubQueryRequest::CheckCreateClubCondition => ParsedClubQueryFields::None,
        ClubQueryRequest::GetClubListCount => ParsedClubQueryFields::ClubListCount {
            club_name_filter: reader.read_utf16_bounded(MAX_CLUB_LIST_NAME_FILTER_UTF16_UNITS)?,
            club_master_filter: reader.read_utf16_bounded(MAX_CLUB_MASTER_FILTER_UTF16_UNITS)?,
        },
        ClubQueryRequest::GetClubWaitingCrewCount => {
            let club_code =
                NonZeroU32::new(reader.read_u32()?).ok_or(ClubQueryProtocolError::ZeroClubCode)?;
            ParsedClubQueryFields::ClubWaitingCrewCount { club_code }
        }
        ClubQueryRequest::SearchClubList => ParsedClubQueryFields::SearchClubList {
            query_type: reader.read_i32()?,
            page: reader.read_i32()?,
            club_name_filter: reader.read_utf16_bounded(MAX_CLUB_LIST_NAME_FILTER_UTF16_UNITS)?,
            club_master_filter: reader.read_utf16_bounded(MAX_CLUB_MASTER_FILTER_UTF16_UNITS)?,
        },
    };
    ensure_exhausted(&reader, kind.request_name())?;
    Ok(ParsedClubQueryRequest { kind, fields })
}

/// Serializes the stock club-tab initialization gate.
///
/// The P5136 client requires this reply before it enables the club UI. The
/// C# compatibility server writes one `Int32`: zero when no club logo is
/// configured and one when the authenticated profile has a club logo.
#[must_use]
pub fn serialize_init_club_reply(has_club_logo: bool) -> Vec<u8> {
    let mut packet = PacketWriter::named(INIT_CLUB_REPLY_NAME);
    packet.write_i32(i32::from(has_club_logo));
    packet.into_inner()
}

/// Serializes the byte-exact fixed `PrInitClubInfoPacket` from the legacy C#
/// P5136 compatibility server.
///
/// This is intentionally not a club database projection. The stock client
/// sends the bodyless request after entering the club tab even when
/// `PrInitClubPacket` reports no club. Without this terminal response the UI
/// waits indefinitely and the player cannot navigate back. The fixed 83-byte
/// body describes an inert club-info snapshot and exists only as a UI escape
/// compatibility sink.
#[must_use]
pub fn serialize_fixed_init_club_info_reply() -> Vec<u8> {
    const CSHARP_FIXED_BODY: [u8; 83] = [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xE3, 0xC3, 0x78, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00,
        0x80, 0x02, 0xB8, 0x16, 0x00, 0x00, 0x00, 0x00,
    ];

    let mut packet = PacketWriter::named(INIT_CLUB_INFO_REPLY_NAME);
    packet.write_bytes(&CSHARP_FIXED_BODY);
    packet.into_inner()
}

/// Serializes a structurally complete `PrCheckMyClubStatePacket` no-club reply.
///
/// The leading zero club code is the stock consumer's membership gate. All
/// remaining fields are still emitted with their exact wire types so the
/// packet stays valid even though the consumer resets them to defaults.
pub fn serialize_no_club_state_reply() -> Result<Vec<u8>, PacketError> {
    let mut packet = PacketWriter::named(CHECK_MY_CLUB_STATE_REPLY_NAME);
    packet.write_u32(0);
    packet.write_utf16("")?;
    packet.write_u32(0);
    packet.write_u32(0);
    packet.write_u16(0);
    packet.write_utf16("")?;
    packet.write_u32(0);
    packet.write_u8(0);
    Ok(packet.into_inner())
}

/// Serializes the complete P5136 profile-backed club-state projection.
///
/// The stock consumer reads the rider grade, nickname, membership flag, and
/// level even when the leading club code selects the no-club branch. Omitting
/// those values leaves later club UI state detached from the authenticated
/// rider. A zero logo still suppresses club identity exactly like the C#
/// compatibility server, while retaining the trailing rider projection.
pub fn serialize_profile_club_state_reply(
    club_code: i32,
    club_name: &str,
    club_mark_logo: i32,
    club_mark_line: i32,
    nickname: &str,
) -> Result<Vec<u8>, PacketError> {
    let mut packet = PacketWriter::named(CHECK_MY_CLUB_STATE_REPLY_NAME);
    if club_mark_logo == 0 {
        packet.write_i32(0);
        packet.write_utf16("")?;
        packet.write_i32(0);
        packet.write_i32(0);
    } else {
        packet.write_i32(club_code);
        packet.write_utf16(club_name)?;
        packet.write_i32(club_mark_logo);
        packet.write_i32(club_mark_line);
    }
    packet.write_i16(5);
    packet.write_utf16(nickname)?;
    packet.write_i32(1);
    packet.write_u8(5);
    Ok(packet.into_inner())
}

/// Serializes a successful waiting-state lookup with no pending club request.
///
/// A zero first field would mean that the lookup itself failed. The evidenced
/// successful-empty representation is `status=1`, `club_code=0`, and an empty
/// UTF-16 club name.
pub fn serialize_no_pending_club_join_reply() -> Result<Vec<u8>, PacketError> {
    let mut packet = PacketWriter::named(GET_USER_WAITING_JOIN_CLUB_REPLY_NAME);
    packet.write_u32(1);
    packet.write_u32(0);
    packet.write_utf16("")?;
    Ok(packet.into_inner())
}

/// Serializes the stock status-3 create-condition failure.
///
/// Status zero enters club creation. Statuses one, two, and four claim
/// specific RP, Lucci, or refresh conditions. Three is the non-success value
/// that does not fabricate one of those unavailable facts.
#[must_use]
pub fn serialize_club_creation_unavailable_reply() -> Vec<u8> {
    let mut packet = PacketWriter::named(CHECK_CREATE_CLUB_CONDITION_REPLY_NAME);
    packet.write_u32(3);
    packet.into_inner()
}

/// Serializes an empty club-list count without the C# server's fake total.
///
/// The stock consumer ignores the second field. When the first field is zero,
/// it applies a local page fallback, so this reply is a safe empty repository
/// boundary rather than a promise that the current UI displays a literal zero.
#[must_use]
pub fn serialize_empty_club_list_count_reply() -> Vec<u8> {
    let mut packet = PacketWriter::named(GET_CLUB_LIST_COUNT_REPLY_NAME);
    packet.write_u32(0);
    packet.write_u32(0);
    packet.into_inner()
}

/// Serializes unavailable waiting-crew capacity as `current == capacity == 0`.
///
/// The stock consumer continues toward a join only when `current < capacity`;
/// equality therefore fails closed without inventing the C# server's 50/50
/// pseudo-capacity.
#[must_use]
pub fn serialize_unavailable_waiting_crew_count_reply() -> Vec<u8> {
    let mut packet = PacketWriter::named(GET_CLUB_WAITING_CREW_COUNT_REPLY_NAME);
    packet.write_u32(0);
    packet.write_u32(0);
    packet.into_inner()
}

/// Serializes a valid, completed empty search result. The stock consumer reads
/// a vector followed by a one-byte completion flag, so omitting either field
/// leaves the club UI waiting indefinitely.
#[must_use]
pub fn serialize_empty_club_search_reply() -> Vec<u8> {
    let mut packet = PacketWriter::named(SEARCH_CLUB_LIST_REPLY_NAME);
    packet.write_u32(0);
    packet.write_u8(1);
    let packet = packet.into_inner();
    debug_assert_eq!(packet.len(), 9);
    debug_assert_eq!(
        u32::from_le_bytes(packet[..4].try_into().unwrap()),
        SEARCH_CLUB_LIST_REPLY_HASH
    );
    packet
}

fn ensure_exhausted(
    reader: &PacketReader<'_>,
    name: &'static str,
) -> Result<(), ClubQueryProtocolError> {
    if reader.remaining().is_empty() {
        Ok(())
    } else {
        Err(ClubQueryProtocolError::TrailingBytes {
            name,
            count: reader.remaining().len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use sha2::{Digest, Sha256};

    use super::{
        CHECK_CREATE_CLUB_CONDITION_REPLY_HASH, CHECK_CREATE_CLUB_CONDITION_REPLY_NAME,
        CHECK_CREATE_CLUB_CONDITION_REQUEST_HASH, CHECK_CREATE_CLUB_CONDITION_REQUEST_NAME,
        CHECK_MY_CLUB_STATE_REPLY_HASH, CHECK_MY_CLUB_STATE_REPLY_NAME,
        CHECK_MY_CLUB_STATE_REQUEST_HASH, CHECK_MY_CLUB_STATE_REQUEST_NAME, ClubQueryProtocolError,
        ClubQueryRequest, GET_CLUB_LIST_COUNT_REPLY_HASH, GET_CLUB_LIST_COUNT_REPLY_NAME,
        GET_CLUB_LIST_COUNT_REQUEST_HASH, GET_CLUB_LIST_COUNT_REQUEST_NAME,
        GET_CLUB_WAITING_CREW_COUNT_REPLY_HASH, GET_CLUB_WAITING_CREW_COUNT_REPLY_NAME,
        GET_CLUB_WAITING_CREW_COUNT_REQUEST_HASH, GET_CLUB_WAITING_CREW_COUNT_REQUEST_NAME,
        GET_USER_WAITING_JOIN_CLUB_REPLY_HASH, GET_USER_WAITING_JOIN_CLUB_REPLY_NAME,
        GET_USER_WAITING_JOIN_CLUB_REQUEST_HASH, GET_USER_WAITING_JOIN_CLUB_REQUEST_NAME,
        INIT_CLUB_INFO_REPLY_HASH, INIT_CLUB_INFO_REPLY_NAME, INIT_CLUB_INFO_REQUEST_HASH,
        INIT_CLUB_INFO_REQUEST_NAME, INIT_CLUB_REPLY_HASH, INIT_CLUB_REPLY_NAME,
        INIT_CLUB_REQUEST_HASH, INIT_CLUB_REQUEST_NAME, MAX_CLUB_LIST_NAME_FILTER_UTF16_UNITS,
        MAX_CLUB_MASTER_FILTER_UTF16_UNITS, SEARCH_CLUB_LIST_REPLY_HASH,
        SEARCH_CLUB_LIST_REPLY_NAME, SEARCH_CLUB_LIST_REQUEST_HASH, SEARCH_CLUB_LIST_REQUEST_NAME,
        classify_club_query_request, parse_club_query_request,
        serialize_club_creation_unavailable_reply, serialize_empty_club_list_count_reply,
        serialize_empty_club_search_reply, serialize_fixed_init_club_info_reply,
        serialize_init_club_reply, serialize_no_club_state_reply,
        serialize_no_pending_club_join_reply, serialize_profile_club_state_reply,
        serialize_unavailable_waiting_crew_count_reply,
    };
    use crate::{
        adler32,
        packet::{PacketError, PacketWriter},
    };

    const HASHES: [(&str, u32); 16] = [
        (INIT_CLUB_REQUEST_NAME, INIT_CLUB_REQUEST_HASH),
        (INIT_CLUB_REPLY_NAME, INIT_CLUB_REPLY_HASH),
        (INIT_CLUB_INFO_REQUEST_NAME, INIT_CLUB_INFO_REQUEST_HASH),
        (INIT_CLUB_INFO_REPLY_NAME, INIT_CLUB_INFO_REPLY_HASH),
        (
            CHECK_MY_CLUB_STATE_REQUEST_NAME,
            CHECK_MY_CLUB_STATE_REQUEST_HASH,
        ),
        (
            CHECK_MY_CLUB_STATE_REPLY_NAME,
            CHECK_MY_CLUB_STATE_REPLY_HASH,
        ),
        (
            GET_USER_WAITING_JOIN_CLUB_REQUEST_NAME,
            GET_USER_WAITING_JOIN_CLUB_REQUEST_HASH,
        ),
        (
            GET_USER_WAITING_JOIN_CLUB_REPLY_NAME,
            GET_USER_WAITING_JOIN_CLUB_REPLY_HASH,
        ),
        (
            CHECK_CREATE_CLUB_CONDITION_REQUEST_NAME,
            CHECK_CREATE_CLUB_CONDITION_REQUEST_HASH,
        ),
        (
            CHECK_CREATE_CLUB_CONDITION_REPLY_NAME,
            CHECK_CREATE_CLUB_CONDITION_REPLY_HASH,
        ),
        (
            GET_CLUB_LIST_COUNT_REQUEST_NAME,
            GET_CLUB_LIST_COUNT_REQUEST_HASH,
        ),
        (
            GET_CLUB_LIST_COUNT_REPLY_NAME,
            GET_CLUB_LIST_COUNT_REPLY_HASH,
        ),
        (
            GET_CLUB_WAITING_CREW_COUNT_REQUEST_NAME,
            GET_CLUB_WAITING_CREW_COUNT_REQUEST_HASH,
        ),
        (
            GET_CLUB_WAITING_CREW_COUNT_REPLY_NAME,
            GET_CLUB_WAITING_CREW_COUNT_REPLY_HASH,
        ),
        (SEARCH_CLUB_LIST_REQUEST_NAME, SEARCH_CLUB_LIST_REQUEST_HASH),
        (SEARCH_CLUB_LIST_REPLY_NAME, SEARCH_CLUB_LIST_REPLY_HASH),
    ];

    const REQUESTS: [(ClubQueryRequest, u32); 8] = [
        (ClubQueryRequest::InitClub, INIT_CLUB_REQUEST_HASH),
        (ClubQueryRequest::InitClubInfo, INIT_CLUB_INFO_REQUEST_HASH),
        (
            ClubQueryRequest::CheckMyClubState,
            CHECK_MY_CLUB_STATE_REQUEST_HASH,
        ),
        (
            ClubQueryRequest::GetUserWaitingJoinClub,
            GET_USER_WAITING_JOIN_CLUB_REQUEST_HASH,
        ),
        (
            ClubQueryRequest::CheckCreateClubCondition,
            CHECK_CREATE_CLUB_CONDITION_REQUEST_HASH,
        ),
        (
            ClubQueryRequest::GetClubListCount,
            GET_CLUB_LIST_COUNT_REQUEST_HASH,
        ),
        (
            ClubQueryRequest::GetClubWaitingCrewCount,
            GET_CLUB_WAITING_CREW_COUNT_REQUEST_HASH,
        ),
        (
            ClubQueryRequest::SearchClubList,
            SEARCH_CLUB_LIST_REQUEST_HASH,
        ),
    ];

    fn hash_only_request(kind: ClubQueryRequest) -> Vec<u8> {
        PacketWriter::named(kind.request_name()).into_inner()
    }

    fn list_count_request(club_name: &str, club_master: &str) -> Vec<u8> {
        let mut packet = PacketWriter::named(GET_CLUB_LIST_COUNT_REQUEST_NAME);
        packet.write_utf16(club_name).expect("test club name fits");
        packet
            .write_utf16(club_master)
            .expect("test club master fits");
        packet.into_inner()
    }

    fn waiting_crew_count_request(club_code: u32) -> Vec<u8> {
        let mut packet = PacketWriter::named(GET_CLUB_WAITING_CREW_COUNT_REQUEST_NAME);
        packet.write_u32(club_code);
        packet.into_inner()
    }

    fn search_request(query_type: i32, page: i32, club_name: &str, club_master: &str) -> Vec<u8> {
        let mut packet = PacketWriter::named(SEARCH_CLUB_LIST_REQUEST_NAME);
        packet.write_i32(query_type);
        packet.write_i32(page);
        packet.write_utf16(club_name).expect("test club name fits");
        packet
            .write_utf16(club_master)
            .expect("test club master fits");
        packet.into_inner()
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    #[test]
    fn packet_names_match_all_exact_p5136_hashes() {
        for (name, expected) in HASHES {
            assert_eq!(adler32::packet_hash(name), expected, "{name}");
        }
    }

    #[test]
    fn classifier_and_names_cover_only_the_eight_requests() {
        fn assert_copy_and_eq<T: Copy + Eq>() {}
        assert_copy_and_eq::<ClubQueryRequest>();

        for (kind, hash) in REQUESTS {
            assert_eq!(classify_club_query_request(hash), Some(kind));
            assert!(!kind.request_name().is_empty());
            assert!(!kind.reply_name().is_empty());
        }
        for hash in [
            0,
            CHECK_MY_CLUB_STATE_REPLY_HASH,
            GET_CLUB_LIST_COUNT_REPLY_HASH,
            u32::MAX,
        ] {
            assert_eq!(classify_club_query_request(hash), None);
        }
    }

    #[test]
    fn exact_hash_only_requests_parse_without_body_fields() {
        for kind in [
            ClubQueryRequest::InitClub,
            ClubQueryRequest::InitClubInfo,
            ClubQueryRequest::CheckMyClubState,
            ClubQueryRequest::GetUserWaitingJoinClub,
            ClubQueryRequest::CheckCreateClubCondition,
        ] {
            let parsed = parse_club_query_request(&hash_only_request(kind)).expect("exact request");
            assert_eq!(parsed.kind(), kind);
            assert_eq!(parsed.club_list_filters(), None);
            assert_eq!(parsed.club_code(), None);
        }
    }

    #[test]
    fn stock_request_shapes_match_full_golden_packets() {
        assert_eq!(
            hash_only_request(ClubQueryRequest::InitClub),
            [0x33, 0x06, 0xAB, 0x33]
        );
        assert_eq!(
            hash_only_request(ClubQueryRequest::InitClubInfo),
            [0xBF, 0x07, 0x08, 0x50]
        );
        assert_eq!(
            hash_only_request(ClubQueryRequest::CheckMyClubState),
            [0x44, 0x09, 0x74, 0x71]
        );
        assert_eq!(
            hash_only_request(ClubQueryRequest::GetUserWaitingJoinClub),
            [0xC1, 0x0B, 0xC5, 0xB4]
        );
        assert_eq!(
            hash_only_request(ClubQueryRequest::CheckCreateClubCondition),
            [0x78, 0x0C, 0x79, 0xC9]
        );
        assert_eq!(
            list_count_request("", ""),
            [0x64, 0x09, 0xC9, 0x72, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(
            waiting_crew_count_request(10_000),
            [0x2C, 0x0C, 0x5E, 0xBF, 0x10, 0x27, 0, 0]
        );
        assert_eq!(
            search_request(12, 0, "", ""),
            [
                0x91, 0x08, 0xE1, 0x60, 12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
            ]
        );
    }

    #[test]
    fn exact_nonempty_request_fields_preserve_wire_order_and_scalar_domain() {
        let list = parse_club_query_request(&list_count_request("Club\u{1F3C1}", "Master"))
            .expect("exact list count");
        assert_eq!(list.kind(), ClubQueryRequest::GetClubListCount);
        assert_eq!(list.club_list_filters(), Some(("Club\u{1F3C1}", "Master")));
        assert_eq!(list.club_code(), None);

        let search = parse_club_query_request(&search_request(-1, 3, "Club", "Master"))
            .expect("exact search");
        assert_eq!(search.kind(), ClubQueryRequest::SearchClubList);
        assert_eq!(search.search_page(), Some((-1, 3)));
        assert_eq!(search.club_list_filters(), Some(("Club", "Master")));

        for club_code in [1, 10_000, u32::MAX] {
            let waiting = parse_club_query_request(&waiting_crew_count_request(club_code))
                .expect("exact waiting count");
            assert_eq!(
                waiting.club_code(),
                NonZeroU32::new(club_code),
                "club code {club_code}"
            );
            assert_eq!(waiting.club_list_filters(), None);
        }
    }

    #[test]
    fn every_truncated_prefix_of_each_exact_request_is_rejected() {
        let fixtures = [
            hash_only_request(ClubQueryRequest::InitClub),
            hash_only_request(ClubQueryRequest::InitClubInfo),
            hash_only_request(ClubQueryRequest::CheckMyClubState),
            hash_only_request(ClubQueryRequest::GetUserWaitingJoinClub),
            hash_only_request(ClubQueryRequest::CheckCreateClubCondition),
            list_count_request("Club\u{1F3C1}", "Master"),
            waiting_crew_count_request(10_000),
            search_request(12, 0, "Club", "Master"),
        ];
        for fixture in fixtures {
            for length in 0..fixture.len() {
                assert!(
                    matches!(
                        parse_club_query_request(&fixture[..length]),
                        Err(ClubQueryProtocolError::Packet(
                            PacketError::Truncated { .. }
                        ))
                    ),
                    "prefix {length} of {} unexpectedly parsed",
                    fixture.len()
                );
            }
        }
    }

    #[test]
    fn unknown_hash_is_rejected_before_any_body_parsing() {
        for hash in [CHECK_MY_CLUB_STATE_REPLY_HASH, 0xDEAD_BEEF] {
            assert!(matches!(
                parse_club_query_request(&hash.to_le_bytes()),
                Err(ClubQueryProtocolError::UnsupportedPacketHash { actual })
                    if actual == hash
            ));
        }
    }

    #[test]
    fn cross_kind_body_drift_and_trailing_bytes_are_rejected() {
        let list = list_count_request("", "");
        let waiting = waiting_crew_count_request(7);

        let mut list_as_hash_only = list.clone();
        list_as_hash_only[..4].copy_from_slice(&CHECK_MY_CLUB_STATE_REQUEST_HASH.to_le_bytes());
        assert!(matches!(
            parse_club_query_request(&list_as_hash_only),
            Err(ClubQueryProtocolError::TrailingBytes {
                name: CHECK_MY_CLUB_STATE_REQUEST_NAME,
                count: 8
            })
        ));

        let mut waiting_as_list = waiting;
        waiting_as_list[..4].copy_from_slice(&GET_CLUB_LIST_COUNT_REQUEST_HASH.to_le_bytes());
        assert!(matches!(
            parse_club_query_request(&waiting_as_list),
            Err(ClubQueryProtocolError::Packet(
                PacketError::Truncated { .. }
            ))
        ));

        for mut request in [
            hash_only_request(ClubQueryRequest::GetUserWaitingJoinClub),
            list,
            waiting_crew_count_request(9),
        ] {
            request.extend_from_slice(&[0xA5, 0x5A]);
            assert!(matches!(
                parse_club_query_request(&request),
                Err(ClubQueryProtocolError::TrailingBytes { count: 2, .. })
            ));
        }
    }

    #[test]
    fn list_filters_are_bounded_by_utf16_units_before_allocation() {
        let maximum_club = "x".repeat(MAX_CLUB_LIST_NAME_FILTER_UTF16_UNITS);
        let maximum_master = "y".repeat(MAX_CLUB_MASTER_FILTER_UTF16_UNITS);
        let parsed = parse_club_query_request(&list_count_request(&maximum_club, &maximum_master))
            .expect("maximum filters");
        assert_eq!(
            parsed.club_list_filters(),
            Some((maximum_club.as_str(), maximum_master.as_str()))
        );

        let surrogate_pairs = "\u{1F3C1}".repeat(MAX_CLUB_LIST_NAME_FILTER_UTF16_UNITS / 2);
        parse_club_query_request(&list_count_request(&surrogate_pairs, ""))
            .expect("maximum surrogate-pair club filter");

        let overlong_club = "x".repeat(MAX_CLUB_LIST_NAME_FILTER_UTF16_UNITS + 1);
        assert!(matches!(
            parse_club_query_request(&list_count_request(&overlong_club, "")),
            Err(ClubQueryProtocolError::Packet(
                PacketError::StringLimitExceeded { length, maximum }
            )) if length == MAX_CLUB_LIST_NAME_FILTER_UTF16_UNITS + 1
                && maximum == MAX_CLUB_LIST_NAME_FILTER_UTF16_UNITS
        ));

        let overlong_master = "x".repeat(MAX_CLUB_MASTER_FILTER_UTF16_UNITS + 1);
        assert!(matches!(
            parse_club_query_request(&list_count_request("", &overlong_master)),
            Err(ClubQueryProtocolError::Packet(
                PacketError::StringLimitExceeded { length, maximum }
            )) if length == MAX_CLUB_MASTER_FILTER_UTF16_UNITS + 1
                && maximum == MAX_CLUB_MASTER_FILTER_UTF16_UNITS
        ));
    }

    #[test]
    fn negative_string_lengths_and_zero_club_code_have_typed_errors() {
        let mut negative = list_count_request("", "");
        negative[4..8].copy_from_slice(&(-1_i32).to_le_bytes());
        assert!(matches!(
            parse_club_query_request(&negative),
            Err(ClubQueryProtocolError::Packet(
                PacketError::NegativeStringLength(-1)
            ))
        ));

        assert!(matches!(
            parse_club_query_request(&waiting_crew_count_request(0)),
            Err(ClubQueryProtocolError::ZeroClubCode)
        ));
    }

    #[test]
    fn no_club_state_reply_matches_the_full_golden_packet_and_digest() {
        let reply = serialize_no_club_state_reply().expect("fixed strings fit");
        let mut expected = [0_u8; 31];
        expected[..4].copy_from_slice(&CHECK_MY_CLUB_STATE_REPLY_HASH.to_le_bytes());
        assert_eq!(reply, expected);
        assert_eq!(reply.len(), 31);
        assert_eq!(
            sha256_hex(&reply),
            "30ff57e681453da377357a5f9012f12bd956546224299e7e101927b7c38eeaf1"
        );
    }

    #[test]
    fn init_club_reply_matches_the_csharp_membership_gate() {
        assert_eq!(
            serialize_init_club_reply(false),
            [0x34, 0x06, 0xBA, 0x33, 0, 0, 0, 0]
        );
        assert_eq!(
            serialize_init_club_reply(true),
            [0x34, 0x06, 0xBA, 0x33, 1, 0, 0, 0]
        );
    }

    #[test]
    fn fixed_init_club_info_reply_matches_the_legacy_csharp_packet() {
        let reply = serialize_fixed_init_club_info_reply();
        let expected = [
            0xC0, 0x07, 0x1B, 0x50, // PrInitClubInfoPacket hash
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xE3, 0xC3, 0x78, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x02, 0x00, 0x00, 0x00, 0x80, 0x02, 0xB8, 0x16, 0x00, 0x00, 0x00, 0x00,
        ];
        assert_eq!(reply, expected);
        assert_eq!(reply.len(), 87);
        assert_eq!(
            sha256_hex(&reply),
            "519f169b06cbab54fa947eadb95e61a45b6fd6f83df9ce93f0c0939ec4d57844"
        );
    }

    #[test]
    fn profile_club_state_keeps_the_authenticated_rider_tail_in_both_branches() {
        let no_club = serialize_profile_club_state_reply(10_000, "ignored", 0, 7, "Yany")
            .expect("bounded fixture strings fit");
        assert_eq!(u32::from_le_bytes(no_club[4..8].try_into().unwrap()), 0);
        assert!(no_club.windows(8).any(|window| window == b"Y\0a\0n\0y\0"));
        assert_eq!(&no_club[no_club.len() - 5..], &[1, 0, 0, 0, 5]);

        let club = serialize_profile_club_state_reply(10_000, "TCCstar", 77, 88, "Yany")
            .expect("bounded fixture strings fit");
        assert_eq!(i32::from_le_bytes(club[4..8].try_into().unwrap()), 10_000);
        assert!(
            club.windows(14)
                .any(|window| { window == b"T\0C\0C\0s\0t\0a\0r\0" })
        );
        assert_eq!(&club[club.len() - 5..], &[1, 0, 0, 0, 5]);
    }

    #[test]
    fn no_pending_join_reply_matches_the_full_golden_packet_and_digest() {
        let reply = serialize_no_pending_club_join_reply().expect("fixed string fits");
        let expected = [
            0xC2, 0x0B, 0xE2, 0xB4, // reply hash
            0x01, 0x00, 0x00, 0x00, // successful lookup
            0x00, 0x00, 0x00, 0x00, // no pending club code
            0x00, 0x00, 0x00, 0x00, // empty UTF-16 club name
        ];
        assert_eq!(reply, expected);
        assert_eq!(reply.len(), 16);
        assert_eq!(
            sha256_hex(&reply),
            "1c405d2e5e0488e12e76810dc28755cbd3a2121e9e13757f5fb945fabf779263"
        );
    }

    #[test]
    fn unavailable_create_and_count_replies_match_full_golden_packets() {
        let create = serialize_club_creation_unavailable_reply();
        assert_eq!(create, [0x79, 0x0C, 0x98, 0xC9, 3, 0, 0, 0]);
        assert_eq!(
            sha256_hex(&create),
            "156e6d86242ad83c166b934584a40a977785e90c6372450ed836be0c657c2615"
        );

        let list = serialize_empty_club_list_count_reply();
        assert_eq!(list, [0x65, 0x09, 0xE0, 0x72, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(
            sha256_hex(&list),
            "4803dd15f50145a10f181d859e9d60074c56374f84a1bea4f265eb4d0983780d"
        );

        let waiting = serialize_unavailable_waiting_crew_count_reply();
        assert_eq!(waiting, [0x2D, 0x0C, 0x7C, 0xBF, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(
            sha256_hex(&waiting),
            "7d6833ca5f580bbc6c1796a4e066782ed44822dd72d8be1d1c24a5cdfb188b39"
        );

        let search = serialize_empty_club_search_reply();
        assert_eq!(search, [0x92, 0x08, 0xF6, 0x60, 0, 0, 0, 0, 1]);
    }
}
