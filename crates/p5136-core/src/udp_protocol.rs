//! Modern P5136 UDP logical packet codecs.
//!
//! The UDP datagram encryption/checksum envelope lives in [`crate::datagram`].
//! This module handles the decrypted logical payload inside that envelope:
//!
//! `account_id:u32 | route_hash:u32 | packet_name:u32 | body`
//!
//! These layouts follow `KartRider.Data/Server/UdpServer.cs` from the P5136
//! server. They intentionally do not use the different 2005 legacy Echo and
//! `TimeSync` layouts.

use std::fmt;

use thiserror::Error;

use crate::datagram::{self, DEFAULT_MAX_DATAGRAM_PAYLOAD, DatagramError, ROUTED_HEADER_LENGTH};

pub const PQ_UDP_ECHO_HASH: u32 = 279_905_129;
pub const PR_UDP_ECHO_HASH: u32 = 280_429_418;
pub const PQ_UDP_TIME_SYNC_HASH: u32 = 584_516_886;
pub const PR_UDP_TIME_SYNC_HASH: u32 = 585_303_319;
pub const GAME_SLOT_PACKET_HASH: u32 = 666_895_732;
pub const ROOM_SLOT_PACKET_HASH: u32 = 696_255_895;

pub const UDP_ECHO_BODY_LENGTH: usize = 8;
pub const UDP_TIME_SYNC_REQUEST_BODY_LENGTH: usize = 4;
pub const UDP_TIME_SYNC_REPLY_BODY_LENGTH: usize = 8;
pub const MAX_UDP_RELAY_BODY_LENGTH: usize = DEFAULT_MAX_DATAGRAM_PAYLOAD - ROUTED_HEADER_LENGTH;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UdpPacketName {
    PqUdpEcho,
    PrUdpEcho,
    PqUdpTimeSync,
    PrUdpTimeSync,
    GameSlotPacket,
    RoomSlotPacket,
}

pub const UDP_PACKET_NAMES: &[UdpPacketName] = &[
    UdpPacketName::PqUdpEcho,
    UdpPacketName::PrUdpEcho,
    UdpPacketName::PqUdpTimeSync,
    UdpPacketName::PrUdpTimeSync,
    UdpPacketName::GameSlotPacket,
    UdpPacketName::RoomSlotPacket,
];

impl UdpPacketName {
    #[must_use]
    pub const fn hash(self) -> u32 {
        match self {
            Self::PqUdpEcho => PQ_UDP_ECHO_HASH,
            Self::PrUdpEcho => PR_UDP_ECHO_HASH,
            Self::PqUdpTimeSync => PQ_UDP_TIME_SYNC_HASH,
            Self::PrUdpTimeSync => PR_UDP_TIME_SYNC_HASH,
            Self::GameSlotPacket => GAME_SLOT_PACKET_HASH,
            Self::RoomSlotPacket => ROOM_SLOT_PACKET_HASH,
        }
    }

    #[must_use]
    pub const fn rtti_name(self) -> &'static str {
        match self {
            Self::PqUdpEcho => "PqUdpEcho",
            Self::PrUdpEcho => "PrUdpEcho",
            Self::PqUdpTimeSync => "PqUdpTimeSync",
            Self::PrUdpTimeSync => "PrUdpTimeSync",
            Self::GameSlotPacket => "GameSlotPacket",
            Self::RoomSlotPacket => "RoomSlotPacket",
        }
    }

    #[must_use]
    pub const fn exact_body_length(self) -> Option<usize> {
        match self {
            Self::PqUdpEcho | Self::PrUdpEcho => Some(UDP_ECHO_BODY_LENGTH),
            Self::PqUdpTimeSync => Some(UDP_TIME_SYNC_REQUEST_BODY_LENGTH),
            Self::PrUdpTimeSync => Some(UDP_TIME_SYNC_REPLY_BODY_LENGTH),
            Self::GameSlotPacket | Self::RoomSlotPacket => None,
        }
    }

    #[must_use]
    pub const fn is_relay(self) -> bool {
        matches!(self, Self::GameSlotPacket | Self::RoomSlotPacket)
    }
}

impl fmt::Display for UdpPacketName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.rtti_name())
    }
}

#[must_use]
pub const fn classify_udp_packet_name(hash: u32) -> Option<UdpPacketName> {
    match hash {
        PQ_UDP_ECHO_HASH => Some(UdpPacketName::PqUdpEcho),
        PR_UDP_ECHO_HASH => Some(UdpPacketName::PrUdpEcho),
        PQ_UDP_TIME_SYNC_HASH => Some(UdpPacketName::PqUdpTimeSync),
        PR_UDP_TIME_SYNC_HASH => Some(UdpPacketName::PrUdpTimeSync),
        GAME_SLOT_PACKET_HASH => Some(UdpPacketName::GameSlotPacket),
        ROOM_SLOT_PACKET_HASH => Some(UdpPacketName::RoomSlotPacket),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutedUdpHeader {
    pub account_id: u32,
    pub route_hash: u32,
    pub packet_name: UdpPacketName,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PqUdpEchoBody {
    pub value_1: i32,
    pub value_2: i32,
}

impl PqUdpEchoBody {
    /// The modern P5136 server echoes both signed values without alteration.
    #[must_use]
    pub const fn reply(self) -> PrUdpEchoBody {
        PrUdpEchoBody {
            value_1: self.value_1,
            value_2: self.value_2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrUdpEchoBody {
    pub value_1: i32,
    pub value_2: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PqUdpTimeSyncBody {
    pub client_tick: i32,
}

impl PqUdpTimeSyncBody {
    /// Pairs the original signed client tick with the current unsigned server
    /// tick, matching modern `UdpServer.cs`.
    #[must_use]
    pub const fn reply(self, server_tick: u32) -> PrUdpTimeSyncBody {
        PrUdpTimeSyncBody {
            client_tick: self.client_tick,
            server_tick,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrUdpTimeSyncBody {
    pub client_tick: i32,
    pub server_tick: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UdpLogicalBody<'a> {
    PqUdpEcho(PqUdpEchoBody),
    PrUdpEcho(PrUdpEchoBody),
    PqUdpTimeSync(PqUdpTimeSyncBody),
    PrUdpTimeSync(PrUdpTimeSyncBody),
    GameSlotPacket(&'a [u8]),
    RoomSlotPacket(&'a [u8]),
}

impl UdpLogicalBody<'_> {
    #[must_use]
    pub const fn packet_name(&self) -> UdpPacketName {
        match self {
            Self::PqUdpEcho(_) => UdpPacketName::PqUdpEcho,
            Self::PrUdpEcho(_) => UdpPacketName::PrUdpEcho,
            Self::PqUdpTimeSync(_) => UdpPacketName::PqUdpTimeSync,
            Self::PrUdpTimeSync(_) => UdpPacketName::PrUdpTimeSync,
            Self::GameSlotPacket(_) => UdpPacketName::GameSlotPacket,
            Self::RoomSlotPacket(_) => UdpPacketName::RoomSlotPacket,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutedUdpPacket<'a> {
    pub account_id: u32,
    pub route_hash: u32,
    pub body: UdpLogicalBody<'a>,
}

impl RoutedUdpPacket<'_> {
    #[must_use]
    pub const fn header(&self) -> RoutedUdpHeader {
        RoutedUdpHeader {
            account_id: self.account_id,
            route_hash: self.route_hash,
            packet_name: self.body.packet_name(),
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum UdpProtocolError {
    #[error(transparent)]
    Datagram(#[from] DatagramError),

    #[error("unknown P5136 UDP packet name hash 0x{0:08X}")]
    UnknownPacketName(u32),

    #[error(
        "{packet_name} body length mismatch: expected exactly {expected} bytes, received {actual}"
    )]
    BodyLengthMismatch {
        packet_name: UdpPacketName,
        expected: usize,
        actual: usize,
    },

    #[error("{packet_name} relay body length {length} exceeds the P5136 UDP maximum {maximum}")]
    RelayBodyTooLarge {
        packet_name: UdpPacketName,
        length: usize,
        maximum: usize,
    },
}

#[must_use]
pub fn encode_routed_udp_header(header: RoutedUdpHeader) -> [u8; ROUTED_HEADER_LENGTH] {
    let mut encoded = [0; ROUTED_HEADER_LENGTH];
    encoded[0..4].copy_from_slice(&header.account_id.to_le_bytes());
    encoded[4..8].copy_from_slice(&header.route_hash.to_le_bytes());
    encoded[8..12].copy_from_slice(&header.packet_name.hash().to_le_bytes());
    encoded
}

pub fn parse_routed_udp_header(
    logical_payload: &[u8],
) -> Result<(RoutedUdpHeader, &[u8]), UdpProtocolError> {
    let routed = datagram::parse_routed_payload(logical_payload)?;
    let packet_name = classify_udp_packet_name(routed.packet_name)
        .ok_or(UdpProtocolError::UnknownPacketName(routed.packet_name))?;
    Ok((
        RoutedUdpHeader {
            account_id: routed.account_id,
            route_hash: routed.route_hash,
            packet_name,
        },
        routed.body,
    ))
}

pub fn parse_routed_udp_packet(
    logical_payload: &[u8],
) -> Result<RoutedUdpPacket<'_>, UdpProtocolError> {
    let (header, body) = parse_routed_udp_header(logical_payload)?;
    let parsed_body = match header.packet_name {
        UdpPacketName::PqUdpEcho => {
            expect_exact_body_length(header.packet_name, body, UDP_ECHO_BODY_LENGTH)?;
            UdpLogicalBody::PqUdpEcho(PqUdpEchoBody {
                value_1: read_i32(body, 0),
                value_2: read_i32(body, 4),
            })
        }
        UdpPacketName::PrUdpEcho => {
            expect_exact_body_length(header.packet_name, body, UDP_ECHO_BODY_LENGTH)?;
            UdpLogicalBody::PrUdpEcho(PrUdpEchoBody {
                value_1: read_i32(body, 0),
                value_2: read_i32(body, 4),
            })
        }
        UdpPacketName::PqUdpTimeSync => {
            expect_exact_body_length(header.packet_name, body, UDP_TIME_SYNC_REQUEST_BODY_LENGTH)?;
            UdpLogicalBody::PqUdpTimeSync(PqUdpTimeSyncBody {
                client_tick: read_i32(body, 0),
            })
        }
        UdpPacketName::PrUdpTimeSync => {
            expect_exact_body_length(header.packet_name, body, UDP_TIME_SYNC_REPLY_BODY_LENGTH)?;
            UdpLogicalBody::PrUdpTimeSync(PrUdpTimeSyncBody {
                client_tick: read_i32(body, 0),
                server_tick: read_u32(body, 4),
            })
        }
        UdpPacketName::GameSlotPacket => {
            validate_relay_body(header.packet_name, body)?;
            UdpLogicalBody::GameSlotPacket(body)
        }
        UdpPacketName::RoomSlotPacket => {
            validate_relay_body(header.packet_name, body)?;
            UdpLogicalBody::RoomSlotPacket(body)
        }
    };

    Ok(RoutedUdpPacket {
        account_id: header.account_id,
        route_hash: header.route_hash,
        body: parsed_body,
    })
}

pub fn encode_routed_udp_packet(packet: &RoutedUdpPacket<'_>) -> Result<Vec<u8>, UdpProtocolError> {
    match packet.body {
        UdpLogicalBody::GameSlotPacket(body) | UdpLogicalBody::RoomSlotPacket(body) => {
            validate_relay_body(packet.body.packet_name(), body)?;
        }
        UdpLogicalBody::PqUdpEcho(_)
        | UdpLogicalBody::PrUdpEcho(_)
        | UdpLogicalBody::PqUdpTimeSync(_)
        | UdpLogicalBody::PrUdpTimeSync(_) => {}
    }

    let mut encoded = Vec::with_capacity(ROUTED_HEADER_LENGTH + encoded_body_length(packet.body));
    encoded.extend_from_slice(&encode_routed_udp_header(packet.header()));
    match packet.body {
        UdpLogicalBody::PqUdpEcho(body) => {
            encoded.extend_from_slice(&body.value_1.to_le_bytes());
            encoded.extend_from_slice(&body.value_2.to_le_bytes());
        }
        UdpLogicalBody::PrUdpEcho(body) => {
            encoded.extend_from_slice(&body.value_1.to_le_bytes());
            encoded.extend_from_slice(&body.value_2.to_le_bytes());
        }
        UdpLogicalBody::PqUdpTimeSync(body) => {
            encoded.extend_from_slice(&body.client_tick.to_le_bytes());
        }
        UdpLogicalBody::PrUdpTimeSync(body) => {
            encoded.extend_from_slice(&body.client_tick.to_le_bytes());
            encoded.extend_from_slice(&body.server_tick.to_le_bytes());
        }
        UdpLogicalBody::GameSlotPacket(body) | UdpLogicalBody::RoomSlotPacket(body) => {
            encoded.extend_from_slice(body);
        }
    }
    Ok(encoded)
}

const fn encoded_body_length(body: UdpLogicalBody<'_>) -> usize {
    match body {
        UdpLogicalBody::PqUdpEcho(_) | UdpLogicalBody::PrUdpEcho(_) => UDP_ECHO_BODY_LENGTH,
        UdpLogicalBody::PqUdpTimeSync(_) => UDP_TIME_SYNC_REQUEST_BODY_LENGTH,
        UdpLogicalBody::PrUdpTimeSync(_) => UDP_TIME_SYNC_REPLY_BODY_LENGTH,
        UdpLogicalBody::GameSlotPacket(body) | UdpLogicalBody::RoomSlotPacket(body) => body.len(),
    }
}

fn expect_exact_body_length(
    packet_name: UdpPacketName,
    body: &[u8],
    expected: usize,
) -> Result<(), UdpProtocolError> {
    if body.len() != expected {
        return Err(UdpProtocolError::BodyLengthMismatch {
            packet_name,
            expected,
            actual: body.len(),
        });
    }
    Ok(())
}

fn validate_relay_body(packet_name: UdpPacketName, body: &[u8]) -> Result<(), UdpProtocolError> {
    if body.len() > MAX_UDP_RELAY_BODY_LENGTH {
        return Err(UdpProtocolError::RelayBodyTooLarge {
            packet_name,
            length: body.len(),
            maximum: MAX_UDP_RELAY_BODY_LENGTH,
        });
    }
    Ok(())
}

fn read_i32(bytes: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_UDP_RELAY_BODY_LENGTH, PQ_UDP_ECHO_HASH, PQ_UDP_TIME_SYNC_HASH, PR_UDP_ECHO_HASH,
        PR_UDP_TIME_SYNC_HASH, PqUdpEchoBody, PqUdpTimeSyncBody, ROOM_SLOT_PACKET_HASH,
        RoutedUdpHeader, RoutedUdpPacket, UDP_PACKET_NAMES, UdpLogicalBody, UdpPacketName,
        UdpProtocolError, classify_udp_packet_name, encode_routed_udp_header,
        encode_routed_udp_packet, parse_routed_udp_header, parse_routed_udp_packet,
    };
    use crate::{
        adler32,
        datagram::{DEFAULT_MAX_DATAGRAM_PAYLOAD, DatagramError, decode_datagram, encode_datagram},
    };

    const ACCOUNT_ID: u32 = 0x1020_3040;
    const ROUTE_HASH: u32 = 0xA1B2_C3D4;
    const IV: u32 = 0x5136_5136;

    #[test]
    fn classifies_the_six_exact_modern_p5136_packet_hashes() {
        let fixtures = [
            (UdpPacketName::PqUdpEcho, 279_905_129),
            (UdpPacketName::PrUdpEcho, 280_429_418),
            (UdpPacketName::PqUdpTimeSync, 584_516_886),
            (UdpPacketName::PrUdpTimeSync, 585_303_319),
            (UdpPacketName::GameSlotPacket, 666_895_732),
            (UdpPacketName::RoomSlotPacket, 696_255_895),
        ];
        assert_eq!(
            UDP_PACKET_NAMES,
            fixtures.map(|(packet_name, _)| packet_name)
        );
        for (packet_name, expected_hash) in fixtures {
            assert_eq!(packet_name.hash(), expected_hash);
            assert_eq!(adler32::packet_hash(packet_name.rtti_name()), expected_hash);
            assert_eq!(classify_udp_packet_name(expected_hash), Some(packet_name));
        }
        assert_eq!(classify_udp_packet_name(0xDEAD_BEEF), None);
    }

    #[test]
    fn routed_header_is_typed_and_little_endian() {
        let header = RoutedUdpHeader {
            account_id: ACCOUNT_ID,
            route_hash: ROUTE_HASH,
            packet_name: UdpPacketName::PqUdpEcho,
        };
        let encoded = encode_routed_udp_header(header);
        let expected = decode_hex("40302010D4C3B2A16903AF10");
        assert_eq!(encoded.as_slice(), expected.as_slice());

        let mut logical = encoded.to_vec();
        logical.extend_from_slice(&[0; 8]);
        let (parsed, body) = parse_routed_udp_header(&logical).unwrap();
        assert_eq!(parsed, header);
        assert_eq!(body, [0; 8]);
    }

    #[test]
    fn control_bodies_round_trip_and_helpers_match_modern_server_behavior() {
        let echo_request = PqUdpEchoBody {
            value_1: -123_456_789,
            value_2: 0x1122_3344,
        };
        assert_eq!(
            echo_request.reply().value_1,
            echo_request.value_1,
            "the first i32 is echoed unchanged"
        );
        assert_eq!(
            echo_request.reply().value_2,
            echo_request.value_2,
            "the second i32 is echoed unchanged"
        );

        let time_request = PqUdpTimeSyncBody {
            client_tick: i32::MIN + 5136,
        };
        let time_reply = time_request.reply(0xFEDC_BA98);
        assert_eq!(time_reply.client_tick, time_request.client_tick);
        assert_eq!(time_reply.server_tick, 0xFEDC_BA98);

        let packets = [
            routed(UdpLogicalBody::PqUdpEcho(echo_request)),
            routed(UdpLogicalBody::PrUdpEcho(echo_request.reply())),
            routed(UdpLogicalBody::PqUdpTimeSync(time_request)),
            routed(UdpLogicalBody::PrUdpTimeSync(time_reply)),
        ];
        for packet in packets {
            let encoded = encode_routed_udp_packet(&packet).unwrap();
            assert_eq!(parse_routed_udp_packet(&encoded).unwrap(), packet);
        }
    }

    #[test]
    fn every_control_body_requires_its_exact_modern_length() {
        let fixtures = [
            (PQ_UDP_ECHO_HASH, 8),
            (PR_UDP_ECHO_HASH, 8),
            (PQ_UDP_TIME_SYNC_HASH, 4),
            (PR_UDP_TIME_SYNC_HASH, 8),
        ];
        for (hash, expected) in fixtures {
            let short = raw_logical(hash, expected - 1);
            assert_eq!(
                parse_routed_udp_packet(&short),
                Err(UdpProtocolError::BodyLengthMismatch {
                    packet_name: classify_udp_packet_name(hash).unwrap(),
                    expected,
                    actual: expected - 1,
                })
            );

            let long = raw_logical(hash, expected + 1);
            assert_eq!(
                parse_routed_udp_packet(&long),
                Err(UdpProtocolError::BodyLengthMismatch {
                    packet_name: classify_udp_packet_name(hash).unwrap(),
                    expected,
                    actual: expected + 1,
                })
            );
        }
    }

    #[test]
    fn rejects_truncated_or_unknown_routed_headers() {
        assert_eq!(
            parse_routed_udp_packet(&[0; 11]),
            Err(UdpProtocolError::Datagram(
                DatagramError::RoutedHeaderTruncated { actual: 11 }
            ))
        );
        let unknown = raw_logical(0xDEAD_BEEF, 0);
        assert_eq!(
            parse_routed_udp_packet(&unknown),
            Err(UdpProtocolError::UnknownPacketName(0xDEAD_BEEF))
        );
    }

    #[test]
    fn relay_bodies_are_opaque_but_bounded_by_the_udp_envelope() {
        let maximum = vec![0xA5; MAX_UDP_RELAY_BODY_LENGTH];
        let maximum_packet = routed(UdpLogicalBody::GameSlotPacket(&maximum));
        let logical = encode_routed_udp_packet(&maximum_packet).unwrap();
        assert_eq!(logical.len(), DEFAULT_MAX_DATAGRAM_PAYLOAD);
        assert_eq!(parse_routed_udp_packet(&logical).unwrap(), maximum_packet);

        let oversized = vec![0; MAX_UDP_RELAY_BODY_LENGTH + 1];
        let oversized_packet = routed(UdpLogicalBody::RoomSlotPacket(&oversized));
        assert!(matches!(
            encode_routed_udp_packet(&oversized_packet),
            Err(UdpProtocolError::RelayBodyTooLarge {
                packet_name: UdpPacketName::RoomSlotPacket,
                ..
            })
        ));

        let oversized_logical = raw_logical(ROOM_SLOT_PACKET_HASH, oversized.len());
        assert!(matches!(
            parse_routed_udp_packet(&oversized_logical),
            Err(UdpProtocolError::RelayBodyTooLarge {
                packet_name: UdpPacketName::RoomSlotPacket,
                ..
            })
        ));
    }

    #[test]
    fn logical_packets_and_full_wires_match_csharp_scratch_goldens() {
        let game_body = (0_u8..16).collect::<Vec<_>>();
        let room_body = [0xFF, 0x00, 0xAA, 0x55];
        let fixtures = [
            (
                routed(UdpLogicalBody::PqUdpEcho(PqUdpEchoBody {
                    value_1: -123_456_789,
                    value_2: 0x1122_3344,
                })),
                "40302010D4C3B2A16903AF10EB32A4F844332211",
                "36513651BE66A5554E803B7C9EC59A651D4A2F5ABA65A754C1859747",
            ),
            (
                routed(UdpLogicalBody::PrUdpEcho(
                    PqUdpEchoBody {
                        value_1: -123_456_789,
                        value_2: 0x1122_3344,
                    }
                    .reply(),
                )),
                "40302010D4C3B2A16A03B710EB32A4F844332211",
                "36513651BE66A5554E803B7C9DC582651D4A2F5ABA65A754C2858F47",
            ),
            (
                routed(UdpLogicalBody::PqUdpTimeSync(PqUdpTimeSyncBody {
                    client_tick: i32::from_le_bytes([0x21, 0x43, 0x65, 0x87]),
                })),
                "40302010D4C3B2A11605D72221436587",
                "36513651BE66A5554E803B7CE1C3E257D73BEE2556F22E0A",
            ),
            (
                routed(UdpLogicalBody::PrUdpTimeSync(
                    PqUdpTimeSyncBody {
                        client_tick: i32::from_le_bytes([0x21, 0x43, 0x65, 0x87]),
                    }
                    .reply(0xFEDC_BA98),
                )),
                "40302010D4C3B2A11705E3222143658798BADCFE",
                "36513651BE66A5554E803B7CE0C3D657D73BEE2566EC59BB3BF71A0A",
            ),
            (
                routed(UdpLogicalBody::GameSlotPacket(&game_body)),
                "40302010D4C3B2A17405C027000102030405060708090A0B0C0D0E0F",
                "36513651BE66A5554E803B7C83C3F552F67989A1FA538342924A83D6FBCB3B7A9BE05E8B",
            ),
            (
                routed(UdpLogicalBody::RoomSlotPacket(&room_body)),
                "40302010D4C3B2A197058029FF00AA55",
                "36513651BE66A5554E803B7C60C3B55C097821F709B1B6D3",
            ),
        ];

        for (expected_packet, logical_golden, wire_golden) in fixtures {
            let logical = encode_routed_udp_packet(&expected_packet).unwrap();
            assert_eq!(logical, decode_hex(logical_golden));
            let wire = encode_datagram(&logical, IV, DEFAULT_MAX_DATAGRAM_PAYLOAD).unwrap();
            assert_eq!(wire, decode_hex(wire_golden));

            let (decoded_iv, decoded_logical) =
                decode_datagram(&wire, DEFAULT_MAX_DATAGRAM_PAYLOAD).unwrap();
            assert_eq!(decoded_iv, IV);
            assert_eq!(
                parse_routed_udp_packet(&decoded_logical).unwrap(),
                expected_packet
            );
        }
    }

    fn routed(body: UdpLogicalBody<'_>) -> RoutedUdpPacket<'_> {
        RoutedUdpPacket {
            account_id: ACCOUNT_ID,
            route_hash: ROUTE_HASH,
            body,
        }
    }

    fn raw_logical(packet_name: u32, body_length: usize) -> Vec<u8> {
        let mut logical = Vec::with_capacity(12 + body_length);
        logical.extend_from_slice(&ACCOUNT_ID.to_le_bytes());
        logical.extend_from_slice(&ROUTE_HASH.to_le_bytes());
        logical.extend_from_slice(&packet_name.to_le_bytes());
        logical.resize(12 + body_length, 0);
        logical
    }

    fn decode_hex(input: &str) -> Vec<u8> {
        assert!(input.len().is_multiple_of(2));
        input
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let text = std::str::from_utf8(pair).unwrap();
                u8::from_str_radix(text, 16).unwrap()
            })
            .collect()
    }
}
