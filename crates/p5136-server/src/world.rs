//! Deterministic, actor-owned server state.

use std::{
    array,
    collections::{BTreeSet, HashMap, HashSet, VecDeque},
    fmt,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    num::{NonZeroU64, NonZeroUsize},
    sync::Arc,
    time::{Duration, Instant},
};

use p5136_core::{
    equipment_protocol::{EquipmentProtocolError, serialize_room_slot_items},
    lobby_protocol::{
        LobbyProtocolError, PlayerSlotState, RoomTeam, StartRoomStatus,
        serialize_change_team_reply, serialize_set_slot_state_reply, serialize_slot_state,
        serialize_start_room_reply,
    },
    myroom_protocol::{
        CharacterPositionRequest, MyRoomInfo, MyRoomProtocolError, MyRoomSlot,
        serialize_character_position, serialize_missing_owner_items, serialize_myroom_info,
        serialize_secede_reply, serialize_slot_data,
    },
    nickname::canonical_nickname_key,
    race_protocol::{
        AiGoalInRequest, GameControlRequest, RaceProtocolError, RaceTeam, ServerGameControl,
        TeamBoosterGaugeRequest, serialize_ai_master_notice, serialize_game_control,
        serialize_game_next_stage, serialize_race_time, serialize_team_booster_gauge,
    },
    race_result_protocol::{
        AiRaceResult, GameResult, HumanRaceResult, RaceResultProtocolError, ResultTeam,
        serialize_game_result,
    },
    race_start_protocol::{
        AiRaceSpec, GrCommandStart, MAX_GR_COMMAND_START_PAYLOAD_LENGTH, P5136KartPhysicsBlock,
        RaceStartProtocolError, serialize_gr_command_start_bounded,
    },
    room_protocol::{
        ChCreateRoomRequest, ChGetRoomListRequest, ChJoinRoomRequest, CreateRoomOutcome,
        JoinRoomStatus, ROOM_DATA_LENGTH, ROOM_OBSERVER_COUNT, ROOM_SLOT_COUNT, RoomListEntry,
        RoomMember as WireRoomMember, RoomObserver, RoomObserverSlot, RoomPlayer,
        RoomProtocolError, RoomSessionData, RoomSlotData, serialize_ch_create_room_reply,
        serialize_ch_get_room_list_reply, serialize_ch_join_room_reply,
        serialize_ch_leave_room_reply, serialize_gr_slot_data, serialize_initial_room_state,
    },
    track::is_random_track_selector,
};
use p5136_profile::{
    AppliedTimeReward, GlobalRaceEpoch, MAX_TIME_REWARD_LUCCI_ROLL, MAX_TIME_REWARD_RP_ROLL,
    TimeReward, time_reward_from_rolls,
};
use rand::Rng;
use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::MissedTickBehavior,
};

use crate::equipment_persistence::{
    DurableRiderEquipment, PreparedRiderEquipmentWrite, RegisteredRiderEquipmentWrite,
    RiderEquipmentProfileCompletion, RiderEquipmentProfileJobResult, RiderEquipmentPublication,
    RiderEquipmentPublicationInvariantError, RiderEquipmentWriteError, RiderEquipmentWriteReceipt,
};
use crate::identity::{
    ChannelBinding, DisconnectOutcome, IdentityBinding, IdentityError, IdentityGeneration,
    IdentityOperationLease, IdentityRegistry, IdentityRegistryInstance, MigrationCompletion,
    MigrationPermit, MigrationPreflight, MigrationToken, ReleasedIdentity, UserNo,
};
use crate::messenger_hub::MessengerIdentity;
use crate::messenger_runtime::{MessengerServiceError, MessengerServiceHandle};
use crate::myroom_hub::{
    MyRoomCommitError, MyRoomDisconnectOutcome, MyRoomHub, MyRoomHubError, MyRoomOwnerItemPlan,
    MyRoomTransition, MyRoomWirePlan, MyRoomWireProjection, MyRoomWireProjectionError,
    RoomEffect as MyRoomEffect, RoomPublication as MyRoomPublication,
};
use crate::myroom_persistence::{
    MigrationAcknowledgement, MigrationProfileCompletion, MyRoomCompletionBridge,
    MyRoomCompletionDrainError, MyRoomCompletionSlot, MyRoomInfoPublication, MyRoomInfoWriteError,
    MyRoomInfoWriteReceipt, MyRoomPersistenceInvariantError, MyRoomProfileCompletion,
    MyRoomProfileJobResult, MyRoomProfileTicketId, PreparedMyRoomInfoWrite,
    RegisteredMyRoomInfoWrite,
};
use crate::operation_gate::{WireOperationGate, WireOperationGuard};
use crate::profile_io::{
    DurableRewardReceipt, MyRoomProfileLease, ProfileJobAdmission, ProfileLanePermit,
};
use crate::udp_runtime::{
    ServerClock, UdpDispatchAction, UdpDispatchOutcome, UdpDispatchRequest, UdpIngress,
    UdpIngressBody, UdpService, UdpServiceError,
};
use crate::udp_state::UdpEndpointStateError;

pub const ROOM_CAPACITY: usize = 8;
pub(crate) const SESSION_OUTBOUND_CAPACITY: usize = 64;
const DEFAULT_WORLD_IDENTITY_CAPACITY: usize = 256;
const LOADING_READY_TIMEOUT: Duration = Duration::from_secs(30);
const RACE_START_DELAY: Duration = Duration::from_secs(1);
const LOADING_HEARTBEAT_INTERVAL: Duration = Duration::from_millis(100);
const RACE_START_TICK_LEAD: u32 = 3_000;
const SETTLEMENT_DELAY: Duration = Duration::from_secs(10);
const SETTLEMENT_TICK_LEAD: u32 = 10_000;
const FINAL_STAGE_TICK_LEAD: u32 = 6_000;
const TEAM_POINTS_BY_RANK: [i32; ROOM_SLOT_COUNT] = [10, 8, 6, 5, 4, 3, 2, 1];
const MAX_REWARD_PERSISTENCE_FAILURES: u8 = 8;
const REWARD_RETRY_BASE_DELAY: Duration = Duration::from_millis(100);
const REWARD_RETRY_MAX_DELAY: Duration = Duration::from_secs(5);
const REWARD_ATTEMPT_LEASE: Duration = Duration::from_secs(30);
const MAX_DUE_REWARD_TASK_BATCH: usize = 64;

trait RewardRollSource {
    fn draw_rp(&mut self) -> u8;
    fn draw_lucci(&mut self) -> u16;
}

struct RandomRewardRollSource;

impl RewardRollSource for RandomRewardRollSource {
    fn draw_rp(&mut self) -> u8 {
        rand::rng().random_range(0..=MAX_TIME_REWARD_RP_ROLL)
    }

    fn draw_lucci(&mut self) -> u16 {
        rand::rng().random_range(0..=MAX_TIME_REWARD_LUCCI_ROLL)
    }
}

/// One ordered write unit for a login session. A batch consumes one bounded
/// queue slot even when a protocol response contains many logical packets.
#[derive(Debug)]
pub(crate) struct OutboundBatch {
    packets: Vec<Vec<u8>>,
    operation: Option<WireOperationGuard>,
}

impl OutboundBatch {
    #[must_use]
    pub(crate) fn single(packet: Vec<u8>) -> Self {
        Self {
            packets: vec![packet],
            operation: None,
        }
    }

    #[must_use]
    pub(crate) fn ordered(packets: Vec<Vec<u8>>) -> Self {
        Self {
            packets,
            operation: None,
        }
    }

    fn duplicate(&self) -> Self {
        Self::ordered(self.packets.clone())
    }

    fn track(mut self, operation: WireOperationGuard) -> Self {
        debug_assert!(
            self.operation.is_none(),
            "one outbound batch owns at most one write operation"
        );
        self.operation = Some(operation);
        self
    }

    #[cfg(test)]
    pub(crate) fn track_for_test(self, operation: WireOperationGuard) -> Self {
        self.track(operation)
    }

    pub(crate) fn into_write_parts(self) -> (Vec<Vec<u8>>, Option<WireOperationGuard>) {
        (self.packets, self.operation)
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn into_packets(self) -> Vec<Vec<u8>> {
        self.packets
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(u64);

impl SessionId {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RoomId(pub u32);

/// The exact actor-owned race incarnation used to fence reward work.
///
/// Room IDs are deliberately reusable. A room ID by itself therefore cannot
/// authorize a persistence completion or release a user's outstanding reward
/// lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RaceFence {
    room_id: RoomId,
    race_epoch: GlobalRaceEpoch,
}

impl RaceFence {
    const fn new(room_id: RoomId, race_epoch: GlobalRaceEpoch) -> Self {
        Self {
            room_id,
            race_epoch,
        }
    }

    #[must_use]
    pub const fn room_id(self) -> RoomId {
        self.room_id
    }

    #[must_use]
    pub const fn race_epoch(self) -> GlobalRaceEpoch {
        self.race_epoch
    }
}

/// Unique identifier for one scheduled persistence attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct RewardAttemptId(NonZeroU64);

impl RewardAttemptId {
    #[must_use]
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "read-only persistence boundary accessor")
    )]
    pub(crate) const fn get(self) -> u64 {
        self.0.get()
    }
}

/// One immutable persistence proposal taken from the World actor.
///
/// Retrying a failed task creates a new [`RewardAttemptId`] but preserves
/// every other field, especially `proposed_reward`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RewardSettlementTask {
    fence: RaceFence,
    attempt_id: RewardAttemptId,
    user_no: UserNo,
    nickname: String,
    canonical_nickname: String,
    proposed_reward: TimeReward,
}

impl RewardSettlementTask {
    #[must_use]
    pub(crate) const fn fence(&self) -> RaceFence {
        self.fence
    }

    #[must_use]
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "read-only persistence boundary accessor")
    )]
    pub(crate) const fn attempt_id(&self) -> RewardAttemptId {
        self.attempt_id
    }

    #[must_use]
    pub(crate) const fn user_no(&self) -> UserNo {
        self.user_no
    }

    #[must_use]
    pub(crate) fn nickname(&self) -> &str {
        &self.nickname
    }

    #[must_use]
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "read-only persistence boundary accessor")
    )]
    pub(crate) fn canonical_nickname(&self) -> &str {
        &self.canonical_nickname
    }

    #[must_use]
    pub(crate) const fn proposed_reward(&self) -> TimeReward {
        self.proposed_reward
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        room_id: RoomId,
        race_epoch: GlobalRaceEpoch,
        attempt_id: NonZeroU64,
        user_no: UserNo,
        nickname: &str,
        proposed_reward: TimeReward,
    ) -> Self {
        Self {
            fence: RaceFence::new(room_id, race_epoch),
            attempt_id: RewardAttemptId(attempt_id),
            user_no,
            nickname: nickname.to_owned(),
            canonical_nickname: canonical_nickname_key(nickname),
            proposed_reward,
        }
    }
}

#[derive(Debug)]
// This actor boundary is consumed by the profile worker in the next
// integration tranche; tests exercise every variant in the meantime.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum RewardPersistenceCompletion {
    Durable(Box<DurableRewardReceipt>),
    RetryableFailure(RewardSettlementTask),
    FatalFailure(RewardSettlementTask),
}

impl RewardPersistenceCompletion {
    fn task(&self) -> &RewardSettlementTask {
        match self {
            Self::Durable(receipt) => receipt.task(),
            Self::RetryableFailure(task) | Self::FatalFailure(task) => task,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RewardCompletionDisposition {
    Applied,
    RetryScheduled { failure_count: u8 },
    TerminalFailure,
    IgnoredStale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewardTerminalReason {
    InvalidRanking,
    RewardSampling,
    RewardPersistence,
    RewardReceiptMismatch,
    RewardParticipantMissing,
    RewardRetryDeadlineOverflow,
    RewardAttemptIdExhausted,
    RewardAttemptLeaseDeadlineOverflow,
    ResultSerialization,
    OutboundReservation,
}

impl RewardTerminalReason {
    const fn permits_persistence_retry(self) -> bool {
        matches!(self, Self::RewardPersistence)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewardLanePhase {
    Loading,
    Running,
    AwaitingDeadline,
    Queued,
    InFlight,
    DurableAwaitingFinalization,
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutstandingRewardLane {
    fence: RaceFence,
    user_no: UserNo,
    nickname: String,
    phase: RewardLanePhase,
}

impl OutstandingRewardLane {
    #[must_use]
    pub const fn fence(&self) -> RaceFence {
        self.fence
    }

    #[must_use]
    pub const fn user_no(&self) -> UserNo {
        self.user_no
    }

    #[must_use]
    pub fn nickname(&self) -> &str {
        &self.nickname
    }

    #[must_use]
    pub const fn phase(&self) -> RewardLanePhase {
        self.phase
    }
}

/// Actor-minted capability naming the exact retained terminal reward state.
///
/// All fields stay private so a caller can request reconciliation only for a
/// dead letter it previously observed. The actor revalidates the full stamp
/// before resetting persistence work. There is exactly one capability per
/// failed settlement; its optional user fields identify the originating
/// failure only. Reconciliation preserves durable entries and resets every
/// non-durable entry in that race together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewardDeadLetter {
    fence: RaceFence,
    failed_attempt_id: Option<RewardAttemptId>,
    failed_user_no: Option<UserNo>,
    failed_nickname: Option<String>,
    failed_canonical_nickname: Option<String>,
    failed_proposed_reward: Option<TimeReward>,
    reason: RewardTerminalReason,
    retained_ranking: Option<SettlementRanking>,
    retained_rewards: Vec<RewardPersistenceEntry>,
    retained_packets: Option<Vec<Vec<u8>>>,
}

impl RewardDeadLetter {
    #[must_use]
    pub const fn fence(&self) -> RaceFence {
        self.fence
    }

    #[must_use]
    pub(crate) const fn failed_attempt_id(&self) -> Option<RewardAttemptId> {
        self.failed_attempt_id
    }

    #[must_use]
    pub const fn failed_user_no(&self) -> Option<UserNo> {
        self.failed_user_no
    }

    #[must_use]
    pub fn failed_nickname(&self) -> Option<&str> {
        self.failed_nickname.as_deref()
    }

    #[must_use]
    pub fn failed_canonical_nickname(&self) -> Option<&str> {
        self.failed_canonical_nickname.as_deref()
    }

    #[must_use]
    pub const fn failed_proposed_reward(&self) -> Option<TimeReward> {
        self.failed_proposed_reward
    }

    #[must_use]
    pub const fn reason(&self) -> RewardTerminalReason {
        self.reason
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewardDrainStatus {
    quiescing: bool,
    outstanding_lanes: Vec<OutstandingRewardLane>,
    dead_letters: Vec<RewardDeadLetter>,
}

impl RewardDrainStatus {
    #[must_use]
    pub const fn is_quiescing(&self) -> bool {
        self.quiescing
    }

    #[must_use]
    pub fn is_drained(&self) -> bool {
        self.outstanding_lanes.is_empty() && self.dead_letters.is_empty()
    }

    #[must_use]
    pub fn outstanding_lanes(&self) -> &[OutstandingRewardLane] {
        &self.outstanding_lanes
    }

    #[must_use]
    pub fn dead_letters(&self) -> &[RewardDeadLetter] {
        &self.dead_letters
    }

    #[must_use]
    pub fn queued_count(&self) -> usize {
        self.outstanding_lanes
            .iter()
            .filter(|lane| lane.phase == RewardLanePhase::Queued)
            .count()
    }

    #[must_use]
    pub fn in_flight_count(&self) -> usize {
        self.outstanding_lanes
            .iter()
            .filter(|lane| lane.phase == RewardLanePhase::InFlight)
            .count()
    }

    #[must_use]
    pub fn terminal_count(&self) -> usize {
        self.dead_letters.len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WorldForceShutdownReport {
    myroom_tickets: usize,
    myroom_user_indexes: usize,
    rider_equipment_tickets: usize,
    rider_equipment_user_indexes: usize,
    migration_transfers: usize,
    identity_operations: usize,
}

impl WorldForceShutdownReport {
    fn capture(world: &World) -> Self {
        Self {
            myroom_tickets: world.pending_myroom_writes.len(),
            myroom_user_indexes: world.pending_myroom_by_user.len(),
            rider_equipment_tickets: world.pending_rider_equipment_writes.len(),
            rider_equipment_user_indexes: world.pending_rider_equipment_by_user.len(),
            migration_transfers: world.identities.transfer_in_progress_count(),
            identity_operations: world.identities.outstanding_operation_count(),
        }
    }

    fn has_abandoned_completion_work(self) -> bool {
        self.myroom_tickets != 0
            || self.myroom_user_indexes != 0
            || self.rider_equipment_tickets != 0
            || self.rider_equipment_user_indexes != 0
            || self.migration_transfers != 0
            || self.identity_operations != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SlotId(pub u8);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomSnapshot {
    pub id: RoomId,
    pub slots: [Option<String>; ROOM_CAPACITY],
}

/// Generation-neutral player data supplied by a session handler. The actor
/// replaces identity fields from its current authorized binding before it
/// commits this value to a room.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RoomParticipant {
    pub(crate) player: RoomPlayer,
    pub(crate) observer: bool,
    pub(crate) kart_physics: P5136KartPhysicsBlock,
}

/// Actor-owned room metadata corresponding to the C# `GameRoom` fields used
/// by list/create/join and the first ready-stage snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RoomSettings {
    pub(crate) channel: ChannelBinding,
    pub(crate) room_name: String,
    pub(crate) password: String,
    pub(crate) game_type: u8,
    pub(crate) speed_type: u8,
    pub(crate) track: u32,
    pub(crate) room_data_header: u32,
    pub(crate) room_data: [u8; ROOM_DATA_LENGTH],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomPhase {
    Lobby,
    Loading,
    Running,
    Settling,
}

#[derive(Debug, Clone)]
pub(crate) struct StartRoomPlan {
    random_track_candidates: Vec<u32>,
    ai_specs: Vec<AiRaceSpec>,
    maximum_payload_length: usize,
}

impl StartRoomPlan {
    #[must_use]
    pub(crate) fn new(random_track_candidates: Vec<u32>, ai_specs: Vec<AiRaceSpec>) -> Self {
        Self {
            random_track_candidates,
            ai_specs,
            maximum_payload_length: MAX_GR_COMMAND_START_PAYLOAD_LENGTH,
        }
    }

    #[cfg(test)]
    fn with_maximum_payload_length(mut self, maximum_payload_length: usize) -> Self {
        self.maximum_payload_length = maximum_payload_length;
        self
    }
}

#[derive(Debug)]
pub(crate) enum LobbyCommandPayload {
    SetSlotState(PlayerSlotState),
    ChangeTeam(RoomTeam),
    ChangeMaster(String),
    StartRoom(StartRoomPlan),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LobbyCommandOutcome {
    SlotStateChanged {
        room_id: RoomId,
        player_id: i32,
        state: PlayerSlotState,
    },
    TeamChanged {
        room_id: RoomId,
        player_id: i32,
        team: RoomTeam,
        slot_id: u8,
    },
    MasterChanged {
        room_id: RoomId,
        previous_player_id: i32,
        next_player_id: i32,
    },
    Started {
        room_id: RoomId,
        race_epoch: u64,
        concrete_track: u32,
        racer_count: usize,
        observer_count: usize,
    },
}

#[derive(Debug)]
pub(crate) enum RaceCommandPayload {
    GameControl(GameControlRequest),
    AiGoalIn(AiGoalInRequest),
    TeamBoosterGauge(TeamBoosterGaugeRequest),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RaceCommandOutcome {
    LoadingAwaiting {
        room_id: RoomId,
        race_epoch: u64,
        expected_participants: usize,
    },
    IgnoredDuplicate {
        room_id: RoomId,
        race_epoch: u64,
    },
    FinishRecorded {
        room_id: RoomId,
        race_epoch: u64,
        player_id: i32,
        began_settlement: bool,
    },
    BoosterGaugeUpdated {
        room_id: RoomId,
        race_epoch: u64,
        team: RaceTeam,
        reached_full: bool,
    },
}

/// Parsed request payloads carried by the next actor command layer. Keeping
/// parsing outside the actor prevents untrusted byte processing from blocking
/// unrelated world state.
#[derive(Debug)]
pub(crate) enum RoomCommandPayload {
    List(ChGetRoomListRequest),
    Create {
        request: ChCreateRoomRequest,
        participant: RoomParticipant,
    },
    Join {
        request: ChJoinRoomRequest,
        participant: RoomParticipant,
    },
    Leave,
    FirstState,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RoomError {
    #[error("room {0} does not exist")]
    NotFound(u32),

    #[error("room {0} is full")]
    Full(u32),

    #[error("identity {0:?} is already in a room")]
    AlreadyJoined(String),

    #[error("identity {0:?} is not in a room")]
    NotJoined(String),
}

#[derive(Debug, Error)]
pub enum LobbyError {
    #[error("world is quiescing and will not start another race")]
    WorldQuiescing,

    #[error("identity is not in a protocol room")]
    NotInRoom,

    #[error("room command requires the lobby phase; current phase is {actual:?}")]
    NotLobby { actual: RoomPhase },

    #[error("only a human racer may perform this lobby command")]
    HumanRacerRequired,

    #[error("observer slot state is server-owned")]
    ObserverStateServerOwned,

    #[error("preparing slot state is server-owned")]
    PreparingStateServerOwned,

    #[error("only the current human room master may perform this command")]
    NotRoomMaster,

    #[error("room-master target {nickname:?} is not a human racer in this room")]
    InvalidMasterTarget { nickname: String },

    #[error("team changes are valid only in game types 3 and 4")]
    TeamModeRequired,

    #[error("{team:?} has no free physical slot")]
    TeamFull { team: RoomTeam },

    #[error("at least one human racer is required to start")]
    NoRacers,

    #[error("AI participants are unsupported until they are frozen into the race roster")]
    AiParticipantsUnsupported,

    #[error("non-master racer {user_no} is not ready")]
    RacerNotReady { user_no: u32 },

    #[error("process-global race epoch is exhausted")]
    RaceEpochExhausted,

    #[error("random track selection requires at least one non-zero concrete candidate")]
    MissingTrackCandidates,

    #[error("room participant {user_no} has no active exact-generation identity")]
    InactiveRosterMember { user_no: u32 },

    #[error(
        "human racer {user_no} still has an outstanding reward for room {room_id}, epoch {race_epoch}"
    )]
    RewardLaneOccupied {
        user_no: u32,
        room_id: u32,
        race_epoch: u64,
    },

    #[error("session {session:?} has no available outbound queue slot")]
    OutboundUnavailable { session: SessionId },

    #[error(transparent)]
    Protocol(#[from] LobbyProtocolError),

    #[error(transparent)]
    RoomProtocol(#[from] RoomProtocolError),

    #[error(transparent)]
    RaceStart(#[from] RaceStartProtocolError),
}

#[derive(Debug, Error)]
pub enum RaceError {
    #[error("identity is not in a protocol room")]
    NotInRoom,

    #[error("race loading command requires Loading; current phase is {actual:?}")]
    WrongPhase { actual: RoomPhase },

    #[error("identity is not an exact-generation participant in the frozen race roster")]
    NotFrozenParticipant,

    #[error("GameControl state {state} is unsupported during race loading")]
    UnsupportedGameControlState { state: i32 },

    #[error("race request requires Running; current phase is {actual:?}")]
    NotRunning { actual: RoomPhase },

    #[error("only a frozen human racer may perform this race command")]
    HumanRacerRequired,

    #[error("frozen race roster has no AI participant in player slot {player_id}")]
    NoFrozenAiParticipant { player_id: i32 },

    #[error("team booster requires game type 3 or 4")]
    TeamModeRequired,

    #[error("team booster request claims {claimed:?}, but sender belongs to wire team {actual}")]
    TeamSpoof { claimed: RaceTeam, actual: u8 },

    #[error("frozen racer has invalid wire team {team}")]
    InvalidFrozenTeam { team: u8 },

    #[error("session {session:?} has no available outbound queue slot")]
    OutboundUnavailable { session: SessionId },

    #[error("the monotonic race loading deadline overflowed")]
    RaceDeadlineOverflow,

    #[error("the monotonic settlement deadline overflowed")]
    SettlementDeadlineOverflow,

    #[error("the settlement deadline has closed this race")]
    SettlementClosed,

    #[error("the bounded pending race fan-out is full")]
    PendingRaceFanoutFull,

    #[error("frozen settlement roster contains {racers} racers; expected 1 through 8")]
    InvalidSettlementRoster { racers: usize },

    #[error("reward sampling failed for player {player_id}: {reason}")]
    RewardSampling {
        player_id: i32,
        reason: p5136_profile::RewardRollError,
    },

    #[error("settlement result serialization was attempted before every reward was durable")]
    RewardsNotDurable,

    #[error("settlement invariant failed: {detail}")]
    SettlementInvariant { detail: &'static str },

    #[error(transparent)]
    Protocol(#[from] RaceProtocolError),

    #[error(transparent)]
    ResultProtocol(#[from] RaceResultProtocolError),
}

impl RaceError {
    #[must_use]
    pub(crate) const fn is_expected_rejection(&self) -> bool {
        matches!(
            self,
            Self::NotInRoom
                | Self::WrongPhase { .. }
                | Self::NotFrozenParticipant
                | Self::UnsupportedGameControlState { .. }
                | Self::NotRunning { .. }
                | Self::HumanRacerRequired
                | Self::NoFrozenAiParticipant { .. }
                | Self::TeamModeRequired
                | Self::TeamSpoof { .. }
                | Self::InvalidFrozenTeam { .. }
                | Self::OutboundUnavailable { .. }
                | Self::SettlementClosed
        )
    }
}

/// Fatal failure while reconciling actor-owned identity and `MyRoom` state.
///
/// Identity release and migration mutate the registry before dependent
/// generation-bound state can be published. Any error at this boundary stops
/// the world actor; continuing would expose a split identity/MyRoom view.
#[derive(Debug, Error)]
pub(crate) enum MyRoomLifecycleError {
    #[error("MyRoom {operation} transition planning failed")]
    Hub {
        operation: &'static str,
        #[source]
        source: MyRoomHubError,
    },

    #[error(transparent)]
    WireProjection(#[from] MyRoomWireProjectionError),

    #[error("MyRoom slot snapshot serialization failed: {0}")]
    Serialization(#[from] MyRoomProtocolError),

    #[error(
        "MyRoom audience identity {user_no:?} expected generation {expected_generation}, active generation is {actual_generation:?}"
    )]
    AudienceIdentityMismatch {
        user_no: UserNo,
        expected_generation: u64,
        actual_generation: Option<u64>,
    },

    #[error("MyRoom outbound queue for session {session:?} is unavailable")]
    OutboundUnavailable { session: SessionId },

    #[error(
        "MyRoom shutdown drain retained {rooms} rooms, {memberships} memberships, and {generations} generations"
    )]
    ShutdownStateRetained {
        rooms: usize,
        memberships: usize,
        generations: usize,
    },

    #[error(transparent)]
    Commit(#[from] MyRoomCommitError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MyRoomCommandPayload {
    FirstState,
    Secede,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum MyRoomPeerCommandPayload {
    CharacterPosition(CharacterPositionRequest),
}

/// Actor-minted topology fence for one profile-backed `MyRoom` command.
///
/// A member plan is intentionally incomplete until the session profile runtime
/// reloads every occupied slot from disk. Completing the plan validates that
/// the projection belongs to this exact opaque topology before it can cross
/// back into the World actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MyRoomCommandPlan {
    expected: IdentityBinding,
    wire: Option<MyRoomWirePlan>,
}

impl MyRoomCommandPlan {
    pub(crate) fn expected_identity(&self) -> &IdentityBinding {
        &self.expected
    }

    pub(crate) fn wire_plan(&self) -> Option<&MyRoomWirePlan> {
        self.wire.as_ref()
    }

    pub(crate) fn complete(
        self,
        projection: Option<MyRoomWireProjection>,
    ) -> Result<MyRoomPreparedCommand, WorldError> {
        let projection_matches = match (&self.wire, &projection) {
            (None, None) => true,
            (Some(plan), Some(projected)) => projected.plan() == plan,
            (None, Some(_)) | (Some(_), None) => false,
        };
        if !projection_matches {
            return Err(WorldError::MyRoomWireProjectionMismatch {
                session: self.expected.owner,
            });
        }
        Ok(MyRoomPreparedCommand {
            expected: self.expected,
            projection,
        })
    }
}

#[derive(Debug)]
pub(crate) struct MyRoomPreparedCommand {
    expected: IdentityBinding,
    projection: Option<MyRoomWireProjection>,
}

/// Profile-derived owner-item packets paired with the canonical profile lane
/// and exact requester operation that produced them.
#[derive(Debug)]
pub(crate) struct MyRoomOwnerItemLoad {
    owner: IdentityBinding,
    packets: Vec<Vec<u8>>,
    lane: ProfileLanePermit,
}

impl MyRoomOwnerItemLoad {
    pub(crate) fn new(
        owner: IdentityBinding,
        packets: Vec<Vec<u8>>,
        lane: ProfileLanePermit,
    ) -> Self {
        Self {
            owner,
            packets,
            lane,
        }
    }
}

/// Actor-minted minimal authorization for one owner-item response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MyRoomOwnerItemsPlan {
    expected: IdentityBinding,
    owner: Option<MyRoomOwnerItemPlan>,
}

impl MyRoomOwnerItemsPlan {
    pub(crate) fn expected_identity(&self) -> &IdentityBinding {
        &self.expected
    }

    pub(crate) fn owner_identity(&self) -> Option<&IdentityBinding> {
        self.owner.as_ref().map(MyRoomOwnerItemPlan::owner)
    }

    #[must_use]
    pub(crate) fn owner_items_visible(&self) -> bool {
        self.owner
            .as_ref()
            .is_some_and(MyRoomOwnerItemPlan::visible)
    }

    pub(crate) fn complete(
        self,
        loaded: Option<MyRoomOwnerItemLoad>,
    ) -> Result<MyRoomPreparedOwnerItems, WorldError> {
        match (&self.owner, &loaded) {
            (None, None) => {}
            (Some(plan), None) if !plan.visible() => {}
            (Some(plan), Some(loaded)) if plan.visible() => {
                if loaded.owner != *plan.owner()
                    || canonical_nickname_key(loaded.lane.subject().nickname())
                        != canonical_nickname_key(&plan.owner().nickname)
                    || loaded.packets.is_empty()
                {
                    return Err(WorldError::MyRoomOwnerItemPlanMismatch {
                        session: self.expected.owner,
                    });
                }
            }
            (Some(_), None | Some(_)) | (None, Some(_)) => {
                return Err(WorldError::MyRoomOwnerItemPlanMismatch {
                    session: self.expected.owner,
                });
            }
        }
        Ok(MyRoomPreparedOwnerItems {
            expected: self.expected,
            owner: self.owner,
            loaded,
        })
    }
}

/// Actor-minted authorization plus a profile-lane-backed owner-item result.
#[derive(Debug)]
pub(crate) struct MyRoomPreparedOwnerItems {
    expected: IdentityBinding,
    owner: Option<MyRoomOwnerItemPlan>,
    loaded: Option<MyRoomOwnerItemLoad>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MyRoomSessionRole {
    PresentOwner,
    Visitor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MyRoomSessionView {
    role: MyRoomSessionRole,
    info: MyRoomInfo,
}

impl MyRoomSessionView {
    pub(crate) const fn role(&self) -> MyRoomSessionRole {
        self.role
    }

    pub(crate) fn info(&self) -> &MyRoomInfo {
        &self.info
    }
}

#[derive(Debug, Error)]
pub enum WorldError {
    #[error("world actor has stopped")]
    Stopped,

    #[error("world is quiescing and no longer accepts new sessions")]
    SessionRegistrationClosed,

    #[error(transparent)]
    Room(#[from] RoomError),

    #[error(transparent)]
    Identity(#[from] IdentityError),

    #[error(transparent)]
    RoomProtocol(#[from] RoomProtocolError),

    #[error(transparent)]
    EquipmentProtocol(#[from] EquipmentProtocolError),

    #[error(transparent)]
    Lobby(#[from] LobbyError),

    #[error(transparent)]
    Race(#[from] RaceError),

    #[error("session {0:?} is not registered")]
    UnknownSession(SessionId),

    #[error("active identity limit {maximum} reached")]
    IdentityLimitReached { maximum: usize },

    #[error("identity operation capability belongs to a different world actor")]
    ForeignIdentityOperation,

    #[error("MyRoom command could not reserve the outbound queue for session {session:?}")]
    MyRoomCommandOutboundUnavailable { session: SessionId },

    #[error("channel-move acknowledgement queue is unavailable for session {session:?}")]
    MigrationAcknowledgementUnavailable { session: SessionId },

    #[error("channel migration peer publication queue is unavailable for session {session:?}")]
    MigrationPublicationUnavailable { session: SessionId },

    #[error("channel migration protocol-room state is inconsistent for user {user_no}")]
    MigrationProtocolRoomInconsistent { user_no: u32 },

    #[error("channel migration lifecycle queue could not reserve commit capacity")]
    MigrationCommitCapacityUnavailable,

    #[error(
        "MyRoom topology changed while live slot profiles were being loaded for session {session:?}"
    )]
    MyRoomWirePlanStale { session: SessionId },

    #[error(
        "MyRoom live profile projection does not belong to the actor-minted plan for session {session:?}"
    )]
    MyRoomWireProjectionMismatch { session: SessionId },

    #[error(
        "MyRoom owner-item result does not belong to the actor-minted plan for session {session:?}"
    )]
    MyRoomOwnerItemPlanMismatch { session: SessionId },

    #[error("MyRoom profile lane for {actual:?} cannot refresh identity {expected:?}")]
    MyRoomProfileSubjectMismatch { expected: String, actual: String },

    #[error(
        "reward attempt ID space exhausted for room {room_id}, epoch {race_epoch}, user {user_no}"
    )]
    RewardAttemptIdExhausted {
        room_id: u32,
        race_epoch: u64,
        user_no: u32,
    },

    #[error(
        "reward attempt lease failures exhausted for room {room_id}, epoch {race_epoch}, user {user_no}"
    )]
    RewardAttemptLeaseFailuresExhausted {
        room_id: u32,
        race_epoch: u64,
        user_no: u32,
    },

    #[error(
        "reward attempt lease deadline overflowed for room {room_id}, epoch {race_epoch}, user {user_no}"
    )]
    RewardAttemptLeaseDeadlineOverflow {
        room_id: u32,
        race_epoch: u64,
        user_no: u32,
    },

    #[error(
        "reward retry deadline overflowed for room {room_id}, epoch {race_epoch}, user {user_no}"
    )]
    RewardRetryDeadlineOverflow {
        room_id: u32,
        race_epoch: u64,
        user_no: u32,
    },

    #[error("reward scheduler invariant failed for room {room_id}, user {user_no}")]
    RewardSchedulerInvariant { room_id: u32, user_no: u32 },

    #[error("terminal reward settlement invariant failed for room {room_id}, epoch {race_epoch}")]
    RewardDeadLetterInvariant { room_id: u32, race_epoch: u64 },

    #[error(
        "world shutdown refused while {outstanding_lanes} reward lanes remain ({dead_letters} terminal)"
    )]
    RewardShutdownBlocked {
        outstanding_lanes: usize,
        dead_letters: usize,
    },

    #[error("reward dead-letter capability is stale")]
    StaleRewardDeadLetter,

    #[error("reward dead-letter reason {reason:?} is not eligible for persistence retry")]
    RewardDeadLetterNotRetryable { reason: RewardTerminalReason },

    #[error("session shutdown drain requires the world to be quiescing")]
    SessionDrainRequiresQuiesce,

    #[error("outbound producer drain requires the world to be quiescing")]
    OutboundProducerDrainRequiresQuiesce,

    #[error("world is quiescing and no longer accepts wire-producing commands")]
    OutboundProductionClosed,

    #[error(
        "world shutdown refused while {pending} MyRoom profile writes remain ({indexed} user indexes)"
    )]
    MyRoomPersistenceShutdownBlocked { pending: usize, indexed: usize },

    #[error("world shutdown refused while {active} identity operations remain active")]
    IdentityOperationShutdownBlocked { active: usize },
}

/// A terminal failure while publishing actor-owned state to a process sidecar.
///
/// Keeping this boundary distinct from [`WorldError`] lets request handlers
/// continue to report ordinary command failures without hiding a world/sidecar
/// divergence that requires the central runtime to stop.
#[derive(Debug, Error)]
pub(crate) enum WorldSidecarError {
    #[error("world messenger sidecar failed: {0}")]
    Messenger(#[from] MessengerServiceError),

    #[error("world UDP sidecar failed: {0}")]
    Udp(#[from] UdpServiceError),

    #[error("world MyRoom lifecycle failed: {0}")]
    MyRoom(#[from] MyRoomLifecycleError),

    #[error("world MyRoom persistence failed: {0}")]
    MyRoomPersistence(#[from] MyRoomPersistenceInvariantError),

    #[error("world rider-equipment publication failed: {0}")]
    RiderEquipment(#[from] RiderEquipmentPublicationInvariantError),

    #[error("world identity capacity must be nonzero")]
    InvalidIdentityCapacity,
}

/// A caller-visible failure to construct the standalone World actor.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum WorldSpawnError {
    #[error("world mailbox capacity must be nonzero")]
    ZeroMailboxCapacity,
}

/// A terminal standalone World-actor failure.
///
/// The concrete sidecar invariant remains internal, while callers can still
/// observe and report the original source through [`std::error::Error`].
#[derive(Debug, Error)]
#[error("world actor terminated: {source}")]
pub struct WorldActorError {
    #[source]
    source: WorldSidecarError,
}

impl From<WorldSidecarError> for WorldActorError {
    fn from(source: WorldSidecarError) -> Self {
        Self { source }
    }
}

#[derive(Debug)]
enum WorldOperationError {
    Command(WorldError),
    MyRoom(MyRoomLifecycleError),
}

#[derive(Debug)]
enum MyRoomInfoRegistrationError {
    Request(Box<MyRoomInfoWriteError>),
    MyRoom(Box<MyRoomLifecycleError>),
    Terminal(Box<MyRoomPersistenceInvariantError>),
}

#[derive(Debug)]
enum RiderEquipmentRegistrationError {
    Request(Box<RiderEquipmentWriteError>),
    Terminal(Box<RiderEquipmentPublicationInvariantError>),
}

impl RiderEquipmentRegistrationError {
    fn request(error: RiderEquipmentWriteError) -> Self {
        Self::Request(Box::new(error))
    }

    fn terminal(error: RiderEquipmentPublicationInvariantError) -> Self {
        Self::Terminal(Box::new(error))
    }
}

impl MyRoomInfoRegistrationError {
    fn request(error: MyRoomInfoWriteError) -> Self {
        Self::Request(Box::new(error))
    }

    fn myroom(error: MyRoomLifecycleError) -> Self {
        Self::MyRoom(Box::new(error))
    }

    fn terminal(error: MyRoomPersistenceInvariantError) -> Self {
        Self::Terminal(Box::new(error))
    }
}

impl From<WorldError> for WorldOperationError {
    fn from(error: WorldError) -> Self {
        Self::Command(error)
    }
}

impl From<IdentityError> for WorldOperationError {
    fn from(error: IdentityError) -> Self {
        Self::Command(error.into())
    }
}

impl From<RoomProtocolError> for WorldOperationError {
    fn from(error: RoomProtocolError) -> Self {
        Self::Command(error.into())
    }
}

impl From<EquipmentProtocolError> for WorldOperationError {
    fn from(error: EquipmentProtocolError) -> Self {
        Self::Command(error.into())
    }
}

impl From<MyRoomLifecycleError> for WorldOperationError {
    fn from(error: MyRoomLifecycleError) -> Self {
        Self::MyRoom(error)
    }
}

/// Cancellation-safe ownership of an identity migration freeze.
///
/// Dropping this capability before submission reports an abort through the
/// pre-reserved completion channel. Once submitted, the World actor owns both
/// the preflight and profile lease until it publishes one terminal result.
pub(crate) struct RegisteredMigrationPreflight {
    preflight: Option<Box<MigrationPreflight>>,
    completion: Option<MyRoomCompletionSlot>,
}

impl fmt::Debug for RegisteredMigrationPreflight {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RegisteredMigrationPreflight")
            .field(
                "nickname",
                &self.preflight.as_deref().map(MigrationPreflight::nickname),
            )
            .finish_non_exhaustive()
    }
}

impl RegisteredMigrationPreflight {
    fn new(preflight: MigrationPreflight, completion: MyRoomCompletionSlot) -> Self {
        Self {
            preflight: Some(Box::new(preflight)),
            completion: Some(completion),
        }
    }

    #[must_use]
    pub(crate) fn nickname(&self) -> &str {
        self.preflight
            .as_deref()
            .expect("a registered migration retains its preflight until submission")
            .nickname()
    }

    #[must_use]
    #[cfg(test)]
    pub(crate) fn canonical_nickname(&self) -> &str {
        self.preflight
            .as_deref()
            .expect("a registered migration retains its preflight until submission")
            .canonical_nickname()
    }

    #[must_use]
    #[cfg(test)]
    pub(crate) fn user_no(&self) -> UserNo {
        self.preflight
            .as_deref()
            .expect("a registered migration retains its preflight until submission")
            .user_no()
    }

    #[must_use]
    #[cfg(test)]
    pub(crate) fn source_generation(&self) -> IdentityGeneration {
        self.preflight
            .as_deref()
            .expect("a registered migration retains its preflight until submission")
            .source_generation()
    }

    #[must_use]
    pub(crate) fn destination_ip(&self) -> IpAddr {
        self.preflight
            .as_deref()
            .expect("a registered migration retains its preflight until submission")
            .destination_ip()
    }

    pub(crate) async fn wait_for_operations_drained(&self) -> Result<(), WorldError> {
        let Some(preflight) = self.preflight.as_deref() else {
            debug_assert!(false, "a submitted migration cannot be awaited again");
            return Err(WorldError::Identity(IdentityError::StaleMigrationPreflight));
        };
        preflight
            .wait_for_operations_drained()
            .await
            .map_err(WorldError::from)
    }

    #[cfg(test)]
    fn submit(
        self,
        profile: MyRoomProfileLease,
        reply: oneshot::Sender<Result<MigrationCompletion, WorldError>>,
    ) {
        self.submit_with_acknowledgement(profile, MigrationAcknowledgement::Omitted, reply);
    }

    fn submit_with_acknowledgement(
        mut self,
        profile: MyRoomProfileLease,
        acknowledgement: MigrationAcknowledgement,
        reply: oneshot::Sender<Result<MigrationCompletion, WorldError>>,
    ) {
        let preflight = self
            .preflight
            .take()
            .expect("a registered migration submits its preflight exactly once");
        let completion = self
            .completion
            .take()
            .expect("a registered migration submits its completion capability exactly once");
        completion.send(MyRoomProfileCompletion::Migration(
            MigrationProfileCompletion::Ready {
                preflight,
                profile: Box::new(profile),
                acknowledgement,
                reply,
            },
        ));
    }
}

impl Drop for RegisteredMigrationPreflight {
    fn drop(&mut self) {
        let (Some(preflight), Some(completion)) = (self.preflight.take(), self.completion.take())
        else {
            return;
        };
        completion.send(MyRoomProfileCompletion::Migration(
            MigrationProfileCompletion::Aborted { preflight },
        ));
    }
}

#[derive(Debug)]
enum WorldCommand {
    AdmitIdentityOperation {
        session: SessionId,
        reply: oneshot::Sender<Result<IdentityOperationLease, WorldError>>,
    },
    /// The retained lease is the authorization capability for the boxed
    /// command. It remains actor-owned even if the request future is cancelled
    /// after enqueueing the command.
    AdmittedIdentityOperation {
        operation: IdentityOperationLease,
        command: Box<WorldCommand>,
    },
    RegisterSession {
        peer: SocketAddr,
        cancellation: Option<oneshot::Sender<()>>,
        outbound: Option<mpsc::Sender<OutboundBatch>>,
        outbound_operations: Option<WireOperationGate>,
        reply: oneshot::Sender<Result<SessionId, WorldError>>,
    },
    SessionClosed {
        id: SessionId,
        reply: Option<oneshot::Sender<()>>,
    },
    ClaimIdentity {
        session: SessionId,
        nickname: String,
        reply: oneshot::Sender<Result<IdentityBinding, WorldError>>,
    },
    AuthorizeIdentity {
        session: SessionId,
        reply: oneshot::Sender<Result<IdentityBinding, WorldError>>,
    },
    BeginMigration {
        session: SessionId,
        channel: ChannelBinding,
        token: MigrationToken,
        now: Instant,
        reply: oneshot::Sender<Result<MigrationPermit, WorldError>>,
    },
    #[cfg(test)]
    CompleteMigration {
        destination: SessionId,
        user_no: UserNo,
        channel_id: u16,
        token: MigrationToken,
        now: Instant,
        reply: oneshot::Sender<Result<MigrationCompletion, WorldError>>,
    },
    #[cfg_attr(not(test), allow(dead_code))]
    PreflightMigration {
        destination: SessionId,
        user_no: UserNo,
        channel_id: u16,
        token: MigrationToken,
        now: Instant,
        completion: MyRoomCompletionSlot,
        reply: oneshot::Sender<Result<RegisteredMigrationPreflight, WorldError>>,
    },
    RoomProtocol {
        session: SessionId,
        payload: Box<RoomCommandPayload>,
        reply: oneshot::Sender<Result<(), WorldError>>,
    },
    #[cfg_attr(not(test), allow(dead_code))]
    PublishRoomEquipment {
        session: SessionId,
        snapshot: Box<[u8; 65]>,
        reply: oneshot::Sender<Result<(), WorldError>>,
    },
    RefreshMyRoomPresentation {
        session: SessionId,
        expected: IdentityBinding,
        profile: Box<MyRoomProfileLease>,
        reply: oneshot::Sender<Result<bool, WorldError>>,
    },
    Lobby {
        session: SessionId,
        payload: LobbyCommandPayload,
        reply: oneshot::Sender<Result<LobbyCommandOutcome, WorldError>>,
    },
    Race {
        session: SessionId,
        payload: RaceCommandPayload,
        reply: oneshot::Sender<Result<RaceCommandOutcome, WorldError>>,
    },
    CreateRoom {
        reply: oneshot::Sender<Result<RoomId, WorldError>>,
    },
    JoinRoom {
        room: RoomId,
        identity: String,
        reply: oneshot::Sender<Result<SlotId, WorldError>>,
    },
    JoinRoomForSession {
        room: RoomId,
        session: SessionId,
        reply: oneshot::Sender<Result<SlotId, WorldError>>,
    },
    LeaveRoom {
        identity: String,
        reply: oneshot::Sender<Result<(), WorldError>>,
    },
    RoomSnapshot {
        room: RoomId,
        reply: oneshot::Sender<Result<RoomSnapshot, RoomError>>,
    },
    SessionCount {
        reply: oneshot::Sender<usize>,
    },
    #[cfg_attr(not(test), allow(dead_code))]
    TakeDueRewardTasks {
        now: Instant,
        maximum: usize,
        reply: oneshot::Sender<Result<Vec<RewardSettlementTask>, WorldError>>,
    },
    #[cfg_attr(not(test), allow(dead_code))]
    CompleteRewardTask {
        completion: RewardPersistenceCompletion,
        now: Instant,
        reply: oneshot::Sender<Result<RewardCompletionDisposition, WorldError>>,
    },
    Quiesce {
        reply: oneshot::Sender<()>,
    },
    DrainOutboundProducers {
        reply: oneshot::Sender<Result<bool, WorldError>>,
    },
    DrainSessions {
        reply: oneshot::Sender<Result<(), WorldError>>,
    },
    PrepareMyRoom {
        session: SessionId,
        reply: oneshot::Sender<Result<MyRoomCommandPlan, WorldError>>,
    },
    PrepareMyRoomOwnerItems {
        session: SessionId,
        reply: oneshot::Sender<Result<MyRoomOwnerItemsPlan, WorldError>>,
    },
    MyRoom {
        session: SessionId,
        payload: MyRoomCommandPayload,
        prepared: Box<MyRoomPreparedCommand>,
        reply: oneshot::Sender<Result<(), WorldError>>,
    },
    MyRoomPeer {
        session: SessionId,
        payload: MyRoomPeerCommandPayload,
        reply: oneshot::Sender<Result<(), WorldError>>,
    },
    PublishMyRoomOwnerItems {
        session: SessionId,
        prepared: Box<MyRoomPreparedOwnerItems>,
        reply: oneshot::Sender<Result<(), WorldError>>,
    },
    RegisterMyRoomInfoWrite {
        session: SessionId,
        prepared: PreparedMyRoomInfoWrite,
        request_reply: oneshot::Sender<Result<MyRoomInfoWriteReceipt, MyRoomInfoWriteError>>,
        reply: oneshot::Sender<Result<RegisteredMyRoomInfoWrite, MyRoomInfoWriteError>>,
    },
    RegisterRiderEquipmentWrite {
        session: SessionId,
        prepared: PreparedRiderEquipmentWrite,
        request_reply:
            oneshot::Sender<Result<RiderEquipmentWriteReceipt, RiderEquipmentWriteError>>,
        reply: oneshot::Sender<Result<RegisteredRiderEquipmentWrite, RiderEquipmentWriteError>>,
    },
    MyRoomSessionView {
        session: SessionId,
        reply: oneshot::Sender<Result<Option<MyRoomSessionView>, WorldError>>,
    },
    RewardDrainStatus {
        reply: oneshot::Sender<Result<RewardDrainStatus, WorldError>>,
    },
    RetryRewardDeadLetter {
        dead_letter: RewardDeadLetter,
        reply: oneshot::Sender<Result<(), WorldError>>,
    },
    Shutdown {
        reply: oneshot::Sender<Result<(), WorldError>>,
    },
    ForceShutdown {
        reply: oneshot::Sender<WorldForceShutdownReport>,
    },
}

#[derive(Debug, Clone)]
pub struct WorldHandle {
    sender: mpsc::Sender<WorldCommand>,
    udp_sender: Option<mpsc::Sender<UdpIngress>>,
    myroom_completions: MyRoomCompletionBridge,
    identity_instance: Arc<IdentityRegistryInstance>,
}

/// Session-facing view which can enqueue only work retained by one exact,
/// actor-minted identity operation.
#[derive(Debug)]
pub(crate) struct AdmittedWorldHandle<'a> {
    world: &'a WorldHandle,
    operation: &'a IdentityOperationLease,
}

#[derive(Debug, Clone, Default)]
struct WorldSidecars {
    messenger: Option<MessengerServiceHandle>,
    udp: Option<UdpService>,
}

impl WorldSidecars {
    fn identity_capacity(&self) -> Option<usize> {
        self.messenger
            .as_ref()
            .map(MessengerServiceHandle::max_identities)
            .into_iter()
            .chain(self.udp.as_ref().map(UdpService::max_identities))
            .min()
    }
}

impl WorldHandle {
    pub(crate) async fn admit_identity_operation(
        &self,
        session: SessionId,
    ) -> Result<IdentityOperationLease, WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::AdmitIdentityOperation { session, reply })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    #[must_use]
    pub(crate) const fn admitted<'a>(
        &'a self,
        operation: &'a IdentityOperationLease,
    ) -> AdmittedWorldHandle<'a> {
        AdmittedWorldHandle {
            world: self,
            operation,
        }
    }

    pub fn spawn(
        mailbox_capacity: usize,
    ) -> Result<(Self, JoinHandle<Result<(), WorldActorError>>), WorldSpawnError> {
        let mailbox_capacity =
            NonZeroUsize::new(mailbox_capacity).ok_or(WorldSpawnError::ZeroMailboxCapacity)?;
        let (sender, receiver) = mpsc::channel(mailbox_capacity.get());
        let identities = IdentityRegistry::new();
        let identity_instance = identities.instance();
        let completion_capacity = NonZeroUsize::new(DEFAULT_WORLD_IDENTITY_CAPACITY)
            .expect("the default World identity capacity is nonzero");
        let (myroom_completions, completion_receiver) =
            MyRoomCompletionBridge::channel(completion_capacity);
        let handle = Self {
            sender,
            udp_sender: None,
            myroom_completions,
            identity_instance,
        };
        let task = tokio::spawn(async move {
            run_world(
                receiver,
                completion_receiver,
                None,
                WorldSidecars::default(),
                ServerClock::new(),
                identities,
            )
            .await
            .map_err(WorldActorError::from)
        });
        Ok((handle, task))
    }

    #[cfg(test)]
    pub(crate) fn spawn_with_messenger(
        mailbox_capacity: usize,
        messenger: MessengerServiceHandle,
    ) -> (Self, JoinHandle<Result<(), MessengerServiceError>>) {
        let (sender, receiver) = mpsc::channel(mailbox_capacity);
        let identities = IdentityRegistry::new();
        let identity_instance = identities.instance();
        let sidecars = WorldSidecars {
            messenger: Some(messenger),
            udp: None,
        };
        let completion_capacity = NonZeroUsize::new(
            sidecars
                .identity_capacity()
                .unwrap_or(DEFAULT_WORLD_IDENTITY_CAPACITY),
        )
        .expect("messenger identity capacity is validated before World startup");
        let (myroom_completions, completion_receiver) =
            MyRoomCompletionBridge::channel(completion_capacity);
        let handle = Self {
            sender,
            udp_sender: None,
            myroom_completions,
            identity_instance,
        };
        let task = tokio::spawn(async move {
            match run_world(
                receiver,
                completion_receiver,
                None,
                sidecars,
                ServerClock::new(),
                identities,
            )
            .await
            {
                Ok(()) => Ok(()),
                Err(WorldSidecarError::Messenger(error)) => Err(error),
                Err(WorldSidecarError::Udp(_)) => {
                    unreachable!("messenger-only world cannot report a UDP sidecar failure")
                }
                Err(
                    WorldSidecarError::MyRoom(_)
                    | WorldSidecarError::MyRoomPersistence(_)
                    | WorldSidecarError::RiderEquipment(_)
                    | WorldSidecarError::InvalidIdentityCapacity,
                ) => {
                    unreachable!("messenger-only test world has no MyRoom mutation commands")
                }
            }
        });
        (handle, task)
    }

    pub(crate) fn spawn_with_services(
        mailbox_capacity: usize,
        udp_mailbox_capacity: usize,
        messenger: MessengerServiceHandle,
        udp: UdpService,
        clock: ServerClock,
    ) -> (Self, JoinHandle<Result<(), WorldSidecarError>>) {
        let (sender, receiver) = mpsc::channel(mailbox_capacity);
        let (udp_sender, udp_receiver) = mpsc::channel(udp_mailbox_capacity);
        let identities = IdentityRegistry::new();
        let identity_instance = identities.instance();
        let sidecars = WorldSidecars {
            messenger: Some(messenger),
            udp: Some(udp),
        };
        let completion_capacity = NonZeroUsize::new(
            sidecars
                .identity_capacity()
                .unwrap_or(DEFAULT_WORLD_IDENTITY_CAPACITY),
        )
        // A zero sidecar capacity remains a typed World startup error. The
        // one-slot mailbox only lets that actor task own and report the error.
        .unwrap_or(NonZeroUsize::MIN);
        let (myroom_completions, completion_receiver) =
            MyRoomCompletionBridge::channel(completion_capacity);
        let handle = Self {
            sender,
            udp_sender: Some(udp_sender),
            myroom_completions,
            identity_instance,
        };
        let task = tokio::spawn(run_world(
            receiver,
            completion_receiver,
            Some(udp_receiver),
            sidecars,
            clock,
            identities,
        ));
        (handle, task)
    }

    /// Admits validated UDP traffic without allowing a saturated data plane to
    /// delay control commands or shutdown.
    pub(crate) fn try_udp_ingress(
        &self,
        ingress: UdpIngress,
    ) -> Result<(), mpsc::error::TrySendError<UdpIngress>> {
        let Some(sender) = &self.udp_sender else {
            return Err(mpsc::error::TrySendError::Closed(ingress));
        };
        sender.try_send(ingress)
    }

    pub async fn register_session(&self, peer: SocketAddr) -> Result<SessionId, WorldError> {
        self.register_session_inner(peer, None, None, None).await
    }

    pub(crate) async fn register_login_session(
        &self,
        peer: SocketAddr,
        outbound_operations: WireOperationGate,
    ) -> Result<
        (
            SessionId,
            oneshot::Receiver<()>,
            mpsc::Receiver<OutboundBatch>,
        ),
        WorldError,
    > {
        let (cancel, cancelled) = oneshot::channel();
        let (outbound, outbound_receiver) = mpsc::channel(SESSION_OUTBOUND_CAPACITY);
        let id = self
            .register_session_inner(
                peer,
                Some(cancel),
                Some(outbound),
                Some(outbound_operations),
            )
            .await?;
        Ok((id, cancelled, outbound_receiver))
    }

    async fn register_session_inner(
        &self,
        peer: SocketAddr,
        cancellation: Option<oneshot::Sender<()>>,
        outbound: Option<mpsc::Sender<OutboundBatch>>,
        outbound_operations: Option<WireOperationGate>,
    ) -> Result<SessionId, WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::RegisterSession {
                peer,
                cancellation,
                outbound,
                outbound_operations,
                reply,
            })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    pub async fn session_closed(&self, id: SessionId) -> Result<(), WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::SessionClosed {
                id,
                reply: Some(reply),
            })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)
    }

    pub(crate) fn try_session_closed(&self, id: SessionId) {
        match self
            .sender
            .try_send(WorldCommand::SessionClosed { id, reply: None })
        {
            Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => {}
            Err(mpsc::error::TrySendError::Full(command)) => {
                let sender = self.sender.clone();
                drop(tokio::spawn(async move {
                    let _ = sender.send(command).await;
                }));
            }
        }
    }

    pub async fn claim_identity(
        &self,
        session: SessionId,
        nickname: impl Into<String>,
    ) -> Result<IdentityBinding, WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::ClaimIdentity {
                session,
                nickname: nickname.into(),
                reply,
            })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    pub async fn authorize_identity(
        &self,
        session: SessionId,
    ) -> Result<IdentityBinding, WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::AuthorizeIdentity { session, reply })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    pub async fn begin_migration(
        &self,
        session: SessionId,
        channel: ChannelBinding,
        token: MigrationToken,
        now: Instant,
    ) -> Result<MigrationPermit, WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::BeginMigration {
                session,
                channel,
                token,
                now,
                reply,
            })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    #[cfg(test)]
    pub(crate) async fn complete_migration(
        &self,
        destination: SessionId,
        user_no: UserNo,
        channel_id: u16,
        token: MigrationToken,
        now: Instant,
    ) -> Result<MigrationCompletion, WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::CompleteMigration {
                destination,
                user_no,
                channel_id,
                token,
                now,
                reply,
            })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) async fn preflight_migration(
        &self,
        destination: SessionId,
        user_no: UserNo,
        channel_id: u16,
        token: MigrationToken,
        now: Instant,
    ) -> Result<RegisteredMigrationPreflight, WorldError> {
        let completion = self
            .myroom_completions
            .reserve()
            .await
            .map_err(|_| WorldError::Stopped)?;
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::PreflightMigration {
                destination,
                user_no,
                channel_id,
                token,
                now,
                completion,
                reply,
            })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    #[cfg(test)]
    pub(crate) async fn complete_preflighted_migration(
        &self,
        preflight: RegisteredMigrationPreflight,
        profile: MyRoomProfileLease,
    ) -> Result<MigrationCompletion, WorldError> {
        let (reply, response) = oneshot::channel();
        preflight.submit(profile, reply);
        response.await.map_err(|_| WorldError::Stopped)?
    }

    pub(crate) async fn complete_preflighted_migration_with_acknowledgement(
        &self,
        preflight: RegisteredMigrationPreflight,
        profile: MyRoomProfileLease,
        acknowledgement: Vec<u8>,
    ) -> Result<MigrationCompletion, WorldError> {
        let (reply, response) = oneshot::channel();
        preflight.submit_with_acknowledgement(
            profile,
            MigrationAcknowledgement::Ordered(acknowledgement),
            reply,
        );
        response.await.map_err(|_| WorldError::Stopped)?
    }

    #[cfg(test)]
    pub(crate) async fn room_protocol(
        &self,
        session: SessionId,
        payload: RoomCommandPayload,
    ) -> Result<(), WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::RoomProtocol {
                session,
                payload: Box::new(payload),
                reply,
            })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) async fn publish_room_equipment(
        &self,
        session: SessionId,
        snapshot: [u8; 65],
    ) -> Result<(), WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::PublishRoomEquipment {
                session,
                snapshot: Box::new(snapshot),
                reply,
            })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    #[cfg(test)]
    pub(crate) async fn lobby_command(
        &self,
        session: SessionId,
        payload: LobbyCommandPayload,
    ) -> Result<LobbyCommandOutcome, WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::Lobby {
                session,
                payload,
                reply,
            })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    #[cfg(test)]
    pub(crate) async fn race_command(
        &self,
        session: SessionId,
        payload: RaceCommandPayload,
    ) -> Result<RaceCommandOutcome, WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::Race {
                session,
                payload,
                reply,
            })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    pub async fn create_room(&self) -> Result<RoomId, WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::CreateRoom { reply })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    pub async fn join_room(
        &self,
        room: RoomId,
        identity: impl Into<String>,
    ) -> Result<SlotId, WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::JoinRoom {
                room,
                identity: identity.into(),
                reply,
            })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    /// Applies a room mutation only if `session` is still the current owner of
    /// its identity at the moment this command reaches the world actor.
    ///
    /// Packet handlers must use session-bound mutation methods such as this one
    /// instead of authorizing in one command and mutating in a later command.
    pub async fn join_room_for_session(
        &self,
        room: RoomId,
        session: SessionId,
    ) -> Result<SlotId, WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::JoinRoomForSession {
                room,
                session,
                reply,
            })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    pub async fn leave_room(&self, identity: impl Into<String>) -> Result<(), WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::LeaveRoom {
                identity: identity.into(),
                reply,
            })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    pub async fn room_snapshot(&self, room: RoomId) -> Result<RoomSnapshot, WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::RoomSnapshot { room, reply })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response
            .await
            .map_err(|_| WorldError::Stopped)?
            .map_err(WorldError::from)
    }

    pub async fn session_count(&self) -> Result<usize, WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::SessionCount { reply })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) async fn take_due_reward_tasks(
        &self,
        now: Instant,
        maximum: usize,
    ) -> Result<Vec<RewardSettlementTask>, WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::TakeDueRewardTasks {
                now,
                maximum,
                reply,
            })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) async fn complete_reward_task(
        &self,
        completion: RewardPersistenceCompletion,
        now: Instant,
    ) -> Result<RewardCompletionDisposition, WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::CompleteRewardTask {
                completion,
                now,
                reply,
            })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    pub async fn quiesce(&self) -> Result<(), WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::Quiesce { reply })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)
    }

    /// Retries every actor-owned wire publication that was already pending at
    /// the quiesce boundary. `true` is a stable producer barrier: timer, UDP,
    /// migration-expiry, and session-close paths cannot create another batch
    /// after it is observed.
    pub(crate) async fn drain_outbound_producers_once(&self) -> Result<bool, WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::DrainOutboundProducers { reply })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    /// Cancels and retires every session through the World actor before the
    /// transport tasks are aborted by the runtime supervisor.
    pub(crate) async fn drain_sessions(&self) -> Result<(), WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::DrainSessions { reply })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    #[cfg(test)]
    pub(crate) async fn persist_myroom_owner_info(
        &self,
        session: SessionId,
        proposed: MyRoomInfo,
        admission: ProfileJobAdmission,
    ) -> Result<MyRoomInfoWriteReceipt, MyRoomInfoWriteError> {
        let completion = self.myroom_completions.reserve().await?;
        let prepared = PreparedMyRoomInfoWrite::new(admission, proposed, completion);
        self.persist_prepared_myroom_owner_info(session, prepared)
            .await
    }

    pub(crate) async fn reserve_rider_equipment_completion(
        &self,
    ) -> Result<crate::myroom_persistence::MyRoomCompletionSlot, RiderEquipmentWriteError> {
        self.myroom_completions
            .reserve()
            .await
            .map_err(|_| RiderEquipmentWriteError::CompletionMailboxClosed)
    }

    #[cfg(test)]
    pub(crate) async fn persist_rider_equipment(
        &self,
        session: SessionId,
        prepared: PreparedRiderEquipmentWrite,
    ) -> Result<RiderEquipmentWriteReceipt, RiderEquipmentWriteError> {
        let (request_reply, request_response) = oneshot::channel();
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::RegisterRiderEquipmentWrite {
                session,
                prepared,
                request_reply,
                reply,
            })
            .await
            .map_err(|_| RiderEquipmentWriteError::WorldStopped)?;
        let registered = response
            .await
            .map_err(|_| RiderEquipmentWriteError::WorldStopped)??;
        // The actor-minted ticket, profile lane, and pre-reserved completion
        // capability are transferred without an intervening await.
        registered.submit();
        request_response
            .await
            .map_err(|_| RiderEquipmentWriteError::WorldStopped)?
    }

    #[cfg(test)]
    pub(crate) async fn persist_myroom_owner_info_with_test_hook(
        &self,
        session: SessionId,
        proposed: MyRoomInfo,
        admission: ProfileJobAdmission,
        test_hook: crate::myroom_persistence::MyRoomPersistenceTestHook,
    ) -> Result<MyRoomInfoWriteReceipt, MyRoomInfoWriteError> {
        let completion = self.myroom_completions.reserve().await?;
        let prepared =
            PreparedMyRoomInfoWrite::new(admission, proposed, completion).with_test_hook(test_hook);
        self.persist_prepared_myroom_owner_info(session, prepared)
            .await
    }

    #[cfg(test)]
    async fn persist_prepared_myroom_owner_info(
        &self,
        session: SessionId,
        prepared: PreparedMyRoomInfoWrite,
    ) -> Result<MyRoomInfoWriteReceipt, MyRoomInfoWriteError> {
        let (request_reply, request_response) = oneshot::channel();
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::RegisterMyRoomInfoWrite {
                session,
                prepared,
                request_reply,
                reply,
            })
            .await
            .map_err(|_| MyRoomInfoWriteError::WorldStopped)?;
        let registered = response
            .await
            .map_err(|_| MyRoomInfoWriteError::WorldStopped)??;
        // No await is allowed between receiving the actor-minted capability
        // and transferring it to the profile runtime.
        registered.submit();
        request_response
            .await
            .map_err(|_| MyRoomInfoWriteError::WorldStopped)?
    }

    #[cfg(test)]
    pub(crate) async fn myroom_session_view(
        &self,
        session: SessionId,
    ) -> Result<Option<MyRoomSessionView>, WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::MyRoomSessionView { session, reply })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    pub(crate) async fn drain_myroom_completions(&self) -> Result<(), MyRoomCompletionDrainError> {
        self.myroom_completions.drain_barrier().await
    }

    pub async fn reward_drain_status(&self) -> Result<RewardDrainStatus, WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::RewardDrainStatus { reply })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    pub async fn retry_reward_dead_letter(
        &self,
        dead_letter: RewardDeadLetter,
    ) -> Result<(), WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::RetryRewardDeadLetter { dead_letter, reply })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    pub async fn shutdown(&self) -> Result<(), WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::Shutdown { reply })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    /// Unconditionally terminates the actor during process-fatal teardown.
    ///
    /// Normal shutdown must use [`Self::shutdown`], whose reward-lane barrier
    /// prevents silent loss of issued or dead-lettered work.
    pub(crate) async fn force_shutdown(&self) -> Result<WorldForceShutdownReport, WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::ForceShutdown { reply })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)
    }
}

impl AdmittedWorldHandle<'_> {
    fn operation_belongs_to_world(&self) -> bool {
        self.operation.belongs_to(&self.world.identity_instance)
    }

    #[must_use]
    pub(crate) fn session_id(&self) -> SessionId {
        self.operation.binding().owner
    }

    pub(crate) const fn operation(&self) -> &IdentityOperationLease {
        self.operation
    }

    async fn send(&self, command: WorldCommand) -> Result<(), WorldError> {
        if !self.operation_belongs_to_world() {
            return Err(WorldError::ForeignIdentityOperation);
        }
        let operation = self.operation.try_retain().map_err(WorldError::from)?;
        self.world
            .sender
            .send(WorldCommand::AdmittedIdentityOperation {
                operation,
                command: Box::new(command),
            })
            .await
            .map_err(|_| WorldError::Stopped)
    }

    pub(crate) async fn authorize_identity(&self) -> Result<IdentityBinding, WorldError> {
        let (reply, response) = oneshot::channel();
        self.send(WorldCommand::AuthorizeIdentity {
            session: self.session_id(),
            reply,
        })
        .await?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    pub(crate) async fn begin_migration(
        &self,
        channel: ChannelBinding,
        token: MigrationToken,
        now: Instant,
    ) -> Result<MigrationPermit, WorldError> {
        let (reply, response) = oneshot::channel();
        self.send(WorldCommand::BeginMigration {
            session: self.session_id(),
            channel,
            token,
            now,
            reply,
        })
        .await?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    pub(crate) async fn room_protocol(
        &self,
        payload: RoomCommandPayload,
    ) -> Result<(), WorldError> {
        let (reply, response) = oneshot::channel();
        self.send(WorldCommand::RoomProtocol {
            session: self.session_id(),
            payload: Box::new(payload),
            reply,
        })
        .await?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    pub(crate) async fn refresh_myroom_presentation(
        &self,
        expected: IdentityBinding,
        profile: MyRoomProfileLease,
    ) -> Result<bool, WorldError> {
        let (reply, response) = oneshot::channel();
        self.send(WorldCommand::RefreshMyRoomPresentation {
            session: self.session_id(),
            expected,
            profile: Box::new(profile),
            reply,
        })
        .await?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    pub(crate) async fn lobby_command(
        &self,
        payload: LobbyCommandPayload,
    ) -> Result<LobbyCommandOutcome, WorldError> {
        let (reply, response) = oneshot::channel();
        self.send(WorldCommand::Lobby {
            session: self.session_id(),
            payload,
            reply,
        })
        .await?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    pub(crate) async fn race_command(
        &self,
        payload: RaceCommandPayload,
    ) -> Result<RaceCommandOutcome, WorldError> {
        let (reply, response) = oneshot::channel();
        self.send(WorldCommand::Race {
            session: self.session_id(),
            payload,
            reply,
        })
        .await?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    pub(crate) async fn prepare_myroom_command(&self) -> Result<MyRoomCommandPlan, WorldError> {
        let (reply, response) = oneshot::channel();
        self.send(WorldCommand::PrepareMyRoom {
            session: self.session_id(),
            reply,
        })
        .await?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    pub(crate) async fn prepare_myroom_owner_items(
        &self,
    ) -> Result<MyRoomOwnerItemsPlan, WorldError> {
        let (reply, response) = oneshot::channel();
        self.send(WorldCommand::PrepareMyRoomOwnerItems {
            session: self.session_id(),
            reply,
        })
        .await?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    pub(crate) async fn myroom_command(
        &self,
        payload: MyRoomCommandPayload,
        prepared: MyRoomPreparedCommand,
    ) -> Result<(), WorldError> {
        let (reply, response) = oneshot::channel();
        self.send(WorldCommand::MyRoom {
            session: self.session_id(),
            payload,
            prepared: Box::new(prepared),
            reply,
        })
        .await?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    pub(crate) async fn myroom_peer_command(
        &self,
        payload: MyRoomPeerCommandPayload,
    ) -> Result<(), WorldError> {
        let (reply, response) = oneshot::channel();
        self.send(WorldCommand::MyRoomPeer {
            session: self.session_id(),
            payload,
            reply,
        })
        .await?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    pub(crate) async fn publish_myroom_owner_items(
        &self,
        prepared: MyRoomPreparedOwnerItems,
    ) -> Result<(), WorldError> {
        let (reply, response) = oneshot::channel();
        self.send(WorldCommand::PublishMyRoomOwnerItems {
            session: self.session_id(),
            prepared: Box::new(prepared),
            reply,
        })
        .await?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    pub(crate) async fn myroom_session_view(
        &self,
    ) -> Result<Option<MyRoomSessionView>, WorldError> {
        let (reply, response) = oneshot::channel();
        self.send(WorldCommand::MyRoomSessionView {
            session: self.session_id(),
            reply,
        })
        .await?;
        response.await.map_err(|_| WorldError::Stopped)?
    }

    pub(crate) async fn persist_myroom_owner_info(
        &self,
        proposed: MyRoomInfo,
        admission: ProfileJobAdmission,
    ) -> Result<MyRoomInfoWriteReceipt, MyRoomInfoWriteError> {
        if !self.operation_belongs_to_world() {
            return Err(MyRoomInfoWriteError::ForeignIdentityOperation);
        }
        let completion = self.world.myroom_completions.reserve().await?;
        let prepared = PreparedMyRoomInfoWrite::new(admission, proposed, completion);
        let (request_reply, request_response) = oneshot::channel();
        let (reply, response) = oneshot::channel();
        let operation = self
            .operation
            .try_retain()
            .map_err(|_| MyRoomInfoWriteError::WorldStopped)?;
        self.world
            .sender
            .send(WorldCommand::AdmittedIdentityOperation {
                operation,
                command: Box::new(WorldCommand::RegisterMyRoomInfoWrite {
                    session: self.session_id(),
                    prepared,
                    request_reply,
                    reply,
                }),
            })
            .await
            .map_err(|_| MyRoomInfoWriteError::WorldStopped)?;
        let registered = response
            .await
            .map_err(|_| MyRoomInfoWriteError::WorldStopped)??;
        registered.submit();
        request_response
            .await
            .map_err(|_| MyRoomInfoWriteError::WorldStopped)?
    }

    pub(crate) async fn reserve_rider_equipment_completion(
        &self,
    ) -> Result<MyRoomCompletionSlot, RiderEquipmentWriteError> {
        if !self.operation_belongs_to_world() {
            return Err(RiderEquipmentWriteError::ForeignIdentityOperation);
        }
        self.world.reserve_rider_equipment_completion().await
    }

    pub(crate) async fn persist_rider_equipment(
        &self,
        prepared: PreparedRiderEquipmentWrite,
    ) -> Result<RiderEquipmentWriteReceipt, RiderEquipmentWriteError> {
        if !self.operation_belongs_to_world() {
            return Err(RiderEquipmentWriteError::ForeignIdentityOperation);
        }
        let (request_reply, request_response) = oneshot::channel();
        let (reply, response) = oneshot::channel();
        let operation = self
            .operation
            .try_retain()
            .map_err(|_| RiderEquipmentWriteError::WorldStopped)?;
        self.world
            .sender
            .send(WorldCommand::AdmittedIdentityOperation {
                operation,
                command: Box::new(WorldCommand::RegisterRiderEquipmentWrite {
                    session: self.session_id(),
                    prepared,
                    request_reply,
                    reply,
                }),
            })
            .await
            .map_err(|_| RiderEquipmentWriteError::WorldStopped)?;
        let registered = response
            .await
            .map_err(|_| RiderEquipmentWriteError::WorldStopped)??;
        registered.submit();
        request_response
            .await
            .map_err(|_| RiderEquipmentWriteError::WorldStopped)?
    }
}

#[derive(Debug)]
struct World {
    sessions: HashMap<SessionId, SessionState>,
    identities: IdentityRegistry,
    myroom: MyRoomHub,
    rooms: HashMap<RoomId, RoomSnapshot>,
    room_by_identity: HashMap<String, RoomId>,
    protocol_rooms: HashMap<RoomId, ProtocolRoomState>,
    protocol_room_by_user: HashMap<UserNo, RoomId>,
    free_protocol_room_ids: BTreeSet<u16>,
    identity_lifecycle: VecDeque<IdentityLifecycleEvent>,
    identity_capacity: NonZeroUsize,
    next_session_id: u64,
    next_room_id: u32,
    /// The next process-global epoch is consumed only after a race start and
    /// its complete outbound fan-out have committed.
    next_race_epoch: Option<GlobalRaceEpoch>,
    /// At most one unsettled race may own a human user. Lanes survive session
    /// and room membership changes and are released only by their exact fence.
    reward_lanes: HashMap<UserNo, RaceFence>,
    next_reward_attempt_id: Option<NonZeroU64>,
    pending_myroom_writes: HashMap<MyRoomProfileTicketId, PendingMyRoomInfoWrite>,
    pending_myroom_by_user: HashMap<UserNo, MyRoomProfileTicketId>,
    pending_rider_equipment_writes: HashMap<MyRoomProfileTicketId, PendingRiderEquipmentWrite>,
    pending_rider_equipment_by_user: HashMap<UserNo, MyRoomProfileTicketId>,
    next_myroom_ticket: Option<MyRoomProfileTicketId>,
    quiescing: bool,
    outbound_producers_sealed: bool,
    /// Exact actor-scoped binding supplied by an
    /// `AdmittedIdentityOperation`. This is never persisted across mailbox
    /// turns; the enclosing non-clone lease remains live for the full dispatch.
    admitted_identity: Option<IdentityBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum IdentityLifecycleEvent {
    Announce(IdentityBinding),
    Advance {
        previous: ReleasedIdentity,
        next: IdentityBinding,
    },
    Release(ReleasedIdentity),
}

#[derive(Debug)]
struct SessionState {
    peer: SocketAddr,
    cancellation: Option<oneshot::Sender<()>>,
    outbound: Option<mpsc::Sender<OutboundBatch>>,
    outbound_operations: Option<WireOperationGate>,
}

impl SessionState {
    fn track_outbound(&self, batch: OutboundBatch) -> Option<OutboundBatch> {
        let Some(operations) = &self.outbound_operations else {
            return Some(batch);
        };
        operations
            .try_begin_outbound()
            .map(|operation| batch.track(operation))
    }
}

#[derive(Debug)]
struct PendingMyRoomInfoWrite {
    expected: IdentityBinding,
    proposed: MyRoomInfo,
    echo: ReservedOutbound,
    reply: oneshot::Sender<Result<MyRoomInfoWriteReceipt, MyRoomInfoWriteError>>,
}

#[derive(Debug)]
struct PendingRiderEquipmentWrite {
    expected: IdentityBinding,
    requested: p5136_core::equipment_protocol::RiderItemSelection,
    reply: Option<oneshot::Sender<Result<RiderEquipmentWriteReceipt, RiderEquipmentWriteError>>>,
    close_requested: bool,
    deferred_close_replies: Vec<oneshot::Sender<()>>,
}

#[derive(Debug)]
struct RoomEquipmentPlan {
    room_id: RoomId,
    user_no: UserNo,
    snapshot: [u8; 65],
    packet: Vec<u8>,
    recipients: Vec<UserNo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MyRoomCompletionIdentityState {
    Active,
    Ownerless,
    Superseded,
    Released,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProtocolRoomMember {
    user_no: UserNo,
    player: RoomPlayer,
    kart_physics: P5136KartPhysicsBlock,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FrozenRaceParticipant {
    identity: IdentityBinding,
    nickname: String,
    player_id: i32,
    observer: bool,
    team: u8,
    result: Option<FrozenHumanResultSnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrozenHumanResultSnapshot {
    kart_id: u16,
    character_id: u16,
    club_mark_logo: i32,
    economy: FrozenResultEconomy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrozenResultEconomy {
    Pending,
    Applied(AppliedTimeReward),
}

impl FrozenResultEconomy {
    const fn applied(self) -> Option<AppliedTimeReward> {
        match self {
            Self::Pending => None,
            Self::Applied(reward) => Some(reward),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrozenAiRaceParticipant {
    player_id: i32,
    team: u8,
    kart_id: i16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FrozenRaceRoster {
    fence: RaceFence,
    concrete_track: u32,
    participants: Vec<FrozenRaceParticipant>,
    ais: Vec<FrozenAiRaceParticipant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SettlementState {
    end_tick: u32,
    deadline: Instant,
    finalization: SettlementFinalization,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SettlementFinalization {
    AwaitingDeadline,
    Persisting {
        ranking: SettlementRanking,
        rewards: Vec<RewardPersistenceEntry>,
    },
    Ready {
        ranking: SettlementRanking,
        rewards: Vec<RewardPersistenceEntry>,
        packets: Vec<Vec<u8>>,
    },
    Failed(SettlementDeadLetterState),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SettlementDeadLetterState {
    ranking: Option<SettlementRanking>,
    rewards: Vec<RewardPersistenceEntry>,
    packets: Option<Vec<Vec<u8>>>,
    failed_user_no: Option<UserNo>,
    failed_nickname: Option<String>,
    reason: RewardTerminalReason,
}

impl SettlementFinalization {
    fn retain_as_dead_letter(
        &mut self,
        reason: RewardTerminalReason,
        failed_user_no: Option<UserNo>,
        failed_nickname: Option<String>,
    ) {
        let previous = std::mem::replace(self, Self::AwaitingDeadline);
        let (ranking, rewards, packets) = match previous {
            Self::Persisting { ranking, rewards } => (Some(ranking), rewards, None),
            Self::Ready {
                ranking,
                rewards,
                packets,
            } => (Some(ranking), rewards, Some(packets)),
            Self::Failed(failed) => {
                *self = Self::Failed(failed);
                return;
            }
            Self::AwaitingDeadline => (None, Vec::new(), None),
        };
        *self = Self::Failed(SettlementDeadLetterState {
            ranking,
            rewards,
            packets,
            failed_user_no,
            failed_nickname,
            reason,
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RewardPersistenceEntry {
    user_no: UserNo,
    nickname: String,
    canonical_nickname: String,
    player_id: i32,
    proposed_reward: TimeReward,
    status: RewardPersistenceStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RewardPersistenceStatus {
    Queued {
        due_at: Instant,
        failure_count: u8,
    },
    InFlight {
        attempt_id: RewardAttemptId,
        failure_count: u8,
        lease_deadline: Instant,
    },
    Durable(AppliedTimeReward),
}

impl RewardPersistenceStatus {
    const fn in_flight_attempt_id(self) -> Option<RewardAttemptId> {
        match self {
            Self::InFlight { attempt_id, .. } => Some(attempt_id),
            Self::Queued { .. } | Self::Durable(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RewardLeaseExpiry {
    NotCurrent,
    Active,
    RetryScheduled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingRaceTime {
    player_id: i32,
    race_time: u32,
    packet: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingBeginSettlement {
    end_tick: u32,
    excluded_user: Option<UserNo>,
    packet: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingRaceFanout {
    race_time: Option<PendingRaceTime>,
    begin_settlement: Option<PendingBeginSettlement>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RaceProgress {
    finish_times: HashMap<i32, u32>,
    settlement: Option<SettlementState>,
    pending_fanouts: VecDeque<PendingRaceFanout>,
    team_gauge_bits: [u32; 2],
}

impl Default for RaceProgress {
    fn default() -> Self {
        Self {
            finish_times: HashMap::new(),
            settlement: None,
            pending_fanouts: VecDeque::new(),
            team_gauge_bits: [0.0_f32.to_bits(); 2],
        }
    }
}

impl RaceProgress {
    fn team_gauge(&self, team: RaceTeam) -> f32 {
        f32::from_bits(self.team_gauge_bits[team_index(team)])
    }

    fn set_team_gauge(&mut self, team: RaceTeam, gauge: f32) {
        self.team_gauge_bits[team_index(team)] = gauge.to_bits();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RankedRaceResult {
    finish_time: u32,
    rank: i32,
    team: Option<ResultTeam>,
    team_points: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SettlementRanking {
    winning_team: Option<ResultTeam>,
    by_player_id: HashMap<i32, RankedRaceResult>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct FrozenParticipantStamp {
    user_no: UserNo,
    generation: IdentityGeneration,
}

impl From<&IdentityBinding> for FrozenParticipantStamp {
    fn from(identity: &IdentityBinding) -> Self {
        Self {
            user_no: identity.user_no,
            generation: identity.generation,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LoadingReadinessCandidate {
    room_id: RoomId,
    race_epoch: GlobalRaceEpoch,
    participant: FrozenParticipantStamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LoadingHandshake {
    Dormant,
    Awaiting {
        expected: HashSet<FrozenParticipantStamp>,
        ready: HashSet<FrozenParticipantStamp>,
        deadline: Instant,
    },
    StartScheduled {
        at: Instant,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProtocolRoomState {
    id: RoomId,
    settings: RoomSettings,
    phase: RoomPhase,
    race_fence: Option<RaceFence>,
    frozen_race: Option<FrozenRaceRoster>,
    race_progress: RaceProgress,
    loading_handshake: LoadingHandshake,
    room_master: i32,
    members_by_id: [Option<ProtocolRoomMember>; ROOM_SLOT_COUNT],
    observers: [Option<ProtocolRoomMember>; ROOM_OBSERVER_COUNT],
    slot_positions: [Option<u8>; ROOM_SLOT_COUNT],
}

type OutboundDelivery = (SessionId, OutboundBatch);

#[derive(Debug)]
struct ProtocolMigrationDelta {
    user_no: UserNo,
    room_id: RoomId,
    expected_room: ProtocolRoomState,
    next_room: Option<ProtocolRoomState>,
    remove_room: bool,
    aborted_loading_fence: Option<RaceFence>,
}

struct ProtocolMigrationCommit<'a> {
    delta: ProtocolMigrationDelta,
    protocol_room_by_user: &'a mut HashMap<UserNo, RoomId>,
    protocol_rooms: &'a mut HashMap<RoomId, ProtocolRoomState>,
    free_protocol_room_ids: &'a mut BTreeSet<u16>,
    reward_lanes: &'a mut HashMap<UserNo, RaceFence>,
}

impl ProtocolMigrationDelta {
    fn lock<'a>(
        self,
        protocol_room_by_user: &'a mut HashMap<UserNo, RoomId>,
        protocol_rooms: &'a mut HashMap<RoomId, ProtocolRoomState>,
        free_protocol_room_ids: &'a mut BTreeSet<u16>,
        reward_lanes: &'a mut HashMap<UserNo, RaceFence>,
    ) -> Result<ProtocolMigrationCommit<'a>, WorldError> {
        if protocol_room_by_user.get(&self.user_no) != Some(&self.room_id)
            || protocol_rooms.get(&self.room_id) != Some(&self.expected_room)
        {
            return Err(WorldError::MigrationProtocolRoomInconsistent {
                user_no: self.user_no.get(),
            });
        }
        Ok(ProtocolMigrationCommit {
            delta: self,
            protocol_room_by_user,
            protocol_rooms,
            free_protocol_room_ids,
            reward_lanes,
        })
    }
}

impl ProtocolMigrationCommit<'_> {
    fn commit(self) {
        let Self {
            delta,
            protocol_room_by_user,
            protocol_rooms,
            free_protocol_room_ids,
            reward_lanes,
        } = self;
        let removed_mapping = protocol_room_by_user.remove(&delta.user_no);
        debug_assert_eq!(removed_mapping, Some(delta.room_id));
        if delta.remove_room {
            let removed_room = protocol_rooms.remove(&delta.room_id);
            debug_assert_eq!(removed_room.as_ref(), Some(&delta.expected_room));
            free_protocol_room_ids
                .insert(u16::try_from(delta.room_id.0).expect("protocol room ID fits in u16"));
        } else if let Some(next_room) = delta.next_room {
            let previous = protocol_rooms.insert(delta.room_id, next_room);
            debug_assert_eq!(previous.as_ref(), Some(&delta.expected_room));
        }
        if let Some(fence) = delta.aborted_loading_fence {
            reward_lanes.retain(|_, owned| *owned != fence);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MyRoomOutboundReservationError {
    session: SessionId,
}

#[derive(Debug)]
struct ReservedOutbound {
    permit: mpsc::OwnedPermit<OutboundBatch>,
    batch: OutboundBatch,
}

/// Exact generations that the actor has already revoked as one bounded batch.
///
/// Shutdown and same-tick migration expiry may retire many identities before
/// their dependent `MyRoom` transitions are committed. Indexing once by the
/// registry-unique user number keeps recipient classification O(1) without
/// weakening the full binding comparison.
#[derive(Default)]
struct UnavailableReleaseIndex<'a> {
    by_user: HashMap<UserNo, &'a ReleasedIdentity>,
}

impl<'a> UnavailableReleaseIndex<'a> {
    fn from_released(released: &'a [ReleasedIdentity]) -> Self {
        let mut by_user = HashMap::with_capacity(released.len());
        for identity in released {
            let previous = by_user.insert(identity.user_no, identity);
            debug_assert!(
                previous.is_none(),
                "the identity registry must mint unique active user numbers"
            );
        }
        Self { by_user }
    }

    fn contains_exact(&self, binding: &IdentityBinding) -> bool {
        self.by_user
            .get(&binding.user_no)
            .is_some_and(|released| released_matches_binding(released, binding))
    }
}

impl ProtocolRoomState {
    fn new(id: RoomId, settings: RoomSettings) -> Self {
        Self {
            id,
            settings,
            phase: RoomPhase::Lobby,
            race_fence: None,
            frozen_race: None,
            race_progress: RaceProgress::default(),
            loading_handshake: LoadingHandshake::Dormant,
            room_master: 0,
            members_by_id: array::from_fn(|_| None),
            observers: array::from_fn(|_| None),
            slot_positions: [None; ROOM_SLOT_COUNT],
        }
    }

    fn add_participant(&mut self, user_no: UserNo, mut participant: RoomParticipant) -> bool {
        if participant.observer {
            let Some(observer_id) = self.observers.iter().position(Option::is_none) else {
                return false;
            };
            participant.player.player_type = 4;
            participant.player.team = 0;
            self.observers[observer_id] = Some(ProtocolRoomMember {
                user_no,
                player: participant.player,
                kart_physics: participant.kart_physics,
            });
            return true;
        }

        let Some(member_id) = self.members_by_id.iter().position(Option::is_none) else {
            return false;
        };
        let Some(slot_id) = self.slot_positions.iter().position(Option::is_none) else {
            return false;
        };

        participant.player.player_type = 2;
        participant.player.team = if matches!(self.settings.game_type, 3 | 4) {
            if slot_id < ROOM_SLOT_COUNT / 2 { 2 } else { 1 }
        } else {
            0
        };
        participant.player.ranking = i32::try_from(self.members_by_id.iter().flatten().count())
            .expect("a room ranking count always fits in i32");
        let member_id = u8::try_from(member_id).expect("an eight-member room ID always fits in u8");
        self.members_by_id[usize::from(member_id)] = Some(ProtocolRoomMember {
            user_no,
            player: participant.player,
            kart_physics: participant.kart_physics,
        });
        self.slot_positions[slot_id] = Some(member_id);
        if self.members_by_id.iter().flatten().count() == 1 {
            self.room_master = i32::from(member_id);
        }
        true
    }

    fn remove_user(&mut self, user_no: UserNo) -> bool {
        if let Some(observer_id) = self.observers.iter().position(|member| {
            member
                .as_ref()
                .is_some_and(|member| member.user_no == user_no)
        }) {
            self.observers[observer_id] = None;
            return true;
        }

        let Some(member_id) = self.members_by_id.iter().position(|member| {
            member
                .as_ref()
                .is_some_and(|member| member.user_no == user_no)
        }) else {
            return false;
        };
        let removed_master = self.room_master
            == i32::try_from(member_id).expect("an eight-member room ID always fits in i32");
        self.members_by_id[member_id] = None;
        let member_id = u8::try_from(member_id).expect("an eight-member room ID always fits in u8");
        if let Some(slot_id) = self
            .slot_positions
            .iter()
            .position(|position| *position == Some(member_id))
        {
            self.slot_positions[slot_id] = None;
        }

        if removed_master {
            self.room_master = self
                .members_by_id
                .iter()
                .position(Option::is_some)
                .map_or(0, |id| {
                    i32::try_from(id).expect("an eight-member room ID always fits in i32")
                });
            if let Ok(master) = usize::try_from(self.room_master)
                && let Some(master) = self.members_by_id[master].as_mut()
            {
                master.player.player_type = 2;
            }
        }
        let mut rankings = self
            .members_by_id
            .iter()
            .enumerate()
            .filter_map(|(id, member)| member.as_ref().map(|member| (id, member.player.ranking)))
            .collect::<Vec<_>>();
        rankings.sort_unstable_by_key(|&(id, ranking)| (ranking, id));
        for (ranking, (member_id, _)) in rankings.into_iter().enumerate() {
            self.members_by_id[member_id]
                .as_mut()
                .expect("the ranking list contains only occupied member IDs")
                .player
                .ranking =
                i32::try_from(ranking).expect("an eight-member room ranking always fits in i32");
        }
        true
    }

    fn is_empty(&self) -> bool {
        self.members_by_id.iter().all(Option::is_none) && self.observers.iter().all(Option::is_none)
    }

    fn user_nos(&self) -> Vec<UserNo> {
        self.members_by_id
            .iter()
            .chain(&self.observers)
            .flatten()
            .map(|member| member.user_no)
            .collect()
    }

    fn member_id(&self, user_no: UserNo) -> Option<usize> {
        self.members_by_id.iter().position(|member| {
            member
                .as_ref()
                .is_some_and(|member| member.user_no == user_no)
        })
    }

    fn observer_id(&self, user_no: UserNo) -> Option<usize> {
        self.observers.iter().position(|member| {
            member
                .as_ref()
                .is_some_and(|member| member.user_no == user_no)
        })
    }

    fn member_by_user_no(&self, user_no: UserNo) -> Option<&ProtocolRoomMember> {
        self.members_by_id
            .iter()
            .chain(&self.observers)
            .flatten()
            .find(|member| member.user_no == user_no)
    }

    fn slot_states(&self) -> [i32; ROOM_SLOT_COUNT] {
        array::from_fn(|member_id| {
            self.members_by_id[member_id]
                .as_ref()
                .map_or(0, |member| member.player.player_type)
        })
    }

    fn wire_slot_positions(&self) -> [i32; ROOM_SLOT_COUNT] {
        array::from_fn(|slot_id| self.slot_positions[slot_id].map_or(-1, i32::from))
    }

    fn select_concrete_track(
        &self,
        race_epoch: u64,
        random_track_candidates: &[u32],
    ) -> Result<u32, LobbyError> {
        if !is_random_track_selector(self.settings.track) {
            return Ok(self.settings.track);
        }
        let mut candidates = random_track_candidates
            .iter()
            .copied()
            .filter(|track| !is_random_track_selector(*track))
            .collect::<Vec<_>>();
        candidates.sort_unstable();
        candidates.dedup();
        if candidates.is_empty() {
            return Err(LobbyError::MissingTrackCandidates);
        }

        // A stable mixer gives random-looking distribution without process
        // entropy or iteration-order dependence. Selection is evaluated once
        // in the pre-commit start transaction and frozen with the race epoch.
        let mut selector = u64::from(self.id.0)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(race_epoch);
        selector ^= selector >> 30;
        selector = selector.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        selector ^= selector >> 27;
        selector = selector.wrapping_mul(0x94D0_49BB_1331_11EB);
        selector ^= selector >> 31;
        let candidate_count =
            u64::try_from(candidates.len()).expect("the candidate count always fits in u64");
        let index = usize::try_from(selector % candidate_count)
            .expect("the candidate modulo result fits in usize");
        Ok(candidates[index])
    }

    fn equipment_player_id(&self, user_no: UserNo) -> Option<i32> {
        if let Some(member_id) = self.members_by_id.iter().position(|member| {
            member
                .as_ref()
                .is_some_and(|member| member.user_no == user_no)
        }) {
            return i32::try_from(member_id).ok();
        }
        self.observers
            .iter()
            .position(|member| {
                member
                    .as_ref()
                    .is_some_and(|member| member.user_no == user_no)
            })
            .and_then(|observer_id| i32::try_from(ROOM_SLOT_COUNT + observer_id).ok())
    }

    fn set_equipment_snapshot(&mut self, user_no: UserNo, snapshot: [u8; 65]) -> bool {
        let updated = if let Some(member) = self
            .members_by_id
            .iter_mut()
            .chain(&mut self.observers)
            .flatten()
            .find(|member| member.user_no == user_no)
        {
            member.player.rider_item_snapshot = snapshot;
            true
        } else {
            false
        };
        if updated
            && let Some(result) = self
                .frozen_race
                .as_mut()
                .and_then(|frozen| {
                    frozen
                        .participants
                        .iter_mut()
                        .find(|participant| participant.identity.user_no == user_no)
                })
                .and_then(|participant| participant.result.as_mut())
        {
            result.character_id = u16::from_le_bytes(
                snapshot[..2]
                    .try_into()
                    .expect("the character field is a fixed two-byte slice"),
            );
            result.kart_id = u16::from_le_bytes(
                snapshot[4..6]
                    .try_into()
                    .expect("the kart field is a fixed two-byte slice"),
            );
        }
        updated
    }

    fn list_entry(&self) -> RoomListEntry {
        let room_id =
            u16::try_from(self.id.0).expect("protocol room IDs are always bounded to u16");
        RoomListEntry {
            room_id: i16::from_le_bytes(room_id.to_le_bytes()),
            room_name: self.settings.room_name.clone(),
            track: self.settings.track,
            locked: !self.settings.password.is_empty(),
            game_type: self.settings.game_type,
            speed_type: self.settings.speed_type,
            started: self.phase != RoomPhase::Lobby,
            available_slots: u8::try_from(ROOM_SLOT_COUNT)
                .expect("the fixed room slot count fits in u8"),
            player_count: u8::try_from(self.members_by_id.iter().flatten().count())
                .expect("the fixed room member count fits in u8"),
        }
    }

    fn session_data(&self) -> RoomSessionData {
        RoomSessionData {
            room_name: self.settings.room_name.clone(),
            password: self.settings.password.clone(),
            game_type: self.settings.game_type,
            speed_type: self.settings.speed_type,
        }
    }

    fn slot_data(&self) -> RoomSlotData {
        let mut slots = RoomSlotData::empty(
            self.settings.track,
            self.settings.room_data_header,
            self.settings.room_data,
            self.room_master,
        );
        for (destination, source) in slots.members_by_id.iter_mut().zip(&self.members_by_id) {
            if let Some(member) = source {
                *destination = WireRoomMember::Player(member.player.clone());
            }
        }
        for (destination, source) in slots.observers.iter_mut().zip(&self.observers) {
            if let Some(member) = source {
                *destination = RoomObserverSlot::Player(RoomObserver {
                    player_type: 4,
                    user_no: member.user_no.get(),
                    p2p_address: member.player.p2p_address,
                    p2p_port: member.player.p2p_port,
                    nickname: member.player.nickname.clone(),
                });
            }
        }
        for (destination, source) in slots.slot_positions.iter_mut().zip(self.slot_positions) {
            *destination = source.map_or(-1, i32::from);
        }
        slots
    }

    fn snapshot(&self) -> RoomSnapshot {
        let mut slots = array::from_fn(|_| None);
        for (slot_id, member_id) in self.slot_positions.iter().enumerate() {
            if let Some(member_id) = member_id {
                slots[slot_id] = self.members_by_id[usize::from(*member_id)]
                    .as_ref()
                    .map(|member| member.player.nickname.clone());
            }
        }
        RoomSnapshot { id: self.id, slots }
    }
}

const fn expected_room_game_type(channel_game_type: u8) -> Option<u8> {
    match channel_game_type {
        67 | 23 => Some(1),
        68 | 24 => Some(3),
        _ => None,
    }
}

const fn source_ipv4(address: IpAddr) -> Ipv4Addr {
    match address {
        IpAddr::V4(address) => address,
        IpAddr::V6(_) => Ipv4Addr::UNSPECIFIED,
    }
}

const fn race_start_tick(server_tick: u32) -> u32 {
    server_tick.wrapping_add(RACE_START_TICK_LEAD)
}

const fn team_index(team: RaceTeam) -> usize {
    match team {
        RaceTeam::Red => 0,
        RaceTeam::Blue => 1,
    }
}

const fn result_team(team: RaceTeam) -> ResultTeam {
    match team {
        RaceTeam::Red => ResultTeam::Red,
        RaceTeam::Blue => ResultTeam::Blue,
    }
}

fn race_team_from_wire(team: u8) -> Result<RaceTeam, RaceError> {
    match team {
        1 => Ok(RaceTeam::Red),
        2 => Ok(RaceTeam::Blue),
        _ => Err(RaceError::InvalidFrozenTeam { team }),
    }
}

fn reward_retry_delay(failure_count: u8) -> Duration {
    let shift = u32::from(failure_count.saturating_sub(1)).min(31);
    REWARD_RETRY_BASE_DELAY
        .saturating_mul(1_u32 << shift)
        .min(REWARD_RETRY_MAX_DELAY)
}

impl Default for World {
    fn default() -> Self {
        let identity_capacity = NonZeroUsize::new(DEFAULT_WORLD_IDENTITY_CAPACITY)
            .expect("the default world identity capacity is nonzero");
        Self {
            sessions: HashMap::new(),
            identities: IdentityRegistry::new(),
            myroom: MyRoomHub::with_identity_capacity(identity_capacity),
            rooms: HashMap::new(),
            room_by_identity: HashMap::new(),
            protocol_rooms: HashMap::new(),
            protocol_room_by_user: HashMap::new(),
            free_protocol_room_ids: BTreeSet::new(),
            identity_lifecycle: VecDeque::new(),
            identity_capacity,
            next_session_id: 1,
            next_room_id: 1,
            next_race_epoch: GlobalRaceEpoch::new(1),
            reward_lanes: HashMap::new(),
            next_reward_attempt_id: Some(NonZeroU64::MIN),
            pending_myroom_writes: HashMap::new(),
            pending_myroom_by_user: HashMap::new(),
            pending_rider_equipment_writes: HashMap::new(),
            pending_rider_equipment_by_user: HashMap::new(),
            next_myroom_ticket: Some(MyRoomProfileTicketId::FIRST),
            quiescing: false,
            outbound_producers_sealed: false,
            admitted_identity: None,
        }
    }
}

impl World {
    /// Reauthorizes a session at an ordinary mailbox boundary, while allowing
    /// an operation admitted before a migration freeze to finish under the
    /// exact binding carried by its enclosing actor-owned lease.
    fn authorize_session_operation(
        &self,
        session: SessionId,
    ) -> Result<IdentityBinding, IdentityError> {
        if let Some(binding) = self
            .admitted_identity
            .as_ref()
            .filter(|binding| binding.owner == session)
        {
            return Ok(binding.clone());
        }
        self.identities.authorize(session)
    }

    #[cfg(test)]
    fn register_session(
        &mut self,
        peer: SocketAddr,
        cancellation: Option<oneshot::Sender<()>>,
        outbound: Option<mpsc::Sender<OutboundBatch>>,
    ) -> Result<SessionId, WorldError> {
        self.register_session_with_operations(peer, cancellation, outbound, None)
    }

    fn register_session_with_operations(
        &mut self,
        peer: SocketAddr,
        cancellation: Option<oneshot::Sender<()>>,
        outbound: Option<mpsc::Sender<OutboundBatch>>,
        outbound_operations: Option<WireOperationGate>,
    ) -> Result<SessionId, WorldError> {
        if self.quiescing {
            return Err(WorldError::SessionRegistrationClosed);
        }
        let id = SessionId::new(self.next_session_id);
        self.next_session_id = self.next_session_id.wrapping_add(1).max(1);
        self.sessions.insert(
            id,
            SessionState {
                peer,
                cancellation,
                outbound,
                outbound_operations,
            },
        );
        Ok(id)
    }

    fn session_ip(&self, session: SessionId) -> Result<IpAddr, WorldError> {
        self.sessions
            .get(&session)
            .map(|state| state.peer.ip())
            .ok_or(WorldError::UnknownSession(session))
    }

    /// Resolves an exact-generation audience from actor-owned state.
    ///
    /// A protocol room is not a racing audience until its start transition has
    /// committed. Observers receive race relays alongside players, while the
    /// source is excluded because the legacy server does not echo slot packets
    /// back to their sender.
    fn racing_udp_targets(&self, source: UserNo) -> Vec<IdentityBinding> {
        let Some(room_id) = self.protocol_room_by_user.get(&source) else {
            return Vec::new();
        };
        let Some(room) = self.protocol_rooms.get(room_id) else {
            return Vec::new();
        };
        if room.phase == RoomPhase::Lobby {
            return Vec::new();
        }
        let Some(frozen) = room.frozen_race.as_ref() else {
            debug_assert!(false, "a non-lobby room must have a frozen race roster");
            return Vec::new();
        };
        debug_assert_eq!(Some(frozen.fence), room.race_fence);
        debug_assert_ne!(frozen.concrete_track, 0);
        let Some(source_identity) = self.identities.active_identity_by_user_no(source) else {
            return Vec::new();
        };
        if !frozen
            .participants
            .iter()
            .any(|participant| participant.identity == source_identity)
        {
            return Vec::new();
        }

        frozen
            .participants
            .iter()
            .filter(|participant| participant.identity.user_no != source)
            .filter_map(|participant| {
                self.exact_identity_in_protocol_room(*room_id, &participant.identity)
            })
            .collect()
    }

    fn myroom_udp_targets(
        &self,
        source: &IdentityBinding,
    ) -> Result<Vec<IdentityBinding>, MyRoomLifecycleError> {
        self.myroom
            .peer_audience_if_member(source)
            .map(|audience| audience.map_or_else(Vec::new, |audience| audience.peers))
            .map_err(|error| myroom_hub_error("UDP audience query", error))
    }

    fn loading_readiness_candidate(
        &self,
        identity: &IdentityBinding,
    ) -> Option<LoadingReadinessCandidate> {
        if self
            .identities
            .active_identity_by_user_no(identity.user_no)
            .as_ref()
            != Some(identity)
        {
            return None;
        }
        let room_id = *self.protocol_room_by_user.get(&identity.user_no)?;
        let room = self.protocol_rooms.get(&room_id)?;
        if room.phase != RoomPhase::Loading {
            return None;
        }
        let frozen = room.frozen_race.as_ref()?;
        if Some(frozen.fence) != room.race_fence
            || !frozen
                .participants
                .iter()
                .any(|participant| participant.identity == *identity)
        {
            return None;
        }
        let participant = FrozenParticipantStamp::from(identity);
        let LoadingHandshake::Awaiting { expected, .. } = &room.loading_handshake else {
            return None;
        };
        expected
            .contains(&participant)
            .then_some(LoadingReadinessCandidate {
                room_id,
                race_epoch: frozen.fence.race_epoch,
                participant,
            })
    }

    fn mark_loading_ready(&mut self, candidate: LoadingReadinessCandidate) -> bool {
        let Some(active) = self
            .identities
            .active_identity_by_user_no(candidate.participant.user_no)
        else {
            return false;
        };
        if FrozenParticipantStamp::from(&active) != candidate.participant {
            return false;
        }
        let Some(room) = self.protocol_rooms.get_mut(&candidate.room_id) else {
            return false;
        };
        if room.phase != RoomPhase::Loading
            || room.race_fence.map(|fence| fence.race_epoch) != Some(candidate.race_epoch)
        {
            return false;
        }
        let Some(frozen) = room.frozen_race.as_ref() else {
            return false;
        };
        if frozen.fence.race_epoch != candidate.race_epoch
            || !frozen
                .participants
                .iter()
                .any(|participant| participant.identity == active)
        {
            return false;
        }
        let LoadingHandshake::Awaiting {
            expected, ready, ..
        } = &mut room.loading_handshake
        else {
            return false;
        };
        expected.contains(&candidate.participant) && ready.insert(candidate.participant)
    }

    fn apply_udp_dispatch_readiness(
        &mut self,
        candidate: Option<LoadingReadinessCandidate>,
        outcome: UdpDispatchOutcome,
    ) -> bool {
        if outcome.action != UdpDispatchAction::TimeSyncReply || outcome.sent_datagrams != 1 {
            return false;
        }
        candidate.is_some_and(|candidate| self.mark_loading_ready(candidate))
    }

    fn advance_loading(&mut self, now: Instant, clock: &ServerClock) {
        let mut reward_rolls = RandomRewardRollSource;
        self.advance_loading_with_reward_source(now, clock, &mut reward_rolls);
    }

    fn advance_loading_with_reward_source(
        &mut self,
        now: Instant,
        clock: &ServerClock,
        reward_rolls: &mut impl RewardRollSource,
    ) {
        if self.quiescing {
            return;
        }
        let loading_rooms = self
            .protocol_rooms
            .iter()
            .filter_map(|(room_id, room)| (room.phase == RoomPhase::Loading).then_some(*room_id))
            .collect::<Vec<_>>();
        for room_id in loading_rooms {
            self.advance_loading_room(room_id, now, clock);
        }
        let running_rooms = self
            .protocol_rooms
            .iter()
            .filter_map(|(room_id, room)| (room.phase == RoomPhase::Running).then_some(*room_id))
            .collect::<Vec<_>>();
        for room_id in running_rooms {
            self.reconcile_running_room(room_id, now, clock);
        }
        let active_race_rooms = self
            .protocol_rooms
            .iter()
            .filter_map(|(room_id, room)| {
                matches!(room.phase, RoomPhase::Running | RoomPhase::Settling).then_some(*room_id)
            })
            .collect::<Vec<_>>();
        for room_id in active_race_rooms {
            let deadline_reached = self.protocol_rooms.get(&room_id).is_some_and(|room| {
                room.phase == RoomPhase::Settling
                    && room
                        .race_progress
                        .settlement
                        .as_ref()
                        .is_some_and(|settlement| now >= settlement.deadline)
            });
            if deadline_reached {
                self.begin_reward_persistence(room_id, now, reward_rolls);
                self.try_finalize_settlement(room_id);
            } else {
                self.try_flush_pending_race_fanouts(room_id);
            }
        }
        self.debug_assert_invariants();
    }

    fn advance_loading_room(&mut self, room_id: RoomId, now: Instant, clock: &ServerClock) {
        let (active_stamps, active_human_count) = self.active_frozen_participant_stamps(room_id);

        if active_human_count == 0 {
            self.abort_loading_room(room_id);
            return;
        }

        self.reconcile_loading_handshake(room_id, &active_stamps, now);
        self.try_start_scheduled_room(room_id, now, clock);
    }

    fn reconcile_running_room(&mut self, room_id: RoomId, now: Instant, clock: &ServerClock) {
        let (_, active_human_count) = self.active_frozen_participant_stamps(room_id);
        if active_human_count != 0 {
            return;
        }

        let end_tick = clock.tick().wrapping_add(SETTLEMENT_TICK_LEAD);
        let deadline = now.checked_add(SETTLEMENT_DELAY).unwrap_or_else(|| {
            tracing::error!(
                room_id = room_id.0,
                "settlement deadline overflowed while reconciling an abandoned Running room"
            );
            now
        });
        let room = self
            .protocol_rooms
            .get_mut(&room_id)
            .expect("the running room list contains existing rooms");
        room.phase = RoomPhase::Settling;
        room.race_progress.settlement = Some(SettlementState {
            end_tick,
            deadline,
            finalization: SettlementFinalization::AwaitingDeadline,
        });
        room.race_progress
            .pending_fanouts
            .push_back(PendingRaceFanout {
                race_time: None,
                begin_settlement: Some(PendingBeginSettlement {
                    end_tick,
                    excluded_user: None,
                    packet: serialize_game_control(ServerGameControl::BeginSettlement, end_tick),
                }),
            });
    }

    fn active_frozen_participant_stamps(
        &self,
        room_id: RoomId,
    ) -> (HashSet<FrozenParticipantStamp>, usize) {
        let room = self
            .protocol_rooms
            .get(&room_id)
            .expect("the loading room list contains existing rooms");
        let frozen = room
            .frozen_race
            .as_ref()
            .expect("a Loading room has a frozen roster");
        let mut active_stamps = HashSet::with_capacity(frozen.participants.len());
        let mut active_human_count = 0;
        for participant in &frozen.participants {
            if self
                .exact_identity_in_protocol_room(room_id, &participant.identity)
                .is_some()
            {
                active_stamps.insert(FrozenParticipantStamp::from(&participant.identity));
                active_human_count += usize::from(!participant.observer);
            }
        }
        (active_stamps, active_human_count)
    }

    fn abort_loading_room(&mut self, room_id: RoomId) {
        let fence = self
            .protocol_rooms
            .get(&room_id)
            .and_then(|room| room.race_fence);
        if let Some(fence) = fence {
            self.release_reward_lanes(fence);
        }
        let room = self
            .protocol_rooms
            .get_mut(&room_id)
            .expect("the loading room list contains existing rooms");
        room.phase = RoomPhase::Lobby;
        room.race_fence = None;
        room.frozen_race = None;
        room.race_progress = RaceProgress::default();
        room.loading_handshake = LoadingHandshake::Dormant;
        for member in room.members_by_id.iter_mut().flatten() {
            member.player.player_type = PlayerSlotState::NotReady as i32;
        }
    }

    fn release_reward_lanes(&mut self, fence: RaceFence) {
        self.reward_lanes.retain(|_, owned| *owned != fence);
    }

    fn quiesce(&mut self) {
        self.quiescing = true;
    }

    fn drain_outbound_producers_once(&mut self) -> Result<bool, WorldOperationError> {
        if !self.quiescing {
            return Err(WorldError::OutboundProducerDrainRequiresQuiesce.into());
        }
        if self.outbound_producers_sealed {
            return Ok(true);
        }

        // No new migration command is admitted after quiesce. Expire every
        // earlier permit in one graceful-only reconciliation turn so a caller
        // holding an abandoned preflight capability cannot keep the producer
        // barrier false until the normal TTL. World::shutdown without this
        // explicit barrier retains its existing outstanding-transfer refusal.
        let now = Instant::now();
        let migration_deadline = now.checked_add(crate::MIGRATION_TTL).unwrap_or(now);
        self.expire_migrations_without_wire(migration_deadline)?;

        // Only retry already-materialized publications. Quiescing freezes every
        // timer-originated state transition, so observing this set empty is a
        // stable boundary rather than a transient zero between heartbeats.
        let pending_rooms = self
            .protocol_rooms
            .iter()
            .filter_map(|(room_id, room)| {
                let ready_settlement =
                    room.race_progress
                        .settlement
                        .as_ref()
                        .is_some_and(|settlement| {
                            matches!(
                                settlement.finalization,
                                SettlementFinalization::Ready { .. }
                            )
                        });
                (!room.race_progress.pending_fanouts.is_empty() || ready_settlement)
                    .then_some(*room_id)
            })
            .collect::<Vec<_>>();
        for room_id in pending_rooms {
            self.try_finalize_settlement(room_id);
        }

        let pending_race_wire = self.protocol_rooms.values().any(|room| {
            !room.race_progress.pending_fanouts.is_empty()
                || room
                    .race_progress
                    .settlement
                    .as_ref()
                    .is_some_and(|settlement| {
                        matches!(
                            settlement.finalization,
                            SettlementFinalization::Ready { .. }
                        )
                    })
        });
        let pending_profile_producers = !self.pending_myroom_writes.is_empty()
            || !self.pending_rider_equipment_writes.is_empty()
            || self.identities.transfer_in_progress_count() != 0
            || self.identities.outstanding_operation_count() != 0;
        let drained = !pending_race_wire && !pending_profile_producers;
        if drained {
            // Completion commands remain admissible for exact capability
            // retirement. Once sealed, this method short-circuits and timer
            // advancement is frozen, so a late stale (or unexpectedly valid)
            // completion can never turn its retained state into new wire.
            self.outbound_producers_sealed = true;
        }
        Ok(drained)
    }

    fn prepare_myroom_command(
        &self,
        session: SessionId,
    ) -> Result<MyRoomCommandPlan, WorldOperationError> {
        let expected = self.authorize_session_operation(session)?;
        let wire = self
            .myroom
            .wire_plan_if_member(&expected)
            .map_err(|source| myroom_hub_error("live wire-plan query", source))?;
        Ok(MyRoomCommandPlan { expected, wire })
    }

    fn prepare_myroom_owner_items(
        &self,
        session: SessionId,
    ) -> Result<MyRoomOwnerItemsPlan, WorldOperationError> {
        let expected = self.authorize_session_operation(session)?;
        let owner = self
            .myroom
            .owner_item_plan_if_member(&expected)
            .map_err(|source| myroom_hub_error("owner-item plan query", source))?;
        Ok(MyRoomOwnerItemsPlan { expected, owner })
    }

    fn myroom_command(
        &mut self,
        session: SessionId,
        payload: MyRoomCommandPayload,
        prepared: MyRoomPreparedCommand,
    ) -> Result<(), WorldOperationError> {
        let identity = self.authorize_session_operation(session)?;
        if identity != prepared.expected {
            return Err(WorldError::MyRoomWirePlanStale { session }.into());
        }
        let projection = match prepared.projection {
            None => {
                let current = self
                    .myroom
                    .wire_plan_if_member(&identity)
                    .map_err(|source| myroom_hub_error("live nonmember revalidation", source))?;
                if current.is_some() {
                    return Err(WorldError::MyRoomWirePlanStale { session }.into());
                }
                None
            }
            Some(projection) => {
                if let Err(source) = self
                    .myroom
                    .revalidate_wire_plan(&identity, projection.plan())
                {
                    if source.is_wire_plan_stale() {
                        return Err(WorldError::MyRoomWirePlanStale { session }.into());
                    }
                    return Err(myroom_hub_error("live wire-plan revalidation", source).into());
                }
                Some(projection)
            }
        };
        match payload {
            MyRoomCommandPayload::FirstState => {
                let slots = projection.map_or_else(
                    || array::from_fn(|_| MyRoomSlot::Empty),
                    |projection| projection.snapshot().slots,
                );
                let packet = serialize_slot_data(&slots).map_err(MyRoomLifecycleError::from)?;
                let reserved = self
                    .try_reserve_myroom_outbound(vec![(session, OutboundBatch::single(packet))])
                    .map_err(|error| WorldError::MyRoomCommandOutboundUnavailable {
                        session: error.session,
                    })?;
                Self::publish_reserved(reserved);
            }
            MyRoomCommandPayload::Secede => {
                let reply = serialize_secede_reply();
                let Some(projection) = projection else {
                    let reserved = self
                        .try_reserve_myroom_outbound(vec![(session, OutboundBatch::single(reply))])
                        .map_err(|error| WorldError::MyRoomCommandOutboundUnavailable {
                            session: error.session,
                        })?;
                    Self::publish_reserved(reserved);
                    return Ok(());
                };

                let transition = self
                    .myroom
                    .leave(&identity)
                    .map_err(|source| myroom_hub_error("secede", source))?;
                self.commit_myroom_command_transition(transition, move |world, outcome| {
                    let mut deliveries = Vec::new();
                    if let MyRoomEffect::Updated(publication) = &outcome.room {
                        let projected = projection.overlay_publication(publication)?;
                        deliveries.extend(
                            world
                                .myroom_publication_deliveries(std::slice::from_ref(&projected))?,
                        );
                    }
                    deliveries.push((session, OutboundBatch::single(reply)));
                    Ok(deliveries)
                })?;
            }
        }
        Ok(())
    }

    fn myroom_peer_command(
        &mut self,
        session: SessionId,
        payload: MyRoomPeerCommandPayload,
    ) -> Result<(), WorldOperationError> {
        let identity = self.authorize_session_operation(session)?;
        let Some(audience) = self
            .myroom
            .peer_audience_if_member(&identity)
            .map_err(|source| myroom_hub_error("peer fanout audience", source))?
        else {
            return Ok(());
        };
        let sender_slot = audience.sender_slot.get();
        let packet = match payload {
            MyRoomPeerCommandPayload::CharacterPosition(request) => {
                if request.slot != sender_slot {
                    tracing::debug!(
                        session_id = session.get(),
                        user_no = identity.user_no.get(),
                        owner = audience.owner.get(),
                        claimed_slot = request.slot,
                        actual_slot = sender_slot,
                        "dropping a MyRoom position update with a spoofed sender slot"
                    );
                    return Ok(());
                }
                serialize_character_position(i32::from(sender_slot), request.transform)
            }
        }
        .map_err(MyRoomLifecycleError::from)?;
        let deliveries = self.myroom_deliveries_from_serialized(
            vec![(audience.peers.as_slice(), packet)],
            &UnavailableReleaseIndex::default(),
        )?;
        let reserved = self
            .try_reserve_myroom_outbound(deliveries)
            .map_err(|error| WorldError::MyRoomCommandOutboundUnavailable {
                session: error.session,
            })?;
        Self::publish_reserved(reserved);
        Ok(())
    }

    fn publish_myroom_owner_items(
        &mut self,
        session: SessionId,
        prepared: MyRoomPreparedOwnerItems,
    ) -> Result<(), WorldOperationError> {
        let identity = self.authorize_session_operation(session)?;
        if identity != prepared.expected {
            return Err(WorldError::MyRoomWirePlanStale { session }.into());
        }

        match prepared.owner.as_ref() {
            Some(plan) => {
                if let Err(source) = self.myroom.revalidate_owner_item_plan(&identity, plan) {
                    if source.is_wire_plan_stale() {
                        return Err(WorldError::MyRoomWirePlanStale { session }.into());
                    }
                    return Err(myroom_hub_error("owner-item plan revalidation", source).into());
                }
            }
            None => {
                match self
                    .myroom
                    .owner_item_plan_if_member(&identity)
                    .map_err(|source| myroom_hub_error("owner-item plan query", source))?
                {
                    None => {}
                    Some(_) => return Err(WorldError::MyRoomWirePlanStale { session }.into()),
                }
            }
        }

        let (packets, profile_lane) = match (prepared.owner.as_ref(), prepared.loaded) {
            (None, None) => (vec![serialize_missing_owner_items()], None),
            (Some(plan), None) if !plan.visible() => (vec![serialize_missing_owner_items()], None),
            (Some(plan), Some(loaded)) if plan.visible() => (loaded.packets, Some(loaded.lane)),
            (None | Some(_), Some(_)) | (Some(_), None) => {
                return Err(WorldError::MyRoomOwnerItemPlanMismatch { session }.into());
            }
        };

        let reserved = self
            .try_reserve_myroom_outbound(vec![(session, OutboundBatch::ordered(packets))])
            .map_err(|error| WorldError::MyRoomCommandOutboundUnavailable {
                session: error.session,
            })?;
        Self::publish_reserved(reserved);
        drop(profile_lane);
        Ok(())
    }

    fn register_myroom_info_write(
        &mut self,
        session: SessionId,
        prepared: PreparedMyRoomInfoWrite,
        reply: oneshot::Sender<Result<MyRoomInfoWriteReceipt, MyRoomInfoWriteError>>,
    ) -> Result<RegisteredMyRoomInfoWrite, MyRoomInfoRegistrationError> {
        if self.quiescing {
            return Err(MyRoomInfoRegistrationError::request(
                MyRoomInfoWriteError::WorldQuiescing,
            ));
        }
        let identity = self.authorize_session_operation(session).map_err(|_| {
            MyRoomInfoRegistrationError::request(MyRoomInfoWriteError::UnauthenticatedSession {
                session,
            })
        })?;
        if canonical_nickname_key(prepared.admitted_nickname())
            != canonical_nickname_key(&identity.nickname)
        {
            return Err(MyRoomInfoRegistrationError::request(
                MyRoomInfoWriteError::ProfileSubjectMismatch {
                    admitted: prepared.admitted_nickname().to_owned(),
                    active: identity.nickname,
                },
            ));
        }
        let membership = self
            .myroom
            .membership_if_member(&identity)
            .map_err(|source| {
                MyRoomInfoRegistrationError::myroom(MyRoomLifecycleError::Hub {
                    operation: "owner-info registration",
                    source,
                })
            })?
            .ok_or_else(|| {
                MyRoomInfoRegistrationError::request(MyRoomInfoWriteError::NotMember {
                    user_no: identity.user_no,
                })
            })?;
        if !membership.is_present_owner(identity.user_no) {
            return Err(MyRoomInfoRegistrationError::request(
                MyRoomInfoWriteError::NotPresentOwner {
                    user_no: identity.user_no,
                },
            ));
        }
        if self.pending_myroom_by_user.contains_key(&identity.user_no)
            || self
                .pending_rider_equipment_by_user
                .contains_key(&identity.user_no)
        {
            return Err(MyRoomInfoRegistrationError::request(
                MyRoomInfoWriteError::AlreadyPending {
                    user_no: identity.user_no,
                },
            ));
        }

        let packet = serialize_myroom_info(prepared.proposed())
            .map_err(MyRoomInfoWriteError::from)
            .map_err(MyRoomInfoRegistrationError::request)?;
        let session_state = self.sessions.get(&session).ok_or_else(|| {
            MyRoomInfoRegistrationError::request(MyRoomInfoWriteError::OutboundUnavailable {
                session,
            })
        })?;
        let outbound = session_state.outbound.clone().ok_or_else(|| {
            MyRoomInfoRegistrationError::request(MyRoomInfoWriteError::OutboundUnavailable {
                session,
            })
        })?;
        let permit = outbound.try_reserve_owned().map_err(|_| {
            MyRoomInfoRegistrationError::request(MyRoomInfoWriteError::OutboundUnavailable {
                session,
            })
        })?;
        let batch = session_state
            .track_outbound(OutboundBatch::single(packet))
            .ok_or_else(|| {
                MyRoomInfoRegistrationError::request(MyRoomInfoWriteError::OutboundUnavailable {
                    session,
                })
            })?;

        let ticket = self.next_myroom_ticket.ok_or_else(|| {
            MyRoomInfoRegistrationError::terminal(
                MyRoomPersistenceInvariantError::TicketIdExhausted,
            )
        })?;
        self.next_myroom_ticket = ticket.successor();
        let pending = PendingMyRoomInfoWrite {
            expected: identity.clone(),
            proposed: prepared.proposed().clone(),
            echo: ReservedOutbound { permit, batch },
            reply,
        };
        let replaced = self.pending_myroom_writes.insert(ticket, pending);
        debug_assert!(replaced.is_none(), "MyRoom ticket IDs are never reused");
        let replaced = self.pending_myroom_by_user.insert(identity.user_no, ticket);
        debug_assert!(
            replaced.is_none(),
            "one user cannot own two pending MyRoom profile tickets"
        );
        self.debug_assert_myroom_persistence();
        Ok(prepared.register(ticket))
    }

    fn register_rider_equipment_write(
        &mut self,
        session: SessionId,
        prepared: PreparedRiderEquipmentWrite,
        reply: oneshot::Sender<Result<RiderEquipmentWriteReceipt, RiderEquipmentWriteError>>,
    ) -> Result<RegisteredRiderEquipmentWrite, RiderEquipmentRegistrationError> {
        if self.quiescing {
            return Err(RiderEquipmentRegistrationError::request(
                RiderEquipmentWriteError::WorldQuiescing,
            ));
        }
        let identity = self.authorize_session_operation(session).map_err(|_| {
            RiderEquipmentRegistrationError::request(
                RiderEquipmentWriteError::UnauthenticatedSession { session },
            )
        })?;
        if canonical_nickname_key(prepared.admitted_nickname())
            != canonical_nickname_key(&identity.nickname)
        {
            return Err(RiderEquipmentRegistrationError::request(
                RiderEquipmentWriteError::ProfileSubjectMismatch {
                    admitted: prepared.admitted_nickname().to_owned(),
                    active: identity.nickname,
                },
            ));
        }
        if self
            .pending_rider_equipment_by_user
            .contains_key(&identity.user_no)
            || self.pending_myroom_by_user.contains_key(&identity.user_no)
        {
            return Err(RiderEquipmentRegistrationError::request(
                RiderEquipmentWriteError::AlreadyPending {
                    user_no: identity.user_no,
                },
            ));
        }

        let ticket = self.next_myroom_ticket.ok_or_else(|| {
            RiderEquipmentRegistrationError::terminal(
                RiderEquipmentPublicationInvariantError::TicketIdExhausted,
            )
        })?;
        self.next_myroom_ticket = ticket.successor();
        let pending = PendingRiderEquipmentWrite {
            expected: identity.clone(),
            requested: prepared.selection(),
            reply: Some(reply),
            close_requested: false,
            deferred_close_replies: Vec::new(),
        };
        let replaced = self.pending_rider_equipment_writes.insert(ticket, pending);
        debug_assert!(
            replaced.is_none(),
            "profile completion ticket IDs are never reused"
        );
        let replaced = self
            .pending_rider_equipment_by_user
            .insert(identity.user_no, ticket);
        debug_assert!(
            replaced.is_none(),
            "one user cannot own two pending rider-equipment tickets"
        );
        self.debug_assert_rider_equipment_persistence();
        Ok(prepared.register(ticket))
    }

    fn myroom_session_view(
        &self,
        session: SessionId,
    ) -> Result<Option<MyRoomSessionView>, WorldOperationError> {
        let identity = self.authorize_session_operation(session)?;
        let Some(membership) = self
            .myroom
            .membership_if_member(&identity)
            .map_err(|source| MyRoomLifecycleError::Hub {
                operation: "session info query",
                source,
            })?
        else {
            return Ok(None);
        };
        let mut info =
            self.myroom
                .room_info(membership.owner)
                .cloned()
                .ok_or(MyRoomLifecycleError::Hub {
                    operation: "session info query",
                    source: MyRoomHubError::RoomMissing {
                        owner: membership.owner,
                    },
                })?;
        let role = if membership.is_present_owner(identity.user_no) {
            MyRoomSessionRole::PresentOwner
        } else {
            MyRoomSessionRole::Visitor
        };
        if role == MyRoomSessionRole::Visitor {
            info.room_password.clear();
            info.item_password.clear();
        }
        Ok(Some(MyRoomSessionView { role, info }))
    }

    fn handle_myroom_profile_completion(
        &mut self,
        completion: MyRoomProfileCompletion,
    ) -> Result<(), MyRoomPersistenceInvariantError> {
        match completion {
            MyRoomProfileCompletion::AbortedBeforeSubmission { ticket } => {
                let pending = self.take_pending_myroom_write(ticket)?;
                let _ = pending
                    .reply
                    .send(Err(MyRoomInfoWriteError::AbortedBeforeSubmission));
                Ok(())
            }
            MyRoomProfileCompletion::AcceptedOutcomeLost { ticket } => {
                drop(self.take_pending_myroom_write(ticket)?);
                Err(MyRoomPersistenceInvariantError::AcceptedOutcomeLost {
                    ticket: ticket.get(),
                })
            }
            MyRoomProfileCompletion::Finished { ticket, result } => {
                self.finish_myroom_profile_write(ticket, *result)
            }
            MyRoomProfileCompletion::RiderEquipment(_) => {
                unreachable!("rider-equipment completions use their dedicated dispatcher")
            }
            MyRoomProfileCompletion::Migration(_) => {
                unreachable!("migration completions use their dedicated dispatcher")
            }
            MyRoomProfileCompletion::DrainBarrier { reply } => {
                let pending = self
                    .pending_myroom_writes
                    .len()
                    .saturating_add(self.pending_rider_equipment_writes.len())
                    .saturating_add(self.identities.transfer_in_progress_count());
                let indexed = self
                    .pending_myroom_by_user
                    .len()
                    .saturating_add(self.pending_rider_equipment_by_user.len());
                if pending == 0 && indexed == 0 {
                    let _ = reply.send(Ok(()));
                    Ok(())
                } else {
                    let _ = reply.send(Err(MyRoomCompletionDrainError::Pending {
                        pending,
                        indexed,
                    }));
                    Err(MyRoomPersistenceInvariantError::PendingAtDrain { pending, indexed })
                }
            }
        }
    }

    fn finish_myroom_profile_write(
        &mut self,
        ticket: MyRoomProfileTicketId,
        result: MyRoomProfileJobResult,
    ) -> Result<(), MyRoomPersistenceInvariantError> {
        let PendingMyRoomInfoWrite {
            expected,
            proposed,
            echo,
            reply,
        } = self.take_pending_myroom_write(ticket)?;
        let completion =
            result.map_err(
                |source| MyRoomPersistenceInvariantError::ProfileInfrastructure {
                    ticket: ticket.get(),
                    source,
                },
            )?;
        let (result, lane) = completion.into_parts();
        let subject_matches = lane
            .subject()
            .matches_nickname(&expected.nickname)
            .map_err(crate::profile_io::ProfileIoError::from)
            .map_err(
                |source| MyRoomPersistenceInvariantError::ProfileInfrastructure {
                    ticket: ticket.get(),
                    source,
                },
            )?;
        if !subject_matches {
            return Err(MyRoomPersistenceInvariantError::CompletionSubjectMismatch {
                ticket: ticket.get(),
                expected: expected.nickname,
                actual: lane.subject().nickname().to_owned(),
            });
        }
        let durable = match result {
            Ok(durable) => durable,
            Err(error) => {
                drop(lane);
                let _ = reply.send(Err(MyRoomInfoWriteError::Persistence(error)));
                return Ok(());
            }
        };
        if durable.info() != &proposed {
            return Err(MyRoomPersistenceInvariantError::DurableValueMismatch {
                ticket: ticket.get(),
            });
        }

        let publication =
            self.publish_durable_myroom_info(ticket, &expected, durable.info(), echo)?;

        let _ = reply.send(Ok(durable.into_receipt(publication)));
        drop(lane);
        self.debug_assert_myroom_persistence();
        Ok(())
    }

    fn handle_rider_equipment_profile_completion(
        &mut self,
        completion: RiderEquipmentProfileCompletion,
    ) -> Result<Vec<oneshot::Sender<()>>, RiderEquipmentPublicationInvariantError> {
        match completion {
            RiderEquipmentProfileCompletion::AbortedBeforeSubmission { ticket } => {
                let mut pending = self.take_pending_rider_equipment_write(ticket)?;
                if let Some(reply) = pending.reply.take() {
                    let _ = reply.send(Err(RiderEquipmentWriteError::AbortedBeforeSubmission));
                }
                self.finish_deferred_equipment_close(ticket, pending)
            }
            RiderEquipmentProfileCompletion::AcceptedOutcomeLost { ticket } => {
                drop(self.take_pending_rider_equipment_write(ticket)?);
                Err(
                    RiderEquipmentPublicationInvariantError::AcceptedOutcomeLost {
                        ticket: ticket.get(),
                    },
                )
            }
            RiderEquipmentProfileCompletion::Finished { ticket, result } => {
                self.finish_rider_equipment_profile_write(ticket, *result)
            }
        }
    }

    fn finish_rider_equipment_profile_write(
        &mut self,
        ticket: MyRoomProfileTicketId,
        result: RiderEquipmentProfileJobResult,
    ) -> Result<Vec<oneshot::Sender<()>>, RiderEquipmentPublicationInvariantError> {
        let mut pending = self.take_pending_rider_equipment_write(ticket)?;
        let completion = result.map_err(|source| {
            RiderEquipmentPublicationInvariantError::ProfileInfrastructure {
                ticket: ticket.get(),
                source,
            }
        })?;
        let (result, lane) = completion.into_parts();
        let subject_matches = lane
            .subject()
            .matches_nickname(&pending.expected.nickname)
            .map_err(crate::profile_io::ProfileIoError::from)
            .map_err(
                |source| RiderEquipmentPublicationInvariantError::ProfileInfrastructure {
                    ticket: ticket.get(),
                    source,
                },
            )?;
        if !subject_matches {
            return Err(
                RiderEquipmentPublicationInvariantError::CompletionSubjectMismatch {
                    ticket: ticket.get(),
                    expected: pending.expected.nickname,
                    actual: lane.subject().nickname().to_owned(),
                },
            );
        }
        let durable = match result {
            Ok(durable) => durable,
            Err(error) => {
                drop(lane);
                if let Some(reply) = pending.reply.take() {
                    let _ = reply.send(Err(RiderEquipmentWriteError::Persistence(error)));
                }
                return self.finish_deferred_equipment_close(ticket, pending);
            }
        };
        if durable.selection() != pending.requested
            || durable.snapshot() != durable.presentation().rider_item_snapshot()
        {
            return Err(
                RiderEquipmentPublicationInvariantError::DurableValueMismatch {
                    ticket: ticket.get(),
                },
            );
        }

        let publication =
            self.publish_durable_rider_equipment(ticket, &pending.expected, &durable)?;
        let receipt = durable.into_receipt(publication);
        if let Some(reply) = pending.reply.take() {
            let _ = reply.send(Ok(receipt));
        }
        drop(lane);
        let close_replies = self.finish_deferred_equipment_close(ticket, pending)?;
        self.debug_assert_rider_equipment_persistence();
        Ok(close_replies)
    }

    fn publish_durable_rider_equipment(
        &mut self,
        ticket: MyRoomProfileTicketId,
        expected: &IdentityBinding,
        durable: &DurableRiderEquipment,
    ) -> Result<RiderEquipmentPublication, RiderEquipmentPublicationInvariantError> {
        let active = self.identities.active_identity_by_user_no(expected.user_no);
        let identity_state = if active.as_ref() == Some(expected) {
            MyRoomCompletionIdentityState::Active
        } else if active.is_none() && self.identities.is_current_ownerless_binding(expected) {
            MyRoomCompletionIdentityState::Ownerless
        } else if active.is_some() {
            MyRoomCompletionIdentityState::Superseded
        } else {
            MyRoomCompletionIdentityState::Released
        };
        if identity_state == MyRoomCompletionIdentityState::Superseded {
            return Ok(RiderEquipmentPublication::PersistedAfterSupersession);
        }
        if identity_state == MyRoomCompletionIdentityState::Released {
            return Ok(RiderEquipmentPublication::PersistedAfterRelease);
        }

        let protocol_plan = if identity_state == MyRoomCompletionIdentityState::Active {
            let mapped_room = self.protocol_room_by_user.get(&expected.user_no).copied();
            let plan = self
                .plan_room_equipment(expected.user_no, *durable.snapshot())
                .map_err(|source| RiderEquipmentPublicationInvariantError::Protocol {
                    ticket: ticket.get(),
                    source,
                })?;
            if let Some(room_id) = mapped_room
                && plan.is_none()
            {
                return Err(
                    RiderEquipmentPublicationInvariantError::ProtocolMembership {
                        ticket: ticket.get(),
                        room_id: room_id.0,
                        user_no: expected.user_no,
                    },
                );
            }
            plan
        } else {
            None
        };
        let transition = match self
            .myroom
            .refresh_profile_if_tracked(expected, durable.presentation())
        {
            Ok(transition) => transition,
            Err(MyRoomHubError::Wire(source)) => {
                tracing::warn!(
                    ticket = ticket.get(),
                    nickname = %expected.nickname,
                    %source,
                    "skipped invalid MyRoom presentation after durable rider-equipment write"
                );
                None
            }
            Err(source) => {
                return Err(RiderEquipmentPublicationInvariantError::Hub {
                    ticket: ticket.get(),
                    source,
                });
            }
        };

        if let Some(transition) = transition {
            transition.commit(&mut self.myroom).map_err(|source| {
                RiderEquipmentPublicationInvariantError::Commit {
                    ticket: ticket.get(),
                    source,
                }
            })?;
        }
        if let Some(plan) = protocol_plan {
            self.commit_room_equipment(plan).map_err(|source| {
                RiderEquipmentPublicationInvariantError::Delivery {
                    ticket: ticket.get(),
                    source: Box::new(source),
                }
            })?;
        }
        self.debug_assert_invariants();
        Ok(if identity_state == MyRoomCompletionIdentityState::Active {
            RiderEquipmentPublication::ActiveCachesUpdated
        } else {
            RiderEquipmentPublication::OwnerlessCachesUpdated
        })
    }

    fn finish_deferred_equipment_close(
        &mut self,
        ticket: MyRoomProfileTicketId,
        pending: PendingRiderEquipmentWrite,
    ) -> Result<Vec<oneshot::Sender<()>>, RiderEquipmentPublicationInvariantError> {
        if pending.close_requested {
            let result = if self.quiescing {
                self.close_session_without_wire(pending.expected.owner, Instant::now())
            } else {
                self.close_session(pending.expected.owner, Instant::now())
            };
            result.map_err(|source| RiderEquipmentPublicationInvariantError::Delivery {
                ticket: ticket.get(),
                source: Box::new(source),
            })?;
        }
        Ok(pending.deferred_close_replies)
    }

    fn publish_durable_myroom_info(
        &mut self,
        ticket: MyRoomProfileTicketId,
        expected: &IdentityBinding,
        persisted: &MyRoomInfo,
        echo: ReservedOutbound,
    ) -> Result<MyRoomInfoPublication, MyRoomPersistenceInvariantError> {
        let active = self.identities.active_identity_by_user_no(expected.user_no);
        let identity_state = if active.as_ref() == Some(expected) {
            MyRoomCompletionIdentityState::Active
        } else if active.is_none() && self.identities.is_current_ownerless_binding(expected) {
            MyRoomCompletionIdentityState::Ownerless
        } else if active.is_some() {
            MyRoomCompletionIdentityState::Superseded
        } else {
            MyRoomCompletionIdentityState::Released
        };
        let (operation, success, publish_echo) = match identity_state {
            MyRoomCompletionIdentityState::Active => (
                "active owner-info completion",
                MyRoomInfoPublication::ActiveOwnerEchoed,
                true,
            ),
            MyRoomCompletionIdentityState::Ownerless => (
                "ownerless owner-info completion",
                MyRoomInfoPublication::OwnerlessGenerationUpdated,
                false,
            ),
            MyRoomCompletionIdentityState::Superseded => {
                drop(echo);
                return Ok(MyRoomInfoPublication::PersistedAfterSupersession);
            }
            MyRoomCompletionIdentityState::Released => {
                drop(echo);
                return Ok(MyRoomInfoPublication::PersistedAfterRelease);
            }
        };

        let transition = match self.myroom.update_owner_info(expected, persisted.clone()) {
            Ok(transition) => transition,
            Err(MyRoomHubError::NotMember { .. } | MyRoomHubError::NotPresentOwner { .. }) => {
                drop(echo);
                return Ok(MyRoomInfoPublication::PersistedAfterRoleChange);
            }
            Err(source) => {
                return Err(MyRoomPersistenceInvariantError::Hub {
                    ticket: ticket.get(),
                    operation,
                    source,
                });
            }
        };
        transition.commit(&mut self.myroom).map_err(|source| {
            MyRoomPersistenceInvariantError::Commit {
                ticket: ticket.get(),
                source,
            }
        })?;
        debug_assert_eq!(self.myroom.audit_invariants(), Ok(()));
        if publish_echo {
            Self::publish_reserved(vec![echo]);
        } else {
            drop(echo);
        }
        Ok(success)
    }

    fn take_pending_myroom_write(
        &mut self,
        ticket: MyRoomProfileTicketId,
    ) -> Result<PendingMyRoomInfoWrite, MyRoomPersistenceInvariantError> {
        let pending = self.pending_myroom_writes.remove(&ticket).ok_or(
            MyRoomPersistenceInvariantError::UnknownTicket {
                ticket: ticket.get(),
            },
        )?;
        let actual = self
            .pending_myroom_by_user
            .remove(&pending.expected.user_no);
        if actual != Some(ticket) {
            return Err(MyRoomPersistenceInvariantError::PendingIndexMismatch {
                user_no: pending.expected.user_no,
                expected: ticket.get(),
                actual: actual.map(MyRoomProfileTicketId::get),
            });
        }
        self.debug_assert_myroom_persistence();
        Ok(pending)
    }

    fn debug_assert_myroom_persistence(&self) {
        debug_assert_eq!(
            self.pending_myroom_writes.len(),
            self.pending_myroom_by_user.len()
        );
        debug_assert!(self.pending_myroom_writes.iter().all(|(ticket, pending)| {
            self.pending_myroom_by_user.get(&pending.expected.user_no) == Some(ticket)
        }));
    }

    fn take_pending_rider_equipment_write(
        &mut self,
        ticket: MyRoomProfileTicketId,
    ) -> Result<PendingRiderEquipmentWrite, RiderEquipmentPublicationInvariantError> {
        let pending = self.pending_rider_equipment_writes.remove(&ticket).ok_or(
            RiderEquipmentPublicationInvariantError::UnknownTicket {
                ticket: ticket.get(),
            },
        )?;
        let actual = self
            .pending_rider_equipment_by_user
            .remove(&pending.expected.user_no);
        if actual != Some(ticket) {
            return Err(
                RiderEquipmentPublicationInvariantError::PendingIndexMismatch {
                    user_no: pending.expected.user_no,
                    expected: ticket.get(),
                    actual: actual.map(MyRoomProfileTicketId::get),
                },
            );
        }
        self.debug_assert_rider_equipment_persistence();
        Ok(pending)
    }

    fn debug_assert_rider_equipment_persistence(&self) {
        debug_assert_eq!(
            self.pending_rider_equipment_writes.len(),
            self.pending_rider_equipment_by_user.len()
        );
        debug_assert!(
            self.pending_rider_equipment_writes
                .iter()
                .all(|(ticket, pending)| {
                    self.pending_rider_equipment_by_user
                        .get(&pending.expected.user_no)
                        == Some(ticket)
                })
        );
    }

    fn defer_session_close_for_rider_equipment(
        &mut self,
        session: SessionId,
        reply: &mut Option<oneshot::Sender<()>>,
    ) -> bool {
        let Some(pending) = self
            .pending_rider_equipment_writes
            .values_mut()
            .find(|pending| pending.expected.owner == session)
        else {
            return false;
        };
        pending.close_requested = true;
        if let Some(reply) = reply.take() {
            pending.deferred_close_replies.push(reply);
        }
        if let Some(mut state) = self.sessions.remove(&session)
            && let Some(cancellation) = state.cancellation.take()
        {
            let _ = cancellation.send(());
        }
        true
    }

    fn drain_sessions_for_shutdown(&mut self) -> Result<(), WorldOperationError> {
        if !self.quiescing {
            return Err(WorldError::SessionDrainRequiresQuiesce.into());
        }
        let active_operations = self.identities.outstanding_operation_count();
        if active_operations != 0 {
            return Err(WorldError::IdentityOperationShutdownBlocked {
                active: active_operations,
            }
            .into());
        }

        // Session tasks receive cancellation while their receivers still
        // exist. The actor then revokes every connected or ownerless identity
        // in one exact release set before any transport task can report a
        // duplicate close.
        self.cancel_all_sessions();
        self.sessions.clear();
        let released = self.identities.drain_active();
        let unavailable = UnavailableReleaseIndex::from_released(&released);
        for identity in &released {
            // Every possible recipient belongs to this same shutdown release
            // set, so actor state is fully reconciled without publishing a
            // snapshot to transports that are about to be aborted.
            drop(self.release_identity_state_skipping(identity, &unavailable)?);
        }

        let rooms = self.myroom.room_count();
        let memberships = self.myroom.member_count();
        let generations = self.myroom.generation_count();
        if rooms != 0 || memberships != 0 || generations != 0 {
            return Err(MyRoomLifecycleError::ShutdownStateRetained {
                rooms,
                memberships,
                generations,
            }
            .into());
        }
        self.debug_assert_invariants();
        Ok(())
    }

    fn reward_drain_status(&self) -> Result<RewardDrainStatus, WorldError> {
        let mut owned_lanes = self
            .reward_lanes
            .iter()
            .map(|(user_no, fence)| (*user_no, *fence))
            .collect::<Vec<_>>();
        owned_lanes.sort_unstable_by_key(|(user_no, fence)| {
            (fence.room_id.0, fence.race_epoch.get(), user_no.get())
        });
        let mut outstanding_lanes = Vec::with_capacity(owned_lanes.len());
        for (user_no, fence) in owned_lanes {
            outstanding_lanes.push(self.outstanding_reward_lane(user_no, fence)?);
        }
        let dead_letters = self.reward_dead_letters()?;
        Ok(RewardDrainStatus {
            quiescing: self.quiescing,
            outstanding_lanes,
            dead_letters,
        })
    }

    fn outstanding_reward_lane(
        &self,
        user_no: UserNo,
        fence: RaceFence,
    ) -> Result<OutstandingRewardLane, WorldError> {
        let invariant = || WorldError::RewardSchedulerInvariant {
            room_id: fence.room_id.0,
            user_no: user_no.get(),
        };
        let room = self
            .protocol_rooms
            .get(&fence.room_id)
            .filter(|room| room.race_fence == Some(fence))
            .ok_or_else(invariant)?;
        let participant = room
            .frozen_race
            .as_ref()
            .and_then(|frozen| {
                frozen.participants.iter().find(|participant| {
                    !participant.observer && participant.identity.user_no == user_no
                })
            })
            .ok_or_else(invariant)?;
        let finalization = room
            .race_progress
            .settlement
            .as_ref()
            .map(|settlement| &settlement.finalization);
        let phase = match (room.phase, finalization) {
            (RoomPhase::Loading, _) => RewardLanePhase::Loading,
            (RoomPhase::Running, _) => RewardLanePhase::Running,
            (RoomPhase::Settling, None | Some(SettlementFinalization::AwaitingDeadline)) => {
                RewardLanePhase::AwaitingDeadline
            }
            (RoomPhase::Settling, Some(SettlementFinalization::Persisting { rewards, .. })) => {
                let reward = rewards
                    .iter()
                    .find(|reward| reward.user_no == user_no)
                    .ok_or_else(invariant)?;
                match reward.status {
                    RewardPersistenceStatus::Queued { .. } => RewardLanePhase::Queued,
                    RewardPersistenceStatus::InFlight { .. } => RewardLanePhase::InFlight,
                    RewardPersistenceStatus::Durable(_) => {
                        RewardLanePhase::DurableAwaitingFinalization
                    }
                }
            }
            (RoomPhase::Settling, Some(SettlementFinalization::Ready { .. })) => {
                RewardLanePhase::DurableAwaitingFinalization
            }
            (RoomPhase::Settling, Some(SettlementFinalization::Failed(_))) => {
                RewardLanePhase::Terminal
            }
            (RoomPhase::Lobby, _) => return Err(invariant()),
        };
        Ok(OutstandingRewardLane {
            fence,
            user_no,
            nickname: participant.nickname.clone(),
            phase,
        })
    }

    fn reward_dead_letters(&self) -> Result<Vec<RewardDeadLetter>, WorldError> {
        let mut failed_rooms = self
            .protocol_rooms
            .values()
            .filter_map(|room| {
                let fence = room.race_fence?;
                let settlement = room.race_progress.settlement.as_ref()?;
                let SettlementFinalization::Failed(failed) = &settlement.finalization else {
                    return None;
                };
                Some((fence, room, failed))
            })
            .collect::<Vec<_>>();
        failed_rooms
            .sort_unstable_by_key(|(fence, _, _)| (fence.room_id.0, fence.race_epoch.get()));
        failed_rooms
            .into_iter()
            .map(|(fence, room, failed)| {
                let failed_reward = failed.failed_user_no.and_then(|user_no| {
                    failed
                        .rewards
                        .iter()
                        .find(|reward| reward.user_no == user_no)
                });
                let failed_participant = failed.failed_user_no.and_then(|user_no| {
                    room.frozen_race.as_ref().and_then(|frozen| {
                        frozen.participants.iter().find(|participant| {
                            !participant.observer && participant.identity.user_no == user_no
                        })
                    })
                });
                let failed_nickname = failed_reward
                    .map(|reward| reward.nickname.clone())
                    .or_else(|| failed_participant.map(|participant| participant.nickname.clone()));
                let failed_attempt_id =
                    failed_reward.and_then(|reward| reward.status.in_flight_attempt_id());
                if failed.failed_nickname != failed_nickname {
                    return Err(WorldError::RewardDeadLetterInvariant {
                        room_id: fence.room_id.0,
                        race_epoch: fence.race_epoch.get(),
                    });
                }
                if failed.reason.permits_persistence_retry() && failed_attempt_id.is_none() {
                    return Err(WorldError::RewardDeadLetterInvariant {
                        room_id: fence.room_id.0,
                        race_epoch: fence.race_epoch.get(),
                    });
                }
                Ok(RewardDeadLetter {
                    fence,
                    failed_attempt_id,
                    failed_user_no: failed.failed_user_no,
                    failed_canonical_nickname: failed_reward
                        .map(|reward| reward.canonical_nickname.clone())
                        .or_else(|| failed_nickname.as_deref().map(canonical_nickname_key)),
                    failed_nickname,
                    failed_proposed_reward: failed_reward.map(|reward| reward.proposed_reward),
                    reason: failed.reason,
                    retained_ranking: failed.ranking.clone(),
                    retained_rewards: failed.rewards.clone(),
                    retained_packets: failed.packets.clone(),
                })
            })
            .collect()
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "the actor command owns a stable dead-letter snapshot while it is queued"
    )]
    fn retry_reward_dead_letter(
        &mut self,
        dead_letter: RewardDeadLetter,
        now: Instant,
    ) -> Result<(), WorldError> {
        let room = self
            .protocol_rooms
            .get_mut(&dead_letter.fence.room_id)
            .filter(|room| room.race_fence == Some(dead_letter.fence))
            .ok_or(WorldError::StaleRewardDeadLetter)?;
        let settlement = room
            .race_progress
            .settlement
            .as_mut()
            .ok_or(WorldError::StaleRewardDeadLetter)?;
        let previous = std::mem::replace(
            &mut settlement.finalization,
            SettlementFinalization::AwaitingDeadline,
        );
        let SettlementFinalization::Failed(mut failed) = previous else {
            settlement.finalization = previous;
            return Err(WorldError::StaleRewardDeadLetter);
        };
        if failed.reason != dead_letter.reason
            || failed.ranking != dead_letter.retained_ranking
            || failed.rewards != dead_letter.retained_rewards
            || failed.packets != dead_letter.retained_packets
        {
            settlement.finalization = SettlementFinalization::Failed(failed);
            return Err(WorldError::StaleRewardDeadLetter);
        }
        if !failed.reason.permits_persistence_retry() {
            let reason = failed.reason;
            settlement.finalization = SettlementFinalization::Failed(failed);
            return Err(WorldError::RewardDeadLetterNotRetryable { reason });
        }
        let failed_reward = failed.failed_user_no.and_then(|user_no| {
            failed
                .rewards
                .iter()
                .find(|reward| reward.user_no == user_no)
        });
        if failed.failed_user_no != dead_letter.failed_user_no
            || failed.failed_nickname != dead_letter.failed_nickname
            || failed_reward.and_then(|reward| reward.status.in_flight_attempt_id())
                != dead_letter.failed_attempt_id
            || failed_reward.map(|reward| reward.canonical_nickname.as_str())
                != dead_letter.failed_canonical_nickname.as_deref()
            || failed_reward.map(|reward| reward.proposed_reward)
                != dead_letter.failed_proposed_reward
            || failed.ranking.is_none()
            || failed.packets.is_some()
        {
            settlement.finalization = SettlementFinalization::Failed(failed);
            return Err(WorldError::StaleRewardDeadLetter);
        }
        for reward in &mut failed.rewards {
            if !matches!(reward.status, RewardPersistenceStatus::Durable(_)) {
                reward.status = RewardPersistenceStatus::Queued {
                    due_at: now,
                    failure_count: 0,
                };
            }
        }
        let Some(ranking) = failed.ranking else {
            settlement.finalization = SettlementFinalization::Failed(failed);
            return Err(WorldError::StaleRewardDeadLetter);
        };
        settlement.finalization = SettlementFinalization::Persisting {
            ranking,
            rewards: failed.rewards,
        };
        self.debug_assert_invariants();
        Ok(())
    }

    fn reconcile_loading_handshake(
        &mut self,
        room_id: RoomId,
        active_stamps: &HashSet<FrozenParticipantStamp>,
        now: Instant,
    ) {
        let room = self
            .protocol_rooms
            .get_mut(&room_id)
            .expect("the loading room list contains existing rooms");
        let LoadingHandshake::Awaiting {
            expected,
            ready,
            deadline,
        } = &mut room.loading_handshake
        else {
            return;
        };
        expected.retain(|participant| active_stamps.contains(participant));
        ready.retain(|participant| expected.contains(participant));
        if (ready.len() == expected.len() || now >= *deadline)
            && let Some(at) = now.checked_add(RACE_START_DELAY)
        {
            room.loading_handshake = LoadingHandshake::StartScheduled { at };
        }
    }

    fn try_start_scheduled_room(&mut self, room_id: RoomId, now: Instant, clock: &ServerClock) {
        if !self.protocol_rooms.get(&room_id).is_some_and(|room| {
            matches!(
                room.loading_handshake,
                LoadingHandshake::StartScheduled { at } if now >= at
            )
        }) {
            return;
        }
        let start_tick = race_start_tick(clock.tick());
        let batch = OutboundBatch::ordered(vec![
            serialize_ai_master_notice(),
            serialize_game_control(ServerGameControl::RaceStart, start_tick),
        ]);
        let deliveries = {
            let room = self
                .protocol_rooms
                .get(&room_id)
                .expect("the loading room list contains existing rooms");
            let frozen = room
                .frozen_race
                .as_ref()
                .expect("a Loading room has a frozen roster");
            frozen
                .participants
                .iter()
                .filter_map(|participant| {
                    self.exact_identity_in_protocol_room(room_id, &participant.identity)
                        .map(|identity| (identity.owner, batch.duplicate()))
                })
                .collect()
        };
        let reserved = match self.reserve_outbound(deliveries) {
            Ok(reserved) => reserved,
            Err(LobbyError::OutboundUnavailable { session }) => {
                tracing::trace!(
                    room_id = room_id.0,
                    session = session.get(),
                    "race start fan-out is blocked; retaining the scheduled transition"
                );
                return;
            }
            Err(error) => {
                debug_assert!(false, "race start reservation returned {error}");
                return;
            }
        };

        let room = self
            .protocol_rooms
            .get_mut(&room_id)
            .expect("the loading room list contains existing rooms");
        room.phase = RoomPhase::Running;
        room.loading_handshake = LoadingHandshake::Dormant;
        Self::publish_reserved(reserved);
    }

    fn try_flush_pending_race_fanouts(&mut self, room_id: RoomId) -> bool {
        let deliveries = {
            let room = self
                .protocol_rooms
                .get(&room_id)
                .expect("the active race room list contains existing rooms");
            if room.race_progress.pending_fanouts.is_empty() {
                return true;
            }
            self.active_frozen_recipient_sessions(room)
                .into_iter()
                .filter_map(|(participant, session)| {
                    let packets = room
                        .race_progress
                        .pending_fanouts
                        .iter()
                        .flat_map(|fanout| {
                            let race_time = fanout
                                .race_time
                                .as_ref()
                                .map(|race_time| race_time.packet.clone());
                            let begin_settlement =
                                fanout.begin_settlement.as_ref().and_then(|begin| {
                                    (begin.excluded_user != Some(participant.identity.user_no))
                                        .then(|| begin.packet.clone())
                                });
                            race_time.into_iter().chain(begin_settlement)
                        })
                        .collect::<Vec<_>>();
                    (!packets.is_empty()).then(|| (session, OutboundBatch::ordered(packets)))
                })
                .collect()
        };
        let reserved = match self.reserve_race_outbound(deliveries) {
            Ok(reserved) => reserved,
            Err(RaceError::OutboundUnavailable { session }) => {
                tracing::trace!(
                    room_id = room_id.0,
                    session = session.get(),
                    "race event fan-out is blocked; retaining it for heartbeat retry"
                );
                return false;
            }
            Err(error) => {
                tracing::error!(
                    room_id = room_id.0,
                    %error,
                    "race event fan-out reservation failed"
                );
                return false;
            }
        };

        self.protocol_rooms
            .get_mut(&room_id)
            .expect("the active race room list contains existing rooms")
            .race_progress
            .pending_fanouts
            .clear();
        Self::publish_reserved(reserved);
        true
    }

    fn begin_reward_persistence(
        &mut self,
        room_id: RoomId,
        now: Instant,
        reward_rolls: &mut impl RewardRollSource,
    ) {
        let should_begin = self.protocol_rooms.get(&room_id).is_some_and(|room| {
            room.race_progress
                .settlement
                .as_ref()
                .is_some_and(|settlement| {
                    matches!(
                        settlement.finalization,
                        SettlementFinalization::AwaitingDeadline
                    )
                })
        });
        if !should_begin {
            return;
        }

        let planned = self
            .protocol_rooms
            .get(&room_id)
            .ok_or(RaceError::NotInRoom)
            .and_then(|room| Self::plan_reward_persistence(room, now, reward_rolls));
        let failed_participant = match &planned {
            Err(RaceError::RewardSampling { player_id, .. }) => self
                .protocol_rooms
                .get(&room_id)
                .and_then(|room| room.frozen_race.as_ref())
                .and_then(|frozen| {
                    frozen.participants.iter().find(|participant| {
                        !participant.observer && participant.player_id == *player_id
                    })
                })
                .map(|participant| (participant.identity.user_no, participant.nickname.clone())),
            _ => None,
        };
        let Some(settlement) = self
            .protocol_rooms
            .get_mut(&room_id)
            .and_then(|room| room.race_progress.settlement.as_mut())
        else {
            return;
        };
        match planned {
            Ok((ranking, rewards)) => {
                settlement.finalization = SettlementFinalization::Persisting { ranking, rewards };
            }
            Err(error) => {
                let failure = match error {
                    RaceError::RewardSampling { .. } => RewardTerminalReason::RewardSampling,
                    _ => RewardTerminalReason::InvalidRanking,
                };
                let (failed_user_no, failed_nickname) = failed_participant
                    .map_or((None, None), |(user_no, nickname)| {
                        (Some(user_no), Some(nickname))
                    });
                settlement.finalization.retain_as_dead_letter(
                    failure,
                    failed_user_no,
                    failed_nickname,
                );
                tracing::error!(
                    room_id = room_id.0,
                    %error,
                    "race rewards could not be frozen; terminally retaining Settling"
                );
            }
        }
    }

    fn plan_reward_persistence(
        room: &ProtocolRoomState,
        now: Instant,
        reward_rolls: &mut impl RewardRollSource,
    ) -> Result<(SettlementRanking, Vec<RewardPersistenceEntry>), RaceError> {
        let ranking = Self::settlement_ranking(room)?;
        let frozen = room
            .frozen_race
            .as_ref()
            .ok_or(RaceError::InvalidSettlementRoster { racers: 0 })?;
        let human_count = frozen
            .participants
            .iter()
            .filter(|participant| !participant.observer)
            .count();
        if !(1..=ROOM_SLOT_COUNT).contains(&human_count) {
            return Err(RaceError::InvalidSettlementRoster {
                racers: human_count,
            });
        }
        let mut rewards = Vec::with_capacity(human_count);
        for participant in frozen
            .participants
            .iter()
            .filter(|participant| !participant.observer)
        {
            let ranked = ranking.by_player_id.get(&participant.player_id).ok_or(
                RaceError::InvalidSettlementRoster {
                    racers: human_count,
                },
            )?;
            let rp_roll = reward_rolls.draw_rp();
            let lucci_roll = reward_rolls.draw_lucci();
            let ranking =
                usize::try_from(ranked.rank).map_err(|_| RaceError::InvalidSettlementRoster {
                    racers: human_count,
                })?;
            let proposed_reward =
                time_reward_from_rolls(ranking, rp_roll, lucci_roll).map_err(|reason| {
                    RaceError::RewardSampling {
                        player_id: participant.player_id,
                        reason,
                    }
                })?;
            rewards.push(RewardPersistenceEntry {
                user_no: participant.identity.user_no,
                nickname: participant.nickname.clone(),
                canonical_nickname: canonical_nickname_key(&participant.nickname),
                player_id: participant.player_id,
                proposed_reward,
                status: RewardPersistenceStatus::Queued {
                    due_at: now,
                    failure_count: 0,
                },
            });
        }
        Ok((ranking, rewards))
    }

    /// Takes a bounded batch of due reward work and marks every returned item
    /// in flight before it leaves the actor.
    pub(crate) fn take_due_reward_tasks(
        &mut self,
        now: Instant,
        maximum: usize,
    ) -> Result<Vec<RewardSettlementTask>, WorldError> {
        self.expire_reward_attempt_leases(now)?;
        let maximum = maximum.min(MAX_DUE_REWARD_TASK_BATCH);
        if maximum == 0 {
            return Ok(Vec::new());
        }
        let mut candidates = self.due_reward_candidates(now, maximum);
        self.limit_candidates_to_attempt_id_capacity(&mut candidates)?;

        let mut tasks = Vec::with_capacity(candidates.len());
        for (room_id, user_no) in candidates {
            if let Some(task) = self.issue_reward_task(room_id, user_no, now)? {
                tasks.push(task);
            }
        }
        Ok(tasks)
    }

    fn due_reward_candidates(&self, now: Instant, maximum: usize) -> Vec<(RoomId, UserNo)> {
        let mut candidates = self
            .protocol_rooms
            .iter()
            .flat_map(|(room_id, room)| {
                room.race_progress
                    .settlement
                    .as_ref()
                    .and_then(|settlement| match &settlement.finalization {
                        SettlementFinalization::Persisting { rewards, .. } => Some(
                            rewards
                                .iter()
                                .filter_map(|reward| match reward.status {
                                    RewardPersistenceStatus::Queued { due_at, .. }
                                        if due_at <= now =>
                                    {
                                        Some((*room_id, reward.user_no))
                                    }
                                    _ => None,
                                })
                                .collect::<Vec<_>>(),
                        ),
                        _ => None,
                    })
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>();
        candidates.sort_unstable_by_key(|(room_id, user_no)| (room_id.0, user_no.get()));
        candidates.truncate(maximum);
        candidates
    }

    fn limit_candidates_to_attempt_id_capacity(
        &mut self,
        candidates: &mut Vec<(RoomId, UserNo)>,
    ) -> Result<(), WorldError> {
        if let Some((room_id, user_no)) = candidates.first().copied() {
            let Some(next_attempt_id) = self.next_reward_attempt_id else {
                let fence = self
                    .protocol_rooms
                    .get(&room_id)
                    .and_then(|room| room.race_fence);
                if let Some(settlement) = self
                    .protocol_rooms
                    .get_mut(&room_id)
                    .and_then(|room| room.race_progress.settlement.as_mut())
                {
                    let nickname = match &settlement.finalization {
                        SettlementFinalization::Persisting { rewards, .. } => rewards
                            .iter()
                            .find(|reward| reward.user_no == user_no)
                            .map(|reward| reward.nickname.clone()),
                        _ => None,
                    };
                    settlement.finalization.retain_as_dead_letter(
                        RewardTerminalReason::RewardAttemptIdExhausted,
                        Some(user_no),
                        nickname,
                    );
                }
                let fence = fence.ok_or(WorldError::RewardSchedulerInvariant {
                    room_id: room_id.0,
                    user_no: user_no.get(),
                })?;
                return Err(WorldError::RewardAttemptIdExhausted {
                    room_id: fence.room_id.0,
                    race_epoch: fence.race_epoch.get(),
                    user_no: user_no.get(),
                });
            };
            if next_attempt_id.get() == u64::MAX {
                candidates.truncate(1);
            }
        }
        Ok(())
    }

    fn issue_reward_task(
        &mut self,
        room_id: RoomId,
        user_no: UserNo,
        now: Instant,
    ) -> Result<Option<RewardSettlementTask>, WorldError> {
        let Some(attempt_id) = self.allocate_reward_attempt_id() else {
            return Err(WorldError::RewardSchedulerInvariant {
                room_id: room_id.0,
                user_no: user_no.get(),
            });
        };
        let Some(lease_deadline) = now.checked_add(REWARD_ATTEMPT_LEASE) else {
            let fence = self
                .protocol_rooms
                .get(&room_id)
                .and_then(|room| room.race_fence);
            if let Some(settlement) = self
                .protocol_rooms
                .get_mut(&room_id)
                .and_then(|room| room.race_progress.settlement.as_mut())
            {
                let nickname = match &settlement.finalization {
                    SettlementFinalization::Persisting { rewards, .. } => rewards
                        .iter()
                        .find(|reward| reward.user_no == user_no)
                        .map(|reward| reward.nickname.clone()),
                    _ => None,
                };
                settlement.finalization.retain_as_dead_letter(
                    RewardTerminalReason::RewardAttemptLeaseDeadlineOverflow,
                    Some(user_no),
                    nickname,
                );
            }
            let fence = fence.ok_or(WorldError::RewardSchedulerInvariant {
                room_id: room_id.0,
                user_no: user_no.get(),
            })?;
            return Err(WorldError::RewardAttemptLeaseDeadlineOverflow {
                room_id: fence.room_id.0,
                race_epoch: fence.race_epoch.get(),
                user_no: user_no.get(),
            });
        };
        let Some(room) = self.protocol_rooms.get_mut(&room_id) else {
            return Ok(None);
        };
        let Some(fence) = room.race_fence else {
            return Ok(None);
        };
        let Some(settlement) = room.race_progress.settlement.as_mut() else {
            return Ok(None);
        };
        let SettlementFinalization::Persisting { rewards, .. } = &mut settlement.finalization
        else {
            return Ok(None);
        };
        let Some(reward) = rewards.iter_mut().find(|reward| reward.user_no == user_no) else {
            return Ok(None);
        };
        let RewardPersistenceStatus::Queued {
            due_at,
            failure_count,
        } = reward.status
        else {
            return Ok(None);
        };
        if due_at > now {
            return Ok(None);
        }
        reward.status = RewardPersistenceStatus::InFlight {
            attempt_id,
            failure_count,
            lease_deadline,
        };
        Ok(Some(RewardSettlementTask {
            fence,
            attempt_id,
            user_no,
            nickname: reward.nickname.clone(),
            canonical_nickname: reward.canonical_nickname.clone(),
            proposed_reward: reward.proposed_reward,
        }))
    }

    fn allocate_reward_attempt_id(&mut self) -> Option<RewardAttemptId> {
        let raw = self.next_reward_attempt_id?;
        self.next_reward_attempt_id = raw.get().checked_add(1).and_then(NonZeroU64::new);
        Some(RewardAttemptId(raw))
    }

    fn expire_reward_attempt_leases(&mut self, now: Instant) -> Result<(), WorldError> {
        let mut expired = self
            .protocol_rooms
            .iter()
            .flat_map(|(_, room)| {
                let fence = room.race_fence;
                room.race_progress
                    .settlement
                    .as_ref()
                    .and_then(|settlement| match &settlement.finalization {
                        SettlementFinalization::Persisting { rewards, .. } => Some(
                            rewards
                                .iter()
                                .filter_map(|reward| match reward.status {
                                    RewardPersistenceStatus::InFlight {
                                        attempt_id,
                                        lease_deadline,
                                        ..
                                    } if lease_deadline <= now => {
                                        fence.map(|fence| RewardSettlementTask {
                                            fence,
                                            attempt_id,
                                            user_no: reward.user_no,
                                            nickname: reward.nickname.clone(),
                                            canonical_nickname: reward.canonical_nickname.clone(),
                                            proposed_reward: reward.proposed_reward,
                                        })
                                    }
                                    _ => None,
                                })
                                .collect::<Vec<_>>(),
                        ),
                        _ => None,
                    })
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>();
        expired.sort_unstable_by_key(|task| {
            (
                task.fence.room_id.0,
                task.fence.race_epoch.get(),
                task.user_no.get(),
            )
        });
        for task in expired {
            self.expire_reward_attempt(&task, now)?;
        }
        Ok(())
    }

    fn current_reward_attempt(&self, task: &RewardSettlementTask) -> Option<(u8, Instant)> {
        let room = self.protocol_rooms.get(&task.fence.room_id)?;
        if room.race_fence != Some(task.fence) {
            return None;
        }
        let settlement = room.race_progress.settlement.as_ref()?;
        let SettlementFinalization::Persisting { rewards, .. } = &settlement.finalization else {
            return None;
        };
        let reward = rewards
            .iter()
            .find(|reward| reward.user_no == task.user_no)?;
        if reward.nickname != task.nickname
            || reward.canonical_nickname != task.canonical_nickname
            || reward.proposed_reward != task.proposed_reward
        {
            return None;
        }
        let RewardPersistenceStatus::InFlight {
            attempt_id,
            failure_count,
            lease_deadline,
        } = reward.status
        else {
            return None;
        };
        (attempt_id == task.attempt_id).then_some((failure_count, lease_deadline))
    }

    fn expire_reward_attempt(
        &mut self,
        task: &RewardSettlementTask,
        now: Instant,
    ) -> Result<RewardLeaseExpiry, WorldError> {
        let Some((failure_count, lease_deadline)) = self.current_reward_attempt(task) else {
            return Ok(RewardLeaseExpiry::NotCurrent);
        };
        if lease_deadline > now {
            return Ok(RewardLeaseExpiry::Active);
        }
        let failure_count = failure_count.saturating_add(1);
        if failure_count >= MAX_REWARD_PERSISTENCE_FAILURES {
            self.fail_reward_settlement(task, RewardTerminalReason::RewardPersistence)?;
            return Err(WorldError::RewardAttemptLeaseFailuresExhausted {
                room_id: task.fence.room_id.0,
                race_epoch: task.fence.race_epoch.get(),
                user_no: task.user_no.get(),
            });
        }
        let Some(due_at) = now.checked_add(reward_retry_delay(failure_count)) else {
            self.fail_reward_settlement(task, RewardTerminalReason::RewardRetryDeadlineOverflow)?;
            return Err(WorldError::RewardRetryDeadlineOverflow {
                room_id: task.fence.room_id.0,
                race_epoch: task.fence.race_epoch.get(),
                user_no: task.user_no.get(),
            });
        };
        let Some(room) = self.protocol_rooms.get_mut(&task.fence.room_id) else {
            return Ok(RewardLeaseExpiry::NotCurrent);
        };
        let Some(settlement) = room.race_progress.settlement.as_mut() else {
            return Ok(RewardLeaseExpiry::NotCurrent);
        };
        let SettlementFinalization::Persisting { rewards, .. } = &mut settlement.finalization
        else {
            return Ok(RewardLeaseExpiry::NotCurrent);
        };
        let Some(reward) = rewards
            .iter_mut()
            .find(|reward| reward.user_no == task.user_no)
        else {
            return Ok(RewardLeaseExpiry::NotCurrent);
        };
        if !matches!(
            reward.status,
            RewardPersistenceStatus::InFlight {
                attempt_id,
                lease_deadline: current_deadline,
                ..
            } if attempt_id == task.attempt_id && current_deadline <= now
        ) {
            return Ok(RewardLeaseExpiry::NotCurrent);
        }
        reward.status = RewardPersistenceStatus::Queued {
            due_at,
            failure_count,
        };
        Ok(RewardLeaseExpiry::RetryScheduled)
    }

    fn fail_reward_settlement(
        &mut self,
        task: &RewardSettlementTask,
        reason: RewardTerminalReason,
    ) -> Result<(), WorldError> {
        let Some(settlement) = self
            .protocol_rooms
            .get_mut(&task.fence.room_id)
            .filter(|room| room.race_fence == Some(task.fence))
            .and_then(|room| room.race_progress.settlement.as_mut())
        else {
            return Err(WorldError::RewardSchedulerInvariant {
                room_id: task.fence.room_id.0,
                user_no: task.user_no.get(),
            });
        };
        settlement.finalization.retain_as_dead_letter(
            reason,
            Some(task.user_no),
            Some(task.nickname.clone()),
        );
        Ok(())
    }

    /// Applies one persistence completion only when all four fence dimensions
    /// (room, epoch, user and attempt) still name the exact in-flight task.
    #[allow(
        clippy::too_many_lines,
        reason = "all completion variants share one exact stamp and lease validation prelude"
    )]
    pub(crate) fn complete_reward_task(
        &mut self,
        completion: RewardPersistenceCompletion,
        now: Instant,
    ) -> Result<RewardCompletionDisposition, WorldOperationError> {
        let task = completion.task().clone();
        match self.expire_reward_attempt(&task, now) {
            Ok(RewardLeaseExpiry::Active) => {}
            Ok(RewardLeaseExpiry::NotCurrent | RewardLeaseExpiry::RetryScheduled) => {
                return Ok(RewardCompletionDisposition::IgnoredStale);
            }
            Err(
                WorldError::RewardAttemptLeaseFailuresExhausted { .. }
                | WorldError::RewardAttemptLeaseDeadlineOverflow { .. }
                | WorldError::RewardRetryDeadlineOverflow { .. },
            ) => return Ok(RewardCompletionDisposition::TerminalFailure),
            Err(error) => return Err(error.into()),
        }
        let Some((failure_count, _)) = self.current_reward_attempt(&task) else {
            return Ok(RewardCompletionDisposition::IgnoredStale);
        };

        match completion {
            RewardPersistenceCompletion::RetryableFailure(_) => {
                let failure_count = failure_count.saturating_add(1);
                if failure_count >= MAX_REWARD_PERSISTENCE_FAILURES {
                    self.fail_reward_settlement(&task, RewardTerminalReason::RewardPersistence)?;
                    return Ok(RewardCompletionDisposition::TerminalFailure);
                }
                let Some(due_at) = now.checked_add(reward_retry_delay(failure_count)) else {
                    self.fail_reward_settlement(
                        &task,
                        RewardTerminalReason::RewardRetryDeadlineOverflow,
                    )?;
                    return Ok(RewardCompletionDisposition::TerminalFailure);
                };
                let Some(room) = self.protocol_rooms.get_mut(&task.fence.room_id) else {
                    return Ok(RewardCompletionDisposition::IgnoredStale);
                };
                let Some(settlement) = room.race_progress.settlement.as_mut() else {
                    return Ok(RewardCompletionDisposition::IgnoredStale);
                };
                let SettlementFinalization::Persisting { rewards, .. } =
                    &mut settlement.finalization
                else {
                    return Ok(RewardCompletionDisposition::IgnoredStale);
                };
                let Some(reward) = rewards
                    .iter_mut()
                    .find(|reward| reward.user_no == task.user_no)
                else {
                    return Ok(RewardCompletionDisposition::IgnoredStale);
                };
                reward.status = RewardPersistenceStatus::Queued {
                    due_at,
                    failure_count,
                };
                Ok(RewardCompletionDisposition::RetryScheduled { failure_count })
            }
            RewardPersistenceCompletion::FatalFailure(_) => {
                self.fail_reward_settlement(&task, RewardTerminalReason::RewardPersistence)?;
                Ok(RewardCompletionDisposition::TerminalFailure)
            }
            RewardPersistenceCompletion::Durable(receipt) => {
                let key = receipt.key();
                let applied = receipt.applied();
                let receipt_matches = key.run_generation().is_some()
                    && key.store_id().is_some()
                    && key.room_id() == task.fence.room_id.0
                    && key.race_epoch() == task.fence.race_epoch
                    && key.user_no() == task.user_no.get()
                    && key.canonical_nickname() == Some(task.canonical_nickname.as_str())
                    && applied.earned_rp == task.proposed_reward.earned_rp()
                    && applied.earned_lucci == task.proposed_reward.earned_lucci();
                if !receipt_matches {
                    self.fail_reward_settlement(
                        &task,
                        RewardTerminalReason::RewardReceiptMismatch,
                    )?;
                    return Ok(RewardCompletionDisposition::TerminalFailure);
                }
                let participant_exists = self
                    .protocol_rooms
                    .get(&task.fence.room_id)
                    .and_then(|room| room.frozen_race.as_ref())
                    .is_some_and(|frozen| {
                        frozen.participants.iter().any(|participant| {
                            !participant.observer
                                && participant.identity.user_no == task.user_no
                                && participant.nickname == task.nickname
                        })
                    });
                if !participant_exists {
                    self.fail_reward_settlement(
                        &task,
                        RewardTerminalReason::RewardParticipantMissing,
                    )?;
                    return Ok(RewardCompletionDisposition::TerminalFailure);
                }
                let (live_rp, myroom_refresh) = match receipt.profile() {
                    Some(profile) => {
                        if !profile
                            .subject()
                            .matches_nickname(&task.nickname)
                            .map_err(IdentityError::from)?
                        {
                            return Err(WorldError::MyRoomProfileSubjectMismatch {
                                expected: task.nickname.clone(),
                                actual: profile.subject().nickname().to_owned(),
                            }
                            .into());
                        }
                        let identity = self
                            .myroom
                            .canonical_identity_if_tracked(task.user_no)
                            .map_err(|source| {
                                myroom_hub_error("durable reward profile refresh", source)
                            })?;
                        let refresh = match identity {
                            Some(identity)
                                if canonical_nickname_key(&identity.nickname)
                                    == task.canonical_nickname =>
                            {
                                match self
                                    .myroom
                                    .refresh_profile_if_tracked(&identity, profile.presentation())
                                {
                                    Ok(refresh) => refresh,
                                    Err(MyRoomHubError::Wire(source)) => {
                                        tracing::warn!(
                                            nickname = %task.nickname,
                                            %source,
                                            "skipped invalid MyRoom presentation after durable reward"
                                        );
                                        None
                                    }
                                    Err(source) => {
                                        return Err(myroom_hub_error(
                                            "durable reward profile refresh",
                                            source,
                                        )
                                        .into());
                                    }
                                }
                            }
                            Some(_) => {
                                return Err(WorldError::RewardSchedulerInvariant {
                                    room_id: task.fence.room_id.0,
                                    user_no: task.user_no.get(),
                                }
                                .into());
                            }
                            None => None,
                        };
                        (profile.presentation().rp(), refresh)
                    }
                    None => (applied.current_rp, None),
                };
                let Some(room) = self.protocol_rooms.get_mut(&task.fence.room_id) else {
                    return Ok(RewardCompletionDisposition::IgnoredStale);
                };
                let Some(settlement) = room.race_progress.settlement.as_mut() else {
                    return Ok(RewardCompletionDisposition::IgnoredStale);
                };
                let SettlementFinalization::Persisting { rewards, .. } =
                    &mut settlement.finalization
                else {
                    return Ok(RewardCompletionDisposition::IgnoredStale);
                };
                let Some(reward) = rewards
                    .iter_mut()
                    .find(|reward| reward.user_no == task.user_no)
                else {
                    return Ok(RewardCompletionDisposition::IgnoredStale);
                };
                reward.status = RewardPersistenceStatus::Durable(applied);
                let result = room
                    .frozen_race
                    .as_mut()
                    .and_then(|frozen| {
                        frozen.participants.iter_mut().find(|participant| {
                            !participant.observer
                                && participant.identity.user_no == task.user_no
                                && participant.nickname == task.nickname
                        })
                    })
                    .and_then(|participant| participant.result.as_mut());
                let Some(result) = result else {
                    self.fail_reward_settlement(
                        &task,
                        RewardTerminalReason::RewardParticipantMissing,
                    )?;
                    return Ok(RewardCompletionDisposition::TerminalFailure);
                };
                result.economy = FrozenResultEconomy::Applied(applied);

                self.update_live_room_rp(task.user_no, live_rp);
                if let Some(transition) = myroom_refresh {
                    self.commit_silent_myroom_transition(transition)?;
                }
                match self.prepare_ready_settlement(task.fence.room_id) {
                    Ok(()) => Ok(RewardCompletionDisposition::Applied),
                    Err(RewardTerminalReason::ResultSerialization) => {
                        Ok(RewardCompletionDisposition::TerminalFailure)
                    }
                    Err(_) => Err(WorldError::RewardSchedulerInvariant {
                        room_id: task.fence.room_id.0,
                        user_no: task.user_no.get(),
                    }
                    .into()),
                }
            }
        }
    }

    fn update_live_room_rp(&mut self, user_no: UserNo, current_rp: u32) {
        let Some(room_id) = self.protocol_room_by_user.get(&user_no).copied() else {
            return;
        };
        if let Some(member) = self.protocol_rooms.get_mut(&room_id).and_then(|room| {
            room.members_by_id
                .iter_mut()
                .flatten()
                .find(|member| member.user_no == user_no)
        }) {
            member.player.rp = current_rp;
        }
    }

    fn prepare_ready_settlement(&mut self, room_id: RoomId) -> Result<(), RewardTerminalReason> {
        let serialized = self.protocol_rooms.get(&room_id).and_then(|room| {
            let settlement = room.race_progress.settlement.as_ref()?;
            let SettlementFinalization::Persisting { ranking, rewards } = &settlement.finalization
            else {
                return None;
            };
            rewards
                .iter()
                .all(|reward| matches!(reward.status, RewardPersistenceStatus::Durable(_)))
                .then(|| Self::settlement_packets(room, ranking))
        });
        let Some(serialized) = serialized else {
            return Ok(());
        };
        let Some(settlement) = self
            .protocol_rooms
            .get_mut(&room_id)
            .and_then(|room| room.race_progress.settlement.as_mut())
        else {
            return Ok(());
        };
        match serialized {
            Ok(packets) => {
                let previous = std::mem::replace(
                    &mut settlement.finalization,
                    SettlementFinalization::AwaitingDeadline,
                );
                let SettlementFinalization::Persisting { ranking, rewards } = previous else {
                    settlement.finalization = previous;
                    return Ok(());
                };
                settlement.finalization = SettlementFinalization::Ready {
                    ranking,
                    rewards,
                    packets,
                };
                Ok(())
            }
            Err(error) => {
                settlement.finalization.retain_as_dead_letter(
                    RewardTerminalReason::ResultSerialization,
                    None,
                    None,
                );
                tracing::error!(
                    room_id = room_id.0,
                    %error,
                    "durable race settlement could not be serialized; terminally retaining Settling"
                );
                Err(RewardTerminalReason::ResultSerialization)
            }
        }
    }

    fn try_finalize_settlement(&mut self, room_id: RoomId) {
        if !self.try_flush_pending_race_fanouts(room_id) {
            return;
        }
        let packets = {
            let Some(room) = self.protocol_rooms.get(&room_id) else {
                tracing::error!(
                    room_id = room_id.0,
                    "settlement room disappeared before final transition"
                );
                return;
            };
            let Some(settlement) = room.race_progress.settlement.as_ref() else {
                tracing::error!(
                    room_id = room_id.0,
                    "settlement timing disappeared before final transition"
                );
                return;
            };
            match &settlement.finalization {
                SettlementFinalization::Ready { packets, .. } => packets.clone(),
                SettlementFinalization::AwaitingDeadline
                | SettlementFinalization::Persisting { .. }
                | SettlementFinalization::Failed(_) => {
                    return;
                }
            }
        };
        let deliveries = {
            let Some(room) = self.protocol_rooms.get(&room_id) else {
                return;
            };
            self.active_frozen_recipient_sessions(room)
                .into_iter()
                .map(|(_, session)| (session, OutboundBatch::ordered(packets.clone())))
                .collect()
        };
        let reserved = match self.reserve_race_outbound(deliveries) {
            Ok(reserved) => reserved,
            Err(RaceError::OutboundUnavailable { session }) => {
                tracing::trace!(
                    room_id = room_id.0,
                    session = session.get(),
                    "settlement fan-out is blocked; retaining Settling for heartbeat retry"
                );
                return;
            }
            Err(error) => {
                if let Some(settlement) = self
                    .protocol_rooms
                    .get_mut(&room_id)
                    .and_then(|room| room.race_progress.settlement.as_mut())
                {
                    settlement.finalization.retain_as_dead_letter(
                        RewardTerminalReason::OutboundReservation,
                        None,
                        None,
                    );
                }
                tracing::error!(
                    room_id = room_id.0,
                    %error,
                    "settlement reservation failed; terminally retaining Settling"
                );
                return;
            }
        };

        let (remove_room, fence) = {
            let Some(room) = self.protocol_rooms.get(&room_id) else {
                return;
            };
            (room.is_empty(), room.race_fence)
        };
        if let Some(fence) = fence {
            self.release_reward_lanes(fence);
        }
        if remove_room {
            self.protocol_rooms.remove(&room_id);
            self.free_protocol_room_ids
                .insert(u16::try_from(room_id.0).expect("protocol room ID fits in u16"));
        } else {
            let Some(room) = self.protocol_rooms.get_mut(&room_id) else {
                return;
            };
            room.phase = RoomPhase::Lobby;
            room.race_fence = None;
            room.frozen_race = None;
            room.race_progress = RaceProgress::default();
            room.loading_handshake = LoadingHandshake::Dormant;
            for member in room.members_by_id.iter_mut().flatten() {
                member.player.player_type = PlayerSlotState::NotReady as i32;
            }
        }
        Self::publish_reserved(reserved);
    }

    fn settlement_packets(
        room: &ProtocolRoomState,
        ranking: &SettlementRanking,
    ) -> Result<Vec<Vec<u8>>, RaceError> {
        let settlement =
            room.race_progress
                .settlement
                .as_ref()
                .ok_or(RaceError::SettlementInvariant {
                    detail: "Settling room has no settlement timing",
                })?;
        let result = Self::serialize_settlement_result(room, ranking)?;
        Ok(vec![
            serialize_game_next_stage(room.settings.game_type),
            result,
            serialize_game_control(
                ServerGameControl::FinalStage,
                settlement.end_tick.wrapping_add(FINAL_STAGE_TICK_LEAD),
            ),
        ])
    }

    fn serialize_settlement_result(
        room: &ProtocolRoomState,
        ranking: &SettlementRanking,
    ) -> Result<Vec<u8>, RaceError> {
        let frozen = room
            .frozen_race
            .as_ref()
            .ok_or(RaceError::SettlementInvariant {
                detail: "Settling room has no frozen race roster",
            })?;
        let team_mode = matches!(room.settings.game_type, 3 | 4);
        let humans = frozen
            .participants
            .iter()
            .filter(|participant| !participant.observer)
            .map(|participant| {
                let ranked = ranking.by_player_id.get(&participant.player_id).ok_or(
                    RaceError::SettlementInvariant {
                        detail: "human result is missing from frozen ranking",
                    },
                )?;
                let snapshot = participant.result.ok_or(RaceError::SettlementInvariant {
                    detail: "human racer has no result admission snapshot",
                })?;
                let economy = snapshot
                    .economy
                    .applied()
                    .ok_or(RaceError::RewardsNotDurable)?;
                Ok(HumanRaceResult {
                    player_id: participant.player_id,
                    finish_time: ranked.finish_time,
                    kart_id: snapshot.kart_id,
                    rank: ranked.rank,
                    current_rp: economy.current_rp,
                    earned_rp: economy.earned_rp,
                    earned_lucci: economy.earned_lucci,
                    current_lucci: economy.current_lucci,
                    team: team_mode.then_some(ranked.team).flatten(),
                    team_points: ranked.team_points,
                    character_id: snapshot.character_id,
                    club_mark_logo: snapshot.club_mark_logo,
                })
            })
            .collect::<Result<Vec<_>, RaceError>>()?;
        let ais = frozen
            .ais
            .iter()
            .map(|participant| {
                let ranked = ranking.by_player_id.get(&participant.player_id).ok_or(
                    RaceError::SettlementInvariant {
                        detail: "AI result is missing from frozen ranking",
                    },
                )?;
                Ok(AiRaceResult {
                    player_id: participant.player_id,
                    finish_time: ranked.finish_time,
                    kart_id: participant.kart_id,
                    rank: ranked.rank,
                    team: team_mode.then_some(ranked.team).flatten(),
                    team_points: ranked.team_points,
                })
            })
            .collect::<Result<Vec<_>, RaceError>>()?;
        Ok(serialize_game_result(&GameResult {
            winning_team: ranking.winning_team,
            humans: &humans,
            ais: &ais,
        })?)
    }

    fn settlement_ranking(room: &ProtocolRoomState) -> Result<SettlementRanking, RaceError> {
        let frozen = room
            .frozen_race
            .as_ref()
            .ok_or(RaceError::SettlementInvariant {
                detail: "settlement ranking has no frozen race roster",
            })?;
        let mut racers = frozen
            .participants
            .iter()
            .filter(|participant| !participant.observer)
            .map(|participant| (participant.player_id, participant.team))
            .chain(
                frozen
                    .ais
                    .iter()
                    .map(|participant| (participant.player_id, participant.team)),
            )
            .map(|(player_id, team)| {
                (
                    room.race_progress
                        .finish_times
                        .get(&player_id)
                        .copied()
                        .unwrap_or(u32::MAX),
                    player_id,
                    team,
                )
            })
            .collect::<Vec<_>>();
        if !(1..=ROOM_SLOT_COUNT).contains(&racers.len()) {
            return Err(RaceError::InvalidSettlementRoster {
                racers: racers.len(),
            });
        }
        racers.sort_unstable_by_key(|&(finish_time, player_id, _)| (finish_time, player_id));
        let team_mode = matches!(room.settings.game_type, 3 | 4);
        let mut team_scores = [0_i32; 2];
        let mut by_player_id = HashMap::with_capacity(racers.len());
        for (rank, (finish_time, player_id, wire_team)) in racers.iter().copied().enumerate() {
            let team = team_mode
                .then(|| race_team_from_wire(wire_team).map(result_team))
                .transpose()?;
            let team_points = if team_mode && finish_time != u32::MAX {
                TEAM_POINTS_BY_RANK.get(rank).copied().ok_or(
                    RaceError::InvalidSettlementRoster {
                        racers: racers.len(),
                    },
                )?
            } else {
                0
            };
            if let Some(team) = team {
                let index = usize::from(team == ResultTeam::Blue);
                team_scores[index] += team_points;
            }
            by_player_id.insert(
                player_id,
                RankedRaceResult {
                    finish_time,
                    rank: i32::try_from(rank).map_err(|_| RaceError::SettlementInvariant {
                        detail: "bounded race rank does not fit in i32",
                    })?,
                    team,
                    team_points,
                },
            );
        }
        let winning_team = if team_mode {
            let first_player_id = racers
                .first()
                .ok_or(RaceError::InvalidSettlementRoster { racers: 0 })?
                .1;
            let first_team = by_player_id
                .get(&first_player_id)
                .and_then(|ranked| ranked.team)
                .ok_or(RaceError::SettlementInvariant {
                    detail: "team-mode winner has no frozen team",
                })?;
            Some(match room.settings.game_type {
                3 if team_scores[0] > team_scores[1] => ResultTeam::Red,
                3 if team_scores[1] > team_scores[0] => ResultTeam::Blue,
                3 | 4 => first_team,
                _ => {
                    return Err(RaceError::SettlementInvariant {
                        detail: "team-mode ranking has a non-team game type",
                    });
                }
            })
        } else {
            None
        };
        Ok(SettlementRanking {
            winning_team,
            by_player_id,
        })
    }

    #[cfg(test)]
    fn claim_identity(
        &mut self,
        session: SessionId,
        nickname: &str,
    ) -> Result<IdentityBinding, WorldError> {
        self.claim_identity_at_udp_epoch(session, nickname, 0)
    }

    fn claim_identity_at_udp_epoch(
        &mut self,
        session: SessionId,
        nickname: &str,
        activated_udp_epoch: u64,
    ) -> Result<IdentityBinding, WorldError> {
        let source_ip = self.session_ip(session)?;
        let maximum = self.identity_capacity.get();
        if self.identities.retained_identity_count() >= maximum {
            return Err(WorldError::IdentityLimitReached { maximum });
        }
        let binding =
            self.identities
                .claim_at(session, source_ip, nickname, activated_udp_epoch)?;
        self.identity_lifecycle
            .push_back(IdentityLifecycleEvent::Announce(binding.clone()));
        Ok(binding)
    }

    #[cfg(test)]
    fn complete_migration(
        &mut self,
        destination: SessionId,
        user_no: UserNo,
        channel_id: u16,
        token: MigrationToken,
        now: Instant,
    ) -> Result<MigrationCompletion, WorldOperationError> {
        let preflight = self.preflight_migration(destination, user_no, channel_id, token, now)?;
        self.complete_preflighted_migration(preflight, None, now)
    }

    fn preflight_migration(
        &mut self,
        destination: SessionId,
        user_no: UserNo,
        channel_id: u16,
        token: MigrationToken,
        now: Instant,
    ) -> Result<MigrationPreflight, WorldError> {
        let destination_ip = self.session_ip(destination)?;
        Ok(self.identities.preflight_migration(
            destination,
            destination_ip,
            user_no,
            channel_id,
            token,
            now,
        )?)
    }

    #[cfg(test)]
    fn complete_preflighted_migration(
        &mut self,
        preflight: MigrationPreflight,
        profile: Option<&MyRoomProfileLease>,
        now: Instant,
    ) -> Result<MigrationCompletion, WorldOperationError> {
        self.complete_preflighted_migration_with_acknowledgement(
            preflight,
            profile,
            MigrationAcknowledgement::Omitted,
            None,
            now,
        )
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the single actor-turn prepare/lock/ACK/commit sequence stays contiguous so its irreversible boundary is auditable"
    )]
    fn complete_preflighted_migration_with_acknowledgement(
        &mut self,
        preflight: MigrationPreflight,
        profile: Option<&MyRoomProfileLease>,
        acknowledgement: MigrationAcknowledgement,
        udp: Option<&UdpService>,
        now: Instant,
    ) -> Result<MigrationCompletion, WorldOperationError> {
        if let Some(profile) = profile {
            let matches = match profile.subject().matches_nickname(preflight.nickname()) {
                Ok(matches) => matches,
                Err(error) => {
                    self.identities.abort_preflighted_migration(&preflight);
                    return Err(IdentityError::from(error).into());
                }
            };
            if !matches {
                self.identities.abort_preflighted_migration(&preflight);
                return Err(WorldError::MyRoomProfileSubjectMismatch {
                    expected: preflight.nickname().to_owned(),
                    actual: profile.subject().nickname().to_owned(),
                }
                .into());
            }
        }
        let destination = preflight.destination_session();
        let current_ip = match self.session_ip(destination) {
            Ok(current_ip) => current_ip,
            Err(error) => {
                self.identities.abort_preflighted_migration(&preflight);
                return Err(error.into());
            }
        };
        if current_ip != preflight.destination_ip() {
            self.identities.abort_preflighted_migration(&preflight);
            return Err(IdentityError::SourceIpMismatch {
                expected: preflight.destination_ip(),
                received: current_ip,
            }
            .into());
        }
        if let Err(error) = self
            .identities
            .validate_preflighted_migration(&preflight, now)
        {
            self.identities.abort_preflighted_migration(&preflight);
            return Err(error.into());
        }
        let candidate = self
            .identities
            .prepare_preflighted_migration(&preflight, now)?;
        let prepared = (|| {
            let completion = candidate.completion();
            let myroom_transition = match profile.map(MyRoomProfileLease::presentation) {
                Some(presentation) => match self.myroom.advance_profiled_identity_if_tracked(
                    &completion.previous_binding,
                    &completion.binding,
                    presentation,
                ) {
                    Ok(transition) => transition,
                    Err(MyRoomHubError::Wire(source)) => {
                        tracing::warn!(
                            nickname = %completion.binding.nickname,
                            %source,
                            "retained the previous MyRoom presentation during identity migration"
                        );
                        self.myroom
                            .advance_migrated_identity_if_tracked(
                                &completion.previous_binding,
                                &completion.binding,
                            )
                            .map_err(|error| {
                                myroom_hub_error("identity migration fallback", error)
                            })?
                    }
                    Err(error) => {
                        return Err(myroom_hub_error("identity migration", error).into());
                    }
                },
                None => self
                    .myroom
                    .advance_migrated_identity_if_tracked(
                        &completion.previous_binding,
                        &completion.binding,
                    )
                    .map_err(|error| myroom_hub_error("identity migration", error))?,
            };
            let (protocol_delta, protocol_deliveries) =
                self.plan_protocol_migration(&completion.binding)?;
            let acknowledgement = match acknowledgement {
                MigrationAcknowledgement::Ordered(packet) => {
                    let destination = preflight.destination_session();
                    let reserved = self
                        .try_reserve_myroom_outbound(vec![(
                            destination,
                            OutboundBatch::single(packet),
                        )])
                        .map_err(|_| WorldError::MigrationAcknowledgementUnavailable {
                            session: destination,
                        })?;
                    Some(reserved)
                }
                #[cfg(test)]
                MigrationAcknowledgement::Omitted => None,
            };
            let protocol_publications = self
                .try_reserve_myroom_outbound(protocol_deliveries)
                .map_err(|error| WorldError::MigrationPublicationUnavailable {
                    session: error.session,
                })?;
            self.identity_lifecycle
                .try_reserve(1)
                .map_err(|_| WorldError::MigrationCommitCapacityUnavailable)?;
            Ok((
                myroom_transition,
                protocol_delta,
                acknowledgement,
                protocol_publications,
            ))
        })();
        let (myroom_transition, protocol_delta, acknowledgement, protocol_publications) =
            match prepared {
                Ok(prepared) => prepared,
                Err(error) => {
                    self.identities.abort_preflighted_migration(&preflight);
                    return Err(error);
                }
            };

        let completion = {
            let World {
                identities,
                myroom,
                protocol_room_by_user,
                protocol_rooms,
                free_protocol_room_ids,
                reward_lanes,
                ..
            } = self;
            let myroom_commit = match myroom_transition {
                Some(transition) => match transition.lock(myroom) {
                    Ok(commit) => Some(commit),
                    Err(error) => {
                        identities.abort_preflighted_migration(&preflight);
                        return Err(MyRoomLifecycleError::from(error).into());
                    }
                },
                None => None,
            };
            let protocol_commit = match protocol_delta {
                Some(delta) => match delta.lock(
                    protocol_room_by_user,
                    protocol_rooms,
                    free_protocol_room_ids,
                    reward_lanes,
                ) {
                    Ok(commit) => Some(commit),
                    Err(error) => {
                        identities.abort_preflighted_migration(&preflight);
                        return Err(error.into());
                    }
                },
                None => None,
            };
            let identity_commit = identities.lock_prepared_migration(preflight, candidate, now)?;

            if let Some(acknowledgement) = acknowledgement {
                // Actual queue insertion is the ownership-publication barrier.
                // Every subsequent step is protected by an exclusive commit
                // capability and has no recoverable rejection path.
                Self::publish_reserved(acknowledgement);
            }
            let activated_udp_epoch = udp.map_or(0, UdpService::advance_arrival_epoch);
            let completion = identity_commit.commit(activated_udp_epoch);
            if let Some(commit) = myroom_commit {
                drop(commit.commit());
            }
            if let Some(commit) = protocol_commit {
                commit.commit();
            }
            completion
        };
        debug_assert_eq!(self.myroom.audit_invariants(), Ok(()));
        self.identity_lifecycle
            .push_back(IdentityLifecycleEvent::Advance {
                previous: completion.previous_identity.clone(),
                next: completion.binding.clone(),
            });
        if let Some(previous_owner) = completion.previous_owner {
            self.cancel_session(previous_owner);
        }
        Self::publish_reserved(protocol_publications);
        Ok(completion)
    }

    fn room_protocol(
        &mut self,
        session: SessionId,
        payload: RoomCommandPayload,
    ) -> Result<(), WorldOperationError> {
        let identity = self.authorize_session_operation(session)?;
        match payload {
            RoomCommandPayload::List(request) => {
                self.protocol_room_list(session, &identity, request)
            }
            RoomCommandPayload::Create {
                request,
                participant,
            } => self.protocol_create_room(session, &identity, request, participant),
            RoomCommandPayload::Join {
                request,
                participant,
            } => self.protocol_join_room(session, &identity, &request, participant),
            RoomCommandPayload::Leave => self.protocol_leave_room(session, &identity),
            RoomCommandPayload::FirstState => self.protocol_first_state(session, &identity),
        }
    }

    fn publish_room_equipment(
        &mut self,
        session: SessionId,
        snapshot: [u8; 65],
    ) -> Result<(), WorldOperationError> {
        let identity = self.authorize_session_operation(session)?;
        let has_membership = self.protocol_room_by_user.contains_key(&identity.user_no);
        let Some(plan) = self.plan_room_equipment(identity.user_no, snapshot)? else {
            debug_assert!(
                !has_membership,
                "protocol membership map and room state diverged"
            );
            return Ok(());
        };
        self.commit_room_equipment(plan)?;
        self.debug_assert_invariants();
        Ok(())
    }

    fn plan_room_equipment(
        &self,
        user_no: UserNo,
        snapshot: [u8; 65],
    ) -> Result<Option<RoomEquipmentPlan>, EquipmentProtocolError> {
        let Some(room_id) = self.protocol_room_by_user.get(&user_no).copied() else {
            return Ok(None);
        };
        let Some(room) = self.protocol_rooms.get(&room_id) else {
            return Ok(None);
        };
        let Some(player_id) = room.equipment_player_id(user_no) else {
            return Ok(None);
        };
        let packet = serialize_room_slot_items(player_id, &snapshot)?;
        let recipients = room
            .user_nos()
            .into_iter()
            .filter(|recipient| *recipient != user_no)
            .collect();
        Ok(Some(RoomEquipmentPlan {
            room_id,
            user_no,
            snapshot,
            packet,
            recipients,
        }))
    }

    fn commit_room_equipment(
        &mut self,
        plan: RoomEquipmentPlan,
    ) -> Result<(), MyRoomLifecycleError> {
        let room = self
            .protocol_rooms
            .get_mut(&plan.room_id)
            .expect("a preplanned protocol equipment room remains actor-owned");
        let updated = room.set_equipment_snapshot(plan.user_no, plan.snapshot);
        debug_assert!(
            updated,
            "a preplanned protocol equipment member remains in its room"
        );
        let deliveries =
            self.deliveries_for_users(plan.recipients, &OutboundBatch::single(plan.packet));
        self.deliver(deliveries, Instant::now())
    }

    fn refresh_myroom_presentation(
        &mut self,
        session: SessionId,
        expected: &IdentityBinding,
        profile: &MyRoomProfileLease,
    ) -> Result<bool, WorldOperationError> {
        let identity = self.authorize_session_operation(session)?;
        if &identity != expected {
            return Err(IdentityError::StaleSession(session).into());
        }
        if !profile
            .subject()
            .matches_nickname(&expected.nickname)
            .map_err(IdentityError::from)?
        {
            return Err(WorldError::MyRoomProfileSubjectMismatch {
                expected: expected.nickname.clone(),
                actual: profile.subject().nickname().to_owned(),
            }
            .into());
        }
        let transition = match self
            .myroom
            .refresh_profile_if_tracked(&identity, profile.presentation())
        {
            Ok(transition) => transition,
            Err(MyRoomHubError::Wire(source)) => {
                tracing::warn!(
                    nickname = %identity.nickname,
                    %source,
                    "skipped invalid MyRoom presentation during profile refresh"
                );
                None
            }
            Err(source) => {
                return Err(myroom_hub_error("profile presentation refresh", source).into());
            }
        };
        let Some(transition) = transition else {
            return Ok(false);
        };
        self.commit_silent_myroom_transition(transition)?;
        self.debug_assert_invariants();
        Ok(true)
    }

    fn lobby_command(
        &mut self,
        session: SessionId,
        payload: LobbyCommandPayload,
    ) -> Result<LobbyCommandOutcome, WorldError> {
        let identity = self.authorize_session_operation(session)?;
        match payload {
            LobbyCommandPayload::SetSlotState(state) => {
                self.set_slot_state(&identity, state).map_err(Into::into)
            }
            LobbyCommandPayload::ChangeTeam(team) => {
                self.change_team(&identity, team).map_err(Into::into)
            }
            LobbyCommandPayload::ChangeMaster(nickname) => {
                self.change_master(&identity, &nickname).map_err(Into::into)
            }
            LobbyCommandPayload::StartRoom(plan) => match self.start_room(&identity, &plan) {
                Ok(outcome) => Ok(outcome),
                Err(error @ LobbyError::OutboundUnavailable { .. }) => Err(error.into()),
                Err(error) => {
                    self.publish_start_rejection(session)?;
                    Err(error.into())
                }
            },
        }
    }

    #[cfg(test)]
    fn race_command(
        &mut self,
        session: SessionId,
        payload: RaceCommandPayload,
        now: Instant,
    ) -> Result<RaceCommandOutcome, WorldError> {
        self.race_command_with_clock(session, payload, now, &ServerClock::new())
    }

    fn race_command_with_clock(
        &mut self,
        session: SessionId,
        payload: RaceCommandPayload,
        now: Instant,
        clock: &ServerClock,
    ) -> Result<RaceCommandOutcome, WorldError> {
        let identity = self.authorize_session_operation(session)?;
        let room_id = self
            .protocol_room_by_user
            .get(&identity.user_no)
            .copied()
            .ok_or(RaceError::NotInRoom)?;
        match payload {
            RaceCommandPayload::GameControl(request) => match request.state {
                0 => self.arm_loading(room_id, &identity, now),
                2 => self.record_human_finish(room_id, &identity, request.value0, now, clock),
                state => Err(RaceError::UnsupportedGameControlState { state }),
            },
            RaceCommandPayload::AiGoalIn(request) => {
                self.record_ai_finish(room_id, &identity, request, now, clock)
            }
            RaceCommandPayload::TeamBoosterGauge(request) => {
                self.update_team_booster(room_id, &identity, request)
            }
        }
        .map_err(Into::into)
    }

    fn arm_loading(
        &mut self,
        room_id: RoomId,
        identity: &IdentityBinding,
        now: Instant,
    ) -> Result<RaceCommandOutcome, RaceError> {
        let room = self
            .protocol_rooms
            .get_mut(&room_id)
            .expect("protocol membership always references an existing room");
        if room.phase != RoomPhase::Loading {
            return Err(RaceError::WrongPhase { actual: room.phase });
        }
        let frozen = room
            .frozen_race
            .as_ref()
            .expect("a Loading room always has a frozen roster");
        if !frozen
            .participants
            .iter()
            .any(|participant| participant.identity == *identity)
        {
            return Err(RaceError::NotFrozenParticipant);
        }

        if matches!(
            room.loading_handshake,
            LoadingHandshake::Awaiting { .. } | LoadingHandshake::StartScheduled { .. }
        ) {
            return Ok(RaceCommandOutcome::IgnoredDuplicate {
                room_id,
                race_epoch: frozen.fence.race_epoch.get(),
            });
        }
        let deadline = now
            .checked_add(LOADING_READY_TIMEOUT)
            .ok_or(RaceError::RaceDeadlineOverflow)?;
        let expected = frozen
            .participants
            .iter()
            .map(|participant| FrozenParticipantStamp::from(&participant.identity))
            .collect::<HashSet<_>>();
        let expected_participants = expected.len();
        room.loading_handshake = LoadingHandshake::Awaiting {
            expected,
            ready: HashSet::new(),
            deadline,
        };
        Ok(RaceCommandOutcome::LoadingAwaiting {
            room_id,
            race_epoch: frozen.fence.race_epoch.get(),
            expected_participants,
        })
    }

    fn record_human_finish(
        &mut self,
        room_id: RoomId,
        identity: &IdentityBinding,
        race_time: u32,
        now: Instant,
        clock: &ServerClock,
    ) -> Result<RaceCommandOutcome, RaceError> {
        let room = self
            .protocol_rooms
            .get(&room_id)
            .expect("protocol membership always references an existing room");
        if !matches!(room.phase, RoomPhase::Running | RoomPhase::Settling) {
            return Err(RaceError::NotRunning { actual: room.phase });
        }
        let participant = room
            .frozen_race
            .as_ref()
            .expect("an active race has a frozen roster")
            .participants
            .iter()
            .find(|participant| participant.identity == *identity)
            .ok_or(RaceError::NotFrozenParticipant)?;
        if participant.observer {
            return Err(RaceError::HumanRacerRequired);
        }
        self.record_finish(
            room_id,
            participant.player_id,
            race_time,
            Some(identity.user_no),
            now,
            clock,
        )
    }

    fn record_ai_finish(
        &mut self,
        room_id: RoomId,
        identity: &IdentityBinding,
        request: AiGoalInRequest,
        now: Instant,
        clock: &ServerClock,
    ) -> Result<RaceCommandOutcome, RaceError> {
        let room = self
            .protocol_rooms
            .get(&room_id)
            .expect("protocol membership always references an existing room");
        if !matches!(room.phase, RoomPhase::Running | RoomPhase::Settling) {
            return Err(RaceError::NotRunning { actual: room.phase });
        }
        let frozen = room
            .frozen_race
            .as_ref()
            .expect("an active race has a frozen roster");
        if !frozen
            .participants
            .iter()
            .any(|participant| participant.identity == *identity)
        {
            return Err(RaceError::NotFrozenParticipant);
        }
        if !frozen
            .ais
            .iter()
            .any(|participant| participant.player_id == request.player_id)
        {
            return Err(RaceError::NoFrozenAiParticipant {
                player_id: request.player_id,
            });
        }
        self.record_finish(
            room_id,
            request.player_id,
            request.race_time,
            None,
            now,
            clock,
        )
    }

    fn record_finish(
        &mut self,
        room_id: RoomId,
        player_id: i32,
        race_time: u32,
        exclude_begin_for: Option<UserNo>,
        now: Instant,
        clock: &ServerClock,
    ) -> Result<RaceCommandOutcome, RaceError> {
        let room = self
            .protocol_rooms
            .get(&room_id)
            .expect("protocol membership always references an existing room");
        if room.phase == RoomPhase::Settling
            && room
                .race_progress
                .settlement
                .as_ref()
                .is_some_and(|settlement| {
                    now >= settlement.deadline
                        || !matches!(
                            settlement.finalization,
                            SettlementFinalization::AwaitingDeadline
                        )
                })
        {
            return Err(RaceError::SettlementClosed);
        }
        if room.race_progress.finish_times.contains_key(&player_id) {
            let race_epoch = room
                .frozen_race
                .as_ref()
                .ok_or(RaceError::NotRunning { actual: room.phase })?
                .fence
                .race_epoch
                .get();
            return Ok(RaceCommandOutcome::IgnoredDuplicate {
                room_id,
                race_epoch,
            });
        }
        let began_settlement = room.phase == RoomPhase::Running;
        let settlement = if began_settlement {
            Some(SettlementState {
                end_tick: clock.tick().wrapping_add(SETTLEMENT_TICK_LEAD),
                deadline: now
                    .checked_add(SETTLEMENT_DELAY)
                    .ok_or(RaceError::SettlementDeadlineOverflow)?,
                finalization: SettlementFinalization::AwaitingDeadline,
            })
        } else {
            None
        };
        let race_time_packet = serialize_race_time(player_id, race_time)?;
        let pending_begin = settlement
            .as_ref()
            .map(|settlement| PendingBeginSettlement {
                end_tick: settlement.end_tick,
                excluded_user: exclude_begin_for,
                packet: serialize_game_control(
                    ServerGameControl::BeginSettlement,
                    settlement.end_tick,
                ),
            });
        if room
            .race_progress
            .pending_fanouts
            .iter()
            .filter(|fanout| fanout.race_time.is_some())
            .count()
            >= ROOM_SLOT_COUNT
        {
            return Err(RaceError::PendingRaceFanoutFull);
        }

        let room = self
            .protocol_rooms
            .get_mut(&room_id)
            .expect("protocol membership always references an existing room");
        room.race_progress.finish_times.insert(player_id, race_time);
        if let Some(settlement) = settlement {
            room.phase = RoomPhase::Settling;
            room.race_progress.settlement = Some(settlement);
        }
        room.race_progress
            .pending_fanouts
            .push_back(PendingRaceFanout {
                race_time: Some(PendingRaceTime {
                    player_id,
                    race_time,
                    packet: race_time_packet,
                }),
                begin_settlement: pending_begin,
            });
        let race_epoch = room
            .frozen_race
            .as_ref()
            .ok_or(RaceError::NotRunning { actual: room.phase })?
            .fence
            .race_epoch
            .get();
        self.try_flush_pending_race_fanouts(room_id);
        Ok(RaceCommandOutcome::FinishRecorded {
            room_id,
            race_epoch,
            player_id,
            began_settlement,
        })
    }

    fn update_team_booster(
        &mut self,
        room_id: RoomId,
        identity: &IdentityBinding,
        request: TeamBoosterGaugeRequest,
    ) -> Result<RaceCommandOutcome, RaceError> {
        let room = self
            .protocol_rooms
            .get(&room_id)
            .expect("protocol membership always references an existing room");
        if room.phase != RoomPhase::Running {
            return Err(RaceError::NotRunning { actual: room.phase });
        }
        if !matches!(room.settings.game_type, 3 | 4) {
            return Err(RaceError::TeamModeRequired);
        }
        let frozen = room
            .frozen_race
            .as_ref()
            .expect("a Running room has a frozen roster");
        let sender = frozen
            .participants
            .iter()
            .find(|participant| participant.identity == *identity)
            .ok_or(RaceError::NotFrozenParticipant)?;
        if sender.observer {
            return Err(RaceError::HumanRacerRequired);
        }
        let sender_team = race_team_from_wire(sender.team)?;
        if sender_team != request.team {
            return Err(RaceError::TeamSpoof {
                claimed: request.team,
                actual: sender.team,
            });
        }
        let team_count = frozen
            .participants
            .iter()
            .filter(|participant| !participant.observer && participant.team == sender.team)
            .count();
        debug_assert_ne!(team_count, 0);
        let contribution = request.contribution * 0.000_125
            / f32::from(u16::try_from(team_count).expect("a room team count fits in u16"));
        let wire_gauge = (room.race_progress.team_gauge(request.team) + contribution).min(1.0);
        let race_epoch = frozen.fence.race_epoch.get();
        let packet = serialize_team_booster_gauge(request.team, wire_gauge)?;
        let deliveries = self
            .active_frozen_recipient_sessions(room)
            .into_iter()
            .filter(|(participant, _)| participant.observer || participant.team == sender.team)
            .map(|(_, session)| (session, OutboundBatch::single(packet.clone())))
            .collect();
        let reserved = self.reserve_race_outbound(deliveries)?;

        let room = self
            .protocol_rooms
            .get_mut(&room_id)
            .expect("protocol membership always references an existing room");
        let reached_full = wire_gauge >= 1.0;
        room.race_progress
            .set_team_gauge(request.team, if reached_full { 0.0 } else { wire_gauge });
        Self::publish_reserved(reserved);
        Ok(RaceCommandOutcome::BoosterGaugeUpdated {
            room_id,
            race_epoch,
            team: request.team,
            reached_full,
        })
    }

    fn active_frozen_recipient_sessions<'a>(
        &self,
        room: &'a ProtocolRoomState,
    ) -> Vec<(&'a FrozenRaceParticipant, SessionId)> {
        room.frozen_race
            .as_ref()
            .expect("a non-Lobby room has a frozen roster")
            .participants
            .iter()
            .filter_map(|participant| {
                self.exact_identity_in_protocol_room(room.id, &participant.identity)
                    .map(|identity| (participant, identity.owner))
            })
            .collect()
    }

    fn exact_identity_in_protocol_room(
        &self,
        room_id: RoomId,
        frozen_identity: &IdentityBinding,
    ) -> Option<IdentityBinding> {
        if self.protocol_room_by_user.get(&frozen_identity.user_no) != Some(&room_id) {
            return None;
        }
        self.identities
            .active_identity_by_user_no(frozen_identity.user_no)
            .filter(|active| active == frozen_identity)
    }

    fn reserve_race_outbound(
        &self,
        deliveries: Vec<OutboundDelivery>,
    ) -> Result<Vec<ReservedOutbound>, RaceError> {
        self.reserve_outbound(deliveries)
            .map_err(|error| match error {
                LobbyError::OutboundUnavailable { session } => {
                    RaceError::OutboundUnavailable { session }
                }
                error => unreachable!("outbound reservation returned {error}"),
            })
    }

    fn protocol_room_id(&self, identity: &IdentityBinding) -> Result<RoomId, LobbyError> {
        self.protocol_room_by_user
            .get(&identity.user_no)
            .copied()
            .ok_or(LobbyError::NotInRoom)
    }

    fn set_slot_state(
        &mut self,
        identity: &IdentityBinding,
        state: PlayerSlotState,
    ) -> Result<LobbyCommandOutcome, LobbyError> {
        let room_id = self.protocol_room_id(identity)?;
        let room = self
            .protocol_rooms
            .get(&room_id)
            .expect("protocol membership always references an existing room");
        Self::require_lobby(room)?;
        if room.observer_id(identity.user_no).is_some() {
            return Err(LobbyError::HumanRacerRequired);
        }
        match state {
            PlayerSlotState::NotReady | PlayerSlotState::Ready => {}
            PlayerSlotState::Observer => return Err(LobbyError::ObserverStateServerOwned),
            PlayerSlotState::Preparing => return Err(LobbyError::PreparingStateServerOwned),
        }
        let member_id = room
            .member_id(identity.user_no)
            .ok_or(LobbyError::HumanRacerRequired)?;
        let player_id =
            i32::try_from(member_id).expect("the fixed room member ID always fits in i32");

        let mut next = room.clone();
        next.members_by_id[member_id]
            .as_mut()
            .expect("the resolved member ID remains occupied")
            .player
            .player_type = state as i32;
        let state_packet = serialize_slot_state(next.slot_states())?;
        let reply_packet =
            serialize_set_slot_state_reply(identity.user_no.get(), true, player_id, state)?;
        let slot_packet = serialize_gr_slot_data(&next.slot_data())?;
        let batch = OutboundBatch::ordered(vec![state_packet, reply_packet, slot_packet]);
        let deliveries = self.same_batch_for_room(&next, &batch)?;
        let reserved = self.reserve_outbound(deliveries)?;

        self.protocol_rooms.insert(room_id, next);
        Self::publish_reserved(reserved);
        self.debug_assert_invariants();
        Ok(LobbyCommandOutcome::SlotStateChanged {
            room_id,
            player_id,
            state,
        })
    }

    fn change_team(
        &mut self,
        identity: &IdentityBinding,
        team: RoomTeam,
    ) -> Result<LobbyCommandOutcome, LobbyError> {
        let room_id = self.protocol_room_id(identity)?;
        let room = self
            .protocol_rooms
            .get(&room_id)
            .expect("protocol membership always references an existing room");
        Self::require_lobby(room)?;
        if !matches!(room.settings.game_type, 3 | 4) {
            return Err(LobbyError::TeamModeRequired);
        }
        let member_id = room
            .member_id(identity.user_no)
            .ok_or(LobbyError::HumanRacerRequired)?;
        let member_id_wire =
            u8::try_from(member_id).expect("the fixed room member ID always fits in u8");
        let player_id =
            i32::try_from(member_id).expect("the fixed room member ID always fits in i32");
        let current_slot = room
            .slot_positions
            .iter()
            .position(|position| *position == Some(member_id_wire))
            .expect("every human racer occupies exactly one physical slot");
        let current_team = room.members_by_id[member_id]
            .as_ref()
            .expect("the resolved member ID remains occupied")
            .player
            .team;

        let mut next = room.clone();
        let target_slot = if current_team == team as u8 {
            current_slot
        } else {
            let mut range = match team {
                RoomTeam::Blue => 0..ROOM_SLOT_COUNT / 2,
                RoomTeam::Red => ROOM_SLOT_COUNT / 2..ROOM_SLOT_COUNT,
            };
            let target_slot = range
                .find(|slot_id| next.slot_positions[*slot_id].is_none())
                .ok_or(LobbyError::TeamFull { team })?;
            next.slot_positions[current_slot] = None;
            next.slot_positions[target_slot] = Some(member_id_wire);
            let member = next.members_by_id[member_id]
                .as_mut()
                .expect("the resolved member ID remains occupied");
            member.player.team = team as u8;
            member.player.player_type = PlayerSlotState::NotReady as i32;
            target_slot
        };

        let team_packet = serialize_change_team_reply(player_id, team, next.wire_slot_positions())?;
        let slot_packet = serialize_gr_slot_data(&next.slot_data())?;
        let deliveries =
            self.requester_then_room_snapshot(&next, identity.user_no, &team_packet, &slot_packet)?;
        let reserved = self.reserve_outbound(deliveries)?;

        self.protocol_rooms.insert(room_id, next);
        Self::publish_reserved(reserved);
        self.debug_assert_invariants();
        Ok(LobbyCommandOutcome::TeamChanged {
            room_id,
            player_id,
            team,
            slot_id: u8::try_from(target_slot)
                .expect("the fixed physical slot ID always fits in u8"),
        })
    }

    fn change_master(
        &mut self,
        identity: &IdentityBinding,
        target_nickname: &str,
    ) -> Result<LobbyCommandOutcome, LobbyError> {
        let room_id = self.protocol_room_id(identity)?;
        let room = self
            .protocol_rooms
            .get(&room_id)
            .expect("protocol membership always references an existing room");
        Self::require_lobby(room)?;
        let requester_id = room
            .member_id(identity.user_no)
            .ok_or(LobbyError::HumanRacerRequired)?;
        if room.room_master != i32::try_from(requester_id).expect("room member ID fits in i32") {
            return Err(LobbyError::NotRoomMaster);
        }
        let target_key = canonical_nickname_key(target_nickname);
        let target_id = room
            .members_by_id
            .iter()
            .position(|member| {
                member.as_ref().is_some_and(|member| {
                    canonical_nickname_key(&member.player.nickname) == target_key
                })
            })
            .ok_or_else(|| LobbyError::InvalidMasterTarget {
                nickname: target_nickname.to_owned(),
            })?;

        let mut next = room.clone();
        next.members_by_id[requester_id]
            .as_mut()
            .expect("the current master member remains occupied")
            .player
            .player_type = PlayerSlotState::NotReady as i32;
        next.members_by_id[target_id]
            .as_mut()
            .expect("the selected master member remains occupied")
            .player
            .player_type = PlayerSlotState::NotReady as i32;
        let previous_player_id = next.room_master;
        let next_player_id =
            i32::try_from(target_id).expect("the fixed room member ID always fits in i32");
        next.room_master = next_player_id;

        let slot_packet = serialize_gr_slot_data(&next.slot_data())?;
        let batch = OutboundBatch::single(slot_packet);
        let deliveries = self.same_batch_for_room(&next, &batch)?;
        let reserved = self.reserve_outbound(deliveries)?;

        self.protocol_rooms.insert(room_id, next);
        Self::publish_reserved(reserved);
        self.debug_assert_invariants();
        Ok(LobbyCommandOutcome::MasterChanged {
            room_id,
            previous_player_id,
            next_player_id,
        })
    }

    fn start_room(
        &mut self,
        identity: &IdentityBinding,
        plan: &StartRoomPlan,
    ) -> Result<LobbyCommandOutcome, LobbyError> {
        if self.quiescing {
            return Err(LobbyError::WorldQuiescing);
        }
        let room_id = self.protocol_room_id(identity)?;
        let room = self
            .protocol_rooms
            .get(&room_id)
            .expect("protocol membership always references an existing room");
        Self::require_lobby(room)?;
        let requester_id = room
            .member_id(identity.user_no)
            .ok_or(LobbyError::HumanRacerRequired)?;
        if room.room_master != i32::try_from(requester_id).expect("room member ID fits in i32") {
            return Err(LobbyError::NotRoomMaster);
        }
        if !plan.ai_specs.is_empty() {
            return Err(LobbyError::AiParticipantsUnsupported);
        }

        let racer_count = self.validate_start_racers(room, requester_id)?;

        let allocated_race_epoch = self.next_race_epoch.ok_or(LobbyError::RaceEpochExhausted)?;
        let race_epoch = allocated_race_epoch.get();
        let following_race_epoch = race_epoch.checked_add(1).and_then(GlobalRaceEpoch::new);
        let race_fence = RaceFence::new(room_id, allocated_race_epoch);
        let concrete_track =
            room.select_concrete_track(race_epoch, &plan.random_track_candidates)?;
        let frozen = self.freeze_race_roster(room, race_fence, concrete_track)?;
        let human_users = frozen
            .participants
            .iter()
            .filter(|participant| !participant.observer)
            .map(|participant| participant.identity.user_no)
            .collect::<Vec<_>>();
        let observer_count = frozen
            .participants
            .iter()
            .filter(|participant| participant.observer)
            .count();
        let session_data = room.session_data();
        let mut slot_data = room.slot_data();
        slot_data.track = concrete_track;
        let success_reply = serialize_start_room_reply(StartRoomStatus::Success);

        let mut deliveries = Vec::with_capacity(frozen.participants.len());
        for participant in &frozen.participants {
            let member = room
                .member_by_user_no(participant.identity.user_no)
                .expect("a frozen participant originated from this room");
            let command = serialize_gr_command_start_bounded(
                &GrCommandStart {
                    session_data: &session_data,
                    slot_data: &slot_data,
                    kart_physics: &member.kart_physics,
                    ai_specs: &plan.ai_specs,
                    concrete_track,
                },
                plan.maximum_payload_length,
            )?;
            let batch = if participant.identity.user_no == identity.user_no {
                OutboundBatch::ordered(vec![success_reply.clone(), command])
            } else {
                OutboundBatch::single(command)
            };
            deliveries.push((participant.identity.owner, batch));
        }
        let reserved = self.reserve_outbound(deliveries)?;

        let mut next = room.clone();
        next.phase = RoomPhase::Loading;
        next.race_fence = Some(race_fence);
        next.frozen_race = Some(frozen);
        next.race_progress = RaceProgress::default();
        next.loading_handshake = LoadingHandshake::Dormant;
        self.protocol_rooms.insert(room_id, next);
        for user_no in human_users {
            let previous = self.reward_lanes.insert(user_no, race_fence);
            debug_assert!(
                previous.is_none(),
                "reward lane preflight and commit diverged"
            );
        }
        self.next_race_epoch = following_race_epoch;
        Self::publish_reserved(reserved);
        self.debug_assert_invariants();
        Ok(LobbyCommandOutcome::Started {
            room_id,
            race_epoch,
            concrete_track,
            racer_count,
            observer_count,
        })
    }

    fn validate_start_racers(
        &self,
        room: &ProtocolRoomState,
        requester_id: usize,
    ) -> Result<usize, LobbyError> {
        let racer_count = room.members_by_id.iter().flatten().count();
        if racer_count == 0 {
            return Err(LobbyError::NoRacers);
        }
        for (member_id, member) in room
            .members_by_id
            .iter()
            .enumerate()
            .filter_map(|(member_id, member)| member.as_ref().map(|member| (member_id, member)))
        {
            if member_id != requester_id
                && member.player.player_type != PlayerSlotState::Ready as i32
            {
                return Err(LobbyError::RacerNotReady {
                    user_no: member.user_no.get(),
                });
            }
            if let Some(fence) = self.reward_lanes.get(&member.user_no) {
                return Err(LobbyError::RewardLaneOccupied {
                    user_no: member.user_no.get(),
                    room_id: fence.room_id.0,
                    race_epoch: fence.race_epoch.get(),
                });
            }
        }
        Ok(racer_count)
    }

    fn freeze_race_roster(
        &self,
        room: &ProtocolRoomState,
        fence: RaceFence,
        concrete_track: u32,
    ) -> Result<FrozenRaceRoster, LobbyError> {
        let mut participants = Vec::with_capacity(room.user_nos().len());
        for (member_id, member) in room
            .members_by_id
            .iter()
            .enumerate()
            .filter_map(|(member_id, member)| member.as_ref().map(|member| (member_id, member)))
        {
            let identity = self
                .identities
                .active_identity_by_user_no(member.user_no)
                .ok_or(LobbyError::InactiveRosterMember {
                    user_no: member.user_no.get(),
                })?;
            participants.push(FrozenRaceParticipant {
                identity,
                nickname: member.player.nickname.clone(),
                player_id: i32::try_from(member_id)
                    .expect("the fixed room member ID always fits in i32"),
                observer: false,
                team: member.player.team,
                result: Some(FrozenHumanResultSnapshot {
                    kart_id: u16::from_le_bytes(
                        member.player.rider_item_snapshot[4..6]
                            .try_into()
                            .expect("the kart field is a fixed two-byte slice"),
                    ),
                    character_id: u16::from_le_bytes(
                        member.player.rider_item_snapshot[..2]
                            .try_into()
                            .expect("the character field is a fixed two-byte slice"),
                    ),
                    club_mark_logo: member.player.club_mark_logo,
                    economy: FrozenResultEconomy::Pending,
                }),
            });
        }
        for (observer_id, member) in room
            .observers
            .iter()
            .enumerate()
            .filter_map(|(observer_id, member)| member.as_ref().map(|member| (observer_id, member)))
        {
            let identity = self
                .identities
                .active_identity_by_user_no(member.user_no)
                .ok_or(LobbyError::InactiveRosterMember {
                    user_no: member.user_no.get(),
                })?;
            participants.push(FrozenRaceParticipant {
                identity,
                nickname: member.player.nickname.clone(),
                player_id: i32::try_from(ROOM_SLOT_COUNT + observer_id)
                    .expect("the fixed observer player ID always fits in i32"),
                observer: true,
                team: 0,
                result: None,
            });
        }
        Ok(FrozenRaceRoster {
            fence,
            concrete_track,
            participants,
            ais: Vec::new(),
        })
    }

    fn require_lobby(room: &ProtocolRoomState) -> Result<(), LobbyError> {
        if room.phase == RoomPhase::Lobby {
            Ok(())
        } else {
            Err(LobbyError::NotLobby { actual: room.phase })
        }
    }

    fn publish_start_rejection(&self, session: SessionId) -> Result<(), LobbyError> {
        let reserved = self.reserve_outbound(vec![(
            session,
            OutboundBatch::single(serialize_start_room_reply(StartRoomStatus::NotAllReady)),
        )])?;
        Self::publish_reserved(reserved);
        Ok(())
    }

    fn protocol_room_list(
        &mut self,
        session: SessionId,
        identity: &IdentityBinding,
        request: ChGetRoomListRequest,
    ) -> Result<(), WorldOperationError> {
        let mut entries = Vec::new();
        if let Some(channel) = identity.channel
            && expected_room_game_type(channel.game_type) == Some(request.room_list_type)
        {
            let page_start = usize::try_from(request.page)
                .expect("the room-list parser accepts only non-negative pages")
                * 10;
            let mut rooms = self
                .protocol_rooms
                .values()
                .filter(|room| room.settings.channel == channel)
                .filter(|room| room.settings.game_type == request.room_list_type)
                .collect::<Vec<_>>();
            rooms.sort_unstable_by_key(|room| room.id.0);
            entries.extend(
                rooms
                    .into_iter()
                    .skip(page_start)
                    .take(10)
                    .map(ProtocolRoomState::list_entry),
            );
        }

        let packet = serialize_ch_get_room_list_reply(request.page, &entries)?;
        self.deliver(
            vec![(session, OutboundBatch::single(packet))],
            Instant::now(),
        )?;
        Ok(())
    }

    fn prepare_participant(
        identity: &IdentityBinding,
        mut participant: RoomParticipant,
    ) -> RoomParticipant {
        participant.player.user_no = identity.user_no.get();
        participant.player.nickname.clone_from(&identity.nickname);
        participant.player.p2p_address = source_ipv4(identity.source_ip);
        participant
    }

    fn allocate_protocol_room_id(&mut self) -> Option<RoomId> {
        while let Some(candidate) = self.free_protocol_room_ids.iter().next().copied() {
            self.free_protocol_room_ids.remove(&candidate);
            let room_id = RoomId(u32::from(candidate));
            if !self.rooms.contains_key(&room_id) && !self.protocol_rooms.contains_key(&room_id) {
                return Some(room_id);
            }
        }

        let mut candidate = u16::try_from(self.next_room_id)
            .ok()
            .filter(|candidate| *candidate != 0)
            .unwrap_or(1);
        for _ in 0..u32::from(u16::MAX) {
            let room_id = RoomId(u32::from(candidate));
            if !self.rooms.contains_key(&room_id) && !self.protocol_rooms.contains_key(&room_id) {
                self.next_room_id = if candidate == u16::MAX {
                    1
                } else {
                    u32::from(candidate) + 1
                };
                return Some(room_id);
            }
            candidate = candidate.wrapping_add(1).max(1);
        }
        None
    }

    fn protocol_create_room(
        &mut self,
        session: SessionId,
        identity: &IdentityBinding,
        request: ChCreateRoomRequest,
        participant: RoomParticipant,
    ) -> Result<(), WorldOperationError> {
        let game_type = request.game_type;
        let channel = identity.channel;
        let accepted = channel.and_then(|channel| expected_room_game_type(channel.game_type))
            == Some(game_type)
            && !self.protocol_room_by_user.contains_key(&identity.user_no);

        let mut outcome = CreateRoomOutcome::Rejected;
        if accepted && let Some(room_id) = self.allocate_protocol_room_id() {
            let settings = RoomSettings {
                channel: channel.expect("accepted room creation has a selected channel"),
                room_name: request.room_name,
                password: request.password,
                game_type,
                speed_type: 7,
                track: 0,
                room_data_header: request.room_data_header,
                room_data: request.room_data,
            };
            let mut room = ProtocolRoomState::new(room_id, settings);
            let participant = Self::prepare_participant(identity, participant);
            if room.add_participant(identity.user_no, participant) {
                if let Err(error) = serialize_gr_slot_data(&room.slot_data()) {
                    self.free_protocol_room_ids
                        .insert(u16::try_from(room_id.0).expect("protocol room ID fits in u16"));
                    return Err(error.into());
                }
                self.protocol_rooms.insert(room_id, room);
                self.protocol_room_by_user.insert(identity.user_no, room_id);
                outcome = CreateRoomOutcome::Created;
            } else {
                self.free_protocol_room_ids
                    .insert(u16::try_from(room_id.0).expect("protocol room ID fits in u16"));
            }
        }

        self.deliver(
            vec![(
                session,
                OutboundBatch::single(serialize_ch_create_room_reply(outcome, game_type)),
            )],
            Instant::now(),
        )?;
        self.debug_assert_invariants();
        Ok(())
    }

    fn protocol_join_room(
        &mut self,
        session: SessionId,
        identity: &IdentityBinding,
        request: &ChJoinRoomRequest,
        participant: RoomParticipant,
    ) -> Result<(), WorldOperationError> {
        let room_id = RoomId(u32::from(request.room_id));
        let mut replacement = None;
        let (status, game_type) = match self.protocol_rooms.get(&room_id) {
            None => (JoinRoomStatus::Unavailable, 0),
            Some(room)
                if room.phase != RoomPhase::Lobby
                    || identity.channel.is_none_or(|channel| {
                        room.settings.channel != channel
                            || expected_room_game_type(channel.game_type)
                                != Some(room.settings.game_type)
                    }) =>
            {
                (JoinRoomStatus::Unavailable, room.settings.game_type)
            }
            Some(room)
                if !room.settings.password.is_empty()
                    && room.settings.password != request.password =>
            {
                (JoinRoomStatus::WrongPassword, room.settings.game_type)
            }
            Some(room) if self.protocol_room_by_user.contains_key(&identity.user_no) => {
                (JoinRoomStatus::Full, room.settings.game_type)
            }
            Some(room) => {
                let mut next = room.clone();
                let participant = Self::prepare_participant(identity, participant);
                if next.add_participant(identity.user_no, participant) {
                    serialize_gr_slot_data(&next.slot_data())?;
                    let game_type = next.settings.game_type;
                    replacement = Some(next);
                    (JoinRoomStatus::Success, game_type)
                } else {
                    (JoinRoomStatus::Full, room.settings.game_type)
                }
            }
        };

        if let Some(room) = replacement {
            self.protocol_rooms.insert(room_id, room);
            self.protocol_room_by_user.insert(identity.user_no, room_id);
        }
        self.deliver(
            vec![(
                session,
                OutboundBatch::single(serialize_ch_join_room_reply(status, game_type)),
            )],
            Instant::now(),
        )?;
        self.debug_assert_invariants();
        Ok(())
    }

    fn protocol_leave_room(
        &mut self,
        session: SessionId,
        identity: &IdentityBinding,
    ) -> Result<(), WorldOperationError> {
        let left = self.protocol_room_by_user.contains_key(&identity.user_no);
        let mut deliveries = vec![(
            session,
            OutboundBatch::single(serialize_ch_leave_room_reply(left)),
        )];
        if left {
            deliveries.extend(self.remove_protocol_user(identity.user_no));
        }
        self.deliver(deliveries, Instant::now())?;
        self.debug_assert_invariants();
        Ok(())
    }

    fn protocol_first_state(
        &mut self,
        session: SessionId,
        identity: &IdentityBinding,
    ) -> Result<(), WorldOperationError> {
        let Some(room_id) = self.protocol_room_by_user.get(&identity.user_no).copied() else {
            return Ok(());
        };
        let room = self
            .protocol_rooms
            .get(&room_id)
            .expect("protocol membership always references an existing room");
        let initial = serialize_initial_room_state(&room.session_data(), &room.slot_data())?;
        let session_packet = initial[0].logical_packet.clone();
        let slot_packet = initial[1].logical_packet.clone();
        let users = room.user_nos();

        let mut deliveries = Vec::with_capacity(users.len());
        for user_no in users {
            if user_no == identity.user_no {
                deliveries.push((
                    session,
                    OutboundBatch::ordered(vec![session_packet.clone(), slot_packet.clone()]),
                ));
            } else if let Some(recipient) = self.identities.active_identity_by_user_no(user_no) {
                deliveries.push((recipient.owner, OutboundBatch::single(slot_packet.clone())));
            }
        }
        self.deliver(deliveries, Instant::now())?;
        Ok(())
    }

    fn plan_protocol_migration(
        &self,
        replacement: &IdentityBinding,
    ) -> Result<(Option<ProtocolMigrationDelta>, Vec<OutboundDelivery>), WorldOperationError> {
        let Some(room_id) = self
            .protocol_room_by_user
            .get(&replacement.user_no)
            .copied()
        else {
            return Ok((None, Vec::new()));
        };
        let expected_room = self.protocol_rooms.get(&room_id).cloned().ok_or(
            WorldError::MigrationProtocolRoomInconsistent {
                user_no: replacement.user_no.get(),
            },
        )?;
        if replacement.channel == Some(expected_room.settings.channel) {
            return Ok((None, Vec::new()));
        }

        let mut next_room = expected_room.clone();
        if !next_room.remove_user(replacement.user_no) {
            return Err(WorldError::MigrationProtocolRoomInconsistent {
                user_no: replacement.user_no.get(),
            }
            .into());
        }
        let remove_room = next_room.is_empty()
            && matches!(next_room.phase, RoomPhase::Lobby | RoomPhase::Loading);
        let aborted_loading_fence = remove_room
            .then_some(next_room.race_fence)
            .flatten()
            .filter(|_| next_room.phase == RoomPhase::Loading);
        let deliveries = if next_room.is_empty() {
            Vec::new()
        } else {
            let packet = serialize_gr_slot_data(&next_room.slot_data())?;
            self.deliveries_for_users(next_room.user_nos(), &OutboundBatch::single(packet))
        };
        Ok((
            Some(ProtocolMigrationDelta {
                user_no: replacement.user_no,
                room_id,
                expected_room,
                next_room: (!remove_room).then_some(next_room),
                remove_room,
                aborted_loading_fence,
            }),
            deliveries,
        ))
    }

    fn remove_protocol_user(&mut self, user_no: UserNo) -> Vec<OutboundDelivery> {
        let Some(room_id) = self.protocol_room_by_user.remove(&user_no) else {
            return Vec::new();
        };
        let mut remove_room = false;
        let mut aborted_loading_fence = None;
        let mut broadcast = None;
        if let Some(room) = self.protocol_rooms.get_mut(&room_id) {
            let removed = room.remove_user(user_no);
            debug_assert!(removed, "protocol membership map and room state diverged");
            if room.is_empty() && matches!(room.phase, RoomPhase::Lobby | RoomPhase::Loading) {
                remove_room = true;
                if room.phase == RoomPhase::Loading {
                    aborted_loading_fence = room.race_fence;
                }
            } else if !room.is_empty() {
                let packet = serialize_gr_slot_data(&room.slot_data())
                    .expect("validated protocol room state remains serializable after removal");
                broadcast = Some((room.user_nos(), packet));
            }
        }
        if let Some(fence) = aborted_loading_fence {
            self.release_reward_lanes(fence);
        }
        if remove_room {
            self.protocol_rooms.remove(&room_id);
            self.free_protocol_room_ids
                .insert(u16::try_from(room_id.0).expect("protocol room ID fits in u16"));
        }

        broadcast.map_or_else(Vec::new, |(users, packet)| {
            self.deliveries_for_users(users, &OutboundBatch::single(packet))
        })
    }

    fn deliveries_for_users(
        &self,
        users: Vec<UserNo>,
        batch: &OutboundBatch,
    ) -> Vec<OutboundDelivery> {
        users
            .into_iter()
            .filter_map(|user_no| {
                self.identities
                    .active_identity_by_user_no(user_no)
                    .map(|identity| (identity.owner, batch.duplicate()))
            })
            .collect()
    }

    fn active_room_sessions(
        &self,
        room: &ProtocolRoomState,
    ) -> Result<Vec<(UserNo, SessionId)>, LobbyError> {
        room.user_nos()
            .into_iter()
            .map(|user_no| {
                self.identities
                    .active_identity_by_user_no(user_no)
                    .map(|identity| (user_no, identity.owner))
                    .ok_or(LobbyError::InactiveRosterMember {
                        user_no: user_no.get(),
                    })
            })
            .collect()
    }

    fn same_batch_for_room(
        &self,
        room: &ProtocolRoomState,
        batch: &OutboundBatch,
    ) -> Result<Vec<OutboundDelivery>, LobbyError> {
        Ok(self
            .active_room_sessions(room)?
            .into_iter()
            .map(|(_, session)| (session, batch.duplicate()))
            .collect())
    }

    fn requester_then_room_snapshot(
        &self,
        room: &ProtocolRoomState,
        requester: UserNo,
        requester_packet: &[u8],
        room_snapshot: &[u8],
    ) -> Result<Vec<OutboundDelivery>, LobbyError> {
        Ok(self
            .active_room_sessions(room)?
            .into_iter()
            .map(|(user_no, session)| {
                let batch = if user_no == requester {
                    OutboundBatch::ordered(vec![requester_packet.to_vec(), room_snapshot.to_vec()])
                } else {
                    OutboundBatch::single(room_snapshot.to_vec())
                };
                (session, batch)
            })
            .collect())
    }

    fn commit_myroom_transition<T>(
        &mut self,
        transition: MyRoomTransition<T>,
        prepare_fanout: impl FnOnce(&Self, &T) -> Result<Vec<OutboundDelivery>, MyRoomLifecycleError>,
    ) -> Result<T, MyRoomLifecycleError> {
        // Serialization, exact-generation audience resolution, and every
        // bounded queue reservation complete before the first state mutation.
        let deliveries = prepare_fanout(self, transition.outcome())?;
        let reserved = self.reserve_myroom_outbound(deliveries)?;
        let outcome = transition.commit(&mut self.myroom)?;
        debug_assert_eq!(self.myroom.audit_invariants(), Ok(()));
        Self::publish_reserved(reserved);
        Ok(outcome)
    }

    fn commit_silent_myroom_transition<T>(
        &mut self,
        transition: MyRoomTransition<T>,
    ) -> Result<T, MyRoomLifecycleError> {
        let outcome = transition.commit(&mut self.myroom)?;
        debug_assert_eq!(self.myroom.audit_invariants(), Ok(()));
        Ok(outcome)
    }

    fn commit_myroom_command_transition<T>(
        &mut self,
        transition: MyRoomTransition<T>,
        prepare_fanout: impl FnOnce(&Self, &T) -> Result<Vec<OutboundDelivery>, MyRoomLifecycleError>,
    ) -> Result<T, WorldOperationError> {
        // A normal TCP request may encounter bounded backpressure without
        // making the actor inconsistent. Reserve the requester response and
        // every peer publication first; a failed reservation drops all prior
        // permits and leaves the transition uncommitted.
        let deliveries = prepare_fanout(self, transition.outcome())?;
        let reserved = self
            .try_reserve_myroom_outbound(deliveries)
            .map_err(|error| WorldError::MyRoomCommandOutboundUnavailable {
                session: error.session,
            })?;
        let outcome = transition
            .commit(&mut self.myroom)
            .map_err(MyRoomLifecycleError::from)?;
        debug_assert_eq!(self.myroom.audit_invariants(), Ok(()));
        Self::publish_reserved(reserved);
        Ok(outcome)
    }

    fn myroom_publication_deliveries(
        &self,
        publications: &[MyRoomPublication],
    ) -> Result<Vec<OutboundDelivery>, MyRoomLifecycleError> {
        let mut serialized = Vec::with_capacity(publications.len());
        for publication in publications {
            serialized.push((
                publication.audience.as_slice(),
                serialize_slot_data(&publication.snapshot.slots)?,
            ));
        }
        self.myroom_deliveries_from_serialized(serialized, &UnavailableReleaseIndex::default())
    }

    fn myroom_disconnect_deliveries(
        &self,
        outcome: &MyRoomDisconnectOutcome,
        unavailable_releases: &UnavailableReleaseIndex<'_>,
    ) -> Result<Vec<OutboundDelivery>, MyRoomLifecycleError> {
        let MyRoomDisconnectOutcome::Applied(effects) = outcome else {
            return Ok(Vec::new());
        };
        let mut serialized = Vec::with_capacity(2);
        if let Some(MyRoomEffect::Updated(publication)) = &effects.membership_room {
            serialized.push((
                publication.audience.as_slice(),
                serialize_slot_data(&publication.snapshot.slots)?,
            ));
        }
        if let Some(closed) = &effects.closed_owned_room {
            let empty_slots: [MyRoomSlot; p5136_core::myroom_protocol::MYROOM_SLOT_COUNT] =
                std::array::from_fn(|_| MyRoomSlot::Empty);
            serialized.push((
                closed.ejected.as_slice(),
                serialize_slot_data(&empty_slots)?,
            ));
        }
        self.myroom_deliveries_from_serialized(serialized, unavailable_releases)
    }

    fn myroom_deliveries_from_serialized(
        &self,
        serialized: Vec<(&[IdentityBinding], Vec<u8>)>,
        unavailable_releases: &UnavailableReleaseIndex<'_>,
    ) -> Result<Vec<OutboundDelivery>, MyRoomLifecycleError> {
        let mut grouped: Vec<(SessionId, Vec<Vec<u8>>)> = Vec::new();
        for (audience, packet) in serialized {
            for recipient in audience {
                let active = self
                    .identities
                    .active_identity_by_user_no(recipient.user_no);
                if active.is_none() && self.identities.is_current_ownerless_binding(recipient) {
                    continue;
                }
                if active.is_none() && unavailable_releases.contains_exact(recipient) {
                    continue;
                }
                if active.as_ref() != Some(recipient) {
                    return Err(MyRoomLifecycleError::AudienceIdentityMismatch {
                        user_no: recipient.user_no,
                        expected_generation: recipient.generation.get(),
                        actual_generation: active.map(|identity| identity.generation.get()),
                    });
                }
                if let Some((_, packets)) = grouped
                    .iter_mut()
                    .find(|(session, _)| *session == recipient.owner)
                {
                    packets.push(packet.clone());
                } else {
                    grouped.push((recipient.owner, vec![packet.clone()]));
                }
            }
        }
        Ok(grouped
            .into_iter()
            .map(|(session, packets)| (session, OutboundBatch::ordered(packets)))
            .collect())
    }

    fn reserve_myroom_outbound(
        &self,
        deliveries: Vec<OutboundDelivery>,
    ) -> Result<Vec<ReservedOutbound>, MyRoomLifecycleError> {
        self.try_reserve_myroom_outbound(deliveries)
            .map_err(|error| MyRoomLifecycleError::OutboundUnavailable {
                session: error.session,
            })
    }

    fn try_reserve_myroom_outbound(
        &self,
        deliveries: Vec<OutboundDelivery>,
    ) -> Result<Vec<ReservedOutbound>, MyRoomOutboundReservationError> {
        let mut reserved = Vec::with_capacity(deliveries.len());
        for (session, batch) in deliveries {
            let state = self
                .sessions
                .get(&session)
                .ok_or(MyRoomOutboundReservationError { session })?;
            let outbound = state
                .outbound
                .clone()
                .ok_or(MyRoomOutboundReservationError { session })?;
            let permit = outbound
                .try_reserve_owned()
                .map_err(|_| MyRoomOutboundReservationError { session })?;
            let batch = state
                .track_outbound(batch)
                .ok_or(MyRoomOutboundReservationError { session })?;
            reserved.push(ReservedOutbound { permit, batch });
        }
        Ok(reserved)
    }

    fn reserve_outbound(
        &self,
        deliveries: Vec<OutboundDelivery>,
    ) -> Result<Vec<ReservedOutbound>, LobbyError> {
        let mut reserved = Vec::with_capacity(deliveries.len());
        for (session, batch) in deliveries {
            let state = self
                .sessions
                .get(&session)
                .ok_or(LobbyError::OutboundUnavailable { session })?;
            let outbound = state
                .outbound
                .clone()
                .ok_or(LobbyError::OutboundUnavailable { session })?;
            let permit = outbound
                .try_reserve_owned()
                .map_err(|_| LobbyError::OutboundUnavailable { session })?;
            let batch = state
                .track_outbound(batch)
                .ok_or(LobbyError::OutboundUnavailable { session })?;
            reserved.push(ReservedOutbound { permit, batch });
        }
        Ok(reserved)
    }

    fn publish_reserved(reserved: Vec<ReservedOutbound>) {
        for ReservedOutbound { permit, batch } in reserved {
            permit.send(batch);
        }
    }

    fn deliver(
        &mut self,
        deliveries: Vec<OutboundDelivery>,
        now: Instant,
    ) -> Result<(), MyRoomLifecycleError> {
        let mut pending = VecDeque::from(deliveries);
        let mut failed_sessions = HashSet::new();
        while let Some((session, batch)) = pending.pop_front() {
            let tracked = self.sessions.get(&session).and_then(|state| {
                let outbound = state.outbound.clone()?;
                let batch = state.track_outbound(batch)?;
                Some((outbound, batch))
            });
            let failed = match tracked {
                Some((outbound, batch)) => outbound.try_send(batch).is_err(),
                None => self.sessions.contains_key(&session),
            };
            if failed && failed_sessions.insert(session) {
                let mut no_reply = None;
                if !self.defer_session_close_for_rider_equipment(session, &mut no_reply) {
                    if self.quiescing {
                        self.close_session_without_wire(session, now)?;
                    } else {
                        pending.extend(self.close_session_state(session, now)?);
                    }
                }
            }
        }
        Ok(())
    }

    fn cancel_session(&mut self, session: SessionId) {
        if let Some(cancellation) = self
            .sessions
            .get_mut(&session)
            .and_then(|state| state.cancellation.take())
        {
            let _ = cancellation.send(());
        }
    }

    fn cancel_all_sessions(&mut self) {
        for state in self.sessions.values_mut() {
            if let Some(cancellation) = state.cancellation.take() {
                let _ = cancellation.send(());
            }
        }
    }

    fn close_session(
        &mut self,
        session: SessionId,
        now: Instant,
    ) -> Result<(), MyRoomLifecycleError> {
        let deliveries = self.close_session_state(session, now)?;
        self.deliver(deliveries, now)
    }

    fn close_session_state(
        &mut self,
        session: SessionId,
        now: Instant,
    ) -> Result<Vec<OutboundDelivery>, MyRoomLifecycleError> {
        if let Some(mut state) = self.sessions.remove(&session)
            && let Some(cancellation) = state.cancellation.take()
        {
            let _ = cancellation.send(());
        }
        match self.identities.disconnect(session, now) {
            DisconnectOutcome::Released(identity) => self.release_identity_state(&identity),
            DisconnectOutcome::Unauthenticated
            | DisconnectOutcome::Stale(_)
            | DisconnectOutcome::Draining(_)
            | DisconnectOutcome::Deferred { .. } => Ok(Vec::new()),
        }
    }

    fn close_session_without_wire(
        &mut self,
        session: SessionId,
        now: Instant,
    ) -> Result<(), MyRoomLifecycleError> {
        if let Some(mut state) = self.sessions.remove(&session)
            && let Some(cancellation) = state.cancellation.take()
        {
            let _ = cancellation.send(());
        }
        if let DisconnectOutcome::Released(identity) = self.identities.disconnect(session, now) {
            self.release_identity_state_without_wire(&identity)?;
        }
        Ok(())
    }

    fn expire_migrations(&mut self, now: Instant) -> Result<(), MyRoomLifecycleError> {
        let identities = self.identities.expire_migrations(now);
        let unavailable = UnavailableReleaseIndex::from_released(&identities);
        let mut deliveries = Vec::new();
        for identity in &identities {
            deliveries.extend(self.release_identity_state_skipping(identity, &unavailable)?);
        }
        self.deliver(deliveries, now)
    }

    fn expire_migrations_without_wire(&mut self, now: Instant) -> Result<(), MyRoomLifecycleError> {
        let identities = self.identities.expire_migrations(now);
        for identity in &identities {
            self.release_identity_state_without_wire(identity)?;
        }
        Ok(())
    }

    fn collect_drained_identity_releases(
        &mut self,
        now: Instant,
    ) -> Result<(), MyRoomLifecycleError> {
        let identities = self.identities.collect_drained_releases();
        if self.quiescing {
            for identity in &identities {
                self.release_identity_state_without_wire(identity)?;
            }
            return Ok(());
        }
        let unavailable = UnavailableReleaseIndex::from_released(&identities);
        let mut deliveries = Vec::new();
        for identity in &identities {
            deliveries.extend(self.release_identity_state_skipping(identity, &unavailable)?);
        }
        self.deliver(deliveries, now)
    }

    fn release_identity_state(
        &mut self,
        identity: &ReleasedIdentity,
    ) -> Result<Vec<OutboundDelivery>, MyRoomLifecycleError> {
        self.release_identity_state_skipping(identity, &UnavailableReleaseIndex::default())
    }

    fn release_identity_state_skipping(
        &mut self,
        identity: &ReleasedIdentity,
        unavailable_releases: &UnavailableReleaseIndex<'_>,
    ) -> Result<Vec<OutboundDelivery>, MyRoomLifecycleError> {
        self.release_identity_state_with_wire_policy(identity, unavailable_releases, true)
    }

    fn release_identity_state_without_wire(
        &mut self,
        identity: &ReleasedIdentity,
    ) -> Result<(), MyRoomLifecycleError> {
        let unavailable_releases = UnavailableReleaseIndex::default();
        drop(self.release_identity_state_with_wire_policy(
            identity,
            &unavailable_releases,
            false,
        )?);
        Ok(())
    }

    fn release_identity_state_with_wire_policy(
        &mut self,
        identity: &ReleasedIdentity,
        unavailable_releases: &UnavailableReleaseIndex<'_>,
        publish_wire: bool,
    ) -> Result<Vec<OutboundDelivery>, MyRoomLifecycleError> {
        let transition = self
            .myroom
            .disconnect_released(identity)
            .map_err(|error| myroom_hub_error("identity release", error))?;
        if publish_wire {
            self.commit_myroom_transition(
                transition,
                |world, outcome: &MyRoomDisconnectOutcome| {
                    world.myroom_disconnect_deliveries(outcome, unavailable_releases)
                },
            )?;
        } else {
            drop(self.commit_silent_myroom_transition(transition)?);
        }
        self.identity_lifecycle
            .push_back(IdentityLifecycleEvent::Release(identity.clone()));
        if let Some(room_id) = self.room_by_identity.remove(&identity.nickname)
            && let Some(room) = self.rooms.get_mut(&room_id)
            && let Some(slot) = room
                .slots
                .iter_mut()
                .find(|slot| slot.as_deref() == Some(identity.nickname.as_str()))
        {
            *slot = None;
        }
        let deliveries = self.remove_protocol_user(identity.user_no);
        self.debug_assert_invariants();
        Ok(if publish_wire { deliveries } else { Vec::new() })
    }

    fn create_room(&mut self) -> RoomId {
        let id = loop {
            let candidate = RoomId(self.next_room_id);
            self.next_room_id = self.next_room_id.wrapping_add(1).max(1);
            if !self.rooms.contains_key(&candidate) && !self.protocol_rooms.contains_key(&candidate)
            {
                if let Ok(protocol_id) = u16::try_from(candidate.0) {
                    self.free_protocol_room_ids.remove(&protocol_id);
                }
                break candidate;
            }
        };
        self.rooms.insert(
            id,
            RoomSnapshot {
                id,
                slots: std::array::from_fn(|_| None),
            },
        );
        id
    }

    fn room_snapshot(&self, room: RoomId) -> Result<RoomSnapshot, RoomError> {
        self.rooms
            .get(&room)
            .cloned()
            .or_else(|| {
                self.protocol_rooms
                    .get(&room)
                    .map(ProtocolRoomState::snapshot)
            })
            .ok_or(RoomError::NotFound(room.0))
    }

    fn join_room(&mut self, room: RoomId, identity: String) -> Result<SlotId, RoomError> {
        if self.room_by_identity.contains_key(&identity) {
            return Err(RoomError::AlreadyJoined(identity));
        }
        let snapshot = self
            .rooms
            .get_mut(&room)
            .ok_or(RoomError::NotFound(room.0))?;
        let index = snapshot
            .slots
            .iter()
            .position(Option::is_none)
            .ok_or(RoomError::Full(room.0))?;
        snapshot.slots[index] = Some(identity.clone());
        self.room_by_identity.insert(identity, room);
        let slot = u8::try_from(index).expect("an eight-slot room index always fits in u8");
        self.debug_assert_invariants();
        Ok(SlotId(slot))
    }

    fn join_room_for_session(
        &mut self,
        room: RoomId,
        session: SessionId,
    ) -> Result<SlotId, WorldError> {
        let identity = self.authorize_session_operation(session)?;
        Ok(self.join_room(room, identity.nickname)?)
    }

    fn leave_room(&mut self, identity: &str) -> Result<(), RoomError> {
        let room = self
            .room_by_identity
            .remove(identity)
            .ok_or_else(|| RoomError::NotJoined(identity.to_owned()))?;
        let snapshot = self
            .rooms
            .get_mut(&room)
            .ok_or(RoomError::NotFound(room.0))?;
        let slot = snapshot
            .slots
            .iter_mut()
            .find(|slot| slot.as_deref() == Some(identity))
            .ok_or_else(|| RoomError::NotJoined(identity.to_owned()))?;
        *slot = None;
        self.debug_assert_invariants();
        Ok(())
    }

    fn debug_assert_invariants(&self) {
        #[cfg(debug_assertions)]
        {
            let mut seen = HashSet::new();
            for (room_id, room) in &self.rooms {
                for identity in room.slots.iter().flatten() {
                    debug_assert!(seen.insert(identity));
                    debug_assert_eq!(self.room_by_identity.get(identity), Some(room_id));
                }
            }
            debug_assert_eq!(seen.len(), self.room_by_identity.len());

            let mut seen_users = HashSet::new();
            let mut expected_reward_lanes = HashMap::new();
            for (room_id, room) in &self.protocol_rooms {
                debug_assert_eq!(*room_id, room.id);
                debug_assert!(u16::try_from(room_id.0).is_ok());
                debug_assert!(!self.rooms.contains_key(room_id));
                let mut positioned_members = [false; ROOM_SLOT_COUNT];
                for member_id in room.slot_positions.iter().flatten() {
                    let member_id = usize::from(*member_id);
                    debug_assert!(member_id < ROOM_SLOT_COUNT);
                    debug_assert!(room.members_by_id[member_id].is_some());
                    debug_assert!(!positioned_members[member_id]);
                    positioned_members[member_id] = true;
                }
                for (member_id, member) in room.members_by_id.iter().enumerate() {
                    debug_assert_eq!(member.is_some(), positioned_members[member_id]);
                    if let Some(member) = member {
                        debug_assert!(seen_users.insert(member.user_no));
                        debug_assert_eq!(member.player.user_no, member.user_no.get());
                        debug_assert_eq!(
                            self.protocol_room_by_user.get(&member.user_no),
                            Some(room_id)
                        );
                    }
                }
                for observer in room.observers.iter().flatten() {
                    debug_assert!(seen_users.insert(observer.user_no));
                    debug_assert_eq!(observer.player.user_no, observer.user_no.get());
                    debug_assert_eq!(
                        self.protocol_room_by_user.get(&observer.user_no),
                        Some(room_id)
                    );
                }
                if room.members_by_id.iter().any(Option::is_some) {
                    let master =
                        usize::try_from(room.room_master).expect("room master is non-negative");
                    debug_assert!(master < ROOM_SLOT_COUNT);
                    debug_assert!(room.members_by_id[master].is_some());
                }
                debug_assert_frozen_race_invariants(*room_id, room, &mut expected_reward_lanes);
                debug_assert_loading_handshake_invariants(room);
                debug_assert_race_progress_invariants(room);
            }
            debug_assert_eq!(seen_users.len(), self.protocol_room_by_user.len());
            debug_assert_eq!(expected_reward_lanes, self.reward_lanes);
            for room_id in &self.free_protocol_room_ids {
                debug_assert_ne!(*room_id, 0);
                let room_id = RoomId(u32::from(*room_id));
                debug_assert!(!self.rooms.contains_key(&room_id));
                debug_assert!(!self.protocol_rooms.contains_key(&room_id));
            }
        }
    }
}

#[cfg(debug_assertions)]
fn debug_assert_frozen_race_invariants(
    room_id: RoomId,
    room: &ProtocolRoomState,
    expected_reward_lanes: &mut HashMap<UserNo, RaceFence>,
) {
    match (&room.phase, &room.frozen_race) {
        (RoomPhase::Lobby, None) => {
            debug_assert!(room.race_fence.is_none());
        }
        (RoomPhase::Lobby, Some(_)) | (_, None) => {
            debug_assert!(false, "room phase and frozen roster diverged");
        }
        (_, Some(frozen)) => {
            debug_assert_eq!(room.race_fence, Some(frozen.fence));
            debug_assert_eq!(frozen.fence.room_id, room_id);
            debug_assert!(!is_random_track_selector(frozen.concrete_track));
            let mut frozen_users = HashSet::new();
            let mut frozen_player_ids = HashSet::new();
            for participant in &frozen.participants {
                debug_assert!(frozen_users.insert(participant.identity.user_no));
                debug_assert!(frozen_player_ids.insert(participant.player_id));
                debug_assert_eq!(participant.observer, participant.player_id >= 8);
                debug_assert_eq!(participant.observer, participant.result.is_none());
                if !participant.observer {
                    debug_assert!(
                        expected_reward_lanes
                            .insert(participant.identity.user_no, frozen.fence)
                            .is_none()
                    );
                }
                debug_assert!(if participant.observer {
                    participant.team == 0
                } else if matches!(room.settings.game_type, 3 | 4) {
                    matches!(participant.team, 1 | 2)
                } else {
                    participant.team == 0
                });
            }
            for participant in &frozen.ais {
                debug_assert!((0..8).contains(&participant.player_id));
                debug_assert!(frozen_player_ids.insert(participant.player_id));
            }
            debug_assert!(frozen_player_ids.len() <= ROOM_SLOT_COUNT);
        }
    }
}

#[cfg(debug_assertions)]
fn debug_assert_race_progress_invariants(room: &ProtocolRoomState) {
    match room.phase {
        RoomPhase::Lobby | RoomPhase::Loading | RoomPhase::Running => {
            debug_assert!(room.race_progress.settlement.is_none());
        }
        RoomPhase::Settling => {
            debug_assert!(room.race_progress.settlement.is_some());
        }
    }
    if matches!(room.phase, RoomPhase::Lobby | RoomPhase::Loading) {
        debug_assert!(room.race_progress.finish_times.is_empty());
        debug_assert!(room.race_progress.pending_fanouts.is_empty());
        debug_assert_eq!(room.race_progress.team_gauge_bits, [0.0_f32.to_bits(); 2]);
    }
    let pending_finish_count = room
        .race_progress
        .pending_fanouts
        .iter()
        .filter(|fanout| fanout.race_time.is_some())
        .count();
    debug_assert!(pending_finish_count <= ROOM_SLOT_COUNT);
    for fanout in &room.race_progress.pending_fanouts {
        if let Some(race_time) = &fanout.race_time {
            debug_assert_eq!(
                room.race_progress.finish_times.get(&race_time.player_id),
                Some(&race_time.race_time)
            );
        }
        if let Some(begin) = &fanout.begin_settlement {
            let settlement = room
                .race_progress
                .settlement
                .as_ref()
                .expect("a pending BeginSettlement has settlement timing");
            debug_assert_eq!(begin.end_tick, settlement.end_tick);
        }
    }
    if let Some(frozen) = &room.frozen_race {
        let racer_ids = frozen
            .participants
            .iter()
            .filter(|participant| !participant.observer)
            .map(|participant| participant.player_id)
            .chain(frozen.ais.iter().map(|participant| participant.player_id))
            .collect::<HashSet<_>>();
        debug_assert!(
            room.race_progress
                .finish_times
                .keys()
                .all(|player_id| racer_ids.contains(player_id))
        );
        if let Some(settlement) = &room.race_progress.settlement
            && let SettlementFinalization::Persisting { ranking, rewards } =
                &settlement.finalization
        {
            let human_count = frozen
                .participants
                .iter()
                .filter(|participant| !participant.observer)
                .count();
            debug_assert_eq!(rewards.len(), human_count);
            debug_assert!(rewards.len() <= ROOM_SLOT_COUNT);
            let mut reward_users = HashSet::new();
            for reward in rewards {
                debug_assert!(reward_users.insert(reward.user_no));
                debug_assert!(ranking.by_player_id.contains_key(&reward.player_id));
                let participant = frozen.participants.iter().find(|participant| {
                    !participant.observer && participant.identity.user_no == reward.user_no
                });
                debug_assert!(participant.is_some());
                if let Some(participant) = participant {
                    let economy = participant
                        .result
                        .as_ref()
                        .and_then(|result| result.economy.applied());
                    match reward.status {
                        RewardPersistenceStatus::Durable(applied) => {
                            debug_assert_eq!(economy, Some(applied));
                        }
                        RewardPersistenceStatus::Queued { .. }
                        | RewardPersistenceStatus::InFlight { .. } => {
                            debug_assert!(economy.is_none());
                        }
                    }
                }
            }
        }
    }
}

#[cfg(debug_assertions)]
fn debug_assert_loading_handshake_invariants(room: &ProtocolRoomState) {
    match &room.loading_handshake {
        LoadingHandshake::Dormant => {}
        LoadingHandshake::StartScheduled { .. } => {
            debug_assert_eq!(room.phase, RoomPhase::Loading);
        }
        LoadingHandshake::Awaiting {
            expected,
            ready,
            deadline: _,
        } => {
            debug_assert_eq!(room.phase, RoomPhase::Loading);
            debug_assert!(ready.is_subset(expected));
            let frozen = room
                .frozen_race
                .as_ref()
                .expect("an awaiting room has a frozen roster");
            let frozen_stamps = frozen
                .participants
                .iter()
                .map(|participant| FrozenParticipantStamp::from(&participant.identity))
                .collect::<HashSet<_>>();
            debug_assert!(expected.is_subset(&frozen_stamps));
        }
    }
}

fn messenger_identity_from_binding(
    identity: &IdentityBinding,
) -> Result<MessengerIdentity, MessengerServiceError> {
    MessengerIdentity::new(
        identity.user_no.get(),
        identity.nickname.clone(),
        identity.generation.get(),
        identity.source_ip,
    )
    .map_err(MessengerServiceError::from)
}

fn messenger_identity_from_release(
    identity: &ReleasedIdentity,
) -> Result<MessengerIdentity, MessengerServiceError> {
    MessengerIdentity::new(
        identity.user_no.get(),
        identity.nickname.clone(),
        identity.generation.get(),
        identity.source_ip,
    )
    .map_err(MessengerServiceError::from)
}

async fn flush_identity_lifecycle(
    world: &mut World,
    sidecars: &WorldSidecars,
) -> Result<(), WorldSidecarError> {
    world
        .collect_drained_identity_releases(Instant::now())
        .map_err(WorldSidecarError::MyRoom)?;
    if sidecars.messenger.is_none() && sidecars.udp.is_none() {
        world.identity_lifecycle.clear();
        return Ok(());
    }

    while let Some(event) = world.identity_lifecycle.front().cloned() {
        if let Some(messenger) = &sidecars.messenger {
            match &event {
                IdentityLifecycleEvent::Announce(identity) => {
                    messenger
                        .announce_identity(messenger_identity_from_binding(identity)?)
                        .await?;
                }
                IdentityLifecycleEvent::Advance { previous, next } => {
                    messenger
                        .advance_identity(
                            messenger_identity_from_release(previous)?,
                            messenger_identity_from_binding(next)?,
                        )
                        .await?;
                }
                IdentityLifecycleEvent::Release(identity) => {
                    messenger
                        .release_identity(messenger_identity_from_release(identity)?)
                        .await?;
                }
            }
        }
        if let Some(udp) = &sidecars.udp {
            match event {
                IdentityLifecycleEvent::Announce(identity) => {
                    udp.advance_identity(identity).await?;
                }
                IdentityLifecycleEvent::Advance { next, .. } => {
                    udp.advance_identity(next).await?;
                }
                IdentityLifecycleEvent::Release(identity) => {
                    udp.release_identity(identity).await?;
                }
            }
        }
        world.identity_lifecycle.pop_front();
    }
    Ok(())
}

fn myroom_hub_error(operation: &'static str, source: MyRoomHubError) -> MyRoomLifecycleError {
    MyRoomLifecycleError::Hub { operation, source }
}

fn released_matches_binding(released: &ReleasedIdentity, binding: &IdentityBinding) -> bool {
    released.nickname == binding.nickname
        && released.user_no == binding.user_no
        && released.generation == binding.generation
        && released.source_ip == binding.source_ip
        && released.channel == binding.channel
}

async fn reply_after_identity_lifecycle<T>(
    world: &mut World,
    sidecars: &WorldSidecars,
    reply: oneshot::Sender<T>,
    value: T,
) -> Result<(), WorldSidecarError> {
    flush_identity_lifecycle(world, sidecars).await?;
    let _ = reply.send(value);
    Ok(())
}

async fn reply_after_world_operation<T>(
    world: &mut World,
    sidecars: &WorldSidecars,
    reply: oneshot::Sender<Result<T, WorldError>>,
    value: Result<T, WorldOperationError>,
) -> Result<(), WorldSidecarError> {
    match value {
        Ok(value) => reply_after_identity_lifecycle(world, sidecars, reply, Ok(value)).await,
        Err(WorldOperationError::Command(error)) => {
            reply_after_identity_lifecycle(world, sidecars, reply, Err(error)).await
        }
        Err(WorldOperationError::MyRoom(error)) => Err(WorldSidecarError::MyRoom(error)),
    }
}

async fn receive_udp_ingress(
    receiver: &mut Option<mpsc::Receiver<UdpIngress>>,
) -> Option<UdpIngress> {
    match receiver {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

async fn receive_myroom_completion(
    receiver: &mut Option<mpsc::Receiver<MyRoomProfileCompletion>>,
) -> Option<MyRoomProfileCompletion> {
    match receiver {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

async fn dispatch_udp_ingress(
    world: &mut World,
    udp: &UdpService,
    ingress: UdpIngress,
) -> Result<(), WorldSidecarError> {
    let Some(user_no) = UserNo::new(ingress.account_id) else {
        tracing::trace!(
            source = %ingress.source,
            "dropping UDP ingress with account ID zero"
        );
        return Ok(());
    };
    let operation = match world
        .identities
        .admit_udp_operation(user_no, ingress.arrival_epoch)
    {
        Ok(operation) => operation,
        Err(error) => {
            tracing::trace!(
                account_id = ingress.account_id,
                source = %ingress.source,
                %error,
                "dropping UDP ingress outside its exact generation admission window"
            );
            return Ok(());
        }
    };
    let identity = operation.binding().clone();
    let account_id = ingress.account_id;
    let source = ingress.source;
    let readiness_candidate = matches!(&ingress.body, UdpIngressBody::PqUdpTimeSync(_))
        .then(|| world.loading_readiness_candidate(&identity))
        .flatten();
    let room_targets = world.myroom_udp_targets(&identity)?;
    let request = UdpDispatchRequest {
        ingress,
        identity,
        racing_targets: world.racing_udp_targets(user_no),
        room_targets,
    };

    let result = match udp.dispatch(request).await {
        Ok(outcome) => {
            world.apply_udp_dispatch_readiness(readiness_candidate, outcome);
            Ok(())
        }
        Err(
            error @ UdpServiceError::EndpointState(
                UdpEndpointStateError::InvalidSourceEndpoint { .. }
                | UdpEndpointStateError::SourceIpMismatch { .. }
                | UdpEndpointStateError::EndpointMismatch { .. },
            ),
        ) => {
            tracing::trace!(
                account_id,
                %source,
                %error,
                "dropping rejected UDP source endpoint"
            );
            Ok(())
        }
        Err(error) => Err(WorldSidecarError::Udp(error)),
    };
    drop(operation);
    result
}

async fn run_world(
    receiver: mpsc::Receiver<WorldCommand>,
    completion_receiver: mpsc::Receiver<MyRoomProfileCompletion>,
    udp_receiver: Option<mpsc::Receiver<UdpIngress>>,
    sidecars: WorldSidecars,
    clock: ServerClock,
    identities: IdentityRegistry,
) -> Result<(), WorldSidecarError> {
    let identity_capacity = sidecars
        .identity_capacity()
        .unwrap_or(DEFAULT_WORLD_IDENTITY_CAPACITY);
    let identity_capacity =
        NonZeroUsize::new(identity_capacity).ok_or(WorldSidecarError::InvalidIdentityCapacity)?;
    let world = World {
        identities,
        identity_capacity,
        myroom: MyRoomHub::with_identity_capacity(identity_capacity),
        ..World::default()
    };
    debug_assert_eq!(world.myroom.identity_capacity(), world.identity_capacity);
    run_world_actor(
        world,
        receiver,
        completion_receiver,
        udp_receiver,
        sidecars,
        clock,
    )
    .await
}

async fn run_world_actor(
    world: World,
    receiver: mpsc::Receiver<WorldCommand>,
    completion_receiver: mpsc::Receiver<MyRoomProfileCompletion>,
    udp_receiver: Option<mpsc::Receiver<UdpIngress>>,
    sidecars: WorldSidecars,
    clock: ServerClock,
) -> Result<(), WorldSidecarError> {
    run_world_actor_with_timers(
        world,
        receiver,
        completion_receiver,
        udp_receiver,
        sidecars,
        clock,
        WorldActorTimers::new(),
    )
    .await
}

struct WorldActorTimers {
    migration_expiry: tokio::time::Interval,
    loading_heartbeat: tokio::time::Interval,
}

impl WorldActorTimers {
    fn new() -> Self {
        let mut migration_expiry = tokio::time::interval(Duration::from_secs(1));
        migration_expiry.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut loading_heartbeat = tokio::time::interval(LOADING_HEARTBEAT_INTERVAL);
        loading_heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
        Self {
            migration_expiry,
            loading_heartbeat,
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the biased actor select loop keeps timer, completion, command, and UDP precedence auditable in one place"
)]
async fn run_world_actor_with_timers(
    mut world: World,
    mut receiver: mpsc::Receiver<WorldCommand>,
    completion_receiver: mpsc::Receiver<MyRoomProfileCompletion>,
    mut udp_receiver: Option<mpsc::Receiver<UdpIngress>>,
    sidecars: WorldSidecars,
    clock: ServerClock,
    mut timers: WorldActorTimers,
) -> Result<(), WorldSidecarError> {
    let mut completion_receiver = Some(completion_receiver);
    loop {
        // Expiry is first so a perpetually ready control mailbox cannot retain
        // ownerless identities forever. Commands still precede heartbeat and
        // UDP work once the once-per-second maintenance tick is serviced.
        tokio::select! {
            biased;

            _ = timers.migration_expiry.tick() => {
                if world.quiescing {
                    continue;
                }
                let now = Instant::now();
                world.advance_loading(now, &clock);
                world.expire_migrations(now)?;
                flush_identity_lifecycle(&mut world, &sidecars).await?;
                world.advance_loading(Instant::now(), &clock);
            }
            completion = receive_myroom_completion(&mut completion_receiver) => {
                let Some(completion) = completion else {
                    if !world.pending_myroom_writes.is_empty()
                        || !world.pending_myroom_by_user.is_empty()
                        || !world.pending_rider_equipment_writes.is_empty()
                        || !world.pending_rider_equipment_by_user.is_empty()
                        || world.identities.transfer_in_progress_count() != 0
                    {
                        return Err(
                            MyRoomPersistenceInvariantError::CompletionMailboxClosed.into()
                        );
                    }
                    completion_receiver = None;
                    continue;
                };
                world.advance_loading(Instant::now(), &clock);
                let mut migration_reply = None;
                let deferred_close_replies = match completion {
                    MyRoomProfileCompletion::RiderEquipment(completion) => {
                        world.handle_rider_equipment_profile_completion(completion)?
                    }
                    MyRoomProfileCompletion::Migration(
                        MigrationProfileCompletion::Aborted { preflight },
                    ) => {
                        world
                            .identities
                            .abort_preflighted_migration(preflight.as_ref());
                        Vec::new()
                    }
                    MyRoomProfileCompletion::Migration(
                        MigrationProfileCompletion::Ready {
                            preflight,
                            profile,
                            acknowledgement,
                            reply,
                        },
                    ) => {
                        let result = match world.complete_preflighted_migration_with_acknowledgement(
                            *preflight,
                            Some(profile.as_ref()),
                            acknowledgement,
                            sidecars.udp.as_ref(),
                            Instant::now(),
                        ) {
                            Ok(completion) => Ok(completion),
                            Err(WorldOperationError::Command(error)) => Err(error),
                            Err(WorldOperationError::MyRoom(error)) => return Err(error.into()),
                        };
                        migration_reply = Some((reply, result));
                        Vec::new()
                    }
                    completion => {
                        world.handle_myroom_profile_completion(completion)?;
                        Vec::new()
                    }
                };
                flush_identity_lifecycle(&mut world, &sidecars).await?;
                for reply in deferred_close_replies {
                    let _ = reply.send(());
                }
                if let Some((reply, result)) = migration_reply {
                    let _ = reply.send(result);
                }
                world.advance_loading(Instant::now(), &clock);
            }
            command = receiver.recv() => {
                let Some(command) = command else {
                    break;
                };
                world.advance_loading(Instant::now(), &clock);
                let should_stop =
                    dispatch_command(&mut world, command, &sidecars, &clock).await?;
                world.advance_loading(Instant::now(), &clock);
                if should_stop {
                    break;
                }
            }
            _ = timers.loading_heartbeat.tick() => {
                world.advance_loading(Instant::now(), &clock);
            }
            ingress = receive_udp_ingress(&mut udp_receiver) => {
                let Some(ingress) = ingress else {
                    udp_receiver = None;
                    continue;
                };
                if world.quiescing {
                    continue;
                }
                let Some(udp) = sidecars.udp.as_ref() else {
                    debug_assert!(false, "UDP ingress mailbox requires a UDP sidecar");
                    continue;
                };
                world.advance_loading(Instant::now(), &clock);
                let result = dispatch_udp_ingress(&mut world, udp, ingress).await;
                world.advance_loading(Instant::now(), &clock);
                result?;
            }
        }
    }
    flush_identity_lifecycle(&mut world, &sidecars).await?;
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "the exhaustive actor command dispatcher must visibly own every reply path"
)]
async fn dispatch_command(
    world: &mut World,
    command: WorldCommand,
    sidecars: &WorldSidecars,
    clock: &ServerClock,
) -> Result<bool, WorldSidecarError> {
    match command {
        WorldCommand::AdmitIdentityOperation { session, reply } => {
            let result = if world.quiescing {
                Err(WorldError::OutboundProductionClosed)
            } else {
                world
                    .identities
                    .admit_operation(session)
                    .map_err(WorldError::from)
            };
            reply_after_identity_lifecycle(world, sidecars, reply, result).await?;
            Ok(false)
        }
        WorldCommand::AdmittedIdentityOperation { operation, command } => {
            if !operation.belongs_to(&world.identities.instance()) {
                tracing::error!(
                    binding = ?operation.binding(),
                    "rejected an identity operation minted by another World actor"
                );
                drop(command);
                drop(operation);
                return Ok(false);
            }
            let previous = world.admitted_identity.replace(operation.binding().clone());
            let result = Box::pin(dispatch_command(world, *command, sidecars, clock)).await;
            world.admitted_identity = previous;
            drop(operation);
            result
        }
        command => return dispatch_unwrapped_command(world, command, sidecars, clock).await,
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the exhaustive actor command dispatcher must visibly own every reply path"
)]
async fn dispatch_unwrapped_command(
    world: &mut World,
    command: WorldCommand,
    sidecars: &WorldSidecars,
    clock: &ServerClock,
) -> Result<bool, WorldSidecarError> {
    let command = if world.quiescing {
        let Some(command) = admit_command_during_quiesce(command) else {
            return Ok(false);
        };
        command
    } else {
        command
    };
    match command {
        WorldCommand::AdmitIdentityOperation { .. }
        | WorldCommand::AdmittedIdentityOperation { .. } => {
            unreachable!("identity-operation envelopes are removed before ordinary dispatch")
        }
        WorldCommand::RegisterSession {
            peer,
            cancellation,
            outbound,
            outbound_operations,
            reply,
        } => {
            let result = world.register_session_with_operations(
                peer,
                cancellation,
                outbound,
                outbound_operations,
            );
            reply_after_identity_lifecycle(world, sidecars, reply, result).await?;
        }
        WorldCommand::SessionClosed { id, reply } => {
            dispatch_session_closed(world, sidecars, id, reply).await?;
        }
        WorldCommand::ClaimIdentity {
            session,
            nickname,
            reply,
        } => {
            // A last operation may have retired between actor turns. Publish
            // its exact Release before admitting a capacity-bearing Announce.
            flush_identity_lifecycle(world, sidecars).await?;
            let activated_udp_epoch = sidecars
                .udp
                .as_ref()
                .map_or(0, UdpService::advance_arrival_epoch);
            let result = world.claim_identity_at_udp_epoch(session, &nickname, activated_udp_epoch);
            reply_after_identity_lifecycle(world, sidecars, reply, result).await?;
        }
        WorldCommand::AuthorizeIdentity { session, reply } => {
            dispatch_authorize_identity(world, sidecars, session, reply).await?;
        }
        WorldCommand::BeginMigration {
            session,
            channel,
            token,
            now,
            reply,
        } => {
            let result = world
                .identities
                .begin_migration(session, channel, token, now)
                .map_err(WorldError::from);
            reply_after_identity_lifecycle(world, sidecars, reply, result).await?;
        }
        #[cfg(test)]
        WorldCommand::CompleteMigration {
            destination,
            user_no,
            channel_id,
            token,
            now,
            reply,
        } => {
            let result = world.complete_migration(destination, user_no, channel_id, token, now);
            reply_after_world_operation(world, sidecars, reply, result).await?;
        }
        WorldCommand::RoomProtocol {
            session,
            payload,
            reply,
        } => {
            let result = world.room_protocol(session, *payload);
            reply_after_world_operation(world, sidecars, reply, result).await?;
        }
        WorldCommand::PublishRoomEquipment {
            session,
            snapshot,
            reply,
        } => {
            let result = world.publish_room_equipment(session, *snapshot);
            reply_after_world_operation(world, sidecars, reply, result).await?;
        }
        WorldCommand::RefreshMyRoomPresentation {
            session,
            expected,
            profile,
            reply,
        } => {
            let result = world.refresh_myroom_presentation(session, &expected, &profile);
            reply_after_world_operation(world, sidecars, reply, result).await?;
        }
        WorldCommand::Lobby {
            session,
            payload,
            reply,
        } => {
            let result = world.lobby_command(session, payload);
            reply_after_identity_lifecycle(world, sidecars, reply, result).await?;
        }
        WorldCommand::Race {
            session,
            payload,
            reply,
        } => {
            let result = world.race_command_with_clock(session, payload, Instant::now(), clock);
            reply_after_identity_lifecycle(world, sidecars, reply, result).await?;
        }
        WorldCommand::RegisterMyRoomInfoWrite {
            session,
            prepared,
            request_reply,
            reply,
        } => dispatch_myroom_info_registration(world, session, prepared, request_reply, reply)?,
        WorldCommand::RegisterRiderEquipmentWrite {
            session,
            prepared,
            request_reply,
            reply,
        } => dispatch_rider_equipment_registration(world, session, prepared, request_reply, reply)?,
        command @ (WorldCommand::MyRoomSessionView { .. }
        | WorldCommand::PrepareMyRoom { .. }
        | WorldCommand::PrepareMyRoomOwnerItems { .. }
        | WorldCommand::MyRoom { .. }
        | WorldCommand::MyRoomPeer { .. }
        | WorldCommand::PublishMyRoomOwnerItems { .. }
        | WorldCommand::DrainSessions { .. }) => {
            dispatch_guarded_world_command(world, sidecars, command).await?;
        }
        command => return dispatch_utility_command(world, command, sidecars).await,
    }
    Ok(false)
}

/// Rejects externally initiated work which could mutate player-visible state
/// or publish another transport batch after the quiesce boundary.
///
/// Completion, reconciliation, read-only, status, drain, and shutdown commands
/// remain admitted. Profile-write registrations are also routed normally so
/// their domain-specific `WorldQuiescing` errors remain observable.
fn admit_command_during_quiesce(command: WorldCommand) -> Option<WorldCommand> {
    match command {
        WorldCommand::AdmitIdentityOperation { .. }
        | WorldCommand::AdmittedIdentityOperation { .. } => None,
        WorldCommand::RegisterSession { reply, .. } => {
            let _ = reply.send(Err(WorldError::SessionRegistrationClosed));
            None
        }
        WorldCommand::ClaimIdentity { reply, .. } => {
            let _ = reply.send(Err(WorldError::OutboundProductionClosed));
            None
        }
        WorldCommand::BeginMigration { reply, .. } => {
            let _ = reply.send(Err(WorldError::OutboundProductionClosed));
            None
        }
        #[cfg(test)]
        WorldCommand::CompleteMigration { reply, .. } => {
            let _ = reply.send(Err(WorldError::OutboundProductionClosed));
            None
        }
        WorldCommand::PreflightMigration { reply, .. } => {
            let _ = reply.send(Err(WorldError::OutboundProductionClosed));
            None
        }
        WorldCommand::RoomProtocol { reply, .. }
        | WorldCommand::PublishRoomEquipment { reply, .. }
        | WorldCommand::LeaveRoom { reply, .. }
        | WorldCommand::MyRoom { reply, .. }
        | WorldCommand::MyRoomPeer { reply, .. }
        | WorldCommand::PublishMyRoomOwnerItems { reply, .. }
        | WorldCommand::RetryRewardDeadLetter { reply, .. } => {
            let _ = reply.send(Err(WorldError::OutboundProductionClosed));
            None
        }
        WorldCommand::Lobby { reply, .. } => {
            let _ = reply.send(Err(WorldError::OutboundProductionClosed));
            None
        }
        WorldCommand::Race { reply, .. } => {
            let _ = reply.send(Err(WorldError::OutboundProductionClosed));
            None
        }
        WorldCommand::CreateRoom { reply } => {
            let _ = reply.send(Err(WorldError::OutboundProductionClosed));
            None
        }
        WorldCommand::JoinRoom { reply, .. } | WorldCommand::JoinRoomForSession { reply, .. } => {
            let _ = reply.send(Err(WorldError::OutboundProductionClosed));
            None
        }
        command @ (WorldCommand::SessionClosed { .. }
        | WorldCommand::AuthorizeIdentity { .. }
        | WorldCommand::RefreshMyRoomPresentation { .. }
        | WorldCommand::RoomSnapshot { .. }
        | WorldCommand::SessionCount { .. }
        | WorldCommand::TakeDueRewardTasks { .. }
        | WorldCommand::CompleteRewardTask { .. }
        | WorldCommand::Quiesce { .. }
        | WorldCommand::DrainOutboundProducers { .. }
        | WorldCommand::DrainSessions { .. }
        | WorldCommand::PrepareMyRoom { .. }
        | WorldCommand::PrepareMyRoomOwnerItems { .. }
        | WorldCommand::RegisterMyRoomInfoWrite { .. }
        | WorldCommand::RegisterRiderEquipmentWrite { .. }
        | WorldCommand::MyRoomSessionView { .. }
        | WorldCommand::RewardDrainStatus { .. }
        | WorldCommand::Shutdown { .. }
        | WorldCommand::ForceShutdown { .. }) => Some(command),
    }
}

async fn dispatch_session_closed(
    world: &mut World,
    sidecars: &WorldSidecars,
    session: SessionId,
    mut reply: Option<oneshot::Sender<()>>,
) -> Result<(), WorldSidecarError> {
    if world.defer_session_close_for_rider_equipment(session, &mut reply) {
        return Ok(());
    }
    if world.quiescing {
        world.close_session_without_wire(session, Instant::now())?;
    } else {
        world.close_session(session, Instant::now())?;
    }
    flush_identity_lifecycle(world, sidecars).await?;
    if let Some(reply) = reply {
        let _ = reply.send(());
    }
    Ok(())
}

async fn dispatch_authorize_identity(
    world: &mut World,
    sidecars: &WorldSidecars,
    session: SessionId,
    reply: oneshot::Sender<Result<IdentityBinding, WorldError>>,
) -> Result<(), WorldSidecarError> {
    let result = world
        .authorize_session_operation(session)
        .map_err(WorldError::from);
    reply_after_identity_lifecycle(world, sidecars, reply, result).await
}

async fn dispatch_guarded_world_command(
    world: &mut World,
    sidecars: &WorldSidecars,
    command: WorldCommand,
) -> Result<(), WorldSidecarError> {
    match command {
        WorldCommand::MyRoomSessionView { session, reply } => {
            let result = world.myroom_session_view(session);
            reply_after_world_operation(world, sidecars, reply, result).await
        }
        WorldCommand::PrepareMyRoom { session, reply } => {
            let result = world.prepare_myroom_command(session);
            reply_after_world_operation(world, sidecars, reply, result).await
        }
        WorldCommand::PrepareMyRoomOwnerItems { session, reply } => {
            let result = world.prepare_myroom_owner_items(session);
            reply_after_world_operation(world, sidecars, reply, result).await
        }
        WorldCommand::MyRoom {
            session,
            payload,
            prepared,
            reply,
        } => {
            let result = world.myroom_command(session, payload, *prepared);
            reply_after_world_operation(world, sidecars, reply, result).await
        }
        WorldCommand::MyRoomPeer {
            session,
            payload,
            reply,
        } => {
            let result = world.myroom_peer_command(session, payload);
            reply_after_world_operation(world, sidecars, reply, result).await
        }
        WorldCommand::PublishMyRoomOwnerItems {
            session,
            prepared,
            reply,
        } => {
            let result = world.publish_myroom_owner_items(session, *prepared);
            reply_after_world_operation(world, sidecars, reply, result).await
        }
        WorldCommand::DrainSessions { reply } => {
            let result = world.drain_sessions_for_shutdown();
            reply_after_world_operation(world, sidecars, reply, result).await
        }
        _ => unreachable!("only guarded World commands are routed to this dispatcher"),
    }
}

fn dispatch_myroom_info_registration(
    world: &mut World,
    session: SessionId,
    prepared: PreparedMyRoomInfoWrite,
    request_reply: oneshot::Sender<Result<MyRoomInfoWriteReceipt, MyRoomInfoWriteError>>,
    reply: oneshot::Sender<Result<RegisteredMyRoomInfoWrite, MyRoomInfoWriteError>>,
) -> Result<(), WorldSidecarError> {
    match world.register_myroom_info_write(session, prepared, request_reply) {
        Ok(registered) => {
            let _ = reply.send(Ok(registered));
            Ok(())
        }
        Err(MyRoomInfoRegistrationError::Request(error)) => {
            let _ = reply.send(Err(*error));
            Ok(())
        }
        Err(MyRoomInfoRegistrationError::MyRoom(error)) => Err(WorldSidecarError::MyRoom(*error)),
        Err(MyRoomInfoRegistrationError::Terminal(error)) => {
            Err(WorldSidecarError::MyRoomPersistence(*error))
        }
    }
}

fn dispatch_rider_equipment_registration(
    world: &mut World,
    session: SessionId,
    prepared: PreparedRiderEquipmentWrite,
    request_reply: oneshot::Sender<Result<RiderEquipmentWriteReceipt, RiderEquipmentWriteError>>,
    reply: oneshot::Sender<Result<RegisteredRiderEquipmentWrite, RiderEquipmentWriteError>>,
) -> Result<(), WorldSidecarError> {
    match world.register_rider_equipment_write(session, prepared, request_reply) {
        Ok(registered) => {
            let _ = reply.send(Ok(registered));
            Ok(())
        }
        Err(RiderEquipmentRegistrationError::Request(error)) => {
            let _ = reply.send(Err(*error));
            Ok(())
        }
        Err(RiderEquipmentRegistrationError::Terminal(error)) => {
            Err(WorldSidecarError::RiderEquipment(*error))
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the actor utility command dispatcher exhaustively owns every reply path"
)]
async fn dispatch_utility_command(
    world: &mut World,
    command: WorldCommand,
    sidecars: &WorldSidecars,
) -> Result<bool, WorldSidecarError> {
    match command {
        WorldCommand::PreflightMigration {
            destination,
            user_no,
            channel_id,
            token,
            now,
            completion,
            reply,
        } => {
            let result = world
                .preflight_migration(destination, user_no, channel_id, token, now)
                .map(|preflight| RegisteredMigrationPreflight::new(preflight, completion));
            reply_after_identity_lifecycle(world, sidecars, reply, result).await?;
        }
        WorldCommand::CreateRoom { reply } => {
            let result = Ok(world.create_room());
            reply_after_identity_lifecycle(world, sidecars, reply, result).await?;
        }
        WorldCommand::JoinRoom {
            room,
            identity,
            reply,
        } => {
            let result = world.join_room(room, identity).map_err(WorldError::from);
            reply_after_identity_lifecycle(world, sidecars, reply, result).await?;
        }
        WorldCommand::JoinRoomForSession {
            room,
            session,
            reply,
        } => {
            let result = world.join_room_for_session(room, session);
            reply_after_identity_lifecycle(world, sidecars, reply, result).await?;
        }
        WorldCommand::LeaveRoom { identity, reply } => {
            let result = world.leave_room(&identity).map_err(WorldError::from);
            reply_after_identity_lifecycle(world, sidecars, reply, result).await?;
        }
        WorldCommand::RoomSnapshot { room, reply } => {
            let result = world.room_snapshot(room);
            reply_after_identity_lifecycle(world, sidecars, reply, result).await?;
        }
        WorldCommand::SessionCount { reply } => {
            let result = world.sessions.len();
            reply_after_identity_lifecycle(world, sidecars, reply, result).await?;
        }
        WorldCommand::TakeDueRewardTasks {
            now,
            maximum,
            reply,
        } => {
            let result = world.take_due_reward_tasks(now, maximum);
            reply_after_identity_lifecycle(world, sidecars, reply, result).await?;
        }
        WorldCommand::CompleteRewardTask {
            completion,
            now,
            reply,
        } => {
            let result = world.complete_reward_task(completion, now);
            reply_after_world_operation(world, sidecars, reply, result).await?;
        }
        WorldCommand::Quiesce { reply } => {
            world.quiesce();
            reply_after_identity_lifecycle(world, sidecars, reply, ()).await?;
        }
        WorldCommand::DrainOutboundProducers { reply } => {
            let result = world.drain_outbound_producers_once();
            reply_after_world_operation(world, sidecars, reply, result).await?;
        }
        WorldCommand::RewardDrainStatus { reply } => {
            let result = world.reward_drain_status();
            reply_after_identity_lifecycle(world, sidecars, reply, result).await?;
        }
        WorldCommand::RetryRewardDeadLetter { dead_letter, reply } => {
            let result = world.retry_reward_dead_letter(dead_letter, Instant::now());
            reply_after_identity_lifecycle(world, sidecars, reply, result).await?;
        }
        WorldCommand::Shutdown { reply } => match world.reward_drain_status() {
            Ok(status)
                if status.is_drained()
                    && world.pending_myroom_writes.is_empty()
                    && world.pending_myroom_by_user.is_empty()
                    && world.pending_rider_equipment_writes.is_empty()
                    && world.pending_rider_equipment_by_user.is_empty()
                    && world.identities.transfer_in_progress_count() == 0
                    && world.identities.outstanding_operation_count() == 0 =>
            {
                let lifecycle = flush_identity_lifecycle(world, sidecars).await;
                world.cancel_all_sessions();
                lifecycle?;
                let _ = reply.send(Ok(()));
                return Ok(true);
            }
            Ok(status) if status.is_drained() => {
                let active_operations = world.identities.outstanding_operation_count();
                let error = if active_operations != 0 {
                    WorldError::IdentityOperationShutdownBlocked {
                        active: active_operations,
                    }
                } else {
                    WorldError::MyRoomPersistenceShutdownBlocked {
                        pending: world
                            .pending_myroom_writes
                            .len()
                            .saturating_add(world.pending_rider_equipment_writes.len())
                            .saturating_add(world.identities.transfer_in_progress_count()),
                        indexed: world
                            .pending_myroom_by_user
                            .len()
                            .saturating_add(world.pending_rider_equipment_by_user.len()),
                    }
                };
                let _ = reply.send(Err(error));
            }
            Ok(status) => {
                let _ = reply.send(Err(WorldError::RewardShutdownBlocked {
                    outstanding_lanes: status.outstanding_lanes.len(),
                    dead_letters: status.dead_letters.len(),
                }));
            }
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        },
        WorldCommand::ForceShutdown { reply } => {
            let report = WorldForceShutdownReport::capture(world);
            if report.has_abandoned_completion_work() {
                tracing::warn!(
                    pending_myroom_tickets = report.myroom_tickets,
                    pending_myroom_user_indexes = report.myroom_user_indexes,
                    pending_rider_equipment_tickets = report.rider_equipment_tickets,
                    pending_rider_equipment_user_indexes = report.rider_equipment_user_indexes,
                    pending_migration_transfers = report.migration_transfers,
                    pending_identity_operations = report.identity_operations,
                    "force shutdown is abandoning profile publication or migration completion work before actor reconciliation and final request reply"
                );
            }
            let lifecycle = flush_identity_lifecycle(world, sidecars).await;
            world.cancel_all_sessions();
            lifecycle?;
            let _ = reply.send(report);
            return Ok(true);
        }
        #[cfg(test)]
        WorldCommand::CompleteMigration { .. } => {
            unreachable!("test migration completion is dispatched by dispatch_command")
        }
        WorldCommand::AdmitIdentityOperation { .. }
        | WorldCommand::AdmittedIdentityOperation { .. }
        | WorldCommand::RegisterSession { .. }
        | WorldCommand::SessionClosed { .. }
        | WorldCommand::ClaimIdentity { .. }
        | WorldCommand::AuthorizeIdentity { .. }
        | WorldCommand::BeginMigration { .. }
        | WorldCommand::RoomProtocol { .. }
        | WorldCommand::PublishRoomEquipment { .. }
        | WorldCommand::RefreshMyRoomPresentation { .. }
        | WorldCommand::Lobby { .. }
        | WorldCommand::Race { .. }
        | WorldCommand::PrepareMyRoom { .. }
        | WorldCommand::PrepareMyRoomOwnerItems { .. }
        | WorldCommand::MyRoom { .. }
        | WorldCommand::MyRoomPeer { .. }
        | WorldCommand::PublishMyRoomOwnerItems { .. }
        | WorldCommand::RegisterMyRoomInfoWrite { .. }
        | WorldCommand::RegisterRiderEquipmentWrite { .. }
        | WorldCommand::MyRoomSessionView { .. }
        | WorldCommand::DrainSessions { .. } => {
            unreachable!("identity-affecting commands are dispatched by dispatch_command")
        }
    }
    Ok(false)
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::myroom_hub::{MyRoomOwner, MyRoomParticipant};
    use p5136_core::myroom_protocol::MyRoomPlayerSlot;

    pub(crate) struct DueRewardWorld {
        pub(crate) handle: WorldHandle,
        pub(crate) actor: JoinHandle<Result<(), WorldSidecarError>>,
        pub(crate) outbound_receivers: Vec<mpsc::Receiver<OutboundBatch>>,
    }

    pub(crate) struct PausedFullMailboxWorld {
        pub(crate) handle: WorldHandle,
        pub(crate) actor: JoinHandle<Result<(), WorldSidecarError>>,
        pub(crate) start: oneshot::Sender<()>,
    }

    pub(crate) struct TestMyRoomSession {
        pub(crate) session: SessionId,
        pub(crate) identity: IdentityBinding,
        pub(crate) outbound: mpsc::Receiver<OutboundBatch>,
    }

    pub(crate) struct MyRoomWorld {
        pub(crate) handle: WorldHandle,
        pub(crate) actor: JoinHandle<Result<(), WorldSidecarError>>,
        pub(crate) owner: TestMyRoomSession,
        pub(crate) visitor: TestMyRoomSession,
    }

    fn register_channel_session(
        world: &mut World,
        nickname: &str,
        source_port: u16,
        outbound_capacity: usize,
    ) -> (SessionId, IdentityBinding, mpsc::Receiver<OutboundBatch>) {
        let source_ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let source = world
            .register_session(SocketAddr::new(source_ip, source_port), None, None)
            .unwrap();
        let claimed = world.claim_identity(source, nickname).unwrap();
        let channel = ChannelBinding {
            channel_id: 67,
            game_type: 67,
        };
        let token = MigrationToken::new(source_port).unwrap();
        world
            .identities
            .begin_migration(source, channel, token, Instant::now())
            .unwrap();
        let (outbound, receiver) = mpsc::channel(outbound_capacity);
        let destination = world
            .register_session(
                SocketAddr::new(source_ip, source_port + 1),
                None,
                Some(outbound),
            )
            .unwrap();
        let completion = world
            .complete_migration(
                destination,
                claimed.user_no,
                channel.channel_id,
                token,
                Instant::now(),
            )
            .unwrap();
        (destination, completion.binding, receiver)
    }

    fn participant() -> RoomParticipant {
        RoomParticipant {
            player: RoomPlayer {
                player_type: 2,
                user_no: 1,
                p2p_address: Ipv4Addr::LOCALHOST,
                p2p_port: 39_312,
                nickname: "untrusted".to_owned(),
                emblem_1: 0,
                emblem_2: 0,
                rider_item_snapshot: [0; p5136_core::startup::RIDER_ITEM_SNAPSHOT_WIRE_LENGTH],
                card: String::new(),
                rp: 0,
                team: 0,
                ranking: 0,
                rider_school_level: 0,
                club_name: String::new(),
                club_mark_logo: 0,
            },
            observer: false,
            kart_physics: P5136KartPhysicsBlock::from([0; 235]),
        }
    }

    fn create_room_request(nickname: &str) -> ChCreateRoomRequest {
        ChCreateRoomRequest {
            room_name: format!("{nickname} reward"),
            password: String::new(),
            game_type: 1,
            reserved_after_game_type: 0,
            ai_count: 0,
            room_data_header: 0,
            room_data: [0; ROOM_DATA_LENGTH],
            connection_context: [0; p5136_core::room_protocol::ROOM_CONNECTION_CONTEXT_LENGTH],
            reserved_before_ai_switch: 0,
            ai_switch: 0,
            reserved_after_ai_switch_1: 0,
            reserved_after_ai_switch_2: 0,
            reserved_tail: 0,
            reserved_last: 0,
        }
    }

    fn drain(receiver: &mut mpsc::Receiver<OutboundBatch>) {
        while receiver.try_recv().is_ok() {}
    }

    fn myroom_participant(identity: &IdentityBinding) -> MyRoomParticipant {
        MyRoomParticipant::new(
            identity.clone(),
            MyRoomPlayerSlot {
                user_no: identity.user_no.get(),
                p2p_address: Ipv4Addr::LOCALHOST,
                p2p_port: 39_312,
                nickname: identity.nickname.clone(),
                rider_item_snapshot: [0; p5136_core::startup::RIDER_ITEM_SNAPSHOT_WIRE_LENGTH],
                rp: 20_000_000,
                club_name: String::new(),
            },
        )
        .unwrap()
    }

    pub(crate) fn spawn_myroom_world(owner_info: MyRoomInfo) -> MyRoomWorld {
        spawn_myroom_world_with_outbound_capacity(owner_info, SESSION_OUTBOUND_CAPACITY)
    }

    pub(crate) fn spawn_myroom_world_with_outbound_capacity(
        owner_info: MyRoomInfo,
        outbound_capacity: usize,
    ) -> MyRoomWorld {
        let mut world = World::default();
        let (owner_session, owner_identity, owner_outbound) =
            register_channel_session(&mut world, "SessionMyRoomOwner", 47_000, outbound_capacity);
        let (visitor_session, visitor_identity, visitor_outbound) = register_channel_session(
            &mut world,
            "SessionMyRoomVisitor",
            47_100,
            outbound_capacity,
        );
        let owner = MyRoomOwner::new(myroom_participant(&owner_identity), owner_info).unwrap();
        world
            .myroom
            .enter(&myroom_participant(&owner_identity), &owner)
            .unwrap()
            .commit(&mut world.myroom)
            .unwrap();
        world
            .myroom
            .enter(&myroom_participant(&visitor_identity), &owner)
            .unwrap()
            .commit(&mut world.myroom)
            .unwrap();

        let (sender, receiver) = mpsc::channel(32);
        let identity_instance = world.identities.instance();
        let (myroom_completions, completion_receiver) =
            MyRoomCompletionBridge::channel(world.identity_capacity);
        let handle = WorldHandle {
            sender,
            udp_sender: None,
            myroom_completions,
            identity_instance,
        };
        let actor = tokio::spawn(async move {
            run_world_actor(
                world,
                receiver,
                completion_receiver,
                None,
                WorldSidecars::default(),
                ServerClock::new(),
            )
            .await
        });
        MyRoomWorld {
            handle,
            actor,
            owner: TestMyRoomSession {
                session: owner_session,
                identity: owner_identity,
                outbound: owner_outbound,
            },
            visitor: TestMyRoomSession {
                session: visitor_session,
                identity: visitor_identity,
                outbound: visitor_outbound,
            },
        }
    }

    pub(crate) fn spawn_due_reward_world(nicknames: &[&str]) -> DueRewardWorld {
        let mut world = World::default();
        let clock = ServerClock::new();
        let mut outbound_receivers = Vec::with_capacity(nicknames.len());
        let finished_at = Instant::now()
            .checked_sub(SETTLEMENT_DELAY + Duration::from_secs(1))
            .unwrap();

        for (index, nickname) in nicknames.iter().enumerate() {
            let source_port = 46_000 + u16::try_from(index * 2).unwrap();
            let (session, identity, mut outbound) = register_channel_session(
                &mut world,
                nickname,
                source_port,
                SESSION_OUTBOUND_CAPACITY,
            );
            world
                .room_protocol(
                    session,
                    RoomCommandPayload::Create {
                        request: create_room_request(nickname),
                        participant: participant(),
                    },
                )
                .unwrap();
            let room_id = world.protocol_room_by_user[&identity.user_no];
            drain(&mut outbound);
            world
                .lobby_command(
                    session,
                    LobbyCommandPayload::StartRoom(StartRoomPlan::new(
                        vec![0x1111_2222],
                        Vec::new(),
                    )),
                )
                .unwrap();
            drain(&mut outbound);
            let room = world.protocol_rooms.get_mut(&room_id).unwrap();
            room.phase = RoomPhase::Running;
            room.loading_handshake = LoadingHandshake::Dormant;
            room.race_progress = RaceProgress::default();
            world
                .race_command_with_clock(
                    session,
                    RaceCommandPayload::GameControl(GameControlRequest {
                        state: 2,
                        optional_pair: None,
                        value0: 456,
                        trailing: Vec::new(),
                    }),
                    finished_at,
                    &clock,
                )
                .unwrap();
            drain(&mut outbound);
            let deadline = world.protocol_rooms[&room_id]
                .race_progress
                .settlement
                .as_ref()
                .unwrap()
                .deadline;
            world.advance_loading(deadline, &clock);
            outbound_receivers.push(outbound);
        }

        let (sender, receiver) = mpsc::channel(64);
        let identity_instance = world.identities.instance();
        let completion_capacity = world.identity_capacity;
        let (myroom_completions, completion_receiver) =
            MyRoomCompletionBridge::channel(completion_capacity);
        let handle = WorldHandle {
            sender,
            udp_sender: None,
            myroom_completions,
            identity_instance,
        };
        let actor = tokio::spawn(async move {
            run_world_actor(
                world,
                receiver,
                completion_receiver,
                None,
                WorldSidecars::default(),
                clock,
            )
            .await
        });
        DueRewardWorld {
            handle,
            actor,
            outbound_receivers,
        }
    }

    pub(crate) fn spawn_paused_full_mailbox_world() -> PausedFullMailboxWorld {
        let (sender, receiver) = mpsc::channel(1);
        let (reply, _response) = oneshot::channel();
        sender
            .try_send(WorldCommand::SessionCount { reply })
            .expect("test World mailbox should accept its single queued command");
        let completion_capacity = NonZeroUsize::new(DEFAULT_WORLD_IDENTITY_CAPACITY)
            .expect("default test World identity capacity is nonzero");
        let (myroom_completions, completion_receiver) =
            MyRoomCompletionBridge::channel(completion_capacity);
        let world = World::default();
        let identity_instance = world.identities.instance();
        let handle = WorldHandle {
            sender,
            udp_sender: None,
            myroom_completions,
            identity_instance,
        };
        let (start, started) = oneshot::channel();
        let actor = tokio::spawn(async move {
            let _ = started.await;
            run_world_actor(
                world,
                receiver,
                completion_receiver,
                None,
                WorldSidecars::default(),
                ServerClock::new(),
            )
            .await
        });
        PausedFullMailboxWorld {
            handle,
            actor,
            start,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        array,
        collections::HashSet,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        num::NonZeroU64,
        sync::{Arc, Barrier},
        time::{Duration, Instant},
    };

    use p5136_core::{
        adler32,
        channel::serialize_pr_channel_move_in,
        equipment_protocol::serialize_room_slot_items,
        lobby_protocol::{
            CHANGE_TEAM_REPLY_NAME, PlayerSlotState, RoomTeam, SET_SLOT_STATE_REPLY_NAME,
            SLOT_STATE_PACKET_NAME, START_ROOM_REPLY_NAME,
        },
        myroom_protocol::{
            CharacterPositionRequest, MYROOM_SLOT_COUNT, MyRoomInfo, MyRoomPlayerSlot, MyRoomSlot,
            serialize_character_position, serialize_missing_owner_items, serialize_myroom_info,
            serialize_secede_reply, serialize_slot_data,
        },
        nickname::canonical_nickname_key,
        race_protocol::{
            AiGoalInRequest, GAME_AI_MASTER_NOTICE_NAME, GAME_CONTROL_PACKET_NAME,
            GAME_NEXT_STAGE_PACKET_NAME, GAME_RACE_TIME_PACKET_NAME, GameControlRequest, RaceTeam,
            TEAM_BOOSTER_REPLY_NAME, TeamBoosterGaugeRequest,
        },
        race_result_protocol::{GAME_RESULT_PACKET_NAME, ResultTeam},
        race_start_protocol::{
            AiRaceSpec, GR_COMMAND_START_PACKET_NAME, P5136KartPhysicsBlock, RaceStartProtocolError,
        },
        room_protocol::{
            ChCreateRoomRequest, ChGetRoomListRequest, ChJoinRoomRequest,
            MAX_CLUB_NAME_UTF16_UNITS, ROOM_CONNECTION_CONTEXT_LENGTH, ROOM_DATA_LENGTH,
            RoomPlayer, RoomProtocolError,
        },
        startup::RIDER_ITEM_SNAPSHOT_WIRE_LENGTH,
    };
    use p5136_profile::{Profile, ProfileStore, rider_item_snapshot};
    use tokio::{
        net::UdpSocket,
        sync::{mpsc, oneshot},
        time,
    };

    use super::{
        GlobalRaceEpoch, LOADING_HEARTBEAT_INTERVAL, LoadingHandshake, LobbyCommandOutcome,
        LobbyCommandPayload, LobbyError, MigrationAcknowledgement, MyRoomCommandPayload,
        MyRoomLifecycleError, MyRoomPeerCommandPayload, MyRoomPreparedCommand,
        MyRoomWireProjection, OutboundBatch, ROOM_CAPACITY, RaceCommandOutcome, RaceCommandPayload,
        RaceError, RegisteredMigrationPreflight, RoomCommandPayload, RoomError, RoomId,
        RoomParticipant, RoomPhase, SessionId, StartRoomPlan, World, WorldCommand, WorldError,
        WorldHandle, WorldOperationError, WorldSidecarError, WorldSidecars, WorldSpawnError,
        dispatch_command, source_ipv4,
    };
    use crate::equipment_persistence::{
        PreparedRiderEquipmentWrite, RiderEquipmentWriteError,
        tests::{catalog as test_equipment_catalog, selection as test_equipment_selection},
    };
    use crate::myroom_hub::{MyRoomOwner, MyRoomParticipant, MyRoomProfilePresentation};
    use crate::myroom_persistence::{
        MyRoomCompletionBridge, MyRoomInfoPublication, MyRoomInfoWriteError,
        PreparedMyRoomInfoWrite,
    };
    use crate::operation_gate::WireOperationGate;
    use crate::profile_io::{MyRoomProfileLease, ProfileIoBootstrap, ProfileIoLimits};
    use crate::{
        ChannelBinding, IdentityBinding, IdentityError, MessengerHubLimits, MessengerRuntimeConfig,
        MessengerServiceError, MessengerServiceHandle, MigrationToken, ServerClock,
        UdpDispatchAction, UdpDispatchOutcome, UdpDispatchRequest, UdpEndpointBindStatus,
        UdpEndpointStateError, UdpIngress, UdpIngressBody, UdpRuntime, UdpRuntimeConfig,
        UdpServiceError, UdpTransport,
    };

    struct TestChannelSession {
        session: SessionId,
        identity: IdentityBinding,
        outbound: mpsc::Receiver<OutboundBatch>,
    }

    struct CountingRewardRolls {
        rp: u8,
        lucci: u16,
        rp_draws: usize,
        lucci_draws: usize,
    }

    fn spawn_prepared_world(
        world: World,
        mailbox_capacity: usize,
    ) -> (WorldHandle, tokio::task::JoinHandle<()>) {
        let (sender, receiver) = mpsc::channel(mailbox_capacity);
        let identity_instance = world.identities.instance();
        let completion_capacity = world.identity_capacity;
        let (myroom_completions, completion_receiver) =
            crate::myroom_persistence::MyRoomCompletionBridge::channel(completion_capacity);
        let handle = WorldHandle {
            sender,
            udp_sender: None,
            myroom_completions,
            identity_instance,
        };
        let actor = tokio::spawn(async move {
            let result = super::run_world_actor(
                world,
                receiver,
                completion_receiver,
                None,
                super::WorldSidecars::default(),
                ServerClock::new(),
            )
            .await;
            assert!(result.is_ok());
        });
        (handle, actor)
    }

    fn spawn_test_profile_io(
        root: &std::path::Path,
    ) -> (
        crate::profile_io::ProfileIoHandle,
        crate::profile_io::ProfileIoRuntime,
    ) {
        ProfileIoBootstrap::acquire(root.to_owned(), ProfileIoLimits::for_tests(8, 8))
            .unwrap()
            .spawn()
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the shared end-to-end harness keeps both durable success and rejection close-barrier paths identical"
    )]
    async fn exercise_rider_equipment_close_barrier(expect_success: bool) {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let nickname = if expect_success {
            "EquipmentCloseSuccess"
        } else {
            "EquipmentCloseFailure"
        };
        let initial = store.save(nickname, &Profile::default()).unwrap();
        let (profiles, profile_runtime) = spawn_test_profile_io(root.path());

        let mut prepared_world = World::default();
        let mut owner = register_channel_session(&mut prepared_world, nickname, 67, 56_070, 16);
        let mut peer =
            register_channel_session(&mut prepared_world, "EquipmentClosePeer", 67, 56_072, 16);
        let room_id = create_protocol_room(&mut prepared_world, &owner, 1);
        join_protocol_room(&mut prepared_world, &peer, room_id, false);
        drain_batches(&mut owner.outbound);
        drain_batches(&mut peer.outbound);

        let session = owner.session;
        let (world, actor) = spawn_prepared_world(prepared_world, 16);
        let completion = world.reserve_rider_equipment_completion().await.unwrap();
        let admission = profiles
            .admit(nickname, "test deferred equipment close")
            .await
            .unwrap();
        let mut selection = test_equipment_selection();
        if !expect_success {
            selection.character = u16::MAX;
        }
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let hook_entered = Arc::clone(&entered);
        let hook_release = Arc::clone(&release);
        let prepared = PreparedRiderEquipmentWrite::new(
            admission,
            selection,
            Arc::new(test_equipment_catalog()),
            completion,
        )
        .with_test_hook(Arc::new(move || {
            hook_entered.wait();
            hook_release.wait();
        }));
        let (request_reply, request_result) = oneshot::channel();
        let (registration_reply, registration_result) = oneshot::channel();
        world
            .sender
            .send(WorldCommand::RegisterRiderEquipmentWrite {
                session,
                prepared,
                request_reply,
                reply: registration_reply,
            })
            .await
            .unwrap();
        registration_result.await.unwrap().unwrap().submit();
        let entered_wait = Arc::clone(&entered);
        tokio::task::spawn_blocking(move || entered_wait.wait())
            .await
            .unwrap();

        let close_world = world.clone();
        let mut close = tokio::spawn(async move { close_world.session_closed(session).await });
        assert!(
            time::timeout(Duration::from_millis(20), &mut close)
                .await
                .is_err(),
            "session close acknowledged before the durable equipment outcome"
        );
        let release_wait = Arc::clone(&release);
        tokio::task::spawn_blocking(move || release_wait.wait())
            .await
            .unwrap();

        let request = time::timeout(Duration::from_secs(1), request_result)
            .await
            .expect("equipment request did not reach a terminal outcome")
            .unwrap();
        time::timeout(Duration::from_secs(1), &mut close)
            .await
            .expect("deferred equipment close was not released")
            .unwrap()
            .unwrap();
        profile_runtime.shutdown().await.unwrap();
        world.drain_myroom_completions().await.unwrap();

        match (expect_success, request) {
            (true, Ok(_)) | (false, Err(RiderEquipmentWriteError::Persistence(_))) => {}
            (expected, actual) => {
                panic!("unexpected equipment close outcome for success={expected}: {actual:?}")
            }
        }
        let loaded = store.load_or_create(nickname).unwrap();
        assert_eq!(
            loaded.revision,
            Some(if expect_success {
                initial.revision + 1
            } else {
                initial.revision
            })
        );

        let mut peer_hashes = Vec::new();
        while let Ok(batch) = peer.outbound.try_recv() {
            peer_hashes.extend(
                batch
                    .into_packets()
                    .iter()
                    .map(|packet| logical_packet_hash(packet)),
            );
        }
        let equipment_hash = adler32::packet_hash("GrSlotItemOnPacket");
        if expect_success {
            assert_eq!(peer_hashes.first(), Some(&equipment_hash));
        } else {
            assert!(!peer_hashes.contains(&equipment_hash));
        }
        assert!(matches!(
            world.authorize_identity(session).await,
            Err(WorldError::Identity(
                IdentityError::UnauthenticatedSession(actual)
            )) if actual == session
        ));

        world.shutdown().await.unwrap();
        actor.await.unwrap();
    }

    fn prepare_myroom_owner(
        nickname: &str,
        port: u16,
        outbound_capacity: usize,
    ) -> (World, TestChannelSession) {
        let mut world = World::default();
        let owner = register_channel_session(&mut world, nickname, 67, port, outbound_capacity);
        enter_myroom(
            &mut world,
            &owner.identity,
            &owner.identity,
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::LOCALHOST,
        );
        (world, owner)
    }

    impl super::RewardRollSource for CountingRewardRolls {
        fn draw_rp(&mut self) -> u8 {
            self.rp_draws += 1;
            self.rp
        }

        fn draw_lucci(&mut self) -> u16 {
            self.lucci_draws += 1;
            self.lucci
        }
    }

    fn register_channel_session(
        world: &mut World,
        nickname: &str,
        channel_game_type: u8,
        port: u16,
        outbound_capacity: usize,
    ) -> TestChannelSession {
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let source = world
            .register_session(SocketAddr::new(ip, port), None, None)
            .unwrap();
        let claimed = world.claim_identity(source, nickname).unwrap();
        let channel = ChannelBinding {
            channel_id: u16::from(channel_game_type),
            game_type: channel_game_type,
        };
        let token = MigrationToken::new(port.max(1)).unwrap();
        world
            .identities
            .begin_migration(source, channel, token, Instant::now())
            .unwrap();
        let (outbound, receiver) = mpsc::channel(outbound_capacity);
        let destination = world
            .register_session(
                SocketAddr::new(ip, port.wrapping_add(1)),
                None,
                Some(outbound),
            )
            .unwrap();
        let completion = world
            .complete_migration(
                destination,
                claimed.user_no,
                channel.channel_id,
                token,
                Instant::now(),
            )
            .unwrap();
        TestChannelSession {
            session: destination,
            identity: completion.binding,
            outbound: receiver,
        }
    }

    fn migrate_channel_session(
        world: &mut World,
        source: &TestChannelSession,
        destination_port: u16,
        outbound_capacity: usize,
    ) -> TestChannelSession {
        migrate_channel_session_to(
            world,
            source,
            source.identity.channel.unwrap(),
            destination_port,
            outbound_capacity,
        )
    }

    fn migrate_channel_session_to(
        world: &mut World,
        source: &TestChannelSession,
        channel: ChannelBinding,
        destination_port: u16,
        outbound_capacity: usize,
    ) -> TestChannelSession {
        let token = MigrationToken::new(destination_port.max(1)).unwrap();
        world
            .identities
            .begin_migration(source.session, channel, token, Instant::now())
            .unwrap();
        let (outbound, receiver) = mpsc::channel(outbound_capacity);
        let destination = world
            .register_session(
                SocketAddr::new(source.identity.source_ip, destination_port),
                None,
                Some(outbound),
            )
            .unwrap();
        let completion = world
            .complete_migration(
                destination,
                source.identity.user_no,
                channel.channel_id,
                token,
                Instant::now(),
            )
            .unwrap();
        TestChannelSession {
            session: destination,
            identity: completion.binding,
            outbound: receiver,
        }
    }

    fn myroom_participant(
        identity: &IdentityBinding,
        presented_ip: Ipv4Addr,
        rp: u32,
    ) -> MyRoomParticipant {
        MyRoomParticipant::new(
            identity.clone(),
            MyRoomPlayerSlot {
                user_no: identity.user_no.get(),
                p2p_address: presented_ip,
                p2p_port: 39_312,
                nickname: identity.nickname.clone(),
                rider_item_snapshot: [identity.user_no.get().to_le_bytes()[0];
                    RIDER_ITEM_SNAPSHOT_WIRE_LENGTH],
                rp,
                club_name: format!("club-{}", identity.user_no.get()),
            },
        )
        .unwrap()
    }

    fn test_myroom_profile_presentation() -> MyRoomProfilePresentation {
        MyRoomProfilePresentation::new(
            39_312,
            [0; RIDER_ITEM_SNAPSHOT_WIRE_LENGTH],
            100,
            "migration-profile".to_owned(),
        )
    }

    fn myroom_owner(identity: &IdentityBinding, presented_ip: Ipv4Addr) -> MyRoomOwner {
        MyRoomOwner::new(
            myroom_participant(identity, presented_ip, 100),
            MyRoomInfo {
                room_id: i16::try_from(identity.user_no.get()).unwrap(),
                ..MyRoomInfo::default()
            },
        )
        .unwrap()
    }

    fn enter_myroom(
        world: &mut World,
        member: &IdentityBinding,
        owner: &IdentityBinding,
        member_ip: Ipv4Addr,
        owner_ip: Ipv4Addr,
    ) {
        let transition = world
            .myroom
            .enter(
                &myroom_participant(member, member_ip, 200),
                &myroom_owner(owner, owner_ip),
            )
            .unwrap();
        transition.commit(&mut world.myroom).unwrap();
    }

    fn prepare_test_myroom_command(
        world: &World,
        session: SessionId,
    ) -> (MyRoomPreparedCommand, Option<MyRoomWireProjection>) {
        let plan = world.prepare_myroom_command(session).unwrap();
        let projection = plan.wire_plan().map(|wire| {
            let topology = world
                .myroom
                .first_snapshot(wire.requester())
                .expect("a member wire plan has a current room snapshot");
            let identities = wire
                .slot_identities()
                .map(Option::<&IdentityBinding>::cloned)
                .collect::<Vec<_>>();
            let players = array::from_fn(|slot| {
                let identity = identities[slot].as_ref()?;
                let MyRoomSlot::Player(mut player) = topology.slots[slot].clone() else {
                    panic!("wire plan occupancy and cached topology must agree");
                };
                player.p2p_address = source_ipv4(identity.source_ip);
                Some(player)
            });
            wire.project(players).unwrap()
        });
        let prepared = plan.complete(projection.clone()).unwrap();
        (prepared, projection)
    }

    fn projected_slots_for_topology(
        projection: &MyRoomWireProjection,
        topology: &crate::myroom_hub::MyRoomSnapshot,
    ) -> [MyRoomSlot; MYROOM_SLOT_COUNT] {
        let projected = projection.snapshot().slots;
        array::from_fn(|slot| match &topology.slots[slot] {
            MyRoomSlot::Empty => MyRoomSlot::Empty,
            MyRoomSlot::Player(topology_player) => projected
                .iter()
                .find_map(|candidate| match candidate {
                    MyRoomSlot::Player(player)
                        if player.user_no == topology_player.user_no
                            && canonical_nickname_key(&player.nickname)
                                == canonical_nickname_key(&topology_player.nickname) =>
                    {
                        Some(MyRoomSlot::Player(player.clone()))
                    }
                    MyRoomSlot::Empty | MyRoomSlot::Player(_) => None,
                })
                .expect("post-command topology contains only projected live identities"),
        })
    }

    fn room_participant() -> RoomParticipant {
        RoomParticipant {
            player: RoomPlayer {
                player_type: 2,
                user_no: 1,
                p2p_address: Ipv4Addr::LOCALHOST,
                p2p_port: 39_312,
                nickname: "untrusted".to_owned(),
                emblem_1: 0,
                emblem_2: 0,
                rider_item_snapshot: [0; RIDER_ITEM_SNAPSHOT_WIRE_LENGTH],
                card: String::new(),
                rp: 0,
                team: 0,
                ranking: 0,
                rider_school_level: 0,
                club_name: String::new(),
                club_mark_logo: 0,
            },
            observer: false,
            kart_physics: P5136KartPhysicsBlock::from([0; 235]),
        }
    }

    fn create_request(name: &str, game_type: u8) -> ChCreateRoomRequest {
        ChCreateRoomRequest {
            room_name: name.to_owned(),
            password: String::new(),
            game_type,
            reserved_after_game_type: 0,
            ai_count: 0,
            room_data_header: 0,
            room_data: [0; ROOM_DATA_LENGTH],
            connection_context: [0; ROOM_CONNECTION_CONTEXT_LENGTH],
            reserved_before_ai_switch: 0,
            ai_switch: 0,
            reserved_after_ai_switch_1: 0,
            reserved_after_ai_switch_2: 0,
            reserved_tail: 0,
            reserved_last: 0,
        }
    }

    fn join_request(room_id: RoomId) -> ChJoinRoomRequest {
        ChJoinRoomRequest {
            room_id: u16::try_from(room_id.0).unwrap(),
            password: String::new(),
            reserved: 0,
            connection_context: [0; ROOM_CONNECTION_CONTEXT_LENGTH],
        }
    }

    fn create_protocol_room(
        world: &mut World,
        owner: &TestChannelSession,
        game_type: u8,
    ) -> RoomId {
        world
            .room_protocol(
                owner.session,
                RoomCommandPayload::Create {
                    request: create_request("Race", game_type),
                    participant: room_participant(),
                },
            )
            .unwrap();
        world.protocol_room_by_user[&owner.identity.user_no]
    }

    fn join_protocol_room(
        world: &mut World,
        session: &TestChannelSession,
        room_id: RoomId,
        observer: bool,
    ) {
        let mut participant = room_participant();
        participant.observer = observer;
        world
            .room_protocol(
                session.session,
                RoomCommandPayload::Join {
                    request: join_request(room_id),
                    participant,
                },
            )
            .unwrap();
    }

    fn drain_batches(receiver: &mut mpsc::Receiver<OutboundBatch>) {
        while receiver.try_recv().is_ok() {}
    }

    fn logical_packet_hash(packet: &[u8]) -> u32 {
        u32::from_le_bytes(packet[..4].try_into().unwrap())
    }

    fn take_single_packet(receiver: &mut mpsc::Receiver<OutboundBatch>) -> Vec<u8> {
        let packets = receiver.try_recv().unwrap().into_packets();
        assert_eq!(packets.len(), 1);
        packets.into_iter().next().unwrap()
    }

    fn room_list_count(packet: &[u8]) -> i32 {
        assert_eq!(
            u32::from_le_bytes(packet[..4].try_into().unwrap()),
            adler32::packet_hash("ChGetRoomListReplyPacket")
        );
        i32::from_le_bytes(packet[8..12].try_into().unwrap())
    }

    fn game_slot_request(identity: &IdentityBinding, source_port: u16) -> UdpDispatchRequest {
        UdpDispatchRequest {
            ingress: UdpIngress {
                arrival_epoch: 0,
                transport: UdpTransport::Game,
                source: SocketAddr::new(identity.source_ip, source_port),
                iv: 0x5136_5136,
                account_id: identity.user_no.get(),
                route_hash: 0x1234_5678,
                body: UdpIngressBody::GameSlotPacket(Vec::new()),
            },
            identity: identity.clone(),
            racing_targets: Vec::new(),
            room_targets: Vec::new(),
        }
    }

    fn game_control_request(state: i32) -> RaceCommandPayload {
        game_control_request_with_value(state, 0)
    }

    fn game_control_request_with_value(state: i32, value0: u32) -> RaceCommandPayload {
        RaceCommandPayload::GameControl(GameControlRequest {
            state,
            optional_pair: None,
            value0,
            trailing: Vec::new(),
        })
    }

    fn ai_goal_in_request(player_id: i32, race_time: u32) -> RaceCommandPayload {
        RaceCommandPayload::AiGoalIn(AiGoalInRequest {
            player_id,
            race_time,
        })
    }

    fn booster_request(team: RaceTeam, contribution: f32) -> RaceCommandPayload {
        RaceCommandPayload::TeamBoosterGauge(TeamBoosterGaugeRequest { team, contribution })
    }

    fn force_running(world: &mut World, room_id: RoomId) {
        let room = world.protocol_rooms.get_mut(&room_id).unwrap();
        assert!(room.frozen_race.is_some());
        room.phase = RoomPhase::Running;
        room.loading_handshake = LoadingHandshake::Dormant;
        room.race_progress = super::RaceProgress::default();
    }

    fn prepare_single_reward_persistence(
        nickname: &str,
        port: u16,
    ) -> (World, TestChannelSession, RoomId, Instant, ServerClock) {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, nickname, 67, port, 64);
        let room_id = create_protocol_room(&mut world, &owner, 1);
        drain_batches(&mut owner.outbound);
        world
            .lobby_command(
                owner.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new())),
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        force_running(&mut world, room_id);
        let now = Instant::now();
        let clock = ServerClock::new();
        world
            .race_command_with_clock(
                owner.session,
                game_control_request_with_value(2, 456),
                now,
                &clock,
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        let deadline = world.protocol_rooms[&room_id]
            .race_progress
            .settlement
            .as_ref()
            .unwrap()
            .deadline;
        world.advance_loading(deadline, &clock);
        (world, owner, room_id, deadline, clock)
    }

    fn add_cancellable_session(world: &mut World, port: u16) -> oneshot::Receiver<()> {
        let (cancellation, cancelled) = oneshot::channel();
        world
            .register_session(
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
                Some(cancellation),
                None,
            )
            .unwrap();
        cancelled
    }

    fn complete_all_due_rewards(
        world: &mut World,
        now: Instant,
    ) -> Vec<super::RewardSettlementTask> {
        let tasks = world
            .take_due_reward_tasks(now, super::ROOM_CAPACITY)
            .unwrap();
        for (index, task) in tasks.iter().enumerate() {
            assert_eq!(
                world
                    .complete_reward_task(
                        durable_completion(task, 50_000 + u32::try_from(index).unwrap()),
                        now,
                    )
                    .unwrap(),
                super::RewardCompletionDisposition::Applied
            );
        }
        tasks
    }

    fn applied_reward(
        task: &super::RewardSettlementTask,
        current_lucci: u32,
    ) -> p5136_profile::AppliedTimeReward {
        p5136_profile::AppliedTimeReward {
            current_rp: p5136_profile::DEFAULT_RP,
            earned_rp: task.proposed_reward().earned_rp(),
            earned_lucci: task.proposed_reward().earned_lucci(),
            current_lucci,
        }
    }

    fn durable_completion(
        task: &super::RewardSettlementTask,
        current_lucci: u32,
    ) -> super::RewardPersistenceCompletion {
        durable_completion_with_key(task, task, current_lucci)
    }

    fn durable_completion_with_key(
        task: &super::RewardSettlementTask,
        key_task: &super::RewardSettlementTask,
        current_lucci: u32,
    ) -> super::RewardPersistenceCompletion {
        let root = tempfile::tempdir().unwrap();
        let store = p5136_profile::ProfileStore::new(root.path());
        store.load_or_create(key_task.nickname()).unwrap();
        let lease = store.acquire_race_run_lease().unwrap();
        let recipient = store
            .bind_race_reward_recipient(&lease, key_task.nickname(), key_task.user_no().get())
            .unwrap();
        let fence = key_task.fence();
        let key = p5136_profile::RaceRewardKey::new(
            &recipient,
            &lease,
            fence.room_id().0,
            fence.race_epoch(),
        )
        .unwrap();
        let receipt = crate::profile_io::DurableRewardReceipt::for_test(
            task.clone(),
            key,
            applied_reward(task, current_lucci),
        );
        super::RewardPersistenceCompletion::Durable(Box::new(receipt))
    }

    fn durable_completion_with_profile(
        task: &super::RewardSettlementTask,
        current_lucci: u32,
        presentation: MyRoomProfilePresentation,
    ) -> super::RewardPersistenceCompletion {
        let root = tempfile::tempdir().unwrap();
        let store = p5136_profile::ProfileStore::new(root.path());
        store.load_or_create(task.nickname()).unwrap();
        let lease = store.acquire_race_run_lease().unwrap();
        let recipient = store
            .bind_race_reward_recipient(&lease, task.nickname(), task.user_no().get())
            .unwrap();
        let fence = task.fence();
        let key = p5136_profile::RaceRewardKey::new(
            &recipient,
            &lease,
            fence.room_id().0,
            fence.race_epoch(),
        )
        .unwrap();
        let receipt = crate::profile_io::DurableRewardReceipt::for_test_with_profile(
            task.clone(),
            key,
            applied_reward(task, current_lucci),
            presentation,
            task.nickname(),
        );
        super::RewardPersistenceCompletion::Durable(Box::new(receipt))
    }

    fn set_result_admission(
        world: &mut World,
        room_id: RoomId,
        player_id: usize,
        character_id: u16,
        kart_id: u16,
        current_rp: u32,
        club_mark_logo: i32,
    ) {
        let player = &mut world
            .protocol_rooms
            .get_mut(&room_id)
            .unwrap()
            .members_by_id[player_id]
            .as_mut()
            .unwrap()
            .player;
        player.rider_item_snapshot[..2].copy_from_slice(&character_id.to_le_bytes());
        player.rider_item_snapshot[4..6].copy_from_slice(&kart_id.to_le_bytes());
        player.rp = current_rp;
        player.club_mark_logo = club_mark_logo;
    }

    fn take_packets(receiver: &mut mpsc::Receiver<OutboundBatch>) -> Vec<Vec<u8>> {
        receiver.try_recv().unwrap().into_packets()
    }

    fn udp_dispatch_outcome(
        action: UdpDispatchAction,
        sent_datagrams: usize,
    ) -> UdpDispatchOutcome {
        UdpDispatchOutcome {
            binding_status: UdpEndpointBindStatus::Bound,
            action,
            sent_datagrams,
            failed_sends: usize::from(sent_datagrams == 0),
            unavailable_targets: 0,
        }
    }

    fn take_race_start_tick(receiver: &mut mpsc::Receiver<OutboundBatch>) -> u32 {
        let packets = receiver.try_recv().unwrap().into_packets();
        assert_eq!(packets.len(), 2);
        assert_eq!(
            logical_packet_hash(&packets[0]),
            adler32::packet_hash(GAME_AI_MASTER_NOTICE_NAME)
        );
        assert_eq!(
            logical_packet_hash(&packets[1]),
            adler32::packet_hash(GAME_CONTROL_PACKET_NAME)
        );
        assert_eq!(i32::from_le_bytes(packets[1][4..8].try_into().unwrap()), 1);
        u32::from_le_bytes(packets[1][9..13].try_into().unwrap())
    }

    fn register_tracked_outbound_session(
        world: &mut World,
        port: u16,
        capacity: usize,
    ) -> (SessionId, WireOperationGate, mpsc::Receiver<OutboundBatch>) {
        let gate = WireOperationGate::new();
        let (outbound, receiver) = mpsc::channel(capacity);
        let session = world
            .register_session_with_operations(
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
                None,
                Some(outbound),
                Some(gate.clone()),
            )
            .unwrap();
        (session, gate, receiver)
    }

    #[test]
    fn reserved_outbound_lease_survives_queue_handoff_until_batch_retirement() {
        let mut world = World::default();
        let (session, gate, mut receiver) =
            register_tracked_outbound_session(&mut world, 39_001, 1);

        let reserved = world
            .reserve_outbound(vec![(session, OutboundBatch::single(vec![0xA1]))])
            .unwrap();
        assert_eq!(gate.active_counts().outbound, 1);
        World::publish_reserved(reserved);
        assert_eq!(gate.active_counts().outbound, 1);

        let batch = receiver.try_recv().unwrap();
        assert_eq!(gate.active_counts().outbound, 1);
        let (packets, operation) = batch.into_write_parts();
        assert_eq!(packets, vec![vec![0xA1]]);
        drop(packets);
        assert_eq!(gate.active_counts().outbound, 1);
        drop(operation);
        assert_eq!(gate.active_counts().outbound, 0);
    }

    #[test]
    fn closed_outbound_gate_fails_atomic_reservations_without_leaking_leases() {
        let mut world = World::default();
        let (first, first_gate, mut first_receiver) =
            register_tracked_outbound_session(&mut world, 39_011, 1);
        let (closed, closed_gate, mut closed_receiver) =
            register_tracked_outbound_session(&mut world, 39_012, 1);
        closed_gate.close_outbound_admission();

        assert!(matches!(
            world.reserve_outbound(vec![
                (first, OutboundBatch::single(vec![0xB1])),
                (closed, OutboundBatch::single(vec![0xB2])),
            ]),
            Err(LobbyError::OutboundUnavailable { session }) if session == closed
        ));
        assert_eq!(first_gate.active_counts().outbound, 0);
        assert_eq!(closed_gate.active_counts().outbound, 0);
        assert!(first_receiver.try_recv().is_err());
        assert!(closed_receiver.try_recv().is_err());

        assert_eq!(
            world
                .try_reserve_myroom_outbound(vec![(closed, OutboundBatch::single(vec![0xB3]),)])
                .unwrap_err()
                .session,
            closed
        );
        assert_eq!(closed_gate.active_counts().outbound, 0);
        assert!(closed_receiver.try_recv().is_err());
    }

    #[test]
    fn best_effort_delivery_tracks_open_batches_and_retires_closed_sessions() {
        let mut world = World::default();
        let (open, open_gate, mut open_receiver) =
            register_tracked_outbound_session(&mut world, 39_021, 1);
        let (closed, closed_gate, mut closed_receiver) =
            register_tracked_outbound_session(&mut world, 39_022, 1);
        closed_gate.close_outbound_admission();

        world
            .deliver(
                vec![
                    (open, OutboundBatch::single(vec![0xC1])),
                    (closed, OutboundBatch::single(vec![0xC2])),
                ],
                Instant::now(),
            )
            .unwrap();

        assert_eq!(open_gate.active_counts().outbound, 1);
        assert_eq!(closed_gate.active_counts().outbound, 0);
        assert!(!world.sessions.contains_key(&closed));
        assert!(closed_receiver.try_recv().is_err());
        drop(open_receiver.try_recv().unwrap());
        assert_eq!(open_gate.active_counts().outbound, 0);
    }

    #[test]
    fn racing_udp_targets_require_started_room_and_include_observers() {
        let mut world = World::default();
        let source = register_channel_session(&mut world, "Source", 67, 40_001, 64);
        let player = register_channel_session(&mut world, "Player", 67, 40_011, 64);
        let observer = register_channel_session(&mut world, "Observer", 67, 40_021, 64);
        let destination_owner = register_channel_session(&mut world, "Destination", 67, 40_031, 64);
        let destination_room = create_protocol_room(&mut world, &destination_owner, 1);

        world
            .room_protocol(
                source.session,
                RoomCommandPayload::Create {
                    request: create_request("Race", 1),
                    participant: room_participant(),
                },
            )
            .unwrap();
        let room_id = world.protocol_room_by_user[&source.identity.user_no];
        world
            .room_protocol(
                player.session,
                RoomCommandPayload::Join {
                    request: join_request(room_id),
                    participant: room_participant(),
                },
            )
            .unwrap();
        let mut observer_participant = room_participant();
        observer_participant.observer = true;
        world
            .room_protocol(
                observer.session,
                RoomCommandPayload::Join {
                    request: join_request(room_id),
                    participant: observer_participant,
                },
            )
            .unwrap();

        assert!(world.racing_udp_targets(source.identity.user_no).is_empty());
        world
            .lobby_command(
                player.session,
                LobbyCommandPayload::SetSlotState(PlayerSlotState::Ready),
            )
            .unwrap();
        world
            .lobby_command(
                source.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new())),
            )
            .unwrap();

        let targets = world.racing_udp_targets(source.identity.user_no);
        assert_eq!(targets.len(), 2);
        assert!(targets.contains(&player.identity));
        assert!(targets.contains(&observer.identity));
        assert!(!targets.contains(&source.identity));

        world
            .room_protocol(player.session, RoomCommandPayload::Leave)
            .unwrap();
        join_protocol_room(&mut world, &player, destination_room, false);
        let targets = world.racing_udp_targets(source.identity.user_no);
        assert_eq!(targets, vec![observer.identity]);
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn lobby_start_authority_readiness_epoch_and_packet_order_are_atomic() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "Owner", 67, 41_001, 64);
        let mut guest = register_channel_session(&mut world, "Guest", 67, 41_011, 64);
        let room_id = create_protocol_room(&mut world, &owner, 1);
        join_protocol_room(&mut world, &guest, room_id, false);
        drain_batches(&mut owner.outbound);
        drain_batches(&mut guest.outbound);

        let before = world.protocol_rooms[&room_id].clone();
        assert!(matches!(
            world.lobby_command(
                guest.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new()))
            ),
            Err(WorldError::Lobby(LobbyError::NotRoomMaster))
        ));
        assert_eq!(world.protocol_rooms[&room_id], before);
        let rejected = guest.outbound.try_recv().unwrap().into_packets();
        assert_eq!(rejected.len(), 1);
        assert_eq!(
            logical_packet_hash(&rejected[0]),
            adler32::packet_hash(START_ROOM_REPLY_NAME)
        );
        assert_eq!(i32::from_le_bytes(rejected[0][4..8].try_into().unwrap()), 2);

        let before = world.protocol_rooms[&room_id].clone();
        assert!(matches!(
            world.lobby_command(
                owner.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(
                    vec![0x1111_2222],
                    Vec::new()
                ))
            ),
            Err(WorldError::Lobby(LobbyError::RacerNotReady { user_no }))
                if user_no == guest.identity.user_no.get()
        ));
        assert_eq!(world.protocol_rooms[&room_id], before);
        drain_batches(&mut owner.outbound);

        assert!(matches!(
            world
                .lobby_command(
                    guest.session,
                    LobbyCommandPayload::SetSlotState(PlayerSlotState::Ready)
                )
                .unwrap(),
            LobbyCommandOutcome::SlotStateChanged {
                room_id: changed_room,
                state: PlayerSlotState::Ready,
                ..
            } if changed_room == room_id
        ));
        let guest_ready = guest.outbound.try_recv().unwrap().into_packets();
        assert_eq!(guest_ready.len(), 3);
        assert_eq!(
            logical_packet_hash(&guest_ready[0]),
            adler32::packet_hash(SLOT_STATE_PACKET_NAME)
        );
        assert_eq!(
            logical_packet_hash(&guest_ready[1]),
            adler32::packet_hash(SET_SLOT_STATE_REPLY_NAME)
        );
        assert_eq!(
            logical_packet_hash(&guest_ready[2]),
            adler32::packet_hash("GrSlotDataPacket")
        );
        drain_batches(&mut owner.outbound);

        let started = world
            .lobby_command(
                owner.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new())),
            )
            .unwrap();
        assert!(matches!(
            started,
            LobbyCommandOutcome::Started {
                room_id: started_room,
                race_epoch: 1,
                concrete_track: 0x1111_2222,
                racer_count: 2,
                observer_count: 0,
            } if started_room == room_id
        ));
        let owner_start = owner.outbound.try_recv().unwrap().into_packets();
        assert_eq!(owner_start.len(), 2);
        assert_eq!(
            logical_packet_hash(&owner_start[0]),
            adler32::packet_hash(START_ROOM_REPLY_NAME)
        );
        assert_eq!(
            logical_packet_hash(&owner_start[1]),
            adler32::packet_hash(GR_COMMAND_START_PACKET_NAME)
        );
        let guest_start = guest.outbound.try_recv().unwrap().into_packets();
        assert_eq!(guest_start.len(), 1);
        assert_eq!(
            logical_packet_hash(&guest_start[0]),
            adler32::packet_hash(GR_COMMAND_START_PACKET_NAME)
        );

        let committed = world.protocol_rooms[&room_id].clone();
        assert_eq!(committed.phase, RoomPhase::Loading);
        assert_eq!(
            committed.race_fence.map(|fence| fence.race_epoch.get()),
            Some(1)
        );
        assert!(committed.frozen_race.is_some());
        assert!(matches!(
            world.lobby_command(
                owner.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x3333_4444], Vec::new()))
            ),
            Err(WorldError::Lobby(LobbyError::NotLobby {
                actual: RoomPhase::Loading
            }))
        ));
        assert_eq!(world.protocol_rooms[&room_id], committed);
        let duplicate = owner.outbound.try_recv().unwrap().into_packets();
        assert_eq!(duplicate.len(), 1);
        assert_eq!(
            logical_packet_hash(&duplicate[0]),
            adler32::packet_hash(START_ROOM_REPLY_NAME)
        );
    }

    #[test]
    fn loading_game_control_arms_once_with_exact_frozen_participants() {
        let mut world = World::default();
        let owner = register_channel_session(&mut world, "ControlOwner", 67, 41_201, 64);
        let guest = register_channel_session(&mut world, "ControlGuest", 67, 41_211, 64);
        let observer = register_channel_session(&mut world, "ControlObserver", 67, 41_221, 64);
        let room_id = create_protocol_room(&mut world, &owner, 1);
        join_protocol_room(&mut world, &guest, room_id, false);
        join_protocol_room(&mut world, &observer, room_id, true);

        let wrong_phase = world
            .race_command(owner.session, game_control_request(0), Instant::now())
            .unwrap_err();
        assert!(matches!(
            &wrong_phase,
            WorldError::Race(RaceError::WrongPhase {
                actual: RoomPhase::Lobby
            })
        ));
        assert!(match &wrong_phase {
            WorldError::Race(error) => error.is_expected_rejection(),
            _ => false,
        });
        assert!(matches!(
            world.race_command(
                owner.session,
                RaceCommandPayload::AiGoalIn(AiGoalInRequest {
                    player_id: 0,
                    race_time: 0,
                }),
                Instant::now()
            ),
            Err(WorldError::Race(RaceError::NotRunning {
                actual: RoomPhase::Lobby
            }))
        ));

        world
            .lobby_command(
                guest.session,
                LobbyCommandPayload::SetSlotState(PlayerSlotState::Ready),
            )
            .unwrap();
        world
            .lobby_command(
                owner.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new())),
            )
            .unwrap();

        let armed_at = Instant::now();
        assert_eq!(
            world
                .race_command(owner.session, game_control_request(0), armed_at)
                .unwrap(),
            RaceCommandOutcome::LoadingAwaiting {
                room_id,
                race_epoch: 1,
                expected_participants: 3,
            }
        );
        let armed = world.protocol_rooms[&room_id].loading_handshake.clone();
        match &armed {
            LoadingHandshake::Awaiting {
                expected,
                ready,
                deadline,
            } => {
                assert_eq!(expected.len(), 3);
                assert!(ready.is_empty());
                assert_eq!(*deadline, armed_at + Duration::from_secs(30));
            }
            LoadingHandshake::Dormant | LoadingHandshake::StartScheduled { .. } => {
                panic!("state=0 did not arm loading")
            }
        }

        assert_eq!(
            world
                .race_command(
                    observer.session,
                    game_control_request(0),
                    armed_at + Duration::from_secs(10)
                )
                .unwrap(),
            RaceCommandOutcome::IgnoredDuplicate {
                room_id,
                race_epoch: 1,
            }
        );
        assert_eq!(world.protocol_rooms[&room_id].loading_handshake, armed);
        assert!(matches!(
            world.race_command(guest.session, game_control_request(2), armed_at),
            Err(WorldError::Race(RaceError::NotRunning {
                actual: RoomPhase::Loading
            }))
        ));
        assert_eq!(world.protocol_rooms[&room_id].loading_handshake, armed);
        assert!(!RaceError::RaceDeadlineOverflow.is_expected_rejection());
    }

    #[test]
    fn loading_game_control_rejects_stale_and_replacement_generations() {
        let mut world = World::default();
        let owner = register_channel_session(&mut world, "FenceOwner", 67, 41_301, 64);
        let guest = register_channel_session(&mut world, "FenceGuest", 67, 41_311, 64);
        let room_id = create_protocol_room(&mut world, &owner, 1);
        join_protocol_room(&mut world, &guest, room_id, false);
        world
            .lobby_command(
                guest.session,
                LobbyCommandPayload::SetSlotState(PlayerSlotState::Ready),
            )
            .unwrap();
        world
            .lobby_command(
                owner.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new())),
            )
            .unwrap();

        world
            .race_command(owner.session, game_control_request(0), Instant::now())
            .unwrap();
        let stale_candidate = world
            .loading_readiness_candidate(&guest.identity)
            .expect("the frozen guest is initially eligible");
        let replacement = migrate_channel_session(&mut world, &guest, 41_401, 64);
        assert!(matches!(
            world.race_command(replacement.session, game_control_request(0), Instant::now()),
            Err(WorldError::Race(RaceError::NotFrozenParticipant))
        ));
        assert!(matches!(
            world.race_command(guest.session, game_control_request(0), Instant::now()),
            Err(WorldError::Identity(IdentityError::StaleSession(session)))
                if session == guest.session
        ));
        assert!(
            world
                .loading_readiness_candidate(&replacement.identity)
                .is_none()
        );
        assert!(!world.apply_udp_dispatch_readiness(
            Some(stale_candidate),
            udp_dispatch_outcome(UdpDispatchAction::TimeSyncReply, 1)
        ));
        world.advance_loading(Instant::now(), &ServerClock::new());
        let LoadingHandshake::Awaiting {
            expected, ready, ..
        } = &world.protocol_rooms[&room_id].loading_handshake
        else {
            panic!("the remaining exact frozen owner should keep waiting");
        };
        assert_eq!(expected.len(), 1);
        assert!(expected.contains(&super::FrozenParticipantStamp::from(&owner.identity)));
        assert!(ready.is_empty());
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn loading_udp_gate_observer_start_wrap_and_queue_retry_are_atomic() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "ReadyOwner", 67, 41_501, 64);
        let mut guest = register_channel_session(&mut world, "ReadyGuest", 67, 41_511, 1);
        let mut observer = register_channel_session(&mut world, "ReadyObserver", 67, 41_521, 64);
        let room_id = create_protocol_room(&mut world, &owner, 1);
        join_protocol_room(&mut world, &guest, room_id, false);
        join_protocol_room(&mut world, &observer, room_id, true);
        drain_batches(&mut owner.outbound);
        drain_batches(&mut guest.outbound);
        drain_batches(&mut observer.outbound);
        world
            .lobby_command(
                guest.session,
                LobbyCommandPayload::SetSlotState(PlayerSlotState::Ready),
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        drain_batches(&mut guest.outbound);
        drain_batches(&mut observer.outbound);
        world
            .lobby_command(
                owner.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new())),
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        drain_batches(&mut guest.outbound);
        drain_batches(&mut observer.outbound);

        let armed_at = Instant::now();
        world
            .race_command(owner.session, game_control_request(0), armed_at)
            .unwrap();
        let owner_candidate = world.loading_readiness_candidate(&owner.identity).unwrap();
        for action in [
            UdpDispatchAction::EchoReply,
            UdpDispatchAction::GameSlotRelay,
            UdpDispatchAction::RoomSlotRelay,
            UdpDispatchAction::ClientReplyDropped,
        ] {
            assert!(!world.apply_udp_dispatch_readiness(
                Some(owner_candidate),
                udp_dispatch_outcome(action, 1)
            ));
        }
        assert!(!world.apply_udp_dispatch_readiness(
            Some(owner_candidate),
            udp_dispatch_outcome(UdpDispatchAction::TimeSyncReply, 0)
        ));
        assert!(!world.apply_udp_dispatch_readiness(
            Some(owner_candidate),
            udp_dispatch_outcome(UdpDispatchAction::TimeSyncReply, 2)
        ));
        assert!(!world.apply_udp_dispatch_readiness(
            None,
            udp_dispatch_outcome(UdpDispatchAction::TimeSyncReply, 1)
        ));
        assert!(world.apply_udp_dispatch_readiness(
            Some(owner_candidate),
            udp_dispatch_outcome(UdpDispatchAction::TimeSyncReply, 1)
        ));
        assert!(!world.apply_udp_dispatch_readiness(
            Some(owner_candidate),
            udp_dispatch_outcome(UdpDispatchAction::TimeSyncReply, 1)
        ));

        for identity in [&guest.identity, &observer.identity] {
            let candidate = world.loading_readiness_candidate(identity).unwrap();
            assert!(world.apply_udp_dispatch_readiness(
                Some(candidate),
                udp_dispatch_outcome(UdpDispatchAction::TimeSyncReply, 1)
            ));
        }

        assert_eq!(super::race_start_tick(u32::MAX - 1_000), 1_999);
        let clock = ServerClock::new();
        let shared_clock = clock.clone();
        world.advance_loading(armed_at, &clock);
        assert_eq!(
            world.protocol_rooms[&room_id].loading_handshake,
            LoadingHandshake::StartScheduled {
                at: armed_at + Duration::from_secs(1)
            }
        );
        assert!(LOADING_HEARTBEAT_INTERVAL <= Duration::from_millis(100));

        let guest_sender = world.sessions[&guest.session].outbound.clone().unwrap();
        guest_sender
            .try_send(OutboundBatch::single(vec![0xAA]))
            .unwrap();
        world.advance_loading(armed_at + Duration::from_secs(1), &clock);
        assert_eq!(world.protocol_rooms[&room_id].phase, RoomPhase::Loading);
        assert!(matches!(
            world.protocol_rooms[&room_id].loading_handshake,
            LoadingHandshake::StartScheduled { .. }
        ));
        assert!(owner.outbound.try_recv().is_err());
        assert!(observer.outbound.try_recv().is_err());
        assert_eq!(
            guest.outbound.try_recv().unwrap().into_packets(),
            vec![vec![0xAA]]
        );

        world.advance_loading(
            armed_at + Duration::from_secs(1) + Duration::from_millis(1),
            &clock,
        );
        assert_eq!(world.protocol_rooms[&room_id].phase, RoomPhase::Running);
        assert!(world.protocol_rooms[&room_id].frozen_race.is_some());
        let owner_tick = take_race_start_tick(&mut owner.outbound);
        assert_eq!(take_race_start_tick(&mut guest.outbound), owner_tick);
        assert_eq!(take_race_start_tick(&mut observer.outbound), owner_tick);
        assert!(owner_tick < 10_000);
        let current_shared_tick = shared_clock.tick().wrapping_add(3_000);
        assert!(
            current_shared_tick.wrapping_sub(owner_tick) < 1_000,
            "UDP and TCP race control must use the same clock epoch"
        );
    }

    #[test]
    fn loading_timeout_schedules_then_starts_one_second_later() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "TimeoutOwner", 67, 41_601, 64);
        let mut guest = register_channel_session(&mut world, "TimeoutGuest", 67, 41_611, 64);
        let room_id = create_protocol_room(&mut world, &owner, 1);
        join_protocol_room(&mut world, &guest, room_id, false);
        world
            .lobby_command(
                guest.session,
                LobbyCommandPayload::SetSlotState(PlayerSlotState::Ready),
            )
            .unwrap();
        world
            .lobby_command(
                owner.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new())),
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        drain_batches(&mut guest.outbound);

        let armed_at = Instant::now();
        world
            .race_command(owner.session, game_control_request(0), armed_at)
            .unwrap();
        let deadline = armed_at + Duration::from_secs(30);
        let clock = ServerClock::new();
        world.advance_loading(deadline, &clock);
        assert_eq!(
            world.protocol_rooms[&room_id].loading_handshake,
            LoadingHandshake::StartScheduled {
                at: deadline + Duration::from_secs(1)
            }
        );
        world.advance_loading(deadline + Duration::from_millis(999), &clock);
        assert_eq!(world.protocol_rooms[&room_id].phase, RoomPhase::Loading);
        world.advance_loading(deadline + Duration::from_secs(1), &clock);
        assert_eq!(world.protocol_rooms[&room_id].phase, RoomPhase::Running);
        assert_eq!(
            take_race_start_tick(&mut owner.outbound),
            take_race_start_tick(&mut guest.outbound)
        );
    }

    #[test]
    fn loading_disconnect_prunes_expected_and_zero_humans_abort_to_lobby() {
        let mut world = World::default();
        let owner = register_channel_session(&mut world, "DropOwner", 67, 41_701, 64);
        let guest = register_channel_session(&mut world, "DropGuest", 67, 41_711, 64);
        let observer = register_channel_session(&mut world, "DropObserver", 67, 41_721, 64);
        let room_id = create_protocol_room(&mut world, &owner, 1);
        join_protocol_room(&mut world, &guest, room_id, false);
        join_protocol_room(&mut world, &observer, room_id, true);
        world
            .lobby_command(
                guest.session,
                LobbyCommandPayload::SetSlotState(PlayerSlotState::Ready),
            )
            .unwrap();
        world
            .lobby_command(
                owner.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new())),
            )
            .unwrap();
        let frozen = world.protocol_rooms[&room_id].frozen_race.clone();
        let now = Instant::now();
        world
            .race_command(owner.session, game_control_request(0), now)
            .unwrap();

        world.close_session(guest.session, now).unwrap();
        world.advance_loading(now, &ServerClock::new());
        let room = &world.protocol_rooms[&room_id];
        assert_eq!(room.frozen_race, frozen);
        let LoadingHandshake::Awaiting {
            expected, ready, ..
        } = &room.loading_handshake
        else {
            panic!("remaining human and observer must continue loading");
        };
        assert_eq!(expected.len(), 2);
        assert!(expected.contains(&super::FrozenParticipantStamp::from(&owner.identity)));
        assert!(expected.contains(&super::FrozenParticipantStamp::from(&observer.identity)));
        assert!(ready.is_empty());

        world.close_session(owner.session, now).unwrap();
        world.advance_loading(now, &ServerClock::new());
        let room = &world.protocol_rooms[&room_id];
        assert_eq!(room.phase, RoomPhase::Lobby);
        assert!(room.frozen_race.is_none());
        assert_eq!(room.loading_handshake, LoadingHandshake::Dormant);
        assert_eq!(room.observers.iter().flatten().count(), 1);
        assert_eq!(room.members_by_id.iter().flatten().count(), 0);
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn running_finish_is_exact_atomic_idempotent_and_starts_settlement() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "FinishOwner", 67, 41_801, 64);
        let mut guest = register_channel_session(&mut world, "FinishGuest", 67, 41_811, 1);
        let mut observer = register_channel_session(&mut world, "FinishObserver", 67, 41_821, 64);
        let room_id = create_protocol_room(&mut world, &owner, 1);
        join_protocol_room(&mut world, &guest, room_id, false);
        join_protocol_room(&mut world, &observer, room_id, true);
        set_result_admission(&mut world, room_id, 0, 101, 201, 301, 401);
        set_result_admission(&mut world, room_id, 1, 102, 202, 302, 402);
        drain_batches(&mut owner.outbound);
        drain_batches(&mut guest.outbound);
        drain_batches(&mut observer.outbound);
        world
            .lobby_command(
                guest.session,
                LobbyCommandPayload::SetSlotState(PlayerSlotState::Ready),
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        drain_batches(&mut guest.outbound);
        drain_batches(&mut observer.outbound);
        world
            .lobby_command(
                owner.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new())),
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        drain_batches(&mut guest.outbound);
        drain_batches(&mut observer.outbound);
        force_running(&mut world, room_id);

        assert!(matches!(
            world.race_command(
                owner.session,
                booster_request(RaceTeam::Red, 1.0),
                Instant::now(),
            ),
            Err(WorldError::Race(RaceError::TeamModeRequired))
        ));
        let guest_sender = world.sessions[&guest.session].outbound.clone().unwrap();
        guest_sender
            .try_send(OutboundBatch::single(vec![0xBB]))
            .unwrap();
        let now = Instant::now();
        let clock = ServerClock::new();
        assert_eq!(
            world
                .race_command_with_clock(
                    owner.session,
                    game_control_request_with_value(2, 1_234),
                    now,
                    &clock,
                )
                .unwrap(),
            RaceCommandOutcome::FinishRecorded {
                room_id,
                race_epoch: 1,
                player_id: 0,
                began_settlement: true,
            }
        );
        let room = &world.protocol_rooms[&room_id];
        assert_eq!(room.phase, RoomPhase::Settling);
        assert_eq!(room.race_progress.finish_times.get(&0), Some(&1_234));
        assert_eq!(room.race_progress.pending_fanouts.len(), 1);
        let settlement = room.race_progress.settlement.as_ref().unwrap().clone();
        assert_eq!(settlement.deadline, now + Duration::from_secs(10));
        assert!(owner.outbound.try_recv().is_err());
        assert!(observer.outbound.try_recv().is_err());
        assert_eq!(
            guest.outbound.try_recv().unwrap().into_packets(),
            vec![vec![0xBB]]
        );

        world.advance_loading(now + Duration::from_millis(1), &clock);
        assert!(
            world.protocol_rooms[&room_id]
                .race_progress
                .pending_fanouts
                .is_empty()
        );

        let owner_packets = take_packets(&mut owner.outbound);
        assert_eq!(owner_packets.len(), 1);
        assert_eq!(
            logical_packet_hash(&owner_packets[0]),
            adler32::packet_hash(GAME_RACE_TIME_PACKET_NAME)
        );
        assert_eq!(
            i32::from_le_bytes(owner_packets[0][4..8].try_into().unwrap()),
            0
        );
        assert_eq!(
            u32::from_le_bytes(owner_packets[0][8..12].try_into().unwrap()),
            1_234
        );
        for receiver in [&mut guest.outbound, &mut observer.outbound] {
            let packets = take_packets(receiver);
            assert_eq!(packets.len(), 2);
            assert_eq!(
                logical_packet_hash(&packets[0]),
                adler32::packet_hash(GAME_RACE_TIME_PACKET_NAME)
            );
            assert_eq!(
                logical_packet_hash(&packets[1]),
                adler32::packet_hash(GAME_CONTROL_PACKET_NAME)
            );
            assert_eq!(i32::from_le_bytes(packets[1][4..8].try_into().unwrap()), 3);
            assert_eq!(
                u32::from_le_bytes(packets[1][9..13].try_into().unwrap()),
                settlement.end_tick
            );
        }

        assert_eq!(
            world
                .race_command_with_clock(
                    owner.session,
                    game_control_request_with_value(2, 9_999),
                    now,
                    &clock,
                )
                .unwrap(),
            RaceCommandOutcome::IgnoredDuplicate {
                room_id,
                race_epoch: 1
            }
        );
        assert!(owner.outbound.try_recv().is_err());
        assert!(guest.outbound.try_recv().is_err());
        assert!(observer.outbound.try_recv().is_err());

        assert!(matches!(
            world
                .race_command_with_clock(
                    guest.session,
                    game_control_request_with_value(2, 1_000),
                    now,
                    &clock,
                )
                .unwrap(),
            RaceCommandOutcome::FinishRecorded {
                player_id: 1,
                began_settlement: false,
                ..
            }
        ));
        for receiver in [
            &mut owner.outbound,
            &mut guest.outbound,
            &mut observer.outbound,
        ] {
            let packets = take_packets(receiver);
            assert_eq!(packets.len(), 1);
            assert_eq!(
                logical_packet_hash(&packets[0]),
                adler32::packet_hash(GAME_RACE_TIME_PACKET_NAME)
            );
        }

        assert!(matches!(
            world.race_command_with_clock(
                observer.session,
                game_control_request_with_value(2, 500),
                now,
                &clock,
            ),
            Err(WorldError::Race(RaceError::HumanRacerRequired))
        ));
        assert!(matches!(
            world.race_command_with_clock(owner.session, game_control_request(0), now, &clock,),
            Err(WorldError::Race(RaceError::WrongPhase {
                actual: RoomPhase::Settling
            }))
        ));
        assert!(matches!(
            world.race_command_with_clock(owner.session, ai_goal_in_request(7, 500), now, &clock,),
            Err(WorldError::Race(RaceError::NoFrozenAiParticipant {
                player_id: 7
            }))
        ));
        assert!(matches!(
            world.race_command_with_clock(owner.session, game_control_request(7), now, &clock,),
            Err(WorldError::Race(RaceError::UnsupportedGameControlState {
                state: 7
            }))
        ));
    }

    #[test]
    fn pending_finish_recomputes_current_room_audience_after_move() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "PendingOwner", 67, 41_851, 64);
        let mut mover = register_channel_session(&mut world, "PendingMover", 67, 41_861, 1);
        let mut destination =
            register_channel_session(&mut world, "PendingDestination", 67, 41_871, 64);
        let room_id = create_protocol_room(&mut world, &owner, 1);
        let destination_room = create_protocol_room(&mut world, &destination, 1);
        join_protocol_room(&mut world, &mover, room_id, false);
        drain_batches(&mut owner.outbound);
        drain_batches(&mut mover.outbound);
        drain_batches(&mut destination.outbound);
        world
            .lobby_command(
                mover.session,
                LobbyCommandPayload::SetSlotState(PlayerSlotState::Ready),
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        drain_batches(&mut mover.outbound);
        world
            .lobby_command(
                owner.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new())),
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        drain_batches(&mut mover.outbound);
        force_running(&mut world, room_id);

        let mover_sender = world.sessions[&mover.session].outbound.clone().unwrap();
        mover_sender
            .try_send(OutboundBatch::single(vec![0xE1]))
            .unwrap();
        let now = Instant::now();
        let clock = ServerClock::new();
        assert!(matches!(
            world
                .race_command_with_clock(
                    owner.session,
                    game_control_request_with_value(2, 321),
                    now,
                    &clock,
                )
                .unwrap(),
            RaceCommandOutcome::FinishRecorded {
                began_settlement: true,
                ..
            }
        ));
        assert_eq!(
            mover.outbound.try_recv().unwrap().into_packets(),
            vec![vec![0xE1]]
        );
        assert!(owner.outbound.try_recv().is_err());

        world
            .room_protocol(mover.session, RoomCommandPayload::Leave)
            .unwrap();
        drain_batches(&mut owner.outbound);
        drain_batches(&mut mover.outbound);
        join_protocol_room(&mut world, &mover, destination_room, false);
        drain_batches(&mut mover.outbound);
        drain_batches(&mut destination.outbound);
        mover_sender
            .try_send(OutboundBatch::single(vec![0xE2]))
            .unwrap();

        world.advance_loading(now + Duration::from_millis(1), &clock);
        let packets = take_packets(&mut owner.outbound);
        assert_eq!(packets.len(), 1);
        assert_eq!(
            logical_packet_hash(&packets[0]),
            adler32::packet_hash(GAME_RACE_TIME_PACKET_NAME)
        );
        assert_eq!(
            mover.outbound.try_recv().unwrap().into_packets(),
            vec![vec![0xE2]]
        );
        assert!(mover.outbound.try_recv().is_err());
        assert!(
            world.protocol_rooms[&room_id]
                .race_progress
                .pending_fanouts
                .is_empty()
        );
    }

    #[test]
    fn settlement_never_overtakes_a_pending_finish_fanout() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "OrderOwner", 67, 41_881, 64);
        let mut guest = register_channel_session(&mut world, "OrderGuest", 67, 41_891, 1);
        let room_id = create_protocol_room(&mut world, &owner, 1);
        join_protocol_room(&mut world, &guest, room_id, false);
        drain_batches(&mut owner.outbound);
        drain_batches(&mut guest.outbound);
        world
            .lobby_command(
                guest.session,
                LobbyCommandPayload::SetSlotState(PlayerSlotState::Ready),
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        drain_batches(&mut guest.outbound);
        world
            .lobby_command(
                owner.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new())),
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        drain_batches(&mut guest.outbound);
        force_running(&mut world, room_id);

        let guest_sender = world.sessions[&guest.session].outbound.clone().unwrap();
        guest_sender
            .try_send(OutboundBatch::single(vec![0xE4]))
            .unwrap();
        let now = Instant::now();
        let clock = ServerClock::new();
        world
            .race_command_with_clock(
                owner.session,
                game_control_request_with_value(2, 777),
                now,
                &clock,
            )
            .unwrap();
        let deadline = world.protocol_rooms[&room_id]
            .race_progress
            .settlement
            .as_ref()
            .unwrap()
            .deadline;
        world.advance_loading(deadline, &clock);
        assert!(owner.outbound.try_recv().is_err());
        assert_eq!(
            guest.outbound.try_recv().unwrap().into_packets(),
            vec![vec![0xE4]]
        );

        world.advance_loading(deadline + Duration::from_millis(1), &clock);
        let owner_finish = take_packets(&mut owner.outbound);
        assert_eq!(owner_finish.len(), 1);
        assert_eq!(
            logical_packet_hash(&owner_finish[0]),
            adler32::packet_hash(GAME_RACE_TIME_PACKET_NAME)
        );
        let guest_finish = take_packets(&mut guest.outbound);
        assert_eq!(guest_finish.len(), 2);
        assert_eq!(
            logical_packet_hash(&guest_finish[0]),
            adler32::packet_hash(GAME_RACE_TIME_PACKET_NAME)
        );
        assert_eq!(
            logical_packet_hash(&guest_finish[1]),
            adler32::packet_hash(GAME_CONTROL_PACKET_NAME)
        );
        assert_eq!(world.protocol_rooms[&room_id].phase, RoomPhase::Settling);
        let reward_tasks =
            complete_all_due_rewards(&mut world, deadline + Duration::from_millis(1));
        assert_eq!(reward_tasks.len(), 2);

        world.advance_loading(deadline + Duration::from_millis(2), &clock);
        for receiver in [&mut owner.outbound, &mut guest.outbound] {
            let packets = take_packets(receiver);
            assert_eq!(packets.len(), 3);
            assert_eq!(
                logical_packet_hash(&packets[0]),
                adler32::packet_hash(GAME_NEXT_STAGE_PACKET_NAME)
            );
            assert_eq!(
                logical_packet_hash(&packets[1]),
                adler32::packet_hash(GAME_RESULT_PACKET_NAME)
            );
            assert_eq!(
                logical_packet_hash(&packets[2]),
                adler32::packet_hash(GAME_CONTROL_PACKET_NAME)
            );
        }
        assert_eq!(world.protocol_rooms[&room_id].phase, RoomPhase::Lobby);
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn reward_sampling_is_once_retry_stable_and_one_slow_receipt_gates_final() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "RewardOwner", 67, 41_900, 64);
        let mut guest = register_channel_session(&mut world, "RewardGuest", 67, 41_901, 64);
        let room_id = create_protocol_room(&mut world, &owner, 1);
        join_protocol_room(&mut world, &guest, room_id, false);
        drain_batches(&mut owner.outbound);
        drain_batches(&mut guest.outbound);
        world
            .lobby_command(
                guest.session,
                LobbyCommandPayload::SetSlotState(PlayerSlotState::Ready),
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        drain_batches(&mut guest.outbound);
        world
            .lobby_command(
                owner.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new())),
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        drain_batches(&mut guest.outbound);
        force_running(&mut world, room_id);

        let now = Instant::now();
        let clock = ServerClock::new();
        world
            .race_command_with_clock(
                owner.session,
                game_control_request_with_value(2, 123),
                now,
                &clock,
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        drain_batches(&mut guest.outbound);
        let deadline = world.protocol_rooms[&room_id]
            .race_progress
            .settlement
            .as_ref()
            .unwrap()
            .deadline;
        let mut rolls = CountingRewardRolls {
            rp: 7,
            lucci: 11,
            rp_draws: 0,
            lucci_draws: 0,
        };
        world.advance_loading_with_reward_source(deadline, &clock, &mut rolls);
        assert_eq!((rolls.rp_draws, rolls.lucci_draws), (2, 2));
        world.advance_loading_with_reward_source(deadline, &clock, &mut rolls);
        assert_eq!(
            (rolls.rp_draws, rolls.lucci_draws),
            (2, 2),
            "a heartbeat must not resample frozen rewards"
        );

        let first = world
            .take_due_reward_tasks(deadline, 1)
            .unwrap()
            .pop()
            .unwrap();
        let first_attempt = first.attempt_id;
        let first_proposal = first.proposed_reward;
        assert_eq!(
            world
                .complete_reward_task(
                    super::RewardPersistenceCompletion::RetryableFailure(first.clone()),
                    deadline,
                )
                .unwrap(),
            super::RewardCompletionDisposition::RetryScheduled { failure_count: 1 }
        );

        let second = world
            .take_due_reward_tasks(deadline, 1)
            .unwrap()
            .pop()
            .unwrap();
        assert_ne!(second.user_no, first.user_no);
        let second_applied = applied_reward(&second, 60_000);
        assert_eq!(
            world
                .complete_reward_task(durable_completion(&second, 60_000), deadline,)
                .unwrap(),
            super::RewardCompletionDisposition::Applied
        );
        assert!(matches!(
            world.protocol_rooms[&room_id]
                .race_progress
                .settlement
                .as_ref()
                .unwrap()
                .finalization,
            super::SettlementFinalization::Persisting { .. }
        ));
        assert!(
            world
                .take_due_reward_tasks(deadline + Duration::from_millis(99), super::ROOM_CAPACITY,)
                .unwrap()
                .is_empty()
        );

        let retry = world
            .take_due_reward_tasks(deadline + Duration::from_millis(100), super::ROOM_CAPACITY)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(retry.user_no, first.user_no);
        assert_eq!(retry.proposed_reward, first_proposal);
        assert_ne!(retry.attempt_id, first_attempt);
        assert_eq!(
            world
                .complete_reward_task(
                    super::RewardPersistenceCompletion::FatalFailure(retry.clone()),
                    deadline + Duration::from_millis(100),
                )
                .unwrap(),
            super::RewardCompletionDisposition::TerminalFailure
        );
        let status = world.reward_drain_status().unwrap();
        assert_eq!(status.outstanding_lanes().len(), 2);
        assert_eq!(status.terminal_count(), 1);
        let dead_letter = status.dead_letters()[0].clone();
        let reset_at = deadline + Duration::from_millis(101);
        world
            .retry_reward_dead_letter(dead_letter, reset_at)
            .unwrap();
        let settlement = world.protocol_rooms[&room_id]
            .race_progress
            .settlement
            .as_ref()
            .unwrap();
        let super::SettlementFinalization::Persisting { rewards, .. } = &settlement.finalization
        else {
            panic!("dead-letter reset must restore persistence");
        };
        assert!(rewards.iter().any(|reward| {
            reward.user_no == second.user_no
                && reward.status == super::RewardPersistenceStatus::Durable(second_applied)
        }));
        assert!(rewards.iter().any(|reward| {
            reward.user_no == first.user_no
                && matches!(
                    reward.status,
                    super::RewardPersistenceStatus::Queued {
                        due_at,
                        failure_count: 0,
                    } if due_at == reset_at
                )
        }));
        assert_eq!(
            world
                .complete_reward_task(
                    super::RewardPersistenceCompletion::RetryableFailure(retry.clone()),
                    reset_at,
                )
                .unwrap(),
            super::RewardCompletionDisposition::IgnoredStale
        );
        let retry_after_reset = world
            .take_due_reward_tasks(reset_at, super::ROOM_CAPACITY)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(retry_after_reset.user_no, first.user_no);
        assert_eq!(retry_after_reset.proposed_reward, first_proposal);
        assert_ne!(retry_after_reset.attempt_id, retry.attempt_id);
        assert_eq!(
            world
                .complete_reward_task(durable_completion(&retry_after_reset, 60_001), reset_at,)
                .unwrap(),
            super::RewardCompletionDisposition::Applied
        );
        assert!(matches!(
            world.protocol_rooms[&room_id]
                .race_progress
                .settlement
                .as_ref()
                .unwrap()
                .finalization,
            super::SettlementFinalization::Ready { .. }
        ));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn running_team_booster_fences_team_and_reserves_observers_atomically() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "BoostOwner", 68, 41_901, 64);
        let mut ally = register_channel_session(&mut world, "BoostAlly", 68, 41_911, 1);
        let mut opponent = register_channel_session(&mut world, "BoostOpponent", 68, 41_921, 64);
        let mut observer = register_channel_session(&mut world, "BoostObserver", 68, 41_931, 64);
        let room_id = create_protocol_room(&mut world, &owner, 3);
        join_protocol_room(&mut world, &ally, room_id, false);
        join_protocol_room(&mut world, &opponent, room_id, false);
        join_protocol_room(&mut world, &observer, room_id, true);
        {
            let room = world.protocol_rooms.get_mut(&room_id).unwrap();
            room.members_by_id[0].as_mut().unwrap().player.team = 1;
            room.members_by_id[1].as_mut().unwrap().player.team = 1;
            room.members_by_id[2].as_mut().unwrap().player.team = 2;
        }
        drain_batches(&mut owner.outbound);
        drain_batches(&mut ally.outbound);
        drain_batches(&mut opponent.outbound);
        drain_batches(&mut observer.outbound);
        for session in [ally.session, opponent.session] {
            world
                .lobby_command(
                    session,
                    LobbyCommandPayload::SetSlotState(PlayerSlotState::Ready),
                )
                .unwrap();
            drain_batches(&mut owner.outbound);
            drain_batches(&mut ally.outbound);
            drain_batches(&mut opponent.outbound);
            drain_batches(&mut observer.outbound);
        }
        world
            .lobby_command(
                owner.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new())),
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        drain_batches(&mut ally.outbound);
        drain_batches(&mut opponent.outbound);
        drain_batches(&mut observer.outbound);
        force_running(&mut world, room_id);

        let ally_sender = world.sessions[&ally.session].outbound.clone().unwrap();
        ally_sender
            .try_send(OutboundBatch::single(vec![0xCC]))
            .unwrap();
        let before = world.protocol_rooms[&room_id].race_progress.clone();
        assert!(matches!(
            world.race_command(
                owner.session,
                booster_request(RaceTeam::Red, 8_000.0),
                Instant::now(),
            ),
            Err(WorldError::Race(RaceError::OutboundUnavailable {
                session
            })) if session == ally.session
        ));
        assert_eq!(world.protocol_rooms[&room_id].race_progress, before);
        assert!(owner.outbound.try_recv().is_err());
        assert!(opponent.outbound.try_recv().is_err());
        assert!(observer.outbound.try_recv().is_err());
        assert_eq!(
            ally.outbound.try_recv().unwrap().into_packets(),
            vec![vec![0xCC]]
        );

        assert_eq!(
            world
                .race_command(
                    owner.session,
                    booster_request(RaceTeam::Red, 8_000.0),
                    Instant::now(),
                )
                .unwrap(),
            RaceCommandOutcome::BoosterGaugeUpdated {
                room_id,
                race_epoch: 1,
                team: RaceTeam::Red,
                reached_full: false,
            }
        );
        for receiver in [
            &mut owner.outbound,
            &mut ally.outbound,
            &mut observer.outbound,
        ] {
            let packets = take_packets(receiver);
            assert_eq!(packets.len(), 1);
            assert_eq!(
                logical_packet_hash(&packets[0]),
                adler32::packet_hash(TEAM_BOOSTER_REPLY_NAME)
            );
            assert_eq!(packets[0][4], RaceTeam::Red as u8);
            assert_eq!(
                f32::from_le_bytes(packets[0][5..9].try_into().unwrap()).to_bits(),
                0.5_f32.to_bits()
            );
        }
        assert!(opponent.outbound.try_recv().is_err());
        assert_eq!(
            world.protocol_rooms[&room_id]
                .race_progress
                .team_gauge(RaceTeam::Red)
                .to_bits(),
            0.5_f32.to_bits()
        );

        let before = world.protocol_rooms[&room_id].race_progress.clone();
        assert!(matches!(
            world.race_command(
                owner.session,
                booster_request(RaceTeam::Blue, 1.0),
                Instant::now(),
            ),
            Err(WorldError::Race(RaceError::TeamSpoof {
                claimed: RaceTeam::Blue,
                actual: 1
            }))
        ));
        assert_eq!(world.protocol_rooms[&room_id].race_progress, before);

        assert!(matches!(
            world
                .race_command(
                    owner.session,
                    booster_request(RaceTeam::Red, 8_000.0),
                    Instant::now(),
                )
                .unwrap(),
            RaceCommandOutcome::BoosterGaugeUpdated {
                reached_full: true,
                ..
            }
        ));
        assert_eq!(
            world.protocol_rooms[&room_id]
                .race_progress
                .team_gauge(RaceTeam::Red)
                .to_bits(),
            0.0_f32.to_bits()
        );
        for receiver in [
            &mut owner.outbound,
            &mut ally.outbound,
            &mut observer.outbound,
        ] {
            let packets = take_packets(receiver);
            assert_eq!(
                f32::from_le_bytes(packets[0][5..9].try_into().unwrap()).to_bits(),
                1.0_f32.to_bits()
            );
        }
        assert!(matches!(
            world.race_command(
                observer.session,
                booster_request(RaceTeam::Red, 1.0),
                Instant::now(),
            ),
            Err(WorldError::Race(RaceError::HumanRacerRequired))
        ));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn settlement_deadline_ranks_dnf_and_retries_ordered_result_atomically() {
        let mut world = World::default();
        let mut p0 = register_channel_session(&mut world, "ResultP0", 68, 42_001, 64);
        let mut p1 = register_channel_session(&mut world, "ResultP1", 68, 42_011, 64);
        let mut p2 = register_channel_session(&mut world, "ResultP2", 68, 42_021, 1);
        let mut p3 = register_channel_session(&mut world, "ResultP3", 68, 42_031, 64);
        let mut observer = register_channel_session(&mut world, "ResultObserver", 68, 42_041, 64);
        let room_id = create_protocol_room(&mut world, &p0, 3);
        for session in [&p1, &p2, &p3] {
            join_protocol_room(&mut world, session, room_id, false);
        }
        join_protocol_room(&mut world, &observer, room_id, true);
        {
            let room = world.protocol_rooms.get_mut(&room_id).unwrap();
            for (player_id, team) in [1_u8, 2, 1, 2].into_iter().enumerate() {
                room.members_by_id[player_id].as_mut().unwrap().player.team = team;
            }
        }
        for player_id in 0..4 {
            let value = u16::try_from(player_id).unwrap();
            set_result_admission(
                &mut world,
                room_id,
                player_id,
                1_010 + value,
                2_010 + value,
                3_010 + u32::from(value),
                4_010 + i32::from(value),
            );
        }
        drain_batches(&mut p0.outbound);
        drain_batches(&mut p1.outbound);
        drain_batches(&mut p2.outbound);
        drain_batches(&mut p3.outbound);
        drain_batches(&mut observer.outbound);
        for session in [p1.session, p2.session, p3.session] {
            world
                .lobby_command(
                    session,
                    LobbyCommandPayload::SetSlotState(PlayerSlotState::Ready),
                )
                .unwrap();
            drain_batches(&mut p0.outbound);
            drain_batches(&mut p1.outbound);
            drain_batches(&mut p2.outbound);
            drain_batches(&mut p3.outbound);
            drain_batches(&mut observer.outbound);
        }
        world
            .lobby_command(
                p0.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new())),
            )
            .unwrap();
        drain_batches(&mut p0.outbound);
        drain_batches(&mut p1.outbound);
        drain_batches(&mut p2.outbound);
        drain_batches(&mut p3.outbound);
        drain_batches(&mut observer.outbound);
        force_running(&mut world, room_id);

        let now = Instant::now();
        let clock = ServerClock::new();
        for (session, time) in [(p1.session, 100_u32), (p0.session, 100), (p3.session, 200)] {
            world
                .race_command_with_clock(
                    session,
                    game_control_request_with_value(2, time),
                    now,
                    &clock,
                )
                .unwrap();
            drain_batches(&mut p0.outbound);
            drain_batches(&mut p1.outbound);
            drain_batches(&mut p2.outbound);
            drain_batches(&mut p3.outbound);
            drain_batches(&mut observer.outbound);
        }
        let frozen = world.protocol_rooms[&room_id].frozen_race.clone();
        let progress = world.protocol_rooms[&room_id].race_progress.clone();
        let settlement = progress.settlement.as_ref().unwrap().clone();

        world.close_session(p0.session, now).unwrap();
        drain_batches(&mut p1.outbound);
        drain_batches(&mut p2.outbound);
        drain_batches(&mut p3.outbound);
        drain_batches(&mut observer.outbound);
        assert_eq!(world.protocol_rooms[&room_id].frozen_race, frozen);
        assert_eq!(world.protocol_rooms[&room_id].race_progress, progress);

        world.advance_loading(
            settlement
                .deadline
                .checked_sub(Duration::from_millis(1))
                .unwrap(),
            &clock,
        );
        assert_eq!(world.protocol_rooms[&room_id].phase, RoomPhase::Settling);
        let p2_sender = world.sessions[&p2.session].outbound.clone().unwrap();
        p2_sender
            .try_send(OutboundBatch::single(vec![0xDD]))
            .unwrap();
        world.advance_loading(settlement.deadline, &clock);
        assert_eq!(world.protocol_rooms[&room_id].phase, RoomPhase::Settling);
        assert_eq!(world.protocol_rooms[&room_id].frozen_race, frozen);
        assert_eq!(
            world.protocol_rooms[&room_id].race_progress.finish_times,
            progress.finish_times
        );
        assert!(matches!(
            &world.protocol_rooms[&room_id]
                .race_progress
                .settlement
                .as_ref()
                .unwrap()
                .finalization,
            super::SettlementFinalization::Persisting { rewards, .. } if rewards.len() == 4
        ));
        let reward_tasks = complete_all_due_rewards(&mut world, settlement.deadline);
        assert_eq!(reward_tasks.len(), 4);
        let frozen_final_packets = match &world.protocol_rooms[&room_id]
            .race_progress
            .settlement
            .as_ref()
            .unwrap()
            .finalization
        {
            super::SettlementFinalization::Ready { packets, .. } => packets.clone(),
            state => panic!("durable receipts must freeze final packets once, got {state:?}"),
        };
        assert!(p1.outbound.try_recv().is_err());
        assert!(p3.outbound.try_recv().is_err());
        assert!(observer.outbound.try_recv().is_err());
        assert_eq!(
            p2.outbound.try_recv().unwrap().into_packets(),
            vec![vec![0xDD]]
        );
        assert!(matches!(
            world.race_command_with_clock(
                p2.session,
                game_control_request_with_value(2, 50),
                settlement.deadline,
                &clock,
            ),
            Err(WorldError::Race(RaceError::SettlementClosed))
        ));
        assert!(RaceError::SettlementClosed.is_expected_rejection());
        assert_eq!(
            match &world.protocol_rooms[&room_id]
                .race_progress
                .settlement
                .as_ref()
                .unwrap()
                .finalization
            {
                super::SettlementFinalization::Ready { packets, .. } => packets,
                state => panic!("blocked finalization changed state to {state:?}"),
            },
            &frozen_final_packets
        );

        world.advance_loading(settlement.deadline + Duration::from_millis(1), &clock);
        let mut expected_packets = None;
        for receiver in [
            &mut p1.outbound,
            &mut p2.outbound,
            &mut p3.outbound,
            &mut observer.outbound,
        ] {
            let packets = take_packets(receiver);
            assert_eq!(packets.len(), 3);
            assert_eq!(
                logical_packet_hash(&packets[0]),
                adler32::packet_hash(GAME_NEXT_STAGE_PACKET_NAME)
            );
            assert_eq!(
                logical_packet_hash(&packets[1]),
                adler32::packet_hash(GAME_RESULT_PACKET_NAME)
            );
            assert_eq!(
                logical_packet_hash(&packets[2]),
                adler32::packet_hash(GAME_CONTROL_PACKET_NAME)
            );
            assert_eq!(i32::from_le_bytes(packets[2][4..8].try_into().unwrap()), 4);
            assert_eq!(
                u32::from_le_bytes(packets[2][9..13].try_into().unwrap()),
                settlement.end_tick.wrapping_add(6_000)
            );
            if let Some(expected) = &expected_packets {
                assert_eq!(&packets, expected);
            } else {
                expected_packets = Some(packets);
            }
        }
        let packets = expected_packets.unwrap();
        assert_eq!(packets, frozen_final_packets);
        let result = &packets[1];
        assert_eq!(result[4], ResultTeam::Blue as u8);
        assert_eq!(i32::from_le_bytes(result[5..9].try_into().unwrap()), 4);
        let record_start = |player_id: usize| 9 + player_id * 217;
        let p0_record = record_start(0);
        assert_eq!(
            u32::from_le_bytes(result[p0_record + 4..p0_record + 8].try_into().unwrap()),
            100
        );
        assert_eq!(
            u16::from_le_bytes(result[p0_record + 9..p0_record + 11].try_into().unwrap()),
            2_010
        );
        assert_eq!(
            i32::from_le_bytes(result[p0_record + 11..p0_record + 15].try_into().unwrap()),
            0
        );
        assert_eq!(
            u32::from_le_bytes(result[p0_record + 18..p0_record + 22].try_into().unwrap()),
            p5136_profile::DEFAULT_RP
        );
        let (p0_reward_index, p0_reward) = reward_tasks
            .iter()
            .enumerate()
            .find(|(_, task)| task.user_no == p0.identity.user_no)
            .map(|(index, task)| (index, task.proposed_reward))
            .unwrap();
        assert_eq!(
            u32::from_le_bytes(result[p0_record + 22..p0_record + 26].try_into().unwrap()),
            p0_reward.earned_rp()
        );
        assert_eq!(
            u32::from_le_bytes(result[p0_record + 26..p0_record + 30].try_into().unwrap()),
            p0_reward.earned_lucci()
        );
        assert_eq!(
            u32::from_le_bytes(result[p0_record + 30..p0_record + 34].try_into().unwrap()),
            50_000 + u32::try_from(p0_reward_index).unwrap()
        );
        assert_eq!(
            i32::from_le_bytes(result[p0_record + 63..p0_record + 67].try_into().unwrap()),
            10
        );
        assert_eq!(
            u16::from_le_bytes(result[p0_record + 85..p0_record + 87].try_into().unwrap()),
            1_010
        );
        assert_eq!(
            i32::from_le_bytes(result[p0_record + 174..p0_record + 178].try_into().unwrap()),
            4_010
        );
        let p2_record = record_start(2);
        assert_eq!(
            u32::from_le_bytes(result[p2_record + 4..p2_record + 8].try_into().unwrap()),
            u32::MAX
        );
        assert_eq!(
            i32::from_le_bytes(result[p2_record + 11..p2_record + 15].try_into().unwrap()),
            3
        );
        assert_eq!(
            i32::from_le_bytes(result[p2_record + 63..p2_record + 67].try_into().unwrap()),
            0
        );

        let room = &world.protocol_rooms[&room_id];
        assert_eq!(room.phase, RoomPhase::Lobby);
        assert!(room.frozen_race.is_none());
        assert_eq!(room.race_progress, super::RaceProgress::default());
        assert!(room.race_fence.is_none());
        assert!(
            room.members_by_id
                .iter()
                .flatten()
                .all(|member| member.player.player_type == PlayerSlotState::NotReady as i32)
        );
    }

    #[test]
    fn settlement_team_tie_and_item_mode_use_first_ranked_team() {
        let mut world = World::default();
        let sessions = (0..6)
            .map(|index| {
                register_channel_session(
                    &mut world,
                    &format!("Tie{index}"),
                    68,
                    42_101 + index * 10,
                    64,
                )
            })
            .collect::<Vec<_>>();
        let room_id = create_protocol_room(&mut world, &sessions[0], 3);
        for session in &sessions[1..] {
            join_protocol_room(&mut world, session, room_id, false);
        }
        {
            let room = world.protocol_rooms.get_mut(&room_id).unwrap();
            for (player_id, team) in [1_u8, 1, 2, 2, 2, 2].into_iter().enumerate() {
                room.members_by_id[player_id].as_mut().unwrap().player.team = team;
            }
        }
        let lobby_snapshot = world.protocol_rooms[&room_id].clone();
        let fence = super::RaceFence::new(room_id, GlobalRaceEpoch::new(1).unwrap());
        let frozen = world
            .freeze_race_roster(&lobby_snapshot, fence, 0x1111_2222)
            .unwrap();
        let room = world.protocol_rooms.get_mut(&room_id).unwrap();
        room.phase = RoomPhase::Settling;
        room.race_fence = Some(fence);
        room.frozen_race = Some(frozen);
        room.race_progress.settlement = Some(super::SettlementState {
            end_tick: 10_000,
            deadline: Instant::now(),
            finalization: super::SettlementFinalization::AwaitingDeadline,
        });
        for player_id in 0..6 {
            room.race_progress
                .finish_times
                .insert(player_id, 100 + u32::try_from(player_id).unwrap());
        }

        let ranking = World::settlement_ranking(room).unwrap();
        assert_eq!(ranking.winning_team, Some(ResultTeam::Red));
        let red_points = [0, 1]
            .into_iter()
            .map(|player_id| ranking.by_player_id[&player_id].team_points)
            .sum::<i32>();
        let blue_points = [2, 3, 4, 5]
            .into_iter()
            .map(|player_id| ranking.by_player_id[&player_id].team_points)
            .sum::<i32>();
        assert_eq!((red_points, blue_points), (18, 18));

        room.settings.game_type = 4;
        room.frozen_race.as_mut().unwrap().participants[0].team = 2;
        assert_eq!(
            World::settlement_ranking(room).unwrap().winning_team,
            Some(ResultTeam::Blue)
        );
    }

    #[test]
    fn running_finish_rejects_stale_and_replacement_generations() {
        let mut world = World::default();
        let owner = register_channel_session(&mut world, "RunFenceOwner", 67, 42_201, 64);
        let guest = register_channel_session(&mut world, "RunFenceGuest", 67, 42_211, 64);
        let room_id = create_protocol_room(&mut world, &owner, 1);
        join_protocol_room(&mut world, &guest, room_id, false);
        world
            .lobby_command(
                guest.session,
                LobbyCommandPayload::SetSlotState(PlayerSlotState::Ready),
            )
            .unwrap();
        world
            .lobby_command(
                owner.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new())),
            )
            .unwrap();
        force_running(&mut world, room_id);
        let before = world.protocol_rooms[&room_id].clone();
        let replacement = migrate_channel_session(&mut world, &guest, 42_221, 64);
        assert!(matches!(
            world.race_command(
                replacement.session,
                game_control_request_with_value(2, 100),
                Instant::now(),
            ),
            Err(WorldError::Race(RaceError::NotFrozenParticipant))
        ));
        assert!(matches!(
            world.race_command(
                guest.session,
                game_control_request_with_value(2, 100),
                Instant::now(),
            ),
            Err(WorldError::Identity(IdentityError::StaleSession(session)))
                if session == guest.session
        ));
        assert_eq!(world.protocol_rooms[&room_id], before);
    }

    #[test]
    fn running_room_without_exact_human_reconciles_to_all_dnf_settlement() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "OrphanOwner", 67, 42_251, 64);
        let mut observer = register_channel_session(&mut world, "OrphanObserver", 67, 42_261, 1);
        let room_id = create_protocol_room(&mut world, &owner, 1);
        join_protocol_room(&mut world, &observer, room_id, true);
        drain_batches(&mut owner.outbound);
        drain_batches(&mut observer.outbound);
        world
            .lobby_command(
                owner.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new())),
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        drain_batches(&mut observer.outbound);
        force_running(&mut world, room_id);

        let mut replacement = migrate_channel_session(&mut world, &owner, 42_271, 64);
        let observer_sender = world.sessions[&observer.session].outbound.clone().unwrap();
        observer_sender
            .try_send(OutboundBatch::single(vec![0xE3]))
            .unwrap();
        let now = Instant::now();
        let clock = ServerClock::new();
        world.advance_loading(now, &clock);

        let room = &world.protocol_rooms[&room_id];
        assert_eq!(room.phase, RoomPhase::Settling);
        assert!(room.race_progress.finish_times.is_empty());
        assert_eq!(room.race_progress.pending_fanouts.len(), 1);
        let settlement = room.race_progress.settlement.as_ref().unwrap();
        assert_eq!(settlement.deadline, now + Duration::from_secs(10));
        assert!(matches!(
            settlement.finalization,
            super::SettlementFinalization::AwaitingDeadline
        ));
        assert_eq!(
            observer.outbound.try_recv().unwrap().into_packets(),
            vec![vec![0xE3]]
        );
        assert!(replacement.outbound.try_recv().is_err());

        world.advance_loading(now + Duration::from_millis(1), &clock);
        let packets = take_packets(&mut observer.outbound);
        assert_eq!(packets.len(), 1);
        assert_eq!(
            logical_packet_hash(&packets[0]),
            adler32::packet_hash(GAME_CONTROL_PACKET_NAME)
        );
        assert_eq!(i32::from_le_bytes(packets[0][4..8].try_into().unwrap()), 3);
        assert!(
            world.protocol_rooms[&room_id]
                .race_progress
                .pending_fanouts
                .is_empty()
        );
        assert!(replacement.outbound.try_recv().is_err());
    }

    #[test]
    fn empty_running_room_id_is_tombstoned_until_terminal_settlement() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "TombstoneOwner", 67, 42_281, 64);
        let mut second = register_channel_session(&mut world, "TombstoneSecond", 67, 42_291, 64);
        let third = register_channel_session(&mut world, "TombstoneThird", 67, 42_301, 64);
        let room_id = create_protocol_room(&mut world, &owner, 1);
        drain_batches(&mut owner.outbound);
        world
            .lobby_command(
                owner.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new())),
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        force_running(&mut world, room_id);

        world
            .room_protocol(owner.session, RoomCommandPayload::Leave)
            .unwrap();
        drain_batches(&mut owner.outbound);
        let room = &world.protocol_rooms[&room_id];
        assert!(room.is_empty());
        assert_eq!(room.phase, RoomPhase::Running);
        assert!(
            !world
                .free_protocol_room_ids
                .contains(&u16::try_from(room_id.0).unwrap())
        );

        let second_room = create_protocol_room(&mut world, &second, 1);
        assert_ne!(second_room, room_id);
        drain_batches(&mut second.outbound);
        let now = Instant::now();
        let clock = ServerClock::new();
        world.advance_loading(now, &clock);
        let room = &world.protocol_rooms[&room_id];
        assert!(room.is_empty());
        assert_eq!(room.phase, RoomPhase::Settling);
        let deadline = room.race_progress.settlement.as_ref().unwrap().deadline;
        assert!(
            !world
                .free_protocol_room_ids
                .contains(&u16::try_from(room_id.0).unwrap())
        );

        world.advance_loading(deadline, &clock);
        assert!(world.protocol_rooms.contains_key(&room_id));
        assert!(
            !world
                .free_protocol_room_ids
                .contains(&u16::try_from(room_id.0).unwrap())
        );
        let reward_tasks = complete_all_due_rewards(&mut world, deadline);
        assert_eq!(reward_tasks.len(), 1);
        world.advance_loading(deadline + Duration::from_millis(1), &clock);
        assert!(!world.protocol_rooms.contains_key(&room_id));
        assert!(
            world
                .free_protocol_room_ids
                .contains(&u16::try_from(room_id.0).unwrap())
        );
        assert_eq!(create_protocol_room(&mut world, &third, 1), room_id);
    }

    #[test]
    fn running_disconnect_keeps_lane_and_reconnect_cannot_start_until_durable_final() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "LaneOwner", 67, 42_311, 64);
        let old_user = owner.identity.user_no;
        let old_room = create_protocol_room(&mut world, &owner, 1);
        drain_batches(&mut owner.outbound);
        world
            .lobby_command(
                owner.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new())),
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        force_running(&mut world, old_room);
        let old_fence = world.reward_lanes[&old_user];

        world.close_session(owner.session, Instant::now()).unwrap();
        assert_eq!(world.reward_lanes.get(&old_user), Some(&old_fence));
        assert!(world.protocol_rooms[&old_room].is_empty());

        let mut replacement = register_channel_session(&mut world, "LaneOwner", 67, 42_312, 64);
        assert_eq!(replacement.identity.user_no, old_user);
        let new_room = create_protocol_room(&mut world, &replacement, 1);
        drain_batches(&mut replacement.outbound);
        assert!(matches!(
            world.lobby_command(
                replacement.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new())),
            ),
            Err(WorldError::Lobby(LobbyError::RewardLaneOccupied {
                user_no,
                room_id,
                race_epoch,
            })) if user_no == old_user.get()
                && room_id == old_room.0
                && race_epoch == old_fence.race_epoch.get()
        ));
        drain_batches(&mut replacement.outbound);

        let clock = ServerClock::new();
        let now = Instant::now();
        world.advance_loading(now, &clock);
        let deadline = world.protocol_rooms[&old_room]
            .race_progress
            .settlement
            .as_ref()
            .unwrap()
            .deadline;
        world.advance_loading(deadline, &clock);
        assert_eq!(complete_all_due_rewards(&mut world, deadline).len(), 1);
        world.advance_loading(deadline + Duration::from_millis(1), &clock);
        assert!(!world.reward_lanes.contains_key(&old_user));
        assert!(!world.protocol_rooms.contains_key(&old_room));

        assert!(matches!(
            world
                .lobby_command(
                    replacement.session,
                    LobbyCommandPayload::StartRoom(StartRoomPlan::new(
                        vec![0x1111_2222],
                        Vec::new(),
                    )),
                )
                .unwrap(),
            LobbyCommandOutcome::Started { room_id, .. } if room_id == new_room
        ));
    }

    #[test]
    fn loading_abort_releases_only_its_exact_reward_lane() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "AbortLane", 67, 42_321, 64);
        let user_no = owner.identity.user_no;
        let room_id = create_protocol_room(&mut world, &owner, 1);
        drain_batches(&mut owner.outbound);
        world
            .lobby_command(
                owner.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new())),
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        assert!(world.reward_lanes.contains_key(&user_no));

        world
            .room_protocol(owner.session, RoomCommandPayload::Leave)
            .unwrap();
        drain_batches(&mut owner.outbound);
        assert!(!world.protocol_rooms.contains_key(&room_id));
        assert!(!world.reward_lanes.contains_key(&user_no));

        let next_room = create_protocol_room(&mut world, &owner, 1);
        drain_batches(&mut owner.outbound);
        assert!(matches!(
            world
                .lobby_command(
                    owner.session,
                    LobbyCommandPayload::StartRoom(StartRoomPlan::new(
                        vec![0x1111_2222],
                        Vec::new(),
                    )),
                )
                .unwrap(),
            LobbyCommandOutcome::Started { room_id, .. } if room_id == next_room
        ));
    }

    #[test]
    fn durable_reward_uses_current_profile_for_live_myroom_and_protocol_caches() {
        let (mut world, mut owner, room_id, deadline, _clock) =
            prepare_single_reward_persistence("RewardPresentation", 42_329);
        enter_myroom(
            &mut world,
            &owner.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 90),
            Ipv4Addr::new(192, 0, 2, 90),
        );
        let revision = world.myroom.revision();
        let task = world
            .take_due_reward_tasks(deadline, 1)
            .unwrap()
            .pop()
            .unwrap();
        let items = [0xD5; RIDER_ITEM_SNAPSHOT_WIRE_LENGTH];
        let presentation =
            MyRoomProfilePresentation::new(45_137, items, 515_137, "RewardFresh".to_owned());

        assert_eq!(
            world
                .complete_reward_task(
                    durable_completion_with_profile(&task, 70_000, presentation),
                    deadline,
                )
                .unwrap(),
            super::RewardCompletionDisposition::Applied
        );

        let protocol_member = world.protocol_rooms[&room_id]
            .members_by_id
            .iter()
            .flatten()
            .find(|member| member.user_no == owner.identity.user_no)
            .unwrap();
        assert_eq!(protocol_member.player.rp, 515_137);
        let snapshot = world.myroom.first_snapshot(&owner.identity).unwrap();
        let MyRoomSlot::Player(player) = &snapshot.slots[0] else {
            panic!("reward recipient must remain the MyRoom owner");
        };
        assert_eq!(player.p2p_port, 45_137);
        assert_eq!(player.rider_item_snapshot, items);
        assert_eq!(player.rp, 515_137);
        assert_eq!(player.club_name, "RewardFresh");
        assert_eq!(world.myroom.revision().get(), revision.get() + 1);
        assert!(owner.outbound.try_recv().is_err());
    }

    #[test]
    fn invalid_myroom_presentation_does_not_reject_a_durable_reward() {
        let (mut world, mut owner, room_id, deadline, _clock) =
            prepare_single_reward_persistence("InvalidRewardPresentation", 42_330);
        enter_myroom(
            &mut world,
            &owner.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 91),
            Ipv4Addr::new(192, 0, 2, 91),
        );
        let revision = world.myroom.revision();
        let cached = world.myroom.first_snapshot(&owner.identity).unwrap();
        let task = world
            .take_due_reward_tasks(deadline, 1)
            .unwrap()
            .pop()
            .unwrap();
        let invalid = MyRoomProfilePresentation::new(
            45_138,
            [0xD6; RIDER_ITEM_SNAPSHOT_WIRE_LENGTH],
            515_138,
            "x".repeat(MAX_CLUB_NAME_UTF16_UNITS + 1),
        );

        assert_eq!(
            world
                .complete_reward_task(
                    durable_completion_with_profile(&task, 70_001, invalid),
                    deadline,
                )
                .unwrap(),
            super::RewardCompletionDisposition::Applied
        );
        let protocol_member = world.protocol_rooms[&room_id]
            .members_by_id
            .iter()
            .flatten()
            .find(|member| member.user_no == owner.identity.user_no)
            .unwrap();
        assert_eq!(protocol_member.player.rp, 515_138);
        assert_eq!(world.myroom.revision(), revision);
        assert_eq!(
            world.myroom.first_snapshot(&owner.identity).unwrap(),
            cached
        );
        assert!(owner.outbound.try_recv().is_err());
    }

    #[test]
    fn reward_completions_are_fenced_by_room_epoch_user_and_attempt() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "FenceReward", 67, 42_331, 64);
        let room_id = create_protocol_room(&mut world, &owner, 1);
        drain_batches(&mut owner.outbound);
        world
            .lobby_command(
                owner.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new())),
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        force_running(&mut world, room_id);
        let now = Instant::now();
        let clock = ServerClock::new();
        world
            .race_command_with_clock(
                owner.session,
                game_control_request_with_value(2, 321),
                now,
                &clock,
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        let deadline = world.protocol_rooms[&room_id]
            .race_progress
            .settlement
            .as_ref()
            .unwrap()
            .deadline;
        world.advance_loading(deadline, &clock);
        let task = world
            .take_due_reward_tasks(deadline, super::ROOM_CAPACITY)
            .unwrap()
            .pop()
            .unwrap();
        let mut stale_room = task.clone();
        stale_room.fence.room_id = RoomId(task.fence.room_id.0 + 1);
        assert_eq!(
            world
                .complete_reward_task(
                    super::RewardPersistenceCompletion::RetryableFailure(stale_room),
                    deadline,
                )
                .unwrap(),
            super::RewardCompletionDisposition::IgnoredStale
        );
        let mut stale_epoch = task.clone();
        stale_epoch.fence.race_epoch =
            GlobalRaceEpoch::new(task.fence.race_epoch.get() + 1).unwrap();
        assert_eq!(
            world
                .complete_reward_task(
                    super::RewardPersistenceCompletion::RetryableFailure(stale_epoch),
                    deadline,
                )
                .unwrap(),
            super::RewardCompletionDisposition::IgnoredStale
        );
        let mut stale_attempt = task.clone();
        stale_attempt.attempt_id =
            super::RewardAttemptId(NonZeroU64::new(task.attempt_id.0.get() + 1).unwrap());
        assert_eq!(
            world
                .complete_reward_task(
                    super::RewardPersistenceCompletion::RetryableFailure(stale_attempt),
                    deadline,
                )
                .unwrap(),
            super::RewardCompletionDisposition::IgnoredStale
        );

        assert_eq!(
            world
                .complete_reward_task(durable_completion(&task, 70_000), deadline)
                .unwrap(),
            super::RewardCompletionDisposition::Applied
        );
        assert!(matches!(
            world.protocol_rooms[&room_id]
                .race_progress
                .settlement
                .as_ref()
                .unwrap()
                .finalization,
            super::SettlementFinalization::Ready { .. }
        ));
    }

    #[test]
    fn standalone_world_spawn_rejects_zero_mailbox_capacity() {
        assert!(matches!(
            WorldHandle::spawn(0),
            Err(WorldSpawnError::ZeroMailboxCapacity)
        ));
    }

    #[tokio::test]
    async fn reward_task_handle_commands_preserve_actor_fencing() {
        let (handle, actor) = WorldHandle::spawn(8).expect("nonzero World mailbox capacity");
        assert!(
            handle
                .take_due_reward_tasks(Instant::now(), 1)
                .await
                .unwrap()
                .is_empty()
        );
        let task = super::RewardSettlementTask::for_test(
            RoomId(1),
            GlobalRaceEpoch::new(1).unwrap(),
            NonZeroU64::MIN,
            crate::UserNo::new(1).unwrap(),
            "NoSuchReward",
            p5136_profile::time_reward_from_rolls(0, 0, 0).unwrap(),
        );
        assert_eq!(
            handle
                .complete_reward_task(
                    super::RewardPersistenceCompletion::FatalFailure(task),
                    Instant::now(),
                )
                .await
                .unwrap(),
            super::RewardCompletionDisposition::IgnoredStale
        );
        handle.shutdown().await.unwrap();
        actor.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn ready_migration_timer_precedes_ready_command_mailbox() {
        let mut world = World::default();
        let source_ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let source = world
            .register_session(SocketAddr::new(source_ip, 43_250), None, None)
            .unwrap();
        let original = world.claim_identity(source, "ExpiredOwnerless").unwrap();
        let issued_at = Instant::now()
            .checked_sub(crate::MIGRATION_TTL + Duration::from_secs(1))
            .unwrap();
        world
            .identities
            .begin_migration(
                source,
                ChannelBinding {
                    channel_id: 67,
                    game_type: 67,
                },
                MigrationToken::new(43_250).unwrap(),
                issued_at,
            )
            .unwrap();
        world
            .close_session(source, issued_at + Duration::from_secs(1))
            .unwrap();
        let replacement = world
            .register_session(SocketAddr::new(source_ip, 43_251), None, None)
            .unwrap();

        let (sender, receiver) = mpsc::channel(4);
        let (reply, response) = oneshot::channel();
        sender
            .try_send(super::WorldCommand::ClaimIdentity {
                session: replacement,
                nickname: "ExpiredOwnerless".to_owned(),
                reply,
            })
            .unwrap();
        let completion_capacity = world.identity_capacity;
        let identity_instance = world.identities.instance();
        let (myroom_completions, completion_receiver) =
            crate::myroom_persistence::MyRoomCompletionBridge::channel(completion_capacity);
        let handle = WorldHandle {
            sender,
            udp_sender: None,
            myroom_completions,
            identity_instance,
        };
        let mut migration_expiry = tokio::time::interval(Duration::from_secs(60));
        migration_expiry.tick().await;
        migration_expiry.reset_immediately();
        let mut loading_heartbeat = tokio::time::interval(Duration::from_secs(60));
        loading_heartbeat.tick().await;
        loading_heartbeat.reset();
        let actor = tokio::spawn(async move {
            super::run_world_actor_with_timers(
                world,
                receiver,
                completion_receiver,
                None,
                super::WorldSidecars::default(),
                ServerClock::new(),
                super::WorldActorTimers {
                    migration_expiry,
                    loading_heartbeat,
                },
            )
            .await
        });

        let rebound = response.await.unwrap().unwrap();
        assert_eq!(rebound.owner, replacement);
        assert_eq!(rebound.user_no, original.user_no);
        assert!(rebound.generation.get() > original.generation.get());
        handle.force_shutdown().await.unwrap();
        actor.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn guarded_shutdown_refuses_loading_queued_and_inflight_reward_lanes() {
        async fn assert_refused(
            mut world: World,
            cancellation_port: u16,
            expected_dead_letters: usize,
        ) {
            let mut cancelled = add_cancellable_session(&mut world, cancellation_port);
            let expected_lanes = world.reward_lanes.len();
            assert!(expected_lanes > 0);
            let (handle, actor) = spawn_prepared_world(world, 16);
            assert!(matches!(
                handle.shutdown().await,
                Err(WorldError::RewardShutdownBlocked {
                    outstanding_lanes,
                    dead_letters,
                }) if outstanding_lanes == expected_lanes
                    && dead_letters == expected_dead_letters
            ));
            let status = handle.reward_drain_status().await.unwrap();
            assert_eq!(status.outstanding_lanes().len(), expected_lanes);
            assert_eq!(status.dead_letters().len(), expected_dead_letters);
            assert!(!status.is_drained());
            assert_eq!(
                cancelled.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            );
            handle.force_shutdown().await.unwrap();
            assert_eq!(cancelled.await, Ok(()));
            actor.await.unwrap();
        }

        let mut loading_world = World::default();
        let mut loading_owner =
            register_channel_session(&mut loading_world, "ShutdownLoading", 67, 42_360, 64);
        let loading_room = create_protocol_room(&mut loading_world, &loading_owner, 1);
        drain_batches(&mut loading_owner.outbound);
        loading_world
            .lobby_command(
                loading_owner.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new())),
            )
            .unwrap();
        drain_batches(&mut loading_owner.outbound);
        assert_eq!(
            loading_world.protocol_rooms[&loading_room].phase,
            RoomPhase::Loading
        );
        assert_refused(loading_world, 42_361, 0).await;

        let (queued_world, _owner, _room_id, _deadline, _clock) =
            prepare_single_reward_persistence("ShutdownQueued", 42_362);
        assert_refused(queued_world, 42_363, 0).await;

        let (mut in_flight_world, _owner, _room_id, deadline, _clock) =
            prepare_single_reward_persistence("ShutdownInFlight", 42_364);
        assert_eq!(
            in_flight_world
                .take_due_reward_tasks(deadline, 1)
                .unwrap()
                .len(),
            1
        );
        assert_refused(in_flight_world, 42_365, 0).await;

        let (mut failed_world, _owner, _room_id, deadline, _clock) =
            prepare_single_reward_persistence("ShutdownDeadLetter", 42_367);
        let task = failed_world
            .take_due_reward_tasks(deadline, 1)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(
            failed_world
                .complete_reward_task(
                    super::RewardPersistenceCompletion::FatalFailure(task),
                    deadline,
                )
                .unwrap(),
            super::RewardCompletionDisposition::TerminalFailure
        );
        assert_refused(failed_world, 42_368, 1).await;
    }

    #[tokio::test]
    async fn drained_shutdown_explicitly_cancels_sessions() {
        let mut world = World::default();
        let cancelled = add_cancellable_session(&mut world, 42_366);
        let (handle, actor) = spawn_prepared_world(world, 8);
        handle.shutdown().await.unwrap();
        assert_eq!(cancelled.await, Ok(()));
        actor.await.unwrap();
    }

    #[tokio::test]
    async fn live_identity_operation_blocks_session_drain_and_guarded_shutdown() {
        let (world, actor) = WorldHandle::spawn(8).expect("nonzero World mailbox capacity");
        let session = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 42_369))
            .await
            .unwrap();
        world
            .claim_identity(session, "ShutdownOperation")
            .await
            .unwrap();
        let operation = world.admit_identity_operation(session).await.unwrap();

        world.quiesce().await.unwrap();
        assert!(matches!(
            world.drain_sessions().await,
            Err(WorldError::IdentityOperationShutdownBlocked { active: 1 })
        ));
        assert!(matches!(
            world.shutdown().await,
            Err(WorldError::IdentityOperationShutdownBlocked { active: 1 })
        ));

        drop(operation);
        world.drain_sessions().await.unwrap();
        world.shutdown().await.unwrap();
        actor.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn identity_operation_capability_cannot_cross_world_actors() {
        let (first_world, first_actor) =
            WorldHandle::spawn(8).expect("nonzero World mailbox capacity");
        let (second_world, second_actor) =
            WorldHandle::spawn(8).expect("nonzero World mailbox capacity");
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let first_session = first_world
            .register_session(SocketAddr::new(ip, 42_370))
            .await
            .unwrap();
        let second_session = second_world
            .register_session(SocketAddr::new(ip, 42_371))
            .await
            .unwrap();
        let first_identity = first_world
            .claim_identity(first_session, "FirstWorld")
            .await
            .unwrap();
        let second_identity = second_world
            .claim_identity(second_session, "FirstWorld")
            .await
            .unwrap();
        assert_eq!(first_session, second_session);
        assert_eq!(
            first_identity, second_identity,
            "independent registries deliberately collide on every public identity field"
        );

        let first_operation = first_world
            .admit_identity_operation(first_session)
            .await
            .unwrap();
        assert_eq!(
            first_world
                .admitted(&first_operation)
                .authorize_identity()
                .await
                .unwrap(),
            first_identity
        );
        assert!(matches!(
            second_world
                .admitted(&first_operation)
                .authorize_identity()
                .await,
            Err(WorldError::ForeignIdentityOperation)
        ));
        let root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) = spawn_test_profile_io(root.path());
        let foreign = second_world.admitted(&first_operation);
        let myroom_admission = profiles
            .admit("FirstWorld", "test foreign MyRoom operation")
            .await
            .unwrap();
        assert!(matches!(
            foreign
                .persist_myroom_owner_info(MyRoomInfo::default(), myroom_admission)
                .await,
            Err(MyRoomInfoWriteError::ForeignIdentityOperation)
        ));
        assert!(matches!(
            foreign.reserve_rider_equipment_completion().await,
            Err(RiderEquipmentWriteError::ForeignIdentityOperation)
        ));

        let equipment_completion = second_world
            .reserve_rider_equipment_completion()
            .await
            .unwrap();
        let equipment_admission = profiles
            .admit("FirstWorld", "test foreign rider-equipment operation")
            .await
            .unwrap();
        let prepared = PreparedRiderEquipmentWrite::new(
            equipment_admission,
            test_equipment_selection(),
            Arc::new(test_equipment_catalog()),
            equipment_completion,
        );
        assert!(matches!(
            foreign.persist_rider_equipment(prepared).await,
            Err(RiderEquipmentWriteError::ForeignIdentityOperation)
        ));
        profile_runtime.shutdown().await.unwrap();

        let second_operation = second_world
            .admit_identity_operation(second_session)
            .await
            .unwrap();
        assert_eq!(
            second_world
                .admitted(&second_operation)
                .authorize_identity()
                .await
                .unwrap(),
            second_identity
        );

        drop(first_operation);
        drop(second_operation);
        first_world.shutdown().await.unwrap();
        second_world.shutdown().await.unwrap();
        first_actor.await.unwrap().unwrap();
        second_actor.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn cancelled_request_cannot_release_an_actor_owned_identity_operation() {
        let mut state = World::default();
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let source = state
            .register_session(SocketAddr::new(ip, 42_372), None, None)
            .unwrap();
        let identity = state.claim_identity(source, "QueuedOperation").unwrap();
        let operation = state.identities.admit_operation(source).unwrap();

        let (sender, receiver) = mpsc::channel(8);
        let identity_instance = state.identities.instance();
        let completion_capacity = state.identity_capacity;
        let (myroom_completions, completion_receiver) =
            MyRoomCompletionBridge::channel(completion_capacity);
        let handle = WorldHandle {
            sender,
            udp_sender: None,
            myroom_completions,
            identity_instance,
        };
        let requester_world = handle.clone();
        let requester = tokio::spawn(async move {
            requester_world
                .admitted(&operation)
                .authorize_identity()
                .await
        });
        time::timeout(Duration::from_secs(1), async {
            while receiver.is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the admitted command was not queued");
        assert_eq!(receiver.len(), 1);
        requester.abort();
        assert!(requester.await.unwrap_err().is_cancelled());

        let channel = ChannelBinding {
            channel_id: 67,
            game_type: 67,
        };
        let token = MigrationToken::new(42_372).unwrap();
        let now = Instant::now();
        state
            .identities
            .begin_migration(source, channel, token, now)
            .unwrap();
        let destination = state
            .register_session(SocketAddr::new(ip, 42_373), None, None)
            .unwrap();
        let preflight = state
            .preflight_migration(
                destination,
                identity.user_no,
                channel.channel_id,
                token,
                now,
            )
            .unwrap();
        assert!(
            !preflight.operations_drained(),
            "the queued actor command must retain its own child lease"
        );
        let completion = handle.myroom_completions.reserve().await.unwrap();
        let registered = RegisteredMigrationPreflight::new(preflight, completion);

        let actor = tokio::spawn(async move {
            super::run_world_actor(
                state,
                receiver,
                completion_receiver,
                None,
                WorldSidecars::default(),
                ServerClock::new(),
            )
            .await
        });
        time::timeout(
            Duration::from_secs(1),
            registered.wait_for_operations_drained(),
        )
        .await
        .expect("the queued actor-owned operation did not retire")
        .unwrap();

        drop(registered);
        handle.drain_myroom_completions().await.unwrap();
        assert_eq!(handle.authorize_identity(source).await.unwrap(), identity);
        handle.shutdown().await.unwrap();
        actor.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn force_shutdown_reports_abandoned_identity_operations() {
        let (world, actor) = WorldHandle::spawn(8).expect("nonzero World mailbox capacity");
        let session = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 42_374))
            .await
            .unwrap();
        world
            .claim_identity(session, "ForcedOperation")
            .await
            .unwrap();
        let operation = world.admit_identity_operation(session).await.unwrap();

        assert!(matches!(
            world.shutdown().await,
            Err(WorldError::IdentityOperationShutdownBlocked { active: 1 })
        ));
        let report = world.force_shutdown().await.unwrap();
        assert_eq!(report.identity_operations, 1);
        actor.await.unwrap().unwrap();
        drop(operation);
    }

    #[test]
    fn migration_acknowledgement_backpressure_preserves_the_source_owner() {
        let mut world = World::default();
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let source = world
            .register_session(SocketAddr::new(ip, 42_420), None, None)
            .unwrap();
        let source_identity = world.claim_identity(source, "AckBackpressure").unwrap();
        let channel = ChannelBinding {
            channel_id: 67,
            game_type: 67,
        };
        let token = MigrationToken::new(4_242).unwrap();
        let issued_at = Instant::now();
        world
            .identities
            .begin_migration(source, channel, token, issued_at)
            .unwrap();

        let (outbound, mut destination_outbound) = mpsc::channel(1);
        outbound
            .try_send(OutboundBatch::single(vec![0xA5]))
            .unwrap();
        let destination = world
            .register_session(SocketAddr::new(ip, 42_421), None, Some(outbound))
            .unwrap();
        let preflight = world
            .preflight_migration(
                destination,
                source_identity.user_no,
                channel.channel_id,
                token,
                issued_at,
            )
            .unwrap();

        assert!(matches!(
            world.complete_preflighted_migration_with_acknowledgement(
                preflight,
                None,
                MigrationAcknowledgement::Ordered(serialize_pr_channel_move_in(39_312, 39_313)),
                None,
                issued_at,
            ),
            Err(WorldOperationError::Command(
                WorldError::MigrationAcknowledgementUnavailable { session }
            )) if session == destination
        ));
        assert_eq!(
            world.identities.authorize(source).unwrap(),
            source_identity,
            "failed ACK admission must reopen the exact source generation"
        );
        assert!(matches!(
            world.identities.authorize(destination),
            Err(IdentityError::UnauthenticatedSession(session)) if session == destination
        ));
        assert_eq!(
            destination_outbound.try_recv().unwrap().into_packets(),
            vec![vec![0xA5]]
        );
        assert!(
            destination_outbound.try_recv().is_err(),
            "backpressure failure must not enqueue a partial ACK"
        );
    }

    #[test]
    fn successful_migration_queues_one_acknowledgement_before_owner_publication() {
        let mut world = World::default();
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let (source_cancellation, mut source_cancelled) = oneshot::channel();
        let source = world
            .register_session(SocketAddr::new(ip, 42_422), Some(source_cancellation), None)
            .unwrap();
        let source_identity = world.claim_identity(source, "AckOrdered").unwrap();
        let channel = ChannelBinding {
            channel_id: 67,
            game_type: 67,
        };
        let token = MigrationToken::new(4_243).unwrap();
        let issued_at = Instant::now();
        world
            .identities
            .begin_migration(source, channel, token, issued_at)
            .unwrap();

        let (destination, outbound_gate, mut destination_outbound) =
            register_tracked_outbound_session(&mut world, 42_423, 1);
        let preflight = world
            .preflight_migration(
                destination,
                source_identity.user_no,
                channel.channel_id,
                token,
                issued_at,
            )
            .unwrap();
        let acknowledgement = serialize_pr_channel_move_in(39_312, 39_313);

        let completion = world
            .complete_preflighted_migration_with_acknowledgement(
                preflight,
                None,
                MigrationAcknowledgement::Ordered(acknowledgement.clone()),
                None,
                issued_at,
            )
            .unwrap();

        assert_eq!(
            world.identities.authorize(destination).unwrap(),
            completion.binding
        );
        assert_eq!(
            outbound_gate.active_counts().outbound,
            1,
            "the already-published owner must still have its queued ACK write lease"
        );
        assert_eq!(
            source_cancelled.try_recv(),
            Ok(()),
            "source cancellation is published only after ACK queue admission"
        );
        assert_eq!(
            destination_outbound.try_recv().unwrap().into_packets(),
            vec![acknowledgement]
        );
        assert!(
            destination_outbound.try_recv().is_err(),
            "migration completion must not duplicate PrChannelMoveIn"
        );
        assert_eq!(outbound_gate.active_counts().outbound, 0);
    }

    #[tokio::test]
    async fn udp_ingress_received_before_migration_cannot_bind_the_new_generation() {
        let game_socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let p2p_socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let udp_runtime =
            UdpRuntime::spawn(game_socket, p2p_socket, UdpRuntimeConfig::default()).unwrap();
        let udp = udp_runtime.service();

        let mut world = World::default();
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let source = world
            .register_session(SocketAddr::new(ip, 42_424), None, None)
            .unwrap();
        let source_identity = world.claim_identity(source, "UdpArrivalFence").unwrap();
        let channel = ChannelBinding {
            channel_id: 67,
            game_type: 67,
        };
        let token = MigrationToken::new(4_244).unwrap();
        let received_at = Instant::now();
        world
            .identities
            .begin_migration(source, channel, token, received_at)
            .unwrap();
        let destination = world
            .register_session(SocketAddr::new(ip, 42_425), None, None)
            .unwrap();
        let preflight = world
            .preflight_migration(
                destination,
                source_identity.user_no,
                channel.channel_id,
                token,
                received_at,
            )
            .unwrap();
        let stale_epoch = udp.advance_arrival_epoch();
        let activated_at = received_at + Duration::from_secs(1);
        let migrated = world
            .complete_preflighted_migration_with_acknowledgement(
                preflight,
                None,
                MigrationAcknowledgement::Omitted,
                Some(&udp),
                activated_at,
            )
            .unwrap()
            .binding;
        udp.advance_identity(migrated.clone()).await.unwrap();

        let stale_endpoint = SocketAddr::new(ip, 58_424);
        super::dispatch_udp_ingress(
            &mut world,
            &udp,
            UdpIngress {
                arrival_epoch: stale_epoch,
                transport: UdpTransport::Game,
                source: stale_endpoint,
                iv: 0x5136_5136,
                account_id: migrated.user_no.get(),
                route_hash: 0xA5A5_0001,
                body: UdpIngressBody::GameSlotPacket(Vec::new()),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            udp.current_target(UdpTransport::Game, migrated.clone())
                .await
                .unwrap(),
            None,
            "a queued datagram from the old arrival epoch must not bind the new generation"
        );

        let fresh_endpoint = SocketAddr::new(ip, 58_425);
        let fresh_epoch = udp.advance_arrival_epoch();
        super::dispatch_udp_ingress(
            &mut world,
            &udp,
            UdpIngress {
                arrival_epoch: fresh_epoch,
                transport: UdpTransport::Game,
                source: fresh_endpoint,
                iv: 0x5136_5136,
                account_id: migrated.user_no.get(),
                route_hash: 0xA5A5_0002,
                body: UdpIngressBody::GameSlotPacket(Vec::new()),
            },
        )
        .await
        .unwrap();
        let current = udp
            .current_target(UdpTransport::Game, migrated)
            .await
            .unwrap()
            .expect("a packet from the current arrival epoch should bind normally");
        assert_eq!(current.endpoint.endpoint, fresh_endpoint);
        assert_eq!(current.endpoint.route_hash, 0xA5A5_0002);

        udp_runtime.shutdown().await;
    }

    #[tokio::test]
    async fn migration_preflight_handle_freezes_source_and_rechecks_destination_liveness() {
        let (handle, actor) = WorldHandle::spawn(16).expect("nonzero World mailbox capacity");
        let source = handle
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 42_350))
            .await
            .unwrap();
        let source_identity = handle
            .claim_identity(source, "PreflightRider")
            .await
            .unwrap();
        let channel = ChannelBinding {
            channel_id: 67,
            game_type: 67,
        };
        let migration_token = MigrationToken::new(777).unwrap();
        handle
            .begin_migration(source, channel, migration_token, Instant::now())
            .await
            .unwrap();
        let first_destination = handle
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 42_351))
            .await
            .unwrap();
        let preflight = handle
            .preflight_migration(
                first_destination,
                source_identity.user_no,
                channel.channel_id,
                migration_token,
                Instant::now(),
            )
            .await
            .unwrap();
        assert_eq!(preflight.nickname(), "PreflightRider");
        assert_eq!(preflight.canonical_nickname(), "preflightrider");
        assert_eq!(preflight.user_no(), source_identity.user_no);
        assert_eq!(preflight.source_generation(), source_identity.generation);
        assert!(matches!(
            handle.authorize_identity(source).await,
            Err(WorldError::Identity(IdentityError::TransferInProgress {
                ref nickname,
            })) if nickname == "PreflightRider"
        ));

        handle.session_closed(first_destination).await.unwrap();
        assert!(matches!(
            handle
                .complete_preflighted_migration(
                    preflight,
                    MyRoomProfileLease::for_test(
                        test_myroom_profile_presentation(),
                        "PreflightRider",
                    ),
                )
                .await,
            Err(WorldError::UnknownSession(session)) if session == first_destination
        ));
        assert_eq!(
            handle.authorize_identity(source).await.unwrap(),
            source_identity
        );

        let second_destination = handle
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 42_352))
            .await
            .unwrap();
        let preflight = handle
            .preflight_migration(
                second_destination,
                source_identity.user_no,
                channel.channel_id,
                migration_token,
                Instant::now(),
            )
            .await
            .unwrap();
        let completion = handle
            .complete_preflighted_migration(
                preflight,
                MyRoomProfileLease::for_test(test_myroom_profile_presentation(), "PreflightRider"),
            )
            .await
            .unwrap();
        assert_eq!(completion.binding.owner, second_destination);
        assert!(completion.binding.generation.get() > source_identity.generation.get());

        handle.shutdown().await.unwrap();
        actor.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn dropped_ready_waiter_cannot_cancel_actor_owned_migration_commit() {
        let mut state = World::default();
        let (source_cancellation, source_cancelled) = oneshot::channel();
        let source = state
            .register_session(
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 42_353),
                Some(source_cancellation),
                None,
            )
            .unwrap();
        let identity = state
            .claim_identity(source, "ReadyCancellationRider")
            .unwrap();
        let channel = ChannelBinding {
            channel_id: 67,
            game_type: 67,
        };
        let token = MigrationToken::new(778).unwrap();
        state
            .identities
            .begin_migration(source, channel, token, Instant::now())
            .unwrap();
        let destination = state
            .register_session(
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 42_354),
                None,
                None,
            )
            .unwrap();
        let raw_preflight = state
            .preflight_migration(
                destination,
                identity.user_no,
                channel.channel_id,
                token,
                Instant::now(),
            )
            .unwrap();

        let (sender, receiver) = mpsc::channel(8);
        let identity_instance = state.identities.instance();
        let (myroom_completions, completion_receiver) =
            MyRoomCompletionBridge::channel(state.identity_capacity);
        let completion = myroom_completions.reserve().await.unwrap();
        let preflight = RegisteredMigrationPreflight::new(raw_preflight, completion);
        let (reply, response) = oneshot::channel();
        preflight.submit(
            MyRoomProfileLease::for_test(
                test_myroom_profile_presentation(),
                "ReadyCancellationRider",
            ),
            reply,
        );
        drop(response);

        let handle = WorldHandle {
            sender,
            udp_sender: None,
            myroom_completions,
            identity_instance,
        };
        let actor = tokio::spawn(async move {
            super::run_world_actor(
                state,
                receiver,
                completion_receiver,
                None,
                WorldSidecars::default(),
                ServerClock::new(),
            )
            .await
        });

        let migrated = handle.authorize_identity(destination).await.unwrap();
        assert_eq!(migrated.user_no, identity.user_no);
        assert!(migrated.generation.get() > identity.generation.get());
        time::timeout(Duration::from_secs(1), source_cancelled)
            .await
            .expect("migration did not cancel the previous owner")
            .unwrap();

        handle.shutdown().await.unwrap();
        actor.await.unwrap().unwrap();
    }

    #[test]
    fn reward_retry_failures_are_bounded_and_end_in_an_observable_terminal_state() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "RetryBound", 67, 42_341, 64);
        let room_id = create_protocol_room(&mut world, &owner, 1);
        drain_batches(&mut owner.outbound);
        world
            .lobby_command(
                owner.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new())),
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        force_running(&mut world, room_id);
        let now = Instant::now();
        let clock = ServerClock::new();
        world
            .race_command_with_clock(
                owner.session,
                game_control_request_with_value(2, 456),
                now,
                &clock,
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        let deadline = world.protocol_rooms[&room_id]
            .race_progress
            .settlement
            .as_ref()
            .unwrap()
            .deadline;
        world.advance_loading(deadline, &clock);

        let mut due_at = deadline;
        let mut proposal = None;
        for failure_count in 1..=super::MAX_REWARD_PERSISTENCE_FAILURES {
            let task = world
                .take_due_reward_tasks(due_at, super::ROOM_CAPACITY)
                .unwrap()
                .pop()
                .unwrap();
            if let Some(proposal) = proposal {
                assert_eq!(task.proposed_reward, proposal);
            } else {
                proposal = Some(task.proposed_reward);
            }
            let disposition = world
                .complete_reward_task(
                    super::RewardPersistenceCompletion::RetryableFailure(task),
                    due_at,
                )
                .unwrap();
            if failure_count == super::MAX_REWARD_PERSISTENCE_FAILURES {
                assert_eq!(
                    disposition,
                    super::RewardCompletionDisposition::TerminalFailure
                );
            } else {
                assert_eq!(
                    disposition,
                    super::RewardCompletionDisposition::RetryScheduled { failure_count }
                );
                due_at += super::reward_retry_delay(failure_count);
            }
        }
        assert!(matches!(
            &world.protocol_rooms[&room_id]
                .race_progress
                .settlement
                .as_ref()
                .unwrap()
                .finalization,
            super::SettlementFinalization::Failed(failed)
                if failed.reason == super::RewardTerminalReason::RewardPersistence
        ));
        assert!(world.reward_lanes.contains_key(&owner.identity.user_no));
    }

    #[test]
    fn abandoned_reward_attempt_lease_requeues_same_proposal_and_fences_late_completion() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "LeaseRetry", 67, 42_342, 64);
        let room_id = create_protocol_room(&mut world, &owner, 1);
        drain_batches(&mut owner.outbound);
        world
            .lobby_command(
                owner.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new())),
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        force_running(&mut world, room_id);
        let now = Instant::now();
        let clock = ServerClock::new();
        world
            .race_command_with_clock(
                owner.session,
                game_control_request_with_value(2, 654),
                now,
                &clock,
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        let deadline = world.protocol_rooms[&room_id]
            .race_progress
            .settlement
            .as_ref()
            .unwrap()
            .deadline;
        world.advance_loading(deadline, &clock);
        let abandoned = world
            .take_due_reward_tasks(deadline, 1)
            .unwrap()
            .pop()
            .unwrap();

        let lease_expired = deadline + super::REWARD_ATTEMPT_LEASE;
        assert!(
            world
                .take_due_reward_tasks(lease_expired, 1)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            world
                .complete_reward_task(durable_completion(&abandoned, 80_000), lease_expired,)
                .unwrap(),
            super::RewardCompletionDisposition::IgnoredStale
        );

        let retry_at = lease_expired + super::reward_retry_delay(1);
        let retry = world
            .take_due_reward_tasks(retry_at, 1)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(retry.fence, abandoned.fence);
        assert_eq!(retry.user_no, abandoned.user_no);
        assert_eq!(retry.nickname, abandoned.nickname);
        assert_eq!(retry.proposed_reward, abandoned.proposed_reward);
        assert_ne!(retry.attempt_id, abandoned.attempt_id);
        assert_eq!(
            world
                .complete_reward_task(durable_completion(&retry, 80_001), retry_at,)
                .unwrap(),
            super::RewardCompletionDisposition::Applied
        );
    }

    #[test]
    fn quiesce_atomically_blocks_new_starts_but_existing_settlement_drains() {
        let mut lobby_world = World::default();
        let mut lobby_owner =
            register_channel_session(&mut lobby_world, "QuiesceLobby", 67, 42_343, 64);
        let lobby_room = create_protocol_room(&mut lobby_world, &lobby_owner, 1);
        drain_batches(&mut lobby_owner.outbound);
        let next_epoch = lobby_world.next_race_epoch;
        lobby_world.quiesce();
        lobby_world.quiesce();
        assert!(matches!(
            lobby_world.lobby_command(
                lobby_owner.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new(),)),
            ),
            Err(WorldError::Lobby(LobbyError::WorldQuiescing))
        ));
        assert_eq!(lobby_world.next_race_epoch, next_epoch);
        assert!(lobby_world.reward_lanes.is_empty());
        assert_eq!(
            lobby_world.protocol_rooms[&lobby_room].phase,
            RoomPhase::Lobby
        );
        let rejection = take_packets(&mut lobby_owner.outbound);
        assert_eq!(rejection.len(), 1);
        assert_eq!(
            logical_packet_hash(&rejection[0]),
            adler32::packet_hash(START_ROOM_REPLY_NAME)
        );

        let (mut settling_world, _owner, room_id, deadline, _clock) =
            prepare_single_reward_persistence("QuiesceDrain", 42_344);
        settling_world.quiesce();
        let task = settling_world
            .take_due_reward_tasks(deadline, 1)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(
            settling_world
                .complete_reward_task(durable_completion(&task, 81_000), deadline)
                .unwrap(),
            super::RewardCompletionDisposition::Applied
        );
        assert!(matches!(
            settling_world.protocol_rooms[&room_id]
                .race_progress
                .settlement
                .as_ref()
                .unwrap()
                .finalization,
            super::SettlementFinalization::Ready { .. }
        ));
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the producer-barrier regression keeps queue pressure, settlement completion, actor sealing, and post-seal admission in one causal scenario"
    )]
    async fn outbound_producer_barrier_retries_backpressure_then_permanently_seals_wire_work() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "BarrierOwner", 67, 42_369, 64);
        let mut guest = register_channel_session(&mut world, "BarrierGuest", 67, 42_371, 1);
        let room_id = create_protocol_room(&mut world, &owner, 1);
        join_protocol_room(&mut world, &guest, room_id, false);
        drain_batches(&mut owner.outbound);
        drain_batches(&mut guest.outbound);
        world
            .lobby_command(
                guest.session,
                LobbyCommandPayload::SetSlotState(PlayerSlotState::Ready),
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        drain_batches(&mut guest.outbound);
        world
            .lobby_command(
                owner.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new())),
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        drain_batches(&mut guest.outbound);
        force_running(&mut world, room_id);

        let operations = WireOperationGate::new();
        world
            .sessions
            .get_mut(&owner.session)
            .unwrap()
            .outbound_operations = Some(operations.clone());
        world
            .sessions
            .get_mut(&guest.session)
            .unwrap()
            .outbound_operations = Some(operations.clone());
        let guest_sender = world.sessions[&guest.session].outbound.clone().unwrap();
        guest_sender
            .try_send(OutboundBatch::single(vec![0xD1]))
            .unwrap();

        let finished_at = Instant::now();
        let clock = ServerClock::new();
        world
            .race_command_with_clock(
                owner.session,
                game_control_request_with_value(2, 1_337),
                finished_at,
                &clock,
            )
            .unwrap();
        assert_eq!(
            world.protocol_rooms[&room_id]
                .race_progress
                .pending_fanouts
                .len(),
            1
        );
        assert_eq!(operations.active_counts().outbound, 0);

        let deadline = world.protocol_rooms[&room_id]
            .race_progress
            .settlement
            .as_ref()
            .unwrap()
            .deadline;
        world.advance_loading(deadline, &clock);
        let completed_tasks = complete_all_due_rewards(&mut world, deadline);
        assert_eq!(completed_tasks.len(), 2);
        assert!(matches!(
            world.protocol_rooms[&room_id]
                .race_progress
                .settlement
                .as_ref()
                .unwrap()
                .finalization,
            super::SettlementFinalization::Ready { .. }
        ));

        let (handle, actor) = spawn_prepared_world(world, 16);
        assert!(matches!(
            handle.drain_outbound_producers_once().await,
            Err(WorldError::OutboundProducerDrainRequiresQuiesce)
        ));
        handle.quiesce().await.unwrap();

        assert!(!handle.drain_outbound_producers_once().await.unwrap());
        assert_eq!(operations.active_counts().outbound, 0);
        assert_eq!(
            guest.outbound.try_recv().unwrap().into_packets(),
            vec![vec![0xD1]]
        );

        // The first retry publishes the retained race-time batch. The final
        // settlement remains blocked because that publication consumes the
        // guest's sole queue slot.
        assert!(!handle.drain_outbound_producers_once().await.unwrap());
        assert_eq!(operations.active_counts().outbound, 2);
        drop(owner.outbound.try_recv().unwrap());
        drop(guest.outbound.try_recv().unwrap());
        assert_eq!(operations.active_counts().outbound, 0);

        // Once both writers make progress, one actor turn publishes the final
        // settlement, releases every reward lane, and permanently seals all
        // future wire-producing commands.
        assert!(handle.drain_outbound_producers_once().await.unwrap());
        assert_eq!(operations.active_counts().outbound, 2);

        let external = handle.clone();
        assert!(matches!(
            external
                .room_protocol(owner.session, RoomCommandPayload::FirstState)
                .await,
            Err(WorldError::OutboundProductionClosed)
        ));
        assert!(matches!(
            external.create_room().await,
            Err(WorldError::OutboundProductionClosed)
        ));
        assert_eq!(operations.active_counts().outbound, 2);

        // Read-only and stale completion work remains admissible after the
        // producer seal and cannot resurrect a retired reward lane.
        assert_eq!(
            external.authorize_identity(owner.session).await.unwrap(),
            owner.identity
        );
        assert!(
            external
                .take_due_reward_tasks(deadline, super::ROOM_CAPACITY)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            external
                .complete_reward_task(
                    super::RewardPersistenceCompletion::RetryableFailure(
                        completed_tasks[0].clone(),
                    ),
                    deadline,
                )
                .await
                .unwrap(),
            super::RewardCompletionDisposition::IgnoredStale
        );
        assert_eq!(operations.active_counts().outbound, 2);

        drop(owner.outbound.try_recv().unwrap());
        drop(guest.outbound.try_recv().unwrap());
        assert_eq!(operations.active_counts().outbound, 0);
        assert!(handle.drain_outbound_producers_once().await.unwrap());

        handle.shutdown().await.unwrap();
        actor.await.unwrap();
    }

    #[test]
    fn quiesce_freezes_a_scheduled_loading_timer_before_it_can_publish() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "FrozenTimerOwner", 67, 42_373, 64);
        let room_id = create_protocol_room(&mut world, &owner, 1);
        drain_batches(&mut owner.outbound);
        world
            .lobby_command(
                owner.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new())),
            )
            .unwrap();
        drain_batches(&mut owner.outbound);

        let operations = WireOperationGate::new();
        world
            .sessions
            .get_mut(&owner.session)
            .unwrap()
            .outbound_operations = Some(operations.clone());
        let armed_at = Instant::now();
        world
            .race_command(owner.session, game_control_request(0), armed_at)
            .unwrap();
        let timeout = armed_at + Duration::from_secs(30);
        let clock = ServerClock::new();
        world.advance_loading(timeout, &clock);
        let scheduled = world.protocol_rooms[&room_id].loading_handshake.clone();
        assert!(matches!(scheduled, LoadingHandshake::StartScheduled { .. }));

        world.quiesce();
        world.advance_loading(timeout + Duration::from_secs(2), &clock);
        assert_eq!(world.protocol_rooms[&room_id].phase, RoomPhase::Loading);
        assert_eq!(world.protocol_rooms[&room_id].loading_handshake, scheduled);
        assert!(owner.outbound.try_recv().is_err());
        assert_eq!(operations.active_counts().outbound, 0);
    }

    #[tokio::test]
    async fn quiesced_actor_drops_udp_ingress_before_sidecar_dispatch() {
        let world = World::default();
        let (sender, receiver) = mpsc::channel(8);
        let (udp_sender, udp_receiver) = mpsc::channel(8);
        let identity_instance = world.identities.instance();
        let (myroom_completions, completion_receiver) =
            MyRoomCompletionBridge::channel(world.identity_capacity);
        let handle = WorldHandle {
            sender,
            udp_sender: Some(udp_sender),
            myroom_completions,
            identity_instance,
        };
        let actor = tokio::spawn(async move {
            super::run_world_actor(
                world,
                receiver,
                completion_receiver,
                Some(udp_receiver),
                WorldSidecars::default(),
                ServerClock::new(),
            )
            .await
        });

        handle.quiesce().await.unwrap();
        handle
            .try_udp_ingress(UdpIngress {
                arrival_epoch: 1,
                transport: UdpTransport::Game,
                source: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 42_375),
                iv: 0x5136_5136,
                account_id: 1,
                route_hash: 0x1234_5678,
                body: UdpIngressBody::GameSlotPacket(Vec::new()),
            })
            .unwrap();
        time::sleep(Duration::from_millis(20)).await;
        assert!(
            !actor.is_finished(),
            "quiesced UDP ingress reached the deliberately absent UDP sidecar"
        );
        assert_eq!(handle.session_count().await.unwrap(), 0);
        assert!(handle.drain_outbound_producers_once().await.unwrap());

        handle.shutdown().await.unwrap();
        actor.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn session_close_during_quiesce_reconciles_rooms_without_wire_publication() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "SilentCloseOwner", 67, 42_377, 8);
        let mut guest = register_channel_session(&mut world, "SilentCloseGuest", 67, 42_379, 8);
        let room_id = create_protocol_room(&mut world, &owner, 1);
        join_protocol_room(&mut world, &guest, room_id, false);
        enter_myroom(
            &mut world,
            &owner.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 77),
            Ipv4Addr::new(192, 0, 2, 77),
        );
        enter_myroom(
            &mut world,
            &guest.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 78),
            Ipv4Addr::new(192, 0, 2, 77),
        );
        drain_batches(&mut owner.outbound);
        drain_batches(&mut guest.outbound);
        let operations = WireOperationGate::new();
        world
            .sessions
            .get_mut(&owner.session)
            .unwrap()
            .outbound_operations = Some(operations.clone());
        world
            .sessions
            .get_mut(&guest.session)
            .unwrap()
            .outbound_operations = Some(operations.clone());
        world.quiesce();

        let (reply, response) = oneshot::channel();
        assert!(
            !dispatch_command(
                &mut world,
                WorldCommand::SessionClosed {
                    id: guest.session,
                    reply: Some(reply),
                },
                &WorldSidecars::default(),
                &ServerClock::new(),
            )
            .await
            .unwrap()
        );
        response.await.unwrap();

        assert!(!world.sessions.contains_key(&guest.session));
        assert!(
            !world
                .protocol_room_by_user
                .contains_key(&guest.identity.user_no)
        );
        assert_eq!(
            world.protocol_rooms[&room_id]
                .members_by_id
                .iter()
                .flatten()
                .count(),
            1
        );
        assert_eq!(world.myroom.member_count(), 1);
        assert_eq!(
            world.myroom.membership_if_member(&guest.identity).unwrap(),
            None
        );
        assert!(owner.outbound.try_recv().is_err());
        assert!(guest.outbound.try_recv().is_err());
        assert_eq!(operations.active_counts().outbound, 0);
    }

    #[test]
    fn producer_barrier_expires_an_ownerless_preflight_without_wire_or_resurrection() {
        let mut world = World::default();
        let source = world
            .register_session(
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 42_381),
                None,
                None,
            )
            .unwrap();
        let identity = world.claim_identity(source, "BarrierMigration").unwrap();
        let channel = ChannelBinding {
            channel_id: 67,
            game_type: 67,
        };
        let token = MigrationToken::new(42_381).unwrap();
        let issued_at = Instant::now();
        world
            .identities
            .begin_migration(source, channel, token, issued_at)
            .unwrap();
        let (outbound, mut destination_outbound) = mpsc::channel(1);
        let operations = WireOperationGate::new();
        let destination = world
            .register_session_with_operations(
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 42_383),
                None,
                Some(outbound),
                Some(operations.clone()),
            )
            .unwrap();
        let preflight = world
            .preflight_migration(
                destination,
                identity.user_no,
                channel.channel_id,
                token,
                issued_at,
            )
            .unwrap();
        assert_eq!(world.identities.transfer_in_progress_count(), 1);

        world.quiesce();
        world.close_session_without_wire(source, issued_at).unwrap();
        assert!(world.identities.is_current_ownerless_binding(&identity));
        assert!(world.drain_outbound_producers_once().unwrap());
        assert_eq!(world.identities.transfer_in_progress_count(), 0);
        assert_eq!(world.identities.active_count(), 0);
        assert!(world.outbound_producers_sealed);

        assert!(matches!(
            world.complete_preflighted_migration(preflight, None, Instant::now()),
            Err(WorldOperationError::Command(WorldError::Identity(
                IdentityError::UnknownUserNo(_)
                    | IdentityError::NoMigrationPermit { .. }
                    | IdentityError::StaleMigrationPreflight
            )))
        ));
        assert_eq!(world.identities.transfer_in_progress_count(), 0);
        assert!(destination_outbound.try_recv().is_err());
        assert_eq!(operations.active_counts().outbound, 0);
    }

    #[test]
    fn completion_at_or_after_lease_deadline_expires_before_it_can_apply() {
        for (index, lateness) in [Duration::ZERO, Duration::from_nanos(1)]
            .into_iter()
            .enumerate()
        {
            let port = 42_345 + u16::try_from(index).unwrap();
            let (mut world, _owner, room_id, deadline, _clock) =
                prepare_single_reward_persistence(&format!("LeaseBoundary{index}"), port);
            let task = world
                .take_due_reward_tasks(deadline, 1)
                .unwrap()
                .pop()
                .unwrap();
            let completion_time = deadline + super::REWARD_ATTEMPT_LEASE + lateness;
            assert_eq!(
                world
                    .complete_reward_task(
                        super::RewardPersistenceCompletion::RetryableFailure(task.clone()),
                        completion_time,
                    )
                    .unwrap(),
                super::RewardCompletionDisposition::IgnoredStale
            );
            let settlement = world.protocol_rooms[&room_id]
                .race_progress
                .settlement
                .as_ref()
                .unwrap();
            let super::SettlementFinalization::Persisting { rewards, .. } =
                &settlement.finalization
            else {
                panic!("expired completion must leave retryable persistence state");
            };
            assert!(matches!(
                rewards[0].status,
                super::RewardPersistenceStatus::Queued {
                    due_at,
                    failure_count: 1,
                } if due_at == completion_time + super::reward_retry_delay(1)
            ));
            assert!(
                world
                    .take_due_reward_tasks(completion_time, 1)
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn terminal_reward_state_is_queryable_inert_and_explicitly_retryable() {
        let (mut world, owner, room_id, deadline, clock) =
            prepare_single_reward_persistence("DeadLetterRetry", 42_347);
        let task = world
            .take_due_reward_tasks(deadline, 1)
            .unwrap()
            .pop()
            .unwrap();
        let proposal = task.proposed_reward();
        let attempt_id = task.attempt_id();
        assert_eq!(task.canonical_nickname(), "deadletterretry");
        assert_eq!(
            world
                .complete_reward_task(
                    super::RewardPersistenceCompletion::FatalFailure(task.clone()),
                    deadline,
                )
                .unwrap(),
            super::RewardCompletionDisposition::TerminalFailure
        );
        let retained = world.protocol_rooms[&room_id]
            .race_progress
            .settlement
            .as_ref()
            .unwrap()
            .finalization
            .clone();
        let super::SettlementFinalization::Failed(failed) = &retained else {
            panic!("fatal persistence must retain a dead letter");
        };
        assert!(failed.ranking.is_some());
        assert_eq!(failed.rewards.len(), 1);
        assert_eq!(failed.rewards[0].proposed_reward, proposal);
        assert_eq!(
            failed.reason,
            super::RewardTerminalReason::RewardPersistence
        );

        let status = world.reward_drain_status().unwrap();
        assert!(!status.is_drained());
        assert_eq!(status.terminal_count(), 1);
        assert_eq!(status.outstanding_lanes().len(), 1);
        assert_eq!(
            status.outstanding_lanes()[0].phase(),
            super::RewardLanePhase::Terminal
        );
        let dead_letter = status.dead_letters()[0].clone();
        let original_dead_letter = dead_letter.clone();
        assert_eq!(dead_letter.fence(), task.fence());
        assert_eq!(dead_letter.failed_attempt_id(), Some(attempt_id));
        assert_eq!(dead_letter.failed_user_no(), Some(owner.identity.user_no));
        assert_eq!(dead_letter.failed_nickname(), Some("DeadLetterRetry"));
        assert_eq!(
            dead_letter.failed_canonical_nickname(),
            Some("deadletterretry")
        );
        assert_eq!(dead_letter.failed_proposed_reward(), Some(proposal));

        world.advance_loading(deadline + Duration::from_secs(60), &clock);
        assert_eq!(
            world.protocol_rooms[&room_id]
                .race_progress
                .settlement
                .as_ref()
                .unwrap()
                .finalization,
            retained
        );

        let reset_at = deadline + Duration::from_secs(61);
        world
            .retry_reward_dead_letter(dead_letter.clone(), reset_at)
            .unwrap();
        let reset_state = world.protocol_rooms[&room_id]
            .race_progress
            .settlement
            .as_ref()
            .unwrap()
            .finalization
            .clone();
        let super::SettlementFinalization::Persisting { ranking, rewards } = &reset_state else {
            panic!("explicit reconciliation must restore persistence");
        };
        assert_eq!(ranking, failed.ranking.as_ref().unwrap());
        assert_eq!(rewards[0].proposed_reward, proposal);
        assert!(matches!(
            rewards[0].status,
            super::RewardPersistenceStatus::Queued {
                due_at,
                failure_count: 0,
            } if due_at == reset_at
        ));
        assert!(matches!(
            world.retry_reward_dead_letter(dead_letter, reset_at),
            Err(WorldError::StaleRewardDeadLetter)
        ));
        assert_eq!(
            world.protocol_rooms[&room_id]
                .race_progress
                .settlement
                .as_ref()
                .unwrap()
                .finalization,
            reset_state
        );
        assert_eq!(
            world
                .complete_reward_task(
                    super::RewardPersistenceCompletion::RetryableFailure(task),
                    reset_at,
                )
                .unwrap(),
            super::RewardCompletionDisposition::IgnoredStale
        );
        let retry = world
            .take_due_reward_tasks(reset_at, 1)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(retry.proposed_reward(), proposal);
        assert_ne!(retry.attempt_id(), attempt_id);
        let retry_attempt_id = retry.attempt_id();
        assert_eq!(
            world
                .complete_reward_task(
                    super::RewardPersistenceCompletion::FatalFailure(retry),
                    reset_at,
                )
                .unwrap(),
            super::RewardCompletionDisposition::TerminalFailure
        );
        let reterminalized_status = world.reward_drain_status().unwrap();
        let reterminalized = &reterminalized_status.dead_letters()[0];
        assert_eq!(reterminalized.fence(), original_dead_letter.fence());
        assert_eq!(reterminalized.failed_attempt_id(), Some(retry_attempt_id));
        assert_ne!(
            reterminalized.failed_attempt_id(),
            original_dead_letter.failed_attempt_id()
        );
        assert!(matches!(
            world.retry_reward_dead_letter(original_dead_letter, reset_at),
            Err(WorldError::StaleRewardDeadLetter)
        ));
    }

    #[test]
    fn reward_completion_revalidates_exact_and_canonical_nickname_stamp() {
        let (mut world, _owner, _room_id, deadline, _clock) =
            prepare_single_reward_persistence("NicknameStamp", 42_348);
        let task = world
            .take_due_reward_tasks(deadline, 1)
            .unwrap()
            .pop()
            .unwrap();

        let mut wrong_exact = task.clone();
        wrong_exact.nickname = "nicknamestamp".to_owned();
        assert_eq!(
            world
                .complete_reward_task(
                    super::RewardPersistenceCompletion::RetryableFailure(wrong_exact),
                    deadline,
                )
                .unwrap(),
            super::RewardCompletionDisposition::IgnoredStale
        );
        let mut wrong_canonical = task.clone();
        wrong_canonical.canonical_nickname.push('x');
        assert_eq!(
            world
                .complete_reward_task(
                    super::RewardPersistenceCompletion::RetryableFailure(wrong_canonical),
                    deadline,
                )
                .unwrap(),
            super::RewardCompletionDisposition::IgnoredStale
        );
        assert_eq!(
            world
                .complete_reward_task(
                    super::RewardPersistenceCompletion::FatalFailure(task),
                    deadline,
                )
                .unwrap(),
            super::RewardCompletionDisposition::TerminalFailure
        );
    }

    #[test]
    fn durable_receipt_key_must_match_the_writer_owned_task_stamp() {
        let (mut world, _owner, room_id, deadline, _clock) =
            prepare_single_reward_persistence("ReceiptStamp", 42_349);
        let task = world
            .take_due_reward_tasks(deadline, 1)
            .unwrap()
            .pop()
            .unwrap();
        let wrong_key_task = super::RewardSettlementTask::for_test(
            task.fence().room_id(),
            task.fence().race_epoch(),
            NonZeroU64::new(task.attempt_id().get()).unwrap(),
            task.user_no(),
            "DifferentRecipient",
            task.proposed_reward(),
        );
        assert_eq!(
            world
                .complete_reward_task(
                    durable_completion_with_key(&task, &wrong_key_task, 82_000),
                    deadline,
                )
                .unwrap(),
            super::RewardCompletionDisposition::TerminalFailure
        );
        let settlement = world.protocol_rooms[&room_id]
            .race_progress
            .settlement
            .as_ref()
            .unwrap();
        assert!(matches!(
            &settlement.finalization,
            super::SettlementFinalization::Failed(failed)
                if failed.reason == super::RewardTerminalReason::RewardReceiptMismatch
        ));
    }

    #[test]
    fn dead_letter_alone_never_satisfies_the_shutdown_drain_barrier() {
        let (mut world, _owner, _room_id, deadline, _clock) =
            prepare_single_reward_persistence("DeadLetterBarrier", 42_350);
        let task = world
            .take_due_reward_tasks(deadline, 1)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(
            world
                .complete_reward_task(
                    super::RewardPersistenceCompletion::FatalFailure(task),
                    deadline,
                )
                .unwrap(),
            super::RewardCompletionDisposition::TerminalFailure
        );

        // Simulate a future invariant regression: the terminal settlement is
        // still authoritative even if its redundant lane index is missing.
        world.reward_lanes.clear();
        let status = world.reward_drain_status().unwrap();
        assert!(status.outstanding_lanes().is_empty());
        assert_eq!(status.terminal_count(), 1);
        assert!(!status.is_drained());
    }

    #[test]
    fn team_full_and_server_owned_states_leave_the_room_unchanged() {
        let mut world = World::default();
        let mut sessions = (0..6)
            .map(|index| {
                register_channel_session(
                    &mut world,
                    &format!("Team{index}"),
                    68,
                    42_001 + index * 10,
                    64,
                )
            })
            .collect::<Vec<_>>();
        let room_id = create_protocol_room(&mut world, &sessions[0], 3);
        for session in &sessions[1..5] {
            join_protocol_room(&mut world, session, room_id, false);
        }
        join_protocol_room(&mut world, &sessions[5], room_id, true);
        for session in &mut sessions {
            drain_batches(&mut session.outbound);
        }

        let red_racer = &sessions[4];
        let before = world.protocol_rooms[&room_id].clone();
        assert!(matches!(
            world.lobby_command(
                red_racer.session,
                LobbyCommandPayload::ChangeTeam(RoomTeam::Blue)
            ),
            Err(WorldError::Lobby(LobbyError::TeamFull {
                team: RoomTeam::Blue
            }))
        ));
        assert_eq!(world.protocol_rooms[&room_id], before);

        let before = world.protocol_rooms[&room_id].clone();
        assert!(matches!(
            world.lobby_command(
                sessions[1].session,
                LobbyCommandPayload::SetSlotState(PlayerSlotState::Preparing)
            ),
            Err(WorldError::Lobby(LobbyError::PreparingStateServerOwned))
        ));
        assert!(matches!(
            world.lobby_command(
                sessions[1].session,
                LobbyCommandPayload::SetSlotState(PlayerSlotState::Observer)
            ),
            Err(WorldError::Lobby(LobbyError::ObserverStateServerOwned))
        ));
        assert!(matches!(
            world.lobby_command(
                sessions[5].session,
                LobbyCommandPayload::SetSlotState(PlayerSlotState::Ready)
            ),
            Err(WorldError::Lobby(LobbyError::HumanRacerRequired))
        ));
        assert!(matches!(
            world.lobby_command(
                sessions[5].session,
                LobbyCommandPayload::ChangeTeam(RoomTeam::Red)
            ),
            Err(WorldError::Lobby(LobbyError::HumanRacerRequired))
        ));
        assert_eq!(world.protocol_rooms[&room_id], before);

        let changed = world
            .lobby_command(
                sessions[0].session,
                LobbyCommandPayload::ChangeTeam(RoomTeam::Red),
            )
            .unwrap();
        assert!(matches!(
            changed,
            LobbyCommandOutcome::TeamChanged {
                team: RoomTeam::Red,
                slot_id: 5,
                ..
            }
        ));
        let owner_packets = sessions[0].outbound.try_recv().unwrap().into_packets();
        assert_eq!(owner_packets.len(), 2);
        assert_eq!(
            logical_packet_hash(&owner_packets[0]),
            adler32::packet_hash(CHANGE_TEAM_REPLY_NAME)
        );
        assert_eq!(
            logical_packet_hash(&owner_packets[1]),
            adler32::packet_hash("GrSlotDataPacket")
        );
    }

    #[test]
    fn master_change_is_authorized_and_stale_generation_is_fenced() {
        let mut world = World::default();
        let owner = register_channel_session(&mut world, "Master", 67, 43_001, 64);
        let guest = register_channel_session(&mut world, "Successor", 67, 43_011, 64);
        let room_id = create_protocol_room(&mut world, &owner, 1);
        join_protocol_room(&mut world, &guest, room_id, false);

        let before = world.protocol_rooms[&room_id].clone();
        assert!(matches!(
            world.lobby_command(
                guest.session,
                LobbyCommandPayload::ChangeMaster(owner.identity.nickname.clone())
            ),
            Err(WorldError::Lobby(LobbyError::NotRoomMaster))
        ));
        assert_eq!(world.protocol_rooms[&room_id], before);

        assert!(matches!(
            world
                .lobby_command(
                    owner.session,
                    LobbyCommandPayload::ChangeMaster(guest.identity.nickname.clone())
                )
                .unwrap(),
            LobbyCommandOutcome::MasterChanged {
                previous_player_id: 0,
                next_player_id: 1,
                ..
            }
        ));
        assert_eq!(world.protocol_rooms[&room_id].room_master, 1);
        assert_eq!(
            world.protocol_rooms[&room_id].members_by_id[0]
                .as_ref()
                .unwrap()
                .player
                .player_type,
            PlayerSlotState::NotReady as i32
        );
        assert_eq!(
            world.protocol_rooms[&room_id].members_by_id[1]
                .as_ref()
                .unwrap()
                .player
                .player_type,
            PlayerSlotState::NotReady as i32
        );

        let _replacement = migrate_channel_session(&mut world, &guest, 43_101, 64);
        let before = world.protocol_rooms[&room_id].clone();
        assert!(matches!(
            world.lobby_command(
                guest.session,
                LobbyCommandPayload::SetSlotState(PlayerSlotState::Ready)
            ),
            Err(WorldError::Identity(IdentityError::StaleSession(session)))
                if session == guest.session
        ));
        assert_eq!(world.protocol_rooms[&room_id], before);
    }

    #[test]
    fn serializer_failure_and_random_selectors_roll_back_start() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "Serialize", 67, 44_001, 64);
        let room_id = create_protocol_room(&mut world, &owner, 1);
        drain_batches(&mut owner.outbound);

        let candidates = vec![40, 0x2222_3333, 1, 0x1111_2222];
        {
            let room = world.protocol_rooms.get_mut(&room_id).unwrap();
            room.settings.track = 1;
            let from_one = room.select_concrete_track(1, &candidates).unwrap();
            room.settings.track = 40;
            let from_forty = room.select_concrete_track(1, &candidates).unwrap();
            assert_eq!(from_one, from_forty);
            assert!(!matches!(from_one, 1 | 40));
        }

        let before = world.protocol_rooms[&room_id].clone();
        let ai_plan = StartRoomPlan::new(
            candidates.clone(),
            vec![AiRaceSpec::try_from([1.0; 6]).unwrap()],
        );
        assert!(matches!(
            world.lobby_command(owner.session, LobbyCommandPayload::StartRoom(ai_plan)),
            Err(WorldError::Lobby(LobbyError::AiParticipantsUnsupported))
        ));
        assert_eq!(world.protocol_rooms[&room_id], before);
        let failure = owner.outbound.try_recv().unwrap().into_packets();
        assert_eq!(failure.len(), 1);
        assert_eq!(
            logical_packet_hash(&failure[0]),
            adler32::packet_hash(START_ROOM_REPLY_NAME)
        );

        let bounded =
            StartRoomPlan::new(candidates.clone(), Vec::new()).with_maximum_payload_length(1);
        assert!(matches!(
            world.lobby_command(owner.session, LobbyCommandPayload::StartRoom(bounded)),
            Err(WorldError::Lobby(LobbyError::RaceStart(
                RaceStartProtocolError::PayloadTooLarge { .. }
            )))
        ));
        assert_eq!(world.protocol_rooms[&room_id], before);
        let failure = owner.outbound.try_recv().unwrap().into_packets();
        assert_eq!(failure.len(), 1);
        assert_eq!(
            logical_packet_hash(&failure[0]),
            adler32::packet_hash(START_ROOM_REPLY_NAME)
        );

        let expected_track = before.select_concrete_track(1, &candidates).unwrap();
        assert!(matches!(
            world
                .lobby_command(
                    owner.session,
                    LobbyCommandPayload::StartRoom(StartRoomPlan::new(candidates, Vec::new()))
                )
                .unwrap(),
            LobbyCommandOutcome::Started {
                race_epoch: 1,
                concrete_track,
                ..
            } if concrete_track == expected_track
        ));
    }

    #[test]
    fn outbound_reservation_failure_rolls_back_without_partial_fanout() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "QueueOwner", 67, 45_001, 1);
        let mut guest = register_channel_session(&mut world, "QueueGuest", 67, 45_011, 1);
        let room_id = create_protocol_room(&mut world, &owner, 1);
        drain_batches(&mut owner.outbound);
        join_protocol_room(&mut world, &guest, room_id, false);
        drain_batches(&mut guest.outbound);

        world
            .lobby_command(
                guest.session,
                LobbyCommandPayload::SetSlotState(PlayerSlotState::Ready),
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        drain_batches(&mut guest.outbound);

        let guest_sender = world.sessions[&guest.session].outbound.clone().unwrap();
        guest_sender
            .try_send(OutboundBatch::single(vec![0xAA]))
            .unwrap();
        let before = world.protocol_rooms[&room_id].clone();
        let epoch_before = world.next_race_epoch;
        assert!(matches!(
            world.lobby_command(
                owner.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(
                    vec![0x1111_2222],
                    Vec::new()
                ))
            ),
            Err(WorldError::Lobby(LobbyError::OutboundUnavailable { session }))
                if session == guest.session
        ));
        assert_eq!(world.protocol_rooms[&room_id], before);
        assert_eq!(world.next_race_epoch, epoch_before);
        assert!(matches!(
            owner.outbound.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert_eq!(
            guest.outbound.try_recv().unwrap().into_packets(),
            vec![vec![0xAA]]
        );
        assert!(matches!(
            guest.outbound.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));

        assert!(matches!(
            world
                .lobby_command(
                    owner.session,
                    LobbyCommandPayload::StartRoom(StartRoomPlan::new(
                        vec![0x1111_2222],
                        Vec::new()
                    ))
                )
                .unwrap(),
            LobbyCommandOutcome::Started { race_epoch: 1, .. }
        ));
        assert_eq!(world.next_race_epoch.map(GlobalRaceEpoch::get), Some(2));
    }

    #[test]
    fn race_epochs_are_process_global_across_room_id_reuse() {
        let mut world = World::default();
        let first = register_channel_session(&mut world, "EpochFirst", 67, 45_101, 64);
        let second = register_channel_session(&mut world, "EpochSecond", 67, 45_111, 64);

        let reused_room_id = create_protocol_room(&mut world, &first, 1);
        assert!(matches!(
            world
                .lobby_command(
                    first.session,
                    LobbyCommandPayload::StartRoom(StartRoomPlan::new(
                        vec![0x1111_2222],
                        Vec::new()
                    ))
                )
                .unwrap(),
            LobbyCommandOutcome::Started {
                room_id,
                race_epoch: 1,
                ..
            } if room_id == reused_room_id
        ));
        world
            .room_protocol(first.session, RoomCommandPayload::Leave)
            .unwrap();
        assert!(!world.protocol_rooms.contains_key(&reused_room_id));

        let second_room_id = create_protocol_room(&mut world, &second, 1);
        assert_eq!(second_room_id, reused_room_id);
        assert!(matches!(
            world
                .lobby_command(
                    second.session,
                    LobbyCommandPayload::StartRoom(StartRoomPlan::new(
                        vec![0x1111_2222],
                        Vec::new()
                    ))
                )
                .unwrap(),
            LobbyCommandOutcome::Started {
                room_id,
                race_epoch: 2,
                ..
            } if room_id == reused_room_id
        ));
        assert_eq!(world.next_race_epoch.map(GlobalRaceEpoch::get), Some(3));
    }

    #[test]
    fn maximum_global_race_epoch_is_allocated_once_then_exhausted() {
        let mut world = World {
            next_race_epoch: GlobalRaceEpoch::new(u64::MAX),
            ..World::default()
        };
        let first = register_channel_session(&mut world, "EpochMax", 67, 45_201, 64);
        let second = register_channel_session(&mut world, "EpochExhausted", 67, 45_211, 64);
        let first_room = create_protocol_room(&mut world, &first, 1);
        let second_room = create_protocol_room(&mut world, &second, 1);

        assert!(matches!(
            world
                .lobby_command(
                    first.session,
                    LobbyCommandPayload::StartRoom(StartRoomPlan::new(
                        vec![0x1111_2222],
                        Vec::new()
                    ))
                )
                .unwrap(),
            LobbyCommandOutcome::Started {
                room_id,
                race_epoch: u64::MAX,
                ..
            } if room_id == first_room
        ));
        assert_eq!(world.next_race_epoch, None);

        let before = world.protocol_rooms[&second_room].clone();
        assert!(matches!(
            world.lobby_command(
                second.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new()))
            ),
            Err(WorldError::Lobby(LobbyError::RaceEpochExhausted))
        ));
        assert_eq!(world.protocol_rooms[&second_room], before);
    }

    #[test]
    fn reward_attempt_ids_are_unique_nonzero_and_maximum_is_allocated_once() {
        let mut world = World::default();
        let first = world.allocate_reward_attempt_id().unwrap();
        let second = world.allocate_reward_attempt_id().unwrap();
        assert_ne!(first, second);
        assert_ne!(first.0.get(), 0);
        assert_ne!(second.0.get(), 0);

        world.next_reward_attempt_id = NonZeroU64::new(u64::MAX);
        assert_eq!(
            world.allocate_reward_attempt_id(),
            Some(super::RewardAttemptId(NonZeroU64::new(u64::MAX).unwrap()))
        );
        assert_eq!(world.allocate_reward_attempt_id(), None);
    }

    #[tokio::test]
    async fn concurrent_joins_are_serialized_into_unique_slots() {
        let (world, task) = WorldHandle::spawn(64).expect("nonzero World mailbox capacity");
        let room = world.create_room().await.unwrap();
        let mut joins = tokio::task::JoinSet::new();

        for index in 0..32 {
            let world = world.clone();
            joins.spawn(async move { world.join_room(room, format!("rider-{index}")).await });
        }

        let mut slots = HashSet::new();
        let mut successful = 0;
        while let Some(result) = joins.join_next().await {
            match result.unwrap() {
                Ok(slot) => {
                    successful += 1;
                    assert!(slots.insert(slot));
                }
                Err(WorldError::Room(RoomError::Full(id))) => assert_eq!(id, room.0),
                Err(error) => panic!("unexpected join error: {error}"),
            }
        }

        assert_eq!(successful, ROOM_CAPACITY);
        assert_eq!(
            world.room_snapshot(room).await.unwrap().slots.len(),
            ROOM_CAPACITY
        );
        world.shutdown().await.unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn migration_cancels_old_owner_and_rejects_its_queued_mutation() {
        let (world, task) = WorldHandle::spawn(64).expect("nonzero World mailbox capacity");
        let peer = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 50_000);
        let (source, source_cancelled, _outbound) = world
            .register_login_session(peer, WireOperationGate::new())
            .await
            .unwrap();
        let destination = world
            .register_session(SocketAddr::new(peer.ip(), 50_001))
            .await
            .unwrap();
        let identity = world.claim_identity(source, "Rider").await.unwrap();
        let room = world.create_room().await.unwrap();
        let token = MigrationToken::new(0x5136).unwrap();
        world
            .begin_migration(
                source,
                ChannelBinding {
                    channel_id: 12,
                    game_type: 67,
                },
                token,
                Instant::now(),
            )
            .await
            .unwrap();

        // Queue the stale packet mutation directly behind the transfer. FIFO
        // ordering from one sender makes this the actor equivalent of bytes
        // already read by the old socket while PqChannelMovein completes.
        let (migration_reply, migration_response) = oneshot::channel();
        world
            .sender
            .send(WorldCommand::CompleteMigration {
                destination,
                user_no: identity.user_no,
                channel_id: 12,
                token,
                now: Instant::now(),
                reply: migration_reply,
            })
            .await
            .unwrap();
        let (mutation_reply, mutation_response) = oneshot::channel();
        world
            .sender
            .send(WorldCommand::JoinRoomForSession {
                room,
                session: source,
                reply: mutation_reply,
            })
            .await
            .unwrap();

        let completion = migration_response.await.unwrap().unwrap();
        assert_eq!(completion.previous_owner, Some(source));
        tokio::time::timeout(Duration::from_millis(100), source_cancelled)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            mutation_response.await.unwrap(),
            Err(WorldError::Identity(IdentityError::StaleSession(id))) if id == source
        ));
        assert!(
            world
                .room_snapshot(room)
                .await
                .unwrap()
                .slots
                .iter()
                .all(Option::is_none)
        );

        let slot = world
            .join_room_for_session(room, destination)
            .await
            .unwrap();
        assert_eq!(slot, super::SlotId(0));
        world.session_closed(source).await.unwrap();
        world.session_closed(destination).await.unwrap();
        world.shutdown().await.unwrap();
        task.await.unwrap().unwrap();
    }

    #[test]
    fn protocol_room_master_changes_only_when_the_master_leaves() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "Owner", 67, 41_000, 16);
        let mut second = register_channel_session(&mut world, "Second", 67, 42_000, 16);
        let mut third = register_channel_session(&mut world, "Third", 67, 43_000, 16);

        world
            .room_protocol(
                owner.session,
                RoomCommandPayload::Create {
                    request: create_request("Three", 1),
                    participant: room_participant(),
                },
            )
            .unwrap();
        assert_eq!(take_single_packet(&mut owner.outbound)[4], 1);
        let room_id = world.protocol_room_by_user[&owner.identity.user_no];

        for rider in [&mut second, &mut third] {
            world
                .room_protocol(
                    rider.session,
                    RoomCommandPayload::Join {
                        request: join_request(room_id),
                        participant: room_participant(),
                    },
                )
                .unwrap();
            assert_eq!(take_single_packet(&mut rider.outbound)[4], 0);
        }
        assert_eq!(world.protocol_rooms[&room_id].room_master, 0);

        world
            .room_protocol(second.session, RoomCommandPayload::Leave)
            .unwrap();
        assert_eq!(take_single_packet(&mut second.outbound)[4], 1);
        let _owner_slot_update = take_single_packet(&mut owner.outbound);
        let _third_slot_update = take_single_packet(&mut third.outbound);
        assert_eq!(world.protocol_rooms[&room_id].room_master, 0);

        world
            .room_protocol(owner.session, RoomCommandPayload::Leave)
            .unwrap();
        assert_eq!(take_single_packet(&mut owner.outbound)[4], 1);
        let _third_slot_update = take_single_packet(&mut third.outbound);
        let room = &world.protocol_rooms[&room_id];
        assert_eq!(room.room_master, 2);
        let successor = room.members_by_id[2].as_ref().unwrap();
        assert_eq!(successor.user_no, third.identity.user_no);
        assert_eq!(successor.player.player_type, 2);
        assert_eq!(successor.player.ranking, 0);
    }

    #[test]
    fn protocol_rooms_are_channel_isolated_and_filter_before_paging() {
        let mut world = World::default();
        let mut speed_owner = register_channel_session(&mut world, "Speed", 67, 44_000, 16);
        let mut item_rider = register_channel_session(&mut world, "Item", 68, 45_000, 16);
        let mut speed_rider = register_channel_session(&mut world, "SpeedTwo", 67, 46_000, 16);

        world
            .room_protocol(
                speed_owner.session,
                RoomCommandPayload::Create {
                    request: create_request("Speed room", 1),
                    participant: room_participant(),
                },
            )
            .unwrap();
        let _ = take_single_packet(&mut speed_owner.outbound);
        let room_id = world.protocol_room_by_user[&speed_owner.identity.user_no];

        world
            .room_protocol(
                item_rider.session,
                RoomCommandPayload::List(ChGetRoomListRequest {
                    page: 0,
                    room_list_type: 3,
                    room_list_mode: 0,
                }),
            )
            .unwrap();
        assert_eq!(
            room_list_count(&take_single_packet(&mut item_rider.outbound)),
            0
        );
        world
            .room_protocol(
                item_rider.session,
                RoomCommandPayload::Join {
                    request: join_request(room_id),
                    participant: room_participant(),
                },
            )
            .unwrap();
        assert_eq!(take_single_packet(&mut item_rider.outbound)[4], 1);

        world
            .room_protocol(
                speed_rider.session,
                RoomCommandPayload::List(ChGetRoomListRequest {
                    page: 0,
                    room_list_type: 1,
                    room_list_mode: 0,
                }),
            )
            .unwrap();
        assert_eq!(
            room_list_count(&take_single_packet(&mut speed_rider.outbound)),
            1
        );
        world
            .room_protocol(
                speed_rider.session,
                RoomCommandPayload::List(ChGetRoomListRequest {
                    page: 0,
                    room_list_type: 3,
                    room_list_mode: 0,
                }),
            )
            .unwrap();
        assert_eq!(
            room_list_count(&take_single_packet(&mut speed_rider.outbound)),
            0
        );
    }

    #[test]
    fn same_game_type_rooms_remain_isolated_by_channel_id() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "ChannelOwner", 67, 46_100, 16);
        let other_source = register_channel_session(&mut world, "OtherChannel", 67, 46_200, 16);
        let mut other = migrate_channel_session_to(
            &mut world,
            &other_source,
            ChannelBinding {
                channel_id: 12,
                game_type: 67,
            },
            46_300,
            16,
        );
        world
            .room_protocol(
                owner.session,
                RoomCommandPayload::Create {
                    request: create_request("Channel 67", 1),
                    participant: room_participant(),
                },
            )
            .unwrap();
        let _ = take_single_packet(&mut owner.outbound);
        let room_id = world.protocol_room_by_user[&owner.identity.user_no];

        world
            .room_protocol(
                other.session,
                RoomCommandPayload::List(ChGetRoomListRequest {
                    page: 0,
                    room_list_type: 1,
                    room_list_mode: 0,
                }),
            )
            .unwrap();
        assert_eq!(room_list_count(&take_single_packet(&mut other.outbound)), 0);
        world
            .room_protocol(
                other.session,
                RoomCommandPayload::Join {
                    request: join_request(room_id),
                    participant: room_participant(),
                },
            )
            .unwrap();
        assert_eq!(take_single_packet(&mut other.outbound)[4], 1);
    }

    #[test]
    fn stale_generation_cannot_leave_or_replace_its_protocol_room() {
        let mut world = World::default();
        let mut source = register_channel_session(&mut world, "Migrating", 67, 47_000, 16);
        world
            .room_protocol(
                source.session,
                RoomCommandPayload::Create {
                    request: create_request("Stable", 1),
                    participant: room_participant(),
                },
            )
            .unwrap();
        let _ = take_single_packet(&mut source.outbound);
        let room_id = world.protocol_room_by_user[&source.identity.user_no];
        let mut destination = migrate_channel_session(&mut world, &source, 47_100, 16);

        assert!(matches!(
            world.room_protocol(source.session, RoomCommandPayload::Leave),
            Err(WorldOperationError::Command(WorldError::Identity(
                IdentityError::StaleSession(session)
            )))
                if session == source.session
        ));
        assert_eq!(
            world.protocol_room_by_user.get(&source.identity.user_no),
            Some(&room_id)
        );
        assert_eq!(
            world.protocol_rooms[&room_id].members_by_id[0]
                .as_ref()
                .unwrap()
                .user_no,
            destination.identity.user_no
        );

        world
            .room_protocol(destination.session, RoomCommandPayload::Leave)
            .unwrap();
        assert_eq!(take_single_packet(&mut destination.outbound)[4], 1);
        assert!(!world.protocol_rooms.contains_key(&room_id));
    }

    #[test]
    fn committed_equipment_snapshot_is_fenced_and_fanned_out_to_room_peers() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "Equipped", 67, 47_110, 16);
        let mut peer = register_channel_session(&mut world, "Witness", 67, 47_120, 16);
        world
            .room_protocol(
                owner.session,
                RoomCommandPayload::Create {
                    request: create_request("Equipment", 1),
                    participant: room_participant(),
                },
            )
            .unwrap();
        let _ = take_single_packet(&mut owner.outbound);
        let room_id = world.protocol_room_by_user[&owner.identity.user_no];
        world
            .room_protocol(
                peer.session,
                RoomCommandPayload::Join {
                    request: join_request(room_id),
                    participant: room_participant(),
                },
            )
            .unwrap();
        let _ = take_single_packet(&mut peer.outbound);

        let committed = [0x5a; RIDER_ITEM_SNAPSHOT_WIRE_LENGTH];
        world
            .publish_room_equipment(owner.session, committed)
            .unwrap();
        let packet = take_single_packet(&mut peer.outbound);
        assert_eq!(
            u32::from_le_bytes(packet[..4].try_into().unwrap()),
            adler32::packet_hash("GrSlotItemOnPacket")
        );
        assert_eq!(i32::from_le_bytes(packet[4..8].try_into().unwrap()), 0);
        assert_eq!(&packet[8..], committed);
        assert!(owner.outbound.try_recv().is_err());
        assert_eq!(
            world.protocol_rooms[&room_id].members_by_id[0]
                .as_ref()
                .unwrap()
                .player
                .rider_item_snapshot,
            committed
        );

        let mut destination = migrate_channel_session(&mut world, &owner, 47_130, 16);
        assert!(matches!(
            world.publish_room_equipment(owner.session, [0x33; 65]),
            Err(WorldOperationError::Command(WorldError::Identity(
                IdentityError::StaleSession(session)
            )))
                if session == owner.session
        ));
        assert!(peer.outbound.try_recv().is_err());
        assert_eq!(
            world.protocol_rooms[&room_id].members_by_id[0]
                .as_ref()
                .unwrap()
                .player
                .rider_item_snapshot,
            committed
        );

        world
            .publish_room_equipment(destination.session, [0x44; 65])
            .unwrap();
        let _ = take_single_packet(&mut peer.outbound);
        assert!(destination.outbound.try_recv().is_err());
    }

    #[test]
    fn live_race_equipment_change_updates_the_serialized_game_result() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "LiveRaceEquipment", 67, 47_140, 64);
        let room_id = create_protocol_room(&mut world, &owner, 1);
        drain_batches(&mut owner.outbound);
        world
            .lobby_command(
                owner.session,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new())),
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        force_running(&mut world, room_id);

        let character_id: u16 = 0x1234;
        let kart_id: u16 = 0x2345;
        let mut snapshot = [0u8; RIDER_ITEM_SNAPSHOT_WIRE_LENGTH];
        snapshot[..2].copy_from_slice(&character_id.to_le_bytes());
        snapshot[4..6].copy_from_slice(&kart_id.to_le_bytes());
        world
            .publish_room_equipment(owner.session, snapshot)
            .unwrap();
        let admission = world.protocol_rooms[&room_id]
            .frozen_race
            .as_ref()
            .unwrap()
            .participants[0]
            .result
            .unwrap();
        assert_eq!(admission.character_id, character_id);
        assert_eq!(admission.kart_id, kart_id);
        assert!(owner.outbound.try_recv().is_err());

        let now = Instant::now();
        let clock = ServerClock::new();
        world
            .race_command_with_clock(
                owner.session,
                game_control_request_with_value(2, 456),
                now,
                &clock,
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        let deadline = world.protocol_rooms[&room_id]
            .race_progress
            .settlement
            .as_ref()
            .unwrap()
            .deadline;
        world.advance_loading(deadline, &clock);
        assert_eq!(complete_all_due_rewards(&mut world, deadline).len(), 1);
        world.advance_loading(deadline + Duration::from_millis(1), &clock);

        let packets = take_packets(&mut owner.outbound);
        let result = packets
            .iter()
            .find(|packet| {
                logical_packet_hash(packet) == adler32::packet_hash(GAME_RESULT_PACKET_NAME)
            })
            .expect("settlement must include GameResultPacket");
        assert_eq!(i32::from_le_bytes(result[5..9].try_into().unwrap()), 1);
        let human_record = 9;
        assert_eq!(
            u16::from_le_bytes(
                result[human_record + 9..human_record + 11]
                    .try_into()
                    .unwrap()
            ),
            kart_id
        );
        assert_eq!(
            u16::from_le_bytes(
                result[human_record + 85..human_record + 87]
                    .try_into()
                    .unwrap()
            ),
            character_id
        );
    }

    #[test]
    fn cross_channel_migration_removes_room_membership_and_fans_out_slots() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "Switching", 67, 47_200, 16);
        let mut peer = register_channel_session(&mut world, "Remaining", 67, 47_300, 16);
        world
            .room_protocol(
                owner.session,
                RoomCommandPayload::Create {
                    request: create_request("Channel", 1),
                    participant: room_participant(),
                },
            )
            .unwrap();
        let _ = take_single_packet(&mut owner.outbound);
        let room_id = world.protocol_room_by_user[&owner.identity.user_no];
        world
            .room_protocol(
                peer.session,
                RoomCommandPayload::Join {
                    request: join_request(room_id),
                    participant: room_participant(),
                },
            )
            .unwrap();
        let _ = take_single_packet(&mut peer.outbound);

        let mut destination = migrate_channel_session_to(
            &mut world,
            &owner,
            ChannelBinding {
                channel_id: 12,
                game_type: 67,
            },
            47_400,
            16,
        );
        let peer_update = take_single_packet(&mut peer.outbound);
        assert_eq!(
            u32::from_le_bytes(peer_update[..4].try_into().unwrap()),
            adler32::packet_hash("GrSlotDataPacket")
        );
        assert!(
            !world
                .protocol_room_by_user
                .contains_key(&owner.identity.user_no)
        );
        assert_eq!(world.protocol_rooms[&room_id].room_master, 1);

        world
            .room_protocol(destination.session, RoomCommandPayload::FirstState)
            .unwrap();
        assert!(destination.outbound.try_recv().is_err());
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "the atomicity regression asserts every identity, room, queue, cancellation, and lifecycle side effect"
    )]
    fn cross_channel_peer_backpressure_aborts_before_ack_or_owner_publication() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "AtomicSwitch", 67, 47_500, 16);
        let mut peer = register_channel_session(&mut world, "AtomicPeer", 67, 47_600, 1);
        world
            .room_protocol(
                owner.session,
                RoomCommandPayload::Create {
                    request: create_request("Atomic Channel", 1),
                    participant: room_participant(),
                },
            )
            .unwrap();
        drain_batches(&mut owner.outbound);
        let room_id = world.protocol_room_by_user[&owner.identity.user_no];
        world
            .room_protocol(
                peer.session,
                RoomCommandPayload::Join {
                    request: join_request(room_id),
                    participant: room_participant(),
                },
            )
            .unwrap();
        drain_batches(&mut peer.outbound);
        enter_myroom(
            &mut world,
            &owner.identity,
            &owner.identity,
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::LOCALHOST,
        );
        enter_myroom(
            &mut world,
            &peer.identity,
            &owner.identity,
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::LOCALHOST,
        );
        world.identity_lifecycle.clear();
        let protocol_before = world.protocol_rooms[&room_id].clone();
        let myroom_before = world
            .myroom
            .canonical_identity_if_tracked(owner.identity.user_no)
            .unwrap();

        let peer_sender = world.sessions[&peer.session].outbound.clone().unwrap();
        peer_sender
            .try_send(OutboundBatch::single(vec![0xFE]))
            .unwrap();
        let (source_cancel, mut source_cancelled) = oneshot::channel();
        world.sessions.get_mut(&owner.session).unwrap().cancellation = Some(source_cancel);

        let channel = ChannelBinding {
            channel_id: 12,
            game_type: 67,
        };
        let token = MigrationToken::new(47_700).unwrap();
        let now = Instant::now();
        world
            .identities
            .begin_migration(owner.session, channel, token, now)
            .unwrap();
        let (destination_sender, mut destination_outbound) = mpsc::channel(1);
        let destination = world
            .register_session(
                SocketAddr::new(owner.identity.source_ip, 47_700),
                None,
                Some(destination_sender),
            )
            .unwrap();
        let preflight = world
            .preflight_migration(
                destination,
                owner.identity.user_no,
                channel.channel_id,
                token,
                now,
            )
            .unwrap();

        assert!(matches!(
            world.complete_preflighted_migration_with_acknowledgement(
                preflight,
                None,
                MigrationAcknowledgement::Ordered(vec![0xAC]),
                None,
                now,
            ),
            Err(WorldOperationError::Command(
                WorldError::MigrationPublicationUnavailable { session }
            )) if session == peer.session
        ));
        assert_eq!(
            world.identities.authorize(owner.session).unwrap(),
            owner.identity
        );
        assert!(matches!(
            world.identities.authorize(destination),
            Err(IdentityError::UnauthenticatedSession(session)) if session == destination
        ));
        assert!(world.identities.admit_operation(owner.session).is_ok());
        assert_eq!(
            source_cancelled.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        );
        assert!(destination_outbound.try_recv().is_err());
        assert_eq!(
            peer.outbound.try_recv().unwrap().into_packets(),
            vec![vec![0xFE]]
        );
        assert_eq!(world.protocol_rooms[&room_id], protocol_before);
        assert_eq!(
            world.protocol_room_by_user.get(&owner.identity.user_no),
            Some(&room_id)
        );
        assert_eq!(
            world
                .myroom
                .canonical_identity_if_tracked(owner.identity.user_no)
                .unwrap(),
            myroom_before
        );
        assert!(world.identity_lifecycle.is_empty());
    }

    #[test]
    fn deleted_room_ids_are_reused_and_invalid_profiles_do_not_commit() {
        let mut world = World::default();
        let mut first = register_channel_session(&mut world, "First", 67, 48_000, 16);
        world
            .room_protocol(
                first.session,
                RoomCommandPayload::Create {
                    request: create_request("Reusable", 1),
                    participant: room_participant(),
                },
            )
            .unwrap();
        let _ = take_single_packet(&mut first.outbound);
        let first_room = world.protocol_room_by_user[&first.identity.user_no];

        world
            .room_protocol(
                first.session,
                RoomCommandPayload::Create {
                    request: create_request("Duplicate", 1),
                    participant: room_participant(),
                },
            )
            .unwrap();
        assert_eq!(take_single_packet(&mut first.outbound)[4], 0);
        assert_eq!(world.protocol_rooms.len(), 1);
        world
            .room_protocol(first.session, RoomCommandPayload::Leave)
            .unwrap();
        let _ = take_single_packet(&mut first.outbound);

        let mut second = register_channel_session(&mut world, "SecondFresh", 67, 49_000, 16);
        world
            .room_protocol(
                second.session,
                RoomCommandPayload::Create {
                    request: create_request("Reused", 1),
                    participant: room_participant(),
                },
            )
            .unwrap();
        let _ = take_single_packet(&mut second.outbound);
        assert_eq!(
            world.protocol_room_by_user[&second.identity.user_no],
            first_room
        );

        let mut invalid = register_channel_session(&mut world, "Invalid", 67, 50_000, 16);
        let mut participant = room_participant();
        participant.player.card = "x".repeat(129);
        assert!(matches!(
            world.room_protocol(
                invalid.session,
                RoomCommandPayload::Create {
                    request: create_request("Invalid", 1),
                    participant,
                },
            ),
            Err(WorldOperationError::Command(WorldError::RoomProtocol(
                RoomProtocolError::LimitExceeded {
                    field: "rider card",
                    ..
                }
            )))
        ));
        assert!(
            !world
                .protocol_room_by_user
                .contains_key(&invalid.identity.user_no)
        );
        assert!(invalid.outbound.try_recv().is_err());
    }

    #[test]
    fn full_outbound_queues_evict_room_members_to_a_fixed_point() {
        let mut world = World::default();
        let owner = register_channel_session(&mut world, "SlowOwner", 67, 51_000, 1);
        let joiner = register_channel_session(&mut world, "SlowJoiner", 67, 52_000, 1);
        world
            .room_protocol(
                owner.session,
                RoomCommandPayload::Create {
                    request: create_request("Slow", 1),
                    participant: room_participant(),
                },
            )
            .unwrap();
        let room_id = world.protocol_room_by_user[&owner.identity.user_no];
        world
            .room_protocol(
                joiner.session,
                RoomCommandPayload::Join {
                    request: join_request(room_id),
                    participant: room_participant(),
                },
            )
            .unwrap();

        // Both one-item queues still contain their direct create/join replies.
        // GrFirst fan-out overflows both, and each eviction can enqueue further
        // cleanup broadcasts. The delivery work queue must converge safely.
        world
            .room_protocol(joiner.session, RoomCommandPayload::FirstState)
            .unwrap();
        assert!(!world.sessions.contains_key(&owner.session));
        assert!(!world.sessions.contains_key(&joiner.session));
        assert!(!world.protocol_rooms.contains_key(&room_id));
        assert!(world.protocol_room_by_user.is_empty());
        assert!(
            world
                .identities
                .active_identity_by_user_no(owner.identity.user_no)
                .is_none()
        );
        assert!(
            world
                .identities
                .active_identity_by_user_no(joiner.identity.user_no)
                .is_none()
        );
    }

    #[tokio::test]
    async fn messenger_identity_lifecycle_is_committed_before_world_replies() {
        let (messenger, messenger_task) =
            MessengerServiceHandle::spawn(MessengerRuntimeConfig::default()).unwrap();
        let (world, world_task) = WorldHandle::spawn_with_messenger(64, messenger.clone());
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);

        let source = world
            .register_session(SocketAddr::new(ip, 53_000))
            .await
            .unwrap();
        let claimed = world.claim_identity(source, "Lifecycle").await.unwrap();
        assert_eq!(messenger.snapshot().await.unwrap().announced_identities, 1);

        let channel = ChannelBinding {
            channel_id: 7,
            game_type: 67,
        };
        let token = MigrationToken::new(777).unwrap();
        world
            .begin_migration(source, channel, token, Instant::now())
            .await
            .unwrap();
        let destination = world
            .register_session(SocketAddr::new(ip, 53_001))
            .await
            .unwrap();
        let migrated = world
            .complete_migration(
                destination,
                claimed.user_no,
                channel.channel_id,
                token,
                Instant::now(),
            )
            .await
            .unwrap();
        assert!(migrated.binding.generation.get() > claimed.generation.get());
        assert_eq!(messenger.snapshot().await.unwrap().announced_identities, 1);

        // The old login transport closes after ownership moved. Its stale
        // generation must not release the newly published messenger identity.
        world.session_closed(source).await.unwrap();
        assert_eq!(messenger.snapshot().await.unwrap().announced_identities, 1);
        world.session_closed(destination).await.unwrap();
        assert_eq!(messenger.snapshot().await.unwrap().announced_identities, 0);

        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
        messenger.shutdown().await.unwrap();
        messenger_task.await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_session_drain_flushes_exact_sidecar_releases_before_reply() {
        let (messenger, messenger_task) =
            MessengerServiceHandle::spawn(MessengerRuntimeConfig::default()).unwrap();
        let (world, world_task) = WorldHandle::spawn_with_messenger(16, messenger.clone());
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let now = Instant::now();
        let ownerless_session = world
            .register_session(SocketAddr::new(ip, 53_100))
            .await
            .unwrap();
        world
            .claim_identity(ownerless_session, "DrainSidecarOwnerless")
            .await
            .unwrap();
        let connected_session = world
            .register_session(SocketAddr::new(ip, 53_101))
            .await
            .unwrap();
        world
            .claim_identity(connected_session, "DrainSidecarConnected")
            .await
            .unwrap();
        world
            .begin_migration(
                ownerless_session,
                ChannelBinding {
                    channel_id: 67,
                    game_type: 67,
                },
                MigrationToken::new(0xD513).unwrap(),
                now,
            )
            .await
            .unwrap();
        world.session_closed(ownerless_session).await.unwrap();
        assert_eq!(messenger.snapshot().await.unwrap().announced_identities, 2);

        world.quiesce().await.unwrap();
        world.drain_sessions().await.unwrap();
        assert_eq!(world.session_count().await.unwrap(), 0);
        assert_eq!(messenger.snapshot().await.unwrap().announced_identities, 0);
        assert!(matches!(
            world.authorize_identity(connected_session).await,
            Err(WorldError::Identity(
                IdentityError::UnauthenticatedSession(id)
            )) if id == connected_session
        ));

        world.session_closed(ownerless_session).await.unwrap();
        world.session_closed(connected_session).await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
        messenger.shutdown().await.unwrap();
        messenger_task.await.unwrap();
    }

    #[tokio::test]
    async fn quiesce_permanently_closes_session_registration() {
        let (world, world_task) = WorldHandle::spawn(16).expect("nonzero World mailbox capacity");
        let peer = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 53_102);

        world.quiesce().await.unwrap();
        assert!(matches!(
            world.register_session(peer).await,
            Err(WorldError::SessionRegistrationClosed)
        ));

        world.drain_sessions().await.unwrap();
        assert!(matches!(
            world
                .register_login_session(peer, WireOperationGate::new())
                .await,
            Err(WorldError::SessionRegistrationClosed)
        ));
        assert_eq!(world.session_count().await.unwrap(), 0);

        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn udp_lifecycle_is_committed_before_replies_and_limits_world_capacity() {
        let messenger_config = MessengerRuntimeConfig {
            max_identities: 3,
            ..MessengerRuntimeConfig::default()
        };
        let (messenger, messenger_task) = MessengerServiceHandle::spawn(messenger_config).unwrap();
        let game_socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let p2p_socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let udp_config = UdpRuntimeConfig {
            maximum_active_identities: 1,
            ..UdpRuntimeConfig::default()
        };
        let udp_runtime = UdpRuntime::spawn(game_socket, p2p_socket, udp_config).unwrap();
        let udp = udp_runtime.service();
        let (world, world_task) = WorldHandle::spawn_with_services(
            64,
            64,
            messenger.clone(),
            udp.clone(),
            ServerClock::new(),
        );
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);

        let source = world
            .register_session(SocketAddr::new(ip, 56_000))
            .await
            .unwrap();
        let claimed = world.claim_identity(source, "UdpLifecycle").await.unwrap();
        udp.dispatch(game_slot_request(&claimed, 58_000))
            .await
            .unwrap();

        // The world must use the minimum of all sidecar capacities. The
        // messenger allows three identities, but the UDP mirror allows one.
        let destination = world
            .register_session(SocketAddr::new(ip, 56_001))
            .await
            .unwrap();
        assert!(matches!(
            world.claim_identity(destination, "CapacityWaiter").await,
            Err(WorldError::IdentityLimitReached { maximum: 1 })
        ));

        let channel = ChannelBinding {
            channel_id: 7,
            game_type: 67,
        };
        let token = MigrationToken::new(778).unwrap();
        world
            .begin_migration(source, channel, token, Instant::now())
            .await
            .unwrap();
        let migrated = world
            .complete_migration(
                destination,
                claimed.user_no,
                channel.channel_id,
                token,
                Instant::now(),
            )
            .await
            .unwrap();

        // Completion replies only after the UDP mirror advances. The prior
        // exact generation is fenced, while the replacement binds normally.
        assert!(matches!(
            udp.dispatch(game_slot_request(&claimed, 58_000)).await,
            Err(UdpServiceError::EndpointState(
                UdpEndpointStateError::StaleGeneration { .. }
            ))
        ));
        udp.dispatch(game_slot_request(&migrated.binding, 58_000))
            .await
            .unwrap();

        // Closing the stale transport cannot release the migrated owner.
        world.session_closed(source).await.unwrap();
        udp.dispatch(game_slot_request(&migrated.binding, 58_000))
            .await
            .unwrap();

        // Closing the current owner publishes the exact release before reply.
        world.session_closed(destination).await.unwrap();
        assert!(matches!(
            udp.dispatch(game_slot_request(&migrated.binding, 58_000))
                .await,
            Err(UdpServiceError::EndpointState(
                UdpEndpointStateError::InactiveAccount { .. }
            ))
        ));

        let replacement_session = world
            .register_session(SocketAddr::new(ip, 56_002))
            .await
            .unwrap();
        let replacement = world
            .claim_identity(replacement_session, "CapacityWaiter")
            .await
            .unwrap();
        udp.dispatch(game_slot_request(&replacement, 58_001))
            .await
            .unwrap();

        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
        udp_runtime.shutdown().await;
        messenger.shutdown().await.unwrap();
        messenger_task.await.unwrap();
    }

    #[test]
    fn myroom_first_state_queues_exact_member_or_all_empty_snapshot() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "FirstOwner", 67, 56_010, 8);
        let mut visitor = register_channel_session(&mut world, "FirstVisitor", 67, 56_020, 8);
        let mut outsider = register_channel_session(&mut world, "FirstOutsider", 67, 56_030, 8);
        enter_myroom(
            &mut world,
            &owner.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 10),
            Ipv4Addr::new(192, 0, 2, 10),
        );
        enter_myroom(
            &mut world,
            &visitor.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 11),
            Ipv4Addr::new(192, 0, 2, 10),
        );
        let (owner_prepared, owner_projection) = prepare_test_myroom_command(&world, owner.session);
        let member_packet =
            serialize_slot_data(&owner_projection.unwrap().snapshot().slots).unwrap();
        let (visitor_prepared, _) = prepare_test_myroom_command(&world, visitor.session);
        let (outsider_prepared, _) = prepare_test_myroom_command(&world, outsider.session);

        world
            .myroom_command(
                owner.session,
                MyRoomCommandPayload::FirstState,
                owner_prepared,
            )
            .unwrap();
        world
            .myroom_command(
                visitor.session,
                MyRoomCommandPayload::FirstState,
                visitor_prepared,
            )
            .unwrap();
        world
            .myroom_command(
                outsider.session,
                MyRoomCommandPayload::FirstState,
                outsider_prepared,
            )
            .unwrap();

        assert_eq!(take_single_packet(&mut owner.outbound), member_packet);
        assert_eq!(take_single_packet(&mut visitor.outbound), member_packet);
        let empty: [MyRoomSlot; MYROOM_SLOT_COUNT] = std::array::from_fn(|_| MyRoomSlot::Empty);
        assert_eq!(
            take_single_packet(&mut outsider.outbound),
            serialize_slot_data(&empty).unwrap()
        );
        world.myroom.audit_invariants().unwrap();
    }

    #[test]
    fn myroom_secede_is_always_success_and_preserves_owner_tombstone() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "SecedeOwner", 67, 56_040, 8);
        let mut visitor = register_channel_session(&mut world, "SecedeVisitor", 67, 56_050, 8);
        let mut outsider = register_channel_session(&mut world, "SecedeOutsider", 67, 56_060, 8);
        enter_myroom(
            &mut world,
            &owner.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 20),
            Ipv4Addr::new(192, 0, 2, 20),
        );
        enter_myroom(
            &mut world,
            &visitor.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 21),
            Ipv4Addr::new(192, 0, 2, 20),
        );

        let revision = world.myroom.revision();
        let (outsider_prepared, _) = prepare_test_myroom_command(&world, outsider.session);
        world
            .myroom_command(
                outsider.session,
                MyRoomCommandPayload::Secede,
                outsider_prepared,
            )
            .unwrap();
        assert_eq!(
            take_single_packet(&mut outsider.outbound),
            serialize_secede_reply()
        );
        assert_eq!(world.myroom.revision(), revision);
        assert!(owner.outbound.try_recv().is_err());
        assert!(visitor.outbound.try_recv().is_err());

        let (owner_prepared, owner_projection) = prepare_test_myroom_command(&world, owner.session);
        world
            .myroom_command(owner.session, MyRoomCommandPayload::Secede, owner_prepared)
            .unwrap();
        assert_eq!(
            take_single_packet(&mut owner.outbound),
            serialize_secede_reply()
        );
        let tombstone_packet = take_single_packet(&mut visitor.outbound);
        let visitor_snapshot = world
            .myroom
            .first_snapshot_if_member(&visitor.identity)
            .unwrap()
            .unwrap();
        assert_eq!(
            tombstone_packet,
            serialize_slot_data(&projected_slots_for_topology(
                owner_projection.as_ref().unwrap(),
                &visitor_snapshot,
            ))
            .unwrap()
        );
        assert!(matches!(
            &visitor_snapshot.slots[0],
            MyRoomSlot::Player(player)
                if player.user_no == owner.identity.user_no.get()
                    && player.nickname == owner.identity.nickname
        ));
        assert_eq!(
            world
                .myroom
                .first_snapshot_if_member(&owner.identity)
                .unwrap(),
            None
        );

        let (owner_prepared, _) = prepare_test_myroom_command(&world, owner.session);
        world
            .myroom_command(
                owner.session,
                MyRoomCommandPayload::FirstState,
                owner_prepared,
            )
            .unwrap();
        let empty: [MyRoomSlot; MYROOM_SLOT_COUNT] = std::array::from_fn(|_| MyRoomSlot::Empty);
        assert_eq!(
            take_single_packet(&mut owner.outbound),
            serialize_slot_data(&empty).unwrap()
        );

        let (visitor_prepared, _) = prepare_test_myroom_command(&world, visitor.session);
        world
            .myroom_command(
                visitor.session,
                MyRoomCommandPayload::Secede,
                visitor_prepared,
            )
            .unwrap();
        assert_eq!(
            take_single_packet(&mut visitor.outbound),
            serialize_secede_reply()
        );
        assert_eq!(world.myroom.room_count(), 0);
        assert_eq!(world.myroom.member_count(), 0);
        assert_eq!(world.myroom.generation_count(), 0);
        world.myroom.audit_invariants().unwrap();
    }

    #[test]
    fn myroom_character_position_with_no_peers_is_a_stateless_success() {
        let (mut world, mut owner) = prepare_myroom_owner("PositionSoloOwner", 56_060, 1);
        let revision = world.myroom.revision();

        world
            .myroom_peer_command(
                owner.session,
                MyRoomPeerCommandPayload::CharacterPosition(CharacterPositionRequest {
                    slot: 0,
                    transform: [0.0; 6],
                }),
            )
            .unwrap();

        assert!(owner.outbound.try_recv().is_err());
        assert_eq!(world.myroom.revision(), revision);
        world.myroom.audit_invariants().unwrap();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one three-peer fixture proves normal relay, spoof rejection, nonmember silence, atomic backpressure, retry, and topology immutability together"
    )]
    fn myroom_character_position_uses_exact_slots_and_atomic_peer_backpressure() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "PositionOwner", 67, 56_061, 8);
        let mut visitor = register_channel_session(&mut world, "PositionVisitor", 67, 56_063, 8);
        let mut third = register_channel_session(&mut world, "PositionThird", 67, 56_065, 1);
        let mut outsider = register_channel_session(&mut world, "PositionOutsider", 67, 56_067, 8);
        enter_myroom(
            &mut world,
            &owner.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 22),
            Ipv4Addr::new(192, 0, 2, 22),
        );
        enter_myroom(
            &mut world,
            &visitor.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 23),
            Ipv4Addr::new(192, 0, 2, 22),
        );
        enter_myroom(
            &mut world,
            &third.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 24),
            Ipv4Addr::new(192, 0, 2, 22),
        );
        let revision = world.myroom.revision();
        let first = CharacterPositionRequest {
            slot: 1,
            transform: [1.0, -2.0, 3.0, -4.0, 5.0, -6.0],
        };
        let first_packet =
            serialize_character_position(i32::from(first.slot), first.transform).unwrap();

        world
            .myroom_peer_command(
                visitor.session,
                MyRoomPeerCommandPayload::CharacterPosition(first),
            )
            .unwrap();
        assert_eq!(take_single_packet(&mut owner.outbound), first_packet);
        assert_eq!(take_single_packet(&mut third.outbound), first_packet);
        assert!(visitor.outbound.try_recv().is_err());
        assert_eq!(
            world.myroom.revision(),
            revision,
            "an ephemeral position update cannot mutate MyRoom topology"
        );

        world
            .myroom_peer_command(
                visitor.session,
                MyRoomPeerCommandPayload::CharacterPosition(CharacterPositionRequest {
                    slot: 0,
                    transform: [7.0; 6],
                }),
            )
            .unwrap();
        assert!(owner.outbound.try_recv().is_err());
        assert!(third.outbound.try_recv().is_err());
        assert!(visitor.outbound.try_recv().is_err());

        world
            .myroom_peer_command(
                outsider.session,
                MyRoomPeerCommandPayload::CharacterPosition(CharacterPositionRequest {
                    slot: 0,
                    transform: [8.0; 6],
                }),
            )
            .unwrap();
        assert!(owner.outbound.try_recv().is_err());
        assert!(third.outbound.try_recv().is_err());
        assert!(outsider.outbound.try_recv().is_err());

        let sentinel = vec![0x51, 0x36];
        world.sessions[&third.session]
            .outbound
            .as_ref()
            .unwrap()
            .try_send(OutboundBatch::single(sentinel.clone()))
            .unwrap();
        let retry = CharacterPositionRequest {
            slot: 1,
            transform: [9.0; 6],
        };
        assert!(matches!(
            world.myroom_peer_command(
                visitor.session,
                MyRoomPeerCommandPayload::CharacterPosition(retry),
            ),
            Err(WorldOperationError::Command(
                WorldError::MyRoomCommandOutboundUnavailable { session }
            )) if session == third.session
        ));
        assert!(
            owner.outbound.try_recv().is_err(),
            "reserving an earlier peer cannot publish a partial fanout"
        );
        assert_eq!(take_single_packet(&mut third.outbound), sentinel);
        assert!(third.outbound.try_recv().is_err());

        world
            .myroom_peer_command(
                visitor.session,
                MyRoomPeerCommandPayload::CharacterPosition(retry),
            )
            .unwrap();
        let retry_packet =
            serialize_character_position(i32::from(retry.slot), retry.transform).unwrap();
        assert_eq!(take_single_packet(&mut owner.outbound), retry_packet);
        assert_eq!(take_single_packet(&mut third.outbound), retry_packet);
        assert!(visitor.outbound.try_recv().is_err());
        assert_eq!(world.myroom.revision(), revision);
        world.myroom.audit_invariants().unwrap();
    }

    #[tokio::test]
    async fn dropped_myroom_character_position_ack_still_publishes() {
        let mut world = World::default();
        let mut owner =
            register_channel_session(&mut world, "DroppedPositionAckOwner", 67, 56_069, 8);
        let mut visitor =
            register_channel_session(&mut world, "DroppedPositionAckVisitor", 67, 56_071, 8);
        enter_myroom(
            &mut world,
            &owner.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 25),
            Ipv4Addr::new(192, 0, 2, 25),
        );
        enter_myroom(
            &mut world,
            &visitor.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 26),
            Ipv4Addr::new(192, 0, 2, 25),
        );
        let request = CharacterPositionRequest {
            slot: 1,
            transform: [10.0; 6],
        };
        let (reply, response) = oneshot::channel();
        drop(response);

        assert!(
            !dispatch_command(
                &mut world,
                WorldCommand::MyRoomPeer {
                    session: visitor.session,
                    payload: MyRoomPeerCommandPayload::CharacterPosition(request),
                    reply,
                },
                &WorldSidecars::default(),
                &ServerClock::new(),
            )
            .await
            .unwrap()
        );
        assert_eq!(
            take_single_packet(&mut owner.outbound),
            serialize_character_position(i32::from(request.slot), request.transform).unwrap()
        );
        assert!(visitor.outbound.try_recv().is_err());
        world.myroom.audit_invariants().unwrap();
    }

    #[tokio::test]
    async fn quiesce_rejects_myroom_character_position_before_publication() {
        let request = CharacterPositionRequest {
            slot: 0,
            transform: [11.0; 6],
        };
        let (reply, response) = oneshot::channel();
        assert!(
            super::admit_command_during_quiesce(WorldCommand::MyRoomPeer {
                session: SessionId::new(1),
                payload: MyRoomPeerCommandPayload::CharacterPosition(request),
                reply,
            })
            .is_none()
        );
        assert!(matches!(
            response.await.unwrap(),
            Err(WorldError::OutboundProductionClosed)
        ));
    }

    #[test]
    fn myroom_secede_backpressure_releases_prior_permits_and_rolls_back() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "BackpressureOwner", 67, 56_070, 1);
        let mut visitor =
            register_channel_session(&mut world, "BackpressureVisitor", 67, 56_080, 1);
        enter_myroom(
            &mut world,
            &owner.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 30),
            Ipv4Addr::new(192, 0, 2, 30),
        );
        enter_myroom(
            &mut world,
            &visitor.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 31),
            Ipv4Addr::new(192, 0, 2, 30),
        );
        world.sessions[&visitor.session]
            .outbound
            .as_ref()
            .unwrap()
            .try_send(OutboundBatch::single(vec![0xA5]))
            .unwrap();
        let revision = world.myroom.revision();
        let snapshot = world.myroom.first_snapshot(&visitor.identity).unwrap();
        let (visitor_prepared, _) = prepare_test_myroom_command(&world, visitor.session);

        assert!(matches!(
            world.myroom_command(
                visitor.session,
                MyRoomCommandPayload::Secede,
                visitor_prepared
            ),
            Err(WorldOperationError::Command(
                WorldError::MyRoomCommandOutboundUnavailable { session }
            )) if session == visitor.session
        ));
        assert_eq!(world.myroom.revision(), revision);
        assert_eq!(
            world.myroom.first_snapshot(&visitor.identity).unwrap(),
            snapshot
        );
        assert_eq!(take_single_packet(&mut visitor.outbound), vec![0xA5]);
        assert!(matches!(
            owner.outbound.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));

        world.sessions[&owner.session]
            .outbound
            .as_ref()
            .unwrap()
            .try_send(OutboundBatch::single(vec![0xB6]))
            .unwrap();
        let (visitor_prepared, _) = prepare_test_myroom_command(&world, visitor.session);
        assert!(matches!(
            world.myroom_command(
                visitor.session,
                MyRoomCommandPayload::Secede,
                visitor_prepared
            ),
            Err(WorldOperationError::Command(
                WorldError::MyRoomCommandOutboundUnavailable { session }
            )) if session == owner.session
        ));
        assert_eq!(world.myroom.revision(), revision);
        assert_eq!(
            world.myroom.first_snapshot(&visitor.identity).unwrap(),
            snapshot
        );
        assert_eq!(take_single_packet(&mut owner.outbound), vec![0xB6]);
        assert!(matches!(
            visitor.outbound.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        world.myroom.audit_invariants().unwrap();
    }

    #[tokio::test]
    async fn dropped_myroom_command_ack_still_commits_and_publishes() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "DroppedAckOwner", 67, 56_090, 8);
        let mut visitor = register_channel_session(&mut world, "DroppedAckVisitor", 67, 56_100, 8);
        enter_myroom(
            &mut world,
            &owner.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 40),
            Ipv4Addr::new(192, 0, 2, 40),
        );
        enter_myroom(
            &mut world,
            &visitor.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 41),
            Ipv4Addr::new(192, 0, 2, 40),
        );
        let (reply, response) = oneshot::channel();
        drop(response);
        let (prepared, projection) = prepare_test_myroom_command(&world, visitor.session);

        assert!(
            !dispatch_command(
                &mut world,
                WorldCommand::MyRoom {
                    session: visitor.session,
                    payload: MyRoomCommandPayload::Secede,
                    prepared: Box::new(prepared),
                    reply,
                },
                &WorldSidecars::default(),
                &ServerClock::new(),
            )
            .await
            .unwrap()
        );

        assert_eq!(
            take_single_packet(&mut visitor.outbound),
            serialize_secede_reply()
        );
        let owner_snapshot = world.myroom.first_snapshot(&owner.identity).unwrap();
        assert_eq!(
            take_single_packet(&mut owner.outbound),
            serialize_slot_data(&projected_slots_for_topology(
                projection.as_ref().unwrap(),
                &owner_snapshot,
            ))
            .unwrap()
        );
        assert_eq!(
            world
                .myroom
                .first_snapshot_if_member(&visitor.identity)
                .unwrap(),
            None
        );
        world.myroom.audit_invariants().unwrap();
    }

    #[tokio::test]
    async fn dropped_myroom_owner_item_ack_still_publishes_the_actor_owned_batch() {
        let mut world = World::default();
        let mut outsider =
            register_channel_session(&mut world, "DroppedItemAckOutsider", 67, 56_105, 1);
        let plan = world.prepare_myroom_owner_items(outsider.session).unwrap();
        let prepared = plan.complete(None).unwrap();
        let (reply, response) = oneshot::channel();
        drop(response);

        assert!(
            !dispatch_command(
                &mut world,
                WorldCommand::PublishMyRoomOwnerItems {
                    session: outsider.session,
                    prepared: Box::new(prepared),
                    reply,
                },
                &WorldSidecars::default(),
                &ServerClock::new(),
            )
            .await
            .unwrap()
        );
        assert_eq!(
            take_single_packet(&mut outsider.outbound),
            serialize_missing_owner_items()
        );
        assert!(matches!(
            outsider.outbound.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn stale_myroom_session_cannot_publish_character_position_after_migration() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "PositionFenceOwner", 67, 56_106, 8);
        let visitor = register_channel_session(&mut world, "PositionFenceVisitor", 67, 56_108, 8);
        enter_myroom(
            &mut world,
            &owner.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 48),
            Ipv4Addr::new(192, 0, 2, 48),
        );
        enter_myroom(
            &mut world,
            &visitor.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 49),
            Ipv4Addr::new(192, 0, 2, 48),
        );
        let mut migrated = migrate_channel_session(&mut world, &visitor, 56_109, 8);
        drain_batches(&mut owner.outbound);
        drain_batches(&mut migrated.outbound);
        let request = CharacterPositionRequest {
            slot: 1,
            transform: [12.0; 6],
        };

        assert!(matches!(
            world.myroom_peer_command(
                visitor.session,
                MyRoomPeerCommandPayload::CharacterPosition(request),
            ),
            Err(WorldOperationError::Command(WorldError::Identity(
                IdentityError::StaleSession(session)
            ))) if session == visitor.session
        ));
        assert!(owner.outbound.try_recv().is_err());
        assert!(migrated.outbound.try_recv().is_err());

        world
            .myroom_peer_command(
                migrated.session,
                MyRoomPeerCommandPayload::CharacterPosition(request),
            )
            .unwrap();
        assert_eq!(
            take_single_packet(&mut owner.outbound),
            serialize_character_position(i32::from(request.slot), request.transform).unwrap()
        );
        assert!(migrated.outbound.try_recv().is_err());
        world.myroom.audit_invariants().unwrap();
    }

    #[test]
    fn stale_myroom_session_cannot_secede_the_current_generation() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "FenceOwner", 67, 56_110, 8);
        let visitor = register_channel_session(&mut world, "FenceVisitor", 67, 56_120, 8);
        enter_myroom(
            &mut world,
            &owner.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 50),
            Ipv4Addr::new(192, 0, 2, 50),
        );
        enter_myroom(
            &mut world,
            &visitor.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 51),
            Ipv4Addr::new(192, 0, 2, 50),
        );
        let (stale_prepared, _) = prepare_test_myroom_command(&world, visitor.session);
        let mut migrated = migrate_channel_session(&mut world, &visitor, 56_130, 8);
        drain_batches(&mut owner.outbound);
        drain_batches(&mut migrated.outbound);

        assert!(matches!(
            world.myroom_command(
                visitor.session,
                MyRoomCommandPayload::Secede,
                stale_prepared
            ),
            Err(WorldOperationError::Command(WorldError::Identity(
                IdentityError::StaleSession(session)
            ))) if session == visitor.session
        ));
        assert!(
            world
                .myroom
                .first_snapshot_if_member(&migrated.identity)
                .unwrap()
                .is_some()
        );
        assert!(owner.outbound.try_recv().is_err());
        assert!(migrated.outbound.try_recv().is_err());
        world.myroom.audit_invariants().unwrap();
    }

    #[test]
    fn stale_myroom_wire_projection_is_retryable_and_side_effect_free() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "PlanOwner", 67, 56_140, 8);
        let mut visitor = register_channel_session(&mut world, "PlanVisitor", 67, 56_150, 8);
        let newcomer = register_channel_session(&mut world, "PlanNewcomer", 67, 56_160, 8);
        enter_myroom(
            &mut world,
            &owner.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 60),
            Ipv4Addr::new(192, 0, 2, 60),
        );
        enter_myroom(
            &mut world,
            &visitor.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 61),
            Ipv4Addr::new(192, 0, 2, 60),
        );
        let (stale_prepared, _) = prepare_test_myroom_command(&world, visitor.session);

        enter_myroom(
            &mut world,
            &newcomer.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 62),
            Ipv4Addr::new(192, 0, 2, 60),
        );
        let revision = world.myroom.revision();
        let snapshot = world.myroom.first_snapshot(&visitor.identity).unwrap();

        assert!(matches!(
            world.myroom_command(
                visitor.session,
                MyRoomCommandPayload::Secede,
                stale_prepared
            ),
            Err(WorldOperationError::Command(
                WorldError::MyRoomWirePlanStale { session }
            )) if session == visitor.session
        ));
        assert_eq!(world.myroom.revision(), revision);
        assert_eq!(
            world.myroom.first_snapshot(&visitor.identity).unwrap(),
            snapshot
        );
        assert!(owner.outbound.try_recv().is_err());
        assert!(visitor.outbound.try_recv().is_err());
        world.myroom.audit_invariants().unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn durable_myroom_owner_write_commits_disk_hub_and_exact_echo() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let initial = store
            .save("DurableActorOwner", &Profile::default())
            .unwrap();
        let (profiles, profile_runtime) = spawn_test_profile_io(root.path());
        let (world, mut owner) = prepare_myroom_owner("DurableActorOwner", 56_050, 8);
        let session = owner.session;
        let (world, actor) = spawn_prepared_world(world, 16);
        let proposed = MyRoomInfo {
            room_id: 5136,
            bgm: 7,
            room_password: "owner room".to_owned(),
            ..MyRoomInfo::default()
        };

        let admission = profiles
            .admit("durableactorowner", "test durable MyRoom actor write")
            .await
            .unwrap();
        let receipt = world
            .persist_myroom_owner_info(session, proposed.clone(), admission)
            .await
            .unwrap();
        assert_eq!(receipt.info(), &proposed);
        assert_eq!(receipt.revision(), initial.revision + 1);
        assert_eq!(
            receipt.publication(),
            MyRoomInfoPublication::ActiveOwnerEchoed
        );
        let view = world.myroom_session_view(session).await.unwrap().unwrap();
        assert_eq!(view.info(), &proposed);
        assert_eq!(
            take_single_packet(&mut owner.outbound),
            serialize_myroom_info(&proposed).unwrap()
        );
        assert!(owner.outbound.try_recv().is_err());
        let loaded = store.load_or_create("DURABLEACTOROWNER").unwrap();
        assert_eq!(loaded.revision, Some(receipt.revision()));
        assert_eq!(
            loaded.profile.my_room.try_to_protocol_info().unwrap(),
            proposed
        );

        profile_runtime.shutdown().await.unwrap();
        world.drain_myroom_completions().await.unwrap();
        world.shutdown().await.unwrap();
        actor.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn accepted_myroom_write_survives_request_result_cancellation() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let initial = store
            .save("CancelledActorOwner", &Profile::default())
            .unwrap();
        let (profiles, profile_runtime) = spawn_test_profile_io(root.path());
        let (world, mut owner) = prepare_myroom_owner("CancelledActorOwner", 56_060, 8);
        let session = owner.session;
        let (world, actor) = spawn_prepared_world(world, 16);
        let proposed = MyRoomInfo {
            room_id: 6060,
            bgm: 6,
            ..MyRoomInfo::default()
        };

        let completion = world.myroom_completions.reserve().await.unwrap();
        let admission = profiles
            .admit("cancelledactorowner", "test cancelled MyRoom request")
            .await
            .unwrap();
        let prepared = PreparedMyRoomInfoWrite::new(admission, proposed.clone(), completion);
        let (request_reply, request_result) = oneshot::channel();
        let (registration_reply, registration_result) = oneshot::channel();
        world
            .sender
            .send(WorldCommand::RegisterMyRoomInfoWrite {
                session,
                prepared,
                request_reply,
                reply: registration_reply,
            })
            .await
            .unwrap();
        let registered = registration_result.await.unwrap().unwrap();
        registered.submit();
        drop(request_result);

        profile_runtime.shutdown().await.unwrap();
        world.drain_myroom_completions().await.unwrap();
        let view = world.myroom_session_view(session).await.unwrap().unwrap();
        assert_eq!(view.info(), &proposed);
        assert_eq!(
            take_single_packet(&mut owner.outbound),
            serialize_myroom_info(&proposed).unwrap()
        );
        assert!(owner.outbound.try_recv().is_err());
        let loaded = store.load_or_create("CancelledActorOwner").unwrap();
        assert_eq!(loaded.revision, Some(initial.revision + 1));
        assert_eq!(
            loaded.profile.my_room.try_to_protocol_info().unwrap(),
            proposed
        );

        world.shutdown().await.unwrap();
        actor.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(
        clippy::too_many_lines,
        reason = "this end-to-end cancellation test covers disk durability and both actor publication planes"
    )]
    async fn accepted_rider_equipment_write_survives_request_cancellation_and_publishes_caches() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let mut initial_profile = Profile::default();
        initial_profile.rider.p2p_port = 45_136;
        initial_profile.rider.rp = 515_136;
        initial_profile.rider.club_name = "DurableEquipment".to_owned();
        let initial = store
            .save("CancelledEquipmentOwner", &initial_profile)
            .unwrap();
        let (profiles, profile_runtime) = spawn_test_profile_io(root.path());

        let mut prepared_world = World::default();
        let mut owner = register_channel_session(
            &mut prepared_world,
            "CancelledEquipmentOwner",
            67,
            56_062,
            16,
        );
        let mut visitor = register_channel_session(
            &mut prepared_world,
            "EquipmentMyRoomVisitor",
            67,
            56_064,
            16,
        );
        let mut peer =
            register_channel_session(&mut prepared_world, "EquipmentProtocolPeer", 67, 56_066, 16);
        enter_myroom(
            &mut prepared_world,
            &owner.identity,
            &owner.identity,
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::LOCALHOST,
        );
        enter_myroom(
            &mut prepared_world,
            &visitor.identity,
            &owner.identity,
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::LOCALHOST,
        );
        let room_id = create_protocol_room(&mut prepared_world, &owner, 1);
        join_protocol_room(&mut prepared_world, &peer, room_id, false);
        let owner_player_id = prepared_world.protocol_rooms[&room_id]
            .equipment_player_id(owner.identity.user_no)
            .unwrap();
        drain_batches(&mut owner.outbound);
        drain_batches(&mut visitor.outbound);
        drain_batches(&mut peer.outbound);

        let session = owner.session;
        let (world, actor) = spawn_prepared_world(prepared_world, 16);
        let completion = world.reserve_rider_equipment_completion().await.unwrap();
        let admission = profiles
            .admit(
                "cancelledequipmentowner",
                "test cancelled rider-equipment request",
            )
            .await
            .unwrap();
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let hook_entered = Arc::clone(&entered);
        let hook_release = Arc::clone(&release);
        let prepared = PreparedRiderEquipmentWrite::new(
            admission,
            test_equipment_selection(),
            Arc::new(test_equipment_catalog()),
            completion,
        )
        .with_test_hook(Arc::new(move || {
            hook_entered.wait();
            hook_release.wait();
        }));
        let (request_reply, request_result) = oneshot::channel();
        let (registration_reply, registration_result) = oneshot::channel();
        world
            .sender
            .send(WorldCommand::RegisterRiderEquipmentWrite {
                session,
                prepared,
                request_reply,
                reply: registration_reply,
            })
            .await
            .unwrap();
        let registered = registration_result.await.unwrap().unwrap();
        registered.submit();
        let entered_wait = Arc::clone(&entered);
        tokio::task::spawn_blocking(move || entered_wait.wait())
            .await
            .unwrap();
        drop(request_result);
        let release_wait = Arc::clone(&release);
        tokio::task::spawn_blocking(move || release_wait.wait())
            .await
            .unwrap();

        profile_runtime.shutdown().await.unwrap();
        world.drain_myroom_completions().await.unwrap();
        let loaded = store.load_or_create("CancelledEquipmentOwner").unwrap();
        assert_eq!(loaded.revision, Some(initial.revision + 1));
        let snapshot = rider_item_snapshot(&loaded.profile.rider_item);
        assert_eq!(
            take_single_packet(&mut peer.outbound),
            serialize_room_slot_items(owner_player_id, &snapshot).unwrap()
        );
        assert!(peer.outbound.try_recv().is_err());
        assert!(
            owner.outbound.try_recv().is_err(),
            "the equipment sender must not receive its game-room publication"
        );
        assert!(
            visitor.outbound.try_recv().is_err(),
            "a profile refresh must not emit an immediate MyRoom snapshot"
        );

        world.session_closed(visitor.session).await.unwrap();
        let expected_owner = crate::profile_io::myroom_profile_presentation(&loaded.profile)
            .player_for(&owner.identity);
        let expected_slots: [MyRoomSlot; MYROOM_SLOT_COUNT] = array::from_fn(|slot| {
            if slot == 0 {
                MyRoomSlot::Player(expected_owner.clone())
            } else {
                MyRoomSlot::Empty
            }
        });
        assert_eq!(
            take_single_packet(&mut owner.outbound),
            serialize_slot_data(&expected_slots).unwrap()
        );
        assert!(owner.outbound.try_recv().is_err());

        world.shutdown().await.unwrap();
        actor.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn invalid_myroom_presentation_does_not_kill_durable_equipment_publication() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let mut initial_profile = Profile::default();
        initial_profile.rider.club_name = "x".repeat(MAX_CLUB_NAME_UTF16_UNITS + 1);
        let initial = store
            .save("InvalidEquipmentPresentation", &initial_profile)
            .unwrap();
        let (profiles, profile_runtime) = spawn_test_profile_io(root.path());

        let mut prepared_world = World::default();
        let mut owner = register_channel_session(
            &mut prepared_world,
            "InvalidEquipmentPresentation",
            67,
            56_067,
            16,
        );
        let mut peer =
            register_channel_session(&mut prepared_world, "InvalidEquipmentPeer", 67, 56_069, 16);
        enter_myroom(
            &mut prepared_world,
            &owner.identity,
            &owner.identity,
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::LOCALHOST,
        );
        let room_id = create_protocol_room(&mut prepared_world, &owner, 1);
        join_protocol_room(&mut prepared_world, &peer, room_id, false);
        let owner_player_id = prepared_world.protocol_rooms[&room_id]
            .equipment_player_id(owner.identity.user_no)
            .unwrap();
        drain_batches(&mut owner.outbound);
        drain_batches(&mut peer.outbound);

        let session = owner.session;
        let (world, actor) = spawn_prepared_world(prepared_world, 16);
        let completion = world.reserve_rider_equipment_completion().await.unwrap();
        let admission = profiles
            .admit(
                "invalidequipmentpresentation",
                "test invalid-presentation rider equipment",
            )
            .await
            .unwrap();
        let prepared = PreparedRiderEquipmentWrite::new(
            admission,
            test_equipment_selection(),
            Arc::new(test_equipment_catalog()),
            completion,
        );
        world
            .persist_rider_equipment(session, prepared)
            .await
            .unwrap();
        profile_runtime.shutdown().await.unwrap();
        world.drain_myroom_completions().await.unwrap();

        let loaded = store
            .load_or_create("InvalidEquipmentPresentation")
            .unwrap();
        assert_eq!(loaded.revision, Some(initial.revision + 1));
        let equipment = rider_item_snapshot(&loaded.profile.rider_item);
        assert_eq!(
            take_single_packet(&mut peer.outbound),
            serialize_room_slot_items(owner_player_id, &equipment).unwrap()
        );
        assert_eq!(
            world.authorize_identity(session).await.unwrap(),
            owner.identity
        );
        let view = world.myroom_session_view(session).await.unwrap().unwrap();
        assert_eq!(view.info().room_id, 1);
        assert!(owner.outbound.try_recv().is_err());

        world.shutdown().await.unwrap();
        actor.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn successful_equipment_write_defers_close_until_publication_finishes() {
        exercise_rider_equipment_close_barrier(true).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rejected_equipment_write_still_releases_the_deferred_close() {
        exercise_rider_equipment_close_barrier(false).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropped_rider_equipment_registration_reply_aborts_ticket_and_releases_lane() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let initial = store
            .save("DroppedEquipmentCapability", &Profile::default())
            .unwrap();
        let (profiles, profile_runtime) = spawn_test_profile_io(root.path());
        let mut prepared_world = World::default();
        let owner = register_channel_session(
            &mut prepared_world,
            "DroppedEquipmentCapability",
            67,
            56_068,
            8,
        );
        let session = owner.session;
        let (world, actor) = spawn_prepared_world(prepared_world, 16);

        let completion = world.reserve_rider_equipment_completion().await.unwrap();
        let admission = profiles
            .admit(
                "droppedequipmentcapability",
                "test dropped rider-equipment registration reply",
            )
            .await
            .unwrap();
        let prepared = PreparedRiderEquipmentWrite::new(
            admission,
            test_equipment_selection(),
            Arc::new(test_equipment_catalog()),
            completion,
        );
        let (request_reply, request_result) = oneshot::channel();
        let (registration_reply, registration_result) = oneshot::channel();
        drop(registration_result);
        world
            .sender
            .send(WorldCommand::RegisterRiderEquipmentWrite {
                session,
                prepared,
                request_reply,
                reply: registration_reply,
            })
            .await
            .unwrap();
        assert!(matches!(
            request_result.await,
            Ok(Err(RiderEquipmentWriteError::AbortedBeforeSubmission))
        ));
        world.drain_myroom_completions().await.unwrap();
        let replacement = profiles
            .admit(
                "droppedequipmentcapability",
                "test released rider-equipment profile lane",
            )
            .await
            .unwrap();
        drop(replacement);
        assert_eq!(
            store
                .load_or_create("DroppedEquipmentCapability")
                .unwrap()
                .revision,
            Some(initial.revision)
        );

        profile_runtime.shutdown().await.unwrap();
        world.shutdown().await.unwrap();
        actor.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn guarded_shutdown_waits_for_registered_rider_equipment_ticket_to_abort() {
        let root = tempfile::tempdir().unwrap();
        ProfileStore::new(root.path())
            .save("PendingEquipmentShutdown", &Profile::default())
            .unwrap();
        let (profiles, profile_runtime) = spawn_test_profile_io(root.path());
        let mut prepared_world = World::default();
        let owner = register_channel_session(
            &mut prepared_world,
            "PendingEquipmentShutdown",
            67,
            56_069,
            8,
        );
        let session = owner.session;
        let (world, actor) = spawn_prepared_world(prepared_world, 16);

        let completion = world.reserve_rider_equipment_completion().await.unwrap();
        let admission = profiles
            .admit(
                "pendingequipmentshutdown",
                "test pending rider-equipment shutdown",
            )
            .await
            .unwrap();
        let prepared = PreparedRiderEquipmentWrite::new(
            admission,
            test_equipment_selection(),
            Arc::new(test_equipment_catalog()),
            completion,
        );
        let (request_reply, request_result) = oneshot::channel();
        let (registration_reply, registration_result) = oneshot::channel();
        world
            .sender
            .send(WorldCommand::RegisterRiderEquipmentWrite {
                session,
                prepared,
                request_reply,
                reply: registration_reply,
            })
            .await
            .unwrap();
        let registered = registration_result.await.unwrap().unwrap();
        assert!(matches!(
            world.shutdown().await,
            Err(WorldError::MyRoomPersistenceShutdownBlocked {
                pending: 1,
                indexed: 1,
            })
        ));
        drop(registered);
        assert!(matches!(
            request_result.await,
            Ok(Err(RiderEquipmentWriteError::AbortedBeforeSubmission))
        ));
        world.drain_myroom_completions().await.unwrap();

        profile_runtime.shutdown().await.unwrap();
        world.shutdown().await.unwrap();
        actor.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropped_registration_reply_aborts_ticket_and_releases_profile_lane() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let initial = store
            .save("DroppedCapabilityOwner", &Profile::default())
            .unwrap();
        let (profiles, profile_runtime) = spawn_test_profile_io(root.path());
        let (world, owner) = prepare_myroom_owner("DroppedCapabilityOwner", 56_070, 8);
        let session = owner.session;
        let (world, actor) = spawn_prepared_world(world, 16);

        let completion = world.myroom_completions.reserve().await.unwrap();
        let admission = profiles
            .admit(
                "droppedcapabilityowner",
                "test dropped MyRoom registration reply",
            )
            .await
            .unwrap();
        let prepared = PreparedMyRoomInfoWrite::new(admission, MyRoomInfo::default(), completion);
        let (request_reply, request_result) = oneshot::channel();
        let (registration_reply, registration_result) = oneshot::channel();
        drop(registration_result);
        world
            .sender
            .send(WorldCommand::RegisterMyRoomInfoWrite {
                session,
                prepared,
                request_reply,
                reply: registration_reply,
            })
            .await
            .unwrap();
        assert!(matches!(
            request_result.await,
            Ok(Err(MyRoomInfoWriteError::AbortedBeforeSubmission))
        ));
        world.drain_myroom_completions().await.unwrap();

        let replacement = profiles
            .admit(
                "DROPPEDCAPABILITYOWNER",
                "test released MyRoom profile lane",
            )
            .await
            .unwrap();
        drop(replacement);
        let loaded = store.load_or_create("DroppedCapabilityOwner").unwrap();
        assert_eq!(loaded.revision, Some(initial.revision));

        profile_runtime.shutdown().await.unwrap();
        world.shutdown().await.unwrap();
        actor.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn full_owner_outbound_rejects_myroom_write_before_disk_mutation() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let initial = store.save("FullWriteOwner", &Profile::default()).unwrap();
        let (profiles, profile_runtime) = spawn_test_profile_io(root.path());
        let (prepared_world, mut owner) = prepare_myroom_owner("FullWriteOwner", 56_080, 1);
        prepared_world.sessions[&owner.session]
            .outbound
            .as_ref()
            .unwrap()
            .try_send(OutboundBatch::single(vec![0xA5]))
            .unwrap();
        let session = owner.session;
        let (world, actor) = spawn_prepared_world(prepared_world, 16);
        let admission = profiles
            .admit("fullwriteowner", "test full MyRoom outbound")
            .await
            .unwrap();

        assert!(matches!(
            world
                .persist_myroom_owner_info(
                    session,
                    MyRoomInfo {
                        room_id: 8080,
                        ..MyRoomInfo::default()
                    },
                    admission,
                )
                .await,
            Err(MyRoomInfoWriteError::OutboundUnavailable { session: actual })
                if actual == session
        ));
        let loaded = store.load_or_create("FullWriteOwner").unwrap();
        assert_eq!(loaded.revision, Some(initial.revision));
        assert_eq!(take_single_packet(&mut owner.outbound), vec![0xA5]);

        profile_runtime.shutdown().await.unwrap();
        world.drain_myroom_completions().await.unwrap();
        world.shutdown().await.unwrap();
        actor.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn guarded_shutdown_waits_for_registered_myroom_ticket_to_abort() {
        let root = tempfile::tempdir().unwrap();
        ProfileStore::new(root.path())
            .save("PendingShutdownOwner", &Profile::default())
            .unwrap();
        let (profiles, profile_runtime) = spawn_test_profile_io(root.path());
        let (prepared_world, owner) = prepare_myroom_owner("PendingShutdownOwner", 56_090, 8);
        let session = owner.session;
        let (world, actor) = spawn_prepared_world(prepared_world, 16);

        let completion = world.myroom_completions.reserve().await.unwrap();
        let admission = profiles
            .admit("pendingshutdownowner", "test pending MyRoom shutdown")
            .await
            .unwrap();
        let prepared = PreparedMyRoomInfoWrite::new(admission, MyRoomInfo::default(), completion);
        let (request_reply, request_result) = oneshot::channel();
        let (registration_reply, registration_result) = oneshot::channel();
        world
            .sender
            .send(WorldCommand::RegisterMyRoomInfoWrite {
                session,
                prepared,
                request_reply,
                reply: registration_reply,
            })
            .await
            .unwrap();
        let registered = registration_result.await.unwrap().unwrap();

        assert!(matches!(
            world.shutdown().await,
            Err(WorldError::MyRoomPersistenceShutdownBlocked {
                pending: 1,
                indexed: 1,
            })
        ));
        drop(registered);
        assert!(matches!(
            request_result.await,
            Ok(Err(MyRoomInfoWriteError::AbortedBeforeSubmission))
        ));
        world.drain_myroom_completions().await.unwrap();

        profile_runtime.shutdown().await.unwrap();
        world.shutdown().await.unwrap();
        actor.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn force_shutdown_reports_abandoned_myroom_ticket_and_user_index() {
        let root = tempfile::tempdir().unwrap();
        ProfileStore::new(root.path())
            .save("ForcePendingOwner", &Profile::default())
            .unwrap();
        let (profiles, profile_runtime) = spawn_test_profile_io(root.path());
        let (prepared_world, mut owner) = prepare_myroom_owner("ForcePendingOwner", 56_095, 8);
        let session = owner.session;
        let (world, actor) = spawn_prepared_world(prepared_world, 16);

        let completion = world.myroom_completions.reserve().await.unwrap();
        let admission = profiles
            .admit("forcependingowner", "test forced MyRoom shutdown")
            .await
            .unwrap();
        let prepared = PreparedMyRoomInfoWrite::new(admission, MyRoomInfo::default(), completion);
        let (request_reply, request_result) = oneshot::channel();
        let (registration_reply, registration_result) = oneshot::channel();
        world
            .sender
            .send(WorldCommand::RegisterMyRoomInfoWrite {
                session,
                prepared,
                request_reply,
                reply: registration_reply,
            })
            .await
            .unwrap();
        let registered = registration_result.await.unwrap().unwrap();

        assert!(matches!(
            world.shutdown().await,
            Err(WorldError::MyRoomPersistenceShutdownBlocked {
                pending: 1,
                indexed: 1,
            })
        ));
        let report = world.force_shutdown().await.unwrap();
        assert_eq!(report.myroom_tickets, 1);
        assert_eq!(report.myroom_user_indexes, 1);

        actor.await.unwrap();
        assert!(
            request_result.await.is_err(),
            "force shutdown must close the deliberately abandoned final reply"
        );
        assert!(
            matches!(
                owner.outbound.try_recv(),
                Err(mpsc::error::TryRecvError::Disconnected)
            ),
            "force shutdown must not publish the reserved owner echo"
        );
        drop(registered);
        profile_runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_barriers_report_an_outstanding_migration_freeze() {
        let (world, actor) = WorldHandle::spawn(8).expect("nonzero World mailbox capacity");
        let source = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 56_096))
            .await
            .unwrap();
        let identity = world
            .claim_identity(source, "FrozenMigrationOwner")
            .await
            .unwrap();
        let channel = ChannelBinding {
            channel_id: 12,
            game_type: 67,
        };
        let token = MigrationToken::new(56_096).unwrap();
        world
            .begin_migration(source, channel, token, Instant::now())
            .await
            .unwrap();
        let destination = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 56_097))
            .await
            .unwrap();
        let preflight = world
            .preflight_migration(
                destination,
                identity.user_no,
                channel.channel_id,
                token,
                Instant::now(),
            )
            .await
            .unwrap();

        assert!(matches!(
            world.shutdown().await,
            Err(WorldError::MyRoomPersistenceShutdownBlocked {
                pending: 1,
                indexed: 0,
            })
        ));
        let report = world.force_shutdown().await.unwrap();
        assert_eq!(report.myroom_tickets, 0);
        assert_eq!(report.myroom_user_indexes, 0);
        assert_eq!(report.rider_equipment_tickets, 0);
        assert_eq!(report.rider_equipment_user_indexes, 0);
        assert_eq!(report.migration_transfers, 1);

        actor.await.unwrap().unwrap();
        drop(preflight);
    }

    #[test]
    fn myroom_udp_audience_is_exact_generation_and_nonmembers_are_empty() {
        let mut world = World::default();
        assert_eq!(world.myroom.identity_capacity(), world.identity_capacity);
        let owner = register_channel_session(&mut world, "MyOwner", 67, 56_100, 8);
        let guest = register_channel_session(&mut world, "MyGuest", 67, 56_110, 8);
        let outsider = register_channel_session(&mut world, "Outsider", 67, 56_120, 8);
        enter_myroom(
            &mut world,
            &owner.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 1),
            Ipv4Addr::new(192, 0, 2, 1),
        );
        enter_myroom(
            &mut world,
            &guest.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 2),
            Ipv4Addr::new(192, 0, 2, 1),
        );

        assert_eq!(
            world.myroom_udp_targets(&owner.identity).unwrap(),
            vec![guest.identity.clone()]
        );
        assert!(
            world
                .myroom_udp_targets(&outsider.identity)
                .unwrap()
                .is_empty()
        );

        let mut forged = owner.identity.clone();
        forged.owner = guest.session;
        assert!(matches!(
            world.myroom_udp_targets(&forged),
            Err(MyRoomLifecycleError::Hub { .. })
        ));
    }

    #[test]
    fn myroom_disconnect_publishes_consistent_snapshot_and_owner_close_ejects() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "CloseOwner", 67, 56_200, 8);
        let guest = register_channel_session(&mut world, "CloseGuest", 67, 56_210, 8);
        enter_myroom(
            &mut world,
            &owner.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 10),
            Ipv4Addr::new(192, 0, 2, 10),
        );
        enter_myroom(
            &mut world,
            &guest.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 11),
            Ipv4Addr::new(192, 0, 2, 10),
        );

        world.close_session(guest.session, Instant::now()).unwrap();
        let snapshot = world.myroom.first_snapshot(&owner.identity).unwrap();
        assert_eq!(
            take_single_packet(&mut owner.outbound),
            serialize_slot_data(&snapshot.slots).unwrap()
        );
        assert_eq!(world.myroom.member_count(), 1);
        assert_eq!(
            world.myroom.membership_if_member(&guest.identity).unwrap(),
            None
        );

        let mut world = World::default();
        let owner = register_channel_session(&mut world, "EjectOwner", 67, 56_220, 8);
        let mut guest = register_channel_session(&mut world, "EjectGuest", 67, 56_230, 8);
        enter_myroom(
            &mut world,
            &owner.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 20),
            Ipv4Addr::new(192, 0, 2, 20),
        );
        enter_myroom(
            &mut world,
            &guest.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 21),
            Ipv4Addr::new(192, 0, 2, 20),
        );
        world.close_session(owner.session, Instant::now()).unwrap();
        let empty: [MyRoomSlot; MYROOM_SLOT_COUNT] = std::array::from_fn(|_| MyRoomSlot::Empty);
        assert_eq!(
            take_single_packet(&mut guest.outbound),
            serialize_slot_data(&empty).unwrap()
        );
        assert_eq!(world.myroom.room_count(), 0);
        assert_eq!(world.myroom.member_count(), 0);
        world.myroom.audit_invariants().unwrap();
    }

    #[test]
    fn myroom_migration_installs_fresh_profile_silently_and_advances_exact_generation() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "MoveOwner", 67, 56_300, 8);
        let mut guest = register_channel_session(&mut world, "MoveGuest", 67, 56_310, 8);
        let stale_presented_ip = Ipv4Addr::new(192, 0, 2, 30);
        enter_myroom(
            &mut world,
            &owner.identity,
            &owner.identity,
            stale_presented_ip,
            stale_presented_ip,
        );
        enter_myroom(
            &mut world,
            &guest.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 31),
            stale_presented_ip,
        );

        let channel = owner.identity.channel.unwrap();
        let token = MigrationToken::new(56_320).unwrap();
        world
            .identities
            .begin_migration(owner.session, channel, token, Instant::now())
            .unwrap();
        let (outbound, receiver) = mpsc::channel(8);
        let destination = world
            .register_session(
                SocketAddr::new(owner.identity.source_ip, 56_320),
                None,
                Some(outbound),
            )
            .unwrap();
        let preflight = world
            .preflight_migration(
                destination,
                owner.identity.user_no,
                channel.channel_id,
                token,
                Instant::now(),
            )
            .unwrap();
        let fresh_items = [0xAB; RIDER_ITEM_SNAPSHOT_WIRE_LENGTH];
        let profile = MyRoomProfileLease::for_test(
            MyRoomProfilePresentation::new(
                45_136,
                fresh_items,
                515_136,
                "FreshMigration".to_owned(),
            ),
            &owner.identity.nickname,
        );
        let completion = world
            .complete_preflighted_migration(preflight, Some(&profile), Instant::now())
            .unwrap();
        let mut migrated = TestChannelSession {
            session: destination,
            identity: completion.binding,
            outbound: receiver,
        };
        assert!(migrated.identity.generation.get() > owner.identity.generation.get());
        assert!(matches!(
            world.myroom_udp_targets(&owner.identity),
            Err(MyRoomLifecycleError::Hub { .. })
        ));
        assert_eq!(
            world.myroom_udp_targets(&migrated.identity).unwrap(),
            vec![guest.identity.clone()]
        );

        let snapshot = world.myroom.first_snapshot(&migrated.identity).unwrap();
        let MyRoomSlot::Player(presentation) = &snapshot.slots[0] else {
            panic!("migrated owner must remain in slot zero");
        };
        assert_eq!(presentation.p2p_address, Ipv4Addr::LOCALHOST);
        assert_eq!(presentation.p2p_port, 45_136);
        assert_eq!(presentation.rider_item_snapshot, fresh_items);
        assert_eq!(presentation.rp, 515_136);
        assert_eq!(presentation.club_name, "FreshMigration");
        assert!(owner.outbound.try_recv().is_err());
        assert!(migrated.outbound.try_recv().is_err());
        assert!(guest.outbound.try_recv().is_err());
        world.myroom.audit_invariants().unwrap();
    }

    #[test]
    fn invalid_migration_presentation_is_skipped_without_rejecting_identity_transfer() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "InvalidMoveOwner", 67, 56_330, 8);
        let mut guest = register_channel_session(&mut world, "InvalidMoveGuest", 67, 56_340, 8);
        enter_myroom(
            &mut world,
            &owner.identity,
            &owner.identity,
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::LOCALHOST,
        );
        enter_myroom(
            &mut world,
            &guest.identity,
            &owner.identity,
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::LOCALHOST,
        );
        let revision = world.myroom.revision();
        let snapshot = world.myroom.first_snapshot(&owner.identity).unwrap();

        let channel = owner.identity.channel.unwrap();
        let token = MigrationToken::new(56_350).unwrap();
        world
            .identities
            .begin_migration(owner.session, channel, token, Instant::now())
            .unwrap();
        let (outbound, _receiver) = mpsc::channel(8);
        let destination = world
            .register_session(
                SocketAddr::new(owner.identity.source_ip, 56_350),
                None,
                Some(outbound),
            )
            .unwrap();
        let preflight = world
            .preflight_migration(
                destination,
                owner.identity.user_no,
                channel.channel_id,
                token,
                Instant::now(),
            )
            .unwrap();
        let profile = MyRoomProfileLease::for_test(
            MyRoomProfilePresentation::new(
                45_136,
                [0xCD; RIDER_ITEM_SNAPSHOT_WIRE_LENGTH],
                515_136,
                "x".repeat(MAX_CLUB_NAME_UTF16_UNITS + 1),
            ),
            &owner.identity.nickname,
        );

        let completion = world
            .complete_preflighted_migration(preflight, Some(&profile), Instant::now())
            .unwrap();
        assert!(matches!(
            world.identities.authorize(owner.session),
            Err(IdentityError::StaleSession(actual)) if actual == owner.session
        ));
        assert_eq!(
            world.identities.authorize(destination).unwrap(),
            completion.binding
        );
        assert_eq!(
            completion.binding.generation.get(),
            owner
                .identity
                .generation
                .get()
                .max(guest.identity.generation.get())
                + 1
        );
        assert_eq!(
            world
                .identities
                .active_identity_by_user_no(owner.identity.user_no),
            Some(completion.binding.clone())
        );
        assert_eq!(world.myroom.revision().get(), revision.get() + 1);
        assert_eq!(
            world.myroom.first_snapshot(&completion.binding).unwrap(),
            snapshot
        );
        assert!(owner.outbound.try_recv().is_err());
        assert!(guest.outbound.try_recv().is_err());
        world.myroom.audit_invariants().unwrap();
    }

    #[tokio::test]
    async fn full_myroom_fanout_rolls_back_and_is_actor_terminal() {
        let mut world = World::default();
        let owner = register_channel_session(&mut world, "FullOwner", 67, 56_400, 1);
        let mut guest = register_channel_session(&mut world, "FullGuest", 67, 56_410, 1);
        enter_myroom(
            &mut world,
            &owner.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 40),
            Ipv4Addr::new(192, 0, 2, 40),
        );
        enter_myroom(
            &mut world,
            &guest.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 41),
            Ipv4Addr::new(192, 0, 2, 40),
        );
        world.sessions[&guest.session]
            .outbound
            .as_ref()
            .unwrap()
            .try_send(OutboundBatch::single(vec![0xA5]))
            .unwrap();
        let revision = world.myroom.revision();
        let (reply, response) = oneshot::channel();

        let result = dispatch_command(
            &mut world,
            WorldCommand::SessionClosed {
                id: owner.session,
                reply: Some(reply),
            },
            &WorldSidecars::default(),
            &ServerClock::new(),
        )
        .await;
        assert!(matches!(
            result,
            Err(WorldSidecarError::MyRoom(
                MyRoomLifecycleError::OutboundUnavailable { session }
            )) if session == guest.session
        ));
        assert!(response.await.is_err());
        assert_eq!(world.myroom.revision(), revision);
        assert_eq!(world.myroom.room_count(), 1);
        assert_eq!(world.myroom.member_count(), 2);
        assert_eq!(take_single_packet(&mut guest.outbound), vec![0xA5]);
    }

    #[test]
    fn shutdown_session_drain_retires_closed_connected_and_ownerless_myroom_peers() {
        let now = Instant::now();
        let mut world = World::default();
        let owner = register_channel_session(&mut world, "DrainOwner", 67, 56_450, 8);
        let guest = register_channel_session(&mut world, "DrainGuest", 67, 56_460, 8);
        let owner_session = owner.session;
        let guest_session = guest.session;
        let expected_users = [owner.identity.user_no, guest.identity.user_no];
        enter_myroom(
            &mut world,
            &owner.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 45),
            Ipv4Addr::new(192, 0, 2, 45),
        );
        enter_myroom(
            &mut world,
            &guest.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 46),
            Ipv4Addr::new(192, 0, 2, 45),
        );
        world
            .identities
            .begin_migration(
                guest_session,
                guest.identity.channel.unwrap(),
                MigrationToken::new(945).unwrap(),
                now,
            )
            .unwrap();
        world.close_session(guest_session, now).unwrap();
        assert!(
            world
                .identities
                .is_current_ownerless_binding(&guest.identity)
        );
        drop(owner.outbound);
        drop(guest.outbound);

        assert!(matches!(
            world.drain_sessions_for_shutdown(),
            Err(WorldOperationError::Command(
                WorldError::SessionDrainRequiresQuiesce
            ))
        ));
        world.quiesce();
        world.drain_sessions_for_shutdown().unwrap();

        assert!(world.sessions.is_empty());
        assert_eq!(world.identities.active_count(), 0);
        assert_eq!(world.myroom.room_count(), 0);
        assert_eq!(world.myroom.member_count(), 0);
        assert_eq!(world.myroom.generation_count(), 0);
        let released_users = world
            .identity_lifecycle
            .iter()
            .filter_map(|event| match event {
                super::IdentityLifecycleEvent::Release(identity) => Some(identity.user_no),
                super::IdentityLifecycleEvent::Announce(_)
                | super::IdentityLifecycleEvent::Advance { .. } => None,
            })
            .collect::<Vec<_>>();
        assert!(
            expected_users
                .iter()
                .all(|user_no| released_users.contains(user_no))
        );

        world.close_session(owner_session, now).unwrap();
        world.close_session(guest_session, now).unwrap();
        world.drain_sessions_for_shutdown().unwrap();
        world.myroom.audit_invariants().unwrap();
    }

    #[test]
    fn ownerless_migration_recipient_is_skipped_without_masking_stale_bindings() {
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "WaitOwner", 67, 56_500, 8);
        let guest = register_channel_session(&mut world, "WaitGuest", 67, 56_510, 8);
        let leaver = register_channel_session(&mut world, "WaitLeaver", 67, 56_520, 8);
        enter_myroom(
            &mut world,
            &owner.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 50),
            Ipv4Addr::new(192, 0, 2, 50),
        );
        enter_myroom(
            &mut world,
            &guest.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 51),
            Ipv4Addr::new(192, 0, 2, 50),
        );
        enter_myroom(
            &mut world,
            &leaver.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 52),
            Ipv4Addr::new(192, 0, 2, 50),
        );

        let token = MigrationToken::new(901).unwrap();
        world
            .identities
            .begin_migration(
                guest.session,
                guest.identity.channel.unwrap(),
                token,
                Instant::now(),
            )
            .unwrap();
        world.close_session(guest.session, Instant::now()).unwrap();
        assert!(
            world
                .identities
                .is_current_ownerless_binding(&guest.identity)
        );

        world.close_session(leaver.session, Instant::now()).unwrap();
        let snapshot = world.myroom.first_snapshot(&owner.identity).unwrap();
        assert_eq!(
            take_single_packet(&mut owner.outbound),
            serialize_slot_data(&snapshot.slots).unwrap()
        );
        assert_eq!(world.myroom.member_count(), 2);
        world.myroom.audit_invariants().unwrap();
    }

    #[test]
    fn same_tick_ownerless_expiry_skips_other_actor_minted_releases() {
        let issued_at = Instant::now();
        let mut world = World::default();
        let mut owner = register_channel_session(&mut world, "SweepOwner", 67, 56_600, 8);
        let first = register_channel_session(&mut world, "SweepFirst", 67, 56_610, 8);
        let second = register_channel_session(&mut world, "SweepSecond", 67, 56_620, 8);
        enter_myroom(
            &mut world,
            &owner.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 60),
            Ipv4Addr::new(192, 0, 2, 60),
        );
        enter_myroom(
            &mut world,
            &first.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 61),
            Ipv4Addr::new(192, 0, 2, 60),
        );
        enter_myroom(
            &mut world,
            &second.identity,
            &owner.identity,
            Ipv4Addr::new(192, 0, 2, 62),
            Ipv4Addr::new(192, 0, 2, 60),
        );
        for (identity, token_value) in [(&first, 902), (&second, 903)] {
            world
                .identities
                .begin_migration(
                    identity.session,
                    identity.identity.channel.unwrap(),
                    MigrationToken::new(token_value).unwrap(),
                    issued_at,
                )
                .unwrap();
            world.close_session(identity.session, issued_at).unwrap();
        }

        world
            .expire_migrations(issued_at + crate::MIGRATION_TTL)
            .unwrap();
        assert_eq!(world.myroom.member_count(), 1);
        assert_eq!(world.myroom.room_count(), 1);
        let mut publications = 0;
        while owner.outbound.try_recv().is_ok() {
            publications += 1;
        }
        assert_eq!(publications, 2);
        world.myroom.audit_invariants().unwrap();
    }

    #[tokio::test]
    async fn rejected_messenger_publication_stops_the_world_before_replying() {
        let defaults = MessengerRuntimeConfig::default();
        let config = MessengerRuntimeConfig {
            max_string_utf16_units: 3,
            hub_limits: MessengerHubLimits {
                max_message_utf16_units: 3,
                ..defaults.hub_limits
            },
            ..defaults
        };
        let (messenger, messenger_task) = MessengerServiceHandle::spawn(config).unwrap();
        let (world, world_task) = WorldHandle::spawn_with_messenger(8, messenger.clone());
        let session = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 54_000))
            .await
            .unwrap();

        assert!(matches!(
            world.claim_identity(session, "TooLong").await,
            Err(WorldError::Stopped)
        ));
        assert!(matches!(
            world_task.await.unwrap(),
            Err(MessengerServiceError::IdentityConflict)
        ));
        assert_eq!(messenger.snapshot().await.unwrap().announced_identities, 0);

        messenger.shutdown().await.unwrap();
        messenger_task.await.unwrap();
    }

    #[tokio::test]
    async fn world_rejects_identity_capacity_before_sidecar_divergence() {
        let config = MessengerRuntimeConfig {
            max_identities: 1,
            ..MessengerRuntimeConfig::default()
        };
        let (messenger, messenger_task) = MessengerServiceHandle::spawn(config).unwrap();
        let (world, world_task) = WorldHandle::spawn_with_messenger(8, messenger.clone());
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let first = world
            .register_session(SocketAddr::new(ip, 55_000))
            .await
            .unwrap();
        world.claim_identity(first, "First").await.unwrap();
        let second = world
            .register_session(SocketAddr::new(ip, 55_001))
            .await
            .unwrap();
        assert!(matches!(
            world.claim_identity(second, "Second").await,
            Err(WorldError::IdentityLimitReached { maximum: 1 })
        ));
        assert_eq!(messenger.snapshot().await.unwrap().announced_identities, 1);
        assert_eq!(world.session_count().await.unwrap(), 2);

        world.session_closed(first).await.unwrap();
        world.claim_identity(second, "Second").await.unwrap();
        assert_eq!(messenger.snapshot().await.unwrap().announced_identities, 1);
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
        messenger.shutdown().await.unwrap();
        messenger_task.await.unwrap();
    }

    #[tokio::test]
    async fn deferred_release_retains_capacity_until_its_operation_drains() {
        let config = MessengerRuntimeConfig {
            max_identities: 1,
            ..MessengerRuntimeConfig::default()
        };
        let (messenger, messenger_task) = MessengerServiceHandle::spawn(config).unwrap();
        let (world, world_task) = WorldHandle::spawn_with_messenger(8, messenger.clone());
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let first = world
            .register_session(SocketAddr::new(ip, 55_010))
            .await
            .unwrap();
        world.claim_identity(first, "DeferredFirst").await.unwrap();
        let operation = world.admit_identity_operation(first).await.unwrap();
        let second = world
            .register_session(SocketAddr::new(ip, 55_011))
            .await
            .unwrap();

        world.session_closed(first).await.unwrap();
        assert!(matches!(
            world.claim_identity(second, "DeferredSecond").await,
            Err(WorldError::IdentityLimitReached { maximum: 1 })
        ));
        assert_eq!(messenger.snapshot().await.unwrap().announced_identities, 1);

        // Claim dispatch first collects the now-drained tombstone and awaits its
        // sidecar Release, then admits and publishes the replacement Announce.
        drop(operation);
        world
            .claim_identity(second, "DeferredSecond")
            .await
            .unwrap();
        assert_eq!(messenger.snapshot().await.unwrap().announced_identities, 1);

        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
        messenger.shutdown().await.unwrap();
        messenger_task.await.unwrap();
    }
}
