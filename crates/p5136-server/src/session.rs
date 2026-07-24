use std::{
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::Arc,
    time::Instant,
};

use chrono::{Local, NaiveDate, Timelike};
use p5136_core::{
    adler32,
    channel::{
        ChannelError, parse_pq_channel_movein, parse_pq_channel_switch, resolve_channel_id,
        serialize_pr_channel_move_in, serialize_pr_channel_switch,
    },
    frame::{self, FrameError},
    handshake,
    inventory::{InventoryError, serialize_get_rider_sequence},
    login::{
        LegacyTime, LoginError, PrLoginFields, parse_pq_login, serialize_pr_cn_authen_login,
        serialize_pr_login,
    },
    packet::PacketError,
    room_protocol::{
        RoomPlayer, RoomProtocolError, RoomProtocolRequest, classify_room_protocol_request,
        parse_ch_create_room_request, parse_ch_get_room_list_request, parse_ch_join_room_request,
        parse_ch_leave_room_request, parse_gr_first_request,
    },
    startup::{
        self, PrGetRiderFields, StartupError, StartupRequest, classify_startup_request,
        is_startup_noop, parse_pq_update_game_option,
    },
};
use p5136_profile::{
    CatalogInventory, EquipmentExceptions, EquipmentStateError, InventoryBuildError, Profile,
    ProfileStore, ProfileStoreError, build_inventory_snapshot_with_equipment, rider_item_snapshot,
};
use rand::Rng;
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    sync::{Mutex, OwnedMutexGuard, mpsc, oneshot},
    task::JoinError,
    time,
};

use crate::{
    ChannelBinding, IdentityBinding, MigrationToken, ServerConfig, SessionId, UserNo, WorldError,
    WorldHandle,
    world::{OutboundBatch, RoomCommandPayload, RoomParticipant},
};

#[derive(Debug, Error)]
pub enum LoginSessionError {
    #[error("login socket I/O failed")]
    Io(#[from] io::Error),

    #[error(transparent)]
    Frame(#[from] FrameError),

    #[error(transparent)]
    Packet(#[from] PacketError),

    #[error(transparent)]
    LoginProtocol(#[from] LoginError),

    #[error(transparent)]
    ChannelProtocol(#[from] ChannelError),

    #[error(transparent)]
    StartupProtocol(#[from] StartupError),

    #[error(transparent)]
    ProfileStore(#[from] ProfileStoreError),

    #[error(transparent)]
    EquipmentState(#[from] EquipmentStateError),

    #[error(transparent)]
    InventoryBuild(#[from] InventoryBuildError),

    #[error(transparent)]
    InventoryProtocol(#[from] InventoryError),

    #[error(transparent)]
    RoomProtocol(#[from] RoomProtocolError),

    #[error("profile worker task failed")]
    ProfileWorker(#[from] JoinError),

    #[error(transparent)]
    World(#[from] WorldError),

    #[error("client did not complete login before the login timeout")]
    LoginTimeout,

    #[error("authenticated login session exceeded its idle timeout")]
    SessionIdleTimeout,

    #[error("login session write exceeded its timeout")]
    WriteTimeout,

    #[error("logical login packet is shorter than its four-byte name hash")]
    MissingPacketHash,

    #[error(
        "P5136 static channel catalog has no record for game type {game_type} and preferred channel {preferred_channel}"
    )]
    UnsupportedChannel {
        game_type: u8,
        preferred_channel: u16,
    },

    #[error("PqChannelMovein contains invalid zero user number")]
    InvalidUserNo,

    #[error("PqChannelMovein contains invalid zero migration token")]
    InvalidMigrationToken,

    #[error("login session was superseded by a newer channel generation")]
    Superseded,

    #[error("the world actor closed the login session's outbound queue")]
    OutboundClosed,

    #[error("the session has no profile bound to its current identity generation")]
    ProfileNotBound,

    #[error("PqGetRider requires a configured and validated inventory catalog")]
    CatalogUnavailable,

    #[error("the bound profile path has no rider directory")]
    ProfileDirectoryUnavailable,

    #[error("profile {nickname:?} does not exist and remote profile creation is disabled")]
    ProfileCreationDenied { nickname: String },
}

/// Shared persistence and ownership-transfer coordination.
///
/// Disk mutations and migration completion take the same asynchronous gate.
/// This prevents an old generation from publishing a profile revision while a
/// destination session takes ownership. The actual filesystem work always
/// runs on Tokio's blocking pool.
#[derive(Debug, Clone)]
pub(crate) struct ProfileCoordinator {
    store: Arc<ProfileStore>,
    catalog: Option<Arc<CatalogInventory>>,
    ownership_gate: Arc<Mutex<()>>,
    #[cfg(test)]
    blocking_update_hook: Option<Arc<BlockingUpdateHook>>,
}

#[cfg(test)]
#[derive(Debug)]
struct BlockingUpdateHook {
    entered: std::sync::Barrier,
    release: std::sync::Barrier,
}

#[cfg(test)]
impl BlockingUpdateHook {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: std::sync::Barrier::new(2),
            release: std::sync::Barrier::new(2),
        })
    }
}

impl ProfileCoordinator {
    #[must_use]
    pub(crate) fn new(root: PathBuf, catalog: Option<Arc<CatalogInventory>>) -> Self {
        Self {
            store: Arc::new(ProfileStore::new(root)),
            catalog,
            ownership_gate: Arc::new(Mutex::new(())),
            #[cfg(test)]
            blocking_update_hook: None,
        }
    }

    #[cfg(test)]
    fn with_blocking_update_hook(mut self, hook: Arc<BlockingUpdateHook>) -> Self {
        self.blocking_update_hook = Some(hook);
        self
    }

    async fn ownership_guard(&self) -> OwnedMutexGuard<()> {
        Arc::clone(&self.ownership_gate).lock_owned().await
    }

    async fn load(
        &self,
        nickname: String,
        allow_creation: bool,
        ownership_guard: OwnedMutexGuard<()>,
    ) -> Result<ProfileSnapshot, LoginSessionError> {
        let store = Arc::clone(&self.store);
        let loaded = tokio::task::spawn_blocking(move || -> Result<_, LoginSessionError> {
            let _ownership_guard = ownership_guard;
            if !allow_creation && !store.profile_exists(&nickname)? {
                return Err(LoginSessionError::ProfileCreationDenied { nickname });
            }
            Ok(store.load_or_create(&nickname)?)
        })
        .await??;
        Ok(ProfileSnapshot {
            profile: loaded.profile,
            source_path: loaded.source_path,
        })
    }

    async fn update_game_options(
        &self,
        nickname: String,
        options: startup::GameOptions,
        ownership_guard: OwnedMutexGuard<()>,
    ) -> Result<ProfileSnapshot, LoginSessionError> {
        let store = Arc::clone(&self.store);
        #[cfg(test)]
        let blocking_update_hook = self.blocking_update_hook.clone();
        let (saved, profile) = tokio::task::spawn_blocking(move || {
            let _ownership_guard = ownership_guard;
            #[cfg(test)]
            if let Some(hook) = blocking_update_hook {
                hook.entered.wait();
                hook.release.wait();
            }
            store.update(&nickname, |profile| {
                apply_game_options(&mut profile.game_option, &options);
            })
        })
        .await??;
        Ok(ProfileSnapshot {
            profile,
            source_path: saved.path,
        })
    }

    async fn get_rider_sequence(
        &self,
        nickname: String,
        profile: ProfileSnapshot,
        ownership_guard: OwnedMutexGuard<()>,
    ) -> Result<Vec<Vec<u8>>, LoginSessionError> {
        let catalog = self
            .catalog
            .clone()
            .ok_or(LoginSessionError::CatalogUnavailable)?;
        let profile_root = self.store.root().to_owned();
        let rider_directory = profile
            .source_path
            .parent()
            .map(std::path::Path::to_owned)
            .ok_or(LoginSessionError::ProfileDirectoryUnavailable)?;

        tokio::task::spawn_blocking(move || {
            let _ownership_guard = ownership_guard;
            let equipment = EquipmentExceptions::load(profile_root, rider_directory)?;
            let inventory =
                build_inventory_snapshot_with_equipment(&catalog, &profile.profile, equipment)?;
            let rider = profile_rider_fields(nickname, &profile.profile);
            Ok(serialize_get_rider_sequence(&inventory, &rider)?
                .into_iter()
                .map(|packet| packet.logical_packet)
                .collect())
        })
        .await?
    }
}

#[derive(Debug, Clone)]
struct ProfileSnapshot {
    profile: Profile,
    source_path: PathBuf,
}

#[derive(Debug, Default)]
struct SessionContext {
    profile: Option<BoundProfile>,
}

impl SessionContext {
    fn is_authenticated(&self) -> bool {
        self.profile.is_some()
    }

    fn bind_profile(&mut self, identity: IdentityBinding, profile: ProfileSnapshot) {
        self.profile = Some(BoundProfile { identity, profile });
    }

    fn profile_for(&self, identity: &IdentityBinding) -> Result<&Profile, LoginSessionError> {
        Ok(&self.bound_profile_for(identity)?.profile.profile)
    }

    fn bound_profile_for(
        &self,
        identity: &IdentityBinding,
    ) -> Result<&BoundProfile, LoginSessionError> {
        self.profile
            .as_ref()
            .filter(|bound| bound.identity.owner == identity.owner)
            .filter(|bound| bound.identity.user_no == identity.user_no)
            .filter(|bound| bound.identity.generation == identity.generation)
            .ok_or(LoginSessionError::ProfileNotBound)
    }
}

#[derive(Debug)]
struct BoundProfile {
    identity: IdentityBinding,
    profile: ProfileSnapshot,
}

#[derive(Debug, Clone, Copy)]
struct SessionServices<'a> {
    config: &'a ServerConfig,
    world: &'a WorldHandle,
    profiles: &'a ProfileCoordinator,
    session_id: SessionId,
}

/// Reads exactly one encrypted frame from an arbitrary async byte stream.
///
/// The encoded length is validated before the body allocation. `read_exact`
/// makes the function insensitive to TCP fragmentation and coalescing.
pub async fn read_encrypted_frame<R>(
    reader: &mut R,
    iv: &mut u32,
    maximum: usize,
) -> Result<Vec<u8>, LoginSessionError>
where
    R: AsyncRead + Unpin,
{
    let mut header = [0_u8; 4];
    reader.read_exact(&mut header).await?;
    let encoded_header = u32::from_le_bytes(header);
    let body_length = frame::encrypted_body_length(encoded_header, *iv, maximum)?;

    let mut wire = Vec::with_capacity(body_length + 4);
    wire.extend_from_slice(&header);
    wire.resize(body_length + 4, 0);
    reader.read_exact(&mut wire[4..]).await?;
    Ok(frame::decode_encrypted(&wire, iv, maximum)?)
}

async fn read_session_frame<R>(
    reader: &mut R,
    iv: &mut u32,
    maximum: usize,
    authenticated: bool,
    login_deadline: time::Instant,
    idle_timeout: std::time::Duration,
) -> Result<Vec<u8>, LoginSessionError>
where
    R: AsyncRead + Unpin,
{
    if authenticated {
        time::timeout(idle_timeout, read_encrypted_frame(reader, iv, maximum))
            .await
            .map_err(|_| LoginSessionError::SessionIdleTimeout)?
    } else {
        time::timeout_at(login_deadline, read_encrypted_frame(reader, iv, maximum))
            .await
            .map_err(|_| LoginSessionError::LoginTimeout)?
    }
}

async fn write_session_bytes<W>(
    writer: &mut W,
    bytes: &[u8],
    timeout: std::time::Duration,
) -> Result<(), LoginSessionError>
where
    W: AsyncWrite + Unpin,
{
    time::timeout(timeout, writer.write_all(bytes))
        .await
        .map_err(|_| LoginSessionError::WriteTimeout)??;
    Ok(())
}

pub(crate) async fn run_login_session(
    mut stream: TcpStream,
    peer: SocketAddr,
    config: ServerConfig,
    world: WorldHandle,
    profiles: ProfileCoordinator,
) -> Result<(), LoginSessionError> {
    let (session_id, mut cancellation, mut outbound) = world.register_login_session(peer).await?;
    let registration = SessionRegistration {
        id: session_id,
        world: world.clone(),
        closed: false,
    };
    let result = run_registered_session(
        &mut stream,
        &config,
        &world,
        &profiles,
        session_id,
        &mut cancellation,
        &mut outbound,
    )
    .await;
    let close_result = registration.close().await;

    match (result, close_result) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error.into()),
        (Ok(()), Ok(())) => Ok(()),
    }
}

struct SessionRegistration {
    id: crate::SessionId,
    world: WorldHandle,
    closed: bool,
}

impl SessionRegistration {
    async fn close(mut self) -> Result<(), WorldError> {
        self.world.session_closed(self.id).await?;
        self.closed = true;
        Ok(())
    }
}

impl Drop for SessionRegistration {
    fn drop(&mut self) {
        if !self.closed {
            self.world.try_session_closed(self.id);
        }
    }
}

async fn run_registered_session(
    stream: &mut TcpStream,
    config: &ServerConfig,
    world: &WorldHandle,
    profiles: &ProfileCoordinator,
    session_id: SessionId,
    cancellation: &mut oneshot::Receiver<()>,
    outbound: &mut mpsc::Receiver<OutboundBatch>,
) -> Result<(), LoginSessionError> {
    let peer = peer_label(stream);
    tokio::select! {
        biased;
        _ = &mut *cancellation => return Err(LoginSessionError::Superseded),
        () = time::sleep(config.first_message_delay) => {}
    }

    // Install the receive state before putting the server-first frame on the
    // wire. No client read begins before this point.
    let mut receive_iv = handshake::initial_iv();
    let mut send_iv = handshake::initial_iv();
    let mut context = SessionContext::default();
    let services = SessionServices {
        config,
        world,
        profiles,
        session_id,
    };
    let payload = handshake::first_message_payload()?;
    let wire = frame::encode_plain(&payload, config.max_login_payload)?;
    tokio::select! {
        biased;
        _ = &mut *cancellation => return Err(LoginSessionError::Superseded),
        result = write_session_bytes(stream, &wire, config.session_write_timeout) => result?,
    }

    // Authentication packets may precede PqLogin, but they do not reset this
    // absolute deadline. A client cannot hold a slot forever by trickling
    // harmless pre-login frames.
    let login_deadline = time::Instant::now() + config.login_timeout;
    let (mut reader, mut writer) = stream.split();
    loop {
        // Keep this exact future alive while broadcasts are written. Dropping a
        // partially completed read_exact would consume bytes and desynchronize
        // the next encrypted frame.
        let frame = read_session_frame(
            &mut reader,
            &mut receive_iv,
            config.max_login_payload,
            context.is_authenticated(),
            login_deadline,
            config.session_idle_timeout,
        );
        tokio::pin!(frame);
        let packet = loop {
            tokio::select! {
                biased;
                _ = &mut *cancellation => return Err(LoginSessionError::Superseded),
                batch = outbound.recv() => {
                    let batch = batch.ok_or(LoginSessionError::OutboundClosed)?;
                    tokio::select! {
                        biased;
                        _ = &mut *cancellation => {
                            return Err(LoginSessionError::Superseded);
                        }
                        result = write_outbound_batch(
                            &mut writer,
                            batch,
                            &mut send_iv,
                            config,
                        ) => result?,
                    }
                }
                result = &mut frame => break result?,
            }
        };

        trace_packet(peer, &packet)?;
        tokio::select! {
            biased;
            _ = &mut *cancellation => return Err(LoginSessionError::Superseded),
            result = process_and_write(
                &mut writer,
                &services,
                &packet,
                &mut context,
                &mut send_iv,
            ) => result?,
        }
    }
}

async fn process_and_write<W>(
    writer: &mut W,
    services: &SessionServices<'_>,
    packet: &[u8],
    context: &mut SessionContext,
    send_iv: &mut u32,
) -> Result<(), LoginSessionError>
where
    W: AsyncWrite + Unpin,
{
    let responses = dispatch_packet(services, packet, context).await?;
    for response in responses {
        write_logical_packet(writer, &response, send_iv, services.config).await?;
    }
    Ok(())
}

async fn write_outbound_batch<W>(
    writer: &mut W,
    batch: OutboundBatch,
    send_iv: &mut u32,
    config: &ServerConfig,
) -> Result<(), LoginSessionError>
where
    W: AsyncWrite + Unpin,
{
    for packet in batch.into_packets() {
        write_logical_packet(writer, &packet, send_iv, config).await?;
    }
    Ok(())
}

async fn write_logical_packet<W>(
    writer: &mut W,
    packet: &[u8],
    send_iv: &mut u32,
    config: &ServerConfig,
) -> Result<(), LoginSessionError>
where
    W: AsyncWrite + Unpin,
{
    let wire = frame::encode_encrypted(packet, send_iv, config.max_login_payload)?;
    write_session_bytes(writer, &wire, config.session_write_timeout).await
}

async fn dispatch_packet(
    services: &SessionServices<'_>,
    packet: &[u8],
    context: &mut SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let hash = packet_hash(packet)?;
    if hash == adler32::packet_hash("PqCnAuthenLogin") {
        return Ok(vec![serialize_pr_cn_authen_login()?]);
    }

    if hash == adler32::packet_hash("PqLogin") {
        return handle_login(
            services.config,
            services.world,
            services.profiles,
            services.session_id,
            packet,
            context,
        )
        .await;
    }

    if hash == adler32::packet_hash("PqChannelSwitch") {
        let request = parse_pq_channel_switch(packet)?;
        let selected_channel =
            resolve_channel_id(request.requested_game_type, request.preferred_channel_id).ok_or(
                LoginSessionError::UnsupportedChannel {
                    game_type: request.requested_game_type,
                    preferred_channel: request.preferred_channel_id,
                },
            )?;
        let token = random_migration_token();
        let permit = services
            .world
            .begin_migration(
                services.session_id,
                ChannelBinding {
                    channel_id: selected_channel,
                    game_type: request.requested_game_type,
                },
                token,
                Instant::now(),
            )
            .await?;
        return Ok(vec![serialize_pr_channel_switch(
            selected_channel,
            permit.token.get(),
            services.config.advertised_address,
            services.config.ports.login_tcp(),
        )]);
    }

    if hash == adler32::packet_hash("PqChannelMovein") {
        return handle_channel_move_in(
            services.config,
            services.world,
            services.profiles,
            services.session_id,
            packet,
            context,
        )
        .await;
    }

    if let Some(request) = classify_room_protocol_request(hash) {
        return handle_room_request(
            services.world,
            services.session_id,
            request,
            packet,
            context,
        )
        .await;
    }

    if let Some(request) = classify_startup_request(hash) {
        return handle_startup_request(
            services.world,
            services.profiles,
            services.session_id,
            request,
            packet,
            context,
        )
        .await;
    }

    if is_startup_noop(hash) {
        let identity = services
            .world
            .authorize_identity(services.session_id)
            .await?;
        let _ = context.profile_for(&identity)?;
        return Ok(Vec::new());
    }

    // Identity-bound packets cannot be processed by a stale connection. Their
    // concrete handlers are ported incrementally on top of this fence.
    let _ = services
        .world
        .authorize_identity(services.session_id)
        .await?;
    Ok(Vec::new())
}

async fn handle_login(
    config: &ServerConfig,
    world: &WorldHandle,
    profiles: &ProfileCoordinator,
    session_id: SessionId,
    packet: &[u8],
    context: &mut SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let login = parse_pq_login(packet)?;
    let ownership_guard = profiles.ownership_guard().await;
    let claimed = world.claim_identity(session_id, login.nickname).await?;
    let profile = profiles
        .load(
            claimed.nickname.clone(),
            config.allow_remote_profile_creation || claimed.source_ip.is_loopback(),
            ownership_guard,
        )
        .await?;
    let identity = world.authorize_identity(session_id).await?;
    context.bind_profile(identity.clone(), profile);
    let profile = context.profile_for(&identity)?;

    Ok(vec![serialize_pr_login(&PrLoginFields {
        time: current_legacy_time(),
        user_no: identity.user_no.get(),
        nickname: identity.nickname,
        pmap: profile.rider.pmap,
        advertised_address: config.advertised_address,
        game_udp_port: config.ports.game_udp(),
        p2p_udp_port: config.ports.p2p_udp(),
        screen: profile.game_option.screen,
    })?])
}

async fn handle_channel_move_in(
    config: &ServerConfig,
    world: &WorldHandle,
    profiles: &ProfileCoordinator,
    session_id: SessionId,
    packet: &[u8],
    context: &mut SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let request = parse_pq_channel_movein(packet)?;
    let user_no = UserNo::new(request.user_no).ok_or(LoginSessionError::InvalidUserNo)?;
    let token = MigrationToken::new(request.migration_token)
        .ok_or(LoginSessionError::InvalidMigrationToken)?;

    let ownership_guard = profiles.ownership_guard().await;
    let completion = world
        .complete_migration(
            session_id,
            user_no,
            request.channel_id,
            token,
            Instant::now(),
        )
        .await?;
    let profile = profiles
        .load(
            completion.binding.nickname.clone(),
            config.allow_remote_profile_creation || completion.binding.source_ip.is_loopback(),
            ownership_guard,
        )
        .await?;
    let identity = world.authorize_identity(session_id).await?;
    context.bind_profile(identity, profile);

    Ok(vec![serialize_pr_channel_move_in(
        config.ports.game_udp(),
        config.ports.p2p_udp(),
    )])
}

async fn handle_room_request(
    world: &WorldHandle,
    session_id: SessionId,
    request: RoomProtocolRequest,
    packet: &[u8],
    context: &SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let payload = match request {
        RoomProtocolRequest::RoomList => {
            RoomCommandPayload::List(parse_ch_get_room_list_request(packet)?)
        }
        RoomProtocolRequest::CreateRoom => {
            let request = parse_ch_create_room_request(packet)?;
            let identity = world.authorize_identity(session_id).await?;
            let profile = context.profile_for(&identity)?;
            RoomCommandPayload::Create {
                request,
                participant: room_participant_from_profile(&identity, profile),
            }
        }
        RoomProtocolRequest::JoinRoom => {
            let request = parse_ch_join_room_request(packet)?;
            let identity = world.authorize_identity(session_id).await?;
            let profile = context.profile_for(&identity)?;
            RoomCommandPayload::Join {
                request,
                participant: room_participant_from_profile(&identity, profile),
            }
        }
        RoomProtocolRequest::LeaveRoom => {
            let _ = parse_ch_leave_room_request(packet)?;
            RoomCommandPayload::Leave
        }
        RoomProtocolRequest::FirstRoomState => {
            parse_gr_first_request(packet)?;
            RoomCommandPayload::FirstState
        }
    };
    world.room_protocol(session_id, payload).await?;
    Ok(Vec::new())
}

fn room_participant_from_profile(identity: &IdentityBinding, profile: &Profile) -> RoomParticipant {
    let observer = matches!(profile.rider.pmap, 590 | 718);
    let p2p_address = match identity.source_ip {
        IpAddr::V4(address) => address,
        IpAddr::V6(_) => Ipv4Addr::UNSPECIFIED,
    };
    let club_name = if profile.rider.club_mark_logo == 0 {
        String::new()
    } else {
        profile.rider.club_name.clone()
    };
    RoomParticipant {
        player: RoomPlayer {
            player_type: if observer { 4 } else { 2 },
            user_no: identity.user_no.get(),
            p2p_address,
            p2p_port: u16::try_from(profile.rider.p2p_port).unwrap_or_default(),
            nickname: identity.nickname.clone(),
            emblem_1: u16::from_le_bytes(profile.rider.emblem1.to_le_bytes()),
            emblem_2: u16::from_le_bytes(profile.rider.emblem2.to_le_bytes()),
            rider_item_snapshot: rider_item_snapshot(&profile.rider_item),
            card: profile.rider.card.clone(),
            rp: profile.rider.rp,
            team: 0,
            ranking: 0,
            rider_school_level: 0,
            club_name,
            club_mark_logo: profile.rider.club_mark_logo,
        },
        observer,
    }
}

async fn handle_startup_request(
    world: &WorldHandle,
    profiles: &ProfileCoordinator,
    session_id: SessionId,
    request: StartupRequest,
    packet: &[u8],
    context: &mut SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    if request == StartupRequest::GetRider {
        return handle_get_rider(world, profiles, session_id, context).await;
    }
    if request == StartupRequest::UpdateGameOption {
        update_game_options(world, profiles, session_id, packet, context).await?;
        return Ok(Vec::new());
    }

    let identity = world.authorize_identity(session_id).await?;
    let profile = context.profile_for(&identity)?;
    Ok(startup_response(request, profile).into_iter().collect())
}

async fn handle_get_rider(
    world: &WorldHandle,
    profiles: &ProfileCoordinator,
    session_id: SessionId,
    context: &SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let ownership_guard = profiles.ownership_guard().await;
    let before = world.authorize_identity(session_id).await?;
    let profile = context.bound_profile_for(&before)?.profile.clone();
    let responses = profiles
        .get_rider_sequence(before.nickname.clone(), profile, ownership_guard)
        .await?;
    let after = world.authorize_identity(session_id).await?;
    let _ = context.profile_for(&after)?;
    Ok(responses)
}

async fn update_game_options(
    world: &WorldHandle,
    profiles: &ProfileCoordinator,
    session_id: SessionId,
    packet: &[u8],
    context: &mut SessionContext,
) -> Result<(), LoginSessionError> {
    let request = parse_pq_update_game_option(packet)?;
    let ownership_guard = profiles.ownership_guard().await;
    let before = world.authorize_identity(session_id).await?;
    let _ = context.profile_for(&before)?;
    let profile = profiles
        .update_game_options(before.nickname.clone(), request.options, ownership_guard)
        .await?;
    let after = world.authorize_identity(session_id).await?;
    context.bind_profile(after, profile);
    Ok(())
}

fn profile_rider_fields(nickname: String, profile: &Profile) -> PrGetRiderFields {
    PrGetRiderFields {
        nickname,
        emblem_1: u16::from_le_bytes(profile.rider.emblem1.to_le_bytes()),
        emblem_2: u16::from_le_bytes(profile.rider.emblem2.to_le_bytes()),
        rider_item_snapshot: rider_item_snapshot(&profile.rider_item),
        lucci: profile.rider.lucci,
        rp: i32::from_le_bytes(profile.rider.rp.to_le_bytes()),
    }
}

fn startup_response(request: StartupRequest, profile: &Profile) -> Option<Vec<u8>> {
    let time = current_legacy_time();
    Some(match request {
        StartupRequest::LoginVipInfo => startup::serialize_pr_login_vip_info(profile.rider.premium),
        StartupRequest::EventReward => startup::serialize_lo_rp_event_reward(),
        StartupRequest::AddRacingTime => startup::serialize_lo_rp_add_racing_time(),
        StartupRequest::EquipTuning => startup::serialize_pr_equip_tuning_failure(),
        StartupRequest::GetGameOption => {
            startup::serialize_pr_get_game_option(&profile_game_options(&profile.game_option))
        }
        StartupRequest::SetPlaytimeEventTick => startup::serialize_pr_set_playtime_event_tick(),
        StartupRequest::ChapterInfo => startup::serialize_pr_chapter_info(),
        StartupRequest::GetDuelMissionBulk => startup::serialize_pr_get_duel_mission_bulk(time),
        StartupRequest::RiderSchoolData => startup::serialize_pr_rider_school_data(time),
        StartupRequest::RiderSchoolProgress => startup::serialize_pr_rider_school_progress(),
        StartupRequest::ChannelStatic => startup::serialize_channel_static_reply(),
        StartupRequest::DynamicCommand => startup::serialize_pr_dynamic_command(),
        StartupRequest::PublicCommand => startup::serialize_pr_public_command(),
        StartupRequest::GetFavoriteChannel => startup::serialize_pr_get_favorite_channel(),
        StartupRequest::KartPassInit => startup::serialize_pr_kart_pass_init(),
        StartupRequest::KartPassReward => startup::serialize_pr_kart_pass_reward(),
        StartupRequest::QuestUxSecond => startup::serialize_pr_quest_ux_second(),
        StartupRequest::GetCurrentRider => startup::serialize_pr_get_current_rider(),
        StartupRequest::DisassembleFeeInfo => startup::serialize_pr_disassemble_fee_info(),
        StartupRequest::SyncDictionaryInfo => startup::serialize_pr_sync_dictionary_info(),
        StartupRequest::AddTimeEventInit => startup::serialize_pr_add_time_event_init(time),
        StartupRequest::GetRider | StartupRequest::UpdateGameOption => return None,
    })
}

fn profile_game_options(options: &p5136_profile::GameOptions) -> startup::GameOptions {
    startup::GameOptions {
        bgm_volume: options.bgm_volume,
        sound_volume: options.sound_volume,
        main_bgm: options.main_bgm,
        sound_effect: options.sound_effect,
        full_screen: options.full_screen,
        show_mirror: options.show_mirror,
        show_other_player_names: options.show_other_player_names,
        show_outlines: options.show_outlines,
        show_shadows: options.show_shadows,
        high_level_effect: options.high_level_effect,
        motion_blur_effect: options.motion_blur_effect,
        motion_distortion_effect: options.motion_distortion_effect,
        high_end_optimization: options.high_end_optimization,
        auto_ready: options.auto_ready,
        prop_description: options.prop_description,
        video_quality: options.video_quality,
        bgm_check: options.bgm_check,
        sound_check: options.sound_check,
        show_hit_info: options.show_hit_info,
        auto_boost: options.auto_boost,
        game_type: options.game_type,
        set_ghost: options.set_ghost,
        speed_type: options.speed_type,
        room_chat: options.room_chat,
        driving_chat: options.driving_chat,
        show_all_player_hit_info: options.show_all_player_hit_info,
        show_team_color: options.show_team_color,
        set_screen: options.screen,
        hide_competitive_rank: options.hide_competitive_rank,
    }
}

fn apply_game_options(destination: &mut p5136_profile::GameOptions, source: &startup::GameOptions) {
    destination.bgm_volume = source.bgm_volume;
    destination.sound_volume = source.sound_volume;
    destination.main_bgm = source.main_bgm;
    destination.sound_effect = source.sound_effect;
    destination.full_screen = source.full_screen;
    destination.show_mirror = source.show_mirror;
    destination.show_other_player_names = source.show_other_player_names;
    destination.show_outlines = source.show_outlines;
    destination.show_shadows = source.show_shadows;
    destination.high_level_effect = source.high_level_effect;
    destination.motion_blur_effect = source.motion_blur_effect;
    destination.motion_distortion_effect = source.motion_distortion_effect;
    destination.high_end_optimization = source.high_end_optimization;
    destination.auto_ready = source.auto_ready;
    destination.prop_description = source.prop_description;
    destination.video_quality = source.video_quality;
    destination.bgm_check = source.bgm_check;
    destination.sound_check = source.sound_check;
    destination.show_hit_info = source.show_hit_info;
    destination.auto_boost = source.auto_boost;
    destination.game_type = source.game_type;
    destination.set_ghost = source.set_ghost;
    destination.speed_type = source.speed_type;
    destination.room_chat = source.room_chat;
    destination.driving_chat = source.driving_chat;
    destination.show_all_player_hit_info = source.show_all_player_hit_info;
    destination.show_team_color = source.show_team_color;
    destination.screen = source.set_screen;
    destination.hide_competitive_rank = source.hide_competitive_rank;
}

fn packet_hash(packet: &[u8]) -> Result<u32, LoginSessionError> {
    let bytes = packet
        .get(..4)
        .ok_or(LoginSessionError::MissingPacketHash)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn random_migration_token() -> MigrationToken {
    let mut random = rand::rng();
    loop {
        if let Some(token) = MigrationToken::new(random.random()) {
            return token;
        }
    }
}

fn current_legacy_time() -> LegacyTime {
    let now = Local::now();
    let epoch = NaiveDate::from_ymd_opt(1900, 1, 1).expect("1900-01-01 is a valid date");
    let days = (now.date_naive() - epoch).num_days().rem_euclid(65_536);
    let quarter_seconds = now.num_seconds_from_midnight() / 4;
    LegacyTime {
        days_since_1900: u16::try_from(days).expect("modulo 65536 fits in u16"),
        quarter_seconds: u16::try_from(quarter_seconds)
            .expect("one day of quarter-seconds fits in u16"),
    }
}

fn peer_label(stream: &TcpStream) -> Option<SocketAddr> {
    stream.peer_addr().ok()
}

fn trace_packet(peer: Option<SocketAddr>, packet: &[u8]) -> Result<(), LoginSessionError> {
    let hash = packet_hash(packet)?;
    tracing::debug!(
        ?peer,
        packet_hash = format_args!("0x{hash:08X}"),
        "login packet"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        sync::Arc,
        time::{Duration, Instant},
    };

    use p5136_core::{
        frame::{DEFAULT_MAX_PAYLOAD, encode_encrypted},
        packet::PacketWriter,
        room_protocol::{RoomProtocolError, RoomProtocolRequest},
        startup::GameOptions,
    };
    use p5136_profile::ProfileStore;
    use tokio::io::{AsyncWriteExt, duplex};
    use tokio::sync::oneshot;
    use tokio::time;

    use super::{
        BlockingUpdateHook, LoginSessionError, ProfileCoordinator, SessionContext,
        handle_room_request, read_encrypted_frame, read_session_frame, update_game_options,
        write_session_bytes,
    };
    use crate::{
        ChannelBinding, IdentityError, MigrationToken, SessionId, WorldError, WorldHandle,
    };

    #[tokio::test]
    async fn fragmented_and_coalesced_frames_decode_in_order() {
        let (mut writer, mut reader) = duplex(4_096);
        let mut send_iv = 0xa1b7_1c9b;
        let first = encode_encrypted(b"first-packet", &mut send_iv, DEFAULT_MAX_PAYLOAD).unwrap();
        let second = encode_encrypted(b"second-packet", &mut send_iv, DEFAULT_MAX_PAYLOAD).unwrap();

        let write_task = tokio::spawn(async move {
            for byte in &first[..7] {
                writer.write_all(&[*byte]).await.unwrap();
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            writer.write_all(&first[7..]).await.unwrap();
            writer.write_all(&second).await.unwrap();
        });

        let mut receive_iv = 0xa1b7_1c9b;
        assert_eq!(
            read_encrypted_frame(&mut reader, &mut receive_iv, DEFAULT_MAX_PAYLOAD)
                .await
                .unwrap(),
            b"first-packet"
        );
        assert_eq!(
            read_encrypted_frame(&mut reader, &mut receive_iv, DEFAULT_MAX_PAYLOAD)
                .await
                .unwrap(),
            b"second-packet"
        );
        write_task.await.unwrap();
        assert_eq!(receive_iv, send_iv);
    }

    #[tokio::test]
    async fn prelogin_deadline_is_absolute_and_authenticated_reads_have_an_idle_timeout() {
        let (mut writer, mut reader) = duplex(4_096);
        let mut send_iv = 0xa1b7_1c9b;
        let wire = encode_encrypted(b"auth-only", &mut send_iv, DEFAULT_MAX_PAYLOAD).unwrap();
        writer.write_all(&wire).await.unwrap();

        let deadline = time::Instant::now() + Duration::from_millis(30);
        let mut receive_iv = 0xa1b7_1c9b;
        assert_eq!(
            read_session_frame(
                &mut reader,
                &mut receive_iv,
                DEFAULT_MAX_PAYLOAD,
                false,
                deadline,
                Duration::from_secs(1),
            )
            .await
            .unwrap(),
            b"auth-only"
        );
        time::sleep(Duration::from_millis(40)).await;
        assert!(matches!(
            read_session_frame(
                &mut reader,
                &mut receive_iv,
                DEFAULT_MAX_PAYLOAD,
                false,
                deadline,
                Duration::from_secs(1),
            )
            .await,
            Err(LoginSessionError::LoginTimeout)
        ));

        assert!(matches!(
            read_session_frame(
                &mut reader,
                &mut receive_iv,
                DEFAULT_MAX_PAYLOAD,
                true,
                time::Instant::now() + Duration::from_secs(1),
                Duration::from_millis(20),
            )
            .await,
            Err(LoginSessionError::SessionIdleTimeout)
        ));
    }

    #[tokio::test]
    async fn write_timeout_bounds_a_client_that_stops_reading() {
        let (mut writer, _reader) = duplex(1);
        let result = write_session_bytes(&mut writer, &[0_u8; 64], Duration::from_millis(20)).await;
        assert!(matches!(result, Err(LoginSessionError::WriteTimeout)));
    }

    #[tokio::test]
    async fn malformed_room_packets_are_rejected_before_world_authorization() {
        let (world, world_task) = WorldHandle::spawn(4);
        let context = SessionContext::default();
        let mut trailing = PacketWriter::named("ChGetRoomListRequestPacket");
        trailing.write_i32(0);
        trailing.write_u8(1);
        trailing.write_u8(0);
        trailing.write_u8(0xff);
        assert!(matches!(
            handle_room_request(
                &world,
                SessionId::new(999),
                RoomProtocolRequest::RoomList,
                trailing.as_slice(),
                &context,
            )
            .await,
            Err(LoginSessionError::RoomProtocol(
                RoomProtocolError::TrailingBytes { count: 1, .. }
            ))
        ));

        let mut invalid_page = PacketWriter::named("ChGetRoomListRequestPacket");
        invalid_page.write_i32(-1);
        invalid_page.write_u8(1);
        invalid_page.write_u8(0);
        assert!(matches!(
            handle_room_request(
                &world,
                SessionId::new(999),
                RoomProtocolRequest::RoomList,
                invalid_page.as_slice(),
                &context,
            )
            .await,
            Err(LoginSessionError::RoomProtocol(
                RoomProtocolError::InvalidPage { page: -1, .. }
            ))
        ));

        world.shutdown().await.unwrap();
        world_task.await.unwrap();
    }

    #[tokio::test]
    async fn creation_policy_rejects_unknown_profiles_without_allocating_disk_state() {
        let profile_root = tempfile::tempdir().unwrap();
        let profiles = ProfileCoordinator::new(profile_root.path().to_owned(), None);

        let guard = profiles.ownership_guard().await;
        let error = profiles
            .load("RemoteRider".to_owned(), false, guard)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            LoginSessionError::ProfileCreationDenied { ref nickname }
                if nickname == "RemoteRider"
        ));
        assert!(!profile_root.path().join("RemoteRider").exists());

        fs::create_dir(profile_root.path().join("RemoteRider")).unwrap();
        let guard = profiles.ownership_guard().await;
        let loaded = profiles
            .load("remoterider".to_owned(), false, guard)
            .await
            .unwrap();
        assert!(loaded.source_path.is_file());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_update_keeps_ownership_gate_until_disk_save_finishes() {
        let profile_root = tempfile::tempdir().unwrap();
        let hook = BlockingUpdateHook::new();
        let profiles = ProfileCoordinator::new(profile_root.path().to_owned(), None)
            .with_blocking_update_hook(Arc::clone(&hook));
        let (world, world_task) = WorldHandle::spawn(32);
        let address = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let source = world
            .register_session(SocketAddr::new(address, 50_000))
            .await
            .unwrap();
        let destination = world
            .register_session(SocketAddr::new(address, 50_001))
            .await
            .unwrap();
        let identity = world.claim_identity(source, "Rider").await.unwrap();
        let ownership_guard = profiles.ownership_guard().await;
        profiles
            .load(identity.nickname.clone(), true, ownership_guard)
            .await
            .unwrap();

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

        let update_profiles = profiles.clone();
        let update = tokio::spawn(async move {
            let ownership_guard = update_profiles.ownership_guard().await;
            update_profiles
                .update_game_options(
                    identity.nickname,
                    GameOptions {
                        video_quality: 77,
                        ..GameOptions::default()
                    },
                    ownership_guard,
                )
                .await
        });
        let entered_hook = Arc::clone(&hook);
        tokio::task::spawn_blocking(move || entered_hook.entered.wait())
            .await
            .unwrap();

        update.abort();
        assert!(update.await.unwrap_err().is_cancelled());

        let migration_profiles = profiles.clone();
        let migration_world = world.clone();
        let user_no = identity.user_no;
        let (attempting, attempted) = oneshot::channel();
        let mut migration = tokio::spawn(async move {
            let _ = attempting.send(());
            let ownership_guard = migration_profiles.ownership_guard().await;
            let completion = migration_world
                .complete_migration(destination, user_no, 12, token, Instant::now())
                .await;
            drop(ownership_guard);
            completion
        });
        attempted.await.unwrap();
        assert!(
            time::timeout(Duration::from_millis(50), &mut migration)
                .await
                .is_err(),
            "migration acquired the ownership gate while the cancelled save still ran"
        );

        let release_hook = Arc::clone(&hook);
        tokio::task::spawn_blocking(move || release_hook.release.wait())
            .await
            .unwrap();
        migration.await.unwrap().unwrap();

        let persisted = ProfileStore::new(profile_root.path())
            .load_or_create("Rider")
            .unwrap();
        assert_eq!(persisted.revision, Some(2));
        assert_eq!(persisted.profile.game_option.video_quality, 77);

        world.session_closed(source).await.unwrap();
        world.session_closed(destination).await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap();
    }

    #[tokio::test]
    async fn stale_generation_cannot_publish_a_profile_update() {
        let profile_root = tempfile::tempdir().unwrap();
        let profiles = ProfileCoordinator::new(profile_root.path().to_owned(), None);
        let (world, world_task) = WorldHandle::spawn(32);
        let address = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let source = world
            .register_session(SocketAddr::new(address, 50_000))
            .await
            .unwrap();
        let destination = world
            .register_session(SocketAddr::new(address, 50_001))
            .await
            .unwrap();
        let identity = world.claim_identity(source, "Rider").await.unwrap();
        let ownership_guard = profiles.ownership_guard().await;
        let profile = profiles
            .load(identity.nickname.clone(), true, ownership_guard)
            .await
            .unwrap();
        let mut context = SessionContext::default();
        context.bind_profile(identity.clone(), profile);

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
        world
            .complete_migration(destination, identity.user_no, 12, token, Instant::now())
            .await
            .unwrap();

        let mut update = PacketWriter::named("PqUpdateGameOption");
        update.write_f32(0.25);
        update.write_f32(0.5);
        update.write_bytes(&[99; 27]);
        assert!(matches!(
            update_game_options(
                &world,
                &profiles,
                source,
                update.as_slice(),
                &mut context
            )
            .await,
            Err(LoginSessionError::World(WorldError::Identity(
                IdentityError::StaleSession(id)
            ))) if id == source
        ));

        let persisted = ProfileStore::new(profile_root.path())
            .load_or_create("Rider")
            .unwrap();
        assert_eq!(persisted.revision, Some(1));
        assert_eq!(persisted.profile.game_option.video_quality, 14);

        world.session_closed(source).await.unwrap();
        world.session_closed(destination).await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap();
    }
}
