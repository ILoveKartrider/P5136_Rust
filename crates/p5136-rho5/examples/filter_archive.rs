use std::{env, error::Error, fs, path::Path};

use p5136_rho5::{Rho5Directory, Rho5Limits, Rho5Region, Rho5WriteEntry, Rho5Writer};

fn main() -> Result<(), Box<dyn Error>> {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    if arguments.len() < 4 {
        return Err(
            "usage: filter_archive <input-dir> <input-archive> <output-file> <entry> [entry ...]"
                .into(),
        );
    }
    let input_directory = Path::new(&arguments[0]);
    let input_archive = &arguments[1];
    let output_file = Path::new(&arguments[2]);
    let requested = &arguments[3..];

    let limits = Rho5Limits::default();
    let directory = Rho5Directory::scan_kr(input_directory, limits.clone())?;
    let mut writer = Rho5Writer::new();
    for requested_path in requested {
        let entry = directory
            .entries()
            .iter()
            .find(|entry| {
                entry.archive_name().eq_ignore_ascii_case(input_archive)
                    && entry.normalized_path().eq_ignore_ascii_case(requested_path)
            })
            .ok_or_else(|| format!("entry {requested_path:?} was not found in {input_archive}"))?;
        writer.add(Rho5WriteEntry {
            path: entry.raw_path().to_owned(),
            data: directory.extract_entry(entry)?,
            flags: entry.flags(),
        });
    }
    let output_name = output_file
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("output file name is not valid UTF-8")?;
    let encoded = writer.encode(output_name, Rho5Region::Korea, &limits)?;
    fs::write(output_file, encoded.as_bytes())?;
    println!(
        "wrote {} entries to {}",
        encoded.entry_count(),
        output_file.display()
    );
    Ok(())
}
