use std::{io, net::SocketAddr};

use thiserror::Error;
use tokio::{
    net::{TcpListener, UdpSocket},
    sync::watch,
    task::{JoinError, JoinHandle, JoinSet},
};

use crate::{ServerConfig, ServerEndpoints, WorldError, WorldHandle, session::run_login_session};

#[derive(Debug, Error)]
pub enum ServerError {
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
    game_udp: UdpSocket,
    login_tcp: TcpListener,
    p2p_udp: UdpSocket,
    messenger_tcp: TcpListener,
}

impl BoundServer {
    /// Transactionally binds all four P5136 transports. If any bind fails,
    /// already-created sockets are dropped before the error is returned.
    pub async fn bind(config: ServerConfig) -> Result<Self, ServerError> {
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

async fn run_supervisor(
    server: BoundServer,
    mut shutdown: watch::Receiver<bool>,
    world: WorldHandle,
    world_task: JoinHandle<()>,
) -> Result<(), ServerError> {
    let BoundServer {
        config,
        game_udp,
        login_tcp,
        p2p_udp,
        messenger_tcp,
    } = server;
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
                        let world = world.clone();
                        let config = config.clone();
                        sessions.spawn(async move {
                            if let Err(error) = run_login_session(stream, peer, config, world).await {
                                tracing::debug!(%peer, %error, "login session closed");
                            }
                        });
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

    sessions.abort_all();
    while sessions.join_next().await.is_some() {}

    let world_shutdown = world.shutdown().await;
    let world_join = world_task.await;
    transport_result?;
    world_shutdown?;
    world_join.map_err(ServerError::SupervisorTask)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr},
        time::Duration,
    };

    use p5136_core::{
        adler32,
        bml::BmlNode,
        channel::serialize_pr_channel_move_in,
        frame, handshake,
        login::serialize_pr_cn_authen_login,
        packet::{PacketReader, PacketWriter},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream, UdpSocket},
        time,
    };

    use super::BoundServer;
    use crate::{ServerConfig, read_encrypted_frame};

    #[tokio::test]
    async fn full_runtime_sends_exact_server_first_handshake_and_shuts_down() {
        let loopback = Ipv4Addr::LOCALHOST;
        let config = ServerConfig {
            bind_address: IpAddr::V4(loopback),
            advertised_address: loopback,
            first_message_delay: Duration::ZERO,
            login_timeout: Duration::from_secs(2),
            ..ServerConfig::default()
        };
        let bound = BoundServer {
            config,
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
    async fn encrypted_auth_login_and_channel_migration_complete_over_real_tcp() {
        let loopback = Ipv4Addr::LOCALHOST;
        let config = ServerConfig {
            bind_address: IpAddr::V4(loopback),
            advertised_address: loopback,
            first_message_delay: Duration::ZERO,
            login_timeout: Duration::from_secs(2),
            ..ServerConfig::default()
        };
        let maximum = config.max_login_payload;
        let bound = BoundServer {
            config,
            game_udp: UdpSocket::bind((loopback, 0)).await.unwrap(),
            login_tcp: TcpListener::bind((loopback, 0)).await.unwrap(),
            p2p_udp: UdpSocket::bind((loopback, 0)).await.unwrap(),
            messenger_tcp: TcpListener::bind((loopback, 0)).await.unwrap(),
        };
        let server = bound.start().unwrap();
        let endpoints = server.endpoints();

        let (mut source, mut source_send_iv, mut source_receive_iv, user_no) =
            authenticate_and_login(endpoints.login_tcp, maximum).await;
        let (selected_channel, migration_token) = request_channel_switch(
            &mut source,
            &mut source_send_iv,
            &mut source_receive_iv,
            maximum,
            12,
        )
        .await;
        assert_eq!(selected_channel, 12);

        // A second request replaces the first permit. Completing the older
        // token must neither transfer ownership nor cancel the source socket.
        let (latest_channel, latest_token) = request_channel_switch(
            &mut source,
            &mut source_send_iv,
            &mut source_receive_iv,
            maximum,
            11,
        )
        .await;
        assert_eq!(latest_channel, 11);

        let (mut stale_destination, mut stale_send_iv, _) =
            connect_login_client(endpoints.login_tcp).await;
        let mut stale_move_in = PacketWriter::named("PqChannelMovein");
        stale_move_in.write_u32(user_no);
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
        assert_login_socket_open(&mut source).await;

        let (mut destination, mut destination_send_iv, mut destination_receive_iv) =
            connect_login_client(endpoints.login_tcp).await;
        let mut move_in = PacketWriter::named("PqChannelMovein");
        move_in.write_u32(user_no);
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

        assert_login_socket_closed(&mut source).await;
        wait_for_session_count(&server.world(), 1).await;
        drop(destination);
        wait_for_session_count(&server.world(), 0).await;
        server.shutdown().await.unwrap();
    }

    async fn authenticate_and_login(
        endpoint: std::net::SocketAddr,
        maximum: usize,
    ) -> (TcpStream, u32, u32, u32) {
        let (mut source, mut send_iv, mut receive_iv) = connect_login_client(endpoint).await;
        let auth_request = PacketWriter::named("PqCnAuthenLogin").into_inner();
        send_packet(&mut source, &auth_request, &mut send_iv, maximum).await;
        let auth_reply = read_encrypted_frame(&mut source, &mut receive_iv, maximum)
            .await
            .unwrap();
        assert_eq!(auth_reply, serialize_pr_cn_authen_login().unwrap());

        let login_request = build_login_request("Yany2");
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
        assert_eq!(user_no, 1);
        assert_eq!(reader.read_utf16().unwrap(), "Yany2");
        (source, send_iv, receive_iv, user_no)
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
