//! Bounded actor and TCP loop for the P5136 messenger service.
//!
//! The actor owns the active login-identity mirror, [`MessengerHub`], and all
//! messenger transport endpoints. It never calls back into the login/world
//! actor. Integration must therefore publish identity changes in commit order
//! and await the corresponding handle method before exposing the new login
//! generation.
//!
//! There is no distributed transaction between actors. The world actor must
//! retain enough prior identity state to compensate until the messenger
//! acknowledgement arrives. A non-`Stopped` error after the world-side commit
//! must be rolled back before another command can observe that identity, or the
//! server must fail fast. [`MessengerServiceError::Stopped`] cannot be safely
//! compensated because messenger state is unknowable; it requires whole-server
//! cancellation.
//!
//! Identity publication awaits are an integration critical section, not
//! cancellation points. Once one of those futures has enqueued its command,
//! dropping it loses an outcome that may already be committed. The world actor
//! must drive it to completion under its non-cancellable ownership gate.

use std::{
    collections::{HashMap, HashSet},
    io,
    net::{IpAddr, SocketAddr},
    num::NonZeroU32,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    time::Duration,
};

use p5136_core::{
    messenger::{
        DEFAULT_MAX_MESSENGER_STRING_UNITS, MESSENGER_FRAME_HEADER_LENGTH, MessengerError,
        MessengerFrameError, MessengerRequest, decode_frame_length, encode_frame, parse_request,
        serialize_chat, serialize_guild_chat, serialize_invite_chat, serialize_leave_chat,
    },
    packet::PacketError,
};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{Notify, Semaphore, mpsc, oneshot},
    task::JoinHandle,
    time,
};

use crate::messenger_hub::{
    ChatClaim, EnterClaim, GenerationAdvance, GuildChatClaim, IdentityRelease, InviteClaim,
    LeaveClaim, MessengerDelivery, MessengerEvent, MessengerHub, MessengerHubError,
    MessengerHubLimits, MessengerIdentity, MessengerRoomId, MessengerSessionId,
};
use crate::packet_log::{PacketDirection, trace_packet};

pub const DEFAULT_MESSENGER_MAILBOX_CAPACITY: usize = 1_024;
pub const DEFAULT_MESSENGER_CONNECTION_CAPACITY: usize = 256;
pub const DEFAULT_MESSENGER_IDENTITY_CAPACITY: usize = 256;
pub const DEFAULT_MESSENGER_OUTBOUND_CAPACITY: usize = 64;
pub const DEFAULT_MAX_MESSENGER_PAYLOAD: usize = 64 * 1_024;

const SHUTDOWN_RUNNING: u8 = 0;
const SHUTDOWN_REQUESTED: u8 = 1;
const SHUTDOWN_COMPLETE: u8 = 2;
const SHUTDOWN_FAILED: u8 = 3;
const COMMAND_PENDING: u8 = 0;
const COMMAND_CLAIMED: u8 = 1;
const COMMAND_CANCELLED: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessengerRuntimeConfig {
    pub mailbox_capacity: usize,
    pub max_connections: usize,
    pub max_identities: usize,
    pub outbound_capacity: usize,
    pub max_frame_payload: usize,
    pub max_string_utf16_units: usize,
    pub enter_timeout: Duration,
    pub idle_timeout: Duration,
    pub write_timeout: Duration,
    pub hub_limits: MessengerHubLimits,
}

impl Default for MessengerRuntimeConfig {
    fn default() -> Self {
        Self {
            mailbox_capacity: DEFAULT_MESSENGER_MAILBOX_CAPACITY,
            max_connections: DEFAULT_MESSENGER_CONNECTION_CAPACITY,
            max_identities: DEFAULT_MESSENGER_IDENTITY_CAPACITY,
            outbound_capacity: DEFAULT_MESSENGER_OUTBOUND_CAPACITY,
            max_frame_payload: DEFAULT_MAX_MESSENGER_PAYLOAD,
            max_string_utf16_units: DEFAULT_MAX_MESSENGER_STRING_UNITS,
            enter_timeout: Duration::from_secs(12),
            idle_timeout: Duration::from_secs(5 * 60),
            write_timeout: Duration::from_secs(15),
            hub_limits: MessengerHubLimits::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessengerCancellation {
    Replaced,
    IdentityReleased,
    Backpressure,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessengerGenerationAdvanceOutcome {
    /// `false` means an exact `(previous, next)` retry observed `next` already
    /// committed and acknowledged it without mutating the hub again.
    pub applied: bool,
    pub hub: GenerationAdvance,
    /// The endpoint whose generation was advanced in place. It remains live;
    /// no transport cancellation is required for a valid same-IP migration.
    pub retained_session: Option<MessengerSessionId>,
    /// Reserved for a future policy that cannot preserve an endpoint. It is
    /// currently always `None`; release/replacement outcomes carry real
    /// cancellation IDs.
    pub cancelled_session: Option<MessengerSessionId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessengerIdentityReleaseOutcome {
    pub applied: bool,
    pub hub: IdentityRelease,
    pub cancelled_session: Option<MessengerSessionId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessengerServiceSnapshot {
    pub announced_identities: usize,
    pub connections: usize,
    pub entered_sessions: usize,
    pub rooms: usize,
}

#[derive(Debug, Error)]
pub enum MessengerServiceError {
    #[error("invalid messenger runtime configuration: {0}")]
    InvalidConfiguration(&'static str),

    #[error("messenger service actor has stopped")]
    Stopped,

    #[error("messenger connection limit {maximum} reached")]
    ConnectionLimitReached { maximum: usize },

    #[error("messenger active identity limit {maximum} reached")]
    IdentityLimitReached { maximum: usize },

    #[error("messenger Enter command reached the actor after its deadline")]
    EnterDeadlineElapsed,

    #[error("messenger command was cancelled before its actor commit point")]
    CommandCancelled,

    #[error("messenger session ID space is exhausted")]
    SessionIdExhausted,

    #[error("messenger session {0:?} is not registered")]
    UnknownSession(MessengerSessionId),

    #[error("messenger session {0:?} has not entered")]
    SessionNotEntered(MessengerSessionId),

    #[error("messenger identity user number {0} is not active")]
    UnknownIdentityUserNo(u32),

    #[error("messenger identity {0:?} is not active")]
    UnknownIdentity(String),

    #[error("messenger identity announcement conflicts with the active mirror")]
    IdentityConflict,

    #[error("messenger identity generation is stale")]
    StaleIdentityGeneration,

    #[error("PqEnterChatServer is valid only as the first complete frame")]
    UnexpectedEnter,

    #[error("messenger event carried an unsupported non-zero result {0}")]
    UnsupportedEventResult(i32),

    #[error(transparent)]
    Hub(#[from] MessengerHubError),

    #[error(transparent)]
    Frame(#[from] MessengerFrameError),

    #[error(transparent)]
    Packet(#[from] PacketError),
}

#[derive(Debug, Error)]
pub enum MessengerConnectionError {
    #[error("the first complete messenger frame was not PqEnterChatServer")]
    FirstFrameWasNotEnter,

    #[error("a messenger connection sent PqEnterChatServer more than once")]
    DuplicateEnter,

    #[error("messenger enter deadline elapsed")]
    EnterTimeout,

    #[error("messenger connection was idle for too long")]
    IdleTimeout,

    #[error("messenger write deadline elapsed")]
    WriteTimeout,

    #[error("messenger actor closed the outbound queue")]
    OutboundClosed,

    #[error("messenger actor stopped without orderly connection cancellation")]
    ActorStopped,

    #[error("messenger connection was cancelled: {0:?}")]
    Cancelled(MessengerCancellation),

    #[error(transparent)]
    Io(#[from] io::Error),

    #[error(transparent)]
    Frame(#[from] MessengerFrameError),

    #[error(transparent)]
    Protocol(#[from] MessengerError),

    #[error(transparent)]
    Service(#[from] MessengerServiceError),
}

#[derive(Debug, Clone)]
pub struct MessengerServiceHandle {
    sender: mpsc::Sender<MessengerCommand>,
    config: Arc<MessengerRuntimeConfig>,
    connection_permits: Arc<Semaphore>,
    shutdown: Arc<MessengerShutdownState>,
}

#[derive(Debug)]
enum MessengerCommand {
    AnnounceIdentity {
        identity: MessengerIdentity,
        reply: oneshot::Sender<Result<(), MessengerServiceError>>,
    },
    AdvanceIdentity {
        previous: MessengerIdentity,
        next: MessengerIdentity,
        reply: oneshot::Sender<Result<MessengerGenerationAdvanceOutcome, MessengerServiceError>>,
    },
    ReleaseIdentity {
        identity: MessengerIdentity,
        reply: oneshot::Sender<Result<MessengerIdentityReleaseOutcome, MessengerServiceError>>,
    },
    RegisterConnection {
        peer_ip: IpAddr,
        cancellation: oneshot::Sender<MessengerCancellation>,
        outbound: mpsc::Sender<Arc<[u8]>>,
        generation: Arc<AtomicU64>,
        reply: oneshot::Sender<Result<MessengerRegistrationLease, MessengerServiceError>>,
    },
    Enter {
        session: MessengerSessionId,
        claim: EnterClaim,
        deadline: time::Instant,
        gate: Arc<MessengerCommitGate>,
        reply: oneshot::Sender<Result<(), MessengerServiceError>>,
    },
    Dispatch {
        session: MessengerSessionId,
        expected_generation: u64,
        request: MessengerRequest,
        gate: Arc<MessengerCommitGate>,
        reply: oneshot::Sender<Result<(), MessengerServiceError>>,
    },
    Disconnect {
        session: MessengerSessionId,
    },
    Snapshot {
        reply: oneshot::Sender<MessengerServiceSnapshot>,
    },
    Shutdown,
}

#[derive(Debug)]
struct MessengerTransport {
    peer_ip: IpAddr,
    entered_key: Option<String>,
    generation: Arc<AtomicU64>,
    cancellation: Option<oneshot::Sender<MessengerCancellation>>,
    outbound: mpsc::Sender<Arc<[u8]>>,
}

#[derive(Debug)]
struct MessengerActor {
    config: Arc<MessengerRuntimeConfig>,
    hub: MessengerHub,
    identities_by_key: HashMap<String, MessengerIdentity>,
    identity_key_by_user_no: HashMap<NonZeroU32, String>,
    last_advance_by_key: HashMap<String, AppliedGenerationAdvance>,
    transports: HashMap<MessengerSessionId, MessengerTransport>,
    next_session_id: Option<MessengerSessionId>,
}

#[derive(Debug, Clone)]
struct AppliedGenerationAdvance {
    previous: MessengerIdentity,
    next: MessengerIdentity,
    retained_session: Option<MessengerSessionId>,
}

#[derive(Debug)]
struct MessengerCleanupQueue {
    pending: Mutex<HashSet<MessengerSessionId>>,
    wake: Notify,
}

#[derive(Debug)]
struct MessengerShutdownState {
    phase: AtomicU8,
    wake: Notify,
}

struct MessengerActorLifecycle {
    shutdown: Arc<MessengerShutdownState>,
}

struct MessengerShutdownRequest {
    shutdown: Arc<MessengerShutdownState>,
    enqueued: bool,
}

#[derive(Debug)]
struct MessengerCommitGate {
    state: AtomicU8,
}

struct MessengerCommitLease {
    gate: Arc<MessengerCommitGate>,
    completed: bool,
}

#[derive(Debug)]
struct MessengerRegistrationLease {
    session: MessengerSessionId,
    cleanup: Arc<MessengerCleanupQueue>,
    armed: bool,
}

#[derive(Debug)]
struct MessengerConnectionRegistration {
    lease: MessengerRegistrationLease,
    service: MessengerServiceHandle,
}

struct RegisteredConnection {
    registration: MessengerConnectionRegistration,
    generation: Arc<AtomicU64>,
    cancellation: oneshot::Receiver<MessengerCancellation>,
    outbound: mpsc::Receiver<Arc<[u8]>>,
}

impl MessengerCleanupQueue {
    fn new() -> Self {
        Self {
            pending: Mutex::new(HashSet::new()),
            wake: Notify::new(),
        }
    }

    /// A lease exists only for an actor-owned transport, so the set is bounded
    /// by `max_connections`. Duplicate cleanup attempts coalesce by session ID.
    fn request(&self, session: MessengerSessionId) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if pending.insert(session) {
            self.wake.notify_one();
        }
    }

    fn take_pending(&self) -> HashSet<MessengerSessionId> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::take(&mut *pending)
    }

    #[cfg(test)]
    fn pending_count(&self) -> usize {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

impl MessengerShutdownState {
    fn new() -> Self {
        Self {
            phase: AtomicU8::new(SHUTDOWN_RUNNING),
            wake: Notify::new(),
        }
    }

    fn complete(&self) {
        self.phase.store(SHUTDOWN_COMPLETE, Ordering::Release);
        self.wake.notify_waiters();
    }

    fn fail_if_incomplete(&self) {
        let mut current = self.phase.load(Ordering::Acquire);
        while current != SHUTDOWN_COMPLETE && current != SHUTDOWN_FAILED {
            match self.phase.compare_exchange_weak(
                current,
                SHUTDOWN_FAILED,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.wake.notify_waiters();
                    return;
                }
                Err(actual) => current = actual,
            }
        }
    }
}

impl Drop for MessengerActorLifecycle {
    fn drop(&mut self) {
        self.shutdown.fail_if_incomplete();
    }
}

impl Drop for MessengerShutdownRequest {
    fn drop(&mut self) {
        if !self.enqueued
            && self
                .shutdown
                .phase
                .compare_exchange(
                    SHUTDOWN_REQUESTED,
                    SHUTDOWN_RUNNING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
        {
            self.shutdown.wake.notify_waiters();
        }
    }
}

impl MessengerCommitGate {
    fn new() -> Self {
        Self {
            state: AtomicU8::new(COMMAND_PENDING),
        }
    }

    fn claim(&self) -> bool {
        self.state
            .compare_exchange(
                COMMAND_PENDING,
                COMMAND_CLAIMED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

impl MessengerCommitLease {
    fn new(gate: Arc<MessengerCommitGate>) -> Self {
        Self {
            gate,
            completed: false,
        }
    }

    fn complete(&mut self) {
        self.completed = true;
    }
}

impl Drop for MessengerCommitLease {
    fn drop(&mut self) {
        if !self.completed {
            let _ = self.gate.state.compare_exchange(
                COMMAND_PENDING,
                COMMAND_CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }
}

impl MessengerRegistrationLease {
    fn new(session: MessengerSessionId, cleanup: Arc<MessengerCleanupQueue>) -> Self {
        Self {
            session,
            cleanup,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for MessengerRegistrationLease {
    fn drop(&mut self) {
        if self.armed {
            self.cleanup.request(self.session);
        }
    }
}

impl MessengerRuntimeConfig {
    fn validate(self) -> Result<Self, MessengerServiceError> {
        for (name, value) in [
            ("mailbox_capacity", self.mailbox_capacity),
            ("max_connections", self.max_connections),
            ("max_identities", self.max_identities),
            ("outbound_capacity", self.outbound_capacity),
            ("max_string_utf16_units", self.max_string_utf16_units),
        ] {
            if value == 0 {
                return Err(MessengerServiceError::InvalidConfiguration(name));
            }
        }
        if self.max_frame_payload < 4 {
            return Err(MessengerServiceError::InvalidConfiguration(
                "max_frame_payload",
            ));
        }
        for (name, value) in [
            ("enter_timeout", self.enter_timeout),
            ("idle_timeout", self.idle_timeout),
            ("write_timeout", self.write_timeout),
        ] {
            if value.is_zero() {
                return Err(MessengerServiceError::InvalidConfiguration(name));
            }
        }
        if self.hub_limits.max_message_utf16_units > self.max_string_utf16_units {
            return Err(MessengerServiceError::InvalidConfiguration(
                "hub message limit exceeds parser string limit",
            ));
        }
        Ok(self)
    }
}

impl MessengerServiceHandle {
    #[must_use]
    pub(crate) fn max_identities(&self) -> usize {
        self.config.max_identities
    }

    /// Starts the messenger actor.
    ///
    /// Identity publishing is deliberately one-way: the messenger actor never
    /// calls the login/world actor. After committing a login identity change,
    /// integration must await `announce_identity`, `advance_identity`, or
    /// `release_identity` before processing the next command for that identity.
    /// Until that acknowledgement, it must retain the previous state needed to
    /// compensate a rejected publication. [`MessengerServiceError::Stopped`]
    /// is fatal and must trigger whole-server cancellation; it is not safe to
    /// keep serving with divergent actor state. Publication futures must never
    /// be dropped for request cancellation after they are started.
    pub fn spawn(
        config: MessengerRuntimeConfig,
    ) -> Result<(Self, JoinHandle<()>), MessengerServiceError> {
        let config = Arc::new(config.validate()?);
        let hub = MessengerHub::new(config.hub_limits)?;
        let (sender, receiver) = mpsc::channel(config.mailbox_capacity);
        let cleanup = Arc::new(MessengerCleanupQueue::new());
        let shutdown = Arc::new(MessengerShutdownState::new());
        let handle = Self {
            sender,
            config: Arc::clone(&config),
            connection_permits: Arc::new(Semaphore::new(config.max_connections)),
            shutdown: Arc::clone(&shutdown),
        };
        let actor = MessengerActor {
            config,
            hub,
            identities_by_key: HashMap::new(),
            identity_key_by_user_no: HashMap::new(),
            last_advance_by_key: HashMap::new(),
            transports: HashMap::new(),
            next_session_id: MessengerSessionId::new(1),
        };
        let task = tokio::spawn(run_messenger_actor(actor, receiver, cleanup, shutdown));
        Ok((handle, task))
    }

    /// Announces an active login identity. Active identities do not consume
    /// `MessengerHub::max_sessions`; that cap is applied only after TCP Enter.
    ///
    /// Identical retries are idempotent. Once called, this future must be
    /// awaited non-cancellably because dropping it can lose a committed reply.
    pub async fn announce_identity(
        &self,
        identity: MessengerIdentity,
    ) -> Result<(), MessengerServiceError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(MessengerCommand::AnnounceIdentity { identity, reply })
            .await
            .map_err(|_| MessengerServiceError::Stopped)?;
        response.await.map_err(|_| MessengerServiceError::Stopped)?
    }

    /// Advances one committed login generation without dropping a valid
    /// same-IP messenger endpoint. The outcome names that retained endpoint.
    ///
    /// An exact `(previous, next)` retry is idempotent and returns
    /// `applied == false` when `next` is already current. Once called, this
    /// future must be awaited non-cancellably because dropping it can lose a
    /// committed reply.
    pub async fn advance_identity(
        &self,
        previous: MessengerIdentity,
        next: MessengerIdentity,
    ) -> Result<MessengerGenerationAdvanceOutcome, MessengerServiceError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(MessengerCommand::AdvanceIdentity {
                previous,
                next,
                reply,
            })
            .await
            .map_err(|_| MessengerServiceError::Stopped)?;
        response.await.map_err(|_| MessengerServiceError::Stopped)?
    }

    /// Releases only the exact current identity generation. A stale release is
    /// a successful idempotent no-op; an applied release cancels the returned
    /// endpoint. Once called, this future must be awaited non-cancellably
    /// because dropping it can lose a committed reply.
    pub async fn release_identity(
        &self,
        identity: MessengerIdentity,
    ) -> Result<MessengerIdentityReleaseOutcome, MessengerServiceError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(MessengerCommand::ReleaseIdentity { identity, reply })
            .await
            .map_err(|_| MessengerServiceError::Stopped)?;
        response.await.map_err(|_| MessengerServiceError::Stopped)?
    }

    pub async fn snapshot(&self) -> Result<MessengerServiceSnapshot, MessengerServiceError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(MessengerCommand::Snapshot { reply })
            .await
            .map_err(|_| MessengerServiceError::Stopped)?;
        response.await.map_err(|_| MessengerServiceError::Stopped)
    }

    /// Runs one TCP or duplex messenger connection with a single bounded
    /// writer. The first complete frame must be Enter before the deadline.
    pub async fn serve_connection<S>(
        &self,
        stream: S,
        peer: SocketAddr,
    ) -> Result<(), MessengerConnectionError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let _connection_permit = Arc::clone(&self.connection_permits)
            .try_acquire_owned()
            .map_err(|_| MessengerServiceError::ConnectionLimitReached {
                maximum: self.config.max_connections,
            })?;
        let deadline = time::Instant::now() + self.config.enter_timeout;
        let registered = self.register_connection(peer.ip()).await?;
        let session = registered.registration.lease.session;
        let result = run_registered_connection(
            stream,
            peer,
            session,
            self,
            registered.generation,
            registered.cancellation,
            registered.outbound,
            deadline,
        )
        .await;
        let close_result = registered.registration.close().await;
        match (result, close_result) {
            (Err(error), _) => Err(error),
            (Ok(()), Err(error)) => Err(error.into()),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    pub async fn shutdown(&self) -> Result<(), MessengerServiceError> {
        loop {
            let notified = self.shutdown.wake.notified();
            match self.shutdown.phase.load(Ordering::Acquire) {
                SHUTDOWN_COMPLETE => return Ok(()),
                SHUTDOWN_FAILED => return Err(MessengerServiceError::Stopped),
                SHUTDOWN_REQUESTED => notified.await,
                SHUTDOWN_RUNNING => {
                    if self
                        .shutdown
                        .phase
                        .compare_exchange(
                            SHUTDOWN_RUNNING,
                            SHUTDOWN_REQUESTED,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_err()
                    {
                        continue;
                    }
                    let mut request = MessengerShutdownRequest {
                        shutdown: Arc::clone(&self.shutdown),
                        enqueued: false,
                    };
                    if self.sender.send(MessengerCommand::Shutdown).await.is_err() {
                        self.shutdown.fail_if_incomplete();
                        return Err(MessengerServiceError::Stopped);
                    }
                    request.enqueued = true;
                }
                _ => unreachable!("messenger shutdown phase is internally bounded"),
            }
        }
    }

    async fn register_connection(
        &self,
        peer_ip: IpAddr,
    ) -> Result<RegisteredConnection, MessengerServiceError> {
        let (cancellation, cancelled) = oneshot::channel();
        let (outbound, outbound_receiver) = mpsc::channel(self.config.outbound_capacity);
        let generation = Arc::new(AtomicU64::new(0));
        let (reply, response) = oneshot::channel();
        self.sender
            .send(MessengerCommand::RegisterConnection {
                peer_ip,
                cancellation,
                outbound,
                generation: Arc::clone(&generation),
                reply,
            })
            .await
            .map_err(|_| MessengerServiceError::Stopped)?;
        let lease = response
            .await
            .map_err(|_| MessengerServiceError::Stopped)??;
        Ok(RegisteredConnection {
            registration: MessengerConnectionRegistration {
                lease,
                service: self.clone(),
            },
            generation,
            cancellation: cancelled,
            outbound: outbound_receiver,
        })
    }

    async fn enter(
        &self,
        session: MessengerSessionId,
        claim: EnterClaim,
        deadline: time::Instant,
    ) -> Result<(), MessengerServiceError> {
        let (reply, response) = oneshot::channel();
        let gate = Arc::new(MessengerCommitGate::new());
        let mut commit = MessengerCommitLease::new(Arc::clone(&gate));
        self.sender
            .send(MessengerCommand::Enter {
                session,
                claim,
                deadline,
                gate,
                reply,
            })
            .await
            .map_err(|_| MessengerServiceError::Stopped)?;
        let result = response.await.map_err(|_| MessengerServiceError::Stopped)?;
        commit.complete();
        result
    }

    async fn dispatch(
        &self,
        session: MessengerSessionId,
        expected_generation: u64,
        request: MessengerRequest,
    ) -> Result<(), MessengerServiceError> {
        let (reply, response) = oneshot::channel();
        let gate = Arc::new(MessengerCommitGate::new());
        let mut commit = MessengerCommitLease::new(Arc::clone(&gate));
        self.sender
            .send(MessengerCommand::Dispatch {
                session,
                expected_generation,
                request,
                gate,
                reply,
            })
            .await
            .map_err(|_| MessengerServiceError::Stopped)?;
        let result = response.await.map_err(|_| MessengerServiceError::Stopped)?;
        commit.complete();
        result
    }

    async fn disconnect(&self, session: MessengerSessionId) -> Result<(), MessengerServiceError> {
        self.sender
            .send(MessengerCommand::Disconnect { session })
            .await
            .map_err(|_| MessengerServiceError::Stopped)
    }
}

impl MessengerConnectionRegistration {
    async fn close(mut self) -> Result<(), MessengerServiceError> {
        self.service.disconnect(self.lease.session).await?;
        self.lease.disarm();
        Ok(())
    }
}

impl MessengerActor {
    fn validate_identity(&self, identity: &MessengerIdentity) -> Result<(), MessengerServiceError> {
        if identity.nickname.encode_utf16().count() > self.config.max_string_utf16_units {
            return Err(MessengerServiceError::IdentityConflict);
        }
        Ok(())
    }

    fn announce_identity(
        &mut self,
        identity: MessengerIdentity,
    ) -> Result<(), MessengerServiceError> {
        self.validate_identity(&identity)?;
        let key = identity.canonical_key().to_owned();
        if let Some(current) = self.identities_by_key.get(&key) {
            return if current == &identity {
                Ok(())
            } else {
                Err(MessengerServiceError::IdentityConflict)
            };
        }
        if self
            .identity_key_by_user_no
            .get(&identity.user_no)
            .is_some_and(|existing| existing != &key)
        {
            return Err(MessengerServiceError::IdentityConflict);
        }
        if self.identities_by_key.len() >= self.config.max_identities {
            return Err(MessengerServiceError::IdentityLimitReached {
                maximum: self.config.max_identities,
            });
        }
        self.identity_key_by_user_no
            .insert(identity.user_no, key.clone());
        self.last_advance_by_key.remove(&key);
        self.identities_by_key.insert(key, identity);
        Ok(())
    }

    fn advance_identity(
        &mut self,
        previous: &MessengerIdentity,
        next: MessengerIdentity,
    ) -> Result<MessengerGenerationAdvanceOutcome, MessengerServiceError> {
        self.validate_identity(previous)?;
        self.validate_identity(&next)?;
        let key = previous.canonical_key();
        if next.canonical_key() != key || next.user_no != previous.user_no {
            return Err(MessengerServiceError::IdentityConflict);
        }
        if next.source_ip != previous.source_ip
            || next.generation.get() <= previous.generation.get()
        {
            return Err(MessengerHubError::MigrationIdentityMismatch.into());
        }

        let index_is_current = self
            .identity_key_by_user_no
            .get(&previous.user_no)
            .map(String::as_str)
            == Some(key);
        if self.identities_by_key.get(key) == Some(&next) && index_is_current {
            let transition = self
                .last_advance_by_key
                .get(key)
                .filter(|transition| transition.previous == *previous && transition.next == next)
                .ok_or(MessengerServiceError::StaleIdentityGeneration)?;
            return Ok(MessengerGenerationAdvanceOutcome {
                applied: false,
                hub: GenerationAdvance {
                    endpoint_updated: false,
                    room_members_updated: 0,
                },
                retained_session: transition.retained_session,
                cancelled_session: None,
            });
        }
        if self.identities_by_key.get(key) != Some(previous) || !index_is_current {
            return Err(MessengerServiceError::StaleIdentityGeneration);
        }

        let hub = self.hub.advance_generation(previous, &next)?;
        let retained_session = hub
            .endpoint_updated
            .then(|| self.hub.session_for_identity(&next.nickname))
            .flatten();
        let next_generation = next.generation.get();
        self.identities_by_key.insert(key.to_owned(), next.clone());
        self.last_advance_by_key.insert(
            key.to_owned(),
            AppliedGenerationAdvance {
                previous: previous.clone(),
                next,
                retained_session,
            },
        );
        if let Some(session) = retained_session
            && let Some(transport) = self.transports.get(&session)
        {
            transport
                .generation
                .store(next_generation, Ordering::Release);
        }
        Ok(MessengerGenerationAdvanceOutcome {
            applied: true,
            hub,
            retained_session,
            cancelled_session: None,
        })
    }

    fn release_identity(
        &mut self,
        identity: &MessengerIdentity,
    ) -> MessengerIdentityReleaseOutcome {
        let key = identity.canonical_key();
        if self.identities_by_key.get(key) != Some(identity) {
            return MessengerIdentityReleaseOutcome {
                applied: false,
                hub: IdentityRelease {
                    disconnected_session: None,
                    room_memberships_removed: 0,
                    empty_rooms_removed: 0,
                },
                cancelled_session: None,
            };
        }

        self.identities_by_key.remove(key);
        self.last_advance_by_key.remove(key);
        if self
            .identity_key_by_user_no
            .get(&identity.user_no)
            .map(String::as_str)
            == Some(key)
        {
            self.identity_key_by_user_no.remove(&identity.user_no);
        }
        let hub = self.hub.release_identity(identity);
        let cancelled_session = hub.disconnected_session;
        if let Some(session) = cancelled_session {
            self.remove_transport_only(session, MessengerCancellation::IdentityReleased);
        }
        MessengerIdentityReleaseOutcome {
            applied: true,
            hub,
            cancelled_session,
        }
    }

    fn register_connection(
        &mut self,
        peer_ip: IpAddr,
        cancellation: oneshot::Sender<MessengerCancellation>,
        outbound: mpsc::Sender<Arc<[u8]>>,
        generation: Arc<AtomicU64>,
    ) -> Result<MessengerSessionId, MessengerServiceError> {
        if self.transports.len() >= self.config.max_connections {
            return Err(MessengerServiceError::ConnectionLimitReached {
                maximum: self.config.max_connections,
            });
        }
        let session = self
            .next_session_id
            .ok_or(MessengerServiceError::SessionIdExhausted)?;
        self.next_session_id = session
            .get()
            .checked_add(1)
            .and_then(MessengerSessionId::new);
        self.transports.insert(
            session,
            MessengerTransport {
                peer_ip,
                entered_key: None,
                generation,
                cancellation: Some(cancellation),
                outbound,
            },
        );
        Ok(session)
    }

    fn enter(
        &mut self,
        session: MessengerSessionId,
        claim: &EnterClaim,
        deadline: time::Instant,
    ) -> Result<(), MessengerServiceError> {
        if time::Instant::now() >= deadline {
            return Err(MessengerServiceError::EnterDeadlineElapsed);
        }
        let transport = self
            .transports
            .get(&session)
            .ok_or(MessengerServiceError::UnknownSession(session))?;
        if transport.entered_key.is_some() {
            return Err(MessengerHubError::SessionAlreadyEntered(session).into());
        }
        let peer_ip = transport.peer_ip;
        let user_no = NonZeroU32::new(claim.user_no)
            .ok_or(MessengerServiceError::UnknownIdentityUserNo(claim.user_no))?;
        let key = self
            .identity_key_by_user_no
            .get(&user_no)
            .ok_or(MessengerServiceError::UnknownIdentityUserNo(claim.user_no))?;
        let active = self
            .identities_by_key
            .get(key)
            .cloned()
            .ok_or_else(|| MessengerServiceError::UnknownIdentity(claim.nickname.clone()))?;
        let outcome = self.hub.enter(session, peer_ip, &active, claim)?;

        self.transports
            .get_mut(&session)
            .expect("validated messenger transport remains registered")
            .entered_key = Some(active.canonical_key().to_owned());
        self.transports
            .get(&session)
            .expect("entered messenger transport remains registered")
            .generation
            .store(active.generation.get(), Ordering::Release);
        if let Some(replaced) = outcome.replaced_session {
            self.remove_transport_only(replaced, MessengerCancellation::Replaced);
        }
        Ok(())
    }

    fn current_identity(
        &self,
        session: MessengerSessionId,
    ) -> Result<MessengerIdentity, MessengerServiceError> {
        let transport = self
            .transports
            .get(&session)
            .ok_or(MessengerServiceError::UnknownSession(session))?;
        let key = transport
            .entered_key
            .as_deref()
            .ok_or(MessengerServiceError::SessionNotEntered(session))?;
        self.identities_by_key
            .get(key)
            .cloned()
            .ok_or_else(|| MessengerServiceError::UnknownIdentity(key.to_owned()))
    }

    fn target_identity(&self, user_no: u32) -> Result<MessengerIdentity, MessengerServiceError> {
        let user_no = NonZeroU32::new(user_no)
            .ok_or(MessengerServiceError::UnknownIdentityUserNo(user_no))?;
        let key = self
            .identity_key_by_user_no
            .get(&user_no)
            .ok_or(MessengerServiceError::UnknownIdentityUserNo(user_no.get()))?;
        self.identities_by_key
            .get(key)
            .cloned()
            .ok_or_else(|| MessengerServiceError::UnknownIdentity(key.clone()))
    }

    fn dispatch(
        &mut self,
        session: MessengerSessionId,
        expected_generation: u64,
        request: MessengerRequest,
    ) -> Result<(), MessengerServiceError> {
        let sender = self.current_identity(session)?;
        if sender.generation.get() != expected_generation {
            return Err(MessengerServiceError::StaleIdentityGeneration);
        }
        let deliveries = match request {
            MessengerRequest::EnterChatServer { .. } => {
                return Err(MessengerServiceError::UnexpectedEnter);
            }
            MessengerRequest::InviteChat {
                inviter_user_no,
                invitee_user_no,
                inviter_nickname,
                invitee_nickname,
            } => {
                let target = self.target_identity(invitee_user_no)?;
                let claim = InviteClaim {
                    inviter_user_no,
                    invitee_user_no,
                    inviter_nickname,
                    invitee_nickname,
                };
                self.preflight_event(&MessengerEvent::InviteChat {
                    inviter_user_no: sender.user_no.get(),
                    invitee_user_no: target.user_no.get(),
                    inviter_nickname: sender.nickname.clone(),
                    invitee_nickname: target.nickname.clone(),
                    room_id: MessengerRoomId::new(1).expect("one is a valid messenger room ID"),
                    result: 0,
                })?;
                self.hub.invite(session, &sender, &target, &claim)?
            }
            MessengerRequest::Chat {
                room_id,
                nickname,
                message,
            } => {
                self.preflight_event(&MessengerEvent::Chat {
                    room_id: MessengerRoomId::new(room_id)
                        .ok_or(MessengerHubError::InvalidRoomId(room_id))?,
                    sender_user_no: sender.user_no.get(),
                    nickname: sender.nickname.clone(),
                    message: Arc::from(message.as_str()),
                    result: 0,
                })?;
                self.hub.chat(
                    session,
                    &sender,
                    ChatClaim {
                        room_id,
                        nickname,
                        message,
                    },
                )?
            }
            MessengerRequest::LeaveChat { user_no, room_id } => {
                self.preflight_event(&MessengerEvent::LeaveChat {
                    user_no: sender.user_no.get(),
                    room_id: MessengerRoomId::new(room_id)
                        .ok_or(MessengerHubError::InvalidRoomId(room_id))?,
                })?;
                self.hub
                    .leave(session, &sender, &LeaveClaim { user_no, room_id })?
            }
            MessengerRequest::GuildChat { nickname, message } => {
                self.preflight_event(&MessengerEvent::GuildChat {
                    nickname: sender.nickname.clone(),
                    message: Arc::from(message.as_str()),
                })?;
                self.hub
                    .guild_chat(session, &sender, GuildChatClaim { nickname, message })?
            }
        };
        self.enqueue_deliveries(deliveries)
    }

    fn preflight_event(&self, event: &MessengerEvent) -> Result<(), MessengerServiceError> {
        let _ = encode_event(event, self.config.max_frame_payload)?;
        Ok(())
    }

    fn enqueue_deliveries(
        &mut self,
        deliveries: Vec<MessengerDelivery>,
    ) -> Result<(), MessengerServiceError> {
        let Some(event) = deliveries.first().map(|delivery| delivery.event.clone()) else {
            return Ok(());
        };
        let frame = encode_event(&event, self.config.max_frame_payload)?;
        let mut failed = Vec::new();
        for delivery in deliveries {
            debug_assert_eq!(delivery.event, event);
            let send_failed = self
                .transports
                .get(&delivery.session)
                .is_some_and(|transport| transport.outbound.try_send(Arc::clone(&frame)).is_err());
            if send_failed {
                failed.push(delivery.session);
            }
        }
        failed.sort_unstable();
        failed.dedup();
        for session in failed {
            self.disconnect_endpoint(session, MessengerCancellation::Backpressure);
        }
        Ok(())
    }

    fn remove_transport_only(
        &mut self,
        session: MessengerSessionId,
        reason: MessengerCancellation,
    ) {
        if let Some(mut transport) = self.transports.remove(&session)
            && let Some(cancellation) = transport.cancellation.take()
        {
            let _ = cancellation.send(reason);
        }
    }

    fn disconnect_endpoint(&mut self, session: MessengerSessionId, reason: MessengerCancellation) {
        self.remove_transport_only(session, reason);
        let _ = self.hub.disconnect_session(session);
    }

    fn disconnect_closed(&mut self, session: MessengerSessionId) {
        self.transports.remove(&session);
        let _ = self.hub.disconnect_session(session);
    }

    fn drain_cleanup(&mut self, cleanup: &MessengerCleanupQueue) {
        let mut sessions = cleanup.take_pending().into_iter().collect::<Vec<_>>();
        sessions.sort_unstable();
        for session in sessions {
            self.disconnect_closed(session);
        }
    }

    fn shutdown(&mut self) {
        let sessions = self.transports.keys().copied().collect::<Vec<_>>();
        for session in sessions {
            self.disconnect_endpoint(session, MessengerCancellation::Shutdown);
        }
    }

    fn snapshot(&self) -> MessengerServiceSnapshot {
        MessengerServiceSnapshot {
            announced_identities: self.identities_by_key.len(),
            connections: self.transports.len(),
            entered_sessions: self.hub.session_count(),
            rooms: self.hub.room_count(),
        }
    }
}

async fn run_messenger_actor(
    mut actor: MessengerActor,
    mut receiver: mpsc::Receiver<MessengerCommand>,
    cleanup: Arc<MessengerCleanupQueue>,
    shutdown: Arc<MessengerShutdownState>,
) {
    let _lifecycle = MessengerActorLifecycle {
        shutdown: Arc::clone(&shutdown),
    };
    loop {
        let command = tokio::select! {
            biased;
            () = cleanup.wake.notified() => {
                actor.drain_cleanup(&cleanup);
                continue;
            }
            command = receiver.recv() => command,
        };
        let Some(command) = command else {
            break;
        };
        let shutdown = match command {
            MessengerCommand::AnnounceIdentity { identity, reply } => {
                let _ = reply.send(actor.announce_identity(identity));
                false
            }
            MessengerCommand::AdvanceIdentity {
                previous,
                next,
                reply,
            } => {
                let _ = reply.send(actor.advance_identity(&previous, next));
                false
            }
            MessengerCommand::ReleaseIdentity { identity, reply } => {
                let outcome = actor.release_identity(&identity);
                let _ = reply.send(Ok(outcome));
                false
            }
            MessengerCommand::RegisterConnection {
                peer_ip,
                cancellation,
                outbound,
                generation,
                reply,
            } => {
                let registration = actor
                    .register_connection(peer_ip, cancellation, outbound, generation)
                    .map(|session| MessengerRegistrationLease::new(session, Arc::clone(&cleanup)));
                let _ = reply.send(registration);
                false
            }
            MessengerCommand::Enter {
                session,
                claim,
                deadline,
                gate,
                reply,
            } => {
                let result = if gate.claim() {
                    actor.enter(session, &claim, deadline)
                } else {
                    Err(MessengerServiceError::CommandCancelled)
                };
                let _ = reply.send(result);
                false
            }
            MessengerCommand::Dispatch {
                session,
                expected_generation,
                request,
                gate,
                reply,
            } => {
                let result = if gate.claim() {
                    actor.dispatch(session, expected_generation, request)
                } else {
                    Err(MessengerServiceError::CommandCancelled)
                };
                let _ = reply.send(result);
                false
            }
            MessengerCommand::Disconnect { session } => {
                actor.disconnect_closed(session);
                false
            }
            MessengerCommand::Snapshot { reply } => {
                let _ = reply.send(actor.snapshot());
                false
            }
            MessengerCommand::Shutdown => {
                actor.shutdown();
                shutdown.complete();
                true
            }
        };
        if shutdown {
            return;
        }
    }
    actor.shutdown();
}

fn encode_event(
    event: &MessengerEvent,
    maximum: usize,
) -> Result<Arc<[u8]>, MessengerServiceError> {
    let logical = match event {
        MessengerEvent::InviteChat {
            inviter_user_no,
            invitee_user_no,
            inviter_nickname,
            invitee_nickname,
            room_id,
            result,
        } => {
            if *result != 0 {
                return Err(MessengerServiceError::UnsupportedEventResult(*result));
            }
            serialize_invite_chat(
                *inviter_user_no,
                *invitee_user_no,
                inviter_nickname,
                invitee_nickname,
                room_id.get(),
            )?
        }
        MessengerEvent::Chat {
            room_id,
            sender_user_no,
            nickname,
            message,
            result,
        } => {
            if *result != 0 {
                return Err(MessengerServiceError::UnsupportedEventResult(*result));
            }
            serialize_chat(room_id.get(), *sender_user_no, nickname, message.as_ref())?
        }
        MessengerEvent::LeaveChat { user_no, room_id } => {
            serialize_leave_chat(*user_no, room_id.get())
        }
        MessengerEvent::GuildChat { nickname, message } => {
            serialize_guild_chat(nickname, message.as_ref())?
        }
    };
    Ok(Arc::from(encode_frame(&logical, maximum)?))
}

/// Reads one exact messenger frame, validating the signed length before body
/// allocation. Repeated calls correctly split coalesced frames.
pub async fn read_messenger_frame<R>(
    reader: &mut R,
    maximum: usize,
) -> Result<Vec<u8>, MessengerConnectionError>
where
    R: AsyncRead + Unpin,
{
    let mut header = [0_u8; MESSENGER_FRAME_HEADER_LENGTH];
    reader.read_exact(&mut header).await?;
    let length = decode_frame_length(header, maximum)?;
    let mut logical = vec![0_u8; length];
    reader.read_exact(&mut logical).await?;
    Ok(logical)
}

/// Reads one messenger frame while preserving partial/malformed wire input in
/// the packet diagnostics. Public codec tests use the bare reader above;
/// live connections use this peer-aware transport boundary.
async fn read_messenger_frame_with_diagnostics<R>(
    reader: &mut R,
    maximum: usize,
    peer: SocketAddr,
) -> Result<Vec<u8>, MessengerConnectionError>
where
    R: AsyncRead + Unpin,
{
    let mut header = [0_u8; MESSENGER_FRAME_HEADER_LENGTH];
    let mut header_read = 0;
    if let Err(error) = read_messenger_exact_with_count(reader, &mut header, &mut header_read).await
    {
        if header_read != 0 {
            trace_packet(
                "messenger-tcp",
                "partial-wire",
                PacketDirection::Received,
                Some(peer),
                &header[..header_read],
            );
        }
        return Err(error.into());
    }
    let length = match decode_frame_length(header, maximum) {
        Ok(length) => length,
        Err(error) => {
            trace_packet(
                "messenger-tcp",
                "wire",
                PacketDirection::Received,
                Some(peer),
                &header,
            );
            return Err(error.into());
        }
    };
    let mut wire = Vec::with_capacity(MESSENGER_FRAME_HEADER_LENGTH + length);
    wire.extend_from_slice(&header);
    wire.resize(MESSENGER_FRAME_HEADER_LENGTH + length, 0);
    let mut body_read = 0;
    if let Err(error) = read_messenger_exact_with_count(
        reader,
        &mut wire[MESSENGER_FRAME_HEADER_LENGTH..],
        &mut body_read,
    )
    .await
    {
        trace_packet(
            "messenger-tcp",
            "partial-wire",
            PacketDirection::Received,
            Some(peer),
            &wire[..MESSENGER_FRAME_HEADER_LENGTH + body_read],
        );
        return Err(error.into());
    }
    Ok(wire.split_off(MESSENGER_FRAME_HEADER_LENGTH))
}

async fn read_messenger_exact_with_count<R>(
    reader: &mut R,
    buffer: &mut [u8],
    read: &mut usize,
) -> io::Result<()>
where
    R: AsyncRead + Unpin,
{
    while *read < buffer.len() {
        let count = reader.read(&mut buffer[*read..]).await?;
        if count == 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        *read += count;
    }
    Ok(())
}

/// Reads one authenticated frame and stamps the socket poll that consumes its
/// first byte.
///
/// A connection may sit in an empty read across a generation advance; bytes
/// arriving afterward belong to the retained new-generation endpoint. The
/// generation is loaded immediately before the nonblocking read poll and
/// checked immediately after `Ready`, closing the consume-then-preempt race.
/// Once that byte is admitted, an advance before the final byte makes the whole
/// frame stale.
async fn read_generation_fenced_messenger_frame<R>(
    reader: &mut R,
    maximum: usize,
    generation: &AtomicU64,
    peer: Option<SocketAddr>,
) -> Result<(Vec<u8>, u64), MessengerConnectionError>
where
    R: AsyncRead + Unpin,
{
    let mut header = [0_u8; MESSENGER_FRAME_HEADER_LENGTH];
    let admitted_generation = std::future::poll_fn(
        |context| -> std::task::Poll<Result<u64, MessengerConnectionError>> {
            let before = generation.load(Ordering::Acquire);
            if before == 0 {
                return std::task::Poll::Ready(Err(
                    MessengerServiceError::StaleIdentityGeneration.into()
                ));
            }
            let mut first_byte = tokio::io::ReadBuf::new(&mut header[..1]);
            match std::pin::Pin::new(&mut *reader).poll_read(context, &mut first_byte) {
                std::task::Poll::Pending => std::task::Poll::Pending,
                std::task::Poll::Ready(Err(error)) => std::task::Poll::Ready(Err(error.into())),
                std::task::Poll::Ready(Ok(())) if first_byte.filled().is_empty() => {
                    std::task::Poll::Ready(
                        Err(io::Error::from(io::ErrorKind::UnexpectedEof).into()),
                    )
                }
                std::task::Poll::Ready(Ok(())) => {
                    let after = generation.load(Ordering::Acquire);
                    if after == before {
                        std::task::Poll::Ready(Ok(before))
                    } else {
                        trace_packet(
                            "messenger-tcp",
                            "partial-wire",
                            PacketDirection::Received,
                            peer,
                            &header[..1],
                        );
                        std::task::Poll::Ready(Err(
                            MessengerServiceError::StaleIdentityGeneration.into()
                        ))
                    }
                }
            }
        },
    )
    .await?;
    let mut header_read = 1;
    if let Err(error) = read_messenger_exact_with_count(reader, &mut header, &mut header_read).await
    {
        trace_packet(
            "messenger-tcp",
            "partial-wire",
            PacketDirection::Received,
            peer,
            &header[..header_read],
        );
        return Err(error.into());
    }
    let length = match decode_frame_length(header, maximum) {
        Ok(length) => length,
        Err(error) => {
            trace_packet(
                "messenger-tcp",
                "wire",
                PacketDirection::Received,
                peer,
                &header,
            );
            return Err(error.into());
        }
    };
    let mut logical = vec![0_u8; length];
    let mut body_read = 0;
    if let Err(error) = read_messenger_exact_with_count(reader, &mut logical, &mut body_read).await
    {
        let mut partial = Vec::with_capacity(MESSENGER_FRAME_HEADER_LENGTH + body_read);
        partial.extend_from_slice(&header);
        partial.extend_from_slice(&logical[..body_read]);
        trace_packet(
            "messenger-tcp",
            "partial-wire",
            PacketDirection::Received,
            peer,
            &partial,
        );
        return Err(error.into());
    }
    if generation.load(Ordering::Acquire) != admitted_generation {
        trace_packet(
            "messenger-tcp",
            "logical",
            PacketDirection::Received,
            peer,
            &logical,
        );
        return Err(MessengerServiceError::StaleIdentityGeneration.into());
    }
    Ok((logical, admitted_generation))
}

#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the connection owner retains the independently linear session, generation, cancellation, bounded outbound capabilities, and complete lifecycle boundary together"
)]
async fn run_registered_connection<S>(
    stream: S,
    peer: SocketAddr,
    session: MessengerSessionId,
    service: &MessengerServiceHandle,
    generation: Arc<AtomicU64>,
    mut cancellation: oneshot::Receiver<MessengerCancellation>,
    mut outbound: mpsc::Receiver<Arc<[u8]>>,
    enter_deadline: time::Instant,
) -> Result<(), MessengerConnectionError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let config = Arc::clone(&service.config);
    let (mut reader, mut writer) = tokio::io::split(stream);
    let logical = tokio::select! {
        biased;
        cancelled = &mut cancellation => {
            return Err(cancellation_error(&cancelled));
        }
        result = time::timeout_at(
            enter_deadline,
            read_messenger_frame_with_diagnostics(&mut reader, config.max_frame_payload, peer),
        ) => {
            result.map_err(|_| MessengerConnectionError::EnterTimeout)??
        }
    };
    trace_packet(
        "messenger-tcp",
        "logical",
        PacketDirection::Received,
        Some(peer),
        &logical,
    );
    let request = parse_request(&logical, config.max_string_utf16_units)?;
    let MessengerRequest::EnterChatServer {
        user_no,
        chat_type,
        nickname,
    } = request
    else {
        return Err(MessengerConnectionError::FirstFrameWasNotEnter);
    };
    match service
        .enter(
            session,
            EnterClaim {
                user_no,
                chat_type,
                nickname,
            },
            enter_deadline,
        )
        .await
    {
        Ok(()) => {}
        Err(MessengerServiceError::EnterDeadlineElapsed) => {
            return Err(MessengerConnectionError::EnterTimeout);
        }
        Err(error) => return Err(error.into()),
    }

    loop {
        let read = time::timeout(
            config.idle_timeout,
            read_generation_fenced_messenger_frame(
                &mut reader,
                config.max_frame_payload,
                generation.as_ref(),
                Some(peer),
            ),
        );
        tokio::pin!(read);
        let (logical, frame_generation) = loop {
            let action = tokio::select! {
                biased;
                cancelled = &mut cancellation => {
                    return Err(cancellation_error(&cancelled));
                }
                action = poll_frame_or_outbound(read.as_mut(), &mut outbound) => action,
            };
            match action {
                FrameOrOutbound::Frame(result) => {
                    break result.map_err(|_| MessengerConnectionError::IdleTimeout)??;
                }
                FrameOrOutbound::Outbound(Some(frame)) => {
                    write_with_cancellation(
                        &mut writer,
                        &frame,
                        config.write_timeout,
                        &mut cancellation,
                        peer,
                    )
                    .await?;
                }
                FrameOrOutbound::Outbound(None) => {
                    return Err(MessengerConnectionError::OutboundClosed);
                }
            }
        };

        trace_packet(
            "messenger-tcp",
            "logical",
            PacketDirection::Received,
            Some(peer),
            &logical,
        );

        let request = parse_request(&logical, config.max_string_utf16_units)?;
        if matches!(request, MessengerRequest::EnterChatServer { .. }) {
            return Err(MessengerConnectionError::DuplicateEnter);
        }
        service.dispatch(session, frame_generation, request).await?;
    }
}

enum FrameOrOutbound<T> {
    Frame(T),
    Outbound(Option<Arc<[u8]>>),
}

async fn poll_frame_or_outbound<F, T>(
    frame: std::pin::Pin<&mut F>,
    outbound: &mut mpsc::Receiver<Arc<[u8]>>,
) -> FrameOrOutbound<T>
where
    F: std::future::Future<Output = T>,
{
    tokio::select! {
        result = frame => FrameOrOutbound::Frame(result),
        outbound = outbound.recv() => FrameOrOutbound::Outbound(outbound),
    }
}

async fn write_with_cancellation<W>(
    writer: &mut W,
    frame: &[u8],
    timeout: Duration,
    cancellation: &mut oneshot::Receiver<MessengerCancellation>,
    peer: SocketAddr,
) -> Result<(), MessengerConnectionError>
where
    W: AsyncWrite + Unpin,
{
    let result = tokio::select! {
        biased;
        cancelled = cancellation => Err(cancellation_error(&cancelled)),
        result = time::timeout(timeout, writer.write_all(frame)) => {
            result
                .map_err(|_| MessengerConnectionError::WriteTimeout)?
                .map_err(MessengerConnectionError::Io)
        }
    };
    result?;
    // Messenger outbound queues carry the 4-byte signed-length frame header;
    // the packet trace deliberately records the logical payload to match the
    // inbound side and the login TCP diagnostics after the write commits.
    trace_packet(
        "messenger-tcp",
        "logical",
        PacketDirection::Sent,
        Some(peer),
        frame.get(MESSENGER_FRAME_HEADER_LENGTH..).unwrap_or(frame),
    );
    Ok(())
}

fn cancellation_error(
    cancellation: &Result<MessengerCancellation, oneshot::error::RecvError>,
) -> MessengerConnectionError {
    match cancellation {
        Ok(reason) => MessengerConnectionError::Cancelled(*reason),
        Err(_) => MessengerConnectionError::ActorStopped,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr, SocketAddr},
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
        time::Duration,
    };

    use p5136_core::{
        adler32,
        messenger::{
            MESSENGER_FRAME_HEADER_LENGTH, MessengerFrameError, encode_frame, parse_request,
            serialize_chat, serialize_guild_chat, serialize_invite_chat, serialize_leave_chat,
        },
        packet::{PacketReader, PacketWriter},
    };
    use tokio::{
        io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, duplex},
        net::{TcpListener, TcpStream},
        sync::{mpsc, oneshot},
        task::JoinHandle,
        time,
    };

    use super::{
        MessengerCancellation, MessengerCleanupQueue, MessengerCommand, MessengerCommitGate,
        MessengerCommitLease, MessengerConnectionError, MessengerRegistrationLease,
        MessengerRuntimeConfig, MessengerServiceHandle, MessengerServiceSnapshot,
        read_generation_fenced_messenger_frame, read_messenger_frame,
        read_messenger_frame_with_diagnostics,
    };
    use crate::messenger_hub::{
        EnterClaim, MessengerHubError, MessengerHubLimits, MessengerIdentity, MessengerSessionId,
    };

    const MAXIMUM: usize = 16 * 1_024;
    const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

    struct AdvanceGenerationOnFirstByte {
        frame: Vec<u8>,
        offset: usize,
        generation: Arc<AtomicU64>,
        advanced: bool,
    }

    impl AsyncRead for AdvanceGenerationOnFirstByte {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _context: &mut std::task::Context<'_>,
            buffer: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            let this = self.get_mut();
            let count = buffer
                .remaining()
                .min(this.frame.len().saturating_sub(this.offset));
            if count == 0 {
                return std::task::Poll::Ready(Ok(()));
            }
            buffer.put_slice(&this.frame[this.offset..this.offset + count]);
            this.offset += count;
            if !this.advanced {
                this.generation.store(2, Ordering::Release);
                this.advanced = true;
            }
            std::task::Poll::Ready(Ok(()))
        }
    }

    fn test_config() -> MessengerRuntimeConfig {
        MessengerRuntimeConfig {
            mailbox_capacity: 64,
            max_connections: 16,
            max_identities: 32,
            outbound_capacity: 8,
            max_frame_payload: MAXIMUM,
            max_string_utf16_units: 128,
            enter_timeout: Duration::from_secs(1),
            idle_timeout: Duration::from_secs(2),
            write_timeout: Duration::from_secs(1),
            hub_limits: MessengerHubLimits {
                max_sessions: 16,
                max_rooms: 64,
                max_rooms_per_identity: 16,
                max_message_utf16_units: 128,
            },
        }
    }

    fn identity(user_no: u32, nickname: &str, generation: u64) -> MessengerIdentity {
        MessengerIdentity::new(user_no, nickname, generation, LOOPBACK).unwrap()
    }

    fn enter_packet(user_no: u32, chat_type: u32, nickname: &str) -> Vec<u8> {
        let mut packet = PacketWriter::named("PqEnterChatServer");
        packet.write_u32(user_no);
        packet.write_u32(chat_type);
        packet.write_utf16(nickname).unwrap();
        packet.into_inner()
    }

    #[allow(clippy::similar_names)]
    fn invite_packet(
        inviter_user_no: u32,
        invitee_user_no: u32,
        inviter_nickname: &str,
        invitee_nickname: &str,
    ) -> Vec<u8> {
        let mut packet = PacketWriter::named("PqInitInviteMsgrChat");
        packet.write_u32(inviter_user_no);
        packet.write_u32(invitee_user_no);
        packet.write_utf16(inviter_nickname).unwrap();
        packet.write_utf16(invitee_nickname).unwrap();
        packet.into_inner()
    }

    fn chat_packet(room_id: u32, nickname: &str, message: &str) -> Vec<u8> {
        let mut packet = PacketWriter::named("PqMsgrChat");
        packet.write_u32(room_id);
        packet.write_utf16(nickname).unwrap();
        packet.write_utf16(message).unwrap();
        packet.into_inner()
    }

    fn leave_packet(user_no: u32, room_id: u32) -> Vec<u8> {
        let mut packet = PacketWriter::named("PqLeaveMsgrChat");
        packet.write_u32(user_no);
        packet.write_u32(room_id);
        packet.into_inner()
    }

    fn guild_packet(nickname: &str, message: &str) -> Vec<u8> {
        let mut packet = PacketWriter::named("PqGuildChat");
        packet.write_utf16(nickname).unwrap();
        packet.write_utf16(message).unwrap();
        packet.into_inner()
    }

    async fn send_logical<W>(writer: &mut W, logical: &[u8])
    where
        W: AsyncWrite + Unpin,
    {
        writer
            .write_all(&encode_frame(logical, MAXIMUM).unwrap())
            .await
            .unwrap();
    }

    async fn receive_logical<R>(reader: &mut R) -> Vec<u8>
    where
        R: AsyncRead + Unpin,
    {
        time::timeout(
            Duration::from_secs(1),
            read_messenger_frame(reader, MAXIMUM),
        )
        .await
        .expect("timed out waiting for a messenger frame")
        .unwrap()
    }

    async fn spawn_tcp_connection(
        service: &MessengerServiceHandle,
    ) -> (TcpStream, JoinHandle<Result<(), MessengerConnectionError>>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let connection_service = service.clone();
        let task = tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            connection_service.serve_connection(stream, peer).await
        });
        let client = TcpStream::connect(endpoint).await.unwrap();
        (client, task)
    }

    fn spawn_duplex_connection(
        service: &MessengerServiceHandle,
        capacity: usize,
        port: u16,
    ) -> (
        DuplexStream,
        JoinHandle<Result<(), MessengerConnectionError>>,
    ) {
        let (client, server) = duplex(capacity);
        let connection_service = service.clone();
        let task = tokio::spawn(async move {
            connection_service
                .serve_connection(server, SocketAddr::new(LOOPBACK, port))
                .await
        });
        (client, task)
    }

    async fn wait_for_snapshot(
        service: &MessengerServiceHandle,
        predicate: impl Fn(MessengerServiceSnapshot) -> bool,
    ) -> MessengerServiceSnapshot {
        time::timeout(Duration::from_secs(1), async {
            loop {
                let snapshot = service.snapshot().await.unwrap();
                if predicate(snapshot) {
                    return snapshot;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("messenger state did not converge")
    }

    fn invite_room_id(packet: &[u8]) -> u32 {
        let mut reader = PacketReader::new(packet);
        assert_eq!(
            reader.read_u32().unwrap(),
            adler32::packet_hash("PrInitInviteMsgrChat")
        );
        reader.read_u32().unwrap();
        reader.read_u32().unwrap();
        reader.read_utf16().unwrap();
        reader.read_utf16().unwrap();
        reader.read_u32().unwrap()
    }

    #[tokio::test]
    async fn dropped_registration_replies_are_cleaned_on_both_sides_of_send() {
        let (service, actor_task) = MessengerServiceHandle::spawn(test_config()).unwrap();

        let (cancellation, cancelled_before_send) = oneshot::channel();
        let (outbound, _outbound_receiver) = mpsc::channel(1);
        let (reply, response) = oneshot::channel();
        drop(response);
        service
            .sender
            .send(MessengerCommand::RegisterConnection {
                peer_ip: LOOPBACK,
                cancellation,
                outbound,
                generation: Arc::new(AtomicU64::new(0)),
                reply,
            })
            .await
            .unwrap();
        wait_for_snapshot(&service, |state| state.connections == 0).await;
        assert!(cancelled_before_send.await.is_err());

        let (cancellation, cancelled_after_send) = oneshot::channel();
        let (outbound, _outbound_receiver) = mpsc::channel(1);
        let (reply, response) = oneshot::channel();
        service
            .sender
            .send(MessengerCommand::RegisterConnection {
                peer_ip: LOOPBACK,
                cancellation,
                outbound,
                generation: Arc::new(AtomicU64::new(0)),
                reply,
            })
            .await
            .unwrap();
        assert_eq!(service.snapshot().await.unwrap().connections, 1);
        drop(response);
        wait_for_snapshot(&service, |state| state.connections == 0).await;
        assert!(cancelled_after_send.await.is_err());

        service.shutdown().await.unwrap();
        actor_task.await.unwrap();
    }

    #[test]
    fn cancelled_actor_commit_gate_has_a_single_linearization_winner() {
        let cancelled = Arc::new(MessengerCommitGate::new());
        drop(MessengerCommitLease::new(Arc::clone(&cancelled)));
        assert!(!cancelled.claim());

        let claimed = Arc::new(MessengerCommitGate::new());
        let lease = MessengerCommitLease::new(Arc::clone(&claimed));
        assert!(claimed.claim());
        drop(lease);
        assert!(!claimed.claim());
    }

    #[tokio::test]
    async fn lease_cleanup_is_bounded_and_coalesced_while_mailbox_is_full() {
        let (mailbox, mut mailbox_receiver) = mpsc::channel(1);
        let (reply, _response) = oneshot::channel();
        mailbox
            .try_send(MessengerCommand::Snapshot { reply })
            .unwrap();

        let cleanup = Arc::new(MessengerCleanupQueue::new());
        let first = MessengerSessionId::new(1).unwrap();
        let second = MessengerSessionId::new(2).unwrap();
        drop(MessengerRegistrationLease::new(first, Arc::clone(&cleanup)));
        drop(MessengerRegistrationLease::new(first, Arc::clone(&cleanup)));
        drop(MessengerRegistrationLease::new(
            second,
            Arc::clone(&cleanup),
        ));

        assert_eq!(cleanup.pending_count(), 2);
        time::timeout(Duration::from_secs(1), cleanup.wake.notified())
            .await
            .expect("cleanup wake was lost while the command mailbox was full");
        let pending = cleanup.take_pending();
        assert_eq!(pending.len(), 2);
        assert!(pending.contains(&first));
        assert!(pending.contains(&second));
        assert!(matches!(
            mailbox_receiver.try_recv(),
            Ok(MessengerCommand::Snapshot { .. })
        ));
        assert!(matches!(
            mailbox_receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn fragmented_and_coalesced_enter_and_event_work_over_real_tcp() {
        let (service, actor_task) = MessengerServiceHandle::spawn(test_config()).unwrap();
        service
            .announce_identity(identity(17, "Rider", 1))
            .await
            .unwrap();
        let (mut client, connection_task) = spawn_tcp_connection(&service).await;

        let enter = encode_frame(&enter_packet(17, 2, "Rider"), MAXIMUM).unwrap();
        let guild = encode_frame(&guild_packet("Rider", "coalesced"), MAXIMUM).unwrap();
        client.write_all(&enter[..2]).await.unwrap();
        tokio::task::yield_now().await;
        let mut tail = enter[2..].to_vec();
        tail.extend_from_slice(&guild);
        client.write_all(&tail).await.unwrap();

        assert_eq!(
            receive_logical(&mut client).await,
            serialize_guild_chat("Rider", "coalesced").unwrap()
        );
        let snapshot = wait_for_snapshot(&service, |state| state.entered_sessions == 1).await;
        assert_eq!(snapshot.connections, 1);

        service.shutdown().await.unwrap();
        assert!(matches!(
            connection_task.await.unwrap(),
            Err(MessengerConnectionError::Cancelled(
                MessengerCancellation::Shutdown
            ))
        ));
        actor_task.await.unwrap();
    }

    #[tokio::test]
    async fn generation_change_during_first_byte_admission_fails_closed() {
        let generation = Arc::new(AtomicU64::new(1));
        let mut reader = AdvanceGenerationOnFirstByte {
            frame: encode_frame(&guild_packet("Rider", "stale-prefix"), MAXIMUM).unwrap(),
            offset: 0,
            generation: Arc::clone(&generation),
            advanced: false,
        };

        assert!(matches!(
            read_generation_fenced_messenger_frame(&mut reader, MAXIMUM, generation.as_ref(), None)
                .await,
            Err(MessengerConnectionError::Service(
                super::MessengerServiceError::StaleIdentityGeneration
            ))
        ));
        assert_eq!(generation.load(Ordering::Acquire), 2);
        assert_eq!(reader.offset, 1);
    }

    #[tokio::test]
    async fn diagnostic_reader_keeps_partial_messenger_frame_failure_typed() {
        let (mut writer, mut reader) = duplex(16);
        let frame = encode_frame(&[1, 2, 3, 4], MAXIMUM).unwrap();
        writer
            .write_all(&frame[..=MESSENGER_FRAME_HEADER_LENGTH])
            .await
            .unwrap();
        writer.shutdown().await.unwrap();

        let error = read_messenger_frame_with_diagnostics(
            &mut reader,
            MAXIMUM,
            SocketAddr::from((Ipv4Addr::LOCALHOST, 39_313)),
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            MessengerConnectionError::Io(ref source)
                if source.kind() == std::io::ErrorKind::UnexpectedEof
        ));
    }

    #[tokio::test]
    async fn two_clients_exchange_exact_invite_chat_leave_and_guild_frames() {
        let (service, actor_task) = MessengerServiceHandle::spawn(test_config()).unwrap();
        service
            .announce_identity(identity(17, "Rider", 1))
            .await
            .unwrap();
        service
            .announce_identity(identity(18, "Peer", 1))
            .await
            .unwrap();
        let (mut rider, rider_task) = spawn_tcp_connection(&service).await;
        let (mut peer, peer_task) = spawn_tcp_connection(&service).await;
        send_logical(&mut rider, &enter_packet(17, 1, "Rider")).await;
        send_logical(&mut peer, &enter_packet(18, 1, "Peer")).await;
        wait_for_snapshot(&service, |state| state.entered_sessions == 2).await;

        send_logical(&mut rider, &invite_packet(17, 18, "Rider", "Peer")).await;
        let rider_invite = receive_logical(&mut rider).await;
        let peer_invite = receive_logical(&mut peer).await;
        assert_eq!(rider_invite, peer_invite);
        let room_id = invite_room_id(&rider_invite);
        assert_ne!(room_id, 0);
        assert_eq!(
            rider_invite,
            serialize_invite_chat(17, 18, "Rider", "Peer", room_id).unwrap()
        );

        send_logical(&mut rider, &chat_packet(room_id, "Rider", "hello")).await;
        let expected_chat = serialize_chat(room_id, 17, "Rider", "hello").unwrap();
        assert_eq!(receive_logical(&mut rider).await, expected_chat);
        assert_eq!(receive_logical(&mut peer).await, expected_chat);

        send_logical(&mut rider, &leave_packet(17, room_id)).await;
        assert_eq!(
            receive_logical(&mut peer).await,
            serialize_leave_chat(17, room_id)
        );
        let mut unexpected = [0_u8; 1];
        assert!(
            time::timeout(Duration::from_millis(30), rider.read_exact(&mut unexpected))
                .await
                .is_err(),
            "the C# leave fan-out excludes the sender"
        );

        send_logical(&mut peer, &guild_packet("Peer", "all")).await;
        let expected_guild = serialize_guild_chat("Peer", "all").unwrap();
        assert_eq!(receive_logical(&mut rider).await, expected_guild);
        assert_eq!(receive_logical(&mut peer).await, expected_guild);
        assert_eq!(service.snapshot().await.unwrap().rooms, 1);

        service.shutdown().await.unwrap();
        assert!(matches!(
            rider_task.await.unwrap(),
            Err(MessengerConnectionError::Cancelled(
                MessengerCancellation::Shutdown
            ))
        ));
        assert!(matches!(
            peer_task.await.unwrap(),
            Err(MessengerConnectionError::Cancelled(
                MessengerCancellation::Shutdown
            ))
        ));
        actor_task.await.unwrap();
    }

    #[tokio::test]
    async fn spoofed_sender_closes_only_that_endpoint() {
        let (service, actor_task) = MessengerServiceHandle::spawn(test_config()).unwrap();
        service
            .announce_identity(identity(17, "Rider", 1))
            .await
            .unwrap();
        service
            .announce_identity(identity(18, "Peer", 1))
            .await
            .unwrap();
        let (mut rider, rider_task) = spawn_tcp_connection(&service).await;
        send_logical(&mut rider, &enter_packet(17, 0, "Rider")).await;
        wait_for_snapshot(&service, |state| state.entered_sessions == 1).await;

        send_logical(&mut rider, &guild_packet("Peer", "spoof")).await;
        assert!(matches!(
            rider_task.await.unwrap(),
            Err(MessengerConnectionError::Service(
                super::MessengerServiceError::Hub(MessengerHubError::SenderNicknameMismatch)
            ))
        ));
        let snapshot = wait_for_snapshot(&service, |state| state.connections == 0).await;
        assert_eq!(snapshot.entered_sessions, 0);
        assert_eq!(snapshot.announced_identities, 2);

        service.shutdown().await.unwrap();
        actor_task.await.unwrap();
    }

    #[tokio::test]
    async fn duplicate_replacement_and_late_close_are_generation_fenced() {
        let (service, actor_task) = MessengerServiceHandle::spawn(test_config()).unwrap();
        let rider_v1 = identity(17, "Rider", 1);
        service.announce_identity(rider_v1.clone()).await.unwrap();

        let (mut old, old_task) = spawn_tcp_connection(&service).await;
        send_logical(&mut old, &enter_packet(17, 1, "Rider")).await;
        wait_for_snapshot(&service, |state| state.entered_sessions == 1).await;

        let (mut replacement, replacement_task) = spawn_tcp_connection(&service).await;
        send_logical(&mut replacement, &enter_packet(17, 2, "Rider")).await;
        assert!(matches!(
            old_task.await.unwrap(),
            Err(MessengerConnectionError::Cancelled(
                MessengerCancellation::Replaced
            ))
        ));
        let snapshot = wait_for_snapshot(&service, |state| {
            state.connections == 1 && state.entered_sessions == 1
        })
        .await;
        assert_eq!(snapshot.rooms, 0);

        let rider_v2 = identity(17, "Rider", 2);
        let advanced = service
            .advance_identity(rider_v1.clone(), rider_v2.clone())
            .await
            .unwrap();
        assert!(advanced.applied);
        assert!(advanced.hub.endpoint_updated);
        assert!(advanced.retained_session.is_some());
        assert_eq!(advanced.cancelled_session, None);

        let retried = service
            .advance_identity(rider_v1.clone(), rider_v2.clone())
            .await
            .unwrap();
        assert!(!retried.applied);
        assert!(!retried.hub.endpoint_updated);
        assert_eq!(retried.hub.room_members_updated, 0);
        assert_eq!(retried.retained_session, advanced.retained_session);
        assert_eq!(retried.cancelled_session, None);

        let stale_release = service.release_identity(rider_v1).await.unwrap();
        assert!(!stale_release.applied);
        send_logical(&mut replacement, &guild_packet("Rider", "still-current")).await;
        assert_eq!(
            receive_logical(&mut replacement).await,
            serialize_guild_chat("Rider", "still-current").unwrap()
        );

        let release = service.release_identity(rider_v2).await.unwrap();
        assert!(release.applied);
        assert_eq!(release.cancelled_session, advanced.retained_session);
        assert!(matches!(
            replacement_task.await.unwrap(),
            Err(MessengerConnectionError::Cancelled(
                MessengerCancellation::IdentityReleased
            ))
        ));
        let snapshot = wait_for_snapshot(&service, |state| state.connections == 0).await;
        assert_eq!(snapshot.entered_sessions, 0);
        assert_eq!(snapshot.announced_identities, 0);

        service.shutdown().await.unwrap();
        actor_task.await.unwrap();
    }

    #[tokio::test]
    async fn non_enter_first_frame_and_duplicate_enter_are_rejected() {
        let (service, actor_task) = MessengerServiceHandle::spawn(test_config()).unwrap();
        service
            .announce_identity(identity(17, "Rider", 1))
            .await
            .unwrap();

        let (mut wrong_first, wrong_first_task) = spawn_duplex_connection(&service, 4_096, 40_001);
        send_logical(&mut wrong_first, &guild_packet("Rider", "too-early")).await;
        assert!(matches!(
            wrong_first_task.await.unwrap(),
            Err(MessengerConnectionError::FirstFrameWasNotEnter)
        ));
        wait_for_snapshot(&service, |state| state.connections == 0).await;

        let (mut duplicate, duplicate_task) = spawn_duplex_connection(&service, 4_096, 40_002);
        let first = encode_frame(&enter_packet(17, 0, "Rider"), MAXIMUM).unwrap();
        let mut coalesced = first.clone();
        coalesced.extend_from_slice(&first);
        duplicate.write_all(&coalesced).await.unwrap();
        assert!(matches!(
            duplicate_task.await.unwrap(),
            Err(MessengerConnectionError::DuplicateEnter)
        ));
        let snapshot = wait_for_snapshot(&service, |state| state.connections == 0).await;
        assert_eq!(snapshot.entered_sessions, 0);

        service.shutdown().await.unwrap();
        actor_task.await.unwrap();
    }

    #[tokio::test]
    async fn full_slow_queue_evicts_only_the_slow_endpoint() {
        let mut config = test_config();
        config.outbound_capacity = 1;
        config.write_timeout = Duration::from_secs(2);
        let (service, actor_task) = MessengerServiceHandle::spawn(config).unwrap();
        service
            .announce_identity(identity(17, "Slow", 1))
            .await
            .unwrap();
        service
            .announce_identity(identity(18, "Sender", 1))
            .await
            .unwrap();

        let (mut slow, slow_task) = spawn_duplex_connection(&service, 1, 41_001);
        send_logical(&mut slow, &enter_packet(17, 0, "Slow")).await;
        let (mut sender, sender_task) = spawn_duplex_connection(&service, 4_096, 41_002);
        send_logical(&mut sender, &enter_packet(18, 0, "Sender")).await;
        wait_for_snapshot(&service, |state| state.entered_sessions == 2).await;

        send_logical(&mut sender, &guild_packet("Sender", "one")).await;
        assert_eq!(
            receive_logical(&mut sender).await,
            serialize_guild_chat("Sender", "one").unwrap()
        );
        let mut first_wire_byte = [0_u8; 1];
        time::timeout(
            Duration::from_secs(1),
            slow.read_exact(&mut first_wire_byte),
        )
        .await
        .expect("slow writer never started")
        .unwrap();

        send_logical(&mut sender, &guild_packet("Sender", "two")).await;
        assert_eq!(
            receive_logical(&mut sender).await,
            serialize_guild_chat("Sender", "two").unwrap()
        );
        send_logical(&mut sender, &guild_packet("Sender", "three")).await;
        assert_eq!(
            receive_logical(&mut sender).await,
            serialize_guild_chat("Sender", "three").unwrap()
        );

        let snapshot = wait_for_snapshot(&service, |state| {
            state.connections == 1 && state.entered_sessions == 1
        })
        .await;
        assert_eq!(snapshot.announced_identities, 2);
        assert!(matches!(
            slow_task.await.unwrap(),
            Err(MessengerConnectionError::Cancelled(
                MessengerCancellation::Backpressure
            ))
        ));

        service.shutdown().await.unwrap();
        assert!(matches!(
            sender_task.await.unwrap(),
            Err(MessengerConnectionError::Cancelled(
                MessengerCancellation::Shutdown
            ))
        ));
        actor_task.await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_interrupts_a_partial_enter_frame() {
        let (service, actor_task) = MessengerServiceHandle::spawn(test_config()).unwrap();
        let (mut client, connection_task) = spawn_duplex_connection(&service, 64, 42_001);
        client.write_all(&16_i32.to_le_bytes()[..2]).await.unwrap();
        wait_for_snapshot(&service, |state| state.connections == 1).await;

        service.shutdown().await.unwrap();
        assert!(matches!(
            time::timeout(Duration::from_secs(1), connection_task)
                .await
                .expect("partial reader ignored shutdown")
                .unwrap(),
            Err(MessengerConnectionError::Cancelled(
                MessengerCancellation::Shutdown
            ))
        ));
        actor_task.await.unwrap();
    }

    #[tokio::test]
    async fn abrupt_actor_stop_is_distinct_from_orderly_shutdown() {
        let (service, actor_task) = MessengerServiceHandle::spawn(test_config()).unwrap();
        service
            .announce_identity(identity(17, "Rider", 1))
            .await
            .unwrap();
        let (mut client, connection_task) = spawn_duplex_connection(&service, 64, 42_002);
        send_logical(&mut client, &enter_packet(17, 0, "Rider")).await;
        wait_for_snapshot(&service, |state| state.entered_sessions == 1).await;

        actor_task.abort();
        assert!(actor_task.await.unwrap_err().is_cancelled());
        assert!(matches!(
            time::timeout(Duration::from_secs(1), connection_task)
                .await
                .expect("connection did not observe the actor stopping")
                .unwrap(),
            Err(MessengerConnectionError::ActorStopped)
        ));
        assert!(matches!(
            service.shutdown().await,
            Err(super::MessengerServiceError::Stopped)
        ));
    }

    #[tokio::test]
    async fn signed_length_maximum_and_connection_timeouts_are_enforced() {
        let mut config = test_config();
        config.max_frame_payload = 32;
        config.enter_timeout = Duration::from_millis(50);
        config.idle_timeout = Duration::from_millis(50);
        let (service, actor_task) = MessengerServiceHandle::spawn(config).unwrap();

        let (mut negative, negative_task) = spawn_duplex_connection(&service, 64, 43_001);
        negative.write_all(&(-1_i32).to_le_bytes()).await.unwrap();
        assert!(matches!(
            negative_task.await.unwrap(),
            Err(MessengerConnectionError::Frame(
                MessengerFrameError::NegativePayloadLength(-1)
            ))
        ));

        let (mut oversized, oversized_task) = spawn_duplex_connection(&service, 64, 43_002);
        oversized.write_all(&33_i32.to_le_bytes()).await.unwrap();
        assert!(matches!(
            oversized_task.await.unwrap(),
            Err(MessengerConnectionError::Frame(
                MessengerFrameError::PayloadTooLarge {
                    length: 33,
                    maximum: 32
                }
            ))
        ));

        let (_silent, timeout_task) = spawn_duplex_connection(&service, 64, 43_003);
        assert!(matches!(
            timeout_task.await.unwrap(),
            Err(MessengerConnectionError::EnterTimeout)
        ));

        service
            .announce_identity(identity(17, "Rider", 1))
            .await
            .unwrap();
        let (mut idle, idle_task) = spawn_duplex_connection(&service, 64, 43_004);
        send_logical(&mut idle, &enter_packet(17, 0, "Rider")).await;
        assert!(matches!(
            idle_task.await.unwrap(),
            Err(MessengerConnectionError::IdleTimeout)
        ));
        wait_for_snapshot(&service, |state| state.connections == 0).await;

        service.shutdown().await.unwrap();
        actor_task.await.unwrap();
    }

    #[tokio::test]
    async fn identity_announcements_do_not_consume_the_hub_session_cap() {
        let mut config = test_config();
        config.hub_limits.max_sessions = 1;
        let (service, actor_task) = MessengerServiceHandle::spawn(config).unwrap();
        for (user_no, nickname) in [(17, "One"), (18, "Two"), (19, "Three")] {
            service
                .announce_identity(identity(user_no, nickname, 1))
                .await
                .unwrap();
        }
        assert_eq!(
            service.snapshot().await.unwrap(),
            MessengerServiceSnapshot {
                announced_identities: 3,
                connections: 0,
                entered_sessions: 0,
                rooms: 0,
            }
        );
        assert!(MessengerSessionId::new(0).is_none());

        service.shutdown().await.unwrap();
        actor_task.await.unwrap();
    }

    #[tokio::test]
    async fn identity_mirror_and_pre_actor_connection_admission_are_bounded() {
        let mut config = test_config();
        config.max_identities = 1;
        config.max_connections = 1;
        let (service, actor_task) = MessengerServiceHandle::spawn(config).unwrap();
        let first = identity(17, "One", 1);
        service.announce_identity(first.clone()).await.unwrap();
        service.announce_identity(first).await.unwrap();
        assert!(matches!(
            service.announce_identity(identity(18, "Two", 1)).await,
            Err(super::MessengerServiceError::IdentityLimitReached { maximum: 1 })
        ));

        let (_first_client, first_task) = spawn_duplex_connection(&service, 64, 44_001);
        wait_for_snapshot(&service, |state| state.connections == 1).await;
        let (_second_client, second_server) = duplex(64);
        assert!(matches!(
            service
                .serve_connection(second_server, SocketAddr::new(LOOPBACK, 44_002))
                .await,
            Err(MessengerConnectionError::Service(
                super::MessengerServiceError::ConnectionLimitReached { maximum: 1 }
            ))
        ));

        service.shutdown().await.unwrap();
        assert!(matches!(
            first_task.await.unwrap(),
            Err(MessengerConnectionError::Cancelled(
                MessengerCancellation::Shutdown
            ))
        ));
        actor_task.await.unwrap();
    }

    #[tokio::test]
    async fn expired_enter_cannot_replace_a_healthy_endpoint() {
        let (service, actor_task) = MessengerServiceHandle::spawn(test_config()).unwrap();
        service
            .announce_identity(identity(17, "Rider", 1))
            .await
            .unwrap();
        let (mut healthy, healthy_task) = spawn_tcp_connection(&service).await;
        send_logical(&mut healthy, &enter_packet(17, 1, "Rider")).await;
        wait_for_snapshot(&service, |state| state.entered_sessions == 1).await;

        let expired = service.register_connection(LOOPBACK).await.unwrap();
        let expired_session = expired.registration.lease.session;
        assert!(matches!(
            service
                .enter(
                    expired_session,
                    EnterClaim {
                        user_no: 17,
                        chat_type: 2,
                        nickname: "Rider".to_owned(),
                    },
                    time::Instant::now(),
                )
                .await,
            Err(super::MessengerServiceError::EnterDeadlineElapsed)
        ));
        expired.registration.close().await.unwrap();

        send_logical(&mut healthy, &guild_packet("Rider", "still-healthy")).await;
        assert_eq!(
            receive_logical(&mut healthy).await,
            serialize_guild_chat("Rider", "still-healthy").unwrap()
        );
        service.shutdown().await.unwrap();
        assert!(matches!(
            healthy_task.await.unwrap(),
            Err(MessengerConnectionError::Cancelled(
                MessengerCancellation::Shutdown
            ))
        ));
        actor_task.await.unwrap();
    }

    #[tokio::test]
    async fn generation_retry_requires_the_exact_recorded_transition() {
        let (service, actor_task) = MessengerServiceHandle::spawn(test_config()).unwrap();
        let v1 = identity(17, "Rider", 1);
        let v2 = identity(17, "Rider", 2);
        let v3 = identity(17, "Rider", 3);

        service.announce_identity(v2.clone()).await.unwrap();
        assert!(matches!(
            service.advance_identity(v1.clone(), v2.clone()).await,
            Err(super::MessengerServiceError::StaleIdentityGeneration)
        ));
        service.release_identity(v2.clone()).await.unwrap();

        service.announce_identity(v1.clone()).await.unwrap();
        service
            .advance_identity(v1.clone(), v2.clone())
            .await
            .unwrap();
        service
            .advance_identity(v2.clone(), v3.clone())
            .await
            .unwrap();
        assert!(matches!(
            service.advance_identity(v1, v3).await,
            Err(super::MessengerServiceError::StaleIdentityGeneration)
        ));

        service.release_identity(v2).await.unwrap();
        assert_eq!(service.snapshot().await.unwrap().announced_identities, 1);
        service.shutdown().await.unwrap();
        actor_task.await.unwrap();
    }

    #[tokio::test]
    async fn frame_admitted_before_generation_advance_cannot_dispatch_as_the_replacement() {
        let (service, actor_task) = MessengerServiceHandle::spawn(test_config()).unwrap();
        let v1 = identity(17, "Rider", 1);
        let v2 = identity(17, "Rider", 2);
        service.announce_identity(v1.clone()).await.unwrap();
        let registered = service.register_connection(LOOPBACK).await.unwrap();
        let session = registered.registration.lease.session;
        service
            .enter(
                session,
                EnterClaim {
                    user_no: 17,
                    chat_type: 1,
                    nickname: "Rider".to_owned(),
                },
                time::Instant::now() + Duration::from_secs(1),
            )
            .await
            .unwrap();
        let super::RegisteredConnection {
            registration,
            generation,
            cancellation: _cancellation,
            mut outbound,
        } = registered;
        assert_eq!(generation.load(std::sync::atomic::Ordering::Acquire), 1);

        service.advance_identity(v1, v2.clone()).await.unwrap();
        assert_eq!(generation.load(std::sync::atomic::Ordering::Acquire), 2);
        let stale_request = parse_request(&guild_packet("Rider", "stale"), MAXIMUM).unwrap();
        assert!(matches!(
            service.dispatch(session, 1, stale_request).await,
            Err(super::MessengerServiceError::StaleIdentityGeneration)
        ));
        assert!(outbound.try_recv().is_err());

        let fresh_request = parse_request(&guild_packet("Rider", "fresh"), MAXIMUM).unwrap();
        service.dispatch(session, 2, fresh_request).await.unwrap();
        assert!(outbound.recv().await.is_some());

        registration.close().await.unwrap();
        service.release_identity(v2).await.unwrap();
        service.shutdown().await.unwrap();
        actor_task.await.unwrap();
    }

    #[tokio::test]
    async fn concurrent_and_repeated_shutdown_is_idempotent() {
        let (service, actor_task) = MessengerServiceHandle::spawn(test_config()).unwrap();
        let first = service.clone();
        let second = service.clone();
        let (first_result, second_result) =
            tokio::join!(async move { first.shutdown().await }, async move {
                second.shutdown().await
            });
        first_result.unwrap();
        second_result.unwrap();
        actor_task.await.unwrap();
        service.shutdown().await.unwrap();
    }
}
