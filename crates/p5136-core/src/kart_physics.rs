//! Pure Korean P5136 kart-physics snapshot calculation and wire encoding.
//!
//! This module deliberately knows nothing about profiles, XML/JSON catalogs,
//! or mutable room state. Callers resolve those sources first, including the
//! mutations performed by the C# `V2Specs.ExceedSpec`, then pass one immutable
//! snapshot to [`build_p5136_kart_physics_block`].
//!
//! The field formulas and order mirror
//! `StartGameData.GetKartSpac` through `WritePost5136KartSpec`. Post-P5136
//! fields are intentionally absent.

use std::{error::Error, fmt};

use crate::{
    encoded,
    race_start_protocol::{P5136_KART_PHYSICS_BLOCK_LENGTH, P5136KartPhysicsBlock},
};

pub const P5136_KART_PHYSICS_FIELD_COUNT: usize = 70;
pub const P5136_ENCODED_F32_COUNT: usize = 49;
pub const P5136_ENCODED_I32_COUNT: usize = 6;
pub const P5136_ENCODED_U8_COUNT: usize = 15;
pub const P5136_S4_DRIFT_MAX_GAUGE: f32 = 1.0;
pub const P5136_S6_BOOSTER_TIME: f32 = 2_000_000.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodedPhysicsFieldKind {
    F32,
    I32,
    U8,
}

impl EncodedPhysicsFieldKind {
    #[must_use]
    pub const fn wire_length(self) -> usize {
        match self {
            Self::F32 | Self::I32 => 4,
            Self::U8 => 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicsFieldLayout {
    pub name: &'static str,
    pub offset: usize,
    pub kind: EncodedPhysicsFieldKind,
}

const fn field(
    name: &'static str,
    offset: usize,
    kind: EncodedPhysicsFieldKind,
) -> PhysicsFieldLayout {
    PhysicsFieldLayout { name, offset, kind }
}

/// Complete P5136 field map, copied from the C# `KartSpecLog` decode order.
pub const P5136_KART_PHYSICS_LAYOUT: [PhysicsFieldLayout; P5136_KART_PHYSICS_FIELD_COUNT] = [
    field("draftMulAccelFactor", 0, EncodedPhysicsFieldKind::F32),
    field("draftTick", 4, EncodedPhysicsFieldKind::I32),
    field("driftBoostMulAccelFactor", 8, EncodedPhysicsFieldKind::F32),
    field("driftBoostTick", 12, EncodedPhysicsFieldKind::I32),
    field("chargeBoostBySpeed", 16, EncodedPhysicsFieldKind::F32),
    field("SpeedSlotCapacity", 20, EncodedPhysicsFieldKind::U8),
    field("ItemSlotCapacity", 21, EncodedPhysicsFieldKind::U8),
    field("SpecialSlotCapacity", 22, EncodedPhysicsFieldKind::U8),
    field("UseTransformBooster", 23, EncodedPhysicsFieldKind::U8),
    field("motorcycleType", 24, EncodedPhysicsFieldKind::U8),
    field("BikeRearWheel", 25, EncodedPhysicsFieldKind::U8),
    field("Mass", 26, EncodedPhysicsFieldKind::F32),
    field("AirFriction", 30, EncodedPhysicsFieldKind::F32),
    field("DragFactor", 34, EncodedPhysicsFieldKind::F32),
    field("ForwardAccelForce", 38, EncodedPhysicsFieldKind::F32),
    field("BackwardAccelForce", 42, EncodedPhysicsFieldKind::F32),
    field("GripBrakeForce", 46, EncodedPhysicsFieldKind::F32),
    field("SlipBrakeForce", 50, EncodedPhysicsFieldKind::F32),
    field("MaxSteerAngle", 54, EncodedPhysicsFieldKind::F32),
    field("SteerConstraint", 58, EncodedPhysicsFieldKind::F32),
    field("FrontGripFactor", 62, EncodedPhysicsFieldKind::F32),
    field("RearGripFactor", 66, EncodedPhysicsFieldKind::F32),
    field("DriftTriggerFactor", 70, EncodedPhysicsFieldKind::F32),
    field("DriftTriggerTime", 74, EncodedPhysicsFieldKind::F32),
    field("DriftSlipFactor", 78, EncodedPhysicsFieldKind::F32),
    field("DriftEscapeForce", 82, EncodedPhysicsFieldKind::F32),
    field("CornerDrawFactor", 86, EncodedPhysicsFieldKind::F32),
    field("DriftLeanFactor", 90, EncodedPhysicsFieldKind::F32),
    field("SteerLeanFactor", 94, EncodedPhysicsFieldKind::F32),
    field("DriftMaxGauge", 98, EncodedPhysicsFieldKind::F32),
    field("NormalBoosterTime", 102, EncodedPhysicsFieldKind::F32),
    field("ItemBoosterTime", 106, EncodedPhysicsFieldKind::F32),
    field("TeamBoosterTime", 110, EncodedPhysicsFieldKind::F32),
    field("AnimalBoosterTime", 114, EncodedPhysicsFieldKind::F32),
    field("SuperBoosterTime", 118, EncodedPhysicsFieldKind::F32),
    field("TransAccelFactor", 122, EncodedPhysicsFieldKind::F32),
    field("BoostAccelFactor", 126, EncodedPhysicsFieldKind::F32),
    field("StartBoosterTimeItem", 130, EncodedPhysicsFieldKind::F32),
    field("StartBoosterTimeSpeed", 134, EncodedPhysicsFieldKind::F32),
    field(
        "StartForwardAccelForceItem",
        138,
        EncodedPhysicsFieldKind::F32,
    ),
    field(
        "StartForwardAccelForceSpeed",
        142,
        EncodedPhysicsFieldKind::F32,
    ),
    field(
        "DriftGaguePreservePercent",
        146,
        EncodedPhysicsFieldKind::F32,
    ),
    field("UseExtendedAfterBooster", 150, EncodedPhysicsFieldKind::U8),
    field(
        "BoostAccelFactorOnlyItem",
        151,
        EncodedPhysicsFieldKind::F32,
    ),
    field("antiCollideBalance", 155, EncodedPhysicsFieldKind::F32),
    field("dualBoosterSetAuto", 159, EncodedPhysicsFieldKind::U8),
    field("dualBoosterTickMin", 160, EncodedPhysicsFieldKind::I32),
    field("dualBoosterTickMax", 164, EncodedPhysicsFieldKind::I32),
    field("dualMulAccelFactor", 168, EncodedPhysicsFieldKind::F32),
    field("dualTransLowSpeed", 172, EncodedPhysicsFieldKind::F32),
    field("PartsEngineLock", 176, EncodedPhysicsFieldKind::U8),
    field("PartsWheelLock", 177, EncodedPhysicsFieldKind::U8),
    field("PartsSteeringLock", 178, EncodedPhysicsFieldKind::U8),
    field("PartsBoosterLock", 179, EncodedPhysicsFieldKind::U8),
    field("PartsCoatingLock", 180, EncodedPhysicsFieldKind::U8),
    field("PartsTailLampLock", 181, EncodedPhysicsFieldKind::U8),
    field(
        "chargeInstAccelGaugeByBoost",
        182,
        EncodedPhysicsFieldKind::F32,
    ),
    field(
        "chargeInstAccelGaugeByGrip",
        186,
        EncodedPhysicsFieldKind::F32,
    ),
    field(
        "chargeInstAccelGaugeByWall",
        190,
        EncodedPhysicsFieldKind::F32,
    ),
    field("instAccelFactor", 194, EncodedPhysicsFieldKind::F32),
    field(
        "instAccelGaugeCooldownTime",
        198,
        EncodedPhysicsFieldKind::I32,
    ),
    field("instAccelGaugeLength", 202, EncodedPhysicsFieldKind::F32),
    field("instAccelGaugeMinUsable", 206, EncodedPhysicsFieldKind::F32),
    field(
        "instAccelGaugeMinVelBound",
        210,
        EncodedPhysicsFieldKind::F32,
    ),
    field(
        "instAccelGaugeMinVelLoss",
        214,
        EncodedPhysicsFieldKind::F32,
    ),
    field(
        "useExtendedAfterBoosterMore",
        218,
        EncodedPhysicsFieldKind::U8,
    ),
    field(
        "wallCollGaugeCooldownTime",
        219,
        EncodedPhysicsFieldKind::I32,
    ),
    field("wallCollGaugeMaxVelLoss", 223, EncodedPhysicsFieldKind::F32),
    field(
        "wallCollGaugeMinVelBound",
        227,
        EncodedPhysicsFieldKind::F32,
    ),
    field("wallCollGaugeMinVelLoss", 231, EncodedPhysicsFieldKind::F32),
];

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct P5136SpeedSpecSnapshot {
    pub mass: f32,
    pub air_friction: f32,
    pub drag_factor: f32,
    pub forward_accel_force: f32,
    pub backward_accel_force: f32,
    pub grip_brake_force: f32,
    pub slip_brake_force: f32,
    pub max_steer_angle: f32,
    pub steer_constraint: f32,
    pub add_spec_steer_constraint: f32,
    pub front_grip_factor: f32,
    pub rear_grip_factor: f32,
    pub drift_trigger_factor: f32,
    pub drift_trigger_time: f32,
    pub drift_slip_factor: f32,
    pub drift_escape_force: f32,
    pub add_spec_drift_escape_force: f32,
    pub corner_draw_factor: f32,
    pub steer_lean_factor: f32,
    pub drift_max_gauge: f32,
    pub normal_booster_time: f32,
    pub team_booster_time: f32,
    pub trans_accel_factor: f32,
    pub add_spec_trans_accel_factor: f32,
    pub boost_accel_factor: f32,
}

impl P5136SpeedSpecSnapshot {
    /// The `SpeedType.Default()` values used by the Korean P5136 server.
    ///
    /// Room speed type 7 falls through to this preset. Keeping this explicit
    /// avoids confusing it with [`Default::default`], whose all-zero value is
    /// useful while assembling a snapshot from external catalog data.
    #[must_use]
    pub const fn csharp_default() -> Self {
        Self {
            mass: 100.0,
            air_friction: 3.0,
            drag_factor: 0.75,
            forward_accel_force: 2_150.0,
            backward_accel_force: 1_725.0,
            grip_brake_force: 2_070.0,
            slip_brake_force: 1_415.0,
            max_steer_angle: 10.0,
            steer_constraint: 22.25,
            add_spec_steer_constraint: 1.95,
            front_grip_factor: 5.0,
            rear_grip_factor: 5.0,
            drift_trigger_factor: 0.2,
            drift_trigger_time: 0.2,
            drift_slip_factor: 0.2,
            drift_escape_force: 2_600.0,
            add_spec_drift_escape_force: 400.0,
            corner_draw_factor: 0.18,
            steer_lean_factor: 0.0,
            drift_max_gauge: 4_300.0,
            normal_booster_time: 0.0,
            team_booster_time: 0.0,
            trans_accel_factor: -0.0045,
            add_spec_trans_accel_factor: 0.2005,
            boost_accel_factor: -0.006,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct P5136KartSpecSnapshot {
    pub draft_mul_accel_factor: f32,
    pub draft_tick: i32,
    pub drift_boost_mul_accel_factor: f32,
    pub drift_boost_tick: i32,
    pub charge_boost_by_speed: f32,
    pub speed_slot_capacity: u8,
    pub item_slot_capacity: u8,
    pub special_slot_capacity: u8,
    pub use_transform_booster: u8,
    pub motorcycle_type: u8,
    pub bike_rear_wheel: u8,
    pub mass: f32,
    pub air_friction: f32,
    pub drag_factor: f32,
    pub forward_accel_force: f32,
    pub backward_accel_force: f32,
    pub grip_brake_force: f32,
    pub slip_brake_force: f32,
    pub max_steer_angle: f32,
    pub steer_constraint: f32,
    pub front_grip_factor: f32,
    pub rear_grip_factor: f32,
    pub drift_trigger_factor: f32,
    pub drift_trigger_time: f32,
    pub drift_slip_factor: f32,
    pub drift_escape_force: f32,
    pub corner_draw_factor: f32,
    pub drift_lean_factor: f32,
    pub steer_lean_factor: f32,
    pub drift_max_gauge: f32,
    pub normal_booster_time: f32,
    pub item_booster_time: f32,
    pub team_booster_time: f32,
    pub animal_booster_time: f32,
    pub super_booster_time: f32,
    pub trans_accel_factor: f32,
    pub boost_accel_factor: f32,
    pub start_booster_time_item: f32,
    pub start_booster_time_speed: f32,
    pub start_forward_accel_factor_item: f32,
    pub start_forward_accel_factor_speed: f32,
    pub drift_gauge_preserve_percent: f32,
    pub use_extended_after_booster: u8,
    pub boost_accel_factor_only_item: f32,
    pub anti_collide_balance: f32,
    pub dual_booster_set_auto: u8,
    pub dual_booster_tick_min: i32,
    pub dual_booster_tick_max: i32,
    pub dual_mul_accel_factor: f32,
    pub dual_trans_low_speed: f32,
    pub parts_engine_lock: u8,
    pub parts_wheel_lock: u8,
    pub parts_steering_lock: u8,
    pub parts_booster_lock: u8,
    pub parts_coating_lock: u8,
    pub parts_tail_lamp_lock: u8,
    pub charge_inst_accel_gauge_by_boost: f32,
    pub charge_inst_accel_gauge_by_grip: f32,
    pub charge_inst_accel_gauge_by_wall: f32,
    pub inst_accel_factor: f32,
    pub inst_accel_gauge_cooldown_time: i32,
    pub inst_accel_gauge_length: f32,
    pub inst_accel_gauge_min_usable: f32,
    pub inst_accel_gauge_min_vel_bound: f32,
    pub inst_accel_gauge_min_vel_loss: f32,
    pub use_extended_after_booster_more: u8,
    pub wall_coll_gauge_cooldown_time: i32,
    pub wall_coll_gauge_max_vel_loss: f32,
    pub wall_coll_gauge_min_vel_bound: f32,
    pub wall_coll_gauge_min_vel_loss: f32,
}

impl P5136KartSpecSnapshot {
    /// The property-initializer values of a newly constructed C#
    /// `KartSpec`, before XML overrides are applied.
    #[must_use]
    pub const fn csharp_default() -> Self {
        Self {
            draft_mul_accel_factor: 1.1,
            draft_tick: 2_000,
            drift_boost_mul_accel_factor: 1.4,
            drift_boost_tick: 500,
            charge_boost_by_speed: 350.0,
            speed_slot_capacity: 2,
            item_slot_capacity: 2,
            special_slot_capacity: 1,
            use_transform_booster: 1,
            motorcycle_type: 0,
            bike_rear_wheel: 1,
            mass: 0.0,
            air_friction: 0.0,
            drag_factor: -0.083,
            forward_accel_force: 154.0,
            backward_accel_force: 100.0,
            grip_brake_force: 0.0,
            slip_brake_force: 0.0,
            max_steer_angle: 0.0,
            steer_constraint: 2.36,
            front_grip_factor: 0.0,
            rear_grip_factor: 0.0,
            drift_trigger_factor: 0.0,
            drift_trigger_time: 0.0,
            drift_slip_factor: 0.0,
            drift_escape_force: 1_600.0,
            corner_draw_factor: 0.074,
            drift_lean_factor: 0.06,
            steer_lean_factor: 0.01,
            drift_max_gauge: -440.0,
            normal_booster_time: 2_900.0,
            item_booster_time: 3_000.0,
            team_booster_time: 4_350.0,
            animal_booster_time: 4_000.0,
            super_booster_time: 3_500.0,
            trans_accel_factor: 1.854,
            boost_accel_factor: 1.5,
            start_booster_time_item: 1_000.0,
            start_booster_time_speed: 1_500.0,
            start_forward_accel_factor_item: 0.0,
            start_forward_accel_factor_speed: 1.7,
            drift_gauge_preserve_percent: 0.5,
            use_extended_after_booster: 0,
            boost_accel_factor_only_item: 1.5,
            anti_collide_balance: 0.91,
            dual_booster_set_auto: 0,
            dual_booster_tick_min: 20,
            dual_booster_tick_max: 30,
            dual_mul_accel_factor: 1.04,
            dual_trans_low_speed: 100.0,
            parts_engine_lock: 1,
            parts_wheel_lock: 1,
            parts_steering_lock: 1,
            parts_booster_lock: 1,
            parts_coating_lock: 1,
            parts_tail_lamp_lock: 1,
            charge_inst_accel_gauge_by_boost: 0.02,
            charge_inst_accel_gauge_by_grip: 0.06,
            charge_inst_accel_gauge_by_wall: 0.15,
            inst_accel_factor: 1.11,
            inst_accel_gauge_cooldown_time: 3_000,
            inst_accel_gauge_length: 2_500.0,
            inst_accel_gauge_min_usable: 750.0,
            inst_accel_gauge_min_vel_bound: 0.0,
            inst_accel_gauge_min_vel_loss: 50.0,
            use_extended_after_booster_more: 0,
            wall_coll_gauge_cooldown_time: 3_000,
            wall_coll_gauge_max_vel_loss: 200.0,
            wall_coll_gauge_min_vel_bound: 200.0,
            wall_coll_gauge_min_vel_loss: 50.0,
        }
    }
}

impl Default for P5136KartSpecSnapshot {
    fn default() -> Self {
        Self::csharp_default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct P5136FlyingPetSpecSnapshot {
    pub drift_escape_force: f32,
    pub normal_booster_time: f32,
    pub forward_accel_force: f32,
    pub drag_factor: f32,
    pub corner_draw_factor: f32,
    pub item_booster_time: f32,
    pub team_booster_time: f32,
    pub start_forward_accel_force_item: f32,
    pub start_forward_accel_force_speed: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct P5136TuneSpecSnapshot {
    pub drift_escape_force: f32,
    pub normal_booster_time: f32,
    pub trans_accel_factor: f32,
    pub forward_accel: f32,
    pub drag_factor: f32,
    pub corner_draw_factor: f32,
    pub drift_max_gauge: f32,
    pub team_booster_time: f32,
    pub start_booster_time_speed: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct P5136Plant43SpecSnapshot {
    pub trans_accel_factor: f32,
    pub forward_accel: f32,
    pub drag_factor: f32,
    pub start_booster_time_speed: f32,
    pub start_forward_accel_item: f32,
    pub start_forward_accel_speed: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct P5136Plant44SpecSnapshot {
    pub grip_brake: f32,
    pub slip_brake: f32,
    pub steer_constraint: f32,
    pub front_grip_factor: f32,
    pub rear_grip_factor: f32,
    pub corner_draw_factor: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct P5136Plant45SpecSnapshot {
    pub drift_escape_force: f32,
    pub drag_factor: f32,
    pub slip_brake: f32,
    pub corner_draw_factor: f32,
    pub drift_max_gauge: f32,
    pub animal_booster_time: f32,
    pub anti_collide_balance: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct P5136Plant46SpecSnapshot {
    /// Zero means "use the kart's base capacity", matching the C# sentinel.
    pub speed_slot_capacity: u8,
    /// Zero means "use the kart's base capacity", matching the C# sentinel.
    pub item_slot_capacity: u8,
    pub normal_booster_time: f32,
    pub forward_accel: f32,
    pub grip_brake: f32,
    pub slip_brake: f32,
    pub drift_slip_factor: f32,
    pub drift_max_gauge: f32,
    pub team_booster_time: f32,
    pub animal_booster_time: f32,
    pub start_booster_time_item: f32,
    pub start_booster_time_speed: f32,
    pub start_forward_accel_item: f32,
    pub start_forward_accel_speed: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct P5136KartLevelSpecSnapshot {
    pub drift_escape_force: f32,
    pub trans_accel_factor: f32,
    pub forward_accel: f32,
    pub drag_factor: f32,
    pub steer_constraint: f32,
    pub corner_draw_factor: f32,
    pub start_booster_time_item: f32,
    pub start_booster_time_speed: f32,
    pub boost_accel_factor_only_item: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct P5136PartOverrideSnapshot {
    /// Zero means "use speed + kart", matching the C# sentinel.
    pub steer_constraint: f32,
    /// Zero means "use speed + kart", matching the C# sentinel.
    pub drift_escape_force: f32,
    /// Zero means "use the kart's normal booster time", matching C#.
    pub normal_booster_time: f32,
    /// Zero means "use speed + kart", matching the C# sentinel.
    pub trans_accel_factor: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct P5136ExcSpecSnapshot {
    pub tune: P5136TuneSpecSnapshot,
    pub plant43: P5136Plant43SpecSnapshot,
    pub plant44: P5136Plant44SpecSnapshot,
    pub plant45: P5136Plant45SpecSnapshot,
    pub plant46: P5136Plant46SpecSnapshot,
    pub kart_level: P5136KartLevelSpecSnapshot,
    pub parts: P5136PartOverrideSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct P5136SpeedPatchSnapshot {
    pub drift_escape_force: f32,
    pub trans_accel_factor: f32,
    pub forward_accel_force: f32,
    pub drag_factor: f32,
    pub corner_draw_factor: f32,
    pub drift_max_gauge: f32,
    pub boost_accel_factor: f32,
    pub start_forward_accel_force_item: f32,
    pub start_forward_accel_force_speed: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct P5136V2SpecSnapshot {
    pub parts_drift_escape_force: f32,
    pub level_drift_escape_force: f32,
    pub parts_normal_booster_time: f32,
    pub level_normal_booster_time: f32,
    pub parts_trans_accel_factor: f32,
    pub level_trans_accel_factor: f32,
    pub level_forward_accel_force: f32,
    pub parts_steer_constraint: f32,
    pub level_corner_draw_factor: f32,
    pub level_drift_max_gauge: f32,
    pub level_team_booster_time: f32,
    pub level_start_booster_time_speed: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct P5136KartPhysicsSnapshot {
    /// The post-override speed byte returned by C# `GetSpeedType`.
    pub speed_type: u8,
    pub speed: P5136SpeedSpecSnapshot,
    /// Must already include direct kart mutations made by `V2Specs.ExceedSpec`.
    pub kart: P5136KartSpecSnapshot,
    pub flying_pet: P5136FlyingPetSpecSnapshot,
    pub exc: P5136ExcSpecSnapshot,
    pub speed_patch: P5136SpeedPatchSnapshot,
    pub v2: P5136V2SpecSnapshot,
}

impl P5136KartPhysicsSnapshot {
    /// A byte-exact input snapshot for the reference server's safe fallback:
    /// S7/`SpeedType.Default()`, kart ID 0, and no optional equipment sidecars.
    ///
    /// This is protocol-valid and exact for the unequipped fallback. It must
    /// not be presented as the exact physics of an arbitrary equipped kart.
    #[must_use]
    pub fn csharp_s7_baseline() -> Self {
        Self {
            speed_type: 7,
            speed: P5136SpeedSpecSnapshot::csharp_default(),
            kart: P5136KartSpecSnapshot::csharp_default(),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum KartPhysicsBuildError {
    NonFiniteFloat {
        field: &'static str,
        value: f32,
    },
    DecimalConversionOverflow {
        field: &'static str,
        value: f32,
    },
    DecimalArithmeticOverflow {
        operation: &'static str,
    },
    LayoutInvariant {
        field_index: usize,
        offset: usize,
        expected_kind: Option<EncodedPhysicsFieldKind>,
        actual_kind: EncodedPhysicsFieldKind,
    },
    WireLengthInvariant {
        actual: usize,
        expected: usize,
    },
}

impl fmt::Display for KartPhysicsBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFiniteFloat { field, value } => {
                write!(
                    formatter,
                    "kart physics field {field} is not finite: {value}"
                )
            }
            Self::DecimalConversionOverflow { field, value } => write!(
                formatter,
                "kart physics field {field} cannot be represented by System.Decimal: {value}"
            ),
            Self::DecimalArithmeticOverflow { operation } => write!(
                formatter,
                "kart physics System.Decimal arithmetic overflow during {operation}"
            ),
            Self::LayoutInvariant {
                field_index,
                offset,
                expected_kind,
                actual_kind,
            } => write!(
                formatter,
                "kart physics layout mismatch at field {field_index}, byte {offset}: \
                 expected {expected_kind:?}, received {actual_kind:?}"
            ),
            Self::WireLengthInvariant { actual, expected } => write!(
                formatter,
                "kart physics block has {actual} bytes after serialization; expected {expected}"
            ),
        }
    }
}

impl Error for KartPhysicsBuildError {}

/// Builds the exact 235-byte pre-post-P5136 physics block used by
/// `GrCommandStartPacket`.
///
/// Keeping the writes together makes their order directly auditable against
/// the original 70-field C# packet writer.
///
/// # Errors
///
/// Returns [`KartPhysicsBuildError`] when a floating-point source is not
/// finite, the C# decimal helper cannot represent an operand, or the fixed
/// P5136 field layout cannot be produced.
#[allow(clippy::too_many_lines)]
pub fn build_p5136_kart_physics_block(
    snapshot: &P5136KartPhysicsSnapshot,
) -> Result<P5136KartPhysicsBlock, KartPhysicsBuildError> {
    validate_snapshot(snapshot)?;

    let speed = &snapshot.speed;
    let kart = &snapshot.kart;
    let pet = &snapshot.flying_pet;
    let exc = &snapshot.exc;
    let tune = &exc.tune;
    let plant43 = &exc.plant43;
    let plant44 = &exc.plant44;
    let plant45 = &exc.plant45;
    let plant46 = &exc.plant46;
    let kart_level = &exc.kart_level;
    let parts = &exc.parts;
    let patch = &snapshot.speed_patch;
    let v2 = &snapshot.v2;

    // Keep the C# left-to-right addition order. Reassociation can change the
    // final f32 bits and therefore the encoded wire.
    let drift_escape_addition = pet.drift_escape_force
        + tune.drift_escape_force
        + plant45.drift_escape_force
        + kart_level.drift_escape_force
        + patch.drift_escape_force
        + v2.parts_drift_escape_force
        + v2.level_drift_escape_force;
    let normal_booster_addition = pet.normal_booster_time
        + tune.normal_booster_time
        + plant46.normal_booster_time
        + v2.parts_normal_booster_time
        + v2.level_normal_booster_time;
    let trans_accel_addition = tune.trans_accel_factor
        + plant43.trans_accel_factor
        + kart_level.trans_accel_factor
        + patch.trans_accel_factor
        + v2.parts_trans_accel_factor
        + v2.level_trans_accel_factor;
    let forward_accel_force = speed.forward_accel_force
        + kart.forward_accel_force
        + pet.forward_accel_force
        + tune.forward_accel
        + plant43.forward_accel
        + plant46.forward_accel
        + kart_level.forward_accel
        + patch.forward_accel_force
        + v2.level_forward_accel_force;
    let start_forward_accel_force_item = calculate_p5136_start_forward_accel_force(
        kart.start_forward_accel_factor_item,
        forward_accel_force,
    )?;
    let start_forward_accel_force_speed = calculate_p5136_start_forward_accel_force(
        kart.start_forward_accel_factor_speed,
        forward_accel_force,
    )?;

    let speed_slot_capacity = if plant46.speed_slot_capacity == 0 {
        kart.speed_slot_capacity
    } else {
        plant46.speed_slot_capacity
    };
    let item_slot_capacity = if plant46.item_slot_capacity == 0 {
        kart.item_slot_capacity
    } else {
        plant46.item_slot_capacity
    };
    let steer_constraint = if parts.steer_constraint == 0.0 {
        speed.steer_constraint
            + kart.steer_constraint
            + plant44.steer_constraint
            + kart_level.steer_constraint
            + v2.parts_steer_constraint
    } else {
        parts.steer_constraint
            + speed.add_spec_steer_constraint
            + plant44.steer_constraint
            + kart_level.steer_constraint
            + v2.parts_steer_constraint
    };
    let drift_escape_force = if parts.drift_escape_force == 0.0 {
        speed.drift_escape_force + kart.drift_escape_force + drift_escape_addition
    } else {
        parts.drift_escape_force + speed.add_spec_drift_escape_force + drift_escape_addition
    };
    let drift_max_gauge =
        if speed.drift_max_gauge == P5136_S4_DRIFT_MAX_GAUGE || snapshot.speed_type == 4 {
            P5136_S4_DRIFT_MAX_GAUGE
        } else {
            speed.drift_max_gauge
                + kart.drift_max_gauge
                + tune.drift_max_gauge
                + plant45.drift_max_gauge
                + plant46.drift_max_gauge
                + patch.drift_max_gauge
                + v2.level_drift_max_gauge
        };
    let normal_booster_time =
        if speed.normal_booster_time == P5136_S6_BOOSTER_TIME || snapshot.speed_type == 6 {
            P5136_S6_BOOSTER_TIME
        } else if parts.normal_booster_time == 0.0 {
            kart.normal_booster_time + normal_booster_addition
        } else {
            parts.normal_booster_time + normal_booster_addition
        };
    let team_booster_time =
        if speed.team_booster_time == P5136_S6_BOOSTER_TIME || snapshot.speed_type == 6 {
            P5136_S6_BOOSTER_TIME
        } else {
            kart.team_booster_time
                + pet.team_booster_time
                + tune.team_booster_time
                + plant46.team_booster_time
                + v2.level_team_booster_time
        };
    let trans_accel_factor = if parts.trans_accel_factor == 0.0 {
        speed.trans_accel_factor + kart.trans_accel_factor + trans_accel_addition
    } else {
        parts.trans_accel_factor + speed.add_spec_trans_accel_factor + trans_accel_addition
    };

    let mut writer = PhysicsBlockWriter::new();
    writer.f32(kart.draft_mul_accel_factor)?;
    writer.i32(kart.draft_tick)?;
    writer.f32(kart.drift_boost_mul_accel_factor)?;
    writer.i32(kart.drift_boost_tick)?;
    writer.f32(kart.charge_boost_by_speed)?;
    writer.u8(speed_slot_capacity)?;
    writer.u8(item_slot_capacity)?;
    writer.u8(kart.special_slot_capacity)?;
    writer.u8(kart.use_transform_booster)?;
    writer.u8(kart.motorcycle_type)?;
    writer.u8(kart.bike_rear_wheel)?;
    writer.f32(speed.mass + kart.mass)?;
    writer.f32(speed.air_friction + kart.air_friction)?;
    writer.f32(
        speed.drag_factor
            + kart.drag_factor
            + pet.drag_factor
            + patch.drag_factor
            + tune.drag_factor
            + plant43.drag_factor
            + plant45.drag_factor
            + kart_level.drag_factor,
    )?;
    writer.f32(forward_accel_force)?;
    writer.f32(speed.backward_accel_force + kart.backward_accel_force)?;
    writer.f32(
        speed.grip_brake_force + kart.grip_brake_force + plant44.grip_brake + plant46.grip_brake,
    )?;
    writer.f32(
        speed.slip_brake_force
            + kart.slip_brake_force
            + plant44.slip_brake
            + plant45.slip_brake
            + plant46.slip_brake,
    )?;
    writer.f32(speed.max_steer_angle + kart.max_steer_angle)?;
    writer.f32(steer_constraint)?;
    writer.f32(speed.front_grip_factor + kart.front_grip_factor + plant44.front_grip_factor)?;
    writer.f32(speed.rear_grip_factor + kart.rear_grip_factor + plant44.rear_grip_factor)?;
    writer.f32(speed.drift_trigger_factor + kart.drift_trigger_factor)?;
    writer.f32(speed.drift_trigger_time + kart.drift_trigger_time)?;
    writer.f32(speed.drift_slip_factor + kart.drift_slip_factor + plant46.drift_slip_factor)?;
    writer.f32(drift_escape_force)?;
    writer.f32(
        speed.corner_draw_factor
            + kart.corner_draw_factor
            + pet.corner_draw_factor
            + tune.corner_draw_factor
            + plant44.corner_draw_factor
            + plant45.corner_draw_factor
            + kart_level.corner_draw_factor
            + patch.corner_draw_factor
            + v2.level_corner_draw_factor,
    )?;
    writer.f32(kart.drift_lean_factor)?;
    writer.f32(speed.steer_lean_factor + kart.steer_lean_factor)?;
    writer.f32(drift_max_gauge)?;
    writer.f32(normal_booster_time)?;
    writer.f32(kart.item_booster_time + pet.item_booster_time)?;
    writer.f32(team_booster_time)?;
    writer.f32(
        kart.animal_booster_time + plant45.animal_booster_time + plant46.animal_booster_time,
    )?;
    writer.f32(kart.super_booster_time)?;
    writer.f32(trans_accel_factor)?;
    writer.f32(speed.boost_accel_factor + kart.boost_accel_factor + patch.boost_accel_factor)?;
    writer.f32(
        kart.start_booster_time_item
            + kart_level.start_booster_time_item
            + plant46.start_booster_time_item,
    )?;
    writer.f32(
        kart.start_booster_time_speed
            + tune.start_booster_time_speed
            + plant43.start_booster_time_speed
            + plant46.start_booster_time_speed
            + kart_level.start_booster_time_speed
            + v2.level_start_booster_time_speed,
    )?;
    writer.f32(
        start_forward_accel_force_item
            + pet.start_forward_accel_force_item
            + patch.start_forward_accel_force_item
            + plant43.start_forward_accel_item
            + plant46.start_forward_accel_item,
    )?;
    writer.f32(
        start_forward_accel_force_speed
            + pet.start_forward_accel_force_speed
            + patch.start_forward_accel_force_speed
            + plant43.start_forward_accel_speed
            + plant46.start_forward_accel_speed,
    )?;
    writer.f32(kart.drift_gauge_preserve_percent)?;
    writer.u8(kart.use_extended_after_booster)?;
    writer.f32(kart.boost_accel_factor_only_item + kart_level.boost_accel_factor_only_item)?;
    writer.f32(kart.anti_collide_balance + plant45.anti_collide_balance)?;
    writer.u8(kart.dual_booster_set_auto)?;
    writer.i32(kart.dual_booster_tick_min)?;
    writer.i32(kart.dual_booster_tick_max)?;
    writer.f32(kart.dual_mul_accel_factor)?;
    writer.f32(kart.dual_trans_low_speed)?;
    writer.u8(kart.parts_engine_lock)?;
    writer.u8(kart.parts_wheel_lock)?;
    writer.u8(kart.parts_steering_lock)?;
    writer.u8(kart.parts_booster_lock)?;
    writer.u8(kart.parts_coating_lock)?;
    writer.u8(kart.parts_tail_lamp_lock)?;
    writer.f32(kart.charge_inst_accel_gauge_by_boost)?;
    writer.f32(kart.charge_inst_accel_gauge_by_grip)?;
    writer.f32(kart.charge_inst_accel_gauge_by_wall)?;
    writer.f32(kart.inst_accel_factor)?;
    writer.i32(kart.inst_accel_gauge_cooldown_time)?;
    writer.f32(kart.inst_accel_gauge_length)?;
    writer.f32(kart.inst_accel_gauge_min_usable)?;
    writer.f32(kart.inst_accel_gauge_min_vel_bound)?;
    writer.f32(kart.inst_accel_gauge_min_vel_loss)?;
    writer.u8(kart.use_extended_after_booster_more)?;
    writer.i32(kart.wall_coll_gauge_cooldown_time)?;
    writer.f32(kart.wall_coll_gauge_max_vel_loss)?;
    writer.f32(kart.wall_coll_gauge_min_vel_bound)?;
    writer.f32(kart.wall_coll_gauge_min_vel_loss)?;
    writer.finish()
}

/// Reproduces the decimal-shaped C# `StartForwardAccelForce` helper.
///
/// The C# method's parameter names are reversed relative to its call sites:
/// the first value is the kart factor and the second is the already combined
/// forward acceleration force.
///
/// # Errors
///
/// Returns [`KartPhysicsBuildError`] for invalid operands, decimal conversion
/// overflow, or an invalid calculated result.
#[allow(clippy::cast_possible_truncation, clippy::float_cmp)]
pub fn calculate_p5136_start_forward_accel_force(
    factor: f32,
    forward_accel_force: f32,
) -> Result<f32, KartPhysicsBuildError> {
    validate_float("kart.start_forward_accel_factor", factor)?;
    validate_float("effective.forward_accel_force", forward_accel_force)?;

    if factor == 0.0 {
        return Ok(forward_accel_force);
    }

    let decimal_factor = csharp_decimal_operand("kart.start_forward_accel_factor", factor)?;
    let decimal_force =
        csharp_decimal_operand("effective.forward_accel_force", forward_accel_force)?;
    let force_term = decimal_force.checked_mul(decimal_factor).ok_or(
        KartPhysicsBuildError::DecimalArithmeticOverflow {
            operation: "forward acceleration multiplication",
        },
    )?;
    let offset = if let Some(offset) = csharp_start_acceleration_offset(decimal_factor) {
        offset
    } else {
        CSharpDecimal::new(588, 1, true)
            .checked_mul(decimal_factor)
            .and_then(|value| value.checked_mul(decimal_factor))
            .ok_or(KartPhysicsBuildError::DecimalArithmeticOverflow {
                operation: "forward acceleration offset multiplication",
            })?
    };
    let result = force_term
        .checked_add(offset)
        .ok_or(KartPhysicsBuildError::DecimalArithmeticOverflow {
            operation: "forward acceleration addition",
        })?
        .to_f32();
    validate_float("effective.start_forward_accel_force", result)?;
    Ok(result)
}

const CSHARP_DECIMAL_MAX_MANTISSA: u128 = (1_u128 << 96) - 1;
const CSHARP_DECIMAL_POWERS_10: [f64; 29] = [
    1.0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
    1e17, 1e18, 1e19, 1e20, 1e21, 1e22, 1e23, 1e24, 1e25, 1e26, 1e27, 1e28,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CSharpDecimal {
    mantissa: u128,
    scale: u32,
    negative: bool,
}

impl CSharpDecimal {
    const ZERO: Self = Self {
        mantissa: 0,
        scale: 0,
        negative: false,
    };

    const fn new(mantissa: u128, scale: u32, negative: bool) -> Self {
        Self {
            mantissa,
            scale,
            negative,
        }
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn from_f32(value: f32) -> Option<Self> {
        let exponent = i32::from(((value.to_bits() >> 23) & 0xff) as u8) - 126;
        if exponent < -94 {
            return Some(Self::ZERO);
        }
        if exponent > 96 {
            return None;
        }

        let negative = value < 0.0;
        let mut scaled = f64::from(if negative { -value } else { value });
        let mut power = 6 - ((exponent * 19_728) >> 16);
        if power >= 0 {
            power = power.min(28);
            scaled *= CSHARP_DECIMAL_POWERS_10[power as usize];
        } else if power != -1 || scaled >= 1e7 {
            scaled /= CSHARP_DECIMAL_POWERS_10[(-power) as usize];
        } else {
            power = 0;
        }
        if scaled < 1e6 && power < 28 {
            scaled *= 10.0;
            power += 1;
        }

        let mut mantissa = scaled.round_ties_even() as u32;
        if mantissa == 0 {
            return Some(Self::ZERO);
        }

        if power < 0 {
            let multiplier = 10_u128.pow((-power) as u32);
            let mantissa = u128::from(mantissa).checked_mul(multiplier)?;
            if mantissa > CSHARP_DECIMAL_MAX_MANTISSA {
                return None;
            }
            return Some(Self {
                mantissa,
                scale: 0,
                negative,
            });
        }

        let mut removable = power.min(6);
        if removable >= 4 && mantissa.is_multiple_of(10_000) {
            mantissa /= 10_000;
            power -= 4;
            removable -= 4;
        }
        if removable >= 2 && mantissa.is_multiple_of(100) {
            mantissa /= 100;
            power -= 2;
            removable -= 2;
        }
        if removable >= 1 && mantissa.is_multiple_of(10) {
            mantissa /= 10;
            power -= 1;
        }
        Some(Self {
            mantissa: u128::from(mantissa),
            scale: power as u32,
            negative,
        })
    }

    fn checked_mul(self, other: Self) -> Option<Self> {
        let product = U256::multiply_u128(self.mantissa, other.mantissa);
        Self::from_wide(
            product,
            self.scale + other.scale,
            self.negative ^ other.negative,
        )
    }

    fn checked_add(self, other: Self) -> Option<Self> {
        let scale = self.scale.max(other.scale);
        let left = U256::from_u128(self.mantissa).checked_mul_power_of_ten(scale - self.scale)?;
        let right =
            U256::from_u128(other.mantissa).checked_mul_power_of_ten(scale - other.scale)?;
        let (magnitude, negative) = if self.negative == other.negative {
            (left.checked_add(right)?, self.negative)
        } else {
            match left.magnitude_cmp(right) {
                std::cmp::Ordering::Less => (right.checked_sub(left)?, other.negative),
                std::cmp::Ordering::Equal => return Some(Self::ZERO),
                std::cmp::Ordering::Greater => (left.checked_sub(right)?, self.negative),
            }
        };
        Self::from_wide(magnitude, scale, negative)
    }

    fn from_wide(value: U256, scale: u32, negative: bool) -> Option<Self> {
        let minimum_removed = scale.saturating_sub(28);
        for removed in minimum_removed..=scale {
            let rounded = value.round_div_power_of_ten(removed)?;
            if rounded.fits_decimal_mantissa() {
                let mantissa = rounded.to_u128();
                return if mantissa == 0 {
                    Some(Self::ZERO)
                } else {
                    Some(Self::new(mantissa, scale - removed, negative))
                };
            }
        }
        None
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::cast_sign_loss
    )]
    fn to_f32(self) -> f32 {
        if self.mantissa == 0 {
            return if self.negative { -0.0 } else { 0.0 };
        }

        let numerator = U256::from_u128(self.mantissa);
        let denominator = U256::from_u128(10_u128.pow(self.scale));
        let mut exponent = numerator.bit_length() as i32 - denominator.bit_length() as i32;
        let below_power = if exponent >= 0 {
            numerator.magnitude_cmp(
                denominator
                    .checked_shift_left(exponent as u32)
                    .expect("decimal exponent comparison fits U256"),
            ) == std::cmp::Ordering::Less
        } else {
            numerator
                .checked_shift_left((-exponent) as u32)
                .expect("decimal exponent comparison fits U256")
                .magnitude_cmp(denominator)
                == std::cmp::Ordering::Less
        };
        if below_power {
            exponent -= 1;
        }

        let significand_shift = 23 - exponent;
        let (scaled_numerator, scaled_denominator) = if significand_shift >= 0 {
            (
                numerator
                    .checked_shift_left(significand_shift as u32)
                    .expect("decimal-to-f32 numerator fits U256"),
                denominator,
            )
        } else {
            (
                numerator,
                denominator
                    .checked_shift_left((-significand_shift) as u32)
                    .expect("decimal-to-f32 denominator fits U256"),
            )
        };
        let (mut significand, remainder) =
            scaled_numerator.div_rem_u32_quotient(scaled_denominator);
        let doubled_remainder = remainder
            .checked_mul_u64(2)
            .expect("decimal-to-f32 remainder fits U256");
        if doubled_remainder.magnitude_cmp(scaled_denominator) == std::cmp::Ordering::Greater
            || (doubled_remainder == scaled_denominator && !significand.is_multiple_of(2))
        {
            significand += 1;
        }
        if significand == 1 << 24 {
            significand >>= 1;
            exponent += 1;
        }

        debug_assert!((1 << 23..1 << 24).contains(&significand));
        let biased_exponent = (exponent + 127) as u32;
        debug_assert!((1..0xff).contains(&biased_exponent));
        let sign = u32::from(self.negative) << 31;
        f32::from_bits(sign | (biased_exponent << 23) | (significand - (1 << 23)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct U256([u64; 4]);

#[allow(clippy::cast_possible_truncation)]
impl U256 {
    const fn from_u128(value: u128) -> Self {
        Self([value as u64, (value >> 64) as u64, 0, 0])
    }

    fn multiply_u128(left: u128, right: u128) -> Self {
        let left = [left as u64, (left >> 64) as u64];
        let right = [right as u64, (right >> 64) as u64];
        let mut product = [0_u64; 4];

        for (left_index, left_limb) in left.into_iter().enumerate() {
            let mut carry = 0_u128;
            for (right_index, right_limb) in right.into_iter().enumerate() {
                let index = left_index + right_index;
                let value = u128::from(product[index])
                    + u128::from(left_limb) * u128::from(right_limb)
                    + carry;
                product[index] = value as u64;
                carry = value >> 64;
            }

            let mut index = left_index + 2;
            while carry != 0 {
                let value = u128::from(product[index]) + carry;
                product[index] = value as u64;
                carry = value >> 64;
                index += 1;
            }
        }
        Self(product)
    }

    fn checked_add(self, other: Self) -> Option<Self> {
        let mut result = [0_u64; 4];
        let mut carry = false;
        for (index, output) in result.iter_mut().enumerate() {
            let (partial, first_carry) = self.0[index].overflowing_add(other.0[index]);
            let (sum, second_carry) = partial.overflowing_add(u64::from(carry));
            *output = sum;
            carry = first_carry || second_carry;
        }
        (!carry).then_some(Self(result))
    }

    fn checked_sub(self, other: Self) -> Option<Self> {
        if self.magnitude_cmp(other) == std::cmp::Ordering::Less {
            return None;
        }

        let mut result = [0_u64; 4];
        let mut borrow = false;
        for (index, output) in result.iter_mut().enumerate() {
            let (partial, first_borrow) = self.0[index].overflowing_sub(other.0[index]);
            let (difference, second_borrow) = partial.overflowing_sub(u64::from(borrow));
            *output = difference;
            borrow = first_borrow || second_borrow;
        }
        debug_assert!(!borrow);
        Some(Self(result))
    }

    fn checked_mul_u64(self, multiplier: u64) -> Option<Self> {
        let mut result = [0_u64; 4];
        let mut carry = 0_u128;
        for (limb, output) in self.0.into_iter().zip(result.iter_mut()) {
            let value = u128::from(limb) * u128::from(multiplier) + carry;
            *output = value as u64;
            carry = value >> 64;
        }
        (carry == 0).then_some(Self(result))
    }

    fn checked_mul_power_of_ten(mut self, power: u32) -> Option<Self> {
        for _ in 0..power {
            self = self.checked_mul_u64(10)?;
        }
        Some(self)
    }

    fn checked_shift_left(mut self, shift: u32) -> Option<Self> {
        for _ in 0..shift {
            self = self.checked_mul_u64(2)?;
        }
        Some(self)
    }

    fn div_rem_u64(self, divisor: u64) -> (Self, u64) {
        let mut quotient = [0_u64; 4];
        let mut remainder = 0_u128;
        for index in (0..4).rev() {
            let value = (remainder << 64) | u128::from(self.0[index]);
            quotient[index] = (value / u128::from(divisor)) as u64;
            remainder = value % u128::from(divisor);
        }
        (Self(quotient), remainder as u64)
    }

    fn round_div_power_of_ten(self, power: u32) -> Option<Self> {
        if power == 0 {
            return Some(self);
        }

        let mut quotient = self;
        let mut sticky = false;
        let mut round_digit = 0_u64;
        for digit in 0..power {
            let (next, remainder) = quotient.div_rem_u64(10);
            quotient = next;
            if digit + 1 == power {
                round_digit = remainder;
            } else {
                sticky |= remainder != 0;
            }
        }

        if round_digit > 5 || (round_digit == 5 && (sticky || quotient.0[0] & 1 != 0)) {
            quotient = quotient.checked_add(Self::from_u128(1))?;
        }
        Some(quotient)
    }

    fn div_rem_u32_quotient(self, divisor: Self) -> (u32, Self) {
        let mut lower = 0_u32;
        let mut upper = 1_u32 << 25;
        while lower + 1 < upper {
            let middle = lower + (upper - lower) / 2;
            let product = divisor
                .checked_mul_u64(u64::from(middle))
                .expect("bounded decimal-to-f32 quotient fits U256");
            if product.magnitude_cmp(self) == std::cmp::Ordering::Greater {
                upper = middle;
            } else {
                lower = middle;
            }
        }
        let product = divisor
            .checked_mul_u64(u64::from(lower))
            .expect("bounded decimal-to-f32 quotient fits U256");
        (
            lower,
            self.checked_sub(product)
                .expect("the quotient product does not exceed the dividend"),
        )
    }

    fn magnitude_cmp(self, other: Self) -> std::cmp::Ordering {
        for index in (0..4).rev() {
            match self.0[index].cmp(&other.0[index]) {
                std::cmp::Ordering::Equal => {}
                ordering => return ordering,
            }
        }
        std::cmp::Ordering::Equal
    }

    fn bit_length(self) -> u32 {
        for index in (0..4).rev() {
            if self.0[index] != 0 {
                return index as u32 * 64 + (64 - self.0[index].leading_zeros());
            }
        }
        0
    }

    fn fits_decimal_mantissa(self) -> bool {
        self.0[2] == 0 && self.0[3] == 0 && u32::try_from(self.0[1]).is_ok()
    }

    fn to_u128(self) -> u128 {
        u128::from(self.0[0]) | (u128::from(self.0[1]) << 64)
    }
}

fn csharp_decimal_operand(
    field: &'static str,
    value: f32,
) -> Result<CSharpDecimal, KartPhysicsBuildError> {
    CSharpDecimal::from_f32(value)
        .ok_or(KartPhysicsBuildError::DecimalConversionOverflow { field, value })
}

fn csharp_start_acceleration_offset(factor: CSharpDecimal) -> Option<CSharpDecimal> {
    match (factor.negative, factor.mantissa, factor.scale) {
        (false, 165, 2) => Some(CSharpDecimal::new(158_679, 3, true)),
        (false, 17, 1) => Some(CSharpDecimal::new(171_212, 3, true)),
        (false, 18, 1) => Some(CSharpDecimal::new(191_644, 3, true)),
        (false, 185, 2) => Some(CSharpDecimal::new(204_276, 3, true)),
        (false, 19, 1) => Some(CSharpDecimal::new(211_547, 3, true)),
        (false, 21, 1) => Some(CSharpDecimal::new(240_324, 3, true)),
        _ => None,
    }
}

struct PhysicsBlockWriter {
    bytes: Vec<u8>,
    field_index: usize,
}

impl PhysicsBlockWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::with_capacity(P5136_KART_PHYSICS_BLOCK_LENGTH),
            field_index: 0,
        }
    }

    fn f32(&mut self, value: f32) -> Result<(), KartPhysicsBuildError> {
        let field = self.begin_field(EncodedPhysicsFieldKind::F32)?;
        validate_float(field.name, value)?;
        self.bytes.extend_from_slice(&encoded::encode_f32(value));
        Ok(())
    }

    fn i32(&mut self, value: i32) -> Result<(), KartPhysicsBuildError> {
        self.begin_field(EncodedPhysicsFieldKind::I32)?;
        self.bytes.extend_from_slice(&encoded::encode_i32(value));
        Ok(())
    }

    fn u8(&mut self, value: u8) -> Result<(), KartPhysicsBuildError> {
        self.begin_field(EncodedPhysicsFieldKind::U8)?;
        self.bytes.push(encoded::encode_u8(value));
        Ok(())
    }

    fn begin_field(
        &mut self,
        actual_kind: EncodedPhysicsFieldKind,
    ) -> Result<PhysicsFieldLayout, KartPhysicsBuildError> {
        let expected = P5136_KART_PHYSICS_LAYOUT.get(self.field_index).copied();
        if expected
            .is_none_or(|field| field.offset != self.bytes.len() || field.kind != actual_kind)
        {
            return Err(KartPhysicsBuildError::LayoutInvariant {
                field_index: self.field_index,
                offset: self.bytes.len(),
                expected_kind: expected.map(|field| field.kind),
                actual_kind,
            });
        }
        self.field_index += 1;
        Ok(expected.expect("the checked field exists"))
    }

    fn finish(self) -> Result<P5136KartPhysicsBlock, KartPhysicsBuildError> {
        if self.field_index != P5136_KART_PHYSICS_FIELD_COUNT
            || self.bytes.len() != P5136_KART_PHYSICS_BLOCK_LENGTH
        {
            return Err(KartPhysicsBuildError::WireLengthInvariant {
                actual: self.bytes.len(),
                expected: P5136_KART_PHYSICS_BLOCK_LENGTH,
            });
        }
        let bytes: [u8; P5136_KART_PHYSICS_BLOCK_LENGTH] =
            self.bytes.try_into().map_err(|bytes: Vec<u8>| {
                KartPhysicsBuildError::WireLengthInvariant {
                    actual: bytes.len(),
                    expected: P5136_KART_PHYSICS_BLOCK_LENGTH,
                }
            })?;
        Ok(P5136KartPhysicsBlock::from(bytes))
    }
}

// A flat validation list keeps every wire-affecting source field explicit and
// prevents a newly added snapshot field from being silently accepted.
#[allow(clippy::too_many_lines)]
fn validate_snapshot(snapshot: &P5136KartPhysicsSnapshot) -> Result<(), KartPhysicsBuildError> {
    macro_rules! validate_fields {
        ($prefix:literal, $source:expr, [$($field:ident),+ $(,)?]) => {
            $(
                validate_float(
                    concat!($prefix, ".", stringify!($field)),
                    $source.$field,
                )?;
            )+
        };
    }

    let speed = &snapshot.speed;
    validate_fields!(
        "speed",
        speed,
        [
            mass,
            air_friction,
            drag_factor,
            forward_accel_force,
            backward_accel_force,
            grip_brake_force,
            slip_brake_force,
            max_steer_angle,
            steer_constraint,
            add_spec_steer_constraint,
            front_grip_factor,
            rear_grip_factor,
            drift_trigger_factor,
            drift_trigger_time,
            drift_slip_factor,
            drift_escape_force,
            add_spec_drift_escape_force,
            corner_draw_factor,
            steer_lean_factor,
            drift_max_gauge,
            normal_booster_time,
            team_booster_time,
            trans_accel_factor,
            add_spec_trans_accel_factor,
            boost_accel_factor,
        ]
    );

    let kart = &snapshot.kart;
    validate_fields!(
        "kart",
        kart,
        [
            draft_mul_accel_factor,
            drift_boost_mul_accel_factor,
            charge_boost_by_speed,
            mass,
            air_friction,
            drag_factor,
            forward_accel_force,
            backward_accel_force,
            grip_brake_force,
            slip_brake_force,
            max_steer_angle,
            steer_constraint,
            front_grip_factor,
            rear_grip_factor,
            drift_trigger_factor,
            drift_trigger_time,
            drift_slip_factor,
            drift_escape_force,
            corner_draw_factor,
            drift_lean_factor,
            steer_lean_factor,
            drift_max_gauge,
            normal_booster_time,
            item_booster_time,
            team_booster_time,
            animal_booster_time,
            super_booster_time,
            trans_accel_factor,
            boost_accel_factor,
            start_booster_time_item,
            start_booster_time_speed,
            start_forward_accel_factor_item,
            start_forward_accel_factor_speed,
            drift_gauge_preserve_percent,
            boost_accel_factor_only_item,
            anti_collide_balance,
            dual_mul_accel_factor,
            dual_trans_low_speed,
            charge_inst_accel_gauge_by_boost,
            charge_inst_accel_gauge_by_grip,
            charge_inst_accel_gauge_by_wall,
            inst_accel_factor,
            inst_accel_gauge_length,
            inst_accel_gauge_min_usable,
            inst_accel_gauge_min_vel_bound,
            inst_accel_gauge_min_vel_loss,
            wall_coll_gauge_max_vel_loss,
            wall_coll_gauge_min_vel_bound,
            wall_coll_gauge_min_vel_loss,
        ]
    );

    let pet = &snapshot.flying_pet;
    validate_fields!(
        "flying_pet",
        pet,
        [
            drift_escape_force,
            normal_booster_time,
            forward_accel_force,
            drag_factor,
            corner_draw_factor,
            item_booster_time,
            team_booster_time,
            start_forward_accel_force_item,
            start_forward_accel_force_speed,
        ]
    );

    let exc = &snapshot.exc;
    validate_fields!(
        "exc.tune",
        exc.tune,
        [
            drift_escape_force,
            normal_booster_time,
            trans_accel_factor,
            forward_accel,
            drag_factor,
            corner_draw_factor,
            drift_max_gauge,
            team_booster_time,
            start_booster_time_speed,
        ]
    );
    validate_fields!(
        "exc.plant43",
        exc.plant43,
        [
            trans_accel_factor,
            forward_accel,
            drag_factor,
            start_booster_time_speed,
            start_forward_accel_item,
            start_forward_accel_speed,
        ]
    );
    validate_fields!(
        "exc.plant44",
        exc.plant44,
        [
            grip_brake,
            slip_brake,
            steer_constraint,
            front_grip_factor,
            rear_grip_factor,
            corner_draw_factor,
        ]
    );
    validate_fields!(
        "exc.plant45",
        exc.plant45,
        [
            drift_escape_force,
            drag_factor,
            slip_brake,
            corner_draw_factor,
            drift_max_gauge,
            animal_booster_time,
            anti_collide_balance,
        ]
    );
    validate_fields!(
        "exc.plant46",
        exc.plant46,
        [
            normal_booster_time,
            forward_accel,
            grip_brake,
            slip_brake,
            drift_slip_factor,
            drift_max_gauge,
            team_booster_time,
            animal_booster_time,
            start_booster_time_item,
            start_booster_time_speed,
            start_forward_accel_item,
            start_forward_accel_speed,
        ]
    );
    validate_fields!(
        "exc.kart_level",
        exc.kart_level,
        [
            drift_escape_force,
            trans_accel_factor,
            forward_accel,
            drag_factor,
            steer_constraint,
            corner_draw_factor,
            start_booster_time_item,
            start_booster_time_speed,
            boost_accel_factor_only_item,
        ]
    );
    validate_fields!(
        "exc.parts",
        exc.parts,
        [
            steer_constraint,
            drift_escape_force,
            normal_booster_time,
            trans_accel_factor,
        ]
    );

    let patch = &snapshot.speed_patch;
    validate_fields!(
        "speed_patch",
        patch,
        [
            drift_escape_force,
            trans_accel_factor,
            forward_accel_force,
            drag_factor,
            corner_draw_factor,
            drift_max_gauge,
            boost_accel_factor,
            start_forward_accel_force_item,
            start_forward_accel_force_speed,
        ]
    );

    let v2 = &snapshot.v2;
    validate_fields!(
        "v2",
        v2,
        [
            parts_drift_escape_force,
            level_drift_escape_force,
            parts_normal_booster_time,
            level_normal_booster_time,
            parts_trans_accel_factor,
            level_trans_accel_factor,
            level_forward_accel_force,
            parts_steer_constraint,
            level_corner_draw_factor,
            level_drift_max_gauge,
            level_team_booster_time,
            level_start_booster_time_speed,
        ]
    );

    Ok(())
}

fn validate_float(field: &'static str, value: f32) -> Result<(), KartPhysicsBuildError> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(KartPhysicsBuildError::NonFiniteFloat { field, value })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        EncodedPhysicsFieldKind, KartPhysicsBuildError, P5136_ENCODED_F32_COUNT,
        P5136_ENCODED_I32_COUNT, P5136_ENCODED_U8_COUNT, P5136_KART_PHYSICS_FIELD_COUNT,
        P5136_KART_PHYSICS_LAYOUT, P5136_S4_DRIFT_MAX_GAUGE, P5136_S6_BOOSTER_TIME,
        P5136ExcSpecSnapshot, P5136FlyingPetSpecSnapshot, P5136KartLevelSpecSnapshot,
        P5136KartPhysicsSnapshot, P5136KartSpecSnapshot, P5136PartOverrideSnapshot,
        P5136Plant43SpecSnapshot, P5136Plant44SpecSnapshot, P5136Plant45SpecSnapshot,
        P5136Plant46SpecSnapshot, P5136SpeedPatchSnapshot, P5136SpeedSpecSnapshot,
        P5136TuneSpecSnapshot, P5136V2SpecSnapshot, build_p5136_kart_physics_block,
        calculate_p5136_start_forward_accel_force,
    };
    use crate::{
        encoded,
        race_start_protocol::{P5136_KART_PHYSICS_BLOCK_LENGTH, P5136KartPhysicsBlock},
    };

    // Generated independently with the C# `CryptoConstants.encryptBytes` and
    // the fixture below evaluated in the source `GetKartSpac` expression
    // order. Keeping the complete block catches both arithmetic and offsets.
    const CSHARP_DERIVED_GOLDEN: [u8; P5136_KART_PHYSICS_BLOCK_LENGTH] = [
        27, 27, 144, 179, 38, 181, 112, 137, 113, 75, 56, 179, 99, 112, 112, 137, 186, 124, 114,
        105, 137, 33, 124, 124, 186, 124, 186, 124, 139, 77, 162, 252, 63, 241, 4, 206, 45, 174,
        186, 124, 40, 105, 186, 124, 199, 77, 186, 124, 221, 177, 186, 124, 214, 177, 186, 124,
        153, 177, 27, 27, 157, 177, 80, 60, 163, 179, 80, 60, 172, 134, 113, 75, 175, 134, 27, 27,
        144, 134, 113, 75, 56, 134, 186, 82, 176, 215, 195, 168, 236, 134, 30, 250, 93, 201, 10,
        206, 159, 201, 197, 12, 16, 179, 186, 124, 158, 206, 186, 82, 134, 206, 186, 9, 217, 206,
        186, 26, 167, 206, 186, 82, 188, 206, 131, 68, 251, 134, 242, 27, 8, 134, 186, 82, 246,
        215, 186, 9, 243, 215, 167, 163, 125, 105, 139, 56, 33, 215, 12, 12, 231, 9, 124, 27, 27,
        121, 179, 27, 27, 144, 179, 124, 106, 124, 112, 137, 234, 124, 112, 137, 27, 27, 144, 179,
        186, 124, 199, 77, 124, 186, 124, 186, 124, 186, 162, 252, 180, 241, 61, 51, 165, 241, 27,
        27, 53, 9, 186, 124, 143, 179, 207, 33, 112, 137, 186, 124, 209, 215, 186, 124, 209, 105,
        186, 124, 126, 105, 186, 124, 126, 77, 124, 207, 33, 112, 137, 186, 124, 126, 105, 186,
        124, 126, 105, 186, 124, 126, 77,
    ];

    #[test]
    fn field_map_is_contiguous_and_counts_the_exact_p5136_shape() {
        let mut offset = 0;
        let mut f32_count = 0;
        let mut i32_count = 0;
        let mut u8_count = 0;
        for field in P5136_KART_PHYSICS_LAYOUT {
            assert_eq!(field.offset, offset, "offset drift at {}", field.name);
            offset += field.kind.wire_length();
            match field.kind {
                EncodedPhysicsFieldKind::F32 => f32_count += 1,
                EncodedPhysicsFieldKind::I32 => i32_count += 1,
                EncodedPhysicsFieldKind::U8 => u8_count += 1,
            }
        }

        assert_eq!(
            P5136_KART_PHYSICS_LAYOUT.len(),
            P5136_KART_PHYSICS_FIELD_COUNT
        );
        assert_eq!(f32_count, P5136_ENCODED_F32_COUNT);
        assert_eq!(i32_count, P5136_ENCODED_I32_COUNT);
        assert_eq!(u8_count, P5136_ENCODED_U8_COUNT);
        assert_eq!(offset, P5136_KART_PHYSICS_BLOCK_LENGTH);
        assert_eq!(
            P5136_ENCODED_F32_COUNT * 4 + P5136_ENCODED_I32_COUNT * 4 + P5136_ENCODED_U8_COUNT,
            235
        );
    }

    #[test]
    fn s7_baseline_uses_the_reference_default_speed_and_kart_inputs() {
        let baseline = P5136KartPhysicsSnapshot::csharp_s7_baseline();
        assert_eq!(baseline.speed_type, 7);
        assert_eq!(
            baseline.speed,
            P5136SpeedSpecSnapshot {
                mass: 100.0,
                air_friction: 3.0,
                drag_factor: 0.75,
                forward_accel_force: 2_150.0,
                backward_accel_force: 1_725.0,
                grip_brake_force: 2_070.0,
                slip_brake_force: 1_415.0,
                max_steer_angle: 10.0,
                steer_constraint: 22.25,
                add_spec_steer_constraint: 1.95,
                front_grip_factor: 5.0,
                rear_grip_factor: 5.0,
                drift_trigger_factor: 0.2,
                drift_trigger_time: 0.2,
                drift_slip_factor: 0.2,
                drift_escape_force: 2_600.0,
                add_spec_drift_escape_force: 400.0,
                corner_draw_factor: 0.18,
                steer_lean_factor: 0.0,
                drift_max_gauge: 4_300.0,
                normal_booster_time: 0.0,
                team_booster_time: 0.0,
                trans_accel_factor: -0.0045,
                add_spec_trans_accel_factor: 0.2005,
                boost_accel_factor: -0.006,
            }
        );
        assert_eq!(baseline.kart, P5136KartSpecSnapshot::csharp_default());
        assert_eq!(baseline.flying_pet, P5136FlyingPetSpecSnapshot::default());
        assert_eq!(baseline.exc, P5136ExcSpecSnapshot::default());
        assert_eq!(baseline.speed_patch, P5136SpeedPatchSnapshot::default());
        assert_eq!(baseline.v2, P5136V2SpecSnapshot::default());

        let block = build_p5136_kart_physics_block(&baseline).unwrap();
        assert_eq!(block.as_bytes().len(), P5136_KART_PHYSICS_BLOCK_LENGTH);
    }

    #[test]
    fn all_sources_and_part_override_branches_match_the_csharp_derived_golden() {
        let block = build_p5136_kart_physics_block(&csharp_fixture()).unwrap();
        assert_eq!(block.as_bytes(), &CSHARP_DERIVED_GOLDEN);

        assert_f32_field(&block, 14, 352.0);
        assert_f32_field(&block, 19, 16.6);
        assert_f32_field(&block, 25, 531.0);
        assert_f32_field(&block, 30, 3_440.0);
        assert_f32_field(&block, 35, 2.71);
        assert_f32_field(&block, 39, 493.121);
        assert_f32_field(&block, 40, f32::from_bits(0x4402_B4D4));
    }

    #[test]
    fn s4_and_s6_sentinels_override_the_normal_additive_paths() {
        let mut snapshot = csharp_fixture();
        snapshot.speed_type = 4;
        let s4 = build_p5136_kart_physics_block(&snapshot).unwrap();
        assert_f32_field(&s4, 29, P5136_S4_DRIFT_MAX_GAUGE);

        snapshot.speed_type = 6;
        let s6 = build_p5136_kart_physics_block(&snapshot).unwrap();
        assert_f32_field(&s6, 30, P5136_S6_BOOSTER_TIME);
        assert_f32_field(&s6, 32, P5136_S6_BOOSTER_TIME);

        snapshot.speed_type = 7;
        snapshot.speed.drift_max_gauge = P5136_S4_DRIFT_MAX_GAUGE;
        snapshot.speed.normal_booster_time = P5136_S6_BOOSTER_TIME;
        snapshot.speed.team_booster_time = P5136_S6_BOOSTER_TIME;
        let sentinel = build_p5136_kart_physics_block(&snapshot).unwrap();
        assert_f32_field(&sentinel, 29, P5136_S4_DRIFT_MAX_GAUGE);
        assert_f32_field(&sentinel, 30, P5136_S6_BOOSTER_TIME);
        assert_f32_field(&sentinel, 32, P5136_S6_BOOSTER_TIME);
    }

    #[test]
    fn decimal_start_acceleration_helper_matches_csharp_float_bits() {
        assert_eq!(
            calculate_p5136_start_forward_accel_force(1.65, 300.125)
                .unwrap()
                .to_bits(),
            0x43A8_437D
        );
        assert_eq!(
            calculate_p5136_start_forward_accel_force(1.77, 300.125)
                .unwrap()
                .to_bits(),
            0x43AD_80DD
        );
        assert_eq!(
            calculate_p5136_start_forward_accel_force(1.8555, 321.23456)
                .unwrap()
                .to_bits(),
            0x43C4_CE02
        );
    }

    #[test]
    fn decimal_rounded_factor_uses_the_csharp_special_offset_key() {
        assert_ne!(1.650_000_1_f32.to_bits(), 1.65_f32.to_bits());
        assert_eq!(
            calculate_p5136_start_forward_accel_force(1.650_000_1, 300.125)
                .unwrap()
                .to_bits(),
            0x43A8_437D
        );
    }

    #[test]
    fn decimal_arithmetic_matches_csharp_near_positive_cancellation() {
        let factor = f32::from_bits(0x40C8_8DB3);
        let force = f32::from_bits(0x43B8_422F);
        assert_eq!(
            calculate_p5136_start_forward_accel_force(factor, force)
                .unwrap()
                .to_bits(),
            0x396F_3613
        );
    }

    #[test]
    fn decimal_arithmetic_matches_csharp_near_negative_cancellation() {
        let factor = f32::from_bits(0xBF39_5696);
        let force = f32::from_bits(0xC22A_478B);
        assert_eq!(
            calculate_p5136_start_forward_accel_force(factor, force)
                .unwrap()
                .to_bits(),
            0xB559_A983
        );
    }

    #[test]
    fn decimal_conversion_rounds_tiny_float_at_scale_28() {
        // 0x10FD87B5 is the requested 9.99999943e-29 C# `float`.
        let result =
            calculate_p5136_start_forward_accel_force(f32::from_bits(0x10FD_87B5), 1.0).unwrap();
        assert_eq!(result.to_bits(), 1e-28_f32.to_bits());
        assert_ne!(result.to_bits(), 0.0_f32.to_bits());
    }

    #[test]
    fn decimal_conversion_preserves_system_decimal_overflow() {
        assert!(super::CSharpDecimal::from_f32(f32::MAX).is_none());
        assert!(super::CSharpDecimal::from_f32(f32::MIN).is_none());
    }

    #[test]
    fn zero_part_sentinels_and_plant46_capacities_take_the_csharp_fallbacks() {
        let mut snapshot = csharp_fixture();
        snapshot.speed.add_spec_steer_constraint = 99.0;
        snapshot.speed.add_spec_drift_escape_force = 99.0;
        snapshot.speed.add_spec_trans_accel_factor = 99.0;
        snapshot.exc.parts = P5136PartOverrideSnapshot::default();
        snapshot.exc.plant46.speed_slot_capacity = 0;
        snapshot.exc.plant46.item_slot_capacity = 0;

        let block = build_p5136_kart_physics_block(&snapshot).unwrap();
        assert_u8_field(&block, 5, 2);
        assert_u8_field(&block, 6, 2);
        assert_f32_field(&block, 19, f32::from_bits(0x40F3_3333));
        assert_f32_field(&block, 25, 161.0);
        assert_f32_field(&block, 30, 3_240.0);
        assert_f32_field(&block, 35, f32::from_bits(0x402D_70A4));
    }

    #[test]
    fn exact_encoder_preserves_the_full_csharp_i32_and_u8_wire_domains() {
        let mut snapshot = csharp_fixture();
        snapshot.kart.draft_tick = i32::MIN;
        snapshot.kart.drift_boost_tick = -1;
        snapshot.kart.dual_booster_tick_min = i32::MAX;
        snapshot.kart.dual_booster_tick_max = i32::MIN + 1;
        snapshot.kart.inst_accel_gauge_cooldown_time = -5_136;
        snapshot.kart.wall_coll_gauge_cooldown_time = i32::MAX - 1;
        snapshot.exc.plant46.speed_slot_capacity = 9;
        snapshot.exc.plant46.item_slot_capacity = u8::MAX;
        snapshot.kart.special_slot_capacity = 254;
        snapshot.kart.use_transform_booster = 2;
        snapshot.kart.motorcycle_type = 3;
        snapshot.kart.bike_rear_wheel = 4;
        snapshot.kart.use_extended_after_booster = 5;
        snapshot.kart.dual_booster_set_auto = 6;
        snapshot.kart.parts_engine_lock = 7;
        snapshot.kart.parts_wheel_lock = 8;
        snapshot.kart.parts_steering_lock = 9;
        snapshot.kart.parts_booster_lock = 10;
        snapshot.kart.parts_coating_lock = 11;
        snapshot.kart.parts_tail_lamp_lock = 12;
        snapshot.kart.use_extended_after_booster_more = 13;

        let block = build_p5136_kart_physics_block(&snapshot).unwrap();
        for (field_index, expected) in [
            (1, i32::MIN),
            (3, -1),
            (46, i32::MAX),
            (47, i32::MIN + 1),
            (60, -5_136),
            (66, i32::MAX - 1),
        ] {
            assert_i32_field(&block, field_index, expected);
        }
        for (field_index, expected) in [
            (5, 9),
            (6, u8::MAX),
            (7, 254),
            (8, 2),
            (9, 3),
            (10, 4),
            (42, 5),
            (45, 6),
            (50, 7),
            (51, 8),
            (52, 9),
            (53, 10),
            (54, 11),
            (55, 12),
            (65, 13),
        ] {
            assert_u8_field(&block, field_index, expected);
        }
    }

    #[test]
    fn finite_values_outside_the_old_policy_limits_still_match_the_csharp_writer() {
        let mut snapshot = csharp_fixture();
        snapshot.speed.mass = 0.0;
        snapshot.kart.mass = 10_000_001.0;
        snapshot.kart.start_forward_accel_factor_item = 10.01;
        let block = build_p5136_kart_physics_block(&snapshot).unwrap();
        assert_f32_field(&block, 11, 10_000_001.0);
    }

    #[test]
    fn non_finite_sources_are_rejected_even_when_a_speed_sentinel_would_hide_them() {
        let mut snapshot = csharp_fixture();
        snapshot.speed_type = 6;
        snapshot.exc.tune.normal_booster_time = f32::NAN;
        assert!(matches!(
            build_p5136_kart_physics_block(&snapshot),
            Err(KartPhysicsBuildError::NonFiniteFloat {
                field: "exc.tune.normal_booster_time",
                ..
            })
        ));
    }

    fn assert_f32_field(block: &P5136KartPhysicsBlock, field_index: usize, expected: f32) {
        let field = P5136_KART_PHYSICS_LAYOUT[field_index];
        assert_eq!(field.kind, EncodedPhysicsFieldKind::F32);
        assert_eq!(
            &block.as_bytes()[field.offset..field.offset + 4],
            encoded::encode_f32(expected).as_slice(),
            "{} at byte {}",
            field.name,
            field.offset
        );
    }

    fn assert_u8_field(block: &P5136KartPhysicsBlock, field_index: usize, expected: u8) {
        let field = P5136_KART_PHYSICS_LAYOUT[field_index];
        assert_eq!(field.kind, EncodedPhysicsFieldKind::U8);
        assert_eq!(
            block.as_bytes()[field.offset],
            encoded::encode_u8(expected),
            "{} at byte {}",
            field.name,
            field.offset
        );
    }

    fn assert_i32_field(block: &P5136KartPhysicsBlock, field_index: usize, expected: i32) {
        let field = P5136_KART_PHYSICS_LAYOUT[field_index];
        assert_eq!(field.kind, EncodedPhysicsFieldKind::I32);
        assert_eq!(
            &block.as_bytes()[field.offset..field.offset + 4],
            encoded::encode_i32(expected).as_slice(),
            "{} at byte {}",
            field.name,
            field.offset
        );
    }

    #[allow(clippy::too_many_lines)]
    fn csharp_fixture() -> P5136KartPhysicsSnapshot {
        P5136KartPhysicsSnapshot {
            speed_type: 7,
            speed: P5136SpeedSpecSnapshot {
                mass: 100.0,
                air_friction: -0.01,
                drag_factor: -0.05,
                forward_accel_force: 300.0,
                backward_accel_force: 90.0,
                grip_brake_force: 10.0,
                slip_brake_force: 5.0,
                max_steer_angle: 20.0,
                steer_constraint: 2.0,
                add_spec_steer_constraint: 7.0,
                front_grip_factor: 1.0,
                rear_grip_factor: 2.0,
                drift_trigger_factor: 3.0,
                drift_trigger_time: 4.0,
                drift_slip_factor: 5.0,
                drift_escape_force: 100.0,
                add_spec_drift_escape_force: 200.0,
                corner_draw_factor: 6.0,
                steer_lean_factor: 0.1,
                drift_max_gauge: 0.5,
                normal_booster_time: 3_000.0,
                team_booster_time: 4_500.0,
                trans_accel_factor: 1.0,
                add_spec_trans_accel_factor: 0.5,
                boost_accel_factor: 1.1,
            },
            kart: P5136KartSpecSnapshot {
                draft_mul_accel_factor: 1.1,
                draft_tick: 2_000,
                drift_boost_mul_accel_factor: 1.4,
                drift_boost_tick: 500,
                charge_boost_by_speed: 350.0,
                speed_slot_capacity: 2,
                item_slot_capacity: 2,
                special_slot_capacity: 1,
                use_transform_booster: 1,
                motorcycle_type: 0,
                bike_rear_wheel: 1,
                mass: 5.0,
                air_friction: 0.02,
                drag_factor: -0.03,
                forward_accel_force: 20.0,
                backward_accel_force: 10.0,
                grip_brake_force: 2.0,
                slip_brake_force: 3.0,
                max_steer_angle: 4.0,
                steer_constraint: 5.0,
                front_grip_factor: 0.1,
                rear_grip_factor: 0.2,
                drift_trigger_factor: 0.3,
                drift_trigger_time: 0.4,
                drift_slip_factor: 0.5,
                drift_escape_force: 30.0,
                corner_draw_factor: 0.6,
                drift_lean_factor: 0.07,
                steer_lean_factor: 0.01,
                drift_max_gauge: 0.2,
                normal_booster_time: 3_000.0,
                item_booster_time: 3_000.0,
                team_booster_time: 4_500.0,
                animal_booster_time: 4_000.0,
                super_booster_time: 3_500.0,
                trans_accel_factor: 1.5,
                boost_accel_factor: 1.5,
                start_booster_time_item: 1_000.0,
                start_booster_time_speed: 1_100.0,
                start_forward_accel_factor_item: 1.65,
                start_forward_accel_factor_speed: 1.77,
                drift_gauge_preserve_percent: 0.3,
                use_extended_after_booster: 1,
                boost_accel_factor_only_item: 1.5,
                anti_collide_balance: 1.0,
                dual_booster_set_auto: 1,
                dual_booster_tick_min: 40,
                dual_booster_tick_max: 60,
                dual_mul_accel_factor: 1.1,
                dual_trans_low_speed: 100.0,
                parts_engine_lock: 1,
                parts_wheel_lock: 0,
                parts_steering_lock: 1,
                parts_booster_lock: 0,
                parts_coating_lock: 1,
                parts_tail_lamp_lock: 0,
                charge_inst_accel_gauge_by_boost: 0.02,
                charge_inst_accel_gauge_by_grip: 0.03,
                charge_inst_accel_gauge_by_wall: 0.2,
                inst_accel_factor: 1.25,
                inst_accel_gauge_cooldown_time: 1_000,
                inst_accel_gauge_length: 2_000.0,
                inst_accel_gauge_min_usable: 500.0,
                inst_accel_gauge_min_vel_bound: 200.0,
                inst_accel_gauge_min_vel_loss: 50.0,
                use_extended_after_booster_more: 1,
                wall_coll_gauge_cooldown_time: 1_000,
                wall_coll_gauge_max_vel_loss: 200.0,
                wall_coll_gauge_min_vel_bound: 200.0,
                wall_coll_gauge_min_vel_loss: 50.0,
            },
            flying_pet: P5136FlyingPetSpecSnapshot {
                drift_escape_force: 10.0,
                normal_booster_time: 100.0,
                forward_accel_force: 5.0,
                drag_factor: 0.01,
                corner_draw_factor: 0.2,
                item_booster_time: 100.0,
                team_booster_time: 100.0,
                start_forward_accel_force_item: 50.0,
                start_forward_accel_force_speed: 60.0,
            },
            exc: P5136ExcSpecSnapshot {
                tune: P5136TuneSpecSnapshot {
                    drift_escape_force: 1.0,
                    normal_booster_time: 10.0,
                    trans_accel_factor: 0.01,
                    forward_accel: 2.0,
                    drag_factor: 0.001,
                    corner_draw_factor: 0.01,
                    drift_max_gauge: 0.01,
                    team_booster_time: 10.0,
                    start_booster_time_speed: 10.0,
                },
                plant43: P5136Plant43SpecSnapshot {
                    trans_accel_factor: 0.02,
                    forward_accel: 3.0,
                    drag_factor: 0.002,
                    start_booster_time_speed: 20.0,
                    start_forward_accel_item: 5.0,
                    start_forward_accel_speed: 6.0,
                },
                plant44: P5136Plant44SpecSnapshot {
                    grip_brake: 1.0,
                    slip_brake: 1.0,
                    steer_constraint: 0.1,
                    front_grip_factor: 0.01,
                    rear_grip_factor: 0.02,
                    corner_draw_factor: 0.02,
                },
                plant45: P5136Plant45SpecSnapshot {
                    drift_escape_force: 2.0,
                    drag_factor: 0.003,
                    slip_brake: 2.0,
                    corner_draw_factor: 0.03,
                    drift_max_gauge: 0.02,
                    animal_booster_time: 10.0,
                    anti_collide_balance: 0.1,
                },
                plant46: P5136Plant46SpecSnapshot {
                    speed_slot_capacity: 3,
                    item_slot_capacity: 4,
                    normal_booster_time: 20.0,
                    forward_accel: 4.0,
                    grip_brake: 2.0,
                    slip_brake: 3.0,
                    drift_slip_factor: 0.1,
                    drift_max_gauge: 0.03,
                    team_booster_time: 20.0,
                    animal_booster_time: 20.0,
                    start_booster_time_item: 10.0,
                    start_booster_time_speed: 30.0,
                    start_forward_accel_item: 7.0,
                    start_forward_accel_speed: 8.0,
                },
                kart_level: P5136KartLevelSpecSnapshot {
                    drift_escape_force: 3.0,
                    trans_accel_factor: 0.03,
                    forward_accel: 5.0,
                    drag_factor: 0.004,
                    steer_constraint: 0.2,
                    corner_draw_factor: 0.04,
                    start_booster_time_item: 20.0,
                    start_booster_time_speed: 40.0,
                    boost_accel_factor_only_item: 0.1,
                },
                parts: P5136PartOverrideSnapshot {
                    steer_constraint: 9.0,
                    drift_escape_force: 300.0,
                    normal_booster_time: 3_200.0,
                    trans_accel_factor: 2.0,
                },
            },
            speed_patch: P5136SpeedPatchSnapshot {
                drift_escape_force: 4.0,
                trans_accel_factor: 0.04,
                forward_accel_force: 6.0,
                drag_factor: 0.005,
                corner_draw_factor: 0.05,
                drift_max_gauge: 0.04,
                boost_accel_factor: 0.1,
                start_forward_accel_force_item: 9.0,
                start_forward_accel_force_speed: 10.0,
            },
            v2: P5136V2SpecSnapshot {
                parts_drift_escape_force: 5.0,
                level_drift_escape_force: 6.0,
                parts_normal_booster_time: 50.0,
                level_normal_booster_time: 60.0,
                parts_trans_accel_factor: 0.05,
                level_trans_accel_factor: 0.06,
                level_forward_accel_force: 7.0,
                parts_steer_constraint: 0.3,
                level_corner_draw_factor: 0.06,
                level_drift_max_gauge: 0.05,
                level_team_booster_time: 50.0,
                level_start_booster_time_speed: 50.0,
            },
        }
    }
}
