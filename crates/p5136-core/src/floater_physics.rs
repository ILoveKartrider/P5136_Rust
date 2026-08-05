//! P5136 legacy Floater/socket tuning contributions.
//!
//! The Korean 5136 server stores three tune codes in `TuneData.json`. Codes
//! 103 through 903 are the nine speed-physics options used by the C#
//! `Use_TuneSpec` path. The 10xxx codes are item-mode client abilities: they
//! are valid persistent Floater state but do not alter the server-authored kart
//! physics block.

use crate::kart_physics::P5136TuneSpecSnapshot;

pub const SPEED_FLOATER_CODES: &[i16] = &[103, 203, 303, 403, 503, 603, 703, 803, 903];
pub const ITEM_FLOATER_CODES: &[i16] = &[
    10_103, 10_203, 10_303, 10_401, 10_503, 10_603, 10_703, 10_803, 10_901, 11_001, 11_103, 11_201,
    11_301, 11_403, 11_501, 11_601, 11_701, 11_803, 11_903, 12_003,
];
pub const BLACK_FLOATER_CODES: [i16; 3] = [603, 703, 903];

#[must_use]
pub fn is_known_floater_code(code: i16) -> bool {
    code == 0 || SPEED_FLOATER_CODES.contains(&code) || ITEM_FLOATER_CODES.contains(&code)
}

/// Returns the C# source pool for one activation-kit selector.
///
/// Selector 6 is speed-only, selector 4 is item-only, and every other positive
/// selector uses the combined pool. Selector 5 is handled separately by the
/// caller because the reference server installs the fixed Black triple.
#[must_use]
pub fn floater_code_pool(selector: i16) -> Option<Vec<i16>> {
    if selector <= 0 {
        return None;
    }
    if selector == 6 {
        Some(SPEED_FLOATER_CODES.to_vec())
    } else if selector == 4 {
        Some(ITEM_FLOATER_CODES.to_vec())
    } else {
        Some(
            SPEED_FLOATER_CODES
                .iter()
                .chain(ITEM_FLOATER_CODES)
                .copied()
                .collect(),
        )
    }
}

/// Reconstructs the nine server-side physics additions from three persisted
/// Floater codes. Valid item-mode codes contribute zero because their effects
/// are consumed by client item logic rather than the race-start physics block.
#[must_use]
pub fn p5136_floater_spec(codes: [i16; 3]) -> Option<P5136TuneSpecSnapshot> {
    let mut seen = Vec::with_capacity(codes.len());
    for code in codes {
        if !is_known_floater_code(code) {
            return None;
        }
        if code != 0 {
            if seen.contains(&code) {
                return None;
            }
            seen.push(code);
        }
    }

    let mut spec = P5136TuneSpecSnapshot::default();
    for code in codes {
        match code {
            0 => {}
            103 => spec.drag_factor = -0.0022,
            203 => spec.forward_accel = 3.5,
            303 => spec.corner_draw_factor = 0.002,
            403 => spec.team_booster_time = 250.0,
            503 => spec.normal_booster_time = 190.0,
            603 => spec.start_booster_time_speed = 800.0,
            703 => spec.trans_accel_factor = 0.018,
            803 => spec.drift_max_gauge = -200.0,
            903 => spec.drift_escape_force = 210.0,
            _ if ITEM_FLOATER_CODES.contains(&code) => {}
            _ => return None,
        }
    }
    Some(spec)
}

#[cfg(test)]
mod tests {
    use super::{
        BLACK_FLOATER_CODES, ITEM_FLOATER_CODES, SPEED_FLOATER_CODES, floater_code_pool,
        p5136_floater_spec,
    };
    use crate::kart_physics::P5136TuneSpecSnapshot;

    #[test]
    fn all_nine_speed_codes_match_the_csharp_grade_three_table() {
        let expected = [
            (103, 0),
            (203, 1),
            (303, 2),
            (403, 3),
            (503, 4),
            (603, 5),
            (703, 6),
            (803, 7),
            (903, 8),
        ];
        for (code, index) in expected {
            let spec = p5136_floater_spec([code, 0, 0]).unwrap();
            let values = [
                spec.drag_factor,
                spec.forward_accel,
                spec.corner_draw_factor,
                spec.team_booster_time,
                spec.normal_booster_time,
                spec.start_booster_time_speed,
                spec.trans_accel_factor,
                spec.drift_max_gauge,
                spec.drift_escape_force,
            ];
            assert_ne!(values[index].to_bits(), 0.0_f32.to_bits());
            assert_eq!(
                values
                    .iter()
                    .enumerate()
                    .filter(|(candidate, value)| {
                        *candidate != index && value.to_bits() != 0.0_f32.to_bits()
                    })
                    .count(),
                0
            );
        }
    }

    #[test]
    fn item_codes_are_valid_but_do_not_fabricate_server_physics() {
        for &code in ITEM_FLOATER_CODES {
            assert_eq!(
                p5136_floater_spec([code, 0, 0]).unwrap(),
                P5136TuneSpecSnapshot::default()
            );
        }
        assert!(p5136_floater_spec([103, 103, 0]).is_none());
        assert!(p5136_floater_spec([i16::MAX, 0, 0]).is_none());
    }

    #[test]
    fn activation_kit_pools_and_black_fixed_triple_match_csharp() {
        assert_eq!(floater_code_pool(6).unwrap(), SPEED_FLOATER_CODES);
        assert_eq!(floater_code_pool(4).unwrap(), ITEM_FLOATER_CODES);
        assert!(floater_code_pool(1).unwrap().len() > SPEED_FLOATER_CODES.len());
        assert!(floater_code_pool(0).is_none());
        assert_eq!(BLACK_FLOATER_CODES, [603, 703, 903]);
    }
}
