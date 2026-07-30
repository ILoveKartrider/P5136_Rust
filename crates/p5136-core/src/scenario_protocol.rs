//! P5136 single-player scenario start/completion wire contracts.

use thiserror::Error;

use crate::{
    adler32,
    packet::{PacketError, PacketReader, PacketWriter},
};

pub const START_SCENARIO_REQUEST_NAME: &str = "PqStartScenario";
pub const START_SCENARIO_REPLY_NAME: &str = "PrStartScenario";
pub const COMPLETE_SCENARIO_REQUEST_NAME: &str = "PqCompleteScenarioSingle";
pub const COMPLETE_SCENARIO_REPLY_NAME: &str = "PrCompleteScenarioSingle";
const COMPLETE_SCENARIO_BODY_LENGTH: usize = 22;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScenarioRequest {
    Start,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartScenarioRequest {
    pub scenario_type: i32,
}

#[derive(Debug, Error)]
pub enum ScenarioProtocolError {
    #[error(transparent)]
    Packet(#[from] PacketError),

    #[error("expected {name} hash 0x{expected:08X}, received 0x{actual:08X}")]
    UnexpectedPacketHash {
        name: &'static str,
        expected: u32,
        actual: u32,
    },

    #[error("{name} has {count} trailing bytes")]
    TrailingBytes { name: &'static str, count: usize },
}

#[must_use]
pub fn classify_scenario_request(hash: u32) -> Option<ScenarioRequest> {
    [
        (START_SCENARIO_REQUEST_NAME, ScenarioRequest::Start),
        (COMPLETE_SCENARIO_REQUEST_NAME, ScenarioRequest::Complete),
    ]
    .into_iter()
    .find_map(|(name, request)| (adler32::packet_hash(name) == hash).then_some(request))
}

/// Parses the exact `hash | scenario_type:i32` start request.
///
/// # Errors
///
/// Returns a typed packet, hash, or trailing-byte error.
pub fn parse_start_scenario_request(
    packet: &[u8],
) -> Result<StartScenarioRequest, ScenarioProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, START_SCENARIO_REQUEST_NAME)?;
    let request = StartScenarioRequest {
        scenario_type: reader.read_i32()?,
    };
    ensure_exhausted(&reader, START_SCENARIO_REQUEST_NAME)?;
    Ok(request)
}

/// Validates the exact captured length of the 22-byte opaque completion body.
///
/// The C# handler does not interpret this body. Rust therefore preserves the
/// evidence boundary by consuming exactly 22 bytes without assigning invented
/// field meanings or validating unobserved field values.
///
/// # Errors
///
/// Returns a typed packet, hash, or trailing-byte error.
pub fn parse_complete_scenario_request(packet: &[u8]) -> Result<(), ScenarioProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, COMPLETE_SCENARIO_REQUEST_NAME)?;
    let _opaque = reader.read_bytes(COMPLETE_SCENARIO_BODY_LENGTH)?;
    ensure_exhausted(&reader, COMPLETE_SCENARIO_REQUEST_NAME)
}

#[must_use]
pub fn serialize_start_scenario_reply(scenario_type: i32) -> Vec<u8> {
    serialize_scenario_status(START_SCENARIO_REPLY_NAME, scenario_type)
}

#[must_use]
pub fn serialize_complete_scenario_reply(scenario_type: i32) -> Vec<u8> {
    serialize_scenario_status(COMPLETE_SCENARIO_REPLY_NAME, scenario_type)
}

fn serialize_scenario_status(name: &'static str, scenario_type: i32) -> Vec<u8> {
    let mut packet = PacketWriter::named(name);
    packet.write_i32(scenario_type);
    packet.write_u8(0);
    packet.into_inner()
}

fn expect_hash(
    reader: &mut PacketReader<'_>,
    name: &'static str,
) -> Result<(), ScenarioProtocolError> {
    let expected = adler32::packet_hash(name);
    let actual = reader.read_u32()?;
    if actual != expected {
        return Err(ScenarioProtocolError::UnexpectedPacketHash {
            name,
            expected,
            actual,
        });
    }
    Ok(())
}

fn ensure_exhausted(
    reader: &PacketReader<'_>,
    name: &'static str,
) -> Result<(), ScenarioProtocolError> {
    let count = reader.remaining().len();
    if count != 0 {
        return Err(ScenarioProtocolError::TrailingBytes { name, count });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        COMPLETE_SCENARIO_REQUEST_NAME, START_SCENARIO_REQUEST_NAME, ScenarioProtocolError,
        ScenarioRequest, classify_scenario_request, parse_complete_scenario_request,
        parse_start_scenario_request, serialize_complete_scenario_reply,
        serialize_start_scenario_reply,
    };
    use crate::{adler32, packet::PacketError};

    #[test]
    fn classifier_and_captured_goldens_match() {
        assert_eq!(
            classify_scenario_request(adler32::packet_hash(START_SCENARIO_REQUEST_NAME)),
            Some(ScenarioRequest::Start)
        );
        assert_eq!(
            classify_scenario_request(adler32::packet_hash(COMPLETE_SCENARIO_REQUEST_NAME)),
            Some(ScenarioRequest::Complete)
        );
        assert_eq!(classify_scenario_request(0xDEAD_BEEF), None);

        let start = [0x03, 0x06, 0x24, 0x2F, 0x34, 0, 0, 1];
        assert_eq!(
            parse_start_scenario_request(&start).unwrap().scenario_type,
            0x0100_0034
        );
        assert_eq!(
            serialize_start_scenario_reply(0x0100_0034),
            [0x04, 0x06, 0x32, 0x2F, 0x34, 0, 0, 1, 0]
        );

        let complete = [
            0x90, 0x09, 0x1D, 0x76, 0x0E, 0, 0, 0, 0x53, 0x02, 0x88, 0x01, 0xB5, 0x04, 0x62, 0x4E,
            0x71, 0x12, 0x28, 0x01, 0x26, 0xB3, 0x34, 0, 0, 1,
        ];
        parse_complete_scenario_request(&complete).unwrap();
        assert_eq!(
            serialize_complete_scenario_reply(0x0100_0034),
            [0x91, 0x09, 0x34, 0x76, 0x34, 0, 0, 1, 0]
        );
    }

    #[test]
    fn malformed_scenario_packets_fail_closed() {
        let start = [0x03, 0x06, 0x24, 0x2F, 0x34, 0, 0, 1];
        for length in 0..start.len() {
            assert!(matches!(
                parse_start_scenario_request(&start[..length]),
                Err(ScenarioProtocolError::Packet(PacketError::Truncated { .. })
                    | ScenarioProtocolError::UnexpectedPacketHash { .. })
            ));
        }
        let mut trailing = start.to_vec();
        trailing.push(0);
        assert!(matches!(
            parse_start_scenario_request(&trailing),
            Err(ScenarioProtocolError::TrailingBytes {
                name: START_SCENARIO_REQUEST_NAME,
                count: 1,
            })
        ));

        let complete_hash = adler32::packet_hash(COMPLETE_SCENARIO_REQUEST_NAME).to_le_bytes();
        assert!(matches!(
            parse_complete_scenario_request(&complete_hash),
            Err(ScenarioProtocolError::Packet(PacketError::Truncated { .. }))
        ));
        let mut short_complete = complete_hash.to_vec();
        short_complete.resize(4 + 21, 0);
        assert!(matches!(
            parse_complete_scenario_request(&short_complete),
            Err(ScenarioProtocolError::Packet(PacketError::Truncated { .. }))
        ));
        let mut long_complete = complete_hash.to_vec();
        long_complete.resize(4 + 23, 0);
        assert!(matches!(
            parse_complete_scenario_request(&long_complete),
            Err(ScenarioProtocolError::TrailingBytes {
                name: COMPLETE_SCENARIO_REQUEST_NAME,
                count: 1,
            })
        ));
        let wrong_hash = vec![0; 4 + 22];
        assert!(matches!(
            parse_complete_scenario_request(&wrong_hash),
            Err(ScenarioProtocolError::UnexpectedPacketHash {
                name: COMPLETE_SCENARIO_REQUEST_NAME,
                actual: 0,
                ..
            })
        ));
    }
}
