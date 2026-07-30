//! Stateless and low-state packet codecs used after `PrLogin`.
//!
//! The stock client does not expose one globally fixed startup order. It
//! issues several requests as earlier replies are processed. In particular,
//! `LoRpEventRewardPacket` must follow the client's
//! `LoRqEventRewardPacket`, and `PrAddTimeEventInitPacket` must follow
//! `PqAddTimeEventInitPacket`; sending either reply speculatively races the
//! client's pending-request registration.

use thiserror::Error;

use crate::{
    adler32,
    login::LegacyTime,
    packet::{PacketError, PacketReader, PacketWriter},
};

pub const RIDER_ITEM_SNAPSHOT_WIRE_LENGTH: usize = 65;
pub const MAX_GAME_OPTION_TRAILING_BYTES: usize = 80;
pub const LOCKED_ITEM_LIST_REQUEST_NAME: &str = "PqLockedItemGet";
pub const LOCKED_ITEM_LIST_REPLY_NAME: &str = "PrLockedItemGet";

const CHANNEL_STATIC_REPLY_BODY_LENGTH: usize = 852;
const CHANNEL_STATIC_REPLY_BASE64: &str = concat!(
    "AU8DAABTAR2z0myODwAAeNqllvlTGjEUx1+9L1hgBcVaau+7Vav2PnZBHH9op6P93VFB64jgANbav77flzR1s2SzS8WZuEk+",
    "78jLS/KqROQ5aI6oS3U6oU1qUg29MrXQ28NXE+PjKSAdOsVnHdPf0e5iuoq2A8GBEUyf0AVtQYrlBoMqv6I9h2LZU7JD0Yjy",
    "YbgUq0V5UBftMq1DdJ9+YLSJ/kgp1oZdwWg+1gM5yoGq01g+1l4Qn9CjGnZpMmMNuvR4KmPVIaGUY9QU9CXtGPUEEce1+hNE",
    "M67VqyCanQF6SGfoNAQk46Xvgx7LXFYTubSjr9pNA5M6ToAcITObmOAcvcBYg6Y5bffw2cVfA1L5MQwc4HMX6tX6CsFBtZIZ",
    "c9j5/4E4M7yOWfMG6lBxKpAzX/4OK31zpkml5/qscL9Fx2jP4KD83rTm33w/QsrSjZxRaLtn/aU4UGm8WUyoUQ/WQjGhfl3s",
    "Vsm66B1kSRtiv5EX5gDcdkTGtUU61mCtDb2/IHi5u3cKxozYFvlXg73zkFN3C8bsiBa4l49wQl+Nsny/YMV7Q/3AjV2kQh8W",
    "I1D75feoPzEVksdziR3TI/ZkIbFgdNifxls3Z90z3oCuuOTaOLx8zfBtxNOHRh+eJxVQ9l7MWwXsm7H4P8LK8pJduCEwvqLr",
    "hkRb5ov5pxDtAOlAQQsT9X/Ay2wE8A09OcburQyJC76B7h6tpgOdLVjfBx8M2lrKCKh1vjI/xhtiu0/pNd8AbbjBSeJDSwvy",
    "x5qBN+kIRJl4y0BNhKaLYF2GTUX1nXyEu6LbEFB4195nepDeAH/IBZ6NYHLyiEzlrhD4mDOUfibwkxv5lIXRz27k0xhGfcfg",
    "p155ls0PrQ5VzA+tDq1PxxrbEeMc3Wo6AFeQMq0QuqHXSyaEX9prNBqqMTxiP2ic4qtuj8EBGg7krUcrPDhIaWsye7TG2JAw",
    "YjsUHq0yOEz9Fbq8KBqh/spdrqMgGV+L8kkEHV+RchBoguKzyGdwkrIJcslDH7+pRLAv4VQErL8HHnGpC6eTwL6EnURwWcKZ",
    "RHBFwtkQbA6cBxy/XCLYl7AbAYejwecDhyEJ7Es4nwguS7iQCK5IeIau8qAtsYpZusqDusgq/gB8YLMe",
);
const CHANNEL_STATIC_REPLY_BODY: [u8; CHANNEL_STATIC_REPLY_BODY_LENGTH] =
    decode_unpadded_base64(CHANNEL_STATIC_REPLY_BASE64.as_bytes());

const STARTUP_NOOP_PACKET_NAMES: &[&str] = &[
    "PcReportRaidOccur",
    "PqGameReportMyBadUdp",
    "PqEnterMagicHatPacket",
    "LoPingRequestPacket",
    "PqCountdownBoxPeriodPacket",
    "PqServerSideUdpBindCheck",
    "PqVipGradeCheck",
    "LoRqUpdateRiderSchoolDataPacket",
    "PcReportStateInGame",
    "PqNeedTimerGiftEvent",
    "GameBoosterAddPacket",
    "LoRqCheckReplayItemPacket",
    "PqGetRecommandChatServerInfo",
    "LoCheckLoginEvent",
    "PqBlockWordLogPacket",
    "PqWriteActionLogPacket",
    "PqMissionAttendPacket",
    "PqEnterShopPacket",
    "PqAddTimeEventTimerPacket",
    "PqTimeShopOpenTimePacket",
    "PqItemPresetSlotDataList",
    "VipPlaytimeCheck",
    "LoRqGetRiderItemPacket",
    "LoRqUploadFilePacket",
    "PqGetRiderQuestUX2ndData",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupRequest {
    LoginVipInfo,
    EventReward,
    AddRacingTime,
    EquipTuning,
    GetRider,
    UpdateGameOption,
    GetGameOption,
    SetPlaytimeEventTick,
    ChapterInfo,
    GetDuelMissionBulk,
    RiderSchoolData,
    RiderSchoolProgress,
    ChannelStatic,
    DynamicCommand,
    PublicCommand,
    GetFavoriteChannel,
    KartPassInit,
    KartPassReward,
    QuestUxSecond,
    GetCurrentRider,
    DisassembleFeeInfo,
    SyncDictionaryInfo,
    AddTimeEventInit,
    LockedItemList,
    ServerTime,
}

pub const STARTUP_REQUESTS: &[StartupRequest] = &[
    StartupRequest::LoginVipInfo,
    StartupRequest::EventReward,
    StartupRequest::AddRacingTime,
    StartupRequest::EquipTuning,
    StartupRequest::GetRider,
    StartupRequest::UpdateGameOption,
    StartupRequest::GetGameOption,
    StartupRequest::SetPlaytimeEventTick,
    StartupRequest::ChapterInfo,
    StartupRequest::GetDuelMissionBulk,
    StartupRequest::RiderSchoolData,
    StartupRequest::RiderSchoolProgress,
    StartupRequest::ChannelStatic,
    StartupRequest::DynamicCommand,
    StartupRequest::PublicCommand,
    StartupRequest::GetFavoriteChannel,
    StartupRequest::KartPassInit,
    StartupRequest::KartPassReward,
    StartupRequest::QuestUxSecond,
    StartupRequest::GetCurrentRider,
    StartupRequest::DisassembleFeeInfo,
    StartupRequest::SyncDictionaryInfo,
    StartupRequest::AddTimeEventInit,
    StartupRequest::LockedItemList,
    StartupRequest::ServerTime,
];

impl StartupRequest {
    #[must_use]
    pub const fn request_name(self) -> &'static str {
        match self {
            Self::LoginVipInfo => "PqLoginVipInfo",
            Self::EventReward => "LoRqEventRewardPacket",
            Self::AddRacingTime => "LoRqAddRacingTimePacket",
            Self::EquipTuning => "PqEquipTuningPacket",
            Self::GetRider => "PqGetRider",
            Self::UpdateGameOption => "PqUpdateGameOption",
            Self::GetGameOption => "PqGetGameOption",
            Self::SetPlaytimeEventTick => "PqSetPlaytimeEventTick",
            Self::ChapterInfo => "PqChapterInfoPacket",
            Self::GetDuelMissionBulk => "PqGetDuelMissionBulk",
            Self::RiderSchoolData => "PqRiderSchoolDataPacket",
            Self::RiderSchoolProgress => "PqRiderSchoolProPacket",
            Self::ChannelStatic => "ChRequestChStaticRequestPacket",
            Self::DynamicCommand => "PqDynamicCommand",
            Self::PublicCommand => "PqPubCommandPacket",
            Self::GetFavoriteChannel => "PqGetFavoriteChannel",
            Self::KartPassInit => "PqKartPassInitPacket",
            Self::KartPassReward => "PqKartPassRewardPacket",
            Self::QuestUxSecond => "PqQuestUX2ndPacket",
            Self::GetCurrentRider => "PqGetCurrentRid",
            Self::DisassembleFeeInfo => "PqDisassembleFeeInfo",
            Self::SyncDictionaryInfo => "PqSyncDictionaryInfoPacket",
            Self::AddTimeEventInit => "PqAddTimeEventInitPacket",
            Self::LockedItemList => LOCKED_ITEM_LIST_REQUEST_NAME,
            Self::ServerTime => "PqServerTime",
        }
    }

    #[must_use]
    pub const fn reply_name(self) -> Option<&'static str> {
        match self {
            Self::LoginVipInfo => Some("PrLoginVipInfo"),
            Self::EventReward => Some("LoRpEventRewardPacket"),
            Self::AddRacingTime => Some("LoRpAddRacingTimePacket"),
            Self::EquipTuning => Some("PrEquipTuningPacket"),
            Self::GetRider => Some("PrGetRider"),
            Self::UpdateGameOption => None,
            Self::GetGameOption => Some("PrGetGameOption"),
            Self::SetPlaytimeEventTick => Some("PrSetPlaytimeEventTick"),
            Self::ChapterInfo => Some("PrChapterInfoPacket"),
            Self::GetDuelMissionBulk => Some("PrGetDuelMissionBulk"),
            Self::RiderSchoolData => Some("PrRiderSchoolDataPacket"),
            Self::RiderSchoolProgress => Some("PrRiderSchoolProPacket"),
            Self::ChannelStatic => Some("ChRequestChStaticReplyPacket"),
            Self::DynamicCommand => Some("PrDynamicCommand"),
            Self::PublicCommand => Some("PrPubCommandPacket"),
            Self::GetFavoriteChannel => Some("PrGetFavoriteChannel"),
            Self::KartPassInit => Some("PrKartPassInitPacket"),
            Self::KartPassReward => Some("PrKartPassRewardPacket"),
            Self::QuestUxSecond => Some("PrQuestUX2ndPacket"),
            Self::GetCurrentRider => Some("PrGetCurrentRid"),
            Self::DisassembleFeeInfo => Some("PrDisassembleFeeInfo"),
            Self::SyncDictionaryInfo => Some("PrSyncDictionaryInfoPacket"),
            Self::AddTimeEventInit => Some("PrAddTimeEventInitPacket"),
            Self::LockedItemList => Some(LOCKED_ITEM_LIST_REPLY_NAME),
            Self::ServerTime => Some("PrServerTime"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct GameOptions {
    pub bgm_volume: f32,
    pub sound_volume: f32,
    pub main_bgm: u8,
    pub sound_effect: u8,
    pub full_screen: u8,
    pub show_mirror: u8,
    pub show_other_player_names: u8,
    pub show_outlines: u8,
    pub show_shadows: u8,
    pub high_level_effect: u8,
    pub motion_blur_effect: u8,
    pub motion_distortion_effect: u8,
    pub high_end_optimization: u8,
    pub auto_ready: u8,
    pub prop_description: u8,
    pub video_quality: u8,
    pub bgm_check: u8,
    pub sound_check: u8,
    pub show_hit_info: u8,
    pub auto_boost: u8,
    pub game_type: u8,
    pub set_ghost: u8,
    pub speed_type: u8,
    pub room_chat: u8,
    pub driving_chat: u8,
    pub show_all_player_hit_info: u8,
    pub show_team_color: u8,
    pub set_screen: u8,
    pub hide_competitive_rank: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PqUpdateGameOption {
    pub options: GameOptions,
    pub trailing: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrGetRiderFields {
    pub nickname: String,
    pub emblem_1: u16,
    pub emblem_2: u16,
    pub emblem_3: u16,
    pub rider_item_snapshot: [u8; RIDER_ITEM_SNAPSHOT_WIRE_LENGTH],
    pub lucci: u32,
    pub rp: i32,
}

#[derive(Debug, Error)]
pub enum StartupError {
    #[error(transparent)]
    Packet(#[from] PacketError),

    #[error("expected {name} hash 0x{expected:08X}, received 0x{actual:08X}")]
    UnexpectedPacketHash {
        name: &'static str,
        expected: u32,
        actual: u32,
    },

    #[error("packet has {length} trailing bytes; configured maximum is {maximum}")]
    TrailingLimitExceeded { length: usize, maximum: usize },

    #[error("{name} has {count} trailing bytes")]
    TrailingBytes { name: &'static str, count: usize },
}

#[must_use]
pub fn classify_startup_request(hash: u32) -> Option<StartupRequest> {
    STARTUP_REQUESTS
        .iter()
        .copied()
        .find(|request| adler32::packet_hash(request.request_name()) == hash)
}

// Evidence boundary: both inspected C# handlers dispatch `PqServerTime`
// solely from the already-read packet hash and do not consume its body. No
// capture or schema establishes a stricter request layout, so this module
// intentionally classifies the hash without inventing a request parser.

/// Returns whether the compatibility server deliberately consumes a request
/// without replying. `LoRqGetRiderItemPacket` is included because the complete
/// inventory must already have been sent before `PrGetRider`.
#[must_use]
pub fn is_startup_noop(hash: u32) -> bool {
    STARTUP_NOOP_PACKET_NAMES
        .iter()
        .any(|name| adler32::packet_hash(name) == hash)
}

/// Parses the persisted portion of `PqUpdateGameOption`. The legacy handler
/// ignores an optional suffix; this parser preserves it but caps the copied
/// data at the largest known option suffix.
pub fn parse_pq_update_game_option(packet: &[u8]) -> Result<PqUpdateGameOption, StartupError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, "PqUpdateGameOption")?;
    let options = read_game_options(&mut reader)?;
    let trailing = reader.remaining();
    if trailing.len() > MAX_GAME_OPTION_TRAILING_BYTES {
        return Err(StartupError::TrailingLimitExceeded {
            length: trailing.len(),
            maximum: MAX_GAME_OPTION_TRAILING_BYTES,
        });
    }
    Ok(PqUpdateGameOption {
        options,
        trailing: trailing.to_vec(),
    })
}

/// Parses the P5136 protected-item list request.
///
/// Both the P5136 compatibility handler and the stock-era handler treat this
/// request as hash-only. Rust requires exact exhaustion before returning the
/// terminal empty list.
pub fn parse_pq_locked_item_get(packet: &[u8]) -> Result<(), StartupError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, LOCKED_ITEM_LIST_REQUEST_NAME)?;
    let trailing = reader.remaining().len();
    if trailing == 0 {
        Ok(())
    } else {
        Err(StartupError::TrailingBytes {
            name: LOCKED_ITEM_LIST_REQUEST_NAME,
            count: trailing,
        })
    }
}

#[must_use]
pub fn serialize_pr_login_vip_info(premium: i32) -> Vec<u8> {
    let mut packet = PacketWriter::named("PrLoginVipInfo");
    packet.write_i32(premium);
    packet.write_u8(1);
    packet.write_i32(0);
    packet.into_inner()
}

#[must_use]
pub fn serialize_lo_rp_event_reward() -> Vec<u8> {
    let mut packet = PacketWriter::named("LoRpEventRewardPacket");
    packet.write_i32(0);
    packet.write_i32(0);
    packet.into_inner()
}

#[must_use]
pub fn serialize_lo_rp_add_racing_time() -> Vec<u8> {
    let mut packet = PacketWriter::named("LoRpAddRacingTimePacket");
    packet.write_bytes(&[0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    packet.into_inner()
}

#[must_use]
pub fn serialize_pr_equip_tuning_failure() -> Vec<u8> {
    let mut packet = PacketWriter::named("PrEquipTuningPacket");
    packet.write_i32(0);
    packet.into_inner()
}

#[must_use]
pub fn serialize_pr_get_game_option(options: &GameOptions) -> Vec<u8> {
    let mut packet = PacketWriter::named("PrGetGameOption");
    write_game_options(&mut packet, options);
    packet.write_bytes(&[0; 80]);
    packet.into_inner()
}

/// Builds only the final rider snapshot packet. The stock client requires the
/// complete legacy inventory stream to be sent before this packet.
pub fn serialize_pr_get_rider(fields: &PrGetRiderFields) -> Result<Vec<u8>, PacketError> {
    let mut packet = PacketWriter::named("PrGetRider");
    packet.write_u8(1);
    packet.write_u8(0);
    packet.write_utf16(&fields.nickname)?;
    packet.write_u16(0);
    packet.write_u16(0);
    packet.write_u16(fields.emblem_1);
    packet.write_u16(fields.emblem_2);
    packet.write_u16(fields.emblem_3);
    packet.write_bytes(&fields.rider_item_snapshot);
    packet.write_utf16("")?;
    packet.write_u32(fields.lucci);
    packet.write_i32(fields.rp);
    packet.write_bytes(&[0; 93]);
    Ok(packet.into_inner())
}

#[must_use]
pub fn serialize_pr_set_playtime_event_tick() -> Vec<u8> {
    let mut packet = PacketWriter::named("PrSetPlaytimeEventTick");
    packet.write_u8(0);
    packet.into_inner()
}

#[must_use]
pub fn serialize_pr_chapter_info() -> Vec<u8> {
    let mut packet = PacketWriter::named("PrChapterInfoPacket");
    packet.write_i32(0);
    packet.into_inner()
}

#[must_use]
pub fn serialize_pr_get_duel_mission_bulk(time: LegacyTime) -> Vec<u8> {
    let mut packet = PacketWriter::named("PrGetDuelMissionBulk");
    packet.write_i32(0);
    packet.write_i32(0);
    write_legacy_time(&mut packet, time);
    packet.write_u8(0x0f);
    packet.write_bytes(&[0; 77]);
    packet.into_inner()
}

#[must_use]
pub fn serialize_pr_rider_school_data(time: LegacyTime) -> Vec<u8> {
    let mut packet = PacketWriter::named("PrRiderSchoolDataPacket");
    packet.write_u8(6);
    packet.write_u8(34);
    write_legacy_time(&mut packet, time);
    packet.write_i32(0);
    packet.write_u8(0);
    packet.into_inner()
}

#[must_use]
pub fn serialize_pr_rider_school_progress() -> Vec<u8> {
    let mut packet = PacketWriter::named("PrRiderSchoolProPacket");
    packet.write_bytes(&[1, 33, 6, 34]);
    packet.write_bytes(&[0; 12]);
    packet.into_inner()
}

#[must_use]
pub const fn channel_static_reply_body() -> &'static [u8] {
    &CHANNEL_STATIC_REPLY_BODY
}

#[must_use]
pub fn serialize_channel_static_reply() -> Vec<u8> {
    let mut packet = PacketWriter::named("ChRequestChStaticReplyPacket");
    packet.write_bytes(channel_static_reply_body());
    packet.into_inner()
}

#[must_use]
pub fn serialize_pr_dynamic_command() -> Vec<u8> {
    let mut packet = PacketWriter::named("PrDynamicCommand");
    packet.write_u8(0);
    packet.write_i32(0);
    packet.into_inner()
}

#[must_use]
pub fn serialize_pr_public_command() -> Vec<u8> {
    let mut packet = PacketWriter::named("PrPubCommandPacket");
    packet.write_i32(0);
    packet.write_i32(0);
    packet.into_inner()
}

#[must_use]
pub fn serialize_pr_get_favorite_channel() -> Vec<u8> {
    let mut packet = PacketWriter::named("PrGetFavoriteChannel");
    packet.write_bytes(&[2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0]);
    packet.into_inner()
}

#[must_use]
pub fn serialize_pr_kart_pass_init() -> Vec<u8> {
    let mut packet = PacketWriter::named("PrKartPassInitPacket");
    packet.write_i32(3);
    packet.write_i32(0);
    packet.into_inner()
}

#[must_use]
pub fn serialize_pr_kart_pass_reward() -> Vec<u8> {
    let mut packet = PacketWriter::named("PrKartPassRewardPacket");
    packet.write_bytes(&[0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0]);
    packet.into_inner()
}

#[must_use]
pub fn serialize_pr_quest_ux_second() -> Vec<u8> {
    let mut packet = PacketWriter::named("PrQuestUX2ndPacket");
    packet.write_i32(1);
    packet.write_i32(1);
    packet.write_i32(7);
    for quest_id in 1211..=1217 {
        packet.write_i32(quest_id);
        packet.write_i32(quest_id);
        packet.write_i32(0);
        packet.write_i32(0);
        packet.write_i32(0);
        packet.write_i32(0);
        packet.write_i32(2);
        packet.write_i32(0);
        packet.write_u8(0);
    }
    packet.into_inner()
}

#[must_use]
pub fn serialize_pr_get_current_rider() -> Vec<u8> {
    let mut packet = PacketWriter::named("PrGetCurrentRid");
    packet.write_i32(0);
    packet.into_inner()
}

#[must_use]
pub fn serialize_pr_disassemble_fee_info() -> Vec<u8> {
    let mut packet = PacketWriter::named("PrDisassembleFeeInfo");
    packet.write_bytes(&[
        0x00, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0xe8, 0x03, 0x01, 0x00, 0xf4,
        0x01, 0x00, 0x00, 0xe8, 0x03, 0x01, 0x00, 0xf4, 0x01, 0x00, 0x00, 0xe8, 0x03, 0x01, 0x00,
        0xf4, 0x01,
    ]);
    packet.into_inner()
}

#[must_use]
pub fn serialize_pr_sync_dictionary_info() -> Vec<u8> {
    let mut packet = PacketWriter::named("PrSyncDictionaryInfoPacket");
    packet.write_i32(1);
    packet.write_i32(1);
    packet.write_bytes(&[0; 24]);
    packet.into_inner()
}

#[must_use]
pub fn serialize_pr_add_time_event_init(time: LegacyTime) -> Vec<u8> {
    let mut packet = PacketWriter::named("PrAddTimeEventInitPacket");
    packet.write_bytes(&[
        0x6d, 0xf8, 0x03, 0x00, 0xe8, 0xb3, 0x18, 0x15, 0xef, 0xb3, 0x17, 0x15,
    ]);
    packet.write_i32(6);
    packet.write_bytes(&[
        0x2f, 0x7d, 0xa1, 0x8b, 0x28, 0x57, 0xbf, 0x7e, 0x3b, 0x6d, 0xa8, 0x52,
    ]);
    packet.write_i32(0);
    packet.write_i32(0);
    packet.write_bytes(&[0x6d, 0xf8, 0x03, 0x00]);
    packet.write_i32(0);
    packet.write_i32(0);
    packet.write_i32(0);
    write_legacy_time(&mut packet, time);
    packet.write_i32(0);
    packet.into_inner()
}

/// Serializes the terminal empty protected-item list.
#[must_use]
pub fn serialize_empty_locked_item_list() -> Vec<u8> {
    let mut packet = PacketWriter::named(LOCKED_ITEM_LIST_REPLY_NAME);
    packet.write_i32(0);
    packet.into_inner()
}

/// Serializes the legacy four-byte server clock representation.
#[must_use]
pub fn serialize_pr_server_time(time: LegacyTime) -> Vec<u8> {
    let mut packet = PacketWriter::named("PrServerTime");
    write_legacy_time(&mut packet, time);
    packet.into_inner()
}

fn expect_hash(reader: &mut PacketReader<'_>, name: &'static str) -> Result<(), StartupError> {
    let actual = reader.read_u32()?;
    let expected = adler32::packet_hash(name);
    if actual == expected {
        Ok(())
    } else {
        Err(StartupError::UnexpectedPacketHash {
            name,
            expected,
            actual,
        })
    }
}

fn read_game_options(reader: &mut PacketReader<'_>) -> Result<GameOptions, PacketError> {
    Ok(GameOptions {
        bgm_volume: f32::from_bits(reader.read_u32()?),
        sound_volume: f32::from_bits(reader.read_u32()?),
        main_bgm: reader.read_u8()?,
        sound_effect: reader.read_u8()?,
        full_screen: reader.read_u8()?,
        show_mirror: reader.read_u8()?,
        show_other_player_names: reader.read_u8()?,
        show_outlines: reader.read_u8()?,
        show_shadows: reader.read_u8()?,
        high_level_effect: reader.read_u8()?,
        motion_blur_effect: reader.read_u8()?,
        motion_distortion_effect: reader.read_u8()?,
        high_end_optimization: reader.read_u8()?,
        auto_ready: reader.read_u8()?,
        prop_description: reader.read_u8()?,
        video_quality: reader.read_u8()?,
        bgm_check: reader.read_u8()?,
        sound_check: reader.read_u8()?,
        show_hit_info: reader.read_u8()?,
        auto_boost: reader.read_u8()?,
        game_type: reader.read_u8()?,
        set_ghost: reader.read_u8()?,
        speed_type: reader.read_u8()?,
        room_chat: reader.read_u8()?,
        driving_chat: reader.read_u8()?,
        show_all_player_hit_info: reader.read_u8()?,
        show_team_color: reader.read_u8()?,
        set_screen: reader.read_u8()?,
        hide_competitive_rank: reader.read_u8()?,
    })
}

fn write_game_options(packet: &mut PacketWriter, options: &GameOptions) {
    packet.write_u32(options.bgm_volume.to_bits());
    packet.write_u32(options.sound_volume.to_bits());
    packet.write_bytes(&[
        options.main_bgm,
        options.sound_effect,
        options.full_screen,
        options.show_mirror,
        options.show_other_player_names,
        options.show_outlines,
        options.show_shadows,
        options.high_level_effect,
        options.motion_blur_effect,
        options.motion_distortion_effect,
        options.high_end_optimization,
        options.auto_ready,
        options.prop_description,
        options.video_quality,
        options.bgm_check,
        options.sound_check,
        options.show_hit_info,
        options.auto_boost,
        options.game_type,
        options.set_ghost,
        options.speed_type,
        options.room_chat,
        options.driving_chat,
        options.show_all_player_hit_info,
        options.show_team_color,
        options.set_screen,
        options.hide_competitive_rank,
    ]);
}

fn write_legacy_time(packet: &mut PacketWriter, time: LegacyTime) {
    packet.write_u16(time.days_since_1900);
    packet.write_u16(time.quarter_seconds);
}

const fn decode_unpadded_base64<const N: usize>(input: &[u8]) -> [u8; N] {
    assert!(input.len().is_multiple_of(4));
    assert!(input.len() / 4 * 3 == N);

    let mut output = [0; N];
    let mut input_index = 0;
    let mut output_index = 0;
    while input_index < input.len() {
        let a = base64_value(input[input_index]);
        let b = base64_value(input[input_index + 1]);
        let c = base64_value(input[input_index + 2]);
        let d = base64_value(input[input_index + 3]);
        output[output_index] = (a << 2) | (b >> 4);
        output[output_index + 1] = ((b & 0x0f) << 4) | (c >> 2);
        output[output_index + 2] = ((c & 0x03) << 6) | d;
        input_index += 4;
        output_index += 3;
    }
    output
}

const fn base64_value(byte: u8) -> u8 {
    match byte {
        b'A'..=b'Z' => byte - b'A',
        b'a'..=b'z' => byte - b'a' + 26,
        b'0'..=b'9' => byte - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => panic!("invalid base64 character"),
    }
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::{
        GameOptions, LOCKED_ITEM_LIST_REPLY_NAME, LOCKED_ITEM_LIST_REQUEST_NAME,
        MAX_GAME_OPTION_TRAILING_BYTES, PrGetRiderFields, RIDER_ITEM_SNAPSHOT_WIRE_LENGTH,
        StartupError, StartupRequest, channel_static_reply_body, classify_startup_request,
        is_startup_noop, parse_pq_locked_item_get, parse_pq_update_game_option,
        serialize_channel_static_reply, serialize_empty_locked_item_list,
        serialize_lo_rp_add_racing_time, serialize_lo_rp_event_reward,
        serialize_pr_add_time_event_init, serialize_pr_chapter_info,
        serialize_pr_disassemble_fee_info, serialize_pr_dynamic_command,
        serialize_pr_equip_tuning_failure, serialize_pr_get_current_rider,
        serialize_pr_get_duel_mission_bulk, serialize_pr_get_favorite_channel,
        serialize_pr_get_game_option, serialize_pr_get_rider, serialize_pr_kart_pass_init,
        serialize_pr_kart_pass_reward, serialize_pr_login_vip_info, serialize_pr_public_command,
        serialize_pr_quest_ux_second, serialize_pr_rider_school_data,
        serialize_pr_rider_school_progress, serialize_pr_server_time,
        serialize_pr_set_playtime_event_tick, serialize_pr_sync_dictionary_info,
    };
    use crate::{
        adler32,
        login::LegacyTime,
        packet::{PacketReader, PacketWriter},
    };

    #[test]
    fn request_classifier_preserves_pairing_and_no_reply_contracts() {
        for request in super::STARTUP_REQUESTS {
            assert_eq!(
                classify_startup_request(adler32::packet_hash(request.request_name())),
                Some(*request)
            );
        }
        assert_eq!(StartupRequest::UpdateGameOption.reply_name(), None);
        assert_eq!(
            StartupRequest::EventReward.reply_name(),
            Some("LoRpEventRewardPacket")
        );
        assert_eq!(
            StartupRequest::AddTimeEventInit.reply_name(),
            Some("PrAddTimeEventInitPacket")
        );
        assert!(is_startup_noop(adler32::packet_hash(
            "LoRqGetRiderItemPacket"
        )));
        assert!(!is_startup_noop(adler32::packet_hash("PqGetRider")));
        assert_eq!(classify_startup_request(0xdead_beef), None);
    }

    #[test]
    fn locked_item_list_is_strict_hash_only_and_exact_terminal_empty() {
        assert_eq!(
            adler32::packet_hash(LOCKED_ITEM_LIST_REQUEST_NAME),
            0x2D81_05C2
        );
        assert_eq!(
            adler32::packet_hash(LOCKED_ITEM_LIST_REPLY_NAME),
            0x2D8F_05C3
        );

        let request = PacketWriter::named(LOCKED_ITEM_LIST_REQUEST_NAME).into_inner();
        assert!(parse_pq_locked_item_get(&request).is_ok());
        for truncated_length in 0..4 {
            assert!(matches!(
                parse_pq_locked_item_get(&request[..truncated_length]),
                Err(StartupError::Packet(_))
            ));
        }
        assert!(matches!(
            parse_pq_locked_item_get(&[0; 4]),
            Err(StartupError::UnexpectedPacketHash {
                name: LOCKED_ITEM_LIST_REQUEST_NAME,
                actual: 0,
                ..
            })
        ));
        let mut trailing = request;
        trailing.push(0x51);
        assert!(matches!(
            parse_pq_locked_item_get(&trailing),
            Err(StartupError::TrailingBytes {
                name: LOCKED_ITEM_LIST_REQUEST_NAME,
                count: 1,
            })
        ));

        assert_eq!(
            serialize_empty_locked_item_list(),
            [0xC3, 0x05, 0x8F, 0x2D, 0, 0, 0, 0]
        );
    }

    #[test]
    fn game_options_round_trip_and_bound_the_ignored_suffix() {
        let options = fixture_options();
        let mut request = PacketWriter::named("PqUpdateGameOption");
        super::write_game_options(&mut request, &options);
        request.write_bytes(&[0xa5; MAX_GAME_OPTION_TRAILING_BYTES]);
        let parsed = parse_pq_update_game_option(request.as_slice()).unwrap();
        assert_eq!(parsed.options, options);
        assert_eq!(parsed.trailing, [0xa5; MAX_GAME_OPTION_TRAILING_BYTES]);

        let mut oversized = request.into_inner();
        oversized.push(0);
        assert!(matches!(
            parse_pq_update_game_option(&oversized),
            Err(StartupError::TrailingLimitExceeded {
                length: 81,
                maximum: MAX_GAME_OPTION_TRAILING_BYTES
            })
        ));
        assert!(matches!(
            parse_pq_update_game_option(&oversized[..20]),
            Err(StartupError::Packet(_))
        ));
    }

    #[test]
    fn fixed_startup_replies_match_packet_hashes_and_exact_bodies() {
        assert_packet(
            &serialize_pr_login_vip_info(5),
            676_267_382,
            &[5, 0, 0, 0, 1, 0, 0, 0, 0],
        );
        assert_packet(&serialize_lo_rp_event_reward(), 1_493_174_332, &[0; 8]);
        assert_packet(
            &serialize_lo_rp_add_racing_time(),
            1_718_487_233,
            &[0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        );
        assert_packet(&serialize_pr_equip_tuning_failure(), 1_256_654_739, &[0; 4]);
        assert_packet(&serialize_pr_set_playtime_event_tick(), 1_671_366_848, &[0]);
        assert_packet(&serialize_pr_chapter_info(), 1_224_542_061, &[0; 4]);
        assert_packet(
            &serialize_pr_rider_school_progress(),
            1_648_560_297,
            &[1, 33, 6, 34, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        );
        assert_packet(
            &serialize_pr_dynamic_command(),
            881_854_022,
            &[0, 0, 0, 0, 0],
        );
        assert_packet(&serialize_pr_public_command(), 1_096_419_072, &[0; 8]);
        assert_packet(
            &serialize_pr_get_favorite_channel(),
            1_356_924_891,
            &[2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0],
        );
        assert_packet(
            &serialize_pr_kart_pass_init(),
            1_358_366_679,
            &[3, 0, 0, 0, 0, 0, 0, 0],
        );
        assert_packet(
            &serialize_pr_kart_pass_reward(),
            1_646_987_432,
            &[0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0],
        );
        assert_packet(&serialize_pr_get_current_rider(), 774_374_884, &[0; 4]);
        assert_packet(
            &serialize_pr_sync_dictionary_info(),
            2_324_630_105,
            &[
                1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0,
            ],
        );
    }

    #[test]
    fn time_dependent_replies_match_the_csharp_layout() {
        let time = LegacyTime {
            days_since_1900: 0x1234,
            quarter_seconds: 0x5678,
        };

        let school = serialize_pr_rider_school_data(time);
        assert_packet(
            &school,
            1_783_761_138,
            &[6, 34, 0x34, 0x12, 0x78, 0x56, 0, 0, 0, 0, 0],
        );

        let duel = serialize_pr_get_duel_mission_bulk(time);
        assert_eq!(duel.len(), 94);
        assert_eq!(
            &duel[..16],
            &[
                0xdc, 0x07, 0x91, 0x50, 0, 0, 0, 0, 0, 0, 0, 0, 0x34, 0x12, 0x78, 0x56,
            ]
        );
        assert_eq!(duel[16], 0x0f);
        assert!(duel[17..].iter().all(|byte| *byte == 0));

        let add_time = serialize_pr_add_time_event_init(time);
        assert_eq!(add_time.len(), 64);
        assert_eq!(
            format!("{:X}", Sha256::digest(&add_time)),
            "4105DD09D53B9CC28AFF41C5F410FF0B6C92DC0B27BC6D4724684318CAADE834"
        );
    }

    #[test]
    fn server_time_classification_and_reply_match_the_exact_legacy_layout() {
        assert_eq!(adler32::packet_hash("PqServerTime"), 0x1E92_04C7);
        assert_eq!(adler32::packet_hash("PrServerTime"), 0x1E9D_04C8);
        assert_eq!(
            classify_startup_request(0x1E92_04C7),
            Some(StartupRequest::ServerTime)
        );
        assert_eq!(StartupRequest::ServerTime.request_name(), "PqServerTime");
        assert_eq!(
            StartupRequest::ServerTime.reply_name(),
            Some("PrServerTime")
        );

        assert_packet(
            &serialize_pr_server_time(LegacyTime {
                days_since_1900: 0x1234,
                quarter_seconds: 0x5678,
            }),
            0x1E9D_04C8,
            &[0x34, 0x12, 0x78, 0x56],
        );
    }

    #[test]
    fn channel_static_reply_is_the_exact_legacy_launcher_blob() {
        assert_eq!(channel_static_reply_body().len(), 852);
        assert_eq!(
            format!("{:X}", Sha256::digest(channel_static_reply_body())),
            "F3E3DAB5BE23416BFCB65A93A827200ACD8AD6894D9E741AED05653D8FE779DD"
        );

        let packet = serialize_channel_static_reply();
        assert_eq!(packet.len(), 856);
        assert_eq!(
            u32::from_le_bytes(packet[..4].try_into().unwrap()),
            2_646_084_363
        );
        assert_eq!(&packet[4..], channel_static_reply_body());
    }

    #[test]
    fn game_option_reply_and_rider_snapshot_match_golden_layouts() {
        let options_packet = serialize_pr_get_game_option(&fixture_options());
        assert_eq!(options_packet.len(), 119);
        assert_eq!(
            format!("{:X}", Sha256::digest(&options_packet)),
            "27CD829FED00597AE4B0ECA9378D860FEA195FA7F608E0486133E66BE50D3B96"
        );

        let rider = serialize_pr_get_rider(&PrGetRiderFields {
            nickname: "Rider".to_owned(),
            emblem_1: 0x1234,
            emblem_2: 0x5678,
            emblem_3: 0,
            rider_item_snapshot: [0xa5; 65],
            lucci: 0x1020_3040,
            rp: -1234,
        })
        .unwrap();
        assert_eq!(rider.len(), 200);
        assert_eq!(
            format!("{:X}", Sha256::digest(&rider)),
            "E966311DE7F0962D53BAB7D7DE566C536569FA60AA41F77F191799AB7C24AE70"
        );

        let mut reader = PacketReader::new(&rider);
        assert_eq!(reader.read_u32().unwrap(), 343_606_232);
        assert_eq!(reader.read_u8().unwrap(), 1);
        assert_eq!(reader.read_u8().unwrap(), 0);
        assert_eq!(reader.read_utf16().unwrap(), "Rider");
        assert_eq!(reader.remaining().len(), 180);
    }

    #[test]
    fn rider_snapshot_writes_nonzero_third_emblem_before_equipment() {
        let rider_items = [0xa5; RIDER_ITEM_SNAPSHOT_WIRE_LENGTH];
        let rider = serialize_pr_get_rider(&PrGetRiderFields {
            nickname: "ThirdEmblem".to_owned(),
            emblem_1: 0x1234,
            emblem_2: 0x5678,
            emblem_3: 0x9abc,
            rider_item_snapshot: rider_items,
            lucci: 0,
            rp: 0,
        })
        .unwrap();

        let mut reader = PacketReader::new(&rider);
        assert_eq!(
            reader.read_u32().unwrap(),
            adler32::packet_hash("PrGetRider")
        );
        assert_eq!(reader.read_u8().unwrap(), 1);
        assert_eq!(reader.read_u8().unwrap(), 0);
        assert_eq!(reader.read_utf16().unwrap(), "ThirdEmblem");
        assert_eq!(reader.read_u16().unwrap(), 0);
        assert_eq!(reader.read_u16().unwrap(), 0);
        assert_eq!(reader.read_u16().unwrap(), 0x1234);
        assert_eq!(reader.read_u16().unwrap(), 0x5678);
        assert_eq!(reader.read_u16().unwrap(), 0x9abc);
        assert_eq!(
            reader.read_bytes(RIDER_ITEM_SNAPSHOT_WIRE_LENGTH).unwrap(),
            rider_items.as_slice()
        );
    }

    #[test]
    fn quest_and_disassembly_payload_lengths_are_not_rounded() {
        let quest = serialize_pr_quest_ux_second();
        assert_eq!(quest.len(), 247);
        assert_eq!(
            format!("{:X}", Sha256::digest(&quest)),
            "B27681086197512D284A5F506DDAD1E4005B9CA0A8C4ED8CE7D08EC6079C8C40"
        );

        let disassembly = serialize_pr_disassemble_fee_info();
        assert_eq!(disassembly.len(), 36);
        assert_eq!(
            format!("{:X}", Sha256::digest(&disassembly)),
            "005455C733F2B3B30FFD39313A300E2B784533F90131BF62B3F06F006EECD903"
        );
    }

    fn fixture_options() -> GameOptions {
        GameOptions {
            bgm_volume: 0.5,
            sound_volume: 0.25,
            main_bgm: 1,
            sound_effect: 2,
            full_screen: 3,
            show_mirror: 4,
            show_other_player_names: 5,
            show_outlines: 6,
            show_shadows: 7,
            high_level_effect: 8,
            motion_blur_effect: 9,
            motion_distortion_effect: 10,
            high_end_optimization: 11,
            auto_ready: 12,
            prop_description: 13,
            video_quality: 14,
            bgm_check: 15,
            sound_check: 16,
            show_hit_info: 17,
            auto_boost: 18,
            game_type: 19,
            set_ghost: 20,
            speed_type: 21,
            room_chat: 22,
            driving_chat: 23,
            show_all_player_hit_info: 24,
            show_team_color: 25,
            set_screen: 26,
            hide_competitive_rank: 27,
        }
    }

    fn assert_packet(packet: &[u8], expected_hash: u32, expected_body: &[u8]) {
        assert_eq!(
            u32::from_le_bytes(packet[..4].try_into().unwrap()),
            expected_hash
        );
        assert_eq!(&packet[4..], expected_body);
    }
}
