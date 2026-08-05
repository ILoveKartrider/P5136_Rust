//! Fixed-width P5136 legacy Floater/socket request and response codecs.

use thiserror::Error;

use crate::{
    adler32,
    inventory::TuneExcRecord,
    packet::{PacketError, PacketReader, PacketWriter},
};

pub const USE_SOCKET_REQUEST_NAME: &str = "PqUseSocketItem";
pub const USE_SOCKET_REPLY_NAME: &str = "PrUseSocketItem";
pub const USE_TUNE_REQUEST_NAME: &str = "PqUseTuneItem";
pub const USE_TUNE_REPLY_NAME: &str = "PrUseTuneItem";
pub const USE_PROTECT_SPANNER_REQUEST_NAME: &str = "PqUseProtectSpannerItem";
pub const USE_PROTECT_SPANNER_REPLY_NAME: &str = "PrUseProtectSpannerItem";
pub const USE_RESET_SOCKET_REQUEST_NAME: &str = "PqUseResetSocketItem";
pub const USE_RESET_SOCKET_REPLY_NAME: &str = "PrUseResetSocketItem";

pub const SIMPLE_FLOATER_REQUEST_WIRE_LENGTH: usize = 14;
pub const PROTECT_FLOATER_REQUEST_WIRE_LENGTH: usize = 18;

pub const FLOATER_RESULT_SUCCESS: i32 = 0;
pub const FLOATER_RESULT_FAILURE: i32 = 1;
pub const FLOATER_PROTECT_RESULT_KART_UNAVAILABLE: i32 = 2;
pub const FLOATER_PROTECT_RESULT_SOCKET_MISSING: i32 = 3;
pub const FLOATER_PROTECT_RESULT_ALREADY_PROTECTED: i32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloaterItemRequest {
    /// Activation/reset consumable ID. For tune requests this is also the C#
    /// pool selector (4=item, 5=Black fixed set, 6=speed).
    pub consumable_id: i16,
    /// Inventory category of the target object. Native P5136 sends category 3
    /// for karts; this is not another consumable ID.
    pub kart_category: i16,
    pub kart_id: i16,
    /// Retained producer field between kart ID and serial. The C# handler does
    /// not use it, but exact parsing prevents length drift.
    pub kart_auxiliary: i16,
    pub kart_serial: i16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloaterProtectRequest {
    pub protect_kind: i16,
    pub consumable_id: i16,
    pub kart_category: i16,
    pub kart_id: i16,
    pub kart_serial: i16,
    pub reserved: i16,
    pub slot: i16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloaterRequestKind {
    ActivateSocket,
    ApplyTune,
    ProtectSlot,
    ResetSocket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloaterRequest {
    ActivateSocket(FloaterItemRequest),
    ApplyTune(FloaterItemRequest),
    ProtectSlot(FloaterProtectRequest),
    ResetSocket(FloaterItemRequest),
}

#[derive(Debug, Error)]
pub enum FloaterProtocolError {
    #[error(transparent)]
    Packet(#[from] PacketError),

    #[error("expected {name} hash 0x{expected:08X}, received 0x{actual:08X}")]
    UnexpectedPacketHash {
        name: &'static str,
        expected: u32,
        actual: u32,
    },

    #[error("packet hash 0x{0:08X} is not a P5136 Floater request")]
    UnknownRequestHash(u32),

    #[error("packet {name} has {count} unexpected trailing bytes")]
    TrailingBytes { name: &'static str, count: usize },
}

#[must_use]
pub fn classify_floater_request(hash: u32) -> Option<FloaterRequestKind> {
    if hash == adler32::packet_hash(USE_SOCKET_REQUEST_NAME) {
        Some(FloaterRequestKind::ActivateSocket)
    } else if hash == adler32::packet_hash(USE_TUNE_REQUEST_NAME) {
        Some(FloaterRequestKind::ApplyTune)
    } else if hash == adler32::packet_hash(USE_PROTECT_SPANNER_REQUEST_NAME) {
        Some(FloaterRequestKind::ProtectSlot)
    } else if hash == adler32::packet_hash(USE_RESET_SOCKET_REQUEST_NAME) {
        Some(FloaterRequestKind::ResetSocket)
    } else {
        None
    }
}

pub fn parse_floater_request(packet: &[u8]) -> Result<FloaterRequest, FloaterProtocolError> {
    let mut reader = PacketReader::new(packet);
    let hash = reader.read_u32()?;
    match classify_floater_request(hash) {
        Some(FloaterRequestKind::ActivateSocket) => Ok(FloaterRequest::ActivateSocket(
            parse_item_request(packet, USE_SOCKET_REQUEST_NAME)?,
        )),
        Some(FloaterRequestKind::ApplyTune) => Ok(FloaterRequest::ApplyTune(parse_item_request(
            packet,
            USE_TUNE_REQUEST_NAME,
        )?)),
        Some(FloaterRequestKind::ProtectSlot) => {
            Ok(FloaterRequest::ProtectSlot(parse_protect_request(packet)?))
        }
        Some(FloaterRequestKind::ResetSocket) => Ok(FloaterRequest::ResetSocket(
            parse_item_request(packet, USE_RESET_SOCKET_REQUEST_NAME)?,
        )),
        None => Err(FloaterProtocolError::UnknownRequestHash(hash)),
    }
}

fn parse_item_request(
    packet: &[u8],
    name: &'static str,
) -> Result<FloaterItemRequest, FloaterProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, name)?;
    let request = FloaterItemRequest {
        consumable_id: reader.read_i16()?,
        kart_category: reader.read_i16()?,
        kart_id: reader.read_i16()?,
        kart_auxiliary: reader.read_i16()?,
        kart_serial: reader.read_i16()?,
    };
    ensure_exhausted(&reader, name)?;
    Ok(request)
}

fn parse_protect_request(packet: &[u8]) -> Result<FloaterProtectRequest, FloaterProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, USE_PROTECT_SPANNER_REQUEST_NAME)?;
    let request = FloaterProtectRequest {
        protect_kind: reader.read_i16()?,
        consumable_id: reader.read_i16()?,
        kart_category: reader.read_i16()?,
        kart_id: reader.read_i16()?,
        kart_serial: reader.read_i16()?,
        reserved: reader.read_i16()?,
        slot: reader.read_i16()?,
    };
    ensure_exhausted(&reader, USE_PROTECT_SPANNER_REQUEST_NAME)?;
    Ok(request)
}

#[must_use]
pub fn serialize_use_socket_reply(
    request: FloaterItemRequest,
    result: i32,
    state: TuneExcRecord,
) -> Vec<u8> {
    let mut packet = PacketWriter::named(USE_SOCKET_REPLY_NAME);
    packet.write_i32(result);
    packet.write_i16(request.consumable_id);
    packet.write_i16(request.kart_category);
    packet.write_i16(request.kart_id);
    // Native `sub_D516E0` consumes this as the initial Tune record state. The
    // C# implementation duplicated the serial here, corrupting serials > 1.
    packet.write_i16(0);
    packet.write_i16(request.kart_serial);
    packet.write_i16(2);
    write_tune_fields(&mut packet, state);
    packet.into_inner()
}

#[must_use]
pub fn serialize_use_tune_reply(
    request: FloaterItemRequest,
    result: i32,
    state: TuneExcRecord,
) -> Vec<u8> {
    let mut packet = PacketWriter::named(USE_TUNE_REPLY_NAME);
    packet.write_i32(result);
    packet.write_i16(request.consumable_id);
    packet.write_i16(request.kart_category);
    packet.write_i16(request.kart_id);
    packet.write_i16(request.kart_serial);
    packet.write_i16(0);
    write_tune_fields(&mut packet, state);
    packet.into_inner()
}

#[must_use]
pub fn serialize_use_protect_spanner_reply(
    request: FloaterProtectRequest,
    result: i32,
    state: TuneExcRecord,
) -> Vec<u8> {
    let mut packet = PacketWriter::named(USE_PROTECT_SPANNER_REPLY_NAME);
    packet.write_i32(result);
    packet.write_i16(request.protect_kind);
    packet.write_i16(request.consumable_id);
    packet.write_i16(request.kart_category);
    packet.write_i16(request.kart_id);
    packet.write_i16(request.kart_serial);
    packet.write_i16(0);
    packet.write_i16(0);
    packet.write_i16(0);
    write_tune_fields(&mut packet, state);
    packet.into_inner()
}

#[must_use]
pub fn serialize_use_reset_socket_reply(
    request: FloaterItemRequest,
    result: i32,
    state_after_reset: TuneExcRecord,
) -> Vec<u8> {
    let mut packet = PacketWriter::named(USE_RESET_SOCKET_REPLY_NAME);
    packet.write_i32(result);
    packet.write_i16(request.consumable_id);
    packet.write_i16(request.kart_category);
    packet.write_i16(request.kart_id);
    packet.write_i16(request.kart_serial);
    // The native compatibility reply reports one returned Floater crystal.
    packet.write_i16(34);
    packet.write_i16(76);
    packet.write_i16(1);
    packet.write_i16(1);
    write_tune_fields(&mut packet, state_after_reset);
    packet.into_inner()
}

fn write_tune_fields(packet: &mut PacketWriter, state: TuneExcRecord) {
    packet.write_i16(state.tune1);
    packet.write_i16(state.tune2);
    packet.write_i16(state.tune3);
    packet.write_i16(state.slot1);
    packet.write_i16(state.count1);
    packet.write_i16(state.slot2);
    packet.write_i16(state.count2);
}

fn expect_hash(
    reader: &mut PacketReader<'_>,
    name: &'static str,
) -> Result<(), FloaterProtocolError> {
    let actual = reader.read_u32()?;
    let expected = adler32::packet_hash(name);
    if actual == expected {
        Ok(())
    } else {
        Err(FloaterProtocolError::UnexpectedPacketHash {
            name,
            expected,
            actual,
        })
    }
}

fn ensure_exhausted(
    reader: &PacketReader<'_>,
    name: &'static str,
) -> Result<(), FloaterProtocolError> {
    let count = reader.remaining().len();
    if count == 0 {
        Ok(())
    } else {
        Err(FloaterProtocolError::TrailingBytes { name, count })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item_request(name: &'static str) -> Vec<u8> {
        let mut packet = PacketWriter::named(name);
        for value in [6_i16, 3, 633, 0, 2] {
            packet.write_i16(value);
        }
        packet.into_inner()
    }

    fn state() -> TuneExcRecord {
        TuneExcRecord {
            id: 633,
            serial: 2,
            tune1: 603,
            tune2: 703,
            tune3: 903,
            slot1: 0,
            count1: 4,
            slot2: 2,
            count2: 3,
        }
    }

    #[test]
    fn packet_hashes_match_the_p5136_enum() {
        assert_eq!(adler32::packet_hash(USE_TUNE_REQUEST_NAME), 589_628_697);
        assert_eq!(adler32::packet_hash(USE_TUNE_REPLY_NAME), 590_415_130);
        assert_eq!(adler32::packet_hash(USE_SOCKET_REQUEST_NAME), 778_896_870);
        assert_eq!(adler32::packet_hash(USE_SOCKET_REPLY_NAME), 779_814_375);
        assert_eq!(
            adler32::packet_hash(USE_RESET_SOCKET_REQUEST_NAME),
            1_375_078_377
        );
        assert_eq!(
            adler32::packet_hash(USE_PROTECT_SPANNER_REQUEST_NAME),
            1_835_010_357
        );
    }

    #[test]
    fn all_requests_are_fixed_width_and_fully_consumed() {
        for (name, expected_kind) in [
            (USE_SOCKET_REQUEST_NAME, FloaterRequestKind::ActivateSocket),
            (USE_TUNE_REQUEST_NAME, FloaterRequestKind::ApplyTune),
            (
                USE_RESET_SOCKET_REQUEST_NAME,
                FloaterRequestKind::ResetSocket,
            ),
        ] {
            let packet = item_request(name);
            assert_eq!(packet.len(), SIMPLE_FLOATER_REQUEST_WIRE_LENGTH);
            assert_eq!(
                classify_floater_request(u32::from_le_bytes(packet[..4].try_into().unwrap())),
                Some(expected_kind)
            );
            assert!(parse_floater_request(&packet).is_ok());
            for length in 0..packet.len() {
                assert!(parse_floater_request(&packet[..length]).is_err());
            }
            let mut trailing = packet;
            trailing.push(0);
            assert!(matches!(
                parse_floater_request(&trailing),
                Err(FloaterProtocolError::TrailingBytes { count: 1, .. })
            ));
        }

        let mut protect = PacketWriter::named(USE_PROTECT_SPANNER_REQUEST_NAME);
        for value in [49_i16, 1, 3, 633, 2, 0, 1] {
            protect.write_i16(value);
        }
        let protect = protect.into_inner();
        assert_eq!(protect.len(), PROTECT_FLOATER_REQUEST_WIRE_LENGTH);
        assert_eq!(
            parse_floater_request(&protect).unwrap(),
            FloaterRequest::ProtectSlot(FloaterProtectRequest {
                protect_kind: 49,
                consumable_id: 1,
                kart_category: 3,
                kart_id: 633,
                kart_serial: 2,
                reserved: 0,
                slot: 1,
            })
        );
    }

    #[test]
    fn replies_preserve_the_csharp_fixed_field_order() {
        let FloaterRequest::ApplyTune(request) =
            parse_floater_request(&item_request(USE_TUNE_REQUEST_NAME)).unwrap()
        else {
            unreachable!()
        };
        let tune = serialize_use_tune_reply(request, FLOATER_RESULT_SUCCESS, state());
        assert_eq!(tune.len(), 32);
        assert_eq!(&tune[4..8], &0_i32.to_le_bytes());
        assert_eq!(&tune[16..18], &0_i16.to_le_bytes());
        assert_eq!(&tune[18..24], &[91, 2, 191, 2, 135, 3]);

        let socket = serialize_use_socket_reply(request, FLOATER_RESULT_SUCCESS, state());
        assert_eq!(socket.len(), 34);
        assert_eq!(&socket[14..16], &0_i16.to_le_bytes());
        assert_eq!(&socket[16..18], &2_i16.to_le_bytes());
        assert_eq!(&socket[18..20], &2_i16.to_le_bytes());
        assert_eq!(&socket[20..34], &tune[18..32]);
        let reset = serialize_use_reset_socket_reply(request, FLOATER_RESULT_SUCCESS, state());
        assert_eq!(reset.len(), 38);
        assert_eq!(&reset[16..24], &[34, 0, 76, 0, 1, 0, 1, 0]);
        assert_eq!(&reset[24..38], &tune[18..32]);
    }
}
