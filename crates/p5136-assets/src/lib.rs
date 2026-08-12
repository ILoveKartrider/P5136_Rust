use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};

#[allow(dead_code)]
mod asset_import;
#[allow(dead_code)]
mod asset_index;
#[allow(dead_code)]
mod bundle;
#[allow(dead_code)]
mod planner;
#[allow(dead_code)]
mod track_bundle;

const COMPATIBILITY_ASSERTION: &str = "p5136-static-verified-v1";

pub use asset_import::{
    AssetCandidate, AssetCategory, AssetImportOptions, AssetImportPhase, AssetImportProgress,
    AssetImportSummary, AssetSelection, discover_asset_candidates,
    discover_asset_candidates_with_progress, import_assets_to_dataraw,
    import_assets_to_dataraw_with_progress,
};

pub use track_bundle::{
    TrackCandidate, TrackImportOptions, TrackImportPhase, TrackImportProgress, TrackImportSummary,
    TrackSourceRegion, discover_track_candidates, discover_track_candidates_with_progress,
    import_tracks_to_dataraw, import_tracks_to_dataraw_with_progress,
};

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

fn verify_sha256(bytes: &[u8], expected: &str, label: &str) -> Result<()> {
    use sha2::{Digest, Sha256};

    let actual = format!("{:x}", Sha256::digest(bytes));
    ensure!(
        actual.eq_ignore_ascii_case(expected),
        "SHA-256 mismatch for {label}: expected {expected}, got {actual}"
    );
    Ok(())
}
