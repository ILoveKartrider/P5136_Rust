//! Bounded Korean P5136 TCP `GameSlotPacket` decoding.
//!
//! This is deliberately separate from [`crate::udp_protocol`]. The stock
//! client also uses the same packet name inside a UDP relay envelope, while
//! this module validates the named logical packet accepted by the login TCP
//! race handler.
//!
//! Parsing is side-effect free. An accepted packet owns its original wire
//! bytes so it can cross an actor boundary, and its [`GameSlotAction`] records
//! the remaining server policy. A [`GameSlotDisposition`] error is an
//! intentional drop; callers must still compare the claimed player ID with
//! their actor-owned frozen race identity before relaying anything.

use thiserror::Error;

pub const GAME_SLOT_PACKET_NAME: &str = "GameSlotPacket";
pub const GAME_SLOT_PACKET_HASH: u32 = 0x27C0_0574;
pub const MAX_GAME_SLOT_LOGICAL_LENGTH: usize = 1_013;
pub const MAX_GAME_SLOT_BLOB_LENGTH: usize = 0x3c0;
pub const MAX_GAME_SLOT_PLAYER_ID: u8 = 15;

pub const GOP_CUBE_HASH: u32 = 0x0A4F_02A5;
pub const GO_ITEM_CUBE_HASH: u32 = 0x1434_03C4;
pub const GAME_KART_ITEM_INFO_HASH: u32 = 0x5FC3_087F;

pub const GOP_BANANA_HASH: u32 = 0x1090_0367;
pub const GO_ITEM_BANANA_HASH: u32 = 0x1CB3_0486;
pub const GOP_COURSE_HASH: u32 = 0x1139_0397;
pub const GO_COURSE_HASH: u32 = 0x0D73_0327;
pub const GOP_ROCKET_HASH: u32 = 0x1129_038E;
pub const GO_ITEM_ROCKET_HASH: u32 = 0x1D4C_04AD;
pub const GOP_BARRICADE_HASH: u32 = 0x1D86_04A3;
pub const GO_ITEM_BARRICADE_HASH: u32 = 0x2D06_05C2;

const COMMON_ENVELOPE_LENGTH: usize = 13;
const P5136_PLAYER_MASK: u32 = 0x0000_ffff;
const PICKUP_BLOB_LENGTH: usize = 24;
const BANANA_OPERATION_LENGTH: usize = 30;
const COURSE_OPERATION_LENGTH: usize = 32;
const ROCKET_OPERATION_LENGTH: usize = 73;
const BARRICADE_OPERATION_LENGTH: usize = 73;

/// Exact operation/base-operation pairs derived from the checked-in P5136
/// packet-name table. `GopCube*` is intentionally excluded; `GopCourse` is
/// paired with the table's exceptional `GoCourse` spelling.
pub const P5136_ITEM_OPERATION_PAIRS: [(u32, u32); 74] = [
    (0x0D49_030D, 0x184D_042C), // GopAngel / GoItemAngel
    (0x1457_03C9, 0x2199_04E8), // GopAreaUfo / GoItemAreaUfo
    (0x14A6_03ED, 0x21E8_050C), // GopBalloon / GoItemBalloon
    (GOP_BANANA_HASH, GO_ITEM_BANANA_HASH),
    (GOP_BARRICADE_HASH, GO_ITEM_BARRICADE_HASH),
    (0x0D59_0311, 0x185D_0430), // GopBlock / GoItemBlock
    (0x233A_0538, 0x33D9_0657), // GopBossPrison / GoItemBossPrison
    (0x1DB9_04A4, 0x2D39_05C3), // GopBoundRoad / GoItemBoundRoad
    (0x1DC1_04AE, 0x2D41_05CD), // GopBoundWall / GoItemBoundWall
    (0x14E9_03F7, 0x222B_0516), // GopChopper / GoItemChopper
    (0x0D7B_031D, 0x187F_043C), // GopCloud / GoItemCloud
    (0x10CA_034F, 0x1CED_046E), // GopCloud2 / GoItemCloud2
    (0x1900_0448, 0x2761_0567), // GopCokebomb / GoItemCokebomb
    (0x2261_0510, 0x3300_062F), // GopCokeRocket / GoItemCokeRocket
    (GOP_COURSE_HASH, GO_COURSE_HASH),
    (0x0D69_031A, 0x186D_0439), // GopDevil / GoItemDevil
    (0x3A06_069F, 0x4F21_07BE), // GopDinoClawRocket / GoItemDinoClawRocket
    (0x0D6A_030E, 0x186E_042D), // GopDrmad / GoItemDrmad
    (0x1977_0461, 0x27D8_0580), // GopDynamite / GoItemDynamite
    (0x07AE_0248, 0x1074_0367), // GopEmp / GoItemEmp
    (0x2856_057F, 0x3A14_069E), // GopEventObject / GoItemEventObject
    (0x14A7_03E3, 0x21E9_0502), // GopFalling / GoItemFalling
    (0x1DC6_04B1, 0x2D46_05D0), // GopForceZone / GoItemForceZone
    (0x0DAE_0334, 0x18B2_0453), // GopFrost / GoItemFrost
    (0x0D8B_032B, 0x188F_044A), // GopGhost / GoItemGhost
    (0x228A_0514, 0x3329_0633), // GopGoldRocket / GoItemGoldRocket
    (0x2271_0505, 0x3310_0624), // GopGoldShield / GoItemGoldShield
    (0x10D3_0380, 0x1CF6_049F), // GopHammer / GoItemHammer
    (0x17FB_040D, 0x265C_052C), // GopHeadBand / GoItemHeadBand
    (0x10C3_0382, 0x1CE6_04A1), // GopIcefly / GoItemIcefly
    (0x2DC1_05C8, 0x409E_06E7), // GopInfectedBomb / GoItemInfectedBomb
    (0x0D82_031D, 0x1886_043C), // GopJewel / GoItemJewel
    (0x3BEA_06CF, 0x5105_07EE), // GopLockdownRocket / GoItemLockdownRocket
    (0x10DE_0382, 0x1D01_04A1), // GopMagnet / GoItemMagnet
    (0x0A6B_02AF, 0x1450_03CE), // GopMine / GoItemMine
    (0x1E52_04C0, 0x2DD2_05DF), // GopMovingUfo / GoItemMovingUfo
    (0x1476_03D8, 0x21B8_04F7), // GopMqDevil / GoItemMqDevil
    (0x18D8_0444, 0x2739_0563), // GopNewDevil / GoItemNewDevil
    (0x07C0_024A, 0x1086_0369), // GopOil / GoItemOil
    (0x2369_052B, 0x3408_064A), // GopPiratebomb / GoItemPiratebomb
    (0x0DC1_0333, 0x18C5_0452), // GopPress / GoItemPress
    (0x1DC5_04A1, 0x2D45_05C0), // GopRobotBeam / GoItemRobotBeam
    (GOP_ROCKET_HASH, GO_ITEM_ROCKET_HASH),
    (0x2954_059D, 0x3B12_06BC), // GopRollingbomb / GoItemRollingbomb
    (0x42E4_071F, 0x591E_083E), // GopRollingCokebomb / GoItemRollingCokebomb
    (0x6381_08BF, 0x7E37_09DE), // GopRollingInfectedbomb / GoItemRollingInfectedbomb
    (0x1942_0457, 0x27A3_0576), // GopScanning / GoItemScanning
    (0x1110_037F, 0x1D33_049E), // GopShield / GoItemShield
    (0x150D_03E9, 0x224F_0508), // GopSilence / GoItemSilence
    (0x0DB2_0327, 0x18B6_0446), // GopSiren / GoItemSiren
    (0x28A5_0580, 0x3A63_069F), // GopSirenShield / GoItemSirenShield
    (0x196B_0451, 0x27CC_0570), // GopSlotLock / GoItemSlotLock
    (0x19EB_046D, 0x284C_058C), // GopSnowbomb / GoItemSnowbomb
    (0x1584_0409, 0x22C6_0528), // GopSnowman / GoItemSnowman
    (0x2262_0502, 0x3301_0621), // GopSpaceCraft / GoItemSpaceCraft
    (0x3473_0640, 0x486F_075F), // GopSpecialShield / GoItemSpecialShield
    (0x2E54_05E8, 0x4131_0707), // GopSpecialSiren / GoItemSpecialSiren
    (0x2E3D_05E0, 0x411A_06FF), // GopSpecialSmall / GoItemSpecialSmall
    (0x1DB2_04AF, 0x2D32_05CE), // GopSpeedDown / GoItemSpeedDown
    (0x116F_0399, 0x1D92_04B8), // GopSpring / GoItemSpring
    (0x3C6F_06D4, 0x518A_07F3), // GopStraightRocket / GoItemStraightRocket
    (0x198F_044A, 0x27F0_0569), // GopSuperMag / GoItemSuperMag
    (0x2973_05B1, 0x3B31_06D0), // GopThunderbolt / GoItemThunderbolt
    (0x2882_0589, 0x3A40_06A8), // GopTigerRocket / GoItemTigerRocket
    (0x196A_0455, 0x27CB_0574), // GopTimebomb / GoItemTimebomb
    (0x2DDA_05D7, 0x40B7_06F6), // GopTimeCokebomb / GoItemTimeCokebomb
    (0x48D7_0757, 0x6030_0876), // GopTimeInfectedBomb / GoItemTimeInfectedBomb
    (0x1909_043E, 0x276A_055D), // GopTimeMine / GoItemTimeMine
    (0x2EC5_05FC, 0x41A2_071B), // GopTimeSnowbomb / GoItemTimeSnowbomb
    (0x1E29_04C1, 0x2DA9_05E0), // GopTombStone / GoItemTombStone
    (0x07CF_0250, 0x1095_036F), // GopUfo / GoItemUfo
    (0x1E65_04C9, 0x2DE5_05E8), // GopWaterbomb / GoItemWaterbomb
    (0x19AE_0474, 0x280F_0593), // GopWaterfly / GoItemWaterfly
    (0x1E04_04B2, 0x2D84_05D1), // GopWaterMine / GoItemWaterMine
];

/// The only actions implied by a successful P5136 decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameSlotAction {
    /// Type 1/2 must never be relayed as received. Until a separate
    /// authoritative award/synthesis path is implemented, this is an explicit
    /// no-relay/drop action.
    PickupRequiresServerSynthesis,
    /// Relay the exact owned input bytes to the validated audience.
    RelayOriginal(GameSlotRelayAudience),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameSlotRelayAudience {
    RoomIncludingSender,
    RoomExceptSender,
    RecipientMaskExceptSender,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemPickupKind {
    Type1,
    Type2,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ItemPickup {
    pub kind: ItemPickupKind,
    pub live_rank: i16,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub blob: GameSlotPayloadRange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemVector {
    items: [u32; 3],
    count: u8,
    pub payload: GameSlotPayloadRange,
}

impl ItemVector {
    #[must_use]
    pub fn items(&self) -> &[u32] {
        &self.items[..usize::from(self.count)]
    }

    #[must_use]
    pub const fn count(&self) -> u8 {
        self.count
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemUse {
    pub uni: u8,
    pub success: u8,
    pub unknown: u8,
    pub skill: i16,
    pub blob: GameSlotPayloadRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemReaction {
    pub uni: u8,
    pub skill: i16,
    pub blob: GameSlotPayloadRange,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ItemOperation {
    pub operation_hash: u32,
    pub operation_base_hash: u32,
    pub shape: ItemOperationShape,
    pub payload: GameSlotPayloadRange,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ItemOperationShape {
    Banana,
    Course,
    Rocket,
    Barricade(BarricadePlacement),
    GenericKnownPair,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BarricadePlacement {
    pub object_id: u32,
    pub tick: u32,
    pub owner_id: u8,
    /// Twelve finite little-endian floats at body offsets 25..73. The first
    /// three are the placement position.
    pub transform: [f32; 12],
}

impl BarricadePlacement {
    #[must_use]
    pub const fn x(self) -> f32 {
        self.transform[0]
    }

    #[must_use]
    pub const fn y(self) -> f32 {
        self.transform[1]
    }

    #[must_use]
    pub const fn z(self) -> f32 {
        self.transform[2]
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum GameSlotBody {
    ItemPickup(ItemPickup),
    ItemVector(ItemVector),
    ItemUse(ItemUse),
    ItemReaction(ItemReaction),
    ItemOperation(ItemOperation),
}

impl GameSlotBody {
    #[must_use]
    pub const fn packet_type(&self) -> u8 {
        match self {
            Self::ItemPickup(ItemPickup {
                kind: ItemPickupKind::Type1,
                ..
            }) => 1,
            Self::ItemPickup(ItemPickup {
                kind: ItemPickupKind::Type2,
                ..
            }) => 2,
            Self::ItemVector(_) => 9,
            Self::ItemUse(_) => 10,
            Self::ItemReaction(_) => 11,
            Self::ItemOperation(_) => 12,
        }
    }

    const fn payload_range(&self) -> GameSlotPayloadRange {
        match self {
            Self::ItemPickup(value) => value.blob,
            Self::ItemVector(value) => value.payload,
            Self::ItemUse(value) => value.blob,
            Self::ItemReaction(value) => value.blob,
            Self::ItemOperation(value) => value.payload,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameSlotPayloadRange {
    offset: usize,
    length: usize,
}

impl GameSlotPayloadRange {
    const fn new(offset: usize, length: usize) -> Self {
        Self { offset, length }
    }

    #[must_use]
    pub const fn offset(self) -> usize {
        self.offset
    }

    #[must_use]
    pub const fn len(self) -> usize {
        self.length
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.length == 0
    }
}

/// Fully validated, actor-transferable P5136 `GameSlotPacket`.
#[derive(Debug, PartialEq)]
pub struct ParsedGameSlotPacket {
    raw: Vec<u8>,
    player_id: u8,
    item_or_recipient_mask: u32,
    body: GameSlotBody,
    action: GameSlotAction,
}

impl ParsedGameSlotPacket {
    #[must_use]
    pub const fn player_id(&self) -> u8 {
        self.player_id
    }

    #[must_use]
    pub const fn item_or_recipient_mask(&self) -> u32 {
        self.item_or_recipient_mask
    }

    #[must_use]
    pub const fn body(&self) -> &GameSlotBody {
        &self.body
    }

    #[must_use]
    pub const fn action(&self) -> GameSlotAction {
        self.action
    }

    #[must_use]
    pub fn raw(&self) -> &[u8] {
        &self.raw
    }

    #[must_use]
    pub fn into_raw(self) -> Vec<u8> {
        self.raw
    }

    #[must_use]
    pub fn payload(&self) -> Option<&[u8]> {
        let payload = self.body.payload_range();
        let end = payload.offset.checked_add(payload.length)?;
        self.raw.get(payload.offset..end)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameSlotMaskRule {
    AllBitsSet,
    LowSixteenBits,
    NonzeroLowSixteenBits,
    NonzeroLowSixteenBitsIncludingSender,
}

/// Every error is a validated no-side-effect drop decision.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GameSlotDropReason {
    #[error("{context} is truncated: got {actual} bytes, need at least {minimum}")]
    Truncated {
        context: &'static str,
        actual: usize,
        minimum: usize,
    },

    #[error("GameSlotPacket has {actual} bytes; the P5136 logical maximum is {maximum}")]
    LogicalLengthOverCap { actual: usize, maximum: usize },

    #[error("expected GameSlotPacket hash 0x{expected:08X}, received 0x{actual:08X}")]
    UnexpectedPacketHash { expected: u32, actual: u32 },

    #[error("claimed GameSlot player ID {0} is outside 0..=15")]
    InvalidPlayerId(i32),

    #[error("GameSlot type {0} is not supported by Korean P5136")]
    UnsupportedType(u8),

    #[error("GameSlot type {packet_type} mask 0x{mask:08X} violates rule {rule:?}")]
    InvalidMask {
        packet_type: u8,
        mask: u32,
        rule: GameSlotMaskRule,
    },

    #[error("GameSlot type {packet_type} blob declares {declared} bytes; maximum is {maximum}")]
    BlobLengthOverCap {
        packet_type: u8,
        declared: u32,
        maximum: usize,
    },

    #[error(
        "GameSlot type {packet_type} blob declares {declared} bytes but exactly {actual} remain"
    )]
    BlobLengthMismatch {
        packet_type: u8,
        declared: u32,
        actual: usize,
    },

    #[error("item-pickup blob has {actual} bytes; expected exactly {expected}")]
    InvalidPickupBlobLength { actual: usize, expected: usize },

    #[error(
        "unsupported item-pickup operation pair 0x{operation_hash:08X}/0x{operation_base_hash:08X}"
    )]
    UnsupportedPickupOperation {
        operation_hash: u32,
        operation_base_hash: u32,
    },

    #[error("item-pickup coordinate {axis} is not finite")]
    NonFinitePickupPosition { axis: usize },

    #[error("item-vector payload hash 0x{actual:08X} is not GameKartItemInfo")]
    UnexpectedItemVectorHash { actual: u32 },

    #[error("item-vector count {count} exceeds the P5136 maximum of 3")]
    ItemVectorCountOverCap { count: u32 },

    #[error("item-vector count {count} requires {expected} payload bytes, declared {declared}")]
    InvalidItemVectorLength {
        count: u32,
        declared: u32,
        expected: usize,
    },

    #[error("unsupported item-operation pair 0x{operation_hash:08X}/0x{operation_base_hash:08X}")]
    UnsupportedItemOperation {
        operation_hash: u32,
        operation_base_hash: u32,
    },

    #[error(
        "known item-operation 0x{operation_hash:08X}/0x{operation_base_hash:08X} has {actual} bytes; expected {expected}"
    )]
    InvalidKnownItemOperationLength {
        operation_hash: u32,
        operation_base_hash: u32,
        actual: usize,
        expected: usize,
    },

    #[error("barricade operation marker is {actual}; expected 1")]
    InvalidBarricadeMarker { actual: u8 },

    #[error("barricade owner ID {owner_id} does not match claimed player ID {player_id}")]
    InvalidBarricadeOwner { player_id: u8, owner_id: i32 },

    #[error("barricade reserved field is 0x{actual:08X}; expected zero")]
    InvalidBarricadeReserved { actual: u32 },

    #[error("barricade transform float {index} is not finite")]
    NonFiniteBarricadeTransform { index: usize },
}

/// `Ok` means actor policy may continue; `Err` means drop without relay or
/// mutation.
pub type GameSlotDisposition = Result<ParsedGameSlotPacket, GameSlotDropReason>;

/// Parses one complete named Korean P5136 TCP `GameSlotPacket`.
///
/// This validates only the claimed ID's wire range. The world actor must bind
/// that claim to the authenticated session's frozen race identity.
pub fn parse_game_slot_packet(packet: &[u8]) -> GameSlotDisposition {
    if packet.len() < COMMON_ENVELOPE_LENGTH {
        return Err(GameSlotDropReason::Truncated {
            context: GAME_SLOT_PACKET_NAME,
            actual: packet.len(),
            minimum: COMMON_ENVELOPE_LENGTH,
        });
    }
    if packet.len() > MAX_GAME_SLOT_LOGICAL_LENGTH {
        return Err(GameSlotDropReason::LogicalLengthOverCap {
            actual: packet.len(),
            maximum: MAX_GAME_SLOT_LOGICAL_LENGTH,
        });
    }

    let actual_hash = read_u32(packet, 0, GAME_SLOT_PACKET_NAME)?;
    if actual_hash != GAME_SLOT_PACKET_HASH {
        return Err(GameSlotDropReason::UnexpectedPacketHash {
            expected: GAME_SLOT_PACKET_HASH,
            actual: actual_hash,
        });
    }

    let claimed_player_id = read_i32(packet, 4, GAME_SLOT_PACKET_NAME)?;
    let player_id = u8::try_from(claimed_player_id)
        .ok()
        .filter(|value| *value <= MAX_GAME_SLOT_PLAYER_ID)
        .ok_or(GameSlotDropReason::InvalidPlayerId(claimed_player_id))?;
    let item_or_recipient_mask = read_u32(packet, 8, GAME_SLOT_PACKET_NAME)?;
    let packet_type = packet[12];

    let (body, action) = match packet_type {
        1 | 2 => parse_item_pickup(packet, packet_type, item_or_recipient_mask)?,
        9 => parse_item_vector(packet, player_id, item_or_recipient_mask)?,
        10 => parse_item_use(packet, item_or_recipient_mask)?,
        11 => parse_item_reaction(packet, item_or_recipient_mask)?,
        12 => parse_item_operation(packet, player_id, item_or_recipient_mask)?,
        unsupported => return Err(GameSlotDropReason::UnsupportedType(unsupported)),
    };

    Ok(ParsedGameSlotPacket {
        raw: packet.to_vec(),
        player_id,
        item_or_recipient_mask,
        body,
        action,
    })
}

fn parse_item_pickup(
    packet: &[u8],
    packet_type: u8,
    mask: u32,
) -> Result<(GameSlotBody, GameSlotAction), GameSlotDropReason> {
    const BLOB_LENGTH_OFFSET: usize = 45;
    const BLOB_OFFSET: usize = 49;
    ensure_minimum(packet, "GameSlot item pickup", BLOB_OFFSET)?;
    validate_mask(packet_type, mask, 0, GameSlotMaskRule::AllBitsSet)?;

    let declared = read_u32(packet, BLOB_LENGTH_OFFSET, "GameSlot item pickup")?;
    let blob = validate_blob(packet, packet_type, BLOB_OFFSET, declared)?;
    if blob.length != PICKUP_BLOB_LENGTH {
        return Err(GameSlotDropReason::InvalidPickupBlobLength {
            actual: blob.length,
            expected: PICKUP_BLOB_LENGTH,
        });
    }

    let operation_hash = read_u32(packet, BLOB_OFFSET, "GameSlot item pickup operation")?;
    let operation_base_hash = read_u32(packet, BLOB_OFFSET + 4, "GameSlot item pickup operation")?;
    if (operation_hash, operation_base_hash) != (GOP_CUBE_HASH, GO_ITEM_CUBE_HASH) {
        return Err(GameSlotDropReason::UnsupportedPickupOperation {
            operation_hash,
            operation_base_hash,
        });
    }

    let position = [
        read_f32(packet, 26, "GameSlot item pickup position")?,
        read_f32(packet, 30, "GameSlot item pickup position")?,
        read_f32(packet, 34, "GameSlot item pickup position")?,
    ];
    for (axis, value) in position.into_iter().enumerate() {
        if !value.is_finite() {
            return Err(GameSlotDropReason::NonFinitePickupPosition { axis });
        }
    }

    let kind = if packet_type == 1 {
        ItemPickupKind::Type1
    } else {
        ItemPickupKind::Type2
    };
    let pickup = ItemPickup {
        kind,
        live_rank: read_i16(packet, 38, "GameSlot item pickup live rank")?,
        x: position[0],
        y: position[1],
        z: position[2],
        blob,
    };
    Ok((
        GameSlotBody::ItemPickup(pickup),
        GameSlotAction::PickupRequiresServerSynthesis,
    ))
}

fn parse_item_vector(
    packet: &[u8],
    player_id: u8,
    mask: u32,
) -> Result<(GameSlotBody, GameSlotAction), GameSlotDropReason> {
    const PAYLOAD_LENGTH_OFFSET: usize = 16;
    const PAYLOAD_OFFSET: usize = 20;
    const MINIMUM_LENGTH: usize = 28;
    ensure_minimum(packet, "GameSlot item vector", MINIMUM_LENGTH)?;
    validate_mask(
        9,
        mask,
        player_id,
        GameSlotMaskRule::NonzeroLowSixteenBitsIncludingSender,
    )?;

    let declared = read_u32(packet, PAYLOAD_LENGTH_OFFSET, "GameSlot item vector")?;
    let payload = validate_blob(packet, 9, PAYLOAD_OFFSET, declared)?;
    let payload_hash = read_u32(packet, PAYLOAD_OFFSET, "GameSlot item vector payload")?;
    if payload_hash != GAME_KART_ITEM_INFO_HASH {
        return Err(GameSlotDropReason::UnexpectedItemVectorHash {
            actual: payload_hash,
        });
    }
    let count = read_u32(packet, PAYLOAD_OFFSET + 4, "GameSlot item vector count")?;
    if count > 3 {
        return Err(GameSlotDropReason::ItemVectorCountOverCap { count });
    }
    let count_usize =
        usize::try_from(count).map_err(|_| GameSlotDropReason::ItemVectorCountOverCap { count })?;
    let expected = 8 + count_usize * 4;
    if payload.length != expected {
        return Err(GameSlotDropReason::InvalidItemVectorLength {
            count,
            declared,
            expected,
        });
    }

    let mut items = [0; 3];
    for (index, slot) in items.iter_mut().take(count_usize).enumerate() {
        *slot = read_u32(
            packet,
            PAYLOAD_OFFSET + 8 + index * 4,
            "GameSlot item vector item",
        )?;
    }
    let count =
        u8::try_from(count).map_err(|_| GameSlotDropReason::ItemVectorCountOverCap { count })?;
    Ok((
        GameSlotBody::ItemVector(ItemVector {
            items,
            count,
            payload,
        }),
        GameSlotAction::RelayOriginal(GameSlotRelayAudience::RoomExceptSender),
    ))
}

fn parse_item_use(
    packet: &[u8],
    mask: u32,
) -> Result<(GameSlotBody, GameSlotAction), GameSlotDropReason> {
    const BLOB_LENGTH_OFFSET: usize = 22;
    const BLOB_OFFSET: usize = 26;
    ensure_minimum(packet, "GameSlot item use", BLOB_OFFSET)?;
    validate_mask(10, mask, 0, GameSlotMaskRule::LowSixteenBits)?;
    let declared = read_u32(packet, BLOB_LENGTH_OFFSET, "GameSlot item use")?;
    let blob = validate_blob(packet, 10, BLOB_OFFSET, declared)?;

    Ok((
        GameSlotBody::ItemUse(ItemUse {
            uni: packet[13],
            success: packet[14],
            unknown: packet[15],
            skill: read_i16(packet, 16, "GameSlot item use skill")?,
            blob,
        }),
        GameSlotAction::RelayOriginal(GameSlotRelayAudience::RoomExceptSender),
    ))
}

fn parse_item_reaction(
    packet: &[u8],
    mask: u32,
) -> Result<(GameSlotBody, GameSlotAction), GameSlotDropReason> {
    const BLOB_LENGTH_OFFSET: usize = 19;
    const BLOB_OFFSET: usize = 23;
    ensure_minimum(packet, "GameSlot item reaction", BLOB_OFFSET)?;
    validate_mask(11, mask, 0, GameSlotMaskRule::NonzeroLowSixteenBits)?;
    let declared = read_u32(packet, BLOB_LENGTH_OFFSET, "GameSlot item reaction")?;
    let blob = validate_blob(packet, 11, BLOB_OFFSET, declared)?;

    Ok((
        GameSlotBody::ItemReaction(ItemReaction {
            uni: packet[13],
            skill: read_i16(packet, 14, "GameSlot item reaction skill")?,
            blob,
        }),
        GameSlotAction::RelayOriginal(GameSlotRelayAudience::RecipientMaskExceptSender),
    ))
}

fn parse_item_operation(
    packet: &[u8],
    player_id: u8,
    mask: u32,
) -> Result<(GameSlotBody, GameSlotAction), GameSlotDropReason> {
    const PAYLOAD_LENGTH_OFFSET: usize = 16;
    const PAYLOAD_OFFSET: usize = 20;
    const MINIMUM_LENGTH: usize = 28;
    ensure_minimum(packet, "GameSlot item operation", MINIMUM_LENGTH)?;
    validate_mask(12, mask, player_id, GameSlotMaskRule::NonzeroLowSixteenBits)?;
    let declared = read_u32(packet, PAYLOAD_LENGTH_OFFSET, "GameSlot item operation")?;
    let payload = validate_blob(packet, 12, PAYLOAD_OFFSET, declared)?;
    let operation_hash = read_u32(packet, PAYLOAD_OFFSET, "GameSlot item operation hash")?;
    let operation_base_hash = read_u32(
        packet,
        PAYLOAD_OFFSET + 4,
        "GameSlot item base-operation hash",
    )?;
    let pair = (operation_hash, operation_base_hash);

    let (shape, audience) = match exact_operation_length(pair) {
        Some(expected) if payload.length != expected => {
            return Err(GameSlotDropReason::InvalidKnownItemOperationLength {
                operation_hash,
                operation_base_hash,
                actual: payload.length,
                expected,
            });
        }
        Some(BANANA_OPERATION_LENGTH) if pair == (GOP_BANANA_HASH, GO_ITEM_BANANA_HASH) => (
            ItemOperationShape::Banana,
            GameSlotRelayAudience::RoomExceptSender,
        ),
        Some(COURSE_OPERATION_LENGTH) if pair == (GOP_COURSE_HASH, GO_COURSE_HASH) => (
            ItemOperationShape::Course,
            GameSlotRelayAudience::RoomExceptSender,
        ),
        Some(ROCKET_OPERATION_LENGTH) if pair == (GOP_ROCKET_HASH, GO_ITEM_ROCKET_HASH) => (
            ItemOperationShape::Rocket,
            GameSlotRelayAudience::RoomExceptSender,
        ),
        Some(BARRICADE_OPERATION_LENGTH)
            if pair == (GOP_BARRICADE_HASH, GO_ITEM_BARRICADE_HASH) =>
        {
            (
                ItemOperationShape::Barricade(parse_barricade(packet, player_id)?),
                GameSlotRelayAudience::RoomIncludingSender,
            )
        }
        None if P5136_ITEM_OPERATION_PAIRS.contains(&pair) => (
            ItemOperationShape::GenericKnownPair,
            GameSlotRelayAudience::RoomExceptSender,
        ),
        Some(_) | None => {
            return Err(GameSlotDropReason::UnsupportedItemOperation {
                operation_hash,
                operation_base_hash,
            });
        }
    };

    Ok((
        GameSlotBody::ItemOperation(ItemOperation {
            operation_hash,
            operation_base_hash,
            shape,
            payload,
        }),
        GameSlotAction::RelayOriginal(audience),
    ))
}

fn parse_barricade(packet: &[u8], player_id: u8) -> Result<BarricadePlacement, GameSlotDropReason> {
    let marker = packet[32];
    if marker != 1 {
        return Err(GameSlotDropReason::InvalidBarricadeMarker { actual: marker });
    }
    let owner_id = read_i32(packet, 37, "GameSlot barricade owner")?;
    if owner_id != i32::from(player_id) {
        return Err(GameSlotDropReason::InvalidBarricadeOwner {
            player_id,
            owner_id,
        });
    }
    let reserved = read_u32(packet, 41, "GameSlot barricade reserved field")?;
    if reserved != 0 {
        return Err(GameSlotDropReason::InvalidBarricadeReserved { actual: reserved });
    }

    let mut transform = [0.0; 12];
    for (index, value) in transform.iter_mut().enumerate() {
        *value = read_f32(packet, 45 + index * 4, "GameSlot barricade transform")?;
        if !value.is_finite() {
            return Err(GameSlotDropReason::NonFiniteBarricadeTransform { index });
        }
    }

    Ok(BarricadePlacement {
        object_id: read_u32(packet, 28, "GameSlot barricade object ID")?,
        tick: read_u32(packet, 33, "GameSlot barricade tick")?,
        owner_id: player_id,
        transform,
    })
}

const fn exact_operation_length(pair: (u32, u32)) -> Option<usize> {
    match pair {
        (GOP_BANANA_HASH, GO_ITEM_BANANA_HASH) => Some(BANANA_OPERATION_LENGTH),
        (GOP_COURSE_HASH, GO_COURSE_HASH) => Some(COURSE_OPERATION_LENGTH),
        (GOP_ROCKET_HASH, GO_ITEM_ROCKET_HASH) => Some(ROCKET_OPERATION_LENGTH),
        (GOP_BARRICADE_HASH, GO_ITEM_BARRICADE_HASH) => Some(BARRICADE_OPERATION_LENGTH),
        _ => None,
    }
}

fn validate_mask(
    packet_type: u8,
    mask: u32,
    player_id: u8,
    rule: GameSlotMaskRule,
) -> Result<(), GameSlotDropReason> {
    let low_only = mask & !P5136_PLAYER_MASK == 0;
    let valid = match rule {
        GameSlotMaskRule::AllBitsSet => mask == u32::MAX,
        GameSlotMaskRule::LowSixteenBits => low_only,
        GameSlotMaskRule::NonzeroLowSixteenBits => low_only && mask != 0,
        GameSlotMaskRule::NonzeroLowSixteenBitsIncludingSender => {
            low_only && mask != 0 && mask & (1_u32 << player_id) != 0
        }
    };
    if valid {
        Ok(())
    } else {
        Err(GameSlotDropReason::InvalidMask {
            packet_type,
            mask,
            rule,
        })
    }
}

fn validate_blob(
    packet: &[u8],
    packet_type: u8,
    offset: usize,
    declared: u32,
) -> Result<GameSlotPayloadRange, GameSlotDropReason> {
    if declared > u32::try_from(MAX_GAME_SLOT_BLOB_LENGTH).unwrap_or(u32::MAX) {
        return Err(GameSlotDropReason::BlobLengthOverCap {
            packet_type,
            declared,
            maximum: MAX_GAME_SLOT_BLOB_LENGTH,
        });
    }
    let actual = packet
        .len()
        .checked_sub(offset)
        .ok_or(GameSlotDropReason::Truncated {
            context: "GameSlot blob",
            actual: packet.len(),
            minimum: offset,
        })?;
    let length = usize::try_from(declared).map_err(|_| GameSlotDropReason::BlobLengthOverCap {
        packet_type,
        declared,
        maximum: MAX_GAME_SLOT_BLOB_LENGTH,
    })?;
    if length != actual {
        return Err(GameSlotDropReason::BlobLengthMismatch {
            packet_type,
            declared,
            actual,
        });
    }
    Ok(GameSlotPayloadRange::new(offset, length))
}

fn ensure_minimum(
    packet: &[u8],
    context: &'static str,
    minimum: usize,
) -> Result<(), GameSlotDropReason> {
    if packet.len() < minimum {
        Err(GameSlotDropReason::Truncated {
            context,
            actual: packet.len(),
            minimum,
        })
    } else {
        Ok(())
    }
}

fn read_i16(
    packet: &[u8],
    offset: usize,
    context: &'static str,
) -> Result<i16, GameSlotDropReason> {
    Ok(i16::from_le_bytes(read_array(packet, offset, context)?))
}

fn read_i32(
    packet: &[u8],
    offset: usize,
    context: &'static str,
) -> Result<i32, GameSlotDropReason> {
    Ok(i32::from_le_bytes(read_array(packet, offset, context)?))
}

fn read_u32(
    packet: &[u8],
    offset: usize,
    context: &'static str,
) -> Result<u32, GameSlotDropReason> {
    Ok(u32::from_le_bytes(read_array(packet, offset, context)?))
}

fn read_f32(
    packet: &[u8],
    offset: usize,
    context: &'static str,
) -> Result<f32, GameSlotDropReason> {
    Ok(f32::from_le_bytes(read_array(packet, offset, context)?))
}

fn read_array<const LENGTH: usize>(
    packet: &[u8],
    offset: usize,
    context: &'static str,
) -> Result<[u8; LENGTH], GameSlotDropReason> {
    let minimum = offset
        .checked_add(LENGTH)
        .ok_or(GameSlotDropReason::Truncated {
            context,
            actual: packet.len(),
            minimum: usize::MAX,
        })?;
    let bytes = packet
        .get(offset..minimum)
        .ok_or(GameSlotDropReason::Truncated {
            context,
            actual: packet.len(),
            minimum,
        })?;
    <[u8; LENGTH]>::try_from(bytes).map_err(|_| GameSlotDropReason::Truncated {
        context,
        actual: packet.len(),
        minimum,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{
        GAME_KART_ITEM_INFO_HASH, GAME_SLOT_PACKET_HASH, GAME_SLOT_PACKET_NAME,
        GO_ITEM_BANANA_HASH, GO_ITEM_BARRICADE_HASH, GO_ITEM_CUBE_HASH, GO_ITEM_ROCKET_HASH,
        GOP_BANANA_HASH, GOP_BARRICADE_HASH, GOP_CUBE_HASH, GOP_ROCKET_HASH, GameSlotAction,
        GameSlotBody, GameSlotDropReason, GameSlotMaskRule, GameSlotRelayAudience,
        ItemOperationShape, ItemPickupKind, MAX_GAME_SLOT_BLOB_LENGTH,
        MAX_GAME_SLOT_LOGICAL_LENGTH, P5136_ITEM_OPERATION_PAIRS, parse_game_slot_packet,
    };
    use crate::{adler32, packet::PacketWriter};

    const PLAYER_ID: i32 = 3;
    const PLAYER_MASK: u32 = 1 << PLAYER_ID;
    const WATERBOMB_PAIR: (u32, u32) = (0x1E65_04C9, 0x2DE5_05E8);
    const WATERFLY_BASE_HASH: u32 = 0x280F_0593;
    const PAIR_NAMES: [(&str, &str); 74] = [
        ("GopAngel", "GoItemAngel"),
        ("GopAreaUfo", "GoItemAreaUfo"),
        ("GopBalloon", "GoItemBalloon"),
        ("GopBanana", "GoItemBanana"),
        ("GopBarricade", "GoItemBarricade"),
        ("GopBlock", "GoItemBlock"),
        ("GopBossPrison", "GoItemBossPrison"),
        ("GopBoundRoad", "GoItemBoundRoad"),
        ("GopBoundWall", "GoItemBoundWall"),
        ("GopChopper", "GoItemChopper"),
        ("GopCloud", "GoItemCloud"),
        ("GopCloud2", "GoItemCloud2"),
        ("GopCokebomb", "GoItemCokebomb"),
        ("GopCokeRocket", "GoItemCokeRocket"),
        ("GopCourse", "GoCourse"),
        ("GopDevil", "GoItemDevil"),
        ("GopDinoClawRocket", "GoItemDinoClawRocket"),
        ("GopDrmad", "GoItemDrmad"),
        ("GopDynamite", "GoItemDynamite"),
        ("GopEmp", "GoItemEmp"),
        ("GopEventObject", "GoItemEventObject"),
        ("GopFalling", "GoItemFalling"),
        ("GopForceZone", "GoItemForceZone"),
        ("GopFrost", "GoItemFrost"),
        ("GopGhost", "GoItemGhost"),
        ("GopGoldRocket", "GoItemGoldRocket"),
        ("GopGoldShield", "GoItemGoldShield"),
        ("GopHammer", "GoItemHammer"),
        ("GopHeadBand", "GoItemHeadBand"),
        ("GopIcefly", "GoItemIcefly"),
        ("GopInfectedBomb", "GoItemInfectedBomb"),
        ("GopJewel", "GoItemJewel"),
        ("GopLockdownRocket", "GoItemLockdownRocket"),
        ("GopMagnet", "GoItemMagnet"),
        ("GopMine", "GoItemMine"),
        ("GopMovingUfo", "GoItemMovingUfo"),
        ("GopMqDevil", "GoItemMqDevil"),
        ("GopNewDevil", "GoItemNewDevil"),
        ("GopOil", "GoItemOil"),
        ("GopPiratebomb", "GoItemPiratebomb"),
        ("GopPress", "GoItemPress"),
        ("GopRobotBeam", "GoItemRobotBeam"),
        ("GopRocket", "GoItemRocket"),
        ("GopRollingbomb", "GoItemRollingbomb"),
        ("GopRollingCokebomb", "GoItemRollingCokebomb"),
        ("GopRollingInfectedbomb", "GoItemRollingInfectedbomb"),
        ("GopScanning", "GoItemScanning"),
        ("GopShield", "GoItemShield"),
        ("GopSilence", "GoItemSilence"),
        ("GopSiren", "GoItemSiren"),
        ("GopSirenShield", "GoItemSirenShield"),
        ("GopSlotLock", "GoItemSlotLock"),
        ("GopSnowbomb", "GoItemSnowbomb"),
        ("GopSnowman", "GoItemSnowman"),
        ("GopSpaceCraft", "GoItemSpaceCraft"),
        ("GopSpecialShield", "GoItemSpecialShield"),
        ("GopSpecialSiren", "GoItemSpecialSiren"),
        ("GopSpecialSmall", "GoItemSpecialSmall"),
        ("GopSpeedDown", "GoItemSpeedDown"),
        ("GopSpring", "GoItemSpring"),
        ("GopStraightRocket", "GoItemStraightRocket"),
        ("GopSuperMag", "GoItemSuperMag"),
        ("GopThunderbolt", "GoItemThunderbolt"),
        ("GopTigerRocket", "GoItemTigerRocket"),
        ("GopTimebomb", "GoItemTimebomb"),
        ("GopTimeCokebomb", "GoItemTimeCokebomb"),
        ("GopTimeInfectedBomb", "GoItemTimeInfectedBomb"),
        ("GopTimeMine", "GoItemTimeMine"),
        ("GopTimeSnowbomb", "GoItemTimeSnowbomb"),
        ("GopTombStone", "GoItemTombStone"),
        ("GopUfo", "GoItemUfo"),
        ("GopWaterbomb", "GoItemWaterbomb"),
        ("GopWaterfly", "GoItemWaterfly"),
        ("GopWaterMine", "GoItemWaterMine"),
    ];

    #[test]
    fn packet_hash_and_operation_allowlist_match_the_audited_table() {
        assert_eq!(
            adler32::packet_hash(GAME_SLOT_PACKET_NAME),
            GAME_SLOT_PACKET_HASH
        );
        assert_eq!(P5136_ITEM_OPERATION_PAIRS.len(), 74);
        assert_eq!(
            P5136_ITEM_OPERATION_PAIRS
                .iter()
                .copied()
                .collect::<HashSet<_>>()
                .len(),
            74
        );
        assert!(P5136_ITEM_OPERATION_PAIRS.contains(&WATERBOMB_PAIR));
        assert!(
            P5136_ITEM_OPERATION_PAIRS
                .iter()
                .all(|pair| pair.0 != GOP_CUBE_HASH)
        );
        for ((operation_hash, base_hash), (operation_name, base_name)) in
            P5136_ITEM_OPERATION_PAIRS.into_iter().zip(PAIR_NAMES)
        {
            assert_eq!(adler32::packet_hash(operation_name), operation_hash);
            assert_eq!(adler32::packet_hash(base_name), base_hash);
        }
    }

    #[test]
    fn both_pickup_types_are_deferred_without_raw_relay() {
        for (packet_type, kind) in [(1, ItemPickupKind::Type1), (2, ItemPickupKind::Type2)] {
            let mut wire = pickup_packet(packet_type, [12.5, -3.25, 0.0], -2);
            let parsed = parse_game_slot_packet(&wire).unwrap();
            assert_eq!(parsed.player_id(), 3);
            assert_eq!(parsed.item_or_recipient_mask(), u32::MAX);
            assert_eq!(
                parsed.action(),
                GameSlotAction::PickupRequiresServerSynthesis
            );
            assert_eq!(parsed.body().packet_type(), packet_type);
            let GameSlotBody::ItemPickup(pickup) = parsed.body() else {
                panic!("expected item pickup");
            };
            assert_eq!(pickup.kind, kind);
            assert_eq!(pickup.live_rank, -2);
            assert_eq!(pickup.x.to_bits(), 12.5_f32.to_bits());
            assert_eq!(pickup.y.to_bits(), (-3.25_f32).to_bits());
            assert_eq!(pickup.z.to_bits(), 0.0_f32.to_bits());
            assert_eq!(pickup.blob.offset(), 49);
            assert_eq!(pickup.blob.len(), 24);
            assert_eq!(
                parsed.payload().unwrap()[..8],
                [GOP_CUBE_HASH.to_le_bytes(), GO_ITEM_CUBE_HASH.to_le_bytes()].concat()
            );

            let original_hash = parsed.raw()[..4].to_vec();
            wire[..4].fill(0);
            assert_eq!(parsed.raw()[..4], original_hash);
            assert_eq!(parsed.into_raw().len(), 73);
        }
    }

    #[test]
    fn common_envelope_limits_hash_ids_and_types_are_strict() {
        assert!(matches!(
            parse_game_slot_packet(&[0; 12]),
            Err(GameSlotDropReason::Truncated {
                context: GAME_SLOT_PACKET_NAME,
                actual: 12,
                minimum: 13,
            })
        ));

        let mut over_cap = vec![0; MAX_GAME_SLOT_LOGICAL_LENGTH + 1];
        over_cap[..4].copy_from_slice(&GAME_SLOT_PACKET_HASH.to_le_bytes());
        assert!(matches!(
            parse_game_slot_packet(&over_cap),
            Err(GameSlotDropReason::LogicalLengthOverCap {
                actual,
                maximum: MAX_GAME_SLOT_LOGICAL_LENGTH,
            }) if actual == MAX_GAME_SLOT_LOGICAL_LENGTH + 1
        ));

        let mut maximum = vec![0; MAX_GAME_SLOT_LOGICAL_LENGTH];
        maximum[..4].copy_from_slice(&GAME_SLOT_PACKET_HASH.to_le_bytes());
        maximum[4..8].copy_from_slice(&PLAYER_ID.to_le_bytes());
        maximum[12] = 99;
        assert!(matches!(
            parse_game_slot_packet(&maximum),
            Err(GameSlotDropReason::UnsupportedType(99))
        ));

        let mut wrong_hash = common_packet(PLAYER_ID, PLAYER_MASK, 9).into_inner();
        wrong_hash.resize(28, 0);
        wrong_hash[..4].fill(0);
        assert!(matches!(
            parse_game_slot_packet(&wrong_hash),
            Err(GameSlotDropReason::UnexpectedPacketHash { .. })
        ));

        for player_id in [-1, 16, i32::MAX] {
            let packet = common_packet(player_id, PLAYER_MASK, 99).into_inner();
            assert!(matches!(
                parse_game_slot_packet(&packet),
                Err(GameSlotDropReason::InvalidPlayerId(actual)) if actual == player_id
            ));
        }
        for packet_type in [0, 3, 4, 5, 6, 7, 8, 13, 17, u8::MAX] {
            let packet = common_packet(PLAYER_ID, PLAYER_MASK, packet_type).into_inner();
            assert!(matches!(
                parse_game_slot_packet(&packet),
                Err(GameSlotDropReason::UnsupportedType(actual)) if actual == packet_type
            ));
        }
    }

    #[test]
    fn pickup_rejects_masks_lengths_operations_and_non_finite_positions() {
        let mut wrong_mask = pickup_packet(1, [0.0; 3], 0);
        set_u32(&mut wrong_mask, 8, PLAYER_MASK);
        assert!(matches!(
            parse_game_slot_packet(&wrong_mask),
            Err(GameSlotDropReason::InvalidMask {
                packet_type: 1,
                rule: GameSlotMaskRule::AllBitsSet,
                ..
            })
        ));

        let mut trailing = pickup_packet(1, [0.0; 3], 0);
        trailing.push(0);
        assert!(matches!(
            parse_game_slot_packet(&trailing),
            Err(GameSlotDropReason::BlobLengthMismatch {
                packet_type: 1,
                declared: 24,
                actual: 25,
            })
        ));

        let mut wrong_length = pickup_packet(1, [0.0; 3], 0);
        wrong_length.truncate(57);
        set_u32(&mut wrong_length, 45, 8);
        assert!(matches!(
            parse_game_slot_packet(&wrong_length),
            Err(GameSlotDropReason::InvalidPickupBlobLength {
                actual: 8,
                expected: 24,
            })
        ));

        let mut wrong_pair = pickup_packet(2, [0.0; 3], 0);
        set_u32(&mut wrong_pair, 49, GOP_BANANA_HASH);
        assert!(matches!(
            parse_game_slot_packet(&wrong_pair),
            Err(GameSlotDropReason::UnsupportedPickupOperation { .. })
        ));

        for non_finite in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            for axis in 0..3 {
                let mut position = [0.0; 3];
                position[axis] = non_finite;
                assert!(matches!(
                    parse_game_slot_packet(&pickup_packet(1, position, 0)),
                    Err(GameSlotDropReason::NonFinitePickupPosition { axis: actual })
                        if actual == axis
                ));
            }
        }
    }

    #[test]
    fn item_vector_has_a_bounded_typed_item_list_and_peer_relay() {
        for items in [vec![], vec![7], vec![7, 11], vec![7, 11, 13]] {
            let wire = item_vector_packet(PLAYER_MASK | 1, &items);
            let parsed = parse_game_slot_packet(&wire).unwrap();
            assert_eq!(
                parsed.action(),
                GameSlotAction::RelayOriginal(GameSlotRelayAudience::RoomExceptSender)
            );
            let GameSlotBody::ItemVector(vector) = parsed.body() else {
                panic!("expected item vector");
            };
            assert_eq!(usize::from(vector.count()), items.len());
            assert_eq!(vector.items(), items);
            assert_eq!(parsed.payload().unwrap().len(), 8 + items.len() * 4);
        }
    }

    #[test]
    fn item_vector_rejects_invalid_masks_hashes_counts_and_exact_length_drift() {
        for mask in [0, 1, 1 << 20] {
            let wire = item_vector_packet(mask, &[7]);
            assert!(matches!(
                parse_game_slot_packet(&wire),
                Err(GameSlotDropReason::InvalidMask {
                    packet_type: 9,
                    rule: GameSlotMaskRule::NonzeroLowSixteenBitsIncludingSender,
                    ..
                })
            ));
        }

        let mut wrong_hash = item_vector_packet(PLAYER_MASK, &[7]);
        set_u32(&mut wrong_hash, 20, 0);
        assert!(matches!(
            parse_game_slot_packet(&wrong_hash),
            Err(GameSlotDropReason::UnexpectedItemVectorHash { actual: 0 })
        ));

        let mut four_items = item_vector_packet(PLAYER_MASK, &[1, 2, 3]);
        set_u32(&mut four_items, 24, 4);
        assert!(matches!(
            parse_game_slot_packet(&four_items),
            Err(GameSlotDropReason::ItemVectorCountOverCap { count: 4 })
        ));

        let mut bad_count_length = item_vector_packet(PLAYER_MASK, &[1]);
        set_u32(&mut bad_count_length, 24, 2);
        assert!(matches!(
            parse_game_slot_packet(&bad_count_length),
            Err(GameSlotDropReason::InvalidItemVectorLength {
                count: 2,
                declared: 12,
                expected: 16,
            })
        ));
    }

    #[test]
    fn item_use_and_reaction_expose_fields_and_distinct_relay_audiences() {
        let use_blob = [0x10, 0x20, 0x30];
        let item_use =
            parse_game_slot_packet(&item_use_packet(0, 7, 2, 9, -123, &use_blob)).unwrap();
        let GameSlotBody::ItemUse(fields) = item_use.body() else {
            panic!("expected item use");
        };
        assert_eq!((fields.uni, fields.success, fields.unknown), (7, 2, 9));
        assert_eq!(fields.skill, -123);
        assert_eq!(item_use.payload(), Some(use_blob.as_slice()));
        assert_eq!(
            item_use.action(),
            GameSlotAction::RelayOriginal(GameSlotRelayAudience::RoomExceptSender)
        );

        let reaction_blob = [0xAA, 0x55];
        let reaction =
            parse_game_slot_packet(&item_reaction_packet(PLAYER_MASK | 1, 4, 9, &reaction_blob))
                .unwrap();
        let GameSlotBody::ItemReaction(fields) = reaction.body() else {
            panic!("expected item reaction");
        };
        assert_eq!(fields.uni, 4);
        assert_eq!(fields.skill, 9);
        assert_eq!(reaction.payload(), Some(reaction_blob.as_slice()));
        assert_eq!(
            reaction.action(),
            GameSlotAction::RelayOriginal(GameSlotRelayAudience::RecipientMaskExceptSender)
        );
    }

    #[test]
    fn use_and_reaction_masks_blob_caps_and_consumption_are_enforced() {
        assert!(parse_game_slot_packet(&item_use_packet(0, 0, 0, 0, 0, &[])).is_ok());
        assert!(matches!(
            parse_game_slot_packet(&item_use_packet(1 << 16, 0, 0, 0, 0, &[])),
            Err(GameSlotDropReason::InvalidMask {
                packet_type: 10,
                rule: GameSlotMaskRule::LowSixteenBits,
                ..
            })
        ));
        for mask in [0, 1 << 16] {
            assert!(matches!(
                parse_game_slot_packet(&item_reaction_packet(mask, 0, 0, &[])),
                Err(GameSlotDropReason::InvalidMask {
                    packet_type: 11,
                    rule: GameSlotMaskRule::NonzeroLowSixteenBits,
                    ..
                })
            ));
        }

        let maximum_blob = vec![0x5a; MAX_GAME_SLOT_BLOB_LENGTH];
        assert!(
            parse_game_slot_packet(&item_use_packet(PLAYER_MASK, 0, 0, 0, 0, &maximum_blob,))
                .is_ok()
        );
        let oversized_blob = vec![0; MAX_GAME_SLOT_BLOB_LENGTH + 1];
        assert!(matches!(
            parse_game_slot_packet(&item_use_packet(PLAYER_MASK, 0, 0, 0, 0, &oversized_blob,)),
            Err(GameSlotDropReason::BlobLengthOverCap {
                packet_type: 10,
                declared: 961,
                maximum: MAX_GAME_SLOT_BLOB_LENGTH,
            })
        ));

        let mut trailing = item_reaction_packet(PLAYER_MASK, 0, 0, &[1]);
        trailing.push(2);
        assert!(matches!(
            parse_game_slot_packet(&trailing),
            Err(GameSlotDropReason::BlobLengthMismatch {
                packet_type: 11,
                declared: 1,
                actual: 2,
            })
        ));
    }

    #[test]
    fn type_twelve_accepts_only_allowlisted_pairs_and_four_strict_shapes() {
        let generic =
            parse_game_slot_packet(&operation_packet(PLAYER_MASK, WATERBOMB_PAIR, 8)).unwrap();
        let GameSlotBody::ItemOperation(operation) = generic.body() else {
            panic!("expected item operation");
        };
        assert_eq!(operation.shape, ItemOperationShape::GenericKnownPair);
        assert_eq!(
            generic.action(),
            GameSlotAction::RelayOriginal(GameSlotRelayAudience::RoomExceptSender)
        );

        let shapes = [
            (
                (GOP_BANANA_HASH, GO_ITEM_BANANA_HASH),
                30,
                ItemOperationShape::Banana,
            ),
            (
                (super::GOP_COURSE_HASH, super::GO_COURSE_HASH),
                32,
                ItemOperationShape::Course,
            ),
            (
                (GOP_ROCKET_HASH, GO_ITEM_ROCKET_HASH),
                73,
                ItemOperationShape::Rocket,
            ),
        ];
        for (pair, length, expected_shape) in shapes {
            let parsed =
                parse_game_slot_packet(&operation_packet(PLAYER_MASK, pair, length)).unwrap();
            let GameSlotBody::ItemOperation(operation) = parsed.body() else {
                panic!("expected item operation");
            };
            assert_eq!(operation.shape, expected_shape);
        }
    }

    #[test]
    fn every_audited_type_twelve_pair_parses_with_its_supported_shape() {
        for pair in P5136_ITEM_OPERATION_PAIRS {
            let wire = if pair == (GOP_BARRICADE_HASH, GO_ITEM_BARRICADE_HASH) {
                barricade_packet()
            } else {
                operation_packet(
                    PLAYER_MASK,
                    pair,
                    super::exact_operation_length(pair).unwrap_or(8),
                )
            };
            let parsed = parse_game_slot_packet(&wire).unwrap();
            let GameSlotBody::ItemOperation(operation) = parsed.body() else {
                panic!("expected item operation");
            };
            assert_eq!(
                (operation.operation_hash, operation.operation_base_hash),
                pair
            );
        }
    }

    #[test]
    fn cube_for_boss_and_lucci_pairs_are_explicitly_outside_the_allowlist() {
        for pair in [
            (
                adler32::packet_hash("GopCubeForBoss"),
                adler32::packet_hash("GoItemCubeForBoss"),
            ),
            (
                adler32::packet_hash("GopLucci"),
                adler32::packet_hash("GoLucci"),
            ),
        ] {
            assert!(!P5136_ITEM_OPERATION_PAIRS.contains(&pair));
            assert!(matches!(
                parse_game_slot_packet(&operation_packet(PLAYER_MASK, pair, 8)),
                Err(GameSlotDropReason::UnsupportedItemOperation {
                    operation_hash,
                    operation_base_hash,
                }) if (operation_hash, operation_base_hash) == pair
            ));
        }
    }

    #[test]
    fn type_twelve_rejects_masks_unknown_pairs_wrong_known_lengths_and_envelopes() {
        for mask in [0, 1 << 16] {
            assert!(matches!(
                parse_game_slot_packet(&operation_packet(mask, WATERBOMB_PAIR, 8)),
                Err(GameSlotDropReason::InvalidMask {
                    packet_type: 12,
                    rule: GameSlotMaskRule::NonzeroLowSixteenBits,
                    ..
                })
            ));
        }
        assert!(matches!(
            parse_game_slot_packet(&operation_packet(
                PLAYER_MASK,
                (WATERBOMB_PAIR.0, WATERFLY_BASE_HASH),
                8,
            )),
            Err(GameSlotDropReason::UnsupportedItemOperation { .. })
        ));

        for (pair, expected) in [
            ((GOP_BANANA_HASH, GO_ITEM_BANANA_HASH), 30),
            ((super::GOP_COURSE_HASH, super::GO_COURSE_HASH), 32),
            ((GOP_ROCKET_HASH, GO_ITEM_ROCKET_HASH), 73),
            ((GOP_BARRICADE_HASH, GO_ITEM_BARRICADE_HASH), 73),
        ] {
            assert!(matches!(
                parse_game_slot_packet(&operation_packet(PLAYER_MASK, pair, 8)),
                Err(GameSlotDropReason::InvalidKnownItemOperationLength {
                    actual: 8,
                    expected: actual_expected,
                    ..
                }) if actual_expected == expected
            ));
        }

        let mut trailing = operation_packet(PLAYER_MASK, WATERBOMB_PAIR, 8);
        trailing.push(0);
        assert!(matches!(
            parse_game_slot_packet(&trailing),
            Err(GameSlotDropReason::BlobLengthMismatch {
                packet_type: 12,
                declared: 8,
                actual: 9,
            })
        ));

        let mut over_cap = operation_packet(PLAYER_MASK, WATERBOMB_PAIR, 8);
        set_u32(
            &mut over_cap,
            16,
            u32::try_from(MAX_GAME_SLOT_BLOB_LENGTH + 1).unwrap(),
        );
        assert!(matches!(
            parse_game_slot_packet(&over_cap),
            Err(GameSlotDropReason::BlobLengthOverCap {
                packet_type: 12,
                ..
            })
        ));
    }

    #[test]
    fn barricade_body_is_typed_finite_and_sender_inclusive() {
        let wire = barricade_packet();
        let parsed = parse_game_slot_packet(&wire).unwrap();
        let GameSlotBody::ItemOperation(operation) = parsed.body() else {
            panic!("expected item operation");
        };
        let ItemOperationShape::Barricade(placement) = operation.shape else {
            panic!("expected barricade placement");
        };
        assert_eq!(placement.object_id, 0x1234_5678);
        assert_eq!(placement.tick, 0x9ABC_DEF0);
        assert_eq!(placement.owner_id, 3);
        assert_eq!(placement.x().to_bits(), 1.25_f32.to_bits());
        assert_eq!(placement.y().to_bits(), (-2.5_f32).to_bits());
        assert_eq!(placement.z().to_bits(), 3.75_f32.to_bits());
        assert_eq!(
            parsed.action(),
            GameSlotAction::RelayOriginal(GameSlotRelayAudience::RoomIncludingSender)
        );
    }

    #[test]
    fn barricade_marker_owner_reserved_and_every_transform_float_are_validated() {
        let mut wrong_marker = barricade_packet();
        wrong_marker[32] = 0;
        assert!(matches!(
            parse_game_slot_packet(&wrong_marker),
            Err(GameSlotDropReason::InvalidBarricadeMarker { actual: 0 })
        ));

        let mut wrong_owner = barricade_packet();
        set_i32(&mut wrong_owner, 37, 2);
        assert!(matches!(
            parse_game_slot_packet(&wrong_owner),
            Err(GameSlotDropReason::InvalidBarricadeOwner {
                player_id: 3,
                owner_id: 2,
            })
        ));

        let mut nonzero_reserved = barricade_packet();
        set_u32(&mut nonzero_reserved, 41, 1);
        assert!(matches!(
            parse_game_slot_packet(&nonzero_reserved),
            Err(GameSlotDropReason::InvalidBarricadeReserved { actual: 1 })
        ));

        for index in 0..12 {
            let mut non_finite = barricade_packet();
            set_f32(&mut non_finite, 45 + index * 4, f32::INFINITY);
            assert!(matches!(
                parse_game_slot_packet(&non_finite),
                Err(GameSlotDropReason::NonFiniteBarricadeTransform { index: actual })
                    if actual == index
            ));
        }
    }

    #[test]
    fn every_supported_type_rejects_every_truncated_prefix_without_panicking() {
        let fixtures = [
            pickup_packet(1, [0.0; 3], 0),
            pickup_packet(2, [0.0; 3], 0),
            item_vector_packet(PLAYER_MASK, &[1, 2, 3]),
            item_use_packet(PLAYER_MASK, 1, 2, 3, 4, &[5, 6]),
            item_reaction_packet(PLAYER_MASK, 1, 2, &[3, 4]),
            operation_packet(PLAYER_MASK, WATERBOMB_PAIR, 8),
            barricade_packet(),
        ];
        for wire in fixtures {
            for length in 0..wire.len() {
                assert!(
                    parse_game_slot_packet(&wire[..length]).is_err(),
                    "type {} unexpectedly accepted a {length}-byte prefix",
                    wire[12]
                );
            }
        }
    }

    fn common_packet(player_id: i32, mask: u32, packet_type: u8) -> PacketWriter {
        let mut packet = PacketWriter::named(GAME_SLOT_PACKET_NAME);
        packet.write_i32(player_id);
        packet.write_u32(mask);
        packet.write_u8(packet_type);
        packet
    }

    fn pickup_packet(packet_type: u8, position: [f32; 3], live_rank: i16) -> Vec<u8> {
        let mut packet = common_packet(PLAYER_ID, u32::MAX, packet_type);
        let mut context = [0; 25];
        for (index, value) in position.into_iter().enumerate() {
            context[13 + index * 4..17 + index * 4].copy_from_slice(&value.to_le_bytes());
        }
        packet.write_bytes(&context);
        packet.write_i16(live_rank);
        packet.write_u8(0);
        packet.write_u32(0);
        packet.write_u32(24);
        packet.write_u32(GOP_CUBE_HASH);
        packet.write_u32(GO_ITEM_CUBE_HASH);
        packet.write_bytes(&[0; 16]);
        packet.into_inner()
    }

    fn item_vector_packet(mask: u32, items: &[u32]) -> Vec<u8> {
        let mut packet = common_packet(PLAYER_ID, mask, 9);
        packet.write_bytes(&[0; 3]);
        packet.write_u32(u32::try_from(8 + items.len() * 4).unwrap());
        packet.write_u32(GAME_KART_ITEM_INFO_HASH);
        packet.write_u32(u32::try_from(items.len()).unwrap());
        for item in items {
            packet.write_u32(*item);
        }
        packet.into_inner()
    }

    fn item_use_packet(
        mask: u32,
        uni: u8,
        success: u8,
        unknown: u8,
        skill: i16,
        blob: &[u8],
    ) -> Vec<u8> {
        let mut packet = common_packet(PLAYER_ID, mask, 10);
        packet.write_u8(uni);
        packet.write_u8(success);
        packet.write_u8(unknown);
        packet.write_i16(skill);
        packet.write_u32(0);
        packet.write_u32(u32::try_from(blob.len()).unwrap());
        packet.write_bytes(blob);
        packet.into_inner()
    }

    fn item_reaction_packet(mask: u32, uni: u8, skill: i16, blob: &[u8]) -> Vec<u8> {
        let mut packet = common_packet(PLAYER_ID, mask, 11);
        packet.write_u8(uni);
        packet.write_i16(skill);
        packet.write_u8(0);
        packet.write_i16(0);
        packet.write_u32(u32::try_from(blob.len()).unwrap());
        packet.write_bytes(blob);
        packet.into_inner()
    }

    fn operation_packet(mask: u32, pair: (u32, u32), length: usize) -> Vec<u8> {
        assert!(length >= 8);
        let mut packet = common_packet(PLAYER_ID, mask, 12);
        packet.write_bytes(&[0; 3]);
        packet.write_u32(u32::try_from(length).unwrap());
        packet.write_u32(pair.0);
        packet.write_u32(pair.1);
        packet.write_bytes(&vec![0; length - 8]);
        packet.into_inner()
    }

    fn barricade_packet() -> Vec<u8> {
        let mut packet = operation_packet(
            PLAYER_MASK,
            (GOP_BARRICADE_HASH, GO_ITEM_BARRICADE_HASH),
            73,
        );
        set_u32(&mut packet, 28, 0x1234_5678);
        packet[32] = 1;
        set_u32(&mut packet, 33, 0x9ABC_DEF0);
        set_i32(&mut packet, 37, PLAYER_ID);
        set_u32(&mut packet, 41, 0);
        for (index, value) in [
            1.25, -2.5, 3.75, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0,
        ]
        .into_iter()
        .enumerate()
        {
            set_f32(&mut packet, 45 + index * 4, value);
        }
        packet
    }

    fn set_u32(packet: &mut [u8], offset: usize, value: u32) {
        packet[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn set_i32(packet: &mut [u8], offset: usize, value: i32) {
        packet[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn set_f32(packet: &mut [u8], offset: usize, value: f32) {
        packet[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
}
