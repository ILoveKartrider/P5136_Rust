use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use p5136_core::{frame::DEFAULT_MAX_PAYLOAD, ports::PortTopology};

use crate::{
    ItemProbabilityConfiguration, ItemProbabilityRankPolicy, RandomTrackConfiguration,
    ResolvedRandomTracks,
};

pub const DEFAULT_MAX_LOGIN_SESSIONS: usize = 256;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub bind_address: IpAddr,
    pub advertised_address: Ipv4Addr,
    pub ports: PortTopology,
    pub profile_root: PathBuf,
    pub catalog_path: Option<PathBuf>,
    /// Optional stock-client `Data` directory containing the KR `*.rho5`
    /// archives used to load authoritative emblem definitions.
    pub client_data_dir: Option<PathBuf>,
    /// Optional validated GUI/CLI override. `None` loads the stock
    /// item.rho/RHO5 tables from `client_data_dir`, falling back to the
    /// bounded safe table when no client data directory is configured.
    pub item_probabilities: Option<ItemProbabilityConfiguration>,
    /// Controls whether `Live` probability bands trust the rank carried by
    /// the validated client pickup request.
    pub item_probability_rank_policy: ItemProbabilityRankPolicy,
    /// Optional pool overrides for the stock `track_common.rho` random-track
    /// catalog. An empty configuration uses the client defaults.
    pub random_tracks: RandomTrackConfiguration,
    /// Pre-resolved client catalog installed transactionally by
    /// [`crate::BoundServer::bind`]. Normal callers should leave this `None`.
    pub resolved_random_tracks: Option<Arc<ResolvedRandomTracks>>,
    pub first_message_delay: Duration,
    pub login_timeout: Duration,
    pub session_idle_timeout: Duration,
    pub session_write_timeout: Duration,
    pub max_login_sessions: usize,
    /// Permit a non-loopback peer to create a previously unknown profile.
    ///
    /// Existing remote profiles can always log in. Keeping creation opt-in
    /// prevents unauthenticated nicknames from growing disk and cache state.
    pub allow_remote_profile_creation: bool,
    pub max_login_payload: usize,
    pub max_messenger_payload: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_address: IpAddr::V4(Ipv4Addr::LOCALHOST),
            advertised_address: Ipv4Addr::LOCALHOST,
            ports: PortTopology::default(),
            profile_root: PathBuf::from("Profile"),
            catalog_path: None,
            client_data_dir: None,
            item_probabilities: None,
            item_probability_rank_policy: ItemProbabilityRankPolicy::default(),
            random_tracks: RandomTrackConfiguration::default(),
            resolved_random_tracks: None,
            first_message_delay: Duration::from_millis(250),
            login_timeout: Duration::from_secs(12),
            session_idle_timeout: Duration::from_secs(5 * 60),
            session_write_timeout: Duration::from_secs(15),
            max_login_sessions: DEFAULT_MAX_LOGIN_SESSIONS,
            allow_remote_profile_creation: false,
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
