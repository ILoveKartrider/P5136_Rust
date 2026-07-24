//! Deterministic, actor-owned server state.

use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
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

pub const ROOM_CAPACITY: usize = 8;

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

    #[error("session {0:?} is not registered")]
    UnknownSession(SessionId),
}

#[derive(Debug)]
enum WorldCommand {
    RegisterSession {
        peer: SocketAddr,
        cancellation: Option<oneshot::Sender<()>>,
        reply: oneshot::Sender<SessionId>,
    },
    SessionClosed {
        id: SessionId,
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
}

impl WorldHandle {
    #[must_use]
    pub fn spawn(mailbox_capacity: usize) -> (Self, JoinHandle<()>) {
        let (sender, receiver) = mpsc::channel(mailbox_capacity);
        let handle = Self { sender };
        let task = tokio::spawn(run_world(receiver));
        (handle, task)
    }

    pub async fn register_session(&self, peer: SocketAddr) -> Result<SessionId, WorldError> {
        self.register_session_inner(peer, None).await
    }

    pub(crate) async fn register_login_session(
        &self,
        peer: SocketAddr,
    ) -> Result<(SessionId, oneshot::Receiver<()>), WorldError> {
        let (cancel, cancelled) = oneshot::channel();
        let id = self.register_session_inner(peer, Some(cancel)).await?;
        Ok((id, cancelled))
    }

    async fn register_session_inner(
        &self,
        peer: SocketAddr,
        cancellation: Option<oneshot::Sender<()>>,
    ) -> Result<SessionId, WorldError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::RegisterSession {
                peer,
                cancellation,
                reply,
            })
            .await
            .map_err(|_| WorldError::Stopped)?;
        response.await.map_err(|_| WorldError::Stopped)
    }

    pub async fn session_closed(&self, id: SessionId) -> Result<(), WorldError> {
        self.sender
            .send(WorldCommand::SessionClosed { id })
            .await
            .map_err(|_| WorldError::Stopped)
    }

    pub(crate) fn try_session_closed(&self, id: SessionId) {
        let _ = self.sender.try_send(WorldCommand::SessionClosed { id });
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
    next_session_id: u64,
    next_room_id: u32,
}

#[derive(Debug)]
struct SessionState {
    peer: SocketAddr,
    cancellation: Option<oneshot::Sender<()>>,
}

impl Default for World {
    fn default() -> Self {
        Self {
            sessions: HashMap::new(),
            identities: IdentityRegistry::new(),
            rooms: HashMap::new(),
            room_by_identity: HashMap::new(),
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
    ) -> SessionId {
        let id = SessionId::new(self.next_session_id);
        self.next_session_id = self.next_session_id.wrapping_add(1).max(1);
        self.sessions
            .insert(id, SessionState { peer, cancellation });
        id
    }

    fn session_ip(&self, session: SessionId) -> Result<IpAddr, WorldError> {
        self.sessions
            .get(&session)
            .map(|state| state.peer.ip())
            .ok_or(WorldError::UnknownSession(session))
    }

    fn claim_identity(
        &mut self,
        session: SessionId,
        nickname: &str,
    ) -> Result<IdentityBinding, WorldError> {
        let source_ip = self.session_ip(session)?;
        Ok(self.identities.claim(session, source_ip, nickname)?)
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
        if let Some(previous_owner) = completion.previous_owner {
            self.cancel_session(previous_owner);
        }
        Ok(completion)
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
        self.sessions.remove(&session);
        if let DisconnectOutcome::Released(identity) = self.identities.disconnect(session, now) {
            self.release_identity(&identity);
        }
    }

    fn expire_migrations(&mut self, now: Instant) {
        for identity in self.identities.expire_migrations(now) {
            self.release_identity(&identity);
        }
    }

    fn release_identity(&mut self, identity: &ReleasedIdentity) {
        let Some(room_id) = self.room_by_identity.remove(&identity.nickname) else {
            return;
        };
        if let Some(room) = self.rooms.get_mut(&room_id)
            && let Some(slot) = room
                .slots
                .iter_mut()
                .find(|slot| slot.as_deref() == Some(identity.nickname.as_str()))
        {
            *slot = None;
        }
        self.debug_assert_invariants();
    }

    fn create_room(&mut self) -> RoomId {
        let id = RoomId(self.next_room_id);
        self.next_room_id = self.next_room_id.wrapping_add(1).max(1);
        self.rooms.insert(
            id,
            RoomSnapshot {
                id,
                slots: std::array::from_fn(|_| None),
            },
        );
        id
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
            let mut seen = std::collections::HashSet::new();
            for (room_id, room) in &self.rooms {
                for identity in room.slots.iter().flatten() {
                    debug_assert!(seen.insert(identity));
                    debug_assert_eq!(self.room_by_identity.get(identity), Some(room_id));
                }
            }
            debug_assert_eq!(seen.len(), self.room_by_identity.len());
        }
    }
}

async fn run_world(mut receiver: mpsc::Receiver<WorldCommand>) {
    let mut world = World::default();
    let mut migration_expiry = tokio::time::interval(Duration::from_secs(1));
    migration_expiry.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        let command = tokio::select! {
            command = receiver.recv() => {
                let Some(command) = command else {
                    break;
                };
                command
            }
            _ = migration_expiry.tick() => {
                world.expire_migrations(Instant::now());
                continue;
            }
        };

        if dispatch_command(&mut world, command) {
            break;
        }
    }
}

fn dispatch_command(world: &mut World, command: WorldCommand) -> bool {
    match command {
        WorldCommand::RegisterSession {
            peer,
            cancellation,
            reply,
        } => {
            let _ = reply.send(world.register_session(peer, cancellation));
        }
        WorldCommand::SessionClosed { id } => world.close_session(id, Instant::now()),
        WorldCommand::ClaimIdentity {
            session,
            nickname,
            reply,
        } => {
            let _ = reply.send(world.claim_identity(session, &nickname));
        }
        WorldCommand::AuthorizeIdentity { session, reply } => {
            let result = world
                .identities
                .authorize(session)
                .map_err(WorldError::from);
            let _ = reply.send(result);
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
            let _ = reply.send(result);
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
            let _ = reply.send(result);
        }
        WorldCommand::CreateRoom { reply } => {
            let _ = reply.send(world.create_room());
        }
        WorldCommand::JoinRoom {
            room,
            identity,
            reply,
        } => {
            let _ = reply.send(world.join_room(room, identity));
        }
        WorldCommand::JoinRoomForSession {
            room,
            session,
            reply,
        } => {
            let _ = reply.send(world.join_room_for_session(room, session));
        }
        WorldCommand::LeaveRoom { identity, reply } => {
            let _ = reply.send(world.leave_room(&identity));
        }
        WorldCommand::RoomSnapshot { room, reply } => {
            let result = world
                .rooms
                .get(&room)
                .cloned()
                .ok_or(RoomError::NotFound(room.0));
            let _ = reply.send(result);
        }
        WorldCommand::SessionCount { reply } => {
            let _ = reply.send(world.sessions.len());
        }
        WorldCommand::Shutdown { reply } => {
            world.cancel_all_sessions();
            let _ = reply.send(());
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        time::{Duration, Instant},
    };

    use tokio::sync::oneshot;

    use super::{ROOM_CAPACITY, RoomError, WorldCommand, WorldError, WorldHandle};
    use crate::{ChannelBinding, IdentityError, MigrationToken};

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
        let (source, source_cancelled) = world.register_login_session(peer).await.unwrap();
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
}
