//! Bounded immutable emblem definitions loaded from the stock KR client.

use std::{borrow::Cow, collections::HashSet};

use p5136_core::myroom_protocol::MAX_MYROOM_EMBLEMS;
use quick_xml::{
    Reader, XmlVersion,
    events::{BytesStart, Event},
    name::QName,
};
use thiserror::Error;

pub const MAX_EMBLEM_XML_BYTES: usize = 1024 * 1024;
const MAX_EMBLEM_ATTRIBUTES: usize = 32;
const MAX_EMBLEM_ATTRIBUTE_BYTES: usize = 4 * 1024;

/// Source-ordered, immutable positive `i16` emblem definitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmblemCatalog {
    ids: Vec<i16>,
    keys: HashSet<i16>,
}

impl EmblemCatalog {
    /// Validates IDs supplied by another authenticated runtime source.
    pub fn from_ids(ids: impl IntoIterator<Item = i16>) -> Result<Self, EmblemCatalogError> {
        let mut ordered = Vec::new();
        let mut keys = HashSet::new();
        for id in ids {
            if ordered.len() >= MAX_MYROOM_EMBLEMS {
                return Err(EmblemCatalogError::TooManyEmblems {
                    maximum: MAX_MYROOM_EMBLEMS,
                });
            }
            if id <= 0 {
                return Err(EmblemCatalogError::InvalidEmblemId);
            }
            if !keys.insert(id) {
                return Err(EmblemCatalogError::DuplicateEmblemId { id });
            }
            ordered.push(id);
        }
        Ok(Self { ids: ordered, keys })
    }

    /// Parses the stock client's bounded UTF-16 or UTF-8 `<kartEmblem>` XML.
    pub fn from_client_xml(xml: &[u8]) -> Result<Self, EmblemCatalogError> {
        if xml.len() > MAX_EMBLEM_XML_BYTES {
            return Err(EmblemCatalogError::DocumentTooLarge {
                actual: xml.len(),
                maximum: MAX_EMBLEM_XML_BYTES,
            });
        }
        let decoded = decode_xml(xml)?;
        parse_client_xml(&decoded)
    }

    #[must_use]
    pub fn ids(&self) -> &[i16] {
        &self.ids
    }

    #[must_use]
    pub fn contains(&self, id: i16) -> bool {
        self.keys.contains(&id)
    }

    pub(crate) fn from_validated_parts(ids: Vec<i16>, keys: HashSet<i16>) -> Self {
        debug_assert_eq!(ids.len(), keys.len());
        debug_assert!(ids.iter().all(|id| *id > 0 && keys.contains(id)));
        Self { ids, keys }
    }
}

#[derive(Debug, Error)]
pub enum EmblemCatalogError {
    #[error("client emblem XML exceeds {maximum} bytes ({actual} bytes)")]
    DocumentTooLarge { actual: usize, maximum: usize },

    #[error("client emblem XML has an incomplete byte-order mark")]
    IncompleteByteOrderMark,

    #[error("client emblem XML contains an odd number of UTF-16 bytes")]
    OddUtf16Length,

    #[error("client emblem XML contains invalid UTF-16")]
    InvalidUtf16,

    #[error("client emblem XML is neither BOM-tagged UTF-16 nor valid UTF-8")]
    UnsupportedEncoding,

    #[error("invalid client emblem XML: {0}")]
    Xml(#[from] quick_xml::Error),

    #[error("client emblem XML contains a prohibited document type declaration")]
    DocumentType,

    #[error("client emblem XML has no kartEmblem root element")]
    MissingRoot,

    #[error("client emblem XML contains more than one root element")]
    MultipleRoots,

    #[error("client emblem XML root is not kartEmblem")]
    WrongRoot,

    #[error("client emblem XML contains unexpected element {name:?}")]
    UnexpectedElement { name: String },

    #[error("client emblem XML contains non-whitespace text")]
    UnexpectedText,

    #[error("client emblem element contains more than {maximum} attributes")]
    TooManyAttributes { maximum: usize },

    #[error("client emblem attribute value exceeds {maximum} bytes")]
    AttributeValueTooLong { maximum: usize },

    #[error("client emblem element has more than one id attribute")]
    DuplicateIdAttribute,

    #[error("client emblem element has no valid positive i16 id")]
    InvalidEmblemId,

    #[error("client emblem XML contains duplicate emblem ID {id}")]
    DuplicateEmblemId { id: i16 },

    #[error("client emblem XML contains more than {maximum} entries")]
    TooManyEmblems { maximum: usize },

    #[error("client emblem XML contains no emblem definitions")]
    MissingEmblems,
}

fn decode_xml(xml: &[u8]) -> Result<Cow<'_, str>, EmblemCatalogError> {
    if xml.len() == 1 && matches!(xml[0], 0xff | 0xfe) {
        return Err(EmblemCatalogError::IncompleteByteOrderMark);
    }
    if let Some(body) = xml.strip_prefix(&[0xff, 0xfe]) {
        return decode_utf16(body, u16::from_le_bytes);
    }
    if let Some(body) = xml.strip_prefix(&[0xfe, 0xff]) {
        return decode_utf16(body, u16::from_be_bytes);
    }
    let body = xml.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(xml);
    std::str::from_utf8(body)
        .map(Cow::Borrowed)
        .map_err(|_| EmblemCatalogError::UnsupportedEncoding)
}

fn decode_utf16(
    bytes: &[u8],
    decode: fn([u8; 2]) -> u16,
) -> Result<Cow<'static, str>, EmblemCatalogError> {
    if !bytes.len().is_multiple_of(2) {
        return Err(EmblemCatalogError::OddUtf16Length);
    }
    let units = bytes
        .chunks_exact(2)
        .map(|pair| decode([pair[0], pair[1]]))
        .collect::<Vec<_>>();
    String::from_utf16(&units)
        .map(Cow::Owned)
        .map_err(|_| EmblemCatalogError::InvalidUtf16)
}

fn parse_client_xml(xml: &str) -> Result<EmblemCatalog, EmblemCatalogError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut depth = 0_usize;
    let mut root_seen = false;
    let mut root_closed = false;
    let mut ids = Vec::new();
    let mut keys = HashSet::new();

    loop {
        match reader.read_event()? {
            Event::Start(element) => {
                handle_start(
                    &reader,
                    &element,
                    depth,
                    root_seen,
                    root_closed,
                    &mut ids,
                    &mut keys,
                )?;
                if depth == 0 {
                    root_seen = true;
                }
                depth = depth
                    .checked_add(1)
                    .ok_or(EmblemCatalogError::UnexpectedElement {
                        name: "excessive XML depth".to_owned(),
                    })?;
            }
            Event::Empty(element) => {
                handle_start(
                    &reader,
                    &element,
                    depth,
                    root_seen,
                    root_closed,
                    &mut ids,
                    &mut keys,
                )?;
                if depth == 0 {
                    root_seen = true;
                    root_closed = true;
                }
            }
            Event::End(_) => {
                depth = depth
                    .checked_sub(1)
                    .ok_or(EmblemCatalogError::MultipleRoots)?;
                if depth == 0 {
                    root_closed = true;
                }
            }
            Event::Text(text) if !text.as_ref().iter().all(u8::is_ascii_whitespace) => {
                return Err(EmblemCatalogError::UnexpectedText);
            }
            Event::CData(text) if !text.as_ref().iter().all(u8::is_ascii_whitespace) => {
                return Err(EmblemCatalogError::UnexpectedText);
            }
            Event::GeneralRef(_) => return Err(EmblemCatalogError::UnexpectedText),
            Event::DocType(_) => return Err(EmblemCatalogError::DocumentType),
            Event::Eof => break,
            Event::Decl(_)
            | Event::PI(_)
            | Event::Comment(_)
            | Event::Text(_)
            | Event::CData(_) => {}
        }
    }

    if !root_seen {
        return Err(EmblemCatalogError::MissingRoot);
    }
    if !root_closed || depth != 0 {
        return Err(EmblemCatalogError::MissingRoot);
    }
    if ids.is_empty() {
        return Err(EmblemCatalogError::MissingEmblems);
    }
    Ok(EmblemCatalog { ids, keys })
}

#[allow(
    clippy::too_many_arguments,
    reason = "the streaming parser passes its bounded state explicitly"
)]
fn handle_start(
    reader: &Reader<&[u8]>,
    element: &BytesStart<'_>,
    depth: usize,
    root_seen: bool,
    root_closed: bool,
    ids: &mut Vec<i16>,
    keys: &mut HashSet<i16>,
) -> Result<(), EmblemCatalogError> {
    if depth == 0 {
        if root_seen || root_closed {
            return Err(EmblemCatalogError::MultipleRoots);
        }
        if element.name() != QName(b"kartEmblem") {
            return Err(EmblemCatalogError::WrongRoot);
        }
        validate_attributes(reader, element, false)?;
        return Ok(());
    }
    if depth != 1 || element.name() != QName(b"emblem") {
        return Err(EmblemCatalogError::UnexpectedElement {
            name: String::from_utf8_lossy(element.name().as_ref()).into_owned(),
        });
    }
    if ids.len() >= MAX_MYROOM_EMBLEMS {
        return Err(EmblemCatalogError::TooManyEmblems {
            maximum: MAX_MYROOM_EMBLEMS,
        });
    }
    let id =
        validate_attributes(reader, element, true)?.ok_or(EmblemCatalogError::InvalidEmblemId)?;
    if !keys.insert(id) {
        return Err(EmblemCatalogError::DuplicateEmblemId { id });
    }
    ids.push(id);
    Ok(())
}

fn validate_attributes(
    reader: &Reader<&[u8]>,
    element: &BytesStart<'_>,
    require_id: bool,
) -> Result<Option<i16>, EmblemCatalogError> {
    let mut count = 0_usize;
    let mut id = None;
    for result in element.attributes() {
        count += 1;
        if count > MAX_EMBLEM_ATTRIBUTES {
            return Err(EmblemCatalogError::TooManyAttributes {
                maximum: MAX_EMBLEM_ATTRIBUTES,
            });
        }
        let attribute = result.map_err(quick_xml::Error::from)?;
        let value =
            attribute.decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())?;
        if value.len() > MAX_EMBLEM_ATTRIBUTE_BYTES {
            return Err(EmblemCatalogError::AttributeValueTooLong {
                maximum: MAX_EMBLEM_ATTRIBUTE_BYTES,
            });
        }
        if attribute.key == QName(b"id") {
            if id.is_some() {
                return Err(EmblemCatalogError::DuplicateIdAttribute);
            }
            id = value.parse::<i16>().ok().filter(|value| *value > 0);
            if id.is_none() {
                return Err(EmblemCatalogError::InvalidEmblemId);
            }
        }
    }
    if require_id && id.is_none() {
        return Err(EmblemCatalogError::InvalidEmblemId);
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::{EmblemCatalog, EmblemCatalogError};

    fn utf16_le(xml: &str) -> Vec<u8> {
        let mut encoded = vec![0xff, 0xfe];
        for unit in xml.encode_utf16() {
            encoded.extend_from_slice(&unit.to_le_bytes());
        }
        encoded
    }

    #[test]
    fn parses_source_ordered_utf16_client_catalog() {
        let xml = utf16_le(
            r#"<?xml version="1.0" encoding="UTF-16"?>
               <kartEmblem>
                 <emblem id="7" name="first" desc="" />
                 <emblem id="8193" name="second" desc="" />
                 <emblem id="32767" name="third" desc="" />
               </kartEmblem>"#,
        );
        let catalog = EmblemCatalog::from_client_xml(&xml).unwrap();
        assert_eq!(catalog.ids(), [7, 8_193, 32_767]);
        assert!(catalog.contains(8_193));
        assert!(!catalog.contains(0));
    }

    #[test]
    fn rejects_nonpositive_duplicate_and_nested_ids() {
        for (xml, expected) in [
            (r#"<kartEmblem><emblem id="0"/></kartEmblem>"#, "positive"),
            (
                r#"<kartEmblem><emblem id="7"/><emblem id="7"/></kartEmblem>"#,
                "duplicate",
            ),
            (
                r#"<kartEmblem><group><emblem id="7"/></group></kartEmblem>"#,
                "unexpected",
            ),
        ] {
            let error = EmblemCatalog::from_client_xml(xml.as_bytes()).unwrap_err();
            assert!(
                error.to_string().contains(expected),
                "{error:?} did not contain {expected:?}"
            );
        }
    }

    #[test]
    fn rejects_doctype_and_odd_utf16() {
        assert!(matches!(
            EmblemCatalog::from_client_xml(
                br#"<!DOCTYPE kartEmblem><kartEmblem><emblem id="7"/></kartEmblem>"#
            ),
            Err(EmblemCatalogError::DocumentType)
        ));
        assert!(matches!(
            EmblemCatalog::from_client_xml(&[0xff, 0xfe, b'<']),
            Err(EmblemCatalogError::OddUtf16Length)
        ));
    }
}
