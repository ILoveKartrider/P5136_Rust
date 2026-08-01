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
    kart_physics::{
        KartPhysicsBuildError, P5136KartPhysicsSnapshot, build_p5136_kart_physics_block,
    },
    login::LegacyTime,
    packet::{PacketError, PacketReader, PacketWriter},
};

pub const RIDER_ITEM_SNAPSHOT_WIRE_LENGTH: usize = 65;
pub const MAX_GAME_OPTION_TRAILING_BYTES: usize = 80;
pub const LOCKED_ITEM_LIST_REQUEST_NAME: &str = "PqLockedItemGet";
pub const LOCKED_ITEM_LIST_REPLY_NAME: &str = "PrLockedItemGet";
pub const REQUEST_EXTRADATA_REQUEST_NAME: &str = "PqRequestExtradata";
pub const REQUEST_EXTRADATA_REPLY_NAME: &str = "PrRequestExtradata";
pub const WEB_EVENT_COMPLETE_CHECK_REQUEST_NAME: &str = "PqWebEventCompleteCheckPacket";
pub const WEB_EVENT_COMPLETE_CHECK_REPLY_NAME: &str = "PrWebEventCompleteCheckPacket";
pub const START_RIDER_SCHOOL_REQUEST_NAME: &str = "PqStartRiderSchool";
pub const START_RIDER_SCHOOL_REPLY_NAME: &str = "PrStartRiderSchool";
pub const START_RIDER_SCHOOL_REQUEST_HASH: u32 = 0x4327_072D;
pub const START_RIDER_SCHOOL_REPLY_HASH: u32 = 0x4338_072E;
pub const GET_RIDER_TASK_CONTEXT_REQUEST_NAME: &str = "PqGetRiderTaskContext";
pub const GET_RIDER_TASK_CONTEXT_REPLY_NAME: &str = "PrGetRiderTaskContext";
pub const GET_RIDER_TASK_CONTEXT_REQUEST_HASH: u32 = 0x5870_084F;
pub const GET_RIDER_TASK_CONTEXT_REPLY_HASH: u32 = 0x5884_0850;
pub const VERSUS_MODE_RANK_ONE_REQUEST_NAME: &str = "PqVersusModeRankOnePacket";
pub const VERSUS_MODE_RANK_ONE_REPLY_NAME: &str = "PrVersusModeRankOnePacket";
pub const VERSUS_MODE_RANK_ONE_REQUEST_HASH: u32 = 0x7FC2_09D4;
pub const VERSUS_MODE_RANK_ONE_REPLY_HASH: u32 = 0x7FDA_09D5;
pub const RIDER_SCHOOL_EXPIRED_CHECK_REQUEST_NAME: &str = "PqRiderSchoolExpiredCheck";
pub const RIDER_SCHOOL_EXPIRED_CHECK_REPLY_NAME: &str = "PrRiderSchoolExpiredCheck";
pub const RIDER_SCHOOL_EXPIRED_CHECK_REQUEST_HASH: u32 = 0x7EC1_09CE;
pub const RIDER_SCHOOL_EXPIRED_CHECK_REPLY_HASH: u32 = 0x7ED9_09CF;
pub const RANKER_INFO_REQUEST_NAME: &str = "PqRankerInfoPacket";
pub const RANKER_INFO_REPLY_NAME: &str = "PrRankerInfoPacket";
pub const RANKER_INFO_REQUEST_HASH: u32 = 0x41C6_0708;
pub const RANKER_INFO_REPLY_HASH: u32 = 0x41D7_0709;
pub const GET_MAX_GIFT_ID_REQUEST_NAME: &str = "SpRqGetMaxGiftIdPacket";
pub const GET_MAX_GIFT_ID_REPLY_NAME: &str = "SpRpGetMaxGiftIdPacket";
pub const GET_MAX_GIFT_ID_REQUEST_HASH: u32 = 0x5EB4_085B;
pub const GET_MAX_GIFT_ID_REPLY_HASH: u32 = 0x5EA1_085A;
pub const KOIN_BALANCE_REQUEST_NAME: &str = "SpRqKoinBalance";
pub const KOIN_BALANCE_REPLY_NAME: &str = "SpRpKoinBalance";
pub const KOIN_BALANCE_REQUEST_HASH: u32 = 0x2D4C_05BD;
pub const KOIN_BALANCE_REPLY_HASH: u32 = 0x2D40_05BC;
pub const FAVORITE_TRACK_MAP_REQUEST_NAME: &str = "PqFavoriteTrackMapGet";
pub const FAVORITE_TRACK_MAP_REPLY_NAME: &str = "PrFavoriteTrackMapGet";
pub const FAVORITE_TRACK_MAP_REQUEST_HASH: u32 = 0x5A3E_0834;
pub const FAVORITE_TRACK_MAP_REPLY_HASH: u32 = 0x5A52_0835;
pub const GET_CASH_INVENTORY_REQUEST_NAME: &str = "SpRqGetCashInventoryPacket";
pub const GET_CASH_INVENTORY_REPLY_NAME: &str = "SpRpGetCashInventoryPacket";
pub const GET_CASH_INVENTORY_REQUEST_HASH: u32 = 0x8773_0A4B;
pub const GET_CASH_INVENTORY_REPLY_HASH: u32 = 0x875C_0A4A;
pub const REMAIN_CASH_REQUEST_NAME: &str = "SpRqRemainCashPacket";
pub const REMAIN_CASH_REPLY_NAME: &str = "SpRpRemainCashPacket";
pub const REMAIN_CASH_REQUEST_HASH: u32 = 0x4FEC_07B9;
pub const REMAIN_CASH_REPLY_HASH: u32 = 0x4FDB_07B8;
pub const REMAIN_TC_CASH_REQUEST_NAME: &str = "SpRqRemainTcCashPacket";
pub const REMAIN_TC_CASH_REPLY_NAME: &str = "SpRpRemainTcCashPacket";
pub const REMAIN_TC_CASH_REQUEST_HASH: u32 = 0x5FE1_0870;
pub const REMAIN_TC_CASH_REPLY_HASH: u32 = 0x5FCE_086F;

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
    "PqNeedTimerGiftEvent",
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
    GetRiderTaskContext,
    VersusModeRankOne,
    UpdateGameOption,
    GetGameOption,
    SetPlaytimeEventTick,
    ChapterInfo,
    GetDuelMissionBulk,
    RiderSchoolData,
    RiderSchoolProgress,
    RiderSchoolExpiredCheck,
    StartRiderSchool,
    RankerInfo,
    GetMaxGiftId,
    KoinBalance,
    FavoriteTrackMap,
    GetCashInventory,
    RemainCash,
    RemainTcCash,
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
    RequestExtradata,
    WebEventCompleteCheck,
}

pub const STARTUP_REQUESTS: &[StartupRequest] = &[
    StartupRequest::LoginVipInfo,
    StartupRequest::EventReward,
    StartupRequest::AddRacingTime,
    StartupRequest::EquipTuning,
    StartupRequest::GetRider,
    StartupRequest::GetRiderTaskContext,
    StartupRequest::VersusModeRankOne,
    StartupRequest::UpdateGameOption,
    StartupRequest::GetGameOption,
    StartupRequest::SetPlaytimeEventTick,
    StartupRequest::ChapterInfo,
    StartupRequest::GetDuelMissionBulk,
    StartupRequest::RiderSchoolData,
    StartupRequest::RiderSchoolProgress,
    StartupRequest::RiderSchoolExpiredCheck,
    StartupRequest::StartRiderSchool,
    StartupRequest::RankerInfo,
    StartupRequest::GetMaxGiftId,
    StartupRequest::KoinBalance,
    StartupRequest::FavoriteTrackMap,
    StartupRequest::GetCashInventory,
    StartupRequest::RemainCash,
    StartupRequest::RemainTcCash,
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
    StartupRequest::RequestExtradata,
    StartupRequest::WebEventCompleteCheck,
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
            Self::GetRiderTaskContext => GET_RIDER_TASK_CONTEXT_REQUEST_NAME,
            Self::VersusModeRankOne => VERSUS_MODE_RANK_ONE_REQUEST_NAME,
            Self::UpdateGameOption => "PqUpdateGameOption",
            Self::GetGameOption => "PqGetGameOption",
            Self::SetPlaytimeEventTick => "PqSetPlaytimeEventTick",
            Self::ChapterInfo => "PqChapterInfoPacket",
            Self::GetDuelMissionBulk => "PqGetDuelMissionBulk",
            Self::RiderSchoolData => "PqRiderSchoolDataPacket",
            Self::RiderSchoolProgress => "PqRiderSchoolProPacket",
            Self::RiderSchoolExpiredCheck => RIDER_SCHOOL_EXPIRED_CHECK_REQUEST_NAME,
            Self::StartRiderSchool => START_RIDER_SCHOOL_REQUEST_NAME,
            Self::RankerInfo => RANKER_INFO_REQUEST_NAME,
            Self::GetMaxGiftId => GET_MAX_GIFT_ID_REQUEST_NAME,
            Self::KoinBalance => KOIN_BALANCE_REQUEST_NAME,
            Self::FavoriteTrackMap => FAVORITE_TRACK_MAP_REQUEST_NAME,
            Self::GetCashInventory => GET_CASH_INVENTORY_REQUEST_NAME,
            Self::RemainCash => REMAIN_CASH_REQUEST_NAME,
            Self::RemainTcCash => REMAIN_TC_CASH_REQUEST_NAME,
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
            Self::RequestExtradata => REQUEST_EXTRADATA_REQUEST_NAME,
            Self::WebEventCompleteCheck => WEB_EVENT_COMPLETE_CHECK_REQUEST_NAME,
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
            Self::GetRiderTaskContext => Some(GET_RIDER_TASK_CONTEXT_REPLY_NAME),
            Self::VersusModeRankOne => Some(VERSUS_MODE_RANK_ONE_REPLY_NAME),
            Self::UpdateGameOption => None,
            Self::GetGameOption => Some("PrGetGameOption"),
            Self::SetPlaytimeEventTick => Some("PrSetPlaytimeEventTick"),
            Self::ChapterInfo => Some("PrChapterInfoPacket"),
            Self::GetDuelMissionBulk => Some("PrGetDuelMissionBulk"),
            Self::RiderSchoolData => Some("PrRiderSchoolDataPacket"),
            Self::RiderSchoolProgress => Some("PrRiderSchoolProPacket"),
            Self::RiderSchoolExpiredCheck => Some(RIDER_SCHOOL_EXPIRED_CHECK_REPLY_NAME),
            Self::StartRiderSchool => Some(START_RIDER_SCHOOL_REPLY_NAME),
            Self::RankerInfo => Some(RANKER_INFO_REPLY_NAME),
            Self::GetMaxGiftId => Some(GET_MAX_GIFT_ID_REPLY_NAME),
            Self::KoinBalance => Some(KOIN_BALANCE_REPLY_NAME),
            Self::FavoriteTrackMap => Some(FAVORITE_TRACK_MAP_REPLY_NAME),
            Self::GetCashInventory => Some(GET_CASH_INVENTORY_REPLY_NAME),
            Self::RemainCash => Some(REMAIN_CASH_REPLY_NAME),
            Self::RemainTcCash => Some(REMAIN_TC_CASH_REPLY_NAME),
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
            Self::RequestExtradata => Some(REQUEST_EXTRADATA_REPLY_NAME),
            Self::WebEventCompleteCheck => Some(WEB_EVENT_COMPLETE_CHECK_REPLY_NAME),
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

/// The single encoded byte produced by the stock `PqStartRiderSchool` client.
///
/// Neither the stock executable nor the available server code establishes a
/// business meaning or a narrower valid range. Construction is intentionally
/// limited to [`parse_pq_start_rider_school`], which accepts the complete
/// decoded `u8` domain without inventing a policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartRiderSchoolRequest(u8);

impl StartRiderSchoolRequest {
    /// Returns the decoded but otherwise opaque request value.
    #[must_use]
    pub const fn value(self) -> u8 {
        self.0
    }
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

    #[error("SpRqKoinBalance mode must be 1, received {actual}")]
    UnexpectedKoinBalanceMode { actual: u8 },
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
    parse_hash_only_request(packet, LOCKED_ITEM_LIST_REQUEST_NAME)
}

/// Parses the stock client's exact hash-only rider task-context request.
///
/// The available C# handler does not consume a body, and the crashing P5136
/// client sent exactly the four-byte hash. Rust therefore keeps this read-only
/// compatibility boundary strict instead of accepting unproven trailing data.
pub fn parse_pq_get_rider_task_context(packet: &[u8]) -> Result<(), StartupError> {
    parse_hash_only_request(packet, GET_RIDER_TASK_CONTEXT_REQUEST_NAME)
}

/// Parses the stock client's exact hash-only versus-rank summary request.
pub fn parse_pq_versus_mode_rank_one(packet: &[u8]) -> Result<(), StartupError> {
    parse_hash_only_request(packet, VERSUS_MODE_RANK_ONE_REQUEST_NAME)
}

/// Parses the stock client's exact hash-only rider-school expiration request.
pub fn parse_pq_rider_school_expired_check(packet: &[u8]) -> Result<(), StartupError> {
    parse_hash_only_request(packet, RIDER_SCHOOL_EXPIRED_CHECK_REQUEST_NAME)
}

/// Parses the stock client's exact hash-only ranker-info request.
pub fn parse_pq_ranker_info(packet: &[u8]) -> Result<(), StartupError> {
    parse_hash_only_request(packet, RANKER_INFO_REQUEST_NAME)
}

/// Parses the exact hash-only gift sequence query observed in the startup log.
pub fn parse_sp_rq_get_max_gift_id(packet: &[u8]) -> Result<(), StartupError> {
    parse_hash_only_request(packet, GET_MAX_GIFT_ID_REQUEST_NAME)
}

/// Parses the exact five-byte KOIN balance query found in every retained
/// stock-client initialization capture.
///
/// The C# handler ignores the terminal byte, so its business meaning is not
/// established. Rust still requires the one observed value (`1`) and exact
/// exhaustion instead of treating arbitrary trailing bytes as valid.
pub fn parse_sp_rq_koin_balance(packet: &[u8]) -> Result<(), StartupError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, KOIN_BALANCE_REQUEST_NAME)?;
    let actual = reader.read_u8()?;
    if actual != 1 {
        return Err(StartupError::UnexpectedKoinBalanceMode { actual });
    }
    let trailing = reader.remaining().len();
    if trailing != 0 {
        return Err(StartupError::TrailingBytes {
            name: KOIN_BALANCE_REQUEST_NAME,
            count: trailing,
        });
    }
    Ok(())
}

/// Parses the exact hash-only favorite-track projection query.
pub fn parse_pq_favorite_track_map_get(packet: &[u8]) -> Result<(), StartupError> {
    parse_hash_only_request(packet, FAVORITE_TRACK_MAP_REQUEST_NAME)
}

/// Parses the exact hash-only cash-inventory query.
pub fn parse_sp_rq_get_cash_inventory(packet: &[u8]) -> Result<(), StartupError> {
    parse_hash_only_request(packet, GET_CASH_INVENTORY_REQUEST_NAME)
}

/// Parses the exact hash-only Cash balance query.
pub fn parse_sp_rq_remain_cash(packet: &[u8]) -> Result<(), StartupError> {
    parse_hash_only_request(packet, REMAIN_CASH_REQUEST_NAME)
}

/// Parses the exact hash-only TC Cash balance query.
pub fn parse_sp_rq_remain_tc_cash(packet: &[u8]) -> Result<(), StartupError> {
    parse_hash_only_request(packet, REMAIN_TC_CASH_REQUEST_NAME)
}

/// Parses the stock client's exact hash-only extra-data request.
///
/// The stock producer allocates only the base packet and writes no payload
/// fields. Rust therefore rejects the trailing bytes that the C# handler
/// ignored instead of widening the evidenced wire shape.
pub fn parse_pq_request_extradata(packet: &[u8]) -> Result<(), StartupError> {
    parse_hash_only_request(packet, REQUEST_EXTRADATA_REQUEST_NAME)
}

/// Parses the stock client's exact hash-only web-event completion request.
///
/// The stock producer allocates only the base packet and writes no payload
/// fields. Rust therefore requires complete consumption.
pub fn parse_pq_web_event_complete_check(packet: &[u8]) -> Result<(), StartupError> {
    parse_hash_only_request(packet, WEB_EVENT_COMPLETE_CHECK_REQUEST_NAME)
}

/// Parses the stock client's exact five-byte rider-school start request.
///
/// The producer writes one encoded byte after the packet hash. The C# handler
/// ignored both that byte and any trailing data; Rust preserves the evidenced
/// field while rejecting truncation and every byte beyond it.
pub fn parse_pq_start_rider_school(packet: &[u8]) -> Result<StartRiderSchoolRequest, StartupError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, START_RIDER_SCHOOL_REQUEST_NAME)?;
    let request = StartRiderSchoolRequest(reader.read_encoded_u8()?);
    let trailing = reader.remaining().len();
    if trailing != 0 {
        return Err(StartupError::TrailingBytes {
            name: START_RIDER_SCHOOL_REQUEST_NAME,
            count: trailing,
        });
    }
    Ok(request)
}

fn parse_hash_only_request(packet: &[u8], name: &'static str) -> Result<(), StartupError> {
    let mut reader = PacketReader::new(packet);
    expect_hash(&mut reader, name)?;
    let trailing = reader.remaining().len();
    if trailing == 0 {
        Ok(())
    } else {
        Err(StartupError::TrailingBytes {
            name,
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

/// Serializes the safe canonical P5136 rider-school physics reply.
///
/// The C# compatibility shortcut hardcoded two start-acceleration fields as
/// `2305.0` and `3745.0`, while its normal physics formula produces `2304.0`
/// and `3745.587890625`. Rust deliberately reuses the canonical physics
/// builder instead of cloning that isolated two-field drift.
///
/// # Errors
///
/// Returns [`KartPhysicsBuildError`] if the canonical snapshot cannot satisfy
/// the validated P5136 physics layout.
pub fn serialize_pr_start_rider_school() -> Result<Vec<u8>, KartPhysicsBuildError> {
    let physics = build_p5136_kart_physics_block(&P5136KartPhysicsSnapshot::csharp_s7_baseline())?;
    let mut packet = PacketWriter::named(START_RIDER_SCHOOL_REPLY_NAME);
    packet.write_u8(1);
    packet.write_bytes(physics.as_bytes());
    Ok(packet.into_inner())
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

/// Serializes the empty task-context response expected after rider startup.
#[must_use]
pub fn serialize_pr_get_rider_task_context() -> Vec<u8> {
    let mut packet = PacketWriter::named(GET_RIDER_TASK_CONTEXT_REPLY_NAME);
    packet.write_i32(0);
    packet.into_inner()
}

/// Serializes the empty/default versus-mode rank-one result.
#[must_use]
pub fn serialize_pr_versus_mode_rank_one() -> Vec<u8> {
    let mut packet = PacketWriter::named(VERSUS_MODE_RANK_ONE_REPLY_NAME);
    packet.write_u8(0);
    packet.write_bytes(&[u8::MAX; 8]);
    packet.into_inner()
}

/// Serializes the all-clear rider-school expiration result.
#[must_use]
pub fn serialize_pr_rider_school_expired_check() -> Vec<u8> {
    let mut packet = PacketWriter::named(RIDER_SCHOOL_EXPIRED_CHECK_REPLY_NAME);
    packet.write_bytes(&[0; 10]);
    packet.into_inner()
}

/// Serializes the profile-backed ranker summary used during startup.
#[must_use]
pub fn serialize_pr_ranker_info(ranker: u8) -> Vec<u8> {
    let mut packet = PacketWriter::named(RANKER_INFO_REPLY_NAME);
    packet.write_u8(0);
    packet.write_u8(ranker);
    packet.write_u32(100.0_f32.to_bits());
    packet.write_u32(0);
    packet.into_inner()
}

/// Serializes the terminal empty gift-sequence state used by a new profile.
#[must_use]
pub fn serialize_sp_rp_get_max_gift_id() -> Vec<u8> {
    let mut packet = PacketWriter::named(GET_MAX_GIFT_ID_REPLY_NAME);
    packet.write_i32(0);
    packet.into_inner()
}

/// Serializes the profile-backed KOIN balance and the stock zero suffix.
#[must_use]
pub fn serialize_sp_rp_koin_balance(koin: u32) -> Vec<u8> {
    let mut packet = PacketWriter::named(KOIN_BALANCE_REPLY_NAME);
    packet.write_u32(koin);
    packet.write_u32(0);
    packet.into_inner()
}

/// Serializes an empty favorite-track projection.
///
/// Rust does not yet persist favorite tracks, so it exposes an honest empty
/// theme list rather than reading the mutable C# sidecar without a lease.
#[must_use]
pub fn serialize_empty_pr_favorite_track_map_get() -> Vec<u8> {
    let mut packet = PacketWriter::named(FAVORITE_TRACK_MAP_REPLY_NAME);
    packet.write_i32(0);
    packet.into_inner()
}

/// Serializes the stock terminal empty cash-inventory projection.
#[must_use]
pub fn serialize_empty_sp_rp_get_cash_inventory() -> Vec<u8> {
    let mut packet = PacketWriter::named(GET_CASH_INVENTORY_REPLY_NAME);
    packet.write_i32(0);
    packet.write_u8(0);
    packet.into_inner()
}

/// Serializes the profile-backed Cash balance and its stock zero prefix.
#[must_use]
pub fn serialize_sp_rp_remain_cash(cash: u32) -> Vec<u8> {
    let mut packet = PacketWriter::named(REMAIN_CASH_REPLY_NAME);
    packet.write_u32(0);
    packet.write_u32(cash);
    packet.into_inner()
}

/// Serializes the profile-backed TC Cash balance.
///
/// Both the stock-era and current C# handlers emit `99` as the leading
/// protocol value. Its business meaning is unknown, so Rust preserves the
/// established wire constant without treating it as profile state.
#[must_use]
pub fn serialize_sp_rp_remain_tc_cash(tc_cash: u32) -> Vec<u8> {
    let mut packet = PacketWriter::named(REMAIN_TC_CASH_REPLY_NAME);
    packet.write_u32(99);
    packet.write_u32(tc_cash);
    packet.into_inner()
}

/// Serializes the legacy four-byte server clock representation.
#[must_use]
pub fn serialize_pr_server_time(time: LegacyTime) -> Vec<u8> {
    let mut packet = PacketWriter::named("PrServerTime");
    write_legacy_time(&mut packet, time);
    packet.into_inner()
}

/// Serializes the fail-closed stock extra-data reply: a zero code followed by
/// an absent optional-value marker.
///
/// The business meaning of code zero is not established, so no speculative
/// success code or optional value is exposed by this API.
#[must_use]
pub fn serialize_pr_request_extradata() -> Vec<u8> {
    let mut packet = PacketWriter::named(REQUEST_EXTRADATA_REPLY_NAME);
    packet.write_u8(0);
    packet.write_u8(0);
    packet.into_inner()
}

/// Serializes the empty stock web-event completion reply.
#[must_use]
pub fn serialize_pr_web_event_complete_check() -> Vec<u8> {
    PacketWriter::named(WEB_EVENT_COMPLETE_CHECK_REPLY_NAME).into_inner()
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
        FAVORITE_TRACK_MAP_REPLY_HASH, FAVORITE_TRACK_MAP_REPLY_NAME,
        FAVORITE_TRACK_MAP_REQUEST_HASH, FAVORITE_TRACK_MAP_REQUEST_NAME,
        GET_CASH_INVENTORY_REPLY_HASH, GET_CASH_INVENTORY_REPLY_NAME,
        GET_CASH_INVENTORY_REQUEST_HASH, GET_CASH_INVENTORY_REQUEST_NAME,
        GET_MAX_GIFT_ID_REPLY_HASH, GET_MAX_GIFT_ID_REPLY_NAME, GET_MAX_GIFT_ID_REQUEST_HASH,
        GET_MAX_GIFT_ID_REQUEST_NAME, GET_RIDER_TASK_CONTEXT_REPLY_HASH,
        GET_RIDER_TASK_CONTEXT_REPLY_NAME, GET_RIDER_TASK_CONTEXT_REQUEST_HASH,
        GET_RIDER_TASK_CONTEXT_REQUEST_NAME, GameOptions, KOIN_BALANCE_REPLY_HASH,
        KOIN_BALANCE_REPLY_NAME, KOIN_BALANCE_REQUEST_HASH, KOIN_BALANCE_REQUEST_NAME,
        LOCKED_ITEM_LIST_REPLY_NAME, LOCKED_ITEM_LIST_REQUEST_NAME, MAX_GAME_OPTION_TRAILING_BYTES,
        PrGetRiderFields, RANKER_INFO_REPLY_HASH, RANKER_INFO_REPLY_NAME, RANKER_INFO_REQUEST_HASH,
        RANKER_INFO_REQUEST_NAME, REMAIN_CASH_REPLY_HASH, REMAIN_CASH_REPLY_NAME,
        REMAIN_CASH_REQUEST_HASH, REMAIN_CASH_REQUEST_NAME, REMAIN_TC_CASH_REPLY_HASH,
        REMAIN_TC_CASH_REPLY_NAME, REMAIN_TC_CASH_REQUEST_HASH, REMAIN_TC_CASH_REQUEST_NAME,
        REQUEST_EXTRADATA_REPLY_NAME, REQUEST_EXTRADATA_REQUEST_NAME,
        RIDER_ITEM_SNAPSHOT_WIRE_LENGTH, RIDER_SCHOOL_EXPIRED_CHECK_REPLY_HASH,
        RIDER_SCHOOL_EXPIRED_CHECK_REPLY_NAME, RIDER_SCHOOL_EXPIRED_CHECK_REQUEST_HASH,
        RIDER_SCHOOL_EXPIRED_CHECK_REQUEST_NAME, START_RIDER_SCHOOL_REPLY_HASH,
        START_RIDER_SCHOOL_REPLY_NAME, START_RIDER_SCHOOL_REQUEST_HASH,
        START_RIDER_SCHOOL_REQUEST_NAME, StartupError, StartupRequest,
        VERSUS_MODE_RANK_ONE_REPLY_HASH, VERSUS_MODE_RANK_ONE_REPLY_NAME,
        VERSUS_MODE_RANK_ONE_REQUEST_HASH, VERSUS_MODE_RANK_ONE_REQUEST_NAME,
        WEB_EVENT_COMPLETE_CHECK_REPLY_NAME, WEB_EVENT_COMPLETE_CHECK_REQUEST_NAME,
        channel_static_reply_body, classify_startup_request, is_startup_noop,
        parse_pq_favorite_track_map_get, parse_pq_get_rider_task_context, parse_pq_locked_item_get,
        parse_pq_ranker_info, parse_pq_request_extradata, parse_pq_rider_school_expired_check,
        parse_pq_start_rider_school, parse_pq_update_game_option, parse_pq_versus_mode_rank_one,
        parse_pq_web_event_complete_check, parse_sp_rq_get_cash_inventory,
        parse_sp_rq_get_max_gift_id, parse_sp_rq_koin_balance, parse_sp_rq_remain_cash,
        parse_sp_rq_remain_tc_cash, serialize_channel_static_reply,
        serialize_empty_locked_item_list, serialize_empty_pr_favorite_track_map_get,
        serialize_empty_sp_rp_get_cash_inventory, serialize_lo_rp_add_racing_time,
        serialize_lo_rp_event_reward, serialize_pr_add_time_event_init, serialize_pr_chapter_info,
        serialize_pr_disassemble_fee_info, serialize_pr_dynamic_command,
        serialize_pr_equip_tuning_failure, serialize_pr_get_current_rider,
        serialize_pr_get_duel_mission_bulk, serialize_pr_get_favorite_channel,
        serialize_pr_get_game_option, serialize_pr_get_rider, serialize_pr_get_rider_task_context,
        serialize_pr_kart_pass_init, serialize_pr_kart_pass_reward, serialize_pr_login_vip_info,
        serialize_pr_public_command, serialize_pr_quest_ux_second, serialize_pr_ranker_info,
        serialize_pr_request_extradata, serialize_pr_rider_school_data,
        serialize_pr_rider_school_expired_check, serialize_pr_rider_school_progress,
        serialize_pr_server_time, serialize_pr_set_playtime_event_tick,
        serialize_pr_start_rider_school, serialize_pr_sync_dictionary_info,
        serialize_pr_versus_mode_rank_one, serialize_pr_web_event_complete_check,
        serialize_sp_rp_get_max_gift_id, serialize_sp_rp_koin_balance, serialize_sp_rp_remain_cash,
        serialize_sp_rp_remain_tc_cash,
    };
    use crate::{
        adler32, encoded,
        login::LegacyTime,
        packet::{PacketReader, PacketWriter},
        race_start_protocol::P5136_KART_PHYSICS_BLOCK_LENGTH,
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
    fn post_rider_startup_queries_preserve_exact_pairs_and_replies() {
        type HashOnlyParser = fn(&[u8]) -> Result<(), StartupError>;
        let startup_queries: &[(&str, u32, &str, u32, StartupRequest, HashOnlyParser)] = &[
            (
                GET_RIDER_TASK_CONTEXT_REQUEST_NAME,
                GET_RIDER_TASK_CONTEXT_REQUEST_HASH,
                GET_RIDER_TASK_CONTEXT_REPLY_NAME,
                GET_RIDER_TASK_CONTEXT_REPLY_HASH,
                StartupRequest::GetRiderTaskContext,
                parse_pq_get_rider_task_context,
            ),
            (
                VERSUS_MODE_RANK_ONE_REQUEST_NAME,
                VERSUS_MODE_RANK_ONE_REQUEST_HASH,
                VERSUS_MODE_RANK_ONE_REPLY_NAME,
                VERSUS_MODE_RANK_ONE_REPLY_HASH,
                StartupRequest::VersusModeRankOne,
                parse_pq_versus_mode_rank_one,
            ),
            (
                RIDER_SCHOOL_EXPIRED_CHECK_REQUEST_NAME,
                RIDER_SCHOOL_EXPIRED_CHECK_REQUEST_HASH,
                RIDER_SCHOOL_EXPIRED_CHECK_REPLY_NAME,
                RIDER_SCHOOL_EXPIRED_CHECK_REPLY_HASH,
                StartupRequest::RiderSchoolExpiredCheck,
                parse_pq_rider_school_expired_check,
            ),
            (
                RANKER_INFO_REQUEST_NAME,
                RANKER_INFO_REQUEST_HASH,
                RANKER_INFO_REPLY_NAME,
                RANKER_INFO_REPLY_HASH,
                StartupRequest::RankerInfo,
                parse_pq_ranker_info,
            ),
        ];
        for &(request_name, request_hash, reply_name, reply_hash, request, parser) in
            startup_queries
        {
            assert_eq!(adler32::packet_hash(request_name), request_hash);
            assert_eq!(adler32::packet_hash(reply_name), reply_hash);
            assert_eq!(classify_startup_request(request_hash), Some(request));
            assert_eq!(request.reply_name(), Some(reply_name));
            assert_strict_hash_only_parser(parser, request_name);
        }

        assert_eq!(
            serialize_pr_get_rider_task_context(),
            [0x50, 0x08, 0x84, 0x58, 0, 0, 0, 0]
        );
        assert_eq!(
            serialize_pr_versus_mode_rank_one(),
            [
                0xD5, 0x09, 0xDA, 0x7F, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF
            ]
        );
        assert_eq!(
            serialize_pr_rider_school_expired_check(),
            [0xCF, 0x09, 0xD9, 0x7E, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(
            serialize_pr_ranker_info(7),
            [0x09, 0x07, 0xD7, 0x41, 0, 7, 0, 0, 0xC8, 0x42, 0, 0, 0, 0]
        );
    }

    #[test]
    fn menu_store_startup_queries_preserve_exact_pairs_and_replies() {
        type HashOnlyParser = fn(&[u8]) -> Result<(), StartupError>;
        let queries: &[(&str, u32, &str, u32, StartupRequest, HashOnlyParser)] = &[
            (
                GET_MAX_GIFT_ID_REQUEST_NAME,
                GET_MAX_GIFT_ID_REQUEST_HASH,
                GET_MAX_GIFT_ID_REPLY_NAME,
                GET_MAX_GIFT_ID_REPLY_HASH,
                StartupRequest::GetMaxGiftId,
                parse_sp_rq_get_max_gift_id,
            ),
            (
                FAVORITE_TRACK_MAP_REQUEST_NAME,
                FAVORITE_TRACK_MAP_REQUEST_HASH,
                FAVORITE_TRACK_MAP_REPLY_NAME,
                FAVORITE_TRACK_MAP_REPLY_HASH,
                StartupRequest::FavoriteTrackMap,
                parse_pq_favorite_track_map_get,
            ),
            (
                GET_CASH_INVENTORY_REQUEST_NAME,
                GET_CASH_INVENTORY_REQUEST_HASH,
                GET_CASH_INVENTORY_REPLY_NAME,
                GET_CASH_INVENTORY_REPLY_HASH,
                StartupRequest::GetCashInventory,
                parse_sp_rq_get_cash_inventory,
            ),
            (
                REMAIN_CASH_REQUEST_NAME,
                REMAIN_CASH_REQUEST_HASH,
                REMAIN_CASH_REPLY_NAME,
                REMAIN_CASH_REPLY_HASH,
                StartupRequest::RemainCash,
                parse_sp_rq_remain_cash,
            ),
            (
                REMAIN_TC_CASH_REQUEST_NAME,
                REMAIN_TC_CASH_REQUEST_HASH,
                REMAIN_TC_CASH_REPLY_NAME,
                REMAIN_TC_CASH_REPLY_HASH,
                StartupRequest::RemainTcCash,
                parse_sp_rq_remain_tc_cash,
            ),
        ];
        for &(request_name, request_hash, reply_name, reply_hash, request, parser) in queries {
            assert_eq!(adler32::packet_hash(request_name), request_hash);
            assert_eq!(adler32::packet_hash(reply_name), reply_hash);
            assert_eq!(classify_startup_request(request_hash), Some(request));
            assert_eq!(request.reply_name(), Some(reply_name));
            assert_strict_hash_only_parser(parser, request_name);
        }

        assert_eq!(
            serialize_sp_rp_get_max_gift_id(),
            [0x5A, 0x08, 0xA1, 0x5E, 0, 0, 0, 0]
        );
        assert_eq!(
            serialize_sp_rp_koin_balance(0x1122_3344),
            [0xBC, 0x05, 0x40, 0x2D, 0x44, 0x33, 0x22, 0x11, 0, 0, 0, 0]
        );
        assert_eq!(
            serialize_empty_pr_favorite_track_map_get(),
            [0x35, 0x08, 0x52, 0x5A, 0, 0, 0, 0]
        );
        assert_eq!(
            serialize_empty_sp_rp_get_cash_inventory(),
            [0x4A, 0x0A, 0x5C, 0x87, 0, 0, 0, 0, 0]
        );
        assert_eq!(
            serialize_sp_rp_remain_cash(0x5566_7788),
            [0xB8, 0x07, 0xDB, 0x4F, 0, 0, 0, 0, 0x88, 0x77, 0x66, 0x55]
        );
        assert_eq!(
            serialize_sp_rp_remain_tc_cash(0x99AA_BBCC),
            [0x6F, 0x08, 0xCE, 0x5F, 99, 0, 0, 0, 0xCC, 0xBB, 0xAA, 0x99]
        );
    }

    #[test]
    fn koin_balance_uses_the_exact_captured_request_shape() {
        assert_eq!(
            adler32::packet_hash(KOIN_BALANCE_REQUEST_NAME),
            KOIN_BALANCE_REQUEST_HASH
        );
        assert_eq!(
            adler32::packet_hash(KOIN_BALANCE_REPLY_NAME),
            KOIN_BALANCE_REPLY_HASH
        );
        assert_eq!(
            classify_startup_request(KOIN_BALANCE_REQUEST_HASH),
            Some(StartupRequest::KoinBalance)
        );
        assert_eq!(
            StartupRequest::KoinBalance.reply_name(),
            Some(KOIN_BALANCE_REPLY_NAME)
        );

        let exact_request = [0xBD, 0x05, 0x4C, 0x2D, 1];
        assert!(parse_sp_rq_koin_balance(&exact_request).is_ok());
        for truncated_length in 0..exact_request.len() {
            assert!(parse_sp_rq_koin_balance(&exact_request[..truncated_length]).is_err());
        }
        let mut wrong_mode = exact_request;
        wrong_mode[4] = 0;
        assert!(matches!(
            parse_sp_rq_koin_balance(&wrong_mode),
            Err(StartupError::UnexpectedKoinBalanceMode { actual: 0 })
        ));
        let mut trailing_request = exact_request.to_vec();
        trailing_request.push(0);
        assert!(matches!(
            parse_sp_rq_koin_balance(&trailing_request),
            Err(StartupError::TrailingBytes {
                name: KOIN_BALANCE_REQUEST_NAME,
                count: 1,
            })
        ));
    }

    #[test]
    fn strict_stock_hash_only_requests_preserve_exact_pairs_and_replies() {
        assert_eq!(
            adler32::packet_hash(REQUEST_EXTRADATA_REQUEST_NAME),
            0x4466_0748
        );
        assert_eq!(
            adler32::packet_hash(REQUEST_EXTRADATA_REPLY_NAME),
            0x4477_0749
        );
        assert_eq!(
            adler32::packet_hash(WEB_EVENT_COMPLETE_CHECK_REQUEST_NAME),
            0xA814_0B50
        );
        assert_eq!(
            adler32::packet_hash(WEB_EVENT_COMPLETE_CHECK_REPLY_NAME),
            0xA830_0B51
        );

        assert_eq!(
            classify_startup_request(0x4466_0748),
            Some(StartupRequest::RequestExtradata)
        );
        assert_eq!(
            StartupRequest::RequestExtradata.reply_name(),
            Some(REQUEST_EXTRADATA_REPLY_NAME)
        );
        assert_eq!(
            classify_startup_request(0xA814_0B50),
            Some(StartupRequest::WebEventCompleteCheck)
        );
        assert_eq!(
            StartupRequest::WebEventCompleteCheck.reply_name(),
            Some(WEB_EVENT_COMPLETE_CHECK_REPLY_NAME)
        );
        assert!(!is_startup_noop(0x4466_0748));
        assert!(!is_startup_noop(0xA814_0B50));

        assert_strict_hash_only_parser(parse_pq_request_extradata, REQUEST_EXTRADATA_REQUEST_NAME);
        assert_strict_hash_only_parser(
            parse_pq_web_event_complete_check,
            WEB_EVENT_COMPLETE_CHECK_REQUEST_NAME,
        );

        assert_eq!(
            serialize_pr_request_extradata(),
            [0x49, 0x07, 0x77, 0x44, 0, 0]
        );
        assert_eq!(
            serialize_pr_web_event_complete_check(),
            [0x51, 0x0B, 0x30, 0xA8]
        );
    }

    #[test]
    fn rider_school_start_is_an_exact_five_byte_encoded_u8_request() {
        assert_eq!(
            adler32::packet_hash(START_RIDER_SCHOOL_REQUEST_NAME),
            START_RIDER_SCHOOL_REQUEST_HASH
        );
        assert_eq!(
            adler32::packet_hash(START_RIDER_SCHOOL_REPLY_NAME),
            START_RIDER_SCHOOL_REPLY_HASH
        );
        assert_eq!(
            classify_startup_request(START_RIDER_SCHOOL_REQUEST_HASH),
            Some(StartupRequest::StartRiderSchool)
        );
        assert_eq!(
            StartupRequest::StartRiderSchool.reply_name(),
            Some(START_RIDER_SCHOOL_REPLY_NAME)
        );

        let mut writer = PacketWriter::named(START_RIDER_SCHOOL_REQUEST_NAME);
        writer.write_encoded_u8(0xa5);
        let request = writer.into_inner();
        assert_eq!(request.len(), 5);
        assert_eq!(parse_pq_start_rider_school(&request).unwrap().value(), 0xa5);

        for truncated_length in 0..5 {
            assert!(matches!(
                parse_pq_start_rider_school(&request[..truncated_length]),
                Err(StartupError::Packet(_))
            ));
        }

        let mut wrong_hash = request.clone();
        wrong_hash[..4].copy_from_slice(&0_u32.to_le_bytes());
        assert!(matches!(
            parse_pq_start_rider_school(&wrong_hash),
            Err(StartupError::UnexpectedPacketHash {
                name: START_RIDER_SCHOOL_REQUEST_NAME,
                expected: START_RIDER_SCHOOL_REQUEST_HASH,
                actual: 0,
            })
        ));

        let mut trailing = request;
        trailing.push(0x51);
        assert!(matches!(
            parse_pq_start_rider_school(&trailing),
            Err(StartupError::TrailingBytes {
                name: START_RIDER_SCHOOL_REQUEST_NAME,
                count: 1,
            })
        ));

        // There is no evidence-backed business range. Every encoded byte is
        // accepted, and the substitution table spans the complete decoded u8
        // domain, including the value the legacy encoder cannot emit
        // canonically because of its modulo-255 behavior.
        let mut seen = [false; 256];
        for encoded_value in u8::MIN..=u8::MAX {
            let mut packet = START_RIDER_SCHOOL_REQUEST_HASH.to_le_bytes().to_vec();
            packet.push(encoded_value);
            let value = parse_pq_start_rider_school(&packet).unwrap().value();
            assert_eq!(value, encoded::decode_u8(encoded_value));
            seen[usize::from(value)] = true;
        }
        assert!(seen.iter().all(|value| *value));
    }

    #[test]
    fn rider_school_reply_pins_the_canonical_builder_not_csharp_shortcut_drift() {
        let packet = serialize_pr_start_rider_school().unwrap();
        assert_eq!(
            packet.len(),
            size_of::<u32>() + 1 + P5136_KART_PHYSICS_BLOCK_LENGTH
        );
        assert_eq!(
            u32::from_le_bytes(packet[..4].try_into().unwrap()),
            START_RIDER_SCHOOL_REPLY_HASH
        );
        assert_eq!(packet[4], 1);

        // The normal C# formula and canonical Rust builder produce these two
        // fields. Deliberately do not clone the compatibility shortcut's
        // isolated 2305.0 / 3745.0 hardcodes.
        let physics_start = 5;
        assert_eq!(
            encoded::decode_f32(
                packet[physics_start + 138..physics_start + 142]
                    .try_into()
                    .unwrap()
            )
            .to_bits(),
            2_304.0_f32.to_bits()
        );
        assert_eq!(
            encoded::decode_f32(
                packet[physics_start + 142..physics_start + 146]
                    .try_into()
                    .unwrap()
            )
            .to_bits(),
            0x456A_1968
        );
        assert_eq!(
            format!("{:X}", Sha256::digest(&packet)),
            "52F16BC897E349AD220B226F3563653CB02718A2A2827076249ECE194104AD9E"
        );
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

    fn assert_strict_hash_only_parser(
        parser: fn(&[u8]) -> Result<(), StartupError>,
        request_name: &'static str,
    ) {
        let request = PacketWriter::named(request_name).into_inner();
        assert_eq!(request.len(), 4);
        assert!(parser(&request).is_ok());

        for truncated_length in 0..4 {
            assert!(matches!(
                parser(&request[..truncated_length]),
                Err(StartupError::Packet(_))
            ));
        }

        match parser(&[0; 4]) {
            Err(StartupError::UnexpectedPacketHash {
                name,
                expected,
                actual,
            }) => {
                assert_eq!(name, request_name);
                assert_eq!(expected, adler32::packet_hash(request_name));
                assert_eq!(actual, 0);
            }
            other => panic!("wrong hash should be rejected, got {other:?}"),
        }

        let mut trailing = request;
        trailing.push(0x51);
        assert!(matches!(
            parser(&trailing),
            Err(StartupError::TrailingBytes { name, count: 1 }) if name == request_name
        ));
    }
}
