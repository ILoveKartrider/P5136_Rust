//! Structural readers for the authentication, login, and channel-migration
//! replies. Evidence is exact C# layout plus successful stock-client
//! progression; these are not labeled as native-consumer-exact.

use std::net::Ipv4Addr;

use crate::{DecodeError, cursor::Cursor};

const AUTH_REPLY_HASH: u32 = 0x2D30_05D1;
const LOGIN_REPLY_HASH: u32 = 0x0A89_02BB;
const CHANNEL_MOVE_IN_HASH: u32 = 0x2DA4_05C9;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthReply {
    pub status: i32,
    pub token: String,
    pub agreement_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub address: Ipv4Addr,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginReply {
    pub status: i32,
    pub days_since_1900: u16,
    pub quarter_seconds: u16,
    pub user_no: u32,
    pub nickname: String,
    pub pmap: u32,
    pub game_udp: Endpoint,
    pub p2p_udp: Endpoint,
    pub content_label: String,
    pub country_key: String,
    pub country_value: String,
    pub screen: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelMoveIn {
    pub accepted: bool,
    pub game_udp: Endpoint,
    pub p2p_udp: Endpoint,
}

pub fn decode_auth_reply(packet: &[u8]) -> Result<AuthReply, DecodeError> {
    let mut reader = Cursor::new(packet);
    reader.expect_hash("PrCnAuthenLogin", AUTH_REPLY_HASH)?;
    let status = reader.i32()?;
    let token = reader.utf16("legacy login token", 256)?;
    reader.u8()?;
    let agreement_url = reader.utf16("agreement URL", 256)?;
    reader.finish()?;
    Ok(AuthReply {
        status,
        token,
        agreement_url,
    })
}

pub fn decode_login_reply(packet: &[u8]) -> Result<LoginReply, DecodeError> {
    let mut reader = Cursor::new(packet);
    reader.expect_hash("PrLogin", LOGIN_REPLY_HASH)?;
    let status = reader.i32()?;
    let days_since_1900 = reader.u16()?;
    let quarter_seconds = reader.u16()?;
    let user_no = reader.u32()?;
    let nickname = reader.utf16("rider nickname", 32)?;
    reader.bytes(3)?;
    reader.i32()?;
    reader.u8()?;
    reader.i32()?;
    let pmap = reader.u32()?;
    for _ in 0..11 {
        reader.i32()?;
    }
    reader.u8()?;
    let game_udp = read_endpoint(&mut reader)?;
    let p2p_udp = read_endpoint(&mut reader)?;
    reader.i32()?;
    reader.utf16("login optional label", 128)?;
    reader.i32()?;
    reader.u8()?;
    let content_label = reader.utf16("content label", 32)?;
    reader.i32()?;
    reader.i32()?;
    let country_key = reader.utf16("country key", 8)?;
    let country_value = reader.utf16("country value", 8)?;
    reader.i32()?;
    reader.u8()?;
    let screen = reader.u8()?;
    reader.finish()?;
    Ok(LoginReply {
        status,
        days_since_1900,
        quarter_seconds,
        user_no,
        nickname,
        pmap,
        game_udp,
        p2p_udp,
        content_label,
        country_key,
        country_value,
        screen,
    })
}

pub fn decode_channel_move_in(packet: &[u8]) -> Result<ChannelMoveIn, DecodeError> {
    let mut reader = Cursor::new(packet);
    reader.expect_hash("PrChannelMoveIn", CHANNEL_MOVE_IN_HASH)?;
    let accepted = reader.u8()? != 0;
    let game_udp = read_endpoint(&mut reader)?;
    let p2p_udp = read_endpoint(&mut reader)?;
    reader.finish()?;
    Ok(ChannelMoveIn {
        accepted,
        game_udp,
        p2p_udp,
    })
}

fn read_endpoint(reader: &mut Cursor<'_>) -> Result<Endpoint, DecodeError> {
    Ok(Endpoint {
        address: Ipv4Addr::from(reader.array::<4>()?),
        port: reader.u16()?,
    })
}
