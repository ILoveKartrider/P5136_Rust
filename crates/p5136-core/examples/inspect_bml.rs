use std::{env, fs};

use p5136_core::{
    bml::{BmlLimits, BmlNode},
    packet::PacketReader,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let path = args
        .next()
        .ok_or("usage: inspect_bml <file> [minimum-kart-id]")?;
    let minimum_kart_id = args.next().map(|value| value.parse::<u16>()).transpose()?;
    let bytes = fs::read(path)?;
    let mut reader = PacketReader::new(&bytes);
    let root = BmlNode::decode_with_limits(
        &mut reader,
        BmlLimits {
            max_depth: 32,
            max_nodes: 200_000,
            max_attributes_per_node: 512,
            max_children_per_node: 100_000,
            max_string_code_units: 8_192,
        },
    )?;
    if !reader.remaining().is_empty() {
        return Err(format!("{} trailing bytes", reader.remaining().len()).into());
    }

    let mut matched = 0_usize;
    let mut pending = vec![(&root, 0_usize)];
    while let Some((node, depth)) = pending.pop() {
        pending.extend(node.children.iter().rev().map(|child| (child, depth + 1)));
        let kart_id = node
            .attributes
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("kartId"))
            .and_then(|(_, value)| value.parse::<u16>().ok());
        if minimum_kart_id.is_some_and(|minimum| kart_id.is_none_or(|id| id < minimum)) {
            continue;
        }
        if minimum_kart_id.is_none() || kart_id.is_some() {
            println!(
                "{}{} value={:?} attrs={:?}",
                "  ".repeat(depth),
                node.name,
                node.value,
                node.attributes
            );
            matched += 1;
        }
    }
    println!("matched={matched}");
    Ok(())
}
