use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

use p5136_core::{frame::DEFAULT_MAX_PAYLOAD, ports::PortTopology};

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub bind_address: IpAddr,
    pub advertised_address: Ipv4Addr,
    pub ports: PortTopology,
    pub first_message_delay: Duration,
    pub login_timeout: Duration,
    pub max_login_payload: usize,
    pub max_messenger_payload: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_address: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            advertised_address: Ipv4Addr::LOCALHOST,
            ports: PortTopology::default(),
            first_message_delay: Duration::from_millis(250),
            login_timeout: Duration::from_secs(12),
            max_login_payload: DEFAULT_MAX_PAYLOAD,
            max_messenger_payload: 256 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerEndpoints {
    pub login_tcp: SocketAddr,
    pub game_udp: SocketAddr,
    pub p2p_udp: SocketAddr,
    pub messenger_tcp: SocketAddr,
}
