use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow};
use eframe::egui;
use p5136_connector::{
    ConnectorCancellation, ConnectorPlan, ConnectorRequest, ConnectorStage, InstallationOptions,
    Runner, RunnerBackend, execute_connector_with_progress_and_cancellation,
};
use p5136_core::{
    floater_physics::{ALL_FLOATER_CODES, p5136_floater_spec},
    ports::{DEFAULT_CONFIGURED_PORT, PortTopology},
};
use p5136_profile::{
    AddKartOutcome, AdditionalKart, CatalogInventory, KartCatalogSearchResult, KartGrantOptions,
    ProfileStore, RiderSchoolProgress, add_kart_with_options, additional_karts, search_karts,
};
use p5136_server::{
    BoundServer, ItemProbabilityConfiguration, ItemProbabilityEntry, ItemProbabilityRankBand,
    ItemProbabilityRankPolicy, RandomTrackCatalog, RandomTrackConfiguration, RandomTrackDefinition,
    RandomTrackPool, RandomTrackPoolOverride, ResolvedRandomTracks, RiderSchoolProMissionSet,
    ServerConfig, ServerEndpoints, TimeAttackPhysicsPreset, load_client_item_probabilities,
    load_client_kart_catalog, load_client_random_track_catalog, load_item_probability_xml,
};
use serde::{Deserialize, Serialize};

use crate::{
    FileLoggingControl, LoggingRuntime, client_paths,
    gui_i18n::{GuiLanguage, tr, tr_format},
};

const WINDOW_TITLE: &str = "KartRider P5136";
const BUILD_VERSION: &str = env!("CARGO_PKG_VERSION");
const GUI_CLOSE_GRACE_PERIOD: Duration = Duration::from_secs(5);
const GUI_SETTINGS_KEY: &str = "p5136-gui-settings-v2";
const MAX_GUI_SETTINGS_BYTES: usize = 2 * 1024 * 1024;

pub(crate) fn run(logging: &LoggingRuntime) -> Result<()> {
    let log_path = logging.log_path.clone();
    let logging_control = logging.control.clone();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([780.0, 680.0])
            .with_min_inner_size([600.0, 520.0]),
        centered: true,
        ..Default::default()
    };

    eframe::run_native(
        WINDOW_TITLE,
        options,
        Box::new(move |creation_context| {
            configure_platform_fonts(&creation_context.egui_ctx);
            Ok(Box::new(P5136GuiApp::new_with_logging(
                log_path,
                logging_control,
                creation_context.storage,
            )))
        }),
    )
    .map_err(|error| anyhow!("failed to run the desktop GUI: {error}"))
}

fn configure_platform_fonts(context: &egui::Context) {
    let loaded = platform_cjk_font_candidates()
        .into_iter()
        .filter_map(|path| std::fs::read(&path).ok().map(|bytes| (path, bytes)))
        .collect::<Vec<_>>();
    if loaded.is_empty() {
        tracing::warn!(
            "CJK UI font was unavailable; install Noto Sans CJK if Korean or Chinese text renders as boxes"
        );
        return;
    }

    let mut fonts = egui::FontDefinitions::default();
    let mut loaded_names = Vec::with_capacity(loaded.len());
    for (index, (font_path, font_bytes)) in loaded.into_iter().enumerate() {
        let font_name = format!("platform-cjk-ui-{index}");
        fonts.font_data.insert(
            font_name.clone(),
            std::sync::Arc::new(egui::FontData::from_owned(font_bytes)),
        );
        tracing::info!(font_path = %font_path.display(), "loaded CJK UI font");
        loaded_names.push(font_name);
    }
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        if let Some(family_fonts) = fonts.families.get_mut(&family) {
            for (index, font_name) in loaded_names.iter().enumerate() {
                family_fonts.insert(index, font_name.clone());
            }
        }
    }
    context.set_fonts(fonts);
}

#[cfg(target_os = "windows")]
fn platform_cjk_font_candidates() -> Vec<PathBuf> {
    let windows_root =
        std::env::var_os("WINDIR").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    vec![
        windows_root.join("Fonts").join("malgun.ttf"),
        windows_root.join("Fonts").join("malgunbd.ttf"),
        windows_root.join("Fonts").join("msyh.ttc"),
        windows_root.join("Fonts").join("msyhbd.ttc"),
        windows_root.join("Fonts").join("simsun.ttc"),
    ]
}

#[cfg(target_os = "macos")]
fn platform_cjk_font_candidates() -> Vec<PathBuf> {
    [
        "/System/Library/Fonts/AppleSDGothicNeo.ttc",
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/Supplemental/AppleGothic.ttf",
        "/Library/Fonts/NanumGothic.ttf",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect()
}

#[cfg(target_os = "linux")]
fn platform_cjk_font_candidates() -> Vec<PathBuf> {
    [
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJKkr-Regular.otf",
        "/usr/share/fonts/truetype/nanum/NanumGothic.ttf",
        "/usr/share/fonts/truetype/unfonts-core/UnDotum.ttf",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect()
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn platform_cjk_font_candidates() -> Vec<PathBuf> {
    Vec::new()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum GuiRunner {
    Auto,
    Native,
    NativeElevated,
    Wine,
    CrossOver,
    Sikarugir,
}

impl GuiRunner {
    const ALL: [Self; 6] = [
        Self::Auto,
        Self::Native,
        Self::NativeElevated,
        Self::Wine,
        Self::CrossOver,
        Self::Sikarugir,
    ];

    fn label(self, language: GuiLanguage) -> &'static str {
        match self {
            Self::Auto => tr!(language, "자동", "Automatic", "自动"),
            Self::Native => tr!(
                language,
                "직접 실행 (관리자 권한 없음)",
                "Direct launch (no administrator rights)",
                "直接启动（无管理员权限）"
            ),
            Self::NativeElevated => tr!(
                language,
                "직접 실행 (Windows UAC)",
                "Direct launch (Windows UAC)",
                "直接启动（Windows UAC）"
            ),
            Self::Wine => "Wine",
            Self::CrossOver => "CrossOver",
            Self::Sikarugir => "Sikarugir wrapper",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct GuiInputs {
    game_directory: String,
    game_executable: String,
    nickname: String,
    observer_mode: bool,
    server: String,
    configured_port: String,
    runner: GuiRunner,
    wine_binary: String,
    wine_prefix: String,
    crossover_binary: String,
    crossover_bottle: String,
    sikarugir_app: String,
}

impl Default for GuiInputs {
    fn default() -> Self {
        Self {
            game_directory: default_game_directory().display().to_string(),
            game_executable: String::new(),
            nickname: "player".to_owned(),
            observer_mode: false,
            server: Ipv4Addr::LOCALHOST.to_string(),
            configured_port: DEFAULT_CONFIGURED_PORT.to_string(),
            runner: GuiRunner::Auto,
            wine_binary: "wine".to_owned(),
            wine_prefix: String::new(),
            crossover_binary: default_crossover_binary().display().to_string(),
            crossover_bottle: "KartRider-P5136".to_owned(),
            sikarugir_app: String::new(),
        }
    }
}

impl GuiInputs {
    fn connector_plan(&self, language: GuiLanguage) -> Result<ConnectorPlan> {
        let game_directory = required_path(
            &self.game_directory,
            tr!(language, "게임 디렉터리", "Game directory", "游戏目录"),
            language,
        )?;
        let game_executable = optional_path(&self.game_executable);
        let server_address = self.server.trim().parse::<Ipv4Addr>().with_context(|| {
            tr!(
                language,
                "서버 주소는 IPv4여야 합니다",
                "The server address must be IPv4",
                "服务器地址必须是 IPv4"
            )
        })?;
        if server_address.is_unspecified() {
            return Err(anyhow!(tr!(
                language,
                "서버 주소로 0.0.0.0을 사용할 수 없습니다",
                "0.0.0.0 cannot be used as the server address",
                "不能将 0.0.0.0 用作服务器地址"
            )));
        }
        let configured_port = self
            .configured_port
            .trim()
            .parse::<u16>()
            .with_context(|| {
                tr!(
                    language,
                    "기준 포트는 0~65535 범위여야 합니다",
                    "The base port must be in the range 0–65535",
                    "基准端口必须在 0–65535 范围内"
                )
            })?;
        let ports = PortTopology::new(configured_port).with_context(|| {
            tr!(
                language,
                "기준 포트에서 필요한 접속기 포트를 모두 만들 수 없습니다",
                "The required connector ports cannot be derived from the base port",
                "无法从基准端口生成所需的连接器端口"
            )
        })?;
        let runner = self.runner(language)?;

        let installation_options = InstallationOptions {
            launcher_profile_role: if self.observer_mode {
                p5136_connector::LauncherProfileRole::ObserverMaster
            } else {
                p5136_connector::LauncherProfileRole::Regular
            },
            ..InstallationOptions::default()
        };

        ConnectorPlan::new(ConnectorRequest {
            game_directory,
            game_executable,
            nickname: self.nickname.clone(),
            server_address,
            ports,
            runner,
            probe_timeout: p5136_connector::DEFAULT_PROBE_TIMEOUT,
            installation_options,
        })
        .with_context(|| {
            tr!(
                language,
                "접속기 설정이 올바르지 않습니다",
                "The connector settings are invalid",
                "连接器设置无效"
            )
        })
    }

    fn runner(&self, language: GuiLanguage) -> Result<Runner> {
        match self.runner {
            GuiRunner::Auto => Ok(Runner::Auto),
            GuiRunner::Native => Ok(Runner::Native),
            GuiRunner::NativeElevated => Ok(Runner::NativeElevated),
            GuiRunner::Wine => Ok(Runner::Wine {
                binary: required_path(
                    &self.wine_binary,
                    tr!(
                        language,
                        "Wine 실행 파일",
                        "Wine executable",
                        "Wine 可执行文件"
                    ),
                    language,
                )?,
                prefix: optional_path(&self.wine_prefix),
            }),
            GuiRunner::CrossOver => Ok(Runner::CrossOver {
                wine_binary: required_path(
                    &self.crossover_binary,
                    tr!(
                        language,
                        "CrossOver 실행 파일",
                        "CrossOver executable",
                        "CrossOver 可执行文件"
                    ),
                    language,
                )?,
                bottle: required_text(
                    &self.crossover_bottle,
                    tr!(
                        language,
                        "CrossOver 보틀",
                        "CrossOver bottle",
                        "CrossOver 容器"
                    ),
                    language,
                )?
                .to_owned(),
            }),
            GuiRunner::Sikarugir => Ok(Runner::Sikarugir {
                app: required_path(
                    &self.sikarugir_app,
                    tr!(
                        language,
                        "Sikarugir wrapper 앱",
                        "Sikarugir wrapper app",
                        "Sikarugir 包装器应用"
                    ),
                    language,
                )?,
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct ServerInputs {
    bind_address: String,
    advertised_address: String,
    configured_port: String,
    profile_root: String,
    client_path: String,
    client_data_dir: String,
    allow_remote_profile_creation: bool,
    first_message_delay_ms: String,
    login_timeout_seconds: String,
    session_idle_timeout_seconds: String,
    session_write_timeout_seconds: String,
    max_login_sessions: String,
    trust_client_item_rank: bool,
    item_probabilities: ItemProbabilityConfiguration,
    item_probability_source: GuiItemProbabilitySource,
    item_probability_xml: String,
    show_team_item_probabilities: bool,
    random_tracks: RandomTrackConfiguration,
    rider_school_pro_mission_set: RiderSchoolProMissionSet,
    time_attack_physics_preset: TimeAttackPhysicsPreset,
    file_logging: GuiFileLogging,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct GuiPersistedSettings {
    #[serde(default)]
    language: GuiLanguage,
    connector: GuiInputs,
    server: ServerInputs,
}

impl GuiPersistedSettings {
    fn load(storage: Option<&dyn eframe::Storage>) -> Option<Self> {
        let encoded = storage?.get_string(GUI_SETTINGS_KEY)?;
        if encoded.len() > MAX_GUI_SETTINGS_BYTES {
            tracing::warn!(
                bytes = encoded.len(),
                "ignored oversized persisted GUI settings"
            );
            return None;
        }
        serde_json::from_str(&encoded)
            .inspect_err(|error| tracing::warn!(%error, "ignored malformed persisted GUI settings"))
            .ok()
    }

    fn save(&self, storage: &mut dyn eframe::Storage) {
        let encoded = match serde_json::to_string(self) {
            Ok(encoded) if encoded.len() <= MAX_GUI_SETTINGS_BYTES => encoded,
            Ok(encoded) => {
                tracing::error!(
                    bytes = encoded.len(),
                    "GUI settings exceed the persistence size limit"
                );
                return;
            }
            Err(error) => {
                tracing::error!(%error, "failed to serialize GUI settings");
                return;
            }
        };
        storage.set_string(GUI_SETTINGS_KEY, encoded);
    }

    fn from_app(app: &P5136GuiApp) -> Self {
        Self {
            language: app.language,
            connector: app.connector_inputs.clone(),
            server: app.server_inputs.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum GuiItemProbabilitySource {
    AutoClient,
    Edited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
enum GuiFileLogging {
    Disabled,
    #[default]
    Enabled,
}

impl GuiFileLogging {
    const fn enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

impl Default for ServerInputs {
    fn default() -> Self {
        Self {
            bind_address: IpAddr::V4(Ipv4Addr::LOCALHOST).to_string(),
            advertised_address: Ipv4Addr::LOCALHOST.to_string(),
            configured_port: DEFAULT_CONFIGURED_PORT.to_string(),
            profile_root: "Profile".to_owned(),
            client_path: String::new(),
            client_data_dir: String::new(),
            allow_remote_profile_creation: false,
            first_message_delay_ms: "250".to_owned(),
            login_timeout_seconds: "12".to_owned(),
            session_idle_timeout_seconds: "300".to_owned(),
            session_write_timeout_seconds: "15".to_owned(),
            max_login_sessions: p5136_server::DEFAULT_MAX_LOGIN_SESSIONS.to_string(),
            trust_client_item_rank: true,
            item_probabilities: ItemProbabilityConfiguration::safe_fallback(),
            item_probability_source: GuiItemProbabilitySource::AutoClient,
            item_probability_xml: String::new(),
            show_team_item_probabilities: false,
            random_tracks: RandomTrackConfiguration::default(),
            rider_school_pro_mission_set: RiderSchoolProMissionSet::Automatic,
            time_attack_physics_preset: TimeAttackPhysicsPreset::default(),
            file_logging: GuiFileLogging::Enabled,
        }
    }
}

impl ServerInputs {
    // Each validation message is kept beside its field in all three GUI languages.
    #[allow(clippy::too_many_lines)]
    fn server_config(&self, language: GuiLanguage) -> Result<ServerConfig> {
        let bind_address = self
            .bind_address
            .trim()
            .parse::<IpAddr>()
            .with_context(|| {
                tr!(
                    language,
                    "바인드 주소는 IPv4 또는 IPv6여야 합니다",
                    "The bind address must be IPv4 or IPv6",
                    "绑定地址必须是 IPv4 或 IPv6"
                )
            })?;
        let advertised_address = self
            .advertised_address
            .trim()
            .parse::<Ipv4Addr>()
            .with_context(|| {
                tr!(
                    language,
                    "클라이언트에 알릴 주소는 IPv4여야 합니다",
                    "The advertised client address must be IPv4",
                    "向客户端公布的地址必须是 IPv4"
                )
            })?;
        if advertised_address.is_unspecified()
            || advertised_address.is_multicast()
            || advertised_address == Ipv4Addr::BROADCAST
        {
            return Err(anyhow!(tr!(
                language,
                "클라이언트에 알릴 IPv4는 0.0.0.0, 멀티캐스트, 브로드캐스트 주소일 수 없습니다",
                "The advertised IPv4 cannot be 0.0.0.0, multicast, or broadcast",
                "向客户端公布的 IPv4 不能是 0.0.0.0、组播或广播地址"
            )));
        }
        let configured_port = self
            .configured_port
            .trim()
            .parse::<u16>()
            .with_context(|| {
                tr!(
                    language,
                    "기준 포트는 0~65535 범위여야 합니다",
                    "The base port must be in the range 0–65535",
                    "基准端口必须在 0–65535 范围内"
                )
            })?;
        let ports = PortTopology::new(configured_port).with_context(|| {
            tr!(
                language,
                "기준 포트에서 P5136 서비스 포트를 모두 만들 수 없습니다",
                "The P5136 service ports cannot be derived from the base port",
                "无法从基准端口生成全部 P5136 服务端口"
            )
        })?;
        let max_login_sessions = parse_usize(
            &self.max_login_sessions,
            tr!(
                language,
                "최대 로그인 세션 수",
                "Maximum login sessions",
                "最大登录会话数"
            ),
            language,
        )?;
        if max_login_sessions == 0 {
            return Err(anyhow!(tr!(
                language,
                "최대 로그인 세션 수는 1 이상이어야 합니다",
                "Maximum login sessions must be at least 1",
                "最大登录会话数必须至少为 1"
            )));
        }
        if let Some(pool) = self
            .random_tracks
            .pools
            .iter()
            .find(|pool| pool.track_ids.is_empty())
        {
            return Err(anyhow!(tr_format!(
                language,
                "랜덤 맵 사용자 지정 목록에는 맵이 1개 이상 필요합니다: game_type={}, selector={}",
                "A custom random-track pool needs at least one track: game_type={}, selector={}",
                "自定义随机地图池至少需要一张地图：game_type={}，selector={}",
                pool.game_type,
                pool.selector
            )));
        }
        let client_path = required_path(
            &self.client_path,
            tr!(
                language,
                "클라이언트 또는 Profile 경로",
                "Client or Profile path",
                "客户端或 Profile 路径"
            ),
            language,
        )?;
        let client_paths = client_paths::resolve_client_runtime_paths(
            Some(&client_path),
            optional_path_ref(&self.client_data_dir),
        )?;

        Ok(ServerConfig {
            bind_address,
            advertised_address,
            ports,
            profile_root: required_path(
                &self.profile_root,
                tr!(
                    language,
                    "프로필 저장 경로",
                    "Profile storage path",
                    "配置文件保存路径"
                ),
                language,
            )?,
            catalog_path: None,
            client_data_dir: client_paths.client_data_dir,
            item_probability_rank_policy: if self.trust_client_item_rank {
                ItemProbabilityRankPolicy::TrustClientReported
            } else {
                ItemProbabilityRankPolicy::CombinedFallback
            },
            item_probabilities: match self.item_probability_source {
                GuiItemProbabilitySource::AutoClient => None,
                GuiItemProbabilitySource::Edited => {
                    self.item_probabilities.validate()?;
                    Some(self.item_probabilities.clone())
                }
            },
            random_tracks: self.random_tracks.clone(),
            rider_school_pro_mission_set: self.rider_school_pro_mission_set,
            time_attack_physics_preset: self.time_attack_physics_preset,
            first_message_delay: Duration::from_millis(parse_u64(
                &self.first_message_delay_ms,
                tr!(
                    language,
                    "첫 메시지 지연",
                    "First-message delay",
                    "首条消息延迟"
                ),
                language,
            )?),
            login_timeout: Duration::from_secs(parse_u64(
                &self.login_timeout_seconds,
                tr!(language, "로그인 제한 시간", "Login timeout", "登录超时"),
                language,
            )?),
            session_idle_timeout: Duration::from_secs(parse_u64(
                &self.session_idle_timeout_seconds,
                tr!(
                    language,
                    "세션 유휴 제한 시간",
                    "Session idle timeout",
                    "会话空闲超时"
                ),
                language,
            )?),
            session_write_timeout: Duration::from_secs(parse_u64(
                &self.session_write_timeout_seconds,
                tr!(
                    language,
                    "세션 전송 제한 시간",
                    "Session write timeout",
                    "会话写入超时"
                ),
                language,
            )?),
            max_login_sessions,
            allow_remote_profile_creation: self.allow_remote_profile_creation,
            ..ServerConfig::default()
        })
    }
}

fn required_text<'a>(value: &'a str, label: &str, language: GuiLanguage) -> Result<&'a str> {
    if value.trim().is_empty() {
        Err(anyhow!(tr_format!(
            language,
            "{label}을(를) 비워 둘 수 없습니다",
            "{label} cannot be empty",
            "{label}不能为空"
        )))
    } else {
        Ok(value)
    }
}

fn required_path(value: &str, label: &str, language: GuiLanguage) -> Result<PathBuf> {
    required_text(value, label, language).map(PathBuf::from)
}

fn optional_path_ref(value: &str) -> Option<&Path> {
    (!value.trim().is_empty()).then(|| Path::new(value))
}

fn optional_path(value: &str) -> Option<PathBuf> {
    optional_path_ref(value).map(Path::to_owned)
}

fn rider_school_grade_label(language: GuiLanguage, progress: RiderSchoolProgress) -> &'static str {
    match progress {
        RiderSchoolProgress::NONE_CLEARED => {
            tr!(
                language,
                "미취득 (처음부터)",
                "Unlicensed (start)",
                "未取得（从头开始）"
            )
        }
        RiderSchoolProgress::BEGINNER => tr!(language, "초보", "Beginner", "初级"),
        RiderSchoolProgress::ROOKIE => tr!(language, "루키", "Rookie", "新手"),
        RiderSchoolProgress::L3 => "L3",
        RiderSchoolProgress::L2 => "L2",
        RiderSchoolProgress::L1 => "L1",
        RiderSchoolProgress::ALL_CLEAR => tr!(language, "PRO", "PRO", "PRO"),
        _ => tr!(language, "사용자 지정", "Custom", "自定义"),
    }
}

fn rider_school_pro_mission_set_label(
    language: GuiLanguage,
    selection: RiderSchoolProMissionSet,
) -> &'static str {
    match selection {
        RiderSchoolProMissionSet::Automatic => tr!(
            language,
            "자동 (현재 2개월 주기)",
            "Automatic (current two-month rotation)",
            "自动（当前双月轮换）"
        ),
        RiderSchoolProMissionSet::FairyMabinogi => tr!(
            language,
            "동화 이상한 나라의 문 → 마비노기 이멘 마하",
            "Fairy Door to Wonderland → Mabinogi Emain Macha",
            "童话 奇境之门 → 洛奇 伊文玛哈"
        ),
        RiderSchoolProMissionSet::ChinaSword => tr!(
            language,
            "차이나 골목길 대질주 → 도검 구름의 협곡",
            "China Alley Rush → Sword Cloud Canyon",
            "中国 胡同疾驰 → 刀剑 云之峡谷"
        ),
        RiderSchoolProMissionSet::GoldAbyss => tr!(
            language,
            "황금문명 비밀장치의 위협 → 어비스 스카이라인",
            "Golden Civilization Secret Device → Abyss Skyline",
            "黄金文明 秘密装置的威胁 → 深渊 天际线"
        ),
        RiderSchoolProMissionSet::ForestOlympus => tr!(
            language,
            "포레스트 아찔한 다운힐 → 올림포스 하늘의 신전",
            "Forest Dizzying Downhill → Olympus Sky Temple",
            "森林 惊险下坡 → 奥林匹斯 天空神殿"
        ),
        RiderSchoolProMissionSet::PirateNemo => tr!(
            language,
            "해적 숨겨진 보물 → 네모 산타의 비밀공간",
            "Pirate Hidden Treasure → Cube Santa's Secret Space",
            "海盗 隐藏宝藏 → 方块 圣诞老人的秘密空间"
        ),
        RiderSchoolProMissionSet::MineMaple => tr!(
            language,
            "광산 위험한 제련소 → 메이플 레헬른 악몽의 시계탑",
            "Mine Dangerous Refinery → MapleStory Lachelein Clocktower",
            "矿山 危险冶炼厂 → 冒险岛 梦都噩梦钟楼"
        ),
    }
}

fn time_attack_physics_preset_label(
    language: GuiLanguage,
    preset: TimeAttackPhysicsPreset,
) -> &'static str {
    match preset {
        TimeAttackPhysicsPreset::ClientDefault => tr!(
            language,
            "기본 설정 (클라이언트 선택)",
            "Default (client selection)",
            "默认设置（客户端选择）"
        ),
        TimeAttackPhysicsPreset::S0 => tr!(language, "S0 보통", "S0 Normal", "S0 普通"),
        TimeAttackPhysicsPreset::S1 => tr!(language, "S1 빠름", "S1 Fast", "S1 快速"),
        TimeAttackPhysicsPreset::S2 => {
            tr!(language, "S2 매우 빠름", "S2 Very fast", "S2 非常快")
        }
        TimeAttackPhysicsPreset::S3 => tr!(language, "S3 가장 빠름", "S3 Fastest", "S3 极速"),
        TimeAttackPhysicsPreset::S4 => tr!(
            language,
            "S4 무한부스터",
            "S4 Infinite Booster",
            "S4 无限加速"
        ),
        TimeAttackPhysicsPreset::S5 => tr!(
            language,
            "S5 특수 프리셋",
            "S5 special preset",
            "S5 特殊预设"
        ),
        TimeAttackPhysicsPreset::S6 => tr!(
            language,
            "S6 이벤트 무한부스터",
            "S6 event Infinite Booster",
            "S6 活动无限加速"
        ),
        TimeAttackPhysicsPreset::S7 => tr!(
            language,
            "S7 통합 스피드",
            "S7 integrated speed",
            "S7 统一竞速"
        ),
        TimeAttackPhysicsPreset::S8 => tr!(
            language,
            "S8 통합 아이템",
            "S8 integrated item",
            "S8 统一道具"
        ),
    }
}

#[allow(clippy::too_many_lines)]
fn floater_code_label(language: GuiLanguage, code: i16) -> String {
    if code == 0 {
        return tr!(language, "없음 (0)", "None (0)", "无（0）").to_owned();
    }
    if code < 10_000 {
        let effect = match code / 100 {
            1 => tr!(language, "항력 감소", "Drag reduction", "阻力降低"),
            2 => tr!(language, "전진 가속", "Forward acceleration", "前进加速"),
            3 => tr!(language, "코너링", "Cornering", "弯道性能"),
            4 => tr!(
                language,
                "팀 부스터 지속",
                "Team-booster duration",
                "组队加速持续时间"
            ),
            5 => tr!(
                language,
                "개인 부스터 지속",
                "Normal-booster duration",
                "普通加速持续时间"
            ),
            6 => tr!(
                language,
                "출발 부스터 지속",
                "Start-booster duration",
                "起步加速持续时间"
            ),
            7 => tr!(language, "변신 가속", "Transform acceleration", "变形加速"),
            8 => tr!(language, "드리프트 게이지", "Drift gauge", "漂移集气"),
            9 => tr!(
                language,
                "드리프트 탈출력",
                "Drift escape force",
                "漂移脱离力"
            ),
            _ => return code.to_string(),
        };
        let grade = code % 100;
        return tr_format!(
            language,
            "{effect} {grade}단계 ({code})",
            "{effect} grade {grade} ({code})",
            "{effect} {grade}级（{code}）"
        );
    }
    // Exact group/id names from the stock Korean P5136
    // `zeta_/kr/enchant/desc.xml` RHO5 entry. The code is group * 100 + id.
    let (ko, en, zh, level) = match code {
        10_103 => ("물폭탄 방어", "Water-bomb defense", "防御水炸弹", Some(3)),
        10_203 => ("물파리 방어", "Water-fly defense", "防御水苍蝇", Some(3)),
        10_303 => (
            "아이템 큐브 획득 시 루찌 획득",
            "Gain Lucci from an item cube",
            "获得道具箱时获得金币",
            Some(3),
        ),
        10_401 => (
            "대마왕 류 방어 (100%)",
            "Devil-family defense (100%)",
            "防御大魔王类道具（100%）",
            None,
        ),
        10_503 => (
            "실드 획득 시 슈퍼 실드로 변경",
            "Chance to convert an acquired shield to a super shield",
            "获得盾牌时一定概率变为超级盾牌",
            Some(3),
        ),
        10_603 => (
            "보스전 미사일 데미지 증가",
            "Increased rocket damage in Boss mode",
            "Boss 模式导弹伤害增加",
            Some(3),
        ),
        10_703 => (
            "우주선 전파를 실드로 획득",
            "Chance to gain a shield from a UFO signal",
            "一定概率将宇宙船电波变为盾牌",
            Some(3),
        ),
        10_803 => (
            "자석 사용 시 부스터 획득",
            "Chance to gain a booster after using a magnet",
            "使用磁铁时一定概率获得加速器",
            Some(3),
        ),
        10_901 => (
            "바나나를 밟았을 때 부스터 획득 (100%)",
            "Gain a booster after hitting a banana (100%)",
            "踩到香蕉时获得加速器（100%）",
            None,
        ),
        11_001 => (
            "물폭탄 대신 독성 물폭탄 사용",
            "Use a toxic water bomb instead of a water bomb",
            "用毒性水炸弹替代水炸弹",
            None,
        ),
        11_103 => (
            "미사일을 황금 미사일로 변경",
            "Chance to convert a rocket to a gold rocket",
            "一定概率将导弹变为黄金导弹",
            Some(3),
        ),
        11_201 => (
            "물폭탄 대신 얼음 물폭탄 사용",
            "Use an ice water bomb instead of a water bomb",
            "用冰冻水炸弹替代水炸弹",
            None,
        ),
        11_301 => (
            "바나나 방어 (100%)",
            "Banana defense (100%)",
            "防御香蕉（100%）",
            None,
        ),
        11_403 => (
            "부스터 획득 시 사이렌으로 변경",
            "Chance to convert an acquired booster to a siren",
            "获得加速器时一定概率变为警笛",
            Some(3),
        ),
        11_501 => (
            "바나나 대신 물지뢰 사용",
            "Use a water mine instead of a banana",
            "用水雷替代香蕉",
            None,
        ),
        11_601 => (
            "물폭탄·물파리에서 재빨리 탈출",
            "Quick escape from a water-bomb or water-fly hit",
            "被水炸弹或水苍蝇命中时快速脱离",
            None,
        ),
        11_701 => (
            "미사일 2발 동시 발사",
            "Fire two rockets simultaneously",
            "同时发射两枚导弹",
            None,
        ),
        11_803 => (
            "부스터 획득 시 슈퍼 실드로 변경",
            "Chance to convert an acquired booster to a super shield",
            "获得加速器时一定概率变为超级盾牌",
            Some(3),
        ),
        11_903 => (
            "배틀·챔피언스 모드 물파리 방어",
            "Water-fly defense in Battle/Champions mode",
            "战斗／冠军模式水苍蝇防御",
            Some(3),
        ),
        12_003 => (
            "배틀·챔피언스 모드 물폭탄 방어",
            "Water-bomb defense in Battle/Champions mode",
            "战斗／冠军模式水炸弹防御",
            Some(3),
        ),
        _ => return code.to_string(),
    };
    let effect = match language {
        GuiLanguage::Korean => ko,
        GuiLanguage::English => en,
        GuiLanguage::SimplifiedChinese => zh,
    };
    if let Some(level) = level {
        tr_format!(
            language,
            "{effect} +{level} ({code})",
            "{effect} +{level} ({code})",
            "{effect} +{level}（{code}）"
        )
    } else {
        tr_format!(
            language,
            "{effect} ({code})",
            "{effect} ({code})",
            "{effect}（{code}）"
        )
    }
}

fn parse_u64(value: &str, label: &str, language: GuiLanguage) -> Result<u64> {
    value.trim().parse::<u64>().with_context(|| {
        tr_format!(
            language,
            "{label}은(는) 0 이상의 정수여야 합니다",
            "{label} must be a non-negative integer",
            "{label}必须是非负整数"
        )
    })
}

fn parse_usize(value: &str, label: &str, language: GuiLanguage) -> Result<usize> {
    value.trim().parse::<usize>().with_context(|| {
        tr_format!(
            language,
            "{label}은(는) 0 이상의 정수여야 합니다",
            "{label} must be a non-negative integer",
            "{label}必须是非负整数"
        )
    })
}

fn discover_lan_ipv4_candidates(language: GuiLanguage) -> Result<Vec<(String, Ipv4Addr)>> {
    let mut candidates = local_ip_address::list_afinet_netifas()
        .with_context(|| {
            tr!(
                language,
                "네트워크 어댑터 목록을 읽지 못했습니다",
                "Failed to read the network-adapter list",
                "无法读取网络适配器列表"
            )
        })?
        .into_iter()
        .filter_map(|(name, address)| match address {
            IpAddr::V4(address)
                if !address.is_loopback()
                    && !address.is_unspecified()
                    && !address.is_multicast()
                    && !address.is_link_local() =>
            {
                Some((name, address))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|(name, address)| {
        (
            virtual_adapter_rank(name),
            lan_address_rank(*address),
            *address,
        )
    });
    candidates.dedup_by_key(|(_, address)| *address);
    if candidates.is_empty() {
        return Err(anyhow!(tr!(
            language,
            "사용 가능한 LAN IPv4 주소를 찾지 못했습니다",
            "No usable LAN IPv4 address was found",
            "未找到可用的局域网 IPv4 地址"
        )));
    }
    Ok(candidates)
}

const fn lan_address_rank(address: Ipv4Addr) -> u8 {
    let octets = address.octets();
    if octets[0] == 192 && octets[1] == 168 {
        0
    } else if octets[0] == 10 {
        1
    } else if octets[0] == 172 && octets[1] >= 16 && octets[1] <= 31 {
        2
    } else if octets[0] == 100 && octets[1] >= 64 && octets[1] <= 127 {
        4
    } else {
        3
    }
}

fn virtual_adapter_rank(name: &str) -> u8 {
    let name = name.to_ascii_lowercase();
    u8::from(
        name.contains("virtual")
            || name.contains("vethernet")
            || name.contains("vmware")
            || name.contains("virtualbox")
            || name.contains("wsl")
            || name.contains("vpn")
            || name.contains("tailscale")
            || name.contains("radmin")
            || name.contains("hyper-v"),
    )
}

fn status_is_error(status: &str) -> bool {
    status.contains("실패") || status.contains("Failed") || status.contains("失败")
}

fn status_is_uncertain(status: &str) -> bool {
    status.contains("확인하지 못했습니다")
        || status.contains("could not be confirmed")
        || status.contains("无法确认")
}

fn item_probability_grid(
    ui: &mut egui::Ui,
    entries: &mut [ItemProbabilityEntry],
    language: GuiLanguage,
) -> bool {
    let mut changed = false;
    egui::ScrollArea::horizontal()
        .id_salt("item-probability-table-scroll")
        .show(ui, |ui| {
            egui::Grid::new("item-probability-table")
                .num_columns(6)
                .striped(true)
                .spacing([12.0, 5.0])
                .show(ui, |ui| {
                    for heading in [
                        "ID",
                        tr!(language, "아이템", "Item", "道具"),
                        tr!(language, "1등", "1st", "第1名"),
                        tr!(language, "상위", "High", "前列"),
                        tr!(language, "중위", "Middle", "中游"),
                        tr!(language, "하위", "Low", "后列"),
                    ] {
                        ui.strong(heading);
                    }
                    ui.end_row();
                    for entry in entries {
                        ui.label(entry.item_id.to_string());
                        ui.label(&entry.name);
                        for weight in [
                            &mut entry.top_weight,
                            &mut entry.high_weight,
                            &mut entry.middle_weight,
                            &mut entry.low_weight,
                        ] {
                            changed |= ui
                                .add(
                                    egui::DragValue::new(weight)
                                        .range(0..=1_000_000_u32)
                                        .speed(1.0),
                                )
                                .changed();
                        }
                        ui.end_row();
                    }
                });
        });
    changed
}

fn rank_band_label(rank_band: ItemProbabilityRankBand, language: GuiLanguage) -> &'static str {
    match rank_band {
        ItemProbabilityRankBand::Live => tr!(
            language,
            "현재 순위 자동",
            "Automatic live rank",
            "自动使用当前排名"
        ),
        ItemProbabilityRankBand::Top => tr!(language, "1등", "1st", "第1名"),
        ItemProbabilityRankBand::High => tr!(language, "상위", "High", "前列"),
        ItemProbabilityRankBand::Middle => tr!(language, "중위", "Middle", "中游"),
        ItemProbabilityRankBand::Low => tr!(language, "하위", "Low", "后列"),
        ItemProbabilityRankBand::Combined => tr!(language, "통합", "Combined", "综合"),
    }
}

fn default_game_directory() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|executable| executable.parent().map(PathBuf::from))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn default_crossover_binary() -> PathBuf {
    if cfg!(target_os = "macos") {
        PathBuf::from("/Applications/CrossOver.app/Contents/SharedSupport/CrossOver/bin/wine")
    } else if cfg!(target_os = "linux") {
        PathBuf::from("/opt/cxoffice/bin/wine")
    } else {
        PathBuf::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GuiSuccess {
    backend: RunnerBackend,
    pid: Option<u32>,
    status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum GuiRunState {
    Idle,
    Running(ConnectorStage),
    Succeeded(GuiSuccess),
    Failed(String),
}

impl GuiRunState {
    fn is_running(&self) -> bool {
        matches!(self, Self::Running(_))
    }

    fn begin(&mut self) -> bool {
        if self.is_running() {
            false
        } else {
            *self = Self::Running(ConnectorStage::PreparingInstallation);
            true
        }
    }

    fn apply(&mut self, event: ConnectorGuiEvent) {
        match event {
            ConnectorGuiEvent::Stage(stage) => *self = Self::Running(stage),
            ConnectorGuiEvent::Finished(Ok(success)) => *self = Self::Succeeded(success),
            ConnectorGuiEvent::Finished(Err(error)) => *self = Self::Failed(error),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ServerRunState {
    Stopped,
    Starting,
    Running(ServerEndpoints),
    Stopping,
    StopBlocked(String),
    Failed(String),
}

impl ServerRunState {
    fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Starting | Self::Running(_) | Self::Stopping | Self::StopBlocked(_)
        )
    }
}

enum ConnectorGuiEvent {
    Stage(ConnectorStage),
    Finished(Result<GuiSuccess, String>),
}

enum ServerControl {
    GracefulShutdown,
    ForceShutdown,
    UpdateRandomTracks(ResolvedRandomTracks),
    GrantKart {
        catalog: Arc<CatalogInventory>,
        nickname: String,
        kart_id: u16,
        options: KartGrantOptions,
    },
    SetRiderSchoolProgress {
        nickname: String,
        progress: RiderSchoolProgress,
    },
}

enum GuiEvent {
    Connector(ConnectorGuiEvent),
    ServerStarted(ServerEndpoints),
    ServerStopBlocked(String),
    RandomTracksUpdated(Result<(), String>),
    KartGranted(Result<AddKartOutcome, String>),
    RiderSchoolProgressSet {
        nickname: String,
        progress: RiderSchoolProgress,
        result: Result<u64, String>,
    },
    ServerFinished(Result<(), String>),
}

struct P5136GuiApp {
    log_path: PathBuf,
    logging_control: FileLoggingControl,
    language: GuiLanguage,
    selected_tab: GuiTab,
    connector_inputs: GuiInputs,
    connector_run_state: GuiRunState,
    server_inputs: ServerInputs,
    server_run_state: ServerRunState,
    event_sender: Sender<GuiEvent>,
    event_receiver: Receiver<GuiEvent>,
    cancellation: Option<ConnectorCancellation>,
    server_controller: Option<tokio::sync::mpsc::UnboundedSender<ServerControl>>,
    server_worker: Option<thread::JoinHandle<()>>,
    close_requested: bool,
    close_force_deadline: Option<Instant>,
    close_force_requested: bool,
    item_probability_status: String,
    lan_candidates: Vec<(String, Ipv4Addr)>,
    selected_lan_candidate: usize,
    lan_status: String,
    random_track_catalog: Option<RandomTrackCatalog>,
    selected_random_track_pool: usize,
    random_track_status: String,
    inventory_catalog: Option<Arc<CatalogInventory>>,
    inventory_catalog_data_dir: Option<PathBuf>,
    inventory_nickname: String,
    inventory_kart_query: String,
    inventory_kart_results: Vec<KartCatalogSearchResult>,
    inventory_selected_kart: Option<KartCatalogSearchResult>,
    inventory_kart_grant_options: KartGrantOptions,
    inventory_additional_karts: Vec<AdditionalKart>,
    inventory_status: String,
    rider_school_selection: RiderSchoolProgress,
    rider_school_status: String,
}

impl P5136GuiApp {
    #[cfg(test)]
    fn new(log_path: PathBuf, storage: Option<&dyn eframe::Storage>) -> Self {
        Self::new_with_logging(log_path, FileLoggingControl::default(), storage)
    }

    fn new_with_logging(
        log_path: PathBuf,
        logging_control: FileLoggingControl,
        storage: Option<&dyn eframe::Storage>,
    ) -> Self {
        let (event_sender, event_receiver) = mpsc::channel();
        let persisted = GuiPersistedSettings::load(storage);
        let language = persisted
            .as_ref()
            .map_or_else(GuiLanguage::default, |settings| settings.language);
        let connector_inputs = persisted
            .as_ref()
            .map_or_else(GuiInputs::default, |settings| settings.connector.clone());
        let server_inputs =
            persisted.map_or_else(ServerInputs::default, |settings| settings.server);
        let inventory_nickname = connector_inputs.nickname.clone();
        logging_control.set_enabled(server_inputs.file_logging.enabled());
        Self {
            log_path,
            logging_control,
            language,
            selected_tab: GuiTab::Server,
            connector_inputs,
            connector_run_state: GuiRunState::Idle,
            server_inputs,
            server_run_state: ServerRunState::Stopped,
            event_sender,
            event_receiver,
            cancellation: None,
            server_controller: None,
            server_worker: None,
            close_requested: false,
            close_force_deadline: None,
            close_force_requested: false,
            item_probability_status: tr!(
                language,
                "자동: 서버 시작 시 클라이언트의 item.rho/RHO5 확률표를 적용합니다.",
                "Automatic: applies the client's item.rho/RHO5 probability table when the server starts.",
                "自动：服务器启动时应用客户端的 item.rho/RHO5 概率表。"
            )
            .to_owned(),
            lan_candidates: Vec::new(),
            selected_lan_candidate: 0,
            lan_status: tr!(
                language,
                "LAN 자동 설정은 활성 네트워크 어댑터의 IPv4를 사용합니다.",
                "Automatic LAN setup uses an IPv4 address from an active network adapter.",
                "局域网自动设置会使用活动网络适配器的 IPv4 地址。"
            )
            .to_owned(),
            random_track_catalog: None,
            selected_random_track_pool: 0,
            random_track_status: tr!(
                language,
                "자동: 서버 시작 시 클라이언트의 track_common.rho 기본 목록을 적용합니다.",
                "Automatic: applies the client's default track_common.rho pools when the server starts.",
                "自动：服务器启动时应用客户端 track_common.rho 的默认地图池。"
            )
            .to_owned(),
            inventory_catalog: None,
            inventory_catalog_data_dir: None,
            inventory_nickname,
            inventory_kart_query: String::new(),
            inventory_kart_results: Vec::new(),
            inventory_selected_kart: None,
            inventory_kart_grant_options: KartGrantOptions::default(),
            inventory_additional_karts: Vec::new(),
            inventory_status: tr!(
                language,
                "카트 목록을 불러온 뒤 닉네임별 추가 소유 카트를 관리할 수 있습니다.",
                "Load the kart list to manage additional owned karts for each nickname.",
                "加载车辆列表后，可按昵称管理额外拥有的车辆。"
            )
            .to_owned(),
            rider_school_selection: RiderSchoolProgress::ALL_CLEAR,
            rider_school_status: tr!(
                language,
                "닉네임과 라이선스 등급을 선택해 적용하세요.",
                "Select a nickname and license grade, then apply it.",
                "请选择昵称和驾照等级，然后应用。"
            )
            .to_owned(),
        }
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.event_receiver.try_recv() {
            match event {
                GuiEvent::Connector(event) => {
                    let finished = matches!(&event, ConnectorGuiEvent::Finished(_));
                    self.connector_run_state.apply(event);
                    if finished {
                        self.cancellation = None;
                    }
                }
                GuiEvent::ServerStarted(endpoints) => {
                    self.server_run_state = if self.close_requested {
                        ServerRunState::Stopping
                    } else {
                        ServerRunState::Running(endpoints)
                    };
                }
                GuiEvent::ServerStopBlocked(error) => {
                    self.server_run_state = ServerRunState::StopBlocked(error);
                }
                GuiEvent::RandomTracksUpdated(result) => match result {
                    Ok(()) => {
                        tr!(
                            self.language,
                            "실행 중 서버에 랜덤 트랙 설정을 적용했습니다. 다음 경기 시작부터 사용합니다.",
                            "Applied the random-track settings to the running server. They take effect from the next race.",
                            "已将随机地图设置应用到运行中的服务器，将从下一场比赛开始生效。"
                        )
                        .clone_into(&mut self.random_track_status);
                    }
                    Err(error) => {
                        self.random_track_status = tr_format!(
                            self.language,
                            "랜덤 트랙 실시간 적용 실패: {error}",
                            "Failed to apply random tracks live: {error}",
                            "实时应用随机地图失败：{error}"
                        );
                    }
                },
                GuiEvent::KartGranted(result) => self.apply_inventory_grant_result(result),
                GuiEvent::RiderSchoolProgressSet {
                    nickname,
                    progress,
                    result,
                } => self.apply_rider_school_result(&nickname, progress, result),
                GuiEvent::ServerFinished(result) => self.finish_server_worker(result),
            }
        }
    }

    fn finish_server_worker(&mut self, result: Result<(), String>) {
        self.server_controller = None;
        self.close_force_deadline = None;
        self.close_force_requested = false;
        let worker_joined = self
            .server_worker
            .take()
            .is_none_or(|worker| worker.join().is_ok());
        self.server_run_state = if worker_joined {
            match result {
                Ok(()) => ServerRunState::Stopped,
                Err(error) => ServerRunState::Failed(error),
            }
        } else {
            ServerRunState::Failed(
                tr!(
                    self.language,
                    "서버 작업 스레드가 종료 중 패닉했습니다",
                    "The server worker thread panicked while stopping",
                    "服务器工作线程在停止时发生崩溃"
                )
                .to_owned(),
            )
        };
    }

    fn start_connector(&mut self, context: &egui::Context) {
        if self.connector_run_state.is_running() {
            return;
        }
        let language = self.language;
        let plan = match self.connector_inputs.connector_plan(language) {
            Ok(plan) => plan,
            Err(error) => {
                self.connector_run_state = GuiRunState::Failed(format!("{error:#}"));
                return;
            }
        };
        if !self.connector_run_state.begin() {
            return;
        }

        let worker_notifier = GuiNotifier {
            sender: self.event_sender.clone(),
            context: context.clone(),
        };
        let cancellation = ConnectorCancellation::new();
        let worker_cancellation = cancellation.clone();
        self.cancellation = Some(cancellation);
        if let Err(error) = thread::Builder::new()
            .name("p5136-connector-worker".to_owned())
            .spawn(move || {
                let outcome =
                    run_connector_worker(&plan, &worker_notifier, &worker_cancellation, language)
                        .map_err(|error| format!("{error:#}"));
                worker_notifier.send(GuiEvent::Connector(ConnectorGuiEvent::Finished(outcome)));
            })
        {
            if let Some(cancellation) = self.cancellation.take() {
                cancellation.cancel();
            }
            self.connector_run_state = GuiRunState::Failed(tr_format!(
                language,
                "접속기 작업 스레드를 시작하지 못했습니다: {error}",
                "Failed to start the connector worker thread: {error}",
                "无法启动连接器工作线程：{error}"
            ));
        }
    }

    // Keeping translated labels beside their controls makes this form intentionally verbose.
    #[allow(clippy::too_many_lines)]
    fn connector_input_panel(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        egui::Grid::new("connector-inputs")
            .num_columns(2)
            .spacing([14.0, 10.0])
            .show(ui, |ui| {
                ui.label(tr!(language, "게임 디렉터리", "Game directory", "游戏目录"));
                ui.add(
                    egui::TextEdit::singleline(&mut self.connector_inputs.game_directory)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label(tr!(
                    language,
                    "실행 파일 (선택)",
                    "Executable (optional)",
                    "可执行文件（可选）"
                ));
                ui.add(
                    egui::TextEdit::singleline(&mut self.connector_inputs.game_executable)
                        .hint_text(tr!(
                            language,
                            "비우면 KartRider.exe",
                            "Leave empty for KartRider.exe",
                            "留空则使用 KartRider.exe"
                        ))
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label(tr!(language, "닉네임", "Nickname", "昵称"));
                ui.add(
                    egui::TextEdit::singleline(&mut self.connector_inputs.nickname)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label(tr!(language, "계정 역할", "Account role", "账号角色"));
                ui.checkbox(
                    &mut self.connector_inputs.observer_mode,
                    tr!(
                        language,
                        "옵저버 모드 (pmap 718)",
                        "Observer mode (pmap 718)",
                        "观察者模式（pmap 718）"
                    ),
                )
                .on_hover_text(tr!(
                    language,
                    "옵저버 방장 역할로 로그인합니다. 현재는 역할/슬롯 진입만 지원하며, 옵저버 채팅과 개인전 맵 교체는 패킷 캡처 후 보강할 예정입니다.",
                    "Logs in as an observer room master. Role and slot entry are supported; observer chat and solo map replacement still need packet captures.",
                    "以观察者房主身份登录。目前支持角色和槽位进入；观察者聊天及个人赛换图仍需抓包完善。"
                ));
                ui.end_row();

                ui.label(tr!(language, "서버 IPv4", "Server IPv4", "服务器 IPv4"));
                ui.add(
                    egui::TextEdit::singleline(&mut self.connector_inputs.server)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label(tr!(language, "기준 포트", "Base port", "基准端口"));
                ui.add(
                    egui::TextEdit::singleline(&mut self.connector_inputs.configured_port)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label(tr!(language, "실행 방식", "Launch method", "启动方式"));
                egui::ComboBox::from_id_salt("connector-runner")
                    .selected_text(self.connector_inputs.runner.label(language))
                    .show_ui(ui, |ui| {
                        for runner in GuiRunner::ALL {
                            ui.selectable_value(
                                &mut self.connector_inputs.runner,
                                runner,
                                runner.label(language),
                            );
                        }
                    });
                ui.end_row();

                self.connector_runner_inputs(ui);
            });

        if self.connector_inputs.runner == GuiRunner::NativeElevated && !cfg!(windows) {
            ui.colored_label(
                egui::Color32::YELLOW,
                tr!(
                    language,
                    "이 운영체제에서는 Windows UAC 실행을 사용할 수 없습니다.",
                    "Windows UAC launch is unavailable on this operating system.",
                    "此操作系统不支持 Windows UAC 启动。"
                ),
            );
        }
        if self.connector_inputs.runner == GuiRunner::Auto {
            let resolution = if cfg!(windows) { "Windows UAC" } else { "Wine" };
            ui.weak(tr_format!(
                language,
                "자동 모드는 이 운영체제에서 {resolution}(으)로 실행합니다.",
                "Automatic mode uses {resolution} on this operating system.",
                "自动模式在此操作系统上使用 {resolution}。"
            ));
        }
        if self.connector_inputs.runner == GuiRunner::Sikarugir && !cfg!(target_os = "macos") {
            ui.colored_label(
                egui::Color32::YELLOW,
                tr!(
                    language,
                    "Sikarugir wrapper 실행은 macOS에서만 사용할 수 있습니다.",
                    "The Sikarugir wrapper is available only on macOS.",
                    "Sikarugir 包装器仅可在 macOS 上使用。"
                ),
            );
        }
    }

    fn connector_runner_inputs(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        match self.connector_inputs.runner {
            GuiRunner::Wine => {
                ui.label(tr!(
                    language,
                    "Wine 실행 파일",
                    "Wine executable",
                    "Wine 可执行文件"
                ));
                ui.add(
                    egui::TextEdit::singleline(&mut self.connector_inputs.wine_binary)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label(tr!(
                    language,
                    "Wine prefix (선택)",
                    "Wine prefix (optional)",
                    "Wine prefix（可选）"
                ));
                ui.add(
                    egui::TextEdit::singleline(&mut self.connector_inputs.wine_prefix)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();
            }
            GuiRunner::CrossOver => {
                ui.label(tr!(
                    language,
                    "CrossOver Wine 실행 파일",
                    "CrossOver Wine executable",
                    "CrossOver Wine 可执行文件"
                ));
                ui.add(
                    egui::TextEdit::singleline(&mut self.connector_inputs.crossover_binary)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label(tr!(
                    language,
                    "CrossOver 보틀",
                    "CrossOver bottle",
                    "CrossOver 容器"
                ));
                ui.add(
                    egui::TextEdit::singleline(&mut self.connector_inputs.crossover_bottle)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();
            }
            GuiRunner::Sikarugir => {
                ui.label(tr!(
                    language,
                    "Sikarugir wrapper 앱",
                    "Sikarugir wrapper app",
                    "Sikarugir 包装器应用"
                ));
                ui.add(
                    egui::TextEdit::singleline(&mut self.connector_inputs.sikarugir_app)
                        .hint_text(tr!(
                            language,
                            "예: /Applications/KartRider.app",
                            "Example: /Applications/KartRider.app",
                            "示例：/Applications/KartRider.app"
                        ))
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();
            }
            _ => {}
        }
    }

    fn connector_status_panel(&self, ui: &mut egui::Ui) {
        let language = self.language;
        match &self.connector_run_state {
            GuiRunState::Idle => {
                ui.weak(tr!(language, "준비됨.", "Ready.", "就绪。"));
            }
            GuiRunState::Running(stage) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(stage_label(*stage, language));
                });
            }
            GuiRunState::Succeeded(success) => {
                let pid = success.pid.map_or_else(
                    || tr!(language, "확인 불가", "Unavailable", "不可用").to_owned(),
                    |pid| pid.to_string(),
                );
                ui.colored_label(
                    egui::Color32::LIGHT_GREEN,
                    tr_format!(
                        language,
                        "{} 방식으로 실행했습니다 — PID {pid}, {}.",
                        "Launched with {} — PID {pid}, {}.",
                        "已通过 {} 启动 — PID {pid}，{}。",
                        success.backend,
                        success.status
                    ),
                );
            }
            GuiRunState::Failed(error) => {
                ui.colored_label(egui::Color32::LIGHT_RED, error);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuiTab {
    Server,
    ServerManagement,
    Connector,
}

impl P5136GuiApp {
    fn start_server(&mut self, context: &egui::Context) {
        if self.server_run_state.is_active() {
            return;
        }
        let language = self.language;
        let mut config = match self.server_inputs.server_config(language) {
            Ok(config) => config,
            Err(error) => {
                self.server_run_state = ServerRunState::Failed(format!("{error:#}"));
                return;
            }
        };
        if let (Some(catalog), Some(loaded_data_dir), Some(configured_data_dir)) = (
            self.inventory_catalog.as_ref(),
            self.inventory_catalog_data_dir.as_ref(),
            config.client_data_dir.as_ref(),
        ) && std::fs::canonicalize(configured_data_dir)
            .is_ok_and(|configured| configured == *loaded_data_dir)
        {
            config.resolved_catalog = Some(Arc::clone(catalog));
        }
        if self.server_inputs.item_probability_source == GuiItemProbabilitySource::AutoClient {
            if let Some(data_dir) = &config.client_data_dir {
                match load_client_item_probabilities(data_dir) {
                    Ok(configuration) => {
                        self.item_probability_status = tr_format!(
                            language,
                            "자동 적용 확인: {} (개인 {}개 / 팀 {}개).",
                            "Automatic table verified: {} ({} solo / {} team entries).",
                            "已确认自动应用：{}（个人 {} 项 / 组队 {} 项）。",
                            data_dir.display(),
                            configuration.individual.len(),
                            configuration.team.len(),
                        );
                        // Pin the exact snapshot reported by the GUI to this
                        // start attempt instead of re-reading mutable files in
                        // the server worker.
                        config.item_probabilities = Some(configuration);
                    }
                    Err(error) => {
                        self.server_run_state = ServerRunState::Failed(tr_format!(
                            language,
                            "클라이언트 아이템 확률표를 읽지 못했습니다: {error:#}",
                            "Failed to read the client item-probability table: {error:#}",
                            "无法读取客户端道具概率表：{error:#}"
                        ));
                        return;
                    }
                }
            } else {
                tr!(
                    language,
                    "클라이언트 Data 경로가 없어 안전 기본 확률표를 사용합니다.",
                    "No client Data path is configured; using the safe fallback probability table.",
                    "未配置客户端 Data 路径，将使用安全的默认概率表。"
                )
                .clone_into(&mut self.item_probability_status);
            }
        }
        self.server_run_state = ServerRunState::Starting;
        let (controller, controls) = tokio::sync::mpsc::unbounded_channel();
        self.server_controller = Some(controller);

        let worker_notifier = GuiNotifier {
            sender: self.event_sender.clone(),
            context: context.clone(),
        };
        match thread::Builder::new()
            .name("p5136-server-worker".to_owned())
            .spawn(move || {
                let outcome = run_server_worker(config, controls, &worker_notifier, language)
                    .map_err(|error| format!("{error:#}"));
                worker_notifier.send(GuiEvent::ServerFinished(outcome));
            }) {
            Ok(worker) => self.server_worker = Some(worker),
            Err(error) => {
                self.server_controller = None;
                self.server_run_state = ServerRunState::Failed(tr_format!(
                    language,
                    "서버 작업 스레드를 시작하지 못했습니다: {error}",
                    "Failed to start the server worker thread: {error}",
                    "无法启动服务器工作线程：{error}"
                ));
            }
        }
    }

    fn request_server_control(&mut self, command: ServerControl) {
        let requests_shutdown = matches!(
            &command,
            ServerControl::GracefulShutdown | ServerControl::ForceShutdown
        );
        let Some(controller) = &self.server_controller else {
            self.server_run_state = ServerRunState::Failed(
                tr!(
                    self.language,
                    "서버 제어 채널을 사용할 수 없습니다. 서버 작업이 끝날 때까지 기다리세요",
                    "The server control channel is unavailable. Wait for the server task to finish.",
                    "服务器控制通道不可用，请等待服务器任务结束。"
                )
                .to_owned(),
            );
            return;
        };
        if controller.send(command).is_err() {
            self.server_run_state = ServerRunState::Failed(
                tr!(
                    self.language,
                    "요청을 전달하기 전에 서버 제어 채널이 닫혔습니다",
                    "The server control channel closed before the request was delivered",
                    "请求发送前服务器控制通道已关闭"
                )
                .to_owned(),
            );
            return;
        }
        if requests_shutdown {
            self.server_run_state = ServerRunState::Stopping;
        }
    }

    fn handle_close_request(&mut self, context: &egui::Context) {
        if context.input(|input| input.viewport().close_requested()) && self.server_worker.is_some()
        {
            context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_requested = true;
        }
        if !self.close_requested {
            return;
        }

        if self.server_worker.is_none() {
            self.close_requested = false;
            context.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        match self.server_run_state.clone() {
            ServerRunState::Starting | ServerRunState::Running(_) => {
                self.request_server_control(ServerControl::GracefulShutdown);
                self.close_force_deadline = Some(Instant::now() + GUI_CLOSE_GRACE_PERIOD);
            }
            ServerRunState::Stopping if !self.close_force_requested => {
                let deadline = self
                    .close_force_deadline
                    .get_or_insert(Instant::now() + GUI_CLOSE_GRACE_PERIOD);
                if Instant::now() >= *deadline {
                    self.request_server_control(ServerControl::ForceShutdown);
                    self.close_force_requested = true;
                }
            }
            ServerRunState::StopBlocked(_) if !self.close_force_requested => {
                self.request_server_control(ServerControl::ForceShutdown);
                self.close_force_requested = true;
            }
            ServerRunState::Stopped => context.send_viewport_cmd(egui::ViewportCommand::Close),
            ServerRunState::Failed(_) => {
                self.close_requested = false;
                self.close_force_deadline = None;
                self.close_force_requested = false;
            }
            ServerRunState::Stopping | ServerRunState::StopBlocked(_) => {}
        }
    }

    fn load_client_item_probability_defaults(&mut self) {
        let language = self.language;
        let outcome = (|| -> Result<(ItemProbabilityConfiguration, PathBuf)> {
            let paths = client_paths::resolve_client_runtime_paths(
                optional_path_ref(&self.server_inputs.client_path),
                optional_path_ref(&self.server_inputs.client_data_dir),
            )?;
            let data_dir = paths.client_data_dir.ok_or_else(|| {
                anyhow!(tr!(
                    language,
                    "먼저 클라이언트 디렉터리 또는 Data 경로를 설정하세요",
                    "Set the client directory or Data path first",
                    "请先设置客户端目录或 Data 路径"
                ))
            })?;
            let configuration = load_client_item_probabilities(&data_dir).with_context(|| {
                tr_format!(
                    language,
                    "{}을(를) 읽지 못했습니다",
                    "Failed to read {}",
                    "无法读取 {}",
                    data_dir.display()
                )
            })?;
            Ok((configuration, data_dir))
        })();
        match outcome {
            Ok((configuration, data_dir)) => {
                self.server_inputs.item_probabilities = configuration;
                self.server_inputs.item_probability_source = GuiItemProbabilitySource::Edited;
                self.item_probability_status = tr_format!(
                    language,
                    "클라이언트 확률표를 불러와 편집값으로 고정했습니다: {}.",
                    "Loaded the client probability table and pinned it as editable values: {}.",
                    "已加载客户端概率表并固定为可编辑值：{}。",
                    data_dir.display()
                );
            }
            Err(error) => {
                self.item_probability_status = tr_format!(
                    language,
                    "item.rho/RHO5 로드 실패: {error:#}",
                    "Failed to load item.rho/RHO5: {error:#}",
                    "加载 item.rho/RHO5 失败：{error:#}"
                );
            }
        }
    }

    fn apply_best_lan_ipv4(&mut self) {
        let language = self.language;
        match discover_lan_ipv4_candidates(language) {
            Ok(candidates) => {
                self.lan_candidates = candidates;
                self.selected_lan_candidate = 0;
                let (_, address) = &self.lan_candidates[0];
                self.server_inputs.bind_address = address.to_string();
                self.server_inputs.advertised_address = address.to_string();
                self.lan_status = tr_format!(
                    language,
                    "바인드 주소와 광고 주소를 {address}로 설정했습니다. 다른 어댑터도 아래에서 선택할 수 있습니다.",
                    "Set both bind and advertised addresses to {address}. You can select another adapter below.",
                    "已将绑定地址和公布地址设为 {address}。可在下方选择其他适配器。"
                );
            }
            Err(error) => {
                self.lan_status = tr_format!(
                    language,
                    "LAN 주소 검색 실패: {error:#}",
                    "Failed to discover a LAN address: {error:#}",
                    "查找局域网地址失败：{error:#}"
                );
            }
        }
    }

    fn load_random_track_catalog(&mut self) {
        let language = self.language;
        let outcome = (|| -> Result<RandomTrackCatalog> {
            let paths = client_paths::resolve_client_runtime_paths(
                optional_path_ref(&self.server_inputs.client_path),
                optional_path_ref(&self.server_inputs.client_data_dir),
            )?;
            let data_dir = paths.client_data_dir.ok_or_else(|| {
                anyhow!(tr!(
                    language,
                    "먼저 클라이언트 디렉터리 또는 Data 경로를 설정하세요",
                    "Set the client directory or Data path first",
                    "请先设置客户端目录或 Data 路径"
                ))
            })?;
            load_client_random_track_catalog(&data_dir).with_context(|| {
                tr_format!(
                    language,
                    "{}의 track_common.rho를 읽지 못했습니다",
                    "Failed to read track_common.rho from {}",
                    "无法读取 {} 中的 track_common.rho",
                    data_dir.display()
                )
            })
        })();
        match outcome {
            Ok(catalog) => {
                self.random_track_status = tr_format!(
                    language,
                    "랜덤 트랙 {}개, 선택 풀 {}개를 읽었습니다: {}",
                    "Loaded {} random tracks across {} selectable pools: {}",
                    "已加载 {} 张随机地图和 {} 个可选地图池：{}",
                    catalog.tracks().len(),
                    catalog.pools().len(),
                    catalog.source_path().display(),
                );
                self.selected_random_track_pool = self
                    .selected_random_track_pool
                    .min(catalog.pools().len().saturating_sub(1));
                self.random_track_catalog = Some(catalog);
            }
            Err(error) => {
                self.random_track_status = tr_format!(
                    language,
                    "랜덤 트랙 로드 실패: {error:#}",
                    "Failed to load random tracks: {error:#}",
                    "加载随机地图失败：{error:#}"
                );
            }
        }
    }

    fn load_inventory_catalog(&mut self) {
        let language = self.language;
        let outcome = (|| -> Result<(Arc<CatalogInventory>, PathBuf, String)> {
            let paths = client_paths::resolve_client_runtime_paths(
                optional_path_ref(&self.server_inputs.client_path),
                optional_path_ref(&self.server_inputs.client_data_dir),
            )?;
            let data_dir = paths.client_data_dir.ok_or_else(|| {
                anyhow!(tr!(
                    language,
                    "먼저 클라이언트 루트, Profile 또는 Data 경로를 설정하세요",
                    "Set the client root, Profile, or Data path first",
                    "请先设置客户端根目录、Profile 或 Data 路径"
                ))
            })?;
            let loaded = load_client_kart_catalog(&data_dir).with_context(|| {
                tr_format!(
                    language,
                    "{}의 RHO 카트 데이터를 읽지 못했습니다",
                    "Failed to read RHO kart data from {}",
                    "无法读取 {} 中的 RHO 车辆数据",
                    data_dir.display()
                )
            })?;
            let stats = loaded.stats();
            let summary = tr_format!(
                language,
                "이름 {}, 물리 {}, 상점 {}개/{}분류, 자동 카트 {}개, 수동 확인 카트 {}개, 변환 {}개",
                "{} names, {} physics specs, {} shop items/{} categories, {} automatic karts, {} manual-review karts, {} transforms",
                "名称 {}、物理 {}、商店 {} 项/{} 类、自动车辆 {}、需手动确认车辆 {}、转换规则 {}",
                stats.names,
                stats.specs,
                stats.inventory_items,
                stats.inventory_categories,
                stats.auto_grant_karts,
                stats.quarantined_karts,
                stats.transform_rules,
            );
            Ok((
                Arc::new(loaded.into_catalog()),
                std::fs::canonicalize(&data_dir)?,
                summary,
            ))
        })();
        match outcome {
            Ok((catalog, data_dir, summary)) => {
                let kart_count = catalog
                    .grant_items()
                    .filter(|item| item.category == 3)
                    .count();
                self.inventory_catalog = Some(catalog);
                self.inventory_catalog_data_dir = Some(data_dir.clone());
                self.refresh_inventory_search_results();
                self.inventory_status = tr_format!(
                    language,
                    "RHO에서 자동 지급 가능한 카트 {kart_count}개를 읽었습니다. 보수적 검사에서 빠진 카트는 정확한 ID로 수동 추가할 수 있습니다 ({summary}): {}",
                    "Loaded {kart_count} automatically grantable karts from RHO. Karts excluded by conservative checks can be added manually by exact ID ({summary}): {}",
                    "已从 RHO 加载 {kart_count} 辆可自动发放的车辆。被保守检查排除的车辆可用精确 ID 手动添加（{summary}）：{}",
                    data_dir.display()
                );
            }
            Err(error) => {
                self.inventory_catalog = None;
                self.inventory_catalog_data_dir = None;
                self.inventory_kart_results.clear();
                self.inventory_selected_kart = None;
                self.inventory_status = tr_format!(
                    language,
                    "RHO 카트 목록 로드 실패: {error:#}",
                    "Failed to load the RHO kart list: {error:#}",
                    "加载 RHO 车辆列表失败：{error:#}"
                );
            }
        }
    }

    fn refresh_inventory_search_results(&mut self) {
        self.inventory_kart_results = self
            .inventory_catalog
            .as_ref()
            .map_or_else(Vec::new, |catalog| {
                search_karts(catalog, &self.inventory_kart_query, 30)
            });
        self.inventory_selected_kart = None;
    }

    fn refresh_inventory_profile(&mut self) {
        let language = self.language;
        let outcome = (|| -> Result<(bool, Vec<AdditionalKart>)> {
            let catalog = self.inventory_catalog.as_ref().ok_or_else(|| {
                anyhow!(tr!(
                    language,
                    "먼저 카트 목록을 불러오세요",
                    "Load the kart list first",
                    "请先加载车辆列表"
                ))
            })?;
            let nickname = required_text(
                &self.inventory_nickname,
                tr!(
                    language,
                    "인벤토리 닉네임",
                    "Inventory nickname",
                    "库存昵称"
                ),
                language,
            )?;
            let store = ProfileStore::new(required_path(
                &self.server_inputs.profile_root,
                tr!(
                    language,
                    "프로필 저장 경로",
                    "Profile storage path",
                    "配置文件保存路径"
                ),
                language,
            )?);
            if !store.profile_exists(nickname)? {
                return Ok((false, Vec::new()));
            }
            let loaded = store.reload(nickname)?;
            Ok((true, additional_karts(catalog, &loaded.profile)))
        })();
        match outcome {
            Ok((true, karts)) => {
                let count = karts.len();
                self.inventory_additional_karts = karts;
                self.inventory_status = tr_format!(
                    language,
                    "{}의 추가 소유 카트 {count}개를 읽었습니다.",
                    "Loaded {count} additional owned karts for {}.",
                    "已为 {} 加载 {count} 辆额外拥有的车辆。",
                    self.inventory_nickname.trim()
                );
            }
            Ok((false, _)) => {
                self.inventory_additional_karts.clear();
                self.inventory_status = tr_format!(
                    language,
                    "{} 프로필은 아직 없습니다. 카트를 추가하면 새 프로필을 만듭니다.",
                    "The profile for {} does not exist yet. Adding a kart will create it.",
                    "尚无 {} 的配置文件。添加车辆时将创建新配置文件。",
                    self.inventory_nickname.trim()
                );
            }
            Err(error) => {
                self.inventory_status = tr_format!(
                    language,
                    "인벤토리 조회 실패: {error:#}",
                    "Failed to query the inventory: {error:#}",
                    "查询库存失败：{error:#}"
                );
            }
        }
    }

    fn apply_selected_rider_school_progress(&mut self) {
        let language = self.language;
        let request = (|| -> Result<(String, RiderSchoolProgress)> {
            let nickname = required_text(
                &self.inventory_nickname,
                tr!(language, "계정 닉네임", "Account nickname", "账号昵称"),
                language,
            )?
            .to_owned();
            let progress = self.rider_school_selection;
            if !progress.is_grade_boundary() {
                return Err(anyhow!(tr!(
                    language,
                    "지원하지 않는 라이선스 경계값입니다.",
                    "The selected license boundary is unsupported.",
                    "所选驾照边界值不受支持。"
                )));
            }
            Ok((nickname, progress))
        })();
        let (nickname, progress) = match request {
            Ok(request) => request,
            Err(error) => {
                self.rider_school_status = tr_format!(
                    language,
                    "라이선스 적용 실패: {error:#}",
                    "Failed to apply the license: {error:#}",
                    "应用驾照失败：{error:#}"
                );
                return;
            }
        };

        if matches!(self.server_run_state, ServerRunState::Running(_)) {
            self.request_server_control(ServerControl::SetRiderSchoolProgress {
                nickname,
                progress,
            });
            tr!(
                language,
                "실행 중 서버의 프로필 큐에 라이선스 변경을 요청했습니다.",
                "Requested the license change through the running server's profile queue.",
                "已通过运行中服务器的资料队列请求修改驾照。"
            )
            .clone_into(&mut self.rider_school_status);
            return;
        }
        if self.server_run_state.is_active() {
            tr!(
                language,
                "서버가 시작 또는 종료 중입니다. 완료 후 다시 적용하세요.",
                "The server is starting or stopping. Apply again after it finishes.",
                "服务器正在启动或停止，请完成后再次应用。"
            )
            .clone_into(&mut self.rider_school_status);
            return;
        }

        let outcome = (|| -> Result<u64> {
            let store = ProfileStore::new(required_path(
                &self.server_inputs.profile_root,
                tr!(
                    language,
                    "프로필 저장 경로",
                    "Profile storage path",
                    "资料存储路径"
                ),
                language,
            )?);
            let (saved, _) = store.update(&nickname, |profile| {
                profile.rider_school = progress;
            })?;
            Ok(saved.revision)
        })();
        self.apply_rider_school_result(
            &nickname,
            progress,
            outcome.map_err(|error| format!("{error:#}")),
        );
    }

    fn apply_rider_school_result(
        &mut self,
        nickname: &str,
        progress: RiderSchoolProgress,
        result: Result<u64, String>,
    ) {
        let language = self.language;
        match result {
            Ok(revision) => {
                let grade = rider_school_grade_label(language, progress);
                self.rider_school_status = tr_format!(
                    language,
                    "{} 계정의 라이선스를 {grade}(으)로 저장했습니다. 프로필 revision {revision}. 접속 중이었다면 재접속 후 반영됩니다.",
                    "Saved the license for {} as {grade}. Profile revision {revision}. Reconnect if the client was online.",
                    "已将 {} 的驾照保存为 {grade}。资料 revision {revision}。若客户端在线，请重新连接。",
                    nickname
                );
            }
            Err(error) => {
                self.rider_school_status = tr_format!(
                    language,
                    "라이선스 적용 실패: {error}",
                    "Failed to apply the license: {error}",
                    "应用驾照失败：{error}"
                );
            }
        }
    }

    fn add_selected_inventory_kart(&mut self) {
        let language = self.language;
        let request = (|| -> Result<(Arc<CatalogInventory>, String, u16, KartGrantOptions)> {
            self.validate_current_inventory_catalog_source()?;
            let catalog = self.inventory_catalog.as_ref().ok_or_else(|| {
                anyhow!(tr!(
                    language,
                    "먼저 카트 목록을 불러오세요",
                    "Load the kart list first",
                    "请先加载车辆列表"
                ))
            })?;
            let selected = self.inventory_selected_kart.as_ref().ok_or_else(|| {
                anyhow!(tr!(
                    language,
                    "검색 결과에서 추가할 카트를 선택하세요",
                    "Select a kart from the search results",
                    "请从搜索结果中选择要添加的车辆"
                ))
            })?;
            let nickname = required_text(
                &self.inventory_nickname,
                tr!(
                    language,
                    "인벤토리 닉네임",
                    "Inventory nickname",
                    "库存昵称"
                ),
                language,
            )?
            .to_owned();
            let supports_enhancements = catalog.supports_legacy_kart_enhancements(selected.kart_id);
            Ok((
                Arc::clone(catalog),
                nickname,
                selected.kart_id,
                KartGrantOptions {
                    apply_floater: supports_enhancements
                        && self.inventory_kart_grant_options.apply_floater,
                    floater_codes: self.inventory_kart_grant_options.floater_codes,
                    apply_grade_five: supports_enhancements
                        && self.inventory_kart_grant_options.apply_grade_five,
                },
            ))
        })();
        let (catalog, nickname, kart_id, options) = match request {
            Ok(request) => request,
            Err(error) => {
                self.inventory_status = tr_format!(
                    language,
                    "카트 추가 실패: {error:#}",
                    "Failed to add the kart: {error:#}",
                    "添加车辆失败：{error:#}"
                );
                return;
            }
        };

        if matches!(self.server_run_state, ServerRunState::Running(_)) {
            self.request_server_control(ServerControl::GrantKart {
                catalog,
                nickname,
                kart_id,
                options,
            });
            tr!(
                language,
                "실행 중 서버의 프로필 저장 큐에 카트 지급을 요청했습니다.",
                "Requested the running server to grant the kart through its profile queue.",
                "已请求运行中的服务器通过配置文件队列发放车辆。"
            )
            .clone_into(&mut self.inventory_status);
            return;
        }
        if self.server_run_state.is_active() {
            tr!(
                language,
                "서버가 시작 또는 종료 중입니다. 실행 완료 후 다시 지급하세요.",
                "The server is starting or stopping. Try granting the kart again after it finishes.",
                "服务器正在启动或停止，请在完成后再次发放车辆。"
            )
            .clone_into(&mut self.inventory_status);
            return;
        }

        let outcome = (|| -> Result<AddKartOutcome> {
            let store = ProfileStore::new(required_path(
                &self.server_inputs.profile_root,
                tr!(
                    language,
                    "프로필 저장 경로",
                    "Profile storage path",
                    "配置文件保存路径"
                ),
                language,
            )?);
            Ok(add_kart_with_options(
                &store, &catalog, &nickname, kart_id, options,
            )?)
        })();
        self.apply_inventory_grant_result(outcome.map_err(|error| format!("{error:#}")));
    }

    fn apply_inventory_grant_result(&mut self, outcome: Result<AddKartOutcome, String>) {
        let language = self.language;
        match outcome {
            Ok(added) => {
                self.inventory_additional_karts = added.additional_karts().to_vec();
                let kart = added.kart();
                let revision = added.saved().revision;
                let enhancements = added.enhancements();
                let mut applied = Vec::<String>::new();
                if let Some([first, second, third]) = enhancements.floater_codes {
                    applied.push(format!("Floater {first}/{second}/{third}"));
                }
                if enhancements.grade_five {
                    applied.push(tr!(language, "5강", "grade 5", "强化 5").to_owned());
                }
                let enhancement_suffix = if applied.is_empty() {
                    String::new()
                } else {
                    tr_format!(
                        language,
                        " 강화 적용: {}.",
                        " Enhancements applied: {}.",
                        " 已应用强化：{}。",
                        applied.join(", ")
                    )
                };
                let durability_suffix = if enhancements.durability_warnings.is_empty() {
                    String::new()
                } else {
                    tr_format!(
                        language,
                        " 강화 파일 동기화 경고: {}",
                        " Enhancement-file synchronization warning: {}",
                        " 强化文件同步警告：{}",
                        enhancements.durability_warnings.join("; ")
                    )
                };
                self.inventory_status = match &added {
                    AddKartOutcome::Durable { .. } => tr_format!(
                        language,
                        "{}에 {} (ID {}, serial {})을 추가했습니다. 프로필 revision {revision}.{enhancement_suffix}{durability_suffix}",
                        "For {}, added {} (ID {}, serial {}). Profile revision {revision}.{enhancement_suffix}{durability_suffix}",
                        "已向 {} 添加 {}（ID {}，serial {}）。配置文件 revision {revision}.{enhancement_suffix}{durability_suffix}",
                        self.inventory_nickname.trim(),
                        kart.name,
                        kart.kart_id,
                        kart.serial,
                    ),
                    AddKartOutcome::DurabilityUncertain { error, .. } => tr_format!(
                        language,
                        "카트는 revision {revision}에 추가됐지만 디렉터리 동기화를 확인하지 못했습니다: {error}. 재추가하지 말고 새로고침으로 확인하세요.{enhancement_suffix}{durability_suffix}",
                        "The kart was added at revision {revision}, but directory synchronization could not be confirmed: {error}. Do not add it again; refresh to verify.{enhancement_suffix}{durability_suffix}",
                        "车辆已在 revision {revision} 添加，但无法确认目录同步：{error}。请勿重复添加，请刷新确认。{enhancement_suffix}{durability_suffix}"
                    ),
                };
            }
            Err(error) => {
                self.inventory_status = tr_format!(
                    language,
                    "카트 추가 실패: {error}",
                    "Failed to add the kart: {error}",
                    "添加车辆失败：{error}"
                );
            }
        }
    }

    fn apply_live_random_tracks(&mut self) {
        let language = self.language;
        if !matches!(self.server_run_state, ServerRunState::Running(_)) {
            tr!(
                language,
                "서버가 실행 중일 때만 실시간 적용할 수 있습니다.",
                "Live settings can be applied only while the server is running.",
                "仅可在服务器运行时实时应用设置。"
            )
            .clone_into(&mut self.random_track_status);
            return;
        }
        let resolved = self
            .random_track_catalog
            .as_ref()
            .ok_or_else(|| {
                anyhow!(tr!(
                    language,
                    "먼저 클라이언트 랜덤 트랙 목록을 불러오세요",
                    "Load the client random-track list first",
                    "请先加载客户端随机地图列表"
                ))
            })
            .and_then(|catalog| {
                catalog
                    .resolve(&self.server_inputs.random_tracks)
                    .map_err(Into::into)
            });
        match resolved {
            Ok(resolved) => {
                self.request_server_control(ServerControl::UpdateRandomTracks(resolved));
                tr!(
                    language,
                    "실행 중 서버에 랜덤 트랙 설정 적용을 요청했습니다.",
                    "Requested the running server to apply the random-track settings.",
                    "已请求运行中的服务器应用随机地图设置。"
                )
                .clone_into(&mut self.random_track_status);
            }
            Err(error) => {
                self.random_track_status = tr_format!(
                    language,
                    "랜덤 트랙 실시간 적용 실패: {error:#}",
                    "Failed to apply random tracks live: {error:#}",
                    "实时应用随机地图失败：{error:#}"
                );
            }
        }
    }

    fn validate_current_inventory_catalog_source(&self) -> Result<()> {
        let language = self.language;
        let loaded_data_dir = self.inventory_catalog_data_dir.as_ref().ok_or_else(|| {
            anyhow!(tr!(
                language,
                "먼저 카트 목록을 불러오세요",
                "Load the kart list first",
                "请先加载车辆列表"
            ))
        })?;
        let paths = client_paths::resolve_client_runtime_paths(
            optional_path_ref(&self.server_inputs.client_path),
            optional_path_ref(&self.server_inputs.client_data_dir),
        )?;
        let current_data_dir = paths.client_data_dir.ok_or_else(|| {
            anyhow!(tr!(
                language,
                "클라이언트 또는 Data 경로가 비어 있습니다",
                "The client or Data path is empty",
                "客户端或 Data 路径为空"
            ))
        })?;
        let current_data_dir = std::fs::canonicalize(&current_data_dir).with_context(|| {
            tr_format!(
                language,
                "{}의 실제 경로를 확인하지 못했습니다",
                "Failed to resolve the canonical path of {}",
                "无法解析 {} 的实际路径",
                current_data_dir.display()
            )
        })?;
        if current_data_dir != *loaded_data_dir {
            return Err(anyhow!(tr_format!(
                language,
                "클라이언트 Data 경로가 바뀌었습니다. RHO 카트 목록을 다시 불러오세요: {} → {}",
                "The client Data path changed. Reload the RHO kart list: {} → {}",
                "客户端 Data 路径已更改，请重新加载 RHO 车辆列表：{} → {}",
                loaded_data_dir.display(),
                current_data_dir.display()
            )));
        }
        Ok(())
    }

    fn invalidate_inventory_catalog(&mut self) {
        self.inventory_catalog = None;
        self.inventory_catalog_data_dir = None;
        self.inventory_kart_results.clear();
        self.inventory_selected_kart = None;
        self.inventory_additional_karts.clear();
        tr!(
            self.language,
            "클라이언트 경로가 바뀌었습니다. 카트 목록을 다시 불러오세요.",
            "The client path changed. Reload the kart list.",
            "客户端路径已更改，请重新加载车辆列表。"
        )
        .clone_into(&mut self.inventory_status);
    }

    fn inventory_editor(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.collapsing(
            tr!(
                language,
                "닉네임별 계정/인벤토리 편집",
                "Account and inventory editor by nickname",
                "按昵称编辑账号与仓库"
            ),
            |ui| {
            self.inventory_catalog_controls(ui);
            ui.separator();
            self.inventory_profile_controls(ui);
            ui.separator();
            self.inventory_kart_search_controls(ui);
            self.inventory_additional_kart_list(ui);
            let failed = status_is_error(&self.inventory_status);
            let uncertain = status_is_uncertain(&self.inventory_status);
            ui.colored_label(
                if failed {
                    egui::Color32::LIGHT_RED
                } else if uncertain {
                    egui::Color32::YELLOW
                } else {
                    egui::Color32::GRAY
                },
                &self.inventory_status,
            );
            ui.weak(tr!(
                language,
                "기본 카트는 모두 serial 1로 제공됩니다. 여기서 추가한 복사본은 serial 2 이상을 받아 서로 다른 강화·파츠 상태를 가질 수 있습니다.",
                "Default karts use serial 1. Copies added here receive serial 2 or higher and can keep separate enhancement and parts states.",
                "默认车辆使用 serial 1。此处添加的副本会获得 serial 2 或更高编号，并可保存独立的强化和部件状态。"
            ));
            ui.weak(tr!(
                language,
                "편집 결과는 해당 닉네임의 프로필 revision에 즉시 저장됩니다. 접속 중이었다면 재접속 후 반영됩니다.",
                "Edits are saved immediately to the nickname's profile revision. Reconnect if the client was already online.",
                "编辑结果会立即保存到该昵称的配置文件 revision。若客户端已在线，请重新连接后查看。"
            ));
        },
        );
    }

    fn inventory_catalog_controls(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.horizontal(|ui| {
            if ui
                .button(tr!(
                    language,
                    "카트 목록 불러오기",
                    "Load kart list",
                    "加载车辆列表"
                ))
                .clicked()
            {
                self.load_inventory_catalog();
            }
            ui.weak(tr!(
                language,
                "클라이언트 Data의 kart.rho/item.rho/RHO5를 직접 읽음",
                "Reads kart.rho, item.rho, and RHO5 directly from client Data",
                "直接读取客户端 Data 中的 kart.rho、item.rho 和 RHO5"
            ));
        });
    }

    fn inventory_profile_controls(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.horizontal(|ui| {
            ui.label(tr!(language, "닉네임", "Nickname", "昵称"));
            if ui
                .add(egui::TextEdit::singleline(&mut self.inventory_nickname).desired_width(180.0))
                .changed()
            {
                self.inventory_additional_karts.clear();
                tr!(
                    language,
                    "닉네임이 바뀌었습니다. 프로필을 새로고침하거나 카트를 추가하세요.",
                    "The nickname changed. Refresh the profile or add a kart.",
                    "昵称已更改，请刷新配置文件或添加车辆。"
                )
                .clone_into(&mut self.inventory_status);
            }
            if ui
                .button(tr!(
                    language,
                    "접속기 닉네임 사용",
                    "Use connector nickname",
                    "使用连接器昵称"
                ))
                .clicked()
            {
                self.inventory_nickname
                    .clone_from(&self.connector_inputs.nickname);
                self.inventory_additional_karts.clear();
            }
            if ui
                .button(tr!(
                    language,
                    "프로필 새로고침",
                    "Refresh profile",
                    "刷新配置文件"
                ))
                .clicked()
            {
                self.refresh_inventory_profile();
            }
        });

        ui.horizontal(|ui| {
            ui.label(tr!(language, "라이선스 등급", "License grade", "驾照等级"));
            egui::ComboBox::from_id_salt("account-rider-school-grade")
                .selected_text(rider_school_grade_label(
                    language,
                    self.rider_school_selection,
                ))
                .width(180.0)
                .show_ui(ui, |ui| {
                    for progress in RiderSchoolProgress::GRADE_BOUNDARIES {
                        ui.selectable_value(
                            &mut self.rider_school_selection,
                            progress,
                            rider_school_grade_label(language, progress),
                        );
                    }
                });
            if ui
                .button(tr!(language, "라이선스 적용", "Apply license", "应用驾照"))
                .clicked()
            {
                self.apply_selected_rider_school_progress();
            }
        });
        ui.colored_label(
            if status_is_error(&self.rider_school_status) {
                egui::Color32::LIGHT_RED
            } else {
                egui::Color32::GRAY
            },
            &self.rider_school_status,
        );
        ui.weak(tr!(
            language,
            "미취득을 선택하면 라이선스 테스트를 처음부터 진행할 수 있습니다. PRO 선택 시 레이싱 마스터 엠블럼은 지급하지 않습니다.",
            "Unlicensed starts the license tests from the beginning. Selecting PRO does not grant the Racing Master emblem.",
            "选择未取得可从头开始驾照考试。选择 PRO 不会发放 Racing Master 徽章。"
        ));
    }

    fn inventory_kart_search_controls(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.horizontal(|ui| {
            ui.label(tr!(
                language,
                "카트 이름 또는 ID",
                "Kart name or ID",
                "车辆名称或 ID"
            ));
            if ui
                .add(
                    egui::TextEdit::singleline(&mut self.inventory_kart_query)
                        .hint_text(tr!(
                            language,
                            "예: 기간테스 V1 또는 1410",
                            "Example: Gigantes V1 or 1410",
                            "示例：Gigantes V1 或 1410"
                        ))
                        .desired_width(260.0),
                )
                .changed()
            {
                self.refresh_inventory_search_results();
            }
        });

        let selected_text = self.inventory_selected_kart.as_ref().map_or_else(
            || {
                tr!(
                    language,
                    "검색 후보 선택",
                    "Select a search result",
                    "选择搜索结果"
                )
                .to_owned()
            },
            |kart| {
                format!(
                    "{} (ID {}){}",
                    kart.name,
                    kart.kart_id,
                    if kart.auto_granted {
                        ""
                    } else {
                        tr!(language, " [수동 확인]", " [manual review]", " [手动确认]")
                    }
                )
            },
        );
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("inventory-kart-search-results")
                .selected_text(selected_text)
                .width(330.0)
                .show_ui(ui, |ui| {
                    if self.inventory_kart_results.is_empty() {
                        ui.weak(tr!(
                            language,
                            "일치하는 카트가 없습니다",
                            "No matching karts",
                            "没有匹配的车辆"
                        ));
                    }
                    for candidate in &self.inventory_kart_results {
                        ui.selectable_value(
                            &mut self.inventory_selected_kart,
                            Some(candidate.clone()),
                            format!(
                                "{} (ID {}){}",
                                candidate.name,
                                candidate.kart_id,
                                if candidate.auto_granted {
                                    ""
                                } else {
                                    tr!(language, " [수동 확인]", " [manual review]", " [手动确认]")
                                }
                            ),
                        );
                    }
                });
            if ui
                .add_enabled(
                    self.inventory_selected_kart.is_some(),
                    egui::Button::new(tr!(
                        language,
                        "선택 카트 추가",
                        "Add selected kart",
                        "添加所选车辆"
                    )),
                )
                .clicked()
            {
                self.add_selected_inventory_kart();
            }
        });
        self.inventory_kart_enhancement_controls(ui);
    }

    #[allow(clippy::too_many_lines)]
    fn inventory_kart_enhancement_controls(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        let enhancements_supported = self
            .inventory_selected_kart
            .as_ref()
            .zip(self.inventory_catalog.as_ref())
            .is_some_and(|(kart, catalog)| catalog.supports_legacy_kart_enhancements(kart.kart_id));
        if !enhancements_supported {
            self.inventory_kart_grant_options = KartGrantOptions::default();
        }
        ui.horizontal(|ui| {
            ui.label(tr!(
                language,
                "지급 시 강화",
                "Enhance on grant",
                "发放时强化"
            ));
            ui.add_enabled(
                enhancements_supported,
                egui::Checkbox::new(
                    &mut self.inventory_kart_grant_options.apply_floater,
                    tr!(language, "플로터 적용", "Apply Floater", "应用强化属性"),
                ),
            );
            ui.add_enabled(
                enhancements_supported,
                egui::Checkbox::new(
                    &mut self.inventory_kart_grant_options.apply_grade_five,
                    tr!(language, "5강 적용", "Apply grade 5", "应用强化 5"),
                ),
            );
            if ui
                .add_enabled(
                    enhancements_supported,
                    egui::Button::new(tr!(language, "333 프리셋", "333 preset", "333 预设")),
                )
                .clicked()
            {
                self.inventory_kart_grant_options.apply_floater = true;
                self.inventory_kart_grant_options.floater_codes = [603, 903, 703];
            }
        });
        ui.add_enabled_ui(
            enhancements_supported && self.inventory_kart_grant_options.apply_floater,
            |ui| {
                let mut codes = self.inventory_kart_grant_options.floater_codes;
                ui.label(tr!(
                    language,
                    "플로터 슬롯",
                    "Floater slots",
                    "强化属性槽"
                ));
                for slot in 0..3 {
                    ui.horizontal(|ui| {
                        ui.label(tr_format!(
                            language,
                            "슬롯 {}",
                            "Slot {}",
                            "槽位 {}",
                            slot + 1
                        ));
                        let mut selected = codes[slot];
                        egui::ComboBox::from_id_salt(("grant-floater-slot", slot))
                            .selected_text(floater_code_label(language, selected))
                            .width(360.0)
                            .show_ui(ui, |ui| {
                                let mut none_candidate = codes;
                                none_candidate[slot] = 0;
                                if p5136_floater_spec(none_candidate).is_some() {
                                    ui.selectable_value(
                                        &mut selected,
                                        0,
                                        floater_code_label(language, 0),
                                    );
                                }
                                for &code in ALL_FLOATER_CODES {
                                    let mut candidate = codes;
                                    candidate[slot] = code;
                                    if p5136_floater_spec(candidate).is_some() {
                                        ui.selectable_value(
                                            &mut selected,
                                            code,
                                            floater_code_label(language, code),
                                        );
                                    }
                                }
                            });
                        codes[slot] = selected;
                    });
                }
                self.inventory_kart_grant_options.floater_codes = codes;
                ui.weak(tr!(
                    language,
                    "동일한 스피드 효과 종류나 같은 아이템 효과는 두 슬롯에 중복 지정할 수 없습니다.",
                    "The same speed-effect family or item effect cannot be selected in more than one slot.",
                    "同一种竞速效果类别或相同道具效果不能重复选择到多个槽位。"
                ));
            },
        );
        if let Some(selected) = &self.inventory_selected_kart {
            ui.weak(tr_format!(
                language,
                "이름 → kart_id 변환: {} → {}",
                "Name → kart_id mapping: {} → {}",
                "名称 → kart_id 映射：{} → {}",
                selected.name,
                selected.kart_id
            ));
            if !selected.auto_granted {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    tr!(
                        language,
                        "이 카트는 리소스/개발 데이터 보수 검사에서 자동 지급이 제외됐습니다. 실제 클라이언트 지원을 확인한 경우에만 정확한 ID로 수동 추가하세요.",
                        "This kart was excluded from automatic grants by conservative resource/development-data checks. Add it by exact ID only after confirming client support.",
                        "此车辆因保守的资源/开发数据检查而未自动发放。仅在确认客户端支持后，才使用精确 ID 手动添加。"
                    ),
                );
            }
            if !enhancements_supported {
                ui.weak(
                    tr!(
                        language,
                        "이 카트는 클라이언트 BodyParam.DescEnchantCap이 없어 플로터/5강 옵션을 적용하지 않습니다.",
                        "This kart has no client BodyParam.DescEnchantCap, so floater and grade-5 options are unavailable.",
                        "此车辆没有客户端 BodyParam.DescEnchantCap，因此无法使用强化属性和强化 5 选项。"
                    ),
                );
            }
        }
    }

    fn inventory_additional_kart_list(&self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.label(tr_format!(
            language,
            "현재 추가 소유 카트: {}개",
            "Additional owned karts: {}",
            "当前额外拥有的车辆：{}",
            self.inventory_additional_karts.len()
        ));
        if self.inventory_additional_karts.is_empty() {
            ui.weak(tr!(
                language,
                "추가 소유분이 없습니다. 기본 serial 1 카트는 이 목록에서 생략합니다.",
                "No additional copies are owned. Default serial-1 karts are omitted from this list.",
                "没有额外拥有的副本。默认 serial 1 车辆不会显示在此列表中。"
            ));
            return;
        }
        egui::ScrollArea::vertical()
            .max_height(180.0)
            .show(ui, |ui| {
                for kart in &self.inventory_additional_karts {
                    ui.label(format!(
                        "{} · ID {} · serial {}",
                        kart.name, kart.kart_id, kart.serial
                    ));
                }
            });
    }

    fn random_track_editor(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.collapsing(
            tr!(
                language,
                "랜덤 트랙 설정",
                "Random-track settings",
                "随机地图设置"
            ),
            |ui| {
            ui.horizontal(|ui| {
                if ui
                    .button(tr!(
                        language,
                        "클라이언트 목록 불러오기",
                        "Load client list",
                        "加载客户端列表"
                    ))
                    .clicked()
                {
                    self.load_random_track_catalog();
                }
                if ui
                    .button(tr!(
                        language,
                        "모든 수동 설정 초기화",
                        "Reset all overrides",
                        "重置全部手动设置"
                    ))
                    .clicked()
                {
                    self.server_inputs.random_tracks = RandomTrackConfiguration::default();
                    tr!(
                        language,
                        "모든 풀을 클라이언트 기본 목록으로 되돌렸습니다.",
                        "Restored every pool to the client defaults.",
                        "已将所有地图池恢复为客户端默认值。"
                    )
                    .clone_into(&mut self.random_track_status);
                }
                if ui
                    .add_enabled(
                        matches!(self.server_run_state, ServerRunState::Running(_))
                            && self.random_track_catalog.is_some(),
                        egui::Button::new(tr!(
                            language,
                            "실행 중 서버에 적용",
                            "Apply to running server",
                            "应用到运行中的服务器"
                        )),
                    )
                    .on_hover_text(tr!(
                        language,
                        "현재 선택을 다음 경기 시작부터 사용합니다.",
                        "Uses the current selection from the next race onward.",
                        "从下一场比赛开始使用当前选择。"
                    ))
                    .clicked()
                {
                    self.apply_live_random_tracks();
                }
            });

            let Some(catalog) = &self.random_track_catalog else {
                ui.weak(tr!(
                    language,
                    "서버 시작 시에는 자동으로 track_common.rho를 읽습니다. 목록을 편집하려면 위 버튼으로 미리 불러오세요.",
                    "The server reads track_common.rho automatically at startup. Load it above first if you want to edit the pools.",
                    "服务器启动时会自动读取 track_common.rho。如需编辑地图池，请先点击上方按钮加载。"
                ));
                ui.colored_label(
                    if status_is_error(&self.random_track_status) { egui::Color32::LIGHT_RED } else { egui::Color32::GRAY },
                    &self.random_track_status,
                );
                return;
            };
            if catalog.pools().is_empty() {
                return;
            }
            self.selected_random_track_pool = self.selected_random_track_pool.min(catalog.pools().len() - 1);
            let pool = catalog.pools()[self.selected_random_track_pool].clone();
            egui::ComboBox::from_id_salt("random-track-pool")
                .selected_text(&pool.korean_name)
                .show_ui(ui, |ui| {
                    for (index, candidate) in catalog.pools().iter().enumerate() {
                        ui.selectable_value(&mut self.selected_random_track_pool, index, &candidate.korean_name);
                    }
                });

            Self::random_track_pool_checker(
                ui,
                catalog,
                &pool,
                &mut self.server_inputs.random_tracks,
                language,
            );
            ui.colored_label(
                if status_is_error(&self.random_track_status) { egui::Color32::LIGHT_RED } else { egui::Color32::GRAY },
                &self.random_track_status,
            );
        },
        );
    }

    fn random_track_pool_checker(
        ui: &mut egui::Ui,
        catalog: &RandomTrackCatalog,
        pool: &RandomTrackPool,
        configuration: &mut RandomTrackConfiguration,
        language: GuiLanguage,
    ) {
        let compatible = catalog
            .compatible_tracks(pool)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let original_override = Self::random_track_override_index(configuration, pool);
        let (mut select_all, mut clear_all, mut restore_defaults) = (false, false, false);
        ui.horizontal(|ui| {
            select_all = ui
                .button(tr!(language, "모두 선택", "Select all", "全选"))
                .clicked();
            clear_all = ui
                .button(tr!(language, "모두 해제", "Clear all", "全部取消"))
                .clicked();
            restore_defaults = ui
                .add_enabled(
                    original_override.is_some(),
                    egui::Button::new(tr!(
                        language,
                        "클라이언트 기본값",
                        "Client defaults",
                        "客户端默认值"
                    )),
                )
                .clicked();
        });
        if restore_defaults && let Some(index) = original_override {
            configuration.pools.remove(index);
        }

        let override_index = Self::random_track_override_index(configuration, pool);
        let selected_ids = override_index.map_or(pool.default_track_ids.as_slice(), |index| {
            configuration.pools[index].track_ids.as_slice()
        });
        let mut selected = selected_ids
            .iter()
            .map(|id| id.to_ascii_lowercase())
            .collect::<HashSet<_>>();
        let mut changed = false;
        if !restore_defaults && select_all {
            selected = compatible
                .iter()
                .map(|track| track.id.to_ascii_lowercase())
                .collect();
            changed = true;
        } else if !restore_defaults && clear_all {
            selected.clear();
            changed = true;
        }
        changed |= Self::random_track_checkbox_list(ui, &compatible, &mut selected);
        if changed {
            Self::write_random_track_override(
                configuration,
                pool,
                &compatible,
                &selected,
                override_index,
            );
        }
        Self::random_track_selection_status(ui, configuration, pool, compatible.len(), language);
    }

    fn random_track_checkbox_list(
        ui: &mut egui::Ui,
        tracks: &[RandomTrackDefinition],
        selected: &mut HashSet<String>,
    ) -> bool {
        let mut changed = false;
        egui::ScrollArea::vertical()
            .max_height(300.0)
            .show(ui, |ui| {
                for track in tracks {
                    let key = track.id.to_ascii_lowercase();
                    let mut checked = selected.contains(&key);
                    if ui
                        .checkbox(
                            &mut checked,
                            format!("{} ({})", track.korean_name, track.id),
                        )
                        .changed()
                    {
                        if checked {
                            selected.insert(key);
                        } else {
                            selected.remove(&key);
                        }
                        changed = true;
                    }
                }
            });
        changed
    }

    fn write_random_track_override(
        configuration: &mut RandomTrackConfiguration,
        pool: &RandomTrackPool,
        compatible: &[RandomTrackDefinition],
        selected: &HashSet<String>,
        override_index: Option<usize>,
    ) {
        let track_ids = compatible
            .iter()
            .filter(|track| selected.contains(&track.id.to_ascii_lowercase()))
            .map(|track| track.id.clone())
            .collect::<Vec<_>>();
        if let Some(index) = override_index {
            configuration.pools[index].track_ids = track_ids;
        } else {
            configuration.pools.push(RandomTrackPoolOverride {
                game_type: pool.game_type,
                selector: pool.selector,
                track_ids,
            });
        }
    }

    fn random_track_selection_status(
        ui: &mut egui::Ui,
        configuration: &RandomTrackConfiguration,
        pool: &RandomTrackPool,
        compatible_count: usize,
        language: GuiLanguage,
    ) {
        let current_override = Self::random_track_override_index(configuration, pool)
            .map(|index| &configuration.pools[index]);
        let selected_count = current_override.map_or(pool.default_track_ids.len(), |configured| {
            configured.track_ids.len()
        });
        ui.horizontal(|ui| {
            ui.weak(if current_override.is_some() {
                tr!(language, "사용자 지정 목록", "Custom list", "自定义列表")
            } else {
                tr!(
                    language,
                    "클라이언트 기본 목록",
                    "Client default list",
                    "客户端默认列表"
                )
            });
            ui.weak(tr_format!(
                language,
                "· 선택: {selected_count}/{compatible_count}개",
                "· Selected: {selected_count}/{compatible_count}",
                "· 已选择：{selected_count}/{compatible_count}"
            ));
        });
        if selected_count == 0 {
            ui.colored_label(
                egui::Color32::LIGHT_RED,
                tr!(
                    language,
                    "맵을 1개 이상 선택해야 서버를 시작할 수 있습니다.",
                    "Select at least one track before starting the server.",
                    "启动服务器前至少选择一张地图。"
                ),
            );
        }
    }

    fn random_track_override_index(
        configuration: &RandomTrackConfiguration,
        pool: &RandomTrackPool,
    ) -> Option<usize> {
        configuration.pools.iter().position(|configured| {
            configured.game_type == pool.game_type && configured.selector == pool.selector
        })
    }

    fn load_item_probability_xml_override(&mut self) {
        let language = self.language;
        let outcome = required_path(
            &self.server_inputs.item_probability_xml,
            tr!(
                language,
                "아이템 확률 XML",
                "Item-probability XML",
                "道具概率 XML"
            ),
            language,
        )
        .and_then(|path| {
            load_item_probability_xml(&path).with_context(|| {
                tr_format!(
                    language,
                    "{}을(를) 불러오지 못했습니다",
                    "Failed to load {}",
                    "无法加载 {}",
                    path.display()
                )
            })
        });
        match outcome {
            Ok(configuration) => {
                self.server_inputs.item_probabilities = configuration;
                self.server_inputs.item_probability_source = GuiItemProbabilitySource::Edited;
                tr!(
                    language,
                    "이식 가능한 XML 확률표를 불러와 고정했습니다.",
                    "Loaded and pinned the portable XML probability table.",
                    "已加载并固定可移植的 XML 概率表。"
                )
                .clone_into(&mut self.item_probability_status);
            }
            Err(error) => {
                self.item_probability_status = tr_format!(
                    language,
                    "XML 로드 실패: {error:#}",
                    "Failed to load XML: {error:#}",
                    "加载 XML 失败：{error:#}"
                );
            }
        }
    }

    fn item_probability_rank_policy_editor(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.checkbox(
            &mut self.server_inputs.trust_client_item_rank,
            tr!(
                language,
                "클라이언트가 보고한 현재 순위 신뢰 (LAN/친구용)",
                "Trust client-reported live rank (LAN/friends)",
                "信任客户端上报的当前排名（局域网/好友）"
            ),
        )
        .on_hover_text(tr!(
            language,
            "체크하면 클라이언트의 1등/상위/중위/하위 순위를 사용합니다. 해제하면 통합 확률을 사용합니다.",
            "When enabled, uses the client's 1st/high/middle/low rank band. When disabled, uses combined probabilities.",
            "勾选后使用客户端的第1名/前列/中游/后列排名；取消后使用综合概率。"
        ));
    }

    // Keeping translated labels beside their controls makes this editor intentionally verbose.
    #[allow(clippy::too_many_lines)]
    fn item_probability_editor(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.collapsing(
            tr!(
                language,
                "아이템 확률표",
                "Item-probability table",
                "道具概率表"
            ),
            |ui| {
            let mut edited = false;
            self.item_probability_rank_policy_editor(ui);
            ui.weak(tr!(
                language,
                "아이템 플로터 확률은 한국 P5136 enchant.xml의 코드별 원본 값을 사용합니다.",
                "Item Floater chances use the per-code stock Korean P5136 enchant.xml values.",
                "道具强化属性概率使用韩服 P5136 enchant.xml 中各代码的原始数值。"
            ));
            let pinned =
                self.server_inputs.item_probability_source == GuiItemProbabilitySource::Edited;
            ui.add_enabled_ui(pinned, |ui| {
                ui.horizontal(|ui| {
                    ui.label(tr!(
                        language,
                        "순위 가중치",
                        "Rank weights",
                        "排名权重"
                    ));
                    egui::ComboBox::from_id_salt("item-probability-rank-band")
                        .selected_text(rank_band_label(
                            self.server_inputs.item_probabilities.rank_band,
                            language,
                        ))
                        .show_ui(ui, |ui| {
                            for rank_band in [
                                ItemProbabilityRankBand::Live,
                                ItemProbabilityRankBand::Top,
                                ItemProbabilityRankBand::High,
                                ItemProbabilityRankBand::Middle,
                                ItemProbabilityRankBand::Low,
                                ItemProbabilityRankBand::Combined,
                            ] {
                                edited |= ui
                                    .selectable_value(
                                        &mut self.server_inputs.item_probabilities.rank_band,
                                        rank_band,
                                        rank_band_label(rank_band, language),
                                    )
                                    .changed();
                            }
                        });
                });
            });

            ui.horizontal(|ui| {
                if ui
                    .button(tr!(
                        language,
                        "클라이언트 item.rho/RHO5 불러와 고정",
                        "Load and pin client item.rho/RHO5",
                        "加载并固定客户端 item.rho/RHO5"
                    ))
                    .clicked()
                {
                    self.load_client_item_probability_defaults();
                }
                if ui
                    .button(tr!(
                        language,
                        "서버 시작 시 자동 적용",
                        "Apply automatically at server start",
                        "服务器启动时自动应用"
                    ))
                    .clicked()
                {
                    self.server_inputs.item_probability_source =
                        GuiItemProbabilitySource::AutoClient;
                    tr!(
                        language,
                        "자동: 서버를 시작할 때마다 클라이언트 item.rho/RHO5를 다시 읽습니다.",
                        "Automatic: rereads client item.rho/RHO5 whenever the server starts.",
                        "自动：每次服务器启动时重新读取客户端 item.rho/RHO5。"
                    )
                    .clone_into(&mut self.item_probability_status);
                }
                if ui
                    .button(tr!(
                        language,
                        "안전 기본값 사용",
                        "Use safe defaults",
                        "使用安全默认值"
                    ))
                    .clicked()
                {
                    self.server_inputs.item_probabilities =
                        ItemProbabilityConfiguration::safe_fallback();
                    self.server_inputs.item_probability_source = GuiItemProbabilitySource::Edited;
                    tr!(
                        language,
                        "개인 14개/팀 18개 안전 기본 확률표를 고정했습니다.",
                        "Pinned the safe fallback table with 14 solo and 18 team entries.",
                        "已固定安全默认概率表：个人 14 项、组队 18 项。"
                    )
                    .clone_into(&mut self.item_probability_status);
                }
            });

            ui.horizontal(|ui| {
                ui.label(tr!(
                    language,
                    "이식 가능한 XML",
                    "Portable XML",
                    "可移植 XML"
                ));
                ui.add(
                    egui::TextEdit::singleline(&mut self.server_inputs.item_probability_xml)
                        .hint_text("item-probabilities.xml")
                        .desired_width(360.0),
                );
                if ui
                    .button(tr!(language, "XML 불러오기", "Load XML", "加载 XML"))
                    .clicked()
                {
                    self.load_item_probability_xml_override();
                }
            });

            if pinned {
                ui.horizontal(|ui| {
                    ui.selectable_value(
                        &mut self.server_inputs.show_team_item_probabilities,
                        false,
                        tr!(language, "아이템 개인전", "Item solo", "道具个人赛"),
                    );
                    ui.selectable_value(
                        &mut self.server_inputs.show_team_item_probabilities,
                        true,
                        tr!(language, "아이템 팀전", "Item team", "道具组队赛"),
                    );
                });

                let entries = if self.server_inputs.show_team_item_probabilities {
                    &mut self.server_inputs.item_probabilities.team
                } else {
                    &mut self.server_inputs.item_probabilities.individual
                };
                edited |= item_probability_grid(ui, entries, language);
            } else {
                ui.weak(tr!(
                    language,
                    "자동 모드입니다. 서버 시작 시 클라이언트 확률표를 읽고 적용 여부와 항목 수를 표시합니다. 편집하려면 위의 '불러와 고정'을 누르세요.",
                    "Automatic mode is active. At startup the server reads the client table and reports the applied source and entry counts. Use 'Load and pin' above to edit it.",
                    "当前为自动模式。服务器启动时会读取客户端概率表，并显示应用来源及条目数。如需编辑，请点击上方“加载并固定”。"
                ));
            }
            if edited {
                self.server_inputs.item_probability_source = GuiItemProbabilitySource::Edited;
                tr!(
                    language,
                    "편집한 확률표를 다음 서버 시작에 사용하도록 고정했습니다.",
                    "Pinned the edited probability table for the next server start.",
                    "已固定编辑后的概率表，将在下次服务器启动时使用。"
                )
                .clone_into(&mut self.item_probability_status);
            }
            let status_color = if status_is_error(&self.item_probability_status) {
                egui::Color32::LIGHT_RED
            } else {
                egui::Color32::GRAY
            };
            ui.colored_label(status_color, &self.item_probability_status);
            ui.weak(tr!(
                language,
                "ID와 아이템 이름은 읽기 전용입니다. 가중치는 서버 바인드 전에 범위와 합계를 검증합니다.",
                "IDs and item names are read-only. Weight ranges and sums are validated before the server binds.",
                "ID 和道具名称为只读。服务器绑定端口前会校验权重范围及总和。"
            ));
        },
        );
    }

    // Keeping translated labels beside their controls makes this form intentionally verbose.
    #[allow(clippy::too_many_lines)]
    fn server_input_panel(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.horizontal(|ui| {
            if ui
                .button(tr!(
                    language,
                    "내 LAN IPv4로 자동 설정",
                    "Auto-configure my LAN IPv4",
                    "自动设置本机局域网 IPv4"
                ))
                .clicked()
            {
                self.apply_best_lan_ipv4();
            }
            if !self.lan_candidates.is_empty() {
                egui::ComboBox::from_id_salt("lan-ipv4-candidate")
                    .selected_text({
                        let (name, address) = &self.lan_candidates[self.selected_lan_candidate];
                        format!("{name}: {address}")
                    })
                    .show_ui(ui, |ui| {
                        for (index, (name, address)) in self.lan_candidates.iter().enumerate() {
                            if ui
                                .selectable_value(
                                    &mut self.selected_lan_candidate,
                                    index,
                                    format!("{name}: {address}"),
                                )
                                .clicked()
                            {
                                self.server_inputs.bind_address = address.to_string();
                                self.server_inputs.advertised_address = address.to_string();
                                self.lan_status = tr_format!(
                                    language,
                                    "바인드 주소와 광고 주소를 {address}로 설정했습니다.",
                                    "Set both bind and advertised addresses to {address}.",
                                    "已将绑定地址和公布地址设为 {address}。"
                                );
                            }
                        }
                    });
            }
        });
        ui.weak(&self.lan_status);
        egui::Grid::new("server-inputs")
            .num_columns(2)
            .spacing([14.0, 10.0])
            .show(ui, |ui| {
                ui.label(tr!(
                    language,
                    "서버 바인드 주소",
                    "Server bind address",
                    "服务器绑定地址"
                ));
                ui.add(
                    egui::TextEdit::singleline(&mut self.server_inputs.bind_address)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label(tr!(
                    language,
                    "클라이언트에 알릴 IPv4",
                    "Advertised client IPv4",
                    "向客户端公布的 IPv4"
                ));
                ui.add(
                    egui::TextEdit::singleline(&mut self.server_inputs.advertised_address)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label(tr!(language, "기준 포트", "Base port", "基准端口"));
                ui.add(
                    egui::TextEdit::singleline(&mut self.server_inputs.configured_port)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label(tr!(
                    language,
                    "프로필 저장 경로",
                    "Profile storage path",
                    "配置文件保存路径"
                ));
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut self.server_inputs.profile_root)
                            .desired_width(f32::INFINITY),
                    )
                    .changed()
                {
                    self.inventory_additional_karts.clear();
                    tr!(
                        language,
                        "프로필 저장 경로가 바뀌었습니다. 인벤토리를 새로고침하세요.",
                        "The profile storage path changed. Refresh the inventory.",
                        "配置文件保存路径已更改，请刷新库存。"
                    )
                    .clone_into(&mut self.inventory_status);
                }
                ui.end_row();

                ui.label(tr!(
                    language,
                    "클라이언트 또는 Profile 경로 (필수)",
                    "Client or Profile path (required)",
                    "客户端或 Profile 路径（必填）"
                ));
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut self.server_inputs.client_path)
                            .desired_width(f32::INFINITY),
                    )
                    .changed()
                {
                    self.invalidate_inventory_catalog();
                }
                ui.end_row();

                ui.label(tr!(
                    language,
                    "원격 프로필 생성",
                    "Remote profile creation",
                    "远程创建配置文件"
                ));
                ui.checkbox(
                    &mut self.server_inputs.allow_remote_profile_creation,
                    tr!(
                        language,
                        "LAN의 새 닉네임 허용",
                        "Allow new nicknames on LAN",
                        "允许局域网中的新昵称"
                    ),
                );
                ui.end_row();
            });

        self.server_advanced_input_panel(ui);
        ui.weak(tr!(
            language,
            "포트: 게임 UDP = 기준, 로그인 TCP/P2P UDP = 기준 + 1, 메신저 TCP = 기준 + 2.",
            "Ports: game UDP = base, login TCP/P2P UDP = base + 1, messenger TCP = base + 2.",
            "端口：游戏 UDP = 基准，登录 TCP/P2P UDP = 基准 + 1，聊天 TCP = 基准 + 2。"
        ));
        ui.weak(tr!(
            language,
            "클라이언트 루트, Profile 또는 Data 폴더를 지정하면 RHO 카트·아이템 데이터를 자동으로 읽습니다. KartCatalog.xml은 필요하지 않습니다.",
            "Point to the client root, Profile, or Data folder to read RHO kart and item data automatically. KartCatalog.xml is not required.",
            "指定客户端根目录、Profile 或 Data 文件夹后，会自动读取 RHO 车辆及道具数据，无需 KartCatalog.xml。"
        ));
        ui.weak(tr!(
            language,
            "주소에는 IP 리터럴만 사용할 수 있습니다. P5136 패킷은 광고 주소를 IPv4 4바이트로 기록하므로 도메인을 직접 넣을 수 없습니다.",
            "Addresses must be IP literals. P5136 stores the advertised address as four IPv4 bytes, so a domain name cannot be entered directly.",
            "地址必须是 IP 字面值。P5136 数据包以 4 字节 IPv4 保存公布地址，因此不能直接输入域名。"
        ));
        ui.weak(tr!(
            language,
            "방 제목에 S0~S8 토큰을 넣으면 다음 경기 시작 패킷의 주행 물리를 해당 등급으로 바꿉니다. 예: '[S2] 친선'.",
            "Put an S0–S8 token in the room title to use that physics grade from the next race. Example: '[S2] Friendly'.",
            "在房间标题中加入 S0–S8 标记，可从下一场比赛起使用对应物理等级。例如：“[S2] 友谊赛”。"
        ));
        ui.weak(tr!(
            language,
            "서버·접속기 입력 설정은 GUI 종료 시 저장되어 다음 실행에 복원됩니다. 실행 상태, 로그, 임시 검색 결과는 저장하지 않습니다.",
            "Server and connector inputs are saved when the GUI closes and restored next time. Runtime state, logs, and temporary search results are not saved.",
            "关闭 GUI 时会保存服务器和连接器输入，并在下次启动时恢复。运行状态、日志及临时搜索结果不会保存。"
        ));
    }

    fn server_advanced_input_panel(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.collapsing(
            tr!(
                language,
                "고급 시간 제한 및 접속 수",
                "Advanced timeouts and connection limits",
                "高级超时与连接限制"
            ),
            |ui| {
                egui::Grid::new("server-advanced-inputs")
                    .num_columns(2)
                    .spacing([14.0, 10.0])
                    .show(ui, |ui| {
                        ui.label(tr!(
                            language,
                            "클라이언트 Data 경로 재정의 (선택)",
                            "Client Data path override (optional)",
                            "客户端 Data 路径覆盖（可选）"
                        ));
                        ui.add(
                            egui::TextEdit::singleline(&mut self.server_inputs.client_data_dir)
                                .desired_width(f32::INFINITY),
                        );
                        ui.end_row();

                        ui.label(tr!(
                            language,
                            "첫 메시지 지연 (ms)",
                            "First-message delay (ms)",
                            "首条消息延迟（毫秒）"
                        ));
                        ui.add(
                            egui::TextEdit::singleline(
                                &mut self.server_inputs.first_message_delay_ms,
                            )
                            .desired_width(f32::INFINITY),
                        );
                        ui.end_row();

                        ui.label(tr!(
                            language,
                            "로그인 제한 시간 (초)",
                            "Login timeout (seconds)",
                            "登录超时（秒）"
                        ));
                        ui.add(
                            egui::TextEdit::singleline(
                                &mut self.server_inputs.login_timeout_seconds,
                            )
                            .desired_width(f32::INFINITY),
                        );
                        ui.end_row();

                        ui.label(tr!(
                            language,
                            "세션 유휴 제한 시간 (초)",
                            "Session idle timeout (seconds)",
                            "会话空闲超时（秒）"
                        ));
                        ui.add(
                            egui::TextEdit::singleline(
                                &mut self.server_inputs.session_idle_timeout_seconds,
                            )
                            .desired_width(f32::INFINITY),
                        );
                        ui.end_row();

                        ui.label(tr!(
                            language,
                            "세션 전송 제한 시간 (초)",
                            "Session write timeout (seconds)",
                            "会话写入超时（秒）"
                        ));
                        ui.add(
                            egui::TextEdit::singleline(
                                &mut self.server_inputs.session_write_timeout_seconds,
                            )
                            .desired_width(f32::INFINITY),
                        );
                        ui.end_row();

                        ui.label(tr!(
                            language,
                            "최대 로그인 세션 수",
                            "Maximum login sessions",
                            "最大登录会话数"
                        ));
                        ui.add(
                            egui::TextEdit::singleline(&mut self.server_inputs.max_login_sessions)
                                .desired_width(f32::INFINITY),
                        );
                        ui.end_row();
                    });
            },
        );
    }

    fn server_status_panel(&self, ui: &mut egui::Ui) {
        let language = self.language;
        match &self.server_run_state {
            ServerRunState::Stopped => {
                ui.weak(tr!(
                    language,
                    "서버가 정지되어 있습니다.",
                    "The server is stopped.",
                    "服务器已停止。"
                ));
            }
            ServerRunState::Starting => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tr!(
                        language,
                        "네트워크 포트를 열고 클라이언트 데이터를 읽는 중...",
                        "Opening network ports and reading client data...",
                        "正在打开网络端口并读取客户端数据……"
                    ));
                });
            }
            ServerRunState::Running(endpoints) => {
                ui.colored_label(
                    egui::Color32::LIGHT_GREEN,
                    tr_format!(
                        language,
                        "실행 중: 게임 UDP {}, 로그인 TCP {}, P2P UDP {}, 메신저 TCP {}.",
                        "Running: game UDP {}, login TCP {}, P2P UDP {}, messenger TCP {}.",
                        "运行中：游戏 UDP {}，登录 TCP {}，P2P UDP {}，聊天 TCP {}。",
                        endpoints.game_udp,
                        endpoints.login_tcp,
                        endpoints.p2p_udp,
                        endpoints.messenger_tcp,
                    ),
                );
            }
            ServerRunState::Stopping => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tr!(
                        language,
                        "서버를 안전하게 종료하는 중...",
                        "Stopping the server safely...",
                        "正在安全停止服务器……"
                    ));
                });
            }
            ServerRunState::StopBlocked(error) => {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    tr_format!(
                        language,
                        "안전 종료가 지연되고 있습니다: {error}",
                        "Safe shutdown is delayed: {error}",
                        "安全停止被延迟：{error}"
                    ),
                );
            }
            ServerRunState::Failed(error) => {
                ui.colored_label(egui::Color32::LIGHT_RED, error);
            }
        }
    }

    // Keeping translated labels beside their controls makes this tab intentionally verbose.
    #[allow(clippy::too_many_lines)]
    fn server_tab(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        let active = self.server_run_state.is_active();
        ui.heading(tr!(language, "서버", "Server", "服务器"));
        ui.label(tr!(
            language,
            "P5136 서버를 설정하고 클라이언트가 접속하는 동안 실행합니다.",
            "Configure the P5136 server and keep it running while clients connect.",
            "配置 P5136 服务器，并在客户端连接期间保持运行。"
        ));
        ui.add_space(10.0);
        let mut file_logging_enabled = self.server_inputs.file_logging.enabled();
        if ui
            .checkbox(
                &mut file_logging_enabled,
                tr!(
                    language,
                    "상세 로그 파일 저장",
                    "Save detailed log file",
                    "保存详细日志文件"
                ),
            )
            .on_hover_text(tr!(
                language,
                "해제하면 이후 파일 기록을 중단하며 화면/터미널 요약은 유지합니다.",
                "Disabling stops future file writes while keeping screen and terminal summaries.",
                "取消后将停止后续文件写入，但保留界面及终端摘要。"
            ))
            .changed()
        {
            self.server_inputs.file_logging = if file_logging_enabled {
                GuiFileLogging::Enabled
            } else {
                GuiFileLogging::Disabled
            };
            self.logging_control.set_enabled(file_logging_enabled);
        }
        ui.add_enabled_ui(!active, |ui| self.server_input_panel(ui));
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !active,
                    egui::Button::new(tr!(language, "서버 시작", "Start server", "启动服务器"))
                        .min_size([130.0, 34.0].into()),
                )
                .clicked()
            {
                self.start_server(ui.ctx());
            }

            if matches!(&self.server_run_state, ServerRunState::Running(_))
                && ui
                    .button(tr!(
                        language,
                        "서버 안전 종료",
                        "Stop server safely",
                        "安全停止服务器"
                    ))
                    .on_hover_text(tr!(
                        language,
                        "진행 중인 프로필 저장을 마친 뒤 포트를 닫습니다.",
                        "Finishes pending profile saves before closing ports.",
                        "完成正在进行的配置文件保存后再关闭端口。"
                    ))
                    .clicked()
            {
                self.request_server_control(ServerControl::GracefulShutdown);
            }

            if matches!(
                &self.server_run_state,
                ServerRunState::Stopping | ServerRunState::StopBlocked(_)
            ) && ui
                .button(tr!(language, "강제 종료", "Force stop", "强制停止"))
                .on_hover_text(tr!(
                    language,
                    "안전 종료가 오래 걸리거나 막힌 경우에만 사용하세요.",
                    "Use only when safe shutdown is taking too long or is blocked.",
                    "仅在安全停止耗时过长或被阻塞时使用。"
                ))
                .clicked()
            {
                self.request_server_control(ServerControl::ForceShutdown);
            }

            if ui
                .button(tr!(
                    language,
                    "서버 주소를 접속기에 복사",
                    "Copy server address to connector",
                    "将服务器地址复制到连接器"
                ))
                .clicked()
            {
                self.connector_inputs
                    .server
                    .clone_from(&self.server_inputs.advertised_address);
                self.connector_inputs
                    .configured_port
                    .clone_from(&self.server_inputs.configured_port);
            }
        });
        ui.add_space(8.0);
        self.server_status_panel(ui);
    }

    fn time_attack_physics_editor(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.collapsing(
            tr!(
                language,
                "타임어택 물리 프리셋",
                "Time-attack physics preset",
                "计时赛物理预设"
            ),
            |ui| {
                egui::ComboBox::from_id_salt("time-attack-physics-preset")
                    .selected_text(time_attack_physics_preset_label(
                        language,
                        self.server_inputs.time_attack_physics_preset,
                    ))
                    .width(260.0)
                    .show_ui(ui, |ui| {
                        for preset in TimeAttackPhysicsPreset::ALL {
                            ui.selectable_value(
                                &mut self.server_inputs.time_attack_physics_preset,
                                preset,
                                time_attack_physics_preset_label(language, preset),
                            );
                        }
                    });
                ui.weak(tr!(
                    language,
                    "기본 설정은 클라이언트가 요청한 등급을 유지합니다. S0~S8을 선택하면 타임어택 시작 물리를 서버가 덮어쓰며, 다음 서버 시작부터 적용됩니다.",
                    "Default preserves the client-requested grade. Selecting S0–S8 overrides time-attack start physics from the next server start.",
                    "默认设置会保留客户端请求的等级。选择 S0–S8 后，服务器会从下次启动起覆盖计时赛开始物理。"
                ));
            },
        );
    }

    fn rider_school_pro_mission_editor(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.collapsing(
            tr!(
                language,
                "PRO 미션 조합",
                "PRO mission pair",
                "PRO 任务组合"
            ),
            |ui| {
                egui::ComboBox::from_id_salt("rider-school-pro-mission-set")
                    .selected_text(rider_school_pro_mission_set_label(
                        language,
                        self.server_inputs.rider_school_pro_mission_set,
                    ))
                    .width(420.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.server_inputs.rider_school_pro_mission_set,
                            RiderSchoolProMissionSet::Automatic,
                            rider_school_pro_mission_set_label(
                                language,
                                RiderSchoolProMissionSet::Automatic,
                            ),
                        );
                        for selection in RiderSchoolProMissionSet::MANUAL {
                            ui.selectable_value(
                                &mut self.server_inputs.rider_school_pro_mission_set,
                                selection,
                                rider_school_pro_mission_set_label(language, selection),
                            );
                        }
                    });
                ui.weak(tr!(
                    language,
                    "왼쪽 타임어택 트랙 → 오른쪽 대결 트랙 순서입니다. 수동 선택은 서버가 클라이언트에 알리는 기준 시각도 같은 2개월 구간으로 맞추며, 다음 서버 시작부터 적용됩니다.",
                    "Pairs are shown as time-attack track → versus track. A manual selection also projects the client-facing server clock into the same two-month window and applies from the next server start.",
                    "组合按计时赛地图 → 对决地图显示。手动选择还会把客户端所见的服务器时间调整到相同的双月周期，并从下次启动服务器时生效。"
                ));
            },
        );
    }

    fn server_management_tab(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        let active = self.server_run_state.is_active();
        ui.heading(tr!(
            language,
            "서버 관리",
            "Server management",
            "服务器管理"
        ));
        ui.label(tr!(
            language,
            "게임 규칙과 닉네임별 계정·인벤토리를 한곳에서 관리합니다.",
            "Manage gameplay rules and nickname-specific accounts and inventories in one place.",
            "在此集中管理游戏规则以及按昵称区分的账号和仓库。"
        ));
        ui.add_space(10.0);

        ui.add_enabled_ui(!active, |ui| {
            self.time_attack_physics_editor(ui);
            self.rider_school_pro_mission_editor(ui);
            self.item_probability_editor(ui);
        });
        if active {
            ui.weak(tr!(
                language,
                "타임어택 물리, PRO 미션 조합과 아이템 확률표는 현재 실행 설정에 포함되어 있으므로 서버를 정지한 뒤 변경할 수 있습니다.",
                "The time-attack physics preset, PRO mission pair, and item-probability table are part of the current runtime configuration; stop the server before changing them.",
                "计时赛物理预设、PRO 任务组合和道具概率表属于当前运行配置，请停止服务器后再更改。"
            ));
        }
        self.inventory_editor(ui);
        self.random_track_editor(ui);
    }

    fn connector_tab(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        let running = self.connector_run_state.is_running();
        ui.heading(tr!(language, "접속기", "Connector", "连接器"));
        ui.label(tr!(
            language,
            "정식 클라이언트 한 개를 준비하고 메신저 접속을 확인한 뒤 실행합니다.",
            "Prepares one stock client, verifies messenger reachability, and launches it.",
            "准备一个原版客户端，确认聊天服务器可连接后启动。"
        ));
        ui.add_space(10.0);
        ui.add_enabled_ui(!running, |ui| self.connector_input_panel(ui));
        ui.add_space(12.0);
        ui.separator();
        ui.add_space(10.0);

        if ui
            .add_enabled(
                !running,
                egui::Button::new(tr!(
                    language,
                    "클라이언트 준비 및 실행",
                    "Prepare and launch client",
                    "准备并启动客户端"
                ))
                .min_size([180.0, 34.0].into()),
            )
            .clicked()
        {
            self.start_connector(ui.ctx());
        }
        ui.add_space(10.0);
        self.connector_status_panel(ui);
    }
}

impl Drop for P5136GuiApp {
    fn drop(&mut self) {
        if let Some(cancellation) = &self.cancellation {
            cancellation.cancel();
        }
        if let Some(controller) = self.server_controller.take() {
            let _ = controller.send(ServerControl::ForceShutdown);
        }
        if let Some(worker) = self.server_worker.take() {
            let _ = worker.join();
        }
    }
}

impl eframe::App for P5136GuiApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        GuiPersistedSettings::from_app(self).save(storage);
    }

    fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();
        self.handle_close_request(context);
        if self.connector_run_state.is_running() || self.server_run_state.is_active() {
            context.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let language = self.language;
        egui::Panel::bottom("build-version-footer").show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.weak(tr_format!(
                    language,
                    "빌드 버전: {}",
                    "Build version: {}",
                    "构建版本：{}",
                    BUILD_VERSION
                ));
            });
        });
        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(WINDOW_TITLE);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    egui::ComboBox::from_id_salt("gui-language")
                        .selected_text(self.language.native_label())
                        .show_ui(ui, |ui| {
                            for candidate in GuiLanguage::ALL {
                                ui.selectable_value(
                                    &mut self.language,
                                    candidate,
                                    candidate.native_label(),
                                );
                            }
                        });
                    ui.label(tr!(language, "언어", "Language", "语言"));
                });
            });
            let language = self.language;
            ui.small(if self.server_inputs.file_logging.enabled() {
                tr_format!(
                    language,
                    "실행 로그 저장 중: {}",
                    "Saving runtime log: {}",
                    "正在保存运行日志：{}",
                    self.log_path.display()
                )
            } else {
                tr!(
                    language,
                    "실행 로그 파일 저장 꺼짐",
                    "Runtime log file saving is disabled",
                    "运行日志文件保存已关闭"
                )
                .to_owned()
            });
            ui.horizontal(|ui| {
                ui.selectable_value(
                    &mut self.selected_tab,
                    GuiTab::Server,
                    tr!(language, "서버", "Server", "服务器"),
                );
                ui.selectable_value(
                    &mut self.selected_tab,
                    GuiTab::ServerManagement,
                    tr!(language, "서버 관리", "Server management", "服务器管理"),
                );
                ui.selectable_value(
                    &mut self.selected_tab,
                    GuiTab::Connector,
                    tr!(language, "접속기", "Connector", "连接器"),
                );
            });
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| match self.selected_tab {
                GuiTab::Server => self.server_tab(ui),
                GuiTab::ServerManagement => self.server_management_tab(ui),
                GuiTab::Connector => self.connector_tab(ui),
            });
        });
    }
}

struct GuiNotifier {
    sender: Sender<GuiEvent>,
    context: egui::Context,
}

impl GuiNotifier {
    fn send(&self, event: GuiEvent) -> bool {
        if self.sender.send(event).is_ok() {
            self.context.request_repaint();
            true
        } else {
            false
        }
    }
}

fn run_connector_worker(
    plan: &ConnectorPlan,
    notifier: &GuiNotifier,
    cancellation: &ConnectorCancellation,
    language: GuiLanguage,
) -> Result<GuiSuccess> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .with_context(|| {
            tr!(
                language,
                "접속기 런타임을 만들지 못했습니다",
                "Failed to create the connector runtime",
                "无法创建连接器运行时"
            )
        })?;
    runtime.block_on(async {
        let mut execution =
            execute_connector_with_progress_and_cancellation(plan, cancellation, |stage| {
                notifier.send(GuiEvent::Connector(ConnectorGuiEvent::Stage(stage)));
            })
            .await
            .with_context(|| {
                tr!(
                    language,
                    "접속기 실행에 실패했습니다",
                    "Connector execution failed",
                    "连接器执行失败"
                )
            })?;
        let status = execution.launched_process.try_status().with_context(|| {
            tr!(
                language,
                "실행한 프로세스 상태를 확인하지 못했습니다",
                "Failed to query the launched process status",
                "无法查询已启动进程的状态"
            )
        })?;
        Ok(GuiSuccess {
            backend: execution.launched_process.backend(),
            pid: execution.launched_process.pid(),
            status: status.to_string(),
        })
    })
}

fn run_server_worker(
    config: ServerConfig,
    mut controls: tokio::sync::mpsc::UnboundedReceiver<ServerControl>,
    notifier: &GuiNotifier,
    language: GuiLanguage,
) -> Result<()> {
    tracing::info!(
        bind_address = %config.bind_address,
        advertised_address = %config.advertised_address,
        profile_root = %config.profile_root.display(),
        catalog_path = ?config.catalog_path,
        client_data_dir = ?config.client_data_dir,
        item_probability_rank_policy = ?config.item_probability_rank_policy,
        remote_profile_creation = config.allow_remote_profile_creation,
        "GUI requested P5136 server startup"
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .with_context(|| {
            tr!(
                language,
                "서버 런타임을 만들지 못했습니다",
                "Failed to create the server runtime",
                "无法创建服务器运行时"
            )
        })?;
    runtime.block_on(async move {
        let server = BoundServer::bind(config)
            .await
            .with_context(|| {
                tr!(
                    language,
                    "P5136 네트워크 포트를 열지 못했습니다",
                    "Failed to open the P5136 network ports",
                    "无法打开 P5136 网络端口"
                )
            })?
            .start()
            .with_context(|| {
                tr!(
                    language,
                    "P5136 서버 감독 작업을 시작하지 못했습니다",
                    "Failed to start the P5136 server supervisor",
                    "无法启动 P5136 服务器监督任务"
                )
            })?;
        let endpoints = server.endpoints();
        notifier.send(GuiEvent::ServerStarted(endpoints));
        run_server_control_loop(&server, &mut controls, notifier, language).await
    })
}

async fn run_server_control_loop(
    server: &p5136_server::ServerHandle,
    controls: &mut tokio::sync::mpsc::UnboundedReceiver<ServerControl>,
    notifier: &GuiNotifier,
    language: GuiLanguage,
) -> Result<()> {
    loop {
        tokio::select! {
            result = server.wait() => return result.with_context(|| tr!(language, "P5136 서버 런타임이 종료되었습니다", "The P5136 server runtime exited", "P5136 服务器运行时已退出")),
            control = controls.recv() => match control {
                Some(ServerControl::GracefulShutdown) => match await_graceful_shutdown_or_force(server, controls, notifier, language).await? {
                    GracefulShutdownOutcome::Stopped => return Ok(()),
                    GracefulShutdownOutcome::Blocked(error) => {
                        if !notifier.send(GuiEvent::ServerStopBlocked(error)) {
                            return server.force_shutdown().await.with_context(|| tr!(language, "GUI 종료 후 서버 강제 종료에 실패했습니다", "Failed to force-stop the server after the GUI closed", "GUI 关闭后强制停止服务器失败"));
                        }
                    }
                },
                Some(ServerControl::ForceShutdown) | None => {
                    return server.force_shutdown().await.with_context(|| tr!(language, "서버 강제 종료에 실패했습니다", "Failed to force-stop the server", "强制停止服务器失败"));
                }
                Some(ServerControl::UpdateRandomTracks(random_tracks)) => {
                    let result = server
                        .update_random_tracks(random_tracks)
                        .await
                        .map_err(|error| format!("{error:#}"));
                    if !notifier.send(GuiEvent::RandomTracksUpdated(result)) {
                        return server
                            .force_shutdown()
                            .await
                            .with_context(|| tr!(language, "GUI 종료 후 서버 강제 종료에 실패했습니다", "Failed to force-stop the server after the GUI closed", "GUI 关闭后强制停止服务器失败"));
                    }
                }
                Some(ServerControl::GrantKart {
                    catalog,
                    nickname,
                    kart_id,
                    options,
                }) => {
                    let result = server
                        .grant_kart(catalog, nickname, kart_id, options)
                        .await
                        .map_err(|error| format!("{error:#}"));
                    if !notifier.send(GuiEvent::KartGranted(result)) {
                        return server
                            .force_shutdown()
                            .await
                            .with_context(|| tr!(language, "GUI 종료 후 서버 강제 종료에 실패했습니다", "Failed to force-stop the server after the GUI closed", "GUI 关闭后强制停止服务器失败"));
                    }
                }
                Some(ServerControl::SetRiderSchoolProgress { nickname, progress }) => {
                    let request_nickname = nickname.clone();
                    let result = server
                        .set_rider_school_progress(nickname, progress)
                        .await
                        .map(|saved| saved.revision)
                        .map_err(|error| format!("{error:#}"));
                    if !notifier.send(GuiEvent::RiderSchoolProgressSet {
                        nickname: request_nickname,
                        progress,
                        result,
                    }) {
                        return server
                            .force_shutdown()
                            .await
                            .with_context(|| tr!(language, "GUI 종료 후 서버 강제 종료에 실패했습니다", "Failed to force-stop the server after the GUI closed", "GUI 关闭后强制停止服务器失败"));
                    }
                }
            }
        }
    }
}

enum GracefulShutdownOutcome {
    Stopped,
    Blocked(String),
}

async fn await_graceful_shutdown_or_force(
    server: &p5136_server::ServerHandle,
    controls: &mut tokio::sync::mpsc::UnboundedReceiver<ServerControl>,
    notifier: &GuiNotifier,
    language: GuiLanguage,
) -> Result<GracefulShutdownOutcome> {
    let mut graceful = Box::pin(server.shutdown());
    loop {
        tokio::select! {
            result = &mut graceful => match result {
                Ok(()) => return Ok(GracefulShutdownOutcome::Stopped),
                Err(error) => return Ok(GracefulShutdownOutcome::Blocked(format!("{error:#}"))),
            },
            control = controls.recv() => match control {
                Some(ServerControl::GracefulShutdown) => {}
                Some(ServerControl::ForceShutdown) | None => {
                    let (forced, graceful_result) = tokio::join!(server.force_shutdown(), &mut graceful);
                    forced.with_context(|| tr!(language, "서버 강제 종료에 실패했습니다", "Failed to force-stop the server", "强制停止服务器失败"))?;
                    graceful_result.with_context(|| tr!(language, "강제 종료 후에도 안전 종료 작업이 끝나지 않았습니다", "Safe shutdown did not finish even after a force-stop", "强制停止后安全停止任务仍未完成"))?;
                    return Ok(GracefulShutdownOutcome::Stopped);
                }
                Some(ServerControl::UpdateRandomTracks(_)) => {
                    notifier.send(GuiEvent::RandomTracksUpdated(Err(
                        tr!(language, "서버가 종료 중이어서 설정을 적용하지 않았습니다", "Settings were not applied because the server is stopping", "服务器正在停止，未应用设置").to_owned()
                    )));
                }
                Some(ServerControl::GrantKart { .. }) => {
                    notifier.send(GuiEvent::KartGranted(Err(
                        tr!(language, "서버가 종료 중이어서 카트를 지급하지 않았습니다", "The kart was not granted because the server is stopping", "服务器正在停止，未发放车辆").to_owned()
                    )));
                }
                Some(ServerControl::SetRiderSchoolProgress { nickname, progress }) => {
                    notifier.send(GuiEvent::RiderSchoolProgressSet {
                        nickname,
                        progress,
                        result: Err(tr!(language, "서버가 종료 중이어서 라이선스를 변경하지 않았습니다", "The license was not changed because the server is stopping", "服务器正在停止，未修改驾照").to_owned()),
                    });
                }
            }
        }
    }
}

fn stage_label(stage: ConnectorStage, language: GuiLanguage) -> &'static str {
    match stage {
        ConnectorStage::PreparingInstallation => tr!(
            language,
            "PIN과 XML 파일을 준비하는 중…",
            "Preparing PIN and XML files…",
            "正在准备 PIN 和 XML 文件……"
        ),
        ConnectorStage::ProbingMessenger => tr!(
            language,
            "메신저 TCP 접속을 확인하는 중…",
            "Checking messenger TCP reachability…",
            "正在检查聊天服务器 TCP 连接……"
        ),
        ConnectorStage::LaunchingGame => tr!(
            language,
            "카트라이더를 실행하는 중…",
            "Launching KartRider…",
            "正在启动跑跑卡丁车……"
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        fs,
        net::{IpAddr, Ipv4Addr},
        path::{Path, PathBuf},
        time::Duration,
    };

    use p5136_connector::{ConnectorCancellation, ConnectorStage, RunnerBackend};
    use p5136_server::{
        ItemProbabilityRankPolicy, RandomTrackConfiguration, RandomTrackPoolOverride,
        RiderSchoolProMissionSet, TimeAttackPhysicsPreset,
    };
    use tempfile::tempdir;

    use super::{
        BUILD_VERSION, ConnectorGuiEvent, GUI_SETTINGS_KEY, GuiInputs, GuiItemProbabilitySource,
        GuiRunState, GuiRunner, GuiSuccess, MAX_GUI_SETTINGS_BYTES, P5136GuiApp, ServerInputs,
        lan_address_rank, virtual_adapter_rank,
    };
    use crate::gui_i18n::GuiLanguage;

    #[derive(Default)]
    struct MemoryStorage(HashMap<String, String>);

    impl eframe::Storage for MemoryStorage {
        fn get_string(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }

        fn set_string(&mut self, key: &str, value: String) {
            self.0.insert(key.to_owned(), value);
        }

        fn remove_string(&mut self, key: &str) {
            self.0.remove(key);
        }

        fn flush(&mut self) {}
    }

    fn fixture_inputs() -> GuiInputs {
        GuiInputs {
            game_directory: "/games/Kart Rider".to_owned(),
            game_executable: "/games/Kart Rider/KartRider.custom.exe".to_owned(),
            nickname: "fixture-user".to_owned(),
            observer_mode: true,
            server: "192.0.2.10".to_owned(),
            configured_port: "39311".to_owned(),
            runner: GuiRunner::Wine,
            wine_binary: "/usr/local/bin/wine64".to_owned(),
            wine_prefix: "/bottles/p5136".to_owned(),
            crossover_binary: "/opt/cxoffice/bin/wine".to_owned(),
            crossover_bottle: "P5136".to_owned(),
            sikarugir_app: "/Applications/KartRider.app".to_owned(),
        }
    }

    #[test]
    fn build_version_footer_uses_the_compiled_package_version() {
        assert_eq!(BUILD_VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn inputs_build_the_same_non_mutating_connector_plan_as_cli() {
        let plan = fixture_inputs()
            .connector_plan(GuiLanguage::Korean)
            .unwrap();

        assert_eq!(plan.game_directory, Path::new("/games/Kart Rider"));
        assert_eq!(
            plan.launch_request.executable(),
            Path::new("/games/Kart Rider/KartRider.custom.exe")
        );
        assert_eq!(plan.nickname, "fixture-user");
        assert_eq!(
            plan.installation_options.launcher_profile_role,
            p5136_connector::LauncherProfileRole::ObserverMaster
        );
        assert_eq!(plan.login_endpoint.to_string(), "192.0.2.10:39312");
        assert_eq!(plan.messenger_endpoint.to_string(), "192.0.2.10:39313");
        assert_eq!(plan.launch_spec.backend(), RunnerBackend::Wine);
        assert_eq!(
            plan.launch_spec.environment,
            [("WINEPREFIX".into(), "/bottles/p5136".into())]
        );
    }

    #[test]
    fn observer_checkbox_maps_to_an_explicit_regular_or_observer_profile() {
        let mut inputs = fixture_inputs();
        inputs.observer_mode = false;
        assert_eq!(
            inputs
                .connector_plan(GuiLanguage::Korean)
                .unwrap()
                .installation_options
                .launcher_profile_role,
            p5136_connector::LauncherProfileRole::Regular
        );

        inputs.observer_mode = true;
        assert_eq!(
            inputs
                .connector_plan(GuiLanguage::Korean)
                .unwrap()
                .installation_options
                .launcher_profile_role,
            p5136_connector::LauncherProfileRole::ObserverMaster
        );
    }

    #[test]
    fn crossover_requires_both_binary_and_bottle() {
        let mut inputs = fixture_inputs();
        inputs.runner = GuiRunner::CrossOver;
        inputs.crossover_bottle.clear();

        let error = inputs
            .connector_plan(GuiLanguage::Korean)
            .unwrap_err()
            .to_string();
        assert!(error.contains("CrossOver 보틀"));
    }

    #[test]
    fn sikarugir_requires_a_wrapper_app_path() {
        let mut inputs = fixture_inputs();
        inputs.runner = GuiRunner::Sikarugir;
        inputs.sikarugir_app.clear();

        let error = inputs
            .connector_plan(GuiLanguage::Korean)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Sikarugir wrapper 앱"));
    }

    #[test]
    fn server_inputs_build_the_same_runtime_configuration_as_cli() {
        let client = tempdir().unwrap();
        fs::create_dir(client.path().join("Data")).unwrap();
        let inputs = ServerInputs {
            bind_address: "::1".to_owned(),
            advertised_address: "192.0.2.20".to_owned(),
            configured_port: "49311".to_owned(),
            profile_root: "runtime/Profiles".to_owned(),
            client_path: client.path().display().to_string(),
            client_data_dir: String::new(),
            allow_remote_profile_creation: true,
            first_message_delay_ms: "500".to_owned(),
            login_timeout_seconds: "10".to_owned(),
            session_idle_timeout_seconds: "240".to_owned(),
            session_write_timeout_seconds: "20".to_owned(),
            max_login_sessions: "32".to_owned(),
            rider_school_pro_mission_set: RiderSchoolProMissionSet::MineMaple,
            time_attack_physics_preset: TimeAttackPhysicsPreset::S4,
            ..ServerInputs::default()
        };

        let config = inputs.server_config(GuiLanguage::Korean).unwrap();

        assert_eq!(config.bind_address, "::1".parse::<IpAddr>().unwrap());
        assert_eq!(config.advertised_address, Ipv4Addr::new(192, 0, 2, 20));
        assert_eq!(config.ports.game_udp(), 49_311);
        assert_eq!(config.ports.login_tcp(), 49_312);
        assert_eq!(config.profile_root, Path::new("runtime/Profiles"));
        assert_eq!(config.catalog_path, None);
        assert_eq!(
            config.client_data_dir,
            Some(fs::canonicalize(client.path().join("Data")).unwrap())
        );
        assert_eq!(
            config.item_probability_rank_policy,
            ItemProbabilityRankPolicy::TrustClientReported
        );
        assert!(config.allow_remote_profile_creation);
        assert_eq!(config.first_message_delay, Duration::from_millis(500));
        assert_eq!(config.login_timeout, Duration::from_secs(10));
        assert_eq!(config.session_idle_timeout, Duration::from_secs(240));
        assert_eq!(config.session_write_timeout, Duration::from_secs(20));
        assert_eq!(config.max_login_sessions, 32);
        assert_eq!(
            config.rider_school_pro_mission_set,
            RiderSchoolProMissionSet::MineMaple
        );
        assert_eq!(
            config.time_attack_physics_preset,
            TimeAttackPhysicsPreset::S4
        );

        let safe = ServerInputs {
            trust_client_item_rank: false,
            ..inputs
        }
        .server_config(GuiLanguage::Korean)
        .unwrap();
        assert_eq!(
            safe.item_probability_rank_policy,
            ItemProbabilityRankPolicy::CombinedFallback
        );
    }

    #[test]
    fn server_inputs_reject_a_zero_login_session_limit_before_starting() {
        let inputs = ServerInputs {
            max_login_sessions: "0".to_owned(),
            ..ServerInputs::default()
        };

        assert!(
            inputs
                .server_config(GuiLanguage::Korean)
                .unwrap_err()
                .to_string()
                .contains("최대 로그인 세션 수")
        );
    }

    #[test]
    fn server_inputs_preserve_nonempty_random_track_checkbox_overrides() {
        let client = tempdir().unwrap();
        fs::create_dir(client.path().join("Data")).unwrap();
        let random_tracks = RandomTrackConfiguration {
            pools: vec![RandomTrackPoolOverride {
                game_type: 0,
                selector: 5,
                track_ids: vec!["china_R01".to_owned(), "village_R01".to_owned()],
            }],
        };
        let inputs = ServerInputs {
            client_path: client.path().display().to_string(),
            random_tracks: random_tracks.clone(),
            ..ServerInputs::default()
        };

        assert_eq!(
            inputs
                .server_config(GuiLanguage::Korean)
                .unwrap()
                .random_tracks,
            random_tracks
        );
    }

    #[test]
    fn server_inputs_require_a_client_location() {
        let error = ServerInputs::default()
            .server_config(GuiLanguage::Korean)
            .unwrap_err();
        assert!(error.to_string().contains("클라이언트 또는 Profile 경로"));
    }

    #[test]
    fn gui_persists_server_and_connector_inputs_between_runs() {
        let mut app = P5136GuiApp::new(PathBuf::new(), None);
        app.language = GuiLanguage::SimplifiedChinese;
        app.connector_inputs = fixture_inputs();
        app.server_inputs = ServerInputs {
            bind_address: "0.0.0.0".to_owned(),
            advertised_address: "192.168.1.10".to_owned(),
            configured_port: "49311".to_owned(),
            profile_root: "D:/P5136/Profile".to_owned(),
            client_path: "D:/Games/KartRider_5136".to_owned(),
            client_data_dir: "D:/Games/KartRider_5136/Data".to_owned(),
            allow_remote_profile_creation: true,
            first_message_delay_ms: "400".to_owned(),
            login_timeout_seconds: "20".to_owned(),
            session_idle_timeout_seconds: "600".to_owned(),
            session_write_timeout_seconds: "30".to_owned(),
            max_login_sessions: "64".to_owned(),
            trust_client_item_rank: false,
            item_probability_source: GuiItemProbabilitySource::Edited,
            item_probability_xml: "D:/P5136/item-probability.xml".to_owned(),
            show_team_item_probabilities: true,
            random_tracks: RandomTrackConfiguration {
                pools: vec![RandomTrackPoolOverride {
                    game_type: 3,
                    selector: 5,
                    track_ids: vec!["village_R01".to_owned(), "desert_R01".to_owned()],
                }],
            },
            rider_school_pro_mission_set: RiderSchoolProMissionSet::GoldAbyss,
            ..ServerInputs::default()
        };
        app.server_inputs.item_probabilities.individual[0].top_weight += 17;
        let expected_connector = app.connector_inputs.clone();
        let expected_server = app.server_inputs.clone();
        let expected_language = app.language;
        let mut storage = MemoryStorage::default();
        eframe::App::save(&mut app, &mut storage);
        assert!(storage.0.contains_key(GUI_SETTINGS_KEY));

        let restored = P5136GuiApp::new(PathBuf::new(), Some(&storage));
        assert_eq!(restored.connector_inputs, expected_connector);
        assert_eq!(restored.server_inputs, expected_server);
        assert_eq!(restored.language, expected_language);
    }

    #[test]
    fn gui_defaults_legacy_settings_without_a_language_to_korean() {
        let encoded = serde_json::json!({
            "connector": GuiInputs::default(),
            "server": ServerInputs::default(),
        })
        .to_string();
        let mut storage = MemoryStorage::default();
        storage.0.insert(GUI_SETTINGS_KEY.to_owned(), encoded);

        let restored = P5136GuiApp::new(PathBuf::new(), Some(&storage));

        assert_eq!(restored.language, GuiLanguage::Korean);
        assert_eq!(restored.connector_inputs, GuiInputs::default());
        assert_eq!(restored.server_inputs, ServerInputs::default());
    }

    #[test]
    fn gui_ignores_malformed_or_oversized_persisted_settings() {
        for encoded in ["{".to_owned(), "x".repeat(MAX_GUI_SETTINGS_BYTES + 1)] {
            let mut storage = MemoryStorage::default();
            storage.0.insert(GUI_SETTINGS_KEY.to_owned(), encoded);
            let restored = P5136GuiApp::new(PathBuf::new(), Some(&storage));
            assert_eq!(restored.connector_inputs, GuiInputs::default());
            assert_eq!(restored.server_inputs, ServerInputs::default());
        }
    }

    #[test]
    fn server_inputs_reject_an_empty_random_track_checkbox_override() {
        let inputs = ServerInputs {
            random_tracks: RandomTrackConfiguration {
                pools: vec![RandomTrackPoolOverride {
                    game_type: 1,
                    selector: 3,
                    track_ids: Vec::new(),
                }],
            },
            ..ServerInputs::default()
        };

        assert!(
            inputs
                .server_config(GuiLanguage::Korean)
                .unwrap_err()
                .to_string()
                .contains("맵이 1개 이상")
        );
    }

    #[test]
    fn server_addresses_are_intentionally_ip_literals_not_domain_names() {
        let bind_domain = ServerInputs {
            bind_address: "server.lan".to_owned(),
            ..ServerInputs::default()
        };
        assert!(bind_domain.server_config(GuiLanguage::Korean).is_err());

        let advertised_domain = ServerInputs {
            advertised_address: "server.lan".to_owned(),
            ..ServerInputs::default()
        };
        assert!(
            advertised_domain
                .server_config(GuiLanguage::Korean)
                .is_err()
        );

        let unspecified = ServerInputs {
            advertised_address: Ipv4Addr::UNSPECIFIED.to_string(),
            ..ServerInputs::default()
        };
        assert!(unspecified.server_config(GuiLanguage::Korean).is_err());
    }

    #[test]
    fn inventory_add_rejects_a_snapshot_after_the_client_data_path_changes() {
        let first = tempdir().unwrap();
        let second = tempdir().unwrap();
        for root in [first.path(), second.path()] {
            fs::create_dir(root.join("Data")).unwrap();
        }
        let mut app = P5136GuiApp::new(PathBuf::new(), None);
        app.server_inputs.client_path = first.path().display().to_string();
        app.inventory_catalog_data_dir = Some(fs::canonicalize(first.path().join("Data")).unwrap());
        app.validate_current_inventory_catalog_source().unwrap();

        app.server_inputs.client_path = second.path().display().to_string();
        assert!(
            app.validate_current_inventory_catalog_source()
                .unwrap_err()
                .to_string()
                .contains("경로가 바뀌었습니다")
        );
    }

    #[test]
    fn lan_candidates_prefer_home_subnets_and_physical_adapters() {
        assert!(
            lan_address_rank(Ipv4Addr::new(192, 168, 1, 10))
                < lan_address_rank(Ipv4Addr::new(10, 0, 0, 10))
        );
        assert!(
            lan_address_rank(Ipv4Addr::new(10, 0, 0, 10))
                < lan_address_rank(Ipv4Addr::new(172, 16, 0, 10))
        );
        assert_eq!(virtual_adapter_rank("Intel Ethernet"), 0);
        assert_eq!(virtual_adapter_rank("vEthernet (Default Switch)"), 1);
        assert_eq!(virtual_adapter_rank("VMware Network Adapter VMnet8"), 1);
        assert_eq!(virtual_adapter_rank("vEthernet (WSL)"), 1);
        assert_eq!(virtual_adapter_rank("Tailscale"), 1);
    }

    #[test]
    fn run_state_rejects_duplicate_start_and_accepts_progress() {
        let mut state = GuiRunState::Idle;
        assert!(state.begin());
        assert!(!state.begin());
        state.apply(ConnectorGuiEvent::Stage(ConnectorStage::ProbingMessenger));
        assert_eq!(
            state,
            GuiRunState::Running(ConnectorStage::ProbingMessenger)
        );

        let success = GuiSuccess {
            backend: RunnerBackend::Wine,
            pid: Some(42),
            status: "running".to_owned(),
        };
        state.apply(ConnectorGuiEvent::Finished(Ok(success.clone())));
        assert_eq!(state, GuiRunState::Succeeded(success));
        assert!(state.begin());
    }

    #[test]
    fn dropping_the_gui_cancels_its_active_worker_before_launch() {
        let cancellation = ConnectorCancellation::new();
        let mut app = P5136GuiApp::new(PathBuf::from("p5136-test.log"), None);
        app.cancellation = Some(cancellation.clone());

        drop(app);

        assert!(cancellation.is_cancelled());
    }
}
