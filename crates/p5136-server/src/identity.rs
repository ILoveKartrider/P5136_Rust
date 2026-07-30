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
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use p5136_core::nickname::{NicknameError, canonical_nickname_key, normalize_nickname};
use thiserror::Error;
use tokio::sync::Notify;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MigrationTransferId(u64);

/// One admitted operation against an exact identity generation.
///
/// This capability is deliberately neither [`Clone`] nor [`Copy`]. Callers
/// that split accepted work into another independently completing branch must
/// explicitly call [`try_retain`](Self::try_retain), making every increment of
/// the generation's drain count visible in code review.
#[must_use = "dropping the lease retires one admitted identity operation"]
pub struct IdentityOperationLease {
    instance: Arc<IdentityRegistryInstance>,
    gate: Arc<IdentityOperationGate>,
    binding: IdentityBinding,
}

impl IdentityOperationLease {
    /// Retains a child lease for work derived from this already-admitted
    /// operation.
    ///
    /// A migration freeze closes *new* admission but deliberately does not
    /// invalidate work that was already accepted. Consequently a live lease
    /// may retain a child while its generation is frozen. Retirement can only
    /// happen after the count reaches zero, so it rejects stale capabilities.
    pub fn try_retain(&self) -> Result<Self, IdentityError> {
        self.gate.try_retain()?;
        Ok(Self {
            instance: Arc::clone(&self.instance),
            gate: Arc::clone(&self.gate),
            binding: self.binding.clone(),
        })
    }

    #[must_use]
    pub const fn binding(&self) -> &IdentityBinding {
        &self.binding
    }

    pub(crate) fn belongs_to(&self, instance: &Arc<IdentityRegistryInstance>) -> bool {
        Arc::ptr_eq(&self.instance, instance)
    }
}

impl fmt::Debug for IdentityOperationLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IdentityOperationLease")
            .field("binding", &self.binding)
            .finish_non_exhaustive()
    }
}

impl Drop for IdentityOperationLease {
    fn drop(&mut self) {
        self.gate.release();
    }
}

/// Shared count for one actor-minted identity generation.
///
/// The registry is the only new-admission authority. Atomics are used only
/// because admitted leases are dropped by tasks outside the actor, while the
/// actor observes the count and migration tasks await its drain notification.
#[derive(Debug)]
struct IdentityOperationGate {
    activated_udp_epoch: u64,
    active_operations: AtomicUsize,
    state: AtomicU64,
    drained: Notify,
}

/// Opaque allocation identity for one registry/world actor instance.
///
/// Pointer equality, not a forgeable numeric ID, ties operation capabilities to
/// the actor that minted them.
#[derive(Debug)]
pub(crate) struct IdentityRegistryInstance;

impl IdentityOperationGate {
    const OPEN: u64 = 0;
    const RETIRED: u64 = u64::MAX;

    fn shared(activated_udp_epoch: u64) -> Arc<Self> {
        Arc::new(Self {
            activated_udp_epoch,
            active_operations: AtomicUsize::new(0),
            state: AtomicU64::new(Self::OPEN),
            drained: Notify::new(),
        })
    }

    fn try_admit(
        self: &Arc<Self>,
        instance: Arc<IdentityRegistryInstance>,
        binding: IdentityBinding,
    ) -> Result<IdentityOperationLease, IdentityError> {
        match self.state.load(Ordering::Acquire) {
            Self::OPEN => {}
            Self::RETIRED => return Err(IdentityError::OperationGenerationRetired),
            _ => return Err(IdentityError::OperationAdmissionClosed),
        }
        self.increment()?;

        // Registry admission and migration freezing are actor-serialized. The
        // second check also keeps this primitive correct if that implementation
        // detail changes: admission never straddles a freeze or retirement.
        match self.state.load(Ordering::Acquire) {
            Self::OPEN => {}
            Self::RETIRED => {
                self.release();
                return Err(IdentityError::OperationGenerationRetired);
            }
            _ => {
                self.release();
                return Err(IdentityError::OperationAdmissionClosed);
            }
        }
        Ok(IdentityOperationLease {
            instance,
            gate: Arc::clone(self),
            binding,
        })
    }

    fn try_retain(&self) -> Result<(), IdentityError> {
        if self.state.load(Ordering::Acquire) == Self::RETIRED {
            return Err(IdentityError::OperationGenerationRetired);
        }
        self.increment()?;
        if self.state.load(Ordering::Acquire) == Self::RETIRED {
            self.release();
            return Err(IdentityError::OperationGenerationRetired);
        }
        Ok(())
    }

    fn increment(&self) -> Result<(), IdentityError> {
        let mut current = self.active_operations.load(Ordering::Acquire);
        loop {
            let next = current
                .checked_add(1)
                .ok_or(IdentityError::OperationCountExhausted)?;
            match self.active_operations.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(observed) => current = observed,
            }
        }
    }

    fn release(&self) {
        let mut current = self.active_operations.load(Ordering::Acquire);
        while current != 0 {
            match self.active_operations.compare_exchange_weak(
                current,
                current - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    if current == 1 {
                        self.drained.notify_waiters();
                    }
                    return;
                }
                Err(observed) => current = observed,
            }
        }
    }

    fn freeze(&self, transfer_id: MigrationTransferId) -> bool {
        self.state
            .compare_exchange(
                Self::OPEN,
                transfer_id.0,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn reopen_exact(&self, transfer_id: MigrationTransferId) -> bool {
        let reopened = self
            .state
            .compare_exchange(
                transfer_id.0,
                Self::OPEN,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok();
        if reopened {
            // A drain waiter must not remain parked after expiry/cancellation
            // reopens this generation to new operations.
            self.drained.notify_waiters();
        }
        reopened
    }

    fn is_frozen_by(&self, transfer_id: MigrationTransferId) -> bool {
        self.state.load(Ordering::Acquire) == transfer_id.0
    }

    fn retire_exact(&self, transfer_id: MigrationTransferId) -> bool {
        if !self.is_drained() {
            return false;
        }
        self.state
            .compare_exchange(
                transfer_id.0,
                Self::RETIRED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn retire_drained(&self) -> bool {
        if !self.is_drained() {
            return false;
        }
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            if state == Self::RETIRED {
                return self.is_drained();
            }
            match self.state.compare_exchange_weak(
                state,
                Self::RETIRED,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return self.is_drained(),
                Err(observed) => state = observed,
            }
        }
    }

    fn is_drained(&self) -> bool {
        self.active_operations.load(Ordering::Acquire) == 0
    }

    fn drained_while_frozen(
        &self,
        transfer_id: MigrationTransferId,
    ) -> Result<bool, IdentityError> {
        if !self.is_frozen_by(transfer_id) {
            return Err(IdentityError::StaleMigrationPreflight);
        }
        Ok(self.is_drained())
    }

    async fn wait_until_drained(
        &self,
        transfer_id: MigrationTransferId,
    ) -> Result<(), IdentityError> {
        loop {
            if self.drained_while_frozen(transfer_id)? {
                return Ok(());
            }
            let notified = self.drained.notified();
            if self.drained_while_frozen(transfer_id)? {
                return Ok(());
            }
            notified.await;
        }
    }
}

#[derive(Debug)]
struct MigrationDrain {
    gate: Arc<IdentityOperationGate>,
}

/// Proof that a channel migration request was validated and its source frozen.
///
/// Fields are private so only [`IdentityRegistry::preflight_migration`] can
/// mint a ticket. Consuming it revalidates the destination, permit, source
/// generation/source state, exact transfer ID and expiry; a ticket is never an
/// authorization snapshot.
pub(crate) struct MigrationPreflight {
    transfer_id: MigrationTransferId,
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
    drain: MigrationDrain,
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

    /// Waits until every operation admitted before this preflight's exact
    /// generation freeze has retired.
    ///
    /// The waiter registers its notification before the second zero check, so
    /// the last lease cannot be lost between observation and suspension. The
    /// world actor must hand this capability to another task rather than await
    /// its own drain while it is responsible for completing accepted work.
    pub(crate) async fn wait_for_operations_drained(&self) -> Result<(), IdentityError> {
        self.drain.gate.wait_until_drained(self.transfer_id).await
    }

    #[must_use]
    pub(crate) fn operations_drained(&self) -> bool {
        self.drain.gate.is_drained()
    }
}

impl fmt::Debug for MigrationPreflight {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MigrationPreflight")
            .field("transfer_id", &self.transfer_id)
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

/// Fully materialized, but not yet published, replacement generation.
///
/// The opaque generation may be burned if World-side preparation later fails;
/// identity generations are fences rather than a gap-free public sequence.
#[must_use = "a prepared migration must be locked for commit or explicitly aborted"]
pub(crate) struct MigrationCandidate {
    canonical_nickname: String,
    operation_gate: Arc<IdentityOperationGate>,
    next_operation_gate: Arc<IdentityOperationGate>,
    completion: MigrationCompletion,
}

impl MigrationCandidate {
    #[must_use]
    pub(crate) const fn completion(&self) -> &MigrationCompletion {
        &self.completion
    }
}

/// Exclusive, exact-transfer commit capability.
///
/// Final validation happens before this guard borrows both the active identity
/// and destination-session index. Dropping it before commit reopens the exact
/// source gate. Once an acknowledgement is queued, [`commit`](Self::commit) has
/// no remaining rejection path.
pub(crate) struct IdentityMigrationCommit<'a> {
    active: &'a mut ActiveIdentity,
    session_bindings: &'a mut HashMap<SessionId, IdentityBinding>,
    transfer_id: MigrationTransferId,
    destination_session: SessionId,
    channel: ChannelBinding,
    operation_gate: Arc<IdentityOperationGate>,
    next_operation_gate: Option<Arc<IdentityOperationGate>>,
    completion: Option<MigrationCompletion>,
    committed: bool,
}

impl IdentityMigrationCommit<'_> {
    /// Publishes the prepared owner and installs its logical UDP boundary.
    #[must_use]
    pub(crate) fn commit(mut self, activated_udp_epoch: u64) -> MigrationCompletion {
        let mut next_operation_gate = self
            .next_operation_gate
            .take()
            .expect("identity migration commit owns its replacement operation gate");
        Arc::get_mut(&mut next_operation_gate)
            .expect("replacement operation gate is private until identity publication")
            .activated_udp_epoch = activated_udp_epoch;
        let completion = self
            .completion
            .take()
            .expect("identity migration commit owns its completion");

        let retired = self.operation_gate.retire_exact(self.transfer_id);
        assert!(
            retired,
            "locked identity migration source must remain frozen and drained"
        );
        self.active.generation = completion.binding.generation;
        self.active.owner = Some(self.destination_session);
        self.active.owner_ip = completion.binding.source_ip;
        self.active.channel = Some(self.channel);
        self.active.permit = None;
        self.active.transfer_in_progress = None;
        self.active.operation_gate = next_operation_gate;
        let previous = self
            .session_bindings
            .insert(self.destination_session, completion.binding.clone());
        debug_assert!(
            previous.is_none(),
            "validated migration destination must remain unauthenticated while locked"
        );
        self.committed = true;
        completion
    }
}

impl Drop for IdentityMigrationCommit<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        if self.active.transfer_in_progress == Some(self.transfer_id)
            && Arc::ptr_eq(&self.active.operation_gate, &self.operation_gate)
        {
            self.active.transfer_in_progress = None;
            let _ = self.operation_gate.reopen_exact(self.transfer_id);
        }
    }
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
    /// Socket ownership ended, but accepted work still holds this exact
    /// generation. Cleanup is returned later by
    /// [`IdentityRegistry::collect_drained_releases`].
    Draining(ReleasedIdentity),
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

    #[error("identity {nickname:?} is frozen for channel transfer")]
    TransferInProgress { nickname: String },

    #[error("identity operation admission is closed")]
    OperationAdmissionClosed,

    #[error("the identity generation has retired")]
    OperationGenerationRetired,

    #[error("identity operation counter space is exhausted")]
    OperationCountExhausted,

    #[error("the migration source generation still has admitted operations")]
    MigrationOperationsActive,

    #[error("UDP ingress predates the current identity generation")]
    UdpIngressPredatesGeneration,

    #[error("identity user number {0} has no connected owner")]
    IdentityOwnerUnavailable(u32),

    #[error("identity user-number space is exhausted")]
    UserNoExhausted,

    #[error("identity generation space is exhausted")]
    GenerationExhausted,

    #[error("identity session index could not reserve migration capacity")]
    SessionIndexCapacityUnavailable,

    #[error("identity migration-transfer ID space is exhausted")]
    MigrationTransferIdExhausted,

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
    transfer_in_progress: Option<MigrationTransferId>,
    operation_gate: Arc<IdentityOperationGate>,
}

#[derive(Debug)]
struct DeferredRelease {
    canonical_nickname: String,
    identity: ReleasedIdentity,
    operation_gate: Arc<IdentityOperationGate>,
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
    instance: Arc<IdentityRegistryInstance>,
    known_by_name: HashMap<String, KnownIdentity>,
    name_by_user_no: HashMap<UserNo, String>,
    active_by_name: HashMap<String, ActiveIdentity>,
    session_bindings: HashMap<SessionId, IdentityBinding>,
    deferred_releases: Vec<DeferredRelease>,
    next_user_no: u32,
    next_generation: u64,
    next_transfer_id: u64,
}

impl Default for IdentityRegistry {
    fn default() -> Self {
        Self {
            instance: Arc::new(IdentityRegistryInstance),
            known_by_name: HashMap::new(),
            name_by_user_no: HashMap::new(),
            active_by_name: HashMap::new(),
            session_bindings: HashMap::new(),
            deferred_releases: Vec::new(),
            next_user_no: 1,
            next_generation: 1,
            next_transfer_id: 1,
        }
    }
}

impl IdentityRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub(crate) fn instance(&self) -> Arc<IdentityRegistryInstance> {
        Arc::clone(&self.instance)
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
        self.claim_at(session, source_ip, requested_nickname, 0)
    }

    /// Claims an identity with an actor-supplied UDP activation epoch.
    ///
    /// The epoch is sampled from the same atomic clock as UDP reader admission,
    /// preventing packets queued before this generation existed from being
    /// reinterpreted as traffic from the new owner.
    pub(crate) fn claim_at(
        &mut self,
        session: SessionId,
        source_ip: IpAddr,
        requested_nickname: &str,
        activated_udp_epoch: u64,
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
        if let Some(deferred) = self
            .deferred_releases
            .iter()
            .find(|deferred| deferred.canonical_nickname == key)
        {
            return Err(IdentityError::DuplicateIdentity {
                nickname: deferred.identity.nickname.clone(),
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
        let operation_gate = IdentityOperationGate::shared(activated_udp_epoch);
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
                transfer_in_progress: None,
                operation_gate,
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
        if active.transfer_in_progress.is_some() {
            return Err(IdentityError::TransferInProgress {
                nickname: active.known.nickname.clone(),
            });
        }
        Ok(binding.clone())
    }

    /// Admits one TCP/actor operation against the current owner and exact
    /// generation.
    ///
    /// The returned lease must travel with accepted work until its last
    /// publication or reply is terminal. Authorization-only lookups are not a
    /// substitute for this method on source operation paths.
    pub(crate) fn admit_operation(
        &self,
        session: SessionId,
    ) -> Result<IdentityOperationLease, IdentityError> {
        let binding = self.authorize(session)?;
        let key = canonical_nickname_key(&binding.nickname);
        let active = self
            .active_by_name
            .get(&key)
            .ok_or(IdentityError::StaleSession(session))?;
        active
            .operation_gate
            .try_admit(Arc::clone(&self.instance), binding)
    }

    /// Admits a UDP source operation using its reactor receive timestamp.
    ///
    /// Numeric identity lookup for recipients intentionally remains
    /// [`active_identity_by_user_no`](Self::active_identity_by_user_no).
    /// Source ingress must use this stricter API so ownerless, frozen, or
    /// pre-generation datagrams never authorize a new operation.
    pub(crate) fn admit_udp_operation(
        &self,
        user_no: UserNo,
        arrival_epoch: u64,
    ) -> Result<IdentityOperationLease, IdentityError> {
        let key = self
            .name_by_user_no
            .get(&user_no)
            .ok_or(IdentityError::UnknownUserNo(user_no.get()))?;
        let active = self
            .active_by_name
            .get(key)
            .ok_or(IdentityError::IdentityOwnerUnavailable(user_no.get()))?;
        let binding = active_identity_binding(active)
            .ok_or(IdentityError::IdentityOwnerUnavailable(user_no.get()))?;
        if active.transfer_in_progress.is_some() {
            return Err(IdentityError::TransferInProgress {
                nickname: active.known.nickname.clone(),
            });
        }
        if arrival_epoch <= active.operation_gate.activated_udp_epoch {
            return Err(IdentityError::UdpIngressPredatesGeneration);
        }
        active
            .operation_gate
            .try_admit(Arc::clone(&self.instance), binding)
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

    /// Validates a migration and freezes the source generation against new
    /// packet operations without changing ownership or consuming its permit.
    pub(crate) fn preflight_migration(
        &mut self,
        destination_session: SessionId,
        destination_ip: IpAddr,
        user_no: UserNo,
        channel_id: u16,
        token: MigrationToken,
        now: Instant,
    ) -> Result<MigrationPreflight, IdentityError> {
        let (
            channel,
            source_generation,
            source_state,
            source_session,
            expires_at,
            nickname,
            canonical_nickname,
            operation_gate,
        ) = {
            let validated = self.validate_migration(
                destination_session,
                destination_ip,
                user_no,
                channel_id,
                token,
                now,
            )?;
            if validated.active.transfer_in_progress.is_some() {
                return Err(IdentityError::TransferInProgress {
                    nickname: validated.active.known.nickname.clone(),
                });
            }
            (
                validated.permit.channel,
                validated.permit.source_generation,
                MigrationSourceState::from_owner(validated.active.owner),
                validated.permit.source_session,
                validated.permit.expires_at,
                validated.active.known.nickname.clone(),
                validated.canonical_nickname.to_owned(),
                Arc::clone(&validated.active.operation_gate),
            )
        };
        let transfer_id = self.allocate_transfer_id()?;
        let preflight = MigrationPreflight {
            transfer_id,
            destination_session,
            destination_ip,
            user_no,
            channel_id,
            channel,
            token,
            source_generation,
            source_state,
            source_session,
            expires_at,
            nickname,
            canonical_nickname,
            drain: MigrationDrain {
                gate: Arc::clone(&operation_gate),
            },
        };
        let active = self
            .active_by_name
            .get_mut(&preflight.canonical_nickname)
            .ok_or(IdentityError::StaleMigrationPreflight)?;
        if !Arc::ptr_eq(&active.operation_gate, &operation_gate)
            || active.generation != source_generation
            || !operation_gate.freeze(transfer_id)
        {
            return Err(IdentityError::StaleMigrationPreflight);
        }
        active.transfer_in_progress = Some(transfer_id);
        Ok(preflight)
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

    /// Revalidates every fallible precondition for publishing a migration,
    /// including generation-number capacity, without changing ownership.
    ///
    /// The actor uses this immediately before it queues `PrChannelMoveIn`, then
    /// calls [`complete_preflighted_migration`](Self::complete_preflighted_migration)
    /// in the same non-awaiting actor turn. Because the source gate is frozen
    /// and drained, no lease can change these conditions between ACK admission
    /// and owner publication.
    pub(crate) fn validate_preflighted_migration(
        &self,
        preflight: &MigrationPreflight,
        now: Instant,
    ) -> Result<(), IdentityError> {
        let validated = self.validate_migration(
            preflight.destination_session,
            preflight.destination_ip,
            preflight.user_no,
            preflight.channel_id,
            preflight.token,
            now,
        )?;
        if validated.canonical_nickname != preflight.canonical_nickname
            || validated.active.known.nickname != preflight.nickname
            || !preflight
                .source_state
                .permits_current(validated.active.owner, validated.permit.source_session)
            || validated.permit.channel != preflight.channel
            || validated.permit.source_generation != preflight.source_generation
            || validated.permit.source_session != preflight.source_session
            || validated.permit.expires_at != preflight.expires_at
            || validated.active.transfer_in_progress != Some(preflight.transfer_id)
            || !Arc::ptr_eq(&validated.active.operation_gate, &preflight.drain.gate)
            || !preflight.drain.gate.is_frozen_by(preflight.transfer_id)
        {
            return Err(IdentityError::StaleMigrationPreflight);
        }
        if !preflight.operations_drained() {
            return Err(IdentityError::MigrationOperationsActive);
        }
        if self.next_generation == 0 || self.next_generation.checked_add(1).is_none() {
            return Err(IdentityError::GenerationExhausted);
        }
        Ok(())
    }

    /// Materializes every replacement-generation value and reserves the
    /// destination session-index capacity without changing ownership.
    ///
    /// Any failure aborts this exact preflight. A later World-side preparation
    /// failure must call [`abort_preflighted_migration`](Self::abort_preflighted_migration)
    /// while it still owns `preflight`.
    pub(crate) fn prepare_preflighted_migration(
        &mut self,
        preflight: &MigrationPreflight,
        now: Instant,
    ) -> Result<MigrationCandidate, IdentityError> {
        let result = (|| {
            self.validate_preflighted_migration(preflight, now)?;
            let (
                canonical_nickname,
                previous_owner,
                previous_binding,
                previous_identity,
                known,
                channel,
                operation_gate,
            ) = {
                let validated = self.validate_migration(
                    preflight.destination_session,
                    preflight.destination_ip,
                    preflight.user_no,
                    preflight.channel_id,
                    preflight.token,
                    now,
                )?;
                (
                    validated.canonical_nickname.to_owned(),
                    validated.active.owner,
                    identity_binding(validated.active, validated.permit.source_session),
                    released_identity(validated.active),
                    validated.active.known.clone(),
                    validated.permit.channel,
                    Arc::clone(&validated.active.operation_gate),
                )
            };
            let generation = self.allocate_generation()?;
            self.session_bindings
                .try_reserve(1)
                .map_err(|_| IdentityError::SessionIndexCapacityUnavailable)?;
            let binding = IdentityBinding {
                nickname: known.nickname,
                user_no: known.user_no,
                generation,
                owner: preflight.destination_session,
                source_ip: preflight.destination_ip,
                channel: Some(channel),
            };
            Ok(MigrationCandidate {
                canonical_nickname,
                operation_gate,
                next_operation_gate: IdentityOperationGate::shared(0),
                completion: MigrationCompletion {
                    binding,
                    previous_binding,
                    previous_identity,
                    previous_owner,
                },
            })
        })();
        if result.is_err() {
            self.abort_preflighted_migration(preflight);
        }
        result
    }

    /// Revalidates and exclusively locks an exact prepared migration.
    ///
    /// The returned guard borrows the only registry fields that can invalidate
    /// the commit. If any later pre-ACK subsystem lock fails, dropping the guard
    /// reopens this exact transfer.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "the non-Clone preflight is consumed to mint one exclusive commit capability"
    )]
    pub(crate) fn lock_prepared_migration(
        &mut self,
        preflight: MigrationPreflight,
        candidate: MigrationCandidate,
        now: Instant,
    ) -> Result<IdentityMigrationCommit<'_>, IdentityError> {
        if let Err(error) = self.validate_preflighted_migration(&preflight, now) {
            self.abort_preflighted_migration(&preflight);
            return Err(error);
        }
        if candidate.canonical_nickname != preflight.canonical_nickname
            || candidate.completion.binding.owner != preflight.destination_session
            || candidate.completion.binding.user_no != preflight.user_no
            || candidate.completion.binding.channel != Some(preflight.channel)
            || candidate.completion.previous_binding.generation != preflight.source_generation
            || !Arc::ptr_eq(&candidate.operation_gate, &preflight.drain.gate)
        {
            self.abort_preflighted_migration(&preflight);
            return Err(IdentityError::StaleMigrationPreflight);
        }

        let IdentityRegistry {
            active_by_name,
            session_bindings,
            ..
        } = self;
        let active = active_by_name
            .get_mut(&candidate.canonical_nickname)
            .expect("validated migration identity remains active while actor-owned");
        Ok(IdentityMigrationCommit {
            active,
            session_bindings,
            transfer_id: preflight.transfer_id,
            destination_session: preflight.destination_session,
            channel: preflight.channel,
            operation_gate: candidate.operation_gate,
            next_operation_gate: Some(candidate.next_operation_gate),
            completion: Some(candidate.completion),
            committed: false,
        })
    }

    /// Revalidates and consumes a previously minted migration ticket.
    pub(crate) fn complete_preflighted_migration(
        &mut self,
        preflight: MigrationPreflight,
        now: Instant,
    ) -> Result<MigrationCompletion, IdentityError> {
        self.complete_preflighted_migration_at_udp_epoch(preflight, now, 0)
    }

    /// Commits a validated migration and installs the exact logical UDP
    /// generation boundary published by the World actor.
    pub(crate) fn complete_preflighted_migration_at_udp_epoch(
        &mut self,
        preflight: MigrationPreflight,
        now: Instant,
        activated_udp_epoch: u64,
    ) -> Result<MigrationCompletion, IdentityError> {
        let candidate = self.prepare_preflighted_migration(&preflight, now)?;
        let commit = self.lock_prepared_migration(preflight, candidate, now)?;
        Ok(commit.commit(activated_udp_epoch))
    }

    /// Clears the exact transfer freeze owned by a preflight whose caller was
    /// cancelled or whose profile load failed. A stale abort cannot unfreeze a
    /// newer generation or a different permit.
    pub(crate) fn abort_preflighted_migration(&mut self, preflight: &MigrationPreflight) -> bool {
        let Some(active) = self.active_by_name.get_mut(&preflight.canonical_nickname) else {
            return false;
        };
        let exact_permit = active.permit.as_ref().is_some_and(|permit| {
            permit.user_no == preflight.user_no
                && permit.source_generation == preflight.source_generation
                && permit.source_session == preflight.source_session
                && permit.channel == preflight.channel
                && permit.token == preflight.token
                && permit.expires_at == preflight.expires_at
        });
        if active.generation != preflight.source_generation
            || active.known.nickname != preflight.nickname
            || active.transfer_in_progress != Some(preflight.transfer_id)
            || !Arc::ptr_eq(&active.operation_gate, &preflight.drain.gate)
            || !exact_permit
        {
            return false;
        }
        if !active.operation_gate.reopen_exact(preflight.transfer_id) {
            return false;
        }
        active.transfer_in_progress = None;
        true
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
        if let Some(transfer_id) = active.transfer_in_progress {
            active.operation_gate.reopen_exact(transfer_id);
        }
        let identity = released_identity(&active);
        if active.operation_gate.retire_drained() {
            DisconnectOutcome::Released(identity)
        } else {
            self.deferred_releases.push(DeferredRelease {
                canonical_nickname: key,
                identity: identity.clone(),
                operation_gate: active.operation_gate,
            });
            DisconnectOutcome::Draining(identity)
        }
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
                if let Some(transfer_id) = active.transfer_in_progress
                    && active.operation_gate.reopen_exact(transfer_id)
                {
                    active.transfer_in_progress = None;
                }
                if active.owner.is_none() {
                    release_keys.push(key.clone());
                }
            }
        }

        let mut released = Vec::new();
        for key in release_keys {
            let Some(active) = self.active_by_name.remove(&key) else {
                continue;
            };
            let identity = released_identity(&active);
            if active.operation_gate.retire_drained() {
                released.push(identity);
            } else {
                self.deferred_releases.push(DeferredRelease {
                    canonical_nickname: key,
                    identity,
                    operation_gate: active.operation_gate,
                });
            }
        }
        released
    }

    /// Collects exact-generation cleanup whose last admitted operation has
    /// retired since disconnect or permit expiry.
    ///
    /// Callers should run this at actor command boundaries. Returned stamps are
    /// generation-bound, so cleanup remains safe if the stable nickname/user
    /// number has already authenticated into a newer generation.
    pub(crate) fn collect_drained_releases(&mut self) -> Vec<ReleasedIdentity> {
        let pending = std::mem::take(&mut self.deferred_releases);
        let mut released = Vec::new();
        for deferred in pending {
            if deferred.operation_gate.retire_drained() {
                released.push(deferred.identity);
            } else {
                self.deferred_releases.push(deferred);
            }
        }
        released.sort_unstable_by_key(|identity| identity.user_no.get());
        released
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
    pub(crate) fn active_identities(&self) -> impl Iterator<Item = IdentityBinding> + '_ {
        self.active_by_name
            .values()
            .filter_map(active_identity_binding)
    }

    #[must_use]
    pub fn active_count(&self) -> usize {
        self.active_by_name.len()
    }

    /// Counts every generation whose shared World/sidecar state must remain
    /// allocated, including disconnected generations waiting for their last
    /// accepted operation to retire.
    #[must_use]
    pub(crate) fn retained_identity_count(&self) -> usize {
        self.active_by_name
            .len()
            .saturating_add(self.deferred_releases.len())
    }

    /// Counts accepted generation-bound operations which have not reached
    /// their terminal publication/reply boundary.
    #[must_use]
    pub(crate) fn outstanding_operation_count(&self) -> usize {
        self.active_by_name
            .values()
            .map(|active| {
                active
                    .operation_gate
                    .active_operations
                    .load(Ordering::Acquire)
            })
            .chain(self.deferred_releases.iter().map(|deferred| {
                deferred
                    .operation_gate
                    .active_operations
                    .load(Ordering::Acquire)
            }))
            .fold(0_usize, usize::saturating_add)
    }

    #[must_use]
    pub(crate) fn transfer_in_progress_count(&self) -> usize {
        self.active_by_name
            .values()
            .filter(|active| active.transfer_in_progress.is_some())
            .count()
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
        let active = self.active_by_name.drain();
        for (canonical_nickname, active) in active {
            self.deferred_releases.push(DeferredRelease {
                canonical_nickname,
                identity: released_identity(&active),
                operation_gate: active.operation_gate,
            });
        }
        self.collect_drained_releases()
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

    fn allocate_transfer_id(&mut self) -> Result<MigrationTransferId, IdentityError> {
        let value = self.next_transfer_id;
        self.next_transfer_id = value
            .checked_add(1)
            .ok_or(IdentityError::MigrationTransferIdExhausted)?;
        if value == 0 {
            return Err(IdentityError::MigrationTransferIdExhausted);
        }
        Ok(MigrationTransferId(value))
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
        sync::{Arc, atomic::Ordering},
        time::{Duration, Instant},
    };

    use super::{
        ChannelBinding, DisconnectOutcome, IdentityError, IdentityRegistry, MIGRATION_TTL,
        MigrationToken, ReleasedIdentity, UserNo,
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
    fn migration_preflight_freezes_source_and_success_is_revalidated_on_consume() {
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
        assert_eq!(identities.active_identity("Rider"), Some(before));
        assert_eq!(
            identities.authorize(session(1)),
            Err(IdentityError::TransferInProgress {
                nickname: "Rider".to_owned(),
            })
        );

        let completed = identities
            .complete_preflighted_migration(preflight, now)
            .unwrap();
        assert_eq!(completed.binding.owner, session(2));
        assert_eq!(completed.binding.channel, Some(CHANNEL));
        assert!(completed.binding.generation.get() > source.generation.get());
    }

    #[tokio::test]
    async fn migration_drain_waits_for_two_explicit_leases_without_lost_wakeup() {
        let now = Instant::now();
        let mut identities = IdentityRegistry::new();
        let source = identities
            .claim_at(session(1), SOURCE_IP, "Rider", 0)
            .unwrap();
        let permit = identities
            .begin_migration(session(1), CHANNEL, token(3_751), now)
            .unwrap();
        let operation = identities.admit_operation(session(1)).unwrap();
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

        // Retaining already-accepted work remains valid after admission closes.
        let child = operation.try_retain().unwrap();
        assert!(!preflight.operations_drained());
        {
            let wait = preflight.wait_for_operations_drained();
            tokio::pin!(wait);
            assert!(
                tokio::time::timeout(Duration::from_millis(1), &mut wait)
                    .await
                    .is_err()
            );
            drop(operation);
            assert!(
                tokio::time::timeout(Duration::from_millis(1), &mut wait)
                    .await
                    .is_err()
            );
            drop(child);
            tokio::time::timeout(Duration::from_secs(1), &mut wait)
                .await
                .unwrap()
                .unwrap();
        }

        let completed = identities
            .complete_preflighted_migration(preflight, now)
            .unwrap();
        assert_eq!(completed.binding.owner, session(2));
    }

    #[tokio::test]
    async fn expired_migration_wakes_its_drain_waiter_before_reopening_admission() {
        let now = Instant::now();
        let mut identities = IdentityRegistry::new();
        let source = identities
            .claim_at(session(1), SOURCE_IP, "Rider", 0)
            .unwrap();
        let permit = identities
            .begin_migration(session(1), CHANNEL, token(3_750), now)
            .unwrap();
        let operation = identities.admit_operation(session(1)).unwrap();
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

        let mut wait = Box::pin(preflight.wait_for_operations_drained());
        assert!(
            tokio::time::timeout(Duration::from_millis(1), &mut wait)
                .await
                .is_err()
        );
        assert!(identities.expire_migrations(permit.expires_at).is_empty());
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), &mut wait)
                .await
                .unwrap(),
            Err(IdentityError::StaleMigrationPreflight)
        );
        drop(wait);

        let reopened = identities.admit_operation(session(1)).unwrap();
        assert_eq!(reopened.binding(), &source);
        drop(reopened);
        drop(operation);
    }

    #[test]
    fn migration_completion_rejects_a_live_operation_and_reopens_exact_freeze() {
        let now = Instant::now();
        let mut identities = IdentityRegistry::new();
        let source = identities.claim(session(1), SOURCE_IP, "Rider").unwrap();
        let permit = identities
            .begin_migration(session(1), CHANNEL, token(3_752), now)
            .unwrap();
        let operation = identities.admit_operation(session(1)).unwrap();
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
            identities.complete_preflighted_migration(preflight, now),
            Err(IdentityError::MigrationOperationsActive)
        );
        assert!(identities.authorize(session(1)).is_ok());
        drop(operation);
    }

    #[test]
    fn operation_counter_rejects_overflow_and_never_underflows() {
        let mut identities = IdentityRegistry::new();
        identities.claim(session(1), SOURCE_IP, "Rider").unwrap();
        let gate = Arc::clone(
            &identities
                .active_by_name
                .get(&canonical_nickname_key("Rider"))
                .unwrap()
                .operation_gate,
        );
        gate.active_operations.store(usize::MAX, Ordering::Release);
        assert!(matches!(
            identities.admit_operation(session(1)),
            Err(IdentityError::OperationCountExhausted)
        ));
        gate.active_operations.store(0, Ordering::Release);
        gate.release();
        assert_eq!(gate.active_operations.load(Ordering::Acquire), 0);
    }

    #[test]
    fn ack_prevalidation_reserves_no_state_and_checks_generation_capacity() {
        let now = Instant::now();
        let mut identities = IdentityRegistry::new();
        let source = identities.claim(session(1), SOURCE_IP, "Rider").unwrap();
        let permit = identities
            .begin_migration(session(1), CHANNEL, token(3_756), now)
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
        identities.next_generation = u64::MAX;

        assert_eq!(
            identities.validate_preflighted_migration(&preflight, now),
            Err(IdentityError::GenerationExhausted)
        );
        assert_eq!(
            identities.active_identity_by_user_no(source.user_no),
            Some(source)
        );
        assert!(identities.abort_preflighted_migration(&preflight));
    }

    #[tokio::test]
    async fn completed_migration_installs_a_fresh_gate_isolated_from_the_old_drain() {
        let now = Instant::now();
        let mut identities = IdentityRegistry::new();
        let source = identities
            .claim_at(session(1), SOURCE_IP, "Rider", 0)
            .unwrap();
        let permit = identities
            .begin_migration(session(1), CHANNEL, token(3_753), now)
            .unwrap();
        let old_operation = identities.admit_operation(session(1)).unwrap();
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
        let old_gate = Arc::clone(&preflight.drain.gate);
        drop(old_operation);
        preflight.wait_for_operations_drained().await.unwrap();
        let completed = identities
            .complete_preflighted_migration(preflight, now)
            .unwrap();

        let new_operation = identities.admit_operation(session(2)).unwrap();
        let current = identities
            .active_by_name
            .get(&canonical_nickname_key("Rider"))
            .unwrap();
        assert!(!Arc::ptr_eq(&old_gate, &current.operation_gate));
        assert!(old_gate.is_drained());
        assert!(!current.operation_gate.is_drained());
        assert_eq!(
            new_operation.binding().generation,
            completed.binding.generation
        );
        drop(new_operation);
    }

    #[test]
    fn udp_source_admission_checks_activation_freeze_and_ownerlessness() {
        let before_activation = 6;
        let activated_epoch = 7;
        let mut identities = IdentityRegistry::new();
        let source = identities
            .claim_at(session(1), SOURCE_IP, "Rider", activated_epoch)
            .unwrap();

        assert!(matches!(
            identities.admit_udp_operation(source.user_no, before_activation),
            Err(IdentityError::UdpIngressPredatesGeneration)
        ));
        let operation = identities
            .admit_udp_operation(source.user_no, activated_epoch + 1)
            .unwrap();
        assert_eq!(operation.binding(), &source);
        drop(operation);

        let permit = identities
            .begin_migration(session(1), CHANNEL, token(3_754), Instant::now())
            .unwrap();
        let preflight = identities
            .preflight_migration(
                session(2),
                SOURCE_IP,
                source.user_no,
                CHANNEL.channel_id,
                permit.token,
                Instant::now(),
            )
            .unwrap();
        assert!(matches!(
            identities.admit_udp_operation(source.user_no, activated_epoch + 1),
            Err(IdentityError::TransferInProgress {
                nickname,
            })
                if nickname == "Rider"
        ));
        // Recipient resolution is deliberately separate from source admission.
        assert_eq!(
            identities.active_identity_by_user_no(source.user_no),
            Some(source.clone())
        );
        assert!(identities.abort_preflighted_migration(&preflight));
        assert!(matches!(
            identities.disconnect(session(1), Instant::now()),
            DisconnectOutcome::Deferred { .. }
        ));
        assert!(matches!(
            identities.admit_udp_operation(source.user_no, activated_epoch + 2),
            Err(IdentityError::IdentityOwnerUnavailable(user_no))
                if user_no == source.user_no.get()
        ));
    }

    #[test]
    fn disconnect_and_expiry_defer_cleanup_until_exact_generation_drains() {
        let now = Instant::now();
        let mut identities = IdentityRegistry::new();
        let first = identities.claim(session(1), SOURCE_IP, "First").unwrap();
        let first_operation = identities.admit_operation(session(1)).unwrap();
        assert_eq!(
            identities.disconnect(session(1), now),
            DisconnectOutcome::Draining(ReleasedIdentity {
                nickname: first.nickname.clone(),
                user_no: first.user_no,
                generation: first.generation,
                source_ip: first.source_ip,
                channel: first.channel,
            })
        );
        assert!(identities.collect_drained_releases().is_empty());
        assert_eq!(
            identities.claim(session(2), SOURCE_IP, "first"),
            Err(IdentityError::DuplicateIdentity {
                nickname: "First".to_owned(),
            })
        );
        drop(first_operation);
        let released = identities.collect_drained_releases();
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].generation, first.generation);
        let replacement = identities.claim(session(2), SOURCE_IP, "first").unwrap();
        assert_eq!(replacement.user_no, first.user_no);
        assert!(replacement.generation.get() > first.generation.get());

        let expiring = identities.claim(session(3), OTHER_IP, "Expiring").unwrap();
        let permit = identities
            .begin_migration(session(3), CHANNEL, token(3_755), now)
            .unwrap();
        let expiring_operation = identities.admit_operation(session(3)).unwrap();
        assert!(matches!(
            identities.disconnect(session(3), now),
            DisconnectOutcome::Deferred { .. }
        ));
        assert!(identities.expire_migrations(permit.expires_at).is_empty());
        assert!(identities.collect_drained_releases().is_empty());
        drop(expiring_operation);
        assert_eq!(
            identities.collect_drained_releases(),
            vec![ReleasedIdentity {
                nickname: expiring.nickname,
                user_no: expiring.user_no,
                generation: expiring.generation,
                source_ip: expiring.source_ip,
                channel: expiring.channel,
            }]
        );
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
    fn migration_preflight_blocks_replacing_the_source_permit_until_consume() {
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
        assert_eq!(
            identities.begin_migration(session(1), replacement_channel, permit.token, now),
            Err(IdentityError::TransferInProgress {
                nickname: "Rider".to_owned(),
            })
        );
        let completed = identities
            .complete_preflighted_migration(preflight, now)
            .unwrap();
        assert_eq!(completed.binding.channel, Some(CHANNEL));
    }

    #[test]
    fn stale_abort_cannot_release_a_new_freeze_for_the_same_permit() {
        let now = Instant::now();
        let mut identities = IdentityRegistry::new();
        let source = identities.claim(session(1), SOURCE_IP, "Rider").unwrap();
        let permit = identities
            .begin_migration(session(1), CHANNEL, token(379), now)
            .unwrap();
        let stale = identities
            .preflight_migration(
                session(2),
                SOURCE_IP,
                source.user_no,
                CHANNEL.channel_id,
                permit.token,
                now,
            )
            .unwrap();
        assert!(identities.abort_preflighted_migration(&stale));

        let current = identities
            .preflight_migration(
                session(2),
                SOURCE_IP,
                source.user_no,
                CHANNEL.channel_id,
                permit.token,
                now,
            )
            .unwrap();
        assert!(!identities.abort_preflighted_migration(&stale));
        assert!(matches!(
            identities.authorize(session(1)),
            Err(IdentityError::TransferInProgress { .. })
        ));

        let completed = identities
            .complete_preflighted_migration(current, now)
            .unwrap();
        assert_eq!(completed.binding.owner, session(2));
        assert!(!identities.abort_preflighted_migration(&stale));
        let new_generation_operation = identities.admit_operation(session(2)).unwrap();
        assert_eq!(
            new_generation_operation.binding().generation,
            completed.binding.generation
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
