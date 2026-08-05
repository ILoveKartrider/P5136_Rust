//! Bounded loading of per-rider plant state and legacy equipped-parts state.

use std::{
    borrow::Cow,
    cell::Cell,
    collections::BTreeMap,
    fmt,
    fs::{self, File, OpenOptions},
    io::{Read, Take, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt as _, OpenOptionsSyncExt as _};
use cap_std::fs::{Dir as CapabilityDir, OpenOptions as CapabilityOpenOptions};
use p5136_core::{
    equipment_protocol::{PlantPartEquipRequest, XPartEquipRequest},
    floater_physics::{BLACK_FLOATER_CODES, floater_code_pool, is_known_floater_code},
    inventory::{KartLevelExcRecord, PartsExcRecord, PlantExcRecord, TuneExcRecord},
};
use quick_xml::{
    Reader, XmlVersion,
    events::{BytesStart, Event},
    name::QName,
};
use rand::seq::SliceRandom;
use serde::{
    Deserialize, Serialize,
    de::{DeserializeSeed, Deserializer, Error as _, SeqAccess, Visitor},
};
use thiserror::Error;

use crate::store::ProfileStoreError;

pub const MAX_EQUIPMENT_STATE_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_EQUIPMENT_RECORDS: usize = 65_535;
static EQUIPMENT_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);
#[cfg(test)]
thread_local! {
    static FAIL_NEXT_EQUIPMENT_DIRECTORY_SYNC: Cell<bool> = const { Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn fail_next_equipment_directory_sync() {
    FAIL_NEXT_EQUIPMENT_DIRECTORY_SYNC.set(true);
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct EquipmentExceptions {
    pub tune: Vec<TuneExcRecord>,
    pub plant: Vec<PlantExcRecord>,
    pub kart_level: Vec<KartLevelExcRecord>,
    pub parts: Vec<PartsExcRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EquipmentSidecar {
    Tune,
    Plant,
    KartLevel,
    GlobalParts,
    RiderParts,
    MergedParts,
}

#[derive(Debug)]
pub struct EquipmentLoadWarning {
    pub sidecar: EquipmentSidecar,
    pub error: EquipmentStateError,
}

#[derive(Debug, Default)]
pub struct LenientEquipmentLoad {
    pub equipment: EquipmentExceptions,
    pub warnings: Vec<EquipmentLoadWarning>,
}

#[derive(Debug)]
pub struct EquipmentMutationOutcome<T> {
    pub value: T,
    /// Set only when the atomic rename published the new state but the final
    /// directory durability check failed. Callers must treat the mutation as
    /// committed to avoid replaying additive operations.
    pub durability_warning: Option<EquipmentStateError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloaterResetOutcome {
    /// C# and the native reply describe the slots that were present when the
    /// reset item was used.
    pub before: TuneExcRecord,
    /// Authoritative post-reset state persisted by the server and used for
    /// subsequent race physics.
    pub after: TuneExcRecord,
}

impl<T> EquipmentMutationOutcome<T> {
    fn new(value: T, durability_warning: Option<EquipmentStateError>) -> Self {
        Self {
            value,
            durability_warning,
        }
    }
}

fn load_optional_component<T, F>(
    sidecar: EquipmentSidecar,
    warnings: &mut Vec<EquipmentLoadWarning>,
    load: F,
) -> T
where
    T: Default,
    F: FnOnce() -> Result<T, EquipmentStateError>,
{
    match load() {
        Ok(value) => value,
        Err(error) => {
            warnings.push(EquipmentLoadWarning { sidecar, error });
            T::default()
        }
    }
}

impl EquipmentExceptions {
    /// Loads `PlantData.json` from the rider directory and the reference
    /// server's global `PartsData.xml` from the profile root.
    ///
    /// Missing sidecars represent an unequipped profile and are not errors.
    pub fn load(
        profile_root: impl AsRef<Path>,
        rider_directory: impl AsRef<Path>,
    ) -> Result<Self, EquipmentStateError> {
        let profile_root = profile_root.as_ref();
        let rider_directory = rider_directory.as_ref();
        let tune_path = rider_directory.join("TuneData.json");
        let plant_path = rider_directory.join("PlantData.json");
        let level_path = rider_directory.join("LevelData.json");
        let parts_path = profile_root.join("PartsData.xml");
        let user_parts_path = rider_directory.join("PartsData.json");
        Ok(Self {
            tune: load_tune_records(&tune_path)?,
            plant: load_plant_records(&plant_path)?,
            kart_level: load_level_records(&level_path)?,
            parts: load_merged_parts_records(&parts_path, &user_parts_path)?,
        })
    }

    /// Applies one validated P5136 plant-part selection and atomically
    /// publishes the C#-compatible `PlantData.json` sidecar.
    pub fn equip_plant_part(
        rider_directory: impl AsRef<Path>,
        request: PlantPartEquipRequest,
    ) -> Result<PlantExcRecord, EquipmentStateError> {
        validate_plant_request(request)?;
        let rider_directory = rider_directory.as_ref();
        fs::create_dir_all(rider_directory).map_err(|source| EquipmentStateError::Io {
            operation: "create rider equipment directory",
            path: rider_directory.to_owned(),
            source,
        })?;
        let path = rider_directory.join("PlantData.json");
        let mut states = load_plant_states(&path)?;
        let serial = normalize_kart_serial(request.kart_id, request.kart_serial);

        let state = if let Some(state) = states
            .iter_mut()
            .find(|state| state.id == request.kart_id && state.serial == serial)
        {
            state
        } else {
            if states.len() >= MAX_EQUIPMENT_RECORDS {
                return Err(EquipmentStateError::TooManyRecords {
                    kind: "plant",
                    maximum: MAX_EQUIPMENT_RECORDS,
                });
            }
            states.push(PlantState {
                id: request.kart_id,
                serial,
                ..PlantState::default()
            });
            states
                .last_mut()
                .expect("a state was appended immediately before lookup")
        };
        state.set_part(request.item_category, request.item_id);
        let result = state.as_exception();
        write_plant_states(&path, &states)?;
        Ok(result)
    }

    /// Applies one validated P5136 X-parts selection and atomically publishes
    /// the rider-specific C# `PartsData.json` sidecar.
    pub fn equip_x_part(
        rider_directory: impl AsRef<Path>,
        request: XPartEquipRequest,
    ) -> Result<PartsExcRecord, EquipmentStateError> {
        validate_x_part_request(request)?;
        let rider_directory = rider_directory.as_ref();
        fs::create_dir_all(rider_directory).map_err(|source| EquipmentStateError::Io {
            operation: "create rider equipment directory",
            path: rider_directory.to_owned(),
            source,
        })?;
        let path = rider_directory.join("PartsData.json");
        let mut states = load_user_parts_states(&path)?;
        let serial = normalize_kart_serial(request.kart_id, request.kart_serial);
        let state = if let Some(state) = states
            .iter_mut()
            .find(|state| state.id == request.kart_id && state.serial == serial)
        {
            state
        } else {
            if states.len() >= MAX_EQUIPMENT_RECORDS {
                return Err(EquipmentStateError::TooManyRecords {
                    kind: "parts",
                    maximum: MAX_EQUIPMENT_RECORDS,
                });
            }
            states.push(PartsState {
                id: request.kart_id,
                serial,
                ..PartsState::default()
            });
            states
                .last_mut()
                .expect("a state was appended immediately before lookup")
        };
        state.set_part(request);
        let result = state.as_exception();
        write_equipment_states(&path, ".PartsData", &states)?;
        Ok(result)
    }

    pub(crate) fn load_from_capabilities(
        profile_root: &CapabilityDir,
        rider_directory: &CapabilityDir,
        profile_root_path: &Path,
        rider_directory_path: &Path,
    ) -> Result<Self, EquipmentStateError> {
        let plant_path = rider_directory_path.join("PlantData.json");
        let tune_path = rider_directory_path.join("TuneData.json");
        let level_path = rider_directory_path.join("LevelData.json");
        let parts_path = profile_root_path.join("PartsData.xml");
        let user_parts_path = rider_directory_path.join("PartsData.json");
        let tune = parse_tune_states(
            &tune_path,
            read_optional_bounded_capability(rider_directory, "TuneData.json", &tune_path)?,
        )?
        .iter()
        .map(TuneState::as_exception)
        .collect();
        let plant = parse_plant_states(
            &plant_path,
            read_optional_bounded_capability(rider_directory, "PlantData.json", &plant_path)?,
        )?
        .iter()
        .map(PlantState::as_exception)
        .collect();
        let kart_level = parse_level_states(
            &level_path,
            read_optional_bounded_capability(rider_directory, "LevelData.json", &level_path)?,
        )?
        .iter()
        .map(LevelState::as_exception)
        .collect();
        let parts = merge_parts_records(
            parse_parts_records(
                &parts_path,
                read_optional_bounded_capability(profile_root, "PartsData.xml", &parts_path)?,
            )?,
            parse_user_parts_states(
                &user_parts_path,
                read_optional_bounded_capability(
                    rider_directory,
                    "PartsData.json",
                    &user_parts_path,
                )?,
            )?,
        )?;
        Ok(Self {
            tune,
            plant,
            kart_level,
            parts,
        })
    }

    pub(crate) fn load_lenient_from_capabilities(
        profile_root: &CapabilityDir,
        rider_directory: &CapabilityDir,
        profile_root_path: &Path,
        rider_directory_path: &Path,
    ) -> LenientEquipmentLoad {
        let tune_path = rider_directory_path.join("TuneData.json");
        let plant_path = rider_directory_path.join("PlantData.json");
        let level_path = rider_directory_path.join("LevelData.json");
        let parts_path = profile_root_path.join("PartsData.xml");
        let user_parts_path = rider_directory_path.join("PartsData.json");
        let mut warnings = Vec::new();

        let tune = load_optional_component(EquipmentSidecar::Tune, &mut warnings, || {
            parse_tune_states(
                &tune_path,
                read_optional_bounded_capability(rider_directory, "TuneData.json", &tune_path)?,
            )
            .map(|states| states.iter().map(TuneState::as_exception).collect())
        });
        let plant = load_optional_component(EquipmentSidecar::Plant, &mut warnings, || {
            parse_plant_states(
                &plant_path,
                read_optional_bounded_capability(rider_directory, "PlantData.json", &plant_path)?,
            )
            .map(|states| states.iter().map(PlantState::as_exception).collect())
        });
        let kart_level =
            load_optional_component(EquipmentSidecar::KartLevel, &mut warnings, || {
                parse_level_states(
                    &level_path,
                    read_optional_bounded_capability(
                        rider_directory,
                        "LevelData.json",
                        &level_path,
                    )?,
                )
                .map(|states| states.iter().map(LevelState::as_exception).collect())
            });
        let global_parts =
            load_optional_component(EquipmentSidecar::GlobalParts, &mut warnings, || {
                parse_parts_records(
                    &parts_path,
                    read_optional_bounded_capability(profile_root, "PartsData.xml", &parts_path)?,
                )
            });
        let rider_parts =
            load_optional_component(EquipmentSidecar::RiderParts, &mut warnings, || {
                parse_user_parts_states(
                    &user_parts_path,
                    read_optional_bounded_capability(
                        rider_directory,
                        "PartsData.json",
                        &user_parts_path,
                    )?,
                )
            });
        let parts = load_optional_component(EquipmentSidecar::MergedParts, &mut warnings, || {
            merge_parts_records(global_parts, rider_parts)
        });

        LenientEquipmentLoad {
            equipment: Self {
                tune,
                plant,
                kart_level,
                parts,
            },
            warnings,
        }
    }

    pub(crate) fn equip_plant_part_capability(
        rider_directory: &CapabilityDir,
        rider_directory_path: &Path,
        request: PlantPartEquipRequest,
    ) -> Result<EquipmentMutationOutcome<PlantExcRecord>, EquipmentStateError> {
        validate_plant_request(request)?;
        let path = rider_directory_path.join("PlantData.json");
        let mut states = parse_plant_states(
            &path,
            read_optional_bounded_capability(rider_directory, "PlantData.json", &path)?,
        )?;
        let serial = normalize_kart_serial(request.kart_id, request.kart_serial);
        let state = find_or_insert_plant_state(&mut states, request.kart_id, serial)?;
        state.set_part(request.item_category, request.item_id);
        let result = state.as_exception();
        let durability_warning = write_equipment_states_capability(
            rider_directory,
            &path,
            "PlantData.json",
            ".PlantData",
            &states,
        )?;
        Ok(EquipmentMutationOutcome::new(result, durability_warning))
    }

    /// Creates the per-kart Floater record. Repeating the request is
    /// idempotent and never erases already rolled slots.
    pub(crate) fn activate_floater_socket_capability(
        rider_directory: &CapabilityDir,
        rider_directory_path: &Path,
        kart_id: i16,
        kart_serial: i16,
    ) -> Result<EquipmentMutationOutcome<TuneExcRecord>, EquipmentStateError> {
        mutate_tune_state_capability(
            rider_directory,
            rider_directory_path,
            kart_id,
            kart_serial,
            true,
            |_state| Ok(()),
        )
    }

    /// Applies an activation kit without consuming it. Empty tune slots are
    /// filled with distinct codes from the C# pool; selector 5 installs the
    /// fixed Black-H triple.
    pub(crate) fn apply_floater_tune_capability(
        rider_directory: &CapabilityDir,
        rider_directory_path: &Path,
        kart_id: i16,
        kart_serial: i16,
        selector: i16,
    ) -> Result<EquipmentMutationOutcome<TuneExcRecord>, EquipmentStateError> {
        if !matches!(selector, 4..=6) {
            return Err(EquipmentStateError::InvalidFloaterSelector(selector));
        }
        mutate_tune_state_capability(
            rider_directory,
            rider_directory_path,
            kart_id,
            kart_serial,
            false,
            move |state| {
                if selector == 5 {
                    [state.tune1, state.tune2, state.tune3] = BLACK_FLOATER_CODES;
                    return Ok(());
                }
                if [state.tune1, state.tune2, state.tune3]
                    .into_iter()
                    .all(|code| code != 0)
                {
                    return Err(EquipmentStateError::FloaterSlotsFull);
                }
                let mut available = floater_code_pool(selector)
                    .ok_or(EquipmentStateError::InvalidFloaterSelector(selector))?;
                let used = [state.tune1, state.tune2, state.tune3];
                available.retain(|code| !used.contains(code));
                available.shuffle(&mut rand::rng());
                for slot in [&mut state.tune1, &mut state.tune2, &mut state.tune3] {
                    if *slot == 0 {
                        *slot = available
                            .pop()
                            .ok_or(EquipmentStateError::FloaterCodePoolExhausted { selector })?;
                    }
                }
                Ok(())
            },
        )
    }

    pub(crate) fn protect_floater_slot_capability(
        rider_directory: &CapabilityDir,
        rider_directory_path: &Path,
        kart_id: i16,
        kart_serial: i16,
        protect_kind: i16,
        slot: i16,
    ) -> Result<EquipmentMutationOutcome<TuneExcRecord>, EquipmentStateError> {
        if !(0..=2).contains(&slot) {
            return Err(EquipmentStateError::InvalidFloaterProtectionSlot(slot));
        }
        mutate_tune_state_capability(
            rider_directory,
            rider_directory_path,
            kart_id,
            kart_serial,
            false,
            move |state| {
                if [state.tune1, state.tune2, state.tune3]
                    .get(usize::try_from(slot).expect("validated non-negative Floater slot"))
                    .copied()
                    .unwrap_or_default()
                    == 0
                {
                    return Err(EquipmentStateError::EmptyFloaterProtectionSlot(slot));
                }
                if (state.count1 > 0 && state.slot1 == slot)
                    || (state.count2 > 0 && state.slot2 == slot)
                {
                    return Err(EquipmentStateError::DuplicateFloaterProtectionSlot(slot));
                }
                match protect_kind {
                    49 => {
                        state.slot1 = slot;
                        state.count1 = 4;
                    }
                    53 => {
                        state.slot2 = slot;
                        state.count2 = 3;
                    }
                    _ => {
                        return Err(EquipmentStateError::InvalidFloaterProtectionKind(
                            protect_kind,
                        ));
                    }
                }
                Ok(())
            },
        )
    }

    pub(crate) fn reset_floater_socket_capability(
        rider_directory: &CapabilityDir,
        rider_directory_path: &Path,
        kart_id: i16,
        kart_serial: i16,
    ) -> Result<EquipmentMutationOutcome<FloaterResetOutcome>, EquipmentStateError> {
        let mut before = None;
        let outcome = mutate_tune_state_capability(
            rider_directory,
            rider_directory_path,
            kart_id,
            kart_serial,
            false,
            |state| {
                before = Some(state.as_exception());
                let mut protected = [false; 3];
                if state.count1 == 0 {
                    state.slot1 = -1;
                } else {
                    consume_floater_protection(state.slot1, &mut state.count1, &mut protected);
                }
                if state.count2 == 0 {
                    state.slot2 = -1;
                } else {
                    consume_floater_protection(state.slot2, &mut state.count2, &mut protected);
                }
                for (index, slot) in [&mut state.tune1, &mut state.tune2, &mut state.tune3]
                    .into_iter()
                    .enumerate()
                {
                    if !protected[index] {
                        *slot = 0;
                    }
                }
                Ok(())
            },
        )?;
        Ok(EquipmentMutationOutcome::new(
            FloaterResetOutcome {
                before: before.expect("a required Floater state was visited before mutation"),
                after: outcome.value,
            },
            outcome.durability_warning,
        ))
    }

    /// Marks a legacy kart as grade five with all 35 points available.
    /// An already upgraded state retains its allocation. A stale pre-upgrade
    /// placeholder is normalized to the same fresh grade-five state as a new
    /// record.
    pub(crate) fn upgrade_kart_level_capability(
        rider_directory: &CapabilityDir,
        rider_directory_path: &Path,
        kart_id: i16,
        kart_serial: i16,
    ) -> Result<EquipmentMutationOutcome<KartLevelExcRecord>, EquipmentStateError> {
        mutate_level_state_capability(
            rider_directory,
            rider_directory_path,
            kart_id,
            kart_serial,
            |state, existed| {
                if !existed || state.grade != 5 {
                    state.grade = 5;
                    state.points = 35;
                    state.level1 = 0;
                    state.level2 = 0;
                    state.level3 = 0;
                    state.level4 = 0;
                } else {
                    let allocated = [state.level1, state.level2, state.level3, state.level4]
                        .into_iter()
                        .try_fold(0_i16, |total, value| {
                            total
                                .checked_add(value)
                                .ok_or(EquipmentStateError::KartLevelPointOverflow)
                        })?;
                    state.points = 35_i16
                        .checked_sub(allocated)
                        .ok_or(EquipmentStateError::KartLevelPointBudgetExceeded { allocated })?;
                }
                Ok(())
            },
        )
    }

    pub(crate) fn update_kart_level_points_capability(
        rider_directory: &CapabilityDir,
        rider_directory_path: &Path,
        kart_id: i16,
        kart_serial: i16,
        additions: [i16; 4],
    ) -> Result<EquipmentMutationOutcome<KartLevelExcRecord>, EquipmentStateError> {
        mutate_level_state_capability(
            rider_directory,
            rider_directory_path,
            kart_id,
            kart_serial,
            |state, existed| {
                if !existed {
                    state.grade = 5;
                    state.points = 35;
                }
                let current = [state.level1, state.level2, state.level3, state.level4];
                let mut next = [0_i16; 4];
                for index in 0..4 {
                    next[index] = current[index]
                        .checked_add(additions[index])
                        .ok_or(EquipmentStateError::KartLevelPointOverflow)?;
                    if !(0..=10).contains(&next[index]) {
                        return Err(EquipmentStateError::InvalidKartLevelPoint {
                            slot: u8::try_from(index + 1).expect("four kart-level slots fit in u8"),
                            value: next[index],
                        });
                    }
                }
                let allocated = next.into_iter().try_fold(0_i16, |total, value| {
                    total
                        .checked_add(value)
                        .ok_or(EquipmentStateError::KartLevelPointOverflow)
                })?;
                let points = 35_i16
                    .checked_sub(allocated)
                    .ok_or(EquipmentStateError::KartLevelPointBudgetExceeded { allocated })?;
                state.points = points;
                [state.level1, state.level2, state.level3, state.level4] = next;
                Ok(())
            },
        )
    }

    pub(crate) fn clear_kart_level_points_capability(
        rider_directory: &CapabilityDir,
        rider_directory_path: &Path,
        kart_id: i16,
        kart_serial: i16,
    ) -> Result<EquipmentMutationOutcome<KartLevelExcRecord>, EquipmentStateError> {
        mutate_level_state_capability(
            rider_directory,
            rider_directory_path,
            kart_id,
            kart_serial,
            |state, _| {
                state.grade = 5;
                state.points = 35;
                state.level1 = 0;
                state.level2 = 0;
                state.level3 = 0;
                state.level4 = 0;
                state.effect = 0;
                Ok(())
            },
        )
    }

    pub(crate) fn update_kart_level_effect_capability(
        rider_directory: &CapabilityDir,
        rider_directory_path: &Path,
        kart_id: i16,
        kart_serial: i16,
        effect: i16,
    ) -> Result<EquipmentMutationOutcome<KartLevelExcRecord>, EquipmentStateError> {
        mutate_level_state_capability(
            rider_directory,
            rider_directory_path,
            kart_id,
            kart_serial,
            |state, existed| {
                if !existed {
                    state.grade = 5;
                    state.points = 35;
                }
                state.effect = effect;
                Ok(())
            },
        )
    }

    pub(crate) fn equip_x_part_capability(
        rider_directory: &CapabilityDir,
        rider_directory_path: &Path,
        request: XPartEquipRequest,
    ) -> Result<EquipmentMutationOutcome<PartsExcRecord>, EquipmentStateError> {
        validate_x_part_request(request)?;
        let path = rider_directory_path.join("PartsData.json");
        let mut states = parse_user_parts_states(
            &path,
            read_optional_bounded_capability(rider_directory, "PartsData.json", &path)?,
        )?;
        let serial = normalize_kart_serial(request.kart_id, request.kart_serial);
        let state = if let Some(state) = states
            .iter_mut()
            .find(|state| state.id == request.kart_id && state.serial == serial)
        {
            state
        } else {
            if states.len() >= MAX_EQUIPMENT_RECORDS {
                return Err(EquipmentStateError::TooManyRecords {
                    kind: "parts",
                    maximum: MAX_EQUIPMENT_RECORDS,
                });
            }
            states.push(PartsState {
                id: request.kart_id,
                serial,
                ..PartsState::default()
            });
            states
                .last_mut()
                .expect("a state was appended immediately before lookup")
        };
        state.set_part(request);
        let result = state.as_exception();
        let durability_warning = write_equipment_states_capability(
            rider_directory,
            &path,
            "PartsData.json",
            ".PartsData",
            &states,
        )?;
        Ok(EquipmentMutationOutcome::new(result, durability_warning))
    }
}

#[derive(Debug, Error)]
pub enum EquipmentStateError {
    #[error("failed to {operation} equipment state file {path}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("equipment state file {path} has {actual} bytes; maximum is {maximum}")]
    TooLarge {
        path: PathBuf,
        actual: u64,
        maximum: u64,
    },

    #[error("equipment state entry at {path} is not a regular non-symbolic-link file")]
    InvalidStorageEntry { path: PathBuf },

    #[error("plant state JSON at {path} is invalid")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("parts state XML at {path} is invalid")]
    Xml {
        path: PathBuf,
        #[source]
        source: quick_xml::Error,
    },

    #[error("parts state XML at {path} contains a prohibited document type declaration")]
    DocumentType { path: PathBuf },

    #[error("parts state XML at {path} has a missing or invalid {attribute} attribute")]
    InvalidPartsAttribute {
        path: PathBuf,
        attribute: &'static str,
    },

    #[error("{kind} equipment state has more than {maximum} records")]
    TooManyRecords { kind: &'static str, maximum: usize },

    #[error("plant part category {0} is outside 43..=46")]
    InvalidPlantPartCategory(i16),

    #[error("plant part item ID {0} cannot be negative")]
    InvalidPlantPartItem(i16),

    #[error("plant target kart ID {0} must be positive")]
    InvalidPlantKart(i16),

    #[error("X-parts category {0} is not one of 63..=66, 68, or 69")]
    InvalidXPartCategory(i16),

    #[error("X-parts item ID {0} cannot be negative")]
    InvalidXPartItem(i16),

    #[error("X-parts target kart ID {0} must be positive")]
    InvalidXPartKart(i16),

    #[error("kart-level target kart ID {0} must be positive")]
    InvalidKartLevelKart(i16),

    #[error("kart-level slot {slot} value {value} is outside 0..=10")]
    InvalidKartLevelPoint { slot: u8, value: i16 },

    #[error("kart-level allocation {allocated} exceeds the 35-point budget")]
    KartLevelPointBudgetExceeded { allocated: i16 },

    #[error("kart-level remaining point value {0} is outside 0..=35")]
    InvalidKartLevelRemainingPoints(i16),

    #[error("kart-level point arithmetic overflowed i16")]
    KartLevelPointOverflow,

    #[error("Floater target kart ID {0} must be positive")]
    InvalidFloaterKart(i16),

    #[error("Floater activation-kit selector {0} must be positive")]
    InvalidFloaterSelector(i16),

    #[error("Floater tune code {0} is not known to the P5136 client")]
    InvalidFloaterCode(i16),

    #[error("Floater tune code {0} occurs in more than one slot")]
    DuplicateFloaterCode(i16),

    #[error("Floater protection kind {0} is not category 49 or 53")]
    InvalidFloaterProtectionKind(i16),

    #[error("Floater protection slot {0} is outside -1..=2")]
    InvalidFloaterProtectionSlot(i16),

    #[error("Floater protection count {0} is outside 0..=4")]
    InvalidFloaterProtectionCount(i16),

    #[error("Floater state does not exist for kart {kart_id} serial {kart_serial}")]
    MissingFloaterState { kart_id: i16, kart_serial: i16 },

    #[error("Floater code pool for selector {selector} cannot fill every empty slot")]
    FloaterCodePoolExhausted { selector: i16 },

    #[error("all Floater tune slots are already occupied")]
    FloaterSlotsFull,

    #[error("Floater tune slot {0} is empty and cannot be protected")]
    EmptyFloaterProtectionSlot(i16),

    #[error("Floater tune slot {0} is already protected")]
    DuplicateFloaterProtectionSlot(i16),
}

#[derive(Debug, Error)]
pub enum EquipmentProfileError {
    #[error(transparent)]
    Store(#[from] ProfileStoreError),

    #[error(transparent)]
    State(#[from] EquipmentStateError),
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
struct TuneState {
    #[serde(rename = "ID")]
    id: i16,
    #[serde(rename = "SN")]
    serial: i16,
    #[serde(rename = "Tune1")]
    tune1: i16,
    #[serde(rename = "Tune2")]
    tune2: i16,
    #[serde(rename = "Tune3")]
    tune3: i16,
    #[serde(rename = "Slot1")]
    slot1: i16,
    #[serde(rename = "Count1")]
    count1: i16,
    #[serde(rename = "Slot2")]
    slot2: i16,
    #[serde(rename = "Count2")]
    count2: i16,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

impl TuneState {
    fn new(id: i16, serial: i16) -> Self {
        Self {
            id,
            serial,
            slot1: -1,
            slot2: -1,
            ..Self::default()
        }
    }

    fn as_exception(&self) -> TuneExcRecord {
        TuneExcRecord {
            id: self.id,
            serial: normalize_kart_serial(self.id, self.serial),
            tune1: self.tune1,
            tune2: self.tune2,
            tune3: self.tune3,
            slot1: self.slot1,
            count1: self.count1,
            slot2: self.slot2,
            count2: self.count2,
        }
    }
}

fn validate_tune_state(state: &TuneState) -> Result<(), EquipmentStateError> {
    if state.id <= 0 {
        return Err(EquipmentStateError::InvalidFloaterKart(state.id));
    }
    let mut seen = Vec::with_capacity(3);
    for code in [state.tune1, state.tune2, state.tune3] {
        if !is_known_floater_code(code) {
            return Err(EquipmentStateError::InvalidFloaterCode(code));
        }
        if code != 0 {
            if seen.contains(&code) {
                return Err(EquipmentStateError::DuplicateFloaterCode(code));
            }
            seen.push(code);
        }
    }
    for (slot, count, maximum) in [
        (state.slot1, state.count1, 4),
        (state.slot2, state.count2, 3),
    ] {
        if !(-1..=2).contains(&slot) {
            return Err(EquipmentStateError::InvalidFloaterProtectionSlot(slot));
        }
        if !(0..=maximum).contains(&count) {
            return Err(EquipmentStateError::InvalidFloaterProtectionCount(count));
        }
        if slot == -1 && count != 0 {
            return Err(EquipmentStateError::InvalidFloaterProtectionCount(count));
        }
    }
    if state.count1 > 0 && state.count2 > 0 && state.slot1 == state.slot2 {
        return Err(EquipmentStateError::DuplicateFloaterProtectionSlot(
            state.slot1,
        ));
    }
    Ok(())
}

fn mutate_tune_state_capability<F>(
    rider_directory: &CapabilityDir,
    rider_directory_path: &Path,
    kart_id: i16,
    kart_serial: i16,
    insert_if_missing: bool,
    mutation: F,
) -> Result<EquipmentMutationOutcome<TuneExcRecord>, EquipmentStateError>
where
    F: FnOnce(&mut TuneState) -> Result<(), EquipmentStateError>,
{
    if kart_id <= 0 {
        return Err(EquipmentStateError::InvalidFloaterKart(kart_id));
    }
    let serial = normalize_kart_serial(kart_id, kart_serial);
    let path = rider_directory_path.join("TuneData.json");
    let mut states = parse_tune_states(
        &path,
        read_optional_bounded_capability(rider_directory, "TuneData.json", &path)?,
    )?;
    let index = states
        .iter()
        .position(|state| state.id == kart_id && state.serial == serial);
    let index = match index {
        Some(index) => index,
        None if insert_if_missing => {
            if states.len() >= MAX_EQUIPMENT_RECORDS {
                return Err(EquipmentStateError::TooManyRecords {
                    kind: "Floater",
                    maximum: MAX_EQUIPMENT_RECORDS,
                });
            }
            states.push(TuneState::new(kart_id, serial));
            states.len() - 1
        }
        None => {
            return Err(EquipmentStateError::MissingFloaterState {
                kart_id,
                kart_serial: serial,
            });
        }
    };
    mutation(&mut states[index])?;
    validate_tune_state(&states[index])?;
    let result = states[index].as_exception();
    let durability_warning = write_equipment_states_capability(
        rider_directory,
        &path,
        "TuneData.json",
        ".TuneData",
        &states,
    )?;
    Ok(EquipmentMutationOutcome::new(result, durability_warning))
}

fn consume_floater_protection(slot: i16, count: &mut i16, protected: &mut [bool; 3]) {
    if *count <= 0 {
        return;
    }
    *count -= 1;
    if let Ok(index) = usize::try_from(slot)
        && let Some(protected) = protected.get_mut(index)
    {
        *protected = true;
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
struct PlantState {
    #[serde(rename = "ID")]
    id: i16,
    #[serde(rename = "SN")]
    serial: i16,
    #[serde(rename = "Engine")]
    engine_category: i16,
    #[serde(rename = "EngineID")]
    engine_id: i16,
    #[serde(rename = "Handle")]
    handle_category: i16,
    #[serde(rename = "HandleID")]
    handle_id: i16,
    #[serde(rename = "Wheel")]
    wheel_category: i16,
    #[serde(rename = "WheelID")]
    wheel_id: i16,
    #[serde(rename = "Kit")]
    kit_category: i16,
    #[serde(rename = "KitID")]
    kit_id: i16,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

impl PlantState {
    fn set_part(&mut self, category: i16, item_id: i16) {
        match category {
            43 => {
                self.engine_category = category;
                self.engine_id = item_id;
            }
            44 => {
                self.handle_category = category;
                self.handle_id = item_id;
            }
            45 => {
                self.wheel_category = category;
                self.wheel_id = item_id;
            }
            46 => {
                self.kit_category = category;
                self.kit_id = item_id;
            }
            _ => unreachable!("plant request validation precedes mutation"),
        }
    }

    fn as_exception(&self) -> PlantExcRecord {
        PlantExcRecord {
            id: self.id,
            serial: normalize_kart_serial(self.id, self.serial),
            engine_category: self.engine_category,
            engine_id: self.engine_id,
            handle_category: self.handle_category,
            handle_id: self.handle_id,
            wheel_category: self.wheel_category,
            wheel_id: self.wheel_id,
            kit_category: self.kit_category,
            kit_id: self.kit_id,
        }
    }
}

fn find_or_insert_plant_state(
    states: &mut Vec<PlantState>,
    id: i16,
    serial: i16,
) -> Result<&mut PlantState, EquipmentStateError> {
    if let Some(index) = states
        .iter()
        .position(|state| state.id == id && state.serial == serial)
    {
        return Ok(&mut states[index]);
    }
    if states.len() >= MAX_EQUIPMENT_RECORDS {
        return Err(EquipmentStateError::TooManyRecords {
            kind: "plant",
            maximum: MAX_EQUIPMENT_RECORDS,
        });
    }
    states.push(PlantState {
        id,
        serial,
        ..PlantState::default()
    });
    Ok(states
        .last_mut()
        .expect("a plant state was appended immediately before lookup"))
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
struct LevelState {
    #[serde(rename = "ID")]
    id: i16,
    #[serde(rename = "SN")]
    serial: i16,
    #[serde(rename = "Grade")]
    grade: i16,
    #[serde(rename = "Points")]
    points: i16,
    #[serde(rename = "Level1")]
    level1: i16,
    #[serde(rename = "Level2")]
    level2: i16,
    #[serde(rename = "Level3")]
    level3: i16,
    #[serde(rename = "Level4")]
    level4: i16,
    #[serde(rename = "Effect")]
    effect: i16,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

impl LevelState {
    fn as_exception(&self) -> KartLevelExcRecord {
        KartLevelExcRecord {
            id: self.id,
            serial: normalize_kart_serial(self.id, self.serial),
            grade: self.grade,
            points: self.points,
            level1: self.level1,
            level2: self.level2,
            level3: self.level3,
            level4: self.level4,
            effect: self.effect,
        }
    }
}

fn mutate_level_state_capability<F>(
    rider_directory: &CapabilityDir,
    rider_directory_path: &Path,
    kart_id: i16,
    kart_serial: i16,
    mutation: F,
) -> Result<EquipmentMutationOutcome<KartLevelExcRecord>, EquipmentStateError>
where
    F: FnOnce(&mut LevelState, bool) -> Result<(), EquipmentStateError>,
{
    if kart_id <= 0 {
        return Err(EquipmentStateError::InvalidKartLevelKart(kart_id));
    }
    let serial = normalize_kart_serial(kart_id, kart_serial);
    let path = rider_directory_path.join("LevelData.json");
    let mut states = parse_level_states(
        &path,
        read_optional_bounded_capability(rider_directory, "LevelData.json", &path)?,
    )?;
    let index = states
        .iter()
        .position(|state| state.id == kart_id && state.serial == serial);
    let existed = index.is_some();
    let index = if let Some(index) = index {
        index
    } else {
        if states.len() >= MAX_EQUIPMENT_RECORDS {
            return Err(EquipmentStateError::TooManyRecords {
                kind: "kart level",
                maximum: MAX_EQUIPMENT_RECORDS,
            });
        }
        states.push(LevelState {
            id: kart_id,
            serial,
            ..LevelState::default()
        });
        states.len() - 1
    };
    mutation(&mut states[index], existed)?;
    let result = states[index].as_exception();
    let durability_warning = write_equipment_states_capability(
        rider_directory,
        &path,
        "LevelData.json",
        ".LevelData",
        &states,
    )?;
    Ok(EquipmentMutationOutcome::new(result, durability_warning))
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct PartsState {
    #[serde(rename = "ID")]
    id: i16,
    #[serde(rename = "SN")]
    serial: i16,
    engine: i16,
    engine_grade: u8,
    engine_value: i16,
    handle: i16,
    handle_grade: u8,
    handle_value: i16,
    wheel: i16,
    wheel_grade: u8,
    wheel_value: i16,
    booster: i16,
    booster_grade: u8,
    booster_value: i16,
    coating: i16,
    tail_lamp: i16,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

impl PartsState {
    fn set_part(&mut self, request: XPartEquipRequest) {
        match request.item_category {
            63 => {
                self.engine = request.item_id;
                self.engine_grade = request.grade;
                self.engine_value = request.parts_value;
            }
            64 => {
                self.handle = request.item_id;
                self.handle_grade = request.grade;
                self.handle_value = request.parts_value;
            }
            65 => {
                self.wheel = request.item_id;
                self.wheel_grade = request.grade;
                self.wheel_value = request.parts_value;
            }
            66 => {
                self.booster = request.item_id;
                self.booster_grade = request.grade;
                self.booster_value = request.parts_value;
            }
            68 => self.coating = request.item_id,
            69 => self.tail_lamp = request.item_id,
            _ => unreachable!("X-parts request validation precedes mutation"),
        }
    }

    fn as_exception(&self) -> PartsExcRecord {
        PartsExcRecord {
            id: self.id,
            serial: normalize_kart_serial(self.id, self.serial),
            engine: self.engine,
            engine_grade: self.engine_grade,
            engine_value: self.engine_value,
            handle: self.handle,
            handle_grade: self.handle_grade,
            handle_value: self.handle_value,
            wheel: self.wheel,
            wheel_grade: self.wheel_grade,
            wheel_value: self.wheel_value,
            booster: self.booster,
            booster_grade: self.booster_grade,
            booster_value: self.booster_value,
            coating: self.coating,
            tail_lamp: self.tail_lamp,
        }
    }
}

struct TuneStatesSeed<'a> {
    limit_exceeded: &'a Cell<bool>,
}

impl<'de> DeserializeSeed<'de> for TuneStatesSeed<'_> {
    type Value = Vec<TuneState>;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(TuneStatesVisitor {
            limit_exceeded: self.limit_exceeded,
        })
    }
}

struct TuneStatesVisitor<'a> {
    limit_exceeded: &'a Cell<bool>,
}

impl<'de> Visitor<'de> for TuneStatesVisitor<'_> {
    type Value = Vec<TuneState>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an array of P5136 Floater state records")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let capacity = sequence.size_hint().unwrap_or(0).min(MAX_EQUIPMENT_RECORDS);
        let mut states = Vec::with_capacity(capacity);
        while states.len() < MAX_EQUIPMENT_RECORDS {
            let Some(state) = sequence.next_element::<TuneState>()? else {
                return Ok(states);
            };
            states.push(state);
        }
        let extra = sequence.next_element_seed(RejectAdditionalTuneRecord {
            limit_exceeded: self.limit_exceeded,
        })?;
        debug_assert!(extra.is_none());
        Ok(states)
    }
}

struct RejectAdditionalTuneRecord<'a> {
    limit_exceeded: &'a Cell<bool>,
}

impl<'de> DeserializeSeed<'de> for RejectAdditionalTuneRecord<'_> {
    type Value = ();

    fn deserialize<D>(self, _deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        self.limit_exceeded.set(true);
        Err(D::Error::custom("Floater equipment record limit exceeded"))
    }
}

fn load_tune_records(path: &Path) -> Result<Vec<TuneExcRecord>, EquipmentStateError> {
    Ok(parse_tune_states(path, read_optional_bounded(path)?)?
        .iter()
        .map(TuneState::as_exception)
        .collect())
}

fn parse_tune_states(
    path: &Path,
    bytes: Option<Vec<u8>>,
) -> Result<Vec<TuneState>, EquipmentStateError> {
    let Some(bytes) = bytes else {
        return Ok(Vec::new());
    };
    let bytes = bytes
        .strip_prefix(&[0xef, 0xbb, 0xbf])
        .unwrap_or(bytes.as_slice());
    let limit_exceeded = Cell::new(false);
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let states = match (TuneStatesSeed {
        limit_exceeded: &limit_exceeded,
    })
    .deserialize(&mut deserializer)
    {
        Ok(states) => states,
        Err(_source) if limit_exceeded.get() => {
            return Err(EquipmentStateError::TooManyRecords {
                kind: "Floater",
                maximum: MAX_EQUIPMENT_RECORDS,
            });
        }
        Err(source) => {
            return Err(EquipmentStateError::Json {
                path: path.to_owned(),
                source,
            });
        }
    };
    deserializer
        .end()
        .map_err(|source| EquipmentStateError::Json {
            path: path.to_owned(),
            source,
        })?;
    let mut states = states;
    for state in &mut states {
        state.serial = normalize_kart_serial(state.id, state.serial);
    }
    let states = deduplicate_last_by_key(states, |state| (state.id, state.serial));
    for state in &states {
        validate_tune_state(state)?;
    }
    Ok(states)
}

struct PlantStatesSeed<'a> {
    limit_exceeded: &'a Cell<bool>,
}

impl<'de> DeserializeSeed<'de> for PlantStatesSeed<'_> {
    type Value = Vec<PlantState>;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(PlantStatesVisitor {
            limit_exceeded: self.limit_exceeded,
        })
    }
}

struct PlantStatesVisitor<'a> {
    limit_exceeded: &'a Cell<bool>,
}

impl<'de> Visitor<'de> for PlantStatesVisitor<'_> {
    type Value = Vec<PlantState>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an array of P5136 plant state records")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let capacity = sequence.size_hint().unwrap_or(0).min(MAX_EQUIPMENT_RECORDS);
        let mut states = Vec::with_capacity(capacity);
        while states.len() < MAX_EQUIPMENT_RECORDS {
            let Some(state) = sequence.next_element::<PlantState>()? else {
                return Ok(states);
            };
            states.push(state);
        }

        let extra = sequence.next_element_seed(RejectAdditionalPlantRecord {
            limit_exceeded: self.limit_exceeded,
        })?;
        debug_assert!(extra.is_none());
        Ok(states)
    }
}

struct RejectAdditionalPlantRecord<'a> {
    limit_exceeded: &'a Cell<bool>,
}

impl<'de> DeserializeSeed<'de> for RejectAdditionalPlantRecord<'_> {
    type Value = ();

    fn deserialize<D>(self, _deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        self.limit_exceeded.set(true);
        Err(D::Error::custom("plant equipment record limit exceeded"))
    }
}

fn load_plant_records(path: &Path) -> Result<Vec<PlantExcRecord>, EquipmentStateError> {
    Ok(load_plant_states(path)?
        .iter()
        .map(PlantState::as_exception)
        .collect())
}

fn load_plant_states(path: &Path) -> Result<Vec<PlantState>, EquipmentStateError> {
    parse_plant_states(path, read_optional_bounded(path)?)
}

fn parse_plant_states(
    path: &Path,
    bytes: Option<Vec<u8>>,
) -> Result<Vec<PlantState>, EquipmentStateError> {
    let Some(bytes) = bytes else {
        return Ok(Vec::new());
    };
    let bytes = bytes
        .strip_prefix(&[0xef, 0xbb, 0xbf])
        .unwrap_or(bytes.as_slice());
    let limit_exceeded = Cell::new(false);
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let states = match (PlantStatesSeed {
        limit_exceeded: &limit_exceeded,
    })
    .deserialize(&mut deserializer)
    {
        Ok(states) => states,
        Err(_source) if limit_exceeded.get() => {
            return Err(EquipmentStateError::TooManyRecords {
                kind: "plant",
                maximum: MAX_EQUIPMENT_RECORDS,
            });
        }
        Err(source) => {
            return Err(EquipmentStateError::Json {
                path: path.to_owned(),
                source,
            });
        }
    };
    deserializer
        .end()
        .map_err(|source| EquipmentStateError::Json {
            path: path.to_owned(),
            source,
        })?;
    let mut states = states;
    for state in &mut states {
        state.serial = normalize_kart_serial(state.id, state.serial);
    }
    Ok(deduplicate_last_by_key(states, |state| {
        (state.id, state.serial)
    }))
}

fn deduplicate_last_by_key<T, K, F>(states: Vec<T>, key: F) -> Vec<T>
where
    K: Ord,
    F: Fn(&T) -> K,
{
    let mut indices = BTreeMap::new();
    let mut unique = Vec::with_capacity(states.len());
    for state in states {
        let state_key = key(&state);
        if let Some(index) = indices.get(&state_key).copied() {
            unique[index] = state;
        } else {
            indices.insert(state_key, unique.len());
            unique.push(state);
        }
    }
    unique
}

fn validate_level_state(state: &LevelState) -> Result<(), EquipmentStateError> {
    if state.id <= 0 {
        return Err(EquipmentStateError::InvalidKartLevelKart(state.id));
    }
    if !(0..=35).contains(&state.points) {
        return Err(EquipmentStateError::InvalidKartLevelRemainingPoints(
            state.points,
        ));
    }
    let levels = [state.level1, state.level2, state.level3, state.level4];
    for (index, value) in levels.into_iter().enumerate() {
        if !(0..=10).contains(&value) {
            return Err(EquipmentStateError::InvalidKartLevelPoint {
                slot: u8::try_from(index + 1).expect("four kart-level slots fit in u8"),
                value,
            });
        }
    }
    let allocated = levels.into_iter().try_fold(0_i16, |total, value| {
        total
            .checked_add(value)
            .ok_or(EquipmentStateError::KartLevelPointOverflow)
    })?;
    if allocated > 35 {
        return Err(EquipmentStateError::KartLevelPointBudgetExceeded { allocated });
    }
    Ok(())
}

fn load_level_records(path: &Path) -> Result<Vec<KartLevelExcRecord>, EquipmentStateError> {
    Ok(parse_level_states(path, read_optional_bounded(path)?)?
        .iter()
        .map(LevelState::as_exception)
        .collect())
}

fn parse_level_states(
    path: &Path,
    bytes: Option<Vec<u8>>,
) -> Result<Vec<LevelState>, EquipmentStateError> {
    let Some(bytes) = bytes else {
        return Ok(Vec::new());
    };
    let bytes = bytes
        .strip_prefix(&[0xef, 0xbb, 0xbf])
        .unwrap_or(bytes.as_slice());
    let limit_exceeded = Cell::new(false);
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let states = match (LevelStatesSeed {
        limit_exceeded: &limit_exceeded,
    })
    .deserialize(&mut deserializer)
    {
        Ok(states) => states,
        Err(_source) if limit_exceeded.get() => {
            return Err(EquipmentStateError::TooManyRecords {
                kind: "kart level",
                maximum: MAX_EQUIPMENT_RECORDS,
            });
        }
        Err(source) => {
            return Err(EquipmentStateError::Json {
                path: path.to_owned(),
                source,
            });
        }
    };
    deserializer
        .end()
        .map_err(|source| EquipmentStateError::Json {
            path: path.to_owned(),
            source,
        })?;
    let mut states = states;
    for state in &mut states {
        validate_level_state(state)?;
        state.serial = normalize_kart_serial(state.id, state.serial);
    }
    Ok(deduplicate_last_by_key(states, |state| {
        (state.id, state.serial)
    }))
}

struct LevelStatesSeed<'a> {
    limit_exceeded: &'a Cell<bool>,
}

impl<'de> DeserializeSeed<'de> for LevelStatesSeed<'_> {
    type Value = Vec<LevelState>;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(LevelStatesVisitor {
            limit_exceeded: self.limit_exceeded,
        })
    }
}

struct LevelStatesVisitor<'a> {
    limit_exceeded: &'a Cell<bool>,
}

impl<'de> Visitor<'de> for LevelStatesVisitor<'_> {
    type Value = Vec<LevelState>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an array of P5136 kart-level state records")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let capacity = sequence.size_hint().unwrap_or(0).min(MAX_EQUIPMENT_RECORDS);
        let mut states = Vec::with_capacity(capacity);
        while states.len() < MAX_EQUIPMENT_RECORDS {
            let Some(state) = sequence.next_element::<LevelState>()? else {
                return Ok(states);
            };
            states.push(state);
        }
        let extra = sequence.next_element_seed(RejectAdditionalLevelRecord {
            limit_exceeded: self.limit_exceeded,
        })?;
        debug_assert!(extra.is_none());
        Ok(states)
    }
}

struct RejectAdditionalLevelRecord<'a> {
    limit_exceeded: &'a Cell<bool>,
}

impl<'de> DeserializeSeed<'de> for RejectAdditionalLevelRecord<'_> {
    type Value = ();

    fn deserialize<D>(self, _deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        self.limit_exceeded.set(true);
        Err(D::Error::custom("kart-level record limit exceeded"))
    }
}

fn validate_plant_request(request: PlantPartEquipRequest) -> Result<(), EquipmentStateError> {
    if !(43..=46).contains(&request.item_category) {
        return Err(EquipmentStateError::InvalidPlantPartCategory(
            request.item_category,
        ));
    }
    if request.item_id < 0 {
        return Err(EquipmentStateError::InvalidPlantPartItem(request.item_id));
    }
    if request.kart_id <= 0 {
        return Err(EquipmentStateError::InvalidPlantKart(request.kart_id));
    }
    Ok(())
}

fn validate_x_part_request(request: XPartEquipRequest) -> Result<(), EquipmentStateError> {
    if !matches!(request.item_category, 63..=66 | 68 | 69) {
        return Err(EquipmentStateError::InvalidXPartCategory(
            request.item_category,
        ));
    }
    if request.item_id < 0 {
        return Err(EquipmentStateError::InvalidXPartItem(request.item_id));
    }
    if request.kart_id <= 0 {
        return Err(EquipmentStateError::InvalidXPartKart(request.kart_id));
    }
    Ok(())
}

fn write_plant_states(path: &Path, states: &[PlantState]) -> Result<(), EquipmentStateError> {
    write_equipment_states(path, ".PlantData", states)
}

fn write_equipment_states<T: Serialize>(
    path: &Path,
    temporary_prefix: &str,
    states: &[T],
) -> Result<(), EquipmentStateError> {
    let mut bytes =
        serde_json::to_vec_pretty(states).map_err(|source| EquipmentStateError::Json {
            path: path.to_owned(),
            source,
        })?;
    bytes.push(b'\n');
    let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if length > MAX_EQUIPMENT_STATE_BYTES {
        return Err(EquipmentStateError::TooLarge {
            path: path.to_owned(),
            actual: length,
            maximum: MAX_EQUIPMENT_STATE_BYTES,
        });
    }

    let directory = path
        .parent()
        .expect("PlantData.json always has a rider-directory parent");
    let temporary_path = create_equipment_temporary(directory, temporary_prefix, &bytes)?;
    let publish_result =
        fs::rename(&temporary_path, path).map_err(|source| EquipmentStateError::Io {
            operation: "publish plant equipment state",
            path: path.to_owned(),
            source,
        });
    if publish_result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    publish_result?;
    sync_equipment_directory(directory)
}

fn write_equipment_states_capability<T: Serialize>(
    directory: &CapabilityDir,
    display_path: &Path,
    filename: &str,
    temporary_prefix: &str,
    states: &[T],
) -> Result<Option<EquipmentStateError>, EquipmentStateError> {
    let mut bytes =
        serde_json::to_vec_pretty(states).map_err(|source| EquipmentStateError::Json {
            path: display_path.to_owned(),
            source,
        })?;
    bytes.push(b'\n');
    let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if length > MAX_EQUIPMENT_STATE_BYTES {
        return Err(EquipmentStateError::TooLarge {
            path: display_path.to_owned(),
            actual: length,
            maximum: MAX_EQUIPMENT_STATE_BYTES,
        });
    }

    let temporary_name = loop {
        let sequence = EQUIPMENT_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary_name = format!("{temporary_prefix}.{}.{}.tmp", std::process::id(), sequence);
        let temporary_path = display_path.with_file_name(&temporary_name);
        let mut options = CapabilityOpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No);
        let mut file = match directory.open_with(&temporary_name, &options) {
            Ok(file) => file,
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(EquipmentStateError::Io {
                    operation: "create no-follow equipment temporary",
                    path: temporary_path,
                    source,
                });
            }
        };
        let metadata = file.metadata().map_err(|source| EquipmentStateError::Io {
            operation: "inspect opened equipment temporary",
            path: temporary_path.clone(),
            source,
        })?;
        if !metadata.file_type().is_file() {
            let _ = directory.remove_file(&temporary_name);
            return Err(EquipmentStateError::InvalidStorageEntry {
                path: temporary_path,
            });
        }
        if let Err(source) = file
            .write_all(&bytes)
            .and_then(|()| file.flush())
            .and_then(|()| file.sync_all())
        {
            drop(file);
            let _ = directory.remove_file(&temporary_name);
            return Err(EquipmentStateError::Io {
                operation: "write no-follow equipment temporary",
                path: temporary_path,
                source,
            });
        }
        break temporary_name;
    };

    let publish = directory.rename(&temporary_name, directory, filename);
    if let Err(source) = publish {
        let _ = directory.remove_file(&temporary_name);
        return Err(EquipmentStateError::Io {
            operation: "publish no-follow equipment state",
            path: display_path.to_owned(),
            source,
        });
    }
    #[cfg(test)]
    if FAIL_NEXT_EQUIPMENT_DIRECTORY_SYNC.replace(false) {
        return Ok(Some(EquipmentStateError::Io {
            operation: "sync equipment profile directory capability",
            path: display_path.parent().unwrap_or(display_path).to_owned(),
            source: std::io::Error::other("injected post-publish equipment sync failure"),
        }));
    }
    Ok(sync_equipment_capability_directory(
        directory,
        display_path.parent().unwrap_or(display_path),
    )
    .err())
}

#[cfg(unix)]
fn sync_equipment_capability_directory(
    directory: &CapabilityDir,
    display_path: &Path,
) -> Result<(), EquipmentStateError> {
    directory
        .open(".")
        .and_then(|directory_file| directory_file.sync_all())
        .map_err(|source| EquipmentStateError::Io {
            operation: "sync equipment profile directory capability",
            path: display_path.to_owned(),
            source,
        })
}

#[cfg(not(unix))]
fn sync_equipment_capability_directory(
    directory: &CapabilityDir,
    display_path: &Path,
) -> Result<(), EquipmentStateError> {
    // Windows does not permit opening a directory for `File::sync_all` through
    // the ordinary file API. The temporary file itself was synced before the
    // atomic rename; retain the capability anchor and verify that the published
    // directory still resolves without following an attacker-controlled path.
    directory
        .dir_metadata()
        .map(|_| ())
        .map_err(|source| EquipmentStateError::Io {
            operation: "verify equipment profile directory capability",
            path: display_path.to_owned(),
            source,
        })
}

fn create_equipment_temporary(
    directory: &Path,
    prefix: &str,
    bytes: &[u8],
) -> Result<PathBuf, EquipmentStateError> {
    loop {
        let sequence = EQUIPMENT_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!("{prefix}.{}.{}.tmp", std::process::id(), sequence));
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(EquipmentStateError::Io {
                    operation: "create temporary plant equipment state",
                    path,
                    source,
                });
            }
        };
        let write_result = file
            .write_all(bytes)
            .and_then(|()| file.flush())
            .and_then(|()| file.sync_all());
        if let Err(source) = write_result {
            drop(file);
            let _ = fs::remove_file(&path);
            return Err(EquipmentStateError::Io {
                operation: "write temporary plant equipment state",
                path,
                source,
            });
        }
        return Ok(path);
    }
}

#[cfg(unix)]
fn sync_equipment_directory(directory: &Path) -> Result<(), EquipmentStateError> {
    File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|source| EquipmentStateError::Io {
            operation: "sync plant equipment directory",
            path: directory.to_owned(),
            source,
        })
}

#[cfg(not(unix))]
fn sync_equipment_directory(directory: &Path) -> Result<(), EquipmentStateError> {
    fs::metadata(directory).map_err(|source| EquipmentStateError::Io {
        operation: "verify plant equipment directory",
        path: directory.to_owned(),
        source,
    })?;
    Ok(())
}

fn load_parts_records(path: &Path) -> Result<Vec<PartsExcRecord>, EquipmentStateError> {
    parse_parts_records(path, read_optional_bounded(path)?)
}

fn parse_parts_records(
    path: &Path,
    bytes: Option<Vec<u8>>,
) -> Result<Vec<PartsExcRecord>, EquipmentStateError> {
    let Some(bytes) = bytes else {
        return Ok(Vec::new());
    };
    let mut reader = Reader::from_reader(bytes.as_slice());
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut records = Vec::new();

    loop {
        let event =
            reader
                .read_event_into(&mut buffer)
                .map_err(|source| EquipmentStateError::Xml {
                    path: path.to_owned(),
                    source,
                })?;
        match event {
            Event::Start(element) | Event::Empty(element) if element.name() == QName(b"Kart") => {
                if records.len() >= MAX_EQUIPMENT_RECORDS {
                    return Err(EquipmentStateError::TooManyRecords {
                        kind: "parts",
                        maximum: MAX_EQUIPMENT_RECORDS,
                    });
                }
                records.push(parse_parts_record(&reader, &element, path)?);
            }
            Event::DocType(_) => {
                return Err(EquipmentStateError::DocumentType {
                    path: path.to_owned(),
                });
            }
            Event::Eof => break,
            Event::Start(_)
            | Event::End(_)
            | Event::Empty(_)
            | Event::Decl(_)
            | Event::Text(_)
            | Event::CData(_)
            | Event::Comment(_)
            | Event::PI(_)
            | Event::GeneralRef(_) => {}
        }
        buffer.clear();
    }
    Ok(records)
}

fn load_merged_parts_records(
    global_path: &Path,
    user_path: &Path,
) -> Result<Vec<PartsExcRecord>, EquipmentStateError> {
    merge_parts_records(
        load_parts_records(global_path)?,
        load_user_parts_states(user_path)?,
    )
}

fn merge_parts_records(
    mut merged: Vec<PartsExcRecord>,
    user_states: Vec<PartsState>,
) -> Result<Vec<PartsExcRecord>, EquipmentStateError> {
    for user in user_states {
        let user = user.as_exception();
        if let Some(existing) = merged
            .iter_mut()
            .find(|record| record.id == user.id && record.serial == user.serial)
        {
            *existing = user;
        } else {
            if merged.len() >= MAX_EQUIPMENT_RECORDS {
                return Err(EquipmentStateError::TooManyRecords {
                    kind: "merged parts",
                    maximum: MAX_EQUIPMENT_RECORDS,
                });
            }
            merged.push(user);
        }
    }
    Ok(merged)
}

fn load_user_parts_states(path: &Path) -> Result<Vec<PartsState>, EquipmentStateError> {
    parse_user_parts_states(path, read_optional_bounded(path)?)
}

fn parse_user_parts_states(
    path: &Path,
    bytes: Option<Vec<u8>>,
) -> Result<Vec<PartsState>, EquipmentStateError> {
    let Some(bytes) = bytes else {
        return Ok(Vec::new());
    };
    let bytes = bytes
        .strip_prefix(&[0xef, 0xbb, 0xbf])
        .unwrap_or(bytes.as_slice());
    let limit_exceeded = Cell::new(false);
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let states = match (PartsStatesSeed {
        limit_exceeded: &limit_exceeded,
    })
    .deserialize(&mut deserializer)
    {
        Ok(states) => states,
        Err(_source) if limit_exceeded.get() => {
            return Err(EquipmentStateError::TooManyRecords {
                kind: "parts",
                maximum: MAX_EQUIPMENT_RECORDS,
            });
        }
        Err(source) => {
            return Err(EquipmentStateError::Json {
                path: path.to_owned(),
                source,
            });
        }
    };
    deserializer
        .end()
        .map_err(|source| EquipmentStateError::Json {
            path: path.to_owned(),
            source,
        })?;
    Ok(states)
}

struct PartsStatesSeed<'a> {
    limit_exceeded: &'a Cell<bool>,
}

impl<'de> DeserializeSeed<'de> for PartsStatesSeed<'_> {
    type Value = Vec<PartsState>;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(PartsStatesVisitor {
            limit_exceeded: self.limit_exceeded,
        })
    }
}

struct PartsStatesVisitor<'a> {
    limit_exceeded: &'a Cell<bool>,
}

impl<'de> Visitor<'de> for PartsStatesVisitor<'_> {
    type Value = Vec<PartsState>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an array of P5136 X-parts state records")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let capacity = sequence.size_hint().unwrap_or(0).min(MAX_EQUIPMENT_RECORDS);
        let mut states = Vec::with_capacity(capacity);
        while states.len() < MAX_EQUIPMENT_RECORDS {
            let Some(state) = sequence.next_element::<PartsState>()? else {
                return Ok(states);
            };
            states.push(state);
        }
        let extra = sequence.next_element_seed(RejectAdditionalPartsRecord {
            limit_exceeded: self.limit_exceeded,
        })?;
        debug_assert!(extra.is_none());
        Ok(states)
    }
}

struct RejectAdditionalPartsRecord<'a> {
    limit_exceeded: &'a Cell<bool>,
}

impl<'de> DeserializeSeed<'de> for RejectAdditionalPartsRecord<'_> {
    type Value = ();

    fn deserialize<D>(self, _deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        self.limit_exceeded.set(true);
        Err(D::Error::custom("X-parts equipment record limit exceeded"))
    }
}

fn parse_parts_record<R>(
    reader: &Reader<R>,
    element: &BytesStart<'_>,
    path: &Path,
) -> Result<PartsExcRecord, EquipmentStateError> {
    let id = i16_attribute(reader, element, path, b"id", "id")?;
    let serial = i16_attribute(reader, element, path, b"sn", "sn")?;
    Ok(PartsExcRecord {
        id,
        serial: normalize_kart_serial(id, serial),
        engine: i16_attribute(reader, element, path, b"Item_Id1", "Item_Id1")?,
        engine_grade: u8_attribute(reader, element, path, b"Grade1", "Grade1")?,
        engine_value: i16_attribute(reader, element, path, b"PartsValue1", "PartsValue1")?,
        handle: i16_attribute(reader, element, path, b"Item_Id2", "Item_Id2")?,
        handle_grade: u8_attribute(reader, element, path, b"Grade2", "Grade2")?,
        handle_value: i16_attribute(reader, element, path, b"PartsValue2", "PartsValue2")?,
        wheel: i16_attribute(reader, element, path, b"Item_Id3", "Item_Id3")?,
        wheel_grade: u8_attribute(reader, element, path, b"Grade3", "Grade3")?,
        wheel_value: i16_attribute(reader, element, path, b"PartsValue3", "PartsValue3")?,
        booster: i16_attribute(reader, element, path, b"Item_Id4", "Item_Id4")?,
        booster_grade: u8_attribute(reader, element, path, b"Grade4", "Grade4")?,
        booster_value: i16_attribute(reader, element, path, b"PartsValue4", "PartsValue4")?,
        coating: i16_attribute(reader, element, path, b"partsCoating", "partsCoating")?,
        tail_lamp: i16_attribute(reader, element, path, b"partsTailLamp", "partsTailLamp")?,
    })
}

fn i16_attribute<R>(
    reader: &Reader<R>,
    element: &BytesStart<'_>,
    path: &Path,
    name: &[u8],
    display_name: &'static str,
) -> Result<i16, EquipmentStateError> {
    numeric_attribute(reader, element, path, name, display_name, str::parse)
}

fn u8_attribute<R>(
    reader: &Reader<R>,
    element: &BytesStart<'_>,
    path: &Path,
    name: &[u8],
    display_name: &'static str,
) -> Result<u8, EquipmentStateError> {
    numeric_attribute(reader, element, path, name, display_name, str::parse)
}

fn numeric_attribute<R, T, E, F>(
    reader: &Reader<R>,
    element: &BytesStart<'_>,
    path: &Path,
    name: &[u8],
    display_name: &'static str,
    parse: F,
) -> Result<T, EquipmentStateError>
where
    F: FnOnce(&str) -> Result<T, E>,
{
    let value = attribute(reader, element, path, name)?.ok_or_else(|| {
        EquipmentStateError::InvalidPartsAttribute {
            path: path.to_owned(),
            attribute: display_name,
        }
    })?;
    parse(&value).map_err(|_| EquipmentStateError::InvalidPartsAttribute {
        path: path.to_owned(),
        attribute: display_name,
    })
}

fn attribute<R>(
    reader: &Reader<R>,
    element: &BytesStart<'_>,
    path: &Path,
    name: &[u8],
) -> Result<Option<String>, EquipmentStateError> {
    for result in element.attributes() {
        let attribute =
            result
                .map_err(quick_xml::Error::from)
                .map_err(|source| EquipmentStateError::Xml {
                    path: path.to_owned(),
                    source,
                })?;
        if attribute.key == QName(name) {
            let value: Cow<'_, str> = attribute
                .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())
                .map_err(|source| EquipmentStateError::Xml {
                    path: path.to_owned(),
                    source,
                })?;
            return Ok(Some(value.into_owned()));
        }
    }
    Ok(None)
}

fn read_optional_bounded(path: &Path) -> Result<Option<Vec<u8>>, EquipmentStateError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(EquipmentStateError::Io {
                operation: "open",
                path: path.to_owned(),
                source,
            });
        }
    };
    let length = file
        .metadata()
        .map_err(|source| EquipmentStateError::Io {
            operation: "inspect",
            path: path.to_owned(),
            source,
        })?
        .len();
    if length > MAX_EQUIPMENT_STATE_BYTES {
        return Err(EquipmentStateError::TooLarge {
            path: path.to_owned(),
            actual: length,
            maximum: MAX_EQUIPMENT_STATE_BYTES,
        });
    }

    let mut bytes = Vec::with_capacity(usize::try_from(length).unwrap_or(64 * 1024));
    let mut input: Take<File> = file.take(MAX_EQUIPMENT_STATE_BYTES.saturating_add(1));
    input
        .read_to_end(&mut bytes)
        .map_err(|source| EquipmentStateError::Io {
            operation: "read",
            path: path.to_owned(),
            source,
        })?;
    let actual = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if actual > MAX_EQUIPMENT_STATE_BYTES {
        return Err(EquipmentStateError::TooLarge {
            path: path.to_owned(),
            actual,
            maximum: MAX_EQUIPMENT_STATE_BYTES,
        });
    }
    Ok(Some(bytes))
}

fn read_optional_bounded_capability(
    directory: &CapabilityDir,
    filename: &str,
    display_path: &Path,
) -> Result<Option<Vec<u8>>, EquipmentStateError> {
    let metadata = match directory.symlink_metadata(filename) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(EquipmentStateError::Io {
                operation: "inspect equipment sidecar without following links",
                path: display_path.to_owned(),
                source,
            });
        }
    };
    if !metadata.file_type().is_file() {
        return Err(EquipmentStateError::InvalidStorageEntry {
            path: display_path.to_owned(),
        });
    }

    let mut options = CapabilityOpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let file =
        directory
            .open_with(filename, &options)
            .map_err(|source| EquipmentStateError::Io {
                operation: "open equipment sidecar without following links",
                path: display_path.to_owned(),
                source,
            })?;
    let opened_metadata = file.metadata().map_err(|source| EquipmentStateError::Io {
        operation: "inspect opened equipment sidecar",
        path: display_path.to_owned(),
        source,
    })?;
    if !opened_metadata.file_type().is_file() {
        return Err(EquipmentStateError::InvalidStorageEntry {
            path: display_path.to_owned(),
        });
    }
    if opened_metadata.len() > MAX_EQUIPMENT_STATE_BYTES {
        return Err(EquipmentStateError::TooLarge {
            path: display_path.to_owned(),
            actual: opened_metadata.len(),
            maximum: MAX_EQUIPMENT_STATE_BYTES,
        });
    }

    let capacity = usize::try_from(opened_metadata.len().min(64 * 1024)).unwrap_or_default();
    let mut bytes = Vec::with_capacity(capacity);
    file.take(MAX_EQUIPMENT_STATE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| EquipmentStateError::Io {
            operation: "read opened equipment sidecar",
            path: display_path.to_owned(),
            source,
        })?;
    let actual = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if actual > MAX_EQUIPMENT_STATE_BYTES {
        return Err(EquipmentStateError::TooLarge {
            path: display_path.to_owned(),
            actual,
            maximum: MAX_EQUIPMENT_STATE_BYTES,
        });
    }
    Ok(Some(bytes))
}

const fn normalize_kart_serial(id: i16, serial: i16) -> i16 {
    if id != 0 && serial == 0 { 1 } else { serial }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use cap_std::{ambient_authority, fs::Dir as CapabilityDir};
    use p5136_core::equipment_protocol::{PlantPartEquipRequest, XPartEquipRequest};
    use serde_json::Value;
    use tempfile::tempdir;

    use super::{
        EquipmentExceptions, EquipmentSidecar, EquipmentStateError, MAX_EQUIPMENT_RECORDS,
        fail_next_equipment_directory_sync,
    };

    const PARTS_XML: &str = r#"<PartsData>
        <Kart id="1401" sn="0"
              Item_Id1="10" Grade1="1" PartsValue1="1053"
              Item_Id2="20" Grade2="2" PartsValue2="1005"
              Item_Id3="30" Grade3="3" PartsValue3="910"
              Item_Id4="40" Grade4="4" PartsValue4="810"
              partsCoating="50" partsTailLamp="60" />
        <Kart id="1401" sn="3"
              Item_Id1="0" Grade1="0" PartsValue1="0"
              Item_Id2="0" Grade2="0" PartsValue2="0"
              Item_Id3="0" Grade3="0" PartsValue3="0"
              Item_Id4="0" Grade4="0" PartsValue4="0"
              partsCoating="0" partsTailLamp="0" />
    </PartsData>"#;

    fn equipment_capabilities(
        root: &tempfile::TempDir,
        rider: &std::path::Path,
    ) -> (CapabilityDir, CapabilityDir) {
        (
            CapabilityDir::open_ambient_dir(root.path(), ambient_authority()).unwrap(),
            CapabilityDir::open_ambient_dir(rider, ambient_authority()).unwrap(),
        )
    }

    #[test]
    fn loads_csharp_sidecars_and_normalizes_zero_kart_serials() {
        let root = tempdir().unwrap();
        let rider = root.path().join("Rider");
        fs::create_dir(&rider).unwrap();
        fs::write(
            rider.join("TuneData.json"),
            concat!(
                "[{\"ID\":1401,\"SN\":0,\"Tune1\":603,\"Tune2\":703,",
                "\"Tune3\":903,\"Slot1\":0,\"Count1\":4,",
                "\"Slot2\":2,\"Count2\":3,\"FutureTuneField\":true}]"
            ),
        )
        .unwrap();
        fs::write(
            rider.join("PlantData.json"),
            concat!(
                "\u{feff}[{\"ID\":1401,\"SN\":0,\"Engine\":43,\"EngineID\":5,",
                "\"Handle\":44,\"HandleID\":2,\"Wheel\":45,\"WheelID\":14,",
                "\"Kit\":46,\"KitID\":6},",
                "{\"ID\":1401,\"SN\":3}]"
            ),
        )
        .unwrap();
        fs::write(
            rider.join("LevelData.json"),
            concat!(
                "[{\"ID\":1401,\"SN\":0,\"Grade\":5,\"Points\":5,",
                "\"Level1\":10,\"Level2\":10,\"Level3\":5,\"Level4\":5,",
                "\"Effect\":7,\"FutureLevelField\":true}]"
            ),
        )
        .unwrap();
        fs::write(root.path().join("PartsData.xml"), PARTS_XML).unwrap();

        let state = EquipmentExceptions::load(root.path(), &rider).unwrap();
        assert_eq!(state.tune.len(), 1);
        assert_eq!(state.tune[0].serial, 1);
        assert_eq!(
            [
                state.tune[0].tune1,
                state.tune[0].tune2,
                state.tune[0].tune3
            ],
            [603, 703, 903]
        );
        assert_eq!(state.plant.len(), 2);
        assert_eq!(state.plant[0].serial, 1);
        assert_eq!(state.plant[1].serial, 3);
        assert_eq!(state.plant[0].engine_category, 43);
        assert_eq!(state.kart_level.len(), 1);
        assert_eq!(state.kart_level[0].serial, 1);
        assert_eq!(state.kart_level[0].grade, 5);
        assert_eq!(state.kart_level[0].points, 5);
        assert_eq!(state.kart_level[0].level1, 10);
        assert_eq!(state.kart_level[0].effect, 7);
        assert_eq!(state.parts.len(), 2);
        assert_eq!(state.parts[0].serial, 1);
        assert_eq!(state.parts[1].serial, 3);
        assert_eq!(state.parts[0].engine_value, 1_053);
        assert_eq!(state.parts[0].tail_lamp, 60);
    }

    #[test]
    fn missing_sidecars_mean_no_equipped_exceptions() {
        let root = tempdir().unwrap();
        let rider = root.path().join("Rider");
        let state = EquipmentExceptions::load(root.path(), &rider).unwrap();
        assert!(state.tune.is_empty());
        assert!(state.plant.is_empty());
        assert!(state.kart_level.is_empty());
        assert!(state.parts.is_empty());
    }

    #[test]
    fn lenient_preload_isolates_one_bad_sidecar_without_losing_valid_streams() {
        let root = tempdir().unwrap();
        let rider = root.path().join("Rider");
        fs::create_dir(&rider).unwrap();
        fs::write(rider.join("PlantData.json"), "not json").unwrap();
        fs::write(
            rider.join("LevelData.json"),
            r#"[{"ID":1401,"SN":1,"Grade":5,"Points":35}]"#,
        )
        .unwrap();
        fs::write(root.path().join("PartsData.xml"), PARTS_XML).unwrap();
        let (root_capability, rider_capability) = equipment_capabilities(&root, &rider);

        let loaded = EquipmentExceptions::load_lenient_from_capabilities(
            &root_capability,
            &rider_capability,
            root.path(),
            &rider,
        );

        assert!(loaded.equipment.plant.is_empty());
        assert_eq!(loaded.equipment.kart_level.len(), 1);
        assert_eq!(loaded.equipment.parts.len(), 2);
        assert_eq!(loaded.warnings.len(), 1);
        assert_eq!(loaded.warnings[0].sidecar, EquipmentSidecar::Plant);
    }

    #[test]
    fn black_floater_workflow_is_atomic_persistent_and_reset_replies_can_use_post_state() {
        let root = tempdir().unwrap();
        let rider = root.path().join("Rider");
        fs::create_dir(&rider).unwrap();
        let (_, rider_capability) = equipment_capabilities(&root, &rider);

        let activated = EquipmentExceptions::activate_floater_socket_capability(
            &rider_capability,
            &rider,
            633,
            0,
        )
        .unwrap();
        assert_eq!(activated.value.serial, 1);
        assert_eq!(
            [
                activated.value.tune1,
                activated.value.tune2,
                activated.value.tune3
            ],
            [0, 0, 0]
        );
        assert_eq!((activated.value.slot1, activated.value.slot2), (-1, -1));

        let black = EquipmentExceptions::apply_floater_tune_capability(
            &rider_capability,
            &rider,
            633,
            1,
            5,
        )
        .unwrap();
        assert_eq!(
            [black.value.tune1, black.value.tune2, black.value.tune3],
            [603, 703, 903]
        );

        let protected_first = EquipmentExceptions::protect_floater_slot_capability(
            &rider_capability,
            &rider,
            633,
            1,
            49,
            0,
        )
        .unwrap();
        assert_eq!(
            (protected_first.value.slot1, protected_first.value.count1),
            (0, 4)
        );
        let protected_third = EquipmentExceptions::protect_floater_slot_capability(
            &rider_capability,
            &rider,
            633,
            1,
            53,
            2,
        )
        .unwrap();
        assert_eq!(
            (protected_third.value.slot2, protected_third.value.count2),
            (2, 3)
        );

        let reset =
            EquipmentExceptions::reset_floater_socket_capability(&rider_capability, &rider, 633, 1)
                .unwrap();
        assert_eq!(
            [
                reset.value.before.tune1,
                reset.value.before.tune2,
                reset.value.before.tune3
            ],
            [603, 703, 903]
        );
        assert_eq!(
            [
                reset.value.after.tune1,
                reset.value.after.tune2,
                reset.value.after.tune3
            ],
            [603, 0, 903]
        );
        assert_eq!((reset.value.after.count1, reset.value.after.count2), (3, 2));

        let loaded = EquipmentExceptions::load(root.path(), &rider).unwrap();
        assert_eq!(loaded.tune, vec![reset.value.after]);
        let encoded: Value =
            serde_json::from_slice(&fs::read(rider.join("TuneData.json")).unwrap()).unwrap();
        assert_eq!(encoded[0]["ID"], 633);
        assert_eq!(encoded[0]["SN"], 1);
        assert_eq!(encoded[0]["Tune2"], 0);
    }

    #[test]
    fn floater_mutations_reject_invalid_state_without_replacing_the_sidecar() {
        let root = tempdir().unwrap();
        let rider = root.path().join("Rider");
        fs::create_dir(&rider).unwrap();
        let original = r#"[{"ID":633,"SN":1,"Tune1":603,"Tune2":603}]"#;
        fs::write(rider.join("TuneData.json"), original).unwrap();
        let (root_capability, rider_capability) = equipment_capabilities(&root, &rider);

        let loaded = EquipmentExceptions::load_lenient_from_capabilities(
            &root_capability,
            &rider_capability,
            root.path(),
            &rider,
        );
        assert!(loaded.equipment.tune.is_empty());
        assert_eq!(loaded.warnings.len(), 1);
        assert_eq!(loaded.warnings[0].sidecar, EquipmentSidecar::Tune);
        assert!(matches!(
            EquipmentExceptions::apply_floater_tune_capability(
                &rider_capability,
                &rider,
                633,
                1,
                5,
            ),
            Err(EquipmentStateError::DuplicateFloaterCode(603))
        ));
        assert_eq!(
            fs::read_to_string(rider.join("TuneData.json")).unwrap(),
            original
        );
    }

    #[test]
    fn repeated_floater_socket_creation_preserves_state_and_future_fields() {
        let root = tempdir().unwrap();
        let rider = root.path().join("Rider");
        fs::create_dir(&rider).unwrap();
        fs::write(
            rider.join("TuneData.json"),
            concat!(
                "[{\"ID\":633,\"SN\":1,\"Tune1\":603,\"Tune2\":703,",
                "\"Tune3\":903,\"Slot1\":0,\"Count1\":4,",
                "\"Slot2\":2,\"Count2\":3,\"Future\":{\"keep\":true}}]"
            ),
        )
        .unwrap();
        let (_, rider_capability) = equipment_capabilities(&root, &rider);

        let repeated = EquipmentExceptions::activate_floater_socket_capability(
            &rider_capability,
            &rider,
            633,
            1,
        )
        .unwrap();
        assert_eq!(
            [
                repeated.value.tune1,
                repeated.value.tune2,
                repeated.value.tune3
            ],
            [603, 703, 903]
        );
        let encoded: Value =
            serde_json::from_slice(&fs::read(rider.join("TuneData.json")).unwrap()).unwrap();
        assert_eq!(encoded[0]["Future"]["keep"], true);
    }

    #[test]
    fn canonical_sidecar_duplicates_are_last_wins_after_serial_normalization() {
        let root = tempdir().unwrap();
        let rider = root.path().join("Rider");
        fs::create_dir(&rider).unwrap();
        fs::write(
            rider.join("TuneData.json"),
            concat!(
                "[{\"ID\":1401,\"SN\":0,\"Tune1\":603,\"Tune2\":603},",
                "{\"ID\":1401,\"SN\":1,\"Tune1\":603,\"Tune2\":703,",
                "\"Tune3\":903,\"Slot1\":-1,\"Slot2\":-1}]"
            ),
        )
        .unwrap();
        fs::write(
            rider.join("PlantData.json"),
            concat!(
                "[{\"ID\":1401,\"SN\":0,\"Engine\":43,\"EngineID\":1},",
                "{\"ID\":1401,\"SN\":1,\"Engine\":43,\"EngineID\":2}]"
            ),
        )
        .unwrap();
        fs::write(
            rider.join("LevelData.json"),
            concat!(
                "[{\"ID\":1401,\"SN\":0,\"Grade\":5,\"Points\":34,\"Level1\":1},",
                "{\"ID\":1401,\"SN\":1,\"Grade\":5,\"Points\":33,\"Level1\":2}]"
            ),
        )
        .unwrap();

        let loaded = EquipmentExceptions::load(root.path(), &rider).unwrap();
        assert_eq!(loaded.tune.len(), 1);
        assert_eq!(loaded.tune[0].serial, 1);
        assert_eq!(loaded.tune[0].tune2, 703);
        assert_eq!(loaded.plant.len(), 1);
        assert_eq!(loaded.plant[0].engine_id, 2);
        assert_eq!(loaded.kart_level.len(), 1);
        assert_eq!(loaded.kart_level[0].level1, 2);
    }

    #[test]
    fn grade_zero_upgrade_normalizes_and_signed_deltas_redistribute_points() {
        let root = tempdir().unwrap();
        let rider = root.path().join("Rider");
        fs::create_dir(&rider).unwrap();
        fs::write(
            rider.join("LevelData.json"),
            r#"[{"ID":1401,"SN":1,"Grade":0,"Points":33,"Level1":2,"Effect":7}]"#,
        )
        .unwrap();
        let (_, rider_capability) = equipment_capabilities(&root, &rider);

        let upgraded =
            EquipmentExceptions::upgrade_kart_level_capability(&rider_capability, &rider, 1_401, 1)
                .unwrap();
        assert_eq!(upgraded.value.grade, 5);
        assert_eq!(upgraded.value.points, 35);
        assert_eq!(upgraded.value.level1, 0);
        assert_eq!(upgraded.value.effect, 7);

        let allocated = EquipmentExceptions::update_kart_level_points_capability(
            &rider_capability,
            &rider,
            1_401,
            1,
            [10, 0, 0, 0],
        )
        .unwrap();
        assert_eq!(allocated.value.level1, 10);
        let redistributed = EquipmentExceptions::update_kart_level_points_capability(
            &rider_capability,
            &rider,
            1_401,
            1,
            [-5, 5, 0, 0],
        )
        .unwrap();
        assert_eq!(redistributed.value.level1, 5);
        assert_eq!(redistributed.value.level2, 5);
        assert_eq!(redistributed.value.points, 25);
    }

    #[test]
    fn post_publish_sync_failure_is_reported_as_committed_not_retriable() {
        let root = tempdir().unwrap();
        let rider = root.path().join("Rider");
        fs::create_dir(&rider).unwrap();
        let (_, rider_capability) = equipment_capabilities(&root, &rider);
        fail_next_equipment_directory_sync();

        let outcome = EquipmentExceptions::update_kart_level_points_capability(
            &rider_capability,
            &rider,
            1_401,
            1,
            [5, 0, 0, 0],
        )
        .unwrap();

        assert!(outcome.durability_warning.is_some());
        assert_eq!(outcome.value.level1, 5);
        let persisted = EquipmentExceptions::load(root.path(), &rider).unwrap();
        assert_eq!(persisted.kart_level[0].level1, 5);
    }

    #[test]
    fn rejects_out_of_range_kart_level_sidecars_before_preload_or_physics() {
        let root = tempdir().unwrap();
        let rider = root.path().join("Rider");
        fs::create_dir(&rider).unwrap();
        fs::write(
            rider.join("LevelData.json"),
            "[{\"ID\":1401,\"SN\":1,\"Points\":35,\"Level1\":11}]",
        )
        .unwrap();

        assert!(matches!(
            EquipmentExceptions::load(root.path(), &rider),
            Err(EquipmentStateError::InvalidKartLevelPoint { slot: 1, value: 11 })
        ));
    }

    #[test]
    fn rejects_doctype_and_missing_numeric_attributes() {
        let root = tempdir().unwrap();
        let rider = root.path().join("Rider");
        fs::write(
            root.path().join("PartsData.xml"),
            "<!DOCTYPE PartsData><PartsData />",
        )
        .unwrap();
        assert!(matches!(
            EquipmentExceptions::load(root.path(), &rider),
            Err(EquipmentStateError::DocumentType { .. })
        ));

        fs::write(
            root.path().join("PartsData.xml"),
            r#"<PartsData><Kart id="1401" /></PartsData>"#,
        )
        .unwrap();
        assert!(matches!(
            EquipmentExceptions::load(root.path(), &rider),
            Err(EquipmentStateError::InvalidPartsAttribute {
                attribute: "sn",
                ..
            })
        ));
    }

    #[test]
    fn rejects_the_first_plant_record_beyond_the_limit() {
        let root = tempdir().unwrap();
        let rider = root.path().join("Rider");
        fs::create_dir(&rider).unwrap();
        let mut json = String::with_capacity((MAX_EQUIPMENT_RECORDS + 1) * 3 + 2);
        json.push('[');
        for index in 0..=MAX_EQUIPMENT_RECORDS {
            if index != 0 {
                json.push(',');
            }
            json.push_str("{}");
        }
        json.push(']');
        fs::write(rider.join("PlantData.json"), json).unwrap();

        let result = EquipmentExceptions::load(root.path(), &rider);

        assert!(matches!(
            result,
            Err(EquipmentStateError::TooManyRecords {
                kind: "plant",
                maximum: MAX_EQUIPMENT_RECORDS,
            })
        ));
    }

    #[test]
    fn equips_and_atomically_replaces_a_csharp_plant_sidecar() {
        let root = tempdir().unwrap();
        let rider = root.path().join("Rider");
        fs::create_dir(&rider).unwrap();
        fs::write(
            rider.join("PlantData.json"),
            concat!(
                "[{\"ID\":1401,\"SN\":1,\"Engine\":43,\"EngineID\":5,",
                "\"Handle\":0,\"HandleID\":0,\"Wheel\":45,\"WheelID\":14,",
                "\"Kit\":0,\"KitID\":0,\"FutureField\":{\"keep\":true}}]"
            ),
        )
        .unwrap();

        let equipped = EquipmentExceptions::equip_plant_part(
            &rider,
            PlantPartEquipRequest {
                item_category: 44,
                item_id: 2,
                kart_category: 3,
                kart_id: 1_401,
                kart_serial: 1,
                replaced_part: None,
            },
        )
        .unwrap();
        assert_eq!(equipped.engine_id, 5);
        assert_eq!(equipped.handle_category, 44);
        assert_eq!(equipped.handle_id, 2);
        assert_eq!(equipped.wheel_id, 14);

        let encoded: Value =
            serde_json::from_slice(&fs::read(rider.join("PlantData.json")).unwrap()).unwrap();
        assert_eq!(encoded[0]["FutureField"]["keep"], true);
        assert_eq!(encoded[0]["Handle"], 44);
        assert_eq!(encoded[0]["HandleID"], 2);
        assert!(fs::read_dir(&rider).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
    }

    #[test]
    fn first_equip_creates_one_normalized_record_and_later_updates_it() {
        let root = tempdir().unwrap();
        let rider = root.path().join("Rider");
        let first = EquipmentExceptions::equip_plant_part(
            &rider,
            PlantPartEquipRequest {
                item_category: 43,
                item_id: 5,
                kart_category: 3,
                kart_id: 1_401,
                kart_serial: 0,
                replaced_part: None,
            },
        )
        .unwrap();
        assert_eq!(first.serial, 1);

        let second = EquipmentExceptions::equip_plant_part(
            &rider,
            PlantPartEquipRequest {
                item_category: 46,
                item_id: 6,
                kart_category: 3,
                kart_id: 1_401,
                kart_serial: 1,
                replaced_part: None,
            },
        )
        .unwrap();
        assert_eq!(second.engine_id, 5);
        assert_eq!(second.kit_id, 6);

        let loaded = EquipmentExceptions::load(root.path(), &rider).unwrap();
        assert_eq!(loaded.plant, vec![second]);
    }

    #[test]
    fn direct_plant_mutation_revalidates_typed_requests() {
        let root = tempdir().unwrap();
        let request = PlantPartEquipRequest {
            item_category: 42,
            item_id: -1,
            kart_category: 3,
            kart_id: 0,
            kart_serial: 0,
            replaced_part: None,
        };
        assert!(matches!(
            EquipmentExceptions::equip_plant_part(root.path(), request),
            Err(EquipmentStateError::InvalidPlantPartCategory(42))
        ));
        assert!(!root.path().join("PlantData.json").exists());
    }

    #[test]
    fn equips_x_parts_atomically_and_preload_prefers_the_rider_sidecar() {
        let root = tempdir().unwrap();
        let rider = root.path().join("Rider");
        fs::create_dir(&rider).unwrap();
        fs::write(root.path().join("PartsData.xml"), PARTS_XML).unwrap();
        fs::write(
            rider.join("PartsData.json"),
            concat!(
                "[{\"ID\":1401,\"SN\":1,\"Engine\":1,\"EngineGrade\":2,",
                "\"EngineValue\":3,\"FutureField\":{\"keep\":true}}]"
            ),
        )
        .unwrap();
        let request = XPartEquipRequest {
            kart_id: 1_401,
            kart_serial: 1,
            item_category: 63,
            item_id: 2,
            quantity: i16::MAX,
            unknown_1: 0,
            grade: 1,
            unknown_2: 1,
            parts_value: 1_180,
            unknown_3: 0,
        };

        let equipped = EquipmentExceptions::equip_x_part(&rider, request).unwrap();
        assert_eq!(equipped.id, 1_401);
        assert_eq!(equipped.serial, 1);
        assert_eq!(equipped.engine, 2);
        assert_eq!(equipped.engine_grade, 1);
        assert_eq!(equipped.engine_value, 1_180);

        let encoded: Value =
            serde_json::from_slice(&fs::read(rider.join("PartsData.json")).unwrap()).unwrap();
        assert_eq!(encoded[0]["FutureField"]["keep"], true);
        assert_eq!(encoded[0]["Engine"], 2);
        assert_eq!(encoded[0]["EngineGrade"], 1);
        assert_eq!(encoded[0]["EngineValue"], 1_180);
        assert!(fs::read_dir(&rider).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));

        let loaded = EquipmentExceptions::load(root.path(), &rider).unwrap();
        assert_eq!(loaded.parts.len(), 2);
        assert_eq!(loaded.parts[0], equipped);
        assert_eq!(loaded.parts[1].serial, 3);
    }

    #[test]
    fn direct_x_part_mutation_revalidates_typed_requests() {
        let root = tempdir().unwrap();
        let request = XPartEquipRequest {
            kart_id: 1_401,
            kart_serial: 1,
            item_category: 67,
            item_id: -1,
            quantity: 0,
            unknown_1: 0,
            grade: 0,
            unknown_2: 0,
            parts_value: 0,
            unknown_3: 0,
        };

        assert!(matches!(
            EquipmentExceptions::equip_x_part(root.path(), request),
            Err(EquipmentStateError::InvalidXPartCategory(67))
        ));
        assert!(!root.path().join("PartsData.json").exists());
    }
}
