use clap::{Parser, Subcommand};
use rusnel::macros::set_verbose;
use rusnel::{run_client, run_server, verbose, ClientConfig, ServerConfig};
use std::net::{IpAddr, SocketAddr};
use tracing::debug;
use tracing_subscriber;

/// Rusnel is a fast tcp/udp multiplexed tunnel.
#[derive(Parser)]
#[command(name = "Rusnel")]
#[command(about = "A fast tcp/udp tunnel", long_about = None)]
struct Args {
    #[command(subcommand)]
    mode: Mode,

    /// enable verbose logging
    #[arg(short, long, default_value_t = false)]
    verbose: bool,

    /// enable debug logging
    #[arg(short, long, default_value_t = false)]
    debug: bool,
}

#[derive(Debug, Subcommand)]
enum Mode {
    /// run Rusnel in server mode
    Server {
        /// defines Rusnel listening host (the network interface)
        #[arg(long, default_value_t = IpAddr::V4(std::net::Ipv4Addr::new(0, 0, 0, 0)))]
        host: IpAddr,

        /// defines Rusnel listening port
        #[arg(long, short, default_value_t = 8080)]
        port: u16,
    },
    /// run Rusnel in client mode
    Client {
        /// defines the Rusnel server address (in form of host:port)
        #[arg(value_parser)]
        server: SocketAddr,
    },
}

fn main() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    let args = Args::parse();

    let log_level = match args.debug {
        true => tracing::Level::DEBUG,
        false => tracing::Level::INFO,
    };
    tracing_subscriber::fmt().with_max_level(log_level).init();

    set_verbose(args.verbose);

    debug!("is verbose enabled: {}", args.verbose);
    debug!("is debug enabled: {}", args.debug);

    match args.mode {
        Mode::Server { host, port } => {
            let server_config = ServerConfig { host, port };
            verbose!("Initialized server with config: {:?}", server_config);
            run_server(server_config);
        }
        Mode::Client { server } => {
            let client_config = ClientConfig { server };
            verbose!("Initialized client with config: {:?}", client_config);
            run_client(client_config);
        }
    }
}
