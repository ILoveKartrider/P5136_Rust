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
    COMPATIBILITY_ASSERTION, EXPERIMENTAL_NATIVE_ASSERTION,
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
    xun_tachometer_compatibility_patches: usize,
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
        let explicitly_selected = selected_assets.is_some_and(|selected| selected.contains(&key));
        let accepted_status = asset.status == "compatible_candidate"
            || (asset.status == "experimental_native_candidate" && explicitly_selected);
        if category_set.contains(&asset.category.to_ascii_lowercase())
            && accepted_status
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
        let expected_assertion = if asset.status == "experimental_native_candidate" {
            EXPERIMENTAL_NATIVE_ASSERTION
        } else {
            COMPATIBILITY_ASSERTION
        };
        ensure!(
            manifest.compatibility == expected_assertion,
            "{} has an invalid compatibility assertion",
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
    let mut xun_tachometer_compatibility_patches = 0_usize;
    for (index, file) in pending.values().enumerate() {
        if index != 0 && index.is_multiple_of(250) {
            eprintln!("extracted {index}/{} resource entries", pending.len());
        }
        let record = source
            .effective(&file.source_path)
            .with_context(|| format!("planned source path disappeared: {}", file.source_path))?;
        let bytes = extractor.extract(record)?;
        verify_sha256(&bytes, &file.expected_sha256, &file.source_path)?;
        let (bytes, patched_xun_tachometer_resource) =
            normalize_imported_xun_resource(&file.path, &bytes)?;
        xun_tachometer_compatibility_patches += usize::from(patched_xun_tachometer_resource);
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
        xun_tachometer_compatibility_patches,
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
    let (target_table_text, _) = normalize_xun_kart_table_text(&target_table_text)?;
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
    let mut table_codes = HashSet::new();

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
        let code_key = format!("{category}:{}", code.to_ascii_lowercase());
        table_codes.insert(code_key.clone());
        let id = parse_u16(source_row, "id")?;
        let key = (category.clone(), id);
        let shop_category = shop_category_for_asset(&category)
            .expect("itemTable rows were filtered to supported asset categories");
        let shop_key = (shop_category, id);

        // Some modern item tables retain hidden/derived rows that deliberately
        // have no public shop-catalog entry.  In particular, cottonXUN_20year
        // appears as both ID 1603 (published) and ID 1604 (orphan).  Importing
        // both rows either aborts here or exposes an inventory entry that the
        // client cannot describe.  Keep only rows backed by either catalog.
        if !target_shop_keys.contains_key(&shop_key) && !source_shop_by_key.contains_key(&shop_key)
        {
            continue;
        }

        mapped_codes.insert(code_key);
        let table_state = if let Some(target_row) = target_table_by_key.get(&key) {
            ensure!(
                target_row
                    .attribute("name")
                    .is_some_and(|target| target.eq_ignore_ascii_case(code)),
                "target itemTable ID collision: {category} {id} is not {code}"
            );
            "existing"
        } else {
            let mut compatible_row = source_row.clone();
            normalize_xun_kart_table_row(&mut compatible_row)?;
            table_append.push(compatible_row);
            "added"
        };
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
                ensure!(
                    !table_codes.contains(&key),
                    "source shop catalog has no published row for {category} {code}"
                );
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

/// Makes the modern XUN tachometer resource consumable by P5136's V1 ABI.
///
/// The injected sidecar resolves `XunGenTacho` to a native V1-layout object.
/// Modern XUN BML renamed four nodes that the P5136 V1 initializer requires;
/// without these aliases it calls `front()` on an empty lookup result and
/// faults at 0x4F6ED1. XUN-only nodes remain intact for the sidecar overlay.
pub(crate) fn normalize_imported_xun_resource(path: &str, bytes: &[u8]) -> Result<(Vec<u8>, bool)> {
    let folded = fold_path(path);
    if folded == "gui/tachometer/xun/tacho.bml" {
        return normalize_xun_tachometer_bml(bytes);
    }

    let Some(file_name) = folded.rsplit('/').next() else {
        return Ok((bytes.to_vec(), false));
    };
    if !folded.starts_with("kart_/")
        || !file_name.starts_with("param")
        || !Path::new(file_name)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("xml"))
    {
        return Ok((bytes.to_vec(), false));
    }

    let mut text = decode_xml(bytes)?;
    if !xml_attribute_equals(&text, "TachometerName", "xun") {
        return Ok((bytes.to_vec(), false));
    }
    // Migrate bundles produced by the earlier crash-only V1 fallback back to
    // the real factory name now handled by the sidecar lookup hook.
    let mut changed =
        replace_xml_attribute_value(&mut text, "TachometerType", "V1GenTacho", "XunGenTacho");
    // P5136's native Exceed renderer still selects its wave through this
    // BodyParam attribute. Modern XUN parameters replaced the explicit wave
    // name with defaultExceedType, so translate only that selector here. The
    // sidecar keeps the XUN charger aura on its own resource/state path.
    if let Some(wave) =
        xml_attribute_value(&text, "defaultExceedType").and_then(xun_exceed_wave_type)
    {
        changed |= set_xml_attribute(&mut text, "ExceedWaveType", wave)?;
    }
    if !changed {
        return Ok((bytes.to_vec(), false));
    }
    validate_xml(&text)?;
    Ok((encode_utf16(&text), true))
}

/// Accepts only migrations produced by this XUN normalizer.
///
/// Compare canonicalized resources so bundles produced by earlier revisions
/// can be upgraded without granting a general overwrite exception.
#[allow(dead_code)]
pub(crate) fn is_safe_imported_xun_migration(
    path: &str,
    existing: &[u8],
    staged: &[u8],
) -> Result<bool> {
    let (normalized, changed) = normalize_imported_xun_resource(path, existing)?;
    let (staged_normalized, staged_changed) = normalize_imported_xun_resource(path, staged)?;
    if !changed || staged_changed {
        return Ok(false);
    }
    if normalized == staged_normalized {
        return Ok(true);
    }

    let folded = fold_path(path);
    if !folded.starts_with("kart_/")
        || !Path::new(&folded)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("xml"))
    {
        return Ok(false);
    }
    let normalized = decode_xml(&normalized)?;
    let staged = decode_xml(&staged_normalized)?;
    Ok(normalized
        .lines()
        .filter(|line| !line.trim().is_empty())
        .eq(staged.lines().filter(|line| !line.trim().is_empty())))
}

#[allow(clippy::too_many_lines)]
fn normalize_xun_tachometer_bml(bytes: &[u8]) -> Result<(Vec<u8>, bool)> {
    let limits = BmlLimits {
        max_depth: 32,
        max_nodes: 200_000,
        max_attributes_per_node: 512,
        max_children_per_node: 100_000,
        max_string_code_units: 8_192,
    };
    let mut reader = PacketReader::new(bytes);
    let mut root = BmlNode::decode_with_limits(&mut reader, limits)
        .context("failed to decode imported XUN tachometer BML")?;
    ensure!(
        reader.remaining().is_empty(),
        "imported XUN tachometer BML contains trailing bytes"
    );

    let mut changed = false;
    let mut pending = vec![&mut root];
    while let Some(node) = pending.pop() {
        if let Some((_, name)) = node
            .attributes
            .iter_mut()
            .find(|(key, _)| key.eq_ignore_ascii_case("name"))
        {
            let replacement = if name.eq_ignore_ascii_case("xungen_bg1") {
                Some("v1gen_bg1")
            } else if name.eq_ignore_ascii_case("xungen_bg2") {
                Some("v1gen_bg2")
            } else if name.eq_ignore_ascii_case("bg_engineIcon1") {
                Some("n2o")
            } else if name.eq_ignore_ascii_case("bg_engineIcon2") {
                Some("n2o_always")
            } else if name.eq_ignore_ascii_case("xunLegacyRoad1") {
                Some("blinkRoad1")
            } else if name.eq_ignore_ascii_case("xunLegacyRoad2") {
                Some("blinkRoad2")
            } else if name.eq_ignore_ascii_case("xunLegacyRoad3") {
                Some("blinkRoad3")
            } else {
                None
            };
            if let Some(replacement) = replacement {
                replacement.clone_into(name);
                changed = true;
            }
        }
        pending.extend(node.children.iter_mut());
    }

    // P5136's V1 flat-gauge controller reads this misspelled legacy property
    // from the tachometer root. The modern XUN resource relies on the later
    // client's implicit default instead, so make the P5136 contract explicit.
    changed |= set_bml_attribute(&mut root, "instAccelFullLenth", "1000");

    // P5136's V1 flat-gauge controller clips and moves these three surfaces,
    // but the later XUN class normally performs their first visibility
    // transition. P5136 never toggles the later `exceedFeatures` parent, so
    // expose that parent while keeping its XUN-only idling/usable/full
    // overlays hidden. The stock controller then owns only the ordinary
    // Exceed gauge leaves, independently from the charger display.
    if let Some(exceed_features) = find_bml_node_mut(&mut root, "exceedFeatures") {
        changed |= set_bml_attribute(exceed_features, "visible", "true");
        for name in ["idling", "usable", "playFull"] {
            if let Some(overlay) = find_bml_node_mut(exceed_features, name) {
                changed |= set_bml_attribute(overlay, "visible", "false");
            }
        }
    }
    if let Some(inst_accel) = find_bml_node_mut(&mut root, "instAccel") {
        // Keep the modern XUN layout geometry intact. Converting this Window
        // into a textured Panel changes how its existing adjust/anchor pair is
        // interpreted and moved a previously-correct Exceed gauge. Also undo
        // that migration for bundles generated by the earlier patch.
        if !inst_accel.name.eq_ignore_ascii_case("Window") {
            "Window".clone_into(&mut inst_accel.name);
            changed = true;
        }
        changed |= set_bml_attribute(inst_accel, "leftTopTex", "0 512");
        changed |= remove_bml_attribute(inst_accel, "texture");
        changed |= set_bml_attribute(inst_accel, "visible", "true");
    }
    if let Some(inst_accel_gauge) = find_bml_node_mut(&mut root, "instAccelGauge") {
        changed |= set_bml_attribute(inst_accel_gauge, "visible", "true");
    }
    if let Some(inst_accel_bar) = find_bml_node_mut(&mut root, "instAccelBar") {
        changed |= set_bml_attribute(inst_accel_bar, "visible", "true");
    }
    // These are activation animations, not the charge gauge itself. The
    // sidecar starts/stops the kart-attached charger effect independently;
    // leaving either dashboard scene visible here would show a permanent
    // full-charge image before the first booster is used.
    for name in ["charger", "charger2"] {
        if let Some(panel) = find_bml_node_mut(&mut root, name) {
            changed |= set_bml_attribute(panel, "visible", "false");
        }
    }

    // The later XUN UI draws three speed-number layers. P5136's V1 controller
    // updates only the legacy `kmh` layer, leaving `kmh2` and `kmh3` at their
    // BML default text (`0`) as a permanent background. Keep the live layer and
    // hide only the two unsupported decorative duplicates.
    for name in ["kmh2", "kmh3"] {
        if let Some(panel) = find_bml_node_mut(&mut root, name) {
            changed |= set_bml_attribute(panel, "visible", "false");
        }
    }

    // V1's update routine unconditionally toggles `n2o/on` at 0x006C1714.
    // Modern XUN flattened the two engine-icon states into sibling panels
    // (`bg_engineIcon1/2`), so aliasing only their names leaves this one
    // required child pointer null. Reuse the second modern icon as the V1
    // active-state child while retaining its sibling alias for the separate
    // `n2o_always` field.
    let n2o_on_template = find_bml_node(&root, "n2o_always").cloned();
    if let (Some(n2o), Some(mut active_icon)) =
        (find_bml_node_mut(&mut root, "n2o"), n2o_on_template)
    {
        let has_active_child = n2o.children.iter().any(|node| {
            xun_bml_attribute(node, "name").is_some_and(|name| name.eq_ignore_ascii_case("on"))
        });
        if !has_active_child {
            set_bml_attribute(&mut active_icon, "name", "on");
            set_bml_attribute(&mut active_icon, "leftTopTex", "0 0");
            set_bml_attribute(&mut active_icon, "visible", "false");
            remove_bml_attribute(&mut active_icon, "windowSize");
            remove_bml_attribute(&mut active_icon, "adjust");
            remove_bml_attribute(&mut active_icon, "align");
            active_icon.children.clear();
            n2o.children.push(active_icon);
            changed = true;
        }
    }

    // Preserve the modern anchor/adjust contract. The sidecar binds a fourth
    // native flat-gauge controller to these exact names and supplies a
    // continuous 0..1 fraction. This hierarchy is separate from instAccel
    // (ordinary Exceed) and from the dashboard blinkRoad state indicators.
    if let Some(charger_features) = find_bml_node_mut(&mut root, "chargerFeatures") {
        changed |= set_bml_attribute(charger_features, "leftTopWH", "0 0 396 74");
    }
    let charger = find_bml_node_mut(&mut root, "instCharger");
    if let Some(charger) = charger {
        if !charger.name.eq_ignore_ascii_case("Window") {
            "Window".clone_into(&mut charger.name);
            changed = true;
        }
        changed |= remove_bml_attribute(charger, "leftTopWH");
        changed |= set_bml_attribute(charger, "visible", "true");
        // Remove the three synthetic slices emitted by the former coarse
        // compatibility patch. They shadowed the real dashboard blinkRoad
        // nodes and turned a continuous 396-pixel gauge into three steps.
        let original_len = charger.children.len();
        charger.children.retain(|node| {
            !xun_bml_attribute(node, "name").is_some_and(|name| {
                ["blinkRoad1", "blinkRoad2", "blinkRoad3"]
                    .iter()
                    .any(|candidate| name.eq_ignore_ascii_case(candidate))
            })
        });
        changed |= charger.children.len() != original_len;
        let gauge_index = charger.children.iter().position(|node| {
            xun_bml_attribute(node, "name").is_some_and(|name| {
                name.eq_ignore_ascii_case("instChargerGauge")
                    || name.eq_ignore_ascii_case("xunInstChargerGauge")
            })
        });
        if let Some(gauge_index) = gauge_index {
            changed |= set_bml_attribute(
                &mut charger.children[gauge_index],
                "name",
                "instChargerGauge",
            );
            changed |= set_bml_attribute(&mut charger.children[gauge_index], "visible", "true");
        }
    }
    if !changed {
        return Ok((bytes.to_vec(), false));
    }

    let mut writer = PacketWriter::new();
    root.encode_with_limits(&mut writer, limits)
        .context("failed to encode compatible XUN tachometer BML")?;
    Ok((writer.into_inner(), true))
}

fn xun_bml_attribute<'a>(node: &'a BmlNode, wanted: &str) -> Option<&'a str> {
    node.attributes
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(wanted))
        .map(|(_, value)| value.as_str())
}

fn set_bml_attribute(node: &mut BmlNode, wanted: &str, value: &str) -> bool {
    if let Some((_, existing)) = node
        .attributes
        .iter_mut()
        .find(|(name, _)| name.eq_ignore_ascii_case(wanted))
    {
        if existing == value {
            return false;
        }
        value.clone_into(existing);
    } else {
        node.attributes.push((wanted.to_owned(), value.to_owned()));
    }
    true
}

fn remove_bml_attribute(node: &mut BmlNode, wanted: &str) -> bool {
    let old_len = node.attributes.len();
    node.attributes
        .retain(|(name, _)| !name.eq_ignore_ascii_case(wanted));
    node.attributes.len() != old_len
}

fn find_bml_node<'a>(node: &'a BmlNode, wanted: &str) -> Option<&'a BmlNode> {
    if xun_bml_attribute(node, "name").is_some_and(|name| name.eq_ignore_ascii_case(wanted)) {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| find_bml_node(child, wanted))
}

fn find_bml_node_mut<'a>(node: &'a mut BmlNode, wanted: &str) -> Option<&'a mut BmlNode> {
    if xun_bml_attribute(node, "name").is_some_and(|name| name.eq_ignore_ascii_case(wanted)) {
        return Some(node);
    }
    node.children
        .iter_mut()
        .find_map(|child| find_bml_node_mut(child, wanted))
}

#[cfg(test)]
fn has_bml_node_named(node: &BmlNode, wanted: &str) -> bool {
    xun_bml_attribute(node, "name").is_some_and(|name| name.eq_ignore_ascii_case(wanted))
        || node
            .children
            .iter()
            .any(|child| has_bml_node_named(child, wanted))
}

fn xml_attribute_equals(text: &str, attribute: &str, expected: &str) -> bool {
    let folded = text.to_ascii_lowercase();
    let name = attribute.to_ascii_lowercase();
    let bytes = folded.as_bytes();
    let mut cursor = 0_usize;
    while cursor < bytes.len() {
        let Some(relative) = folded[cursor..].find(&name) else {
            return false;
        };
        let mut index = cursor + relative + name.len();
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        if bytes.get(index) != Some(&b'=') {
            cursor = index;
            continue;
        }
        index += 1;
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        let Some(&quote @ (b'\'' | b'"')) = bytes.get(index) else {
            cursor = index;
            continue;
        };
        let value_start = index + 1;
        let Some(value_length) = bytes[value_start..]
            .iter()
            .position(|value| *value == quote)
        else {
            return false;
        };
        let value_end = value_start + value_length;
        return text[value_start..value_end].eq_ignore_ascii_case(expected);
    }
    false
}

fn xml_attribute_value<'a>(text: &'a str, attribute: &str) -> Option<&'a str> {
    let folded = text.to_ascii_lowercase();
    let name = attribute.to_ascii_lowercase();
    let bytes = folded.as_bytes();
    let mut cursor = 0_usize;
    while cursor < bytes.len() {
        let relative = folded[cursor..].find(&name)?;
        let name_start = cursor + relative;
        let name_end = name_start + name.len();
        let left_boundary = name_start == 0
            || !bytes[name_start - 1].is_ascii_alphanumeric() && bytes[name_start - 1] != b'_';
        let right_boundary = bytes
            .get(name_end)
            .is_none_or(|value| !value.is_ascii_alphanumeric() && *value != b'_');
        if !left_boundary || !right_boundary {
            cursor = name_end;
            continue;
        }
        let mut equals = name_end;
        while bytes.get(equals).is_some_and(u8::is_ascii_whitespace) {
            equals += 1;
        }
        if bytes.get(equals) != Some(&b'=') {
            cursor = name_end;
            continue;
        }
        let mut quote_index = equals + 1;
        while bytes.get(quote_index).is_some_and(u8::is_ascii_whitespace) {
            quote_index += 1;
        }
        let &quote @ (b'\'' | b'"') = bytes.get(quote_index)? else {
            cursor = name_end;
            continue;
        };
        let value_start = quote_index + 1;
        let value_length = bytes[value_start..]
            .iter()
            .position(|value| *value == quote)?;
        return Some(&text[value_start..value_start + value_length]);
    }
    None
}

fn xun_exceed_wave_type(value: &str) -> Option<&'static str> {
    match value.trim() {
        "1" => Some("Exd_Wave_C"),
        "2" => Some("Exd_Wave_S"),
        "3" => Some("Exd_Wave_B"),
        "4" => Some("Exd_Wave_L"),
        _ => None,
    }
}

fn set_xml_attribute(text: &mut String, attribute: &str, value: &str) -> Result<bool> {
    if let Some(current) = xml_attribute_value(text, attribute) {
        if current.eq_ignore_ascii_case(value) {
            return Ok(false);
        }
        let value_start = current.as_ptr() as usize - text.as_ptr() as usize;
        let value_end = value_start + current.len();
        text.replace_range(value_start..value_end, value);
        return Ok(true);
    }

    let folded = text.to_ascii_lowercase();
    let root_start = folded
        .find("<bodyparam")
        .context("XUN kart parameter is missing its BodyParam root")?;
    let close = text[root_start..]
        .find('>')
        .map(|relative| root_start + relative)
        .context("XUN BodyParam start tag is unterminated")?;
    let insert_at = if text.as_bytes().get(close.wrapping_sub(1)) == Some(&b'/') {
        close - 1
    } else {
        close
    };
    let opening = &text[root_start..insert_at];
    let insertion = if opening.contains('\n') {
        let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
        format!("\t{attribute}='{value}'{newline}")
    } else {
        format!(" {attribute}='{value}'")
    };
    text.insert_str(insert_at, &insertion);
    Ok(true)
}

fn replace_xml_attribute_value(
    text: &mut String,
    attribute: &str,
    expected: &str,
    replacement: &str,
) -> bool {
    let folded = text.to_ascii_lowercase();
    let name = attribute.to_ascii_lowercase();
    let bytes = folded.as_bytes();
    let mut cursor = 0_usize;
    while cursor < bytes.len() {
        let Some(relative) = folded[cursor..].find(&name) else {
            return false;
        };
        let name_start = cursor + relative;
        let mut index = name_start + name.len();
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        if bytes.get(index) != Some(&b'=') {
            cursor = index;
            continue;
        }
        index += 1;
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        let Some(&quote @ (b'\'' | b'"')) = bytes.get(index) else {
            cursor = index;
            continue;
        };
        let value_start = index + 1;
        let Some(value_length) = bytes[value_start..]
            .iter()
            .position(|value| *value == quote)
        else {
            return false;
        };
        let value_end = value_start + value_length;
        if text[value_start..value_end].eq_ignore_ascii_case(expected) {
            text.replace_range(value_start..value_end, replacement);
            return true;
        }
        cursor = value_end + 1;
    }
    false
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

fn normalize_xun_kart_table_row(row: &mut XmlRow) -> Result<bool> {
    if !row.element.eq_ignore_ascii_case("kart")
        || !row
            .attribute("name")
            .is_some_and(|name| name.to_ascii_lowercase().contains("xun"))
    {
        return Ok(false);
    }

    // P5136's catalog/UI tables only define the V1 generation and grade. The
    // sidecar restores XUN's separate body/parts display conversion, while the
    // stock client still receives values it can safely classify everywhere
    // else in the inventory and tuning UI.
    let mut changed = false;
    if row.attribute("grade") == Some("13") {
        row.replace_attribute("grade", "12".to_owned())?;
        changed = true;
    }
    if row.attribute("engineGrade") == Some("9") {
        row.replace_attribute("engineGrade", "8".to_owned())?;
        changed = true;
    }
    Ok(changed)
}

fn normalize_xun_kart_table_text(text: &str) -> Result<(String, usize)> {
    let mut output = text.to_owned();
    let mut cursor = 0_usize;
    let mut changed = 0_usize;
    loop {
        let folded = output.to_ascii_lowercase();
        let Some(relative_start) = folded[cursor..].find("<kart") else {
            break;
        };
        let start = cursor + relative_start;
        let Some(relative_end) = output[start..].find('>') else {
            bail!("itemTable kart row is not closed");
        };
        let end = start + relative_end + 1;
        let wrapper = format!("<root>{}</root>", &output[start..end]);
        let mut rows = xml_rows(&wrapper, &["kart"])?;
        let Some(mut row) = rows.pop() else {
            cursor = end;
            continue;
        };
        if normalize_xun_kart_table_row(&mut row)? {
            let replacement = render_row(&row)?;
            output.replace_range(start..end, &replacement);
            cursor = start + replacement.len();
            changed += 1;
        } else {
            cursor = end;
        }
    }
    Ok((output, changed))
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
        BmlLimits, BmlNode, PacketReader, PacketWriter, XmlRow, ascii_hash,
        is_safe_imported_xun_migration, korean_region_param_alias, normalize_imported_xun_resource,
        normalize_xun_kart_table_text, sanitize_shop_row, sequential_archive_names,
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
    fn old_xun_v1_fallback_is_restored_to_the_sidecar_factory() {
        let source = "<?xml version='1.0' encoding='UTF-16'?><BodyParam TachometerType='V1GenTacho' TachometerName = \"xun\"/>";
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend(source.encode_utf16().flat_map(u16::to_le_bytes));

        let (normalized, changed) =
            normalize_imported_xun_resource("kart_/mancarXUN/param@kr.xml", &bytes).unwrap();
        let text = super::decode_xml(&normalized).unwrap();

        assert!(changed);
        assert!(text.contains("TachometerType='XunGenTacho'"));
        assert!(text.contains("TachometerName = \"xun\""));
        assert!(!text.contains("ExceedWaveType"));
    }

    #[test]
    fn xun_param_restores_native_exceed_wave_without_conflating_charger() {
        let source = "<?xml version='1.0' encoding='UTF-16'?><BodyParam TachometerType='XunGenTacho' TachometerName='xun' defaultExceedType='4'/>";
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend(source.encode_utf16().flat_map(u16::to_le_bytes));

        let (normalized, changed) =
            normalize_imported_xun_resource("kart_/slrProXUN/param@kr.xml", &bytes).unwrap();
        let text = super::decode_xml(&normalized).unwrap();
        assert!(changed);
        assert!(text.contains("ExceedWaveType='Exd_Wave_L'"));

        let (second, second_changed) =
            normalize_imported_xun_resource("kart_/slrProXUN/param@kr.xml", &normalized).unwrap();
        assert!(!second_changed);
        assert_eq!(second, normalized);
    }

    #[test]
    fn xun_migration_accepts_only_the_canonical_exceed_selector_upgrade() {
        let existing = "<?xml version='1.0' encoding='UTF-16'?>\r\n<BodyParam\r\n\tTachometerType='XunGenTacho'\r\n\tTachometerName='xun'\r\n\tdefaultExceedType='4'\r\n>\r\n</BodyParam>\r\n";
        let staged = "<?xml version='1.0' encoding='UTF-16'?>\r\n<BodyParam\r\n\tTachometerType='XunGenTacho'\r\n\tTachometerName='xun'\r\n\tdefaultExceedType='4'\r\n\tExceedWaveType='Exd_Wave_L'\r\n>\r\n</BodyParam>\r\n";
        let encode = |text: &str| {
            let mut bytes = vec![0xff, 0xfe];
            bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
            bytes
        };

        assert!(
            is_safe_imported_xun_migration(
                "kart_/slrProXUN/param@kr.xml",
                &encode(existing),
                &encode(staged),
            )
            .unwrap()
        );

        let changed_value = staged.replace("TachometerName='xun'", "TachometerName='v1'");
        assert!(
            !is_safe_imported_xun_migration(
                "kart_/slrProXUN/param@kr.xml",
                &encode(existing),
                &encode(&changed_value),
            )
            .unwrap()
        );
    }

    #[test]
    fn xun_item_profile_maps_default_exceed_type_one_to_c_wave() {
        let source = "<?xml version='1.0' encoding='UTF-16'?>\n<BodyParam\n\tTachometerType='XunGenTacho'\n\tTachometerName='xun'\n\tdefaultExceedType='1'\n>\n</BodyParam>\n";
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend(source.encode_utf16().flat_map(u16::to_le_bytes));

        let (normalized, changed) =
            normalize_imported_xun_resource("kart_/mancarXUN/param@kr.xml", &bytes).unwrap();
        let text = super::decode_xml(&normalized).unwrap();
        assert!(changed);
        assert!(text.contains("ExceedWaveType='Exd_Wave_C'"));
    }

    #[test]
    fn xun_param_corrects_a_stale_exceed_wave_selector() {
        let source = "<?xml version='1.0' encoding='UTF-16'?><BodyParam TachometerType='XunGenTacho' TachometerName='xun' defaultExceedType='3' ExceedWaveType='Exd_Wave_S'/>";
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend(source.encode_utf16().flat_map(u16::to_le_bytes));

        let (normalized, changed) =
            normalize_imported_xun_resource("kart_/testXUN/param@kr.xml", &bytes).unwrap();
        let text = super::decode_xml(&normalized).unwrap();
        assert!(changed);
        assert!(text.contains("ExceedWaveType='Exd_Wave_B'"));
        assert!(!text.contains("ExceedWaveType='Exd_Wave_S'"));
    }

    #[test]
    fn xun_catalog_uses_safe_v1_generation_while_sidecar_restores_display_conversion() {
        let source = r#"<itemtable><kart id="1574" name="slrProXUN" uniqueLevel="2" grade="13" engineGrade="9" kartType="2"/><kart id="1486" name="artemisV1" grade="12" engineGrade="8"/></itemtable>"#;
        let (normalized, changed) = normalize_xun_kart_table_text(source).unwrap();
        assert_eq!(changed, 1);
        assert!(normalized.contains("name=\"slrProXUN\""));
        assert!(normalized.contains("grade=\"12\" engineGrade=\"8\""));
        assert!(normalized.contains("name=\"artemisV1\" grade=\"12\" engineGrade=\"8\""));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn xun_tachometer_bml_keeps_modern_nodes_and_restores_v1_required_names() {
        let limits = BmlLimits {
            max_depth: 8,
            max_nodes: 64,
            max_attributes_per_node: 16,
            max_children_per_node: 64,
            max_string_code_units: 128,
        };
        let mut root = BmlNode::new("Container", "");
        for name in [
            "xungen_bg1",
            "xungen_bg2",
            "bg_engineIcon1",
            "bg_engineIcon2",
        ] {
            let mut child = BmlNode::new("Panel", "");
            child.attributes.push(("name".to_owned(), name.to_owned()));
            root.children.push(child);
        }
        for name in ["blinkRoad1", "blinkRoad2", "blinkRoad3"] {
            let mut child = BmlNode::new("Panel", "");
            child.attributes.push(("name".to_owned(), name.to_owned()));
            root.children.push(child);
        }
        let mut charger = BmlNode::new("Panel", "");
        charger
            .attributes
            .push(("name".to_owned(), "instCharger".to_owned()));
        let mut gauge = BmlNode::new("Panel", "");
        gauge
            .attributes
            .push(("name".to_owned(), "instChargerGauge".to_owned()));
        gauge
            .attributes
            .push(("visible".to_owned(), "true".to_owned()));
        charger.children.push(gauge);
        root.children.push(charger);
        let mut charger_features = BmlNode::new("Container", "");
        charger_features
            .attributes
            .push(("name".to_owned(), "chargerFeatures".to_owned()));
        charger_features
            .attributes
            .push(("leftTopWH".to_owned(), "0 0 396 74".to_owned()));
        root.children.push(charger_features);
        let mut exceed_features = BmlNode::new("Container", "");
        exceed_features
            .attributes
            .push(("name".to_owned(), "exceedFeatures".to_owned()));
        exceed_features
            .attributes
            .push(("visible".to_owned(), "false".to_owned()));
        for name in ["idling", "usable", "playFull"] {
            let mut overlay = BmlNode::new("Panel", "");
            overlay
                .attributes
                .push(("name".to_owned(), name.to_owned()));
            overlay
                .attributes
                .push(("visible".to_owned(), "true".to_owned()));
            exceed_features.children.push(overlay);
        }
        let mut inst_accel = BmlNode::new("Window", "");
        inst_accel
            .attributes
            .push(("name".to_owned(), "instAccel".to_owned()));
        inst_accel
            .attributes
            .push(("visible".to_owned(), "false".to_owned()));
        let mut inst_accel_gauge = BmlNode::new("Panel", "");
        inst_accel_gauge
            .attributes
            .push(("name".to_owned(), "instAccelGauge".to_owned()));
        inst_accel_gauge
            .attributes
            .push(("visible".to_owned(), "false".to_owned()));
        let mut inst_accel_bar = BmlNode::new("Panel", "");
        inst_accel_bar
            .attributes
            .push(("name".to_owned(), "instAccelBar".to_owned()));
        inst_accel_bar
            .attributes
            .push(("visible".to_owned(), "false".to_owned()));
        inst_accel.children.push(inst_accel_gauge);
        inst_accel.children.push(inst_accel_bar);
        exceed_features.children.push(inst_accel);
        root.children.push(exceed_features);
        for name in ["charger", "charger2"] {
            let mut panel = BmlNode::new("Play1SPanel", "");
            panel.attributes.push(("name".to_owned(), name.to_owned()));
            panel
                .attributes
                .push(("visible".to_owned(), "true".to_owned()));
            root.children.push(panel);
        }
        for name in ["kmh", "kmh2", "kmh3"] {
            let mut panel = BmlNode::new("CharPanel", "");
            panel.attributes.push(("name".to_owned(), name.to_owned()));
            panel
                .attributes
                .push(("visible".to_owned(), "true".to_owned()));
            panel.attributes.push(("text".to_owned(), "0".to_owned()));
            root.children.push(panel);
        }
        let mut encoded = PacketWriter::new();
        root.encode_with_limits(&mut encoded, limits).unwrap();

        let (normalized, changed) =
            normalize_imported_xun_resource("gui/tachometer/xun/tacho.bml", &encoded.into_inner())
                .unwrap();
        assert!(changed);
        let mut reader = PacketReader::new(&normalized);
        let decoded = BmlNode::decode_with_limits(&mut reader, limits).unwrap();
        for expected in [
            "v1gen_bg1",
            "v1gen_bg2",
            "n2o",
            "n2o_always",
            "instChargerGauge",
            "blinkRoad1",
            "blinkRoad2",
            "blinkRoad3",
        ] {
            assert!(
                super::has_bml_node_named(&decoded, expected),
                "missing {expected}"
            );
        }
        let n2o = super::find_bml_node(&decoded, "n2o").unwrap();
        let n2o_on = n2o
            .children
            .iter()
            .find(|node| super::xun_bml_attribute(node, "name") == Some("on"))
            .expect("V1 update requires the nested n2o/on state panel");
        assert_eq!(super::xun_bml_attribute(n2o_on, "visible"), Some("false"));
        assert_eq!(super::xun_bml_attribute(n2o_on, "leftTopTex"), Some("0 0"));
        assert_eq!(
            super::xun_bml_attribute(&decoded, "instAccelFullLenth"),
            Some("1000")
        );
        for name in ["instAccel", "instAccelGauge", "instAccelBar"] {
            let node = super::find_bml_node(&decoded, name).unwrap();
            assert_eq!(
                super::xun_bml_attribute(node, "visible"),
                Some("true"),
                "P5136 V1 Exceed gauge node {name} must be drawable by the stock controller"
            );
        }
        let inst_accel = super::find_bml_node(&decoded, "instAccel").unwrap();
        assert_eq!(inst_accel.name, "Window");
        assert_eq!(super::xun_bml_attribute(inst_accel, "texture"), None);
        assert_eq!(
            super::xun_bml_attribute(inst_accel, "leftTopTex"),
            Some("0 512")
        );
        let exceed_features = super::find_bml_node(&decoded, "exceedFeatures").unwrap();
        assert_eq!(
            super::xun_bml_attribute(exceed_features, "visible"),
            Some("true")
        );
        for name in ["idling", "usable", "playFull"] {
            let node = super::find_bml_node(exceed_features, name).unwrap();
            assert_eq!(
                super::xun_bml_attribute(node, "visible"),
                Some("false"),
                "modern-only Exceed overlay {name} must not start permanently visible"
            );
        }
        for name in ["charger", "charger2"] {
            let node = super::find_bml_node(&decoded, name).unwrap();
            assert_eq!(
                super::xun_bml_attribute(node, "visible"),
                Some("false"),
                "modern-only XUN animation {name} must not start permanently visible"
            );
        }
        assert_eq!(
            super::xun_bml_attribute(super::find_bml_node(&decoded, "kmh").unwrap(), "visible"),
            Some("true")
        );
        for name in ["kmh2", "kmh3"] {
            assert_eq!(
                super::xun_bml_attribute(super::find_bml_node(&decoded, name).unwrap(), "visible"),
                Some("false"),
                "unsupported decorative speed layer {name} must not leave a static zero"
            );
        }
        let charger = decoded
            .children
            .iter()
            .find(|node| super::xun_bml_attribute(node, "name") == Some("instCharger"))
            .unwrap();
        assert_eq!(charger.name, "Window");
        assert_eq!(super::xun_bml_attribute(charger, "leftTopWH"), None);
        let charger_features = super::find_bml_node(&decoded, "chargerFeatures").unwrap();
        assert_eq!(
            super::xun_bml_attribute(charger_features, "leftTopWH"),
            Some("0 0 396 74")
        );
        let gauge = charger
            .children
            .iter()
            .find(|node| super::xun_bml_attribute(node, "name") == Some("instChargerGauge"))
            .unwrap();
        assert_eq!(super::xun_bml_attribute(gauge, "visible"), Some("true"));
        assert!(charger.children.iter().all(|node| {
            !super::xun_bml_attribute(node, "name")
                .is_some_and(|name| ["blinkRoad1", "blinkRoad2", "blinkRoad3"].contains(&name))
        }));

        let (second_pass, second_changed) =
            normalize_imported_xun_resource("gui/tachometer/xun/tacho.bml", &normalized).unwrap();
        assert!(!second_changed);
        assert_eq!(second_pass, normalized);
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
        "- XUN tachometer resources patched for the P5136-compatible sidecar ABI: {}",
        report.xun_tachometer_compatibility_patches
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
