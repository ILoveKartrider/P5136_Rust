//! Independent reconstruction of the newly recovered type-12 item consumers.
//!
//! Pair values, state locations, lengths, actor offsets, and phase branches
//! are repeated here deliberately.  This module has no normal dependency on
//! the server codec and therefore acts as a differential client oracle.

use crate::DecodeError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Meaning {
    Unknown,
    Place,
    Launch,
    Activate,
    Impact,
    Resolve,
    Retarget,
    Remove,
    UpdateRuntimeFlag,
    NoClientAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsumedOperation {
    pub class_name: &'static str,
    pub object_id: u32,
    pub state: u32,
    pub meaning: Meaning,
    pub native_phase: Option<u8>,
    pub transition_token: Option<u32>,
    pub source_object_id: Option<u32>,
    pub target_object_id: Option<u32>,
    pub target_object_ids: Vec<u32>,
    pub variant: Option<u8>,
    pub effect_item_id: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Angel,
    Balloon,
    Block,
    BossPrison,
    BoundRoad,
    BoundWall,
    Cloud,
    Course,
    Devil,
    Emp,
    EventObject,
    Falling,
    GiantTalisman,
    Ghost,
    GoldShield,
    Icefly,
    HeadBand,
    TargetedPhaseThree,
    CokeBomb,
    Cube,
    CubeForBoss,
    SnowBomb,
    InfectedBomb,
    RollingBomb,
    RollingInfected,
    WaterMine,
    TimeMine,
    TimeCokeBomb,
    TimeInfectedBomb,
    TimeSnowBomb,
    BigTimebomb,
    AreaUfo,
    LockdownRocket,
    Magnet,
    NewDevil,
    MovingUfo,
    Press,
    RobotBeam,
    Shield,
    SpecialShield,
    Ufo,
    Thunderbolt,
    ForceZone,
    Oil,
    PirateBomb,
    Scanning,
    Silence,
    Siren,
    SirenShield,
    SlotLock,
    SpaceCraft,
    SpecialSiren,
    SpecialSmall,
    SpeedDown,
    StraightRocket,
    TargetKart,
    TombStone,
    WitchUnionMagic,
}

#[allow(
    clippy::too_many_lines,
    reason = "the independent hash table stays local and auditable against the recovered client"
)]
pub fn consume(raw: &[u8]) -> Result<ConsumedOperation, DecodeError> {
    let operation_hash = u32_at(raw, 0)?;
    let base_hash = u32_at(raw, 4)?;
    let (class_name, kind) = match (operation_hash, base_hash) {
        (0x0D49_030D, 0x184D_042C) => ("GopAngel", Kind::Angel),
        (0x14A6_03ED, 0x21E8_050C) => ("GopBalloon", Kind::Balloon),
        (0x0D59_0311, 0x185D_0430) => ("GopBlock", Kind::Block),
        (0x233A_0538, 0x33D9_0657) => ("GopBossPrison", Kind::BossPrison),
        (0x1DB9_04A4, 0x2D39_05C3) => ("GopBoundRoad", Kind::BoundRoad),
        (0x1DC1_04AE, 0x2D41_05CD) => ("GopBoundWall", Kind::BoundWall),
        (0x0D7B_031D, 0x187F_043C) => ("GopCloud", Kind::Cloud),
        (0x10CA_034F, 0x1CED_046E) => ("GopCloud2", Kind::Cloud),
        (0x1139_0397, 0x0D73_0327) => ("GopCourse", Kind::Course),
        (0x0D69_031A, 0x186D_0439) => ("GopDevil", Kind::Devil),
        (0x07AE_0248, 0x1074_0367) => ("GopEmp", Kind::Emp),
        (0x2856_057F, 0x3A14_069E) => ("GopEventObject", Kind::EventObject),
        (0x14A7_03E3, 0x21E9_0502) => ("GopFalling", Kind::Falling),
        (0x3442_0652, 0x483E_0771) => ("GopGiantTalisman", Kind::GiantTalisman),
        (0x0D8B_032B, 0x188F_044A) => ("GopGhost", Kind::Ghost),
        (0x2271_0505, 0x3310_0624) => ("GopGoldShield", Kind::GoldShield),
        (0x10C3_0382, 0x1CE6_04A1) => ("GopIcefly", Kind::Icefly),
        (0x17FB_040D, 0x265C_052C) => ("GopHeadBand", Kind::HeadBand),
        (0x1977_0461, 0x27D8_0580) => ("GopDynamite", Kind::TargetedPhaseThree),
        (0x10D3_0380, 0x1CF6_049F) => ("GopHammer", Kind::TargetedPhaseThree),
        (0x1476_03D8, 0x21B8_04F7) => ("GopMqDevil", Kind::Devil),
        (0x1900_0448, 0x2761_0567) => ("GopCokebomb", Kind::CokeBomb),
        (0x0A4F_02A5, 0x1434_03C4) => ("GopCube", Kind::Cube),
        (0x273E_0563, 0x38FC_0682) => ("GopCubeForBoss", Kind::CubeForBoss),
        (0x19EB_046D, 0x284C_058C) => ("GopSnowbomb", Kind::SnowBomb),
        (0x2DC1_05C8, 0x409E_06E7) => ("GopInfectedBomb", Kind::InfectedBomb),
        (0x42E4_071F, 0x591E_083E) => ("GopRollingCokebomb", Kind::RollingBomb),
        (0x2954_059D, 0x3B12_06BC) => ("GopRollingbomb", Kind::RollingBomb),
        (0x6381_08BF, 0x7E37_09DE) => ("GopRollingInfectedbomb", Kind::RollingInfected),
        (0x1E04_04B2, 0x2D84_05D1) => ("GopWaterMine", Kind::WaterMine),
        (0x1909_043E, 0x276A_055D) => ("GopTimeMine", Kind::TimeMine),
        (0x41CC_070F, 0x3996_067F) => ("GopItemTimeFlybomb", Kind::TimeCokeBomb),
        (0x2DDA_05D7, 0x40B7_06F6) => ("GopTimeCokebomb", Kind::TimeCokeBomb),
        (0x48D7_0757, 0x6030_0876) => ("GopTimeInfectedBomb", Kind::TimeInfectedBomb),
        (0x2EC5_05FC, 0x41A2_071B) => ("GopTimeSnowbomb", Kind::TimeSnowBomb),
        (0x196A_0455, 0x27CB_0574) => ("GopTimebomb", Kind::TimeSnowBomb),
        (0x276B_0567, 0x3929_0686) => ("GopBigTimebomb", Kind::BigTimebomb),
        (0x1457_03C9, 0x2199_04E8) => ("GopAreaUfo", Kind::AreaUfo),
        (0x3BEA_06CF, 0x5105_07EE) => ("GopLockdownRocket", Kind::LockdownRocket),
        (0x10DE_0382, 0x1D01_04A1) => ("GopMagnet", Kind::Magnet),
        (0x18D8_0444, 0x2739_0563) => ("GopNewDevil", Kind::NewDevil),
        (0x1E52_04C0, 0x2DD2_05DF) => ("GopMovingUfo", Kind::MovingUfo),
        (0x0DC1_0333, 0x18C5_0452) => ("GopPress", Kind::Press),
        (0x1DC5_04A1, 0x2D45_05C0) => ("GopRobotBeam", Kind::RobotBeam),
        (0x1110_037F, 0x1D33_049E) => ("GopShield", Kind::Shield),
        (0x3473_0640, 0x486F_075F) => ("GopSpecialShield", Kind::SpecialShield),
        (0x07CF_0250, 0x1095_036F) => ("GopUfo", Kind::Ufo),
        (0x2973_05B1, 0x3B31_06D0) => ("GopThunderbolt", Kind::Thunderbolt),
        (0x1DC6_04B1, 0x2D46_05D0) => ("GopForceZone", Kind::ForceZone),
        (0x07C0_024A, 0x1086_0369) => ("GopOil", Kind::Oil),
        (0x2369_052B, 0x3408_064A) => ("GopPiratebomb", Kind::PirateBomb),
        (0x1942_0457, 0x27A3_0576) => ("GopScanning", Kind::Scanning),
        (0x150D_03E9, 0x224F_0508) => ("GopSilence", Kind::Silence),
        (0x0DB2_0327, 0x18B6_0446) => ("GopSiren", Kind::Siren),
        (0x28A5_0580, 0x3A63_069F) => ("GopSirenShield", Kind::SirenShield),
        (0x196B_0451, 0x27CC_0570) => ("GopSlotLock", Kind::SlotLock),
        (0x2262_0502, 0x3301_0621) => ("GopSpaceCraft", Kind::SpaceCraft),
        (0x2E54_05E8, 0x4131_0707) => ("GopSpecialSiren", Kind::SpecialSiren),
        (0x2E3D_05E0, 0x411A_06FF) => ("GopSpecialSmall", Kind::SpecialSmall),
        (0x1DB2_04AF, 0x2D32_05CE) => ("GopSpeedDown", Kind::SpeedDown),
        (0x3C6F_06D4, 0x518A_07F3) => ("GopStraightRocket", Kind::StraightRocket),
        (0x22FA_051F, 0x1D74_04AF) => ("GopTargetKart", Kind::TargetKart),
        (0x1E29_04C1, 0x2DA9_05E0) => ("GopTombStone", Kind::TombStone),
        (0x42B8_070F, 0x58F2_082E) => ("GopWitchUnionMagic", Kind::WitchUnionMagic),
        _ => {
            return Err(DecodeError::UnsupportedDiscriminant {
                field: "type-12 operation hash",
                value: i32::from_le_bytes(operation_hash.to_le_bytes()),
            });
        }
    };
    let object_id = u32_at(raw, 8)?;
    let state = match kind {
        Kind::TimeMine | Kind::Block | Kind::BoundRoad | Kind::BoundWall | Kind::Falling => {
            u32_at(raw, 16)?
        }
        Kind::BigTimebomb => u32_at(raw, 13)?,
        Kind::LockdownRocket => u32::from(byte(raw, 12)?),
        _ => u32_at(raw, 12)?,
    };
    require_exact_length(raw, expected_length(kind, state, raw)?)?;

    let (meaning, native_phase, transition_token, source_object_id, target_object_id, variant) =
        bindings(kind, raw, state)?;
    let target_object_ids = if matches!((kind, state), (Kind::Thunderbolt, 1)) {
        counted_object_ids(raw, 25, 29)?
    } else {
        Vec::new()
    };
    let effect_item_id = effect_item_id(class_name, kind, raw, state)?;
    Ok(ConsumedOperation {
        class_name,
        object_id,
        state,
        meaning,
        native_phase,
        transition_token,
        source_object_id,
        target_object_id,
        target_object_ids,
        variant,
        effect_item_id,
    })
}

#[allow(
    clippy::match_same_arms,
    clippy::too_many_lines,
    reason = "class-local writer branches remain explicit so the oracle is auditable against IDA"
)]
fn expected_length(kind: Kind, state: u32, raw: &[u8]) -> Result<usize, DecodeError> {
    let length = match (kind, state) {
        (Kind::Angel, 0) => 25,
        (Kind::Angel, 2) => 28,
        (Kind::Balloon, 1) => 28,
        (Kind::Balloon, 2) => 16,
        (Kind::Block, 1) => 89,
        (Kind::Block, 2) => 29,
        (Kind::BossPrison, 1) => 77,
        (Kind::BossPrison, 2) => 64,
        (Kind::BossPrison, 3) => 68,
        (Kind::BossPrison, 4) => 16,
        (Kind::BoundRoad, 1) => 63,
        (Kind::BoundRoad, 2 | 3) => 33,
        (Kind::BoundWall, 1) => 137,
        (Kind::BoundWall, 2 | 3) => 29,
        (Kind::Cube, 1) => 24,
        (Kind::Cube, 2) => 28,
        (Kind::CubeForBoss, 0) => 69,
        (Kind::CubeForBoss, 1) => 77,
        (Kind::Course, _) => {
            let code_units =
                usize::try_from(u32_at(raw, 16)?).map_err(|_| DecodeError::InvalidCount {
                    field: "Course UTF-16 code-unit count",
                    value: -1,
                    maximum: 468,
                })?;
            if code_units > 468 {
                return Err(DecodeError::InvalidCount {
                    field: "Course UTF-16 code-unit count",
                    value: i32::try_from(code_units).unwrap_or(i32::MAX),
                    maximum: 468,
                });
            }
            24usize
                .checked_add(code_units.checked_mul(2).ok_or(DecodeError::InvalidCount {
                    field: "Course UTF-16 code-unit count",
                    value: -1,
                    maximum: 468,
                })?)
                .ok_or(DecodeError::InvalidCount {
                    field: "Course UTF-16 code-unit count",
                    value: -1,
                    maximum: 468,
                })?
        }
        (Kind::HeadBand, 1) => 21,
        (Kind::HeadBand, 2) => 16,
        (Kind::TargetedPhaseThree, 1) => 30,
        (Kind::TargetedPhaseThree, 2) => 29,
        (Kind::Press, 1) => 72,
        (Kind::Press, 2) => 28,
        (Kind::RobotBeam | Kind::TombStone, 1) => 72,
        (Kind::RobotBeam | Kind::TombStone, 2) => 24,
        (Kind::GiantTalisman | Kind::WitchUnionMagic, _) => 28,
        (Kind::EventObject | Kind::TargetKart, _) => 20,
        (Kind::Cloud, 1) => 73,
        (Kind::Cloud, 2) => 20,
        (Kind::Magnet, 1) => 30,
        (Kind::Devil, 1) => 31,
        (Kind::NewDevil, 1) => 27,
        (Kind::Emp | Kind::SpecialSiren, 0) => 26,
        (Kind::Ghost, 1) | (Kind::SlotLock, 1 | 2) => 29,
        (Kind::GoldShield, 0) => 28,
        (Kind::GoldShield, 2) => 34,
        (Kind::Icefly, 1) => 78,
        (Kind::Falling, 1) => 91,
        (Kind::Falling, 2 | 3) => 33,
        (Kind::Scanning, 1) => 30,
        (Kind::SpaceCraft, 0) => 30,
        (Kind::SpaceCraft, 2..=5) => 29,
        (Kind::SpaceCraft, 7) => 17,
        (Kind::StraightRocket, 1) => 58,
        (Kind::StraightRocket, 2 | 3) => 24,
        (Kind::CokeBomb | Kind::SnowBomb, 1) => 120,
        (Kind::InfectedBomb, 1) => 121,
        (Kind::InfectedBomb, 2 | 3) | (Kind::TimeMine, 2..=4) | (Kind::AreaUfo | Kind::Ufo, 1) => {
            33
        }
        (Kind::RollingBomb | Kind::RollingInfected, 1) => 132,
        (Kind::WaterMine, 1) => 73,
        (Kind::TimeMine, 1) => 85,
        (Kind::RollingInfected | Kind::TimeInfectedBomb, 2) => 32,
        (Kind::WaterMine, 2..=4 | 7)
        | (Kind::BigTimebomb, _)
        | (Kind::Shield, 2)
        | (Kind::Thunderbolt, 3) => 29,
        (Kind::CokeBomb | Kind::SnowBomb | Kind::TimeSnowBomb, 2..=4)
        | (Kind::RollingBomb | Kind::TimeCokeBomb, 2)
        | (Kind::RollingInfected | Kind::TimeInfectedBomb, 3) => 28,
        (Kind::RollingBomb, 3 | 4)
        | (Kind::TimeMine, 5)
        | (Kind::TimeCokeBomb, 1 | 3 | 4)
        | (Kind::TimeInfectedBomb | Kind::TimeSnowBomb, 1)
        | (Kind::AreaUfo | Kind::MovingUfo, 2) => 24,
        (Kind::MovingUfo, 1) => 72,
        (Kind::Ufo, 2) | (Kind::LockdownRocket, 1) => 20,
        (Kind::Shield, 1) => 31,
        (Kind::SpecialShield, 0) => 27,
        (Kind::SpecialShield, 2 | 3) | (Kind::LockdownRocket, 4 | 9) | (Kind::Thunderbolt, 2) => 25,
        (Kind::LockdownRocket, 2) => 17,
        (Kind::LockdownRocket, 3) => 13,
        (Kind::LockdownRocket, 5 | 6) => 18,
        (Kind::LockdownRocket, 7 | 8) => 26,
        (Kind::Thunderbolt, 1) => {
            let count = u32_at(raw, 25)?;
            let count = usize::try_from(count).map_err(|_| DecodeError::InvalidCount {
                field: "Thunderbolt target count",
                value: -1,
                maximum: 232,
            })?;
            if count > 232 {
                return Err(DecodeError::InvalidCount {
                    field: "Thunderbolt target count",
                    value: i32::try_from(count).unwrap_or(i32::MAX),
                    maximum: 232,
                });
            }
            30 + count * 4
        }
        (Kind::ForceZone, 1) => 72,
        (Kind::ForceZone, 2 | 3) => 29,
        (Kind::ForceZone, 5) => 25,
        (Kind::Oil, 1) => 73,
        (Kind::Oil, 2) | (Kind::Silence, 1 | 2) | (Kind::SpecialSmall, 1) => 29,
        (Kind::Oil, 3) | (Kind::SirenShield, 0 | 2) => 25,
        (Kind::PirateBomb, 1..=4) => 28,
        (Kind::Siren, 1) => 26,
        (Kind::Siren, 2) => 31,
        (Kind::SirenShield, 1) => 24,
        (Kind::SpecialSmall, 0) => 30,
        (Kind::SpecialSmall, 2) => 17,
        (Kind::SpeedDown, 1) => 24,
        (Kind::SpeedDown, 2) => 20,
        _ => return unsupported_state(state),
    };
    Ok(length)
}

#[allow(
    clippy::too_many_lines,
    clippy::match_same_arms,
    clippy::type_complexity,
    reason = "one exhaustive match mirrors client branches without sharing server types"
)]
fn bindings(
    kind: Kind,
    raw: &[u8],
    state: u32,
) -> Result<
    (
        Meaning,
        Option<u8>,
        Option<u32>,
        Option<u32>,
        Option<u32>,
        Option<u8>,
    ),
    DecodeError,
> {
    let fields = match (kind, state) {
        (Kind::Angel, 0) => (
            Meaning::Activate,
            Some(0),
            object_id(raw, 16)?,
            object_id(raw, 21)?,
            None,
            Some(byte(raw, 20)?),
        ),
        // The shared defense resolver (`sub_99B4B0`) reaches this branch only
        // when an active Angel blocks an attack and returns item id 11. Its
        // trailing `sub_4E83E0` call inserts the protected kart into the
        // attack object's processed-target container; it does not remove the
        // timed Angel effect from the kart's active-effect collection. The
        // producer builds a fresh impact object while the original Angel
        // remains in active state 1, which permits later blocks during its
        // duration.
        // The receiver normalizes raw 16 and binds raw 20/24 before phase 2.
        // It then passes the stale state-0 member at object +28 to the phase
        // helper instead of the normalized +40 member; preserve that native
        // quirk without discarding the proven wire roles or impact meaning.
        (Kind::Angel, 2) => (
            Meaning::Impact,
            Some(2),
            object_id(raw, 16)?,
            object_id(raw, 20)?,
            object_id(raw, 24)?,
            None,
        ),
        (Kind::GoldShield, 0) => (
            Meaning::Activate,
            Some(0),
            object_id(raw, 16)?,
            object_id(raw, 20)?,
            None,
            u8::try_from(u32_at(raw, 24)?).ok(),
        ),
        (Kind::GoldShield, 2) => (
            Meaning::Impact,
            Some(2),
            object_id(raw, 16)?,
            object_id(raw, 20)?,
            object_id(raw, 24)?,
            u8::try_from(u32_at(raw, 28)?).ok(),
        ),
        (Kind::Balloon, 1) => (
            Meaning::Activate,
            Some(1),
            object_id(raw, 16)?,
            None,
            None,
            Some(byte(raw, 20)?),
        ),
        (Kind::Balloon, 2) => runtime_flag(),
        (Kind::Block, 1) => (
            Meaning::Place,
            Some(0),
            object_id(raw, 20)?,
            object_id(raw, 24)?,
            None,
            Some(byte(raw, 88)?),
        ),
        (Kind::Block, 2) => {
            let hit = byte(raw, 28)? != 0;
            (
                if hit {
                    Meaning::Impact
                } else {
                    Meaning::Resolve
                },
                Some(if hit { 3 } else { 4 }),
                object_id(raw, 20)?,
                None,
                if hit { object_id(raw, 24)? } else { None },
                Some(byte(raw, 28)?),
            )
        }
        (Kind::BossPrison, 1) => (
            Meaning::Launch,
            Some(0),
            object_id(raw, 16)?,
            object_id(raw, 20)?,
            object_id(raw, 24)?,
            Some(byte(raw, 28)?),
        ),
        (Kind::BossPrison, 2) => (Meaning::Impact, None, None, None, None, None),
        (Kind::BossPrison, 3) => (
            Meaning::Resolve,
            Some(3),
            object_id(raw, 64)?,
            None,
            None,
            None,
        ),
        (Kind::BossPrison, 4) => (Meaning::Remove, None, None, None, None, None),
        (Kind::BoundRoad, 1) => (
            Meaning::Place,
            Some(0),
            object_id(raw, 20)?,
            object_id(raw, 53)?,
            None,
            Some(byte(raw, 62)?),
        ),
        (Kind::BoundRoad, 2 | 3) => contact_hazard(raw, state, 2, 5, None)?,
        (Kind::BoundWall, 1) => (
            Meaning::Place,
            Some(0),
            object_id(raw, 20)?,
            object_id(raw, 24)?,
            None,
            Some(byte(raw, 136)?),
        ),
        (Kind::BoundWall, 2 | 3) => {
            let has_target = byte(raw, 28)? != 0;
            (
                if has_target {
                    if state == 2 {
                        Meaning::Impact
                    } else {
                        Meaning::Resolve
                    }
                } else {
                    Meaning::Remove
                },
                has_target.then(|| u8::try_from(state).ok()).flatten(),
                object_id(raw, 20)?,
                None,
                if has_target {
                    object_id(raw, 24)?
                } else {
                    None
                },
                Some(byte(raw, 28)?),
            )
        }
        (Kind::Cube, 1) => (
            Meaning::Impact,
            None,
            object_id(raw, 20)?,
            None,
            object_id(raw, 16)?,
            None,
        ),
        (Kind::Cube, 2) => no_action(),
        (Kind::CubeForBoss, 0) => (
            Meaning::Place,
            Some(0),
            None,
            None,
            None,
            Some(byte(raw, 68)?),
        ),
        (Kind::CubeForBoss, 1) => (
            Meaning::Impact,
            None,
            object_id(raw, 73)?,
            None,
            object_id(raw, 69)?,
            None,
        ),
        (Kind::Course, _) => (
            Meaning::NoClientAction,
            None,
            course_transition_token(raw)?,
            None,
            object_id(raw, 12)?,
            None,
        ),
        (Kind::HeadBand, 1) => (
            Meaning::Activate,
            Some(1),
            object_id(raw, 16)?,
            None,
            None,
            Some(byte(raw, 20)?),
        ),
        (Kind::HeadBand, 2) => runtime_flag(),
        (Kind::TargetedPhaseThree, 1 | 2) => (
            if state == 1 {
                Meaning::Activate
            } else {
                Meaning::Impact
            },
            Some(if state == 1 { 0 } else { 3 }),
            object_id(raw, 16)?,
            object_id(raw, 20)?,
            object_id(raw, 24)?,
            Some(byte(raw, 28)?),
        ),
        (Kind::Press, 1) => (
            Meaning::Place,
            Some(0),
            object_id(raw, 16)?,
            object_id(raw, 20)?,
            None,
            None,
        ),
        (Kind::Press, 2) => (
            Meaning::Impact,
            Some(5),
            object_id(raw, 16)?,
            object_id(raw, 20)?,
            object_id(raw, 24)?,
            None,
        ),
        (Kind::RobotBeam | Kind::TombStone, 1) => (
            Meaning::Place,
            Some(0),
            object_id(raw, 16)?,
            object_id(raw, 20)?,
            None,
            None,
        ),
        (Kind::RobotBeam, 2) => asymmetric_spatial_impact(raw, 2)?,
        (Kind::TombStone, 2) => asymmetric_spatial_impact(raw, 1)?,
        (Kind::GiantTalisman | Kind::WitchUnionMagic, _) => (
            Meaning::Unknown,
            u8::try_from(state).ok(),
            object_id(raw, 16)?,
            object_id(raw, 24)?,
            object_id(raw, 20)?,
            None,
        ),
        (Kind::EventObject, _) => (
            Meaning::Unknown,
            None,
            object_id(raw, 16)?,
            None,
            object_id(raw, 12)?,
            None,
        ),
        (Kind::TargetKart, 2) => no_action(),
        (Kind::Emp, 0) => (
            Meaning::Activate,
            Some(0),
            object_id(raw, 16)?,
            object_id(raw, 22)?,
            None,
            Some(byte(raw, 20)?),
        ),
        (Kind::Ghost, 1) => (
            Meaning::Activate,
            Some(0),
            object_id(raw, 16)?,
            object_id(raw, 20)?,
            object_id(raw, 24)?,
            Some(byte(raw, 28)?),
        ),
        (Kind::Icefly, 1) => (
            Meaning::Launch,
            Some(0),
            object_id(raw, 16)?,
            object_id(raw, 20)?,
            object_id(raw, 24)?,
            Some(byte(raw, 77)?),
        ),
        (Kind::Falling, 1) => (
            Meaning::Launch,
            Some(0),
            object_id(raw, 20)?,
            object_id(raw, 85)?,
            None,
            Some(byte(raw, 90)?),
        ),
        (Kind::Falling, 2 | 3) => contact_hazard(raw, state, 3, 5, Some(29))?,
        (Kind::Scanning, 1) => (
            Meaning::Activate,
            Some(0),
            object_id(raw, 20)?,
            object_id(raw, 16)?,
            object_id(raw, 24)?,
            Some(byte(raw, 28)?),
        ),
        (Kind::SlotLock, 1) => (
            Meaning::Activate,
            Some(0),
            object_id(raw, 24)?,
            object_id(raw, 16)?,
            None,
            Some(byte(raw, 28)?),
        ),
        (Kind::SlotLock, 2) => (
            Meaning::Impact,
            Some(1),
            object_id(raw, 24)?,
            object_id(raw, 16)?,
            object_id(raw, 20)?,
            Some(byte(raw, 28)?),
        ),
        (Kind::SpecialSiren, 0) => (
            Meaning::Activate,
            None,
            object_id(raw, 16)?,
            object_id(raw, 21)?,
            None,
            Some(byte(raw, 20)?),
        ),
        (Kind::SpaceCraft, 0) => (
            Meaning::Launch,
            Some(0),
            object_id(raw, 16)?,
            object_id(raw, 24)?,
            object_id(raw, 20)?,
            Some(byte(raw, 29)?),
        ),
        (Kind::SpaceCraft, 2) => (
            Meaning::Impact,
            Some(2),
            object_id(raw, 16)?,
            object_id(raw, 24)?,
            object_id(raw, 20)?,
            Some(byte(raw, 28)?),
        ),
        (Kind::SpaceCraft, 3 | 5) => (
            Meaning::Resolve,
            Some(if state == 3 { 3 } else { 5 }),
            object_id(raw, 16)?,
            object_id(raw, 24)?,
            object_id(raw, 20)?,
            Some(byte(raw, 28)?),
        ),
        (Kind::SpaceCraft, 4) => (
            Meaning::Resolve,
            Some(6),
            object_id(raw, 16)?,
            None,
            object_id(raw, 20)?,
            Some(byte(raw, 28)?),
        ),
        (Kind::SpaceCraft, 7) => (
            Meaning::UpdateRuntimeFlag,
            None,
            None,
            None,
            None,
            Some(byte(raw, 16)?),
        ),
        (Kind::StraightRocket, 1) => (
            Meaning::Launch,
            Some(1),
            object_id(raw, 16)?,
            object_id(raw, 20)?,
            None,
            Some(byte(raw, 56)?),
        ),
        // The concrete consumer accepts these writer shapes but performs no
        // class-specific binding, phase transition, or helper call.
        (Kind::StraightRocket, 2 | 3) => no_action(),
        (Kind::Cloud, 1) => (
            Meaning::Place,
            Some(0),
            object_id(raw, 16)?,
            object_id(raw, 20)?,
            None,
            Some(byte(raw, 24)?),
        ),
        (Kind::Cloud, 2) => (
            Meaning::Impact,
            Some(2),
            None,
            None,
            object_id(raw, 16)?,
            None,
        ),
        (Kind::Magnet, 1) => (
            Meaning::Activate,
            Some(1),
            object_id(raw, 16)?,
            object_id(raw, 20)?,
            object_id(raw, 24)?,
            None,
        ),
        (Kind::Devil, 1) => (
            Meaning::Activate,
            Some(0),
            object_id(raw, 16)?,
            object_id(raw, 21)?,
            if byte(raw, 20)? == 5 {
                object_id(raw, 27)?
            } else {
                None
            },
            Some(byte(raw, 20)?),
        ),
        (Kind::NewDevil, 1) => (
            Meaning::Activate,
            Some(0),
            object_id(raw, 16)?,
            object_id(raw, 21)?,
            None,
            Some(byte(raw, 20)?),
        ),
        (Kind::CokeBomb | Kind::SnowBomb, 1) => (
            Meaning::Launch,
            Some(0),
            object_id(raw, 16)?,
            object_id(raw, 20)?,
            None,
            None,
        ),
        (Kind::CokeBomb | Kind::SnowBomb, 2) => (
            Meaning::Impact,
            Some(2),
            object_id(raw, 16)?,
            object_id(raw, 24)?,
            object_id(raw, 20)?,
            None,
        ),
        (Kind::CokeBomb | Kind::SnowBomb, 3) => common_actor(raw, 3)?,
        (Kind::CokeBomb, 4) => (
            Meaning::Resolve,
            Some(4),
            object_id(raw, 16)?,
            object_id(raw, 24)?,
            object_id(raw, 20)?,
            None,
        ),
        (Kind::SnowBomb, 4) => common_actor(raw, 4)?,
        (Kind::InfectedBomb, 1) => (
            Meaning::Launch,
            Some(0),
            object_id(raw, 16)?,
            object_id(raw, 20)?,
            None,
            Some(byte(raw, 120)?),
        ),
        (Kind::InfectedBomb, 2) => (
            Meaning::Impact,
            Some(2),
            object_id(raw, 16)?,
            object_id(raw, 29)?,
            object_id(raw, 20)?,
            Some(byte(raw, 28)?),
        ),
        (Kind::InfectedBomb, 3) => (
            Meaning::Resolve,
            Some(4),
            object_id(raw, 16)?,
            object_id(raw, 29)?,
            object_id(raw, 20)?,
            Some(byte(raw, 28)?),
        ),
        (
            Kind::RollingBomb
            | Kind::RollingInfected
            | Kind::TimeCokeBomb
            | Kind::TimeInfectedBomb
            | Kind::TimeSnowBomb,
            1,
        ) => launch_at_20(raw)?,
        (Kind::RollingBomb | Kind::TimeCokeBomb | Kind::TimeSnowBomb, 2) => impact_20_24(raw)?,
        (Kind::RollingBomb | Kind::TimeCokeBomb | Kind::TimeSnowBomb, 3 | 4) => {
            common_actor(raw, state)?
        }
        (Kind::RollingInfected | Kind::TimeInfectedBomb, 2) => (
            Meaning::Impact,
            Some(2),
            object_id(raw, 16)?,
            object_id(raw, 28)?,
            object_id(raw, 20)?,
            None,
        ),
        (Kind::RollingInfected | Kind::TimeInfectedBomb, 3) => (
            Meaning::Resolve,
            Some(4),
            object_id(raw, 16)?,
            None,
            object_id(raw, 20)?,
            None,
        ),
        (Kind::WaterMine, 1) => (
            Meaning::Place,
            Some(0),
            object_id(raw, 16)?,
            object_id(raw, 68)?,
            None,
            Some(byte(raw, 72)?),
        ),
        (Kind::WaterMine, 2..=4) => (
            if state == 2 {
                Meaning::Impact
            } else {
                Meaning::Resolve
            },
            u8::try_from(state).ok(),
            object_id(raw, 16)?,
            object_id(raw, 24)?,
            object_id(raw, 20)?,
            Some(byte(raw, 28)?),
        ),
        (Kind::WaterMine, 7) | (Kind::TimeMine, 4) => no_action(),
        (Kind::TimeMine, 1) => (
            Meaning::Place,
            Some(0),
            object_id(raw, 20)?,
            object_id(raw, 81)?,
            None,
            Some(byte(raw, 80)?),
        ),
        (Kind::TimeMine, 2 | 3) => {
            let flag = byte(raw, 28)?;
            let target = if flag == 0 { None } else { object_id(raw, 24)? };
            (
                if state == 2 && flag != 0 {
                    Meaning::Impact
                } else {
                    Meaning::Resolve
                },
                None,
                object_id(raw, 20)?,
                None,
                target,
                Some(flag),
            )
        }
        (Kind::TimeMine, 5) => (
            Meaning::Resolve,
            None,
            object_id(raw, 20)?,
            None,
            None,
            None,
        ),
        (Kind::BigTimebomb, _) => big_timebomb_fields(raw, state)?,
        (Kind::AreaUfo | Kind::Ufo, 1) => (
            Meaning::Activate,
            Some(0),
            object_id(raw, 16)?,
            object_id(raw, 25)?,
            object_id(raw, 21)?,
            Some(byte(raw, 20)?),
        ),
        (Kind::AreaUfo, 2) => {
            let actor = object_id(raw, 20)?;
            (
                Meaning::Resolve,
                Some(5),
                object_id(raw, 16)?,
                actor,
                actor,
                None,
            )
        }
        (Kind::MovingUfo, 1) => (
            Meaning::Activate,
            Some(0),
            object_id(raw, 16)?,
            object_id(raw, 20)?,
            None,
            None,
        ),
        (Kind::MovingUfo, 2) => (Meaning::Impact, None, object_id(raw, 16)?, None, None, None),
        (Kind::Ufo, 2) => (Meaning::Resolve, None, None, None, None, None),
        (Kind::Shield, 1) => (
            Meaning::Activate,
            Some(0),
            object_id(raw, 18)?,
            object_id(raw, 22)?,
            None,
            Some(byte(raw, 30)?),
        ),
        (Kind::Shield, 2) => (
            Meaning::Impact,
            Some(1),
            object_id(raw, 16)?,
            object_id(raw, 20)?,
            object_id(raw, 24)?,
            Some(byte(raw, 28)?),
        ),
        (Kind::SpecialShield, 0) => (
            Meaning::Activate,
            Some(0),
            object_id(raw, 17)?,
            object_id(raw, 22)?,
            None,
            Some(byte(raw, 16)?),
        ),
        (Kind::SpecialShield, 2 | 3) => (
            if state == 2 {
                Meaning::Impact
            } else {
                Meaning::Resolve
            },
            u8::try_from(state).ok(),
            object_id(raw, 17)?,
            object_id(raw, 21)?,
            None,
            Some(byte(raw, 16)?),
        ),
        (Kind::LockdownRocket, 1) => (
            Meaning::Launch,
            Some(0),
            object_id(raw, 14)?,
            None,
            None,
            Some(byte(raw, 13)?),
        ),
        (Kind::LockdownRocket, 2) => (
            Meaning::Retarget,
            None,
            None,
            None,
            object_id(raw, 13)?,
            None,
        ),
        (Kind::LockdownRocket, 3) => (Meaning::Remove, None, None, None, None, None),
        (Kind::LockdownRocket, 4) => lockdown_actor_transition(raw, 1, Meaning::Impact)?,
        (Kind::LockdownRocket, 5 | 6) => (
            Meaning::Resolve,
            None,
            object_id(raw, 13)?,
            None,
            None,
            Some(byte(raw, 17)?),
        ),
        (Kind::LockdownRocket, 7) => lockdown_variant_transition(raw, 7, 8)?,
        (Kind::LockdownRocket, 8) => lockdown_variant_transition(raw, 11, 10)?,
        (Kind::LockdownRocket, 9) => lockdown_actor_transition(raw, 9, Meaning::Resolve)?,
        (Kind::Thunderbolt, 1) => (
            Meaning::Activate,
            Some(0),
            object_id(raw, 16)?,
            object_id(raw, 21)?,
            None,
            Some(*raw.last().ok_or(DecodeError::UnexpectedEof {
                offset: 0,
                needed: 1,
                remaining: 0,
            })?),
        ),
        (Kind::Thunderbolt, 2 | 3) => thunderbolt_impact(raw, state)?,
        (Kind::ForceZone, 1) => (
            Meaning::Place,
            Some(0),
            object_id(raw, 16)?,
            object_id(raw, 68)?,
            None,
            None,
        ),
        (Kind::ForceZone, 2 | 3) => force_zone_result(raw, state)?,
        (Kind::ForceZone, 5) => {
            let succeeded = byte(raw, 24)? != 0;
            (
                Meaning::Resolve,
                succeeded.then_some(5),
                None,
                if succeeded { object_id(raw, 20)? } else { None },
                None,
                Some(byte(raw, 24)?),
            )
        }
        (Kind::Oil, 1) => (
            Meaning::Place,
            Some(0),
            object_id(raw, 16)?,
            object_id(raw, 69)?,
            None,
            Some(byte(raw, 20)?),
        ),
        (Kind::Oil, 2) => conditional_impact_or_remove(raw, 25, 20, 2)?,
        (Kind::Oil, 3) => conditional_resolve_or_remove(raw, 20, None, 3)?,
        (Kind::PirateBomb, 1) => (
            Meaning::Activate,
            Some(0),
            object_id(raw, 16)?,
            object_id(raw, 20)?,
            object_id(raw, 24)?,
            None,
        ),
        (Kind::PirateBomb, 2) => (
            Meaning::Impact,
            Some(2),
            object_id(raw, 16)?,
            None,
            object_id(raw, 24)?,
            None,
        ),
        (Kind::PirateBomb, 3) => (
            Meaning::Remove,
            Some(3),
            object_id(raw, 16)?,
            object_id(raw, 20)?,
            object_id(raw, 24)?,
            None,
        ),
        (Kind::PirateBomb, 4) => (
            Meaning::Resolve,
            Some(4),
            object_id(raw, 16)?,
            object_id(raw, 24)?,
            object_id(raw, 24)?,
            None,
        ),
        (Kind::Silence, 1) => (
            Meaning::Activate,
            Some(0),
            object_id(raw, 17)?,
            object_id(raw, 21)?,
            object_id(raw, 25)?,
            Some(byte(raw, 16)?),
        ),
        (Kind::Silence, 2) => no_action(),
        (Kind::Siren, 1) => (
            Meaning::Activate,
            Some(0),
            object_id(raw, 16)?,
            object_id(raw, 21)?,
            None,
            Some(byte(raw, 20)?),
        ),
        (Kind::Siren, 2) => (
            Meaning::Impact,
            Some(1),
            object_id(raw, 16)?,
            object_id(raw, 24)?,
            object_id(raw, 20)?,
            Some(byte(raw, 28)?),
        ),
        (Kind::SirenShield, 0 | 2) => (
            if state == 0 {
                Meaning::Activate
            } else {
                Meaning::Resolve
            },
            u8::try_from(state).ok(),
            object_id(raw, 16)?,
            object_id(raw, 21)?,
            None,
            Some(byte(raw, 20)?),
        ),
        (Kind::SirenShield, 1) => {
            let actor = object_id(raw, 20)?;
            (
                Meaning::Impact,
                Some(1),
                object_id(raw, 16)?,
                actor,
                actor,
                None,
            )
        }
        (Kind::SpecialSmall, 0) => (
            Meaning::Activate,
            Some(0),
            object_id(raw, 16)?,
            object_id(raw, 24)?,
            object_id(raw, 20)?,
            Some(byte(raw, 29)?),
        ),
        (Kind::SpecialSmall, 1) => (
            Meaning::Impact,
            Some(3),
            object_id(raw, 16)?,
            object_id(raw, 24)?,
            object_id(raw, 20)?,
            Some(byte(raw, 28)?),
        ),
        (Kind::SpecialSmall, 2) => (
            Meaning::UpdateRuntimeFlag,
            None,
            None,
            None,
            None,
            Some(byte(raw, 16)?),
        ),
        (Kind::SpeedDown, 1) => (
            Meaning::Activate,
            Some(0),
            object_id(raw, 16)?,
            None,
            object_id(raw, 20)?,
            None,
        ),
        (Kind::SpeedDown, 2) => (
            Meaning::Remove,
            Some(2),
            object_id(raw, 16)?,
            None,
            None,
            None,
        ),
        _ => return unsupported_state(state),
    };
    Ok(fields)
}

type Fields = (
    Meaning,
    Option<u8>,
    Option<u32>,
    Option<u32>,
    Option<u32>,
    Option<u8>,
);

fn runtime_flag() -> Fields {
    (Meaning::UpdateRuntimeFlag, None, None, None, None, None)
}

fn asymmetric_spatial_impact(raw: &[u8], native_phase: u8) -> Result<Fields, DecodeError> {
    Ok((
        Meaning::Unknown,
        Some(native_phase),
        object_id(raw, 16)?,
        None,
        object_id(raw, 20)?,
        None,
    ))
}

fn big_timebomb_fields(raw: &[u8], state: u32) -> Result<Fields, DecodeError> {
    let source = object_id(raw, 25)?;
    let target = object_id(raw, 21)?;
    let Some((source, target)) = source.zip(target) else {
        return Ok((Meaning::Unknown, None, None, None, None, None));
    };
    let meaning = match state {
        0 => Meaning::Activate,
        2 | 3 => Meaning::Impact,
        4 => Meaning::Resolve,
        _ => Meaning::Unknown,
    };
    Ok((
        meaning,
        u8::try_from(state).ok(),
        object_id(raw, 17)?,
        Some(source),
        Some(target),
        Some(byte(raw, 12)?),
    ))
}

fn lockdown_actor_transition(
    raw: &[u8],
    phase: u8,
    meaning: Meaning,
) -> Result<Fields, DecodeError> {
    Ok((
        meaning,
        Some(phase),
        object_id(raw, 13)?,
        object_id(raw, 17)?,
        object_id(raw, 21)?,
        None,
    ))
}

fn lockdown_variant_transition(
    raw: &[u8],
    phase_if_zero: u8,
    phase_if_nonzero: u8,
) -> Result<Fields, DecodeError> {
    let variant = byte(raw, 25)?;
    Ok((
        Meaning::Resolve,
        Some(if variant == 0 {
            phase_if_zero
        } else {
            phase_if_nonzero
        }),
        object_id(raw, 13)?,
        object_id(raw, 17)?,
        object_id(raw, 21)?,
        Some(variant),
    ))
}

fn thunderbolt_impact(raw: &[u8], state: u32) -> Result<Fields, DecodeError> {
    let target = object_id(raw, 21)?;
    let Some(target) = target else {
        return Ok((Meaning::Impact, None, None, None, None, None));
    };
    Ok((
        Meaning::Impact,
        Some(if state == 2 { 4 } else { 3 }),
        object_id(raw, 17)?,
        if state == 3 {
            object_id(raw, 25)?
        } else {
            None
        },
        Some(target),
        None,
    ))
}

fn conditional_impact_or_remove(
    raw: &[u8],
    source_offset: usize,
    target_offset: usize,
    phase: u8,
) -> Result<Fields, DecodeError> {
    let succeeded = byte(raw, 24)? != 0;
    Ok((
        if succeeded {
            Meaning::Impact
        } else {
            Meaning::Remove
        },
        succeeded.then_some(phase),
        object_id(raw, 16)?,
        object_id(raw, source_offset)?,
        if succeeded {
            object_id(raw, target_offset)?
        } else {
            None
        },
        Some(byte(raw, 24)?),
    ))
}

fn force_zone_result(raw: &[u8], state: u32) -> Result<Fields, DecodeError> {
    let succeeded = byte(raw, 24)? != 0;
    let (meaning, source, target, phase) = match state {
        2 => (
            if succeeded {
                Meaning::Impact
            } else {
                Meaning::Resolve
            },
            object_id(raw, 25)?,
            if succeeded { object_id(raw, 20)? } else { None },
            succeeded.then_some(2),
        ),
        3 => (
            Meaning::Resolve,
            if succeeded { object_id(raw, 20)? } else { None },
            if succeeded { object_id(raw, 25)? } else { None },
            succeeded.then_some(3),
        ),
        _ => return unsupported_state(state),
    };
    Ok((
        meaning,
        phase,
        object_id(raw, 16)?,
        source,
        target,
        Some(byte(raw, 24)?),
    ))
}

fn conditional_resolve_or_remove(
    raw: &[u8],
    source_offset: usize,
    target_offset: Option<usize>,
    phase: u8,
) -> Result<Fields, DecodeError> {
    let succeeded = byte(raw, 24)? != 0;
    Ok((
        if succeeded {
            Meaning::Resolve
        } else {
            Meaning::Remove
        },
        succeeded.then_some(phase),
        object_id(raw, 16)?,
        if succeeded {
            object_id(raw, source_offset)?
        } else {
            None
        },
        if succeeded {
            target_offset
                .map(|offset| object_id(raw, offset))
                .transpose()?
                .flatten()
        } else {
            None
        },
        Some(byte(raw, 24)?),
    ))
}

fn counted_object_ids(
    raw: &[u8],
    count_offset: usize,
    items_offset: usize,
) -> Result<Vec<u32>, DecodeError> {
    let count =
        usize::try_from(u32_at(raw, count_offset)?).map_err(|_| DecodeError::InvalidCount {
            field: "Thunderbolt target count",
            value: -1,
            maximum: 232,
        })?;
    let byte_length = count.checked_mul(4).ok_or(DecodeError::InvalidCount {
        field: "Thunderbolt target count",
        value: i32::try_from(count).unwrap_or(i32::MAX),
        maximum: 232,
    })?;
    let end = items_offset
        .checked_add(byte_length)
        .ok_or(DecodeError::InvalidCount {
            field: "Thunderbolt target count",
            value: i32::try_from(count).unwrap_or(i32::MAX),
            maximum: 232,
        })?;
    let bytes = raw
        .get(items_offset..end)
        .ok_or(DecodeError::UnexpectedEof {
            offset: items_offset,
            needed: byte_length,
            remaining: raw.len().saturating_sub(items_offset),
        })?;
    bytes
        .chunks_exact(4)
        .map(|chunk| {
            Ok(u32::from_le_bytes(
                chunk.try_into().expect("chunks_exact proves four bytes"),
            ))
        })
        .collect()
}

fn launch_at_20(raw: &[u8]) -> Result<Fields, DecodeError> {
    Ok((
        Meaning::Launch,
        Some(0),
        object_id(raw, 16)?,
        object_id(raw, 20)?,
        None,
        None,
    ))
}

fn impact_20_24(raw: &[u8]) -> Result<Fields, DecodeError> {
    Ok((
        Meaning::Impact,
        Some(2),
        object_id(raw, 16)?,
        object_id(raw, 24)?,
        object_id(raw, 20)?,
        None,
    ))
}

fn common_actor(raw: &[u8], phase: u32) -> Result<Fields, DecodeError> {
    let actor = object_id(raw, 20)?;
    Ok((
        Meaning::Resolve,
        u8::try_from(phase).ok(),
        object_id(raw, 16)?,
        actor,
        actor,
        None,
    ))
}

fn contact_hazard(
    raw: &[u8],
    state: u32,
    state_two_phase: u8,
    state_three_phase: u8,
    source_offset: Option<usize>,
) -> Result<Fields, DecodeError> {
    let has_target = byte(raw, 28)? != 0;
    Ok((
        if !has_target {
            Meaning::Remove
        } else if state == 2 {
            Meaning::Impact
        } else {
            Meaning::Resolve
        },
        has_target.then_some(if state == 2 {
            state_two_phase
        } else {
            state_three_phase
        }),
        object_id(raw, 20)?,
        if has_target {
            source_offset
                .map(|offset| object_id(raw, offset))
                .transpose()?
                .flatten()
        } else {
            None
        },
        if has_target {
            object_id(raw, 24)?
        } else {
            None
        },
        Some(byte(raw, 28)?),
    ))
}

fn course_transition_token(raw: &[u8]) -> Result<Option<u32>, DecodeError> {
    let code_units = usize::try_from(u32_at(raw, 16)?).map_err(|_| DecodeError::InvalidCount {
        field: "Course UTF-16 code-unit count",
        value: -1,
        maximum: 468,
    })?;
    let string_bytes = code_units.checked_mul(2).ok_or(DecodeError::InvalidCount {
        field: "Course UTF-16 code-unit count",
        value: -1,
        maximum: 468,
    })?;
    let token_offset = 20usize
        .checked_add(string_bytes)
        .ok_or(DecodeError::InvalidCount {
            field: "Course UTF-16 code-unit count",
            value: -1,
            maximum: 468,
        })?;
    object_id(raw, token_offset)
}

const fn no_action() -> Fields {
    (Meaning::NoClientAction, None, None, None, None, None)
}

fn object_id(raw: &[u8], offset: usize) -> Result<Option<u32>, DecodeError> {
    let value = u32_at(raw, offset)?;
    Ok((value != u32::MAX).then_some(value))
}

fn gold_shield_effect_item_id(raw: &[u8], state: u32) -> Result<u16, DecodeError> {
    let kind = u32_at(raw, if state == 0 { 24 } else { 28 })?;
    if state == 2 && u16_at(raw, 32)? == 106 {
        return Ok(106);
    }
    match kind {
        0 => Ok(36),
        3 => Ok(81),
        _ => Err(DecodeError::UnsupportedDiscriminant {
            field: "GopGoldShield shield kind",
            value: i32::from_le_bytes(kind.to_le_bytes()),
        }),
    }
}

fn effect_item_id(
    class_name: &str,
    kind: Kind,
    raw: &[u8],
    state: u32,
) -> Result<Option<u16>, DecodeError> {
    let item_id = match (kind, state) {
        (Kind::GoldShield, _) => Some(gold_shield_effect_item_id(raw, state)?),
        (Kind::Shield, 1) => Some(u16_at(raw, 16)?),
        (Kind::Cloud, 1) => match (class_name, byte(raw, 24)?) {
            ("GopCloud", 0) => Some(0),
            ("GopCloud", 3) => Some(1),
            ("GopCloud", 6) => Some(43),
            ("GopCloud2", 0) => Some(114),
            ("GopCloud2", 3) => Some(115),
            ("GopCloud2", 6) => Some(116),
            _ => None,
        },
        (Kind::BigTimebomb, _) => Some(122),
        (Kind::Icefly, _) => Some(80),
        (Kind::SpecialShield, _) => Some(40),
        (Kind::StraightRocket, _) => Some(73),
        (Kind::TimeSnowBomb, _) if class_name == "GopTimebomb" => Some(13),
        _ => None,
    };
    Ok(item_id)
}

fn byte(raw: &[u8], offset: usize) -> Result<u8, DecodeError> {
    raw.get(offset).copied().ok_or(DecodeError::UnexpectedEof {
        offset,
        needed: 1,
        remaining: raw.len().saturating_sub(offset),
    })
}

fn u32_at(raw: &[u8], offset: usize) -> Result<u32, DecodeError> {
    let Some(end) = offset.checked_add(4) else {
        return Err(DecodeError::UnexpectedEof {
            offset,
            needed: 4,
            remaining: raw.len().saturating_sub(offset),
        });
    };
    let bytes = raw.get(offset..end).ok_or(DecodeError::UnexpectedEof {
        offset,
        needed: 4,
        remaining: raw.len().saturating_sub(offset),
    })?;
    let mut value = [0_u8; 4];
    value.copy_from_slice(bytes);
    Ok(u32::from_le_bytes(value))
}

fn u16_at(raw: &[u8], offset: usize) -> Result<u16, DecodeError> {
    let Some(end) = offset.checked_add(2) else {
        return Err(DecodeError::UnexpectedEof {
            offset,
            needed: 2,
            remaining: raw.len().saturating_sub(offset),
        });
    };
    let bytes = raw.get(offset..end).ok_or(DecodeError::UnexpectedEof {
        offset,
        needed: 2,
        remaining: raw.len().saturating_sub(offset),
    })?;
    let mut value = [0_u8; 2];
    value.copy_from_slice(bytes);
    Ok(u16::from_le_bytes(value))
}

fn require_exact_length(raw: &[u8], expected: usize) -> Result<(), DecodeError> {
    use std::cmp::Ordering;

    match raw.len().cmp(&expected) {
        Ordering::Less => Err(DecodeError::UnexpectedEof {
            offset: raw.len(),
            needed: expected - raw.len(),
            remaining: 0,
        }),
        Ordering::Greater => Err(DecodeError::TrailingBytes {
            offset: expected,
            remaining: raw.len() - expected,
        }),
        Ordering::Equal => Ok(()),
    }
}

fn unsupported_state<T>(state: u32) -> Result<T, DecodeError> {
    Err(DecodeError::UnsupportedDiscriminant {
        field: "type-12 item state",
        value: i32::from_le_bytes(state.to_le_bytes()),
    })
}
