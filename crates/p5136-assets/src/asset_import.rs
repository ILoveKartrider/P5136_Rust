use std::{
    collections::{BTreeSet, HashMap},
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use p5136_rho5::{Rho5Directory, Rho5Limits};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    asset_index::{AssetIndex, AssetRegion, fold_path},
    bundle::stage_selected_compatible,
    planner::{PlanOptions, run_plan},
};

const REPORT_JSON: &str = "bundle-report.json";
const TABLE_PATH: &str = "etc_/itemTable.kml";
const TARGET_SHOP_PATH: &str = "zeta_/kr/shop/data/item.kml";
const ABILITY_PATHS: &[&str] = &[
    "item/slot/transformByKart.bml",
    "item/slot/fired2Gain.bml",
    "item/slot/firing2Gain.bml",
    "item/slot/animalBooster.bml",
];
const TABLE_ARCHIVE: &str = "DataPack1_00000.rho5";
const CATALOG_ARCHIVE: &str = "DataPack4_00002.rho5";
const RESOURCE_ARCHIVE: &str = "P5136Asset_00000.rho5";
const MAX_ASSET_BYTES: usize = 512 * 1024 * 1024;
const PRISTINE_SUFFIX: &str = ".pristine.bak";
pub const UNSUPPORTED_ITEM_RULE_REASON: &str =
    "requires item-result rules that do not exist in P5136";

// These are the statically audited, previously staged successfully candidates.
// XUN/kart12 native-backport groups are intentionally absent.
const AUDITED_KARTS: &[&str] = &[
    "arowanaV1",
    "artemisV1",
    "blackBeatleV1",
    "blitzV1",
    "candy_xmasV1",
    "carriageV1",
    "carrotcraftV1",
    "chinaV1",
    "cronosV1",
    "deliveryV1",
    "djV1",
    "dragon_goldV1",
    "dragon_redV1",
    "flowerCarriageV1",
    "goldKnightV1",
    "houndV1",
    "hyperV1",
    "lionmaskV1",
    "lionmaskV1_gold",
    "longlifeV1",
    "magmaV1",
    "mantisV1",
    "marathonV1_xmas",
    "mechanicdragon_blueV1",
    "mechanicdragon_redV1",
    "octopusV1",
    "paragonV1",
    "paragonV1_gold",
    "pigV1",
    "roadsterV1",
    "rollerBrushV1",
    "rollerBrushV1_gold",
    "run_cakebok",
    "run_cakechu",
    "run_cakehae",
    "run_cakejung",
    "run_cakepi",
    "run_zombie_chinese",
    "saintV1",
    "shefferV1",
    "Sinsu_GiV1",
    "Sinsu_LinV1",
    "skunaV1",
    "spector_dragonV1",
    "spinteacupV1",
    "sprintV1",
    "stalkerV1",
    "SteamIV1",
    "SteamSV1",
    "stingRayV1",
    "stormbladeV1",
    "stormbladeV1_gold",
    "swordV1",
    "turtleV1",
    "unicorntubeV1",
    "yongyong_redV1",
    "yongyongV1",
];

const AUDITED_CHARACTERS: &[&str] = &[
    "abysschaser",
    "BaiSuzhen",
    "bazzi_20year",
    "bazzi_pisces",
    "bazzi_shark",
    "bazzi_trump",
    "bongyeom",
    "brodi_archer",
    "caocao",
    "charles",
    "chipaowolhee",
    "daji",
    "damarhou",
    "dao_aquarius",
    "dao_trump",
    "deliveryman",
    "dizni_aries",
    "Eluna",
    "ethi_cancer",
    "eunrang",
    "Gstorm",
    "hantao",
    "James",
    "kayla",
    "kephi_capricorn",
    "KunPengforce",
    "kwanwoo_richesgod",
    "LeiZhenzi",
    "Lingling",
    "Lingling_baby",
    "liubei",
    "marid_robot",
    "marid_virgo",
    "mayyangyang",
    "mobi_baby",
    "mos_taurus",
    "mrkart",
    "myeonglee",
    "nezha",
    "nymph_Libra",
    "panda_paper",
    "pengu",
    "pengzi",
    "Reto_Liondance",
    "Reto_Raincoat",
    "rick_baby",
    "run_bazzi_zombie",
    "run_cakebok",
    "run_cakechu",
    "run_cakehae",
    "run_cakejung",
    "run_cakepi",
    "run_damarhou",
    "run_dao_zombie",
    "run_dizini_zombie",
    "run_zombie_chinese",
    "ShenGongbao",
    "sunquan",
    "sword",
    "taigong",
    "tiera_paper",
    "tiera_scorpio",
    "tutu_baby",
    "uni_Gemini",
    "wonwon_baby",
    "wonwon_Leo",
    "wonwon_org",
    "xiaoqing",
    "xiyangyang",
    "yangjian",
    "yongtaek",
    "zhou",
    "zombie_chineseFe",
];

const AUDITED_PETS: &[&str] = &[
    "Alpaca",
    "babyPenguin",
    "bazzi_block",
    "bazzi_zombie",
    "bluedudu",
    "celestialdog",
    "curseragdoll",
    "dao_block",
    "dizini_block",
    "dragonBoat_zongzi",
    "GiLin_Gi",
    "GiLin_Lin",
    "jujak",
    "juju",
    "milu",
    "moonRabbit",
    "redHorse",
    "snowtiger",
    "squirrel",
    "weasel",
    "wonwon_mermaid",
    "xyy1",
    "xyy2",
    "yellowCow",
];

const AUDITED_FLYING_PETS: &[&str] = &[
    "flying20year",
    "flyingAnchor",
    "flyingAquarius",
    "flyingArcher",
    "flyingAries",
    "flyingBabydragon",
    "flyingBambooPanda",
    "flyingBambooPole",
    "flyingBlackBeatle",
    "flyingBlueDragon",
    "flyingBlueSnake",
    "flyingCancer",
    "flyingCapricorn",
    "flyingCornusGreen",
    "flyingCornusRed",
    "flyingDragonjewel",
    "flyingFirecracker",
    "flyingGemini",
    "flyingGoldRing",
    "flyingGourdbottle",
    "flyingheartstone",
    "flyingHoneyBee",
    "flyingkite",
    "flyingKunPeng",
    "flyingLeo",
    "flyingLibra",
    "flyingLonglife",
    "flyingMagpie",
    "flyingMechanic_blue",
    "flyingMechanic_red",
    "flyingMobile",
    "flyingParagon",
    "flyingPisces",
    "flyingQiankunRings",
    "flyingRedDragon",
    "flyingRedlight_Nday",
    "flyingScorpio",
    "flyingshadow",
    "flyingSpringmine",
    "flyingTaurus",
    "flyingTheArtofWar",
    "flyingVirgo",
    "flyingWhale",
    "flyingWhiteSnake",
    "flyingYinyangMirror",
    "flyingZenith",
    "flyingZongziPanda",
];

const UNSUPPORTED_RULE_KARTS: &[&str] = &[
    "SteamIV1",
    "deliveryV1",
    "flowerCarriageV1",
    "lionmaskV1_gold",
    "mechanicdragon_redV1",
    "skunaV1",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetCategory {
    Kart,
    Character,
    Pet,
    FlyingPet,
}

impl AssetCategory {
    pub const ALL: [Self; 4] = [Self::Kart, Self::Character, Self::Pet, Self::FlyingPet];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Kart => "kart",
            Self::Character => "character",
            Self::Pet => "pet",
            Self::FlyingPet => "flying_pet",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetSelection {
    pub category: AssetCategory,
    pub id: String,
}

impl AssetSelection {
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}:{}", self.category.label(), self.id.to_ascii_lowercase())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetCandidate {
    pub category: AssetCategory,
    pub id: String,
    pub already_installed: bool,
    pub eligible: bool,
    pub reason: Option<String>,
}

impl AssetCandidate {
    #[must_use]
    pub fn key(&self) -> String {
        AssetSelection {
            category: self.category,
            id: self.id.clone(),
        }
        .key()
    }
}

#[derive(Debug, Clone)]
pub struct AssetImportOptions {
    pub source_data: PathBuf,
    pub target_data: PathBuf,
    pub target_data_raw: PathBuf,
    pub workspace: PathBuf,
    pub backup: PathBuf,
    pub assets: Vec<AssetSelection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetImportPhase {
    IndexSource,
    Plan,
    Stage,
    InstallDataRaw,
    UpdateCatalogs,
    Complete,
}

#[derive(Debug, Clone, Copy)]
pub struct AssetImportProgress {
    pub phase: AssetImportPhase,
    pub fraction: f32,
    pub current: usize,
    pub total: usize,
}

#[derive(Debug, Clone)]
pub struct AssetImportSummary {
    pub assets: usize,
    pub karts: usize,
    pub characters: usize,
    pub pets: usize,
    pub flying_pets: usize,
    pub resources_written: usize,
    pub resources_identical: usize,
    pub catalogs_updated: usize,
    pub report: PathBuf,
    pub asset_keys: Vec<String>,
}

type ProgressCallback<'a> = &'a mut dyn FnMut(AssetImportProgress);

fn progress(
    callback: ProgressCallback<'_>,
    phase: AssetImportPhase,
    fraction: f32,
    current: usize,
    total: usize,
) {
    callback(AssetImportProgress {
        phase,
        fraction: fraction.clamp(0.0, 1.0),
        current,
        total,
    });
}

#[allow(clippy::cast_precision_loss)]
fn mapped_fraction(start: f32, end: f32, current: usize, total: usize) -> f32 {
    if total == 0 {
        end
    } else {
        start + (end - start) * (current.min(total) as f32 / total as f32)
    }
}

pub fn discover_asset_candidates(
    source_data: &Path,
    target_data_raw: Option<&Path>,
    cache: &Path,
) -> Result<Vec<AssetCandidate>> {
    discover_asset_candidates_with_progress(source_data, target_data_raw, cache, &mut |_| {})
}

pub fn discover_asset_candidates_with_progress(
    source_data: &Path,
    target_data_raw: Option<&Path>,
    cache: &Path,
    on_progress: ProgressCallback<'_>,
) -> Result<Vec<AssetCandidate>> {
    progress(on_progress, AssetImportPhase::IndexSource, 0.0, 0, 0);
    let source = AssetIndex::scan_with_progress(
        source_data,
        AssetRegion::China,
        &cache.join("source-legacy.json"),
        &mut |current, total| {
            progress(
                on_progress,
                AssetImportPhase::IndexSource,
                if total == 0 {
                    0.0
                } else {
                    mapped_fraction(0.0, 0.9, current, total)
                },
                current,
                total,
            );
        },
    )?;
    let prefixes = source_prefixes(&source);
    let mut candidates = Vec::new();
    append_candidates(
        &mut candidates,
        AssetCategory::Kart,
        AUDITED_KARTS,
        &prefixes,
        target_data_raw,
    );
    append_candidates(
        &mut candidates,
        AssetCategory::Pet,
        AUDITED_PETS,
        &prefixes,
        target_data_raw,
    );
    append_candidates(
        &mut candidates,
        AssetCategory::FlyingPet,
        AUDITED_FLYING_PETS,
        &prefixes,
        target_data_raw,
    );
    append_candidates(
        &mut candidates,
        AssetCategory::Character,
        AUDITED_CHARACTERS,
        &prefixes,
        target_data_raw,
    );
    candidates.sort_unstable_by(|left, right| {
        (left.category.label(), left.id.to_ascii_lowercase())
            .cmp(&(right.category.label(), right.id.to_ascii_lowercase()))
    });
    progress(
        on_progress,
        AssetImportPhase::Complete,
        1.0,
        candidates.len(),
        candidates.len(),
    );
    Ok(candidates)
}

fn source_prefixes(source: &AssetIndex) -> HashMap<String, String> {
    let mut output = HashMap::new();
    for record in source.effective_records() {
        let mut parts = record.virtual_path.split('/');
        let Some(root) = parts.next() else { continue };
        let Some(id) = parts.next() else { continue };
        let category = match root.to_ascii_lowercase().as_str() {
            "kart" | "kart_" => AssetCategory::Kart,
            "character" | "character_" | "rider" | "rider_" => AssetCategory::Character,
            "pet" | "pet_" => AssetCategory::Pet,
            "flyingpet" | "flyingpet_" => AssetCategory::FlyingPet,
            _ => continue,
        };
        output
            .entry(format!("{}:{}", category.label(), id.to_ascii_lowercase()))
            .or_insert_with(|| format!("{root}/{id}"));
    }
    output
}

fn append_candidates(
    output: &mut Vec<AssetCandidate>,
    category: AssetCategory,
    audited: &[&str],
    prefixes: &HashMap<String, String>,
    target_data_raw: Option<&Path>,
) {
    for &id in audited {
        let key = format!("{}:{}", category.label(), id.to_ascii_lowercase());
        let Some(prefix) = prefixes.get(&key) else {
            continue;
        };
        let unsupported_rule = category == AssetCategory::Kart
            && UNSUPPORTED_RULE_KARTS
                .iter()
                .any(|blocked| blocked.eq_ignore_ascii_case(id));
        let already_installed = target_data_raw.is_some_and(|data_raw| {
            prefix
                .split('/')
                .fold(data_raw.to_path_buf(), |path, component| {
                    path.join(component)
                })
                .is_dir()
        });
        output.push(AssetCandidate {
            category,
            id: id.to_owned(),
            already_installed,
            eligible: !unsupported_rule,
            reason: unsupported_rule.then(|| UNSUPPORTED_ITEM_RULE_REASON.to_owned()),
        });
    }
}

pub fn import_assets_to_dataraw(options: &AssetImportOptions) -> Result<AssetImportSummary> {
    import_assets_to_dataraw_with_progress(options, &mut |_| {})
}

#[allow(clippy::too_many_lines)]
pub fn import_assets_to_dataraw_with_progress(
    options: &AssetImportOptions,
    on_progress: ProgressCallback<'_>,
) -> Result<AssetImportSummary> {
    validate_options(options)?;
    let selected = options
        .assets
        .iter()
        .map(AssetSelection::key)
        .collect::<BTreeSet<_>>();
    ensure!(
        selected.len() == options.assets.len(),
        "asset selection contains duplicates"
    );
    for asset in &options.assets {
        ensure!(
            audited(asset),
            "asset is outside the audited compatibility set: {}",
            asset.key()
        );
        ensure!(
            !(asset.category == AssetCategory::Kart
                && UNSUPPORTED_RULE_KARTS
                    .iter()
                    .any(|blocked| blocked.eq_ignore_ascii_case(&asset.id))),
            "{}: {UNSUPPORTED_ITEM_RULE_REASON}",
            asset.id
        );
    }

    fs::create_dir_all(&options.workspace)?;
    fs::create_dir_all(&options.backup)?;
    let plan_dir = options.workspace.join("plan");
    let bundle_dir = options.workspace.join("bundle");
    progress(
        on_progress,
        AssetImportPhase::Plan,
        0.06,
        0,
        options.assets.len(),
    );
    let report = run_plan(
        &options.source_data,
        &options.target_data,
        &plan_dir,
        &PlanOptions {
            category: "all".to_owned(),
            asset: None,
            asset_selectors: selected.clone(),
            include_existing: true,
            max_assets: options.assets.len(),
            max_asset_bytes: MAX_ASSET_BYTES,
        },
    )?;
    progress(
        on_progress,
        AssetImportPhase::Stage,
        0.32,
        0,
        options.assets.len(),
    );
    stage_selected_compatible(
        &options.source_data,
        &options.target_data,
        &report,
        &bundle_dir,
        &[
            "kart".to_owned(),
            "character".to_owned(),
            "pet".to_owned(),
            "flying_pet".to_owned(),
        ],
        RESOURCE_ARCHIVE,
        TABLE_ARCHIVE,
        CATALOG_ARCHIVE,
        true,
        &selected,
    )?;
    progress(on_progress, AssetImportPhase::InstallDataRaw, 0.72, 0, 0);
    let install = install_bundle(
        &bundle_dir,
        &options.target_data,
        &options.target_data_raw,
        &options.backup,
        on_progress,
    )?;
    let karts = options
        .assets
        .iter()
        .filter(|asset| asset.category == AssetCategory::Kart)
        .count();
    let characters = options
        .assets
        .iter()
        .filter(|asset| asset.category == AssetCategory::Character)
        .count();
    let pets = options
        .assets
        .iter()
        .filter(|asset| asset.category == AssetCategory::Pet)
        .count();
    let flying_pets = options
        .assets
        .iter()
        .filter(|asset| asset.category == AssetCategory::FlyingPet)
        .count();
    let summary = AssetImportSummary {
        assets: options.assets.len(),
        karts,
        characters,
        pets,
        flying_pets,
        resources_written: install.written,
        resources_identical: install.identical,
        catalogs_updated: install.catalogs,
        report: bundle_dir.join("bundle-report.md"),
        asset_keys: selected.into_iter().collect(),
    };
    progress(
        on_progress,
        AssetImportPhase::Complete,
        1.0,
        summary.assets,
        summary.assets,
    );
    Ok(summary)
}

fn audited(asset: &AssetSelection) -> bool {
    let values = match asset.category {
        AssetCategory::Kart => AUDITED_KARTS,
        AssetCategory::Character => AUDITED_CHARACTERS,
        AssetCategory::Pet => AUDITED_PETS,
        AssetCategory::FlyingPet => AUDITED_FLYING_PETS,
    };
    values
        .iter()
        .any(|value| value.eq_ignore_ascii_case(&asset.id))
}

fn validate_options(options: &AssetImportOptions) -> Result<()> {
    ensure!(!options.assets.is_empty(), "select at least one asset");
    ensure!(
        options.source_data.join("aaa.pk").is_file(),
        "source Data has no aaa.pk"
    );
    ensure!(
        options.target_data.join("aaa.pk").is_file(),
        "target Data has no aaa.pk"
    );
    ensure!(
        options.target_data_raw.join(TABLE_PATH).is_file()
            && options.target_data_raw.join(TARGET_SHOP_PATH).is_file(),
        "target must contain a complete P5136 DataRaw tree"
    );
    Ok(())
}

#[derive(Debug, Deserialize)]
struct BundleReport {
    schema_version: u32,
    archives: Vec<BundleArchive>,
}

#[derive(Debug, Deserialize)]
struct BundleArchive {
    name: String,
    role: String,
    bytes: usize,
    sha256: String,
}

struct InstallCounts {
    written: usize,
    identical: usize,
    catalogs: usize,
}

#[allow(clippy::too_many_lines)]
fn install_bundle(
    bundle: &Path,
    target_data: &Path,
    data_raw: &Path,
    backup: &Path,
    on_progress: ProgressCallback<'_>,
) -> Result<InstallCounts> {
    let report_path = bundle.join(REPORT_JSON);
    let report: BundleReport = serde_json::from_slice(&fs::read(&report_path)?)?;
    ensure!(
        report.schema_version == 1,
        "unsupported asset bundle schema"
    );
    let roles = report
        .archives
        .iter()
        .map(|archive| (archive.name.to_ascii_lowercase(), archive.role.as_str()))
        .collect::<HashMap<_, _>>();
    for archive in &report.archives {
        let bytes = fs::read(bundle.join(&archive.name))?;
        ensure!(
            bytes.len() == archive.bytes,
            "archive size mismatch: {}",
            archive.name
        );
        ensure!(
            format!("{:x}", Sha256::digest(&bytes)).eq_ignore_ascii_case(&archive.sha256),
            "archive hash mismatch: {}",
            archive.name
        );
    }
    let directory = Rho5Directory::scan_kr(bundle, Rho5Limits::default())?;
    ensure!(
        directory.archive_count() == report.archives.len(),
        "bundle contains unreported archives"
    );
    let mut counts = InstallCounts {
        written: 0,
        identical: 0,
        catalogs: 0,
    };
    let total = directory.entries().len();
    for (index, entry) in directory.entries().iter().enumerate() {
        progress(
            on_progress,
            AssetImportPhase::InstallDataRaw,
            mapped_fraction(0.72, 0.90, index, total),
            index,
            total,
        );
        let role = roles
            .get(&entry.archive_name().to_ascii_lowercase())
            .with_context(|| format!("missing role for {}", entry.archive_name()))?;
        let path = entry.normalized_path();
        let catalog = is_catalog_path(path);
        let selected_entry = match *role {
            "resource" => !catalog,
            "item_table_base" => fold_path(path) == fold_path(TABLE_PATH),
            "catalog_overlay" => catalog && fold_path(path) != fold_path(TABLE_PATH),
            _ => false,
        };
        if !selected_entry {
            continue;
        }
        let relative = safe_relative_path(path)?;
        let destination = data_raw.join(&relative);
        let bytes = directory.extract_entry_with_legacy_padding(entry)?;
        if destination.is_file() {
            let existing = fs::read(&destination)?;
            if existing == bytes {
                counts.identical += 1;
                continue;
            }
            ensure!(
                catalog,
                "resource path conflicts with existing DataRaw file: {path}"
            );
            backup_once(&destination, &backup.join(&relative))?;
            counts.catalogs += 1;
        } else {
            ensure!(
                !destination.exists(),
                "destination is not a file: {}",
                destination.display()
            );
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&destination, &bytes)?;
        ensure!(
            fs::read(&destination)? == bytes,
            "failed to verify {}",
            destination.display()
        );
        counts.written += 1;
    }

    progress(on_progress, AssetImportPhase::UpdateCatalogs, 0.92, 0, 2);
    for (index, archive_name) in [TABLE_ARCHIVE, CATALOG_ARCHIVE].into_iter().enumerate() {
        let source = bundle.join(archive_name);
        let destination = target_data.join(archive_name);
        ensure!(
            source.is_file() && destination.is_file(),
            "catalog archive is missing: {archive_name}"
        );
        backup_sibling_once(&destination)?;
        let staged = fs::read(&source)?;
        if fs::read(&destination)? != staged {
            fs::write(&destination, &staged)?;
            ensure!(
                fs::read(&destination)? == staged,
                "failed to verify {archive_name}"
            );
            counts.catalogs += 1;
        }
        progress(
            on_progress,
            AssetImportPhase::UpdateCatalogs,
            mapped_fraction(0.92, 0.98, index + 1, 2),
            index + 1,
            2,
        );
    }
    Ok(counts)
}

fn is_catalog_path(path: &str) -> bool {
    let folded = fold_path(path);
    folded == fold_path(TABLE_PATH)
        || folded == fold_path(TARGET_SHOP_PATH)
        || ABILITY_PATHS
            .iter()
            .any(|candidate| folded == fold_path(candidate))
}

fn safe_relative_path(path: &str) -> Result<PathBuf> {
    ensure!(!path.contains(':'), "asset path contains a drive prefix");
    let normalized = path.replace('\\', "/");
    let relative = Path::new(&normalized);
    ensure!(!relative.is_absolute(), "asset path is absolute");
    let mut output = PathBuf::new();
    for component in relative.components() {
        match component {
            Component::Normal(value) => output.push(value),
            Component::CurDir => {}
            _ => ensure!(false, "asset path escapes DataRaw"),
        }
    }
    ensure!(!output.as_os_str().is_empty(), "asset path is empty");
    Ok(output)
}

fn backup_once(source: &Path, backup: &Path) -> Result<()> {
    if backup.exists() {
        ensure!(
            backup.is_file(),
            "backup path is not a file: {}",
            backup.display()
        );
        return Ok(());
    }
    if let Some(parent) = backup.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(source, backup)?;
    Ok(())
}

fn backup_sibling_once(source: &Path) -> Result<()> {
    let file_name = source
        .file_name()
        .context("catalog archive has no file name")?;
    let backup = source.with_file_name(format!(
        "{}{}",
        file_name.to_string_lossy(),
        PRISTINE_SUFFIX
    ));
    backup_once(source, &backup)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audited_sets_have_expected_sizes_and_no_duplicate_keys() {
        assert_eq!(AUDITED_KARTS.len(), 57);
        assert_eq!(AUDITED_CHARACTERS.len(), 73);
        assert_eq!(AUDITED_PETS.len(), 24);
        assert_eq!(AUDITED_FLYING_PETS.len(), 47);
        let keys = AUDITED_KARTS
            .iter()
            .map(|id| format!("kart:{}", id.to_ascii_lowercase()))
            .chain(
                AUDITED_CHARACTERS
                    .iter()
                    .map(|id| format!("character:{}", id.to_ascii_lowercase())),
            )
            .chain(
                AUDITED_PETS
                    .iter()
                    .map(|id| format!("pet:{}", id.to_ascii_lowercase())),
            )
            .chain(
                AUDITED_FLYING_PETS
                    .iter()
                    .map(|id| format!("flying_pet:{}", id.to_ascii_lowercase())),
            )
            .collect::<BTreeSet<_>>();
        assert_eq!(keys.len(), 201);
    }

    #[test]
    fn unsupported_item_rule_karts_are_all_audited() {
        assert_eq!(UNSUPPORTED_RULE_KARTS.len(), 6);
        assert!(UNSUPPORTED_RULE_KARTS.iter().all(|blocked| {
            AUDITED_KARTS
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(blocked))
        }));
    }

    #[test]
    fn selection_keys_keep_category_for_shared_asset_ids() {
        let kart = AssetSelection {
            category: AssetCategory::Kart,
            id: "run_cakebok".to_owned(),
        };
        let character = AssetSelection {
            category: AssetCategory::Character,
            id: "run_cakebok".to_owned(),
        };
        assert_ne!(kart.key(), character.key());
    }
}
