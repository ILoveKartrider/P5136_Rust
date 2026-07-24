//! Pure, actor-owned state for the P5136 messenger service.
//!
//! This module deliberately performs no socket I/O and owns no asynchronous
//! tasks. A runtime supplies the current login identity for every action,
//! serializes returned events, and delivers them through bounded single-writer
//! queues.

use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
    net::IpAddr,
    num::{NonZeroU32, NonZeroU64},
    sync::Arc,
};

use p5136_core::nickname::canonical_nickname_key;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MessengerSessionId(NonZeroU64);

impl MessengerSessionId {
    #[must_use]
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MessengerGeneration(NonZeroU64);

impl MessengerGeneration {
    #[must_use]
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MessengerRoomId(NonZeroU32);

impl MessengerRoomId {
    #[must_use]
    pub const fn new(value: u32) -> Option<Self> {
        match NonZeroU32::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessengerIdentity {
    pub user_no: NonZeroU32,
    pub nickname: String,
    pub generation: MessengerGeneration,
    pub source_ip: IpAddr,
    key: String,
}

impl MessengerIdentity {
    pub fn new(
        user_no: u32,
        nickname: impl Into<String>,
        generation: u64,
        source_ip: IpAddr,
    ) -> Result<Self, MessengerHubError> {
        let user_no = NonZeroU32::new(user_no).ok_or(MessengerHubError::InvalidUserNo)?;
        let generation =
            MessengerGeneration::new(generation).ok_or(MessengerHubError::InvalidGeneration)?;
        let nickname = nickname.into();
        if nickname.trim().is_empty() {
            return Err(MessengerHubError::EmptyNickname);
        }
        let key = canonical_nickname_key(&nickname);
        Ok(Self {
            user_no,
            nickname,
            generation,
            source_ip,
            key,
        })
    }

    #[must_use]
    pub fn canonical_key(&self) -> &str {
        &self.key
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessengerHubLimits {
    pub max_sessions: usize,
    pub max_rooms: usize,
    pub max_rooms_per_identity: usize,
    pub max_message_utf16_units: usize,
}

impl Default for MessengerHubLimits {
    fn default() -> Self {
        Self {
            max_sessions: 256,
            max_rooms: 4_096,
            max_rooms_per_identity: 64,
            max_message_utf16_units: 4_096,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnterClaim {
    pub user_no: u32,
    pub chat_type: u32,
    pub nickname: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteClaim {
    pub inviter_user_no: u32,
    pub invitee_user_no: u32,
    pub inviter_nickname: String,
    pub invitee_nickname: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatClaim {
    pub room_id: u32,
    pub nickname: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaveClaim {
    pub user_no: u32,
    pub room_id: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuildChatClaim {
    pub nickname: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessengerEvent {
    InviteChat {
        inviter_user_no: u32,
        invitee_user_no: u32,
        inviter_nickname: String,
        invitee_nickname: String,
        room_id: MessengerRoomId,
        result: i32,
    },
    Chat {
        room_id: MessengerRoomId,
        sender_user_no: u32,
        nickname: String,
        message: Arc<str>,
        result: i32,
    },
    LeaveChat {
        user_no: u32,
        room_id: MessengerRoomId,
    },
    GuildChat {
        nickname: String,
        message: Arc<str>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessengerDelivery {
    pub session: MessengerSessionId,
    pub event: MessengerEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnterOutcome {
    pub replaced_session: Option<MessengerSessionId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerationAdvance {
    pub endpoint_updated: bool,
    pub room_members_updated: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentityRelease {
    pub disconnected_session: Option<MessengerSessionId>,
    pub room_memberships_removed: usize,
    pub empty_rooms_removed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessengerHubError {
    InvalidUserNo,
    InvalidGeneration,
    EmptyNickname,
    ZeroLimit(&'static str),
    SessionAlreadyEntered(MessengerSessionId),
    SessionLimitReached {
        maximum: usize,
    },
    UnauthenticatedSession(MessengerSessionId),
    StaleSession(MessengerSessionId),
    SourceIpMismatch {
        expected: IpAddr,
        received: IpAddr,
    },
    SenderUserNoMismatch,
    SenderNicknameMismatch,
    TargetIdentityMismatch,
    SelfInvite,
    RoomLimitReached {
        maximum: usize,
    },
    IdentityRoomLimitReached {
        nickname: String,
        maximum: usize,
    },
    RoomIdExhausted,
    InvalidRoomId(u32),
    RoomNotFound(MessengerRoomId),
    NotRoomMember {
        room_id: MessengerRoomId,
        nickname: String,
    },
    MessageTooLong {
        length: usize,
        maximum: usize,
    },
    MigrationIdentityMismatch,
}

impl fmt::Display for MessengerHubError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUserNo => formatter.write_str("messenger user number must be non-zero"),
            Self::InvalidGeneration => {
                formatter.write_str("messenger identity generation must be non-zero")
            }
            Self::EmptyNickname => formatter.write_str("messenger nickname must not be empty"),
            Self::ZeroLimit(name) => write!(formatter, "messenger limit {name} must be non-zero"),
            Self::SessionAlreadyEntered(session) => {
                write!(
                    formatter,
                    "messenger session {} already entered",
                    session.get()
                )
            }
            Self::SessionLimitReached { maximum } => {
                write!(formatter, "messenger session limit {maximum} reached")
            }
            Self::UnauthenticatedSession(session) => {
                write!(
                    formatter,
                    "messenger session {} is unauthenticated",
                    session.get()
                )
            }
            Self::StaleSession(session) => {
                write!(formatter, "messenger session {} is stale", session.get())
            }
            Self::SourceIpMismatch { expected, received } => {
                write!(
                    formatter,
                    "messenger source IP mismatch: expected {expected}, received {received}"
                )
            }
            Self::SenderUserNoMismatch => {
                formatter.write_str("messenger sender user number does not match its identity")
            }
            Self::SenderNicknameMismatch => {
                formatter.write_str("messenger sender nickname does not match its identity")
            }
            Self::TargetIdentityMismatch => {
                formatter.write_str("messenger invite target does not match an active identity")
            }
            Self::SelfInvite => formatter.write_str("messenger self-invites are not supported"),
            Self::RoomLimitReached { maximum } => {
                write!(formatter, "messenger room limit {maximum} reached")
            }
            Self::IdentityRoomLimitReached { nickname, maximum } => {
                write!(
                    formatter,
                    "messenger identity {nickname:?} reached its room limit {maximum}"
                )
            }
            Self::RoomIdExhausted => formatter.write_str("messenger room ID space is exhausted"),
            Self::InvalidRoomId(room_id) => {
                write!(formatter, "messenger room ID {room_id} is invalid")
            }
            Self::RoomNotFound(room_id) => {
                write!(formatter, "messenger room {} does not exist", room_id.get())
            }
            Self::NotRoomMember { room_id, nickname } => {
                write!(
                    formatter,
                    "messenger identity {nickname:?} is not in room {}",
                    room_id.get()
                )
            }
            Self::MessageTooLong { length, maximum } => {
                write!(
                    formatter,
                    "messenger message has {length} UTF-16 units; maximum is {maximum}"
                )
            }
            Self::MigrationIdentityMismatch => {
                formatter.write_str("messenger generation advance changes identity or source IP")
            }
        }
    }
}

impl Error for MessengerHubError {}

#[derive(Debug, Clone)]
struct MessengerEndpoint {
    identity: MessengerIdentity,
    chat_type: u32,
}

#[derive(Debug, Clone)]
struct MessengerRoom {
    members: Vec<MessengerIdentity>,
}

#[derive(Debug)]
pub struct MessengerHub {
    limits: MessengerHubLimits,
    endpoints_by_session: HashMap<MessengerSessionId, MessengerEndpoint>,
    session_by_identity: HashMap<String, MessengerSessionId>,
    rooms: HashMap<MessengerRoomId, MessengerRoom>,
    rooms_by_identity: HashMap<String, HashSet<MessengerRoomId>>,
    next_room_id: Option<NonZeroU32>,
}

impl MessengerHub {
    pub fn new(limits: MessengerHubLimits) -> Result<Self, MessengerHubError> {
        for (name, value) in [
            ("max_sessions", limits.max_sessions),
            ("max_rooms", limits.max_rooms),
            ("max_rooms_per_identity", limits.max_rooms_per_identity),
            ("max_message_utf16_units", limits.max_message_utf16_units),
        ] {
            if value == 0 {
                return Err(MessengerHubError::ZeroLimit(name));
            }
        }
        Ok(Self {
            limits,
            endpoints_by_session: HashMap::new(),
            session_by_identity: HashMap::new(),
            rooms: HashMap::new(),
            rooms_by_identity: HashMap::new(),
            next_room_id: NonZeroU32::new(1),
        })
    }

    #[must_use]
    pub fn limits(&self) -> MessengerHubLimits {
        self.limits
    }

    #[must_use]
    pub fn session_count(&self) -> usize {
        self.endpoints_by_session.len()
    }

    #[must_use]
    pub fn room_count(&self) -> usize {
        self.rooms.len()
    }

    #[must_use]
    pub fn rooms_for_identity(&self, nickname: &str) -> usize {
        self.rooms_by_identity
            .get(&canonical_nickname_key(nickname))
            .map_or(0, HashSet::len)
    }

    #[must_use]
    pub fn session_for_identity(&self, nickname: &str) -> Option<MessengerSessionId> {
        self.session_by_identity
            .get(&canonical_nickname_key(nickname))
            .copied()
    }

    #[must_use]
    pub fn room_members(&self, room_id: MessengerRoomId) -> Option<&[MessengerIdentity]> {
        self.rooms.get(&room_id).map(|room| room.members.as_slice())
    }

    pub fn enter(
        &mut self,
        session: MessengerSessionId,
        peer_ip: IpAddr,
        active: &MessengerIdentity,
        claim: &EnterClaim,
    ) -> Result<EnterOutcome, MessengerHubError> {
        if self.endpoints_by_session.contains_key(&session) {
            return Err(MessengerHubError::SessionAlreadyEntered(session));
        }
        if claim.user_no != active.user_no.get() {
            return Err(MessengerHubError::SenderUserNoMismatch);
        }
        if canonical_nickname_key(&claim.nickname) != active.key {
            return Err(MessengerHubError::SenderNicknameMismatch);
        }
        if peer_ip != active.source_ip {
            return Err(MessengerHubError::SourceIpMismatch {
                expected: active.source_ip,
                received: peer_ip,
            });
        }

        let replaced_session = self.session_by_identity.get(&active.key).copied();
        if replaced_session.is_none() && self.endpoints_by_session.len() >= self.limits.max_sessions
        {
            return Err(MessengerHubError::SessionLimitReached {
                maximum: self.limits.max_sessions,
            });
        }
        if let Some(previous) = replaced_session {
            self.endpoints_by_session.remove(&previous);
        }
        self.endpoints_by_session.insert(
            session,
            MessengerEndpoint {
                identity: active.clone(),
                chat_type: claim.chat_type,
            },
        );
        self.session_by_identity.insert(active.key.clone(), session);
        self.debug_assert_invariants();
        Ok(EnterOutcome { replaced_session })
    }

    #[must_use]
    pub fn chat_type(&self, session: MessengerSessionId) -> Option<u32> {
        self.endpoints_by_session
            .get(&session)
            .map(|endpoint| endpoint.chat_type)
    }

    pub fn invite(
        &mut self,
        session: MessengerSessionId,
        active_sender: &MessengerIdentity,
        active_target: &MessengerIdentity,
        claim: &InviteClaim,
    ) -> Result<Vec<MessengerDelivery>, MessengerHubError> {
        let sender = self.authorize_session(session, active_sender)?;
        Self::validate_sender(&sender, claim.inviter_user_no, &claim.inviter_nickname)?;
        if claim.invitee_user_no != active_target.user_no.get()
            || canonical_nickname_key(&claim.invitee_nickname) != active_target.key
        {
            return Err(MessengerHubError::TargetIdentityMismatch);
        }
        if sender.key == active_target.key {
            return Err(MessengerHubError::SelfInvite);
        }
        if self.rooms.len() >= self.limits.max_rooms {
            return Err(MessengerHubError::RoomLimitReached {
                maximum: self.limits.max_rooms,
            });
        }
        self.ensure_identity_room_capacity(&sender)?;
        self.ensure_identity_room_capacity(active_target)?;

        let room_id = self.allocate_room_id()?;
        let members = vec![sender.clone(), active_target.clone()];
        self.rooms.insert(
            room_id,
            MessengerRoom {
                members: members.clone(),
            },
        );
        for member in &members {
            self.rooms_by_identity
                .entry(member.key.clone())
                .or_default()
                .insert(room_id);
        }

        let event = MessengerEvent::InviteChat {
            inviter_user_no: sender.user_no.get(),
            invitee_user_no: active_target.user_no.get(),
            inviter_nickname: sender.nickname.clone(),
            invitee_nickname: active_target.nickname.clone(),
            room_id,
            result: 0,
        };
        let deliveries = self.deliver_to_members(&members, &event);
        self.debug_assert_invariants();
        Ok(deliveries)
    }

    pub fn chat(
        &self,
        session: MessengerSessionId,
        active_sender: &MessengerIdentity,
        claim: ChatClaim,
    ) -> Result<Vec<MessengerDelivery>, MessengerHubError> {
        let sender = self.authorize_session(session, active_sender)?;
        Self::validate_sender_nickname(&sender, &claim.nickname)?;
        self.validate_message(&claim.message)?;
        let room_id = MessengerRoomId::new(claim.room_id)
            .ok_or(MessengerHubError::InvalidRoomId(claim.room_id))?;
        let room = self
            .rooms
            .get(&room_id)
            .ok_or(MessengerHubError::RoomNotFound(room_id))?;
        if !room
            .members
            .iter()
            .any(|member| identity_stamp_matches(member, &sender))
        {
            return Err(MessengerHubError::NotRoomMember {
                room_id,
                nickname: sender.nickname,
            });
        }

        let event = MessengerEvent::Chat {
            room_id,
            sender_user_no: sender.user_no.get(),
            nickname: sender.nickname,
            message: Arc::from(claim.message),
            result: 0,
        };
        Ok(self.deliver_to_members(&room.members, &event))
    }

    pub fn leave(
        &mut self,
        session: MessengerSessionId,
        active_sender: &MessengerIdentity,
        claim: &LeaveClaim,
    ) -> Result<Vec<MessengerDelivery>, MessengerHubError> {
        let sender = self.authorize_session(session, active_sender)?;
        if claim.user_no != sender.user_no.get() {
            return Err(MessengerHubError::SenderUserNoMismatch);
        }
        let room_id = MessengerRoomId::new(claim.room_id)
            .ok_or(MessengerHubError::InvalidRoomId(claim.room_id))?;
        let (remaining, empty) = {
            let room = self
                .rooms
                .get_mut(&room_id)
                .ok_or(MessengerHubError::RoomNotFound(room_id))?;
            let index = room
                .members
                .iter()
                .position(|member| identity_stamp_matches(member, &sender))
                .ok_or_else(|| MessengerHubError::NotRoomMember {
                    room_id,
                    nickname: sender.nickname.clone(),
                })?;
            room.members.remove(index);
            (room.members.clone(), room.members.is_empty())
        };
        self.remove_inverse_room(&sender.key, room_id);
        if empty {
            self.rooms.remove(&room_id);
        }

        let event = MessengerEvent::LeaveChat {
            user_no: sender.user_no.get(),
            room_id,
        };
        let deliveries = self.deliver_to_members(&remaining, &event);
        self.debug_assert_invariants();
        Ok(deliveries)
    }

    pub fn guild_chat(
        &self,
        session: MessengerSessionId,
        active_sender: &MessengerIdentity,
        claim: GuildChatClaim,
    ) -> Result<Vec<MessengerDelivery>, MessengerHubError> {
        let sender = self.authorize_session(session, active_sender)?;
        Self::validate_sender_nickname(&sender, &claim.nickname)?;
        self.validate_message(&claim.message)?;
        let event = MessengerEvent::GuildChat {
            nickname: sender.nickname,
            message: Arc::from(claim.message),
        };
        let mut sessions = self
            .endpoints_by_session
            .keys()
            .copied()
            .collect::<Vec<_>>();
        sessions.sort_unstable();
        Ok(sessions
            .into_iter()
            .map(|session| MessengerDelivery {
                session,
                event: event.clone(),
            })
            .collect())
    }

    pub fn advance_generation(
        &mut self,
        previous: &MessengerIdentity,
        next: &MessengerIdentity,
    ) -> Result<GenerationAdvance, MessengerHubError> {
        if previous.key != next.key
            || previous.user_no != next.user_no
            || previous.source_ip != next.source_ip
            || next.generation.get() <= previous.generation.get()
        {
            return Err(MessengerHubError::MigrationIdentityMismatch);
        }

        let mut endpoint_updated = false;
        if let Some(session) = self.session_by_identity.get(&previous.key).copied()
            && let Some(endpoint) = self.endpoints_by_session.get_mut(&session)
            && identity_stamp_matches(&endpoint.identity, previous)
        {
            endpoint.identity = next.clone();
            endpoint_updated = true;
        }

        let mut room_members_updated = 0;
        let room_ids = self
            .rooms_by_identity
            .get(&previous.key)
            .map(|rooms| rooms.iter().copied().collect::<Vec<_>>())
            .unwrap_or_default();
        for room_id in room_ids {
            if let Some(room) = self.rooms.get_mut(&room_id)
                && let Some(member) = room
                    .members
                    .iter_mut()
                    .find(|member| identity_stamp_matches(member, previous))
            {
                *member = next.clone();
                room_members_updated += 1;
            }
        }
        self.debug_assert_invariants();
        Ok(GenerationAdvance {
            endpoint_updated,
            room_members_updated,
        })
    }

    pub fn release_identity(&mut self, identity: &MessengerIdentity) -> IdentityRelease {
        let disconnected_session =
            self.session_by_identity
                .get(&identity.key)
                .copied()
                .filter(|session| {
                    self.endpoints_by_session
                        .get(session)
                        .is_some_and(|endpoint| {
                            identity_stamp_matches(&endpoint.identity, identity)
                        })
                });
        if let Some(session) = disconnected_session {
            self.endpoints_by_session.remove(&session);
            if self.session_by_identity.get(&identity.key) == Some(&session) {
                self.session_by_identity.remove(&identity.key);
            }
        }
        let (room_memberships_removed, empty_rooms_removed) =
            self.remove_identity_from_rooms(identity);
        self.debug_assert_invariants();
        IdentityRelease {
            disconnected_session,
            room_memberships_removed,
            empty_rooms_removed,
        }
    }

    pub fn disconnect_session(&mut self, session: MessengerSessionId) -> Option<IdentityRelease> {
        let endpoint = self.endpoints_by_session.remove(&session)?;
        if self.session_by_identity.get(&endpoint.identity.key) == Some(&session) {
            self.session_by_identity.remove(&endpoint.identity.key);
        }
        let (room_memberships_removed, empty_rooms_removed) =
            self.remove_identity_from_rooms(&endpoint.identity);
        self.debug_assert_invariants();
        Some(IdentityRelease {
            disconnected_session: Some(session),
            room_memberships_removed,
            empty_rooms_removed,
        })
    }

    fn authorize_session(
        &self,
        session: MessengerSessionId,
        active: &MessengerIdentity,
    ) -> Result<MessengerIdentity, MessengerHubError> {
        let endpoint = self
            .endpoints_by_session
            .get(&session)
            .ok_or(MessengerHubError::UnauthenticatedSession(session))?;
        if !identity_stamp_matches(&endpoint.identity, active)
            || self.session_by_identity.get(&active.key) != Some(&session)
        {
            return Err(MessengerHubError::StaleSession(session));
        }
        Ok(endpoint.identity.clone())
    }

    fn validate_sender(
        sender: &MessengerIdentity,
        claimed_user_no: u32,
        claimed_nickname: &str,
    ) -> Result<(), MessengerHubError> {
        if claimed_user_no != sender.user_no.get() {
            return Err(MessengerHubError::SenderUserNoMismatch);
        }
        Self::validate_sender_nickname(sender, claimed_nickname)
    }

    fn validate_sender_nickname(
        sender: &MessengerIdentity,
        claimed_nickname: &str,
    ) -> Result<(), MessengerHubError> {
        if canonical_nickname_key(claimed_nickname) == sender.key {
            Ok(())
        } else {
            Err(MessengerHubError::SenderNicknameMismatch)
        }
    }

    fn validate_message(&self, message: &str) -> Result<(), MessengerHubError> {
        let length = message.encode_utf16().count();
        if length > self.limits.max_message_utf16_units {
            Err(MessengerHubError::MessageTooLong {
                length,
                maximum: self.limits.max_message_utf16_units,
            })
        } else {
            Ok(())
        }
    }

    fn ensure_identity_room_capacity(
        &self,
        identity: &MessengerIdentity,
    ) -> Result<(), MessengerHubError> {
        let count = self
            .rooms_by_identity
            .get(&identity.key)
            .map_or(0, HashSet::len);
        if count >= self.limits.max_rooms_per_identity {
            Err(MessengerHubError::IdentityRoomLimitReached {
                nickname: identity.nickname.clone(),
                maximum: self.limits.max_rooms_per_identity,
            })
        } else {
            Ok(())
        }
    }

    fn allocate_room_id(&mut self) -> Result<MessengerRoomId, MessengerHubError> {
        let value = self
            .next_room_id
            .take()
            .ok_or(MessengerHubError::RoomIdExhausted)?;
        self.next_room_id = value.get().checked_add(1).and_then(NonZeroU32::new);
        Ok(MessengerRoomId(value))
    }

    fn deliver_to_members(
        &self,
        members: &[MessengerIdentity],
        event: &MessengerEvent,
    ) -> Vec<MessengerDelivery> {
        members
            .iter()
            .filter_map(|member| {
                let session = self.session_by_identity.get(&member.key).copied()?;
                let endpoint = self.endpoints_by_session.get(&session)?;
                identity_stamp_matches(&endpoint.identity, member).then_some(MessengerDelivery {
                    session,
                    event: event.clone(),
                })
            })
            .collect()
    }

    fn remove_inverse_room(&mut self, identity_key: &str, room_id: MessengerRoomId) {
        let remove_index = self
            .rooms_by_identity
            .get_mut(identity_key)
            .is_some_and(|rooms| {
                rooms.remove(&room_id);
                rooms.is_empty()
            });
        if remove_index {
            self.rooms_by_identity.remove(identity_key);
        }
    }

    fn remove_identity_from_rooms(&mut self, identity: &MessengerIdentity) -> (usize, usize) {
        let room_ids = self
            .rooms_by_identity
            .get(&identity.key)
            .map(|rooms| rooms.iter().copied().collect::<Vec<_>>())
            .unwrap_or_default();
        let mut memberships_removed = 0;
        let mut empty_rooms_removed = 0;
        for room_id in room_ids {
            let (removed, empty) = if let Some(room) = self.rooms.get_mut(&room_id) {
                let original = room.members.len();
                room.members
                    .retain(|member| !identity_stamp_matches(member, identity));
                (original - room.members.len(), room.members.is_empty())
            } else {
                (0, false)
            };
            if removed == 0 {
                continue;
            }
            memberships_removed += removed;
            self.remove_inverse_room(&identity.key, room_id);
            if empty {
                self.rooms.remove(&room_id);
                empty_rooms_removed += 1;
            }
        }
        (memberships_removed, empty_rooms_removed)
    }

    fn debug_assert_invariants(&self) {
        #[cfg(debug_assertions)]
        {
            debug_assert!(self.endpoints_by_session.len() <= self.limits.max_sessions);
            debug_assert!(self.rooms.len() <= self.limits.max_rooms);
            debug_assert_eq!(
                self.endpoints_by_session.len(),
                self.session_by_identity.len()
            );
            for (session, endpoint) in &self.endpoints_by_session {
                debug_assert_eq!(
                    self.session_by_identity.get(&endpoint.identity.key),
                    Some(session)
                );
            }
            for (key, room_ids) in &self.rooms_by_identity {
                debug_assert!(room_ids.len() <= self.limits.max_rooms_per_identity);
                for room_id in room_ids {
                    let room = self.rooms.get(room_id).expect("inverse room must exist");
                    debug_assert!(room.members.iter().any(|member| member.key == *key));
                }
            }
            for (room_id, room) in &self.rooms {
                debug_assert!(!room.members.is_empty());
                debug_assert!(room.members.len() <= 2);
                for member in &room.members {
                    debug_assert!(
                        self.rooms_by_identity
                            .get(&member.key)
                            .is_some_and(|rooms| rooms.contains(room_id))
                    );
                }
            }
        }
    }
}

impl Default for MessengerHub {
    fn default() -> Self {
        Self::new(MessengerHubLimits::default()).expect("default messenger hub limits are non-zero")
    }
}

fn identity_stamp_matches(left: &MessengerIdentity, right: &MessengerIdentity) -> bool {
    left.key == right.key
        && left.user_no == right.user_no
        && left.generation == right.generation
        && left.source_ip == right.source_ip
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::{
        ChatClaim, EnterClaim, GuildChatClaim, InviteClaim, LeaveClaim, MessengerEvent,
        MessengerGeneration, MessengerHub, MessengerHubError, MessengerHubLimits,
        MessengerIdentity, MessengerRoomId, MessengerSessionId,
    };

    const SOURCE_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10));
    const OTHER_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 11));

    fn session(value: u64) -> MessengerSessionId {
        MessengerSessionId::new(value).unwrap()
    }

    fn identity(user_no: u32, nickname: &str, generation: u64) -> MessengerIdentity {
        MessengerIdentity::new(user_no, nickname, generation, SOURCE_IP).unwrap()
    }

    fn enter(hub: &mut MessengerHub, session: MessengerSessionId, identity: &MessengerIdentity) {
        hub.enter(
            session,
            SOURCE_IP,
            identity,
            &EnterClaim {
                user_no: identity.user_no.get(),
                chat_type: 2,
                nickname: identity.nickname.clone(),
            },
        )
        .unwrap();
    }

    fn invite_claim(source: &MessengerIdentity, target: &MessengerIdentity) -> InviteClaim {
        InviteClaim {
            inviter_user_no: source.user_no.get(),
            invitee_user_no: target.user_no.get(),
            inviter_nickname: source.nickname.clone(),
            invitee_nickname: target.nickname.clone(),
        }
    }

    #[test]
    fn identifiers_and_configuration_are_checked_and_nonzero() {
        assert!(MessengerSessionId::new(0).is_none());
        assert!(MessengerGeneration::new(0).is_none());
        assert!(MessengerRoomId::new(0).is_none());
        assert_eq!(
            MessengerIdentity::new(0, "Rider", 1, SOURCE_IP),
            Err(MessengerHubError::InvalidUserNo)
        );
        assert_eq!(
            MessengerIdentity::new(1, "Rider", 0, SOURCE_IP),
            Err(MessengerHubError::InvalidGeneration)
        );
        assert_eq!(
            MessengerIdentity::new(1, " ", 1, SOURCE_IP),
            Err(MessengerHubError::EmptyNickname)
        );

        let limits = MessengerHubLimits {
            max_sessions: 0,
            ..MessengerHubLimits::default()
        };
        assert!(matches!(
            MessengerHub::new(limits),
            Err(MessengerHubError::ZeroLimit("max_sessions"))
        ));
    }

    #[test]
    fn enter_validates_identity_source_and_replacement_session_fence() {
        let mut hub = MessengerHub::new(MessengerHubLimits {
            max_sessions: 1,
            ..MessengerHubLimits::default()
        })
        .unwrap();
        let rider = identity(17, "Rider", 1);
        let first = session(1);
        let replacement = session(2);

        assert_eq!(
            hub.enter(
                first,
                SOURCE_IP,
                &rider,
                &EnterClaim {
                    user_no: 18,
                    chat_type: 2,
                    nickname: rider.nickname.clone(),
                },
            ),
            Err(MessengerHubError::SenderUserNoMismatch)
        );
        assert_eq!(
            hub.enter(
                first,
                SOURCE_IP,
                &rider,
                &EnterClaim {
                    user_no: 17,
                    chat_type: 2,
                    nickname: "Other".to_owned(),
                },
            ),
            Err(MessengerHubError::SenderNicknameMismatch)
        );
        assert_eq!(
            hub.enter(
                first,
                OTHER_IP,
                &rider,
                &EnterClaim {
                    user_no: 17,
                    chat_type: 2,
                    nickname: "rIDER".to_owned(),
                },
            ),
            Err(MessengerHubError::SourceIpMismatch {
                expected: SOURCE_IP,
                received: OTHER_IP,
            })
        );

        enter(&mut hub, first, &rider);
        assert_eq!(hub.chat_type(first), Some(2));
        assert_eq!(
            hub.enter(
                first,
                SOURCE_IP,
                &rider,
                &EnterClaim {
                    user_no: 17,
                    chat_type: 9,
                    nickname: "Rider".to_owned(),
                },
            ),
            Err(MessengerHubError::SessionAlreadyEntered(first))
        );
        let outcome = hub
            .enter(
                replacement,
                SOURCE_IP,
                &rider,
                &EnterClaim {
                    user_no: 17,
                    chat_type: 9,
                    nickname: "rider".to_owned(),
                },
            )
            .unwrap();
        assert_eq!(outcome.replaced_session, Some(first));
        assert_eq!(hub.session_count(), 1);
        assert_eq!(hub.session_for_identity("RIDER"), Some(replacement));
        assert_eq!(hub.chat_type(replacement), Some(9));
        assert!(hub.disconnect_session(first).is_none());

        let peer = identity(18, "Peer", 1);
        assert_eq!(
            hub.enter(
                session(3),
                SOURCE_IP,
                &peer,
                &EnterClaim {
                    user_no: 18,
                    chat_type: 0,
                    nickname: "Peer".to_owned(),
                },
            ),
            Err(MessengerHubError::SessionLimitReached { maximum: 1 })
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn invite_chat_and_leave_preserve_the_csharp_recipient_semantics() {
        let mut hub = MessengerHub::default();
        let rider = identity(17, "Rider", 1);
        let peer = identity(18, "Peer", 2);
        let outsider = identity(19, "Outsider", 3);
        let rider_session = session(1);
        let peer_session = session(2);
        let outsider_session = session(3);
        enter(&mut hub, rider_session, &rider);
        enter(&mut hub, peer_session, &peer);
        enter(&mut hub, outsider_session, &outsider);

        let invite = hub
            .invite(rider_session, &rider, &peer, &invite_claim(&rider, &peer))
            .unwrap();
        assert_eq!(
            invite
                .iter()
                .map(|delivery| delivery.session)
                .collect::<Vec<_>>(),
            vec![rider_session, peer_session]
        );
        let MessengerEvent::InviteChat {
            inviter_user_no,
            invitee_user_no,
            inviter_nickname,
            invitee_nickname,
            room_id,
            result,
        } = &invite[0].event
        else {
            panic!("invite must emit the C# reply shape");
        };
        assert_eq!(
            (
                *inviter_user_no,
                *invitee_user_no,
                inviter_nickname.as_str(),
                invitee_nickname.as_str(),
                room_id.get(),
                *result,
            ),
            (17, 18, "Rider", "Peer", 1, 0)
        );
        assert_eq!(hub.rooms_for_identity("rider"), 1);
        assert_eq!(hub.rooms_for_identity("PEER"), 1);

        let chat = hub
            .chat(
                rider_session,
                &rider,
                ChatClaim {
                    room_id: room_id.get(),
                    nickname: "rIDER".to_owned(),
                    message: "hello".to_owned(),
                },
            )
            .unwrap();
        assert_eq!(
            chat.iter()
                .map(|delivery| delivery.session)
                .collect::<Vec<_>>(),
            vec![rider_session, peer_session],
            "C# sends PrMsgrChat back to the sender too"
        );
        assert!(matches!(
            &chat[0].event,
            MessengerEvent::Chat {
                sender_user_no: 17,
                nickname,
                message,
                result: 0,
                ..
            } if nickname == "Rider" && message.as_ref() == "hello"
        ));
        assert!(matches!(
            hub.chat(
                outsider_session,
                &outsider,
                ChatClaim {
                    room_id: room_id.get(),
                    nickname: "Outsider".to_owned(),
                    message: "spoof".to_owned(),
                },
            ),
            Err(MessengerHubError::NotRoomMember { .. })
        ));

        let leave = hub
            .leave(
                peer_session,
                &peer,
                &LeaveClaim {
                    user_no: 18,
                    room_id: room_id.get(),
                },
            )
            .unwrap();
        assert_eq!(leave.len(), 1);
        assert_eq!(leave[0].session, rider_session);
        assert!(matches!(
            leave[0].event,
            MessengerEvent::LeaveChat {
                user_no: 18,
                room_id: left_room,
            } if left_room == *room_id
        ));

        let final_leave = hub
            .leave(
                rider_session,
                &rider,
                &LeaveClaim {
                    user_no: 17,
                    room_id: room_id.get(),
                },
            )
            .unwrap();
        assert!(final_leave.is_empty());
        assert_eq!(hub.room_count(), 0);
        assert_eq!(hub.rooms_for_identity("Rider"), 0);
        assert_eq!(hub.rooms_for_identity("Peer"), 0);
    }

    #[test]
    fn sender_target_and_message_claims_cannot_be_spoofed() {
        let mut hub = MessengerHub::new(MessengerHubLimits {
            max_message_utf16_units: 4,
            ..MessengerHubLimits::default()
        })
        .unwrap();
        let rider = identity(17, "Rider", 1);
        let peer = identity(18, "Peer", 2);
        let rider_session = session(1);
        enter(&mut hub, rider_session, &rider);

        let mut spoofed_sender = invite_claim(&rider, &peer);
        spoofed_sender.inviter_user_no = 18;
        assert_eq!(
            hub.invite(rider_session, &rider, &peer, &spoofed_sender),
            Err(MessengerHubError::SenderUserNoMismatch)
        );
        let mut spoofed_target = invite_claim(&rider, &peer);
        spoofed_target.invitee_nickname = "SomeoneElse".to_owned();
        assert_eq!(
            hub.invite(rider_session, &rider, &peer, &spoofed_target),
            Err(MessengerHubError::TargetIdentityMismatch)
        );
        assert_eq!(
            hub.invite(rider_session, &rider, &rider, &invite_claim(&rider, &rider),),
            Err(MessengerHubError::SelfInvite)
        );

        let room_id = match &hub
            .invite(rider_session, &rider, &peer, &invite_claim(&rider, &peer))
            .unwrap()[0]
            .event
        {
            MessengerEvent::InviteChat { room_id, .. } => *room_id,
            _ => unreachable!(),
        };
        assert_eq!(
            hub.chat(
                rider_session,
                &rider,
                ChatClaim {
                    room_id: room_id.get(),
                    nickname: "Peer".to_owned(),
                    message: "ok".to_owned(),
                },
            ),
            Err(MessengerHubError::SenderNicknameMismatch)
        );
        assert_eq!(
            hub.chat(
                rider_session,
                &rider,
                ChatClaim {
                    room_id: room_id.get(),
                    nickname: "Rider".to_owned(),
                    message: "12345".to_owned(),
                },
            ),
            Err(MessengerHubError::MessageTooLong {
                length: 5,
                maximum: 4,
            })
        );
        assert_eq!(
            hub.leave(
                rider_session,
                &rider,
                &LeaveClaim {
                    user_no: 18,
                    room_id: room_id.get(),
                },
            ),
            Err(MessengerHubError::SenderUserNoMismatch)
        );
    }

    #[test]
    fn room_caps_and_checked_allocator_prevent_growth_and_wraparound() {
        let mut hub = MessengerHub::new(MessengerHubLimits {
            max_rooms: 3,
            max_rooms_per_identity: 1,
            ..MessengerHubLimits::default()
        })
        .unwrap();
        let rider = identity(17, "Rider", 1);
        let peer = identity(18, "Peer", 2);
        let other = identity(19, "Other", 3);
        let rider_session = session(1);
        enter(&mut hub, rider_session, &rider);
        hub.invite(rider_session, &rider, &peer, &invite_claim(&rider, &peer))
            .unwrap();
        assert_eq!(
            hub.invite(rider_session, &rider, &other, &invite_claim(&rider, &other),),
            Err(MessengerHubError::IdentityRoomLimitReached {
                nickname: "Rider".to_owned(),
                maximum: 1,
            })
        );

        let mut exhausted = MessengerHub::default();
        enter(&mut exhausted, rider_session, &rider);
        exhausted.next_room_id = std::num::NonZeroU32::new(u32::MAX);
        let first = exhausted
            .invite(rider_session, &rider, &peer, &invite_claim(&rider, &peer))
            .unwrap();
        assert!(matches!(
            first[0].event,
            MessengerEvent::InviteChat { room_id, .. } if room_id.get() == u32::MAX
        ));
        assert_eq!(
            exhausted.invite(rider_session, &rider, &other, &invite_claim(&rider, &other),),
            Err(MessengerHubError::RoomIdExhausted)
        );
    }

    #[test]
    fn generation_advance_preserves_continuity_and_fences_stale_release() {
        let mut hub = MessengerHub::default();
        let rider_v1 = identity(17, "Rider", 10);
        let rider_v2 = identity(17, "Rider", 11);
        let peer = identity(18, "Peer", 20);
        let rider_session = session(1);
        let peer_session = session(2);
        enter(&mut hub, rider_session, &rider_v1);
        enter(&mut hub, peer_session, &peer);
        let room_id = match &hub
            .invite(
                rider_session,
                &rider_v1,
                &peer,
                &invite_claim(&rider_v1, &peer),
            )
            .unwrap()[0]
            .event
        {
            MessengerEvent::InviteChat { room_id, .. } => *room_id,
            _ => unreachable!(),
        };

        let advanced = hub.advance_generation(&rider_v1, &rider_v2).unwrap();
        assert!(advanced.endpoint_updated);
        assert_eq!(advanced.room_members_updated, 1);
        assert!(matches!(
            hub.chat(
                rider_session,
                &rider_v1,
                ChatClaim {
                    room_id: room_id.get(),
                    nickname: "Rider".to_owned(),
                    message: "stale".to_owned(),
                },
            ),
            Err(MessengerHubError::StaleSession(id)) if id == rider_session
        ));
        assert_eq!(
            hub.chat(
                rider_session,
                &rider_v2,
                ChatClaim {
                    room_id: room_id.get(),
                    nickname: "Rider".to_owned(),
                    message: "current".to_owned(),
                },
            )
            .unwrap()
            .len(),
            2
        );

        let stale_release = hub.release_identity(&rider_v1);
        assert_eq!(stale_release.disconnected_session, None);
        assert_eq!(stale_release.room_memberships_removed, 0);
        assert_eq!(hub.session_for_identity("Rider"), Some(rider_session));
        assert_eq!(hub.room_members(room_id).unwrap().len(), 2);

        let release = hub.release_identity(&rider_v2);
        assert_eq!(release.disconnected_session, Some(rider_session));
        assert_eq!(release.room_memberships_removed, 1);
        assert_eq!(release.empty_rooms_removed, 0);
        assert_eq!(hub.room_members(room_id).unwrap().len(), 1);
        assert!(hub.disconnect_session(rider_session).is_none());

        let moved_ip =
            MessengerIdentity::new(18, "Peer", 21, OTHER_IP).expect("valid identity fixture");
        assert_eq!(
            hub.advance_generation(&peer, &moved_ip),
            Err(MessengerHubError::MigrationIdentityMismatch)
        );
    }

    #[test]
    fn replacement_preserves_rooms_but_current_disconnect_cleans_inverse_indexes() {
        let mut hub = MessengerHub::default();
        let rider = identity(17, "Rider", 1);
        let peer = identity(18, "Peer", 2);
        let old_session = session(1);
        let peer_session = session(2);
        let replacement = session(3);
        enter(&mut hub, old_session, &rider);
        enter(&mut hub, peer_session, &peer);
        let room_id = match &hub
            .invite(old_session, &rider, &peer, &invite_claim(&rider, &peer))
            .unwrap()[0]
            .event
        {
            MessengerEvent::InviteChat { room_id, .. } => *room_id,
            _ => unreachable!(),
        };

        let replaced = hub
            .enter(
                replacement,
                SOURCE_IP,
                &rider,
                &EnterClaim {
                    user_no: 17,
                    chat_type: 2,
                    nickname: "Rider".to_owned(),
                },
            )
            .unwrap();
        assert_eq!(replaced.replaced_session, Some(old_session));
        assert!(hub.disconnect_session(old_session).is_none());
        assert_eq!(hub.room_members(room_id).unwrap().len(), 2);

        let rider_close = hub.disconnect_session(replacement).unwrap();
        assert_eq!(rider_close.room_memberships_removed, 1);
        assert_eq!(hub.rooms_for_identity("Rider"), 0);
        assert_eq!(hub.rooms_for_identity("Peer"), 1);
        assert_eq!(hub.room_members(room_id).unwrap().len(), 1);

        let peer_close = hub.disconnect_session(peer_session).unwrap();
        assert_eq!(peer_close.empty_rooms_removed, 1);
        assert_eq!(hub.room_count(), 0);
        assert_eq!(hub.rooms_for_identity("Peer"), 0);
    }

    #[test]
    fn guild_chat_is_the_authenticated_csharp_global_hub_broadcast() {
        let mut hub = MessengerHub::default();
        let rider = identity(17, "Rider", 1);
        let peer = identity(18, "Peer", 2);
        let third = identity(19, "Third", 3);
        for (id, active) in [
            (session(3), &third),
            (session(1), &rider),
            (session(2), &peer),
        ] {
            enter(&mut hub, id, active);
        }

        assert_eq!(
            hub.guild_chat(
                session(1),
                &rider,
                GuildChatClaim {
                    nickname: "Peer".to_owned(),
                    message: "spoof".to_owned(),
                },
            ),
            Err(MessengerHubError::SenderNicknameMismatch)
        );
        let deliveries = hub
            .guild_chat(
                session(1),
                &rider,
                GuildChatClaim {
                    nickname: "rIDER".to_owned(),
                    message: "hello all".to_owned(),
                },
            )
            .unwrap();
        assert_eq!(
            deliveries
                .iter()
                .map(|delivery| delivery.session)
                .collect::<Vec<_>>(),
            vec![session(1), session(2), session(3)]
        );
        assert!(deliveries.iter().all(|delivery| matches!(
            &delivery.event,
            MessengerEvent::GuildChat { nickname, message }
                if nickname == "Rider" && message.as_ref() == "hello all"
        )));
    }

    #[test]
    fn an_active_but_offline_target_matches_the_csharp_source_only_reply() {
        let mut hub = MessengerHub::default();
        let rider = identity(17, "Rider", 1);
        let offline = identity(18, "Offline", 2);
        let rider_session = session(1);
        enter(&mut hub, rider_session, &rider);

        let deliveries = hub
            .invite(
                rider_session,
                &rider,
                &offline,
                &invite_claim(&rider, &offline),
            )
            .unwrap();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].session, rider_session);
        assert_eq!(hub.rooms_for_identity("Offline"), 1);
    }
}
