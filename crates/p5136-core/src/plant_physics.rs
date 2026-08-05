//! Recovered Korean P5136 plant-part performance contributions.
//!
//! The table mirrors the verified `zeta_/kr/enchant/enchantMaterials.xml`
//! snapshot already audited in the C# compatibility implementation. Cosmetic
//! and item-ability-only entries intentionally resolve successfully with a
//! zero physics contribution.

use crate::kart_physics::P5136ExcSpecSnapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum P5136PlantGameMode {
    Unknown,
    Speed,
    Item,
    Battle,
    TimeAttack,
}

impl P5136PlantGameMode {
    #[must_use]
    pub const fn from_room_game_type(game_type: u8) -> Self {
        match game_type {
            1 | 3 => Self::Speed,
            2 | 4 => Self::Item,
            _ => Self::Unknown,
        }
    }

    const fn mask(self) -> u8 {
        match self {
            Self::Unknown => 0,
            Self::Speed => 1,
            Self::Item => 2,
            Self::Battle => 4,
            Self::TimeAttack => 8,
        }
    }
}

const ALL: u8 = 1 | 2 | 4 | 8;
const SPEED: u8 = 1 | 8;
const ITEM: u8 = 2 | 4;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct PlantSpec {
    modes: u8,
    trans_accel_factor: f32,
    drag_factor: f32,
    start_forward_accel_speed: f32,
    start_forward_accel_item: f32,
    forward_accel: f32,
    start_booster_time_speed: f32,
    start_booster_time_item: f32,
    slip_brake: f32,
    grip_brake: f32,
    rear_grip_factor: f32,
    front_grip_factor: f32,
    corner_draw_factor: f32,
    steer_constraint: f32,
    drift_escape_force: f32,
    drift_max_gauge: f32,
    animal_booster_time: f32,
    anti_collide_balance: f32,
    normal_booster_time: f32,
    drift_slip_factor: f32,
    team_booster_time: f32,
    item_slot_capacity: u8,
    speed_slot_capacity: u8,
}

impl PlantSpec {
    const fn all() -> Self {
        Self {
            modes: ALL,
            trans_accel_factor: 0.0,
            drag_factor: 0.0,
            start_forward_accel_speed: 0.0,
            start_forward_accel_item: 0.0,
            forward_accel: 0.0,
            start_booster_time_speed: 0.0,
            start_booster_time_item: 0.0,
            slip_brake: 0.0,
            grip_brake: 0.0,
            rear_grip_factor: 0.0,
            front_grip_factor: 0.0,
            corner_draw_factor: 0.0,
            steer_constraint: 0.0,
            drift_escape_force: 0.0,
            drift_max_gauge: 0.0,
            animal_booster_time: 0.0,
            anti_collide_balance: 0.0,
            normal_booster_time: 0.0,
            drift_slip_factor: 0.0,
            team_booster_time: 0.0,
            item_slot_capacity: 0,
            speed_slot_capacity: 0,
        }
    }
}

/// Applies one recovered plant part to its category-specific physics slot.
///
/// `true` means that the category/ID is known, including cosmetic,
/// ability-only, and mode-inactive records whose numeric contribution is zero.
#[must_use]
pub fn apply_p5136_plant_part(
    target: &mut P5136ExcSpecSnapshot,
    category: i16,
    id: i16,
    mode: P5136PlantGameMode,
) -> bool {
    let Some(spec) = plant_spec(category, id) else {
        return false;
    };
    if spec.modes != ALL && spec.modes & mode.mask() == 0 {
        return true;
    }

    match category {
        43 => {
            target.plant43.trans_accel_factor = spec.trans_accel_factor;
            target.plant43.drag_factor = spec.drag_factor;
            target.plant43.start_forward_accel_speed = spec.start_forward_accel_speed;
            target.plant43.start_forward_accel_item = spec.start_forward_accel_item;
            target.plant43.forward_accel = spec.forward_accel;
            target.plant43.start_booster_time_speed = spec.start_booster_time_speed;
        }
        44 => {
            target.plant44.slip_brake = spec.slip_brake;
            target.plant44.grip_brake = spec.grip_brake;
            target.plant44.rear_grip_factor = spec.rear_grip_factor;
            target.plant44.front_grip_factor = spec.front_grip_factor;
            target.plant44.corner_draw_factor = spec.corner_draw_factor;
            target.plant44.steer_constraint = spec.steer_constraint;
        }
        45 => {
            target.plant45.drift_escape_force = spec.drift_escape_force;
            target.plant45.drift_max_gauge = spec.drift_max_gauge;
            target.plant45.corner_draw_factor = spec.corner_draw_factor;
            target.plant45.slip_brake = spec.slip_brake;
            target.plant45.animal_booster_time = spec.animal_booster_time;
            target.plant45.anti_collide_balance = spec.anti_collide_balance;
            target.plant45.drag_factor = spec.drag_factor;
        }
        46 => {
            target.plant46.drift_max_gauge = spec.drift_max_gauge;
            target.plant46.normal_booster_time = spec.normal_booster_time;
            target.plant46.drift_slip_factor = spec.drift_slip_factor;
            target.plant46.forward_accel = spec.forward_accel;
            target.plant46.animal_booster_time = spec.animal_booster_time;
            target.plant46.team_booster_time = spec.team_booster_time;
            target.plant46.start_forward_accel_speed = spec.start_forward_accel_speed;
            target.plant46.start_forward_accel_item = spec.start_forward_accel_item;
            target.plant46.start_booster_time_speed = spec.start_booster_time_speed;
            target.plant46.start_booster_time_item = spec.start_booster_time_item;
            target.plant46.item_slot_capacity = spec.item_slot_capacity;
            target.plant46.speed_slot_capacity = spec.speed_slot_capacity;
            target.plant46.grip_brake = spec.grip_brake;
            target.plant46.slip_brake = spec.slip_brake;
        }
        _ => unreachable!("plant_spec only returns categories 43 through 46"),
    }
    true
}

#[allow(
    clippy::too_many_lines,
    reason = "audited immutable P5136 compatibility table"
)]
fn plant_spec(category: i16, id: i16) -> Option<PlantSpec> {
    let mut spec = PlantSpec::all();
    match (category, id) {
        (43, 1) => {
            spec.trans_accel_factor = 0.002;
            spec.drag_factor = -0.0007;
            spec.start_forward_accel_speed = 0.02;
        }
        (43, 2) => {
            spec.trans_accel_factor = 0.002;
            spec.forward_accel = 2.0;
        }
        (43, 3) => {
            spec.start_forward_accel_speed = 0.02;
            spec.start_booster_time_speed = 15.0;
        }
        (43, 4) => spec.start_forward_accel_speed = 0.04,
        (43, 5) => spec.start_forward_accel_item = 0.04,
        (43, 6) => {
            spec.modes = SPEED;
            spec.drag_factor = -0.0021;
        }
        (43, 7) => spec.drag_factor = -0.0014,
        (43, 8) => {
            spec.modes = SPEED;
            spec.forward_accel = 1.0;
            spec.start_forward_accel_speed = 0.02;
        }
        (43, 9) => {
            spec.forward_accel = 1.0;
            spec.start_forward_accel_speed = 0.02;
        }
        (43, 10) => {
            spec.modes = SPEED;
            spec.forward_accel = 2.0;
        }
        (43, 11) => spec.forward_accel = 2.0,
        (43, 12) => {
            spec.drag_factor = -0.0007;
            spec.forward_accel = 1.0;
        }
        (43, 13) => {
            spec.modes = SPEED;
            spec.drag_factor = -0.0007;
            spec.forward_accel = 1.0;
        }
        (43, 14) => spec.drag_factor = -0.0007,
        (43, 15) => {
            spec.modes = SPEED;
            spec.drag_factor = -0.0014;
        }
        (43, 16) => {
            spec.trans_accel_factor = 0.0002;
            spec.drag_factor = -0.0014;
        }
        (43, 17) => {
            spec.trans_accel_factor = 0.0004;
            spec.drag_factor = -0.0007;
        }
        (43, 18) => {
            spec.trans_accel_factor = 0.0002;
            spec.forward_accel = 2.0;
        }
        (43, 19) => {
            spec.trans_accel_factor = 0.0004;
            spec.forward_accel = 1.0;
        }
        (43, 20) => {
            spec.trans_accel_factor = 0.0006;
            spec.forward_accel = 1.0;
        }
        (43, 21) => spec.trans_accel_factor = 0.0008,
        (43, 22) => {
            spec.trans_accel_factor = 0.0012;
            spec.drag_factor = -0.0014;
        }
        (43, 23) => {
            spec.forward_accel = 1.0;
            spec.trans_accel_factor = 0.002;
            spec.start_booster_time_speed = 30.0;
        }

        (44, 1) => {
            spec.slip_brake = -40.0;
            spec.grip_brake = -40.0;
            spec.rear_grip_factor = 0.2;
            spec.front_grip_factor = 0.2;
            spec.corner_draw_factor = 0.0005;
        }
        (44, 2) => {
            spec.modes = SPEED;
            spec.grip_brake = -12.0;
            spec.rear_grip_factor = 0.3;
            spec.front_grip_factor = 0.3;
            spec.corner_draw_factor = 0.001;
        }
        (44, 3) => {
            spec.slip_brake = -10.0;
            spec.rear_grip_factor = 0.2;
            spec.front_grip_factor = 0.2;
        }
        (44, 4) => {
            spec.rear_grip_factor = 0.1;
            spec.front_grip_factor = 0.1;
        }
        (44, 5) => {
            spec.rear_grip_factor = 0.05;
            spec.front_grip_factor = 0.05;
            spec.grip_brake = -20.0;
        }
        (44, 6) => spec.grip_brake = -20.0,
        (44, 7) => spec.grip_brake = -15.0,
        (44, 8) => spec.steer_constraint = 0.2,
        (44, 9) => spec.steer_constraint = 0.4,
        (44, 10) => spec.steer_constraint = 0.8,
        (44, 11) => spec.steer_constraint = -0.4,
        (44, 12) => {
            spec.grip_brake = -5.0;
            spec.slip_brake = -8.0;
        }
        (44, 13) => {
            spec.grip_brake = -7.0;
            spec.slip_brake = -6.0;
        }
        (44, 14) => {
            spec.grip_brake = -9.0;
            spec.slip_brake = -4.0;
        }
        (44, 15) => {
            spec.grip_brake = -11.0;
            spec.slip_brake = -2.0;
        }

        (45, 1) => {
            spec.drift_escape_force = 70.0;
            spec.drift_max_gauge = -40.0;
            spec.corner_draw_factor = 0.001;
        }
        (45, 2) => {
            spec.drift_max_gauge = -60.0;
            spec.slip_brake = -192.0;
        }
        (45, 3) => {
            spec.animal_booster_time = 100.0;
            spec.drift_escape_force = 70.0;
        }
        (45, 4) => spec.drift_max_gauge = -60.0,
        (45, 5) => {
            spec.drift_max_gauge = -40.0;
            spec.animal_booster_time = 100.0;
        }
        (45, 6) => spec.drift_escape_force = 50.0,
        (45, 7) => {
            spec.drift_escape_force = 30.0;
            spec.corner_draw_factor = 0.0005;
        }
        (45, 8) => spec.drift_max_gauge = -40.0,
        (45, 9) => {
            spec.drift_max_gauge = -60.0;
            spec.drift_escape_force = -20.0;
        }
        (45, 10) => {
            spec.drift_max_gauge = -100.0;
            spec.drift_escape_force = -60.0;
        }
        (45, 11) => {
            spec.drift_max_gauge = -80.0;
            spec.drift_escape_force = -40.0;
        }
        (45, 12) => spec.drift_escape_force = 10.0,
        (45, 13) => spec.drift_escape_force = 30.0,
        (45, 14) => {
            spec.modes = ITEM;
            spec.drift_escape_force = 50.0;
            spec.drift_max_gauge = 40.0;
        }
        (45, 15) => {
            spec.drift_escape_force = 70.0;
            spec.drift_max_gauge = 60.0;
        }
        (45, 16) => {
            spec.anti_collide_balance = -0.005;
            spec.corner_draw_factor = 0.0005;
        }
        (45, 17) => {
            spec.anti_collide_balance = -0.005;
            spec.drag_factor = -0.0007;
        }
        (45, 18) => {
            spec.anti_collide_balance = -0.005;
            spec.drift_max_gauge = -40.0;
        }
        (45, 19) => spec.anti_collide_balance = -0.01,
        (45, 20) => {
            spec.anti_collide_balance = -0.01;
            spec.drift_max_gauge = -30.0;
        }
        (45, 21) => spec.anti_collide_balance = -0.015,
        (45, 22) => {
            spec.anti_collide_balance = -0.02;
            spec.drift_escape_force = 30.0;
        }
        (45, 23) => {
            spec.drift_escape_force = 90.0;
            spec.corner_draw_factor = 0.0005;
        }

        (46, 1) => {
            spec.modes = SPEED;
            spec.drift_max_gauge = -80.0;
            spec.normal_booster_time = 120.0;
        }
        (46, 2) => {
            spec.drift_slip_factor = -0.03;
            spec.forward_accel = 2.0;
        }
        (46, 3) => spec.animal_booster_time = 200.0,
        (46, 4 | 10 | 13 | 14 | 19 | 20 | 27 | 28 | 29 | 30) => {}
        (46, 5) => {
            spec.normal_booster_time = 90.0;
            spec.team_booster_time = 60.0;
            spec.animal_booster_time = 50.0;
            spec.start_forward_accel_speed = 0.02;
            spec.start_forward_accel_item = 0.02;
        }
        (46, 6) => {
            spec.normal_booster_time = 60.0;
            spec.animal_booster_time = 80.0;
        }
        (46, 7) => spec.start_booster_time_speed = 105.0,
        (46, 8) => spec.start_booster_time_item = 105.0,
        (46, 9) => spec.start_booster_time_speed = 195.0,
        (46, 11) => {
            spec.modes = 4;
            spec.item_slot_capacity = 3;
        }
        (46, 12) => {
            spec.modes = 8;
            spec.speed_slot_capacity = 3;
        }
        (46, 15) => {
            spec.animal_booster_time = 100.0;
            spec.grip_brake = 10.0;
        }
        (46, 16) => {
            spec.animal_booster_time = 100.0;
            spec.slip_brake = 10.0;
        }
        (46, 17) => {
            spec.modes = 4;
            spec.animal_booster_time = 100.0;
            spec.slip_brake = 9.0;
        }
        (46, 18) => {
            spec.modes = 4;
            spec.animal_booster_time = 120.0;
        }
        (46, 21) => spec.start_booster_time_speed = 150.0,
        (46, 22) => spec.forward_accel = 1.5,
        (46, 23) => spec.normal_booster_time = 60.0,
        (46, 24) => spec.team_booster_time = 60.0,
        (46, 25) => {
            spec.normal_booster_time = 90.0;
            spec.team_booster_time = -30.0;
        }
        (46, 26) => {
            spec.normal_booster_time = -30.0;
            spec.team_booster_time = 90.0;
        }
        _ => return None,
    }
    Some(spec)
}

#[cfg(test)]
mod tests {
    use super::{P5136PlantGameMode, apply_p5136_plant_part};
    use crate::kart_physics::P5136ExcSpecSnapshot;

    #[test]
    fn room_game_types_match_csharp_individual_and_team_modes() {
        assert_eq!(
            P5136PlantGameMode::from_room_game_type(1),
            P5136PlantGameMode::Speed
        );
        assert_eq!(
            P5136PlantGameMode::from_room_game_type(2),
            P5136PlantGameMode::Item
        );
        assert_eq!(
            P5136PlantGameMode::from_room_game_type(3),
            P5136PlantGameMode::Speed
        );
        assert_eq!(
            P5136PlantGameMode::from_room_game_type(4),
            P5136PlantGameMode::Item
        );
    }

    #[test]
    fn recovered_category_counts_are_complete() {
        let expected = [(43, 23), (44, 15), (45, 23), (46, 30)];
        for (category, count) in expected {
            assert_eq!(
                (1..=count)
                    .filter(|id| {
                        let mut target = P5136ExcSpecSnapshot::default();
                        apply_p5136_plant_part(
                            &mut target,
                            category,
                            *id,
                            P5136PlantGameMode::Speed,
                        )
                    })
                    .count(),
                usize::try_from(count).unwrap()
            );
        }
    }

    #[test]
    fn live_equipped_parts_apply_exact_recovered_values_and_modes() {
        let mut speed = P5136ExcSpecSnapshot::default();
        assert!(apply_p5136_plant_part(
            &mut speed,
            43,
            23,
            P5136PlantGameMode::Speed
        ));
        assert!(apply_p5136_plant_part(
            &mut speed,
            44,
            2,
            P5136PlantGameMode::Speed
        ));
        assert!(apply_p5136_plant_part(
            &mut speed,
            45,
            23,
            P5136PlantGameMode::Speed
        ));
        assert!(apply_p5136_plant_part(
            &mut speed,
            46,
            1,
            P5136PlantGameMode::Speed
        ));
        assert_eq!(speed.plant43.forward_accel.to_bits(), 1.0_f32.to_bits());
        assert_eq!(
            speed.plant43.trans_accel_factor.to_bits(),
            0.002_f32.to_bits()
        );
        assert_eq!(
            speed.plant43.start_booster_time_speed.to_bits(),
            30.0_f32.to_bits()
        );
        assert_eq!(speed.plant44.steer_constraint.to_bits(), 0.0_f32.to_bits());
        assert_eq!(speed.plant44.grip_brake.to_bits(), (-12.0_f32).to_bits());
        assert_eq!(
            speed.plant45.drift_escape_force.to_bits(),
            90.0_f32.to_bits()
        );
        assert_eq!(
            speed.plant46.normal_booster_time.to_bits(),
            120.0_f32.to_bits()
        );

        let mut item = P5136ExcSpecSnapshot::default();
        assert!(apply_p5136_plant_part(
            &mut item,
            44,
            2,
            P5136PlantGameMode::Item
        ));
        assert_eq!(item.plant44, P5136ExcSpecSnapshot::default().plant44);
    }

    #[test]
    fn ability_only_parts_are_known_without_fabricated_physics() {
        let mut target = P5136ExcSpecSnapshot::default();
        assert!(apply_p5136_plant_part(
            &mut target,
            46,
            4,
            P5136PlantGameMode::Item
        ));
        assert_eq!(target, P5136ExcSpecSnapshot::default());
        assert!(!apply_p5136_plant_part(
            &mut target,
            46,
            31,
            P5136PlantGameMode::Item
        ));
    }
}
