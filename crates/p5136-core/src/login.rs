//! P5136 authentication and account-login packet codecs.

use std::net::Ipv4Addr;

use thiserror::Error;

use crate::{
    adler32,
    bml::{BmlError, BmlNode},
    packet::{PacketError, PacketReader, PacketWriter},
};

pub const ACCOUNT_DATA_PROFILE_NAME: &str = "AccountDataProfile";
pub const P5136_REGULAR_PMAP: u32 = 0;
pub const P5136_OBSERVER_PMAP: u32 = 590;
pub const P5136_OBSERVER_MASTER_PMAP: u32 = 718;
pub const LEGACY_LOGIN_TOKEN: &str = "lppicekedkgjdqmncddpddecdogjppqhrghqifqjmjhcfiorecpmockdlngloorhqmekhrpdpejlgnclklrmddhoprcqknrfjolidjhndejiokfjoogqrgldgigqlhpp";
pub const AGREEMENT_URL: &str = "https://www.tiancity.com/agreement";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PqLogin {
    pub varying_value_1: u32,
    pub varying_value_2: u32,
    pub profile: BmlNode,
    pub nickname: String,
    /// Optional role selector supplied by the local connector profile.
    ///
    /// Stock launchers omit this node. The server accepts only the known
    /// regular/observer presets and otherwise rejects the login rather than
    /// exposing arbitrary permission-map bits to a client.
    pub requested_pmap: Option<u32>,
    pub trailing: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegacyTime {
    pub days_since_1900: u16,
    pub quarter_seconds: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrLoginFields {
    pub time: LegacyTime,
    pub user_no: u32,
    pub nickname: String,
    pub pmap: u32,
    pub advertised_address: Ipv4Addr,
    pub game_udp_port: u16,
    pub p2p_udp_port: u16,
    pub screen: u8,
}

#[derive(Debug, Error)]
pub enum LoginError {
    #[error(transparent)]
    Packet(#[from] PacketError),

    #[error(transparent)]
    Bml(#[from] BmlError),

    #[error("expected {name} hash 0x{expected:08X}, received 0x{actual:08X}")]
    UnexpectedPacketHash {
        name: &'static str,
        expected: u32,
        actual: u32,
    },

    #[error("expected AccountDataProfile hash 0x{expected:08X}, received 0x{actual:08X}")]
    UnexpectedProfileHash { expected: u32, actual: u32 },

    #[error("AccountDataProfile reserved byte is {0}, not zero")]
    InvalidReservedByte(u8),

    #[error("AccountDataProfile root is {actual:?}, not \"profile\"")]
    InvalidProfileRoot { actual: String },

    #[error("AccountDataProfile does not contain a non-empty username value")]
    MissingUsername,

    #[error("AccountDataProfile pmap value {value:?} is not an unsigned decimal integer")]
    InvalidRequestedPmap { value: String },

    #[error("AccountDataProfile pmap value {0} is not a supported P5136 role preset")]
    UnsupportedRequestedPmap(u32),
}

/// Parses a complete logical `PqLogin` packet and extracts its first
/// case-insensitive `username` BML node.
pub fn parse_pq_login(packet: &[u8]) -> Result<PqLogin, LoginError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, "PqLogin")?;
    let varying_value_1 = reader.read_u32()?;
    let varying_value_2 = reader.read_u32()?;

    let profile_hash = reader.read_u32()?;
    let expected_profile_hash = adler32::packet_hash(ACCOUNT_DATA_PROFILE_NAME);
    if profile_hash != expected_profile_hash {
        return Err(LoginError::UnexpectedProfileHash {
            expected: expected_profile_hash,
            actual: profile_hash,
        });
    }

    let reserved = reader.read_u8()?;
    if reserved != 0 {
        return Err(LoginError::InvalidReservedByte(reserved));
    }

    let profile = BmlNode::decode(&mut reader)?;
    if !profile.name.eq_ignore_ascii_case("profile") {
        return Err(LoginError::InvalidProfileRoot {
            actual: profile.name.clone(),
        });
    }
    let nickname = profile
        .first_value_named("username")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(LoginError::MissingUsername)?
        .to_owned();
    let requested_pmap = profile
        .first_value_named("pmap")
        .map(str::trim)
        .map(|value| {
            let pmap = value
                .parse::<u32>()
                .map_err(|_| LoginError::InvalidRequestedPmap {
                    value: value.to_owned(),
                })?;
            match pmap {
                P5136_REGULAR_PMAP | P5136_OBSERVER_PMAP | P5136_OBSERVER_MASTER_PMAP => Ok(pmap),
                _ => Err(LoginError::UnsupportedRequestedPmap(pmap)),
            }
        })
        .transpose()?;

    Ok(PqLogin {
        varying_value_1,
        varying_value_2,
        profile,
        nickname,
        requested_pmap,
        trailing: reader.remaining().to_vec(),
    })
}

/// Builds the fixed P5136 `PrCnAuthenLogin` logical packet.
pub fn serialize_pr_cn_authen_login() -> Result<Vec<u8>, PacketError> {
    let mut packet = PacketWriter::named("PrCnAuthenLogin");
    packet.write_i32(1);
    packet.write_utf16(LEGACY_LOGIN_TOKEN)?;
    packet.write_u8(0);
    packet.write_utf16(AGREEMENT_URL)?;
    Ok(packet.into_inner())
}

/// Builds the Korean P5136 `PrLogin` logical packet. Time conversion and
/// identity allocation remain host concerns and are supplied as raw fields.
pub fn serialize_pr_login(fields: &PrLoginFields) -> Result<Vec<u8>, PacketError> {
    let mut packet = PacketWriter::named("PrLogin");
    packet.write_i32(0);
    packet.write_u16(fields.time.days_since_1900);
    packet.write_u16(fields.time.quarter_seconds);
    packet.write_u32(fields.user_no);
    packet.write_utf16(&fields.nickname)?;
    packet.write_u8(2);
    packet.write_u8(1);
    packet.write_u8(0);
    packet.write_i32(0);
    packet.write_u8(0);
    packet.write_i32(1_415_577_599);
    packet.write_u32(fields.pmap);
    for _ in 0..11 {
        packet.write_i32(0);
    }
    packet.write_u8(0);
    write_endpoint(&mut packet, fields.advertised_address, fields.game_udp_port);
    write_endpoint(&mut packet, fields.advertised_address, fields.p2p_udp_port);
    packet.write_i32(0);
    packet.write_utf16("")?;
    packet.write_i32(0);
    packet.write_u8(1);
    packet.write_utf16("content")?;
    packet.write_i32(0);
    packet.write_i32(1);
    packet.write_utf16("cc")?;
    packet.write_utf16("kr")?;
    packet.write_i32(0);
    packet.write_u8(0);
    packet.write_u8(fields.screen);
    Ok(packet.into_inner())
}

fn expect_hash(reader: &mut PacketReader<'_>, name: &'static str) -> Result<(), LoginError> {
    let actual = reader.read_u32()?;
    let expected = adler32::packet_hash(name);
    if actual == expected {
        Ok(())
    } else {
        Err(LoginError::UnexpectedPacketHash {
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
        AGREEMENT_URL, LEGACY_LOGIN_TOKEN, LegacyTime, P5136_OBSERVER_MASTER_PMAP, PrLoginFields,
        parse_pq_login, serialize_pr_cn_authen_login, serialize_pr_login,
    };
    use crate::{
        adler32,
        bml::BmlNode,
        packet::{PacketReader, PacketWriter},
    };

    #[test]
    fn parses_the_csharp_pq_login_fixture_and_keeps_trailing_bytes() {
        let mut packet = decode_hex(
            "BA02830A1096018B93B006BA18072B420007000000700072006F00660069006C006500\
             0000000000000000010000000800000075007300650072006E0061006D00650005000000\
             590061006E00790032000000000000000000",
        );
        packet.extend_from_slice(&[0xaa, 0xbb]);

        let login = parse_pq_login(&packet).unwrap();
        assert_eq!(login.varying_value_1, 0x8b01_9610);
        assert_eq!(login.varying_value_2, 0xba06_b093);
        assert_eq!(login.nickname, "Yany2");
        assert_eq!(login.requested_pmap, None);
        assert_eq!(login.profile.name, "profile");
        assert_eq!(login.trailing, [0xaa, 0xbb]);
        assert_eq!(
            format!("{:X}", Sha256::digest(&packet[..packet.len() - 2])),
            "6B020D4F8C308D67432D02EA79BBA78E9A9D43CAD199432EBCFE6CD4B81B1FA7"
        );
    }

    #[test]
    fn connector_pmap_accepts_only_known_regular_and_observer_presets() {
        let observer = login_packet_with_pmap("718");
        assert_eq!(
            parse_pq_login(&observer).unwrap().requested_pmap,
            Some(P5136_OBSERVER_MASTER_PMAP)
        );

        let regular = login_packet_with_pmap("0");
        assert_eq!(parse_pq_login(&regular).unwrap().requested_pmap, Some(0));

        assert!(matches!(
            parse_pq_login(&login_packet_with_pmap("3130")),
            Err(super::LoginError::UnsupportedRequestedPmap(3130))
        ));
        assert!(matches!(
            parse_pq_login(&login_packet_with_pmap("observer")),
            Err(super::LoginError::InvalidRequestedPmap { .. })
        ));
    }

    #[test]
    fn pr_cn_authen_login_matches_the_exact_csharp_serializer() {
        let packet = serialize_pr_cn_authen_login().unwrap();
        assert_eq!(packet.len(), 341);
        assert_eq!(
            format!("{:X}", Sha256::digest(&packet)),
            "E2D704B6E346579A0B544530D159FAADB7BBDBE10D386E34A1BE4B2C69778FC4"
        );

        let mut reader = PacketReader::new(&packet);
        assert_eq!(
            reader.read_u32().unwrap(),
            adler32::packet_hash("PrCnAuthenLogin")
        );
        assert_eq!(reader.read_i32().unwrap(), 1);
        assert_eq!(reader.read_utf16().unwrap(), LEGACY_LOGIN_TOKEN);
        assert_eq!(reader.read_u8().unwrap(), 0);
        assert_eq!(reader.read_utf16().unwrap(), AGREEMENT_URL);
        assert!(reader.remaining().is_empty());
    }

    #[test]
    fn pr_login_matches_the_csharp_field_layout_golden() {
        let fields = PrLoginFields {
            time: LegacyTime {
                days_since_1900: 0x1234,
                quarter_seconds: 0x5678,
            },
            user_no: 0x1020_3040,
            nickname: "Yany2".to_owned(),
            pmap: 0xaabb_ccdd,
            advertised_address: Ipv4Addr::LOCALHOST,
            game_udp_port: 39_311,
            p2p_udp_port: 39_312,
            screen: 7,
        };
        let packet = serialize_pr_login(&fields).unwrap();

        assert_eq!(packet.len(), 164);
        assert_eq!(
            format!("{:X}", Sha256::digest(&packet)),
            "64882295CF34A4AA6ECA6EF5BA8490F63B1422002BED0A4DC5113FF2811E8DD2"
        );
        assert_eq!(
            &packet[..32],
            decode_hex("BB02890A00000000341278564030201005000000590061006E00790032000201")
        );
    }

    fn decode_hex(input: &str) -> Vec<u8> {
        let compact = input
            .bytes()
            .filter(|byte| !byte.is_ascii_whitespace())
            .collect::<Vec<_>>();
        assert!(compact.len().is_multiple_of(2));
        compact
            .chunks_exact(2)
            .map(|pair| {
                let text = std::str::from_utf8(pair).unwrap();
                u8::from_str_radix(text, 16).unwrap()
            })
            .collect()
    }

    fn login_packet_with_pmap(pmap: &str) -> Vec<u8> {
        let mut profile = BmlNode::new("profile", "");
        profile.children.push(BmlNode::new("username", "Observer"));
        profile.children.push(BmlNode::new("pmap", pmap));

        let mut packet = PacketWriter::named("PqLogin");
        packet.write_u32(1);
        packet.write_u32(2);
        packet.write_u32(adler32::packet_hash(super::ACCOUNT_DATA_PROFILE_NAME));
        packet.write_u8(0);
        profile.encode(&mut packet).unwrap();
        packet.into_inner()
    }
}
