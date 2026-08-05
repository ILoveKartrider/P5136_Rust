//! P5136 rider-equipment, plant-part, and X-parts codecs.

use thiserror::Error;

use crate::{
    adler32,
    packet::{PacketError, PacketReader, PacketWriter},
    startup::RIDER_ITEM_SNAPSHOT_WIRE_LENGTH,
};

pub const SET_RIDER_ITEMS_REQUEST_NAME: &str = "LoRqSetRiderItemOnPacket";
pub const EQUIP_PLANT_PART_REQUEST_NAME: &str = "PqEquipTuningExPacket";
pub const EQUIP_TUNING_REPLY_NAME: &str = "PrEquipTuningPacket";
pub const EQUIP_X_PART_REQUEST_NAME: &str = "PqEquipXPartsItem";
pub const EQUIP_X_PART_REPLY_NAME: &str = "PrEquipXPartsItem";
pub const ROOM_SLOT_ITEMS_PACKET_NAME: &str = "GrSlotItemOnPacket";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EquipmentRequest {
    SetRiderItems,
    EquipPlantPart,
    EquipXPart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RiderItemSelection {
    pub character: u16,
    pub paint: u16,
    pub kart: u16,
    pub plate: u16,
    pub goggle: u16,
    pub balloon: u16,
    pub unknown1: u16,
    pub head_band: u16,
    pub head_phone: u16,
    pub hand_gear_left: u16,
    pub unknown2: u16,
    pub uniform: u16,
    pub decal: u16,
    pub pet: u16,
    pub flying_pet: u16,
    pub aura: u16,
    pub skid_mark: u16,
    pub special_kit: u16,
    pub rider_color: u16,
    pub bonus_card: u16,
    pub boss_mode_card: u16,
    /// Plant engine item (inventory category 43).
    pub kart_plant1: u16,
    /// Plant wheel item (inventory category 45). P5136 places this before the
    /// handle in the rider-item wire snapshot.
    pub kart_plant2: u16,
    /// Plant handle item (inventory category 44).
    pub kart_plant3: u16,
    /// Plant kit item (inventory category 46).
    pub kart_plant4: u16,
    pub unknown3: u16,
    pub fishing_pole: u16,
    pub tachometer: u16,
    pub dye: u16,
    pub kart_serial: u16,
    pub unknown4: u8,
    pub kart_coating: u16,
    pub kart_tail_lamp: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlantPartEquipRequest {
    pub item_category: i16,
    pub item_id: i16,
    pub kart_category: i16,
    pub kart_id: i16,
    pub kart_serial: i16,
    /// Stock P5136 also identifies the part that was displaced by this
    /// operation. An all-zero descriptor means that the destination slot was
    /// empty. The server does not need to consume the old part, but retaining
    /// the descriptor keeps the native request semantics explicit.
    pub replaced_part: Option<PlantPartDescriptor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlantPartDescriptor {
    pub item_category: i16,
    pub item_id: i16,
    pub kart_category: i16,
    pub kart_id: i16,
    pub kart_serial: i16,
}

impl PlantPartDescriptor {
    pub const EMPTY: Self = Self {
        item_category: 0,
        item_id: 0,
        kart_category: 0,
        kart_id: 0,
        kart_serial: 0,
    };
}

/// The exact 18-byte body emitted by P5136 for one X-parts selection.
///
/// The four fields whose meaning is still unknown are retained and echoed
/// verbatim. Only the fields consumed by the C# persistence path are used for
/// state mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XPartEquipRequest {
    pub kart_id: i16,
    pub kart_serial: i16,
    pub item_category: i16,
    pub item_id: i16,
    pub quantity: i16,
    pub unknown_1: i16,
    pub grade: u8,
    pub unknown_2: u8,
    pub parts_value: i16,
    pub unknown_3: i16,
}

#[derive(Debug, Error)]
pub enum EquipmentProtocolError {
    #[error(transparent)]
    Packet(#[from] PacketError),

    #[error("expected {name} hash 0x{expected:08X}, received 0x{actual:08X}")]
    UnexpectedPacketHash {
        name: &'static str,
        expected: u32,
        actual: u32,
    },

    #[error("packet {name} has {count} unexpected trailing bytes")]
    TrailingBytes { name: &'static str, count: usize },

    #[error("plant part category {0} is outside 43..=46")]
    InvalidPlantPartCategory(i16),

    #[error("plant part item ID {0} cannot be negative")]
    InvalidPlantPartItem(i16),

    #[error("plant target kart ID {0} must be positive")]
    InvalidPlantKart(i16),

    #[error("X-parts category {0} is not one of 63..=66, 68, or 69")]
    InvalidXPartCategory(i16),

    #[error("X-parts item ID {0} cannot be negative")]
    InvalidXPartItem(i16),

    #[error("X-parts target kart ID {0} must be positive")]
    InvalidXPartKart(i16),

    #[error("room equipment player ID {0} is outside 0..=15")]
    InvalidRoomPlayerId(i32),
}

#[must_use]
pub fn classify_equipment_request(hash: u32) -> Option<EquipmentRequest> {
    if hash == adler32::packet_hash(SET_RIDER_ITEMS_REQUEST_NAME) {
        Some(EquipmentRequest::SetRiderItems)
    } else if hash == adler32::packet_hash(EQUIP_PLANT_PART_REQUEST_NAME) {
        Some(EquipmentRequest::EquipPlantPart)
    } else if hash == adler32::packet_hash(EQUIP_X_PART_REQUEST_NAME) {
        Some(EquipmentRequest::EquipXPart)
    } else {
        None
    }
}

pub fn parse_set_rider_items(packet: &[u8]) -> Result<RiderItemSelection, EquipmentProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, SET_RIDER_ITEMS_REQUEST_NAME)?;
    let selection = RiderItemSelection {
        character: reader.read_u16()?,
        paint: reader.read_u16()?,
        kart: reader.read_u16()?,
        plate: reader.read_u16()?,
        goggle: reader.read_u16()?,
        balloon: reader.read_u16()?,
        unknown1: reader.read_u16()?,
        head_band: reader.read_u16()?,
        head_phone: reader.read_u16()?,
        hand_gear_left: reader.read_u16()?,
        unknown2: reader.read_u16()?,
        uniform: reader.read_u16()?,
        decal: reader.read_u16()?,
        pet: reader.read_u16()?,
        flying_pet: reader.read_u16()?,
        aura: reader.read_u16()?,
        skid_mark: reader.read_u16()?,
        special_kit: reader.read_u16()?,
        rider_color: reader.read_u16()?,
        bonus_card: reader.read_u16()?,
        boss_mode_card: reader.read_u16()?,
        kart_plant1: reader.read_u16()?,
        kart_plant2: reader.read_u16()?,
        kart_plant3: reader.read_u16()?,
        kart_plant4: reader.read_u16()?,
        unknown3: reader.read_u16()?,
        fishing_pole: reader.read_u16()?,
        tachometer: reader.read_u16()?,
        dye: reader.read_u16()?,
        kart_serial: reader.read_u16()?,
        unknown4: reader.read_u8()?,
        kart_coating: reader.read_u16()?,
        kart_tail_lamp: reader.read_u16()?,
    };
    // The P5136 C# handler reads exactly one 65-byte rider snapshot from the
    // request body and ignores any bytes that follow it.
    Ok(RiderItemSelection {
        kart_serial: normalize_kart_serial(selection.kart, selection.kart_serial),
        ..selection
    })
}

pub fn parse_equip_plant_part(
    packet: &[u8],
) -> Result<PlantPartEquipRequest, EquipmentProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, EQUIP_PLANT_PART_REQUEST_NAME)?;
    let mut request = PlantPartEquipRequest {
        item_category: reader.read_i16()?,
        item_id: reader.read_i16()?,
        kart_category: reader.read_i16()?,
        kart_id: reader.read_i16()?,
        kart_serial: reader.read_i16()?,
        replaced_part: None,
    };
    // Retained C# fixtures contain only the first descriptor. The stock 5136
    // writer appends a second five-i16 descriptor for the displaced part.
    // Accept the retained legacy shape as well as the exact stock shape, but
    // reject every other size.
    match reader.remaining().len() {
        0 => {}
        10 => {
            let mut replaced = PlantPartDescriptor {
                item_category: reader.read_i16()?,
                item_id: reader.read_i16()?,
                kart_category: reader.read_i16()?,
                kart_id: reader.read_i16()?,
                kart_serial: reader.read_i16()?,
            };
            if replaced != PlantPartDescriptor::EMPTY {
                replaced.kart_serial =
                    normalize_signed_kart_serial(replaced.kart_id, replaced.kart_serial);
                request.replaced_part = Some(replaced);
            }
        }
        count => {
            return Err(EquipmentProtocolError::TrailingBytes {
                name: EQUIP_PLANT_PART_REQUEST_NAME,
                count,
            });
        }
    }
    if !(43..=46).contains(&request.item_category) {
        return Err(EquipmentProtocolError::InvalidPlantPartCategory(
            request.item_category,
        ));
    }
    if request.item_id < 0 {
        return Err(EquipmentProtocolError::InvalidPlantPartItem(
            request.item_id,
        ));
    }
    if request.kart_id <= 0 {
        return Err(EquipmentProtocolError::InvalidPlantKart(request.kart_id));
    }
    request.kart_serial = normalize_signed_kart_serial(request.kart_id, request.kart_serial);
    Ok(request)
}

pub fn parse_equip_x_part(packet: &[u8]) -> Result<XPartEquipRequest, EquipmentProtocolError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, EQUIP_X_PART_REQUEST_NAME)?;
    let request = XPartEquipRequest {
        kart_id: reader.read_i16()?,
        kart_serial: reader.read_i16()?,
        item_category: reader.read_i16()?,
        item_id: reader.read_i16()?,
        quantity: reader.read_i16()?,
        unknown_1: reader.read_i16()?,
        grade: reader.read_u8()?,
        unknown_2: reader.read_u8()?,
        parts_value: reader.read_i16()?,
        unknown_3: reader.read_i16()?,
    };
    ensure_exhausted(&reader, EQUIP_X_PART_REQUEST_NAME)?;
    if !matches!(request.item_category, 63..=66 | 68 | 69) {
        return Err(EquipmentProtocolError::InvalidXPartCategory(
            request.item_category,
        ));
    }
    if request.item_id < 0 {
        return Err(EquipmentProtocolError::InvalidXPartItem(request.item_id));
    }
    if request.kart_id <= 0 {
        return Err(EquipmentProtocolError::InvalidXPartKart(request.kart_id));
    }
    Ok(request)
}

#[must_use]
pub fn serialize_equip_tuning_failure() -> Vec<u8> {
    let mut packet = PacketWriter::named(EQUIP_TUNING_REPLY_NAME);
    // The native decoder always consumes `u8 result + 5*i16`, including on
    // failure. The retained C# implementation emitted a truncated i32 body.
    packet.write_u8(0);
    for _ in 0..5 {
        packet.write_i16(0);
    }
    packet.into_inner()
}

#[must_use]
pub fn serialize_equip_tuning_success(request: PlantPartEquipRequest) -> Vec<u8> {
    let mut packet = PacketWriter::named(EQUIP_TUNING_REPLY_NAME);
    packet.write_u8(1);
    packet.write_i16(request.kart_serial);
    packet.write_i16(request.kart_serial);
    packet.write_i16(request.kart_id);
    packet.write_i16(request.item_category);
    packet.write_i16(request.item_id);
    packet.into_inner()
}

/// Serializes the C# `result = 0` response followed by an exact request echo.
#[must_use]
pub fn serialize_equip_x_part_success(request: XPartEquipRequest) -> Vec<u8> {
    serialize_equip_x_part_reply(0, request)
}

/// Serializes an inferred non-terminal rejection followed by the exact echo.
///
/// Only result `0` is present in the retained C#/client traffic. Result `1` is
/// an explicit compatibility inference from the leading `i32` result field;
/// keeping the fixed reply layout avoids turning rejected input into a
/// transport failure, but does not claim a captured failure golden.
#[must_use]
pub fn serialize_equip_x_part_failure(request: XPartEquipRequest) -> Vec<u8> {
    serialize_equip_x_part_reply(1, request)
}

fn serialize_equip_x_part_reply(result: i32, request: XPartEquipRequest) -> Vec<u8> {
    let mut packet = PacketWriter::named(EQUIP_X_PART_REPLY_NAME);
    packet.write_i32(result);
    packet.write_i16(request.kart_id);
    packet.write_i16(request.kart_serial);
    packet.write_i16(request.item_category);
    packet.write_i16(request.item_id);
    packet.write_i16(request.quantity);
    packet.write_i16(request.unknown_1);
    packet.write_u8(request.grade);
    packet.write_u8(request.unknown_2);
    packet.write_i16(request.parts_value);
    packet.write_i16(request.unknown_3);
    packet.into_inner()
}

pub fn serialize_room_slot_items(
    player_id: i32,
    rider_item_snapshot: &[u8; RIDER_ITEM_SNAPSHOT_WIRE_LENGTH],
) -> Result<Vec<u8>, EquipmentProtocolError> {
    if !(0..=15).contains(&player_id) {
        return Err(EquipmentProtocolError::InvalidRoomPlayerId(player_id));
    }
    let mut packet = PacketWriter::named(ROOM_SLOT_ITEMS_PACKET_NAME);
    packet.write_i32(player_id);
    packet.write_bytes(rider_item_snapshot);
    Ok(packet.into_inner())
}

fn expect_hash(
    reader: &mut PacketReader<'_>,
    name: &'static str,
) -> Result<(), EquipmentProtocolError> {
    let actual = reader.read_u32()?;
    let expected = adler32::packet_hash(name);
    if actual == expected {
        Ok(())
    } else {
        Err(EquipmentProtocolError::UnexpectedPacketHash {
            name,
            expected,
            actual,
        })
    }
}

fn ensure_exhausted(
    reader: &PacketReader<'_>,
    name: &'static str,
) -> Result<(), EquipmentProtocolError> {
    if reader.remaining().is_empty() {
        Ok(())
    } else {
        Err(EquipmentProtocolError::TrailingBytes {
            name,
            count: reader.remaining().len(),
        })
    }
}

const fn normalize_kart_serial(kart_id: u16, serial: u16) -> u16 {
    if kart_id != 0 && serial == 0 {
        1
    } else {
        serial
    }
}

const fn normalize_signed_kart_serial(kart_id: i16, serial: i16) -> i16 {
    if kart_id != 0 && serial == 0 {
        1
    } else {
        serial
    }
}

const _: () = assert!(RIDER_ITEM_SNAPSHOT_WIRE_LENGTH == 65);

#[cfg(test)]
mod tests {
    use super::{
        EQUIP_PLANT_PART_REQUEST_NAME, EQUIP_X_PART_REQUEST_NAME, EquipmentProtocolError,
        EquipmentRequest, PlantPartDescriptor, PlantPartEquipRequest, RiderItemSelection,
        SET_RIDER_ITEMS_REQUEST_NAME, XPartEquipRequest, classify_equipment_request,
        parse_equip_plant_part, parse_equip_x_part, parse_set_rider_items,
        serialize_equip_tuning_failure, serialize_equip_tuning_success,
        serialize_equip_x_part_failure, serialize_equip_x_part_success, serialize_room_slot_items,
    };
    use crate::{adler32, packet::PacketWriter};

    #[test]
    fn classifier_uses_the_exact_p5136_packet_names() {
        assert_eq!(
            classify_equipment_request(0x7234_0944),
            Some(EquipmentRequest::SetRiderItems)
        );
        assert_eq!(
            classify_equipment_request(0x5AB9_084F),
            Some(EquipmentRequest::EquipPlantPart)
        );
        assert_eq!(
            classify_equipment_request(0x3B5A_06B6),
            Some(EquipmentRequest::EquipXPart)
        );
        assert_eq!(
            adler32::packet_hash(SET_RIDER_ITEMS_REQUEST_NAME),
            0x7234_0944
        );
        assert_eq!(
            adler32::packet_hash(EQUIP_PLANT_PART_REQUEST_NAME),
            0x5AB9_084F
        );
        assert_eq!(adler32::packet_hash(EQUIP_X_PART_REQUEST_NAME), 0x3B5A_06B6);
        assert_eq!(classify_equipment_request(0xDEAD_BEEF), None);
    }

    #[test]
    fn rider_item_selection_matches_the_exact_65_byte_csharp_order() {
        let mut packet = PacketWriter::named(SET_RIDER_ITEMS_REQUEST_NAME);
        for value in 1_u16..=30 {
            packet.write_u16(value);
        }
        packet.write_u8(0xA5);
        packet.write_u16(0x1234);
        packet.write_u16(0x5678);
        let parsed = parse_set_rider_items(&packet.into_inner()).unwrap();

        assert_eq!(parsed.character, 1);
        assert_eq!(parsed.kart, 3);
        assert_eq!(parsed.pet, 14);
        assert_eq!(parsed.kart_plant4, 25);
        assert_eq!(parsed.kart_serial, 30);
        assert_eq!(parsed.unknown4, 0xA5);
        assert_eq!(parsed.kart_coating, 0x1234);
        assert_eq!(parsed.kart_tail_lamp, 0x5678);
    }

    #[test]
    fn nonzero_kart_with_zero_serial_is_normalized() {
        let mut packet = PacketWriter::named(SET_RIDER_ITEMS_REQUEST_NAME);
        for index in 0..30 {
            packet.write_u16(if index == 2 { 1_450 } else { 0 });
        }
        packet.write_u8(0);
        packet.write_u16(0);
        packet.write_u16(0);
        assert_eq!(
            parse_set_rider_items(&packet.into_inner())
                .unwrap()
                .kart_serial,
            1
        );
    }

    #[test]
    fn plant_part_request_and_replies_match_csharp_goldens() {
        let request = decode_hex("4F08B95A2B000500030079050000");
        let parsed = parse_equip_plant_part(&request).unwrap();
        assert_eq!(
            parsed,
            PlantPartEquipRequest {
                item_category: 43,
                item_id: 5,
                kart_category: 3,
                kart_id: 1_401,
                kart_serial: 1,
                replaced_part: None,
            }
        );
        assert_eq!(
            serialize_equip_tuning_success(parsed),
            decode_hex("9307E74A010100010079052B000500")
        );
        assert_eq!(
            serialize_equip_tuning_failure(),
            decode_hex("9307E74A0000000000000000000000")
        );
    }

    #[test]
    fn stock_plant_part_writer_displaced_descriptor_is_typed_and_bounded() {
        let request = decode_hex("4F08B95A2B0017000300BB03010000000000000000000000");
        assert_eq!(
            parse_equip_plant_part(&request).unwrap(),
            PlantPartEquipRequest {
                item_category: 43,
                item_id: 23,
                kart_category: 3,
                kart_id: 955,
                kart_serial: 1,
                replaced_part: None,
            }
        );

        let replacement = decode_hex("4F08B95A2B0017000300BB0301002B0005000300BB030100");
        assert_eq!(
            parse_equip_plant_part(&replacement).unwrap().replaced_part,
            Some(PlantPartDescriptor {
                item_category: 43,
                item_id: 5,
                kart_category: 3,
                kart_id: 955,
                kart_serial: 1,
            })
        );

        let mut unsupported = request;
        unsupported.push(0);
        assert!(matches!(
            parse_equip_plant_part(&unsupported),
            Err(EquipmentProtocolError::TrailingBytes {
                name: EQUIP_PLANT_PART_REQUEST_NAME,
                count: 11,
            })
        ));
    }

    #[test]
    fn captured_x_part_request_and_success_reply_match_the_csharp_echo() {
        let request = decode_hex("B6065A3B790501003F000200FF7F000001019C040000");
        let parsed = parse_equip_x_part(&request).unwrap();
        assert_eq!(
            parsed,
            XPartEquipRequest {
                kart_id: 1_401,
                kart_serial: 1,
                item_category: 63,
                item_id: 2,
                quantity: i16::MAX,
                unknown_1: 0,
                grade: 1,
                unknown_2: 1,
                parts_value: 1_180,
                unknown_3: 0,
            }
        );
        assert_eq!(
            serialize_equip_x_part_success(parsed),
            decode_hex("B7066A3B00000000790501003F000200FF7F000001019C040000")
        );
        let mut trailing = request;
        trailing.push(0);
        assert!(matches!(
            parse_equip_x_part(&trailing),
            Err(EquipmentProtocolError::TrailingBytes {
                name: EQUIP_X_PART_REQUEST_NAME,
                count: 1,
            })
        ));
    }

    #[test]
    fn inferred_x_part_failure_preserves_the_reply_shape_and_exact_echo() {
        let parsed =
            parse_equip_x_part(&decode_hex("B6065A3B790501003F000200FF7F000001019C040000"))
                .unwrap();
        assert_eq!(
            serialize_equip_x_part_failure(parsed),
            decode_hex("B7066A3B01000000790501003F000200FF7F000001019C040000")
        );
    }

    #[test]
    fn x_part_success_echo_preserves_a_zero_wire_serial() {
        let request = XPartEquipRequest {
            kart_id: 1_401,
            kart_serial: 0,
            item_category: 63,
            item_id: 2,
            quantity: i16::MAX,
            unknown_1: 0,
            grade: 1,
            unknown_2: 1,
            parts_value: 1_180,
            unknown_3: 0,
        };
        let mut packet = PacketWriter::named(EQUIP_X_PART_REQUEST_NAME);
        packet.write_i16(request.kart_id);
        packet.write_i16(request.kart_serial);
        packet.write_i16(request.item_category);
        packet.write_i16(request.item_id);
        packet.write_i16(request.quantity);
        packet.write_i16(request.unknown_1);
        packet.write_u8(request.grade);
        packet.write_u8(request.unknown_2);
        packet.write_i16(request.parts_value);
        packet.write_i16(request.unknown_3);

        let parsed = parse_equip_x_part(packet.as_slice()).unwrap();
        assert_eq!(parsed.kart_serial, 0);
        let reply = serialize_equip_x_part_success(parsed);
        assert_eq!(i16::from_le_bytes(reply[10..12].try_into().unwrap()), 0);
    }

    #[test]
    fn equipment_parsers_match_csharp_rider_length_and_reject_invalid_plant_fields() {
        assert!(parse_set_rider_items(&[0; 4]).is_err());

        let mut trailing = PacketWriter::named(SET_RIDER_ITEMS_REQUEST_NAME);
        trailing.write_bytes(&[0; 66]);
        assert_eq!(
            parse_set_rider_items(&trailing.into_inner()).unwrap(),
            RiderItemSelection {
                character: 0,
                paint: 0,
                kart: 0,
                plate: 0,
                goggle: 0,
                balloon: 0,
                unknown1: 0,
                head_band: 0,
                head_phone: 0,
                hand_gear_left: 0,
                unknown2: 0,
                uniform: 0,
                decal: 0,
                pet: 0,
                flying_pet: 0,
                aura: 0,
                skid_mark: 0,
                special_kit: 0,
                rider_color: 0,
                bonus_card: 0,
                boss_mode_card: 0,
                kart_plant1: 0,
                kart_plant2: 0,
                kart_plant3: 0,
                kart_plant4: 0,
                unknown3: 0,
                fishing_pole: 0,
                tachometer: 0,
                dye: 0,
                kart_serial: 0,
                unknown4: 0,
                kart_coating: 0,
                kart_tail_lamp: 0,
            }
        );

        let invalid = decode_hex("4F08B95A2A000500030079050100");
        assert!(matches!(
            parse_equip_plant_part(&invalid),
            Err(EquipmentProtocolError::InvalidPlantPartCategory(42))
        ));
    }

    #[test]
    fn room_equipment_broadcast_matches_the_csharp_layout() {
        let mut snapshot = [0_u8; 65];
        for (value, expected) in snapshot.iter_mut().zip(0_u8..65) {
            *value = expected;
        }
        let packet = serialize_room_slot_items(2, &snapshot).unwrap();
        assert_eq!(&packet[..8], &decode_hex("FF06834102000000"));
        assert_eq!(&packet[8..], &snapshot);
        assert_eq!(packet.len(), 73);
        assert!(matches!(
            serialize_room_slot_items(16, &snapshot),
            Err(EquipmentProtocolError::InvalidRoomPlayerId(16))
        ));
    }

    fn decode_hex(input: &str) -> Vec<u8> {
        input
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }
}
