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
use p5136_core::ports::{DEFAULT_CONFIGURED_PORT, PortTopology};
use p5136_profile::{
    AddKartOutcome, AdditionalKart, CatalogInventory, KartCatalogSearchResult, ProfileStore,
    add_kart, additional_karts, search_karts,
};
use p5136_server::{
    BoundServer, ItemProbabilityConfiguration, ItemProbabilityEntry, ItemProbabilityRankBand,
    ItemProbabilityRankPolicy, RandomTrackCatalog, RandomTrackConfiguration, RandomTrackDefinition,
    RandomTrackPool, RandomTrackPoolOverride, ServerConfig, ServerEndpoints,
    load_client_item_probabilities, load_client_kart_catalog, load_client_random_track_catalog,
    load_item_probability_xml,
};
use serde::{Deserialize, Serialize};

use crate::{LoggingRuntime, client_paths};

const WINDOW_TITLE: &str = "카트라이더 P5136";
const BUILD_VERSION_LABEL: &str = concat!("빌드 버전: ", env!("CARGO_PKG_VERSION"));
const GUI_CLOSE_GRACE_PERIOD: Duration = Duration::from_secs(5);
const GUI_SETTINGS_KEY: &str = "p5136-gui-settings-v2";
const MAX_GUI_SETTINGS_BYTES: usize = 2 * 1024 * 1024;

pub(crate) fn run(log_path: PathBuf, _logging: LoggingRuntime) -> Result<()> {
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
            Ok(Box::new(P5136GuiApp::new(
                log_path,
                creation_context.storage,
            )))
        }),
    )
    .map_err(|error| anyhow!("데스크톱 GUI를 실행하지 못했습니다: {error}"))
}

fn configure_platform_fonts(context: &egui::Context) {
    let Some((font_path, font_bytes)) = platform_korean_font_candidates()
        .into_iter()
        .find_map(|path| std::fs::read(&path).ok().map(|bytes| (path, bytes)))
    else {
        tracing::warn!(
            "Korean UI font was unavailable; install Noto Sans CJK KR or NanumGothic if text renders as boxes"
        );
        return;
    };

    let font_name = "platform-korean-ui".to_owned();
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        font_name.clone(),
        std::sync::Arc::new(egui::FontData::from_owned(font_bytes)),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        if let Some(family_fonts) = fonts.families.get_mut(&family) {
            family_fonts.insert(0, font_name.clone());
        }
    }
    context.set_fonts(fonts);
    tracing::info!(font_path = %font_path.display(), "loaded Korean UI font");
}

#[cfg(target_os = "windows")]
fn platform_korean_font_candidates() -> Vec<PathBuf> {
    let windows_root =
        std::env::var_os("WINDIR").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    vec![
        windows_root.join("Fonts").join("malgun.ttf"),
        windows_root.join("Fonts").join("malgunbd.ttf"),
    ]
}

#[cfg(target_os = "macos")]
fn platform_korean_font_candidates() -> Vec<PathBuf> {
    [
        "/System/Library/Fonts/AppleSDGothicNeo.ttc",
        "/System/Library/Fonts/Supplemental/AppleGothic.ttf",
        "/Library/Fonts/NanumGothic.ttf",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect()
}

#[cfg(target_os = "linux")]
fn platform_korean_font_candidates() -> Vec<PathBuf> {
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
fn platform_korean_font_candidates() -> Vec<PathBuf> {
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

    const fn label(self) -> &'static str {
        match self {
            Self::Auto => "자동",
            Self::Native => "직접 실행 (관리자 권한 없음)",
            Self::NativeElevated => "직접 실행 (Windows UAC)",
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
    fn connector_plan(&self) -> Result<ConnectorPlan> {
        let game_directory = required_path(&self.game_directory, "게임 디렉터리")?;
        let game_executable = optional_path(&self.game_executable);
        let server_address = self
            .server
            .trim()
            .parse::<Ipv4Addr>()
            .context("서버 주소는 IPv4여야 합니다")?;
        if server_address.is_unspecified() {
            return Err(anyhow!("서버 주소로 0.0.0.0을 사용할 수 없습니다"));
        }
        let configured_port = self
            .configured_port
            .trim()
            .parse::<u16>()
            .context("기준 포트는 0~65535 범위여야 합니다")?;
        let ports = PortTopology::new(configured_port)
            .context("기준 포트에서 필요한 접속기 포트를 모두 만들 수 없습니다")?;
        let runner = self.runner()?;

        ConnectorPlan::new(ConnectorRequest {
            game_directory,
            game_executable,
            nickname: self.nickname.clone(),
            server_address,
            ports,
            runner,
            probe_timeout: p5136_connector::DEFAULT_PROBE_TIMEOUT,
            installation_options: InstallationOptions::default(),
        })
        .context("접속기 설정이 올바르지 않습니다")
    }

    fn runner(&self) -> Result<Runner> {
        match self.runner {
            GuiRunner::Auto => Ok(Runner::Auto),
            GuiRunner::Native => Ok(Runner::Native),
            GuiRunner::NativeElevated => Ok(Runner::NativeElevated),
            GuiRunner::Wine => Ok(Runner::Wine {
                binary: required_path(&self.wine_binary, "Wine 실행 파일")?,
                prefix: optional_path(&self.wine_prefix),
            }),
            GuiRunner::CrossOver => Ok(Runner::CrossOver {
                wine_binary: required_path(&self.crossover_binary, "CrossOver 실행 파일")?,
                bottle: required_text(&self.crossover_bottle, "CrossOver 보틀")?.to_owned(),
            }),
            GuiRunner::Sikarugir => Ok(Runner::Sikarugir {
                app: required_path(&self.sikarugir_app, "Sikarugir wrapper 앱")?,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct GuiPersistedSettings {
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
        }
    }
}

impl ServerInputs {
    fn server_config(&self) -> Result<ServerConfig> {
        let bind_address = self
            .bind_address
            .trim()
            .parse::<IpAddr>()
            .context("바인드 주소는 IPv4 또는 IPv6여야 합니다")?;
        let advertised_address = self
            .advertised_address
            .trim()
            .parse::<Ipv4Addr>()
            .context("클라이언트에 알릴 주소는 IPv4여야 합니다")?;
        if advertised_address.is_unspecified()
            || advertised_address.is_multicast()
            || advertised_address == Ipv4Addr::BROADCAST
        {
            return Err(anyhow!(
                "클라이언트에 알릴 IPv4는 0.0.0.0, 멀티캐스트, 브로드캐스트 주소일 수 없습니다"
            ));
        }
        let configured_port = self
            .configured_port
            .trim()
            .parse::<u16>()
            .context("기준 포트는 0~65535 범위여야 합니다")?;
        let ports = PortTopology::new(configured_port)
            .context("기준 포트에서 P5136 서비스 포트를 모두 만들 수 없습니다")?;
        let max_login_sessions = parse_usize(&self.max_login_sessions, "최대 로그인 세션 수")?;
        if max_login_sessions == 0 {
            return Err(anyhow!("최대 로그인 세션 수는 1 이상이어야 합니다"));
        }
        if let Some(pool) = self
            .random_tracks
            .pools
            .iter()
            .find(|pool| pool.track_ids.is_empty())
        {
            return Err(anyhow!(
                "랜덤 맵 사용자 지정 목록에는 맵이 1개 이상 필요합니다: game_type={}, selector={}",
                pool.game_type,
                pool.selector
            ));
        }
        let client_path = required_path(&self.client_path, "클라이언트 또는 Profile 경로")?;
        let client_paths = client_paths::resolve_client_runtime_paths(
            Some(&client_path),
            optional_path_ref(&self.client_data_dir),
        )?;

        Ok(ServerConfig {
            bind_address,
            advertised_address,
            ports,
            profile_root: required_path(&self.profile_root, "프로필 저장 경로")?,
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
            first_message_delay: Duration::from_millis(parse_u64(
                &self.first_message_delay_ms,
                "첫 메시지 지연",
            )?),
            login_timeout: Duration::from_secs(parse_u64(
                &self.login_timeout_seconds,
                "로그인 제한 시간",
            )?),
            session_idle_timeout: Duration::from_secs(parse_u64(
                &self.session_idle_timeout_seconds,
                "세션 유휴 제한 시간",
            )?),
            session_write_timeout: Duration::from_secs(parse_u64(
                &self.session_write_timeout_seconds,
                "세션 전송 제한 시간",
            )?),
            max_login_sessions,
            allow_remote_profile_creation: self.allow_remote_profile_creation,
            ..ServerConfig::default()
        })
    }
}

fn required_text<'a>(value: &'a str, label: &str) -> Result<&'a str> {
    if value.trim().is_empty() {
        Err(anyhow!("{label}을(를) 비워 둘 수 없습니다"))
    } else {
        Ok(value)
    }
}

fn required_path(value: &str, label: &str) -> Result<PathBuf> {
    required_text(value, label).map(PathBuf::from)
}

fn optional_path_ref(value: &str) -> Option<&Path> {
    (!value.trim().is_empty()).then(|| Path::new(value))
}

fn optional_path(value: &str) -> Option<PathBuf> {
    optional_path_ref(value).map(Path::to_owned)
}

fn parse_u64(value: &str, label: &str) -> Result<u64> {
    value
        .trim()
        .parse::<u64>()
        .with_context(|| format!("{label}은(는) 0 이상의 정수여야 합니다"))
}

fn parse_usize(value: &str, label: &str) -> Result<usize> {
    value
        .trim()
        .parse::<usize>()
        .with_context(|| format!("{label}은(는) 0 이상의 정수여야 합니다"))
}

fn discover_lan_ipv4_candidates() -> Result<Vec<(String, Ipv4Addr)>> {
    let mut candidates = local_ip_address::list_afinet_netifas()
        .context("네트워크 어댑터 목록을 읽지 못했습니다")?
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
        return Err(anyhow!("사용 가능한 LAN IPv4 주소를 찾지 못했습니다"));
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

fn item_probability_grid(ui: &mut egui::Ui, entries: &mut [ItemProbabilityEntry]) -> bool {
    let mut changed = false;
    egui::ScrollArea::horizontal()
        .id_salt("item-probability-table-scroll")
        .show(ui, |ui| {
            egui::Grid::new("item-probability-table")
                .num_columns(6)
                .striped(true)
                .spacing([12.0, 5.0])
                .show(ui, |ui| {
                    for heading in ["ID", "아이템", "1등", "상위", "중위", "하위"] {
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

const fn rank_band_label_ko(rank_band: ItemProbabilityRankBand) -> &'static str {
    match rank_band {
        ItemProbabilityRankBand::Live => "현재 순위 자동",
        ItemProbabilityRankBand::Top => "1등",
        ItemProbabilityRankBand::High => "상위",
        ItemProbabilityRankBand::Middle => "중위",
        ItemProbabilityRankBand::Low => "하위",
        ItemProbabilityRankBand::Combined => "통합",
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

#[derive(Debug, Clone, Copy)]
enum ServerControl {
    GracefulShutdown,
    ForceShutdown,
}

enum GuiEvent {
    Connector(ConnectorGuiEvent),
    ServerStarted(ServerEndpoints),
    ServerStopBlocked(String),
    ServerFinished(Result<(), String>),
}

struct P5136GuiApp {
    log_path: PathBuf,
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
    inventory_additional_karts: Vec<AdditionalKart>,
    inventory_status: String,
}

impl P5136GuiApp {
    fn new(log_path: PathBuf, storage: Option<&dyn eframe::Storage>) -> Self {
        let (event_sender, event_receiver) = mpsc::channel();
        let persisted = GuiPersistedSettings::load(storage);
        let connector_inputs = persisted
            .as_ref()
            .map_or_else(GuiInputs::default, |settings| settings.connector.clone());
        let server_inputs =
            persisted.map_or_else(ServerInputs::default, |settings| settings.server);
        let inventory_nickname = connector_inputs.nickname.clone();
        Self {
            log_path,
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
            item_probability_status:
                "자동: 서버 시작 시 클라이언트의 item.rho/RHO5 확률표를 적용합니다.".to_owned(),
            lan_candidates: Vec::new(),
            selected_lan_candidate: 0,
            lan_status: "LAN 자동 설정은 활성 네트워크 어댑터의 IPv4를 사용합니다.".to_owned(),
            random_track_catalog: None,
            selected_random_track_pool: 0,
            random_track_status:
                "자동: 서버 시작 시 클라이언트의 track_common.rho 기본 목록을 적용합니다."
                    .to_owned(),
            inventory_catalog: None,
            inventory_catalog_data_dir: None,
            inventory_nickname,
            inventory_kart_query: String::new(),
            inventory_kart_results: Vec::new(),
            inventory_selected_kart: None,
            inventory_additional_karts: Vec::new(),
            inventory_status: "카트 목록을 불러온 뒤 닉네임별 추가 소유 카트를 관리할 수 있습니다."
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
            ServerRunState::Failed("서버 작업 스레드가 종료 중 패닉했습니다".to_owned())
        };
    }

    fn start_connector(&mut self, context: &egui::Context) {
        if self.connector_run_state.is_running() {
            return;
        }
        let plan = match self.connector_inputs.connector_plan() {
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
                let outcome = run_connector_worker(&plan, &worker_notifier, &worker_cancellation)
                    .map_err(|error| format!("{error:#}"));
                worker_notifier.send(GuiEvent::Connector(ConnectorGuiEvent::Finished(outcome)));
            })
        {
            if let Some(cancellation) = self.cancellation.take() {
                cancellation.cancel();
            }
            self.connector_run_state =
                GuiRunState::Failed(format!("접속기 작업 스레드를 시작하지 못했습니다: {error}"));
        }
    }

    fn connector_input_panel(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("connector-inputs")
            .num_columns(2)
            .spacing([14.0, 10.0])
            .show(ui, |ui| {
                ui.label("게임 디렉터리");
                ui.add(
                    egui::TextEdit::singleline(&mut self.connector_inputs.game_directory)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label("실행 파일 (선택)");
                ui.add(
                    egui::TextEdit::singleline(&mut self.connector_inputs.game_executable)
                        .hint_text("비우면 KartRider.exe")
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label("닉네임");
                ui.add(
                    egui::TextEdit::singleline(&mut self.connector_inputs.nickname)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label("서버 IPv4");
                ui.add(
                    egui::TextEdit::singleline(&mut self.connector_inputs.server)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label("기준 포트");
                ui.add(
                    egui::TextEdit::singleline(&mut self.connector_inputs.configured_port)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label("실행 방식");
                egui::ComboBox::from_id_salt("connector-runner")
                    .selected_text(self.connector_inputs.runner.label())
                    .show_ui(ui, |ui| {
                        for runner in GuiRunner::ALL {
                            ui.selectable_value(
                                &mut self.connector_inputs.runner,
                                runner,
                                runner.label(),
                            );
                        }
                    });
                ui.end_row();

                self.connector_runner_inputs(ui);
            });

        if self.connector_inputs.runner == GuiRunner::NativeElevated && !cfg!(windows) {
            ui.colored_label(
                egui::Color32::YELLOW,
                "이 운영체제에서는 Windows UAC 실행을 사용할 수 없습니다.",
            );
        }
        if self.connector_inputs.runner == GuiRunner::Auto {
            let resolution = if cfg!(windows) { "Windows UAC" } else { "Wine" };
            ui.weak(format!(
                "자동 모드는 이 운영체제에서 {resolution}(으)로 실행합니다."
            ));
        }
        if self.connector_inputs.runner == GuiRunner::Sikarugir && !cfg!(target_os = "macos") {
            ui.colored_label(
                egui::Color32::YELLOW,
                "Sikarugir wrapper 실행은 macOS에서만 사용할 수 있습니다.",
            );
        }
    }

    fn connector_runner_inputs(&mut self, ui: &mut egui::Ui) {
        match self.connector_inputs.runner {
            GuiRunner::Wine => {
                ui.label("Wine 실행 파일");
                ui.add(
                    egui::TextEdit::singleline(&mut self.connector_inputs.wine_binary)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label("Wine prefix (선택)");
                ui.add(
                    egui::TextEdit::singleline(&mut self.connector_inputs.wine_prefix)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();
            }
            GuiRunner::CrossOver => {
                ui.label("CrossOver Wine 실행 파일");
                ui.add(
                    egui::TextEdit::singleline(&mut self.connector_inputs.crossover_binary)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label("CrossOver 보틀");
                ui.add(
                    egui::TextEdit::singleline(&mut self.connector_inputs.crossover_bottle)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();
            }
            GuiRunner::Sikarugir => {
                ui.label("Sikarugir wrapper 앱");
                ui.add(
                    egui::TextEdit::singleline(&mut self.connector_inputs.sikarugir_app)
                        .hint_text("예: /Applications/KartRider.app")
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();
            }
            _ => {}
        }
    }

    fn connector_status_panel(&self, ui: &mut egui::Ui) {
        match &self.connector_run_state {
            GuiRunState::Idle => {
                ui.weak("준비됨.");
            }
            GuiRunState::Running(stage) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(stage_label(*stage));
                });
            }
            GuiRunState::Succeeded(success) => {
                let pid = success
                    .pid
                    .map_or_else(|| "확인 불가".to_owned(), |pid| pid.to_string());
                ui.colored_label(
                    egui::Color32::LIGHT_GREEN,
                    format!(
                        "{} 방식으로 실행했습니다 — PID {pid}, {}.",
                        success.backend, success.status
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
    Connector,
}

impl P5136GuiApp {
    fn start_server(&mut self, context: &egui::Context) {
        if self.server_run_state.is_active() {
            return;
        }
        let mut config = match self.server_inputs.server_config() {
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
                        self.item_probability_status = format!(
                            "자동 적용 확인: {} (개인 {}개 / 팀 {}개).",
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
                        self.server_run_state = ServerRunState::Failed(format!(
                            "클라이언트 아이템 확률표를 읽지 못했습니다: {error:#}"
                        ));
                        return;
                    }
                }
            } else {
                "클라이언트 Data 경로가 없어 안전 기본 확률표를 사용합니다."
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
                let outcome = run_server_worker(config, controls, &worker_notifier)
                    .map_err(|error| format!("{error:#}"));
                worker_notifier.send(GuiEvent::ServerFinished(outcome));
            }) {
            Ok(worker) => self.server_worker = Some(worker),
            Err(error) => {
                self.server_controller = None;
                self.server_run_state = ServerRunState::Failed(format!(
                    "서버 작업 스레드를 시작하지 못했습니다: {error}"
                ));
            }
        }
    }

    fn request_server_control(&mut self, command: ServerControl) {
        let Some(controller) = &self.server_controller else {
            self.server_run_state = ServerRunState::Failed(
                "서버 제어 채널을 사용할 수 없습니다. 서버 작업이 끝날 때까지 기다리세요"
                    .to_owned(),
            );
            return;
        };
        if controller.send(command).is_err() {
            self.server_run_state = ServerRunState::Failed(
                "요청을 전달하기 전에 서버 제어 채널이 닫혔습니다".to_owned(),
            );
            return;
        }
        self.server_run_state = ServerRunState::Stopping;
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
        let outcome = (|| -> Result<(ItemProbabilityConfiguration, PathBuf)> {
            let paths = client_paths::resolve_client_runtime_paths(
                optional_path_ref(&self.server_inputs.client_path),
                optional_path_ref(&self.server_inputs.client_data_dir),
            )?;
            let data_dir = paths
                .client_data_dir
                .ok_or_else(|| anyhow!("먼저 클라이언트 디렉터리 또는 Data 경로를 설정하세요"))?;
            let configuration = load_client_item_probabilities(&data_dir)
                .with_context(|| format!("{}을(를) 읽지 못했습니다", data_dir.display()))?;
            Ok((configuration, data_dir))
        })();
        match outcome {
            Ok((configuration, data_dir)) => {
                self.server_inputs.item_probabilities = configuration;
                self.server_inputs.item_probability_source = GuiItemProbabilitySource::Edited;
                self.item_probability_status = format!(
                    "클라이언트 확률표를 불러와 편집값으로 고정했습니다: {}.",
                    data_dir.display()
                );
            }
            Err(error) => {
                self.item_probability_status = format!("item.rho/RHO5 로드 실패: {error:#}");
            }
        }
    }

    fn apply_best_lan_ipv4(&mut self) {
        match discover_lan_ipv4_candidates() {
            Ok(candidates) => {
                self.lan_candidates = candidates;
                self.selected_lan_candidate = 0;
                let (_, address) = &self.lan_candidates[0];
                self.server_inputs.bind_address = address.to_string();
                self.server_inputs.advertised_address = address.to_string();
                self.lan_status = format!(
                    "바인드 주소와 광고 주소를 {address}로 설정했습니다. 다른 어댑터도 아래에서 선택할 수 있습니다."
                );
            }
            Err(error) => self.lan_status = format!("LAN 주소 검색 실패: {error:#}"),
        }
    }

    fn load_random_track_catalog(&mut self) {
        let outcome = (|| -> Result<RandomTrackCatalog> {
            let paths = client_paths::resolve_client_runtime_paths(
                optional_path_ref(&self.server_inputs.client_path),
                optional_path_ref(&self.server_inputs.client_data_dir),
            )?;
            let data_dir = paths
                .client_data_dir
                .ok_or_else(|| anyhow!("먼저 클라이언트 디렉터리 또는 Data 경로를 설정하세요"))?;
            load_client_random_track_catalog(&data_dir).with_context(|| {
                format!(
                    "{}의 track_common.rho를 읽지 못했습니다",
                    data_dir.display()
                )
            })
        })();
        match outcome {
            Ok(catalog) => {
                self.random_track_status = format!(
                    "랜덤 트랙 {}개, 선택 풀 {}개를 읽었습니다: {}",
                    catalog.tracks().len(),
                    catalog.pools().len(),
                    catalog.source_path().display(),
                );
                self.selected_random_track_pool = self
                    .selected_random_track_pool
                    .min(catalog.pools().len().saturating_sub(1));
                self.random_track_catalog = Some(catalog);
            }
            Err(error) => self.random_track_status = format!("랜덤 트랙 로드 실패: {error:#}"),
        }
    }

    fn load_inventory_catalog(&mut self) {
        let outcome = (|| -> Result<(Arc<CatalogInventory>, PathBuf, String)> {
            let paths = client_paths::resolve_client_runtime_paths(
                optional_path_ref(&self.server_inputs.client_path),
                optional_path_ref(&self.server_inputs.client_data_dir),
            )?;
            let data_dir = paths.client_data_dir.ok_or_else(|| {
                anyhow!("먼저 클라이언트 루트, Profile 또는 Data 경로를 설정하세요")
            })?;
            let loaded = load_client_kart_catalog(&data_dir).with_context(|| {
                format!("{}의 RHO 카트 데이터를 읽지 못했습니다", data_dir.display())
            })?;
            let stats = loaded.stats();
            let summary = format!(
                "이름 {}, 물리 {}, 상점 {}개/{}분류, 자동 카트 {}개, 수동 확인 카트 {}개, 변환 {}개",
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
                self.inventory_status = format!(
                    "RHO에서 자동 지급 가능한 카트 {kart_count}개를 읽었습니다. 보수적 검사에서 빠진 카트는 정확한 ID로 수동 추가할 수 있습니다 ({summary}): {}",
                    data_dir.display()
                );
            }
            Err(error) => {
                self.inventory_catalog = None;
                self.inventory_catalog_data_dir = None;
                self.inventory_kart_results.clear();
                self.inventory_selected_kart = None;
                self.inventory_status = format!("RHO 카트 목록 로드 실패: {error:#}");
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
        let outcome = (|| -> Result<(bool, Vec<AdditionalKart>)> {
            let catalog = self
                .inventory_catalog
                .as_ref()
                .ok_or_else(|| anyhow!("먼저 카트 목록을 불러오세요"))?;
            let nickname = required_text(&self.inventory_nickname, "인벤토리 닉네임")?;
            let store = ProfileStore::new(required_path(
                &self.server_inputs.profile_root,
                "프로필 저장 경로",
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
                self.inventory_status = format!(
                    "{}의 추가 소유 카트 {count}개를 읽었습니다.",
                    self.inventory_nickname.trim()
                );
            }
            Ok((false, _)) => {
                self.inventory_additional_karts.clear();
                self.inventory_status = format!(
                    "{} 프로필은 아직 없습니다. 카트를 추가하면 새 프로필을 만듭니다.",
                    self.inventory_nickname.trim()
                );
            }
            Err(error) => {
                self.inventory_status = format!("인벤토리 조회 실패: {error:#}");
            }
        }
    }

    fn add_selected_inventory_kart(&mut self) {
        let outcome = (|| -> Result<AddKartOutcome> {
            self.validate_current_inventory_catalog_source()?;
            let catalog = self
                .inventory_catalog
                .as_ref()
                .ok_or_else(|| anyhow!("먼저 카트 목록을 불러오세요"))?;
            let selected = self
                .inventory_selected_kart
                .as_ref()
                .ok_or_else(|| anyhow!("검색 결과에서 추가할 카트를 선택하세요"))?;
            let nickname = required_text(&self.inventory_nickname, "인벤토리 닉네임")?;
            let store = ProfileStore::new(required_path(
                &self.server_inputs.profile_root,
                "프로필 저장 경로",
            )?);
            Ok(add_kart(&store, catalog, nickname, selected.kart_id)?)
        })();
        match outcome {
            Ok(added) => {
                self.inventory_additional_karts = added.additional_karts().to_vec();
                let kart = added.kart();
                let revision = added.saved().revision;
                self.inventory_status = match added {
                    AddKartOutcome::Durable { .. } => format!(
                        "{}에 {} (ID {}, serial {})을 추가했습니다. 프로필 revision {revision}.",
                        self.inventory_nickname.trim(),
                        kart.name,
                        kart.kart_id,
                        kart.serial,
                    ),
                    AddKartOutcome::DurabilityUncertain { error, .. } => format!(
                        "카트는 revision {revision}에 추가됐지만 디렉터리 동기화를 확인하지 못했습니다: {error}. 재추가하지 말고 새로고침으로 확인하세요."
                    ),
                };
            }
            Err(error) => {
                self.inventory_status = format!("카트 추가 실패: {error:#}");
            }
        }
    }

    fn validate_current_inventory_catalog_source(&self) -> Result<()> {
        let loaded_data_dir = self
            .inventory_catalog_data_dir
            .as_ref()
            .ok_or_else(|| anyhow!("먼저 카트 목록을 불러오세요"))?;
        let paths = client_paths::resolve_client_runtime_paths(
            optional_path_ref(&self.server_inputs.client_path),
            optional_path_ref(&self.server_inputs.client_data_dir),
        )?;
        let current_data_dir = paths
            .client_data_dir
            .ok_or_else(|| anyhow!("클라이언트 또는 Data 경로가 비어 있습니다"))?;
        let current_data_dir = std::fs::canonicalize(&current_data_dir).with_context(|| {
            format!(
                "{}의 실제 경로를 확인하지 못했습니다",
                current_data_dir.display()
            )
        })?;
        if current_data_dir != *loaded_data_dir {
            return Err(anyhow!(
                "클라이언트 Data 경로가 바뀌었습니다. RHO 카트 목록을 다시 불러오세요: {} → {}",
                loaded_data_dir.display(),
                current_data_dir.display()
            ));
        }
        Ok(())
    }

    fn invalidate_inventory_catalog(&mut self) {
        self.inventory_catalog = None;
        self.inventory_catalog_data_dir = None;
        self.inventory_kart_results.clear();
        self.inventory_selected_kart = None;
        self.inventory_additional_karts.clear();
        "클라이언트 경로가 바뀌었습니다. 카트 목록을 다시 불러오세요."
            .clone_into(&mut self.inventory_status);
    }

    fn inventory_editor(&mut self, ui: &mut egui::Ui) {
        ui.collapsing("닉네임별 인벤토리 편집", |ui| {
            self.inventory_catalog_controls(ui);
            ui.separator();
            self.inventory_profile_controls(ui);
            ui.separator();
            self.inventory_kart_search_controls(ui);
            self.inventory_additional_kart_list(ui);
            let failed = self.inventory_status.contains("실패");
            let uncertain = self.inventory_status.contains("확인하지 못했습니다");
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
            ui.weak(
                "기본 카트는 모두 serial 1로 제공됩니다. 여기서 추가한 복사본은 serial 2 이상을 받아 서로 다른 강화·파츠 상태를 가질 수 있습니다.",
            );
            ui.weak("편집 결과는 해당 닉네임의 프로필 revision에 즉시 저장됩니다. 접속 중이었다면 재접속 후 반영됩니다.");
        });
    }

    fn inventory_catalog_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("카트 목록 불러오기").clicked() {
                self.load_inventory_catalog();
            }
            ui.weak("클라이언트 Data의 kart.rho/item.rho/RHO5를 직접 읽음");
        });
    }

    fn inventory_profile_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("닉네임");
            if ui
                .add(egui::TextEdit::singleline(&mut self.inventory_nickname).desired_width(180.0))
                .changed()
            {
                self.inventory_additional_karts.clear();
                "닉네임이 바뀌었습니다. 프로필을 새로고침하거나 카트를 추가하세요."
                    .clone_into(&mut self.inventory_status);
            }
            if ui.button("접속기 닉네임 사용").clicked() {
                self.inventory_nickname
                    .clone_from(&self.connector_inputs.nickname);
                self.inventory_additional_karts.clear();
            }
            if ui.button("프로필 새로고침").clicked() {
                self.refresh_inventory_profile();
            }
        });
    }

    fn inventory_kart_search_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("카트 이름 또는 ID");
            if ui
                .add(
                    egui::TextEdit::singleline(&mut self.inventory_kart_query)
                        .hint_text("예: 기간테스 V1 또는 1410")
                        .desired_width(260.0),
                )
                .changed()
            {
                self.refresh_inventory_search_results();
            }
        });

        let selected_text = self.inventory_selected_kart.as_ref().map_or_else(
            || "검색 후보 선택".to_owned(),
            |kart| {
                format!(
                    "{} (ID {}){}",
                    kart.name,
                    kart.kart_id,
                    if kart.auto_granted {
                        ""
                    } else {
                        " [수동 확인]"
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
                        ui.weak("일치하는 카트가 없습니다");
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
                                    " [수동 확인]"
                                }
                            ),
                        );
                    }
                });
            if ui
                .add_enabled(
                    self.inventory_selected_kart.is_some(),
                    egui::Button::new("선택 카트 추가"),
                )
                .clicked()
            {
                self.add_selected_inventory_kart();
            }
        });
        if let Some(selected) = &self.inventory_selected_kart {
            ui.weak(format!(
                "이름 → kart_id 변환: {} → {}",
                selected.name, selected.kart_id
            ));
            if !selected.auto_granted {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    "이 카트는 리소스/개발 데이터 보수 검사에서 자동 지급이 제외됐습니다. 실제 클라이언트 지원을 확인한 경우에만 정확한 ID로 수동 추가하세요.",
                );
            }
        }
    }

    fn inventory_additional_kart_list(&self, ui: &mut egui::Ui) {
        ui.label(format!(
            "현재 추가 소유 카트: {}개",
            self.inventory_additional_karts.len()
        ));
        if self.inventory_additional_karts.is_empty() {
            ui.weak("추가 소유분이 없습니다. 기본 serial 1 카트는 이 목록에서 생략합니다.");
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
        ui.collapsing("랜덤 트랙 설정", |ui| {
            ui.horizontal(|ui| {
                if ui.button("클라이언트 목록 불러오기").clicked() {
                    self.load_random_track_catalog();
                }
                if ui.button("모든 수동 설정 초기화").clicked() {
                    self.server_inputs.random_tracks = RandomTrackConfiguration::default();
                    "모든 풀을 클라이언트 기본 목록으로 되돌렸습니다."
                        .clone_into(&mut self.random_track_status);
                }
            });

            let Some(catalog) = &self.random_track_catalog else {
                ui.weak("서버 시작 시에는 자동으로 track_common.rho를 읽습니다. 목록을 편집하려면 위 버튼으로 미리 불러오세요.");
                ui.colored_label(
                    if self.random_track_status.contains("실패") { egui::Color32::LIGHT_RED } else { egui::Color32::GRAY },
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
            );
            ui.colored_label(
                if self.random_track_status.contains("실패") { egui::Color32::LIGHT_RED } else { egui::Color32::GRAY },
                &self.random_track_status,
            );
        });
    }

    fn random_track_pool_checker(
        ui: &mut egui::Ui,
        catalog: &RandomTrackCatalog,
        pool: &RandomTrackPool,
        configuration: &mut RandomTrackConfiguration,
    ) {
        let compatible = catalog
            .compatible_tracks(pool)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let original_override = Self::random_track_override_index(configuration, pool);
        let (mut select_all, mut clear_all, mut restore_defaults) = (false, false, false);
        ui.horizontal(|ui| {
            select_all = ui.button("모두 선택").clicked();
            clear_all = ui.button("모두 해제").clicked();
            restore_defaults = ui
                .add_enabled(
                    original_override.is_some(),
                    egui::Button::new("클라이언트 기본값"),
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
        Self::random_track_selection_status(ui, configuration, pool, compatible.len());
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
    ) {
        let current_override = Self::random_track_override_index(configuration, pool)
            .map(|index| &configuration.pools[index]);
        let selected_count = current_override.map_or(pool.default_track_ids.len(), |configured| {
            configured.track_ids.len()
        });
        ui.horizontal(|ui| {
            ui.weak(if current_override.is_some() {
                "사용자 지정 목록"
            } else {
                "클라이언트 기본 목록"
            });
            ui.weak(format!("· 선택: {selected_count}/{compatible_count}개"));
        });
        if selected_count == 0 {
            ui.colored_label(
                egui::Color32::LIGHT_RED,
                "맵을 1개 이상 선택해야 서버를 시작할 수 있습니다.",
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
        let outcome = required_path(&self.server_inputs.item_probability_xml, "아이템 확률 XML")
            .and_then(|path| {
                load_item_probability_xml(&path)
                    .with_context(|| format!("{}을(를) 불러오지 못했습니다", path.display()))
            });
        match outcome {
            Ok(configuration) => {
                self.server_inputs.item_probabilities = configuration;
                self.server_inputs.item_probability_source = GuiItemProbabilitySource::Edited;
                "이식 가능한 XML 확률표를 불러와 고정했습니다."
                    .clone_into(&mut self.item_probability_status);
            }
            Err(error) => {
                self.item_probability_status = format!("XML 로드 실패: {error:#}");
            }
        }
    }

    fn item_probability_rank_policy_editor(&mut self, ui: &mut egui::Ui) {
        ui.checkbox(
            &mut self.server_inputs.trust_client_item_rank,
            "클라이언트가 보고한 현재 순위 신뢰 (LAN/친구용)",
        )
        .on_hover_text(
            "체크하면 클라이언트의 1등/상위/중위/하위 순위를 사용합니다. 해제하면 통합 확률을 사용합니다.",
        );
    }

    fn item_probability_editor(&mut self, ui: &mut egui::Ui) {
        ui.collapsing("아이템 확률표", |ui| {
            let mut edited = false;
            self.item_probability_rank_policy_editor(ui);
            let pinned =
                self.server_inputs.item_probability_source == GuiItemProbabilitySource::Edited;
            ui.add_enabled_ui(pinned, |ui| {
                ui.horizontal(|ui| {
                    ui.label("순위 가중치");
                    egui::ComboBox::from_id_salt("item-probability-rank-band")
                        .selected_text(rank_band_label_ko(self.server_inputs.item_probabilities.rank_band))
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
                                        rank_band_label_ko(rank_band),
                                    )
                                    .changed();
                            }
                        });
                });
            });

            ui.horizontal(|ui| {
                if ui.button("클라이언트 item.rho/RHO5 불러와 고정").clicked() {
                    self.load_client_item_probability_defaults();
                }
                if ui.button("서버 시작 시 자동 적용").clicked() {
                    self.server_inputs.item_probability_source =
                        GuiItemProbabilitySource::AutoClient;
                    "자동: 서버를 시작할 때마다 클라이언트 item.rho/RHO5를 다시 읽습니다."
                        .clone_into(&mut self.item_probability_status);
                }
                if ui.button("안전 기본값 사용").clicked() {
                    self.server_inputs.item_probabilities =
                        ItemProbabilityConfiguration::safe_fallback();
                    self.server_inputs.item_probability_source = GuiItemProbabilitySource::Edited;
                    "개인 14개/팀 18개 안전 기본 확률표를 고정했습니다."
                        .clone_into(&mut self.item_probability_status);
                }
            });

            ui.horizontal(|ui| {
                ui.label("이식 가능한 XML");
                ui.add(
                    egui::TextEdit::singleline(&mut self.server_inputs.item_probability_xml)
                        .hint_text("item-probabilities.xml")
                        .desired_width(360.0),
                );
                if ui.button("XML 불러오기").clicked() {
                    self.load_item_probability_xml_override();
                }
            });

            if pinned {
                ui.horizontal(|ui| {
                    ui.selectable_value(
                        &mut self.server_inputs.show_team_item_probabilities,
                        false,
                        "아이템 개인전",
                    );
                    ui.selectable_value(
                        &mut self.server_inputs.show_team_item_probabilities,
                        true,
                        "아이템 팀전",
                    );
                });

                let entries = if self.server_inputs.show_team_item_probabilities {
                    &mut self.server_inputs.item_probabilities.team
                } else {
                    &mut self.server_inputs.item_probabilities.individual
                };
                edited |= item_probability_grid(ui, entries);
            } else {
                ui.weak(
                    "자동 모드입니다. 서버 시작 시 클라이언트 확률표를 읽고 적용 여부와 항목 수를 표시합니다. 편집하려면 위의 '불러와 고정'을 누르세요.",
                );
            }
            if edited {
                self.server_inputs.item_probability_source = GuiItemProbabilitySource::Edited;
                "편집한 확률표를 다음 서버 시작에 사용하도록 고정했습니다."
                    .clone_into(&mut self.item_probability_status);
            }
            let status_color = if self.item_probability_status.contains("실패") {
                egui::Color32::LIGHT_RED
            } else {
                egui::Color32::GRAY
            };
            ui.colored_label(status_color, &self.item_probability_status);
            ui.weak(
                "ID와 아이템 이름은 읽기 전용입니다. 가중치는 서버 바인드 전에 범위와 합계를 검증합니다.",
            );
        });
    }

    fn server_input_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("내 LAN IPv4로 자동 설정").clicked() {
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
                                self.lan_status =
                                    format!("바인드 주소와 광고 주소를 {address}로 설정했습니다.");
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
                ui.label("서버 바인드 주소");
                ui.add(
                    egui::TextEdit::singleline(&mut self.server_inputs.bind_address)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label("클라이언트에 알릴 IPv4");
                ui.add(
                    egui::TextEdit::singleline(&mut self.server_inputs.advertised_address)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label("기준 포트");
                ui.add(
                    egui::TextEdit::singleline(&mut self.server_inputs.configured_port)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label("프로필 저장 경로");
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut self.server_inputs.profile_root)
                            .desired_width(f32::INFINITY),
                    )
                    .changed()
                {
                    self.inventory_additional_karts.clear();
                    "프로필 저장 경로가 바뀌었습니다. 인벤토리를 새로고침하세요."
                        .clone_into(&mut self.inventory_status);
                }
                ui.end_row();

                ui.label("클라이언트 또는 Profile 경로 (필수)");
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

                ui.label("원격 프로필 생성");
                ui.checkbox(
                    &mut self.server_inputs.allow_remote_profile_creation,
                    "LAN의 새 닉네임 허용",
                );
                ui.end_row();
            });

        self.server_advanced_input_panel(ui);
        self.item_probability_editor(ui);
        self.random_track_editor(ui);
        self.inventory_editor(ui);

        ui.weak("포트: 게임 UDP = 기준, 로그인 TCP/P2P UDP = 기준 + 1, 메신저 TCP = 기준 + 2.");
        ui.weak(
            "클라이언트 루트, Profile 또는 Data 폴더를 지정하면 RHO 카트·아이템 데이터를 자동으로 읽습니다. KartCatalog.xml은 필요하지 않습니다.",
        );
        ui.weak("주소에는 IP 리터럴만 사용할 수 있습니다. P5136 패킷은 광고 주소를 IPv4 4바이트로 기록하므로 도메인을 직접 넣을 수 없습니다.");
        ui.weak("방 제목에 S0~S8 토큰을 넣으면 다음 경기 시작 패킷의 주행 물리를 해당 등급으로 바꿉니다. 예: '[S2] 친선'.");
        ui.weak("서버·접속기 입력 설정은 GUI 종료 시 저장되어 다음 실행에 복원됩니다. 실행 상태, 로그, 임시 검색 결과는 저장하지 않습니다.");
    }

    fn server_advanced_input_panel(&mut self, ui: &mut egui::Ui) {
        ui.collapsing("고급 시간 제한 및 접속 수", |ui| {
            egui::Grid::new("server-advanced-inputs")
                .num_columns(2)
                .spacing([14.0, 10.0])
                .show(ui, |ui| {
                    ui.label("클라이언트 Data 경로 재정의 (선택)");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.server_inputs.client_data_dir)
                            .desired_width(f32::INFINITY),
                    );
                    ui.end_row();

                    ui.label("첫 메시지 지연 (ms)");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.server_inputs.first_message_delay_ms)
                            .desired_width(f32::INFINITY),
                    );
                    ui.end_row();

                    ui.label("로그인 제한 시간 (초)");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.server_inputs.login_timeout_seconds)
                            .desired_width(f32::INFINITY),
                    );
                    ui.end_row();

                    ui.label("세션 유휴 제한 시간 (초)");
                    ui.add(
                        egui::TextEdit::singleline(
                            &mut self.server_inputs.session_idle_timeout_seconds,
                        )
                        .desired_width(f32::INFINITY),
                    );
                    ui.end_row();

                    ui.label("세션 전송 제한 시간 (초)");
                    ui.add(
                        egui::TextEdit::singleline(
                            &mut self.server_inputs.session_write_timeout_seconds,
                        )
                        .desired_width(f32::INFINITY),
                    );
                    ui.end_row();

                    ui.label("최대 로그인 세션 수");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.server_inputs.max_login_sessions)
                            .desired_width(f32::INFINITY),
                    );
                    ui.end_row();
                });
        });
    }

    fn server_status_panel(&self, ui: &mut egui::Ui) {
        match &self.server_run_state {
            ServerRunState::Stopped => {
                ui.weak("서버가 정지되어 있습니다.");
            }
            ServerRunState::Starting => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("네트워크 포트를 열고 클라이언트 데이터를 읽는 중...");
                });
            }
            ServerRunState::Running(endpoints) => {
                ui.colored_label(
                    egui::Color32::LIGHT_GREEN,
                    format!(
                        "실행 중: 게임 UDP {}, 로그인 TCP {}, P2P UDP {}, 메신저 TCP {}.",
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
                    ui.label("서버를 안전하게 종료하는 중...");
                });
            }
            ServerRunState::StopBlocked(error) => {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    format!("안전 종료가 지연되고 있습니다: {error}"),
                );
            }
            ServerRunState::Failed(error) => {
                ui.colored_label(egui::Color32::LIGHT_RED, error);
            }
        }
    }

    fn server_tab(&mut self, ui: &mut egui::Ui) {
        let active = self.server_run_state.is_active();
        ui.heading("서버");
        ui.label("P5136 서버를 설정하고 클라이언트가 접속하는 동안 실행합니다.");
        ui.add_space(10.0);
        ui.add_enabled_ui(!active, |ui| self.server_input_panel(ui));
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !active,
                    egui::Button::new("서버 시작").min_size([130.0, 34.0].into()),
                )
                .clicked()
            {
                self.start_server(ui.ctx());
            }

            if matches!(&self.server_run_state, ServerRunState::Running(_))
                && ui
                    .button("서버 안전 종료")
                    .on_hover_text("진행 중인 프로필 저장을 마친 뒤 포트를 닫습니다.")
                    .clicked()
            {
                self.request_server_control(ServerControl::GracefulShutdown);
            }

            if matches!(
                &self.server_run_state,
                ServerRunState::Stopping | ServerRunState::StopBlocked(_)
            ) && ui
                .button("강제 종료")
                .on_hover_text("안전 종료가 오래 걸리거나 막힌 경우에만 사용하세요.")
                .clicked()
            {
                self.request_server_control(ServerControl::ForceShutdown);
            }

            if ui.button("서버 주소를 접속기에 복사").clicked() {
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

    fn connector_tab(&mut self, ui: &mut egui::Ui) {
        let running = self.connector_run_state.is_running();
        ui.heading("접속기");
        ui.label("정식 클라이언트 한 개를 준비하고 메신저 접속을 확인한 뒤 실행합니다.");
        ui.add_space(10.0);
        ui.add_enabled_ui(!running, |ui| self.connector_input_panel(ui));
        ui.add_space(12.0);
        ui.separator();
        ui.add_space(10.0);

        if ui
            .add_enabled(
                !running,
                egui::Button::new("클라이언트 준비 및 실행").min_size([180.0, 34.0].into()),
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
        egui::Panel::bottom("build-version-footer").show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.weak(BUILD_VERSION_LABEL);
            });
        });
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading(WINDOW_TITLE);
            ui.small(format!("실행 로그: {}", self.log_path.display()));
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.selected_tab, GuiTab::Server, "서버");
                ui.selectable_value(&mut self.selected_tab, GuiTab::Connector, "접속기");
            });
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| match self.selected_tab {
                GuiTab::Server => self.server_tab(ui),
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
) -> Result<GuiSuccess> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("접속기 런타임을 만들지 못했습니다")?;
    runtime.block_on(async {
        let mut execution =
            execute_connector_with_progress_and_cancellation(plan, cancellation, |stage| {
                notifier.send(GuiEvent::Connector(ConnectorGuiEvent::Stage(stage)));
            })
            .await
            .context("접속기 실행에 실패했습니다")?;
        let status = execution
            .launched_process
            .try_status()
            .context("실행한 프로세스 상태를 확인하지 못했습니다")?;
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
        .context("서버 런타임을 만들지 못했습니다")?;
    runtime.block_on(async move {
        let server = BoundServer::bind(config)
            .await
            .context("P5136 네트워크 포트를 열지 못했습니다")?
            .start()
            .context("P5136 서버 감독 작업을 시작하지 못했습니다")?;
        let endpoints = server.endpoints();
        notifier.send(GuiEvent::ServerStarted(endpoints));
        run_server_control_loop(&server, &mut controls, notifier).await
    })
}

async fn run_server_control_loop(
    server: &p5136_server::ServerHandle,
    controls: &mut tokio::sync::mpsc::UnboundedReceiver<ServerControl>,
    notifier: &GuiNotifier,
) -> Result<()> {
    loop {
        tokio::select! {
            result = server.wait() => return result.context("P5136 서버 런타임이 종료되었습니다"),
            control = controls.recv() => match control {
                Some(ServerControl::GracefulShutdown) => match await_graceful_shutdown_or_force(server, controls).await? {
                    GracefulShutdownOutcome::Stopped => return Ok(()),
                    GracefulShutdownOutcome::Blocked(error) => {
                        if !notifier.send(GuiEvent::ServerStopBlocked(error)) {
                            return server.force_shutdown().await.context("GUI 종료 후 서버 강제 종료에 실패했습니다");
                        }
                    }
                },
                Some(ServerControl::ForceShutdown) | None => {
                    return server.force_shutdown().await.context("서버 강제 종료에 실패했습니다");
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
                    forced.context("서버 강제 종료에 실패했습니다")?;
                    graceful_result.context("강제 종료 후에도 안전 종료 작업이 끝나지 않았습니다")?;
                    return Ok(GracefulShutdownOutcome::Stopped);
                }
            }
        }
    }
}

const fn stage_label(stage: ConnectorStage) -> &'static str {
    match stage {
        ConnectorStage::PreparingInstallation => "PIN과 XML 파일을 준비하는 중…",
        ConnectorStage::ProbingMessenger => "메신저 TCP 접속을 확인하는 중…",
        ConnectorStage::LaunchingGame => "카트라이더를 실행하는 중…",
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
    };
    use tempfile::tempdir;

    use super::{
        BUILD_VERSION_LABEL, ConnectorGuiEvent, GUI_SETTINGS_KEY, GuiInputs,
        GuiItemProbabilitySource, GuiRunState, GuiRunner, GuiSuccess, MAX_GUI_SETTINGS_BYTES,
        P5136GuiApp, ServerInputs, lan_address_rank, virtual_adapter_rank,
    };

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
        assert_eq!(
            BUILD_VERSION_LABEL,
            concat!("빌드 버전: ", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn inputs_build_the_same_non_mutating_connector_plan_as_cli() {
        let plan = fixture_inputs().connector_plan().unwrap();

        assert_eq!(plan.game_directory, Path::new("/games/Kart Rider"));
        assert_eq!(
            plan.launch_request.executable(),
            Path::new("/games/Kart Rider/KartRider.custom.exe")
        );
        assert_eq!(plan.nickname, "fixture-user");
        assert_eq!(plan.login_endpoint.to_string(), "192.0.2.10:39312");
        assert_eq!(plan.messenger_endpoint.to_string(), "192.0.2.10:39313");
        assert_eq!(plan.launch_spec.backend(), RunnerBackend::Wine);
        assert_eq!(
            plan.launch_spec.environment,
            [("WINEPREFIX".into(), "/bottles/p5136".into())]
        );
    }

    #[test]
    fn crossover_requires_both_binary_and_bottle() {
        let mut inputs = fixture_inputs();
        inputs.runner = GuiRunner::CrossOver;
        inputs.crossover_bottle.clear();

        let error = inputs.connector_plan().unwrap_err().to_string();
        assert!(error.contains("CrossOver 보틀"));
    }

    #[test]
    fn sikarugir_requires_a_wrapper_app_path() {
        let mut inputs = fixture_inputs();
        inputs.runner = GuiRunner::Sikarugir;
        inputs.sikarugir_app.clear();

        let error = inputs.connector_plan().unwrap_err().to_string();
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
            ..ServerInputs::default()
        };

        let config = inputs.server_config().unwrap();

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

        let safe = ServerInputs {
            trust_client_item_rank: false,
            ..inputs
        }
        .server_config()
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
                .server_config()
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

        assert_eq!(inputs.server_config().unwrap().random_tracks, random_tracks);
    }

    #[test]
    fn server_inputs_require_a_client_location() {
        let error = ServerInputs::default().server_config().unwrap_err();
        assert!(error.to_string().contains("클라이언트 또는 Profile 경로"));
    }

    #[test]
    fn gui_persists_server_and_connector_inputs_between_runs() {
        let mut app = P5136GuiApp::new(PathBuf::new(), None);
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
            ..ServerInputs::default()
        };
        app.server_inputs.item_probabilities.individual[0].top_weight += 17;
        let expected_connector = app.connector_inputs.clone();
        let expected_server = app.server_inputs.clone();
        let mut storage = MemoryStorage::default();
        eframe::App::save(&mut app, &mut storage);
        assert!(storage.0.contains_key(GUI_SETTINGS_KEY));

        let restored = P5136GuiApp::new(PathBuf::new(), Some(&storage));
        assert_eq!(restored.connector_inputs, expected_connector);
        assert_eq!(restored.server_inputs, expected_server);
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
                .server_config()
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
        assert!(bind_domain.server_config().is_err());

        let advertised_domain = ServerInputs {
            advertised_address: "server.lan".to_owned(),
            ..ServerInputs::default()
        };
        assert!(advertised_domain.server_config().is_err());

        let unspecified = ServerInputs {
            advertised_address: Ipv4Addr::UNSPECIFIED.to_string(),
            ..ServerInputs::default()
        };
        assert!(unspecified.server_config().is_err());
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
