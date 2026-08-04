//! Race-ceremony codec and scene-order oracle.
//!
//! `GameNextStagePacket` has a native codec and consumer. The three-packet
//! order is acceptance evidence from the known-working deployed trace rather
//! than a claim that all `GameControl` state effects were recovered statically.

use crate::{DecodeError, cursor::Cursor, game_result, legacy_scalar};

const GAME_CONTROL_HASH: u32 = 0x3ACB_06B3;
const GAME_NEXT_STAGE_HASH: u32 = 0x4891_0765;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameControl {
    pub state: i32,
    pub value0: u32,
    pub encoded_status: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameNextStage {
    pub game_type: u8,
    pub stage_marker_1: i32,
    pub stage_marker_2: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CeremonyState {
    Racing,
    FinalStage,
    StageAdvanced,
    Podium,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ceremony {
    state: CeremonyState,
}

impl Default for Ceremony {
    fn default() -> Self {
        Self {
            state: CeremonyState::Racing,
        }
    }
}

impl Ceremony {
    #[must_use]
    pub const fn state(self) -> CeremonyState {
        self.state
    }

    pub fn accept_game_control(&mut self, packet: &[u8]) -> Result<GameControl, DecodeError> {
        if self.state != CeremonyState::Racing {
            return Err(DecodeError::InvalidSequence {
                expected: "GameControl(state=4)",
                actual: state_name(self.state),
            });
        }
        let control = decode_game_control(packet)?;
        if control.state != 4 {
            return Err(DecodeError::UnsupportedDiscriminant {
                field: "ceremony GameControl state",
                value: control.state,
            });
        }
        self.state = CeremonyState::FinalStage;
        Ok(control)
    }

    pub fn accept_next_stage(&mut self, packet: &[u8]) -> Result<GameNextStage, DecodeError> {
        if self.state != CeremonyState::FinalStage {
            return Err(DecodeError::InvalidSequence {
                expected: "GameNextStagePacket",
                actual: state_name(self.state),
            });
        }
        let next = decode_game_next_stage(packet)?;
        self.state = CeremonyState::StageAdvanced;
        Ok(next)
    }

    pub fn accept_game_result(
        &mut self,
        packet: &[u8],
    ) -> Result<game_result::GameResult, DecodeError> {
        if self.state != CeremonyState::StageAdvanced {
            return Err(DecodeError::InvalidSequence {
                expected: "GameResultPacket",
                actual: state_name(self.state),
            });
        }
        let result = game_result::decode(packet)?;
        self.state = CeremonyState::Podium;
        Ok(result)
    }
}

pub fn decode_game_control(packet: &[u8]) -> Result<GameControl, DecodeError> {
    let mut reader = Cursor::new(packet);
    reader.expect_hash("GameControlPacket", GAME_CONTROL_HASH)?;
    let state = reader.i32()?;
    reader.u8()?;
    let value0 = reader.u32()?;
    reader.i32()?;
    reader.i32()?;
    reader.u8()?;
    reader.i32()?;
    reader.bytes(40)?;
    reader.bytes(10)?;
    reader.i32()?;
    reader.i32()?;
    let encoded_status = legacy_scalar::decode_u8(reader.u8()?);
    reader.finish()?;
    Ok(GameControl {
        state,
        value0,
        encoded_status,
    })
}

pub fn decode_game_next_stage(packet: &[u8]) -> Result<GameNextStage, DecodeError> {
    let mut reader = Cursor::new(packet);
    reader.expect_hash("GameNextStagePacket", GAME_NEXT_STAGE_HASH)?;
    let game_type = reader.u8()?;
    let stage_marker_1 = reader.i32()?;
    let stage_marker_2 = reader.i32()?;
    reader.finish()?;
    Ok(GameNextStage {
        game_type,
        stage_marker_1,
        stage_marker_2,
    })
}

const fn state_name(state: CeremonyState) -> &'static str {
    match state {
        CeremonyState::Racing => "racing",
        CeremonyState::FinalStage => "final-stage accepted",
        CeremonyState::StageAdvanced => "next-stage accepted",
        CeremonyState::Podium => "podium",
    }
}
