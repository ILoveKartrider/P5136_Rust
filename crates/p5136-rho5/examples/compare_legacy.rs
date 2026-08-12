use std::{collections::BTreeSet, env, path::Path};

use p5136_rho5::{LegacyRhoArchive, LegacyRhoLimits};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let left_path = args
        .next()
        .ok_or("usage: compare_legacy <left.rho> <right.rho> [contains]")?;
    let right_path = args
        .next()
        .ok_or("usage: compare_legacy <left.rho> <right.rho> [contains]")?;
    let needle = args.next().map(|value| value.to_ascii_lowercase());
    let limits = LegacyRhoLimits {
        max_archive_bytes: 1024 * 1024 * 1024,
        max_blocks: 1_000_000,
        max_compressed_block_bytes: 512 * 1024 * 1024,
        max_plaintext_block_bytes: 512 * 1024 * 1024,
        max_entries_per_directory: 250_000,
        max_path_components: 64,
        max_name_utf16_units: 4_096,
    };
    let left = LegacyRhoArchive::open(Path::new(&left_path), limits)?;
    let right = LegacyRhoArchive::open(Path::new(&right_path), limits)?;
    let selected = |path: &str| {
        needle
            .as_ref()
            .is_none_or(|needle| path.to_ascii_lowercase().contains(needle))
    };
    let left_paths = left
        .entries()?
        .into_iter()
        .map(|entry| entry.normalized_path().to_owned())
        .filter(|path| selected(path))
        .collect::<BTreeSet<_>>();
    let right_paths = right
        .entries()?
        .into_iter()
        .map(|entry| entry.normalized_path().to_owned())
        .filter(|path| selected(path))
        .collect::<BTreeSet<_>>();

    let mut identical = 0_usize;
    let mut changed = Vec::new();
    let mut changed_same_image_layout = 0_usize;
    let mut changed_image_layout = Vec::new();
    for path in left_paths.intersection(&right_paths) {
        let left_bytes = left.extract_exact(path)?;
        let right_bytes = right.extract_exact(path)?;
        if left_bytes == right_bytes {
            identical += 1;
        } else {
            match (image_layout(&left_bytes), image_layout(&right_bytes)) {
                (Some(left_layout), Some(right_layout)) if left_layout == right_layout => {
                    changed_same_image_layout += 1;
                }
                (Some(left_layout), Some(right_layout)) => {
                    changed_image_layout.push(format!(
                        "{path}\tleft={left_layout:?}\tright={right_layout:?}"
                    ));
                }
                _ => {}
            }
            changed.push(path.clone());
        }
    }
    let left_only = left_paths
        .difference(&right_paths)
        .cloned()
        .collect::<Vec<_>>();
    let right_only = right_paths
        .difference(&left_paths)
        .cloned()
        .collect::<Vec<_>>();
    println!(
        "left={} right={} common={} identical={} changed={} changed_same_image_layout={} changed_image_layout={} left_only={} right_only={}",
        left_paths.len(),
        right_paths.len(),
        identical + changed.len(),
        identical,
        changed.len(),
        changed_same_image_layout,
        changed_image_layout.len(),
        left_only.len(),
        right_only.len()
    );
    for path in &changed {
        println!("changed\t{path}");
    }
    for detail in &changed_image_layout {
        println!("image_layout_changed\t{detail}");
    }
    for path in &left_only {
        println!("left_only\t{path}");
    }
    for path in &right_only {
        println!("right_only\t{path}");
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum ImageLayout {
    Png {
        width: u32,
        height: u32,
        bit_depth: u8,
        color_type: u8,
    },
    Dds {
        width: u32,
        height: u32,
        mipmaps: u32,
        pixel_flags: u32,
        four_cc: [u8; 4],
        rgb_bits: u32,
    },
}

fn image_layout(bytes: &[u8]) -> Option<ImageLayout> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.len() >= 26 {
        return Some(ImageLayout::Png {
            width: u32::from_be_bytes(bytes[16..20].try_into().ok()?),
            height: u32::from_be_bytes(bytes[20..24].try_into().ok()?),
            bit_depth: bytes[24],
            color_type: bytes[25],
        });
    }
    if bytes.starts_with(b"DDS ") && bytes.len() >= 92 {
        return Some(ImageLayout::Dds {
            width: u32::from_le_bytes(bytes[16..20].try_into().ok()?),
            height: u32::from_le_bytes(bytes[12..16].try_into().ok()?),
            mipmaps: u32::from_le_bytes(bytes[28..32].try_into().ok()?),
            pixel_flags: u32::from_le_bytes(bytes[80..84].try_into().ok()?),
            four_cc: bytes[84..88].try_into().ok()?,
            rgb_bits: u32::from_le_bytes(bytes[88..92].try_into().ok()?),
        });
    }
    None
}
