//! Legacy grade-five kart point allocation to P5136 physics contributions.

use crate::kart_physics::P5136KartLevelSpecSnapshot;

pub const MAX_KART_LEVEL_SLOT: i16 = 10;
const MAX_KART_LEVEL_SLOT_INDEX: usize = 10;

const DRAG_FACTOR: [f32; 11] = [
    0.0, -0.0001, -0.0002, -0.0003, -0.0004, -0.0005, -0.0006, -0.0007, -0.0008, -0.001, -0.0012,
];
const FORWARD_ACCEL: [f32; 11] = [0.0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 1.0, 1.5];
const CORNER_DRAW_FACTOR: [f32; 11] = [
    0.0, 0.0001, 0.0002, 0.0003, 0.0004, 0.0005, 0.0006, 0.0007, 0.0008, 0.0009, 0.001,
];
const STEER_CONSTRAINT: [f32; 11] = [
    0.0, 0.01, 0.02, 0.03, 0.04, 0.05, 0.06, 0.08, 0.11, 0.15, 0.2,
];
const DRIFT_ESCAPE_FORCE: [f32; 11] =
    [0.0, 1.0, 3.0, 6.0, 10.0, 15.0, 20.0, 26.0, 33.0, 40.0, 50.0];
const TRANS_ACCEL_FACTOR: [f32; 11] = [
    0.0, 0.0001, 0.0003, 0.0006, 0.001, 0.0014, 0.0019, 0.0025, 0.0032, 0.004, 0.005,
];
const START_BOOSTER_TIME: [f32; 11] = [
    0.0, 5.0, 10.0, 15.0, 20.0, 30.0, 40.0, 50.0, 65.0, 80.0, 100.0,
];
const ITEM_BOOST_ACCEL_FACTOR: [f32; 11] = [
    0.0, 0.001, 0.003, 0.005, 0.009, 0.013, 0.019, 0.025, 0.033, 0.041, 0.05,
];

/// Resolves the four persisted point slots without indexing on untrusted
/// sidecar values. Invalid legacy data returns `None` and therefore contributes
/// no physics instead of panicking the server.
#[must_use]
pub fn p5136_kart_level_spec(allocation: [i16; 4]) -> Option<P5136KartLevelSpecSnapshot> {
    let [drag_slot, handling_slot, escape_slot, booster_slot] = allocation.map(|value| {
        usize::try_from(value)
            .ok()
            .filter(|value| *value <= MAX_KART_LEVEL_SLOT_INDEX)
    });
    let (drag_slot, handling_slot, escape_slot, booster_slot) =
        (drag_slot?, handling_slot?, escape_slot?, booster_slot?);

    Some(P5136KartLevelSpecSnapshot {
        drag_factor: DRAG_FACTOR[drag_slot],
        forward_accel: FORWARD_ACCEL[drag_slot],
        corner_draw_factor: CORNER_DRAW_FACTOR[handling_slot],
        steer_constraint: STEER_CONSTRAINT[handling_slot],
        drift_escape_force: DRIFT_ESCAPE_FORCE[escape_slot],
        trans_accel_factor: TRANS_ACCEL_FACTOR[booster_slot],
        start_booster_time_speed: START_BOOSTER_TIME[booster_slot],
        start_booster_time_item: START_BOOSTER_TIME[booster_slot],
        boost_accel_factor_only_item: ITEM_BOOST_ACCEL_FACTOR[booster_slot],
    })
}

#[cfg(test)]
mod tests {
    use crate::kart_physics::P5136KartLevelSpecSnapshot;

    use super::p5136_kart_level_spec;

    #[test]
    fn zero_and_maximum_allocations_match_the_csharp_tables() {
        assert_eq!(
            p5136_kart_level_spec([0; 4]).unwrap(),
            P5136KartLevelSpecSnapshot::default()
        );
        let maximum = p5136_kart_level_spec([10; 4]).unwrap();
        assert_eq!(maximum.drag_factor.to_bits(), (-0.0012_f32).to_bits());
        assert_eq!(maximum.forward_accel.to_bits(), 1.5_f32.to_bits());
        assert_eq!(maximum.steer_constraint.to_bits(), 0.2_f32.to_bits());
        assert_eq!(maximum.drift_escape_force.to_bits(), 50.0_f32.to_bits());
        assert_eq!(maximum.trans_accel_factor.to_bits(), 0.005_f32.to_bits());
        assert_eq!(
            maximum.start_booster_time_speed.to_bits(),
            100.0_f32.to_bits()
        );
        assert_eq!(
            maximum.boost_accel_factor_only_item.to_bits(),
            0.05_f32.to_bits()
        );
    }

    #[test]
    fn invalid_sidecar_indices_fail_closed_without_indexing() {
        assert!(p5136_kart_level_spec([-1, 0, 0, 0]).is_none());
        assert!(p5136_kart_level_spec([0, 0, 0, 11]).is_none());
    }
}
