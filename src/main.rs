use clap::Parser;
use rusnel::{run_client, run_server};


/// Rusnel is a fast tcp/udp multiplexed tunnel.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// run Rusnel as client/server
    #[arg(short, long)]
    mode: String

}

fn main() {
    let args = Args::parse();

    match args.mode.as_str() {
        "server" => run_server(),
        "client" => run_client(),
        _ => {println!("the selected mode is invalid: {}", args.mode)}

    }
}