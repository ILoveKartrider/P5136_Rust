use crate::{
    codec_error::PinCodecError,
    limits::CodecLimits,
    wire::{WireReader, WireWriter, enforce_limit, reserve_items},
};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BmlObject {
    pub name: String,
    pub value: String,
    /// Attribute order is retained because the C# serializer emits insertion
    /// order and real PIN files may rely on byte-stable unknown fields.
    pub attributes: Vec<(String, String)>,
    pub children: Vec<BmlObject>,
}

impl BmlObject {
    #[must_use]
    pub fn remove_direct_children_named(&mut self, name: &str) -> usize {
        let before = self.children.len();
        self.children
            .retain(|child| !child.name.eq_ignore_ascii_case(name));
        before - self.children.len()
    }
}

pub(crate) struct BmlBudget {
    nodes: usize,
}

impl BmlBudget {
    pub(crate) const fn new() -> Self {
        Self { nodes: 0 }
    }

    fn enter(&mut self, depth: usize, limits: &CodecLimits) -> Result<(), PinCodecError> {
        enforce_limit("BML depth", depth, limits.max_bml_depth)?;
        self.nodes = self
            .nodes
            .checked_add(1)
            .ok_or(PinCodecError::LengthOverflow("BML node count"))?;
        enforce_limit("BML node count", self.nodes, limits.max_bml_nodes)
    }
}

pub(crate) fn read_optional_bml(
    reader: &mut WireReader<'_>,
    limits: &CodecLimits,
    budget: &mut BmlBudget,
) -> Result<Option<BmlObject>, PinCodecError> {
    if reader.read_bool()? {
        read_bml(reader, limits, budget, 0).map(Some)
    } else {
        Ok(None)
    }
}

pub(crate) fn write_optional_bml(
    writer: &mut WireWriter,
    value: Option<&BmlObject>,
    limits: &CodecLimits,
    budget: &mut BmlBudget,
) -> Result<(), PinCodecError> {
    writer.write_bool(value.is_some())?;
    if let Some(value) = value {
        write_bml(writer, value, limits, budget, 0)?;
    }
    Ok(())
}

fn read_bml(
    reader: &mut WireReader<'_>,
    limits: &CodecLimits,
    budget: &mut BmlBudget,
    depth: usize,
) -> Result<BmlObject, PinCodecError> {
    budget.enter(depth, limits)?;
    let name = reader.read_string(limits)?;
    let value = reader.read_string(limits)?;

    let attribute_count = reader.read_count("BML attribute count", limits.max_collection_items)?;
    let mut attributes = Vec::new();
    reserve_items(&mut attributes, attribute_count, "BML attributes")?;
    for _ in 0..attribute_count {
        let key = reader.read_string(limits)?;
        if attributes.iter().any(|(existing, _)| existing == &key) {
            return Err(PinCodecError::DuplicateBmlAttribute(key));
        }
        let value = reader.read_string(limits)?;
        attributes.push((key, value));
    }

    let child_count = reader.read_count("BML child count", limits.max_collection_items)?;
    let mut children = Vec::new();
    reserve_items(&mut children, child_count, "BML children")?;
    let child_depth = depth
        .checked_add(1)
        .ok_or(PinCodecError::LengthOverflow("BML depth"))?;
    for _ in 0..child_count {
        children.push(read_bml(reader, limits, budget, child_depth)?);
    }

    Ok(BmlObject {
        name,
        value,
        attributes,
        children,
    })
}

fn write_bml(
    writer: &mut WireWriter,
    value: &BmlObject,
    limits: &CodecLimits,
    budget: &mut BmlBudget,
    depth: usize,
) -> Result<(), PinCodecError> {
    budget.enter(depth, limits)?;
    writer.write_string(&value.name, limits)?;
    writer.write_string(&value.value, limits)?;
    writer.write_count(
        value.attributes.len(),
        "BML attribute count",
        limits.max_collection_items,
    )?;
    for (key, value) in &value.attributes {
        writer.write_string(key, limits)?;
        writer.write_string(value, limits)?;
    }

    writer.write_count(
        value.children.len(),
        "BML child count",
        limits.max_collection_items,
    )?;
    let child_depth = depth
        .checked_add(1)
        .ok_or(PinCodecError::LengthOverflow("BML depth"))?;
    for child in &value.children {
        write_bml(writer, child, limits, budget, child_depth)?;
    }
    Ok(())
}
