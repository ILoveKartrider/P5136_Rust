//! Standalone modern-P5136 UDP socket and dispatch service.
//!
//! Reader tasks perform datagram decryption and logical packet parsing before
//! they admit an owned [`UdpIngress`] into the bounded queue. The service
//! caller then resolves the exact active [`IdentityBinding`] and, for
//! `GameSlotPacket`, the current racing-room audience before submitting a
//! bounded actor command. Endpoint mutation and generation fencing happen only
//! inside that actor.

use std::{
    collections::{HashMap, HashSet},
    io,
    net::SocketAddr,
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use p5136_core::{
    datagram::{DEFAULT_MAX_DATAGRAM_PAYLOAD, DatagramError, decode_datagram, encode_datagram},
    udp_protocol::{
        PqUdpEchoBody, PqUdpTimeSyncBody, PrUdpEchoBody, PrUdpTimeSyncBody, RoutedUdpPacket,
        UdpLogicalBody, UdpProtocolError, encode_routed_udp_packet, parse_routed_udp_packet,
    },
};
use thiserror::Error;
use tokio::{
    net::UdpSocket,
    sync::{mpsc, oneshot},
    task::{AbortHandle, Id as TaskId, JoinError, JoinSet},
};

use crate::{
    IdentityBinding, ReleasedIdentity,
    udp_state::{
        CurrentUdpEndpoint, UdpEndpointBindStatus, UdpEndpointState, UdpEndpointStateError,
        UdpTransport,
    },
};

pub const DEFAULT_UDP_ADMISSION_CAPACITY: usize = 256;
pub const DEFAULT_UDP_COMMAND_CAPACITY: usize = 256;
pub const DEFAULT_MAX_RELAY_TARGETS: usize = 16;
pub const DEFAULT_MAX_ACTIVE_UDP_IDENTITIES: usize = 256;
const MAX_UDP_RECEIVE_DATAGRAM_SIZE: usize = 65_536;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UdpRuntimeConfig {
    pub maximum_payload: usize,
    pub admission_capacity: usize,
    pub command_capacity: usize,
    pub maximum_relay_targets: usize,
    pub maximum_active_identities: usize,
}

impl Default for UdpRuntimeConfig {
    fn default() -> Self {
        Self {
            maximum_payload: DEFAULT_MAX_DATAGRAM_PAYLOAD,
            admission_capacity: DEFAULT_UDP_ADMISSION_CAPACITY,
            command_capacity: DEFAULT_UDP_COMMAND_CAPACITY,
            maximum_relay_targets: DEFAULT_MAX_RELAY_TARGETS,
            maximum_active_identities: DEFAULT_MAX_ACTIVE_UDP_IDENTITIES,
        }
    }
}

/// One process-wide monotonic clock domain shared by UDP time-sync and race
/// control packets. A bound server creates this once and passes clones to every
/// protocol component that emits P5136 ticks.
#[derive(Debug, Clone)]
pub struct ServerClock {
    epoch: Instant,
}

impl Default for ServerClock {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerClock {
    #[must_use]
    pub fn new() -> Self {
        Self {
            epoch: Instant::now(),
        }
    }

    #[must_use]
    pub const fn from_epoch(epoch: Instant) -> Self {
        Self { epoch }
    }

    #[must_use]
    pub fn tick(&self) -> u32 {
        p5136_tick_from_elapsed_millis(self.epoch.elapsed().as_millis())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UdpRuntimeEndpoints {
    pub game: SocketAddr,
    pub p2p: SocketAddr,
}

#[derive(Debug, Error)]
pub enum UdpRuntimeStartError {
    #[error("{queue} UDP queue capacity must be non-zero")]
    ZeroQueueCapacity { queue: &'static str },

    #[error("maximum UDP relay target count must be non-zero")]
    ZeroRelayTargetLimit,

    #[error("maximum active UDP identity count must be non-zero")]
    ZeroActiveIdentityLimit,

    #[error(
        "configured UDP logical payload maximum {configured} exceeds protocol maximum {maximum}"
    )]
    PayloadMaximumTooLarge { configured: usize, maximum: usize },

    #[error("could not inspect bound {transport} socket: {source}")]
    LocalEndpoint {
        transport: UdpTransport,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum UdpIngressDecodeError {
    #[error(transparent)]
    Datagram(#[from] DatagramError),

    #[error(transparent)]
    Protocol(#[from] UdpProtocolError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UdpIngressBody {
    PqUdpEcho(PqUdpEchoBody),
    PrUdpEcho(PrUdpEchoBody),
    PqUdpTimeSync(PqUdpTimeSyncBody),
    PrUdpTimeSync(PrUdpTimeSyncBody),
    GameSlotPacket(Vec<u8>),
    RoomSlotPacket(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UdpIngress {
    pub transport: UdpTransport,
    pub source: SocketAddr,
    pub iv: u32,
    pub account_id: u32,
    pub route_hash: u32,
    pub body: UdpIngressBody,
}

/// Identifies one of the fixed-size set of tasks owned by [`UdpRuntime`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UdpRuntimeTask {
    Reader(UdpTransport),
    Actor,
    /// Defensive fallback if Tokio reports a task identifier that was not
    /// present in the runtime's supervision table.
    Untracked,
}

/// A terminal failure of a UDP runtime task.
#[derive(Debug, Error)]
pub enum UdpRuntimeFailure {
    #[error("{transport} UDP reader stopped after a socket error: {source}")]
    ReaderSocket {
        transport: UdpTransport,
        #[source]
        source: io::Error,
    },

    #[error("{transport} UDP reader lost its admission queue")]
    ReaderAdmissionClosed { transport: UdpTransport },

    #[error("UDP actor command queue closed without an explicit shutdown")]
    ActorCommandChannelClosed,

    #[error("{task:?} UDP runtime task panicked")]
    TaskPanicked { task: UdpRuntimeTask },

    #[error("{task:?} UDP runtime task was cancelled unexpectedly")]
    TaskCancelled { task: UdpRuntimeTask },

    #[error("{task:?} UDP runtime task could not be joined")]
    TaskJoinFailed { task: UdpRuntimeTask },
}

/// The unified stream consumed by the central server supervisor.
#[derive(Debug)]
pub enum UdpRuntimeEvent {
    Ingress(UdpIngress),
    Fatal(UdpRuntimeFailure),
}

/// Decrypts, checksum-validates, and fully parses a datagram without touching
/// endpoint or identity state.
pub fn decode_udp_ingress(
    transport: UdpTransport,
    source: SocketAddr,
    wire: &[u8],
    maximum_payload: usize,
) -> Result<UdpIngress, UdpIngressDecodeError> {
    let (iv, logical) = decode_datagram(wire, maximum_payload)?;
    let packet = parse_routed_udp_packet(&logical)?;
    let body = match packet.body {
        UdpLogicalBody::PqUdpEcho(body) => UdpIngressBody::PqUdpEcho(body),
        UdpLogicalBody::PrUdpEcho(body) => UdpIngressBody::PrUdpEcho(body),
        UdpLogicalBody::PqUdpTimeSync(body) => UdpIngressBody::PqUdpTimeSync(body),
        UdpLogicalBody::PrUdpTimeSync(body) => UdpIngressBody::PrUdpTimeSync(body),
        UdpLogicalBody::GameSlotPacket(body) => UdpIngressBody::GameSlotPacket(body.to_vec()),
        UdpLogicalBody::RoomSlotPacket(body) => UdpIngressBody::RoomSlotPacket(body.to_vec()),
    };
    Ok(UdpIngress {
        transport,
        source,
        iv,
        account_id: packet.account_id,
        route_hash: packet.route_hash,
        body,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UdpDispatchRequest {
    pub ingress: UdpIngress,
    /// The exact active identity resolved by the service caller for
    /// `ingress.account_id`.
    pub identity: IdentityBinding,
    /// Exact-generation identities currently eligible to receive racing-room
    /// traffic. Ignored for non-`GameSlotPacket` ingress.
    pub racing_targets: Vec<IdentityBinding>,
    /// Exact-generation identities currently eligible to receive `MyRoom`
    /// traffic. Ignored for non-`RoomSlotPacket` ingress.
    pub room_targets: Vec<IdentityBinding>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UdpDispatchAction {
    EchoReply,
    TimeSyncReply,
    GameSlotRelay,
    RoomSlotRelay,
    ClientReplyDropped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UdpDispatchOutcome {
    pub binding_status: UdpEndpointBindStatus,
    pub action: UdpDispatchAction,
    pub sent_datagrams: usize,
    pub failed_sends: usize,
    pub unavailable_targets: usize,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum UdpServiceError {
    #[error("UDP service actor is closed")]
    Closed,

    #[error(
        "resolved identity user number {resolved_account_id} does not match ingress account ID {ingress_account_id}"
    )]
    IdentityMismatch {
        ingress_account_id: u32,
        resolved_account_id: u32,
    },

    #[error("UDP relay target count {actual} exceeds configured maximum {maximum}")]
    TooManyRelayTargets { actual: usize, maximum: usize },

    #[error(transparent)]
    EndpointState(#[from] UdpEndpointStateError),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UdpRuntimeStats {
    pub malformed_dropped: u64,
    pub admission_full_dropped: u64,
    pub receive_errors: u64,
    pub send_errors: u64,
}

#[derive(Debug, Default)]
struct SharedStats {
    malformed_dropped: AtomicU64,
    admission_full_dropped: AtomicU64,
    receive_errors: AtomicU64,
    send_errors: AtomicU64,
}

impl SharedStats {
    fn snapshot(&self) -> UdpRuntimeStats {
        UdpRuntimeStats {
            malformed_dropped: self.malformed_dropped.load(Ordering::Relaxed),
            admission_full_dropped: self.admission_full_dropped.load(Ordering::Relaxed),
            receive_errors: self.receive_errors.load(Ordering::Relaxed),
            send_errors: self.send_errors.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone)]
pub struct UdpService {
    commands: mpsc::Sender<UdpCommand>,
    maximum_relay_targets: usize,
    maximum_active_identities: usize,
}

impl UdpService {
    #[must_use]
    pub const fn max_identities(&self) -> usize {
        self.maximum_active_identities
    }

    pub async fn dispatch(
        &self,
        request: UdpDispatchRequest,
    ) -> Result<UdpDispatchOutcome, UdpServiceError> {
        if request.racing_targets.len() > self.maximum_relay_targets {
            return Err(UdpServiceError::TooManyRelayTargets {
                actual: request.racing_targets.len(),
                maximum: self.maximum_relay_targets,
            });
        }
        if request.room_targets.len() > self.maximum_relay_targets {
            return Err(UdpServiceError::TooManyRelayTargets {
                actual: request.room_targets.len(),
                maximum: self.maximum_relay_targets,
            });
        }
        let (response_tx, response_rx) = oneshot::channel();
        self.commands
            .send(UdpCommand::Dispatch {
                request,
                response: response_tx,
            })
            .await
            .map_err(|_| UdpServiceError::Closed)?;
        response_rx.await.map_err(|_| UdpServiceError::Closed)?
    }

    /// Establishes a generation fence before its first UDP packet arrives.
    pub async fn advance_identity(&self, identity: IdentityBinding) -> Result<(), UdpServiceError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.commands
            .send(UdpCommand::AdvanceIdentity {
                identity,
                response: response_tx,
            })
            .await
            .map_err(|_| UdpServiceError::Closed)?;
        response_rx.await.map_err(|_| UdpServiceError::Closed)??;
        Ok(())
    }

    /// Releases only the exact generation carried by `identity`.
    pub async fn release_identity(
        &self,
        identity: ReleasedIdentity,
    ) -> Result<(), UdpServiceError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.commands
            .send(UdpCommand::ReleaseIdentity {
                identity,
                response: response_tx,
            })
            .await
            .map_err(|_| UdpServiceError::Closed)?;
        response_rx.await.map_err(|_| UdpServiceError::Closed)
    }

    pub async fn current_target(
        &self,
        transport: UdpTransport,
        identity: IdentityBinding,
    ) -> Result<Option<CurrentUdpEndpoint>, UdpServiceError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.commands
            .send(UdpCommand::CurrentTarget {
                transport,
                identity,
                response: response_tx,
            })
            .await
            .map_err(|_| UdpServiceError::Closed)?;
        response_rx.await.map_err(|_| UdpServiceError::Closed)
    }

    async fn shutdown(&self) -> Result<(), UdpServiceError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.commands
            .send(UdpCommand::Shutdown {
                response: response_tx,
            })
            .await
            .map_err(|_| UdpServiceError::Closed)?;
        response_rx.await.map_err(|_| UdpServiceError::Closed)
    }
}

#[derive(Debug)]
enum UdpCommand {
    Dispatch {
        request: UdpDispatchRequest,
        response: oneshot::Sender<Result<UdpDispatchOutcome, UdpServiceError>>,
    },
    AdvanceIdentity {
        identity: IdentityBinding,
        response: oneshot::Sender<Result<(), UdpEndpointStateError>>,
    },
    ReleaseIdentity {
        identity: ReleasedIdentity,
        response: oneshot::Sender<()>,
    },
    CurrentTarget {
        transport: UdpTransport,
        identity: IdentityBinding,
        response: oneshot::Sender<Option<CurrentUdpEndpoint>>,
    },
    Shutdown {
        response: oneshot::Sender<()>,
    },
}

#[derive(Debug)]
enum UdpWorkerExit {
    ExplicitShutdown,
    Fatal(UdpRuntimeFailure),
}

#[derive(Debug)]
pub struct UdpRuntime {
    endpoints: UdpRuntimeEndpoints,
    service: UdpService,
    admissions: mpsc::Receiver<UdpIngress>,
    tasks: JoinSet<UdpWorkerExit>,
    task_kinds: HashMap<TaskId, UdpRuntimeTask>,
    reader_aborts: Vec<AbortHandle>,
    stats: Arc<SharedStats>,
}

impl UdpRuntime {
    pub fn spawn(
        game_socket: UdpSocket,
        p2p_socket: UdpSocket,
        config: UdpRuntimeConfig,
    ) -> Result<Self, UdpRuntimeStartError> {
        Self::spawn_with_clock(game_socket, p2p_socket, config, ServerClock::default())
    }

    pub fn spawn_with_clock(
        game_socket: UdpSocket,
        p2p_socket: UdpSocket,
        config: UdpRuntimeConfig,
        clock: ServerClock,
    ) -> Result<Self, UdpRuntimeStartError> {
        validate_config(config)?;
        let endpoints = UdpRuntimeEndpoints {
            game: game_socket.local_addr().map_err(|source| {
                UdpRuntimeStartError::LocalEndpoint {
                    transport: UdpTransport::Game,
                    source,
                }
            })?,
            p2p: p2p_socket
                .local_addr()
                .map_err(|source| UdpRuntimeStartError::LocalEndpoint {
                    transport: UdpTransport::P2p,
                    source,
                })?,
        };

        let game_socket = Arc::new(game_socket);
        let p2p_socket = Arc::new(p2p_socket);
        let stats = Arc::new(SharedStats::default());
        let (admission_tx, admissions) = mpsc::channel(config.admission_capacity);
        let mut tasks = JoinSet::new();
        let mut task_kinds = HashMap::with_capacity(3);
        let mut reader_aborts = Vec::with_capacity(2);
        let game_reader = tasks.spawn(run_reader(
            UdpTransport::Game,
            Arc::clone(&game_socket),
            admission_tx.clone(),
            config.maximum_payload,
            Arc::clone(&stats),
        ));
        task_kinds.insert(game_reader.id(), UdpRuntimeTask::Reader(UdpTransport::Game));
        reader_aborts.push(game_reader);
        let p2p_reader = tasks.spawn(run_reader(
            UdpTransport::P2p,
            Arc::clone(&p2p_socket),
            admission_tx,
            config.maximum_payload,
            Arc::clone(&stats),
        ));
        task_kinds.insert(p2p_reader.id(), UdpRuntimeTask::Reader(UdpTransport::P2p));
        reader_aborts.push(p2p_reader);

        let (command_tx, command_rx) = mpsc::channel(config.command_capacity);
        let actor_task = tasks.spawn(run_actor(
            game_socket,
            p2p_socket,
            command_rx,
            config.maximum_payload,
            NonZeroUsize::new(config.maximum_active_identities)
                .expect("configuration validation rejects zero active-identity capacity"),
            clock,
            Arc::clone(&stats),
        ));
        task_kinds.insert(actor_task.id(), UdpRuntimeTask::Actor);
        Ok(Self {
            endpoints,
            service: UdpService {
                commands: command_tx,
                maximum_relay_targets: config.maximum_relay_targets,
                maximum_active_identities: config.maximum_active_identities,
            },
            admissions,
            tasks,
            task_kinds,
            reader_aborts,
            stats,
        })
    }

    #[must_use]
    pub const fn endpoints(&self) -> UdpRuntimeEndpoints {
        self.endpoints
    }

    #[must_use]
    pub fn service(&self) -> UdpService {
        self.service.clone()
    }

    pub async fn next_ingress(&mut self) -> Option<UdpIngress> {
        self.admissions.recv().await
    }

    pub fn try_next_ingress(&mut self) -> Result<UdpIngress, mpsc::error::TryRecvError> {
        self.admissions.try_recv()
    }

    /// Waits for either the next decoded ingress or a terminal task failure.
    ///
    /// This is the preferred API for a central supervisor. The legacy
    /// [`Self::next_ingress`] and [`Self::try_next_ingress`] methods continue
    /// to consume the same bounded ingress queue without observing task
    /// failures.
    pub async fn next_event(&mut self) -> Option<UdpRuntimeEvent> {
        loop {
            let admissions_pending = !(self.admissions.is_closed() && self.admissions.is_empty());
            let tasks_pending = !self.tasks.is_empty();
            match (admissions_pending, tasks_pending) {
                (false, false) => return None,
                (true, false) => {
                    return self.admissions.recv().await.map(UdpRuntimeEvent::Ingress);
                }
                (false, true) => {
                    let Some(completion) = self.tasks.join_next_with_id().await else {
                        continue;
                    };
                    if let Some(event) =
                        runtime_event_from_task_completion(completion, &mut self.task_kinds)
                    {
                        return Some(event);
                    }
                }
                (true, true) => {
                    enum Ready {
                        Ingress(Option<UdpIngress>),
                        Task(Option<Result<(TaskId, UdpWorkerExit), JoinError>>),
                    }

                    let ready = tokio::select! {
                        biased;
                        completion = self.tasks.join_next_with_id() => Ready::Task(completion),
                        ingress = self.admissions.recv() => Ready::Ingress(ingress),
                    };
                    match ready {
                        Ready::Ingress(Some(ingress)) => {
                            return Some(UdpRuntimeEvent::Ingress(ingress));
                        }
                        Ready::Task(Some(completion)) => {
                            if let Some(event) =
                                runtime_event_from_task_completion(completion, &mut self.task_kinds)
                            {
                                return Some(event);
                            }
                        }
                        Ready::Ingress(None) | Ready::Task(None) => {}
                    }
                }
            }
        }
    }

    #[must_use]
    pub fn stats(&self) -> UdpRuntimeStats {
        self.stats.snapshot()
    }

    pub async fn shutdown(mut self) {
        for task in &self.reader_aborts {
            task.abort();
        }
        self.reader_aborts.clear();
        let _ = self.service.shutdown().await;
        while self.tasks.join_next().await.is_some() {
            // Cancellation is intentional here, and the actor acknowledges an
            // explicit shutdown. Neither condition is a runtime failure.
        }
    }
}

impl Drop for UdpRuntime {
    fn drop(&mut self) {
        self.tasks.abort_all();
    }
}

fn runtime_event_from_task_completion(
    completion: Result<(TaskId, UdpWorkerExit), JoinError>,
    task_kinds: &mut HashMap<TaskId, UdpRuntimeTask>,
) -> Option<UdpRuntimeEvent> {
    match completion {
        Ok((task_id, UdpWorkerExit::ExplicitShutdown)) => {
            task_kinds.remove(&task_id);
            None
        }
        Ok((task_id, UdpWorkerExit::Fatal(failure))) => {
            task_kinds.remove(&task_id);
            Some(UdpRuntimeEvent::Fatal(failure))
        }
        Err(error) => {
            let task = task_kinds
                .remove(&error.id())
                .unwrap_or(UdpRuntimeTask::Untracked);
            let failure = if error.is_cancelled() {
                UdpRuntimeFailure::TaskCancelled { task }
            } else if error.is_panic() {
                UdpRuntimeFailure::TaskPanicked { task }
            } else {
                UdpRuntimeFailure::TaskJoinFailed { task }
            };
            Some(UdpRuntimeEvent::Fatal(failure))
        }
    }
}

fn validate_config(config: UdpRuntimeConfig) -> Result<(), UdpRuntimeStartError> {
    if config.admission_capacity == 0 {
        return Err(UdpRuntimeStartError::ZeroQueueCapacity { queue: "admission" });
    }
    if config.command_capacity == 0 {
        return Err(UdpRuntimeStartError::ZeroQueueCapacity { queue: "command" });
    }
    if config.maximum_relay_targets == 0 {
        return Err(UdpRuntimeStartError::ZeroRelayTargetLimit);
    }
    if config.maximum_active_identities == 0 {
        return Err(UdpRuntimeStartError::ZeroActiveIdentityLimit);
    }
    if config.maximum_payload > DEFAULT_MAX_DATAGRAM_PAYLOAD {
        return Err(UdpRuntimeStartError::PayloadMaximumTooLarge {
            configured: config.maximum_payload,
            maximum: DEFAULT_MAX_DATAGRAM_PAYLOAD,
        });
    }
    Ok(())
}

async fn run_reader(
    transport: UdpTransport,
    socket: Arc<UdpSocket>,
    admission: mpsc::Sender<UdpIngress>,
    maximum_payload: usize,
    stats: Arc<SharedStats>,
) -> UdpWorkerExit {
    // Always receive the complete platform-maximum UDP datagram. In
    // particular, Winsock reports WSAEMSGSIZE instead of returning a truncated
    // packet when the caller's buffer is smaller; treating that as fatal makes
    // a single oversized datagram a permanent remote reader shutdown.
    let mut buffer = vec![0_u8; MAX_UDP_RECEIVE_DATAGRAM_SIZE];
    loop {
        match socket.recv_from(&mut buffer).await {
            Ok((length, source)) => {
                let ingress = match decode_udp_ingress(
                    transport,
                    source,
                    &buffer[..length],
                    maximum_payload,
                ) {
                    Ok(ingress) => ingress,
                    Err(error) => {
                        stats.malformed_dropped.fetch_add(1, Ordering::Relaxed);
                        tracing::trace!(%transport, %source, %error, "dropping malformed UDP ingress");
                        continue;
                    }
                };
                match admission.try_send(ingress) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        stats.admission_full_dropped.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        return UdpWorkerExit::Fatal(UdpRuntimeFailure::ReaderAdmissionClosed {
                            transport,
                        });
                    }
                }
            }
            Err(error) if is_connection_reset(&error) => {
                stats.receive_errors.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(%transport, %error, "ignoring UDP connection reset");
            }
            Err(error) if is_message_too_long(&error) => {
                stats.malformed_dropped.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(%transport, %error, "dropping oversized UDP ingress");
            }
            Err(error) => {
                stats.receive_errors.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(%transport, %error, "UDP reader stopped after socket error");
                return UdpWorkerExit::Fatal(UdpRuntimeFailure::ReaderSocket {
                    transport,
                    source: error,
                });
            }
        }
    }
}

fn is_connection_reset(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::ConnectionReset || error.raw_os_error() == Some(10_054)
}

fn is_message_too_long(error: &io::Error) -> bool {
    // WSAEMSGSIZE, Linux EMSGSIZE, and Darwin/BSD EMSGSIZE respectively.
    matches!(error.raw_os_error(), Some(10_040 | 90 | 40))
}

async fn run_actor(
    game_socket: Arc<UdpSocket>,
    p2p_socket: Arc<UdpSocket>,
    mut commands: mpsc::Receiver<UdpCommand>,
    maximum_payload: usize,
    maximum_active_identities: NonZeroUsize,
    clock: ServerClock,
    stats: Arc<SharedStats>,
) -> UdpWorkerExit {
    let mut endpoints = UdpEndpointState::with_max_active_identities(maximum_active_identities);
    while let Some(command) = commands.recv().await {
        match command {
            UdpCommand::Dispatch { request, response } => {
                let result = dispatch_ingress(
                    &mut endpoints,
                    &game_socket,
                    &p2p_socket,
                    request,
                    maximum_payload,
                    &clock,
                    &stats,
                )
                .await;
                let _ = response.send(result);
            }
            UdpCommand::AdvanceIdentity { identity, response } => {
                let result = endpoints.advance_identity(&identity);
                let _ = response.send(result);
            }
            UdpCommand::ReleaseIdentity { identity, response } => {
                endpoints.remove_released_identity(&identity);
                let _ = response.send(());
            }
            UdpCommand::CurrentTarget {
                transport,
                identity,
                response,
            } => {
                let target = endpoints.current_authorized_target(transport, &identity);
                let _ = response.send(target);
            }
            UdpCommand::Shutdown { response } => {
                endpoints.clear();
                let _ = response.send(());
                return UdpWorkerExit::ExplicitShutdown;
            }
        }
    }
    UdpWorkerExit::Fatal(UdpRuntimeFailure::ActorCommandChannelClosed)
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_ingress(
    endpoints: &mut UdpEndpointState,
    game_socket: &UdpSocket,
    p2p_socket: &UdpSocket,
    request: UdpDispatchRequest,
    maximum_payload: usize,
    clock: &ServerClock,
    stats: &SharedStats,
) -> Result<UdpDispatchOutcome, UdpServiceError> {
    let resolved_account_id = request.identity.user_no.get();
    if resolved_account_id != request.ingress.account_id {
        return Err(UdpServiceError::IdentityMismatch {
            ingress_account_id: request.ingress.account_id,
            resolved_account_id,
        });
    }

    let binding = endpoints.bind_authorized_ingress(
        request.ingress.transport,
        &request.identity,
        request.ingress.source,
        request.ingress.route_hash,
    )?;
    let socket = match request.ingress.transport {
        UdpTransport::Game => game_socket,
        UdpTransport::P2p => p2p_socket,
    };
    let mut outcome = UdpDispatchOutcome {
        binding_status: binding.status,
        action: UdpDispatchAction::ClientReplyDropped,
        sent_datagrams: 0,
        failed_sends: 0,
        unavailable_targets: 0,
    };

    match request.ingress.body {
        UdpIngressBody::PqUdpEcho(body) => {
            outcome.action = UdpDispatchAction::EchoReply;
            record_send(
                &mut outcome,
                send_packet(
                    socket,
                    request.ingress.source,
                    request.ingress.account_id,
                    request.ingress.route_hash,
                    UdpLogicalBody::PrUdpEcho(body.reply()),
                    maximum_payload,
                )
                .await,
                stats,
            );
        }
        UdpIngressBody::PqUdpTimeSync(body) => {
            outcome.action = UdpDispatchAction::TimeSyncReply;
            record_send(
                &mut outcome,
                send_packet(
                    socket,
                    request.ingress.source,
                    request.ingress.account_id,
                    request.ingress.route_hash,
                    UdpLogicalBody::PrUdpTimeSync(body.reply(clock.tick())),
                    maximum_payload,
                )
                .await,
                stats,
            );
        }
        UdpIngressBody::GameSlotPacket(body) => {
            outcome.action = UdpDispatchAction::GameSlotRelay;
            relay_to_targets(
                endpoints,
                socket,
                &request.identity,
                request.ingress.route_hash,
                request.racing_targets,
                UdpRelayBody::GameSlot(&body),
                maximum_payload,
                &mut outcome,
                stats,
            )
            .await;
        }
        UdpIngressBody::RoomSlotPacket(body) => {
            outcome.action = UdpDispatchAction::RoomSlotRelay;
            relay_to_targets(
                endpoints,
                socket,
                &request.identity,
                request.ingress.route_hash,
                request.room_targets,
                UdpRelayBody::RoomSlot(&body),
                maximum_payload,
                &mut outcome,
                stats,
            )
            .await;
        }
        UdpIngressBody::PrUdpEcho(_) | UdpIngressBody::PrUdpTimeSync(_) => {}
    }
    Ok(outcome)
}

#[derive(Debug, Clone, Copy)]
enum UdpRelayBody<'a> {
    GameSlot(&'a [u8]),
    RoomSlot(&'a [u8]),
}

impl<'a> UdpRelayBody<'a> {
    fn logical(self) -> UdpLogicalBody<'a> {
        match self {
            Self::GameSlot(body) => UdpLogicalBody::GameSlotPacket(body),
            Self::RoomSlot(body) => UdpLogicalBody::RoomSlotPacket(body),
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn relay_to_targets(
    endpoints: &UdpEndpointState,
    socket: &UdpSocket,
    source_identity: &IdentityBinding,
    source_route_hash: u32,
    targets: Vec<IdentityBinding>,
    body: UdpRelayBody<'_>,
    maximum_payload: usize,
    outcome: &mut UdpDispatchOutcome,
    stats: &SharedStats,
) {
    let mut seen = HashSet::with_capacity(targets.len());
    for target_identity in targets {
        if target_identity.user_no == source_identity.user_no
            || !seen.insert(target_identity.user_no)
        {
            continue;
        }
        let Some(target) =
            endpoints.current_authorized_target(UdpTransport::Game, &target_identity)
        else {
            outcome.unavailable_targets += 1;
            continue;
        };
        let route_hash = if target.endpoint.route_hash == 0 {
            source_route_hash
        } else {
            target.endpoint.route_hash
        };
        record_send(
            outcome,
            send_packet(
                socket,
                target.endpoint.endpoint,
                target.identity.user_no.get(),
                route_hash,
                body.logical(),
                maximum_payload,
            )
            .await,
            stats,
        );
    }
}

fn p5136_tick_from_elapsed_millis(elapsed_millis: u128) -> u32 {
    let maximum = u128::from(u32::MAX);
    if elapsed_millis <= maximum {
        u32::try_from(elapsed_millis).expect("bounded elapsed milliseconds fit in u32")
    } else {
        u32::try_from(elapsed_millis % maximum).expect("the modulo result always fits in u32")
    }
}

async fn send_packet(
    socket: &UdpSocket,
    target: SocketAddr,
    account_id: u32,
    route_hash: u32,
    body: UdpLogicalBody<'_>,
    maximum_payload: usize,
) -> bool {
    let packet = RoutedUdpPacket {
        account_id,
        route_hash,
        body,
    };
    let Ok(logical) = encode_routed_udp_packet(&packet) else {
        return false;
    };
    let Ok(wire) = encode_datagram(&logical, rand::random(), maximum_payload) else {
        return false;
    };
    matches!(
        socket.send_to(&wire, target).await,
        Ok(sent) if sent == wire.len()
    )
}

fn record_send(outcome: &mut UdpDispatchOutcome, sent: bool, stats: &SharedStats) {
    if sent {
        outcome.sent_datagrams += 1;
    } else {
        outcome.failed_sends += 1;
        stats.send_errors.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        future, io,
        net::{Ipv4Addr, SocketAddr},
        num::NonZeroUsize,
        sync::Arc,
        time::Duration,
    };

    use p5136_core::{
        datagram::encode_datagram,
        udp_protocol::{PqUdpEchoBody, RoutedUdpPacket, UdpLogicalBody, encode_routed_udp_packet},
    };
    use tokio::{net::UdpSocket, sync::mpsc, task::JoinSet, time::timeout};

    use super::{
        DEFAULT_MAX_DATAGRAM_PAYLOAD, ServerClock, SharedStats, UdpRuntime, UdpRuntimeConfig,
        UdpRuntimeEvent, UdpRuntimeFailure, UdpRuntimeStartError, UdpRuntimeTask, UdpWorkerExit,
        is_connection_reset, is_message_too_long, p5136_tick_from_elapsed_millis, run_actor,
        runtime_event_from_task_completion, validate_config,
    };
    use crate::udp_state::UdpTransport;

    #[test]
    fn windows_connection_reset_and_portable_error_kind_are_nonfatal() {
        assert!(is_connection_reset(&io::Error::from(
            io::ErrorKind::ConnectionReset
        )));
        assert!(is_connection_reset(&io::Error::from_raw_os_error(10_054)));
        assert!(!is_connection_reset(&io::Error::from(
            io::ErrorKind::ConnectionAborted
        )));
        assert!(is_message_too_long(&io::Error::from_raw_os_error(10_040)));
        assert!(is_message_too_long(&io::Error::from_raw_os_error(90)));
        assert!(is_message_too_long(&io::Error::from_raw_os_error(40)));
        assert!(!is_message_too_long(&io::Error::from_raw_os_error(10_054)));
    }

    #[test]
    fn p5136_tick_matches_csharp_maximum_wrap_rule() {
        let maximum = u128::from(u32::MAX);
        assert_eq!(p5136_tick_from_elapsed_millis(0), 0);
        assert_eq!(
            p5136_tick_from_elapsed_millis(maximum),
            u32::MAX,
            "C# returns uint.MaxValue without wrapping at the boundary"
        );
        assert_eq!(p5136_tick_from_elapsed_millis(maximum + 1), 1);
        assert_eq!(p5136_tick_from_elapsed_millis(maximum * 2), 0);
    }

    #[test]
    fn queue_and_protocol_limits_are_validated_before_spawning() {
        for (queue, config) in [
            (
                "admission",
                UdpRuntimeConfig {
                    admission_capacity: 0,
                    ..UdpRuntimeConfig::default()
                },
            ),
            (
                "command",
                UdpRuntimeConfig {
                    command_capacity: 0,
                    ..UdpRuntimeConfig::default()
                },
            ),
        ] {
            assert!(matches!(
                validate_config(config),
                Err(UdpRuntimeStartError::ZeroQueueCapacity {
                    queue: actual
                }) if actual == queue
            ));
        }
        assert!(matches!(
            validate_config(UdpRuntimeConfig {
                maximum_payload: DEFAULT_MAX_DATAGRAM_PAYLOAD + 1,
                ..UdpRuntimeConfig::default()
            }),
            Err(UdpRuntimeStartError::PayloadMaximumTooLarge { .. })
        ));
        assert!(matches!(
            validate_config(UdpRuntimeConfig {
                maximum_relay_targets: 0,
                ..UdpRuntimeConfig::default()
            }),
            Err(UdpRuntimeStartError::ZeroRelayTargetLimit)
        ));
        assert!(matches!(
            validate_config(UdpRuntimeConfig {
                maximum_active_identities: 0,
                ..UdpRuntimeConfig::default()
            }),
            Err(UdpRuntimeStartError::ZeroActiveIdentityLimit)
        ));
    }

    #[tokio::test]
    async fn next_event_yields_decoded_ingress() {
        let game_server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let game_endpoint = game_server.local_addr().unwrap();
        let p2p_server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let mut runtime =
            UdpRuntime::spawn(game_server, p2p_server, UdpRuntimeConfig::default()).unwrap();
        let logical = encode_routed_udp_packet(&RoutedUdpPacket {
            account_id: 5_136,
            route_hash: 0x1020_3040,
            body: UdpLogicalBody::PqUdpEcho(PqUdpEchoBody {
                value_1: -1,
                value_2: 2,
            }),
        })
        .unwrap();
        let wire = encode_datagram(&logical, 7, DEFAULT_MAX_DATAGRAM_PAYLOAD).unwrap();
        client.send_to(&wire, game_endpoint).await.unwrap();

        let event = timeout(Duration::from_secs(2), runtime.next_event())
            .await
            .unwrap()
            .unwrap();
        let UdpRuntimeEvent::Ingress(ingress) = event else {
            panic!("decoded ingress should win over a healthy task set");
        };
        assert_eq!(ingress.transport, UdpTransport::Game);
        assert_eq!(ingress.account_id, 5_136);
        assert_eq!(ingress.route_hash, 0x1020_3040);
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn panic_and_cancellation_are_typed_without_reading_panic_payloads() {
        let mut tasks: JoinSet<UdpWorkerExit> = JoinSet::new();
        let mut task_kinds = HashMap::new();
        let panicking = tasks.spawn(async { std::panic::panic_any(5_136_u32) });
        task_kinds.insert(panicking.id(), UdpRuntimeTask::Reader(UdpTransport::Game));
        let completion = tasks.join_next_with_id().await.unwrap();
        assert!(matches!(
            runtime_event_from_task_completion(completion, &mut task_kinds),
            Some(UdpRuntimeEvent::Fatal(UdpRuntimeFailure::TaskPanicked {
                task: UdpRuntimeTask::Reader(UdpTransport::Game)
            }))
        ));

        let cancelled = tasks.spawn(future::pending::<UdpWorkerExit>());
        task_kinds.insert(cancelled.id(), UdpRuntimeTask::Actor);
        cancelled.abort();
        let completion = tasks.join_next_with_id().await.unwrap();
        assert!(matches!(
            runtime_event_from_task_completion(completion, &mut task_kinds),
            Some(UdpRuntimeEvent::Fatal(UdpRuntimeFailure::TaskCancelled {
                task: UdpRuntimeTask::Actor
            }))
        ));
    }

    #[tokio::test]
    async fn actor_reports_unexpected_command_channel_close() {
        let game_socket = Arc::new(
            UdpSocket::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
                .await
                .unwrap(),
        );
        let p2p_socket = Arc::new(
            UdpSocket::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
                .await
                .unwrap(),
        );
        let (commands, command_rx) = mpsc::channel(1);
        drop(commands);

        let exit = run_actor(
            game_socket,
            p2p_socket,
            command_rx,
            DEFAULT_MAX_DATAGRAM_PAYLOAD,
            NonZeroUsize::new(1).unwrap(),
            ServerClock::new(),
            Arc::new(SharedStats::default()),
        )
        .await;
        assert!(matches!(
            exit,
            UdpWorkerExit::Fatal(UdpRuntimeFailure::ActorCommandChannelClosed)
        ));
    }

    #[tokio::test]
    async fn explicit_actor_shutdown_is_not_reported_as_failure() {
        let game_server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let p2p_server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let mut runtime =
            UdpRuntime::spawn(game_server, p2p_server, UdpRuntimeConfig::default()).unwrap();

        runtime.service().shutdown().await.unwrap();
        assert!(
            timeout(Duration::from_millis(25), runtime.next_event())
                .await
                .is_err(),
            "an explicitly stopped actor must not produce a fatal runtime event"
        );
        runtime.shutdown().await;
    }
}
