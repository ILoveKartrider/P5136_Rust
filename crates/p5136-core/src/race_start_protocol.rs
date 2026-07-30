//! P5136 room-to-race start packet serialization.
//!
//! Kart physics calculation is intentionally outside this codec. The caller
//! supplies one already encoded, exactly sized P5136 physics block.

use thiserror::Error;

use crate::{
    frame::DEFAULT_MAX_PAYLOAD,
    packet::PacketWriter,
    room_protocol::{
        ROOM_SLOT_COUNT, RoomMember, RoomProtocolError, RoomSessionData, RoomSlotData,
        serialize_gr_session_data, serialize_gr_slot_data,
    },
};

pub const GR_COMMAND_START_PACKET_NAME: &str = "GrCommandStartPacket";
pub const MISSION_INFO_NAME: &str = "MissionInfo";
pub const MISSION_INFO_HASH: u32 = 0x1A54_046E;
pub const P5136_KART_PHYSICS_BLOCK_LENGTH: usize = 235;
pub const AI_RACE_SPEC_VALUE_COUNT: usize = 6;
pub const MAX_GR_COMMAND_START_AI_COUNT: usize = ROOM_SLOT_COUNT;
pub const MAX_GR_COMMAND_START_PAYLOAD_LENGTH: usize = DEFAULT_MAX_PAYLOAD;

const AI_RACE_SPEC_WIRE_LENGTH: usize = AI_RACE_SPEC_VALUE_COUNT * size_of::<f32>();
const FIXED_WIRE_LENGTH_EXCLUDING_NESTED_PACKETS_AND_AI: usize = size_of::<u32>() // GrCommandStartPacket hash
    + size_of::<i32>() // reserved before kart physics
    + P5136_KART_PHYSICS_BLOCK_LENGTH
    + size_of::<i32>() // AI count
    + size_of::<u32>() // concrete track
    + size_of::<i32>() // race time limit
    + size_of::<i32>() // reserved before MissionInfo
    + size_of::<u32>() // MissionInfo hash
    + MISSION_INFO_TAIL.len();

/// Generous per-field ceilings over the values emitted by the C# AI generator.
///
/// Fields zero and three are small control factors. The other four are
/// force/time-like values whose original maxima are at most 3,500.
pub const AI_RACE_SPEC_MAX_VALUES: [f32; AI_RACE_SPEC_VALUE_COUNT] =
    [10.0, 10_000.0, 10_000.0, 10.0, 10_000.0, 10_000.0];

pub const MISSION_INFO_TAIL: [u8; 23] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

/// An opaque P5136 on-wire kart physics block.
///
/// A later physics builder can return `[u8; 235]` and convert it directly into
/// this type. Byte slices are accepted through `TryFrom` only after exact
/// length validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct P5136KartPhysicsBlock([u8; P5136_KART_PHYSICS_BLOCK_LENGTH]);

impl P5136KartPhysicsBlock {
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; P5136_KART_PHYSICS_BLOCK_LENGTH] {
        &self.0
    }
}

impl From<[u8; P5136_KART_PHYSICS_BLOCK_LENGTH]> for P5136KartPhysicsBlock {
    fn from(value: [u8; P5136_KART_PHYSICS_BLOCK_LENGTH]) -> Self {
        Self(value)
    }
}

impl TryFrom<&[u8]> for P5136KartPhysicsBlock {
    type Error = RaceStartProtocolError;

    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        if value.len() != P5136_KART_PHYSICS_BLOCK_LENGTH {
            return Err(RaceStartProtocolError::InvalidKartPhysicsBlockLength {
                actual: value.len(),
                expected: P5136_KART_PHYSICS_BLOCK_LENGTH,
            });
        }

        let mut bytes = [0; P5136_KART_PHYSICS_BLOCK_LENGTH];
        bytes.copy_from_slice(value);
        Ok(Self(bytes))
    }
}

/// Six validated plaintext values that are encoded while writing the packet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AiRaceSpec([f32; AI_RACE_SPEC_VALUE_COUNT]);

impl AiRaceSpec {
    pub fn new(values: [f32; AI_RACE_SPEC_VALUE_COUNT]) -> Result<Self, RaceStartProtocolError> {
        for (field, (&value, &maximum)) in values
            .iter()
            .zip(AI_RACE_SPEC_MAX_VALUES.iter())
            .enumerate()
        {
            if !value.is_finite() {
                return Err(RaceStartProtocolError::NonFiniteAiSpecValue { field, value });
            }
            if !(0.0..=maximum).contains(&value) {
                return Err(RaceStartProtocolError::AiSpecValueOutOfBounds {
                    field,
                    value,
                    minimum: 0.0,
                    maximum,
                });
            }
        }
        Ok(Self(values))
    }

    #[must_use]
    pub const fn values(&self) -> &[f32; AI_RACE_SPEC_VALUE_COUNT] {
        &self.0
    }
}

impl TryFrom<[f32; AI_RACE_SPEC_VALUE_COUNT]> for AiRaceSpec {
    type Error = RaceStartProtocolError;

    fn try_from(value: [f32; AI_RACE_SPEC_VALUE_COUNT]) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct GrCommandStart<'a> {
    pub session_data: &'a RoomSessionData,
    pub slot_data: &'a RoomSlotData,
    pub kart_physics: &'a P5136KartPhysicsBlock,
    pub ai_specs: &'a [AiRaceSpec],
    pub concrete_track: u32,
}

#[derive(Debug, Error)]
pub enum RaceStartProtocolError {
    #[error(transparent)]
    Room(#[from] RoomProtocolError),

    #[error("P5136 kart physics block has {actual} bytes; expected exactly {expected}")]
    InvalidKartPhysicsBlockLength { actual: usize, expected: usize },

    #[error("AI count {actual} exceeds the P5136 room limit of {maximum}")]
    TooManyAi { actual: usize, maximum: usize },

    #[error("GrCommandStartPacket has {slot_count} AI room records but {spec_count} AI race specs")]
    AiCountMismatch {
        slot_count: usize,
        spec_count: usize,
    },

    #[error("AI race-spec field {field} is not finite: {value}")]
    NonFiniteAiSpecValue { field: usize, value: f32 },

    #[error("AI race-spec field {field} value {value} is outside {minimum}..={maximum}")]
    AiSpecValueOutOfBounds {
        field: usize,
        value: f32,
        minimum: f32,
        maximum: f32,
    },

    #[error("GrCommandStartPacket length arithmetic overflowed")]
    PayloadLengthOverflow,

    #[error("GrCommandStartPacket payload has {actual} bytes; maximum is {maximum}")]
    PayloadTooLarge { actual: usize, maximum: usize },
}

/// Serializes one personalized P5136 race-start command under the normal frame
/// payload ceiling.
pub fn serialize_gr_command_start(
    command: &GrCommandStart<'_>,
) -> Result<Vec<u8>, RaceStartProtocolError> {
    serialize_gr_command_start_bounded(command, MAX_GR_COMMAND_START_PAYLOAD_LENGTH)
}

/// Serializes one personalized P5136 race-start command under an explicit
/// logical-payload ceiling.
pub fn serialize_gr_command_start_bounded(
    command: &GrCommandStart<'_>,
    maximum_payload_length: usize,
) -> Result<Vec<u8>, RaceStartProtocolError> {
    if command.ai_specs.len() > MAX_GR_COMMAND_START_AI_COUNT {
        return Err(RaceStartProtocolError::TooManyAi {
            actual: command.ai_specs.len(),
            maximum: MAX_GR_COMMAND_START_AI_COUNT,
        });
    }
    let slot_ai_count = command
        .slot_data
        .members_by_id
        .iter()
        .filter(|member| matches!(member, RoomMember::Ai(_)))
        .count();
    if slot_ai_count != command.ai_specs.len() {
        return Err(RaceStartProtocolError::AiCountMismatch {
            slot_count: slot_ai_count,
            spec_count: command.ai_specs.len(),
        });
    }

    let session_packet = serialize_gr_session_data(command.session_data)?;
    let slot_packet = serialize_race_start_slot_data(command.slot_data)?;
    let ai_wire_length = command
        .ai_specs
        .len()
        .checked_mul(AI_RACE_SPEC_WIRE_LENGTH)
        .ok_or(RaceStartProtocolError::PayloadLengthOverflow)?;
    let payload_length = FIXED_WIRE_LENGTH_EXCLUDING_NESTED_PACKETS_AND_AI
        .checked_add(session_packet.len())
        .and_then(|length| length.checked_add(slot_packet.len()))
        .and_then(|length| length.checked_add(ai_wire_length))
        .ok_or(RaceStartProtocolError::PayloadLengthOverflow)?;
    if payload_length > maximum_payload_length {
        return Err(RaceStartProtocolError::PayloadTooLarge {
            actual: payload_length,
            maximum: maximum_payload_length,
        });
    }

    let mut packet = PacketWriter::named(GR_COMMAND_START_PACKET_NAME);
    packet.write_bytes(&session_packet);
    packet.write_bytes(&slot_packet);
    packet.write_i32(0);
    packet.write_bytes(command.kart_physics.as_bytes());
    packet.write_i32(
        i32::try_from(command.ai_specs.len())
            .expect("the validated P5136 AI count always fits in i32"),
    );
    for spec in command.ai_specs {
        for value in spec.values() {
            packet.write_encoded_f32(*value);
        }
    }
    packet.write_u32(command.concrete_track);
    packet.write_i32(10_000);
    packet.write_i32(0);
    packet.write_u32(MISSION_INFO_HASH);
    packet.write_bytes(&MISSION_INFO_TAIL);

    debug_assert_eq!(packet.as_slice().len(), payload_length);
    Ok(packet.into_inner())
}

fn serialize_race_start_slot_data(slots: &RoomSlotData) -> Result<Vec<u8>, RaceStartProtocolError> {
    let mut start_slots = slots.clone();
    if let Ok(master_id) = usize::try_from(start_slots.room_master)
        && let Some(RoomMember::Player(master)) = start_slots.members_by_id.get_mut(master_id)
        && master.player_type == 2
    {
        master.player_type = 3;
    }
    Ok(serialize_gr_slot_data(&start_slots)?)
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::{
        AI_RACE_SPEC_MAX_VALUES, AiRaceSpec, GR_COMMAND_START_PACKET_NAME, GrCommandStart,
        MAX_GR_COMMAND_START_AI_COUNT, MISSION_INFO_HASH, MISSION_INFO_NAME, MISSION_INFO_TAIL,
        P5136_KART_PHYSICS_BLOCK_LENGTH, P5136KartPhysicsBlock, RaceStartProtocolError,
        serialize_gr_command_start, serialize_gr_command_start_bounded,
    };
    use crate::{
        adler32,
        packet::PacketReader,
        room_protocol::{
            RoomAi, RoomMember, RoomPlayer, RoomSessionData, RoomSlotData,
            serialize_gr_session_data, serialize_gr_slot_data,
        },
    };
    use std::net::Ipv4Addr;

    #[test]
    fn personalized_command_start_matches_the_synthetic_wire_golden() {
        let session = RoomSessionData {
            room_name: "Arena".into(),
            password: "pw".into(),
            game_type: 3,
            speed_type: 7,
        };
        let mut slots = RoomSlotData::empty(0x0102_0304, 0x1112_1314, [0xA5; 32], 0);
        slots.members_by_id[0] = RoomMember::Ai(RoomAi {
            character: 1,
            rider: 2,
            kart: 3,
            balloon: 4,
            head_band: 5,
            goggle: 6,
            team: 1,
        });
        slots.members_by_id[1] = RoomMember::Ai(RoomAi {
            character: 7,
            rider: 8,
            kart: 9,
            balloon: 10,
            head_band: 11,
            goggle: 12,
            team: 2,
        });
        let kart_bytes =
            std::array::from_fn(|index| u8::try_from(index).expect("235 is below u8::MAX"));
        let kart = P5136KartPhysicsBlock::from(kart_bytes);
        let ai_specs = [
            AiRaceSpec::try_from([0.7, 2_400.0, 2_950.0, 1.5, 1_000.0, 1_500.0]).unwrap(),
            AiRaceSpec::try_from([1.0, 2_900.0, 3_400.0, 2.0, 1_000.0, 1_500.0]).unwrap(),
        ];
        let command = GrCommandStart {
            session_data: &session,
            slot_data: &slots,
            kart_physics: &kart,
            ai_specs: &ai_specs,
            concrete_track: 0x89AB_CDEF,
        };

        let packet = serialize_gr_command_start(&command).unwrap();
        let session_packet = serialize_gr_session_data(&session).unwrap();
        let slot_packet = serialize_gr_slot_data(&slots).unwrap();
        assert_eq!(packet.len(), 578);
        assert_eq!(
            adler32::packet_hash(GR_COMMAND_START_PACKET_NAME),
            0x50EC_07DE
        );

        let mut offset = 0;
        assert_eq!(
            &packet[offset..offset + 4],
            &adler32::packet_hash(GR_COMMAND_START_PACKET_NAME).to_le_bytes()
        );
        offset += 4;
        assert_eq!(
            &packet[offset..offset + session_packet.len()],
            session_packet
        );
        offset += session_packet.len();
        assert_eq!(&packet[offset..offset + slot_packet.len()], slot_packet);
        offset += slot_packet.len();

        let mut suffix = PacketReader::new(&packet[offset..]);
        assert_eq!(suffix.read_i32().unwrap(), 0);
        assert_eq!(
            suffix.read_bytes(P5136_KART_PHYSICS_BLOCK_LENGTH).unwrap(),
            kart.as_bytes()
        );
        assert_eq!(suffix.read_i32().unwrap(), 2);
        for expected in &ai_specs {
            for value in expected.values() {
                assert_eq!(
                    suffix.read_encoded_f32().unwrap().to_bits(),
                    value.to_bits()
                );
            }
        }
        assert_eq!(suffix.read_u32().unwrap(), 0x89AB_CDEF);
        assert_eq!(suffix.read_i32().unwrap(), 10_000);
        assert_eq!(suffix.read_i32().unwrap(), 0);
        assert_eq!(suffix.read_u32().unwrap(), MISSION_INFO_HASH);
        assert_eq!(
            suffix.read_bytes(MISSION_INFO_TAIL.len()).unwrap(),
            MISSION_INFO_TAIL
        );
        assert!(suffix.remaining().is_empty());
        assert_eq!(MISSION_INFO_HASH, adler32::packet_hash(MISSION_INFO_NAME));

        let digest: [u8; 32] = Sha256::digest(&packet).into();
        assert_eq!(
            digest,
            [
                0x2E, 0xF6, 0x05, 0xC7, 0x3C, 0x69, 0x14, 0xA9, 0x38, 0x3D, 0x5B, 0xAF, 0x8F, 0x32,
                0x82, 0x98, 0xEE, 0xFC, 0xC7, 0x6A, 0xD4, 0xEB, 0x93, 0x49, 0x52, 0xB3, 0xE6, 0xD5,
                0x3D, 0xC3, 0x2C, 0xA1,
            ]
        );
    }

    #[test]
    fn race_start_promotes_the_room_master_from_type_two_to_three() {
        let session = RoomSessionData {
            room_name: "master".into(),
            password: String::new(),
            game_type: 1,
            speed_type: 7,
        };
        let mut slots = RoomSlotData::empty(1, 0, [0; 32], 0);
        slots.members_by_id[0] = RoomMember::Player(RoomPlayer {
            player_type: 2,
            user_no: 17,
            p2p_address: Ipv4Addr::LOCALHOST,
            p2p_port: 39_312,
            nickname: "Master".into(),
            emblem_1: 0,
            emblem_2: 0,
            emblem_3: 0,
            rider_item_snapshot: [0; 65],
            card: String::new(),
            rp: 20_000_000,
            team: 0,
            ranking: 0,
            rider_school_level: 0,
            club_name: String::new(),
            club_mark_logo: 0,
        });
        let kart = P5136KartPhysicsBlock::from([0; P5136_KART_PHYSICS_BLOCK_LENGTH]);
        let packet = serialize_gr_command_start(&GrCommandStart {
            session_data: &session,
            slot_data: &slots,
            kart_physics: &kart,
            ai_specs: &[],
            concrete_track: 1,
        })
        .unwrap();
        let session_length = serialize_gr_session_data(&session).unwrap().len();
        let first_member_type_offset = 4 + session_length + 79;
        assert_eq!(
            &packet[first_member_type_offset..first_member_type_offset + 4],
            &3_i32.to_le_bytes()
        );
        assert_eq!(
            &serialize_gr_slot_data(&slots).unwrap()[79..83],
            &2_i32.to_le_bytes()
        );
    }

    #[test]
    fn kart_physics_block_rejects_every_non_p5136_length() {
        let short = [0; P5136_KART_PHYSICS_BLOCK_LENGTH - 1];
        assert!(matches!(
            P5136KartPhysicsBlock::try_from(short.as_slice()),
            Err(RaceStartProtocolError::InvalidKartPhysicsBlockLength {
                actual: 234,
                expected: P5136_KART_PHYSICS_BLOCK_LENGTH,
            })
        ));

        let long = [0; P5136_KART_PHYSICS_BLOCK_LENGTH + 1];
        assert!(matches!(
            P5136KartPhysicsBlock::try_from(long.as_slice()),
            Err(RaceStartProtocolError::InvalidKartPhysicsBlockLength {
                actual: 236,
                expected: P5136_KART_PHYSICS_BLOCK_LENGTH,
            })
        ));
    }

    #[test]
    fn ai_specs_reject_non_finite_negative_and_excessive_values() {
        let mut values = [1.0; 6];
        values[2] = f32::NAN;
        assert!(matches!(
            AiRaceSpec::try_from(values),
            Err(RaceStartProtocolError::NonFiniteAiSpecValue { field: 2, .. })
        ));

        values[2] = -0.01;
        assert!(matches!(
            AiRaceSpec::try_from(values),
            Err(RaceStartProtocolError::AiSpecValueOutOfBounds {
                field: 2,
                minimum: 0.0,
                ..
            })
        ));

        values[2] = AI_RACE_SPEC_MAX_VALUES[2] + 1.0;
        assert!(matches!(
            AiRaceSpec::try_from(values),
            Err(RaceStartProtocolError::AiSpecValueOutOfBounds { field: 2, .. })
        ));
    }

    #[test]
    fn command_start_rejects_excess_ai_and_enforces_the_payload_cap() {
        let session = RoomSessionData {
            room_name: "cap".into(),
            password: String::new(),
            game_type: 1,
            speed_type: 7,
        };
        let mut slots = RoomSlotData::empty(1, 0, [0; 32], 0);
        for (index, member) in slots.members_by_id.iter_mut().enumerate() {
            *member = RoomMember::Ai(RoomAi {
                character: i16::try_from(index).unwrap(),
                rider: 0,
                kart: 0,
                balloon: 0,
                head_band: 0,
                goggle: 0,
                team: 1,
            });
        }
        let kart = P5136KartPhysicsBlock::from([0; P5136_KART_PHYSICS_BLOCK_LENGTH]);
        let valid_spec = AiRaceSpec::try_from([1.0; 6]).unwrap();
        let excessive_ai = vec![valid_spec; MAX_GR_COMMAND_START_AI_COUNT + 1];
        let excessive_command = GrCommandStart {
            session_data: &session,
            slot_data: &slots,
            kart_physics: &kart,
            ai_specs: &excessive_ai,
            concrete_track: 1,
        };
        assert!(matches!(
            serialize_gr_command_start(&excessive_command),
            Err(RaceStartProtocolError::TooManyAi {
                actual: 9,
                maximum: MAX_GR_COMMAND_START_AI_COUNT,
            })
        ));

        let ai_specs = [valid_spec];
        let mut one_ai_slots = RoomSlotData::empty(1, 0, [0; 32], 0);
        one_ai_slots.members_by_id[0] = slots.members_by_id[0].clone();
        let command = GrCommandStart {
            slot_data: &one_ai_slots,
            ai_specs: &ai_specs,
            ..excessive_command
        };
        let packet = serialize_gr_command_start(&command).unwrap();
        assert_eq!(
            serialize_gr_command_start_bounded(&command, packet.len()).unwrap(),
            packet
        );
        assert!(matches!(
            serialize_gr_command_start_bounded(&command, packet.len() - 1),
            Err(RaceStartProtocolError::PayloadTooLarge {
                actual,
                maximum,
            }) if actual == packet.len() && maximum == packet.len() - 1
        ));
    }

    #[test]
    fn command_start_rejects_ai_roster_and_spec_count_mismatches() {
        let session = RoomSessionData {
            room_name: "mismatch".into(),
            password: String::new(),
            game_type: 1,
            speed_type: 7,
        };
        let mut slots = RoomSlotData::empty(1, 0, [0; 32], 0);
        slots.members_by_id[0] = RoomMember::Ai(RoomAi {
            character: 1,
            rider: 2,
            kart: 3,
            balloon: 4,
            head_band: 5,
            goggle: 6,
            team: 1,
        });
        let kart = P5136KartPhysicsBlock::from([0; P5136_KART_PHYSICS_BLOCK_LENGTH]);
        assert!(matches!(
            serialize_gr_command_start(&GrCommandStart {
                session_data: &session,
                slot_data: &slots,
                kart_physics: &kart,
                ai_specs: &[],
                concrete_track: 1,
            }),
            Err(RaceStartProtocolError::AiCountMismatch {
                slot_count: 1,
                spec_count: 0,
            })
        ));
    }
}
