use std::{env, error::Error, fs, path::Path};

use p5136_core::{
    bml::{BmlLimits, BmlNode},
    packet::{PacketReader, PacketWriter},
};

fn main() -> Result<(), Box<dyn Error>> {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    if arguments.len() != 2 {
        return Err("usage: bml_roundtrip <input.bml> <output.bml>".into());
    }
    let input_path = Path::new(&arguments[0]);
    let output_path = Path::new(&arguments[1]);
    let input = fs::read(input_path)?;
    let limits = BmlLimits {
        max_depth: 32,
        max_nodes: 200_000,
        max_attributes_per_node: 512,
        max_children_per_node: 100_000,
        max_string_code_units: 8_192,
    };
    let mut reader = PacketReader::new(&input);
    let root = BmlNode::decode_with_limits(&mut reader, limits)?;
    if !reader.remaining().is_empty() {
        return Err(format!("{} trailing bytes remain", reader.remaining().len()).into());
    }
    let mut writer = PacketWriter::new();
    root.encode_with_limits(&mut writer, limits)?;
    fs::write(output_path, writer.as_slice())?;
    println!(
        "decoded root={:?} children={} input={} output={} exact={}",
        root.name,
        root.children.len(),
        input.len(),
        writer.as_slice().len(),
        input == writer.as_slice()
    );
    Ok(())
}
