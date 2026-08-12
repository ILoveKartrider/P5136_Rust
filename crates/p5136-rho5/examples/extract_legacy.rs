use std::{env, fs, path::Path};

use p5136_rho5::{LegacyRhoArchive, LegacyRhoLimits};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let archive_path = env::args()
        .nth(1)
        .ok_or("usage: extract_legacy <archive.rho> <virtual-path> <output>")?;
    let virtual_path = env::args()
        .nth(2)
        .ok_or("usage: extract_legacy <archive.rho> <virtual-path> <output>")?;
    let output_path = env::args()
        .nth(3)
        .ok_or("usage: extract_legacy <archive.rho> <virtual-path> <output>")?;

    let limits = LegacyRhoLimits {
        max_archive_bytes: 512 * 1024 * 1024,
        max_plaintext_block_bytes: 128 * 1024 * 1024,
        ..LegacyRhoLimits::default()
    };
    let archive = LegacyRhoArchive::open(Path::new(&archive_path), limits)?;
    let bytes = archive.extract_exact(&virtual_path)?;
    if let Some(parent) = Path::new(&output_path).parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output_path, &bytes)?;
    println!("archive={archive_path}");
    println!("virtual_path={virtual_path}");
    println!("output={output_path}");
    println!("bytes={}", bytes.len());
    Ok(())
}
