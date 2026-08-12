//! Bounded codec for the legacy `Data/aaa.pk` pack-folder index.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use flate2::{Compression, bufread::ZlibDecoder, write::ZlibEncoder};
use std::io::Write as _;
use thiserror::Error;

use crate::legacy::{adler32, decrypt_data};

const KR_DATA_MAGIC: u8 = 0x53;
const KR_DATA_COMPRESSED: u8 = 1;
const KR_DATA_ENCRYPTED: u8 = 2;

/// Resource limits for `aaa.pk` and its recursive binary-XML tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AaaLimits {
    pub max_file_bytes: usize,
    pub max_plaintext_bytes: usize,
    pub max_nodes: usize,
    pub max_depth: usize,
    pub max_attributes_per_node: usize,
    pub max_children_per_node: usize,
    pub max_string_utf16_units: usize,
}

impl Default for AaaLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 16 * 1024 * 1024,
            max_plaintext_bytes: 64 * 1024 * 1024,
            max_nodes: 1_000_000,
            max_depth: 128,
            max_attributes_per_node: 256,
            max_children_per_node: 1_000_000,
            max_string_utf16_units: 16 * 1024,
        }
    }
}

/// One binary-XML node. Vectors intentionally preserve on-disk order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AaaNode {
    pub name: String,
    pub text: String,
    pub attributes: Vec<(String, String)>,
    pub children: Vec<AaaNode>,
}

impl AaaNode {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            text: String::new(),
            attributes: Vec::new(),
            children: Vec::new(),
        }
    }

    #[must_use]
    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find_map(|(candidate, value)| (candidate == name).then_some(value.as_str()))
    }

    pub fn set_attribute(&mut self, name: impl Into<String>, value: impl Into<String>) {
        let name = name.into();
        let value = value.into();
        if let Some((_, existing)) = self
            .attributes
            .iter_mut()
            .find(|(candidate, _)| *candidate == name)
        {
            *existing = value;
        } else {
            self.attributes.push((name, value));
        }
    }

    /// Decodes the raw binary-XML representation used inside KRData/BML files.
    pub fn decode_binary_xml(bytes: &[u8], limits: AaaLimits) -> Result<Self, AaaError> {
        validate_limits(limits)?;
        if bytes.len() > limits.max_plaintext_bytes {
            return Err(AaaError::PlaintextTooLarge {
                actual: bytes.len(),
                maximum: limits.max_plaintext_bytes,
            });
        }
        let mut reader = AaaReader::new(bytes);
        let mut node_count = 0_usize;
        let node = decode_node(&mut reader, limits, 0, &mut node_count)?;
        if reader.remaining() != 0 {
            return Err(AaaError::TrailingBinaryXml(reader.remaining()));
        }
        Ok(node)
    }

    /// Encodes raw binary XML without a `KRData` wrapper.
    pub fn encode_binary_xml(&self, limits: AaaLimits) -> Result<Vec<u8>, AaaError> {
        validate_limits(limits)?;
        let mut output = Vec::new();
        let mut node_count = 0_usize;
        encode_node(self, &mut output, limits, 0, &mut node_count)?;
        if output.len() > limits.max_plaintext_bytes {
            return Err(AaaError::PlaintextTooLarge {
                actual: output.len(),
                maximum: limits.max_plaintext_bytes,
            });
        }
        Ok(output)
    }
}

/// Metadata stored in one `RhoFolder` child node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AaaRhoFolder {
    pub name: String,
    pub file_name: String,
    pub key: u32,
    pub data_hash: u32,
    pub media_size: u64,
}

/// One legacy RHO mount in the order declared by `aaa.pk`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AaaRhoMount {
    pub pack_path: Vec<String>,
    pub folder: AaaRhoFolder,
}

impl AaaRhoMount {
    /// Returns the virtual path prepended to paths stored inside the RHO.
    #[must_use]
    pub fn virtual_prefix(&self) -> String {
        let mut components = self.pack_path.clone();
        if !self.folder.name.is_empty() {
            components.push(self.folder.name.clone());
        }
        components.join("/")
    }
}

/// Parsed `aaa.pk` document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AaaDocument {
    pub root: AaaNode,
}

impl AaaDocument {
    pub fn read(path: impl AsRef<Path>, limits: AaaLimits) -> Result<Self, AaaError> {
        validate_limits(limits)?;
        let path = path.as_ref();
        let bytes = fs::read(path).map_err(|source| AaaError::Io {
            operation: "read aaa.pk",
            path: path.to_path_buf(),
            source,
        })?;
        if bytes.len() > limits.max_file_bytes {
            return Err(AaaError::FileTooLarge {
                actual: bytes.len(),
                maximum: limits.max_file_bytes,
            });
        }
        Self::decode(&bytes, limits)
    }

    pub fn decode(bytes: &[u8], limits: AaaLimits) -> Result<Self, AaaError> {
        validate_limits(limits)?;
        if bytes.len() > limits.max_file_bytes {
            return Err(AaaError::FileTooLarge {
                actual: bytes.len(),
                maximum: limits.max_file_bytes,
            });
        }
        let mut reader = AaaReader::new(bytes);
        let payload_length = reader.nonnegative_i32("KRData payload length")?;
        if payload_length != reader.remaining() {
            return Err(AaaError::PayloadLength {
                declared: payload_length,
                actual: reader.remaining(),
            });
        }
        let payload = reader.take(payload_length, "KRData payload")?;
        let plaintext = decode_kr_data(payload, limits)?;
        let root = AaaNode::decode_binary_xml(&plaintext, limits)?;
        Ok(Self { root })
    }

    /// Encodes the stock compressed, unencrypted `KRData` representation.
    pub fn encode(&self, limits: AaaLimits) -> Result<Vec<u8>, AaaError> {
        validate_limits(limits)?;
        let plaintext = self.root.encode_binary_xml(limits)?;

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(&plaintext)
            .map_err(AaaError::Compression)?;
        let compressed = encoder.finish().map_err(AaaError::Compression)?;
        let payload_length = 1_usize
            .checked_add(1)
            .and_then(|value| value.checked_add(4))
            .and_then(|value| value.checked_add(4))
            .and_then(|value| value.checked_add(compressed.len()))
            .ok_or(AaaError::ArithmeticOverflow)?;
        let file_length = payload_length
            .checked_add(4)
            .ok_or(AaaError::ArithmeticOverflow)?;
        if file_length > limits.max_file_bytes {
            return Err(AaaError::FileTooLarge {
                actual: file_length,
                maximum: limits.max_file_bytes,
            });
        }
        let mut output = Vec::with_capacity(file_length);
        push_i32(&mut output, payload_length)?;
        output.push(KR_DATA_MAGIC);
        output.push(KR_DATA_COMPRESSED);
        output.extend_from_slice(&adler32(&plaintext).to_le_bytes());
        push_i32(&mut output, plaintext.len())?;
        output.extend_from_slice(&compressed);
        Ok(output)
    }

    pub fn write_to(&self, path: impl AsRef<Path>, limits: AaaLimits) -> Result<(), AaaError> {
        let path = path.as_ref();
        let bytes = self.encode(limits)?;
        fs::write(path, bytes).map_err(|source| AaaError::Io {
            operation: "write aaa.pk",
            path: path.to_path_buf(),
            source,
        })
    }

    /// Enumerates every legacy RHO mount using the client's `PackFolder` rules.
    pub fn rho_mounts(&self) -> Result<Vec<AaaRhoMount>, AaaError> {
        if self.root.name != "PackFolder" {
            return Err(AaaError::InvalidRoot(self.root.name.clone()));
        }
        let mut mounts = Vec::new();
        collect_rho_mounts(&self.root, &mut Vec::new(), &mut mounts)?;
        Ok(mounts)
    }

    /// Inserts or replaces one `RhoFolder` below a `PackFolder` path.
    /// Existing unrelated nodes and all node/attribute ordering are retained.
    pub fn upsert_rho_folder(
        &mut self,
        pack_path: &[&str],
        rho: &AaaRhoFolder,
    ) -> Result<(), AaaError> {
        if self.root.name != "PackFolder" {
            return Err(AaaError::InvalidRoot(self.root.name.clone()));
        }
        let mut current = &mut self.root;
        for component in pack_path {
            let position = current.children.iter().position(|child| {
                child.name == "PackFolder" && child.attribute("name") == Some(*component)
            });
            if let Some(position) = position {
                current = &mut current.children[position];
            } else {
                let mut child = AaaNode::new("PackFolder");
                child.set_attribute("name", *component);
                current.children.push(child);
                current = current
                    .children
                    .last_mut()
                    .expect("a PackFolder was just appended");
            }
        }
        let existing = current.children.iter().position(|child| {
            child.name == "RhoFolder"
                && (child.attribute("fileName") == Some(rho.file_name.as_str())
                    || (!rho.name.is_empty() && child.attribute("name") == Some(rho.name.as_str())))
        });
        let node = if let Some(position) = existing {
            &mut current.children[position]
        } else {
            current.children.push(AaaNode::new("RhoFolder"));
            current
                .children
                .last_mut()
                .expect("a RhoFolder was just appended")
        };
        node.set_attribute("name", &rho.name);
        node.set_attribute("fileName", &rho.file_name);
        node.set_attribute("key", rho.key.to_string());
        node.set_attribute("dataHash", rho.data_hash.to_string());
        node.set_attribute("mediaSize", rho.media_size.to_string());
        Ok(())
    }
}

fn collect_rho_mounts(
    node: &AaaNode,
    pack_path: &mut Vec<String>,
    mounts: &mut Vec<AaaRhoMount>,
) -> Result<(), AaaError> {
    for child in &node.children {
        match child.name.as_str() {
            "PackFolder" => {
                let name = required_attribute(child, "name")?;
                pack_path.push(name.to_owned());
                collect_rho_mounts(child, pack_path, mounts)?;
                pack_path.pop();
            }
            "RhoFolder" => mounts.push(AaaRhoMount {
                pack_path: pack_path.clone(),
                folder: AaaRhoFolder {
                    name: required_attribute(child, "name")?.to_owned(),
                    file_name: required_attribute(child, "fileName")?.to_owned(),
                    key: parse_attribute(child, "key")?,
                    data_hash: parse_attribute(child, "dataHash")?,
                    media_size: parse_attribute(child, "mediaSize")?,
                },
            }),
            _ => {}
        }
    }
    Ok(())
}

fn required_attribute<'a>(node: &'a AaaNode, name: &'static str) -> Result<&'a str, AaaError> {
    node.attribute(name)
        .ok_or_else(|| AaaError::MissingAttribute {
            node: node.name.clone(),
            attribute: name,
        })
}

fn parse_attribute<T>(node: &AaaNode, name: &'static str) -> Result<T, AaaError>
where
    T: std::str::FromStr,
{
    let value = required_attribute(node, name)?;
    value.parse().map_err(|_| AaaError::InvalidAttribute {
        node: node.name.clone(),
        attribute: name,
        value: value.to_owned(),
    })
}

#[derive(Debug, Error)]
pub enum AaaError {
    #[error("failed to {operation} at {path}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("aaa.pk limit {0} must be nonzero")]
    InvalidLimit(&'static str),
    #[error("aaa.pk has {actual} bytes; maximum is {maximum}")]
    FileTooLarge { actual: usize, maximum: usize },
    #[error("aaa.pk KRData payload declares {declared} bytes; actual is {actual}")]
    PayloadLength { declared: usize, actual: usize },
    #[error("aaa.pk is truncated while reading {context}: need {needed}, remaining {remaining}")]
    Truncated {
        context: &'static str,
        needed: usize,
        remaining: usize,
    },
    #[error("aaa.pk KRData magic {0:#04x} is invalid")]
    InvalidKrDataMagic(u8),
    #[error("aaa.pk KRData mode {0:#04x} contains unsupported flags")]
    InvalidKrDataMode(u8),
    #[error("aaa.pk KRData declares negative {context}: {value}")]
    NegativeValue { context: &'static str, value: i32 },
    #[error("aaa.pk KRData plaintext has {actual} bytes; expected {expected}")]
    PlaintextLength { actual: usize, expected: usize },
    #[error("aaa.pk KRData plaintext checksum is {actual:#010x}; expected {expected:#010x}")]
    Checksum { actual: u32, expected: u32 },
    #[error("aaa.pk zlib stream consumed {consumed} of {stored} bytes")]
    TrailingCompressedBytes { consumed: usize, stored: usize },
    #[error("aaa.pk decompression failed")]
    Decompression(#[source] std::io::Error),
    #[error("aaa.pk compression failed")]
    Compression(#[source] std::io::Error),
    #[error("aaa.pk binary XML plaintext has {actual} bytes; maximum is {maximum}")]
    PlaintextTooLarge { actual: usize, maximum: usize },
    #[error("aaa.pk binary XML depth exceeds {0}")]
    TooDeep(usize),
    #[error("aaa.pk binary XML contains more than {0} nodes")]
    TooManyNodes(usize),
    #[error("aaa.pk binary XML {kind} count {actual} exceeds {maximum}")]
    CountTooLarge {
        kind: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("aaa.pk binary XML string has {actual} UTF-16 units; maximum is {maximum}")]
    StringTooLong { actual: usize, maximum: usize },
    #[error("aaa.pk binary XML contains invalid UTF-16")]
    InvalidUtf16,
    #[error("aaa.pk binary XML has {0} trailing bytes")]
    TrailingBinaryXml(usize),
    #[error("aaa.pk root is {0:?}, expected PackFolder")]
    InvalidRoot(String),
    #[error("aaa.pk node {node:?} is missing required attribute {attribute:?}")]
    MissingAttribute {
        node: String,
        attribute: &'static str,
    },
    #[error("aaa.pk node {node:?} has invalid {attribute:?} value {value:?}")]
    InvalidAttribute {
        node: String,
        attribute: &'static str,
        value: String,
    },
    #[error("aaa.pk integer conversion or offset calculation overflowed")]
    ArithmeticOverflow,
}

fn validate_limits(limits: AaaLimits) -> Result<(), AaaError> {
    for (name, value) in [
        ("max_file_bytes", limits.max_file_bytes),
        ("max_plaintext_bytes", limits.max_plaintext_bytes),
        ("max_nodes", limits.max_nodes),
        ("max_depth", limits.max_depth),
        ("max_attributes_per_node", limits.max_attributes_per_node),
        ("max_children_per_node", limits.max_children_per_node),
        ("max_string_utf16_units", limits.max_string_utf16_units),
    ] {
        if value == 0 {
            return Err(AaaError::InvalidLimit(name));
        }
    }
    Ok(())
}

fn decode_kr_data(payload: &[u8], limits: AaaLimits) -> Result<Vec<u8>, AaaError> {
    let mut reader = AaaReader::new(payload);
    let magic = reader.u8("KRData magic")?;
    if magic != KR_DATA_MAGIC {
        return Err(AaaError::InvalidKrDataMagic(magic));
    }
    let mode = reader.u8("KRData mode")?;
    if mode & !(KR_DATA_COMPRESSED | KR_DATA_ENCRYPTED) != 0 {
        return Err(AaaError::InvalidKrDataMode(mode));
    }
    let expected_checksum = reader.u32("KRData checksum")?;
    let key = if mode & KR_DATA_ENCRYPTED != 0 {
        Some(reader.u32("KRData encryption key")?)
    } else {
        None
    };
    let expected_plaintext = if mode & KR_DATA_COMPRESSED != 0 {
        Some(reader.nonnegative_i32("KRData plaintext length")?)
    } else {
        None
    };
    let mut stored = reader.take(reader.remaining(), "KRData body")?.to_vec();
    if let Some(key) = key {
        decrypt_data(&mut stored, key);
    }
    let plaintext = if let Some(expected) = expected_plaintext {
        if expected > limits.max_plaintext_bytes {
            return Err(AaaError::PlaintextTooLarge {
                actual: expected,
                maximum: limits.max_plaintext_bytes,
            });
        }
        let mut decoder = ZlibDecoder::new(stored.as_slice());
        let mut plaintext = Vec::with_capacity(expected);
        (&mut decoder)
            .take(
                u64::try_from(expected)
                    .unwrap_or(u64::MAX)
                    .saturating_add(1),
            )
            .read_to_end(&mut plaintext)
            .map_err(AaaError::Decompression)?;
        if plaintext.len() != expected {
            return Err(AaaError::PlaintextLength {
                actual: plaintext.len(),
                expected,
            });
        }
        let consumed =
            usize::try_from(decoder.total_in()).map_err(|_| AaaError::ArithmeticOverflow)?;
        if consumed != stored.len() {
            return Err(AaaError::TrailingCompressedBytes {
                consumed,
                stored: stored.len(),
            });
        }
        plaintext
    } else {
        if stored.len() > limits.max_plaintext_bytes {
            return Err(AaaError::PlaintextTooLarge {
                actual: stored.len(),
                maximum: limits.max_plaintext_bytes,
            });
        }
        stored
    };
    let actual_checksum = adler32(&plaintext);
    if actual_checksum != expected_checksum {
        return Err(AaaError::Checksum {
            actual: actual_checksum,
            expected: expected_checksum,
        });
    }
    Ok(plaintext)
}

fn decode_node(
    reader: &mut AaaReader<'_>,
    limits: AaaLimits,
    depth: usize,
    node_count: &mut usize,
) -> Result<AaaNode, AaaError> {
    if depth > limits.max_depth {
        return Err(AaaError::TooDeep(limits.max_depth));
    }
    *node_count = node_count
        .checked_add(1)
        .ok_or(AaaError::ArithmeticOverflow)?;
    if *node_count > limits.max_nodes {
        return Err(AaaError::TooManyNodes(limits.max_nodes));
    }
    let name = reader.utf16(limits.max_string_utf16_units)?;
    let text = reader.utf16(limits.max_string_utf16_units)?;
    let attribute_count = reader.bounded_count("attribute", limits.max_attributes_per_node)?;
    let mut attributes = Vec::with_capacity(attribute_count);
    for _ in 0..attribute_count {
        attributes.push((
            reader.utf16(limits.max_string_utf16_units)?,
            reader.utf16(limits.max_string_utf16_units)?,
        ));
    }
    let child_count = reader.bounded_count("child", limits.max_children_per_node)?;
    let mut children = Vec::with_capacity(child_count.min(16_384));
    for _ in 0..child_count {
        children.push(decode_node(reader, limits, depth + 1, node_count)?);
    }
    Ok(AaaNode {
        name,
        text,
        attributes,
        children,
    })
}

fn encode_node(
    node: &AaaNode,
    output: &mut Vec<u8>,
    limits: AaaLimits,
    depth: usize,
    node_count: &mut usize,
) -> Result<(), AaaError> {
    if depth > limits.max_depth {
        return Err(AaaError::TooDeep(limits.max_depth));
    }
    *node_count = node_count
        .checked_add(1)
        .ok_or(AaaError::ArithmeticOverflow)?;
    if *node_count > limits.max_nodes {
        return Err(AaaError::TooManyNodes(limits.max_nodes));
    }
    push_utf16(output, &node.name, limits)?;
    push_utf16(output, &node.text, limits)?;
    check_count(
        "attribute",
        node.attributes.len(),
        limits.max_attributes_per_node,
    )?;
    push_i32(output, node.attributes.len())?;
    for (name, value) in &node.attributes {
        push_utf16(output, name, limits)?;
        push_utf16(output, value, limits)?;
    }
    check_count("child", node.children.len(), limits.max_children_per_node)?;
    push_i32(output, node.children.len())?;
    for child in &node.children {
        encode_node(child, output, limits, depth + 1, node_count)?;
    }
    Ok(())
}

fn check_count(kind: &'static str, actual: usize, maximum: usize) -> Result<(), AaaError> {
    if actual > maximum {
        return Err(AaaError::CountTooLarge {
            kind,
            actual,
            maximum,
        });
    }
    Ok(())
}

fn push_utf16(output: &mut Vec<u8>, value: &str, limits: AaaLimits) -> Result<(), AaaError> {
    let units = value.encode_utf16().collect::<Vec<_>>();
    if units.len() > limits.max_string_utf16_units {
        return Err(AaaError::StringTooLong {
            actual: units.len(),
            maximum: limits.max_string_utf16_units,
        });
    }
    push_i32(output, units.len())?;
    for unit in units {
        output.extend_from_slice(&unit.to_le_bytes());
    }
    Ok(())
}

fn push_i32(output: &mut Vec<u8>, value: usize) -> Result<(), AaaError> {
    let value = i32::try_from(value).map_err(|_| AaaError::ArithmeticOverflow)?;
    output.extend_from_slice(&value.to_le_bytes());
    Ok(())
}

struct AaaReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> AaaReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    const fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }

    fn take(&mut self, length: usize, context: &'static str) -> Result<&'a [u8], AaaError> {
        if length > self.remaining() {
            return Err(AaaError::Truncated {
                context,
                needed: length,
                remaining: self.remaining(),
            });
        }
        let start = self.position;
        self.position += length;
        Ok(&self.bytes[start..start + length])
    }

    fn u8(&mut self, context: &'static str) -> Result<u8, AaaError> {
        Ok(self.take(1, context)?[0])
    }

    fn u32(&mut self, context: &'static str) -> Result<u32, AaaError> {
        Ok(u32::from_le_bytes(
            self.take(4, context)?.try_into().expect("four bytes"),
        ))
    }

    fn i32(&mut self, context: &'static str) -> Result<i32, AaaError> {
        Ok(i32::from_le_bytes(
            self.take(4, context)?.try_into().expect("four bytes"),
        ))
    }

    fn nonnegative_i32(&mut self, context: &'static str) -> Result<usize, AaaError> {
        let value = self.i32(context)?;
        usize::try_from(value).map_err(|_| AaaError::NegativeValue { context, value })
    }

    fn bounded_count(&mut self, kind: &'static str, maximum: usize) -> Result<usize, AaaError> {
        let actual = self.nonnegative_i32(kind)?;
        check_count(kind, actual, maximum)?;
        Ok(actual)
    }

    fn utf16(&mut self, maximum: usize) -> Result<String, AaaError> {
        let length = self.nonnegative_i32("UTF-16 string length")?;
        if length > maximum {
            return Err(AaaError::StringTooLong {
                actual: length,
                maximum,
            });
        }
        let byte_length = length.checked_mul(2).ok_or(AaaError::ArithmeticOverflow)?;
        let bytes = self.take(byte_length, "UTF-16 string")?;
        let units = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        String::from_utf16(&units).map_err(|_| AaaError::InvalidUtf16)
    }
}

#[cfg(test)]
mod tests {
    use std::{env, path::PathBuf};

    use super::{AaaDocument, AaaLimits, AaaNode, AaaRhoFolder};

    #[test]
    fn binary_xml_and_krdata_round_trip_preserves_order() {
        let mut root = AaaNode::new("PackFolder");
        root.set_attribute("name", "KartRider");
        root.set_attribute("regionCode", "kr");
        let mut kart = AaaNode::new("PackFolder");
        kart.set_attribute("name", "kart");
        kart.set_attribute("loadPass", "1");
        root.children.push(kart);
        let document = AaaDocument { root };
        let encoded = document.encode(AaaLimits::default()).unwrap();
        let decoded = AaaDocument::decode(&encoded, AaaLimits::default()).unwrap();
        assert_eq!(decoded, document);
    }

    #[test]
    fn rho_folder_upsert_preserves_unrelated_children() {
        let mut root = AaaNode::new("PackFolder");
        root.set_attribute("name", "KartRider");
        root.children.push(AaaNode::new("Unrelated"));
        let mut document = AaaDocument { root };
        let rho = AaaRhoFolder {
            name: "xun".to_owned(),
            file_name: "kart_xun.rho".to_owned(),
            key: 1,
            data_hash: 2,
            media_size: 3,
        };
        document.upsert_rho_folder(&["kart"], &rho).unwrap();
        document.upsert_rho_folder(&["kart"], &rho).unwrap();
        assert_eq!(document.root.children[0].name, "Unrelated");
        let kart = &document.root.children[1];
        assert_eq!(kart.children.len(), 1);
        assert_eq!(kart.children[0].attribute("fileName"), Some("kart_xun.rho"));
    }

    #[test]
    fn rho_mounts_follow_nested_pack_folder_paths() {
        let mut root = AaaNode::new("PackFolder");
        root.set_attribute("name", "KartRider");
        let mut kart = AaaNode::new("PackFolder");
        kart.set_attribute("name", "kart_");
        let mut rho = AaaNode::new("RhoFolder");
        for (name, value) in [
            ("name", "xun"),
            ("fileName", "kart_xun.rho"),
            ("key", "1"),
            ("dataHash", "2"),
            ("mediaSize", "3"),
        ] {
            rho.set_attribute(name, value);
        }
        kart.children.push(rho);
        root.children.push(kart);
        let mounts = AaaDocument { root }.rho_mounts().unwrap();
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].pack_path, ["kart_"]);
        assert_eq!(mounts[0].virtual_prefix(), "kart_/xun");
        assert_eq!(mounts[0].folder.file_name, "kart_xun.rho");
    }

    #[test]
    fn root_mounts_with_empty_names_are_matched_by_archive() {
        let mut root = AaaNode::new("PackFolder");
        root.set_attribute("name", "KartRider");
        let mut document = AaaDocument { root };
        for file_name in ["import_a.rho", "import_b.rho", "import_a.rho"] {
            document
                .upsert_rho_folder(
                    &[],
                    &AaaRhoFolder {
                        name: String::new(),
                        file_name: file_name.to_owned(),
                        key: 1,
                        data_hash: 2,
                        media_size: 3,
                    },
                )
                .unwrap();
        }
        let mounts = document.rho_mounts().unwrap();
        assert_eq!(mounts.len(), 2);
        assert_eq!(mounts[0].folder.file_name, "import_a.rho");
        assert_eq!(mounts[1].folder.file_name, "import_b.rho");
        assert!(mounts.iter().all(|mount| mount.virtual_prefix().is_empty()));
    }

    #[test]
    fn raw_binary_xml_round_trip() {
        let mut root = AaaNode::new("Config");
        root.text = "테스트".to_owned();
        root.set_attribute("locale", "kr");
        let encoded = root.encode_binary_xml(AaaLimits::default()).unwrap();
        assert_eq!(
            AaaNode::decode_binary_xml(&encoded, AaaLimits::default()).unwrap(),
            root
        );
    }

    #[test]
    #[ignore = "requires a local proprietary P5136 installation via P5136_DATA_DIR"]
    fn local_stock_aaa_semantic_round_trip() {
        let data = PathBuf::from(env::var_os("P5136_DATA_DIR").expect("P5136_DATA_DIR is set"));
        let limits = AaaLimits::default();
        let document = AaaDocument::read(data.join("aaa.pk"), limits).unwrap();
        assert_eq!(document.root.name, "PackFolder");
        assert_eq!(document.root.attribute("name"), Some("KartRider"));
        let encoded = document.encode(limits).unwrap();
        assert_eq!(AaaDocument::decode(&encoded, limits).unwrap(), document);
    }
}
