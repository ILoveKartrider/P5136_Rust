//! Fail-closed P5136 rider-info packet primitives.
//!
//! Stock producer evidence establishes one exact `PqGetRiderInfo` form: a
//! zero scalar, an empty reserved UTF-16 string, a bounded target nickname,
//! and one raw mode byte. This module accepts only that producer-minted form
//! and intentionally exposes no successful `PrGetRiderInfo` DTO until the
//! cross-profile authorization and data-projection policy is defined.

use thiserror::Error;

use crate::{
    packet::{PacketError, PacketReader, PacketWriter},
    room_protocol::MAX_RIDER_NICKNAME_UTF16_UNITS,
};

pub const GET_RIDER_INFO_REQUEST_NAME: &str = "PqGetRiderInfo";
pub const GET_RIDER_INFO_REPLY_NAME: &str = "PrGetRiderInfo";

pub const GET_RIDER_INFO_REQUEST_HASH: u32 = 0x2777_0563;
pub const GET_RIDER_INFO_REPLY_HASH: u32 = 0x2784_0564;

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
        GET_RIDER_INFO_REQUEST_NAME, RiderInfoProtocolError, parse_get_rider_info_request,
        serialize_get_rider_info_failure,
    };
    use crate::{
        adler32,
        packet::{PacketError, PacketWriter},
        room_protocol::MAX_RIDER_NICKNAME_UTF16_UNITS,
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
}
