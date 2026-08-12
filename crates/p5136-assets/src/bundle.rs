use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fmt::Write as _,
    fs,
    path::Path,
};

use anyhow::{Context, Result, bail, ensure};
use p5136_core::{
    bml::{BmlLimits, BmlNode},
    packet::{PacketReader, PacketWriter},
};
use p5136_rho5::{
    P5136_PACKED_ENTRY_FLAGS, Rho5Directory, Rho5Limits, Rho5Region, Rho5WriteEntry, Rho5Writer,
};
use quick_xml::{
    Reader, Writer, XmlVersion,
    events::{BytesStart, Event},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    COMPATIBILITY_ASSERTION,
    asset_index::{AssetIndex, AssetRegion, fold_path},
    ensure_staging_destination, verify_sha256,
};

const TABLE_PATH: &str = "etc_/itemTable.kml";
const SOURCE_SHOP_PATH: &str = "zeta_/cn/shop/data/item.kml";
const TARGET_SHOP_PATH: &str = "zeta_/kr/shop/data/item.kml";
const TRANSFORM_BY_KART_PATH: &str = "item/slot/transformByKart.bml";
const FIRED_TO_GAIN_PATH: &str = "item/slot/fired2Gain.bml";
const FIRING_TO_GAIN_PATH: &str = "item/slot/firing2Gain.bml";
const ANIMAL_BOOSTER_PATH: &str = "item/slot/animalBooster.bml";
const ABILITY_PATHS: &[&str] = &[
    TRANSFORM_BY_KART_PATH,
    FIRED_TO_GAIN_PATH,
    FIRING_TO_GAIN_PATH,
    ANIMAL_BOOSTER_PATH,
];
const VERIFIED_P5136_ITEM_SYMBOLS: &[&str] = &[
    "animalBooster",
    "bigBanana",
    "blockRocket",
    "candyRocket",
    "cokeBomb",
    "cokeRocket",
    "cokeRocketWorldCup",
    "darkCloud",
    "darkCloud2",
    "dinoClawRocket",
    "dinoEggRocket",
    "drrMine",
    "duckMine",
    "eggMine",
    "foxTailRocket",
    "goldEggMine",
    "goldRocket",
    "goldShield",
    "infectedBomb",
    "infectedWaterFly",
    "lockdownRocket",
    "prisonBomb",
    "protectShield",
    "pumpkinBomb",
    "rainbowCloud",
    "rollingCokeBomb",
    "rollingInfectedBomb",
    "siren",
    "sirenShield",
    "snowBomb",
    "snowWaterFly",
    "snowman",
    "superMagnet",
    "tigerGhost",
    "tigerRocket",
    "timeCokeBomb",
    "timeInfectedBomb",
    "timeSnowBomb",
    "waterMine",
    "waterbombFly",
];
const MAX_CHUNK_PLAINTEXT_BYTES: usize = 40 * 1024 * 1024;
const MAX_OUTPUT_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct StoredPlan {
    schema_version: u32,
    source_data: String,
    target_data: String,
    assets: Vec<StoredAsset>,
}

#[derive(Debug, Deserialize)]
struct StoredAsset {
    category: String,
    asset_id: String,
    status: String,
    manifest: String,
}

#[derive(Debug, Deserialize)]
struct StoredManifest {
    schema_version: u32,
    compatibility: String,
    entries: Vec<StoredEntry>,
}

#[derive(Debug, Deserialize)]
struct StoredEntry {
    target_path: String,
    expected_sha256: String,
}

#[derive(Debug, Deserialize)]
struct PreviousBundleReport {
    archives: Vec<PreviousBundleArchive>,
}

#[derive(Debug, Deserialize)]
struct PreviousBundleArchive {
    name: String,
}

#[derive(Debug, Clone)]
struct PendingFile {
    path: String,
    source_path: String,
    expected_sha256: String,
}

#[derive(Debug, Clone)]
struct XmlRow {
    element: String,
    attributes: Vec<(String, String)>,
}

impl XmlRow {
    fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn replace_attribute(&mut self, name: &str, value: String) -> Result<()> {
        let (_, existing) = self
            .attributes
            .iter_mut()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .with_context(|| format!("{} row is missing {name}", self.element))?;
        *existing = value;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
struct CatalogItemReport {
    category: String,
    id: u16,
    code: String,
    table: String,
    shop: String,
    hashed_fields: usize,
}

#[derive(Debug, Serialize)]
struct BundleReport {
    schema_version: u32,
    source_data: String,
    target_data: String,
    archives: Vec<BundleArchiveReport>,
    total_archive_bytes: usize,
    asset_groups: BTreeMap<String, usize>,
    resource_entries: usize,
    preserved_table_archive_entries: usize,
    preserved_catalog_archive_entries: usize,
    localized_kart_param_aliases: usize,
    localized_flying_pet_param_aliases: usize,
    catalog_items: Vec<CatalogItemReport>,
    item_abilities: ItemAbilityReport,
    resource_only_assets: Vec<String>,
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
struct ItemAbilityReport {
    transform_by_kart: usize,
    fired_to_gain: usize,
    firing_to_gain: usize,
    animal_booster: usize,
    skipped_unsupported: usize,
}

#[derive(Debug, Serialize)]
struct BundleArchiveReport {
    name: String,
    role: String,
    bytes: usize,
    sha256: String,
    entries: usize,
}

struct CatalogOverlays {
    table: Vec<u8>,
    shop: Vec<u8>,
    ability_files: Vec<(&'static str, Vec<u8>)>,
    ability_report: ItemAbilityReport,
    items: Vec<CatalogItemReport>,
    resource_only: Vec<String>,
}

type AbilityFiles = Vec<(&'static str, Vec<u8>)>;

struct MaterializedArchive {
    entries: Vec<Rho5WriteEntry>,
    preserved_entries: usize,
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(crate) fn stage_compatible(
    source_data: &Path,
    target_data: &Path,
    report_path: &Path,
    output: &Path,
    categories: &[String],
    archive_name: &str,
    table_archive_name: &str,
    catalog_archive_name: &str,
    force: bool,
) -> Result<()> {
    stage_compatible_inner(
        source_data,
        target_data,
        report_path,
        output,
        categories,
        archive_name,
        table_archive_name,
        catalog_archive_name,
        force,
        None,
    )
}

#[allow(dead_code, clippy::too_many_arguments)]
pub(crate) fn stage_selected_compatible(
    source_data: &Path,
    target_data: &Path,
    report_path: &Path,
    output: &Path,
    categories: &[String],
    archive_name: &str,
    table_archive_name: &str,
    catalog_archive_name: &str,
    force: bool,
    selected_assets: &BTreeSet<String>,
) -> Result<()> {
    ensure!(!selected_assets.is_empty(), "no assets were selected");
    stage_compatible_inner(
        source_data,
        target_data,
        report_path,
        output,
        categories,
        archive_name,
        table_archive_name,
        catalog_archive_name,
        force,
        Some(selected_assets),
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn stage_compatible_inner(
    source_data: &Path,
    target_data: &Path,
    report_path: &Path,
    output: &Path,
    categories: &[String],
    archive_name: &str,
    table_archive_name: &str,
    catalog_archive_name: &str,
    force: bool,
    selected_assets: Option<&BTreeSet<String>>,
) -> Result<()> {
    ensure_staging_destination(source_data, target_data, output)?;
    validate_archive_name(archive_name)?;
    validate_archive_name(table_archive_name)?;
    validate_archive_name(catalog_archive_name)?;
    ensure!(
        !archive_name.eq_ignore_ascii_case(table_archive_name)
            && !archive_name.eq_ignore_ascii_case(catalog_archive_name)
            && !table_archive_name.eq_ignore_ascii_case(catalog_archive_name),
        "resource, item-table, and catalog archive names must differ"
    );
    let category_set = categories
        .iter()
        .map(|category| category.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    ensure!(
        !category_set.is_empty(),
        "at least one category is required"
    );
    ensure!(
        category_set.iter().all(|category| {
            matches!(
                category.as_str(),
                "kart" | "character" | "pet" | "flying_pet"
            )
        }),
        "stage-compatible accepts only kart, character, pet, and flying_pet"
    );

    let report_bytes = fs::read(report_path)
        .with_context(|| format!("failed to read {}", report_path.display()))?;
    let report: StoredPlan = serde_json::from_slice(&report_bytes)
        .with_context(|| format!("failed to parse {}", report_path.display()))?;
    ensure!(report.schema_version == 1, "unsupported plan schema");
    ensure_same_directory(source_data, Path::new(&report.source_data), "source Data")?;
    ensure_same_directory(target_data, Path::new(&report.target_data), "target Data")?;
    let report_directory = report_path
        .parent()
        .context("plan report has no parent directory")?;

    let mut selected = Vec::new();
    let mut group_counts = BTreeMap::<String, usize>::new();
    for asset in report.assets {
        let key = format!(
            "{}:{}",
            asset.category.to_ascii_lowercase(),
            asset.asset_id.to_ascii_lowercase()
        );
        if category_set.contains(&asset.category.to_ascii_lowercase())
            && asset.status == "compatible_candidate"
            && selected_assets.is_none_or(|selected| selected.contains(&key))
        {
            *group_counts.entry(asset.category.clone()).or_default() += 1;
            selected.push(asset);
        }
    }
    ensure!(
        !selected.is_empty(),
        "plan contains no selected compatible assets"
    );

    let mut pending = BTreeMap::<String, PendingFile>::new();
    let mut pending_region_param_aliases = Vec::new();
    let mut codes = BTreeMap::<String, BTreeSet<String>>::new();
    for asset in &selected {
        codes
            .entry(asset.category.clone())
            .or_default()
            .insert(asset.asset_id.clone());
        let manifest_path = report_directory.join(&asset.manifest);
        let manifest: StoredManifest = serde_json::from_slice(
            &fs::read(&manifest_path)
                .with_context(|| format!("failed to read {}", manifest_path.display()))?,
        )
        .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
        ensure!(manifest.schema_version == 1, "unsupported manifest schema");
        ensure!(
            manifest.compatibility == COMPATIBILITY_ASSERTION,
            "{} is not a verified compatible manifest",
            manifest_path.display()
        );
        for entry in manifest.entries {
            let key = fold_path(&entry.target_path);
            if let Some(existing) = pending.get(&key) {
                ensure!(
                    existing
                        .expected_sha256
                        .eq_ignore_ascii_case(&entry.expected_sha256),
                    "manifests disagree on {}",
                    entry.target_path
                );
                continue;
            }
            if let Some(alias_path) = korean_region_param_alias(&asset.category, &entry.target_path)
            {
                pending_region_param_aliases.push((
                    asset.category.clone(),
                    PendingFile {
                        path: alias_path,
                        source_path: entry.target_path.clone(),
                        expected_sha256: entry.expected_sha256.clone(),
                    },
                ));
            }
            pending.insert(
                key,
                PendingFile {
                    source_path: entry.target_path.clone(),
                    path: entry.target_path,
                    expected_sha256: entry.expected_sha256,
                },
            );
        }
    }
    let mut localized_kart_param_aliases = 0_usize;
    let mut localized_flying_pet_param_aliases = 0_usize;
    for (category, alias) in pending_region_param_aliases {
        let key = fold_path(&alias.path);
        if pending.contains_key(&key) {
            continue;
        }
        pending.insert(key, alias);
        if category.eq_ignore_ascii_case("kart") {
            localized_kart_param_aliases += 1;
        } else if category.eq_ignore_ascii_case("flying_pet") {
            localized_flying_pet_param_aliases += 1;
        }
    }

    fs::create_dir_all(output).with_context(|| format!("failed to create {}", output.display()))?;
    let json_path = output.join("bundle-report.json");
    let markdown_path = output.join("bundle-report.md");
    if force {
        remove_previous_bundle_archives(output, &json_path)?;
    } else {
        ensure!(
            !output.join(archive_name).exists()
                && !output.join(table_archive_name).exists()
                && !output.join(catalog_archive_name).exists()
                && !json_path.exists()
                && !markdown_path.exists(),
            "staging output already exists; pass --force to replace generated bundle files"
        );
    }

    eprintln!("indexing source and target catalogs...");
    let cache = output.join(".index-cache");
    let source = AssetIndex::scan(
        source_data,
        AssetRegion::China,
        &cache.join("source-legacy.json"),
    )?;
    let target = AssetIndex::scan(
        target_data,
        AssetRegion::Korea,
        &cache.join("target-legacy.json"),
    )?;
    let mut extractor = source.extractor();
    let mut entries = Vec::with_capacity(pending.len());
    for (index, file) in pending.values().enumerate() {
        if index != 0 && index.is_multiple_of(250) {
            eprintln!("extracted {index}/{} resource entries", pending.len());
        }
        let record = source
            .effective(&file.source_path)
            .with_context(|| format!("planned source path disappeared: {}", file.source_path))?;
        let bytes = extractor.extract(record)?;
        verify_sha256(&bytes, &file.expected_sha256, &file.source_path)?;
        entries.push(Rho5WriteEntry {
            path: file.path.clone(),
            data: bytes,
            flags: P5136_PACKED_ENTRY_FLAGS,
        });
    }

    let catalogs = build_catalog_overlays(&source, &target, &codes)?;
    let table_archive = materialize_archive(
        target_data,
        table_archive_name,
        &[TABLE_PATH],
        &[(TABLE_PATH, catalogs.table.as_slice())],
    )?;
    let mut catalog_replacements = vec![(TARGET_SHOP_PATH, catalogs.shop.as_slice())];
    catalog_replacements.extend(
        catalogs
            .ability_files
            .iter()
            .map(|(path, bytes)| (*path, bytes.as_slice())),
    );
    let mut catalog_excluded_paths = vec![TABLE_PATH, TARGET_SHOP_PATH];
    catalog_excluded_paths.extend_from_slice(ABILITY_PATHS);
    let catalog_archive = materialize_archive(
        target_data,
        catalog_archive_name,
        &catalog_excluded_paths,
        &catalog_replacements,
    )?;

    let chunks = partition_entries(entries);
    let archive_names = sequential_archive_names(archive_name, chunks.len())?;
    ensure!(
        archive_names
            .iter()
            .all(|name| !name.eq_ignore_ascii_case(table_archive_name)
                && !name.eq_ignore_ascii_case(catalog_archive_name)),
        "resource archive sequence collides with item-table or catalog archive"
    );
    eprintln!(
        "encoding {} resource entries plus preserved item-table and shop base archives into {} archives...",
        pending.len(),
        chunks.len() + 2
    );
    let limits = Rho5Limits {
        max_archive_bytes: MAX_OUTPUT_ARCHIVE_BYTES,
        ..Rho5Limits::default()
    };
    let mut archive_reports = Vec::with_capacity(chunks.len() + 2);
    archive_reports.push(write_archive(
        output,
        table_archive_name,
        "item_table_base",
        table_archive.entries,
        &limits,
    )?);
    for (name, chunk) in archive_names.into_iter().zip(chunks) {
        archive_reports.push(write_archive(output, &name, "resource", chunk, &limits)?);
    }
    archive_reports.push(write_archive(
        output,
        catalog_archive_name,
        "catalog_overlay",
        catalog_archive.entries,
        &limits,
    )?);
    let total_archive_bytes = archive_reports.iter().map(|archive| archive.bytes).sum();
    let bundle_report = BundleReport {
        schema_version: 1,
        source_data: fs::canonicalize(source_data)?.display().to_string(),
        target_data: fs::canonicalize(target_data)?.display().to_string(),
        archives: archive_reports,
        total_archive_bytes,
        asset_groups: group_counts,
        resource_entries: pending.len(),
        preserved_table_archive_entries: table_archive.preserved_entries,
        preserved_catalog_archive_entries: catalog_archive.preserved_entries,
        localized_kart_param_aliases,
        localized_flying_pet_param_aliases,
        catalog_items: catalogs.items,
        item_abilities: catalogs.ability_report,
        resource_only_assets: catalogs.resource_only,
    };
    fs::write(&json_path, serde_json::to_vec_pretty(&bundle_report)?)?;
    fs::write(&markdown_path, render_markdown(&bundle_report))?;
    verify_staged_archives(output, &bundle_report)?;
    println!(
        "staged groups={} resources={} catalog_items={} archives={} bytes={}",
        selected.len(),
        bundle_report.resource_entries,
        bundle_report.catalog_items.len(),
        bundle_report.archives.len(),
        bundle_report.total_archive_bytes
    );
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn build_catalog_overlays(
    source: &AssetIndex,
    target: &AssetIndex,
    codes: &BTreeMap<String, BTreeSet<String>>,
) -> Result<CatalogOverlays> {
    let source_table = effective_bytes(source, TABLE_PATH)?;
    let target_table = effective_bytes(target, TABLE_PATH)?;
    let source_shop = effective_bytes(source, SOURCE_SHOP_PATH)?;
    let target_shop = effective_bytes(target, TARGET_SHOP_PATH)?;
    let source_table_text = decode_xml(&source_table)?;
    let target_table_text = decode_xml(&target_table)?;
    let source_shop_text = decode_xml(&source_shop)?;
    let target_shop_text = decode_xml(&target_shop)?;
    let source_table_rows = xml_rows(
        &source_table_text,
        &["kart", "character", "pet", "flyingPet"],
    )?;
    let target_table_rows = xml_rows(
        &target_table_text,
        &["kart", "character", "pet", "flyingPet"],
    )?;
    let source_shop_rows = xml_rows(&source_shop_text, &["item"])?;
    let target_shop_rows = xml_rows(&target_shop_text, &["item"])?;

    let target_table_by_key = keyed_table_rows(&target_table_rows)?;
    let target_shop_keys = keyed_shop_rows(&target_shop_rows)?;
    let source_shop_by_key = keyed_shop_rows(&source_shop_rows)?;
    let mut table_append = Vec::new();
    let mut shop_append = Vec::new();
    let mut reports = Vec::new();
    let mut mapped_codes = HashSet::new();

    for source_row in &source_table_rows {
        let category = canonical_table_category(&source_row.element).to_owned();
        let Some(wanted) = codes.get(&category) else {
            continue;
        };
        let Some(code) = source_row.attribute("name") else {
            continue;
        };
        if !wanted
            .iter()
            .any(|wanted| wanted.eq_ignore_ascii_case(code))
        {
            continue;
        }
        mapped_codes.insert(format!("{category}:{}", code.to_ascii_lowercase()));
        let id = parse_u16(source_row, "id")?;
        let key = (category.clone(), id);
        let table_state = if let Some(target_row) = target_table_by_key.get(&key) {
            ensure!(
                target_row
                    .attribute("name")
                    .is_some_and(|target| target.eq_ignore_ascii_case(code)),
                "target itemTable ID collision: {category} {id} is not {code}"
            );
            "existing"
        } else {
            table_append.push(source_row.clone());
            "added"
        };
        let shop_category = shop_category_for_asset(&category)
            .expect("itemTable rows were filtered to supported asset categories");
        let shop_key = (shop_category, id);
        let (shop_state, hashed_fields) = if target_shop_keys.contains_key(&shop_key) {
            ("existing", 0)
        } else {
            let source_shop_row = source_shop_by_key.get(&shop_key).with_context(|| {
                format!("source shop catalog is missing category {shop_category} ID {id}")
            })?;
            let (sanitized, hashed) = sanitize_shop_row(source_shop_row, code)?;
            shop_append.push(sanitized);
            ("added", hashed)
        };
        reports.push(CatalogItemReport {
            category,
            id,
            code: code.to_owned(),
            table: table_state.to_owned(),
            shop: shop_state.to_owned(),
            hashed_fields,
        });
    }

    let mut resource_only = Vec::new();
    for (category, wanted) in codes {
        for code in wanted {
            let key = format!(
                "{}:{}",
                category.to_ascii_lowercase(),
                code.to_ascii_lowercase()
            );
            if !mapped_codes.contains(&key) {
                resource_only.push(format!("{category}/{code}"));
            }
        }
    }
    reports.sort_unstable_by(|left, right| {
        (&left.category, left.id, &left.code).cmp(&(&right.category, right.id, &right.code))
    });
    resource_only.sort_unstable();
    let selected_kart_ids = reports
        .iter()
        .filter(|item| item.category.eq_ignore_ascii_case("kart"))
        .map(|item| item.id)
        .collect::<HashSet<_>>();
    let (ability_files, ability_report) =
        build_item_ability_overlays(source, target, &selected_kart_ids)?;
    let merged_table = append_rows(&target_table_text, "itemtable", &table_append)?;
    let merged_shop = append_rows(&target_shop_text, "itemList", &shop_append)?;
    validate_xml(&merged_table)?;
    validate_xml(&merged_shop)?;
    ensure!(
        merged_table.is_ascii() == target_table_text.is_ascii(),
        "unexpected table encoding classification change"
    );
    Ok(CatalogOverlays {
        table: encode_utf16(&merged_table),
        shop: encode_utf16(&merged_shop),
        ability_files,
        ability_report,
        items: reports,
        resource_only,
    })
}

fn effective_bytes(index: &AssetIndex, path: &str) -> Result<Vec<u8>> {
    let record = index
        .effective(path)
        .with_context(|| format!("effective catalog path {path:?} was not found"))?;
    index.extract(record)
}

fn korean_region_param_alias(category: &str, path: &str) -> Option<String> {
    let (directory, file_name) = path.rsplit_once('/')?;
    match category.to_ascii_lowercase().as_str() {
        "kart" if file_name.eq_ignore_ascii_case("param@cn.xml") => {
            Some(format!("{directory}/param@kr.xml"))
        }
        "flying_pet" if file_name.eq_ignore_ascii_case("param@cn.bml") => {
            Some(format!("{directory}/param@kr.bml"))
        }
        _ => None,
    }
}

fn build_item_ability_overlays(
    source: &AssetIndex,
    target: &AssetIndex,
    selected_kart_ids: &HashSet<u16>,
) -> Result<(AbilityFiles, ItemAbilityReport)> {
    let supported_items = p5136_item_symbols(target)?;
    let mut report = ItemAbilityReport::default();
    let mut output = Vec::with_capacity(ABILITY_PATHS.len());
    for &path in ABILITY_PATHS {
        let target_bytes = effective_bytes(target, path)?;
        let source_bytes = effective_bytes(source, path)?;
        let mut target_root = decode_bml(path, &target_bytes)?;
        let source_root = decode_bml(path, &source_bytes)?;
        let mut existing = HashSet::new();
        collect_ability_keys(&target_root, &mut existing);
        let mut additions = Vec::new();
        collect_ability_nodes(
            &source_root,
            selected_kart_ids,
            &supported_items,
            &mut existing,
            &mut additions,
            &mut report.skipped_unsupported,
        );
        match path {
            TRANSFORM_BY_KART_PATH => report.transform_by_kart = additions.len(),
            FIRED_TO_GAIN_PATH => report.fired_to_gain = additions.len(),
            FIRING_TO_GAIN_PATH => report.firing_to_gain = additions.len(),
            ANIMAL_BOOSTER_PATH => report.animal_booster = additions.len(),
            _ => unreachable!("bounded ability path list"),
        }
        target_root.children.extend(additions);
        output.push((path, encode_bml(path, &target_root)?));
    }
    Ok((output, report))
}

fn p5136_item_symbols(target: &AssetIndex) -> Result<HashSet<String>> {
    let mut symbols = VERIFIED_P5136_ITEM_SYMBOLS
        .iter()
        .map(|name| name.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    for record in target.effective_records().filter(|record| {
        let path = fold_path(&record.virtual_path);
        let extension = Path::new(&path)
            .extension()
            .and_then(|value| value.to_str());
        path.starts_with("item/slot/")
            && path.contains("prob")
            && extension.is_some_and(|value| {
                ["bml", "kml", "xml"]
                    .iter()
                    .any(|wanted| value.eq_ignore_ascii_case(wanted))
            })
    }) {
        let bytes = target.extract(record)?;
        if record.virtual_path.to_ascii_lowercase().ends_with(".bml") {
            let root = decode_bml(&record.virtual_path, &bytes)?;
            let mut pending = vec![&root];
            while let Some(node) = pending.pop() {
                pending.extend(node.children.iter());
                if node.name.eq_ignore_ascii_case("item")
                    && let Some(name) = bml_attribute(node, "name")
                {
                    symbols.insert(name.to_ascii_lowercase());
                }
            }
        }
    }
    Ok(symbols)
}

fn collect_ability_nodes(
    node: &BmlNode,
    selected_kart_ids: &HashSet<u16>,
    supported_items: &HashSet<String>,
    existing: &mut HashSet<String>,
    output: &mut Vec<BmlNode>,
    skipped_unsupported: &mut usize,
) {
    if let Some(kart_id) = bml_attribute(node, "kartId").and_then(|value| value.parse().ok())
        && selected_kart_ids.contains(&kart_id)
    {
        let referenced = [
            "srcIdx",
            "dstIdx",
            "firedItemIdx",
            "firingItemIdx",
            "gainItemIdx",
        ];
        let supported = referenced.iter().all(|name| {
            bml_attribute(node, name)
                .is_none_or(|value| supported_items.contains(&value.to_ascii_lowercase()))
        });
        let key = ability_key(node);
        if supported && existing.insert(key) {
            output.push(node.clone());
        } else if !supported {
            *skipped_unsupported += 1;
        }
    }
    for child in &node.children {
        collect_ability_nodes(
            child,
            selected_kart_ids,
            supported_items,
            existing,
            output,
            skipped_unsupported,
        );
    }
}

fn collect_ability_keys(node: &BmlNode, output: &mut HashSet<String>) {
    if bml_attribute(node, "kartId").is_some() {
        output.insert(ability_key(node));
    }
    for child in &node.children {
        collect_ability_keys(child, output);
    }
}

fn ability_key(node: &BmlNode) -> String {
    let mut attributes = node.attributes.clone();
    attributes.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    let mut key = node.name.to_ascii_lowercase();
    for (name, value) in attributes {
        write!(
            key,
            "|{}={}",
            name.to_ascii_lowercase(),
            value.to_ascii_lowercase()
        )
        .expect("String write");
    }
    key
}

fn bml_attribute<'a>(node: &'a BmlNode, name: &str) -> Option<&'a str> {
    node.attributes
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn decode_bml(path: &str, bytes: &[u8]) -> Result<BmlNode> {
    let limits = BmlLimits {
        max_depth: 32,
        max_nodes: 200_000,
        max_attributes_per_node: 512,
        max_children_per_node: 100_000,
        max_string_code_units: 8_192,
    };
    let mut reader = PacketReader::new(bytes);
    let root = BmlNode::decode_with_limits(&mut reader, limits)
        .with_context(|| format!("failed to decode {path}"))?;
    ensure!(reader.remaining().is_empty(), "{path} has trailing bytes");
    Ok(root)
}

fn encode_bml(path: &str, root: &BmlNode) -> Result<Vec<u8>> {
    let limits = BmlLimits {
        max_depth: 32,
        max_nodes: 200_000,
        max_attributes_per_node: 512,
        max_children_per_node: 100_000,
        max_string_code_units: 8_192,
    };
    let mut writer = PacketWriter::new();
    root.encode_with_limits(&mut writer, limits)
        .with_context(|| format!("failed to encode {path}"))?;
    Ok(writer.into_inner())
}

fn materialize_archive(
    target_data: &Path,
    archive_name: &str,
    excluded_paths: &[&str],
    replacements: &[(&str, &[u8])],
) -> Result<MaterializedArchive> {
    let archive_path = target_data.join(archive_name);
    ensure!(
        archive_path.is_file(),
        "overlay base archive does not exist: {}",
        archive_path.display()
    );
    let directory = Rho5Directory::scan_kr(target_data, Rho5Limits::default())?;
    let mut entries = BTreeMap::<String, Rho5WriteEntry>::new();
    for entry in directory
        .entries()
        .iter()
        .filter(|entry| entry.archive_name().eq_ignore_ascii_case(archive_name))
    {
        let key = fold_path(entry.normalized_path());
        if excluded_paths.iter().any(|path| key == fold_path(path)) {
            continue;
        }
        let data = directory.extract_entry_with_legacy_padding(entry)?;
        ensure!(
            entries
                .insert(
                    key,
                    Rho5WriteEntry {
                        path: entry.raw_path().to_owned(),
                        data,
                        // Every payload is decoded and encoded again below.
                        // Preserve the raw path, but normalize processing flags
                        // to match the writer's compressed/two-layer encrypted
                        // representation.  Copying a stale zero flag makes the
                        // native client expose ciphertext to resource readers.
                        flags: P5136_PACKED_ENTRY_FLAGS,
                    },
                )
                .is_none(),
            "overlay base archive contains a case-insensitive duplicate path"
        );
    }
    let preserved_entries = entries.len();
    for (path, data) in replacements {
        entries.insert(
            fold_path(path),
            Rho5WriteEntry {
                path: (*path).to_owned(),
                data: (*data).to_vec(),
                flags: P5136_PACKED_ENTRY_FLAGS,
            },
        );
    }
    Ok(MaterializedArchive {
        entries: entries.into_values().collect(),
        preserved_entries,
    })
}

fn write_archive(
    output: &Path,
    name: &str,
    role: &str,
    entries: Vec<Rho5WriteEntry>,
    limits: &Rho5Limits,
) -> Result<BundleArchiveReport> {
    let mut writer = Rho5Writer::new();
    for entry in entries {
        writer.add(entry);
    }
    let encoded = writer.encode(name, Rho5Region::Korea, limits)?;
    let archive_path = output.join(name);
    let sha256 = format!("{:x}", Sha256::digest(encoded.as_bytes()));
    fs::write(&archive_path, encoded.as_bytes())
        .with_context(|| format!("failed to write {}", archive_path.display()))?;
    Ok(BundleArchiveReport {
        name: name.to_owned(),
        role: role.to_owned(),
        bytes: encoded.as_bytes().len(),
        sha256,
        entries: encoded.entry_count(),
    })
}

fn keyed_table_rows(rows: &[XmlRow]) -> Result<HashMap<(String, u16), XmlRow>> {
    let mut output = HashMap::new();
    for row in rows {
        if row.attribute("name").is_none() {
            continue;
        }
        let key = (
            canonical_table_category(&row.element).to_owned(),
            parse_u16(row, "id")?,
        );
        ensure!(
            output.insert(key.clone(), row.clone()).is_none(),
            "target itemTable repeats {} ID {}",
            key.0,
            key.1
        );
    }
    Ok(output)
}

fn canonical_table_category(value: &str) -> &str {
    if value.eq_ignore_ascii_case("flyingPet") {
        "flying_pet"
    } else if value.eq_ignore_ascii_case("character") {
        "character"
    } else if value.eq_ignore_ascii_case("kart") {
        "kart"
    } else if value.eq_ignore_ascii_case("pet") {
        "pet"
    } else {
        value
    }
}

fn shop_category_for_asset(category: &str) -> Option<u16> {
    match category {
        "kart" => Some(3),
        "character" => Some(1),
        "pet" => Some(21),
        "flying_pet" => Some(52),
        _ => None,
    }
}

fn keyed_shop_rows(rows: &[XmlRow]) -> Result<HashMap<(u16, u16), XmlRow>> {
    let mut output = HashMap::new();
    for row in rows {
        let key = (parse_u16(row, "itemCatId")?, parse_u16(row, "itemId")?);
        ensure!(
            output.insert(key, row.clone()).is_none(),
            "shop catalog repeats category {} ID {}",
            key.0,
            key.1
        );
    }
    Ok(output)
}

fn parse_u16(row: &XmlRow, name: &str) -> Result<u16> {
    row.attribute(name)
        .with_context(|| format!("{} row is missing {name}", row.element))?
        .parse()
        .with_context(|| format!("{} row has invalid {name}", row.element))
}

fn sanitize_shop_row(source: &XmlRow, code: &str) -> Result<(XmlRow, usize)> {
    let mut output = source.clone();
    output.replace_attribute("itemName", code.to_owned())?;
    let mut hashed = 0_usize;
    for (key, value) in &mut output.attributes {
        if !key.eq_ignore_ascii_case("itemName") && !value.is_ascii() {
            *value = ascii_hash(value);
            hashed += 1;
        }
        ensure!(value.is_ascii(), "sanitized catalog value is not ASCII");
    }
    Ok((output, hashed))
}

fn ascii_hash(value: &str) -> String {
    let digest = format!("{:x}", Sha256::digest(value.as_bytes()));
    format!("sha256_{}", &digest[..16])
}

fn xml_rows(text: &str, wanted: &[&str]) -> Result<Vec<XmlRow>> {
    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(false);
    let mut output = Vec::new();
    loop {
        match reader.read_event()? {
            Event::Start(element) | Event::Empty(element)
                if wanted.iter().any(|wanted| {
                    element
                        .local_name()
                        .as_ref()
                        .eq_ignore_ascii_case(wanted.as_bytes())
                }) =>
            {
                output.push(read_row(&reader, &element)?);
            }
            Event::DocType(_) => bail!("catalog XML contains a document type"),
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(output)
}

fn read_row(reader: &Reader<&[u8]>, element: &BytesStart<'_>) -> Result<XmlRow> {
    let name = String::from_utf8(element.local_name().as_ref().to_vec())?;
    let mut attributes = Vec::new();
    for attribute in element.attributes().with_checks(true) {
        let attribute = attribute?;
        let key = String::from_utf8(attribute.key.as_ref().to_vec())?;
        let value = attribute
            .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())?
            .into_owned();
        attributes.push((key, value));
    }
    Ok(XmlRow {
        element: name,
        attributes,
    })
}

fn append_rows(text: &str, root: &str, rows: &[XmlRow]) -> Result<String> {
    if rows.is_empty() {
        return Ok(text.to_owned());
    }
    let closing = format!("</{root}>");
    let position = text
        .to_ascii_lowercase()
        .rfind(&closing.to_ascii_lowercase())
        .with_context(|| format!("catalog XML has no closing {closing}"))?;
    let mut output = String::with_capacity(text.len() + rows.len() * 256);
    output.push_str(&text[..position]);
    if !output.ends_with(['\r', '\n']) {
        output.push_str("\r\n");
    }
    for row in rows {
        output.push_str("\t\t");
        output.push_str(&render_row(row)?);
        output.push_str("\r\n");
    }
    output.push_str(&text[position..]);
    Ok(output)
}

fn render_row(row: &XmlRow) -> Result<String> {
    let mut writer = Writer::new(Vec::new());
    let mut element = BytesStart::new(row.element.as_str());
    for (key, value) in &row.attributes {
        element.push_attribute((key.as_str(), value.as_str()));
    }
    writer.write_event(Event::Empty(element))?;
    Ok(String::from_utf8(writer.into_inner())?)
}

fn validate_xml(text: &str) -> Result<()> {
    let mut reader = Reader::from_str(text);
    loop {
        match reader.read_event()? {
            Event::DocType(_) => bail!("generated catalog contains a document type"),
            Event::Eof => return Ok(()),
            _ => {}
        }
    }
}

fn decode_xml(bytes: &[u8]) -> Result<String> {
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.get(1) == Some(&0) {
        let start = usize::from(bytes.starts_with(&[0xff, 0xfe])) * 2;
        let body = &bytes[start..];
        ensure!(
            body.len().is_multiple_of(2),
            "UTF-16 XML has an odd byte count"
        );
        let units = body
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        Ok(String::from_utf16(&units)?
            .trim_start_matches('\u{feff}')
            .to_owned())
    } else if bytes.starts_with(&[0xfe, 0xff]) {
        let body = &bytes[2..];
        ensure!(
            body.len().is_multiple_of(2),
            "UTF-16 XML has an odd byte count"
        );
        let units = body
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        Ok(String::from_utf16(&units)?
            .trim_start_matches('\u{feff}')
            .to_owned())
    } else {
        Ok(std::str::from_utf8(bytes)?
            .trim_start_matches('\u{feff}')
            .to_owned())
    }
}

fn encode_utf16(text: &str) -> Vec<u8> {
    let mut output = Vec::with_capacity(text.len() * 2 + 2);
    output.extend_from_slice(&[0xff, 0xfe]);
    for unit in text.encode_utf16() {
        output.extend_from_slice(&unit.to_le_bytes());
    }
    output
}

fn partition_entries(entries: Vec<Rho5WriteEntry>) -> Vec<Vec<Rho5WriteEntry>> {
    let mut chunks = Vec::new();
    let mut current = Vec::new();
    let mut current_bytes = 0_usize;
    for entry in entries {
        let starts_next = !current.is_empty()
            && current_bytes.saturating_add(entry.data.len()) > MAX_CHUNK_PLAINTEXT_BYTES;
        if starts_next {
            chunks.push(std::mem::take(&mut current));
            current_bytes = 0;
        }
        current_bytes = current_bytes.saturating_add(entry.data.len());
        current.push(entry);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn sequential_archive_names(first: &str, count: usize) -> Result<Vec<String>> {
    let stem = first
        .strip_suffix(".rho5")
        .or_else(|| first.strip_suffix(".RHO5"))
        .context("archive name has no .rho5 suffix")?;
    let (prefix, suffix) = stem
        .rsplit_once('_')
        .context("archive name must end in an underscore and decimal sequence")?;
    ensure!(
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()),
        "archive name must end in a decimal sequence"
    );
    let first_number = suffix.parse::<u32>()?;
    (0..count)
        .map(|offset| {
            let number = first_number
                .checked_add(u32::try_from(offset)?)
                .context("archive sequence overflow")?;
            Ok(format!(
                "{prefix}_{number:0width$}.rho5",
                width = suffix.len()
            ))
        })
        .collect()
}

fn verify_staged_archives(output: &Path, report: &BundleReport) -> Result<()> {
    let directory = p5136_rho5::Rho5Directory::scan_kr(output, Rho5Limits::default())?;
    ensure!(
        directory.archive_count() == report.archives.len(),
        "staging directory contains extra RHO5 files"
    );
    ensure!(
        directory.entries().len()
            == report
                .archives
                .iter()
                .map(|archive| archive.entries)
                .sum::<usize>(),
        "staged RHO5 entry count differs"
    );
    for (path, expected_role) in [
        (TABLE_PATH, "item_table_base"),
        (TARGET_SHOP_PATH, "catalog_overlay"),
    ] {
        let entry = directory.unique_entry(path)?;
        let archive = report
            .archives
            .iter()
            .find(|archive| archive.name.eq_ignore_ascii_case(entry.archive_name()))
            .with_context(|| {
                format!(
                    "staged catalog entry {path:?} came from unreported archive {}",
                    entry.archive_name()
                )
            })?;
        ensure!(
            archive.role == expected_role,
            "staged catalog entry {path:?} is in {} ({}) instead of a {expected_role} archive",
            archive.name,
            archive.role
        );
        let bytes = directory.extract_entry(entry)?;
        validate_xml(&decode_xml(&bytes)?)?;
    }
    for archive in &report.archives {
        let path = output.join(&archive.name);
        ensure!(
            path.is_file(),
            "staged archive disappeared: {}",
            path.display()
        );
        let bytes = fs::read(&path)?;
        ensure!(
            bytes.len() == archive.bytes
                && format!("{:x}", Sha256::digest(&bytes)) == archive.sha256,
            "staged archive verification failed: {}",
            path.display()
        );
    }
    Ok(())
}

fn remove_previous_bundle_archives(output: &Path, report_path: &Path) -> Result<()> {
    if !report_path.is_file() {
        return Ok(());
    }
    let previous: PreviousBundleReport = serde_json::from_slice(
        &fs::read(report_path)
            .with_context(|| format!("failed to read {}", report_path.display()))?,
    )
    .with_context(|| format!("failed to parse {}", report_path.display()))?;
    for archive in previous.archives {
        validate_archive_name(&archive.name)?;
        let path = output.join(archive.name);
        if path.is_file() {
            fs::remove_file(&path)
                .with_context(|| format!("failed to remove old staged file {}", path.display()))?;
        }
    }
    Ok(())
}

fn ensure_same_directory(left: &Path, right: &Path, label: &str) -> Result<()> {
    ensure!(
        fs::canonicalize(left)? == fs::canonicalize(right)?,
        "plan {label} does not match the requested directory"
    );
    Ok(())
}

fn validate_archive_name(value: &str) -> Result<()> {
    ensure!(
        Path::new(value).file_name().and_then(|name| name.to_str()) == Some(value)
            && value.is_ascii()
            && value.to_ascii_lowercase().ends_with(".rho5"),
        "archive-name must be a plain ASCII .rho5 file name"
    );
    Ok(())
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::{
        XmlRow, ascii_hash, korean_region_param_alias, sanitize_shop_row, sequential_archive_names,
        shop_category_for_asset,
    };

    #[test]
    fn shop_localization_is_ascii_and_deterministic() {
        let source = XmlRow {
            element: "item".to_owned(),
            attributes: vec![
                ("itemCatId".to_owned(), "3".to_owned()),
                ("itemId".to_owned(), "1457".to_owned()),
                ("itemName".to_owned(), "Chinese name".to_owned()),
                ("itemDesc".to_owned(), "Chinese description".to_owned()),
                ("itemEffect".to_owned(), "ASCII effect".to_owned()),
            ],
        };
        let mut source = source;
        source
            .replace_attribute("itemName", "\u{4e2d}\u{6587}\u{540d}\u{79f0}".to_owned())
            .unwrap();
        source
            .replace_attribute("itemDesc", "\u{4e2d}\u{6587}\u{8bf4}\u{660e}".to_owned())
            .unwrap();
        let expected_description = ascii_hash("\u{4e2d}\u{6587}\u{8bf4}\u{660e}");

        let (sanitized, hashed) = sanitize_shop_row(&source, "spinteacupV1").unwrap();
        assert_eq!(sanitized.attribute("itemName"), Some("spinteacupV1"));
        assert_eq!(
            sanitized.attribute("itemDesc"),
            Some(expected_description.as_str())
        );
        assert_eq!(sanitized.attribute("itemEffect"), Some("ASCII effect"));
        assert_eq!(hashed, 1);
        assert!(
            sanitized
                .attributes
                .iter()
                .all(|(_, value)| value.is_ascii())
        );
    }

    #[test]
    fn archive_sequence_preserves_width() {
        assert_eq!(
            sequential_archive_names("DataPack1_00002.rho5", 3).unwrap(),
            [
                "DataPack1_00002.rho5",
                "DataPack1_00003.rho5",
                "DataPack1_00004.rho5",
            ]
        );
    }

    #[test]
    fn chinese_kart_parameter_gets_a_korean_region_alias() {
        assert_eq!(
            korean_region_param_alias("kart", "kart_/rollerBrushV1/param@cn.xml").as_deref(),
            Some("kart_/rollerBrushV1/param@kr.xml")
        );
        assert_eq!(
            korean_region_param_alias("kart", "kart_/rollerBrushV1/param.xml"),
            None
        );
        assert_eq!(
            korean_region_param_alias("flying_pet", "flyingPet/flying20year/param@cn.bml")
                .as_deref(),
            Some("flyingPet/flying20year/param@kr.bml")
        );
    }

    #[test]
    fn asset_categories_map_to_stock_shop_categories() {
        assert_eq!(shop_category_for_asset("character"), Some(1));
        assert_eq!(shop_category_for_asset("kart"), Some(3));
        assert_eq!(shop_category_for_asset("pet"), Some(21));
        assert_eq!(shop_category_for_asset("flying_pet"), Some(52));
        assert_eq!(shop_category_for_asset("plate"), None);
    }
}

fn render_markdown(report: &BundleReport) -> String {
    let mut output = String::new();
    writeln!(output, "# P5136 compatible asset bundle\n").expect("String write");
    writeln!(
        output,
        "- Total encoded size: {} bytes",
        report.total_archive_bytes
    )
    .expect("String write");
    writeln!(
        output,
        "- Imported flying-pet `param@cn.bml` -> `param@kr.bml` aliases: {}",
        report.localized_flying_pet_param_aliases
    )
    .expect("String write");
    writeln!(output, "- Resource entries: {}", report.resource_entries).expect("String write");
    writeln!(
        output,
        "- Preserved entries in item-table base: {}",
        report.preserved_table_archive_entries
    )
    .expect("String write");
    writeln!(
        output,
        "- Preserved entries in catalog overlay base: {}",
        report.preserved_catalog_archive_entries
    )
    .expect("String write");
    writeln!(
        output,
        "- Imported kart `param@cn.xml` -> `param@kr.xml` aliases: {}",
        report.localized_kart_param_aliases
    )
    .expect("String write");
    writeln!(
        output,
        "- Imported item abilities: transform={}, fired-to-gain={}, firing-to-gain={}, animal-booster={} ({} unsupported rules skipped)",
        report.item_abilities.transform_by_kart,
        report.item_abilities.fired_to_gain,
        report.item_abilities.firing_to_gain,
        report.item_abilities.animal_booster,
        report.item_abilities.skipped_unsupported,
    )
    .expect("String write");
    for (category, count) in &report.asset_groups {
        writeln!(output, "- {category} groups: {count}").expect("String write");
    }
    writeln!(output, "\n## RHO5 archives\n").expect("String write");
    writeln!(output, "| File | Role | Entries | Bytes | SHA-256 |").expect("String write");
    writeln!(output, "|---|---|---:|---:|---|").expect("String write");
    for archive in &report.archives {
        writeln!(
            output,
            "| `{}` | {} | {} | {} | `{}` |",
            archive.name, archive.role, archive.entries, archive.bytes, archive.sha256
        )
        .expect("String write");
    }
    writeln!(
        output,
        "\nChinese display names are replaced with asset codes. Every other non-ASCII attribute is replaced with a deterministic `sha256_` token.\n"
    )
    .expect("String write");
    writeln!(
        output,
        "| Category | ID | Code | itemTable | Shop | Hashed fields |"
    )
    .expect("String write");
    writeln!(output, "|---|---:|---|---|---|---:|").expect("String write");
    for item in &report.catalog_items {
        writeln!(
            output,
            "| {} | {} | `{}` | {} | {} | {} |",
            item.category, item.id, item.code, item.table, item.shop, item.hashed_fields
        )
        .expect("String write");
    }
    if !report.resource_only_assets.is_empty() {
        writeln!(output, "\n## Resource-only groups without a catalog ID\n").expect("String write");
        for asset in &report.resource_only_assets {
            writeln!(output, "- `{asset}`").expect("String write");
        }
    }
    output
}
