use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use anyhow::{Context, Result, bail};
use p5136_rho5::{
    AaaDocument, AaaLimits, LegacyRhoArchive, LegacyRhoFileProperty, LegacyRhoLimits,
    Rho5Directory, Rho5Limits,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const CACHE_SCHEMA: u32 = 1;
type RecordMap = BTreeMap<String, Vec<AssetRecord>>;

#[derive(Debug, Clone, Copy)]
pub(crate) enum AssetRegion {
    Korea,
    China,
}

#[derive(Debug, Clone)]
pub(crate) struct AssetRecord {
    pub virtual_path: String,
    pub size: usize,
    pub origin: AssetOrigin,
}

#[derive(Debug, Clone)]
pub(crate) enum AssetOrigin {
    Legacy {
        archive: String,
        internal_path: String,
        property: LegacyRhoFileProperty,
    },
    Rho5 {
        entry_index: usize,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum OriginReport {
    Legacy {
        archive: String,
        path: String,
        property: String,
    },
    Rho5Cn {
        path: String,
        archive: String,
    },
    Rho5Kr {
        path: String,
        archive: String,
    },
}

pub(crate) struct AssetIndex {
    data_dir: PathBuf,
    region: AssetRegion,
    rho5: Rho5Directory,
    records: RecordMap,
    pub warnings: Vec<String>,
    pub legacy_archive_count: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct LegacyIndexCache {
    schema_version: u32,
    data_directory: String,
    fingerprint: String,
    records: BTreeMap<String, Vec<CachedRecord>>,
    warnings: Vec<String>,
    archive_count: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedRecord {
    virtual_path: String,
    size: usize,
    archive: String,
    internal_path: String,
    property: String,
}

/// Extracts adjacent entries while retaining at most one decoded legacy RHO.
pub(crate) struct AssetExtractor<'a> {
    index: &'a AssetIndex,
    legacy: Option<(String, LegacyRhoArchive)>,
}

impl AssetIndex {
    pub fn scan(data_dir: &Path, region: AssetRegion, cache_path: &Path) -> Result<Self> {
        Self::scan_with_progress(data_dir, region, cache_path, &mut |_, _| {})
    }

    pub fn scan_with_progress(
        data_dir: &Path,
        region: AssetRegion,
        cache_path: &Path,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<Self> {
        let aaa_path = data_dir.join("aaa.pk");
        let fingerprint = legacy_fingerprint(data_dir, &aaa_path)?;
        let canonical_data = fs::canonicalize(data_dir)?.display().to_string();
        let aaa = AaaDocument::read(&aaa_path, AaaLimits::default())
            .with_context(|| format!("failed to read mount index {}", aaa_path.display()))?;
        let mounts = aaa
            .rho_mounts()
            .context("failed to enumerate aaa.pk mounts")?;
        let limits = large_legacy_limits();
        let cached = read_cache(cache_path, &canonical_data, &fingerprint)?;
        let (mut records, warnings, archive_count) = if let Some(cache) = cached {
            eprintln!("reused legacy asset index {}", cache_path.display());
            progress(1, 1);
            (
                restore_cached_records(cache.records)?,
                cache.warnings,
                cache.archive_count,
            )
        } else {
            let (records, warnings, archive_count) =
                scan_legacy_mounts(data_dir, mounts, limits, progress)?;
            write_cache(
                cache_path,
                &LegacyIndexCache {
                    schema_version: CACHE_SCHEMA,
                    data_directory: canonical_data,
                    fingerprint,
                    records: cache_records(&records),
                    warnings: warnings.clone(),
                    archive_count,
                },
            )?;
            (records, warnings, archive_count)
        };

        let rho5 = match region {
            AssetRegion::Korea => Rho5Directory::scan_kr(data_dir, Rho5Limits::default()),
            AssetRegion::China => Rho5Directory::scan_cn(data_dir, Rho5Limits::default()),
        }
        .with_context(|| format!("failed to index RHO5 archives at {}", data_dir.display()))?;
        for (entry_index, entry) in rho5.entries().iter().enumerate() {
            records
                .entry(fold_path(entry.normalized_path()))
                .or_default()
                .push(AssetRecord {
                    virtual_path: entry.normalized_path().to_owned(),
                    size: entry.plaintext_size(),
                    origin: AssetOrigin::Rho5 { entry_index },
                });
        }

        Ok(Self {
            data_dir: data_dir.to_path_buf(),
            region,
            rho5,
            records,
            warnings,
            legacy_archive_count: archive_count,
        })
    }

    pub fn effective(&self, path: &str) -> Option<&AssetRecord> {
        self.records
            .get(&fold_path(path))
            .and_then(|items| items.last())
    }

    pub fn effective_records(&self) -> impl Iterator<Item = &AssetRecord> {
        self.records.values().filter_map(|items| items.last())
    }

    pub fn overlay_count(&self) -> usize {
        self.records
            .values()
            .filter(|items| items.len() > 1)
            .count()
    }

    pub fn entry_count(&self) -> usize {
        self.records.len()
    }

    pub fn rho5_archive_count(&self) -> usize {
        self.rho5.archive_count()
    }

    pub fn extract(&self, record: &AssetRecord) -> Result<Vec<u8>> {
        match &record.origin {
            AssetOrigin::Legacy {
                archive,
                internal_path,
                ..
            } => {
                let opened =
                    LegacyRhoArchive::open(self.data_dir.join(archive), large_legacy_limits())?;
                Ok(opened.extract_exact(internal_path)?)
            }
            AssetOrigin::Rho5 { entry_index } => {
                let entry = self
                    .rho5
                    .entries()
                    .get(*entry_index)
                    .context("RHO5 entry index changed after scan")?;
                Ok(self.rho5.extract_entry_with_legacy_padding(entry)?)
            }
        }
    }

    pub fn extractor(&self) -> AssetExtractor<'_> {
        AssetExtractor {
            index: self,
            legacy: None,
        }
    }

    pub fn origin_report(&self, record: &AssetRecord) -> OriginReport {
        match &record.origin {
            AssetOrigin::Legacy {
                archive,
                internal_path,
                property,
            } => OriginReport::Legacy {
                archive: archive.clone(),
                path: internal_path.clone(),
                property: property_name(*property).to_owned(),
            },
            AssetOrigin::Rho5 { entry_index } => {
                let entry = &self.rho5.entries()[*entry_index];
                match self.region {
                    AssetRegion::China => OriginReport::Rho5Cn {
                        path: entry.normalized_path().to_owned(),
                        archive: entry.archive_name().to_owned(),
                    },
                    AssetRegion::Korea => OriginReport::Rho5Kr {
                        path: entry.normalized_path().to_owned(),
                        archive: entry.archive_name().to_owned(),
                    },
                }
            }
        }
    }
}

fn scan_legacy_mounts(
    data_dir: &Path,
    mounts: Vec<p5136_rho5::AaaRhoMount>,
    limits: LegacyRhoLimits,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<(RecordMap, Vec<String>, usize)> {
    let mut records = BTreeMap::<String, Vec<AssetRecord>>::new();
    let mut warnings = Vec::new();
    let mut seen_archives = HashSet::new();
    let mut archive_count = 0_usize;
    let mount_count = mounts.len();
    progress(0, mount_count);
    for (mount_index, mount) in mounts.into_iter().enumerate() {
        progress(mount_index, mount_count);
        if mount_index != 0 && mount_index.is_multiple_of(250) {
            eprintln!(
                "indexed {mount_index}/{mount_count} legacy mounts at {}",
                data_dir.display()
            );
        }
        let file_name = &mount.folder.file_name;
        validate_plain_file_name(file_name)?;
        if !seen_archives.insert(file_name.to_ascii_lowercase()) {
            warnings.push(format!(
                "aaa.pk repeats legacy archive {file_name:?}; client first-mount semantics applied"
            ));
            continue;
        }
        let archive_path = data_dir.join(file_name);
        if !archive_path.is_file() {
            warnings.push(format!(
                "aaa.pk references missing legacy archive {}",
                archive_path.display()
            ));
            continue;
        }
        let archive = LegacyRhoArchive::open(&archive_path, limits)
            .with_context(|| format!("failed to index {}", archive_path.display()))?;
        archive_count += 1;
        if archive.rho_key() != mount.folder.key {
            warnings.push(format!(
                "{} key mismatch: aaa={} rho={}",
                file_name,
                mount.folder.key,
                archive.rho_key()
            ));
        }
        if archive.data_hash() != mount.folder.data_hash {
            warnings.push(format!(
                "{} dataHash mismatch: aaa={} rho={}",
                file_name,
                mount.folder.data_hash,
                archive.data_hash()
            ));
        }
        let actual_size = fs::metadata(&archive_path)?.len();
        if actual_size != mount.folder.media_size {
            warnings.push(format!(
                "{} mediaSize mismatch: aaa={} disk={actual_size}",
                file_name, mount.folder.media_size
            ));
        }
        let prefix = mount.virtual_prefix();
        for entry in archive.entries()? {
            let virtual_path = join_virtual(&prefix, entry.normalized_path());
            records
                .entry(fold_path(&virtual_path))
                .or_default()
                .push(AssetRecord {
                    virtual_path,
                    size: entry.plaintext_size(),
                    origin: AssetOrigin::Legacy {
                        archive: file_name.clone(),
                        internal_path: entry.normalized_path().to_owned(),
                        property: entry.file_property(),
                    },
                });
        }
    }
    progress(mount_count, mount_count);
    Ok((records, warnings, archive_count))
}

fn legacy_fingerprint(data_dir: &Path, aaa_path: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(fs::read(aaa_path)?);
    let mut files = fs::read_dir(data_dir)?
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.to_ascii_lowercase()
                .ends_with(".rho")
                .then_some((name, entry.path()))
        })
        .collect::<Vec<_>>();
    files.sort_by(|left, right| {
        left.0
            .to_ascii_lowercase()
            .cmp(&right.0.to_ascii_lowercase())
    });
    for (name, path) in files {
        let metadata = fs::metadata(path)?;
        let modified = metadata
            .modified()?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        hasher.update(name.as_bytes());
        hasher.update(metadata.len().to_le_bytes());
        hasher.update(modified.to_le_bytes());
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn read_cache(
    cache_path: &Path,
    data_directory: &str,
    fingerprint: &str,
) -> Result<Option<LegacyIndexCache>> {
    if !cache_path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(cache_path)?;
    let Ok(cache) = serde_json::from_slice::<LegacyIndexCache>(&bytes) else {
        eprintln!(
            "ignored unreadable asset index cache {}",
            cache_path.display()
        );
        return Ok(None);
    };
    Ok((cache.schema_version == CACHE_SCHEMA
        && cache.data_directory == data_directory
        && cache.fingerprint == fingerprint)
        .then_some(cache))
}

fn write_cache(cache_path: &Path, cache: &LegacyIndexCache) -> Result<()> {
    if let Some(parent) = cache_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = cache_path.with_extension(format!("{}.tmp", std::process::id()));
    fs::write(&temporary, serde_json::to_vec(cache)?)?;
    if cache_path.exists() {
        fs::remove_file(cache_path)?;
    }
    fs::rename(&temporary, cache_path)?;
    Ok(())
}

fn cache_records(records: &RecordMap) -> BTreeMap<String, Vec<CachedRecord>> {
    records
        .iter()
        .map(|(key, values)| {
            let values = values
                .iter()
                .filter_map(|record| match &record.origin {
                    AssetOrigin::Legacy {
                        archive,
                        internal_path,
                        property,
                    } => Some(CachedRecord {
                        virtual_path: record.virtual_path.clone(),
                        size: record.size,
                        archive: archive.clone(),
                        internal_path: internal_path.clone(),
                        property: property_name(*property).to_owned(),
                    }),
                    AssetOrigin::Rho5 { .. } => None,
                })
                .collect();
            (key.clone(), values)
        })
        .collect()
}

fn restore_cached_records(records: BTreeMap<String, Vec<CachedRecord>>) -> Result<RecordMap> {
    records
        .into_iter()
        .map(|(key, values)| {
            let values = values
                .into_iter()
                .map(|record| {
                    Ok(AssetRecord {
                        virtual_path: record.virtual_path,
                        size: record.size,
                        origin: AssetOrigin::Legacy {
                            archive: record.archive,
                            internal_path: record.internal_path,
                            property: parse_property(&record.property)?,
                        },
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok((key, values))
        })
        .collect()
}

impl AssetExtractor<'_> {
    pub fn extract(&mut self, record: &AssetRecord) -> Result<Vec<u8>> {
        match &record.origin {
            AssetOrigin::Legacy {
                archive,
                internal_path,
                ..
            } => {
                let needs_open = self
                    .legacy
                    .as_ref()
                    .is_none_or(|(opened, _)| !opened.eq_ignore_ascii_case(archive));
                if needs_open {
                    let opened = LegacyRhoArchive::open(
                        self.index.data_dir.join(archive),
                        large_legacy_limits(),
                    )?;
                    self.legacy = Some((archive.clone(), opened));
                }
                Ok(self
                    .legacy
                    .as_ref()
                    .expect("legacy archive was just opened")
                    .1
                    .extract_exact(internal_path)?)
            }
            AssetOrigin::Rho5 { .. } => self.index.extract(record),
        }
    }
}

pub(crate) fn fold_path(path: &str) -> String {
    path.replace('\\', "/")
        .trim_matches('/')
        .to_ascii_lowercase()
}

fn join_virtual(prefix: &str, path: &str) -> String {
    if prefix.is_empty() {
        path.replace('\\', "/")
    } else {
        format!("{}/{}", prefix.trim_matches('/'), path.replace('\\', "/"))
    }
}

fn validate_plain_file_name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.contains(['/', '\\', '\0'])
        || value == "."
        || value == ".."
        || !value.to_ascii_lowercase().ends_with(".rho")
    {
        bail!("aaa.pk contains unsafe legacy archive name {value:?}");
    }
    Ok(())
}

fn property_name(property: LegacyRhoFileProperty) -> &'static str {
    match property {
        LegacyRhoFileProperty::None => "none",
        LegacyRhoFileProperty::Compressed => "compressed",
        LegacyRhoFileProperty::Encrypted => "encrypted",
        LegacyRhoFileProperty::PartialEncrypted => "partial_encrypted",
        LegacyRhoFileProperty::CompressedEncrypted => "compressed_encrypted",
    }
}

fn parse_property(value: &str) -> Result<LegacyRhoFileProperty> {
    match value {
        "none" => Ok(LegacyRhoFileProperty::None),
        "compressed" => Ok(LegacyRhoFileProperty::Compressed),
        "encrypted" => Ok(LegacyRhoFileProperty::Encrypted),
        "partial_encrypted" => Ok(LegacyRhoFileProperty::PartialEncrypted),
        "compressed_encrypted" => Ok(LegacyRhoFileProperty::CompressedEncrypted),
        _ => bail!("asset index cache has unknown legacy property {value:?}"),
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
