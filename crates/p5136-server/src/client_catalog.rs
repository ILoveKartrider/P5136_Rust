//! Direct, bounded construction of the server's kart catalog from a stock
//! Korean P5136 client `Data` directory.
//!
//! The C# compatibility server exported these sources to
//! `Profile/KartCatalog.xml`.  Rust keeps the same overlay rules but publishes
//! one immutable in-memory snapshot, so no generated sidecar is required.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use p5136_core::{
    bml::{BmlError, BmlLimits, BmlNode},
    packet::PacketReader,
};
use p5136_profile::{CatalogInventory, CatalogInventoryError, MAX_CATALOG_BYTES};
use p5136_rho5::{
    LegacyRhoArchive, LegacyRhoError, LegacyRhoLimits, Rho5Directory, Rho5Error, Rho5Limits,
};
use quick_xml::{
    Reader, Writer, XmlVersion,
    events::{BytesEnd, BytesStart, Event},
};
use thiserror::Error;

const MINIMUM_KART_NAMES: usize = 1_400;
const MINIMUM_KART_SPECS: usize = 1_300;
const MINIMUM_TRANSFORM_RULES: usize = 450;
const MAX_SOURCE_ENTRY_BYTES: usize = 8 * 1024 * 1024;
const MAX_XML_EVENTS: usize = 200_000;
const MAX_XML_DEPTH: usize = 32;
const MAX_SOURCE_ELEMENTS: usize = 100_000;
const MAX_SOURCE_ATTRIBUTES: usize = 256;
const MAX_SOURCE_STRING_BYTES: usize = 4 * 1024;
const SHOP_INVENTORY_PATH: &str = "zeta_/kr/shop/data/item.kml";

const VERIFIED_ITEM_SYMBOLS: &[(&str, i16)] = &[
    ("animalBooster", 31),
    ("bigBanana", 85),
    ("blockRocket", 117),
    ("candyRocket", 102),
    ("cokeBomb", 20),
    ("cokeRocket", 30),
    ("cokeRocketWorldCup", 39),
    ("darkCloud", 1),
    ("darkCloud2", 115),
    ("dinoClawRocket", 108),
    ("dinoEggRocket", 107),
    ("drrMine", 23),
    ("duckMine", 45),
    ("eggMine", 82),
    ("foxTailRocket", 126),
    ("goldEggMine", 83),
    ("goldRocket", 32),
    ("goldShield", 36),
    ("infectedBomb", 27),
    ("infectedWaterFly", 119),
    ("lockdownRocket", 104),
    ("prisonBomb", 47),
    ("protectShield", 81),
    ("pumpkinBomb", 44),
    ("rainbowCloud", 43),
    ("rollingCokeBomb", 22),
    ("rollingInfectedBomb", 29),
    ("siren", 24),
    ("sirenShield", 106),
    ("snowBomb", 34),
    ("snowWaterFly", 118),
    ("snowman", 112),
    ("superMagnet", 103),
    ("tigerGhost", 101),
    ("tigerRocket", 99),
    ("timeCokeBomb", 21),
    ("timeInfectedBomb", 28),
    ("timeSnowBomb", 35),
    ("waterMine", 37),
    ("waterbombFly", 120),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientKartCatalogStats {
    pub names: usize,
    pub specs: usize,
    pub inventory_items: usize,
    pub inventory_categories: usize,
    pub inventory_karts: usize,
    pub auto_grant_karts: usize,
    pub quarantined_karts: usize,
    pub transform_rules: usize,
    pub item_symbols: usize,
}

#[derive(Debug)]
pub struct LoadedClientKartCatalog {
    catalog: CatalogInventory,
    source_directory: PathBuf,
    stats: ClientKartCatalogStats,
}

impl LoadedClientKartCatalog {
    #[must_use]
    pub fn catalog(&self) -> &CatalogInventory {
        &self.catalog
    }

    #[must_use]
    pub fn into_catalog(self) -> CatalogInventory {
        self.catalog
    }

    #[must_use]
    pub fn source_directory(&self) -> &Path {
        &self.source_directory
    }

    #[must_use]
    pub const fn stats(&self) -> ClientKartCatalogStats {
        self.stats
    }
}

#[derive(Debug, Error)]
pub enum ClientKartCatalogError {
    #[error("failed to inspect client Data directory {path}")]
    InspectData {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("client Data path is not a directory: {path}")]
    NotDirectory { path: PathBuf },
    #[error("required stock archive is missing: {path}")]
    MissingArchive { path: PathBuf },
    #[error(transparent)]
    LegacyRho(#[from] LegacyRhoError),
    #[error(transparent)]
    Rho5(#[from] Rho5Error),
    #[error("client catalog source {path:?} exceeds {maximum} bytes ({actual} bytes)")]
    SourceTooLarge {
        path: String,
        actual: usize,
        maximum: usize,
    },
    #[error("client catalog source {path:?} is empty")]
    EmptySource { path: String },
    #[error("failed to decode BML client catalog source {path:?}")]
    Bml {
        path: String,
        #[source]
        source: BmlError,
    },
    #[error("BML client catalog source {path:?} contains trailing bytes")]
    BmlTrailingBytes { path: String },
    #[error("client catalog XML source {path:?} has invalid UTF-16")]
    InvalidUtf16 { path: String },
    #[error("client catalog XML source {path:?} has invalid UTF-8")]
    InvalidUtf8 { path: String },
    #[error("failed to parse client catalog XML source {path:?}")]
    Xml {
        path: String,
        #[source]
        source: quick_xml::Error,
    },
    #[error("client catalog XML source {path:?} contains a prohibited document type")]
    DocumentType { path: String },
    #[error("client catalog source {path:?} contains an invalid {field}")]
    InvalidField { path: String, field: &'static str },
    #[error("client catalog source {path:?} exceeds the bounded {limit}")]
    SourceComplexity { path: String, limit: &'static str },
    #[error("client item symbol {name:?} maps to both {first} and {second}")]
    ConflictingItemSymbol {
        name: String,
        first: i16,
        second: i16,
    },
    #[error("stock client catalog is incomplete: names={names}, specs={specs}")]
    IncompleteKartMetadata { names: usize, specs: usize },
    #[error("stock client item transform table is incomplete: {actual} rules")]
    IncompleteTransforms { actual: usize },
    #[error(
        "stock client item transform is unresolved: kart={kart_id}, {source_symbol:?}->{target_symbol:?}"
    )]
    UnresolvedTransform {
        kart_id: u16,
        source_symbol: String,
        target_symbol: String,
    },
    #[error("stock client shop inventory {SHOP_INVENTORY_PATH:?} was not found in RHO5")]
    MissingInventory,
    #[error("generated in-memory client catalog exceeds {maximum} bytes")]
    CatalogTooLarge { maximum: u64 },
    #[error("direct RHO client catalog validation failed")]
    Catalog(#[from] CatalogInventoryError),
    #[error("direct RHO client catalog failed the P5136 sentinel check: {check}")]
    Sentinel { check: &'static str },
}

#[derive(Debug, Clone)]
struct SourceElement {
    attributes: Vec<(String, String)>,
}

impl SourceElement {
    fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Debug)]
struct Prioritized<T> {
    priority: i32,
    value: T,
}

#[derive(Debug)]
struct RawTransform {
    kart_id: u16,
    source: String,
    source_id: i16,
    target_id: i16,
    probability: u8,
    mode: String,
}

type KartNames = BTreeMap<u16, Prioritized<String>>;
type KartSpecs = BTreeMap<String, Prioritized<SourceElement>>;
#[derive(Debug, Default)]
struct KartResources {
    model_folders: HashSet<String>,
}

#[derive(Debug)]
struct InventoryItem {
    category: u16,
    id: u16,
    name: String,
    auto_grant: bool,
}
type TransformSources = BTreeMap<String, Prioritized<SourceElement>>;

/// Reads the authoritative catalog inputs directly from `kart.rho`,
/// `item.rho`, and the KR RHO5 overlays in a stock client `Data` directory.
/// No file is written and the returned snapshot no longer retains archive
/// handles.
pub fn load_client_kart_catalog(
    data_directory: impl AsRef<Path>,
) -> Result<LoadedClientKartCatalog, ClientKartCatalogError> {
    let (source_directory, kart_path, item_path) = client_catalog_paths(data_directory.as_ref())?;
    let (names, specs, resources, rho5) = load_kart_metadata(&source_directory, &kart_path)?;
    validate_kart_metadata(&names, &specs)?;
    let mut inventory = load_inventory(&rho5, &names)?;
    classify_kart_auto_grants(&mut inventory, &names, &specs, &resources);
    let (symbols, transforms) = load_item_transforms(&item_path)?;

    let xml = build_catalog_xml(&names, &specs, &inventory, &transforms)?;
    let catalog = CatalogInventory::from_xml(&xml)?;
    validate_p5136_sentinels(&catalog)?;
    let stats = ClientKartCatalogStats {
        names: names.len(),
        specs: specs.len(),
        inventory_items: inventory.len(),
        inventory_categories: inventory
            .iter()
            .map(|item| item.category)
            .collect::<BTreeSet<_>>()
            .len(),
        inventory_karts: inventory.iter().filter(|item| item.category == 3).count(),
        auto_grant_karts: inventory
            .iter()
            .filter(|item| item.category == 3 && item.auto_grant)
            .count(),
        quarantined_karts: inventory
            .iter()
            .filter(|item| item.category == 3 && !item.auto_grant)
            .count(),
        transform_rules: transforms.len(),
        item_symbols: symbols.len(),
    };
    Ok(LoadedClientKartCatalog {
        catalog,
        source_directory,
        stats,
    })
}

fn client_catalog_paths(
    requested: &Path,
) -> Result<(PathBuf, PathBuf, PathBuf), ClientKartCatalogError> {
    let source_directory =
        fs::canonicalize(requested).map_err(|source| ClientKartCatalogError::InspectData {
            path: requested.to_path_buf(),
            source,
        })?;
    let metadata =
        fs::metadata(&source_directory).map_err(|source| ClientKartCatalogError::InspectData {
            path: source_directory.clone(),
            source,
        })?;
    if !metadata.is_dir() {
        return Err(ClientKartCatalogError::NotDirectory {
            path: source_directory,
        });
    }
    let kart_path = source_directory.join("kart.rho");
    let item_path = source_directory.join("item.rho");
    for path in [&kart_path, &item_path] {
        if !path.is_file() {
            return Err(ClientKartCatalogError::MissingArchive { path: path.clone() });
        }
    }
    Ok((source_directory, kart_path, item_path))
}

fn load_kart_metadata(
    source_directory: &Path,
    kart_path: &Path,
) -> Result<(KartNames, KartSpecs, KartResources, Rho5Directory), ClientKartCatalogError> {
    let kart_archive = LegacyRhoArchive::open(kart_path, kart_rho_limits())?;
    let mut names = KartNames::new();
    let mut specs = KartSpecs::new();
    let mut resources = KartResources::default();
    for entry in kart_archive.entries()? {
        let path = entry.normalized_path();
        if let Some(folder) = kart_model_folder(path) {
            resources.model_folders.insert(folder);
        }
        let file_name = path.rsplit('/').next().unwrap_or(path);
        let name_priority = catalog_file_priority(file_name, "itemTable", "kr");
        if name_priority > 0 {
            let bytes = checked_legacy_extract(&kart_archive, &entry)?;
            merge_names(&mut names, name_priority, path, &bytes)?;
        }
        if let Some((kart_name, priority)) = kart_param_candidate(path, "kr") {
            let bytes = checked_legacy_extract(&kart_archive, &entry)?;
            merge_spec(&mut specs, priority, kart_name, path, &bytes)?;
        }
    }

    let rho5 = Rho5Directory::scan_kr(source_directory, catalog_rho5_limits())?;
    for entry in rho5.entries() {
        let path = entry.normalized_path();
        if let Some(folder) = kart_model_folder(path) {
            resources.model_folders.insert(folder);
        }
        let file_name = path.rsplit('/').next().unwrap_or(path);
        let name_priority = catalog_file_priority(file_name, "itemTable", "kr");
        let spec = kart_param_candidate(path, "kr");
        if name_priority == 0 && spec.is_none() {
            continue;
        }
        let bytes = checked_rho5_extract(&rho5, entry)?;
        if name_priority > 0 {
            merge_names(&mut names, name_priority, path, &bytes)?;
        }
        if let Some((kart_name, priority)) = spec {
            merge_spec(&mut specs, priority, kart_name, path, &bytes)?;
        }
    }
    // `kart.rho` is 112 MiB in the stock build. Release it before opening
    // `item.rho` so startup does not retain both legacy archives at once.
    drop(kart_archive);
    Ok((names, specs, resources, rho5))
}

fn validate_kart_metadata(
    names: &KartNames,
    specs: &KartSpecs,
) -> Result<(), ClientKartCatalogError> {
    if names.len() < MINIMUM_KART_NAMES || specs.len() < MINIMUM_KART_SPECS {
        return Err(ClientKartCatalogError::IncompleteKartMetadata {
            names: names.len(),
            specs: specs.len(),
        });
    }
    Ok(())
}

fn load_inventory(
    rho5: &Rho5Directory,
    names: &KartNames,
) -> Result<Vec<InventoryItem>, ClientKartCatalogError> {
    let inventory_entry = rho5
        .entries()
        .iter()
        .rfind(|entry| {
            entry
                .normalized_path()
                .eq_ignore_ascii_case(SHOP_INVENTORY_PATH)
        })
        .ok_or(ClientKartCatalogError::MissingInventory)?;
    let inventory_bytes = checked_rho5_extract(rho5, inventory_entry)?;
    parse_inventory(inventory_entry.normalized_path(), &inventory_bytes, names)
}

fn classify_kart_auto_grants(
    inventory: &mut [InventoryItem],
    names: &KartNames,
    specs: &KartSpecs,
    resources: &KartResources,
) {
    for item in inventory.iter_mut().filter(|item| item.category == 3) {
        item.auto_grant = kart_is_safe_for_automatic_grant(item, names, specs, resources);
    }
}

fn kart_is_safe_for_automatic_grant(
    item: &InventoryItem,
    names: &KartNames,
    specs: &KartSpecs,
    resources: &KartResources,
) -> bool {
    let Some(internal_name) = names.get(&item.id).map(|name| name.value.trim()) else {
        return false;
    };
    if internal_name.is_empty() || looks_like_non_player_kart(internal_name, &item.name) {
        return false;
    }
    let Some(spec) = specs.get(&internal_name.to_ascii_lowercase()) else {
        return false;
    };
    let model_folder = spec
        .value
        .attribute("addModelFolder")
        .map(str::trim)
        .filter(|folder| !folder.is_empty())
        .unwrap_or(internal_name)
        .to_ascii_lowercase();
    resources.model_folders.contains(&model_folder)
}

fn looks_like_non_player_kart(internal_name: &str, display_name: &str) -> bool {
    let internal = internal_name.to_ascii_lowercase();
    let display = display_name.to_ascii_lowercase();
    internal.contains("dummy")
        || internal.starts_with("npc_")
        || internal.starts_with("ai_")
        || internal.contains("test")
        || display.contains("dummy")
        || display.contains("test")
        || display.contains("더미")
}

fn kart_model_folder(path: &str) -> Option<String> {
    let mut components = path.rsplit('/');
    if !components.next()?.eq_ignore_ascii_case("model.1s") {
        return None;
    }
    let folder = components.next()?.trim();
    (!folder.is_empty()).then(|| folder.to_ascii_lowercase())
}

fn load_item_transforms(
    item_path: &Path,
) -> Result<(HashMap<String, i16>, Vec<RawTransform>), ClientKartCatalogError> {
    let item_archive = LegacyRhoArchive::open(item_path, LegacyRhoLimits::default())?;
    let mut symbols = HashMap::<String, i16>::new();
    let mut transform_sources = TransformSources::new();
    for entry in item_archive.entries()? {
        let path = entry.normalized_path();
        let file_name = path.rsplit('/').next().unwrap_or(path);
        let lower_path = path.to_ascii_lowercase();
        if lower_path
            .split('/')
            .any(|component| component.eq_ignore_ascii_case("slot"))
            && file_name.to_ascii_lowercase().contains("prob")
            && catalog_file_format_priority(file_name) > 0
        {
            let bytes = checked_legacy_extract(&item_archive, &entry)?;
            merge_item_symbols(&mut symbols, path, &bytes)?;
        }
        let priority = catalog_file_priority(file_name, "transformByKart", "kr");
        if priority > 0 {
            let bytes = checked_legacy_extract(&item_archive, &entry)?;
            merge_transform_sources(&mut transform_sources, priority, path, &bytes)?;
        }
    }
    for &(name, id) in VERIFIED_ITEM_SYMBOLS {
        add_item_symbol(&mut symbols, name, id)?;
    }
    let transforms = resolve_transforms(transform_sources, &symbols)?;
    if transforms.len() < MINIMUM_TRANSFORM_RULES {
        return Err(ClientKartCatalogError::IncompleteTransforms {
            actual: transforms.len(),
        });
    }
    Ok((symbols, transforms))
}

fn validate_p5136_sentinels(catalog: &CatalogInventory) -> Result<(), ClientKartCatalogError> {
    for (id, expected_name) in [(1_450, "shurikenV1"), (1_453, "chicken_goldV1")] {
        if catalog.kart_name(id) != Some(expected_name) {
            return Err(ClientKartCatalogError::Sentinel {
                check: "kart name identity",
            });
        }
        let Some(spec) = catalog.kart_spec(id) else {
            return Err(ClientKartCatalogError::Sentinel {
                check: "kart BodyParam presence",
            });
        };
        if spec.item_slot_capacity != 3 || spec.special_slot_capacity != 2 {
            return Err(ClientKartCatalogError::Sentinel {
                check: "kart item/special slot capacities",
            });
        }
    }
    for (source_id, expected_target) in [(8, 83), (5, 103)] {
        let Some(rule) = catalog.item_transform(1_453, source_id, "no_flag") else {
            return Err(ClientKartCatalogError::Sentinel {
                check: "chicken_goldV1 transform presence",
            });
        };
        if rule.target_item_id != expected_target || rule.probability != 100 {
            return Err(ClientKartCatalogError::Sentinel {
                check: "chicken_goldV1 transform semantics",
            });
        }
    }
    Ok(())
}

fn kart_rho_limits() -> LegacyRhoLimits {
    LegacyRhoLimits {
        max_archive_bytes: 192 * 1024 * 1024,
        max_blocks: 100_000,
        ..LegacyRhoLimits::default()
    }
}

fn catalog_rho5_limits() -> Rho5Limits {
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

fn checked_legacy_extract(
    archive: &LegacyRhoArchive,
    entry: &p5136_rho5::LegacyRhoEntry,
) -> Result<Vec<u8>, ClientKartCatalogError> {
    check_source_size(entry.normalized_path(), entry.plaintext_size())?;
    archive.extract_entry(entry).map_err(Into::into)
}

fn checked_rho5_extract(
    directory: &Rho5Directory,
    entry: &p5136_rho5::Rho5Entry,
) -> Result<Vec<u8>, ClientKartCatalogError> {
    check_source_size(entry.normalized_path(), entry.plaintext_size())?;
    directory
        .extract_entry_with_legacy_padding(entry)
        .map_err(Into::into)
}

fn check_source_size(path: &str, size: usize) -> Result<(), ClientKartCatalogError> {
    if size == 0 {
        return Err(ClientKartCatalogError::EmptySource {
            path: path.to_owned(),
        });
    }
    if size > MAX_SOURCE_ENTRY_BYTES {
        return Err(ClientKartCatalogError::SourceTooLarge {
            path: path.to_owned(),
            actual: size,
            maximum: MAX_SOURCE_ENTRY_BYTES,
        });
    }
    Ok(())
}

fn merge_names(
    names: &mut BTreeMap<u16, Prioritized<String>>,
    priority: i32,
    path: &str,
    bytes: &[u8],
) -> Result<(), ClientKartCatalogError> {
    for element in source_elements(path, bytes, "kart")? {
        let Some(id) = element.attribute("id").and_then(|value| value.parse().ok()) else {
            continue;
        };
        let Some(name) = element
            .attribute("name")
            .filter(|name| !name.trim().is_empty())
        else {
            continue;
        };
        let replace = names
            .get(&id)
            .is_none_or(|current| priority >= current.priority);
        if replace {
            names.insert(
                id,
                Prioritized {
                    priority,
                    value: name.to_owned(),
                },
            );
        }
    }
    Ok(())
}

fn merge_spec(
    specs: &mut BTreeMap<String, Prioritized<SourceElement>>,
    priority: i32,
    kart_name: &str,
    path: &str,
    bytes: &[u8],
) -> Result<(), ClientKartCatalogError> {
    let Some(body) = source_elements(path, bytes, "BodyParam")?
        .into_iter()
        .next()
    else {
        return Ok(());
    };
    let key = kart_name.to_ascii_lowercase();
    let replace = specs
        .get(&key)
        .is_none_or(|current| priority >= current.priority);
    if replace {
        specs.insert(
            key,
            Prioritized {
                priority,
                value: body,
            },
        );
    }
    Ok(())
}

fn parse_inventory(
    path: &str,
    bytes: &[u8],
    names: &BTreeMap<u16, Prioritized<String>>,
) -> Result<Vec<InventoryItem>, ClientKartCatalogError> {
    let mut inventory = BTreeMap::new();
    for element in source_elements(path, bytes, "item")? {
        let category = parse_required::<u16>(&element, "itemCatId", path)?;
        let id = parse_required::<u16>(&element, "itemId", path)?;
        if id == 0 || (category == 3 && !names.contains_key(&id)) {
            return Err(ClientKartCatalogError::InvalidField {
                path: path.to_owned(),
                field: "shop item ID",
            });
        }
        let name = element.attribute("itemName").unwrap_or_default().to_owned();
        if inventory.insert((category, id), name).is_some() {
            return Err(ClientKartCatalogError::InvalidField {
                path: path.to_owned(),
                field: "duplicate shop item",
            });
        }
    }
    Ok(inventory
        .into_iter()
        .map(|((category, id), name)| InventoryItem {
            category,
            id,
            name,
            auto_grant: true,
        })
        .collect())
}

fn merge_item_symbols(
    symbols: &mut HashMap<String, i16>,
    path: &str,
    bytes: &[u8],
) -> Result<(), ClientKartCatalogError> {
    for element in source_elements(path, bytes, "item")? {
        let Some(id) = element
            .attribute("idx")
            .and_then(|value| value.parse::<i16>().ok())
        else {
            continue;
        };
        let Some(name) = element
            .attribute("name")
            .filter(|name| !name.trim().is_empty())
        else {
            continue;
        };
        add_item_symbol(symbols, name, id)?;
    }
    Ok(())
}

fn add_item_symbol(
    symbols: &mut HashMap<String, i16>,
    name: &str,
    id: i16,
) -> Result<(), ClientKartCatalogError> {
    if let Some(first) = symbols.insert(name.to_owned(), id)
        && first != id
    {
        return Err(ClientKartCatalogError::ConflictingItemSymbol {
            name: name.to_owned(),
            first,
            second: id,
        });
    }
    Ok(())
}

fn merge_transform_sources(
    transforms: &mut BTreeMap<String, Prioritized<SourceElement>>,
    priority: i32,
    path: &str,
    bytes: &[u8],
) -> Result<(), ClientKartCatalogError> {
    for element in source_elements(path, bytes, "item")? {
        let Some(kart_id) = element.attribute("kartId") else {
            continue;
        };
        let Some(source) = element.attribute("srcIdx") else {
            continue;
        };
        let mode = element.attribute("gitType").unwrap_or_default();
        let key = format!("{kart_id}|{source}|{mode}");
        let replace = transforms
            .get(&key)
            .is_none_or(|current| priority >= current.priority);
        if replace {
            transforms.insert(
                key,
                Prioritized {
                    priority,
                    value: element,
                },
            );
        }
    }
    Ok(())
}

fn resolve_transforms(
    sources: BTreeMap<String, Prioritized<SourceElement>>,
    symbols: &HashMap<String, i16>,
) -> Result<Vec<RawTransform>, ClientKartCatalogError> {
    let mut transforms = Vec::with_capacity(sources.len());
    for source in sources.into_values() {
        let element = source.value;
        let kart_id = parse_required::<u16>(&element, "kartId", "item.rho transformByKart")?;
        let source_name = required_attribute(&element, "srcIdx", "item.rho transformByKart")?;
        let target_name = required_attribute(&element, "dstIdx", "item.rho transformByKart")?;
        let source_id = symbols.get(source_name).copied().ok_or_else(|| {
            ClientKartCatalogError::UnresolvedTransform {
                kart_id,
                source_symbol: source_name.to_owned(),
                target_symbol: target_name.to_owned(),
            }
        })?;
        let target_id = symbols.get(target_name).copied().ok_or_else(|| {
            ClientKartCatalogError::UnresolvedTransform {
                kart_id,
                source_symbol: source_name.to_owned(),
                target_symbol: target_name.to_owned(),
            }
        })?;
        transforms.push(RawTransform {
            kart_id,
            source: source_name.to_owned(),
            source_id,
            target_id,
            probability: parse_required(&element, "probability", "item.rho transformByKart")?,
            mode: element.attribute("gitType").unwrap_or_default().to_owned(),
        });
    }
    transforms.sort_unstable_by(|left, right| {
        (left.kart_id, &left.source, &left.mode).cmp(&(right.kart_id, &right.source, &right.mode))
    });
    Ok(transforms)
}

fn build_catalog_xml(
    names: &KartNames,
    specs: &KartSpecs,
    inventory: &[InventoryItem],
    transforms: &[RawTransform],
) -> Result<Vec<u8>, ClientKartCatalogError> {
    let mut writer = Writer::new(Vec::with_capacity(8 * 1024 * 1024));
    let mut root = BytesStart::new("KartCatalog");
    root.push_attribute(("formatVersion", "3"));
    root.push_attribute(("protocolVersion", "5136"));
    root.push_attribute(("region", "kr"));
    writer
        .write_event(Event::Start(root))
        .map_err(xml_build_error)?;
    write_catalog_names_and_specs(&mut writer, names, specs)?;
    write_catalog_inventory(&mut writer, inventory)?;
    write_catalog_transforms(&mut writer, transforms)?;
    writer
        .write_event(Event::End(BytesEnd::new("KartCatalog")))
        .map_err(xml_build_error)?;
    let xml = writer.into_inner();
    let maximum = usize::try_from(MAX_CATALOG_BYTES).unwrap_or(usize::MAX);
    if xml.len() > maximum {
        return Err(ClientKartCatalogError::CatalogTooLarge {
            maximum: MAX_CATALOG_BYTES,
        });
    }
    Ok(xml)
}

fn write_catalog_names_and_specs(
    writer: &mut Writer<Vec<u8>>,
    names: &KartNames,
    specs: &KartSpecs,
) -> Result<(), ClientKartCatalogError> {
    writer
        .write_event(Event::Start(BytesStart::new("Names")))
        .map_err(xml_build_error)?;
    for (&id, name) in names {
        let id = id.to_string();
        let mut element = BytesStart::new("Kart");
        element.push_attribute(("id", id.as_str()));
        element.push_attribute(("name", name.value.as_str()));
        writer
            .write_event(Event::Empty(element))
            .map_err(xml_build_error)?;
    }
    writer
        .write_event(Event::End(BytesEnd::new("Names")))
        .map_err(xml_build_error)?;

    writer
        .write_event(Event::Start(BytesStart::new("Specs")))
        .map_err(xml_build_error)?;
    for (name, spec) in specs {
        let mut element = BytesStart::new("Spec");
        element.push_attribute(("name", name.as_str()));
        writer
            .write_event(Event::Start(element))
            .map_err(xml_build_error)?;
        write_source_element(writer, "BodyParam", &spec.value)?;
        writer
            .write_event(Event::End(BytesEnd::new("Spec")))
            .map_err(xml_build_error)?;
    }
    writer
        .write_event(Event::End(BytesEnd::new("Specs")))
        .map_err(xml_build_error)?;
    Ok(())
}

fn write_catalog_inventory(
    writer: &mut Writer<Vec<u8>>,
    inventory: &[InventoryItem],
) -> Result<(), ClientKartCatalogError> {
    let total = inventory.len().to_string();
    let categories = inventory
        .iter()
        .map(|item| item.category)
        .collect::<BTreeSet<_>>()
        .len()
        .to_string();
    let mut inventory_root = BytesStart::new("Inventory");
    inventory_root.push_attribute(("total", total.as_str()));
    inventory_root.push_attribute(("categories", categories.as_str()));
    writer
        .write_event(Event::Start(inventory_root))
        .map_err(xml_build_error)?;
    for item in inventory {
        let category = item.category.to_string();
        let id = item.id.to_string();
        let mut element = BytesStart::new("Item");
        element.push_attribute(("category", category.as_str()));
        element.push_attribute(("id", id.as_str()));
        if !item.name.trim().is_empty() {
            element.push_attribute(("name", item.name.as_str()));
        }
        if !item.auto_grant {
            element.push_attribute(("autoGrant", "false"));
        }
        writer
            .write_event(Event::Empty(element))
            .map_err(xml_build_error)?;
    }
    writer
        .write_event(Event::End(BytesEnd::new("Inventory")))
        .map_err(xml_build_error)?;
    Ok(())
}

fn write_catalog_transforms(
    writer: &mut Writer<Vec<u8>>,
    transforms: &[RawTransform],
) -> Result<(), ClientKartCatalogError> {
    writer
        .write_event(Event::Start(BytesStart::new("Abilities")))
        .map_err(xml_build_error)?;
    writer
        .write_event(Event::Start(BytesStart::new("TransformByKart")))
        .map_err(xml_build_error)?;
    for transform in transforms {
        let kart_id = transform.kart_id.to_string();
        let source_id = transform.source_id.to_string();
        let target_id = transform.target_id.to_string();
        let probability = transform.probability.to_string();
        let mut element = BytesStart::new("Rule");
        element.push_attribute(("kartId", kart_id.as_str()));
        element.push_attribute(("sourceId", source_id.as_str()));
        element.push_attribute(("targetId", target_id.as_str()));
        element.push_attribute(("probability", probability.as_str()));
        if !transform.mode.is_empty() {
            element.push_attribute(("gitType", transform.mode.as_str()));
        }
        writer
            .write_event(Event::Empty(element))
            .map_err(xml_build_error)?;
    }
    writer
        .write_event(Event::End(BytesEnd::new("TransformByKart")))
        .map_err(xml_build_error)?;
    writer
        .write_event(Event::End(BytesEnd::new("Abilities")))
        .map_err(xml_build_error)?;
    Ok(())
}

fn write_source_element(
    writer: &mut Writer<Vec<u8>>,
    name: &str,
    source: &SourceElement,
) -> Result<(), ClientKartCatalogError> {
    let mut element = BytesStart::new(name);
    for (key, value) in &source.attributes {
        element.push_attribute((key.as_str(), value.as_str()));
    }
    writer
        .write_event(Event::Empty(element))
        .map_err(xml_build_error)
}

fn xml_build_error(source: std::io::Error) -> ClientKartCatalogError {
    ClientKartCatalogError::Xml {
        path: "<in-memory catalog>".to_owned(),
        source: source.into(),
    }
}

fn parse_required<T: std::str::FromStr>(
    element: &SourceElement,
    field: &'static str,
    path: &str,
) -> Result<T, ClientKartCatalogError> {
    element
        .attribute(field)
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| ClientKartCatalogError::InvalidField {
            path: path.to_owned(),
            field,
        })
}

fn required_attribute<'a>(
    element: &'a SourceElement,
    field: &'static str,
    path: &str,
) -> Result<&'a str, ClientKartCatalogError> {
    element
        .attribute(field)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ClientKartCatalogError::InvalidField {
            path: path.to_owned(),
            field,
        })
}

fn source_elements(
    path: &str,
    bytes: &[u8],
    wanted: &str,
) -> Result<Vec<SourceElement>, ClientKartCatalogError> {
    if path
        .rsplit('.')
        .next()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("bml"))
    {
        return bml_elements(path, bytes, wanted);
    }
    xml_elements(path, bytes, wanted)
}

fn bml_elements(
    path: &str,
    bytes: &[u8],
    wanted: &str,
) -> Result<Vec<SourceElement>, ClientKartCatalogError> {
    let limits = BmlLimits {
        max_depth: 16,
        max_nodes: 100_000,
        max_attributes_per_node: 256,
        max_children_per_node: 50_000,
        max_string_code_units: 4_096,
    };
    let mut reader = PacketReader::new(bytes);
    let root = BmlNode::decode_with_limits(&mut reader, limits).map_err(|source| {
        ClientKartCatalogError::Bml {
            path: path.to_owned(),
            source,
        }
    })?;
    if !reader.remaining().is_empty() {
        return Err(ClientKartCatalogError::BmlTrailingBytes {
            path: path.to_owned(),
        });
    }
    let mut output = Vec::new();
    let mut pending = vec![&root];
    while let Some(node) = pending.pop() {
        if node.name.eq_ignore_ascii_case(wanted) {
            output.push(SourceElement {
                attributes: node.attributes.clone(),
            });
        }
        pending.extend(node.children.iter().rev());
    }
    Ok(output)
}

fn xml_elements(
    path: &str,
    bytes: &[u8],
    wanted: &str,
) -> Result<Vec<SourceElement>, ClientKartCatalogError> {
    let xml = decode_xml_text(path, bytes)?;
    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(true);
    let mut output = Vec::new();
    let mut event_count = 0_usize;
    let mut depth = 0_usize;
    loop {
        event_count = event_count.saturating_add(1);
        if event_count > MAX_XML_EVENTS {
            return Err(ClientKartCatalogError::SourceComplexity {
                path: path.to_owned(),
                limit: "XML event count",
            });
        }
        match reader
            .read_event()
            .map_err(|source| ClientKartCatalogError::Xml {
                path: path.to_owned(),
                source,
            })? {
            Event::Start(element) => {
                depth = depth.saturating_add(1);
                if depth > MAX_XML_DEPTH {
                    return Err(ClientKartCatalogError::SourceComplexity {
                        path: path.to_owned(),
                        limit: "XML nesting depth",
                    });
                }
                if element
                    .local_name()
                    .as_ref()
                    .eq_ignore_ascii_case(wanted.as_bytes())
                {
                    output.push(read_source_xml_element(path, &reader, &element)?);
                }
            }
            Event::Empty(element)
                if element
                    .local_name()
                    .as_ref()
                    .eq_ignore_ascii_case(wanted.as_bytes()) =>
            {
                output.push(read_source_xml_element(path, &reader, &element)?);
            }
            Event::End(_) => depth = depth.saturating_sub(1),
            Event::DocType(_) => {
                return Err(ClientKartCatalogError::DocumentType {
                    path: path.to_owned(),
                });
            }
            Event::Eof => break,
            _ => {}
        }
        if output.len() > MAX_SOURCE_ELEMENTS {
            return Err(ClientKartCatalogError::SourceComplexity {
                path: path.to_owned(),
                limit: "matching element count",
            });
        }
    }
    Ok(output)
}

fn read_source_xml_element(
    path: &str,
    reader: &Reader<&[u8]>,
    element: &BytesStart<'_>,
) -> Result<SourceElement, ClientKartCatalogError> {
    let mut attributes = Vec::new();
    for attribute in element.attributes().with_checks(true) {
        if attributes.len() >= MAX_SOURCE_ATTRIBUTES {
            return Err(ClientKartCatalogError::SourceComplexity {
                path: path.to_owned(),
                limit: "attributes per element",
            });
        }
        let attribute = attribute
            .map_err(quick_xml::Error::from)
            .map_err(|source| ClientKartCatalogError::Xml {
                path: path.to_owned(),
                source,
            })?;
        let key = String::from_utf8_lossy(attribute.key.as_ref()).into_owned();
        let value = attribute
            .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())
            .map_err(|source| ClientKartCatalogError::Xml {
                path: path.to_owned(),
                source,
            })?
            .into_owned();
        if key.len() > MAX_SOURCE_STRING_BYTES || value.len() > MAX_SOURCE_STRING_BYTES {
            return Err(ClientKartCatalogError::SourceComplexity {
                path: path.to_owned(),
                limit: "attribute name/value length",
            });
        }
        attributes.push((key, value));
    }
    Ok(SourceElement { attributes })
}

fn decode_xml_text(path: &str, bytes: &[u8]) -> Result<String, ClientKartCatalogError> {
    if bytes.is_empty() {
        return Err(ClientKartCatalogError::EmptySource {
            path: path.to_owned(),
        });
    }
    let mut text = if bytes.starts_with(&[0xff, 0xfe]) || bytes.get(1) == Some(&0) {
        decode_utf16(
            path,
            &bytes[usize::from(bytes.starts_with(&[0xff, 0xfe])) * 2..],
            true,
        )?
    } else if bytes.starts_with(&[0xfe, 0xff]) {
        decode_utf16(path, &bytes[2..], false)?
    } else {
        std::str::from_utf8(bytes)
            .map_err(|_| ClientKartCatalogError::InvalidUtf8 {
                path: path.to_owned(),
            })?
            .trim_start_matches('\u{feff}')
            .to_owned()
    };
    text = text
        .trim_start_matches(['\u{feff}', '\0', ' ', '\t', '\r', '\n'])
        .to_owned();
    while let Some(start) = text.to_ascii_lowercase().find("<?xml") {
        let Some(relative_end) = text[start..].find("?>") else {
            break;
        };
        text.replace_range(start..start + relative_end + 2, "");
    }
    Ok(text
        .trim_start_matches(['\u{feff}', '\0', ' ', '\t', '\r', '\n'])
        .to_owned())
}

fn decode_utf16(
    path: &str,
    bytes: &[u8],
    little_endian: bool,
) -> Result<String, ClientKartCatalogError> {
    let chunks = bytes.chunks_exact(2);
    if !chunks.remainder().is_empty() {
        return Err(ClientKartCatalogError::InvalidUtf16 {
            path: path.to_owned(),
        });
    }
    let units = chunks
        .map(|pair| {
            if little_endian {
                u16::from_le_bytes([pair[0], pair[1]])
            } else {
                u16::from_be_bytes([pair[0], pair[1]])
            }
        })
        .collect::<Vec<_>>();
    String::from_utf16(&units).map_err(|_| ClientKartCatalogError::InvalidUtf16 {
        path: path.to_owned(),
    })
}

fn kart_param_candidate<'a>(path: &'a str, region: &str) -> Option<(&'a str, i32)> {
    let normalized = path.trim_start_matches('/');
    let prefix = if normalized
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("kart_/"))
    {
        6
    } else if normalized
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("kart/"))
    {
        5
    } else {
        return None;
    };
    let relative = &normalized[prefix..];
    let (kart_name, file_name) = relative.split_once('/')?;
    if kart_name.is_empty() || file_name.contains('/') {
        return None;
    }
    let priority = catalog_file_priority(file_name, "param", region);
    (priority > 0).then_some((kart_name, priority))
}

fn catalog_file_priority(file_name: &str, stem: &str, region: &str) -> i32 {
    let format_priority = catalog_file_format_priority(file_name);
    if format_priority == 0 {
        return 0;
    }
    let base = file_name
        .rsplit_once('.')
        .map_or(file_name, |(base, _)| base);
    if base.eq_ignore_ascii_case(stem) {
        return format_priority;
    }
    if base.eq_ignore_ascii_case(&format!("{stem}@{region}")) {
        100 + format_priority
    } else {
        0
    }
}

fn catalog_file_format_priority(file_name: &str) -> i32 {
    match file_name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .as_deref()
    {
        Some("bml") => 1,
        Some("kml") => 2,
        Some("xml") => 3,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{
        InventoryItem, KartNames, KartResources, KartSpecs, Prioritized, SourceElement,
        catalog_file_priority, classify_kart_auto_grants, decode_xml_text, kart_model_folder,
        kart_param_candidate, load_client_kart_catalog,
    };

    #[test]
    fn source_priority_matches_the_reference_exporter() {
        assert_eq!(catalog_file_priority("param.bml", "param", "kr"), 1);
        assert_eq!(catalog_file_priority("param.kml", "param", "kr"), 2);
        assert_eq!(catalog_file_priority("param@kr.xml", "param", "kr"), 103);
        assert_eq!(catalog_file_priority("param@cn.xml", "param", "kr"), 0);
        assert_eq!(
            kart_param_candidate("kart_/tiger/param@kr.kml", "kr"),
            Some(("tiger", 102))
        );
        assert_eq!(kart_param_candidate("other/tiger/param.kml", "kr"), None);
    }

    #[test]
    fn utf16_and_duplicate_declarations_are_normalized_in_memory() {
        let text = "<?xml version='1.0'?>\r\n<?xml version='1.0'?><root/>";
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(decode_xml_text("fixture.kml", &bytes).unwrap(), "<root/>");
    }

    #[test]
    fn automatic_grants_require_a_spec_and_resolvable_model_resources() {
        let names = KartNames::from([
            (
                1,
                Prioritized {
                    priority: 1,
                    value: "normalKart".to_owned(),
                },
            ),
            (
                2,
                Prioritized {
                    priority: 1,
                    value: "sharedKart".to_owned(),
                },
            ),
            (
                3,
                Prioritized {
                    priority: 1,
                    value: "missingModel".to_owned(),
                },
            ),
            (
                4,
                Prioritized {
                    priority: 1,
                    value: "missingSpec".to_owned(),
                },
            ),
            (
                5,
                Prioritized {
                    priority: 1,
                    value: "development_test_kart".to_owned(),
                },
            ),
        ]);
        let body = |attributes: &[(&str, &str)]| Prioritized {
            priority: 1,
            value: SourceElement {
                attributes: attributes
                    .iter()
                    .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                    .collect(),
            },
        };
        let specs = KartSpecs::from([
            ("normalkart".to_owned(), body(&[])),
            (
                "sharedkart".to_owned(),
                body(&[("addModelFolder", "commonModel")]),
            ),
            ("missingmodel".to_owned(), body(&[])),
            ("development_test_kart".to_owned(), body(&[])),
        ]);
        let resources = KartResources {
            model_folders: HashSet::from([
                "normalkart".to_owned(),
                "commonmodel".to_owned(),
                "development_test_kart".to_owned(),
            ]),
        };
        let mut inventory = (1..=5)
            .map(|id| InventoryItem {
                category: 3,
                id,
                name: format!("Kart {id}"),
                auto_grant: true,
            })
            .collect::<Vec<_>>();

        classify_kart_auto_grants(&mut inventory, &names, &specs, &resources);
        assert_eq!(
            inventory
                .iter()
                .map(|item| item.auto_grant)
                .collect::<Vec<_>>(),
            vec![true, true, false, false, false]
        );
    }

    #[test]
    fn model_folder_detection_is_case_insensitive_and_requires_model_file() {
        assert_eq!(
            kart_model_folder("kart_/GigantesV1/MODEL.1S"),
            Some("gigantesv1".to_owned())
        );
        assert_eq!(kart_model_folder("kart_/GigantesV1/param.xml"), None);
        assert_eq!(kart_model_folder("model.1s"), None);
    }

    #[test]
    fn configured_real_client_catalog_matches_the_known_p5136_shape() {
        let Ok(data) = std::env::var("P5136_CLIENT_DATA_DIR") else {
            return;
        };
        let loaded = load_client_kart_catalog(data).unwrap();
        let stats = loaded.stats();
        assert_eq!(stats.names, 1_456);
        assert_eq!(stats.specs, 1_353);
        assert_eq!(stats.inventory_items, 6_929);
        assert_eq!(stats.inventory_categories, 65);
        assert_eq!(stats.inventory_karts, 1_296);
        assert_eq!(stats.auto_grant_karts, 1_282);
        assert_eq!(stats.quarantined_karts, 14);
        assert_eq!(stats.transform_rules, 493);
        assert_eq!(stats.item_symbols, 73);
        assert_eq!(loaded.catalog().kart_name(1_410), Some("gigantesV1"));
        assert!(loaded.catalog().grants_item(3, 1_410));
        assert!(loaded.catalog().contains_kart(814));
        assert!(!loaded.catalog().grants_item(3, 814));
        assert!(loaded.catalog().grants_item(28, 49));
        for unsafe_myroom_id in [14, 37, 50] {
            assert!(!loaded.catalog().grants_item(28, unsafe_myroom_id));
        }
        assert_eq!(
            loaded
                .catalog()
                .category(3)
                .filter(|item| !item.auto_grant)
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            vec![
                199, 312, 323, 352, 657, 658, 659, 744, 745, 746, 795, 814, 886, 1167,
            ]
        );
    }
}
