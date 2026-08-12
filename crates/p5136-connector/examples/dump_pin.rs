use std::{env, fs, path::Path};

use p5136_connector::{BmlObject, PinDocument};

fn dump_bml(label: &str, node: &BmlObject, depth: usize) {
    let indent = "  ".repeat(depth);
    println!("{indent}{label}<{}> value={:?}", node.name, node.value);
    for (key, value) in &node.attributes {
        println!("{indent}  @{key}={value:?}");
    }
    for child in &node.children {
        dump_bml("", child, depth + 1);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = env::args()
        .nth(1)
        .ok_or("usage: dump_pin <KartRider.pin>")?;
    let bytes = fs::read(&path)?;
    let pin = PinDocument::decode(&bytes)?;
    println!("path={} bytes={}", Path::new(&path).display(), bytes.len());
    println!("header={:?}", pin.header);
    for (index, auth) in pin.auth_methods.iter().enumerate() {
        println!(
            "auth[{index}] index={} name={:?} endpoints={:?}",
            auth.index, auth.name, auth.login_servers
        );
        if let Some(node) = &auth.account_config {
            dump_bml("account_config=", node, 1);
        }
        if let Some(node) = &auth.extra_config {
            dump_bml("auth_extra=", node, 1);
        }
    }
    if let Some(node) = &pin.storage_config {
        dump_bml("storage_config=", node, 0);
    }
    if let Some(node) = &pin.extra_config {
        dump_bml("extra_config=", node, 0);
    }
    println!(
        "trailing_payload={} trailing_envelope={}",
        pin.trailing_payload().len(),
        pin.trailing_envelope().len()
    );
    Ok(())
}
