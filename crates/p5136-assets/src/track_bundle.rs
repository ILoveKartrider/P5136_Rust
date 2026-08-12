use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fmt::Write as _,
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use p5136_core::{
    bml::{BmlLimits, BmlNode},
    packet::{PacketReader, PacketWriter},
};
use p5136_rho5::{
    P5136_PACKED_ENTRY_FLAGS, Rho5Directory, Rho5Limits, Rho5Region, Rho5WriteEntry, Rho5Writer,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    asset_index::{AssetIndex, AssetRecord, AssetRegion, fold_path},
    ensure_staging_destination,
};

const SOURCE_TRACK_TABLE: &str = "track/common/track@zz.bml";
const SOURCE_TRACK_LOCALE: &str = "track/common/trackLocale@cn.bml";
const TARGET_TRACK_LOCALE: &str = "track/common/trackLocale@kr.bml";
const SELECT_TRACK_CONFIGS: [&str; 2] = [
    "dialog2/selectTrackEx/config@cn.bml",
    "dialog2/selectTrackEx/config@tw.bml",
];
const SELECT_TRACK_STRING_BAG: &str = "dialog2/selectTrackEx/selectTrackEx_stringBag.bml";
const CONTENT_CONFIG: &str = "zeta_/kr/content/config.xml";
const P5136_NATIVE_THEMES: [&str; 34] = [
    "steam",
    "forest",
    "desert",
    "village",
    "ice",
    "tomb",
    "mine",
    "northeu",
    "factory",
    "pirate",
    "fairy",
    "moonhill",
    "gold",
    "china",
    "castle",
    "nymph",
    "mechanic",
    "xyy",
    "wkc",
    "brodi",
    "park",
    "beach",
    "transFormer",
    "jurassic",
    "world",
    "nemo",
    "sword",
    "god",
    "abyss",
    "camelot",
    "olympos",
    "korea",
    "mabi",
    "maple",
];
const MAX_CHUNK_PLAINTEXT_BYTES: usize = 40 * 1024 * 1024;
const MAX_OUTPUT_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;
const REPORT_JSON: &str = "track-bundle-report.json";
const REPORT_MARKDOWN: &str = "track-bundle-report.md";

const BML_LIMITS: BmlLimits = BmlLimits {
    max_depth: 32,
    max_nodes: 200_000,
    max_attributes_per_node: 512,
    max_children_per_node: 100_000,
    max_string_code_units: 8_192,
};

#[derive(Debug, Clone)]
struct PendingFile {
    target_path: String,
    source: AssetRecord,
    role: &'static str,
}

#[derive(Debug, Serialize, Deserialize)]
struct TrackBundleReport {
    schema_version: u32,
    source_data: String,
    target_data: String,
    selection_rule: String,
    tracks: Vec<TrackReport>,
    themes: Vec<ThemeReport>,
    archives: Vec<ArchiveReport>,
    resource_entries: usize,
    resource_plaintext_bytes: usize,
    preserved_catalog_entries: usize,
    role_counts: BTreeMap<String, usize>,
    #[serde(default)]
    dependencies: Vec<DependencyReport>,
    warnings: Vec<String>,
    #[serde(default)]
    content_registry_catalogs: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct TrackReport {
    id: String,
    game_type: String,
    theme: String,
    display_name: String,
    #[serde(default)]
    catalog_already_installed: bool,
    resource_entries: usize,
    ai_path_entries: usize,
    thumbnail_aliases: usize,
    #[serde(default)]
    material_dependencies: usize,
    #[serde(default)]
    sound_dependencies: usize,
    #[serde(default)]
    embedded_dependencies: usize,
    #[serde(default)]
    unresolved_dependencies: usize,
    #[serde(default)]
    conflicting_dependencies: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DependencyReport {
    track_id: String,
    kind: String,
    symbol: String,
    path: Option<String>,
    status: String,
    detail: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ThemeReport {
    id: String,
    new_to_p5136: bool,
    staged_source_only_entries: usize,
    preserved_existing_entries: usize,
    #[serde(default)]
    selector_catalogs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ArchiveReport {
    name: String,
    role: String,
    bytes: usize,
    sha256: String,
    entries: usize,
}

#[derive(Debug, Deserialize)]
struct PreviousReport {
    archives: Vec<ArchiveReport>,
}

struct SelectedTrack {
    id: String,
    game_type: String,
    theme: String,
    already_installed: bool,
    locale_already_installed: bool,
    definition: BmlNode,
    locale: BmlNode,
}

/// Encryption/locale family used by the external client selected as an import source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackSourceRegion {
    Korea,
    China,
}

impl TrackSourceRegion {
    fn asset_region(self) -> AssetRegion {
        match self {
            Self::Korea => AssetRegion::Korea,
            Self::China => AssetRegion::China,
        }
    }

    fn locale_path(self) -> &'static str {
        match self {
            Self::Korea => TARGET_TRACK_LOCALE,
            Self::China => SOURCE_TRACK_LOCALE,
        }
    }
}

/// One selectable I/R track discovered in an external client's mounted namespace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackCandidate {
    pub id: String,
    pub name: String,
    pub game_type: String,
    pub theme: String,
    pub already_installed: bool,
    pub eligible: bool,
    pub reason: Option<String>,
}

/// Complete, non-destructive `DataRaw` import request used by the integrated GUI.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TrackImportOptions {
    pub source_data: PathBuf,
    pub source_region: TrackSourceRegion,
    pub target_data: PathBuf,
    pub target_data_raw: PathBuf,
    pub workspace: PathBuf,
    pub backup: PathBuf,
    pub tracks: Vec<String>,
}

/// Result returned after a staged bundle has been verified and installed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct TrackImportSummary {
    pub tracks: usize,
    pub track_ids: Vec<String>,
    pub resources: usize,
    pub dependencies: usize,
    pub warnings: Vec<String>,
    pub report: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackImportPhase {
    IndexSource,
    IndexTarget,
    ReadCatalog,
    SelectTracks,
    CollectResources,
    AnalyzeDependencies,
    WriteBundle,
    VerifyBundle,
    InstallDataRaw,
    Complete,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrackImportProgress {
    pub phase: TrackImportPhase,
    pub fraction: f32,
    pub current: usize,
    pub total: usize,
}

type ProgressCallback<'a> = &'a mut dyn FnMut(TrackImportProgress);

fn report_progress(
    progress: ProgressCallback<'_>,
    phase: TrackImportPhase,
    fraction: f32,
    current: usize,
    total: usize,
) {
    progress(TrackImportProgress {
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

struct MaterializedArchive {
    entries: Vec<Rho5WriteEntry>,
    preserved_entries: usize,
}

struct SelectorCatalogMerge {
    replacements: Vec<(String, Vec<u8>)>,
    registrations: BTreeMap<String, Vec<String>>,
    content_registry_catalogs: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
struct ThemeSlotAlias {
    native_code: &'static str,
    native_id: u32,
    string_key: &'static str,
    icon: &'static str,
    content_key: &'static str,
}

fn theme_slot_alias(theme: &str) -> Option<ThemeSlotAlias> {
    theme
        .eq_ignore_ascii_case("fengshen")
        .then_some(ThemeSlotAlias {
            // P5136 has no native enum 35. XYY's three definitions survive, but
            // two physical track archives and its KR locale/BGM were removed, so
            // enum 17 is the only dormant selector slot available without an EXE
            // hook. The imported track IDs and physical fengshen paths stay intact.
            native_code: "xyy",
            native_id: 17,
            string_key: "themeFengshen",
            icon: "fengshen",
            content_key: "themeXyy",
        })
}

fn is_native_theme(theme: &str) -> bool {
    P5136_NATIVE_THEMES
        .iter()
        .any(|native| native.eq_ignore_ascii_case(theme))
}

fn selector_registration_id(theme: &str) -> String {
    theme_slot_alias(theme).map_or_else(|| theme.to_owned(), |alias| alias.native_id.to_string())
}

/// Enumerates selectable source tracks without modifying either client.
pub fn discover_track_candidates(
    source_data: &Path,
    source_region: TrackSourceRegion,
    target_data_raw: Option<&Path>,
    cache: &Path,
) -> Result<Vec<TrackCandidate>> {
    discover_track_candidates_with_progress(
        source_data,
        source_region,
        target_data_raw,
        cache,
        &mut |_| {},
    )
}

#[allow(clippy::too_many_lines)]
pub fn discover_track_candidates_with_progress(
    source_data: &Path,
    source_region: TrackSourceRegion,
    target_data_raw: Option<&Path>,
    cache: &Path,
    progress: ProgressCallback<'_>,
) -> Result<Vec<TrackCandidate>> {
    fs::create_dir_all(cache).with_context(|| format!("failed to create {}", cache.display()))?;
    report_progress(progress, TrackImportPhase::IndexSource, 0.0, 0, 0);
    let source = AssetIndex::scan_with_progress(
        source_data,
        source_region.asset_region(),
        &cache.join("source-legacy.json"),
        &mut |current, total| {
            report_progress(
                progress,
                TrackImportPhase::IndexSource,
                mapped_fraction(0.0, 0.82, current, total),
                current,
                total,
            );
        },
    )?;
    report_progress(progress, TrackImportPhase::ReadCatalog, 0.86, 0, 0);
    let locale_path = source_region.locale_path();
    let table = decode_effective(&source, SOURCE_TRACK_TABLE)?;
    let locale = decode_effective(&source, locale_path)?;
    validate_track_root(SOURCE_TRACK_TABLE, &table)?;
    validate_track_root(locale_path, &locale)?;
    let locales = nodes_by_id(&locale)?;
    let installed = target_data_raw
        .filter(|root| root.join(SOURCE_TRACK_TABLE).is_file())
        .map(|root| -> Result<HashSet<String>> {
            let target = decode_bml(
                SOURCE_TRACK_TABLE,
                &fs::read(root.join(SOURCE_TRACK_TABLE))?,
            )?;
            Ok(target
                .children
                .iter()
                .filter_map(|node| attribute(node, "id"))
                .map(fold_path)
                .collect())
        })
        .transpose()?
        .unwrap_or_default();

    let definition_count = table.children.len();
    let mut candidates = table
        .children
        .iter()
        .enumerate()
        .filter_map(|definition| {
            let (index, definition) = definition;
            report_progress(
                progress,
                TrackImportPhase::SelectTracks,
                mapped_fraction(0.9, 0.99, index, definition_count),
                index,
                definition_count,
            );
            let id = attribute(definition, "id")?;
            if !is_ordinary_track_id(id) {
                return None;
            }
            let locale = locales.get(&fold_path(id));
            let game_type = attribute(definition, "gameType")
                .unwrap_or_default()
                .to_ascii_lowercase();
            let theme = track_theme(id).unwrap_or_default().to_owned();
            let already_installed = installed.contains(&fold_path(id));
            let reason = if !matches!(game_type.as_str(), "item" | "speed") {
                Some(format!("unsupported gameType {game_type:?}"))
            } else if locale.is_none() {
                Some("missing source locale row".to_owned())
            } else if locale.is_some_and(|row| {
                is_true(attribute(row, "blocked"))
                    || attribute(row, "choosable").is_some_and(|value| !is_true(Some(value)))
            }) {
                Some("source locale marks the track blocked or unchoosable".to_owned())
            } else if !has_track_resources(&source, id) {
                Some("source track folder has no effective files".to_owned())
            } else {
                None
            };
            let name = locale
                .and_then(|row| attribute(row, "name"))
                .filter(|name| !name.trim().is_empty())
                .unwrap_or(id)
                .to_owned();
            Some(TrackCandidate {
                id: id.to_owned(),
                name,
                game_type,
                theme,
                already_installed,
                eligible: reason.is_none(),
                reason,
            })
        })
        .collect::<Vec<_>>();
    candidates.sort_unstable_by(|left, right| {
        fold_path(&left.theme)
            .cmp(&fold_path(&right.theme))
            .then_with(|| fold_path(&left.id).cmp(&fold_path(&right.id)))
    });
    report_progress(
        progress,
        TrackImportPhase::Complete,
        1.0,
        candidates.len(),
        candidates.len(),
    );
    Ok(candidates)
}

/// Stages a selected dependency closure, verifies it, and installs it into a
/// complete P5136 `DataRaw` tree. Existing non-catalog files are never replaced.
#[allow(dead_code)]
pub fn import_tracks_to_dataraw(options: &TrackImportOptions) -> Result<TrackImportSummary> {
    import_tracks_to_dataraw_with_progress(options, &mut |_| {})
}

pub fn import_tracks_to_dataraw_with_progress(
    options: &TrackImportOptions,
    progress: ProgressCallback<'_>,
) -> Result<TrackImportSummary> {
    ensure!(!options.tracks.is_empty(), "select at least one track");
    ensure!(
        options.target_data.is_dir(),
        "target Data directory is missing"
    );
    ensure!(
        options.target_data_raw.is_dir(),
        "target DataRaw directory is missing"
    );
    fs::create_dir_all(&options.workspace)?;
    let bundle = options.workspace.join("bundle");
    // These archives are verified transport containers and are never copied
    // into live Data. DataRaw import therefore does not consume or depend on
    // the client's small set of stock-empty pack slots.
    let archive_names = (0..64)
        .map(|index| format!("P5136TrackImport_{index:05}.rho5"))
        .collect::<Vec<_>>();
    stage_tracks_from_source(
        &options.source_data,
        options.source_region,
        &options.target_data,
        Some(&options.target_data_raw),
        &bundle,
        &options.tracks,
        &archive_names,
        "DataPack1_00013.rho5",
        true,
        progress,
    )?;
    install_tracks_dataraw_with_progress(
        &bundle,
        &options.target_data_raw,
        &options.backup,
        progress,
    )?;
    let report_path = bundle.join(REPORT_JSON);
    let report: TrackBundleReport = serde_json::from_slice(&fs::read(&report_path)?)?;
    let summary = TrackImportSummary {
        tracks: report.tracks.len(),
        track_ids: report.tracks.iter().map(|track| track.id.clone()).collect(),
        resources: report.resource_entries,
        dependencies: report.dependencies.len(),
        warnings: report.warnings,
        report: bundle.join(REPORT_MARKDOWN),
    };
    report_progress(
        progress,
        TrackImportPhase::Complete,
        1.0,
        summary.tracks,
        summary.tracks,
    );
    Ok(summary)
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(crate) fn stage_tracks(
    source_data: &Path,
    source_region: TrackSourceRegion,
    target_data: &Path,
    output: &Path,
    requested_tracks: &[String],
    archive_names: &[String],
    catalog_archive_name: &str,
    force: bool,
) -> Result<()> {
    stage_tracks_from_source(
        source_data,
        source_region,
        target_data,
        None,
        output,
        requested_tracks,
        archive_names,
        catalog_archive_name,
        force,
        &mut |_| {},
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn stage_tracks_from_source(
    source_data: &Path,
    source_region: TrackSourceRegion,
    target_data: &Path,
    target_data_raw: Option<&Path>,
    output: &Path,
    requested_tracks: &[String],
    archive_names: &[String],
    catalog_archive_name: &str,
    force: bool,
    progress: ProgressCallback<'_>,
) -> Result<()> {
    ensure_staging_destination(source_data, target_data, output)?;
    validate_archive_name(catalog_archive_name)?;
    ensure!(
        !archive_names.is_empty(),
        "at least one resource archive slot is required"
    );
    let mut unique_archives = HashSet::new();
    for archive_name in archive_names {
        validate_archive_name(archive_name)?;
        ensure!(
            !archive_name.eq_ignore_ascii_case(catalog_archive_name),
            "resource and catalog archive names must differ"
        );
        ensure!(
            unique_archives.insert(archive_name.to_ascii_lowercase()),
            "resource archive slots contain a duplicate name"
        );
    }
    for id in requested_tracks {
        ensure!(
            is_ordinary_track_id(id),
            "requested track {id:?} is not an Ixx/Rxx track (only an optional _kd suffix is accepted)"
        );
    }

    fs::create_dir_all(output).with_context(|| format!("failed to create {}", output.display()))?;
    let json_path = output.join(REPORT_JSON);
    let markdown_path = output.join(REPORT_MARKDOWN);
    if force {
        remove_previous_archives(output, &json_path)?;
        remove_if_file(&json_path)?;
        remove_if_file(&markdown_path)?;
    } else {
        ensure!(
            !json_path.exists() && !markdown_path.exists(),
            "track staging report already exists; pass --force to replace only its recorded output"
        );
    }

    let cache = output.join(".index-cache");
    fs::create_dir_all(&cache)?;
    eprintln!("indexing external source Data for I/R tracks...");
    report_progress(progress, TrackImportPhase::IndexSource, 0.0, 0, 0);
    let source = AssetIndex::scan_with_progress(
        source_data,
        source_region.asset_region(),
        &cache.join("source-legacy.json"),
        &mut |current, total| {
            report_progress(
                progress,
                TrackImportPhase::IndexSource,
                mapped_fraction(0.0, 0.23, current, total),
                current,
                total,
            );
        },
    )?;
    eprintln!("indexing Korean P5136 target Data...");
    report_progress(progress, TrackImportPhase::IndexTarget, 0.23, 0, 0);
    let target = AssetIndex::scan_with_progress(
        target_data,
        AssetRegion::Korea,
        &cache.join("target-legacy.json"),
        &mut |current, total| {
            report_progress(
                progress,
                TrackImportPhase::IndexTarget,
                mapped_fraction(0.23, 0.4, current, total),
                current,
                total,
            );
        },
    )?;

    report_progress(progress, TrackImportPhase::ReadCatalog, 0.41, 0, 0);
    let source_table = decode_effective(&source, SOURCE_TRACK_TABLE)?;
    let source_locale_path = source_region.locale_path();
    let source_locale = decode_effective(&source, source_locale_path)?;
    let mut target_table = decode_target_bml(&target, target_data_raw, SOURCE_TRACK_TABLE)?;
    let mut target_locale = decode_target_bml(&target, target_data_raw, TARGET_TRACK_LOCALE)?;
    validate_track_root(SOURCE_TRACK_TABLE, &source_table)?;
    validate_track_root(source_locale_path, &source_locale)?;
    validate_track_root(SOURCE_TRACK_TABLE, &target_table)?;
    validate_track_root(TARGET_TRACK_LOCALE, &target_locale)?;

    let source_locales = nodes_by_id(&source_locale)?;
    let target_ids = target_table
        .children
        .iter()
        .filter_map(|node| attribute(node, "id"))
        .map(fold_path)
        .collect::<HashSet<_>>();
    let target_locale_ids = target_locale
        .children
        .iter()
        .filter_map(|node| attribute(node, "id"))
        .map(fold_path)
        .collect::<HashSet<_>>();
    let requested = requested_tracks
        .iter()
        .map(|id| fold_path(id))
        .collect::<BTreeSet<_>>();
    let mut selected = Vec::new();
    let mut rejection = HashMap::<String, String>::new();
    let source_definition_count = source_table.children.len();
    for (definition_index, definition) in source_table.children.iter().enumerate() {
        report_progress(
            progress,
            TrackImportPhase::SelectTracks,
            mapped_fraction(0.43, 0.47, definition_index, source_definition_count),
            definition_index,
            source_definition_count,
        );
        let Some(id) = attribute(definition, "id") else {
            continue;
        };
        let folded_id = fold_path(id);
        if !is_ordinary_track_id(id) {
            continue;
        }
        if !requested.is_empty() && !requested.contains(&folded_id) {
            continue;
        }
        let game_type = attribute(definition, "gameType").unwrap_or_default();
        if !matches!(game_type.to_ascii_lowercase().as_str(), "item" | "speed") {
            rejection.insert(folded_id, format!("unsupported gameType {game_type:?}"));
            continue;
        }
        let already_installed = target_ids.contains(&folded_id);
        if already_installed && requested.is_empty() {
            continue;
        }
        let Some(locale) = source_locales.get(&folded_id) else {
            rejection.insert(folded_id, "missing source locale row".to_owned());
            continue;
        };
        if is_true(attribute(locale, "blocked"))
            || attribute(locale, "choosable").is_some_and(|value| !is_true(Some(value)))
        {
            rejection.insert(
                folded_id,
                "source locale marks the row blocked/unchoosable".to_owned(),
            );
            continue;
        }
        if !has_track_resources(&source, id) {
            rejection.insert(
                folded_id,
                "source track folder has no effective files".to_owned(),
            );
            continue;
        }
        let theme = track_theme(id)
            .with_context(|| format!("cannot derive theme from track ID {id}"))?
            .to_owned();
        ensure!(
            is_native_theme(&theme) || theme_slot_alias(&theme).is_some(),
            "theme {theme:?} is not in P5136's native 34-theme enum and has no reviewed compatibility slot"
        );
        let definition = normalize_definition_for_p5136(definition, &theme);
        let locale = normalize_locale_for_p5136(locale, id);
        selected.push(SelectedTrack {
            id: id.to_owned(),
            game_type: game_type.to_ascii_lowercase(),
            theme,
            already_installed,
            locale_already_installed: target_locale_ids.contains(&folded_id),
            definition,
            locale,
        });
    }
    selected.sort_unstable_by_key(|track| fold_path(&track.id));
    if !requested.is_empty() {
        let selected_ids = selected
            .iter()
            .map(|track| fold_path(&track.id))
            .collect::<HashSet<_>>();
        for requested_id in requested_tracks {
            if !selected_ids.contains(&fold_path(requested_id)) {
                let reason = rejection
                    .get(&fold_path(requested_id))
                    .map_or("not present in the source track table", String::as_str);
                bail!("requested track {requested_id:?} is not eligible: {reason}");
            }
        }
    }
    ensure!(
        !selected.is_empty(),
        "no source-only active I/R tracks are eligible for staging"
    );

    for track in &selected {
        if track.already_installed {
            let target_definition = target_table
                .children
                .iter_mut()
                .find(|node| {
                    attribute(node, "id").is_some_and(|id| id.eq_ignore_ascii_case(&track.id))
                })
                .expect("target ID set came from the target table");
            if theme_slot_alias(&track.theme).is_some() {
                *target_definition = track.definition.clone();
            } else {
                apply_theme_compatibility(target_definition, &track.theme);
            }
        } else {
            target_table.children.push(track.definition.clone());
        }
        if !track.locale_already_installed {
            target_locale.children.push(track.locale.clone());
        }
    }
    let table_bytes = encode_bml(SOURCE_TRACK_TABLE, &target_table)?;
    let locale_bytes = encode_bml(TARGET_TRACK_LOCALE, &target_locale)?;

    let selected_ids = selected
        .iter()
        .map(|track| fold_path(&track.id))
        .collect::<HashSet<_>>();
    let themes = selected
        .iter()
        .map(|track| fold_path(&track.theme))
        .collect::<BTreeSet<_>>();
    let theme_ids = selected
        .iter()
        .map(|track| (fold_path(&track.theme), track.theme.clone()))
        .collect::<BTreeMap<_, _>>();
    let selector_catalogs =
        merge_selector_theme_catalogs(&source, &target, target_data_raw, &theme_ids)?;
    let mut pending = BTreeMap::<String, PendingFile>::new();
    let mut per_track = selected
        .iter()
        .map(|track| {
            (
                fold_path(&track.id),
                (
                    0_usize, 0_usize, 0_usize, 0_usize, 0_usize, 0_usize, 0_usize, 0_usize,
                ),
            )
        })
        .collect::<HashMap<_, _>>();
    let mut per_theme = themes
        .iter()
        .map(|theme| (theme.clone(), (0_usize, 0_usize)))
        .collect::<HashMap<_, _>>();

    let source_entry_count = source.entry_count();
    for (record_index, record) in source.effective_records().enumerate() {
        report_progress(
            progress,
            TrackImportPhase::CollectResources,
            mapped_fraction(0.48, 0.6, record_index, source_entry_count),
            record_index,
            source_entry_count,
        );
        let path = record.virtual_path.replace('\\', "/");
        if let Some(track_id) = track_folder_id(&path)
            && selected_ids.contains(&fold_path(track_id))
        {
            if add_pending(
                &target,
                target_data_raw,
                &mut pending,
                record,
                &path,
                "track",
            ) {
                per_track
                    .get_mut(&fold_path(track_id))
                    .expect("selected track")
                    .0 += 1;
            }
            continue;
        }
        if let Some(track_id) = ai_path_track_id(&path, &selected) {
            if add_pending(
                &target,
                target_data_raw,
                &mut pending,
                record,
                &path,
                "ai_path",
            ) {
                per_track
                    .get_mut(&fold_path(track_id))
                    .expect("selected track")
                    .1 += 1;
            }
            continue;
        }
        if let Some(theme) = ancillary_theme(&path, &themes) {
            let added = add_pending(
                &target,
                target_data_raw,
                &mut pending,
                record,
                &path,
                "theme_dependency",
            );
            let counts = per_theme.get_mut(theme).expect("selected theme");
            if added {
                counts.0 += 1;
            } else {
                counts.1 += 1;
            }

            // P5136 resolves bare material names through the native theme
            // selected by the track row. An imported theme can borrow a
            // dormant selector slot, but leaving its files only under the
            // newer theme name makes the old client render untextured
            // geometry. Mirror the complete theme namespace into the native
            // slot while retaining the original paths for newer/path-based
            // references.
            let original_theme = theme_ids
                .get(theme)
                .expect("selected theme ID was collected above");
            if let Some(alias_path) = aliased_theme_resource_path(&path, original_theme) {
                add_pending(
                    &target,
                    target_data_raw,
                    &mut pending,
                    record,
                    &alias_path,
                    "theme_slot_alias",
                );
            }
        }
    }

    // P5136's selector also looks under trackThumb even when the newer client
    // keeps the same image inside the per-track RHO5 folder.
    for track in &selected {
        for file_name in ["xt_trackThumb.png", "xt_trackCard.png"] {
            let source_image = [
                format!("track_/{}/{file_name}", track.id),
                format!("track/{}/{file_name}", track.id),
            ]
            .into_iter()
            .find_map(|path| source.effective(&path));
            if let Some(record) = source_image {
                let alias = format!("trackThumb/{}/{file_name}", track.id);
                if add_pending(
                    &target,
                    target_data_raw,
                    &mut pending,
                    record,
                    &alias,
                    "thumbnail_alias",
                ) {
                    per_track
                        .get_mut(&fold_path(&track.id))
                        .expect("selected track")
                        .2 += 1;
                }
            }
        }
    }

    // Track geometry stores material symbols as AA27 records. Resolve them in
    // runtime priority order: track-local, selected theme, common, other
    // themes, and finally dynamic PPL overrides. This catches cross-theme
    // reuse without assuming every symbol lives under the selected theme.
    let mut dependencies = Vec::new();
    let material_paths = material_path_index(&source);
    let reference_paths = reference_path_index(&source);
    let selected_count = selected.len();
    for (track_index, track) in selected.iter().enumerate() {
        report_progress(
            progress,
            TrackImportPhase::AnalyzeDependencies,
            mapped_fraction(0.6, 0.76, track_index, selected_count),
            track_index,
            selected_count,
        );
        let track_record = [
            format!("track_/{}/track.1s", track.id),
            format!("track/{}/track.1s", track.id),
        ]
        .into_iter()
        .find_map(|path| source.effective(&path))
        .with_context(|| format!("track.1s is missing for {}", track.id))?;
        let track_bytes = source.extract(track_record)?;
        for material in track_material_symbols(&track_bytes) {
            let candidates = material_candidate_paths(&material_paths, track, &material);
            if let Some(path) = candidates.first() {
                per_track
                    .get_mut(&fold_path(&track.id))
                    .expect("selected track")
                    .3 += 1;
                let record = source
                    .effective(path)
                    .expect("material index contains an effective path");
                if path_in_prefix(path, "zeta") {
                    dependencies.push(DependencyReport {
                        track_id: track.id.clone(),
                        kind: "material".to_owned(),
                        symbol: material,
                        path: Some(path.clone()),
                        status: "runtime_dynamic".to_owned(),
                        detail: format!(
                            "resolved only as a dynamic PPL/advertising override; {} candidate path(s) were found and none are imported",
                            candidates.len()
                        ),
                    });
                    continue;
                }
                let (status, mut detail) =
                    dependency_status(&source, &target, target_data_raw, record, path)?;
                if candidates.len() > 1 {
                    write!(
                        detail,
                        "; selected first of {} basename matches using track/theme priority",
                        candidates.len()
                    )
                    .expect("String write");
                }
                if status == "staged" {
                    add_pending(
                        &target,
                        target_data_raw,
                        &mut pending,
                        record,
                        path,
                        "material_dependency",
                    );
                }
                if status == "target_conflict_preserved" {
                    per_track
                        .get_mut(&fold_path(&track.id))
                        .expect("selected track")
                        .6 += 1;
                }
                dependencies.push(DependencyReport {
                    track_id: track.id.clone(),
                    kind: "material".to_owned(),
                    symbol: material,
                    path: Some(path.clone()),
                    status: status.to_owned(),
                    detail,
                });
            } else {
                per_track
                    .get_mut(&fold_path(&track.id))
                    .expect("selected track")
                    .5 += 1;
                dependencies.push(DependencyReport {
                    track_id: track.id.clone(),
                    kind: "material".to_owned(),
                    symbol: material,
                    path: None,
                    status: "source_missing".to_owned(),
                    detail: "no track-local, theme, common, or dynamic DDS/PNG resolved this material symbol"
                        .to_owned(),
                });
            }
        }

        for sound in track_sound_keys(&track_bytes) {
            let candidates = sound_candidate_paths(&sound);
            let mut found = false;
            for path in candidates {
                let Some(record) = source.effective(&path) else {
                    continue;
                };
                found = true;
                per_track
                    .get_mut(&fold_path(&track.id))
                    .expect("selected track")
                    .4 += 1;
                let (status, detail) =
                    dependency_status(&source, &target, target_data_raw, record, &path)?;
                if status == "staged" {
                    add_pending(
                        &target,
                        target_data_raw,
                        &mut pending,
                        record,
                        &path,
                        "sound_dependency",
                    );
                }
                if status == "target_conflict_preserved" {
                    per_track
                        .get_mut(&fold_path(&track.id))
                        .expect("selected track")
                        .6 += 1;
                }
                dependencies.push(DependencyReport {
                    track_id: track.id.clone(),
                    kind: "sound".to_owned(),
                    symbol: sound.clone(),
                    path: Some(path),
                    status: status.to_owned(),
                    detail,
                });
                break;
            }
            if !found {
                per_track
                    .get_mut(&fold_path(&track.id))
                    .expect("selected track")
                    .5 += 1;
                dependencies.push(DependencyReport {
                    track_id: track.id.clone(),
                    kind: "sound".to_owned(),
                    symbol: sound,
                    path: None,
                    status: "source_missing".to_owned(),
                    detail: "no surround OGG/WAV resolved this serialized sound key".to_owned(),
                });
            }
        }

        let bgm_prefix = format!("sound/bgm/{}", fold_path(&track.theme));
        for record in source.effective_records().filter(|record| {
            path_in_prefix(&record.virtual_path, &bgm_prefix)
                && matches!(
                    Path::new(&record.virtual_path)
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .map(str::to_ascii_lowercase)
                        .as_deref(),
                    Some("ogg" | "wav")
                )
        }) {
            let path = record.virtual_path.replace('\\', "/");
            per_track
                .get_mut(&fold_path(&track.id))
                .expect("selected track")
                .4 += 1;
            let (status, detail) =
                dependency_status(&source, &target, target_data_raw, record, &path)?;
            if status == "staged" {
                add_pending(
                    &target,
                    target_data_raw,
                    &mut pending,
                    record,
                    &path,
                    "bgm_dependency",
                );
            }
            if status == "target_conflict_preserved" {
                per_track
                    .get_mut(&fold_path(&track.id))
                    .expect("selected track")
                    .6 += 1;
            }
            dependencies.push(DependencyReport {
                track_id: track.id.clone(),
                kind: "bgm".to_owned(),
                symbol: Path::new(&path)
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or(&path)
                    .to_owned(),
                path: Some(path),
                status: status.to_owned(),
                detail,
            });
        }

        let mut embedded_seen = HashSet::new();
        let track_records = source
            .effective_records()
            .filter(|record| {
                track_folder_id(&record.virtual_path)
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(&track.id))
            })
            .cloned()
            .collect::<Vec<_>>();
        for container in track_records {
            let extension = Path::new(&container.virtual_path)
                .extension()
                .and_then(|extension| extension.to_str())
                .map(str::to_ascii_lowercase);
            if !matches!(
                extension.as_deref(),
                Some("1s" | "uset" | "kap" | "xml" | "kml" | "bml")
            ) {
                continue;
            }
            let bytes = source.extract(&container)?;
            for reference in embedded_asset_references(&bytes) {
                let resolved = resolve_embedded_reference(
                    &source,
                    &reference_paths,
                    &container.virtual_path,
                    track,
                    &reference,
                );
                let dedup = format!(
                    "{}\0{}",
                    fold_path(&reference),
                    resolved.as_deref().map(fold_path).unwrap_or_default()
                );
                if !embedded_seen.insert(dedup) {
                    continue;
                }
                per_track
                    .get_mut(&fold_path(&track.id))
                    .expect("selected track")
                    .7 += 1;
                let Some(path) = resolved else {
                    per_track
                        .get_mut(&fold_path(&track.id))
                        .expect("selected track")
                        .5 += 1;
                    dependencies.push(DependencyReport {
                        track_id: track.id.clone(),
                        kind: "embedded_asset".to_owned(),
                        symbol: reference,
                        path: None,
                        status: "source_missing".to_owned(),
                        detail: format!(
                            "serialized reference from {} did not resolve in the mounted source namespace",
                            container.virtual_path
                        ),
                    });
                    continue;
                };
                let record = source
                    .effective(&path)
                    .expect("resolved embedded path must be effective");
                let (status, detail) =
                    dependency_status(&source, &target, target_data_raw, record, &path)?;
                if status == "staged" {
                    add_pending(
                        &target,
                        target_data_raw,
                        &mut pending,
                        record,
                        &path,
                        "embedded_dependency",
                    );
                }
                if status == "target_conflict_preserved" {
                    per_track
                        .get_mut(&fold_path(&track.id))
                        .expect("selected track")
                        .6 += 1;
                }
                dependencies.push(DependencyReport {
                    track_id: track.id.clone(),
                    kind: "embedded_asset".to_owned(),
                    symbol: reference,
                    path: Some(path),
                    status: status.to_owned(),
                    detail: format!("{detail}; referenced by {}", container.virtual_path),
                });
            }
        }
    }
    let mut catalog_replacements = vec![
        (SOURCE_TRACK_TABLE.to_owned(), table_bytes),
        (TARGET_TRACK_LOCALE.to_owned(), locale_bytes),
    ];
    catalog_replacements.extend(selector_catalogs.replacements);
    let catalog_paths = catalog_replacements
        .iter()
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    let catalog = materialize_archive(
        target_data,
        catalog_archive_name,
        &catalog_paths,
        &catalog_replacements,
    )?;
    let resource_plaintext_bytes = pending
        .values()
        .try_fold(0_usize, |total, file| total.checked_add(file.source.size))
        .context("resource byte count overflow")?;
    let mut role_counts = BTreeMap::<String, usize>::new();
    let mut extractor = source.extractor();
    let mut resource_entries = Vec::with_capacity(pending.len());
    let pending_count = pending.len();
    for (file_index, file) in pending.values().enumerate() {
        report_progress(
            progress,
            TrackImportPhase::WriteBundle,
            mapped_fraction(0.76, 0.87, file_index, pending_count),
            file_index,
            pending_count,
        );
        *role_counts.entry(file.role.to_owned()).or_default() += 1;
        resource_entries.push(Rho5WriteEntry {
            path: file.target_path.clone(),
            data: extractor.extract(&file.source)?,
            flags: P5136_PACKED_ENTRY_FLAGS,
        });
    }
    let chunks = partition_entries(resource_entries);
    ensure!(
        chunks.len() <= archive_names.len(),
        "track resources require {} empty stock slots, but only {} were supplied",
        chunks.len(),
        archive_names.len()
    );
    let resource_names = &archive_names[..chunks.len()];
    if target_data_raw.is_none() {
        validate_empty_stock_slots(target_data, resource_names)?;
    }
    let limits = output_limits();
    report_progress(
        progress,
        TrackImportPhase::WriteBundle,
        0.88,
        0,
        chunks.len() + 1,
    );
    let mut archives = vec![write_archive(
        output,
        catalog_archive_name,
        "track_catalog",
        catalog.entries,
        &limits,
    )?];
    let resource_archive_count = resource_names.len();
    for (archive_index, (name, entries)) in resource_names.iter().zip(chunks).enumerate() {
        report_progress(
            progress,
            TrackImportPhase::WriteBundle,
            mapped_fraction(0.88, 0.92, archive_index + 1, resource_archive_count + 1),
            archive_index + 1,
            resource_archive_count + 1,
        );
        archives.push(write_archive(
            output,
            name,
            "track_resources",
            entries,
            &limits,
        )?);
    }

    let track_reports = selected
        .iter()
        .map(|track| {
            let counts = per_track[&fold_path(&track.id)];
            TrackReport {
                id: track.id.clone(),
                game_type: track.game_type.clone(),
                theme: track.theme.clone(),
                display_name: track.id.clone(),
                catalog_already_installed: track.already_installed,
                resource_entries: counts.0,
                ai_path_entries: counts.1,
                thumbnail_aliases: counts.2,
                material_dependencies: counts.3,
                sound_dependencies: counts.4,
                embedded_dependencies: counts.7,
                unresolved_dependencies: counts.5,
                conflicting_dependencies: counts.6,
            }
        })
        .collect::<Vec<_>>();
    let theme_reports = themes
        .iter()
        .map(|theme| {
            let counts = per_theme[theme];
            ThemeReport {
                id: selected
                    .iter()
                    .find(|track| fold_path(&track.theme) == *theme)
                    .map_or_else(|| theme.clone(), |track| track.theme.clone()),
                new_to_p5136: !target
                    .effective_records()
                    .any(|record| path_in_prefix(&record.virtual_path, &format!("theme/{theme}"))),
                staged_source_only_entries: counts.0,
                preserved_existing_entries: counts.1,
                selector_catalogs: selector_catalogs
                    .registrations
                    .get(theme)
                    .cloned()
                    .unwrap_or_default(),
            }
        })
        .collect::<Vec<_>>();
    let mut warnings = Vec::new();
    for theme in &theme_reports {
        if let Some(alias) = theme_slot_alias(&theme.id) {
            warnings.push(format!(
                "theme {} uses P5136 native selector slot {} ({}) because the client enum ends at 34; original track/resource IDs remain unchanged",
                theme.id, alias.native_id, alias.native_code
            ));
        }
        if theme.preserved_existing_entries > 0 {
            warnings.push(format!(
                "theme {} kept {} P5136 same-path resources; this bundle never overwrites an existing theme file",
                theme.id, theme.preserved_existing_entries
            ));
        }
    }
    let unresolved = track_reports
        .iter()
        .map(|track| track.unresolved_dependencies)
        .sum::<usize>();
    let conflicts = track_reports
        .iter()
        .map(|track| track.conflicting_dependencies)
        .sum::<usize>();
    if unresolved > 0 {
        warnings.push(format!(
            "{unresolved} serialized material/sound symbols could not be resolved in the source namespace; inspect the dependency table before runtime testing"
        ));
    }
    if conflicts > 0 {
        warnings.push(format!(
            "{conflicts} dependency paths differ between source and P5136; the non-destructive importer preserved P5136 bytes"
        ));
    }
    warnings.push(
        "Only data assets are staged. Native gimmicks or shaders absent from P5136 can still make an imported track fail at runtime."
            .to_owned(),
    );
    warnings.push(
        "Copy every reported .rho5 file together. Do not copy the cache or reports into the client Data directory."
            .to_owned(),
    );
    let report = TrackBundleReport {
        schema_version: 5,
        source_data: fs::canonicalize(source_data)?.display().to_string(),
        target_data: fs::canonicalize(target_data)?.display().to_string(),
        selection_rule: format!(
            "selected source-only active gameType=item/speed IDs matching *_Ixx or *_Rxx, with optional _kd suffix; source_region={source_region:?}"
        ),
        tracks: track_reports,
        themes: theme_reports,
        archives,
        resource_entries: pending.len(),
        resource_plaintext_bytes,
        preserved_catalog_entries: catalog.preserved_entries,
        role_counts,
        dependencies,
        warnings,
        content_registry_catalogs: selector_catalogs.content_registry_catalogs,
    };
    report_progress(progress, TrackImportPhase::VerifyBundle, 0.925, 0, 1);
    verify_staged(output, &report)?;
    fs::write(&json_path, serde_json::to_vec_pretty(&report)?)?;
    fs::write(&markdown_path, render_markdown(&report))?;
    report_progress(progress, TrackImportPhase::VerifyBundle, 0.94, 1, 1);
    println!(
        "staged tracks={} resources={} archives={} report={}",
        report.tracks.len(),
        report.resource_entries,
        report.archives.len(),
        markdown_path.display()
    );
    Ok(())
}

/// Installs a verified staged track bundle into a complete `DataRaw` tree.
///
/// Resource entries are additive and may only reuse an existing path when the
/// plaintext bytes are identical. Merged track/locale/theme catalog files are backed up
/// once and then replaced, making repeated installs idempotent without losing
/// the original pre-import state.
pub(crate) fn install_tracks_dataraw(bundle: &Path, data_raw: &Path, backup: &Path) -> Result<()> {
    install_tracks_dataraw_with_progress(bundle, data_raw, backup, &mut |_| {})
}

#[allow(clippy::too_many_lines)]
fn install_tracks_dataraw_with_progress(
    bundle: &Path,
    data_raw: &Path,
    backup: &Path,
    progress: ProgressCallback<'_>,
) -> Result<()> {
    report_progress(progress, TrackImportPhase::InstallDataRaw, 0.945, 0, 0);
    ensure!(
        data_raw.is_dir(),
        "DataRaw directory does not exist: {}",
        data_raw.display()
    );
    ensure!(
        bundle.is_dir(),
        "track bundle directory does not exist: {}",
        bundle.display()
    );
    ensure!(
        data_raw.join(SOURCE_TRACK_TABLE).is_file() && data_raw.join(TARGET_TRACK_LOCALE).is_file(),
        "destination is not a complete P5136 DataRaw tree"
    );

    let canonical_bundle = fs::canonicalize(bundle)?;
    let canonical_data_raw = fs::canonicalize(data_raw)?;
    ensure!(
        canonical_bundle != canonical_data_raw
            && !canonical_bundle.starts_with(&canonical_data_raw),
        "track bundle must be outside DataRaw"
    );
    let backup_absolute = absolute_path(backup)?;
    ensure!(
        backup_absolute != canonical_data_raw && !backup_absolute.starts_with(&canonical_data_raw),
        "backup directory must be outside DataRaw"
    );

    let report_path = canonical_bundle.join(REPORT_JSON);
    let report: TrackBundleReport = serde_json::from_slice(
        &fs::read(&report_path)
            .with_context(|| format!("failed to read {}", report_path.display()))?,
    )?;
    ensure!(
        matches!(report.schema_version, 1..=5),
        "unsupported track bundle schema"
    );
    ensure!(!report.tracks.is_empty(), "track bundle contains no tracks");

    let archive_count = report.archives.len();
    for (archive_index, archive) in report.archives.iter().enumerate() {
        report_progress(
            progress,
            TrackImportPhase::InstallDataRaw,
            mapped_fraction(0.945, 0.96, archive_index, archive_count),
            archive_index,
            archive_count,
        );
        validate_archive_name(&archive.name)?;
        let path = canonical_bundle.join(&archive.name);
        let bytes = fs::read(&path)
            .with_context(|| format!("failed to read staged archive {}", path.display()))?;
        ensure!(
            bytes.len() == archive.bytes,
            "archive size mismatch for {}",
            archive.name
        );
        ensure!(
            format!("{:x}", Sha256::digest(&bytes)).eq_ignore_ascii_case(&archive.sha256),
            "archive hash mismatch for {}",
            archive.name
        );
    }

    let directory = Rho5Directory::scan_kr(&canonical_bundle, Rho5Limits::default())?;
    ensure!(
        directory.archive_count() == report.archives.len(),
        "bundle directory contains unreported RHO5 archives"
    );
    let archive_roles = report
        .archives
        .iter()
        .map(|archive| (archive.name.to_ascii_lowercase(), archive.role.as_str()))
        .collect::<HashMap<_, _>>();
    let mut installed = 0_usize;
    let mut identical = 0_usize;
    let mut replaced = 0_usize;
    let mut installed_paths = Vec::new();

    let entry_count = directory.entries().len();
    for (entry_index, entry) in directory.entries().iter().enumerate() {
        report_progress(
            progress,
            TrackImportPhase::InstallDataRaw,
            mapped_fraction(0.96, 0.995, entry_index, entry_count),
            entry_index,
            entry_count,
        );
        let role = archive_roles
            .get(&entry.archive_name().to_ascii_lowercase())
            .with_context(|| {
                format!(
                    "archive {} is not in the bundle report",
                    entry.archive_name()
                )
            })?;
        let path = entry.normalized_path();
        let is_catalog = is_track_bundle_catalog_path(path);
        if *role == "track_catalog" && !is_catalog {
            continue;
        }
        ensure!(
            *role == "track_catalog" || *role == "track_resources",
            "unsupported track bundle archive role {role:?}"
        );
        let relative = safe_relative_path(path)?;
        let destination = canonical_data_raw.join(&relative);
        let bytes = directory.extract_entry(entry)?;
        if destination.is_file() {
            let existing = fs::read(&destination)?;
            if existing == bytes {
                identical += 1;
                continue;
            }
            ensure!(
                is_catalog,
                "resource path already exists with different bytes: {}",
                destination.display()
            );
            backup_once(&destination, &backup_absolute.join(&relative))?;
            replaced += 1;
        } else {
            ensure!(
                !destination.exists(),
                "refusing to replace non-file {}",
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
        installed += 1;
        installed_paths.push(path.to_owned());
    }

    let installed_table = decode_bml(
        SOURCE_TRACK_TABLE,
        &fs::read(canonical_data_raw.join(SOURCE_TRACK_TABLE))?,
    )?;
    let installed_locale = decode_bml(
        TARGET_TRACK_LOCALE,
        &fs::read(canonical_data_raw.join(TARGET_TRACK_LOCALE))?,
    )?;
    for track in &report.tracks {
        ensure!(
            installed_table
                .children
                .iter()
                .any(|node| attribute(node, "id") == Some(track.id.as_str())),
            "installed track table is missing {}",
            track.id
        );
        ensure!(
            installed_locale
                .children
                .iter()
                .any(|node| attribute(node, "id") == Some(track.id.as_str())),
            "installed track locale is missing {}",
            track.id
        );
    }
    for theme in &report.themes {
        for path in &theme.selector_catalogs {
            let installed_config = decode_bml(path, &fs::read(canonical_data_raw.join(path))?)?;
            ensure!(
                selector_has_theme(&installed_config, &theme.id),
                "installed selector catalog {path} is missing theme {}",
                theme.id
            );
        }
    }
    if report
        .themes
        .iter()
        .any(|theme| theme_slot_alias(&theme.id).is_some())
    {
        let string_bag = decode_bml(
            SELECT_TRACK_STRING_BAG,
            &fs::read(canonical_data_raw.join(SELECT_TRACK_STRING_BAG))?,
        )?;
        for theme in &report.themes {
            if let Some(alias) = theme_slot_alias(&theme.id) {
                ensure!(
                    string_bag_has_key(&string_bag, alias.string_key),
                    "installed selector string bag is missing {}",
                    alias.string_key
                );
            }
        }
    }
    for path in &report.content_registry_catalogs {
        let content_config = fs::read(canonical_data_raw.join(path))?;
        for theme in &report.themes {
            if let Some(alias) = theme_slot_alias(&theme.id) {
                ensure!(
                    content_registry_entry_enabled(path, &content_config, alias.content_key)?,
                    "installed content registry {path} does not enable {}",
                    alias.content_key
                );
            }
        }
    }

    let install_report = serde_json::json!({
        "schema_version": 1,
        "tracks": report.tracks.iter().map(|track| track.id.as_str()).collect::<Vec<_>>(),
        "data_raw": canonical_data_raw.display().to_string(),
        "backup": backup_absolute.display().to_string(),
        "written": installed,
        "replaced": replaced,
        "identical_existing": identical,
        "written_paths": installed_paths,
    });
    fs::write(
        canonical_bundle.join("dataraw-install-report.json"),
        serde_json::to_vec_pretty(&install_report)?,
    )?;
    report_progress(
        progress,
        TrackImportPhase::InstallDataRaw,
        0.999,
        entry_count,
        entry_count,
    );
    println!(
        "installed tracks={} written={} replaced={} identical={} backup={}",
        report.tracks.len(),
        installed,
        replaced,
        identical,
        backup_absolute.display()
    );
    Ok(())
}

fn safe_relative_path(value: &str) -> Result<PathBuf> {
    let normalized = value.replace('\\', "/");
    let path = Path::new(&normalized);
    ensure!(
        !path.is_absolute(),
        "absolute asset path is not allowed: {value:?}"
    );
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => output.push(value),
            _ => bail!("unsafe asset path component in {value:?}"),
        }
    }
    ensure!(
        !output.as_os_str().is_empty(),
        "empty asset path is not allowed"
    );
    Ok(output)
}

fn track_material_symbols(bytes: &[u8]) -> BTreeSet<String> {
    const TAG: [u8; 2] = [0xAA, 0x27];
    const MIN_SYMBOL_UNITS: usize = 4;
    const MAX_SYMBOL_UNITS: usize = 64;
    let mut symbols = BTreeSet::new();
    for offset in 0..bytes.len().saturating_sub(8) {
        if bytes[offset..offset + TAG.len()] != TAG {
            continue;
        }
        let length = u32::from_le_bytes([
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
            bytes[offset + 7],
        ]) as usize;
        if !(MIN_SYMBOL_UNITS..=MAX_SYMBOL_UNITS).contains(&length) {
            continue;
        }
        let Some(byte_length) = length.checked_mul(2) else {
            continue;
        };
        let start = offset + 8;
        let Some(end) = start.checked_add(byte_length) else {
            continue;
        };
        let Some(encoded) = bytes.get(start..end) else {
            continue;
        };
        let units = encoded
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        let Ok(symbol) = String::from_utf16(&units) else {
            continue;
        };
        let symbol = symbol.trim_end_matches('\0');
        if matches!(symbol, "property" | "transparency" | "waterdrop")
            || !symbol.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b' ' | b'.' | b'-')
            })
            || !symbol
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphabetic)
        {
            continue;
        }
        symbols.insert(symbol.to_owned());
    }
    symbols
}

fn material_path_index(index: &AssetIndex) -> HashMap<String, Vec<String>> {
    let mut paths = HashMap::<String, Vec<String>>::new();
    for record in index.effective_records() {
        let path = record.virtual_path.replace('\\', "/");
        let folded = fold_path(&path);
        let supported_root = (folded.starts_with("theme/") && folded.contains("/texture/"))
            || folded.starts_with("track/")
            || folded.starts_with("track_/")
            || (folded.starts_with("zeta/") && folded.contains("/ingame/"));
        if !supported_root
            || !matches!(
                Path::new(&path)
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .map(str::to_ascii_lowercase)
                    .as_deref(),
                Some("dds" | "png")
            )
        {
            continue;
        }
        let Some(stem) = Path::new(&path).file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        paths.entry(fold_path(stem)).or_default().push(path);
    }
    for candidates in paths.values_mut() {
        candidates.sort_unstable_by_key(|path| fold_path(path));
        candidates.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    }
    paths
}

fn material_candidate_paths(
    index: &HashMap<String, Vec<String>>,
    track: &SelectedTrack,
    symbol: &str,
) -> Vec<String> {
    let mut candidates = index.get(&fold_path(symbol)).cloned().unwrap_or_default();
    candidates.sort_by(|left, right| {
        material_path_priority(left, track)
            .cmp(&material_path_priority(right, track))
            .then_with(|| fold_path(left).cmp(&fold_path(right)))
    });
    candidates
}

fn material_path_priority(path: &str, track: &SelectedTrack) -> u8 {
    if path_in_prefix(path, &format!("track_/{}", track.id))
        || path_in_prefix(path, &format!("track/{}", track.id))
    {
        0
    } else if path_in_prefix(path, &format!("theme/{}/texture", track.theme)) {
        1
    } else if path_in_prefix(path, "theme/common/texture") {
        2
    } else if path_in_prefix(path, "theme") {
        3
    } else if path_in_prefix(path, "zeta") {
        4
    } else {
        5
    }
}

fn track_sound_keys(bytes: &[u8]) -> BTreeSet<String> {
    let encoded_key = "filename"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    let mut sounds = BTreeSet::new();
    let mut cursor = 0_usize;
    while let Some(relative) = bytes[cursor..]
        .windows(encoded_key.len())
        .position(|window| window == encoded_key)
    {
        let key_start = cursor + relative;
        let value_length_offset = key_start + encoded_key.len();
        let Some(length_bytes) = bytes.get(value_length_offset..value_length_offset + 4) else {
            break;
        };
        let units = u32::from_le_bytes(length_bytes.try_into().expect("four bytes")) as usize;
        if (1..=256).contains(&units) {
            let value_start = value_length_offset + 4;
            if let Some(value_end) = units
                .checked_mul(2)
                .and_then(|bytes| value_start.checked_add(bytes))
                && let Some(encoded) = bytes.get(value_start..value_end)
            {
                let utf16 = encoded
                    .chunks_exact(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .collect::<Vec<_>>();
                if let Ok(value) = String::from_utf16(&utf16) {
                    let value = value.trim_matches(['\0', ' ', '\t']);
                    if !value.is_empty()
                        && !value.contains(['\0', ':'])
                        && !value.starts_with(['/', '\\'])
                    {
                        sounds.insert(value.replace('\\', "/"));
                    }
                }
            }
        }
        cursor = value_length_offset;
    }
    sounds
}

fn sound_candidate_paths(sound: &str) -> Vec<String> {
    let normalized = sound.replace('\\', "/");
    let lower = normalized.to_ascii_lowercase();
    if lower.starts_with("sound/") {
        if Path::new(&normalized).extension().is_some() {
            return vec![normalized];
        }
        return vec![format!("{normalized}.ogg"), format!("{normalized}.wav")];
    }
    if Path::new(&normalized).extension().is_some() {
        vec![format!("sound/fx/surround/{normalized}")]
    } else {
        vec![
            format!("sound/fx/surround/{normalized}.ogg"),
            format!("sound/fx/surround/{normalized}.wav"),
        ]
    }
}

fn reference_path_index(index: &AssetIndex) -> HashMap<String, Vec<String>> {
    let mut paths = HashMap::<String, Vec<String>>::new();
    for record in index.effective_records() {
        let path = record.virtual_path.replace('\\', "/");
        let extension = Path::new(&path)
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase);
        if !extension.as_deref().is_some_and(is_track_asset_extension) {
            continue;
        }
        let Some(name) = Path::new(&path).file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        paths.entry(fold_path(name)).or_default().push(path);
    }
    for candidates in paths.values_mut() {
        candidates.sort_unstable_by_key(|path| fold_path(path));
        candidates.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    }
    paths
}

fn embedded_asset_references(bytes: &[u8]) -> BTreeSet<String> {
    let mut strings = Vec::new();
    let mut ascii = Vec::new();
    for byte in bytes.iter().copied().chain([0]) {
        if byte.is_ascii_graphic() || byte == b' ' {
            ascii.push(byte);
        } else {
            if ascii.len() >= 4 {
                strings.push(String::from_utf8_lossy(&ascii).into_owned());
            }
            ascii.clear();
        }
    }
    for offset in 0..bytes.len().saturating_sub(8) {
        let units = u32::from_le_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .expect("bounded four-byte window"),
        ) as usize;
        if !(4..=4_096).contains(&units) {
            continue;
        }
        let start = offset + 4;
        let Some(end) = units
            .checked_mul(2)
            .and_then(|byte_length| start.checked_add(byte_length))
        else {
            continue;
        };
        let Some(encoded) = bytes.get(start..end) else {
            continue;
        };
        let utf16 = encoded
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        if let Ok(value) = String::from_utf16(&utf16)
            && value
                .chars()
                .all(|character| !character.is_control() || matches!(character, '\t' | '\n'))
        {
            strings.push(value);
        }
    }
    strings
        .into_iter()
        .flat_map(|value| reference_tokens(&value))
        .collect()
}

fn reference_tokens(value: &str) -> Vec<String> {
    value
        .split(|character: char| {
            !(character.is_alphanumeric()
                || matches!(character, '_' | '-' | '.' | '@' | '/' | '\\'))
        })
        .filter_map(|token| {
            let token = token.trim().replace('\\', "/");
            let extension = Path::new(&token)
                .extension()
                .and_then(|extension| extension.to_str())?
                .to_ascii_lowercase();
            (token.len() <= 4_096 && is_track_asset_extension(&extension)).then_some(token)
        })
        .collect()
}

fn is_track_asset_extension(extension: &str) -> bool {
    matches!(
        extension,
        "1s" | "dds"
            | "tga"
            | "png"
            | "jpg"
            | "jpeg"
            | "bmh"
            | "bmx"
            | "f30"
            | "hdr"
            | "fft"
            | "wav"
            | "ogg"
            | "flac"
            | "xml"
            | "kml"
            | "bml"
            | "uset"
            | "kap"
    )
}

fn resolve_embedded_reference(
    source: &AssetIndex,
    basename_index: &HashMap<String, Vec<String>>,
    container_path: &str,
    track: &SelectedTrack,
    reference: &str,
) -> Option<String> {
    let parent = container_path
        .rsplit_once('/')
        .map_or("", |(parent, _)| parent);
    let name = Path::new(reference)
        .file_name()
        .and_then(|name| name.to_str())?;
    let mut attempts = [
        normalize_virtual_path(reference),
        normalize_virtual_path(&format!("{parent}/{reference}")),
        normalize_virtual_path(&format!("track_/{}/{reference}", track.id)),
        normalize_virtual_path(&format!("track/{}/{reference}", track.id)),
        normalize_virtual_path(&format!("theme/{}/{reference}", track.theme)),
        normalize_virtual_path(&format!("theme/{}/texture/{name}", track.theme)),
        normalize_virtual_path(&format!("theme/common/{reference}")),
        normalize_virtual_path(&format!("theme/common/texture/{name}")),
        normalize_virtual_path(&format!("sound/fx/surround/{name}")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    attempts.sort_unstable_by_key(|path| fold_path(path));
    attempts.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    for attempt in attempts {
        if let Some(record) = source.effective(&attempt) {
            return Some(record.virtual_path.clone());
        }
    }
    let mut basenames = basename_index
        .get(&fold_path(name))?
        .iter()
        .filter(|path| !path_in_prefix(path, "zeta"))
        .cloned()
        .collect::<Vec<_>>();
    basenames.sort_by(|left, right| {
        embedded_path_priority(left, track)
            .cmp(&embedded_path_priority(right, track))
            .then_with(|| fold_path(left).cmp(&fold_path(right)))
    });
    let first = basenames.first()?;
    let priority = embedded_path_priority(first, track);
    (basenames
        .iter()
        .filter(|path| embedded_path_priority(path, track) == priority)
        .count()
        == 1)
        .then(|| first.clone())
}

fn embedded_path_priority(path: &str, track: &SelectedTrack) -> u8 {
    if path_in_prefix(path, &format!("track_/{}", track.id))
        || path_in_prefix(path, &format!("track/{}", track.id))
    {
        0
    } else if path_in_prefix(path, &format!("theme/{}", track.theme)) {
        1
    } else if path_in_prefix(path, "theme/common") {
        2
    } else if path_in_prefix(path, "theme") {
        3
    } else if path_in_prefix(path, "sound") {
        4
    } else {
        5
    }
}

fn normalize_virtual_path(value: &str) -> Option<String> {
    if value.contains(':') || value.starts_with(['/', '\\']) {
        return None;
    }
    let normalized = value.replace('\\', "/");
    let mut components = Vec::new();
    for component in normalized.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop()?;
            }
            value => components.push(value),
        }
    }
    (!components.is_empty()).then(|| components.join("/"))
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
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
    fs::copy(source, backup).with_context(|| {
        format!(
            "failed to back up {} to {}",
            source.display(),
            backup.display()
        )
    })?;
    Ok(())
}

pub(crate) fn is_ordinary_track_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    let marker = bytes
        .windows(2)
        .rposition(|pair| pair[0] == b'_' && matches!(pair[1], b'I' | b'i' | b'R' | b'r'));
    let Some(marker) = marker else {
        return false;
    };
    if marker == 0 || bytes.len() < marker + 4 {
        return false;
    }
    let number = &bytes[marker + 2..marker + 4];
    if !number.iter().all(u8::is_ascii_digit) {
        return false;
    }
    let suffix = &id[marker + 4..];
    suffix.is_empty() || suffix.eq_ignore_ascii_case("_kd")
}

pub(crate) fn active_ordinary_track_ids(index: &AssetIndex) -> Result<HashSet<String>> {
    let table = decode_effective(index, SOURCE_TRACK_TABLE)?;
    let locale = decode_effective(index, SOURCE_TRACK_LOCALE)?;
    validate_track_root(SOURCE_TRACK_TABLE, &table)?;
    validate_track_root(SOURCE_TRACK_LOCALE, &locale)?;
    let locales = nodes_by_id(&locale)?;
    Ok(table
        .children
        .iter()
        .filter_map(|definition| {
            let id = attribute(definition, "id")?;
            let locale = locales.get(&fold_path(id))?;
            let game_type = attribute(definition, "gameType").unwrap_or_default();
            (is_ordinary_track_id(id)
                && matches!(game_type.to_ascii_lowercase().as_str(), "item" | "speed")
                && !is_true(attribute(locale, "blocked"))
                && attribute(locale, "choosable").is_none_or(|value| is_true(Some(value))))
            .then(|| fold_path(id))
        })
        .collect())
}

fn track_theme(id: &str) -> Option<&str> {
    let lower = id.to_ascii_lowercase();
    let marker = lower
        .rfind("_i")
        .into_iter()
        .chain(lower.rfind("_r"))
        .max()?;
    Some(&id[..marker])
}

fn decode_effective(index: &AssetIndex, path: &str) -> Result<BmlNode> {
    let record = index
        .effective(path)
        .with_context(|| format!("effective BML {path:?} was not found"))?;
    decode_bml(path, &index.extract(record)?)
}

fn decode_target_bml(
    target: &AssetIndex,
    target_data_raw: Option<&Path>,
    path: &str,
) -> Result<BmlNode> {
    if let Some(root) = target_data_raw {
        let candidate = root.join(safe_relative_path(path)?);
        if candidate.is_file() {
            return decode_bml(path, &fs::read(&candidate)?);
        }
    }
    decode_effective(target, path)
}

fn merge_selector_theme_catalogs(
    source: &AssetIndex,
    target: &AssetIndex,
    target_data_raw: Option<&Path>,
    themes: &BTreeMap<String, String>,
) -> Result<SelectorCatalogMerge> {
    let mut replacements = Vec::new();
    let mut content_registry_catalogs = Vec::new();
    let mut registrations = themes
        .keys()
        .map(|theme| (theme.clone(), Vec::new()))
        .collect::<BTreeMap<_, _>>();

    for path in SELECT_TRACK_CONFIGS {
        let Some(target_bytes) = target_bytes(target, target_data_raw, path)? else {
            continue;
        };
        let mut target_config = decode_bml(path, &target_bytes)?;
        let source_config = source
            .effective(path)
            .map(|record| source.extract(record))
            .transpose()?
            .map(|bytes| decode_bml(path, &bytes))
            .transpose()?;
        let registered = merge_theme_tab_order(source_config.as_ref(), &mut target_config, themes)
            .with_context(|| format!("failed to merge selector themes into {path}"))?;
        for theme in registered {
            registrations
                .get_mut(&theme)
                .expect("merged only requested themes")
                .push(path.to_owned());
        }
        replacements.push((path.to_owned(), encode_bml(path, &target_config)?));
    }

    if themes
        .values()
        .any(|theme| theme_slot_alias(theme).is_some())
    {
        let string_bag_bytes = target_bytes(target, target_data_raw, SELECT_TRACK_STRING_BAG)?
            .context("P5136 selectTrackEx string bag is missing")?;
        let mut string_bag = decode_bml(SELECT_TRACK_STRING_BAG, &string_bag_bytes)?;
        for theme in themes.values() {
            if let Some(alias) = theme_slot_alias(theme) {
                ensure_theme_string(&mut string_bag, alias.string_key, theme);
            }
        }
        replacements.push((
            SELECT_TRACK_STRING_BAG.to_owned(),
            encode_bml(SELECT_TRACK_STRING_BAG, &string_bag)?,
        ));

        let target_bytes = target_bytes(target, target_data_raw, CONTENT_CONFIG)?
            .context("P5136 content/config.xml is missing")?;
        let mut content_config = target_bytes;
        for theme in themes.values() {
            if let Some(alias) = theme_slot_alias(theme) {
                content_config = enable_content_registry_entry(
                    CONTENT_CONFIG,
                    &content_config,
                    alias.content_key,
                    "themeMechanic",
                )?;
            }
        }
        replacements.push((CONTENT_CONFIG.to_owned(), content_config));
        content_registry_catalogs.push(CONTENT_CONFIG.to_owned());
    }

    for (theme, paths) in &registrations {
        ensure!(
            !paths.is_empty(),
            "P5136 has no usable selectTrackEx theme catalog in which to register {:?}",
            themes.get(theme).expect("requested theme")
        );
    }
    Ok(SelectorCatalogMerge {
        replacements,
        registrations,
        content_registry_catalogs,
    })
}

fn ensure_theme_string(root: &mut BmlNode, key: &str, theme: &str) {
    let entry = if let Some(index) = root.children.iter().position(|node| {
        node.name.eq_ignore_ascii_case("k")
            && attribute(node, "n").is_some_and(|name| name.eq_ignore_ascii_case(key))
    }) {
        &mut root.children[index]
    } else {
        let mut entry = BmlNode::new("k", "");
        entry.attributes.push(("n".to_owned(), key.to_owned()));
        root.children.push(entry);
        root.children.last_mut().expect("just appended string key")
    };
    let translations = [
        ("kr", "봉신"),
        ("us", "Fengshen"),
        ("cn", "封神"),
        ("tw", "封神"),
        ("jp", "Fengshen"),
        ("th", "Fengshen"),
        ("vn", "Fengshen"),
        ("ru", "Fengshen"),
        ("id", "Fengshen"),
    ];
    for (locale, translated) in translations {
        let translated = if theme.eq_ignore_ascii_case("fengshen") {
            translated
        } else {
            theme
        };
        if let Some(node) = entry.children.iter_mut().find(|node| {
            node.name.eq_ignore_ascii_case("m")
                && attribute(node, "c").is_some_and(|code| code.eq_ignore_ascii_case(locale))
        }) {
            set_attribute(node, "v", translated);
        } else {
            let mut node = BmlNode::new("m", "");
            node.attributes.push(("c".to_owned(), locale.to_owned()));
            node.attributes
                .push(("v".to_owned(), translated.to_owned()));
            entry.children.push(node);
        }
    }
}

fn merge_theme_tab_order(
    source_config: Option<&BmlNode>,
    target_config: &mut BmlNode,
    themes: &BTreeMap<String, String>,
) -> Result<BTreeSet<String>> {
    ensure!(
        target_config.name.eq_ignore_ascii_case("config"),
        "selector config root is {:?}, expected config",
        target_config.name
    );
    let source_order = source_config
        .and_then(|config| {
            config
                .children
                .iter()
                .find(|node| node.name.eq_ignore_ascii_case("themeTabOrder"))
        })
        .map(|order| order.children.clone())
        .unwrap_or_default();
    let target_order = target_config
        .children
        .iter_mut()
        .find(|node| node.name.eq_ignore_ascii_case("themeTabOrder"))
        .context("selector config has no themeTabOrder node")?;
    let mut registered = BTreeSet::new();

    for (folded_theme, original_theme) in themes {
        let alias = theme_slot_alias(original_theme);
        let registration_id = selector_registration_id(original_theme);
        if let Some(alias) = alias {
            target_order.children.retain(|node| {
                attribute(node, "id").is_none_or(|id| {
                    !id.eq_ignore_ascii_case(original_theme)
                        && !id.eq_ignore_ascii_case(alias.native_code)
                        && id != registration_id
                })
            });
        } else if target_order.children.iter().any(|node| {
            attribute(node, "id").is_some_and(|id| id.eq_ignore_ascii_case(&registration_id))
        }) {
            registered.insert(folded_theme.clone());
            continue;
        }

        let source_index = source_order.iter().position(|node| {
            attribute(node, "id").is_some_and(|id| fold_path(id) == *folded_theme)
        });
        let mut row = source_index
            .and_then(|index| source_order.get(index))
            .cloned()
            .unwrap_or_else(|| {
                let mut row = BmlNode::new("theme", "");
                row.attributes
                    .push(("id".to_owned(), original_theme.to_owned()));
                row
            });
        "theme".clone_into(&mut row.name);
        set_attribute(&mut row, "id", &registration_id);
        if let Some(alias) = alias {
            set_attribute(&mut row, "stringKey", alias.string_key);
            set_attribute(&mut row, "icon", alias.icon);
        }

        let insertion = source_index
            .and_then(|index| {
                source_order[index + 1..].iter().find_map(|candidate| {
                    let id = attribute(candidate, "id")?;
                    target_order.children.iter().position(|target| {
                        attribute(target, "id")
                            .is_some_and(|target_id| fold_path(target_id) == fold_path(id))
                    })
                })
            })
            .or_else(|| {
                source_index.and_then(|index| {
                    source_order[..index].iter().rev().find_map(|candidate| {
                        let id = attribute(candidate, "id")?;
                        target_order
                            .children
                            .iter()
                            .position(|target| {
                                attribute(target, "id")
                                    .is_some_and(|target_id| fold_path(target_id) == fold_path(id))
                            })
                            .map(|position| position + 1)
                    })
                })
            })
            .unwrap_or(target_order.children.len());
        target_order.children.insert(insertion, row);
        registered.insert(folded_theme.clone());
    }
    Ok(registered)
}

fn decode_bml(path: &str, bytes: &[u8]) -> Result<BmlNode> {
    let mut reader = PacketReader::new(bytes);
    let root = BmlNode::decode_with_limits(&mut reader, BML_LIMITS)
        .with_context(|| format!("failed to decode {path}"))?;
    ensure!(reader.remaining().is_empty(), "{path} has trailing bytes");
    Ok(root)
}

fn encode_bml(path: &str, root: &BmlNode) -> Result<Vec<u8>> {
    let mut writer = PacketWriter::new();
    root.encode_with_limits(&mut writer, BML_LIMITS)
        .with_context(|| format!("failed to encode {path}"))?;
    Ok(writer.into_inner())
}

#[derive(Debug, Clone, Copy)]
enum XmlTextEncoding {
    Utf16LeBom,
    Utf8Bom,
    Utf8,
}

fn decode_xml_text(path: &str, bytes: &[u8]) -> Result<(String, XmlTextEncoding)> {
    if let Some(payload) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        ensure!(
            payload.len() % 2 == 0,
            "{path} contains an odd number of UTF-16LE bytes"
        );
        let units = payload
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect::<Vec<_>>();
        return Ok((
            String::from_utf16(&units).with_context(|| format!("{path} is not valid UTF-16LE"))?,
            XmlTextEncoding::Utf16LeBom,
        ));
    }
    if let Some(payload) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return Ok((
            std::str::from_utf8(payload)
                .with_context(|| format!("{path} is not valid UTF-8"))?
                .to_owned(),
            XmlTextEncoding::Utf8Bom,
        ));
    }
    Ok((
        std::str::from_utf8(bytes)
            .with_context(|| format!("{path} is neither UTF-16LE nor UTF-8"))?
            .to_owned(),
        XmlTextEncoding::Utf8,
    ))
}

fn encode_xml_text(text: &str, encoding: XmlTextEncoding) -> Vec<u8> {
    match encoding {
        XmlTextEncoding::Utf16LeBom => {
            let mut output = vec![0xFF, 0xFE];
            output.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
            output
        }
        XmlTextEncoding::Utf8Bom => {
            let mut output = vec![0xEF, 0xBB, 0xBF];
            output.extend_from_slice(text.as_bytes());
            output
        }
        XmlTextEncoding::Utf8 => text.as_bytes().to_vec(),
    }
}

fn find_content_element(text: &str, key: &str) -> Option<(usize, usize)> {
    let folded = text.to_ascii_lowercase();
    let key = key.to_ascii_lowercase();
    let marker = [format!("name='{key}'"), format!("name=\"{key}\"")]
        .into_iter()
        .find_map(|marker| folded.find(&marker))?;
    let start = folded[..marker].rfind("<content")?;
    let end = marker + folded[marker..].find("/>")? + 2;
    Some((start, end))
}

fn set_xml_bool_attribute(element: &mut String, name: &str) -> Result<()> {
    let folded = element.to_ascii_lowercase();
    let name = name.to_ascii_lowercase();
    for quote in ['\'', '"'] {
        let marker = format!("{name}={quote}");
        if let Some(value_start) = folded.find(&marker).map(|index| index + marker.len()) {
            let value_end = value_start
                + folded[value_start..]
                    .find(quote)
                    .context("unterminated XML attribute")?;
            element.replace_range(value_start..value_end, "true");
            return Ok(());
        }
    }
    let insertion = element
        .rfind("/>")
        .context("content XML row is not self-closing")?;
    element.insert_str(insertion, &format!(" {name}='true'"));
    Ok(())
}

fn enable_content_registry_entry(
    path: &str,
    bytes: &[u8],
    key: &str,
    insert_after: &str,
) -> Result<Vec<u8>> {
    let (mut text, encoding) = decode_xml_text(path, bytes)?;
    if let Some((start, end)) = find_content_element(&text, key) {
        let mut element = text[start..end].to_owned();
        set_xml_bool_attribute(&mut element, "enable")?;
        set_xml_bool_attribute(&mut element, "visible")?;
        text.replace_range(start..end, &element);
    } else {
        let (_, anchor_end) = find_content_element(&text, insert_after).with_context(|| {
            format!("{path} has no {insert_after} row near which to register {key}")
        })?;
        let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
        text.insert_str(
            anchor_end,
            &format!("{newline}\t<content name='{key}' enable='true' visible='true'/>"),
        );
    }
    let output = encode_xml_text(&text, encoding);
    ensure!(
        content_registry_entry_enabled(path, &output, key)?,
        "failed to enable {key} in {path}"
    );
    Ok(output)
}

fn content_registry_entry_enabled(path: &str, bytes: &[u8], key: &str) -> Result<bool> {
    let (text, _) = decode_xml_text(path, bytes)?;
    let Some((start, end)) = find_content_element(&text, key) else {
        return Ok(false);
    };
    let element = text[start..end].to_ascii_lowercase();
    let attribute_is_true = |name: &str| {
        element.contains(&format!("{name}='true'")) || element.contains(&format!("{name}=\"true\""))
    };
    Ok(attribute_is_true("enable") && attribute_is_true("visible"))
}

fn validate_track_root(path: &str, root: &BmlNode) -> Result<()> {
    ensure!(
        root.name.eq_ignore_ascii_case("trackList"),
        "{path} root is {:?}, expected trackList",
        root.name
    );
    Ok(())
}

fn selector_has_theme(root: &BmlNode, theme: &str) -> bool {
    let registration_id = selector_registration_id(theme);
    root.children
        .iter()
        .find(|node| node.name.eq_ignore_ascii_case("themeTabOrder"))
        .is_some_and(|order| {
            order.children.iter().any(|node| {
                attribute(node, "id").is_some_and(|id| id.eq_ignore_ascii_case(&registration_id))
            })
        })
}

fn string_bag_has_key(root: &BmlNode, key: &str) -> bool {
    root.children.iter().any(|node| {
        node.name.eq_ignore_ascii_case("k")
            && attribute(node, "n").is_some_and(|name| name.eq_ignore_ascii_case(key))
    })
}

fn is_track_bundle_catalog_path(path: &str) -> bool {
    path.eq_ignore_ascii_case(SOURCE_TRACK_TABLE)
        || path.eq_ignore_ascii_case(TARGET_TRACK_LOCALE)
        || path.eq_ignore_ascii_case(SELECT_TRACK_STRING_BAG)
        || path.eq_ignore_ascii_case(CONTENT_CONFIG)
        || SELECT_TRACK_CONFIGS
            .iter()
            .any(|candidate| path.eq_ignore_ascii_case(candidate))
}

fn nodes_by_id(root: &BmlNode) -> Result<HashMap<String, &BmlNode>> {
    let mut output = HashMap::new();
    for node in &root.children {
        let Some(id) = attribute(node, "id") else {
            continue;
        };
        ensure!(
            output.insert(fold_path(id), node).is_none(),
            "track locale repeats ID {id}"
        );
    }
    Ok(output)
}

fn attribute<'a>(node: &'a BmlNode, name: &str) -> Option<&'a str> {
    node.attributes
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn normalize_definition_for_p5136(source: &BmlNode, theme: &str) -> BmlNode {
    let mut definition = source.clone();
    if attribute(&definition, "level").is_none() {
        let difficulty = attribute(&definition, "difficulty")
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0);
        let level = difficulty.clamp(0, 3).to_string();
        let insertion = definition
            .attributes
            .iter()
            .position(|(name, _)| name.eq_ignore_ascii_case("difficulty"))
            .unwrap_or(definition.attributes.len());
        definition
            .attributes
            .insert(insertion, ("level".to_owned(), level));
    }
    apply_theme_compatibility(&mut definition, theme);
    definition
}

fn apply_theme_compatibility(definition: &mut BmlNode, theme: &str) {
    let Some(alias) = theme_slot_alias(theme) else {
        return;
    };
    set_attribute(definition, "theme", alias.native_code);
    // The stock client derives the material-theme mask from the track ID before it
    // applies the explicit `theme` attribute. A newer prefix such as
    // `fengshen_*` therefore leaves that mask at zero even though the track is
    // classified as XYY. `texTheme` is the separate path that populates the
    // mask, so prepend the borrowed native slot while preserving any genuine
    // cross-theme dependencies already declared by the source row.
    let texture_themes = attribute(definition, "texTheme")
        .map(|value| {
            value
                .split('|')
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .into_iter()
        .flatten()
        .filter(|value| !value.eq_ignore_ascii_case(alias.native_code))
        .fold(alias.native_code.to_owned(), |mut merged, value| {
            merged.push('|');
            merged.push_str(value);
            merged
        });
    set_attribute(definition, "texTheme", &texture_themes);
    // BGM lookup is path/string based rather than the native theme enum, so
    // retain the imported theme's real sound namespace.
    set_attribute(definition, "bgmTheme", theme);
}

fn set_attribute(node: &mut BmlNode, name: &str, value: &str) {
    if let Some((_, existing)) = node
        .attributes
        .iter_mut()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
    {
        value.clone_into(existing);
    } else {
        node.attributes.push((name.to_owned(), value.to_owned()));
    }
}

fn normalize_locale_for_p5136(source: &BmlNode, id: &str) -> BmlNode {
    const OPTIONAL_ATTRIBUTES: &[&str] = &[
        "bpLevelMin",
        "bpLevelMax",
        "allowed1vs1",
        "pursuit1vs1",
        "basicAi",
        "isAllowedRoadBlock",
        "f1speed",
        "isOnlyTraining",
    ];

    let mut locale = BmlNode::new("track", "");
    locale.attributes.push(("id".to_owned(), id.to_owned()));
    // Imported text must remain representable by the Korean P5136 client.
    // Until a reviewed translation exists, the ASCII track ID is deterministic
    // and cannot trip the legacy locale decoder.
    locale.attributes.push(("name".to_owned(), id.to_owned()));
    for name in OPTIONAL_ATTRIBUTES {
        if let Some(value) = attribute(source, name) {
            locale
                .attributes
                .push(((*name).to_owned(), value.to_owned()));
        }
    }
    locale
}

fn is_true(value: Option<&str>) -> bool {
    value.is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
}

fn has_track_resources(index: &AssetIndex, id: &str) -> bool {
    index.effective_records().any(|record| {
        track_folder_id(&record.virtual_path)
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(id))
    })
}

fn track_folder_id(path: &str) -> Option<&str> {
    let mut parts = path.split('/');
    let root = parts.next()?;
    if !matches!(root.to_ascii_lowercase().as_str(), "track" | "track_") {
        return None;
    }
    let id = parts.next()?;
    parts.next()?;
    Some(id)
}

fn ai_path_track_id<'a>(path: &str, tracks: &'a [SelectedTrack]) -> Option<&'a str> {
    let folded = fold_path(path);
    let prefix = "track/common/aipath/";
    let file = folded.strip_prefix(prefix)?;
    tracks
        .iter()
        .find(|track| {
            let id = fold_path(&track.id);
            file.strip_prefix(&id)
                .is_some_and(|rest| rest.starts_with('_'))
        })
        .map(|track| track.id.as_str())
}

fn ancillary_theme<'a>(path: &str, themes: &'a BTreeSet<String>) -> Option<&'a String> {
    let folded = fold_path(path);
    themes.iter().find(|theme| {
        path_in_prefix(&folded, &format!("theme/{theme}"))
            || themed_remainder(&folded, "dialog2/selecttrackex/", theme)
            || themed_remainder(&folded, "sound/bgm/", theme)
            || themed_remainder(&folded, "item/itemcube/", theme)
            || folded
                .strip_prefix("stage/scene/bg/")
                .is_some_and(|rest| rest.starts_with(theme.as_str()))
    })
}

fn aliased_theme_resource_path(path: &str, original_theme: &str) -> Option<String> {
    let alias = theme_slot_alias(original_theme)?;
    let normalized = path.replace('\\', "/");
    let source_prefix = format!("theme/{original_theme}");
    let folded = fold_path(&normalized);
    let folded_prefix = fold_path(&source_prefix);
    let folded_suffix = folded
        .strip_prefix(&folded_prefix)
        .filter(|suffix| suffix.is_empty() || suffix.starts_with('/'))?;
    let suffix = normalized
        .get(normalized.len() - folded_suffix.len()..)
        .expect("ASCII theme prefix preserves the suffix byte length");
    Some(format!("theme/{}{suffix}", alias.native_code))
}

fn themed_remainder(path: &str, prefix: &str, theme: &str) -> bool {
    path.strip_prefix(prefix)
        .is_some_and(|rest| starts_with_component(rest, theme))
}

fn starts_with_component(value: &str, prefix: &str) -> bool {
    let Some(rest) = value.strip_prefix(prefix) else {
        return false;
    };
    rest.is_empty() || rest.starts_with(['/', '_', '.', '-'])
}

fn path_in_prefix(path: &str, prefix: &str) -> bool {
    let path = fold_path(path);
    let prefix = fold_path(prefix);
    path == prefix
        || path
            .strip_prefix(&prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn add_pending(
    target: &AssetIndex,
    target_data_raw: Option<&Path>,
    pending: &mut BTreeMap<String, PendingFile>,
    source: &AssetRecord,
    target_path: &str,
    role: &'static str,
) -> bool {
    if target_has_path(target, target_data_raw, target_path) {
        return false;
    }
    let key = fold_path(target_path);
    if let std::collections::btree_map::Entry::Vacant(entry) = pending.entry(key) {
        entry.insert(PendingFile {
            target_path: target_path.to_owned(),
            source: source.clone(),
            role,
        });
        true
    } else {
        false
    }
}

fn target_has_path(target: &AssetIndex, target_data_raw: Option<&Path>, path: &str) -> bool {
    if let Some(root) = target_data_raw {
        return safe_relative_path(path)
            .ok()
            .is_some_and(|relative| root.join(relative).is_file());
    }
    target.effective(path).is_some()
}

fn target_bytes(
    target: &AssetIndex,
    target_data_raw: Option<&Path>,
    path: &str,
) -> Result<Option<Vec<u8>>> {
    if let Some(root) = target_data_raw {
        let candidate = root.join(safe_relative_path(path)?);
        if candidate.is_file() {
            return fs::read(&candidate)
                .with_context(|| format!("failed to read {}", candidate.display()))
                .map(Some);
        }
        return Ok(None);
    }
    target
        .effective(path)
        .map_or(Ok(None), |record| target.extract(record).map(Some))
}

fn dependency_status(
    source: &AssetIndex,
    target: &AssetIndex,
    target_data_raw: Option<&Path>,
    source_record: &AssetRecord,
    path: &str,
) -> Result<(&'static str, String)> {
    let Some(existing) = target_bytes(target, target_data_raw, path)? else {
        return Ok((
            "staged",
            "missing from P5136; included in the verified resource bundle".to_owned(),
        ));
    };
    let incoming = source.extract(source_record)?;
    if existing == incoming {
        Ok((
            "target_identical",
            "already present in P5136 with identical bytes".to_owned(),
        ))
    } else {
        Ok((
            "target_conflict_preserved",
            format!(
                "same path differs (source sha256={:x}, target sha256={:x}); preserved the P5136 file",
                Sha256::digest(&incoming),
                Sha256::digest(&existing)
            ),
        ))
    }
}

fn materialize_archive(
    target_data: &Path,
    archive_name: &str,
    excluded_paths: &[String],
    replacements: &[(String, Vec<u8>)],
) -> Result<MaterializedArchive> {
    ensure!(
        target_data.join(archive_name).is_file(),
        "catalog slot {archive_name} is not a stock client archive; P5136 may not enumerate new names"
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
                        flags: P5136_PACKED_ENTRY_FLAGS,
                    },
                )
                .is_none(),
            "catalog base archive contains a duplicate path"
        );
    }
    let preserved_entries = entries.len();
    for (path, data) in replacements {
        entries.insert(
            fold_path(path),
            Rho5WriteEntry {
                path: path.clone(),
                data: data.clone(),
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
) -> Result<ArchiveReport> {
    ensure!(!entries.is_empty(), "cannot write an empty {role} archive");
    let mut writer = Rho5Writer::new();
    for entry in entries {
        writer.add(entry);
    }
    let encoded = writer.encode(name, Rho5Region::Korea, limits)?;
    let bytes = encoded.as_bytes();
    fs::write(output.join(name), bytes)?;
    Ok(ArchiveReport {
        name: name.to_owned(),
        role: role.to_owned(),
        bytes: bytes.len(),
        sha256: format!("{:x}", Sha256::digest(bytes)),
        entries: encoded.entry_count(),
    })
}

fn partition_entries(entries: Vec<Rho5WriteEntry>) -> Vec<Vec<Rho5WriteEntry>> {
    let mut chunks = Vec::new();
    let mut current = Vec::new();
    let mut current_bytes = 0_usize;
    for entry in entries {
        if !current.is_empty()
            && current_bytes.saturating_add(entry.data.len()) > MAX_CHUNK_PLAINTEXT_BYTES
        {
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

fn validate_empty_stock_slots(target_data: &Path, archive_names: &[String]) -> Result<()> {
    let directory = Rho5Directory::scan_kr(target_data, Rho5Limits::default())?;
    for archive_name in archive_names {
        let path = target_data.join(archive_name);
        ensure!(
            path.is_file(),
            "resource slot {} is not a stock client archive; P5136 may not enumerate new names",
            path.display()
        );
        let entries = directory
            .entries()
            .iter()
            .filter(|entry| entry.archive_name().eq_ignore_ascii_case(archive_name))
            .count();
        ensure!(
            entries == 0,
            "resource slot {archive_name} is not empty ({entries} entries); refusing to overwrite it"
        );
    }
    Ok(())
}

fn output_limits() -> Rho5Limits {
    Rho5Limits {
        max_archive_bytes: MAX_OUTPUT_ARCHIVE_BYTES,
        ..Rho5Limits::default()
    }
}

fn verify_staged(output: &Path, report: &TrackBundleReport) -> Result<()> {
    let directory = Rho5Directory::scan_kr(output, Rho5Limits::default())?;
    ensure!(
        directory.archive_count() == report.archives.len(),
        "staging directory contains extra RHO5 files"
    );
    let catalog = directory.extract_exact(SOURCE_TRACK_TABLE)?;
    let locale = directory.extract_exact(TARGET_TRACK_LOCALE)?;
    let catalog = decode_bml(SOURCE_TRACK_TABLE, &catalog)?;
    let locale = decode_bml(TARGET_TRACK_LOCALE, &locale)?;
    for track in &report.tracks {
        ensure!(
            catalog
                .children
                .iter()
                .any(|node| attribute(node, "id") == Some(track.id.as_str())),
            "verified catalog is missing {}",
            track.id
        );
        ensure!(
            locale
                .children
                .iter()
                .any(|node| attribute(node, "id") == Some(track.id.as_str())),
            "verified locale is missing {}",
            track.id
        );
    }
    for theme in &report.themes {
        for path in &theme.selector_catalogs {
            let config = directory.extract_exact(path)?;
            let config = decode_bml(path, &config)?;
            ensure!(
                selector_has_theme(&config, &theme.id),
                "verified selector catalog {path} is missing theme {}",
                theme.id
            );
        }
    }
    if report
        .themes
        .iter()
        .any(|theme| theme_slot_alias(&theme.id).is_some())
    {
        let string_bag = directory.extract_exact(SELECT_TRACK_STRING_BAG)?;
        let string_bag = decode_bml(SELECT_TRACK_STRING_BAG, &string_bag)?;
        for theme in &report.themes {
            if let Some(alias) = theme_slot_alias(&theme.id) {
                ensure!(
                    string_bag_has_key(&string_bag, alias.string_key),
                    "verified selector string bag is missing {}",
                    alias.string_key
                );
            }
        }
    }
    for path in &report.content_registry_catalogs {
        let content_config = directory.extract_exact(path)?;
        for theme in &report.themes {
            if let Some(alias) = theme_slot_alias(&theme.id) {
                ensure!(
                    content_registry_entry_enabled(path, &content_config, alias.content_key)?,
                    "verified content registry {path} does not enable {}",
                    alias.content_key
                );
            }
        }
    }
    Ok(())
}

fn render_markdown(report: &TrackBundleReport) -> String {
    let mut output = String::new();
    writeln!(output, "# P5136 I/R Track Staging Report\n").expect("String write");
    writeln!(output, "- Selection: `{}`", report.selection_rule).expect("String write");
    writeln!(output, "- Tracks: {}", report.tracks.len()).expect("String write");
    writeln!(output, "- Resource entries: {}", report.resource_entries).expect("String write");
    writeln!(
        output,
        "- Plaintext bytes: {}\n",
        report.resource_plaintext_bytes
    )
    .expect("String write");
    writeln!(output, "## Tracks\n").expect("String write");
    writeln!(
        output,
        "| ID | Type | Theme | Repair | Track files | AI paths | Thumb aliases | Material deps | Sound/BGM deps | Embedded refs | Unresolved | Conflicts |"
    )
    .expect("String write");
    writeln!(
        output,
        "|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|"
    )
    .expect("String write");
    for track in &report.tracks {
        writeln!(
            output,
            "| `{}` | {} | `{}` | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            track.id,
            track.game_type,
            track.theme,
            track.catalog_already_installed,
            track.resource_entries,
            track.ai_path_entries,
            track.thumbnail_aliases,
            track.material_dependencies,
            track.sound_dependencies,
            track.embedded_dependencies,
            track.unresolved_dependencies,
            track.conflicting_dependencies
        )
        .expect("String write");
    }
    writeln!(output, "\n## Serialized dependency audit\n").expect("String write");
    writeln!(
        output,
        "| Track | Kind | Symbol | Resolved path | Status | Detail |"
    )
    .expect("String write");
    writeln!(output, "|---|---|---|---|---|---|").expect("String write");
    for dependency in &report.dependencies {
        writeln!(
            output,
            "| `{}` | {} | `{}` | `{}` | {} | {} |",
            dependency.track_id,
            dependency.kind,
            dependency.symbol.replace('|', "\\|"),
            dependency.path.as_deref().unwrap_or("-"),
            dependency.status,
            dependency.detail.replace('|', "\\|")
        )
        .expect("String write");
    }
    writeln!(output, "\n## Themes\n").expect("String write");
    writeln!(
        output,
        "| Theme | New | Selector catalogs | Staged source-only | Preserved P5136 paths |"
    )
    .expect("String write");
    writeln!(output, "|---|---:|---|---:|---:|").expect("String write");
    for theme in &report.themes {
        writeln!(
            output,
            "| `{}` | {} | `{}` | {} | {} |",
            theme.id,
            theme.new_to_p5136,
            theme.selector_catalogs.join(", "),
            theme.staged_source_only_entries,
            theme.preserved_existing_entries
        )
        .expect("String write");
    }
    writeln!(output, "\n## Archives\n").expect("String write");
    for archive in &report.archives {
        writeln!(
            output,
            "- `{}` — {} entries, {} bytes ({})",
            archive.name, archive.entries, archive.bytes, archive.role
        )
        .expect("String write");
    }
    writeln!(output, "\n## Warnings\n").expect("String write");
    for warning in &report.warnings {
        writeln!(output, "- {warning}").expect("String write");
    }
    output
}

fn remove_previous_archives(output: &Path, report_path: &Path) -> Result<()> {
    if !report_path.is_file() {
        return Ok(());
    }
    let previous: PreviousReport = serde_json::from_slice(&fs::read(report_path)?)?;
    for archive in previous.archives {
        validate_archive_name(&archive.name)?;
        remove_if_file(&output.join(archive.name))?;
    }
    Ok(())
}

fn remove_if_file(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(
                metadata.file_type().is_file(),
                "refusing to remove non-file {}",
                path.display()
            );
            fs::remove_file(path)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn validate_archive_name(value: &str) -> Result<()> {
    let path = Path::new(value);
    ensure!(
        path.components().count() == 1
            && path.file_name().and_then(|name| name.to_str()) == Some(value)
            && value.to_ascii_lowercase().ends_with(".rho5"),
        "invalid RHO5 archive file name {value:?}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        aliased_theme_resource_path, content_registry_entry_enabled, embedded_asset_references,
        enable_content_registry_entry, ensure_theme_string, is_ordinary_track_id, mapped_fraction,
        merge_theme_tab_order, normalize_definition_for_p5136, normalize_locale_for_p5136,
        selector_has_theme, sound_candidate_paths, starts_with_component, string_bag_has_key,
        track_material_symbols, track_sound_keys, track_theme,
    };
    use p5136_core::bml::BmlNode;
    use std::collections::BTreeMap;

    fn selector_config(themes: &[&str]) -> BmlNode {
        let mut root = BmlNode::new("config", "");
        let mut order = BmlNode::new("themeTabOrder", "");
        for theme in themes {
            let mut row = BmlNode::new("theme", "");
            row.attributes.push(("id".to_owned(), (*theme).to_owned()));
            order.children.push(row);
        }
        root.children.push(order);
        root
    }

    #[test]
    fn accepts_only_normal_i_r_tracks_and_kd_variants() {
        for id in ["forest_I10", "wkc_R12", "northeu_I10_kd", "pirate_R06_KD"] {
            assert!(is_ordinary_track_id(id), "{id}");
        }
        for id in [
            "maple_S01",
            "village_P01",
            "china_R01_sn10",
            "xyy_X01",
            "I01",
            "forest_Rx1",
        ] {
            assert!(!is_ordinary_track_id(id), "{id}");
        }
        assert_eq!(track_theme("northeu_I10_kd"), Some("northeu"));
    }

    #[test]
    fn progress_fraction_is_bounded_and_mapped() {
        assert!((mapped_fraction(0.2, 0.6, 0, 4) - 0.2).abs() < f32::EPSILON);
        assert!((mapped_fraction(0.2, 0.6, 2, 4) - 0.4).abs() < f32::EPSILON);
        assert!((mapped_fraction(0.2, 0.6, 99, 4) - 0.6).abs() < f32::EPSILON);
        assert!((mapped_fraction(0.2, 0.6, 0, 0) - 0.6).abs() < f32::EPSILON);
    }

    #[test]
    fn registers_and_reenables_the_dormant_xyy_content_slot() {
        let source = "<?xml version='1.0' encoding='UTF-16'?>\r\n<contentList>\r\n\t<content name='themeMechanic' enable='true' visible='true'/>\r\n</contentList>";
        let mut utf16 = vec![0xFF, 0xFE];
        utf16.extend(source.encode_utf16().flat_map(u16::to_le_bytes));
        let inserted = enable_content_registry_entry(
            "content/config.xml",
            &utf16,
            "themeXyy",
            "themeMechanic",
        )
        .unwrap();
        assert!(
            content_registry_entry_enabled("content/config.xml", &inserted, "themeXyy").unwrap()
        );

        let disabled = "<contentList><content name=\"themeXyy\" enable=\"false\" visible=\"false\"/></contentList>";
        let enabled = enable_content_registry_entry(
            "content/config.xml",
            disabled.as_bytes(),
            "themeXyy",
            "themeMechanic",
        )
        .unwrap();
        assert!(
            content_registry_entry_enabled("content/config.xml", &enabled, "themeXyy").unwrap()
        );
    }

    #[test]
    fn maps_fengshen_to_the_reviewed_numeric_xyy_selector_slot() {
        let source = selector_config(&["ice", "mine", "fengshen", "china", "forest"]);
        let mut target = selector_config(&["china", "mine", "ice", "forest", "xyy"]);
        let themes = BTreeMap::from([("fengshen".to_owned(), "fengshen".to_owned())]);

        let registered = merge_theme_tab_order(Some(&source), &mut target, &themes).unwrap();

        assert_eq!(registered.into_iter().collect::<Vec<_>>(), ["fengshen"]);
        assert!(selector_has_theme(&target, "fengshen"));
        let order = &target.children[0];
        let ids = order
            .children
            .iter()
            .filter_map(|node| super::attribute(node, "id"))
            .collect::<Vec<_>>();
        assert_eq!(ids, ["17", "china", "mine", "ice", "forest"]);
        let row = &order.children[0];
        assert_eq!(super::attribute(row, "stringKey"), Some("themeFengshen"));
        assert_eq!(super::attribute(row, "icon"), Some("fengshen"));
    }

    #[test]
    fn mirrors_aliased_theme_resources_into_the_native_runtime_namespace() {
        assert_eq!(
            aliased_theme_resource_path("theme/fengshen/texture/by_road_A01.png", "fengshen")
                .as_deref(),
            Some("theme/xyy/texture/by_road_A01.png")
        );
        assert_eq!(
            aliased_theme_resource_path("THEME/FENGSHEN/texture/by_tree_B01.dds", "Fengshen")
                .as_deref(),
            Some("theme/xyy/texture/by_tree_B01.dds")
        );
        assert_eq!(
            aliased_theme_resource_path("theme/fengshen2/texture/tree.dds", "fengshen"),
            None
        );
        assert_eq!(
            aliased_theme_resource_path("theme/forest/texture/tree.dds", "forest"),
            None
        );
    }

    #[test]
    fn adds_localized_fengshen_selector_text() {
        let mut root = BmlNode::new("StringBag", "");
        ensure_theme_string(&mut root, "themeFengshen", "fengshen");
        assert!(string_bag_has_key(&root, "themeFengshen"));
        let entry = &root.children[0];
        assert!(entry.children.iter().any(|node| {
            super::attribute(node, "c") == Some("kr") && super::attribute(node, "v") == Some("봉신")
        }));
        assert!(entry.children.iter().any(|node| {
            super::attribute(node, "c") == Some("us")
                && super::attribute(node, "v") == Some("Fengshen")
        }));
        assert!(entry.children.iter().any(|node| {
            super::attribute(node, "c") == Some("cn") && super::attribute(node, "v") == Some("封神")
        }));
    }

    #[test]
    fn synthesizes_a_simple_theme_row_when_the_source_has_no_selector_catalog() {
        let mut target = selector_config(&["forest"]);
        let themes = BTreeMap::from([("custom".to_owned(), "custom".to_owned())]);

        merge_theme_tab_order(None, &mut target, &themes).unwrap();

        assert!(selector_has_theme(&target, "custom"));
    }

    #[test]
    fn theme_prefix_requires_a_component_boundary() {
        assert!(starts_with_component("fengshen_I01.ogg", "fengshen"));
        assert!(starts_with_component("fengshen/menu.ogg", "fengshen"));
        assert!(!starts_with_component("fengshen2/menu.ogg", "fengshen"));
    }

    #[test]
    fn extracts_bounded_aa27_utf16_material_symbols() {
        let mut bytes = vec![0x01, 0xAA, 0x27, 0x35, 0x12];
        let symbol = "elf_tree00_t";
        bytes.extend_from_slice(&u32::try_from(symbol.len()).unwrap().to_le_bytes());
        for unit in symbol.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes.extend_from_slice(&[0xAA, 0x27, 0x36, 0x12]);
        bytes.extend_from_slice(&8_u32.to_le_bytes());
        for unit in "property".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }

        assert_eq!(
            track_material_symbols(&bytes)
                .into_iter()
                .collect::<Vec<_>>(),
            ["elf_tree00_t"]
        );
    }

    #[test]
    fn extracts_serialized_environment_sound_names() {
        let mut bytes = vec![0x05, 0, 0, 0, 0x08, 0, 0, 0];
        for unit in "filename".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let sound = "동굴속앰비언스2";
        bytes.extend_from_slice(
            &u32::try_from(sound.encode_utf16().count())
                .unwrap()
                .to_le_bytes(),
        );
        for unit in sound.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(
            track_sound_keys(&bytes).into_iter().collect::<Vec<_>>(),
            [sound]
        );
        assert_eq!(
            sound_candidate_paths(sound),
            [
                format!("sound/fx/surround/{sound}.ogg"),
                format!("sound/fx/surround/{sound}.wav"),
            ]
        );
    }

    #[test]
    fn extracts_bounded_ascii_and_utf16_asset_references() {
        let mut bytes = b"prefix theme\\forest\\tree.dds suffix\0".to_vec();
        let reference = "objects/bridge.1s";
        bytes.extend_from_slice(
            &u32::try_from(reference.encode_utf16().count())
                .unwrap()
                .to_le_bytes(),
        );
        for unit in reference.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes.extend_from_slice(b"ignored.exe\0");
        assert_eq!(
            embedded_asset_references(&bytes)
                .into_iter()
                .collect::<Vec<_>>(),
            ["objects/bridge.1s", "theme/forest/tree.dds"]
        );
    }

    #[test]
    fn normalizes_newer_track_rows_to_the_p5136_schema() {
        let mut definition = BmlNode::new("track", "");
        definition.attributes = vec![
            ("id".to_owned(), "forest_I10".to_owned()),
            ("gameType".to_owned(), "item".to_owned()),
            ("laps".to_owned(), "3".to_owned()),
            ("difficulty".to_owned(), "3".to_owned()),
            ("length".to_owned(), "50".to_owned()),
        ];
        let definition = normalize_definition_for_p5136(&definition, "forest");
        assert_eq!(
            definition
                .attributes
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["id", "gameType", "laps", "level", "difficulty", "length"]
        );
        assert_eq!(definition.attributes[3].1, "3");

        let fengshen = normalize_definition_for_p5136(&definition, "fengshen");
        assert_eq!(super::attribute(&fengshen, "theme"), Some("xyy"));
        assert_eq!(super::attribute(&fengshen, "texTheme"), Some("xyy"));
        assert_eq!(super::attribute(&fengshen, "bgmTheme"), Some("fengshen"));

        let mut cross_theme = definition.clone();
        cross_theme
            .attributes
            .push(("texTheme".to_owned(), "abyss|sword".to_owned()));
        let cross_theme = normalize_definition_for_p5136(&cross_theme, "fengshen");
        assert_eq!(
            super::attribute(&cross_theme, "texTheme"),
            Some("xyy|abyss|sword")
        );

        let mut source_locale = BmlNode::new("track", "");
        source_locale.attributes = vec![
            ("id".to_owned(), "forest_I10".to_owned()),
            ("name".to_owned(), "newer localized name".to_owned()),
            ("gameType".to_owned(), "item".to_owned()),
            ("laps".to_owned(), "3".to_owned()),
            ("difficulty".to_owned(), "3".to_owned()),
            ("length".to_owned(), "50".to_owned()),
        ];
        let locale = normalize_locale_for_p5136(&source_locale, "forest_I10");
        assert_eq!(
            locale.attributes,
            vec![
                ("id".to_owned(), "forest_I10".to_owned()),
                ("name".to_owned(), "forest_I10".to_owned()),
            ]
        );
    }
}
