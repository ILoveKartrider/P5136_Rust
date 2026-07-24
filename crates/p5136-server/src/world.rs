//! Deterministic, actor-owned server state.

use std::{
    array,
    collections::{BTreeSet, HashMap, HashSet, VecDeque},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::{Duration, Instant},
};

use p5136_core::{
    equipment_protocol::{EquipmentProtocolError, serialize_room_slot_items},
    room_protocol::{
        ChCreateRoomRequest, ChGetRoomListRequest, ChJoinRoomRequest, CreateRoomOutcome,
        JoinRoomStatus, ROOM_DATA_LENGTH, ROOM_OBSERVER_COUNT, ROOM_SLOT_COUNT, RoomListEntry,
        RoomMember as WireRoomMember, RoomObserver, RoomObserverSlot, RoomPlayer,
        RoomProtocolError, RoomSessionData, RoomSlotData, serialize_ch_create_room_reply,
        serialize_ch_get_room_list_reply, serialize_ch_join_room_reply,
        serialize_ch_leave_room_reply, serialize_gr_slot_data, serialize_initial_room_state,
    },
};
use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::MissedTickBehavior,
};

use crate::identity::{
    ChannelBinding, DisconnectOutcome, IdentityBinding, IdentityError, IdentityRegistry,
    MigrationCompletion, MigrationPermit, MigrationToken, ReleasedIdentity, UserNo,
};
use crate::messenger_hub::MessengerIdentity;
use crate::messenger_runtime::{MessengerServiceError, MessengerServiceHandle};
use crate::udp_runtime::{UdpDispatchRequest, UdpIngress, UdpService, UdpServiceError};
use crate::udp_state::UdpEndpointStateError;

pub const ROOM_CAPACITY: usize = 8;
pub(crate) const SESSION_OUTBOUND_CAPACITY: usize = 64;

/// One ordered write unit for a login session. A batch consumes one bounded
/// queue slot even when a protocol response contains many logical packets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutboundBatch {
    packets: Vec<Vec<u8>>,
}

impl OutboundBatch {
    #[must_use]
    pub(crate) fn single(packet: Vec<u8>) -> Self {
        Self {
            packets: vec![packet],
        }
    }

    #[must_use]
    pub(crate) fn ordered(packets: Vec<Vec<u8>>) -> Self {
        Self { packets }
    }

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
    pub(crate) started: bool,
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
pub enum WorldError {
    #[error("world actor has stopped")]
    Stopped,

    #[error(transparent)]
    Room(#[from] RoomError),

    #[error(transparent)]
    Identity(#[from] IdentityError),

    #[error(transparent)]
    RoomProtocol(#[from] RoomProtocolError),

    #[error(transparent)]
    EquipmentProtocol(#[from] EquipmentProtocolError),

    #[error("session {0:?} is not registered")]
    UnknownSession(SessionId),

    #[error("active identity limit {maximum} reached")]
    IdentityLimitReached { maximum: usize },
}

/// A terminal failure while publishing actor-owned state to a process sidecar.
///
/// Keeping this boundary distinct from [`WorldError`] lets request handlers
/// continue to report ordinary command failures without hiding a world/sidecar
/// divergence that requires the central runtime to stop.
#[derive(Debug, Error)]
pub enum WorldSidecarError {
    #[error("world messenger sidecar failed: {0}")]
    Messenger(#[from] MessengerServiceError),

    #[error("world UDP sidecar failed: {0}")]
    Udp(#[from] UdpServiceError),
}

#[derive(Debug)]
enum WorldCommand {
    RegisterSession {
        peer: SocketAddr,
        cancellation: Option<oneshot::Sender<()>>,
        outbound: Option<mpsc::Sender<OutboundBatch>>,
        reply: oneshot::Sender<SessionId>,
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
    CompleteMigration {
        destination: SessionId,
        user_no: UserNo,
        channel_id: u16,
        token: MigrationToken,
        now: Instant,
        reply: oneshot::Sender<Result<MigrationCompletion, WorldError>>,
    },
    RoomProtocol {
        session: SessionId,
        payload: Box<RoomCommandPayload>,
        reply: oneshot::Sender<Result<(), WorldError>>,
    },
    PublishRoomEquipment {
        session: SessionId,
        snapshot: Box<[u8; 65]>,
        reply: oneshot::Sender<Result<(), WorldError>>,
    },
    CreateRoom {
        reply: oneshot::Sender<RoomId>,
    },
    JoinRoom {
        room: RoomId,
        identity: String,
        reply: oneshot::Sender<Result<SlotId, RoomError>>,
    },
    JoinRoomForSession {
        room: RoomId,
        session: SessionId,
        reply: oneshot::Sender<Result<SlotId, WorldError>>,
    },
    LeaveRoom {
        identity: String,
        reply: oneshot::Sender<Result<(), RoomError>>,
    },
    RoomSnapshot {
        room: RoomId,
        reply: oneshot::Sender<Result<RoomSnapshot, RoomError>>,
    },
    SessionCount {
        reply: oneshot::Sender<usize>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

#[derive(Debug, Clone)]
pub struct WorldHandle {
    sender: mpsc::Sender<WorldCommand>,
    udp_sender: Option<mpsc::Sender<UdpIngress>>,
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
    #[must_use]
    pub fn spawn(mailbox_capacity: usize) -> (Self, JoinHandle<()>) {
        let (sender, receiver) = mpsc::channel(mailbox_capacity);
        let handle = Self {
            sender,
            udp_sender: None,
        };
        let task = tokio::spawn(async move {
            let result = run_world(receiver, None, WorldSidecars::default()).await;
            debug_assert!(result.is_ok());
        });
        (handle, task)
    }

    #[cfg(test)]
    pub(crate) fn spawn_with_messenger(
        mailbox_capacity: usize,
        messenger: MessengerServiceHandle,
    ) -> (Self, JoinHandle<Result<(), MessengerServiceError>>) {
        let (sender, receiver) = mpsc::channel(mailbox_capacity);
        let handle = Self {
            sender,
            udp_sender: None,
        };
        let sidecars = WorldSidecars {
            messenger: Some(messenger),
            udp: None,
        };
        let task = tokio::spawn(async move {
            match run_world(receiver, None, sidecars).await {
                Ok(()) => Ok(()),
                Err(WorldSidecarError::Messenger(error)) => Err(error),
                Err(WorldSidecarError::Udp(_)) => {
                    unreachable!("messenger-only world cannot report a UDP sidecar failure")
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
    ) -> (Self, JoinHandle<Result<(), WorldSidecarError>>) {
        let (sender, receiver) = mpsc::channel(mailbox_capacity);
        let (udp_sender, udp_receiver) = mpsc::channel(udp_mailbox_capacity);
        let handle = Self {
            sender,
            udp_sender: Some(udp_sender),
        };
        let sidecars = WorldSidecars {
            messenger: Some(messenger),
            udp: Some(udp),
        };
        let task = tokio::spawn(run_world(receiver, Some(udp_receiver), sidecars));
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
        self.register_session_inner(peer, None, None).await
    }

    pub(crate) async fn register_login_session(
        &self,
        peer: SocketAddr,
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
            .register_session_inner(peer, Some(cancel), Some(outbound))
            .await?;
        Ok((id, cancelled, outbound_receiver))
    }

    async fn register_session_inner(
        &self,
        peer: SocketAddr,
        cancellation: Option<oneshot::Sender<()>>,
        outbound: Option<mpsc::Sender<OutboundBatch>>,
    ) -> Result<SessionId, WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::RegisterSession {
                peer,
                cancellation,
                outbound,
                reply,
            })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)
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

    pub async fn complete_migration(
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

    pub async fn create_room(&self) -> Result<RoomId, WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::CreateRoom { reply })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)
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
        response
            .await
            .map_err(|_| WorldError::Stopped)?
            .map_err(WorldError::from)
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
        response
            .await
            .map_err(|_| WorldError::Stopped)?
            .map_err(WorldError::from)
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

    pub async fn shutdown(&self) -> Result<(), WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::Shutdown { reply })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)
    }
}

#[derive(Debug)]
struct World {
    sessions: HashMap<SessionId, SessionState>,
    identities: IdentityRegistry,
    rooms: HashMap<RoomId, RoomSnapshot>,
    room_by_identity: HashMap<String, RoomId>,
    protocol_rooms: HashMap<RoomId, ProtocolRoomState>,
    protocol_room_by_user: HashMap<UserNo, RoomId>,
    free_protocol_room_ids: BTreeSet<u16>,
    identity_lifecycle: VecDeque<IdentityLifecycleEvent>,
    identity_capacity: Option<usize>,
    next_session_id: u64,
    next_room_id: u32,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProtocolRoomMember {
    user_no: UserNo,
    player: RoomPlayer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProtocolRoomState {
    id: RoomId,
    settings: RoomSettings,
    room_master: i32,
    members_by_id: [Option<ProtocolRoomMember>; ROOM_SLOT_COUNT],
    observers: [Option<ProtocolRoomMember>; ROOM_OBSERVER_COUNT],
    slot_positions: [Option<u8>; ROOM_SLOT_COUNT],
}

type OutboundDelivery = (SessionId, OutboundBatch);

impl ProtocolRoomState {
    fn new(id: RoomId, settings: RoomSettings) -> Self {
        Self {
            id,
            settings,
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
        if let Some(member) = self
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
        }
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
            started: self.settings.started,
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

impl Default for World {
    fn default() -> Self {
        Self {
            sessions: HashMap::new(),
            identities: IdentityRegistry::new(),
            rooms: HashMap::new(),
            room_by_identity: HashMap::new(),
            protocol_rooms: HashMap::new(),
            protocol_room_by_user: HashMap::new(),
            free_protocol_room_ids: BTreeSet::new(),
            identity_lifecycle: VecDeque::new(),
            identity_capacity: None,
            next_session_id: 1,
            next_room_id: 1,
        }
    }
}

impl World {
    fn register_session(
        &mut self,
        peer: SocketAddr,
        cancellation: Option<oneshot::Sender<()>>,
        outbound: Option<mpsc::Sender<OutboundBatch>>,
    ) -> SessionId {
        let id = SessionId::new(self.next_session_id);
        self.next_session_id = self.next_session_id.wrapping_add(1).max(1);
        self.sessions.insert(
            id,
            SessionState {
                peer,
                cancellation,
                outbound,
            },
        );
        id
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
        if !room.settings.started {
            return Vec::new();
        }

        room.user_nos()
            .into_iter()
            .filter(|user_no| *user_no != source)
            .filter_map(|user_no| self.identities.active_identity_by_user_no(user_no))
            .collect()
    }

    fn claim_identity(
        &mut self,
        session: SessionId,
        nickname: &str,
    ) -> Result<IdentityBinding, WorldError> {
        let source_ip = self.session_ip(session)?;
        if let Some(maximum) = self.identity_capacity
            && self.identities.active_count() >= maximum
        {
            return Err(WorldError::IdentityLimitReached { maximum });
        }
        let binding = self.identities.claim(session, source_ip, nickname)?;
        self.identity_lifecycle
            .push_back(IdentityLifecycleEvent::Announce(binding.clone()));
        Ok(binding)
    }

    fn complete_migration(
        &mut self,
        destination: SessionId,
        user_no: UserNo,
        channel_id: u16,
        token: MigrationToken,
        now: Instant,
    ) -> Result<MigrationCompletion, WorldError> {
        let destination_ip = self.session_ip(destination)?;
        let completion = self.identities.complete_migration(
            destination,
            destination_ip,
            user_no,
            channel_id,
            token,
            now,
        )?;
        self.identity_lifecycle
            .push_back(IdentityLifecycleEvent::Advance {
                previous: completion.previous_identity.clone(),
                next: completion.binding.clone(),
            });
        let changed_room_channel = self
            .protocol_room_by_user
            .get(&completion.binding.user_no)
            .and_then(|room_id| self.protocol_rooms.get(room_id))
            .is_some_and(|room| completion.binding.channel != Some(room.settings.channel));
        let deliveries = if changed_room_channel {
            self.remove_protocol_user(completion.binding.user_no)
        } else {
            Vec::new()
        };
        if let Some(previous_owner) = completion.previous_owner {
            self.cancel_session(previous_owner);
        }
        self.deliver(deliveries, now);
        Ok(completion)
    }

    fn room_protocol(
        &mut self,
        session: SessionId,
        payload: RoomCommandPayload,
    ) -> Result<(), WorldError> {
        let identity = self.identities.authorize(session)?;
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
            RoomCommandPayload::Leave => {
                self.protocol_leave_room(session, &identity);
                Ok(())
            }
            RoomCommandPayload::FirstState => self.protocol_first_state(session, &identity),
        }
    }

    fn publish_room_equipment(
        &mut self,
        session: SessionId,
        snapshot: [u8; 65],
    ) -> Result<(), WorldError> {
        let identity = self.identities.authorize(session)?;
        let Some(room_id) = self.protocol_room_by_user.get(&identity.user_no).copied() else {
            return Ok(());
        };
        let room = self
            .protocol_rooms
            .get_mut(&room_id)
            .expect("protocol membership always references an existing room");
        let Some(player_id) = room.equipment_player_id(identity.user_no) else {
            debug_assert!(false, "protocol membership map and room state diverged");
            return Ok(());
        };
        let packet = serialize_room_slot_items(player_id, &snapshot)?;
        let recipients = room
            .user_nos()
            .into_iter()
            .filter(|user_no| *user_no != identity.user_no)
            .collect();
        let updated = room.set_equipment_snapshot(identity.user_no, snapshot);
        debug_assert!(updated, "protocol membership map and room state diverged");
        let deliveries = self.deliveries_for_users(recipients, &OutboundBatch::single(packet));
        self.deliver(deliveries, Instant::now());
        self.debug_assert_invariants();
        Ok(())
    }

    fn protocol_room_list(
        &mut self,
        session: SessionId,
        identity: &IdentityBinding,
        request: ChGetRoomListRequest,
    ) -> Result<(), WorldError> {
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
        );
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
    ) -> Result<(), WorldError> {
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
                started: false,
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
        );
        self.debug_assert_invariants();
        Ok(())
    }

    fn protocol_join_room(
        &mut self,
        session: SessionId,
        identity: &IdentityBinding,
        request: &ChJoinRoomRequest,
        participant: RoomParticipant,
    ) -> Result<(), WorldError> {
        let room_id = RoomId(u32::from(request.room_id));
        let mut replacement = None;
        let (status, game_type) = match self.protocol_rooms.get(&room_id) {
            None => (JoinRoomStatus::Unavailable, 0),
            Some(room)
                if room.settings.started
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
        );
        self.debug_assert_invariants();
        Ok(())
    }

    fn protocol_leave_room(&mut self, session: SessionId, identity: &IdentityBinding) {
        let left = self.protocol_room_by_user.contains_key(&identity.user_no);
        let mut deliveries = vec![(
            session,
            OutboundBatch::single(serialize_ch_leave_room_reply(left)),
        )];
        if left {
            deliveries.extend(self.remove_protocol_user(identity.user_no));
        }
        self.deliver(deliveries, Instant::now());
        self.debug_assert_invariants();
    }

    fn protocol_first_state(
        &mut self,
        session: SessionId,
        identity: &IdentityBinding,
    ) -> Result<(), WorldError> {
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
        self.deliver(deliveries, Instant::now());
        Ok(())
    }

    fn remove_protocol_user(&mut self, user_no: UserNo) -> Vec<OutboundDelivery> {
        let Some(room_id) = self.protocol_room_by_user.remove(&user_no) else {
            return Vec::new();
        };
        let mut remove_room = false;
        let mut broadcast = None;
        if let Some(room) = self.protocol_rooms.get_mut(&room_id) {
            let removed = room.remove_user(user_no);
            debug_assert!(removed, "protocol membership map and room state diverged");
            if room.is_empty() {
                remove_room = true;
            } else {
                let packet = serialize_gr_slot_data(&room.slot_data())
                    .expect("validated protocol room state remains serializable after removal");
                broadcast = Some((room.user_nos(), packet));
            }
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
                    .map(|identity| (identity.owner, batch.clone()))
            })
            .collect()
    }

    fn deliver(&mut self, deliveries: Vec<OutboundDelivery>, now: Instant) {
        let mut pending = VecDeque::from(deliveries);
        let mut failed_sessions = HashSet::new();
        while let Some((session, batch)) = pending.pop_front() {
            let outbound = self
                .sessions
                .get(&session)
                .and_then(|state| state.outbound.clone());
            let failed = match outbound {
                Some(outbound) => outbound.try_send(batch).is_err(),
                None => self.sessions.contains_key(&session),
            };
            if failed && failed_sessions.insert(session) {
                pending.extend(self.close_session_state(session, now));
            }
        }
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

    fn close_session(&mut self, session: SessionId, now: Instant) {
        let deliveries = self.close_session_state(session, now);
        self.deliver(deliveries, now);
    }

    fn close_session_state(&mut self, session: SessionId, now: Instant) -> Vec<OutboundDelivery> {
        if let Some(mut state) = self.sessions.remove(&session)
            && let Some(cancellation) = state.cancellation.take()
        {
            let _ = cancellation.send(());
        }
        match self.identities.disconnect(session, now) {
            DisconnectOutcome::Released(identity) => self.release_identity_state(&identity),
            DisconnectOutcome::Unauthenticated
            | DisconnectOutcome::Stale(_)
            | DisconnectOutcome::Deferred { .. } => Vec::new(),
        }
    }

    fn expire_migrations(&mut self, now: Instant) {
        let identities = self.identities.expire_migrations(now);
        let deliveries = identities
            .iter()
            .flat_map(|identity| self.release_identity_state(identity))
            .collect();
        self.deliver(deliveries, now);
    }

    fn release_identity_state(&mut self, identity: &ReleasedIdentity) -> Vec<OutboundDelivery> {
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
        deliveries
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
        let identity = self.identities.authorize(session)?;
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
            }
            debug_assert_eq!(seen_users.len(), self.protocol_room_by_user.len());
            for room_id in &self.free_protocol_room_ids {
                debug_assert_ne!(*room_id, 0);
                let room_id = RoomId(u32::from(*room_id));
                debug_assert!(!self.rooms.contains_key(&room_id));
                debug_assert!(!self.protocol_rooms.contains_key(&room_id));
            }
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

async fn receive_udp_ingress(
    receiver: &mut Option<mpsc::Receiver<UdpIngress>>,
) -> Option<UdpIngress> {
    match receiver {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

async fn dispatch_udp_ingress(
    world: &World,
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
    let Some(identity) = world.identities.active_identity_by_user_no(user_no) else {
        tracing::trace!(
            account_id = ingress.account_id,
            source = %ingress.source,
            "dropping UDP ingress without an active world identity"
        );
        return Ok(());
    };
    let account_id = ingress.account_id;
    let source = ingress.source;
    let request = UdpDispatchRequest {
        ingress,
        identity,
        racing_targets: world.racing_udp_targets(user_no),
        // MyRoom is a distinct feature and does not share protocol-room
        // membership. Its audience remains empty until that state is ported.
        room_targets: Vec::new(),
    };

    match udp.dispatch(request).await {
        Ok(_) => Ok(()),
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
    }
}

async fn run_world(
    mut receiver: mpsc::Receiver<WorldCommand>,
    mut udp_receiver: Option<mpsc::Receiver<UdpIngress>>,
    sidecars: WorldSidecars,
) -> Result<(), WorldSidecarError> {
    let mut world = World {
        identity_capacity: sidecars.identity_capacity(),
        ..World::default()
    };
    let mut migration_expiry = tokio::time::interval(Duration::from_secs(1));
    migration_expiry.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;

            command = receiver.recv() => {
                let Some(command) = command else {
                    break;
                };
                if dispatch_command(&mut world, command, &sidecars).await? {
                    break;
                }
            }
            _ = migration_expiry.tick() => {
                world.expire_migrations(Instant::now());
                flush_identity_lifecycle(&mut world, &sidecars).await?;
            }
            ingress = receive_udp_ingress(&mut udp_receiver) => {
                let Some(ingress) = ingress else {
                    udp_receiver = None;
                    continue;
                };
                let Some(udp) = sidecars.udp.as_ref() else {
                    debug_assert!(false, "UDP ingress mailbox requires a UDP sidecar");
                    continue;
                };
                dispatch_udp_ingress(&world, udp, ingress).await?;
            }
        }
    }
    flush_identity_lifecycle(&mut world, &sidecars).await?;
    Ok(())
}

async fn dispatch_command(
    world: &mut World,
    command: WorldCommand,
    sidecars: &WorldSidecars,
) -> Result<bool, WorldSidecarError> {
    match command {
        WorldCommand::RegisterSession {
            peer,
            cancellation,
            outbound,
            reply,
        } => {
            let result = world.register_session(peer, cancellation, outbound);
            reply_after_identity_lifecycle(world, sidecars, reply, result).await?;
        }
        WorldCommand::SessionClosed { id, reply } => {
            world.close_session(id, Instant::now());
            flush_identity_lifecycle(world, sidecars).await?;
            if let Some(reply) = reply {
                let _ = reply.send(());
            }
        }
        WorldCommand::ClaimIdentity {
            session,
            nickname,
            reply,
        } => {
            let result = world.claim_identity(session, &nickname);
            reply_after_identity_lifecycle(world, sidecars, reply, result).await?;
        }
        WorldCommand::AuthorizeIdentity { session, reply } => {
            let result = world
                .identities
                .authorize(session)
                .map_err(WorldError::from);
            reply_after_identity_lifecycle(world, sidecars, reply, result).await?;
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
        WorldCommand::CompleteMigration {
            destination,
            user_no,
            channel_id,
            token,
            now,
            reply,
        } => {
            let result = world.complete_migration(destination, user_no, channel_id, token, now);
            reply_after_identity_lifecycle(world, sidecars, reply, result).await?;
        }
        WorldCommand::RoomProtocol {
            session,
            payload,
            reply,
        } => {
            let result = world.room_protocol(session, *payload);
            reply_after_identity_lifecycle(world, sidecars, reply, result).await?;
        }
        WorldCommand::PublishRoomEquipment {
            session,
            snapshot,
            reply,
        } => {
            let result = world.publish_room_equipment(session, *snapshot);
            reply_after_identity_lifecycle(world, sidecars, reply, result).await?;
        }
        command => return dispatch_utility_command(world, command, sidecars).await,
    }
    Ok(false)
}

async fn dispatch_utility_command(
    world: &mut World,
    command: WorldCommand,
    sidecars: &WorldSidecars,
) -> Result<bool, WorldSidecarError> {
    match command {
        WorldCommand::CreateRoom { reply } => {
            let result = world.create_room();
            reply_after_identity_lifecycle(world, sidecars, reply, result).await?;
        }
        WorldCommand::JoinRoom {
            room,
            identity,
            reply,
        } => {
            let result = world.join_room(room, identity);
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
            let result = world.leave_room(&identity);
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
        WorldCommand::Shutdown { reply } => {
            flush_identity_lifecycle(world, sidecars).await?;
            world.cancel_all_sessions();
            let _ = reply.send(());
            return Ok(true);
        }
        WorldCommand::RegisterSession { .. }
        | WorldCommand::SessionClosed { .. }
        | WorldCommand::ClaimIdentity { .. }
        | WorldCommand::AuthorizeIdentity { .. }
        | WorldCommand::BeginMigration { .. }
        | WorldCommand::CompleteMigration { .. }
        | WorldCommand::RoomProtocol { .. }
        | WorldCommand::PublishRoomEquipment { .. } => {
            unreachable!("identity-affecting commands are dispatched by dispatch_command")
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        time::{Duration, Instant},
    };

    use p5136_core::{
        adler32,
        room_protocol::{
            ChCreateRoomRequest, ChGetRoomListRequest, ChJoinRoomRequest,
            ROOM_CONNECTION_CONTEXT_LENGTH, ROOM_DATA_LENGTH, RoomPlayer, RoomProtocolError,
        },
        startup::RIDER_ITEM_SNAPSHOT_WIRE_LENGTH,
    };
    use tokio::{
        net::UdpSocket,
        sync::{mpsc, oneshot},
    };

    use super::{
        OutboundBatch, ROOM_CAPACITY, RoomCommandPayload, RoomError, RoomId, RoomParticipant,
        SessionId, World, WorldCommand, WorldError, WorldHandle,
    };
    use crate::{
        ChannelBinding, IdentityBinding, IdentityError, MessengerHubLimits, MessengerRuntimeConfig,
        MessengerServiceError, MessengerServiceHandle, MigrationToken, UdpDispatchRequest,
        UdpEndpointStateError, UdpIngress, UdpIngressBody, UdpRuntime, UdpRuntimeConfig,
        UdpServiceError, UdpTransport,
    };

    struct TestChannelSession {
        session: SessionId,
        identity: IdentityBinding,
        outbound: mpsc::Receiver<OutboundBatch>,
    }

    fn register_channel_session(
        world: &mut World,
        nickname: &str,
        channel_game_type: u8,
        port: u16,
        outbound_capacity: usize,
    ) -> TestChannelSession {
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let source = world.register_session(SocketAddr::new(ip, port), None, None);
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
        let destination = world.register_session(
            SocketAddr::new(ip, port.wrapping_add(1)),
            None,
            Some(outbound),
        );
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
        let destination = world.register_session(
            SocketAddr::new(source.identity.source_ip, destination_port),
            None,
            Some(outbound),
        );
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

    #[test]
    fn racing_udp_targets_require_started_room_and_include_observers() {
        let mut world = World::default();
        let source = register_channel_session(&mut world, "Source", 67, 40_001, 64);
        let player = register_channel_session(&mut world, "Player", 67, 40_011, 64);
        let observer = register_channel_session(&mut world, "Observer", 67, 40_021, 64);

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
            .protocol_rooms
            .get_mut(&room_id)
            .unwrap()
            .settings
            .started = true;

        let targets = world.racing_udp_targets(source.identity.user_no);
        assert_eq!(targets.len(), 2);
        assert!(targets.contains(&player.identity));
        assert!(targets.contains(&observer.identity));
        assert!(!targets.contains(&source.identity));
    }

    #[tokio::test]
    async fn concurrent_joins_are_serialized_into_unique_slots() {
        let (world, task) = WorldHandle::spawn(64);
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
        task.await.unwrap();
    }

    #[tokio::test]
    async fn migration_cancels_old_owner_and_rejects_its_queued_mutation() {
        let (world, task) = WorldHandle::spawn(64);
        let peer = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 50_000);
        let (source, source_cancelled, _outbound) =
            world.register_login_session(peer).await.unwrap();
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
        task.await.unwrap();
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
            Err(WorldError::Identity(IdentityError::StaleSession(session)))
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
            Err(WorldError::Identity(IdentityError::StaleSession(session)))
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
            Err(WorldError::RoomProtocol(RoomProtocolError::LimitExceeded {
                field: "rider card",
                ..
            }))
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
        let (world, world_task) =
            WorldHandle::spawn_with_services(64, 64, messenger.clone(), udp.clone());
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
}
