//! Profile- and catalog-backed construction of the legacy inventory preload.

use std::collections::{BTreeMap, HashSet};

use p5136_core::inventory::{InventorySnapshot, RiderItemGroup, RiderItemRecord};
use thiserror::Error;

use crate::{CatalogInventory, EquipmentExceptions, Profile, RiderItems, is_grant_item};

const PRE_PARTS_CATEGORY_ORDER: &[u16] = &[
    21, 52, 1, 32, 16, 11, 8, 9, 61, 7, 28, 22, 23, 12, 13, 18, 20, 4, 31, 27, 26, 70, 2, 30, 36,
    55, 59, 43, 44, 45, 46, 37, 38, 49, 53, 39,
];
const POST_PARTS_CATEGORY_ORDER: &[u16] = &[68, 69, 67, 14, 3];
const UNIT_AMOUNT_CATEGORIES: &[u16] = &[
    1, 2, 3, 4, 8, 11, 12, 14, 16, 18, 20, 21, 26, 27, 28, 30, 31, 32, 52, 61, 70,
];

const MINIMUM_GRANT_RECORDS: usize = 5_000;
const MINIMUM_GRANT_KARTS: usize = 1_200;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum InventoryBuildError {
    #[error("P5136 grant inventory category {category} is missing")]
    MissingCategory { category: u16 },

    #[error("P5136 grant inventory is incomplete (items={items}, karts={karts})")]
    Incomplete { items: usize, karts: usize },
}

/// Encodes the exact 65-byte equipment block reused by rider, room, and race
/// packets in the Korean 5136 protocol.
#[must_use]
pub fn rider_item_snapshot(items: &RiderItems) -> [u8; 65] {
    let mut output = [0_u8; 65];
    let values = [
        items.character,
        items.paint,
        items.kart,
        items.plate,
        items.goggle,
        items.balloon,
        items.unknown1,
        items.head_band,
        items.head_phone,
        items.hand_gear_left,
        items.unknown2,
        items.uniform,
        items.decal,
        items.pet,
        items.flying_pet,
        items.aura,
        items.skid_mark,
        items.special_kit,
        items.rider_color,
        items.bonus_card,
        items.boss_mode_card,
        items.kart_plant1,
        items.kart_plant2,
        items.kart_plant3,
        items.kart_plant4,
        items.unknown3,
        items.fishing_pole,
        items.tachometer,
        items.dye,
        normalize_kart_serial(items.kart, items.kart_serial),
    ];
    for (index, value) in values.into_iter().enumerate() {
        let offset = index * 2;
        output[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }
    output[60] = items.unknown4;
    output[61..63].copy_from_slice(&items.kart_coating.to_le_bytes());
    output[63..65].copy_from_slice(&items.kart_tail_lamp.to_le_bytes());
    output
}

/// Builds the complete catalog-backed rider-item portion of `PqGetRider`.
///
/// Plant and equipped-parts exception records are profile sidecars in the C#
/// implementation. They remain empty here until those two sidecar formats are
/// loaded; the item stream itself is complete and preserves its physical
/// category/X-parts boundaries.
pub fn build_inventory_snapshot(
    catalog: &CatalogInventory,
    profile: &Profile,
) -> Result<InventorySnapshot, InventoryBuildError> {
    build_inventory_snapshot_with_equipment(catalog, profile, EquipmentExceptions::default())
}

pub fn build_inventory_snapshot_with_equipment(
    catalog: &CatalogInventory,
    profile: &Profile,
    equipment: EquipmentExceptions,
) -> Result<InventorySnapshot, InventoryBuildError> {
    let prevent_item = profile.server_setting.prevent_item_use != 0;
    let slot_changer = profile.rider.slot_changer;
    let mut records = catalog
        .grant_items()
        .map(|item| {
            RiderItemRecord::normal(
                item.category,
                item.id,
                if item.category == 3 { 1 } else { item.serial },
                catalog_amount(item.category, item.id, slot_changer),
                prevent_item,
            )
        })
        .collect::<Vec<_>>();
    add_granted_karts(&mut records, catalog, profile, prevent_item);

    let kart_count = records.iter().filter(|record| record.category == 3).count();
    if records.len() < MINIMUM_GRANT_RECORDS || kart_count < MINIMUM_GRANT_KARTS {
        return Err(InventoryBuildError::Incomplete {
            items: records.len(),
            karts: kart_count,
        });
    }

    let mut by_category = BTreeMap::<u16, Vec<RiderItemRecord>>::new();
    for record in records {
        by_category.entry(record.category).or_default().push(record);
    }

    let mut item_groups =
        Vec::with_capacity(PRE_PARTS_CATEGORY_ORDER.len() + 8 + POST_PARTS_CATEGORY_ORDER.len());
    add_catalog_groups(&mut item_groups, &mut by_category, PRE_PARTS_CATEGORY_ORDER)?;

    add_generated_parts_groups(&mut item_groups, slot_changer);

    add_catalog_groups(
        &mut item_groups,
        &mut by_category,
        POST_PARTS_CATEGORY_ORDER,
    )?;

    Ok(InventorySnapshot {
        plant_exceptions: equipment.plant,
        parts_exceptions: equipment.parts,
        item_groups,
    })
}

fn add_generated_parts_groups(item_groups: &mut Vec<RiderItemGroup>, slot_changer: u16) {
    add_parts_group(
        item_groups,
        1,
        1,
        [1_053, 1_053, 1_053, 1_053],
        1_080,
        3,
        slot_changer,
    );
    add_parts_group(
        item_groups,
        1,
        2,
        [1_005, 1_005, 1_005, 1_005],
        1_050,
        5,
        slot_changer,
    );
    add_parts_group(
        item_groups,
        1,
        3,
        [910, 910, 910, 910],
        1_000,
        10,
        slot_changer,
    );
    add_parts_group(
        item_groups,
        1,
        4,
        [810, 810, 810, 810],
        900,
        10,
        slot_changer,
    );
    add_parts_group(
        item_groups,
        2,
        1,
        [1_153, 1_053, 1_153, 1_053],
        1_180,
        3,
        slot_changer,
    );
    add_parts_group(
        item_groups,
        2,
        2,
        [1_105, 1_005, 1_105, 1_005],
        1_150,
        5,
        slot_changer,
    );
    add_parts_group(
        item_groups,
        2,
        3,
        [1_010, 910, 1_010, 910],
        1_100,
        10,
        slot_changer,
    );
    add_parts_group(
        item_groups,
        2,
        4,
        [910, 810, 910, 810],
        1_000,
        10,
        slot_changer,
    );
}

fn catalog_amount(category: u16, id: u16, slot_changer: u16) -> u16 {
    if UNIT_AMOUNT_CATEGORIES.binary_search(&category).is_ok()
        || (category == 7 && matches!(id, 3 | 4))
    {
        1
    } else {
        slot_changer
    }
}

const fn normalize_kart_serial(kart_id: u16, serial: u16) -> u16 {
    if kart_id != 0 && serial == 0 {
        1
    } else {
        serial
    }
}

fn add_granted_karts(
    records: &mut Vec<RiderItemRecord>,
    catalog: &CatalogInventory,
    profile: &Profile,
    prevent_item: bool,
) {
    let known_karts = catalog
        .items()
        .iter()
        .filter(|item| item.category == 3 && is_grant_item(item))
        .map(|item| item.id)
        .collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    records.extend(profile.granted_karts.iter().filter_map(|grant| {
        if grant.serial < 2
            || grant.serial > i16::MAX as u16
            || !known_karts.contains(&grant.kart_id)
            || !seen.insert((grant.kart_id, grant.serial))
        {
            return None;
        }
        Some(RiderItemRecord::normal(
            3,
            grant.kart_id,
            grant.serial,
            1,
            prevent_item,
        ))
    }));
}

fn add_catalog_groups(
    destination: &mut Vec<RiderItemGroup>,
    by_category: &mut BTreeMap<u16, Vec<RiderItemRecord>>,
    category_order: &[u16],
) -> Result<(), InventoryBuildError> {
    for &category in category_order {
        let records = by_category
            .remove(&category)
            .ok_or(InventoryBuildError::MissingCategory { category })?;
        destination.push(RiderItemGroup::new(records));
    }
    Ok(())
}

fn add_parts_group(
    destination: &mut Vec<RiderItemGroup>,
    item_id: u16,
    grade: u8,
    starts: [i16; 4],
    end: i16,
    step: i16,
    amount: u16,
) {
    let mut records = Vec::with_capacity(40);
    for (category, start) in (63_u16..=66).zip(starts) {
        let adjusted_end = end - starts[0] + start;
        let mut value = start;
        while value <= adjusted_end {
            records.push(RiderItemRecord::part(
                category, item_id, amount, grade, value,
            ));
            value += step;
        }
    }
    destination.push(RiderItemGroup::new(records));
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fmt::Write as _};

    use p5136_core::inventory::{PartsExcRecord, PlantExcRecord, RiderItemGroup};

    use crate::{CatalogInventory, EquipmentExceptions, GrantedKart, Profile};

    use super::{
        InventoryBuildError, add_catalog_groups, build_inventory_snapshot_with_equipment,
        rider_item_snapshot,
    };

    fn complete_catalog_xml() -> String {
        const GRANT_CATEGORIES: &[u16] = &[
            1, 2, 3, 4, 7, 8, 9, 11, 12, 13, 14, 16, 18, 20, 21, 22, 23, 26, 27, 28, 30, 31, 32,
            36, 37, 38, 39, 43, 44, 45, 46, 49, 52, 53, 55, 59, 61, 67, 68, 69, 70,
        ];
        const OTHER_CATEGORIES: &[u16] = &[
            5, 6, 10, 15, 17, 19, 24, 25, 29, 33, 34, 35, 40, 41, 42, 47, 48, 50, 51,
        ];

        let mut items = Vec::new();
        for &category in GRANT_CATEGORIES {
            let count = if category == 3 { 1_300 } else { 120 };
            for id in 1..=count {
                items.push((category, id));
            }
        }
        items.push((3, 1_450));
        items.push((3, 1_453));
        for &category in OTHER_CATEGORIES {
            for id in 1..=40 {
                items.push((category, id));
            }
        }

        let mut xml = format!(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr"><Inventory total="{}" categories="60">"#,
            items.len()
        );
        for (category, id) in items {
            write!(xml, r#"<Item category="{category}" id="{id}" />"#).unwrap();
        }
        xml.push_str("</Inventory></KartCatalog>");
        xml
    }

    #[test]
    fn builds_csharp_category_and_xparts_order() {
        let catalog = CatalogInventory::from_xml(complete_catalog_xml().as_bytes()).unwrap();
        let mut profile = Profile::default();
        profile.server_setting.prevent_item_use = 1;
        profile.rider.slot_changer = 321;
        profile.granted_karts = vec![
            GrantedKart {
                kart_id: 1_450,
                serial: 2,
            },
            GrantedKart {
                kart_id: 1_450,
                serial: 2,
            },
            GrantedKart {
                kart_id: 1_453,
                serial: 1,
            },
        ];

        let equipment = EquipmentExceptions {
            plant: vec![PlantExcRecord {
                id: 1_450,
                serial: 2,
                engine_category: 43,
                engine_id: 1,
                handle_category: 44,
                handle_id: 2,
                wheel_category: 45,
                wheel_id: 3,
                kit_category: 46,
                kit_id: 4,
            }],
            parts: vec![PartsExcRecord {
                id: 1_450,
                serial: 2,
                engine: 1,
                engine_grade: 1,
                engine_value: 1_053,
                handle: 2,
                handle_grade: 1,
                handle_value: 1_053,
                wheel: 3,
                wheel_grade: 1,
                wheel_value: 1_053,
                booster: 4,
                booster_grade: 1,
                booster_value: 1_053,
                coating: 5,
                tail_lamp: 6,
            }],
        };
        let snapshot =
            build_inventory_snapshot_with_equipment(&catalog, &profile, equipment).unwrap();
        assert_eq!(snapshot.plant_exceptions.len(), 1);
        assert_eq!(snapshot.parts_exceptions.len(), 1);
        assert_eq!(snapshot.item_groups.len(), 49);
        assert_eq!(snapshot.item_groups[0].records[0].category, 21);
        assert_eq!(snapshot.item_groups[35].records[0].category, 39);
        assert_eq!(snapshot.item_groups[36].records[0].category, 63);
        assert_eq!(snapshot.item_groups[36].records[0].grade, 1);
        assert_eq!(snapshot.item_groups[36].records[0].value, 1_053);
        assert_eq!(snapshot.item_groups[43].records[0].grade, 4);
        assert_eq!(snapshot.item_groups[44].records[0].category, 68);
        assert_eq!(snapshot.item_groups[48].records[0].category, 3);

        let category_three = &snapshot.item_groups[48].records;
        assert_eq!(
            category_three
                .iter()
                .filter(|record| record.id == 1_450 && record.serial == 2)
                .count(),
            1
        );
        assert!(category_three.iter().all(|record| record.amount == 1));
        assert!(category_three.iter().all(|record| record.prevent == 1));

        let category_seven = &snapshot.item_groups[9].records;
        assert_eq!(
            category_seven
                .iter()
                .find(|record| record.id == 3)
                .unwrap()
                .amount,
            1
        );
        assert_eq!(
            category_seven
                .iter()
                .find(|record| record.id == 5)
                .unwrap()
                .amount,
            321
        );
    }

    #[test]
    fn reports_a_missing_required_grant_category() {
        let mut destination = Vec::<RiderItemGroup>::new();
        let mut categories = BTreeMap::new();
        let result = add_catalog_groups(&mut destination, &mut categories, &[21]);
        assert_eq!(
            result,
            Err(InventoryBuildError::MissingCategory { category: 21 })
        );
    }

    #[test]
    fn rider_item_snapshot_matches_the_exact_65_byte_csharp_layout() {
        let mut profile = Profile::default();
        profile.rider_item.kart = 1_450;
        profile.rider_item.kart_serial = 0;
        profile.rider_item.pet = 0x1234;
        profile.rider_item.unknown4 = 0x56;
        profile.rider_item.kart_coating = 0x789a;
        profile.rider_item.kart_tail_lamp = 0xbcde;

        let snapshot = rider_item_snapshot(&profile.rider_item);
        assert_eq!(snapshot.len(), 65);
        assert_eq!(
            u16::from_le_bytes(snapshot[4..6].try_into().unwrap()),
            1_450
        );
        assert_eq!(
            u16::from_le_bytes(snapshot[26..28].try_into().unwrap()),
            0x1234
        );
        assert_eq!(u16::from_le_bytes(snapshot[58..60].try_into().unwrap()), 1);
        assert_eq!(snapshot[60], 0x56);
        assert_eq!(
            u16::from_le_bytes(snapshot[61..63].try_into().unwrap()),
            0x789a
        );
        assert_eq!(
            u16::from_le_bytes(snapshot[63..65].try_into().unwrap()),
            0xbcde
        );
    }
}
