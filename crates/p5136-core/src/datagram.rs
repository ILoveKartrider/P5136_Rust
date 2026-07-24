//! P5136 game/P2P UDP envelope.
//!
//! Unlike login TCP frames, a datagram carries its random IV in cleartext and
//! does not maintain per-peer IV state:
//!
//! `iv:u32 | encrypted logical payload | encoded checksum:u32`

use thiserror::Error;

use crate::crypto;

pub const DATAGRAM_OVERHEAD: usize = 8;
pub const ROUTED_HEADER_LENGTH: usize = 12;
pub const DATAGRAM_CHECKSUM_XOR: u32 = 1_329_075_907;
pub const DEFAULT_MAX_DATAGRAM_PAYLOAD: usize = 65_499;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DatagramError {
    #[error("datagram payload length {length} exceeds configured maximum {maximum}")]
    PayloadTooLarge { length: usize, maximum: usize },

    #[error("datagram is truncated: expected at least {minimum} bytes, received {actual}")]
    Truncated { minimum: usize, actual: usize },

    #[error("datagram checksum mismatch")]
    ChecksumMismatch,

    #[error("logical routed payload is shorter than its 12-byte header: {actual} bytes")]
    RoutedHeaderTruncated { actual: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutedPayload<'a> {
    pub account_id: u32,
    pub route_hash: u32,
    pub packet_name: u32,
    pub body: &'a [u8],
}

pub fn encode_datagram(payload: &[u8], iv: u32, maximum: usize) -> Result<Vec<u8>, DatagramError> {
    validate_payload_length(payload.len(), maximum)?;

    let mut encrypted = payload.to_vec();
    let checksum = crypto::encrypt_in_place(&mut encrypted, iv);
    let encoded_checksum = iv ^ checksum ^ DATAGRAM_CHECKSUM_XOR;

    let mut wire = Vec::with_capacity(payload.len() + DATAGRAM_OVERHEAD);
    wire.extend_from_slice(&iv.to_le_bytes());
    wire.extend_from_slice(&encrypted);
    wire.extend_from_slice(&encoded_checksum.to_le_bytes());
    Ok(wire)
}

pub fn decode_datagram(wire: &[u8], maximum: usize) -> Result<(u32, Vec<u8>), DatagramError> {
    if wire.len() < DATAGRAM_OVERHEAD {
        return Err(DatagramError::Truncated {
            minimum: DATAGRAM_OVERHEAD,
            actual: wire.len(),
        });
    }

    let payload_length = wire.len() - DATAGRAM_OVERHEAD;
    validate_payload_length(payload_length, maximum)?;
    let iv = read_u32(wire, 0);
    let checksum_offset = wire.len() - 4;
    let encoded_checksum = read_u32(wire, checksum_offset);
    let mut payload = wire[4..checksum_offset].to_vec();
    let actual_checksum = crypto::decrypt_in_place(&mut payload, iv);
    let expected_checksum = iv ^ encoded_checksum ^ DATAGRAM_CHECKSUM_XOR;
    if actual_checksum != expected_checksum {
        return Err(DatagramError::ChecksumMismatch);
    }
    Ok((iv, payload))
}

pub fn parse_routed_payload(payload: &[u8]) -> Result<RoutedPayload<'_>, DatagramError> {
    if payload.len() < ROUTED_HEADER_LENGTH {
        return Err(DatagramError::RoutedHeaderTruncated {
            actual: payload.len(),
        });
    }
    Ok(RoutedPayload {
        account_id: read_u32(payload, 0),
        route_hash: read_u32(payload, 4),
        packet_name: read_u32(payload, 8),
        body: &payload[ROUTED_HEADER_LENGTH..],
    })
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn validate_payload_length(length: usize, maximum: usize) -> Result<(), DatagramError> {
    if length > maximum {
        Err(DatagramError::PayloadTooLarge { length, maximum })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_MAX_DATAGRAM_PAYLOAD, DatagramError, decode_datagram, encode_datagram,
        parse_routed_payload,
    };

    #[test]
    fn frame_matches_the_csharp_udp_golden() {
        let payload = (0_u8..21).collect::<Vec<_>>();
        let wire = encode_datagram(&payload, 0x5136_5136, DEFAULT_MAX_DATAGRAM_PAYLOAD).unwrap();
        assert_eq!(
            wire,
            [
                0x36, 0x51, 0x36, 0x51, 0xFE, 0x57, 0x87, 0x46, 0x9E, 0x46, 0x8F, 0xDA, 0xFF, 0xCF,
                0x3F, 0x7E, 0xFA, 0x75, 0x85, 0xAD, 0xEE, 0x47, 0x97, 0x56, 0x8E, 0x57, 0x46, 0x0E,
                0x1E,
            ]
        );

        let (iv, decoded) = decode_datagram(&wire, DEFAULT_MAX_DATAGRAM_PAYLOAD).unwrap();
        assert_eq!(iv, 0x5136_5136);
        assert_eq!(decoded, payload);
    }

    #[test]
    fn checksum_corruption_is_rejected() {
        let mut wire =
            encode_datagram(b"routed-payload", 0x1234_5678, DEFAULT_MAX_DATAGRAM_PAYLOAD).unwrap();
        wire[6] ^= 0x40;
        assert_eq!(
            decode_datagram(&wire, DEFAULT_MAX_DATAGRAM_PAYLOAD),
            Err(DatagramError::ChecksumMismatch)
        );
    }

    #[test]
    fn routed_header_is_parsed_without_copying_the_body() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&7_u32.to_le_bytes());
        payload.extend_from_slice(&0x1122_3344_u32.to_le_bytes());
        payload.extend_from_slice(&0xAABB_CCDD_u32.to_le_bytes());
        payload.extend_from_slice(b"body");

        let routed = parse_routed_payload(&payload).unwrap();
        assert_eq!(routed.account_id, 7);
        assert_eq!(routed.route_hash, 0x1122_3344);
        assert_eq!(routed.packet_name, 0xAABB_CCDD);
        assert_eq!(routed.body, b"body");
    }

    #[test]
    fn bounds_are_checked_before_copying() {
        assert_eq!(
            decode_datagram(&[0; 7], 100),
            Err(DatagramError::Truncated {
                minimum: 8,
                actual: 7,
            })
        );
        let oversized = [0_u8; 17];
        assert_eq!(
            decode_datagram(&oversized, 8),
            Err(DatagramError::PayloadTooLarge {
                length: 9,
                maximum: 8,
            })
        );
    }
}
