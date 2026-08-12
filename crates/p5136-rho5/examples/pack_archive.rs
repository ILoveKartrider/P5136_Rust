use std::{env, error::Error, fs, path::Path};

use p5136_rho5::{P5136_PACKED_ENTRY_FLAGS, Rho5Limits, Rho5Region, Rho5WriteEntry, Rho5Writer};

fn main() -> Result<(), Box<dyn Error>> {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    if arguments.len() < 2 {
        return Err("usage: pack_archive <output.rho5> <virtual-path>=<input-file> [...]".into());
    }
    let output = Path::new(&arguments[0]);
    let output_name = output
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("output file name is not valid UTF-8")?;
    let mut writer = Rho5Writer::new();
    for mapping in &arguments[1..] {
        let (virtual_path, input_file) = mapping
            .split_once('=')
            .ok_or_else(|| format!("mapping {mapping:?} has no '='"))?;
        writer.add(Rho5WriteEntry {
            path: virtual_path.to_owned(),
            data: fs::read(input_file)?,
            flags: P5136_PACKED_ENTRY_FLAGS,
        });
    }
    let encoded = writer.encode(output_name, Rho5Region::Korea, &Rho5Limits::default())?;
    fs::write(output, encoded.as_bytes())?;
    println!(
        "wrote {} entries to {}",
        encoded.entry_count(),
        output.display()
    );
    Ok(())
}
