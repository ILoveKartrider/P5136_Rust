//! P5136 rider-info packet primitives.
//!
//! Stock producer evidence establishes one exact `PqGetRiderInfo` form: a
//! zero scalar, an empty reserved UTF-16 string, a bounded target nickname,
//! and one raw mode byte. This module accepts only that producer-minted form.
//! The successful reply layout is corroborated by the Korean C# compatibility
//! handler and the 199-byte `PrGetRiderInfo` capture from 2026-07-15.

use thiserror::Error;

use crate::{
    login::LegacyTime,
    packet::{PacketError, PacketReader, PacketWriter},
    room_protocol::MAX_RIDER_NICKNAME_UTF16_UNITS,
    startup::RIDER_ITEM_SNAPSHOT_WIRE_LENGTH,
};

pub const GET_RIDER_INFO_REQUEST_NAME: &str = "PqGetRiderInfo";
pub const GET_RIDER_INFO_REPLY_NAME: &str = "PrGetRiderInfo";

pub const GET_RIDER_INFO_REQUEST_HASH: u32 = 0x2777_0563;
pub const GET_RIDER_INFO_REPLY_HASH: u32 = 0x2784_0564;

/// Profile-backed fields in a successful Korean P5136 rider-info reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiderInfoFields {
    pub user_no: u32,
    pub account_name: String,
    pub nickname: String,
    pub profile_time: LegacyTime,
    pub rider_item_snapshot: [u8; RIDER_ITEM_SNAPSHOT_WIRE_LENGTH],
    pub card: String,
    pub rp: u32,
    pub license_level: u8,
    pub emblem_1: i16,
    pub emblem_2: i16,
    pub rider_intro: String,
    pub premium: i32,
    pub premium_points: i32,
    pub club_code: i32,
    pub club_mark_logo: i32,
    pub club_mark_line: i32,
    pub club_name: String,
    pub ranker: u8,
}

/// A fully validated, exactly consumed stock `PqGetRiderInfo` request.
///
/// Fields stay private so only this module's exact parser can mint a request.
/// `Debug` is deliberately not implemented because the target nickname is
/// sensitive log data.
pub struct ParsedGetRiderInfoRequest {
    target_nickname: String,
    mode: u8,
}

impl ParsedGetRiderInfoRequest {
    #[must_use]
    pub fn target_nickname(&self) -> &str {
        &self.target_nickname
    }

    #[must_use]
    pub const fn mode(&self) -> u8 {
        self.mode
    }
}

#[derive(Debug, Error)]
pub enum RiderInfoProtocolError {
    #[error(transparent)]
    Packet(#[from] PacketError),

    #[error("expected {name} hash 0x{expected:08X}, received 0x{actual:08X}")]
    UnexpectedPacketHash {
        name: &'static str,
        expected: u32,
        actual: u32,
    },

    #[error("PqGetRiderInfo producer scalar must be zero, received {actual}")]
    NonZeroProducerScalar { actual: u32 },

    #[error("PqGetRiderInfo reserved UTF-16 length is negative: {actual}")]
    NegativeReservedStringLength { actual: i32 },

    #[error(
        "PqGetRiderInfo reserved UTF-16 string must be empty, received {utf16_units} code units"
    )]
    NonEmptyReservedString { utf16_units: i32 },

    #[error("packet {name} has {count} unexpected trailing bytes")]
    TrailingBytes { name: &'static str, count: usize },
}

/// Parses only the exact request form emitted by the stock P5136 producer.
///
/// The raw mode byte has no evidenced business meaning or restricted range,
/// so every `u8` value is preserved. The target is bounded before allocation,
/// and all input must be consumed.
pub fn parse_get_rider_info_request(
    packet: &[u8],
) -> Result<ParsedGetRiderInfoRequest, RiderInfoProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader)?;

    let scalar = reader.read_u32()?;
    if scalar != 0 {
        return Err(RiderInfoProtocolError::NonZeroProducerScalar { actual: scalar });
    }

    let reserved_length = reader.read_i32()?;
    if reserved_length < 0 {
        return Err(RiderInfoProtocolError::NegativeReservedStringLength {
            actual: reserved_length,
        });
    }
    if reserved_length != 0 {
        return Err(RiderInfoProtocolError::NonEmptyReservedString {
            utf16_units: reserved_length,
        });
    }

    let target_nickname = reader.read_utf16_bounded(MAX_RIDER_NICKNAME_UTF16_UNITS)?;
    let mode = reader.read_u8()?;
    ensure_exhausted(&reader)?;

    Ok(ParsedGetRiderInfoRequest {
        target_nickname,
        mode,
    })
}

/// Serializes the exact fail-closed `PrGetRiderInfo` reply.
#[must_use]
pub fn serialize_get_rider_info_failure() -> Vec<u8> {
    let mut packet = PacketWriter::named(GET_RIDER_INFO_REPLY_NAME);
    packet.write_u8(0);
    packet.into_inner()
}

/// Serializes the exact successful `PrGetRiderInfo` projection consumed by
/// the Korean P5136 client.
pub fn serialize_get_rider_info_success(fields: &RiderInfoFields) -> Result<Vec<u8>, PacketError> {
    let mut packet = PacketWriter::named(GET_RIDER_INFO_REPLY_NAME);
    packet.write_u8(1);
    packet.write_u32(fields.user_no);
    packet.write_utf16(&fields.account_name)?;
    packet.write_utf16(&fields.nickname)?;
    write_legacy_time(&mut packet, fields.profile_time);
    packet.write_bytes(&fields.rider_item_snapshot);
    packet.write_utf16(&fields.card)?;
    packet.write_u32(fields.rp);
    packet.write_i32(0);
    packet.write_u8(fields.license_level);
    write_legacy_time(&mut packet, fields.profile_time);
    packet.write_bytes(&[0; 17]);
    packet.write_i16(fields.emblem_1);
    packet.write_i16(fields.emblem_2);
    packet.write_i16(0);
    packet.write_utf16(&fields.rider_intro)?;
    packet.write_i32(fields.premium);
    packet.write_u8(1);
    packet.write_i32(fields.premium_points);
    if fields.club_mark_logo == 0 {
        packet.write_i32(0);
        packet.write_i32(0);
        packet.write_i32(0);
        packet.write_utf16("")?;
    } else {
        packet.write_i32(fields.club_code);
        packet.write_i32(fields.club_mark_logo);
        packet.write_i32(fields.club_mark_line);
        packet.write_utf16(&fields.club_name)?;
    }
    packet.write_i32(0);
    packet.write_u8(fields.ranker);
    for _ in 0..5 {
        packet.write_i32(0);
    }
    packet.write_bytes(&[0; 3]);
    Ok(packet.into_inner())
}

fn write_legacy_time(packet: &mut PacketWriter, time: LegacyTime) {
    packet.write_u16(time.days_since_1900);
    packet.write_u16(time.quarter_seconds);
}

fn expect_hash(reader: &mut PacketReader<'_>) -> Result<(), RiderInfoProtocolError> {
    let actual = reader.read_u32()?;
    if actual == GET_RIDER_INFO_REQUEST_HASH {
        Ok(())
    } else {
        Err(RiderInfoProtocolError::UnexpectedPacketHash {
            name: GET_RIDER_INFO_REQUEST_NAME,
            expected: GET_RIDER_INFO_REQUEST_HASH,
            actual,
        })
    }
}

fn ensure_exhausted(reader: &PacketReader<'_>) -> Result<(), RiderInfoProtocolError> {
    if reader.remaining().is_empty() {
        Ok(())
    } else {
        Err(RiderInfoProtocolError::TrailingBytes {
            name: GET_RIDER_INFO_REQUEST_NAME,
            count: reader.remaining().len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        GET_RIDER_INFO_REPLY_HASH, GET_RIDER_INFO_REPLY_NAME, GET_RIDER_INFO_REQUEST_HASH,
        GET_RIDER_INFO_REQUEST_NAME, RiderInfoFields, RiderInfoProtocolError,
        parse_get_rider_info_request, serialize_get_rider_info_failure,
        serialize_get_rider_info_success,
    };
    use crate::{
        adler32,
        login::LegacyTime,
        packet::{PacketError, PacketWriter},
        room_protocol::MAX_RIDER_NICKNAME_UTF16_UNITS,
        startup::RIDER_ITEM_SNAPSHOT_WIRE_LENGTH,
    };

    fn request_fixture(target_nickname: &str, mode: u8) -> Vec<u8> {
        let mut packet = PacketWriter::named(GET_RIDER_INFO_REQUEST_NAME);
        packet.write_u32(0);
        packet.write_i32(0);
        packet
            .write_utf16(target_nickname)
            .expect("test string fits");
        packet.write_u8(mode);
        packet.into_inner()
    }

    #[test]
    fn packet_names_match_the_exact_p5136_hashes() {
        assert_eq!(
            adler32::packet_hash(GET_RIDER_INFO_REQUEST_NAME),
            GET_RIDER_INFO_REQUEST_HASH
        );
        assert_eq!(
            adler32::packet_hash(GET_RIDER_INFO_REPLY_NAME),
            GET_RIDER_INFO_REPLY_HASH
        );
    }

    #[test]
    fn golden_stock_request_parses_without_exposing_reserved_fields() {
        let packet = [
            0x63, 0x05, 0x77, 0x27, // PqGetRiderInfo hash
            0x00, 0x00, 0x00, 0x00, // producer scalar = 0
            0x00, 0x00, 0x00, 0x00, // reserved string = empty
            0x03, 0x00, 0x00, 0x00, // target UTF-16 length
            0x41, 0x00, 0x3D, 0xD8, 0xCE, 0xDF, // "A🟎"
            0xFF, // raw mode
        ];

        let parsed = parse_get_rider_info_request(&packet).expect("golden request");
        assert!(parsed.target_nickname() == "A🟎");
        assert!(parsed.mode() == u8::MAX);
    }

    #[test]
    fn every_truncated_prefix_of_an_exact_request_is_rejected() {
        let packet = request_fixture("Rider", 1);
        for length in 0..packet.len() {
            assert!(
                matches!(
                    parse_get_rider_info_request(&packet[..length]),
                    Err(RiderInfoProtocolError::Packet(
                        PacketError::Truncated { .. }
                    ))
                ),
                "prefix {length} of {} unexpectedly parsed",
                packet.len()
            );
        }
    }

    #[test]
    fn wrong_hash_is_rejected_before_body_parsing() {
        let mut packet = request_fixture("Rider", 0);
        let actual = GET_RIDER_INFO_REPLY_HASH;
        packet[..4].copy_from_slice(&actual.to_le_bytes());

        assert!(matches!(
            parse_get_rider_info_request(&packet),
            Err(RiderInfoProtocolError::UnexpectedPacketHash {
                expected: GET_RIDER_INFO_REQUEST_HASH,
                actual: received,
                ..
            }) if received == actual
        ));
    }

    #[test]
    fn nonzero_producer_scalar_is_rejected() {
        let mut packet = request_fixture("Rider", 0);
        packet[4..8].copy_from_slice(&7_u32.to_le_bytes());

        assert!(matches!(
            parse_get_rider_info_request(&packet),
            Err(RiderInfoProtocolError::NonZeroProducerScalar { actual: 7 })
        ));
    }

    #[test]
    fn negative_and_nonempty_reserved_lengths_have_distinct_errors() {
        let mut negative = request_fixture("Rider", 0);
        negative[8..12].copy_from_slice(&(-1_i32).to_le_bytes());
        assert!(matches!(
            parse_get_rider_info_request(&negative),
            Err(RiderInfoProtocolError::NegativeReservedStringLength { actual: -1 })
        ));

        let mut nonempty = request_fixture("Rider", 0);
        nonempty[8..12].copy_from_slice(&1_i32.to_le_bytes());
        assert!(matches!(
            parse_get_rider_info_request(&nonempty),
            Err(RiderInfoProtocolError::NonEmptyReservedString { utf16_units: 1 })
        ));
    }

    #[test]
    fn target_limit_counts_utf16_units_and_rejects_overlong_values() {
        let maximum = "x".repeat(MAX_RIDER_NICKNAME_UTF16_UNITS);
        let parsed =
            parse_get_rider_info_request(&request_fixture(&maximum, 0)).expect("maximum target");
        assert!(parsed.target_nickname() == maximum);

        let surrogate_pairs = "🟎".repeat(MAX_RIDER_NICKNAME_UTF16_UNITS / 2);
        let parsed = parse_get_rider_info_request(&request_fixture(&surrogate_pairs, 0))
            .expect("maximum surrogate-pair target");
        assert!(parsed.target_nickname() == surrogate_pairs);

        let overlong = "x".repeat(MAX_RIDER_NICKNAME_UTF16_UNITS + 1);
        assert!(matches!(
            parse_get_rider_info_request(&request_fixture(&overlong, 0)),
            Err(RiderInfoProtocolError::Packet(
                PacketError::StringLimitExceeded {
                    length,
                    maximum
                }
            )) if length == MAX_RIDER_NICKNAME_UTF16_UNITS + 1
                && maximum == MAX_RIDER_NICKNAME_UTF16_UNITS
        ));
    }

    #[test]
    fn mode_preserves_the_full_u8_domain() {
        for mode in [0, 1, 127, 254, u8::MAX] {
            let parsed =
                parse_get_rider_info_request(&request_fixture("Rider", mode)).expect("raw mode");
            assert!(parsed.mode() == mode);
        }
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut packet = request_fixture("Rider", 0);
        packet.extend_from_slice(&[0xAA, 0xBB]);

        assert!(matches!(
            parse_get_rider_info_request(&packet),
            Err(RiderInfoProtocolError::TrailingBytes {
                name: GET_RIDER_INFO_REQUEST_NAME,
                count: 2
            })
        ));
    }

    #[test]
    fn failure_response_matches_the_exact_five_byte_layout() {
        let response = serialize_get_rider_info_failure();
        assert_eq!(response, [0x64, 0x05, 0x84, 0x27, 0x00]);
        assert_eq!(response.len(), 5);
    }

    #[test]
    fn successful_response_matches_the_csharp_layout_and_199_byte_capture_shape() {
        let response = serialize_get_rider_info_success(&RiderInfoFields {
            user_no: 5,
            account_name: "Yany".to_owned(),
            nickname: "Yany".to_owned(),
            profile_time: LegacyTime {
                days_since_1900: 0xB488,
                quarter_seconds: 0x503D,
            },
            rider_item_snapshot: [0; RIDER_ITEM_SNAPSHOT_WIRE_LENGTH],
            card: String::new(),
            rp: 0x7735_9417,
            license_level: 6,
            emblem_1: 0,
            emblem_2: 0,
            rider_intro: String::new(),
            premium: 5,
            premium_points: 200_000,
            club_code: 10_000,
            club_mark_logo: 0,
            club_mark_line: 0,
            club_name: "must be hidden without a logo".to_owned(),
            ranker: 0,
        })
        .unwrap();

        assert_eq!(response.len(), 199);
        assert_eq!(&response[..5], &[0x64, 0x05, 0x84, 0x27, 1]);
        assert_eq!(&response[5..9], &5_u32.to_le_bytes());
        assert_eq!(
            &response[9..21],
            &[4, 0, 0, 0, 0x59, 0, 0x61, 0, 0x6E, 0, 0x79, 0]
        );
        assert_eq!(&response[21..33], &response[9..21]);
        assert_eq!(&response[33..37], &[0x88, 0xB4, 0x3D, 0x50]);
        assert!(
            response[37..37 + RIDER_ITEM_SNAPSHOT_WIRE_LENGTH]
                .iter()
                .all(|byte| *byte == 0)
        );
        assert_eq!(&response[102..106], &[0; 4]);
        assert_eq!(&response[106..110], &0x7735_9417_u32.to_le_bytes());
        assert_eq!(response[114], 6);
        assert_eq!(&response[115..119], &[0x88, 0xB4, 0x3D, 0x50]);
        assert_eq!(&response[146..150], &5_i32.to_le_bytes());
        assert_eq!(response[150], 1);
        assert_eq!(&response[151..155], &200_000_i32.to_le_bytes());
        assert!(response[155..].iter().all(|byte| *byte == 0));
    }
}
