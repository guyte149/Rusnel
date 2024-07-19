use clap::{Parser, Subcommand};
use rusnel::{run_client, run_server};
use tracing_subscriber;


/// Rusnel is a fast tcp/udp multiplexed tunnel.
#[derive(Parser)]
#[command(name = "Rusnel")]
#[command(about = "A fast tcp/udp tunnel", long_about = None)]
struct Args {
    #[command(subcommand)]
    mode: Mode,
}

#[derive(Subcommand)]
enum Mode {
    /// run Rusnel in server mode
    Server, 
    /// run Rusnel in client mode
    Client 
}

fn main() {
    rustls::crypto::ring::default_provider().install_default().expect("Failed to install rustls crypto provider");

    tracing_subscriber::fmt::init();

    let args = Args::parse();

    match &args.mode {
        Mode::Server => run_server(),
        Mode::Client => run_client()
    }
}