//! Strict codecs for the captured P5136 single-player and time-attack flow.
//!
//! The reference C# handlers read only the fields they happen to use and leave
//! several producer bytes unread. The Rust codec retains those fixed-size
//! producer fields but still requires the complete observed packet shape.

use std::array::TryFromSliceError;

use thiserror::Error;

use crate::{
    adler32,
    packet::{PacketError, PacketReader, PacketWriter},
    race_start_protocol::P5136KartPhysicsBlock,
};

pub const START_SINGLE_REQUEST_NAME: &str = "LoRqStartSinglePacket";
pub const USE_SINGLE_ITEM_REQUEST_NAME: &str = "LoRqUseItemPacket";
pub const KART_SPEC_REQUEST_NAME: &str = "PqKartSpec";
pub const KART_SPEC_REPLY_NAME: &str = "PrKartSpec";
pub const START_TIME_ATTACK_REQUEST_NAME: &str = "PqStartTimeAttack";
pub const START_TIME_ATTACK_REPLY_NAME: &str = "PrStartTimeAttack";
pub const FINISH_TIME_ATTACK_REQUEST_NAME: &str = "PqFinishTimeAttack";
pub const FINISH_TIME_ATTACK_REPLY_NAME: &str = "PrFinishTimeAttack";

pub const START_SINGLE_PACKET_LENGTH: usize = 41;
pub const USE_SINGLE_ITEM_PACKET_LENGTH: usize = 10;
pub const KART_SPEC_REQUEST_LENGTH: usize = 15;
pub const KART_SPEC_REPLY_LENGTH: usize = 241;
pub const START_TIME_ATTACK_REQUEST_LENGTH: usize = 39;
pub const START_TIME_ATTACK_REPLY_LENGTH: usize = 268;
pub const FINISH_TIME_ATTACK_REQUEST_LENGTH: usize = 33;
pub const FINISH_TIME_ATTACK_REPLY_LENGTH: usize = 37;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinglePlayerRequestKind {
    StartSingle,
    UseItem,
    KartSpec,
    StartTimeAttack,
    FinishTimeAttack,
}

impl SinglePlayerRequestKind {
    #[must_use]
    pub const fn request_name(self) -> &'static str {
        match self {
            Self::StartSingle => START_SINGLE_REQUEST_NAME,
            Self::UseItem => USE_SINGLE_ITEM_REQUEST_NAME,
            Self::KartSpec => KART_SPEC_REQUEST_NAME,
            Self::StartTimeAttack => START_TIME_ATTACK_REQUEST_NAME,
            Self::FinishTimeAttack => FINISH_TIME_ATTACK_REQUEST_NAME,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartSingleRequest {
    pub start_ticks: i32,
    pub producer_proof: [u8; 33],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UseSingleItemRequest {
    pub slot_item_category: u16,
    pub slot_item_id: u16,
    pub remaining_quantity: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KartSpecRequest {
    pub speed_type: u8,
    pub kart_id: u16,
    pub flying_pet_id: u16,
    pub producer_context: [u8; 6],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartTimeAttackRequest {
    pub start_token: i32,
    pub unknown_2: i32,
    pub requested_track: u32,
    pub speed_type: u8,
    pub game_type: u8,
    pub kart_id: u16,
    pub flying_pet_id: u16,
    pub start_type: u8,
    pub unknown_3: i32,
    pub unknown_4: i32,
    pub unknown_5: u8,
    pub attack_type: u8,
    pub mode_type: u8,
    pub mode: i32,
    pub random_track_game_type: u8,
}

impl StartTimeAttackRequest {
    #[must_use]
    pub const fn entry_fee(self) -> u32 {
        if self.mode_type == 1 { 1_000 } else { 0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinishTimeAttackRequest {
    pub result_type: i32,
    pub unknown_1: i32,
    pub reward_type: u8,
    pub unknown_2: i32,
    pub unknown_3: i32,
    pub booster_count: i32,
    pub crash_count: i32,
    pub race_time: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinglePlayerRequest {
    StartSingle(StartSingleRequest),
    UseItem(UseSingleItemRequest),
    KartSpec(KartSpecRequest),
    StartTimeAttack(StartTimeAttackRequest),
    FinishTimeAttack(FinishTimeAttackRequest),
}

#[derive(Debug, Error)]
pub enum SinglePlayerProtocolError {
    #[error(transparent)]
    Packet(#[from] PacketError),

    #[error("unsupported single-player packet hash 0x{hash:08X}")]
    UnsupportedPacketHash { hash: u32 },

    #[error(
        "{packet} has logical length {actual}; expected the captured producer length {expected}"
    )]
    InvalidLength {
        packet: &'static str,
        actual: usize,
        expected: usize,
    },

    #[error("{packet} contained packet hash 0x{actual:08X}; expected 0x{expected:08X}")]
    PacketHashMismatch {
        packet: &'static str,
        actual: u32,
        expected: u32,
    },

    #[error("PqFinishTimeAttack reward type {0} is outside the captured 0..=1 domain")]
    UnsupportedRewardType(u8),

    #[error("fixed-size single-player producer field could not be represented")]
    FixedField(#[from] TryFromSliceError),
}

#[must_use]
pub fn classify_single_player_request(hash: u32) -> Option<SinglePlayerRequestKind> {
    [
        SinglePlayerRequestKind::StartSingle,
        SinglePlayerRequestKind::UseItem,
        SinglePlayerRequestKind::KartSpec,
        SinglePlayerRequestKind::StartTimeAttack,
        SinglePlayerRequestKind::FinishTimeAttack,
    ]
    .into_iter()
    .find(|kind| adler32::packet_hash(kind.request_name()) == hash)
}

pub fn parse_single_player_request(
    kind: SinglePlayerRequestKind,
    packet: &[u8],
) -> Result<SinglePlayerRequest, SinglePlayerProtocolError> {
    require_length(kind, packet)?;
    let mut reader = PacketReader::new(packet);
    require_hash(&mut reader, kind)?;
    let request = match kind {
        SinglePlayerRequestKind::StartSingle => {
            SinglePlayerRequest::StartSingle(StartSingleRequest {
                start_ticks: reader.read_i32()?,
                producer_proof: reader.read_bytes(33)?.try_into()?,
            })
        }
        SinglePlayerRequestKind::UseItem => SinglePlayerRequest::UseItem(UseSingleItemRequest {
            slot_item_category: reader.read_u16()?,
            slot_item_id: reader.read_u16()?,
            remaining_quantity: reader.read_u16()?,
        }),
        SinglePlayerRequestKind::KartSpec => SinglePlayerRequest::KartSpec(KartSpecRequest {
            speed_type: reader.read_u8()?,
            kart_id: reader.read_u16()?,
            flying_pet_id: reader.read_u16()?,
            producer_context: reader.read_bytes(6)?.try_into()?,
        }),
        SinglePlayerRequestKind::StartTimeAttack => {
            SinglePlayerRequest::StartTimeAttack(StartTimeAttackRequest {
                start_token: reader.read_i32()?,
                unknown_2: reader.read_i32()?,
                requested_track: reader.read_u32()?,
                speed_type: reader.read_u8()?,
                game_type: reader.read_u8()?,
                kart_id: reader.read_u16()?,
                flying_pet_id: reader.read_u16()?,
                start_type: reader.read_u8()?,
                unknown_3: reader.read_i32()?,
                unknown_4: reader.read_i32()?,
                unknown_5: reader.read_u8()?,
                attack_type: reader.read_u8()?,
                mode_type: reader.read_u8()?,
                mode: reader.read_i32()?,
                random_track_game_type: reader.read_u8()?,
            })
        }
        SinglePlayerRequestKind::FinishTimeAttack => {
            let request = FinishTimeAttackRequest {
                result_type: reader.read_i32()?,
                unknown_1: reader.read_i32()?,
                reward_type: reader.read_u8()?,
                unknown_2: reader.read_i32()?,
                unknown_3: reader.read_i32()?,
                booster_count: reader.read_i32()?,
                crash_count: reader.read_i32()?,
                race_time: reader.read_u32()?,
            };
            if request.reward_type > 1 {
                return Err(SinglePlayerProtocolError::UnsupportedRewardType(
                    request.reward_type,
                ));
            }
            SinglePlayerRequest::FinishTimeAttack(request)
        }
    };
    debug_assert!(reader.remaining().is_empty());
    Ok(request)
}

#[must_use]
pub fn serialize_kart_spec_reply(physics: &P5136KartPhysicsBlock) -> Vec<u8> {
    let mut packet = PacketWriter::named(KART_SPEC_REPLY_NAME);
    packet.write_u8(1);
    packet.write_bytes(physics.as_bytes());
    packet.write_u8(0);
    let packet = packet.into_inner();
    debug_assert_eq!(packet.len(), KART_SPEC_REPLY_LENGTH);
    packet
}

#[must_use]
pub fn serialize_start_time_attack_reply(
    start_token: i32,
    physics: &P5136KartPhysicsBlock,
    lucci: u32,
    koin: u32,
    track: u32,
) -> Vec<u8> {
    let mut packet = PacketWriter::named(START_TIME_ATTACK_REPLY_NAME);
    packet.write_i32(start_token);
    packet.write_i32(0);
    packet.write_bytes(physics.as_bytes());
    packet.write_u8(0);
    packet.write_i32(0);
    packet.write_i32(0);
    packet.write_u32(lucci);
    packet.write_u32(koin);
    packet.write_u32(track);
    let packet = packet.into_inner();
    debug_assert_eq!(packet.len(), START_TIME_ATTACK_REPLY_LENGTH);
    packet
}

#[must_use]
pub fn serialize_finish_time_attack_reply(
    result_type: i32,
    attack_type: u8,
    reward_type: u8,
    training_level: u8,
) -> Vec<u8> {
    let mut packet = PacketWriter::named(FINISH_TIME_ATTACK_REPLY_NAME);
    packet.write_i32(result_type);
    if attack_type == 0 && reward_type == 1 {
        packet.write_i32(0);
        packet.write_i32(0);
        packet.write_bytes(&[0; 4]);
        packet.write_i32(0);
        packet.write_u8(training_level);
        packet.write_i32(0);
    } else {
        packet.write_bytes(&[
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0, 0,
        ]);
    }
    match reward_type {
        0 => {
            packet.write_u32(10);
            packet.write_u32(20);
        }
        1 => {
            packet.write_u32(20);
            packet.write_u32(50);
        }
        _ => unreachable!("parser and caller validate the reward type"),
    }
    let packet = packet.into_inner();
    debug_assert_eq!(packet.len(), FINISH_TIME_ATTACK_REPLY_LENGTH);
    packet
}

fn expected_length(kind: SinglePlayerRequestKind) -> usize {
    match kind {
        SinglePlayerRequestKind::StartSingle => START_SINGLE_PACKET_LENGTH,
        SinglePlayerRequestKind::UseItem => USE_SINGLE_ITEM_PACKET_LENGTH,
        SinglePlayerRequestKind::KartSpec => KART_SPEC_REQUEST_LENGTH,
        SinglePlayerRequestKind::StartTimeAttack => START_TIME_ATTACK_REQUEST_LENGTH,
        SinglePlayerRequestKind::FinishTimeAttack => FINISH_TIME_ATTACK_REQUEST_LENGTH,
    }
}

fn require_length(
    kind: SinglePlayerRequestKind,
    packet: &[u8],
) -> Result<(), SinglePlayerProtocolError> {
    let expected = expected_length(kind);
    if packet.len() == expected {
        Ok(())
    } else {
        Err(SinglePlayerProtocolError::InvalidLength {
            packet: kind.request_name(),
            actual: packet.len(),
            expected,
        })
    }
}

fn require_hash(
    reader: &mut PacketReader<'_>,
    kind: SinglePlayerRequestKind,
) -> Result<(), SinglePlayerProtocolError> {
    let actual = reader.read_u32()?;
    let expected = adler32::packet_hash(kind.request_name());
    if actual == expected {
        Ok(())
    } else {
        Err(SinglePlayerProtocolError::PacketHashMismatch {
            packet: kind.request_name(),
            actual,
            expected,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FINISH_TIME_ATTACK_REPLY_LENGTH, KART_SPEC_REPLY_LENGTH, SinglePlayerProtocolError,
        SinglePlayerRequest, SinglePlayerRequestKind, USE_SINGLE_ITEM_REQUEST_NAME,
        UseSingleItemRequest, classify_single_player_request, parse_single_player_request,
        serialize_finish_time_attack_reply, serialize_kart_spec_reply,
    };
    use crate::{adler32, packet::PacketWriter, race_start_protocol::P5136KartPhysicsBlock};

    fn captured(hex: &str) -> Vec<u8> {
        hex.split_ascii_whitespace()
            .map(|byte| u8::from_str_radix(byte, 16).unwrap())
            .collect()
    }

    #[test]
    fn captured_requests_parse_with_their_complete_producer_shape() {
        let kart = captured("DE 03 D1 14 00 3C 05 20 00 14 00 00 00 01 00");
        let request =
            parse_single_player_request(SinglePlayerRequestKind::KartSpec, &kart).unwrap();
        assert!(matches!(
            request,
            SinglePlayerRequest::KartSpec(request)
                if request.speed_type == 0
                    && request.kart_id == 1_340
                    && request.flying_pet_id == 32
                    && request.producer_context == [0x14, 0, 0, 0, 1, 0]
        ));

        let start = captured(
            "B6 06 E8 3B B0 A2 04 2B 00 00 00 00 8E 03 1E 2B 07 00 79 05 20 00 \
             00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00",
        );
        let request =
            parse_single_player_request(SinglePlayerRequestKind::StartTimeAttack, &start).unwrap();
        assert!(matches!(
            request,
            SinglePlayerRequest::StartTimeAttack(request)
                if request.start_token == 0x2B04_A2B0
                    && request.requested_track == 0x2B1E_038E
                    && request.speed_type == 7
                    && request.kart_id == 1_401
                    && request.flying_pet_id == 32
                    && request.entry_fee() == 0
        ));

        let finish = captured(
            "09 07 EF 41 02 00 00 00 61 00 24 01 01 52 45 9C 11 00 00 00 00 1F \
             00 00 00 04 00 00 00 63 8D 01 00",
        );
        let request =
            parse_single_player_request(SinglePlayerRequestKind::FinishTimeAttack, &finish)
                .unwrap();
        assert!(matches!(
            request,
            SinglePlayerRequest::FinishTimeAttack(request)
                if request.result_type == 2
                    && request.reward_type == 1
                    && request.booster_count == 31
                    && request.crash_count == 4
                    && request.race_time == 101_731
        ));
    }

    #[test]
    fn captured_finish_reply_is_exact() {
        let expected = captured(
            "0A 07 00 42 02 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 \
             00 00 00 00 01 00 00 00 00 14 00 00 00 32 00 00 00",
        );
        assert_eq!(serialize_finish_time_attack_reply(2, 0, 1, 1), expected);
        assert_eq!(expected.len(), FINISH_TIME_ATTACK_REPLY_LENGTH);
    }

    #[test]
    fn kart_reply_wraps_exactly_one_physics_block() {
        let block = P5136KartPhysicsBlock::from([0x5a; 235]);
        let reply = serialize_kart_spec_reply(&block);
        assert_eq!(reply.len(), KART_SPEC_REPLY_LENGTH);
        assert_eq!(
            u32::from_le_bytes(reply[..4].try_into().unwrap()),
            adler32::packet_hash("PrKartSpec")
        );
        assert_eq!(reply[4], 1);
        assert_eq!(&reply[5..240], block.as_bytes());
        assert_eq!(reply[240], 0);
    }

    #[test]
    fn classifier_and_length_checks_fail_closed() {
        assert_eq!(
            classify_single_player_request(adler32::packet_hash("LoRqUseItemPacket")),
            Some(SinglePlayerRequestKind::UseItem)
        );
        let short = adler32::packet_hash("LoRqUseItemPacket").to_le_bytes();
        assert!(matches!(
            parse_single_player_request(SinglePlayerRequestKind::UseItem, &short),
            Err(SinglePlayerProtocolError::InvalidLength {
                actual: 4,
                expected: 10,
                ..
            })
        ));
    }

    #[test]
    fn use_item_fields_are_three_unsigned_words() {
        let mut packet = PacketWriter::named(USE_SINGLE_ITEM_REQUEST_NAME);
        packet.write_u16(0x8001);
        packet.write_u16(0xFFFF);
        packet.write_u16(0xBEEF);

        assert!(matches!(
            parse_single_player_request(SinglePlayerRequestKind::UseItem, packet.as_slice()),
            Ok(SinglePlayerRequest::UseItem(UseSingleItemRequest {
                slot_item_category: 0x8001,
                slot_item_id: 0xFFFF,
                remaining_quantity: 0xBEEF,
            }))
        ));
    }
}
