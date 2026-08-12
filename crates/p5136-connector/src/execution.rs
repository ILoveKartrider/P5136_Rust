use std::{
    future::Future,
    net::{Ipv4Addr, SocketAddrV4},
    path::PathBuf,
    time::Duration,
};

use p5136_core::ports::PortTopology;
use thiserror::Error;
use tokio::{sync::watch, task::JoinError};

use crate::{
    IdentityError, InstallationError, InstallationOptions, LaunchError, LaunchRequest, LaunchSpec,
    LaunchedProcess, PreparedInstallation, ProbeError, Runner, normalize_nickname,
    prepare_installation, probe_messenger,
};

#[derive(Debug, Clone)]
pub struct ConnectorRequest {
    pub game_directory: PathBuf,
    /// Optional game executable. Relative paths are resolved from `game_directory`.
    pub game_executable: Option<PathBuf>,
    pub nickname: String,
    pub server_address: Ipv4Addr,
    pub ports: PortTopology,
    pub runner: Runner,
    pub probe_timeout: Duration,
    pub installation_options: InstallationOptions,
}

#[derive(Debug, Clone)]
pub struct ConnectorPlan {
    pub game_directory: PathBuf,
    pub nickname: String,
    pub server_address: Ipv4Addr,
    pub ports: PortTopology,
    pub login_endpoint: SocketAddrV4,
    pub messenger_endpoint: SocketAddrV4,
    pub launch_request: LaunchRequest,
    pub launch_spec: LaunchSpec,
    pub probe_timeout: Duration,
    pub installation_options: InstallationOptions,
}

pub struct ConnectorExecution {
    pub prepared_installation: PreparedInstallation,
    pub launched_process: LaunchedProcess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectorStage {
    PreparingInstallation,
    ProbingMessenger,
    LaunchingGame,
}

#[derive(Debug, Clone)]
pub struct ConnectorCancellation {
    sender: watch::Sender<bool>,
}

impl ConnectorCancellation {
    #[must_use]
    pub fn new() -> Self {
        let (sender, _receiver) = watch::channel(false);
        Self { sender }
    }

    pub fn cancel(&self) {
        self.sender.send_replace(true);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        *self.sender.borrow()
    }

    async fn cancelled(&self) {
        let mut receiver = self.sender.subscribe();
        loop {
            if *receiver.borrow() {
                return;
            }
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }
}

impl Default for ConnectorCancellation {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Error)]
pub enum ConnectorPlanError {
    #[error("invalid connector nickname")]
    Identity(#[from] IdentityError),

    #[error("failed to construct the game launch command")]
    Launch(#[from] LaunchError),
}

#[derive(Debug, Error)]
pub enum ConnectorExecutionError {
    #[error("connector execution was cancelled")]
    Cancelled,

    #[error("installation preparation task failed")]
    PreparationTask(#[from] JoinError),

    #[error("installation preparation failed")]
    Installation(#[from] InstallationError),

    #[error("messenger reachability probe failed")]
    Probe(#[from] ProbeError),

    #[error("game launch failed")]
    Launch(#[from] LaunchError),
}

impl ConnectorPlan {
    /// Builds a complete, non-mutating execution plan.
    pub fn new(request: ConnectorRequest) -> Result<Self, ConnectorPlanError> {
        let nickname = normalize_nickname(&request.nickname)?;
        let login_endpoint = SocketAddrV4::new(request.server_address, request.ports.login_tcp());
        let messenger_endpoint =
            SocketAddrV4::new(request.server_address, request.ports.messenger_tcp());
        let launch_request = request.game_executable.map_or_else(
            || LaunchRequest::new(&request.game_directory),
            |executable| LaunchRequest::new(&request.game_directory).with_executable(executable),
        );
        let launch_spec = request.runner.build(&launch_request)?;

        Ok(Self {
            game_directory: request.game_directory,
            nickname,
            server_address: request.server_address,
            ports: request.ports,
            login_endpoint,
            messenger_endpoint,
            launch_request,
            launch_spec,
            probe_timeout: request.probe_timeout,
            installation_options: request.installation_options,
        })
    }

    #[must_use]
    pub fn prepared_paths(&self) -> Vec<PathBuf> {
        let mut paths = vec![
            self.game_directory.join("KartRider.pin"),
            self.game_directory.join("KartRider.xml"),
            self.game_directory.join("Profile/kr/launcher.xml"),
        ];
        if self.installation_options.unlock_special_tracks {
            paths.push(
                self.game_directory
                    .join("Data")
                    .join(crate::SPECIAL_TRACK_OVERLAY_FILE),
            );
        }
        paths
    }
}

/// Executes the connector in the required order: prepare, probe, then launch.
pub async fn execute_connector(
    plan: &ConnectorPlan,
) -> Result<ConnectorExecution, ConnectorExecutionError> {
    execute_connector_with_progress(plan, |_| {}).await
}

/// Executes the connector while reporting each stage before it starts.
pub async fn execute_connector_with_progress(
    plan: &ConnectorPlan,
    mut report_stage: impl FnMut(ConnectorStage),
) -> Result<ConnectorExecution, ConnectorExecutionError> {
    execute_connector_with_progress_and_cancellation(
        plan,
        &ConnectorCancellation::new(),
        &mut report_stage,
    )
    .await
}

/// Executes the connector with cooperative cancellation.
///
/// Once installation preparation has started it is always awaited to preserve
/// each atomic file replacement. Cancellation can stop execution before
/// preparation, while probing, or before launch is committed. Once process
/// creation begins, the launch future is awaited because dropping a pending
/// UAC handoff could leave an untracked child process running.
pub async fn execute_connector_with_progress_and_cancellation(
    plan: &ConnectorPlan,
    cancellation: &ConnectorCancellation,
    mut report_stage: impl FnMut(ConnectorStage),
) -> Result<ConnectorExecution, ConnectorExecutionError> {
    ensure_not_cancelled(cancellation)?;
    report_stage(ConnectorStage::PreparingInstallation);
    let game_directory = plan.game_directory.clone();
    let login_endpoint = plan.login_endpoint;
    let nickname = plan.nickname.clone();
    let installation_options = plan.installation_options;
    let prepared_installation = tokio::task::spawn_blocking(move || {
        prepare_installation(
            &game_directory,
            login_endpoint,
            &nickname,
            &installation_options,
        )
    })
    .await??;

    ensure_not_cancelled(cancellation)?;
    report_stage(ConnectorStage::ProbingMessenger);
    cancel_or(cancellation, async {
        probe_messenger(plan.server_address, plan.ports, plan.probe_timeout)
            .await
            .map_err(ConnectorExecutionError::from)
    })
    .await?;

    report_stage(ConnectorStage::LaunchingGame);
    let game_executable = plan.launch_request.executable();
    let launched_process = commit_launch(
        cancellation,
        || plan.launch_spec.preflight(&game_executable),
        plan.launch_spec.spawn_validated(),
    )
    .await?;

    Ok(ConnectorExecution {
        prepared_installation,
        launched_process,
    })
}

fn ensure_not_cancelled(
    cancellation: &ConnectorCancellation,
) -> Result<(), ConnectorExecutionError> {
    if cancellation.is_cancelled() {
        Err(ConnectorExecutionError::Cancelled)
    } else {
        Ok(())
    }
}

async fn cancel_or<T>(
    cancellation: &ConnectorCancellation,
    operation: impl Future<Output = Result<T, ConnectorExecutionError>>,
) -> Result<T, ConnectorExecutionError> {
    tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(ConnectorExecutionError::Cancelled),
        result = operation => result,
    }
}

async fn commit_launch<T>(
    cancellation: &ConnectorCancellation,
    preflight: impl FnOnce() -> Result<(), LaunchError>,
    launch: impl Future<Output = Result<T, LaunchError>>,
) -> Result<T, ConnectorExecutionError> {
    preflight()?;
    ensure_not_cancelled(cancellation)?;
    // Intentionally do not put this await in a cancellation select. Polling
    // commits process creation, and dropping PowerShell's pending `output()`
    // future does not guarantee that its already-spawned child is terminated.
    launch.await.map_err(ConnectorExecutionError::from)
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsString, fs, net::Ipv4Addr, path::PathBuf, time::Duration};

    use p5136_core::ports::PortTopology;
    use tempfile::tempdir;
    use tokio::{net::TcpListener, sync::oneshot, time};

    use super::{
        ConnectorCancellation, ConnectorExecutionError, ConnectorPlan, ConnectorRequest,
        ConnectorStage, commit_launch, execute_connector_with_progress,
        execute_connector_with_progress_and_cancellation,
    };
    use crate::{
        InstallationOptions, LaunchStatus, Runner, RunnerBackend,
        test_fixture::csharp_synthetic_pin,
    };

    #[tokio::test]
    async fn prepare_probe_and_harmless_launch_run_end_to_end_in_order() {
        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("KartRider.exe"),
            b"validation fixture only",
        )
        .unwrap();
        fs::write(
            directory.path().join("KartRider.pin"),
            csharp_synthetic_pin(),
        )
        .unwrap();

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let messenger_port = listener.local_addr().unwrap().port();
        let configured_port = messenger_port.checked_sub(2).unwrap();
        let ports = PortTopology::new(configured_port).unwrap();
        let accept = tokio::spawn(async move { listener.accept().await.unwrap() });

        let mut plan = ConnectorPlan::new(ConnectorRequest {
            game_directory: directory.path().to_owned(),
            game_executable: None,
            nickname: "fixture-user".to_owned(),
            server_address: Ipv4Addr::LOCALHOST,
            ports,
            runner: Runner::Native,
            probe_timeout: Duration::from_secs(1),
            installation_options: InstallationOptions::default(),
        })
        .unwrap();
        configure_harmless_command(&mut plan.launch_spec);

        let mut stages = Vec::new();
        let mut execution = execute_connector_with_progress(&plan, |stage| stages.push(stage))
            .await
            .unwrap();
        let _accepted = accept.await.unwrap();
        assert_eq!(
            stages,
            [
                ConnectorStage::PreparingInstallation,
                ConnectorStage::ProbingMessenger,
                ConnectorStage::LaunchingGame,
            ]
        );
        assert!(execution.prepared_installation.pin_path.is_file());
        assert!(execution.prepared_installation.game_config_path.is_file());
        assert!(
            execution
                .prepared_installation
                .launcher_profile_path
                .is_file()
        );
        assert_eq!(execution.launched_process.backend(), RunnerBackend::Native);
        assert!(execution.launched_process.pid().is_some());
        let status = execution.launched_process.wait().await.unwrap();
        assert!(matches!(status, LaunchStatus::Exited(exit) if exit.success()));
    }

    #[tokio::test]
    async fn cancellation_after_preparation_never_reaches_the_launch_command() {
        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("KartRider.exe"),
            b"validation fixture only",
        )
        .unwrap();
        fs::write(
            directory.path().join("KartRider.pin"),
            csharp_synthetic_pin(),
        )
        .unwrap();

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let messenger_port = listener.local_addr().unwrap().port();
        let configured_port = messenger_port.checked_sub(2).unwrap();
        let mut plan = ConnectorPlan::new(ConnectorRequest {
            game_directory: directory.path().to_owned(),
            game_executable: None,
            nickname: "cancelled-user".to_owned(),
            server_address: Ipv4Addr::LOCALHOST,
            ports: PortTopology::new(configured_port).unwrap(),
            runner: Runner::Native,
            probe_timeout: Duration::from_secs(1),
            installation_options: InstallationOptions::default(),
        })
        .unwrap();
        plan.launch_spec.program = directory.path().join("must-not-be-spawned");

        let cancellation = ConnectorCancellation::new();
        let result =
            execute_connector_with_progress_and_cancellation(&plan, &cancellation, |stage| {
                if stage == ConnectorStage::ProbingMessenger {
                    cancellation.cancel();
                }
            })
            .await;

        assert!(matches!(result, Err(ConnectorExecutionError::Cancelled)));
        assert!(directory.path().join("KartRider.xml").is_file());
        assert!(directory.path().join("Profile/kr/launcher.xml").is_file());
    }

    #[tokio::test]
    async fn cancellation_after_launch_commit_does_not_drop_the_spawn_future() {
        let cancellation = ConnectorCancellation::new();
        let worker_cancellation = cancellation.clone();
        let (started_sender, started_receiver) = oneshot::channel();
        let (release_sender, release_receiver) = oneshot::channel();
        let mut launch = tokio::spawn(async move {
            commit_launch(&worker_cancellation, || Ok(()), async move {
                let _ = started_sender.send(());
                let _ = release_receiver.await;
                Ok(())
            })
            .await
        });

        started_receiver.await.unwrap();
        cancellation.cancel();
        assert!(
            time::timeout(Duration::from_millis(25), &mut launch)
                .await
                .is_err(),
            "a committed launch future must stay alive after cancellation"
        );

        release_sender.send(()).unwrap();
        assert!(launch.await.unwrap().is_ok());
    }

    #[cfg(windows)]
    fn configure_harmless_command(spec: &mut crate::LaunchSpec) {
        spec.program = std::env::var_os("ComSpec").map_or_else(
            || PathBuf::from(r"C:\Windows\System32\cmd.exe"),
            PathBuf::from,
        );
        spec.arguments = [
            OsString::from("/D"),
            OsString::from("/C"),
            OsString::from("exit"),
            OsString::from("0"),
        ]
        .into();
        spec.environment.clear();
    }

    #[cfg(not(windows))]
    fn configure_harmless_command(spec: &mut crate::LaunchSpec) {
        spec.program = PathBuf::from("/bin/sh");
        spec.arguments = [OsString::from("-c"), OsString::from("exit 0")].into();
        spec.environment.clear();
    }
}
