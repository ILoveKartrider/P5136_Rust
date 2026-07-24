//! Bounded loader for the inventory section of an exported P5136 kart catalog.
//!
//! `KartCatalog.xml` is generated from a user's own client installation. It is
//! deliberately treated as runtime input and is never embedded in this crate.

use std::{
    borrow::Cow,
    collections::{BTreeSet, HashSet},
    fmt,
    fs::File,
    io::{BufRead, BufReader, Read},
    path::Path,
};

use quick_xml::{
    Reader, XmlVersion,
    events::{BytesStart, Event},
    name::QName,
};
use thiserror::Error;

pub const MAX_CATALOG_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_CATALOG_ITEMS: usize = 100_000;
pub const MAX_ITEM_NAME_BYTES: usize = 1_024;

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
}

impl fmt::Display for CatalogInventoryStats {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "items={}, categories={}, karts={}, grant={}/{} categories",
            self.items, self.categories, self.karts, self.grant_items, self.grant_categories
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogInventory {
    items: Vec<CatalogInventoryItem>,
    stats: CatalogInventoryStats,
}

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

    pub fn category(&self, category: u16) -> impl Iterator<Item = &CatalogInventoryItem> {
        self.items
            .iter()
            .filter(move |item| item.category == category)
    }

    pub fn grant_items(&self) -> impl Iterator<Item = &CatalogInventoryItem> {
        self.items.iter().filter(|item| is_grant_item(item))
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
    root_seen: bool,
    inventory_seen: bool,
    in_inventory: bool,
    depth: usize,
    declared_items: Option<usize>,
    declared_categories: Option<usize>,
    keys: HashSet<(u16, u16)>,
    items: Vec<CatalogInventoryItem>,
}

impl CatalogParser {
    fn new(policy: ValidationPolicy) -> Self {
        Self {
            policy,
            root_seen: false,
            inventory_seen: false,
            in_inventory: false,
            depth: 0,
            declared_items: None,
            declared_categories: None,
            keys: HashSet::new(),
            items: Vec::new(),
        }
    }

    fn parse<R: BufRead>(&mut self, input: R) -> Result<CatalogInventory, CatalogInventoryError> {
        let mut reader = Reader::from_reader(input);
        reader.config_mut().trim_text(true);
        let mut buffer = Vec::new();

        loop {
            match reader.read_event_into(&mut buffer)? {
                Event::Start(element) => {
                    self.open_element(&reader, &element)?;
                    self.depth = self.depth.saturating_add(1);
                }
                Event::Empty(element) => self.empty_element(&reader, &element)?,
                Event::End(element) => {
                    self.depth = self.depth.saturating_sub(1);
                    if self.depth == 1 && element.name() == QName(b"Inventory") {
                        self.in_inventory = false;
                    }
                }
                Event::DocType(_) => return Err(CatalogInventoryError::DocumentType),
                Event::Eof => break,
                Event::Decl(_)
                | Event::Text(_)
                | Event::CData(_)
                | Event::Comment(_)
                | Event::PI(_)
                | Event::GeneralRef(_) => {}
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
            self.in_inventory = true;
        } else if self.depth == 2 && self.in_inventory && element.name() == QName(b"Item") {
            self.read_item(reader, element)?;
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
        } else if self.depth == 2 && self.in_inventory && element.name() == QName(b"Item") {
            self.read_item(reader, element)?;
        }
        Ok(())
    }

    fn read_root<R: BufRead>(
        &mut self,
        reader: &Reader<R>,
        element: &BytesStart<'_>,
    ) -> Result<(), CatalogInventoryError> {
        if self.root_seen {
            return Err(CatalogInventoryError::MultipleRoots);
        }
        self.root_seen = true;

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
        if self.inventory_seen {
            return Err(CatalogInventoryError::MultipleInventories);
        }
        self.inventory_seen = true;
        self.declared_items = Some(parse_usize_attribute(reader, element, b"total", "total")?);
        self.declared_categories = Some(parse_usize_attribute(
            reader,
            element,
            b"categories",
            "categories",
        )?);
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

    fn finish(&mut self) -> Result<CatalogInventory, CatalogInventoryError> {
        if !self.root_seen {
            return Err(CatalogInventoryError::MissingRoot);
        }
        if !self.inventory_seen {
            return Err(CatalogInventoryError::MissingInventory);
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

        Ok(CatalogInventory {
            items: std::mem::take(&mut self.items),
            stats,
        })
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
    use std::{fs, fs::File};

    use tempfile::tempdir;

    use super::{
        CatalogInventory, CatalogInventoryError, CatalogInventoryItem, CatalogParser,
        ValidationPolicy, is_grant_category, is_grant_item,
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
