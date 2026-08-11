use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use p5136_core::{
    frame::DEFAULT_MAX_PAYLOAD,
    ports::PortTopology,
    race_start_protocol::{AiRaceSpec, RaceStartProtocolError},
};
use p5136_profile::CatalogInventory;

use crate::{
    ItemProbabilityConfiguration, ItemProbabilityRankPolicy, RandomTrackConfiguration,
    ResolvedRandomTracks,
};

pub const DEFAULT_MAX_LOGIN_SESSIONS: usize = 256;
pub const DEFAULT_SPEED_BASIC_AI_PARAMETER_VALUES: [f32; 6] =
    [0.7, 2_400.0, 2_950.0, 1.5, 1_000.0, 1_500.0];
pub const DEFAULT_ITEM_BASIC_AI_PARAMETER_VALUES: [f32; 6] =
    [0.6, 2_400.0, 2_950.0, 1.5, 1_000.0, 1_500.0];

/// Validated parameters serialized once for each basic-AI racer at race start.
///
/// P5136 consumes the first four values as target-speed factor, base-speed
/// scalar, boost duration in milliseconds, and boost-acceleration multiplier.
/// The final two values remain codec-visible compatibility fields but are not
/// read by this client's `GoBasicAiKart` consumer path.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BasicAiParameters {
    race_spec: AiRaceSpec,
}

impl BasicAiParameters {
    #[must_use]
    pub const fn values(self) -> [f32; 6] {
        *self.race_spec.values()
    }

    #[must_use]
    pub(crate) const fn race_spec(self) -> AiRaceSpec {
        self.race_spec
    }
}

impl Default for BasicAiParameters {
    fn default() -> Self {
        Self::try_from(DEFAULT_SPEED_BASIC_AI_PARAMETER_VALUES)
            .expect("the stock P5136 basic-AI parameter vector is valid")
    }
}

impl TryFrom<[f32; 6]> for BasicAiParameters {
    type Error = RaceStartProtocolError;

    fn try_from(values: [f32; 6]) -> Result<Self, Self::Error> {
        Ok(Self {
            race_spec: AiRaceSpec::try_from(values)?,
        })
    }
}

/// Separate server vectors for speed and item races.
///
/// The original C# implementation chose a lower field-0 range for item mode,
/// in addition to the client's own `speedVal`/`itemVal` multiplier. Team and
/// individual races share the vector for their game mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BasicAiModeParameters {
    speed: BasicAiParameters,
    item: BasicAiParameters,
}

impl BasicAiModeParameters {
    #[must_use]
    pub const fn new(speed: BasicAiParameters, item: BasicAiParameters) -> Self {
        Self { speed, item }
    }

    #[must_use]
    pub const fn speed(self) -> BasicAiParameters {
        self.speed
    }

    #[must_use]
    pub const fn item(self) -> BasicAiParameters {
        self.item
    }

    #[must_use]
    pub(crate) const fn race_spec_for_game_type(self, game_type: u8) -> AiRaceSpec {
        if matches!(game_type, 2 | 4) {
            self.item.race_spec()
        } else {
            self.speed.race_spec()
        }
    }
}

impl Default for BasicAiModeParameters {
    fn default() -> Self {
        Self {
            speed: BasicAiParameters::try_from(DEFAULT_SPEED_BASIC_AI_PARAMETER_VALUES)
                .expect("the stock P5136 speed-AI vector is valid"),
            item: BasicAiParameters::try_from(DEFAULT_ITEM_BASIC_AI_PARAMETER_VALUES)
                .expect("the stock P5136 item-AI vector is valid"),
        }
    }
}

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
    use super::{
        BasicAiModeParameters, BasicAiParameters, DEFAULT_ITEM_BASIC_AI_PARAMETER_VALUES,
        DEFAULT_SPEED_BASIC_AI_PARAMETER_VALUES, TimeAttackPhysicsPreset,
    };

    fn float_bits(values: [f32; 6]) -> [u32; 6] {
        values.map(f32::to_bits)
    }

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

    #[test]
    fn basic_ai_parameters_preserve_stock_values_and_reject_invalid_input() {
        assert_eq!(
            float_bits(BasicAiParameters::default().values()),
            float_bits(DEFAULT_SPEED_BASIC_AI_PARAMETER_VALUES)
        );
        assert!(BasicAiParameters::try_from([f32::NAN; 6]).is_err());
        assert!(BasicAiParameters::try_from([10_001.0; 6]).is_err());

        let modes = BasicAiModeParameters::default();
        assert_eq!(
            float_bits(modes.speed().values()),
            float_bits(DEFAULT_SPEED_BASIC_AI_PARAMETER_VALUES)
        );
        assert_eq!(
            float_bits(modes.item().values()),
            float_bits(DEFAULT_ITEM_BASIC_AI_PARAMETER_VALUES)
        );
        assert_eq!(
            float_bits(*modes.race_spec_for_game_type(1).values()),
            float_bits(DEFAULT_SPEED_BASIC_AI_PARAMETER_VALUES)
        );
        assert_eq!(
            float_bits(*modes.race_spec_for_game_type(2).values()),
            float_bits(DEFAULT_ITEM_BASIC_AI_PARAMETER_VALUES)
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
    /// Per-racer basic-AI behaviour vector written into each room start.
    pub basic_ai_parameters: BasicAiModeParameters,
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
            basic_ai_parameters: BasicAiModeParameters::default(),
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
