//! Strict codecs for one-way P5136 client events observed in the captured
//! initialization and reconnect traces.

use thiserror::Error;

use crate::{
    adler32,
    packet::{PacketError, PacketReader},
};

pub const NEW_CAREER_ITEM_STATE_NAME: &str = "PqNewCareerItemStatePacket";
pub const REPORT_UDP_RECONNECT_NAME: &str = "PqReportUdpReconnect";
const MAX_NEW_CAREER_ITEM_STATE_ENTRIES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientEventRequest {
    NewCareerItemState,
    ReportUdpReconnect,
}

impl ClientEventRequest {
    #[must_use]
    pub const fn request_name(self) -> &'static str {
        match self {
            Self::NewCareerItemState => NEW_CAREER_ITEM_STATE_NAME,
            Self::ReportUdpReconnect => REPORT_UDP_RECONNECT_NAME,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCareerItemState {
    pub career_id: i32,
    pub state: i32,
    pub entries: Vec<NewCareerItemStateEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewCareerItemStateEntry {
    pub item_id: i16,
    pub unknown_1: i32,
    pub unknown_2: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientEvent {
    NewCareerItemState(NewCareerItemState),
    ReportUdpReconnect,
}

#[derive(Debug, Error)]
pub enum ClientEventProtocolError {
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

    #[error("{name} has negative item-state entry count {count}")]
    NegativeEntryCount { name: &'static str, count: i32 },

    #[error("{name} has {count} item-state entries; configured maximum is {maximum}")]
    EntryCountOverCap {
        name: &'static str,
        count: usize,
        maximum: usize,
    },
}

#[must_use]
pub fn classify_client_event(hash: u32) -> Option<ClientEventRequest> {
    if hash == adler32::packet_hash(NEW_CAREER_ITEM_STATE_NAME) {
        Some(ClientEventRequest::NewCareerItemState)
    } else if hash == adler32::packet_hash(REPORT_UDP_RECONNECT_NAME) {
        Some(ClientEventRequest::ReportUdpReconnect)
    } else {
        None
    }
}

pub fn parse_client_event(
    request: ClientEventRequest,
    packet: &[u8],
) -> Result<ClientEvent, ClientEventProtocolError> {
    let mut reader = PacketReader::new(packet);
    let actual = reader.read_u32()?;
    let expected = adler32::packet_hash(request.request_name());
    if actual != expected {
        return Err(ClientEventProtocolError::UnexpectedPacketHash {
            name: request.request_name(),
            expected,
            actual,
        });
    }
    let event = match request {
        ClientEventRequest::NewCareerItemState => {
            let career_id = reader.read_i32()?;
            let state = reader.read_i32()?;
            let count = reader.read_i32()?;
            if count < 0 {
                return Err(ClientEventProtocolError::NegativeEntryCount {
                    name: request.request_name(),
                    count,
                });
            }
            let count = usize::try_from(count).map_err(|_| {
                ClientEventProtocolError::EntryCountOverCap {
                    name: request.request_name(),
                    count: usize::MAX,
                    maximum: MAX_NEW_CAREER_ITEM_STATE_ENTRIES,
                }
            })?;
            if count > MAX_NEW_CAREER_ITEM_STATE_ENTRIES {
                return Err(ClientEventProtocolError::EntryCountOverCap {
                    name: request.request_name(),
                    count,
                    maximum: MAX_NEW_CAREER_ITEM_STATE_ENTRIES,
                });
            }
            let mut entries = Vec::with_capacity(count);
            for _ in 0..count {
                entries.push(NewCareerItemStateEntry {
                    item_id: reader.read_i16()?,
                    unknown_1: reader.read_i32()?,
                    unknown_2: reader.read_i32()?,
                });
            }
            ClientEvent::NewCareerItemState(NewCareerItemState {
                career_id,
                state,
                entries,
            })
        }
        ClientEventRequest::ReportUdpReconnect => ClientEvent::ReportUdpReconnect,
    };
    if !reader.remaining().is_empty() {
        return Err(ClientEventProtocolError::TrailingBytes {
            name: request.request_name(),
            count: reader.remaining().len(),
        });
    }
    Ok(event)
}

#[cfg(test)]
mod tests {
    use super::{
        ClientEvent, ClientEventProtocolError, ClientEventRequest, NewCareerItemState,
        NewCareerItemStateEntry, classify_client_event, parse_client_event,
    };
    use crate::{adler32, packet::PacketWriter};

    #[test]
    fn classifier_uses_the_captured_packet_hashes() {
        assert_eq!(
            adler32::packet_hash("PqNewCareerItemStatePacket"),
            0x86CF_0A25
        );
        assert_eq!(adler32::packet_hash("PqReportUdpReconnect"), 0x5305_0807);
        assert_eq!(
            classify_client_event(0x86CF_0A25),
            Some(ClientEventRequest::NewCareerItemState)
        );
        assert_eq!(
            classify_client_event(0x5305_0807),
            Some(ClientEventRequest::ReportUdpReconnect)
        );
        assert_eq!(classify_client_event(0xDEAD_BEEF), None);
    }

    #[test]
    fn captured_counted_career_and_reconnect_shapes_are_exact() {
        let single_entry = [
            0x25, 0x0A, 0xCF, 0x86, 0x0A, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 8, 0, 1, 0, 0, 0, 1, 0,
            0, 0,
        ];
        assert_eq!(
            parse_client_event(ClientEventRequest::NewCareerItemState, &single_entry).unwrap(),
            ClientEvent::NewCareerItemState(NewCareerItemState {
                career_id: 10,
                state: 1,
                entries: vec![NewCareerItemStateEntry {
                    item_id: 8,
                    unknown_1: 1,
                    unknown_2: 1,
                }],
            })
        );

        let barricade_hit = [
            0x25, 0x0A, 0xCF, 0x86, 1, 0, 0, 0, 5, 0, 0, 0, 2, 0, 0, 0, 11, 0, 1, 0, 0, 0, 1, 0, 0,
            0, 113, 0, 1, 0, 0, 0, 1, 0, 0, 0,
        ];
        assert_eq!(
            parse_client_event(ClientEventRequest::NewCareerItemState, &barricade_hit).unwrap(),
            ClientEvent::NewCareerItemState(NewCareerItemState {
                career_id: 1,
                state: 5,
                entries: vec![
                    NewCareerItemStateEntry {
                        item_id: 11,
                        unknown_1: 1,
                        unknown_2: 1,
                    },
                    NewCareerItemStateEntry {
                        item_id: 113,
                        unknown_1: 1,
                        unknown_2: 1,
                    },
                ],
            })
        );

        let reconnect = PacketWriter::named("PqReportUdpReconnect").into_inner();
        assert_eq!(
            parse_client_event(ClientEventRequest::ReportUdpReconnect, &reconnect).unwrap(),
            ClientEvent::ReportUdpReconnect
        );

        let mut trailing = reconnect;
        trailing.push(0);
        assert!(matches!(
            parse_client_event(ClientEventRequest::ReportUdpReconnect, &trailing),
            Err(ClientEventProtocolError::TrailingBytes {
                name: "PqReportUdpReconnect",
                count: 1,
            })
        ));
    }

    #[test]
    fn career_item_state_rejects_negative_oversized_and_inexact_vectors() {
        let mut negative = PacketWriter::named("PqNewCareerItemStatePacket");
        negative.write_i32(1);
        negative.write_i32(2);
        negative.write_i32(-1);
        assert!(matches!(
            parse_client_event(
                ClientEventRequest::NewCareerItemState,
                &negative.into_inner()
            ),
            Err(ClientEventProtocolError::NegativeEntryCount { count: -1, .. })
        ));

        let mut oversized = PacketWriter::named("PqNewCareerItemStatePacket");
        oversized.write_i32(1);
        oversized.write_i32(2);
        oversized.write_i32(65);
        assert!(matches!(
            parse_client_event(
                ClientEventRequest::NewCareerItemState,
                &oversized.into_inner()
            ),
            Err(ClientEventProtocolError::EntryCountOverCap {
                count: 65,
                maximum: 64,
                ..
            })
        ));

        let mut truncated = PacketWriter::named("PqNewCareerItemStatePacket");
        truncated.write_i32(1);
        truncated.write_i32(2);
        truncated.write_i32(1);
        truncated.write_i16(113);
        assert!(matches!(
            parse_client_event(
                ClientEventRequest::NewCareerItemState,
                &truncated.into_inner()
            ),
            Err(ClientEventProtocolError::Packet(_))
        ));

        let mut trailing = PacketWriter::named("PqNewCareerItemStatePacket");
        trailing.write_i32(1);
        trailing.write_i32(2);
        trailing.write_i32(0);
        let mut trailing = trailing.into_inner();
        trailing.push(0);
        assert!(matches!(
            parse_client_event(ClientEventRequest::NewCareerItemState, &trailing),
            Err(ClientEventProtocolError::TrailingBytes { count: 1, .. })
        ));
    }
}
