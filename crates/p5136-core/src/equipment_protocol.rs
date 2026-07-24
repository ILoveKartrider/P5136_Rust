//! P5136 rider-equipment update and plant-part equip codecs.

use thiserror::Error;

use crate::{
    adler32,
    packet::{PacketError, PacketReader, PacketWriter},
    startup::RIDER_ITEM_SNAPSHOT_WIRE_LENGTH,
};

pub const SET_RIDER_ITEMS_REQUEST_NAME: &str = "LoRqSetRiderItemOnPacket";
pub const EQUIP_PLANT_PART_REQUEST_NAME: &str = "PqEquipTuningExPacket";
pub const EQUIP_TUNING_REPLY_NAME: &str = "PrEquipTuningPacket";
pub const ROOM_SLOT_ITEMS_PACKET_NAME: &str = "GrSlotItemOnPacket";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EquipmentRequest {
    SetRiderItems,
    EquipPlantPart,
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
    pub kart_plant1: u16,
    pub kart_plant2: u16,
    pub kart_plant3: u16,
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

    #[error("room equipment player ID {0} is outside 0..=15")]
    InvalidRoomPlayerId(i32),
}

#[must_use]
pub fn classify_equipment_request(hash: u32) -> Option<EquipmentRequest> {
    if hash == adler32::packet_hash(SET_RIDER_ITEMS_REQUEST_NAME) {
        Some(EquipmentRequest::SetRiderItems)
    } else if hash == adler32::packet_hash(EQUIP_PLANT_PART_REQUEST_NAME) {
        Some(EquipmentRequest::EquipPlantPart)
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
    ensure_exhausted(&reader, SET_RIDER_ITEMS_REQUEST_NAME)?;
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
    };
    ensure_exhausted(&reader, EQUIP_PLANT_PART_REQUEST_NAME)?;
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

#[must_use]
pub fn serialize_equip_tuning_failure() -> Vec<u8> {
    let mut packet = PacketWriter::named(EQUIP_TUNING_REPLY_NAME);
    packet.write_i32(0);
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
        EQUIP_PLANT_PART_REQUEST_NAME, EquipmentProtocolError, EquipmentRequest,
        PlantPartEquipRequest, SET_RIDER_ITEMS_REQUEST_NAME, classify_equipment_request,
        parse_equip_plant_part, parse_set_rider_items, serialize_equip_tuning_failure,
        serialize_equip_tuning_success, serialize_room_slot_items,
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
            adler32::packet_hash(SET_RIDER_ITEMS_REQUEST_NAME),
            0x7234_0944
        );
        assert_eq!(
            adler32::packet_hash(EQUIP_PLANT_PART_REQUEST_NAME),
            0x5AB9_084F
        );
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
            }
        );
        assert_eq!(
            serialize_equip_tuning_success(parsed),
            decode_hex("9307E74A010100010079052B000500")
        );
        assert_eq!(
            serialize_equip_tuning_failure(),
            decode_hex("9307E74A00000000")
        );
    }

    #[test]
    fn equipment_parsers_reject_truncation_trailing_bytes_and_invalid_fields() {
        assert!(parse_set_rider_items(&[0; 4]).is_err());

        let mut trailing = PacketWriter::named(SET_RIDER_ITEMS_REQUEST_NAME);
        trailing.write_bytes(&[0; 66]);
        assert!(matches!(
            parse_set_rider_items(&trailing.into_inner()),
            Err(EquipmentProtocolError::TrailingBytes { count: 1, .. })
        ));

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
