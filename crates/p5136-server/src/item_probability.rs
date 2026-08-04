use std::{
    collections::HashSet,
    fs::File,
    io::Read,
    num::NonZeroU64,
    path::{Path, PathBuf},
};

use p5136_core::{
    bml::{BmlError, BmlLimits, BmlNode},
    packet::PacketReader,
};
use p5136_rho5::{
    LegacyRhoArchive, LegacyRhoError, LegacyRhoLimits, Rho5Directory, Rho5Error, Rho5Limits,
};
use quick_xml::{Reader, events::Event};
use thiserror::Error;

const INDIVIDUAL_BML_PATH: &str = "slot/itemProb_indi@zz.bml";
const TEAM_BML_PATH: &str = "slot/itemProb_team@zz.bml";
const MAX_ENTRIES_PER_TABLE: usize = 512;
const MAX_ITEM_NAME_BYTES: usize = 256;
const MAX_WEIGHT: u32 = 1_000_000;
const MAX_TABLE_BYTES: usize = 1024 * 1024;
const MAX_TABLE_READ_BYTES: u64 = 1024 * 1024 + 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemProbabilityRankBand {
    Live,
    Top,
    High,
    Middle,
    Low,
    Combined,
}

// Stock P5136 uses a fixed participant-count matrix, not an even three-way
// split of every rank below first place. The index within each row is the
// zero-based rank reported by the client.
const LIVE_RANK_BANDS: [&[ItemProbabilityRankBand]; 8] = [
    &[ItemProbabilityRankBand::Top],
    &[
        ItemProbabilityRankBand::Top,
        ItemProbabilityRankBand::Middle,
    ],
    &[
        ItemProbabilityRankBand::Top,
        ItemProbabilityRankBand::Middle,
        ItemProbabilityRankBand::Low,
    ],
    &[
        ItemProbabilityRankBand::Top,
        ItemProbabilityRankBand::Middle,
        ItemProbabilityRankBand::Middle,
        ItemProbabilityRankBand::Low,
    ],
    &[
        ItemProbabilityRankBand::Top,
        ItemProbabilityRankBand::High,
        ItemProbabilityRankBand::Middle,
        ItemProbabilityRankBand::Middle,
        ItemProbabilityRankBand::Low,
    ],
    &[
        ItemProbabilityRankBand::Top,
        ItemProbabilityRankBand::High,
        ItemProbabilityRankBand::Middle,
        ItemProbabilityRankBand::Middle,
        ItemProbabilityRankBand::Low,
        ItemProbabilityRankBand::Low,
    ],
    &[
        ItemProbabilityRankBand::Top,
        ItemProbabilityRankBand::High,
        ItemProbabilityRankBand::High,
        ItemProbabilityRankBand::Middle,
        ItemProbabilityRankBand::Middle,
        ItemProbabilityRankBand::Low,
        ItemProbabilityRankBand::Low,
    ],
    &[
        ItemProbabilityRankBand::Top,
        ItemProbabilityRankBand::High,
        ItemProbabilityRankBand::High,
        ItemProbabilityRankBand::Middle,
        ItemProbabilityRankBand::Middle,
        ItemProbabilityRankBand::Middle,
        ItemProbabilityRankBand::Low,
        ItemProbabilityRankBand::Low,
    ],
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ItemProbabilityRankPolicy {
    /// LAN/friends compatibility mode: use the rank carried by the validated
    /// client pickup request, matching the C# probability-band selection.
    #[default]
    TrustClientReported,
    /// Do not let the client select a live probability band until the actor
    /// owns authoritative race positions.
    CombinedFallback,
}

impl ItemProbabilityRankBand {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Live => "Live rank (automatic)",
            Self::Top => "1st place",
            Self::High => "High",
            Self::Middle => "Middle",
            Self::Low => "Low",
            Self::Combined => "Combined",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "live" => Some(Self::Live),
            "top" => Some(Self::Top),
            "high" => Some(Self::High),
            "middle" | "mid" => Some(Self::Middle),
            "low" => Some(Self::Low),
            "combined" | "all" => Some(Self::Combined),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemProbabilityEntry {
    pub item_id: i16,
    pub name: String,
    pub top_weight: u32,
    pub high_weight: u32,
    pub middle_weight: u32,
    pub low_weight: u32,
}

impl ItemProbabilityEntry {
    #[must_use]
    pub const fn weight(&self, rank_band: ItemProbabilityRankBand) -> u64 {
        match rank_band {
            ItemProbabilityRankBand::Top => self.top_weight as u64,
            ItemProbabilityRankBand::High => self.high_weight as u64,
            ItemProbabilityRankBand::Middle => self.middle_weight as u64,
            ItemProbabilityRankBand::Low => self.low_weight as u64,
            ItemProbabilityRankBand::Combined | ItemProbabilityRankBand::Live => {
                self.top_weight as u64
                    + self.high_weight as u64
                    + self.middle_weight as u64
                    + self.low_weight as u64
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemProbabilityConfiguration {
    pub rank_band: ItemProbabilityRankBand,
    pub individual: Vec<ItemProbabilityEntry>,
    pub team: Vec<ItemProbabilityEntry>,
}

impl Default for ItemProbabilityConfiguration {
    fn default() -> Self {
        Self::safe_fallback()
    }
}

impl ItemProbabilityConfiguration {
    #[must_use]
    pub fn safe_fallback() -> Self {
        let common = [
            (2, "devil"),
            (3, "ufo"),
            (4, "waterFly"),
            (5, "magnet"),
            (6, "booster"),
            (7, "rocket"),
            (8, "banana"),
            (9, "waterBomb"),
            (10, "shield"),
            (11, "angel"),
            (12, "emp"),
            (13, "timeBomb"),
            (33, "guideRocket"),
            (111, "thunderbolt"),
        ];
        let individual = common
            .into_iter()
            .map(|(item_id, name)| equal_entry(item_id, name))
            .collect::<Vec<_>>();
        let mut team = individual.clone();
        team.extend([
            equal_entry(109, "scanning"),
            equal_entry(110, "slotLock"),
            equal_entry(113, "barricade"),
            equal_entry(114, "cloud2"),
        ]);
        Self {
            rank_band: ItemProbabilityRankBand::Live,
            individual,
            team,
        }
    }

    pub fn validate(&self) -> Result<(), ItemProbabilityError> {
        validate_table("individual", &self.individual, self.rank_band)?;
        validate_table("team", &self.team, self.rank_band)
    }

    #[must_use]
    pub fn resolve_rank_band(
        configured: ItemProbabilityRankBand,
        client_reported_rank: i16,
        racer_count: usize,
        policy: ItemProbabilityRankPolicy,
    ) -> ItemProbabilityRankBand {
        if configured != ItemProbabilityRankBand::Live {
            return configured;
        }
        if policy == ItemProbabilityRankPolicy::CombinedFallback {
            return ItemProbabilityRankBand::Combined;
        }
        let Ok(live_rank) = usize::try_from(client_reported_rank) else {
            return ItemProbabilityRankBand::Combined;
        };
        racer_count
            .checked_sub(1)
            .and_then(|row| LIVE_RANK_BANDS.get(row))
            .and_then(|bands| bands.get(live_rank))
            .copied()
            .unwrap_or(ItemProbabilityRankBand::Combined)
    }

    pub fn roll_total(
        &self,
        team_mode: bool,
        live_rank: i16,
        racer_count: usize,
        rank_policy: ItemProbabilityRankPolicy,
    ) -> Result<(ItemProbabilityRankBand, NonZeroU64), ItemProbabilityError> {
        let band = Self::resolve_rank_band(self.rank_band, live_rank, racer_count, rank_policy);
        let total = self
            .table(team_mode)
            .iter()
            .try_fold(0_u64, |total, entry| {
                total
                    .checked_add(entry.weight(band))
                    .ok_or(ItemProbabilityError::WeightTotalOverflow)
            })?;
        let total = NonZeroU64::new(total).ok_or(ItemProbabilityError::ZeroActiveWeight {
            table: if team_mode { "team" } else { "individual" },
            rank_band: band,
        })?;
        Ok((band, total))
    }

    pub fn select_with_roll(
        &self,
        team_mode: bool,
        live_rank: i16,
        racer_count: usize,
        roll: u64,
        rank_policy: ItemProbabilityRankPolicy,
    ) -> Result<(i16, ItemProbabilityRankBand), ItemProbabilityError> {
        let (band, total) = self.roll_total(team_mode, live_rank, racer_count, rank_policy)?;
        if roll >= total.get() {
            return Err(ItemProbabilityError::RollOutOfRange {
                roll,
                total: total.get(),
            });
        }
        let mut cursor = roll;
        for entry in self.table(team_mode) {
            let weight = entry.weight(band);
            if cursor < weight {
                return Ok((entry.item_id, band));
            }
            cursor -= weight;
        }
        Err(ItemProbabilityError::SelectionInvariant)
    }

    fn table(&self, team_mode: bool) -> &[ItemProbabilityEntry] {
        if team_mode {
            &self.team
        } else {
            &self.individual
        }
    }
}

#[derive(Debug, Error)]
pub enum ItemProbabilityError {
    #[error("{table} item-probability table is empty")]
    EmptyTable { table: &'static str },
    #[error("{table} item-probability table has {actual} entries; maximum is {maximum}")]
    TooManyEntries {
        table: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("{table} item-probability entry {index} has invalid item ID {item_id}")]
    InvalidItemId {
        table: &'static str,
        index: usize,
        item_id: i16,
    },
    #[error("{table} item-probability table repeats item ID {item_id}")]
    DuplicateItemId { table: &'static str, item_id: i16 },
    #[error("{table} item-probability entry {index} has an invalid name")]
    InvalidName { table: &'static str, index: usize },
    #[error(
        "{table} item-probability entry {index} has {field} weight {actual}; maximum is {maximum}"
    )]
    WeightTooLarge {
        table: &'static str,
        index: usize,
        field: &'static str,
        actual: u32,
        maximum: u32,
    },
    #[error("{table} item-probability table has zero weight for {rank_band:?}")]
    ZeroActiveWeight {
        table: &'static str,
        rank_band: ItemProbabilityRankBand,
    },
    #[error("item-probability weight total overflowed")]
    WeightTotalOverflow,
    #[error("item-probability roll {roll} is outside 0..{total}")]
    RollOutOfRange { roll: u64, total: u64 },
    #[error("item-probability selection did not resolve despite a validated nonzero total")]
    SelectionInvariant,
    #[error("failed to read item-probability XML {path}")]
    ReadXml {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("item-probability XML {path} has {actual} bytes; maximum is {maximum}")]
    XmlTooLarge {
        path: PathBuf,
        actual: usize,
        maximum: usize,
    },
    #[error("failed to parse item-probability XML")]
    Xml(#[source] quick_xml::Error),
    #[error("item-probability XML is missing {section}")]
    MissingXmlSection { section: &'static str },
    #[error("item-probability XML has no itemProbabilities root")]
    MissingXmlRoot,
    #[error("item-probability XML contains unexpected element {element:?}")]
    UnexpectedXmlElement { element: String },
    #[error("item-probability XML repeats {section}")]
    DuplicateXmlSection { section: &'static str },
    #[error("item-probability XML document types are not supported")]
    XmlDocumentType,
    #[error("item-probability XML nesting depth overflowed")]
    XmlNestingTooDeep,
    #[error("item-probability XML ended with {depth} unclosed elements")]
    UnclosedXml { depth: usize },
    #[error("item-probability XML contains character data outside attributes")]
    UnexpectedXmlText,
    #[error("item-probability XML has an invalid rankBand value {value:?}")]
    InvalidRankBand { value: String },
    #[error("item-probability XML item is missing attribute {attribute}")]
    MissingAttribute { attribute: &'static str },
    #[error("item-probability value {attribute}={value:?} is invalid")]
    InvalidAttribute {
        attribute: &'static str,
        value: String,
    },
    #[error(transparent)]
    Rho5(#[from] Rho5Error),
    #[error(transparent)]
    LegacyRho(#[from] LegacyRhoError),
    #[error(transparent)]
    Bml(#[from] BmlError),
    #[error("item-probability BML contains trailing bytes")]
    BmlTrailingBytes,
    #[error("item-probability BML root must be 'items', got {actual:?}")]
    InvalidBmlRoot { actual: String },
}

pub fn load_client_item_probabilities(
    data_directory: impl AsRef<Path>,
) -> Result<ItemProbabilityConfiguration, ItemProbabilityError> {
    let data_directory = data_directory.as_ref();
    let legacy_item_rho = data_directory.join("item.rho");
    let (individual, team) = if legacy_item_rho.is_file() {
        let archive = LegacyRhoArchive::open(legacy_item_rho, LegacyRhoLimits::default())?;
        (
            parse_bml_table(&archive.extract_exact(INDIVIDUAL_BML_PATH)?)?,
            parse_bml_table(&archive.extract_exact(TEAM_BML_PATH)?)?,
        )
    } else {
        let directory = Rho5Directory::scan_kr(data_directory, production_rho5_limits())?;
        (
            extract_rho5_bml_table(&directory, INDIVIDUAL_BML_PATH)?,
            extract_rho5_bml_table(&directory, TEAM_BML_PATH)?,
        )
    };
    let configuration = ItemProbabilityConfiguration {
        rank_band: ItemProbabilityRankBand::Live,
        individual,
        team,
    };
    configuration.validate()?;
    Ok(configuration)
}

/// Loads a portable XML override:
///
/// `<itemProbabilities rankBand="live"><individual><item .../></individual>`
/// `<team><item .../></team></itemProbabilities>`.
pub fn load_item_probability_xml(
    path: impl AsRef<Path>,
) -> Result<ItemProbabilityConfiguration, ItemProbabilityError> {
    let path = path.as_ref();
    let file = File::open(path).map_err(|source| ItemProbabilityError::ReadXml {
        path: path.to_path_buf(),
        source,
    })?;
    let metadata = file
        .metadata()
        .map_err(|source| ItemProbabilityError::ReadXml {
            path: path.to_path_buf(),
            source,
        })?;
    let declared_length = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
    if declared_length > MAX_TABLE_BYTES {
        return Err(ItemProbabilityError::XmlTooLarge {
            path: path.to_path_buf(),
            actual: declared_length,
            maximum: MAX_TABLE_BYTES,
        });
    }
    let mut bytes = Vec::with_capacity(declared_length);
    file.take(MAX_TABLE_READ_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|source| ItemProbabilityError::ReadXml {
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() > MAX_TABLE_BYTES {
        return Err(ItemProbabilityError::XmlTooLarge {
            path: path.to_path_buf(),
            actual: bytes.len(),
            maximum: MAX_TABLE_BYTES,
        });
    }
    parse_portable_xml(&bytes)
}

fn parse_portable_xml(bytes: &[u8]) -> Result<ItemProbabilityConfiguration, ItemProbabilityError> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(true);
    let mut rank_band = ItemProbabilityRankBand::Live;
    let mut current_table = None;
    let mut depth = 0_usize;
    let mut saw_root = false;
    let mut saw_individual = false;
    let mut saw_team = false;
    let mut individual = Vec::new();
    let mut team = Vec::new();

    loop {
        match reader.read_event().map_err(ItemProbabilityError::Xml)? {
            Event::Start(event) => {
                start_xml_element(
                    &reader,
                    &event,
                    depth,
                    &mut rank_band,
                    &mut current_table,
                    &mut saw_root,
                    &mut saw_individual,
                    &mut saw_team,
                    &mut individual,
                    &mut team,
                )?;
                depth = depth
                    .checked_add(1)
                    .ok_or(ItemProbabilityError::XmlNestingTooDeep)?;
            }
            Event::Empty(event) => {
                start_xml_element(
                    &reader,
                    &event,
                    depth,
                    &mut rank_band,
                    &mut current_table,
                    &mut saw_root,
                    &mut saw_individual,
                    &mut saw_team,
                    &mut individual,
                    &mut team,
                )?;
                if depth == 1 {
                    current_table = None;
                }
            }
            Event::End(event) => {
                if depth == 0 {
                    return Err(ItemProbabilityError::UnexpectedXmlElement {
                        element: String::from_utf8_lossy(event.local_name().as_ref()).into_owned(),
                    });
                }
                if depth == 2 && matches!(event.local_name().as_ref(), b"individual" | b"team") {
                    current_table = None;
                }
                depth -= 1;
            }
            Event::DocType(_) => return Err(ItemProbabilityError::XmlDocumentType),
            Event::Text(text) if !text.as_ref().is_empty() => {
                return Err(ItemProbabilityError::UnexpectedXmlText);
            }
            Event::CData(_) | Event::GeneralRef(_) => {
                return Err(ItemProbabilityError::UnexpectedXmlText);
            }
            Event::Eof => {
                if depth != 0 || current_table.is_some() {
                    return Err(ItemProbabilityError::UnclosedXml { depth });
                }
                break;
            }
            _ => {}
        }
    }
    if !saw_root {
        return Err(ItemProbabilityError::MissingXmlRoot);
    }
    if !saw_individual {
        return Err(ItemProbabilityError::MissingXmlSection {
            section: "individual",
        });
    }
    if !saw_team {
        return Err(ItemProbabilityError::MissingXmlSection { section: "team" });
    }
    let configuration = ItemProbabilityConfiguration {
        rank_band,
        individual,
        team,
    };
    configuration.validate()?;
    Ok(configuration)
}

#[allow(clippy::too_many_arguments)]
fn start_xml_element(
    reader: &Reader<&[u8]>,
    event: &quick_xml::events::BytesStart<'_>,
    depth: usize,
    rank_band: &mut ItemProbabilityRankBand,
    current_table: &mut Option<bool>,
    saw_root: &mut bool,
    saw_individual: &mut bool,
    saw_team: &mut bool,
    individual: &mut Vec<ItemProbabilityEntry>,
    team: &mut Vec<ItemProbabilityEntry>,
) -> Result<(), ItemProbabilityError> {
    let name = event.local_name();
    match (depth, name.as_ref()) {
        (0, b"itemProbabilities") if !*saw_root => {
            *saw_root = true;
            for attribute in event.attributes().with_checks(true) {
                let attribute = attribute
                    .map_err(quick_xml::Error::from)
                    .map_err(ItemProbabilityError::Xml)?;
                if attribute.key.local_name().as_ref() == b"rankBand" {
                    let value = attribute
                        .decoded_and_normalized_value(
                            quick_xml::XmlVersion::Implicit1_0,
                            reader.decoder(),
                        )
                        .map_err(ItemProbabilityError::Xml)?
                        .into_owned();
                    *rank_band = ItemProbabilityRankBand::parse(&value)
                        .ok_or(ItemProbabilityError::InvalidRankBand { value })?;
                }
            }
        }
        (1, b"individual") if !*saw_individual && current_table.is_none() => {
            *current_table = Some(false);
            *saw_individual = true;
        }
        (1, b"individual") => {
            return Err(ItemProbabilityError::DuplicateXmlSection {
                section: "individual",
            });
        }
        (1, b"team") if !*saw_team && current_table.is_none() => {
            *current_table = Some(true);
            *saw_team = true;
        }
        (1, b"team") => {
            return Err(ItemProbabilityError::DuplicateXmlSection { section: "team" });
        }
        (2, b"item") if *current_table == Some(false) => {
            individual.push(xml_entry(reader, event)?);
        }
        (2, b"item") if *current_table == Some(true) => {
            team.push(xml_entry(reader, event)?);
        }
        _ => {
            return Err(ItemProbabilityError::UnexpectedXmlElement {
                element: String::from_utf8_lossy(name.as_ref()).into_owned(),
            });
        }
    }
    Ok(())
}

fn xml_entry(
    reader: &Reader<&[u8]>,
    event: &quick_xml::events::BytesStart<'_>,
) -> Result<ItemProbabilityEntry, ItemProbabilityError> {
    let mut values = std::collections::HashMap::new();
    for attribute in event.attributes().with_checks(true) {
        let attribute = attribute
            .map_err(quick_xml::Error::from)
            .map_err(ItemProbabilityError::Xml)?;
        let key = String::from_utf8_lossy(attribute.key.local_name().as_ref()).into_owned();
        let value = attribute
            .decoded_and_normalized_value(quick_xml::XmlVersion::Implicit1_0, reader.decoder())
            .map_err(ItemProbabilityError::Xml)?
            .into_owned();
        values.insert(key, value);
    }
    entry_from_values(|name| values.get(name).map(String::as_str))
}

fn extract_rho5_bml_table(
    directory: &Rho5Directory,
    path: &str,
) -> Result<Vec<ItemProbabilityEntry>, ItemProbabilityError> {
    let entry = directory.unique_entry(path)?;
    if entry.plaintext_size() > MAX_TABLE_BYTES {
        return Err(ItemProbabilityError::XmlTooLarge {
            path: PathBuf::from(path),
            actual: entry.plaintext_size(),
            maximum: MAX_TABLE_BYTES,
        });
    }
    parse_bml_table(&directory.extract_exact(path)?)
}

fn parse_bml_table(bytes: &[u8]) -> Result<Vec<ItemProbabilityEntry>, ItemProbabilityError> {
    let limits = BmlLimits {
        max_depth: 4,
        max_nodes: MAX_ENTRIES_PER_TABLE + 1,
        max_attributes_per_node: 16,
        max_children_per_node: MAX_ENTRIES_PER_TABLE,
        max_string_code_units: 128,
    };
    let mut reader = PacketReader::new(bytes);
    let root = BmlNode::decode_with_limits(&mut reader, limits)?;
    if !reader.remaining().is_empty() {
        return Err(ItemProbabilityError::BmlTrailingBytes);
    }
    if !root.name.eq_ignore_ascii_case("items") {
        return Err(ItemProbabilityError::InvalidBmlRoot { actual: root.name });
    }
    root.children
        .iter()
        .filter(|child| child.name.eq_ignore_ascii_case("item"))
        .map(|child| {
            entry_from_values(|name| {
                child
                    .attributes
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case(name))
                    .map(|(_, value)| value.as_str())
            })
        })
        .collect()
}

fn entry_from_values<'a>(
    get: impl Fn(&str) -> Option<&'a str>,
) -> Result<ItemProbabilityEntry, ItemProbabilityError> {
    let item_id = required_parse(&get, "idx")?;
    let name = get("name")
        .ok_or(ItemProbabilityError::MissingAttribute { attribute: "name" })?
        .to_owned();
    Ok(ItemProbabilityEntry {
        item_id,
        name,
        top_weight: required_parse(&get, "toprank")?,
        high_weight: required_parse(&get, "highrank")?,
        middle_weight: required_parse(&get, "midrank")?,
        low_weight: required_parse(&get, "lowrank")?,
    })
}

fn required_parse<'a, T: std::str::FromStr>(
    get: &impl Fn(&str) -> Option<&'a str>,
    attribute: &'static str,
) -> Result<T, ItemProbabilityError> {
    let value = get(attribute).ok_or(ItemProbabilityError::MissingAttribute { attribute })?;
    value
        .parse()
        .map_err(|_| ItemProbabilityError::InvalidAttribute {
            attribute,
            value: value.to_owned(),
        })
}

fn validate_table(
    table: &'static str,
    entries: &[ItemProbabilityEntry],
    configured_band: ItemProbabilityRankBand,
) -> Result<(), ItemProbabilityError> {
    if entries.is_empty() {
        return Err(ItemProbabilityError::EmptyTable { table });
    }
    if entries.len() > MAX_ENTRIES_PER_TABLE {
        return Err(ItemProbabilityError::TooManyEntries {
            table,
            actual: entries.len(),
            maximum: MAX_ENTRIES_PER_TABLE,
        });
    }
    let mut ids = HashSet::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        if entry.item_id <= 0 {
            return Err(ItemProbabilityError::InvalidItemId {
                table,
                index,
                item_id: entry.item_id,
            });
        }
        if !ids.insert(entry.item_id) {
            return Err(ItemProbabilityError::DuplicateItemId {
                table,
                item_id: entry.item_id,
            });
        }
        if entry.name.is_empty()
            || entry.name.chars().count() > 64
            || entry.name.len() > MAX_ITEM_NAME_BYTES
            || entry.name.chars().any(char::is_control)
        {
            return Err(ItemProbabilityError::InvalidName { table, index });
        }
        for (field, weight) in [
            ("top", entry.top_weight),
            ("high", entry.high_weight),
            ("middle", entry.middle_weight),
            ("low", entry.low_weight),
        ] {
            if weight > MAX_WEIGHT {
                return Err(ItemProbabilityError::WeightTooLarge {
                    table,
                    index,
                    field,
                    actual: weight,
                    maximum: MAX_WEIGHT,
                });
            }
        }
    }

    let bands: &[ItemProbabilityRankBand] = if configured_band == ItemProbabilityRankBand::Live {
        &[
            ItemProbabilityRankBand::Top,
            ItemProbabilityRankBand::High,
            ItemProbabilityRankBand::Middle,
            ItemProbabilityRankBand::Low,
        ]
    } else {
        &[configured_band]
    };
    for &rank_band in bands {
        if entries.iter().all(|entry| entry.weight(rank_band) == 0) {
            return Err(ItemProbabilityError::ZeroActiveWeight { table, rank_band });
        }
    }
    Ok(())
}

fn equal_entry(item_id: i16, name: &str) -> ItemProbabilityEntry {
    ItemProbabilityEntry {
        item_id,
        name: name.to_owned(),
        top_weight: 1,
        high_weight: 1,
        middle_weight: 1,
        low_weight: 1,
    }
}

fn production_rho5_limits() -> Rho5Limits {
    Rho5Limits {
        max_directory_entries: 4_096,
        max_archives: 128,
        max_archive_bytes: 64 * 1024 * 1024,
        max_archive_name_bytes: 255,
        max_files_per_archive: 4_096,
        max_total_files: 30_000,
        max_path_utf16_units: 512,
        max_normalized_path_bytes: 2 * 1024,
        max_table_bytes: 8 * 1024 * 1024,
        max_compressed_bytes: 16 * 1024 * 1024,
        max_plaintext_bytes: 32 * 1024 * 1024,
        max_total_declared_compressed_bytes: 1024 * 1024 * 1024,
        max_total_declared_plaintext_bytes: 2 * 1024 * 1024 * 1024,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ItemProbabilityConfiguration, ItemProbabilityError, ItemProbabilityRankBand,
        ItemProbabilityRankPolicy, MAX_TABLE_BYTES, load_client_item_probabilities,
        load_item_probability_xml, parse_portable_xml,
    };

    #[test]
    fn live_rank_policy_uses_the_stock_participant_matrix_or_combined_fallback() {
        use ItemProbabilityRankBand::{High, Low, Middle, Top};

        let expected: &[(usize, &[ItemProbabilityRankBand])] = &[
            (1, &[Top]),
            (2, &[Top, Middle]),
            (3, &[Top, Middle, Low]),
            (4, &[Top, Middle, Middle, Low]),
            (5, &[Top, High, Middle, Middle, Low]),
            (6, &[Top, High, Middle, Middle, Low, Low]),
            (7, &[Top, High, High, Middle, Middle, Low, Low]),
            (8, &[Top, High, High, Middle, Middle, Middle, Low, Low]),
        ];
        for &(racer_count, bands) in expected {
            let actual = bands
                .iter()
                .enumerate()
                .map(|(rank, _)| {
                    ItemProbabilityConfiguration::resolve_rank_band(
                        ItemProbabilityRankBand::Live,
                        i16::try_from(rank).unwrap(),
                        racer_count,
                        ItemProbabilityRankPolicy::TrustClientReported,
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(actual, bands, "{racer_count}-racer matrix");
        }
        for (rank, racer_count) in [(-1, 8), (8, 8), (0, 0), (0, 9)] {
            assert_eq!(
                ItemProbabilityConfiguration::resolve_rank_band(
                    ItemProbabilityRankBand::Live,
                    rank,
                    racer_count,
                    ItemProbabilityRankPolicy::TrustClientReported,
                ),
                ItemProbabilityRankBand::Combined
            );
        }
        for rank in [-1, 0, 7, 8] {
            assert_eq!(
                ItemProbabilityConfiguration::resolve_rank_band(
                    ItemProbabilityRankBand::Live,
                    rank,
                    8,
                    ItemProbabilityRankPolicy::CombinedFallback,
                ),
                ItemProbabilityRankBand::Combined
            );
        }
    }

    #[test]
    fn portable_xml_loads_both_tables_and_selects_deterministically() {
        let xml = br#"
            <itemProbabilities rankBand="combined">
              <individual>
                <item idx="8" name="banana" toprank="1" highrank="2" midrank="3" lowrank="4"/>
                <item idx="10" name="shield" toprank="0" highrank="0" midrank="0" lowrank="1"/>
              </individual>
              <team>
                <item idx="11" name="angel" toprank="1" highrank="1" midrank="1" lowrank="1"/>
              </team>
            </itemProbabilities>
        "#;
        let configuration = parse_portable_xml(xml).unwrap();
        assert_eq!(
            configuration
                .select_with_roll(
                    false,
                    0,
                    8,
                    9,
                    ItemProbabilityRankPolicy::TrustClientReported,
                )
                .unwrap(),
            (8, ItemProbabilityRankBand::Combined)
        );
        assert_eq!(
            configuration
                .select_with_roll(
                    false,
                    0,
                    8,
                    10,
                    ItemProbabilityRankPolicy::TrustClientReported,
                )
                .unwrap(),
            (10, ItemProbabilityRankBand::Combined)
        );
        assert_eq!(
            configuration
                .select_with_roll(
                    true,
                    0,
                    8,
                    3,
                    ItemProbabilityRankPolicy::TrustClientReported,
                )
                .unwrap(),
            (11, ItemProbabilityRankBand::Combined)
        );
    }

    #[test]
    fn portable_xml_requires_one_exact_root_and_each_section_once() {
        let wrong_root = br#"
            <items>
              <individual><item idx="8" name="banana" toprank="1" highrank="1" midrank="1" lowrank="1"/></individual>
              <team><item idx="11" name="angel" toprank="1" highrank="1" midrank="1" lowrank="1"/></team>
            </items>
        "#;
        assert!(matches!(
            parse_portable_xml(wrong_root),
            Err(ItemProbabilityError::UnexpectedXmlElement { .. })
        ));

        let duplicate = br#"
            <itemProbabilities rankBand="top">
              <individual><item idx="8" name="banana" toprank="1" highrank="1" midrank="1" lowrank="1"/></individual>
              <individual><item idx="10" name="shield" toprank="1" highrank="1" midrank="1" lowrank="1"/></individual>
              <team><item idx="11" name="angel" toprank="1" highrank="1" midrank="1" lowrank="1"/></team>
            </itemProbabilities>
        "#;
        assert!(matches!(
            parse_portable_xml(duplicate),
            Err(ItemProbabilityError::DuplicateXmlSection {
                section: "individual"
            })
        ));

        let truncated = br#"
            <itemProbabilities rankBand="top">
              <individual><item idx="8" name="banana" toprank="1" highrank="1" midrank="1" lowrank="1"/></individual>
              <team><item idx="11" name="angel" toprank="1" highrank="1" midrank="1" lowrank="1"/>
        "#;
        assert!(matches!(
            parse_portable_xml(truncated),
            Err(ItemProbabilityError::UnclosedXml { .. })
        ));

        let character_data = br#"
            <itemProbabilities rankBand="top">
              <individual>unexpected<item idx="8" name="banana" toprank="1" highrank="1" midrank="1" lowrank="1"/></individual>
              <team><item idx="11" name="angel" toprank="1" highrank="1" midrank="1" lowrank="1"/></team>
            </itemProbabilities>
        "#;
        assert!(matches!(
            parse_portable_xml(character_data),
            Err(ItemProbabilityError::UnexpectedXmlText)
        ));

        let missing_weight = br#"
            <itemProbabilities rankBand="top">
              <individual><item idx="8" name="banana" toprank="1" midrank="1" lowrank="1"/></individual>
              <team><item idx="11" name="angel" toprank="1" highrank="1" midrank="1" lowrank="1"/></team>
            </itemProbabilities>
        "#;
        assert!(matches!(
            parse_portable_xml(missing_weight),
            Err(ItemProbabilityError::MissingAttribute {
                attribute: "highrank"
            })
        ));
    }

    #[test]
    fn portable_xml_rejects_an_oversized_file_before_parsing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("oversized.xml");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(u64::try_from(MAX_TABLE_BYTES + 1).unwrap())
            .unwrap();
        assert!(matches!(
            load_item_probability_xml(path),
            Err(ItemProbabilityError::XmlTooLarge {
                actual,
                maximum: MAX_TABLE_BYTES,
                ..
            }) if actual == MAX_TABLE_BYTES + 1
        ));
    }

    #[test]
    fn configured_stock_client_archive_tables_match_the_known_p5136_totals() {
        let Ok(data_directory) = std::env::var("P5136_CLIENT_DATA_DIR") else {
            return;
        };
        let configuration = load_client_item_probabilities(data_directory).unwrap();
        assert_eq!(configuration.individual.len(), 14);
        assert_eq!(configuration.team.len(), 18);
        assert_eq!(
            configuration
                .individual
                .iter()
                .map(|entry| entry.weight(ItemProbabilityRankBand::Combined))
                .sum::<u64>(),
            400
        );
        assert_eq!(
            configuration
                .team
                .iter()
                .map(|entry| entry.weight(ItemProbabilityRankBand::Combined))
                .sum::<u64>(),
            410
        );
    }
}
