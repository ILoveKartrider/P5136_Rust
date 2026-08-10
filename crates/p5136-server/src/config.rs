use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use p5136_core::{frame::DEFAULT_MAX_PAYLOAD, ports::PortTopology};
use p5136_profile::CatalogInventory;

use crate::{
    ItemProbabilityConfiguration, ItemProbabilityRankPolicy, RandomTrackConfiguration,
    ResolvedRandomTracks,
};

pub const DEFAULT_MAX_LOGIN_SESSIONS: usize = 256;

/// Server-selected modern physics preset for stock time-attack starts.
///
/// The visible S grade is not identical to the P5136 protocol speed byte:
/// S0 uses byte 3, while S1 through S3 use bytes 0 through 2.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeAttackPhysicsPreset {
    #[default]
    ClientDefault,
    S0,
    S1,
    S2,
    S3,
    S4,
    S5,
    S6,
    S7,
    S8,
}

impl TimeAttackPhysicsPreset {
    pub const ALL: [Self; 10] = [
        Self::ClientDefault,
        Self::S0,
        Self::S1,
        Self::S2,
        Self::S3,
        Self::S4,
        Self::S5,
        Self::S6,
        Self::S7,
        Self::S8,
    ];

    #[must_use]
    pub const fn speed_type(self) -> Option<u8> {
        match self {
            Self::ClientDefault => None,
            Self::S0 => Some(3),
            Self::S1 => Some(0),
            Self::S2 => Some(1),
            Self::S3 => Some(2),
            Self::S4 => Some(4),
            Self::S5 => Some(5),
            Self::S6 => Some(6),
            Self::S7 => Some(7),
            Self::S8 => Some(8),
        }
    }

    #[must_use]
    pub const fn resolve(self, client_speed_type: u8) -> u8 {
        match self.speed_type() {
            Some(speed_type) => speed_type,
            None => client_speed_type,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TimeAttackPhysicsPreset;

    #[test]
    fn time_attack_presets_preserve_default_and_map_visible_s_grades_to_wire_bytes() {
        assert_eq!(TimeAttackPhysicsPreset::default().resolve(6), 6);
        assert_eq!(
            TimeAttackPhysicsPreset::ALL.map(TimeAttackPhysicsPreset::speed_type),
            [
                None,
                Some(3),
                Some(0),
                Some(1),
                Some(2),
                Some(4),
                Some(5),
                Some(6),
                Some(7),
                Some(8),
            ]
        );
    }
}

/// Selects the stock P5136 PRO license mission pair.
///
/// All six pairs already exist in the Korean client RHO. Manual variants only
/// change the server-projected rider-school reference month and packet steps;
/// they do not require a client patch.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiderSchoolProMissionSet {
    #[default]
    Automatic,
    FairyMabinogi,
    ChinaSword,
    GoldAbyss,
    ForestOlympus,
    PirateNemo,
    MineMaple,
}

impl RiderSchoolProMissionSet {
    pub const MANUAL: [Self; 6] = [
        Self::FairyMabinogi,
        Self::ChinaSword,
        Self::GoldAbyss,
        Self::ForestOlympus,
        Self::PirateNemo,
        Self::MineMaple,
    ];

    #[must_use]
    pub const fn pair_index(self) -> Option<usize> {
        match self {
            Self::Automatic => None,
            Self::FairyMabinogi => Some(0),
            Self::ChinaSword => Some(1),
            Self::GoldAbyss => Some(2),
            Self::ForestOlympus => Some(3),
            Self::PirateNemo => Some(4),
            Self::MineMaple => Some(5),
        }
    }

    /// January, March, May, July, September, or November for the selected
    /// pair. The client groups consecutive months into the same pair.
    #[must_use]
    pub const fn reference_month(self) -> Option<u32> {
        match self {
            Self::Automatic => None,
            Self::FairyMabinogi => Some(1),
            Self::ChinaSword => Some(3),
            Self::GoldAbyss => Some(5),
            Self::ForestOlympus => Some(7),
            Self::PirateNemo => Some(9),
            Self::MineMaple => Some(11),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub bind_address: IpAddr,
    pub advertised_address: Ipv4Addr,
    pub ports: PortTopology,
    pub profile_root: PathBuf,
    /// Compatibility-only path for callers that still provide an exported
    /// catalog. Normal GUI/CLI startup leaves this unset and reads RHO data.
    pub catalog_path: Option<PathBuf>,
    /// Optional stock-client `Data` directory containing the KR `*.rho5`
    /// archives used to load authoritative emblem definitions.
    pub client_data_dir: Option<PathBuf>,
    /// Pre-resolved direct-RHO catalog snapshot. The GUI uses this after an
    /// inventory search load so server startup does not parse `kart.rho`
    /// twice. Normal CLI callers leave it `None`.
    pub resolved_catalog: Option<Arc<CatalogInventory>>,
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
    /// Automatic bi-monthly rotation or one manually selected stock PRO pair.
    pub rider_school_pro_mission_set: RiderSchoolProMissionSet,
    /// Physics grade written into every accepted time-attack start reply.
    pub time_attack_physics_preset: TimeAttackPhysicsPreset,
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
            resolved_catalog: None,
            item_probabilities: None,
            item_probability_rank_policy: ItemProbabilityRankPolicy::default(),
            random_tracks: RandomTrackConfiguration::default(),
            rider_school_pro_mission_set: RiderSchoolProMissionSet::Automatic,
            time_attack_physics_preset: TimeAttackPhysicsPreset::default(),
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
