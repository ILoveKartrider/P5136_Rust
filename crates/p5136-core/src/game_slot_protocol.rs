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

use crate::game_slot_item_schema::{
    ItemOperationEvidence, ItemOperationSchema, ItemOperationValidationError,
    is_known_item_operation_pair, item_operation_schema,
};
use crate::game_slot_item_semantics::{
    ItemOperationSemantics, decode_validated_item_operation_semantics,
};

pub const GAME_SLOT_PACKET_NAME: &str = "GameSlotPacket";
pub const GAME_SLOT_PACKET_HASH: u32 = 0x27C0_0574;
pub const MAX_GAME_SLOT_LOGICAL_LENGTH: usize = 1_013;
pub const MAX_GAME_SLOT_BLOB_LENGTH: usize = 0x3c0;
pub const MAX_GAME_SLOT_PLAYER_ID: u8 = 15;

pub const GOP_CUBE_HASH: u32 = 0x0A4F_02A5;
pub const GO_ITEM_CUBE_HASH: u32 = 0x1434_03C4;
pub const GAME_KART_ITEM_INFO_HASH: u32 = 0x5FC3_087F;
pub const GAME_KART_PACKET_HASH: u32 = 0x2725_0564;
pub const GAME_KART_QUAD_PACKET_HASH: u32 = 0x4060_06EF;

pub const GOP_BANANA_HASH: u32 = 0x1090_0367;
pub const GO_ITEM_BANANA_HASH: u32 = 0x1CB3_0486;
pub const GOP_COURSE_HASH: u32 = 0x1139_0397;
pub const GO_COURSE_HASH: u32 = 0x0D73_0327;
pub const GOP_ROCKET_HASH: u32 = 0x1129_038E;
pub const GO_ITEM_ROCKET_HASH: u32 = 0x1D4C_04AD;
pub const GOP_BARRICADE_HASH: u32 = 0x1D86_04A3;
pub const GO_ITEM_BARRICADE_HASH: u32 = 0x2D06_05C2;

pub const GOP_LUCCI_HASH: u32 = 0x0D89_0316;
pub const GO_LUCCI_HASH: u32 = 0x0A33_02A6;
pub const GOP_BONUS_ITEM_HASH: u32 = 0x1DF9_04BC;
pub const GO_BONUS_ITEM_HASH: u32 = 0x18E3_044C;
pub const GOP_TEAM_FLAG_HASH: u32 = 0x18A2_0427;
pub const GO_TEAM_FLAG_HASH: u32 = 0x13FC_03B7;

const COMMON_ENVELOPE_LENGTH: usize = 13;
const P5136_PLAYER_MASK: u32 = 0x0000_ffff;
const PICKUP_BLOB_LENGTH: usize = 24;

/// The only actions implied by a successful P5136 decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameSlotAction {
    /// Accept a statically proven client notification without replying,
    /// relaying, or mutating shared race state.
    ConsumeNoReply,
    /// The actor must select an item from its immutable probability snapshot
    /// and synthesize a sender-inclusive authoritative pickup response.
    SynthesizeItemPickup,
    /// The wire shape is validated, but a server behavior needed for relay has
    /// not yet crossed its evidence gate.
    EvidencePending(GameSlotEvidencePending),
    /// Relay the exact owned input bytes to the validated audience.
    RelayOriginal(GameSlotRelayAudience),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameSlotEvidencePending {
    WorldObjectCollectionAuthorization(WorldObjectCollectionKind),
    TeamFlagAuthorization(TeamFlagTransitionKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameSlotRelayAudience {
    /// The actor must derive the complete current racer peer mask and require
    /// the client mask to match it before publishing.
    AllRacePeersMaskMatch,
    RecipientMaskExceptSender,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemPickupKind {
    Type1,
    Type2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemPickupToken {
    pub object_id: u32,
    pub operation_tick: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ItemPickup {
    pub kind: ItemPickupKind,
    pub token: ItemPickupToken,
    pub live_rank: i16,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub blob: GameSlotPayloadRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorldObjectCollectionKind {
    Lucci,
    BonusItem,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldObjectCollection {
    pub kind: WorldObjectCollectionKind,
    pub object_id: u32,
    pub current_tick: u32,
    pub expiry_tick: u32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub trailing_word: u16,
    pub collector_id: u8,
    pub operation_tick: u32,
    pub variant: u8,
    pub blob: GameSlotPayloadRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeamFlagTransitionKind {
    PickupAttach,
    DropRelocate,
    ReturnReset,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TeamFlagTransition {
    pub kind: TeamFlagTransitionKind,
    pub object_id: u32,
    pub current_tick: u32,
    pub expiry_tick: u32,
    pub outer_position: [f32; 3],
    pub trailing_word: u16,
    pub carrier_id: Option<u8>,
    pub transition_tick: u32,
    pub transition_position: Option<[f32; 3]>,
    pub payload: GameSlotPayloadRange,
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

/// Kart-specific `firing2Gain` probability/reward resolution belongs to the
/// P5136 client. The server must relay this report byte-exactly and must not
/// synthesize a second item award for the reporting player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemUse {
    pub kind: ItemUseKind,
    /// Raw wire byte 13. `sub_842180` does not initialise it; captures vary
    /// for the same effect and confirm that it is not a stable enum.
    pub raw_byte_13: u8,
    /// Full phase/status word. In particular, 0x0010 selects the receiver's
    /// held-slot reconciliation path; 0x1001/0x1002/0x1004 are distinct.
    pub status: u16,
    pub item_or_skill: u16,
    /// Raw producer byte at wire offset 18. It is not a boolean: the stock
    /// producer leaves it uninitialized for most item-use forms.
    pub raw_byte_18: u8,
    /// Producer-supplied byte at wire offset 19 (`sub_842180` argument 7).
    /// Every retained P5136 sample is zero; no broader enum is inferred.
    pub raw_byte_19: u8,
    /// After successful local slot reconciliation, a nonzero value runs
    /// `sub_AC3060` followed by `sub_AC3AC0`. Individual effect IDs remain
    /// packet-specific, so Rust only preserves the value.
    pub ancillary_effect_code: u16,
    pub blob: GameSlotPayloadRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemUseKind {
    Ordinary,
    SpawnedWorldObject,
}

/// Kart-specific `fired2Gain` probability/reward resolution belongs to the
/// P5136 client. This report is peer state, not a server award request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemReaction {
    pub uni: u8,
    pub skill: i16,
    pub blob: GameSlotPayloadRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DashboardAllocationNotice {
    /// `sub_842180` does not initialise this generic envelope byte.
    pub raw_byte_13: u8,
    pub payload: GameSlotPayloadRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KartSnapshotKind {
    FullPrecision,
    QuantizedQuad,
}

/// Type-17 remote-kart state. The protected/quantized numerical members stay
/// byte-preserving; the recovered native writer determines their exact length
/// from these three flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KartSnapshot {
    pub kind: KartSnapshotKind,
    pub common: u8,
    pub object_id: u32,
    pub flags: [u8; 3],
    pub payload: GameSlotPayloadRange,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ItemOperation {
    pub operation_hash: u32,
    pub operation_base_hash: u32,
    pub schema: &'static ItemOperationSchema,
    pub object_id: u32,
    pub state: u32,
    pub evidence: ItemOperationEvidence,
    pub semantics: ItemOperationSemantics,
    pub barricade: Option<BarricadeOperation>,
    pub payload: GameSlotPayloadRange,
}

/// A type-12 operation whose pair is known to the P5136 compatibility family,
/// but whose body is not one of the recovered native-writer shapes.
///
/// This is deliberately opaque: type-12 has no Rust-side world mutation. The
/// outer parser has already verified the exact envelope length, hash pair,
/// claimed actor, and low-16 peer mask before the World actor may relay it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundedItemOperation {
    pub operation_hash: u32,
    pub operation_base_hash: u32,
    pub schema: Option<&'static ItemOperationSchema>,
    pub payload: GameSlotPayloadRange,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BarricadeOperation {
    Placement(BarricadePlacement),
    Transition(BarricadeTransition),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarricadeTransition {
    pub object_id: u32,
    pub state: u8,
    pub tick: u32,
    /// The original installer carried by the native operation. A remote kart
    /// may report the hit, so this deliberately need not equal the outer
    /// `GameSlot` sender.
    pub owner_id: u8,
    pub trailing: u32,
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
    WorldObjectCollection(WorldObjectCollection),
    TeamFlagTransition(TeamFlagTransition),
    ItemVector(ItemVector),
    ItemUse(ItemUse),
    ItemReaction(ItemReaction),
    DashboardAllocationNotice(DashboardAllocationNotice),
    KartSnapshot(KartSnapshot),
    ItemOperation(ItemOperation),
    BoundedItemOperation(BoundedItemOperation),
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
            Self::WorldObjectCollection(WorldObjectCollection {
                kind: WorldObjectCollectionKind::Lucci,
                ..
            }) => 4,
            Self::WorldObjectCollection(WorldObjectCollection {
                kind: WorldObjectCollectionKind::BonusItem,
                ..
            }) => 6,
            Self::TeamFlagTransition(TeamFlagTransition {
                kind: TeamFlagTransitionKind::PickupAttach,
                ..
            }) => 5,
            Self::TeamFlagTransition(TeamFlagTransition {
                kind: TeamFlagTransitionKind::DropRelocate,
                ..
            }) => 7,
            Self::TeamFlagTransition(TeamFlagTransition {
                kind: TeamFlagTransitionKind::ReturnReset,
                ..
            }) => 8,
            Self::ItemVector(_) => 9,
            Self::ItemUse(ItemUse {
                kind: ItemUseKind::Ordinary,
                ..
            }) => 10,
            Self::ItemReaction(_) => 11,
            Self::ItemOperation(_) | Self::BoundedItemOperation(_) => 12,
            Self::DashboardAllocationNotice(_) => 13,
            Self::ItemUse(ItemUse {
                kind: ItemUseKind::SpawnedWorldObject,
                ..
            }) => 16,
            Self::KartSnapshot(_) => 17,
        }
    }

    const fn payload_range(&self) -> GameSlotPayloadRange {
        match self {
            Self::ItemPickup(value) => value.blob,
            Self::WorldObjectCollection(value) => value.blob,
            Self::TeamFlagTransition(value) => value.payload,
            Self::ItemVector(value) => value.payload,
            Self::ItemUse(value) => value.blob,
            Self::ItemReaction(value) => value.blob,
            Self::DashboardAllocationNotice(value) => value.payload,
            Self::KartSnapshot(value) => value.payload,
            Self::ItemOperation(value) => value.payload,
            Self::BoundedItemOperation(value) => value.payload,
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

    pub fn into_item_pickup_award(
        mut self,
        item_id: i16,
    ) -> Result<Vec<u8>, GameSlotSynthesisError> {
        if item_id <= 0 {
            return Err(GameSlotSynthesisError::InvalidItemId(item_id));
        }
        if self.action != GameSlotAction::SynthesizeItemPickup
            || !matches!(self.body, GameSlotBody::ItemPickup(_))
        {
            return Err(GameSlotSynthesisError::NotItemPickup);
        }
        let item_id_end = 40;
        if self.raw.len() <= item_id_end {
            return Err(GameSlotSynthesisError::PickupInvariant {
                actual: self.raw.len(),
                minimum: item_id_end + 1,
            });
        }
        self.raw[38..item_id_end].copy_from_slice(&item_id.to_le_bytes());
        self.raw[item_id_end] = 1;
        Ok(self.raw)
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
    LowSixteenBitsExcludingSender,
    NonzeroLowSixteenBits,
    NonzeroLowSixteenBitsIncludingSender,
    NonzeroLowSixteenBitsExcludingSender,
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

    #[error("GameSlot type 17 reserved word is 0x{actual:04X}; expected zero")]
    InvalidKartSnapshotReservedWord { actual: u16 },

    #[error("GameSlot type 13 reserved word is 0x{actual:04X}; expected zero")]
    InvalidDashboardAllocationReservedWord { actual: u16 },

    #[error("GameSlot type 13 has a {actual}-byte nested body; expected an empty body")]
    InvalidDashboardAllocationBodyLength { actual: usize },

    #[error("GameSlot type 17 nested hash 0x{actual:08X} is not a P5136 kart snapshot")]
    UnexpectedKartSnapshotHash { actual: u32 },

    #[error(
        "GameSlot type 17 {kind:?} snapshot has {actual} payload bytes; flags require exactly {expected}"
    )]
    InvalidKartSnapshotLength {
        kind: KartSnapshotKind,
        actual: usize,
        expected: usize,
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

    #[error(
        "GameSlot type {packet_type} item-pickup request state is status {status}, item {item_id}; not a captured pre-award shape"
    )]
    InvalidPickupRequestState {
        packet_type: u8,
        status: u8,
        item_id: u32,
    },

    #[error("item-pickup outer object ID 0x{outer:08X} differs from nested ID 0x{nested:08X}")]
    PickupObjectIdMismatch { outer: u32, nested: u32 },

    #[error(
        "GameSlot type 1 item-pickup object ID 0x{actual:08X} is outside 0xF0000000..=0xF00000FF"
    )]
    InvalidType1PickupObjectId { actual: u32 },

    #[error("GameSlot type 2 item-pickup object ID 0x{actual:08X} is not 0x00FFFFFF")]
    InvalidType2PickupObjectId { actual: u32 },

    #[error(
        "GameSlot type {packet_type} item-pickup {field} tick is {actual}; expected {expected}"
    )]
    InvalidPickupTick {
        packet_type: u8,
        field: &'static str,
        actual: u32,
        expected: u32,
    },

    #[error("item-pickup nested state is {actual}; expected 1")]
    InvalidPickupState { actual: u32 },

    #[error("item-pickup nested owner {nested} does not match claimed player {claimed}")]
    InvalidPickupOwner { claimed: u8, nested: u32 },

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

    #[error("GameSlot type 12 reserved word is 0x{actual:04X}; expected zero")]
    InvalidItemOperationReservedWord { actual: u16 },

    #[error(transparent)]
    ItemOperationValidation(#[from] ItemOperationValidationError),

    #[error(
        "GameSlot type {packet_type} collection operation pair 0x{operation_hash:08X}/0x{operation_base_hash:08X} is not the statically bound class"
    )]
    InvalidCollectionOperation {
        packet_type: u8,
        operation_hash: u32,
        operation_base_hash: u32,
    },

    #[error(
        "GameSlot type {packet_type} collection body has {actual} bytes; expected exactly {expected}"
    )]
    InvalidCollectionBlobLength {
        packet_type: u8,
        actual: usize,
        expected: usize,
    },

    #[error("GameSlot type {packet_type} collection carries object ID -1")]
    MissingCollectionObjectId { packet_type: u8 },

    #[error(
        "GameSlot type {packet_type} outer object ID {outer} differs from nested object ID {nested}"
    )]
    CollectionObjectIdMismatch {
        packet_type: u8,
        outer: u32,
        nested: u32,
    },

    #[error("GameSlot type {packet_type} collection state is {actual}; expected 1")]
    InvalidCollectionState { packet_type: u8, actual: u32 },

    #[error("GameSlot type {packet_type} collector ID {collector} is outside 0..=15")]
    InvalidCollectionCollector { packet_type: u8, collector: i32 },

    #[error("GameSlot type {packet_type} collection coordinate {axis} is not finite")]
    NonFiniteCollectionPosition { packet_type: u8, axis: usize },

    #[error(
        "GameSlot type {packet_type} team-flag body has {actual} bytes; expected exactly {expected}"
    )]
    InvalidTeamFlagBlobLength {
        packet_type: u8,
        actual: usize,
        expected: usize,
    },

    #[error(
        "GameSlot type {packet_type} team-flag operation pair 0x{operation_hash:08X}/0x{operation_base_hash:08X} is invalid"
    )]
    InvalidTeamFlagOperation {
        packet_type: u8,
        operation_hash: u32,
        operation_base_hash: u32,
    },

    #[error(
        "GameSlot type {packet_type} outer team-flag ID {outer} differs from nested ID {nested}"
    )]
    TeamFlagObjectIdMismatch {
        packet_type: u8,
        outer: u32,
        nested: u32,
    },

    #[error("GameSlot type {packet_type} team-flag state is {actual}; expected {expected}")]
    InvalidTeamFlagState {
        packet_type: u8,
        actual: u32,
        expected: u32,
    },

    #[error("GameSlot type {packet_type} team-flag carrier ID {carrier} is outside 0..=15")]
    InvalidTeamFlagCarrier { packet_type: u8, carrier: i32 },

    #[error("GameSlot type {packet_type} team-flag coordinate {axis} is not finite")]
    NonFiniteTeamFlagPosition { packet_type: u8, axis: usize },

    #[error("barricade placement owner ID {owner_id} does not match claimed player ID {player_id}")]
    InvalidBarricadePlacementOwner { player_id: u8, owner_id: i32 },

    #[error("barricade transition owner ID {owner_id} is outside 0..=15")]
    InvalidBarricadeTransitionOwner { owner_id: i32 },

    #[error("barricade reserved field is 0x{actual:08X}; expected zero")]
    InvalidBarricadeReserved { actual: u32 },

    #[error("barricade transform float {index} is not finite")]
    NonFiniteBarricadeTransform { index: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GameSlotSynthesisError {
    #[error("item-pickup award item ID {0} must be positive")]
    InvalidItemId(i16),
    #[error("only a validated item-pickup request can synthesize an item award")]
    NotItemPickup,
    #[error(
        "validated item-pickup packet invariant failed: got {actual} bytes, need at least {minimum}"
    )]
    PickupInvariant { actual: usize, minimum: usize },
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
        1 | 2 => parse_item_pickup(packet, packet_type, player_id, item_or_recipient_mask)?,
        4 | 6 => {
            parse_world_object_collection(packet, packet_type, player_id, item_or_recipient_mask)?
        }
        5 | 7 | 8 => parse_team_flag_transition(packet, packet_type, item_or_recipient_mask)?,
        9 => parse_item_vector(packet, player_id, item_or_recipient_mask)?,
        10 | 16 => parse_item_use(packet, packet_type, player_id, item_or_recipient_mask)?,
        11 => parse_item_reaction(packet, player_id, item_or_recipient_mask)?,
        12 => parse_item_operation(packet, player_id, item_or_recipient_mask)?,
        13 => parse_dashboard_allocation_notice(packet, player_id, item_or_recipient_mask)?,
        17 => parse_kart_snapshot(packet, player_id, item_or_recipient_mask)?,
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
    player_id: u8,
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
    let (kind, token) = parse_item_pickup_token(packet, packet_type, player_id)?;

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

    let pickup = ItemPickup {
        kind,
        token,
        live_rank: read_i16(packet, 38, "GameSlot item pickup live rank")?,
        x: position[0],
        y: position[1],
        z: position[2],
        blob,
    };
    Ok((
        GameSlotBody::ItemPickup(pickup),
        GameSlotAction::SynthesizeItemPickup,
    ))
}

fn parse_item_pickup_token(
    packet: &[u8],
    packet_type: u8,
    player_id: u8,
) -> Result<(ItemPickupKind, ItemPickupToken), GameSlotDropReason> {
    let request_status = packet[40];
    let request_item_id = read_u32(packet, 41, "GameSlot item pickup request state")?;
    let valid_request_state = if packet_type == 1 {
        request_status == 0 && request_item_id == 0x0000_ffff
    } else {
        request_status == 8 && (1..=32_767).contains(&request_item_id)
    };
    if !valid_request_state {
        return Err(GameSlotDropReason::InvalidPickupRequestState {
            packet_type,
            status: request_status,
            item_id: request_item_id,
        });
    }

    let object_id = read_u32(packet, 14, "GameSlot item pickup object")?;
    let nested_object_id = read_u32(packet, 57, "GameSlot item pickup nested object")?;
    if nested_object_id != object_id {
        return Err(GameSlotDropReason::PickupObjectIdMismatch {
            outer: object_id,
            nested: nested_object_id,
        });
    }
    let nested_state = read_u32(packet, 61, "GameSlot item pickup state")?;
    if nested_state != 1 {
        return Err(GameSlotDropReason::InvalidPickupState {
            actual: nested_state,
        });
    }
    let nested_owner = read_u32(packet, 65, "GameSlot item pickup owner")?;
    if nested_owner != u32::from(player_id) {
        return Err(GameSlotDropReason::InvalidPickupOwner {
            claimed: player_id,
            nested: nested_owner,
        });
    }

    let first_tick = read_u32(packet, 18, "GameSlot item pickup tick")?;
    let second_tick = read_u32(packet, 22, "GameSlot item pickup tick")?;
    let operation_tick = read_u32(packet, 69, "GameSlot item pickup operation tick")?;
    let kind = if packet_type == 1 {
        if object_id & 0xffff_ff00 != 0xf000_0000 {
            return Err(GameSlotDropReason::InvalidType1PickupObjectId { actual: object_id });
        }
        if first_tick != operation_tick {
            return Err(GameSlotDropReason::InvalidPickupTick {
                packet_type,
                field: "first",
                actual: first_tick,
                expected: operation_tick,
            });
        }
        let expected_second = first_tick.wrapping_add(1_500);
        if second_tick != expected_second {
            return Err(GameSlotDropReason::InvalidPickupTick {
                packet_type,
                field: "second",
                actual: second_tick,
                expected: expected_second,
            });
        }
        ItemPickupKind::Type1
    } else {
        if object_id != 0x00ff_ffff {
            return Err(GameSlotDropReason::InvalidType2PickupObjectId { actual: object_id });
        }
        if second_tick != first_tick {
            return Err(GameSlotDropReason::InvalidPickupTick {
                packet_type,
                field: "second",
                actual: second_tick,
                expected: first_tick,
            });
        }
        ItemPickupKind::Type2
    };
    Ok((
        kind,
        ItemPickupToken {
            object_id,
            operation_tick,
        },
    ))
}

fn parse_world_object_collection(
    packet: &[u8],
    packet_type: u8,
    player_id: u8,
    mask: u32,
) -> Result<(GameSlotBody, GameSlotAction), GameSlotDropReason> {
    const BLOB_LENGTH_OFFSET: usize = 40;
    const BLOB_OFFSET: usize = 44;
    const BLOB_LENGTH: usize = 25;
    ensure_minimum(packet, "GameSlot world-object collection", BLOB_OFFSET)?;
    validate_mask(packet_type, mask, player_id, GameSlotMaskRule::AllBitsSet)?;
    let declared = read_u32(
        packet,
        BLOB_LENGTH_OFFSET,
        "GameSlot world-object collection",
    )?;
    let blob = validate_blob(packet, packet_type, BLOB_OFFSET, declared)?;
    if blob.length != BLOB_LENGTH {
        return Err(GameSlotDropReason::InvalidCollectionBlobLength {
            packet_type,
            actual: blob.length,
            expected: BLOB_LENGTH,
        });
    }

    let kind = if packet_type == 4 {
        WorldObjectCollectionKind::Lucci
    } else {
        WorldObjectCollectionKind::BonusItem
    };
    let expected_pair = match kind {
        WorldObjectCollectionKind::Lucci => (GOP_LUCCI_HASH, GO_LUCCI_HASH),
        WorldObjectCollectionKind::BonusItem => (GOP_BONUS_ITEM_HASH, GO_BONUS_ITEM_HASH),
    };
    let operation_hash = read_u32(packet, BLOB_OFFSET, "collection operation hash")?;
    let operation_base_hash = read_u32(packet, BLOB_OFFSET + 4, "collection base-operation hash")?;
    if (operation_hash, operation_base_hash) != expected_pair {
        return Err(GameSlotDropReason::InvalidCollectionOperation {
            packet_type,
            operation_hash,
            operation_base_hash,
        });
    }

    let outer_object_id = read_u32(packet, 14, "collection outer object ID")?;
    let nested_object_id = read_u32(packet, BLOB_OFFSET + 8, "collection nested object ID")?;
    if outer_object_id == u32::MAX || nested_object_id == u32::MAX {
        return Err(GameSlotDropReason::MissingCollectionObjectId { packet_type });
    }
    if outer_object_id != nested_object_id {
        return Err(GameSlotDropReason::CollectionObjectIdMismatch {
            packet_type,
            outer: outer_object_id,
            nested: nested_object_id,
        });
    }
    let state = read_u32(packet, BLOB_OFFSET + 12, "collection state")?;
    if state != 1 {
        return Err(GameSlotDropReason::InvalidCollectionState {
            packet_type,
            actual: state,
        });
    }
    let collector = read_i32(packet, BLOB_OFFSET + 16, "collection collector ID")?;
    let collector_id = u8::try_from(collector)
        .ok()
        .filter(|value| *value <= MAX_GAME_SLOT_PLAYER_ID)
        .ok_or(GameSlotDropReason::InvalidCollectionCollector {
            packet_type,
            collector,
        })?;

    let position = [
        read_f32(packet, 26, "collection position")?,
        read_f32(packet, 30, "collection position")?,
        read_f32(packet, 34, "collection position")?,
    ];
    for (axis, value) in position.into_iter().enumerate() {
        if !value.is_finite() {
            return Err(GameSlotDropReason::NonFiniteCollectionPosition { packet_type, axis });
        }
    }

    Ok((
        GameSlotBody::WorldObjectCollection(WorldObjectCollection {
            kind,
            object_id: outer_object_id,
            current_tick: read_u32(packet, 18, "collection current tick")?,
            expiry_tick: read_u32(packet, 22, "collection expiry tick")?,
            x: position[0],
            y: position[1],
            z: position[2],
            trailing_word: read_u16(packet, 38, "collection trailing word")?,
            collector_id,
            operation_tick: read_u32(packet, BLOB_OFFSET + 20, "collection operation tick")?,
            variant: packet[BLOB_OFFSET + 24],
            blob,
        }),
        GameSlotAction::EvidencePending(
            GameSlotEvidencePending::WorldObjectCollectionAuthorization(kind),
        ),
    ))
}

fn parse_team_flag_transition(
    packet: &[u8],
    packet_type: u8,
    mask: u32,
) -> Result<(GameSlotBody, GameSlotAction), GameSlotDropReason> {
    const PAYLOAD_LENGTH_OFFSET: usize = 40;
    const PAYLOAD_OFFSET: usize = 44;
    ensure_minimum(packet, "GameSlot team-flag transition", PAYLOAD_OFFSET)?;
    validate_mask(packet_type, mask, 0, GameSlotMaskRule::AllBitsSet)?;
    let declared = read_u32(
        packet,
        PAYLOAD_LENGTH_OFFSET,
        "GameSlot team-flag transition",
    )?;
    let payload = validate_blob(packet, packet_type, PAYLOAD_OFFSET, declared)?;
    let (kind, expected_state, expected_length) = match packet_type {
        5 => (TeamFlagTransitionKind::PickupAttach, 1, 24),
        7 => (TeamFlagTransitionKind::DropRelocate, 3, 32),
        8 => (TeamFlagTransitionKind::ReturnReset, 4, 32),
        _ => unreachable!("team-flag parser is only dispatched for types 5, 7, and 8"),
    };
    if payload.length != expected_length {
        return Err(GameSlotDropReason::InvalidTeamFlagBlobLength {
            packet_type,
            actual: payload.length,
            expected: expected_length,
        });
    }
    let operation_hash = read_u32(packet, PAYLOAD_OFFSET, "team-flag operation hash")?;
    let operation_base_hash = read_u32(packet, PAYLOAD_OFFSET + 4, "team-flag base hash")?;
    if (operation_hash, operation_base_hash) != (GOP_TEAM_FLAG_HASH, GO_TEAM_FLAG_HASH) {
        return Err(GameSlotDropReason::InvalidTeamFlagOperation {
            packet_type,
            operation_hash,
            operation_base_hash,
        });
    }
    let object_id = read_u32(packet, 14, "team-flag outer object ID")?;
    let nested_object_id = read_u32(packet, PAYLOAD_OFFSET + 8, "team-flag nested object ID")?;
    if object_id != nested_object_id {
        return Err(GameSlotDropReason::TeamFlagObjectIdMismatch {
            packet_type,
            outer: object_id,
            nested: nested_object_id,
        });
    }
    let state = read_u32(packet, PAYLOAD_OFFSET + 12, "team-flag state")?;
    if state != expected_state {
        return Err(GameSlotDropReason::InvalidTeamFlagState {
            packet_type,
            actual: state,
            expected: expected_state,
        });
    }

    let outer_position = read_finite_team_flag_position(packet, packet_type, 26, 0)?;
    let TeamFlagVariantFields {
        carrier_id,
        transition_tick,
        transition_position,
    } = parse_team_flag_variant_fields(packet, packet_type, kind, PAYLOAD_OFFSET)?;

    Ok((
        GameSlotBody::TeamFlagTransition(TeamFlagTransition {
            kind,
            object_id,
            current_tick: read_u32(packet, 18, "team-flag current tick")?,
            expiry_tick: read_u32(packet, 22, "team-flag expiry tick")?,
            outer_position,
            trailing_word: read_u16(packet, 38, "team-flag trailing word")?,
            carrier_id,
            transition_tick,
            transition_position,
            payload,
        }),
        GameSlotAction::EvidencePending(GameSlotEvidencePending::TeamFlagAuthorization(kind)),
    ))
}

struct TeamFlagVariantFields {
    carrier_id: Option<u8>,
    transition_tick: u32,
    transition_position: Option<[f32; 3]>,
}

fn parse_team_flag_variant_fields(
    packet: &[u8],
    packet_type: u8,
    kind: TeamFlagTransitionKind,
    payload_offset: usize,
) -> Result<TeamFlagVariantFields, GameSlotDropReason> {
    Ok(match kind {
        TeamFlagTransitionKind::PickupAttach => {
            let carrier = read_i32(packet, payload_offset + 16, "team-flag carrier ID")?;
            let carrier_id = u8::try_from(carrier)
                .ok()
                .filter(|value| *value <= MAX_GAME_SLOT_PLAYER_ID)
                .ok_or(GameSlotDropReason::InvalidTeamFlagCarrier {
                    packet_type,
                    carrier,
                })?;
            TeamFlagVariantFields {
                carrier_id: Some(carrier_id),
                transition_tick: read_u32(packet, payload_offset + 20, "team-flag pickup tick")?,
                transition_position: None,
            }
        }
        TeamFlagTransitionKind::DropRelocate => TeamFlagVariantFields {
            carrier_id: None,
            transition_tick: read_u32(packet, payload_offset + 28, "team-flag drop tick")?,
            transition_position: Some(read_finite_team_flag_position(
                packet,
                packet_type,
                payload_offset + 16,
                3,
            )?),
        },
        TeamFlagTransitionKind::ReturnReset => TeamFlagVariantFields {
            carrier_id: None,
            transition_tick: read_u32(packet, payload_offset + 16, "team-flag return tick")?,
            transition_position: Some(read_finite_team_flag_position(
                packet,
                packet_type,
                payload_offset + 20,
                3,
            )?),
        },
    })
}

fn read_finite_team_flag_position(
    packet: &[u8],
    packet_type: u8,
    offset: usize,
    axis_base: usize,
) -> Result<[f32; 3], GameSlotDropReason> {
    let position = [
        read_f32(packet, offset, "team-flag position")?,
        read_f32(packet, offset + 4, "team-flag position")?,
        read_f32(packet, offset + 8, "team-flag position")?,
    ];
    for (axis, value) in position.into_iter().enumerate() {
        if !value.is_finite() {
            return Err(GameSlotDropReason::NonFiniteTeamFlagPosition {
                packet_type,
                axis: axis_base + axis,
            });
        }
    }
    Ok(position)
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
        GameSlotAction::RelayOriginal(GameSlotRelayAudience::RecipientMaskExceptSender),
    ))
}

fn parse_item_use(
    packet: &[u8],
    packet_type: u8,
    player_id: u8,
    mask: u32,
) -> Result<(GameSlotBody, GameSlotAction), GameSlotDropReason> {
    const BLOB_LENGTH_OFFSET: usize = 22;
    const BLOB_OFFSET: usize = 26;
    ensure_minimum(packet, "GameSlot item use", BLOB_OFFSET)?;
    validate_mask(
        packet_type,
        mask,
        player_id,
        GameSlotMaskRule::LowSixteenBitsExcludingSender,
    )?;
    let declared = read_u32(packet, BLOB_LENGTH_OFFSET, "GameSlot item use")?;
    let blob = validate_blob(packet, packet_type, BLOB_OFFSET, declared)?;
    let kind = if packet_type == 10 {
        ItemUseKind::Ordinary
    } else {
        ItemUseKind::SpawnedWorldObject
    };
    let action = match kind {
        ItemUseKind::Ordinary | ItemUseKind::SpawnedWorldObject => {
            GameSlotAction::RelayOriginal(GameSlotRelayAudience::RecipientMaskExceptSender)
        }
    };

    Ok((
        GameSlotBody::ItemUse(ItemUse {
            kind,
            raw_byte_13: packet[13],
            status: read_u16(packet, 14, "GameSlot item use status")?,
            item_or_skill: read_u16(packet, 16, "GameSlot item use item/skill")?,
            raw_byte_18: packet[18],
            raw_byte_19: packet[19],
            ancillary_effect_code: read_u16(packet, 20, "GameSlot item-use ancillary effect")?,
            blob,
        }),
        action,
    ))
}

fn parse_item_reaction(
    packet: &[u8],
    player_id: u8,
    mask: u32,
) -> Result<(GameSlotBody, GameSlotAction), GameSlotDropReason> {
    const BLOB_LENGTH_OFFSET: usize = 19;
    const BLOB_OFFSET: usize = 23;
    ensure_minimum(packet, "GameSlot item reaction", BLOB_OFFSET)?;
    validate_mask(
        11,
        mask,
        player_id,
        GameSlotMaskRule::NonzeroLowSixteenBitsExcludingSender,
    )?;
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

fn parse_kart_snapshot(
    packet: &[u8],
    player_id: u8,
    mask: u32,
) -> Result<(GameSlotBody, GameSlotAction), GameSlotDropReason> {
    const PAYLOAD_LENGTH_OFFSET: usize = 16;
    const PAYLOAD_OFFSET: usize = 20;
    const FLAGS_OFFSET_IN_PAYLOAD: usize = 8;
    const MINIMUM_HEADER_LENGTH: usize = PAYLOAD_OFFSET + FLAGS_OFFSET_IN_PAYLOAD + 3;
    ensure_minimum(packet, "GameSlot kart snapshot", MINIMUM_HEADER_LENGTH)?;
    validate_mask(
        17,
        mask,
        player_id,
        GameSlotMaskRule::NonzeroLowSixteenBitsExcludingSender,
    )?;

    let reserved_word = read_u16(packet, 14, "GameSlot kart snapshot reserved word")?;
    if reserved_word != 0 {
        return Err(GameSlotDropReason::InvalidKartSnapshotReservedWord {
            actual: reserved_word,
        });
    }
    let declared = read_u32(
        packet,
        PAYLOAD_LENGTH_OFFSET,
        "GameSlot kart snapshot payload length",
    )?;
    let payload = validate_blob(packet, 17, PAYLOAD_OFFSET, declared)?;
    let nested_hash = read_u32(packet, PAYLOAD_OFFSET, "GameSlot kart snapshot hash")?;
    let kind = match nested_hash {
        GAME_KART_PACKET_HASH => KartSnapshotKind::FullPrecision,
        GAME_KART_QUAD_PACKET_HASH => KartSnapshotKind::QuantizedQuad,
        actual => return Err(GameSlotDropReason::UnexpectedKartSnapshotHash { actual }),
    };
    let flags = [
        packet[PAYLOAD_OFFSET + FLAGS_OFFSET_IN_PAYLOAD],
        packet[PAYLOAD_OFFSET + FLAGS_OFFSET_IN_PAYLOAD + 1],
        packet[PAYLOAD_OFFSET + FLAGS_OFFSET_IN_PAYLOAD + 2],
    ];
    let (base_length, secondary_orientation, optional_scalar) = match kind {
        KartSnapshotKind::FullPrecision => (116, 16, 4),
        KartSnapshotKind::QuantizedQuad => (76, 4, 2),
    };
    let expected = base_length
        + usize::from(flags[1] & 0x07 != 0) * secondary_orientation
        + usize::from(flags[0] & 0x04 != 0) * optional_scalar
        + usize::from(flags[0] & 0x08 != 0) * optional_scalar;
    if payload.length != expected {
        return Err(GameSlotDropReason::InvalidKartSnapshotLength {
            kind,
            actual: payload.length,
            expected,
        });
    }

    Ok((
        GameSlotBody::KartSnapshot(KartSnapshot {
            kind,
            common: packet[13],
            object_id: read_u32(
                packet,
                PAYLOAD_OFFSET + 4,
                "GameSlot kart snapshot object ID",
            )?,
            flags,
            payload,
        }),
        GameSlotAction::RelayOriginal(GameSlotRelayAudience::RecipientMaskExceptSender),
    ))
}

fn parse_dashboard_allocation_notice(
    packet: &[u8],
    player_id: u8,
    mask: u32,
) -> Result<(GameSlotBody, GameSlotAction), GameSlotDropReason> {
    const PAYLOAD_LENGTH_OFFSET: usize = 16;
    const PAYLOAD_OFFSET: usize = 20;
    ensure_minimum(packet, "GameSlot dashboard allocation", PAYLOAD_OFFSET)?;
    validate_mask(
        13,
        mask,
        player_id,
        GameSlotMaskRule::LowSixteenBitsExcludingSender,
    )?;
    let reserved_word = read_u16(packet, 14, "GameSlot dashboard allocation reserved word")?;
    if reserved_word != 0 {
        return Err(GameSlotDropReason::InvalidDashboardAllocationReservedWord {
            actual: reserved_word,
        });
    }
    let declared = read_u32(
        packet,
        PAYLOAD_LENGTH_OFFSET,
        "GameSlot dashboard allocation payload length",
    )?;
    let payload = validate_blob(packet, 13, PAYLOAD_OFFSET, declared)?;
    if !payload.is_empty() {
        return Err(GameSlotDropReason::InvalidDashboardAllocationBodyLength {
            actual: payload.len(),
        });
    }
    Ok((
        GameSlotBody::DashboardAllocationNotice(DashboardAllocationNotice {
            raw_byte_13: packet[13],
            payload,
        }),
        GameSlotAction::ConsumeNoReply,
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
    validate_mask(12, mask, player_id, GameSlotMaskRule::LowSixteenBits)?;
    let reserved_word = read_u16(packet, 14, "GameSlot item operation reserved word")?;
    if reserved_word != 0 {
        return Err(GameSlotDropReason::InvalidItemOperationReservedWord {
            actual: reserved_word,
        });
    }
    let declared = read_u32(packet, PAYLOAD_LENGTH_OFFSET, "GameSlot item operation")?;
    let payload = validate_blob(packet, 12, PAYLOAD_OFFSET, declared)?;
    let operation_hash = read_u32(packet, PAYLOAD_OFFSET, "GameSlot item operation hash")?;
    let operation_base_hash = read_u32(
        packet,
        PAYLOAD_OFFSET + 4,
        "GameSlot item base-operation hash",
    )?;
    let schema = item_operation_schema(operation_hash, operation_base_hash);
    if schema.is_none() && !is_known_item_operation_pair(operation_hash, operation_base_hash) {
        return Err(GameSlotDropReason::UnsupportedItemOperation {
            operation_hash,
            operation_base_hash,
        });
    }
    let payload_end = payload.offset.checked_add(payload.length).ok_or(
        GameSlotDropReason::BlobLengthOverCap {
            packet_type: 12,
            declared,
            maximum: MAX_GAME_SLOT_BLOB_LENGTH,
        },
    )?;
    let raw =
        packet
            .get(payload.offset..payload_end)
            .ok_or(GameSlotDropReason::BlobLengthMismatch {
                packet_type: 12,
                declared,
                actual: packet.len().saturating_sub(payload.offset),
            })?;
    // Preserve the stricter state/owner/finite-transform rules for the one
    // type-12 family that creates a server-observable world object. All other
    // known pairs are byte-preserving client events: C# relays them after the
    // common envelope and pair checks, even when a recovered writer schema
    // cannot describe that particular body.
    if operation_hash == GOP_BARRICADE_HASH {
        let schema = schema.expect("GopBarricade has a checked-in P5136 schema");
        let validated = schema.validate(raw)?;
        let semantics = decode_validated_item_operation_semantics(validated, raw);
        let barricade = match validated.state() {
            1 => Some(BarricadeOperation::Placement(parse_barricade_placement(
                packet, player_id,
            )?)),
            2..=4 => Some(BarricadeOperation::Transition(parse_barricade_transition(
                packet,
                validated.state(),
            )?)),
            _ => None,
        };
        return Ok((
            GameSlotBody::ItemOperation(ItemOperation {
                operation_hash,
                operation_base_hash,
                schema,
                object_id: validated.object_id(),
                state: validated.state(),
                evidence: validated.evidence(),
                semantics,
                barricade,
                payload,
            }),
            GameSlotAction::RelayOriginal(GameSlotRelayAudience::AllRacePeersMaskMatch),
        ));
    }

    if let Some(validated) = schema.and_then(|schema| schema.validate(raw).ok()) {
        let semantics = decode_validated_item_operation_semantics(validated, raw);
        return Ok((
            GameSlotBody::ItemOperation(ItemOperation {
                operation_hash,
                operation_base_hash,
                schema: validated.schema(),
                object_id: validated.object_id(),
                state: validated.state(),
                evidence: validated.evidence(),
                semantics,
                barricade: None,
                payload,
            }),
            GameSlotAction::RelayOriginal(GameSlotRelayAudience::AllRacePeersMaskMatch),
        ));
    }

    Ok((
        GameSlotBody::BoundedItemOperation(BoundedItemOperation {
            operation_hash,
            operation_base_hash,
            schema,
            payload,
        }),
        GameSlotAction::RelayOriginal(GameSlotRelayAudience::AllRacePeersMaskMatch),
    ))
}

fn parse_barricade_placement(
    packet: &[u8],
    player_id: u8,
) -> Result<BarricadePlacement, GameSlotDropReason> {
    validate_barricade_placement_owner(packet, player_id)?;
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

fn validate_barricade_placement_owner(
    packet: &[u8],
    player_id: u8,
) -> Result<(), GameSlotDropReason> {
    let owner_id = read_i32(packet, 37, "GameSlot barricade owner")?;
    if owner_id != i32::from(player_id) {
        return Err(GameSlotDropReason::InvalidBarricadePlacementOwner {
            player_id,
            owner_id,
        });
    }
    Ok(())
}

fn parse_barricade_transition(
    packet: &[u8],
    state: u32,
) -> Result<BarricadeTransition, GameSlotDropReason> {
    let owner_id = read_i32(packet, 37, "GameSlot barricade transition owner")?;
    let owner_id = u8::try_from(owner_id)
        .ok()
        .filter(|owner_id| *owner_id <= MAX_GAME_SLOT_PLAYER_ID)
        .ok_or(GameSlotDropReason::InvalidBarricadeTransitionOwner { owner_id })?;
    Ok(BarricadeTransition {
        object_id: read_u32(packet, 28, "GameSlot barricade transition object ID")?,
        state: u8::try_from(state).map_err(|_| {
            GameSlotDropReason::ItemOperationValidation(
                ItemOperationValidationError::InvalidLength {
                    class_name: "GopBarricade",
                    state,
                    actual: packet.len().saturating_sub(20),
                    expected: 25,
                },
            )
        })?,
        tick: read_u32(packet, 33, "GameSlot barricade transition tick")?,
        owner_id,
        // Captures prove zero for state 2 and both one and two for state 3.
        // Keep the complete bounded word instead of inventing an enum.
        trailing: read_u32(packet, 41, "GameSlot barricade transition trailing word")?,
    })
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
        GameSlotMaskRule::LowSixteenBitsExcludingSender => {
            low_only && mask & (1_u32 << player_id) == 0
        }
        GameSlotMaskRule::NonzeroLowSixteenBits => low_only && mask != 0,
        GameSlotMaskRule::NonzeroLowSixteenBitsIncludingSender => {
            low_only && mask != 0 && mask & (1_u32 << player_id) != 0
        }
        GameSlotMaskRule::NonzeroLowSixteenBitsExcludingSender => {
            low_only && mask != 0 && mask & (1_u32 << player_id) == 0
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

fn read_u16(
    packet: &[u8],
    offset: usize,
    context: &'static str,
) -> Result<u16, GameSlotDropReason> {
    Ok(u16::from_le_bytes(read_array(packet, offset, context)?))
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
    use super::{
        BarricadeOperation, GAME_KART_ITEM_INFO_HASH, GAME_KART_PACKET_HASH,
        GAME_KART_QUAD_PACKET_HASH, GAME_SLOT_PACKET_HASH, GAME_SLOT_PACKET_NAME,
        GO_ITEM_BARRICADE_HASH, GO_ITEM_CUBE_HASH, GOP_BANANA_HASH, GOP_BARRICADE_HASH,
        GOP_CUBE_HASH, GameSlotAction, GameSlotBody, GameSlotDropReason, GameSlotEvidencePending,
        GameSlotMaskRule, GameSlotRelayAudience, ItemPickupKind, ItemUseKind, KartSnapshotKind,
        MAX_GAME_SLOT_BLOB_LENGTH, MAX_GAME_SLOT_LOGICAL_LENGTH, TeamFlagTransitionKind,
        WorldObjectCollectionKind, parse_game_slot_packet,
    };
    use crate::{
        game_slot_item_schema::{ItemOperationEvidence, ItemOperationValidationError},
        packet::PacketWriter,
    };

    const PLAYER_ID: i32 = 3;
    const PLAYER_MASK: u32 = 1 << PLAYER_ID;
    const WATERBOMB_PAIR: (u32, u32) = (0x1E65_04C9, 0x2DE5_05E8);
    const WATERFLY_BASE_HASH: u32 = 0x280F_0593;

    #[test]
    fn both_pickup_types_synthesize_a_bounded_authoritative_award() {
        for (packet_type, kind) in [(1, ItemPickupKind::Type1), (2, ItemPickupKind::Type2)] {
            let mut wire = pickup_packet(packet_type, [12.5, -3.25, 0.0], -2);
            let parsed = parse_game_slot_packet(&wire).unwrap();
            assert_eq!(parsed.player_id(), 3);
            assert_eq!(parsed.item_or_recipient_mask(), u32::MAX);
            assert_eq!(parsed.action(), GameSlotAction::SynthesizeItemPickup);
            assert_eq!(parsed.body().packet_type(), packet_type);
            let GameSlotBody::ItemPickup(pickup) = parsed.body() else {
                panic!("expected item pickup");
            };
            assert_eq!(pickup.kind, kind);
            assert_eq!(
                pickup.token.object_id,
                if packet_type == 1 {
                    0xf000_0001
                } else {
                    0x00ff_ffff
                }
            );
            assert_eq!(
                pickup.token.operation_tick,
                if packet_type == 1 { 1_000 } else { 2_000 }
            );
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
            let original_tail = parsed.raw()[41..].to_vec();
            wire[..4].fill(0);
            assert_eq!(parsed.raw()[..4], original_hash);
            let award = parsed.into_item_pickup_award(111).unwrap();
            assert_eq!(award.len(), 73);
            assert_eq!(&award[..4], original_hash);
            assert_eq!(i16::from_le_bytes(award[38..40].try_into().unwrap()), 111);
            assert_eq!(award[40], 1);
            assert_eq!(&award[41..], original_tail);
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
        for packet_type in [0, 3, u8::MAX] {
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

        let mut reflected_award = pickup_packet(1, [0.0; 3], 0);
        reflected_award[40] = 1;
        assert!(matches!(
            parse_game_slot_packet(&reflected_award),
            Err(GameSlotDropReason::InvalidPickupRequestState {
                packet_type: 1,
                status: 1,
                ..
            })
        ));

        let mut mismatched_object = pickup_packet(1, [0.0; 3], 0);
        set_u32(&mut mismatched_object, 57, 0xf000_0002);
        assert!(matches!(
            parse_game_slot_packet(&mismatched_object),
            Err(GameSlotDropReason::PickupObjectIdMismatch { .. })
        ));

        let mut wrong_tick = pickup_packet(1, [0.0; 3], 0);
        set_u32(&mut wrong_tick, 69, 999);
        assert!(matches!(
            parse_game_slot_packet(&wrong_tick),
            Err(GameSlotDropReason::InvalidPickupTick {
                packet_type: 1,
                field: "first",
                ..
            })
        ));

        let mut wrong_owner = pickup_packet(2, [0.0; 3], 0);
        set_u32(&mut wrong_owner, 65, 4);
        assert!(matches!(
            parse_game_slot_packet(&wrong_owner),
            Err(GameSlotDropReason::InvalidPickupOwner {
                claimed: 3,
                nested: 4,
            })
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
    fn lucci_and_bonus_collection_wire_shapes_are_typed_evidence_pending() {
        for (packet_type, kind) in [
            (4, WorldObjectCollectionKind::Lucci),
            (6, WorldObjectCollectionKind::BonusItem),
        ] {
            let parsed =
                parse_game_slot_packet(&world_object_collection_packet(packet_type)).unwrap();
            let GameSlotBody::WorldObjectCollection(collection) = parsed.body() else {
                panic!("expected world-object collection");
            };
            assert_eq!(collection.kind, kind);
            assert_eq!(collection.object_id, 0x1122_3344);
            assert_eq!(collection.collector_id, u8::try_from(PLAYER_ID).unwrap());
            assert_eq!(collection.current_tick, 0x1020_3040);
            assert_eq!(collection.operation_tick, 0x1020_3040);
            assert_eq!(collection.variant, 7);
            assert_eq!(collection.blob.len(), 25);
            assert_eq!(
                parsed.action(),
                GameSlotAction::EvidencePending(
                    GameSlotEvidencePending::WorldObjectCollectionAuthorization(kind)
                )
            );
        }
    }

    #[test]
    fn collection_rejects_invalid_ids_states_pairs_and_non_finite_positions() {
        let mut mismatched_object = world_object_collection_packet(4);
        set_u32(&mut mismatched_object, 52, 9);
        assert!(matches!(
            parse_game_slot_packet(&mismatched_object),
            Err(GameSlotDropReason::CollectionObjectIdMismatch { .. })
        ));

        let mut wrong_state = world_object_collection_packet(6);
        set_u32(&mut wrong_state, 56, 2);
        assert!(matches!(
            parse_game_slot_packet(&wrong_state),
            Err(GameSlotDropReason::InvalidCollectionState {
                packet_type: 6,
                actual: 2
            })
        ));

        let mut wrong_collector = world_object_collection_packet(4);
        set_i32(&mut wrong_collector, 60, 16);
        assert!(matches!(
            parse_game_slot_packet(&wrong_collector),
            Err(GameSlotDropReason::InvalidCollectionCollector { packet_type: 4, .. })
        ));

        let mut unbound_collector = world_object_collection_packet(4);
        set_i32(&mut unbound_collector, 60, PLAYER_ID + 1);
        let parsed = parse_game_slot_packet(&unbound_collector).unwrap();
        let GameSlotBody::WorldObjectCollection(collection) = parsed.body() else {
            panic!("expected world-object collection");
        };
        assert_eq!(
            collection.collector_id,
            u8::try_from(PLAYER_ID + 1).unwrap()
        );

        let mut wrong_pair = world_object_collection_packet(4);
        set_u32(&mut wrong_pair, 44, super::GOP_BONUS_ITEM_HASH);
        assert!(matches!(
            parse_game_slot_packet(&wrong_pair),
            Err(GameSlotDropReason::InvalidCollectionOperation { packet_type: 4, .. })
        ));

        let mut non_finite = world_object_collection_packet(6);
        set_f32(&mut non_finite, 30, f32::NAN);
        assert!(matches!(
            parse_game_slot_packet(&non_finite),
            Err(GameSlotDropReason::NonFiniteCollectionPosition {
                packet_type: 6,
                axis: 1
            })
        ));
    }

    #[test]
    fn team_flag_types_have_exact_typed_transition_shapes() {
        for (packet_type, kind, state, length) in [
            (5, TeamFlagTransitionKind::PickupAttach, 1, 24),
            (7, TeamFlagTransitionKind::DropRelocate, 3, 32),
            (8, TeamFlagTransitionKind::ReturnReset, 4, 32),
        ] {
            let parsed = parse_game_slot_packet(&team_flag_packet(packet_type)).unwrap();
            let GameSlotBody::TeamFlagTransition(transition) = parsed.body() else {
                panic!("expected team-flag transition");
            };
            assert_eq!(transition.kind, kind);
            assert_eq!(transition.object_id, 0x1234_5678);
            assert_eq!(transition.payload.len(), length);
            assert_eq!(transition.current_tick, 1_000);
            assert_eq!(transition.expiry_tick, 2_000);
            assert_eq!(
                transition.outer_position.map(f32::to_bits),
                [1.0_f32.to_bits(), 2.0_f32.to_bits(), 3.0_f32.to_bits()]
            );
            assert_eq!(transition.transition_tick, 1_500);
            assert_eq!(
                transition.carrier_id,
                (packet_type == 5).then_some(u8::try_from(PLAYER_ID).unwrap())
            );
            assert_eq!(
                transition
                    .transition_position
                    .map(|position| position.map(f32::to_bits)),
                (packet_type != 5).then_some([
                    4.0_f32.to_bits(),
                    5.0_f32.to_bits(),
                    6.0_f32.to_bits(),
                ])
            );
            assert_eq!(
                parsed.action(),
                GameSlotAction::EvidencePending(GameSlotEvidencePending::TeamFlagAuthorization(
                    kind
                ))
            );

            let mut wrong_state = team_flag_packet(packet_type);
            set_u32(&mut wrong_state, 56, state + 1);
            assert!(matches!(
                parse_game_slot_packet(&wrong_state),
                Err(GameSlotDropReason::InvalidTeamFlagState {
                    packet_type: actual_type,
                    actual,
                    expected,
                }) if actual_type == packet_type && actual == state + 1 && expected == state
            ));
        }
    }

    #[test]
    fn team_flag_transitions_reject_spoofed_objects_carriers_and_positions() {
        let mut wrong_pair = team_flag_packet(5);
        set_u32(&mut wrong_pair, 44, 0);
        assert!(matches!(
            parse_game_slot_packet(&wrong_pair),
            Err(GameSlotDropReason::InvalidTeamFlagOperation { packet_type: 5, .. })
        ));

        let mut wrong_object = team_flag_packet(7);
        set_u32(&mut wrong_object, 52, 9);
        assert!(matches!(
            parse_game_slot_packet(&wrong_object),
            Err(GameSlotDropReason::TeamFlagObjectIdMismatch { packet_type: 7, .. })
        ));

        let mut wrong_carrier = team_flag_packet(5);
        set_i32(&mut wrong_carrier, 60, 16);
        assert!(matches!(
            parse_game_slot_packet(&wrong_carrier),
            Err(GameSlotDropReason::InvalidTeamFlagCarrier {
                packet_type: 5,
                carrier: 16,
            })
        ));

        let mut non_finite_outer = team_flag_packet(8);
        set_f32(&mut non_finite_outer, 30, f32::NAN);
        assert!(matches!(
            parse_game_slot_packet(&non_finite_outer),
            Err(GameSlotDropReason::NonFiniteTeamFlagPosition {
                packet_type: 8,
                axis: 1,
            })
        ));

        let mut non_finite_transition = team_flag_packet(7);
        set_f32(&mut non_finite_transition, 64, f32::INFINITY);
        assert!(matches!(
            parse_game_slot_packet(&non_finite_transition),
            Err(GameSlotDropReason::NonFiniteTeamFlagPosition {
                packet_type: 7,
                axis: 4,
            })
        ));
    }

    #[test]
    fn item_vector_has_a_bounded_typed_item_list_and_peer_relay() {
        for items in [vec![], vec![7], vec![7, 11], vec![7, 11, 13]] {
            let wire = item_vector_packet(PLAYER_MASK | 1, &items);
            let parsed = parse_game_slot_packet(&wire).unwrap();
            assert_eq!(
                parsed.action(),
                GameSlotAction::RelayOriginal(GameSlotRelayAudience::RecipientMaskExceptSender)
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
        let item_use = parse_game_slot_packet(&item_use_packet(
            10, 0, 7, 0x1002, 0xFF85, 9, 10, 0x1234, &use_blob,
        ))
        .unwrap();
        let GameSlotBody::ItemUse(fields) = item_use.body() else {
            panic!("expected item use");
        };
        assert_eq!(fields.kind, ItemUseKind::Ordinary);
        assert_eq!(fields.raw_byte_13, 7);
        assert_eq!(fields.status, 0x1002);
        assert_eq!(fields.item_or_skill, 0xFF85);
        assert_eq!((fields.raw_byte_18, fields.raw_byte_19), (9, 10));
        assert_eq!(fields.ancillary_effect_code, 0x1234);
        assert_eq!(item_use.payload(), Some(use_blob.as_slice()));
        assert_eq!(
            item_use.action(),
            GameSlotAction::RelayOriginal(GameSlotRelayAudience::RecipientMaskExceptSender)
        );

        let type_16 =
            parse_game_slot_packet(&item_use_packet(16, 0, 7, 1, 0x71, 0, 0, 0, &use_blob))
                .unwrap();
        let GameSlotBody::ItemUse(fields) = type_16.body() else {
            panic!("expected spawned-world-object item use");
        };
        assert_eq!(fields.kind, ItemUseKind::SpawnedWorldObject);
        assert_eq!(
            type_16.action(),
            GameSlotAction::RelayOriginal(GameSlotRelayAudience::RecipientMaskExceptSender)
        );

        let reaction_blob = [0xAA, 0x55];
        let reaction =
            parse_game_slot_packet(&item_reaction_packet(1, 4, 9, &reaction_blob)).unwrap();
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
        assert!(parse_game_slot_packet(&item_use_packet(10, 0, 0, 0, 0, 0, 0, 0, &[])).is_ok());
        assert!(matches!(
            parse_game_slot_packet(&item_use_packet(10, 1 << 16, 0, 0, 0, 0, 0, 0, &[])),
            Err(GameSlotDropReason::InvalidMask {
                packet_type: 10,
                rule: GameSlotMaskRule::LowSixteenBitsExcludingSender,
                ..
            })
        ));
        assert!(matches!(
            parse_game_slot_packet(&item_use_packet(16, PLAYER_MASK, 0, 0, 0, 0, 0, 0, &[])),
            Err(GameSlotDropReason::InvalidMask {
                packet_type: 16,
                rule: GameSlotMaskRule::LowSixteenBitsExcludingSender,
                ..
            })
        ));
        for mask in [0, PLAYER_MASK, 1 << 16] {
            assert!(matches!(
                parse_game_slot_packet(&item_reaction_packet(mask, 0, 0, &[])),
                Err(GameSlotDropReason::InvalidMask {
                    packet_type: 11,
                    rule: GameSlotMaskRule::NonzeroLowSixteenBitsExcludingSender,
                    ..
                })
            ));
        }

        let maximum_blob = vec![0x5a; MAX_GAME_SLOT_BLOB_LENGTH];
        assert!(
            parse_game_slot_packet(&item_use_packet(10, 1, 0, 0, 0, 0, 0, 0, &maximum_blob,))
                .is_ok()
        );
        let oversized_blob = vec![0; MAX_GAME_SLOT_BLOB_LENGTH + 1];
        assert!(matches!(
            parse_game_slot_packet(&item_use_packet(10, 1, 0, 0, 0, 0, 0, 0, &oversized_blob,)),
            Err(GameSlotDropReason::BlobLengthOverCap {
                packet_type: 10,
                declared: 961,
                maximum: MAX_GAME_SLOT_BLOB_LENGTH,
            })
        ));

        let mut trailing = item_reaction_packet(1, 0, 0, &[1]);
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
    fn retained_quad_snapshot_is_typed_and_relayed_to_other_masked_peers() {
        let wire = hex_bytes(
            "7405C0270000000002000000116000004C000000EF0660403659150303808CFF36C0439B87F043988A7D4168FFEF7F0000000000000000000000000000000000000000000000007200000000000018000000784D7CCD00000000000000000000",
        );
        let parsed = parse_game_slot_packet(&wire).unwrap();
        let GameSlotBody::KartSnapshot(snapshot) = parsed.body() else {
            panic!("expected kart snapshot");
        };
        assert_eq!(snapshot.kind, KartSnapshotKind::QuantizedQuad);
        assert_eq!(snapshot.common, 0x60);
        assert_eq!(snapshot.object_id, 0x0315_5936);
        assert_eq!(snapshot.flags, [0x03, 0x80, 0x8c]);
        assert_eq!(snapshot.payload.len(), 76);
        assert_eq!(parsed.payload(), Some(&wire[20..]));
        assert_eq!(
            parsed.action(),
            GameSlotAction::RelayOriginal(GameSlotRelayAudience::RecipientMaskExceptSender)
        );
        assert_eq!(parsed.raw(), wire);
    }

    #[test]
    fn both_kart_snapshot_codecs_enforce_the_native_flag_length_formula() {
        for (hash, kind, base, secondary, scalar) in [
            (
                GAME_KART_PACKET_HASH,
                KartSnapshotKind::FullPrecision,
                116,
                16,
                4,
            ),
            (
                GAME_KART_QUAD_PACKET_HASH,
                KartSnapshotKind::QuantizedQuad,
                76,
                4,
                2,
            ),
        ] {
            for flags in [[0, 0, 0], [0, 1, 0], [4, 0, 0], [8, 0, 0], [12, 7, 0]] {
                let wire = kart_snapshot_packet(hash, flags);
                let parsed = parse_game_slot_packet(&wire).unwrap();
                let GameSlotBody::KartSnapshot(snapshot) = parsed.body() else {
                    panic!("expected kart snapshot");
                };
                let expected = base
                    + usize::from(flags[1] & 7 != 0) * secondary
                    + usize::from(flags[0] & 4 != 0) * scalar
                    + usize::from(flags[0] & 8 != 0) * scalar;
                assert_eq!(snapshot.kind, kind);
                assert_eq!(snapshot.payload.len(), expected);

                let mut drifted = wire;
                drifted[28] ^= 4;
                let toggled_expected = if flags[0] & 4 == 0 {
                    expected + scalar
                } else {
                    expected - scalar
                };
                assert!(matches!(
                    parse_game_slot_packet(&drifted),
                    Err(GameSlotDropReason::InvalidKartSnapshotLength {
                        kind: actual_kind,
                        expected: actual_expected,
                        ..
                    }) if actual_kind == kind && actual_expected == toggled_expected
                ));
            }
        }
    }

    #[test]
    fn kart_snapshot_rejects_spoofed_mask_reserved_hash_and_blob_length() {
        let base = kart_snapshot_packet(GAME_KART_QUAD_PACKET_HASH, [0; 3]);

        let mut sender_in_mask = base.clone();
        set_u32(&mut sender_in_mask, 8, PLAYER_MASK | 1);
        assert!(matches!(
            parse_game_slot_packet(&sender_in_mask),
            Err(GameSlotDropReason::InvalidMask {
                packet_type: 17,
                rule: GameSlotMaskRule::NonzeroLowSixteenBitsExcludingSender,
                ..
            })
        ));

        let mut reserved = base.clone();
        reserved[14..16].copy_from_slice(&1_u16.to_le_bytes());
        assert!(matches!(
            parse_game_slot_packet(&reserved),
            Err(GameSlotDropReason::InvalidKartSnapshotReservedWord { actual: 1 })
        ));

        let mut unknown_hash = base.clone();
        set_u32(&mut unknown_hash, 20, 0xDEAD_BEEF);
        assert!(matches!(
            parse_game_slot_packet(&unknown_hash),
            Err(GameSlotDropReason::UnexpectedKartSnapshotHash {
                actual: 0xDEAD_BEEF
            })
        ));

        let mut trailing = base;
        trailing.push(0);
        assert!(matches!(
            parse_game_slot_packet(&trailing),
            Err(GameSlotDropReason::BlobLengthMismatch {
                packet_type: 17,
                declared: 76,
                actual: 77,
            })
        ));
    }

    #[test]
    fn dashboard_allocation_notice_is_an_exact_empty_no_reply_event() {
        let wire = dashboard_allocation_packet(1);
        let parsed = parse_game_slot_packet(&wire).unwrap();
        let GameSlotBody::DashboardAllocationNotice(notice) = parsed.body() else {
            panic!("expected dashboard allocation notice");
        };
        assert_eq!(notice.raw_byte_13, 0x7e);
        assert!(notice.payload.is_empty());
        assert_eq!(parsed.action(), GameSlotAction::ConsumeNoReply);
        assert_eq!(parsed.payload(), Some(&[][..]));

        let mut reserved = wire.clone();
        reserved[14..16].copy_from_slice(&1_u16.to_le_bytes());
        assert!(matches!(
            parse_game_slot_packet(&reserved),
            Err(GameSlotDropReason::InvalidDashboardAllocationReservedWord { actual: 1 })
        ));

        let mut body = wire.clone();
        body.push(0xaa);
        set_u32(&mut body, 16, 1);
        assert!(matches!(
            parse_game_slot_packet(&body),
            Err(GameSlotDropReason::InvalidDashboardAllocationBodyLength { actual: 1 })
        ));

        let mut sender_mask = wire;
        set_u32(&mut sender_mask, 8, PLAYER_MASK);
        assert!(matches!(
            parse_game_slot_packet(&sender_mask),
            Err(GameSlotDropReason::InvalidMask {
                packet_type: 13,
                rule: GameSlotMaskRule::LowSixteenBitsExcludingSender,
                ..
            })
        ));
    }

    #[test]
    fn retained_type_twelve_shapes_are_strict_relay_capabilities() {
        let fixtures = [
            operation_state_packet(0, (0x1090_0367, 0x1CB3_0486), 2, 30),
            operation_state_packet(PLAYER_MASK, (0x0A6B_02AF, 0x1450_03CE), 2, 29),
            course_packet(PLAYER_MASK, 0, 4),
            operation_state_packet(PLAYER_MASK, (0x1129_038E, 0x1D4C_04AD), 2, 73),
            barricade_packet(),
            barricade_transition_packet(2),
            barricade_transition_packet(3),
        ];
        for wire in fixtures {
            let parsed = parse_game_slot_packet(&wire).unwrap();
            let GameSlotBody::ItemOperation(operation) = parsed.body() else {
                panic!("expected item operation");
            };
            assert_eq!(operation.evidence, ItemOperationEvidence::RetainedTrace);
            assert_ne!(operation.object_id, u32::MAX);
            assert_eq!(
                parsed.action(),
                GameSlotAction::RelayOriginal(GameSlotRelayAudience::AllRacePeersMaskMatch)
            );
        }
    }

    #[test]
    fn static_writer_type_twelve_shapes_relay_after_common_validation() {
        for wire in [
            operation_state_packet(PLAYER_MASK, WATERBOMB_PAIR, 1, 125),
            operation_state_packet(
                PLAYER_MASK,
                (super::GOP_CUBE_HASH, super::GO_ITEM_CUBE_HASH),
                1,
                24,
            ),
            operation_state_packet(
                PLAYER_MASK,
                (super::GOP_LUCCI_HASH, super::GO_LUCCI_HASH),
                1,
                25,
            ),
        ] {
            let parsed = parse_game_slot_packet(&wire).unwrap();
            let GameSlotBody::ItemOperation(operation) = parsed.body() else {
                panic!("expected item operation");
            };
            assert_ne!(operation.evidence, ItemOperationEvidence::RetainedTrace);
            assert_eq!(
                parsed.action(),
                GameSlotAction::RelayOriginal(GameSlotRelayAudience::AllRacePeersMaskMatch)
            );
        }
    }

    #[test]
    fn type_twelve_rejects_spoofed_envelopes_and_unknown_pairs_but_relays_known_fallbacks() {
        assert!(matches!(
            parse_game_slot_packet(&operation_state_packet(1 << 16, WATERBOMB_PAIR, 1, 125,)),
            Err(GameSlotDropReason::InvalidMask {
                packet_type: 12,
                rule: GameSlotMaskRule::LowSixteenBits,
                ..
            })
        ));

        let mut nonzero_reserved = operation_state_packet(PLAYER_MASK, WATERBOMB_PAIR, 1, 125);
        nonzero_reserved[14..16].copy_from_slice(&0xBEEF_u16.to_le_bytes());
        assert!(matches!(
            parse_game_slot_packet(&nonzero_reserved),
            Err(GameSlotDropReason::InvalidItemOperationReservedWord { actual: 0xBEEF })
        ));

        assert!(matches!(
            parse_game_slot_packet(&operation_state_packet(
                PLAYER_MASK,
                (WATERBOMB_PAIR.0, WATERFLY_BASE_HASH),
                1,
                125,
            )),
            Err(GameSlotDropReason::UnsupportedItemOperation { .. })
        ));

        let extra_name_derived_pair = (0x14E9_03F7, 0x222B_0516);
        for wire in [
            operation_state_packet(PLAYER_MASK, extra_name_derived_pair, 1, 16),
            operation_state_packet(PLAYER_MASK, (0x1129_038E, 0x1D4C_04AD), 1, 73),
        ] {
            let parsed = parse_game_slot_packet(&wire).unwrap();
            assert!(matches!(
                parsed.body(),
                GameSlotBody::BoundedItemOperation(_)
            ));
            assert_eq!(
                parsed.action(),
                GameSlotAction::RelayOriginal(GameSlotRelayAudience::AllRacePeersMaskMatch)
            );
        }

        let mut missing_id = operation_state_packet(PLAYER_MASK, WATERBOMB_PAIR, 1, 125);
        set_u32(&mut missing_id, 28, u32::MAX);
        let parsed = parse_game_slot_packet(&missing_id).unwrap();
        assert!(matches!(
            parsed.body(),
            GameSlotBody::BoundedItemOperation(_)
        ));
        assert_eq!(
            parsed.action(),
            GameSlotAction::RelayOriginal(GameSlotRelayAudience::AllRacePeersMaskMatch)
        );

        let mut trailing = operation_state_packet(PLAYER_MASK, WATERBOMB_PAIR, 1, 125);
        trailing.push(0);
        assert!(matches!(
            parse_game_slot_packet(&trailing),
            Err(GameSlotDropReason::BlobLengthMismatch {
                packet_type: 12,
                declared: 125,
                actual: 126,
            })
        ));

        let mut over_cap = operation_state_packet(PLAYER_MASK, WATERBOMB_PAIR, 1, 125);
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
    fn barricade_body_is_typed_finite_and_trace_confirmed() {
        let wire = barricade_packet();
        let parsed = parse_game_slot_packet(&wire).unwrap();
        let GameSlotBody::ItemOperation(operation) = parsed.body() else {
            panic!("expected item operation");
        };
        let Some(BarricadeOperation::Placement(placement)) = operation.barricade else {
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
            GameSlotAction::RelayOriginal(GameSlotRelayAudience::AllRacePeersMaskMatch)
        );
    }

    #[test]
    fn retained_remote_barricade_hit_preserves_installer_distinct_from_reporter() {
        let wire = hex_bytes(
            "7405C02701000000010000000CFF000019000000A304861DC205062D48000010025B3804000000000000000000",
        );
        let parsed = parse_game_slot_packet(&wire).unwrap();
        assert_eq!(parsed.player_id(), 1);
        let GameSlotBody::ItemOperation(operation) = parsed.body() else {
            panic!("expected item operation");
        };
        let Some(BarricadeOperation::Transition(transition)) = operation.barricade else {
            panic!("expected barricade transition");
        };
        assert_eq!(transition.object_id, 0x1000_0048);
        assert_eq!(transition.state, 2);
        assert_eq!(transition.owner_id, 0);
        assert_eq!(transition.tick, 0x0004_385B);
        assert_eq!(transition.trailing, 0);
    }

    #[test]
    fn barricade_marker_owner_reserved_and_every_transform_float_are_validated() {
        let mut wrong_marker = barricade_packet();
        wrong_marker[32] = 0;
        assert!(matches!(
            parse_game_slot_packet(&wrong_marker),
            Err(GameSlotDropReason::ItemOperationValidation(
                ItemOperationValidationError::InvalidLength {
                    class_name: "GopBarricade",
                    state: 0,
                    actual: 73,
                    expected: 25,
                }
            ))
        ));

        let mut wrong_owner = barricade_packet();
        set_i32(&mut wrong_owner, 37, 2);
        assert!(matches!(
            parse_game_slot_packet(&wrong_owner),
            Err(GameSlotDropReason::InvalidBarricadePlacementOwner {
                player_id: 3,
                owner_id: 2,
            })
        ));

        for state in [2, 3] {
            let mut bounded_transition_owner = barricade_transition_packet(state);
            set_i32(&mut bounded_transition_owner, 37, 1);
            let parsed = parse_game_slot_packet(&bounded_transition_owner).unwrap();
            let GameSlotBody::ItemOperation(operation) = parsed.body() else {
                panic!("expected item operation");
            };
            let Some(BarricadeOperation::Transition(transition)) = operation.barricade else {
                panic!("expected barricade transition");
            };
            assert_eq!(transition.owner_id, 1);
            assert_eq!(u32::from(transition.state), state);

            set_i32(&mut bounded_transition_owner, 37, 16);
            assert!(matches!(
                parse_game_slot_packet(&bounded_transition_owner),
                Err(GameSlotDropReason::InvalidBarricadeTransitionOwner { owner_id: 16 })
            ));
        }

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
            world_object_collection_packet(4),
            team_flag_packet(5),
            world_object_collection_packet(6),
            team_flag_packet(7),
            team_flag_packet(8),
            item_vector_packet(PLAYER_MASK, &[1, 2, 3]),
            item_use_packet(10, PLAYER_MASK, 1, 2, 3, 4, 5, 6, &[7, 8]),
            item_use_packet(16, 0, 1, 2, 3, 4, 5, 6, &[7, 8]),
            item_reaction_packet(1, 1, 2, &[3, 4]),
            dashboard_allocation_packet(1),
            kart_snapshot_packet(GAME_KART_PACKET_HASH, [0; 3]),
            kart_snapshot_packet(GAME_KART_QUAD_PACKET_HASH, [12, 7, 0]),
            operation_state_packet(PLAYER_MASK, WATERBOMB_PAIR, 1, 125),
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
        let (object_id, first_tick, second_tick, operation_tick) = if packet_type == 1 {
            (0xf000_0001_u32, 1_000_u32, 2_500_u32, 1_000_u32)
        } else {
            (0x00ff_ffff_u32, 1_000_u32, 1_000_u32, 2_000_u32)
        };
        context[1..5].copy_from_slice(&object_id.to_le_bytes());
        context[5..9].copy_from_slice(&first_tick.to_le_bytes());
        context[9..13].copy_from_slice(&second_tick.to_le_bytes());
        for (index, value) in position.into_iter().enumerate() {
            context[13 + index * 4..17 + index * 4].copy_from_slice(&value.to_le_bytes());
        }
        packet.write_bytes(&context);
        packet.write_i16(live_rank);
        if packet_type == 1 {
            packet.write_u8(0);
            packet.write_u32(0x0000_ffff);
        } else {
            packet.write_u8(8);
            packet.write_u32(10);
        }
        packet.write_u32(24);
        packet.write_u32(GOP_CUBE_HASH);
        packet.write_u32(GO_ITEM_CUBE_HASH);
        packet.write_u32(object_id);
        packet.write_u32(1);
        packet.write_u32(u32::try_from(PLAYER_ID).unwrap());
        packet.write_u32(operation_tick);
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

    #[allow(clippy::too_many_arguments)]
    fn item_use_packet(
        packet_type: u8,
        mask: u32,
        common: u8,
        status: u16,
        item_or_skill: u16,
        raw_byte_18: u8,
        raw_byte_19: u8,
        ancillary_effect_code: u16,
        blob: &[u8],
    ) -> Vec<u8> {
        let mut packet = common_packet(PLAYER_ID, mask, packet_type);
        packet.write_u8(common);
        packet.write_u16(status);
        packet.write_u16(item_or_skill);
        packet.write_u8(raw_byte_18);
        packet.write_u8(raw_byte_19);
        packet.write_u16(ancillary_effect_code);
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

    fn kart_snapshot_packet(hash: u32, flags: [u8; 3]) -> Vec<u8> {
        let (base, secondary, scalar) = match hash {
            GAME_KART_PACKET_HASH => (116, 16, 4),
            GAME_KART_QUAD_PACKET_HASH => (76, 4, 2),
            _ => panic!("test helper requires a known kart snapshot hash"),
        };
        let length = base
            + usize::from(flags[1] & 7 != 0) * secondary
            + usize::from(flags[0] & 4 != 0) * scalar
            + usize::from(flags[0] & 8 != 0) * scalar;
        let mut payload = vec![0; length];
        payload[..4].copy_from_slice(&hash.to_le_bytes());
        payload[4..8].copy_from_slice(&0x1020_3040_u32.to_le_bytes());
        payload[8..11].copy_from_slice(&flags);

        let mut packet = common_packet(PLAYER_ID, 1, 17);
        packet.write_u8(0x5a);
        packet.write_u16(0);
        packet.write_u32(u32::try_from(payload.len()).unwrap());
        packet.write_bytes(&payload);
        packet.into_inner()
    }

    fn dashboard_allocation_packet(mask: u32) -> Vec<u8> {
        let mut packet = common_packet(PLAYER_ID, mask, 13);
        packet.write_u8(0x7e);
        packet.write_u16(0);
        packet.write_u32(0);
        packet.into_inner()
    }

    fn hex_bytes(text: &str) -> Vec<u8> {
        let compact: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
        assert_eq!(compact.len() % 2, 0);
        (0..compact.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&compact[index..index + 2], 16).unwrap())
            .collect()
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

    fn operation_state_packet(mask: u32, pair: (u32, u32), state: u32, length: usize) -> Vec<u8> {
        assert!(length >= 16);
        let mut packet = operation_packet(mask, pair, length);
        set_u32(&mut packet, 28, 1);
        set_u32(&mut packet, 32, state);
        packet
    }

    fn barricade_transition_packet(state: u32) -> Vec<u8> {
        let mut packet =
            operation_state_packet(0, (GOP_BARRICADE_HASH, GO_ITEM_BARRICADE_HASH), state, 25);
        set_i32(&mut packet, 37, PLAYER_ID);
        match state {
            2 => set_u32(&mut packet, 41, 0),
            3 => set_u32(&mut packet, 41, 2),
            _ => panic!("captured barricade transition state must be 2 or 3"),
        }
        packet
    }

    fn course_packet(mask: u32, state: u32, count: u32) -> Vec<u8> {
        let count_usize = usize::try_from(count).unwrap();
        let mut packet = operation_state_packet(
            mask,
            (super::GOP_COURSE_HASH, super::GO_COURSE_HASH),
            state,
            24 + count_usize * 2,
        );
        set_u32(&mut packet, 36, count);
        packet
    }

    fn world_object_collection_packet(packet_type: u8) -> Vec<u8> {
        let (operation_hash, base_hash) = if packet_type == 4 {
            (super::GOP_LUCCI_HASH, super::GO_LUCCI_HASH)
        } else {
            (super::GOP_BONUS_ITEM_HASH, super::GO_BONUS_ITEM_HASH)
        };
        let mut packet = common_packet(PLAYER_ID, u32::MAX, packet_type);
        packet.write_u8(0);
        packet.write_u32(0x1122_3344);
        packet.write_u32(0x1020_3040);
        packet.write_u32(0x5060_7080);
        packet.write_f32(1.25);
        packet.write_f32(-2.5);
        packet.write_f32(3.75);
        packet.write_u16(0x55AA);
        packet.write_u32(25);
        packet.write_u32(operation_hash);
        packet.write_u32(base_hash);
        packet.write_u32(0x1122_3344);
        packet.write_u32(1);
        packet.write_i32(PLAYER_ID);
        packet.write_u32(0x1020_3040);
        packet.write_u8(7);
        packet.into_inner()
    }

    fn team_flag_packet(packet_type: u8) -> Vec<u8> {
        let (state, payload_length) = match packet_type {
            5 => (1, 24),
            7 => (3, 32),
            8 => (4, 32),
            _ => panic!("team-flag fixture requires type 5, 7, or 8"),
        };
        let mut packet = common_packet(PLAYER_ID, u32::MAX, packet_type);
        packet.write_u8(0);
        packet.write_u32(0x1234_5678);
        packet.write_u32(1_000);
        packet.write_u32(2_000);
        for value in [1.0, 2.0, 3.0] {
            packet.write_f32(value);
        }
        packet.write_u16(0x55aa);
        packet.write_u32(payload_length);
        packet.write_u32(super::GOP_TEAM_FLAG_HASH);
        packet.write_u32(super::GO_TEAM_FLAG_HASH);
        packet.write_u32(0x1234_5678);
        packet.write_u32(state);
        match packet_type {
            5 => {
                packet.write_i32(PLAYER_ID);
                packet.write_u32(1_500);
            }
            7 => {
                for value in [4.0, 5.0, 6.0] {
                    packet.write_f32(value);
                }
                packet.write_u32(1_500);
            }
            8 => {
                packet.write_u32(1_500);
                for value in [4.0, 5.0, 6.0] {
                    packet.write_f32(value);
                }
            }
            _ => unreachable!(),
        }
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
