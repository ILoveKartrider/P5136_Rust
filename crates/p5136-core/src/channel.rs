//! Packet codecs for P5136's TCP channel hand-off.

use std::net::Ipv4Addr;

use thiserror::Error;

use crate::{
    adler32,
    packet::{PacketError, PacketReader, PacketWriter},
};

pub const DEFAULT_MAX_SWITCH_OPAQUE_LENGTH: usize = 1_048_576;
pub const CLIENT_P2P_ADDRESS_PACKET_NAME: &str = "ChClientP2pAddrPacket";
pub const CLIENT_UDP_ADDRESS_PACKET_NAME: &str = "ChClientUdpAddrPacket";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientEndpointReportKind {
    P2p,
    GameUdp,
}

impl ClientEndpointReportKind {
    #[must_use]
    pub const fn packet_name(self) -> &'static str {
        match self {
            Self::P2p => CLIENT_P2P_ADDRESS_PACKET_NAME,
            Self::GameUdp => CLIENT_UDP_ADDRESS_PACKET_NAME,
        }
    }
}

pub const CLIENT_ENDPOINT_REPORTS: &[ClientEndpointReportKind] = &[
    ClientEndpointReportKind::P2p,
    ClientEndpointReportKind::GameUdp,
];

/// The only trusted value extracted from a client endpoint report.
///
/// The four reported address bytes are consumed for exact wire validation but
/// deliberately discarded. Room advertisement remains bound to the
/// authenticated TCP connection's source IP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientEndpointPortReport {
    port: u16,
}

impl ClientEndpointPortReport {
    #[must_use]
    pub const fn port(self) -> u16 {
        self.port
    }
}

/// Resolves P5136's mode-group value to a concrete record from the static
/// channel table advertised by `ChRequestChStaticReplyPacket`.
#[must_use]
pub fn resolve_channel_id(requested_game_type: u8, preferred_channel_id: u16) -> Option<u16> {
    let channel_ids: &[u16] = match requested_game_type {
        20 => &[1],
        52 => &[2],
        54 => &[3],
        53 => &[4],
        7 => &[5],
        8 => &[6],
        13 => &[7],
        14 => &[8],
        65 => &[9],
        66 => &[10],
        67 => &[11, 12],
        23 => &[13, 14, 15, 16],
        68 => &[17, 18],
        24 => &[19, 20, 21, 22],
        49 => &[23],
        48 => &[24],
        _ => return None,
    };

    if preferred_channel_id != 0 && channel_ids.contains(&preferred_channel_id) {
        Some(preferred_channel_id)
    } else {
        channel_ids.first().copied()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PqChannelSwitch {
    pub opaque: Vec<u8>,
    pub requested_game_type: u8,
    pub preferred_channel_id: u16,
    pub trailing: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PqChannelMovein {
    pub user_no: u32,
    pub channel_id: u16,
    pub migration_token: u16,
    pub trailing: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum ChannelError {
    #[error(transparent)]
    Packet(#[from] PacketError),

    #[error("expected {name} hash 0x{expected:08X}, received 0x{actual:08X}")]
    UnexpectedPacketHash {
        name: &'static str,
        expected: u32,
        actual: u32,
    },

    #[error("negative PqChannelSwitch opaque length {0}")]
    NegativeOpaqueLength(i32),

    #[error("PqChannelSwitch opaque length {length} exceeds configured maximum {maximum}")]
    OpaqueTooLarge { length: usize, maximum: usize },

    #[error("{name} has {count} trailing bytes")]
    TrailingBytes { name: &'static str, count: usize },
}

#[must_use]
pub fn classify_client_endpoint_report(hash: u32) -> Option<ClientEndpointReportKind> {
    CLIENT_ENDPOINT_REPORTS
        .iter()
        .copied()
        .find(|kind| adler32::packet_hash(kind.packet_name()) == hash)
}

pub fn parse_client_endpoint_report(
    kind: ClientEndpointReportKind,
    packet: &[u8],
) -> Result<ClientEndpointPortReport, ChannelError> {
    let name = kind.packet_name();
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, name)?;
    let _reported_address = reader.read_bytes(4)?;
    let port = reader.read_u16()?;
    let trailing = reader.remaining().len();
    if trailing != 0 {
        return Err(ChannelError::TrailingBytes {
            name,
            count: trailing,
        });
    }
    Ok(ClientEndpointPortReport { port })
}

pub fn parse_pq_channel_switch(packet: &[u8]) -> Result<PqChannelSwitch, ChannelError> {
    parse_pq_channel_switch_with_limit(packet, DEFAULT_MAX_SWITCH_OPAQUE_LENGTH)
}

pub fn parse_pq_channel_switch_with_limit(
    packet: &[u8],
    maximum_opaque_length: usize,
) -> Result<PqChannelSwitch, ChannelError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, "PqChannelSwitch")?;
    let signed_length = reader.read_i32()?;
    let length = usize::try_from(signed_length)
        .map_err(|_| ChannelError::NegativeOpaqueLength(signed_length))?;
    if length > maximum_opaque_length {
        return Err(ChannelError::OpaqueTooLarge {
            length,
            maximum: maximum_opaque_length,
        });
    }

    let opaque = reader.read_bytes(length)?.to_vec();
    let requested_game_type = reader.read_u8()?;
    let preferred_channel_id = reader.read_u16()?;
    Ok(PqChannelSwitch {
        opaque,
        requested_game_type,
        preferred_channel_id,
        trailing: reader.remaining().to_vec(),
    })
}

#[must_use]
pub fn serialize_pr_channel_switch(
    selected_channel_id: u16,
    migration_token: u16,
    login_address: Ipv4Addr,
    login_port: u16,
) -> Vec<u8> {
    let mut packet = PacketWriter::named("PrChannelSwitch");
    packet.write_i32(0);
    packet.write_u16(selected_channel_id);
    packet.write_u16(migration_token);
    write_endpoint(&mut packet, login_address, login_port);
    packet.into_inner()
}

pub fn parse_pq_channel_movein(packet: &[u8]) -> Result<PqChannelMovein, ChannelError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, "PqChannelMovein")?;
    let user_no = reader.read_u32()?;
    let channel_id = reader.read_u16()?;
    let migration_token = reader.read_u16()?;
    Ok(PqChannelMovein {
        user_no,
        channel_id,
        migration_token,
        trailing: reader.remaining().to_vec(),
    })
}

/// Builds `PrChannelMoveIn`, whose P5136 implementation deliberately writes
/// `0.0.0.0` for both UDP endpoint addresses.
#[must_use]
pub fn serialize_pr_channel_move_in(game_udp_port: u16, p2p_udp_port: u16) -> Vec<u8> {
    let mut packet = PacketWriter::named("PrChannelMoveIn");
    packet.write_u8(1);
    write_endpoint(&mut packet, Ipv4Addr::UNSPECIFIED, game_udp_port);
    write_endpoint(&mut packet, Ipv4Addr::UNSPECIFIED, p2p_udp_port);
    packet.into_inner()
}

fn expect_hash(reader: &mut PacketReader<'_>, name: &'static str) -> Result<(), ChannelError> {
    let actual = reader.read_u32()?;
    let expected = adler32::packet_hash(name);
    if actual == expected {
        Ok(())
    } else {
        Err(ChannelError::UnexpectedPacketHash {
            name,
            expected,
            actual,
        })
    }
}

fn write_endpoint(packet: &mut PacketWriter, address: Ipv4Addr, port: u16) {
    packet.write_bytes(&address.octets());
    packet.write_u16(port);
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use sha2::{Digest, Sha256};

    use super::{
        CLIENT_ENDPOINT_REPORTS, CLIENT_P2P_ADDRESS_PACKET_NAME, CLIENT_UDP_ADDRESS_PACKET_NAME,
        ChannelError, ClientEndpointReportKind, classify_client_endpoint_report,
        parse_client_endpoint_report, parse_pq_channel_movein, parse_pq_channel_switch,
        parse_pq_channel_switch_with_limit, resolve_channel_id, serialize_pr_channel_move_in,
        serialize_pr_channel_switch,
    };

    #[test]
    fn resolves_concrete_records_from_the_p5136_static_channel_table() {
        assert_eq!(resolve_channel_id(67, 12), Some(12));
        assert_eq!(resolve_channel_id(67, 99), Some(11));
        assert_eq!(resolve_channel_id(23, 0), Some(13));
        assert_eq!(resolve_channel_id(48, 24), Some(24));
        assert_eq!(resolve_channel_id(0, 0), None);
    }

    #[test]
    fn channel_switch_request_and_reply_match_csharp_layout() {
        let request = [
            0xec, 0x05, 0x09, 0x2e, 0x03, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x43, 0x0c, 0x00,
        ];
        let parsed = parse_pq_channel_switch(&request).unwrap();
        assert_eq!(parsed.opaque, [1, 2, 3]);
        assert_eq!(parsed.requested_game_type, 67);
        assert_eq!(parsed.preferred_channel_id, 12);
        assert!(parsed.trailing.is_empty());
        assert_eq!(
            format!("{:X}", Sha256::digest(request)),
            "1AC5CD5E1FFE7CD41FEAF28BBC5E353699DF7A2F82A8C3044FF1F266A37591CA"
        );

        let reply = serialize_pr_channel_switch(12, 0xbeef, Ipv4Addr::LOCALHOST, 39_312);
        assert_eq!(
            reply,
            [
                0xed, 0x05, 0x17, 0x2e, 0x00, 0x00, 0x00, 0x00, 0x0c, 0x00, 0xef, 0xbe, 0x7f, 0x00,
                0x00, 0x01, 0x90, 0x99,
            ]
        );
        assert_eq!(
            format!("{:X}", Sha256::digest(&reply)),
            "F690E0A1AE840730DC5614FF22161D184B91ACB0158DB5E11C8282DA8624431D"
        );
    }

    #[test]
    fn channel_movein_request_and_reply_match_csharp_layout_and_case() {
        let request = [
            0xe8, 0x05, 0xd6, 0x2d, 0x40, 0x30, 0x20, 0x10, 0x0c, 0x00, 0xef, 0xbe,
        ];
        let parsed = parse_pq_channel_movein(&request).unwrap();
        assert_eq!(parsed.user_no, 0x1020_3040);
        assert_eq!(parsed.channel_id, 12);
        assert_eq!(parsed.migration_token, 0xbeef);
        assert!(parsed.trailing.is_empty());
        assert_eq!(
            format!("{:X}", Sha256::digest(request)),
            "CA8715A08EC2D9A0F9A8AD9AAD39B989074358AE089F2251CA465F71251CD788"
        );

        let reply = serialize_pr_channel_move_in(39_311, 39_312);
        assert_eq!(
            reply,
            [
                0xc9, 0x05, 0xa4, 0x2d, 0x01, 0x00, 0x00, 0x00, 0x00, 0x8f, 0x99, 0x00, 0x00, 0x00,
                0x00, 0x90, 0x99,
            ]
        );
        assert_eq!(
            format!("{:X}", Sha256::digest(&reply)),
            "787B77F735942EF6D57F07297FBD9E2B908023377DDD6089CA044A61E998F09C"
        );
    }

    #[test]
    fn client_endpoint_reports_are_exact_and_discard_the_claimed_address() {
        let p2p = [0xcf, 0x07, 0x97, 0x53, 203, 0, 113, 99, 0x34, 0x12];
        assert_eq!(
            classify_client_endpoint_report(u32::from_le_bytes([p2p[0], p2p[1], p2p[2], p2p[3]])),
            Some(ClientEndpointReportKind::P2p)
        );
        assert_eq!(
            parse_client_endpoint_report(ClientEndpointReportKind::P2p, &p2p)
                .unwrap()
                .port(),
            0x1234
        );

        let udp = [0x06, 0x08, 0x30, 0x56, 198, 51, 100, 7, 0, 0];
        assert_eq!(
            classify_client_endpoint_report(u32::from_le_bytes([udp[0], udp[1], udp[2], udp[3]])),
            Some(ClientEndpointReportKind::GameUdp)
        );
        assert_eq!(
            parse_client_endpoint_report(ClientEndpointReportKind::GameUdp, &udp)
                .unwrap()
                .port(),
            0
        );

        assert_eq!(
            crate::adler32::packet_hash(CLIENT_P2P_ADDRESS_PACKET_NAME),
            1_402_406_863
        );
        assert_eq!(
            crate::adler32::packet_hash(CLIENT_UDP_ADDRESS_PACKET_NAME),
            1_445_988_358
        );
        assert_eq!(CLIENT_ENDPOINT_REPORTS.len(), 2);
        assert_eq!(classify_client_endpoint_report(0xDEAD_BEEF), None);
    }

    #[test]
    fn client_endpoint_reports_reject_wrong_hash_truncation_and_trailing_bytes() {
        let exact = [0xcf, 0x07, 0x97, 0x53, 127, 0, 0, 1, 0x90, 0x99];
        assert!(matches!(
            parse_client_endpoint_report(ClientEndpointReportKind::GameUdp, &exact),
            Err(ChannelError::UnexpectedPacketHash {
                name: CLIENT_UDP_ADDRESS_PACKET_NAME,
                ..
            })
        ));
        for length in 0..exact.len() {
            assert!(
                parse_client_endpoint_report(ClientEndpointReportKind::P2p, &exact[..length])
                    .is_err(),
                "truncated endpoint length {length} must be rejected"
            );
        }

        let mut trailing = exact.to_vec();
        trailing.push(0xff);
        assert!(matches!(
            parse_client_endpoint_report(ClientEndpointReportKind::P2p, &trailing),
            Err(ChannelError::TrailingBytes {
                name: CLIENT_P2P_ADDRESS_PACKET_NAME,
                count: 1
            })
        ));

        let mut maximum_port = exact;
        maximum_port[8..].copy_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(
            parse_client_endpoint_report(ClientEndpointReportKind::P2p, &maximum_port)
                .unwrap()
                .port(),
            u16::MAX
        );
        maximum_port[4..8].copy_from_slice(&[203, 0, 113, 250]);
        assert_eq!(
            parse_client_endpoint_report(ClientEndpointReportKind::P2p, &maximum_port)
                .unwrap()
                .port(),
            u16::MAX,
            "claimed address bytes must not affect the trusted result"
        );
    }

    #[test]
    fn channel_switch_opaque_length_is_bounded_before_copying() {
        let request = [
            0xec, 0x05, 0x09, 0x2e, 0x03, 0x00, 0x00, 0x00, 0xaa, 0xbb, 0xcc, 0x43, 0x0b, 0x00,
        ];
        assert!(matches!(
            parse_pq_channel_switch_with_limit(&request, 2),
            Err(ChannelError::OpaqueTooLarge {
                length: 3,
                maximum: 2
            })
        ));
    }
}
