use std::{
    collections::BTreeMap,
    env,
    fmt::Write as _,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use p5136_rho5::{Rho5Directory, Rho5Limits};

fn compact_hex(bytes: &[u8]) -> String {
    bytes.iter().fold(
        String::with_capacity(bytes.len().saturating_mul(2)),
        |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to String cannot fail");
            output
        },
    )
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args().skip(1);
    let data = arguments
        .next()
        .ok_or("usage: inspect_archive <Data> <archive>")?;
    let archive = arguments
        .next()
        .ok_or("usage: inspect_archive <Data> <archive>")?;
    let requested_path = arguments.next();
    let output_path = arguments.next();
    let directory = Rho5Directory::scan_kr(Path::new(&data), Rho5Limits::default())?;
    let entries = directory
        .entries()
        .iter()
        .filter(|entry| archive == "*" || entry.archive_name().eq_ignore_ascii_case(&archive))
        .collect::<Vec<_>>();
    let mut flags = BTreeMap::<i32, usize>::new();
    let mut raw_path_differences = 0_usize;
    for entry in &entries {
        *flags.entry(entry.flags()).or_default() += 1;
        raw_path_differences += usize::from(entry.raw_path() != entry.normalized_path());
    }
    println!("archive={archive} entries={}", entries.len());
    println!("raw_path_differences={raw_path_differences}");
    for (value, count) in flags {
        println!("flags={value:#010x} count={count}");
    }
    if let Some(requested_path) = requested_path {
        for entry in entries.into_iter().filter(|entry| {
            entry
                .normalized_path()
                .eq_ignore_ascii_case(&requested_path)
        }) {
            let mut file = File::open(entry.archive_path())?;
            file.seek(SeekFrom::Start(entry.physical_data_offset()))?;
            let mut raw = vec![0_u8; entry.compressed_size().min(64)];
            file.read_exact(&mut raw)?;
            let plaintext = directory.extract_entry_with_legacy_padding(entry)?;
            if let Some(output_path) = output_path.as_deref() {
                fs::write(output_path, &plaintext)?;
                println!("wrote={output_path:?}");
            }
            println!(
                "path={:?} raw_path={:?} offset={:#x} compressed={} plaintext={} md5={} flags={:#010x} raw_head={} plaintext_head={}",
                entry.normalized_path(),
                entry.raw_path(),
                entry.physical_data_offset(),
                entry.compressed_size(),
                entry.plaintext_size(),
                compact_hex(&entry.plaintext_md5()),
                entry.flags(),
                raw.iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<Vec<_>>()
                    .join(" "),
                plaintext
                    .iter()
                    .take(64)
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
    }
    Ok(())
}
