//! Bounded codec for the recursive BML tree embedded in `PqLogin`.

use thiserror::Error;

use crate::packet::{PacketError, PacketReader, PacketWriter};

pub const DEFAULT_MAX_DEPTH: usize = 8;
pub const DEFAULT_MAX_NODES: usize = 64;
pub const DEFAULT_MAX_ATTRIBUTES_PER_NODE: usize = 64;
pub const DEFAULT_MAX_CHILDREN_PER_NODE: usize = 64;
pub const DEFAULT_MAX_STRING_CODE_UNITS: usize = 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BmlLimits {
    pub max_depth: usize,
    pub max_nodes: usize,
    pub max_attributes_per_node: usize,
    pub max_children_per_node: usize,
    pub max_string_code_units: usize,
}

impl Default for BmlLimits {
    fn default() -> Self {
        Self {
            max_depth: DEFAULT_MAX_DEPTH,
            max_nodes: DEFAULT_MAX_NODES,
            max_attributes_per_node: DEFAULT_MAX_ATTRIBUTES_PER_NODE,
            max_children_per_node: DEFAULT_MAX_CHILDREN_PER_NODE,
            max_string_code_units: DEFAULT_MAX_STRING_CODE_UNITS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BmlNode {
    pub name: String,
    pub value: String,
    pub attributes: Vec<(String, String)>,
    pub children: Vec<Self>,
}

impl BmlNode {
    #[must_use]
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
            attributes: Vec::new(),
            children: Vec::new(),
        }
    }

    pub fn decode(reader: &mut PacketReader<'_>) -> Result<Self, BmlError> {
        Self::decode_with_limits(reader, BmlLimits::default())
    }

    pub fn decode_with_limits(
        reader: &mut PacketReader<'_>,
        limits: BmlLimits,
    ) -> Result<Self, BmlError> {
        let mut node_count = 0;
        decode_node(reader, limits, 0, &mut node_count)
    }

    pub fn encode(&self, writer: &mut PacketWriter) -> Result<(), BmlError> {
        self.encode_with_limits(writer, BmlLimits::default())
    }

    pub fn encode_with_limits(
        &self,
        writer: &mut PacketWriter,
        limits: BmlLimits,
    ) -> Result<(), BmlError> {
        let mut node_count = 0;
        encode_node(self, writer, limits, 0, &mut node_count)
    }

    #[must_use]
    pub fn first_value_named(&self, name: &str) -> Option<&str> {
        if self.name.eq_ignore_ascii_case(name) {
            return Some(&self.value);
        }
        self.children
            .iter()
            .find_map(|child| child.first_value_named(name))
    }
}

#[derive(Debug, Error)]
pub enum BmlError {
    #[error(transparent)]
    Packet(#[from] PacketError),

    #[error("BML depth {depth} exceeds configured maximum {maximum}")]
    DepthExceeded { depth: usize, maximum: usize },

    #[error("BML node count exceeds configured maximum {maximum}")]
    NodeLimitExceeded { maximum: usize },

    #[error("BML {kind} count {count} is outside 0..={maximum}")]
    InvalidCount {
        kind: &'static str,
        count: i32,
        maximum: usize,
    },

    #[error("BML {kind} count {count} does not fit in its i32 wire field")]
    CountOverflow { kind: &'static str, count: usize },

    #[error("BML {field} has {length} UTF-16 code units; configured maximum is {maximum}")]
    StringLimitExceeded {
        field: &'static str,
        length: usize,
        maximum: usize,
    },
}

fn decode_node(
    reader: &mut PacketReader<'_>,
    limits: BmlLimits,
    depth: usize,
    node_count: &mut usize,
) -> Result<BmlNode, BmlError> {
    check_node_admission(limits, depth, node_count)?;

    let name = reader.read_utf16_bounded(limits.max_string_code_units)?;
    let value = reader.read_utf16_bounded(limits.max_string_code_units)?;

    let attribute_count = read_count(reader, "attribute", limits.max_attributes_per_node)?;
    let mut attributes = Vec::with_capacity(attribute_count);
    for _ in 0..attribute_count {
        let attribute_name = reader.read_utf16_bounded(limits.max_string_code_units)?;
        let attribute_value = reader.read_utf16_bounded(limits.max_string_code_units)?;
        attributes.push((attribute_name, attribute_value));
    }

    let child_count = read_count(reader, "child", limits.max_children_per_node)?;
    let mut children = Vec::with_capacity(child_count);
    for _ in 0..child_count {
        children.push(decode_node(reader, limits, depth + 1, node_count)?);
    }

    Ok(BmlNode {
        name,
        value,
        attributes,
        children,
    })
}

fn encode_node(
    node: &BmlNode,
    writer: &mut PacketWriter,
    limits: BmlLimits,
    depth: usize,
    node_count: &mut usize,
) -> Result<(), BmlError> {
    check_node_admission(limits, depth, node_count)?;
    write_bounded_string(writer, &node.name, "node name", limits)?;
    write_bounded_string(writer, &node.value, "node value", limits)?;

    write_count(
        writer,
        "attribute",
        node.attributes.len(),
        limits.max_attributes_per_node,
    )?;
    for (name, value) in &node.attributes {
        write_bounded_string(writer, name, "attribute name", limits)?;
        write_bounded_string(writer, value, "attribute value", limits)?;
    }

    write_count(
        writer,
        "child",
        node.children.len(),
        limits.max_children_per_node,
    )?;
    for child in &node.children {
        encode_node(child, writer, limits, depth + 1, node_count)?;
    }
    Ok(())
}

fn check_node_admission(
    limits: BmlLimits,
    depth: usize,
    node_count: &mut usize,
) -> Result<(), BmlError> {
    if depth > limits.max_depth {
        return Err(BmlError::DepthExceeded {
            depth,
            maximum: limits.max_depth,
        });
    }
    *node_count = node_count
        .checked_add(1)
        .ok_or(BmlError::NodeLimitExceeded {
            maximum: limits.max_nodes,
        })?;
    if *node_count > limits.max_nodes {
        return Err(BmlError::NodeLimitExceeded {
            maximum: limits.max_nodes,
        });
    }
    Ok(())
}

fn read_count(
    reader: &mut PacketReader<'_>,
    kind: &'static str,
    maximum: usize,
) -> Result<usize, BmlError> {
    let count = reader.read_i32()?;
    let Ok(unsigned) = usize::try_from(count) else {
        return Err(BmlError::InvalidCount {
            kind,
            count,
            maximum,
        });
    };
    if unsigned > maximum {
        return Err(BmlError::InvalidCount {
            kind,
            count,
            maximum,
        });
    }
    Ok(unsigned)
}

fn write_count(
    writer: &mut PacketWriter,
    kind: &'static str,
    count: usize,
    maximum: usize,
) -> Result<(), BmlError> {
    if count > maximum {
        return Err(BmlError::InvalidCount {
            kind,
            count: i32::try_from(count).unwrap_or(i32::MAX),
            maximum,
        });
    }
    let signed = i32::try_from(count).map_err(|_| BmlError::CountOverflow { kind, count })?;
    writer.write_i32(signed);
    Ok(())
}

fn write_bounded_string(
    writer: &mut PacketWriter,
    value: &str,
    field: &'static str,
    limits: BmlLimits,
) -> Result<(), BmlError> {
    let length = value.encode_utf16().count();
    if length > limits.max_string_code_units {
        return Err(BmlError::StringLimitExceeded {
            field,
            length,
            maximum: limits.max_string_code_units,
        });
    }
    writer.write_utf16(value)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{BmlError, BmlLimits, BmlNode};
    use crate::packet::{PacketReader, PacketWriter};

    const LOGIN_PROFILE_BML: &[u8] = &[
        0x07, 0x00, 0x00, 0x00, 0x70, 0x00, 0x72, 0x00, 0x6f, 0x00, 0x66, 0x00, 0x69, 0x00, 0x6c,
        0x00, 0x65, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
        0x08, 0x00, 0x00, 0x00, 0x75, 0x00, 0x73, 0x00, 0x65, 0x00, 0x72, 0x00, 0x6e, 0x00, 0x61,
        0x00, 0x6d, 0x00, 0x65, 0x00, 0x05, 0x00, 0x00, 0x00, 0x59, 0x00, 0x61, 0x00, 0x6e, 0x00,
        0x79, 0x00, 0x32, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    #[test]
    fn matches_the_csharp_login_profile_layout() {
        let mut reader = PacketReader::new(LOGIN_PROFILE_BML);
        let profile = BmlNode::decode(&mut reader).unwrap();

        assert_eq!(profile.name, "profile");
        assert_eq!(profile.first_value_named("USERNAME"), Some("Yany2"));
        assert!(reader.remaining().is_empty());

        let mut writer = PacketWriter::new();
        profile.encode(&mut writer).unwrap();
        assert_eq!(writer.as_slice(), LOGIN_PROFILE_BML);
    }

    #[test]
    fn rejects_an_excessive_string_before_reading_its_payload() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1_025_i32.to_le_bytes());
        let mut reader = PacketReader::new(&bytes);
        assert!(matches!(
            BmlNode::decode(&mut reader),
            Err(BmlError::Packet(
                crate::packet::PacketError::StringLimitExceeded {
                    length: 1_025,
                    maximum: 1_024
                }
            ))
        ));
        assert_eq!(reader.position(), 4);
    }

    #[test]
    fn parser_and_writer_enforce_depth_and_node_limits() {
        let mut root = BmlNode::new("root", "");
        root.children.push(BmlNode::new("child", ""));

        let limits = BmlLimits {
            max_depth: 0,
            ..BmlLimits::default()
        };
        let mut writer = PacketWriter::new();
        assert!(matches!(
            root.encode_with_limits(&mut writer, limits),
            Err(BmlError::DepthExceeded {
                depth: 1,
                maximum: 0
            })
        ));

        let limits = BmlLimits {
            max_nodes: 1,
            ..BmlLimits::default()
        };
        let mut writer = PacketWriter::new();
        assert!(matches!(
            root.encode_with_limits(&mut writer, limits),
            Err(BmlError::NodeLimitExceeded { maximum: 1 })
        ));
    }
}
