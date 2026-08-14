use std::{
    collections::HashMap,
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand, ValueEnum};
use p5136_rho5::{
    AaaDocument, AaaLimits, AaaRhoFolder, LegacyRhoArchive, LegacyRhoFileProperty, LegacyRhoLimits,
    LegacyRhoWriteEntry, LegacyRhoWriter, Rho5Directory, Rho5Limits,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod asset_index;
mod bundle;
mod planner;
mod track_bundle;

const MANIFEST_SCHEMA: u32 = 1;
const COMPATIBILITY_ASSERTION: &str = "p5136-static-verified-v1";
const EXPERIMENTAL_NATIVE_ASSERTION: &str = "p5136-xun-sidecar-experimental-v1";

#[derive(Debug, Parser)]
#[command(
    name = "p5136-assets",
    version,
    about = "Plan and stage compatible P5136 legacy RHO or RHO5 asset overlays"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List the audited asset candidates exposed by the integrated importer.
    ListCompatibleAssets {
        #[arg(long)]
        source_data: PathBuf,
        #[arg(long)]
        target_data_raw: Option<PathBuf>,
        #[arg(long)]
        cache: PathBuf,
    },
    /// Resolve, verify, and install selected audited assets into a complete P5136 `DataRaw` tree.
    ImportAssetsDataRaw {
        #[arg(long)]
        source_data: PathBuf,
        #[arg(long)]
        target_data: PathBuf,
        #[arg(long)]
        target_data_raw: PathBuf,
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        backup: PathBuf,
        /// Asset selectors such as `kart:mancarXUN`.
        #[arg(long = "asset", value_delimiter = ',')]
        assets: Vec<String>,
    },
    /// Build dependency closures and guarded import manifests without changing live Data.
    Plan {
        #[arg(long)]
        source_data: PathBuf,
        #[arg(long)]
        target_data: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value = "all")]
        category: String,
        #[arg(long)]
        asset: Option<String>,
        #[arg(long)]
        include_existing: bool,
        #[arg(long, default_value_t = 100)]
        max_assets: usize,
        #[arg(long, default_value_t = 512 * 1024 * 1024)]
        max_asset_bytes: usize,
    },
    /// Index the newer Chinese client's RHO5 archives without extracting them.
    ScanCn {
        #[arg(long)]
        source_data: PathBuf,
        #[arg(long)]
        prefix: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Extract and hash one Chinese RHO5 entry for a pinned import manifest.
    HashCn {
        #[arg(long)]
        source_data: PathBuf,
        #[arg(long)]
        path: String,
    },
    /// List effective client-visible paths after legacy mounts and RHO5 overlays.
    ListEffective {
        #[arg(long)]
        data: PathBuf,
        #[arg(long, value_enum)]
        region: RegionArg,
        #[arg(long)]
        contains: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
        #[arg(long)]
        cache: PathBuf,
        #[arg(long)]
        show_origin: bool,
    },
    /// Extract one effective client-visible path after applying overlay precedence.
    ExtractEffective {
        #[arg(long)]
        data: PathBuf,
        #[arg(long, value_enum)]
        region: RegionArg,
        #[arg(long)]
        path: String,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        cache: PathBuf,
    },
    /// Extract every effective client-visible file into a `DataRaw` tree.
    ExtractAllEffective {
        #[arg(long)]
        data: PathBuf,
        #[arg(long, value_enum)]
        region: RegionArg,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        cache: PathBuf,
        /// Keep files already present in the output tree, allowing local overrides.
        #[arg(long)]
        preserve_existing: bool,
    },
    /// List paths stored directly in one legacy RHO archive.
    ListLegacy {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        contains: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Extract one exact path directly from a legacy RHO archive.
    ExtractLegacy {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        path: String,
        #[arg(long)]
        output: PathBuf,
    },
    /// Build a new legacy RHO and updated aaa.pk in an isolated staging directory.
    Import {
        #[arg(long)]
        source_data: PathBuf,
        #[arg(long)]
        target_data: PathBuf,
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        force: bool,
    },
    /// Stage compatible karts, characters, pets, and flying pets as bounded Korean RHO5 overlays.
    StageCompatible {
        #[arg(long)]
        source_data: PathBuf,
        #[arg(long)]
        target_data: PathBuf,
        #[arg(long)]
        report: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, value_delimiter = ',', default_value = "kart,character")]
        categories: Vec<String>,
        #[arg(long, default_value = "DataPack1_00002.rho5")]
        archive_name: String,
        #[arg(long, default_value = "DataPack1_00000.rho5")]
        table_archive_name: String,
        #[arg(long, default_value = "DataPack4_00002.rho5")]
        catalog_archive_name: String,
        #[arg(long)]
        force: bool,
    },
    /// Stage source-only I/R tracks and their non-destructive theme dependencies.
    StageTracks {
        #[arg(long)]
        source_data: PathBuf,
        #[arg(long, value_enum, default_value_t = RegionArg::Cn)]
        source_region: RegionArg,
        #[arg(long)]
        target_data: PathBuf,
        #[arg(long)]
        output: PathBuf,
        /// Comma-separated track IDs. Omit to stage every eligible source-only I/R track.
        #[arg(long = "track", value_delimiter = ',')]
        tracks: Vec<String>,
        /// Existing empty P5136 RHO5 slots, used in order and never synthesized past stock ranges.
        #[arg(
            long = "archive-name",
            value_delimiter = ',',
            default_value = "DataPack1_00007.rho5,DataPack1_00008.rho5,DataPack1_00009.rho5,DataPack1_00010.rho5,DataPack1_00011.rho5,DataPack1_00012.rho5,DataPack3_00001.rho5,DataPack3_00004.rho5,DataPack3_00005.rho5"
        )]
        archive_names: Vec<String>,
        #[arg(long, default_value = "DataPack1_00013.rho5")]
        catalog_archive_name: String,
        #[arg(long)]
        force: bool,
    },
    /// Discover selectable I/R tracks in any supported external client Data directory.
    DiscoverTracks {
        #[arg(long)]
        source_data: PathBuf,
        #[arg(long, value_enum, default_value_t = RegionArg::Cn)]
        source_region: RegionArg,
        #[arg(long)]
        target_data_raw: Option<PathBuf>,
        #[arg(long)]
        cache: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Resolve, stage, verify, and install selected tracks into a complete P5136 `DataRaw` tree.
    ImportTracksDataRaw {
        #[arg(long)]
        source_data: PathBuf,
        #[arg(long, value_enum, default_value_t = RegionArg::Cn)]
        source_region: RegionArg,
        #[arg(long)]
        target_data: PathBuf,
        #[arg(long)]
        target_data_raw: PathBuf,
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        backup: PathBuf,
        #[arg(long = "track", value_delimiter = ',')]
        tracks: Vec<String>,
    },
    /// Install a verified staged track bundle into a complete `DataRaw` tree.
    InstallTrackBundleDataRaw {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        data_raw: PathBuf,
        /// One-time backup root for files replaced by the bundle.
        #[arg(long)]
        backup: PathBuf,
    },
    /// Semantically repackage one legacy RHO to exercise the writer.
    RepackRho {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Decode and re-encode aaa.pk while preserving its complete node tree.
    RepackAaa {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum RegionArg {
    Kr,
    Cn,
}

impl From<RegionArg> for asset_index::AssetRegion {
    fn from(value: RegionArg) -> Self {
        match value {
            RegionArg::Kr => Self::Korea,
            RegionArg::Cn => Self::China,
        }
    }
}

impl From<RegionArg> for track_bundle::TrackSourceRegion {
    fn from(value: RegionArg) -> Self {
        match value {
            RegionArg::Kr => Self::Korea,
            RegionArg::Cn => Self::China,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ImportManifest {
    schema_version: u32,
    compatibility: String,
    output_archive: String,
    rho_folder_name: String,
    pack_path: Vec<String>,
    entries: Vec<ManifestEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ManifestEntry {
    source: ManifestSource,
    target_path: String,
    #[serde(default)]
    property: Option<ManifestProperty>,
    expected_sha256: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ManifestSource {
    Rho5Cn { path: String },
    Rho5Kr { path: String },
    Legacy { archive: String, path: String },
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum ManifestProperty {
    None,
    Compressed,
    Encrypted,
    PartialEncrypted,
    CompressedEncrypted,
}

impl From<ManifestProperty> for LegacyRhoFileProperty {
    fn from(value: ManifestProperty) -> Self {
        match value {
            ManifestProperty::None => Self::None,
            ManifestProperty::Compressed => Self::Compressed,
            ManifestProperty::Encrypted => Self::Encrypted,
            ManifestProperty::PartialEncrypted => Self::PartialEncrypted,
            ManifestProperty::CompressedEncrypted => Self::CompressedEncrypted,
        }
    }
}

#[allow(clippy::too_many_lines)]
fn main() -> Result<()> {
    match Cli::parse().command {
        Command::ListCompatibleAssets {
            source_data,
            target_data_raw,
            cache,
        } => {
            let candidates = p5136_assets::discover_asset_candidates(
                &source_data,
                target_data_raw.as_deref(),
                &cache,
            )?;
            for candidate in &candidates {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    candidate.category.label(),
                    candidate.id,
                    if candidate.eligible {
                        "eligible"
                    } else {
                        "blocked"
                    },
                    if candidate.already_installed {
                        "installed"
                    } else {
                        "not-installed"
                    },
                    candidate.reason.as_deref().unwrap_or("")
                );
            }
            let eligible = candidates
                .iter()
                .filter(|candidate| candidate.eligible)
                .count();
            eprintln!(
                "candidates={} eligible={} blocked={}",
                candidates.len(),
                eligible,
                candidates.len() - eligible
            );
            Ok(())
        }
        Command::ImportAssetsDataRaw {
            source_data,
            target_data,
            target_data_raw,
            workspace,
            backup,
            assets,
        } => {
            let assets = assets
                .iter()
                .map(|value| parse_asset_selection(value))
                .collect::<Result<Vec<_>>>()?;
            let summary =
                p5136_assets::import_assets_to_dataraw(&p5136_assets::AssetImportOptions {
                    source_data,
                    target_data,
                    target_data_raw,
                    workspace,
                    backup,
                    assets,
                })?;
            println!(
                "imported assets={} karts={} characters={} pets={} flying_pets={} resources_written={} resources_identical={} catalogs_updated={} report={}",
                summary.assets,
                summary.karts,
                summary.characters,
                summary.pets,
                summary.flying_pets,
                summary.resources_written,
                summary.resources_identical,
                summary.catalogs_updated,
                summary.report.display()
            );
            Ok(())
        }
        Command::Plan {
            source_data,
            target_data,
            output,
            category,
            asset,
            include_existing,
            max_assets,
            max_asset_bytes,
        } => {
            let report = planner::run_plan(
                &source_data,
                &target_data,
                &output,
                &planner::PlanOptions {
                    category,
                    asset,
                    asset_selectors: std::collections::BTreeSet::new(),
                    experimental_native_selectors: std::collections::BTreeSet::new(),
                    include_existing,
                    max_assets,
                    max_asset_bytes,
                },
            )?;
            println!("wrote compatibility plan {}", report.display());
            Ok(())
        }
        Command::ScanCn {
            source_data,
            prefix,
            limit,
        } => scan_cn(&source_data, prefix.as_deref(), limit),
        Command::HashCn { source_data, path } => hash_cn(&source_data, &path),
        Command::ListEffective {
            data,
            region,
            contains,
            limit,
            cache,
            show_origin,
        } => list_effective(
            &data,
            region.into(),
            contains.as_deref(),
            limit,
            &cache,
            show_origin,
        ),
        Command::ExtractEffective {
            data,
            region,
            path,
            output,
            cache,
        } => extract_effective(&data, region.into(), &path, &output, &cache),
        Command::ExtractAllEffective {
            data,
            region,
            output,
            cache,
            preserve_existing,
        } => extract_all_effective(&data, region.into(), &output, &cache, preserve_existing),
        Command::ListLegacy {
            input,
            contains,
            limit,
        } => list_legacy(&input, contains.as_deref(), limit),
        Command::ExtractLegacy {
            input,
            path,
            output,
        } => extract_legacy(&input, &path, &output),
        Command::Import {
            source_data,
            target_data,
            manifest,
            output,
            force,
        } => import(&source_data, &target_data, &manifest, &output, force),
        Command::StageCompatible {
            source_data,
            target_data,
            report,
            output,
            categories,
            archive_name,
            table_archive_name,
            catalog_archive_name,
            force,
        } => bundle::stage_compatible(
            &source_data,
            &target_data,
            &report,
            &output,
            &categories,
            &archive_name,
            &table_archive_name,
            &catalog_archive_name,
            force,
        ),
        Command::StageTracks {
            source_data,
            source_region,
            target_data,
            output,
            tracks,
            archive_names,
            catalog_archive_name,
            force,
        } => track_bundle::stage_tracks(
            &source_data,
            source_region.into(),
            &target_data,
            &output,
            &tracks,
            &archive_names,
            &catalog_archive_name,
            force,
        ),
        Command::DiscoverTracks {
            source_data,
            source_region,
            target_data_raw,
            cache,
            json,
        } => {
            let candidates = track_bundle::discover_track_candidates(
                &source_data,
                source_region.into(),
                target_data_raw.as_deref(),
                &cache,
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&candidates)?);
            } else {
                for candidate in &candidates {
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{}",
                        candidate.id,
                        candidate.name,
                        candidate.game_type,
                        candidate.theme,
                        candidate.eligible,
                        candidate.reason.as_deref().unwrap_or("")
                    );
                }
                println!(
                    "tracks={} eligible={}",
                    candidates.len(),
                    candidates
                        .iter()
                        .filter(|candidate| candidate.eligible)
                        .count()
                );
            }
            Ok(())
        }
        Command::ImportTracksDataRaw {
            source_data,
            source_region,
            target_data,
            target_data_raw,
            workspace,
            backup,
            tracks,
        } => {
            let summary =
                track_bundle::import_tracks_to_dataraw(&track_bundle::TrackImportOptions {
                    source_data,
                    source_region: source_region.into(),
                    target_data,
                    target_data_raw,
                    workspace,
                    backup,
                    tracks,
                })?;
            println!(
                "imported tracks={} resources={} dependencies={} warnings={} report={}",
                summary.tracks,
                summary.resources,
                summary.dependencies,
                summary.warnings.len(),
                summary.report.display()
            );
            Ok(())
        }
        Command::InstallTrackBundleDataRaw {
            bundle,
            data_raw,
            backup,
        } => track_bundle::install_tracks_dataraw(&bundle, &data_raw, &backup),
        Command::RepackRho { input, output } => repack_rho(&input, &output),
        Command::RepackAaa { input, output } => repack_aaa(&input, &output),
    }
}

fn parse_asset_selection(value: &str) -> Result<p5136_assets::AssetSelection> {
    let (category, id) = value
        .split_once(':')
        .with_context(|| format!("asset selector must be category:id: {value}"))?;
    ensure!(!id.trim().is_empty(), "asset selector has an empty ID");
    let category = match category.to_ascii_lowercase().as_str() {
        "kart" => p5136_assets::AssetCategory::Kart,
        "character" => p5136_assets::AssetCategory::Character,
        "pet" => p5136_assets::AssetCategory::Pet,
        "flying_pet" | "flying-pet" => p5136_assets::AssetCategory::FlyingPet,
        _ => bail!("unsupported asset category in selector: {value}"),
    };
    Ok(p5136_assets::AssetSelection {
        category,
        id: id.trim().to_owned(),
    })
}

fn list_legacy(input: &Path, contains: Option<&str>, limit: usize) -> Result<()> {
    let archive = LegacyRhoArchive::open(input, large_legacy_limits())
        .with_context(|| format!("failed to open {}", input.display()))?;
    let needle = contains.map(str::to_ascii_lowercase);
    let mut paths = archive
        .entries()?
        .into_iter()
        .filter(|entry| {
            needle.as_ref().is_none_or(|needle| {
                entry
                    .normalized_path()
                    .to_ascii_lowercase()
                    .contains(needle)
            })
        })
        .map(|entry| entry.normalized_path().to_owned())
        .collect::<Vec<_>>();
    paths.sort_unstable_by_key(|path| path.to_ascii_lowercase());
    for path in paths.iter().take(limit) {
        println!("{path}");
    }
    println!("matched={} shown={}", paths.len(), paths.len().min(limit));
    Ok(())
}

fn extract_legacy(input: &Path, path: &str, output: &Path) -> Result<()> {
    validate_archive_path(path)?;
    let archive = LegacyRhoArchive::open(input, large_legacy_limits())
        .with_context(|| format!("failed to open {}", input.display()))?;
    let bytes = archive
        .extract_exact(path)
        .with_context(|| format!("failed to extract {path:?} from {}", input.display()))?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(output, &bytes).with_context(|| format!("failed to write {}", output.display()))?;
    println!(
        "extracted path={path} bytes={} sha256={:x} output={}",
        bytes.len(),
        Sha256::digest(&bytes),
        output.display()
    );
    Ok(())
}

fn list_effective(
    data: &Path,
    region: asset_index::AssetRegion,
    contains: Option<&str>,
    limit: usize,
    cache: &Path,
    show_origin: bool,
) -> Result<()> {
    let index = asset_index::AssetIndex::scan(data, region, cache)?;
    let needle = contains.map(str::to_ascii_lowercase);
    let mut records = index
        .effective_records()
        .filter(|record| {
            needle
                .as_ref()
                .is_none_or(|needle| record.virtual_path.to_ascii_lowercase().contains(needle))
        })
        .collect::<Vec<_>>();
    records.sort_unstable_by_key(|record| record.virtual_path.to_ascii_lowercase());
    for record in records.iter().take(limit) {
        if show_origin {
            println!(
                "{}\t{}",
                record.virtual_path,
                serde_json::to_string(&index.origin_report(record))?
            );
        } else {
            println!("{}", record.virtual_path);
        }
    }
    println!(
        "matched={} shown={}",
        records.len(),
        records.len().min(limit)
    );
    Ok(())
}

fn extract_effective(
    data: &Path,
    region: asset_index::AssetRegion,
    path: &str,
    output: &Path,
    cache: &Path,
) -> Result<()> {
    validate_archive_path(path)?;
    let index = asset_index::AssetIndex::scan(data, region, cache)?;
    let record = index
        .effective(path)
        .with_context(|| format!("effective asset path {path:?} was not found"))?;
    let bytes = index.extract(record)?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(output, &bytes).with_context(|| format!("failed to write {}", output.display()))?;
    println!(
        "extracted path={path} bytes={} sha256={:x} output={}",
        bytes.len(),
        Sha256::digest(&bytes),
        output.display()
    );
    Ok(())
}

fn extract_all_effective(
    data: &Path,
    region: asset_index::AssetRegion,
    output: &Path,
    cache: &Path,
    preserve_existing: bool,
) -> Result<()> {
    ensure!(
        preserve_existing || !output.exists(),
        "output already exists; pass --preserve-existing to keep existing overrides"
    );
    let index = asset_index::AssetIndex::scan(data, region, cache)?;
    let mut records = index.effective_records().collect::<Vec<_>>();
    // AssetExtractor retains one decoded legacy archive, so grouping records by
    // their archive prevents thousands of unnecessary archive reopen operations.
    records.sort_unstable_by(|left, right| {
        extraction_order(left)
            .cmp(&extraction_order(right))
            .then_with(|| left.virtual_path.cmp(&right.virtual_path))
    });
    let total = records.len();
    let mut extractor = index.extractor();
    let mut written = 0_usize;
    let mut skipped = 0_usize;
    let mut bytes = 0_u64;
    for (position, record) in records.into_iter().enumerate() {
        validate_archive_path(&record.virtual_path)?;
        let destination = data_raw_destination(output, &record.virtual_path)?;
        if preserve_existing && destination.exists() {
            skipped += 1;
            continue;
        }
        let payload = extractor.extract(record).with_context(|| {
            format!(
                "failed to extract effective asset {:?}",
                record.virtual_path
            )
        })?;
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::write(&destination, &payload)
            .with_context(|| format!("failed to write {}", destination.display()))?;
        written += 1;
        bytes = bytes.saturating_add(payload.len() as u64);
        if (position + 1).is_multiple_of(1_000) || position + 1 == total {
            eprintln!(
                "extracted {}/{} written={} skipped={} bytes={}",
                position + 1,
                total,
                written,
                skipped,
                bytes
            );
        }
    }
    println!(
        "extracted effective DataRaw files={} skipped={} bytes={} output={}",
        written,
        skipped,
        bytes,
        output.display()
    );
    Ok(())
}

fn extraction_order(record: &asset_index::AssetRecord) -> (u8, &str, usize) {
    match &record.origin {
        asset_index::AssetOrigin::Legacy { archive, .. } => (0, archive.as_str(), 0),
        asset_index::AssetOrigin::Rho5 { entry_index } => (1, "", *entry_index),
    }
}

fn data_raw_destination(output: &Path, virtual_path: &str) -> Result<PathBuf> {
    let mut destination = output.to_path_buf();
    for component in virtual_path.replace('\\', "/").split('/') {
        validate_component(component)?;
        destination.push(component);
    }
    Ok(destination)
}

fn hash_cn(source_data: &Path, path: &str) -> Result<()> {
    validate_archive_path(path)?;
    let directory =
        Rho5Directory::scan_cn(source_data, Rho5Limits::default()).with_context(|| {
            format!(
                "failed to index Chinese RHO5 data at {}",
                source_data.display()
            )
        })?;
    let data =
        extract_rho5(&directory, path).with_context(|| format!("failed to extract {path:?}"))?;
    println!("sha256={:x} bytes={}", Sha256::digest(&data), data.len());
    Ok(())
}

fn scan_cn(source_data: &Path, prefix: Option<&str>, limit: usize) -> Result<()> {
    let directory =
        Rho5Directory::scan_cn(source_data, Rho5Limits::default()).with_context(|| {
            format!(
                "failed to index Chinese RHO5 data at {}",
                source_data.display()
            )
        })?;
    println!(
        "archives={} entries={}",
        directory.archive_count(),
        directory.entries().len()
    );
    let mut shown = 0_usize;
    for entry in directory
        .entries()
        .iter()
        .filter(|entry| prefix.is_none_or(|prefix| entry.normalized_path().starts_with(prefix)))
    {
        if shown == limit {
            break;
        }
        println!(
            "{}\t{}\t{}",
            entry.normalized_path(),
            entry.plaintext_size(),
            entry.archive_name()
        );
        shown += 1;
    }
    println!("shown={shown}");
    Ok(())
}

fn import(
    source_data: &Path,
    target_data: &Path,
    manifest_path: &Path,
    output: &Path,
    force: bool,
) -> Result<()> {
    let manifest_bytes = fs::read(manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let manifest: ImportManifest = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
    validate_manifest(&manifest)?;
    ensure_staging_destination(source_data, target_data, output)?;

    let archive_output = output.join(&manifest.output_archive);
    let aaa_output = output.join("aaa.pk");
    if !force {
        ensure!(
            !archive_output.exists() && !aaa_output.exists(),
            "staging output already exists; pass --force to replace only the two generated files"
        );
    }

    let needs_cn = manifest
        .entries
        .iter()
        .any(|entry| matches!(entry.source, ManifestSource::Rho5Cn { .. }));
    let needs_kr = manifest
        .entries
        .iter()
        .any(|entry| matches!(entry.source, ManifestSource::Rho5Kr { .. }));
    let cn = needs_cn
        .then(|| Rho5Directory::scan_cn(source_data, Rho5Limits::default()))
        .transpose()
        .context("failed to index Chinese source RHO5 archives")?;
    let kr = needs_kr
        .then(|| Rho5Directory::scan_kr(source_data, Rho5Limits::default()))
        .transpose()
        .context("failed to index Korean source RHO5 archives")?;
    let legacy_limits = large_legacy_limits();
    let writer = load_manifest_entries(
        &manifest,
        source_data,
        cn.as_ref(),
        kr.as_ref(),
        legacy_limits,
    )?;

    let archive_stem = Path::new(&manifest.output_archive)
        .file_stem()
        .and_then(|value| value.to_str())
        .context("output archive has no Unicode stem")?;
    let encoded = writer
        .encode_with_metadata(archive_stem, legacy_limits)
        .context("failed to encode imported legacy RHO")?;
    let mut aaa = AaaDocument::read(target_data.join("aaa.pk"), AaaLimits::default())
        .context("failed to decode target aaa.pk")?;
    let pack_path = manifest
        .pack_path
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    aaa.upsert_rho_folder(
        &pack_path,
        &AaaRhoFolder {
            name: manifest.rho_folder_name.clone(),
            file_name: manifest.output_archive.clone(),
            key: encoded.rho_key(),
            data_hash: encoded.data_hash(),
            media_size: u64::try_from(encoded.as_bytes().len()).context("archive size overflow")?,
        },
    )?;
    let aaa_bytes = aaa.encode(AaaLimits::default())?;

    fs::create_dir_all(output)
        .with_context(|| format!("failed to create staging directory {}", output.display()))?;
    fs::write(&archive_output, encoded.as_bytes())
        .with_context(|| format!("failed to write {}", archive_output.display()))?;
    fs::write(&aaa_output, aaa_bytes)
        .with_context(|| format!("failed to write {}", aaa_output.display()))?;
    println!(
        "staged entries={} archive={} aaa={}",
        manifest.entries.len(),
        archive_output.display(),
        aaa_output.display()
    );
    Ok(())
}

fn load_manifest_entries(
    manifest: &ImportManifest,
    source_data: &Path,
    cn: Option<&Rho5Directory>,
    kr: Option<&Rho5Directory>,
    legacy_limits: LegacyRhoLimits,
) -> Result<LegacyRhoWriter> {
    let mut legacy = HashMap::<String, LegacyRhoArchive>::new();
    let mut writer = LegacyRhoWriter::new();
    for entry in &manifest.entries {
        let data = match &entry.source {
            ManifestSource::Rho5Cn { path } => {
                extract_rho5(cn.expect("CN index was requested"), path)
                    .with_context(|| format!("failed to extract CN RHO5 entry {path:?}"))?
            }
            ManifestSource::Rho5Kr { path } => {
                extract_rho5(kr.expect("KR index was requested"), path)
                    .with_context(|| format!("failed to extract KR RHO5 entry {path:?}"))?
            }
            ManifestSource::Legacy { archive, path } => {
                validate_file_name(archive, ".rho")?;
                if !legacy.contains_key(archive) {
                    let archive_path = source_data.join(archive);
                    let opened = LegacyRhoArchive::open(&archive_path, legacy_limits)
                        .with_context(|| format!("failed to open {}", archive_path.display()))?;
                    legacy.insert(archive.clone(), opened);
                }
                legacy
                    .get(archive)
                    .expect("legacy archive was just inserted")
                    .extract_exact(path)
                    .with_context(|| format!("failed to extract {archive}:{path}"))?
            }
        };
        verify_sha256(&data, &entry.expected_sha256, &entry.target_path)?;
        let property = entry.property.map_or_else(
            || default_property(&entry.target_path, data.len()),
            Into::into,
        );
        writer.add(LegacyRhoWriteEntry {
            path: entry.target_path.clone(),
            data,
            property,
        });
    }
    Ok(writer)
}

fn extract_rho5(directory: &Rho5Directory, path: &str) -> Result<Vec<u8>> {
    let entry = directory.unique_entry(path)?;
    Ok(directory.extract_entry_with_legacy_padding(entry)?)
}

fn repack_rho(input: &Path, output: &Path) -> Result<()> {
    let limits = large_legacy_limits();
    let archive = LegacyRhoArchive::open(input, limits)
        .with_context(|| format!("failed to open {}", input.display()))?;
    let writer = LegacyRhoWriter::from_archive(&archive)?;
    writer
        .write_to(output, limits)
        .with_context(|| format!("failed to write {}", output.display()))?;
    let check = LegacyRhoArchive::open(output, limits)?;
    let entries = archive.entries()?;
    ensure!(entries == check.entries()?, "repacked metadata differs");
    for entry in &entries {
        ensure!(
            archive.extract_entry(entry)? == check.extract_exact(entry.normalized_path())?,
            "repacked payload differs at {:?}",
            entry.normalized_path()
        );
    }
    println!("repacked {} entries", entries.len());
    Ok(())
}

fn repack_aaa(input: &Path, output: &Path) -> Result<()> {
    let limits = AaaLimits::default();
    let document = AaaDocument::read(input, limits)?;
    document.write_to(output, limits)?;
    let check = AaaDocument::read(output, limits)?;
    ensure!(check == document, "repacked aaa.pk tree differs");
    println!("repacked aaa.pk root={}", check.root.name);
    Ok(())
}

fn validate_manifest(manifest: &ImportManifest) -> Result<()> {
    ensure!(
        manifest.schema_version == MANIFEST_SCHEMA,
        "unsupported schema_version"
    );
    ensure!(
        manifest.compatibility == COMPATIBILITY_ASSERTION,
        "compatibility must explicitly be {COMPATIBILITY_ASSERTION:?}"
    );
    validate_file_name(&manifest.output_archive, ".rho")?;
    ensure!(!manifest.entries.is_empty(), "manifest has no entries");
    for component in &manifest.pack_path {
        validate_component(component)?;
    }
    for entry in &manifest.entries {
        validate_archive_path(&entry.target_path)?;
        ensure!(
            entry.expected_sha256.len() == 64,
            "expected_sha256 must have 64 hex digits"
        );
        match &entry.source {
            ManifestSource::Rho5Cn { path } | ManifestSource::Rho5Kr { path } => {
                validate_archive_path(path)?;
            }
            ManifestSource::Legacy { archive, path } => {
                validate_file_name(archive, ".rho")?;
                validate_archive_path(path)?;
            }
        }
    }
    Ok(())
}

fn validate_file_name(value: &str, extension: &str) -> Result<()> {
    ensure!(
        Path::new(value).file_name().and_then(|name| name.to_str()) == Some(value),
        "archive name must be a plain file name: {value:?}"
    );
    ensure!(
        value.to_ascii_lowercase().ends_with(extension),
        "expected a {extension} name"
    );
    Ok(())
}

fn validate_archive_path(value: &str) -> Result<()> {
    ensure!(!value.is_empty(), "archive path is empty");
    for component in Path::new(&value.replace('\\', "/")).components() {
        match component {
            Component::Normal(value) => validate_component(&value.to_string_lossy())?,
            _ => bail!("archive path must be relative and may not contain dot segments: {value:?}"),
        }
    }
    Ok(())
}

fn validate_component(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty() && value != "." && value != "..",
        "invalid path component"
    );
    ensure!(
        !value.contains(['/', '\\', '\0']),
        "invalid path component {value:?}"
    );
    Ok(())
}

fn ensure_staging_destination(source: &Path, target: &Path, output: &Path) -> Result<()> {
    let source = fs::canonicalize(source)
        .with_context(|| format!("failed to canonicalize {}", source.display()))?;
    let target = fs::canonicalize(target)
        .with_context(|| format!("failed to canonicalize {}", target.display()))?;
    let output_absolute = canonicalize_future_path(output)?;
    ensure!(
        !output_absolute.starts_with(&source),
        "output must not be inside the source Data directory"
    );
    ensure!(
        !output_absolute.starts_with(&target),
        "output must not be inside the live target Data directory"
    );
    Ok(())
}

fn canonicalize_future_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut cursor = absolute.as_path();
    let mut missing = Vec::new();
    while !cursor.exists() {
        let name = cursor
            .file_name()
            .context("staging output has no existing ancestor")?;
        missing.push(name.to_os_string());
        cursor = cursor
            .parent()
            .context("staging output has no existing ancestor")?;
    }
    let mut canonical = fs::canonicalize(cursor)
        .with_context(|| format!("failed to canonicalize {}", cursor.display()))?;
    for component in missing.iter().rev() {
        canonical.push(component);
    }
    Ok(canonical)
}

fn verify_sha256(data: &[u8], expected: &str, path: &str) -> Result<()> {
    ensure!(
        expected.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid SHA-256 hex"
    );
    let actual = format!("{:x}", Sha256::digest(data));
    ensure!(
        actual.eq_ignore_ascii_case(expected),
        "SHA-256 mismatch for {path:?}: {actual}"
    );
    Ok(())
}

fn default_property(path: &str, size: usize) -> LegacyRhoFileProperty {
    let extension = Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "1s" | "dds" | "tga" | "bmh" | "bmx" | "f30" | "hdr" | "fft" | "wav" => {
            LegacyRhoFileProperty::Compressed
        }
        "uset" | "xml" => LegacyRhoFileProperty::Encrypted,
        "png" if size <= 256 => LegacyRhoFileProperty::Encrypted,
        "png" | "kap" | "ogg" | "jpg" | "flac" | "ksv" => LegacyRhoFileProperty::PartialEncrypted,
        "bml" => LegacyRhoFileProperty::CompressedEncrypted,
        _ => LegacyRhoFileProperty::None,
    }
}

fn large_legacy_limits() -> LegacyRhoLimits {
    LegacyRhoLimits {
        max_archive_bytes: 1024 * 1024 * 1024,
        max_blocks: 1_000_000,
        max_compressed_block_bytes: 512 * 1024 * 1024,
        max_plaintext_block_bytes: 512 * 1024 * 1024,
        max_entries_per_directory: 250_000,
        max_path_components: 64,
        max_name_utf16_units: 4_096,
    }
}
