use std::{env, error::Error, fs, path::Path};

use p5136_core::{
    bml::{BmlLimits, BmlNode},
    packet::{PacketReader, PacketWriter},
};

fn attribute<'a>(node: &'a BmlNode, key: &str) -> Option<&'a str> {
    node.attributes
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
        .map(|(_, value)| value.as_str())
}

fn replace_track_name(node: &mut BmlNode, track_id: &str, replacement: &str) -> Vec<String> {
    let mut previous = Vec::new();
    if attribute(node, "id").is_some_and(|id| id.eq_ignore_ascii_case(track_id))
        && let Some((_, value)) = node
            .attributes
            .iter_mut()
            .find(|(name, _)| name.eq_ignore_ascii_case("name"))
    {
        previous.push(std::mem::replace(value, replacement.to_owned()));
    }
    for child in &mut node.children {
        previous.extend(replace_track_name(child, track_id, replacement));
    }
    previous
}

fn main() -> Result<(), Box<dyn Error>> {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    if arguments.len() != 4 {
        return Err(
            "usage: bml_set_track_name <input.bml> <output.bml> <track-id> <new-name>".into(),
        );
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
    let mut root = BmlNode::decode_with_limits(&mut reader, limits)?;
    if !reader.remaining().is_empty() {
        return Err(format!("{} trailing bytes remain", reader.remaining().len()).into());
    }
    let previous = replace_track_name(&mut root, &arguments[2], &arguments[3]);
    if previous.len() != 1 {
        return Err(format!(
            "track {:?} matched {} locale nodes",
            arguments[2],
            previous.len()
        )
        .into());
    }
    let mut writer = PacketWriter::new();
    root.encode_with_limits(&mut writer, limits)?;
    fs::write(output_path, writer.as_slice())?;
    println!("track_id={:?}", arguments[2]);
    println!("old_name={:?}", previous[0]);
    println!("new_name={:?}", arguments[3]);
    println!(
        "input_bytes={} output_bytes={}",
        input.len(),
        writer.as_slice().len()
    );
    Ok(())
}
