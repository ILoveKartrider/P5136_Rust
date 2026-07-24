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

    use p5136_core::handshake;
    use tokio::{
        io::AsyncReadExt,
        net::{TcpListener, TcpStream, UdpSocket},
        time,
    };

    use super::BoundServer;
    use crate::ServerConfig;

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
}
