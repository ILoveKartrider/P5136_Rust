use std::{
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use p5136_connector::{
    ConnectorPlan, ConnectorRequest, DEFAULT_PROBE_TIMEOUT, InstallationOptions, Runner,
    execute_connector, launcher_profile_xml, probe_messenger, server_config_xml,
};
use p5136_core::ports::{DEFAULT_CONFIGURED_PORT, PortTopology};
use p5136_server::{
    BoundServer, DEFAULT_MAX_LOGIN_SESSIONS, RewardPersistenceRuntimeError, ServerConfig,
    ServerError, ServerHandle,
};
use tracing_subscriber::EnvFilter;

mod gui;

#[derive(Debug, Parser)]
#[command(
    name = "p5136",
    version,
    about = "Cross-platform KartRider P5136 server and connector"
)]
struct Cli {
    /// Increase runtime logging.
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the four-port P5136 server foundation.
    Server(ServerArgs),

    /// Test the connector's messenger TCP reachability check.
    Probe(ProbeArgs),

    /// Prepare the client, probe the server, and launch through native/Wine/CrossOver.
    Connect(ConnectArgs),
}

#[derive(Debug, clap::Args)]
struct ServerArgs {
    #[arg(long, default_value = "127.0.0.1")]
    bind: IpAddr,

    #[arg(long, default_value = "127.0.0.1")]
    advertise: Ipv4Addr,

    #[arg(long, default_value_t = DEFAULT_CONFIGURED_PORT)]
    configured_port: u16,

    #[arg(long, default_value = "Profile", value_name = "PATH")]
    profile_root: PathBuf,

    #[arg(long, value_name = "KartCatalog.xml")]
    catalog: Option<PathBuf>,

    #[arg(long, default_value_t = 250)]
    first_message_delay_ms: u64,

    #[arg(long, default_value_t = 12)]
    login_timeout_seconds: u64,

    #[arg(long, default_value_t = 300)]
    session_idle_timeout_seconds: u64,

    #[arg(long, default_value_t = 15)]
    session_write_timeout_seconds: u64,

    #[arg(long, default_value_t = DEFAULT_MAX_LOGIN_SESSIONS)]
    max_login_sessions: usize,

    /// Allow non-loopback clients to create profiles for new nicknames.
    #[arg(long)]
    allow_remote_profile_creation: bool,
}

#[derive(Debug, clap::Args)]
struct ProbeArgs {
    #[arg(long, default_value = "127.0.0.1")]
    server: Ipv4Addr,

    #[arg(long, default_value_t = DEFAULT_CONFIGURED_PORT)]
    configured_port: u16,

    #[arg(long, default_value_t = 4_000)]
    timeout_ms: u64,
}

#[derive(Debug, clap::Args)]
struct ConnectArgs {
    #[arg(long)]
    game_dir: PathBuf,

    #[arg(long)]
    username: String,

    #[arg(long, default_value = "127.0.0.1")]
    server: Ipv4Addr,

    #[arg(long, default_value_t = DEFAULT_CONFIGURED_PORT)]
    configured_port: u16,

    #[arg(long, value_enum, default_value_t = RunnerKind::Auto)]
    runner: RunnerKind,

    #[arg(long)]
    wine_binary: Option<PathBuf>,

    #[arg(long)]
    wine_prefix: Option<PathBuf>,

    #[arg(long)]
    crossover_binary: Option<PathBuf>,

    #[arg(long)]
    bottle: Option<String>,

    #[arg(long, default_value_t = 4_000)]
    timeout_ms: u64,

    /// Print the exact preparation plan without modifying or launching.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum RunnerKind {
    Auto,
    /// Launch directly as the current user, without UAC.
    Native,
    /// Request Windows UAC elevation before launching.
    NativeElevated,
    Wine,
    Crossover,
}

fn main() -> Result<()> {
    if should_start_gui(std::env::args_os()) {
        init_tracing(false);
        return gui::run();
    }

    let cli = Cli::parse();
    init_tracing(cli.verbose);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to create CLI runtime")?;
    runtime.block_on(run_cli(cli))
}

fn should_start_gui(arguments: impl IntoIterator<Item = std::ffi::OsString>) -> bool {
    arguments.into_iter().nth(1).is_none()
}

async fn run_cli(cli: Cli) -> Result<()> {
    match cli.command {
        Some(Command::Server(args)) => run_server(args).await,
        Some(Command::Probe(args)) => run_probe(args).await,
        Some(Command::Connect(args)) => run_connector(args).await,
        None => {
            Cli::command().print_help()?;
            println!();
            Ok(())
        }
    }
}

fn init_tracing(verbose: bool) {
    let default_level = if verbose { "debug" } else { "info" };
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

async fn run_server(args: ServerArgs) -> Result<()> {
    let ports = PortTopology::new(args.configured_port)
        .context("configured port cannot provide all P5136 service offsets")?;
    let config = ServerConfig {
        bind_address: args.bind,
        advertised_address: args.advertise,
        ports,
        profile_root: args.profile_root,
        catalog_path: args.catalog,
        first_message_delay: Duration::from_millis(args.first_message_delay_ms),
        login_timeout: Duration::from_secs(args.login_timeout_seconds),
        session_idle_timeout: Duration::from_secs(args.session_idle_timeout_seconds),
        session_write_timeout: Duration::from_secs(args.session_write_timeout_seconds),
        max_login_sessions: args.max_login_sessions,
        allow_remote_profile_creation: args.allow_remote_profile_creation,
        ..ServerConfig::default()
    };
    let catalog_configured = config.catalog_path.is_some();

    let server = BoundServer::bind(config)
        .await
        .context("failed to bind the P5136 transport set")?
        .start()
        .context("failed to start the P5136 supervisor")?;
    let endpoints = server.endpoints();
    tracing::info!(
        game_udp = %endpoints.game_udp,
        login_tcp = %endpoints.login_tcp,
        p2p_udp = %endpoints.p2p_udp,
        messenger_tcp = %endpoints.messenger_tcp,
        "P5136 foundation server is running"
    );
    if catalog_configured {
        tracing::warn!("race settlement, MyRoom, and progression handling remain in progress");
    } else {
        tracing::warn!(
            "no inventory catalog is configured, so PqGetRider is unavailable; \
             race settlement, MyRoom, and progression handling remain in progress"
        );
    }

    wait_for_server_exit(&server).await
}

async fn wait_for_server_exit(server: &ServerHandle) -> Result<()> {
    tokio::select! {
        result = server.wait() => result.context("P5136 server runtime stopped"),
        signal = shutdown_signal() => {
            signal.context("failed to install the process shutdown-signal handler")?;
            shutdown_server_after_signal(server).await
        }
    }
}

async fn shutdown_server_after_signal(server: &ServerHandle) -> Result<()> {
    match server.shutdown().await {
        Ok(()) => Ok(()),
        Err(
            error @ ServerError::RewardPersistence(RewardPersistenceRuntimeError::DeadLetter {
                ..
            }),
        ) => {
            let status = server
                .reward_status()
                .await
                .context("failed to inspect retained reward recovery state")?;
            tracing::error!(
                %error,
                outstanding_lanes = status.outstanding_lanes().len(),
                dead_letters = status.dead_letters().len(),
                "graceful shutdown is paused on retained reward recovery state"
            );
            tracing::warn!(
                "send the shutdown signal again to explicitly discard in-memory reward recovery and force shutdown"
            );
            tokio::select! {
                result = server.wait() => {
                    result.context(
                        "P5136 server runtime stopped while awaiting force-shutdown confirmation",
                    )
                }
                signal = shutdown_signal() => {
                    signal.context("failed to wait for force-shutdown confirmation")?;
                    server
                        .force_shutdown()
                        .await
                        .context("forced server shutdown failed")
                }
            }
        }
        Err(error) => Err(error).context("server shutdown failed"),
    }
}

#[cfg(unix)]
async fn shutdown_signal() -> std::io::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    tokio::select! {
        _ = interrupt.recv() => Ok(()),
        _ = terminate.recv() => Ok(()),
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() -> std::io::Result<()> {
    tokio::signal::ctrl_c().await
}

async fn run_probe(args: ProbeArgs) -> Result<()> {
    reject_unspecified_server(args.server)?;
    let ports = PortTopology::new(args.configured_port)
        .context("configured port cannot provide the messenger offset")?;
    let timeout = if args.timeout_ms == 0 {
        DEFAULT_PROBE_TIMEOUT
    } else {
        Duration::from_millis(args.timeout_ms)
    };
    probe_messenger(args.server, ports, timeout)
        .await
        .context("server reachability probe failed")?;
    println!(
        "Messenger TCP reachable at {}:{}",
        args.server,
        ports.messenger_tcp()
    );
    Ok(())
}

async fn run_connector(args: ConnectArgs) -> Result<()> {
    reject_unspecified_server(args.server)?;
    let ports = PortTopology::new(args.configured_port)
        .context("configured port cannot provide all connector offsets")?;
    let runner = build_runner(&args)?;
    let timeout = if args.timeout_ms == 0 {
        DEFAULT_PROBE_TIMEOUT
    } else {
        Duration::from_millis(args.timeout_ms)
    };
    let plan = ConnectorPlan::new(ConnectorRequest {
        game_directory: args.game_dir,
        nickname: args.username,
        server_address: args.server,
        ports,
        runner,
        probe_timeout: timeout,
        installation_options: InstallationOptions::default(),
    })
    .context("failed to construct the connector plan")?;

    println!("Files that will be prepared:");
    for path in plan.prepared_paths() {
        println!("  {}", path.display());
    }
    println!("\nKartRider.xml (UTF-8, no BOM):");
    println!(
        "{}",
        String::from_utf8_lossy(&server_config_xml(plan.login_endpoint))
    );
    println!("\nProfile/kr/launcher.xml (UTF-8, no BOM):");
    println!(
        "{}",
        String::from_utf8_lossy(&launcher_profile_xml(&plan.nickname))
    );
    println!("\nGame target:");
    println!(
        "  {} -profile:launcher",
        plan.launch_request.executable().display()
    );
    println!("Host launch command ({}):", plan.launch_spec.backend());
    println!("  {}", plan.launch_spec.display());
    if !plan.launch_spec.environment.is_empty() {
        println!("Host launch environment:");
        for (name, value) in &plan.launch_spec.environment {
            println!("  {}={}", name.to_string_lossy(), value.to_string_lossy());
        }
    }
    println!("Game working directory:");
    println!("  {}", plan.game_directory.display());
    println!("Messenger probe endpoint:");
    println!("  {}", plan.messenger_endpoint);

    if args.dry_run {
        println!("\nDry run complete; no files, sockets, or processes were touched.");
        return Ok(());
    }

    let mut execution = execute_connector(&plan)
        .await
        .context("connector execution failed")?;
    println!(
        "\nInstallation prepared ({:?}).",
        execution.prepared_installation.build_evidence
    );
    println!("Messenger TCP reachable at {}.", plan.messenger_endpoint);
    let status = execution
        .launched_process
        .try_status()
        .context("failed to inspect the launched process")?;
    let pid = execution
        .launched_process
        .pid()
        .map_or_else(|| "unavailable".to_owned(), |pid| pid.to_string());
    println!(
        "Game launch accepted via {}: PID {pid}, {status}.",
        execution.launched_process.backend()
    );
    Ok(())
}

fn build_runner(args: &ConnectArgs) -> Result<Runner> {
    match args.runner {
        RunnerKind::Auto => Ok(Runner::Auto),
        RunnerKind::Native => Ok(Runner::Native),
        RunnerKind::NativeElevated => Ok(Runner::NativeElevated),
        RunnerKind::Wine => Ok(Runner::Wine {
            binary: args
                .wine_binary
                .clone()
                .unwrap_or_else(|| PathBuf::from("wine")),
            prefix: args.wine_prefix.clone(),
        }),
        RunnerKind::Crossover => Ok(Runner::CrossOver {
            wine_binary: args
                .crossover_binary
                .clone()
                .context("--crossover-binary is required for the CrossOver runner")?,
            bottle: args
                .bottle
                .clone()
                .context("--bottle is required for the CrossOver runner")?,
        }),
    }
}

fn reject_unspecified_server(address: Ipv4Addr) -> Result<()> {
    if address.is_unspecified() {
        bail!("connector server address cannot be 0.0.0.0");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        net::Ipv4Addr,
        path::{Path, PathBuf},
    };

    use clap::Parser;
    use tempfile::tempdir;

    use super::{Cli, Command, ConnectArgs, RunnerKind, run_connector};

    #[test]
    fn no_arguments_select_gui_and_any_argument_selects_cli() {
        assert!(super::should_start_gui([OsString::from("p5136")]));
        assert!(!super::should_start_gui([
            OsString::from("p5136"),
            OsString::from("--help"),
        ]));
        assert!(!super::should_start_gui([
            OsString::from("p5136"),
            OsString::new(),
        ]));
    }

    #[test]
    fn server_uses_legacy_profile_directory_and_no_catalog_by_default() {
        let cli = Cli::try_parse_from(["p5136", "server"]).unwrap();
        let Some(Command::Server(args)) = cli.command else {
            panic!("server subcommand should parse as Command::Server");
        };

        assert_eq!(args.profile_root, Path::new("Profile"));
        assert_eq!(args.catalog, None);
        assert_eq!(args.bind, std::net::IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(
            args.max_login_sessions,
            p5136_server::DEFAULT_MAX_LOGIN_SESSIONS
        );
        assert!(!args.allow_remote_profile_creation);
    }

    #[test]
    fn server_accepts_explicit_profile_and_catalog_paths() {
        let cli = Cli::try_parse_from([
            "p5136",
            "server",
            "--profile-root",
            "profiles",
            "--catalog",
            "KartCatalog.xml",
        ])
        .unwrap();
        let Some(Command::Server(args)) = cli.command else {
            panic!("server subcommand should parse as Command::Server");
        };

        assert_eq!(args.profile_root, Path::new("profiles"));
        assert_eq!(args.catalog.as_deref(), Some(Path::new("KartCatalog.xml")));
    }

    #[tokio::test]
    async fn connector_dry_run_does_not_create_or_modify_the_game_directory() {
        let temporary = tempdir().unwrap();
        let missing_game_directory = temporary.path().join("missing-game");
        assert!(!missing_game_directory.exists());

        run_connector(ConnectArgs {
            game_dir: missing_game_directory.clone(),
            username: "dry-run-user".to_owned(),
            server: Ipv4Addr::LOCALHOST,
            configured_port: 39_311,
            runner: RunnerKind::Native,
            wine_binary: None,
            wine_prefix: None,
            crossover_binary: None,
            bottle: None,
            timeout_ms: 10,
            dry_run: true,
        })
        .await
        .unwrap();

        assert!(!missing_game_directory.exists());
    }

    #[test]
    fn native_runner_is_the_explicit_non_elevated_mode() {
        let args = ConnectArgs {
            game_dir: PathBuf::from("game"),
            username: "user".to_owned(),
            server: Ipv4Addr::LOCALHOST,
            configured_port: 39_311,
            runner: RunnerKind::Native,
            wine_binary: None,
            wine_prefix: None,
            crossover_binary: None,
            bottle: None,
            timeout_ms: 10,
            dry_run: true,
        };

        assert!(matches!(
            super::build_runner(&args).unwrap(),
            p5136_connector::Runner::Native
        ));
    }
}
