use std::net::IpAddr;
use clap::{Parser, Subcommand};
use rusnel::{run_client, run_server, ClientConfig, ServerConfig};
use tracing_subscriber;

/// Rusnel is a fast tcp/udp multiplexed tunnel.
#[derive(Parser)]
#[command(name = "Rusnel")]
#[command(about = "A fast tcp/udp tunnel", long_about = None)]
struct Args {
    #[command(subcommand)]
    mode: Mode,
}

#[derive(Debug)]
#[derive(Subcommand)]
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
        /// defines the Rusnel server address
        #[arg(value_parser)]
        server: IpAddr,

        /// defines the Rusnel server port
        #[arg(value_parser)]
        port: u16,
    },
}


fn main() {
    rustls::crypto::ring::default_provider().install_default().expect("Failed to install rustls crypto provider");

    tracing_subscriber::fmt::init();

    let args = Args::parse();

    match mode {
        Mode::Server { host, port } => {
            let server_config = ServerConfig { host, port };
            println!("Initialized server with config: {:?}", server_config);
            run_server(server_config);
        },
        Mode::Client { server, port } => {
            let client_config = ClientConfig { server, port };
            println!("Initialized client with config: {:?}", client_config);
            run_client(client_config);
        },
    }
}