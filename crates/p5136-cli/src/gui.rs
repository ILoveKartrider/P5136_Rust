use std::{
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
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
use p5136_server::{BoundServer, ServerConfig, ServerEndpoints};

use crate::LoggingRuntime;

const WINDOW_TITLE: &str = "KartRider P5136";
const GUI_CLOSE_GRACE_PERIOD: Duration = Duration::from_secs(5);

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
        Box::new(move |_creation_context| Ok(Box::new(P5136GuiApp::new(log_path)))),
    )
    .map_err(|error| anyhow!("failed to run desktop connector: {error}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuiRunner {
    Auto,
    Native,
    NativeElevated,
    Wine,
    CrossOver,
}

impl GuiRunner {
    const ALL: [Self; 5] = [
        Self::Auto,
        Self::Native,
        Self::NativeElevated,
        Self::Wine,
        Self::CrossOver,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Native => "Native (no elevation)",
            Self::NativeElevated => "Native (Windows UAC)",
            Self::Wine => "Wine",
            Self::CrossOver => "CrossOver",
        }
    }
}

#[derive(Debug, Clone)]
struct GuiInputs {
    game_directory: String,
    nickname: String,
    server: String,
    configured_port: String,
    runner: GuiRunner,
    wine_binary: String,
    wine_prefix: String,
    crossover_binary: String,
    crossover_bottle: String,
}

impl Default for GuiInputs {
    fn default() -> Self {
        Self {
            game_directory: default_game_directory().display().to_string(),
            nickname: "player".to_owned(),
            server: Ipv4Addr::LOCALHOST.to_string(),
            configured_port: DEFAULT_CONFIGURED_PORT.to_string(),
            runner: GuiRunner::Auto,
            wine_binary: "wine".to_owned(),
            wine_prefix: String::new(),
            crossover_binary: default_crossover_binary().display().to_string(),
            crossover_bottle: "KartRider-P5136".to_owned(),
        }
    }
}

impl GuiInputs {
    fn connector_plan(&self) -> Result<ConnectorPlan> {
        let game_directory = required_path(&self.game_directory, "game directory")?;
        let server_address = self
            .server
            .trim()
            .parse::<Ipv4Addr>()
            .context("server must be an IPv4 address")?;
        if server_address.is_unspecified() {
            return Err(anyhow!("server address cannot be 0.0.0.0"));
        }
        let configured_port = self
            .configured_port
            .trim()
            .parse::<u16>()
            .context("configured port must be between 0 and 65535")?;
        let ports = PortTopology::new(configured_port)
            .context("configured port cannot provide all connector offsets")?;
        let runner = self.runner()?;

        ConnectorPlan::new(ConnectorRequest {
            game_directory,
            nickname: self.nickname.clone(),
            server_address,
            ports,
            runner,
            probe_timeout: p5136_connector::DEFAULT_PROBE_TIMEOUT,
            installation_options: InstallationOptions::default(),
        })
        .context("invalid connector settings")
    }

    fn runner(&self) -> Result<Runner> {
        match self.runner {
            GuiRunner::Auto => Ok(Runner::Auto),
            GuiRunner::Native => Ok(Runner::Native),
            GuiRunner::NativeElevated => Ok(Runner::NativeElevated),
            GuiRunner::Wine => Ok(Runner::Wine {
                binary: required_path(&self.wine_binary, "Wine binary")?,
                prefix: optional_path(&self.wine_prefix),
            }),
            GuiRunner::CrossOver => Ok(Runner::CrossOver {
                wine_binary: required_path(&self.crossover_binary, "CrossOver binary")?,
                bottle: required_text(&self.crossover_bottle, "CrossOver bottle")?.to_owned(),
            }),
        }
    }
}

#[derive(Debug, Clone)]
struct ServerInputs {
    bind_address: String,
    advertised_address: String,
    configured_port: String,
    profile_root: String,
    catalog_path: String,
    client_data_dir: String,
    allow_remote_profile_creation: bool,
    first_message_delay_ms: String,
    login_timeout_seconds: String,
    session_idle_timeout_seconds: String,
    session_write_timeout_seconds: String,
    max_login_sessions: String,
}

impl Default for ServerInputs {
    fn default() -> Self {
        Self {
            bind_address: IpAddr::V4(Ipv4Addr::LOCALHOST).to_string(),
            advertised_address: Ipv4Addr::LOCALHOST.to_string(),
            configured_port: DEFAULT_CONFIGURED_PORT.to_string(),
            profile_root: "Profile".to_owned(),
            catalog_path: String::new(),
            client_data_dir: String::new(),
            allow_remote_profile_creation: false,
            first_message_delay_ms: "250".to_owned(),
            login_timeout_seconds: "12".to_owned(),
            session_idle_timeout_seconds: "300".to_owned(),
            session_write_timeout_seconds: "15".to_owned(),
            max_login_sessions: p5136_server::DEFAULT_MAX_LOGIN_SESSIONS.to_string(),
        }
    }
}

impl ServerInputs {
    fn server_config(&self) -> Result<ServerConfig> {
        let bind_address = self
            .bind_address
            .trim()
            .parse::<IpAddr>()
            .context("bind address must be an IPv4 or IPv6 address")?;
        let advertised_address = self
            .advertised_address
            .trim()
            .parse::<Ipv4Addr>()
            .context("advertised address must be an IPv4 address")?;
        let configured_port = self
            .configured_port
            .trim()
            .parse::<u16>()
            .context("configured port must be between 0 and 65535")?;
        let ports = PortTopology::new(configured_port)
            .context("configured port cannot provide all P5136 service offsets")?;
        let max_login_sessions = parse_usize(&self.max_login_sessions, "maximum login sessions")?;
        if max_login_sessions == 0 {
            return Err(anyhow!("maximum login sessions must be at least 1"));
        }

        Ok(ServerConfig {
            bind_address,
            advertised_address,
            ports,
            profile_root: required_path(&self.profile_root, "profile root")?,
            catalog_path: optional_path(&self.catalog_path),
            client_data_dir: optional_path(&self.client_data_dir),
            first_message_delay: Duration::from_millis(parse_u64(
                &self.first_message_delay_ms,
                "first-message delay",
            )?),
            login_timeout: Duration::from_secs(parse_u64(
                &self.login_timeout_seconds,
                "login timeout",
            )?),
            session_idle_timeout: Duration::from_secs(parse_u64(
                &self.session_idle_timeout_seconds,
                "session idle timeout",
            )?),
            session_write_timeout: Duration::from_secs(parse_u64(
                &self.session_write_timeout_seconds,
                "session write timeout",
            )?),
            max_login_sessions,
            allow_remote_profile_creation: self.allow_remote_profile_creation,
            ..ServerConfig::default()
        })
    }
}

fn required_text<'a>(value: &'a str, label: &str) -> Result<&'a str> {
    if value.trim().is_empty() {
        Err(anyhow!("{label} cannot be empty"))
    } else {
        Ok(value)
    }
}

fn required_path(value: &str, label: &str) -> Result<PathBuf> {
    required_text(value, label).map(PathBuf::from)
}

fn optional_path(value: &str) -> Option<PathBuf> {
    (!value.trim().is_empty()).then(|| PathBuf::from(value))
}

fn parse_u64(value: &str, label: &str) -> Result<u64> {
    value
        .trim()
        .parse::<u64>()
        .with_context(|| format!("{label} must be a nonnegative integer"))
}

fn parse_usize(value: &str, label: &str) -> Result<usize> {
    value
        .trim()
        .parse::<usize>()
        .with_context(|| format!("{label} must be a nonnegative integer"))
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
}

impl P5136GuiApp {
    fn new(log_path: PathBuf) -> Self {
        let (event_sender, event_receiver) = mpsc::channel();
        Self {
            log_path,
            selected_tab: GuiTab::Server,
            connector_inputs: GuiInputs::default(),
            connector_run_state: GuiRunState::Idle,
            server_inputs: ServerInputs::default(),
            server_run_state: ServerRunState::Stopped,
            event_sender,
            event_receiver,
            cancellation: None,
            server_controller: None,
            server_worker: None,
            close_requested: false,
            close_force_deadline: None,
            close_force_requested: false,
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
            ServerRunState::Failed("server worker panicked while stopping".to_owned())
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
                GuiRunState::Failed(format!("failed to start connector worker: {error}"));
        }
    }

    fn connector_input_panel(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("connector-inputs")
            .num_columns(2)
            .spacing([14.0, 10.0])
            .show(ui, |ui| {
                ui.label("Game directory");
                ui.add(
                    egui::TextEdit::singleline(&mut self.connector_inputs.game_directory)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label("Nickname");
                ui.add(
                    egui::TextEdit::singleline(&mut self.connector_inputs.nickname)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label("Server IPv4");
                ui.add(
                    egui::TextEdit::singleline(&mut self.connector_inputs.server)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label("Configured port");
                ui.add(
                    egui::TextEdit::singleline(&mut self.connector_inputs.configured_port)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label("Runner");
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

                match self.connector_inputs.runner {
                    GuiRunner::Wine => {
                        ui.label("Wine binary");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.connector_inputs.wine_binary)
                                .desired_width(f32::INFINITY),
                        );
                        ui.end_row();

                        ui.label("Wine prefix (optional)");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.connector_inputs.wine_prefix)
                                .desired_width(f32::INFINITY),
                        );
                        ui.end_row();
                    }
                    GuiRunner::CrossOver => {
                        ui.label("CrossOver wine binary");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.connector_inputs.crossover_binary)
                                .desired_width(f32::INFINITY),
                        );
                        ui.end_row();

                        ui.label("CrossOver bottle");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.connector_inputs.crossover_bottle)
                                .desired_width(f32::INFINITY),
                        );
                        ui.end_row();
                    }
                    _ => {}
                }
            });

        if self.connector_inputs.runner == GuiRunner::NativeElevated && !cfg!(windows) {
            ui.colored_label(
                egui::Color32::YELLOW,
                "Windows UAC mode is unavailable on this host.",
            );
        }
        if self.connector_inputs.runner == GuiRunner::Auto {
            let resolution = if cfg!(windows) { "Windows UAC" } else { "Wine" };
            ui.weak(format!("Auto resolves to {resolution} on this host."));
        }
    }

    fn connector_status_panel(&self, ui: &mut egui::Ui) {
        match &self.connector_run_state {
            GuiRunState::Idle => {
                ui.weak("Ready.");
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
                    .map_or_else(|| "unavailable".to_owned(), |pid| pid.to_string());
                ui.colored_label(
                    egui::Color32::LIGHT_GREEN,
                    format!(
                        "Started via {} — PID {pid}, {}.",
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
        let config = match self.server_inputs.server_config() {
            Ok(config) => config,
            Err(error) => {
                self.server_run_state = ServerRunState::Failed(format!("{error:#}"));
                return;
            }
        };
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
                self.server_run_state =
                    ServerRunState::Failed(format!("failed to start server worker: {error}"));
            }
        }
    }

    fn request_server_control(&mut self, command: ServerControl) {
        let Some(controller) = &self.server_controller else {
            self.server_run_state = ServerRunState::Failed(
                "server control channel is unavailable; wait for the server worker to finish"
                    .to_owned(),
            );
            return;
        };
        if controller.send(command).is_err() {
            self.server_run_state = ServerRunState::Failed(
                "server control channel closed before the request was delivered".to_owned(),
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

    fn server_input_panel(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("server-inputs")
            .num_columns(2)
            .spacing([14.0, 10.0])
            .show(ui, |ui| {
                ui.label("Bind address");
                ui.add(
                    egui::TextEdit::singleline(&mut self.server_inputs.bind_address)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label("Advertised IPv4");
                ui.add(
                    egui::TextEdit::singleline(&mut self.server_inputs.advertised_address)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label("Configured port");
                ui.add(
                    egui::TextEdit::singleline(&mut self.server_inputs.configured_port)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label("Profile root");
                ui.add(
                    egui::TextEdit::singleline(&mut self.server_inputs.profile_root)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label("KartCatalog.xml (optional)");
                ui.add(
                    egui::TextEdit::singleline(&mut self.server_inputs.catalog_path)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label("Client Data directory (optional)");
                ui.add(
                    egui::TextEdit::singleline(&mut self.server_inputs.client_data_dir)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label("Remote profile creation");
                ui.checkbox(
                    &mut self.server_inputs.allow_remote_profile_creation,
                    "Allow new remote nicknames",
                );
                ui.end_row();
            });

        ui.collapsing("Advanced timeouts and limits", |ui| {
            egui::Grid::new("server-advanced-inputs")
                .num_columns(2)
                .spacing([14.0, 10.0])
                .show(ui, |ui| {
                    ui.label("First-message delay (ms)");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.server_inputs.first_message_delay_ms)
                            .desired_width(f32::INFINITY),
                    );
                    ui.end_row();

                    ui.label("Login timeout (seconds)");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.server_inputs.login_timeout_seconds)
                            .desired_width(f32::INFINITY),
                    );
                    ui.end_row();

                    ui.label("Session idle timeout (seconds)");
                    ui.add(
                        egui::TextEdit::singleline(
                            &mut self.server_inputs.session_idle_timeout_seconds,
                        )
                        .desired_width(f32::INFINITY),
                    );
                    ui.end_row();

                    ui.label("Session write timeout (seconds)");
                    ui.add(
                        egui::TextEdit::singleline(
                            &mut self.server_inputs.session_write_timeout_seconds,
                        )
                        .desired_width(f32::INFINITY),
                    );
                    ui.end_row();

                    ui.label("Maximum login sessions");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.server_inputs.max_login_sessions)
                            .desired_width(f32::INFINITY),
                    );
                    ui.end_row();
                });
        });

        ui.weak(
            "Port offsets: game UDP = base, login TCP/P2P UDP = base + 1, messenger TCP = base + 2.",
        );
        ui.weak("Settings apply only when the server starts and are not persisted by the GUI.");
    }

    fn server_status_panel(&self, ui: &mut egui::Ui) {
        match &self.server_run_state {
            ServerRunState::Stopped => {
                ui.weak("Server is stopped.");
            }
            ServerRunState::Starting => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Binding transports and loading runtime data...");
                });
            }
            ServerRunState::Running(endpoints) => {
                ui.colored_label(
                    egui::Color32::LIGHT_GREEN,
                    format!(
                        "Running: game UDP {}, login TCP {}, P2P UDP {}, messenger TCP {}.",
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
                    ui.label("Stopping server gracefully...");
                });
            }
            ServerRunState::StopBlocked(error) => {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    format!("Graceful shutdown is blocked: {error}"),
                );
            }
            ServerRunState::Failed(error) => {
                ui.colored_label(egui::Color32::LIGHT_RED, error);
            }
        }
    }

    fn server_tab(&mut self, ui: &mut egui::Ui) {
        let active = self.server_run_state.is_active();
        ui.heading("Server");
        ui.label("Configure the P5136 server and keep it running while clients connect.");
        ui.add_space(10.0);
        ui.add_enabled_ui(!active, |ui| self.server_input_panel(ui));
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !active,
                    egui::Button::new("Start server").min_size([130.0, 34.0].into()),
                )
                .clicked()
            {
                self.start_server(ui.ctx());
            }

            if matches!(&self.server_run_state, ServerRunState::Running(_))
                && ui
                    .button("Stop server gracefully")
                    .on_hover_text("Drains accepted profile work before closing sockets.")
                    .clicked()
            {
                self.request_server_control(ServerControl::GracefulShutdown);
            }

            if matches!(
                &self.server_run_state,
                ServerRunState::Stopping | ServerRunState::StopBlocked(_)
            ) && ui
                .button("Force stop (discard pending recovery)")
                .on_hover_text("Use only if graceful shutdown is taking too long or is blocked.")
                .clicked()
            {
                self.request_server_control(ServerControl::ForceShutdown);
            }

            if ui.button("Use server address in Connector").clicked() {
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
        ui.heading("Connector");
        ui.label("Prepare one stock client, verify messenger reachability, and launch it.");
        ui.add_space(10.0);
        ui.add_enabled_ui(!running, |ui| self.connector_input_panel(ui));
        ui.add_space(12.0);
        ui.separator();
        ui.add_space(10.0);

        if ui
            .add_enabled(
                !running,
                egui::Button::new("Prepare and launch client").min_size([180.0, 34.0].into()),
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
    fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();
        self.handle_close_request(context);
        if self.connector_run_state.is_running() || self.server_run_state.is_active() {
            context.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading(WINDOW_TITLE);
            ui.small(format!("Runtime log: {}", self.log_path.display()));
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.selected_tab, GuiTab::Server, "Server");
                ui.selectable_value(&mut self.selected_tab, GuiTab::Connector, "Connector");
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
        .context("failed to create connector runtime")?;
    runtime.block_on(async {
        let mut execution =
            execute_connector_with_progress_and_cancellation(plan, cancellation, |stage| {
                notifier.send(GuiEvent::Connector(ConnectorGuiEvent::Stage(stage)));
            })
            .await
            .context("connector execution failed")?;
        let status = execution
            .launched_process
            .try_status()
            .context("failed to inspect launched process")?;
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
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create server runtime")?;
    runtime.block_on(async move {
        let server = BoundServer::bind(config)
            .await
            .context("failed to bind the P5136 transport set")?
            .start()
            .context("failed to start the P5136 supervisor")?;
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
            result = server.wait() => return result.context("P5136 server runtime stopped"),
            control = controls.recv() => match control {
                Some(ServerControl::GracefulShutdown) => match await_graceful_shutdown_or_force(server, controls).await? {
                    GracefulShutdownOutcome::Stopped => return Ok(()),
                    GracefulShutdownOutcome::Blocked(error) => {
                        if !notifier.send(GuiEvent::ServerStopBlocked(error)) {
                            return server.force_shutdown().await.context("forced server shutdown after GUI close failed");
                        }
                    }
                },
                Some(ServerControl::ForceShutdown) | None => {
                    return server.force_shutdown().await.context("forced server shutdown failed");
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
                    forced.context("forced server shutdown failed")?;
                    graceful_result.context("graceful shutdown did not complete after force shutdown")?;
                    return Ok(GracefulShutdownOutcome::Stopped);
                }
            }
        }
    }
}

const fn stage_label(stage: ConnectorStage) -> &'static str {
    match stage {
        ConnectorStage::PreparingInstallation => "Preparing PIN and XML files…",
        ConnectorStage::ProbingMessenger => "Checking the messenger TCP endpoint…",
        ConnectorStage::LaunchingGame => "Launching KartRider…",
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr},
        path::{Path, PathBuf},
        time::Duration,
    };

    use p5136_connector::{ConnectorCancellation, ConnectorStage, RunnerBackend};

    use super::{
        ConnectorGuiEvent, GuiInputs, GuiRunState, GuiRunner, GuiSuccess, P5136GuiApp, ServerInputs,
    };

    fn fixture_inputs() -> GuiInputs {
        GuiInputs {
            game_directory: "/games/Kart Rider".to_owned(),
            nickname: "fixture-user".to_owned(),
            server: "192.0.2.10".to_owned(),
            configured_port: "39311".to_owned(),
            runner: GuiRunner::Wine,
            wine_binary: "/usr/local/bin/wine64".to_owned(),
            wine_prefix: "/bottles/p5136".to_owned(),
            crossover_binary: "/opt/cxoffice/bin/wine".to_owned(),
            crossover_bottle: "P5136".to_owned(),
        }
    }

    #[test]
    fn inputs_build_the_same_non_mutating_connector_plan_as_cli() {
        let plan = fixture_inputs().connector_plan().unwrap();

        assert_eq!(plan.game_directory, Path::new("/games/Kart Rider"));
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
        assert!(error.contains("CrossOver bottle"));
    }

    #[test]
    fn server_inputs_build_the_same_runtime_configuration_as_cli() {
        let inputs = ServerInputs {
            bind_address: "::1".to_owned(),
            advertised_address: "192.0.2.20".to_owned(),
            configured_port: "49311".to_owned(),
            profile_root: "runtime/Profiles".to_owned(),
            catalog_path: "runtime/KartCatalog.xml".to_owned(),
            client_data_dir: "runtime/Data".to_owned(),
            allow_remote_profile_creation: true,
            first_message_delay_ms: "500".to_owned(),
            login_timeout_seconds: "10".to_owned(),
            session_idle_timeout_seconds: "240".to_owned(),
            session_write_timeout_seconds: "20".to_owned(),
            max_login_sessions: "32".to_owned(),
        };

        let config = inputs.server_config().unwrap();

        assert_eq!(config.bind_address, "::1".parse::<IpAddr>().unwrap());
        assert_eq!(config.advertised_address, Ipv4Addr::new(192, 0, 2, 20));
        assert_eq!(config.ports.game_udp(), 49_311);
        assert_eq!(config.ports.login_tcp(), 49_312);
        assert_eq!(config.profile_root, Path::new("runtime/Profiles"));
        assert_eq!(
            config.catalog_path.as_deref(),
            Some(Path::new("runtime/KartCatalog.xml"))
        );
        assert_eq!(
            config.client_data_dir.as_deref(),
            Some(Path::new("runtime/Data"))
        );
        assert!(config.allow_remote_profile_creation);
        assert_eq!(config.first_message_delay, Duration::from_millis(500));
        assert_eq!(config.login_timeout, Duration::from_secs(10));
        assert_eq!(config.session_idle_timeout, Duration::from_secs(240));
        assert_eq!(config.session_write_timeout, Duration::from_secs(20));
        assert_eq!(config.max_login_sessions, 32);
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
                .contains("maximum login sessions")
        );
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
        let mut app = P5136GuiApp::new(PathBuf::from("p5136-test.log"));
        app.cancellation = Some(cancellation.clone());

        drop(app);

        assert!(cancellation.is_cancelled());
    }
}
