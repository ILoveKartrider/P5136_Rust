//! Strict read-only query/reply pairs recovered from the retained C# packet
//! trace corpus.
//!
//! This module is deliberately not a generic "ignore unknown packet" escape
//! hatch. It admits only the five queries whose complete terminal responses
//! can be produced without inventing room, AI, progression, equipment, or
//! time-attack state. Stateful packets from the same corpus remain unsupported
//! until their owning Rust domain implements the required transition.

use thiserror::Error;

use crate::{
    adler32,
    packet::{PacketError, PacketReader, PacketWriter},
};

const MAX_EVENT_BUY_IDS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapturedQueryRequest {
    CurrentCompetition,
    ChallengerInfo,
    EventBuyCount,
    GetTrainingMission,
    NewCareerList,
}

pub const CAPTURED_QUERY_REQUESTS: &[CapturedQueryRequest] = &[
    CapturedQueryRequest::CurrentCompetition,
    CapturedQueryRequest::ChallengerInfo,
    CapturedQueryRequest::EventBuyCount,
    CapturedQueryRequest::GetTrainingMission,
    CapturedQueryRequest::NewCareerList,
];

impl CapturedQueryRequest {
    #[must_use]
    pub const fn request_name(self) -> &'static str {
        match self {
            Self::CurrentCompetition => "ChGetCurrentCmpRequestPacket",
            Self::ChallengerInfo => "PqChallengerInfoPacket",
            Self::EventBuyCount => "PqEventBuyCount",
            Self::GetTrainingMission => "PqGetTrainingMission",
            Self::NewCareerList => "PqNewCareerListPacket",
        }
    }

    #[must_use]
    pub fn request_hash(self) -> u32 {
        adler32::packet_hash(self.request_name())
    }

    #[must_use]
    pub const fn observed_lengths(self) -> &'static [usize] {
        match self {
            Self::CurrentCompetition | Self::ChallengerInfo | Self::NewCareerList => &[4],
            Self::EventBuyCount => &[24],
            Self::GetTrainingMission => &[12],
        }
    }
}

#[derive(Debug, Error)]
pub enum CapturedQueryError {
    #[error(transparent)]
    Packet(#[from] PacketError),

    #[error("expected {name} hash 0x{expected:08X}, received 0x{actual:08X}")]
    UnexpectedPacketHash {
        name: &'static str,
        expected: u32,
        actual: u32,
    },

    #[error("{name} has unsupported logical length {actual}; captured lengths are {expected:?}")]
    UnsupportedLength {
        name: &'static str,
        actual: usize,
        expected: &'static [usize],
    },

    #[error("PqEventBuyCount contains {actual} IDs; captured count is {expected}")]
    UnexpectedEventBuyCount { actual: i32, expected: usize },
}

#[must_use]
pub fn classify_captured_query_request(hash: u32) -> Option<CapturedQueryRequest> {
    CAPTURED_QUERY_REQUESTS
        .iter()
        .copied()
        .find(|request| request.request_hash() == hash)
}

/// Validates one captured read-only query and produces its terminal reply.
///
/// # Errors
///
/// Returns a typed error for a wrong hash, any logical length absent from the
/// corpus, a truncated body, or an invalid bounded read-only query shape.
pub fn process_captured_query_request(
    request: CapturedQueryRequest,
    packet: &[u8],
) -> Result<Vec<u8>, CapturedQueryError> {
    validate_header_and_length(request, packet)?;
    Ok(match request {
        CapturedQueryRequest::CurrentCompetition => serialize_current_competition(),
        CapturedQueryRequest::ChallengerInfo => serialize_challenger_info(),
        CapturedQueryRequest::EventBuyCount => {
            serialize_event_buy_count(parse_event_buy_ids(packet)?)
        }
        CapturedQueryRequest::GetTrainingMission => {
            serialize_training_mission(parse_training_mission_type(packet)?)
        }
        CapturedQueryRequest::NewCareerList => serialize_new_career_list(),
    })
}

fn validate_header_and_length(
    request: CapturedQueryRequest,
    packet: &[u8],
) -> Result<(), CapturedQueryError> {
    let mut reader = PacketReader::new(packet);
    let actual = reader.read_u32()?;
    let expected = request.request_hash();
    if actual != expected {
        return Err(CapturedQueryError::UnexpectedPacketHash {
            name: request.request_name(),
            expected,
            actual,
        });
    }
    if !request.observed_lengths().contains(&packet.len()) {
        return Err(CapturedQueryError::UnsupportedLength {
            name: request.request_name(),
            actual: packet.len(),
            expected: request.observed_lengths(),
        });
    }
    Ok(())
}

fn parse_event_buy_ids(packet: &[u8]) -> Result<[u32; MAX_EVENT_BUY_IDS], CapturedQueryError> {
    let mut reader = PacketReader::new(packet);
    let _hash = reader.read_u32()?;
    let actual = reader.read_i32()?;
    if actual != i32::try_from(MAX_EVENT_BUY_IDS).expect("small fixed count fits i32") {
        return Err(CapturedQueryError::UnexpectedEventBuyCount {
            actual,
            expected: MAX_EVENT_BUY_IDS,
        });
    }
    let ids = [
        reader.read_u32()?,
        reader.read_u32()?,
        reader.read_u32()?,
        reader.read_u32()?,
    ];
    debug_assert!(reader.remaining().is_empty());
    Ok(ids)
}

fn parse_training_mission_type(packet: &[u8]) -> Result<i32, PacketError> {
    let mut reader = PacketReader::new(packet);
    let _hash = reader.read_u32()?;
    let mission_type = reader.read_i32()?;
    let _track = reader.read_u32()?;
    debug_assert!(reader.remaining().is_empty());
    Ok(mission_type)
}

fn serialize_current_competition() -> Vec<u8> {
    let mut packet = PacketWriter::named("ChGetCurrentCmpReplyPacket");
    packet.write_i32(0);
    packet.write_i32(0);
    packet.write_i32(0);
    packet.write_u8(0);
    packet.into_inner()
}

fn serialize_challenger_info() -> Vec<u8> {
    let mut packet = PacketWriter::named("PrChallengerInfoPacket");
    packet.write_i32(40);
    for _ in 0..40 {
        packet.write_u16(55);
    }
    packet.write_i32(0);
    packet.write_u8(1);
    packet.into_inner()
}

fn serialize_event_buy_count(ids: [u32; MAX_EVENT_BUY_IDS]) -> Vec<u8> {
    let mut packet = PacketWriter::named("PrEventBuyCount");
    packet.write_i32(i32::try_from(ids.len()).expect("small fixed count fits i32"));
    for id in ids {
        packet.write_u32(id);
        packet.write_i32(0);
    }
    packet.into_inner()
}

fn serialize_training_mission(mission_type: i32) -> Vec<u8> {
    let mut packet = PacketWriter::named("PrGetTrainingMission");
    packet.write_i32(mission_type);
    packet.write_i32(0);
    packet.write_i32(0);
    packet.write_bytes(&[0; 4]);
    packet.into_inner()
}

fn serialize_new_career_list() -> Vec<u8> {
    let mut packet = PacketWriter::named("PrNewCareerListPacket");
    packet.write_bytes(&[0; 20]);
    packet.into_inner()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{
        CAPTURED_QUERY_REQUESTS, CapturedQueryError, CapturedQueryRequest,
        classify_captured_query_request, process_captured_query_request,
    };
    use crate::packet::PacketWriter;

    #[test]
    fn captured_query_classifier_is_complete_and_collision_free() {
        let mut hashes = HashSet::new();
        for request in CAPTURED_QUERY_REQUESTS {
            let hash = request.request_hash();
            assert!(hashes.insert(hash), "duplicate hash for {request:?}");
            assert_eq!(classify_captured_query_request(hash), Some(*request));
        }
        assert_eq!(classify_captured_query_request(0xDEAD_BEEF), None);
    }

    #[test]
    fn every_captured_query_shape_has_a_terminal_reply() {
        for request in CAPTURED_QUERY_REQUESTS {
            for &length in request.observed_lengths() {
                let mut packet = vec![0; length];
                packet[..4].copy_from_slice(&request.request_hash().to_le_bytes());
                if *request == CapturedQueryRequest::EventBuyCount {
                    packet[4..8].copy_from_slice(&4_i32.to_le_bytes());
                }
                let reply = process_captured_query_request(*request, &packet);
                assert!(
                    reply.as_ref().is_ok_and(|packet| !packet.is_empty()),
                    "{request:?} length {length}: {reply:?}"
                );
            }
        }
    }

    #[test]
    fn unobserved_lengths_and_wrong_hashes_fail_closed() {
        let request = CapturedQueryRequest::GetTrainingMission;
        let mut wrong_length = request.request_hash().to_le_bytes().to_vec();
        wrong_length.resize(11, 0);
        assert!(matches!(
            process_captured_query_request(request, &wrong_length),
            Err(CapturedQueryError::UnsupportedLength {
                name: "PqGetTrainingMission",
                actual: 11,
                ..
            })
        ));

        let wrong_hash = vec![0; 12];
        assert!(matches!(
            process_captured_query_request(request, &wrong_hash),
            Err(CapturedQueryError::UnexpectedPacketHash {
                name: "PqGetTrainingMission",
                actual: 0,
                ..
            })
        ));
    }

    #[test]
    fn safe_terminal_replies_match_csharp_trace_goldens() {
        let current = PacketWriter::named("ChGetCurrentCmpRequestPacket").into_inner();
        assert_eq!(
            process_captured_query_request(CapturedQueryRequest::CurrentCompetition, &current)
                .unwrap(),
            [
                0x32, 0x0A, 0x81, 0x86, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
            ]
        );

        let challenger = PacketWriter::named("PqChallengerInfoPacket").into_inner();
        let challenger_reply =
            process_captured_query_request(CapturedQueryRequest::ChallengerInfo, &challenger)
                .unwrap();
        assert_eq!(challenger_reply.len(), 93);
        assert_eq!(
            &challenger_reply[..8],
            &[0x9B, 0x08, 0x79, 0x61, 40, 0, 0, 0]
        );
        assert!(
            challenger_reply[8..88]
                .chunks_exact(2)
                .all(|word| word == [55, 0])
        );
        assert_eq!(&challenger_reply[88..], &[0, 0, 0, 0, 1]);

        let mut event = PacketWriter::named("PqEventBuyCount");
        event.write_i32(4);
        for id in [3915, 3916, 3914, 3917] {
            event.write_u32(id);
        }
        assert_eq!(
            process_captured_query_request(CapturedQueryRequest::EventBuyCount, event.as_slice())
                .unwrap(),
            [
                0xFD, 0x05, 0x7F, 0x2E, 4, 0, 0, 0, 0x4B, 0x0F, 0, 0, 0, 0, 0, 0, 0x4C, 0x0F, 0, 0,
                0, 0, 0, 0, 0x4A, 0x0F, 0, 0, 0, 0, 0, 0, 0x4D, 0x0F, 0, 0, 0, 0, 0, 0,
            ]
        );

        let mut training = PacketWriter::named("PqGetTrainingMission");
        training.write_i32(6);
        training.write_u32(0x1572_025B);
        assert_eq!(
            process_captured_query_request(
                CapturedQueryRequest::GetTrainingMission,
                training.as_slice(),
            )
            .unwrap(),
            [
                0x00, 0x08, 0xB7, 0x51, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
            ]
        );

        let career = PacketWriter::named("PqNewCareerListPacket").into_inner();
        assert_eq!(
            process_captured_query_request(CapturedQueryRequest::NewCareerList, &career).unwrap(),
            [
                0x32, 0x08, 0x5E, 0x58, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
            ]
        );
    }
}
