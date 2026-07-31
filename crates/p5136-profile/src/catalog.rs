//! Bounded loader for the inventory section of an exported P5136 kart catalog.
//!
//! `KartCatalog.xml` is generated from a user's own client installation. It is
//! deliberately treated as runtime input and is never embedded in this crate.

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fmt,
    fs::File,
    io::{BufRead, BufReader, Read},
    path::Path,
};

pub use p5136_core::kart_physics::P5136KartSpecSnapshot;
use p5136_core::{dotnet_decimal::DotNetDecimal, myroom_protocol::MAX_MYROOM_EMBLEMS};
use quick_xml::{
    Reader, XmlVersion,
    events::{BytesStart, Event},
    name::QName,
};
use thiserror::Error;

use crate::emblem_catalog::EmblemCatalog;

pub const MAX_CATALOG_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_CATALOG_ITEMS: usize = 100_000;
pub const MAX_CATALOG_EMBLEMS: usize = MAX_MYROOM_EMBLEMS;
pub const MAX_ITEM_NAME_BYTES: usize = 1_024;
pub const MAX_KART_NAMES: usize = 10_000;
pub const MAX_KART_SPECS: usize = 10_000;
pub const MAX_KART_NAME_BYTES: usize = 256;
pub const MAX_XML_TEXT_BYTES: usize = 4 * 1024;
pub const MAX_XML_ATTRIBUTE_VALUE_BYTES: usize = 4 * 1024;
pub const MAX_XML_ATTRIBUTES_PER_ELEMENT: usize = 256;

const CATALOG_FORMAT_VERSION: &str = "3";
const CATALOG_PROTOCOL_VERSION: &str = "5136";
const CATALOG_REGION: &str = "kr";

const MINIMUM_INVENTORY_ITEMS: usize = 6_800;
const MINIMUM_INVENTORY_CATEGORIES: usize = 60;
const MINIMUM_INVENTORY_KARTS: usize = 1_200;
const MINIMUM_GRANT_ITEMS: usize = 5_250;
const MINIMUM_GRANT_CATEGORIES: usize = 41;

const GRANT_CATEGORY_IDS: &[u16] = &[
    1, 2, 3, 4, 7, 8, 9, 11, 12, 13, 14, 16, 18, 20, 21, 22, 23, 26, 27, 28, 30, 31, 32, 36, 37,
    38, 39, 43, 44, 45, 46, 49, 52, 53, 55, 59, 61, 67, 68, 69, 70,
];

const UNSAFE_CHARACTER_ITEM_IDS: &[u16] = &[
    45, 47, 48, 52, 59, 116, 117, 124, 128, 130, 137, 144, 147, 149, 159, 175, 176, 184, 192, 193,
    194, 195, 196, 197, 231, 245, 246, 247, 265, 301, 302, 333, 350, 376, 377, 391, 392, 396, 397,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogInventoryItem {
    pub category: u16,
    pub id: u16,
    pub serial: u16,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogInventoryStats {
    pub items: usize,
    pub categories: usize,
    pub karts: usize,
    pub grant_items: usize,
    pub grant_categories: usize,
    pub emblems: usize,
}

impl fmt::Display for CatalogInventoryStats {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "items={}, categories={}, karts={}, grant={}/{} categories, emblems={}",
            self.items,
            self.categories,
            self.karts,
            self.grant_items,
            self.grant_categories,
            self.emblems
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogKartSpecStats {
    pub names: usize,
    pub specs: usize,
    pub resolved_names: usize,
    pub unresolved_names: usize,
    pub unreferenced_specs: usize,
}

impl fmt::Display for CatalogKartSpecStats {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "names={}, specs={}, resolved={}, unresolved={}, unreferenced={}",
            self.names,
            self.specs,
            self.resolved_names,
            self.unresolved_names,
            self.unreferenced_specs
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CatalogInventory {
    items: Vec<CatalogInventoryItem>,
    stats: CatalogInventoryStats,
    kart_names: BTreeMap<u16, String>,
    kart_specs: BTreeMap<String, P5136KartSpecSnapshot>,
    kart_spec_stats: CatalogKartSpecStats,
    emblem_catalog: Option<EmblemCatalog>,
}

// Catalog parsing rejects non-finite values, so the floating-point snapshots
// retained here satisfy the reflexivity requirement of `Eq`.
impl Eq for CatalogInventory {}

impl CatalogInventory {
    /// Loads and fully validates the inventory portion of a Korean P5136
    /// catalog exported by the reference server.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, CatalogInventoryError> {
        let path = path.as_ref();
        let file = File::open(path)?;
        let byte_len = file.metadata()?.len();
        if byte_len > MAX_CATALOG_BYTES {
            return Err(CatalogInventoryError::DocumentTooLarge {
                actual: byte_len,
                maximum: MAX_CATALOG_BYTES,
            });
        }
        Self::from_bounded_file(file, MAX_CATALOG_BYTES, ValidationPolicy::production())
    }

    /// Parses a complete in-memory catalog using production validation.
    pub fn from_xml(xml: &[u8]) -> Result<Self, CatalogInventoryError> {
        let byte_len =
            u64::try_from(xml.len()).map_err(|_| CatalogInventoryError::DocumentTooLarge {
                actual: u64::MAX,
                maximum: MAX_CATALOG_BYTES,
            })?;
        if byte_len > MAX_CATALOG_BYTES {
            return Err(CatalogInventoryError::DocumentTooLarge {
                actual: byte_len,
                maximum: MAX_CATALOG_BYTES,
            });
        }
        Self::from_reader(xml, ValidationPolicy::production())
    }

    #[must_use]
    pub fn items(&self) -> &[CatalogInventoryItem] {
        &self.items
    }

    #[must_use]
    pub const fn stats(&self) -> CatalogInventoryStats {
        self.stats
    }

    /// Returns the exported parameter name associated with a kart ID.
    #[must_use]
    pub fn kart_name(&self, kart_id: u16) -> Option<&str> {
        self.kart_names.get(&kart_id).map(String::as_str)
    }

    /// Resolves a kart ID through `<Names>` to its parsed `<Specs>` snapshot.
    ///
    /// Some legitimate P5136 catalog names do not have a corresponding
    /// `BodyParam`; those IDs intentionally return `None`.
    #[must_use]
    pub fn kart_spec(&self, kart_id: u16) -> Option<&P5136KartSpecSnapshot> {
        self.kart_name(kart_id)
            .and_then(|name| self.kart_spec_by_name(name))
    }

    /// Looks up a parsed spec using the reference server's ASCII
    /// case-insensitive name behavior.
    #[must_use]
    pub fn kart_spec_by_name(&self, name: &str) -> Option<&P5136KartSpecSnapshot> {
        self.kart_specs.get(&normalize_spec_name(name))
    }

    #[must_use]
    pub const fn kart_spec_stats(&self) -> CatalogKartSpecStats {
        self.kart_spec_stats
    }

    /// Returns the optional, source-ordered `MyRoom` emblem catalog.
    ///
    /// Format-3 catalogs produced before the Rust port's `<Emblems>` extension
    /// remain valid and expose an empty slice.
    #[must_use]
    pub fn emblems(&self) -> &[i16] {
        self.emblem_catalog.as_ref().map_or(&[], EmblemCatalog::ids)
    }

    #[must_use]
    pub fn emblem_catalog(&self) -> Option<&[i16]> {
        self.emblem_catalog.as_ref().map(EmblemCatalog::ids)
    }

    #[must_use]
    pub fn emblem_definitions(&self) -> Option<&EmblemCatalog> {
        self.emblem_catalog.as_ref()
    }

    #[must_use]
    pub fn contains_emblem(&self, emblem: i16) -> bool {
        self.emblem_catalog
            .as_ref()
            .is_some_and(|catalog| catalog.contains(emblem))
    }

    pub fn category(&self, category: u16) -> impl Iterator<Item = &CatalogInventoryItem> {
        self.items
            .iter()
            .filter(move |item| item.category == category)
    }

    /// Returns whether one catalog item is safe to expose as owned inventory.
    ///
    /// Older inventory-only catalogs have no kart-name/spec metadata, so their
    /// kart grants retain the historical behavior. When a kart has an exported
    /// name, however, that name must resolve to an actual spec; exposing a
    /// named-but-unresolved kart makes the stock client's `MyRoom` search
    /// instantiate incomplete client data.
    #[must_use]
    pub fn grants_item(&self, category: u16, item_id: u16) -> bool {
        self.items.iter().any(|item| {
            item.category == category
                && item.id == item_id
                && is_grant_item(item)
                && self.has_usable_kart_spec(item)
        })
    }

    pub fn grant_items(&self) -> impl Iterator<Item = &CatalogInventoryItem> {
        self.items
            .iter()
            .filter(|item| is_grant_item(item) && self.has_usable_kart_spec(item))
    }

    fn has_usable_kart_spec(&self, item: &CatalogInventoryItem) -> bool {
        item.category != 3 || self.kart_name(item.id).is_none() || self.kart_spec(item.id).is_some()
    }

    fn from_reader<R: BufRead>(
        input: R,
        policy: ValidationPolicy,
    ) -> Result<Self, CatalogInventoryError> {
        let mut parser = CatalogParser::new(policy);
        parser.parse(input)
    }

    fn from_bounded_file(
        file: File,
        maximum: u64,
        policy: ValidationPolicy,
    ) -> Result<Self, CatalogInventoryError> {
        // Metadata is only a snapshot: the externally managed catalog can grow
        // after the check in `load`. Keep a hard read limit as well so one huge
        // text/comment event cannot make quick-xml allocate beyond the contract.
        let mut input = BufReader::new(file).take(maximum.saturating_add(1));
        let parsed = Self::from_reader(&mut input, policy);
        if input.limit() == 0 {
            return Err(CatalogInventoryError::DocumentTooLarge {
                actual: maximum.saturating_add(1),
                maximum,
            });
        }
        parsed
    }
}

#[must_use]
pub fn is_grant_category(category: u16) -> bool {
    GRANT_CATEGORY_IDS.binary_search(&category).is_ok()
}

#[must_use]
pub fn is_grant_item(item: &CatalogInventoryItem) -> bool {
    is_grant_category(item.category)
        && (item.category != 1 || UNSAFE_CHARACTER_ITEM_IDS.binary_search(&item.id).is_err())
}

#[derive(Debug, Error)]
pub enum CatalogInventoryError {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("invalid kart catalog XML: {0}")]
    Xml(#[from] quick_xml::Error),

    #[error("kart catalog XML exceeds {maximum} bytes ({actual} bytes)")]
    DocumentTooLarge { actual: u64, maximum: u64 },

    #[error("kart catalog XML contains a prohibited document type declaration")]
    DocumentType,

    #[error("kart catalog XML text exceeds {maximum} bytes")]
    TextTooLong { maximum: usize },

    #[error("kart catalog XML element contains more than {maximum} attributes")]
    TooManyAttributes { maximum: usize },

    #[error("kart catalog XML attribute value exceeds {maximum} bytes")]
    AttributeValueTooLong { maximum: usize },

    #[error("kart catalog XML has no KartCatalog root element")]
    MissingRoot,

    #[error("kart catalog XML contains more than one root element")]
    MultipleRoots,

    #[error(
        "kart catalog is not a Korean protocol 5136 format-3 catalog \
         (format={format_version:?}, protocol={protocol_version:?}, region={region:?})"
    )]
    WrongCatalog {
        format_version: Option<String>,
        protocol_version: Option<String>,
        region: Option<String>,
    },

    #[error("kart catalog XML has no Inventory element")]
    MissingInventory,

    #[error("kart catalog XML has more than one Inventory element")]
    MultipleInventories,

    #[error("kart catalog XML has more than one Names element")]
    MultipleNames,

    #[error("kart catalog XML has more than one Specs element")]
    MultipleSpecs,

    #[error("kart catalog XML has more than one Emblems element")]
    MultipleEmblems,

    #[error("kart catalog Emblems contains more than {maximum} entries")]
    TooManyEmblems { maximum: usize },

    #[error("kart catalog XML has an invalid Emblems total")]
    InvalidEmblemCount,

    #[error("kart catalog XML has an invalid Emblems/Emblem id")]
    InvalidEmblemId,

    #[error("kart catalog XML has duplicate emblem ID {id}")]
    DuplicateEmblemId { id: i16 },

    #[error("kart catalog emblem count mismatch ({declared} declared, {actual} read)")]
    EmblemCountMismatch { declared: usize, actual: usize },

    #[error("kart catalog XML has incomplete kart metadata (names={names}, specs={specs})")]
    PartialKartMetadata { names: usize, specs: usize },

    #[error("kart catalog Names contains more than {maximum} entries")]
    TooManyKartNames { maximum: usize },

    #[error("kart catalog Specs contains more than {maximum} entries")]
    TooManyKartSpecs { maximum: usize },

    #[error("kart catalog XML has an invalid Names/Kart {attribute}")]
    InvalidKartNameAttribute { attribute: &'static str },

    #[error("kart catalog kart/spec name exceeds {maximum} bytes")]
    KartNameTooLong { maximum: usize },

    #[error("kart catalog XML has duplicate kart ID {id}")]
    DuplicateKartId { id: u16 },

    #[error("kart catalog XML has an invalid Specs/Spec name")]
    InvalidKartSpecName,

    #[error("kart catalog XML has duplicate spec name {name:?}")]
    DuplicateKartSpecName { name: String },

    #[error("kart catalog spec {name:?} has no BodyParam")]
    MissingBodyParam { name: String },

    #[error("kart catalog spec {name:?} has more than one BodyParam")]
    MultipleBodyParams { name: String },

    #[error("kart catalog spec {spec:?} has an invalid {field} value")]
    InvalidBodyParamValue { spec: String, field: &'static str },

    #[error("kart catalog inventory has a missing or invalid {attribute} attribute")]
    InvalidInventoryAttribute { attribute: &'static str },

    #[error("kart catalog XML has an invalid inventory item {attribute}")]
    InvalidItemAttribute { attribute: &'static str },

    #[error("kart catalog XML has duplicate inventory item {category}:{id}")]
    DuplicateItem { category: u16, id: u16 },

    #[error("kart catalog inventory contains more than {maximum} items")]
    TooManyItems { maximum: usize },

    #[error("kart catalog inventory item name exceeds {maximum} bytes")]
    ItemNameTooLong { maximum: usize },

    #[error("kart catalog inventory count mismatch ({declared} declared, {actual} read)")]
    ItemCountMismatch { declared: usize, actual: usize },

    #[error(
        "kart catalog inventory category count mismatch \
         ({declared} declared, {actual} read)"
    )]
    CategoryCountMismatch { declared: usize, actual: usize },

    #[error("incomplete P5136 inventory catalog ({stats})")]
    Incomplete { stats: CatalogInventoryStats },

    #[error("P5136 inventory catalog is missing required kart {id}")]
    MissingSentinelKart { id: u16 },
}

#[derive(Debug, Clone, Copy)]
struct ValidationPolicy {
    minimum_items: usize,
    minimum_categories: usize,
    minimum_karts: usize,
    minimum_grant_items: usize,
    minimum_grant_categories: usize,
    require_sentinels: bool,
}

impl ValidationPolicy {
    const fn production() -> Self {
        Self {
            minimum_items: MINIMUM_INVENTORY_ITEMS,
            minimum_categories: MINIMUM_INVENTORY_CATEGORIES,
            minimum_karts: MINIMUM_INVENTORY_KARTS,
            minimum_grant_items: MINIMUM_GRANT_ITEMS,
            minimum_grant_categories: MINIMUM_GRANT_CATEGORIES,
            require_sentinels: true,
        }
    }

    #[cfg(test)]
    const fn structural_only() -> Self {
        Self {
            minimum_items: 0,
            minimum_categories: 0,
            minimum_karts: 0,
            minimum_grant_items: 0,
            minimum_grant_categories: 0,
            require_sentinels: false,
        }
    }
}

#[derive(Debug)]
struct CatalogParser {
    policy: ValidationPolicy,
    sections_seen: u8,
    active_section: Option<CatalogSection>,
    depth: usize,
    declared_items: Option<usize>,
    declared_categories: Option<usize>,
    declared_emblems: Option<usize>,
    keys: HashSet<(u16, u16)>,
    items: Vec<CatalogInventoryItem>,
    emblem_keys: HashSet<i16>,
    emblems: Vec<i16>,
    kart_names: BTreeMap<u16, String>,
    spec_keys: HashSet<String>,
    kart_specs: BTreeMap<String, P5136KartSpecSnapshot>,
    active_spec: Option<PendingKartSpec>,
}

#[derive(Debug)]
struct PendingKartSpec {
    name: String,
    normalized_name: String,
    body: Option<P5136KartSpecSnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CatalogSection {
    Inventory,
    Names,
    Specs,
    Emblems,
}

const ROOT_SEEN: u8 = 1 << 0;
const INVENTORY_SEEN: u8 = 1 << 1;
const NAMES_SEEN: u8 = 1 << 2;
const SPECS_SEEN: u8 = 1 << 3;
const EMBLEMS_SEEN: u8 = 1 << 4;

impl CatalogParser {
    fn new(policy: ValidationPolicy) -> Self {
        Self {
            policy,
            sections_seen: 0,
            active_section: None,
            depth: 0,
            declared_items: None,
            declared_categories: None,
            declared_emblems: None,
            keys: HashSet::new(),
            items: Vec::new(),
            emblem_keys: HashSet::new(),
            emblems: Vec::new(),
            kart_names: BTreeMap::new(),
            spec_keys: HashSet::new(),
            kart_specs: BTreeMap::new(),
            active_spec: None,
        }
    }

    fn parse<R: BufRead>(&mut self, input: R) -> Result<CatalogInventory, CatalogInventoryError> {
        let mut reader = Reader::from_reader(input);
        reader.config_mut().trim_text(true);
        let mut buffer = Vec::new();

        loop {
            match reader.read_event_into(&mut buffer)? {
                Event::Start(element) => {
                    validate_element_bounds(&reader, &element)?;
                    self.open_element(&reader, &element)?;
                    self.depth = self.depth.saturating_add(1);
                }
                Event::Empty(element) => {
                    validate_element_bounds(&reader, &element)?;
                    self.empty_element(&reader, &element)?;
                }
                Event::End(element) => {
                    self.depth = self.depth.saturating_sub(1);
                    if self.depth == 1
                        && matches!(
                            element.name().as_ref(),
                            b"Inventory" | b"Names" | b"Specs" | b"Emblems"
                        )
                    {
                        self.active_section = None;
                    } else if self.depth == 2 && element.name() == QName(b"Spec") {
                        self.finish_spec()?;
                    }
                }
                Event::DocType(_) => return Err(CatalogInventoryError::DocumentType),
                Event::Eof => break,
                Event::Text(text) => validate_text_bound(text.len())?,
                Event::CData(text) => validate_text_bound(text.len())?,
                Event::Decl(_) | Event::Comment(_) | Event::PI(_) | Event::GeneralRef(_) => {}
            }
            buffer.clear();
        }

        self.finish()
    }

    fn open_element<R: BufRead>(
        &mut self,
        reader: &Reader<R>,
        element: &BytesStart<'_>,
    ) -> Result<(), CatalogInventoryError> {
        if self.depth == 0 {
            self.read_root(reader, element)?;
        } else if self.depth == 1 && element.name() == QName(b"Inventory") {
            self.read_inventory_header(reader, element)?;
            self.active_section = Some(CatalogSection::Inventory);
        } else if self.depth == 1 && element.name() == QName(b"Names") {
            self.read_names_header()?;
            self.active_section = Some(CatalogSection::Names);
        } else if self.depth == 1 && element.name() == QName(b"Specs") {
            self.read_specs_header()?;
            self.active_section = Some(CatalogSection::Specs);
        } else if self.depth == 1 && element.name() == QName(b"Emblems") {
            self.read_emblems_header(reader, element)?;
            self.active_section = Some(CatalogSection::Emblems);
        } else if self.depth == 2
            && self.active_section == Some(CatalogSection::Inventory)
            && element.name() == QName(b"Item")
        {
            self.read_item(reader, element)?;
        } else if self.depth == 2
            && self.active_section == Some(CatalogSection::Names)
            && element.name() == QName(b"Kart")
        {
            self.read_kart_name(reader, element)?;
        } else if self.depth == 2
            && self.active_section == Some(CatalogSection::Specs)
            && element.name() == QName(b"Spec")
        {
            self.start_spec(reader, element)?;
        } else if self.depth == 2
            && self.active_section == Some(CatalogSection::Emblems)
            && element.name() == QName(b"Emblem")
        {
            self.read_emblem(reader, element)?;
        } else if self.active_spec.is_some() && element.name() == QName(b"BodyParam") {
            self.read_body_param(reader, element)?;
        }
        Ok(())
    }

    fn empty_element<R: BufRead>(
        &mut self,
        reader: &Reader<R>,
        element: &BytesStart<'_>,
    ) -> Result<(), CatalogInventoryError> {
        if self.depth == 0 {
            self.read_root(reader, element)?;
        } else if self.depth == 1 && element.name() == QName(b"Inventory") {
            self.read_inventory_header(reader, element)?;
        } else if self.depth == 1 && element.name() == QName(b"Names") {
            self.read_names_header()?;
        } else if self.depth == 1 && element.name() == QName(b"Specs") {
            self.read_specs_header()?;
        } else if self.depth == 1 && element.name() == QName(b"Emblems") {
            self.read_emblems_header(reader, element)?;
        } else if self.depth == 2
            && self.active_section == Some(CatalogSection::Inventory)
            && element.name() == QName(b"Item")
        {
            self.read_item(reader, element)?;
        } else if self.depth == 2
            && self.active_section == Some(CatalogSection::Names)
            && element.name() == QName(b"Kart")
        {
            self.read_kart_name(reader, element)?;
        } else if self.depth == 2
            && self.active_section == Some(CatalogSection::Specs)
            && element.name() == QName(b"Spec")
        {
            self.start_spec(reader, element)?;
            self.finish_spec()?;
        } else if self.depth == 2
            && self.active_section == Some(CatalogSection::Emblems)
            && element.name() == QName(b"Emblem")
        {
            self.read_emblem(reader, element)?;
        } else if self.active_spec.is_some() && element.name() == QName(b"BodyParam") {
            self.read_body_param(reader, element)?;
        }
        Ok(())
    }

    fn read_root<R: BufRead>(
        &mut self,
        reader: &Reader<R>,
        element: &BytesStart<'_>,
    ) -> Result<(), CatalogInventoryError> {
        if self.sections_seen & ROOT_SEEN != 0 {
            return Err(CatalogInventoryError::MultipleRoots);
        }
        self.sections_seen |= ROOT_SEEN;

        let format_version = attribute(reader, element, b"formatVersion")?;
        let protocol_version = attribute(reader, element, b"protocolVersion")?;
        let region = attribute(reader, element, b"region")?;
        if element.name() != QName(b"KartCatalog")
            || format_version.as_deref() != Some(CATALOG_FORMAT_VERSION)
            || protocol_version.as_deref() != Some(CATALOG_PROTOCOL_VERSION)
            || region.as_deref() != Some(CATALOG_REGION)
        {
            return Err(CatalogInventoryError::WrongCatalog {
                format_version,
                protocol_version,
                region,
            });
        }
        Ok(())
    }

    fn read_inventory_header<R: BufRead>(
        &mut self,
        reader: &Reader<R>,
        element: &BytesStart<'_>,
    ) -> Result<(), CatalogInventoryError> {
        if self.sections_seen & INVENTORY_SEEN != 0 {
            return Err(CatalogInventoryError::MultipleInventories);
        }
        self.sections_seen |= INVENTORY_SEEN;
        self.declared_items = Some(parse_usize_attribute(reader, element, b"total", "total")?);
        self.declared_categories = Some(parse_usize_attribute(
            reader,
            element,
            b"categories",
            "categories",
        )?);
        Ok(())
    }

    fn read_names_header(&mut self) -> Result<(), CatalogInventoryError> {
        if self.sections_seen & NAMES_SEEN != 0 {
            return Err(CatalogInventoryError::MultipleNames);
        }
        self.sections_seen |= NAMES_SEEN;
        Ok(())
    }

    fn read_specs_header(&mut self) -> Result<(), CatalogInventoryError> {
        if self.sections_seen & SPECS_SEEN != 0 {
            return Err(CatalogInventoryError::MultipleSpecs);
        }
        self.sections_seen |= SPECS_SEEN;
        Ok(())
    }

    fn read_emblems_header<R: BufRead>(
        &mut self,
        reader: &Reader<R>,
        element: &BytesStart<'_>,
    ) -> Result<(), CatalogInventoryError> {
        if self.sections_seen & EMBLEMS_SEEN != 0 {
            return Err(CatalogInventoryError::MultipleEmblems);
        }
        self.sections_seen |= EMBLEMS_SEEN;
        self.declared_emblems = match attribute(reader, element, b"total")? {
            Some(value) => {
                let total = value
                    .parse::<usize>()
                    .map_err(|_| CatalogInventoryError::InvalidEmblemCount)?;
                if total > MAX_CATALOG_EMBLEMS {
                    return Err(CatalogInventoryError::TooManyEmblems {
                        maximum: MAX_CATALOG_EMBLEMS,
                    });
                }
                Some(total)
            }
            None => None,
        };
        Ok(())
    }

    fn read_emblem<R: BufRead>(
        &mut self,
        reader: &Reader<R>,
        element: &BytesStart<'_>,
    ) -> Result<(), CatalogInventoryError> {
        if self.emblems.len() >= MAX_CATALOG_EMBLEMS {
            return Err(CatalogInventoryError::TooManyEmblems {
                maximum: MAX_CATALOG_EMBLEMS,
            });
        }
        let id = attribute(reader, element, b"id")?
            .and_then(|value| value.parse::<i16>().ok())
            .filter(|id| *id > 0)
            .ok_or(CatalogInventoryError::InvalidEmblemId)?;
        if !self.emblem_keys.insert(id) {
            return Err(CatalogInventoryError::DuplicateEmblemId { id });
        }
        self.emblems.push(id);
        Ok(())
    }

    fn read_kart_name<R: BufRead>(
        &mut self,
        reader: &Reader<R>,
        element: &BytesStart<'_>,
    ) -> Result<(), CatalogInventoryError> {
        if self.kart_names.len() >= MAX_KART_NAMES {
            return Err(CatalogInventoryError::TooManyKartNames {
                maximum: MAX_KART_NAMES,
            });
        }

        let id = attribute(reader, element, b"id")?
            .and_then(|value| value.trim().parse::<u16>().ok())
            .filter(|id| *id != 0)
            .ok_or(CatalogInventoryError::InvalidKartNameAttribute { attribute: "id" })?;
        let name = attribute(reader, element, b"name")?
            .filter(|name| !name.trim().is_empty())
            .ok_or(CatalogInventoryError::InvalidKartNameAttribute { attribute: "name" })?;
        validate_kart_name_length(&name)?;

        if self.kart_names.insert(id, name).is_some() {
            return Err(CatalogInventoryError::DuplicateKartId { id });
        }
        Ok(())
    }

    fn start_spec<R: BufRead>(
        &mut self,
        reader: &Reader<R>,
        element: &BytesStart<'_>,
    ) -> Result<(), CatalogInventoryError> {
        if self.active_spec.is_some() {
            return Err(CatalogInventoryError::InvalidKartSpecName);
        }
        if self.kart_specs.len() >= MAX_KART_SPECS {
            return Err(CatalogInventoryError::TooManyKartSpecs {
                maximum: MAX_KART_SPECS,
            });
        }

        let name = attribute(reader, element, b"name")?
            .filter(|name| !name.trim().is_empty())
            .ok_or(CatalogInventoryError::InvalidKartSpecName)?;
        validate_kart_name_length(&name)?;
        let normalized_name = normalize_spec_name(&name);
        if !self.spec_keys.insert(normalized_name.clone()) {
            return Err(CatalogInventoryError::DuplicateKartSpecName { name });
        }

        self.active_spec = Some(PendingKartSpec {
            name,
            normalized_name,
            body: None,
        });
        Ok(())
    }

    fn read_body_param<R: BufRead>(
        &mut self,
        reader: &Reader<R>,
        element: &BytesStart<'_>,
    ) -> Result<(), CatalogInventoryError> {
        let pending = self
            .active_spec
            .as_mut()
            .ok_or(CatalogInventoryError::InvalidKartSpecName)?;
        if pending.body.is_some() {
            return Err(CatalogInventoryError::MultipleBodyParams {
                name: pending.name.clone(),
            });
        }
        pending.body = Some(parse_body_param(reader, element, &pending.name)?);
        Ok(())
    }

    fn finish_spec(&mut self) -> Result<(), CatalogInventoryError> {
        let pending = self
            .active_spec
            .take()
            .ok_or(CatalogInventoryError::InvalidKartSpecName)?;
        let body = pending
            .body
            .ok_or_else(|| CatalogInventoryError::MissingBodyParam {
                name: pending.name.clone(),
            })?;
        self.kart_specs.insert(pending.normalized_name, body);
        Ok(())
    }

    fn read_item<R: BufRead>(
        &mut self,
        reader: &Reader<R>,
        element: &BytesStart<'_>,
    ) -> Result<(), CatalogInventoryError> {
        if self.items.len() >= MAX_CATALOG_ITEMS {
            return Err(CatalogInventoryError::TooManyItems {
                maximum: MAX_CATALOG_ITEMS,
            });
        }

        let category = parse_u16_attribute(reader, element, b"category", "category", true)?;
        let id = parse_u16_attribute(reader, element, b"id", "id", false)?;
        let serial =
            match attribute(reader, element, b"serial")? {
                Some(value) => value.parse::<u16>().map_err(|_| {
                    CatalogInventoryError::InvalidItemAttribute {
                        attribute: "serial",
                    }
                })?,
                None => 0,
            };
        let name = attribute(reader, element, b"name")?.unwrap_or_default();
        if name.len() > MAX_ITEM_NAME_BYTES {
            return Err(CatalogInventoryError::ItemNameTooLong {
                maximum: MAX_ITEM_NAME_BYTES,
            });
        }
        if !self.keys.insert((category, id)) {
            return Err(CatalogInventoryError::DuplicateItem { category, id });
        }
        self.items.push(CatalogInventoryItem {
            category,
            id,
            serial,
            name,
        });
        Ok(())
    }

    // Keep the cross-section validation together so publication remains one
    // auditable all-or-nothing operation.
    #[allow(clippy::too_many_lines)]
    fn finish(&mut self) -> Result<CatalogInventory, CatalogInventoryError> {
        if self.sections_seen & ROOT_SEEN == 0 {
            return Err(CatalogInventoryError::MissingRoot);
        }
        if self.sections_seen & INVENTORY_SEEN == 0 {
            return Err(CatalogInventoryError::MissingInventory);
        }
        if self.active_spec.is_some() {
            return Err(CatalogInventoryError::InvalidKartSpecName);
        }
        if self.kart_names.is_empty() != self.kart_specs.is_empty() {
            return Err(CatalogInventoryError::PartialKartMetadata {
                names: self.kart_names.len(),
                specs: self.kart_specs.len(),
            });
        }
        if let Some(declared) = self.declared_emblems
            && declared != self.emblems.len()
        {
            return Err(CatalogInventoryError::EmblemCountMismatch {
                declared,
                actual: self.emblems.len(),
            });
        }

        let declared_items = self
            .declared_items
            .ok_or(CatalogInventoryError::InvalidInventoryAttribute { attribute: "total" })?;
        if declared_items != self.items.len() {
            return Err(CatalogInventoryError::ItemCountMismatch {
                declared: declared_items,
                actual: self.items.len(),
            });
        }

        self.items
            .sort_unstable_by_key(|item| (item.category, item.id));
        let categories = self
            .items
            .iter()
            .map(|item| item.category)
            .collect::<BTreeSet<_>>();
        let declared_categories =
            self.declared_categories
                .ok_or(CatalogInventoryError::InvalidInventoryAttribute {
                    attribute: "categories",
                })?;
        if declared_categories != categories.len() {
            return Err(CatalogInventoryError::CategoryCountMismatch {
                declared: declared_categories,
                actual: categories.len(),
            });
        }

        let karts = self.items.iter().filter(|item| item.category == 3).count();
        let grant_items = self.items.iter().filter(|item| is_grant_item(item)).count();
        let grant_categories = self
            .items
            .iter()
            .filter(|item| is_grant_item(item))
            .map(|item| item.category)
            .collect::<BTreeSet<_>>()
            .len();
        let stats = CatalogInventoryStats {
            items: self.items.len(),
            categories: categories.len(),
            karts,
            grant_items,
            grant_categories,
            emblems: self.emblems.len(),
        };
        if stats.items < self.policy.minimum_items
            || stats.categories < self.policy.minimum_categories
            || stats.karts < self.policy.minimum_karts
            || stats.grant_items < self.policy.minimum_grant_items
            || stats.grant_categories < self.policy.minimum_grant_categories
        {
            return Err(CatalogInventoryError::Incomplete { stats });
        }
        if self.policy.require_sentinels {
            for id in [1_450, 1_453] {
                if !self
                    .items
                    .iter()
                    .any(|item| item.category == 3 && item.id == id)
                {
                    return Err(CatalogInventoryError::MissingSentinelKart { id });
                }
            }
        }

        let referenced_specs = self
            .kart_names
            .values()
            .map(|name| normalize_spec_name(name))
            .collect::<BTreeSet<_>>();
        let resolved_names = self
            .kart_names
            .values()
            .filter(|name| self.kart_specs.contains_key(&normalize_spec_name(name)))
            .count();
        let kart_spec_stats = CatalogKartSpecStats {
            names: self.kart_names.len(),
            specs: self.kart_specs.len(),
            resolved_names,
            unresolved_names: self.kart_names.len() - resolved_names,
            unreferenced_specs: self
                .kart_specs
                .keys()
                .filter(|name| !referenced_specs.contains(*name))
                .count(),
        };

        let emblem_catalog = (self.sections_seen & EMBLEMS_SEEN != 0).then(|| {
            EmblemCatalog::from_validated_parts(
                std::mem::take(&mut self.emblems),
                std::mem::take(&mut self.emblem_keys),
            )
        });
        Ok(CatalogInventory {
            items: std::mem::take(&mut self.items),
            stats,
            kart_names: std::mem::take(&mut self.kart_names),
            kart_specs: std::mem::take(&mut self.kart_specs),
            kart_spec_stats,
            emblem_catalog,
        })
    }
}

fn normalize_spec_name(name: &str) -> String {
    name.to_ascii_lowercase()
}

fn validate_kart_name_length(name: &str) -> Result<(), CatalogInventoryError> {
    if name.len() > MAX_KART_NAME_BYTES {
        Err(CatalogInventoryError::KartNameTooLong {
            maximum: MAX_KART_NAME_BYTES,
        })
    } else {
        Ok(())
    }
}

fn validate_text_bound(length: usize) -> Result<(), CatalogInventoryError> {
    if length > MAX_XML_TEXT_BYTES {
        Err(CatalogInventoryError::TextTooLong {
            maximum: MAX_XML_TEXT_BYTES,
        })
    } else {
        Ok(())
    }
}

fn validate_element_bounds<R: BufRead>(
    reader: &Reader<R>,
    element: &BytesStart<'_>,
) -> Result<(), CatalogInventoryError> {
    let mut count = 0_usize;
    for result in element.attributes() {
        let attribute = result.map_err(quick_xml::Error::from)?;
        count = count.saturating_add(1);
        if count > MAX_XML_ATTRIBUTES_PER_ELEMENT {
            return Err(CatalogInventoryError::TooManyAttributes {
                maximum: MAX_XML_ATTRIBUTES_PER_ELEMENT,
            });
        }
        let value: Cow<'_, str> =
            attribute.decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())?;
        if value.len() > MAX_XML_ATTRIBUTE_VALUE_BYTES {
            return Err(CatalogInventoryError::AttributeValueTooLong {
                maximum: MAX_XML_ATTRIBUTE_VALUE_BYTES,
            });
        }
    }
    Ok(())
}

// This deliberately mirrors the ordered C# KartSpecConfigs table one field at
// a time. Splitting it would make default/fallback/scale drift harder to audit.
#[allow(clippy::too_many_lines)]
fn parse_body_param<R: BufRead>(
    reader: &Reader<R>,
    element: &BytesStart<'_>,
    spec_name: &str,
) -> Result<P5136KartSpecSnapshot, CatalogInventoryError> {
    let mut attributes = HashMap::new();
    for result in element.attributes() {
        let attribute = result.map_err(quick_xml::Error::from)?;
        let Some(field) = known_body_param_field(attribute.key.as_ref()) else {
            continue;
        };
        let value: Cow<'_, str> =
            attribute.decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())?;
        attributes.insert(field, value.into_owned());
    }

    let mut snapshot = P5136KartSpecSnapshot::csharp_default();

    macro_rules! float_field {
        ($member:ident, $xml:literal, $fallback:expr, $default:expr, $scale:expr) => {
            snapshot.$member =
                resolve_decimal_field(&attributes, $xml, $fallback, $default, $scale, spec_name)?
                    .to_f32();
        };
    }
    macro_rules! int_field {
        ($member:ident, $xml:literal, $fallback:expr, $default:expr, $scale:expr) => {
            snapshot.$member =
                resolve_decimal_field(&attributes, $xml, $fallback, $default, $scale, spec_name)?
                    .to_i32()
                    .ok_or_else(|| CatalogInventoryError::InvalidBodyParamValue {
                        spec: spec_name.to_owned(),
                        field: $xml,
                    })?;
        };
    }
    macro_rules! byte_field {
        ($member:ident, $xml:literal, $fallback:expr, $default:expr, $scale:expr) => {
            snapshot.$member =
                resolve_decimal_field(&attributes, $xml, $fallback, $default, $scale, spec_name)?
                    .to_u8()
                    .ok_or_else(|| CatalogInventoryError::InvalidBodyParamValue {
                        spec: spec_name.to_owned(),
                        field: $xml,
                    })?;
        };
    }
    macro_rules! bool_field {
        ($member:ident, $xml:literal, $default:expr) => {
            snapshot.$member = resolve_bool_field(&attributes, $xml, $default);
        };
    }

    float_field!(
        draft_mul_accel_factor,
        "draftMulAccelFactor",
        DECIMAL_ZERO,
        DECIMAL_ONE,
        DECIMAL_ONE
    );
    int_field!(
        draft_tick,
        "draftTick",
        DECIMAL_ZERO,
        DECIMAL_ZERO,
        DECIMAL_ONE
    );
    float_field!(
        drift_boost_mul_accel_factor,
        "driftBoostMulAccelFactor",
        DECIMAL_ZERO,
        decimal_literal(131, 2, false),
        DECIMAL_ONE
    );
    int_field!(
        drift_boost_tick,
        "driftBoostTick",
        DECIMAL_ZERO,
        DECIMAL_ZERO,
        DECIMAL_ONE
    );
    float_field!(
        charge_boost_by_speed,
        "chargeBoostBySpeed",
        DECIMAL_ZERO,
        decimal_literal(2, 0, false),
        DECIMAL_ONE
    );
    byte_field!(
        speed_slot_capacity,
        "SpeedSlotCapacity",
        DECIMAL_ZERO,
        decimal_literal(2, 0, false),
        DECIMAL_ONE
    );
    byte_field!(
        item_slot_capacity,
        "ItemSlotCapacity",
        DECIMAL_ZERO,
        decimal_literal(2, 0, false),
        DECIMAL_ONE
    );
    byte_field!(
        special_slot_capacity,
        "SpecialSlotCapacity",
        DECIMAL_ZERO,
        DECIMAL_ONE,
        DECIMAL_ONE
    );
    bool_field!(use_transform_booster, "UseTransformBooster", false);
    bool_field!(motorcycle_type, "motorcycleType", false);
    bool_field!(bike_rear_wheel, "BikeRearWheel", true);
    float_field!(mass, "Mass", DECIMAL_ZERO, DECIMAL_ZERO, DECIMAL_ONE);
    float_field!(
        air_friction,
        "AirFriction",
        DECIMAL_ZERO,
        DECIMAL_ZERO,
        DECIMAL_ONE
    );
    float_field!(
        drag_factor,
        "DragFactor",
        DECIMAL_ZERO,
        DECIMAL_ZERO,
        DECIMAL_ONE
    );
    float_field!(
        forward_accel_force,
        "ForwardAccelForce",
        DECIMAL_ZERO,
        DECIMAL_ZERO,
        DECIMAL_ONE
    );
    float_field!(
        backward_accel_force,
        "BackwardAccelForce",
        DECIMAL_ZERO,
        DECIMAL_ZERO,
        DECIMAL_ONE
    );
    float_field!(
        grip_brake_force,
        "GripBrakeForce",
        DECIMAL_ZERO,
        DECIMAL_ZERO,
        DECIMAL_ONE
    );
    float_field!(
        slip_brake_force,
        "SlipBrakeForce",
        DECIMAL_ZERO,
        DECIMAL_ZERO,
        DECIMAL_ONE
    );
    float_field!(
        max_steer_angle,
        "MaxSteerAngle",
        DECIMAL_ZERO,
        DECIMAL_ZERO,
        DECIMAL_ONE
    );
    float_field!(
        steer_constraint,
        "SteerConstraint",
        DECIMAL_ZERO,
        DECIMAL_ZERO,
        DECIMAL_ONE
    );
    float_field!(
        front_grip_factor,
        "FrontGripFactor",
        DECIMAL_ZERO,
        DECIMAL_ZERO,
        DECIMAL_ONE
    );
    float_field!(
        rear_grip_factor,
        "RearGripFactor",
        DECIMAL_ZERO,
        DECIMAL_ZERO,
        DECIMAL_ONE
    );
    float_field!(
        drift_trigger_factor,
        "DriftTriggerFactor",
        DECIMAL_ZERO,
        DECIMAL_ZERO,
        DECIMAL_ONE
    );
    float_field!(
        drift_trigger_time,
        "DriftTriggerTime",
        DECIMAL_ZERO,
        DECIMAL_ZERO,
        DECIMAL_ONE
    );
    float_field!(
        drift_slip_factor,
        "DriftSlipFactor",
        DECIMAL_ZERO,
        DECIMAL_ZERO,
        DECIMAL_ONE
    );
    float_field!(
        drift_escape_force,
        "DriftEscapeForce",
        DECIMAL_ZERO,
        DECIMAL_ZERO,
        DECIMAL_ONE
    );
    float_field!(
        corner_draw_factor,
        "CornerDrawFactor",
        DECIMAL_ZERO,
        DECIMAL_ZERO,
        DECIMAL_ONE
    );
    float_field!(
        drift_lean_factor,
        "DriftLeanFactor",
        decimal_literal(7, 2, false),
        decimal_literal(7, 2, false),
        DECIMAL_ONE
    );
    float_field!(
        steer_lean_factor,
        "SteerLeanFactor",
        decimal_literal(1, 2, false),
        decimal_literal(1, 2, false),
        DECIMAL_ONE
    );
    float_field!(
        drift_max_gauge,
        "DriftMaxGauge",
        DECIMAL_ZERO,
        DECIMAL_ZERO,
        DECIMAL_ONE
    );
    float_field!(
        normal_booster_time,
        "NormalBoosterTime",
        decimal_literal(3_000, 0, false),
        decimal_literal(3_000, 0, false),
        DECIMAL_ONE
    );
    float_field!(
        item_booster_time,
        "ItemBoosterTime",
        decimal_literal(3_000, 0, false),
        decimal_literal(3_000, 0, false),
        DECIMAL_ONE
    );
    float_field!(
        team_booster_time,
        "TeamBoosterTime",
        decimal_literal(4_500, 0, false),
        decimal_literal(4_500, 0, false),
        DECIMAL_ONE
    );
    float_field!(
        animal_booster_time,
        "AnimalBoosterTime",
        decimal_literal(4_000, 0, false),
        decimal_literal(4_000, 0, false),
        DECIMAL_ONE
    );
    float_field!(
        super_booster_time,
        "SuperBoosterTime",
        decimal_literal(3_500, 0, false),
        decimal_literal(3_500, 0, false),
        DECIMAL_ONE
    );
    float_field!(
        trans_accel_factor,
        "TransAccelFactor",
        DECIMAL_ZERO,
        decimal_literal(15, 1, false),
        DECIMAL_ONE
    );
    float_field!(
        boost_accel_factor,
        "BoostAccelFactor",
        DECIMAL_ZERO,
        decimal_literal(15, 1, false),
        DECIMAL_ONE
    );
    float_field!(
        start_booster_time_item,
        "StartBoosterTimeItem",
        DECIMAL_ZERO,
        decimal_literal(1_000, 0, false),
        DECIMAL_ONE
    );
    float_field!(
        start_booster_time_speed,
        "StartBoosterTimeSpeed",
        DECIMAL_ZERO,
        decimal_literal(1_000, 0, false),
        DECIMAL_ONE
    );
    float_field!(
        start_forward_accel_factor_item,
        "StartForwardAccelFactorItem",
        DECIMAL_ZERO,
        DECIMAL_ZERO,
        DECIMAL_ONE
    );
    float_field!(
        start_forward_accel_factor_speed,
        "StartForwardAccelFactorSpeed",
        DECIMAL_ZERO,
        DECIMAL_ZERO,
        DECIMAL_ONE
    );
    float_field!(
        drift_gauge_preserve_percent,
        "DriftGaguePreservePercent",
        DECIMAL_ZERO,
        DECIMAL_ZERO,
        DECIMAL_ONE
    );
    bool_field!(use_extended_after_booster, "UseExtendedAfterBooster", false);
    float_field!(
        boost_accel_factor_only_item,
        "BoostAccelFactorOnlyItem",
        DECIMAL_ZERO,
        decimal_literal(15, 1, false),
        DECIMAL_ONE
    );
    float_field!(
        anti_collide_balance,
        "antiCollideBalance",
        DECIMAL_ZERO,
        DECIMAL_ONE,
        DECIMAL_ONE
    );
    bool_field!(dual_booster_set_auto, "dualBoosterSetAuto", false);
    int_field!(
        dual_booster_tick_min,
        "dualBoosterTickMin",
        DECIMAL_ZERO,
        decimal_literal(40, 0, false),
        DECIMAL_ONE
    );
    int_field!(
        dual_booster_tick_max,
        "dualBoosterTickMax",
        DECIMAL_ZERO,
        decimal_literal(60, 0, false),
        DECIMAL_ONE
    );
    float_field!(
        dual_mul_accel_factor,
        "dualMulAccelFactor",
        DECIMAL_ZERO,
        decimal_literal(11, 1, false),
        DECIMAL_ONE
    );
    float_field!(
        dual_trans_low_speed,
        "dualTransLowSpeed",
        DECIMAL_ZERO,
        decimal_literal(100, 0, false),
        DECIMAL_ONE
    );
    bool_field!(parts_engine_lock, "PartsEngineLock", false);
    bool_field!(parts_wheel_lock, "PartsWheelLock", false);
    bool_field!(parts_steering_lock, "PartsSteeringLock", false);
    bool_field!(parts_booster_lock, "PartsBoosterLock", false);
    bool_field!(parts_coating_lock, "PartsCoatingLock", false);
    bool_field!(parts_tail_lamp_lock, "PartsTailLampLock", false);
    float_field!(
        charge_inst_accel_gauge_by_boost,
        "chargeInstAccelGaugeByBoost",
        DECIMAL_ZERO,
        decimal_literal(2, 2, false),
        DECIMAL_ONE
    );
    float_field!(
        charge_inst_accel_gauge_by_grip,
        "chargeInstAccelGaugeByGrip",
        DECIMAL_ZERO,
        decimal_literal(2, 2, false),
        DECIMAL_ONE
    );
    float_field!(
        charge_inst_accel_gauge_by_wall,
        "chargeInstAccelGaugeByWall",
        DECIMAL_ZERO,
        decimal_literal(2, 1, false),
        DECIMAL_ONE
    );
    float_field!(
        inst_accel_factor,
        "instAccelFactor",
        DECIMAL_ZERO,
        decimal_literal(125, 2, false),
        DECIMAL_ONE
    );
    int_field!(
        inst_accel_gauge_cooldown_time,
        "instAccelGaugeCooldownTime",
        DECIMAL_ZERO,
        DECIMAL_ZERO,
        DECIMAL_THOUSAND
    );
    float_field!(
        inst_accel_gauge_length,
        "instAccelGaugeLength",
        DECIMAL_ZERO,
        DECIMAL_ZERO,
        DECIMAL_THOUSAND
    );

    let raw_length = attributes
        .get("instAccelGaugeLength")
        .and_then(|value| DotNetDecimal::parse_invariant(value))
        .unwrap_or(DECIMAL_ZERO);
    let minimum_usable_scale = DECIMAL_THOUSAND.checked_mul(raw_length).ok_or_else(|| {
        CatalogInventoryError::InvalidBodyParamValue {
            spec: spec_name.to_owned(),
            field: "instAccelGaugeMinUsable",
        }
    })?;
    snapshot.inst_accel_gauge_min_usable = resolve_decimal_field(
        &attributes,
        "instAccelGaugeMinUsable",
        DECIMAL_ZERO,
        DECIMAL_ZERO,
        minimum_usable_scale,
        spec_name,
    )?
    .to_f32();

    float_field!(
        inst_accel_gauge_min_vel_bound,
        "instAccelGaugeMinVelBound",
        DECIMAL_ZERO,
        decimal_literal(200, 0, false),
        DECIMAL_ONE
    );
    float_field!(
        inst_accel_gauge_min_vel_loss,
        "instAccelGaugeMinVelLoss",
        DECIMAL_ZERO,
        decimal_literal(50, 0, false),
        DECIMAL_ONE
    );
    bool_field!(
        use_extended_after_booster_more,
        "useExtendedAfterBoosterMore",
        false
    );
    int_field!(
        wall_coll_gauge_cooldown_time,
        "wallCollGaugeCooldownTime",
        DECIMAL_ZERO,
        DECIMAL_ZERO,
        DECIMAL_THOUSAND
    );
    float_field!(
        wall_coll_gauge_max_vel_loss,
        "wallCollGaugeMaxVelLoss",
        DECIMAL_ZERO,
        decimal_literal(200, 0, false),
        DECIMAL_ONE
    );
    float_field!(
        wall_coll_gauge_min_vel_bound,
        "wallCollGaugeMinVelBound",
        DECIMAL_ZERO,
        decimal_literal(200, 0, false),
        DECIMAL_ONE
    );
    float_field!(
        wall_coll_gauge_min_vel_loss,
        "wallCollGaugeMinVelLoss",
        DECIMAL_ZERO,
        decimal_literal(50, 0, false),
        DECIMAL_ONE
    );

    Ok(snapshot)
}

fn known_body_param_field(name: &[u8]) -> Option<&'static str> {
    Some(match name {
        b"draftMulAccelFactor" => "draftMulAccelFactor",
        b"draftTick" => "draftTick",
        b"driftBoostMulAccelFactor" => "driftBoostMulAccelFactor",
        b"driftBoostTick" => "driftBoostTick",
        b"chargeBoostBySpeed" => "chargeBoostBySpeed",
        b"SpeedSlotCapacity" => "SpeedSlotCapacity",
        b"ItemSlotCapacity" => "ItemSlotCapacity",
        b"SpecialSlotCapacity" => "SpecialSlotCapacity",
        b"UseTransformBooster" => "UseTransformBooster",
        b"motorcycleType" => "motorcycleType",
        b"BikeRearWheel" => "BikeRearWheel",
        b"Mass" => "Mass",
        b"AirFriction" => "AirFriction",
        b"DragFactor" => "DragFactor",
        b"ForwardAccelForce" => "ForwardAccelForce",
        b"BackwardAccelForce" => "BackwardAccelForce",
        b"GripBrakeForce" => "GripBrakeForce",
        b"SlipBrakeForce" => "SlipBrakeForce",
        b"MaxSteerAngle" => "MaxSteerAngle",
        b"SteerConstraint" => "SteerConstraint",
        b"FrontGripFactor" => "FrontGripFactor",
        b"RearGripFactor" => "RearGripFactor",
        b"DriftTriggerFactor" => "DriftTriggerFactor",
        b"DriftTriggerTime" => "DriftTriggerTime",
        b"DriftSlipFactor" => "DriftSlipFactor",
        b"DriftEscapeForce" => "DriftEscapeForce",
        b"CornerDrawFactor" => "CornerDrawFactor",
        b"DriftLeanFactor" => "DriftLeanFactor",
        b"SteerLeanFactor" => "SteerLeanFactor",
        b"DriftMaxGauge" => "DriftMaxGauge",
        b"NormalBoosterTime" => "NormalBoosterTime",
        b"ItemBoosterTime" => "ItemBoosterTime",
        b"TeamBoosterTime" => "TeamBoosterTime",
        b"AnimalBoosterTime" => "AnimalBoosterTime",
        b"SuperBoosterTime" => "SuperBoosterTime",
        b"TransAccelFactor" => "TransAccelFactor",
        b"BoostAccelFactor" => "BoostAccelFactor",
        b"StartBoosterTimeItem" => "StartBoosterTimeItem",
        b"StartBoosterTimeSpeed" => "StartBoosterTimeSpeed",
        b"StartForwardAccelFactorItem" => "StartForwardAccelFactorItem",
        b"StartForwardAccelFactorSpeed" => "StartForwardAccelFactorSpeed",
        b"DriftGaguePreservePercent" => "DriftGaguePreservePercent",
        b"UseExtendedAfterBooster" => "UseExtendedAfterBooster",
        b"BoostAccelFactorOnlyItem" => "BoostAccelFactorOnlyItem",
        b"antiCollideBalance" => "antiCollideBalance",
        b"dualBoosterSetAuto" => "dualBoosterSetAuto",
        b"dualBoosterTickMin" => "dualBoosterTickMin",
        b"dualBoosterTickMax" => "dualBoosterTickMax",
        b"dualMulAccelFactor" => "dualMulAccelFactor",
        b"dualTransLowSpeed" => "dualTransLowSpeed",
        b"PartsEngineLock" => "PartsEngineLock",
        b"PartsWheelLock" => "PartsWheelLock",
        b"PartsSteeringLock" => "PartsSteeringLock",
        b"PartsBoosterLock" => "PartsBoosterLock",
        b"PartsCoatingLock" => "PartsCoatingLock",
        b"PartsTailLampLock" => "PartsTailLampLock",
        b"chargeInstAccelGaugeByBoost" => "chargeInstAccelGaugeByBoost",
        b"chargeInstAccelGaugeByGrip" => "chargeInstAccelGaugeByGrip",
        b"chargeInstAccelGaugeByWall" => "chargeInstAccelGaugeByWall",
        b"instAccelFactor" => "instAccelFactor",
        b"instAccelGaugeCooldownTime" => "instAccelGaugeCooldownTime",
        b"instAccelGaugeLength" => "instAccelGaugeLength",
        b"instAccelGaugeMinUsable" => "instAccelGaugeMinUsable",
        b"instAccelGaugeMinVelBound" => "instAccelGaugeMinVelBound",
        b"instAccelGaugeMinVelLoss" => "instAccelGaugeMinVelLoss",
        b"useExtendedAfterBoosterMore" => "useExtendedAfterBoosterMore",
        b"wallCollGaugeCooldownTime" => "wallCollGaugeCooldownTime",
        b"wallCollGaugeMaxVelLoss" => "wallCollGaugeMaxVelLoss",
        b"wallCollGaugeMinVelBound" => "wallCollGaugeMinVelBound",
        b"wallCollGaugeMinVelLoss" => "wallCollGaugeMinVelLoss",
        _ => return None,
    })
}

fn resolve_bool_field(
    attributes: &HashMap<&'static str, String>,
    field: &'static str,
    default: bool,
) -> u8 {
    let value = attributes.get(field).map_or(default, |value| {
        if value.trim().eq_ignore_ascii_case("true") {
            true
        } else if value.trim().eq_ignore_ascii_case("false") {
            false
        } else {
            default
        }
    });
    u8::from(value)
}

fn resolve_decimal_field(
    attributes: &HashMap<&'static str, String>,
    field: &'static str,
    fallback: DotNetDecimal,
    default: DotNetDecimal,
    scale: DotNetDecimal,
    spec_name: &str,
) -> Result<DotNetDecimal, CatalogInventoryError> {
    let Some(value) = attributes
        .get(field)
        .and_then(|value| DotNetDecimal::parse_invariant(value))
    else {
        return Ok(default);
    };
    value
        .checked_mul(scale)
        .and_then(|value| value.checked_add(fallback))
        .ok_or_else(|| CatalogInventoryError::InvalidBodyParamValue {
            spec: spec_name.to_owned(),
            field,
        })
}

const DECIMAL_ZERO: DotNetDecimal = DotNetDecimal::ZERO;
const DECIMAL_ONE: DotNetDecimal = DotNetDecimal::ONE;
const DECIMAL_THOUSAND: DotNetDecimal = decimal_literal(1_000, 0, false);

const fn decimal_literal(mantissa: u128, scale: u32, negative: bool) -> DotNetDecimal {
    match DotNetDecimal::from_parts(mantissa, scale, negative) {
        Some(value) => value,
        None => panic!("P5136 decimal literal fits System.Decimal"),
    }
}

fn attribute<R: BufRead>(
    reader: &Reader<R>,
    element: &BytesStart<'_>,
    name: &[u8],
) -> Result<Option<String>, CatalogInventoryError> {
    for result in element.attributes() {
        let attribute = result.map_err(quick_xml::Error::from)?;
        if attribute.key == QName(name) {
            let value: Cow<'_, str> = attribute
                .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())?;
            return Ok(Some(value.into_owned()));
        }
    }
    Ok(None)
}

fn parse_usize_attribute<R: BufRead>(
    reader: &Reader<R>,
    element: &BytesStart<'_>,
    name: &[u8],
    display_name: &'static str,
) -> Result<usize, CatalogInventoryError> {
    attribute(reader, element, name)?
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or(CatalogInventoryError::InvalidInventoryAttribute {
            attribute: display_name,
        })
}

fn parse_u16_attribute<R: BufRead>(
    reader: &Reader<R>,
    element: &BytesStart<'_>,
    name: &[u8],
    display_name: &'static str,
    zero_allowed: bool,
) -> Result<u16, CatalogInventoryError> {
    let value = attribute(reader, element, name)?
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or(CatalogInventoryError::InvalidItemAttribute {
            attribute: display_name,
        })?;
    if !zero_allowed && value == 0 {
        return Err(CatalogInventoryError::InvalidItemAttribute {
            attribute: display_name,
        });
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use std::{fmt::Write as _, fs, fs::File};

    use tempfile::tempdir;

    use super::{
        CatalogInventory, CatalogInventoryError, CatalogInventoryItem, CatalogParser,
        MAX_CATALOG_EMBLEMS, MAX_KART_NAME_BYTES, MAX_XML_ATTRIBUTE_VALUE_BYTES,
        MAX_XML_ATTRIBUTES_PER_ELEMENT, MAX_XML_TEXT_BYTES, ValidationPolicy, is_grant_category,
        is_grant_item,
    };

    fn parse_structural(xml: &str) -> Result<CatalogInventory, CatalogInventoryError> {
        CatalogParser::new(ValidationPolicy::structural_only()).parse(xml.as_bytes())
    }

    #[test]
    fn bounded_file_reader_rejects_growth_past_its_metadata_check() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("catalog.xml");
        fs::write(
            &path,
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Inventory total="0" categories="0" />
            </KartCatalog>"#,
        )
        .unwrap();

        let error = CatalogInventory::from_bounded_file(
            File::open(path).unwrap(),
            32,
            ValidationPolicy::structural_only(),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "kart catalog XML exceeds 32 bytes (33 bytes)"
        );
    }

    #[test]
    fn parses_normalizes_and_classifies_inventory() {
        let catalog = parse_structural(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Names />
                <Inventory total="3" categories="2">
                    <Item category="3" id="1453" name="chicken_goldV1" />
                    <Item category="1" id="45" name="dummy" />
                    <Item category="3" id="1450" serial="7" name="shurikenV1" />
                </Inventory>
            </KartCatalog>"#,
        )
        .unwrap();

        assert_eq!(catalog.stats().items, 3);
        assert_eq!(catalog.stats().categories, 2);
        assert_eq!(catalog.stats().karts, 2);
        assert_eq!(
            catalog
                .items()
                .iter()
                .map(|item| (item.category, item.id))
                .collect::<Vec<_>>(),
            vec![(1, 45), (3, 1450), (3, 1453)]
        );
        assert_eq!(catalog.items()[1].serial, 7);
        assert!(is_grant_category(3));
        assert!(!is_grant_category(5));
        assert!(!is_grant_item(&CatalogInventoryItem {
            category: 1,
            id: 45,
            serial: 0,
            name: String::new(),
        }));
        assert_eq!(catalog.grant_items().count(), 2);
        assert_eq!(catalog.kart_spec_stats().names, 0);
        assert!(catalog.kart_spec(1450).is_none());
        assert_eq!(catalog.emblem_catalog(), None);
        assert!(catalog.emblems().is_empty());
    }

    #[test]
    fn optional_emblem_catalog_is_bounded_unique_and_source_ordered() {
        let catalog = parse_structural(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Emblems total="3">
                    <Emblem id="7" />
                    <Emblem id="8193" />
                    <Emblem id="32767" />
                </Emblems>
                <Inventory total="0" categories="0" />
            </KartCatalog>"#,
        )
        .unwrap();
        assert_eq!(catalog.emblem_catalog(), Some(&[7, 8_193, 32_767][..]));
        assert_eq!(catalog.stats().emblems, 3);
        assert!(catalog.contains_emblem(8_193));

        for invalid in ["-3", "0"] {
            let xml = format!(
                r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                    <Emblems><Emblem id="{invalid}" /></Emblems>
                    <Inventory total="0" categories="0" />
                </KartCatalog>"#
            );
            assert!(matches!(
                parse_structural(&xml),
                Err(CatalogInventoryError::InvalidEmblemId)
            ));
        }

        let duplicate = parse_structural(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Emblems><Emblem id="7" /><Emblem id="7" /></Emblems>
                <Inventory total="0" categories="0" />
            </KartCatalog>"#,
        )
        .unwrap_err();
        assert!(matches!(
            duplicate,
            CatalogInventoryError::DuplicateEmblemId { id: 7 }
        ));

        let mismatch = parse_structural(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Emblems total="2"><Emblem id="7" /></Emblems>
                <Inventory total="0" categories="0" />
            </KartCatalog>"#,
        )
        .unwrap_err();
        assert!(matches!(
            mismatch,
            CatalogInventoryError::EmblemCountMismatch {
                declared: 2,
                actual: 1
            }
        ));

        let excessive = format!(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Emblems total="{}" />
                <Inventory total="0" categories="0" />
            </KartCatalog>"#,
            MAX_CATALOG_EMBLEMS + 1
        );
        assert!(matches!(
            parse_structural(&excessive),
            Err(CatalogInventoryError::TooManyEmblems { .. })
        ));
    }

    #[test]
    fn resolves_generated_names_and_specs_with_exact_csharp_defaults_and_scales() {
        let catalog = parse_structural(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Names>
                    <Kart id="1450" name="SHURIKENV1" />
                    <Kart id="1451" name="sharedSpec" />
                    <Kart id="1452" name="sharedSpec" />
                    <Kart id="1453" name="missingSpec" />
                </Names>
                <Specs>
                    <Spec name="shurikenV1">
                        <BodyParam
                            ForwardAccelForce="147"
                            NormalBoosterTime="-100"
                            DriftLeanFactor="-0.005"
                            draftTick="12.9"
                            UseTransformBooster="TrUe"
                            PartsEngineLock="1"
                            instAccelGaugeCooldownTime="3.25"
                            instAccelGaugeLength="2.5"
                            instAccelGaugeMinUsable="0.3"
                            TransAccelFactor="NaN"
                            DragFactor="Infinity"
                            FutureP5136Field="preserved-by-newer-servers" />
                    </Spec>
                    <Spec name="sharedSpec"><BodyParam /></Spec>
                    <Spec name="unusedSpec"><BodyParam /></Spec>
                </Specs>
                <Inventory total="1" categories="1">
                    <Item category="3" id="1450" name="shurikenV1" />
                </Inventory>
            </KartCatalog>"#,
        )
        .unwrap();

        assert_eq!(catalog.kart_name(1450), Some("SHURIKENV1"));
        let spec = catalog.kart_spec(1450).unwrap();
        assert_eq!(spec.forward_accel_force.to_bits(), 147.0_f32.to_bits());
        assert_eq!(spec.normal_booster_time.to_bits(), 2_900.0_f32.to_bits());
        assert_eq!(spec.drift_lean_factor.to_bits(), 0.065_f32.to_bits());
        assert_eq!(spec.draft_tick, 12);
        assert_eq!(spec.use_transform_booster, 1);
        // BooleanAttributes uses bool.TryParse: a numeric "1" falls back.
        assert_eq!(spec.parts_engine_lock, 0);
        assert_eq!(spec.inst_accel_gauge_cooldown_time, 3_250);
        assert_eq!(
            spec.inst_accel_gauge_length.to_bits(),
            2_500.0_f32.to_bits()
        );
        assert_eq!(
            spec.inst_accel_gauge_min_usable.to_bits(),
            750.0_f32.to_bits()
        );
        // Non-decimal/non-finite spellings take the C# config default.
        assert_eq!(spec.trans_accel_factor.to_bits(), 1.5_f32.to_bits());
        assert_eq!(spec.drag_factor.to_bits(), 0.0_f32.to_bits());
        // Missing fields use KartSpecConfig.DefaultValue, not the property
        // initializer values in P5136KartSpecSnapshot::csharp_default().
        assert_eq!(
            spec.drift_boost_mul_accel_factor.to_bits(),
            1.31_f32.to_bits()
        );
        assert_eq!(spec.item_booster_time.to_bits(), 3_000.0_f32.to_bits());
        assert_eq!(spec.bike_rear_wheel, 1);
        assert_eq!(spec.parts_wheel_lock, 0);

        assert!(std::ptr::eq(
            catalog.kart_spec(1451).unwrap(),
            catalog.kart_spec(1452).unwrap()
        ));
        assert!(catalog.kart_spec(1453).is_none());
        assert!(catalog.kart_spec_by_name("UNUSEDSPEC").is_some());
        assert_eq!(
            catalog.kart_spec_stats(),
            super::CatalogKartSpecStats {
                names: 4,
                specs: 3,
                resolved_names: 3,
                unresolved_names: 1,
                unreferenced_specs: 1,
            }
        );
    }

    #[test]
    fn rejects_duplicate_ids_spec_names_sections_and_body_params() {
        let duplicate_id = parse_structural(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Names><Kart id="1" name="one" /><Kart id="1" name="two" /></Names>
                <Specs><Spec name="one"><BodyParam /></Spec></Specs>
                <Inventory total="0" categories="0" />
            </KartCatalog>"#,
        );
        assert!(matches!(
            duplicate_id,
            Err(CatalogInventoryError::DuplicateKartId { id: 1 })
        ));

        let duplicate_spec = parse_structural(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Names><Kart id="1" name="one" /></Names>
                <Specs>
                    <Spec name="One"><BodyParam /></Spec>
                    <Spec name="oNE"><BodyParam /></Spec>
                </Specs>
                <Inventory total="0" categories="0" />
            </KartCatalog>"#,
        );
        assert!(matches!(
            duplicate_spec,
            Err(CatalogInventoryError::DuplicateKartSpecName { .. })
        ));

        let duplicate_sections = parse_structural(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Names /><Names />
                <Inventory total="0" categories="0" />
            </KartCatalog>"#,
        );
        assert!(matches!(
            duplicate_sections,
            Err(CatalogInventoryError::MultipleNames)
        ));

        let duplicate_body = parse_structural(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Names><Kart id="1" name="one" /></Names>
                <Specs><Spec name="one"><BodyParam /><BodyParam /></Spec></Specs>
                <Inventory total="0" categories="0" />
            </KartCatalog>"#,
        );
        assert!(matches!(
            duplicate_body,
            Err(CatalogInventoryError::MultipleBodyParams { .. })
        ));
    }

    #[test]
    fn rejects_partial_or_malformed_metadata_but_allows_unresolved_entries() {
        let names_only = parse_structural(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Names><Kart id="1" name="one" /></Names>
                <Inventory total="0" categories="0" />
            </KartCatalog>"#,
        );
        assert!(matches!(
            names_only,
            Err(CatalogInventoryError::PartialKartMetadata { names: 1, specs: 0 })
        ));

        let specs_only = parse_structural(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Specs><Spec name="one"><BodyParam /></Spec></Specs>
                <Inventory total="0" categories="0" />
            </KartCatalog>"#,
        );
        assert!(matches!(
            specs_only,
            Err(CatalogInventoryError::PartialKartMetadata { names: 0, specs: 1 })
        ));

        let missing_body = parse_structural(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Names><Kart id="1" name="one" /></Names>
                <Specs><Spec name="one"><ModelParam /></Spec></Specs>
                <Inventory total="0" categories="0" />
            </KartCatalog>"#,
        );
        assert!(matches!(
            missing_body,
            Err(CatalogInventoryError::MissingBodyParam { .. })
        ));

        let unresolved = parse_structural(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Names>
                    <Kart id="1" name="one" />
                    <Kart id="2" name="intentionallyMissing" />
                </Names>
                <Specs>
                    <Spec name="one"><BodyParam /></Spec>
                    <Spec name="unreferenced"><BodyParam /></Spec>
                </Specs>
                <Inventory total="2" categories="1">
                    <Item category="3" id="1" />
                    <Item category="3" id="2" />
                </Inventory>
            </KartCatalog>"#,
        )
        .unwrap();
        assert!(unresolved.kart_spec(1).is_some());
        assert!(unresolved.kart_spec(2).is_none());
        assert_eq!(unresolved.kart_spec_stats().unresolved_names, 1);
        assert_eq!(unresolved.kart_spec_stats().unreferenced_specs, 1);
        assert!(unresolved.grants_item(3, 1));
        assert!(!unresolved.grants_item(3, 2));
        assert_eq!(
            unresolved
                .grant_items()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            vec![1]
        );

        let target_overflow = parse_structural(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Names><Kart id="1" name="one" /></Names>
                <Specs>
                    <Spec name="one"><BodyParam SpeedSlotCapacity="256" /></Spec>
                </Specs>
                <Inventory total="0" categories="0" />
            </KartCatalog>"#,
        );
        assert!(matches!(
            target_overflow,
            Err(CatalogInventoryError::InvalidBodyParamValue {
                field: "SpeedSlotCapacity",
                ..
            })
        ));
    }

    #[test]
    fn enforces_name_text_attribute_and_field_bounds() {
        let long_name = "n".repeat(MAX_KART_NAME_BYTES + 1);
        let error = parse_structural(&format!(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Names><Kart id="1" name="{long_name}" /></Names>
                <Specs><Spec name="one"><BodyParam /></Spec></Specs>
                <Inventory total="0" categories="0" />
            </KartCatalog>"#
        ));
        assert!(matches!(
            error,
            Err(CatalogInventoryError::KartNameTooLong { .. })
        ));

        let long_attribute = "v".repeat(MAX_XML_ATTRIBUTE_VALUE_BYTES + 1);
        let error = parse_structural(&format!(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Unknown value="{long_attribute}" />
                <Inventory total="0" categories="0" />
            </KartCatalog>"#
        ));
        assert!(matches!(
            error,
            Err(CatalogInventoryError::AttributeValueTooLong { .. })
        ));

        let long_text = "x".repeat(MAX_XML_TEXT_BYTES + 1);
        let error = parse_structural(&format!(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Unknown>{long_text}</Unknown>
                <Inventory total="0" categories="0" />
            </KartCatalog>"#
        ));
        assert!(matches!(
            error,
            Err(CatalogInventoryError::TextTooLong { .. })
        ));

        let mut attributes = String::new();
        for index in 0..=MAX_XML_ATTRIBUTES_PER_ELEMENT {
            write!(attributes, r#" f{index}="0""#).unwrap();
        }
        let error = parse_structural(&format!(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Unknown{attributes} />
                <Inventory total="0" categories="0" />
            </KartCatalog>"#
        ));
        assert!(matches!(
            error,
            Err(CatalogInventoryError::TooManyAttributes { .. })
        ));
    }

    #[test]
    fn rejects_wrong_catalog_identity_and_doctype() {
        let wrong = parse_structural(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="cn">
                <Inventory total="0" categories="0" />
            </KartCatalog>"#,
        );
        assert!(matches!(
            wrong,
            Err(CatalogInventoryError::WrongCatalog { .. })
        ));

        let doctype = parse_structural(
            r#"<!DOCTYPE KartCatalog>
            <KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Inventory total="0" categories="0" />
            </KartCatalog>"#,
        );
        assert!(matches!(doctype, Err(CatalogInventoryError::DocumentType)));
    }

    #[test]
    fn rejects_duplicate_and_zero_item_ids() {
        let duplicate = parse_structural(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Inventory total="2" categories="1">
                    <Item category="3" id="1450" />
                    <Item category="3" id="1450" />
                </Inventory>
            </KartCatalog>"#,
        );
        assert!(matches!(
            duplicate,
            Err(CatalogInventoryError::DuplicateItem {
                category: 3,
                id: 1450
            })
        ));

        let zero = parse_structural(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Inventory total="1" categories="1"><Item category="3" id="0" /></Inventory>
            </KartCatalog>"#,
        );
        assert!(matches!(
            zero,
            Err(CatalogInventoryError::InvalidItemAttribute { attribute: "id" })
        ));
    }

    #[test]
    fn checks_declared_counts() {
        let item_count = parse_structural(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Inventory total="2" categories="1"><Item category="3" id="1450" /></Inventory>
            </KartCatalog>"#,
        );
        assert!(matches!(
            item_count,
            Err(CatalogInventoryError::ItemCountMismatch {
                declared: 2,
                actual: 1
            })
        ));

        let category_count = parse_structural(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Inventory total="1" categories="2"><Item category="3" id="1450" /></Inventory>
            </KartCatalog>"#,
        );
        assert!(matches!(
            category_count,
            Err(CatalogInventoryError::CategoryCountMismatch {
                declared: 2,
                actual: 1
            })
        ));
    }

    #[test]
    fn production_load_rejects_truncated_catalog() {
        let result = CatalogInventory::from_xml(
            br#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Inventory total="2" categories="1">
                    <Item category="3" id="1450" />
                    <Item category="3" id="1453" />
                </Inventory>
            </KartCatalog>"#,
        );
        assert!(matches!(
            result,
            Err(CatalogInventoryError::Incomplete { .. })
        ));
    }

    #[test]
    fn rejects_nested_inventory_spoof() {
        let result = parse_structural(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Wrapper><Inventory total="0" categories="0" /></Wrapper>
            </KartCatalog>"#,
        );
        assert!(matches!(
            result,
            Err(CatalogInventoryError::MissingInventory)
        ));
    }
}
