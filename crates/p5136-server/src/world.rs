//! Deterministic, actor-owned server state.

use std::{collections::HashMap, net::SocketAddr};

use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

pub const ROOM_CAPACITY: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(u64);

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
}

#[derive(Debug)]
enum WorldCommand {
    RegisterSession {
        peer: SocketAddr,
        reply: oneshot::Sender<SessionId>,
    },
    SessionClosed {
        id: SessionId,
    },
    CreateRoom {
        reply: oneshot::Sender<RoomId>,
    },
    JoinRoom {
        room: RoomId,
        identity: String,
        reply: oneshot::Sender<Result<SlotId, RoomError>>,
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
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WorldCommand::RegisterSession { peer, reply })
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
    sessions: HashMap<SessionId, SocketAddr>,
    rooms: HashMap<RoomId, RoomSnapshot>,
    room_by_identity: HashMap<String, RoomId>,
    next_session_id: u64,
    next_room_id: u32,
}

impl Default for World {
    fn default() -> Self {
        Self {
            sessions: HashMap::new(),
            rooms: HashMap::new(),
            room_by_identity: HashMap::new(),
            next_session_id: 1,
            next_room_id: 1,
        }
    }
}

impl World {
    fn register_session(&mut self, peer: SocketAddr) -> SessionId {
        let id = SessionId(self.next_session_id);
        self.next_session_id = self.next_session_id.wrapping_add(1).max(1);
        self.sessions.insert(id, peer);
        id
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
    while let Some(command) = receiver.recv().await {
        match command {
            WorldCommand::RegisterSession { peer, reply } => {
                let _ = reply.send(world.register_session(peer));
            }
            WorldCommand::SessionClosed { id } => {
                world.sessions.remove(&id);
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
                let _ = reply.send(());
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{ROOM_CAPACITY, RoomError, WorldError, WorldHandle};

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
}
