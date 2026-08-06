//! P5136 legacy Floater/socket tuning contributions.
//!
//! The Korean 5136 server stores three tune codes in `TuneData.json`. Codes
//! 101 through 903 are the three grades of nine speed-physics options used by the C#
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

/// Stock P5136 karts whose Floater codes are fixed by the service catalog.
///
/// This numeric projection is intentionally compiled into the compatibility
/// layer. Runtime code neither scrapes localized shop descriptions nor relies
/// on a client-supplied sidecar for these non-upgradable built-in variants.
pub const INTRINSIC_FLOATER_CODES: &[(u16, [i16; 3])] = &[
    (628, [702, 602, 0]),
    (629, [502, 0, 0]),
    (631, [703, 603, 102]),
    (632, [503, 702, 0]),
    (633, [603, 903, 702]),
    (634, [703, 602, 0]),
    (635, [503, 0, 0]),
    (637, [703, 603, 0]),
    (638, [503, 0, 0]),
    (645, [703, 603, 102]),
    (646, [503, 702, 0]),
    (647, [603, 903, 702]),
    (648, [703, 603, 102]),
    (649, [503, 702, 0]),
    (650, [603, 903, 702]),
    (655, [601, 0, 0]),
    (667, [703, 603, 0]),
    (668, [503, 0, 0]),
    (669, [703, 603, 102]),
    (670, [503, 702, 0]),
    (671, [603, 903, 702]),
    (672, [703, 602, 0]),
    (673, [503, 0, 0]),
    (677, [701, 601, 0]),
    (678, [501, 0, 0]),
    (679, [503, 702, 0]),
    (680, [703, 603, 102]),
    (681, [603, 903, 702]),
    (688, [703, 0, 0]),
    (689, [503, 0, 0]),
    (700, [903, 702, 602]),
    (701, [903, 702, 602]),
    (702, [903, 702, 602]),
    (703, [903, 702, 602]),
    (704, [903, 702, 602]),
    (705, [503, 0, 0]),
    (708, [703, 603, 0]),
    (709, [503, 0, 0]),
    (710, [703, 602, 0]),
    (711, [503, 0, 0]),
    (712, [703, 603, 102]),
    (713, [701, 601, 101]),
    (718, [503, 702, 0]),
    (719, [603, 903, 702]),
    (740, [503, 702, 0]),
    (741, [703, 603, 102]),
    (742, [603, 903, 702]),
    (750, [903, 702, 602]),
    (751, [903, 702, 602]),
    (775, [703, 603, 0]),
    (776, [503, 0, 0]),
    (778, [703, 602, 0]),
    (779, [503, 0, 0]),
    (780, [502, 702, 0]),
    (787, [603, 903, 702]),
    (788, [903, 702, 602]),
];

#[must_use]
pub fn intrinsic_floater_codes(kart_id: u16) -> Option<[i16; 3]> {
    INTRINSIC_FLOATER_CODES
        .binary_search_by_key(&kart_id, |(candidate, _)| *candidate)
        .ok()
        .map(|index| INTRINSIC_FLOATER_CODES[index].1)
}

fn speed_floater_kind_and_grade(code: i16) -> Option<(usize, usize)> {
    let kind = code / 100;
    let grade = code % 100;
    if (1..=9).contains(&kind) && (1..=3).contains(&grade) {
        Some((usize::try_from(kind).ok()?, usize::try_from(grade).ok()?))
    } else {
        None
    }
}

#[must_use]
pub fn is_speed_floater_code(code: i16) -> bool {
    speed_floater_kind_and_grade(code).is_some()
}

#[must_use]
pub fn is_known_floater_code(code: i16) -> bool {
    code == 0 || speed_floater_kind_and_grade(code).is_some() || ITEM_FLOATER_CODES.contains(&code)
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
    let mut seen_speed_kinds = [false; 9];
    let mut seen_item_codes = Vec::with_capacity(codes.len());
    let mut spec = P5136TuneSpecSnapshot::default();
    for code in codes {
        if code == 0 {
            continue;
        }
        if let Some((kind, grade)) = speed_floater_kind_and_grade(code) {
            let seen = seen_speed_kinds.get_mut(kind - 1)?;
            if *seen {
                return None;
            }
            *seen = true;
            match kind {
                1 => spec.drag_factor = [0.0, -0.0008, -0.0015, -0.0022][grade],
                2 => spec.forward_accel = [0.0, 1.5, 2.5, 3.5][grade],
                3 => spec.corner_draw_factor = [0.0, 0.0007, 0.0014, 0.002][grade],
                4 => spec.team_booster_time = [0.0, 100.0, 180.0, 250.0][grade],
                5 => spec.normal_booster_time = [0.0, 70.0, 120.0, 190.0][grade],
                6 => spec.start_booster_time_speed = [0.0, 200.0, 400.0, 800.0][grade],
                7 => spec.trans_accel_factor = [0.0, 0.006, 0.01, 0.018][grade],
                8 => spec.drift_max_gauge = [0.0, -70.0, -140.0, -200.0][grade],
                9 => spec.drift_escape_force = [0.0, 80.0, 140.0, 210.0][grade],
                _ => return None,
            }
        } else if ITEM_FLOATER_CODES.contains(&code) {
            if seen_item_codes.contains(&code) {
                return None;
            }
            seen_item_codes.push(code);
        } else {
            return None;
        }
    }
    Some(spec)
}

#[cfg(test)]
mod tests {
    use super::{
        BLACK_FLOATER_CODES, INTRINSIC_FLOATER_CODES, ITEM_FLOATER_CODES, SPEED_FLOATER_CODES,
        floater_code_pool, intrinsic_floater_codes, p5136_floater_spec,
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
    fn intrinsic_grade_one_and_two_codes_match_the_csharp_tables() {
        let grade_one = p5136_floater_spec([101, 601, 901]).unwrap();
        assert_eq!(grade_one.drag_factor.to_bits(), (-0.0008_f32).to_bits());
        assert_eq!(
            grade_one.start_booster_time_speed.to_bits(),
            200.0_f32.to_bits()
        );
        assert_eq!(grade_one.drift_escape_force.to_bits(), 80.0_f32.to_bits());

        let grade_two = p5136_floater_spec([702, 802, 902]).unwrap();
        assert_eq!(grade_two.trans_accel_factor.to_bits(), 0.01_f32.to_bits());
        assert_eq!(grade_two.drift_max_gauge.to_bits(), (-140.0_f32).to_bits());
        assert_eq!(grade_two.drift_escape_force.to_bits(), 140.0_f32.to_bits());

        assert!(p5136_floater_spec([101, 103, 0]).is_none());
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

    #[test]
    fn intrinsic_kart_table_is_sorted_unique_and_every_triple_is_convertible() {
        assert_eq!(intrinsic_floater_codes(787), Some([603, 903, 702]));
        assert_eq!(intrinsic_floater_codes(764), None);
        for window in INTRINSIC_FLOATER_CODES.windows(2) {
            assert!(window[0].0 < window[1].0);
        }
        for &(kart_id, codes) in INTRINSIC_FLOATER_CODES {
            assert!(
                p5136_floater_spec(codes).is_some(),
                "intrinsic kart {kart_id} has invalid codes {codes:?}"
            );
        }
    }
}
