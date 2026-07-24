use std::{io, net::SocketAddr, path::PathBuf, sync::Arc};

use p5136_profile::{CatalogInventory, CatalogInventoryError};
use thiserror::Error;
use tokio::{
    net::{TcpListener, UdpSocket},
    sync::{OwnedSemaphorePermit, Semaphore, watch},
    task::{JoinError, JoinHandle, JoinSet},
};

use crate::{
    ServerConfig, ServerEndpoints, WorldError, WorldHandle,
    session::{ProfileCoordinator, run_login_session},
};

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

    #[error("{service} listener failed")]
    ListenerIo {
        service: &'static str,
        #[source]
        source: io::Error,
    },

    #[error("server supervisor task failed")]
    SupervisorTask(#[from] JoinError),

    #[error(transparent)]
    World(#[from] WorldError),
}

#[derive(Debug)]
pub struct BoundServer {
    config: ServerConfig,
    catalog: Option<Arc<CatalogInventory>>,
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

        Ok(Self {
            config,
            catalog,
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
        let (shutdown, shutdown_receiver) = watch::channel(false);
        let (world, world_task) = WorldHandle::spawn(1_024);
        let supervisor_world = world.clone();
        let task = tokio::spawn(async move {
            run_supervisor(self, shutdown_receiver, supervisor_world, world_task).await
        });

        Ok(ServerHandle {
            endpoints,
            shutdown,
            world,
            task,
        })
    }
}

#[derive(Debug)]
pub struct ServerHandle {
    endpoints: ServerEndpoints,
    shutdown: watch::Sender<bool>,
    world: WorldHandle,
    task: JoinHandle<Result<(), ServerError>>,
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

    pub async fn shutdown(self) -> Result<(), ServerError> {
        let _ = self.shutdown.send(true);
        self.task.await?
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

async fn finish_supervisor(
    mut sessions: JoinSet<()>,
    world: WorldHandle,
    world_task: JoinHandle<()>,
    transport_result: Result<(), ServerError>,
) -> Result<(), ServerError> {
    sessions.abort_all();
    while sessions.join_next().await.is_some() {}
    let world_shutdown = world.shutdown().await;
    let world_join = world_task.await;
    transport_result?;
    world_shutdown?;
    world_join.map_err(ServerError::SupervisorTask)
}

async fn run_supervisor(
    server: BoundServer,
    mut shutdown: watch::Receiver<bool>,
    world: WorldHandle,
    world_task: JoinHandle<()>,
) -> Result<(), ServerError> {
    let BoundServer {
        config,
        catalog,
        game_udp,
        login_tcp,
        p2p_udp,
        messenger_tcp,
    } = server;
    let profiles = ProfileCoordinator::new(config.profile_root.clone(), catalog);
    let login_session_permits = Arc::new(Semaphore::new(config.max_login_sessions));
    let mut sessions = JoinSet::new();
    let mut game_buffer = vec![0_u8; 65_535];
    let mut p2p_buffer = vec![0_u8; 65_535];

    let transport_result = loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break Ok(());
                }
            }
            accepted = login_tcp.accept() => {
                match accepted {
                    Ok((stream, peer)) => {
                        let Some(permit) = try_login_session_permit(
                            &login_session_permits,
                            config.max_login_sessions,
                            peer,
                        ) else {
                            drop(stream);
                            continue;
                        };
                        spawn_login_session(
                            &mut sessions,
                            stream,
                            peer,
                            permit,
                            &config,
                            &world,
                            &profiles,
                        );
                    }
                    Err(source) => {
                        break Err(ServerError::ListenerIo {
                            service: "login TCP",
                            source,
                        });
                    }
                }
            }
            datagram = game_udp.recv_from(&mut game_buffer) => {
                match datagram {
                    Ok((length, peer)) => {
                        tracing::trace!(%peer, length, "game UDP packet ignored by milestone runtime");
                    }
                    Err(source) => {
                        break Err(ServerError::ListenerIo {
                            service: "game UDP",
                            source,
                        });
                    }
                }
            }
            datagram = p2p_udp.recv_from(&mut p2p_buffer) => {
                match datagram {
                    Ok((length, peer)) => {
                        tracing::trace!(%peer, length, "P2P UDP packet ignored by milestone runtime");
                    }
                    Err(source) => {
                        break Err(ServerError::ListenerIo {
                            service: "P2P UDP",
                            source,
                        });
                    }
                }
            }
            accepted = messenger_tcp.accept() => {
                match accepted {
                    Ok((stream, peer)) => {
                        tracing::debug!(%peer, "messenger protocol is not implemented; closing connection");
                        drop(stream);
                    }
                    Err(source) => {
                        break Err(ServerError::ListenerIo {
                            service: "messenger TCP",
                            source,
                        });
                    }
                }
            }
            completed = sessions.join_next(), if !sessions.is_empty() => {
                if let Some(Err(error)) = completed {
                    tracing::error!(%error, "login session task panicked");
                }
            }
        }
    };

    finish_supervisor(sessions, world, world_task, transport_result).await
}

#[cfg(test)]
mod tests {
    use std::{
        fmt::Write as _,
        fs,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        path::Path,
        time::Duration,
    };

    use p5136_core::{
        adler32,
        bml::BmlNode,
        channel::serialize_pr_channel_move_in,
        frame, handshake,
        login::{LegacyTime, serialize_pr_cn_authen_login},
        packet::{PacketReader, PacketWriter},
        room_protocol::{
            CreateRoomOutcome, JoinRoomStatus, serialize_ch_create_room_reply,
            serialize_ch_join_room_reply, serialize_ch_leave_room_reply,
        },
        startup::{
            GameOptions, PrGetRiderFields, serialize_channel_static_reply,
            serialize_lo_rp_event_reward, serialize_pr_add_time_event_init,
            serialize_pr_get_game_option, serialize_pr_get_rider, serialize_pr_login_vip_info,
        },
    };
    use p5136_profile::{Profile, ProfileStore, rider_item_snapshot};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream, UdpSocket},
        time,
    };

    use super::{BoundServer, ServerError, ServerHandle, load_catalog};
    use crate::{ServerConfig, read_encrypted_frame};

    struct LoginClient {
        stream: TcpStream,
        send_iv: u32,
        receive_iv: u32,
        user_no: u32,
        pmap: u32,
        screen: u8,
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
        let bound = BoundServer {
            config,
            catalog: None,
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
        let bound = BoundServer {
            config,
            catalog: None,
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
        let bound = BoundServer {
            config,
            catalog: None,
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
        let (server, maximum) = start_test_server(profile_root.path(), None).await;
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
        let bound = BoundServer {
            config,
            catalog,
            game_udp: UdpSocket::bind((loopback, 0)).await.unwrap(),
            login_tcp: TcpListener::bind((loopback, 0)).await.unwrap(),
            p2p_udp: UdpSocket::bind((loopback, 0)).await.unwrap(),
            messenger_tcp: TcpListener::bind((loopback, 0)).await.unwrap(),
        };
        (bound.start().unwrap(), maximum)
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
