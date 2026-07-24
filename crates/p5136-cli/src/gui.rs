use std::{
    net::Ipv4Addr,
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow};
use eframe::egui;
use p5136_connector::{
    ConnectorCancellation, ConnectorPlan, ConnectorRequest, ConnectorStage, InstallationOptions,
    Runner, RunnerBackend, execute_connector_with_progress_and_cancellation,
};
use p5136_core::ports::{DEFAULT_CONFIGURED_PORT, PortTopology};

const WINDOW_TITLE: &str = "KartRider P5136 Connector";

pub(crate) fn run() -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([680.0, 590.0])
            .with_min_inner_size([540.0, 480.0]),
        centered: true,
        ..Default::default()
    };

    eframe::run_native(
        WINDOW_TITLE,
        options,
        Box::new(|_creation_context| Ok(Box::new(ConnectorGuiApp::new()))),
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

    fn apply(&mut self, event: GuiEvent) {
        match event {
            GuiEvent::Stage(stage) => *self = Self::Running(stage),
            GuiEvent::Finished(Ok(success)) => *self = Self::Succeeded(success),
            GuiEvent::Finished(Err(error)) => *self = Self::Failed(error),
        }
    }
}

enum GuiEvent {
    Stage(ConnectorStage),
    Finished(Result<GuiSuccess, String>),
}

struct ConnectorGuiApp {
    inputs: GuiInputs,
    run_state: GuiRunState,
    event_sender: Sender<GuiEvent>,
    event_receiver: Receiver<GuiEvent>,
    cancellation: Option<ConnectorCancellation>,
}

impl ConnectorGuiApp {
    fn new() -> Self {
        let (event_sender, event_receiver) = mpsc::channel();
        Self {
            inputs: GuiInputs::default(),
            run_state: GuiRunState::Idle,
            event_sender,
            event_receiver,
            cancellation: None,
        }
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.event_receiver.try_recv() {
            let finished = matches!(&event, GuiEvent::Finished(_));
            self.run_state.apply(event);
            if finished {
                self.cancellation = None;
            }
        }
    }

    fn start(&mut self, context: &egui::Context) {
        if self.run_state.is_running() {
            return;
        }
        let plan = match self.inputs.connector_plan() {
            Ok(plan) => plan,
            Err(error) => {
                self.run_state = GuiRunState::Failed(format!("{error:#}"));
                return;
            }
        };
        if !self.run_state.begin() {
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
                worker_notifier.send(GuiEvent::Finished(outcome));
            })
        {
            if let Some(cancellation) = self.cancellation.take() {
                cancellation.cancel();
            }
            self.run_state =
                GuiRunState::Failed(format!("failed to start connector worker: {error}"));
        }
    }

    fn input_panel(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("connector-inputs")
            .num_columns(2)
            .spacing([14.0, 10.0])
            .show(ui, |ui| {
                ui.label("Game directory");
                ui.add(
                    egui::TextEdit::singleline(&mut self.inputs.game_directory)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label("Nickname");
                ui.add(
                    egui::TextEdit::singleline(&mut self.inputs.nickname)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label("Server IPv4");
                ui.add(
                    egui::TextEdit::singleline(&mut self.inputs.server)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label("Configured port");
                ui.add(
                    egui::TextEdit::singleline(&mut self.inputs.configured_port)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();

                ui.label("Runner");
                egui::ComboBox::from_id_salt("connector-runner")
                    .selected_text(self.inputs.runner.label())
                    .show_ui(ui, |ui| {
                        for runner in GuiRunner::ALL {
                            ui.selectable_value(&mut self.inputs.runner, runner, runner.label());
                        }
                    });
                ui.end_row();

                match self.inputs.runner {
                    GuiRunner::Wine => {
                        ui.label("Wine binary");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.inputs.wine_binary)
                                .desired_width(f32::INFINITY),
                        );
                        ui.end_row();

                        ui.label("Wine prefix (optional)");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.inputs.wine_prefix)
                                .desired_width(f32::INFINITY),
                        );
                        ui.end_row();
                    }
                    GuiRunner::CrossOver => {
                        ui.label("CrossOver wine binary");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.inputs.crossover_binary)
                                .desired_width(f32::INFINITY),
                        );
                        ui.end_row();

                        ui.label("CrossOver bottle");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.inputs.crossover_bottle)
                                .desired_width(f32::INFINITY),
                        );
                        ui.end_row();
                    }
                    _ => {}
                }
            });

        if self.inputs.runner == GuiRunner::NativeElevated && !cfg!(windows) {
            ui.colored_label(
                egui::Color32::YELLOW,
                "Windows UAC mode is unavailable on this host.",
            );
        }
        if self.inputs.runner == GuiRunner::Auto {
            let resolution = if cfg!(windows) { "Windows UAC" } else { "Wine" };
            ui.weak(format!("Auto resolves to {resolution} on this host."));
        }
    }

    fn status_panel(&self, ui: &mut egui::Ui) {
        match &self.run_state {
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

impl Drop for ConnectorGuiApp {
    fn drop(&mut self) {
        if let Some(cancellation) = &self.cancellation {
            cancellation.cancel();
        }
    }
}

impl eframe::App for ConnectorGuiApp {
    fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();
        if self.run_state.is_running() {
            context.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let running = self.run_state.is_running();
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading(WINDOW_TITLE);
            ui.label("Prepare the P5136 client, verify the messenger port, and launch the game.");
            ui.add_space(12.0);

            ui.add_enabled_ui(!running, |ui| self.input_panel(ui));
            ui.add_space(14.0);
            ui.separator();
            ui.add_space(10.0);

            if ui
                .add_enabled(
                    !running,
                    egui::Button::new("Start").min_size([110.0, 34.0].into()),
                )
                .clicked()
            {
                self.start(ui.ctx());
            }
            ui.add_space(10.0);
            self.status_panel(ui);
        });
    }
}

struct GuiNotifier {
    sender: Sender<GuiEvent>,
    context: egui::Context,
}

impl GuiNotifier {
    fn send(&self, event: GuiEvent) {
        if self.sender.send(event).is_ok() {
            self.context.request_repaint();
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
                notifier.send(GuiEvent::Stage(stage));
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

const fn stage_label(stage: ConnectorStage) -> &'static str {
    match stage {
        ConnectorStage::PreparingInstallation => "Preparing PIN and XML files…",
        ConnectorStage::ProbingMessenger => "Checking the messenger TCP endpoint…",
        ConnectorStage::LaunchingGame => "Launching KartRider…",
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use p5136_connector::{ConnectorCancellation, ConnectorStage, RunnerBackend};

    use super::{ConnectorGuiApp, GuiEvent, GuiInputs, GuiRunState, GuiRunner, GuiSuccess};

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
    fn run_state_rejects_duplicate_start_and_accepts_progress() {
        let mut state = GuiRunState::Idle;
        assert!(state.begin());
        assert!(!state.begin());
        state.apply(GuiEvent::Stage(ConnectorStage::ProbingMessenger));
        assert_eq!(
            state,
            GuiRunState::Running(ConnectorStage::ProbingMessenger)
        );

        let success = GuiSuccess {
            backend: RunnerBackend::Wine,
            pid: Some(42),
            status: "running".to_owned(),
        };
        state.apply(GuiEvent::Finished(Ok(success.clone())));
        assert_eq!(state, GuiRunState::Succeeded(success));
        assert!(state.begin());
    }

    #[test]
    fn dropping_the_gui_cancels_its_active_worker_before_launch() {
        let cancellation = ConnectorCancellation::new();
        let mut app = ConnectorGuiApp::new();
        app.cancellation = Some(cancellation.clone());

        drop(app);

        assert!(cancellation.is_cancelled());
    }
}
