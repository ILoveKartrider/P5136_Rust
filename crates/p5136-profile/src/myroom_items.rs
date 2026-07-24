//! Bounded compatibility loader for the three C# `MyRoom` owner-item sidecars.

use std::{
    cell::Cell,
    fmt,
    fs::File,
    io::{Read, Take},
    marker::PhantomData,
    path::{Path, PathBuf},
};

use p5136_core::myroom_protocol::{MyRoomKart, MyRoomParts, MyRoomTune};
use serde::{
    Deserialize,
    de::{DeserializeSeed, Deserializer, Error as _, SeqAccess, Visitor},
};
use thiserror::Error;

pub const MAX_MYROOM_ITEM_STATE_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_MYROOM_ITEM_RECORDS: usize = 65_535;

const NEW_KART_FILENAME: &str = "NewKart.json";
const TUNE_FILENAME: &str = "TuneData.json";
const PARTS_FILENAME: &str = "PartsData.json";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MyRoomOwnerInventory {
    pub tunes: Vec<MyRoomTune>,
    pub karts: Vec<MyRoomKart>,
    pub parts: Vec<MyRoomParts>,
}

impl MyRoomOwnerInventory {
    /// Loads the exact C# sidecar filenames below one already-resolved rider
    /// directory. Missing files represent empty collections.
    pub fn load(rider_directory: impl AsRef<Path>) -> Result<Self, MyRoomItemStateError> {
        let rider_directory = rider_directory.as_ref();
        let tunes = load_records::<TuneState>(&rider_directory.join(TUNE_FILENAME), "tune")?
            .into_iter()
            .map(TuneState::into_wire)
            .collect();
        let karts = load_records::<KartState>(&rider_directory.join(NEW_KART_FILENAME), "kart")?
            .into_iter()
            .map(KartState::into_wire)
            .collect();
        let parts = load_records::<PartsState>(&rider_directory.join(PARTS_FILENAME), "parts")?
            .into_iter()
            .map(PartsState::into_wire)
            .collect();
        Ok(Self {
            tunes,
            karts,
            parts,
        })
    }
}

#[derive(Debug, Error)]
pub enum MyRoomItemStateError {
    #[error("failed to {operation} MyRoom item state file {path}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("MyRoom item state file {path} has {actual} bytes; maximum is {maximum}")]
    TooLarge {
        path: PathBuf,
        actual: u64,
        maximum: u64,
    },

    #[error("MyRoom item state JSON at {path} is invalid")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("MyRoom {kind} state has more than {maximum} records")]
    TooManyRecords { kind: &'static str, maximum: usize },
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct KartState {
    #[serde(rename = "KartID")]
    kart_id: u16,
    #[serde(rename = "KartSN")]
    serial_number: u16,
}

impl KartState {
    const fn into_wire(self) -> MyRoomKart {
        MyRoomKart {
            kart_id: self.kart_id,
            serial_number: self.serial_number,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct TuneState {
    #[serde(rename = "ID")]
    item_id: i16,
    #[serde(rename = "SN")]
    serial_number: i16,
    tune1: i16,
    tune2: i16,
    tune3: i16,
    slot1: i16,
    count1: i16,
    slot2: i16,
    count2: i16,
}

impl TuneState {
    const fn into_wire(self) -> MyRoomTune {
        MyRoomTune {
            item_id: self.item_id,
            serial_number: self.serial_number,
            tune_1: self.tune1,
            tune_2: self.tune2,
            tune_3: self.tune3,
            slot_1: self.slot1,
            count_1: self.count1,
            slot_2: self.slot2,
            count_2: self.count2,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct PartsState {
    #[serde(rename = "ID")]
    item_id: i16,
    #[serde(rename = "SN")]
    serial_number: i16,
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
}

impl PartsState {
    const fn into_wire(self) -> MyRoomParts {
        MyRoomParts {
            item_id: self.item_id,
            serial_number: self.serial_number,
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

struct RecordsSeed<'a, T> {
    limit_exceeded: &'a Cell<bool>,
    marker: PhantomData<T>,
}

impl<'de, T> DeserializeSeed<'de> for RecordsSeed<'_, T>
where
    T: Deserialize<'de>,
{
    type Value = Vec<T>;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(RecordsVisitor {
            limit_exceeded: self.limit_exceeded,
            marker: PhantomData,
        })
    }
}

struct RecordsVisitor<'a, T> {
    limit_exceeded: &'a Cell<bool>,
    marker: PhantomData<T>,
}

impl<'de, T> Visitor<'de> for RecordsVisitor<'_, T>
where
    T: Deserialize<'de>,
{
    type Value = Vec<T>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an array of P5136 MyRoom item records")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let capacity = sequence
            .size_hint()
            .unwrap_or(0)
            .min(MAX_MYROOM_ITEM_RECORDS);
        let mut records = Vec::with_capacity(capacity);
        while records.len() < MAX_MYROOM_ITEM_RECORDS {
            let Some(record) = sequence.next_element::<T>()? else {
                return Ok(records);
            };
            records.push(record);
        }
        let extra = sequence.next_element_seed(RejectAdditionalRecord {
            limit_exceeded: self.limit_exceeded,
        })?;
        debug_assert!(extra.is_none());
        Ok(records)
    }
}

struct RejectAdditionalRecord<'a> {
    limit_exceeded: &'a Cell<bool>,
}

impl<'de> DeserializeSeed<'de> for RejectAdditionalRecord<'_> {
    type Value = ();

    fn deserialize<D>(self, _deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        self.limit_exceeded.set(true);
        Err(D::Error::custom("MyRoom item record limit exceeded"))
    }
}

fn load_records<T>(path: &Path, kind: &'static str) -> Result<Vec<T>, MyRoomItemStateError>
where
    T: for<'de> Deserialize<'de>,
{
    let Some(bytes) = read_optional_bounded(path)? else {
        return Ok(Vec::new());
    };
    let bytes = bytes
        .strip_prefix(&[0xef, 0xbb, 0xbf])
        .unwrap_or(bytes.as_slice());
    let limit_exceeded = Cell::new(false);
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let records = match (RecordsSeed::<T> {
        limit_exceeded: &limit_exceeded,
        marker: PhantomData,
    })
    .deserialize(&mut deserializer)
    {
        Ok(records) => records,
        Err(_source) if limit_exceeded.get() => {
            return Err(MyRoomItemStateError::TooManyRecords {
                kind,
                maximum: MAX_MYROOM_ITEM_RECORDS,
            });
        }
        Err(source) => {
            return Err(MyRoomItemStateError::Json {
                path: path.to_owned(),
                source,
            });
        }
    };
    deserializer
        .end()
        .map_err(|source| MyRoomItemStateError::Json {
            path: path.to_owned(),
            source,
        })?;
    Ok(records)
}

fn read_optional_bounded(path: &Path) -> Result<Option<Vec<u8>>, MyRoomItemStateError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(MyRoomItemStateError::Io {
                operation: "open",
                path: path.to_owned(),
                source,
            });
        }
    };
    let length = file
        .metadata()
        .map_err(|source| MyRoomItemStateError::Io {
            operation: "inspect",
            path: path.to_owned(),
            source,
        })?
        .len();
    if length > MAX_MYROOM_ITEM_STATE_BYTES {
        return Err(MyRoomItemStateError::TooLarge {
            path: path.to_owned(),
            actual: length,
            maximum: MAX_MYROOM_ITEM_STATE_BYTES,
        });
    }

    let mut bytes = Vec::with_capacity(usize::try_from(length).unwrap_or(64 * 1024));
    let mut input: Take<File> = file.take(MAX_MYROOM_ITEM_STATE_BYTES.saturating_add(1));
    input
        .read_to_end(&mut bytes)
        .map_err(|source| MyRoomItemStateError::Io {
            operation: "read",
            path: path.to_owned(),
            source,
        })?;
    let actual = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if actual > MAX_MYROOM_ITEM_STATE_BYTES {
        return Err(MyRoomItemStateError::TooLarge {
            path: path.to_owned(),
            actual,
            maximum: MAX_MYROOM_ITEM_STATE_BYTES,
        });
    }
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{
        MAX_MYROOM_ITEM_RECORDS, MAX_MYROOM_ITEM_STATE_BYTES, MyRoomItemStateError,
        MyRoomOwnerInventory,
    };
    use p5136_core::myroom_protocol::{MyRoomKart, MyRoomParts, MyRoomTune};

    #[test]
    fn missing_sidecars_are_empty() {
        let root = tempdir().unwrap();
        assert_eq!(
            MyRoomOwnerInventory::load(root.path()).unwrap(),
            MyRoomOwnerInventory::default()
        );
    }

    #[test]
    fn csharp_pascal_case_sidecars_and_utf8_bom_map_to_wire_records() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join("TuneData.json"),
            concat!(
                "\u{feff}[{\"ID\":1,\"SN\":2,\"Tune1\":3,\"Tune2\":4,",
                "\"Tune3\":5,\"Slot1\":6,\"Count1\":7,\"Slot2\":8,\"Count2\":9}]"
            ),
        )
        .unwrap();
        fs::write(
            root.path().join("NewKart.json"),
            br#"[{"KartID":5136,"KartSN":7,"FutureField":true}]"#,
        )
        .unwrap();
        fs::write(
            root.path().join("PartsData.json"),
            br#"[{"ID":10,"SN":11,"Engine":12,"EngineGrade":13,"EngineValue":14,"Handle":15,"HandleGrade":16,"HandleValue":17,"Wheel":18,"WheelGrade":19,"WheelValue":20,"Booster":21,"BoosterGrade":22,"BoosterValue":23,"Coating":24,"TailLamp":25}]"#,
        )
        .unwrap();

        let inventory = MyRoomOwnerInventory::load(root.path()).unwrap();
        assert_eq!(
            inventory.tunes,
            [MyRoomTune {
                item_id: 1,
                serial_number: 2,
                tune_1: 3,
                tune_2: 4,
                tune_3: 5,
                slot_1: 6,
                count_1: 7,
                slot_2: 8,
                count_2: 9,
            }]
        );
        assert_eq!(
            inventory.karts,
            [MyRoomKart {
                kart_id: 5136,
                serial_number: 7,
            }]
        );
        assert_eq!(
            inventory.parts,
            [MyRoomParts {
                item_id: 10,
                serial_number: 11,
                engine: 12,
                engine_grade: 13,
                engine_value: 14,
                handle: 15,
                handle_grade: 16,
                handle_value: 17,
                wheel: 18,
                wheel_grade: 19,
                wheel_value: 20,
                booster: 21,
                booster_grade: 22,
                booster_value: 23,
                coating: 24,
                tail_lamp: 25,
            }]
        );
    }

    #[test]
    fn malformed_and_oversized_files_are_typed_errors() {
        let root = tempdir().unwrap();
        let malformed = root.path().join("NewKart.json");
        fs::write(&malformed, b"[{").unwrap();
        assert!(matches!(
            MyRoomOwnerInventory::load(root.path()),
            Err(MyRoomItemStateError::Json { path, .. }) if path == malformed
        ));

        fs::write(&malformed, b"[]").unwrap();
        let oversized = root.path().join("TuneData.json");
        let file = fs::File::create(&oversized).unwrap();
        file.set_len(MAX_MYROOM_ITEM_STATE_BYTES + 1).unwrap();
        assert!(matches!(
            MyRoomOwnerInventory::load(root.path()),
            Err(MyRoomItemStateError::TooLarge {
                path,
                actual,
                maximum: MAX_MYROOM_ITEM_STATE_BYTES,
            }) if path == oversized && actual == MAX_MYROOM_ITEM_STATE_BYTES + 1
        ));
    }

    #[test]
    fn record_limit_is_enforced_during_deserialization() {
        let root = tempdir().unwrap();
        let path = root.path().join("NewKart.json");
        let mut source = String::with_capacity((MAX_MYROOM_ITEM_RECORDS + 1) * 3);
        source.push('[');
        for index in 0..=MAX_MYROOM_ITEM_RECORDS {
            if index != 0 {
                source.push(',');
            }
            source.push_str("{}");
        }
        source.push(']');
        fs::write(&path, source).unwrap();

        assert!(matches!(
            MyRoomOwnerInventory::load(root.path()),
            Err(MyRoomItemStateError::TooManyRecords {
                kind: "kart",
                maximum: MAX_MYROOM_ITEM_RECORDS,
            })
        ));
    }
}
