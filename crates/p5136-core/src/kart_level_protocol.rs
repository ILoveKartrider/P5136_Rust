//! P5136 legacy kart-level request and response codecs.
//!
//! These packet layouts follow the Korean 5136 C# compatibility handlers.
//! Request parsers deliberately require the exact stock-client body sizes: the
//! retained client emits fixed-width records and no optional suffix is known.

use thiserror::Error;

use crate::{
    adler32,
    login::LegacyTime,
    packet::{PacketError, PacketReader, PacketWriter},
};

pub const KART_LEVEL_UP_PROBABILITY_REQUEST_NAME: &str = "PqKartLevelUpProbText";
pub const KART_LEVEL_UP_PROBABILITY_REPLY_NAME: &str = "PrKartLevelUpProbText";
pub const KART_LEVEL_UP_REQUEST_NAME: &str = "PqKartLevelUp";
pub const KART_LEVEL_UP_REPLY_NAME: &str = "PrKartLevelUp";
pub const KART_LEVEL_POINT_UPDATE_REQUEST_NAME: &str = "PqKartLevelPointUpdate";
pub const KART_LEVEL_POINT_UPDATE_REPLY_NAME: &str = "PrKartLevelPointUpdate";
pub const KART_LEVEL_POINT_CLEAR_REQUEST_NAME: &str = "PqKartLevelPointClear";
pub const KART_LEVEL_POINT_CLEAR_REPLY_NAME: &str = "PrKartLevelPointClear";
pub const KART_LEVEL_SPECIAL_SLOT_UPDATE_REQUEST_NAME: &str = "PqKartLevelSpecialSlotUpdate";
pub const KART_LEVEL_SPECIAL_SLOT_UPDATE_REPLY_NAME: &str = "PrKartLevelSpecialSlotUpdate";

pub const KART_LEVEL_UP_PROBABILITY_REQUEST_WIRE_LENGTH: usize = 12;
pub const KART_LEVEL_UP_REQUEST_WIRE_LENGTH: usize = 16;
pub const KART_LEVEL_POINT_UPDATE_REQUEST_WIRE_LENGTH: usize = 16;
pub const KART_LEVEL_POINT_CLEAR_REQUEST_WIRE_LENGTH: usize = 8;
pub const KART_LEVEL_SPECIAL_SLOT_UPDATE_REQUEST_WIRE_LENGTH: usize = 10;

const STATE_REPLY_SUCCESS: i32 = 1;
const STATE_REPLY_FAILURE: i32 = 0;
// This is the client-facing result code, not the configured success chance.
// The retained C# handler writes zero for an accepted probability-text query.
const PROBABILITY_REPLY_SUCCESS: i32 = 0;

/// Identifies one independently owned kart inventory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KartInstance {
    pub kart_id: i16,
    pub serial: i16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KartLevelUpProbabilityRequest {
    pub kart: KartInstance,
    pub donor: KartInstance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KartLevelUpRequest {
    pub kart: KartInstance,
    pub donor: KartInstance,
    pub cost: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KartLevelPointUpdateRequest {
    pub kart: KartInstance,
    /// Additive point deltas in the client's level-1 through level-4 order.
    pub level_deltas: [i16; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KartLevelPointClearRequest {
    pub kart: KartInstance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KartLevelSpecialSlotUpdateRequest {
    pub kart: KartInstance,
    pub effect: i16,
}

/// Persistent state shared by the point-clear, point-update, and special-slot
/// replies. `points` is the number of unallocated points remaining.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KartLevelState {
    pub kart_id: i16,
    pub serial: i16,
    pub grade: i16,
    pub points: i16,
    pub level1: i16,
    pub level2: i16,
    pub level3: i16,
    pub level4: i16,
    pub effect: i16,
}

impl KartLevelState {
    #[must_use]
    pub const fn kart(self) -> KartInstance {
        KartInstance {
            kart_id: self.kart_id,
            serial: self.serial,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KartLevelRequestKind {
    LevelUpProbability,
    LevelUp,
    PointUpdate,
    PointClear,
    SpecialSlotUpdate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KartLevelRequest {
    LevelUpProbability(KartLevelUpProbabilityRequest),
    LevelUp(KartLevelUpRequest),
    PointUpdate(KartLevelPointUpdateRequest),
    PointClear(KartLevelPointClearRequest),
    SpecialSlotUpdate(KartLevelSpecialSlotUpdateRequest),
}

#[derive(Debug, Error)]
pub enum KartLevelProtocolError {
    #[error(transparent)]
    Packet(#[from] PacketError),

    #[error("expected {name} hash 0x{expected:08X}, received 0x{actual:08X}")]
    UnexpectedPacketHash {
        name: &'static str,
        expected: u32,
        actual: u32,
    },

    #[error("packet hash 0x{0:08X} is not a P5136 kart-level request")]
    UnknownRequestHash(u32),

    #[error("packet {name} has {count} unexpected trailing bytes")]
    TrailingBytes { name: &'static str, count: usize },
}

#[must_use]
pub fn classify_kart_level_request(hash: u32) -> Option<KartLevelRequestKind> {
    if hash == adler32::packet_hash(KART_LEVEL_UP_PROBABILITY_REQUEST_NAME) {
        Some(KartLevelRequestKind::LevelUpProbability)
    } else if hash == adler32::packet_hash(KART_LEVEL_UP_REQUEST_NAME) {
        Some(KartLevelRequestKind::LevelUp)
    } else if hash == adler32::packet_hash(KART_LEVEL_POINT_UPDATE_REQUEST_NAME) {
        Some(KartLevelRequestKind::PointUpdate)
    } else if hash == adler32::packet_hash(KART_LEVEL_POINT_CLEAR_REQUEST_NAME) {
        Some(KartLevelRequestKind::PointClear)
    } else if hash == adler32::packet_hash(KART_LEVEL_SPECIAL_SLOT_UPDATE_REQUEST_NAME) {
        Some(KartLevelRequestKind::SpecialSlotUpdate)
    } else {
        None
    }
}

pub fn parse_kart_level_request(packet: &[u8]) -> Result<KartLevelRequest, KartLevelProtocolError> {
    let mut reader = PacketReader::new(packet);
    let hash = reader.read_u32()?;
    match classify_kart_level_request(hash) {
        Some(KartLevelRequestKind::LevelUpProbability) => Ok(KartLevelRequest::LevelUpProbability(
            parse_kart_level_up_probability_request(packet)?,
        )),
        Some(KartLevelRequestKind::LevelUp) => Ok(KartLevelRequest::LevelUp(
            parse_kart_level_up_request(packet)?,
        )),
        Some(KartLevelRequestKind::PointUpdate) => Ok(KartLevelRequest::PointUpdate(
            parse_kart_level_point_update_request(packet)?,
        )),
        Some(KartLevelRequestKind::PointClear) => Ok(KartLevelRequest::PointClear(
            parse_kart_level_point_clear_request(packet)?,
        )),
        Some(KartLevelRequestKind::SpecialSlotUpdate) => Ok(KartLevelRequest::SpecialSlotUpdate(
            parse_kart_level_special_slot_update_request(packet)?,
        )),
        None => Err(KartLevelProtocolError::UnknownRequestHash(hash)),
    }
}

pub fn parse_kart_level_up_probability_request(
    packet: &[u8],
) -> Result<KartLevelUpProbabilityRequest, KartLevelProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, KART_LEVEL_UP_PROBABILITY_REQUEST_NAME)?;
    let request = KartLevelUpProbabilityRequest {
        kart: read_kart_instance(&mut reader)?,
        donor: read_kart_instance(&mut reader)?,
    };
    ensure_exhausted(&reader, KART_LEVEL_UP_PROBABILITY_REQUEST_NAME)?;
    Ok(request)
}

pub fn parse_kart_level_up_request(
    packet: &[u8],
) -> Result<KartLevelUpRequest, KartLevelProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, KART_LEVEL_UP_REQUEST_NAME)?;
    let request = KartLevelUpRequest {
        kart: read_kart_instance(&mut reader)?,
        donor: read_kart_instance(&mut reader)?,
        cost: reader.read_i32()?,
    };
    ensure_exhausted(&reader, KART_LEVEL_UP_REQUEST_NAME)?;
    Ok(request)
}

pub fn parse_kart_level_point_update_request(
    packet: &[u8],
) -> Result<KartLevelPointUpdateRequest, KartLevelProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, KART_LEVEL_POINT_UPDATE_REQUEST_NAME)?;
    let request = KartLevelPointUpdateRequest {
        kart: read_kart_instance(&mut reader)?,
        level_deltas: [
            reader.read_i16()?,
            reader.read_i16()?,
            reader.read_i16()?,
            reader.read_i16()?,
        ],
    };
    ensure_exhausted(&reader, KART_LEVEL_POINT_UPDATE_REQUEST_NAME)?;
    Ok(request)
}

pub fn parse_kart_level_point_clear_request(
    packet: &[u8],
) -> Result<KartLevelPointClearRequest, KartLevelProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, KART_LEVEL_POINT_CLEAR_REQUEST_NAME)?;
    let request = KartLevelPointClearRequest {
        kart: read_kart_instance(&mut reader)?,
    };
    ensure_exhausted(&reader, KART_LEVEL_POINT_CLEAR_REQUEST_NAME)?;
    Ok(request)
}

pub fn parse_kart_level_special_slot_update_request(
    packet: &[u8],
) -> Result<KartLevelSpecialSlotUpdateRequest, KartLevelProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, KART_LEVEL_SPECIAL_SLOT_UPDATE_REQUEST_NAME)?;
    let request = KartLevelSpecialSlotUpdateRequest {
        kart: read_kart_instance(&mut reader)?,
        effect: reader.read_i16()?,
    };
    ensure_exhausted(&reader, KART_LEVEL_SPECIAL_SLOT_UPDATE_REQUEST_NAME)?;
    Ok(request)
}

/// Serializes the accepted result code used by the retained P5136 C# handler.
///
/// This `0` must not be replaced by the server's configured success percentage:
/// the client passes the field into its kart-level probability UI as a result
/// value, where `100` is outside the expected domain and terminates the client.
/// The retained C# response contains no other wire fields.
#[must_use]
pub fn serialize_kart_level_up_probability_success() -> Vec<u8> {
    serialize_kart_level_up_probability_result(PROBABILITY_REPLY_SUCCESS)
}

/// Serializes the same single-i32 response with a caller-selected non-zero
/// compatibility error code.
#[must_use]
pub fn serialize_kart_level_up_probability_failure(error_code: i32) -> Vec<u8> {
    debug_assert_ne!(error_code, PROBABILITY_REPLY_SUCCESS);
    serialize_kart_level_up_probability_result(error_code)
}

/// Serializes the stock response state followed by the three zero consumed-item
/// fields. They are category, item ID, and serial; zeroing them is compatible
/// with the server policy that validates but does not consume the donor kart.
#[must_use]
pub fn serialize_kart_level_up_success(
    time: LegacyTime,
    state: KartLevelState,
    koin: u32,
    lucci: u32,
) -> Vec<u8> {
    serialize_kart_level_up_reply(time, STATE_REPLY_SUCCESS, state, koin, lucci)
}

/// Serializes an inferred fixed-width failure using the same fields as the
/// C# success response and result `0`. Supplying the current state lets the
/// client retain a coherent snapshot after a rejected mutation.
#[must_use]
pub fn serialize_kart_level_up_failure(
    time: LegacyTime,
    current_state: KartLevelState,
    koin: u32,
    lucci: u32,
) -> Vec<u8> {
    serialize_kart_level_up_reply(time, STATE_REPLY_FAILURE, current_state, koin, lucci)
}

#[must_use]
pub fn serialize_kart_level_point_update_success(state: KartLevelState) -> Vec<u8> {
    serialize_state_reply(
        KART_LEVEL_POINT_UPDATE_REPLY_NAME,
        STATE_REPLY_SUCCESS,
        state,
    )
}

#[must_use]
pub fn serialize_kart_level_point_update_failure(current_state: KartLevelState) -> Vec<u8> {
    serialize_state_reply(
        KART_LEVEL_POINT_UPDATE_REPLY_NAME,
        STATE_REPLY_FAILURE,
        current_state,
    )
}

#[must_use]
pub fn serialize_kart_level_point_clear_success(state: KartLevelState, koin: u32) -> Vec<u8> {
    serialize_state_and_koin_reply(
        KART_LEVEL_POINT_CLEAR_REPLY_NAME,
        STATE_REPLY_SUCCESS,
        state,
        koin,
    )
}

#[must_use]
pub fn serialize_kart_level_point_clear_failure(
    current_state: KartLevelState,
    koin: u32,
) -> Vec<u8> {
    serialize_state_and_koin_reply(
        KART_LEVEL_POINT_CLEAR_REPLY_NAME,
        STATE_REPLY_FAILURE,
        current_state,
        koin,
    )
}

#[must_use]
pub fn serialize_kart_level_special_slot_update_success(state: KartLevelState) -> Vec<u8> {
    serialize_state_reply(
        KART_LEVEL_SPECIAL_SLOT_UPDATE_REPLY_NAME,
        STATE_REPLY_SUCCESS,
        state,
    )
}

#[must_use]
pub fn serialize_kart_level_special_slot_update_failure(current_state: KartLevelState) -> Vec<u8> {
    serialize_state_reply(
        KART_LEVEL_SPECIAL_SLOT_UPDATE_REPLY_NAME,
        STATE_REPLY_FAILURE,
        current_state,
    )
}

fn serialize_kart_level_up_probability_result(result: i32) -> Vec<u8> {
    let mut packet = PacketWriter::named(KART_LEVEL_UP_PROBABILITY_REPLY_NAME);
    packet.write_i32(result);
    packet.into_inner()
}

fn serialize_kart_level_up_reply(
    time: LegacyTime,
    result: i32,
    state: KartLevelState,
    koin: u32,
    lucci: u32,
) -> Vec<u8> {
    let mut packet = PacketWriter::named(KART_LEVEL_UP_REPLY_NAME);
    write_legacy_time(&mut packet, time);
    packet.write_i32(result);
    write_state(&mut packet, state);
    packet.write_i16(0); // consumed item category
    packet.write_i16(0); // consumed item ID
    packet.write_i16(0); // consumed item serial
    packet.write_u32(koin);
    packet.write_u32(lucci);
    packet.write_i32(0);
    packet.into_inner()
}

fn serialize_state_reply(name: &'static str, result: i32, state: KartLevelState) -> Vec<u8> {
    let mut packet = PacketWriter::named(name);
    packet.write_i32(result);
    write_state(&mut packet, state);
    packet.into_inner()
}

fn serialize_state_and_koin_reply(
    name: &'static str,
    result: i32,
    state: KartLevelState,
    koin: u32,
) -> Vec<u8> {
    let mut packet = PacketWriter::named(name);
    packet.write_i32(result);
    write_state(&mut packet, state);
    packet.write_u32(koin);
    packet.into_inner()
}

fn read_kart_instance(
    reader: &mut PacketReader<'_>,
) -> Result<KartInstance, KartLevelProtocolError> {
    Ok(KartInstance {
        kart_id: reader.read_i16()?,
        serial: reader.read_i16()?,
    })
}

fn write_state_without_effect(packet: &mut PacketWriter, state: KartLevelState) {
    packet.write_i16(state.kart_id);
    packet.write_i16(state.serial);
    packet.write_i16(state.grade);
    packet.write_i16(state.points);
    packet.write_i16(state.level1);
    packet.write_i16(state.level2);
    packet.write_i16(state.level3);
    packet.write_i16(state.level4);
}

fn write_state(packet: &mut PacketWriter, state: KartLevelState) {
    write_state_without_effect(packet, state);
    packet.write_i16(state.effect);
}

fn write_legacy_time(packet: &mut PacketWriter, time: LegacyTime) {
    packet.write_u16(time.days_since_1900);
    packet.write_u16(time.quarter_seconds);
}

fn expect_hash(
    reader: &mut PacketReader<'_>,
    name: &'static str,
) -> Result<(), KartLevelProtocolError> {
    let actual = reader.read_u32()?;
    let expected = adler32::packet_hash(name);
    if actual == expected {
        Ok(())
    } else {
        Err(KartLevelProtocolError::UnexpectedPacketHash {
            name,
            expected,
            actual,
        })
    }
}

fn ensure_exhausted(
    reader: &PacketReader<'_>,
    name: &'static str,
) -> Result<(), KartLevelProtocolError> {
    if reader.remaining().is_empty() {
        Ok(())
    } else {
        Err(KartLevelProtocolError::TrailingBytes {
            name,
            count: reader.remaining().len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        KART_LEVEL_POINT_CLEAR_REQUEST_NAME, KART_LEVEL_POINT_CLEAR_REQUEST_WIRE_LENGTH,
        KART_LEVEL_POINT_UPDATE_REQUEST_NAME, KART_LEVEL_POINT_UPDATE_REQUEST_WIRE_LENGTH,
        KART_LEVEL_SPECIAL_SLOT_UPDATE_REQUEST_NAME,
        KART_LEVEL_SPECIAL_SLOT_UPDATE_REQUEST_WIRE_LENGTH, KART_LEVEL_UP_PROBABILITY_REQUEST_NAME,
        KART_LEVEL_UP_PROBABILITY_REQUEST_WIRE_LENGTH, KART_LEVEL_UP_REQUEST_NAME,
        KART_LEVEL_UP_REQUEST_WIRE_LENGTH, KartInstance, KartLevelPointClearRequest,
        KartLevelPointUpdateRequest, KartLevelProtocolError, KartLevelRequest,
        KartLevelRequestKind, KartLevelSpecialSlotUpdateRequest, KartLevelState,
        KartLevelUpProbabilityRequest, KartLevelUpRequest, classify_kart_level_request,
        parse_kart_level_point_clear_request, parse_kart_level_point_update_request,
        parse_kart_level_request, parse_kart_level_special_slot_update_request,
        parse_kart_level_up_probability_request, parse_kart_level_up_request,
        serialize_kart_level_point_clear_failure, serialize_kart_level_point_clear_success,
        serialize_kart_level_point_update_failure, serialize_kart_level_point_update_success,
        serialize_kart_level_special_slot_update_failure,
        serialize_kart_level_special_slot_update_success, serialize_kart_level_up_failure,
        serialize_kart_level_up_probability_failure, serialize_kart_level_up_probability_success,
        serialize_kart_level_up_success,
    };
    use crate::{adler32, login::LegacyTime};

    const TARGET: KartInstance = KartInstance {
        kart_id: 1_031,
        serial: 1,
    };
    const DONOR: KartInstance = KartInstance {
        kart_id: 1_302,
        serial: 1,
    };
    const STATE: KartLevelState = KartLevelState {
        kart_id: 1_031,
        serial: 1,
        grade: 5,
        points: 25,
        level1: 1,
        level2: 2,
        level3: 3,
        level4: 4,
        effect: 9,
    };

    #[test]
    fn classifier_uses_exact_p5136_hashes() {
        let cases = [
            (
                KART_LEVEL_UP_PROBABILITY_REQUEST_NAME,
                0x5938_0848,
                KartLevelRequestKind::LevelUpProbability,
            ),
            (
                KART_LEVEL_UP_REQUEST_NAME,
                0x22B3_0510,
                KartLevelRequestKind::LevelUp,
            ),
            (
                KART_LEVEL_POINT_UPDATE_REQUEST_NAME,
                0x627D_08B8,
                KartLevelRequestKind::PointUpdate,
            ),
            (
                KART_LEVEL_POINT_CLEAR_REQUEST_NAME,
                0x595C_083C,
                KartLevelRequestKind::PointClear,
            ),
            (
                KART_LEVEL_SPECIAL_SLOT_UPDATE_REQUEST_NAME,
                0x9E37_0B11,
                KartLevelRequestKind::SpecialSlotUpdate,
            ),
        ];
        for (name, hash, kind) in cases {
            assert_eq!(adler32::packet_hash(name), hash);
            assert_eq!(classify_kart_level_request(hash), Some(kind));
        }
        assert_eq!(classify_kart_level_request(0xDEAD_BEEF), None);
    }

    #[test]
    fn captured_probability_query_decodes_exactly() {
        let packet = decode_hex("480838590704010016050100");
        assert_eq!(packet.len(), KART_LEVEL_UP_PROBABILITY_REQUEST_WIRE_LENGTH);
        let expected = KartLevelUpProbabilityRequest {
            kart: TARGET,
            donor: DONOR,
        };
        assert_eq!(
            parse_kart_level_up_probability_request(&packet).unwrap(),
            expected
        );
        assert_eq!(
            parse_kart_level_request(&packet).unwrap(),
            KartLevelRequest::LevelUpProbability(expected)
        );
    }

    #[test]
    fn level_up_request_decodes_target_donor_and_cost() {
        let packet = decode_hex("1005B322070401001605010044332211");
        assert_eq!(packet.len(), KART_LEVEL_UP_REQUEST_WIRE_LENGTH);
        let expected = KartLevelUpRequest {
            kart: TARGET,
            donor: DONOR,
            cost: 0x1122_3344,
        };
        assert_eq!(parse_kart_level_up_request(&packet).unwrap(), expected);
        assert_eq!(
            parse_kart_level_request(&packet).unwrap(),
            KartLevelRequest::LevelUp(expected)
        );
    }

    #[test]
    fn point_update_request_preserves_signed_additive_deltas() {
        let packet = decode_hex("B8087D620704010001000200FFFF0400");
        assert_eq!(packet.len(), KART_LEVEL_POINT_UPDATE_REQUEST_WIRE_LENGTH);
        let expected = KartLevelPointUpdateRequest {
            kart: TARGET,
            level_deltas: [1, 2, -1, 4],
        };
        assert_eq!(
            parse_kart_level_point_update_request(&packet).unwrap(),
            expected
        );
        assert_eq!(
            parse_kart_level_request(&packet).unwrap(),
            KartLevelRequest::PointUpdate(expected)
        );
    }

    #[test]
    fn clear_and_special_slot_requests_decode_exactly() {
        let clear = decode_hex("3C085C5907040100");
        assert_eq!(clear.len(), KART_LEVEL_POINT_CLEAR_REQUEST_WIRE_LENGTH);
        let clear_expected = KartLevelPointClearRequest { kart: TARGET };
        assert_eq!(
            parse_kart_level_point_clear_request(&clear).unwrap(),
            clear_expected
        );
        assert_eq!(
            parse_kart_level_request(&clear).unwrap(),
            KartLevelRequest::PointClear(clear_expected)
        );

        let special = decode_hex("110B379E070401000900");
        assert_eq!(
            special.len(),
            KART_LEVEL_SPECIAL_SLOT_UPDATE_REQUEST_WIRE_LENGTH
        );
        let special_expected = KartLevelSpecialSlotUpdateRequest {
            kart: TARGET,
            effect: 9,
        };
        assert_eq!(
            parse_kart_level_special_slot_update_request(&special).unwrap(),
            special_expected
        );
        assert_eq!(
            parse_kart_level_request(&special).unwrap(),
            KartLevelRequest::SpecialSlotUpdate(special_expected)
        );
    }

    #[test]
    fn every_fixed_width_parser_rejects_truncation_and_trailing_bytes() {
        let packets = [
            decode_hex("480838590704010016050100"),
            decode_hex("1005B322070401001605010044332211"),
            decode_hex("B8087D62070401000100020003000400"),
            decode_hex("3C085C5907040100"),
            decode_hex("110B379E070401000900"),
        ];
        for packet in packets {
            assert!(parse_kart_level_request(&packet[..packet.len() - 1]).is_err());
            let mut trailing = packet;
            trailing.push(0xA5);
            assert!(matches!(
                parse_kart_level_request(&trailing),
                Err(KartLevelProtocolError::TrailingBytes { count: 1, .. })
            ));
        }
    }

    #[test]
    fn individual_parser_rejects_a_different_known_hash() {
        let packet = decode_hex("1005B322070401001605010044332211");
        assert!(matches!(
            parse_kart_level_up_probability_request(&packet),
            Err(KartLevelProtocolError::UnexpectedPacketHash {
                name: KART_LEVEL_UP_PROBABILITY_REQUEST_NAME,
                expected: 0x5938_0848,
                actual: 0x22B3_0510,
            })
        ));
        assert!(matches!(
            parse_kart_level_request(&decode_hex("EFBEADDE")),
            Err(KartLevelProtocolError::UnknownRequestHash(0xDEAD_BEEF))
        ));
    }

    #[test]
    fn probability_result_codes_match_the_csharp_fixed_shape() {
        assert_eq!(
            serialize_kart_level_up_probability_success(),
            decode_hex("49084C5900000000")
        );
        assert_eq!(
            serialize_kart_level_up_probability_failure(7),
            decode_hex("49084C5907000000")
        );
    }

    #[test]
    fn level_up_success_and_failure_match_the_csharp_fixed_shape() {
        let time = LegacyTime {
            days_since_1900: 0x1234,
            quarter_seconds: 0x5678,
        };
        assert_eq!(
            serialize_kart_level_up_success(time, STATE, 0x1122_3344, 0x5566_7788),
            decode_hex(concat!(
                "1105BF22",
                "34127856",
                "01000000",
                "07040100",
                "05001900",
                "0100020003000400",
                "0900000000000000",
                "44332211",
                "88776655",
                "00000000"
            ))
        );
        assert_eq!(
            serialize_kart_level_up_failure(time, STATE, 0x1122_3344, 0x5566_7788),
            decode_hex(concat!(
                "1105BF22",
                "34127856",
                "00000000",
                "07040100",
                "05001900",
                "0100020003000400",
                "0900000000000000",
                "44332211",
                "88776655",
                "00000000"
            ))
        );
    }

    #[test]
    fn state_mutation_replies_include_effect_and_use_one_or_zero_result() {
        assert_eq!(
            serialize_kart_level_point_update_success(STATE),
            decode_hex("B908926201000000070401000500190001000200030004000900")
        );
        assert_eq!(
            serialize_kart_level_point_update_failure(STATE),
            decode_hex("B908926200000000070401000500190001000200030004000900")
        );
        assert_eq!(
            serialize_kart_level_special_slot_update_success(STATE),
            decode_hex("120B529E01000000070401000500190001000200030004000900")
        );
        assert_eq!(
            serialize_kart_level_special_slot_update_failure(STATE),
            decode_hex("120B529E00000000070401000500190001000200030004000900")
        );
    }

    #[test]
    fn clear_reply_appends_koin_after_the_shared_state() {
        assert_eq!(
            serialize_kart_level_point_clear_success(STATE, 0x1122_3344),
            decode_hex("3D0870590100000007040100050019000100020003000400090044332211")
        );
        assert_eq!(
            serialize_kart_level_point_clear_failure(STATE, 0x1122_3344),
            decode_hex("3D0870590000000007040100050019000100020003000400090044332211")
        );
        assert_eq!(STATE.kart(), TARGET);
    }

    fn decode_hex(input: &str) -> Vec<u8> {
        input
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }
}
