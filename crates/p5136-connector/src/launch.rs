use std::{
    ffi::{OsStr, OsString},
    fmt, io,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
};

use thiserror::Error;
use tokio::process::{Child, Command};

const GAME_EXECUTABLE: &str = "KartRider.exe";
const LAUNCH_ARGUMENT: &str = "-profile:launcher";
const ELEVATED_PID_MARKER: &str = "P5136_ELEVATED_PID=";

#[cfg(windows)]
const ELEVATED_GAME_EXE_ENV: &str = "P5136_CONNECTOR_GAME_EXE";
#[cfg(windows)]
const ELEVATED_GAME_DIR_ENV: &str = "P5136_CONNECTOR_GAME_DIR";
#[cfg(windows)]
const ELEVATED_POWERSHELL_SCRIPT: &str = concat!(
    "$ErrorActionPreference = 'Stop'; ",
    "$process = Start-Process ",
    "-FilePath $env:P5136_CONNECTOR_GAME_EXE ",
    "-ArgumentList @('-profile:launcher') ",
    "-WorkingDirectory $env:P5136_CONNECTOR_GAME_DIR ",
    "-Verb RunAs -PassThru; ",
    "[Console]::Out.WriteLine('P5136_ELEVATED_PID=' + $process.Id)"
);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchRequest {
    pub game_directory: PathBuf,
    executable: Option<PathBuf>,
}

impl LaunchRequest {
    #[must_use]
    pub fn new(game_directory: impl Into<PathBuf>) -> Self {
        Self {
            game_directory: game_directory.into(),
            executable: None,
        }
    }

    /// Uses a different executable while keeping the supplied game directory
    /// as the working directory and location for connector-managed files.
    #[must_use]
    pub fn with_executable(mut self, executable: impl Into<PathBuf>) -> Self {
        let executable = executable.into();
        self.executable = Some(if executable.is_absolute() {
            executable
        } else {
            self.game_directory.join(executable)
        });
        self
    }

    #[must_use]
    pub fn executable(&self) -> PathBuf {
        self.executable
            .clone()
            .unwrap_or_else(|| self.game_directory.join(GAME_EXECUTABLE))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Runner {
    /// Select UAC-backed native execution on Windows and Wine elsewhere.
    Auto,
    /// Launch directly as the current user, without requesting elevation.
    Native,
    /// Launch through Windows `PowerShell`'s safe `Start-Process -Verb RunAs` API.
    NativeElevated,
    Wine {
        binary: PathBuf,
        prefix: Option<PathBuf>,
    },
    CrossOver {
        wine_binary: PathBuf,
        bottle: String,
    },
    /// Open a preconfigured Sikarugir wrapper on macOS.
    Sikarugir { app: PathBuf },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerBackend {
    Native,
    NativeElevated,
    Wine,
    CrossOver,
    Sikarugir,
}

impl fmt::Display for RunnerBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Native => "native",
            Self::NativeElevated => "native-elevated",
            Self::Wine => "wine",
            Self::CrossOver => "crossover",
            Self::Sikarugir => "sikarugir",
        };
        formatter.write_str(name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaunchMethod {
    Direct,
    #[cfg_attr(not(windows), allow(dead_code))]
    PowerShellUac,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    pub program: PathBuf,
    pub arguments: Vec<OsString>,
    pub current_directory: PathBuf,
    pub environment: Vec<(OsString, OsString)>,
    backend: RunnerBackend,
    method: LaunchMethod,
}

pub struct LaunchedProcess {
    pid: Option<u32>,
    backend: RunnerBackend,
    handle: ProcessHandle,
}

enum ProcessHandle {
    Tracked(Box<Child>),
    /// `PowerShell` exits after handing the elevated process to the Windows shell.
    UacDetached,
}

#[derive(Debug)]
pub enum LaunchStatus {
    Running,
    Exited(ExitStatus),
    /// Windows accepted the UAC launch and returned the target PID, but the
    /// unelevated connector cannot retain a Tokio child handle across that boundary.
    StartedDetached,
}

impl fmt::Display for LaunchStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Running => formatter.write_str("running"),
            Self::Exited(status) => write!(formatter, "exited ({status})"),
            Self::StartedDetached => formatter.write_str("started (UAC handoff)"),
        }
    }
}

#[derive(Debug, Error)]
pub enum LaunchError {
    #[error("CrossOver bottle name cannot be empty")]
    EmptyBottle,

    #[error("game executable does not exist: {0}")]
    MissingExecutable(PathBuf),

    #[error("Sikarugir wrapper is not an existing .app directory: {0}")]
    MissingSikarugirApp(PathBuf),

    #[error("native elevated launch is supported only on Windows")]
    UnsupportedNativeElevation,

    #[error("Sikarugir wrapper launch is supported only on macOS")]
    UnsupportedSikarugir,

    #[error("SystemRoot is unavailable; cannot resolve the trusted Windows PowerShell path")]
    MissingSystemRoot,

    #[error("failed to launch game")]
    Spawn(#[source] io::Error),

    #[error("Windows UAC launcher failed with {status}: {detail}")]
    ElevatedLauncherFailed { status: ExitStatus, detail: String },

    #[error("Windows UAC launcher did not return a valid game PID: {output}")]
    InvalidElevatedPid { output: String },

    #[error("failed to inspect launched process")]
    Inspect(#[source] io::Error),

    #[error("failed to wait for launched process")]
    Wait(#[source] io::Error),
}

impl Runner {
    pub fn build(&self, request: &LaunchRequest) -> Result<LaunchSpec, LaunchError> {
        let resolved = self.resolve()?;
        let executable = request.executable();
        let current_directory = request.game_directory.clone();

        match resolved {
            Self::Native => Ok(LaunchSpec {
                program: executable,
                arguments: vec![OsString::from(LAUNCH_ARGUMENT)],
                current_directory,
                environment: Vec::new(),
                backend: RunnerBackend::Native,
                method: LaunchMethod::Direct,
            }),
            Self::NativeElevated => elevated_native_spec(request),
            Self::Wine { binary, prefix } => {
                let mut environment = Vec::new();
                if let Some(prefix) = prefix {
                    environment.push((OsString::from("WINEPREFIX"), prefix.into_os_string()));
                }
                Ok(LaunchSpec {
                    program: binary,
                    arguments: vec![executable.into_os_string(), OsString::from(LAUNCH_ARGUMENT)],
                    current_directory,
                    environment,
                    backend: RunnerBackend::Wine,
                    method: LaunchMethod::Direct,
                })
            }
            Self::CrossOver {
                wine_binary,
                bottle,
            } => {
                if bottle.trim().is_empty() {
                    return Err(LaunchError::EmptyBottle);
                }
                Ok(LaunchSpec {
                    program: wine_binary,
                    arguments: vec![
                        OsString::from("--bottle"),
                        OsString::from(bottle),
                        OsString::from("--cx-app"),
                        executable.into_os_string(),
                        OsString::from(LAUNCH_ARGUMENT),
                    ],
                    current_directory,
                    environment: Vec::new(),
                    backend: RunnerBackend::CrossOver,
                    method: LaunchMethod::Direct,
                })
            }
            Self::Sikarugir { app } => Ok(LaunchSpec {
                program: PathBuf::from("/usr/bin/open"),
                arguments: vec![app.into_os_string()],
                current_directory,
                environment: Vec::new(),
                backend: RunnerBackend::Sikarugir,
                method: LaunchMethod::Direct,
            }),
            Self::Auto => unreachable!("the automatic runner is resolved before dispatch"),
        }
    }

    pub fn resolved_backend(&self) -> Result<RunnerBackend, LaunchError> {
        match self.resolve()? {
            Self::Native => Ok(RunnerBackend::Native),
            Self::NativeElevated => Ok(RunnerBackend::NativeElevated),
            Self::Wine { .. } => Ok(RunnerBackend::Wine),
            Self::CrossOver { .. } => Ok(RunnerBackend::CrossOver),
            Self::Sikarugir { .. } => Ok(RunnerBackend::Sikarugir),
            Self::Auto => unreachable!("the automatic runner is resolved before dispatch"),
        }
    }

    fn resolve(&self) -> Result<Self, LaunchError> {
        match self {
            Self::Auto if cfg!(windows) => Ok(Self::NativeElevated),
            Self::Auto => Ok(Self::Wine {
                binary: PathBuf::from("wine"),
                prefix: None,
            }),
            Self::NativeElevated if !cfg!(windows) => Err(LaunchError::UnsupportedNativeElevation),
            Self::Sikarugir { .. } if !cfg!(target_os = "macos") => {
                Err(LaunchError::UnsupportedSikarugir)
            }
            other => Ok(other.clone()),
        }
    }
}

#[cfg(windows)]
fn elevated_native_spec(request: &LaunchRequest) -> Result<LaunchSpec, LaunchError> {
    let system_root = std::env::var_os("SystemRoot")
        .filter(|value| !value.is_empty())
        .ok_or(LaunchError::MissingSystemRoot)?;
    let powershell =
        PathBuf::from(system_root).join("System32/WindowsPowerShell/v1.0/powershell.exe");

    Ok(LaunchSpec {
        program: powershell,
        arguments: vec![
            OsString::from("-NoLogo"),
            OsString::from("-NoProfile"),
            OsString::from("-NonInteractive"),
            OsString::from("-Command"),
            OsString::from(ELEVATED_POWERSHELL_SCRIPT),
        ],
        current_directory: request.game_directory.clone(),
        environment: vec![
            (
                OsString::from(ELEVATED_GAME_EXE_ENV),
                request.executable().into_os_string(),
            ),
            (
                OsString::from(ELEVATED_GAME_DIR_ENV),
                request.game_directory.clone().into_os_string(),
            ),
        ],
        backend: RunnerBackend::NativeElevated,
        method: LaunchMethod::PowerShellUac,
    })
}

#[cfg(not(windows))]
fn elevated_native_spec(_request: &LaunchRequest) -> Result<LaunchSpec, LaunchError> {
    Err(LaunchError::UnsupportedNativeElevation)
}

impl LaunchSpec {
    #[must_use]
    pub fn backend(&self) -> RunnerBackend {
        self.backend
    }

    pub fn validate(&self, game_executable: &Path) -> Result<(), LaunchError> {
        if !game_executable.is_file() {
            return Err(LaunchError::MissingExecutable(game_executable.to_owned()));
        }
        if self.backend == RunnerBackend::Sikarugir {
            let app = self
                .arguments
                .first()
                .map(PathBuf::from)
                .unwrap_or_default();
            if !app.is_dir() || app.extension() != Some(OsStr::new("app")) {
                return Err(LaunchError::MissingSikarugirApp(app));
            }
        }
        Ok(())
    }

    pub async fn spawn(&self, game_executable: &Path) -> Result<LaunchedProcess, LaunchError> {
        self.preflight(game_executable)?;
        self.spawn_validated().await
    }

    pub(crate) fn preflight(&self, game_executable: &Path) -> Result<(), LaunchError> {
        self.validate(game_executable)
    }

    /// Commits a launch that has already passed `preflight`.
    ///
    /// Callers must not cancel this future after polling begins: the host
    /// process may already exist even while a UAC handoff is still pending.
    pub(crate) async fn spawn_validated(&self) -> Result<LaunchedProcess, LaunchError> {
        match self.method {
            LaunchMethod::Direct => self.spawn_direct(),
            LaunchMethod::PowerShellUac => self.spawn_powershell_uac().await,
        }
    }

    fn spawn_direct(&self) -> Result<LaunchedProcess, LaunchError> {
        let mut command = self.command();
        let child = command.spawn().map_err(LaunchError::Spawn)?;
        Ok(LaunchedProcess {
            pid: child.id(),
            backend: self.backend,
            handle: ProcessHandle::Tracked(Box::new(child)),
        })
    }

    async fn spawn_powershell_uac(&self) -> Result<LaunchedProcess, LaunchError> {
        let mut command = self.command();
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = command.output().await.map_err(LaunchError::Spawn)?;
        if !output.status.success() {
            return Err(LaunchError::ElevatedLauncherFailed {
                status: output.status,
                detail: output_detail(&output.stderr),
            });
        }
        let pid = parse_elevated_pid(&output.stdout)?;
        Ok(LaunchedProcess {
            pid: Some(pid),
            backend: self.backend,
            handle: ProcessHandle::UacDetached,
        })
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(&self.arguments)
            .current_dir(&self.current_directory);
        for (name, value) in &self.environment {
            command.env(name, value);
        }
        command
    }

    #[must_use]
    pub fn display(&self) -> String {
        std::iter::once(self.program.as_os_str())
            .chain(self.arguments.iter().map(OsString::as_os_str))
            .map(quote_for_display)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl LaunchedProcess {
    /// Returns the native game PID for UAC launches and the host launcher PID
    /// for Wine/CrossOver launches.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    #[must_use]
    pub fn backend(&self) -> RunnerBackend {
        self.backend
    }

    pub fn try_status(&mut self) -> Result<LaunchStatus, LaunchError> {
        match &mut self.handle {
            ProcessHandle::Tracked(child) => child
                .try_wait()
                .map(|status| status.map_or(LaunchStatus::Running, LaunchStatus::Exited))
                .map_err(LaunchError::Inspect),
            ProcessHandle::UacDetached => Ok(LaunchStatus::StartedDetached),
        }
    }

    pub async fn wait(&mut self) -> Result<LaunchStatus, LaunchError> {
        match &mut self.handle {
            ProcessHandle::Tracked(child) => child
                .wait()
                .await
                .map(LaunchStatus::Exited)
                .map_err(LaunchError::Wait),
            ProcessHandle::UacDetached => Ok(LaunchStatus::StartedDetached),
        }
    }
}

fn parse_elevated_pid(stdout: &[u8]) -> Result<u32, LaunchError> {
    let output = String::from_utf8_lossy(stdout).replace('\0', "");
    let pid = output
        .lines()
        .filter_map(|line| {
            line.trim()
                .rsplit_once(ELEVATED_PID_MARKER)
                .map(|(_, value)| value)
        })
        .filter_map(|value| value.trim().parse::<u32>().ok())
        .next_back();
    pid.filter(|pid| *pid != 0)
        .ok_or_else(|| LaunchError::InvalidElevatedPid {
            output: output.trim().to_owned(),
        })
}

fn output_detail(output: &[u8]) -> String {
    let detail = String::from_utf8_lossy(output).replace('\0', "");
    let detail = detail.trim();
    if detail.is_empty() {
        "elevation was declined or PowerShell returned no diagnostic".to_owned()
    } else {
        detail.chars().take(2_000).collect()
    }
}

fn quote_for_display(value: &OsStr) -> String {
    let text = value.to_string_lossy();
    if text.contains(char::is_whitespace) || text.contains('"') {
        format!("\"{}\"", text.replace('"', "\\\""))
    } else {
        text.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        fs,
        path::{Path, PathBuf},
    };

    use tempfile::tempdir;

    use super::{
        ELEVATED_PID_MARKER, LaunchError, LaunchRequest, LaunchSpec, LaunchStatus, Runner,
        RunnerBackend, parse_elevated_pid,
    };

    #[test]
    fn wine_command_has_explicit_executable_cwd_and_prefix() {
        let request = LaunchRequest::new(Path::new("/games/Kart Rider"));
        let spec = Runner::Wine {
            binary: PathBuf::from("/usr/local/bin/wine64"),
            prefix: Some(PathBuf::from("/bottles/p5136")),
        }
        .build(&request)
        .unwrap();

        assert_eq!(spec.program, Path::new("/usr/local/bin/wine64"));
        assert_eq!(
            spec.arguments,
            [
                request.executable().into_os_string(),
                OsString::from("-profile:launcher"),
            ]
        );
        assert_eq!(spec.current_directory, Path::new("/games/Kart Rider"));
        assert_eq!(
            spec.environment,
            [(
                OsString::from("WINEPREFIX"),
                OsString::from("/bottles/p5136")
            )]
        );
        assert_eq!(spec.backend(), RunnerBackend::Wine);
    }

    #[test]
    fn crossover_command_keeps_the_bottle_explicit() {
        let request = LaunchRequest::new("/Games/KartRider");
        let spec = Runner::CrossOver {
            wine_binary: PathBuf::from(
                "/Applications/CrossOver.app/Contents/SharedSupport/CrossOver/bin/wine",
            ),
            bottle: "KartRider-P5136".to_owned(),
        }
        .build(&request)
        .unwrap();

        assert_eq!(
            spec.arguments,
            [
                OsString::from("--bottle"),
                OsString::from("KartRider-P5136"),
                OsString::from("--cx-app"),
                request.executable().into_os_string(),
                OsString::from("-profile:launcher"),
            ]
        );
        assert_eq!(spec.backend(), RunnerBackend::CrossOver);
    }

    #[test]
    fn custom_executable_is_resolved_from_the_game_directory() {
        let request = LaunchRequest::new("/Games/KartRider").with_executable("KartRider.test.exe");

        assert_eq!(
            request.executable(),
            Path::new("/Games/KartRider/KartRider.test.exe")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sikarugir_opens_the_preconfigured_wrapper() {
        let request = LaunchRequest::new("/games/Kart Rider");
        let spec = Runner::Sikarugir {
            app: PathBuf::from("/Applications/KartRider P5136.app"),
        }
        .build(&request)
        .unwrap();

        assert_eq!(spec.program, Path::new("/usr/bin/open"));
        assert_eq!(
            spec.arguments,
            [OsString::from("/Applications/KartRider P5136.app")]
        );
        assert_eq!(spec.current_directory, Path::new("/games/Kart Rider"));
        assert_eq!(spec.backend(), RunnerBackend::Sikarugir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sikarugir_preflight_requires_an_existing_app_directory() {
        let directory = tempdir().unwrap();
        let game_executable = directory.path().join("KartRider.exe");
        fs::write(&game_executable, b"validation fixture only").unwrap();
        let missing_app = directory.path().join("missing.app");
        let spec = Runner::Sikarugir {
            app: missing_app.clone(),
        }
        .build(&LaunchRequest::new(directory.path()))
        .unwrap();

        assert!(matches!(
            spec.validate(&game_executable),
            Err(LaunchError::MissingSikarugirApp(path)) if path == missing_app
        ));

        let app = directory.path().join("KartRider.app");
        fs::create_dir(&app).unwrap();
        let valid = Runner::Sikarugir { app }
            .build(&LaunchRequest::new(directory.path()))
            .unwrap();
        valid.validate(&game_executable).unwrap();
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn sikarugir_is_rejected_outside_macos() {
        let error = Runner::Sikarugir {
            app: PathBuf::from("/Applications/KartRider.app"),
        }
        .build(&LaunchRequest::new("/games/KartRider"))
        .unwrap_err();

        assert!(matches!(error, LaunchError::UnsupportedSikarugir));
    }

    #[test]
    fn automatic_runner_resolves_for_the_build_host() {
        let expected = if cfg!(windows) {
            RunnerBackend::NativeElevated
        } else {
            RunnerBackend::Wine
        };
        assert_eq!(Runner::Auto.resolved_backend().unwrap(), expected);
    }

    #[test]
    fn elevated_pid_parser_uses_only_the_tagged_nonzero_value() {
        let output = format!("noise\n\u{feff}{ELEVATED_PID_MARKER}12345\n");
        assert_eq!(parse_elevated_pid(output.as_bytes()).unwrap(), 12_345);
        assert!(parse_elevated_pid(b"P5136_ELEVATED_PID=0").is_err());
        assert!(parse_elevated_pid(b"12345").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn elevated_native_uses_fixed_script_and_environment_bound_paths() {
        let request = LaunchRequest::new(r"C:\Games\Kart Rider; not-script");
        let spec = Runner::NativeElevated.build(&request).unwrap();

        assert!(spec.program.is_absolute());
        assert_eq!(spec.backend(), RunnerBackend::NativeElevated);
        assert!(spec.arguments.iter().any(|arg| {
            arg.to_string_lossy()
                .contains("$env:P5136_CONNECTOR_GAME_EXE")
        }));
        assert!(!spec.arguments.iter().any(|arg| {
            arg.to_string_lossy()
                .contains(request.game_directory.to_string_lossy().as_ref())
        }));
        assert!(spec.environment.iter().any(|(name, value)| {
            name == "P5136_CONNECTOR_GAME_EXE" && value == request.executable().as_os_str()
        }));
        assert!(spec.environment.iter().any(|(name, value)| {
            name == "P5136_CONNECTOR_GAME_DIR" && value == request.game_directory.as_os_str()
        }));
    }

    #[cfg(windows)]
    #[test]
    fn elevated_preflight_accepts_an_alternate_executable() {
        let directory = tempdir().unwrap();
        let game_executable = directory.path().join("KartRider.exe");
        fs::write(
            &game_executable,
            b"unpacked or otherwise modified P5136 executable",
        )
        .unwrap();
        let request = LaunchRequest::new(directory.path());
        let spec = Runner::NativeElevated.build(&request).unwrap();

        spec.preflight(&game_executable).unwrap();
    }

    #[tokio::test]
    async fn harmless_direct_command_reports_pid_and_success() {
        let directory = tempdir().unwrap();
        let game_executable = directory.path().join("KartRider.exe");
        fs::write(&game_executable, b"validation fixture only").unwrap();

        let request = LaunchRequest::new(directory.path());
        let mut spec = Runner::Native.build(&request).unwrap();
        configure_harmless_command(&mut spec);

        let mut process = spec.spawn(&game_executable).await.unwrap();
        assert!(process.pid().is_some());
        assert_eq!(process.backend(), RunnerBackend::Native);
        let status = process.wait().await.unwrap();
        assert!(matches!(status, LaunchStatus::Exited(exit) if exit.success()));
    }

    #[cfg(windows)]
    fn configure_harmless_command(spec: &mut LaunchSpec) {
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
    fn configure_harmless_command(spec: &mut LaunchSpec) {
        spec.program = PathBuf::from("/bin/sh");
        spec.arguments = [OsString::from("-c"), OsString::from("exit 0")].into();
        spec.environment.clear();
    }
}
