use std::{
    collections::{HashMap, HashSet},
    io,
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use p5136_profile::{CatalogInventory, CatalogInventoryError, ProfileStoreError};
use thiserror::Error;
use tokio::{
    net::{TcpListener, UdpSocket},
    sync::{Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore, oneshot, watch},
    task::{JoinError, JoinHandle, JoinSet},
    time::MissedTickBehavior,
};

use crate::{
    MessengerConnectionError, MessengerRuntimeConfig, MessengerServiceError,
    MessengerServiceHandle, ServerClock, ServerConfig, ServerEndpoints, UdpRuntime,
    UdpRuntimeConfig, UdpRuntimeEvent, UdpRuntimeFailure, UdpRuntimeStartError, UdpServiceError,
    WorldError, WorldHandle,
    profile_io::{
        DurableRewardReceipt, ProfileIoBootstrap, ProfileIoConfigError, ProfileIoHandle,
        ProfileIoRuntime, ProfileIoShutdownError, RewardFailureClassification,
        RewardPersistenceFailure,
    },
    session::{ProfileCoordinator, run_login_session},
    world::{
        RewardCompletionDisposition, RewardDrainStatus, RewardPersistenceCompletion,
        RewardSettlementTask, RewardTerminalReason, WorldSidecarError,
    },
};

const MAX_REWARD_PERSISTENCE_WORKERS: usize = 32;
const REWARD_PERSISTENCE_POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("maximum concurrent login sessions must be greater than zero")]
    InvalidLoginSessionLimit,

    #[error("failed to bind game UDP listener")]
    BindGameUdp(#[source] io::Error),

    #[error("failed to bind login TCP listener")]
    BindLoginTcp(#[source] io::Error),

    #[error("failed to bind P2P UDP listener")]
    BindP2pUdp(#[source] io::Error),

    #[error("failed to bind messenger TCP listener")]
    BindMessengerTcp(#[source] io::Error),

    #[error("failed to inspect a bound listener address")]
    LocalAddress(#[source] io::Error),

    #[error("failed to load inventory catalog {path}")]
    LoadCatalog {
        path: PathBuf,
        #[source]
        source: CatalogInventoryError,
    },

    #[error("inventory catalog loader task failed")]
    CatalogTask(#[source] JoinError),

    #[error(transparent)]
    ProfileIoConfig(#[from] ProfileIoConfigError),

    #[error("profile-store bootstrap task failed")]
    ProfileBootstrapTask(#[source] JoinError),

    #[error("failed to initialize the profile store")]
    ProfileBootstrap(#[source] ProfileStoreError),

    #[error(transparent)]
    ProfileIoShutdown(#[from] ProfileIoShutdownError),

    #[error("profile I/O runtime stopped unexpectedly")]
    ProfileIoRuntimeStopped,

    #[error(transparent)]
    RewardPersistence(#[from] RewardPersistenceRuntimeError),

    #[error("reward persistence runtime stopped unexpectedly")]
    RewardPersistenceRuntimeStopped,

    #[error("server supervisor has already been joined")]
    SupervisorAlreadyJoined,

    #[error("server supervisor previously failed: {message}")]
    SupervisorPreviouslyFailed { message: String },

    #[error("{service} listener failed")]
    ListenerIo {
        service: &'static str,
        #[source]
        source: io::Error,
    },

    #[error("server supervisor task failed")]
    SupervisorTask(#[from] JoinError),

    #[error("world actor stopped unexpectedly")]
    WorldActorStopped,

    #[error("world actor stopped after a messenger publication failure")]
    WorldActorMessenger(#[source] MessengerServiceError),

    #[error("world actor stopped after a UDP publication or dispatch failure")]
    WorldActorUdp(#[source] UdpServiceError),

    #[error("world actor stopped after a MyRoom failure")]
    WorldActorMyRoom {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    #[error("world actor was configured with a zero identity capacity")]
    WorldActorInvalidIdentityCapacity,

    #[error("failed to start the UDP runtime")]
    UdpRuntimeStart(#[from] UdpRuntimeStartError),

    #[error("UDP runtime stopped unexpectedly")]
    UdpRuntimeStopped,

    #[error("UDP runtime task stopped unexpectedly")]
    UdpRuntime(#[source] UdpRuntimeFailure),

    #[error("messenger actor stopped unexpectedly")]
    MessengerActorStopped,

    #[error("{service} actor task failed")]
    ActorTask {
        service: &'static str,
        #[source]
        source: JoinError,
    },

    #[error(transparent)]
    Messenger(#[from] MessengerServiceError),

    #[error(transparent)]
    World(#[from] WorldError),
}

#[derive(Debug, Error)]
pub enum RewardPersistenceRuntimeError {
    #[error("World reward scheduler failed")]
    World(#[source] WorldError),

    #[error(
        "reward persistence worker task failed for {nickname:?}/{user_no:?} in room {room_id:?}, epoch {race_epoch:?}, attempt {attempt_id:?}; terminalization error: {terminalization_error:?}"
    )]
    WorkerTask {
        room_id: Option<u32>,
        race_epoch: Option<u64>,
        attempt_id: Option<u64>,
        user_no: Option<u32>,
        nickname: Option<String>,
        terminalization_error: Option<String>,
        #[source]
        source: JoinError,
    },

    #[error(
        "terminal reward dead letter retained for {nickname:?}/{user_no:?} in room {room_id}, epoch {race_epoch}, attempt {attempt_id:?}: {reason:?} ({outstanding_lanes} outstanding lanes)"
    )]
    DeadLetter {
        room_id: u32,
        race_epoch: u64,
        attempt_id: Option<u64>,
        user_no: Option<u32>,
        nickname: Option<String>,
        reason: RewardTerminalReason,
        outstanding_lanes: usize,
    },

    #[error(
        "fatal profile persistence failure for {nickname:?}/{user_no} in room {room_id}, epoch {race_epoch}, attempt {attempt_id}: {message}"
    )]
    FatalProfile {
        room_id: u32,
        race_epoch: u64,
        attempt_id: u64,
        user_no: u32,
        nickname: String,
        message: String,
    },

    #[error(
        "World terminally rejected reward completion for {nickname:?}/{user_no} in room {room_id}, epoch {race_epoch}, attempt {attempt_id}; last persistence error: {last_persistence_error:?}"
    )]
    TerminalCompletion {
        room_id: u32,
        race_epoch: u64,
        attempt_id: u64,
        user_no: u32,
        nickname: String,
        last_persistence_error: Option<String>,
    },

    #[error(
        "profile persistence infrastructure failed for {nickname:?}/{user_no} in room {room_id}, epoch {race_epoch}, attempt {attempt_id}: {message}; terminalization error: {terminalization_error:?}"
    )]
    ProfileInfrastructure {
        room_id: u32,
        race_epoch: u64,
        attempt_id: u64,
        user_no: u32,
        nickname: String,
        message: String,
        terminalization_error: Option<String>,
    },
}

#[derive(Debug)]
pub struct BoundServer {
    config: ServerConfig,
    catalog: Option<Arc<CatalogInventory>>,
    profiles: ProfileIoBootstrap,
    game_udp: UdpSocket,
    login_tcp: TcpListener,
    p2p_udp: UdpSocket,
    messenger_tcp: TcpListener,
}

impl BoundServer {
    /// Transactionally binds all four P5136 transports. If any bind fails,
    /// already-created sockets are dropped before the error is returned.
    pub async fn bind(config: ServerConfig) -> Result<Self, ServerError> {
        if config.max_login_sessions == 0 {
            return Err(ServerError::InvalidLoginSessionLimit);
        }
        let profile_limits = crate::profile_io::ProfileIoLimits::for_server(
            config.max_login_sessions,
            reward_persistence_worker_limit(config.max_login_sessions),
        )?;
        let catalog = load_catalog(config.catalog_path.clone()).await?;
        let bind_address = config.bind_address;
        let game_udp = UdpSocket::bind(SocketAddr::new(bind_address, config.ports.game_udp()))
            .await
            .map_err(ServerError::BindGameUdp)?;
        let login_tcp = TcpListener::bind(SocketAddr::new(bind_address, config.ports.login_tcp()))
            .await
            .map_err(ServerError::BindLoginTcp)?;
        let p2p_udp = UdpSocket::bind(SocketAddr::new(bind_address, config.ports.p2p_udp()))
            .await
            .map_err(ServerError::BindP2pUdp)?;
        let messenger_tcp =
            TcpListener::bind(SocketAddr::new(bind_address, config.ports.messenger_tcp()))
                .await
                .map_err(ServerError::BindMessengerTcp)?;
        let profile_root = config.profile_root.clone();
        let profiles = tokio::task::spawn_blocking(move || {
            ProfileIoBootstrap::acquire(profile_root, profile_limits)
        })
        .await
        .map_err(ServerError::ProfileBootstrapTask)?
        .map_err(ServerError::ProfileBootstrap)?;

        Ok(Self {
            config,
            catalog,
            profiles,
            game_udp,
            login_tcp,
            p2p_udp,
            messenger_tcp,
        })
    }

    pub fn endpoints(&self) -> Result<ServerEndpoints, ServerError> {
        Ok(ServerEndpoints {
            login_tcp: self
                .login_tcp
                .local_addr()
                .map_err(ServerError::LocalAddress)?,
            game_udp: self
                .game_udp
                .local_addr()
                .map_err(ServerError::LocalAddress)?,
            p2p_udp: self
                .p2p_udp
                .local_addr()
                .map_err(ServerError::LocalAddress)?,
            messenger_tcp: self
                .messenger_tcp
                .local_addr()
                .map_err(ServerError::LocalAddress)?,
        })
    }

    pub fn start(self) -> Result<ServerHandle, ServerError> {
        let endpoints = self.endpoints()?;
        let BoundServer {
            config,
            catalog,
            profiles,
            game_udp,
            login_tcp,
            p2p_udp,
            messenger_tcp,
        } = self;
        let (shutdown, shutdown_receiver) = watch::channel(false);
        let force_shutdown_requested = Arc::new(AtomicBool::new(false));
        let messenger_config = messenger_runtime_config(&config);
        let udp_config = udp_runtime_config(&config);
        let udp_mailbox_capacity = udp_config.admission_capacity;
        let clock = ServerClock::new();
        let udp = UdpRuntime::spawn_with_clock(game_udp, p2p_udp, udp_config, clock.clone())?;
        let (messenger, messenger_task) = MessengerServiceHandle::spawn(messenger_config)?;
        let (world, world_task) = WorldHandle::spawn_with_services(
            1_024,
            udp_mailbox_capacity,
            messenger.clone(),
            udp.service(),
            clock,
        );
        let (profile_io, profile_runtime) = profiles.spawn();
        let supervisor_world = world.clone();
        let supervisor_force_shutdown = Arc::clone(&force_shutdown_requested);
        let task = tokio::spawn(async move {
            run_supervisor(
                SupervisorTransports {
                    config,
                    catalog,
                    profile_io,
                    profile_runtime,
                    login_tcp,
                    messenger_tcp,
                    udp,
                },
                shutdown_receiver,
                supervisor_world,
                world_task,
                messenger,
                messenger_task,
                supervisor_force_shutdown,
            )
            .await
        });

        Ok(ServerHandle {
            endpoints,
            shutdown,
            world,
            force_shutdown_requested,
            supervisor: AsyncMutex::new(SupervisorJoin::Running(task)),
        })
    }
}

const fn reward_persistence_worker_limit(maximum_sessions: usize) -> usize {
    if maximum_sessions < MAX_REWARD_PERSISTENCE_WORKERS {
        maximum_sessions
    } else {
        MAX_REWARD_PERSISTENCE_WORKERS
    }
}

fn udp_runtime_config(config: &ServerConfig) -> UdpRuntimeConfig {
    UdpRuntimeConfig {
        maximum_active_identities: config.max_login_sessions,
        ..UdpRuntimeConfig::default()
    }
}

fn messenger_runtime_config(config: &ServerConfig) -> MessengerRuntimeConfig {
    let defaults = MessengerRuntimeConfig::default();
    MessengerRuntimeConfig {
        max_connections: config.max_login_sessions,
        max_identities: config.max_login_sessions,
        max_frame_payload: config.max_messenger_payload,
        enter_timeout: config.login_timeout,
        idle_timeout: config.session_idle_timeout,
        write_timeout: config.session_write_timeout,
        hub_limits: crate::MessengerHubLimits {
            max_sessions: config.max_login_sessions,
            ..defaults.hub_limits
        },
        ..defaults
    }
}

#[derive(Debug)]
enum SupervisorCompletion {
    Succeeded,
    Failed { message: String },
}

impl SupervisorCompletion {
    fn result(&self) -> Result<(), ServerError> {
        match self {
            Self::Succeeded => Ok(()),
            Self::Failed { message } => Err(ServerError::SupervisorPreviouslyFailed {
                message: message.clone(),
            }),
        }
    }
}

#[derive(Debug)]
enum SupervisorJoin {
    Running(JoinHandle<Result<(), ServerError>>),
    Finished(SupervisorCompletion),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SupervisorWaitMode {
    CompletionOnly,
    ReportRewardDeadLetters,
}

impl SupervisorWaitMode {
    const fn reports_reward_dead_letters(self) -> bool {
        matches!(self, Self::ReportRewardDeadLetters)
    }
}

#[derive(Debug)]
#[must_use = "retain the server handle and call shutdown or force_shutdown before dropping it"]
pub struct ServerHandle {
    endpoints: ServerEndpoints,
    shutdown: watch::Sender<bool>,
    world: WorldHandle,
    force_shutdown_requested: Arc<AtomicBool>,
    supervisor: AsyncMutex<SupervisorJoin>,
}

impl ServerHandle {
    #[must_use]
    pub fn endpoints(&self) -> ServerEndpoints {
        self.endpoints
    }

    #[must_use]
    pub fn world(&self) -> WorldHandle {
        self.world.clone()
    }

    /// Returns the current actor-owned reward drain and recovery state.
    pub async fn reward_status(&self) -> Result<RewardDrainStatus, WorldError> {
        self.world.reward_drain_status().await
    }

    /// Waits for the server supervisor without requesting shutdown.
    ///
    /// This is cancellation-safe: dropping the wait future never consumes the
    /// supervisor join handle, so another waiter or [`Self::shutdown`] can
    /// continue observing the same terminal result.
    pub async fn wait(&self) -> Result<(), ServerError> {
        self.join_supervisor(SupervisorWaitMode::CompletionOnly)
            .await
    }

    /// Starts graceful shutdown and joins the supervisor.
    ///
    /// If a terminal reward lane blocks shutdown, this returns its structured
    /// status without consuming the handle. The caller can inspect
    /// [`Self::reward_status`], retry an actor-minted dead letter through
    /// [`Self::world`], and call `shutdown` again to finish joining the same
    /// in-progress supervisor. Do not drop the handle after that error: choose
    /// recovery or an explicit [`Self::force_shutdown`] so actor-owned tasks
    /// and the profile-store lease are not left detached.
    pub async fn shutdown(&self) -> Result<(), ServerError> {
        let _ = self.shutdown.send(true);
        self.join_supervisor(SupervisorWaitMode::ReportRewardDeadLetters)
            .await
    }

    /// Explicitly abandons in-memory actor publication/recovery state and
    /// stops the World.
    ///
    /// This is intended for an operator-confirmed emergency exit after
    /// [`Self::shutdown`] reports a retained dead letter. Durable profile
    /// jobs are still drained and accepted writes may commit to disk, but
    /// unresolved reward recovery and any pending `MyRoom` Hub publication,
    /// reserved owner echo, or final request reply are discarded. If `MyRoom`
    /// outcomes are pending, the World emits a structured warning with the
    /// pending ticket and user-index counts before stopping. `Ok(())` means
    /// forced teardown joined successfully; it does not mean those actor
    /// publications completed. Prefer [`Self::shutdown`] whenever publication
    /// must complete.
    /// Once first polled, this future transfers the World stop request to an
    /// owned runtime task, so cancelling the caller cannot leave the supervisor
    /// in force mode without a corresponding stop request.
    pub async fn force_shutdown(&self) -> Result<(), ServerError> {
        self.force_shutdown_requested.store(true, Ordering::Release);
        let _ = self.shutdown.send(true);
        let world = self.world.clone();
        let force_request = tokio::spawn(async move { world.force_shutdown().await });
        match force_request.await {
            Ok(Ok(_) | Err(WorldError::Stopped)) => {}
            Ok(Err(error)) => return Err(error.into()),
            Err(source) => {
                return Err(ServerError::ActorTask {
                    service: "world force-shutdown request",
                    source,
                });
            }
        }
        self.join_supervisor(SupervisorWaitMode::CompletionOnly)
            .await
    }

    async fn join_supervisor(&self, mode: SupervisorWaitMode) -> Result<(), ServerError> {
        let mut supervisor = self.supervisor.lock().await;
        if let SupervisorJoin::Finished(completion) = &*supervisor {
            return completion.result();
        }
        let mut poll = tokio::time::interval(REWARD_PERSISTENCE_POLL_INTERVAL);
        poll.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            let SupervisorJoin::Running(task) = &mut *supervisor else {
                return Err(ServerError::SupervisorAlreadyJoined);
            };
            tokio::select! {
                joined = task => {
                    let result = match joined {
                        Ok(result) => result,
                        Err(source) => Err(ServerError::SupervisorTask(source)),
                    };
                    match result {
                        Ok(()) => {
                            *supervisor =
                                SupervisorJoin::Finished(SupervisorCompletion::Succeeded);
                            return Ok(());
                        }
                        Err(error) => {
                            let message = error.to_string();
                            *supervisor = SupervisorJoin::Finished(
                                SupervisorCompletion::Failed { message },
                            );
                            return Err(error);
                        }
                    }
                }
                _ = poll.tick(), if mode.reports_reward_dead_letters() => {
                    match self.reward_status().await {
                        Ok(status) => {
                            if let Some(error) = dead_letter_error(&status) {
                                return Err(error.into());
                            }
                        }
                        Err(WorldError::Stopped) => {}
                        Err(error) => return Err(error.into()),
                    }
                }
            }
        }
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        let supervisor_is_live = self.supervisor.try_lock().map_or(true, |supervisor| {
            matches!(*supervisor, SupervisorJoin::Running(_))
        });
        if supervisor_is_live {
            tracing::warn!(
                "dropping a live ServerHandle detaches its supervisor; call shutdown or force_shutdown first"
            );
        }
    }
}

async fn load_catalog(path: Option<PathBuf>) -> Result<Option<Arc<CatalogInventory>>, ServerError> {
    let Some(path) = path else {
        return Ok(None);
    };
    let worker_path = path.clone();
    let catalog = tokio::task::spawn_blocking(move || CatalogInventory::load(worker_path))
        .await
        .map_err(ServerError::CatalogTask)?
        .map_err(|source| ServerError::LoadCatalog { path, source })?;
    Ok(Some(Arc::new(catalog)))
}

fn spawn_login_session(
    sessions: &mut JoinSet<()>,
    stream: tokio::net::TcpStream,
    peer: SocketAddr,
    permit: OwnedSemaphorePermit,
    config: &ServerConfig,
    world: &WorldHandle,
    profiles: &ProfileCoordinator,
) {
    let world = world.clone();
    let config = config.clone();
    let profiles = profiles.clone();
    sessions.spawn(async move {
        let _permit = permit;
        if let Err(error) = run_login_session(stream, peer, config, world, profiles).await {
            tracing::debug!(%peer, %error, "login session closed");
        }
    });
}

fn spawn_messenger_session(
    sessions: &mut JoinSet<()>,
    stream: tokio::net::TcpStream,
    peer: SocketAddr,
    permit: OwnedSemaphorePermit,
    messenger: &MessengerServiceHandle,
) {
    let messenger = messenger.clone();
    sessions.spawn(async move {
        let _permit = permit;
        if let Err(error) = messenger.serve_connection(stream, peer).await {
            if let MessengerConnectionError::Cancelled(_) = error {
                tracing::trace!(%peer, %error, "messenger session cancelled");
            } else {
                tracing::debug!(%peer, %error, "messenger session closed");
            }
        }
    });
}

fn try_login_session_permit(
    permits: &Arc<Semaphore>,
    maximum: usize,
    peer: SocketAddr,
) -> Option<OwnedSemaphorePermit> {
    Arc::clone(permits).try_acquire_owned().ok().or_else(|| {
        tracing::debug!(%peer, maximum, "login session limit reached; rejecting connection");
        None
    })
}

fn try_messenger_session_permit(
    permits: &Arc<Semaphore>,
    maximum: usize,
    peer: SocketAddr,
) -> Option<OwnedSemaphorePermit> {
    Arc::clone(permits).try_acquire_owned().ok().or_else(|| {
        tracing::debug!(%peer, maximum, "messenger session limit reached; rejecting connection");
        None
    })
}

fn handle_login_accept(
    accepted: io::Result<(tokio::net::TcpStream, SocketAddr)>,
    sessions: &mut JoinSet<()>,
    permits: &Arc<Semaphore>,
    maximum: usize,
    config: &ServerConfig,
    world: &WorldHandle,
    profiles: &ProfileCoordinator,
) -> Result<(), ServerError> {
    let (stream, peer) = accepted.map_err(|source| ServerError::ListenerIo {
        service: "login TCP",
        source,
    })?;
    let Some(permit) = try_login_session_permit(permits, maximum, peer) else {
        drop(stream);
        return Ok(());
    };
    spawn_login_session(sessions, stream, peer, permit, config, world, profiles);
    Ok(())
}

fn handle_messenger_accept(
    accepted: io::Result<(tokio::net::TcpStream, SocketAddr)>,
    sessions: &mut JoinSet<()>,
    permits: &Arc<Semaphore>,
    maximum: usize,
    messenger: &MessengerServiceHandle,
) -> Result<(), ServerError> {
    let (stream, peer) = accepted.map_err(|source| ServerError::ListenerIo {
        service: "messenger TCP",
        source,
    })?;
    let Some(permit) = try_messenger_session_permit(permits, maximum, peer) else {
        drop(stream);
        return Ok(());
    };
    spawn_messenger_session(sessions, stream, peer, permit, messenger);
    Ok(())
}

struct RewardPersistencePump {
    stop: Option<oneshot::Sender<()>>,
}

impl RewardPersistencePump {
    fn spawn(
        world: WorldHandle,
        profiles: ProfileIoHandle,
        maximum_workers: usize,
    ) -> (Self, JoinHandle<Result<(), RewardPersistenceRuntimeError>>) {
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(run_reward_persistence(
            world,
            profiles,
            stopped,
            maximum_workers,
        ));
        (Self { stop: Some(stop) }, task)
    }

    fn begin_drain(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

#[derive(Debug)]
struct RewardTaskContext {
    room_id: u32,
    race_epoch: u64,
    attempt_id: u64,
    user_no: u32,
    nickname: String,
}

impl RewardTaskContext {
    fn from_task(task: &RewardSettlementTask) -> Self {
        let fence = task.fence();
        Self {
            room_id: fence.room_id().0,
            race_epoch: fence.race_epoch().get(),
            attempt_id: task.attempt_id().get(),
            user_no: task.user_no().get(),
            nickname: task.nickname().to_owned(),
        }
    }

    fn terminal_error(
        self,
        last_persistence_error: Option<String>,
    ) -> RewardPersistenceRuntimeError {
        RewardPersistenceRuntimeError::TerminalCompletion {
            room_id: self.room_id,
            race_epoch: self.race_epoch,
            attempt_id: self.attempt_id,
            user_no: self.user_no,
            nickname: self.nickname,
            last_persistence_error,
        }
    }

    fn fatal_profile_error(self, message: String) -> RewardPersistenceRuntimeError {
        RewardPersistenceRuntimeError::FatalProfile {
            room_id: self.room_id,
            race_epoch: self.race_epoch,
            attempt_id: self.attempt_id,
            user_no: self.user_no,
            nickname: self.nickname,
            message,
        }
    }

    fn infrastructure_error(
        self,
        message: String,
        terminalization_error: Option<String>,
    ) -> RewardPersistenceRuntimeError {
        RewardPersistenceRuntimeError::ProfileInfrastructure {
            room_id: self.room_id,
            race_epoch: self.race_epoch,
            attempt_id: self.attempt_id,
            user_no: self.user_no,
            nickname: self.nickname,
            message,
            terminalization_error,
        }
    }

    fn worker_task_error(
        context: Option<Self>,
        source: JoinError,
        terminalization_error: Option<String>,
    ) -> RewardPersistenceRuntimeError {
        RewardPersistenceRuntimeError::WorkerTask {
            room_id: context.as_ref().map(|context| context.room_id),
            race_epoch: context.as_ref().map(|context| context.race_epoch),
            attempt_id: context.as_ref().map(|context| context.attempt_id),
            user_no: context.as_ref().map(|context| context.user_no),
            nickname: context.map(|context| context.nickname),
            terminalization_error,
            source,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct RewardHealthIncident {
    room_id: u32,
    race_epoch: u64,
    attempt_id: Option<u64>,
}

#[derive(Debug, Default)]
struct RewardHealthState {
    reported_incidents: HashSet<RewardHealthIncident>,
}

impl RewardHealthState {
    fn synchronize(&mut self, active_incidents: impl IntoIterator<Item = RewardHealthIncident>) {
        let active_incidents = active_incidents.into_iter().collect::<HashSet<_>>();
        self.reported_incidents
            .retain(|incident| active_incidents.contains(incident));
    }

    fn should_report(&mut self, error: &RewardPersistenceRuntimeError) -> bool {
        reward_health_incident(error)
            .is_none_or(|incident| self.reported_incidents.insert(incident))
    }
}

#[derive(Debug)]
enum RewardCompletionDiagnostic {
    DurableReceipt,
    RetryableProfile(String),
    JobFatalProfile(String),
    InfrastructureFatalProfile(String),
}

fn reward_health_incident(error: &RewardPersistenceRuntimeError) -> Option<RewardHealthIncident> {
    let (room_id, race_epoch, attempt_id) = match error {
        RewardPersistenceRuntimeError::DeadLetter {
            room_id,
            race_epoch,
            attempt_id,
            ..
        } => (*room_id, *race_epoch, *attempt_id),
        RewardPersistenceRuntimeError::FatalProfile {
            room_id,
            race_epoch,
            attempt_id,
            ..
        }
        | RewardPersistenceRuntimeError::TerminalCompletion {
            room_id,
            race_epoch,
            attempt_id,
            ..
        }
        | RewardPersistenceRuntimeError::ProfileInfrastructure {
            room_id,
            race_epoch,
            attempt_id,
            ..
        } => (*room_id, *race_epoch, Some(*attempt_id)),
        RewardPersistenceRuntimeError::World(
            WorldError::RewardAttemptIdExhausted {
                room_id,
                race_epoch,
                ..
            }
            | WorldError::RewardAttemptLeaseFailuresExhausted {
                room_id,
                race_epoch,
                ..
            }
            | WorldError::RewardAttemptLeaseDeadlineOverflow {
                room_id,
                race_epoch,
                ..
            }
            | WorldError::RewardRetryDeadlineOverflow {
                room_id,
                race_epoch,
                ..
            },
        ) => (*room_id, *race_epoch, None),
        RewardPersistenceRuntimeError::World(_)
        | RewardPersistenceRuntimeError::WorkerTask { .. } => {
            return None;
        }
    };
    Some(RewardHealthIncident {
        room_id,
        race_epoch,
        attempt_id,
    })
}

fn dead_letter_runtime_error(
    status: &RewardDrainStatus,
    dead_letter: &crate::RewardDeadLetter,
) -> RewardPersistenceRuntimeError {
    let fence = dead_letter.fence();
    RewardPersistenceRuntimeError::DeadLetter {
        room_id: fence.room_id().0,
        race_epoch: fence.race_epoch().get(),
        attempt_id: dead_letter
            .failed_attempt_id()
            .map(crate::world::RewardAttemptId::get),
        user_no: dead_letter.failed_user_no().map(crate::UserNo::get),
        nickname: dead_letter.failed_nickname().map(str::to_owned),
        reason: dead_letter.reason(),
        outstanding_lanes: status.outstanding_lanes().len(),
    }
}

fn dead_letter_error(status: &RewardDrainStatus) -> Option<RewardPersistenceRuntimeError> {
    let dead_letter = status.dead_letters().first()?;
    Some(dead_letter_runtime_error(status, dead_letter))
}

async fn publish_reward_completion(
    world: &WorldHandle,
    result: Result<DurableRewardReceipt, RewardPersistenceFailure>,
) -> Result<Option<RewardPersistenceRuntimeError>, RewardPersistenceRuntimeError> {
    let (context, completion, diagnostic) = match result {
        Ok(receipt) => (
            RewardTaskContext::from_task(receipt.task()),
            RewardPersistenceCompletion::Durable(receipt),
            RewardCompletionDiagnostic::DurableReceipt,
        ),
        Err(failure) => {
            let context = RewardTaskContext::from_task(failure.task());
            let classification = failure.classification();
            let message = failure.to_string();
            let task = failure.into_task();
            let (completion, diagnostic) = match classification {
                RewardFailureClassification::Retryable => (
                    RewardPersistenceCompletion::RetryableFailure(task),
                    RewardCompletionDiagnostic::RetryableProfile(message),
                ),
                RewardFailureClassification::JobFatal => (
                    RewardPersistenceCompletion::FatalFailure(task),
                    RewardCompletionDiagnostic::JobFatalProfile(message),
                ),
                RewardFailureClassification::InfrastructureFatal => (
                    RewardPersistenceCompletion::FatalFailure(task),
                    RewardCompletionDiagnostic::InfrastructureFatalProfile(message),
                ),
            };
            (context, completion, diagnostic)
        }
    };
    let completion_result = world.complete_reward_task(completion, Instant::now()).await;
    let diagnostic = match diagnostic {
        RewardCompletionDiagnostic::InfrastructureFatalProfile(message) => {
            let terminalization_error = match completion_result {
                Ok(RewardCompletionDisposition::TerminalFailure) => None,
                Ok(disposition) => Some(format!(
                    "World returned {disposition:?} instead of retaining terminal reward state"
                )),
                Err(error) => Some(error.to_string()),
            };
            return Err(context.infrastructure_error(message, terminalization_error));
        }
        diagnostic => diagnostic,
    };
    let disposition = completion_result.map_err(RewardPersistenceRuntimeError::World)?;
    if disposition != RewardCompletionDisposition::TerminalFailure {
        return Ok(None);
    }
    let error = match diagnostic {
        RewardCompletionDiagnostic::DurableReceipt => context.terminal_error(None),
        RewardCompletionDiagnostic::RetryableProfile(message) => {
            context.terminal_error(Some(message))
        }
        RewardCompletionDiagnostic::JobFatalProfile(message) => {
            context.fatal_profile_error(message)
        }
        RewardCompletionDiagnostic::InfrastructureFatalProfile(message) => {
            return Err(context.infrastructure_error(
                message,
                Some("infrastructure classification bypassed terminal handling".to_owned()),
            ));
        }
    };
    Ok(Some(error))
}

fn report_reward_health_failure(
    error: &RewardPersistenceRuntimeError,
    health: &mut RewardHealthState,
) {
    if health.should_report(error) {
        tracing::error!(
            %error,
            "reward persistence entered a terminal state; inspect reward status and retry an actor-minted dead letter"
        );
    }
}

fn report_reward_status_health(status: &RewardDrainStatus, health: &mut RewardHealthState) {
    health.synchronize(status.dead_letters().iter().map(|dead_letter| {
        let fence = dead_letter.fence();
        RewardHealthIncident {
            room_id: fence.room_id().0,
            race_epoch: fence.race_epoch().get(),
            attempt_id: dead_letter
                .failed_attempt_id()
                .map(crate::world::RewardAttemptId::get),
        }
    }));
    for dead_letter in status.dead_letters() {
        report_reward_health_failure(&dead_letter_runtime_error(status, dead_letter), health);
    }
}

const fn is_terminal_reward_scheduler_error(error: &WorldError) -> bool {
    matches!(
        error,
        WorldError::RewardAttemptIdExhausted { .. }
            | WorldError::RewardAttemptLeaseFailuresExhausted { .. }
            | WorldError::RewardAttemptLeaseDeadlineOverflow { .. }
            | WorldError::RewardRetryDeadlineOverflow { .. }
    )
}

async fn terminalize_reward_task(
    world: &WorldHandle,
    task: RewardSettlementTask,
) -> Option<String> {
    match world
        .complete_reward_task(
            RewardPersistenceCompletion::FatalFailure(task),
            Instant::now(),
        )
        .await
    {
        Ok(RewardCompletionDisposition::TerminalFailure) => None,
        Ok(disposition) => Some(format!(
            "World returned {disposition:?} instead of retaining terminal reward state"
        )),
        Err(error) => Some(error.to_string()),
    }
}

fn retain_reward_runtime_error(
    retained: &mut Option<RewardPersistenceRuntimeError>,
    error: RewardPersistenceRuntimeError,
) {
    if retained.is_none() {
        *retained = Some(error);
    } else {
        tracing::error!(
            %error,
            "reward persistence also failed while accepted worker outcomes were draining"
        );
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the pump loop co-locates tracked worker ownership, actor completion, and drain ordering"
)]
async fn run_reward_persistence(
    world: WorldHandle,
    profiles: ProfileIoHandle,
    mut stop: oneshot::Receiver<()>,
    maximum_workers: usize,
) -> Result<(), RewardPersistenceRuntimeError> {
    let maximum_workers = maximum_workers.max(1);
    let mut workers: JoinSet<Result<DurableRewardReceipt, RewardPersistenceFailure>> =
        JoinSet::new();
    let mut worker_tasks = HashMap::new();
    let mut poll = tokio::time::interval(REWARD_PERSISTENCE_POLL_INTERVAL);
    poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut draining = false;
    let mut retained_error = None;
    let mut health = RewardHealthState::default();

    loop {
        if workers.is_empty() {
            debug_assert!(worker_tasks.is_empty());
            if let Some(error) = retained_error.take() {
                return Err(error);
            }
        }
        if draining && workers.is_empty() {
            let status = world
                .reward_drain_status()
                .await
                .map_err(RewardPersistenceRuntimeError::World)?;
            report_reward_status_health(&status, &mut health);
            if status.is_drained() {
                return Ok(());
            }
        }

        tokio::select! {
            biased;
            _ = &mut stop, if !draining => {
                draining = true;
            }
            completed = workers.join_next_with_id(), if !workers.is_empty() => {
                match completed {
                    Some(Ok((worker_id, result))) => {
                        worker_tasks.remove(&worker_id);
                        match publish_reward_completion(&world, result).await {
                            Ok(Some(error)) => {
                                report_reward_health_failure(&error, &mut health);
                            }
                            Ok(None) => {}
                            Err(error @ RewardPersistenceRuntimeError::ProfileInfrastructure {
                                ..
                            }) => {
                                report_reward_health_failure(&error, &mut health);
                                retain_reward_runtime_error(&mut retained_error, error);
                            }
                            Err(error) => {
                                retain_reward_runtime_error(&mut retained_error, error);
                            }
                        }
                    }
                    Some(Err(source)) => {
                        let worker_id = source.id();
                        let task = worker_tasks.remove(&worker_id);
                        let context = task.as_ref().map(RewardTaskContext::from_task);
                        let terminalization_error = match task {
                            Some(task) => terminalize_reward_task(&world, task).await,
                            None => Some(
                                "worker task had no tracked actor-owned reward capability"
                                    .to_owned(),
                            ),
                        };
                        let error = RewardTaskContext::worker_task_error(
                            context,
                            source,
                            terminalization_error,
                        );
                        retain_reward_runtime_error(&mut retained_error, error);
                    }
                    None => {}
                }
            }
            _ = poll.tick(), if retained_error.is_none() => {
                let status = world
                    .reward_drain_status()
                    .await
                    .map_err(RewardPersistenceRuntimeError::World)?;
                report_reward_status_health(&status, &mut health);
                let available = maximum_workers.saturating_sub(workers.len());
                if available == 0 {
                    continue;
                }
                let tasks = match world
                    .take_due_reward_tasks(Instant::now(), available)
                    .await
                {
                    Ok(tasks) => tasks,
                    Err(error) if is_terminal_reward_scheduler_error(&error) => {
                        let error = RewardPersistenceRuntimeError::World(error);
                        report_reward_health_failure(&error, &mut health);
                        continue;
                    }
                    Err(error) => return Err(RewardPersistenceRuntimeError::World(error)),
                };
                for task in tasks {
                    let tracked_task = task.clone();
                    let profiles = profiles.clone();
                    let worker = workers.spawn(async move { profiles.persist_reward(task).await });
                    drop(worker_tasks.insert(worker.id(), tracked_task));
                }
            }
        }
    }
}

struct SupervisorTransports {
    config: ServerConfig,
    catalog: Option<Arc<CatalogInventory>>,
    profile_io: ProfileIoHandle,
    profile_runtime: ProfileIoRuntime,
    login_tcp: TcpListener,
    messenger_tcp: TcpListener,
    udp: UdpRuntime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeTaskState {
    Running,
    Completed,
}

impl RuntimeTaskState {
    const fn is_running(self) -> bool {
        matches!(self, Self::Running)
    }
}

struct CoreActors {
    world: WorldHandle,
    world_task: JoinHandle<Result<(), WorldSidecarError>>,
    world_state: RuntimeTaskState,
    reward_pump: RewardPersistencePump,
    reward_task: JoinHandle<Result<(), RewardPersistenceRuntimeError>>,
    reward_state: RuntimeTaskState,
    udp: UdpRuntime,
    messenger: MessengerServiceHandle,
    messenger_task: JoinHandle<()>,
    messenger_state: RuntimeTaskState,
    profile_runtime: ProfileIoRuntime,
    profile_state: RuntimeTaskState,
}

fn retain_cleanup_error(
    retained: &mut Option<ServerError>,
    error: ServerError,
    secondary_context: &'static str,
) {
    match retained {
        None => *retained = Some(error),
        Some(current)
            if is_provisional_world_stopped(current)
                && is_authoritative_world_actor_error(&error) =>
        {
            tracing::error!(
                error = %current,
                "provisional World stop observation was superseded by the actor task cause"
            );
            *current = error;
        }
        Some(_) => tracing::error!(%error, "{secondary_context}"),
    }
}

fn is_provisional_world_stopped(error: &ServerError) -> bool {
    matches!(
        error,
        ServerError::World(WorldError::Stopped)
            | ServerError::RewardPersistence(RewardPersistenceRuntimeError::World(
                WorldError::Stopped
            ))
    )
}

fn is_authoritative_world_actor_error(error: &ServerError) -> bool {
    matches!(
        error,
        ServerError::WorldActorStopped
            | ServerError::WorldActorMessenger(_)
            | ServerError::WorldActorUdp(_)
            | ServerError::WorldActorMyRoom { .. }
            | ServerError::WorldActorInvalidIdentityCapacity
            | ServerError::ActorTask {
                service: "world",
                ..
            }
    )
}

async fn stop_reward_runtime(
    mut pump: RewardPersistencePump,
    task: JoinHandle<Result<(), RewardPersistenceRuntimeError>>,
    state: RuntimeTaskState,
) -> Option<ServerError> {
    if !state.is_running() {
        return None;
    }
    pump.begin_drain();
    match task.await {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error.into()),
        Err(source) => Some(ServerError::ActorTask {
            service: "reward persistence",
            source,
        }),
    }
}

async fn quiesce_and_drain_sessions(
    world: &WorldHandle,
    world_state: RuntimeTaskState,
) -> Option<ServerError> {
    if !world_state.is_running() {
        return None;
    }
    if let Err(error) = world.quiesce().await {
        // A closed command reply is only provisional evidence: the actor task
        // join below owns the typed terminal cause, if one exists.
        if matches!(error, WorldError::Stopped) {
            return None;
        }
        return Some(error.into());
    }
    match world.drain_sessions().await {
        Ok(()) | Err(WorldError::Stopped) => None,
        Err(error) => Some(error.into()),
    }
}

async fn request_world_shutdown(world: &WorldHandle) -> Option<ServerError> {
    let graceful = world.shutdown().await;
    // A semantic terminal lane never reaches this branch: the live reward
    // pump waits for explicit recovery and drains it first. Force is reserved
    // for collapse of the pump itself, where no recovery runtime remains.
    let forced = if graceful.is_err() {
        Some(world.force_shutdown().await)
    } else {
        None
    };
    let mut error = None;
    if let Err(source) = graceful {
        retain_cleanup_error(
            &mut error,
            source.into(),
            "World graceful shutdown barrier also failed",
        );
    }
    if let Some(Err(source)) = forced {
        retain_cleanup_error(
            &mut error,
            source.into(),
            "World force shutdown also failed",
        );
    }
    error
}

async fn stop_world_runtime(
    world: WorldHandle,
    task: JoinHandle<Result<(), WorldSidecarError>>,
    world_state: RuntimeTaskState,
    force_shutdown_requested: &AtomicBool,
) -> Option<ServerError> {
    if !world_state.is_running() {
        return None;
    }
    let mut error = if force_shutdown_requested.load(Ordering::Acquire) {
        None
    } else {
        request_world_shutdown(&world).await
    };
    let provisional_stopped = matches!(error, Some(ServerError::World(WorldError::Stopped)));
    if provisional_stopped {
        // `Stopped` means the reply channel closed, not why the actor stopped.
        // Defer the public error until the unique actor JoinHandle is observed.
        error = None;
    }
    match task.await {
        Ok(Ok(())) if provisional_stopped => {
            error = Some(ServerError::WorldActorStopped);
        }
        Ok(Ok(())) => {}
        Ok(Err(source)) => retain_cleanup_error(
            &mut error,
            world_sidecar_error(source),
            "World sidecar also failed during cleanup",
        ),
        Err(source) => retain_cleanup_error(
            &mut error,
            ServerError::ActorTask {
                service: "world",
                source,
            },
            "World actor task also failed during cleanup",
        ),
    }
    error
}

async fn stop_messenger_runtime(
    messenger: MessengerServiceHandle,
    task: JoinHandle<()>,
    state: RuntimeTaskState,
) -> Option<ServerError> {
    if !state.is_running() {
        return None;
    }
    let graceful = messenger.shutdown().await;
    match task.await {
        Ok(()) => graceful.err().map(ServerError::from),
        Err(source) => Some(ServerError::ActorTask {
            service: "messenger",
            source,
        }),
    }
}

async fn stop_profile_runtime(
    profile_runtime: ProfileIoRuntime,
    state: RuntimeTaskState,
) -> Option<ServerError> {
    let result = match state {
        RuntimeTaskState::Running => profile_runtime.shutdown().await,
        RuntimeTaskState::Completed => {
            profile_runtime.finish_completed();
            Ok(())
        }
    };
    result.err().map(ServerError::from)
}

async fn finish_supervisor(
    mut sessions: JoinSet<()>,
    actors: CoreActors,
    transport_result: Result<(), ServerError>,
    force_shutdown_requested: &AtomicBool,
) -> Result<(), ServerError> {
    let CoreActors {
        world,
        world_task,
        world_state,
        reward_pump,
        reward_task,
        reward_state,
        udp,
        messenger,
        messenger_task,
        messenger_state,
        profile_runtime,
        profile_state,
    } = actors;
    let mut cleanup_error = None;

    if let Some(error) = quiesce_and_drain_sessions(&world, world_state).await {
        cleanup_error = Some(error);
    }

    sessions.abort_all();
    while sessions.join_next().await.is_some() {}

    if let Some(error) = stop_reward_runtime(reward_pump, reward_task, reward_state).await {
        let expected_forced_stop = force_shutdown_requested.load(Ordering::Acquire)
            && matches!(
                error,
                ServerError::RewardPersistence(RewardPersistenceRuntimeError::World(
                    WorldError::Stopped
                ))
            );
        if !expected_forced_stop {
            retain_cleanup_error(
                &mut cleanup_error,
                error,
                "reward persistence drain also failed during cleanup",
            );
        }
    }
    // Reward workers no longer need profile admission. Graceful teardown keeps
    // World alive until every accepted profile job has synchronously queued
    // its completion and the dedicated FIFO barrier has been observed.
    if force_shutdown_requested.load(Ordering::Acquire) {
        if let Some(error) =
            stop_world_runtime(world, world_task, world_state, force_shutdown_requested).await
        {
            retain_cleanup_error(
                &mut cleanup_error,
                error,
                "World force shutdown also failed during cleanup",
            );
        }
        if let Some(error) = stop_profile_runtime(profile_runtime, profile_state).await {
            retain_cleanup_error(
                &mut cleanup_error,
                error,
                "profile I/O drain also failed after forced World shutdown",
            );
        }
    } else {
        if let Some(error) = stop_profile_runtime(profile_runtime, profile_state).await {
            retain_cleanup_error(
                &mut cleanup_error,
                error,
                "profile I/O drain also failed during server cleanup",
            );
        }
        if world_state.is_running()
            && let Err(error) = world.drain_myroom_completions().await
        {
            // The World task is the authoritative source for a closed or
            // failed completion barrier and is joined immediately below.
            tracing::error!(%error, "MyRoom completion drain barrier failed");
        }
        if let Some(error) =
            stop_world_runtime(world, world_task, world_state, force_shutdown_requested).await
        {
            retain_cleanup_error(
                &mut cleanup_error,
                error,
                "World shutdown also failed during cleanup",
            );
        }
    }

    udp.shutdown().await;

    if let Some(error) = stop_messenger_runtime(messenger, messenger_task, messenger_state).await {
        retain_cleanup_error(
            &mut cleanup_error,
            error,
            "messenger shutdown also failed during cleanup",
        );
    }

    match transport_result {
        Err(error) => {
            if let Some(cleanup_error) = cleanup_error {
                tracing::error!(
                    %cleanup_error,
                    "server cleanup also failed after the primary transport failure"
                );
            }
            Err(error)
        }
        Ok(()) => cleanup_error.map_or(Ok(()), Err),
    }
}

fn unexpected_world_exit(result: Result<Result<(), WorldSidecarError>, JoinError>) -> ServerError {
    match result {
        Ok(Ok(())) => ServerError::WorldActorStopped,
        Ok(Err(source)) => world_sidecar_error(source),
        Err(source) => ServerError::ActorTask {
            service: "world",
            source,
        },
    }
}

fn is_expected_forced_world_exit(
    force_shutdown_requested: &AtomicBool,
    result: &Result<Result<(), WorldSidecarError>, JoinError>,
) -> bool {
    force_shutdown_requested.load(Ordering::Acquire) && matches!(result, Ok(Ok(())))
}

fn unexpected_reward_exit(
    result: Result<Result<(), RewardPersistenceRuntimeError>, JoinError>,
) -> ServerError {
    match result {
        Ok(Ok(())) => ServerError::RewardPersistenceRuntimeStopped,
        Ok(Err(source)) => ServerError::RewardPersistence(source),
        Err(source) => ServerError::ActorTask {
            service: "reward persistence",
            source,
        },
    }
}

fn is_expected_forced_reward_exit(
    force_shutdown_requested: &AtomicBool,
    result: &Result<Result<(), RewardPersistenceRuntimeError>, JoinError>,
) -> bool {
    force_shutdown_requested.load(Ordering::Acquire)
        && matches!(
            result,
            Ok(Err(RewardPersistenceRuntimeError::World(
                WorldError::Stopped
            )))
        )
}

fn world_sidecar_error(error: WorldSidecarError) -> ServerError {
    match error {
        WorldSidecarError::Messenger(source) => ServerError::WorldActorMessenger(source),
        WorldSidecarError::Udp(source) => ServerError::WorldActorUdp(source),
        WorldSidecarError::MyRoom(source) => ServerError::WorldActorMyRoom {
            source: Box::new(source),
        },
        WorldSidecarError::MyRoomPersistence(source) => ServerError::WorldActorMyRoom {
            source: Box::new(source),
        },
        WorldSidecarError::InvalidIdentityCapacity => {
            ServerError::WorldActorInvalidIdentityCapacity
        }
    }
}

fn unexpected_messenger_exit(result: Result<(), JoinError>) -> ServerError {
    match result {
        Ok(()) => ServerError::MessengerActorStopped,
        Err(source) => ServerError::ActorTask {
            service: "messenger",
            source,
        },
    }
}

fn handle_udp_event(
    world: &WorldHandle,
    event: Option<UdpRuntimeEvent>,
) -> Result<(), ServerError> {
    match event {
        Some(UdpRuntimeEvent::Ingress(ingress)) => match world.try_udp_ingress(ingress) {
            Ok(()) => Ok(()),
            Err(tokio::sync::mpsc::error::TrySendError::Full(ingress)) => {
                tracing::trace!(
                    transport = %ingress.transport,
                    source = %ingress.source,
                    account_id = ingress.account_id,
                    "dropping UDP ingress because the world UDP mailbox is full"
                );
                Ok(())
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                Err(ServerError::WorldActorStopped)
            }
        },
        Some(UdpRuntimeEvent::Fatal(source)) => Err(ServerError::UdpRuntime(source)),
        None => Err(ServerError::UdpRuntimeStopped),
    }
}

fn unexpected_profile_exit(result: Result<(), ProfileIoShutdownError>) -> ServerError {
    match result {
        Ok(()) => ServerError::ProfileIoRuntimeStopped,
        Err(error) => error.into(),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "all readiness branches belong to one tokio::select lifecycle boundary"
)]
async fn run_supervisor(
    transports: SupervisorTransports,
    mut shutdown: watch::Receiver<bool>,
    world: WorldHandle,
    mut world_task: JoinHandle<Result<(), WorldSidecarError>>,
    messenger: MessengerServiceHandle,
    mut messenger_task: JoinHandle<()>,
    force_shutdown_requested: Arc<AtomicBool>,
) -> Result<(), ServerError> {
    let SupervisorTransports {
        config,
        catalog,
        profile_io,
        mut profile_runtime,
        login_tcp,
        messenger_tcp,
        mut udp,
    } = transports;
    let (reward_pump, mut reward_task) = RewardPersistencePump::spawn(
        world.clone(),
        profile_io.clone(),
        reward_persistence_worker_limit(config.max_login_sessions),
    );
    let profiles = ProfileCoordinator::new(profile_io, catalog);
    let login_session_permits = Arc::new(Semaphore::new(config.max_login_sessions));
    let messenger_session_permits = Arc::new(Semaphore::new(config.max_login_sessions));
    let mut sessions = JoinSet::new();
    let mut world_state = RuntimeTaskState::Running;
    let mut reward_state = RuntimeTaskState::Running;
    let mut messenger_state = RuntimeTaskState::Running;
    let mut profile_state = RuntimeTaskState::Running;

    let transport_result = loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break Ok(());
                }
            }
            accepted = login_tcp.accept() => {
                if let Err(error) = handle_login_accept(
                    accepted,
                    &mut sessions,
                    &login_session_permits,
                    config.max_login_sessions,
                    &config,
                    &world,
                    &profiles,
                ) {
                    break Err(error);
                }
            }
            event = udp.next_event() => {
                if let Err(error) = handle_udp_event(&world, event) {
                    break Err(error);
                }
            }
            accepted = messenger_tcp.accept() => {
                if let Err(error) = handle_messenger_accept(
                    accepted,
                    &mut sessions,
                    &messenger_session_permits,
                    config.max_login_sessions,
                    &messenger,
                ) {
                    break Err(error);
                }
            }
            completed = sessions.join_next(), if !sessions.is_empty() => {
                if let Some(Err(error)) = completed {
                    tracing::error!(%error, "transport session task panicked");
                }
            }
            result = &mut world_task => {
                world_state = RuntimeTaskState::Completed;
                break if is_expected_forced_world_exit(&force_shutdown_requested, &result) {
                    Ok(())
                } else {
                    Err(unexpected_world_exit(result))
                };
            }
            result = &mut reward_task => {
                reward_state = RuntimeTaskState::Completed;
                break if is_expected_forced_reward_exit(&force_shutdown_requested, &result) {
                    Ok(())
                } else {
                    Err(unexpected_reward_exit(result))
                };
            }
            result = profile_runtime.wait() => {
                profile_state = RuntimeTaskState::Completed;
                break Err(unexpected_profile_exit(result));
            }
            result = &mut messenger_task => {
                messenger_state = RuntimeTaskState::Completed;
                break Err(unexpected_messenger_exit(result));
            }
        }
    };

    finish_supervisor(
        sessions,
        CoreActors {
            world,
            world_task,
            world_state,
            reward_pump,
            reward_task,
            reward_state,
            udp,
            messenger,
            messenger_task,
            messenger_state,
            profile_runtime,
            profile_state,
        },
        transport_result,
        &force_shutdown_requested,
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::{
        error::Error,
        fmt::Write as _,
        fs,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        path::Path,
        sync::{Arc, Barrier, atomic::AtomicBool},
        time::{Duration, Instant},
    };

    use p5136_core::{
        adler32,
        bml::BmlNode,
        channel::serialize_pr_channel_move_in,
        datagram::{DEFAULT_MAX_DATAGRAM_PAYLOAD, encode_datagram},
        equipment_protocol::{
            PlantPartEquipRequest, serialize_equip_tuning_failure, serialize_equip_tuning_success,
        },
        frame, handshake,
        login::{LegacyTime, serialize_pr_cn_authen_login},
        messenger::{encode_frame as encode_messenger_frame, serialize_guild_chat},
        myroom_protocol::MyRoomInfo,
        packet::{PacketReader, PacketWriter},
        race_start_protocol::P5136KartPhysicsBlock,
        room_protocol::{
            ChCreateRoomRequest, CreateRoomOutcome, JoinRoomStatus, ROOM_CONNECTION_CONTEXT_LENGTH,
            ROOM_DATA_LENGTH, RoomPlayer, serialize_ch_create_room_reply,
            serialize_ch_join_room_reply, serialize_ch_leave_room_reply,
        },
        startup::{
            GameOptions, PrGetRiderFields, RIDER_ITEM_SNAPSHOT_WIRE_LENGTH,
            serialize_channel_static_reply, serialize_lo_rp_event_reward,
            serialize_pr_add_time_event_init, serialize_pr_get_game_option, serialize_pr_get_rider,
            serialize_pr_login_vip_info,
        },
        udp_protocol::{
            PqUdpEchoBody, PqUdpTimeSyncBody, RoutedUdpPacket, UdpLogicalBody,
            encode_routed_udp_packet,
        },
    };
    use p5136_profile::{EquipmentExceptions, Profile, ProfileStore, rider_item_snapshot};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream, UdpSocket},
        sync::{mpsc, oneshot},
        time,
    };

    use super::{
        BoundServer, RewardHealthState, RewardPersistencePump, RewardPersistenceRuntimeError,
        RewardTaskContext, RuntimeTaskState, ServerError, ServerHandle, SupervisorJoin,
        SupervisorTransports, is_expected_forced_reward_exit, is_expected_forced_world_exit,
        is_terminal_reward_scheduler_error, load_catalog, messenger_runtime_config,
        request_world_shutdown, retain_cleanup_error, run_supervisor, stop_reward_runtime,
        stop_world_runtime, udp_runtime_config, unexpected_profile_exit, world_sidecar_error,
    };
    use crate::{
        ChannelBinding, MessengerServiceHandle, MigrationToken, ProfileIoConfigError,
        ProfileIoError, ProfileIoRuntimeError, ProfileIoShutdownError, ServerClock, ServerConfig,
        ServerEndpoints, UdpIngress, UdpIngressBody, UdpRuntime, UdpTransport, WorldError,
        WorldHandle, decode_udp_ingress,
        myroom_persistence::{MyRoomInfoPublication, MyRoomInfoWriteError},
        profile_io::{ProfileIoBootstrap, ProfileIoLimits},
        read_encrypted_frame,
        world::{
            LobbyCommandPayload, MyRoomLifecycleError, RewardCompletionDisposition,
            RewardLanePhase, RewardPersistenceCompletion, RoomCommandPayload, RoomParticipant,
            StartRoomPlan, WorldSidecarError,
            test_support::{
                spawn_due_reward_world, spawn_myroom_world, spawn_paused_full_mailbox_world,
            },
        },
    };

    struct LoginClient {
        stream: TcpStream,
        send_iv: u32,
        receive_iv: u32,
        user_no: u32,
        pmap: u32,
        screen: u8,
    }

    type RewardTestHook = Arc<dyn Fn(&str) + Send + Sync>;

    struct RewardFailureBarriers {
        panic_entered: Arc<Barrier>,
        panic_release: Arc<Barrier>,
        commit_entered: Arc<Barrier>,
        commit_release: Arc<Barrier>,
    }

    impl RewardFailureBarriers {
        fn new() -> Self {
            Self {
                panic_entered: Arc::new(Barrier::new(2)),
                panic_release: Arc::new(Barrier::new(2)),
                commit_entered: Arc::new(Barrier::new(2)),
                commit_release: Arc::new(Barrier::new(2)),
            }
        }

        fn hook(&self) -> RewardTestHook {
            let panic_entered = Arc::clone(&self.panic_entered);
            let panic_release = Arc::clone(&self.panic_release);
            let commit_entered = Arc::clone(&self.commit_entered);
            let commit_release = Arc::clone(&self.commit_release);
            Arc::new(move |nickname: &str| match nickname {
                "PanicReward" => {
                    panic_entered.wait();
                    panic_release.wait();
                    panic!("synthetic reward infrastructure failure");
                }
                "CommittedReward" => {
                    commit_entered.wait();
                    commit_release.wait();
                }
                other => panic!("unexpected reward profile {other:?}"),
            })
        }

        async fn rendezvous(barrier: Arc<Barrier>) {
            time::timeout(
                Duration::from_secs(1),
                tokio::task::spawn_blocking(move || barrier.wait()),
            )
            .await
            .unwrap()
            .unwrap();
        }

        async fn wait_until_both_entered(&self) {
            Self::rendezvous(Arc::clone(&self.panic_entered)).await;
            Self::rendezvous(Arc::clone(&self.commit_entered)).await;
        }

        async fn release_panic(&self) {
            Self::rendezvous(Arc::clone(&self.panic_release)).await;
        }

        async fn release_commit(&self) {
            Self::rendezvous(Arc::clone(&self.commit_release)).await;
        }
    }

    #[test]
    fn recorded_scheduler_terminal_errors_do_not_mean_world_collapse() {
        assert!(is_terminal_reward_scheduler_error(
            &WorldError::RewardAttemptIdExhausted {
                room_id: 1,
                race_epoch: 2,
                user_no: 3,
            }
        ));
        assert!(is_terminal_reward_scheduler_error(
            &WorldError::RewardAttemptLeaseFailuresExhausted {
                room_id: 1,
                race_epoch: 2,
                user_no: 3,
            }
        ));
        assert!(is_terminal_reward_scheduler_error(
            &WorldError::RewardAttemptLeaseDeadlineOverflow {
                room_id: 1,
                race_epoch: 2,
                user_no: 3,
            }
        ));
        assert!(is_terminal_reward_scheduler_error(
            &WorldError::RewardRetryDeadlineOverflow {
                room_id: 1,
                race_epoch: 2,
                user_no: 3,
            }
        ));
        assert!(!is_terminal_reward_scheduler_error(
            &WorldError::RewardSchedulerInvariant {
                room_id: 1,
                user_no: 3,
            }
        ));
    }

    #[test]
    fn unexpected_profile_runtime_exit_preserves_its_typed_cause() {
        assert!(matches!(
            unexpected_profile_exit(Ok(())),
            ServerError::ProfileIoRuntimeStopped
        ));

        let error = unexpected_profile_exit(Err(ProfileIoShutdownError::Runtime(
            ProfileIoRuntimeError::WorkerPanicked {
                operation: "profile test",
                message: "synthetic panic".to_owned(),
            },
        )));
        assert!(matches!(
            error,
            ServerError::ProfileIoShutdown(ProfileIoShutdownError::Runtime(
                ProfileIoRuntimeError::WorkerPanicked {
                    operation: "profile test",
                    ref message,
                }
            )) if message == "synthetic panic"
        ));
    }

    fn loopback_endpoints() -> ServerEndpoints {
        let endpoint = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        ServerEndpoints {
            login_tcp: endpoint,
            game_udp: endpoint,
            p2p_udp: endpoint,
            messenger_tcp: endpoint,
        }
    }

    fn synthetic_server_handle(
        supervisor_task: tokio::task::JoinHandle<Result<(), ServerError>>,
    ) -> (ServerHandle, tokio::task::JoinHandle<()>) {
        let (world, world_task) = WorldHandle::spawn(16);
        let (shutdown, _shutdown_receiver) = tokio::sync::watch::channel(false);
        (
            ServerHandle {
                endpoints: loopback_endpoints(),
                shutdown,
                world,
                force_shutdown_requested: Arc::new(AtomicBool::new(false)),
                supervisor: tokio::sync::Mutex::new(SupervisorJoin::Running(supervisor_task)),
            },
            world_task,
        )
    }

    async fn supervised_prepared_server(
        profile_root: &Path,
        world: WorldHandle,
        world_task: tokio::task::JoinHandle<Result<(), WorldSidecarError>>,
    ) -> Result<ServerHandle, Box<dyn Error + Send + Sync>> {
        let (server, _profile_io) =
            supervised_prepared_server_with_profile_io(profile_root, world, world_task).await?;
        Ok(server)
    }

    async fn supervised_prepared_server_with_profile_io(
        profile_root: &Path,
        world: WorldHandle,
        world_task: tokio::task::JoinHandle<Result<(), WorldSidecarError>>,
    ) -> Result<(ServerHandle, crate::profile_io::ProfileIoHandle), Box<dyn Error + Send + Sync>>
    {
        let loopback = Ipv4Addr::LOCALHOST;
        let config = ServerConfig {
            bind_address: IpAddr::V4(loopback),
            advertised_address: loopback,
            profile_root: profile_root.to_owned(),
            first_message_delay: Duration::ZERO,
            ..ServerConfig::default()
        };
        let (profile_io, profile_runtime) = test_profile_bootstrap(&config).spawn();
        let test_profile_io = profile_io.clone();
        let game_udp = UdpSocket::bind((loopback, 0)).await?;
        let p2p_udp = UdpSocket::bind((loopback, 0)).await?;
        let udp = UdpRuntime::spawn_with_clock(
            game_udp,
            p2p_udp,
            udp_runtime_config(&config),
            ServerClock::new(),
        )?;
        let login_tcp = TcpListener::bind((loopback, 0)).await?;
        let messenger_tcp = TcpListener::bind((loopback, 0)).await?;
        let (messenger, messenger_task) =
            MessengerServiceHandle::spawn(messenger_runtime_config(&config))?;
        let (shutdown, shutdown_receiver) = tokio::sync::watch::channel(false);
        let force_shutdown_requested = Arc::new(AtomicBool::new(false));
        let supervisor_force_shutdown = Arc::clone(&force_shutdown_requested);
        let supervisor = tokio::spawn(run_supervisor(
            SupervisorTransports {
                config,
                catalog: None,
                profile_io,
                profile_runtime,
                login_tcp,
                messenger_tcp,
                udp,
            },
            shutdown_receiver,
            world.clone(),
            world_task,
            messenger,
            messenger_task,
            supervisor_force_shutdown,
        ));

        Ok((
            ServerHandle {
                endpoints: loopback_endpoints(),
                shutdown,
                world,
                force_shutdown_requested,
                supervisor: tokio::sync::Mutex::new(SupervisorJoin::Running(supervisor)),
            },
            test_profile_io,
        ))
    }

    async fn inject_loading_reward_lane(
        world: &WorldHandle,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let source = world.register_session(SocketAddr::new(ip, 43_500)).await?;
        let claimed = world.claim_identity(source, "PumpFailureLane").await?;
        let channel = ChannelBinding {
            channel_id: 67,
            game_type: 67,
        };
        let token = MigrationToken::new(43_500)
            .ok_or_else(|| std::io::Error::other("test migration token must be nonzero"))?;
        world
            .begin_migration(source, channel, token, Instant::now())
            .await?;
        let (destination, _cancelled, _outbound) = world
            .register_login_session(SocketAddr::new(ip, 43_501))
            .await?;
        world
            .complete_migration(
                destination,
                claimed.user_no,
                channel.channel_id,
                token,
                Instant::now(),
            )
            .await?;
        world
            .room_protocol(
                destination,
                RoomCommandPayload::Create {
                    request: ChCreateRoomRequest {
                        room_name: "Pump failure lane".to_owned(),
                        password: String::new(),
                        game_type: 1,
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
                    },
                    participant: RoomParticipant {
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
                    },
                },
            )
            .await?;
        world
            .lobby_command(
                destination,
                LobbyCommandPayload::StartRoom(StartRoomPlan::new(vec![0x1111_2222], Vec::new())),
            )
            .await?;
        let status = world.reward_drain_status().await?;
        assert_eq!(status.outstanding_lanes().len(), 1);
        assert!(!status.is_drained());
        Ok(())
    }

    #[tokio::test]
    async fn stopped_reward_pump_forces_world_after_guarded_shutdown_refusal()
    -> Result<(), Box<dyn Error + Send + Sync>> {
        let (world, world_task) = WorldHandle::spawn(16);
        inject_loading_reward_lane(&world).await?;
        let pump = RewardPersistencePump { stop: None };
        let pump_task = tokio::spawn(async {
            Err(RewardPersistenceRuntimeError::World(
                WorldError::RewardSchedulerInvariant {
                    room_id: 1,
                    user_no: 1,
                },
            ))
        });
        let pump_error = stop_reward_runtime(pump, pump_task, RuntimeTaskState::Running).await;
        assert!(matches!(
            pump_error,
            Some(ServerError::RewardPersistence(
                RewardPersistenceRuntimeError::World(WorldError::RewardSchedulerInvariant { .. })
            ))
        ));

        let world_error =
            time::timeout(Duration::from_secs(1), request_world_shutdown(&world)).await?;
        assert!(matches!(
            world_error,
            Some(ServerError::World(WorldError::RewardShutdownBlocked {
                outstanding_lanes: 1,
                ..
            }))
        ));
        time::timeout(Duration::from_secs(1), world_task).await??;
        Ok(())
    }

    #[test]
    fn reward_health_dedupes_current_incident_and_resets_after_recovery() {
        let mut health = RewardHealthState::default();
        let rich = RewardTaskContext {
            room_id: 7,
            race_epoch: 11,
            attempt_id: 17,
            user_no: 13,
            nickname: "RetryRider".to_owned(),
        }
        .terminal_error(Some("exact final persistence cause".to_owned()));
        let generic = RewardPersistenceRuntimeError::DeadLetter {
            room_id: 7,
            race_epoch: 11,
            attempt_id: Some(17),
            user_no: Some(13),
            nickname: Some("RetryRider".to_owned()),
            reason: crate::RewardTerminalReason::RewardPersistence,
            outstanding_lanes: 1,
        };
        let other = RewardPersistenceRuntimeError::DeadLetter {
            room_id: 8,
            race_epoch: 12,
            attempt_id: Some(19),
            user_no: Some(14),
            nickname: Some("OtherRider".to_owned()),
            reason: crate::RewardTerminalReason::RewardPersistence,
            outstanding_lanes: 1,
        };

        assert!(health.should_report(&rich));
        assert!(!health.should_report(&generic));
        let reterminalized = RewardPersistenceRuntimeError::DeadLetter {
            room_id: 7,
            race_epoch: 11,
            attempt_id: Some(18),
            user_no: Some(13),
            nickname: Some("RetryRider".to_owned()),
            reason: crate::RewardTerminalReason::RewardPersistence,
            outstanding_lanes: 1,
        };
        assert!(
            health.should_report(&reterminalized),
            "a new actor-minted attempt on the same race fence is a distinct incident"
        );
        assert!(health.should_report(&other));
        assert!(rich.to_string().contains("exact final persistence cause"));
        health.synchronize(std::iter::empty());
        assert!(health.should_report(&rich));
    }

    #[test]
    fn public_profile_error_graph_is_available_from_the_crate_root() {
        fn assert_public_error<T: Error>() {}

        assert_public_error::<ProfileIoConfigError>();
        assert_public_error::<ProfileIoError>();
        assert_public_error::<ProfileIoRuntimeError>();
        assert_public_error::<ProfileIoShutdownError>();
    }

    #[tokio::test]
    async fn concurrent_and_repeated_shutdown_share_cached_success()
    -> Result<(), Box<dyn Error + Send + Sync>> {
        let profile_root = tempfile::tempdir()?;
        let (server, _) = start_test_server(profile_root.path(), None).await;
        let (first, second) = tokio::join!(server.shutdown(), server.shutdown());
        assert!(first.is_ok());
        assert!(second.is_ok());
        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_supervisor_join_failure_is_cached_for_followup_callers()
    -> Result<(), Box<dyn Error + Send + Sync>> {
        let supervisor_task = tokio::spawn(std::future::pending::<Result<(), ServerError>>());
        supervisor_task.abort();
        let (server, world_task) = synthetic_server_handle(supervisor_task);

        let (first, second) = tokio::join!(server.shutdown(), server.shutdown());
        let first_is_original = matches!(&first, Err(ServerError::SupervisorTask(_)));
        let second_is_original = matches!(&second, Err(ServerError::SupervisorTask(_)));
        let first_is_cached = matches!(&first, Err(ServerError::SupervisorPreviouslyFailed { .. }));
        let second_is_cached =
            matches!(&second, Err(ServerError::SupervisorPreviouslyFailed { .. }));
        assert_ne!(first_is_original, second_is_original);
        assert_ne!(first_is_cached, second_is_cached);
        assert!(matches!(
            server.shutdown().await,
            Err(ServerError::SupervisorPreviouslyFailed { ref message })
                if message.contains("server supervisor task failed")
        ));

        server.world.force_shutdown().await?;
        world_task.await?;
        Ok(())
    }

    #[tokio::test]
    async fn wait_observes_supervisor_failure_without_requesting_shutdown()
    -> Result<(), Box<dyn Error + Send + Sync>> {
        let supervisor_task = tokio::spawn(async { Err(ServerError::ProfileIoRuntimeStopped) });
        let (server, world_task) = synthetic_server_handle(supervisor_task);

        assert!(matches!(
            server.wait().await,
            Err(ServerError::ProfileIoRuntimeStopped)
        ));
        assert!(matches!(
            server.wait().await,
            Err(ServerError::SupervisorPreviouslyFailed { ref message })
                if message.contains("profile I/O runtime stopped unexpectedly")
        ));

        server.world.force_shutdown().await?;
        world_task.await?;
        Ok(())
    }

    #[tokio::test]
    async fn cancelling_completion_wait_does_not_consume_supervisor_join()
    -> Result<(), Box<dyn Error + Send + Sync>> {
        let (finish, finished) = oneshot::channel();
        let supervisor_task = tokio::spawn(async move {
            let _ = finished.await;
            Ok(())
        });
        let (server, world_task) = synthetic_server_handle(supervisor_task);

        assert!(
            time::timeout(Duration::from_millis(10), server.wait())
                .await
                .is_err()
        );
        assert!(finish.send(()).is_ok());
        server.wait().await?;
        server.wait().await?;

        server.world.force_shutdown().await?;
        world_task.await?;
        Ok(())
    }

    #[tokio::test]
    async fn cancelling_shutdown_wait_does_not_consume_supervisor_join()
    -> Result<(), Box<dyn Error + Send + Sync>> {
        let (finish, finished) = oneshot::channel();
        let supervisor_task = tokio::spawn(async move {
            let _ = finished.await;
            Ok(())
        });
        let (server, world_task) = synthetic_server_handle(supervisor_task);

        {
            let shutdown = server.shutdown();
            tokio::pin!(shutdown);
            assert!(
                time::timeout(Duration::from_millis(10), &mut shutdown)
                    .await
                    .is_err()
            );
        }
        assert!(finish.send(()).is_ok());
        server.shutdown().await?;
        server.shutdown().await?;

        server.world.force_shutdown().await?;
        world_task.await?;
        Ok(())
    }

    #[test]
    fn force_exit_normalization_matches_only_operator_caused_terminal_shapes() {
        let requested = AtomicBool::new(true);
        assert!(is_expected_forced_world_exit(&requested, &Ok(Ok(()))));
        assert!(is_expected_forced_reward_exit(
            &requested,
            &Ok(Err(RewardPersistenceRuntimeError::World(
                WorldError::Stopped
            )))
        ));
        assert!(!is_expected_forced_reward_exit(
            &requested,
            &Ok(Err(RewardPersistenceRuntimeError::World(
                WorldError::RewardSchedulerInvariant {
                    room_id: 1,
                    user_no: 1,
                }
            )))
        ));

        let not_requested = AtomicBool::new(false);
        assert!(!is_expected_forced_world_exit(&not_requested, &Ok(Ok(()))));
        assert!(!is_expected_forced_reward_exit(
            &not_requested,
            &Ok(Err(RewardPersistenceRuntimeError::World(
                WorldError::Stopped
            )))
        ));
    }

    #[test]
    fn myroom_world_terminal_preserves_its_typed_error_source() {
        let session = crate::SessionId::new(77);
        let error = world_sidecar_error(WorldSidecarError::MyRoom(
            MyRoomLifecycleError::OutboundUnavailable { session },
        ));
        let ServerError::WorldActorMyRoom { source } = error else {
            panic!("MyRoom terminal must retain its supervisor error category");
        };
        assert!(matches!(
            source.downcast_ref::<MyRoomLifecycleError>(),
            Some(MyRoomLifecycleError::OutboundUnavailable { session: actual })
                if *actual == session
        ));
    }

    #[tokio::test]
    async fn world_join_cause_replaces_provisional_stopped_reply() {
        let (world, stopped_actor) = WorldHandle::spawn(4);
        world.force_shutdown().await.unwrap();
        stopped_actor.await.unwrap();

        let session = crate::SessionId::new(78);
        let actor = tokio::spawn(async move {
            Err(WorldSidecarError::MyRoom(
                MyRoomLifecycleError::OutboundUnavailable { session },
            ))
        });
        let error = stop_world_runtime(
            world,
            actor,
            RuntimeTaskState::Running,
            &AtomicBool::new(false),
        )
        .await
        .expect("the typed actor failure must survive cleanup");

        let ServerError::WorldActorMyRoom { source } = error else {
            panic!("the actor JoinHandle must outrank a provisional WorldError::Stopped");
        };
        assert!(matches!(
            source.downcast_ref::<MyRoomLifecycleError>(),
            Some(MyRoomLifecycleError::OutboundUnavailable { session: actual })
                if *actual == session
        ));
    }

    #[test]
    fn cleanup_selection_replaces_only_provisional_world_stop_observations() {
        let session = crate::SessionId::new(79);
        let typed_world_error = || {
            world_sidecar_error(WorldSidecarError::MyRoom(
                MyRoomLifecycleError::OutboundUnavailable { session },
            ))
        };

        let mut direct = Some(ServerError::World(WorldError::Stopped));
        retain_cleanup_error(&mut direct, typed_world_error(), "test secondary");
        assert!(matches!(direct, Some(ServerError::WorldActorMyRoom { .. })));

        let mut reward = Some(ServerError::RewardPersistence(
            RewardPersistenceRuntimeError::World(WorldError::Stopped),
        ));
        retain_cleanup_error(&mut reward, typed_world_error(), "test secondary");
        assert!(matches!(reward, Some(ServerError::WorldActorMyRoom { .. })));

        let mut meaningful = Some(ServerError::ProfileIoRuntimeStopped);
        retain_cleanup_error(&mut meaningful, typed_world_error(), "test secondary");
        assert!(matches!(
            meaningful,
            Some(ServerError::ProfileIoRuntimeStopped)
        ));
    }

    #[tokio::test]
    async fn force_shutdown_preserves_unrelated_supervisor_failure()
    -> Result<(), Box<dyn Error + Send + Sync>> {
        let supervisor_task = tokio::spawn(async { Err(ServerError::ProfileIoRuntimeStopped) });
        let (server, world_task) = synthetic_server_handle(supervisor_task);

        assert!(matches!(
            server.force_shutdown().await,
            Err(ServerError::ProfileIoRuntimeStopped)
        ));
        assert!(matches!(
            server.force_shutdown().await,
            Err(ServerError::SupervisorPreviouslyFailed { ref message })
                if message.contains("profile I/O runtime stopped unexpectedly")
        ));
        world_task.await?;
        Ok(())
    }

    #[tokio::test]
    async fn cancelling_force_shutdown_cannot_strand_a_full_world_mailbox()
    -> Result<(), Box<dyn Error + Send + Sync>> {
        let fixture = spawn_paused_full_mailbox_world();
        let (finish, finished) = oneshot::channel();
        let supervisor_task = tokio::spawn(async move {
            let _ = finished.await;
            Ok(())
        });
        let (shutdown, _shutdown_receiver) = tokio::sync::watch::channel(false);
        let server = ServerHandle {
            endpoints: loopback_endpoints(),
            shutdown,
            world: fixture.handle,
            force_shutdown_requested: Arc::new(AtomicBool::new(false)),
            supervisor: tokio::sync::Mutex::new(SupervisorJoin::Running(supervisor_task)),
        };

        let mut force = Box::pin(server.force_shutdown());
        let first_poll = std::future::poll_fn(|context| {
            std::task::Poll::Ready(std::future::Future::poll(force.as_mut(), context))
        })
        .await;
        assert!(
            first_poll.is_pending(),
            "a full mailbox must hold the owned force request pending"
        );
        drop(force);

        assert!(fixture.start.send(()).is_ok());
        time::timeout(Duration::from_secs(1), fixture.actor).await???;
        assert!(finish.send(()).is_ok());
        time::timeout(Duration::from_secs(1), server.wait()).await??;
        Ok(())
    }

    #[tokio::test]
    async fn retained_dead_letter_can_be_forced_through_server_handle()
    -> Result<(), Box<dyn Error + Send + Sync>> {
        let profile_root = tempfile::tempdir()?;
        let fixture = spawn_due_reward_world(&["ForceDeadLetter"]);
        let tasks = fixture
            .handle
            .take_due_reward_tasks(Instant::now(), 1)
            .await?;
        assert_eq!(tasks.len(), 1);
        assert_eq!(
            fixture
                .handle
                .complete_reward_task(
                    RewardPersistenceCompletion::FatalFailure(
                        tasks.into_iter().next().expect("one due reward task"),
                    ),
                    Instant::now(),
                )
                .await?,
            RewardCompletionDisposition::TerminalFailure
        );
        let _outbound_receivers = fixture.outbound_receivers;
        let server =
            supervised_prepared_server(profile_root.path(), fixture.handle, fixture.actor).await?;

        let graceful_error = time::timeout(Duration::from_secs(1), server.shutdown())
            .await?
            .expect_err("retained dead letter must block graceful shutdown");
        assert!(matches!(
            graceful_error,
            ServerError::RewardPersistence(RewardPersistenceRuntimeError::DeadLetter {
                ref nickname,
                ..
            }) if nickname.as_deref() == Some("ForceDeadLetter")
        ));
        let status = server.reward_status().await?;
        assert_eq!(status.dead_letters().len(), 1);
        assert!(!status.is_drained());

        time::timeout(Duration::from_secs(1), server.force_shutdown()).await??;
        time::timeout(Duration::from_secs(1), server.wait()).await??;
        time::timeout(Duration::from_secs(1), server.shutdown()).await??;
        time::timeout(Duration::from_secs(1), server.force_shutdown()).await??;
        Ok(())
    }

    #[tokio::test]
    async fn reward_pump_drains_before_profile_and_world_shutdown() {
        let root = tempfile::tempdir().unwrap();
        let bootstrap =
            ProfileIoBootstrap::acquire(root.path().to_owned(), ProfileIoLimits::for_tests(2, 2))
                .unwrap();
        let (profiles, profile_runtime) = bootstrap.spawn();
        let (world, world_task) = WorldHandle::spawn(8);
        let (mut pump, pump_task) = RewardPersistencePump::spawn(world.clone(), profiles, 2);

        world.quiesce().await.unwrap();
        pump.begin_drain();
        time::timeout(Duration::from_secs(1), pump_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        world.shutdown().await.unwrap();
        time::timeout(Duration::from_secs(1), world_task)
            .await
            .unwrap()
            .unwrap();
        profile_runtime.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn graceful_supervisor_keeps_world_alive_until_myroom_profile_completion_drains()
    -> Result<(), Box<dyn Error + Send + Sync>> {
        let root = tempfile::tempdir()?;
        ProfileStore::new(root.path()).save("SessionMyRoomOwner", &Profile::default())?;
        let fixture = spawn_myroom_world(MyRoomInfo::default());
        let owner_session = fixture.owner.session;
        let owner_nickname = fixture.owner.identity.nickname.clone();
        let (server, profile_io) =
            supervised_prepared_server_with_profile_io(root.path(), fixture.handle, fixture.actor)
                .await?;
        let world = server.world();
        let admission = profile_io
            .admit(&owner_nickname, "test blocked MyRoom supervisor drain")
            .await?;
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let worker_entered = Arc::clone(&entered);
        let worker_release = Arc::clone(&release);
        let hook = Arc::new(move || {
            worker_entered.wait();
            worker_release.wait();
        });
        let proposed = MyRoomInfo {
            room_id: 5136,
            bgm: 7,
            ..MyRoomInfo::default()
        };
        let persisted = proposed.clone();
        let write_world = world.clone();
        let write = tokio::spawn(async move {
            write_world
                .persist_myroom_owner_info_with_test_hook(owner_session, proposed, admission, hook)
                .await
        });
        RewardFailureBarriers::rendezvous(entered).await;

        let mut shutdown = Box::pin(server.shutdown());
        assert!(
            time::timeout(Duration::from_millis(50), &mut shutdown)
                .await
                .is_err(),
            "graceful shutdown must wait for the accepted profile worker"
        );
        world.reward_drain_status().await?;

        RewardFailureBarriers::rendezvous(release).await;
        let receipt = time::timeout(Duration::from_secs(1), write).await???;
        assert_eq!(receipt.info(), &persisted);
        assert_eq!(
            receipt.publication(),
            MyRoomInfoPublication::PersistedAfterRelease,
            "session drain releases the owner before the durable completion is published"
        );
        time::timeout(Duration::from_secs(1), &mut shutdown).await??;
        let loaded = ProfileStore::new(root.path()).load_or_create("SessionMyRoomOwner")?;
        assert_eq!(loaded.revision, Some(2));
        assert_eq!(loaded.profile.my_room.try_to_protocol_info()?, persisted);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn forced_supervisor_drains_disk_after_abandoning_myroom_publication()
    -> Result<(), Box<dyn Error + Send + Sync>> {
        let root = tempfile::tempdir()?;
        ProfileStore::new(root.path()).save("SessionMyRoomOwner", &Profile::default())?;
        let fixture = spawn_myroom_world(MyRoomInfo::default());
        let owner_session = fixture.owner.session;
        let owner_nickname = fixture.owner.identity.nickname.clone();
        let mut owner_outbound = fixture.owner.outbound;
        let _visitor_outbound = fixture.visitor.outbound;
        let (server, profile_io) =
            supervised_prepared_server_with_profile_io(root.path(), fixture.handle, fixture.actor)
                .await?;
        let world = server.world();
        let admission = profile_io
            .admit(&owner_nickname, "test blocked forced MyRoom drain")
            .await?;
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let worker_entered = Arc::clone(&entered);
        let worker_release = Arc::clone(&release);
        let hook = Arc::new(move || {
            worker_entered.wait();
            worker_release.wait();
        });
        let proposed = MyRoomInfo {
            room_id: 5136,
            bgm: 9,
            ..MyRoomInfo::default()
        };
        let persisted = proposed.clone();
        let write_world = world.clone();
        let write = tokio::spawn(async move {
            write_world
                .persist_myroom_owner_info_with_test_hook(owner_session, proposed, admission, hook)
                .await
        });
        RewardFailureBarriers::rendezvous(entered).await;

        let mut force = Box::pin(server.force_shutdown());
        assert!(
            time::timeout(Duration::from_millis(50), &mut force)
                .await
                .is_err(),
            "forced teardown must still drain the accepted profile worker"
        );
        time::timeout(Duration::from_secs(1), async {
            loop {
                match world.reward_drain_status().await {
                    Err(WorldError::Stopped) => break,
                    Ok(_) => tokio::task::yield_now().await,
                    Err(error) => panic!("unexpected World status while forcing shutdown: {error}"),
                }
            }
        })
        .await?;

        let write_result = time::timeout(Duration::from_secs(1), write).await??;
        assert!(matches!(
            write_result,
            Err(MyRoomInfoWriteError::WorldStopped)
        ));
        assert_eq!(
            owner_outbound.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected),
            "force shutdown must abandon the reserved owner echo"
        );

        RewardFailureBarriers::rendezvous(release).await;
        time::timeout(Duration::from_secs(1), &mut force).await??;
        let loaded = ProfileStore::new(root.path()).load_or_create("SessionMyRoomOwner")?;
        assert_eq!(loaded.revision, Some(2));
        assert_eq!(loaded.profile.my_room.try_to_protocol_info()?, persisted);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn infrastructure_failure_drains_other_accepted_reward_outcomes() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        store.load_or_create("PanicReward").unwrap();
        store.load_or_create("CommittedReward").unwrap();
        let bootstrap =
            ProfileIoBootstrap::acquire(root.path().to_owned(), ProfileIoLimits::for_tests(4, 4))
                .unwrap();
        let (profiles, profile_runtime) = bootstrap.spawn();
        let barriers = RewardFailureBarriers::new();
        let profiles = profiles.with_reward_test_hook(barriers.hook());
        let fixture = spawn_due_reward_world(&["PanicReward", "CommittedReward"]);
        let world = fixture.handle.clone();
        let actor = fixture.actor;
        let _outbound_receivers = fixture.outbound_receivers;
        let (_pump, pump_task) = RewardPersistencePump::spawn(world.clone(), profiles, 2);

        barriers.wait_until_both_entered().await;
        barriers.release_panic().await;

        let status = time::timeout(Duration::from_secs(1), async {
            loop {
                let status = world.reward_drain_status().await.unwrap();
                if status
                    .dead_letters()
                    .iter()
                    .any(|dead_letter| dead_letter.failed_nickname() == Some("PanicReward"))
                {
                    break status;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            status.outstanding_lanes().iter().any(|lane| {
                lane.nickname() == "CommittedReward" && lane.phase() == RewardLanePhase::InFlight
            }),
            "an accepted peer reward must remain actor-owned while its disk outcome is pending"
        );

        barriers.release_commit().await;
        let error = time::timeout(Duration::from_secs(1), pump_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(matches!(
            error,
            RewardPersistenceRuntimeError::ProfileInfrastructure {
                ref nickname,
                ..
            } if nickname == "PanicReward"
        ));

        let final_status = world.reward_drain_status().await.unwrap();
        assert_eq!(final_status.dead_letters().len(), 1);
        assert_eq!(
            final_status.dead_letters()[0].failed_nickname(),
            Some("PanicReward")
        );
        assert!(
            final_status
                .outstanding_lanes()
                .iter()
                .all(|lane| lane.nickname() != "CommittedReward")
        );
        let committed = store.load_or_create("CommittedReward").unwrap();
        assert_eq!(committed.revision, Some(2));
        assert!(committed.profile.race_reward_receipt.is_some());
        assert!(committed.profile.rider.lucci >= 1_000_000);

        world.force_shutdown().await.unwrap();
        actor.await.unwrap().unwrap();
        assert!(matches!(
            profile_runtime.shutdown().await,
            Err(ProfileIoShutdownError::Runtime(
                ProfileIoRuntimeError::WorkerPanicked {
                    operation: "persist race reward",
                    ref message,
                }
            )) if message == "synthetic reward infrastructure failure"
        ));
    }

    #[tokio::test]
    async fn reward_pump_exits_promptly_when_world_is_already_dead() {
        let root = tempfile::tempdir().unwrap();
        let bootstrap =
            ProfileIoBootstrap::acquire(root.path().to_owned(), ProfileIoLimits::for_tests(2, 2))
                .unwrap();
        let (profiles, profile_runtime) = bootstrap.spawn();
        let (world, world_task) = WorldHandle::spawn(8);
        let (_pump, pump_task) = RewardPersistencePump::spawn(world.clone(), profiles, 2);

        world.force_shutdown().await.unwrap();
        time::timeout(Duration::from_secs(1), world_task)
            .await
            .unwrap()
            .unwrap();
        let error = time::timeout(Duration::from_secs(1), pump_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(matches!(
            error,
            RewardPersistenceRuntimeError::World(WorldError::Stopped)
        ));
        profile_runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn full_runtime_sends_exact_server_first_handshake_and_shuts_down() {
        let loopback = Ipv4Addr::LOCALHOST;
        let profile_root = tempfile::tempdir().unwrap();
        let config = ServerConfig {
            bind_address: IpAddr::V4(loopback),
            advertised_address: loopback,
            profile_root: profile_root.path().to_owned(),
            first_message_delay: Duration::ZERO,
            login_timeout: Duration::from_secs(2),
            ..ServerConfig::default()
        };
        let profiles = test_profile_bootstrap(&config);
        let bound = BoundServer {
            config,
            catalog: None,
            profiles,
            game_udp: UdpSocket::bind((loopback, 0)).await.unwrap(),
            login_tcp: TcpListener::bind((loopback, 0)).await.unwrap(),
            p2p_udp: UdpSocket::bind((loopback, 0)).await.unwrap(),
            messenger_tcp: TcpListener::bind((loopback, 0)).await.unwrap(),
        };

        let server = bound.start().unwrap();
        let endpoints = server.endpoints();
        let world = server.world();
        let mut client = TcpStream::connect(endpoints.login_tcp).await.unwrap();

        let mut length_bytes = [0_u8; 4];
        client.read_exact(&mut length_bytes).await.unwrap();
        let length = usize::try_from(u32::from_le_bytes(length_bytes)).unwrap();
        assert_eq!(length, 308);
        let mut payload = vec![0_u8; length];
        client.read_exact(&mut payload).await.unwrap();
        assert_eq!(payload, handshake::first_message_payload().unwrap());
        assert_eq!(world.session_count().await.unwrap(), 1);

        drop(client);
        time::timeout(Duration::from_secs(1), async {
            loop {
                if world.session_count().await.unwrap() == 0 {
                    break;
                }
                time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();

        server.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn authenticated_identity_drives_the_bound_messenger_listener() {
        let profile_root = tempfile::tempdir().unwrap();
        let (server, maximum) = start_test_server(profile_root.path(), None).await;
        let endpoints = server.endpoints();
        let login = authenticate_and_login(endpoints.login_tcp, maximum, "MessengerRider").await;

        let mut messenger = TcpStream::connect(endpoints.messenger_tcp).await.unwrap();
        let mut enter = PacketWriter::named("PqEnterChatServer");
        enter.write_u32(login.user_no);
        enter.write_u32(2);
        enter.write_utf16("MessengerRider").unwrap();
        messenger
            .write_all(
                &encode_messenger_frame(enter.as_slice(), 256 * 1024)
                    .expect("the fixture enter frame is bounded"),
            )
            .await
            .unwrap();

        let mut guild = PacketWriter::named("PqGuildChat");
        guild.write_utf16("MessengerRider").unwrap();
        guild.write_utf16("through-bound-server").unwrap();
        messenger
            .write_all(
                &encode_messenger_frame(guild.as_slice(), 256 * 1024)
                    .expect("the fixture guild frame is bounded"),
            )
            .await
            .unwrap();

        let mut length = [0_u8; 4];
        time::timeout(Duration::from_secs(1), messenger.read_exact(&mut length))
            .await
            .expect("timed out waiting for messenger guild echo")
            .unwrap();
        let length = usize::try_from(i32::from_le_bytes(length)).unwrap();
        let mut logical = vec![0_u8; length];
        messenger.read_exact(&mut logical).await.unwrap();
        assert_eq!(
            logical,
            serialize_guild_chat("MessengerRider", "through-bound-server").unwrap()
        );

        drop(login);
        let mut byte = [0_u8; 1];
        assert_eq!(
            time::timeout(Duration::from_secs(1), messenger.read(&mut byte))
                .await
                .expect("messenger endpoint was not cancelled after identity release")
                .unwrap(),
            0
        );
        server.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn authenticated_identity_drives_bound_udp_and_release_fences_the_generation() {
        let profile_root = tempfile::tempdir().unwrap();
        let (server, maximum) = start_test_server(profile_root.path(), None).await;
        let endpoints = server.endpoints();
        let world = server.world();
        let login = authenticate_and_login(endpoints.login_tcp, maximum, "UdpRider").await;
        let udp = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();

        let echo = PqUdpEchoBody {
            value_1: i32::MIN + 5_136,
            value_2: -123_456_789,
        };
        send_bound_udp_request(
            &udp,
            endpoints.game_udp,
            login.user_no,
            0x1111_2222,
            UdpLogicalBody::PqUdpEcho(echo),
            7,
        )
        .await;
        let reply = receive_bound_udp(&udp, UdpTransport::Game).await;
        assert_eq!(reply.account_id, login.user_no);
        assert_eq!(reply.route_hash, 0x1111_2222);
        assert_eq!(reply.body, UdpIngressBody::PrUdpEcho(echo.reply()));

        let time_sync = PqUdpTimeSyncBody {
            client_tick: i32::MAX - 5_136,
        };
        send_bound_udp_request(
            &udp,
            endpoints.game_udp,
            login.user_no,
            0x3333_4444,
            UdpLogicalBody::PqUdpTimeSync(time_sync),
            8,
        )
        .await;
        let reply = receive_bound_udp(&udp, UdpTransport::Game).await;
        let UdpIngressBody::PrUdpTimeSync(time_reply) = reply.body else {
            panic!("bound time-sync request returned the wrong UDP packet");
        };
        assert_eq!(time_reply.client_tick, time_sync.client_tick);

        let user_no = login.user_no;
        drop(login);
        wait_for_session_count(&world, 0).await;
        send_bound_udp_request(
            &udp,
            endpoints.game_udp,
            user_no,
            0x5555_6666,
            UdpLogicalBody::PqUdpEcho(echo),
            9,
        )
        .await;
        assert_no_bound_udp(&udp).await;

        let replacement = authenticate_and_login(endpoints.login_tcp, maximum, "UdpRider").await;
        assert_eq!(replacement.user_no, user_no);
        send_bound_udp_request(
            &udp,
            endpoints.game_udp,
            replacement.user_no,
            0x7777_8888,
            UdpLogicalBody::PqUdpEcho(echo),
            10,
        )
        .await;
        assert_eq!(
            receive_bound_udp(&udp, UdpTransport::Game).await.body,
            UdpIngressBody::PrUdpEcho(echo.reply())
        );

        // Exercise shutdown while an active generation still owns a UDP
        // endpoint, rather than relying only on an empty-runtime fixture.
        server.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn invalid_configured_catalog_fails_before_any_listener_is_bound() {
        let profile_root = tempfile::tempdir().unwrap();
        let catalog_path = profile_root.path().join("invalid-catalog.xml");
        fs::write(&catalog_path, b"<not-a-kart-catalog />").unwrap();
        let config = ServerConfig {
            profile_root: profile_root.path().to_owned(),
            catalog_path: Some(catalog_path.clone()),
            ..ServerConfig::default()
        };

        let error = BoundServer::bind(config).await.unwrap_err();
        assert!(
            matches!(
                &error,
                ServerError::LoadCatalog { path, .. } if path == &catalog_path
            ),
            "unexpected bind error: {error}"
        );
    }

    #[tokio::test]
    async fn zero_login_session_limit_is_rejected_before_binding() {
        let error = BoundServer::bind(ServerConfig {
            max_login_sessions: 0,
            ..ServerConfig::default()
        })
        .await
        .unwrap_err();
        assert!(matches!(error, ServerError::InvalidLoginSessionLimit));
    }

    #[tokio::test]
    async fn oversized_login_session_limit_is_rejected_before_runtime_allocation() {
        let error = BoundServer::bind(ServerConfig {
            max_login_sessions: tokio::sync::Semaphore::MAX_PERMITS,
            ..ServerConfig::default()
        })
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            ServerError::ProfileIoConfig(
                ProfileIoConfigError::CapacityExceedsRuntimeLimit {
                    configured,
                    maximum,
                }
            ) if configured > maximum && maximum == tokio::sync::Semaphore::MAX_PERMITS
        ));
    }

    #[tokio::test]
    async fn concurrent_login_session_limit_rejects_excess_connections() {
        let loopback = Ipv4Addr::LOCALHOST;
        let profile_root = tempfile::tempdir().unwrap();
        let config = ServerConfig {
            bind_address: IpAddr::V4(loopback),
            advertised_address: loopback,
            profile_root: profile_root.path().to_owned(),
            first_message_delay: Duration::ZERO,
            login_timeout: Duration::from_secs(2),
            max_login_sessions: 1,
            ..ServerConfig::default()
        };
        let profiles = test_profile_bootstrap(&config);
        let bound = BoundServer {
            config,
            catalog: None,
            profiles,
            game_udp: UdpSocket::bind((loopback, 0)).await.unwrap(),
            login_tcp: TcpListener::bind((loopback, 0)).await.unwrap(),
            p2p_udp: UdpSocket::bind((loopback, 0)).await.unwrap(),
            messenger_tcp: TcpListener::bind((loopback, 0)).await.unwrap(),
        };
        let server = bound.start().unwrap();
        let endpoints = server.endpoints();

        let (first, _, _) = connect_login_client(endpoints.login_tcp).await;
        wait_for_session_count(&server.world(), 1).await;
        let mut excess = TcpStream::connect(endpoints.login_tcp).await.unwrap();
        assert_login_socket_closed(&mut excess).await;
        assert_eq!(server.world().session_count().await.unwrap(), 1);

        drop(first);
        wait_for_session_count(&server.world(), 0).await;
        server.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn profile_backed_startup_pairs_and_updates_flow_over_real_tcp() {
        let profile_root = tempfile::tempdir().unwrap();
        let mut profile = Profile::default();
        profile.rider.pmap = 0xaabb_ccdd;
        profile.rider.premium = 9;
        let mut initial_options = fixture_game_options(1);
        initial_options.set_screen = 7;
        apply_fixture_options(&mut profile, &initial_options);
        ProfileStore::new(profile_root.path())
            .save("ProfileRider", &profile)
            .unwrap();

        let (server, maximum) = start_test_server(profile_root.path(), None).await;
        let mut client =
            authenticate_and_login(server.endpoints().login_tcp, maximum, "ProfileRider").await;
        assert_eq!(client.pmap, 0xaabb_ccdd);
        assert_eq!(client.screen, 7);
        assert_no_login_data(&mut client.stream).await;

        assert_eq!(
            request_named(&mut client, "PqLoginVipInfo", maximum).await,
            serialize_pr_login_vip_info(9)
        );
        assert_no_login_data(&mut client.stream).await;
        assert_eq!(
            request_named(&mut client, "LoRqEventRewardPacket", maximum).await,
            serialize_lo_rp_event_reward()
        );
        assert_no_login_data(&mut client.stream).await;

        let add_time = request_named(&mut client, "PqAddTimeEventInitPacket", maximum).await;
        let response_time = LegacyTime {
            days_since_1900: u16::from_le_bytes([add_time[56], add_time[57]]),
            quarter_seconds: u16::from_le_bytes([add_time[58], add_time[59]]),
        };
        assert_eq!(add_time, serialize_pr_add_time_event_init(response_time));
        assert_eq!(
            request_named(&mut client, "ChRequestChStaticRequestPacket", maximum).await,
            serialize_channel_static_reply()
        );
        assert_eq!(
            request_named(&mut client, "PqGetGameOption", maximum).await,
            serialize_pr_get_game_option(&initial_options)
        );

        let updated_options = fixture_game_options(31);
        send_game_option_update(&mut client, &updated_options, maximum).await;
        assert_eq!(
            request_named(&mut client, "PqGetGameOption", maximum).await,
            serialize_pr_get_game_option(&updated_options)
        );

        let get_rider = PacketWriter::named("PqGetRider").into_inner();
        send_packet(&mut client.stream, &get_rider, &mut client.send_iv, maximum).await;
        assert_login_socket_closed(&mut client.stream).await;
        wait_for_session_count(&server.world(), 0).await;
        server.shutdown().await.unwrap();

        let persisted = ProfileStore::new(profile_root.path())
            .load_or_create("ProfileRider")
            .unwrap();
        assert_eq!(persisted.revision, Some(2));
        assert_eq!(
            persisted.profile.game_option.video_quality,
            updated_options.video_quality
        );
        assert_eq!(
            persisted.profile.game_option.screen,
            updated_options.set_screen
        );
    }

    #[tokio::test]
    async fn catalog_inventory_precedes_get_rider_and_late_request_is_a_noop() {
        let profile_root = tempfile::tempdir().unwrap();
        let catalog_path = profile_root.path().join("KartCatalog.xml");
        fs::write(&catalog_path, complete_catalog_xml()).unwrap();

        let nickname = "InventoryRider";
        let mut profile = Profile::default();
        profile.rider.emblem1 = -12_345;
        profile.rider.emblem2 = 23_456;
        profile.rider.lucci = 0xdead_beef;
        profile.rider.rp = 0xfedc_ba98;
        profile.rider.premium = 17;
        profile.rider_item.character = 0x0102;
        profile.rider_item.paint = 0x0304;
        profile.rider_item.kart = 1_450;
        profile.rider_item.pet = 0x0506;
        profile.rider_item.dye = 0x0708;
        profile.rider_item.kart_serial = 0;
        profile.rider_item.unknown4 = 0xa5;
        profile.rider_item.kart_coating = 0x090a;
        profile.rider_item.kart_tail_lamp = 0x0b0c;
        ProfileStore::new(profile_root.path())
            .save(nickname, &profile)
            .unwrap();

        let expected_rider = serialize_pr_get_rider(&PrGetRiderFields {
            nickname: nickname.to_owned(),
            emblem_1: u16::from_le_bytes(profile.rider.emblem1.to_le_bytes()),
            emblem_2: u16::from_le_bytes(profile.rider.emblem2.to_le_bytes()),
            rider_item_snapshot: rider_item_snapshot(&profile.rider_item),
            lucci: profile.rider.lucci,
            rp: i32::from_le_bytes(profile.rider.rp.to_le_bytes()),
        })
        .unwrap();

        let (server, maximum) = start_test_server(profile_root.path(), Some(&catalog_path)).await;
        let mut client =
            authenticate_and_login(server.endpoints().login_tcp, maximum, nickname).await;
        let get_rider = PacketWriter::named("PqGetRider").into_inner();
        send_packet(&mut client.stream, &get_rider, &mut client.send_iv, maximum).await;

        let mut final_rider = None;
        let mut response_count = 0_usize;
        while response_count < 256 {
            let packet = time::timeout(
                Duration::from_secs(5),
                read_encrypted_frame(&mut client.stream, &mut client.receive_iv, maximum),
            )
            .await
            .expect("timed out waiting for the catalog inventory sequence")
            .unwrap();
            response_count += 1;
            let mut reader = PacketReader::new(&packet);
            let hash = reader.read_u32().unwrap();
            if response_count == 1 {
                assert_eq!(hash, adler32::packet_hash("LoRpGetRiderItemPacket"));
                assert_eq!(reader.read_i32().unwrap(), 1);
                assert_eq!(reader.read_i32().unwrap(), 1);
                assert_eq!(reader.read_i32().unwrap(), 100);
                assert_eq!(reader.read_u16().unwrap(), 21);
                assert_eq!(reader.read_u16().unwrap(), 1);
            }
            if hash == adler32::packet_hash("PrGetRider") {
                final_rider = Some(packet);
                break;
            }
        }

        assert!(response_count > 1);
        assert_eq!(
            final_rider.expect("inventory sequence omitted final PrGetRider"),
            expected_rider
        );

        let late_request = PacketWriter::named("LoRqGetRiderItemPacket").into_inner();
        send_packet(
            &mut client.stream,
            &late_request,
            &mut client.send_iv,
            maximum,
        )
        .await;
        assert_no_login_data(&mut client.stream).await;
        assert_eq!(
            request_named(&mut client, "PqLoginVipInfo", maximum).await,
            serialize_pr_login_vip_info(17)
        );

        drop(client);
        wait_for_session_count(&server.world(), 0).await;
        server.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn encrypted_auth_login_and_channel_migration_complete_over_real_tcp() {
        let loopback = Ipv4Addr::LOCALHOST;
        let profile_root = tempfile::tempdir().unwrap();
        let config = ServerConfig {
            bind_address: IpAddr::V4(loopback),
            advertised_address: loopback,
            profile_root: profile_root.path().to_owned(),
            first_message_delay: Duration::ZERO,
            login_timeout: Duration::from_secs(2),
            ..ServerConfig::default()
        };
        let maximum = config.max_login_payload;
        let profiles = test_profile_bootstrap(&config);
        let bound = BoundServer {
            config,
            catalog: None,
            profiles,
            game_udp: UdpSocket::bind((loopback, 0)).await.unwrap(),
            login_tcp: TcpListener::bind((loopback, 0)).await.unwrap(),
            p2p_udp: UdpSocket::bind((loopback, 0)).await.unwrap(),
            messenger_tcp: TcpListener::bind((loopback, 0)).await.unwrap(),
        };
        let server = bound.start().unwrap();
        let endpoints = server.endpoints();

        let mut source = authenticate_and_login(endpoints.login_tcp, maximum, "Yany2").await;
        let (selected_channel, migration_token) = request_channel_switch(
            &mut source.stream,
            &mut source.send_iv,
            &mut source.receive_iv,
            maximum,
            12,
        )
        .await;
        assert_eq!(selected_channel, 12);

        // A second request replaces the first permit. Completing the older
        // token must neither transfer ownership nor cancel the source socket.
        let (latest_channel, latest_token) = request_channel_switch(
            &mut source.stream,
            &mut source.send_iv,
            &mut source.receive_iv,
            maximum,
            11,
        )
        .await;
        assert_eq!(latest_channel, 11);

        let (mut stale_destination, mut stale_send_iv, _) =
            connect_login_client(endpoints.login_tcp).await;
        let mut stale_move_in = PacketWriter::named("PqChannelMovein");
        stale_move_in.write_u32(source.user_no);
        stale_move_in.write_u16(selected_channel);
        stale_move_in.write_u16(migration_token);
        send_packet(
            &mut stale_destination,
            stale_move_in.as_slice(),
            &mut stale_send_iv,
            maximum,
        )
        .await;
        assert_login_socket_closed(&mut stale_destination).await;
        wait_for_session_count(&server.world(), 1).await;
        assert_login_socket_open(&mut source.stream).await;

        let (mut destination, mut destination_send_iv, mut destination_receive_iv) =
            connect_login_client(endpoints.login_tcp).await;
        let mut move_in = PacketWriter::named("PqChannelMovein");
        move_in.write_u32(source.user_no);
        move_in.write_u16(latest_channel);
        move_in.write_u16(latest_token);
        send_packet(
            &mut destination,
            move_in.as_slice(),
            &mut destination_send_iv,
            maximum,
        )
        .await;
        let move_in_reply =
            read_encrypted_frame(&mut destination, &mut destination_receive_iv, maximum)
                .await
                .unwrap();
        assert_eq!(move_in_reply, serialize_pr_channel_move_in(39_311, 39_312));

        let vip_request = PacketWriter::named("PqLoginVipInfo").into_inner();
        send_packet(
            &mut destination,
            &vip_request,
            &mut destination_send_iv,
            maximum,
        )
        .await;
        let vip_reply =
            read_encrypted_frame(&mut destination, &mut destination_receive_iv, maximum)
                .await
                .unwrap();
        assert_eq!(vip_reply, serialize_pr_login_vip_info(5));

        assert_login_socket_closed(&mut source.stream).await;
        wait_for_session_count(&server.world(), 1).await;
        drop(destination);
        wait_for_session_count(&server.world(), 0).await;
        server.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn two_real_tcp_clients_create_list_join_first_and_leave_a_room() {
        let profile_root = tempfile::tempdir().unwrap();
        let catalog_path = profile_root.path().join("KartCatalog.xml");
        fs::write(&catalog_path, complete_catalog_xml()).unwrap();
        let (server, maximum) = start_test_server(profile_root.path(), Some(&catalog_path)).await;
        let endpoint = server.endpoints().login_tcp;

        let owner_source = authenticate_and_login(endpoint, maximum, "Owner").await;
        let owner_user_no = owner_source.user_no;
        let mut owner = migrate_login_client(owner_source, endpoint, maximum).await;
        let joiner_source = authenticate_and_login(endpoint, maximum, "Joiner").await;
        let joiner_user_no = joiner_source.user_no;
        let mut joiner = migrate_login_client(joiner_source, endpoint, maximum).await;
        assert_ne!(owner_user_no, joiner_user_no);
        wait_for_session_count(&server.world(), 2).await;

        let create = build_create_room_request();
        send_packet(&mut owner.stream, &create, &mut owner.send_iv, maximum).await;
        assert_eq!(
            read_login_packet(&mut owner, maximum).await,
            serialize_ch_create_room_reply(CreateRoomOutcome::Created, 1)
        );

        let list = build_room_list_request();
        send_packet(&mut joiner.stream, &list, &mut joiner.send_iv, maximum).await;
        let list_reply = read_login_packet(&mut joiner, maximum).await;
        let mut reader = PacketReader::new(&list_reply);
        assert_eq!(
            reader.read_u32().unwrap(),
            adler32::packet_hash("ChGetRoomListReplyPacket")
        );
        assert_eq!(reader.read_i32().unwrap(), 0);
        assert_eq!(reader.read_i32().unwrap(), 1);
        let room_id = u16::from_le_bytes(reader.read_i16().unwrap().to_le_bytes());
        assert_ne!(room_id, 0);
        assert_eq!(reader.read_utf16().unwrap(), "Room");
        assert_eq!(reader.read_u32().unwrap(), 0);
        assert_eq!(reader.read_u8().unwrap(), 0);
        assert_eq!(reader.read_u8().unwrap(), 1);
        assert_eq!(reader.read_u8().unwrap(), 7);
        assert_eq!(reader.read_u8().unwrap(), 0);
        assert_eq!(reader.read_u8().unwrap(), 8);
        assert_eq!(reader.read_u8().unwrap(), 1);
        assert_eq!(reader.read_bytes(2).unwrap(), [0, 0]);
        assert!(reader.remaining().is_empty());

        let join = build_join_room_request(room_id);
        send_packet(&mut joiner.stream, &join, &mut joiner.send_iv, maximum).await;
        assert_eq!(
            read_login_packet(&mut joiner, maximum).await,
            serialize_ch_join_room_reply(JoinRoomStatus::Success, 1)
        );
        assert_no_login_data(&mut owner.stream).await;

        let first = PacketWriter::named("GrFirstRequestPacket").into_inner();
        let owner_first_wire =
            frame::encode_encrypted(&first, &mut owner.send_iv, maximum).unwrap();
        let split = owner_first_wire.len() / 2;
        owner
            .stream
            .write_all(&owner_first_wire[..split])
            .await
            .unwrap();
        time::sleep(Duration::from_millis(10)).await;
        send_packet(&mut joiner.stream, &first, &mut joiner.send_iv, maximum).await;
        let session_data = read_login_packet(&mut joiner, maximum).await;
        assert_eq!(
            u32::from_le_bytes(session_data[..4].try_into().unwrap()),
            adler32::packet_hash("GrSessionDataPacket")
        );
        let joiner_slots = read_login_packet(&mut joiner, maximum).await;
        let owner_slots = read_login_packet(&mut owner, maximum).await;
        assert_eq!(
            u32::from_le_bytes(joiner_slots[..4].try_into().unwrap()),
            adler32::packet_hash("GrSlotDataPacket")
        );
        assert_eq!(owner_slots, joiner_slots);

        // The owner session had already consumed half of this encrypted frame
        // when the joiner's slot broadcast arrived. Finishing it proves the
        // read future remained pinned across the outbound write.
        owner
            .stream
            .write_all(&owner_first_wire[split..])
            .await
            .unwrap();
        let owner_session_data = read_login_packet(&mut owner, maximum).await;
        let owner_own_slots = read_login_packet(&mut owner, maximum).await;
        let joiner_peer_slots = read_login_packet(&mut joiner, maximum).await;
        assert_eq!(owner_session_data, session_data);
        assert_eq!(owner_own_slots, joiner_slots);
        assert_eq!(joiner_peer_slots, joiner_slots);

        exercise_equipment_over_real_tcp(&mut owner, &mut joiner, profile_root.path(), maximum)
            .await;

        let leave = build_leave_room_request();
        send_packet(&mut joiner.stream, &leave, &mut joiner.send_iv, maximum).await;
        assert_eq!(
            read_login_packet(&mut joiner, maximum).await,
            serialize_ch_leave_room_reply(true)
        );
        let owner_after_leave = read_login_packet(&mut owner, maximum).await;
        assert_eq!(
            u32::from_le_bytes(owner_after_leave[..4].try_into().unwrap()),
            adler32::packet_hash("GrSlotDataPacket")
        );
        assert_ne!(owner_after_leave, owner_slots);

        drop(joiner);
        drop(owner);
        wait_for_session_count(&server.world(), 0).await;
        server.shutdown().await.unwrap();
    }

    async fn exercise_equipment_over_real_tcp(
        owner: &mut LoginClient,
        joiner: &mut LoginClient,
        profile_root: &Path,
        maximum: usize,
    ) {
        let rider_update = build_rider_item_update();
        let expected_snapshot: [u8; 65] = rider_update[4..].try_into().unwrap();
        send_packet(
            &mut owner.stream,
            &rider_update,
            &mut owner.send_iv,
            maximum,
        )
        .await;
        let peer_equipment = read_login_packet(joiner, maximum).await;
        assert_eq!(
            u32::from_le_bytes(peer_equipment[..4].try_into().unwrap()),
            adler32::packet_hash("GrSlotItemOnPacket")
        );
        assert_eq!(
            i32::from_le_bytes(peer_equipment[4..8].try_into().unwrap()),
            0
        );
        assert_eq!(&peer_equipment[8..], expected_snapshot);
        assert_no_login_data(&mut owner.stream).await;
        let persisted = ProfileStore::new(profile_root)
            .load_or_create("Owner")
            .unwrap();
        assert_eq!(persisted.revision, Some(2));
        assert_eq!(
            rider_item_snapshot(&persisted.profile.rider_item),
            expected_snapshot
        );

        let plant_request = PlantPartEquipRequest {
            item_category: 43,
            item_id: 1,
            kart_category: 3,
            kart_id: 1,
            kart_serial: 1,
        };
        let plant_packet = build_plant_part_request(plant_request, false);
        send_packet(
            &mut owner.stream,
            &plant_packet,
            &mut owner.send_iv,
            maximum,
        )
        .await;
        assert_eq!(
            read_login_packet(owner, maximum).await,
            serialize_equip_tuning_success(plant_request)
        );
        let rider_directory = persisted.source_path.parent().unwrap();
        let equipment = EquipmentExceptions::load(profile_root, rider_directory).unwrap();
        assert_eq!(equipment.plant.len(), 1);
        assert_eq!(equipment.plant[0].engine_category, 43);
        assert_eq!(equipment.plant[0].engine_id, 1);

        let malformed_plant = build_plant_part_request(plant_request, true);
        send_packet(
            &mut owner.stream,
            &malformed_plant,
            &mut owner.send_iv,
            maximum,
        )
        .await;
        assert_eq!(
            read_login_packet(owner, maximum).await,
            serialize_equip_tuning_failure()
        );
        assert_eq!(
            request_named(owner, "PqLoginVipInfo", maximum).await,
            serialize_pr_login_vip_info(5)
        );
    }

    async fn start_test_server(
        profile_root: &Path,
        catalog_path: Option<&Path>,
    ) -> (ServerHandle, usize) {
        let loopback = Ipv4Addr::LOCALHOST;
        let config = ServerConfig {
            bind_address: IpAddr::V4(loopback),
            advertised_address: loopback,
            profile_root: profile_root.to_owned(),
            catalog_path: catalog_path.map(Path::to_owned),
            first_message_delay: Duration::ZERO,
            login_timeout: Duration::from_secs(2),
            ..ServerConfig::default()
        };
        let maximum = config.max_login_payload;
        let catalog = load_catalog(config.catalog_path.clone()).await.unwrap();
        let profiles = test_profile_bootstrap(&config);
        let bound = BoundServer {
            config,
            catalog,
            profiles,
            game_udp: UdpSocket::bind((loopback, 0)).await.unwrap(),
            login_tcp: TcpListener::bind((loopback, 0)).await.unwrap(),
            p2p_udp: UdpSocket::bind((loopback, 0)).await.unwrap(),
            messenger_tcp: TcpListener::bind((loopback, 0)).await.unwrap(),
        };
        (bound.start().unwrap(), maximum)
    }

    fn test_profile_bootstrap(config: &ServerConfig) -> ProfileIoBootstrap {
        let limits =
            ProfileIoLimits::for_tests(config.max_login_sessions, config.max_login_sessions);
        ProfileIoBootstrap::acquire(config.profile_root.clone(), limits)
            .expect("test server should acquire its isolated profile-store lease")
    }

    async fn send_bound_udp_request(
        socket: &UdpSocket,
        endpoint: SocketAddr,
        account_id: u32,
        route_hash: u32,
        body: UdpLogicalBody<'_>,
        iv: u32,
    ) {
        let logical = encode_routed_udp_packet(&RoutedUdpPacket {
            account_id,
            route_hash,
            body,
        })
        .unwrap();
        let wire = encode_datagram(&logical, iv, DEFAULT_MAX_DATAGRAM_PAYLOAD).unwrap();
        socket.send_to(&wire, endpoint).await.unwrap();
    }

    async fn receive_bound_udp(socket: &UdpSocket, transport: UdpTransport) -> UdpIngress {
        let mut wire = vec![0_u8; 65_535];
        let (length, source) = time::timeout(Duration::from_secs(2), socket.recv_from(&mut wire))
            .await
            .expect("timed out waiting for the bound UDP runtime")
            .unwrap();
        decode_udp_ingress(
            transport,
            source,
            &wire[..length],
            DEFAULT_MAX_DATAGRAM_PAYLOAD,
        )
        .unwrap()
    }

    async fn assert_no_bound_udp(socket: &UdpSocket) {
        let mut wire = vec![0_u8; 65_535];
        assert!(
            time::timeout(Duration::from_millis(100), socket.recv_from(&mut wire))
                .await
                .is_err(),
            "released UDP generation unexpectedly received a response"
        );
    }

    async fn migrate_login_client(
        mut source: LoginClient,
        endpoint: SocketAddr,
        maximum: usize,
    ) -> LoginClient {
        let (channel, token) = request_channel_switch(
            &mut source.stream,
            &mut source.send_iv,
            &mut source.receive_iv,
            maximum,
            12,
        )
        .await;
        let (mut stream, mut send_iv, mut receive_iv) = connect_login_client(endpoint).await;
        let mut move_in = PacketWriter::named("PqChannelMovein");
        move_in.write_u32(source.user_no);
        move_in.write_u16(channel);
        move_in.write_u16(token);
        send_packet(&mut stream, move_in.as_slice(), &mut send_iv, maximum).await;
        assert_eq!(
            read_encrypted_frame(&mut stream, &mut receive_iv, maximum)
                .await
                .unwrap(),
            serialize_pr_channel_move_in(39_311, 39_312)
        );
        assert_login_socket_closed(&mut source.stream).await;
        LoginClient {
            stream,
            send_iv,
            receive_iv,
            user_no: source.user_no,
            pmap: source.pmap,
            screen: source.screen,
        }
    }

    fn build_room_list_request() -> Vec<u8> {
        let mut packet = PacketWriter::named("ChGetRoomListRequestPacket");
        packet.write_i32(0);
        packet.write_u8(1);
        packet.write_u8(0);
        packet.into_inner()
    }

    fn build_create_room_request() -> Vec<u8> {
        let mut packet = PacketWriter::named("ChCreateRoomRequestPacket");
        packet.write_utf16("Room").unwrap();
        packet.write_utf16("").unwrap();
        packet.write_encoded_u8(1);
        packet.write_i32(0);
        packet.write_i32(0);
        packet.write_u32(0xaabb_ccdd);
        packet.write_bytes(&[0; 32]);
        packet.write_bytes(&[0; 28]);
        packet.write_u8(0);
        packet.write_i32(0);
        packet.write_u8(0);
        packet.write_u8(0);
        packet.write_i32(0);
        packet.write_u8(0);
        packet.into_inner()
    }

    fn build_join_room_request(room_id: u16) -> Vec<u8> {
        let mut packet = PacketWriter::named("ChJoinRoomRequestPacket");
        packet.write_u16(room_id);
        packet.write_utf16("").unwrap();
        packet.write_u8(0);
        packet.write_bytes(&[0; 28]);
        packet.into_inner()
    }

    fn build_leave_room_request() -> Vec<u8> {
        let mut packet = PacketWriter::named("ChLeaveRoomRequestPacket");
        packet.write_u8(0);
        packet.into_inner()
    }

    fn build_rider_item_update() -> Vec<u8> {
        let mut packet = PacketWriter::named("LoRqSetRiderItemOnPacket");
        let mut items = [0_u16; 30];
        items[0] = 1;
        items[1] = 1;
        items[2] = 1;
        items[29] = 1;
        for item in items {
            packet.write_u16(item);
        }
        packet.write_u8(0);
        packet.write_u16(0);
        packet.write_u16(0);
        packet.into_inner()
    }

    fn build_plant_part_request(request: PlantPartEquipRequest, trailing: bool) -> Vec<u8> {
        let mut packet = PacketWriter::named("PqEquipTuningExPacket");
        packet.write_i16(request.item_category);
        packet.write_i16(request.item_id);
        packet.write_i16(request.kart_category);
        packet.write_i16(request.kart_id);
        packet.write_i16(request.kart_serial);
        if trailing {
            packet.write_u8(0xff);
        }
        packet.into_inner()
    }

    async fn read_login_packet(client: &mut LoginClient, maximum: usize) -> Vec<u8> {
        time::timeout(
            Duration::from_secs(2),
            read_encrypted_frame(&mut client.stream, &mut client.receive_iv, maximum),
        )
        .await
        .expect("timed out waiting for a login-server packet")
        .unwrap()
    }

    async fn request_named(
        client: &mut LoginClient,
        request_name: &str,
        maximum: usize,
    ) -> Vec<u8> {
        let request = PacketWriter::named(request_name).into_inner();
        send_packet(&mut client.stream, &request, &mut client.send_iv, maximum).await;
        read_encrypted_frame(&mut client.stream, &mut client.receive_iv, maximum)
            .await
            .unwrap()
    }

    async fn assert_no_login_data(stream: &mut TcpStream) {
        let mut byte = [0_u8; 1];
        assert!(
            time::timeout(Duration::from_millis(50), stream.read(&mut byte))
                .await
                .is_err(),
            "server sent a startup reply before the matching request"
        );
    }

    async fn send_game_option_update(
        client: &mut LoginClient,
        options: &GameOptions,
        maximum: usize,
    ) {
        let mut request = PacketWriter::named("PqUpdateGameOption");
        request.write_f32(options.bgm_volume);
        request.write_f32(options.sound_volume);
        request.write_bytes(&[
            options.main_bgm,
            options.sound_effect,
            options.full_screen,
            options.show_mirror,
            options.show_other_player_names,
            options.show_outlines,
            options.show_shadows,
            options.high_level_effect,
            options.motion_blur_effect,
            options.motion_distortion_effect,
            options.high_end_optimization,
            options.auto_ready,
            options.prop_description,
            options.video_quality,
            options.bgm_check,
            options.sound_check,
            options.show_hit_info,
            options.auto_boost,
            options.game_type,
            options.set_ghost,
            options.speed_type,
            options.room_chat,
            options.driving_chat,
            options.show_all_player_hit_info,
            options.show_team_color,
            options.set_screen,
            options.hide_competitive_rank,
        ]);
        send_packet(
            &mut client.stream,
            request.as_slice(),
            &mut client.send_iv,
            maximum,
        )
        .await;
    }

    fn fixture_game_options(offset: u8) -> GameOptions {
        GameOptions {
            bgm_volume: f32::from(offset) / 100.0,
            sound_volume: f32::from(offset) / 200.0,
            main_bgm: offset,
            sound_effect: offset + 1,
            full_screen: offset + 2,
            show_mirror: offset + 3,
            show_other_player_names: offset + 4,
            show_outlines: offset + 5,
            show_shadows: offset + 6,
            high_level_effect: offset + 7,
            motion_blur_effect: offset + 8,
            motion_distortion_effect: offset + 9,
            high_end_optimization: offset + 10,
            auto_ready: offset + 11,
            prop_description: offset + 12,
            video_quality: offset + 13,
            bgm_check: offset + 14,
            sound_check: offset + 15,
            show_hit_info: offset + 16,
            auto_boost: offset + 17,
            game_type: offset + 18,
            set_ghost: offset + 19,
            speed_type: offset + 20,
            room_chat: offset + 21,
            driving_chat: offset + 22,
            show_all_player_hit_info: offset + 23,
            show_team_color: offset + 24,
            set_screen: offset + 25,
            hide_competitive_rank: offset + 26,
        }
    }

    fn apply_fixture_options(profile: &mut Profile, options: &GameOptions) {
        let destination = &mut profile.game_option;
        destination.bgm_volume = options.bgm_volume;
        destination.sound_volume = options.sound_volume;
        destination.main_bgm = options.main_bgm;
        destination.sound_effect = options.sound_effect;
        destination.full_screen = options.full_screen;
        destination.show_mirror = options.show_mirror;
        destination.show_other_player_names = options.show_other_player_names;
        destination.show_outlines = options.show_outlines;
        destination.show_shadows = options.show_shadows;
        destination.high_level_effect = options.high_level_effect;
        destination.motion_blur_effect = options.motion_blur_effect;
        destination.motion_distortion_effect = options.motion_distortion_effect;
        destination.high_end_optimization = options.high_end_optimization;
        destination.auto_ready = options.auto_ready;
        destination.prop_description = options.prop_description;
        destination.video_quality = options.video_quality;
        destination.bgm_check = options.bgm_check;
        destination.sound_check = options.sound_check;
        destination.show_hit_info = options.show_hit_info;
        destination.auto_boost = options.auto_boost;
        destination.game_type = options.game_type;
        destination.set_ghost = options.set_ghost;
        destination.speed_type = options.speed_type;
        destination.room_chat = options.room_chat;
        destination.driving_chat = options.driving_chat;
        destination.show_all_player_hit_info = options.show_all_player_hit_info;
        destination.show_team_color = options.show_team_color;
        destination.screen = options.set_screen;
        destination.hide_competitive_rank = options.hide_competitive_rank;
    }

    fn complete_catalog_xml() -> String {
        const GRANT_CATEGORIES: &[u16] = &[
            1, 2, 3, 4, 7, 8, 9, 11, 12, 13, 14, 16, 18, 20, 21, 22, 23, 26, 27, 28, 30, 31, 32,
            36, 37, 38, 39, 43, 44, 45, 46, 49, 52, 53, 55, 59, 61, 67, 68, 69, 70,
        ];
        const OTHER_CATEGORIES: &[u16] = &[
            5, 6, 10, 15, 17, 19, 24, 25, 29, 33, 34, 35, 40, 41, 42, 47, 48, 50, 51,
        ];

        let mut items = Vec::new();
        for &category in GRANT_CATEGORIES {
            let count = if category == 3 { 1_300 } else { 120 };
            for id in 1..=count {
                items.push((category, id));
            }
        }
        items.push((3, 1_450));
        items.push((3, 1_453));
        for &category in OTHER_CATEGORIES {
            for id in 1..=40 {
                items.push((category, id));
            }
        }

        let mut xml = format!(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr"><Inventory total="{}" categories="60">"#,
            items.len()
        );
        for (category, id) in items {
            write!(xml, r#"<Item category="{category}" id="{id}" />"#).unwrap();
        }
        xml.push_str("</Inventory></KartCatalog>");
        xml
    }

    async fn authenticate_and_login(
        endpoint: std::net::SocketAddr,
        maximum: usize,
        nickname: &str,
    ) -> LoginClient {
        let (mut source, mut send_iv, mut receive_iv) = connect_login_client(endpoint).await;
        let auth_request = PacketWriter::named("PqCnAuthenLogin").into_inner();
        send_packet(&mut source, &auth_request, &mut send_iv, maximum).await;
        let auth_reply = read_encrypted_frame(&mut source, &mut receive_iv, maximum)
            .await
            .unwrap();
        assert_eq!(auth_reply, serialize_pr_cn_authen_login().unwrap());

        let login_request = build_login_request(nickname);
        send_packet(&mut source, &login_request, &mut send_iv, maximum).await;
        let login_reply = read_encrypted_frame(&mut source, &mut receive_iv, maximum)
            .await
            .unwrap();
        let mut reader = PacketReader::new(&login_reply);
        assert_eq!(reader.read_u32().unwrap(), adler32::packet_hash("PrLogin"));
        assert_eq!(reader.read_i32().unwrap(), 0);
        let _days = reader.read_u16().unwrap();
        let _quarter_seconds = reader.read_u16().unwrap();
        let user_no = reader.read_u32().unwrap();
        assert_ne!(user_no, 0);
        assert_eq!(reader.read_utf16().unwrap(), nickname);
        reader.read_bytes(12).unwrap();
        let pmap = reader.read_u32().unwrap();
        reader.read_bytes(45).unwrap();
        reader.read_bytes(12).unwrap();
        reader.read_i32().unwrap();
        assert!(reader.read_utf16().unwrap().is_empty());
        reader.read_i32().unwrap();
        reader.read_u8().unwrap();
        assert_eq!(reader.read_utf16().unwrap(), "content");
        reader.read_i32().unwrap();
        reader.read_i32().unwrap();
        assert_eq!(reader.read_utf16().unwrap(), "cc");
        assert_eq!(reader.read_utf16().unwrap(), "kr");
        reader.read_i32().unwrap();
        reader.read_u8().unwrap();
        let screen = reader.read_u8().unwrap();
        assert!(reader.remaining().is_empty());
        LoginClient {
            stream: source,
            send_iv,
            receive_iv,
            user_no,
            pmap,
            screen,
        }
    }

    async fn request_channel_switch(
        source: &mut TcpStream,
        send_iv: &mut u32,
        receive_iv: &mut u32,
        maximum: usize,
        preferred_channel: u16,
    ) -> (u16, u16) {
        let mut request = PacketWriter::named("PqChannelSwitch");
        request.write_i32(0);
        request.write_u8(67);
        request.write_u16(preferred_channel);
        send_packet(source, request.as_slice(), send_iv, maximum).await;

        let reply = read_encrypted_frame(source, receive_iv, maximum)
            .await
            .unwrap();
        let mut reader = PacketReader::new(&reply);
        assert_eq!(
            reader.read_u32().unwrap(),
            adler32::packet_hash("PrChannelSwitch")
        );
        assert_eq!(reader.read_i32().unwrap(), 0);
        let channel = reader.read_u16().unwrap();
        let token = reader.read_u16().unwrap();
        assert_ne!(token, 0);
        assert_eq!(reader.read_bytes(4).unwrap(), Ipv4Addr::LOCALHOST.octets());
        assert_eq!(reader.read_u16().unwrap(), 39_312);
        assert!(reader.remaining().is_empty());
        (channel, token)
    }

    async fn connect_login_client(endpoint: std::net::SocketAddr) -> (TcpStream, u32, u32) {
        let mut stream = TcpStream::connect(endpoint).await.unwrap();
        let mut length_bytes = [0_u8; 4];
        stream.read_exact(&mut length_bytes).await.unwrap();
        let length = usize::try_from(u32::from_le_bytes(length_bytes)).unwrap();
        let mut payload = vec![0_u8; length];
        stream.read_exact(&mut payload).await.unwrap();
        assert_eq!(payload, handshake::first_message_payload().unwrap());
        (stream, handshake::initial_iv(), handshake::initial_iv())
    }

    async fn send_packet(stream: &mut TcpStream, packet: &[u8], send_iv: &mut u32, maximum: usize) {
        let wire = frame::encode_encrypted(packet, send_iv, maximum).unwrap();
        stream.write_all(&wire).await.unwrap();
    }

    async fn assert_login_socket_closed(stream: &mut TcpStream) {
        let mut byte = [0_u8; 1];
        let result = time::timeout(Duration::from_secs(1), stream.read(&mut byte))
            .await
            .expect("superseded login socket remained open");
        match result {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::BrokenPipe
                ) => {}
            Ok(length) => panic!("superseded login socket received {length} unexpected bytes"),
            Err(error) => panic!("unexpected superseded-socket read error: {error}"),
        }
    }

    async fn assert_login_socket_open(stream: &mut TcpStream) {
        let mut byte = [0_u8; 1];
        assert!(
            time::timeout(Duration::from_millis(50), stream.read(&mut byte))
                .await
                .is_err(),
            "source login socket closed or received unexpected data after a stale migration"
        );
    }

    fn build_login_request(nickname: &str) -> Vec<u8> {
        let mut packet = PacketWriter::named("PqLogin");
        packet.write_u32(0x8b01_9610);
        packet.write_u32(0xba06_b093);
        packet.write_u32(adler32::packet_hash("AccountDataProfile"));
        packet.write_u8(0);
        let mut profile = BmlNode::new("profile", "");
        profile.children.push(BmlNode::new("username", nickname));
        profile.encode(&mut packet).unwrap();
        packet.into_inner()
    }

    async fn wait_for_session_count(world: &crate::WorldHandle, expected: usize) {
        time::timeout(Duration::from_secs(1), async {
            loop {
                if world.session_count().await.unwrap() == expected {
                    break;
                }
                time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
}
