//! Bounded codecs for P5136 no-reply driving telemetry.
//!
//! These packets are diagnostics or client-side relay probes in the reference
//! server; they do not authorize profile or world mutation. Known structured
//! vectors are fully consumed. The one unidentified captured hash is isolated
//! behind its four observed producer lengths instead of becoming a generic
//! "accept unknown packet" escape hatch.

use thiserror::Error;

use crate::{
    adler32,
    packet::{PacketError, PacketReader},
};

pub const GAME_AI_REPORT_NAME: &str = "GameAiReportPacket";
pub const GAME_REPORT_NAME: &str = "GameReportPacket";
pub const GAME_CLIENT_FRAME_NAME: &str = "PcGameClientFramePacket";
pub const GAME_REQUEST_RELAY_NAME: &str = "PcGameRequestRelay";
pub const RIDE_EVENT_REPORT_NAME: &str = "PcRideEventReportPacket";
pub const RIDE_PATH_REPORT_NAME: &str = "PcRidePathReportPacket";
pub const UNIDENTIFIED_DRIVING_REPORT_HASH: u32 = 0x5815_082A;

pub const GAME_AI_REPORT_LENGTH: usize = 36;
pub const GAME_REPORT_LENGTH: usize = 361;
pub const GAME_CLIENT_FRAME_LENGTH: usize = 16;
pub const GAME_REQUEST_RELAY_LENGTH: usize = 12;
pub const MAX_RIDE_EVENT_COUNT: u32 = 64;
pub const MAX_RIDE_EVENT_STRING_UNITS: usize = 64;
pub const MAX_RIDE_PATH_SAMPLES: u32 = 64;
pub const RIDE_PATH_SAMPLE_LENGTH: usize = 27;
pub const UNIDENTIFIED_DRIVING_REPORT_LENGTHS: [usize; 4] = [64, 68, 76, 80];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryRequestKind {
    GameAiReport,
    GameReport,
    GameClientFrame,
    GameRequestRelay,
    RideEventReport,
    RidePathReport,
    UnidentifiedDrivingReport,
}

impl TelemetryRequestKind {
    #[must_use]
    pub const fn request_name(self) -> &'static str {
        match self {
            Self::GameAiReport => GAME_AI_REPORT_NAME,
            Self::GameReport => GAME_REPORT_NAME,
            Self::GameClientFrame => GAME_CLIENT_FRAME_NAME,
            Self::GameRequestRelay => GAME_REQUEST_RELAY_NAME,
            Self::RideEventReport => RIDE_EVENT_REPORT_NAME,
            Self::RidePathReport => RIDE_PATH_REPORT_NAME,
            Self::UnidentifiedDrivingReport => "unknown-0x5815082A",
        }
    }

    #[must_use]
    pub fn request_hash(self) -> u32 {
        match self {
            Self::UnidentifiedDrivingReport => UNIDENTIFIED_DRIVING_REPORT_HASH,
            _ => adler32::packet_hash(self.request_name()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TelemetryReport {
    GameAiReport,
    GameReport {
        plane_check: i32,
        distance: f32,
    },
    GameClientFrame {
        local_frame: u32,
        server_frame: u32,
        acknowledged_frame: u32,
    },
    GameRequestRelay {
        value: i32,
        route_hash: u32,
    },
    RideEventReport {
        event_count: u32,
    },
    RidePathReport {
        sample_count: u32,
    },
    UnidentifiedDrivingReport {
        logical_length: usize,
    },
}

#[derive(Debug, Error)]
pub enum TelemetryProtocolError {
    #[error(transparent)]
    Packet(#[from] PacketError),

    #[error("unsupported telemetry packet hash 0x{hash:08X}")]
    UnsupportedPacketHash { hash: u32 },

    #[error("{packet} contained hash 0x{actual:08X}; expected 0x{expected:08X}")]
    PacketHashMismatch {
        packet: &'static str,
        actual: u32,
        expected: u32,
    },

    #[error("{packet} has logical length {actual}; expected {expected}")]
    InvalidLength {
        packet: &'static str,
        actual: usize,
        expected: usize,
    },

    #[error(
        "unidentified 0x5815082A report has logical length {actual}; captured lengths are {allowed:?}"
    )]
    UnidentifiedLength { actual: usize, allowed: [usize; 4] },

    #[error("{packet} declares {actual} records; operational maximum is {maximum}")]
    RecordLimit {
        packet: &'static str,
        actual: u32,
        maximum: u32,
    },

    #[error("{packet} left {count} unparsed bytes")]
    TrailingBytes { packet: &'static str, count: usize },

    #[error("{packet} record-byte length overflowed usize")]
    RecordLengthOverflow { packet: &'static str },
}

#[must_use]
pub fn classify_telemetry_request(hash: u32) -> Option<TelemetryRequestKind> {
    [
        TelemetryRequestKind::GameAiReport,
        TelemetryRequestKind::GameReport,
        TelemetryRequestKind::GameClientFrame,
        TelemetryRequestKind::GameRequestRelay,
        TelemetryRequestKind::RideEventReport,
        TelemetryRequestKind::RidePathReport,
        TelemetryRequestKind::UnidentifiedDrivingReport,
    ]
    .into_iter()
    .find(|kind| kind.request_hash() == hash)
}

pub fn parse_telemetry_request(
    kind: TelemetryRequestKind,
    packet: &[u8],
) -> Result<TelemetryReport, TelemetryProtocolError> {
    match kind {
        TelemetryRequestKind::GameAiReport => parse_game_ai_report(packet),
        TelemetryRequestKind::GameReport => parse_game_report(packet),
        TelemetryRequestKind::GameClientFrame => parse_game_client_frame(packet),
        TelemetryRequestKind::GameRequestRelay => parse_game_request_relay(packet),
        TelemetryRequestKind::RideEventReport => parse_ride_event_report(packet),
        TelemetryRequestKind::RidePathReport => parse_ride_path_report(packet),
        TelemetryRequestKind::UnidentifiedDrivingReport => {
            parse_unidentified_driving_report(packet)
        }
    }
}

fn parse_game_ai_report(packet: &[u8]) -> Result<TelemetryReport, TelemetryProtocolError> {
    require_exact_length(
        TelemetryRequestKind::GameAiReport,
        packet,
        GAME_AI_REPORT_LENGTH,
    )?;
    let mut reader = checked_reader(TelemetryRequestKind::GameAiReport, packet)?;
    let _producer_payload = reader.read_bytes(32)?;
    finish(reader, TelemetryRequestKind::GameAiReport)?;
    Ok(TelemetryReport::GameAiReport)
}

fn parse_game_report(packet: &[u8]) -> Result<TelemetryReport, TelemetryProtocolError> {
    require_exact_length(TelemetryRequestKind::GameReport, packet, GAME_REPORT_LENGTH)?;
    let mut reader = checked_reader(TelemetryRequestKind::GameReport, packet)?;
    let _date_time = reader.read_bytes(18)?;
    let _get_item = reader.read_i32()?;
    let _use_item = reader.read_i32()?;
    let _use_booster = reader.read_encoded_i32()?;
    for _ in 0..60 {
        let _ = reader.read_encoded_i32()?;
    }
    let _hash_1 = reader.read_encoded_i32()?;
    let _hash_2 = reader.read_encoded_i32()?;
    let _hash_3 = reader.read_encoded_i32()?;
    let _single_1 = reader.read_encoded_f32()?;
    let _single_2 = reader.read_encoded_f32()?;
    let distance = reader.read_encoded_f32()?;
    let plane_check = reader.read_i32()?;
    let _hash_array_2 = reader.read_bytes(20)?;
    let _hash_4 = reader.read_i32()?;
    let _hash_array_3 = reader.read_bytes(16)?;
    // Every captured producer appends the same 19-byte post-5136 extension
    // that the current C# handler leaves unread.
    let _post_5136_extension = reader.read_bytes(19)?;
    finish(reader, TelemetryRequestKind::GameReport)?;
    Ok(TelemetryReport::GameReport {
        plane_check,
        distance,
    })
}

fn parse_game_client_frame(packet: &[u8]) -> Result<TelemetryReport, TelemetryProtocolError> {
    require_exact_length(
        TelemetryRequestKind::GameClientFrame,
        packet,
        GAME_CLIENT_FRAME_LENGTH,
    )?;
    let mut reader = checked_reader(TelemetryRequestKind::GameClientFrame, packet)?;
    let report = TelemetryReport::GameClientFrame {
        local_frame: reader.read_u32()?,
        server_frame: reader.read_u32()?,
        acknowledged_frame: reader.read_u32()?,
    };
    finish(reader, TelemetryRequestKind::GameClientFrame)?;
    Ok(report)
}

fn parse_game_request_relay(packet: &[u8]) -> Result<TelemetryReport, TelemetryProtocolError> {
    require_exact_length(
        TelemetryRequestKind::GameRequestRelay,
        packet,
        GAME_REQUEST_RELAY_LENGTH,
    )?;
    let mut reader = checked_reader(TelemetryRequestKind::GameRequestRelay, packet)?;
    let report = TelemetryReport::GameRequestRelay {
        value: reader.read_i32()?,
        route_hash: reader.read_u32()?,
    };
    finish(reader, TelemetryRequestKind::GameRequestRelay)?;
    Ok(report)
}

fn parse_ride_event_report(packet: &[u8]) -> Result<TelemetryReport, TelemetryProtocolError> {
    let mut reader = checked_reader(TelemetryRequestKind::RideEventReport, packet)?;
    let event_count = reader.read_u32()?;
    require_record_limit(
        TelemetryRequestKind::RideEventReport,
        event_count,
        MAX_RIDE_EVENT_COUNT,
    )?;
    for _ in 0..event_count {
        let _event_name = reader.read_utf16_bounded(MAX_RIDE_EVENT_STRING_UNITS)?;
        let _position_x = reader.read_f32()?;
        let _position_y = reader.read_f32()?;
        let _position_z = reader.read_f32()?;
        let _event_state = reader.read_u32()?;
        let _active = reader.read_u8()?;
        let _event_id = reader.read_u16()?;
        let _event_value = reader.read_u32()?;
        let _event_subject = reader.read_utf16_bounded(MAX_RIDE_EVENT_STRING_UNITS)?;
        let _tick = reader.read_u32()?;
    }
    finish(reader, TelemetryRequestKind::RideEventReport)?;
    Ok(TelemetryReport::RideEventReport { event_count })
}

fn parse_ride_path_report(packet: &[u8]) -> Result<TelemetryReport, TelemetryProtocolError> {
    let mut reader = checked_reader(TelemetryRequestKind::RidePathReport, packet)?;
    let sample_count = reader.read_u32()?;
    require_record_limit(
        TelemetryRequestKind::RidePathReport,
        sample_count,
        MAX_RIDE_PATH_SAMPLES,
    )?;
    let sample_count = usize::try_from(sample_count).map_err(|_| {
        TelemetryProtocolError::RecordLengthOverflow {
            packet: RIDE_PATH_REPORT_NAME,
        }
    })?;
    let byte_length = sample_count.checked_mul(RIDE_PATH_SAMPLE_LENGTH).ok_or(
        TelemetryProtocolError::RecordLengthOverflow {
            packet: RIDE_PATH_REPORT_NAME,
        },
    )?;
    let _samples = reader.read_bytes(byte_length)?;
    finish(reader, TelemetryRequestKind::RidePathReport)?;
    Ok(TelemetryReport::RidePathReport {
        sample_count: u32::try_from(sample_count)
            .expect("sample count was converted from a bounded u32"),
    })
}

fn parse_unidentified_driving_report(
    packet: &[u8],
) -> Result<TelemetryReport, TelemetryProtocolError> {
    if !UNIDENTIFIED_DRIVING_REPORT_LENGTHS.contains(&packet.len()) {
        return Err(TelemetryProtocolError::UnidentifiedLength {
            actual: packet.len(),
            allowed: UNIDENTIFIED_DRIVING_REPORT_LENGTHS,
        });
    }
    let reader = checked_reader(TelemetryRequestKind::UnidentifiedDrivingReport, packet)?;
    let _bounded_opaque_body = reader.remaining();
    Ok(TelemetryReport::UnidentifiedDrivingReport {
        logical_length: packet.len(),
    })
}

fn checked_reader(
    kind: TelemetryRequestKind,
    packet: &[u8],
) -> Result<PacketReader<'_>, TelemetryProtocolError> {
    let mut reader = PacketReader::new(packet);
    let actual = reader.read_u32()?;
    let expected = kind.request_hash();
    if actual != expected {
        return Err(TelemetryProtocolError::PacketHashMismatch {
            packet: kind.request_name(),
            actual,
            expected,
        });
    }
    Ok(reader)
}

fn require_exact_length(
    kind: TelemetryRequestKind,
    packet: &[u8],
    expected: usize,
) -> Result<(), TelemetryProtocolError> {
    if packet.len() == expected {
        Ok(())
    } else {
        Err(TelemetryProtocolError::InvalidLength {
            packet: kind.request_name(),
            actual: packet.len(),
            expected,
        })
    }
}

fn require_record_limit(
    kind: TelemetryRequestKind,
    actual: u32,
    maximum: u32,
) -> Result<(), TelemetryProtocolError> {
    if actual <= maximum {
        Ok(())
    } else {
        Err(TelemetryProtocolError::RecordLimit {
            packet: kind.request_name(),
            actual,
            maximum,
        })
    }
}

fn finish(
    reader: PacketReader<'_>,
    kind: TelemetryRequestKind,
) -> Result<(), TelemetryProtocolError> {
    if reader.remaining().is_empty() {
        Ok(())
    } else {
        Err(TelemetryProtocolError::TrailingBytes {
            packet: kind.request_name(),
            count: reader.remaining().len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_RIDE_PATH_SAMPLES, TelemetryProtocolError, TelemetryReport, TelemetryRequestKind,
        UNIDENTIFIED_DRIVING_REPORT_HASH, UNIDENTIFIED_DRIVING_REPORT_LENGTHS,
        classify_telemetry_request, parse_telemetry_request,
    };
    use crate::{adler32, packet::PacketWriter};

    fn captured(hex: &str) -> Vec<u8> {
        hex.split_ascii_whitespace()
            .map(|byte| u8::from_str_radix(byte, 16).unwrap())
            .collect()
    }

    #[test]
    fn captured_fixed_reports_are_strictly_decoded() {
        let ai = captured(
            "F8 06 0F 40 74 00 65 00 6D 00 70 00 5F 00 62 00 67 00 31 00 00 00 \
             67 00 72 00 00 00 66 00 00 00 67 00 00 00",
        );
        assert_eq!(
            parse_telemetry_request(TelemetryRequestKind::GameAiReport, &ai).unwrap(),
            TelemetryReport::GameAiReport
        );

        let frame = captured("CF 08 0A 67 7E 00 00 00 91 00 00 00 8C 00 00 00");
        assert_eq!(
            parse_telemetry_request(TelemetryRequestKind::GameClientFrame, &frame).unwrap(),
            TelemetryReport::GameClientFrame {
                local_frame: 126,
                server_frame: 145,
                acknowledged_frame: 140,
            }
        );

        let relay = captured("13 07 D1 40 00 00 00 00 9D 1C B7 A1");
        assert_eq!(
            parse_telemetry_request(TelemetryRequestKind::GameRequestRelay, &relay).unwrap(),
            TelemetryReport::GameRequestRelay {
                value: 0,
                route_hash: 0xA1B7_1C9D,
            }
        );
    }

    #[test]
    fn captured_ride_event_and_path_vectors_are_fully_consumed() {
        let event = captured(
            "0D 09 F0 69 01 00 00 00 07 00 00 00 69 00 74 00 65 00 6D 00 55 00 \
             73 00 65 00 14 29 A3 43 1C 69 4C 44 99 66 B1 41 00 00 00 00 01 0A \
             00 01 00 00 00 00 00 00 00 E0 65 00 00",
        );
        assert_eq!(
            parse_telemetry_request(TelemetryRequestKind::RideEventReport, &event).unwrap(),
            TelemetryReport::RideEventReport { event_count: 1 }
        );

        let path = captured(
            "98 08 40 60 01 00 00 00 DB A1 0F 43 03 2C 02 44 77 CC D6 41 BD B4 \
             9A 3F 00 01 00 00 00 00 00 00 00 00 00",
        );
        assert_eq!(
            parse_telemetry_request(TelemetryRequestKind::RidePathReport, &path).unwrap(),
            TelemetryReport::RidePathReport { sample_count: 1 }
        );
    }

    #[test]
    fn record_limits_are_checked_before_payload_consumption() {
        let mut path = PacketWriter::named("PcRidePathReportPacket");
        path.write_u32(MAX_RIDE_PATH_SAMPLES + 1);
        assert!(matches!(
            parse_telemetry_request(TelemetryRequestKind::RidePathReport, path.as_slice()),
            Err(TelemetryProtocolError::RecordLimit {
                actual: 65,
                maximum: 64,
                ..
            })
        ));
    }

    #[test]
    fn unidentified_hash_is_bounded_to_only_captured_lengths() {
        assert_eq!(
            classify_telemetry_request(UNIDENTIFIED_DRIVING_REPORT_HASH),
            Some(TelemetryRequestKind::UnidentifiedDrivingReport)
        );
        for length in UNIDENTIFIED_DRIVING_REPORT_LENGTHS {
            let mut packet = vec![0; length];
            packet[..4].copy_from_slice(&UNIDENTIFIED_DRIVING_REPORT_HASH.to_le_bytes());
            assert_eq!(
                parse_telemetry_request(TelemetryRequestKind::UnidentifiedDrivingReport, &packet)
                    .unwrap(),
                TelemetryReport::UnidentifiedDrivingReport {
                    logical_length: length,
                }
            );
        }
        let mut packet = vec![0; 72];
        packet[..4].copy_from_slice(&UNIDENTIFIED_DRIVING_REPORT_HASH.to_le_bytes());
        assert!(matches!(
            parse_telemetry_request(TelemetryRequestKind::UnidentifiedDrivingReport, &packet),
            Err(TelemetryProtocolError::UnidentifiedLength { actual: 72, .. })
        ));
        assert!(classify_telemetry_request(adler32::packet_hash("actually unknown")).is_none());
    }
}
