//! Packet codecs for P5136's TCP channel hand-off.

use std::net::Ipv4Addr;

use thiserror::Error;

use crate::{
    adler32,
    packet::{PacketError, PacketReader, PacketWriter},
};

pub const DEFAULT_MAX_SWITCH_OPAQUE_LENGTH: usize = 1_048_576;

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
        ChannelError, parse_pq_channel_movein, parse_pq_channel_switch,
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
