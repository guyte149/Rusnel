use clap::crate_version;
use clap::{Parser, Subcommand};
use rusnel::common::remote::RemoteRequest;
use rusnel::macros::set_verbose;
use rusnel::{run_client, run_server, verbose, ClientConfig, ServerConfig};
use std::net::{IpAddr, ToSocketAddrs};
use std::process;
use tracing::debug;
use tracing_subscriber;

/// Rusnel is a fast tcp/udp multiplexed tunnel.
#[derive(Parser)]
#[command(name = "Rusnel", version = crate_version!())]
#[command(about = "A fast tcp/udp tunnel", long_about = None)]
struct Args {
    #[command(subcommand)]
    mode: Mode,
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

        /// Allow clients to specify reverse port forwarding remotes.
        #[arg(long, default_value_t = false)]
        allow_reverse: bool,

        /// enable verbose logging
        #[arg(short('v'), long("verbose"), default_value_t = false)]
        is_verbose: bool,

        /// enable debug logging
        #[arg(long("debug"), default_value_t = false)]
        is_debug: bool,
    },
    /// run Rusnel in client mode
    Client {
        /// defines the Rusnel server address (in form of host:port)
        #[arg(value_parser)]
        server: String,

        #[arg(name = "remote", required = true , value_delimiter = ' ', num_args = 1.., help=r#"
<remote>s are remote connections tunneled through the server, each which come in the form:

    <local-host>:<local-port>:<remote-host>:<remote-port>/<protocol>

    ■ local-host defaults to 0.0.0.0 (all interfaces).
    ■ local-port defaults to remote-port.
    ■ remote-port is required*.
    ■ remote-host defaults to 0.0.0.0 (server localhost).
    ■ protocol defaults to tcp.

which shares <remote-host>:<remote-port> from the server to the client as <local-host>:<local-port>, or:

    R:<local-host>:<local-port>:<remote-host>:<remote-port>/<protocol>

which does reverse port forwarding,
sharing <remote-host>:<remote-port> from the client to the server's <local-host>:<local-port>.

    example remotes

        1337
        example.com:1337
        1337:google.com:80
        192.168.1.14:5000:google.com:80
        socks
        5000:socks
        R:2222:localhost:22
        R:socks
        R:5000:socks
        1.1.1.1:53/udp
    
    When the Rusnel server has --allow-reverse enabled, remotes can be prefixed with R to denote that they are reversed.

    Remotes can specify "socks" in place of remote-host and remote-port.
    The default local host and port for a "socks" remote is 127.0.0.1:1080.
        "#)]
        remotes: Vec<String>,

        /// enable verbose logging
        #[arg(short('v'), long("verbose"), default_value_t = false)]
        is_verbose: bool,

        /// enable debug logging
        #[arg(long("debug"), default_value_t = false)]
        is_debug: bool,
    },
}

fn main() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    let args = Args::parse();

    match args.mode {
        Mode::Server {
            host,
            port,
            allow_reverse,
            is_verbose,
            is_debug,
        } => {
            set_log_level(is_verbose, is_debug);

            let server_config = ServerConfig { host, port, allow_reverse };
            verbose!("Initialized server with config: {:?}", server_config);
            run_server(server_config);
        }
        Mode::Client {
            server,
            remotes,
            is_verbose,
            is_debug,
        } => {
            set_log_level(is_verbose, is_debug);
            let server_addr = server
                .to_socket_addrs()
                .expect(&format!("Failed to resolve server address: {server}"))
                .next()
                .unwrap();

            let mut remotes_list: Vec<RemoteRequest> = vec![];
            for remote_str in remotes {
                let remote = RemoteRequest::from_str(remote_str).unwrap_or_else(|error| {
                    eprintln!("Remote parsing error: {}", error);
                    process::exit(1);
                });
                remotes_list.push(remote);
            }

            let client_config = ClientConfig {
                server: server_addr,
                remotes: remotes_list,
            };
            verbose!("Initialized client with config: {:?}", client_config);
            run_client(client_config);
        }
    }
}

fn set_log_level(is_verbose: bool, is_debug: bool) {
    let log_level = match is_debug {
        true => tracing::Level::DEBUG,
        false => tracing::Level::INFO,
    };
    tracing_subscriber::fmt().with_max_level(log_level).init();

    set_verbose(is_verbose);

    debug!("is verbose enabled: {}", is_verbose);
    debug!("is debug enabled: {}", is_debug);
}
