//! P5136 matchmaking request and empty-match reply codecs.
//!
//! `PcStartMatching` is not a room-filter request. Its seven words are a
//! client session/auth envelope, so this module validates the fixed wire shape
//! without treating client-supplied values as room-selection authority.

use thiserror::Error;

use crate::{
    adler32,
    packet::{PacketError, PacketReader, PacketWriter},
};

pub const START_MATCHING_REQUEST_NAME: &str = "PcStartMatching";
pub const CANCEL_MATCHING_REQUEST_NAME: &str = "PcCancelMatching";
pub const MATCHING_FOUND_REPLY_NAME: &str = "PcMatchingFound";

pub const START_MATCHING_PACKET_LENGTH: usize = 32;
pub const CANCEL_MATCHING_PACKET_LENGTH: usize = 4;
pub const MATCHING_FOUND_CREATE_PACKET_LENGTH: usize = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchingRequestKind {
    Start,
    Cancel,
}

impl MatchingRequestKind {
    #[must_use]
    pub const fn request_name(self) -> &'static str {
        match self {
            Self::Start => START_MATCHING_REQUEST_NAME,
            Self::Cancel => CANCEL_MATCHING_REQUEST_NAME,
        }
    }
}

/// A verified `PcStartMatching` auth envelope.
///
/// The words intentionally stay private: they are neither room filters nor
/// server authority, and callers must not log or route from them.
#[derive(Clone, PartialEq, Eq)]
pub struct StartMatchingRequest {
    _session_auth: [u32; 7],
}

#[derive(Clone, PartialEq, Eq)]
pub enum MatchingRequest {
    Start(StartMatchingRequest),
    Cancel,
}

#[derive(Debug, Error)]
pub enum MatchingProtocolError {
    #[error(transparent)]
    Packet(#[from] PacketError),

    #[error("unsupported P5136 matching packet hash 0x{actual:08X}")]
    UnsupportedPacketHash { actual: u32 },

    #[error("{packet} has logical length {actual}; expected {expected}")]
    InvalidLength {
        packet: &'static str,
        actual: usize,
        expected: usize,
    },
}

#[must_use]
pub fn classify_matching_request(hash: u32) -> Option<MatchingRequestKind> {
    [MatchingRequestKind::Start, MatchingRequestKind::Cancel]
        .into_iter()
        .find(|kind| adler32::packet_hash(kind.request_name()) == hash)
}

/// Parses a complete matching request before any reply is emitted.
pub fn parse_matching_request(packet: &[u8]) -> Result<MatchingRequest, MatchingProtocolError> {
    let mut reader = PacketReader::new(packet);
    let hash = reader.read_u32()?;
    let kind = classify_matching_request(hash)
        .ok_or(MatchingProtocolError::UnsupportedPacketHash { actual: hash })?;
    let expected = match kind {
        MatchingRequestKind::Start => START_MATCHING_PACKET_LENGTH,
        MatchingRequestKind::Cancel => CANCEL_MATCHING_PACKET_LENGTH,
    };
    if packet.len() != expected {
        return Err(MatchingProtocolError::InvalidLength {
            packet: kind.request_name(),
            actual: packet.len(),
            expected,
        });
    }

    match kind {
        MatchingRequestKind::Start => {
            let mut session_auth = [0; 7];
            for word in &mut session_auth {
                *word = reader.read_u32()?;
            }
            Ok(MatchingRequest::Start(StartMatchingRequest {
                _session_auth: session_auth,
            }))
        }
        MatchingRequestKind::Cancel => Ok(MatchingRequest::Cancel),
    }
}

/// Serializes the stock client's complete empty-match/create state variant.
///
/// The state-0 body is exactly three zero bytes. It replaces the C# empty-room
/// two-byte truncation but deliberately does not randomly join a visible room.
#[must_use]
pub fn serialize_matching_found_create() -> Vec<u8> {
    let mut packet = PacketWriter::named(MATCHING_FOUND_REPLY_NAME);
    packet.write_bytes(&[0, 0, 0]);
    packet.into_inner()
}

#[cfg(test)]
mod tests {
    use super::{
        CANCEL_MATCHING_PACKET_LENGTH, CANCEL_MATCHING_REQUEST_NAME,
        MATCHING_FOUND_CREATE_PACKET_LENGTH, MATCHING_FOUND_REPLY_NAME, MatchingProtocolError,
        MatchingRequest, MatchingRequestKind, START_MATCHING_PACKET_LENGTH,
        START_MATCHING_REQUEST_NAME, classify_matching_request, parse_matching_request,
        serialize_matching_found_create,
    };
    use crate::{adler32, packet::PacketWriter};

    #[test]
    fn matching_envelope_and_cancel_are_exactly_consumed() {
        let mut start = PacketWriter::named(START_MATCHING_REQUEST_NAME);
        for word in 0..7 {
            start.write_u32(word);
        }
        assert_eq!(start.as_slice().len(), START_MATCHING_PACKET_LENGTH);
        assert!(matches!(
            parse_matching_request(start.as_slice()),
            Ok(MatchingRequest::Start(_))
        ));

        let cancel = PacketWriter::named(CANCEL_MATCHING_REQUEST_NAME);
        assert_eq!(cancel.as_slice().len(), CANCEL_MATCHING_PACKET_LENGTH);
        assert!(matches!(
            parse_matching_request(cancel.as_slice()),
            Ok(MatchingRequest::Cancel)
        ));

        for length in 0..START_MATCHING_PACKET_LENGTH {
            assert!(parse_matching_request(&start.as_slice()[..length]).is_err());
        }
        let mut trailing = start.into_inner();
        trailing.push(0);
        assert!(matches!(
            parse_matching_request(&trailing),
            Err(MatchingProtocolError::InvalidLength { .. })
        ));
    }

    #[test]
    fn classifier_and_empty_create_variant_match_the_stock_shape() {
        assert_eq!(
            classify_matching_request(adler32::packet_hash(START_MATCHING_REQUEST_NAME)),
            Some(MatchingRequestKind::Start)
        );
        assert_eq!(
            classify_matching_request(adler32::packet_hash(CANCEL_MATCHING_REQUEST_NAME)),
            Some(MatchingRequestKind::Cancel)
        );
        let reply = serialize_matching_found_create();
        assert_eq!(reply.len(), MATCHING_FOUND_CREATE_PACKET_LENGTH);
        assert_eq!(
            u32::from_le_bytes(reply[..4].try_into().unwrap()),
            adler32::packet_hash(MATCHING_FOUND_REPLY_NAME)
        );
        assert_eq!(&reply[4..], &[0, 0, 0]);
    }
}
