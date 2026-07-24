//! P5136 inventory preload packet codecs.
//!
//! The stock client expects `PqGetRider` to enqueue every inventory packet
//! before the final `PrGetRider` snapshot. The observed order is plant
//! exception data, parts exception data, rider-item groups, then the rider
//! snapshot. The later `LoRqGetRiderItemPacket` is deliberately consumed
//! without another reply.
//!
//! Inventory wire records contain no strings. All record, group, and output
//! packet counts are bounded before any output packet is allocated.

use thiserror::Error;

use crate::{
    adler32,
    packet::{PacketError, PacketWriter},
    startup::{PrGetRiderFields, serialize_pr_get_rider},
};

pub const RECORDS_PER_INVENTORY_PACKET: usize = 100;
pub const MAX_INVENTORY_RECORDS: usize = 65_535;
pub const MAX_INVENTORY_ITEM_GROUPS: usize = 4_096;
pub const MAX_INVENTORY_PRELOAD_PACKETS: usize = 8_192;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InventoryRequest {
    GetRider,
    LateGetRiderItem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InventoryRequestDisposition {
    PreloadThenRider,
    ConsumeWithoutReply,
}

impl InventoryRequest {
    #[must_use]
    pub const fn request_name(self) -> &'static str {
        match self {
            Self::GetRider => "PqGetRider",
            Self::LateGetRiderItem => "LoRqGetRiderItemPacket",
        }
    }

    #[must_use]
    pub const fn disposition(self) -> InventoryRequestDisposition {
        match self {
            Self::GetRider => InventoryRequestDisposition::PreloadThenRider,
            Self::LateGetRiderItem => InventoryRequestDisposition::ConsumeWithoutReply,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RiderItemRecord {
    pub category: u16,
    pub id: u16,
    pub serial: u16,
    pub amount: u16,
    pub prevent: u8,
    pub reserved: u8,
    pub expiration_low: i16,
    pub expiration_high: i16,
    pub part_flag: u8,
    pub grade: u8,
    pub value: i16,
}

impl RiderItemRecord {
    #[must_use]
    pub const fn normal(
        category: u16,
        id: u16,
        serial: u16,
        amount: u16,
        prevent_item: bool,
    ) -> Self {
        Self {
            category,
            id,
            serial,
            amount,
            prevent: if prevent_item { 1 } else { 0 },
            reserved: 0,
            expiration_low: -1,
            expiration_high: 0,
            part_flag: 0,
            grade: 0,
            value: 0,
        }
    }

    #[must_use]
    pub const fn part(category: u16, id: u16, amount: u16, grade: u8, value: i16) -> Self {
        Self {
            category,
            id,
            serial: 0,
            amount,
            prevent: 0,
            reserved: 0,
            expiration_low: -1,
            expiration_high: -1,
            part_flag: 1,
            grade,
            value,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlantExcRecord {
    pub id: i16,
    pub serial: i16,
    pub engine_category: i16,
    pub engine_id: i16,
    pub handle_category: i16,
    pub handle_id: i16,
    pub wheel_category: i16,
    pub wheel_id: i16,
    pub kit_category: i16,
    pub kit_id: i16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartsExcRecord {
    pub id: i16,
    pub serial: i16,
    pub engine: i16,
    pub engine_grade: u8,
    pub engine_value: i16,
    pub handle: i16,
    pub handle_grade: u8,
    pub handle_value: i16,
    pub wheel: i16,
    pub wheel_grade: u8,
    pub wheel_value: i16,
    pub booster: i16,
    pub booster_grade: u8,
    pub booster_value: i16,
    pub coating: i16,
    pub tail_lamp: i16,
}

/// A catalog category or one generated X-parts batch. Adjacent groups remain
/// separate when serialized, matching the C# physical packet boundaries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RiderItemGroup {
    pub records: Vec<RiderItemRecord>,
}

impl RiderItemGroup {
    #[must_use]
    pub fn new(records: Vec<RiderItemRecord>) -> Self {
        Self { records }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InventorySnapshot {
    pub plant_exceptions: Vec<PlantExcRecord>,
    pub parts_exceptions: Vec<PartsExcRecord>,
    pub item_groups: Vec<RiderItemGroup>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GetRiderPacketKind {
    PlantExceptions { first_chunk: bool },
    PartsExceptions { first_chunk: bool },
    RiderItems,
    RiderSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetRiderPacket {
    pub kind: GetRiderPacketKind,
    pub logical_packet: Vec<u8>,
}

impl GetRiderPacket {
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.logical_packet
    }
}

#[derive(Debug, Error)]
pub enum InventoryError {
    #[error(transparent)]
    Packet(#[from] PacketError),

    #[error("{stream} cannot be serialized with an empty record chunk")]
    EmptyChunk { stream: &'static str },

    #[error("{resource} count {actual} exceeds configured maximum {maximum}")]
    LimitExceeded {
        resource: &'static str,
        actual: usize,
        maximum: usize,
    },

    #[error("{resource} count overflowed the host address space")]
    CountOverflow { resource: &'static str },
}

#[must_use]
pub fn classify_inventory_request(hash: u32) -> Option<InventoryRequest> {
    [
        InventoryRequest::GetRider,
        InventoryRequest::LateGetRiderItem,
    ]
    .into_iter()
    .find(|request| adler32::packet_hash(request.request_name()) == hash)
}

/// Serializes only the inventory packets that must precede `PrGetRider`.
///
/// Plant and parts exception arrays are chunked globally. Rider items are
/// chunked independently inside each group so category/X-parts boundaries are
/// not merged.
pub fn serialize_inventory_preload(
    snapshot: &InventorySnapshot,
) -> Result<Vec<GetRiderPacket>, InventoryError> {
    validate_snapshot(snapshot)?;

    let packet_count = preload_packet_count(snapshot)?;
    let mut packets = Vec::with_capacity(packet_count);

    for (index, records) in snapshot
        .plant_exceptions
        .chunks(RECORDS_PER_INVENTORY_PACKET)
        .enumerate()
    {
        let first_chunk = index == 0;
        packets.push(GetRiderPacket {
            kind: GetRiderPacketKind::PlantExceptions { first_chunk },
            logical_packet: serialize_plant_exc_packet(records, first_chunk)?,
        });
    }

    for (index, records) in snapshot
        .parts_exceptions
        .chunks(RECORDS_PER_INVENTORY_PACKET)
        .enumerate()
    {
        let first_chunk = index == 0;
        packets.push(GetRiderPacket {
            kind: GetRiderPacketKind::PartsExceptions { first_chunk },
            logical_packet: serialize_parts_exc_packet(records, first_chunk)?,
        });
    }

    for group in &snapshot.item_groups {
        for records in group.records.chunks(RECORDS_PER_INVENTORY_PACKET) {
            packets.push(GetRiderPacket {
                kind: GetRiderPacketKind::RiderItems,
                logical_packet: serialize_rider_item_packet(records)?,
            });
        }
    }

    Ok(packets)
}

/// Serializes the complete response sequence for `PqGetRider`. Returning the
/// final rider snapshot in the same vector makes it harder for a caller to
/// publish `PrGetRider` before its required inventory preload.
pub fn serialize_get_rider_sequence(
    snapshot: &InventorySnapshot,
    rider: &PrGetRiderFields,
) -> Result<Vec<GetRiderPacket>, InventoryError> {
    let mut packets = serialize_inventory_preload(snapshot)?;
    packets.push(GetRiderPacket {
        kind: GetRiderPacketKind::RiderSnapshot,
        logical_packet: serialize_pr_get_rider(rider)?,
    });
    Ok(packets)
}

pub fn serialize_rider_item_packet(records: &[RiderItemRecord]) -> Result<Vec<u8>, InventoryError> {
    let count = validate_packet_chunk("rider-item stream", records.len())?;
    let mut packet = PacketWriter::named("LoRpGetRiderItemPacket");
    packet.write_i32(1);
    packet.write_i32(1);
    packet.write_i32(count);
    for record in records {
        packet.write_u16(record.category);
        packet.write_u16(record.id);
        packet.write_u16(record.serial);
        packet.write_u16(record.amount);
        packet.write_u8(record.prevent);
        packet.write_u8(record.reserved);
        write_i16(&mut packet, record.expiration_low);
        write_i16(&mut packet, record.expiration_high);
        packet.write_u8(record.part_flag);
        packet.write_u8(record.grade);
        write_i16(&mut packet, record.value);
    }
    Ok(packet.into_inner())
}

pub fn serialize_parts_exc_packet(
    records: &[PartsExcRecord],
    first_chunk: bool,
) -> Result<Vec<u8>, InventoryError> {
    let count = validate_packet_chunk("parts exception stream", records.len())?;
    let mut packet = PacketWriter::named("LoRpGetRiderExcDataPacket");
    packet.write_bytes(&[0, 0, 0, u8::from(first_chunk), 0, 0]);
    packet.write_i32(0);
    packet.write_i32(0);
    packet.write_i32(0);
    packet.write_i32(count);

    for record in records {
        write_i16(&mut packet, record.id);
        write_i16(&mut packet, record.serial);
        write_i16(&mut packet, 0);
        write_i16(&mut packet, -1);
        write_i16(&mut packet, 0);
        write_part_slot(
            &mut packet,
            record.engine,
            record.engine_grade,
            record.engine_value,
        );
        write_part_slot(
            &mut packet,
            record.handle,
            record.handle_grade,
            record.handle_value,
        );
        write_part_slot(
            &mut packet,
            record.wheel,
            record.wheel_grade,
            record.wheel_value,
        );
        write_part_slot(
            &mut packet,
            record.booster,
            record.booster_grade,
            record.booster_value,
        );
        write_part_slot(&mut packet, record.coating, 0, 0);
        write_part_slot(&mut packet, record.tail_lamp, 0, 0);
    }

    packet.write_i32(0);
    packet.write_i32(0);
    Ok(packet.into_inner())
}

pub fn serialize_plant_exc_packet(
    records: &[PlantExcRecord],
    first_chunk: bool,
) -> Result<Vec<u8>, InventoryError> {
    let count = validate_packet_chunk("plant exception stream", records.len())?;
    let mut packet = PacketWriter::named("LoRpGetRiderExcDataPacket");
    packet.write_bytes(&[0, u8::from(first_chunk), 0, 0, 0, 0]);
    packet.write_i32(0);
    packet.write_i32(count);

    for record in records {
        write_i16(&mut packet, record.id);
        write_i16(&mut packet, record.serial);
        packet.write_i32(4);
        write_i16(&mut packet, record.engine_category);
        write_i16(&mut packet, record.engine_id);
        write_i16(&mut packet, record.handle_category);
        write_i16(&mut packet, record.handle_id);
        write_i16(&mut packet, record.wheel_category);
        write_i16(&mut packet, record.wheel_id);
        write_i16(&mut packet, record.kit_category);
        write_i16(&mut packet, record.kit_id);
    }

    packet.write_i32(0);
    packet.write_i32(0);
    packet.write_i32(0);
    packet.write_i32(0);
    Ok(packet.into_inner())
}

fn validate_snapshot(snapshot: &InventorySnapshot) -> Result<(), InventoryError> {
    enforce_limit(
        "rider-item group",
        snapshot.item_groups.len(),
        MAX_INVENTORY_ITEM_GROUPS,
    )?;

    let mut record_count = snapshot
        .plant_exceptions
        .len()
        .checked_add(snapshot.parts_exceptions.len())
        .ok_or(InventoryError::CountOverflow {
            resource: "inventory record",
        })?;
    for group in &snapshot.item_groups {
        record_count =
            record_count
                .checked_add(group.records.len())
                .ok_or(InventoryError::CountOverflow {
                    resource: "inventory record",
                })?;
    }
    enforce_limit("inventory record", record_count, MAX_INVENTORY_RECORDS)?;

    let packet_count = preload_packet_count(snapshot)?;
    enforce_limit(
        "inventory preload packet",
        packet_count,
        MAX_INVENTORY_PRELOAD_PACKETS,
    )
}

fn preload_packet_count(snapshot: &InventorySnapshot) -> Result<usize, InventoryError> {
    let mut packet_count = chunk_count(snapshot.plant_exceptions.len())
        .checked_add(chunk_count(snapshot.parts_exceptions.len()))
        .ok_or(InventoryError::CountOverflow {
            resource: "inventory preload packet",
        })?;
    for group in &snapshot.item_groups {
        packet_count = packet_count
            .checked_add(chunk_count(group.records.len()))
            .ok_or(InventoryError::CountOverflow {
                resource: "inventory preload packet",
            })?;
    }
    Ok(packet_count)
}

fn chunk_count(record_count: usize) -> usize {
    record_count.div_ceil(RECORDS_PER_INVENTORY_PACKET)
}

fn validate_packet_chunk(stream: &'static str, count: usize) -> Result<i32, InventoryError> {
    if count == 0 {
        return Err(InventoryError::EmptyChunk { stream });
    }
    enforce_limit(stream, count, RECORDS_PER_INVENTORY_PACKET)?;
    i32::try_from(count).map_err(|_| InventoryError::CountOverflow { resource: stream })
}

fn enforce_limit(
    resource: &'static str,
    actual: usize,
    maximum: usize,
) -> Result<(), InventoryError> {
    if actual > maximum {
        Err(InventoryError::LimitExceeded {
            resource,
            actual,
            maximum,
        })
    } else {
        Ok(())
    }
}

fn write_part_slot(packet: &mut PacketWriter, id: i16, grade: u8, value: i16) {
    write_i16(packet, id);
    packet.write_u8(grade);
    write_i16(packet, value);
}

fn write_i16(packet: &mut PacketWriter, value: i16) {
    packet.write_bytes(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::{
        GetRiderPacketKind, InventoryError, InventoryRequest, InventoryRequestDisposition,
        InventorySnapshot, MAX_INVENTORY_ITEM_GROUPS, MAX_INVENTORY_RECORDS, PartsExcRecord,
        PlantExcRecord, RECORDS_PER_INVENTORY_PACKET, RiderItemGroup, RiderItemRecord,
        classify_inventory_request, serialize_get_rider_sequence, serialize_inventory_preload,
        serialize_parts_exc_packet, serialize_plant_exc_packet, serialize_rider_item_packet,
    };
    use crate::{adler32, startup::PrGetRiderFields};

    #[test]
    fn request_pairing_exposes_the_preload_gate_and_late_noop() {
        assert_eq!(
            classify_inventory_request(343_016_407),
            Some(InventoryRequest::GetRider)
        );
        assert_eq!(
            InventoryRequest::GetRider.disposition(),
            InventoryRequestDisposition::PreloadThenRider
        );
        assert_eq!(
            classify_inventory_request(1_602_488_443),
            Some(InventoryRequest::LateGetRiderItem)
        );
        assert_eq!(
            InventoryRequest::LateGetRiderItem.disposition(),
            InventoryRequestDisposition::ConsumeWithoutReply
        );
        assert_eq!(
            adler32::packet_hash(InventoryRequest::GetRider.request_name()),
            343_016_407
        );
        assert_eq!(classify_inventory_request(0xdead_beef), None);
    }

    #[test]
    fn default_item_records_match_the_csharp_golden() {
        let packet = serialize_rider_item_packet(&[
            RiderItemRecord::normal(3, 1001, 1, 1, false),
            RiderItemRecord::part(63, 1, 77, 3, 1053),
        ])
        .unwrap();

        assert_eq!(packet.len(), 52);
        assert_eq!(
            format!("{:X}", Sha256::digest(&packet)),
            "E12B74D64E369401769681AB58C490BDB31FB2DD048E70A447068A770E550FE2"
        );
        assert_eq!(
            packet,
            decode_hex(
                "7A08715F0100000001000000020000000300E903010001000000FFFF000000000000\
                 3F00010000004D000000FFFFFFFF01031D04"
            )
        );
    }

    #[test]
    fn nonempty_preload_matches_csharp_goldens_and_wire_order() {
        let snapshot = InventorySnapshot {
            plant_exceptions: vec![fixture_plant()],
            parts_exceptions: vec![fixture_parts()],
            item_groups: vec![RiderItemGroup::new(vec![
                RiderItemRecord::normal(3, 1001, 1, 1, false),
                RiderItemRecord::part(63, 1, 77, 3, 1053),
                RiderItemRecord::normal(7, 5, 9, 88, true),
            ])],
        };
        let packets = serialize_inventory_preload(&snapshot).unwrap();

        assert_eq!(
            packets.iter().map(|packet| packet.kind).collect::<Vec<_>>(),
            [
                GetRiderPacketKind::PlantExceptions { first_chunk: true },
                GetRiderPacketKind::PartsExceptions { first_chunk: true },
                GetRiderPacketKind::RiderItems,
            ]
        );
        assert_packet_golden(
            &packets[0].logical_packet,
            58,
            "72053399104CE43F103C2DCCB08F21634A48C779062ECCFC9EE87A762AA77264",
        );
        assert_packet_golden(
            &packets[1].logical_packet,
            74,
            "412DA167F2C0607D3C4370742AFA66D59E3CB88E4C81314A89A30FC5A292D45A",
        );
        assert_packet_golden(
            &packets[2].logical_packet,
            70,
            "20A470BD78D0554A6033746CCEBE1FA06EB742DFC2C26A0E601D4977AA0C210C",
        );
    }

    #[test]
    fn complete_sequence_never_publishes_rider_before_inventory() {
        let snapshot = InventorySnapshot {
            plant_exceptions: vec![fixture_plant()],
            parts_exceptions: vec![fixture_parts()],
            item_groups: vec![RiderItemGroup::new(vec![RiderItemRecord::normal(
                3, 1001, 1, 1, false,
            )])],
        };
        let sequence = serialize_get_rider_sequence(&snapshot, &fixture_rider()).unwrap();

        assert_eq!(sequence.len(), 4);
        assert!(matches!(
            sequence[0].kind,
            GetRiderPacketKind::PlantExceptions { .. }
        ));
        assert!(matches!(
            sequence[1].kind,
            GetRiderPacketKind::PartsExceptions { .. }
        ));
        assert_eq!(sequence[2].kind, GetRiderPacketKind::RiderItems);
        assert_eq!(sequence[3].kind, GetRiderPacketKind::RiderSnapshot);
        assert_eq!(
            u32::from_le_bytes(sequence[3].as_slice()[..4].try_into().unwrap()),
            343_606_232
        );

        let empty =
            serialize_get_rider_sequence(&InventorySnapshot::default(), &fixture_rider()).unwrap();
        assert_eq!(empty.len(), 1);
        assert_eq!(empty[0].kind, GetRiderPacketKind::RiderSnapshot);
    }

    #[test]
    fn chunk_flags_and_item_group_boundaries_match_the_csharp_plan() {
        let repeated_plant = vec![fixture_plant(); RECORDS_PER_INVENTORY_PACKET + 1];
        let repeated_parts = vec![fixture_parts(); RECORDS_PER_INVENTORY_PACKET + 1];
        let item = RiderItemRecord::normal(3, 1001, 1, 1, false);
        let snapshot = InventorySnapshot {
            plant_exceptions: repeated_plant,
            parts_exceptions: repeated_parts,
            item_groups: vec![
                RiderItemGroup::new(vec![item; 60]),
                RiderItemGroup::new(vec![item; 60]),
            ],
        };
        let packets = serialize_inventory_preload(&snapshot).unwrap();

        assert_eq!(packets.len(), 6);
        assert_eq!(
            packets[0].kind,
            GetRiderPacketKind::PlantExceptions { first_chunk: true }
        );
        assert_eq!(
            packets[1].kind,
            GetRiderPacketKind::PlantExceptions { first_chunk: false }
        );
        assert_eq!(
            packets[2].kind,
            GetRiderPacketKind::PartsExceptions { first_chunk: true }
        );
        assert_eq!(
            packets[3].kind,
            GetRiderPacketKind::PartsExceptions { first_chunk: false }
        );
        assert_eq!(packets[4].kind, GetRiderPacketKind::RiderItems);
        assert_eq!(packets[5].kind, GetRiderPacketKind::RiderItems);
        assert_eq!(read_i32(&packets[4].logical_packet, 12), 60);
        assert_eq!(read_i32(&packets[5].logical_packet, 12), 60);
    }

    #[test]
    fn continuation_exception_flags_match_csharp_goldens() {
        let plant = serialize_plant_exc_packet(&[fixture_plant()], false).unwrap();
        assert_packet_golden(
            &plant,
            58,
            "908628F5B29AD5477DCDAAF5D01C7D92F093D87654223CF52D83DE7175514D1B",
        );
        let parts = serialize_parts_exc_packet(&[fixture_parts()], false).unwrap();
        assert_packet_golden(
            &parts,
            74,
            "DEE094439F8C925B46C1FE5F3227F4CA51373282E15B3E3B5196CDC5B15F2ADD",
        );
    }

    #[test]
    fn record_and_group_limits_are_rejected_before_serialization() {
        assert!(matches!(
            serialize_rider_item_packet(&[]),
            Err(InventoryError::EmptyChunk { .. })
        ));
        assert!(matches!(
            serialize_rider_item_packet(
                &[RiderItemRecord::normal(3, 1, 1, 1, false); RECORDS_PER_INVENTORY_PACKET + 1]
            ),
            Err(InventoryError::LimitExceeded {
                actual: 101,
                maximum: RECORDS_PER_INVENTORY_PACKET,
                ..
            })
        ));

        let too_many_records = InventorySnapshot {
            item_groups: vec![RiderItemGroup::new(vec![
                RiderItemRecord::normal(
                    3, 1, 1, 1, false
                );
                MAX_INVENTORY_RECORDS + 1
            ])],
            ..InventorySnapshot::default()
        };
        assert!(matches!(
            serialize_inventory_preload(&too_many_records),
            Err(InventoryError::LimitExceeded {
                actual: 65_536,
                maximum: MAX_INVENTORY_RECORDS,
                ..
            })
        ));

        let too_many_groups = InventorySnapshot {
            item_groups: vec![RiderItemGroup::default(); MAX_INVENTORY_ITEM_GROUPS + 1],
            ..InventorySnapshot::default()
        };
        assert!(matches!(
            serialize_inventory_preload(&too_many_groups),
            Err(InventoryError::LimitExceeded {
                actual: 4_097,
                maximum: MAX_INVENTORY_ITEM_GROUPS,
                ..
            })
        ));
    }

    fn fixture_plant() -> PlantExcRecord {
        PlantExcRecord {
            id: 101,
            serial: 1,
            engine_category: 43,
            engine_id: 11,
            handle_category: 44,
            handle_id: 22,
            wheel_category: 45,
            wheel_id: 33,
            kit_category: 46,
            kit_id: 44,
        }
    }

    fn fixture_parts() -> PartsExcRecord {
        PartsExcRecord {
            id: 1001,
            serial: 2,
            engine: 10,
            engine_grade: 1,
            engine_value: 1053,
            handle: 20,
            handle_grade: 2,
            handle_value: 1005,
            wheel: 30,
            wheel_grade: 3,
            wheel_value: 910,
            booster: 40,
            booster_grade: 4,
            booster_value: 810,
            coating: 50,
            tail_lamp: 60,
        }
    }

    fn fixture_rider() -> PrGetRiderFields {
        PrGetRiderFields {
            nickname: "Rider".to_owned(),
            emblem_1: 0,
            emblem_2: 0,
            rider_item_snapshot: [0; 65],
            lucci: 0,
            rp: 0,
        }
    }

    fn assert_packet_golden(packet: &[u8], expected_length: usize, expected_sha256: &str) {
        assert_eq!(packet.len(), expected_length);
        assert_eq!(format!("{:X}", Sha256::digest(packet)), expected_sha256);
    }

    fn read_i32(packet: &[u8], offset: usize) -> i32 {
        i32::from_le_bytes(packet[offset..offset + 4].try_into().unwrap())
    }

    fn decode_hex(input: &str) -> Vec<u8> {
        let compact = input
            .bytes()
            .filter(|byte| !byte.is_ascii_whitespace())
            .collect::<Vec<_>>();
        assert!(compact.len().is_multiple_of(2));
        compact
            .chunks_exact(2)
            .map(|pair| {
                let text = std::str::from_utf8(pair).unwrap();
                u8::from_str_radix(text, 16).unwrap()
            })
            .collect()
    }
}
