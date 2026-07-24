//! Bounded loading of per-rider plant state and legacy equipped-parts state.

use std::{
    borrow::Cow,
    cell::Cell,
    fmt,
    fs::File,
    io::{Read, Take},
    path::{Path, PathBuf},
};

use p5136_core::inventory::{PartsExcRecord, PlantExcRecord};
use quick_xml::{
    Reader, XmlVersion,
    events::{BytesStart, Event},
    name::QName,
};
use serde::{
    Deserialize,
    de::{DeserializeSeed, Deserializer, Error as _, SeqAccess, Visitor},
};
use thiserror::Error;

pub const MAX_EQUIPMENT_STATE_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_EQUIPMENT_RECORDS: usize = 65_535;

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
        Ok(Self {
            plant: load_plant_records(&plant_path)?,
            parts: load_parts_records(&parts_path)?,
        })
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
}

#[derive(Debug, Default, Deserialize)]
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
    let Some(bytes) = read_optional_bounded(path)? else {
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
    Ok(states
        .into_iter()
        .map(|state| PlantExcRecord {
            id: state.id,
            serial: normalize_kart_serial(state.id, state.serial),
            engine_category: state.engine_category,
            engine_id: state.engine_id,
            handle_category: state.handle_category,
            handle_id: state.handle_id,
            wheel_category: state.wheel_category,
            wheel_id: state.wheel_id,
            kit_category: state.kit_category,
            kit_id: state.kit_id,
        })
        .collect())
}

fn load_parts_records(path: &Path) -> Result<Vec<PartsExcRecord>, EquipmentStateError> {
    let Some(bytes) = read_optional_bounded(path)? else {
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

const fn normalize_kart_serial(id: i16, serial: i16) -> i16 {
    if id != 0 && serial == 0 { 1 } else { serial }
}

#[cfg(test)]
mod tests {
    use std::fs;

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
}
