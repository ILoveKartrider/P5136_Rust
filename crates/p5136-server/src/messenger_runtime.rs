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
    sync::{Arc, Mutex},
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
    sync::{Notify, mpsc, oneshot},
    task::JoinHandle,
    time,
};

use crate::messenger_hub::{
    ChatClaim, EnterClaim, GenerationAdvance, GuildChatClaim, IdentityRelease, InviteClaim,
    LeaveClaim, MessengerDelivery, MessengerEvent, MessengerHub, MessengerHubError,
    MessengerHubLimits, MessengerIdentity, MessengerRoomId, MessengerSessionId,
};

pub const DEFAULT_MESSENGER_MAILBOX_CAPACITY: usize = 1_024;
pub const DEFAULT_MESSENGER_CONNECTION_CAPACITY: usize = 256;
pub const DEFAULT_MESSENGER_OUTBOUND_CAPACITY: usize = 64;
pub const DEFAULT_MAX_MESSENGER_PAYLOAD: usize = 64 * 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessengerRuntimeConfig {
    pub mailbox_capacity: usize,
    pub max_connections: usize,
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
        reply: oneshot::Sender<Result<MessengerRegistrationLease, MessengerServiceError>>,
    },
    Enter {
        session: MessengerSessionId,
        claim: EnterClaim,
        reply: oneshot::Sender<Result<(), MessengerServiceError>>,
    },
    Dispatch {
        session: MessengerSessionId,
        request: MessengerRequest,
        reply: oneshot::Sender<Result<(), MessengerServiceError>>,
    },
    Disconnect {
        session: MessengerSessionId,
    },
    Snapshot {
        reply: oneshot::Sender<MessengerServiceSnapshot>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

#[derive(Debug)]
struct MessengerTransport {
    peer_ip: IpAddr,
    entered_key: Option<String>,
    cancellation: Option<oneshot::Sender<MessengerCancellation>>,
    outbound: mpsc::Sender<Arc<[u8]>>,
}

#[derive(Debug)]
struct MessengerActor {
    config: Arc<MessengerRuntimeConfig>,
    hub: MessengerHub,
    identities_by_key: HashMap<String, MessengerIdentity>,
    identity_key_by_user_no: HashMap<NonZeroU32, String>,
    transports: HashMap<MessengerSessionId, MessengerTransport>,
    next_session_id: Option<MessengerSessionId>,
}

#[derive(Debug)]
struct MessengerCleanupQueue {
    pending: Mutex<HashSet<MessengerSessionId>>,
    wake: Notify,
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
        let handle = Self {
            sender,
            config: Arc::clone(&config),
        };
        let actor = MessengerActor {
            config,
            hub,
            identities_by_key: HashMap::new(),
            identity_key_by_user_no: HashMap::new(),
            transports: HashMap::new(),
            next_session_id: MessengerSessionId::new(1),
        };
        let task = tokio::spawn(run_messenger_actor(actor, receiver, cleanup));
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
        let registered = self.register_connection(peer.ip()).await?;
        let session = registered.registration.lease.session;
        let result = run_registered_connection(
            stream,
            session,
            self,
            registered.cancellation,
            registered.outbound,
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
        let (reply, response) = oneshot::channel();
        self.sender
            .send(MessengerCommand::Shutdown { reply })
            .await
            .map_err(|_| MessengerServiceError::Stopped)?;
        response.await.map_err(|_| MessengerServiceError::Stopped)
    }

    async fn register_connection(
        &self,
        peer_ip: IpAddr,
    ) -> Result<RegisteredConnection, MessengerServiceError> {
        let (cancellation, cancelled) = oneshot::channel();
        let (outbound, outbound_receiver) = mpsc::channel(self.config.outbound_capacity);
        let (reply, response) = oneshot::channel();
        self.sender
            .send(MessengerCommand::RegisterConnection {
                peer_ip,
                cancellation,
                outbound,
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
            cancellation: cancelled,
            outbound: outbound_receiver,
        })
    }

    async fn enter(
        &self,
        session: MessengerSessionId,
        claim: EnterClaim,
    ) -> Result<(), MessengerServiceError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(MessengerCommand::Enter {
                session,
                claim,
                reply,
            })
            .await
            .map_err(|_| MessengerServiceError::Stopped)?;
        response.await.map_err(|_| MessengerServiceError::Stopped)?
    }

    async fn dispatch(
        &self,
        session: MessengerSessionId,
        request: MessengerRequest,
    ) -> Result<(), MessengerServiceError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(MessengerCommand::Dispatch {
                session,
                request,
                reply,
            })
            .await
            .map_err(|_| MessengerServiceError::Stopped)?;
        response.await.map_err(|_| MessengerServiceError::Stopped)?
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
        self.identity_key_by_user_no
            .insert(identity.user_no, key.clone());
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
            return Ok(MessengerGenerationAdvanceOutcome {
                applied: false,
                hub: GenerationAdvance {
                    endpoint_updated: false,
                    room_members_updated: 0,
                },
                retained_session: self.hub.session_for_identity(&next.nickname),
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
        self.identities_by_key.insert(key.to_owned(), next);
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
    ) -> Result<(), MessengerServiceError> {
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
        request: MessengerRequest,
    ) -> Result<(), MessengerServiceError> {
        let sender = self.current_identity(session)?;
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
) {
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
                reply,
            } => {
                let registration = actor
                    .register_connection(peer_ip, cancellation, outbound)
                    .map(|session| MessengerRegistrationLease::new(session, Arc::clone(&cleanup)));
                let _ = reply.send(registration);
                false
            }
            MessengerCommand::Enter {
                session,
                claim,
                reply,
            } => {
                let _ = reply.send(actor.enter(session, &claim));
                false
            }
            MessengerCommand::Dispatch {
                session,
                request,
                reply,
            } => {
                let _ = reply.send(actor.dispatch(session, request));
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
            MessengerCommand::Shutdown { reply } => {
                actor.shutdown();
                let _ = reply.send(());
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

async fn run_registered_connection<S>(
    stream: S,
    session: MessengerSessionId,
    service: &MessengerServiceHandle,
    mut cancellation: oneshot::Receiver<MessengerCancellation>,
    mut outbound: mpsc::Receiver<Arc<[u8]>>,
) -> Result<(), MessengerConnectionError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let config = Arc::clone(&service.config);
    let (mut reader, mut writer) = tokio::io::split(stream);
    let bootstrap = async {
        let logical = read_messenger_frame(&mut reader, config.max_frame_payload).await?;
        let request = parse_request(&logical, config.max_string_utf16_units)?;
        let MessengerRequest::EnterChatServer {
            user_no,
            chat_type,
            nickname,
        } = request
        else {
            return Err(MessengerConnectionError::FirstFrameWasNotEnter);
        };
        service
            .enter(
                session,
                EnterClaim {
                    user_no,
                    chat_type,
                    nickname,
                },
            )
            .await?;
        Ok(())
    };
    tokio::select! {
        biased;
        cancelled = &mut cancellation => {
            return Err(cancellation_error(&cancelled));
        }
        result = time::timeout(config.enter_timeout, bootstrap) => {
            result.map_err(|_| MessengerConnectionError::EnterTimeout)??;
        }
    }

    loop {
        let read = time::timeout(
            config.idle_timeout,
            read_messenger_frame(&mut reader, config.max_frame_payload),
        );
        tokio::pin!(read);
        let logical = loop {
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
                    )
                    .await?;
                }
                FrameOrOutbound::Outbound(None) => {
                    return Err(MessengerConnectionError::OutboundClosed);
                }
            }
        };

        let request = parse_request(&logical, config.max_string_utf16_units)?;
        if matches!(request, MessengerRequest::EnterChatServer { .. }) {
            return Err(MessengerConnectionError::DuplicateEnter);
        }
        tokio::select! {
            biased;
            cancelled = &mut cancellation => {
                return Err(cancellation_error(&cancelled));
            }
            result = service.dispatch(session, request) => result?,
        }
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
) -> Result<(), MessengerConnectionError>
where
    W: AsyncWrite + Unpin,
{
    tokio::select! {
        biased;
        cancelled = cancellation => Err(cancellation_error(&cancelled)),
        result = time::timeout(timeout, writer.write_all(frame)) => {
            result
                .map_err(|_| MessengerConnectionError::WriteTimeout)?
                .map_err(MessengerConnectionError::Io)
        }
    }
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
        sync::Arc,
        time::Duration,
    };

    use p5136_core::{
        adler32,
        messenger::{
            MessengerFrameError, encode_frame, serialize_chat, serialize_guild_chat,
            serialize_invite_chat, serialize_leave_chat,
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
        MessengerCancellation, MessengerCleanupQueue, MessengerCommand, MessengerConnectionError,
        MessengerRegistrationLease, MessengerRuntimeConfig, MessengerServiceHandle,
        MessengerServiceSnapshot, read_messenger_frame,
    };
    use crate::messenger_hub::{
        MessengerHubError, MessengerHubLimits, MessengerIdentity, MessengerSessionId,
    };

    const MAXIMUM: usize = 16 * 1_024;
    const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

    fn test_config() -> MessengerRuntimeConfig {
        MessengerRuntimeConfig {
            mailbox_capacity: 64,
            max_connections: 16,
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
}
