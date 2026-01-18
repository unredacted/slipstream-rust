mod server;
mod server_tquic;
mod streams;
mod target;

use clap::Parser;
use server::{run_server, ServerConfig};
use server_tquic::{run_server_tquic, TquicServerConfig};
use slipstream_core::{normalize_domain, parse_host_port, AddressKind, HostPort};
use tokio::runtime::Builder;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "slipstream-server",
    about = "slipstream-server - A high-performance covert channel over DNS (server)"
)]
struct Args {
    #[arg(long = "dns-listen-port", short = 'l', default_value_t = 53)]
    dns_listen_port: u16,
    #[arg(
        long = "target-address",
        short = 'a',
        default_value = "127.0.0.1:5201",
        value_parser = parse_target_address
    )]
    target_address: HostPort,
    #[arg(long = "cert", short = 'c', value_name = "PATH")]
    cert: String,
    #[arg(long = "key", short = 'k', value_name = "PATH")]
    key: String,
    #[arg(long = "domain", short = 'd', value_parser = parse_domain, required = true)]
    domains: Vec<String>,
    #[arg(long = "max-connections", short = 'm', default_value_t = 256)]
    max_connections: u32,
    #[arg(long = "debug-streams")]
    debug_streams: bool,
    #[arg(long = "debug-commands")]
    debug_commands: bool,
    /// Use the tquic-based runtime instead of picoquic (experimental)
    #[arg(long = "use-tquic", default_value_t = false)]
    use_tquic: bool,
}

fn main() {
    init_logging();
    let args = Args::parse();

    let runtime = Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .expect("Failed to build Tokio runtime");

    if args.use_tquic {
        // Use tquic-based runtime (pure Rust)
        tracing::info!("Using tquic runtime (experimental)");
        let tquic_config = TquicServerConfig {
            dns_listen_port: args.dns_listen_port,
            target_address: args.target_address,
            cert: args.cert,
            key: args.key,
            domains: args.domains,
            max_connections: args.max_connections,
            debug_streams: args.debug_streams,
            debug_commands: args.debug_commands,
        };
        match runtime.block_on(run_server_tquic(&tquic_config)) {
            Ok(code) => std::process::exit(code),
            Err(err) => {
                tracing::error!("Server error: {}", err);
                std::process::exit(1);
            }
        }
    } else {
        // Use picoquic-based runtime (default)
        let config = ServerConfig {
            dns_listen_port: args.dns_listen_port,
            target_address: args.target_address,
            cert: args.cert,
            key: args.key,
            domains: args.domains,
            max_connections: args.max_connections,
            debug_streams: args.debug_streams,
            debug_commands: args.debug_commands,
        };
        match runtime.block_on(run_server(&config)) {
            Ok(code) => std::process::exit(code),
            Err(err) => {
                tracing::error!("Server error: {}", err);
                std::process::exit(1);
            }
        }
    }
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .try_init();
}

fn parse_domain(input: &str) -> Result<String, String> {
    normalize_domain(input).map_err(|err| err.to_string())
}

fn parse_target_address(input: &str) -> Result<HostPort, String> {
    parse_host_port(input, 5201, AddressKind::Target).map_err(|err| err.to_string())
}
