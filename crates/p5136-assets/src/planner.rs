use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use p5136_rho5::{AaaLimits, AaaNode, LegacyRhoFileProperty};
use quick_xml::{
    Reader, XmlVersion,
    events::{BytesStart, Event},
};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::asset_index::{
    AssetExtractor, AssetIndex, AssetOrigin, AssetRecord, AssetRegion, OriginReport, fold_path,
};
use crate::{COMPATIBILITY_ASSERTION, EXPERIMENTAL_NATIVE_ASSERTION};

const MAX_STRUCTURED_STRINGS: usize = 250_000;
const MAX_STRING_BYTES: usize = 64 * 1024;
const REVIEW_REQUIRED: &str = "review-required-not-importable";

#[derive(Debug, Clone)]
pub(crate) struct PlanOptions {
    pub category: String,
    pub asset: Option<String>,
    /// Optional exact `category:asset_id` selectors used by the integrated GUI.
    /// The legacy CLI leaves this empty and retains its existing category/asset
    /// filtering behavior.
    pub asset_selectors: BTreeSet<String>,
    /// Exact native-backport selectors that may receive the experimental
    /// sidecar assertion. The integrated importer supplies a bounded allowlist;
    /// the general CLI deliberately leaves this empty.
    pub experimental_native_selectors: BTreeSet<String>,
    pub include_existing: bool,
    pub max_assets: usize,
    pub max_asset_bytes: usize,
}

#[derive(Debug, Serialize)]
pub(crate) struct PlanReport {
    schema_version: u32,
    source_data: String,
    target_data: String,
    source_index: IndexSummary,
    target_index: IndexSummary,
    selected_groups: usize,
    status_counts: BTreeMap<String, usize>,
    warnings: Vec<String>,
    assets: Vec<AssetPlan>,
}

#[derive(Debug, Serialize)]
struct IndexSummary {
    legacy_archives: usize,
    rho5_archives: usize,
    effective_entries: usize,
    overlays: usize,
}

#[derive(Debug, Serialize)]
struct AssetPlan {
    category: String,
    asset_id: String,
    virtual_prefix: String,
    target_already_has_group: bool,
    status: CompatibilityStatus,
    reasons: Vec<String>,
    total_bytes: usize,
    files: Vec<FilePlan>,
    references: Vec<ReferenceReport>,
    unresolved_references: Vec<String>,
    localization_tasks: Vec<LocalizationTask>,
    manifest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CompatibilityStatus {
    CompatibleCandidate,
    ExperimentalNativeCandidate,
    LocalizationRequired,
    NativeBackportRequired,
    Unresolved,
}

impl CompatibilityStatus {
    const fn label(self) -> &'static str {
        match self {
            Self::CompatibleCandidate => "compatible_candidate",
            Self::ExperimentalNativeCandidate => "experimental_native_candidate",
            Self::LocalizationRequired => "localization_required",
            Self::NativeBackportRequired => "native_backport_required",
            Self::Unresolved => "unresolved",
        }
    }
}

#[derive(Debug, Serialize)]
struct FilePlan {
    virtual_path: String,
    bytes: usize,
    sha256: String,
    origin: OriginReport,
    format_signature: Option<String>,
    format_seen_in_p5136: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct ReferenceReport {
    from: String,
    value: String,
    resolved_path: Option<String>,
    resolution: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct LocalizationTask {
    source_path: String,
    selector: String,
    field: String,
    original: String,
    target_locales: Vec<String>,
    replacement: Option<String>,
}

#[derive(Debug, Serialize)]
struct DraftManifest {
    schema_version: u32,
    compatibility: String,
    output_archive: String,
    rho_folder_name: String,
    pack_path: Vec<String>,
    entries: Vec<DraftManifestEntry>,
}

#[derive(Debug, Serialize)]
struct DraftManifestEntry {
    source: DraftSource,
    target_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    property: Option<String>,
    expected_sha256: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum DraftSource {
    Rho5Cn { path: String },
    Legacy { archive: String, path: String },
}

struct StructuredString {
    selector: String,
    field: String,
    value: String,
}

struct ContentAnalysis {
    strings: Vec<StructuredString>,
    parse_error: Option<String>,
}

struct Planner<'a> {
    source: &'a AssetIndex,
    target: &'a AssetIndex,
    source_extractor: AssetExtractor<'a>,
    target_extractor: AssetExtractor<'a>,
    target_signatures: HashMap<String, HashSet<Vec<u8>>>,
}

pub(crate) fn run_plan(
    source_data: &Path,
    target_data: &Path,
    output: &Path,
    options: &PlanOptions,
) -> Result<PathBuf> {
    ensure!(options.max_assets > 0, "max-assets must be nonzero");
    ensure!(
        options.max_asset_bytes > 0,
        "max-asset-bytes must be nonzero"
    );
    validate_category(&options.category)?;
    ensure_report_destination(source_data, target_data, output)?;

    let cache_directory = output.join(".index-cache");
    fs::create_dir_all(&cache_directory)
        .with_context(|| format!("failed to create {}", cache_directory.display()))?;
    eprintln!("indexing Chinese source Data...");
    let source = AssetIndex::scan(
        source_data,
        AssetRegion::China,
        &cache_directory.join("source-legacy.json"),
    )?;
    eprintln!("indexing Korean P5136 target Data...");
    let target = AssetIndex::scan(
        target_data,
        AssetRegion::Korea,
        &cache_directory.join("target-legacy.json"),
    )?;
    let groups = select_groups(&source, &target, options)?;
    fs::create_dir_all(output.join("manifests"))
        .with_context(|| format!("failed to create {}", output.display()))?;

    let source_summary = summary(&source);
    let target_summary = summary(&target);
    let warnings = source
        .warnings
        .iter()
        .map(|warning| format!("source: {warning}"))
        .chain(
            target
                .warnings
                .iter()
                .map(|warning| format!("target: {warning}")),
        )
        .collect::<Vec<_>>();
    let mut warnings = bounded_warnings(warnings, 250);
    let mut planner = Planner {
        source: &source,
        target: &target,
        source_extractor: source.extractor(),
        target_extractor: target.extractor(),
        target_signatures: HashMap::new(),
    };
    let mut assets = Vec::new();
    for group in groups {
        match planner.plan_group(&group, output, options) {
            Ok(plan) => assets.push(plan),
            Err(error) => warnings.push(format!("{}: {error:#}", group.prefix)),
        }
    }
    let mut status_counts = BTreeMap::new();
    for asset in &assets {
        *status_counts
            .entry(asset.status.label().to_owned())
            .or_insert(0) += 1;
    }
    let report = PlanReport {
        schema_version: 1,
        source_data: source_data.display().to_string(),
        target_data: target_data.display().to_string(),
        source_index: source_summary,
        target_index: target_summary,
        selected_groups: assets.len(),
        status_counts,
        warnings,
        assets,
    };
    let report_path = output.join("compatibility-report.json");
    fs::write(&report_path, serde_json::to_vec_pretty(&report)?)
        .with_context(|| format!("failed to write {}", report_path.display()))?;
    let markdown_path = output.join("compatibility-report.md");
    fs::write(&markdown_path, render_markdown(&report))
        .with_context(|| format!("failed to write {}", markdown_path.display()))?;
    Ok(report_path)
}

fn render_markdown(report: &PlanReport) -> String {
    let mut output = String::new();
    writeln!(output, "# P5136 에셋 변환 가능 후보 보고서\n")
        .expect("writing to String cannot fail");
    writeln!(output, "- 소스: `{}`", markdown_cell(&report.source_data))
        .expect("writing to String cannot fail");
    writeln!(output, "- 대상: `{}`", markdown_cell(&report.target_data))
        .expect("writing to String cannot fail");
    writeln!(output, "- 분석 그룹: {}개", report.selected_groups)
        .expect("writing to String cannot fail");
    writeln!(
        output,
        "- 소스 인덱스: legacy {} / RHO5 {} / 유효 경로 {}",
        report.source_index.legacy_archives,
        report.source_index.rho5_archives,
        report.source_index.effective_entries
    )
    .expect("writing to String cannot fail");
    writeln!(
        output,
        "- 대상 인덱스: legacy {} / RHO5 {} / 유효 경로 {}\n",
        report.target_index.legacy_archives,
        report.target_index.rho5_archives,
        report.target_index.effective_entries
    )
    .expect("writing to String cannot fail");
    writeln!(
        output,
        "> 정적 후보 판정입니다. `compatible_candidate`도 별도 staging 클라이언트에서 인벤토리, 로딩, 주행, 시상식 검증을 거쳐야 합니다.\n"
    )
    .expect("writing to String cannot fail");

    write_asset_table(
        &mut output,
        "변환 가능 후보",
        report.assets.iter().filter(|asset| {
            matches!(
                asset.status,
                CompatibilityStatus::CompatibleCandidate
                    | CompatibilityStatus::ExperimentalNativeCandidate
                    | CompatibilityStatus::LocalizationRequired
            )
        }),
    );
    write_asset_table(
        &mut output,
        "네이티브 백포트 필요",
        report
            .assets
            .iter()
            .filter(|asset| asset.status == CompatibilityStatus::NativeBackportRequired),
    );
    write_asset_table(
        &mut output,
        "미해결 후보",
        report
            .assets
            .iter()
            .filter(|asset| asset.status == CompatibilityStatus::Unresolved),
    );

    if !report.warnings.is_empty() {
        writeln!(output, "## 인덱스 경고\n").expect("writing to String cannot fail");
        for warning in report.warnings.iter().take(50) {
            writeln!(output, "- {}", markdown_cell(warning))
                .expect("writing to String cannot fail");
        }
        if report.warnings.len() > 50 {
            writeln!(
                output,
                "- 그 외 {}개 경고는 JSON 보고서 참조",
                report.warnings.len() - 50
            )
            .expect("writing to String cannot fail");
        }
    }
    output
}

fn write_asset_table<'a>(
    output: &mut String,
    title: &str,
    assets: impl Iterator<Item = &'a AssetPlan>,
) {
    writeln!(output, "## {title}\n").expect("writing to String cannot fail");
    writeln!(
        output,
        "| 분류 | 에셋 | 상태 | 파일 | 크기 | 현지화 | 미해결 | 근거 | manifest |"
    )
    .expect("writing to String cannot fail");
    writeln!(output, "|---|---|---|---:|---:|---:|---:|---|---|")
        .expect("writing to String cannot fail");
    let mut count = 0_usize;
    for asset in assets {
        count += 1;
        let reasons = if asset.reasons.is_empty() {
            "-".to_owned()
        } else {
            asset
                .reasons
                .iter()
                .map(|reason| markdown_cell(reason))
                .collect::<Vec<_>>()
                .join("<br>")
        };
        writeln!(
            output,
            "| {} | `{}` | `{}` | {} | {} | {} | {} | {} | [{}]({}) |",
            markdown_cell(&asset.category),
            markdown_cell(&asset.asset_id),
            asset.status.label(),
            asset.files.len(),
            asset.total_bytes,
            asset.localization_tasks.len(),
            asset.unresolved_references.len(),
            reasons,
            markdown_cell(&asset.manifest),
            asset.manifest
        )
        .expect("writing to String cannot fail");
    }
    if count == 0 {
        writeln!(output, "| - | - | - | 0 | 0 | 0 | 0 | 해당 없음 | - |")
            .expect("writing to String cannot fail");
    }
    output.push('\n');
}

fn markdown_cell(value: &str) -> String {
    value
        .replace('|', "\\|")
        .replace(['\r', '\n'], " ")
        .replace('`', "\\`")
}

#[derive(Debug)]
struct AssetGroup {
    category: String,
    asset_id: String,
    prefix: String,
    target_has_group: bool,
}

fn select_groups(
    source: &AssetIndex,
    target: &AssetIndex,
    options: &PlanOptions,
) -> Result<Vec<AssetGroup>> {
    let mut candidates = BTreeMap::<String, AssetGroup>::new();
    let active_tracks = crate::track_bundle::active_ordinary_track_ids(source)?;
    let target_groups = target
        .effective_records()
        .filter_map(|record| group_for_path(&record.virtual_path))
        .map(|(_, _, prefix)| fold_path(&prefix))
        .collect::<HashSet<_>>();
    for record in source.effective_records() {
        let Some((category, asset_id, prefix)) = group_for_path(&record.virtual_path) else {
            continue;
        };
        if category == "track" && !active_tracks.contains(&fold_path(&asset_id)) {
            continue;
        }
        if options.category != "all" && options.category != category {
            continue;
        }
        if let Some(wanted) = &options.asset
            && !asset_id.eq_ignore_ascii_case(wanted)
            && !prefix.eq_ignore_ascii_case(wanted)
        {
            continue;
        }
        if !options.asset_selectors.is_empty()
            && !options.asset_selectors.contains(&format!(
                "{}:{}",
                category.to_ascii_lowercase(),
                asset_id.to_ascii_lowercase()
            ))
        {
            continue;
        }
        let target_has_group = target_groups.contains(&fold_path(&prefix));
        if target_has_group && !options.include_existing {
            continue;
        }
        candidates.entry(fold_path(&prefix)).or_insert(AssetGroup {
            category,
            asset_id,
            prefix,
            target_has_group,
        });
    }
    let mut groups = candidates.into_values().collect::<Vec<_>>();
    if groups.len() > options.max_assets {
        if options.asset.is_some() || !options.asset_selectors.is_empty() {
            bail!(
                "asset selector matched {} groups, exceeding max-assets {}",
                groups.len(),
                options.max_assets
            );
        }
        groups.truncate(options.max_assets);
    }
    Ok(groups)
}

impl Planner<'_> {
    #[allow(clippy::too_many_lines)]
    fn plan_group(
        &mut self,
        group: &AssetGroup,
        output: &Path,
        options: &PlanOptions,
    ) -> Result<AssetPlan> {
        let asset_key = format!(
            "{}:{}",
            group.category.to_ascii_lowercase(),
            group.asset_id.to_ascii_lowercase()
        );
        let experimental_native = options.experimental_native_selectors.contains(&asset_key);
        let mut queue = VecDeque::<String>::new();
        for record in self.source.effective_records() {
            if path_in_prefix(&record.virtual_path, &group.prefix) {
                queue.push_back(record.virtual_path.clone());
            }
        }
        ensure!(!queue.is_empty(), "asset group contains no effective files");
        if experimental_native
            && group.category.eq_ignore_ascii_case("kart")
            && group.asset_id.to_ascii_lowercase().ends_with("xun")
        {
            for record in self.source.effective_records() {
                if path_in_prefix(&record.virtual_path, "gui/tachometer/xun")
                    || path_in_prefix(&record.virtual_path, "effect/charger")
                {
                    queue.push_back(record.virtual_path.clone());
                }
            }
        }

        let mut visited = HashSet::new();
        let mut files = Vec::new();
        let mut manifest_entries = Vec::new();
        let mut references = BTreeSet::new();
        let mut unresolved = BTreeSet::new();
        let mut localization = BTreeSet::new();
        let mut reasons = BTreeSet::new();
        let mut total_bytes = 0_usize;
        let mut native_required = false;
        let mut native_hits = BTreeSet::new();
        let mut format_unresolved = false;
        let mut format_mismatch_count = 0_usize;

        while let Some(path) = queue.pop_front() {
            let folded = fold_path(&path);
            if !visited.insert(folded) {
                continue;
            }
            let record = self
                .source
                .effective(&path)
                .with_context(|| format!("dependency disappeared from source index: {path}"))?;
            total_bytes = total_bytes
                .checked_add(record.size)
                .context("asset dependency byte count overflow")?;
            ensure!(
                total_bytes <= options.max_asset_bytes,
                "dependency closure exceeds max-asset-bytes {}",
                options.max_asset_bytes
            );
            let bytes = self
                .source_extractor
                .extract(record)
                .with_context(|| format!("failed to extract {path}"))?;
            ensure!(
                bytes.len() == record.size,
                "indexed size changed for {path}"
            );
            let sha256 = format!("{:x}", Sha256::digest(&bytes));
            let extension = extension(&path).to_ascii_lowercase();
            let analysis = analyze_content(&path, &extension, &bytes);
            if let Some(error) = &analysis.parse_error {
                format_unresolved = true;
                reasons.insert(format!("structured parse failed at {path}: {error}"));
            }
            if let Some(marker) = native_marker(&path) {
                native_required = true;
                native_hits.insert(format!("{marker} in path {path}"));
            }
            for item in &analysis.strings {
                if !item.selector.starts_with("binary:")
                    && let Some(marker) = native_marker(&item.value)
                {
                    native_required = true;
                    native_hits.insert(format!("{marker} in {path} {}", item.selector));
                }
            }
            if matches!(extension.as_str(), "xml" | "kml" | "bml") {
                for item in &analysis.strings {
                    if has_han(&item.value) {
                        localization.insert(LocalizationTask {
                            source_path: path.clone(),
                            selector: item.selector.clone(),
                            field: item.field.clone(),
                            original: item.value.clone(),
                            target_locales: vec!["ko-KR".to_owned(), "en-US".to_owned()],
                            replacement: None,
                        });
                    }
                }
            }

            for item in &analysis.strings {
                for candidate in reference_candidates(&item.value) {
                    let resolved =
                        resolve_reference(self.source, self.target, &path, group, &candidate);
                    let (resolved_path, resolution) = match resolved {
                        ReferenceResolution::Source(found) => {
                            queue.push_back(found.clone());
                            (Some(found), "source_dependency")
                        }
                        ReferenceResolution::Target(found) => (Some(found), "target_satisfied"),
                        ReferenceResolution::Missing => {
                            unresolved.insert(format!("{path}: {candidate}"));
                            (None, "unresolved")
                        }
                    };
                    references.insert(ReferenceReport {
                        from: path.clone(),
                        value: candidate,
                        resolved_path,
                        resolution: resolution.to_owned(),
                    });
                }
            }

            let (format_signature, format_seen) = if extension == "1s" {
                let signature = bytes.get(..2).unwrap_or(&bytes).to_vec();
                let seen = self.target_format_seen("1s", &signature)?;
                if !seen {
                    format_unresolved = true;
                    format_mismatch_count += 1;
                }
                (Some(hex(&signature)), Some(seen))
            } else {
                (
                    bytes
                        .get(..8)
                        .filter(|_| is_binary_extension(&extension))
                        .map(hex),
                    None,
                )
            };
            files.push(FilePlan {
                virtual_path: path.clone(),
                bytes: bytes.len(),
                sha256: sha256.clone(),
                origin: self.source.origin_report(record),
                format_signature,
                format_seen_in_p5136: format_seen,
            });
            manifest_entries.push(manifest_entry(record, sha256));
        }

        if !unresolved.is_empty() {
            reasons.insert(format!(
                "{} dependency references are unresolved",
                unresolved.len()
            ));
        }
        if !localization.is_empty() {
            reasons.insert(format!(
                "{} Chinese strings require structural translation",
                localization.len()
            ));
        }
        if native_required {
            for hit in native_hits.iter().take(20) {
                reasons.insert(format!("post-P5136 native marker: {hit}"));
            }
        }
        if format_mismatch_count != 0 {
            reasons.insert(format!(
                "{format_mismatch_count} .1s files have no format signature observed in P5136"
            ));
        }
        let status = if native_required
            && experimental_native
            && !format_unresolved
            && unresolved.is_empty()
            && localization.is_empty()
        {
            CompatibilityStatus::ExperimentalNativeCandidate
        } else if native_required {
            CompatibilityStatus::NativeBackportRequired
        } else if format_unresolved || !unresolved.is_empty() {
            CompatibilityStatus::Unresolved
        } else if !localization.is_empty() {
            CompatibilityStatus::LocalizationRequired
        } else {
            CompatibilityStatus::CompatibleCandidate
        };
        files.sort_by(|left, right| left.virtual_path.cmp(&right.virtual_path));
        manifest_entries.sort_by(|left, right| left.target_path.cmp(&right.target_path));
        let manifest_name = format!("{}.json", safe_asset_name(&group.prefix));
        let manifest_path = output.join("manifests").join(&manifest_name);
        let manifest = DraftManifest {
            schema_version: 1,
            compatibility: match status {
                CompatibilityStatus::CompatibleCandidate => COMPATIBILITY_ASSERTION.to_owned(),
                CompatibilityStatus::ExperimentalNativeCandidate => {
                    EXPERIMENTAL_NATIVE_ASSERTION.to_owned()
                }
                _ => REVIEW_REQUIRED.to_owned(),
            },
            output_archive: format!("import_{}.rho", safe_asset_name(&group.prefix)),
            rho_folder_name: String::new(),
            pack_path: Vec::new(),
            entries: manifest_entries,
        };
        fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;
        Ok(AssetPlan {
            category: group.category.clone(),
            asset_id: group.asset_id.clone(),
            virtual_prefix: group.prefix.clone(),
            target_already_has_group: group.target_has_group,
            status,
            reasons: reasons.into_iter().collect(),
            total_bytes,
            files,
            references: references.into_iter().collect(),
            unresolved_references: unresolved.into_iter().collect(),
            localization_tasks: localization.into_iter().collect(),
            manifest: format!("manifests/{manifest_name}"),
        })
    }

    fn target_format_seen(&mut self, extension: &str, signature: &[u8]) -> Result<bool> {
        if self
            .target_signatures
            .get(extension)
            .is_some_and(|known| known.contains(signature))
        {
            return Ok(true);
        }
        let mut signatures = self.target_signatures.remove(extension).unwrap_or_default();
        let mut found = false;
        for record in self
            .target
            .effective_records()
            .filter(|record| crate::planner::extension(&record.virtual_path) == extension)
            .take(128)
        {
            let bytes = self.target_extractor.extract(record)?;
            let candidate = bytes.get(..2).unwrap_or(&bytes).to_vec();
            found |= candidate == signature;
            signatures.insert(candidate);
            if found {
                break;
            }
        }
        self.target_signatures
            .insert(extension.to_owned(), signatures);
        Ok(found)
    }
}

fn manifest_entry(record: &AssetRecord, expected_sha256: String) -> DraftManifestEntry {
    let (source, property) = match &record.origin {
        AssetOrigin::Legacy {
            archive,
            internal_path,
            property,
        } => (
            DraftSource::Legacy {
                archive: archive.clone(),
                path: internal_path.clone(),
            },
            Some(property_name(*property).to_owned()),
        ),
        AssetOrigin::Rho5 { .. } => (
            DraftSource::Rho5Cn {
                path: record.virtual_path.clone(),
            },
            None,
        ),
    };
    DraftManifestEntry {
        source,
        target_path: record.virtual_path.clone(),
        property,
        expected_sha256,
    }
}

fn analyze_content(path: &str, extension: &str, bytes: &[u8]) -> ContentAnalysis {
    match extension {
        "xml" | "kml" => analyze_xml(bytes).unwrap_or_else(|error| ContentAnalysis {
            strings: Vec::new(),
            parse_error: Some(error.to_string()),
        }),
        "bml" => analyze_bml(bytes).unwrap_or_else(|error| ContentAnalysis {
            strings: Vec::new(),
            parse_error: Some(error.to_string()),
        }),
        "1s" | "uset" | "kap" => ContentAnalysis {
            strings: scan_binary_strings(path, bytes),
            parse_error: None,
        },
        _ => ContentAnalysis {
            strings: Vec::new(),
            parse_error: None,
        },
    }
}

fn analyze_xml(bytes: &[u8]) -> Result<ContentAnalysis> {
    let text = decode_text(bytes)?;
    let mut reader = Reader::from_str(&text);
    reader.config_mut().trim_text(false);
    let mut stack = Vec::<String>::new();
    let mut strings = Vec::new();
    loop {
        match reader.read_event()? {
            Event::Start(element) => {
                let name = String::from_utf8_lossy(element.local_name().as_ref()).into_owned();
                stack.push(name);
                let selector = format!("/{}", stack.join("/"));
                collect_xml_attributes(&reader, &element, &selector, &mut strings)?;
            }
            Event::Empty(element) => {
                let name = String::from_utf8_lossy(element.local_name().as_ref()).into_owned();
                stack.push(name);
                let selector = format!("/{}", stack.join("/"));
                collect_xml_attributes(&reader, &element, &selector, &mut strings)?;
                stack.pop();
            }
            Event::Text(value) => {
                let value = value.decode()?.into_owned();
                push_structured(
                    &mut strings,
                    &format!("/{}", stack.join("/")),
                    "text",
                    value,
                )?;
            }
            Event::CData(value) => {
                let value = reader.decoder().decode(value.as_ref())?.into_owned();
                push_structured(
                    &mut strings,
                    &format!("/{}", stack.join("/")),
                    "cdata",
                    value,
                )?;
            }
            Event::End(_) => {
                stack.pop();
            }
            Event::DocType(_) => bail!("DOCTYPE is not accepted"),
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(ContentAnalysis {
        strings,
        parse_error: None,
    })
}

fn collect_xml_attributes(
    reader: &Reader<&[u8]>,
    element: &BytesStart<'_>,
    selector: &str,
    output: &mut Vec<StructuredString>,
) -> Result<()> {
    for attribute in element.attributes().with_checks(true) {
        let attribute = attribute?;
        let key = String::from_utf8_lossy(attribute.key.as_ref()).into_owned();
        let value = attribute
            .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())?
            .into_owned();
        push_structured(output, selector, &format!("attribute:{key}"), value)?;
    }
    Ok(())
}

fn analyze_bml(bytes: &[u8]) -> Result<ContentAnalysis> {
    let node = AaaNode::decode_binary_xml(bytes, AaaLimits::default())?;
    let mut strings = Vec::new();
    collect_bml_strings(&node, "", 0, &mut strings)?;
    Ok(ContentAnalysis {
        strings,
        parse_error: None,
    })
}

fn collect_bml_strings(
    node: &AaaNode,
    parent: &str,
    sibling: usize,
    output: &mut Vec<StructuredString>,
) -> Result<()> {
    let selector = format!("{parent}/{}[{sibling}]", node.name);
    push_structured(output, &selector, "text", node.text.clone())?;
    for (name, value) in &node.attributes {
        push_structured(
            output,
            &selector,
            &format!("attribute:{name}"),
            value.clone(),
        )?;
    }
    for (index, child) in node.children.iter().enumerate() {
        collect_bml_strings(child, &selector, index, output)?;
    }
    Ok(())
}

fn push_structured(
    output: &mut Vec<StructuredString>,
    selector: &str,
    field: &str,
    value: String,
) -> Result<()> {
    ensure!(
        output.len() < MAX_STRUCTURED_STRINGS,
        "too many structured strings"
    );
    ensure!(
        value.len() <= MAX_STRING_BYTES,
        "structured string is too large"
    );
    if !value.trim().is_empty() {
        output.push(StructuredString {
            selector: selector.to_owned(),
            field: field.to_owned(),
            value,
        });
    }
    Ok(())
}

fn scan_binary_strings(path: &str, bytes: &[u8]) -> Vec<StructuredString> {
    let mut values = BTreeSet::new();
    let mut current = Vec::new();
    for byte in bytes.iter().copied().chain([0]) {
        if byte.is_ascii_graphic() || byte == b' ' {
            current.push(byte);
        } else {
            if current.len() >= 4 {
                values.insert(String::from_utf8_lossy(&current).into_owned());
            }
            current.clear();
        }
    }
    let mut units = Vec::new();
    for pair in bytes
        .chunks_exact(2)
        .chain(std::iter::once(&[0_u8, 0_u8][..]))
    {
        let unit = u16::from_le_bytes([pair[0], pair[1]]);
        if unit >= 0x20 && unit != 0x7f {
            units.push(unit);
        } else {
            if units.len() >= 4
                && let Ok(value) = String::from_utf16(&units)
            {
                values.insert(value);
            }
            units.clear();
        }
    }
    values
        .into_iter()
        .filter(|value| !reference_candidates(value).is_empty())
        .take(MAX_STRUCTURED_STRINGS)
        .enumerate()
        .map(|(index, value)| StructuredString {
            selector: format!("binary:{path}:{index}"),
            field: "embedded_string".to_owned(),
            value,
        })
        .collect()
}

enum ReferenceResolution {
    Source(String),
    Target(String),
    Missing,
}

fn resolve_reference(
    source: &AssetIndex,
    target: &AssetIndex,
    from: &str,
    group: &AssetGroup,
    candidate: &str,
) -> ReferenceResolution {
    let parent = from.rsplit_once('/').map_or("", |(parent, _)| parent);
    let group_root = group.prefix.split_once('/').map_or("", |(root, _)| root);
    let mut attempts = Vec::new();
    if category_for_root(candidate.split('/').next().unwrap_or_default()).is_some() {
        attempts.push(normalize_virtual(candidate));
    }
    attempts.push(normalize_virtual(&format!("{parent}/{candidate}")));
    attempts.push(normalize_virtual(&format!("{}/{candidate}", group.prefix)));
    attempts.push(normalize_virtual(&format!("{group_root}/{candidate}")));
    attempts.sort();
    attempts.dedup();
    for attempt in attempts.iter().flatten() {
        if let Some(record) = source.effective(attempt) {
            return ReferenceResolution::Source(record.virtual_path.clone());
        }
    }
    for attempt in attempts.into_iter().flatten() {
        if let Some(record) = target.effective(&attempt) {
            return ReferenceResolution::Target(record.virtual_path.clone());
        }
    }
    ReferenceResolution::Missing
}

fn reference_candidates(value: &str) -> Vec<String> {
    let mut output = BTreeSet::new();
    for token in value.split(|character: char| {
        !(character.is_alphanumeric() || matches!(character, '_' | '-' | '.' | '@' | '/' | '\\'))
    }) {
        let token = token.replace('\\', "/");
        let ext = extension(&token);
        let has_stem = token
            .rsplit_once('.')
            .is_some_and(|(stem, _)| !stem.trim_matches(['/', '\\', '.']).is_empty());
        if has_stem && token.len() <= 4_096 && is_asset_extension(ext) {
            output.insert(token);
        }
    }
    output.into_iter().collect()
}

fn group_for_path(path: &str) -> Option<(String, String, String)> {
    let mut components = path.split('/');
    let root = components.next()?;
    let asset = components.next()?;
    components.next()?;
    let category = category_for_root(root)?;
    Some((
        category.to_owned(),
        asset.to_owned(),
        format!("{root}/{asset}"),
    ))
}

fn category_for_root(root: &str) -> Option<&'static str> {
    match root.to_ascii_lowercase().as_str() {
        "kart_" | "kart" => Some("kart"),
        "track_" | "track" => Some("track"),
        "character_" | "character" | "rider_" | "rider" => Some("character"),
        "pet" | "pet_" => Some("pet"),
        "flyingpet" | "flyingpet_" => Some("flying_pet"),
        _ => None,
    }
}

fn validate_category(category: &str) -> Result<()> {
    ensure!(
        matches!(
            category,
            "all" | "kart" | "character" | "track" | "pet" | "flying_pet"
        ),
        "category must be all, kart, character, track, pet, or flying_pet"
    );
    Ok(())
}

fn path_in_prefix(path: &str, prefix: &str) -> bool {
    let path = fold_path(path);
    let prefix = fold_path(prefix);
    path == prefix
        || path
            .strip_prefix(&prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn extension(path: &str) -> &str {
    path.rsplit_once('.')
        .map_or("", |(_, extension)| extension)
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
}

fn is_asset_extension(extension: &str) -> bool {
    matches!(
        extension.to_ascii_lowercase().as_str(),
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

fn is_binary_extension(extension: &str) -> bool {
    !matches!(extension, "xml" | "kml")
}

fn has_han(value: &str) -> bool {
    value.chars().any(|character| {
        matches!(
            character as u32,
            0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff | 0x20000..=0x2fa1f
        )
    })
}

fn native_marker(value: &str) -> Option<&'static str> {
    let value = value.to_ascii_lowercase();
    ["xun", "kart12", "parts12", "xungen", "generation12"]
        .iter()
        .copied()
        .find(|marker| value.contains(marker))
}

fn normalize_virtual(value: &str) -> Option<String> {
    if value.contains(':') || value.starts_with(['/', '\\']) {
        return None;
    }
    let mut components = Vec::new();
    let normalized = value.replace('\\', "/");
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

fn decode_text(bytes: &[u8]) -> Result<String> {
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.get(1) == Some(&0) {
        return decode_utf16(
            &bytes[usize::from(bytes.starts_with(&[0xff, 0xfe])) * 2..],
            true,
        );
    }
    if bytes.starts_with(&[0xfe, 0xff]) {
        return decode_utf16(&bytes[2..], false);
    }
    Ok(std::str::from_utf8(bytes)?
        .trim_start_matches('\u{feff}')
        .to_owned())
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> Result<String> {
    ensure!(bytes.len().is_multiple_of(2), "odd UTF-16 byte length");
    let units = bytes.chunks_exact(2).map(|pair| {
        if little_endian {
            u16::from_le_bytes([pair[0], pair[1]])
        } else {
            u16::from_be_bytes([pair[0], pair[1]])
        }
    });
    Ok(char::decode_utf16(units).collect::<std::result::Result<String, _>>()?)
}

fn safe_asset_name(value: &str) -> String {
    let mut output = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    while output.contains("__") {
        output = output.replace("__", "_");
    }
    output = output.trim_matches('_').to_owned();
    if output.is_empty() {
        output = hex(&Sha256::digest(value.as_bytes())[..8]);
    }
    output
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(
        String::with_capacity(bytes.len().saturating_mul(2)),
        |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to String cannot fail");
            output
        },
    )
}

fn summary(index: &AssetIndex) -> IndexSummary {
    IndexSummary {
        legacy_archives: index.legacy_archive_count,
        rho5_archives: index.rho5_archive_count(),
        effective_entries: index.entry_count(),
        overlays: index.overlay_count(),
    }
}

fn bounded_warnings(mut warnings: Vec<String>, maximum: usize) -> Vec<String> {
    if warnings.len() <= maximum {
        return warnings;
    }
    let omitted = warnings.len() - maximum;
    warnings.truncate(maximum);
    warnings.push(format!("{omitted} additional index warnings were omitted"));
    warnings
}

fn ensure_report_destination(source: &Path, target: &Path, output: &Path) -> Result<()> {
    let source = fs::canonicalize(source)?;
    let target = fs::canonicalize(target)?;
    let output = canonicalize_future_path(output)?;
    ensure!(
        !output.starts_with(source),
        "report output may not be inside source Data"
    );
    ensure!(
        !output.starts_with(target),
        "report output may not be inside target Data"
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
        missing.push(
            cursor
                .file_name()
                .context("output path has no existing ancestor")?
                .to_os_string(),
        );
        cursor = cursor
            .parent()
            .context("output path has no existing ancestor")?;
    }
    let mut result = fs::canonicalize(cursor)?;
    for component in missing.iter().rev() {
        result.push(component);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::{has_han, native_marker, normalize_virtual, reference_candidates, safe_asset_name};

    #[test]
    fn references_are_bounded_to_known_asset_extensions() {
        assert_eq!(
            reference_candidates("../common/body.1s and ignored.exe"),
            ["../common/body.1s"]
        );
        assert!(reference_candidates(".1s .png").is_empty());
    }

    #[test]
    fn virtual_normalization_rejects_escape() {
        assert_eq!(
            normalize_virtual("kart_/a/../b/model.1s").unwrap(),
            "kart_/b/model.1s"
        );
        assert!(normalize_virtual("../../outside.1s").is_none());
    }

    #[test]
    fn localization_and_safe_names_are_deterministic() {
        assert!(has_han("黄金推进器"));
        assert!(!has_han("Golden booster"));
        assert_eq!(safe_asset_name("kart_/mancarXUN"), "kart_mancarxun");
    }

    #[test]
    fn ordinary_v1_exceed_is_not_a_native_backport_marker() {
        assert_eq!(native_marker("exceed"), None);
        assert_eq!(native_marker("kart_/spectorV1/param@kr.xml"), None);
        assert_eq!(native_marker("kart_/spectorXUN/model.1s"), Some("xun"));
        assert_eq!(native_marker("Parts12Skill"), Some("parts12"));
    }

    #[test]
    fn image_metadata_is_not_treated_as_an_asset_dependency_codec() {
        let analysis = super::analyze_content(
            "track_/sample/thumbnail.png",
            "png",
            b"/unrelated/atlas/other.png XUN",
        );
        assert!(analysis.strings.is_empty());
        assert!(analysis.parse_error.is_none());
    }
}
