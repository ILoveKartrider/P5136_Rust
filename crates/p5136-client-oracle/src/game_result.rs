//! Client-side `GameResultPacket` reader reconstructed from `sub_726CC0`,
//! `sub_71BF00`, and `sub_71BAD0`.

use crate::{DecodeError, cursor::Cursor};

const PACKET_HASH: u32 = 0x345C_0651;
const MAX_PARTICIPANTS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HumanResult {
    pub player_id: i32,
    pub finish_time: u32,
    pub kart_id: u16,
    pub rank: i32,
    pub team_mode_marker: i16,
    pub current_rp: u32,
    pub earned_rp: u32,
    pub earned_lucci: u32,
    pub current_lucci: u32,
    pub team: u8,
    pub team_points: i32,
    pub result_marker: i32,
    pub character_id: u16,
    pub display_marker: u8,
    pub club_mark_logo: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiResult {
    pub player_id: i32,
    pub finish_time: u32,
    pub kart_id: i16,
    pub rank: i32,
    pub team_mode_marker: i16,
    pub team: u8,
    pub team_points: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameResult {
    pub winning_team: u8,
    pub humans: Vec<HumanResult>,
    pub ais: Vec<AiResult>,
    pub terminal_marker: u32,
    pub terminal_status: u8,
}

pub fn decode(packet: &[u8]) -> Result<GameResult, DecodeError> {
    let mut reader = Cursor::new(packet);
    reader.expect_hash("GameResultPacket", PACKET_HASH)?;
    let winning_team = reader.u8()?;
    let human_count = reader.count("human result", MAX_PARTICIPANTS)?;
    let mut humans = Vec::with_capacity(human_count);
    for _ in 0..human_count {
        humans.push(read_human(&mut reader)?);
    }
    let ai_count = reader.count("AI result", MAX_PARTICIPANTS.saturating_sub(human_count))?;
    let mut ais = Vec::with_capacity(ai_count);
    for _ in 0..ai_count {
        ais.push(read_ai(&mut reader)?);
    }

    // `sub_726CC0` consumes this fixed P5136 result-stage tail. Its 34-byte
    // object state remains opaque; only consumption is asserted.
    reader.bytes(34)?;
    let terminal_marker = reader.u32()?;
    let terminal_status = reader.u8()?;
    reader.finish()?;
    Ok(GameResult {
        winning_team,
        humans,
        ais,
        terminal_marker,
        terminal_status,
    })
}

fn read_human(reader: &mut Cursor<'_>) -> Result<HumanResult, DecodeError> {
    let start = reader.position();
    let player_id = reader.i32()?;
    let finish_time = reader.u32()?;
    reader.u8()?;
    let kart_id = reader.u16()?;
    let rank = reader.i32()?;
    let team_mode_marker = reader.i16()?;
    reader.u8()?;
    let current_rp = reader.u32()?;
    let earned_rp = reader.u32()?;
    let earned_lucci = reader.u32()?;
    let current_lucci = reader.u32()?;
    reader.bytes(29)?;
    let team = reader.u8()?;
    let team_points = reader.i32()?;
    reader.bytes(12)?;
    let result_marker = reader.i32()?;
    reader.u8()?;
    let character_id = reader.u16()?;
    reader.bytes(49)?;
    let display_marker = reader.u8()?;
    reader.bytes(37)?;
    let club_mark_logo = reader.i32()?;
    reader.bytes(34)?;
    debug_assert_eq!(reader.position() - start, 212);
    Ok(HumanResult {
        player_id,
        finish_time,
        kart_id,
        rank,
        team_mode_marker,
        current_rp,
        earned_rp,
        earned_lucci,
        current_lucci,
        team,
        team_points,
        result_marker,
        character_id,
        display_marker,
        club_mark_logo,
    })
}

fn read_ai(reader: &mut Cursor<'_>) -> Result<AiResult, DecodeError> {
    let start = reader.position();
    let player_id = reader.i32()?;
    let finish_time = reader.u32()?;
    reader.u8()?;
    let kart_id = reader.i16()?;
    let rank = reader.i32()?;
    let team_mode_marker = reader.i16()?;
    let team = reader.u8()?;
    let team_points = reader.i32()?;
    debug_assert_eq!(reader.position() - start, 22);
    Ok(AiResult {
        player_id,
        finish_time,
        kart_id,
        rank,
        team_mode_marker,
        team,
        team_points,
    })
}
