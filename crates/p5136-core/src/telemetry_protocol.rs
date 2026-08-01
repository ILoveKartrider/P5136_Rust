//! Bounded codecs for P5136 no-reply driving telemetry.
//!
//! These packets are diagnostics or client-side relay probes in the reference
//! server; they do not authorize profile or world mutation. Known structured
//! vectors are fully consumed. In particular, `0x5815082A` is the stock
//! client's misspelled `PcRideSwithInfoPacket`, not a generic unknown-packet
//! escape hatch.

use thiserror::Error;

use crate::{
    adler32,
    packet::{PacketError, PacketReader},
};

pub const GAME_AI_REPORT_NAME: &str = "GameAiReportPacket";
pub const GAME_REPORT_NAME: &str = "GameReportPacket";
pub const GAME_CLIENT_FRAME_NAME: &str = "PcGameClientFramePacket";
pub const GAME_REQUEST_RELAY_NAME: &str = "PcGameRequestRelay";
pub const GAME_BOOSTER_ADD_NAME: &str = "GameBoosterAddPacket";
pub const REPORT_STATE_IN_GAME_NAME: &str = "PcReportStateInGame";
pub const RIDE_EVENT_REPORT_NAME: &str = "PcRideEventReportPacket";
pub const RIDE_PATH_REPORT_NAME: &str = "PcRidePathReportPacket";
pub const RIDE_SWITCH_INFO_NAME: &str = "PcRideSwithInfoPacket";
pub const RIDE_SWITCH_INFO_HASH: u32 = 0x5815_082A;

pub const GAME_AI_REPORT_LENGTH: usize = 36;
pub const GAME_REPORT_LENGTH: usize = 361;
pub const GAME_CLIENT_FRAME_LENGTH: usize = 16;
pub const GAME_REQUEST_RELAY_LENGTH: usize = 12;
pub const GAME_BOOSTER_ADD_LENGTH: usize = 4;
pub const REPORT_STATE_IN_GAME_LENGTH: usize = 20;
pub const MAX_RIDE_EVENT_COUNT: u32 = 64;
pub const MAX_RIDE_EVENT_STRING_UNITS: usize = 64;
pub const MAX_RIDE_PATH_SAMPLES: u32 = 64;
pub const RIDE_PATH_SAMPLE_LENGTH: usize = 27;
pub const MAX_RIDE_SWITCH_PARTICIPANTS: u32 = 8;
pub const MAX_RIDE_SWITCH_NAME_UNITS: usize = 64;
pub const MAX_RIDE_SWITCH_SAMPLES: u32 = 64;

const GAME_REPORT_DIAGNOSTIC_CHANNEL_COUNT: u8 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryRequestKind {
    GameAiReport,
    GameReport,
    GameClientFrame,
    GameRequestRelay,
    GameBoosterAdd,
    ReportStateInGame,
    RideEventReport,
    RidePathReport,
    RideSwitchInfo,
}

impl TelemetryRequestKind {
    #[must_use]
    pub const fn request_name(self) -> &'static str {
        match self {
            Self::GameAiReport => GAME_AI_REPORT_NAME,
            Self::GameReport => GAME_REPORT_NAME,
            Self::GameClientFrame => GAME_CLIENT_FRAME_NAME,
            Self::GameRequestRelay => GAME_REQUEST_RELAY_NAME,
            Self::GameBoosterAdd => GAME_BOOSTER_ADD_NAME,
            Self::ReportStateInGame => REPORT_STATE_IN_GAME_NAME,
            Self::RideEventReport => RIDE_EVENT_REPORT_NAME,
            Self::RidePathReport => RIDE_PATH_REPORT_NAME,
            Self::RideSwitchInfo => RIDE_SWITCH_INFO_NAME,
        }
    }

    #[must_use]
    pub fn request_hash(self) -> u32 {
        match self {
            Self::RideSwitchInfo => RIDE_SWITCH_INFO_HASH,
            _ => adler32::packet_hash(self.request_name()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TelemetryReport {
    /// Eight client-owned diagnostic float bit patterns. Their individual
    /// metric names are not established, so preserve bits rather than giving
    /// them misleading semantic labels or rejecting non-finite diagnostics.
    GameAiReport {
        metric_bits: [u32; 8],
    },
    GameReport {
        protected_metric_0: i32,
        protected_metric_1: i32,
        diagnostic_channel_count: u8,
    },
    GameClientFrame {
        metric: [u32; 3],
    },
    GameRequestRelay {
        desired_peer_slot: u32,
        requester_slot: u32,
    },
    GameBoosterAdd,
    ReportStateInGame {
        report_sequence: u32,
        zero_or_reserved: u32,
        transformed_tick: u32,
        tick_validation_partner: u32,
    },
    RideEventReport {
        event_count: u32,
    },
    RidePathReport {
        sample_count: u32,
    },
    RideSwitchInfo {
        elapsed_or_total: f32,
        participant_count: u32,
        sample_count: u32,
        aggregate_values: [u32; 8],
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
        TelemetryRequestKind::GameBoosterAdd,
        TelemetryRequestKind::ReportStateInGame,
        TelemetryRequestKind::RideEventReport,
        TelemetryRequestKind::RidePathReport,
        TelemetryRequestKind::RideSwitchInfo,
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
        TelemetryRequestKind::GameBoosterAdd => parse_game_booster_add(packet),
        TelemetryRequestKind::ReportStateInGame => parse_report_state_in_game(packet),
        TelemetryRequestKind::RideEventReport => parse_ride_event_report(packet),
        TelemetryRequestKind::RidePathReport => parse_ride_path_report(packet),
        TelemetryRequestKind::RideSwitchInfo => parse_ride_switch_info(packet),
    }
}

fn parse_game_ai_report(packet: &[u8]) -> Result<TelemetryReport, TelemetryProtocolError> {
    require_exact_length(
        TelemetryRequestKind::GameAiReport,
        packet,
        GAME_AI_REPORT_LENGTH,
    )?;
    let mut reader = checked_reader(TelemetryRequestKind::GameAiReport, packet)?;
    let mut metric_bits = [0_u32; 8];
    for metric in &mut metric_bits {
        *metric = reader.read_u32()?;
    }
    finish(reader, TelemetryRequestKind::GameAiReport)?;
    Ok(TelemetryReport::GameAiReport { metric_bits })
}

fn parse_game_report(packet: &[u8]) -> Result<TelemetryReport, TelemetryProtocolError> {
    require_exact_length(TelemetryRequestKind::GameReport, packet, GAME_REPORT_LENGTH)?;
    let mut reader = checked_reader(TelemetryRequestKind::GameReport, packet)?;
    let _shared_timestamp = reader.read_bytes(18)?;
    let protected_metric_0 = reader.read_i32()?;
    let protected_metric_1 = reader.read_i32()?;
    let _encoded_header_metric = reader.read_encoded_i32()?;
    for _ in 0..usize::from(GAME_REPORT_DIAGNOSTIC_CHANNEL_COUNT) * 3 {
        let _ = reader.read_encoded_i32()?;
    }
    let _additional_encoded_metrics = [
        reader.read_encoded_i32()?,
        reader.read_encoded_i32()?,
        reader.read_encoded_i32()?,
    ];
    let _protected_float_metrics = [
        reader.read_encoded_f32()?,
        reader.read_encoded_f32()?,
        reader.read_encoded_f32()?,
    ];
    let _diagnostic_metric = reader.read_i32()?;
    let _nested_diagnostic_prefix = reader.read_bytes(20)?;
    let _nested_diagnostic_value = reader.read_i32()?;
    let _nested_diagnostic_suffix = reader.read_bytes(16)?;
    // Every captured producer appends the same 19-byte post-5136 extension
    // that the current C# handler leaves unread.
    let _post_5136_extension = reader.read_bytes(19)?;
    finish(reader, TelemetryRequestKind::GameReport)?;
    Ok(TelemetryReport::GameReport {
        protected_metric_0,
        protected_metric_1,
        diagnostic_channel_count: GAME_REPORT_DIAGNOSTIC_CHANNEL_COUNT,
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
        metric: [reader.read_u32()?, reader.read_u32()?, reader.read_u32()?],
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
        desired_peer_slot: reader.read_u32()?,
        requester_slot: reader.read_u32()?,
    };
    finish(reader, TelemetryRequestKind::GameRequestRelay)?;
    Ok(report)
}

fn parse_game_booster_add(packet: &[u8]) -> Result<TelemetryReport, TelemetryProtocolError> {
    require_exact_length(
        TelemetryRequestKind::GameBoosterAdd,
        packet,
        GAME_BOOSTER_ADD_LENGTH,
    )?;
    let reader = checked_reader(TelemetryRequestKind::GameBoosterAdd, packet)?;
    finish(reader, TelemetryRequestKind::GameBoosterAdd)?;
    Ok(TelemetryReport::GameBoosterAdd)
}

fn parse_report_state_in_game(packet: &[u8]) -> Result<TelemetryReport, TelemetryProtocolError> {
    require_exact_length(
        TelemetryRequestKind::ReportStateInGame,
        packet,
        REPORT_STATE_IN_GAME_LENGTH,
    )?;
    let mut reader = checked_reader(TelemetryRequestKind::ReportStateInGame, packet)?;
    let report = TelemetryReport::ReportStateInGame {
        report_sequence: reader.read_u32()?,
        zero_or_reserved: reader.read_u32()?,
        transformed_tick: reader.read_u32()?,
        tick_validation_partner: reader.read_u32()?,
    };
    finish(reader, TelemetryRequestKind::ReportStateInGame)?;
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
        let _phase_or_category = reader.read_u8()?;
        let _event_or_item_id = reader.read_u16()?;
        let _auxiliary_value = reader.read_u32()?;
        let _subject_nickname = reader.read_utf16_bounded(MAX_RIDE_EVENT_STRING_UNITS)?;
        let _race_tick = reader.read_u32()?;
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

fn parse_ride_switch_info(packet: &[u8]) -> Result<TelemetryReport, TelemetryProtocolError> {
    let mut reader = checked_reader(TelemetryRequestKind::RideSwitchInfo, packet)?;
    let elapsed_or_total = reader.read_f32()?;
    let participant_count = reader.read_u32()?;
    require_record_limit(
        TelemetryRequestKind::RideSwitchInfo,
        participant_count,
        MAX_RIDE_SWITCH_PARTICIPANTS,
    )?;
    for _ in 0..participant_count {
        let _nickname = reader.read_utf16_bounded(MAX_RIDE_SWITCH_NAME_UNITS)?;
        let _participant_value = reader.read_u32()?;
    }
    let sample_count = reader.read_u32()?;
    require_record_limit(
        TelemetryRequestKind::RideSwitchInfo,
        sample_count,
        MAX_RIDE_SWITCH_SAMPLES,
    )?;
    for _ in 0..sample_count {
        let _u64_like_pair = reader.read_bytes(8)?;
    }
    let mut aggregate_values = [0_u32; 8];
    for aggregate in &mut aggregate_values {
        *aggregate = reader.read_u32()?;
    }
    finish(reader, TelemetryRequestKind::RideSwitchInfo)?;
    Ok(TelemetryReport::RideSwitchInfo {
        elapsed_or_total,
        participant_count,
        sample_count,
        aggregate_values,
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
        MAX_RIDE_PATH_SAMPLES, MAX_RIDE_SWITCH_PARTICIPANTS, MAX_RIDE_SWITCH_SAMPLES,
        RIDE_SWITCH_INFO_HASH, RIDE_SWITCH_INFO_NAME, TelemetryProtocolError, TelemetryReport,
        TelemetryRequestKind, classify_telemetry_request, parse_telemetry_request,
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
            TelemetryReport::GameAiReport {
                metric_bits: [
                    0x0065_0074,
                    0x0070_006D,
                    0x0062_005F,
                    0x0031_0067,
                    0x0067_0000,
                    0x0000_0072,
                    0x0000_0066,
                    0x0000_0067,
                ],
            }
        );

        let frame = captured("CF 08 0A 67 7E 00 00 00 91 00 00 00 8C 00 00 00");
        assert_eq!(
            parse_telemetry_request(TelemetryRequestKind::GameClientFrame, &frame).unwrap(),
            TelemetryReport::GameClientFrame {
                metric: [126, 145, 140],
            }
        );

        let relay = captured("13 07 D1 40 00 00 00 00 9D 1C B7 A1");
        assert_eq!(
            parse_telemetry_request(TelemetryRequestKind::GameRequestRelay, &relay).unwrap(),
            TelemetryReport::GameRequestRelay {
                desired_peer_slot: 0,
                requester_slot: 0xA1B7_1C9D,
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
    fn ride_switch_info_uses_the_recovered_containers() {
        assert_eq!(
            classify_telemetry_request(RIDE_SWITCH_INFO_HASH),
            Some(TelemetryRequestKind::RideSwitchInfo)
        );
        assert_eq!(
            crate::adler32::packet_hash(RIDE_SWITCH_INFO_NAME),
            RIDE_SWITCH_INFO_HASH
        );
        let captured_retire_report = [
            0x2A, 0x08, 0x15, 0x58, 0xA6, 0xF4, 0xA1, 0x43, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00,
            0x00, 0x00, 0xBE, 0xE0, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x17, 0x00, 0x00, 0x00,
            0xD6, 0x6F, 0x14, 0x42, 0xC2, 0xDC, 0x44, 0x42, 0x64, 0xB8, 0x53, 0x34, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xB0, 0x00, 0x00, 0x00, 0x15, 0x00, 0x00, 0x00,
        ];
        assert_eq!(captured_retire_report.len(), 56);
        assert_eq!(
            parse_telemetry_request(
                TelemetryRequestKind::RideSwitchInfo,
                &captured_retire_report,
            )
            .unwrap(),
            TelemetryReport::RideSwitchInfo {
                elapsed_or_total: f32::from_bits(0x43A1_F4A6),
                participant_count: 0,
                sample_count: 1,
                aggregate_values: [23, 0x4214_6FD6, 0x4244_DCC2, 0x3453_B864, 0, 0, 176, 21,],
            }
        );
    }

    #[test]
    fn ride_switch_info_parses_the_captured_finish_container() {
        let captured_finish_report = [
            0x2A, 0x08, 0x15, 0x58, 0x12, 0xA7, 0xC4, 0x43, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00,
            0x00, 0x00, 0x29, 0xAF, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0xE8, 0x90, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0xC4, 0x8F, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x23, 0x00,
            0x00, 0x00, 0x69, 0xF1, 0x31, 0x42, 0xE8, 0xBB, 0x2F, 0x43, 0xCA, 0x9A, 0x69, 0x40,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0E, 0x00, 0x00, 0x00, 0x22, 0x00,
            0x00, 0x00,
        ];
        assert_eq!(captured_finish_report.len(), 72);
        assert_eq!(
            parse_telemetry_request(
                TelemetryRequestKind::RideSwitchInfo,
                &captured_finish_report,
            )
            .unwrap(),
            TelemetryReport::RideSwitchInfo {
                elapsed_or_total: f32::from_bits(0x43C4_A712),
                participant_count: 0,
                sample_count: 3,
                aggregate_values: [35, 0x4231_F169, 0x432F_BBE8, 0x4069_9ACA, 0, 0, 14, 34,],
            }
        );
    }

    #[test]
    fn ride_switch_info_parses_the_captured_post_goal_container() {
        let captured_post_goal_report = captured(
            "2A 08 15 58 67 51 80 43 01 00 00 00 04 00 00 00 \
             59 00 61 00 6E 00 79 00 07 00 00 00 03 00 00 00 \
             8C D0 01 00 00 00 00 00 28 75 00 00 00 00 00 00 \
             92 B2 00 00 00 00 00 00 17 00 00 00 B8 5B 10 42 \
             F7 32 80 42 41 59 88 3F 00 00 00 00 00 00 00 00 \
             14 00 00 00 16 00 00 00",
        );
        assert_eq!(captured_post_goal_report.len(), 88);
        assert_eq!(
            parse_telemetry_request(
                TelemetryRequestKind::RideSwitchInfo,
                &captured_post_goal_report,
            )
            .unwrap(),
            TelemetryReport::RideSwitchInfo {
                elapsed_or_total: f32::from_bits(0x4380_5167),
                participant_count: 1,
                sample_count: 3,
                aggregate_values: [23, 0x4210_5BB8, 0x4280_32F7, 0x3F88_5941, 0, 0, 20, 22,],
            }
        );
    }

    #[test]
    fn ride_switch_info_bounds_container_counts_before_consumption() {
        let mut too_many_participants = PacketWriter::named(RIDE_SWITCH_INFO_NAME);
        too_many_participants.write_f32(0.0);
        too_many_participants.write_u32(MAX_RIDE_SWITCH_PARTICIPANTS + 1);
        assert!(matches!(
            parse_telemetry_request(
                TelemetryRequestKind::RideSwitchInfo,
                too_many_participants.as_slice(),
            ),
            Err(TelemetryProtocolError::RecordLimit {
                actual,
                maximum: MAX_RIDE_SWITCH_PARTICIPANTS,
                ..
            }) if actual == MAX_RIDE_SWITCH_PARTICIPANTS + 1
        ));

        let mut too_many_samples = PacketWriter::named(RIDE_SWITCH_INFO_NAME);
        too_many_samples.write_f32(0.0);
        too_many_samples.write_u32(0);
        too_many_samples.write_u32(MAX_RIDE_SWITCH_SAMPLES + 1);
        assert!(matches!(
            parse_telemetry_request(
                TelemetryRequestKind::RideSwitchInfo,
                too_many_samples.as_slice(),
            ),
            Err(TelemetryProtocolError::RecordLimit {
                actual,
                maximum: MAX_RIDE_SWITCH_SAMPLES,
                ..
            }) if actual == MAX_RIDE_SWITCH_SAMPLES + 1
        ));
        assert!(classify_telemetry_request(adler32::packet_hash("actually unknown")).is_none());
    }

    #[test]
    fn empty_booster_and_in_game_state_reports_are_strictly_consumed() {
        let booster = PacketWriter::named("GameBoosterAddPacket");
        assert_eq!(
            parse_telemetry_request(TelemetryRequestKind::GameBoosterAdd, booster.as_slice())
                .unwrap(),
            TelemetryReport::GameBoosterAdd
        );

        let mut heartbeat = PacketWriter::named("PcReportStateInGame");
        heartbeat.write_u32(17);
        heartbeat.write_u32(0);
        heartbeat.write_u32(18);
        heartbeat.write_u32(19);
        assert_eq!(
            parse_telemetry_request(
                TelemetryRequestKind::ReportStateInGame,
                heartbeat.as_slice()
            )
            .unwrap(),
            TelemetryReport::ReportStateInGame {
                report_sequence: 17,
                zero_or_reserved: 0,
                transformed_tick: 18,
                tick_validation_partner: 19,
            }
        );
    }
}
