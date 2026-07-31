use std::{
    fs::OpenOptions,
    io,
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
    time::Duration,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use p5136_connector::{
    ConnectorPlan, ConnectorRequest, DEFAULT_PROBE_TIMEOUT, InstallationOptions, Runner,
    execute_connector, launcher_profile_xml, probe_messenger, server_config_xml,
};
use p5136_core::ports::{DEFAULT_CONFIGURED_PORT, PortTopology};
use p5136_server::{
    BoundServer, DEFAULT_MAX_LOGIN_SESSIONS, ItemProbabilityRankPolicy,
    RewardPersistenceRuntimeError, ServerConfig, ServerError, ServerHandle,
    load_item_probability_xml,
};
use tracing_appender::non_blocking::{NonBlockingBuilder, WorkerGuard};
use tracing_subscriber::{EnvFilter, Layer, layer::SubscriberExt, util::SubscriberInitExt};

mod client_paths;
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

    /// Stock-client directory, its Profile directory, or an exported KartCatalog.xml.
    #[arg(long, value_name = "CLIENT_DIR_OR_CATALOG")]
    catalog: Option<PathBuf>,

    /// Stock-client Data directory containing KR archives and item.rho.
    #[arg(long, value_name = "DATA_DIR")]
    client_data_dir: Option<PathBuf>,

    /// Portable item-probability XML override. Without this, client
    /// item.rho/RHO5 data is loaded automatically when Data is configured.
    #[arg(long, value_name = "PATH")]
    item_probability_xml: Option<PathBuf>,

    /// Trust the live rank carried by item-pickup packets. Set false for the
    /// Combined fallback when clients are outside the LAN/friends trust boundary.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    trust_client_item_rank: bool,

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
        let logging = init_tracing(false)?;
        return gui::run(logging.log_path.clone(), logging);
    }

    let cli = Cli::parse();
    let _logging = init_tracing(cli.verbose)?;
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

struct LoggingRuntime {
    log_path: PathBuf,
    _file_worker: WorkerGuard,
}

fn init_tracing(verbose: bool) -> Result<LoggingRuntime> {
    let default_level = if verbose { "debug" } else { "info" };
    let console_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));
    let log_path = create_log_file()?;
    let file = OpenOptions::new()
        .append(true)
        .open(&log_path)
        .with_context(|| format!("failed to open log file {}", log_path.display()))?;

    let console = tracing_subscriber::fmt::layer()
        .with_writer(io::stderr)
        .with_filter(console_filter);
    // Packet events deliberately have their own filter: the default terminal
    // level stays concise, while every transport-boundary packet is retained
    // in the diagnostic file even without `--verbose` or `RUST_LOG=debug`.
    let (file_writer, file_worker) = NonBlockingBuilder::default()
        .buffered_lines_limit(4_096)
        .lossy(true)
        .finish(file);
    let packet_file = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(file_writer)
        .with_filter(EnvFilter::new("info,p5136_packet=debug"));
    tracing_subscriber::registry()
        .with(console)
        .with(packet_file)
        .init();
    tracing::info!(log_file = %log_path.display(), "file logging enabled");
    Ok(LoggingRuntime {
        log_path,
        _file_worker: file_worker,
    })
}

fn create_log_file() -> Result<PathBuf> {
    let directory = match std::env::var_os("P5136_LOG_DIR") {
        Some(directory) if !directory.is_empty() => PathBuf::from(directory),
        _ => default_log_directory()?,
    };
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_millis();
    create_log_file_in(&directory, timestamp, std::process::id())
}

fn create_log_file_in(directory: &Path, timestamp: u128, process: u32) -> Result<PathBuf> {
    std::fs::create_dir_all(directory)
        .with_context(|| format!("failed to create log directory {}", directory.display()))?;
    for sequence in 0_u8..=99 {
        let suffix = if sequence == 0 {
            String::new()
        } else {
            format!("-{sequence}")
        };
        let path = directory.join(format!("p5136-{timestamp}-{process}{suffix}.log"));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to create log file {}", path.display()));
            }
        }
    }
    bail!(
        "could not reserve a unique P5136 log file in {}",
        directory.display()
    )
}

fn default_log_directory() -> Result<PathBuf> {
    Ok(std::env::current_dir()
        .context("failed to locate the current working directory")?
        .join("logs"))
}

async fn run_server(args: ServerArgs) -> Result<()> {
    let ports = PortTopology::new(args.configured_port)
        .context("configured port cannot provide all P5136 service offsets")?;
    let client_paths =
        client_paths::resolve_client_runtime_paths(args.catalog, args.client_data_dir)?;
    let item_probabilities = args
        .item_probability_xml
        .as_deref()
        .map(load_item_probability_xml)
        .transpose()
        .context("failed to load the item-probability XML override")?;
    let config = ServerConfig {
        bind_address: args.bind,
        advertised_address: args.advertise,
        ports,
        profile_root: args.profile_root,
        catalog_path: client_paths.catalog_path,
        client_data_dir: client_paths.client_data_dir,
        item_probabilities,
        item_probability_rank_policy: if args.trust_client_item_rank {
            ItemProbabilityRankPolicy::TrustClientReported
        } else {
            ItemProbabilityRankPolicy::CombinedFallback
        },
        first_message_delay: Duration::from_millis(args.first_message_delay_ms),
        login_timeout: Duration::from_secs(args.login_timeout_seconds),
        session_idle_timeout: Duration::from_secs(args.session_idle_timeout_seconds),
        session_write_timeout: Duration::from_secs(args.session_write_timeout_seconds),
        max_login_sessions: args.max_login_sessions,
        allow_remote_profile_creation: args.allow_remote_profile_creation,
        ..ServerConfig::default()
    };
    let catalog_configured = config.catalog_path.is_some();
    let emblem_catalog_configured = config.client_data_dir.is_some();

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
    if emblem_catalog_configured {
        tracing::info!("stock KR client emblem definitions are configured");
    } else {
        tracing::warn!(
            "no client data directory is configured, so RequestEmblems uses only an optional \
             KartCatalog.xml Emblems extension"
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

    use super::{Cli, Command, ConnectArgs, RunnerKind, default_log_directory, run_connector};

    #[test]
    fn logs_default_beneath_the_process_current_directory() {
        assert_eq!(
            default_log_directory().unwrap(),
            std::env::current_dir().unwrap().join("logs")
        );
    }

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
        assert_eq!(args.client_data_dir, None);
        assert_eq!(args.bind, std::net::IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(
            args.max_login_sessions,
            p5136_server::DEFAULT_MAX_LOGIN_SESSIONS
        );
        assert!(args.trust_client_item_rank);
        assert!(!args.allow_remote_profile_creation);

        let cli =
            Cli::try_parse_from(["p5136", "server", "--trust-client-item-rank", "false"]).unwrap();
        let Some(Command::Server(args)) = cli.command else {
            panic!("server subcommand should parse as Command::Server");
        };
        assert!(!args.trust_client_item_rank);
    }

    #[test]
    fn server_accepts_explicit_profile_catalog_and_client_data_paths() {
        let cli = Cli::try_parse_from([
            "p5136",
            "server",
            "--profile-root",
            "profiles",
            "--catalog",
            "KartCatalog.xml",
            "--client-data-dir",
            "client/Data",
        ])
        .unwrap();
        let Some(Command::Server(args)) = cli.command else {
            panic!("server subcommand should parse as Command::Server");
        };

        assert_eq!(args.profile_root, Path::new("profiles"));
        assert_eq!(args.catalog.as_deref(), Some(Path::new("KartCatalog.xml")));
        assert_eq!(
            args.client_data_dir.as_deref(),
            Some(Path::new("client/Data"))
        );
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

    #[test]
    fn file_log_reservation_creates_distinct_appendable_files() {
        let temporary = tempdir().unwrap();
        let directory = temporary.path().join("nested").join("logs");
        let first = super::create_log_file_in(&directory, 123, 456).unwrap();
        let second = super::create_log_file_in(&directory, 123, 456).unwrap();

        assert_eq!(first.file_name().unwrap(), "p5136-123-456.log");
        assert_eq!(second.file_name().unwrap(), "p5136-123-456-1.log");
        assert!(first.is_file());
        assert!(second.is_file());
    }
}
