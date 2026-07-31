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
    inventory::{PartsExcRecord, PlantExcRecord},
};
use quick_xml::{
    Reader, XmlVersion,
    events::{BytesStart, Event},
    name::QName,
};
use serde::{
    Deserialize, Serialize,
    de::{DeserializeSeed, Deserializer, Error as _, SeqAccess, Visitor},
};
use thiserror::Error;

use crate::store::ProfileStoreError;

pub const MAX_EQUIPMENT_STATE_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_EQUIPMENT_RECORDS: usize = 65_535;
static EQUIPMENT_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct EquipmentExceptions {
    pub plant: Vec<PlantExcRecord>,
    pub parts: Vec<PartsExcRecord>,
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
        let plant_path = rider_directory.join("PlantData.json");
        let parts_path = profile_root.join("PartsData.xml");
        let user_parts_path = rider_directory.join("PartsData.json");
        Ok(Self {
            plant: load_plant_records(&plant_path)?,
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
        let parts_path = profile_root_path.join("PartsData.xml");
        let user_parts_path = rider_directory_path.join("PartsData.json");
        let plant = parse_plant_states(
            &plant_path,
            read_optional_bounded_capability(rider_directory, "PlantData.json", &plant_path)?,
        )?
        .iter()
        .map(PlantState::as_exception)
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
        Ok(Self { plant, parts })
    }

    pub(crate) fn equip_x_part_capability(
        rider_directory: &CapabilityDir,
        rider_directory_path: &Path,
        request: XPartEquipRequest,
    ) -> Result<PartsExcRecord, EquipmentStateError> {
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
        write_equipment_states_capability(
            rider_directory,
            &path,
            "PartsData.json",
            ".PartsData",
            &states,
        )?;
        Ok(result)
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
    Ok(states)
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
) -> Result<(), EquipmentStateError> {
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
    sync_equipment_capability_directory(directory, display_path.parent().unwrap_or(display_path))
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

    use p5136_core::equipment_protocol::{PlantPartEquipRequest, XPartEquipRequest};
    use serde_json::Value;
    use tempfile::tempdir;

    use super::{EquipmentExceptions, EquipmentStateError, MAX_EQUIPMENT_RECORDS};

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

    #[test]
    fn loads_csharp_sidecars_and_normalizes_zero_kart_serials() {
        let root = tempdir().unwrap();
        let rider = root.path().join("Rider");
        fs::create_dir(&rider).unwrap();
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
        fs::write(root.path().join("PartsData.xml"), PARTS_XML).unwrap();

        let state = EquipmentExceptions::load(root.path(), &rider).unwrap();
        assert_eq!(state.plant.len(), 2);
        assert_eq!(state.plant[0].serial, 1);
        assert_eq!(state.plant[1].serial, 3);
        assert_eq!(state.plant[0].engine_category, 43);
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
        assert!(state.plant.is_empty());
        assert!(state.parts.is_empty());
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
