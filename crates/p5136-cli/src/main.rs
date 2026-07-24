use std::{
    net::{IpAddr, Ipv4Addr, SocketAddrV4},
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use p5136_connector::{
    DEFAULT_PROBE_TIMEOUT, LaunchRequest, Runner, launcher_profile_xml, normalize_nickname,
    probe_messenger, server_config_xml,
};
use p5136_core::ports::{DEFAULT_CONFIGURED_PORT, PortTopology};
use p5136_server::{BoundServer, ServerConfig};
use tracing_subscriber::EnvFilter;

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

    /// Preview connector files and a native/Wine/CrossOver launch command.
    Connect(ConnectArgs),
}

#[derive(Debug, clap::Args)]
struct ServerArgs {
    #[arg(long, default_value = "0.0.0.0")]
    bind: IpAddr,

    #[arg(long, default_value = "127.0.0.1")]
    advertise: Ipv4Addr,

    #[arg(long, default_value_t = DEFAULT_CONFIGURED_PORT)]
    configured_port: u16,

    #[arg(long, default_value_t = 250)]
    first_message_delay_ms: u64,

    #[arg(long, default_value_t = 12)]
    login_timeout_seconds: u64,
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

    /// Print the exact preparation plan without modifying or launching.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum RunnerKind {
    Auto,
    Native,
    Wine,
    Crossover,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    match cli.command {
        Some(Command::Server(args)) => run_server(args).await,
        Some(Command::Probe(args)) => run_probe(args).await,
        Some(Command::Connect(args)) => run_connector_preview(args),
        None => {
            Cli::command().print_help()?;
            println!("\n\nGUI startup is planned after the PIN/BML patcher is ported.");
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
        first_message_delay: Duration::from_millis(args.first_message_delay_ms),
        login_timeout: Duration::from_secs(args.login_timeout_seconds),
        ..ServerConfig::default()
    };

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
    tracing::warn!(
        "only the server-first login handshake is implemented; gameplay packets are not ready"
    );

    tokio::signal::ctrl_c()
        .await
        .context("failed to install Ctrl-C handler")?;
    server.shutdown().await.context("server shutdown failed")
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

fn run_connector_preview(args: ConnectArgs) -> Result<()> {
    reject_unspecified_server(args.server)?;
    let nickname = normalize_nickname(&args.username).context("invalid P5136 nickname")?;
    let ports = PortTopology::new(args.configured_port)
        .context("configured port cannot provide all connector offsets")?;
    let login_endpoint = SocketAddrV4::new(args.server, ports.login_tcp());
    let runner = build_runner(&args)?;
    let request = LaunchRequest::new(args.game_dir);
    let spec = runner
        .build(&request)
        .context("failed to construct the game launch command")?;

    println!("Files that will be prepared:");
    println!(
        "  {}",
        request.game_directory.join("KartRider.pin").display()
    );
    println!(
        "  {}",
        request.game_directory.join("KartRider.xml").display()
    );
    println!(
        "  {}",
        request
            .game_directory
            .join("Profile/kr/launcher.xml")
            .display()
    );
    println!("\nKartRider.xml (UTF-8, no BOM):");
    println!(
        "{}",
        String::from_utf8_lossy(&server_config_xml(login_endpoint))
    );
    println!("\nProfile/kr/launcher.xml (UTF-8, no BOM):");
    println!(
        "{}",
        String::from_utf8_lossy(&launcher_profile_xml(&nickname))
    );
    println!("\nLaunch command:");
    println!("  {}", spec.display());
    println!("Working directory:");
    println!("  {}", spec.current_directory.display());
    println!("Messenger probe endpoint:");
    println!("  {}:{}", args.server, ports.messenger_tcp());

    if !args.dry_run {
        bail!(
            "live connector execution is intentionally blocked until the PIN/BML patcher and \
             pristine-backup transaction are ported; rerun with --dry-run"
        );
    }
    Ok(())
}

fn build_runner(args: &ConnectArgs) -> Result<Runner> {
    match args.runner {
        RunnerKind::Auto => Ok(Runner::Auto),
        RunnerKind::Native => Ok(Runner::Native),
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
