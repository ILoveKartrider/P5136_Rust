//! Identity ownership, generation fencing, and channel migration.
//!
//! This module deliberately contains no timers or I/O. The world actor supplies
//! [`Instant`] values, periodically expires permits, and applies the returned
//! cleanup work before it processes its next command.

use std::{
    collections::HashMap,
    fmt,
    net::IpAddr,
    num::NonZeroU16,
    time::{Duration, Instant},
};

use p5136_core::nickname::{NicknameError, canonical_nickname_key, normalize_nickname};
use thiserror::Error;

use crate::SessionId;

/// P5136 accepts a channel-migration permit for fifteen seconds.
pub const MIGRATION_TTL: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UserNo(u32);

impl UserNo {
    #[must_use]
    pub const fn new(value: u32) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IdentityGeneration(u64);

impl IdentityGeneration {
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A non-zero opaque value copied from `PrChannelSwitch` into
/// `PqChannelMovein`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MigrationToken(NonZeroU16);

impl MigrationToken {
    #[must_use]
    pub const fn new(value: u16) -> Option<Self> {
        match NonZeroU16::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelBinding {
    pub channel_id: u16,
    pub game_type: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityBinding {
    pub nickname: String,
    pub user_no: UserNo,
    pub generation: IdentityGeneration,
    pub owner: SessionId,
    pub source_ip: IpAddr,
    pub channel: Option<ChannelBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationPermit {
    pub user_no: UserNo,
    pub source_generation: IdentityGeneration,
    pub source_session: SessionId,
    pub source_ip: IpAddr,
    pub channel: ChannelBinding,
    pub token: MigrationToken,
    pub expires_at: Instant,
}

/// Read-only proof that a channel migration request was fully validated.
///
/// Fields are private so only [`IdentityRegistry::preflight_migration`] can
/// mint a ticket. Consuming it revalidates the destination, permit, source
/// generation/source state and expiry; a ticket is never an authorization
/// snapshot.
#[derive(PartialEq, Eq)]
pub(crate) struct MigrationPreflight {
    destination_session: SessionId,
    destination_ip: IpAddr,
    user_no: UserNo,
    channel_id: u16,
    channel: ChannelBinding,
    token: MigrationToken,
    source_generation: IdentityGeneration,
    source_state: MigrationSourceState,
    source_session: SessionId,
    expires_at: Instant,
    nickname: String,
    canonical_nickname: String,
}

/// A migration source may only advance from connected to disconnected while a
/// preflight ticket waits on profile I/O. Reconnection or owner replacement
/// requires a new ticket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MigrationSourceState {
    Connected(SessionId),
    Disconnected,
}

impl MigrationSourceState {
    const fn from_owner(owner: Option<SessionId>) -> Self {
        match owner {
            Some(owner) => Self::Connected(owner),
            None => Self::Disconnected,
        }
    }

    fn permits_current(self, current_owner: Option<SessionId>, permit_source: SessionId) -> bool {
        match (self, current_owner) {
            (Self::Connected(expected), Some(current)) => {
                expected == permit_source && current == expected
            }
            (Self::Connected(expected), None) => expected == permit_source,
            (Self::Disconnected, None) => true,
            (Self::Disconnected, Some(_)) => false,
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
impl MigrationPreflight {
    #[must_use]
    pub(crate) fn nickname(&self) -> &str {
        &self.nickname
    }

    #[must_use]
    pub(crate) fn canonical_nickname(&self) -> &str {
        &self.canonical_nickname
    }

    #[must_use]
    pub(crate) const fn user_no(&self) -> UserNo {
        self.user_no
    }

    #[must_use]
    pub(crate) const fn source_generation(&self) -> IdentityGeneration {
        self.source_generation
    }

    #[must_use]
    pub(crate) const fn destination_session(&self) -> SessionId {
        self.destination_session
    }

    #[must_use]
    pub(crate) const fn destination_ip(&self) -> IpAddr {
        self.destination_ip
    }
}

impl fmt::Debug for MigrationPreflight {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MigrationPreflight")
            .field("destination_session", &self.destination_session)
            .field("destination_ip", &self.destination_ip)
            .field("user_no", &self.user_no)
            .field("channel_id", &self.channel_id)
            .field("channel", &self.channel)
            .field("source_generation", &self.source_generation)
            .field("source_state", &self.source_state)
            .field("source_session", &self.source_session)
            .field("expires_at", &self.expires_at)
            .field("nickname", &self.nickname)
            .field("canonical_nickname", &self.canonical_nickname)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationCompletion {
    pub binding: IdentityBinding,
    /// Exact actor-minted binding for the generation that was transferred.
    ///
    /// This remains available even when the source socket disconnected before
    /// completion, so generation-bound sidecars never have to reconstruct the
    /// former owner from [`previous_owner`](Self::previous_owner).
    pub previous_binding: IdentityBinding,
    /// Exact pre-transfer stamp used to advance generation-bound sidecars.
    pub previous_identity: ReleasedIdentity,
    /// `Some` when the old owner was still connected at transfer time. The
    /// caller should close it; its old generation is already stale either way.
    pub previous_owner: Option<SessionId>,
}

/// State that the world actor must remove from rooms and endpoint tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleasedIdentity {
    pub nickname: String,
    pub user_no: UserNo,
    pub generation: IdentityGeneration,
    pub source_ip: IpAddr,
    pub channel: Option<ChannelBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisconnectOutcome {
    /// The session never established an identity, or was already forgotten.
    Unauthenticated,
    /// The session carried an older generation and cannot affect the current
    /// owner.
    Stale(IdentityBinding),
    /// The source disconnected while its latest permit was still usable.
    /// Shared room and endpoint state must remain alive until completion or
    /// expiration.
    Deferred {
        identity: IdentityBinding,
        permit: MigrationPermit,
    },
    /// No valid migration can take ownership. Cleanup can run immediately.
    Released(ReleasedIdentity),
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum IdentityError {
    #[error("nickname must not be empty")]
    EmptyNickname,

    #[error(transparent)]
    InvalidNickname(#[from] NicknameError),

    #[error("session {session:?} already owns identity {nickname:?}")]
    SessionAlreadyAuthenticated {
        session: SessionId,
        nickname: String,
    },

    #[error("identity {nickname:?} is already active")]
    DuplicateIdentity { nickname: String },

    #[error("session {0:?} has not established an identity")]
    UnauthenticatedSession(SessionId),

    #[error("session {0:?} is not the current identity owner")]
    StaleSession(SessionId),

    #[error("user number {0} is unknown")]
    UnknownUserNo(u32),

    #[error("identity {nickname:?} has no channel-migration permit")]
    NoMigrationPermit { nickname: String },

    #[error("the channel-migration permit expired")]
    MigrationExpired,

    #[error("migration channel mismatch: expected {expected}, received {received}")]
    ChannelMismatch { expected: u16, received: u16 },

    #[error("migration token mismatch")]
    TokenMismatch,

    #[error("migration source IP mismatch: expected {expected}, received {received}")]
    SourceIpMismatch { expected: IpAddr, received: IpAddr },

    #[error("the migration permit belongs to a stale identity generation")]
    StaleMigrationGeneration,

    #[error("the migration preflight no longer matches the current permit or source state")]
    StaleMigrationPreflight,

    #[error("identity user-number space is exhausted")]
    UserNoExhausted,

    #[error("identity generation space is exhausted")]
    GenerationExhausted,

    #[error("the system clock cannot represent the migration deadline")]
    MigrationDeadlineOverflow,
}

#[derive(Debug, Clone)]
struct KnownIdentity {
    nickname: String,
    user_no: UserNo,
}

#[derive(Debug, Clone)]
struct ActiveIdentity {
    known: KnownIdentity,
    generation: IdentityGeneration,
    owner: Option<SessionId>,
    owner_ip: IpAddr,
    channel: Option<ChannelBinding>,
    permit: Option<MigrationPermit>,
}

struct ValidatedMigration<'a> {
    canonical_nickname: &'a str,
    active: &'a ActiveIdentity,
    permit: &'a MigrationPermit,
}

/// Deterministic identity state owned by the server's world actor.
///
/// A caller must never invoke methods on this value from multiple tasks behind
/// a mutex. Put the registry inside one actor and serialize commands through
/// that actor's mailbox.
#[derive(Debug)]
pub struct IdentityRegistry {
    known_by_name: HashMap<String, KnownIdentity>,
    name_by_user_no: HashMap<UserNo, String>,
    active_by_name: HashMap<String, ActiveIdentity>,
    session_bindings: HashMap<SessionId, IdentityBinding>,
    next_user_no: u32,
    next_generation: u64,
}

impl Default for IdentityRegistry {
    fn default() -> Self {
        Self {
            known_by_name: HashMap::new(),
            name_by_user_no: HashMap::new(),
            active_by_name: HashMap::new(),
            session_bindings: HashMap::new(),
            next_user_no: 1,
            next_generation: 1,
        }
    }
}

impl IdentityRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Claims a nickname for a newly authenticated login session.
    ///
    /// The same Windows-safe validation is applied on every host before the
    /// identity can become a profile directory or room member.
    pub fn claim(
        &mut self,
        session: SessionId,
        source_ip: IpAddr,
        requested_nickname: &str,
    ) -> Result<IdentityBinding, IdentityError> {
        if requested_nickname.trim().is_empty() {
            return Err(IdentityError::EmptyNickname);
        }
        let nickname = normalize_nickname(requested_nickname)?;

        if let Some(binding) = self.session_bindings.get(&session) {
            return Err(IdentityError::SessionAlreadyAuthenticated {
                session,
                nickname: binding.nickname.clone(),
            });
        }

        let key = canonical_nickname_key(&nickname);
        if let Some(active) = self.active_by_name.get(&key) {
            return Err(IdentityError::DuplicateIdentity {
                nickname: active.known.nickname.clone(),
            });
        }

        let known = if let Some(known) = self.known_by_name.get(&key) {
            known.clone()
        } else {
            let user_no = self.allocate_user_no()?;
            let known = KnownIdentity {
                nickname: nickname.clone(),
                user_no,
            };
            self.known_by_name.insert(key.clone(), known.clone());
            self.name_by_user_no.insert(user_no, key.clone());
            known
        };
        let generation = self.allocate_generation()?;
        let binding = IdentityBinding {
            nickname: known.nickname.clone(),
            user_no: known.user_no,
            generation,
            owner: session,
            source_ip,
            channel: None,
        };
        self.active_by_name.insert(
            key,
            ActiveIdentity {
                known,
                generation,
                owner: Some(session),
                owner_ip: source_ip,
                channel: None,
                permit: None,
            },
        );
        self.session_bindings.insert(session, binding.clone());
        Ok(binding)
    }

    /// Verifies that a packet came from the current owner and generation.
    pub fn authorize(&self, session: SessionId) -> Result<IdentityBinding, IdentityError> {
        let binding = self
            .session_bindings
            .get(&session)
            .ok_or(IdentityError::UnauthenticatedSession(session))?;
        let key = canonical_nickname_key(&binding.nickname);
        let active = self
            .active_by_name
            .get(&key)
            .ok_or(IdentityError::StaleSession(session))?;

        if active.owner != Some(session) || active.generation != binding.generation {
            return Err(IdentityError::StaleSession(session));
        }
        Ok(binding.clone())
    }

    /// Replaces any existing permit for this generation. Entropy generation is
    /// deliberately outside this pure state machine; the runtime must supply a
    /// cryptographically random non-zero token.
    pub fn begin_migration(
        &mut self,
        source_session: SessionId,
        channel: ChannelBinding,
        token: MigrationToken,
        now: Instant,
    ) -> Result<MigrationPermit, IdentityError> {
        let source = self.authorize(source_session)?;
        let expires_at = now
            .checked_add(MIGRATION_TTL)
            .ok_or(IdentityError::MigrationDeadlineOverflow)?;
        let permit = MigrationPermit {
            user_no: source.user_no,
            source_generation: source.generation,
            source_session,
            source_ip: source.source_ip,
            channel,
            token,
            expires_at,
        };
        let key = canonical_nickname_key(&source.nickname);
        let active = self
            .active_by_name
            .get_mut(&key)
            .ok_or(IdentityError::StaleSession(source_session))?;
        active.permit = Some(permit.clone());
        Ok(permit)
    }

    /// Validates a migration without changing identity ownership or consuming
    /// its permit.
    pub(crate) fn preflight_migration(
        &self,
        destination_session: SessionId,
        destination_ip: IpAddr,
        user_no: UserNo,
        channel_id: u16,
        token: MigrationToken,
        now: Instant,
    ) -> Result<MigrationPreflight, IdentityError> {
        let validated = self.validate_migration(
            destination_session,
            destination_ip,
            user_no,
            channel_id,
            token,
            now,
        )?;
        Ok(MigrationPreflight {
            destination_session,
            destination_ip,
            user_no,
            channel_id,
            channel: validated.permit.channel,
            token,
            source_generation: validated.permit.source_generation,
            source_state: MigrationSourceState::from_owner(validated.active.owner),
            source_session: validated.permit.source_session,
            expires_at: validated.permit.expires_at,
            nickname: validated.active.known.nickname.clone(),
            canonical_nickname: validated.canonical_nickname.to_owned(),
        })
    }

    fn validate_migration(
        &self,
        destination_session: SessionId,
        destination_ip: IpAddr,
        user_no: UserNo,
        channel_id: u16,
        token: MigrationToken,
        now: Instant,
    ) -> Result<ValidatedMigration<'_>, IdentityError> {
        if let Some(binding) = self.session_bindings.get(&destination_session) {
            return Err(IdentityError::SessionAlreadyAuthenticated {
                session: destination_session,
                nickname: binding.nickname.clone(),
            });
        }

        let key = self
            .name_by_user_no
            .get(&user_no)
            .ok_or(IdentityError::UnknownUserNo(user_no.get()))?;
        let active =
            self.active_by_name
                .get(key)
                .ok_or_else(|| IdentityError::NoMigrationPermit {
                    nickname: self.known_by_name[key].nickname.clone(),
                })?;
        let permit = active
            .permit
            .as_ref()
            .ok_or_else(|| IdentityError::NoMigrationPermit {
                nickname: active.known.nickname.clone(),
            })?;

        if now >= permit.expires_at {
            return Err(IdentityError::MigrationExpired);
        }
        if permit.user_no != user_no {
            return Err(IdentityError::UnknownUserNo(user_no.get()));
        }
        if permit.channel.channel_id != channel_id {
            return Err(IdentityError::ChannelMismatch {
                expected: permit.channel.channel_id,
                received: channel_id,
            });
        }
        if permit.token != token {
            return Err(IdentityError::TokenMismatch);
        }
        if permit.source_ip != destination_ip {
            return Err(IdentityError::SourceIpMismatch {
                expected: permit.source_ip,
                received: destination_ip,
            });
        }
        if permit.source_generation != active.generation
            || active
                .owner
                .is_some_and(|owner| owner != permit.source_session)
        {
            return Err(IdentityError::StaleMigrationGeneration);
        }
        Ok(ValidatedMigration {
            canonical_nickname: key,
            active,
            permit,
        })
    }

    /// Transfers ownership after immediately minting and consuming a validated
    /// preflight ticket.
    pub fn complete_migration(
        &mut self,
        destination_session: SessionId,
        destination_ip: IpAddr,
        user_no: UserNo,
        channel_id: u16,
        token: MigrationToken,
        now: Instant,
    ) -> Result<MigrationCompletion, IdentityError> {
        let preflight = self.preflight_migration(
            destination_session,
            destination_ip,
            user_no,
            channel_id,
            token,
            now,
        )?;
        self.complete_preflighted_migration(preflight, now)
    }

    /// Revalidates and consumes a previously minted migration ticket.
    pub(crate) fn complete_preflighted_migration(
        &mut self,
        preflight: MigrationPreflight,
        now: Instant,
    ) -> Result<MigrationCompletion, IdentityError> {
        let MigrationPreflight {
            destination_session,
            destination_ip,
            user_no,
            channel_id,
            channel: expected_channel,
            token,
            source_generation,
            source_state,
            source_session,
            expires_at,
            nickname,
            canonical_nickname: expected_canonical_nickname,
        } = preflight;
        let (
            canonical_nickname,
            previous_owner,
            previous_binding,
            previous_identity,
            known,
            channel,
        ) = {
            let validated = self.validate_migration(
                destination_session,
                destination_ip,
                user_no,
                channel_id,
                token,
                now,
            )?;
            if validated.canonical_nickname != expected_canonical_nickname
                || validated.active.known.nickname != nickname
                || !source_state
                    .permits_current(validated.active.owner, validated.permit.source_session)
                || validated.permit.channel != expected_channel
                || validated.permit.source_generation != source_generation
                || validated.permit.source_session != source_session
                || validated.permit.expires_at != expires_at
            {
                return Err(IdentityError::StaleMigrationPreflight);
            }
            (
                validated.canonical_nickname.to_owned(),
                validated.active.owner,
                identity_binding(validated.active, validated.permit.source_session),
                released_identity(validated.active),
                validated.active.known.clone(),
                validated.permit.channel,
            )
        };
        let generation = self.allocate_generation()?;
        let binding = IdentityBinding {
            nickname: known.nickname.clone(),
            user_no: known.user_no,
            generation,
            owner: destination_session,
            source_ip: destination_ip,
            channel: Some(channel),
        };

        let active = self
            .active_by_name
            .get_mut(&canonical_nickname)
            .ok_or(IdentityError::StaleMigrationPreflight)?;
        active.generation = generation;
        active.owner = Some(destination_session);
        active.owner_ip = destination_ip;
        active.channel = Some(channel);
        active.permit = None;
        self.session_bindings
            .insert(destination_session, binding.clone());

        Ok(MigrationCompletion {
            binding,
            previous_binding,
            previous_identity,
            previous_owner,
        })
    }

    /// Removes a socket binding. A valid migration keeps the identity active
    /// without an owner; otherwise the identity is returned for immediate world
    /// cleanup.
    pub fn disconnect(&mut self, session: SessionId, now: Instant) -> DisconnectOutcome {
        let Some(binding) = self.session_bindings.remove(&session) else {
            return DisconnectOutcome::Unauthenticated;
        };
        let key = canonical_nickname_key(&binding.nickname);
        let Some(active) = self.active_by_name.get_mut(&key) else {
            return DisconnectOutcome::Stale(binding);
        };
        if active.owner != Some(session) || active.generation != binding.generation {
            return DisconnectOutcome::Stale(binding);
        }

        if let Some(permit) = active
            .permit
            .as_ref()
            .filter(|permit| permit.expires_at > now)
            .cloned()
        {
            active.owner = None;
            return DisconnectOutcome::Deferred {
                identity: binding,
                permit,
            };
        }

        let Some(active) = self.active_by_name.remove(&key) else {
            return DisconnectOutcome::Stale(binding);
        };
        DisconnectOutcome::Released(released_identity(&active))
    }

    /// Expires all due permits. Identities whose source already disconnected
    /// are removed and returned for room/endpoint cleanup.
    pub fn expire_migrations(&mut self, now: Instant) -> Vec<ReleasedIdentity> {
        let mut release_keys = Vec::new();
        for (key, active) in &mut self.active_by_name {
            if active
                .permit
                .as_ref()
                .is_some_and(|permit| now >= permit.expires_at)
            {
                active.permit = None;
                if active.owner.is_none() {
                    release_keys.push(key.clone());
                }
            }
        }

        release_keys
            .into_iter()
            .filter_map(|key| self.active_by_name.remove(&key))
            .map(|active| released_identity(&active))
            .collect()
    }

    #[must_use]
    pub fn known_user_no(&self, nickname: &str) -> Option<UserNo> {
        self.known_by_name
            .get(&canonical_nickname_key(nickname))
            .map(|known| known.user_no)
    }

    #[must_use]
    pub fn active_identity(&self, nickname: &str) -> Option<IdentityBinding> {
        let active = self.active_by_name.get(&canonical_nickname_key(nickname))?;
        active_identity_binding(active)
    }

    /// Resolves a numeric UDP account header only when it still has a current
    /// owner. A disconnected migration source remains known, but is not active
    /// until its destination completes the generation transfer.
    #[must_use]
    pub fn active_identity_by_user_no(&self, user_no: UserNo) -> Option<IdentityBinding> {
        let key = self.name_by_user_no.get(&user_no)?;
        let active = self.active_by_name.get(key)?;
        active_identity_binding(active)
    }

    /// Confirms that an exact binding is the temporarily ownerless source
    /// generation retained by a registry-owned migration permit.
    ///
    /// `MyRoom` keeps this generation in its bounded audience until migration
    /// completion or the expiry sweep. This includes a deadline-reached permit
    /// which the actor has not swept yet. Callers may skip delivery to this
    /// exact state, but must not mistake an arbitrary inactive or stale binding
    /// for the registry-retained generation.
    pub(crate) fn is_current_ownerless_binding(&self, binding: &IdentityBinding) -> bool {
        let Some(key) = self.name_by_user_no.get(&binding.user_no) else {
            return false;
        };
        let Some(active) = self.active_by_name.get(key) else {
            return false;
        };
        let Some(permit) = active.permit.as_ref() else {
            return false;
        };
        active.owner.is_none()
            && identity_binding(active, permit.source_session) == *binding
            && permit.source_generation == binding.generation
    }

    /// Iterates over exact bindings for identities that currently have an
    /// owning session. Ownerless migration generations are deliberately
    /// omitted until a destination completes the transfer.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the pending random MyRoom entry command consumes active identities"
        )
    )]
    pub(crate) fn active_identities(&self) -> impl Iterator<Item = IdentityBinding> + '_ {
        self.active_by_name
            .values()
            .filter_map(active_identity_binding)
    }

    #[must_use]
    pub fn active_count(&self) -> usize {
        self.active_by_name.len()
    }

    /// Releases every connected or ownerless active generation during the
    /// actor-owned server shutdown barrier.
    ///
    /// Stable nickname/user-number assignments remain available for the
    /// lifetime of the registry, while all session authorization and
    /// migration permits are revoked atomically before dependent world state
    /// is retired.
    pub(crate) fn drain_active(&mut self) -> Vec<ReleasedIdentity> {
        self.session_bindings.clear();
        let mut released = self
            .active_by_name
            .drain()
            .map(|(_, active)| released_identity(&active))
            .collect::<Vec<_>>();
        released.sort_unstable_by_key(|identity| identity.user_no.get());
        released
    }

    fn allocate_user_no(&mut self) -> Result<UserNo, IdentityError> {
        let value = self.next_user_no;
        self.next_user_no = value.checked_add(1).ok_or(IdentityError::UserNoExhausted)?;
        if value == 0 {
            return Err(IdentityError::UserNoExhausted);
        }
        Ok(UserNo(value))
    }

    fn allocate_generation(&mut self) -> Result<IdentityGeneration, IdentityError> {
        let value = self.next_generation;
        self.next_generation = value
            .checked_add(1)
            .ok_or(IdentityError::GenerationExhausted)?;
        if value == 0 {
            return Err(IdentityError::GenerationExhausted);
        }
        Ok(IdentityGeneration(value))
    }
}

fn released_identity(active: &ActiveIdentity) -> ReleasedIdentity {
    ReleasedIdentity {
        nickname: active.known.nickname.clone(),
        user_no: active.known.user_no,
        generation: active.generation,
        source_ip: active.owner_ip,
        channel: active.channel,
    }
}

fn active_identity_binding(active: &ActiveIdentity) -> Option<IdentityBinding> {
    let owner = active.owner?;
    Some(identity_binding(active, owner))
}

fn identity_binding(active: &ActiveIdentity, owner: SessionId) -> IdentityBinding {
    IdentityBinding {
        nickname: active.known.nickname.clone(),
        user_no: active.known.user_no,
        generation: active.generation,
        owner,
        source_ip: active.owner_ip,
        channel: active.channel,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr},
        time::{Duration, Instant},
    };

    use super::{
        ChannelBinding, DisconnectOutcome, IdentityError, IdentityRegistry, MIGRATION_TTL,
        MigrationToken, UserNo,
    };
    use crate::SessionId;
    use p5136_core::nickname::{NicknameError, canonical_nickname_key};

    const SOURCE_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10));
    const OTHER_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 11));
    const CHANNEL: ChannelBinding = ChannelBinding {
        channel_id: 11,
        game_type: 67,
    };

    fn session(value: u64) -> SessionId {
        SessionId::new(value)
    }

    fn token(value: u16) -> MigrationToken {
        MigrationToken::new(value).unwrap()
    }

    #[test]
    fn duplicate_claim_is_case_insensitive() {
        let mut identities = IdentityRegistry::new();
        let first = identities.claim(session(1), SOURCE_IP, "RiderOne").unwrap();

        assert_eq!(
            identities.claim(session(2), SOURCE_IP, "rIDERoNE"),
            Err(IdentityError::DuplicateIdentity {
                nickname: "RiderOne".to_owned()
            })
        );
        assert_eq!(identities.authorize(session(1)).unwrap(), first);
    }

    #[test]
    fn unsafe_profile_path_nickname_is_rejected_at_the_server_boundary() {
        let mut identities = IdentityRegistry::new();
        assert_eq!(
            identities.claim(session(1), SOURCE_IP, "../escape"),
            Err(IdentityError::InvalidNickname(
                NicknameError::InvalidCharacter {
                    codepoint: u32::from('/'),
                },
            ))
        );
    }

    #[test]
    fn user_number_is_stable_but_generation_advances_after_reconnect() {
        let now = Instant::now();
        let mut identities = IdentityRegistry::new();
        let first = identities.claim(session(1), SOURCE_IP, "Rider").unwrap();
        assert!(matches!(
            identities.disconnect(session(1), now),
            DisconnectOutcome::Released(_)
        ));

        let replacement = identities.claim(session(2), SOURCE_IP, "rider").unwrap();
        assert_eq!(replacement.nickname, "Rider");
        assert_eq!(replacement.user_no, first.user_no);
        assert!(replacement.generation.get() > first.generation.get());
    }

    #[test]
    fn user_number_lookup_only_returns_the_current_owned_generation() {
        let now = Instant::now();
        let mut identities = IdentityRegistry::new();
        assert_eq!(UserNo::new(0), None);
        assert!(
            identities
                .active_identity_by_user_no(UserNo::new(u32::MAX).unwrap())
                .is_none()
        );

        let source = identities.claim(session(1), SOURCE_IP, "Rider").unwrap();
        assert_eq!(
            identities.active_identity_by_user_no(source.user_no),
            Some(source.clone())
        );
        let permit = identities
            .begin_migration(session(1), CHANNEL, token(350), now)
            .unwrap();
        assert!(matches!(
            identities.disconnect(session(1), now),
            DisconnectOutcome::Deferred { .. }
        ));
        assert!(
            identities
                .active_identity_by_user_no(source.user_no)
                .is_none(),
            "an ownerless migration generation must not authorize UDP"
        );

        let destination = identities
            .complete_migration(
                session(2),
                SOURCE_IP,
                source.user_no,
                CHANNEL.channel_id,
                permit.token,
                now,
            )
            .unwrap()
            .binding;
        assert!(destination.generation.get() > source.generation.get());
        assert_eq!(
            identities.active_identity_by_user_no(source.user_no),
            Some(destination.clone())
        );

        assert!(matches!(
            identities.disconnect(session(2), now),
            DisconnectOutcome::Released(_)
        ));
        assert!(
            identities
                .active_identity_by_user_no(source.user_no)
                .is_none()
        );
    }

    #[test]
    fn ownerless_binding_check_accepts_only_registry_retained_exact_generation() {
        let now = Instant::now();
        let mut identities = IdentityRegistry::new();
        let source = identities.claim(session(1), SOURCE_IP, "Retained").unwrap();
        identities
            .begin_migration(session(1), CHANNEL, token(352), now)
            .unwrap();
        assert!(matches!(
            identities.disconnect(session(1), now),
            DisconnectOutcome::Deferred { .. }
        ));
        assert!(identities.is_current_ownerless_binding(&source));

        let mut forged = source.clone();
        forged.owner = session(99);
        assert!(!identities.is_current_ownerless_binding(&forged));

        let expired = identities.expire_migrations(now + MIGRATION_TTL);
        assert_eq!(expired.len(), 1);
        assert!(!identities.is_current_ownerless_binding(&source));
    }

    #[test]
    fn active_identity_iteration_omits_ownerless_migration_generations() {
        let now = Instant::now();
        let mut identities = IdentityRegistry::new();
        let source = identities.claim(session(1), SOURCE_IP, "Rider").unwrap();
        let other = identities.claim(session(9), OTHER_IP, "Other").unwrap();
        let permit = identities
            .begin_migration(session(1), CHANNEL, token(351), now)
            .unwrap();

        let mut connected = identities.active_identities().collect::<Vec<_>>();
        connected.sort_by_key(|binding| binding.owner.get());
        assert_eq!(connected, vec![source.clone(), other.clone()]);

        assert!(matches!(
            identities.disconnect(session(1), now),
            DisconnectOutcome::Deferred { .. }
        ));
        assert_eq!(
            identities.active_identities().collect::<Vec<_>>(),
            vec![other.clone()]
        );

        let destination = identities
            .complete_migration(
                session(2),
                SOURCE_IP,
                source.user_no,
                CHANNEL.channel_id,
                permit.token,
                now,
            )
            .unwrap()
            .binding;
        let connected = identities.active_identities().collect::<Vec<_>>();
        assert_eq!(connected.len(), 2);
        assert!(connected.contains(&other));
        assert!(connected.contains(&destination));
    }

    #[test]
    fn shutdown_drain_releases_connected_and_ownerless_generations_but_keeps_stable_identity() {
        let now = Instant::now();
        let mut identities = IdentityRegistry::new();
        let ownerless = identities
            .claim(session(1), SOURCE_IP, "Ownerless")
            .unwrap();
        let connected = identities.claim(session(2), OTHER_IP, "Connected").unwrap();
        identities
            .begin_migration(session(1), CHANNEL, token(353), now)
            .unwrap();
        assert!(matches!(
            identities.disconnect(session(1), now),
            DisconnectOutcome::Deferred { .. }
        ));

        let released = identities.drain_active();
        assert_eq!(
            released
                .iter()
                .map(|identity| identity.user_no)
                .collect::<Vec<_>>(),
            vec![ownerless.user_no, connected.user_no]
        );
        assert_eq!(identities.active_count(), 0);
        assert!(matches!(
            identities.authorize(session(2)),
            Err(IdentityError::UnauthenticatedSession(id)) if id == session(2)
        ));
        assert!(identities.drain_active().is_empty());

        let replacement = identities
            .claim(session(3), SOURCE_IP, "ownerless")
            .unwrap();
        assert_eq!(replacement.user_no, ownerless.user_no);
        assert!(replacement.generation.get() > ownerless.generation.get());
    }

    #[test]
    fn latest_migration_permit_wins() {
        let now = Instant::now();
        let mut identities = IdentityRegistry::new();
        let source = identities.claim(session(1), SOURCE_IP, "Rider").unwrap();
        let old = identities
            .begin_migration(session(1), CHANNEL, token(100), now)
            .unwrap();
        let replacement_channel = ChannelBinding {
            channel_id: 12,
            game_type: 67,
        };
        let latest = identities
            .begin_migration(
                session(1),
                replacement_channel,
                token(200),
                now + Duration::from_secs(1),
            )
            .unwrap();

        assert_eq!(old.user_no, source.user_no);
        assert_eq!(
            identities.complete_migration(
                session(2),
                SOURCE_IP,
                source.user_no,
                old.channel.channel_id,
                old.token,
                now + Duration::from_secs(2),
            ),
            Err(IdentityError::ChannelMismatch {
                expected: latest.channel.channel_id,
                received: old.channel.channel_id,
            })
        );
        assert_eq!(
            identities.complete_migration(
                session(2),
                SOURCE_IP,
                source.user_no,
                latest.channel.channel_id,
                old.token,
                now + Duration::from_secs(2),
            ),
            Err(IdentityError::TokenMismatch)
        );
        let complete = identities
            .complete_migration(
                session(2),
                SOURCE_IP,
                source.user_no,
                latest.channel.channel_id,
                latest.token,
                now + Duration::from_secs(2),
            )
            .unwrap();
        assert_eq!(complete.binding.channel, Some(replacement_channel));
    }

    #[test]
    fn migration_preflight_is_read_only_and_success_is_revalidated_on_consume() {
        let now = Instant::now();
        let mut identities = IdentityRegistry::new();
        let source = identities.claim(session(1), SOURCE_IP, "Rider").unwrap();
        let permit = identities
            .begin_migration(session(1), CHANNEL, token(375), now)
            .unwrap();
        let before = identities.authorize(session(1)).unwrap();

        let preflight = identities
            .preflight_migration(
                session(2),
                SOURCE_IP,
                source.user_no,
                CHANNEL.channel_id,
                permit.token,
                now,
            )
            .unwrap();
        assert_eq!(preflight.nickname(), "Rider");
        assert_eq!(preflight.canonical_nickname(), "rider");
        assert_eq!(preflight.user_no(), source.user_no);
        assert_eq!(preflight.source_generation(), source.generation);
        assert_eq!(preflight.destination_session(), session(2));
        assert_eq!(preflight.destination_ip(), SOURCE_IP);
        assert_eq!(identities.authorize(session(1)).unwrap(), before);

        let completed = identities
            .complete_preflighted_migration(preflight, now)
            .unwrap();
        assert_eq!(completed.binding.owner, session(2));
        assert_eq!(completed.binding.channel, Some(CHANNEL));
        assert!(completed.binding.generation.get() > source.generation.get());
    }

    #[test]
    fn migration_preflight_allows_source_disconnect_before_consume() {
        let now = Instant::now();
        let mut identities = IdentityRegistry::new();
        let source = identities.claim(session(1), SOURCE_IP, "Rider").unwrap();
        let permit = identities
            .begin_migration(session(1), CHANNEL, token(376), now)
            .unwrap();
        let owner_bound = identities
            .preflight_migration(
                session(2),
                SOURCE_IP,
                source.user_no,
                CHANNEL.channel_id,
                permit.token,
                now,
            )
            .unwrap();
        assert!(matches!(
            identities.disconnect(session(1), now),
            DisconnectOutcome::Deferred { .. }
        ));
        let completed = identities
            .complete_preflighted_migration(owner_bound, now)
            .unwrap();
        assert_eq!(completed.previous_owner, None);
        assert_eq!(completed.previous_binding, source);
        assert_eq!(completed.binding.owner, session(2));
        assert!(completed.binding.generation.get() > source.generation.get());
    }

    #[test]
    fn migration_preflight_rejects_exact_expiry() {
        let now = Instant::now();
        let mut identities = IdentityRegistry::new();
        let source = identities.claim(session(1), SOURCE_IP, "Rider").unwrap();
        let permit = identities
            .begin_migration(session(1), CHANNEL, token(376), now)
            .unwrap();
        let preflight = identities
            .preflight_migration(
                session(2),
                SOURCE_IP,
                source.user_no,
                CHANNEL.channel_id,
                permit.token,
                now,
            )
            .unwrap();
        assert_eq!(
            identities.complete_preflighted_migration(preflight, permit.expires_at),
            Err(IdentityError::MigrationExpired)
        );
    }

    #[test]
    fn ownerless_migration_preflight_rejects_source_reconnection() {
        let now = Instant::now();
        let mut identities = IdentityRegistry::new();
        let source = identities.claim(session(1), SOURCE_IP, "Rider").unwrap();
        let permit = identities
            .begin_migration(session(1), CHANNEL, token(378), now)
            .unwrap();
        assert!(matches!(
            identities.disconnect(session(1), now),
            DisconnectOutcome::Deferred { .. }
        ));
        let preflight = identities
            .preflight_migration(
                session(2),
                SOURCE_IP,
                source.user_no,
                CHANNEL.channel_id,
                permit.token,
                now,
            )
            .unwrap();

        identities
            .active_by_name
            .get_mut(&canonical_nickname_key("Rider"))
            .unwrap()
            .owner = Some(session(1));

        assert_eq!(
            identities.complete_preflighted_migration(preflight, now),
            Err(IdentityError::StaleMigrationPreflight)
        );
    }

    #[test]
    fn migration_preflight_rejects_same_token_channel_and_expiry_with_changed_game_type() {
        let now = Instant::now();
        let mut identities = IdentityRegistry::new();
        let source = identities.claim(session(1), SOURCE_IP, "Rider").unwrap();
        let permit = identities
            .begin_migration(session(1), CHANNEL, token(377), now)
            .unwrap();
        let preflight = identities
            .preflight_migration(
                session(2),
                SOURCE_IP,
                source.user_no,
                CHANNEL.channel_id,
                permit.token,
                now,
            )
            .unwrap();
        let replacement_channel = ChannelBinding {
            channel_id: CHANNEL.channel_id,
            game_type: CHANNEL.game_type.wrapping_add(1),
        };
        let replacement = identities
            .begin_migration(session(1), replacement_channel, permit.token, now)
            .unwrap();
        assert_eq!(replacement.token, permit.token);
        assert_eq!(replacement.channel.channel_id, permit.channel.channel_id);
        assert_eq!(replacement.expires_at, permit.expires_at);

        assert_eq!(
            identities.complete_preflighted_migration(preflight, now),
            Err(IdentityError::StaleMigrationPreflight)
        );
    }

    #[test]
    fn migration_validates_user_channel_token_and_source_ip() {
        let now = Instant::now();
        let mut identities = IdentityRegistry::new();
        let source = identities.claim(session(1), SOURCE_IP, "Rider").unwrap();
        let other = identities.claim(session(9), SOURCE_IP, "Other").unwrap();
        let permit = identities
            .begin_migration(session(1), CHANNEL, token(400), now)
            .unwrap();

        assert_eq!(
            identities.complete_migration(
                session(2),
                SOURCE_IP,
                UserNo(other.user_no.get()),
                CHANNEL.channel_id,
                permit.token,
                now,
            ),
            Err(IdentityError::NoMigrationPermit {
                nickname: other.nickname
            })
        );
        assert_eq!(
            identities.complete_migration(
                session(2),
                SOURCE_IP,
                source.user_no,
                CHANNEL.channel_id + 1,
                permit.token,
                now,
            ),
            Err(IdentityError::ChannelMismatch {
                expected: CHANNEL.channel_id,
                received: CHANNEL.channel_id + 1,
            })
        );
        assert_eq!(
            identities.complete_migration(
                session(2),
                SOURCE_IP,
                source.user_no,
                CHANNEL.channel_id,
                token(401),
                now,
            ),
            Err(IdentityError::TokenMismatch)
        );
        assert_eq!(
            identities.complete_migration(
                session(2),
                OTHER_IP,
                source.user_no,
                CHANNEL.channel_id,
                permit.token,
                now,
            ),
            Err(IdentityError::SourceIpMismatch {
                expected: SOURCE_IP,
                received: OTHER_IP,
            })
        );
    }

    #[test]
    fn source_disconnect_is_deferred_until_valid_destination_takes_ownership() {
        let now = Instant::now();
        let mut identities = IdentityRegistry::new();
        let source = identities.claim(session(1), SOURCE_IP, "Rider").unwrap();
        let permit = identities
            .begin_migration(session(1), CHANNEL, token(500), now)
            .unwrap();

        let DisconnectOutcome::Deferred {
            identity,
            permit: held,
        } = identities.disconnect(session(1), now + Duration::from_secs(1))
        else {
            panic!("source identity was not held for migration");
        };
        assert_eq!(identity, source);
        assert_eq!(held, permit);
        assert_eq!(identities.active_count(), 1);
        assert!(identities.active_identity("Rider").is_none());
        assert_eq!(
            identities.claim(session(3), SOURCE_IP, "rIDER"),
            Err(IdentityError::DuplicateIdentity {
                nickname: "Rider".to_owned(),
            })
        );

        let complete = identities
            .complete_migration(
                session(2),
                SOURCE_IP,
                source.user_no,
                CHANNEL.channel_id,
                permit.token,
                now + Duration::from_secs(2),
            )
            .unwrap();
        assert_eq!(complete.previous_owner, None);
        assert_eq!(complete.binding.owner, session(2));
        assert!(complete.binding.generation.get() > source.generation.get());
        assert_eq!(identities.authorize(session(2)).unwrap(), complete.binding);
    }

    #[test]
    fn permit_expires_at_exact_deadline_and_releases_disconnected_source() {
        let now = Instant::now();
        let mut identities = IdentityRegistry::new();
        let source = identities.claim(session(1), SOURCE_IP, "Rider").unwrap();
        let permit = identities
            .begin_migration(session(1), CHANNEL, token(600), now)
            .unwrap();
        assert!(matches!(
            identities.disconnect(session(1), now),
            DisconnectOutcome::Deferred { .. }
        ));

        assert!(
            identities
                .expire_migrations(
                    permit
                        .expires_at
                        .checked_sub(Duration::from_nanos(1))
                        .unwrap()
                )
                .is_empty()
        );
        assert_eq!(
            identities.complete_migration(
                session(2),
                SOURCE_IP,
                source.user_no,
                CHANNEL.channel_id,
                permit.token,
                permit.expires_at,
            ),
            Err(IdentityError::MigrationExpired)
        );

        let released = identities.expire_migrations(permit.expires_at);
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].nickname, "Rider");
        assert_eq!(identities.active_count(), 0);
    }

    #[test]
    fn expired_permit_does_not_release_a_connected_owner() {
        let now = Instant::now();
        let mut identities = IdentityRegistry::new();
        identities.claim(session(1), SOURCE_IP, "Rider").unwrap();
        identities
            .begin_migration(session(1), CHANNEL, token(700), now)
            .unwrap();

        assert!(identities.expire_migrations(now + MIGRATION_TTL).is_empty());
        assert!(identities.authorize(session(1)).is_ok());
    }

    #[test]
    fn successful_transfer_fences_old_session_and_stale_disconnect_is_harmless() {
        let now = Instant::now();
        let mut identities = IdentityRegistry::new();
        let source = identities.claim(session(1), SOURCE_IP, "Rider").unwrap();
        let permit = identities
            .begin_migration(session(1), CHANNEL, token(800), now)
            .unwrap();
        let complete = identities
            .complete_migration(
                session(2),
                SOURCE_IP,
                source.user_no,
                CHANNEL.channel_id,
                permit.token,
                now,
            )
            .unwrap();

        assert_eq!(complete.previous_owner, Some(session(1)));
        assert_eq!(complete.previous_binding, source);
        assert_eq!(complete.previous_identity.nickname, source.nickname);
        assert_eq!(complete.previous_identity.user_no, source.user_no);
        assert_eq!(complete.previous_identity.generation, source.generation);
        assert_eq!(complete.previous_identity.source_ip, source.source_ip);
        assert_eq!(
            identities.authorize(session(1)),
            Err(IdentityError::StaleSession(session(1)))
        );
        assert_eq!(
            identities.disconnect(session(1), now),
            DisconnectOutcome::Stale(source)
        );
        assert_eq!(identities.authorize(session(2)).unwrap(), complete.binding);
        assert_eq!(identities.active_count(), 1);
    }
}
